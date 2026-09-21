import json
import os
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
                return {"content": [{"type": "text", "text": "#연출 동작=Error 표정=cry\n음... 다들 조용하네 (=^･ω･^=)"}, {"type": "tool_use", "name": "evil", "input": {}}, {"type": "tool_use", "name": "focus_pane", "input": {}}]}

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
            status, payload = ask.answer(lambda _cancel: None, {"text": "다들 뭐 해?", "pane": "%9", "catalog": CATALOG})
        self.assertEqual(status, 200)
        self.assertEqual(payload["answer"], "음... 다들 조용하네 (=^･ω･^=)")
        self.assertEqual(payload["act"], {"motion": "Error", "expression": "cry"})
        self.assertIn("[할 수 있는 동작]", client.calls[0]["messages"][0]["content"])
        self.assertEqual([a["kind"] for a in payload["actions"]], ["focus_pane"])
        self.assertIn(("focus", "%9"), calls)
        prompt = client.calls[0]["messages"][0]["content"]
        self.assertIn("■ 미니 — 연결됨", prompt)
        self.assertIn("# 지금 자리", client.calls[0]["system"])
        self.assertEqual(client.calls[0]["max_tokens"], 900)


CATALOG = {"motions": [{"group": "Idle", "label": "대기"}, {"group": "Think", "label": "생각"}, {"group": "Error", "label": "곤란"}],
           "expressions": [{"name": "blush", "label": "홍조"}, {"name": "cry", "label": "눈물"}]}


