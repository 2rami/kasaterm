import io
import base64
import json
import re
import subprocess
import threading
import unittest
from unittest.mock import patch

from tools.request_journal.nacho import HTTPNachoProvider, SSHNachoProvider
from tools.request_journal.remote_helper import main, completion_payload, ProviderFailure, clean_diagnostic


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
            result = json.loads(output.getvalue())
            self.assertFalse(result["ok"])
            self.assertEqual(result["error"], "summary_unavailable")
            self.assertNotIn("SECRET", output.getvalue())

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
            with self.assertRaisesRegex(ProviderFailure, "^request journal provider failed$"):
                SSHNachoProvider().summarize(PAYLOAD)
        with self.assertRaises(ValueError):
            SSHNachoProvider("-oProxyCommand=bad")

    def test_http_owns_only_new_uuid_and_cleans_input_on_success_and_failure(self):
        class Fake(HTTPNachoProvider):
            def __init__(self, response):
                super().__init__()
                self.response = response
                self.calls = []
                self.screen = ""

            def _call(self, method, route, data=None, raw=False):
                self.calls.append((method, route, data))
                if route.startswith("/term/spawn"):
                    return {"ok": True, "id": "web-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"}
                if route.startswith("/term/screen"):
                    limit = int(re.search(r"lines=(\d+)",route).group(1))
                    return "\n".join(self.screen.splitlines()[-limit:])
                if route.startswith("/term/input") and isinstance(data, str):
                    marker = re.search(r"RJ_[0-9a-f]+", data)
                    if marker:
                        marker = marker.group(0)
                        if "_DIR" in data:
                            self.screen = f"{marker}_DIR:/tmp/kasaterm-journal.ABC123\n"
                        elif "_BEGIN" in data:
                            encoded = base64.b64encode(json.dumps(self.response,ensure_ascii=False).encode()).decode()
                            encoded = "\n".join(encoded[i:i+120] for i in range(0,len(encoded),120))
                            self.screen = f"{marker}_BEGIN\n{encoded}\n{marker}_END\n"
                        elif "_CLEAN" in data:
                            self.screen = f"{marker}_CLEAN\n"
                return {"ok": True}

        maximum = (chr(0x20000) * 11980 + "한글 공백\n줄바꿈 검증 마지막 글자!")[:12000]
        for response in ({"ok": True, "text": "학생 보고에 따르면 미반영이에요."}, {"ok": False, "stage": "inference_unavailable"}, {"ok":True,"text":maximum}):
            provider = Fake(response)
            if response["ok"]:
                if response["text"] == maximum:
                    self.assertEqual(provider.complete({"mode":"chat","question":"transport fixture"}),maximum)
                else:
                    self.assertIn("미반영", provider.summarize(PAYLOAD))
            else:
                with self.assertRaises(RuntimeError):
                    provider.summarize(PAYLOAD)
            self.assertEqual(provider.calls[-1][0], "DELETE")
            for method, route, data in provider.calls:
                if "pane=" in route:
                    self.assertIn("pane=web-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee", route)
                if route.startswith("/term/input"):
                    self.assertNotIn("펫 기능", data)
                if route == "/save-markdown":
                    self.assertTrue(data["path"].startswith("/tmp/kasaterm-journal.ABC123/"))
            self.assertTrue(any("_CLEAN" in str(data) and "[ ! -d /tmp/kasaterm-journal.ABC123 ]" in data for _, _, data in provider.calls if isinstance(data, str)))

    def test_http_cancel_before_start_has_no_remote_effect(self):
        event = threading.Event()
        event.set()
        provider = HTTPNachoProvider(cancel_event=event)
        with patch.object(provider, "_call", side_effect=AssertionError("no remote call")):
            with self.assertRaises(ProviderFailure) as caught:
                provider.summarize(PAYLOAD)
            self.assertEqual(caught.exception.diagnostic["stage"],"cancel")
        with self.assertRaises(ValueError):
            HTTPNachoProvider("http://external.example")

    def test_complete_preserves_long_answers_and_rejects_oversized_input(self):
        from tools.request_journal.nacho import NachoProvider
        testcase = self
        class Client:
            async def messages(self, **kwargs):
                testcase.assertIsNone(kwargs["tools"])
                testcase.assertEqual(kwargs["max_tokens"], 4096)
                testcase.assertIn("입력에 없는 요청 ID", kwargs["system"])
                return {"stop_reason": "end_turn", "content": []}
            def extract_text(self, response):
                return "확인할 것\n" * 200
        answer = NachoProvider(Client()).complete(json.dumps({"mode":"chat", "question":"뭘 확인해요?", "requests":[]}))
        self.assertGreater(len(answer), 400)
        with self.assertRaises(ProviderFailure) as caught:
            completion_payload(json.dumps({"mode":"chat", "question":"x" * 32001}))
        self.assertEqual(caught.exception.diagnostic["stage"],"input_size")

    def test_diagnostic_contains_only_enums_and_counts(self):
        value=clean_diagnostic({"stage":["SECRET"],"exception_type":"SECRET","input_chars":"SECRET","input_bytes":5,"output_chars":True,"text":"SECRET"})
        self.assertEqual(set(value),{"stage","exception_type","input_chars","input_bytes","output_chars"})
        self.assertNotIn("SECRET",json.dumps(value))
        self.assertIsNone(value["output_chars"])

    def test_model_truncation_reports_counts_without_response_or_error_text(self):
        class Client:
            async def messages(self,**kwargs):
                return {"stop_reason":"max_tokens","content":[]}
            def extract_text(self,response):
                return "SECRET"
        output=io.StringIO()
        self.assertEqual(main(io.StringIO(PAYLOAD),output,Client),1)
        result=json.loads(output.getvalue())
        self.assertEqual(result["diagnostic"]["stage"],"model_truncated")
        self.assertEqual(result["diagnostic"]["output_chars"],6)
        self.assertGreater(result["diagnostic"]["input_bytes"],result["diagnostic"]["input_chars"])
        self.assertNotIn("SECRET",output.getvalue())


if __name__ == "__main__":
    unittest.main()
