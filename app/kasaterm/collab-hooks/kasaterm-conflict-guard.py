#!/usr/bin/env python3
"""PreToolUse(Edit|Write|MultiEdit) 겹침 가드.

다른 kasaterm pane이 "지금" 같은 파일을 작업 중이면 이 편집을 deny로 막는다.
진실의 원천은 각 pane의 claude transcript(jsonl) — 별도 락 파일/데몬 없이
transcript를 직접 비교하므로 kasaterm 빌드 상태와 무관하게 동작한다.
kasaterm pane 밖($KASATERM_PANE_ID 없음)이면 no-op.

왜 transcript 비교인가: tell(프롬프트 주입)은 상대가 작업 중이면 턴이 끝나야
읽혀 실시간 조율에 못 쓴다. PreToolUse는 Edit 직전 동기 실행되고, 상대의
transcript는 상대가 바쁘든 말든 실시간으로 쌓이므로, 말 걸지 않고 즉시 판정한다.

"지금 작업 중" 판정 — 절대 시간 윈도우가 아니라 진행 상태로:
  - 상대가 그 파일 이후 다른 작업(어떤 tool이든)으로 넘어갔으면 → 손 뗌 → 통과
  - 상대가 그 파일 만지고 IDLE초 넘게 조용하면 → 끝/쉼 → 통과
  - 그 파일이 상대의 '마지막 활동'이고 아직 활발(IDLE 내)일 때만 → 차단
끝난 작업을 시간만으로 계속 막던 문제를 이 진행-상태 판정으로 해소한다.
"""
import sys, json, os, glob, subprocess
from datetime import datetime

# 상대가 그 파일 만진 뒤 이 초 넘게 조용하면 "손 뗌"으로 보고 통과시킨다.
IDLE = int(os.environ.get("KASATERM_CONFLICT_IDLE", "15"))
# 이 시간 넘게 활동 없는 transcript는 아예 스캔하지 않는다(싼 1차 필터).
STALE = int(os.environ.get("KASATERM_CONFLICT_WINDOW", "90"))
# transcript 꼬리에서 이만큼만 읽는다(큰 jsonl 전체 파싱 회피).
TAIL_BYTES = 300_000


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


def teammate_name(sid):
    """충돌 상대 세션 id → 같은 방 팀의 SendMessage 주소(agent 이름).

    shim 이름 규칙 = <로마자>-<sid 앞4자> 라서 인박스 파일명 꼬리로 역추적한다
    (bridge.rs 와 같은 매칭). 꼬리 충돌(스테일 인박스 누적)로 후보가 2개 이상이면
    오배달 대신 None — 호출부가 tell 안내로 폴백한다."""
    team = os.environ.get("KASATERM_TEAM")
    if not team or not sid:
        return None
    tail = "-" + sid[:4]
    d = os.path.expanduser("~/.claude/teams/" + team + "/inboxes")
    try:
        names = [f[:-5] for f in os.listdir(d) if f.endswith(".json")]
    except OSError:
        return None
    hits = [n for n in names if n.endswith(tail)]
    return hits[0] if len(hits) == 1 else None


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


def main():
    if not os.environ.get("KASATERM_PANE_ID"):
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

    # 빈 문자열을 realpath 하면 **cwd** 가 나와 어떤 jsonl 과도 안 맞는다 —
    # 그러면 아래 자기 제외가 통째로 무력해진다(빈 값이 틀린 값보다 위험한 자리).
    raw_tp = payload.get("transcript_path") or ""
    my_tp = os.path.realpath(raw_tp) if raw_tp else ""
    me = os.environ.get("KASATERM_PANE_ID")
    cwd = payload.get("cwd") or os.getcwd()
    enc = cwd.replace("/", "-").replace(".", "-")
    proj = os.path.expanduser("~/.claude/projects/" + enc)
    if not os.path.isdir(proj):
        return

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
        # 조율 채널은 SendMessage 우선(거노 07-17) — tell 은 상대가 작업 중이면 턴이
        # 끝나야 읽지만 teammate-message 는 작업 중에도 도착해 실시간 조율이 된다.
        # 주소 역추적이 안 되는 상대(비팀원 pane·detach 포크·꼬리 충돌)만 tell 폴백.
        addr = teammate_name(sid)
        if addr:
            coord = (
                f"① 같은 문제면 합류/분담 — SendMessage 도구로 to:\"{addr}\" 에게 "
                f"\"나도 {name} 작업 필요, 조율하자\" 처럼 의도를 보내세요(상대가 작업 중이어도 도착). "
            )
        else:
            tellref = pane or "<pane>"
            coord = f"① 같은 문제면 합류 → kasaterm-cli tell {tellref} \"나도 {name} 보는 중, 합칠까?\" "
        reason = (
            f"{who}이 {secs}초 전부터 '{name}'을(를) 작업 중이에요{doing}. "
            f"같은 파일 겹침을 막았어요. 조율하세요: "
            + coord
            + f"② 독립 작업이면 다른 파일부터. "
            f"③ 그 pane이 손 뗄 때까지(~{IDLE}초 조용 또는 다른 작업) 기다렸다 재시도."
        )
        print(
            json.dumps(
                {
                    "hookSpecificOutput": {
                        "hookEventName": "PreToolUse",
                        "permissionDecision": "deny",
                        "permissionDecisionReason": reason,
                    }
                },
                ensure_ascii=False,
            )
        )
        return


if __name__ == "__main__":
    main()
