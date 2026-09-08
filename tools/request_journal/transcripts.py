"""Normalize complete JSONL records, excluding harness and tool injections."""

from datetime import datetime, timezone
import hashlib
import json
import os
import re

INJECTION_PREFIXES = (
    "# AGENTS.md instructions", "<INSTRUCTIONS>", "<user_instructions>",
    "<environment_context>", "<skills_instructions>", "<recommended_plugins>",
    "<teammate-message", "<system-reminder>", "<local-command-caveat>",
    "<local-command-stdout>", "<command-name>", "[Request interrupted",
    "<cross-session-message", "<task-notification", "<command-message>",
    "<command-args>", "<local-command-stderr>", "<bash-input>",
    "<bash-stdout>", "<bash-stderr>", "Caveat:", "Your tool call was malformed",
    "This session is being continued from a previous conversation",
    "Message Type: NEW_TASK", "Message Type: MESSAGE", "Message Type: FINAL_ANSWER",
    "You are a worker agent", "You are an agent in a team of agents",
)


def is_injection(text):
    stripped = text.lstrip()
    return stripped.startswith(INJECTION_PREFIXES) or bool(re.match(r"^\[.*?\]\s*<(?:system|teammate)", stripped))


def content_text(content, user=False):
    if isinstance(content, str):
        chunks = [content]
    elif isinstance(content, list):
        # A tool-result wrapper is not a human message even when its siblings
        # contain a textual explanation of the tool response.
        if user and any(isinstance(c, dict) and c.get("type") in {"tool_result", "function_call_output"} for c in content):
            return ""
        chunks = [c.get("text", "") for c in content if isinstance(c, dict) and c.get("type") in {"text", "input_text", "output_text"}]
    else:
        return ""
    return "\n".join(c for c in chunks if isinstance(c, str) and c.strip() and (not user or not is_injection(c)))


