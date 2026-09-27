"""읽기 전용 git LFS 창구 — CI Windows 굽기가 GitHub LFS 대신 미니에서 그림을 받는다.

GitHub LFS 대역폭 예산이 막히면 Windows 굽기가 그림을 못 받아 릴리스가 반쪽으로 선다(2026-09-27 v0.2.1). 그래서 미니가
가진 실물을 batch API 로 내준다. 받기(download)만 한다 — 올리기·잠금은 거부해, 토큰이 새어도 저장소 내용을 못 바꾼다.
토큰 없는 요청은 경로와 무관하게 401 이라 무엇이 있는지도 드러내지 않는다.

이 파일 하나만 복사해 띄울 수 있게 표준 라이브러리만 쓴다 — launchd 가 부르는 파이썬은 ~/Desktop 을 못 읽는다(TCC).
설치·동기화·상주는 scripts/mini-lfs.sh, 운영은 docs/fast-patch-release.md 「미니 LFS 창구」.
"""

import argparse
import base64
import hmac
import json
import os
from pathlib import Path
import re
import shutil
import stat
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlsplit

LFS_JSON = "application/vnd.git-lfs+json"
OID = re.compile(r"[0-9a-f]{64}\Z")
MAX_BODY = 1 << 20
MAX_OBJECTS = 1000
MIN_TOKEN = 32


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


def authorized(values, token):
    want = token.encode()
    return any((got := presented_secret(v)) and hmac.compare_digest(got.encode(), want) for v in values)


def batch(request, store, href_base, token):
    """(상태, 본문). 없는 그림은 객체별 404 로 알린다 — git lfs 가 어느 그림이 모자란지 이름을 대고 멈춘다."""
    if not isinstance(request, dict):
        return 422, {"message": "batch 요청이 JSON 객체가 아니다"}
    if request.get("operation") != "download":
        return 403, {"message": "읽기 전용 창구다 — download 만 받는다"}
    if "basic" not in (request.get("transfers") or ["basic"]):
        return 422, {"message": "basic 전송만 한다"}
    wanted = request.get("objects")
    if not isinstance(wanted, list) or len(wanted) > MAX_OBJECTS:
        return 422, {"message": f"objects 는 {MAX_OBJECTS}개 이하의 목록이어야 한다"}
    out = []
    for item in wanted:
        oid = item.get("oid") if isinstance(item, dict) else None
        size = item.get("size") if isinstance(item, dict) else None
        if not isinstance(oid, str) or not OID.match(oid) or not isinstance(size, int) or size < 0:
            out.append({"oid": oid, "size": size, "error": {"code": 422, "message": "oid·size 가 올바르지 않다"}})
            continue
        try:
            actual = object_path(store, oid).stat().st_size
        except OSError:
            out.append({"oid": oid, "size": size, "error": {"code": 404, "message": "미니 창구에 없는 그림이다 — mini-lfs.sh sync"}})
            continue
        if actual != size:
            out.append({"oid": oid, "size": size, "error": {"code": 422, "message": f"크기가 다르다(창구 {actual})"}})
            continue
        out.append({"oid": oid, "size": size, "authenticated": True,
                    "actions": {"download": {"href": f"{href_base}/objects/{oid}",
                                             "header": {"Authorization": f"Bearer {token}"}, "expires_in": 3600}}})
    return 200, {"transfer": "basic", "objects": out, "hash_algo": "sha256"}


def read_token(path):
    """토큰 파일은 주인만 읽어야 하고 충분히 길어야 한다 — 어기면 띄우지 않는다."""
    st = os.stat(path)
    if stat.S_IMODE(st.st_mode) & 0o077:
        raise SystemExit(f"{path}: 권한이 너무 넓다(chmod 600)")
    token = Path(path).read_text().strip()
    if len(token) < MIN_TOKEN:
        raise SystemExit(f"{path}: 토큰이 {MIN_TOKEN}자보다 짧다")
    return token


