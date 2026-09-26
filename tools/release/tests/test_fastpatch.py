from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import subprocess
import tempfile
import threading
import time
import unittest

from tools.release import fastpatch as fp

# 이 검사는 진짜 원격·피드·기기를 부르지 않는다. 원격은 임시 bare 저장소, 피드는 임시 파일,
# 기기는 이 프로세스가 연 가짜 `/version` 서버다. 실제 백엔드는 모든 단계를 거절하는지만 본다.

REPO = Path(__file__).resolve().parents[3]


def sh(cwd, *args):
    return subprocess.run(args, cwd=cwd, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE).stdout.decode().strip()


def feed_xml(version):
    return (f'<rss xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle"><channel><item>'
            f'<sparkle:version>{version}</sparkle:version></item></channel></rss>')


class FakeDevice:
    """`/version` 만 답하는 가짜 기기. answer 를 바꾸면 다음 물음부터 그 판을 말한다."""

    def __init__(self, version, build):
        self.answer = {"ok": True, "version": version, "build": build, "machine_id": "fake"}
        owner = self

        class H(BaseHTTPRequestHandler):
            def do_GET(self):
                body = json.dumps(owner.answer).encode()
                self.send_response(200 if self.path == "/version" else 404)
                self.send_header("Content-Type", "application/json")
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


class Fixture(unittest.TestCase):
    """v0.2.0 이 나간 뒤 네이티브 변경 하나·문서 변경 하나가 main 에 올라간 저장소."""

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="fastpatch-"))
        self.origin = self.tmp / "origin.git"
        self.work = self.tmp / "work"
        sh(self.tmp, "git", "init", "-q", "--bare", "-b", "main", str(self.origin))
        sh(self.tmp, "git", "clone", "-q", str(self.origin), str(self.work))
        for k, v in (("user.name", "t"), ("user.email", "t@t"), ("commit.gpgsign", "false"), ("tag.gpgsign", "false")):
            sh(self.work, "git", "config", k, v)
        (self.work / "Cargo.toml").write_text('[workspace.package]\nversion = "0.1.19"\n')
        (self.work / "app/kasaterm/src").mkdir(parents=True)
        (self.work / "app/kasaterm/src/main.rs").write_text("fn main() {}\n")
        self.commit("first")
        self.release("0.2.0")
        (self.work / "app/kasaterm/src/main.rs").write_text("fn main() { println!(); }\n")
        self.commit("fix: 작은 수정")
        (self.work / "docs").mkdir()
        (self.work / "docs/note.md").write_text("n\n")
        self.commit("docs: 메모")
        sh(self.work, "git", "push", "-q", "origin", "main")
        self.head = sh(self.work, "git", "rev-parse", "HEAD")
        self.feed = self.tmp / "appcast.xml"
        self.feed.write_text(feed_xml("0.2.0"))
        self.state = self.tmp / "state"
        self.devices = []

    def tearDown(self):
        for d in self.devices:
            d.close()
        subprocess.run(["rm", "-rf", str(self.tmp)])

    def commit(self, msg):
        sh(self.work, "git", "add", "-A")
        sh(self.work, "git", "commit", "-qm", msg)
        return sh(self.work, "git", "rev-parse", "HEAD")

    def release(self, version):
        cargo = self.work / "Cargo.toml"
        cargo.write_text(f'[workspace.package]\nversion = "{version}"\n')
        self.commit(f"chore(release): v{version}")
        sh(self.work, "git", "tag", f"v{version}")
        sh(self.work, "git", "push", "-q", "origin", "main", f"v{version}")

    def device(self, label, version, build):
        d = FakeDevice(version, build)
        self.devices.append(d)
        return {"label": label, "base": d.base, "fake": d}

    def plan(self, devices=(), **kw):
        plan = fp.make_plan(self.work, feed=str(self.feed),
                            devices=[{"label": d["label"], "base": d["base"]} for d in devices], **kw)
        fp.save_plan(plan, self.state)
        return plan

    def approve(self, plan, **over):
        a = {**fp.approval_template(plan), "approved_by": "사람", "approved_at_ms": fp.now_ms(),
             "expires_at_ms": fp.now_ms() + 3600_000, **over}
        (fp.plan_dir(plan["plan_id"], self.state) / "approval.json").write_text(json.dumps(a))
        return a

    def mock(self, devices=(), drop=()):
        return fp.MockBackend(self.work, self.feed, self.tmp / "artifacts",
                              [{"label": d["label"], "base": d["base"]} for d in devices], drop)

    def remote_tags(self):
        return sorted(line.split("/")[-1] for line in sh(self.work, "git", "ls-remote", "--tags", "origin").splitlines()
                      if not line.endswith("^{}"))


