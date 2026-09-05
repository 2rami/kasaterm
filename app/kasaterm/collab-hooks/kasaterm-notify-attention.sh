#!/bin/bash
# Notification hook: claude in THIS pane is blocked on a permission prompt, is
# asking a question, or has gone idle waiting for input → flag the pane on the
# kasaterm board with the **kind** of wait and raise a desktop alert unless
# you're already looking at that exact pane. This fills the gap the
# transcript-board can't see: a blocked claude writes nothing, so without this
# hook a stuck background pane reads as idle. The flag clears itself once claude
# resumes (transcript grows) or its Stop hook fires. No-op outside a kasaterm pane.
#
# 종류(kind): permission(승인) · question(질문·선택) · idle(답 없이 60초 넘게 방치).
# 페이로드의 notification_type 이 정본이고, 없으면 $1(등록 시 matcher 별로 건 인자).
# auth_success·agent_completed 처럼 사람을 기다리는 게 아닌 알림은 건너뛴다.
#
# $KASATERM_PANE_ID is injected by pty-backend when the pane spawns; the
# Notification payload arrives on stdin.
[ -z "$KASATERM_PANE_ID" ] && exit 0
payload="$(cat 2>/dev/null)"
out="$(printf '%s' "$payload" | KIND_ARG="${1:-}" python3 -c 'import os, sys, json
try:
    d = json.load(sys.stdin)
except Exception:
    d = {}
t = str(d.get("notification_type") or d.get("type") or "")
kind = {
    "permission_prompt": "permission",
    "idle_prompt": "idle",
    "elicitation_dialog": "question",
    "elicitation_url_dialog": "question",
    "agent_needs_input": "question",
}.get(t)
if kind is None and t:
    raise SystemExit(0)          # 사람을 기다리는 알림이 아니다
if kind is None:
    kind = os.environ.get("KIND_ARG") or ""
print(kind)
print((d.get("message") or "").strip())' 2>/dev/null)"
[ -n "$out" ] || exit 0
kind="$(printf '%s\n' "$out" | sed -n 1p)"
reason="$(printf '%s\n' "$out" | sed -n 2p)"
if [ -n "$kind" ]; then
  kasaterm-cli attention --kind "$kind" "$reason" >/dev/null 2>&1 || true
else
  kasaterm-cli attention "$reason" >/dev/null 2>&1 || true
fi
exit 0
