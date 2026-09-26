import base64
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import time
import unittest

from tools.release import deps
from tools.release import fastpatch as fp
from tools.release import nacho
from tools.release.backend import asset_names
from tools.release.common import Refused, feed_item, sha256_bytes
from tools.release.proc import Http, Result, Runner

# 이 검사는 진짜 원격·피드·기기·나쵸를 부르지 않는다. 원격은 임시 bare 저장소, 피드는 임시 파일, 기기는 이 프로세스가
# 연 가짜 `/version` 서버, 나쵸는 같은 계약을 흉내 낸 가짜 창구다. `git` 과 `openssl` 은 진짜로 돈다 — 태그 push 의
# 명령 구성·원격 대조와 Sparkle EdDSA 확인을 실제 도구로 본다. gh·codesign·hdiutil·cargo 만 가짜다.
# openssl 은 PATH 의 것이 아니라 `deps.find_openssl` 이 RFC 8032 벡터로 고른 것이다 — 없으면 까닭을 말하고 건너뛴다.

OPENSSL = deps.find_openssl(Runner("dry"))

REPO = Path(__file__).resolve().parents[3]
CONTROLLER = "mini-test"
DEVID = {"verified": True, "authority": "Developer ID Application: Test Org (ABCDE12345)", "team": "ABCDE12345", "notarized": True}
SELF = {"verified": True, "authority": "kasaterm-ci", "team": None, "notarized": False}


def sh(cwd, *args):
    return subprocess.run(args, cwd=cwd, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE).stdout.decode().strip()


class FakeDevice:
    def __init__(self, version, build, machine_id="dev-1"):
        self.answer = {"ok": True, "version": version, "build": build, "machine_id": machine_id}
        owner = self

        class H(BaseHTTPRequestHandler):
            def do_GET(self):
                body = json.dumps(owner.answer).encode()
                self.send_response(200 if self.path == "/version" else 404)
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *_):
                pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), H)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.base = f"http://127.0.0.1:{self.server.server_address[1]}"

    def close(self):
        self.server.shutdown()
        self.server.server_close()


class FakeNacho:
    """나쵸 `approvals.py`·`appserve.py` 의 승인 계약을 흉내 낸다 — 헤더 검사, 동작별 HTTP 창구, 해시 재계산, 한 번 소비."""

    base = "http://nacho.test"

    def __init__(self, key="k-test", local=CONTROLLER, actions=("kasaterm_restart", "kasaterm_release")):
        self.key, self.local, self.actions = key, local, set(actions)
        self.items, self.consumes, self.gets, self.resumes = {}, 0, 0, []

    def approve(self, scope, state="approved", ttl_ms=600_000, action=nacho.ACTION):
        aid = "ap_" + format(len(self.items) + 1, "032x")
        self.items[aid] = {"id": aid, "action": action, "scope": scope, "scope_hash": nacho.scope_hash(scope),
                           "state": state, "expires_at_ms": fp.now_ms() + ttl_ms, "consumed_at_ms": None, "consumed_by": None}
        return aid

    def request(self, method, url, headers=None, body=None, timeout=10):
        h = headers or {}
        if h.get("X-Nacho-Token") != self.key:
            return 403, b'{"error":"bad_token"}'
        if h.get("X-Kasa-Owner") != "1" or not h.get("X-Kasa-User"):
            return 403, b'{"error":"not_owner"}'
        if method == "POST" and (h.get("Content-Type") != "application/json" or h.get("X-Journal-Request") != "1"):
            return 415, b'{"error":"json_request_required"}'
        path = url[len(self.base):]
        out = lambda code, doc: (code, json.dumps(doc).encode())  # noqa: E731
        if path == "/api/app/capabilities":
            return out(200, {"ok": True, "approvals": {"enabled": True, "http_actions": sorted(self.actions)}})
        parts = path.split("/")
        a = self.items.get(parts[4]) if len(parts) > 4 else None
        if a is None or a["action"] not in self.actions:
            return out(404, {"error": "no_approval"})
        if method == "GET":
            self.gets += 1
            return out(200, {"ok": True, "approval": dict(a)})
        consumer = body.get("consumer_machine_id")
        if parts[-1] == "resume":
            # 아리스 계약: 소비된 것만, 같은 소비 기기·같은 해시, 7일 안, 이을 단계, 태그 부모 == 계획 커밋.
            if not a["consumed_at_ms"]:
                return out(409, {"error": "not_consumed"})
            if consumer != self.local or consumer != a["consumed_by"]:
                return out(409, {"error": "wrong_consumer"})
            if nacho.scope_hash(body.get("scope")) != a["scope_hash"]:
                return out(409, {"error": "scope_changed"})
            if fp.now_ms() - a["consumed_at_ms"] > nacho.KEEP_MS:
                return out(410, {"error": "resume_expired"})
            if body.get("stage") not in ("tag", "release", "feed", "devices"):
                return out(400, {"error": "bad_stage"})
            parent = (body.get("remote") or {}).get("tag_parent")
            if parent and parent != a["scope"]["commit"]:
                return out(409, {"error": "remote_mismatch"})
            self.resumes.append((body["stage"], body.get("remote")))
            return out(200, {"ok": True, "approval": dict(a)})
        if consumer != self.local:
            return out(409, {"error": "wrong_consumer"})
        if a["state"] != "approved":
            return out(409, {"error": f"not_approved:{a['state']}"})
        if a["consumed_at_ms"]:
            return out(409, {"error": "already_used"})
        if fp.now_ms() >= a["expires_at_ms"]:
            return out(410, {"error": "expired"})
        if nacho.scope_hash(body.get("scope")) != a["scope_hash"]:
            return out(409, {"error": "scope_changed"})
        if consumer != a["scope"].get("controller"):
            return out(409, {"error": "wrong_consumer"})
        self.consumes += 1
        a["consumed_at_ms"], a["consumed_by"] = fp.now_ms(), a["scope"]["controller"]
        return out(200, {"ok": True, "approval": dict(a)})


class RoutedHttp(Http):
    def __init__(self, nacho_fake):
        self.nacho = nacho_fake

    def request(self, method, url, headers=None, body=None, timeout=10):
        if url.startswith(FakeNacho.base):
            return self.nacho.request(method, url, headers, body, timeout)
        return super().request(method, url, headers, body, timeout)


class FakeRunner(Runner):
    """git·openssl 은 진짜로, 나머지는 fixture 가 정한 답으로."""

    def __init__(self, mode, fx):
        super().__init__(mode)
        self.fx = fx

    def execute(self, argv, cwd, timeout, env):
        tool = argv[0]
        if tool == "git" and "push" in argv and self.fx.push_answer:
            return self.fx.push_answer(lambda: super(FakeRunner, self).execute(argv, cwd, timeout, env))
        if tool in ("git", OPENSSL["path"]):
            return super().execute(argv, cwd, timeout, env)
        return self.fx.fake(argv, cwd, env)


