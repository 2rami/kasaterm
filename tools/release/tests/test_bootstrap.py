import contextlib
import io
import json
import os
from pathlib import Path
import plistlib
import shutil
import sys
import tempfile
import threading
from types import SimpleNamespace
import unittest
from unittest import mock

from tools.release import bootstrap as b


class FakeRuntime:
    def __init__(self, home):
        self.home = home
        self.identity = "fixture-machine"
        self.live = {}
        self.signature_calls = []
        self.moves = []
        self.waits = []
        self.launched = []
        self.rename_failure = None
        self.exchange_failure = None
        self.registered = set()
        self.process_hook = None
        self.bad_signature = set()

    def machine(self):
        return self.identity

    def signature(self, app):
        self.signature_calls.append(app)
        if str(app) in self.bad_signature:
            raise b.Refused("fixture invalid signature")

    def copy(self, source, target):
        shutil.copytree(source, target, symlinks=True)

    def processes(self):
        if self.process_hook:
            self.process_hook()
        return dict(self.live)

    def wait_for_exit(self, pids, timeout):
        self.waits.append((dict(pids), timeout))
        self.live.clear()

    def rename_exclusive(self, source, target):
        self.moves.append((source, target))
        if self.rename_failure and self.rename_failure(source, target):
            raise OSError("fixture move failure")
        if target.exists() or target.is_symlink():
            raise FileExistsError("fixture destination exists")
        os.rename(source, target)

    def exchange(self, source, target):
        self.moves.append((source, target))
        if self.exchange_failure:
            raise OSError("fixture exchange failure")
        temporary = source.parent / (source.name + ".fixture-exchange")
        os.rename(source, temporary)
        os.rename(target, source)
        os.rename(temporary, target)

    def launch(self, plist):
        self.launched.append(plist)
        self.registered.add(plistlib.loads(plist.read_bytes())["Label"])

    def job_registered(self, label):
        return label in self.registered


class BootstrapTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name).resolve()
        self.home = self.root / "user"
        self.home.mkdir()
        (self.home / "Applications").mkdir()
        (self.home / ".config/kasaterm").mkdir(parents=True)
        self.settings = self.home / ".config/kasaterm/settings.json"
        self.original = {"theme":"custom", "secret":"never-log-this", "devices":[{"token":"private"}],
                         "update_channel":"stable", "automatic_update_on_quit":False}
        self.settings.write_text(json.dumps(self.original))
        self.runtime = FakeRuntime(self.home)
        self.tool = b.Bootstrap(runtime=self.runtime)
        self.app = self.bundle(self.home / "Applications/kasaterm.app", "old")
        self.source = self.bundle(self.root / "source.app", "new")

    def tearDown(self):
        self.tmp.cleanup()

    def bundle(self, path, content, kind="kasaterm.app", version="0.2.2", build="0.2.2"):
        bundle_id, executable = b.KINDS[kind]
        (path / "Contents/MacOS").mkdir(parents=True)
        (path / "Contents/Info.plist").write_bytes(plistlib.dumps({
            "CFBundleIdentifier":bundle_id, "CFBundleExecutable":executable,
            "CFBundleShortVersionString":version, "CFBundleVersion":build}))
        binary = path / "Contents/MacOS" / executable
        binary.write_text(content)
        binary.chmod(0o755)
        return path

    def stage(self, app=None, source=None):
        app, source = app or self.app, source or self.source
        info = plistlib.loads((source / "Contents/Info.plist").read_bytes())
        return self.tool.stage(app, source, "fixture-machine",
                               b.file_hash(source / "Contents/MacOS" / info["CFBundleExecutable"]),
                               info["CFBundleShortVersionString"])

    def binary(self, app):
        info = plistlib.loads((app / "Contents/Info.plist").read_bytes())
        return (app / "Contents/MacOS" / info["CFBundleExecutable"]).read_text()

    def test_lifecycle_waits_for_exact_executable_then_preserves_both_bundles_and_settings(self):
        result = self.stage()
        self.runtime.live = {42:str(self.app / "Contents/MacOS/kasaterm")}
        self.assertEqual(self.tool.install_when_closed(result["id"])["state"], "waiting")
        self.assertEqual(json.loads(self.settings.read_text()), self.original)
        self.assertEqual(self.runtime.moves, [])
        completed = self.tool.wait(result["id"], 60)
        self.assertEqual(completed["state"], "installed")
        self.assertEqual(len(self.runtime.waits), 1)
        self.assertEqual(self.binary(self.app), "new")
        self.assertEqual(self.binary(Path(result["backup"])), "old")
        self.assertFalse(Path(result["stage"]).exists())
        expected = dict(self.original, update_channel="preview", automatic_update_on_quit=True)
        self.assertEqual(json.loads(self.settings.read_text()), expected)
        self.assertNotIn("never-log-this", json.dumps(completed))
        self.assertEqual(self.tool.install_when_closed(result["id"]), completed)

    def test_viewer_waits_only_for_its_own_process_and_never_changes_shared_settings(self):
        app = self.bundle(self.home / "Applications/KasaViewer.app", "viewer-old", "kasaviewer.app")
        source = self.bundle(self.root / "viewer.app", "viewer-new", "kasaviewer.app")
        result = self.stage(app, source)
        self.runtime.live = {31:str(app / "Contents/MacOS/kasaterm-viewer")}
        self.assertEqual(self.tool.install_when_closed(result["id"])["state"], "waiting")
        self.runtime.live = {32:str(self.app / "Contents/MacOS/kasaterm")}
        self.assertEqual(self.tool.install_when_closed(result["id"])["state"], "installed")
        self.assertEqual(self.binary(app), "viewer-new")
        self.assertEqual(json.loads(self.settings.read_text()), self.original)

    def test_a_reopened_application_is_not_replaced(self):
        result = self.stage()
        calls = []
        def reopen():
            calls.append(None)
            if len(calls) == 2:
                self.runtime.live = {90:str(self.app / "Contents/MacOS/kasaterm")}
        self.runtime.process_hook = reopen
        self.assertEqual(self.tool.install_when_closed(result["id"])["state"], "waiting")
        self.assertEqual(self.runtime.moves, [])
        self.assertEqual(self.binary(self.app), "old")

    def test_install_rename_failure_rolls_back_without_deleting_new_bundle(self):
        result = self.stage()
        self.runtime.rename_failure = lambda source, _: source == Path(result["stage"])
        self.assertEqual(self.tool.install_when_closed(result["id"])["state"], "failed")
        self.assertEqual(self.binary(self.app), "old")
        self.assertEqual(self.binary(Path(result["stage"])), "new")
        self.assertFalse(Path(result["backup"]).exists())

    def test_unsupported_exchange_keeps_installed_path_and_staged_bundle(self):
        result = self.stage()
        self.runtime.exchange_failure = True
        self.assertEqual(self.tool.install_when_closed(result["id"])["state"], "failed")
        self.assertEqual(self.binary(self.app), "old")
        self.assertEqual(self.binary(Path(result["stage"])), "new")
        self.assertFalse(Path(result["backup"]).exists())

    def test_inventory_failure_after_exchange_preserves_both_bundles_and_live_path(self):
        result = self.stage()
        def inventory():
            if self.runtime.moves:
                raise b.Refused("fixture unavailable inventory")
        self.runtime.process_hook = inventory
        self.assertEqual(self.tool.install_when_closed(result["id"])["state"], "rollback_required")
        self.assertEqual(self.binary(self.app), "new")
        self.assertEqual(self.binary(Path(result["stage"])), "old")
        self.assertFalse(Path(result["backup"]).exists())
        self.assertEqual(len(self.runtime.moves), 1)

    def test_failed_atomic_rollback_keeps_new_app_and_old_staged_bundle(self):
        result = self.stage()
        def fail_backup(source, target):
            self.runtime.exchange_failure = True
            return True
        self.runtime.rename_failure = fail_backup
        self.assertEqual(self.tool.install_when_closed(result["id"])["state"], "rollback_required")
        self.assertEqual(self.binary(self.app), "new")
        self.assertEqual(self.binary(Path(result["stage"])), "old")
        self.assertFalse(Path(result["backup"]).exists())

    def test_interrupted_exchange_recovers_from_pins_before_state_was_saved(self):
        result = self.stage()
        self.tool.save(self.tool.load(result["id"]), "swapping")
        self.runtime.exchange(self.app, Path(result["stage"]))
        self.assertEqual(self.tool.install_when_closed(result["id"])["state"], "installed")
        self.assertEqual(self.binary(self.app), "new")
        self.assertEqual(self.binary(Path(result["backup"])), "old")
        self.assertFalse(Path(result["stage"]).exists())

    def test_backup_move_accepted_before_error_is_recognized(self):
        result = self.stage()
        original = self.runtime.rename_exclusive
        def move_then_fail(source, target):
            original(source, target)
            raise OSError("fixture lost response")
        self.runtime.rename_exclusive = move_then_fail
        self.assertEqual(self.tool.install_when_closed(result["id"])["state"], "installed")
        self.assertEqual(self.binary(self.app), "new")
        self.assertEqual(self.binary(Path(result["backup"])), "old")

    def test_rollback_never_exchanges_an_unknown_target(self):
        result = self.stage()
        def fail_and_replace(source, target):
            (self.app / "Contents/MacOS/kasaterm").write_text("unknown")
            return True
        self.runtime.rename_failure = fail_and_replace
        self.assertEqual(self.tool.install_when_closed(result["id"])["state"], "rollback_required")
        self.assertEqual(self.binary(self.app), "unknown")
        self.assertEqual(self.binary(Path(result["stage"])), "old")

    def test_backup_collision_is_not_overwritten_or_used_for_rollback(self):
        result = self.stage()
        original = self.runtime.rename_exclusive
        def collide(source, target):
            self.bundle(target, "unknown backup")
            original(source, target)
        self.runtime.rename_exclusive = collide
        self.assertEqual(self.tool.install_when_closed(result["id"])["state"], "rollback_required")
        self.assertEqual(self.binary(self.app), "new")
        self.assertEqual(self.binary(Path(result["stage"])), "old")
        self.assertEqual(self.binary(Path(result["backup"])), "unknown backup")

    def test_interrupted_old_move_recovers_and_requires_explicit_new_stage(self):
        result = self.stage()
        os.rename(self.app, result["backup"])
        self.assertEqual(self.tool.install_when_closed(result["id"])["state"], "failed")
        self.assertEqual(self.binary(self.app), "old")
        self.assertEqual(self.binary(Path(result["stage"])), "new")

    def test_interrupted_completed_swap_is_recognized_only_with_both_pins(self):
        result = self.stage()
        os.rename(self.app, result["backup"])
        os.rename(result["stage"], self.app)
        self.assertEqual(self.tool.install_when_closed(result["id"])["state"], "installed")
        self.assertEqual(self.binary(Path(result["backup"])), "old")

    def test_changed_target_or_stage_stops_before_any_moves(self):
        for key in ("app", "stage"):
            with self.subTest(key=key):
                result = self.stage()
                target = Path(result[key]) / "Contents/MacOS/kasaterm"
                before = target.read_bytes()
                target.write_text("other agent build")
                with self.assertRaises(b.Refused):
                    self.tool.install_when_closed(result["id"])
                self.assertEqual(self.runtime.moves, [])
                target.write_bytes(before)
                self.tool.save(self.tool.load(result["id"]), "failed")

    def test_signature_identity_executable_version_and_sha_must_match(self):
        self.runtime.bad_signature.add(str(self.app))
        with self.assertRaises(b.Refused):
            self.stage()
        self.runtime.bad_signature.clear()
        self.runtime.identity = "different-machine"
        with self.assertRaises(b.Refused):
            self.stage()
        self.runtime.identity = "fixture-machine"
        with self.assertRaises(b.Refused):
            self.tool.stage(self.app, self.source, "fixture-machine", "0" * 64, "0.2.2")
        old = self.bundle(self.root / "downgrade.app", "outdated", version="0.2.1", build="0.2.1")
        with self.assertRaises(b.Refused):
            self.stage(source=old)
        wrong = self.bundle(self.root / "wrong.app", "viewer", "kasaviewer.app")
        with self.assertRaises(b.Refused):
            self.stage(source=wrong)

    def test_staged_signature_is_rechecked_after_copy_and_before_install(self):
        result = self.stage()
        self.assertIn(Path(result["stage"]), self.runtime.signature_calls)
        self.runtime.bad_signature.add(result["stage"])
        with self.assertRaises(b.Refused):
            self.tool.install_when_closed(result["id"])
        self.assertEqual(self.runtime.moves, [])

    def test_symlinks_broad_paths_and_protected_state_are_rejected(self):
        link = self.root / "linked.app"
        link.symlink_to(self.source)
        for target in (self.home, Path("/"), self.home / "Applications", self.home / "Applications/Other.app"):
            with self.subTest(target=target), self.assertRaises(b.Refused):
                self.stage(app=target)
        with self.assertRaises(b.Refused):
            self.stage(source=link)
        with self.assertRaises(b.Refused):
            b.Bootstrap(self.home / "Desktop/state", self.runtime)
        (self.source / "Contents/escape").symlink_to(self.settings)
        with self.assertRaises(b.Refused):
            self.stage()

    def test_internal_framework_symlinks_are_preserved(self):
        framework = self.source / "Contents/Frameworks/Example.framework"
        (framework / "Versions/A").mkdir(parents=True)
        (framework / "Versions/Current").symlink_to("A")
        result = self.stage()
        self.assertTrue((Path(result["stage"]) / "Contents/Frameworks/Example.framework/Versions/Current").is_symlink())

    def test_pending_stage_and_tampered_plan_are_rejected(self):
        result = self.stage()
        with self.assertRaises(b.Refused):
            self.stage()
        path = self.tool.record_path(result["id"])
        record = b.read_private(path)
        record["plan"]["backup"] = str(self.home)
        b.atomic_json(path, record)
        with self.assertRaises(b.Refused):
            self.tool.install_when_closed(result["id"])
        self.assertEqual(self.runtime.moves, [])

    def test_malformed_record_never_falls_back_to_a_new_plan(self):
        result = self.stage()
        for malformed in ([], {}, {"plan":[]}):
            with self.subTest(malformed=malformed):
                b.atomic_json(self.tool.record_path(result["id"]), malformed)
                with self.assertRaises(b.Refused):
                    self.tool.install_when_closed(result["id"])
        self.assertEqual(self.runtime.moves, [])

    def test_invalid_or_symlink_settings_never_replace_apps_or_disclose_contents(self):
        result = self.stage()
        self.settings.write_text("not-json never-log-this")
        with self.assertRaises(b.Refused):
            self.tool.install_when_closed(result["id"])
        self.assertEqual(self.runtime.moves, [])
        self.settings.unlink()
        self.settings.symlink_to(self.root / "secret")
        with self.assertRaises(b.Refused):
            self.tool.install_when_closed(result["id"])
        output = io.StringIO()
        with contextlib.redirect_stdout(output), mock.patch.object(b, "Bootstrap", return_value=self.tool):
            self.assertEqual(b.main(["install-when-closed", "--plan", result["id"]]), 2)
        self.assertNotIn("never-log-this", output.getvalue())

    def test_arm_copies_standalone_helper_outside_desktop_and_releases_lock_before_launch(self):
        result = self.stage()
        python = self.root / "python"
        python.write_text("fixture interpreter")
        python.chmod(0o755)
        def launch(plist):
            with self.tool.locked():
                self.runtime.launched.append(plist)
        self.runtime.launch = launch
        armed = self.tool.arm(result["id"], python)
        plist = plistlib.loads(self.runtime.launched[0].read_bytes())
        argv = plist["ProgramArguments"]
        self.assertEqual(plist["Label"], armed["job"])
        self.assertNotIn("StartInterval", plist)
        self.assertNotIn("KeepAlive", plist)
        self.assertTrue(Path(argv[1]).is_relative_to(self.tool.root))
        self.assertEqual(b.file_hash(Path(argv[1])), Path(argv[1]).stem.removeprefix("helper-"))
        self.assertEqual(argv[-2:], ["--wait-seconds", "86400"])
        self.assertNotIn("never-log-this", self.runtime.launched[0].read_text())
        with self.assertRaises(b.Refused):
            self.tool.arm(result["id"], python)

    def test_real_verifier_requests_strict_signature_and_required_team(self):
        runtime = b.Runtime()
        with mock.patch.object(runtime, "command", side_effect=["", "TeamIdentifier=L366799VND\n"]) as command:
            runtime.signature(self.source)
        self.assertEqual(command.call_args_list[0].args[0][1:4], ["--verify", "--deep", "--strict"])
        for details in ("TeamIdentifier=OTHER\n", "TeamIdentifier=L366799VND\nSignature=adhoc"):
            with mock.patch.object(runtime, "command", side_effect=["", details]), self.assertRaises(b.Refused):
                runtime.signature(self.source)

    def test_copy_preserves_os_security_metadata(self):
        runtime = b.Runtime()
        with mock.patch.object(runtime, "command") as command:
            runtime.copy(self.source, self.app)
        self.assertEqual(command.call_args.args[0], ["/usr/bin/ditto", str(self.source), str(self.app)])

    def test_failed_launch_registration_can_be_retried_without_a_new_stage(self):
        result = self.stage()
        python = self.root / "python"
        python.write_text("fixture interpreter")
        python.chmod(0o755)
        with mock.patch.object(self.runtime, "launch", side_effect=b.Refused("fixture launch failure")):
            with self.assertRaises(b.Refused):
                self.tool.arm(result["id"], python)
        self.assertIsNone(self.tool.load(result["id"]).get("job"))
        self.assertEqual(list((self.home / "Library/LaunchAgents").glob("*.plist")), [])
        self.assertEqual(self.tool.arm(result["id"], python)["state"], "staged")

    def test_launch_accepted_then_error_preserves_job_and_prevents_duplicate_arm(self):
        result = self.stage()
        python = self.root / "python"
        python.write_text("fixture interpreter")
        python.chmod(0o755)
        original = self.runtime.launch
        def accepted_then_error(plist):
            original(plist)
            raise b.Refused("fixture response lost")
        with mock.patch.object(self.runtime, "launch", side_effect=accepted_then_error):
            with self.assertRaises(b.Refused):
                self.tool.arm(result["id"], python)
        status = self.tool.summary(self.tool.load(result["id"]))
        self.assertEqual(status["registration"], "registered")
        self.assertTrue((self.home / "Library/LaunchAgents" / (status["job"] + ".plist")).exists())
        with self.assertRaises(b.Refused):
            self.tool.arm(result["id"], python)
        self.assertEqual(len(self.runtime.launched), 1)

    def test_unknown_registration_preserves_plist_job_and_blocks_rearming(self):
        result = self.stage()
        python = self.root / "python"
        python.write_text("fixture interpreter")
        python.chmod(0o755)
        with mock.patch.object(self.runtime, "launch", side_effect=b.Refused("fixture launch error")), \
                mock.patch.object(self.runtime, "job_registered", return_value=None):
            with self.assertRaises(b.Refused):
                self.tool.arm(result["id"], python)
        status = self.tool.summary(self.tool.load(result["id"]))
        self.assertEqual(status["registration"], "uncertain")
        self.assertTrue((self.home / "Library/LaunchAgents" / (status["job"] + ".plist")).exists())
        with self.assertRaises(b.Refused):
            self.tool.arm(result["id"], python)

    def test_launch_error_cannot_forget_helper_that_already_completed(self):
        result = self.stage()
        python = self.root / "python"
        python.write_text("fixture interpreter")
        python.chmod(0o755)
        def install_then_error(plist):
            self.assertEqual(self.tool.install_when_closed(result["id"])["state"], "installed")
            raise b.Refused("fixture response lost after helper exited")
        with mock.patch.object(self.runtime, "launch", side_effect=install_then_error):
            with self.assertRaises(b.Refused):
                self.tool.arm(result["id"], python)
        status = self.tool.summary(self.tool.load(result["id"]))
        self.assertEqual(status["state"], "installed")
        self.assertEqual(status["registration"], "uncertain")
        self.assertTrue((self.home / "Library/LaunchAgents" / (status["job"] + ".plist")).exists())
        self.assertEqual(self.binary(self.app), "new")
        with self.assertRaises(b.Refused):
            self.tool.arm(result["id"], python)

    def test_launchctl_probe_only_accepts_exact_missing_service_diagnostic(self):
        runtime = b.Runtime()
        label = "com.kasaterm.bootstrap." + "a" * 24
        missing = f'Bad request.\nCould not find service "{label}" in domain for user gui: {os.getuid()}\n'
        cases = [(0, "service exists", "", True), (113, "", missing, False),
                 (113, "", "Could not find domain", None), (1, "", missing, None),
                 (113, "", missing.replace(label, "another.job"), None)]
        for code, stdout, stderr, expected in cases:
            with self.subTest(code=code, stderr=stderr), mock.patch.object(b.subprocess, "run",
                    return_value=SimpleNamespace(returncode=code, stdout=stdout, stderr=stderr)) as run:
                self.assertIs(runtime.job_registered(label), expected)
                self.assertEqual(run.call_args.args[0], ["/bin/launchctl", "print", f"gui/{os.getuid()}/{label}"])
        with mock.patch.object(b.subprocess, "run", side_effect=b.subprocess.TimeoutExpired("launchctl", 30)):
            self.assertIsNone(runtime.job_registered(label))

    @unittest.skipUnless(sys.platform == "darwin", "macOS atomic rename API")
    def test_real_atomic_directory_exchange_and_exclusive_move_in_temporary_directory(self):
        runtime = b.Runtime()
        first = self.root / "atomic-first"
        second = self.root / "atomic-second"
        backup = self.root / "atomic-backup"
        first.mkdir()
        second.mkdir()
        (first / "old").write_text("old")
        (second / "new").write_text("new")
        runtime.exchange(first, second)
        self.assertTrue((first / "new").exists())
        self.assertTrue((second / "old").exists())
        with self.assertRaises(FileExistsError):
            runtime.rename_exclusive(second, first)
        self.assertTrue((first / "new").exists())
        self.assertTrue((second / "old").exists())
        runtime.rename_exclusive(second, backup)
        self.assertTrue((backup / "old").exists())
        self.assertFalse(second.exists())

    def test_event_wait_uses_process_exit_notifications_and_fails_closed_on_permission_error(self):
        runtime = b.Runtime()
        queue = mock.MagicMock()
        queue.__enter__.return_value = queue
        queue.control.return_value = []
        values = {"kqueue":mock.Mock(return_value=queue), "kevent":mock.Mock(side_effect=lambda *a, **k:(a, k)),
                  "KQ_FILTER_PROC":1, "KQ_EV_ADD":2, "KQ_EV_ENABLE":4, "KQ_EV_ONESHOT":8,
                  "KQ_NOTE_EXIT":16, "KQ_EV_ERROR":32}
        with mock.patch.multiple(b.select, create=True, **values):
            runtime.wait_for_exit({22:"fixture"}, 60)
            event = queue.control.call_args.args[0][0]
            self.assertEqual(event[0], (22,))
            self.assertEqual(event[1]["fflags"], 16)
            self.assertEqual(queue.control.call_args.args[-1], 60)
            queue.control.return_value = [SimpleNamespace(flags=32, data=b.errno.EPERM)]
            with self.assertRaises(b.Refused):
                runtime.wait_for_exit({22:"fixture"}, 60)

    def test_lock_serializes_helpers_without_a_spin_poll(self):
        entered = threading.Event()
        finished = threading.Event()
        def other_helper():
            entered.set()
            with self.tool.locked():
                finished.set()
        with self.tool.locked():
            thread = threading.Thread(target=other_helper)
            thread.start()
            self.assertTrue(entered.wait(2))
            self.assertFalse(finished.wait(0.02))
        thread.join(2)
        self.assertTrue(finished.is_set())

    def test_unavailable_process_inventory_and_wait_expiry_do_not_move_apps(self):
        result = self.stage()
        with mock.patch.object(self.runtime, "processes", side_effect=b.Refused("process inventory unavailable")):
            with self.assertRaises(b.Refused):
                self.tool.install_when_closed(result["id"])
        self.runtime.live = {41:str(self.app / "Contents/MacOS/kasaterm")}
        with mock.patch.object(b.time, "monotonic", side_effect=[0, 2]):
            self.assertEqual(self.tool.wait(result["id"], 1)["state"], "expired")
        self.assertEqual(self.runtime.moves, [])
        self.assertEqual(json.loads(self.settings.read_text()), self.original)

    def test_reopen_after_exchange_preserves_both_bundles_and_waits_again(self):
        result = self.stage()
        original = self.runtime.exchange
        def exchange_then_reopen(source, target):
            original(source, target)
            self.runtime.live = {99:str(self.app / "Contents/MacOS/kasaterm")}
        self.runtime.exchange = exchange_then_reopen
        self.assertEqual(self.tool.install_when_closed(result["id"])["state"], "waiting")
        self.assertEqual(self.binary(self.app), "new")
        self.assertEqual(self.binary(Path(result["stage"])), "old")
        self.assertFalse(Path(result["backup"]).exists())
        self.assertEqual(self.tool.wait(result["id"], 60)["state"], "installed")

    def test_following_upgrade_preserves_the_first_backup(self):
        first = self.stage()
        self.tool.install_when_closed(first["id"])
        later = self.bundle(self.root / "later.app", "later", version="0.2.3", build="0.2.3")
        second = self.stage(source=later)
        self.tool.install_when_closed(second["id"])
        self.assertEqual(self.binary(self.app), "later")
        self.assertEqual(self.binary(Path(first["backup"])), "old")
        self.assertEqual(self.binary(Path(second["backup"])), "new")


if __name__ == "__main__":
    unittest.main()
