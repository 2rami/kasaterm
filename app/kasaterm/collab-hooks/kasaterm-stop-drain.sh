#!/bin/bash
# Stop hook: claude in THIS pane is trying to end its turn. Two jobs, in order:
#   1. inbox drain (munder drainForStop 이식) — 내게 온 안 읽은 협업 메시지가
#      있으면 stdout 으로 {"decision":"block","reason":...} JSON 을 내서 claude 가
#      멈추지 못하게 막고, 그 메시지들을 처리하게 강제한다. read=True 마킹이
#      멱등 키라 같은 메시지로 두 번 막지 않는다(kasacollab.py drain-stop).
#   2. 막을 게 없으면 — 기존 notify-complete 동작: 작업 완료 데스크탑 알림.
#      (이 스크립트가 옛 kasaterm-notify-complete.sh 를 흡수했다.)
#
# 무한루프 방지 2겹: ① read 마킹(같은 메시지 재surface 안 됨) ② stop_hook_active —
# claude 가 우리 block 으로 이미 한 번 더 돈 뒤 다시 멈추려는 재진입이면 그냥
# 통과시킨다(메시지 처리할 시간을 줬으니 더는 안 막는다).
#
# $KASATERM_PANE_ID 는 pty-backend 가 pane 스폰 때 주입. Stop payload 는 stdin.
[ -z "$KASATERM_PANE_ID" ] && exit 0

input=$(cat 2>/dev/null)
active=$(printf '%s' "$input" | python3 -c "import sys,json
try: print('1' if json.load(sys.stdin).get('stop_hook_active') else '')
except Exception: pass" 2>/dev/null)

HOOKS_DIR="$(cd "$(dirname "$0")" && pwd)"
complete() {
  dir="${PWD##*/}"
  kasaterm-cli notify "✓ ${dir} — claude 완료" "작업을 마쳤어" >/dev/null 2>&1 || true
}

# 재진입(이미 우리 block 으로 한 번 더 돈 상태) → 더 안 막고 완료 처리.
if [ -n "$active" ]; then
  complete
  exit 0
fi

# inbox drain: 미읽 있으면 drain-stop 이 block JSON 을 stdout 으로 내고 exit 10.
out=$(python3 "$HOOKS_DIR/kasacollab.py" drain-stop 2>/dev/null)
rc=$?
if [ "$rc" -eq 10 ]; then
  printf '%s\n' "$out"   # block JSON 을 claude 에 그대로 통과 → 멈춤 차단
  exit 0                 # stdout JSON 이 결정함 — 정상 종료(exit 2 아님)
fi

# 막을 메시지 없음 → 진짜 완료.
complete
exit 0
