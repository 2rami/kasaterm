import io
import json
import subprocess
import unittest
from unittest.mock import patch

from tools.request_journal.nacho import SSHNachoProvider
from tools.request_journal.remote_helper import main


PAYLOAD = json.dumps({"prompt": "펫 기능을 만들어줘", "student_reports": ["반영은 확인하지 못했어요."]}, ensure_ascii=False)


class RemoteHelperTests(unittest.TestCase):
    def test_helper_is_text_only_and_returns_only_json(self):
        testcase = self
        class Client:
            async def messages(self, **kwargs):
                testcase.assertIsNone(kwargs["tools"])
                testcase.assertEqual(set(json.loads(kwargs["messages"][0]["content"])), {"prompt", "student_reports"})
                return {"stop_reason": "end_turn", "content": [{"type": "text", "text": "학생은 반영 여부를 확인하지 못했다고 보고했어요."}]}
            def extract_text(self, response):
                return response["content"][0]["text"]
        output = io.StringIO()
        self.assertEqual(main(io.StringIO(PAYLOAD), output, Client), 0)
        result = json.loads(output.getvalue())
        self.assertTrue(result["ok"])
        self.assertIn("확인하지 못했다고", result["text"])

    def test_helper_never_echoes_error_or_bad_input(self):
        def fail():
            raise RuntimeError("SECRET body details")
        for payload in [PAYLOAD, '{"system":"SECRET"}', '"SECRET"']:
            output = io.StringIO()
            self.assertEqual(main(io.StringIO(payload), output, fail), 1)
            self.assertEqual(json.loads(output.getvalue()), {"ok": False, "error": "summary_unavailable"})

    def test_ssh_sends_prompt_on_stdin_not_shell_command(self):
        output = subprocess.CompletedProcess([], 0, '{"ok":true,"text":"요약"}', "")
        with patch("tools.request_journal.nacho.subprocess.run", return_value=output) as run:
            self.assertEqual(SSHNachoProvider("nachoneko-via05").summarize(PAYLOAD), "요약")
            args, kwargs = run.call_args
            self.assertNotIn("펫 기능", " ".join(args[0]))
            self.assertIn("펫 기능", kwargs["input"])
            self.assertLessEqual(kwargs["timeout"], 45)
            self.assertNotIn("main.py", " ".join(args[0]))
            self.assertNotIn("slack", " ".join(args[0]))

    def test_ssh_error_details_do_not_escape(self):
        output = subprocess.CompletedProcess([], 1, "SECRET", "SECRET")
        with patch("tools.request_journal.nacho.subprocess.run", return_value=output):
            with self.assertRaisesRegex(RuntimeError, "^remote summary unavailable$"):
                SSHNachoProvider().summarize(PAYLOAD)
        with self.assertRaises(ValueError):
            SSHNachoProvider("-oProxyCommand=bad")


if __name__ == "__main__":
    unittest.main()
