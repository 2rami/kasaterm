#!/bin/bash
# Stop hook: claude in THIS pane finished its turn → fire a kasaterm
# work-complete notification. The host raises a desktop alert only when the
# pane isn't the one you're already looking at (background window or a sibling
# pane), and flashes the pane header either way. No-op outside a kasaterm pane.
#
# $KASATERM_PANE_ID is injected by pty-backend when the pane spawns; the Stop
# payload arrives on stdin (we don't need it yet, but drain it so claude isn't
# left writing to a closed pipe).
[ -z "$KASATERM_PANE_ID" ] && exit 0
cat >/dev/null 2>&1
dir="${PWD##*/}"
kasaterm-cli notify "✓ ${dir} — claude 완료" "작업을 마쳤어" >/dev/null 2>&1 || true
exit 0
