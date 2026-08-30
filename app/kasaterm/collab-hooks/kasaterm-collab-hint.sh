#!/bin/bash
# SessionStart hook: kasaterm pane에서 시작한 claude에게 협업 체계를 안내한다.
# 감지·차단·분담·대화 대부분이 자동(hook)이므로, claude가 알아야 할 건 "능동적으로
# 쓸 도구 이름"과 "자동으로 뭐가 오는지"뿐이다. kasaterm 밖이면 no-op.
[ -z "$KASATERM_PANE_ID" ] && exit 0
cat >/dev/null  # stdin(hook payload) 소비

read -r -d '' CTX <<'EOF'
[kasaterm 협업 환경] 이 pane은 다른 pane들과 같은 레포를 동시에 만질 수 있다.

- 같은 파일을 동시에 만지려 하면 conflict-guard가 자동으로 막고, 상대가 뭐 하는지 + 합류/회피 옵션을 알려준다.
- 다른 pane이 뭐 하는지 보려면: kasaterm-cli board (제목·시킨일·최근답변·도구), peek %N (화면), transcript %N (대화), activity %N (실제로 친 명령과 그 결과·성패를 시간순으로). board는 호출 시점 pull이라 항상 최신이다. board는 도구를 최근 8개만 싣고 성패를 안 주니 「쟤 뭐 하나」까지가 board고 「쟤 왜 저러나」는 activity다 — 같은 오류 반복은 board로 원리적으로 안 보인다. 대신 activity는 한 사람 것이 board 전체보다 크니 지목할 때만.
- 사용자가 닫은 pane은 화면에 없는데도 그 안의 claude는 계속 돈다. 명부(ListAgents)에는 닫힘이 안 보이므로 board의 detached(화면밖)로만 알 수 있다 — 거기 새 일을 시키면 사용자가 못 보는 곳에서 작업이 돈다. 잊고 보내도 SendMessage 직전에 자동으로 막히니, 막히면 새 pane을 쪼개거나 사용자에게 되살리기를 부탁해라.
- 전체를 계속 감시하려면(팀장/오케스트레이터): Monitor 도구로 `kasaterm-cli board-watch 3` 를 persistent로 걸면 pane 상태가 바뀔 때마다(working↔idle↔building, 합류/종료) 알림이 온다.

네가 능동적으로 할 것:
- 작업을 맡을 때 선언: kasacollab task add "무슨 일" (다른 pane과 안 겹치게). 끝나면 kasacollab task done <id>.
- 다른 pane이 시킨 작업(브리프)을 마쳤으면 마지막에: kasaterm-cli done succeeded "한 줄: 뭘 했고 뭐가 남았나" — board에 완료가 정본으로 뜬다(추정 아님). 실패로 끝나도 숨기지 말고 failed로 같은 보고를 해라. (kasacollab task done은 태스크 목록 정리, kasaterm-cli done은 pane 완료 보고 — 다른 것)
- 다른 pane에 말 걸기 — **통로가 상대에 따라 갈린다.** claude pane이면 SendMessage: cross-session 명부에 올라 있어 유휴로 프롬프트만 떠 있어도 읽고, 상대 화면을 안 어지럽힌다. claude가 아닌 pane(codex·agy·opencode·gemini·cursor…)은 그 명부에 안 올라 SendMessage가 아예 안 닿으니 kasaterm-cli tell %N "메시지" — 상대 입력창에 글자를 밀어넣는 것이라 타이핑 중이면 섞인다. 무엇으로 도는지는 kasaterm-cli board의 harness로 본다(하네스는 claude·codex·agy 말고도 서른 종이 넘는다). ⚠️둘을 겹쳐 보내지 마라 — 같은 말이 상대 화면에 두 번 뜨고 상대가 두 번 깨어난다. kasacollab msg %N "메시지"는 상대가 kasacollab inbox로 확인하는 비동기 쪽지라 급하지 않을 때 쓴다.
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
