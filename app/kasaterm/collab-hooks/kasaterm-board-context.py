#!/usr/bin/env python3
"""UserPromptSubmit hook: 매 턴 시작 시 board(다른 pane 활동) + inbox(내게 온
메시지)를 프롬프트에 주입.

모든 pane 이 자기 턴에 "다른 pane 이 뭐 하나"(board) 와 "나한테 온 메시지"(inbox)를
자동으로 본다(pull). 따로 Monitor·조회 없이 claude 턴이라는 자연스러운 시점에
한 번 당겨 컨텍스트로 넣는다. 둘 다 없으면(혼자+메시지 없음) 조용. pane 밖이면 no-op.

- board: `kasaterm-cli board` = 호출 시점 pull. 제목(ai-title)·상태·시킨 일만 간결히.
- inbox: kasacollab 의 messages.jsonl 에서 to==나·미읽을 띄우고 읽음 처리(본 것).
  답장은 claude 가 `kasacollab msg <상대> "..."` 로.
"""
import sys, os, json, subprocess

me = os.environ.get("KASATERM_PANE_ID")
if not me:
    sys.exit(0)
try:
    sys.stdin.read()  # payload 소비
except Exception:
    pass


def collab_dir():
    enc = os.getcwd().replace("/", "-").replace(".", "-")
    return os.path.join("/tmp/kasaterm-collab", enc)


def board_section():
    """다른 pane 활동. 형제 없으면 None."""
    cli = os.environ.get("KASATERM_CLI", "kasaterm-cli")
    try:
        out = subprocess.run([cli, "board"], capture_output=True, text=True, timeout=3).stdout
        board = json.loads(out)["result"]["board"]
    except Exception:
        return None
    sibs = [p for p in board if p.get("surface_id") != me]
    if not sibs:
        return None
    lines = []
    for p in sorted(sibs, key=lambda x: x.get("surface_id", "")):
        sid = p.get("surface_id", "?")
        st = p.get("status", "")
        title = (p.get("title") or "").strip() or "(제목 없음)"
        prompt = (p.get("last_prompt") or "").strip()
        line = f"  {sid} [{st}] {title}"
        if prompt:
            line += f" — 시킴: {prompt[:60]}"
        lines.append(line)
    return "[협업 보드] 같은 레포를 동시에 만지는 다른 pane:\n" + "\n".join(lines)


def inbox_section():
    """내게 온 미읽 메시지. 띄우면서 읽음 처리(턴에 실으면 본 것). 없으면 None."""
    p = os.path.join(collab_dir(), "messages.jsonl")
    if not os.path.exists(p):
        return None
    try:
        msgs = [json.loads(l) for l in open(p).read().splitlines() if l.strip()]
    except Exception:
        return None
    mine = [m for m in msgs if m.get("to") == me and not m.get("read")]
    if not mine:
        return None
    for m in mine:
        m["read"] = True
    try:
        with open(p, "w") as f:
            for m in msgs:
                f.write(json.dumps(m, ensure_ascii=False) + "\n")
    except OSError:
        pass
    lines = [f"  {m.get('from', '?')}: {m.get('text', '')}" for m in mine]
    return ("[받은 메시지] 나한테 온 말 (답장: kasacollab msg <상대> \"...\"):\n"
            + "\n".join(lines))


parts = [s for s in (board_section(), inbox_section()) if s]
if not parts:
    sys.exit(0)

ctx = ("\n".join(parts)
       + "\n(자세히: kasaterm-cli transcript %N / peek %N. 겹치면 피하거나 합류. "
       "급히 깨우기: kasaterm-cli tell %N \"메시지\".)")
print(json.dumps({
    "hookSpecificOutput": {"hookEventName": "UserPromptSubmit", "additionalContext": ctx}
}))
sys.exit(0)