def make_handler(store, public_url, token):
    href_base = public_url.rstrip("/")
    prefix = urlsplit(href_base).path.rstrip("/")

    class Handler(BaseHTTPRequestHandler):
        server_version = "kasaterm-mini-lfs"
        sys_version = ""
        protocol_version = "HTTP/1.1"
        timeout = 60

        def parse_request(self):
            self.consumed = False
            return super().parse_request()

        def log_message(self, fmt, *args):
            headers = getattr(self, "headers", None)
            who = headers.get("CF-Connecting-IP") if headers else None
            sys.stderr.write(f"{self.log_date_time_string()} {who or self.client_address[0]} {fmt % args}\n")

        def reply(self, status, body):
            data = json.dumps(body, ensure_ascii=False).encode()
            self.send_response(status)
            self.send_header("Content-Type", LFS_JSON)
            self.send_header("Content-Length", str(len(data)))
            self.send_header("Cache-Control", "no-store")
            # 안 읽은 요청 본문이 남은 채 연결을 이어 쓰면 그 본문이 다음 요청으로 읽힌다.
            if self.command in ("POST", "PUT", "PATCH", "DELETE") and not self.consumed:
                self.close_connection = True
                self.send_header("Connection", "close")
            self.end_headers()
            if self.command != "HEAD":
                self.wfile.write(data)

        def route(self):
            """(종류, oid). 토큰 확인이 먼저라 무토큰 요청은 여기까지 오지 않는다."""
            path = urlsplit(self.path).path
            if not path.startswith(prefix + "/"):
                return None, None
            rest = path[len(prefix):]
            if rest == "/objects/batch":
                return "batch", None
            m = re.fullmatch(r"/objects/([0-9a-f]{64})", rest)
            if m:
                return "object", m.group(1)
            if rest.startswith("/locks"):
                return "locks", None
            return None, None

        def gate(self):
            if authorized(self.headers.get_all("Authorization") or [], token):
                return True
            self.reply(401, {"message": "토큰이 필요하다"})
            return False

        def do_GET(self):
            if not self.gate():
                return
            kind, oid = self.route()
            if kind == "locks":
                return self.reply(403, {"message": "읽기 전용 창구다 — 잠금은 없다"})
            if kind != "object":
                return self.reply(404, {"message": "없는 경로다"})
            try:
                f = open(object_path(store, oid), "rb")
            except OSError:
                return self.reply(404, {"message": "미니 창구에 없는 그림이다"})
            with f:
                size = os.fstat(f.fileno()).st_size
                self.send_response(200)
                self.send_header("Content-Type", "application/octet-stream")
                self.send_header("Content-Length", str(size))
                self.send_header("Cache-Control", "private, no-store")
                self.end_headers()
                if self.command != "HEAD":
                    try:
                        shutil.copyfileobj(f, self.wfile, 1 << 20)
                    except (BrokenPipeError, ConnectionResetError):
                        self.close_connection = True

        do_HEAD = do_GET

        def do_POST(self):
            if not self.gate():
                return
            kind, _ = self.route()
            if kind != "batch":
                return self.reply(403, {"message": "읽기 전용 창구다"})
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
            self.reply(*batch(request, store, href_base, token))

        def refuse(self):
            if self.gate():
                self.reply(403, {"message": "읽기 전용 창구다"})

        do_PUT = do_DELETE = do_PATCH = refuse

    return Handler


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--store", required=True, help="LFS 객체 폴더(ab/cd/<oid>)")
    ap.add_argument("--token-file", required=True)
    ap.add_argument("--public-url", required=True, help="CI 가 lfs.url 로 쓰는 주소 — 내려받기 href 의 바탕")
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, required=True)
    args = ap.parse_args(argv)
    if not Path(args.store).is_dir():
        raise SystemExit(f"{args.store}: 객체 폴더가 없다")
    if urlsplit(args.public_url).scheme not in ("http", "https") or not urlsplit(args.public_url).path.strip("/"):
        raise SystemExit("--public-url 은 경로가 붙은 http(s) 주소여야 한다(예: https://host/lfs)")
    token = read_token(args.token_file)
    server = ThreadingHTTPServer((args.host, args.port), make_handler(args.store, args.public_url, token))
    server.daemon_threads = True
    sys.stderr.write(f"mini-lfs {args.host}:{args.port} store={args.store}\n")
    server.serve_forever()


if __name__ == "__main__":
    main()
