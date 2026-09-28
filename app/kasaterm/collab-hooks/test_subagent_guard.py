"""서브에이전트 가드 회귀 시험 — pane 안의 범용 서브에이전트만 막고 나머지는 통과시킨다."""
import json
import os
from pathlib import Path
import subprocess
import unittest

SCRIPT = Path(__file__).with_name("kasaterm-subagent-guard.py")


def run(payload, pane="%3"):
    env = {k: v for k, v in os.environ.items() if k != "KASATERM_PANE_ID"}
    if pane:
        env["KASATERM_PANE_ID"] = pane
    out = subprocess.run(["python3", str(SCRIPT)], input=json.dumps(payload),
                         capture_output=True, text=True, env=env, timeout=10).stdout
    return json.loads(out)["hookSpecificOutput"]["permissionDecision"] if out.strip() else "pass"


def agent(**tool_input):
    return {"tool_name": "Agent", "tool_input": {"description": "고치기", "prompt": "…", **tool_input}}


class SubagentGuardTests(unittest.TestCase):
    def test_general_purpose_and_forks_are_denied_inside_a_pane(self):
        self.assertEqual(run(agent(subagent_type="general-purpose")), "deny")
        self.assertEqual(run(agent()), "deny")
        self.assertEqual(run(agent(subagent_type="fork")), "deny")
        self.assertEqual(run({"tool_name": "Task", "tool_input": {"prompt": "…"}}), "deny")

    def test_reason_points_to_summon(self):
        env = dict(os.environ, KASATERM_PANE_ID="%3")
        out = subprocess.run(["python3", str(SCRIPT)], input=json.dumps(agent()),
                             capture_output=True, text=True, env=env, timeout=10).stdout
        self.assertIn("kasaterm-cli summon", json.loads(out)["hookSpecificOutput"]["permissionDecisionReason"])

    def test_read_only_helpers_explicit_bypass_and_outside_pane_pass(self):
        for kind in ("Explore", "Plan", "claude-code-guide", "statusline-setup"):
            self.assertEqual(run(agent(subagent_type=kind)), "pass", kind)
        self.assertEqual(run(agent(description="사람이 청한 조사 [subagent]")), "pass")
        self.assertEqual(run(agent(), pane=""), "pass")
        self.assertEqual(run({"tool_name": "Bash", "tool_input": {"command": "ls"}}), "pass")

    def test_unreadable_input_passes(self):
        env = dict(os.environ, KASATERM_PANE_ID="%3")
        out = subprocess.run(["python3", str(SCRIPT)], input="not json",
                             capture_output=True, text=True, env=env, timeout=10)
        self.assertEqual((out.returncode, out.stdout.strip()), (0, ""))


if __name__ == "__main__":
    unittest.main()
