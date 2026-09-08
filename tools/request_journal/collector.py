"""Independent, read-only transcript discovery and incremental collection."""

from datetime import datetime, timedelta, timezone
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import re
import sqlite3
import urllib.parse
import urllib.request
import uuid

from .store import stable_id, utc_now
from .transcripts import parse_lines


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise ValueError("Journal discovery does not follow redirects")


def loopback_url(value):
    parsed = urllib.parse.urlsplit(value)
    if parsed.scheme not in {"http", "https"} or parsed.username or parsed.password or parsed.query or parsed.fragment:
        raise ValueError("Expected a loopback HTTP base URL")
    host = parsed.hostname or ""
    if host != "localhost":
        try:
            if not ipaddress.ip_address(host).is_loopback:
                raise ValueError("Remote connections require an explicit local tunnel")
        except ValueError as exc:
            raise ValueError("Expected a loopback host") from exc
    return value.rstrip("/")


def _time(value):
    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00"))
    except (AttributeError, ValueError):
        return None


class Collector:
    def __init__(self, store, project, base_url="http://127.0.0.1:8765", *, home=None,
                 remote_endpoints=None, remote_projects=None, chunk_bytes=4 * 1024 * 1024):
        self.store = store
        self.project = str(Path(project).expanduser().resolve())
        self.base_url = loopback_url(base_url)
        self.home = Path(home or Path.home()).expanduser()
        self.remote_endpoints = {k: loopback_url(v) for k, v in (remote_endpoints or {}).items()}
        self.remote_projects = dict(remote_projects or {})
        self.chunk_bytes = max(128, int(chunk_bytes))
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())

    def _get(self, endpoint, path, query=None, *, text=False):
        url = loopback_url(endpoint) + path
        if query:
            url += "?" + urllib.parse.urlencode(query)
        with self.opener.open(url, timeout=5) as response:
            body = response.read(64 * 1024 * 1024 + 1)
        if len(body) > 64 * 1024 * 1024:
            raise ValueError("Response exceeds journal read limit")
        decoded = body.decode("utf-8")
        return decoded if text else json.loads(decoded)

    @staticmethod
    def _in_project(cwd, project):
        try:
            return os.path.commonpath([os.path.realpath(cwd), os.path.realpath(project)]) == os.path.realpath(project)
        except (TypeError, ValueError):
            return False

    def _allowed_path(self, candidate, harness):
        candidate = Path(candidate).expanduser()
        roots = [self.home / ".claude/projects"] if harness == "claude" else [self.home / ".codex/sessions", self.home / ".codex/archived_sessions"]
        try:
            resolved = candidate.resolve(strict=True)
            if resolved.is_file() and any(resolved.is_relative_to(root.resolve()) for root in roots):
                return str(resolved)
        except (OSError, ValueError):
            pass
        return None

    def _local_path(self, harness, session_id, cwd, supplied=None):
        if not re.fullmatch(r"[a-fA-F0-9-]{36}", session_id):
            return None
        if supplied:
            allowed = self._allowed_path(supplied, harness)
            if allowed and session_id in Path(allowed).name:
                return allowed
        if harness == "claude":
            slug = cwd.replace("/", "-").replace(".", "-")
            return self._allowed_path(self.home / ".claude/projects" / slug / (session_id + ".jsonl"), harness)
        # Query only the known active session, never enumerate every rollout.
        for database in sorted((self.home / ".codex").glob("state_*.sqlite"), reverse=True):
            try:
                connection = sqlite3.connect(database.resolve().as_uri() + "?mode=ro", uri=True, timeout=1)
                try:
                    row = connection.execute("SELECT rollout_path FROM threads WHERE id=?", (session_id,)).fetchone()
                finally:
                    connection.close()
                if row and row[0]:
                    candidate = self._allowed_path(row[0], harness)
                    if candidate and session_id in Path(candidate).name:
                        return candidate
                    parts = Path(row[0]).parts
                    if "sessions" in parts:
                        suffix = parts[parts.index("sessions") + 1:]
                        candidate = self._allowed_path(self.home / ".codex/sessions" / Path(*suffix), harness)
                        if candidate and session_id in Path(candidate).name:
                            return candidate
            except sqlite3.Error:
                continue
        try:
            identifier = uuid.UUID(session_id)
            day = datetime.fromtimestamp((identifier.int >> 80) / 1000, timezone.utc) if identifier.version == 7 else datetime.now(timezone.utc)
        except (ValueError, OverflowError, OSError):
            return None
        for delta in (0, -1, 1):
            folder = self.home / ".codex/sessions" / (day + timedelta(days=delta)).strftime("%Y/%m/%d")
            for candidate in folder.glob(f"rollout-*{session_id}.jsonl"):
                allowed = self._allowed_path(candidate, harness)
                if allowed:
                    return allowed
        return None

    def discover(self):
        data = self._get(self.base_url, "/board")
        rows = data.get("board", []) if isinstance(data, dict) else []
        discovered, board, errors = 0, {}, []
        for row in rows:
            if not isinstance(row, dict) or row.get("detached"):
                continue
            harness = (row.get("harness") or "").lower()
            if harness not in {"claude", "codex"}:
                continue
            machine = row.get("machine") or "local"
            scope = self.remote_projects.get(machine, self.project)
            cwd = row.get("cwd") or ""
            if not self._in_project(cwd, scope):
                continue
            endpoint = self.base_url if machine == "local" else self.remote_endpoints.get(machine)
            if not endpoint:
                errors.append({"kind": "remote_endpoint_unconfigured", "machine": machine})
                continue
            pane = row.get("surface_id")
            if not isinstance(pane, str) or not pane:
                continue
            try:
                sid = self._get(endpoint, "/pane-session", {"pane": pane}, text=True).strip()
                if not re.fullmatch(r"[a-fA-F0-9-]{36}", sid):
                    continue
                source_id = stable_id(machine, harness, sid, self.project)
                old = self.store.get_source(source_id)
                meta = {"id": source_id, "session_id": sid, "project": self.project,
                        "harness": harness, "machine": machine, "endpoint": endpoint, "pane": pane,
                        "meta": {"cwd": cwd, "scope_cwd": scope, "discovered_at": utc_now(),
                                 "character": row.get("character"), "peer_name": row.get("peer_name")}}
                if machine == "local":
                    local = self._local_path(harness, sid, cwd, row.get("transcript_path"))
                    if local:
                        meta["path"] = local
                self.store.upsert_source(meta)
                board[source_id] = row
                discovered += int(old is None)
            except (OSError, ValueError, sqlite3.Error):
                errors.append({"kind": "session_discovery_failed", "machine": machine, "pane": pane})
        return discovered, board, errors

    def _local(self, source):
        path = self._allowed_path(source["path"], source["harness"])
        if not path:
            raise FileNotFoundError("Discovered transcript is unavailable")
        offset, generation = source["offset"], source["generation"]
        parser_state = source["parser_state"]
        old = source["meta"]
        with open(path, "rb") as stream:
            stat = os.fstat(stream.fileno())
            prefix = stream.read(256)
            previous_length = int(old.get("prefix_length", 0))
            changed_prefix = previous_length and hashlib.sha256(prefix[:previous_length]).hexdigest() != old.get("prefix_hash")
            inode = f"{stat.st_dev}:{stat.st_ino}"
            reset = stat.st_size < offset or changed_prefix or (old.get("inode") and old["inode"] != inode)
            # A local file can replace an HTTP tail source; reread it from its
            # beginning rather than preserving a permanently incomplete history.
            reset = reset or (old.get("coverage") == "partial_tail")
            if reset:
                offset, generation, parser_state = 0, generation + 1, {}
            stream.seek(offset)
            data = stream.read(self.chunk_bytes)
            if data and b"\n" not in data:
                data += stream.readline(max(0, 16 * 1024 * 1024 - len(data)))
                if b"\n" not in data and len(data) >= 16 * 1024 * 1024:
                    raise ValueError("Transcript line exceeds parsing limit")
        events, consumed, state = parse_lines(data, harness=source["harness"], offset=offset, generation=generation, state=parser_state,
                                              project=old.get("scope_cwd", self.project))
        next_offset = offset + consumed
        meta = {"generation": generation, "parser_state": state, "meta": {
            "prefix_length": len(prefix), "prefix_hash": hashlib.sha256(prefix).hexdigest(), "inode": inode,
            "coverage": "full", "backfill_complete": next_offset >= stat.st_size,
            "partial_line": consumed < len(data), "last_read_at": utc_now()}}
        return self.store.ingest(source["id"], events, next_offset, meta)

    def _http(self, source):
        endpoint, offset, generation = source["endpoint"], source["offset"], source["generation"]
        state = source["parser_state"]
        coverage = source["meta"].get("coverage", "partial_tail")
        response = None
        if offset == 0 and source["harness"] == "claude":
            try:
                full = self._get(endpoint, "/session-transcript-raw", {"id": source["session_id"], "cwd": source["meta"]["cwd"]})
                if isinstance(full, dict) and full.get("ok") and isinstance(full.get("raw"), str):
                    raw = full["raw"].encode()
                    response = {"ok": True, "raw": full["raw"], "offset": len(raw), "reset": False}
                    coverage = "full"
            except (OSError, ValueError):
                pass
        if response is None:
            response = self._get(endpoint, "/transcript-raw", {"surface": source["pane"], "offset": offset})
        if not isinstance(response, dict) or not response.get("ok") or not isinstance(response.get("raw"), str):
            raise ValueError("Transcript endpoint did not return data")
        raw = response["raw"].encode()
        end = response.get("offset")
        if not isinstance(end, int) or end < len(raw):
            raise ValueError("Invalid transcript byte checkpoint")
        start = end - len(raw)
        if response.get("reset"):
            coverage = "partial_tail"
            if offset:
                generation += 1
            state = {}
        events, consumed, state = parse_lines(raw, harness=source["harness"], offset=start, generation=generation, state=state,
                                              project=source["meta"].get("scope_cwd", self.project))
        return self.store.ingest(source["id"], events, start + consumed, {"generation": generation, "parser_state": state,
                                 "meta": {"coverage": coverage, "backfill_complete": coverage == "full", "partial_line": consumed < len(raw), "last_read_at": utc_now()}})

    def _board_evidence(self, source_id, row):
        source = self.store.get_source(source_id)
        request = self.store.get_request(source["last_request_id"]) if source["last_request_id"] else None
        if request is None:
            return
        evidence = {"kind": "board_observation", "observed_at": utc_now(), "surface_id": source["pane"], "machine": source["machine"]}
        outcome, age = row.get("done_outcome"), row.get("done_ago_secs")
        if outcome in {"succeeded", "failed"} and isinstance(age, (int, float)) and age >= 0:
            reported_at = datetime.now(timezone.utc) - timedelta(seconds=age)
            created = _time(request["created_at"])
            if created is not None and created <= reported_at:
                evidence.update(done_outcome=outcome, done_summary=row.get("done_summary"), reported_at=reported_at.isoformat())
                self.store.set_reported_status(request["id"], "reported_done" if outcome == "succeeded" else "needs_attention", evidence, origin="board")
                return
        status = row.get("status")
        if status in {"working", "building", "waiting", "blocked"}:
            evidence["status"] = status
            self.store.set_reported_status(request["id"], "needs_attention" if status in {"waiting", "blocked"} else "working", evidence, origin="board")

    def poll_once(self):
        result = {"discovered": 0, "sources": 0, "events": 0, "errors": []}
        try:
            result["discovered"], board, errors = self.discover()
            result["errors"].extend(errors)
        except (OSError, ValueError, sqlite3.Error):
            board = {}
            result["errors"].append({"kind": "board_unavailable"})
        for source in self.store.list_sources():
            if source["project"] != self.project:
                continue
            try:
                collected = self._local(source) if source.get("path") else self._http(source)
                result["events"] += collected["inserted"]
                result["sources"] += 1
                if source["id"] in board:
                    self._board_evidence(source["id"], board[source["id"]])
            except (OSError, ValueError, sqlite3.Error, KeyError):
                result["errors"].append({"kind": "source_read_failed", "source_id": source["id"]})
        return result
