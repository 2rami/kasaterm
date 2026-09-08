"""Loopback-only request journal API. Request content never enters access logs."""

import json
import os
import shutil
import subprocess
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, unquote, urlsplit


STATIC = Path(__file__).with_name("static")


def request_title(row, limit=65):
    summary = row.get("summary")
    candidate = summary.splitlines()[0] if isinstance(summary, str) and summary.strip() else row.get("prompt", "")
    if candidate.startswith(("나쵸 요약", "학생 보고 기준")):
        candidate = next((line for line in summary.splitlines()[1:] if line.strip() and not line.startswith("실제 반영")), row.get("prompt", ""))
    text = " ".join(str(candidate).removeprefix("요청:").split()) or "내용 없는 요청"
    return text if len(text) <= limit else text[:limit - 1] + "…"


def request_view(row):
    if row is None:
        return None
    result = dict(row)
    updates = row.get("updates", [])
    result["reported_evidence"] = next((item.get("evidence") for item in reversed(updates) if item.get("kind") == "reported_status"), None)
    result["applied_evidence"] = next((item.get("evidence") for item in reversed(updates) if item.get("kind") == "applied_status"), None)
    result["ack_history"] = [dict(item, applied_status=item.get("value")) for item in updates if item.get("kind") == "applied_status"]
    return result


def summary_view(row):
    if row is None:
        return None
    # The native pet has a small response budget; transcript evidence belongs
    # exclusively to the full request endpoint, never this discovery summary.
    return {
        "id": str(row.get("id", ""))[:128],
        "summary": str(row.get("summary", ""))[:120],
        "prompt_preview": str(row.get("prompt", ""))[:80],
        "created_at": str(row.get("created_at", ""))[:64],
        "reported_status": str(row.get("reported_status", "received"))[:32],
        "applied_status": str(row.get("applied_status", "unknown"))[:32],
    }


def waiting_summary(rows):
    restart = [row for row in rows if row.get("applied_status") == "restart_required"]
    pending = [row for row in rows if row.get("applied_status") == "pending"]
    unknown = [row for row in rows if row.get("applied_status") == "unknown"]
    parts = []
    if restart:
        parts.append("최근 재시작 대기: " + " · ".join(request_title(row, 55) for row in restart[:2]))
    else:
        parts.append("재시작으로 바뀔 항목은 아직 확인 못했어요")
    if pending:
        parts.append("최근 반영 대기(재시작 효과는 미확인): " + " · ".join(request_title(row, 55) for row in pending[:2]))
    elif unknown:
        parts.append("상태 확인이 필요한 최근 요청: " + " · ".join(request_title(row, 55) for row in unknown[:2]))
    return ". ".join(parts)[:249] + "."


def summary_payload(store, project, port, provider):
    rows = store.list_requests(project=project, limit=50)
    pending = [row for row in rows if row.get("reported_status") == "reported_done" and row.get("applied_status") not in ("applied", "not_applicable")]
    text = "최근 요청: " + " · ".join(request_title(row) for row in rows[:3]) + ". 반영 확인은 별도입니다." if rows else "아직 기록된 요청이 없습니다."
    sources = [(row.get("summary_evidence") or {}).get("provider") if isinstance(row.get("summary_evidence"), dict) else None for row in rows[:3]]
    text_source = "nacho" if sources and all(source in ("nacho-http", "nacho-llm") for source in sources) else "structured-fallback"
    return {"version": 1, "project": project, "counts": store.stats(project=project), "latest": summary_view(rows[0]) if rows else None,
            "needs_confirmation": [summary_view(row) for row in pending[:5]], "text": text[:250], "waiting_text": waiting_summary(rows),
            "url": f"http://127.0.0.1:{port}/", "summarizer": provider, "text_source": text_source}


