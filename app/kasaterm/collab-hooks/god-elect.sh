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

# 모드 게이트 — 기본 solo(거노가 직접 오케스트레이션 + conflict-guard 가 파일
# 겹침 차단). god 은 옵트인(`kasacollab mode god`). solo 면 선출/색/표시 일절
# 안 하고 조용히 종료한다. god→solo 전환으로 lead 가 남아있으면 제거(다른 pane
# 들이 board 에서 stale god 을 안 보게).
if [ "$($KASACOLLAB mode show 2>/dev/null)" != "god" ]; then
  [ -f "$LEAD" ] && $KASACOLLAB lead off >/dev/null 2>&1
  exit 0
fi

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

# 내 세션 sid = bind 마커(inode:transcript_path)의 uuid. roster 의 role:god
# session_id 와 비교해 '이전 god 세션' 우선권을 판정한다. 마커 부재(첫 턴 race)면
# sid 불명 → 양보 경로로 빠져 2초 뒤 마커 생겨 자가수렴(순서 의존 회피).
marker="/tmp/kasaterm-bound-${ME//[^A-Za-z0-9]/_}"
my_sid=""
[ -f "$marker" ] && my_sid=$(sed 's#.*/##; s#\.jsonl$##' "$marker" 2>/dev/null)
god_sid=$($KASACOLLAB roster-god-sid 2>/dev/null)

# lead 가 살아있는 pane 을 가리키면: 내가 god 이면 표시 보정, 아니면 워커 색.
if [ -n "$cur_god" ] && printf '%s\n' "$surfaces" | grep -qx "$cur_god"; then
  if [ "$cur_god" = "$ME" ]; then
    ensure_god_look
    # roster role:god 을 내 세션으로 유지(claim 턴에 마커가 없어 마킹을 놓쳤어도
    # 여기서 수렴 — 재시작 우선권 기준이 항상 최신 god 세션을 가리키게).
    [ -n "$my_sid" ] && [ "$god_sid" != "$my_sid" ] && \
      $KASACOLLAB roster-mark-god "$my_sid" >/dev/null 2>&1
  else
    ensure_worker_look
  fi
  exit 0
fi

# lead 없거나 stale(죽은 god 가리킴) → 정리 후 원자 선점 경쟁.
[ -n "$cur_god" ] && $KASACOLLAB lead off >/dev/null 2>&1

# god 세션 우선권: 재선출이 선착순이면 '이전 god 세션'(거노 대면 + god 타이틀)이
# 늦게 떠도 다른 pane 에 god 을 뺏긴다(거노 발견). 내가 이전 god 세션이면 즉시
# claim, 아니면 2초 양보해 god 세션이 먼저 claim 하게 둔다(roster 는 lead 와 달리
# 재시작 청소에 안 지워져 우선권 기준으로 영속한다).
if [ -n "$god_sid" ] && [ "$god_sid" != "$my_sid" ]; then
  # 이전 god 세션이 따로 있다(또는 내 sid 불명) → 그 세션이 돌아올 2초를 준다.
  # mkdir 원자 락으로 한 turn 만 양보(매 턴 fire 되는 god-elect 가 sleep 을
  # 중첩 누적하는 걸 막는다). 락 못 잡으면 다른 elect 가 이미 양보 중 → 워커.
  ylock="$BASE/god-yield.lock"
  if mkdir "$ylock" 2>/dev/null; then
    sleep 2
    rmdir "$ylock" 2>/dev/null
    ng=$(cat "$LEAD" 2>/dev/null)
    if [ -n "$ng" ] && printf '%s\n' "$surfaces" | grep -qx "$ng"; then
      ensure_worker_look; exit 0   # god 세션이 그 사이 claim 함 → 워커
    fi
    # 2초 뒤에도 lead 비었으면 god 세션이 안 떴다고 보고 인계 claim(아래로)
  else
    ensure_worker_look; exit 0
  fi
fi

if $KASACOLLAB lead claim >/dev/null 2>&1; then
  ensure_god_look
  # claude 세션 타이틀 god 주입은 roster 의 god 세션이 없거나 그게 곧 나일 때만 —
  # 선착순으로 임시 claim 한 경우(god_sid 가 딴 세션) 타이틀 god 중복을 피해 보류
  # (강등 시 자동 원복은 claude slash 한계로 불가하므로 애초에 안 친다). 타이틀
  # resume(`claude --resume god`)·색은 진짜 god 세션 1회 주입에만.
  if [ -z "$god_sid" ] || [ "$god_sid" = "$my_sid" ]; then
    $CLI tell "$ME" "/rename god" >/dev/null 2>&1
    sleep 1
    $CLI tell "$ME" "/color yellow" >/dev/null 2>&1
    sleep 1
    $CLI tell "$ME" "[god] 너가 god 이다. 팀 통솔 시작 — board-watch 로 변경점 감시, 워커 done 보고 받으면 단독 커밋." >/dev/null 2>&1
  fi
  # 내가 god 됐으니 roster 에 인계 마킹 — 다음 재선출의 우선권 기준.
  [ -n "$my_sid" ] && $KASACOLLAB roster-mark-god "$my_sid" >/dev/null 2>&1
else
  ensure_worker_look
fi
exit 0
