#!/usr/bin/env python3
"""kasacollab — pane 간 협업 공유 상태 CLI (빌드 무관, 파일 기반).

board(감지)·conflict-guard(차단) 위에 얹는 '분담·대화' 층. 두 가지 공유 상태를
/tmp/kasaterm-collab/<cwd>/ 아래 파일로 관리한다:
  - tasks.json     : 작업판 — 누가 무슨 일을 맡았나(중복 분담 방지)
  - messages.jsonl : 메시지함 — pane끼리 주고받는 메모/지시/질문

매 턴 자동 주입(옛 board-context)은 폐기 — `kasacollab task list`/`inbox` 로
직접 조회한다. 모니터링은 팀장이 lead-watch 를 Monitor 에 거는 방식(아래 lead).

사용:
  kasacollab task add "BSP 레이아웃 리팩터"   # 내 pane이 이 일 맡음(선언)
  kasacollab task list                          # 전체 작업판
  kasacollab task done <id>                      # 완료
  kasacollab task drop <id>                      # 취소
  kasacollab msg %2 "이 파일 곧 끝나"           # %2에게 메시지
  kasacollab inbox                               # 내게 온 미읽 메시지(읽음 처리)
"""
import sys, os, json, time, subprocess, re, glob
from contextlib import contextmanager

try:
    import fcntl  # POSIX 전용 — Windows pane 에선 없음(아래 _locked 가 no-op 폴백)
except ImportError:
    fcntl = None


@contextmanager
def _locked(path):
    """`path` 에 대한 배타 락(임계구역). 본 파일이 아니라 별도 `<path>.lock` 에
    flock 을 잡는다 — read-modify-write 가 본 파일을 os.replace 로 갈아치우면
    inode 가 바뀌어 본 파일에 잡은 flock 은 새 파일과 무관해지지만, 락 파일은
    교체되지 않아 inode 가 불변이라 직렬화가 유지된다. fcntl 없는 플랫폼
    (Windows)에선 no-op — atomic replace 만으로 부분읽기는 막고 lost-update 만
    남으나 협업은 POSIX pane 위주라 허용."""
    if fcntl is None:
        yield
        return
    lockp = path + ".lock"
    f = open(lockp, "w")
    try:
        fcntl.flock(f.fileno(), fcntl.LOCK_EX)
        yield
    finally:
        try:
            fcntl.flock(f.fileno(), fcntl.LOCK_UN)
        finally:
            f.close()


def _write_lines_atomic(path, lines):
    """lines 를 tmp 에 쓰고 os.replace 로 원자 교체 — 읽는 쪽은 항상 완전한
    옛 파일 또는 완전한 새 파일을 본다(중간 잘린 상태 없음)."""
    tmp = path + ".tmp"
    with open(tmp, "w") as f:
        for l in lines:
            f.write(l + "\n")
    os.replace(tmp, path)


def _schale_state_path():
    home = os.environ.get("HOME", "")
    if not home:
        return None
    return os.path.join(home, ".config", "kasaterm", "schale-state.json")


def _reward_done():
    """done 보고 1건 → Credit+50, Exp+30. Exp 300마다 Gold+1000."""
    path = _schale_state_path()
    if not path:
        return
    default = {"credits": 0, "gold": 0, "affinity_lv": 1, "exp": 0}
    try:
        with open(path) as f:
            s = json.load(f)
    except Exception:
        s = dict(default)
    credits = s.get("credits", 0) + 50
    exp_prev = s.get("exp", 0)
    gold = s.get("gold", 0)
    affinity_lv = s.get("affinity_lv", 1)
    exp_new = exp_prev + 30
    gained_lv = exp_new // 300 - exp_prev // 300
    gold += gained_lv * 1000
    exp = exp_new % 300
    affinity_lv += gained_lv
    new_state = {"credits": credits, "gold": gold, "affinity_lv": affinity_lv, "exp": exp}
    try:
        os.makedirs(os.path.dirname(path), exist_ok=True)
        tmp = path + ".tmp"
        with open(tmp, "w") as f:
            json.dump(new_state, f)
        os.replace(tmp, path)
    except Exception:
        pass


