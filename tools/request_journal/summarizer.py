"""Bounded summaries which never certify completion or application state."""
from __future__ import annotations

import hashlib
import json
import time


def _text(value):
    if isinstance(value, str):
        return value
    if isinstance(value, dict):
        return value.get("text", "") if isinstance(value.get("text", ""), str) else ""
    return ""


def source(row):
    finals = row.get("finals") or []
    if isinstance(finals, (str, dict)):
        finals = [finals]
    return {"prompt": _text(row.get("prompt")), "student_reports": [
        _text(item) for item in finals
        if _text(item) and (not isinstance(item, dict) or item.get("kind", "assistant_final") == "assistant_final")
    ]}


def fingerprint(data):
    return hashlib.sha256(json.dumps(data, ensure_ascii=False, sort_keys=True).encode()).hexdigest()


def short(text, limit=150):
    text = " ".join(text.split())
    return text if len(text) <= limit else text[:limit - 1] + "…"


def fallback(data):
    requested = short(data["prompt"], 150) or "요청 원문 없음"
    reports = data["student_reports"]
    report = short(reports[-1], 150) if reports else "아직 최종 보고 없음"
    return f"요청: {requested}\n학생 보고: {report}\n나쵸 요약 연결 안 됨 · 원문 발췌이며 실제 반영은 별도 확인이 필요해요."


class Summarizer:
    def __init__(self, provider=None):
        self.provider = provider
        self._cursor = None
        self._retry_after = 0

    def update(self, store, project=None, limit=8):
        provider_name = self.provider.name if self.provider else "structured-fallback"
        result = {"provider": provider_name, "updated": 0, "skipped": 0}
        started = time.monotonic()
        # Carry the cursor across cycles so a long history is not starved by the newest page.
        rows = store.list_requests(project=project, limit=32, before=self._cursor)
        exhausted = True
        for row in rows:
            if result["updated"] >= max(1, min(limit, 8)) or time.monotonic() - started > 40:
                exhausted = False
                break
            self._cursor = row["id"]
            data = source(row)
            digest = fingerprint(data)
            evidence = row.get("summary_evidence") or {}
            if not isinstance(evidence, dict):
                evidence = {}
            if evidence.get("source_hash") == digest and (evidence.get("provider") == provider_name
                    or (self.provider is None and evidence.get("provider") in ("nacho-llm", "nacho-ssh"))):
                result["skipped"] += 1
                continue
            if (evidence.get("source_hash") == digest
                    and evidence.get("attempted_provider") == provider_name
                    and evidence.get("reason") == "provider_error"
                    and time.time() - evidence.get("attempted_at", 0) < 300):
                result["skipped"] += 1
                result["provider"] = "structured-fallback"
                result["error"] = "provider_unavailable"
                continue
            summary = fallback(data)
            used_provider = "structured-fallback"
            reason = "provider_unavailable"
            if self.provider and time.time() < self._retry_after:
                reason = "provider_error"
                result["error"] = "provider_unavailable"
                result["provider"] = "structured-fallback"
            elif self.provider:
                bounded = {"prompt": data["prompt"][:6000], "student_reports": [t[:2000] for t in data["student_reports"][-3:]]}
                try:
                    summary = self.provider.summarize(json.dumps(bounded, ensure_ascii=False))
                    if not isinstance(summary, str) or not summary.strip():
                        raise ValueError("empty summary")
                    # A model summary remains explicitly attributed, even if it claims success.
                    summary = "나쵸 요약 · 학생 보고 기준\n" + summary.strip()[:400] + "\n실제 반영은 별도 확인이 필요해요."
                    used_provider = provider_name
                    reason = None
                except Exception:
                    # Transport exceptions can contain request bodies or credentials.
                    result["error"] = "provider_unavailable"
                    result["provider"] = "structured-fallback"
                    reason = "provider_error"
                    self._retry_after = time.time() + 300
            store.set_summary(row["id"], summary, evidence={
                "source_hash": digest, "provider": used_provider,
                "reason": reason, "source": "user_prompt_and_student_finals",
                "attempted_provider": provider_name, "attempted_at": time.time(),
            })
            result["updated"] += 1
            if self.provider and reason:
                # An outage must not multiply into one failing call per old request.
                exhausted = False
                break
        if exhausted and len(rows) < 32:
            self._cursor = None
        return result
