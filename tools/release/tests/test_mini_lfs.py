import base64
import hashlib
import http.client
import json
import os
from pathlib import Path
import re
import shutil
import socket
import subprocess
import tempfile
import threading
import unittest
from unittest import mock
from http.server import ThreadingHTTPServer

from tools.release import mini_lfs

# 미니 LFS 서버 — 이 프로세스가 띄운 진짜 서버에 진짜 HTTP 로 묻는다. 끝의 git-lfs 왕복은 저장소와 같은 .lfsconfig 와
# git 자격 도우미로 받고 올린다 — 서버만이 아니라 기기·CI 가 쓰는 설정 방식이 통하는지 본다.

TOKEN = "t" * 20 + "0123456789abcdefghijklmnopqrstuvwxyz"
OTHER = "o" * 20 + "0123456789abcdefghijklmnopqrstuvwxyz"
REPO = Path(__file__).resolve().parents[3]


def put_object(store, data):
    oid = hashlib.sha256(data).hexdigest()
    path = mini_lfs.object_path(store, oid)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)
    return oid


def stored(store):
    return sorted(p.name for p in Path(store).rglob("*") if p.is_file())


class Served(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="mini-lfs-"))
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.store = self.tmp / "objects"
        self.store.mkdir()
        self.token_file = self.tmp / "token"
        self.write_tokens(f"mac {TOKEN}\nci {OTHER}\n")
        tokens = mini_lfs.Tokens(self.token_file)
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), mini_lfs.make_handler(self.store, "http://placeholder/lfs", tokens))
        self.port = self.server.server_address[1]
        self.base = f"http://127.0.0.1:{self.port}/lfs"
        self.server.RequestHandlerClass = mini_lfs.make_handler(self.store, self.base, tokens)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)

    def write_tokens(self, text, mode=0o600):
        self.token_file.write_text(text)
        os.chmod(self.token_file, mode)
        # 같은 초·같은 크기로 바꿔도 다시 읽게 mtime 을 민다.
        st = os.stat(self.token_file)
        os.utime(self.token_file, ns=(st.st_atime_ns, st.st_mtime_ns + 1_000_000_000))

    def call(self, method, path, body=None, auth=None, conn=None, headers=None):
        c = conn or http.client.HTTPConnection("127.0.0.1", self.port, timeout=10)
        h = {"Content-Type": mini_lfs.LFS_JSON, **(headers or {})}
        if auth:
            h["Authorization"] = auth
        data = json.dumps(body).encode() if isinstance(body, (dict, list)) else body
        c.request(method, path, body=data, headers=h)
        r = c.getresponse()
        raw = r.read()
        if conn is None:
            c.close()
        return r.status, raw, r

    def batch(self, objects, operation="download", **kw):
        status, raw, _ = self.call("POST", "/lfs/objects/batch", {"operation": operation, "transfers": ["basic"], "objects": objects}, **kw)
        return status, json.loads(raw)