def _room_suffix():
    """방별 분리(거노): KASATERM_ROOM env 가 있으면 slug 에 `__room_<id>` 를 붙여
    같은 cwd 의 다른 방(윈도우)과 collab(board/inbox/roster)을 격리한다.
    env 없으면 빈 문자열 → 기존 동작 그대로(역호환). 모든 slug 계산이 이걸 더한다."""
    room = os.environ.get("KASATERM_ROOM", "")
    return f"__room_{room}" if room else ""


def base():
    enc = os.getcwd().replace("/", "-").replace(".", "-") + _room_suffix()
    d = os.path.join("/tmp/kasaterm-collab", enc)
    os.makedirs(d, exist_ok=True)
    return d


def me():
    return os.environ.get("KASATERM_PANE_ID", "?")


# ── sid 라우팅 헬퍼 ────────────────────────────────────────────────
# 메시지 주소를 pane id 가 아니라 session id(sid)에 묶는다 — pane id 는 재시작
# 재배치마다 주인이 바뀌어 옛 주소 메시지가 엉뚱한 pane 에 오배달됐다(06-10
# 실측: 시로코 %3→%2 재배치로 발주가 유우카에게). sid 는 claude 세션에 영속.
# pane↔sid 매핑은 bind 마커(/tmp/kasaterm-bound-<N>, 내용 '<sock inode>:<tp>')
# — inode 가 현 데몬과 다르면 옛 세대 잔재라 무시(복구가드 v2 와 같은 규칙).

BOUND_GLOB = "/tmp/kasaterm-bound-*"  # 테스트가 임시 디렉터리로 패치한다

_SID_RE = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")


def is_sid(s):
    return bool(s) and bool(_SID_RE.fullmatch(s))


def _sock_inode():
    sock = os.environ.get("KASATERM_SOCKET_PATH") or \
        os.path.expanduser("~/.config/kasaterm/daemon.sock")
    try:
        return str(os.stat(sock).st_ino)
    except OSError:
        return None


def pane_sid(pane):
    """pane 의 현 세대 bound sid. 마커 없음/옛 세대/소켓 불명이면 None —
    이때 호출처는 레거시 pane id 주소로 폴백한다(fail-open)."""
    enc = re.sub(r"[^A-Za-z0-9]", "_", pane)
    base_dir = os.path.dirname(BOUND_GLOB)
    try:
        raw = open(os.path.join(base_dir, f"kasaterm-bound-{enc}")).read().strip()
    except OSError:
        return None
    ino, _, tp = raw.partition(":")
    cur = _sock_inode()
    if cur is None or ino != cur:
        return None
    sid = os.path.splitext(os.path.basename(tp))[0]
    return sid or None


def my_sid():
    return pane_sid(me())


def sid_to_pane(sid):
    """sid 가 지금 bind 된 pane — 현 세대 마커 첫 매칭. 같은 sid 두 pane(이중
    attach)은 복구가드가 막는 전제라 첫 매칭으로 충분. 없으면 None."""
    for bm in sorted(glob.glob(BOUND_GLOB)):
        enc = os.path.basename(bm).split("kasaterm-bound-", 1)[-1]
        pane = "%" + (enc[1:] if enc.startswith("_") else enc)
        if pane_sid(pane) == sid:
            return pane
    return None


def addr_label(addr):
    """저장 주소(sid 또는 레거시 %N)를 사람이 읽는 pane 표기로 — 사람과 board
    는 pane 으로 소통한다. sid 면 현재 bind pane, bind 가 없으면(세션 죽음/
    재시작 직후) sid 앞 8자로 식별만."""
    if not is_sid(addr):
        return addr
    return sid_to_pane(addr) or addr[:8]


def tasks_path():
    return os.path.join(base(), "tasks.json")


def msgs_path():
    return os.path.join(base(), "messages.jsonl")


def load_tasks():
    try:
        return json.load(open(tasks_path()))
    except Exception:
        return []


