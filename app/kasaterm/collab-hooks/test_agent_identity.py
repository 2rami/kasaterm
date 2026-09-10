"""Regression tests for pre-exec identity delivery. No real harness or user files."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib.parse import parse_qs, urlparse

SCRIPT = Path(__file__).with_name("kasaterm-agent-identity.py")
spec = importlib.util.spec_from_file_location("identity", SCRIPT)
identity = importlib.util.module_from_spec(spec)
spec.loader.exec_module(identity)
SID = "01234567-1234-1234-1234-123456789abc"


class IdentityTests(unittest.TestCase):
    def test_resume_parser_uses_selected_session_not_spawn_anchor(self):
        self.assertEqual(identity.session_id("codex", ["-m", "model", "resume", SID], "old"), SID)
        self.assertEqual(identity.session_id("codex", ["exec", "resume", SID], "old"), SID)
        self.assertEqual(identity.session_id("codex", ["resume", "--last"], "old"), "")
        self.assertEqual(identity.session_id("codex", [], "old"), "")
        self.assertEqual(identity.session_id("claude", ["--resume", SID], "old"), SID)
        self.assertEqual(identity.session_id("claude", ["-r", SID], "old"), SID)
        self.assertEqual(identity.session_id("claude", [], "new-anchor"), "new-anchor")

    def test_both_harnesses_take_one_fresh_snapshot_and_respect_the_pane_port(self):
        queries = []
        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                queries.append(parse_qs(urlparse(self.path).query))
                self.send_response(200)
                self.end_headers()
                self.wfile.write(json.dumps({"character": "코하루", "persona": "너는 코하루.", "slug": "koharu"}).encode())
            def log_message(self, *args):
                pass
        server = HTTPServer(("127.0.0.1", 0), Handler)
        worker = threading.Thread(target=server.serve_forever, daemon=True)
        worker.start()
        try:
            with tempfile.TemporaryDirectory(prefix="kasaterm-identity-test-") as tmp:
                root = Path(tmp)
                (root / "socket.mcp_port").write_text(str(server.server_port))
                env = dict(os.environ, KASATERM_PANE_ID="%test", KASATERM_SOCKET_PATH=str(root / "socket.sock"),
                           KASASPACE_MCP_PORT="1", KASATERM_CHARACTER="모모이", KASATERM_PERSONA="너는 모모이.")
                for harness, args in [("codex", ["resume", SID]), ("claude", ["--resume", SID])]:
                    result = subprocess.run(["python3", str(SCRIPT), harness, str(root), "old", *args],
                                            env=env, capture_output=True, text=True, check=True)
                    target = Path(result.stdout.strip())
                    self.assertEqual(target.parent, root)
                    self.assertEqual((target / "character").read_text(), "코하루")
                    self.assertEqual((target / "persona").read_text(), "너는 코하루.")
                    self.assertEqual(queries[-1]["sid"], [SID])
                self.assertEqual(len(queries), 2, "one request per launch, not separate name/persona reads")
                fresh_sids = []
                for _ in range(2):
                    result = subprocess.run(["python3", str(SCRIPT), "claude", str(root), "stale-pane-anchor"],
                                            env=env, capture_output=True, text=True, check=True)
                    target = Path(result.stdout.strip())
                    fresh_sids.append((target / "session_id").read_text())
                    self.assertEqual(queries[-1]["sid"], [fresh_sids[-1]])
                    self.assertNotEqual(fresh_sids[-1], "stale-pane-anchor")
                self.assertNotEqual(*fresh_sids, "two fresh runs in one pane need distinct conversation IDs")
        finally:
            server.shutdown()
            worker.join()
            server.server_close()

    def test_unreachable_identity_does_not_launch_with_stale_persona(self):
        with tempfile.TemporaryDirectory(prefix="kasaterm-identity-fail-") as tmp:
            env = dict(os.environ, KASATERM_PANE_ID="%test", KASATERM_SOCKET_PATH=str(Path(tmp) / "missing.sock"),
                       KASASPACE_MCP_PORT="0", KASATERM_PERSONA="너는 모모이.")
            result = subprocess.run(["python3", str(SCRIPT), "codex", tmp, "", "resume", SID],
                                    env=env, capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(result.stdout, "")
            self.assertIn("실행을 멈췄어요", result.stderr)


if __name__ == "__main__":
    unittest.main()