def send_pet_summary(text):
    root = Path(__file__).resolve().parents[2]
    candidates = [Path.home() / "Applications/kasaterm.app/Contents/MacOS/kasaterm-cli", root / "target/release/kasaterm-cli"]
    binary = next((str(path) for path in candidates if path.is_file() and os.access(path, os.X_OK)), None) or shutil.which("kasaterm-cli")
    if not binary:
        raise RuntimeError("pet transport unavailable")
    running = False
    try:
        pid = int((Path.home() / ".config/kasaterm/pet.pid").read_text().strip())
        if 0 < pid < 2**31:
            os.kill(pid, 0)
            running = True
    except (OSError, ValueError):
        pass
    result = subprocess.run([binary, "pet-say", "--from", "요청장부", "--state", "wait", text[:250]], capture_output=True, timeout=8)
    if result.returncode:
        raise RuntimeError("pet transport unavailable")
    return {"ok": True, "pet_running": running, "state": "sent" if running else "queued", "message": "곽향에 요약을 전달했습니다" if running else "펫이 꺼져 있어 요약을 알림 대기열에 넣었습니다"}


def chat_context_view(context):
    if not isinstance(context, dict):
        return None
    result = {key: context[key] for key in ("build_count", "ready_build_count", "since_start_count", "carryover_count", "timestamp_unknown_count") if key in context}
    result["artifact_states"] = context.get("artifact_states", [])[:10]
    for name in ("current_run", "last_run", "previous_run"):
        run = context.get(name)
        result[name] = {key: str(run[key])[:160] if isinstance(run[key], str) else run[key] for key in ("id", "pid", "machine", "started_at", "build_id", "linked_build_id") if key in run} if isinstance(run, dict) else None
    return result


def chat_job_view(job):
    result = {key: job[key] for key in ("id", "job_id", "conversation_id", "status", "user_message_id", "provider", "progress", "error", "errors", "partial") if key in job}
    result["context"] = chat_context_view(job.get("context"))
    result["text"] = str(job.get("text", ""))[:1500]
    result["text_is_preview"] = len(str(job.get("text", ""))) > 1500
    result["checklist_url"] = "/api/checklist?job_id=" + job["id"]
    result["history_after"] = max(0, (job.get("user_message_id") or 1) - 1)
    return result


def checklist_page(checklist, offset=0, limit=20, view="main"):
    if view not in ("main", "supplementary"):
        raise ValueError("invalid checklist view")
    offset, limit = max(0, int(offset)), min(20, max(1, int(limit)))
    all_items = checklist.get("supplementary_items", []) if view == "supplementary" else checklist.get("items", [])
    items = []
    for original in all_items[offset:offset + limit]:
        item = dict(original)
        for key in ("source_request_ids", "evidence_ids"):
            item[key + "_count"] = len(item.get(key, []))
            item[key] = item.get(key, [])[:100]
        item["context_note_count"] = len(item.get("context_notes", []))
        item["context_notes"] = item.get("context_notes", [])[:10]
        items.append(item)
    coverage = {key: value for key, value in checklist.get("coverage", {}).items() if not isinstance(value, list)}
    for key, value in checklist.get("coverage", {}).items():
        if isinstance(value, list):
            coverage[key.removesuffix("_ids") + "_count"] = len(value)
    return {"status": checklist.get("status", "completed"), "view": view, "items": items, "groups": checklist.get("groups", {}), "coverage": coverage, "context": chat_context_view(checklist.get("context")), "total_items": len(all_items), "supplementary_count": len(checklist.get("supplementary_items", [])), "offset": offset, "next_offset": offset + len(items) if offset + len(items) < len(all_items) else None}


class JournalServer(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, store_factory, project, port=0, pet_sender=None):
        self.store_factory = store_factory
        self.project = str(Path(project).resolve())
        self.summarizer_status = {"provider": "unavailable", "updated": 0, "skipped": 0}
        self.summary_transport = None
        self.collector_status = "disabled"
        self.runtime_status = "disabled"
        self.chat = None
        self.pet_sender = pet_sender or send_pet_summary
        super().__init__(("127.0.0.1", port), Handler)