class PublicReadTests(Served):
    def test_download_needs_no_token_and_points_at_this_server(self):
        oid = put_object(self.store, b"sprite")
        missing = "0" * 64
        status, body = self.batch([{"oid": oid, "size": 6}, {"oid": missing, "size": 3}, {"oid": oid, "size": 7},
                                   {"oid": "../../token", "size": 1}])
        self.assertEqual(status, 200)
        got, gone, wrong, bad = body["objects"]
        self.assertEqual(got["actions"]["download"]["href"], f"{self.base}/objects/{oid}")
        self.assertNotIn("header", got["actions"]["download"])
        self.assertEqual(gone["error"]["code"], 404)
        self.assertEqual(wrong["error"]["code"], 422)
        self.assertEqual(bad["error"]["code"], 422)

    def test_a_wrong_or_stale_credential_does_not_block_downloads(self):
        oid = put_object(self.store, b"sprite")
        for auth in ("Bearer revoked", "Basic " + base64.b64encode(b"lfs:old").decode()):
            self.assertEqual(self.batch([{"oid": oid, "size": 6}], auth=auth)[0], 200)
            self.assertEqual(self.call("GET", f"/lfs/objects/{oid}", auth=auth)[0], 200)

    def test_object_bytes_and_head_length(self):
        data = os.urandom(3 * (1 << 20) + 17)
        oid = put_object(self.store, data)
        status, raw, r = self.call("GET", f"/lfs/objects/{oid}")
        self.assertEqual((status, raw), (200, data))
        self.assertIn("immutable", r.getheader("Cache-Control"))
        status, raw, r = self.call("HEAD", f"/lfs/objects/{oid}")
        self.assertEqual((status, raw, r.getheader("Content-Length")), (200, b"", str(len(data))))

    def test_traversal_and_odd_paths_stay_inside_the_store(self):
        (self.tmp / "secret").write_text("no")
        for path in ("/lfs/objects/../secret", "/lfs/objects/%2e%2e%2fsecret", "/lfs/objects/" + "A" * 64,
                     "/lfs/../secret", "/lfsx/objects/batch", "/lfs/objects/../token"):
            status, raw, _ = self.call("GET", path)
            self.assertEqual(status, 404, path)
            self.assertNotIn(b"no", raw.replace(b"\\u", b""))
            self.assertNotIn(TOKEN.encode(), raw)

    def test_there_is_no_lock_api_and_nothing_can_be_deleted(self):
        oid = put_object(self.store, b"keep")
        for method, path in (("POST", "/lfs/locks"), ("POST", "/lfs/locks/verify"), ("GET", "/lfs/locks")):
            self.assertEqual(self.call(method, path, b"{}", auth=f"Bearer {TOKEN}")[0], 404, (method, path))
        for method in ("DELETE", "PATCH"):
            self.assertEqual(self.call(method, f"/lfs/objects/{oid}", b"", auth=f"Bearer {TOKEN}")[0], 405)
        self.assertEqual(mini_lfs.object_path(self.store, oid).read_bytes(), b"keep")

    def test_oversized_or_broken_batch_bodies(self):
        # 큰 본문은 다 받기 전에 끊는다 — 길이만 크게 적고 본문은 안 보낸 채로 답을 받는다.
        with socket.create_connection(("127.0.0.1", self.port), timeout=10) as s:
            s.sendall(f"POST /lfs/objects/batch HTTP/1.1\r\nHost: x\r\nContent-Length: {mini_lfs.MAX_BODY + 1}\r\n\r\n".encode())
            self.assertTrue(s.recv(64).startswith(b"HTTP/1.1 413"))
        self.assertEqual(self.call("POST", "/lfs/objects/batch", b"{not json")[0], 422)
        self.assertEqual(self.call("POST", "/lfs/objects/batch", [1])[0], 422)
        self.assertEqual(self.batch([{"oid": "3" * 64, "size": 1}] * (mini_lfs.MAX_OBJECTS + 1))[0], 422)
        self.assertEqual(self.batch([], operation="delete")[0], 422)


class WriteGateTests(Served):
    def test_writes_without_a_valid_token_are_401_and_store_nothing(self):
        data = b"new art"
        oid = hashlib.sha256(data).hexdigest()
        for auth in (None, "Bearer wrong", f"Bearer {TOKEN}x", "Token " + TOKEN, "Basic !!!",
                     "Basic " + base64.b64encode(b"u:nope").decode()):
            status, raw, r = self.call("POST", "/lfs/objects/batch",
                                       {"operation": "upload", "objects": [{"oid": oid, "size": len(data)}]}, auth=auth)
            self.assertEqual(status, 401, auth)
            self.assertIn("Basic", r.getheader("LFS-Authenticate"))
            self.assertNotIn(b"actions", raw)
            self.assertEqual(self.call("PUT", f"/lfs/objects/{oid}", data, auth=auth)[0], 401, auth)
        self.assertEqual(stored(self.store), [])

    def test_upload_batch_of_objects_already_there_needs_no_token(self):
        oid = put_object(self.store, b"old art")
        status, body = self.batch([{"oid": oid, "size": 7}], operation="upload")
        self.assertEqual(status, 200)
        self.assertNotIn("actions", body["objects"][0])

    def test_basic_auth_takes_the_token_as_password_and_the_action_carries_it(self):
        auth = "Basic " + base64.b64encode(f"anyone:{TOKEN}".encode()).decode()
        status, body = self.batch([{"oid": "4" * 64, "size": 3}], operation="upload", auth=auth)
        self.assertEqual(status, 200)
        action = body["objects"][0]["actions"]["upload"]
        self.assertEqual((action["href"], action["header"]["Authorization"]), (f"{self.base}/objects/{'4' * 64}", auth))

    def test_token_file_rules(self):
        f = self.tmp / "t2"
        for text, mode in ((f"a {TOKEN}", 0o644), ("a short", 0o600), (f"a {TOKEN} extra", 0o600),
                           (f"a {TOKEN}\na {OTHER}", 0o600), (f"bad/name {TOKEN}", 0o600)):
            f.write_text(text)
            os.chmod(f, mode)
            with self.assertRaises(ValueError, msg=text):
                mini_lfs.read_tokens(f)
        f.write_text(TOKEN + "\n")
        self.assertEqual(mini_lfs.read_tokens(f), {"default": TOKEN})
        f.write_text(f"# 주석\nmac {TOKEN}\n\nci {OTHER}\n")
        self.assertEqual(mini_lfs.read_tokens(f), {"mac": TOKEN, "ci": OTHER})

    def test_tokens_are_added_and_revoked_without_a_restart_and_a_broken_file_blocks_all(self):
        new = "n" * 40
        up = [{"oid": "5" * 64, "size": 1}]
        self.assertEqual(self.batch(up, operation="upload", auth=f"Bearer {new}")[0], 401)
        self.write_tokens(f"mac {TOKEN}\nnew {new}\n")
        self.assertEqual(self.batch(up, operation="upload", auth=f"Bearer {new}")[0], 200)
        self.write_tokens(f"mac {TOKEN}\n")
        self.assertEqual(self.batch(up, operation="upload", auth=f"Bearer {new}")[0], 401)
        self.write_tokens(f"mac {TOKEN}\n", mode=0o644)
        self.assertEqual(self.batch(up, operation="upload", auth=f"Bearer {TOKEN}")[0], 401)
        self.write_tokens(f"mac {TOKEN}\n")
        self.assertEqual(self.batch(up, operation="upload", auth=f"Bearer {TOKEN}")[0], 200)


