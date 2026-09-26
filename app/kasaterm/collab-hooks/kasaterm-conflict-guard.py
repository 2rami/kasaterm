#!/usr/bin/env python3
"""PreToolUse(Edit|Write|MultiEdit) 겹침 가드.

다른 kasaterm pane이 "지금" 같은 파일을 작업 중이면 이 편집을 deny로 막는다.
진실의 원천은 각 pane의 claude transcript(jsonl) — 별도 락 파일/데몬 없이
transcript를 직접 비교하므로 kasaterm 빌드 상태와 무관하게 동작한다.
kasaterm pane 밖($KASATERM_PANE_ID 없음)이면 no-op.

충돌 판정은 상대의 답변을 기다리면 늦는다. Edit 직전에 transcript를 직접
비교해 판정하고, 차단 안내에서 최신 board 주소로 조율하게 한다.

"지금 작업 중" 판정 — 절대 시간 윈도우가 아니라 진행 상태로:
  - 상대가 그 파일 이후 다른 작업(어떤 tool이든)으로 넘어갔으면 → 손 뗌 → 통과
  - 상대가 그 파일 만지고 IDLE초 넘게 조용하면 → 끝/쉼 → 통과
  - 그 파일이 상대의 '마지막 활동'이고 아직 활발(IDLE 내)일 때만 → 차단
끝난 작업을 시간만으로 계속 막던 문제를 이 진행-상태 판정으로 해소한다.

"잡기"(Perforce 의 독점 체크아웃) — 위 판정은 상대가 다른 도구로 넘어가는 순간
풀려서, 커밋 안 된 남의 수정 위에 또 고쳐 두 학생의 변경이 섞였다. 그래서 편집을
허락할 때마다 그 파일을 그 세션이 잡았다고 적어 두고(claims.json), 그 파일이 git 에서
아직 더럽고 잡은 뒤 커밋이 없고 잡은 세션이 살아 있으면 남의 편집을 막는다. 커밋·되돌리기
·창 닫기로 저절로 풀리고, 넘길 때는 `release` 로 놓는다.
"""
import sys, json, os, glob, subprocess, time
from datetime import datetime

try:
    import fcntl
except ImportError:  # Windows — 잠금 없이 원자적 교체만 한다.
    fcntl = None

# 상대가 그 파일 만진 뒤 이 초 넘게 조용하면 "손 뗌"으로 보고 통과시킨다.
IDLE = int(os.environ.get("KASATERM_CONFLICT_IDLE", "15"))
# 이 시간 넘게 활동 없는 transcript는 아예 스캔하지 않는다(싼 1차 필터).
STALE = int(os.environ.get("KASATERM_CONFLICT_WINDOW", "90"))
# transcript 꼬리에서 이만큼만 읽는다(큰 jsonl 전체 파싱 회피).
TAIL_BYTES = 300_000
# 이보다 오래된 잡기 기록은 쓸 때 걷는다 — 그 사이 커밋이 없었더라도 잡은 세션은 대개 끝났다.
CLAIM_TTL = 7 * 24 * 3600


def claims_path():
    # 검증 인스턴스는 KASATERM_COLLAB_ROOT 로 협업 상태를 가른다 — 잡기도 같은 뿌리에.
    root = os.environ.get("KASATERM_COLLAB_ROOT") or os.path.expanduser("~/.config/kasaterm")
    return os.path.join(root, "claims.json")


def load_claims(path=None):
    try:
        with open(path or claims_path()) as f:
            d = json.load(f)
        return d if isinstance(d, dict) else {}
    except Exception:
        return {}


def update_claims(change):
    """claims.json 을 잠그고 읽어 `change(d)` 로 고친 뒤 원자적으로 바꿔 쓴다."""
    path = claims_path()
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path + ".lock", "a") as lock:
        if fcntl:
            fcntl.flock(lock, fcntl.LOCK_EX)
        d = load_claims(path)
        result = change(d)
        tmp = f"{path}.{os.getpid()}.tmp"
        with open(tmp, "w") as f:
            json.dump(d, f, ensure_ascii=False, indent=1)
        os.replace(tmp, path)
        return result


def record_claim(fp, session, pane):
    now = time.time()

    def change(d):
        for k in [k for k, v in d.items() if not isinstance(v, dict) or now - v.get("ts", 0) > CLAIM_TTL]:
            del d[k]
        d[fp] = {"session": session, "pane": pane, "ts": now}

    update_claims(change)


