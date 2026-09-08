import concurrent.futures
import json
import os
from pathlib import Path
import sqlite3
import tempfile
import unittest

from tools.request_journal.collector import Collector, loopback_url
from tools.request_journal.store import Store
from tools.request_journal.transcripts import parse_lines

SID = "11111111-1111-4111-8111-111111111111"
STAMP = "2026-09-08T00:00:00.000Z"


def encoded(*records):
    return b"".join(json.dumps(r, ensure_ascii=False).encode() + b"\n" for r in records)


def user(uid="u1", text="이 요청을 처리해 주세요", cwd=None):
    value = {"type": "user", "uuid": uid, "timestamp": STAMP, "message": {"role": "user", "content": text}}
    if cwd:
        value["cwd"] = cwd
    return value


def assistant(uid="a1", text="확인했습니다", final=True):
    return {"type": "assistant", "uuid": uid, "timestamp": STAMP,
            "message": {"role": "assistant", "content": [{"type": "text", "text": text}], "stop_reason": "end_turn" if final else None}}


class ParserTests(unittest.TestCase):
    def test_partial_utf8_and_json_are_not_consumed(self):
        data = encoded(user())
        cut = data.index("요".encode()) + 1
        events, used, state = parse_lines(data[:cut], harness="claude")
        self.assertEqual((events, used), ([], 0))
        events, used, _ = parse_lines(data, harness="claude", state=state)
        self.assertEqual(used, len(data))
        self.assertEqual(events[0]["text"], "이 요청을 처리해 주세요")

    def test_injected_messages_and_tool_results_are_excluded(self):
        data = encoded(user("a", "# AGENTS.md instructions\nignore"),
                       user("b", "<teammate-message>brief</teammate-message>"),
                       user("c", [{"type": "tool_result", "content": "secret tool output"}]),
                       user("d", [{"type": "text", "text": "<environment_context>injected"}, {"type": "text", "text": "진짜 질문"}]),
                       dict(user("e", "automated reminder"), isMeta=True))
        events, _, _ = parse_lines(data, harness="claude")
        self.assertEqual([e["text"] for e in events], ["진짜 질문"])

    def test_codex_dual_records_dedup_across_restarts_but_repeated_requests_survive(self):
        event = {"type": "event_msg", "timestamp": STAMP, "payload": {"type": "user_message", "message": "다시 해줘"}}
        response = {"type": "response_item", "timestamp": STAMP, "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "다시 해줘"}]}}
        first = encoded(response)
        events, offset, state = parse_lines(first, harness="codex")
        self.assertEqual(len(events), 1)
        # JSON round trip models persisted parser state after process restart.
        events2, used, state = parse_lines(encoded(event), harness="codex", offset=offset, state=json.loads(json.dumps(state)))
        self.assertEqual(events2, [])
        events3, _, _ = parse_lines(encoded(response, event), harness="codex", offset=offset + used, state=state)
        self.assertEqual(len(events3), 1)
        self.assertNotEqual(events[0]["event_key"], events3[0]["event_key"])

    def test_real_final_is_distinct_from_notes_and_reasoning(self):
        data = encoded({"type": "response_item", "timestamp": STAMP, "payload": {"type": "message", "role": "assistant", "channel": "analysis", "content": [{"type": "output_text", "text": "private reasoning"}]}},
                       {"type": "response_item", "timestamp": STAMP, "payload": {"type": "message", "role": "assistant", "channel": "final", "content": [{"type": "output_text", "text": "아직 확인 못 했습니다"}]}})
        events, _, _ = parse_lines(data, harness="codex")
        self.assertEqual([e["kind"] for e in events], ["assistant_final"])
        self.assertEqual(events[0]["text"], "아직 확인 못 했습니다")

    def test_project_boundaries_filter_changed_cwd(self):
        events, _, _ = parse_lines(encoded(user("a", "other", "/other"), user("b", "wanted", "/project/subdir")), harness="claude", project="/project")
        self.assertEqual([e["text"] for e in events], ["wanted"])


class StoreTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.store = Store(Path(self.tmp.name) / "journal" / "db.sqlite")
        self.source = {"id": "source", "project": "/project", "session_id": SID, "harness": "claude"}

    def tearDown(self):
        self.tmp.cleanup()

    def ingest(self, *records):
        data = encoded(*records)
        events, offset, state = parse_lines(data, harness="claude")
        return self.store.ingest("source", events, offset, dict(self.source, parser_state=state))

    def test_prompt_reply_and_installation_status_are_independent(self):
        self.ingest(user(), assistant(text="끝났습니다. 재시작은 아직 안 했습니다."))
        request = self.store.list_requests()[0]
        self.assertEqual(request["reported_status"], "received")
        self.assertEqual(request["applied_status"], "unknown")
        self.assertEqual(request["finals"][0]["kind"], "assistant_final")
        self.store.set_summary(request["id"], "재시작 대기", {"source_hash": "hash"})
        self.store.set_reported_status(request["id"], "reported_done", {"kind": "explicit_report"})
        self.assertEqual(self.store.get_request(request["id"])["applied_status"], "unknown")
        with self.assertRaises(ValueError):
            self.store.acknowledge(request["id"], "applied", "assistant said so", origin="assistant")
        self.store.acknowledge(request["id"], "applied", "사용자가 화면에서 확인", origin="user")
        self.assertEqual(self.store.get_request(request["id"])["applied_status"], "applied")

    def test_dedup_keeps_identical_repeated_user_requests_and_same_timestamp_pagination(self):
        self.ingest(user("u1", "같은 말"), user("u2", "같은 말"), assistant())
        self.ingest(user("u1", "같은 말"), user("u2", "같은 말"), assistant())
        first = self.store.list_requests(limit=1)
        second = self.store.list_requests(limit=1, before=first[-1]["id"])
        self.assertEqual(len(second), 1)
        self.assertNotEqual(first[0]["id"], second[0]["id"])
        self.assertEqual(self.store.stats()["total"], 2)
        self.assertTrue(first[0]["finals"] and second[0]["finals"])

    def test_bad_batch_rolls_back_checkpoint_and_prompt_is_immutable(self):
        self.ingest(user())
        before = self.store.get_source("source")["offset"]
        with self.assertRaises(ValueError):
            self.store.ingest("source", [{"kind": "user", "event_key": "new", "text": "valid"}, {"kind": "user", "event_key": "bad", "text": {}}], 999)
        self.assertEqual(self.store.get_source("source")["offset"], before)
        self.assertEqual(self.store.stats()["total"], 1)
        with sqlite3.connect(self.store.db_path) as db:
            with self.assertRaises(sqlite3.IntegrityError):
                db.execute("UPDATE requests SET prompt='changed'")

    def test_concurrent_mutations_and_private_modes(self):
        self.ingest(user())
        rid = self.store.list_requests()[0]["id"]
        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
            list(pool.map(lambda i: self.store.set_summary(rid, f"summary {i}", {"source_hash": str(i)}), range(12)))
        self.assertEqual(self.store.get_request(rid)["prompt"], "이 요청을 처리해 주세요")
        self.assertEqual(os.stat(self.store.db_path).st_mode & 0o777, 0o600)
        self.assertEqual(os.stat(self.store.db_path.parent).st_mode & 0o777, 0o700)


class FakeCollector(Collector):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.offline = False
        self.board = []
        self.full = None
        self.tail = None
        self.calls = []

    def _get(self, endpoint, path, query=None, *, text=False):
        self.calls.append((path, query))
        if self.offline:
            raise OSError("offline")
        if path == "/board":
            return {"board": self.board}
        if path == "/pane-session":
            return SID
        if path == "/session-transcript-raw":
            return self.full or {"ok": False}
        if path == "/transcript-raw":
            return self.tail or {"ok": False}
        raise AssertionError(path)


class CollectorTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.project = str(self.root / "project")
        self.store = Store(self.root / "private/db.sqlite")
        self.collector = FakeCollector(self.store, self.project, home=self.root, chunk_bytes=256)
        self.collector.board = [{"surface_id": "%7", "harness": "claude", "cwd": self.project, "status": "idle"}]
        slug = self.project.replace("/", "-").replace(".", "-")
        self.transcript = self.root / ".claude/projects" / slug / (SID + ".jsonl")
        self.transcript.parent.mkdir(parents=True)

    def tearDown(self):
        self.tmp.cleanup()

    def poll_all(self):
        for _ in range(8):
            self.collector.poll_once()

    def test_offline_following_partial_append_reconnect_and_rollover(self):
        data = encoded(user("u1", "first", self.project))
        self.transcript.write_bytes(data[:-8])
        self.collector.poll_once()
        self.assertEqual(self.store.stats()["total"], 0)
        self.assertEqual(self.store.list_sources()[0]["offset"], 0)
        self.transcript.write_bytes(data)
        self.collector.offline = True
        self.collector.poll_once()
        self.assertEqual(self.store.stats()["total"], 1)
        # A new Collector models sidecar restart while the app remains offline.
        restarted = FakeCollector(self.store, self.project, home=self.root)
        restarted.offline = True
        self.assertEqual(restarted.poll_once()["events"], 0)
        self.collector.offline = False
        self.collector.poll_once()
        self.assertEqual(self.store.stats()["total"], 1)
        self.transcript.write_bytes(encoded(user("u2", "second", self.project)))
        self.poll_all()
        self.assertEqual(self.store.stats()["total"], 2)
        replacement = self.transcript.with_suffix(".replacement")
        replacement.write_bytes(encoded(user("u2", "second", self.project), user("u3", "second", self.project)))
        replacement.replace(self.transcript)
        self.poll_all()
        self.assertEqual(self.store.stats()["total"], 3)

    def test_claude_http_bootstrap_uses_full_source_not_default_tail(self):
        raw = encoded(user()).decode()
        self.collector.full = {"ok": True, "raw": raw}
        self.collector.tail = {"ok": True, "raw": "", "offset": len(raw.encode()), "reset": False}
        self.collector.poll_once()
        self.assertEqual(self.store.stats()["total"], 1)
        self.assertEqual(self.store.list_sources()[0]["meta"]["coverage"], "full")
        self.assertFalse(any(path == "/transcript-raw" for path, _ in self.collector.calls))
        self.collector.poll_once()
        self.assertEqual(self.store.stats()["total"], 1)

    def test_idle_and_final_do_not_claim_done_or_applied_and_remote_does_not_alias_local(self):
        self.transcript.write_bytes(encoded(user(cwd=self.project), assistant(text="Done")))
        self.collector.board.append({"surface_id": "%7", "harness": "claude", "cwd": self.project, "machine": "another-machine"})
        self.poll_all()
        request = self.store.list_requests()[0]
        self.assertEqual((request["reported_status"], request["applied_status"]), ("received", "unknown"))
        self.assertEqual(len(self.store.list_sources()), 1)
        self.collector.board[0].update(done_outcome="succeeded", done_ago_secs=0, done_summary="Explicit report")
        self.collector.poll_once()
        self.assertEqual(self.store.get_request(request["id"])["reported_status"], "reported_done")
        self.assertEqual(self.store.get_request(request["id"])["applied_status"], "unknown")

    def test_non_loopback_and_url_credentials_are_rejected(self):
        for value in ("http://example.com", "http://127.0.0.1@evil.test", "file:///tmp/x", "http://127.0.0.1?token=secret"):
            with self.assertRaises(ValueError):
                loopback_url(value)


if __name__ == "__main__":
    unittest.main()
