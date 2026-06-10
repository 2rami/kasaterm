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
  # solo 복귀: 워커 1회 색 마커를 정리해 다음 god 전환 때 재주입되게.
  # (persona 컨텍스트는 board-context.py 의 additionalContext 주입으로 이전 —
  #  tell 이 아니라 마커가 필요 없다.)
  rm -f "/tmp/kasaterm-collab/$slug/claude-colored-${ME//[^A-Za-z0-9]/_}" 2>/dev/null
  exit 0
fi

# 캐릭터 테마(있으면): god=leader, 워커=member. 헤더색·이름·인사·claude /color 를
# 캐릭터로 입힌다. ~/.config 우선→번들. 없으면 기본(노랑 #FFD400 / "god").
CHARS_JSON=""
for cf in "$HOME/.config/kasaterm/characters.json" "$HOOKS_DIR/characters.json"; do
  [ -f "$cf" ] && { CHARS_JSON="$cf"; break; }
done
GOD_NAME="god"
GOD_CLAUDE_COLOR="yellow"
GOD_GREETING="[god] 너가 god 이다. 팀 통솔 시작 — board-watch 로 변경점 감시, 워커 done 보고 받으면 단독 커밋."
# 캐릭터 JSON 필드 추출 헬퍼: char_field <name> <field> (leader+members 통합 조회).
char_field() {
  python3 -c "import json,sys
try:
    d=json.load(open(sys.argv[1])); ms=[d.get('leader') or {}]+(d.get('members') or [])
    print(next((m.get(sys.argv[3],'') for m in ms if m.get('name')==sys.argv[2]),''))
except Exception: pass" "$CHARS_JSON" "$1" "$2" 2>/dev/null
}
if [ -n "$CHARS_JSON" ]; then
  _ln=$(python3 -c "import json;print((json.load(open('$CHARS_JSON')).get('leader') or {}).get('name',''))" 2>/dev/null)
  if [ -n "$_ln" ]; then
    GOD_NAME="$_ln"
    GOD_COLOR=$(char_field "$_ln" header_color); [ -z "$GOD_COLOR" ] && GOD_COLOR="#FFD400"
    GOD_CLAUDE_COLOR=$(char_field "$_ln" claude_color); [ -z "$GOD_CLAUDE_COLOR" ] && GOD_CLAUDE_COLOR="yellow"
    _gr=$(python3 -c "import json;print((json.load(open('$CHARS_JSON')).get('leader') or {}).get('greeting',''))" 2>/dev/null)
    [ -n "$_gr" ] && GOD_GREETING="$_gr"
  fi
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
# 켜짐'을 보장. 옛 워처 정리는 god-loop 자신의 후발 교체 락이 한다(같은 slug
# 의 선점자만 정확히 죽임) — 여기 있던 전역 pkill -f 는 다른 방의 워처까지
# 죽이는 오폭이라 제거(2026-06-10). mkdir 락은 board-context 가 매 턴 god-elect
# 를 백그라운드로 쏠 때 nohup 중복 발사를 줄이는 dedupe 로만 유지(겹쳐 발사돼도
# god-loop 락이 1개로 수렴시키니 정확성은 거기서 보장). 보유자 사망 대비 60초
# 지난 락은 회수.
start_god_loop() {
  local lock="/tmp/kasaterm-collab/$slug/god-loop.lock"
  if [ -d "$lock" ] && [ -n "$(find "$lock" -maxdepth 0 -mmin +1 2>/dev/null)" ]; then
    rmdir "$lock" 2>/dev/null
  fi
  mkdir "$lock" 2>/dev/null || return 0
  nohup bash "$HOOKS_DIR/god-loop.sh" "$ME" >/dev/null 2>&1 &
  rmdir "$lock" 2>/dev/null
}

