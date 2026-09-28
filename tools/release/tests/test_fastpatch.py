import base64
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import time
import contextlib
import io
import unittest
from unittest import mock

import plistlib
import re
import shutil
import sys

from tools.release import common, deps
from tools.release import fastpatch as fp
from tools.release import macsign
from tools.release import nacho
from tools.release.backend import asset_names
from tools.release.common import Refused, feed_item, sha256_bytes
from tools.release.proc import Http, Result, Runner
from tools.request_journal import build_manifest as proof

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


def feed_xml(version, item=None):
    item = item or {}
    return ('<rss xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle"><channel><item>'
            f'<sparkle:version>{version}</sparkle:version>'
            f'<enclosure url="{item.get("url", "https://x/old")}" length="{item.get("length", 1)}" '
            f'sparkle:edSignature="{item.get("sig", "")}"/></item></channel></rss>')


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
        self.release_prerelease = False
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
            path.write_text(feed_xml(version, i))

    def sign(self, data):
        f = self.tmp / "tosign"
        f.write_bytes(data)
        sig = subprocess.run([self.openssl, "pkeyutl", "-sign", "-inkey", str(self.key), "-rawin", "-in", str(f)],
                             check=True, stdout=subprocess.PIPE).stdout
        return base64.b64encode(sig).decode()

    def ci_publish(self, plan, conclusion="success", status="completed", drop=(), corrupt_sig=False, feed=True, keep=()):
        """태그가 선 뒤 release.yml 이 하는 일을 흉내 — 실행 기록, 릴리스 산출물, 서명된 두 피드."""
        tag = plan["tag"]
        bump = sh(self.work, "git", "ls-remote", "origin", f"refs/tags/{tag}").split()[0]
        self.ci_runs = [{"databaseId": 42, "status": status, "conclusion": conclusion if status == "completed" else None,
                         "headBranch": tag, "headSha": bump, "event": "push"}]
        items = {}
        self.release_prerelease = plan["channel"] == "preview"
        for platform, name in asset_names(tag).items():
            if platform not in plan["platforms"]:
                continue
            if name in drop:
                continue
            # keep: 조종 기기가 이미 올린 산출물(로컬 mac 판) — CI 는 그것을 굽지 않고 그대로 싣는다.
            data = self.asset_bytes[name] if name in keep else f"{name} 몸통".encode()
            self.asset_bytes[name] = data
            self.assets[name] = {"name": name, "size": len(data), "digest": sha256_bytes(data)}
            sig = self.sign(b"x" + data if corrupt_sig else data)
            items[platform] = {"url": f"https://github.com/2rami/kasaterm/releases/download/{tag}/{name}",
                               "length": len(data), "sig": sig}
        if feed:
            if plan["channel"] == "preview":
                Path(plan["feed"]["source"]).write_text(feed_xml(plan["version"], items["macos"]))
            else:
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
            return Result(0, json.dumps({"assets": list(self.assets.values()), "isDraft": False,
                                        "isPrerelease": self.release_prerelease})) if self.assets \
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
        plan = fp.make_plan(self.work, feed=kw.pop("feed", str(self.feed)), feed_win=kw.pop("feed_win", str(self.feed_win)), http=self.http,
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
        cargo = [c for c in self.calls if c[0] == "cargo" and c[1] == "test"]
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

    def test_a_finish_run_from_main_continues_a_tag_run_that_stopped(self):
        # 2026-09-27 v0.2.1: 태그 실행이 체크아웃의 LFS 예산 초과로 멈췄다. 태그의 워크플로는 못 고치니 main 의 워크플로로
        # 그 태그를 마무리한다(`release vX (both|macos)`) — 가장 최근 것을 본다.
        plan = self.plan()
        aid = self.nacho.approve(plan["approval_scope"])
        self.go(plan, "live", aid)
        self.ci_publish(plan, conclusion="failure", feed=False)
        failed = dict(self.ci_runs[0])
        with self.assertRaisesRegex(Refused, "CI 실패"):
            self.go(plan, "live", aid)
        self.assets.pop("kasaterm-v0.2.1-windows-x86_64.msi")      # mac 만 마무리한 릴리스 — msi 가 없다
        self.ci_runs = [{"databaseId": 43, "status": "completed", "conclusion": "success", "headBranch": "main",
                         "event": "workflow_dispatch", "displayTitle": "release v0.2.1 (macos)"}, failed]
        with self.assertRaisesRegex(Refused, "산출물이 모자라다\\(windows\\)"):
            self.go(plan, "live", aid)
        self.ci_publish(plan, feed=False)
        self.ci_runs = [{"databaseId": 44, "status": "completed", "conclusion": "success", "headBranch": "main",
                         "event": "workflow_dispatch", "displayTitle": "release v0.2.1 (both)"},
                        {"databaseId": 45, "status": "completed", "conclusion": "success", "headBranch": "main",
                         "event": "workflow_dispatch", "displayTitle": "release v0.2.10 (both)"}, failed]
        state = self.go(plan, "live", aid)
        self.assertEqual((state["stages"]["release"]["status"], state["stages"]["release"]["detail"]["run"]), ("done", 44))
        # 이름이 비슷한 다른 태그·수동 빌드 검증 실행은 그 태그의 것으로 치지 않는다.
        b = self.backend(plan, "dry")
        self.ci_runs = [{"databaseId": 46, "status": "completed", "conclusion": "success", "headBranch": "main",
                         "event": "workflow_dispatch", "displayTitle": "release v0.2.10 (both)"},
                        {"databaseId": 47, "status": "completed", "conclusion": "success", "headBranch": "main",
                         "event": "workflow_dispatch", "displayTitle": "Release main"}]
        self.assertIsNone(b.ci_run("v0.2.1"))

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

    def rollout_case(self):
        old = sh(self.work, "git", "rev-parse", "v0.2.0")[:8]
        devs = [self.device("미니", "0.2.0", old, "mac-1"), self.device("맥북", "0.2.0", old, "mac-2"),
                self.device("윈도", "0.2.0", old, "win-1"), self.device("꺼진 맥", "0.2.0", old, "mac-3")]
        plan = self.plan(devs)
        plan["controller"] = "mac-1"
        plan["plan_id"] = fp.sha256_bytes(fp.canonical(fp.core_of(plan)))[7:23]
        release = {"status": "done", "detail": {"assets": {"macos": {"name": "kasaterm-v0.2.1.dmg", "size": 10, "sha256": "sha256:" + "a" * 64}}}}
        state = {"stages": {"release": release, "feed": {"status": "done", "detail": {"signatures": {"macos": "c2ln"}}}}}
        on = {"capability": 1, "enabled": True}
        targets = {"mac-1": self.facts("mac-1", **on), "mac-2": self.facts("mac-2", refusals=("busy_students",), **on),
                   "win-1": self.facts("win-1", "windows", refusals=("unsupported_os",)), "mac-3": self.facts("mac-3", capability=1)}
        rows = fp.devices.plan_devices(plan, state, targets)
        return plan, state, targets, rows

    def test_one_rollout_carries_every_ready_mac_in_order_with_the_controller_last(self):
        plan, state, targets, rows = self.rollout_case()
        rollout, why = fp.devices.update_rollout(plan, state, targets, rows, 1790000000000)
        self.assertIsNone(why)
        self.assertEqual([j["machine_id"] for j in rollout["jobs"]], ["mac-2", "mac-1"], "바쁜 맥도 싣는다(적용만 기다림), 조종 기기는 맨 뒤")
        self.assertEqual({e["machine_id"]: e["code"] for e in rollout["excluded"]}, {"win-1": "not_macos", "mac-3": "update_disabled"})
        scope = rollout["approval_scope"]
        self.assertEqual(sorted(scope), ["action", "asset", "commit", "controller", "plan", "tag", "targets", "version"])
        self.assertEqual(scope["targets"], [{"order": 1, "machine_id": "mac-2", "hash": "tmac-2"}, {"order": 2, "machine_id": "mac-1", "hash": "tmac-1"}])
        self.assertEqual(rollout["approval_scope_hash"], fp.nacho.scope_hash(scope))
        self.assertEqual(rollout["id"], fp.devices.fnv([plan["plan_id"], "sha256:" + "a" * 64, "mac-2=tmac-2", "mac-1=tmac-1", "1790000000000"]))
        for job in rollout["jobs"]:
            self.assertEqual(job["plan_hash"], rollout["id"])
            self.assertEqual(job["job_id"], fp.devices.update_job_id(rollout["id"], job["machine_id"], job["asset"]["sha256"]))
        again, _ = fp.devices.update_rollout(plan, state, targets, rows, 1790000000001)
        self.assertNotEqual(again["id"], rollout["id"], "다시 굴리면 새 id — 기기의 옛 실패 기록과 안 겹친다")
        # 러스트(`the_scope_hash_is_the_one_nacho_computes`)와 같은 정규화.
        vector = {"action": "kasaterm_update", "plan": "abcdef0123456789", "controller": "ctl", "tag": "v0.2.1", "version": "0.2.1",
                  "commit": "5e4d138684156c710831055f5b042688b335f617", "asset": {"name": "kasaterm-v0.2.1.dmg", "sha256": "sha256:" + "a" * 64, "size": 10},
                  "targets": [{"order": 1, "machine_id": "mac-a", "hash": "0123456789abcdef"}, {"order": 2, "machine_id": "ctl", "hash": "fedcba9876543210"}]}
        self.assertEqual(fp.nacho.scope_hash(vector), "sha256:5352981248969a5cb4a59cf3e9a06ea348894a5d01a3d21dc65af53f03d9f720")
        self.assertIsNone(fp.devices.update_rollout(plan, {"stages": {}}, targets, rows, 1)[0], "게시 전엔 보낼 것이 없다")
        # 러스트 `the_target_hash_matches_the_controller_side` 와 같은 값.
        self.assertEqual(fp.devices.target_hash({"machine_id": "mac-a", "app_path": "/Users/x/Applications/kasaterm.app", "pid": 11,
                                                 "binary": {"inode": 1, "mtime_ms": 2, "build": "8933a0ca"}, "capability": 1}), "d206bd151df55dd2")

    def test_device_apply_hands_the_rollout_to_the_runner_only_from_the_nacho_tool(self):
        plan, state, targets, rows = self.rollout_case()
        rollout, _ = fp.devices.update_rollout(plan, state, targets, rows, 1790000000000)
        sd = self.tmp / "state"
        path = fp.save_rollout(plan["plan_id"], rollout, sd)
        aid = "ap_" + "0" * 32
        calls = []

        class Rec:
            def run(self, argv, timeout=None, kind=None):
                calls.append((argv, timeout, kind))
                return Result(0, "mac-2 {}\n")
        def apply(st, **kw):
            with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                return fp.device_apply(plan["plan_id"], plan, st, rollout["id"], kw.get("aid", aid), kw.get("live", True), sd,
                                       runner=Rec(), env=kw.get("env", {"KASATERM_RELEASE_INVOKER": "nacho-tool"}))
        self.assertEqual(apply({"stages": {"release": state["stages"]["release"]}}), 2, "피드 확인 전엔 안 띄운다")
        self.assertEqual(apply(state, aid="ap_x"), 2)
        self.assertEqual(apply(state, env={"KASATERM_PANE_ID": "%3", "KASATERM_RELEASE_INVOKER": "nacho-tool"}), 2, "창 안에서는 못 친다")
        self.assertEqual(apply(state, env={}), 2)
        self.assertEqual(apply(state, live=False), 0)
        self.assertEqual(calls, [], "미리보기·거절은 아무것도 안 띄운다")
        with mock.patch.object(fp.devices, "cli_path", return_value="/x/kasaterm-cli"):
            self.assertEqual(apply(state), 0)
        argv, timeout, kind = calls[-1]
        self.assertEqual(argv, ["/x/kasaterm-cli", "app-update", "run", "--approval", aid, "--rollout", str(path),
                                "--record", str(path.with_name(rollout["id"] + ".grant.json"))])
        self.assertEqual((timeout, kind), (2 * 2100 + 120, "publish"))
        other = {**plan, "plan_id": "f" * 16}
        with contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(fp.device_apply(other["plan_id"], other, state, rollout["id"], aid, False, sd), 2, "다른 계획의 rollout")

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
        self.assertEqual(caps["macos"]["channels"], ["stable", "preview"])
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


# ── 로컬 mac 판(release.yml MAC_ARTIFACT=local) ────────────────────────────────────────────────────────────────
LOCAL_WORKFLOW = ("env:\n  MAC_ARTIFACT: local\n  MAC_TEAM: ABCDE12345\n"
                  "# Verify locally signed DMG · source=Notarized Developer ID · xcrun stapler validate · VERIFIED_SHA\n"
                  "          KASATERM_SIGN_ID: kasaterm-ci\n")
MACHO_FILES = ("Contents/MacOS/kasaterm", "Contents/MacOS/kasaterm-cli", "Contents/MacOS/kasa-serve-web",
               "Contents/Resources/kasapet", "Contents/Frameworks/Sparkle.framework/Versions/B/Sparkle")
DEVID_SHA = "A1" * 20
SIG_DEVID = ("Identifier=x\nCodeDirectory v=20500 size=1 flags=0x10000(runtime) hashes=1\n"
             "Authority=Developer ID Application: Test Org (ABCDE12345)\nAuthority=Developer ID Certification Authority\n"
             "Timestamp=Sep 27, 2026 at 10:00:00\nTeamIdentifier=ABCDE12345\n")
SIG_ADHOC = "Identifier=x\nCodeDirectory v=20400 size=1 flags=0x20002(adhoc,linker-signed) hashes=1\nSignature=adhoc\nTeamIdentifier=not set\n"


class LocalFixture(Fixture):
    """mac dmg 를 이 기기가 Developer ID 로 서명·공증하는 저장소 — 굽기·공증·staple·릴리스 올리기를 가짜 도구로."""

    ci_signing = "self"

    def setUp(self):
        super().setUp()
        self.ci_signing = "local"
        self.tools = {**self.tools, "security": {"path": "security"}, "xcrun": {"path": "xcrun"}, "ditto": {"path": "ditto"}}
        (self.work / ".github/workflows/release.yml").write_text(LOCAL_WORKFLOW)
        bake = self.work / "scripts/build-app.sh"
        bake.write_text(bake.read_text() + "KASATERM_SIGN_HARDENED\n")
        (self.work / "scripts/kasaterm.entitlements").write_text("<plist/>\n")
        self.head = self.commit("build: 로컬 서명")
        sh(self.work, "git", "push", "-q", "origin", "main")
        self.keychain = self.tmp / "codesign.keychain-db"
        self.keychain.write_text("keychain")
        self.keychain_lines = [f'  1) {DEVID_SHA} "Developer ID Application: Test Org (ABCDE12345)"']
        self.sigs, self.build_envs, self.notary_calls, self.uploads = {}, [], [], []
        self.build_adhoc, self.build_dirty, self.debuggable, self.locked = (), False, False, False
        self.unlocks = []
        self.helper = fake_bin(self.tmp, "unlock-signing", "exit 0\n")
        self.notary_answer = lambda: Result(0, json.dumps({"id": "sub-1", "status": "Accepted", "message": "ok"}))
        self.env = {"KASATERM_SIGN_KEYCHAIN": str(self.keychain), "KASATERM_NOTARY_PROFILE": "", "KASATERM_SIGN_ID": ""}

    def plan(self, devices=(), **kw):
        with mock.patch.dict(os.environ, self.env), mock.patch.object(macsign, "UNLOCK_HELPER", self.helper):
            return super().plan(devices, **kw)

    def fill_app(self, app, version, kind="devid", adhoc=()):
        for rel in MACHO_FILES:
            f = Path(app) / rel
            f.parent.mkdir(parents=True, exist_ok=True)
            f.write_bytes(b"\xcf\xfa\xed\xfe" + rel.encode())
            self.sigs[str(f)] = SIG_ADHOC if kind == "adhoc" or rel in adhoc else SIG_DEVID
        with open(Path(app) / "Contents/Info.plist", "wb") as fh:
            plistlib.dump({"CFBundleShortVersionString": version}, fh)

    def fake(self, argv, cwd, env):
        tool, rest = os.path.basename(argv[0]), argv[1:]
        if tool == "security" and rest[:1] == ["find-identity"]:
            self.calls.append(argv)
            return Result(0, "\n".join(self.keychain_lines) + f"\n     {len(self.keychain_lines)} valid identities found\n")
        if tool == "bash" and rest == ["scripts/build-app.sh"]:
            self.calls.append(argv)
            hardened = env.get("KASATERM_SIGN_HARDENED") == "1"
            if hardened and env.get("KASATERM_SIGN_UNLOCK"):
                self.fake([env["KASATERM_SIGN_UNLOCK"]], cwd, env)
            self.build_envs.append({k: env.get(k) for k in ("KASATERM_SIGN_ID", "KASATERM_SIGN_KEYCHAIN", "KASATERM_SIGN_HARDENED")})
            version = re.search(r'^version = "([^"]+)"', (Path(cwd) / "Cargo.toml").read_text(), re.M).group(1)
            app = Path(cwd) / "dist/kasaterm.app"
            self.fill_app(app, version, "devid" if hardened else "adhoc", self.build_adhoc)
            self.identities[str(app)] = {**DEVID, "notarized": False} if hardened else {"verified": True, "authority": None, "team": None}
            head = sh(cwd, "git", "rev-parse", "HEAD")
            (Path(cwd) / "dist/kasaterm.build.json").write_text(json.dumps(
                {"source": {"source_commit": None if self.build_dirty else head, "dirty": self.build_dirty}}))
            return Result(0)
        if tool == "codesign" and rest[0] == "-dvv" and rest[-1] in self.sigs:
            return Result(0, "", self.sigs[rest[-1]])
        if tool == "codesign" and rest[:2] == ["-d", "--entitlements"]:
            return Result(0, "<plist><key>com.apple.security.get-task-allow</key></plist>" if self.debuggable else "<plist/>")
        if tool == "codesign" and rest[0] == "--force":
            self.calls.append(argv)
            return Result(1, "", "errSecInternalComponent") if self.locked else Result(0)
        if argv[0] == self.helper:
            self.unlocks.append(len(self.notary_calls))
            self.locked = False
            return Result(0)
        if tool == "ditto":
            shutil.copytree(rest[0], rest[1], symlinks=True)
            return Result(0)
        if tool == "hdiutil" and rest[0] == "create":
            self.calls.append(argv)
            stage, dmg = Path(rest[rest.index("-srcfolder") + 1]), Path(rest[-1])
            with open(stage / "kasaterm.app/Contents/Info.plist", "rb") as fh:
                version = plistlib.load(fh)["CFBundleShortVersionString"]
            dmg.write_bytes(f"dmg|{version}|{time.time_ns()}".encode())
            return Result(0)
        if tool == "hdiutil" and rest[0] == "attach":
            mnt, data = Path(rest[rest.index("-mountpoint") + 1]), Path(rest[-1]).read_bytes()
            version = data.split(b"|")[1].decode()
            self.fill_app(mnt / "kasaterm.app", version)
            self.identities[str(mnt / "kasaterm.app")] = {**DEVID, "notarized": data.endswith(b"+ticket")}
            return Result(0)
        if tool == "spctl" and "open" in rest:
            data = Path(argv[-1]).read_bytes() if Path(argv[-1]).exists() else b""
            return Result(0, "", "accepted\nsource=Notarized Developer ID") if data.endswith(b"+ticket") else Result(3, "", "rejected")
        if tool == "xcrun" and rest[:2] == ["notarytool", "submit"]:
            self.notary_calls.append(argv)
            return self.notary_answer()
        if tool == "xcrun" and rest[:2] == ["stapler", "staple"]:
            with open(rest[-1], "ab") as fh:
                fh.write(b"+ticket")
            return Result(0)
        if tool == "xcrun" and rest[:2] == ["stapler", "validate"]:
            return Result(0 if Path(rest[-1]).read_bytes().endswith(b"+ticket") else 65, "", "")
        if tool == "gh" and rest[:2] in (["release", "upload"], ["release", "create"]):
            if rest[1] == "create":
                self.release_prerelease = "--prerelease" in rest
            self.uploads.append(rest[:2] + [a for a in rest if a.startswith("--")])
            f = Path(rest[3])
            data = f.read_bytes()
            self.asset_bytes[f.name] = data
            self.assets[f.name] = {"name": f.name, "size": len(data), "digest": sha256_bytes(data)}
            return Result(0)
        return super().fake(argv, cwd, env)

    def dmg_name(self, plan):
        return asset_names(plan["tag"])["macos"]


class LocalSigningPlanTests(LocalFixture):
    def test_the_local_identity_replaces_the_ci_signature_without_moving_any_key(self):
        plan = self.plan()
        self.assertEqual((plan["errors"], plan["live_blocks"]), ([], []))
        rel = plan["signing"]["release"]
        self.assertEqual((rel["source"], rel["team"], rel["notarized"]), ("local", "ABCDE12345", True))
        local = plan["mac_artifact"]
        self.assertEqual((local["identity"]["sha1"], local["keychain"], local["notary_profile"]),
                         (DEVID_SHA, str(self.keychain), "AC_NOTARY"))
        self.assertIn("approval_scope", plan)
        # 계획의 약속(나쵸가 다시 재는 core)은 그대로다 — 서명 길은 core 밖에 둔다.
        self.assertEqual(tuple(fp.core_of(plan)), fp.CORE_KEYS)
        self.assertIn("이 기기(로컬 서명·공증)", fp.describe(plan))

    def test_every_missing_preparation_blocks_the_tag_with_its_reason(self):
        cases = [
            ("workflow", lambda: (self.work / ".github/workflows/release.yml").write_text(
                "env:\n  MAC_ARTIFACT: local\n  MAC_TEAM: ABCDE12345\n"), "검증하지 않는다"),
            ("team", lambda: (self.work / ".github/workflows/release.yml").write_text(
                LOCAL_WORKFLOW.replace("ABCDE12345", "ZZZZZ99999")), "MAC_TEAM(ZZZZZ99999)"),
            ("bake", lambda: (self.work / "scripts/build-app.sh").write_text("# Sparkle.framework\n"), "KASATERM_SIGN_HARDENED"),
        ]
        for name, change, word in cases:
            with self.subTest(name):
                self.tearDown()
                self.setUp()
                change()
                self.commit(f"ci: {name}")
                sh(self.work, "git", "push", "-q", "origin", "main")
                plan = self.plan()
                self.assertIn(word, " ".join(plan["live_blocks"]))
                self.assertTrue(any(b.startswith("mac 서명") for b in plan["live_blocks"]))
        for name, setup, word in (
            ("no identity", lambda: setattr(self, "keychain_lines", []), "Developer ID Application 신원이 없다"),
            ("two identities", lambda: self.keychain_lines.append(f'  2) {"B2" * 20} "Developer ID Application: Test Org (ABCDE12345)"'), "여럿이다"),
            ("no keychain", lambda: self.keychain.unlink(), "열쇠고리"),
            ("other team", lambda: self.identities.update({str(self.app): {**DEVID, "team": "QQQQQ11111"}}), "설치본 팀(QQQQQ11111)"),
            ("no xcrun", lambda: self.tools.update(xcrun={"path": None, "why": "/usr/bin/xcrun 가 없다"}), "xcrun"),
        ):
            with self.subTest(name):
                self.tearDown()
                self.setUp()
                setup()
                self.assertIn(word, " ".join(self.plan()["live_blocks"]))

    def test_a_ci_mode_workflow_keeps_the_ci_identity_gate(self):
        (self.work / ".github/workflows/release.yml").write_text("          KASATERM_SIGN_ID: kasaterm-ci\n")
        self.commit("ci: 예전 길")
        sh(self.work, "git", "push", "-q", "origin", "main")
        plan = self.plan()
        self.assertIsNone(plan["mac_artifact"])
        self.assertIn("CI mac 판에 팀 서명 신원이 없다", " ".join(plan["live_blocks"]))


class LocalSigningUnlockTests(LocalFixture):
    def test_a_locked_key_fails_fast_before_the_long_bake_and_is_never_unlocked_unasked(self):
        plan = self.plan()
        self.locked = True
        with self.assertRaisesRegex(Refused, "서명 열쇠를 지금 못 쓴다.*errSecInternalComponent.*KASATERM_RELEASE_UNLOCK=1"):
            self.go(plan)
        self.assertEqual((self.build_envs, self.unlocks), ([], []))

    def test_an_asked_unlock_runs_the_existing_helper_right_before_signing_and_notarizing(self):
        plan = self.plan()
        self.locked = True
        with mock.patch.dict(os.environ, {"KASATERM_RELEASE_UNLOCK": "1"}):
            aid = self.nacho.approve(plan["approval_scope"])
            state = self.go(plan, "live", aid)
        self.assertEqual(self.build_envs[0]["KASATERM_SIGN_ID"], DEVID_SHA)
        self.assertEqual(state["stages"]["tag"]["status"], "done")
        # 굽기는 build-app.sh 가 서명 직전에 도우미를 부르게 넘기고(가짜 굽기도 그 줄을 흉내), 공증 직전에 한 번 더 부른다.
        self.assertEqual(self.build_envs[0].get("KASATERM_SIGN_HARDENED"), "1")
        self.assertEqual(self.unlocks, [0, 0])
        self.assertEqual(len(self.notary_calls), 1)

    def test_the_bake_script_calls_the_unlock_hook_only_in_hardened_mode_and_just_before_signing(self):
        bake = (REPO / "scripts/build-app.sh").read_text()
        hook = bake.index('"$KASATERM_SIGN_UNLOCK" ||')
        self.assertLess(bake.index('if [[ "$HARDENED" == "1" ]]; then'), hook)
        self.assertLess(hook, bake.index('sign_part "$FW"'))
        self.assertLess(bake.index("cargo build"), hook)


class LocalSigningBuildTests(LocalFixture):
    def test_the_signed_build_bakes_the_version_commit_in_the_plan_workdir(self):
        plan = self.plan()
        state = self.go(plan)
        build = state["stages"]["build"]["detail"]
        self.assertEqual([state["stages"][s]["status"] for s in fp.STAGES], ["done", "done", "dry", "dry", "dry", "done"])
        self.assertEqual(sh(self.work, "git", "rev-parse", f"{build['commit']}^"), self.head)
        self.assertIn('version = "0.2.1"', sh(self.work, "git", "show", f"{build['commit']}:Cargo.toml"))
        self.assertEqual(sh(self.work, "git", "log", "-1", "--format=%s", build["commit"]), "chore(release): v0.2.1")
        self.assertEqual(self.build_envs, [{"KASATERM_SIGN_ID": DEVID_SHA, "KASATERM_SIGN_KEYCHAIN": str(self.keychain),
                                            "KASATERM_SIGN_HARDENED": "1"}])
        dmg = Path(build["dmg"])
        self.assertEqual((dmg.name, dmg.parent), (self.dmg_name(plan), fp.plan_dir(plan["plan_id"], self.state) / "work/out"))
        self.assertEqual((build["version"], build["machos"]), ("0.2.1", len(MACHO_FILES)))
        self.assertTrue(any(c[:2] == ["codesign", "--force"] and "--timestamp" in c and c[-1] == str(dmg) for c in self.calls))
        # 공유 저장소에는 dist·태그·ref 가 안 생기고, 공증·올리기는 승인 전이라 명령만 보인다.
        self.assertFalse((self.work / "dist").exists())
        self.assertEqual((self.remote_tags(), self.notary_calls, self.uploads), (["v0.2.0"], [], []))
        would = "\n".join(state["stages"]["tag"]["detail"]["would"])
        self.assertIn("notarytool submit", would)
        self.assertIn("--keychain-profile AC_NOTARY", would)
        self.assertIn("덮지 않고 멈춤", "\n".join(state["stages"]["release"]["detail"]["would"]))

    def test_the_lockfile_exists_before_the_bake_so_the_build_proof_stays_certain(self):
        state = self.go(self.plan())
        self.assertEqual(state["stages"]["build"]["status"], "done")
        names = [" ".join(c[:2]) for c in self.calls]
        self.assertLess(names.index("cargo metadata"), names.index("bash scripts/build-app.sh"))

    def test_an_existing_lock_from_verification_is_refreshed_after_the_version_bump(self):
        cargo = shutil.which("cargo")
        if not cargo:
            self.skipTest("cargo is required for the lockfile regression")
        (self.work / "Cargo.toml").write_text(
            '[workspace]\nmembers = ["app/kasaterm", "crates/helper"]\nresolver = "2"\n'
            '[workspace.package]\nversion = "0.2.0"\n')
        (self.work / "app/kasaterm/Cargo.toml").write_text(
            '[package]\nname = "kasaterm"\nversion.workspace = true\nedition = "2021"\n'
            '[dependencies]\nhelper = { path = "../../crates/helper" }\n')
        helper = self.work / "crates/helper"
        (helper / "src").mkdir(parents=True)
        (helper / "Cargo.toml").write_text('[package]\nname = "helper"\nversion.workspace = true\nedition = "2021"\n')
        (helper / "src/lib.rs").write_text("pub fn value() -> u32 { 1 }\n")
        (self.work / ".gitignore").write_text("/Cargo.lock\n/dist/\n")
        self.head = self.commit("build: lockfile fixture")
        sh(self.work, "git", "push", "-q", "origin", "main")
        plan = self.plan()
        backend = self.backend(plan, "local")
        backend.tools["cargo"] = {"path": cargo}
        wt = backend.worktree(plan)
        env = {**backend.env(), "CARGO_NET_OFFLINE": "true"}
        sh(wt, cargo, "metadata", "--offline", "--format-version", "1")
        old_lock = (wt / "Cargo.lock").read_bytes()
        bump = backend.ensure_bump(wt, plan)
        self.assertEqual((wt / "Cargo.lock").read_bytes(), old_lock)
        captured = {}

        def execute(argv, cwd, timeout, env):
            if argv[0] == cargo:
                return Runner.execute(backend.runner, argv, cwd, timeout, env)
            if argv == ["bash", "scripts/build-app.sh"]:
                snapshot = backend.workdir / "lock-regression-before.json"
                captured["before"] = proof.begin(wt, "release", snapshot)
                # Exercise Cargo's real pre-fix lock update inside the proof
                # interval; a stale lock must still make that proof uncertain.
                result = Runner.execute(backend.runner, [cargo, "build", "--offline"], cwd, timeout, env)
                if not result.ok:
                    return result
                self.fake(argv, cwd, env)
                manifest = proof.finish(snapshot, wt / "dist/kasaterm.app", wt / "dist/kasaterm.build.json",
                                        signature_verifier=lambda _: {"verified": True})
                captured["source"] = manifest["source"]
                return Result(0)
            return self.fake(argv, cwd, env)

        with mock.patch.object(backend.runner, "execute", side_effect=execute):
            backend.bake(wt, env, "lock-regression")
        self.assertNotEqual((wt / "Cargo.lock").read_bytes(), old_lock)
        self.assertEqual(captured["source"]["status"], "stable_clean")
        self.assertEqual(captured["source"]["source_commit"], bump)
        self.assertEqual(captured["source"]["before"]["input_digest"], captured["source"]["after"]["input_digest"])

    def test_a_failed_existing_lock_refresh_stops_before_the_bake(self):
        plan = self.plan()
        backend = self.backend(plan, "local")
        wt = backend.worktree(plan)
        (wt / "Cargo.lock").write_text("stale lock\n")
        for answer, reason in ((Result(101, "", "resolution failed"), "resolution failed"),
                               (Result(None, timed_out=True), "시간 초과")):
            with self.subTest(reason=reason):
                self.calls.clear()
                self.cargo_answer = lambda _argv, result=answer: result
                with self.assertRaisesRegex(Refused, "Cargo.lock.*" + reason):
                    backend.bake(wt, backend.env(), "failed-lock")
                self.assertFalse(any(c[:2] == ["bash", "scripts/build-app.sh"] for c in self.calls))

    def test_the_version_commit_is_the_same_every_time_it_is_made(self):
        plan = self.plan()
        b = self.backend(plan, "local")
        one = b.ensure_bump(b.worktree(plan, "a"), plan)
        two = b.ensure_bump(b.worktree(plan, "b"), plan)
        self.assertEqual(one, two)
        for name in ("a", "b"):
            b.git("worktree", "remove", "--force", str(b.workdir / name), kind="local")

    def test_an_unsigned_piece_a_dirty_bake_or_a_debuggable_bundle_stops_the_build(self):
        for name, setup, word in (
            ("adhoc pet", lambda: setattr(self, "build_adhoc", ("Contents/Resources/kasapet",)), "kasapet: Developer ID 가 아님"),
            ("dirty", lambda: setattr(self, "build_dirty", True), "깨끗한 판이 아니다"),
            ("debuggable", lambda: setattr(self, "debuggable", True), "get-task-allow"),
        ):
            with self.subTest(name):
                self.tearDown()
                self.setUp()
                setup()
                plan = self.plan()
                with self.assertRaisesRegex(Refused, word):
                    self.go(plan)
                self.assertFalse(fp.live_ready(plan, fp.load(plan["plan_id"], self.state)[1]))

    def test_a_ready_build_is_never_used_for_the_local_signed_release(self):
        dist = self.work / "dist"
        app = dist / f"kasaterm.app.ready-{self.head[:8]}/Contents/MacOS"
        app.mkdir(parents=True)
        (app / "kasaterm").write_bytes(b"ready")
        (dist / f"kasaterm.build.ready-{self.head[:8]}.json").write_text(json.dumps(
            {"source": {"source_commit": self.head, "dirty": False},
             "components": {"app": {"sha256": sha256_bytes(b"ready")[7:]}}}))
        self.identities[str(dist / f"kasaterm.app.ready-{self.head[:8]}")] = dict(DEVID)
        state = self.go(self.plan())
        self.assertNotIn("reused", state["stages"]["build"]["detail"])
        self.assertEqual(len(self.build_envs), 1)

    def test_preflight_bakes_unsigned_in_the_plan_workdir_and_counts_what_signing_must_fix(self):
        plan = self.plan()
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = fp.mac_preflight(self.work, plan, self.state, runner=FakeRunner("local", self), http=self.http)
        self.assertEqual(code, 0)
        doc = json.loads((fp.plan_dir(plan["plan_id"], self.state) / "preflight.json").read_text())
        self.assertEqual((doc["version"], doc["uncovered"], len(doc["machos"])), ("0.2.1", [], len(MACHO_FILES)))
        self.assertEqual(len(doc["signing_would_fix"]), len(MACHO_FILES))
        self.assertTrue(Path(doc["dmg"]).is_relative_to(fp.plan_dir(plan["plan_id"], self.state)))
        self.assertEqual(self.build_envs[0]["KASATERM_SIGN_HARDENED"], None)
        self.assertTrue(self.build_envs[0]["KASATERM_SIGN_KEYCHAIN"].endswith("no-keychain"))
        self.assertFalse((self.work / "dist").exists())
        self.assertNotIn("wt-preflight", sh(self.work, "git", "worktree", "list"))
        # 미리 굽기의 버전 커밋은 실제 굽기가 만들 커밋과 같다(날짜를 계획 시각에 못 박았다).
        state = self.go(plan)
        self.assertEqual(state["stages"]["build"]["detail"]["commit"], doc["commit"])


class LocalSigningLiveTests(LocalFixture):
    def test_notarize_before_push_upload_that_exact_dmg_and_follow_it_to_the_feed(self):
        dev = self.device("맥북", "0.2.0", sh(self.work, "git", "rev-parse", "v0.2.0")[:8], "dev-mac")
        plan = self.plan([dev])
        aid = self.nacho.approve(plan["approval_scope"])
        state = self.go(plan, "live", aid, [dev])
        built = state["stages"]["build"]["detail"]
        tagged = state["stages"]["tag"]["detail"]
        self.assertEqual((self.remote(f"refs/tags/{plan['tag']}"), tagged["commit"]), (built["commit"], built["commit"]))
        self.assertEqual(len(self.notary_calls), 1)
        submit = self.notary_calls[0]
        self.assertEqual(submit[3], built["dmg"])
        self.assertIn("--wait", submit)
        self.assertEqual(submit[submit.index("--keychain") + 1], str(self.keychain))
        seal = tagged["notarized"]
        self.assertEqual((seal["notarized"], seal["stapled"], seal["team"], seal["version"]), (True, True, "ABCDE12345", "0.2.1"))
        self.assertNotEqual(seal["dmg_sha256"], built["dmg_sha256"])  # staple 이 티켓을 붙였다
        self.assertEqual(self.uploads, [["release", "create", "--repo", "--verify-tag", "--title", "--notes"]])
        self.assertEqual(self.assets[self.dmg_name(plan)]["digest"], seal["dmg_sha256"])
        self.assertIn("CI 가 아직 안 떴다", state["stages"]["release"]["detail"])

        self.ci_publish(plan, feed=False, keep=(self.dmg_name(plan),))
        state = self.go(plan, "live", aid, [dev])
        rel = state["stages"]["release"]["detail"]
        self.assertEqual((rel["dmg_upload"], rel["assets"]["macos"]["sha256"]), ("already", seal["dmg_sha256"]))
        self.assertEqual((rel["mac_identity"]["team"], rel["mac_identity"]["source"]), ("ABCDE12345", "local"))

        self.ci_publish(plan, keep=(self.dmg_name(plan),))
        state = self.go(plan, "live", aid, [dev])
        self.assertEqual([state["stages"][s]["status"] for s in fp.STAGES], ["done"] * 6)
        self.assertEqual((len(self.notary_calls), len(self.uploads), self.nacho.consumes), (1, 1, 1))

    def test_a_notary_rejection_raises_no_tag_and_a_rerun_submits_the_same_dmg(self):
        plan = self.plan()
        aid = self.nacho.approve(plan["approval_scope"])
        self.notary_answer = lambda: Result(1, json.dumps({"id": "sub-bad", "status": "Invalid"}))
        with self.assertRaisesRegex(Refused, r"공증 실패\(Invalid\).*notarytool log sub-bad"):
            self.go(plan, "live", aid)
        self.assertEqual((self.remote_tags(), self.remote("refs/heads/main"), self.uploads), (["v0.2.0"], self.head, []))
        self.notary_answer = lambda: Result(0, json.dumps({"id": "sub-2", "status": "Accepted"}))
        state = self.go(plan, "live", aid)
        self.assertEqual(state["stages"]["tag"]["status"], "done")
        self.assertEqual((len(self.notary_calls), self.nacho.consumes), (2, 1))

    def test_a_failed_push_after_notarization_does_not_submit_again(self):
        plan = self.plan()
        aid = self.nacho.approve(plan["approval_scope"])
        self.push_answer = lambda real: Result(1, "", "remote rejected")
        with self.assertRaisesRegex(Refused, "원격은 그대로다"):
            self.go(plan, "live", aid)
        self.push_answer = None
        state = self.go(plan, "live", aid)
        self.assertEqual((state["stages"]["tag"]["status"], len(self.notary_calls)), ("done", 1))
        self.assertEqual(state["stages"]["tag"]["detail"]["notarized"]["id"], "sub-1")

    def test_a_dmg_changed_after_the_build_is_never_notarized_or_tagged(self):
        plan = self.plan()
        state = self.go(plan)
        Path(state["stages"]["build"]["detail"]["dmg"]).write_bytes(b"dmg|0.2.1|swapped")
        aid = self.nacho.approve(plan["approval_scope"])
        with self.assertRaisesRegex(Refused, "굽고 나서 바뀌었다"):
            self.go(plan, "live", aid)
        self.assertEqual((self.notary_calls, self.remote_tags()), ([], ["v0.2.0"]))

    def test_a_different_dmg_on_the_release_is_never_overwritten(self):
        plan = self.plan()
        self.assets[self.dmg_name(plan)] = {"name": self.dmg_name(plan), "size": 3, "digest": "sha256:" + "0" * 64}
        aid = self.nacho.approve(plan["approval_scope"])
        with self.assertRaisesRegex(Refused, "덮지 않는다"):
            self.go(plan, "live", aid)
        self.assertEqual(self.uploads, [])

    def test_the_release_must_carry_the_file_this_machine_notarized(self):
        plan = self.plan()
        aid = self.nacho.approve(plan["approval_scope"])
        self.go(plan, "live", aid)
        self.ci_publish(plan, feed=False, keep=(self.dmg_name(plan),))
        name = self.dmg_name(plan)
        self.asset_bytes[name] = b"dmg|0.2.1|other+ticket"
        self.assets[name] = {"name": name, "size": len(self.asset_bytes[name]), "digest": sha256_bytes(self.asset_bytes[name])}
        with self.assertRaisesRegex(Refused, "덮지 않는다|공증한 그 파일이 아니다"):
            self.go(plan, "live", aid)


class MacsignUnitTests(unittest.TestCase):
    def test_signature_fields_runtime_timestamp_and_adhoc(self):
        dev, adhoc = macsign.parse_signature(SIG_DEVID), macsign.parse_signature(SIG_ADHOC)
        self.assertEqual((dev["authority"], dev["team"], dev["runtime"], dev["timestamp"], dev["adhoc"]),
                         ("Developer ID Application: Test Org (ABCDE12345)", "ABCDE12345", True, True, False))
        self.assertEqual((adhoc["team"], adhoc["runtime"], adhoc["timestamp"], adhoc["adhoc"]), (None, False, False, True))
        signed_time = SIG_DEVID.replace("Timestamp=", "Signed Time=")
        self.assertFalse(macsign.parse_signature(signed_time)["timestamp"])

    def test_identities_notary_json_and_workflow_mode(self):
        text = f'  1) {DEVID_SHA} "Developer ID Application: A B (ABCDE12345)"\n  2) {"C3" * 20} "Apple Development: A (XYZ)"\n'
        self.assertEqual(macsign.parse_identities(text), [{"sha1": DEVID_SHA, "name": "Developer ID Application: A B (ABCDE12345)",
                                                           "team": "ABCDE12345"}])
        self.assertEqual(macsign.parse_notary('{"id":"x","status":"Accepted"}')["status"], "Accepted")
        self.assertEqual(macsign.parse_notary("not json")["status"], None)
        self.assertEqual(macsign.workflow_mode(LOCAL_WORKFLOW), {"mode": "local", "team": "ABCDE12345", "verifies": True})
        self.assertEqual(macsign.workflow_mode("KASATERM_SIGN_ID: kasaterm-ci\n")["mode"], "ci")

    def test_machos_are_found_by_magic_and_checked_against_the_hardened_list(self):
        with tempfile.TemporaryDirectory() as d:
            app = Path(d) / "kasaterm.app"
            for rel in MACHO_FILES + ("Contents/Helpers/stray",):
                (app / rel).parent.mkdir(parents=True, exist_ok=True)
                (app / rel).write_bytes(b"\xcf\xfa\xed\xfe")
            (app / "Contents/Resources/readme.txt").write_text("text")
            os.symlink("kasaterm", app / "Contents/MacOS/link")
            found = [str(p.relative_to(app)) for p in macsign.machos(app)]
            self.assertEqual(sorted(found), sorted(MACHO_FILES + ("Contents/Helpers/stray",)))
            rows = [{"path": rel, "covered": macsign.covered(rel), **macsign.parse_signature(SIG_DEVID)} for rel in found]
            self.assertEqual(macsign.readiness_problems(rows, "ABCDE12345"), ["Contents/Helpers/stray: build-app.sh 의 hardened 서명 목록 밖"])
            self.assertIn("팀 ABCDE12345", macsign.readiness_problems(rows, "OTHER00000")[0])


class ViewerSigningTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="viewer-signing-")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name).resolve()
        (self.root / "scripts").mkdir()
        shutil.copy2(REPO / "scripts/build-viewer-app.sh", self.root / "scripts/build-viewer-app.sh")
        shutil.copy2(REPO / "scripts/kasaterm.entitlements", self.root / "scripts/kasaterm.entitlements")
        (self.root / "Cargo.toml").write_text('[workspace.package]\nversion = "0.2.2"\n')
        for relative in ("assets/ViewerIcon.icns", "app/kasaterm/assets/fonts/NotoSansKR-Variable.ttf",
                         "app/kasaterm/assets/fonts/OFL-NotoSansKR.txt", "target/debug/kasaterm"):
            path = self.root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b"isolated signing fixture")
        (self.root / "target/debug/kasaterm").chmod(0o755)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.log = self.root / "commands.jsonl"
        stub = (f"#!{sys.executable}\n" + '''import json, os, pathlib, plistlib, sys
tool = pathlib.Path(sys.argv[0]).name
with open(os.environ["VIEWER_TEST_LOG"], "a") as out:
    out.write(json.dumps([tool, *sys.argv[1:]]) + "\\n")
if tool == "security":
    print(os.environ.get("VIEWER_TEST_IDENTITIES", ""))
elif tool == "codesign":
    key = "VIEWER_TEST_VERIFY_STATUS" if "--verify" in sys.argv else "VIEWER_TEST_SIGN_STATUS"
    sys.exit(int(os.environ.get(key, "0")))
elif tool == "plutil":
    with open(sys.argv[-1], "rb") as inp:
        doc = plistlib.load(inp)
    if sys.argv[1] == "-extract":
        print(doc[sys.argv[2]])
elif tool == "unlock-viewer":
    sys.exit(int(os.environ.get("VIEWER_TEST_UNLOCK_STATUS", "0")))
''')
        for tool in ("cargo", "npm", "security", "codesign", "plutil", "unlock-viewer"):
            path = self.bin / tool
            path.write_text(stub)
            path.chmod(0o755)
        self.keychain = self.root / "signing keychain"
        self.keychain.write_text("fixture; contains no credentials")

    def build(self, **extra):
        env = {k: v for k, v in os.environ.items() if not k.startswith("KASATERM_SIGN_") and k != "CARGO_TARGET_DIR"}
        env.update(PATH=str(self.bin) + os.pathsep + env.get("PATH", ""), VIEWER_TEST_LOG=str(self.log))
        env.update(extra)
        result = subprocess.run(["bash", "scripts/build-viewer-app.sh", "--debug"], cwd=self.root, env=env,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=30)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        return result, calls

    def test_hardened_viewer_pins_identity_keychain_and_unlocks_only_before_signing(self):
        identity = "A" * 40
        result, calls = self.build(KASATERM_SIGN_ID=identity, KASATERM_SIGN_KEYCHAIN=str(self.keychain),
                                  KASATERM_SIGN_HARDENED="1", KASATERM_SIGN_UNLOCK=str(self.bin / "unlock-viewer"),
                                  VIEWER_TEST_IDENTITIES=f'1) {identity} "Developer ID Application: Test (ABCDE12345)"')
        self.assertEqual(result.returncode, 0, result.stderr)
        sign = next(call for call in calls if call[:2] == ["codesign", "--force"])
        self.assertEqual(sign[sign.index("--sign") + 1], identity)
        self.assertEqual(sign[sign.index("--keychain") + 1], str(self.keychain))
        self.assertEqual(sign[sign.index("--options") + 1], "runtime")
        self.assertIn("--timestamp", sign)
        self.assertNotIn("--timestamp=none", sign)
        self.assertEqual(sign[sign.index("--entitlements") + 1], str(self.root / "scripts/kasaterm.entitlements"))
        unlock = calls.index(["unlock-viewer"])
        self.assertGreater(unlock, max(i for i, call in enumerate(calls) if call[0] == "cargo"))
        self.assertEqual(calls[unlock + 1], sign)

    def test_an_explicit_unavailable_identity_never_falls_back_to_adhoc(self):
        result, calls = self.build(KASATERM_SIGN_ID="missing-identity", VIEWER_TEST_SIGN_STATUS="1")
        self.assertNotEqual(result.returncode, 0)
        signs = [call for call in calls if call[:2] == ["codesign", "--force"]]
        self.assertEqual(len(signs), 1)
        self.assertEqual(signs[0][signs[0].index("--sign") + 1], "missing-identity")
        self.assertFalse((self.root / "dist/KasaViewer.app").exists())

    def test_hardened_signing_rejects_a_development_identity_before_unlock(self):
        result, calls = self.build(KASATERM_SIGN_ID="kasaterm-dev", KASATERM_SIGN_HARDENED="1",
                                  KASATERM_SIGN_UNLOCK=str(self.bin / "unlock-viewer"),
                                  VIEWER_TEST_IDENTITIES='1) BBB "kasaterm-dev"')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Developer ID Application", result.stderr)
        self.assertFalse(any(call[0] in ("codesign", "unlock-viewer") for call in calls))

    def test_a_failed_unlock_cannot_publish_a_bundle(self):
        result, calls = self.build(KASATERM_SIGN_ID="explicit", KASATERM_SIGN_HARDENED="1",
                                  KASATERM_SIGN_UNLOCK=str(self.bin / "unlock-viewer"), VIEWER_TEST_UNLOCK_STATUS="1",
                                  VIEWER_TEST_IDENTITIES='1) explicit "Developer ID Application: Test (ABCDE12345)"')
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(any(call[0] == "codesign" for call in calls))
        self.assertFalse((self.root / "dist/KasaViewer.app").exists())

    def test_ordinary_development_builds_remain_separate_from_sparkle_updates(self):
        result, calls = self.build()
        self.assertEqual(result.returncode, 0, result.stderr)
        sign = next(call for call in calls if call[:2] == ["codesign", "--force"])
        self.assertEqual(sign[sign.index("--sign") + 1], "-")
        self.assertIn("--timestamp=none", sign)
        self.assertFalse(any(call[0] == "unlock-viewer" for call in calls))
        app = self.root / "dist/KasaViewer.app"
        with (app / "Contents/Info.plist").open("rb") as inp:
            info = plistlib.load(inp)
        self.assertNotIn("SUFeedURL", info)
        self.assertFalse((app / "Contents/Frameworks/Sparkle.framework").exists())
        self.assertFalse((app / "Contents/Resources/Sparkle.framework").exists())


