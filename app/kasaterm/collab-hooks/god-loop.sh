#!/bin/bash
# god 전용 워처. god-elect 가 god 선출 시 백그라운드(nohup)로 강제 기동한다 —
# claude Monitor 에 기대지 않고 외부 프로세스로 '감시 반드시 켜짐'을 보장하는 게
# 핵심. god pane 이 죽어 재선출되면 god-elect 의 pkill→nohup 으로 새 god 워처가
# 옛 것을 대체해 항상 정확히 1개만 돈다.
#
# 역할은 하나: 워커 막힘(승인/입력 대기) → god 에게 1회 msg 알림. 상태 히스토리
# 누적(옛 fleet.log)은 폐기 — 현재 상태는 board-context 가 매 턴 주입하고, 이벤트
# (done 보고·막힘·stop-drain)는 msg 로 push 되므로 읽는 쪽 없는 로그였다
# (2026-06-10 거노 결정).
GOD="${1:-${KASATERM_PANE_ID:-}}"
CLI="${KASATERM_CLI:-kasaterm-cli}"
[ -z "$GOD" ] && exit 0
INTERVAL="${KASATERM_GOD_LOOP_INTERVAL:-4}"
slug=$(pwd | sed 's#[/.]#-#g')
BASE="/tmp/kasaterm-collab/$slug"
mkdir -p "$BASE"

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

HOOKS_DIR="$(cd "$(dirname "$0")" && pwd)"

# board-watch = pane 상태 변화 polling stream(1 line/change). 워커가 승인/입력
# 대기로 막히면 god 에게 1회 알림(munder: 워커 프롬프트는 사람이 아니라 god 이
# 처리). GUI 는 워커 프롬프트에 토스트를 안 띄우므로 이 알림이 없으면 막힌
# 워커를 아무도 모른다. 같은 pane 의 대기가 풀리면 마커를 지워 재무장.
$CLI board-watch "$INTERVAL" 2>/dev/null | while IFS= read -r line; do
  [ -z "$line" ] && continue
  pane="${line%% *}"
  case "$pane" in %*) ;; *) continue ;; esac
  case "$line" in
    *"  waiting"*|*"  blocked"*)
      if [ "$pane" != "$GOD" ] && [ ! -f "$BASE/god-notified-$pane" ]; then
        touch "$BASE/god-notified-$pane"
        python3 "$HOOKS_DIR/kasacollab.py" msg "$GOD" \
          "$pane 승인/입력 대기로 막힘 — peek $pane 로 프롬프트 확인하고 처리해(직접 키 주입 또는 사용자 에스컬레이션)" \
          >/dev/null 2>&1 || true
      fi
      ;;
    *) rm -f "$BASE/god-notified-$pane" 2>/dev/null ;;
  esac
done