class Fixture(unittest.TestCase):
    """v0.2.0 이 나간 뒤 네이티브 변경 하나·문서 변경 하나가 main 에 올라간 저장소."""

    ci_signing = "devid"

    def setUp(self):
        if not OPENSSL["path"]:
            raise unittest.SkipTest(OPENSSL["why"])
        self.openssl = OPENSSL["path"]
        self.tools = {"openssl": OPENSSL, "gh": {"path": "gh", "logged_in": True}, "cargo": {"path": "cargo"},
                      "codesign": {"path": "codesign"}, "spctl": {"path": "spctl"}, "hdiutil": {"path": "hdiutil"}}
        self.tmp = Path(tempfile.mkdtemp(prefix="fastpatch-"))
        self.origin, self.work = self.tmp / "origin.git", self.tmp / "work"
        sh(self.tmp, "git", "init", "-q", "--bare", "-b", "main", str(self.origin))
        sh(self.tmp, "git", "clone", "-q", str(self.origin), str(self.work))
        for k, v in (("user.name", "t"), ("user.email", "t@t"), ("commit.gpgsign", "false"), ("tag.gpgsign", "false")):
            sh(self.work, "git", "config", k, v)
        self.key = self.tmp / "ed.pem"
        sh(self.tmp, self.openssl, "genpkey", "-algorithm", "ed25519", "-out", str(self.key))
        der = subprocess.run([self.openssl, "pkey", "-in", str(self.key), "-pubout", "-outform", "DER"],
                             check=True, stdout=subprocess.PIPE).stdout
        self.pub = base64.b64encode(der[-32:]).decode()
        (self.work / "Cargo.toml").write_text('[workspace.package]\nversion = "0.1.19"\n')
        (self.work / "app/kasaterm/src").mkdir(parents=True)
        (self.work / "app/kasaterm/src/main.rs").write_text("fn main() {}\n")
        (self.work / "scripts").mkdir()
        (self.work / "scripts/build-app.sh").write_text(
            f"# Sparkle.framework\n<key>SUFeedURL</key>\n<string>https://x/appcast.xml</string>\n"
            f"<key>SUPublicEDKey</key>\n<string>{self.pub}</string>\n")
        (self.work / ".github/workflows").mkdir(parents=True)
        sign = ("          KASATERM_SIGN_ID: Developer ID Application: Test Org (ABCDE12345)\n          xcrun notarytool submit x\n"
                if self.ci_signing == "devid" else "          KASATERM_SIGN_ID: kasaterm-ci\n")
        (self.work / ".github/workflows/release.yml").write_text(sign)
        self.commit("first")
        self.release_tag("0.2.0")
        (self.work / "app/kasaterm/src/main.rs").write_text("fn main() { println!(); }\n")
        self.commit("fix: 작은 수정")
        (self.work / "docs").mkdir()
        (self.work / "docs/note.md").write_text("n\n")
        self.commit("docs: 메모")
        sh(self.work, "git", "push", "-q", "origin", "main")
        self.head = sh(self.work, "git", "rev-parse", "HEAD")
        self.feed, self.feed_win = self.tmp / "appcast.xml", self.tmp / "appcast-win.xml"
        self.write_feeds("0.2.0")
        self.state = self.tmp / "state"
        self.app = self.tmp / "Applications/kasaterm.app"
        self.app.mkdir(parents=True)
        self.identities = {str(self.app): dict(DEVID)}
        self.ci_actual = dict(DEVID)
        self.cargo_answer = lambda argv: Result(0, "test result: ok")
        self.push_answer = None
        self.ci_runs, self.assets, self.asset_bytes = [], {}, {}
        self.nacho = FakeNacho()
        self.http = RoutedHttp(self.nacho)
        self.devices, self.calls = [], []

    def tearDown(self):
        for d in self.devices:
            d.close()
        subprocess.run(["git", "-C", str(self.work), "worktree", "prune"])
        subprocess.run(["rm", "-rf", str(self.tmp)])

    # ── 저장소·피드 ─────────────────────────────────────────────────────────
    def commit(self, msg):
        sh(self.work, "git", "add", "-A")
        sh(self.work, "git", "commit", "-qm", msg)
        return sh(self.work, "git", "rev-parse", "HEAD")

    def release_tag(self, version):
        (self.work / "Cargo.toml").write_text(f'[workspace.package]\nversion = "{version}"\n')
        self.commit(f"chore(release): v{version}")
        sh(self.work, "git", "tag", f"v{version}")
        sh(self.work, "git", "push", "-q", "origin", "main", f"v{version}")

    def write_feeds(self, version, items=None):
        items = items or {}
        for path, platform in ((self.feed, "macos"), (self.feed_win, "windows")):
            i = items.get(platform, {})
            path.write_text(
                '<rss xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle"><channel><item>'
                f'<sparkle:version>{version}</sparkle:version>'
                f'<enclosure url="{i.get("url", "https://x/old")}" length="{i.get("length", 1)}" type="application/octet-stream" '
                f'sparkle:edSignature="{i.get("sig", "")}"/></item></channel></rss>')

    def sign(self, data):
        f = self.tmp / "tosign"
        f.write_bytes(data)
        sig = subprocess.run([self.openssl, "pkeyutl", "-sign", "-inkey", str(self.key), "-rawin", "-in", str(f)],
                             check=True, stdout=subprocess.PIPE).stdout
        return base64.b64encode(sig).decode()

    def ci_publish(self, plan, conclusion="success", status="completed", drop=(), corrupt_sig=False, feed=True):
        """태그가 선 뒤 release.yml 이 하는 일을 흉내 — 실행 기록, 릴리스 산출물, 서명된 두 피드."""
        tag = plan["tag"]
        bump = sh(self.work, "git", "ls-remote", "origin", f"refs/tags/{tag}").split()[0]
        self.ci_runs = [{"databaseId": 42, "status": status, "conclusion": conclusion if status == "completed" else None,
                         "headBranch": tag, "headSha": bump, "event": "push"}]
        items = {}
        for platform, name in asset_names(tag).items():
            if name in drop:
                continue
            data = f"{name} 몸통".encode()
            self.asset_bytes[name] = data
            self.assets[name] = {"name": name, "size": len(data), "digest": sha256_bytes(data)}
            sig = self.sign(b"x" + data if corrupt_sig else data)
            items[platform] = {"url": f"https://github.com/2rami/kasaterm/releases/download/{tag}/{name}",
                               "length": len(data), "sig": sig}
        if feed:
            self.write_feeds(plan["version"], items)

    # ── 가짜 도구 ──────────────────────────────────────────────────────────
    def fake(self, argv, cwd, env):
        self.calls.append(argv)
        tool, rest = os.path.basename(argv[0]), argv[1:]
        if tool == "codesign" and rest[0] == "--verify":
            return Result(0 if rest[-1] in self.identities else 1, "", "code object is not signed")
        if tool == "codesign" and rest[0] == "-dvv":
            i = self.identities.get(rest[-1]) or {}
            return Result(0, "", f"Authority={i.get('authority')}\nTeamIdentifier={i.get('team') or 'not set'}\n")
        if tool == "spctl":
            i = self.identities.get(argv[-1]) or {}
            return Result(0, "", "accepted\nsource=Notarized Developer ID") if i.get("notarized") else Result(3, "", "rejected")
        if tool == "cargo":
            assert env and env.get("CARGO_TARGET_DIR"), "검사는 격리 target 에서 돈다"
            return self.cargo_answer(argv)
        if tool == "bash" and rest == ["scripts/build-app.sh"]:
            out = Path(cwd) / "dist/kasaterm.app/Contents/MacOS"
            out.mkdir(parents=True, exist_ok=True)
            (out / "kasaterm").write_bytes(b"binary")
            self.identities[str(Path(cwd) / "dist/kasaterm.app")] = dict(DEVID)
            return Result(0)
        if tool == "gh" and rest[:2] == ["run", "list"]:
            return Result(0, json.dumps(self.ci_runs))
        if tool == "gh" and rest[:2] == ["release", "view"]:
            return Result(0, json.dumps({"assets": list(self.assets.values()), "isDraft": False})) if self.assets \
                else Result(1, "", "release not found")
        if tool == "gh" and rest[:2] == ["release", "download"]:
            name, out = rest[rest.index("--pattern") + 1], Path(rest[rest.index("--dir") + 1])
            (out / name).write_bytes(self.asset_bytes[name])
            return Result(0)
        if tool == "hdiutil" and rest[0] == "attach":
            mnt = Path(rest[rest.index("-mountpoint") + 1])
            (mnt / "kasaterm.app").mkdir(parents=True, exist_ok=True)
            self.identities[str(mnt / "kasaterm.app")] = dict(self.ci_actual)
            return Result(0)
        if tool == "hdiutil":
            return Result(0)
        return Result(127, "", f"검사에 없는 명령: {argv}")

    # ── 도우미 ─────────────────────────────────────────────────────────────
    def device(self, label, version, build, machine_id=None):
        d = FakeDevice(version, build, machine_id or f"id-{len(self.devices) + 1}")
        self.devices.append(d)
        return {"label": label, "base": d.base, "fake": d}

    def plan(self, devices=(), **kw):
        plan = fp.make_plan(self.work, feed=str(self.feed), feed_win=str(self.feed_win), http=self.http,
                            runner=FakeRunner("dry", self), installed_app=self.app, controller=CONTROLLER, tools=kw.pop("tools", self.tools),
                            devices=[{"label": d["label"], "base": d["base"]} for d in devices], **kw)
        fp.save_plan(plan, self.state)
        return plan

    def backend(self, plan, mode, devices=()):
        return fp.backend_for(self.work, plan, mode, self.state, http=self.http, runner=FakeRunner(mode, self),
                              devices=[{"label": d["label"], "base": d["base"]} for d in devices])

    def go(self, plan, mode="local", approval=None, devices=(), prepare=True):
        if mode == "live" and prepare and not fp.live_ready(plan, fp.load(plan["plan_id"], self.state)[1]) \
                and not plan["live_blocks"]:
            self.go(plan, "local", devices=devices)
        b = self.backend(plan, mode, devices)
        try:
            return fp.run(plan["plan_id"], b, self.state, approval_id=approval,
                          authority=nacho.NachoAuthority(FakeNacho.base, self.nacho.key, self.http))
        finally:
            fp.cleanup(b, plan)

    def remote(self, ref):
        out = sh(self.work, "git", "ls-remote", "origin", ref)
        return out.split()[0] if out else None

    def remote_tags(self):
        return sorted(line.split("/")[-1] for line in sh(self.work, "git", "ls-remote", "--tags", "origin").splitlines()
                      if not line.endswith("^{}"))


