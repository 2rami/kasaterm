import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from tools.request_journal import ask


def board_all():
    return {
        "sources": [
            {"machine_id": "mac", "label": "맥북", "state": "online", "is_local": True},
            {"machine_id": "mini", "label": "미니", "state": "online", "is_local": False},
            {"machine_id": "desk", "label": "데스크탑", "state": "offline", "is_local": False},
        ],
        "panes": [
            {"address": {"machine_id": "mac", "surface_id": "%9"}, "room_label": "kasaterm", "character": "유우카",
             "title": "펫 기기 요약", "status": "working", "request": "요약해줘", "progress": "판을 읽는 중", "status_reason": "transcript turn open", "freshness": "fresh"},
            {"address": {"machine_id": "mac", "surface_id": "%2"}, "room_label": "momewomo", "character": "호시노",
             "title": "3d", "status": "idle", "request": "", "progress": "", "status_reason": "transcript turn closed", "freshness": "fresh"},
            {"address": {"machine_id": "mac", "surface_id": "%0"}, "room_label": "나쵸네코 · swarm", "character": "유즈",
             "title": "거울", "status": "unknown", "status_reason": "remote mirror; observe agent on its source machine", "freshness": "fresh"},
            {"address": {"machine_id": "mini", "surface_id": "%0"}, "room_label": "mission-control", "character": "유즈",
             "title": "문서 자동 생성", "status": "working", "request": "문서 만들어", "progress": "", "status_reason": "transcript turn open", "freshness": "stale"},
            {"address": {"machine_id": "mini", "surface_id": "%1"}, "room_label": "mission-control", "character": "모모이",
             "title": "브리프", "status": "idle", "request": "", "progress": "", "status_reason": "transcript turn closed", "freshness": "fresh"},
        ],
    }


LOCAL = {"%9": {"surface_id": "%9", "intent": "Bash cargo test", "idle_secs": 0},
         "%2": {"surface_id": "%2", "idle_secs": 1272, "waiting_for": "권한 승인"}}
MACHINES = [{"label": "미니", "route": "~mini", "online": True,
             "panes": [{"id": "%0", "name": "유즈", "status": "working", "doing": "Bash npx prisma migrate", "idle_secs": None},
                       {"id": "%1", "name": "모모이", "status": "idle", "idle_secs": 552}]},
            {"label": "데스크탑", "route": "데스크탑", "online": False, "panes": []}]


class FleetTests(unittest.TestCase):
    def test_mirrors_are_dropped_and_machines_keep_their_students(self):
        machines = ask.fleet(board_all(), LOCAL, MACHINES)
        self.assertEqual([m["label"] for m in machines], ["맥북", "미니", "데스크탑"])
        mac = machines[0]
        self.assertTrue(mac["is_local"] and mac["online"])
        names = sorted(e["name"] for rows in mac["rooms"].values() for e in rows)
        self.assertEqual(names, ["유우카", "호시노"], "거울 줄(유즈)은 이 기기 학생이 아니다")
        self.assertFalse(machines[2]["online"])

    def test_human_needed_comes_first_and_extras_fill_in(self):
        machines = ask.fleet(board_all(), LOCAL, MACHINES)
        by_name = {e["name"]: e for m in machines for rows in m["rooms"].values() for e in rows}
        self.assertEqual(by_name["호시노"]["state"], "사람 손 필요")
        self.assertEqual(by_name["호시노"]["waiting"], "권한 승인")
        self.assertEqual(by_name["호시노"]["idle_min"], 21)
        self.assertEqual(by_name["유우카"]["doing"], "Bash cargo test")
        self.assertEqual(by_name["유즈"]["doing"], "Bash npx prisma migrate")
        self.assertTrue(by_name["유즈"]["stale"])
        self.assertEqual(by_name["모모이"]["idle_min"], 9)

    def test_digest_reads_by_machine_with_offline_line_and_tally(self):
        text = ask.digest(ask.fleet(board_all(), LOCAL, MACHINES))
        self.assertIn("■ 맥북 (이 기기) — 연결됨 · 학생 2 (사람 손 필요 1 · 일하는 중 1)", text)
        self.assertIn("■ 데스크탑 — 끊김", text)
        self.assertIn("기다림: 권한 승인", text)
        self.assertIn("(오래된 관측)", text)
        self.assertLess(text.index("호시노"), text.index("유우카"), "사람 손 필요한 학생이 앞이다")
        self.assertNotIn("거울", text)
        self.assertEqual(ask.digest([]), "(기계 목록이 비어 있다)")

    def test_digest_is_bounded(self):
        board = board_all()
        board["panes"] = [dict(board["panes"][0], address={"machine_id": "mac", "surface_id": f"%{i}"}, progress="가" * 500) for i in range(80)]
        text = ask.digest(ask.fleet(board, {}, []))
        self.assertLessEqual(len(text), ask.FLEET_CHARS)


