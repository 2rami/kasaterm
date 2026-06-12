#!/bin/bash
# 수정 확인용 빠른 재실행 — arona-ui(프론트) 빌드+동기화 후 kasaterm 재시작 +
# 아로나 패널 자동오픈(KASATERM_AUTOARONA_MS). Rust 변경은 `--app`(전체 빌드+설치).
# graceful quit 실패해도 force-quit 으로 확실히 재시작(UI 확인용이라 세션 보존 불요).
#
#   scripts/show.sh         # 프론트만(arona-ui) 빌드+동기화+재시작
#   scripts/show.sh --app   # Rust 포함 전체(build-app)
set -e
cd "$(dirname "$0")/.."
INSTALLED="$HOME/Applications/kasaterm.app"

if [ "$1" = "--app" ]; then
  echo "[show] 전체 빌드(build-app)…"
  bash scripts/build-app.sh >/tmp/show-build.log 2>&1
  rm -rf "$INSTALLED" && cp -R dist/kasaterm.app "$INSTALLED"
else
  echo "[show] arona-ui 빌드…"
  ( cd web/arona-ui && npm run build ) >/tmp/show-build.log 2>&1
  APPRES="$INSTALLED/Contents/Resources/arona-ui"
  rm -rf "$APPRES" && cp -R web/arona-ui/dist "$APPRES"
fi
echo "[show] 빌드+동기화 완료"

# 종료(graceful → force)
osascript -e 'quit app "kasaterm"' >/dev/null 2>&1 || true
sleep 2
pkill -f "kasaterm.app/Contents/MacOS/kasaterm" 2>/dev/null || true
sleep 1

# 재시작 + 패널 자동오픈
KASATERM_AUTOARONA_MS=2500 nohup "$INSTALLED/Contents/MacOS/kasaterm" >/tmp/kasaterm-run.log 2>&1 &
echo "[show] kasaterm 재시작(아로나 패널 자동오픈) pid $!"