class PlanTests(Fixture):
    def test_next_patch_scope_and_feed_base_are_fixed_in_the_plan(self):
        dev = self.device("맥북", "0.2.0", sh(self.work, "git", "rev-parse", "v0.2.0")[:8], "dev-mac")
        plan = self.plan([dev])
        self.assertEqual((plan["errors"], plan["live_blocks"]), ([], []))
        self.assertEqual((plan["tag"], plan["commit"], plan["base"]["tag"]), ("v0.2.1", self.head, "v0.2.0"))
        self.assertEqual([c["subject"] for c in plan["changes"]["commits"]], ["docs: 메모", "fix: 작은 수정"])
        self.assertEqual(plan["platforms"], ["macos", "windows"])
        scope = plan["approval_scope"]
        self.assertEqual(sorted(scope), sorted(nacho.SCOPE_KEYS))
        self.assertIsNone(nacho.scope_problem(scope))
        self.assertEqual((scope["devices"], scope["controller"], scope["commit"]), (["dev-mac"], CONTROLLER, self.head))
        self.assertRegex(scope["feed_base"], r"^sha256:[0-9a-f]{64}$")
        self.assertEqual(plan["approval_scope_hash"], nacho.scope_hash(scope))

    def test_feeds_and_remote_tags_set_the_floor_not_local_tags(self):
        sh(self.work, "git", "tag", "-d", "v0.2.0")
        self.assertEqual(self.plan()["tag"], "v0.2.1")
        self.write_feeds("0.2.3")
        self.assertEqual(self.plan()["tag"], "v0.2.4")

    def test_existing_tag_lower_version_dirty_and_unpushed_are_refused(self):
        self.assertIn("원격에 이미 있다", " ".join(self.plan(version="0.2.0")["errors"]))
        self.assertIn("다운그레이드", " ".join(self.plan(version="0.1.30")["errors"]))
        (self.work / "app/kasaterm/src/main.rs").write_text("fn main() { loop {} }\n")
        self.assertIn("커밋 안 된 변경", " ".join(self.plan()["errors"]))
        self.commit("wip")
        self.assertIn("push 된 커밋만", " ".join(self.plan()["errors"]))

    def test_a_channel_the_updater_does_not_have_is_refused(self):
        self.assertIn("채널 preview 은 이 앱의 업데이터에 없다", " ".join(self.plan(channel="preview")["errors"]))

    def test_git_lfs_missing_in_an_lfs_repo_is_a_plan_error(self):
        (self.work / ".gitattributes").write_text("*.bin filter=lfs diff=lfs merge=lfs -text\n")
        self.commit("chore: lfs")
        sh(self.work, "git", "push", "-q", "origin", "main")
        plan = self.plan(tools={**self.tools, "git-lfs": {"path": None, "why": "git-lfs 을 찾지 못했다"}})
        self.assertIn("git-lfs 가 없다", " ".join(plan["errors"]))

    def test_mobile_changes_ask_for_a_testflight_build_outside_the_desktop_stages(self):
        (self.work / "mobile/lib").mkdir(parents=True)
        (self.work / "mobile/lib/a.dart").write_text("//\n")
        self.commit("feat: 폰")
        sh(self.work, "git", "push", "-q", "origin", "main")
        plan = self.plan()
        self.assertRegex(plan["ios_build"], r"^\d{10}$")
        self.assertNotIn("ios", plan["approval_scope"]["platforms"])

    def test_devices_are_compared_by_version_and_sha(self):
        old = sh(self.work, "git", "rev-parse", "v0.2.0")[:8]
        rows = [self.device("같은 번호 다른 판", "0.2.0", old), self.device("미커밋 판", "0.2.0", old + "+"),
                self.device("원격에 없는 판", "0.2.0", "84190e33"), self.device("더 새 판", "0.3.0", "abcdef12"),
                self.device("id 없음", "0.2.0", old, machine_id="")]
        rows[4]["fake"].answer["machine_id"] = None
        plan = self.plan(rows + [{"label": "꺼진 기기", "base": "http://127.0.0.1:9"}])
        states = {d["label"]: d["state"] for d in plan["baseline"]}
        self.assertEqual(states, {"같은 번호 다른 판": "update", "미커밋 판": "update", "원격에 없는 판": "hold",
                                  "더 새 판": "newer", "id 없음": "unscoped", "꺼진 기기": "offline"})
        self.assertEqual(plan["devices"], ["같은 번호 다른 판", "미커밋 판"])
        self.assertEqual(len(plan["approval_scope"]["devices"]), 2)

    def test_plan_id_is_bound_to_the_fixed_scope(self):
        a, b = self.plan(), self.plan()
        self.assertEqual(a["plan_id"], b["plan_id"])
        self.assertNotEqual(a["plan_id"], self.plan(version="0.2.5")["plan_id"])
        self.write_feeds("0.2.0", {"macos": {"url": "https://x/other"}})
        self.assertNotEqual(a["plan_id"], self.plan()["plan_id"])


class SelfSignedCiTests(Fixture):
    ci_signing = "self"

    def test_self_signed_unnotarized_ci_blocks_live_before_the_tag(self):
        plan = self.plan()
        self.assertEqual(plan["errors"], [])
        block = " ".join(plan["live_blocks"])
        self.assertIn("팀 서명 신원이 없다(자체 서명)", block)
        self.assertIn("태그부터 막는다", block)
        aid = self.nacho.approve(plan["approval_scope"])
        with self.assertRaisesRegex(Refused, "live 게시가 막혀 있다"):
            self.go(plan, "live", aid)
        self.assertEqual((self.remote_tags(), self.nacho.consumes), (["v0.2.0"], 0))
        # 검사·굽기와 게시 미리보기는 그대로 된다 — 막히는 것은 밖으로 나가는 단계뿐이다.
        state = self.go(plan, "local")
        self.assertEqual(state["stages"]["verify"]["status"], "done")
        self.assertEqual(state["stages"]["tag"]["status"], "dry")

    def test_an_unknown_installed_identity_also_blocks(self):
        self.identities.pop(str(self.app))
        (self.work / ".github/workflows/release.yml").write_text(
            "KASATERM_SIGN_ID: Developer ID Application: Test Org (ABCDE12345)\nnotarytool\n")
        self.commit("ci: devid")
        sh(self.work, "git", "push", "-q", "origin", "main")
        self.assertIn("설치본 서명 신원을 확인하지 못했다", " ".join(self.plan()["live_blocks"]))


