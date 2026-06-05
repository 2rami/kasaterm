#!/bin/bash
# Stop / UserPromptSubmit hook: announce THIS pane's turn boundary to sibling
# panes so a claude elsewhere is woken with the news (no board polling).
#   arg $1 = start (UserPromptSubmit) | stop (Stop)
# No-op outside a kasaterm pane.
#
# Loop-breaker: notify lines are injected into sibling prompts with an `[알림]`
# prefix. That injection wakes the sibling, whose own UserPromptSubmit/Stop
# would re-fire this hook and ping-pong forever — so bail when the prompt that
# triggered this turn starts with `[알림]`.
[ -z "$KASATERM_PANE_ID" ] && exit 0
kind="${1:-stop}"
input=$(cat)
trigger=$(printf '%s' "$input" | python3 -c '
import sys, json
try:
    d = json.load(sys.stdin)
except Exception:
    print(""); raise SystemExit
# UserPromptSubmit carries the prompt directly; Stop only has the transcript
# path, so walk it for the last *real* user text (skip tool_result turns).
p = d.get("prompt")
if isinstance(p, str) and p.strip():
    print(p); raise SystemExit
last = ""
try:
    with open(d.get("transcript_path","")) as f:
        for line in f:
            try: o = json.loads(line)
            except Exception: continue
            if o.get("type") != "user": continue
            c = o.get("message", {}).get("content")
            if isinstance(c, str): txt = c
            elif isinstance(c, list):
                txt = " ".join(b.get("text","") for b in c
                               if isinstance(b, dict) and b.get("type") == "text")
            else: txt = ""
            if txt.strip(): last = txt
except Exception:
    pass
print(last)
' 2>/dev/null)
case "$trigger" in
  "[알림]"*) exit 0 ;;
esac
kasaterm-cli notify "$kind" >/dev/null 2>&1
exit 0