def release_claims(pane, files):
    """이 pane 이 잡은 것을 놓는다(files 가 비면 전부). 놓은 개수."""
    want = {os.path.realpath(f) for f in files}

    def change(d):
        gone = [k for k, v in d.items()
                if isinstance(v, dict) and v.get("pane") == pane and (not want or k in want)]
        for k in gone:
            del d[k]
        return len(gone)

    return update_claims(change)


def git(fp, *args):
    """그 파일이 든 폴더에서 git — 레포 밖이거나 실패면 None."""
    d = os.path.dirname(fp)
    while d and not os.path.isdir(d):
        d = os.path.dirname(d)
    try:
        r = subprocess.run(["git", "-C", d or ".", *args], capture_output=True, text=True, timeout=2)
    except Exception:
        return None
    return r.stdout if r.returncode == 0 else None


def live_panes():
    """살아 있는 이 기기 pane 의 세션 id → (surface_id, 캐릭터, 제목). 보드를 못 읽으면 None."""
    cli = os.environ.get("KASATERM_CLI", "kasaterm-cli")
    try:
        out = subprocess.run([cli, "board", "--local"], capture_output=True, text=True, timeout=2).stdout
        panes = json.loads(out)["result"]["panes"]
    except Exception:
        return None
    live = {}
    for p in panes:
        sid = (p.get("address") or {}).get("session_id")
        if sid:
            live[sid] = ((p.get("address") or {}).get("surface_id"), p.get("character") or "", p.get("title") or "")
    return live


def holder(fp, session, me):
    """남이 잡고 있으면 (claim, pane 정보). 못 가르면 막지 않는다 — 가드는 틀려도 열리는 쪽이어야 한다."""
    c = load_claims().get(fp)
    if not isinstance(c, dict) or not c.get("session"):
        return None
    if c.get("session") == session or (me and c.get("pane") == me):
        return None
    status = git(fp, "status", "--porcelain", "--", fp)
    if not status or not status.strip():
        return None  # 레포 밖이거나 이미 커밋·되돌림 → 놓인 것.
    try:
        committed = int((git(fp, "log", "-1", "--format=%ct", "--", fp) or "0").strip() or 0)
    except ValueError:
        committed = 0
    if c.get("ts", 0) <= committed:
        return None  # 잡은 뒤 커밋이 있었다 — 지금 더러운 건 다른 누군가의 새 변경.
    live = live_panes()
    if not live or c["session"] not in live:
        return None  # 잡은 세션이 끝났다(창 닫힘·새 대화).
    return c, live[c["session"]]


def deny(reason):
    print(json.dumps({"hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "permissionDecision": "deny",
        "permissionDecisionReason": reason,
    }}, ensure_ascii=False))


COORD = (
    "kasaterm-cli board --all에서 기기·방·신원을 확인하고, 최신 address 전체로 "
    "kasaterm-cli tell --address '<주소 JSON>' --stdin을 사용하세요. "
    "대상이 불명확하면 보내지 마세요. 사용자 범위 안에서 담당만 조율하고, "
    "위험·취향·범위 질문은 사용자에게 직접 하세요. "
)


def parse_ts(s):
    try:
        return datetime.fromisoformat(s.replace("Z", "+00:00")).timestamp()
    except Exception:
        return None


def scan(jf, fp):
    """jf 꼬리에서 (fp를 Edit/Write한 마지막 ts, 아무 tool_use의 마지막 ts).

    last_file == last_any 면 그 파일이 이 pane의 가장 최근 활동(아직 붙잡음).
    last_any > last_file 면 그 파일 뒤 다른 작업으로 넘어간 것(손 뗌)."""
    try:
        with open(jf, "rb") as f:
            f.seek(0, 2)
            size = f.tell()
            f.seek(max(0, size - TAIL_BYTES))
            chunk = f.read().decode("utf-8", "ignore")
    except OSError:
        return None, None
    last_file = None
    last_any = None
    for line in chunk.splitlines():
        if '"tool_use"' not in line:
            continue
        try:
            obj = json.loads(line)
        except Exception:
            continue
        ts = parse_ts(obj.get("timestamp", "") or "")
        if ts is None:
            continue
        for c in (obj.get("message") or {}).get("content") or []:
            if not (isinstance(c, dict) and c.get("type") == "tool_use"):
                continue
            if last_any is None or ts > last_any:
                last_any = ts
            if (
                c.get("name") in ("Edit", "Write", "MultiEdit")
                and (c.get("input") or {}).get("file_path") == fp
            ):
                if last_file is None or ts > last_file:
                    last_file = ts
    return last_file, last_any


