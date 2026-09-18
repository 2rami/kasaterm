#!/bin/bash
# 격리 앱에서 훅 → 판정 → 보드/알림 전이를 실측한다. 사용자 앱과는 소켓·세션·설정·창 파일을
# 전부 따로 쓴다(CLAUDE.md 「검증용 앱을 띄우고 거두는 법」). 진짜 claude 대신 이름만 claude 인
# 잠자는 바이너리를 pane 에 띄워 하네스 판정만 빌린다 — 상태는 전부 `kasaterm-cli turn/attention/
# notify` 로 넣는다(훅이 하는 것과 같은 호출). 판정은 `board --local --json` 과 `[state]` 로그로 본다.
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
D="${TMPDIR:-/tmp}/kasaterm-agent-state-rig"
rm -rf "$D"; mkdir -p "$D/probe" "$D/students"
export KASATERM_SOCKET_PATH="$D/rig.sock"
CLI="$ROOT/target/debug/kasaterm-cli"
APPBIN="$ROOT/target/debug/kasaterm"
[ -x "$CLI" ] && [ -x "$APPBIN" ] || { echo "먼저 cargo build -p kasaterm -p kasa-socket"; exit 2; }
printf 'fn main(){std::thread::sleep(std::time::Duration::from_secs(600));}' > "$D/probe/c.rs"
rustc -o "$D/probe/claude" "$D/probe/c.rs" >/dev/null 2>&1 || { echo "rustc 실패"; exit 2; }

KASATERM_SESSION_FILE="$D/session.json" \
KASATERM_SETTINGS_FILE="$D/settings.json" \
KASATERM_WINDOW_FILE="$D/window.json" \
KASATERM_AUTORESTORE=fresh \
KASATERM_STUDENTS_DIR="$D/students" \
KASATERM_AUTOQUIT_MS=120000 \
KASATERM_STATE_LOG=1 \
"$APPBIN" > "$D/app.log" 2>&1 &
# 앱은 stderr 가 tty 가 아니면 `$TMPDIR/kasaterm-app.log` 로 돌린다 — [state] 줄은 거기 쌓인다.
STDERR_LOG="${TMPDIR:-/tmp}/kasaterm-app.log"; LOG_FROM=$(wc -l < "$STDERR_LOG" 2>/dev/null || echo 0)
APP=$!
trap 'kill $APP 2>/dev/null; wait $APP 2>/dev/null' EXIT
for _ in $(seq 1 80); do [ -S "$KASATERM_SOCKET_PATH" ] && break; sleep 0.25; done
[ -S "$KASATERM_SOCKET_PATH" ] || { echo "소켓이 안 떴다"; tail -20 "$D/app.log"; exit 1; }
sleep 2
PANE="$("$CLI" list surfaces 2>/dev/null | grep -o '%[0-9]*' | head -n 1)"
[ -n "$PANE" ] || { echo "pane 을 못 찾았다"; "$CLI" list surfaces; exit 1; }
export KASATERM_PANE_ID="$PANE"
echo "pane=$PANE"
"$CLI" send --surface "$PANE" "$D/probe/claude"$'\n' >/dev/null
sleep 3

row() {
  # `board --local` 은 board_service 가 주기적으로 긁는 스냅샷이라 몇 초 늦을 수 있다 —
  # 그래서 기대값이 뜰 때까지 폴링한다(아래 expect).
  "$CLI" board --local --json 2>/dev/null | python3 -c '
import sys, json
want = sys.argv[1]
def walk(v):
    if isinstance(v, dict):
        a = v.get("address")
        if v.get("surface_id") == want or (isinstance(a, dict) and a.get("surface_id") == want):
            return v
        for x in v.values():
            r = walk(x)
            if r: return r
    elif isinstance(v, list):
        for x in v:
            r = walk(x)
            if r: return r
try:
    r = walk(json.load(sys.stdin))
except Exception as e:
    print("parse-error", e); sys.exit(0)
if not r:
    print("no-row"); sys.exit(0)
print(r.get("status"), "|", r.get("status_reason"), "|", r.get("attention_kind"), "|", r.get("waiting_for"))
' "$PANE"
}
FAIL=0
# expect <라벨> <기대 status> <기대 reason 조각(빈칸이면 무시)> <명령...>
expect() {
  local label="$1" want="$2" why="$3"; shift 3
  "$@" >/dev/null 2>&1 || echo "  (CLI 실패: $*)"
  # 반영까지 걸린 시간(ms) — 관측이 신호로 깨는지가 곧 이 숫자다(2026-09-18 「보드가 느리네」).
  local t0=$(python3 -c 'import time; print(int(time.time()*1000))') got="" ok=0
  local seen=""
  for _ in $(seq 1 80); do
    got="$(row)"
    case "$got" in
      "$want | "*"$why"*) ok=1; break ;;
    esac
    # 기대값이 오기 전에 보드가 보여 준 중간값 — 느린 단계의 원인이 여기 찍힌다.
    case "$seen" in *"$got"*) ;; *) seen="$seen
      ⋯ $(python3 -c 'import time; print(int(time.time()*1000)%100000)') $got" ;; esac
    sleep 0.1
  done
  [ -n "$seen" ] && [ "$ok" = 1 ] && printf '%s\n' "$seen"
  local dt=$(( $(python3 -c 'import time; print(int(time.time()*1000))') - t0 ))
  if [ "$ok" = 1 ]; then
    printf 'OK   %-28s %5sms → %s\n' "$label" "$dt" "$got"
  else
    FAIL=$((FAIL + 1))
    printf 'FAIL %-28s      → %s (기대 %s / %s)\n' "$label" "$got" "$want" "$why"
  fi
}
expect "fake claude(엔터 브리지)" working "enter bridge" true
# 브리지(4초)가 끝나길 기다린다 — 안 그러면 뒤 단계의 훅 신호와 브리지가 섞여 반영 시간이
# 브리지 만료 시각에 끌려간다(2026-09-18 실측: turn end 1.1초·reset 0.6초가 그것이었다).
sleep 1.5
expect "turn start (bypass)" working "hook turn open" "$CLI" turn start --permission-mode bypassPermissions
expect "attention permission" waiting "hook attention" "$CLI" attention --kind permission "Bash 승인"
expect "turn end" idle "turn closed" "$CLI" turn end
expect "turn start" working "hook turn open" "$CLI" turn start
expect "compact_start" working "precompact" "$CLI" turn compact_start
expect "compact_end" working "hook turn open" "$CLI" turn compact_end
expect "notify (Stop drain)" idle "turn closed" "$CLI" notify "✓ 완료" "작업을 마쳤어"
expect "turn start" working "hook turn open" "$CLI" turn start
expect "attention idle(턴 열림→무시)" working "hook turn open" "$CLI" attention --kind idle "60초 방치"
expect "turn reset" unknown "" "$CLI" turn reset
echo "실패 $FAIL 건"
echo "--- [state] 전이 로그(이 실행분) ---"
tail -n +"$((LOG_FROM + 1))" "$STDERR_LOG" 2>/dev/null | grep -E '^\[state\]' || echo "(전이 로그 없음)"
