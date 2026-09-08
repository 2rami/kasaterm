import argparse
import contextlib
import io
from pathlib import Path
import tempfile
import unittest
import json
import subprocess
import sys
import time
import threading
from unittest.mock import patch

from tools.request_journal import service


class LifecycleTests(unittest.TestCase):
    def test_bootstrap_retries_transient_unload_race(self):
        results = [subprocess.CompletedProcess([], 5), subprocess.CompletedProcess([], 0)]
        with patch.object(service.subprocess, "run", side_effect=results) as run, patch.object(service.time, "sleep") as sleep:
            self.assertEqual(service.bootstrap(Path("/tmp/journal.plist")).returncode, 0)
            self.assertEqual(run.call_count, 2)
            sleep.assert_called_once_with(.2)

    def test_explicit_http_provider_does_not_require_direct_llm_mode(self):
        args = argparse.Namespace(llm=False, nacho_http="http://127.0.0.1:18795", nacho_repo=None)
        stopped = threading.Event()
        with patch("tools.request_journal.nacho.HTTPNachoProvider") as http, patch("tools.request_journal.nacho.NachoProvider.from_environment") as direct:
            self.assertIs(service.summary_provider(args, stopped), http.return_value)
            http.assert_called_once_with(base_url=args.nacho_http, cancel_event=stopped)
            direct.assert_not_called()
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
            service.main(["run", "--llm", "--nacho-http", args.nacho_http])
        self.assertEqual(error.exception.code, 2)

    def test_run_publishes_private_discovery_and_stops_cleanly(self):
        with tempfile.TemporaryDirectory() as root:
            command = [sys.executable, "-m", "tools.request_journal", "run", "--project", root, "--data-dir", root, "--port", "0"]
            process = subprocess.Popen(command, cwd=service.ROOT, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            try:
                discovery = Path(root) / "service.json"
                for _ in range(60):
                    if discovery.exists():
                        break
                    self.assertIsNone(process.poll(), "service exited before publishing discovery")
                    time.sleep(.05)
                data = json.loads(discovery.read_text())
                self.assertTrue(data["base_url"].startswith("http://127.0.0.1:"))
                self.assertGreater(int(data["base_url"].rsplit(":", 1)[1]), 0)
                self.assertEqual(discovery.stat().st_mode & 0o777, 0o600)
                process.terminate()
                self.assertEqual(process.wait(timeout=4), 0)
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait()

    def test_launch_agent_is_project_scoped_and_has_no_secret_environment(self):
        spec = service.launchd_spec("/tmp/project", "/tmp/journal", 19001, 4, llm=True, nacho_repo="/tmp/nacho")
        args = spec["ProgramArguments"]
        self.assertEqual(spec["Label"], "com.kasaterm.request-journal")
        self.assertEqual(args[args.index("--project") + 1], str(Path("/tmp/project").resolve()))
        self.assertIn("--collect", args)
        self.assertIn("--llm", args)
        self.assertNotIn("EnvironmentVariables", spec)
        self.assertEqual(spec["KeepAlive"], {"SuccessfulExit": False})

    def test_install_preview_and_stop_preview_do_not_mutate(self):
        with tempfile.TemporaryDirectory() as root:
            args = argparse.Namespace(project=root, data_dir=Path(root) / "data", port=19001, interval=5, apply=False, base_url="http://127.0.0.1:8765", llm=False, nacho_repo=None)
            with patch.object(Path, "home", return_value=Path(root)), patch.object(service.subprocess, "run") as run, contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(service.install(args), 0)
                self.assertEqual(service.stop(args), 0)
                run.assert_not_called()
            self.assertEqual(list(Path(root).iterdir()), [])

    def test_install_precreates_private_launchd_log(self):
        with tempfile.TemporaryDirectory() as root:
            args = argparse.Namespace(project=root, data_dir=Path(root) / "data", port=0, interval=5, apply=True, base_url="http://127.0.0.1:8765", llm=False, nacho_repo=None)
            result = subprocess.CompletedProcess([], 0)
            with patch.object(Path, "home", return_value=Path(root)), patch.object(service.sys, "platform", "darwin"), patch.object(service.subprocess, "run", return_value=result), contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(service.install(args), 0)
            self.assertEqual((args.data_dir / "service.log").stat().st_mode & 0o777, 0o600)
            self.assertEqual((Path(root) / "Library/LaunchAgents/com.kasaterm.request-journal.plist").stat().st_mode & 0o777, 0o600)

    def test_replace_only_reloads_matching_journal_and_preserves_data(self):
        with tempfile.TemporaryDirectory() as root:
            args = argparse.Namespace(project=root, data_dir=Path(root) / "data", port=0, interval=5, apply=True, base_url="http://127.0.0.1:8765", llm=False, nacho_repo=None)
            result = subprocess.CompletedProcess([], 0)
            with patch.object(Path, "home", return_value=Path(root)), patch.object(service.sys, "platform", "darwin"), patch.object(service.subprocess, "run", return_value=result) as run, contextlib.redirect_stdout(io.StringIO()):
                service.install(args)
                preserved = args.data_dir / "journal.sqlite3"
                preserved.write_bytes(b"preserved journal")
                args.replace = True
                args.nacho_http = "http://127.0.0.1:18795"
                run.reset_mock()
                self.assertEqual(service.install(args), 0)
                self.assertEqual([call.args[0][1] for call in run.call_args_list], ["bootout", "bootstrap"])
                self.assertEqual(preserved.read_bytes(), b"preserved journal")
                args.project = "/another-project"
                run.reset_mock()
                with self.assertRaisesRegex(RuntimeError, "another installation"):
                    service.install(args)
                run.assert_not_called()


if __name__ == "__main__":
    unittest.main()
