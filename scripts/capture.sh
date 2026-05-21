#!/usr/bin/env bash
# Drive kasaterm's autocapture/autosplit through `open` so the .app's
# screen-recording TCC permission applies (a directly-run binary loses
# the bundle's permission). `open` strips shell env, so we hand the
# KASATERM_* vars off via a one-shot config file the app reads on launch.
#
# Usage:
#   scripts/capture.sh KASATERM_AUTOCAPTURE_MS=6000 KASATERM_AUTOCAPTURE_PATH=/tmp/shot.png
#   scripts/capture.sh KASATERM_AUTOSPLIT=hv KASATERM_AUTOSPLIT_MS=2000 \
#                      KASATERM_AUTOCAPTURE_MS=6000 KASATERM_AUTOCAPTURE_PATH=/tmp/shot.png
set -euo pipefail

APP="$HOME/Applications/kasaterm.app"
CONF="${TMPDIR:-/tmp}/kasaterm-capture.env"

if [[ ! -d "$APP" ]]; then
  echo "error: $APP not found — run scripts/build-app.sh --install first" >&2
  exit 1
fi

: > "$CONF"
for kv in "$@"; do
  echo "$kv" >> "$CONF"
done

pkill -f "kasaterm.app/Contents/MacOS/kasaterm" 2>/dev/null || true
pkill -f "tmux -C" 2>/dev/null || true
sleep 1
rm -rf /tmp/tmux-501 2>/dev/null || true

open -n "$APP"
echo "launched $APP via open"
echo "config ($CONF):"
sed 's/^/  /' "$CONF"
