#!/usr/bin/env bash
# split 성능 측정 — PROFILE 데몬으로 kasaterm 재시작.
# 주의: 현재 모든 pane PTY(claude 세션 포함)가 끊기고 재시작된다.
#       kasaterm이 claude --resume 으로 복원을 시도한다.
set -u

APP="$HOME/Applications/kasaterm.app/Contents/MacOS/kasaterm"

echo "▶ 현재 kasaterm 종료 (모든 pane 끊김)…"
pkill -f "Applications/kasaterm.app" 2>/dev/null || true
pkill -f "kasaterm --daemon" 2>/dev/null || true
sleep 2

# 데몬 로그 비우기 — 이번 측정분만 남게
: > /tmp/kasaterm-daemon.log

echo "▶ KASATERM_PROFILE=1 로 재시작…"
KASATERM_PROFILE=1 "$APP" > /tmp/kasaterm-gui.log 2>&1 &
echo "   GUI 로그: /tmp/kasaterm-gui.log"
echo "   데몬 로그: /tmp/kasaterm-daemon.log"
echo ""
echo "다음 순서로 측정:"
echo "  1) pane 복원될 때까지 잠깐 기다린다"
echo "  2) Cmd+D (또는 split 단축키)로 split 을 3~4번 한다"
echo "  3) 아래 분석 실행:  bash scripts/analyze-split.sh"
