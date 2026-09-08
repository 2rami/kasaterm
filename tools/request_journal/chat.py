"""Durable, bounded asynchronous chat; model output never executes actions."""

from contextlib import contextmanager
import json
from pathlib import Path
import queue
import re
import sqlite3
import threading
import time
import uuid

from .checklist import apply_semantic, build_id, context_hash, make_checklist, requests_in, semantic_batches


def render_checklist(checklist):
    lines = ["", "재시작 확인 목록"]
    for state, label in checklist.get("groups", {}).items():
        items = [item for item in checklist.get("items", []) if item["buildstate"] == state]
        if not items:
            continue
        lines.extend(["", label])
        for item in items:
            lines.append("• " + item["title"])
            lines.extend("  - " + step for step in item.get("steps", []))
            if item.get("uncertain"):
                lines.append("  - 포함 여부와 실제 동작은 아직 확인이 필요합니다.")
    return "\n".join(lines)


def text_parts(text):
    parts = []
    pending = [text[index:index + 8000] for index in range(0, len(text), 8000)]
    while pending:
        part = pending.pop(0)
        if len(json.dumps(part, ensure_ascii=False).encode()) > 32000:
            half = len(part) // 2
            pending[:0] = [part[:half], part[half:]]
        else:
            parts.append(part)
    return parts


