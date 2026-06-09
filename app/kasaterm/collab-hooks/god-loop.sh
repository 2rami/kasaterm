#!/bin/bash
# god 전용 모니터링 워처. god-elect 가 god 선출 시 백그라운드(nohup)로 강제
# 기동한다 — claude Monitor 에 기대지 않고 외부 프로세스로 '모니터링 반드시 켜짐'을
# 보장하는 게 핵심. god pane 이 죽어 재선출되면 god-elect 의 pkill→nohup 으로
# 새 god 워처가 옛 것을 대체해 항상 정확히 1개만 돈다.
#
# P0(골격): board-watch 변화 stream 을 fleet.log 에 누적(god 이 자기 턴에 읽음).
# P2(확장): git status --short 교차검증 + list surfaces 로 pane/claude 수 집계 +
#           Edit/Write 변경파일 종합 → "누가 뭘 바꿨고 미커밋인지"를 god 에 push.
GOD="${1:-${KASATERM_PANE_ID:-}}"
CLI="${KASATERM_CLI:-kasaterm-cli}"
[ -z "$GOD" ] && exit 0
INTERVAL="${KASATERM_GOD_LOOP_INTERVAL:-4}"
slug=$(pwd | sed 's#[/.]#-#g')
BASE="/tmp/kasaterm-collab/$slug"
mkdir -p "$BASE"
FLEET="$BASE/fleet.log"

# 단일 워처 보장 — mkdir 원자성으로 동시 중복 기동을 막는다(start_god_loop 의
# pkill 과 이중 안전). 락의 pid 가 죽었으면(stale) 탈취한다.
LOCK="$BASE/god-loop.lock.d"
if ! mkdir "$LOCK" 2>/dev/null; then
  oldpid=$(cat "$LOCK/pid" 2>/dev/null)
  if [ -n "$oldpid" ] && kill -0 "$oldpid" 2>/dev/null; then
    exit 0
  fi
  rm -rf "$LOCK"; mkdir "$LOCK" 2>/dev/null || exit 0
fi
echo $$ > "$LOCK/pid"
trap 'rm -rf "$LOCK"' EXIT

echo "[god-loop] started god=$GOD pid=$$" >> "$FLEET"

# board-watch = pane 상태 변화 polling stream(1 line/change). 받아서 누적만 —
# god 입력창을 직접 건드리지 않아(god 타이핑 방해 없음) board-context 가 god 턴에
# fleet.log 를 당겨 보여주는 방식으로 P2 에서 잇는다.
$CLI board-watch "$INTERVAL" 2>/dev/null | while IFS= read -r line; do
  [ -z "$line" ] && continue
  printf '%s %s\n' "$(date '+%H:%M:%S')" "$line" >> "$FLEET"
done