class NachoRelayTests(unittest.TestCase):
    """두뇌 하나 — 판·화면은 여기서 모으고 답은 나쵸가 낸다(2026-09-21). 나쵸가 없으면 옛 길로."""

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="nacho-ask-"))
        self.descriptor = self.tmp / "nacho-ask.json"
        self.token = self.tmp / "key"
        self.ctx = {"who": {"character": "유우카"}, "screen": "화면 끝", "fleet": "■ 미니 — 연결됨", "machine": "맥북"}

    def _cli(self):
        def cli(*args, **kw):
            if args == ("board", "--all"):
                return True, json.dumps({"result": board_all()})
            if args == ("board",):
                return True, json.dumps({"result": {"board": list(LOCAL.values())}})
            if args[0] == "peek":
                return True, json.dumps({"result": {"text": "화면 끝"}})
            return True, "{}"
        return cli

    def test_without_a_descriptor_nothing_is_relayed(self):
        with patch.object(ask, "NACHO_ASK_DESCRIPTOR", self.tmp / "없다.json"), \
                patch.dict(os.environ, {}, clear=False), patch.object(ask, "urlopen") as opened:
            os.environ.pop("NACHO_ASK_URL", None)
            self.assertEqual(ask.nacho_base(), "")
            self.assertIsNone(ask.ask_nacho("안녕", "%9", self.ctx))
        opened.assert_not_called()

    def test_a_bad_descriptor_is_ignored(self):
        for bad in ('{"version":2,"url":"http://x"}', '{"version":1,"url":"ftp://x"}', "not json", "{}"):
            self.descriptor.write_text(bad, encoding="utf-8")
            with patch.object(ask, "NACHO_ASK_DESCRIPTOR", self.descriptor):
                os.environ.pop("NACHO_ASK_URL", None)
                self.assertEqual(ask.nacho_base(), "", bad)

    def test_the_question_goes_to_nacho_with_the_fleet_and_its_answer_is_returned(self):
        self.descriptor.write_text(json.dumps({"version": 1, "url": "http://10.0.0.1:8792/"}), encoding="utf-8")
        self.token.write_text("s3cret\n", encoding="utf-8")
        sent = {}

        class Resp:
            status = 200

            def read(self, _n=None):
                return json.dumps({"answer": "유우카가 펫 고치는 중이래", "actions": [],
                                   "act": {"motion": "Talk", "expression": None}}).encode()

            def __enter__(self):
                return self

            def __exit__(self, *a):
                return False

        def fake_open(req, timeout=None):
            sent["url"], sent["timeout"] = req.full_url, timeout
            sent["headers"] = {k.lower(): v for k, v in req.headers.items()}
            sent["body"] = json.loads(req.data)
            return Resp()

        with patch.object(ask, "NACHO_ASK_DESCRIPTOR", self.descriptor), \
                patch.object(ask, "NACHO_ASK_TOKEN_FILE", self.token), \
                patch.object(ask, "urlopen", side_effect=fake_open), \
                patch.object(ask, "run_cli", side_effect=self._cli()), \
                patch.object(ask, "_machines_http", return_value=MACHINES), \
                patch.object(ask, "llm_client") as local_brain:
            os.environ.pop("NACHO_ASK_URL", None)
            status, payload = ask.answer(lambda _c: None, {"text": "다들 뭐 해?", "pane": "%9", "catalog": CATALOG})
        self.assertEqual((status, payload["answer"]), (200, "유우카가 펫 고치는 중이래"))
        self.assertEqual(payload["act"], {"motion": "Talk", "expression": None})
        local_brain.assert_not_called()          # 나쵸가 답했으면 이 기계 모델은 안 돈다
        self.assertEqual(sent["url"], "http://10.0.0.1:8792/api/ask")
        self.assertEqual(sent["headers"]["x-journal-request"], "1")
        self.assertEqual(sent["headers"]["x-nacho-token"], "s3cret")
        self.assertLessEqual(sent["timeout"], 45)
        self.assertEqual(sent["body"]["pane"], "%9")
        self.assertEqual(sent["body"]["machine"], "맥북")          # 보드가 말하는 이 기계 이름
        self.assertIn("■ 미니 — 연결됨", sent["body"]["context"]["fleet"])
        self.assertEqual(sent["body"]["context"]["screen"], "화면 끝")
        self.assertEqual(sent["body"]["catalog"], CATALOG)

    def test_no_token_file_means_no_token_header(self):
        self.descriptor.write_text(json.dumps({"version": 1, "url": "http://10.0.0.1:8792"}), encoding="utf-8")
        seen = {}

        class Resp:
            status = 200
            def read(self, _n=None):
                return b'{"answer":"\uc751"}'
            def __enter__(self):
                return self
            def __exit__(self, *a):
                return False

        def fake_open(req, timeout=None):
            seen.update({k.lower(): v for k, v in req.headers.items()})
            return Resp()

        with patch.object(ask, "NACHO_ASK_DESCRIPTOR", self.descriptor), \
                patch.object(ask, "NACHO_ASK_TOKEN_FILE", self.tmp / "없다"), \
                patch.object(ask, "urlopen", side_effect=fake_open):
            os.environ.pop("NACHO_ASK_URL", None)
            out = ask.ask_nacho("안녕", "%9", self.ctx)
        self.assertEqual(out, {"answer": "응", "actions": [], "act": {"motion": None, "expression": None}})
        self.assertNotIn("x-nacho-token", seen)

    def test_when_nacho_fails_the_local_brain_answers(self):
        self.descriptor.write_text(json.dumps({"version": 1, "url": "http://10.0.0.1:8792"}), encoding="utf-8")

        class Down:
            status = 504
            def read(self, _n=None):
                return b"{}"
            def __enter__(self):
                return self
            def __exit__(self, *a):
                return False

        class Empty(Down):
            status = 200
            def read(self, _n=None):
                return b'{"answer":"   "}'

        class Client:
            def __init__(self):
                self.calls = []
            async def messages(self, **kw):
                self.calls.append(kw)
                return {"content": [{"type": "text", "text": "여기서 답했어"}]}
            @staticmethod
            def extract_text(resp, names):
                return resp["content"][0]["text"]
            @staticmethod
            def extract_tool_uses(resp):
                return []

        for opener in (OSError("메시 끊김"), Down(), Empty()):
            client = Client()
            side = (lambda *a, **k: (_ for _ in ()).throw(opener)) if isinstance(opener, Exception) else (lambda *a, **k: opener)
            with patch.object(ask, "NACHO_ASK_DESCRIPTOR", self.descriptor), \
                    patch.object(ask, "NACHO_ASK_TOKEN_FILE", self.tmp / "없다"), \
                    patch.object(ask, "urlopen", side_effect=side), \
                    patch.object(ask, "llm_client", return_value=client), \
                    patch.object(ask, "run_cli", side_effect=self._cli()), \
                    patch.object(ask, "_machines_http", return_value=MACHINES):
                os.environ.pop("NACHO_ASK_URL", None)
                status, payload = ask.answer(lambda _c: None, {"text": "다들 뭐 해?", "pane": "%9", "catalog": CATALOG})
            self.assertEqual((status, payload["answer"]), (200, "여기서 답했어"), repr(opener))
            self.assertIn("■ 미니 — 연결됨", client.calls[0]["messages"][0]["content"])

    def test_an_env_url_wins_over_the_descriptor(self):
        self.descriptor.write_text(json.dumps({"version": 1, "url": "http://파일:1"}), encoding="utf-8")
        with patch.object(ask, "NACHO_ASK_DESCRIPTOR", self.descriptor), \
                patch.dict(os.environ, {"NACHO_ASK_URL": "http://환경:2/"}):
            self.assertEqual(ask.nacho_base(), "http://환경:2")


class PerformTests(unittest.TestCase):
    def test_the_act_line_is_taken_off_the_answer_and_checked_against_the_catalog(self):
        text, act = ask.perform("#연출 동작=think 표정=Blush\n음... 확인 중이야", CATALOG)
        self.assertEqual(text, "음... 확인 중이야")
        self.assertEqual(act, {"motion": "Think", "expression": "blush"})
        text, act = ask.perform("#연출 동작=Dance 표정=없음\n꺄", CATALOG)
        self.assertEqual((text, act), ("꺄", {"motion": None, "expression": None}))
        text, act = ask.perform("연출 없이 온 답", CATALOG)
        self.assertEqual((text, act["motion"]), ("연출 없이 온 답", None))

    def test_the_prompt_lists_what_the_character_can_do(self):
        prompt = ask.build_prompt("뭐 해?", "%9", {"who": {}, "screen": "", "fleet": "-"}, CATALOG)
        self.assertIn("[할 수 있는 동작] Idle(대기), Think(생각), Error(곤란)", prompt)
        self.assertIn("[지을 수 있는 표정] blush(홍조), cry(눈물)", prompt)
        self.assertNotIn("[할 수 있는 동작]", ask.build_prompt("뭐 해?", "%9", {"who": {}, "screen": "", "fleet": "-"}, None))


