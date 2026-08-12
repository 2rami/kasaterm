#!/bin/bash
# SessionStart hook: kasaterm pane에서 시작한 claude에게 협업 체계를 안내한다.
# 감지·차단·분담·대화 대부분이 자동(hook)이므로, claude가 알아야 할 건 "능동적으로
# 쓸 도구 이름"과 "자동으로 뭐가 오는지"뿐이다. kasaterm 밖이면 no-op.
[ -z "$KASATERM_PANE_ID" ] && exit 0
cat >/dev/null  # stdin(hook payload) 소비

read -r -d '' CTX <<'EOF'
[kasaterm 협업 환경] 이 pane은 다른 pane들과 같은 레포를 동시에 만질 수 있다.

- 같은 파일을 동시에 만지려 하면 conflict-guard가 자동으로 막고, 상대가 뭐 하는지 + 합류/회피 옵션을 알려준다.
- 다른 pane이 뭐 하는지 보려면: kasaterm-cli board (제목·시킨일·최근답변·도구), peek %N (화면), transcript %N (대화). board는 호출 시점 pull이라 항상 최신이다.
- 전체를 계속 감시하려면(팀장/오케스트레이터): Monitor 도구로 `kasaterm-cli board-watch 3` 를 persistent로 걸면 pane 상태가 바뀔 때마다(working↔idle↔building, 합류/종료) 알림이 온다.

네가 능동적으로 할 것:
- 작업을 맡을 때 선언: kasacollab task add "무슨 일" (다른 pane과 안 겹치게). 끝나면 kasacollab task done <id>.
- 다른 pane이 시킨 작업(브리프)을 마쳤으면 마지막에: kasaterm-cli done succeeded "한 줄: 뭘 했고 뭐가 남았나" — board에 완료가 정본으로 뜬다(추정 아님). 실패로 끝나도 숨기지 말고 failed로 같은 보고를 해라. (kasacollab task done은 태스크 목록 정리, kasaterm-cli done은 pane 완료 보고 — 다른 것)
- 다른 pane에 말 걸기: kasacollab msg %N "메시지" (상대가 kasacollab inbox로 확인). 급히 깨우려면 kasaterm-cli tell %N "메시지" (idle claude를 즉시 깨운다).
- kasacollab = python3 ~/.claude/hooks/kasacollab.py — task add|list|done, msg, inbox.

혼자 작업이면(다른 pane 없음) 신경 쓸 것 없다.
EOF

jq -n --arg ctx "$CTX" '{
  continue: true,
  hookSpecificOutput: {
    hookEventName: "SessionStart",
    additionalContext: $ctx
  }
}'
exit 0
