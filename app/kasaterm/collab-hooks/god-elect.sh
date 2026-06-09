#!/bin/bash
# god 선출 hook. pane 2개+ 이고 god 없으면 kasacollab lead claim(O_EXCL 원자)으로
# god 자임 → 노랑 #FFD400 + "● god" + god-loop 강제 기동. 패배/워커는 자동 색.
# board-context.py(UserPromptSubmit)가 매 턴 백그라운드로 호출 → 자가치유한다.
# pane 밖이면 no-op.
CLI="${KASATERM_CLI:-kasaterm-cli}"
ME="${KASATERM_PANE_ID:-}"
[ -z "$ME" ] && exit 0
HOOKS_DIR="$(cd "$(dirname "$0")" && pwd)"
KASACOLLAB="python3 $HOOKS_DIR/kasacollab.py"
slug=$(pwd | sed 's#[/.]#-#g')
LEAD="/tmp/kasaterm-collab/$slug/lead"
GOD_COLOR="#FFD400"

# 워커 색 — pane id 숫자(%5 → 5)로 팔레트에서 안 겹치게(노랑 god 색은 팔레트에
# 없음). board webview JS 가 같은 식으로 색을 재현해 헤더 색과 카드 색이 일치한다.
worker_color() {
  local palette=("#5B9BD5" "#70AD47" "#C00000" "#7030A0" "#ED7D31" "#1F9D8E" "#E84393")
  local num
  num=$(printf '%s' "$ME" | tr -dc '0-9')
  [ -z "$num" ] && num=0
  echo "${palette[$((num % ${#palette[@]}))]}"
}

# 헤더 팔레트의 claude /color 근사색 — 같은 인덱스로 1:1 매핑(yellow 는 god 예약).
worker_claude_color() {
  local names=("blue" "green" "red" "purple" "orange" "cyan" "pink")
  local num
  num=$(printf '%s' "$ME" | tr -dc '0-9')
  [ -z "$num" ] && num=0
  echo "${names[$((num % ${#names[@]}))]}"
}

# god-loop 강제 기동 — claude Monitor 의존 없이 외부 프로세스로 '모니터링 반드시
# 켜짐'을 보장. pkill 로 옛 god 워처 정리해 항상 정확히 1개만 돈다.
# mkdir 원자 락: board-context 가 매 턴 god-elect 를 백그라운드로 쏘므로 두 턴이
# 겹치면 둘 다 '죽었네' 판정 후 둘 다 기동하는 race 로 loop 가 2개 뜬다(실측).
# 락 잡은 쪽만 pkill+기동, 진 쪽은 양보. 보유자 사망 대비 60초 지난 락은 회수.
start_god_loop() {
  local lock="/tmp/kasaterm-collab/$slug/god-loop.lock"
  if [ -d "$lock" ] && [ -n "$(find "$lock" -maxdepth 0 -mmin +1 2>/dev/null)" ]; then
    rmdir "$lock" 2>/dev/null
  fi
  mkdir "$lock" 2>/dev/null || return 0
  pkill -f "god-loop.sh" 2>/dev/null
  nohup bash "$HOOKS_DIR/god-loop.sh" "$ME" >/dev/null 2>&1 &
  rmdir "$lock" 2>/dev/null
}

ensure_god_look() {
  $CLI color "$ME" "$GOD_COLOR" >/dev/null 2>&1
  $CLI rename "$ME" "● god" >/dev/null 2>&1
  # 사이드바 세션 이름도 god 마킹 — pane 헤더만이 아니라 세션 라벨까지(거노 요청).
  # 강등 원복은 안 함(단순화 — god 윈도우만 마킹, 재선출 때 갱신).
  $CLI rename-window "● god" >/dev/null 2>&1
  # god 인데 워처가 죽어있으면 조용히 재기동(자가치유) — '반드시 켜짐'.
  pgrep -f "god-loop.sh $ME" >/dev/null 2>&1 || start_god_loop
}

