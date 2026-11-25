import base64
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import unittest

from tools.release import verify_hosts as vh
from tools.release.common import Pending, Refused
from tools.release.proc import Result

COMMIT = "c" * 40
PLAN = {"plan_id": "0123456789abcdef", "commit": COMMIT}
PACKAGES = ("kasaterm", "kasa-socket")
MINI = {"ncpu": "10", "load": "4.88", "mem": str(16 * 2**30), "free_pct": "31", "power": "Now drawing from 'AC Power'",
        "disk_kb": str(26 * 2**20)}
MACBOOK = {"ncpu": "14", "load": "6.97", "mem": str(36 * 2**30), "free_pct": "74", "power": "Now drawing from 'AC Power'",
           "disk_kb": str(146 * 2**20)}


def lines(facts):
    return "".join(f"{k}={v}\n" for k, v in facts.items())


def script_of(argv):
    """ssh 인자의 원격 명령에서 실은 스크립트를 꺼낸다."""
    encoded = argv[-1].split()[1]
    return base64.b64decode(encoded).decode()


class FakeHosts:
    """controller 와 원격 맥의 답을 정한다. 원격 검사의 상태는 status 로 바꾼다."""

    def __init__(self):
        self.local, self.remote, self.reachable = dict(MINI), dict(MACBOOK), True
        self.status = None
        self.started, self.cancelled = [], []

    def run(self, argv, cwd=None, timeout=60, env=None, kind="read"):
        if argv[:2] == ["sh", "-c"]:
            return Result(0, lines(self.local))
        if argv[0].endswith("rustc"):
            return Result(0, "rustc 1.98.1 (48a229cea 2026-09-01)\n")
        if argv[0] == "ssh":
            if not self.reachable:
                return Result(255, "", "ssh: connect to host localhost port 2222: Connection refused")
            script = script_of(argv)
            if script == vh.PROBE:
                return Result(0, lines(self.remote))
            if script == vh.START:
                self.started.append(argv[-1].split("sh -s -- ")[1].split())
                self.status = {"plan": PLAN["plan_id"], "commit": COMMIT, "state": "running"}
                return Result(0, "state=started\n")
            if script == vh.STATUS:
                return Result(0, lines(self.status or {"state": "missing"}))
            if script == vh.CANCEL:
                self.cancelled.append(argv[-1])
                return Result(0, "state=cancelled\n")
        return Result(127, "", f"없는 명령: {argv}")


class CapacityTests(unittest.TestCase):
    def test_the_busy_mini_gets_one_job_and_the_idle_macbook_more(self):
        self.assertEqual(vh.capacity(MINI, personal=False)[0], 1)
        self.assertEqual(vh.capacity(MACBOOK, personal=True)[0], 5)

    def test_a_personal_machine_keeps_two_cores_and_needs_power_and_disk(self):
        quiet = {**MACBOOK, "load": "0.5"}
        self.assertEqual(vh.capacity(quiet, personal=True)[0], 8)
        self.assertEqual(vh.capacity({**quiet, "ncpu": "8"}, personal=True)[0], 5)
        self.assertEqual(vh.capacity({**quiet, "power": "Now drawing from 'Battery Power'"}, personal=True),
                         (0, "배터리로 돈다"))
        self.assertEqual(vh.capacity({**quiet, "disk_kb": str(10 * 2**20)}, personal=True)[0], 0)

    def test_memory_pressure_bounds_jobs_and_unreadable_usage_gives_none(self):
        self.assertEqual(vh.capacity({**MACBOOK, "load": "0", "free_pct": "10"}, personal=True)[0], 1)
        self.assertEqual(vh.capacity({}, personal=False), (0, "사용량을 못 읽었다"))


