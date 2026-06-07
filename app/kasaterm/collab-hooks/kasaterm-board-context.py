#!/usr/bin/env python3
"""UserPromptSubmit hook: 매 턴 시작 시 board(다른 pane 활동)를 프롬프트에 주입.

모든 pane 이 자기 턴에 "다른 pane 이 뭐 하나"를 자동으로 본다(pull). 따로
Monitor 를 걸 필요 없이, claude 턴이라는 자연스러운 시점에 board 를 한 번 당겨
컨텍스트로 넣는다. 혼자면(다른 pane 없음) 조용. pane 밖이면 no-op.

board 는 `kasaterm-cli board` = 호출 시점 pull(각 pane transcript tail). 여기선
제목(ai-title)·상태·시킨 일(last-prompt)만 간결히 — 자세한 답변/화면은 claude 가
필요할 때 `kasaterm-cli transcript %N` / `peek %N` 로 직접 본다.
"""
import sys, os, json, subprocess

me = os.environ.get("KASATERM_PANE_ID")
if not me:
    sys.exit(0)
try:
    sys.stdin.read()  # payload 소비
except Exception:
    pass

cli = os.environ.get("KASATERM_CLI", "kasaterm-cli")
try:
    out = subprocess.run([cli, "board"], capture_output=True, text=True, timeout=3).stdout
    board = json.loads(out)["result"]["board"]
except Exception:
    sys.exit(0)

sibs = [p for p in board if p.get("surface_id") != me]
if not sibs:
    sys.exit(0)  # 혼자면 조용

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

ctx = (
    "[협업 보드] 같은 레포를 동시에 만지는 다른 pane:\n"
    + "\n".join(lines)
    + "\n(자세히: kasaterm-cli transcript %N / peek %N. 겹치면 피하거나 합류. "
    "급히 깨우기: kasaterm-cli tell %N \"메시지\".)"
)
print(json.dumps({
    "hookSpecificOutput": {"hookEventName": "UserPromptSubmit", "additionalContext": ctx}
}))
sys.exit(0)
