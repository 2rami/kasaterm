#!/bin/bash
# PostToolUse 훅 — steer 큐 파일 있으면 additionalContext 로 반환 후 삭제.
# busy 에이전트도 다음 tool call 경계에서 반드시 1회 소비 → tell 씹힘 없음.
PANE="${KASATERM_PANE_ID:-}"
[ -z "$PANE" ] && exit 0
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
