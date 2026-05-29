#!/usr/bin/env bash
# Build a macOS .app bundle around the kasaterm binary.
#
# Output: dist/kasaterm.app/Contents/{Info.plist, MacOS/kasaterm,
#                                     Resources/AppIcon.icns}
#
# Usage:
#   scripts/build-app.sh            # release build, dist/kasaterm.app
#   scripts/build-app.sh --debug    # debug build (faster, larger)
#   scripts/build-app.sh --install  # also copy into ~/Applications

set -euo pipefail

PROFILE="release"
INSTALL=0
for arg in "$@"; do
  case "$arg" in
    --debug)   PROFILE="debug" ;;
    --install) INSTALL=1 ;;
    *) echo "unknown arg: $arg" >&2; exit 2 ;;
  esac
done

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

if [[ ! -f assets/AppIcon.icns ]]; then
  echo "error: assets/AppIcon.icns missing — run the iconset builder first" >&2
  exit 1
fi

# Build the binaries. Besides kasaterm we need the tmux shim ("tmux")
# and kasaterm-cli: claude code's teammate mode shells out to `tmux
# split-window` / `send-keys`, which our shim rewrites into kasaterm-cli
# socket calls. Without bundling these next to kasaterm, a packaged .app
# can't find them (install_tmux_shim / locate_* look beside the exe) and
# teammate splits silently fall back to the it2/real-tmux path.
if [[ "$PROFILE" == "release" ]]; then
  cargo build --release -p kasaterm -p tmux-shim -p agent-socket
  BINDIR="target/release"
else
  cargo build -p kasaterm -p tmux-shim -p agent-socket
  BINDIR="target/debug"
fi

APP="dist/kasaterm.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

cp "$BINDIR/kasaterm" "$APP/Contents/MacOS/kasaterm"
cp "$BINDIR/tmux" "$APP/Contents/MacOS/tmux"
cp "$BINDIR/kasaterm-cli" "$APP/Contents/MacOS/kasaterm-cli"
cp assets/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"

# Minimal Info.plist. Bundle id namespaced under the project root so
# Launchpad / Spotlight key the icon to this binary specifically.
cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>kasaterm</string>
    <key>CFBundleDisplayName</key>
    <string>kasaterm</string>
    <key>CFBundleIdentifier</key>
    <string>com.kasa.kasaterm</string>
    <key>CFBundleVersion</key>
    <string>0.1.0</string>
    <key>CFBundleShortVersionString</key>
    <string>0.1.0</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleExecutable</key>
    <string>kasaterm</string>
    <key>CFBundleIconFile</key>
    <string>AppIcon</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSPrincipalClass</key>
    <string>NSApplication</string>
    <key>NSRequiresAquaSystemAppearance</key>
    <false/>
</dict>
</plist>
PLIST

# Sign with a stable self-signed identity when one exists, so macOS keeps
# TCC permissions (screen recording / automation) across rebuilds instead
# of re-prompting every time. Ad-hoc (`-`) changes the code hash on every
# build, which macOS treats as a brand-new app. Create the identity once:
#   Keychain Access → Certificate Assistant → Create a Certificate
#   Name: kasaterm-dev, Type: Self-Signed Root, Certificate Type: Code Signing
SIGN_ID="kasaterm-dev"
# No -v: a self-signed cert is valid-but-untrusted (CSSMERR_TP_NOT_TRUSTED),
# which -v filters out. codesign still signs with it, and TCC keys
# permissions off the signing identity, so untrusted is fine for local use.
if security find-identity -p codesigning 2>/dev/null | grep -q "$SIGN_ID"; then
  codesign --force --sign "$SIGN_ID" --deep "$APP" 2>/dev/null \
    && echo "signed with '$SIGN_ID' — TCC permissions persist across rebuilds" \
    || echo "warning: signing with '$SIGN_ID' failed; app left unsigned"
else
  # Fall back to ad-hoc so Gatekeeper still accepts a Finder launch.
  codesign --force --sign - --deep "$APP" 2>/dev/null || true
  echo "signed ad-hoc — create a '$SIGN_ID' code-signing cert to stop the permission re-prompts"
fi

# Bust the icon cache so the new .icns shows immediately in Finder /
# Dock instead of waiting for macOS to notice on its own.
touch "$APP"

echo "built $APP ($PROFILE)"

if [[ "$INSTALL" -eq 1 ]]; then
  mkdir -p "$HOME/Applications"
  rm -rf "$HOME/Applications/kasaterm.app"
  cp -R "$APP" "$HOME/Applications/kasaterm.app"
  touch "$HOME/Applications/kasaterm.app"
  echo "installed to ~/Applications/kasaterm.app"
fi
