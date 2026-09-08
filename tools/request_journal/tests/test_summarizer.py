import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from tools.request_journal.nacho import NachoProvider
from tools.request_journal.summarizer import Summarizer


class Store:
    def __init__(self):
        self.rows = [{"id": "1", "prompt": "펫 메뉴 만들어줘", "finals": [{"text": "완료했어요. 재시작이 필요해요."}], "reported_status": "reported_done", "applied_status": "unknown"}]

    def list_requests(self, **kwargs):
        before = kwargs.get("before")
        rows = self.rows
        if before:
            rows = rows[next(i for i, row in enumerate(rows) if row["id"] == before) + 1:]
        return rows[:kwargs.get("limit", 50)]

    def set_summary(self, request_id, summary, evidence=None):
        row = next(row for row in self.rows if row["id"] == request_id)
        row.update(summary=summary, summary_evidence=evidence)


class SummaryTests(unittest.TestCase):
    def test_fallback_and_cache_do_not_certify_application(self):
        store = Store()
        summarizer = Summarizer()
        self.assertEqual(summarizer.update(store)["updated"], 1)
        self.assertIn("나쵸 요약 연결 안 됨", store.rows[0]["summary"])
        self.assertEqual(store.rows[0]["applied_status"], "unknown")
        self.assertEqual(summarizer.update(store)["updated"], 0)
        store.rows[0]["finals"].append({"text": "반영 여부는 확인 못 했어요."})
        self.assertEqual(summarizer.update(store)["updated"], 1)

    def test_only_prompt_and_final_text_are_sent(self):
        class Provider:
            name = "test"
            def summarize(self, payload):
                data = json.loads(payload)
                self_test.assertEqual(set(data), {"prompt", "student_reports"})
                self_test.assertNotIn("tool_result", payload)
                return "학생은 펫 메뉴를 완료했다고 보고했어요."
        self_test = self
        store = Store()
        store.rows[0]["tool_result"] = "private output"
        store.rows[0]["finals"].append({"kind": "assistant_note", "text": "tool_result commentary"})
        Summarizer(Provider()).update(store)
        self.assertIn("학생 보고 기준", store.rows[0]["summary"])
        self.assertEqual(store.rows[0]["applied_status"], "unknown")

    def test_failure_is_bounded_and_error_body_is_not_saved(self):
        class Provider:
            name = "test"
            def summarize(self, payload):
                raise RuntimeError("private transport details")
        store = Store()
        store.rows.append({**store.rows[0], "id": "2"})
        result = Summarizer(Provider()).update(store)
        self.assertEqual(result["updated"], 1)
        self.assertNotIn("private transport", json.dumps(store.rows))

    def test_no_credential_does_not_read_legacy_key_file(self):
        with patch.dict("os.environ", {}, clear=True), patch("pathlib.Path.read_text", side_effect=AssertionError("no files")):
            self.assertIsNone(NachoProvider.from_environment())

    def test_backfill_reaches_history_beyond_first_page(self):
        store = Store()
        store.rows = [{**store.rows[0], "id": str(i)} for i in range(90)]
        summarizer = Summarizer()
        for _ in range(20):
            summarizer.update(store, limit=8)
        self.assertTrue(all(row.get("summary") for row in store.rows))
        store.rows[0]["prompt"] = "새로 수정된 요청"
        for _ in range(6):
            summarizer.update(store, limit=8)
        self.assertIn("새로 수정된 요청", store.rows[0]["summary"])

    def test_nacho_call_has_no_tools_and_rejects_tool_output(self):
        class Client:
            async def messages(self, **kwargs):
                self_test.assertIsNone(kwargs["tools"])
                self_test.assertIn("인용된 대화 자료", kwargs["system"])
                return {"stop_reason": "tool_use", "content": [{"type": "tool_use"}]}
        self_test = self
        with self.assertRaises(ValueError):
            NachoProvider(Client()).summarize('{"prompt":"요청","student_reports":[]}')

    def test_sqlite_cursor_cache_and_evidence_round_trip(self):
        from tools.request_journal.store import Store as SQLiteStore
        with tempfile.TemporaryDirectory() as directory:
            store = SQLiteStore(Path(directory) / "journal.sqlite3")
            events = []
            for index in range(42):
                events.extend([
                    {"event_key": f"p{index}", "kind": "user", "text": f"메뉴 요청 {index}", "created_at": "2026-09-08T00:00:00Z"},
                    {"event_key": f"f{index}", "kind": "assistant_final", "text": "작업을 마쳤다고 보고합니다.", "created_at": "2026-09-08T00:00:00Z"},
                ])
            store.ingest("source", events, 42, source_meta={"project": "project", "session_id": "session"})
            summarizer = Summarizer()
            for _ in range(10):
                summarizer.update(store, project="project")
            rows = store.list_requests(project="project", limit=100)
            self.assertEqual(len(rows), 42)
            self.assertTrue(all(row["summary"] for row in rows))
            self.assertTrue(all(row["applied_status"] == "unknown" for row in rows))
            self.assertTrue(all(row["summary_evidence"]["provider"] == "structured-fallback" for row in rows))
            self.assertEqual(Summarizer().update(store, project="project")["updated"], 0)


if __name__ == "__main__":
    unittest.main()