class LocalRunTests(Fixture):
    def test_dry_run_reads_only_and_writes_nothing(self):
        plan = self.plan()
        b = self.backend(plan, "dry")
        state = fp.run(plan["plan_id"], b, self.state)
        self.assertFalse((fp.plan_dir(plan["plan_id"], self.state) / "state.json").exists())
        self.assertFalse(any(c[0] == "cargo" for c in self.calls))
        skipped = [" ".join(c["argv"]) for c in b.runner.calls if not c["ran"]]
        self.assertTrue(any("worktree add --detach" in s for s in skipped))
        self.assertIn("git push --atomic origin HEAD:refs/heads/main HEAD:refs/tags/v0.2.1", "\n".join(state["stages"]["tag"]["detail"]["would"]))
        self.assertEqual(state["stages"]["tag"]["detail"]["remote"]["main"], self.head)
        self.assertEqual(self.remote_tags(), ["v0.2.0"])

    def test_local_run_verifies_and_builds_for_real_and_only_previews_publishing(self):
        tags_before = sh(self.work, "git", "tag", "-l")
        state = self.go(self.plan())
        self.assertEqual([state["stages"][s]["status"] for s in fp.STAGES], ["done", "done", "dry", "dry", "dry", "done"])
        cargo = [c for c in self.calls if c[0] == "cargo"]
        self.assertEqual([c[3] for c in cargo], ["kasaterm", "kasa-mcp", "kasa-socket"])
        self.assertIn("built", state["stages"]["build"]["detail"])
        self.assertEqual(state["stages"]["build"]["detail"]["identity"]["team"], "ABCDE12345")
        self.assertEqual((self.remote_tags(), self.remote("refs/heads/main")), (["v0.2.0"], self.head))
        self.assertEqual(sh(self.work, "git", "tag", "-l"), tags_before)
        self.assertNotIn("wt", sh(self.work, "git", "worktree", "list"))

    def test_a_ready_build_of_the_same_commit_is_reverified_and_reused(self):
        dist = self.work / "dist"
        app = dist / f"kasaterm.app.ready-{self.head[:8]}/Contents/MacOS"
        app.mkdir(parents=True)
        (app / "kasaterm").write_bytes(b"ready")
        (dist / f"kasaterm.build.ready-{self.head[:8]}.json").write_text(json.dumps(
            {"source": {"source_commit": self.head, "dirty": False},
             "components": {"app": {"sha256": sha256_bytes(b"ready")[7:]}}}))
        self.identities[str(dist / f"kasaterm.app.ready-{self.head[:8]}")] = dict(DEVID)
        state = self.go(self.plan())
        self.assertIn("reused", state["stages"]["build"]["detail"])
        self.assertFalse(any(c[:2] == ["bash", "scripts/build-app.sh"] for c in self.calls))

    def test_failing_or_timed_out_tests_stop_with_the_command(self):
        plan = self.plan()
        self.cargo_answer = lambda argv: Result(101, "", "test foo ... FAILED") if argv[3] == "kasa-mcp" else Result(0)
        with self.assertRaisesRegex(Refused, "검사 실패: cargo test -p kasa-mcp"):
            self.go(plan)
        _, state = fp.load(plan["plan_id"], self.state)
        self.assertEqual(state["stages"]["verify"]["status"], "failed")
        self.cargo_answer = lambda argv: Result(None, timed_out=True)
        with self.assertRaisesRegex(Refused, "시간 초과"):
            self.go(plan)


class ApprovalTests(Fixture):
    def test_publishing_needs_a_nacho_approval_not_a_local_file(self):
        plan = self.plan()
        (fp.plan_dir(plan["plan_id"], self.state) / "approval.json").write_text(json.dumps({"approved": True}))
        with self.assertRaisesRegex(Refused, "나쵸 승인"):
            self.go(plan, "live")
        self.assertEqual(self.remote_tags(), ["v0.2.0"])

    def test_every_denial_leaves_the_remote_untouched(self):
        plan = self.plan()
        scope = plan["approval_scope"]
        cases = [
            (self.nacho.approve(scope, state="pending"), "not_approved:pending"),
            (self.nacho.approve(scope, ttl_ms=-1), "expired"),
            (self.nacho.approve({**scope, "devices": ["someone"]}), "scope_changed"),
            (self.nacho.approve(scope, action="kasaterm_restart"), "릴리스 승인이 아니다"),
            ("ap_nope", "approval id 모양"),
        ]
        for aid, word in cases:
            with self.subTest(word=word):
                with self.assertRaisesRegex(Refused, word):
                    self.go(plan, "live", aid)
        self.nacho.actions.discard(nacho.ACTION)
        with self.assertRaisesRegex(Refused, "kasaterm_release 승인 창구를 열지 않았다"):
            self.go(plan, "live", self.nacho.approve(scope))
        self.assertEqual((self.remote_tags(), self.nacho.consumes), (["v0.2.0"], 0))

    def test_a_feed_changed_after_the_plan_needs_a_new_plan(self):
        plan = self.plan()
        aid = self.nacho.approve(plan["approval_scope"])
        self.write_feeds("0.2.0", {"macos": {"url": "https://x/someone-else"}})
        with self.assertRaisesRegex(Refused, "계획 뒤에 피드가 바뀌었다"):
            self.go(plan, "live", aid)
        self.assertEqual(self.nacho.consumes, 0)

    def test_scope_contract_matches_the_nacho_side(self):
        plan = self.plan()
        s = plan["approval_scope"]
        self.assertIn("플랫폼", nacho.scope_problem({**s, "platforms": []}))
        self.assertIn("태그", nacho.scope_problem({**s, "tag": "v9.9.9"}))
        self.assertIn("칸", nacho.scope_problem({**s, "extra": 1}))
        self.assertIn("stable", nacho.scope_problem({**s, "channel": "preview"}))
        self.assertIn("기기", nacho.scope_problem({**s, "devices": ["b", "a"]}))


