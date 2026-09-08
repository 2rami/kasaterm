"""Transactional journal storage. Raw prompts are append-only evidence."""

from contextlib import contextmanager
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import tempfile

REPORTED_STATUSES = {"received", "working", "reported_done", "needs_attention"}
APPLIED_STATUSES = {"unknown", "pending", "restart_required", "applied", "not_applicable"}


def utc_now():
    return datetime.now(timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")


def stable_id(*parts):
    return hashlib.sha256(json.dumps(parts, ensure_ascii=False).encode()).hexdigest()


def event_time(value):
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
        if parsed.tzinfo is not None:
            return parsed.astimezone(timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")
    except (AttributeError, ValueError, TypeError, OverflowError):
        pass
    return ""


def default_db_path():
    return Path.home() / ".config/kasaterm/request-journal/journal.sqlite3"


class Store:
    def __init__(self, db_path=None):
        self.db_path = Path(db_path or default_db_path()).expanduser().absolute()
        if self.db_path.parent.resolve() in {Path("/"), Path.home().resolve(), Path(tempfile.gettempdir()).resolve()}:
            raise ValueError("Use a dedicated private journal directory")
        self.db_path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        os.chmod(self.db_path.parent, 0o700)
        if self.db_path.is_symlink():
            raise ValueError("Journal database must not be a symlink")
        fd = os.open(self.db_path, os.O_CREAT | os.O_RDWR, 0o600)
        os.close(fd)
        os.chmod(self.db_path, 0o600)
        with self._connection() as db:
            db.executescript("""
                PRAGMA journal_mode=WAL;
                CREATE TABLE IF NOT EXISTS sources (
                    id TEXT PRIMARY KEY, session_id TEXT NOT NULL DEFAULT '',
                    project TEXT NOT NULL DEFAULT '', harness TEXT NOT NULL DEFAULT '',
                    machine TEXT NOT NULL DEFAULT 'local', path TEXT, endpoint TEXT, pane TEXT,
                    offset INTEGER NOT NULL DEFAULT 0, generation INTEGER NOT NULL DEFAULT 0,
                    parser_state TEXT NOT NULL DEFAULT '{}', meta TEXT NOT NULL DEFAULT '{}',
                    last_request_id TEXT, pending_requests TEXT NOT NULL DEFAULT '[]',
                    updated_at TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS requests (
                    id TEXT PRIMARY KEY, source_id TEXT NOT NULL REFERENCES sources(id),
                    event_key TEXT NOT NULL, session_id TEXT NOT NULL, project TEXT NOT NULL,
                    prompt TEXT NOT NULL, created_at TEXT NOT NULL, received_at TEXT NOT NULL,
                    reported_status TEXT NOT NULL DEFAULT 'received',
                    applied_status TEXT NOT NULL DEFAULT 'unknown', summary TEXT NOT NULL DEFAULT '',
                    summary_evidence TEXT NOT NULL DEFAULT 'null', updated_at TEXT NOT NULL,
                    UNIQUE(source_id, event_key)
                );
                CREATE TRIGGER IF NOT EXISTS immutable_request_prompt
                BEFORE UPDATE OF prompt, event_key, source_id, session_id, project, created_at ON requests
                BEGIN SELECT RAISE(ABORT, 'request evidence is immutable'); END;
                CREATE INDEX IF NOT EXISTS requests_project_time ON requests(project, created_at DESC, id);
                CREATE TABLE IF NOT EXISTS events (
                    id TEXT PRIMARY KEY, source_id TEXT NOT NULL REFERENCES sources(id),
                    event_key TEXT NOT NULL, kind TEXT NOT NULL, text TEXT NOT NULL DEFAULT '',
                    created_at TEXT NOT NULL, evidence TEXT NOT NULL DEFAULT '{}',
                    UNIQUE(source_id, event_key)
                );
                CREATE TABLE IF NOT EXISTS request_finals (
                    request_id TEXT NOT NULL REFERENCES requests(id),
                    event_id TEXT NOT NULL REFERENCES events(id), PRIMARY KEY(request_id,event_id)
                );
                CREATE TABLE IF NOT EXISTS updates (
                    id INTEGER PRIMARY KEY, request_id TEXT NOT NULL REFERENCES requests(id),
                    kind TEXT NOT NULL, value TEXT NOT NULL, evidence TEXT NOT NULL,
                    origin TEXT NOT NULL, created_at TEXT NOT NULL
                );
            """)
            if db.execute("PRAGMA user_version").fetchone()[0] < 1:
                db.executescript("""
                    BEGIN IMMEDIATE;
                    CREATE TABLE IF NOT EXISTS builds (
                        id TEXT PRIMARY KEY, project TEXT NOT NULL, completed_at TEXT NOT NULL,
                        manifest TEXT NOT NULL, observed_at TEXT NOT NULL
                    );
                    CREATE TABLE IF NOT EXISTS app_runs (
                        id TEXT PRIMARY KEY, project TEXT NOT NULL, machine TEXT NOT NULL,
                        pid INTEGER NOT NULL, started_at TEXT NOT NULL, executable TEXT NOT NULL,
                        build_id TEXT, component_sha256 TEXT, linked_build_id TEXT,
                        previous_run_id TEXT REFERENCES app_runs(id),
                        evidence TEXT NOT NULL, first_seen_at TEXT NOT NULL, last_seen_at TEXT NOT NULL,
                        UNIQUE(project,machine,pid,started_at)
                    );
                    CREATE TABLE IF NOT EXISTS runtime_state (
                        project TEXT NOT NULL, machine TEXT NOT NULL,
                        last_run_id TEXT REFERENCES app_runs(id), observed_at TEXT NOT NULL,
                        backend_reachable INTEGER NOT NULL DEFAULT 0, process_alive INTEGER,
                        PRIMARY KEY(project,machine)
                    );
                    CREATE TABLE IF NOT EXISTS observations (
                        id TEXT PRIMARY KEY, project TEXT NOT NULL, machine TEXT NOT NULL,
                        kind TEXT NOT NULL, evidence TEXT NOT NULL,
                        first_seen_at TEXT NOT NULL, observed_at TEXT NOT NULL, sightings INTEGER NOT NULL DEFAULT 1
                    );
                    CREATE INDEX IF NOT EXISTS app_runs_project_time ON app_runs(project,machine,started_at);
                    CREATE INDEX IF NOT EXISTS observations_project_time ON observations(project,machine,observed_at);
                    PRAGMA user_version=1;
                    COMMIT;
                """)
        self._secure_sidecars()

    def _secure_sidecars(self):
        for suffix in ("", "-wal", "-shm"):
            path = Path(str(self.db_path) + suffix)
            if path.exists():
                try:
                    os.chmod(path, 0o600)
                except FileNotFoundError:
                    pass

    @contextmanager
    def _connection(self):
        db = sqlite3.connect(self.db_path, timeout=10)
        db.row_factory = sqlite3.Row
        db.execute("PRAGMA foreign_keys=ON")
        db.execute("PRAGMA busy_timeout=10000")
        try:
            with db:
                yield db
        finally:
            db.close()
            self._secure_sidecars()

    @staticmethod
    def _upsert_source(db, meta):
        source_id = meta.get("id") or meta.get("source_id")
        if not source_id:
            raise ValueError("source id is required")
        db.execute("INSERT OR IGNORE INTO sources(id,updated_at) VALUES(?,?)", (source_id, utc_now()))
        fields = {k: meta[k] for k in ("session_id", "project", "harness", "machine", "path", "endpoint", "pane") if k in meta}
        if fields:
            db.execute("UPDATE sources SET " + ",".join(k + "=?" for k in fields) + " WHERE id=?", (*fields.values(), source_id))
        row = db.execute("SELECT meta FROM sources WHERE id=?", (source_id,)).fetchone()
        existing = json.loads(row[0])
        existing.update(meta.get("meta", {}))
        db.execute("UPDATE sources SET meta=?,updated_at=? WHERE id=?", (json.dumps(existing), utc_now(), source_id))
        return source_id

    def upsert_source(self, meta):
        with self._connection() as db:
            source_id = self._upsert_source(db, meta)
        return self.get_source(source_id)

    @staticmethod
    def _source(row):
        if row is None:
            return None
        result = dict(row)
        for key in ("parser_state", "meta", "pending_requests"):
            result[key] = json.loads(result[key])
        return result

    def get_source(self, source_id):
        with self._connection() as db:
            return self._source(db.execute("SELECT * FROM sources WHERE id=?", (source_id,)).fetchone())

    def list_sources(self):
        with self._connection() as db:
            return [self._source(r) for r in db.execute("SELECT * FROM sources ORDER BY id")]

    def ingest(self, source_id, events, offset, source_meta=None):
        """Commit messages and the consumed byte checkpoint as one transaction.

        Events: {event_key,kind:user|assistant_final|assistant_note|activity,
        text,created_at,evidence}. source_meta may carry parser_state/generation.
        """
        if not isinstance(offset, int) or offset < 0:
            raise ValueError("offset must be a nonnegative byte count")
        meta = dict(source_meta or {}, id=source_id)
        inserted = 0
        with self._connection() as db:
            db.execute("BEGIN IMMEDIATE")
            self._upsert_source(db, meta)
            source = self._source(db.execute("SELECT * FROM sources WHERE id=?", (source_id,)).fetchone())
            last = source["last_request_id"]
            pending = [] if meta.get("generation", source["generation"]) != source["generation"] else source["pending_requests"]
            for event in events:
                key, kind = str(event["event_key"]), event["kind"]
                if kind not in {"user", "assistant_final", "assistant_note", "activity"}:
                    continue
                eid = stable_id(source_id, key)
                text = event.get("text", "")
                if not isinstance(text, str):
                    raise ValueError("event text must be a string")
                stamp = event_time(event.get("created_at"))
                evidence = json.dumps(event.get("evidence", {}), ensure_ascii=False)
                added = db.execute("INSERT OR IGNORE INTO events VALUES(?,?,?,?,?,?,?)", (eid, source_id, key, kind, text, stamp, evidence)).rowcount
                if not added:
                    # Replay can cross a truncation or replacement; existing ids
                    # reestablish attribution without creating a second request.
                    if kind == "user":
                        last = eid
                        closed = db.execute("SELECT 1 FROM request_finals f JOIN events e ON e.id=f.event_id WHERE f.request_id=? AND e.kind='assistant_final' LIMIT 1", (eid,)).fetchone()
                        if not closed and eid not in pending:
                            pending.append(eid)
                    continue
                inserted += 1
                if kind == "user":
                    last = eid
                    pending.append(eid)
                    db.execute("INSERT INTO requests(id,source_id,event_key,session_id,project,prompt,created_at,received_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?)",
                               (eid, source_id, key, source["session_id"], source["project"], text, stamp, utc_now(), utc_now()))
                elif kind in {"assistant_final", "assistant_note"}:
                    for rid in pending or ([last] if last else []):
                        db.execute("INSERT OR IGNORE INTO request_finals VALUES(?,?)", (rid, eid))
                    if kind == "assistant_final":
                        pending = []
                elif kind == "activity" and last:
                    changed = db.execute("UPDATE requests SET reported_status='working',updated_at=? WHERE id=? AND reported_status='received'", (utc_now(), last)).rowcount
                    if changed:
                        db.execute("INSERT INTO updates(request_id,kind,value,evidence,origin,created_at) VALUES(?,?,?,?,?,?)", (last, "reported_status", "working", evidence, "transcript", utc_now()))
            db.execute("UPDATE sources SET offset=?,generation=?,parser_state=?,last_request_id=?,pending_requests=?,updated_at=? WHERE id=?",
                       (offset, meta.get("generation", source["generation"]), json.dumps(meta.get("parser_state", source["parser_state"]), ensure_ascii=False), last, json.dumps(pending), utc_now(), source_id))
        return {"inserted": inserted, "offset": offset, "last_request_id": last}

    @staticmethod
    def _request(db, row):
        if row is None:
            return None
        result = dict(row)
        result["timestamp_known"] = bool(result["created_at"])
        result["created_at"] = result["created_at"] or None
        result["summary_evidence"] = json.loads(result["summary_evidence"])
        result["finals"] = []
        for final in db.execute("SELECT e.* FROM events e JOIN request_finals f ON f.event_id=e.id WHERE f.request_id=? ORDER BY e.created_at,e.rowid", (row["id"],)):
            item = dict(final)
            item["evidence"] = json.loads(item["evidence"])
            result["finals"].append(item)
        result["updates"] = []
        for update in db.execute("SELECT * FROM updates WHERE request_id=? ORDER BY id", (row["id"],)):
            item = dict(update)
            item["evidence"] = json.loads(item["evidence"])
            result["updates"].append(item)
        return result

    def get_request(self, request_id):
        with self._connection() as db:
            return self._request(db, db.execute("SELECT * FROM requests WHERE id=?", (request_id,)).fetchone())

    def list_requests(self, project=None, limit=50, before=None, reported_status=None):
        terms, args = [], []
        for column, value in (("project", project), ("reported_status", reported_status)):
            if value is not None:
                terms.append(column + "=?")
                args.append(value)
        with self._connection() as db:
            if before:
                cursor = db.execute("SELECT created_at,id FROM requests WHERE id=?", (before,)).fetchone()
                if cursor is None:
                    raise ValueError("unknown pagination cursor")
                terms.append("(created_at < ? OR (created_at = ? AND id < ?))")
                args.extend((cursor["created_at"], cursor["created_at"], cursor["id"]))
            query = "SELECT * FROM requests" + (" WHERE " + " AND ".join(terms) if terms else "")
            query += " ORDER BY created_at DESC,id DESC LIMIT ?"
            args.append(max(1, min(int(limit), 500)))
            rows = db.execute(query, args).fetchall()
            return [self._request(db, row) for row in rows]

    def stats(self, project=None):
        where, args = (" WHERE project=?", (project,)) if project is not None else ("", ())
        with self._connection() as db:
            result = {"total": db.execute("SELECT COUNT(*) FROM requests" + where, args).fetchone()[0]}
            for field in ("reported_status", "applied_status"):
                result[field] = dict(db.execute(f"SELECT {field},COUNT(*) FROM requests" + where + f" GROUP BY {field}", args).fetchall())
            result["sources"] = db.execute("SELECT COUNT(*) FROM sources" + where, args).fetchone()[0]
            return result

    def _update(self, request_id, kind, value, evidence, origin):
        with self._connection() as db:
            db.execute("BEGIN IMMEDIATE")
            row = db.execute("SELECT * FROM requests WHERE id=?", (request_id,)).fetchone()
            if row is None:
                raise KeyError(request_id)
            encoded = json.dumps(evidence, ensure_ascii=False)
            if row[kind] == value and (kind != "summary" or row["summary_evidence"] == encoded):
                return self._request(db, row)
            db.execute(f"UPDATE requests SET {kind}=?,updated_at=? WHERE id=?", (value, utc_now(), request_id))
            if kind == "summary":
                db.execute("UPDATE requests SET summary_evidence=? WHERE id=?", (encoded, request_id))
            db.execute("INSERT INTO updates(request_id,kind,value,evidence,origin,created_at) VALUES(?,?,?,?,?,?)",
                       (request_id, kind, value, encoded, origin, utc_now()))
            return self._request(db, db.execute("SELECT * FROM requests WHERE id=?", (request_id,)).fetchone())

    def set_summary(self, request_id, summary, evidence=None):
        if not isinstance(summary, str) or len(summary) > 2000:
            raise ValueError("summary must be a short string")
        return self._update(request_id, "summary", summary, evidence, "summary")

    def set_reported_status(self, request_id, status, evidence, origin="manual"):
        if status not in REPORTED_STATUSES or not evidence:
            raise ValueError("reported status requires a valid status and evidence")
        return self._update(request_id, "reported_status", status, evidence, origin)

    def acknowledge(self, request_id, applied_status, evidence, origin="user"):
        if applied_status not in APPLIED_STATUSES or not evidence:
            raise ValueError("application status requires a valid status and evidence")
        if origin not in {"user", "verified_build"}:
            raise ValueError("only user confirmation or verified build evidence can acknowledge application")
        return self._update(request_id, "applied_status", applied_status, evidence, origin)

    def record_build(self, manifest):
        if not isinstance(manifest, dict) or manifest.get("success") is not True or not isinstance(manifest.get("signature"), dict) or manifest["signature"].get("verified") is not True:
            raise ValueError("Only a completed, signature-verified bundle is a ready build")
        for key in ("id", "project", "completed_at", "components"):
            if not manifest.get(key):
                raise ValueError("Build evidence is incomplete")
        with self._connection() as db:
            db.execute("INSERT OR IGNORE INTO builds VALUES(?,?,?,?,?)", (manifest["id"], manifest["project"], manifest["completed_at"], json.dumps(manifest, ensure_ascii=False), utc_now()))
        return dict(manifest)

    def list_builds(self, project=None, since=None):
        terms, args = [], []
        if project:
            terms.append("project=?"); args.append(project)
        if since:
            terms.append("completed_at>=?"); args.append(since)
        query = "SELECT manifest FROM builds" + (" WHERE " + " AND ".join(terms) if terms else "") + " ORDER BY completed_at DESC,id DESC"
        with self._connection() as db:
            return [json.loads(row[0]) for row in db.execute(query, args)]

    @staticmethod
    def _run(row):
        if row is None:
            return None
        result = dict(row)
        result["evidence"] = json.loads(result["evidence"])
        return result

    def record_app_run(self, run):
        required = ("project", "machine", "pid", "started_at", "executable", "evidence")
        if any(not run.get(key) for key in required) or not isinstance(run["pid"], int) or run["pid"] < 1:
            raise ValueError("An app run requires its actual OS process identity")
        started = datetime.fromisoformat(run["started_at"].replace("Z", "+00:00"))
        if started.tzinfo is None:
            raise ValueError("OS start time must have a timezone")
        stamp = started.astimezone(timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")
        rid = stable_id(run["project"], run["machine"], run["pid"], stamp)
        observed = utc_now()
        with self._connection() as db:
            db.execute("BEGIN IMMEDIATE")
            previous = db.execute("SELECT last_run_id FROM runtime_state WHERE project=? AND machine=?", (run["project"], run["machine"])).fetchone()
            previous_id = previous[0] if previous and previous[0] != rid else None
            db.execute("INSERT OR IGNORE INTO app_runs VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)",
                       (rid, run["project"], run["machine"], run["pid"], stamp, run["executable"], run.get("build_id"),
                        run.get("component_sha256"), run.get("linked_build_id"), previous_id,
                        json.dumps(run["evidence"], ensure_ascii=False), observed, observed))
            # The file at an executable path may have been replaced since launch.
            # Once observed, running-code evidence is never overwritten by it.
            db.execute("UPDATE app_runs SET last_seen_at=?,build_id=COALESCE(build_id,?),component_sha256=COALESCE(component_sha256,?),linked_build_id=COALESCE(linked_build_id,?) WHERE id=?",
                       (observed, run.get("build_id"), run.get("component_sha256"), run.get("linked_build_id"), rid))
            db.execute("INSERT INTO runtime_state VALUES(?,?,?,?,?,?) ON CONFLICT(project,machine) DO UPDATE SET last_run_id=excluded.last_run_id,observed_at=excluded.observed_at,backend_reachable=excluded.backend_reachable,process_alive=excluded.process_alive",
                       (run["project"], run["machine"], rid, observed, int(run.get("backend_reachable", True)), 1))
            return self._run(db.execute("SELECT * FROM app_runs WHERE id=?", (rid,)).fetchone())

    def record_observation(self, kind, evidence, machine="local", project=None):
        if not isinstance(evidence, dict):
            raise ValueError("Observation evidence must be an object")
        project = project or evidence.get("project")
        if not project:
            raise ValueError("Observation must be project-scoped")
        value = {k: v for k, v in evidence.items() if k != "observed_at"}
        oid = stable_id(project, machine, kind, value)
        observed = evidence.get("observed_at") or utc_now()
        with self._connection() as db:
            db.execute("BEGIN IMMEDIATE")
            db.execute("INSERT INTO observations VALUES(?,?,?,?,?,?,?,1) ON CONFLICT(id) DO UPDATE SET observed_at=excluded.observed_at,sightings=observations.sightings+1",
                       (oid, project, machine, kind, json.dumps(value, ensure_ascii=False), observed, observed))
            if kind == "runtime_unavailable":
                # Losing HTTP alone is not proof that the app stopped.
                alive = evidence.get("process_alive")
                db.execute("INSERT INTO runtime_state VALUES(?,?,NULL,?,0,?) ON CONFLICT(project,machine) DO UPDATE SET observed_at=excluded.observed_at,backend_reachable=0,process_alive=excluded.process_alive",
                           (project, machine, observed, None if alive is None else int(alive)))
        return oid

    def runtime_context(self, project=None, machine="local"):
        if not project:
            raise ValueError("Runtime context requires a project")
        with self._connection() as db:
            state_row = db.execute("SELECT * FROM runtime_state WHERE project=? AND machine=?", (project, machine)).fetchone()
            state = dict(state_row) if state_row else None
            last = self._run(db.execute("SELECT * FROM app_runs WHERE id=?", (state["last_run_id"],)).fetchone()) if state and state["last_run_id"] else None
            previous = self._run(db.execute("SELECT * FROM app_runs WHERE id=?", (last["previous_run_id"],)).fetchone()) if last and last["previous_run_id"] else None
            latest = db.execute("SELECT * FROM observations WHERE project=? AND machine=? ORDER BY observed_at DESC,rowid DESC LIMIT 1", (project, machine)).fetchone()
            observation = dict(latest) if latest else None
            if observation:
                observation["evidence"] = json.loads(observation["evidence"])
            def requests(condition, args):
                rows = db.execute("SELECT * FROM requests WHERE project=? AND " + condition + " ORDER BY created_at,id", (project, *args)).fetchall()
                return [self._request(db, row) for row in rows]
            since = requests("created_at>=?", (last["started_at"],)) if last else []
            carry = requests("created_at<>'' AND created_at<? AND applied_status IN ('pending','restart_required')", (last["started_at"],)) if last else requests("created_at<>'' AND applied_status IN ('pending','restart_required')", ())
            unknown_time = requests("created_at=''", ())
            previous_requests = requests("created_at>=? AND created_at<?", (previous["started_at"], last["started_at"])) if previous else []
            builds = [json.loads(row[0]) for row in db.execute("SELECT manifest FROM builds WHERE project=? ORDER BY completed_at DESC,id DESC", (project,))]
            return {"current_run": last if state and state["process_alive"] == 1 else None,
                    "last_run": last, "previous_run": previous, "state": state,
                    "requests_since_start": since, "carryover": carry,
                    "timestamp_unknown": unknown_time,
                    "previous_run_requests": previous_requests, "builds": builds,
                    "latest_observation": observation}

    def get_last_run(self, project, machine="local"):
        with self._connection() as db:
            return self._run(db.execute("SELECT r.* FROM app_runs r JOIN runtime_state s ON s.last_run_id=r.id WHERE s.project=? AND s.machine=?", (project, machine)).fetchone())
