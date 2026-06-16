#!/bin/bash
# god 전용 워처. god-elect 가 god 선출 시 백그라운드(nohup)로 강제 기동한다 —
# claude Monitor 에 기대지 않고 외부 프로세스로 '감시 반드시 켜짐'을 보장하는 게
# 핵심. 단일 인스턴스는 후발 교체 정책(아래 락)으로 수렴한다.
#
# 역할은 하나: 워커 막힘(승인/입력 대기) → god 에게 1회 msg 알림. 상태 히스토리
# 누적(옛 fleet.log)은 폐기 — 현재 상태는 board-context 가 매 턴 주입하고, 이벤트
# (done 보고·막힘·stop-drain)는 msg 로 push 되므로 읽는 쪽 없는 로그였다
# (2026-06-10 거노 결정).
GOD="${1:-${KASATERM_PANE_ID:-}}"
CLI="${KASATERM_CLI:-kasaterm-cli}"
[ -z "$GOD" ] && exit 0
INTERVAL="${KASATERM_GOD_LOOP_INTERVAL:-4}"
slug=$(pwd | sed 's#[/.]#-#g')${KASATERM_ROOM:+__room_$KASATERM_ROOM}
BASE="/tmp/kasaterm-collab/$slug"
mkdir -p "$BASE"

# ── 단일 인스턴스: 후발 교체 + 원자 탈취 ──
# 옛 방식(살아있으면 양보 + stale rm -rf)의 실측 사고 2종을 막는다:
# ① mkdir 성공~pid 기록 사이 '빈 pid' 찰나를 다른 신생이 stale 로 오판,
#    rm -rf 로 남의 락을 지워 둘 다 진행 → god-loop 2개(거노 실측).
# ② pkill 직후 옛 루프가 시그널 처리 전이면 신생이 '살아있네' 양보 exit,
#    직후 옛 것이 죽어 0개.
# 새 정책: god-elect 가 명시적으로 띄우는 신생이 정통 — 선점자가 살아있어도
# 죽이고 교체한다(소켓 죽은 구세대 좀비·사망 직전 옛 루프에 양보하지 않음).
# 탈취는 mv(rename 원자) — 성공한 단 한 놈만 재선점 자격을 갖는다. 선점 후
# pid 재확인으로 '내 락을 통째 뺏긴' 쪽은 스스로 퇴장해 어떤 인터리빙에서도
# 정확히 1개만 남는다.
LOCK="$BASE/god-loop.lock.d"
claim() {
  mkdir "$LOCK" 2>/dev/null || return 1
  echo $$ > "$LOCK/pid" 2>/dev/null
  [ "$(cat "$LOCK/pid" 2>/dev/null)" = "$$" ]
}
if ! claim; then
  won=""
  for _ in 1 2 3 4 5; do
    oldpid=$(cat "$LOCK/pid" 2>/dev/null)
    if [ -n "$oldpid" ] && [ "$oldpid" != "$$" ] && kill -0 "$oldpid" 2>/dev/null; then
      # 자식(nudge 서브셸·board-watch)을 본체 사망 **전에** 캡처한다 — 본체가
      # 죽는 순간 자식은 init 으로 reparent 돼 pgrep -P 가 빈손이 되는 순서
      # 버그로 고아 watch 가 5개 살아남았다(시뮬 실측).
      oldkids=$(pgrep -P "$oldpid" 2>/dev/null)
      kill "$oldpid" 2>/dev/null
      for _ in 1 2 3 4 5 6 7 8 9 10; do
        kill -0 "$oldpid" 2>/dev/null || break
        sleep 0.2
      done
      [ -n "$oldkids" ] && kill $oldkids 2>/dev/null
    fi
    # TOCTOU 방어: kill~mv 사이에 다른 도전자가 락을 차지했을 수 있다 — 락
    # pid 가 처음 본 oldpid 와 다르면(새 주인) 이 라운드는 탈취하지 않고
    # 재평가한다. stale 정보로 승자의 신선한 락을 통째 mv 해 둘 다 진행되던
    # 사고(시뮬 실측: 두 가족 동시 생존)의 직접 원인.
    curpid=$(cat "$LOCK/pid" 2>/dev/null)
    if [ -n "$curpid" ] && [ "$curpid" != "$oldpid" ] && kill -0 "$curpid" 2>/dev/null; then
      sleep 0.2
      continue
    fi
    if mv "$LOCK" "$LOCK.reap.$$" 2>/dev/null; then
      rm -rf "$LOCK.reap.$$"
    fi
    if claim; then won=1; break; fi
    sleep 0.2
  done
  [ -n "$won" ] || exit 0
