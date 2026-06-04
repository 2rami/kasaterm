#!/usr/bin/env bash
# 격리 kasaterm 데몬으로 `claude agents --json` 상태 소스화를 자동 검증한다.
#
# 메인 GUI(동료 pane들이 떠 있는 창)는 절대 건드리지 않는다. 전용 --socket으로
# 별개 데몬을 띄우고, 가짜 `claude`(agents --json → 원하는 상태)를 PATH로 먹인 뒤
# collab.board RPC로 상태 전파를 assert하고, 끝나면 데몬·임시파일을 정리한다.
# 핵심: "메인을 재시작"하는 게 아니라 "격리 인스턴스를 따로 띄워" 검증한다 —
# 그래서 같은 레포를 만지는 다른 pane들을 죽이지 않는다.
#
# 사용:
#   scripts/verify-agents-status.sh              # release 빌드 후 백엔드 검증
#   scripts/verify-agents-status.sh --no-build   # 기존 target/release 바이너리로
#   scripts/verify-agents-status.sh --gui        # board panel waiting 렌더까지(headless chrome)
#   (플래그는 조합 가능: --no-build --gui)
#
# 종료코드 0 = 통과, 1 = 실패(빌드/스폰 오류 또는 assert 실패).
set -euo pipefail

NO_BUILD=0; GUI=0
for a in "$@"; do
  case "$a" in
    --no-build) NO_BUILD=1 ;;
    --gui)      GUI=1 ;;
    *) echo "✗ 모르는 인자: $a"; exit 1 ;;
  esac
done

REPO="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$REPO/target/release/kasaterm"
CLI="$REPO/target/release/kasaterm-cli"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/kasaterm-isoverify.XXXXXX")"
SOCK="$WORK/sock"
PROJ="$WORK/proj"
SHIM="$WORK/bin"
SESSION="ISOVERIFY$$"                   # transcript 파일 stem = mock claude의 sessionId
DAEMON_PID=""

cleanup() {
  if [ -n "$DAEMON_PID" ]; then
    kill -9 "$DAEMON_PID" 2>/dev/null || true
    wait "$DAEMON_PID" 2>/dev/null || true   # 잡 종료 메시지(Killed) 삼키기
  fi
  rm -rf "$WORK"
}
trap cleanup EXIT

# ── 1. 빌드 ──────────────────────────────────────────────────────────────
if [ "$NO_BUILD" = 0 ]; then
  echo "▶ build (release)…"
  cargo build --release -p kasaterm --manifest-path "$REPO/Cargo.toml" -q
fi
[ -x "$BIN" ] || { echo "✗ no binary: $BIN (먼저 빌드하거나 --no-build 빼고 실행)"; exit 1; }
[ -x "$CLI" ] || { echo "✗ no cli: $CLI"; exit 1; }

# ── 2. mock claude shim ─────────────────────────────────────────────────
#    `claude agents --json` 호출 시 $WORK/status 파일이 정하는 상태를 뱉는다.
#    데몬 재기동 없이 파일만 바꿔 idle↔busy↔waiting 전이를 만든다.
mkdir -p "$SHIM" "$PROJ"
cat > "$SHIM/claude" <<EOF
#!/bin/bash
[ "\$1" = "agents" ] || exit 0
ST=\$(cat "$WORK/status" 2>/dev/null || echo idle)
if [ "\$ST" = waiting ]; then
  printf '[{"sessionId":"$SESSION","status":"waiting","waitingFor":"permission"}]\n'
else
  printf '[{"sessionId":"$SESSION","status":"%s"}]\n' "\$ST"
fi
EOF
chmod +x "$SHIM/claude"
printf '{"type":"summary","summary":"iso"}\n' > "$PROJ/$SESSION.jsonl"

# ── 3. 격리 데몬 스폰 (전용 소켓 + mock PATH) ───────────────────────────
echo "▶ spawn isolated daemon (socket=$SOCK)…"
PATH="$SHIM:$PATH" nohup "$BIN" --daemon --socket "$SOCK" > "$WORK/daemon.log" 2>&1 &
DAEMON_PID=$!
for _ in $(seq 1 40); do [ -S "$SOCK" ] && break; sleep 0.25; done
[ -S "$SOCK" ] || { echo "✗ daemon socket no-show"; cat "$WORK/daemon.log"; exit 1; }

# 기본 pane id를 받아 가짜 transcript를 바인드 → watcher가 stem(sessionId)으로 매칭.
PANE="$(KASATERM_SOCKET_PATH="$SOCK" "$CLI" list surfaces \
  | python3 -c 'import sys,json;print(json.load(sys.stdin)["result"]["surfaces"][0]["id"])')"