class LiveRunTests(Fixture):
    def test_one_approval_carries_tag_ci_feed_and_devices_with_resume(self):
        dev = self.device("맥북", "0.2.0", sh(self.work, "git", "rev-parse", "v0.2.0")[:8], "dev-mac")
        plan = self.plan([dev])
        aid = self.nacho.approve(plan["approval_scope"])
        state = self.go(plan, "live", aid, [dev])
        bump = self.remote(f"refs/tags/{plan['tag']}")
        self.assertEqual(self.remote("refs/heads/main"), bump)
        self.assertEqual(sh(self.work, "git", "rev-parse", f"{bump}^"), self.head)
        self.assertEqual(state["stages"]["release"]["status"], "waiting")
        self.assertIn("CI 가 아직 안 떴다", state["stages"]["release"]["detail"])
        self.assertEqual((self.nacho.consumes, state["approval"]["id"]), (1, aid))

        self.ci_publish(plan, status="in_progress", feed=False)
        state = self.go(plan, "live", aid, [dev])
        self.assertIn("CI 진행 중", state["stages"]["release"]["detail"])

        self.ci_publish(plan, feed=False)
        state = self.go(plan, "live", aid, [dev])
        self.assertEqual(state["stages"]["release"]["status"], "done")
        self.assertEqual(state["stages"]["release"]["detail"]["mac_identity"]["team"], "ABCDE12345")
        self.assertIn("CI 가 appcast 를 올리기를 기다린다", state["stages"]["feed"]["detail"])

        self.ci_publish(plan)
        state = self.go(plan, "live", aid, [dev])
        self.assertEqual([state["stages"][s]["status"] for s in fp.STAGES], ["done"] * 6)
        self.assertEqual(sorted(state["stages"]["feed"]["detail"]["hashes"]), ["macos", "windows"])
        self.assertEqual(state["devices"]["맥북"]["state"], "update")
        dev["fake"].answer.update(version="0.2.1", build=bump[:8])
        state = self.go(plan, "live", aid, [dev])
        self.assertEqual(state["devices"]["맥북"]["state"], "current")
        # 한 번 소비하고(소비 전 GET 1), 게시 단계를 이을 때마다 나쵸 resume 으로 대조만 했다(3) — 원격 사실을 싣는다.
        # 게시가 다 끝난 뒤의 기기 추적은 승인을 다시 대조하지 않는다.
        self.assertEqual((self.nacho.consumes, self.nacho.gets), (1, 1))
        self.assertEqual([st for st, _ in self.nacho.resumes], ["release", "release", "feed"])
        self.assertTrue(all(r["tag_parent"] == self.head and r["main"] == bump for _, r in self.nacho.resumes))
        self.assertEqual(self.remote_tags(), ["v0.2.0", "v0.2.1"])

    def test_resume_refuses_a_different_approval_or_a_stale_record(self):
        plan = self.plan()
        aid = self.nacho.approve(plan["approval_scope"])
        self.go(plan, "live", aid)
        other = self.nacho.approve(plan["approval_scope"])
        with self.assertRaisesRegex(Refused, "다른 승인"):
            self.go(plan, "live", other)
        p, state = fp.load(plan["plan_id"], self.state)
        state["approval"]["consumed_at_ms"] -= nacho.KEEP_MS + 1
        fp.save_state(plan["plan_id"], state, self.state)
        with self.assertRaisesRegex(Refused, "7일"):
            self.go(plan, "live", aid)

    def test_push_timeout_is_reconciled_from_the_remote(self):
        plan = self.plan()
        aid = self.nacho.approve(plan["approval_scope"])
        self.push_answer = lambda real: (real(), Result(None, timed_out=True))[1]
        state = self.go(plan, "live", aid)
        self.assertIn("원격에 반영돼 있었다", state["stages"]["tag"]["detail"]["note"])
        self.assertEqual(self.remote_tags(), ["v0.2.0", "v0.2.1"])

    def test_a_rejected_push_leaves_nothing_and_rerun_continues(self):
        plan = self.plan()
        aid = self.nacho.approve(plan["approval_scope"])
        self.push_answer = lambda real: Result(1, "", "remote rejected")
        with self.assertRaisesRegex(Refused, "원격은 그대로다"):
            self.go(plan, "live", aid)
        self.assertEqual((self.remote_tags(), self.remote("refs/heads/main")), (["v0.2.0"], self.head))
        self.push_answer = None
        state = self.go(plan, "live", aid)
        self.assertEqual(state["stages"]["tag"]["status"], "done")
        self.assertEqual(self.nacho.consumes, 1)

    def test_main_moving_after_the_plan_stops_before_push(self):
        plan = self.plan()
        aid = self.nacho.approve(plan["approval_scope"])
        (self.work / "app/kasaterm/src/main.rs").write_text("fn main() { todo!() }\n")
        self.commit("fix: 계획 뒤 변경")
        sh(self.work, "git", "push", "-q", "origin", "main")
        with self.assertRaisesRegex(Refused, "새 계획"):
            self.go(plan, "live", aid)
        self.assertEqual(self.remote_tags(), ["v0.2.0"])

    def test_our_own_tag_is_reconciled_after_a_lost_state_file(self):
        plan = self.plan()
        aid = self.nacho.approve(plan["approval_scope"])
        self.go(plan, "live", aid)
        (fp.plan_dir(plan["plan_id"], self.state) / "state.json").write_text(json.dumps(
            {"stages": {}, "devices": {}, "approval": fp.load(plan["plan_id"], self.state)[1]["approval"]}))
        state = self.go(plan, "live", aid)
        self.assertIn("다시 세우지 않음", state["stages"]["tag"]["detail"]["result"])
        self.assertEqual(self.remote_tags(), ["v0.2.0", "v0.2.1"])

    def test_a_foreign_tag_raised_after_the_plan_is_never_touched(self):
        plan = self.plan()
        aid = self.nacho.approve(plan["approval_scope"])
        sh(self.work, "git", "push", "-q", "origin", f"{self.head}:refs/tags/v0.2.1")
        with self.assertRaisesRegex(Refused, "이 계획의 버전 커밋이 아니다"):
            self.go(plan, "live", aid)
        self.assertEqual(self.remote("refs/tags/v0.2.1"), self.head)
        self.assertEqual(self.remote("refs/heads/main"), self.head)

    def test_ci_failure_and_bad_artifacts_never_reach_the_feed_check(self):
        cases = [
            (dict(conclusion="failure"), "CI 실패"),
            (dict(drop=("kasaterm-v0.2.1-windows-x86_64.msi",)), "산출물이 모자라다"),
        ]
        for kw, word in cases:
            with self.subTest(word=word):
                self.tearDown()
                self.setUp()
                plan = self.plan()
                aid = self.nacho.approve(plan["approval_scope"])
                self.go(plan, "live", aid)
                self.ci_publish(plan, **kw)
                with self.assertRaisesRegex(Refused, word):
                    self.go(plan, "live", aid)
                _, state = fp.load(plan["plan_id"], self.state)
                self.assertNotIn("feed", {k for k, v in state["stages"].items() if v["status"] != "dry"})

    def test_artifact_digest_and_dmg_identity_are_checked(self):
        plan = self.plan()
        aid = self.nacho.approve(plan["approval_scope"])
        self.go(plan, "live", aid)
        self.ci_publish(plan)
        self.assets["kasaterm-v0.2.1.dmg"]["digest"] = "sha256:" + "0" * 64
        with self.assertRaisesRegex(Refused, "해시가 릴리스 기록과 다르다"):
            self.go(plan, "live", aid)
        self.ci_publish(plan)
        self.ci_actual = dict(SELF)
        with self.assertRaisesRegex(Refused, "mac 판 서명 확인 — CI mac 판에 팀 서명 신원이 없다"):
            self.go(plan, "live", aid)

    def test_feed_signature_must_verify_with_the_repo_key(self):
        plan = self.plan()
        aid = self.nacho.approve(plan["approval_scope"])
        self.go(plan, "live", aid)
        self.ci_publish(plan, corrupt_sig=True)
        with self.assertRaisesRegex(Refused, "EdDSA 서명이 산출물과 맞지 않는다"):
            self.go(plan, "live", aid)
        self.ci_publish(plan)
        self.write_feeds("0.3.0")
        with self.assertRaisesRegex(Refused, "더 새 판"):
            self.go(plan, "live", aid)


def fake_bin(dir, name, body):
    path = Path(dir) / name
    path.write_text("#!/bin/sh\n" + body)
    path.chmod(0o755)
    return str(path)


LIBRE = 'case "$1" in version) echo "LibreSSL 3.3.6";; *) echo "unknown option -rawin" >&2; exit 1;; esac\n'
LIAR = 'case "$1" in version) echo "OpenSSL 3.0.0";; *) echo "Signature Verified Successfully";; esac\n'


