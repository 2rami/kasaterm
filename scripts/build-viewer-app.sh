#!/usr/bin/env bash
# Build the standalone Markdown viewer bundle without installing it or touching
# the main kasaterm app. The OpenHuman palette, icons and Galmuri fallback font
# are compiled into this same binary (`include_*` in gpu.rs), so duplicating the
# terminal CLI, web UI, updater framework or font files in Resources is needless.
set -euo pipefail

PROFILE=release
for arg in "$@"; do
  case "$arg" in
    --debug) PROFILE=debug ;;
    *) echo "unknown arg: $arg" >&2; exit 2 ;;
  esac
done

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

ICON="$ROOT/assets/AppIcon.icns"
[[ -f "$ICON" ]] || { echo "error: assets/AppIcon.icns missing" >&2; exit 1; }

VERSION="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"(.*)".*/\1/')"
if [[ "$PROFILE" == release ]]; then
  cargo build --release -p kasaterm --bin kasaterm
  BINDIR="$ROOT/target/release"
else
  cargo build -p kasaterm --bin kasaterm
  BINDIR="$ROOT/target/debug"
fi

SOURCE_BIN="$BINDIR/kasaterm"
[[ -x "$SOURCE_BIN" ]] || { echo "error: built kasaterm binary missing" >&2; exit 1; }

APP="$ROOT/dist/Kasaterm Viewer.app"
STAGE="$ROOT/dist/.Kasaterm Viewer.app.$$.new"
cleanup() { rm -rf "$STAGE"; }
trap cleanup EXIT HUP INT TERM

rm -rf "$STAGE"
mkdir -p "$STAGE/Contents/MacOS" "$STAGE/Contents/Resources"
cp "$SOURCE_BIN" "$STAGE/Contents/MacOS/kasaterm-viewer"
cp "$ICON" "$STAGE/Contents/Resources/AppIcon.icns"

cat > "$STAGE/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>Kasaterm Viewer</string>
    <key>CFBundleDisplayName</key>
    <string>Kasaterm Viewer</string>
    <key>CFBundleIdentifier</key>
    <string>com.kasa.kasaterm.viewer</string>
    <key>CFBundleVersion</key>
    <string>$VERSION</string>
    <key>CFBundleShortVersionString</key>
    <string>$VERSION</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleSignature</key>
    <string>????</string>
    <key>CFBundleExecutable</key>
    <string>kasaterm-viewer</string>
    <key>CFBundleIconFile</key>
    <string>AppIcon</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSPrincipalClass</key>
    <string>NSApplication</string>
    <key>LSMultipleInstancesProhibited</key>
    <true/>
    <key>LSSupportsOpeningDocumentsInPlace</key>
    <true/>
    <key>NSDesktopFolderUsageDescription</key>
    <string>Kasaterm Viewer가 데스크톱의 Markdown 문서를 엽니다.</string>
    <key>NSDocumentsFolderUsageDescription</key>
    <string>Kasaterm Viewer가 문서 폴더의 Markdown 문서를 엽니다.</string>
    <key>NSDownloadsFolderUsageDescription</key>
    <string>Kasaterm Viewer가 다운로드한 Markdown 문서를 엽니다.</string>
    <key>CFBundleDocumentTypes</key>
    <array>
      <dict>
        <key>CFBundleTypeName</key>
        <string>Markdown Document</string>
        <key>CFBundleTypeRole</key>
        <string>Viewer</string>
        <key>LSHandlerRank</key>
        <string>Alternate</string>
        <key>LSItemContentTypes</key>
        <array>
          <string>net.daringfireball.markdown</string>
        </array>
        <key>CFBundleTypeExtensions</key>
        <array>
          <string>md</string>
          <string>markdown</string>
          <string>mdown</string>
          <string>mkd</string>
        </array>
      </dict>
    </array>
</dict>
</plist>
PLIST

plutil -lint "$STAGE/Contents/Info.plist" >/dev/null

SIGN_ID="${KASATERM_SIGN_ID:-kasaterm-dev}"
if security find-identity -p codesigning 2>/dev/null | grep -q "$SIGN_ID"; then
  SIGN="$SIGN_ID"
  SIGN_MSG="signed with '$SIGN_ID'"
else
  SIGN="-"
  SIGN_MSG="signed ad-hoc"
fi
codesign --force --timestamp=none --sign "$SIGN" "$STAGE"
codesign --verify --strict --verbose=2 "$STAGE"

[[ "$(plutil -extract CFBundleIdentifier raw -o - "$STAGE/Contents/Info.plist")" == \
  com.kasa.kasaterm.viewer ]] || { echo "error: viewer bundle id mismatch" >&2; exit 1; }
[[ "$(plutil -extract CFBundleExecutable raw -o - "$STAGE/Contents/Info.plist")" == \
  kasaterm-viewer ]] || { echo "error: viewer executable mismatch" >&2; exit 1; }
for forbidden in \
  "$STAGE/Contents/MacOS/kasaterm-cli" \
  "$STAGE/Contents/MacOS/kasa-serve-web" \
  "$STAGE/Contents/Frameworks" \
  "$STAGE/Contents/Resources/Sparkle.framework" \
  "$STAGE/Contents/Resources/arona-ui" \
  "$STAGE/Contents/Resources/build-root"; do
  [[ ! -e "$forbidden" ]] || { echo "error: viewer contains $(basename "$forbidden")" >&2; exit 1; }
done

rm -rf "$APP"
mv "$STAGE" "$APP"
trap - EXIT HUP INT TERM
touch "$APP"

echo "built $APP ($PROFILE, $SIGN_MSG)"
