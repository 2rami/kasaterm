"""잡기(커밋할 때까지 파일 독점) 회귀 시험. 임시 레포·임시 협업 뿌리·가짜 보드만 쓴다."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import unittest

SCRIPT = Path(__file__).with_name("kasaterm-conflict-guard.py")
A = "aaaaaaaa-0000-0000-0000-000000000001"
B = "bbbbbbbb-0000-0000-0000-000000000002"


class ClaimTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        root = Path(self.tmp.name)
        self.repo = root / "repo"
        self.repo.mkdir()
        self.file = self.repo / "a.rs"
        self.file.write_text("one\n")
        self.git("init", "-q")
        self.git("add", ".")
        self.git("commit", "-qm", "init")
        self.collab = root / "collab"
        self.board = root / "board.json"
        self.set_live({A: ("%9", "시로코"), B: ("%6", "프라나")})
        cli = root / "fake-cli"
        cli.write_text(f"#!/bin/sh\ncat '{self.board}'\n")
        cli.chmod(0o755)
        self.env = {**os.environ, "HOME": str(root), "KASATERM_COLLAB_ROOT": str(self.collab),
                    "KASATERM_CLI": str(cli)}

    def tearDown(self):
        self.tmp.cleanup()

    def git(self, *args):
        subprocess.run(["git", "-C", str(self.repo), "-c", "user.name=t", "-c", "user.email=t@t", *args],
                       check=True)

    def set_live(self, sessions):
        panes = [{"address": {"session_id": s, "surface_id": pane}, "character": name, "title": ""}
                 for s, (pane, name) in sessions.items()]
        self.board.write_text(json.dumps({"ok": True, "result": {"panes": panes}}))

    def edit(self, session, pane, path=None):
        """그 학생의 Edit 한 번. 허락이면 파일을 실제로 고치고 True."""
        path = path or self.file
        payload = {"tool_name": "Edit", "session_id": session, "cwd": str(self.repo),
                   "tool_input": {"file_path": str(path)}}
        out = subprocess.run([str(SCRIPT)], input=json.dumps(payload), capture_output=True, text=True,
                             env={**self.env, "KASATERM_PANE_ID": pane}).stdout
        if out.strip():
            self.last_reason = json.loads(out)["hookSpecificOutput"]["permissionDecisionReason"]
            return False
        with open(path, "a") as f:
            f.write(f"{pane}\n")
        return True

    def test_uncommitted_edit_holds_the_file_against_others(self):
        self.assertTrue(self.edit(A, "%9"))
        self.assertFalse(self.edit(B, "%6"))
        self.assertIn("시로코(%9)", self.last_reason)
        self.assertIn("release", self.last_reason)
        self.assertTrue(self.edit(A, "%9"), "잡은 본인은 계속 고친다")

    def test_commit_releases_and_the_next_editor_takes_it(self):
        self.assertTrue(self.edit(A, "%9"))
        time.sleep(1.1)  # 커밋 시각은 초 단위 — 잡은 뒤에 커밋했음이 확실하게.
        self.git("commit", "-qam", "a")
        self.assertTrue(self.edit(B, "%6"))
        self.assertFalse(self.edit(A, "%9"), "커밋 뒤 새로 고친 쪽이 잡는다")

    def test_revert_releases(self):
        self.assertTrue(self.edit(A, "%9"))
        self.git("checkout", "--", "a.rs")
        self.assertTrue(self.edit(B, "%6"))

    def test_closed_pane_releases(self):
        self.assertTrue(self.edit(A, "%9"))
        self.set_live({B: ("%6", "프라나")})
        self.assertTrue(self.edit(B, "%6"))

    def test_release_command_hands_over(self):
        self.assertTrue(self.edit(A, "%9"))
        out = subprocess.run([str(SCRIPT), "release", str(self.file)], capture_output=True, text=True,
                             env={**self.env, "KASATERM_PANE_ID": "%9"}).stdout
        self.assertIn("1개", out)
        self.assertTrue(self.edit(B, "%6"))

    def test_new_untracked_file_is_held_too(self):
        new = self.repo / "b.rs"
        self.assertTrue(self.edit(A, "%9", new))
        self.assertFalse(self.edit(B, "%6", new))

    def test_unreadable_board_fails_open(self):
        self.assertTrue(self.edit(A, "%9"))
        self.board.write_text("not json")
        self.assertTrue(self.edit(B, "%6"))

    def test_outside_git_never_holds(self):
        loose = Path(self.tmp.name) / "loose.txt"
        loose.write_text("x\n")
        self.assertTrue(self.edit(A, "%9", loose))
        self.assertTrue(self.edit(B, "%6", loose))


if __name__ == "__main__":
    unittest.main()