fi

cleanup() {
  trap - TERM INT EXIT
  [ "$(cat "$LOCK/pid" 2>/dev/null)" = "$$" ] && rm -rf "$LOCK"
  pkill -P $$ 2>/dev/null
  exit 0
}
# trap 은 락 확정 직후·서브셸 발사 **전**에 건다 — wait 도달 전 찰나에 후발의
# SIGTERM 을 맞으면 trap 미등록으로 즉사해 자식이 고아가 되던 창을 닫는다.
trap cleanup TERM INT EXIT

HOOKS_DIR="$(cd "$(dirname "$0")" && pwd)"

# ── 구세대 자살 기준점 ──
# 소켓 경로에 앱 pid 가 박혀 있어(kasaterm-<pid>.sock) 앱이 재시작하면 경로가
# 사라지거나 inode 가 갈린다. nohup 으로 뜬 이 루프는 앱이 죽어도 살아남아
# 난립하므로(거노 실측: 재시작 전 구버전 잔존), 주기마다 기준 inode 와 비교해
# 다르면 가족 전체(자식 포함)가 자살한다. env 가 없으면 검사 스킵(fail-open).
SOCK="${KASATERM_SOCKET_PATH:-}"
SOCK_INODE=""
[ -n "$SOCK" ] && SOCK_INODE=$(stat -f %i "$SOCK" 2>/dev/null)

