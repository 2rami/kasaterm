from contextlib import closing
import os
from pathlib import Path
import sqlite3
import tempfile
import unittest

from tools.request_journal.runtime import RuntimeObserver
from tools.request_journal.store import Store


class FakeObserver(RuntimeObserver):
    def __init__(self, store, project):
        super().__init__(store, project)
        self.pid = 42
        self.started = "2026-09-08T01:00:00.000Z"
        self.alive = True
        self.listening = True
        self.http = True
        self.build = "abcd1234+"
        self.sha = None
        self.hash_calls = 0

    def _listener_pid(self):
        return self.pid if self.listening and self.alive else None

    def _process(self, pid):
        if self.alive and pid == self.pid:
            return {"pid": pid, "started_at": self.started, "executable": "/fixture/kasaterm",
                    "evidence": {"kind": "os_process", "start_source": "fixture"}}
        return None

    def _version(self):
        if not self.http:
            raise OSError("temporarily offline")
        return {"build": self.build, "version": "0.2.0", "machine_id": "fixture"}

    def _running_component(self, process):
        self.hash_calls += 1
        return {"sha256": self.sha, "confidence": "fixture_verified" if self.sha else "unknown"}

    def _builds(self):
        return [{"status": "legacy_unknown", "source_commit": None}]

    def _git(self, since):
        return {"since": since, "head": "new-git-head", "dirty": True,
                "commits_since_start": [{"commit": "new-git-head", "evidence_kind": "git_commit_only"}]}


class RuntimeTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.project = str((self.root / "project").resolve())
        self.store = Store(self.root / "journal/db.sqlite")
        self.observer = FakeObserver(self.store, self.project)

    def tearDown(self):
        self.tmp.cleanup()

    def request(self, key, created):
        self.store.ingest("source", [{"kind": "user", "event_key": key, "text": key, "created_at": created}], 1,
                          {"project": self.project, "session_id": "session"})
        return self.store.list_requests(limit=1)[0]["id"]

    def test_first_observation_uses_os_start_not_service_start_and_offline_is_not_restart(self):
        self.observer.poll_once()
        first = self.store.runtime_context(self.project)
        run = first["current_run"]
        self.assertEqual(run["started_at"], self.observer.started)
        self.assertNotEqual(run["started_at"], run["first_seen_at"])
        self.assertIsNone(run["linked_build_id"])
        self.observer.http = False
        self.observer.poll_once()
        self.observer.listening = False
        self.observer.poll_once()
        during = self.store.runtime_context(self.project)
        self.assertEqual(during["current_run"]["id"], run["id"])
        self.assertFalse(during["state"]["backend_reachable"])
        self.assertIsNone(during["previous_run"])
        self.observer.listening = self.observer.http = True
        self.observer.poll_once()
        self.assertEqual(self.store.runtime_context(self.project)["last_run"]["id"], run["id"])

    def test_pid_reuse_preserves_previous_window_without_promoting_old_unknowns(self):
        self.request("old unknown", "2026-09-07T01:00:00.000Z")
        explicit = self.request("explicit restart", "2026-09-07T02:00:00.000Z")
        self.store.acknowledge(explicit, "restart_required", "Explicit pending item")
        self.observer.poll_once()
        old = self.store.get_last_run(self.project)
        self.request("during first run", "2026-09-08T01:30:00.000Z")
        self.observer.alive = False
        self.observer.poll_once()
        offline = self.store.runtime_context(self.project)
        self.assertIsNone(offline["current_run"])
        self.assertEqual(offline["last_run"]["id"], old["id"])
        self.observer.alive = True
        self.observer.started = "2026-09-08T02:00:00.000Z"
        self.observer.poll_once()
        new = self.store.runtime_context(self.project)
        self.assertNotEqual(new["current_run"]["id"], old["id"])
        self.assertEqual(new["previous_run"]["id"], old["id"])
        self.assertEqual([r["prompt"] for r in new["previous_run_requests"]], ["during first run"])
        self.assertEqual([r["prompt"] for r in new["carryover"]], ["explicit restart"])
        self.assertEqual(self.store.stats(self.project)["total"], 3)

    def test_runtime_window_has_no_recent_five_hundred_limit(self):
        self.observer.poll_once()
        events = [{"kind": "user", "event_key": str(i), "text": str(i), "created_at": "2026-09-08T01:30:00.000Z"} for i in range(601)]
        self.store.ingest("source", events, 1, {"project": self.project, "session_id": "session"})
        self.assertEqual(len(self.store.runtime_context(self.project)["requests_since_start"]), 601)

    def test_missing_event_time_never_becomes_a_recent_request(self):
        self.observer.poll_once()
        self.store.ingest("source", [{"kind": "user", "event_key": "missing", "text": "unknown age"},
                                     {"kind": "user", "event_key": "invalid", "text": "invalid age", "created_at": "not-a-date"}],
                          1, {"project": self.project, "session_id": "session"})
        context = self.store.runtime_context(self.project)
        self.assertEqual(context["requests_since_start"], [])
        self.assertEqual(context["carryover"], [])
        self.assertEqual(len(context["timestamp_unknown"]), 2)
        self.assertTrue(all(r["created_at"] is None for r in context["timestamp_unknown"]))

    def test_replaced_disk_binary_cannot_rewrite_running_code_evidence(self):
        self.observer.sha = "old-component"
        self.observer.poll_once()
        self.observer.sha = "new-component-at-same-path"
        self.observer.poll_once()
        run = self.store.get_last_run(self.project)
        self.assertEqual(run["component_sha256"], "old-component")
        self.assertEqual(self.observer.hash_calls, 1)
        self.assertEqual(run["build_id"], "abcd1234+")
        self.assertIsNone(run["linked_build_id"])

    def test_os_inode_mismatch_and_post_start_changes_are_not_running_hashes(self):
        binary = self.root / "kasaterm"
        binary.write_bytes(b"old binary")
        inode = binary.stat().st_ino
        process = {"pid": 42, "started_at": "2026-09-08T01:00:00.000Z", "executable": str(binary)}
        output = f"p42\nftxt\ni{inode}\nn{binary}\n"
        self.observer._command = lambda args: output
        replacement = self.root / "replacement"
        replacement.write_bytes(b"new binary")
        replacement.replace(binary)
        result = RuntimeObserver._running_component(self.observer, process)
        self.assertNotIn("sha256", result)
        self.assertEqual(result["reason"], "executable_path_replaced")
        inode = binary.stat().st_ino
        self.observer._command = lambda args: f"p42\nftxt\ni{inode}\nn{binary}\n"
        os.utime(binary, (2000000000, 2000000000))
        self.assertNotIn("sha256", RuntimeObserver._running_component(self.observer, process))

    def test_schema_upgrade_keeps_prompts_and_other_feature_tables(self):
        self.request("immutable older prompt", "2026-09-07T00:00:00.000Z")
        with closing(sqlite3.connect(self.store.db_path)) as db:
            db.executescript("DROP TABLE observations; DROP TABLE runtime_state; DROP TABLE app_runs; DROP TABLE builds; CREATE TABLE chat_fixture(value TEXT); INSERT INTO chat_fixture VALUES('kept'); PRAGMA user_version=0;")
        upgraded = Store(self.store.db_path)
        self.assertEqual(upgraded.list_requests()[0]["prompt"], "immutable older prompt")
        with closing(sqlite3.connect(upgraded.db_path)) as db:
            self.assertEqual(db.execute("SELECT value FROM chat_fixture").fetchone()[0], "kept")
            self.assertEqual(db.execute("PRAGMA user_version").fetchone()[0], 1)
        self.assertIsNone(upgraded.runtime_context(self.project)["last_run"])


if __name__ == "__main__":
    unittest.main()