echo "  pane=$PANE  session=$SESSION"
KASATERM_PANE_ID="$PANE" KASATERM_SOCKET_PATH="$SOCK" \
  "$CLI" bind-transcript "$PROJ/$SESSION.jsonl" >/dev/null

# ── 4. override 매트릭스 assert ─────────────────────────────────────────
#    watcher는 매 750ms build_activity로 working을 쓰고, agents 폴은 그 위를
#    덮는다. 폴 주기(1.5s)+여유로 3초 기다린 뒤 board를 읽는다.
read_status() {
  KASATERM_SOCKET_PATH="$SOCK" "$CLI" board 2>/dev/null | python3 -c \
    'import sys,json;b=json.load(sys.stdin)["result"]["board"];print((b[0]["status"]+"|"+str(b[0].get("waiting_for"))) if b else "EMPTY")'
}
FAIL=0
expect() { # $1=mock상태  $2=기대 "status|waiting_for"
  echo "$1" > "$WORK/status"; sleep 3
  got="$(read_status)"
  if [ "$got" = "$2" ]; then
    echo "  ✓ mock=$1 → $got"
  else
    echo "  ✗ mock=$1 → got[$got] want[$2]"; FAIL=1
  fi
}

echo "▶ assert override matrix (waiting 우선 / idle 공식우선 / busy 정상화)…"
expect waiting "waiting|permission"   # 맹점: transcript 못 보는 권한대기
expect idle    "idle|None"            # 공식 우선: transcript working 오판 교정
expect busy    "working|None"         # busy 정상화 + 사유 클리어
expect waiting "waiting|permission"   # 양방향 전이 복귀

# ── 5. (옵션) GUI: board panel waiting 렌더 검증 ────────────────────────
#    main.rs의 BOARD_PANEL_HTML을 그대로 추출(드리프트 0)해 mock board를 박고
#    headless chrome으로 렌더 → DOM에 '⚠ 권한 대기중' + .status.waiting(주황)이
#    찍히는지 assert + 스샷 저장. GUI 창을 띄우지 않아 메인 화면 방해 0.
if [ "$GUI" = 1 ]; then
  echo "▶ GUI: board panel waiting 렌더 검증 (headless chrome)…"
  CHROME="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
  if [ ! -x "$CHROME" ]; then
    echo "  ⚠ chrome 없음 — GUI 검증 skip"
  else
    MOCK="$WORK/board-mock.html"
    SHOT_OUT="${TMPDIR:-/tmp}/kasaterm-board-panel-verify.png"
    sed -n '/const BOARD_PANEL_HTML/,/^"#;/p' "$REPO/app/kasaterm/src/main.rs" \
      | sed '1s/.*r#"//' | sed '$s/"#;//' \
      | sed 's/__PORT__/0/' \
      | grep -v 'setInterval(poll' \
      | sed 's#^poll();$#render([{surface_id:"%3",status:"waiting",waiting_for:"permission",intent:"demo",files:["auth.ts"]},{surface_id:"%4",status:"working",intent:"demo"},{surface_id:"%5",status:"idle",intent:"demo"}]);#' \
      > "$MOCK"
    "$CHROME" --headless=new --disable-gpu --hide-scrollbars --force-device-scale-factor=2 \
      --window-size=460,360 --screenshot="$SHOT_OUT" "file://$MOCK" >/dev/null 2>&1 || true
    DOM="$("$CHROME" --headless=new --dump-dom "file://$MOCK" 2>/dev/null || true)"
    # here-string(파이프 아님) — grep -q early-exit이 pipefail을 건드리지 않게.
    if grep -q '권한 대기중' <<<"$DOM"; then
      echo "  ✓ waiting → '⚠ 권한 대기중' 렌더"
    else
      echo "  ✗ waiting 라벨 누락"; FAIL=1
    fi
    if grep -q 'status waiting' <<<"$DOM"; then
      echo "  ✓ .status.waiting(주황 #f0883e) 클래스 적용"
    else
      echo "  ✗ waiting 클래스 누락"; FAIL=1
    fi
    [ -f "$SHOT_OUT" ] && echo "  ▸ 스샷: $SHOT_OUT (Read로 눈으로 확인)"
  fi
fi

if [ "$FAIL" = 0 ]; then
  echo "✅ PASS — agents --json 상태 소스화 격리검증 통과 (메인 GUI 무손상)"
else
  echo "❌ FAIL — 위 ✗ 항목 확인"; exit 1
fi
