"""bin/transcript-items.py 시험 — `python3 -m unittest discover -s tests -p 'test_*.py'`."""
import json
import os
import subprocess
import sys
import tempfile
import unittest

SCRIPT = os.path.join(os.path.dirname(__file__), "..", "bin", "transcript-items.py")


def items(rows):
    parent = None
    with tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False) as f:
        for i, (kind, content, extra) in enumerate(rows):
            row = {"uuid": f"r{i}", "parentUuid": parent, "type": kind, "message": {"content": content}, **extra}
            f.write(json.dumps(row, ensure_ascii=False) + "\n")
            parent = row["uuid"]
    try:
        out = subprocess.run([sys.executable, SCRIPT, f.name], capture_output=True, text=True, check=True).stdout
    finally:
        os.unlink(f.name)
    return [(r[1], r[4]) for r in json.loads(out)]


class TranscriptItems(unittest.TestCase):
    def test_a_tell_is_a_prompt_and_its_framing_is_not_its_text(self):
        got = items([("user", "The kasaterm-bridge plugin sent a message:\n⟦아즈사⟧ 할 일\n\n끝", {"origin": {"kind": "plugin", "name": "kasaterm-bridge"}})])
        self.assertEqual(got, [("u", "⟦아즈사⟧ 할 일")])

    def test_drawn_rows_that_are_no_prompt_keep_their_place(self):
        got = items([
            ("user", "첫 질문", {"origin": {"kind": "composer"}}),
            ("user", "<task-notification>\n<summary>끝</summary>\n</task-notification>", {"origin": {"kind": "task-notification"}}),
            ("user", "<local-command-stdout>Set model</local-command-stdout>", {}),
            ("user", "<local-command-caveat>Caveat: 안 그려짐</local-command-caveat>", {}),
            ("user", "둘째 질문", {"origin": {"kind": "composer"}}),
        ])
        self.assertEqual(got, [("u", "첫 질문"), ("n", ""), ("n", ""), ("u", "둘째 질문")])

    def test_a_slash_command_is_a_prompt_as_drawn(self):
        got = items([("user", "<command-name>/model</command-name>\n<command-message>model</command-message>\n<command-args>haiku</command-args>", {})])
        self.assertEqual(got, [("u", "/model haiku")])

    def test_a_local_command_row_is_drawn_like_a_person_row(self):
        got = items([
            ("system", None, {"subtype": "local_command", "content": "<command-name>/model</command-name>\n<command-args></command-args>"}),
            ("system", None, {"subtype": "local_command", "content": "<local-command-stdout>Set model</local-command-stdout>"}),
            ("system", None, {"subtype": "turn_duration"}),
        ])
        self.assertEqual(got, [("u", "/model"), ("n", "")])

    def test_the_navigation_command_itself_is_no_prompt(self):
        got = items([("user", "<command-name>/prompt-nav</command-name>\n<command-args>3</command-args>", {})])
        self.assertEqual(got, [("n", "")])

    def test_a_long_paste_counts_every_line(self):
        paste = "봐 줘\n" + "\n".join(f"로그 {i}" for i in range(40))
        with tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False) as f:
            f.write(json.dumps({"uuid": "p", "parentUuid": None, "type": "user", "message": {"content": paste}}, ensure_ascii=False) + "\n")
        try:
            out = json.loads(subprocess.run([sys.executable, SCRIPT, f.name], capture_output=True, text=True, check=True).stdout)
        finally:
            os.unlink(f.name)
        self.assertEqual(out[0][1:3], ["u", 41])


if __name__ == "__main__":
    unittest.main()
