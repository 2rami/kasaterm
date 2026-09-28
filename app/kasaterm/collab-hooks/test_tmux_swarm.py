"""Claude Code 팀원 tmux 호출이 kasaterm pane 명령으로 옮겨지는지. 진짜 앱 없이 가짜 CLI 로."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("kasaterm-tmux-swarm.py")
SOCK = "claude-swarm-4242"

FAKE_CLI = r'''#!/usr/bin/env python3
import json, os, sys
state_path = os.environ["FAKE_STATE"]
st = json.load(open(state_path))
args = sys.argv[1:]
st["calls"].append(args)
def reply(result=None, error=None):
    json.dump(st, open(state_path, "w"))
    if error:
        print(json.dumps({"id": "x", "ok": False, "error": {"message": error}}))
        sys.exit(1)
    print(json.dumps({"id": "x", "ok": True, "result": result or {}}))
    sys.exit(0)
cmd = args[0]
if cmd == "list":
    reply({"surfaces": [{"id": k, "character": v} for k, v in st["surfaces"].items()]})
if cmd in ("split", "tab"):
    if cmd == "split" and st.get("split_fails"):
        reply(error="no room")
    pid = "%" + str(st["next"])
    st["next"] += 1
    st["surfaces"][pid] = "student" + pid
    reply({"surface": {"id": pid, "character": "student" + pid}})
if cmd == "close":
    st["surfaces"].pop(args[1], None)
reply({"ok": True})
'''


class SwarmTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.shim = Path(self.tmp.name)
        cli = self.shim / "kasaterm-cli"
        cli.write_text(FAKE_CLI)
        cli.chmod(0o755)
        self.state = self.shim / "fake.json"
        self.write_state({"surfaces": {"%2": "leader"}, "next": 10, "calls": []})

    def tearDown(self):
        self.tmp.cleanup()

    def write_state(self, st):
        self.state.write_text(json.dumps(st))

    def read_state(self):
        return json.loads(self.state.read_text())

    def tmux(self, *args):
        env = dict(os.environ, KASATERM_TMUX_SHIM_DIR=str(self.shim), KASATERM_PANE_ID="%2",
                   FAKE_STATE=str(self.state))
        out = subprocess.run([sys.executable, str(SCRIPT), "-L", SOCK, *args],
                             capture_output=True, text=True, env=env, timeout=30)
        return out.returncode, out.stdout.strip(), out.stderr.strip()

    def calls(self, name):
        return [c for c in self.read_state()["calls"] if c[0] == name]

    def spawn_first(self):
        self.assertEqual(self.tmux("has-session", "-t", "claude-swarm")[0], 1)
        code, out, _ = self.tmux("new-session", "-d", "-s", "claude-swarm", "-n", "swarm-view",
                                 "-P", "-F", "#{pane_id}", "--", "cat")
        self.assertEqual((code, out), (0, "%10"))
        return out

    def test_claude_external_sequence_lands_beside_the_leader(self):
        pane = self.spawn_first()
        self.assertEqual(self.calls("split"), [["split", "auto", "%2"]])
        self.assertEqual(self.tmux("list-panes", "-t", "claude-swarm:swarm-view", "-F", "#{pane_id}")[1], pane)
        for args in (["set-option", "-w", "-t", "claude-swarm:swarm-view", "pane-border-status", "top"],
                     ["set-option", "-p", "-t", pane, "window-style", "bg=default,fg=blue"],
                     ["select-layout", "-t", "claude-swarm:swarm-view", "tiled"],
                     ["set-option", "-p", "-t", pane, "remain-on-exit", "failed"]):
            self.assertEqual(self.tmux(*args)[0], 0, args)
        self.assertEqual(self.tmux("select-pane", "-t", pane, "-T", "explore-gateway")[0], 0)
        self.assertEqual(self.calls("rename"), [["rename", pane, "explore-gateway"]])

        self.assertEqual(self.tmux("has-session", "-t", "claude-swarm")[0], 0)
        self.assertEqual(self.tmux("list-windows", "-t", "claude-swarm", "-F", "#{window_name}")[1], "swarm-view")
        code, second, _ = self.tmux("split-window", "-d", "-t", pane, "-v", "-P", "-F", "#{pane_id}", "--", "cat")
        self.assertEqual((code, second), (0, "%11"))
        self.assertEqual(self.calls("split")[-1], ["split", "down", pane])

    def test_teammate_command_goes_through_a_self_deleting_file_not_the_prompt(self):
        pane = self.spawn_first()
        command = "cd '/repo' && env CLAUDECODE=1 ANTHROPIC_API_KEY='sk-secret' '/bin/claude' --agent-id a@t"
        self.assertEqual(self.tmux("respawn-pane", "-k", "-t", pane, "--", command)[0], 0)
        (send,) = self.calls("send")
        self.assertEqual(send[:3], ["send", "--surface", pane])
        self.assertNotIn("sk-secret", send[3])
        self.assertTrue(send[3].endswith(" && exit\n"))
        script = Path(send[3].split()[1])
        body = script.read_text()
        self.assertIn(command, body)
        self.assertTrue(body.startswith('rm -f -- "$0"'))
        self.assertEqual(oct(script.stat().st_mode & 0o777), "0o600")
        subprocess.run(["sh", str(script)], capture_output=True)
        self.assertFalse(script.exists())

    def test_kill_pane_closes_for_real_and_forgets_it(self):
        pane = self.spawn_first()
        self.assertEqual(self.tmux("kill-pane", "-t", pane)[0], 0)
        self.assertEqual(self.calls("close"), [["close", pane]])
        self.assertEqual(self.calls("closed"), [["closed", pane]])
        self.assertEqual(self.tmux("has-session", "-t", "claude-swarm")[0], 1)

    def swarm_file(self, suffix):
        return self.shim / "swarm" / f"{SOCK}{suffix}"

    def test_a_reused_pane_number_is_not_ours(self):
        pane = self.spawn_first()
        dead = subprocess.Popen(["true"])
        dead.wait()
        self.swarm_file(f".{pane[1:]}.shell").write_text(str(dead.pid))
        self.assertNotEqual(self.tmux("kill-pane", "-t", pane)[0], 0)
        self.assertNotEqual(self.tmux("respawn-pane", "-k", "-t", pane, "--", "echo hi")[0], 0)
        self.assertEqual(self.calls("close"), [])
        self.assertEqual(self.calls("send"), [])

    def test_a_pane_that_never_got_its_command_goes_stale(self):
        pane = self.spawn_first()
        state = json.loads(self.swarm_file(".json").read_text())
        state["panes"][0]["created"] = 0
        self.swarm_file(".json").write_text(json.dumps(state))
        self.assertNotEqual(self.tmux("kill-pane", "-t", pane)[0], 0)
        self.assertEqual(self.tmux("has-session", "-t", "claude-swarm")[0], 1)

    def test_a_running_teammate_shell_keeps_the_pane_ours(self):
        pane = self.spawn_first()
        self.assertEqual(self.tmux("respawn-pane", "-k", "-t", pane, "--", "true")[0], 0)
        script = self.calls("send")[0][3].split()[1]
        subprocess.run(["sh", script], check=True)
        self.assertEqual(self.swarm_file(f".{pane[1:]}.shell").read_text().strip(), str(os.getpid()))
        state = json.loads(self.swarm_file(".json").read_text())
        state["panes"][0]["created"] = 0
        self.swarm_file(".json").write_text(json.dumps(state))
        self.assertEqual(self.tmux("kill-pane", "-t", pane)[0], 0)
        self.assertEqual(self.calls("close"), [["close", pane]])
        self.assertFalse(self.swarm_file(f".{pane[1:]}.shell").exists())

    def test_never_touches_the_leader_or_other_panes(self):
        self.spawn_first()
        self.assertNotEqual(self.tmux("respawn-pane", "-k", "-t", "%2", "--", "echo hi")[0], 0)
        self.assertNotEqual(self.tmux("split-window", "-d", "-t", "%2", "-h", "-P", "-F", "#{pane_id}")[0], 0)
        self.assertNotEqual(self.tmux("kill-pane", "-t", "%2")[0], 0)
        self.assertEqual(self.calls("send") + self.calls("close"), [])

    def test_no_room_to_split_falls_back_to_a_tab(self):
        st = self.read_state()
        st["split_fails"] = True
        self.write_state(st)
        code, out, _ = self.tmux("new-session", "-d", "-s", "claude-swarm", "-n", "swarm-view",
                                 "-P", "-F", "#{pane_id}", "--", "cat")
        self.assertEqual((code, out), (0, "%10"))
        self.assertEqual(self.calls("tab"), [["tab", "%2"]])

    def test_attach_focuses_the_teammates(self):
        pane = self.spawn_first()
        code, out, _ = self.tmux("a")
        self.assertEqual(code, 0)
        self.assertIn(pane, out)
        self.assertEqual(self.calls("focus"), [["focus", pane]])

    def test_unknown_command_fails_loudly(self):
        code, _, err = self.tmux("pipe-pane", "-t", "%10")
        self.assertNotEqual(code, 0)
        self.assertIn("pipe-pane", err)


if __name__ == "__main__":
    unittest.main()
