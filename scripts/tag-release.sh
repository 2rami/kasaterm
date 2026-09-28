#!/usr/bin/env bash
# stable 릴리스 시작 — 버전·채널 bump 커밋 + 태그 push. Windows는 CI가 굽고,
# MAC_ARTIFACT=local이면 fastpatch가 승인받아 서명·공증한 dmg를 올려야 CI가 피드를 완성한다.
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

# Cargo.toml(버전 bump 자체, 중단된 실행의 잔여 허용) 외의 변경이 있으면 거부 —
# 릴리스 커밋에 무관한 작업이 섞이는 것을 막는다.
DIRTY="$(git status --porcelain | grep -v ' Cargo.toml$' || true)"
[[ -z "$DIRTY" ]] || { echo "error: 워킹트리에 Cargo.toml 외 변경 있음 — 먼저 커밋/스태시:" >&2; echo "$DIRTY" >&2; exit 1; }
git tag -l "$VERSION" | grep -q . && { echo "error: 태그 $VERSION 이미 존재" >&2; exit 1; }

# workspace 버전 단일 소스([workspace.package] version) bump. 첫 매치만 치환.
perl -0pi -e "s/^version = \"[^\"]*\"/version = \"$VER\"/m" Cargo.toml
# Cargo.lock(이 레포는 gitignore, 로컬 전용) 워크스페이스 멤버 버전 동기화.
cargo metadata --format-version 1 >/dev/null

# preview manifest가 main에 남아 있어도 수동 stable bump는 채널을 새로 고정한다.
python3 - "$VERSION" <<'PY'
import json
from pathlib import Path
import subprocess
import sys
from tools.release.common import CHANNEL_MANIFEST, Refused, channel_manifest, validate_channel_manifest

tag = sys.argv[1]
head = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
path = Path(CHANNEL_MANIFEST)
same_bump = False
if path.exists() and not subprocess.check_output(["git", "status", "--porcelain", "Cargo.toml"], text=True).strip():
    try:
        parent = subprocess.check_output(["git", "rev-parse", "HEAD^"], text=True).strip()
        channel, platforms = validate_channel_manifest(json.loads(path.read_text()), tag, parent)
        same_bump = channel == "stable" and platforms == ["macos", "windows"]
    except (Refused, ValueError, subprocess.CalledProcessError):
        pass
if not same_bump:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(channel_manifest("stable", tag, head), sort_keys=True, indent=2) + "\n")
PY
if [[ -n "$(git status --porcelain Cargo.toml .github/release-channel.json)" ]]; then
  git add Cargo.toml .github/release-channel.json
  git commit -m "chore(release): v$VER"
fi
git tag "$VERSION"
git push --atomic origin main "$VERSION"

echo ""
echo "→ $VERSION stable 태그 push 완료. CI가 Windows를 굽고 mac dmg를 검증한다."
echo "  MAC_ARTIFACT=local이면 승인·서명·공증된 로컬 dmg 업로드가 필요하다."
echo "  두 산출물 검증 뒤 stable appcast 2종 커밋"
echo "  진행 상황: https://github.com/2rami/kasaterm/actions"
