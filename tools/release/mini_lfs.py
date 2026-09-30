"""미니 LFS 서버 — 이 저장소의 git LFS 정본. GitHub LFS 대신 모든 기기·CI·controller 가 여기서 받고 여기로 올린다.

GitHub LFS 예산이 막혀 CI 굽기(2026-09-27 v0.2.1)와 controller 격리 워크트리(2026-09-29)가 섰다. 그래서 저장소 .lfsconfig 가
이 서버를 가리킨다. 받기는 토큰 없이 누구나 — 공개 저장소의 그림이라 가릴 것이 없고, 남이 clone 해도 그림이 보여야 한다.
올리기는 토큰이 있어야 하고, 받은 바이트의 sha256 이 이름(oid)과 같을 때만 저장한다 — 토큰이 새어도 있는 그림을 바꾸거나
지우지 못한다(저장소 내용은 GitHub push 권한이 정한다). 잠금 API 는 없다.

이 파일 하나만 복사해 띄울 수 있게 표준 라이브러리만 쓴다 — launchd 가 부르는 파이썬은 ~/Desktop 을 못 읽는다(TCC).
설치·토큰·상주는 scripts/mini-lfs.sh, 운영은 docs/fast-patch-release.md 「미니 LFS 서버」.
"""

import argparse
import base64
import hashlib
import hmac
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import stat
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlsplit

LFS_JSON = "application/vnd.git-lfs+json"
OID = re.compile(r"[0-9a-f]{64}\Z")
NAME = re.compile(r"[A-Za-z0-9_.-]{1,40}\Z")
MAX_BODY = 1 << 20
MAX_OBJECTS = 1000
MIN_TOKEN = 32
# Cloudflare 무료 요청 본문 한도(100 MB)가 바깥 길에서 먼저 자른다 — 그 아래로 잡아 올리기가 중간에 끊기지 않고 이름을 대고 멈춘다.
MAX_OBJECT = 90 << 20
# 미니는 다른 서비스도 돈다 — 그림이 디스크를 다 먹지 않게 이만큼은 남긴다.
MIN_FREE = 5 << 30
CHALLENGE = 'Basic realm="kasaterm-lfs"'


def object_path(store, oid):
    return Path(store) / oid[:2] / oid[2:4] / oid


def presented_secret(value):
    """Authorization 한 줄에서 비밀 부분. Basic 은 사용자 이름을 보지 않는다 — git 자격 도우미가 무엇을 넣든 비밀만 맞으면 된다."""
    scheme, _, cred = (value or "").strip().partition(" ")
    cred = cred.strip()
    if scheme.lower() == "bearer":
        return cred
    if scheme.lower() == "basic":
        try:
            return base64.b64decode(cred, validate=True).decode("utf-8").partition(":")[2]
        except (ValueError, UnicodeDecodeError):
            return ""
    return ""


def read_tokens(path):
    """{이름: 토큰}. 한 줄에 「이름 토큰」, 이름 없는 한 줄짜리(예전 형식)는 「default」. 주인만 읽어야 하고 충분히 길어야 한다."""
    st = os.stat(path)
    if stat.S_IMODE(st.st_mode) & 0o077:
        raise ValueError(f"{path}: 권한이 너무 넓다(chmod 600)")
    table = {}
    for line in Path(path).read_text().splitlines():
        parts = line.split()
        if not parts or parts[0].startswith("#"):
            continue
        if len(parts) > 2:
            raise ValueError(f"{path}: 「이름 토큰」 줄이 아니다")
        name, token = parts if len(parts) == 2 else ("default", parts[0])
        if not NAME.match(name) or name in table:
            raise ValueError(f"{path}: 이름이 올바르지 않거나 겹친다")
        if len(token) < MIN_TOKEN:
            raise ValueError(f"{path}: {name} 토큰이 {MIN_TOKEN}자보다 짧다")
        table[name] = token
    return table


