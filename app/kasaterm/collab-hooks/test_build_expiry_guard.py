"""빌드 만료 가드 회귀 시험 — App Store Connect 빌드 만료 요청만 막고 나머지는 통과시킨다."""
import json
import os
from pathlib import Path
import subprocess
import unittest

SCRIPT = Path(__file__).with_name("kasaterm-build-expiry-guard.py")


def run(command, pane="%3", tool="Bash"):
    env = {k: v for k, v in os.environ.items() if k != "KASATERM_PANE_ID"}
    if pane:
        env["KASATERM_PANE_ID"] = pane
    payload = {"tool_name": tool, "tool_input": {"command": command}}
    out = subprocess.run(["python3", str(SCRIPT)], input=json.dumps(payload),
                         capture_output=True, text=True, env=env, timeout=10).stdout
    return json.loads(out)["hookSpecificOutput"]["permissionDecision"] if out.strip() else "pass"


class BuildExpiryGuardTests(unittest.TestCase):
    def test_expiry_requests_are_denied(self):
        node = ("node -e \"await asc('PATCH', '/v1/builds/'+id, { data: { type: 'builds', id, "
                "attributes: { expired: true } } })\"")
        self.assertEqual(run(node), "deny")
        py = "python3 - <<'X'\nasc.call('PATCH', f'/builds/{b}', {'data': {'attributes': {'expired': True}}})\nX\ncd mobile && tool/asc.py"
        self.assertEqual(run(py), "deny")
        curl = "curl -X PATCH https://api.appstoreconnect.apple.com/v1/builds/abc -d '{\"data\":{\"attributes\":{\"expired\":true}}}'"
        self.assertEqual(run(curl), "deny")
        self.assertEqual(run("fastlane pilot expire_all_builds"), "deny")

    def test_everything_else_passes(self):
        self.assertEqual(run("cd mobile && tool/testflight.sh"), "pass", "올리기는 막지 않는다")
        self.assertEqual(run("python3 tool/asc.py builds 6809705414"), "pass", "조회는 막지 않는다")
        self.assertEqual(run("echo '{\"expired\": true}' | jq ."), "pass", "빌드와 무관한 글")
        self.assertEqual(run("curl -X PATCH https://api.appstoreconnect.apple.com/v1/builds/abc -d '{\"expired\":true}'", pane=""), "pass", "pane 밖")
        self.assertEqual(run("expired: true /v1/builds", tool="Edit"), "pass", "Bash 만 본다")


if __name__ == "__main__":
    unittest.main()
