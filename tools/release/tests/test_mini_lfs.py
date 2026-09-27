import base64
import hashlib
import http.client
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import threading
import unittest
from http.server import ThreadingHTTPServer

from tools.release import mini_lfs

# 미니 LFS 창구 — 이 프로세스가 띄운 진짜 서버에 진짜 HTTP 로 묻는다. 끝의 git-lfs 왕복은 CI Windows 굽기의
# 「Fetch LFS assets」 단계와 같은 방식(환경변수 git 설정 + extraheader)으로 받는다 — 서버만이 아니라 설정 방식이 통하는지 본다.

TOKEN = "t" * 20 + "0123456789abcdefghijklmnopqrstuvwxyz"
REPO = Path(__file__).resolve().parents[3]


def put_object(store, data):
    oid = hashlib.sha256(data).hexdigest()
    path = mini_lfs.object_path(store, oid)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)
    return oid


class Served(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="mini-lfs-"))
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.store = self.tmp / "objects"
        self.store.mkdir()
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), mini_lfs.make_handler(self.store, "http://placeholder/lfs", TOKEN))
        self.port = self.server.server_address[1]
        self.base = f"http://127.0.0.1:{self.port}/lfs"
        self.server.RequestHandlerClass = mini_lfs.make_handler(self.store, self.base, TOKEN)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)

    def call(self, method, path, body=None, auth=f"Bearer {TOKEN}", conn=None):
        c = conn or http.client.HTTPConnection("127.0.0.1", self.port, timeout=10)
        headers = {"Content-Type": mini_lfs.LFS_JSON}
        if auth:
            headers["Authorization"] = auth
        data = json.dumps(body).encode() if isinstance(body, (dict, list)) else body
        c.request(method, path, body=data, headers=headers)
        r = c.getresponse()
        raw = r.read()
        return r.status, raw, r

    def batch(self, objects, operation="download", **kw):
        status, raw, _ = self.call("POST", "/lfs/objects/batch", {"operation": operation, "transfers": ["basic"], "objects": objects}, **kw)
        return status, json.loads(raw)


class GateTests(Served):
    def test_every_route_without_a_valid_token_is_401_and_reveals_nothing(self):
        oid = put_object(self.store, b"x")
        for auth in (None, "Bearer wrong", f"Bearer {TOKEN}x", "Token " + TOKEN, "Basic !!!",
                     "Basic " + base64.b64encode(b"u:nope").decode()):
            for method, path, body in (("POST", "/lfs/objects/batch", {"operation": "download", "objects": []}),
                                       ("GET", f"/lfs/objects/{oid}", None), ("GET", "/lfs/locks", None),
                                       ("GET", "/nowhere", None), ("PUT", f"/lfs/objects/{oid}", b"y")):
                status, raw, _ = self.call(method, path, body, auth=auth)
                self.assertEqual(status, 401, (auth, method, path))
                self.assertNotIn(oid.encode(), raw)

    def test_basic_auth_takes_the_token_as_password_under_any_user(self):
        auth = "Basic " + base64.b64encode(f"anyone:{TOKEN}".encode()).decode()
        self.assertEqual(self.batch([], auth=auth)[0], 200)

    def test_a_weak_or_shared_token_file_refuses_to_start(self):
        f = self.tmp / "token"
        f.write_text(TOKEN)
        os.chmod(f, 0o644)
        with self.assertRaises(SystemExit):
            mini_lfs.read_token(f)
        f.write_text("short")
        os.chmod(f, 0o600)
        with self.assertRaises(SystemExit):
            mini_lfs.read_token(f)
        f.write_text(TOKEN + "\n")
        self.assertEqual(mini_lfs.read_token(f), TOKEN)


