"""Loopback-only request journal API. Request content never enters access logs."""

import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, unquote, urlsplit


STATIC = Path(__file__).with_name("static")


def request_title(row, limit=65):
    summary = row.get("summary")
    candidate = summary.splitlines()[0] if isinstance(summary, str) and summary.strip() else row.get("prompt", "")
    if candidate.startswith(("나쵸 요약", "학생 보고 기준")):
        candidate = row.get("prompt", "")
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


class JournalServer(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, store_factory, project, port=18769):
        self.store_factory = store_factory
        self.project = str(Path(project).resolve())
        self.summarizer_status = {"provider": "unavailable", "updated": 0, "skipped": 0}
        self.collector_status = "disabled"
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
            return self.reply(200, {"ok": True, "service": "request-journal", "version": 1, "collector": self.server.collector_status})
        assets = {"/": ("index.html", "text/html; charset=utf-8"), "/app.js": ("app.js", "text/javascript; charset=utf-8"), "/style.css": ("style.css", "text/css; charset=utf-8")}
        if route in assets:
            filename, kind = assets[route]
            return self.reply(200, (STATIC / filename).read_bytes(), kind)
        query = parse_qs(parsed.query)
        project = query.get("project", [self.server.project])[0]
        if project != self.server.project:
            return self.reply(400, {"error": "project_out_of_scope"})
        try:
            store = self.server.store_factory()
            if route == "/api/requests":
                limit = min(100, max(1, int(query.get("limit", [50])[0])))
                status = query.get("reported_status", [None])[0]
                if status not in (None, "received", "working", "reported_done", "needs_attention"):
                    return self.reply(400, {"error": "invalid_status"})
                rows = store.list_requests(project=project, limit=limit, before=query.get("before", [None])[0], reported_status=status)
                return self.reply(200, {"requests": [request_view(row) for row in rows], "next_before": rows[-1]["id"] if len(rows) == limit else None, "project": project})
            if route in ("/api/summary", "/api/ask"):
                rows = store.list_requests(project=project, limit=50)
                pending = [row for row in rows if row.get("reported_status") == "reported_done" and row.get("applied_status") not in ("applied", "not_applicable")]
                counts = store.stats(project=project)
                text = "최근 요청: " + " · ".join(request_title(row) for row in rows[:3]) + ". 반영 확인은 별도입니다." if rows else "아직 기록된 요청이 없습니다."
                waiting = waiting_summary(rows)
                url = f"http://127.0.0.1:{self.server.server_port}/"
                if route == "/api/ask":
                    question = query.get("q", [""])[0][:300]
                    answer = waiting if any(word in question for word in ("남", "대기", "아직", "재시작", "wait", "left", "restart")) else text
                    return self.reply(200, {"version": 1, "project": project, "text": answer[:250], "url": url})
                return self.reply(200, {"version": 1, "project": project, "counts": counts, "latest": rows[0] if rows else None, "needs_confirmation": pending, "text": text[:250], "waiting_text": waiting[:250], "url": url, "summarizer": self.server.summarizer_status})
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
        if len(parts) != 4 or parts[:2] != ["api", "requests"] or parts[3] != "ack":
            return self.reply(404, {"error": "not_found"})
        try:
            length = int(self.headers.get("Content-Length", "0"))
            if not 0 < length <= 65536 or self.headers.get("Transfer-Encoding"):
                return self.reply(413, {"error": "invalid_body_size"})
            body = json.loads(self.rfile.read(length))
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
        except Exception:
            self.reply(500, {"error": "journal_unavailable"})