class ChatStore:
    def __init__(self, db_path):
        self.db_path = Path(db_path)
        with self.connection() as db:
            db.executescript("""
                CREATE TABLE IF NOT EXISTS chat_messages (
                    id INTEGER PRIMARY KEY, project TEXT NOT NULL,
                    conversation_id TEXT NOT NULL, role TEXT NOT NULL,
                    text TEXT NOT NULL, job_id TEXT NOT NULL, created_at REAL NOT NULL
                );
                CREATE INDEX IF NOT EXISTS chat_messages_conversation ON chat_messages(project,conversation_id,id);
                CREATE TABLE IF NOT EXISTS chat_jobs (
                    id TEXT PRIMARY KEY, project TEXT NOT NULL, conversation_id TEXT NOT NULL,
                    client_request_id TEXT, question TEXT NOT NULL, status TEXT NOT NULL,
                    result TEXT NOT NULL DEFAULT '{}', created_at REAL NOT NULL, updated_at REAL NOT NULL,
                    UNIQUE(project,conversation_id,client_request_id)
                );
                CREATE TABLE IF NOT EXISTS chat_cache (
                    project TEXT NOT NULL, cache_key TEXT NOT NULL, result TEXT NOT NULL,
                    updated_at REAL NOT NULL, PRIMARY KEY(project,cache_key)
                );
                CREATE TABLE IF NOT EXISTS chat_pending_checks (
                    project TEXT NOT NULL, item_id TEXT NOT NULL, first_run_id TEXT,
                    content TEXT NOT NULL, updated_at REAL NOT NULL,
                    PRIMARY KEY(project,item_id)
                );
            """)

    @contextmanager
    def connection(self):
        db = sqlite3.connect(self.db_path, timeout=10)
        db.row_factory = sqlite3.Row
        try:
            with db:
                yield db
        finally:
            db.close()

    def find_client_job(self, project, conversation, client_id):
        if not client_id:
            return None
        with self.connection() as db:
            row = db.execute("SELECT id,question FROM chat_jobs WHERE project=? AND conversation_id=? AND client_request_id=?", (project, conversation, client_id)).fetchone()
            return dict(row) if row else None

    def create(self, project, conversation, question, client_id):
        job_id, stamp = str(uuid.uuid4()), time.time()
        with self.connection() as db:
            db.execute("INSERT INTO chat_jobs(id,project,conversation_id,client_request_id,question,status,created_at,updated_at) VALUES(?,?,?,?,?,'queued',?,?)", (job_id, project, conversation, client_id, question, stamp, stamp))
            db.execute("INSERT INTO chat_messages(project,conversation_id,role,text,job_id,created_at) VALUES(?,?,'user',?,?,?)", (project, conversation, question, job_id, stamp))
        return job_id

    def get(self, project, job_id):
        with self.connection() as db:
            row = db.execute("SELECT * FROM chat_jobs WHERE project=? AND id=?", (project, job_id)).fetchone()
            user = db.execute("SELECT id FROM chat_messages WHERE project=? AND job_id=? AND role='user' ORDER BY id LIMIT 1", (project, job_id)).fetchone()
        if not row:
            return None
        result = json.loads(row["result"])
        return dict(result, id=row["id"], job_id=row["id"], conversation_id=row["conversation_id"], status=row["status"], user_message_id=user[0] if user else None)

    def update(self, project, job_id, status, result):
        with self.connection() as db:
            old = db.execute("SELECT conversation_id,status FROM chat_jobs WHERE project=? AND id=?", (project, job_id)).fetchone()
            if not old or old["status"] in ("completed", "cancelled", "failed"):
                return False
            db.execute("UPDATE chat_jobs SET status=?,result=?,updated_at=? WHERE project=? AND id=?", (status, json.dumps(result, ensure_ascii=False), time.time(), project, job_id))
            if status in ("completed", "failed") and result.get("text"):
                text = result["text"]
                parts = text_parts(text)
                for index, part in enumerate(parts):
                    shown = f"답변 {index + 1}/{len(parts)}\n{part}" if len(parts) > 1 else part
                    db.execute("INSERT INTO chat_messages(project,conversation_id,role,text,job_id,created_at) VALUES(?,?,'assistant',?,?,?)", (project, old["conversation_id"], shown, job_id, time.time()))
        return True

    def history(self, project, conversation):
        with self.connection() as db:
            return [dict(row) for row in db.execute("SELECT id,role,text,job_id,created_at FROM chat_messages WHERE project=? AND conversation_id=? ORDER BY id", (project, conversation))]

    def history_page(self, project, conversation, before=None, limit=20, after=None):
        limit = min(20, max(1, int(limit)))
        if before is not None and after is not None:
            raise ValueError("choose before or after")
        with self.connection() as db:
            if after is not None:
                messages = [dict(row) for row in db.execute("SELECT id,role,text,job_id,created_at FROM chat_messages WHERE project=? AND conversation_id=? AND id>? ORDER BY id LIMIT ?", (project, conversation, int(after), limit))]
                later = bool(messages and db.execute("SELECT 1 FROM chat_messages WHERE project=? AND conversation_id=? AND id>? LIMIT 1", (project, conversation, messages[-1]["id"])).fetchone())
                return {"conversation_id": conversation, "messages": messages, "next_after": messages[-1]["id"] if later else None, "next_before": None}
            rows = list(db.execute("SELECT id,role,text,job_id,created_at FROM chat_messages WHERE project=? AND conversation_id=? AND id<? ORDER BY id DESC LIMIT ?", (project, conversation, int(before) if before else 2**63 - 1, limit)))
            messages = [dict(row) for row in reversed(rows)]
            earlier = bool(messages and db.execute("SELECT 1 FROM chat_messages WHERE project=? AND conversation_id=? AND id<? LIMIT 1", (project, conversation, messages[0]["id"])).fetchone())
            return {"conversation_id": conversation, "messages": messages, "next_before": messages[0]["id"] if earlier else None, "next_after": None}

    def pending(self, project):
        with self.connection() as db:
            return [dict(row, content=json.loads(row["content"])) for row in db.execute("SELECT * FROM chat_pending_checks WHERE project=?", (project,))]

    def save_pending(self, project, checklist):
        run = checklist.get("context", {}).get("current_run") or checklist.get("context", {}).get("last_run") or {}
        with self.connection() as db:
            for item in checklist.get("items", []):
                db.execute("INSERT INTO chat_pending_checks VALUES(?,?,?,?,?) ON CONFLICT(project,item_id) DO UPDATE SET content=excluded.content,updated_at=excluded.updated_at", (project, item["id"], run.get("id"), json.dumps(item, ensure_ascii=False), time.time()))

    def interrupt_pending(self, project):
        with self.connection() as db:
            db.execute("UPDATE chat_jobs SET status='failed',result=?,updated_at=? WHERE project=? AND status IN ('queued','running')", (json.dumps({"error": "service_restarted", "text": "장부 서비스가 다시 시작되어 답변이 중단됐습니다. 질문 기록은 남아 있습니다."}, ensure_ascii=False), time.time(), project))

    def cache_get(self, project, key):
        with self.connection() as db:
            row = db.execute("SELECT result FROM chat_cache WHERE project=? AND cache_key=?", (project, key)).fetchone()
            return json.loads(row[0]) if row else None

    def cache_put(self, project, key, result):
        with self.connection() as db:
            db.execute("INSERT INTO chat_cache VALUES(?,?,?,?) ON CONFLICT(project,cache_key) DO UPDATE SET result=excluded.result,updated_at=excluded.updated_at", (project, key, json.dumps(result, ensure_ascii=False), time.time()))

    def cache_latest(self, project):
        with self.connection() as db:
            row = db.execute("SELECT result FROM chat_cache WHERE project=? ORDER BY updated_at DESC LIMIT 1", (project,)).fetchone()
            return json.loads(row[0]) if row else None