def save_tasks(t):
    p = tasks_path()
    tmp = p + ".tmp"
    json.dump(t, open(tmp, "w"), ensure_ascii=False)
    os.replace(tmp, p)  # 원자 교체 — 읽는 쪽은 완전한 옛/새 파일만 본다


def short_id():
    # time+pane 기반 4hex. hashlib.md5는 이 머신 framework python 3.14에서
    # import 자체가 행한다(OpenSSL 로드 블록) — id 생성에만 쓰던 의존을 끊어
    # 어느 python3 에서도 즉시 돌게 한다. 한 ms 안의 충돌은 pane id XOR로 분산.
    return f"{(int(time.time() * 1000) ^ hash(me())) & 0xFFFF:04x}"


def cmd_task(args):
    if not args:
        print("kasacollab task add|list|done|drop")
        return
    sub = args[0]
    if sub == "list":
        # read-only — atomic write 라 완전한 파일을 보므로 락 불필요.
        live = [i for i in load_tasks() if i.get("status") == "doing"]
        if not live:
            print("(진행 중 작업 없음)")
            return
        for i in live:
            print(f"[{i['id']}] {i['pane']}: {i['desc']}")
    elif sub == "add":
        desc = " ".join(args[1:]).strip()
        if not desc:
            print("desc 필요: task add \"<무슨 일>\"")
            return
        item = {"id": short_id(), "pane": me(), "desc": desc,
                "status": "doing", "ts": time.time()}
        # 락 안에서 최신 스냅샷 재load → 변경 → atomic save (lost-update 방지).
        with _locked(tasks_path()):
            t = load_tasks()
            t.append(item)
            save_tasks(t)
        print(f"맡음 [{item['id']}] {desc}")
    elif sub in ("done", "drop"):
        if len(args) < 2:
            print(f"{sub} <id>")
            return
        tid = args[1]
        with _locked(tasks_path()):
            t = load_tasks()
            hit = False
            for i in t:
                if i["id"] == tid:
                    i["status"] = "done" if sub == "done" else "dropped"
                    hit = True
            save_tasks(t)
        print(f"{tid} {sub}" if hit else f"{tid} 없음")
    else:
        print("kasacollab task add|list|done|drop")


def read_msgs():
    p = msgs_path()
    if not os.path.exists(p):
        return []
    try:
        return [json.loads(l) for l in open(p).read().splitlines() if l.strip()]
    except Exception:
        return []


def append_msg(m):
    """메시지 1건 추가 — 락 안에서. read-modify-write(drain_unread)와 같은 락을
    공유해, 마킹이 도는 사이 append 가 끼어 옛 스냅샷 재작성에 유실되는 걸 막는다."""
    p = msgs_path()
    with _locked(p):
        with open(p, "a") as f:
            f.write(json.dumps(m, ensure_ascii=False) + "\n")


def drain_unread():
    """내게 온 미읽 메시지(to==me·from!=me·read=False)를 read=True 로 마킹하고
    그 목록을 반환. **inbox·drain-stop·board-context 가 공유하는 단일 임계구역** —
    락+atomic 으로 lost-update(동시 재작성이 마킹 유실)을 구조로 막는다. 없으면 []."""
    p = msgs_path()
    if not os.path.exists(p):
        return []
    with _locked(p):
        msgs = read_msgs()
        # 내 주소 = 내 sid(신규 라우팅) + 내 pane id(레거시 메시지 호환).
        # sid 가 안 잡히면(마커 없음 찰나) pane 기준만 — 다음 턴에 재시도된다.
        sid = my_sid()
        mine_addrs = {me()} | ({sid} if sid else set())
        mine = [m for m in msgs
                if m.get("to") in mine_addrs
                and m.get("from") not in mine_addrs and not m.get("read")]
        if not mine:
            return []
        for m in mine:
            m["read"] = True
        _write_lines_atomic(p, [json.dumps(m, ensure_ascii=False) for m in msgs])
        return mine