ensure_god_look() {
  # 치장(리더 이름·색·세션 라벨)은 **진짜 리더 세션**일 때만 — roster 의 god 세션이
  # 따로 있는데 임시 인계자가 "● 아로나"를 뒤집어쓰면 화면에 아로나가 둘 생긴다
  # (거노 실측). 임시 인계자는 자기 캐릭터 look 을 유지한 채 역할(커밋·워처)만 수행.
  # TAKEOVER=1(유예 만료 정식 승계)은 치장 허용.
  if [ -z "$god_sid" ] || [ "$god_sid" = "$my_sid" ] || [ "$TAKEOVER" = "1" ]; then
    $CLI color "$ME" "$GOD_COLOR" >/dev/null 2>&1
    $CLI rename "$ME" "● $GOD_NAME" >/dev/null 2>&1
    # 사이드바 세션 이름도 god 마킹 — pane 헤더만이 아니라 세션 라벨까지(거노 요청).
    # 강등 원복은 안 함(단순화 — god 윈도우만 마킹, 재선출 때 갱신).
    $CLI rename-window "● $GOD_NAME" >/dev/null 2>&1
  else
    ensure_worker_look
  fi
  # god 인데 워처가 죽어있으면 조용히 재기동(자가치유) — '반드시 켜짐'.
  pgrep -f "god-loop.sh $ME" >/dev/null 2>&1 || start_god_loop
}