class ChooseTests(unittest.TestCase):
    def test_the_machine_with_more_room_wins_and_a_tie_stays_on_the_controller(self):
        hosts = FakeHosts()
        choice = vh.choose(hosts, ("macbook",))
        self.assertEqual((choice["host"], choice["jobs"]), ("macbook", 5))
        self.assertEqual([s["host"] for s in choice["seen"]], ["controller", "macbook"])
        hosts.remote["load"] = "11.5"
        self.assertEqual(vh.choose(hosts, ("macbook",))["host"], None)

    def test_an_unreachable_host_is_noted_and_skipped(self):
        hosts = FakeHosts()
        hosts.reachable = False
        choice = vh.choose(hosts, ("macbook",))
        self.assertEqual((choice["host"], choice["seen"][1]["why"]), (None, "닿지 않는다"))

    def test_an_explicit_job_budget_fixes_the_controller(self):
        hosts = FakeHosts()
        hosts.remote["load"] = "11.5"
        self.assertEqual(vh.choose(hosts, ("macbook",), fixed=3)["jobs"], 3)
        self.assertEqual(vh.explicit_jobs({"CARGO_BUILD_JOBS": "3"}), 3)
        self.assertIsNone(vh.explicit_jobs({"CARGO_BUILD_JOBS": "auto"}))

    def test_hosts_come_from_the_environment_when_set(self):
        self.assertEqual(vh.configured_hosts({}), vh.HOSTS)
        self.assertEqual(vh.configured_hosts({vh.HOSTS_ENV: ""}), ())
        self.assertEqual(vh.configured_hosts({vh.HOSTS_ENV: "a, b;rm"}), ("a",))

    def test_remote_commands_carry_the_script_and_refuse_quoted_arguments(self):
        cmd = vh.remote_command(vh.STATUS, "abc", "-p", "kasa-socket")
        self.assertEqual(script_of(["ssh", "host", cmd]), vh.STATUS)
        self.assertTrue(cmd.endswith("sh -s -- abc -p kasa-socket"))
        with self.assertRaises(Refused):
            vh.remote_command(vh.STATUS, "a b")
        with self.assertRaises(Refused):
            vh.remote_command(vh.STATUS, "$(id)")