class LocalSigningRepoTests(unittest.TestCase):
    """이 저장소의 실제 release.yml·build-app.sh 가 로컬 판 계약을 지키는가 — 글자로 본다(yaml 모듈 없이)."""

    def setUp(self):
        self.wf = (REPO / ".github/workflows/release.yml").read_text()
        self.bake = (REPO / "scripts/build-app.sh").read_text()

    def test_the_workflow_verifies_the_local_dmg_and_signs_only_that_hash(self):
        self.assertEqual(macsign.workflow_mode(self.wf), {"mode": "local", "team": "L366799VND", "verifies": True})
        self.assertEqual(fp.capabilities(REPO)["macos"]["artifact"]["mode"], "local")
        self.assertEqual(macsign.repo_ready(REPO), [])
        build = self.wf[self.wf.index("- name: Build .app + .dmg"):self.wf.index("- name: Verify locally signed DMG")]
        self.assertIn("if: env.MAC_ARTIFACT == 'ci'", build)
        verify = self.wf[self.wf.index("- name: Verify locally signed DMG"):self.wf.index("- name: Attach DMG to release")]
        for must in ('[[ "$WANT" == "$GOT" ]]', "source=Notarized Developer ID", "xcrun stapler validate",
                     "TeamIdentifier=$MAC_TEAM", "flags=.*runtime", "CFBundleShortVersionString", 'echo "sha256='):
            self.assertIn(must, verify)
        self.assertNotIn("--clobber\n", verify.split("gh release download")[0])
        attach = self.wf[self.wf.index("- name: Attach DMG to release"):self.wf.index("- name: Upload DMG artifact")]
        local_branch = attach[attach.index('if [[ "$MAC_ARTIFACT" == "local" ]]; then\n            # dmg'):attach.index("elif gh release view")]
        self.assertNotIn("upload", local_branch)
        appcast = self.wf[self.wf.index("  appcast:"):]
        self.assertIn("VERIFIED_SHA: ${{ needs.build-dmg.outputs.dmg_sha256 }}", appcast)
        self.assertLess(appcast.index('[[ "$GOT" == "$VERIFIED_SHA" ]]'), appcast.index("vendor/Sparkle/bin/generate_appcast \\\n"))

    def test_the_bake_takes_binaries_from_the_isolated_target_the_release_tool_passes(self):
        # 2026-09-27 미리 굽기 실측: cargo 는 CARGO_TARGET_DIR 로 다 굽고, cp 가 고정 경로 target/release 에서 못 찾아 멈췄다.
        self.assertIn('TARGET_DIR="${CARGO_TARGET_DIR:-target}"', self.bake)
        self.assertIn('BINDIR="$TARGET_DIR/release"', self.bake)
        self.assertNotIn('BINDIR="target/', self.bake)

    def test_direct_bakes_resolve_before_source_evidence_and_keep_the_lock_fixed(self):
        resolve = self.bake.index("cargo metadata --format-version 1")
        begin = self.bake.index('build_manifest.py" begin')
        builds = [line.strip() for line in self.bake.splitlines() if line.strip().startswith("cargo build ")]
        self.assertLess(resolve, begin)
        self.assertEqual(len(builds), 2)
        self.assertTrue(all("--locked" in line.split() for line in builds))

    def test_only_real_builds_fetch_lfs_and_an_existing_tag_can_be_finished_from_main(self):
        wf = self.wf
        msi = wf[wf.index("  build-msi:"):wf.index("  build-dmg:")]
        dmg = wf[wf.index("  build-dmg:"):wf.index("  appcast:")]
        app = wf[wf.index("  appcast:"):]
        # Windows 굽기는 학생 그림을 실행 파일에 넣으니 LFS 가 필요하다 — 쓰는 것만 받고, 남은 포인터가 있으면 멈춘다.
        # 로컬 dmg 검증·appcast 는 그림이 필요 없다.
        self.assertIn("lfs: false", msi)
        self.assertIn('git lfs pull --exclude="$EXCLUDE"', msi)
        self.assertIn("EXCLUDE='mobile/**,web/arona-ui/character-src/**'", msi)
        self.assertIn("grep -vE '^(mobile/|web/arona-ui/character-src/)'", msi)
        self.assertLess(msi.index("Fetch LFS assets for the Windows build"), msi.index("Build and verify Windows packages"))
        # 받는 곳은 GitHub LFS 가 아니라 미니 창구다(자세한 계약은 test_mini_lfs) — mac·appcast 는 미니를 안 부른다.
        self.assertIn("MINI_LFS_TOKEN: ${{ secrets.MINI_LFS_TOKEN }}", msi)
        self.assertNotIn("MINI_LFS", dmg + app)
        self.assertIn("lfs: ${{ env.MAC_ARTIFACT == 'ci' || env.RELEASE_TAG == '' }}", dmg)
        self.assertNotIn("lfs: true", msi + dmg + app)
        self.assertIn("GIT_LFS_SKIP_SMUDGE: '1'", app)
        # 태그 마무리: 입력한 태그를 체크아웃하고 그 릴리스에 붙이며, 태그를 만들거나 옮기는 명령은 없다.
        self.assertIn("RELEASE_TAG: ${{ inputs.tag || (startsWith(github.ref, 'refs/tags/v') && github.ref_name) || '' }}", wf)
        self.assertEqual(wf.count("ref: ${{ inputs.tag || '' }}"), 1)
        self.assertEqual(wf.count("ref: ${{ needs.resolve.outputs.commit }}"), 2)
        self.assertIn("tag_name: ${{ env.RELEASE_TAG }}", msi)
        self.assertNotIn("GITHUB_REF_NAME", msi + dmg + app)
        for forbidden in ("git tag", "git push --force", "git push -f"):
            self.assertNotIn(forbidden, wf.replace("startsWith(github.ref, 'refs/tags/v')", ""))
        self.assertIn("run-name: ${{ inputs.tag && format('release {0} ({1})', inputs.tag, inputs.platforms)", wf)
        # mac 만 마무리: Windows job 을 건너뛰고, appcast 는 dmg 검증 성공 + (Windows 성공 또는 일부러 건너뜀)일 때만.
        self.assertIn("if: needs.resolve.outputs.platforms == 'both'", msi)
        self.assertIn("needs.build-dmg.result == 'success'", app)
        self.assertIn("(needs.resolve.outputs.platforms == 'macos' && needs.build-msi.result == 'skipped')", app)
        both_start = app.index('if [[ "$PLATFORMS" == "both" ]]; then')
        both = app[both_start:app.index('echo "directory=', both_start)]
        self.assertIn("appcast-win.xml", both)
        publish = app[app.index("- name: Publish verified appcasts"):]
        self.assertIn('python3 -m tools.release.feed_publish "${FEED_ARGS[@]}" --attempts 5', publish)
        self.assertIn('--commit "$RELEASE_COMMIT"', publish)
        self.assertIn('--dmg-sha256 "$DMG_SHA"', publish)
        self.assertIn('if [[ "$PLATFORMS" == "both" ]]; then\n            FEED_ARGS+=(--windows', publish)
        self.assertNotIn("git pull --rebase", app)
        self.assertNotIn("git pull --ff-only", app)
        self.assertLess(app.index("- name: Generate signed appcasts"), app.index("- name: Publish verified appcasts"))

    def test_the_lfs_folders_the_windows_build_skips_are_really_unused_by_it(self):
        # 빼는 폴더가 Windows 굽기에 쓰이면 그림이 포인터로 들어가 빈칸이 된다 — 굽기가 읽는 자리에서 그 이름을 찾는다.
        for rel in ("scripts/windows/package.ps1", "web/arona-ui/package.json", "web/arona-ui/vite.config.ts"):
            text = (REPO / rel).read_text()
            self.assertNotIn("character-src", text, rel)
            self.assertNotIn("mobile/", text, rel)
        for path in list((REPO / "app").rglob("*.rs")) + list((REPO / "crates").rglob("*.rs")):
            for m in re.finditer(r'include_bytes!\(\s*(?:concat!\(\s*)?"([^"]+)"', path.read_text(errors="ignore")):
                target = os.path.normpath(os.path.join(path.parent.relative_to(REPO), m.group(1)))
                self.assertFalse(target.startswith(("mobile/", "web/arona-ui/character-src/")), f"{path}: {target}")

    def test_the_hardened_bake_signs_every_listed_piece_with_runtime_and_timestamp(self):
        for rel in macsign.HARDENED_SIGNED:
            self.assertIn(rel.split("/")[-1], self.bake)
        self.assertIn("SIGN_ARGS+=(--options runtime --timestamp)", self.bake)
        self.assertIn('sign_part "$APP/Contents/Resources/kasapet"', self.bake)
        self.assertIn("--entitlements \"$ROOT/scripts/kasaterm.entitlements\"", self.bake)
        ent = plistlib.loads((REPO / "scripts/kasaterm.entitlements").read_bytes())
        self.assertEqual(ent, {"com.apple.security.device.audio-input": True, "com.apple.security.automation.apple-events": True})
        self.assertIn("NSMicrophoneUsageDescription", self.bake)