class ReadOnlyTests(Served):
    def test_download_batch_points_at_this_server_and_names_what_is_missing(self):
        oid = put_object(self.store, b"sprite")
        missing = "0" * 64
        status, body = self.batch([{"oid": oid, "size": 6}, {"oid": missing, "size": 3}, {"oid": oid, "size": 7},
                                   {"oid": "../../token", "size": 1}])
        self.assertEqual(status, 200)
        got, gone, wrong, bad = body["objects"]
        self.assertEqual(got["actions"]["download"]["href"], f"{self.base}/objects/{oid}")
        self.assertEqual(got["actions"]["download"]["header"]["Authorization"], f"Bearer {TOKEN}")
        self.assertEqual(gone["error"]["code"], 404)
        self.assertEqual(wrong["error"]["code"], 422)
        self.assertEqual(bad["error"]["code"], 422)

    def test_object_bytes_and_head_length(self):
        data = os.urandom(3 * (1 << 20) + 17)
        oid = put_object(self.store, data)
        status, raw, r = self.call("GET", f"/lfs/objects/{oid}")
        self.assertEqual((status, raw), (200, data))
        self.assertIn("no-store", r.getheader("Cache-Control"))
        status, raw, r = self.call("HEAD", f"/lfs/objects/{oid}")
        self.assertEqual((status, raw, r.getheader("Content-Length")), (200, b"", str(len(data))))

    def test_upload_lock_and_write_verbs_are_refused(self):
        oid = put_object(self.store, b"keep")
        status, body = self.batch([{"oid": "1" * 64, "size": 4}], operation="upload")
        self.assertEqual(status, 403)
        self.assertNotIn("objects", body)
        for method, path in (("PUT", f"/lfs/objects/{oid}"), ("DELETE", f"/lfs/objects/{oid}"),
                             ("POST", "/lfs/locks"), ("POST", "/lfs/locks/verify"), ("GET", "/lfs/locks")):
            self.assertEqual(self.call(method, path, b"{}")[0], 403, (method, path))
        self.assertEqual(mini_lfs.object_path(self.store, oid).read_bytes(), b"keep")
        self.assertEqual(sorted(p.name for p in self.store.rglob("*") if p.is_file()), [oid])

    def test_traversal_and_odd_paths_stay_inside_the_store(self):
        (self.tmp / "secret").write_text("no")
        for path in ("/lfs/objects/../secret", "/lfs/objects/%2e%2e%2fsecret", "/lfs/objects/" + "A" * 64,
                     "/lfs/../secret", "/lfsx/objects/batch"):
            status, raw, _ = self.call("GET", path)
            self.assertEqual(status, 404, path)
            self.assertNotIn(b"no", raw.replace(b"\\u", b""))

    def test_an_unread_body_does_not_leak_into_the_next_request(self):
        c = http.client.HTTPConnection("127.0.0.1", self.port, timeout=10)
        status, _, r = self.call("PUT", "/lfs/objects/" + "2" * 64, b"GET /lfs/objects/batch HTTP/1.1\r\n\r\n", conn=c)
        self.assertEqual((status, r.getheader("Connection")), (403, "close"))
        c = http.client.HTTPConnection("127.0.0.1", self.port, timeout=10)
        self.assertEqual(self.batch([], operation="upload", conn=c)[0], 403)
        self.assertEqual(self.batch([], conn=c)[0], 200)

    def test_oversized_or_broken_batch_bodies(self):
        # 큰 본문은 다 받기 전에 끊는다 — 길이만 크게 적고 본문은 안 보낸 채로 답을 받는다.
        with socket.create_connection(("127.0.0.1", self.port), timeout=10) as s:
            s.sendall(f"POST /lfs/objects/batch HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {TOKEN}\r\n"
                      f"Content-Length: {mini_lfs.MAX_BODY + 1}\r\n\r\n".encode())
            self.assertTrue(s.recv(64).startswith(b"HTTP/1.1 413"))
        self.assertEqual(self.call("POST", "/lfs/objects/batch", b"{not json")[0], 422)
        self.assertEqual(self.call("POST", "/lfs/objects/batch", [1])[0], 422)
        self.assertEqual(self.batch([{"oid": "3" * 64, "size": 1}] * (mini_lfs.MAX_OBJECTS + 1))[0], 422)