def _stamp(raw):
    value = raw.get("timestamp") or raw.get("created_at")
    if isinstance(value, (int, float)):
        return datetime.fromtimestamp(value / 1000 if value > 1e11 else value, timezone.utc).isoformat().replace("+00:00", "Z")
    if isinstance(value, str):
        try:
            return datetime.fromisoformat(value.replace("Z", "+00:00")).astimezone(timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")
        except ValueError:
            return None
    return None


def _near(a, b):
    if not a or not b:
        return True
    try:
        return abs((datetime.fromisoformat(a.replace("Z", "+00:00")) - datetime.fromisoformat(b.replace("Z", "+00:00"))).total_seconds()) < 2
    except ValueError:
        return False


def _pair_duplicate(state, role, origin, text, stamp):
    """Codex mirrors one utterance in two record families; never dedup by text alone."""
    key = "pair_" + role
    digest = hashlib.sha256(text.encode()).hexdigest()
    last = state.get(key)
    if last and not last["paired"] and last["origin"] != origin and last["digest"] == digest and _near(last.get("stamp"), stamp):
        last["paired"] = True
        return True
    state[key] = {"origin": origin, "digest": digest, "stamp": stamp, "paired": False}
    return False


def parse_record(raw, harness, position, generation, state, project=None):
    if not isinstance(raw, dict):
        return []
    payload = raw.get("payload") if isinstance(raw.get("payload"), dict) else raw
    cwd = raw.get("cwd") or payload.get("cwd")
    if isinstance(cwd, str):
        state["cwd"] = cwd
    stamp = _stamp(raw) or _stamp(payload)
    native = raw.get("uuid") or raw.get("event_id") or payload.get("id")
    base = "native:" + str(native) if native else f"line:{stamp or generation}:{position}"
    evidence = {"harness": harness, "record_type": raw.get("type"), "byte_offset": position, "generation": generation}

    def event(kind, text="", suffix=""):
        return {"event_key": base + suffix, "kind": kind, "text": text, "created_at": stamp, "evidence": evidence}

    if harness == "codex":
        outer, kind = raw.get("type"), payload.get("type")
        if outer == "session_meta":
            origin = payload.get("source")
            state["subagent"] = (isinstance(origin, dict) and "subagent" in origin) or payload.get("originator") == "codex_exec"
            return []
        if state.get("subagent"):
            return []
        role = None
        if outer == "event_msg" and kind == "user_message":
            role, text, family = "user", content_text(payload.get("message"), user=True), "event"
        elif (outer == "response_item" or outer == "message") and payload.get("role") == "user":
            role, text, family = "user", content_text(payload.get("content"), user=True), "response"
        elif outer == "event_msg" and kind == "agent_message":
            role, text, family = "assistant", content_text(payload.get("message")), "event"
        elif (outer == "response_item" or outer == "message") and payload.get("role") == "assistant":
            if payload.get("channel") in {"analysis", "reasoning"}:
                return []
            role, text, family = "assistant", content_text(payload.get("content")), "response"
        elif (outer == "response_item" and kind in {"function_call", "custom_tool_call"}) or (outer == "event_msg" and kind == "task_started"):
            state.pop("pair_user", None)
            return [event("activity")]
        else:
            return []
        if not text:
            return []
        if _pair_duplicate(state, role, family, text, stamp):
            return []
        if role == "user":
            state.pop("pair_assistant", None)
            return [event("user", text)]
        state.pop("pair_user", None)
        final = payload.get("channel") == "final" or payload.get("phase") == "final_answer"
        return [event("assistant_final" if final else "assistant_note", text)]

    if harness == "claude":
        if any(raw.get(flag) is True for flag in ("isMeta", "isSidechain", "isCompactSummary", "isVisibleInTranscriptOnly")):
            return []
        message = raw.get("message") if isinstance(raw.get("message"), dict) else raw
        role = raw.get("type") or message.get("role")
        if role == "user":
            text = content_text(message.get("content"), user=True)
            return [event("user", text)] if text else []
        if role == "assistant":
            content = message.get("content")
            tools = isinstance(content, list) and any(isinstance(c, dict) and c.get("type") == "tool_use" for c in content)
            text = content_text(content)
            result = [event("activity", suffix=":activity")] if tools else []
            if text:
                final = message.get("stop_reason") == "end_turn" or message.get("channel") == "final"
                result.append(event("assistant_final" if final and not tools else "assistant_note", text, ":text"))
            return result
    return []


def parse_lines(data, *, harness, offset=0, generation=0, state=None, project=None, max_line_bytes=16 * 1024 * 1024):
    """Return (events, complete_bytes_consumed, next_state); leave partial UTF-8/JSON unread."""
    if not isinstance(data, bytes):
        raise TypeError("JSONL input must be bytes for accurate offsets")
    state = dict(state or {})
    # Nested pair state is copied because callers retain the previous checkpoint
    # until Store.ingest succeeds.
    for key in ("pair_user", "pair_assistant"):
        if key in state:
            state[key] = dict(state[key])
    boundary = data.rfind(b"\n") + 1
    if not boundary:
        return [], 0, state
    events, consumed = [], 0
    for line in data[:boundary].splitlines(keepends=True):
        position = offset + consumed
        consumed += len(line)
        if len(line) > max_line_bytes:
            state["oversized_lines"] = state.get("oversized_lines", 0) + 1
            continue
        try:
            raw = json.loads(line)
        except (ValueError, UnicodeDecodeError):
            state["invalid_lines"] = state.get("invalid_lines", 0) + 1
            continue
        parsed = parse_record(raw, harness, position, generation, state, project)
        if project and state.get("cwd"):
            try:
                inside = os.path.commonpath([os.path.realpath(project), os.path.realpath(state["cwd"])]) == os.path.realpath(project)
            except ValueError:
                inside = False
            if not inside:
                state.pop("pair_user", None)
                continue
        events.extend(parsed)
    return events, consumed, state