class Handler(BaseHTTPRequestHandler):
    server_version = "RequestJournal/1"

    def log_message(self, *_args):
        pass

    def reply(self, status, payload, content_type="application/json; charset=utf-8"):
        body = json.dumps(payload, ensure_ascii=False).encode() if not isinstance(payload, bytes) else payload
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("Referrer-Policy", "no-referrer")
        self.send_header("Content-Security-Policy", "default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'")
        self.end_headers()
        self.wfile.write(body)

    def guard(self):
        port = self.server.server_port
        allowed = {f"127.0.0.1:{port}", f"localhost:{port}"}
        host = self.headers.get("Host", "")
        origin = self.headers.get("Origin")
        if host not in allowed or (origin is not None and origin != f"http://{host}"):
            self.reply(403, {"error": "local_origin_required"})
            return False
        return True

    def do_GET(self):
        if not self.guard():
            return
        parsed = urlsplit(self.path)
        route = unquote(parsed.path)
        if route == "/health":
            return self.reply(200, {"ok": True, "service": "request-journal", "version": 1, "collector": self.server.collector_status, "runtime": self.server.runtime_status, "summary_transport": self.server.summary_transport})
        assets = {"/": ("index.html", "text/html; charset=utf-8"), "/app.js": ("app.js", "text/javascript; charset=utf-8"), "/chat.js": ("chat.js", "text/javascript; charset=utf-8"), "/style.css": ("style.css", "text/css; charset=utf-8")}
        if route in assets:
            filename, kind = assets[route]
            return self.reply(200, (STATIC / filename).read_bytes(), kind)
        query = parse_qs(parsed.query)
        project = query.get("project", [self.server.project])[0]
        if project != self.server.project:
            return self.reply(400, {"error": "project_out_of_scope"})
        try:
            store = self.server.store_factory()
            if route.startswith("/api/chat/") or route.startswith("/api/checklist"):
                if self.server.chat is None:
                    return self.reply(503, {"error": "chat_unavailable"})
                if route == "/api/chat/history":
                    conversation = query.get("conversation_id", ["pet"])[0]
                    return self.reply(200, self.server.chat.chat.history_page(project, conversation, before=query.get("before", [None])[0], after=query.get("after", [None])[0], limit=query.get("limit", [20])[0]))
                if route.startswith("/api/chat/jobs/"):
                    job = self.server.chat.chat.get(project, route.rsplit("/", 1)[1])
                    return self.reply(200, chat_job_view(job)) if job else self.reply(404, {"error": "not_found"})
                if route in ("/api/checklist", "/api/checklist/evidence"):
                    job_id = query.get("job_id", [None])[0]
                    job = self.server.chat.chat.get(project, job_id) if job_id else None
                    if job_id and not job:
                        return self.reply(404, {"error": "not_found"})
                    checks = (job or {}).get("checklist") or self.server.chat.checklist()
                    if route == "/api/checklist/evidence":
                        item_id = query.get("item_id", [""])[0]
                        item = next((item for item in checks.get("items", []) + checks.get("supplementary_items", []) if item["id"] == item_id), None)
                        if not item:
                            return self.reply(404, {"error": "not_found"})
                        offset = max(0, int(query.get("offset", [0])[0]))
                        source, evidence = item.get("source_request_ids", []), item.get("evidence_ids", [])
                        return self.reply(200, {"source_request_ids": source[offset:offset + 100], "evidence_ids": evidence[offset:offset + 100], "next_offset": offset + 100 if max(len(source), len(evidence)) > offset + 100 else None})
                    return self.reply(200, checklist_page(checks, query.get("offset", [0])[0], query.get("limit", [20])[0], query.get("view", ["main"])[0]))
            if route == "/api/requests":
                limit = min(100, max(1, int(query.get("limit", [50])[0])))
                status = query.get("reported_status", [None])[0]
                if status not in (None, "received", "working", "reported_done", "needs_attention"):
                    return self.reply(400, {"error": "invalid_status"})
                rows = store.list_requests(project=project, limit=limit, before=query.get("before", [None])[0], reported_status=status)
                return self.reply(200, {"requests": [request_view(row) for row in rows], "next_before": rows[-1]["id"] if len(rows) == limit else None, "project": project})
            if route in ("/api/summary", "/api/ask"):
                payload = summary_payload(store, project, self.server.server_port, self.server.summarizer_status)
                if route == "/api/ask":
                    question = query.get("q", [""])[0][:300]
                    answer = payload["waiting_text"] if any(word in question for word in ("남", "대기", "아직", "재시작", "wait", "left", "restart")) else payload["text"]
                    return self.reply(200, {"version": 1, "project": project, "text": answer, "url": payload["url"]})
                return self.reply(200, payload)
            if route.startswith("/api/requests/") and "/" not in route[len("/api/requests/"):]:
                row = store.get_request(route.rsplit("/", 1)[1])
                if not row or row.get("project") != project:
                    return self.reply(404, {"error": "not_found"})
                return self.reply(200, request_view(row))
            return self.reply(404, {"error": "not_found"})
        except (ValueError, TypeError):
            self.reply(400, {"error": "invalid_request"})
        except Exception:
            self.reply(500, {"error": "journal_unavailable"})

    def do_POST(self):
        if not self.guard():
            return
        if self.headers.get("Content-Type", "").split(";", 1)[0].strip() != "application/json" or self.headers.get("X-Journal-Request") != "1":
            return self.reply(415, {"error": "json_request_required"})
        route = unquote(urlsplit(self.path).path)
        parts = route.strip("/").split("/")
        pet_request = route == "/api/pet-summary"
        chat_request = route == "/api/chat"
        if not pet_request and not chat_request and (len(parts) != 4 or parts[:2] != ["api", "requests"] or parts[3] != "ack"):
            return self.reply(404, {"error": "not_found"})
        try:
            length = int(self.headers.get("Content-Length", "0"))
            if not 0 < length <= 65536 or self.headers.get("Transfer-Encoding"):
                return self.reply(413, {"error": "invalid_body_size"})
            body = json.loads(self.rfile.read(length))
            if chat_request:
                if self.server.chat is None:
                    return self.reply(503, {"error": "chat_unavailable"})
                if not isinstance(body, dict) or set(body) - {"text", "conversation_id", "client_request_id", "read_only"}:
                    return self.reply(400, {"error": "invalid_chat_request"})
                job = self.server.chat.submit(body.get("text"), body.get("conversation_id", "pet"), body.get("client_request_id"), read_only=body.get("read_only", False))
                return self.reply(202, {"job_id": job["id"], "conversation_id": job["conversation_id"], "status": job["status"], "user_message_id": job["user_message_id"]})
            if pet_request:
                if body != {}:
                    return self.reply(400, {"error": "empty_object_required"})
                payload = summary_payload(self.server.store_factory(), self.server.project, self.server.server_port, self.server.summarizer_status)
                source = "나쵸 요약" if payload["text_source"] == "nacho" else "기본 정리"
                return self.reply(200, self.server.pet_sender(f"{source} · {payload['text']}"[:250]))
            if not isinstance(body, dict) or body.get("applied_status") not in ("applied", "pending"):
                return self.reply(400, {"error": "invalid_acknowledgement"})
            evidence = body.get("evidence", "")
            if not isinstance(evidence, str) or len(evidence) > 2000:
                return self.reply(400, {"error": "invalid_evidence"})
            store = self.server.store_factory()
            row = store.get_request(parts[2])
            if not row or row.get("project") != self.server.project:
                return self.reply(404, {"error": "not_found"})
            store.acknowledge(parts[2], body["applied_status"], evidence={"method": "web_user_confirmation", "note": evidence}, origin="user")
            self.reply(200, request_view(store.get_request(parts[2])))
        except (ValueError, TypeError, UnicodeDecodeError):
            self.reply(400, {"error": "invalid_json"})
        except OverflowError:
            self.reply(429, {"error": "chat_queue_full"})
        except Exception:
            self.reply(500, {"error": "journal_unavailable"})

    def do_DELETE(self):
        if not self.guard():
            return
        if self.headers.get("Content-Type", "").split(";", 1)[0].strip() != "application/json" or self.headers.get("X-Journal-Request") != "1":
            return self.reply(415, {"error": "json_request_required"})
        route = unquote(urlsplit(self.path).path)
        if not route.startswith("/api/chat/jobs/") or self.server.chat is None:
            return self.reply(404, {"error": "not_found"})
        try:
            length = int(self.headers.get("Content-Length", "0"))
            if not 0 < length <= 128 or self.headers.get("Transfer-Encoding"):
                return self.reply(413, {"error": "invalid_body_size"})
            if json.loads(self.rfile.read(length)) != {}:
                return self.reply(400, {"error": "empty_object_required"})
            job = self.server.chat.cancel(route.rsplit("/", 1)[1])
            return self.reply(200, chat_job_view(job)) if job else self.reply(404, {"error": "not_found"})
        except (ValueError, UnicodeDecodeError):
            self.reply(400, {"error": "invalid_json"})
