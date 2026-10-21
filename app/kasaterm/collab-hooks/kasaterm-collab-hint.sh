#!/bin/bash
# 보관된 SessionStart 안내. 협업 규약은 kasapane 스킬(skills/kasapane/collab.md)에 있다.
# 수동 등록 환경에서도 현행 주소·전달 규칙과 충돌하지 않게 유지한다.
[ -z "$KASATERM_PANE_ID" ] && exit 0
cat >/dev/null  # stdin(hook payload) 소비

read -r -d '' CTX <<'EOF'
[Kasaterm] Before collaborating, read `kasaterm-cli guide collab` or the kasapane skill's collab.md. Other guides are needed only for their task.
EOF

jq -n --arg ctx "$CTX" '{
  continue: true,
  hookSpecificOutput: {
    hookEventName: "SessionStart",
    additionalContext: $ctx
  }
}'
exit 0
