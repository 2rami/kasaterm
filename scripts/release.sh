#!/usr/bin/env bash
# Build a signed .app, wrap it in a drag-to-install .dmg, and (optionally)
# publish it as a GitHub release.
#
# Usage:
#   scripts/release.sh v0.1.0            # build dist/kasaterm-v0.1.0.dmg only
#   scripts/release.sh v0.1.0 --publish  # also create the GitHub release
#
# The friend who downloads it must right-click → Open the first time
# (self-signed, so Gatekeeper shows "unidentified developer" once).
set -euo pipefail

VERSION="${1:?usage: release.sh <version> [--publish]  e.g. release.sh v0.1.0}"
PUBLISH=0
[[ "${2:-}" == "--publish" ]] && PUBLISH=1

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# 1. Build + sign the release .app.
bash scripts/build-app.sh

APP="dist/kasaterm.app"
DMG="dist/kasaterm-$VERSION.dmg"

# 2. Stage a DMG layout: the app next to an /Applications symlink so the
#    user just drags one onto the other.
STAGE="$(mktemp -d)"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"

# 3. Compressed DMG (UDZO).
rm -f "$DMG"
hdiutil create -volname "kasaterm" -srcfolder "$STAGE" -ov -format UDZO "$DMG" >/dev/null
rm -rf "$STAGE"
echo "built $DMG ($(du -h "$DMG" | cut -f1))"

if [[ "$PUBLISH" -eq 0 ]]; then
  echo "dmg only — re-run with --publish to create the GitHub release"
  exit 0
fi

# 4. Publish. Needs the tag's commit pushed already.
NOTES="$(cat <<EOF
kasaterm $VERSION — 한글 특화 GUI 터미널 + Claude 런처

## 설치
1. \`kasaterm-$VERSION.dmg\` 다운로드 → 열기
2. kasaterm을 Applications 폴더로 드래그
3. **처음 실행만**: Applications에서 kasaterm **우클릭 → 열기** → "열기" 확인
   (직접 만든 앱이라 macOS가 한 번 확인받아요. 이후엔 그냥 더블클릭)
4. 첫 실행 때 화면 녹화/접근 권한 물으면 허용

## 쓰는 법
앱 열면 바로 터미널. \`claude\` 치면 Claude Code 실행돼요.
EOF
)"

gh release create "$VERSION" "$DMG" \
  --title "kasaterm $VERSION" \
  --notes "$NOTES"
echo "published: $(gh release view "$VERSION" --json url -q .url)"

# 5. Sparkle appcast — dmg 를 EdDSA 서명(Keychain 개인키)해 appcast.xml 생성하고
#    docs/ 로 커밋·push 한다. docs = GitHub Pages(SUFeedURL). 설치된 앱들이 이 feed 를
#    보고 자동 업데이트한다. enclosure URL 은 방금 만든 GitHub release 자산을 가리킨다.
APPCAST_DIR="$(mktemp -d)"
cp "$DMG" "$APPCAST_DIR/"
"$ROOT/vendor/Sparkle/bin/generate_appcast" \
  --download-url-prefix "https://github.com/2rami/kasaterm/releases/download/$VERSION/" \
  --link "https://github.com/2rami/kasaterm" \
  "$APPCAST_DIR"
mkdir -p docs
cp "$APPCAST_DIR/appcast.xml" docs/appcast.xml
touch docs/.nojekyll  # Jekyll 이 .xml 을 건드리지 않게
rm -rf "$APPCAST_DIR"
git add docs/appcast.xml docs/.nojekyll
git commit -m "chore(release): appcast $VERSION" >/dev/null 2>&1 || true
git push
echo "appcast → docs/appcast.xml (Pages: https://2rami.github.io/kasaterm/appcast.xml)"