class PlanTests(Fixture):
    def test_next_patch_is_above_every_published_version(self):
        plan = self.plan()
        self.assertEqual(plan["errors"], [])
        self.assertEqual(plan["tag"], "v0.2.1")
        self.assertEqual(plan["commit"], self.head)
        self.assertEqual(plan["base"]["tag"], "v0.2.0")
        self.assertEqual([c["subject"] for c in plan["changes"]["commits"]], ["docs: 메모", "fix: 작은 수정"])
        self.assertEqual(plan["platforms"], ["macos", "windows"])
        self.assertIsNone(plan["ios_build"])

    def test_the_feed_counts_even_when_a_tag_is_missing(self):
        # 로컬 태그가 늦거나 원격 태그가 지워져도 피드에 나간 판보다 낮은 번호는 다시 안 쓴다.
        self.feed.write_text(feed_xml("0.2.3"))
        self.assertEqual(self.plan()["tag"], "v0.2.4")

    def test_local_tags_do_not_decide_the_next_number(self):
        # 원격엔 있는데 이 저장소엔 없는 태그 — 미니에 v0.2.0 이 없던 때와 같은 모양.
        sh(self.work, "git", "tag", "-d", "v0.2.0")
        plan = self.plan()
        self.assertEqual(plan["tag"], "v0.2.1")
        self.assertEqual(plan["base"]["tag"], "v0.2.0")
        self.assertEqual(len(plan["changes"]["commits"]), 2)

    def test_an_existing_tag_or_a_lower_version_is_refused(self):
        self.assertIn("원격에 이미 있다", " ".join(self.plan(version="0.2.0")["errors"]))
        self.assertIn("다운그레이드", " ".join(self.plan(version="0.1.30")["errors"]))

    def test_uncommitted_and_unpushed_work_is_refused(self):
        (self.work / "app/kasaterm/src/main.rs").write_text("fn main() { loop {} }\n")
        self.assertIn("커밋 안 된 변경", " ".join(self.plan()["errors"]))
        self.commit("wip")
        self.assertIn("push 된 커밋만", " ".join(self.plan()["errors"]))

    def test_a_channel_the_updater_does_not_have_is_refused(self):
        errors = " ".join(self.plan(channel="preview")["errors"])
        self.assertIn("채널 preview 은 이 앱의 업데이터에 없다", errors)

    def test_mobile_changes_ask_for_a_testflight_build(self):
        (self.work / "mobile/lib").mkdir(parents=True)
        (self.work / "mobile/lib/a.dart").write_text("//\n")
        self.commit("feat: 폰")
        sh(self.work, "git", "push", "-q", "origin", "main")
        plan = self.plan()
        self.assertIn("ios", plan["platforms"])
        self.assertRegex(plan["ios_build"], r"^\d{10}$")
        self.assertTrue(plan["changes"]["needs"]["ios_build"])

    def test_devices_are_compared_by_version_and_sha(self):
        old = sh(self.work, "git", "rev-parse", "v0.2.0")[:8]
        same_version = self.device("같은 번호 다른 판", "0.2.0", old)
        dirty = self.device("미커밋 판", "0.2.0", old + "+")
        stranger = self.device("원격에 없는 판", "0.2.0", "84190e33")
        ahead = self.device("더 새 판", "0.3.0", "abcdef12")
        plan = self.plan([same_version, dirty, stranger, ahead,
                          {"label": "꺼진 기기", "base": "http://127.0.0.1:9"}])
        states = {d["label"]: d["state"] for d in plan["baseline"]}
        self.assertEqual(states, {"같은 번호 다른 판": "update", "미커밋 판": "update", "원격에 없는 판": "hold",
                                  "더 새 판": "newer", "꺼진 기기": "offline"})
        # 보류·더 새 판은 이 계획의 기기 범위에 안 들어간다 — 승인도 그 범위만 받는다.
        self.assertEqual(plan["devices"], ["같은 번호 다른 판", "꺼진 기기", "미커밋 판"])

    def test_the_plan_id_is_bound_to_the_fixed_scope(self):
        a, b = self.plan(), self.plan()
        self.assertEqual(a["plan_id"], b["plan_id"])
        self.assertNotEqual(a["plan_id"], self.plan(version="0.2.5")["plan_id"])


