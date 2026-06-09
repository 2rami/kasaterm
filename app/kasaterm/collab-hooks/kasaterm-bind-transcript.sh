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
  # roster upsert — 재시작 후 god 이 워커들을 `claude --resume` 로 부활시킬 수
  # 있게 {pane_id, session_id(transcript uuid), cwd, ts} 를 영속 기록한다.
  # **/tmp 아님** (~/.config): 재시작 청소가 /tmp/kasaterm-collab 를 비워도
  # roster 는 살아남아야 복구가 된다. 같은 pane_id 는 갱신(claude --resume 로
  # session 이 바뀌면 최신으로 수렴). 30일 지난 entry 는 prune.
  python3 - "$KASATERM_PANE_ID" "$tp" "$PWD" <<'PY' 2>/dev/null || true
import sys, os, json, time
pane, tp, cwd = sys.argv[1], sys.argv[2], sys.argv[3]
sid = os.path.splitext(os.path.basename(tp))[0]  # transcript 파일명 = session uuid
slug = cwd.replace('/', '-').replace('.', '-')
d = os.path.expanduser('~/.config/kasaterm/agent-roster')
os.makedirs(d, exist_ok=True)
p = os.path.join(d, slug + '.json')
try:
    roster = json.load(open(p))
    if not isinstance(roster, dict):
        roster = {}
except Exception:
    roster = {}
roster[pane] = {"pane_id": pane, "session_id": sid, "cwd": cwd, "ts": time.time()}
cutoff = time.time() - 30 * 86400
roster = {k: v for k, v in roster.items() if v.get("ts", 0) >= cutoff}
tmp = p + ".tmp"
json.dump(roster, open(tmp, "w"), ensure_ascii=False)
os.replace(tmp, p)
PY
fi
exit 0
