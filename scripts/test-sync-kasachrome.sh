#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
SYNC="$ROOT/scripts/sync-kasachrome.sh"
TMP=$(mktemp -d /tmp/kasachrome-sync-test.XXXXXX)
trap 'rm -rf "$TMP"' EXIT

ORIGIN="$TMP/origin.git"
SEED="$TMP/seed"
SRC="$TMP/src"
DST="$TMP/runtime"
STATE="$TMP/state"
LOG="$TMP/sync.log"

fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
assert_file() { [ "$(cat "$1")" = "$2" ] || fail "$1 content"; }
assert_log() { grep -Fq "$1" "$LOG" || fail "log missing: $1"; }

run_sync() {
  KASACHROME_SYNC_SRC="$SRC" \
  KASACHROME_SYNC_DST="$DST" \
  KASACHROME_SYNC_STATE="$STATE" \
  KASACHROME_SYNC_LOG="$LOG" \
  bash "$SYNC" "$@"
}

expect_fail() {
  local want="$1"
  shift
  set +e
  "$@"
  local got=$?
  set -e
  [ "$got" -eq "$want" ] || fail "expected rc=$want, got rc=$got"
}

git init --bare -q "$ORIGIN"
git init -q "$SEED"
git -C "$SEED" config user.name test
git -C "$SEED" config user.email test@example.invalid
mkdir -p "$SEED/kasachrome/extension" "$SEED/kasachrome/mcp"
printf 'base extension\n' > "$SEED/kasachrome/extension/content.js"
printf 'base server\n' > "$SEED/kasachrome/mcp/server.mjs"
printf 'remove me\n' > "$SEED/kasachrome/mcp/old.mjs"
printf 'dependency\n' > "$SEED/kasachrome/package.json"
git -C "$SEED" add .
git -C "$SEED" commit -qm baseline
git -C "$SEED" branch -M main
git -C "$SEED" remote add origin "$ORIGIN"
git -C "$SEED" push -qu origin main
git --git-dir="$ORIGIN" symbolic-ref HEAD refs/heads/main
git clone -q "$ORIGIN" "$SRC"
git -C "$SRC" config user.name test
git -C "$SRC" config user.email test@example.invalid
mkdir -p "$DST/node_modules/pkg"
rsync -a "$SRC/kasachrome/" "$DST/"
printf 'runtime dependency\n' > "$DST/node_modules/pkg/index.js"
printf 'runtime extra\n' > "$DST/runtime-only.txt"
printf 'operator backup\n' > "$DST/mcp/server.mjs.bak-operator"

# A missing baseline is a failure, and adoption refuses an unverified mismatch.
expect_fail 78 run_sync
printf 'manual\n' > "$DST/mcp/server.mjs"
expect_fail 78 run_sync --adopt
assert_file "$DST/mcp/server.mjs" manual
cp "$SRC/kasachrome/mcp/server.mjs" "$DST/mcp/server.mjs"
run_sync --adopt
[ -s "$STATE/deployed.manifest" ] || fail 'adopt manifest missing'
assert_file "$DST/node_modules/pkg/index.js" 'runtime dependency'

# No update is a real success and does not touch runtime-only dependencies.
run_sync
assert_log 'OK stage=done'
assert_file "$DST/node_modules/pkg/index.js" 'runtime dependency'

# Uncommitted source is never deployable, even when the tracked HEAD is clean.
printf 'not committed\n' > "$SRC/kasachrome/mcp/local-only.mjs"
expect_fail 65 run_sync
[ ! -e "$DST/mcp/local-only.mjs" ] || fail 'uncommitted source reached runtime'
assert_log 'FAIL stage=source-clean rc=65'
rm "$SRC/kasachrome/mcp/local-only.mjs"

# A fast-forward deploys source changes but never restarts Chrome by itself.
printf 'next extension\n' > "$SEED/kasachrome/extension/content.js"
printf 'new module\n' > "$SEED/kasachrome/mcp/new.mjs"
rm "$SEED/kasachrome/mcp/old.mjs"
git -C "$SEED" add .
git -C "$SEED" commit -qm upstream-one
git -C "$SEED" push -qu origin main
run_sync
assert_file "$DST/extension/content.js" 'next extension'
assert_file "$DST/mcp/new.mjs" 'new module'
[ ! -e "$DST/mcp/old.mjs" ] || fail 'removed managed file stayed in runtime'
assert_file "$DST/node_modules/pkg/index.js" 'runtime dependency'
assert_file "$DST/runtime-only.txt" 'runtime extra'
assert_file "$DST/mcp/server.mjs.bak-operator" 'operator backup'
assert_log 'NOTICE stage=reload extension changed; browser restart is required'
! grep -q 'launchctl' "$SYNC" || fail 'sync script must not restart services'