class ApprovalTests(Fixture):
    def test_nothing_runs_without_a_matching_live_approval(self):
        plan = self.plan()
        with self.assertRaisesRegex(fp.Refused, "승인 파일이 없다"):
            fp.run(plan["plan_id"], self.mock(), self.state)
        self.approve(plan, plan_id="0" * 16)
        with self.assertRaisesRegex(fp.Refused, "plan_id"):
            fp.run(plan["plan_id"], self.mock(), self.state)
        self.approve(plan, devices=["아무 기기"])
        with self.assertRaisesRegex(fp.Refused, "범위"):
            fp.run(plan["plan_id"], self.mock(), self.state)
        self.approve(plan, expires_at_ms=fp.now_ms() - 1)
        with self.assertRaisesRegex(fp.Refused, "만료"):
            fp.run(plan["plan_id"], self.mock(), self.state)
        self.approve(plan, approved_by="")
        with self.assertRaisesRegex(fp.Refused, "승인한 사람"):
            fp.run(plan["plan_id"], self.mock(), self.state)
        self.assertEqual(self.remote_tags(), ["v0.2.0"])

    def test_a_plan_with_errors_never_runs_even_when_approved(self):
        plan = self.plan(version="0.2.0")
        self.approve(plan)
        with self.assertRaisesRegex(fp.Refused, "막힘"):
            fp.run(plan["plan_id"], self.mock(), self.state)

    def test_the_real_backend_publishes_nothing_in_this_version(self):
        plan = self.plan()
        self.approve(plan)
        with self.assertRaisesRegex(fp.Refused, "verify: 실제 백엔드는 이 판에서 막혀 있다"):
            fp.run(plan["plan_id"], fp.RealBackend(), self.state)
        _, state = fp.load(plan["plan_id"], self.state)
        self.assertEqual(state["stages"]["verify"]["status"], "failed")
        self.assertNotIn("tag", state["stages"])
        self.assertEqual(self.remote_tags(), ["v0.2.0"])


