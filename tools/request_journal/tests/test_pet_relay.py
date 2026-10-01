import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from tools.request_journal import pet_relay as ask

# ⚠️**이 검사들은 진짜 나쵸를 부르면 안 된다.**
#
# 넘길 곳은 서술자 `~/.config/kasaterm/nacho-ask.json` 이 정한다 — 그 파일은 **사람이 쓰는
# 기계에 실제로 있다.** 가짜를 덜 세운 검사가 조용히 네트워크로 나가 **도는 봇**에게 말을 걸고
# 그쪽 대화 기록을 늘린 적이 있다(2026-09-22: 미니에서 `nacho_base()` 가 `http://127.0.0.1:8792`
# 를 가리켰다).
#
# 그래서 이 모듈은 **없는 서술자**를 가리켜 기본적으로 아무 데도 안 넘긴다. 중계를 보는
# 검사는 자기 자리에서 가짜 서술자·가짜 `urlopen` 을 세워 쓴다.
_ISOLATED = Path(tempfile.mkdtemp(prefix="ask-test-isolated-")) / "없는-서술자.json"
ask.NACHO_ASK_DESCRIPTOR = _ISOLATED
ask.NACHO_ASK_TOKEN_FILE = _ISOLATED.with_name("없는-열쇠")
os.environ.pop("NACHO_ASK_URL", None)


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


class PromptTests(unittest.TestCase):
    def test_persona_is_cut_before_memory_rules_and_falls_back(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            self.assertEqual(ask.persona(repo), ask.FALLBACK_PERSONA)
            (repo / "prompts").mkdir()
            (repo / "prompts/system.md").write_text("너는 나쵸\n\n# 톤\n- 반말\n\n# 메모리 (가장 중요한 도구)\n볼트를 뒤져라\n", encoding="utf-8")
            text = ask.persona(repo)
            self.assertIn("# 톤", text)
            self.assertNotIn("볼트", text)

class IsolationTests(unittest.TestCase):
    """검사가 **사람이 쓰는 봇**에 말을 걸지 않는지. 이 한 건이 무너지면 나머지가 전부
    네트워크 검사가 되고, 실패는 남의 기계 사정에 따라 오간다(2026-09-22)."""

    def test_no_test_in_this_module_relays_to_a_real_nacho(self):
        self.assertEqual(ask.nacho_base(), "", "기본은 아무 데도 안 넘긴다")
        self.assertFalse(ask.NACHO_ASK_DESCRIPTOR.exists(), str(ask.NACHO_ASK_DESCRIPTOR))
        with patch.object(ask, "urlopen") as opened:
            self.assertEqual(ask.nacho_call("/api/pet/poll", {"machine": "맥북"}, 1.0), (0, None))
        opened.assert_not_called()


class NachoDescriptorTests(unittest.TestCase):
    """넘길 곳은 서술자·env 가 정하고, 없거나 이상하면 아무 데도 안 넘긴다."""

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="nacho-ask-"))
        self.descriptor = self.tmp / "nacho-ask.json"
        self.token = self.tmp / "key"

    def test_without_a_descriptor_nothing_is_relayed(self):
        with patch.object(ask, "NACHO_ASK_DESCRIPTOR", self.tmp / "없다.json"), \
                patch.dict(os.environ, {}, clear=False), patch.object(ask, "urlopen") as opened:
            os.environ.pop("NACHO_ASK_URL", None)
            self.assertEqual(ask.nacho_base(), "")
            self.assertEqual(ask.nacho_call("/api/pet/poll", {}, 1.0), (0, None))
        opened.assert_not_called()

    def test_a_bad_descriptor_is_ignored(self):
        for bad in ('{"version":2,"url":"http://x"}', '{"version":1,"url":"ftp://x"}', "not json", "{}"):
            self.descriptor.write_text(bad, encoding="utf-8")
            with patch.object(ask, "NACHO_ASK_DESCRIPTOR", self.descriptor):
                os.environ.pop("NACHO_ASK_URL", None)
                self.assertEqual(ask.nacho_base(), "", bad)

    def test_no_token_file_means_no_token_header(self):
        self.descriptor.write_text(json.dumps({"version": 1, "url": "http://10.0.0.1:8792"}), encoding="utf-8")
        seen = {}

        class Resp:
            status = 200
            def read(self, _n=None):
                return rb'{"answer":"\uc751"}'
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
            out = ask.nacho_call("/api/pet/poll", {"machine": "맥북"}, 1.0)
        self.assertEqual(out, (200, {"answer": "응"}))
        self.assertNotIn("x-nacho-token", seen)

    def test_an_env_url_wins_over_the_descriptor(self):
        self.descriptor.write_text(json.dumps({"version": 1, "url": "http://파일:1"}), encoding="utf-8")
        with patch.object(ask, "NACHO_ASK_DESCRIPTOR", self.descriptor), \
                patch.dict(os.environ, {"NACHO_ASK_URL": "http://환경:2/"}):
            self.assertEqual(ask.nacho_base(), "http://환경:2")