class PreviewReleaseTests(LocalFixture):
    def setUp(self):
        super().setUp()
        (self.work / "app/kasaterm/src/macos_sparkle.rs").write_text(f'const PREVIEW_FEED: &str = "{fp.PREVIEW_FEED}";\n')
        self.head = self.commit("preview feed support")
        sh(self.work, "git", "push", "-q", "origin", "main")
        self.preview_feed = self.tmp / "appcast-preview.xml"
        self.preview_feed.write_bytes((REPO / "docs/appcast-preview.xml").read_bytes())

    def preview_plan(self, **kwargs):
        return self.plan(channel="preview", feed=str(self.preview_feed), feed_win=None,
                         stable_feed=str(self.feed), **kwargs)

    def publish(self, plan, authorizer):
        backend = self.backend(plan, "live")
        try:
            return fp.run(plan["plan_id"], backend, self.state, publisher_authorizer=authorizer)
        finally:
            fp.cleanup(backend, plan)

    def test_preview_defaults_to_its_feed_and_empty_channel_uses_stable_base(self):
        plan = self.preview_plan()
        self.assertEqual((plan["errors"], plan["live_blocks"]), ([], []))
        self.assertEqual((plan["platforms"], plan["feed"]["windows"], plan["feed"]["version"]), (["macos"], None, None))
        self.assertEqual(plan["base"]["tag"], "v0.2.0")
        self.assertNotIn("approval_scope", plan)
        self.assertEqual(self.backend(plan, "dry").feed_base(plan), plan["feed_base"])
        response = {fp.PREVIEW_FEED: (404, b""), fp.MAC_FEED: (200, self.feed.read_bytes())}
        with mock.patch.object(self.http, "get", side_effect=lambda url, **_: response[url]):
            default = self.plan(channel="preview", feed=None, feed_win=None)
        self.assertEqual(default["feed"]["source"], fp.PREVIEW_FEED)
        self.assertEqual(default["base"]["tag"], "v0.2.0")

    def test_channel_base_does_not_skip_changes_just_because_a_preview_tag_exists(self):
        source = self.head
        (self.work / common.CHANNEL_MANIFEST).write_text(json.dumps(common.channel_manifest("preview", "v0.2.1", source)))
        self.release_tag("0.2.1")
        self.preview_feed.write_text(feed_xml("0.2.1"))
        (self.work / "app/kasaterm/src/main.rs").write_text("fn main() { loop {} }\n")
        self.head = self.commit("next native change")
        sh(self.work, "git", "push", "-q", "origin", "main")
        preview, stable = self.preview_plan(), self.plan()
        self.assertEqual((preview["base"]["tag"], stable["base"]["tag"]), ("v0.2.1", "v0.2.0"))
        self.assertEqual((preview["tag"], stable["tag"]), ("v0.2.2", "v0.2.2"))
        self.assertGreater(len(stable["changes"]["commits"]), len(preview["changes"]["commits"]))
        backend = self.backend(stable, "local")
        try:
            wt = backend.worktree(stable)
            backend.ensure_bump(wt, stable)
            manifest = json.loads((wt / common.CHANNEL_MANIFEST).read_text())
            self.assertEqual(manifest, common.channel_manifest("stable", "v0.2.2", self.head))
            self.assertEqual(common.ci_release_context(wt, "v0.2.2")["channel"], "stable")
        finally:
            fp.cleanup(backend, stable)

    def test_preview_publishes_only_signed_macos_prerelease_and_reauthorizes_each_stage(self):
        before = (self.feed.read_bytes(), self.feed_win.read_bytes())
        plan = self.preview_plan()
        self.go(plan, "local")
        calls = []

        def authorize(current, state, backend, stage):
            calls.append(stage)
            self.assertNotIn("approval", state)
            return {"policy": "test", "plan_id": current["plan_id"], "stage": stage}

        state = self.publish(plan, authorize)
        self.assertEqual(calls, ["tag", "release"])
        self.assertEqual(state["stages"]["release"]["status"], "waiting")
        self.assertIn("--prerelease", self.uploads[0])
        self.assertIn("--latest=false", self.uploads[0])
        self.ci_publish(plan, keep=[self.dmg_name(plan)])
        state = self.publish(plan, authorize)
        self.assertEqual(calls, ["tag", "release", "release", "feed"])
        self.assertEqual(set(state["stages"]["release"]["detail"]["assets"]), {"macos"})
        self.assertEqual(set(state["stages"]["feed"]["detail"]["hashes"]), {"macos"})
        self.assertEqual((self.feed.read_bytes(), self.feed_win.read_bytes()), before)
        self.assertEqual(self.nacho.consumes, 0)
        self.assertEqual(state["authorization"]["stage"], "feed")

    def test_authorizer_is_not_an_optional_approval_bypass_and_can_stop_after_tag(self):
        plan = self.preview_plan()
        backend = self.backend(plan, "live")
        called = []
        authorizer = lambda *args: called.append(args[-1]) or {"stage": args[-1]}
        with self.assertRaisesRegex(Refused, "먼저 run"):
            fp.run(plan["plan_id"], backend, self.state, publisher_authorizer=authorizer)
        self.assertEqual(called, [])
        self.go(plan, "local")
        with self.assertRaisesRegex(Refused, "나쵸 승인"):
            fp.run(plan["plan_id"], backend, self.state)
        with self.assertRaisesRegex(Refused, "혼용"):
            fp.run(plan["plan_id"], backend, self.state, authority=object(), publisher_authorizer=authorizer)
        _, state = fp.load(plan["plan_id"], self.state)
        state["approval"] = {"id": "existing-nacho-approval"}
        fp.save_state(plan["plan_id"], state, self.state)
        with self.assertRaisesRegex(Refused, "혼용"):
            fp.run(plan["plan_id"], backend, self.state, publisher_authorizer=authorizer)
        state.pop("approval")
        fp.save_state(plan["plan_id"], state, self.state)
        with self.assertRaisesRegex(Refused, "기록"):
            fp.run(plan["plan_id"], backend, self.state, publisher_authorizer=lambda *args: None)

        def revoked(current, state, backend, stage):
            if stage == "release":
                raise Refused("policy revoked")
            return {"stage": stage}

        with self.assertRaisesRegex(Refused, "policy revoked"):
            self.publish(plan, revoked)
        self.assertEqual(self.uploads, [])
        with self.assertRaisesRegex(Refused, "재사용"):
            fp.run(plan["plan_id"], backend, self.state, approval_id="ap_x", authority=object())

    def test_preview_ci_context_is_pinned_and_refuses_stale_or_cross_platform_manifest(self):
        plan = self.preview_plan()
        backend = self.backend(plan, "local")
        try:
            wt = backend.worktree(plan)
            bump = backend.ensure_bump(wt, plan)
            context = common.ci_release_context(wt, plan["tag"], "both")
            self.assertEqual((context["platforms"], context["prerelease"], context["commit"], context["feed_path"]),
                             ("macos", "true", bump, "docs/appcast-preview.xml"))
            expected = json.loads((wt / common.CHANNEL_MANIFEST).read_text())
            for change in ({"tag": "v0.2.0"}, {"source_commit": "a" * 40}, {"platforms": ["macos", "windows"]}):
                (wt / common.CHANNEL_MANIFEST).write_text(json.dumps({**expected, **change}))
                with self.assertRaises(Refused):
                    common.ci_release_context(wt, plan["tag"])
            (wt / common.CHANNEL_MANIFEST).write_text(json.dumps(expected))
            workflow = (REPO / ".github/workflows/release.yml").read_text()
            section = workflow.split("- name: Resolve release channel", 1)[1].split("\n  build-msi:", 1)[0]
            script = "\n".join(line[10:] for line in section.split("run: |\n", 1)[1].splitlines())
            output = self.tmp / "context.out"
            result = subprocess.run(["bash", "-e", "-o", "pipefail", "-c", script], cwd=wt,
                                    env={**os.environ, "RELEASE_TAG": plan["tag"], "REQUESTED_PLATFORMS": "both",
                                         "GITHUB_OUTPUT": str(output), "PYTHONPATH": str(REPO)}, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("platforms=macos\n", output.read_text())
            self.assertIn("if: needs.resolve.outputs.platforms == 'both'", workflow)
        finally:
            fp.cleanup(backend, plan)

    def test_manifestless_legacy_tags_remain_stable(self):
        context = common.ci_release_context(self.work, "v0.2.0")
        self.assertEqual((context["channel"], context["platforms"], context["feed_path"]),
                         ("stable", "both", "docs/appcast.xml"))

    def test_manual_stable_script_overwrites_previous_preview_manifest(self):
        shutil.copyfile(REPO / "scripts/tag-release.sh", self.work / "scripts/tag-release.sh")
        (self.work / common.CHANNEL_MANIFEST).write_text(json.dumps(common.channel_manifest("preview", "v0.2.0", self.head)))
        source = self.commit("previous preview marker and stable script")
        sh(self.work, "git", "push", "-q", "origin", "main")
        binaries = self.tmp / "bin"
        binaries.mkdir()
        fake_bin(binaries, "cargo", "exit 0\n")
        result = subprocess.run(["bash", "scripts/tag-release.sh", "v0.2.1"], cwd=self.work, capture_output=True, text=True,
                                env={**os.environ, "PATH": str(binaries) + os.pathsep + os.environ["PATH"], "PYTHONPATH": str(REPO)})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads((self.work / common.CHANNEL_MANIFEST).read_text()),
                         common.channel_manifest("stable", "v0.2.1", source))
        self.assertEqual(self.remote("refs/heads/main"), self.remote("refs/tags/v0.2.1"))

    def test_a_moved_tag_cannot_receive_the_previously_signed_preview_asset(self):
        plan = self.preview_plan()
        self.go(plan, "local")
        self.publish(plan, lambda *args: {"stage": args[-1]})
        self.uploads.clear()
        sh(self.work, "git", "push", "-q", "-f", "origin", f"{self.head}:refs/tags/{plan['tag']}")
        with self.assertRaisesRegex(Refused, "태그 커밋"):
            self.publish(plan, lambda *args: {"stage": args[-1]})
        self.assertEqual(self.uploads, [])


