#!/usr/bin/env bash
# Pull the embedded kasachrome source and deploy it without overwriting a
# locally edited runtime tree. Intended for a 15-minute launchd job.
set -uo pipefail

SRC="${KASACHROME_SYNC_SRC:-$HOME/kasaterm-src}"
DST="${KASACHROME_SYNC_DST:-$HOME/kasachrome}"
STATE="${KASACHROME_SYNC_STATE:-$HOME/.local/state/kasachrome-sync}"
LOG="${KASACHROME_SYNC_LOG:-/tmp/kasachrome-sync.log}"
REMOTE="${KASACHROME_SYNC_REMOTE:-origin}"
BRANCH="${KASACHROME_SYNC_BRANCH:-main}"
RSYNC_BIN="${KASACHROME_SYNC_RSYNC:-rsync}"
MODE=sync

case "${1:-}" in
  "") ;;
  --adopt) MODE=adopt ;;
  --help)
    printf '%s\n' \
      'usage: sync-kasachrome.sh [--adopt]' \
      '  --adopt  record an already verified source/runtime match; never deploy'
    exit 0
    ;;
  *) printf 'unknown argument: %s\n' "$1" >&2; exit 2 ;;
esac

export PATH="$HOME/.local/bin:/opt/homebrew/bin:/usr/bin:/bin:${PATH:-}"
MANIFEST="$STATE/deployed.manifest"
DEPLOYED_HEAD="$STATE/deployed.head"
LOCK="$STATE/lock"
STAGE=preflight
LOCK_HELD=0
REPORTED=0
TMP_FILES=""

rotate_log() {
  [ -f "$LOG" ] || return 0
  local size
  size=$(wc -c < "$LOG" 2>/dev/null | tr -d ' ') || return 0
  if [ "${size:-0}" -gt 131072 ]; then
    tail -c 65536 "$LOG" > "$LOG.tmp" && mv "$LOG.tmp" "$LOG"
  fi
}

record() {
  local line="$(date -Iseconds) $*"
  printf '[sync-kasachrome] %s\n' "$line" >&2
  printf '%s\n' "$line" >> "$LOG" 2>/dev/null || true
  rotate_log
}

die() {
  local code="$1"
  shift
  record "FAIL stage=$STAGE rc=$code $*"
  REPORTED=1
  exit "$code"
}

on_exit() {
  local code="$1"
  if [ "$LOCK_HELD" -eq 1 ]; then
    lock_pid=$(sed -n '1p' "$LOCK/pid" 2>/dev/null || true)
    if [ "$lock_pid" = "$$" ]; then
      rm -f "$LOCK/pid"
      rmdir "$LOCK" 2>/dev/null || true
    fi
  fi
  local file
  for file in $TMP_FILES; do
    rm -f "$file"
  done
  if [ "$code" -ne 0 ] && [ "$REPORTED" -eq 0 ]; then
    record "FAIL stage=$STAGE rc=$code unexpected"
  fi
}
trap 'on_exit $?' EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