class PetPollTests(unittest.TestCase):
    """펫이 인계·완료 소식을 **끌어가는** 길(2026-09-22). 이 기계 이름만 여기서 채우고
    나머지는 나쵸 우편함을 그대로 중계한다."""

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="pet-poll-"))
        self.descriptor = self.tmp / "nacho-ask.json"
        self.descriptor.write_text(json.dumps({"version": 1, "url": "http://10.0.0.1:8792"}), encoding="utf-8")
        ask._MACHINE.update(label="", at=0.0)

    def _board(self, *args, **kw):
        if args == ("board", "--all"):
            return True, json.dumps({"result": board_all()})
        return True, "{}"

    def _relay(self, body, reply, status=200, cli=None):
        sent = {}

        class Resp:
            def read(self, _n=None):
                return json.dumps(reply).encode()

            def __enter__(self):
                return self

            def __exit__(self, *a):
                return False

        Resp.status = status

        def fake_open(req, timeout=None):
            sent["url"], sent["timeout"] = req.full_url, timeout
            sent["body"] = json.loads(req.data)
            return Resp()

        with patch.object(ask, "NACHO_ASK_DESCRIPTOR", self.descriptor), \
                patch.object(ask, "NACHO_ASK_TOKEN_FILE", self.tmp / "없다"), \
                patch.object(ask, "urlopen", side_effect=fake_open), \
                patch.object(ask, "run_cli", side_effect=cli or self._board):
            os.environ.pop("NACHO_ASK_URL", None)
            got = ask.relay_poll(body)
        return got, sent

    def test_the_machine_name_is_filled_in_here_and_handed_back(self):
        # 펫은 자기가 어느 바탕화면인지 모른다 — 그 이름이 곧 나쵸 쪽 대화 자리다.
        (status, payload), sent = self._relay(
            {"wait": 20}, {"ok": True, "conv": "kasapet:맥북", "messages": [{"id": "a", "text": "이어받을게"}]})
        self.assertEqual(status, 200)
        self.assertEqual(sent["url"], "http://10.0.0.1:8792/api/pet/poll")
        self.assertEqual(sent["body"]["machine"], "맥북")
        self.assertEqual(payload["machine"], "맥북")      # 펫이 다음에 그대로 들고 온다
        self.assertEqual(payload["messages"][0]["id"], "a")

    def test_the_pet_can_carry_the_name_itself(self):
        _got, sent = self._relay({"machine": "나쵸네코", "ack": ["a", "b"], "wait": 5}, {"ok": True, "messages": []})
        self.assertEqual(sent["body"]["machine"], "나쵸네코")
        self.assertEqual(sent["body"]["ack"], ["a", "b"])
        self.assertEqual(sent["body"]["wait"], 5)

    def test_an_unknown_machine_never_binds_to_a_default_seat(self):
        # 이름을 못 읽었는데 기본값으로 붙이면 그 펫이 **남의 바탕화면 소식**을 받는다.
        (status, payload), sent = self._relay({}, {"ok": True}, cli=lambda *a, **k: (False, "판을 못 읽었다"))
        self.assertEqual((status, payload["error"]), (503, "machine_unknown"))
        self.assertEqual(sent, {}, "자리를 모르면 나쵸에 묻지도 않는다")

    def test_when_nacho_is_unreachable_the_pet_is_told_to_come_back_later(self):
        with patch.object(ask, "NACHO_ASK_DESCRIPTOR", self.descriptor), \
                patch.object(ask, "urlopen", side_effect=OSError("끊김")), \
                patch.object(ask, "run_cli", side_effect=self._board):
            os.environ.pop("NACHO_ASK_URL", None)
            status, payload = ask.relay_poll({"machine": "맥북", "wait": 0})
        self.assertEqual((status, payload["error"]), (503, "nacho_unreachable"))

    def test_the_wait_is_capped_so_the_pet_is_never_left_hanging(self):
        _got, sent = self._relay({"machine": "맥북", "wait": 9999}, {"ok": True, "messages": []})
        self.assertEqual(sent["body"]["wait"], ask.POLL_WAIT_CAP)
        # 왕복 여유를 얹어 기다린다 — 여기서 먼저 끊으면 준 줄이 못 받은 것으로 남는다.
        self.assertGreater(sent["timeout"], ask.POLL_WAIT_CAP)

    def test_the_machine_name_is_held_for_a_while(self):
        calls = []

        def counting(*args, **kw):
            calls.append(args)
            return self._board(*args, **kw)

        self._relay({}, {"ok": True, "messages": []}, cli=counting)
        self._relay({}, {"ok": True, "messages": []}, cli=counting)
        self.assertEqual(len([c for c in calls if c == ("board", "--all")]), 1,
                         "몇 초마다 들르는 길이라 판을 매번 읽지 않는다")

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

class PlainTests(unittest.TestCase):
    def test_markdown_marks_are_stripped_for_the_bubble(self):
        self.assertEqual(ask.plain("**■ 맥북** — 연결됨\n- **아리스** — 질문 중\n### 끝\n```\nx\n```"), "■ 맥북 — 연결됨\n· 아리스 — 질문 중\n끝\nx")


if __name__ == "__main__":
    unittest.main()
