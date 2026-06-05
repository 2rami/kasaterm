#!/usr/bin/env bash
# measure-split.sh 로 재시작 + split 한 뒤 실행. split 경로 타이밍을 짝지어 보여준다.
set -u

GUI=/tmp/kasaterm-gui.log
DAE=/tmp/kasaterm-daemon.log

echo "=== GUI: split rpc 전송 → 화면 반영 ==="
grep -E '\[pf-split\]' "$GUI" 2>/dev/null || echo "(없음 — split 안 했거나 PROFILE 미적용)"

echo ""
echo "=== 데몬: split 내부 단계 (enter→active_cwd→spawn_pane→broadcast) ==="
grep -E '\[pf-dsplit\]' "$DAE" 2>/dev/null || echo "(없음)"

echo ""
echo "=== 단계별 소요(ms) 자동 계산 ==="
grep -E '\[pf-dsplit\]' "$DAE" 2>/dev/null | awk '
  { stage=$2; t=$NF; if (prev_t!="") printf "  %-16s → %-16s : %d ms\n", prev_s, stage, t-prev_t; prev_s=stage; prev_t=t }
'
echo ""
echo "rpc sent → state applied (GUI 왕복 총합):"
grep -E '\[pf-split\]' "$GUI" 2>/dev/null | awk '
  /rpc sent/ { sent=$NF }
  /state applied/ { if (sent!="") { printf "  %d ms\n", $NF-sent; sent="" } }
'
