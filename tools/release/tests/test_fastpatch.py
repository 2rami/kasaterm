from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import subprocess
import tempfile
import threading
import time
import unittest

from tools.release import fastpatch as fp
from tools.release import nacho
from tools.release.backend import asset_names
from tools.release.common import Refused, feed_item, sha256_bytes
from tools.release.proc import Http, Result, Runner

# 이 검사는 진짜 원격·피드·기기·나쵸를 부르지 않는다. 원격은 임시 bare 저장소, 피드는 임시 파일, 기기는 이 프로세스가
# 연 가짜 `/version` 서버, 나쵸는 같은 계약을 흉내 낸 가짜 창구다. `git` 과 `openssl` 은 진짜로 돈다 — 태그 push 의
# 명령 구성·원격 대조와 Sparkle EdDSA 확인을 실제 도구로 본다. gh·codesign·hdiutil·cargo 만 가짜다.

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
        self.items, self.consumes, self.gets = {}, 0, 0

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
        if tool in ("git", "openssl"):
            return super().execute(argv, cwd, timeout, env)
        return self.fx.fake(argv, cwd, env)


class Fixture(unittest.TestCase):
    """v0.2.0 이 나간 뒤 네이티브 변경 하나·문서 변경 하나가 main 에 올라간 저장소."""

    ci_signing = "devid"

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="fastpatch-"))
        self.origin, self.work = self.tmp / "origin.git", self.tmp / "work"
        sh(self.tmp, "git", "init", "-q", "--bare", "-b", "main", str(self.origin))
        sh(self.tmp, "git", "clone", "-q", str(self.origin), str(self.work))
        for k, v in (("user.name", "t"), ("user.email", "t@t"), ("commit.gpgsign", "false"), ("tag.gpgsign", "false")):
            sh(self.work, "git", "config", k, v)
        self.key = self.tmp / "ed.pem"
        sh(self.tmp, "openssl", "genpkey", "-algorithm", "ed25519", "-out", str(self.key))
        der = subprocess.run(["openssl", "pkey", "-in", str(self.key), "-pubout", "-outform", "DER"],
                             check=True, stdout=subprocess.PIPE).stdout
        import base64
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
        sig = subprocess.run(["openssl", "pkeyutl", "-sign", "-inkey", str(self.key), "-rawin", "-in", str(f)],
                             check=True, stdout=subprocess.PIPE).stdout
        import base64
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
        tool, rest = argv[0], argv[1:]
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
                            runner=FakeRunner("dry", self), installed_app=self.app, controller=CONTROLLER,
                            devices=[{"label": d["label"], "base": d["base"]} for d in devices], **kw)
        fp.save_plan(plan, self.state)
        return plan

    def backend(self, plan, mode, devices=()):
        return fp.backend_for(self.work, plan, mode, self.state, http=self.http, runner=FakeRunner(mode, self),
                              devices=[{"label": d["label"], "base": d["base"]} for d in devices])

    def go(self, plan, mode="local", approval=None, devices=()):
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
        # 한 번 소비하고(소비 전 GET 1), 게시 단계를 이을 때마다 나쵸 기록을 읽어 대조만 했다(3). 게시가 다 끝난 뒤의
        # 기기 추적은 승인을 다시 읽지 않는다.
        self.assertEqual((self.nacho.consumes, self.nacho.gets), (1, 4))
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