class OpensslChoiceTests(unittest.TestCase):
    """나쵸 기본 셸은 /usr/bin(LibreSSL)이 앞이라 같은 명령이 다른 openssl 을 잡았다 — 기능을 재서 고르는지 본다."""

    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="ossl-")
        self.libre, self.liar = fake_bin(self.tmp, "libre", LIBRE), fake_bin(self.tmp, "liar", LIAR)

    def tearDown(self):
        subprocess.run(["rm", "-rf", self.tmp])

    def test_libressl_and_a_yes_man_are_both_refused_with_the_reason(self):
        got = deps.find_openssl(Runner("dry"), env={}, which=lambda _: self.libre, candidates=[self.liar])
        self.assertIsNone(got["path"])
        why = {r["path"]: r["why"] for r in got["rejected"]}
        self.assertIn("LibreSSL", why[self.libre])
        self.assertIn("틀린 서명도 통과", why[self.liar])
        self.assertIn("자동 설치하지 않는다", got["why"])

    @unittest.skipUnless(OPENSSL["path"], "Ed25519 를 검증하는 openssl 이 이 기기에 없다")
    def test_a_capable_openssl_after_libressl_on_path_is_chosen_by_absolute_path(self):
        got = deps.find_openssl(Runner("dry"), env={}, which=lambda _: self.libre, candidates=[OPENSSL["path"]])
        self.assertEqual(got["path"], OPENSSL["path"])
        self.assertTrue(os.path.isabs(got["path"]))
        self.assertIn("LibreSSL", got["rejected"][0]["why"])

    @unittest.skipUnless(OPENSSL["path"], "Ed25519 를 검증하는 openssl 이 이 기기에 없다")
    def test_an_explicit_choice_is_never_silently_replaced(self):
        got = deps.find_openssl(Runner("dry"), env={"KASATERM_OPENSSL": self.libre}, which=lambda _: OPENSSL["path"],
                                candidates=[OPENSSL["path"]])
        self.assertIsNone(got["path"])
        self.assertIn("KASATERM_OPENSSL 이 가리킨 것만", got["why"])

    def test_missing_publishing_tools_become_live_blocks(self):
        tools = {"openssl": {"path": None, "why": "Ed25519 를 검증할 openssl 이 없다"}, "gh": {"path": "gh", "logged_in": False},
                 "cargo": {"path": None, "why": "cargo 을 찾지 못했다"}, "codesign": {"path": "/usr/bin/codesign"},
                 "spctl": {"path": "/usr/sbin/spctl"}, "hdiutil": {"path": "/usr/bin/hdiutil"}}
        text = " ".join(deps.blocks(tools))
        self.assertIn("openssl", text)
        self.assertIn("gh 로그인이 없다", text)
        self.assertNotIn("cargo", text)


class ToolEnvTests(unittest.TestCase):
    def test_found_tool_folders_go_first_on_path_only_for_absolute_paths(self):
        tools = {"git-lfs": {"path": "/home/x/.local/bin/git-lfs"}, "cargo": {"path": "/home/x/.cargo/bin/cargo"},
                 "gh": {"path": "gh"}}
        env = deps.tool_env(tools, ("cargo", "git-lfs", "gh"), base={"PATH": "/usr/bin:/bin"})
        self.assertEqual(env["PATH"], "/home/x/.cargo/bin:/home/x/.local/bin:/usr/bin:/bin")
        self.assertEqual(deps.git_env({}, base={"PATH": "/usr/bin"})["PATH"], "/usr/bin")

    def test_a_lfs_repo_without_git_lfs_is_refused_by_the_plan(self):
        tmp = Path(tempfile.mkdtemp(prefix="lfs-"))
        try:
            (tmp / ".gitattributes").write_text("*.png filter=lfs diff=lfs merge=lfs -text\n")
            self.assertTrue(deps.uses_lfs(tmp))
            self.assertFalse(deps.uses_lfs(tmp / "none"))
        finally:
            subprocess.run(["rm", "-rf", str(tmp)])


class ContractTests(Fixture):
    def test_plan_core_keys_and_id_are_the_contract_nacho_recomputes(self):
        plan = self.plan()
        self.assertEqual(fp.CORE_KEYS, ("schema", "commit", "branch", "remote", "version", "tag", "channel", "platforms",
                                        "ios_build", "devices", "device_ids", "controller", "feed_base", "stages"))
        self.assertEqual(plan["schema"], "kasa-release-plan/2")
        raw = json.dumps({k: plan[k] for k in fp.CORE_KEYS}, sort_keys=True, ensure_ascii=False, separators=(",", ":"))
        import hashlib
        self.assertEqual(plan["plan_id"], hashlib.sha256(raw.encode()).hexdigest()[:16])
        for key in ("base", "changes", "errors", "live_blocks", "approval_scope", "approval_scope_hash", "tools"):
            self.assertIn(key, plan)
        self.assertEqual(sorted(plan["changes"]["files"]), ["docs", "feed", "infra", "mobile", "native"])
        self.assertEqual(plan["approval_scope"], nacho.release_scope(plan, CONTROLLER, plan["feed_base"]))

    def test_a_tampered_plan_is_never_published(self):
        plan = self.plan()
        self.go(plan)
        aid = self.nacho.approve(plan["approval_scope"])
        forged = {**plan, "devices": ["아무 기기"]}
        fp.save_plan(forged, self.state)
        self.assertFalse(fp.plan_hash_ok(forged))
        with self.assertRaisesRegex(Refused, "손댄 계획"):
            self.go(forged, "live", aid, prepare=False)
        self.assertEqual((self.remote_tags(), self.nacho.consumes), (["v0.2.0"], 0))

    def test_live_waits_for_verify_and_build_so_the_ten_minutes_are_not_spent_baking(self):
        plan = self.plan()
        aid = self.nacho.approve(plan["approval_scope"])
        with self.assertRaisesRegex(Refused, f"먼저 run {plan['plan_id']}"):
            self.go(plan, "live", aid, prepare=False)
        self.assertEqual(self.nacho.consumes, 0)
        self.assertFalse(fp.status_doc(plan, fp.load(plan["plan_id"], self.state)[1])["live_ready"])
        self.go(plan)
        doc = fp.status_doc(plan, fp.load(plan["plan_id"], self.state)[1])
        self.assertTrue(doc["live_ready"] and doc["plan_hash_ok"])
        self.assertEqual({k: v["status"] for k, v in doc["stages"].items()},
                         {"verify": "done", "build": "done", "tag": "dry", "release": "dry", "feed": "dry", "devices": "done"})
        self.assertIsNone(doc["approval"])

    def test_only_the_nacho_tool_may_run_live(self):
        self.assertIn("창 안에서는", fp.invoker_problem({"KASATERM_PANE_ID": "%3", "KASATERM_RELEASE_INVOKER": "nacho-tool"}))
        self.assertIn("CLAUDECODE", fp.invoker_problem({"CLAUDECODE": "1", "KASATERM_RELEASE_INVOKER": "nacho-tool"}))
        self.assertIn("나쵸 도구만", fp.invoker_problem({}))
        self.assertIsNone(fp.invoker_problem({"KASATERM_RELEASE_INVOKER": "nacho-tool"}))
        plan = self.plan()
        from unittest import mock
        import contextlib
        import io
        with mock.patch.dict(os.environ, {"KASATERM_PANE_ID": "%9", "NACHO_ASK_URL": "http://127.0.0.1:9"}), \
                contextlib.redirect_stderr(io.StringIO()):
            code = fp.main(["--state-dir", str(self.state), "run", plan["plan_id"], "--live", "--approval", "ap_" + "0" * 32,
                            "--repo", str(self.work)])
        self.assertEqual((code, self.remote_tags()), (2, ["v0.2.0"]))