class UploadTests(Served):
    auth = f"Bearer {TOKEN}"

    def test_put_keeps_only_bytes_whose_hash_is_the_name(self):
        data = os.urandom(2 * (1 << 20) + 5)
        oid = hashlib.sha256(data).hexdigest()
        self.assertEqual(self.call("PUT", f"/lfs/objects/{oid}", data, auth=self.auth)[0], 200)
        path = mini_lfs.object_path(self.store, oid)
        self.assertEqual((path.read_bytes(), oct(path.stat().st_mode & 0o777)), (data, "0o644"))
        liar = "6" * 64
        status, raw, _ = self.call("PUT", f"/lfs/objects/{liar}", b"not what the name says", auth=self.auth)
        self.assertEqual(status, 422)
        self.assertEqual(stored(self.store), [oid])
        self.assertEqual(list((self.tmp / "incoming").iterdir()), [])
        status, body = self.batch([{"oid": oid, "size": len(data)}])
        self.assertIn("download", body["objects"][0]["actions"])

    def test_a_cut_off_body_is_not_stored(self):
        oid = hashlib.sha256(b"x" * 100).hexdigest()
        with socket.create_connection(("127.0.0.1", self.port), timeout=10) as s:
            s.sendall(f"PUT /lfs/objects/{oid} HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {TOKEN}\r\n"
                      f"Content-Length: 100\r\n\r\n".encode() + b"x" * 10)
            s.shutdown(socket.SHUT_WR)
            s.recv(64)
        self.assertEqual(stored(self.store), [])
        self.assertEqual(list((self.tmp / "incoming").iterdir()), [])

    def test_size_and_count_caps(self):
        status, body = self.batch([{"oid": "7" * 64, "size": mini_lfs.MAX_OBJECT + 1}], operation="upload", auth=self.auth)
        self.assertEqual((status, body["objects"][0]["error"]["code"]), (200, 413))
        with socket.create_connection(("127.0.0.1", self.port), timeout=10) as s:
            s.sendall(f"PUT /lfs/objects/{'7' * 64} HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {TOKEN}\r\n"
                      f"Content-Length: {mini_lfs.MAX_OBJECT + 1}\r\n\r\n".encode())
            reply = s.recv(256)
        self.assertTrue(reply.startswith(b"HTTP/1.1 413"))
        self.assertIn(b"Connection: close", reply)
        with socket.create_connection(("127.0.0.1", self.port), timeout=10) as s:
            s.sendall(f"PUT /lfs/objects/{'7' * 64} HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {TOKEN}\r\n\r\n".encode())
            self.assertTrue(s.recv(64).startswith(b"HTTP/1.1 411"))
        self.assertEqual(stored(self.store), [])

    def test_the_disk_floor_refuses_before_writing(self):
        data = b"big"
        oid = hashlib.sha256(data).hexdigest()
        with mock.patch.object(mini_lfs, "MIN_FREE", 1 << 62):
            self.assertEqual(self.batch([{"oid": oid, "size": 3}], operation="upload", auth=self.auth)[0], 507)
            self.assertEqual(self.call("PUT", f"/lfs/objects/{oid}", data, auth=self.auth)[0], 507)
        self.assertEqual(stored(self.store), [])

    def test_an_unread_body_does_not_leak_into_the_next_request(self):
        c = http.client.HTTPConnection("127.0.0.1", self.port, timeout=10)
        status, _, r = self.call("PUT", "/lfs/objects/" + "2" * 64, b"GET /lfs/objects/batch HTTP/1.1\r\n\r\n", conn=c)
        self.assertEqual((status, r.getheader("Connection")), (401, "close"))
        c = http.client.HTTPConnection("127.0.0.1", self.port, timeout=10)
        self.assertEqual(self.batch([], operation="upload", conn=c)[0], 200)
        self.assertEqual(self.batch([], conn=c)[0], 200)