class PlaceTests(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="verify-hosts-"))
        self.hosts = FakeHosts()
        self.now = 1_000_000

    def tearDown(self):
        shutil.rmtree(self.tmp)

    def place(self):
        return vh.place(self.hosts, self.tmp, PLAN, "/x/.cargo/bin/cargo", PACKAGES, hosts=("macbook",),
                        clock=lambda: self.now, timeout_ms=7200 * 1000)

    def test_a_remote_check_runs_in_the_background_and_its_pass_is_the_evidence(self):
        with self.assertRaisesRegex(Pending, "macbook 에서 검사 시작"):
            self.place()
        self.assertEqual(self.hosts.started, [[PLAN["plan_id"], COMMIT, "5", "1.98.1", "-p", "kasaterm", "-p", "kasa-socket"]])
        self.now += 7 * 60000
        with self.assertRaisesRegex(Pending, "검사 중\\(일꾼 5\\) · 7분째"):
            self.place()
        self.hosts.status = {**self.hosts.status, "head": COMMIT, "exit": "0"}
        done = self.place()["done"]
        self.assertEqual((done["host"], done["jobs"], done["ran"]),
                         ("macbook", 5, ["cargo +1.98.1 test -p kasaterm -p kasa-socket --release"]))
        self.assertEqual(len(self.hosts.started), 1)

    def test_when_the_controller_has_more_room_it_checks_locally(self):
        self.hosts.remote["load"] = "11.5"
        placed = self.place()
        self.assertEqual((placed["jobs"], placed["choice"]["host"], self.hosts.started), (1, None, []))

    def test_a_failed_setup_falls_back_to_the_controller_for_this_plan(self):
        with self.assertRaises(Pending):
            self.place()
        self.hosts.status = {**self.hosts.status, "exit": "setup", "why": "rustup 이 없다"}
        placed = self.place()
        self.assertEqual((placed["jobs"], placed["choice"]["fallback"]), (1, "macbook 준비 실패 — rustup 이 없다"))
        self.assertEqual(self.place()["choice"]["fallback"], "macbook 준비 실패 — rustup 이 없다")
        self.assertEqual(len(self.hosts.started), 1)

    def test_a_failing_test_is_retried_once_then_kept(self):
        with self.assertRaises(Pending):
            self.place()
        failed = {**self.hosts.status, "head": COMMIT, "exit": "101", "log": "test socket::x ... FAILED"}
        self.hosts.status = failed
        with self.assertRaisesRegex(Refused, "검사 실패\\(macbook\\): cargo \\+1.98.1 test .* socket::x ... FAILED"):
            self.place()
        with self.assertRaises(Pending):
            self.place()
        self.hosts.status = failed
        with self.assertRaisesRegex(Refused, "검사 실패"):
            self.place()
        with self.assertRaisesRegex(Refused, "검사 실패"):
            self.place()
        self.assertEqual(len(self.hosts.started), 2)

    def test_an_unreachable_host_is_waited_for_then_abandoned(self):
        with self.assertRaises(Pending):
            self.place()
        self.hosts.reachable = False
        self.now += 5 * 60000
        with self.assertRaisesRegex(Pending, "확인하지 못했다"):
            self.place()
        self.now += vh.UNREACHABLE_GRACE_MS + 1
        placed = self.place()
        self.assertEqual(placed["choice"]["fallback"], "macbook 에 20분 닿지 않았다")

    def test_a_check_past_the_time_limit_is_cancelled_and_refused(self):
        with self.assertRaises(Pending):
            self.place()
        self.now += 7200 * 1000 + 1
        with self.assertRaisesRegex(Refused, "검사 시간 초과\\(macbook"):
            self.place()
        self.assertEqual(len(self.hosts.cancelled), 1)
        with self.assertRaisesRegex(Refused, "검사 시간 초과"):
            self.place()

    def test_a_vanished_remote_check_is_started_again(self):
        with self.assertRaises(Pending):
            self.place()
        self.hosts.status = {"state": "missing"}
        with self.assertRaisesRegex(Pending, "검사 시작"):
            self.place()
        self.assertEqual(len(self.hosts.started), 2)

    def test_a_pass_from_another_commit_is_not_evidence(self):
        with self.assertRaises(Pending):
            self.place()
        self.hosts.status = {**self.hosts.status, "head": "d" * 40, "exit": "0"}
        with self.assertRaisesRegex(Refused, "검사 실패"):
            self.place()

    def test_without_the_controller_toolchain_it_stays_local(self):
        run = self.hosts.run
        self.hosts.run = lambda argv, **kw: Result(1, "") if argv[0].endswith("rustc") else run(argv, **kw)
        placed = self.place()
        self.assertEqual((placed["jobs"], placed["choice"]["host"]), (1, None))
        self.assertEqual(placed["choice"]["fallback"], "controller 의 rust 판을 못 읽었다")
        self.assertEqual(self.hosts.started, [])


