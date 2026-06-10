#!/usr/bin/env bash
# Quit the running kasaterm app, (re)install the built bundle, relaunch it.
#
# Why this is safe (the install-while-running hazard, see CLAUDE.md / shim
# notes): kasaterm stages its tmux + kasaterm-cli helpers as *symlinks* into the
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

# Graceful quit + wait for full termination. Reused by the initial pre-install
# quit and the post-build "stale binary re-opened" guard. Never force-kills —
# a SIGKILL skips exiting()/save_session_state() and loses the restore. Returns
# non-zero (without exiting) if it doesn't go down in $1 seconds so callers can
# tailor the message.
quit_app() {
  local timeout="${1:-20}"
  osascript -e "tell application \"$APP_NAME\" to quit" 2>/dev/null || true
  local deadline=$(( $(date +%s) + timeout ))
  while app_running; do
    [[ $(date +%s) -ge $deadline ]] && return 1
    sleep 0.5
  done
  return 0
}

# 1. Graceful quit → kasaterm `exiting()` → save_session_state() (A3 data).
#    Steps 2-3 (wait + install) leave the OLD bundle openable for the whole
#    build window (tens of seconds). The post-build guard (step 4) cures a
#    stale re-open, but the cheapest prevention is to ask the user not to.
if app_running; then
  echo "[relaunch] quitting running $APP_NAME (graceful, so session saves)…"
  # 2. Wait for full termination — the install must not race a live bundle.
  #    exiting() (the session save) completes before the process disappears,
  #    so once it's gone the saved state is on disk.
  if ! quit_app 20; then
    echo "[relaunch] ERROR: $APP_NAME did not quit within 20s." >&2
    echo "[relaunch] Quit it manually (Cmd+Q) and re-run — not force-killing," >&2
    echo "[relaunch] because a kill skips the session save (you'd lose restore)." >&2
    exit 1
  fi
  echo "[relaunch] $APP_NAME quit cleanly; session saved."
  echo "[relaunch] NOTE: building now — do NOT launch kasaterm from the Dock/Finder" >&2
  echo "[relaunch]       until this finishes, or you'll run the stale binary." >&2
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

# 4. Relaunch + verify, auto-remediating a stale binary.
#
# 빌드(수십 초) 동안 사용자가 Dock 등으로 앱을 직접 열면 그 시점의 *옛* 번들이
# 실행되고, 아래 open 은 "이미 떠 있음"이라 그 인스턴스를 앞으로 가져올 뿐이라
# 옛 바이너리가 계속 돈다(2026-06-10 두 차례 실측 — lsof txt 가 .app.old 의
# 이미 unlink 된 inode). 옛 'WARNING 후 끝' 대신: 떠 있던 인스턴스를 graceful
# quit → open → 실행 inode == 디스크 inode 까지 검증, 불일치면 자동 재시도. 옛
# 번들은 install 이 unlink 했으니 재open 은 무조건 신 번들을 문다. 사람 개입 0.
disk_ino=$(stat -f %i "$INSTALLED/Contents/MacOS/kasaterm" 2>/dev/null)

# Echo the live process's running-binary inode (empty if not up yet).
running_inode() {
  local l pid
  pid=$(ps -Axww -o pid=,command= | while IFS= read -r l; do [[ "$l" == *"$PATTERN"* ]] && { echo "${l%% *}"; break; }; done)
  [[ -z "$pid" ]] && return 0
  lsof -p "$pid" 2>/dev/null | awk '/MacOS\/kasaterm$/ {print $(NF-1); exit}'
}

verified=0
for attempt in 1 2 3; do
  # Any instance already up at this point is a stale Dock re-open — quit it so
  # the next open() launches the freshly installed bundle off disk.
  if app_running; then
    echo "[relaunch] app already running (stale Dock re-open) — quitting first…"
    if ! quit_app 20; then
      echo "[relaunch] ERROR: stale instance won't quit — Cmd+Q it and re-run with --no-build." >&2
      exit 1
    fi
  fi
  echo "[relaunch] launching ${INSTALLED} (attempt ${attempt})…"
  open "$INSTALLED"

  run_ino=""
  for _ in $(seq 1 20); do
    sleep 0.5
    run_ino=$(running_inode)
    [[ -n "$run_ino" ]] && break
  done
  if [[ -z "$run_ino" ]]; then
    echo "[relaunch] WARNING: launched but no running process detected — investigate." >&2
    break
  fi
  if [[ -z "$disk_ino" || "$run_ino" == "$disk_ino" ]]; then
    echo "[relaunch] verified: running image matches installed binary (inode ${disk_ino:-unknown})."
    verified=1
    break
  fi
  echo "[relaunch] running inode $run_ino != installed $disk_ino — stale binary; remediating (attempt ${attempt})…" >&2
done
if [[ "$verified" -ne 1 && -n "${run_ino:-}" && -n "$disk_ino" && "$run_ino" != "$disk_ino" ]]; then
  echo "[relaunch] ERROR: still running a stale binary after 3 attempts (inode $run_ino != $disk_ino)." >&2
  echo "[relaunch] Cmd+Q kasaterm fully, then re-run with --no-build." >&2
  exit 1
fi
echo "[relaunch] done."
