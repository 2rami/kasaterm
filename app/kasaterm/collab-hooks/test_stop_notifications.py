import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


class StopNotificationTests(unittest.TestCase):
    def run_hook(self, payload, pane="%fixture"):
        with tempfile.TemporaryDirectory(prefix="kasa-stop-test-") as root:
            root = Path(root)
            hooks = Path(__file__).resolve().parent
            for name in ("kasaterm-stop-drain.sh", "kasaterm-turn.sh"):
                shutil.copy2(hooks / name, root / name)
            cli = root / "kasaterm-cli"
            cli.write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "$KASA_TEST_LOG"\n', encoding="utf-8")
            cli.chmod(0o755)
            log = root / "calls.txt"
            env = dict(os.environ, PATH=str(root) + os.pathsep + os.environ["PATH"],
                       KASATERM_PANE_ID=pane, KASA_TEST_LOG=str(log))
            result = subprocess.run(["bash", str(root / "kasaterm-stop-drain.sh")],
                                    input=json.dumps(payload), text=True, capture_output=True,
                                    env=env, timeout=5)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout, "")
            return log.read_text(encoding="utf-8").splitlines() if log.exists() else []

    def test_stop_is_a_boundary_not_an_unconditional_completion_message(self):
        self.assertEqual(self.run_hook({}), ["turn end"])

    def test_permission_mode_stays_attached_to_the_boundary(self):
        self.assertEqual(self.run_hook({"permission_mode": "default"}),
                         ["turn end --permission-mode default"])

    def test_reentrant_stop_and_non_pane_execution_are_silent(self):
        self.assertEqual(self.run_hook({"stop_hook_active": True}), [])
        self.assertEqual(self.run_hook({}, pane=""), [])


if __name__ == "__main__":
    unittest.main()