def roster_pane(cwd, sid):
    """이 transcript(sid)를 돌리는 pane — roster 가 pane↔session 의 정본이다.

    차단 판정은 남의 jsonl 로 하면서 '누구'는 board 에서 **파일 경로**로 찾던 게
    문제였다: 같은 파일을 오늘 만진 pane 이 여럿이면 아무나 걸리고, 하필 나
    자신이 걸리면 "%3이 작업 중이라 막았다"는 자가당착이 된다(2026-08-11 실측 —
    내 Edit 이 내 이름으로 거부됐다). 세션 id 로 주인을 확정하면 안 어긋난다."""
    enc = cwd.replace("/", "-").replace(".", "-")
    path = os.path.expanduser("~/.config/kasaterm/agent-roster/" + enc + ".json")
    try:
        with open(path) as f:
            d = json.load(f)
    except Exception:
        return None
    for key, rec in (d or {}).items():
        if not isinstance(rec, dict):
            continue
        rs = str(rec.get("session_id") or "")
        # codex 는 `rollout-<날짜>-<uuid>` 라 접미 일치도 본다.
        if rs and (rs == sid or rs.endswith(sid)):
            return rec.get("pane_id") or key
    return None


def pane_doing(fp, owner=None):
    """차단 원인 pane 의 (surface_id, intent).

    deny 메시지에 '누구'와 '무슨 작업 중'을 채워, 막기를 조율(합류/회피)로
    끌어올리기 위함. `owner`(roster 로 확정한 주인)가 있으면 그 pane 의 intent 를
    쓰고, 없을 때만 파일 경로로 훑는다 — 그때도 **나 자신은 건너뛴다.**
    board 조회 실패하면 (owner, None)."""
    cli = os.environ.get("KASATERM_CLI", "kasaterm-cli")
    me = os.environ.get("KASATERM_PANE_ID")
    try:
        out = subprocess.run(
            [cli, "board"], capture_output=True, text=True, timeout=2
        ).stdout
        board = json.loads(out)["result"]["board"]
    except Exception:
        return owner, None
    if owner:
        for p in board:
            if p.get("surface_id") == owner:
                return owner, (p.get("intent") or "")
        return owner, None
    for p in board:
        if p.get("surface_id") == me:
            continue
        if fp in (p.get("files") or []):
            return p.get("surface_id"), (p.get("intent") or "")
    return None, None


def project_slug(cwd):
    """Claude Code 가 transcript 를 두는 `~/.claude/projects/<slug>` 의 이름 규칙.

    Windows 는 드라이브 콜론과 역슬래시까지 같이 접힌다 —
    `C:\\Users\\kshkj\\desktop` → `C--Users-kshkj-desktop`(실측). `/`·`.` 만
    접던 종전 규칙은 Windows 에서 존재할 수 없는 폴더를 가리켜, 아래 isdir
    가드에 걸려 충돌 감지가 통째로 무음 정지했다.

    추가 두 글자는 Windows 에서만 접는다 — unix 폴더 이름엔 `\\`·`:` 가 실제로
    들어갈 수 있어, 거기서까지 접으면 멀쩡하던 경로가 어긋난다.
    """
    chars = ("/", ".", "\\", ":") if os.name == "nt" else ("/", ".")
    for ch in chars:
        cwd = cwd.replace(ch, "-")
    return cwd


