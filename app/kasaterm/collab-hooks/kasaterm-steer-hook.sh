#!/bin/bash
# PostToolUse 훅 — 기다리던 tell 이나 steer 큐 파일을 additionalContext 로 넘긴다.
# busy 에이전트도 다음 tool call 경계에서 받아 간다.
PANE="${KASATERM_PANE_ID:-}"
[ -z "$PANE" ] && exit 0
# 일하는 세션에 기다리던 tell 이 있으면 도구 사이에 하나 받아 간다(docs/tell-protocol.md 「세션이 받아 가기」).
out="$(kasaterm-cli turn tool 2>/dev/null)"
case "$out" in
  '{"hookSpecificOutput"'*) printf '%s\n' "$out"; exit 0 ;;
esac
slug=$(pwd | sed 's#[/.]#-#g')${KASATERM_ROOM:+__room_$KASATERM_ROOM}
ENC="${PANE//%/_pane_}"
STEER="/tmp/kasaterm-collab/$slug/steer/${ENC}.txt"
[ -f "$STEER" ] || exit 0
MSG=$(cat "$STEER" 2>/dev/null)
[ -z "$MSG" ] && { rm -f "$STEER"; exit 0; }
rm -f "$STEER"
python3 -c "
import json, sys
msg = sys.argv[1]
print(json.dumps({'hookSpecificOutput': {'additionalContext': msg}}, ensure_ascii=False))
" "$MSG"
exit 0
