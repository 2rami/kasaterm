import copy
import http.client
import json
import re
from pathlib import Path
import tempfile
import threading
import time
import unittest

from tools.request_journal.chat import ChatManager, ChatStore
from tools.request_journal.checklist import apply_semantic, context_hash, make_checklist, semantic_batches
from tools.request_journal.server import JournalServer
from tools.request_journal.server import chat_job_view, checklist_page
from tools.request_journal.store import Store


def request(index, prompt=None, **extra):
    return {"id": f"request-{index}", "prompt": prompt or f"펫 기능 {index}를 확인해 주세요", "source_id": "source", "created_at": f"2026-09-08T14:{index:04d}:00Z", "updated_at": "unchanged", "applied_status": "unknown", "reported_status": "working", "finals": [], **extra}


def context(rows=None):
    return {"current_run": {"id": "run-1", "pid": 88156, "started_at": "2026-09-08T04:13:51.000Z", "last_seen_at": "one"}, "last_run": None, "previous_run": None, "requests_since_start": rows or [], "carryover": [], "builds": []}


class ContextStore(Store):
    def runtime_context(self, **_kwargs):
        return copy.deepcopy(self.fixture)

    def get_request(self, request_id):
        return next((copy.deepcopy(row) for row in getattr(self, "archived", []) if row["id"] == request_id), None) or super().get_request(request_id)


class JSONProvider:
    name = "nacho-http"

    def __init__(self):
        self.inputs = []

    def complete(self, serialized):
        self.inputs.append(serialized)
        data = json.loads(serialized)
        if data["mode"] == "chat":
            return "현재 실행본과 새 빌드를 구분해서 답했습니다."
        items = [{"title": "펫 메뉴 확인", "steps": ["펫을 우클릭해 요청한 메뉴가 보이는지 확인합니다."], "source_request_ids": [row["request_id"]], "evidence_ids": []} for row in data.get("requests", [])]
        for partial in data.get("partials", []):
            items.extend(partial.get("items", []))
        return json.dumps({"text": "학생 보고와 실제 반영은 별도입니다.", "items": items}, ensure_ascii=False)


class ChecklistTests(unittest.TestCase):
    def test_short_followup_only_joins_the_same_source(self):
        a = request(1, "펫 우클릭 메뉴 추가", source_id="student-a")
        b = request(2, "폰 화면 수정", source_id="student-b")
        c = request(3, "우클릭 메뉴에", source_id="student-a")
        checks = make_checklist(context([a, b, c]))
        self.assertEqual(checks["items"][0]["source_request_ids"], ["request-1", "request-3"])
        self.assertEqual(checks["items"][1]["source_request_ids"], ["request-2"])
        parts = [part for batch in semantic_batches(context([a, b, c])) for part in batch]
        followup = next(part for part in parts if part["request_id"] == "request-3")
        self.assertEqual(followup["previous_request_id"], "request-1")
        self.assertEqual(followup["source_id"], "student-a")

    def test_missing_request_time_is_not_classified_as_this_app_run(self):
        snapshot = context([request(1)])
        snapshot["timestamp_unknown"] = [request(2, created_at=None)]
        checks = make_checklist(snapshot)
        self.assertEqual(checks["context"]["since_start_count"], 1)
        self.assertEqual(checks["context"]["timestamp_unknown_count"], 1)
        item = next(item for item in checks["items"] if "request-2" in item["source_request_ids"])
        self.assertIn("단정할 수 없습니다", item["context_notes"][0])

    def test_os_epoch_cache_and_every_request_part_are_preserved(self):
        rows = [request(index) for index in range(601)]
        rows[-1]["prompt"] = "처음" + "한" * 17000 + "마지막"
        snapshot = context(rows)
        batches = list(semantic_batches(snapshot))
        parts = [part for batch in batches for part in batch]
        self.assertEqual({row["id"] for row in rows}, {part["request_id"] for part in parts})
        self.assertEqual("".join(part["text"] for part in parts if part["request_id"] == rows[-1]["id"]), rows[-1]["prompt"])
        self.assertTrue(all(len(json.dumps(batch, ensure_ascii=False)) < 6000 for batch in batches))
        changed = copy.deepcopy(snapshot)
        changed["current_run"]["last_seen_at"] = "later service poll"
        self.assertEqual(context_hash(snapshot), context_hash(changed))
        changed["current_run"]["id"] = "run-2"
        self.assertNotEqual(context_hash(snapshot), context_hash(changed))

    def test_code_build_evidence_is_separate_from_unmapped_request(self):
        snapshot = context([request(1), request(2, "ㄱㄱ")])
        snapshot["builds"] = [{"id": "build-1", "success": True, "signature": {"verified": True}, "completed_at": "2026-09-08T06:00:00Z", "source": {"status": "stable_dirty", "source_commit": None}}]
        snapshot["git_changes"] = [{"id": "commit:abc", "title": "펫 메뉴 추가", "build_ids": ["build-1"], "source_exact": False}]
        snapshot["latest_observation"] = {"evidence": {"artifacts": [{"build_id": "build-1", "status": "verified_ready"}]}}
        checks = make_checklist(snapshot)
        self.assertEqual(checks["items"][0]["buildstate"], "built_not_running")
        self.assertTrue(checks["items"][0]["uncertain"])
        self.assertEqual(checks["items"][1]["buildstate"], "implementation_unverified")
        self.assertEqual(checks["items"][1]["source_request_ids"], ["request-1", "request-2"])
        forged = [{"title": "적용 완료", "steps": ["끝"], "source_request_ids": ["invented"], "evidence_ids": ["fake-build"]}]
        self.assertEqual(apply_semantic(checks, forged, {"request-1", "request-2"}, {"build-1", "commit:abc"})["items"], checks["items"])
        snapshot["latest_observation"]["evidence"]["artifacts"][0]["status"] = "unverified"
        self.assertEqual(make_checklist(snapshot)["items"][0]["buildstate"], "build_unverified")


class ChatTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.project = str(Path(self.temp.name).resolve())
        self.store = ContextStore(Path(self.temp.name) / "journal.sqlite3")
        self.store.fixture = context([request(1), request(2)])
        self.managers = []

    def tearDown(self):
        for manager in self.managers:
            manager.close()
        self.temp.cleanup()

    def manager(self, factory=None, **options):
        manager = ChatManager(self.store, self.project, factory, **options)
        self.managers.append(manager)
        return manager

    def wait(self, manager, job):
        for _ in range(300):
            result = manager.chat.get(self.project, job["id"])
            if result["status"] in ("completed", "failed", "cancelled"):
                return result
            time.sleep(.02)
        self.fail("job did not finish")

    def test_json_string_provider_items_cache_and_history(self):
        provider = JSONProvider()
        manager = self.manager(lambda _cancel: provider)
        job = manager.submit("재시작하면 뭐 확인해?", client_id="first")
        duplicate = manager.submit("재시작하면 뭐 확인해?", client_id="first")
        self.assertEqual(job["id"], duplicate["id"])
        done = self.wait(manager, job)
        self.assertEqual(done["status"], "completed")
        self.assertIn("펫 메뉴 확인", [item["title"] for item in done["checklist"]["items"]])
        self.assertEqual(set(done["checklist"]["coverage"]["semantic_request_ids"]), {"request-1", "request-2"})
        call_count = len(provider.inputs)
        self.wait(manager, manager.submit("재시작 확인", client_id="second"))
        self.assertEqual(len(provider.inputs), call_count)
        self.assertEqual(len(manager.chat.history(self.project, "pet")), 4)
        self.assertTrue(all(len(value) <= 12000 for value in provider.inputs))
        self.assertEqual(ChatStore(self.store.db_path).history(self.project, "pet"), manager.chat.history(self.project, "pet"))

    def test_partial_failure_keeps_all_requests_in_fallback(self):
        class Broken:
            name = "nacho-http"
            def complete(self, _payload):
                return "not valid JSON"
        self.store.fixture = context([request(index) for index in range(620)])
        manager = self.manager(lambda _cancel: Broken())
        done = self.wait(manager, manager.submit("재시작 확인"))
        self.assertTrue(done["partial"])
        self.assertEqual(len(done["checklist"]["coverage"]["covered_request_ids"]), 620)
        self.assertEqual(len(done["checklist"]["coverage"]["fallback_request_ids"]), 620)

    def test_many_json_string_batches_preserve_all_request_coverage(self):
        provider = JSONProvider()
        self.store.fixture = context([request(index) for index in range(605)])
        manager = self.manager(lambda _cancel: provider)
        done = self.wait(manager, manager.submit("재시작 확인"))
        expected = {f"request-{index}" for index in range(605)}
        self.assertEqual(set(done["checklist"]["coverage"]["semantic_request_ids"]), expected)
        self.assertEqual({key for item in done["checklist"]["items"] for key in item["source_request_ids"]}, expected)
        self.assertTrue(all(len(value) <= 12000 for value in provider.inputs))

    def test_previous_generated_checks_survive_new_epoch_until_confirmed(self):
        self.store.fixture = context([request(1)])
        self.store.archived = [request(1)]
        manager = self.manager()
        self.wait(manager, manager.submit("재시작 확인"))
        self.store.fixture = context([request(2)])
        self.store.fixture["current_run"]["id"] = "run-2"
        self.assertEqual([row["id"] for row in manager.context()["carryover"]], ["request-1"])
        for _ in range(3):
            self.wait(manager, manager.submit("재시작 확인"))
            self.assertEqual([row["id"] for row in manager.context()["carryover"]], ["request-1"])
        manager.close()
        restarted = self.manager()
        self.assertEqual([row["id"] for row in restarted.context()["carryover"]], ["request-1"])
        self.store.archived[0]["applied_status"] = "applied"
        self.assertEqual(restarted.context()["carryover"], [])

    def test_native_history_always_contains_real_checklist_without_provider(self):
        manager = self.manager()
        done = self.wait(manager, manager.submit("재시작 확인"))
        body = "\n".join(row["text"] for row in manager.chat.history(self.project, "pet") if row["role"] == "assistant")
        self.assertIn("펫 기능 1", body)
        self.assertIn("실제 수정·빌드 근거", body)
        self.assertIn("재시작 확인 목록", body)
        self.assertIn("source_request_ids", json.dumps(done["checklist"]))

    def test_long_utf8_job_history_and_checklist_pages_stay_bounded_without_loss(self):
        self.store.fixture = context([request(index, "펫 긴 요청 " + "한" * 500) for index in range(620)])
        manager = self.manager()
        done = self.wait(manager, manager.submit("재시작 확인"))
        self.assertLess(len(json.dumps(chat_job_view(done), ensure_ascii=False).encode()), 1024 * 1024)
        self.assertNotIn("checklist", chat_job_view(done))
        after, messages, pages = done["user_message_id"] - 1, [], 0
        while True:
            pages += 1
            page = manager.chat.history_page(self.project, "pet", after=after)
            self.assertLess(len(json.dumps(page, ensure_ascii=False).encode()), 1024 * 1024)
            messages.extend(page["messages"])
            if page["next_after"] is None: break
            after = page["next_after"]
        reconstructed = "".join(re.sub(r"^답변 \d+/\d+\n", "", row["text"]) for row in messages if row["role"] == "assistant")
        self.assertEqual(reconstructed, done["text"])
        self.assertGreater(pages, 1)
        offset, delivered = 0, []
        while True:
            page = checklist_page(done["checklist"], offset)
            self.assertLess(len(json.dumps(page, ensure_ascii=False).encode()), 1024 * 1024)
            delivered.extend(page["items"])
            if page["next_offset"] is None: break
            offset = page["next_offset"]
        self.assertEqual([item["id"] for item in delivered], [item["id"] for item in done["checklist"]["items"]])

    def test_async_http_cancel_does_not_block_health(self):
        started = threading.Event()
        class Slow:
            name = "nacho-http"
            def __init__(self, cancel): self.cancel = cancel
            def complete(self, _payload):
                started.set(); self.cancel.wait(2); raise InterruptedError()
        manager = self.manager(Slow)
        server = JournalServer(lambda: self.store, self.project, port=0)
        server.chat = manager
        worker = threading.Thread(target=server.serve_forever, kwargs={"poll_interval": .01}, daemon=True)
        worker.start()
        def call(method, path, body=None):
            conn = http.client.HTTPConnection("127.0.0.1", server.server_port, timeout=1)
            conn.request(method, path, body=body.encode() if isinstance(body, str) else body, headers={"Content-Type":"application/json", "X-Journal-Request":"1"})
            reply = conn.getresponse(); result = reply.status, json.loads(reply.read()); conn.close(); return result
        try:
            before = time.monotonic()
            status, job = call("POST", "/api/chat", '{"text":"재시작 확인"}')
            self.assertEqual(status, 202)
            self.assertLess(time.monotonic() - before, .5)
            self.assertTrue(started.wait(1))
            self.assertEqual(call("GET", "/health")[0], 200)
            status, cancelled = call("DELETE", "/api/chat/jobs/" + job["job_id"], "{}")
            self.assertEqual(cancelled["status"], "cancelled")
            self.assertEqual(len(manager.chat.history(self.project, "pet")), 1)
        finally:
            server.shutdown(); server.server_close(); worker.join()

    def test_time_budget_cancels_provider_and_preserves_default_checks(self):
        class Slow:
            name = "nacho-http"
            def __init__(self, cancel): self.cancel = cancel
            def complete(self, _payload):
                self.cancel.wait(2)
                raise InterruptedError()
        manager = self.manager(Slow, max_seconds=.05)
        done = self.wait(manager, manager.submit("재시작 확인"))
        self.assertEqual(done["status"], "failed")
        self.assertEqual(done["error"], "chat_time_budget")
        self.assertEqual(done["checklist"]["coverage"]["total_requests"], 2)
        self.assertIn("펫 기능 1", done["text"])
        self.assertIn("실제 수정·빌드 근거", done["text"])


if __name__ == "__main__":
    unittest.main()
