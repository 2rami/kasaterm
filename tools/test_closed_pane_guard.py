import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

HOOK = Path(__file__).resolve().parents[1] / "app/kasaterm/collab-hooks/kasaterm-closed-pane-guard.py"
spec = importlib.util.spec_from_file_location("closed_guard", HOOK)
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)


class ClosedPaneGuardTests(unittest.TestCase):
    def invoke(self, payload, env, rows=()):
        output = io.StringIO()
        with patch.dict(os.environ, env), patch.object(guard, "board_rows", return_value=rows), \
                patch("sys.stdin", io.StringIO(json.dumps(payload))), contextlib.redirect_stdout(output):
            guard.main()
        return json.loads(output.getvalue()) if output.getvalue() else None

    def test_closed_receiver_stops_tools_and_incoming_prompts(self):
        with tempfile.TemporaryDirectory() as directory:
            socket = str(Path(directory) / "socket")
            marker = Path(socket + ".closing") / "7"
            marker.parent.mkdir()
            marker.touch()
            env = {"KASATERM_PANE_ID": "%7", "KASATERM_SOCKET_PATH": socket}
            for payload in ({"tool_name": "Bash"}, {"hook_event_name": "UserPromptSubmit"}):
                self.assertFalse(self.invoke(payload, env)["continue"])
            marker.unlink()
            self.assertIsNone(self.invoke({"tool_name": "Bash"}, env))

    def test_closed_peer_session_name_is_not_a_sendmessage_escape(self):
        env = {"KASATERM_PANE_ID": "%1", "KASATERM_SOCKET_PATH": "/nonexistent/test-socket"}
        result = self.invoke({"tool_name": "SendMessage", "tool_input": {"to": "task-name"}}, env,
                             [{"peer_name": "task-name", "surface_id": "%7", "detached": True}])
        self.assertEqual(result["hookSpecificOutput"]["permissionDecision"], "deny")


if __name__ == "__main__":
    unittest.main()