ensure_worker_look() {
  # 캐릭터(assign-character 가 박은 character-<N> 마커)가 있으면 그 색, 없으면
  # pane 숫자 해시 색. 헤더색·claude /color 둘 다 캐릭터 우선.
  local hc="" cc="" cname=""
  if [ -n "$CHARS_JSON" ]; then
    local cm="/tmp/kasaterm-collab/$slug/character-${ME#%}"
    [ -f "$cm" ] && cname=$(cat "$cm" 2>/dev/null)
  fi
  if [ -n "$cname" ]; then
    hc=$(char_field "$cname" header_color)
    cc=$(char_field "$cname" claude_color)
    # 헤더 이름도 캐릭터로 복원 — 임시 god 인계가 "● 아로나"를 씌웠다 강등되면
    # rename 이 박제되던 구멍(거노 실측 '아로나 두 개'). idempotent.
    $CLI rename "$ME" "● $cname" >/dev/null 2>&1
  fi
  [ -z "$hc" ] && hc=$(worker_color)
  [ -z "$cc" ] && cc=$(worker_claude_color)
  $CLI color "$ME" "$hc" >/dev/null 2>&1
  # claude 프롬프트 바도 헤더 근사색으로. 매 턴 재주입하면 강제 제출 스팸이라
  # pane 당 1회 마커. god-elect 는 claude hook 에서만 돌므로 이 pane 엔 claude
  # 가 떠 있다는 게 보장된다(셸에 /color 오타칠 일 없음).
  local marker="/tmp/kasaterm-collab/$slug/claude-colored-${ME//[^A-Za-z0-9]/_}"
  if [ ! -f "$marker" ]; then
    touch "$marker"
    $CLI tell "$ME" "/color $cc" >/dev/null 2>&1
  fi
  # 워커 persona 주입은 board-context.py(_worker_persona)의 additionalContext
  # 로 이전 — tell 주입은 사용자 화면에 노출돼 몰입을 깼다(거노 지시).
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
    # roster role:god 수렴은 **마킹이 비어 있을 때만**(claim 턴에 내 마커가 없어
    # 마킹을 놓친 보정). god_sid 가 딴 세션을 가리키면 나는 임시 인계자 — 여기서
    # 내 sid 로 덮으면 진짜 god 세션의 우선권이 소멸한다(반환 전제 유지).
    [ -n "$my_sid" ] && [ -z "$god_sid" ] && \
      $KASACOLLAB roster-mark-god "$my_sid" >/dev/null 2>&1
    # 진짜 god 이 자리에 있으면 남은 양보 유예 마커는 정리.
    [ "$god_sid" = "$my_sid" ] && rm -f "$BASE/god-yield-since" 2>/dev/null
  else
    ensure_worker_look
  fi
  exit 0
fi

# lead 없거나 stale(죽은 god 가리킴) → 정리 후 원자 선점 경쟁.
[ -n "$cur_god" ] && $KASACOLLAB lead off >/dev/null 2>&1

# god 세션 우선권: 재선출이 선착순이면 '이전 god 세션'(거노 대면 + 리더 타이틀)이
# 늦게 떠도 다른 pane 에 god 을 뺏긴다(거노 발견). 내가 이전 god 세션이면 즉시
# claim. 아니면 양보 — 옛 '2초 sleep 양보'는 재시작 직후 god 의 첫 턴이 수십 초
# 늦는 시나리오에서 항상 만료돼 인계 사고를 냈다(실측: 부활 워커가 자리+아로나
# 치장 탈취). 새 규칙: ① god 세션이 살아있는 pane 에 bind 돼 있으면 무기한 양보
# (걔가 곧 claim 한다) ② 안 보이면 부팅 중일 수 있어 60초 유예(sleep 없이 유예
# 마커 + 매 턴 폴링) ③ 만료에만 정식 승계(TAKEOVER — 치장·roster 마킹 허용).
TAKEOVER=0
if [ -n "$god_sid" ] && [ "$god_sid" != "$my_sid" ]; then
  for bm in /tmp/kasaterm-bound-*; do
    [ -e "$bm" ] || continue
    bsid=$(sed 's#.*/##; s#\.jsonl$##' "$bm" 2>/dev/null)
    [ "$bsid" = "$god_sid" ] || continue
    bp="${bm##*kasaterm-bound-}"
    bpane="%${bp#_}"
    if printf '%s\n' "$surfaces" | grep -qx "$bpane"; then
      # 생존 확인 턴마다 유예 시계 리셋 — 일시 스캔 실패로 생긴 yield-since
      # 가 잔존하면 다음 실패 한 번에 즉시 만료(시한폭탄)되는 결함 방어.
      rm -f "$BASE/god-yield-since" 2>/dev/null
      ensure_worker_look; exit 0   # god 세션 생존 — 무기한 양보
    fi
  done
  since="$BASE/god-yield-since"
  now=$(date +%s)
  [ -f "$since" ] || printf '%s' "$now" > "$since"
  start=$(cat "$since" 2>/dev/null); : "${start:=$now}"
  if [ $((now - start)) -lt 60 ]; then
    ensure_worker_look; exit 0     # 부팅 유예 — 아직 인계하지 않는다
  fi
  rm -f "$since"
  TAKEOVER=1                        # 유예 만료 — god 세션 사망 판정, 정식 승계
fi

if $KASACOLLAB lead claim >/dev/null 2>&1; then
  ensure_god_look
  # claude 세션 타이틀 god 주입은 roster 의 god 세션이 없거나 그게 곧 나일 때만 —
  # 선착순으로 임시 claim 한 경우(god_sid 가 딴 세션) 타이틀 god 중복을 피해 보류
  # (강등 시 자동 원복은 claude slash 한계로 불가하므로 애초에 안 친다). 타이틀
  # resume(`claude --resume god`)·색은 진짜 god 세션 1회 주입에만.
  if [ -z "$god_sid" ] || [ "$god_sid" = "$my_sid" ] || [ "$TAKEOVER" = "1" ]; then
    $CLI tell "$ME" "/rename $GOD_NAME" >/dev/null 2>&1
    sleep 1
    $CLI tell "$ME" "/color $GOD_CLAUDE_COLOR" >/dev/null 2>&1
    sleep 1
    # greeting 1회 마커 — sid 8자 기준: 같은 세션 재선출이면 생략, 새 세션만 인사.
    # lead 만료→재claim 마다 입력창에 인사말이 끼어들던 사고(거노 실측 2회) 방지.
    _gsid8="${my_sid:0:8}"
    _greet_marker="$BASE/god-greeted-${_gsid8:-default}"
    if [ ! -f "$_greet_marker" ]; then
      mkdir -p "$BASE" 2>/dev/null
      touch "$_greet_marker" 2>/dev/null
      $CLI tell "$ME" "$GOD_GREETING" >/dev/null 2>&1
    fi
    # 정당한 승계(첫 god/진짜 god 복귀/유예 만료)만 roster 마킹 — 양보 경로는
    # claim 에 도달하지 않으므로 여기 도달=항상 정당하지만 의도를 명시한다.
    [ -n "$my_sid" ] && $KASACOLLAB roster-mark-god "$my_sid" >/dev/null 2>&1
  fi
else
  ensure_worker_look
fi
exit 0