class ChatManager:
    def __init__(self, store, project, provider_factory=None, max_seconds=180):
        self.store, self.project = store, project
        self.chat = ChatStore(store.db_path)
        self.provider_factory = provider_factory or (lambda _cancel: None)
        self.max_seconds = max_seconds
        self.pending = queue.Queue(maxsize=4)
        self.lock = threading.Lock()
        self.cancellations = {}
        self.fallbacks = {}
        self.stopped = threading.Event()
        self.latest_checklist = self.chat.cache_latest(project)
        if self.latest_checklist:
            self.chat.save_pending(project, self.latest_checklist)
        self.chat.interrupt_pending(project)
        self.worker = threading.Thread(target=self._work, name="journal-chat", daemon=True)
        self.worker.start()

    def submit(self, text, conversation="pet", client_id=None):
        if not isinstance(text, str) or not 1 <= len(text.strip()) <= 4000:
            raise ValueError("question must contain 1–4000 characters")
        if not isinstance(conversation, str) or not re.fullmatch(r"[A-Za-z0-9_-]{1,80}", conversation):
            raise ValueError("invalid conversation id")
        if client_id is not None and (not isinstance(client_id, str) or not re.fullmatch(r"[A-Za-z0-9_-]{1,128}", client_id)):
            raise ValueError("invalid client request id")
        with self.lock:
            existing = self.chat.find_client_job(self.project, conversation, client_id)
            if existing:
                if existing["question"] != text:
                    raise ValueError("client request id already belongs to another question")
                return self.chat.get(self.project, existing["id"])
            if self.pending.full():
                raise OverflowError("chat queue is full")
            job_id = self.chat.create(self.project, conversation, text, client_id)
            cancel = threading.Event()
            self.cancellations[job_id] = cancel
            self.pending.put_nowait((job_id, text, cancel))
        return self.chat.get(self.project, job_id)

    def cancel(self, job_id):
        current = self.chat.get(self.project, job_id)
        if not current:
            return None
        with self.lock:
            event = self.cancellations.get(job_id)
            if event:
                event.set()
        self.chat.update(self.project, job_id, "cancelled", {"text": "답변 생성을 취소했습니다. 질문 기록은 남아 있습니다."})
        return self.chat.get(self.project, job_id)

    def context(self):
        context = self.store.runtime_context(project=self.project, machine="local")
        run = context.get("current_run") or context.get("last_run") or {}
        existing = {row["id"] for row in requests_in(context)}
        context["carryover"] = list(context.get("carryover", []))
        context["carryover_items"] = []
        for pending in self.chat.pending(self.project):
            if pending["first_run_id"] == run.get("id"):
                continue
            item = pending["content"]
            if not item.get("source_request_ids"):
                context["carryover_items"].append(item)
            for request_id in item.get("source_request_ids", []):
                if request_id not in existing:
                    row = self.store.get_request(request_id)
                    if row and row.get("applied_status") not in ("applied", "not_applicable"):
                        context["carryover"].append(row)
                        existing.add(request_id)
        return context

    def checklist(self):
        if self.latest_checklist:
            return dict(self.latest_checklist, status="cached", refresh_required=True)
        return {"status": "not_generated", "items": [], "coverage": {}, "context": None}

    def close(self):
        self.stopped.set()
        with self.lock:
            for event in self.cancellations.values():
                event.set()
        self.worker.join(timeout=15)

    def _work(self):
        while not self.stopped.is_set():
            try:
                job_id, question, cancel = self.pending.get(timeout=.2)
            except queue.Empty:
                continue
            timer = threading.Timer(self.max_seconds, self._expire, args=(job_id, cancel))
            timer.daemon = True
            timer.start()
            try:
                if not cancel.is_set():
                    self._answer(job_id, question, cancel)
            except Exception:
                with self.lock:
                    checks = self.fallbacks.get(job_id)
                result = {"error": "chat_unavailable", "text": "답변을 만들지 못했습니다. 요청 기록은 보존됐습니다. 잠시 후 다시 질문해 주세요."}
                if checks:
                    result.update(checklist=checks, context=checks["context"])
                    result["text"] += render_checklist(checks)
                if self.chat.update(self.project, job_id, "failed", result) and checks:
                    self.chat.save_pending(self.project, checks)
            finally:
                timer.cancel()
                with self.lock:
                    self.cancellations.pop(job_id, None)
                    self.fallbacks.pop(job_id, None)
                self.pending.task_done()

    def _expire(self, job_id, cancel):
        cancel.set()
        with self.lock:
            checklist = self.fallbacks.get(job_id)
        result = {"error": "chat_time_budget", "partial": True, "text": "답변 제한 시간이 지나 의미 정리를 멈췄습니다. 확인된 근거와 기본 확인 항목은 보존했습니다."}
        if checklist:
            result.update(checklist=checklist, context=checklist["context"])
            result["text"] += render_checklist(checklist)
        if self.chat.update(self.project, job_id, "failed", result) and checklist:
            self.chat.save_pending(self.project, checklist)

    def _answer(self, job_id, question, cancel):
        from collections import Counter
        from .checklist import enrich_code_evidence, component_checks
        context = enrich_code_evidence(self.context(), self.project)
        checklist = make_checklist(context)
        with self.lock:
            self.fallbacks[job_id] = checklist
        self.chat.update(self.project, job_id, "running", {"text": "앱을 마지막으로 켠 뒤의 요청과 빌드 근거를 확인하고 있습니다.", "context": checklist["context"]})
        key = checklist["context_hash"]
        cached = self.chat.cache_get(self.project, key)
        deadline = time.monotonic() + self.max_seconds
        provider = self.provider_factory(cancel)
        provider_name = getattr(provider, "name", "structured-fallback") if provider else "structured-fallback"
        partials, proposals, errors = [], [], []
        runtime = {name: {key: value for key, value in (context.get(name) or {}).items() if key in ("id", "pid", "started_at", "build_id", "linked_build_id")} for name in ("current_run", "previous_run")}
        runtime["artifact_states"] = checklist["context"].get("artifact_states", [])
        runtime["ready_build_ids"] = [item.get("build_id") for item in (context.get("latest_observation") or {}).get("evidence", {}).get("artifacts", []) if item.get("status") == "verified_ready"]
        known_requests = {row["id"] for row in requests_in(context)}
        known_evidence = {key for item in checklist["items"] for key in item["evidence_ids"]}
        known_evidence.update(build_id(build) for build in context.get("builds", []))
        planned, processed = Counter(), Counter()
        final_summary = ""

        def complete(payload, structured=True):
            if cancel.is_set():
                raise InterruptedError("cancelled")
            if time.monotonic() >= deadline:
                raise TimeoutError("semantic_time_budget")
            serialized = json.dumps(payload, ensure_ascii=False)
            if len(serialized) > 12000:
                raise ValueError("completion_input_budget")
            raw = provider.complete(serialized)
            if not structured:
                if not isinstance(raw, str):
                    raise ValueError("invalid_chat_response")
                return raw
            value = json.loads(raw) if isinstance(raw, str) else raw
            if not isinstance(value, dict) or not isinstance(value.get("items", []), list) or not isinstance(value.get("text", ""), str):
                raise ValueError("invalid_checklist_response")
            return value

        def packs(records, budget=5000):
            pack, length = [], 0
            for record in records:
                size = len(json.dumps(record, ensure_ascii=False))
                if size > budget:
                    encoded = json.dumps(record, ensure_ascii=False)
                    split = [{"part": index + 1, "parts": (len(encoded) + 2499) // 2500, "text": encoded[pos:pos + 2500]} for index, pos in enumerate(range(0, len(encoded), 2500))]
                    if pack:
                        yield pack
                        pack, length = [], 0
                    for part in split:
                        yield [part]
                else:
                    if pack and length + size > budget:
                        yield pack
                        pack, length = [], 0
                    pack.append(record)
                    length += size
            if pack:
                yield pack

        if cached and cached.get("provider") == provider_name:
            checklist = cached
            final_summary = cached.get("semantic_summary", "")
        elif provider and hasattr(provider, "complete"):
            batches = [(batch, []) for batch in semantic_batches(context, budget=5000)]
            code = [{"evidence_id": item["id"], "title": item["title"], "buildstate": item["buildstate"], "evidence_ids": item["evidence_ids"], "uncertain": item["uncertain"]} for item in checklist["items"] if item.get("basis") == "code_change"]
            facts = [{"id": build_id(build), "completed_at": build.get("completed_at"), "success": build.get("success"), "signature_verified": build.get("signature", {}).get("verified"), "source": build.get("source")} for build in context.get("builds", [])]
            batches.extend(([], batch) for batch in packs(code + facts))
            for records, _ in batches:
                planned.update(item["request_id"] for item in records)
            for index, (records, builds) in enumerate(batches):
                if cancel.is_set():
                    return
                try:
                    reply = complete({"mode": "checklist", "question": question, "requests": records, "builds": builds, "runtime": runtime})
                    proposals.extend(reply.get("items", []))
                    partials.append(reply)
                    processed.update(item["request_id"] for item in records)
                except Exception:
                    errors.append("partial_summary_unavailable" if time.monotonic() < deadline else "semantic_time_budget")
                    break
                self.chat.update(self.project, job_id, "running", {"text": "요청 묶음을 정리하고 있습니다.", "progress": {"completed_batches": index + 1, "total_batches": len(batches)}, "context": checklist["context"]})
            merged = partials
            # Every partial participates in a merge tree; nothing is silently
            # reduced to the last few requests or last few summaries.
            try:
                while len(merged) > 1:
                    groups = list(packs(merged))
                    next_level = []
                    for group in groups:
                        next_level.append(complete({"mode": "checklist", "question": "부분 확인 목록을 중복 없이 합치세요. 알려진 요청과 근거 ID만 유지하세요. " + question, "partials": group, "runtime": runtime}))
                    if len(next_level) >= len(merged):
                        errors.append("merge_budget")
                        break
                    merged = next_level
                if len(merged) == 1:
                    final_summary = merged[0].get("text", "")
                    merged_items = merged[0].get("items", [])
                    merged_ids = {key for item in merged_items if isinstance(item, dict) for key in item.get("source_request_ids", []) if isinstance(key, str) and key in known_requests}
                    merged_evidence = {key for item in merged_items if isinstance(item, dict) for key in item.get("evidence_ids", []) if isinstance(key, str) and key in known_evidence}
                    proposals = merged_items + [item for item in proposals if isinstance(item, dict) and (set(item.get("source_request_ids", [])) - merged_ids or (not item.get("source_request_ids") and set(item.get("evidence_ids", [])) - merged_evidence))]
            except Exception:
                errors.append("partial_merge_unavailable")
            checklist = apply_semantic(checklist, proposals, known_requests, known_evidence)
            complete_ids = {key for key, amount in planned.items() if processed[key] == amount}
            checklist["coverage"].update({"semantic_request_ids": sorted(complete_ids), "fallback_request_ids": sorted(known_requests - complete_ids), "total_parts": sum(planned.values()), "processed_parts": sum(processed.values())})
        else:
            checklist["coverage"]["fallback_request_ids"] = sorted(known_requests)
        if cancel.is_set():
            return
        checklist = component_checks(checklist, context)
        total = checklist["coverage"]["total_requests"]
        counts = {group: sum(item["buildstate"] == group for item in checklist["items"]) for group in checklist["groups"]}
        text = f"마지막 앱 실행 이후 요청과 이전 확인 목록 {total}개를 검토했습니다. 새 빌드 확인 {counts['built_not_running']}개, 빌드 파일 검증 필요 {counts['build_unverified']}개, 아직 빌드되지 않은 수정 {counts['not_built']}개, 구현 확인 필요 {counts['implementation_unverified']}개, 이전부터 남은 확인 {counts['carryover']}개입니다."
        if not context.get("current_run"):
            text = "현재 앱 실행 시점을 확인하지 못했습니다. 서비스 시작 시각을 앱 재시작으로 간주하지 않습니다. " + text
        if final_summary:
            text += "\n나쵸의 근거 기반 정리:\n" + final_summary
        restart_question = any(word in question for word in ("재시작", "다시 켜", "확인", "체크", "restart", "checklist"))
        if not restart_question and provider and hasattr(provider, "complete"):
            try:
                job = self.chat.get(self.project, job_id)
                history = self.chat.history(self.project, job["conversation_id"])
                recent = [{"role": row["role"], "text": row["text"][:800]} for row in history[-4:]]
                text = complete({"mode": "chat", "question": question, "partials": [{"text": final_summary or text}], "history": recent, "runtime": runtime}, structured=False)
            except Exception:
                errors.append("chat_answer_unavailable")
                text = "질문에 대한 나쵸 답변을 만들지 못했습니다. 확인 가능한 요청·빌드 상태는 아래 목록에 남겼습니다.\n" + text
        if errors:
            text += "\n일부 의미 정리가 끝나지 않아 해당 요청은 기본 확인 항목으로 보존했습니다."
        elif not provider:
            text += "\n나쵸 연결이 없어 저장된 근거를 기본 목록으로 보여 드립니다."
        if restart_question:
            text += render_checklist(checklist)
        result = {"text": text, "provider": provider_name, "checklist": checklist, "context": checklist["context"], "partial": bool(errors), "errors": errors}
        self.latest_checklist = checklist
        if not errors:
            self.chat.cache_put(self.project, key, dict(checklist, provider=provider_name, semantic_summary=final_summary))
        if self.chat.update(self.project, job_id, "completed", result):
            self.chat.save_pending(self.project, checklist)
