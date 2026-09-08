import http.client
import json
from pathlib import Path
import tempfile
import threading
import unittest

from tools.request_journal.server import JournalServer
from tools.request_journal.store import Store


class ServerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.project = str((Path(self.temp.name) / "project").resolve())
        self.store = Store(Path(self.temp.name) / "journal.sqlite3")
        result = self.store.ingest("fixture", [
            {"event_key": "user-1", "kind": "user", "text": '<img src=x onerror="alert(1)"> 펫 메뉴를 만들어 주세요', "created_at": "2026-09-08T01:00:00Z"},
            {"event_key": "final-1", "kind": "assistant_final", "text": "학생 보고: 작업을 마쳤습니다", "created_at": "2026-09-08T01:01:00Z"},
        ], 100, {"project": self.project, "session_id": "fixture-session"})
        self.id = result["last_request_id"]
        self.other_id = self.store.ingest("other", [{"event_key": "u", "kind": "user", "text": "other project"}], 20, {"project": "/elsewhere"})["last_request_id"]
        self.server = JournalServer(lambda: self.store, self.project, port=0)
        self.thread = threading.Thread(target=self.server.serve_forever, kwargs={"poll_interval": .01}, daemon=True)
        self.thread.start()

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()
        self.temp.cleanup()

    def call(self, route, method="GET", body=None, headers=None):
        connection = http.client.HTTPConnection("127.0.0.1", self.server.server_port, timeout=2)
        connection.request(method, route, body=body, headers=headers or {})
        response = connection.getresponse()
        raw = response.read()
        result = (response.status, dict(response.getheaders()), raw)
        connection.close()
        return result

    def ack(self, status="applied", evidence="", headers=None):
        return self.call(f"/api/requests/{self.id}/ack", "POST", json.dumps({"applied_status": status, "evidence": evidence}), headers or {"Content-Type": "application/json", "X-Journal-Request": "1"})

    def test_final_report_does_not_confirm_application(self):
        status, _, body = self.call("/api/requests")
        rows = json.loads(body)["requests"]
        self.assertEqual(status, 200)
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["applied_status"], "unknown")
        self.assertEqual(rows[0]["reported_status"], "received")
        self.assertIn("<img", rows[0]["prompt"])
        self.assertEqual(self.ack()[0], 200)
        row = self.store.get_request(self.id)
        self.assertEqual(row["applied_status"], "applied")
        self.assertEqual(row["reported_status"], "received")
        self.assertEqual(self.ack("pending", "화면에는 아직 없습니다")[0], 200)
        status, _, body = self.call(f"/api/requests/{self.id}")
        self.assertEqual(len(json.loads(body)["ack_history"]), 2)

    def test_cross_origin_rebinding_and_non_json_writes_are_blocked(self):
        self.assertEqual(self.call("/health", headers={"Host": "attacker.example"})[0], 403)
        self.assertEqual(self.call("/api/requests", headers={"Origin": "https://attacker.example"})[0], 403)
        self.assertEqual(self.ack(headers={"Content-Type": "application/json", "X-Journal-Request": "1", "Origin": "https://attacker.example"})[0], 403)
        self.assertEqual(self.ack(headers={"Content-Type": "text/plain", "X-Journal-Request": "1"})[0], 415)
        self.assertEqual(self.ack(headers={"Content-Type": "application/json"})[0], 415)
        self.assertEqual(self.store.get_request(self.id)["applied_status"], "unknown")

    def test_scope_validation_and_payload_validation(self):
        self.assertEqual(self.call(f"/api/requests/{self.other_id}")[0], 404)
        self.assertEqual(self.call("/api/requests?project=/elsewhere")[0], 400)
        self.assertEqual(self.ack("restart_required")[0], 400)
        self.assertEqual(self.ack(evidence="x" * 2001)[0], 400)
        self.assertEqual(self.call("/api/requests?limit=invalid")[0], 400)

    def test_same_timestamp_pagination_does_not_drop_requests(self):
        self.store.ingest("fixture", [
            {"event_key": "same-time-a", "kind": "user", "text": "A", "created_at": "2026-09-08T01:00:00Z"},
            {"event_key": "same-time-b", "kind": "user", "text": "B", "created_at": "2026-09-08T01:00:00Z"},
        ], 200)
        _, _, raw = self.call("/api/requests?limit=2")
        first = json.loads(raw)
        _, _, raw = self.call(f"/api/requests?limit=2&before={first['next_before']}")
        second = json.loads(raw)
        ids = [row["id"] for row in first["requests"] + second["requests"]]
        self.assertEqual(len(ids), 3)
        self.assertEqual(len(set(ids)), 3)

    def test_summary_contract_and_static_content_policy(self):
        self.store.set_reported_status(self.id, "reported_done", {"fixture": True})
        status, headers, body = self.call("/api/summary")
        summary = json.loads(body)
        self.assertEqual(status, 200)
        self.assertEqual(summary["version"], 1)
        self.assertEqual(len(summary["needs_confirmation"]), 1)
        self.assertLessEqual(len(summary["text"]), 250)
        self.assertIn("펫 메뉴", summary["text"])
        self.assertIn("펫 메뉴", summary["waiting_text"])
        self.assertTrue(summary["text"].startswith("최근 요청:"))
        self.assertIn("frame-ancestors 'none'", headers["Content-Security-Policy"])
        self.assertNotIn("Access-Control-Allow-Origin", headers)
        self.assertEqual(self.call("/api/ask?q=left")[0], 200)
        self.assertEqual(self.call("/../store.py")[0], 404)
        _, _, source = self.call("/app.js")
        self.assertNotIn(b"innerHTML", source)
        self.assertIn(b"textContent", source)


if __name__ == "__main__":
    unittest.main()