class Tokens:
    """토큰 파일을 요청마다 다시 본다(바뀌었을 때만) — 추가·폐기가 재시작 없이 바로 먹는다. 파일이 틀리면 아무도 못 올린다."""

    def __init__(self, path):
        self.path, self.stamp, self.table, self.lock = path, None, {}, threading.Lock()

    def current(self):
        with self.lock:
            try:
                st = os.stat(self.path)
                stamp = (st.st_mtime_ns, st.st_size, st.st_ino, st.st_mode)
                if stamp != self.stamp:
                    self.stamp, self.table = stamp, read_tokens(self.path)
            except (OSError, ValueError) as error:
                if self.table or self.stamp != "bad":
                    sys.stderr.write(f"mini-lfs: 토큰 파일을 못 읽어 올리기를 막는다 — {error}\n")
                self.stamp, self.table = "bad", {}
            return self.table

    def who(self, values):
        """맞는 토큰의 이름. 없으면 None."""
        table = self.current()
        for value in values:
            got = presented_secret(value).encode()
            if not got:
                continue
            for name, token in table.items():
                if hmac.compare_digest(got, token.encode()):
                    return name
        return None


def free_bytes(store):
    st = os.statvfs(store)
    return st.f_bavail * st.f_frsize


def batch(request, store, href_base, who, authorization):
    """(상태, 본문). who 는 올바른 토큰의 이름(없으면 None), authorization 은 그 요청의 Authorization 줄.

    받기는 누구에게나 답한다. 올리기는 서버에 없는 그림이 하나라도 있을 때만 토큰을 묻는다 — 이미 있는 그림만 든 push 는
    토큰 없는 기기(controller·CI 의 버전 커밋)에서도 통과하고, 새로 쓰는 일은 토큰 없이는 절대 없다."""
    if not isinstance(request, dict):
        return 422, {"message": "batch 요청이 JSON 객체가 아니다"}
    operation = request.get("operation")
    if operation not in ("download", "upload"):
        return 422, {"message": "operation 은 download·upload 뿐이다"}
    if "basic" not in (request.get("transfers") or ["basic"]):
        return 422, {"message": "basic 전송만 한다"}
    wanted = request.get("objects")
    if not isinstance(wanted, list) or len(wanted) > MAX_OBJECTS:
        return 422, {"message": f"objects 는 {MAX_OBJECTS}개 이하의 목록이어야 한다"}
    out, missing = [], []
    for item in wanted:
        oid = item.get("oid") if isinstance(item, dict) else None
        size = item.get("size") if isinstance(item, dict) else None
        if not isinstance(oid, str) or not OID.match(oid) or type(size) is not int or size < 0:
            out.append({"oid": oid, "size": size, "error": {"code": 422, "message": "oid·size 가 올바르지 않다"}})
            continue
        try:
            actual = object_path(store, oid).stat().st_size
        except OSError:
            actual = None
        if actual is not None and actual != size:
            out.append({"oid": oid, "size": size, "error": {"code": 422, "message": f"크기가 다르다(서버 {actual})"}})
        elif operation == "download" and actual is None:
            out.append({"oid": oid, "size": size, "error": {"code": 404, "message": "미니 LFS 에 없는 그림이다 — 올린 기기에서 git lfs push"}})
        elif operation == "download":
            out.append({"oid": oid, "size": size, "authenticated": True,
                        "actions": {"download": {"href": f"{href_base}/objects/{oid}", "expires_in": 86400}}})
        elif actual is not None:
            out.append({"oid": oid, "size": size})
        elif size > MAX_OBJECT:
            out.append({"oid": oid, "size": size, "error": {"code": 413, "message": f"그림 하나가 {MAX_OBJECT >> 20} MB 를 넘는다"}})
        else:
            missing.append(len(out))
            out.append({"oid": oid, "size": size})
    if missing:
        if who is None:
            return 401, {"message": "올리기는 토큰이 필요하다 — scripts/mini-lfs.sh login"}
        need = sum(out[i]["size"] for i in missing)
        if free_bytes(store) - need < MIN_FREE:
            return 507, {"message": f"미니 디스크 여유가 {MIN_FREE >> 30} GB 아래로 내려간다 — 올리지 않는다"}
        for i in missing:
            out[i]["authenticated"] = True
            out[i]["actions"] = {"upload": {"href": f"{href_base}/objects/{out[i]['oid']}",
                                            "header": {"Authorization": authorization}, "expires_in": 3600}}
    return 200, {"transfer": "basic", "objects": out, "hash_algo": "sha256"}