class PromptTests(unittest.TestCase):
    def test_prompt_carries_fleet_and_stays_bounded(self):
        ctx = {"who": {"character": "유우카", "status": "working"}, "screen": "줄\n" * 5000, "fleet": "■ 맥북 — 연결됨"}
        prompt = ask.build_prompt("다들 뭐 해?", "%9", ctx)
        self.assertIn("[모든 기기]\n■ 맥북 — 연결됨", prompt)
        self.assertIn("[지금 보는 창] %9 · 학생 유우카", prompt)
        self.assertLessEqual(len(prompt), ask.MAX_CHARS + 10)

    def test_persona_is_cut_before_memory_rules_and_falls_back(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            self.assertEqual(ask.persona(repo), ask.FALLBACK_PERSONA)
            (repo / "prompts").mkdir()
            (repo / "prompts/system.md").write_text("너는 나쵸\n\n# 톤\n- 반말\n\n# 메모리 (가장 중요한 도구)\n볼트를 뒤져라\n", encoding="utf-8")
            text = ask.persona(repo)
            self.assertIn("# 톤", text)
            self.assertNotIn("볼트", text)
            self.assertIn("# 지금 자리", ask.system_prompt(repo))

    def test_context_survives_a_missing_cli(self):
        with patch.object(ask, "run_cli", return_value=(False, "kasaterm-cli 를 못 찾았다")), patch.object(ask, "_machines_http", return_value=[]):
            ctx = ask.context("%9")
        self.assertEqual(ctx["who"], {})
        self.assertIn("판을 읽지 못했다", ctx["fleet"])


class AnswerTests(unittest.TestCase):
    def test_answer_uses_persona_and_fleet_and_runs_only_listed_tools(self):
        class Client:
            def __init__(self):
                self.calls = []

            async def messages(self, **kw):
                self.calls.append(kw)
                return {"content": [{"type": "text", "text": "음... 다들 조용하네 (=^･ω･^=)"}, {"type": "tool_use", "name": "evil", "input": {}}, {"type": "tool_use", "name": "focus_pane", "input": {}}]}

            @staticmethod
            def extract_text(resp, names):
                return "".join(b["text"] for b in resp["content"] if b["type"] == "text")

            @staticmethod
            def extract_tool_uses(resp):
                return [b for b in resp["content"] if b["type"] == "tool_use"]

        client = Client()
        calls = []

        def cli(*args, **kw):
            calls.append(args)
            if args == ("board", "--all"):
                return True, json.dumps({"result": board_all()})
            if args == ("board",):
                return True, json.dumps({"result": {"board": list(LOCAL.values())}})
            if args[0] == "peek":
                return True, json.dumps({"result": {"text": "화면 끝"}})
            return True, "{}"

        with patch.object(ask, "llm_client", return_value=client), patch.object(ask, "run_cli", side_effect=cli), patch.object(ask, "_machines_http", return_value=MACHINES):
            status, payload = ask.answer(lambda _cancel: None, {"text": "다들 뭐 해?", "pane": "%9"})
        self.assertEqual(status, 200)
        self.assertEqual(payload["answer"], "음... 다들 조용하네 (=^･ω･^=)")
        self.assertEqual([a["kind"] for a in payload["actions"]], ["focus_pane"])
        self.assertIn(("focus", "%9"), calls)
        prompt = client.calls[0]["messages"][0]["content"]
        self.assertIn("■ 미니 — 연결됨", prompt)
        self.assertIn("# 지금 자리", client.calls[0]["system"])
        self.assertEqual(client.calls[0]["max_tokens"], 900)


class PlainTests(unittest.TestCase):
    def test_markdown_marks_are_stripped_for_the_bubble(self):
        self.assertEqual(ask.plain("**■ 맥북** — 연결됨\n- **아리스** — 질문 중\n### 끝\n```\nx\n```"), "■ 맥북 — 연결됨\n· 아리스 — 질문 중\n끝\nx")


if __name__ == "__main__":
    unittest.main()