# idle nudge — god 방에서 msg 가 tell 을 생략하므로(입력창 오염 방지, kasacollab
# cmd_msg 참조) 미읽 inbox 를 가진 워커를 주기마다 깨운다. 대상 판정:
# board 에 있으면 status==idle 만(working/waiting 은 다음 턴 board-context/
# stop-drain 이 싣는다 — 입력창 청결). board 에 **없어도** list surfaces 에
# 살아있으면 대상 — resume 직후 무입력 pane 은 첫 턴이 없어 bind 가 안 돼
# board 에 안 뜨고, tell 묵음이라 첫 턴 트리거도 없어 msg 가 영영 안 읽히는
# 닭-달걀(거노 실측: 재시작 직후 전원 idle 방치). 첫 턴 전이라 working 일 수
# 없으니 안전하다. (board 부재 pane 이 셸일 가능성은 미읽 msg 보유가 전제라
# 실동선이 없고, solo 방 tell 도 같은 리스크를 이미 감수한다.)
# 같은 미읽 id 세트에는 재nudge 하지 않음(마커 $BASE/god-nudged-<pane> 에
# 세트 저장, 새 메시지로 세트가 바뀌면 재무장).
(
  while sleep "$INTERVAL"; do
    if [ -n "$SOCK_INODE" ]; then
      cur=$(stat -f %i "$SOCK" 2>/dev/null)
      if [ "$cur" != "$SOCK_INODE" ]; then
        kill -TERM $$ 2>/dev/null   # 본체 trap → 가족 전체 정리
        exit 0
      fi
    fi
    kill -0 $$ 2>/dev/null || exit 0   # 본체 사망 시 고아로 남지 않기
    # 락 소유 재검증 — 단일 인스턴스의 **최종 보장**.
    # 두 케이스로 자살: ① 락 pid 가 내 부모(본체)가 아님 — 다른 god-loop 가
    # 탈취했다. ② 본체 자체가 이미 없음(kill 됐는데 cleanup race 로 락이
    # 아직 살아있는 찰나) — 고아 서브셸로 남지 않게.
    _lock_pid=$(cat "$LOCK/pid" 2>/dev/null)
    if [ "$_lock_pid" != "$$" ] || ! kill -0 $$ 2>/dev/null; then
      kill -TERM $$ 2>/dev/null
      exit 0
    fi
    RENUDGE_SECS="${KASATERM_RENUDGE_SECS:-180}"
    now_ts=$(date +%s)
    python3 - "$BASE" "$GOD" <<'PY' 2>/dev/null |
import json, os, subprocess, sys, time
base, god = sys.argv[1], sys.argv[2]
STALE_WAIT_SECS = int(os.environ.get("KASATERM_STALE_WAIT_SECS", "120"))
cli = os.environ.get("KASATERM_CLI", "kasaterm-cli")
def call(*args):
    r = subprocess.run([cli, *args], capture_output=True, text=True, timeout=3)
    return (json.loads(r.stdout).get("result") or {})
try:
    surfaces = call("list", "surfaces").get("surfaces") or []
    live = {s.get("id") for s in surfaces if s.get("id")}
    board = call("board").get("board") or []
except Exception:
    sys.exit(0)  # 조회 실패 시 fail-safe — working 오판 nudge 방지
status = {b.get("surface_id"): b.get("status") for b in board if b.get("surface_id")}
now = int(time.time())
# idle/bind전 → 즉시 대상. waiting → STALE_WAIT_SECS 초과 시 대상(tell 씹힘 자가 복구).
# god 도 대상 — 제외하면 idle god 이 워커 done 보고를 영영 못 받는다(실측).
eligible = set()
for p in live:
    st = status.get(p)
    if st is None or st == "idle":
        eligible.add(p)
    elif st == "waiting":
        since_f = os.path.join(base, f"god-wait-since-{p.lstrip('%')}")
        if not os.path.exists(since_f):
            try:
                open(since_f, "w").write(str(now))
            except Exception:
                pass
            # 첫 관찰: 기록만, 이번엔 skip — 진짜 승인 대기 초반은 보호
        else:
            try:
                since = int(open(since_f).read().strip())
                if now - since >= STALE_WAIT_SECS:
                    eligible.add(p)
            except Exception:
                eligible.add(p)
# waiting 아닌 상태로 바뀐 pane 의 wait-since 마커 정리
for p in live:
    if status.get(p) != "waiting":
        try:
            os.remove(os.path.join(base, f"god-wait-since-{p.lstrip('%')}"))
        except FileNotFoundError:
            pass
if not eligible:
    sys.exit(0)
unread = {}
try:
    with open(os.path.join(base, "messages.jsonl")) as f:
        for line in f:
            try:
                m = json.loads(line)
            except Exception:
                continue
            if not m.get("read") and m.get("to") in eligible:
                unread.setdefault(m["to"], []).append(str(m.get("id")))
except Exception:
    sys.exit(0)
for pane, ids in unread.items():
    print(pane + "\t" + str(len(ids)) + "\t" + ",".join(sorted(ids)))
# compact: 미읽 0 + idle + IDLE_COMPACT_SECS 이상 지속 + board intent 없음 → /compact 1회.
# god 제외(거노 대화 컨텍스트 임의 압축 금지).
# working 관찰 시 compact+idle_since 마커 삭제 → 재무장.
# idle_since 는 working 거친 후 새 idle 진입 시점부터 카운트 — 막 깨운 워커는
# working 안 거치면 이전 since 로 카운트되므로 intent 체크가 2차 안전망 역할.
IDLE_COMPACT_SECS = int(os.environ.get("KASATERM_IDLE_COMPACT_SECS", "1800"))
intent = {b.get("surface_id"): (b.get("intent") or "") for b in board if b.get("surface_id")}
for p in live:
    if p == god:
        continue
    st = status.get(p)
    compact_f = os.path.join(base, f"god-compacted-{p.lstrip('%')}")
    idle_f = os.path.join(base, f"god-idle-since-{p.lstrip('%')}")
    if st == "idle":
        if not os.path.exists(idle_f):
            try: open(idle_f, "w").write(str(now))
            except Exception: pass
        elif p not in unread:
            try:
                since = int(open(idle_f).read().strip())
                # board intent 가 비어있어야 안전 — 진행 중 작업 있으면 compact 보류.
                has_intent = bool(intent.get(p, "").strip())
                if (now - since >= IDLE_COMPACT_SECS
                        and not has_intent
                        and not os.path.exists(compact_f)):
                    subprocess.run([cli, "tell", p, "/compact"],
                                   capture_output=True, timeout=3)
                    open(compact_f, "w").write(str(now))
            except Exception:
                pass
    else:
        if st == "working":
            try: os.remove(compact_f)
            except FileNotFoundError: pass
        try: os.remove(idle_f)
        except FileNotFoundError: pass
PY
    while IFS=$'\t' read -r pane n ids; do
      [ -z "$pane" ] && continue
      marker="$BASE/god-nudged-$pane"
      if [ -f "$marker" ]; then
        stored=$(cat "$marker" 2>/dev/null)
        stored_ids="${stored%%:*}"
        stored_ts="${stored##*:}"
        if [ "$stored_ids" = "$ids" ]; then
          if [[ "$stored_ts" =~ ^[0-9]+$ ]]; then
            age=$((now_ts - stored_ts))
            [ "$age" -lt "$RENUDGE_SECS" ] && continue
          fi
        fi
      fi
      if [ "$pane" = "$GOD" ]; then
        txt="[inbox] 워커 보고 ${n}건 — kasacollab inbox 확인"
      else
        txt="[inbox] 미읽 ${n}건 — kasacollab inbox 확인"
      fi
      # steer 먼저 push — busy 에이전트는 다음 PostToolUse 경계에서 소비(씹힘 없음).
      # tell 은 병행 유지 — idle 에이전트 즉시 깨우기 목적(steer 는 push만, 깨우지 않음).
      python3 "$HOOKS_DIR/kasacollab.py" steer "$pane" "$txt" >/dev/null 2>&1 || true
      "$CLI" tell "$pane" "$txt" >/dev/null 2>&1
      # tell 씹힘 자가복구: 발사 후 화면을 peek — 보낸 텍스트가 입력창에
      # 박혀있으면(메뉴·응답 중 등 제출 실패) enter 를 추가 주입한다.
      sleep 0.4
      _peek=$("$CLI" peek "$pane" 2>/dev/null | python3 -c "
import sys, json
try:
    d = json.load(sys.stdin)
    t = (d.get('result') or {}).get('text', '') or ''
    lines = t.rstrip().splitlines()
    print('\n'.join(lines[-6:]) if lines else '')
except:
    pass" 2>/dev/null)
      if printf '%s' "$_peek" | grep -qF '[inbox]' 2>/dev/null; then
        "$CLI" key --surface "$pane" enter >/dev/null 2>&1
      fi
      echo "${ids}:${now_ts}" > "$marker"
    done
  done
) &
NUDGE_PID=$!

# board-watch = pane 상태 변화 polling stream(1 line/change). 워커가 승인/입력
# 대기로 막히면 god 에게 1회 알림(munder: 워커 프롬프트는 사람이 아니라 god 이
# 처리). GUI 는 워커 프롬프트에 토스트를 안 띄우므로 이 알림이 없으면 막힌
# 워커를 아무도 모른다. 같은 pane 의 대기가 풀리면 마커를 지워 재무장.
# read -t 타임아웃: 스트림은 변화가 없으면 라인을 안 흘려 plain read 는 영원히
# 블록된다 — 본체가 죽어도 watch 서브셸이 고아로 영생하던 원인(시뮬 실측).
# 주기적으로 깨어나 본체 생존을 확인하고, EOF(소켓/스트림 종료)면 끝낸다.
(
  $CLI board-watch "$INTERVAL" 2>/dev/null | while :; do
    line=""
    IFS= read -t 5 -r line
    rc=$?
    kill -0 $$ 2>/dev/null || exit 0
    if [ "$rc" -ne 0 ]; then
      [ "$rc" -gt 128 ] && continue   # 타임아웃 — 생존 체크만 하고 계속
      break                            # EOF — 스트림 종료
    fi
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
) &
WATCH_PID=$!

# 본체는 wait 로 대기 — 옛 구조(board-watch 를 포그라운드로 직접 실행)는
# SIGTERM 을 받아도 bash 가 포그라운드 자식이 끝나길 기다려 trap 이 영영 안
# 돌았다(스트림은 안 끝남). wait 는 시그널에 즉시 깨어나 cleanup 이 돈다.
trap cleanup TERM INT EXIT
wait "$NUDGE_PID" "$WATCH_PID"