def receive(store, incoming, oid, length, rfile):
    """본문을 임시 파일로 받아 sha256 이 oid 와 같을 때만 제자리로 옮긴다. (상태, 메시지, 본문을 다 읽었는가)."""
    fd, tmp = None, None
    try:
        fd, tmp = _mkstemp(incoming)
        digest, left = hashlib.sha256(), length
        with os.fdopen(fd, "wb") as out:
            fd = None
            while left:
                chunk = rfile.read(min(left, 1 << 20))
                if not chunk:
                    return 400, "본문이 Content-Length 보다 짧다", False
                digest.update(chunk)
                out.write(chunk)
                left -= len(chunk)
            if digest.hexdigest() != oid:
                return 422, "받은 바이트의 sha256 이 oid 와 다르다 — 저장하지 않는다", True
            out.flush()
            os.fsync(out.fileno())
        os.chmod(tmp, 0o644)
        final = object_path(store, oid)
        final.parent.mkdir(parents=True, exist_ok=True)
        os.replace(tmp, final)
        tmp = None
        return 200, "", True
    finally:
        if fd is not None:
            os.close(fd)
        if tmp is not None:
            Path(tmp).unlink(missing_ok=True)


def _mkstemp(incoming):
    path = Path(incoming) / f"{secrets.token_hex(16)}.part"
    return os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0), 0o600), str(path)