@unittest.skipUnless(shutil.which("git-lfs") or Path.home().joinpath(".local/bin/git-lfs").exists(), "git-lfs 없음")
class RealGitLfsTests(Served):
    """진짜 git-lfs 가 .lfsconfig 만 보고 이 서버에서 받고, 자격 도우미의 토큰으로 올리고, 토큰 없으면 못 올리는가."""

    def git(self, *args, cwd, env=None, check=True):
        r = subprocess.run(["git", *args], cwd=cwd, env=env or self.env, capture_output=True, text=True, timeout=120)
        if check and r.returncode:
            self.fail(f"git {' '.join(args)}: {r.stderr}")
        return r

    def setUp(self):
        super().setUp()
        lfs = shutil.which("git-lfs") or str(Path.home() / ".local/bin/git-lfs")
        home = self.tmp / "home"
        home.mkdir()
        self.env = {"PATH": f"{Path(lfs).parent}:/usr/bin:/bin", "HOME": str(home), "GIT_CONFIG_NOSYSTEM": "1",
                    "GIT_TERMINAL_PROMPT": "0", "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@t",
                    "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@t"}
        self.git("lfs", "install", "--skip-repo", cwd=self.tmp)
        src = self.tmp / "src"
        src.mkdir()
        self.git("init", "-q", "-b", "main", cwd=src)
        self.git("lfs", "track", "*.png", cwd=src)
        # 저장소의 .lfsconfig 와 같은 모양 — 주소만 이 서버.
        (src / ".lfsconfig").write_text((REPO / ".lfsconfig").read_text().replace(PUBLIC_URL, self.base))
        (src / "a.png").write_bytes(os.urandom(4096))
        (src / "b.png").write_bytes(os.urandom(100))
        self.git("add", ".", cwd=src)
        self.git("commit", "-q", "-m", "art", cwd=src)
        self.want = {n: (src / n).read_bytes() for n in ("a.png", "b.png")}
        for data in self.want.values():
            put_object(self.store, data)
        self.origin = self.tmp / "origin.git"
        self.git("clone", "-q", "--bare", str(src), str(self.origin), cwd=self.tmp, env={**self.env, "GIT_LFS_SKIP_SMUDGE": "1"})
        self.dst = self.clone("dst")

    def clone(self, name):
        dst = self.tmp / name
        self.git("clone", "-q", str(self.origin), str(dst), cwd=self.tmp, env={**self.env, "GIT_LFS_SKIP_SMUDGE": "1"})
        self.git("lfs", "install", "--local", cwd=dst)
        return dst

    def login(self, token=TOKEN):
        # scripts/mini-lfs.sh login 이 하는 일 — 토큰을 git 자격 도우미에 넣는다.
        self.git("config", "--global", "credential.helper", f"store --file={self.tmp / 'creds'}", cwd=self.tmp)
        host = f"127.0.0.1:{self.port}"
        subprocess.run(["git", "credential", "approve"], input=f"protocol=http\nhost={host}\nusername=lfs\npassword={token}\n\n",
                       env=self.env, cwd=self.tmp, text=True, check=True, timeout=30)

    def pointers_left(self, repo):
        out = self.git("lfs", "ls-files", cwd=repo).stdout
        return [line.split()[-1] for line in out.splitlines() if line.split()[1:2] == ["-"]]

    def add_art(self, repo, name="c.png"):
        data = os.urandom(2048)
        (repo / name).write_bytes(data)
        self.git("add", name, cwd=repo)
        self.git("commit", "-q", "-m", "more art", cwd=repo)
        return data

    def test_anyone_pulls_every_pointer_without_a_token(self):
        self.git("lfs", "pull", cwd=self.dst)
        self.assertEqual(self.pointers_left(self.dst), [])
        for name, data in self.want.items():
            self.assertEqual((self.dst / name).read_bytes(), data)
        self.assertNotIn(TOKEN, (self.dst / ".git/config").read_text())

    def test_a_missing_object_fails_the_pull(self):
        oid = hashlib.sha256(self.want["b.png"]).hexdigest()
        mini_lfs.object_path(self.store, oid).unlink()
        r = self.git("lfs", "pull", cwd=self.dst, check=False)
        self.assertNotEqual(r.returncode, 0)
        self.assertEqual(self.pointers_left(self.dst), ["b.png"])

    def test_push_with_the_credential_uploads_and_another_clone_receives_it(self):
        self.login()
        data = self.add_art(self.dst)
        self.git("push", "-q", "origin", "main", cwd=self.dst)
        self.assertIn(hashlib.sha256(data).hexdigest(), stored(self.store))
        other = self.clone("other")
        self.git("lfs", "pull", cwd=other)
        self.assertEqual((other / "c.png").read_bytes(), data)

    def test_push_without_a_credential_is_refused_before_the_commit_leaves(self):
        data = self.add_art(self.dst)
        r = self.git("push", "-q", "origin", "main", cwd=self.dst, check=False)
        self.assertNotEqual(r.returncode, 0)
        self.assertNotIn(hashlib.sha256(data).hexdigest(), stored(self.store))
        self.assertNotEqual(self.git("rev-parse", "main", cwd=self.origin).stdout, self.git("rev-parse", "main", cwd=self.dst).stdout)

    def test_a_revoked_credential_cannot_push(self):
        self.login("r" * 40)
        data = self.add_art(self.dst)
        self.assertNotEqual(self.git("push", "-q", "origin", "main", cwd=self.dst, check=False).returncode, 0)
        self.assertNotIn(hashlib.sha256(data).hexdigest(), stored(self.store))

    def test_a_push_without_new_art_needs_no_credential(self):
        (self.dst / "notes.txt").write_text("version bump\n")
        self.git("add", "notes.txt", cwd=self.dst)
        self.git("commit", "-q", "-m", "text only", cwd=self.dst)
        self.git("push", "-q", "origin", "main", cwd=self.dst)


