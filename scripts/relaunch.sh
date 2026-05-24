#!/usr/bin/env bash
# Quit the running kasaterm app, (re)install the built bundle, relaunch it.
#
# Why this is safe (the install-while-running hazard, see CLAUDE.md / shim
# notes): kasaterm stages its tmux + cmux-compat helpers as *symlinks* into the
# app bundle ($TMPDIR/kasaterm-shim-<pid>/tmux -> .../Contents/MacOS/tmux).
# Overwriting the bundle while the app runs would dangle those symlinks mid
# rm/cp and skew helper versions. So we quit FIRST and wait for full
# termination, then install — the bundle is no longer in use.
#
# The quit is a graceful Quit AppleEvent (not SIGKILL) so kasaterm's `exiting`
# handler runs `save_session_state()`. That's what makes the relaunch restore
# the workspace (A3): pane layout, per-pane cwd, and `claude --resume`. A forced
# kill would skip the save and lose the session — so we never force-kill; if the
# app won't quit we abort and ask the user to Cmd+Q manually.
#
# Usage:
#   scripts/relaunch.sh             # build (release) + install + relaunch
#   scripts/relaunch.sh --no-build  # reuse the existing dist/kasaterm.app
set -euo pipefail

NO_BUILD=0
for arg in "$@"; do
  case "$arg" in
    --no-build) NO_BUILD=1 ;;
    *) echo "unknown arg: $arg" >&2; exit 2 ;;
  esac
done

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

APP_NAME="kasaterm"
INSTALLED="$HOME/Applications/kasaterm.app"
# Match ONLY an installed-app process, never a dev `cargo run`
# (target/debug|release/kasaterm). Both ~/Applications and /Applications paths
# contain this substring; a target/ build does not.
PATTERN="Applications/kasaterm.app/Contents/MacOS/kasaterm"

# macOS `pgrep -f` fails to match this full bundle path. Detect via ps + a bash
# builtin substring test — no external grep, so a user's grep->ugrep alias or
# any PATH shadowing can't break it, and there's no grep-self-match to filter.
app_running() {
  local line
  while IFS= read -r line; do
    [[ "$line" == *"$PATTERN"* ]] && return 0
  done < <(ps -Axww -o command=)
  return 1
}

# 1. Graceful quit → kasaterm `exiting()` → save_session_state() (A3 data).
if app_running; then
  echo "[relaunch] quitting running $APP_NAME (graceful, so session saves)…"
  osascript -e "tell application \"$APP_NAME\" to quit" 2>/dev/null || true

  # 2. Wait for full termination — the install must not race a live bundle.
  #    exiting() (the session save) completes before the process disappears,
  #    so once pgrep is empty the saved state is on disk.
  deadline=$(( $(date +%s) + 20 ))
  while app_running; do
    if [[ $(date +%s) -ge $deadline ]]; then
      echo "[relaunch] ERROR: $APP_NAME did not quit within 20s." >&2
      echo "[relaunch] Quit it manually (Cmd+Q) and re-run — not force-killing," >&2
      echo "[relaunch] because a kill skips the session save (you'd lose restore)." >&2
      exit 1
    fi
    sleep 0.5
  done
  echo "[relaunch] $APP_NAME quit cleanly; session saved."
else
  echo "[relaunch] $APP_NAME not running — proceeding to a fresh install."
fi

# 3. Install. The app is down now, so replacing the bundle is safe.
if [[ "$NO_BUILD" -eq 1 ]]; then
  if [[ ! -d dist/kasaterm.app ]]; then
    echo "[relaunch] ERROR: --no-build set but dist/kasaterm.app is missing." >&2
    echo "[relaunch] Run scripts/build-app.sh first, or drop --no-build." >&2
    exit 1
  fi
  echo "[relaunch] installing existing dist/kasaterm.app (no rebuild)…"
  mkdir -p "$HOME/Applications"
  rm -rf "$INSTALLED"
  cp -R dist/kasaterm.app "$INSTALLED"
  touch "$INSTALLED"
  echo "[relaunch] installed to $INSTALLED"
else
  echo "[relaunch] building + installing release bundle…"
  scripts/build-app.sh --install
fi

# 4. Relaunch. A3 restore brings panes/sessions (and claude --resume) back.
echo "[relaunch] launching ${INSTALLED}..."
open "$INSTALLED"
echo "[relaunch] done."
