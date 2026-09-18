import unittest
from unittest.mock import patch

from tools.request_journal import chatter


ROW = {"surface_id": "%4", "character": "유우카", "status": "working", "title": "펫이 먼저 거는 말",
       "last_prompt": "10초에 한 줄씩 말하게 하자", "last_reply": "구현 들어갈게요",
       "intent": "Bash cargo test", "recent_tools": ["Bash 빌드", "Edit main.rs"]}


class SplitTests(unittest.TestCase):
    def test_bullets_numbers_and_quotes_are_stripped(self):
        lines = chatter.split("1. 유우카가 펫을 고치는 중이다냥\n- 검사도 돌렸다넹\n· 「한번 봐 보라냥~」", 5)
        self.assertEqual(lines, ["유우카가 펫을 고치는 중이다냥", "검사도 돌렸다넹", "한번 봐 보라냥~"])

    def test_repeats_and_blank_lines_are_dropped_and_count_is_capped(self):
        lines = chatter.split("첫 줄\n\n첫 줄\n둘째 줄\n셋째 줄", 2)
        self.assertEqual(lines, ["첫 줄", "둘째 줄"])

    def test_a_long_line_is_clipped_to_one_glance(self):
        [line] = chatter.split("가" * 200, 1)
        self.assertEqual(len(line), chatter.LINE_CHARS)

    def test_markdown_emphasis_never_reaches_the_bubble(self):
        self.assertEqual(chatter.split("**굵은 말**이다넹", 1), ["굵은 말이다넹"])


class ContextTests(unittest.TestCase):
    def test_only_the_focused_row_and_its_screen_are_loaded(self):
        calls = []

        def cli(*args, **kwargs):
            calls.append(args)
            if args[0] == "board":
                return True, '{"board":[{"surface_id":"%1"},' + '{"surface_id":"%4","character":"유우카"}]}'
            return True, '{"text":"첫 줄\\n\\n끝 줄"}'

        with patch.object(chatter, "run_cli", cli):
            ctx = chatter.context("%4")
        self.assertEqual(ctx["who"]["character"], "유우카")
        self.assertEqual(ctx["screen"], "첫 줄\n끝 줄")
        self.assertNotIn(("board", "--all"), calls, "모든 기기 판은 한 창 혼잣말에 싣지 않는다")


class ChatterTests(unittest.TestCase):
    def setUp(self):
        self.reply = "유우카가 펫을 고치는 중이다넹\n검사 91개 통과했다냥\n한번 봐 보라냥~"

    def client(self):
        class Client:
            sent = {}

            async def messages(self, **kwargs):
                Client.sent = kwargs
                return {"reply": True}

            @staticmethod
            def extract_text(resp, tool_names=()):
                return "유우카가 펫을 고치는 중이다넹\n검사 91개 통과했다냥\n한번 봐 보라냥~"

        return Client()

    def run_chatter(self, body, client=None, ctx=None):
        with patch.object(chatter, "llm_client", lambda factory: client), \
             patch.object(chatter, "context", lambda pane: ctx or {"who": ROW, "screen": "화면"}), \
             patch.object(chatter, "machine_label", lambda: "건호의 MacBook Pro"):
            return chatter.chatter(object(), body)

    def test_a_focused_pane_is_required(self):
        self.assertEqual(self.run_chatter({})[0], 400)
        self.assertEqual(self.run_chatter({"pane": "web-1"})[0], 400)
        self.assertEqual(self.run_chatter({"pane": "%4", "count": 99})[0], 400)

    def test_without_a_model_the_pet_stays_quiet_instead_of_guessing(self):
        status, payload = self.run_chatter({"pane": "%4"}, client=None)
        self.assertEqual((status, payload), (503, {"error": "llm_unavailable"}))

    def test_an_empty_board_and_screen_asks_no_model(self):
        status, payload = self.run_chatter({"pane": "%4"}, client=self.client(), ctx={"who": {}, "screen": ""})
        self.assertEqual((status, payload), (200, {"lines": []}))

    def test_lines_come_back_split_and_the_prompt_carries_the_machine_name(self):
        client = self.client()
        status, payload = self.run_chatter({"pane": "%4", "count": 3}, client=client)
        self.assertEqual(status, 200)
        self.assertEqual(payload["lines"], ["유우카가 펫을 고치는 중이다넹", "검사 91개 통과했다냥", "한번 봐 보라냥~"])
        sent = type(client).sent
        self.assertIn("건호의 MacBook Pro", sent["messages"][0]["content"])
        self.assertIn("유우카", sent["messages"][0]["content"])
        self.assertIn("3 줄", sent["system"])
        self.assertEqual(sent["reasoning_effort"], "low")
        self.assertIsNone(sent.get("tools"), "먼저 거는 말은 아무것도 실행하지 않는다")

    def test_a_model_failure_is_reported_rather_than_faked(self):
        class Broken:
            async def messages(self, **kwargs):
                raise RuntimeError("게이트웨이 끊김")

            @staticmethod
            def extract_text(resp, tool_names=()):
                return ""

        status, payload = self.run_chatter({"pane": "%4"}, client=Broken())
        self.assertEqual(status, 502)
        self.assertEqual(payload["error"], "model_failed")


if __name__ == "__main__":
    unittest.main()
