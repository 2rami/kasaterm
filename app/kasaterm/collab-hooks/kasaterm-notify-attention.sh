#!/bin/bash
# Notification hook: claude in THIS pane is blocked on a permission prompt or
# has gone idle waiting for input → flag the pane on the kasaterm board
# ("⚠ 권한 대기중") and raise a desktop alert unless you're already looking at
# that exact pane. This fills the gap the transcript-board can't see: a blocked
# claude writes nothing, so without this hook a stuck background pane reads as
# idle. The flag clears itself once claude resumes (transcript grows) or its
# Stop hook fires. No-op outside a kasaterm pane.
#
# $KASATERM_PANE_ID is injected by pty-backend when the pane spawns; the
# Notification payload arrives on stdin — we pull its human-readable `message`
# (e.g. "Claude needs your permission to use Bash") to show as the reason.
[ -z "$KASATERM_PANE_ID" ] && exit 0
payload="$(cat 2>/dev/null)"
reason="$(printf '%s' "$payload" | python3 -c 'import sys, json
try:
    print((json.load(sys.stdin).get("message") or "").strip())
except Exception:
    pass' 2>/dev/null)"
kasaterm-cli attention "$reason" >/dev/null 2>&1 || true
exit 0
