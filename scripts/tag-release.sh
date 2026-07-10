#!/usr/bin/env bash
# 릴리스 시작 — 버전 bump 커밋 + 태그 push 만 한다. 나머지(msi + dmg 빌드,
# 릴리스 업로드, appcast 서명·커밋)는 GitHub Actions(release.yml)가 전부 처리.
#
# Usage:
#   scripts/tag-release.sh v0.1.7
#
# (scripts/release.sh 는 CI 없이 로컬에서 dmg 를 굽는 수동 폴백으로 남아 있다.)
set -euo pipefail

VERSION="${1:?usage: tag-release.sh vX.Y.Z  e.g. tag-release.sh v0.1.7}"
[[ "$VERSION" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "error: 버전은 vX.Y.Z 형식" >&2; exit 2; }
VER="${VERSION#v}"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

[[ -z "$(git status --porcelain)" ]] || { echo "error: 워킹트리가 깨끗하지 않음 — 먼저 커밋/스태시" >&2; exit 1; }
git tag -l "$VERSION" | grep -q . && { echo "error: 태그 $VERSION 이미 존재" >&2; exit 1; }

# workspace 버전 단일 소스([workspace.package] version) bump. 첫 매치만 치환.
perl -0pi -e "s/^version = \"[^\"]*\"/version = \"$VER\"/m" Cargo.toml
# Cargo.lock 의 워크스페이스 멤버 버전 동기화(의존성은 안 건드림).
cargo metadata --format-version 1 >/dev/null

git add Cargo.toml Cargo.lock
git commit -m "chore(release): v$VER"
git tag "$VERSION"
git push origin main "$VERSION"

echo ""
echo "→ $VERSION 태그 push 완료. 이후는 GitHub Actions 가 전부 처리한다:"
echo "  msi + dmg 빌드 → 릴리스 첨부 → appcast 2종 커밋"
echo "  진행 상황: https://github.com/2rami/kasaterm/actions"