ensure_worker_look() {
  $CLI color "$ME" "$(worker_color)" >/dev/null 2>&1
  # claude 프롬프트 바도 헤더 근사색으로. 매 턴 재주입하면 강제 제출 스팸이라
  # pane 당 1회 마커. god-elect 는 claude hook 에서만 돌므로 이 pane 엔 claude
  # 가 떠 있다는 게 보장된다(셸에 /color 오타칠 일 없음).
  local marker="/tmp/kasaterm-collab/$slug/claude-colored-${ME//[^A-Za-z0-9]/_}"
  if [ ! -f "$marker" ]; then
    touch "$marker"
    $CLI tell "$ME" "/color $(worker_claude_color)" >/dev/null 2>&1
  fi
}

# 살아있는 pane id 목록 (전체 — 아래 lead 생존 확인·room 교집합에 씀)
surfaces=$($CLI list surfaces 2>/dev/null | python3 -c '
import sys, json
try:
    print("\n".join(s["id"] for s in json.load(sys.stdin)["result"]["surfaces"]))
except Exception:
    pass
')

# 같은 방(slug) claude pane 만 카운트해 선출 게이트로 쓴다. list surfaces 전체는
# 다른 레포 pane 까지 세서, 다른 경로에서 혼자 작업하는 사람에게도 god 치장이
# 붙고 세션 이름 "● god" 가 방마다 겹친다(거노 발견). board-context.py 가 매 턴
# 쓰는 ctx-cache-<N>(pane %N)이 '이 방에서 claude 가 돌았다'는 증거 — surfaces 와
# 교집합해 닫힌 pane 의 잔존 캐시를 거르고, 자기 자신은 무조건 포함한다(god-elect
# 실행 자체가 이 방 claude 증거, 첫 턴 전 ctx-cache 부재 보완). 첫 턴 전 동료
# 누락은 다음 턴 자가치유라 허용. 같은 방 2+ 일 때만 선출 진행.
BASE="/tmp/kasaterm-collab/$slug"
room_count=$(
  {
    printf '%s\n' "$ME"
    for f in "$BASE"/ctx-cache-*; do
      [ -e "$f" ] || continue
      n="${f##*/ctx-cache-}"
      printf '%s\n' "$surfaces" | grep -qx "%$n" && printf '%%%s\n' "$n"
    done
  } | sort -u | grep -c .
)

# 같은 방에 혼자(또는 0)면 god 불필요
[ "${room_count:-0}" -lt 2 ] && exit 0

cur_god=""
[ -f "$LEAD" ] && cur_god=$(cat "$LEAD" 2>/dev/null)

# lead 가 살아있는 pane 을 가리키면: 내가 god 이면 표시 보정, 아니면 워커 색.
if [ -n "$cur_god" ] && printf '%s\n' "$surfaces" | grep -qx "$cur_god"; then
  if [ "$cur_god" = "$ME" ]; then
    ensure_god_look
  else
    ensure_worker_look
  fi
  exit 0
fi

# lead 없거나 stale(죽은 god 가리킴) → 정리 후 원자 선점 경쟁.
[ -n "$cur_god" ] && $KASACOLLAB lead off >/dev/null 2>&1
if $KASACOLLAB lead claim >/dev/null 2>&1; then
  ensure_god_look
  # claude 세션 타이틀도 god 으로 — 재시작 후 `claude --resume god` 한 방으로
  # god 세션을 이어가게(타이틀 resume 은 Claude Code 공식 지원). 프롬프트 바
  # 색도 god 노랑으로(헤더 #FFD400 과 짝). 선출 순간 1회만 주입(매 턴
  # 재적용하면 강제 제출 스팸이라 claim 분기에만).
  $CLI tell "$ME" "/rename god" >/dev/null 2>&1
  sleep 1
  $CLI tell "$ME" "/color yellow" >/dev/null 2>&1
  sleep 1
  $CLI tell "$ME" "[god] 너가 god 이다. 팀 통솔 시작 — board-watch 로 변경점 감시, 워커 done 보고 받으면 단독 커밋." >/dev/null 2>&1
else
  ensure_worker_look
fi
exit 0