# Manual runtime edits block rsync even after source advances. Resolving the
# runtime edit lets the next 15-minute-style retry deploy the pending source.
printf 'manual runtime patch\n' > "$DST/mcp/server.mjs"
printf 'upstream pending\n' > "$SEED/kasachrome/mcp/pending.mjs"
git -C "$SEED" add .
git -C "$SEED" commit -qm upstream-two
git -C "$SEED" push -qu origin main
expect_fail 79 run_sync
assert_file "$DST/mcp/server.mjs" 'manual runtime patch'
[ ! -e "$DST/mcp/pending.mjs" ] || fail 'rsync ran over a dirty runtime'
assert_log 'FAIL stage=deploy-dirty rc=79'
cp "$SRC/kasachrome/mcp/server.mjs" "$DST/mcp/server.mjs"
run_sync
assert_file "$DST/mcp/pending.mjs" 'upstream pending'

# A source-side local commit is preserved. Even a conflicting upstream commit
# is fetched but never auto-merged and never reaches the runtime tree.
printf 'local source\n' > "$SRC/kasachrome/mcp/server.mjs"
git -C "$SRC" add .
git -C "$SRC" commit -qm local-divergence
printf 'remote source\n' > "$SEED/kasachrome/mcp/server.mjs"
git -C "$SEED" add .
git -C "$SEED" commit -qm remote-divergence
git -C "$SEED" push -qu origin main
expect_fail 65 run_sync
assert_file "$SRC/kasachrome/mcp/server.mjs" 'local source'
[ ! -e "$SRC/.git/MERGE_HEAD" ] || fail 'script left a merge in progress'
assert_log 'FAIL stage=divergence rc=65'
git -C "$SRC" reset -q --hard origin/main

# Fetch failures stay nonzero and the same clean job succeeds on a later run.
git -C "$SRC" remote set-url origin "$TMP/missing.git"
expect_fail 128 run_sync
assert_log 'FAIL stage=fetch rc=128'
git -C "$SRC" remote set-url origin "$ORIGIN"
run_sync
assert_file "$DST/mcp/server.mjs" 'remote source'

# A live lock is visible as temporary failure, not a fake successful no-op.
mkdir "$STATE/lock"
printf '%s\n' "$$" > "$STATE/lock/pid"
expect_fail 75 run_sync
assert_log "another sync is running (pid $$)"
rm "$STATE/lock/pid"
rmdir "$STATE/lock"
assert_log 'FAIL stage=lock rc=75'

# A just-created lock without a pid is not stolen: another process may be in
# the mkdir-to-pid write window.
mkdir "$STATE/lock"
expect_fail 75 run_sync
[ -d "$STATE/lock" ] || fail 'pid-less live-race lock was removed'
rmdir "$STATE/lock"

# A dead lock owner is reclaimed once, so a SIGKILL cannot disable every
# future 15-minute launchd run.
sleep 0 &
dead_pid=$!
wait "$dead_pid"
mkdir "$STATE/lock"
printf '%s\n' "$dead_pid" > "$STATE/lock/pid"
run_sync
[ ! -d "$STATE/lock" ] || fail 'reclaimed lock was not cleaned on exit'

# Preserve an rsync error code and leave the deployment marker untouched.
printf 'last upstream\n' > "$SEED/kasachrome/mcp/last.mjs"
git -C "$SEED" add .
git -C "$SEED" commit -qm upstream-three
git -C "$SEED" push -qu origin main
FAKE_RSYNC="$TMP/rsync-fail"
printf '#!/bin/sh\nexit 44\n' > "$FAKE_RSYNC"
chmod +x "$FAKE_RSYNC"
set +e
KASACHROME_SYNC_SRC="$SRC" \
KASACHROME_SYNC_DST="$DST" \
KASACHROME_SYNC_STATE="$STATE" \
KASACHROME_SYNC_LOG="$LOG" \
KASACHROME_SYNC_RSYNC="$FAKE_RSYNC" \
bash "$SYNC"
rc=$?
set -e
[ "$rc" -eq 44 ] || fail "rsync rc not preserved: $rc"
[ ! -e "$DST/mcp/last.mjs" ] || fail 'failed rsync changed runtime'
assert_log 'FAIL stage=rsync rc=44'
run_sync
assert_file "$DST/mcp/last.mjs" 'last upstream'

printf 'sync-kasachrome tests passed\n'