class ChannelFeedTests(unittest.TestCase):
    def test_preview_stages_only_its_feed_and_rejects_changed_identity_or_downgrade(self):
        with tempfile.TemporaryDirectory() as directory:
            repo = Path(directory)
            (repo / "docs").mkdir()
            stable, windows = repo / "docs/appcast.xml", repo / "docs/appcast-win.xml"
            stable.write_bytes(b"stable unchanged")
            windows.write_bytes(b"Windows unchanged")
            candidate = repo / "candidate.xml"
            item = {"url": "https://github.com/2rami/kasaterm/releases/download/v0.2.2/kasaterm-v0.2.2.dmg", "length": 100, "sig": "signed"}
            candidate.write_text(feed_xml("0.2.2", item))
            self.assertEqual(common.stage_appcasts(repo, "v0.2.2", "preview", candidate), ["docs/appcast-preview.xml"])
            self.assertEqual(common.stage_appcasts(repo, "v0.2.2", "preview", candidate), [])
            for altered in ({**item, "sig": "different"}, {**item, "url": "https://other.invalid/kasaterm-v0.2.2.dmg"}):
                candidate.write_text(feed_xml("0.2.2", altered))
                with self.assertRaises(Refused):
                    common.stage_appcasts(repo, "v0.2.2", "preview", candidate)
            candidate.write_text(feed_xml("0.2.1", {**item, "url": item["url"].replace("0.2.2", "0.2.1")}))
            with self.assertRaisesRegex(Refused, "이전 판"):
                common.stage_appcasts(repo, "v0.2.1", "preview", candidate)
            with self.assertRaisesRegex(Refused, "Windows"):
                common.stage_appcasts(repo, "v0.2.1", "preview", candidate, windows)
            self.assertEqual((stable.read_bytes(), windows.read_bytes()), (b"stable unchanged", b"Windows unchanged"))

    def test_invalid_second_feed_cannot_partially_stage_the_first(self):
        with tempfile.TemporaryDirectory() as directory:
            repo = Path(directory)
            (repo / "docs").mkdir()
            original = repo / "docs/appcast.xml"
            original.write_text(feed_xml("0.2.0"))
            before = original.read_bytes()
            mac, windows = repo / "mac.xml", repo / "windows.xml"
            mac.write_text(feed_xml("0.2.1", {"url": "https://github.com/2rami/kasaterm/releases/download/v0.2.1/kasaterm-v0.2.1.dmg", "length": 10, "sig": "ok"}))
            windows.write_text(feed_xml("0.2.0"))
            with self.assertRaises(Refused):
                common.stage_appcasts(repo, "v0.2.1", "stable", mac, windows)
            self.assertEqual(original.read_bytes(), before)
            self.assertFalse((repo / "docs/appcast-win.xml").exists())

    def test_checkpoint_replacement_is_atomic_and_private(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "state.json"
            common.save_json_atomic(path, {"old": True})
            with mock.patch.object(common.os, "replace", side_effect=OSError("interrupted")):
                with self.assertRaises(OSError):
                    common.save_json_atomic(path, {"new": True})
            self.assertEqual(json.loads(path.read_text()), {"old": True})
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            self.assertEqual(list(Path(directory).iterdir()), [path])


if __name__ == "__main__":
    unittest.main()