class ResumeTests(Fixture):
    def test_a_moved_remote_tag_is_refused_by_nacho_resume(self):
        plan = self.plan()
        aid = self.nacho.approve(plan["approval_scope"])
        self.go(plan, "live", aid)
        old = sh(self.work, "git", "rev-parse", "v0.2.0")
        sh(self.work, "git", "push", "-q", "-f", "origin", f"{old}:refs/tags/{plan['tag']}")
        with self.assertRaisesRegex(Refused, "remote_mismatch"):
            self.go(plan, "live", aid)

    def test_an_unconsumed_approval_is_never_resumed(self):
        plan = self.plan()
        aid = self.nacho.approve(plan["approval_scope"])
        self.go(plan, "live", aid)
        self.nacho.items[aid].update(consumed_at_ms=None, consumed_by=None)
        with self.assertRaisesRegex(Refused, "not_consumed"):
            self.go(plan, "live", aid)

    def test_a_pinned_openssl_that_stops_working_stops_the_feed_check(self):
        plan = self.plan()
        aid = self.nacho.approve(plan["approval_scope"])
        self.go(plan, "live", aid)
        self.ci_publish(plan)
        libre = fake_bin(self.tmp, "libre", LIBRE)
        broken = {**plan, "tools": {**plan["tools"], "openssl": {"path": libre, "version": "OpenSSL 3"}}}
        fp.save_plan(broken, self.state)
        with self.assertRaisesRegex(Refused, "계획에 고정한 openssl"):
            self.go(broken, "live", aid)
        self.assertFalse(any(os.path.basename(c[0]) == "openssl" for c in self.calls))


class DevicePlanTests(Fixture):
    """원격 받기·설치 예약은 dry-run 뿐 — 앱 재시작 사실을 다시 써서 기기마다 무엇이 막혔는지 정확히 말하는지 본다."""

    def facts(self, mid, osname="macos", busy=(), capability=0, refusals=(), enabled=False):
        return {"machine_id": mid, "hash": "t" + mid, "facts": {"machine_id": mid, "os": osname, "pid": 4242,
                                                              "app_path": "/Users/x/Applications/kasaterm.app", "busy": list(busy),
                                                              "update_capability": capability, "update_enabled": enabled},
                "refusals": [{"code": c} for c in refusals]}

    def test_every_device_is_blocked_until_the_update_endpoint_exists(self):
        old = sh(self.work, "git", "rev-parse", "v0.2.0")[:8]
        devs = [self.device("미니", "0.2.0", old, "mac-1"), self.device("바쁜 맥", "0.2.0", old, "mac-2"),
                self.device("윈도", "0.2.0", old, "win-1"), self.device("꺼진 뒤", "0.2.0", old, "gone")]
        plan = self.plan(devs)
        state = {"stages": {"release": {"status": "done", "detail": {"assets": {
            "macos": {"name": "kasaterm-v0.2.1.dmg", "size": 10, "sha256": "sha256:" + "a" * 64},
            "windows": {"name": "kasaterm-v0.2.1-windows-x86_64.msi", "size": 20, "sha256": "sha256:" + "b" * 64}}}}}}
        targets = {"mac-1": self.facts("mac-1"),
                   "mac-2": self.facts("mac-2", refusals=("busy_students",)),
                   "win-1": self.facts("win-1", "windows", refusals=("unsupported_os",))}
        rows = {r["label"]: r for r in fp.devices.plan_devices(plan, state, targets)}
        self.assertTrue(all(r["status"] == "blocked" for r in rows.values()))
        codes = {k: [x["code"] for x in r["reasons"]] for k, r in rows.items()}
        self.assertEqual(codes["미니"], ["update_endpoint_missing"])
        self.assertEqual(codes["바쁜 맥"], ["update_endpoint_missing", "busy_students"])
        self.assertEqual(codes["윈도"], ["update_endpoint_missing"])
        self.assertEqual(codes["꺼진 뒤"], ["unreachable"])
        self.assertEqual(len(rows["꺼진 뒤"]["steps"]), 1)
        gone = fp.devices.plan_devices(plan, state, {"gone": {"machine_id": "gone", "facts": None,
                                                              "refusals": [{"code": "unreachable", "reason": "터널 없음"}]}})
        self.assertEqual([(x["code"], x["why"]) for x in next(r for r in gone if r["machine_id"] == "gone")["reasons"]], [("unreachable", "터널 없음")])
        mac = rows["미니"]["steps"]
        self.assertEqual(len(mac), 6)
        self.assertIn("10바이트 · sha256:aaaaaaaaaaaa", mac[1])
        self.assertIn("팀 ABCDE12345", mac[1])
        self.assertIn(".kasaterm.app.previous", mac[3])
        self.assertIn("강제 종료 없음", mac[4])
        self.assertIn("WinSparkle", rows["윈도"]["steps"][3])
        self.assertIn("releases/download/v0.2.1/kasaterm-v0.2.1.dmg", rows["미니"]["fallback"])

    def test_once_the_endpoint_exists_waits_defer_and_signing_blocks(self):
        dev = self.device("맥", "0.2.0", sh(self.work, "git", "rev-parse", "v0.2.0")[:8], "mac-1")
        plan = self.plan([dev])
        state = {"stages": {"release": {"status": "done", "detail": {"assets": {
            "macos": {"name": "kasaterm-v0.2.1.dmg", "size": 10, "sha256": "sha256:" + "a" * 64}}}}}}
        on = {"capability": 1, "enabled": True}
        off = fp.devices.plan_devices(plan, state, {"mac-1": self.facts("mac-1", capability=1)})
        self.assertEqual((off[0]["status"], [x["code"] for x in off[0]["reasons"]]), ("blocked", ["update_disabled"]))
        ready = fp.devices.plan_devices(plan, state, {"mac-1": self.facts("mac-1", **on)})
        self.assertEqual(ready[0]["status"], "ready")
        wait = fp.devices.plan_devices(plan, state, {"mac-1": self.facts("mac-1", refusals=("unsaved_editors",), **on)})
        self.assertEqual(wait[0]["status"], "deferred")
        self.assertEqual(fp.devices.plan_devices(plan, state, {"mac-1": self.facts("mac-1", refusals=("capability_missing",), **on)})[0]["status"],
                         "ready", "재시작 창구 판은 업데이트를 막지 않는다")
        unverified = fp.devices.plan_devices(plan, {"stages": {}}, {"mac-1": self.facts("mac-1", **on)})
        self.assertEqual([x["code"] for x in unverified[0]["reasons"]], ["artifacts_unverified"])
        signed = {**plan, "live_blocks": ["mac 서명: 자체 서명"]}
        self.assertEqual(fp.devices.plan_devices(signed, state, {"mac-1": self.facts("mac-1", **on)})[0]["status"], "blocked")

    def test_the_update_job_is_the_shape_the_device_accepts(self):
        dev = self.device("맥", "0.2.0", sh(self.work, "git", "rev-parse", "v0.2.0")[:8], "mac-1")
        plan = self.plan([dev])
        release = {"status": "done", "detail": {"assets": {"macos": {"name": "kasaterm-v0.2.1.dmg", "size": 10, "sha256": "sha256:" + "a" * 64}}}}
        target = self.facts("mac-1", capability=1, enabled=True)
        job, why = fp.devices.update_job(plan, {"stages": {"release": release}}, target, 1000)
        self.assertIsNone(job)
        self.assertIn("EdDSA", why, "피드 단계가 서명을 확인하기 전엔 작업을 짓지 않는다")
        state = {"stages": {"release": release, "feed": {"status": "done", "detail": {"signatures": {"macos": "c2ln"}}}}}
        job, why = fp.devices.update_job(plan, state, target, 1000)
        self.assertIsNone(why)
        self.assertEqual(job["asset"]["url"], "https://github.com/2rami/kasaterm/releases/download/v0.2.1/kasaterm-v0.2.1.dmg")
        self.assertEqual((job["schema"], job["tag"], job["version"], job["team"], job["old_pid"], job["target_hash"], job["build"]),
                         ("kasaterm-update/1", "v0.2.1", "0.2.1", "ABCDE12345", 4242, "tmac-1", plan["commit"]))
        self.assertEqual(job["job_id"], fp.devices.update_job_id(plan["plan_id"], "mac-1", "sha256:" + "a" * 64))
        scope = fp.devices.update_scope(job, plan["controller"])
        self.assertEqual((scope["action"], scope["targets"], scope["asset"]["sha256"]),
                         ("kasaterm_update", [{"order": 1, "machine_id": "mac-1", "hash": "tmac-1"}], "sha256:" + "a" * 64))
        self.assertIn("mac 만", fp.devices.update_job(plan, state, self.facts("win-1", "windows"), 1000)[1])
        # 기기(kasa_socket::app_update `the_job_id_matches_the_controller_side`)와 같은 값.
        self.assertEqual(fp.devices.update_job_id("abcdef0123456789", "mac-1", "sha256:" + "0" * 64), "upd4e7a732ffa7a1c8")

    def test_facts_come_from_the_app_restart_plan_read_only(self):
        answer = {"targets": [self.facts("mac-1")]}
        runner = FakeRunner("dry", self)
        self.fake_cli = answer
        orig = self.fake

        def fake(argv, cwd, env):
            if os.path.basename(argv[0]) == "kasaterm-cli":
                self.calls.append(argv)
                return Result(0, json.dumps(answer))
            return orig(argv, cwd, env)
        self.fake = fake
        got, problem = fp.devices.gather_facts(runner, ["mac-1", "win-1"], cli="/x/kasaterm-cli")
        self.assertIsNone(problem)
        self.assertEqual(list(got), ["mac-1"])
        self.assertEqual(self.calls[-1], ["/x/kasaterm-cli", "app-restart", "plan", "--machine", "mac-1,win-1", "--json"])
        self.assertEqual(runner.calls[-1]["kind"], "read")


