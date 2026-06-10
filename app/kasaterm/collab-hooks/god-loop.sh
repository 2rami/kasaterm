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

HOOKS_DIR="$(cd "$(dirname "$0")" && pwd)"

# idle nudge — god 방에서 msg 가 tell 을 생략하므로(입력창 오염 방지, kasacollab
# cmd_msg 참조) idle 인데 미읽 inbox 를 가진 워커는 아무도 안 깨우면 영원히 모른다.
# 주기마다 board(status==idle, god 제외)와 messages.jsonl(read=false)을 대조해
# 해당 워커만 조용히 1회 tell. working 워커는 절대 건드리지 않는다 — 다음 턴
# board-context/stop-drain 이 어차피 싣는다. 같은 미읽 id 세트에는 재nudge 하지
# 않음(마커 $BASE/god-nudged-<pane> 에 세트 저장, 새 메시지로 세트가 바뀌면 재무장).
(
  while sleep "$INTERVAL"; do
    python3 - "$BASE" "$GOD" <<'PY' 2>/dev/null |
import json, os, subprocess, sys
base, god = sys.argv[1], sys.argv[2]
cli = os.environ.get("KASATERM_CLI", "kasaterm-cli")
try:
    r = subprocess.run([cli, "board"], capture_output=True, text=True, timeout=3)
    board = (json.loads(r.stdout).get("result") or {}).get("board") or []
except Exception:
    sys.exit(0)
idle = {b.get("surface_id") for b in board
        if b.get("status") == "idle" and b.get("surface_id")
        and b.get("surface_id") != god}
if not idle:
    sys.exit(0)
unread = {}
try:
    with open(os.path.join(base, "messages.jsonl")) as f:
        for line in f:
            try:
                m = json.loads(line)
            except Exception:
                continue
            if not m.get("read") and m.get("to") in idle:
                unread.setdefault(m["to"], []).append(str(m.get("id")))
except Exception:
    sys.exit(0)
for pane, ids in unread.items():
    print(pane + "\t" + str(len(ids)) + "\t" + ",".join(sorted(ids)))
PY
    while IFS=$'\t' read -r pane n ids; do
      [ -z "$pane" ] && continue
      marker="$BASE/god-nudged-$pane"
      [ -f "$marker" ] && [ "$(cat "$marker")" = "$ids" ] && continue
      "$CLI" tell "$pane" "[inbox] 미읽 ${n}건 — kasacollab inbox 확인" \
        >/dev/null 2>&1 && echo "$ids" > "$marker"
    done
  done
) &
NUDGE_PID=$!
trap 'rm -rf "$LOCK"; kill "$NUDGE_PID" 2>/dev/null' EXIT

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