def active_editor(payload, fp, me):
    """상대가 **바로 지금** 그 파일을 고치는 중이면 차단 사유. 잡기 기록이 없는 세션(훅이
    붙기 전에 떠 있던 pane 등)도 이것으로는 걸린다."""
    # 빈 문자열을 realpath 하면 **cwd** 가 나와 어떤 jsonl 과도 안 맞는다 —
    # 그러면 아래 자기 제외가 통째로 무력해진다(빈 값이 틀린 값보다 위험한 자리).
    raw_tp = payload.get("transcript_path") or ""
    my_tp = os.path.realpath(raw_tp) if raw_tp else ""
    cwd = payload.get("cwd") or os.getcwd()
    proj = os.path.expanduser("~/.claude/projects/" + project_slug(cwd))
    if not os.path.isdir(proj):
        return None

    now = datetime.now().timestamp()
    for jf in glob.glob(os.path.join(proj, "*.jsonl")):
        # 내 transcript는 제외 — 내가 방금 만진 걸 충돌로 오인하면 안 된다.
        if my_tp and os.path.realpath(jf) == my_tp:
            continue
        # transcript 는 하나가 아니다 — resume 로 갈린 이전 대화, 서브에이전트가
        # 같은 폴더에 자기 jsonl 을 남긴다. 그 주인이 나면 내 편집이 내 이름으로
        # 막힌다(2026-08-11 실측). 세션 id → pane 은 roster 가 정본.
        owner = roster_pane(cwd, os.path.basename(jf).split(".")[0])
        if owner and me and owner == me:
            continue
        try:
            if now - os.path.getmtime(jf) > STALE:
                continue  # 한참 조용한 pane은 볼 것도 없다.
        except OSError:
            continue
        last_file, last_any = scan(jf, fp)
        if last_file is None:
            continue  # 이 파일을 만진 적 없음.
        if last_any is not None and last_any > last_file + 0.001:
            continue  # 그 파일 뒤 다른 작업으로 넘어감 → 손 뗌.
        if now - last_file > IDLE:
            continue  # 그 파일 만지고 조용해짐 → 끝/쉼.
        # 그 파일이 상대의 마지막 활동이고 아직 활발 → 진짜 작업 중 → 차단.
        # 단순 차단이 아니라 '누가/뭘' 하는지 + 합류·회피 선택지를 줘서 조율로.
        name = os.path.basename(fp)
        secs = int(now - last_file)
        pane, intent = pane_doing(fp, owner)
        sid = os.path.basename(jf).split(".")[0]
        who = pane or f"다른 pane({sid[:8]}…)"
        doing = f" (지금: {intent[:70]})" if intent else ""
        return (
            f"{who}이 {secs}초 전부터 '{name}'을(를) 작업 중이에요{doing}. "
            "같은 파일 겹침을 막았어요. "
            + COORD
            + "독립 작업은 다른 파일부터 하세요. 이 파일은 상대가 손을 뗀 뒤 다시 시도하세요."
        )
    return None


def held_reason(fp, claim, pane_info):
    surface, character, title = pane_info
    who = f"{character}({surface})" if character else (surface or "다른 pane")
    mins = int((time.time() - claim.get("ts", time.time())) // 60)
    doing = f" (지금: {title[:70]})" if title else ""
    me = os.path.realpath(__file__)
    return (
        f"{who}이 '{os.path.basename(fp)}'을(를) 고친 뒤 아직 커밋하지 않아 잡고 있어요"
        f"({mins}분 전부터){doing}. 커밋하거나 되돌리기 전까지 다른 학생은 이 파일을 못 고쳐요. "
        + COORD
        + f"상대에게 커밋을 부탁하거나, 넘겨받아야 하면 상대 pane 에서 `python3 \"{me}\" release \"{fp}\"` "
        "로 놓아 달라고 하세요. 독립 작업은 다른 파일부터 하세요."
    )


def cli(args):
    """`release [파일…]` — 이 pane 이 잡은 파일을 놓는다(넘겨줄 때). `list` — 이 기기의 잡기 목록."""
    me = os.environ.get("KASATERM_PANE_ID")
    if args[0] == "release":
        if not me:
            print("kasaterm pane 안에서만 놓을 수 있어요(KASATERM_PANE_ID 없음).", file=sys.stderr)
            return 1
        print(f"{release_claims(me, args[1:])}개 놓았어요.")
        return 0
    if args[0] == "list":
        live = live_panes() or {}
        for fp, c in sorted(load_claims().items()):
            if not isinstance(c, dict):
                continue
            info = live.get(c.get("session"))
            status = git(fp, "status", "--porcelain", "--", fp)
            state = "잡음" if info and status and status.strip() else "풀림"
            who = f"{info[1]}({info[0]})" if info else f"{c.get('pane')}(끝남)"
            print(f"{state}\t{who}\t{fp}")
        return 0
    print("사용법: release [파일…] | list", file=sys.stderr)
    return 2


def main():
    if len(sys.argv) > 1:
        sys.exit(cli(sys.argv[1:]))
    me = os.environ.get("KASATERM_PANE_ID")
    if not me:
        return
    try:
        payload = json.load(sys.stdin)
    except Exception:
        return
    if payload.get("tool_name") not in ("Edit", "Write", "MultiEdit"):
        return
    fp = (payload.get("tool_input") or {}).get("file_path", "")
    if not fp:
        return
    # 잡기 표는 실제 경로로 적는다(같은 파일을 심링크로 불러도 한 칸). transcript 비교는
    # 도구에 적힌 그대로여야 맞으니 원래 경로를 넘긴다.
    real = os.path.realpath(fp)
    session = payload.get("session_id") or ""

    held = holder(real, session, me)
    if held:
        deny(held_reason(real, *held))
        return
    reason = active_editor(payload, fp, me)
    if reason:
        deny(reason)
        return
    if session:
        try:
            record_claim(real, session, me)
        except Exception:
            pass  # 기록 실패로 편집까지 막지는 않는다.


if __name__ == "__main__":
    main()
