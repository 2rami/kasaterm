#!/bin/bash
# SessionStart hook: kasaterm pane에서 시작한 claude에게 협업 체계를 안내한다.
# 감지·차단·분담·대화 대부분이 자동(hook)이므로, claude가 알아야 할 건 "능동적으로
# 쓸 도구 이름"과 "자동으로 뭐가 오는지"뿐이다. kasaterm 밖이면 no-op.
[ -z "$KASATERM_PANE_ID" ] && exit 0
cat >/dev/null  # stdin(hook payload) 소비

read -r -d '' CTX <<'EOF'
[kasaterm 협업 환경] 이 pane은 다른 pane들과 같은 레포를 동시에 만질 수 있다. 협업은 대부분 자동이다:

- 매 턴 시작 시 [협업 보드]가 자동 주입된다 — 다른 pane이 뭐 하는지, 새 pane이 합류했는지, 현재 작업 분담, 내게 온 메시지. 따로 조회할 필요 없다.
- 같은 파일을 동시에 만지려 하면 conflict-guard가 자동으로 막고, 상대가 뭐 하는지 + 합류/회피 옵션을 알려준다.
- 다른 pane의 작업 시작/끝도 매 턴 [협업 보드]/[협업 알림]으로 자동 주입된다(pull monitor). 따로 watcher를 걸 필요 없다 — 그건 폐기됐다. 급히 깨워야 하면 kasaterm-cli tell %N "메시지".

네가 능동적으로 할 것:
- 작업을 맡을 때 선언: kasacollab task add "무슨 일" (다른 pane과 안 겹치게). 끝나면 kasacollab task done <id>.
- 다른 pane에 말 걸기: kasacollab msg %N "메시지" (상대가 자기 턴에 읽는다). 급히 깨우려면 kasaterm-cli tell %N "메시지" (idle claude를 즉시 깨운다).
- kasacollab = python3 ~/.claude/hooks/kasacollab.py — task add|list|done, msg, inbox.

혼자 작업이면(다른 pane 없음) 보드는 조용하니 신경 쓸 것 없다.
EOF

jq -n --arg ctx "$CTX" '{
  continue: true,
  hookSpecificOutput: {
    hookEventName: "SessionStart",
    additionalContext: $ctx
  }
}'
exit 0