def live_panes():
    """실재하는 surface_id 집합(`list surfaces`). 조회 실패면 None(검증 스킵).
    msg 보낼 때 받는 id 가 실재하는지 즉시 확인하는 데 쓴다 — stale pane id
    (재시작 전 %3 등)나 오타로 죽은 pane 에 보내 좀비 메시지가 쌓이는 걸 막는다.
    board(bind 기반)를 쓰면 resume 직후 아직 프롬프트를 안 받아 bind 안 된
    claude pane 을 '죽음'으로 거부하는 닭-달걀(깨워야 bind 되는데 msg 가 막힘)
    이 생겨 surfaces 기준으로 판정한다(2026-06-10 실측)."""
    cli = os.environ.get("KASATERM_CLI", "kasaterm-cli")
    try:
        r = subprocess.run([cli, "list", "surfaces"], capture_output=True, text=True, timeout=3)
        if r.returncode != 0:
            return None
        surfaces = (json.loads(r.stdout).get("result") or {}).get("surfaces") or []
        ids = {s.get("id") for s in surfaces if s.get("id")}
        return ids or None
    except Exception:
        return None


def cmd_msg(args):
    if len(args) < 2:
        print('msg <pane> "<text>"')
        return
    to, text = args[0], " ".join(args[1:])
    if to == me():
        print(f"자기 자신({to})에게는 못 보냄 — 내 pane id가 {me()}다. "
              f"받는 상대 id를 board에서 다시 확인해라.")
        return
    # 받는 pane 이 board 에 실재하는지 즉시 검증(fail-open: 조회 불가면 통과).
    # 죽은/없는 id 면 보내지 않고 살아있는 상대 목록을 띄운다 — 좀비 메시지·
    # "허공에 대고 보고" 방지(사용자가 일일이 알려줄 필요 없음).
    live = live_panes()
    if live is not None and to not in live:
        others = ", ".join(sorted(live - {me()})) or "(다른 pane 없음)"
        print(f"'{to}' 는 지금 살아있는 pane 이 아니야 — 메시지 안 보냄. "
              f"board 에 있는 상대: {others}. `kasaterm-cli board` 로 확인해라.")
        return
    # 주소는 sid 로 저장(재시작 재배치에도 같은 세션에 닿는다). 전송 시점의
    # pane 은 *_pane 에 박제 — 표시·디버그용일 뿐 라우팅엔 안 쓴다. bound 가
    # 없는 상대(셸 pane/claude 부팅 전)는 레거시 pane 주소 폴백.
    m = {"id": short_id(),
         "from": my_sid() or me(), "from_pane": me(),
         "to": pane_sid(to) or to, "to_pane": to,
         "text": text, "ts": time.time(), "read": False}
    append_msg(m)  # 락 안 append — drain 과 같은 임계구역
    if text.startswith("done:"):
        _reward_done()
    # steer push — 입력창(PTY) 우회. busy 에이전트도 다음 PostToolUse 경계에서
    # additionalContext로 pull(tell 씹힘 없음, munder 정렬). to_pane = 현 pane id.
    try:
        p = steer_path(to)
        tmp = p + ".tmp"
        with open(tmp, "w", encoding="utf-8") as f:
            f.write(text)
        os.replace(tmp, p)
    except Exception:
        pass
    # steer 는 이미 push됨(상대 다음 훅 경계에서 pull). tell 로 즉시 깨운다.
    cli = os.environ.get("KASATERM_CLI", "kasaterm-cli")
    woke = False
    try:
        r = subprocess.run(
            [cli, "tell", to, f"[inbox] {me()} 메시지 도착 — 위 '받은 메시지' 확인하고 "
             f"필요하면 답장(kasacollab msg {me()} \"...\")"],
            capture_output=True, text=True, timeout=3)
        woke = r.returncode == 0
    except Exception:
        pass
    print(f"→ {to}: {text}" + (" · tell 로 깨움" if woke else " · (tell 실패, inbox 에만 쌓임)"))


