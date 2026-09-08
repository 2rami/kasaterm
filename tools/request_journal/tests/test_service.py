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
from unittest.mock import patch

from tools.request_journal import service


class LifecycleTests(unittest.TestCase):
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


if __name__ == "__main__":
    unittest.main()
