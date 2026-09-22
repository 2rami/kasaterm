#!/usr/bin/env bash
# Build KasaLite — the terminal-only pinned bundle. Same kasaterm binary, renamed
# `kasaterm-lite` so `ViewerLaunch::detect` boots it in lite mode: no characters,
# board, HTTP server, pet, webview or self-install, and its config/socket live in
# ~/.config/kasaterm-lite. It never touches the main app or its install path.
#
#   scripts/build-lite-app.sh [--debug] [--install]
#
# --install copies the result to ~/Applications/KasaLite.app once. Lite is meant
# to be baked once and left alone — that is the whole point of it.
set -euo pipefail

PROFILE=release
INSTALL=0
for arg in "$@"; do
  case "$arg" in
    --debug) PROFILE=debug ;;
    --install) INSTALL=1 ;;
    *) echo "unknown arg: $arg" >&2; exit 2 ;;
  esac
done

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

ICON="$ROOT/assets/LiteIcon.icns"
[[ -f "$ICON" ]] || { echo "error: assets/LiteIcon.icns missing" >&2; exit 1; }

VERSION="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"(.*)".*/\1/')"
if [[ "$PROFILE" == release ]]; then
  cargo build --release -p kasaterm --bin kasaterm -p kasa-socket --bin kasaterm-cli
  BINDIR="$ROOT/target/release"
else
  cargo build -p kasaterm --bin kasaterm -p kasa-socket --bin kasaterm-cli
  BINDIR="$ROOT/target/debug"
fi

SOURCE_BIN="$BINDIR/kasaterm"
CLI_BIN="$BINDIR/kasaterm-cli"
[[ -x "$SOURCE_BIN" ]] || { echo "error: built kasaterm binary missing" >&2; exit 1; }
[[ -x "$CLI_BIN" ]] || { echo "error: built kasaterm-cli binary missing" >&2; exit 1; }

APP="$ROOT/dist/KasaLite.app"
STAGE="$ROOT/dist/.KasaLite.app.$$.new"
cleanup() { rm -rf "$STAGE"; }
trap cleanup EXIT HUP INT TERM

rm -rf "$STAGE"
mkdir -p "$STAGE/Contents/MacOS" "$STAGE/Contents/Resources"
cp "$SOURCE_BIN" "$STAGE/Contents/MacOS/kasaterm-lite"
# kasaterm-cli sits next to the executable: `locate_cmux_compat_binary` looks
# there first, so the minimal shim stages it onto every pane's PATH.
cp "$CLI_BIN" "$STAGE/Contents/MacOS/kasaterm-cli"
cp "$ICON" "$STAGE/Contents/Resources/LiteIcon.icns"

cat > "$STAGE/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>KasaLite</string>
    <key>CFBundleDisplayName</key>
    <string>카사라이트</string>
    <key>CFBundleIdentifier</key>
    <string>com.kasa.kasaterm.lite</string>
    <key>CFBundleVersion</key>
    <string>$VERSION</string>
    <key>CFBundleShortVersionString</key>
    <string>$VERSION</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleSignature</key>
    <string>????</string>
    <key>CFBundleExecutable</key>
    <string>kasaterm-lite</string>
    <key>CFBundleIconFile</key>
    <string>LiteIcon</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSPrincipalClass</key>
    <string>NSApplication</string>
    <key>LSMultipleInstancesProhibited</key>
    <true/>
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
# kasaterm-cli is a separate executable the app signature does not cover.
codesign --force --timestamp=none --sign "$SIGN" "$STAGE/Contents/MacOS/kasaterm-cli"
codesign --force --timestamp=none --sign "$SIGN" "$STAGE"
codesign --verify --strict --verbose=2 "$STAGE"

[[ "$(plutil -extract CFBundleIdentifier raw -o - "$STAGE/Contents/Info.plist")" == \
  com.kasa.kasaterm.lite ]] || { echo "error: lite bundle id mismatch" >&2; exit 1; }
[[ "$(plutil -extract CFBundleExecutable raw -o - "$STAGE/Contents/Info.plist")" == \
  kasaterm-lite ]] || { echo "error: lite executable mismatch" >&2; exit 1; }
[[ -x "$STAGE/Contents/MacOS/kasaterm-cli" ]] || { echo "error: kasaterm-cli missing" >&2; exit 1; }
for forbidden in \
  "$STAGE/Contents/MacOS/kasapet" \
  "$STAGE/Contents/MacOS/kasa-serve-web" \
  "$STAGE/Contents/Frameworks" \
  "$STAGE/Contents/Resources/Sparkle.framework" \
  "$STAGE/Contents/Resources/arona-ui" \
  "$STAGE/Contents/Resources/build-root" \
  "$STAGE/Contents/Resources/collab-hooks"; do
  [[ ! -e "$forbidden" ]] || { echo "error: lite contains $(basename "$forbidden")" >&2; exit 1; }
done

rm -rf "$APP"
mv "$STAGE" "$APP"
trap - EXIT HUP INT TERM
touch "$APP"

echo "built $APP ($PROFILE, $SIGN_MSG)"

if [[ "$INSTALL" == 1 ]]; then
  DEST="$HOME/Applications/KasaLite.app"
  if pgrep -f "$DEST/Contents/MacOS/kasaterm-lite" >/dev/null 2>&1; then
    echo "error: $DEST is running — quit it first, then rerun with --install" >&2
    exit 1
  fi
  mkdir -p "$HOME/Applications"
  rm -rf "$DEST"
  cp -R "$APP" "$DEST"
  touch "$DEST"
  echo "installed $DEST"
fi