@unittest.skipUnless(shutil.which("perl"), "perl 이 없다")
class ScriptTests(unittest.TestCase):
    """원격에서 도는 셸 스크립트를 가짜 git·rustup·cargo 로 실제로 돌린다."""

    def setUp(self):
        self.home = Path(tempfile.mkdtemp(prefix="verify-home-"))
        bin_dir = self.home / ".cargo/bin"
        bin_dir.mkdir(parents=True)
        fakes = {
            "git": """#!/bin/sh
[ -n "${FAKE_NO_LFS:-}" ] && [ "$1" = lfs ] && exit 1
case "$1 $2" in
  "lfs version"|"lfs install"|"fetch -q"|"checkout -q"|"clean -ffdxq") exit 0 ;;
  "clone -q") mkdir -p "$6/.git"; exit 0 ;;
  "show $FAKE_COMMIT:.lfsconfig") printf '[lfs]\\n\\turl = https://lfs.invalid/lfs\\n'; exit 0 ;;
  "config -f") echo https://lfs.invalid/lfs; exit 0 ;;
  "config lfs.url") echo "$3" > "$HOME/lfs-url"; exit 0 ;;
  "rev-parse HEAD") echo "$FAKE_COMMIT"; exit 0 ;;
  "status --porcelain") exit 0 ;;
esac
echo "fake git: $*" >&2; exit 1
""",
            "rustup": "#!/bin/sh\n[ \"$1 $2\" = 'toolchain list' ] && echo 1.98.1-aarch64-apple-darwin\nexit 0\n",
            "cargo": "#!/bin/sh\necho \"$CARGO_BUILD_JOBS $CARGO_TARGET_DIR $*\" > \"$HOME/cargo-args\"\n"
                     "echo 'test result: ok'\nexit \"${FAKE_CARGO_EXIT:-0}\"\n",
        }
        for name, body in fakes.items():
            (bin_dir / name).write_text(body)
            (bin_dir / name).chmod(0o755)

    def tearDown(self):
        shutil.rmtree(self.home)

    def sh(self, script, *args, **env):
        out = subprocess.run(["sh", "-c", vh.remote_command(script, *args)], capture_output=True, text=True, timeout=30,
                             env={"HOME": str(self.home), "PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "FAKE_COMMIT": COMMIT, **env})
        return vh.parse(out.stdout)

    def wait_exit(self):
        for _ in range(100):
            st = self.sh(vh.STATUS)
            if "exit" in st:
                return st
            time.sleep(0.1)
        self.fail("원격 검사가 끝나지 않았다")

    def test_scripts_parse(self):
        for script in (vh.PROBE, vh.JOB, vh.START, vh.STATUS, vh.CANCEL):
            subprocess.run(["sh", "-n"], input=script, text=True, check=True)

    def test_start_runs_the_tests_on_the_plan_commit_in_the_background(self):
        self.assertEqual(self.sh(vh.START, PLAN["plan_id"], COMMIT, "5", "1.98.1", "-p", "kasaterm")["state"], "started")
        st = self.wait_exit()
        self.assertEqual((st["exit"], st["head"], st["plan"]), ("0", COMMIT, PLAN["plan_id"]))
        self.assertIn("test result: ok", st["log"])
        root = self.home / ".local/share/kasaterm-verify"
        self.assertEqual((self.home / "cargo-args").read_text().split(),
                         ["5", str(root / "target"), "+1.98.1", "test", "-p", "kasaterm", "--release"])
        self.assertEqual((self.home / "lfs-url").read_text().strip(), "https://lfs.invalid/lfs")

    def test_a_failing_run_reports_its_exit_code(self):
        self.sh(vh.START, PLAN["plan_id"], COMMIT, "2", "1.98.1", "-p", "kasaterm", FAKE_CARGO_EXIT="101")
        self.assertEqual(self.wait_exit()["exit"], "101")

    def test_a_missing_tool_is_a_setup_failure(self):
        self.sh(vh.START, PLAN["plan_id"], COMMIT, "2", "1.98.1", "-p", "kasaterm", FAKE_NO_LFS="1")
        st = self.wait_exit()
        self.assertEqual((st["exit"], st["why"]), ("setup", "git-lfs 가 없다"))
        self.assertFalse((self.home / "cargo-args").exists())

    def test_a_newer_plan_stops_the_old_run(self):
        (self.home / ".cargo/bin/cargo").write_text("#!/bin/sh\nsleep 30\n")
        self.sh(vh.START, PLAN["plan_id"], COMMIT, "2", "1.98.1", "-p", "kasaterm")
        for _ in range(100):
            if self.sh(vh.STATUS).get("state") == "running" and (self.home / ".local/share/kasaterm-verify/run/head").exists():
                break
            time.sleep(0.1)
        old = (self.home / ".local/share/kasaterm-verify/run/pid").read_text().strip()
        self.assertEqual(self.sh(vh.START, PLAN["plan_id"], COMMIT, "2", "1.98.1", "-p", "kasaterm")["state"], "running")
        self.sh(vh.START, "fedcba9876543210", COMMIT, "2", "1.98.1", "-p", "kasaterm")
        self.assertNotEqual(subprocess.run(["kill", "-0", old], capture_output=True).returncode, 0)
        self.assertEqual(self.sh(vh.STATUS)["plan"], "fedcba9876543210")
        self.sh(vh.CANCEL, "fedcba9876543210")


if __name__ == "__main__":
    unittest.main()