NACHO_FIXTURE = Path(os.environ.get("NACHO_REPO") or Path.home() / "Desktop/momewomo/nacho-neko") \
    / "docs/development/api/fixtures/approval.kasaterm_release.implemented.json"


class CaptureHttp(Http):
    def __init__(self, answers):
        self.answers, self.seen = answers, []

    def request(self, method, url, headers=None, body=None, timeout=10):
        self.seen.append((method, url, dict(headers or {}), body))
        status, doc = self.answers(method, url)
        return status, json.dumps(doc).encode()


@unittest.skipUnless(NACHO_FIXTURE.exists(), "나쵸 저장소의 대조 자료(approval.kasaterm_release.implemented.json)가 없다")
class NachoFixtureTests(unittest.TestCase):
    """나쵸 쪽 구현(94a3033)이 낸 대조 자료에 이 카사텀 코드를 그대로 댄다 — 한쪽만 바뀌면 여기서 깨진다."""

    def setUp(self):
        self.fx = json.loads(NACHO_FIXTURE.read_text())
        self.plan = self.fx["plan"]

    def test_core_keys_plan_id_scope_and_hash_agree(self):
        self.assertEqual(tuple(self.fx["plan_core_keys"]), fp.CORE_KEYS)
        self.assertTrue(fp.plan_hash_ok(self.plan))
        scope = nacho.release_scope(self.plan, self.plan["controller"], self.plan["feed_base"])
        self.assertEqual(scope, self.plan["approval_scope"])
        self.assertEqual(nacho.scope_hash(scope), self.plan["approval_scope_hash"])
        self.assertIsNone(nacho.scope_problem(scope))

    def test_status_json_carries_every_field_nacho_reads(self):
        want = self.fx["status_json_ready"]
        state = {"stages": {"verify": {"status": "done"}, "build": {"status": "done"}}, "remote": want["remote"]}
        doc = fp.status_doc(self.plan, state)
        self.assertTrue(set(want) <= set(doc))
        self.assertEqual((doc["plan_id"], doc["plan_hash_ok"], doc["stages"], doc["approval"], doc["live_ready"], doc["live_blocks"]),
                         (want["plan_id"], want["plan_hash_ok"], want["stages"], want["approval"], want["live_ready"], want["live_blocks"]))

    def test_get_consume_and_resume_requests_are_what_nacho_expects(self):
        fx = self.fx
        answers = lambda method, url: (200, fx["resume" if url.endswith("/resume") else "consume" if url.endswith("/consume") else "get"]["200"])  # noqa: E731
        http = CaptureHttp(answers)
        auth = nacho.NachoAuthority("http://nacho.test", "k", http)
        aid = fx["get"]["request"]["path"].rsplit("/", 1)[-1]
        body_c, body_r = fx["consume"]["request"]["body"], fx["resume"]["request"]["body"]
        auth.get(aid)
        auth.consume(aid, body_c["scope"], body_c["consumer_machine_id"])
        auth.resume(aid, body_r["scope"], body_r["consumer_machine_id"], body_r["stage"], body_r["remote"])
        for (method, url, headers, body), key in zip(http.seen, ("get", "consume", "resume")):
            req = fx[key]["request"]
            self.assertEqual((method, url[len("http://nacho.test"):]), (req["method"], req["path"]))
            self.assertEqual({k: v for k, v in headers.items() if k != "X-Nacho-Token"},
                             {k: v for k, v in req["headers"].items() if k != "X-Nacho-Token"})
            self.assertEqual(body, req.get("body"))
        self.assertEqual(set(body_r["remote"]), {"main", "tag_parent"})

    def test_every_nacho_error_word_reaches_the_person(self):
        for key in ("get", "consume", "resume"):
            for code, doc in self.fx[key].items():
                if code in ("request", "200"):
                    continue
                auth = nacho.NachoAuthority("http://nacho.test", "k", CaptureHttp(lambda m, u, c=code, d=doc: (int(c[:3]), d)))
                with self.subTest(key=key, code=code), self.assertRaisesRegex(nacho.Denied, doc["error"].split(":")[0]):
                    auth._call("GET", "/api/app/approvals/x")


class RealRepoTests(unittest.TestCase):
    def test_capabilities_match_the_updaters_in_this_repo(self):
        caps = fp.capabilities(REPO)
        self.assertEqual(caps["macos"]["channels"], ["stable"])
        self.assertEqual(caps["macos"]["feed"], fp.MAC_FEED)
        ci = caps["macos"]["ci_identity"]
        self.assertEqual((ci["authority"], ci["team"], ci["notarized"]), ("kasaterm-ci", None, False))
        win = (REPO / "app/kasaterm/src/win_sparkle.rs").read_text()
        self.assertIn(caps["macos"]["ed_public_key"], win)
        self.assertEqual(caps["windows"]["channels"], ["stable"])
        self.assertFalse(caps["ios"]["hotpatch"])

    def test_tunnel_ports_match_machines_rs(self):
        self.assertEqual((fp.tunnel_port("맥북"), fp.tunnel_port("windesktop")), (18986, 18917))

    def test_classify_splits_native_mobile_and_non_shipping_files(self):
        kinds = fp.classify(["app/kasaterm/src/a.rs", "crates/kasa-mcp/src/b.rs", "mobile/lib/c.dart",
                             "docs/d.md", "docs/appcast.xml", "scripts/e.sh", "README.md"])
        self.assertEqual(kinds["native"], ["app/kasaterm/src/a.rs", "crates/kasa-mcp/src/b.rs"])
        self.assertEqual(kinds["mobile"], ["mobile/lib/c.dart"])
        self.assertEqual(kinds["feed"], ["docs/appcast.xml"])
        self.assertEqual(kinds["docs"], ["docs/d.md", "README.md"])

    def test_the_real_feed_item_parser_reads_both_appcasts(self):
        mac = feed_item((REPO / "docs/appcast.xml").read_bytes())
        win = feed_item((REPO / "docs/appcast-win.xml").read_bytes())
        self.assertTrue(mac["url"].endswith(".dmg") and mac["length"] and mac["signature"])
        self.assertTrue(win["url"].endswith(".msi") and win["length"] and win["signature"])


if __name__ == "__main__":
    unittest.main()
