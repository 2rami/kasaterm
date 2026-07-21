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
# bg job(claude Task/background)은 부모 pane 의 KASATERM_PANE_ID 를 그대로 물려받는다
# — 여기서 bind 하면 부모 pane 의 transcript 를 bg job 의 jsonl 로 덮어써(=부모 오염)
# 미도리 같은 멀쩡한 pane 까지 board 에서 깨진다. bg job 은 $CLAUDE_JOB_DIR 로 식별되니
# 건너뛴다(자기 surface 는 host 쪽에서 따로 bind 한다).
[ -n "$CLAUDE_JOB_DIR" ] && exit 0
input=$(cat)
tp=$(printf '%s' "$input" | python3 -c "import sys,json;print(json.load(sys.stdin).get('transcript_path',''))" 2>/dev/null)
[ -z "$tp" ] && exit 0
# title-sync 가 띄운 headless claude(및 그 내부 제목생성 서브세션)는 부모 pane 의
# KASATERM_PANE_ID 를 물려받는다 — 여기서 bind 하면 pane_claude_sid 가 이 title-gen
# 세션으로 덮여 입력박스 인레이에 "아래 대화의 주제를…" 메타프롬프트가 샌다. 전용
# junk cwd(kasaterm-title-gen)로 식별해 건너뛴다(title-sync env 정리의 이중 안전망).
case "$tp$PWD" in *kasaterm-title-gen*) exit 0 ;; esac
# detach 포크 페르소나 복원(거노: 백그라운드 가면 페르소나 풀림): 데몬이 포크 argv 를
# 재구성하며 --append-system-prompt 가 유실된다. env KASATERM_PERSONA 는 데몬 env(데몬을
# 낳은 옛 pane 고정)라 계보가 틀려 못 쓴다 — 물려받은 transcript stem(포크 첫 부팅 =
# 부모 세션 id)의 캐릭터 바인딩을 kasaterm 에 조회해 SessionStart 문맥으로 재주입한다.
# 정상 pane 부팅은 조상 argv 에 --append-system-prompt 가 있어 중복 주입하지 않는다.
pp=$PPID; has_persona=""; parent_sid=""
i=0
while [ "$i" -lt 4 ] && [ -n "$pp" ] && [ "$pp" != "0" ] && [ "$pp" != "1" ]; do
  cmd="$(ps -ww -o command= -p "$pp" 2>/dev/null)"
  case "$cmd" in *--append-system-prompt*) has_persona=1; break ;; esac
  # 포크 계보: 조상 argv 의 `--resume <부모 jsonl>` — 포크 첫 부팅은 자기 sid 가
  # 아직 미바인딩이라 부모 stem 폴백이 있어야 persona 가 잡힌다.
  case "$cmd" in
    *" --resume "*)
      rp="${cmd#* --resume }"; rp="${rp%% *}"
      [ -n "$rp" ] && parent_sid="$(basename "$rp" .jsonl)"
      ;;
  esac
  pp=$(ps -o ppid= -p "$pp" 2>/dev/null | tr -d ' ')
  i=$((i + 1))
done
if [ -z "$has_persona" ]; then
  psid="$(basename "$tp" .jsonl)"
  # 포트는 env(스테일 가능)가 아니라 현 인스턴스가 쓰는 mcp_port 파일에서 — 소켓과
  # 같은 TMPDIR 라 인스턴스가 갈려도 최신 것을 읽는다.
  portf="$(dirname "${KASATERM_SOCKET_PATH:-/tmp/nosock}")/mcp_port"
  port="$(cat "$portf" 2>/dev/null)"
  persona="$(curl -s --max-time 2 --get --data-urlencode "sid=$psid" \
    "http://127.0.0.1:${port:-8765}/persona" 2>/dev/null)"
  if [ -z "$persona" ] && [ -n "$parent_sid" ]; then
    persona="$(curl -s --max-time 2 --get --data-urlencode "sid=$parent_sid" \
      "http://127.0.0.1:${port:-8765}/persona" 2>/dev/null)"
  fi
  [ -n "$persona" ] && printf '%s\n' "$persona"