mkdir -p "$STATE" || die 73 "state-dir=$STATE"
mkdir -p "$(dirname "$LOG")" || die 73 "log-dir=$(dirname "$LOG")"
STAGE=lock
if ! mkdir "$LOCK" 2>/dev/null; then
  lock_pid=$(sed -n '1p' "$LOCK/pid" 2>/dev/null || true)
  case "$lock_pid" in
    ''|*[!0-9]*) die 75 "sync lock exists without a valid pid" ;;
  esac
  if kill -0 "$lock_pid" 2>/dev/null; then
    die 75 "another sync is running (pid $lock_pid)"
  fi
  lock_extra=""
  for lock_entry in "$LOCK"/* "$LOCK"/.[!.]* "$LOCK"/..?*; do
    if { [ -e "$lock_entry" ] || [ -L "$lock_entry" ]; } && [ "$lock_entry" != "$LOCK/pid" ]; then
      lock_extra="$lock_entry"
      break
    fi
  done
  [ -z "$lock_extra" ] || die 75 "stale sync lock contains unknown files"
  stale_lock="$STATE/lock.stale.$$"
  mv "$LOCK" "$stale_lock" 2>/dev/null || die 75 "stale sync lock changed while reclaiming"
  stale_pid=$(sed -n '1p' "$stale_lock/pid" 2>/dev/null || true)
  if [ "$stale_pid" != "$lock_pid" ]; then
    mv "$stale_lock" "$LOCK" 2>/dev/null || true
    die 75 "stale sync lock identity changed while reclaiming"
  fi
  rm -f "$stale_lock/pid"
  rmdir "$stale_lock" 2>/dev/null || die 75 "stale sync lock contains unknown files"
  mkdir "$LOCK" 2>/dev/null || die 75 "sync lock was claimed during retry"
fi
printf '%s\n' "$$" > "$LOCK/pid" || {
  rm -f "$LOCK/pid"
  rmdir "$LOCK" 2>/dev/null || true
  die 73 "cannot record sync lock owner"
}
LOCK_HELD=1

new_tmp() {
  NEW_TMP=$(mktemp "$STATE/manifest.XXXXXX") || return 1
  TMP_FILES="$TMP_FILES $NEW_TMP"
}

file_record() {
  local root="$1"
  local rel="$2"
  local path="$root/$rel"
  local mode hash
  if [ -L "$path" ]; then
    mode=link
    hash=$(readlink "$path" | shasum -a 256 | awk '{print $1}') || return 1
  elif [ -f "$path" ]; then
    mode=$(stat -f %Lp "$path" 2>/dev/null || stat -c %a "$path" 2>/dev/null) || return 1
    hash=$(shasum -a 256 "$path" | awk '{print $1}') || return 1
  else
    return 1
  fi
  printf '%s\t%s\t%s\n' "$mode" "$hash" "$rel"
}

manifest_tree() {
  local root="$1"
  local output="$2"
  [ -d "$root" ] || return 1
  (
    cd "$root" || exit 1
    find . \
      \( -path './node_modules' -o -path './.git' \) -prune -o \
      \( -type f -o -type l \) \
      ! -name '*.bak*' ! -name '*.before-*' ! -name '*.pre-sync-*' \
      -print |
      LC_ALL=C sort |
      while IFS= read -r item; do
        rel=${item#./}
        case "$rel" in
          *$'\t'*|*$'\n'*) exit 1 ;;
        esac
        file_record "$root" "$rel" || exit 1
      done
  ) > "$output"
}

verify_manifest() {
  local root="$1"
  local expected="$2"
  local mode hash rel actual
  while IFS=$'\t' read -r mode hash rel; do
    [ -n "$rel" ] || continue
    actual=$(file_record "$root" "$rel") || return 1
    [ "$actual" = "$mode"$'\t'"$hash"$'\t'"$rel" ] || return 1
  done < "$expected"
}

new_path_collides() {
  local source_manifest="$1"
  local old_manifest="$2"
  local mode hash rel actual
  while IFS=$'\t' read -r mode hash rel; do
    [ -n "$rel" ] || continue
    if ! awk -F '\t' -v path="$rel" '$3 == path { found=1 } END { exit !found }' "$old_manifest"; then
      if [ -e "$DST/$rel" ] || [ -L "$DST/$rel" ]; then
        actual=$(file_record "$DST" "$rel") || return 0
        [ "$actual" = "$mode"$'\t'"$hash"$'\t'"$rel" ] || return 0
      fi
    fi
  done < "$source_manifest"
  return 1
}

manifest_has_path() {
  local manifest="$1"
  local rel="$2"
  awk -F '\t' -v path="$rel" '$3 == path { found=1 } END { exit !found }' "$manifest"
}

remove_stale_managed() {
  local old_manifest="$1"
  local source_manifest="$2"
  local mode hash rel actual expected
  while IFS=$'\t' read -r mode hash rel; do
    [ -n "$rel" ] || continue
    manifest_has_path "$source_manifest" "$rel" && continue
    case "$rel" in
      /*|..|../*|*/../*|*/..) return 2 ;;
    esac
    expected="$mode"$'\t'"$hash"$'\t'"$rel"
    actual=$(file_record "$DST" "$rel") || return 1
    [ "$actual" = "$expected" ] || return 1
    rm -f -- "$DST/$rel" || return 1
  done < "$old_manifest"
}

STAGE=preflight
[ -d "$SRC/.git" ] || die 66 "source repo missing: $SRC"
[ -d "$SRC/kasachrome" ] || die 66 "source tree missing: $SRC/kasachrome"
[ -d "$DST" ] || die 66 "runtime tree missing: $DST"
cd "$SRC" || die 72 "cannot enter source repo"

