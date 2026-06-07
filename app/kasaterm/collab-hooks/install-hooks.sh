#!/bin/bash
# kasaterm pane 협업 hook 배포기. 이 디렉터리가 hook 소스의 정본(canonical)이고,
# Claude Code 는 ~/.claude/hooks/<name> 경로에서 실행하므로 여기서 그 경로로
# 배포한다. settings.json 은 건드리지 않는다 — 이미 ~/.claude/hooks/<name> 를
# 가리키므로 cp/symlink 만으로 호환된다.
#
#   ./install-hooks.sh            친구 배포·재현용. 소스를 ~/.claude/hooks 로 복사.
#   ./install-hooks.sh --symlink  개발용. 심볼릭 링크 → 레포에서 고치면 즉시 반영.
set -euo pipefail

SRC="$(cd "$(dirname "$0")" && pwd)"
DST="$HOME/.claude/hooks"
MODE="copy"
[ "${1:-}" = "--symlink" ] && MODE="symlink"

mkdir -p "$DST"
for f in "$SRC"/kasaterm-*.sh "$SRC"/kasaterm-*.py "$SRC"/kasacollab.py; do
  [ -e "$f" ] || continue
  name="$(basename "$f")"
  if [ "$MODE" = "symlink" ]; then
    ln -sf "$f" "$DST/$name"
    echo "symlink: $name"
  else
    cp "$f" "$DST/$name"
    chmod +x "$DST/$name"
    echo "copy: $name"
  fi
done
echo "배포 완료 ($MODE) → $DST"