class CliTests(unittest.TestCase):
    def test_a_failed_cli_call_yields_the_reason_not_the_json_envelope(self):
        import subprocess
        envelope = '{"id":"cli-3","ok":false,"error":{"code":"backend","message":"%9 은 이미 원격 pane 이다 — 이사할 로컬이 없다"}}'
        done = subprocess.CompletedProcess(["kasaterm-cli"], 1, stdout=envelope, stderr="")
        with patch.object(ask, "cli_path", return_value="/bin/kasaterm-cli"), patch.object(ask, "socket_path", return_value=None), patch.object(ask.subprocess, "run", return_value=done):
            ok, detail = ask.run_cli("migrate", "%9", "나쵸네코")
        self.assertFalse(ok)
        self.assertEqual(detail, "%9 은 이미 원격 pane 이다 — 이사할 로컬이 없다")
        done = subprocess.CompletedProcess(["kasaterm-cli"], 2, stdout="", stderr="kasaterm-cli: 소켓이 없다\n")
        with patch.object(ask, "cli_path", return_value="/bin/kasaterm-cli"), patch.object(ask, "socket_path", return_value=None), patch.object(ask.subprocess, "run", return_value=done):
            self.assertEqual(ask.run_cli("focus", "%9"), (False, "kasaterm-cli: 소켓이 없다"))

    def test_a_queued_migration_says_so_instead_of_claiming_the_move(self):
        queued = '{"id":"cli-4","ok":true,"result":{"ok":true,"remote_id":"예약됨 — %9 가 하던 턴을 마치면 이사간다"}}'
        with patch.object(ask, "run_cli", return_value=(True, queued)):
            rows = ask.execute("%9", [{"name": "migrate_pane", "input": {"machine": "나쵸네코"}}])
        self.assertEqual(rows[0]["detail"], "예약됨 — %9 가 하던 턴을 마치면 이사간다")
        with patch.object(ask, "run_cli", return_value=(True, '{"id":"cli-5","ok":true,"result":{"ok":true,"remote_id":"%3"}}')):
            rows = ask.execute("%9", [{"name": "migrate_pane", "input": {"machine": "나쵸네코"}}])
        self.assertEqual(rows[0]["detail"], "나쵸네코로 이사")


class HomepcTests(unittest.TestCase):
    def test_home_pc_tool_relays_the_script_line_and_its_outcome(self):
        import os, stat, subprocess
        d = tempfile.mkdtemp()
        script = Path(d) / "homepc"
        script.write_text("#!/bin/sh\ncase \"$1\" in on) echo \"켜기 신호 보냄\";; *) echo \"터널 안 뜸\" >&2; exit 1;; esac\n")
        os.chmod(script, stat.S_IRWXU)
        with patch.object(ask, "HOMEPC_BIN", script):
            self.assertEqual(ask.homepc("on"), (True, "켜기 신호 보냄"))
            self.assertEqual(ask.homepc("status"), (False, "터널 안 뜸"))
            self.assertEqual(ask.homepc("dance")[0], False)
            rows = ask.execute("%9", [{"name": "homepc", "input": {"action": "on"}}])
        self.assertEqual(rows, [{"kind": "homepc", "ok": True, "detail": "켜기 신호 보냄"}])
        with patch.object(ask, "HOMEPC_BIN", Path(d) / "없다"):
            self.assertIn("없다", ask.homepc("on")[1])
        # launchd 의 빈 PATH 라도 홈브루·~/.local/bin 이 앞에 붙는다 — cloudflared 를 찾는 길이다.
        script.write_text("#!/bin/sh\necho \"$PATH\"\n")
        with patch.object(ask, "HOMEPC_BIN", script), patch.dict(os.environ, {"PATH": "/usr/bin:/bin"}):
            ok, path = ask.homepc("status")
        self.assertTrue(ok)
        self.assertTrue(path.startswith(str(Path.home() / ".local/bin")), path)
        self.assertIn("/opt/homebrew/bin", path)


class PlainTests(unittest.TestCase):
    def test_markdown_marks_are_stripped_for_the_bubble(self):
        self.assertEqual(ask.plain("**■ 맥북** — 연결됨\n- **아리스** — 질문 중\n### 끝\n```\nx\n```"), "■ 맥북 — 연결됨\n· 아리스 — 질문 중\n끝\nx")


if __name__ == "__main__":
    unittest.main()