@unittest.skipUnless(shutil.which("git-lfs") or Path.home().joinpath(".local/bin/git-lfs").exists(), "git-lfs 없음")
class RealGitLfsTests(Served):
    """진짜 git-lfs 가 이 창구에서 받고, 무토큰이면 멈추고, 올리기는 거부당하는가."""

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
        (src / "a.png").write_bytes(os.urandom(4096))
        (src / "b.png").write_bytes(os.urandom(100))
        self.git("add", ".", cwd=src)
        self.git("commit", "-q", "-m", "art", cwd=src)
        self.want = {n: (src / n).read_bytes() for n in ("a.png", "b.png")}
        for data in self.want.values():
            put_object(self.store, data)
        self.dst = self.tmp / "dst"
        self.git("clone", "-q", str(src), str(self.dst), cwd=self.tmp, env={**self.env, "GIT_LFS_SKIP_SMUDGE": "1"})
        self.assertLess(len((self.dst / "a.png").read_bytes()), 200)
        self.git("remote", "set-url", "origin", "https://example.invalid/kasaterm.git", cwd=self.dst)

    def mini_env(self, token=TOKEN):
        # release.yml build-msi 「Fetch LFS assets」 와 같은 모양 — 파일에 안 쓰고 이 명령의 환경으로만 준다.
        return {**self.env, "GIT_CONFIG_COUNT": "3",
                "GIT_CONFIG_KEY_0": "lfs.url", "GIT_CONFIG_VALUE_0": self.base,
                "GIT_CONFIG_KEY_1": f"http.{self.base}.extraheader", "GIT_CONFIG_VALUE_1": f"Authorization: Bearer {token}",
                "GIT_CONFIG_KEY_2": "credential.helper", "GIT_CONFIG_VALUE_2": ""}

    def pointers_left(self):
        out = self.git("lfs", "ls-files", cwd=self.dst).stdout
        return [line for line in out.splitlines() if line.split()[1:2] == ["-"]]

    def test_pull_fills_every_pointer_from_the_mini(self):
        self.git("lfs", "pull", cwd=self.dst, env=self.mini_env())
        self.assertEqual(self.pointers_left(), [])
        for name, data in self.want.items():
            self.assertEqual((self.dst / name).read_bytes(), data)
        self.assertNotIn(TOKEN, (self.dst / ".git/config").read_text())

    def test_without_the_token_the_pull_fails_instead_of_skipping(self):
        r = self.git("lfs", "pull", cwd=self.dst, env=self.mini_env("wrong-" + TOKEN), check=False)
        self.assertNotEqual(r.returncode, 0)
        self.assertEqual(len(self.pointers_left()), 2)

    def test_a_missing_object_fails_the_pull(self):
        oid = hashlib.sha256(self.want["b.png"]).hexdigest()
        mini_lfs.object_path(self.store, oid).unlink()
        r = self.git("lfs", "pull", cwd=self.dst, env=self.mini_env(), check=False)
        self.assertNotEqual(r.returncode, 0)
        self.assertEqual([line.split()[-1] for line in self.pointers_left()], ["b.png"])

    def test_push_to_the_mini_is_refused(self):
        self.git("lfs", "pull", cwd=self.dst, env=self.mini_env())
        (self.dst / "c.png").write_bytes(os.urandom(64))
        self.git("add", "c.png", cwd=self.dst)
        self.git("commit", "-q", "-m", "more", cwd=self.dst)
        r = self.git("lfs", "push", "--all", "origin", cwd=self.dst, env=self.mini_env(), check=False)
        self.assertNotEqual(r.returncode, 0)
        self.assertEqual(len([p for p in self.store.rglob("*") if p.is_file()]), 2)


class WorkflowTests(unittest.TestCase):
    """release.yml build-msi 가 미니 창구로 받는 계약 — 글자로 본다(yaml 모듈 없이)."""

    def setUp(self):
        wf = (REPO / ".github/workflows/release.yml").read_text()
        self.msi = wf[wf.index("  build-msi:"):wf.index("  build-dmg:")]
        self.rest = wf[wf.index("  build-dmg:"):]
        self.step = self.msi[self.msi.index("- name: Fetch LFS assets for the Windows build"):self.msi.index("- name: Rust toolchain")]

    def test_only_the_windows_fetch_step_talks_to_the_mini(self):
        self.assertIn("MINI_LFS_URL: ${{ vars.MINI_LFS_URL }}", self.step)
        self.assertIn("MINI_LFS_TOKEN: ${{ secrets.MINI_LFS_TOKEN }}", self.step)
        self.assertIn('GIT_CONFIG_KEY_0=lfs.url GIT_CONFIG_VALUE_0="$MINI_LFS_URL"', self.step)
        self.assertIn('GIT_CONFIG_KEY_1="http.$MINI_LFS_URL.extraheader"', self.step)
        self.assertIn("GIT_TERMINAL_PROMPT: '0'", self.step)
        self.assertNotIn("MINI_LFS", self.rest)
        self.assertNotIn("git config", self.step)
        self.assertFalse((REPO / ".lfsconfig").exists())

    def test_a_dark_or_refusing_mini_stops_the_build_loudly_before_the_pull(self):
        self.assertIn('[[ -n "$MINI_LFS_URL" && -n "$MINI_LFS_TOKEN" ]] || {', self.step)
        self.assertIn('[[ "$CODE" == 200 ]] || {', self.step)
        self.assertLess(self.step.index('[[ "$CODE" == 200 ]]'), self.step.index("git lfs pull"))
        self.assertIn("exit 1", self.step)
        self.assertNotIn("continue-on-error", self.msi)
        self.assertNotIn("|| true\n          git lfs pull", self.step)
