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

# Build the binaries. Besides kasaterm we bundle kasaterm-cli so a pane can
# drive siblings via the socket (install_pane_shims stages it on the pane PATH).
if [[ "$PROFILE" == "release" ]]; then
  cargo build --release -p kasaterm -p kasa-socket
  BINDIR="target/release"
else
  cargo build -p kasaterm -p kasa-socket
  BINDIR="target/debug"
fi

APP="dist/kasaterm.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

cp "$BINDIR/kasaterm" "$APP/Contents/MacOS/kasaterm"
cp "$BINDIR/kasaterm-cli" "$APP/Contents/MacOS/kasaterm-cli"
cp assets/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"
# 협업 훅 정본 — claude PATH shim 이 --settings 로 가리키는 스크립트들
# (locate_collab_hooks_dir 의 번들 경로). install-hooks.sh 배포 불필요.
cp -R app/kasaterm/collab-hooks "$APP/Contents/Resources/collab-hooks"
rm -rf "$APP/Contents/Resources/collab-hooks/__pycache__"

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
  # 원자 스왑 — rm -rf 후 cp 는 복사되는 수 초간 번들 경로가 비어, 그 창에
  # 실행 중인 pane 의 hook(Resources/collab-hooks/*)이 돌면 exit 127
  # ("Stop hook failed, no stderr" 실측). 옆에 다 복사해 두고 mv 두 번으로 교체.
  rm -rf "$HOME/Applications/kasaterm.app.new" "$HOME/Applications/kasaterm.app.old"
  cp -R "$APP" "$HOME/Applications/kasaterm.app.new"
  if [[ -d "$HOME/Applications/kasaterm.app" ]]; then
    mv "$HOME/Applications/kasaterm.app" "$HOME/Applications/kasaterm.app.old"
  fi
  mv "$HOME/Applications/kasaterm.app.new" "$HOME/Applications/kasaterm.app"
  rm -rf "$HOME/Applications/kasaterm.app.old"
  touch "$HOME/Applications/kasaterm.app"
  echo "installed to ~/Applications/kasaterm.app"
fi
