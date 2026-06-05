#!/bin/bash
# SessionStart / PreToolUse hook: register THIS pane's claude transcript with
# kasaterm so the host can tail it and auto-fill the collab board (no manual
# `announce` needed). No-op outside a kasaterm pane.
#
# stdin (hook payload) carries `transcript_path`; $KASATERM_PANE_ID is injected
# by pty-backend when the pane spawns. A per-pane marker file dedups so we only
# hit the socket when the transcript path actually changes (e.g. claude --resume
# swaps it) instead of on every tool call.
[ -z "$KASATERM_PANE_ID" ] && exit 0
input=$(cat)
tp=$(printf '%s' "$input" | python3 -c "import sys,json;print(json.load(sys.stdin).get('transcript_path',''))" 2>/dev/null)
[ -z "$tp" ] && exit 0
marker="/tmp/kasaterm-bound-${KASATERM_PANE_ID//[^A-Za-z0-9]/_}"
# bind는 데몬 메모리에만 산다(재시작하면 소실). marker는 /tmp에 영속이라, sock
# inode를 dedup 키에 섞지 않으면 데몬 재시작 후에도 "이미 bind함"으로 오판해 영영
# 재-bind를 건너뛴다(=그 pane이 board에서 사라짐). sock이 새로 생기면 inode가
# 바뀌므로 자동으로 재-bind 된다.
sock="${KASATERM_SOCKET_PATH:-$HOME/.config/kasaterm/daemon.sock}"
sig="$(stat -f %i "$sock" 2>/dev/null || stat -c %i "$sock" 2>/dev/null):$tp"
[ "$(cat "$marker" 2>/dev/null)" = "$sig" ] && exit 0
if kasaterm-cli bind-transcript "$tp" >/dev/null 2>&1; then
  printf '%s' "$sig" > "$marker"
fi
exit 0
