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


def pane_doing(fp):
    """board에서 이 파일(fp)을 만지는 중인 pane의 (surface_id, intent).

    deny 메시지에 '누구'와 '무슨 작업 중'을 채워, 막기를 조율(합류/회피)로
    끌어올리기 위함. board 조회 실패하면 (None, None)."""
    cli = os.environ.get("KASATERM_CLI", "kasaterm-cli")
    try:
        out = subprocess.run(
            [cli, "board"], capture_output=True, text=True, timeout=2
        ).stdout
        board = json.loads(out)["result"]["board"]
    except Exception:
        return None, None
    for p in board:
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

    my_tp = os.path.realpath(payload.get("transcript_path", "") or "")
    cwd = payload.get("cwd") or os.getcwd()
    enc = cwd.replace("/", "-").replace(".", "-")
    proj = os.path.expanduser("~/.claude/projects/" + enc)
    if not os.path.isdir(proj):
        return

    now = datetime.now().timestamp()
    for jf in glob.glob(os.path.join(proj, "*.jsonl")):
        # 내 transcript는 제외 — 내가 방금 만진 걸 충돌로 오인하면 안 된다.
        if os.path.realpath(jf) == my_tp:
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
        pane, intent = pane_doing(fp)
        who = pane or f"다른 pane({os.path.basename(jf).split('.')[0][:8]}…)"
        doing = f" (지금: {intent[:70]})" if intent else ""
        tellref = pane or "<pane>"
        reason = (
            f"{who}이 {secs}초 전부터 '{name}'을(를) 작업 중이에요{doing}. "
            f"같은 파일 겹침을 막았어요. 조율하세요: "
            f"① 같은 문제면 합류 → kasaterm-cli tell {tellref} \"나도 {name} 보는 중, 합칠까?\" "
            f"② 독립 작업이면 다른 파일부터. "
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
