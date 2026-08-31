#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
사용법:
  scripts/rename-repo-to-kasaterm.sh \
    --source /absolute/path/tmuxify \
    --target /absolute/path/kasaterm [--apply]

기본은 dry-run입니다. 실제 이동은 kasaterm·Claude Code·Codex를 모두 닫은 뒤,
이전 폴더의 바깥에서 --apply를 붙여 실행하세요.
EOF
}

die() {
  printf '오류: %s\n' "$*" >&2
  exit 1
}

canonical_slot() {
  local raw=$1 parent name
  [[ "$raw" = /* ]] || die "source와 target은 절대경로여야 합니다: $raw"
  parent=$(dirname "$raw")
  name=$(basename "$raw")
  [[ -d "$parent" ]] || die "상위 폴더가 없습니다: $parent"
  (cd "$parent" && printf '%s/%s\n' "$(pwd -P)" "$name")
}

device_id() {
  if stat -f '%d' "$1" >/dev/null 2>&1; then
    stat -f '%d' "$1"
  else
    stat -c '%d' "$1"
  fi
}

is_under() {
  [[ "$1" = "$2" || "$1" = "$2"/* ]]
}

valid_repo() {
  [[ -d "$1/.git" && -f "$1/Cargo.toml" && -d "$1/app/kasaterm" ]]
}

process_cwd() {
  command -v lsof >/dev/null 2>&1 || return 0
  lsof -a -p "$1" -d cwd -Fn 2>/dev/null | sed -n 's/^n//p' | head -n 1
}

assert_agents_stopped() {
  if pgrep -x kasaterm >/dev/null 2>&1; then
    die "kasaterm이 실행 중입니다. 앱을 정상 종료해 session/window 상태를 먼저 저장하세요."
  fi
  local command pid cwd
  for command in claude codex; do
    if pgrep -x "$command" >/dev/null 2>&1 && ! command -v lsof >/dev/null 2>&1; then
      die "$command 작업 위치를 확인하려면 lsof가 필요합니다"
    fi
    for pid in $(pgrep -x "$command" 2>/dev/null || true); do
      cwd=$(process_cwd "$pid")
      if [[ -n "$cwd" ]] && is_under "$cwd" "$SOURCE"; then
        die "$command 프로세스($pid)가 이전 폴더를 사용 중입니다: $cwd"
      fi
    done
  done
}

SOURCE_RAW=
TARGET_RAW=
MIGRATION_HOME=${HOME:?HOME이 필요합니다}
BACKUP_RAW=
APPLY=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --source) [[ $# -ge 2 ]] || die "--source 값이 없습니다"; SOURCE_RAW=$2; shift 2 ;;
    --target) [[ $# -ge 2 ]] || die "--target 값이 없습니다"; TARGET_RAW=$2; shift 2 ;;
    --home) [[ $# -ge 2 ]] || die "--home 값이 없습니다"; MIGRATION_HOME=$2; shift 2 ;;
    --backup-dir) [[ $# -ge 2 ]] || die "--backup-dir 값이 없습니다"; BACKUP_RAW=$2; shift 2 ;;
    --apply) APPLY=1; shift ;;
    --dry-run) APPLY=0; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "알 수 없는 옵션입니다: $1" ;;
  esac
done

[[ -n "$SOURCE_RAW" && -n "$TARGET_RAW" ]] || { usage; exit 2; }
SOURCE=$(canonical_slot "$SOURCE_RAW")
TARGET=$(canonical_slot "$TARGET_RAW")
MIGRATION_HOME=$(canonical_slot "$MIGRATION_HOME")

[[ "$SOURCE" != "$TARGET" ]] || die "source와 target이 같습니다"
[[ $(basename "$SOURCE") = tmuxify ]] || die "source 폴더명은 tmuxify여야 합니다: $SOURCE"
[[ $(basename "$TARGET") = kasaterm ]] || die "target 폴더명은 kasaterm이어야 합니다: $TARGET"
[[ $(dirname "$SOURCE") = "$(dirname "$TARGET")" ]] || die "같은 상위 폴더 안에서만 이름을 바꿀 수 있습니다"
[[ ! -L "$SOURCE" && ! -L "$TARGET" ]] || die "심볼릭 링크는 source/target으로 쓸 수 없습니다"

if [[ -e "$SOURCE" && -e "$TARGET" ]]; then
  die "source와 target이 둘 다 있습니다. 자동 병합하지 않습니다"
elif [[ -d "$SOURCE" ]]; then
  valid_repo "$SOURCE" || die "source가 kasaterm 주 저장소가 아닙니다: $SOURCE"
  REPO=$SOURCE
  NEEDS_MOVE=1
elif [[ -d "$TARGET" ]]; then
  valid_repo "$TARGET" || die "target이 이동된 kasaterm 주 저장소가 아닙니다: $TARGET"
  REPO=$TARGET
  NEEDS_MOVE=0
else
  die "source와 target 어느 쪽에도 저장소가 없습니다"
fi

TOP=$(git -C "$REPO" rev-parse --show-toplevel 2>/dev/null || true)
[[ "$TOP" = "$REPO" ]] || die "주 worktree의 루트에서만 실행할 수 있습니다: $REPO"
if [[ $NEEDS_MOVE -eq 1 ]]; then
  [[ $(device_id "$SOURCE") = "$(device_id "$(dirname "$TARGET")")" ]] || die "다른 디스크로는 이동하지 않습니다"
fi

if [[ -z "$BACKUP_RAW" ]]; then
  STAMP=$(date +%Y%m%d-%H%M%S)
  BACKUP_RAW="$MIGRATION_HOME/.config/kasaterm/migrations/repo-path-$STAMP"
fi
BACKUP=$(python3 -c 'import os,sys; print(os.path.abspath(sys.argv[1]))' "$BACKUP_RAW")
is_under "$BACKUP" "$SOURCE" && die "백업 폴더를 source 안에 둘 수 없습니다"
is_under "$BACKUP" "$TARGET" && die "백업 폴더를 target 안에 둘 수 없습니다"

HELPER="$REPO/scripts/migrate-repo-path.py"
[[ -f "$HELPER" ]] || die "상태 이전 도우미가 없습니다: $HELPER"

printf 'source: %s\ntarget: %s\nhome:   %s\nmode:   %s\n' \
  "$SOURCE" "$TARGET" "$MIGRATION_HOME" "$([[ $APPLY -eq 1 ]] && printf apply || printf dry-run)"
python3 "$HELPER" \
  --source "$SOURCE" --target "$TARGET" --home "$MIGRATION_HOME" --backup-dir "$BACKUP"

if [[ $APPLY -eq 0 ]]; then
  printf '\ndry-run만 끝났습니다. 파일·설정·폴더는 바꾸지 않았습니다.\n'
  printf '실행하려면 저장소 바깥에서 같은 명령에 --apply를 붙이세요.\n'
  exit 0
fi

if [[ $NEEDS_MOVE -eq 1 ]] && is_under "$(pwd -P)" "$SOURCE"; then
  die "현재 셸이 source 안에 있습니다. cd $(dirname "$SOURCE") 후 다시 실행하세요"
fi
if [[ "$MIGRATION_HOME" = "$(canonical_slot "$HOME")" ]]; then
  assert_agents_stopped
fi
[[ ! -e "$BACKUP" ]] || die "백업 폴더가 이미 있습니다: $BACKUP"
mkdir -p "$BACKUP/git-linked"

WORKTREES_FILE="$BACKUP/worktrees-before.txt"
git -C "$REPO" worktree list --porcelain > "$WORKTREES_FILE"
if [[ -d "$REPO/.git/worktrees" ]]; then
  cp -pR "$REPO/.git/worktrees" "$BACKUP/git-main-worktrees"
fi
cp -p "$REPO/.git/config" "$BACKUP/git-config"

WORKTREES=()
while IFS= read -r line; do
  case "$line" in
    worktree\ *) WORKTREES+=("${line#worktree }") ;;
  esac
done < "$WORKTREES_FILE"

index=0
for worktree in "${WORKTREES[@]}"; do
  [[ "$worktree" = "$REPO" ]] && continue
  if [[ -f "$worktree/.git" ]]; then
    printf '%s\n' "$worktree" > "$BACKUP/git-linked/$index.path"
    cp -p "$worktree/.git" "$BACKUP/git-linked/$index.git"
    index=$((index + 1))
  fi
done

if [[ $NEEDS_MOVE -eq 1 ]]; then
  mv "$SOURCE" "$TARGET"
  printf '저장소 이동: %s -> %s\n' "$SOURCE" "$TARGET"
fi

LINKED=()
for worktree in "${WORKTREES[@]}"; do
  [[ "$worktree" = "$SOURCE" || "$worktree" = "$TARGET" ]] && continue
  if is_under "$worktree" "$SOURCE"; then
    worktree="$TARGET${worktree#"$SOURCE"}"
  fi
  LINKED+=("$worktree")
done

git -C "$TARGET" worktree repair "${LINKED[@]}"
[[ $(git -C "$TARGET" rev-parse --show-toplevel) = "$TARGET" ]] || die "주 worktree 검증에 실패했습니다"
AFTER="$BACKUP/worktrees-after.txt"
git -C "$TARGET" worktree list --porcelain > "$AFTER"
grep -Fqx "worktree $TARGET" "$AFTER" || die "이동된 주 worktree가 목록에 없습니다"
for worktree in "${LINKED[@]}"; do
  [[ -d "$worktree" ]] || die "연결 worktree가 없습니다: $worktree"
  [[ $(git -C "$worktree" rev-parse --show-toplevel) = "$worktree" ]] || die "연결 worktree가 깨졌습니다: $worktree"
  grep -Fqx "worktree $worktree" "$AFTER" || die "연결 worktree가 목록에서 빠졌습니다: $worktree"
  if [[ -f "$worktree/.git" ]] && grep -Fq "$SOURCE" "$worktree/.git"; then
    die "연결 worktree가 아직 이전 gitdir를 가리킵니다: $worktree"
  fi
done
printf 'git worktree repair 및 연결 검증 완료: 연결 %d개\n' "${#LINKED[@]}"

python3 "$TARGET/scripts/migrate-repo-path.py" \
  --source "$SOURCE" --target "$TARGET" --home "$MIGRATION_HOME" --backup-dir "$BACKUP" --apply
python3 "$TARGET/scripts/migrate-repo-path.py" \
  --source "$SOURCE" --target "$TARGET" --home "$MIGRATION_HOME" --backup-dir "$BACKUP" --check-clean

[[ ! -e "$SOURCE" && ! -L "$SOURCE" ]] || die "이전 경로가 남았습니다. 영구 호환 symlink는 만들지 않습니다"
printf '\n완료: %s\n백업: %s\n' "$TARGET" "$BACKUP"