def cmd_inbox(args):
    mine = drain_unread()
    if not mine:
        print("(새 메시지 없음)")
        return
    for m in mine:
        print(f"{addr_label(m.get('from', '?'))}: {m['text']}")


def cmd_drain_stop(args):
    # Stop hook 전용 inbox drain (munder drainForStop 이식). claude 가 턴을
    # 끝내려 할 때 내게 온 미읽 메시지가 있으면 reason 텍스트를 stdout 으로
    # 내고 exit 10 → Stop hook 스크립트가 {"decision":"block"} 로 멈춤을 막아
    # claude 가 그 메시지를 처리하게 강제한다. 없으면 exit 0(그냥 멈춤).
    #
    # 멱등: surface 하는 즉시 read=True 마킹한다 — munder 의 cursor.json(id>
    # lastProcessed) 대용. 우리 short_id 는 16비트 충돌+비단조라 id 비교가
    # 불가능하므로 board-context.py 가 이미 쓰는 'read' 플래그를 멱등 키로
    # 공유한다. 한 번 surface 된 메시지는 read=True 라 다음 Stop 에 안 잡혀
    # 무한루프가 안 난다(+ Stop hook 스크립트의 stop_hook_active 가드로 이중).
    # drain_unread 가 락+atomic 으로 마킹해 동시 재작성이 마킹을 유실시켜 같은
    # 메시지가 재surface 되던 lost-update 사고(거노 실측)를 구조로 막는다.
    mine = drain_unread()
    if not mine:
        sys.exit(0)
    lines = "\n".join(f"- {addr_label(m.get('from', '?'))}: {m['text']}" for m in mine)
    reason = (f"끝내기 전에 inbox 에 안 읽은 협업 메시지 {len(mine)}건이 있어. "
              f"각각 확인하고 필요하면 답장(kasacollab msg <상대> \"...\")해라:\n{lines}")
    # Stop hook 의 stdout JSON 으로 멈춤을 막는다(munder 검증 형식 — command
    # hook 도 top-level decision:block 을 읽는다). reason 은 다음 턴 지시로 주입.
    # json.dumps 로 개행·따옴표를 안전 인코딩 → shell 이 JSON 을 안 만져도 됨.
    print(json.dumps({"decision": "block", "reason": reason}, ensure_ascii=False))
    sys.exit(10)  # 내부 신호: stop-drain.sh 가 complete 알림을 건너뛰게


def _steer_dir():
    d = os.path.join(base(), "steer")
    os.makedirs(d, exist_ok=True)
    return d


def _steer_enc(pane):
    return pane.replace("%", "_pane_")


def steer_path(pane):
    return os.path.join(_steer_dir(), f"{_steer_enc(pane)}.txt")


def cmd_steer(args):
    """steer 큐 — PostToolUse additionalContext 경로로 메시지 전달.
    tell(PTY 주입)과 달리 훅 경계에서 반드시 소비되므로 busy 에이전트에도 씹히지 않음."""
    if not args:
        print("kasacollab steer <pane> <text>  — steer 큐 push (1건 덮어쓰기)")
        print("kasacollab steer drain           — 내 steer 큐 drain (stdout 후 삭제)")
        return
    sub = args[0]
    if sub == "drain":
        p = steer_path(me())
        try:
            with open(p) as f:
                text = f.read()
            os.remove(p)
            sys.stdout.write(text)
        except FileNotFoundError:
            pass
        return
    to = sub
    text = " ".join(args[1:])
    if not text:
        print("text 필요")
        return
    p = steer_path(to)
    tmp = p + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        f.write(text)
    os.replace(tmp, p)
    print(f"steer → {to}: {text[:80]}")


def main():
    a = sys.argv[1:]
    if not a:
        print("kasacollab task|msg|inbox|steer|drain-stop")
        return
    {"task": cmd_task, "msg": cmd_msg, "inbox": cmd_inbox,
     "steer": cmd_steer, "drain-stop": cmd_drain_stop}.get(
        a[0], lambda _: print("kasacollab task|msg|inbox|steer|drain-stop")
    )(a[1:])


if __name__ == "__main__":
    main()