class RunTests(Fixture):
    def test_one_approval_carries_every_stage_and_rerun_adds_nothing(self):
        dev = self.device("맥북", "0.2.0", sh(self.work, "git", "rev-parse", "v0.2.0")[:8])
        plan = self.plan([dev])
        self.approve(plan)
        state = fp.run(plan["plan_id"], self.mock([dev]), self.state)
        self.assertEqual({k: v["status"] for k, v in state["stages"].items()}, {s: "done" for s in fp.STAGES})
        self.assertEqual(self.remote_tags(), ["v0.2.0", "v0.2.1"])
        bump = sh(self.work, "git", "rev-parse", "v0.2.1^{commit}")
        self.assertEqual(sh(self.work, "git", "rev-parse", f"{bump}^"), self.head)
        self.assertEqual(fp.feed_version(self.feed.read_text()), "0.2.1")
        self.assertEqual(state["devices"]["맥북"]["state"], "update")
        self.assertIn("피드는 나갔다", state["devices"]["맥북"]["why"])

        # 기기가 CI 가 구운 태그 커밋 판을 받아 오면 목표 판으로 본다.
        dev["fake"].answer = {"ok": True, "version": "0.2.1", "build": bump[:8]}
        state = fp.run(plan["plan_id"], self.mock([dev]), self.state)
        self.assertEqual(state["devices"]["맥북"]["state"], "current")
        self.assertEqual(self.remote_tags(), ["v0.2.0", "v0.2.1"])

    def test_a_lost_state_file_does_not_raise_a_second_tag(self):
        plan = self.plan()
        self.approve(plan)
        fp.run(plan["plan_id"], self.mock(), self.state)
        (fp.plan_dir(plan["plan_id"], self.state) / "state.json").unlink()
        state = fp.run(plan["plan_id"], self.mock(), self.state)
        self.assertIn("다시 세우지 않음", state["stages"]["tag"]["detail"]["result"])
        self.assertIn("다시 쓰지 않음", state["stages"]["feed"]["detail"]["result"])
        self.assertEqual(self.remote_tags(), ["v0.2.0", "v0.2.1"])

    def test_missing_artifacts_keep_the_feed_untouched(self):
        plan = self.plan()
        self.approve(plan)
        with self.assertRaisesRegex(fp.Refused, "msi"):
            fp.run(plan["plan_id"], self.mock(drop=[".msi"]), self.state)
        _, state = fp.load(plan["plan_id"], self.state)
        self.assertEqual(state["stages"]["release"]["status"], "failed")
        self.assertNotIn("feed", state["stages"])
        self.assertEqual(fp.feed_version(self.feed.read_text()), "0.2.0")

    def test_main_moving_after_the_plan_needs_a_new_plan(self):
        plan = self.plan()
        self.approve(plan)
        (self.work / "app/kasaterm/src/main.rs").write_text("fn main() { todo!() }\n")
        self.commit("fix: 계획 뒤 변경")
        sh(self.work, "git", "push", "-q", "origin", "main")
        with self.assertRaisesRegex(fp.Refused, "새 계획"):
            fp.run(plan["plan_id"], self.mock(), self.state)
        self.assertEqual(self.remote_tags(), ["v0.2.0"])

    def test_a_feed_already_ahead_is_never_rolled_back(self):
        plan = self.plan()
        self.approve(plan)
        self.feed.write_text(feed_xml("0.3.0"))
        with self.assertRaisesRegex(fp.Refused, "다운그레이드"):
            fp.run(plan["plan_id"], self.mock(), self.state)
        self.assertEqual(fp.feed_version(self.feed.read_text()), "0.3.0")

    def test_status_keeps_the_last_seen_build_of_an_offline_device(self):
        dev = self.device("windesktop", "0.2.0", sh(self.work, "git", "rev-parse", "v0.2.0")[:8])
        plan = self.plan([dev])
        _, state = fp.load(plan["plan_id"], self.state)
        seen = fp.track_devices(self.work, plan, state, [{"label": "windesktop", "base": dev["base"]}])
        checked = seen["windesktop"]["checked_at_ms"]
        dev["fake"].close()
        self.devices.remove(dev["fake"])
        time.sleep(0.01)
        row = fp.track_devices(self.work, plan, state, [{"label": "windesktop", "base": dev["base"]}])["windesktop"]
        self.assertEqual((row["state"], row["version"], row["checked_at_ms"]), ("offline", "0.2.0", checked))
        self.assertIn("오프라인", fp.describe(plan, state))


class RealRepoTests(unittest.TestCase):
    def test_capabilities_match_the_updaters_in_this_repo(self):
        caps = fp.capabilities(REPO)
        # Sparkle 에 채널을 넘기는 대리자가 없으니 stable 하나뿐이다 — preview 를 지어내지 않는다.
        self.assertEqual(caps["macos"]["channels"], ["stable"])
        self.assertEqual(caps["macos"]["updater"], "sparkle")
        self.assertEqual(caps["macos"]["feed"], "https://2rami.github.io/kasaterm/appcast.xml")
        self.assertFalse(caps["macos"]["notarized"])
        self.assertEqual(caps["windows"]["channels"], ["stable"])
        self.assertFalse(caps["ios"]["hotpatch"])
        self.assertTrue(caps["notes"])

    def test_tunnel_ports_match_machines_rs(self):
        self.assertEqual((fp.tunnel_port("맥북"), fp.tunnel_port("windesktop")), (18986, 18917))

    def test_classify_splits_native_mobile_and_non_shipping_files(self):
        kinds = fp.classify(["app/kasaterm/src/a.rs", "crates/kasa-mcp/src/b.rs", "mobile/lib/c.dart",
                             "docs/d.md", "docs/appcast.xml", "scripts/e.sh", "README.md"])
        self.assertEqual(kinds["native"], ["app/kasaterm/src/a.rs", "crates/kasa-mcp/src/b.rs"])
        self.assertEqual(kinds["mobile"], ["mobile/lib/c.dart"])
        self.assertEqual(kinds["feed"], ["docs/appcast.xml"])
        self.assertEqual(kinds["docs"], ["docs/d.md", "README.md"])
        self.assertEqual(kinds["infra"], ["scripts/e.sh"])


if __name__ == "__main__":
    unittest.main()
