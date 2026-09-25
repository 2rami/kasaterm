import contextlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location(
    "kasaterm_statusline", Path(__file__).parents[1] / "collab-hooks/statusline.py"
)
statusline = importlib.util.module_from_spec(spec)
spec.loader.exec_module(statusline)


class StatuslinePreferencesTest(unittest.TestCase):
    def test_hidden_fields_preserve_report_and_pane_marker(self):
        payload = {"cwd": "/tmp/hidden-directory", "session_id": "fixture", "model": {"id": "claude-fixture", "display_name": "FixtureModel"}, "context_window": {"used_percentage": 23}}
        with tempfile.TemporaryDirectory() as directory:
            settings = Path(directory) / "settings.json"
            settings.write_text(json.dumps({f"agent_statusline_{field}": False for field in ("model", "usage", "cwd")}))
            output = io.StringIO()
            with patch.dict(statusline.os.environ, {"KASATERM_SETTINGS_FILE": str(settings), "KASATERM_PANE_ID": "1", "KASATERM_CHARACTER": "Fixture", "KASATERM_SESSION_ID": "fixture"}), patch.object(statusline.sys, "stdin", io.StringIO(json.dumps(payload))), patch.object(statusline, "report_cwd_to_kasaterm") as report, patch.object(statusline, "get_git_branch", return_value=""), contextlib.redirect_stdout(output):
                statusline.main()
            self.assertEqual(output.getvalue(), statusline.SPRITE + "\n")
            self.assertEqual(report.call_count, 1)
            self.assertEqual(report.call_args.args[:2], ("/tmp/hidden-directory", "fixture"))
            self.assertEqual(report.call_args.args[4], "claude-fixture")

    def test_missing_or_invalid_preferences_keep_legacy_fields(self):
        with tempfile.TemporaryDirectory() as directory:
            settings = Path(directory) / "settings.json"
            with patch.dict(statusline.os.environ, {"KASATERM_SETTINGS_FILE": str(settings)}):
                self.assertTrue(all(statusline.statusline_fields().values()))
                settings.write_text("invalid")
                self.assertTrue(all(statusline.statusline_fields().values()))
                settings.write_text(json.dumps({"agent_statusline_model": False, "agent_statusline_usage": "false"}))
                self.assertEqual(statusline.statusline_fields(), {"model": False, "usage": True, "cwd": True})


if __name__ == "__main__":
    unittest.main()