PUBLIC_URL = "https://kasaterm.debimarlene.com/lfs"


class RepoContractTests(unittest.TestCase):
    """저장소가 미니를 LFS 정본으로 삼는 계약 — .lfsconfig·release.yml·받기 액션을 글자로 본다(yaml 모듈 없이)."""

    def setUp(self):
        self.wf = (REPO / ".github/workflows/release.yml").read_text()
        self.action = (REPO / ".github/actions/lfs-pull/action.yml").read_text()

    def test_lfsconfig_points_everyone_at_the_mini(self):
        text = (REPO / ".lfsconfig").read_text()
        self.assertRegex(text, rf"(?m)^\s*url = {re.escape(PUBLIC_URL)}$")
        self.assertRegex(text, r"(?m)^\s*locksverify = false$")

    def test_no_job_fetches_from_github_lfs(self):
        self.assertEqual(set(re.findall(r"(?m)^\s+lfs: (.+)$", self.wf)), {"false"})
        self.assertNotIn("MINI_LFS", self.wf)
        self.assertNotIn("secrets", self.action)

    def test_the_builds_that_need_art_pull_it_through_the_action(self):
        msi = self.wf[self.wf.index("  build-msi:"):self.wf.index("  build-dmg:")]
        dmg = self.wf[self.wf.index("  build-dmg:"):]
        self.assertIn("uses: ./.github/actions/lfs-pull", msi)
        self.assertIn("uses: ./.github/actions/lfs-pull", dmg)
        self.assertLess(msi.index("uses: actions/checkout"), msi.index("uses: ./.github/actions/lfs-pull"))

    def test_a_dark_mini_or_a_leftover_pointer_stops_the_build_loudly(self):
        self.assertIn('[[ "$CODE" == 200 ]] || {', self.action)
        self.assertLess(self.action.index('[[ "$CODE" == 200 ]]'), self.action.index("lfs pull"))
        self.assertIn('[[ -z "$LEFT" ]] || {', self.action)
        self.assertIn("GIT_TERMINAL_PROMPT: '0'", self.action)
        self.assertNotIn("continue-on-error", self.wf)