def make_handler(store, public_url, tokens):
    href_base = public_url.rstrip("/")
    prefix = urlsplit(href_base).path.rstrip("/")
    incoming = Path(store).parent / "incoming"
    incoming.mkdir(mode=0o700, exist_ok=True)

    class Handler(BaseHTTPRequestHandler):
        server_version = "kasaterm-mini-lfs"
        sys_version = ""
        protocol_version = "HTTP/1.1"
        # cloudflared 는 쉬는 연결을 90초 붙들고 다시 쓴다 — 그보다 먼저 닫으면 막 닫힌 연결에 요청을 실어 520 이 난다.
        timeout = 120

        def parse_request(self):
            self.consumed = False
            return super().parse_request()

        def log_message(self, fmt, *args):
            headers = getattr(self, "headers", None)
            who = headers.get("CF-Connecting-IP") if headers else None
            sys.stderr.write(f"{self.log_date_time_string()} {who or self.client_address[0]} {fmt % args}\n")

        def reply(self, status, body, challenge=False):
            data = json.dumps(body, ensure_ascii=False).encode()
            self.send_response(status)
            self.send_header("Content-Type", LFS_JSON)
            self.send_header("Content-Length", str(len(data)))
            self.send_header("Cache-Control", "no-store")
            if challenge:
                self.send_header("LFS-Authenticate", CHALLENGE)
            # 안 읽은 요청 본문이 남은 채 연결을 이어 쓰면 그 본문이 다음 요청으로 읽힌다.
            if self.command in ("POST", "PUT", "PATCH", "DELETE") and not self.consumed:
                self.close_connection = True
                self.send_header("Connection", "close")
            self.end_headers()
            if self.command != "HEAD":
                self.wfile.write(data)

        def route(self):
            path = urlsplit(self.path).path
            if not path.startswith(prefix + "/"):
                return None, None
            rest = path[len(prefix):]
            if rest == "/objects/batch":
                return "batch", None
            m = re.fullmatch(r"/objects/([0-9a-f]{64})", rest)
            if m:
                return "object", m.group(1)
            if rest == "/locks" or rest.startswith("/locks/"):
                return "locks", None
            return None, None

        def who(self):
            return tokens.who(self.headers.get_all("Authorization") or [])

        def do_GET(self):
            kind, oid = self.route()
            if kind == "locks":
                return self.reply(404, {"message": "잠금 API 는 없다"})
            if kind != "object":
                return self.reply(404, {"message": "없는 경로다"})
            try:
                f = open(object_path(store, oid), "rb")
            except OSError:
                return self.reply(404, {"message": "미니 LFS 에 없는 그림이다"})
            with f:
                size = os.fstat(f.fileno()).st_size
                self.send_response(200)
                self.send_header("Content-Type", "application/octet-stream")
                self.send_header("Content-Length", str(size))
                # 이름이 곧 내용의 해시라 한 번 받은 것은 영원히 맞다.
                self.send_header("Cache-Control", "public, max-age=31536000, immutable")
                self.end_headers()
                if self.command != "HEAD":
                    try:
                        shutil.copyfileobj(f, self.wfile, 1 << 20)
                    except (BrokenPipeError, ConnectionResetError):
                        self.close_connection = True

        do_HEAD = do_GET

        def do_POST(self):
            kind, _ = self.route()
            if kind == "locks":
                return self.reply(404, {"message": "잠금 API 는 없다"})
            if kind != "batch":
                return self.reply(404, {"message": "없는 경로다"})
            try:
                length = int(self.headers.get("Content-Length") or "0")
            except ValueError:
                length = -1
            if not 0 < length <= MAX_BODY:
                return self.reply(413, {"message": f"batch 본문은 1..{MAX_BODY} 바이트여야 한다"})
            try:
                raw = self.rfile.read(length)
                self.consumed = True
                request = json.loads(raw)
            except ValueError:
                return self.reply(422, {"message": "batch 본문이 JSON 이 아니다"})
            auth = self.headers.get("Authorization") or ""
            status, body = batch(request, store, href_base, self.who(), auth)
            self.reply(status, body, challenge=status == 401)

        def do_PUT(self):
            kind, oid = self.route()
            if kind != "object":
                return self.reply(404, {"message": "없는 경로다"})
            name = self.who()
            if name is None:
                return self.reply(401, {"message": "올리기는 토큰이 필요하다"}, challenge=True)
            if self.headers.get("Transfer-Encoding"):
                return self.reply(411, {"message": "Content-Length 로 보낸다"})
            try:
                length = int(self.headers.get("Content-Length") or "")
            except ValueError:
                return self.reply(411, {"message": "Content-Length 가 필요하다"})
            if not 0 <= length <= MAX_OBJECT:
                return self.reply(413, {"message": f"그림 하나가 {MAX_OBJECT >> 20} MB 를 넘는다"})
            if free_bytes(store) - length < MIN_FREE:
                return self.reply(507, {"message": f"미니 디스크 여유가 {MIN_FREE >> 30} GB 아래로 내려간다"})
            status, message, whole = receive(store, incoming, oid, length, self.rfile)
            self.consumed = whole
            if status != 200:
                return self.reply(status, {"message": message})
            self.log_message("올림 %s %d 바이트 (%s)", oid, length, name)
            self.reply(200, {})

        def refuse(self):
            self.reply(405, {"message": "지우기·고치기는 없다"})

        do_DELETE = do_PATCH = refuse

    return Handler


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--store", required=True, help="LFS 객체 폴더(ab/cd/<oid>). 그 부모에 incoming/ 을 만든다")
    ap.add_argument("--token-file", required=True, help="올리기 토큰 — 한 줄에 「이름 토큰」, 600")
    ap.add_argument("--public-url", required=True, help=".lfsconfig 의 lfs.url — 내려받기·올리기 href 의 바탕")
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, required=True)
    args = ap.parse_args(argv)
    if not Path(args.store).is_dir():
        raise SystemExit(f"{args.store}: 객체 폴더가 없다")
    if urlsplit(args.public_url).scheme not in ("http", "https") or not urlsplit(args.public_url).path.strip("/"):
        raise SystemExit("--public-url 은 경로가 붙은 http(s) 주소여야 한다(예: https://host/lfs)")
    try:
        read_tokens(args.token_file)
    except (OSError, ValueError) as error:
        raise SystemExit(str(error))
    handler = make_handler(args.store, args.public_url, Tokens(args.token_file))
    server = ThreadingHTTPServer((args.host, args.port), handler)
    server.daemon_threads = True
    sys.stderr.write(f"mini-lfs {args.host}:{args.port} store={args.store}\n")
    server.serve_forever()


if __name__ == "__main__":
    main()