STAGE=source-clean
[ -z "$(git status --porcelain --untracked-files=normal)" ] || \
  die 65 "source has uncommitted changes; commit them before deployment"
current_branch=$(git symbolic-ref --quiet --short HEAD 2>/dev/null) || \
  die 65 "source is on a detached HEAD"
[ "$current_branch" = "$BRANCH" ] || \
  die 65 "source branch is $current_branch, expected $BRANCH"

STAGE=fetch
git fetch --quiet "$REMOTE" "$BRANCH" || die $? "git fetch $REMOTE $BRANCH"
remote_ref="$REMOTE/$BRANCH"
counts=$(git rev-list --left-right --count "HEAD...$remote_ref" 2>/dev/null) || \
  die 65 "cannot compare HEAD with $remote_ref"
ahead=${counts%%[[:space:]]*}
behind=${counts##*[[:space:]]}
if [ "$ahead" -ne 0 ]; then
  STAGE=divergence
  die 65 "source diverged: ahead=$ahead behind=$behind; preserved without merge"
fi

before=$(git rev-parse HEAD 2>/dev/null) || die 65 "cannot read source HEAD"
if [ "$behind" -gt 0 ]; then
  STAGE=fast-forward
  git merge --quiet --ff-only "$remote_ref" || die $? "fast-forward refused"
fi
after=$(git rev-parse HEAD 2>/dev/null) || die 65 "cannot read updated HEAD"

NEW_TMP=""
new_tmp || die 73 "cannot create source manifest"
source_manifest="$NEW_TMP"
STAGE=manifest
manifest_tree "$SRC/kasachrome" "$source_manifest" || die 74 "cannot hash source tree"

if [ "$MODE" = adopt ]; then
  STAGE=adopt
  verify_manifest "$DST" "$source_manifest" || \
    die 78 "runtime differs from source; inspect and preserve it before --adopt"
  cp "$source_manifest" "$MANIFEST" || die 73 "cannot store adopted manifest"
  printf '%s\n' "$after" > "$DEPLOYED_HEAD" || die 73 "cannot store adopted revision"
  record "OK stage=adopt head=${after:0:7} runtime verified; no files deployed"
  exit 0
fi

STAGE=baseline
[ -s "$MANIFEST" ] || die 78 "no trusted runtime baseline; verify equality and run --adopt once"
[ -s "$DEPLOYED_HEAD" ] || die 78 "trusted runtime revision is missing; run --adopt once"

STAGE=deploy-dirty
verify_manifest "$DST" "$MANIFEST" || \
  die 79 "runtime changed since the last verified deployment; preserved without rsync"
new_path_collides "$source_manifest" "$MANIFEST" && \
  die 79 "a new source path would overwrite an untracked runtime file"

deployed=$(sed -n '1p' "$DEPLOYED_HEAD" 2>/dev/null) || die 78 "cannot read deployed revision"
if [ "$deployed" = "$after" ]; then
  record "OK stage=done head=${after:0:7} no source update"
  exit 0
fi

extension_changed=0
if git cat-file -e "$deployed^{commit}" 2>/dev/null && \
   ! git diff --quiet "$deployed" "$after" -- kasachrome/extension; then
  extension_changed=1
fi

STAGE=rsync
"$RSYNC_BIN" -a \
  --exclude node_modules --exclude .git \
  --exclude '*.bak*' --exclude '*.before-*' --exclude '*.pre-sync-*' \
  "$SRC/kasachrome/" "$DST/" || die $? "rsync failed; runtime baseline was not advanced"

STAGE=remove-stale
remove_stale_managed "$MANIFEST" "$source_manifest" ||
  die 79 "a previously managed stale path changed or was unsafe; preserved"

STAGE=verify
verify_manifest "$DST" "$source_manifest" || \
  die 74 "runtime verification failed after rsync"
cp "$source_manifest" "$MANIFEST" || die 73 "cannot store deployment manifest"
printf '%s\n' "$after" > "$DEPLOYED_HEAD" || die 73 "cannot store deployed revision"

record "OK stage=done ${deployed:0:7} -> ${after:0:7}"
if [ "$extension_changed" -eq 1 ]; then
  record "NOTICE stage=reload extension changed; browser restart is required"
fi