fi
marker="/tmp/kasaterm-bound-${KASATERM_PANE_ID//[^A-Za-z0-9]/_}"
# bind는 데몬 메모리에만 산다(재시작하면 소실). marker는 /tmp에 영속이라, sock
# inode를 dedup 키에 섞지 않으면 데몬 재시작 후에도 "이미 bind함"으로 오판해 영영
# 재-bind를 건너뛴다(=그 pane이 board에서 사라짐). sock이 새로 생기면 inode가
# 바뀌므로 자동으로 재-bind 된다.
sock="${KASATERM_SOCKET_PATH:-$HOME/.config/kasaterm/daemon.sock}"
sig="$(stat -f %i "$sock" 2>/dev/null || stat -c %i "$sock" 2>/dev/null):$tp"
[ "$(cat "$marker" 2>/dev/null)" = "$sig" ] && exit 0
# 소켓 birth = 이번 GUI 세대 시작 시각. roster 의 옛 세대 항목(ts < birth) 을
# archive 하는 기준(아래 python). birth 미지원(Linux %W=0)이면 mtime 으로 폴백 —
# 소켓은 bind-server 시작 때 한 번 생기므로 mtime≈birth.
sock_birth="$(stat -f %B "$sock" 2>/dev/null || stat -c %W "$sock" 2>/dev/null)"
case "$sock_birth" in ''|0) sock_birth="$(stat -f %m "$sock" 2>/dev/null || stat -c %Y "$sock" 2>/dev/null)";; esac
sock_birth="${sock_birth:-0}"
if kasaterm-cli bind-transcript "$tp" >/dev/null 2>&1; then
  printf '%s' "$sig" > "$marker"
  # roster upsert — 재시작 후 `claude --resume` 로 세션을 복원할 수 있게
  # {pane_id, session_id(transcript uuid), cwd, ts} 를 영속 기록한다.
  # **/tmp 아님** (~/.config): 재시작 청소가 /tmp/kasaterm-collab 를 비워도
  # roster 는 살아남아야 복구가 된다. 같은 pane_id 는 갱신(claude --resume 로
  # session 이 바뀌면 최신으로 수렴). 30일 지난 entry 는 prune.
  python3 - "$KASATERM_PANE_ID" "$tp" "$PWD" "$sock_birth" <<'PY' 2>/dev/null || true
import sys, os, json, time
try:
    import fcntl
except ImportError:
    fcntl = None
pane, tp, cwd = sys.argv[1], sys.argv[2], sys.argv[3]
try:
    sock_birth = float(sys.argv[4])
except (IndexError, ValueError):
    sock_birth = 0.0
sid = os.path.splitext(os.path.basename(tp))[0]  # transcript 파일명 = session uuid
# 방별 분리(거노): KASATERM_ROOM 있으면 roster slug 도 방별로(없으면 기존).
_room = os.environ.get('KASATERM_ROOM', '')
slug = cwd.replace('/', '-').replace('.', '-') + (f'__room_{_room}' if _room else '')
d = os.path.expanduser('~/.config/kasaterm/agent-roster')
os.makedirs(d, exist_ok=True)
p = os.path.join(d, slug + '.json')
# 동시 bind(여러 pane 이 같은 cwd roster 를 read-modify-write)의 lost-update 를
# 막는다 — 별도 .lock 에 flock(본 파일은 os.replace 로 inode 가 바뀌므로).
lf = open(p + '.lock', 'w')
if fcntl is not None:
    fcntl.flock(lf.fileno(), fcntl.LOCK_EX)
try:
    try:
        roster = json.load(open(p))
        if not isinstance(roster, dict):
            roster = {}
    except Exception:
        roster = {}
    # entry 는 archived 없이 새로 만든다 = 이 pane 은 지금 활성. claude --resume
    # 로 옛 세션을 부활시켜 이 pane 이 다시 bind 하면 옛 archived 가 빠져(=활성)
    # 자동 복귀한다(munder setArchived 재스폰 false 대응, ④).
    entry = {"pane_id": pane, "session_id": sid, "cwd": cwd, "ts": time.time()}
    roster[pane] = entry
    # 세대 archive(②b·munder 차용): 이 GUI 세대(sock birth) 이전에 기록된 옛
    # pane 항목은 fresh 시작으로 넘어온 잔재 — archive 해 복구목록에서 뺀다.
    # SIGKILL relaunch 처럼 close_pane 훅이 못 도는 경로의 안전망. 삭제 아닌
    # 플래그라 데이터는 남아(resume 가능), 재 bind 시 위에서 해제된다.
    if sock_birth > 0:
        for k, v in roster.items():
            if k != pane and isinstance(v, dict) and v.get("ts", 0) < sock_birth:
                v["archived"] = True
    cutoff = time.time() - 30 * 86400
    roster = {k: v for k, v in roster.items() if v.get("ts", 0) >= cutoff}
    tmp = p + ".tmp"
    json.dump(roster, open(tmp, "w"), ensure_ascii=False)
    os.replace(tmp, p)
finally:
    if fcntl is not None:
        fcntl.flock(lf.fileno(), fcntl.LOCK_UN)
    lf.close()
PY
fi
exit 0
