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

# 버전 = workspace Cargo.toml 단일 소스. Sparkle 은 CFBundleVersion 증가로 새 버전을
# 판정하므로 Info.plist 버전을 여기서 동적 주입한다(예전엔 0.1.0 하드코딩).
VERSION="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"(.*)".*/\1/')"

# Sparkle.framework — vendor 에 없으면 release tarball 받아 추출(캐시, .gitignore).
SPARKLE_VER="2.9.3"
SPARKLE_FW="vendor/Sparkle/Sparkle.framework"
if [[ ! -d "$SPARKLE_FW" ]]; then
  echo "[build-app] Sparkle $SPARKLE_VER 받는 중..."
  mkdir -p vendor/Sparkle
  gh release download "$SPARKLE_VER" --repo sparkle-project/Sparkle -p "Sparkle-$SPARKLE_VER.tar.xz" -D vendor --clobber
  tar -xJf "vendor/Sparkle-$SPARKLE_VER.tar.xz" -C vendor/Sparkle/
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

# Sparkle.framework → Contents/Frameworks (ditto 로 symlink·권한 보존). 자동 업데이트용.
# macos_sparkle.rs 가 런타임에 Versions/B/Sparkle 를 dlopen 한다.
mkdir -p "$APP/Contents/Frameworks"
ditto "$SPARKLE_FW" "$APP/Contents/Frameworks/Sparkle.framework"

# 아로나(god 모드) UI 정적 번들 → Resources/arona-ui. node/npm 없으면 경고+skip
# (solo 전용 사용자는 영향 0 — god 모드일 때만 이 웹뷰를 띄운다). lock 있으면
# 재현 빌드(npm ci), 없으면 npm install 폴백.
if command -v npm >/dev/null 2>&1; then
  echo "[build-app] arona-ui 번들 중..."
  if ( cd web/arona-ui && { [ -f package-lock.json ] && npm ci --silent || npm install --silent --no-audit --no-fund; } && npm run build >/dev/null 2>&1 ); then
    rm -rf "$APP/Contents/Resources/arona-ui"
    cp -R web/arona-ui/dist "$APP/Contents/Resources/arona-ui"
    echo "[build-app] arona-ui → Resources/arona-ui"
  else
    echo "[build-app] ⚠ arona-ui 빌드 실패 — god 모드 UI 제외하고 계속"
  fi
else
  echo "[build-app] ⚠ npm 없음 — arona-ui 번들 skip(god 모드 UI 제외)"
fi

# Minimal Info.plist. Bundle id namespaced under the project root so
# Launchpad / Spotlight key the icon to this binary specifically.
cat > "$APP/Contents/Info.plist" <<PLIST
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
    <string>$VERSION</string>
    <key>CFBundleShortVersionString</key>
    <string>$VERSION</string>
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
    <!-- 이게 없으면 macOS 가 Desktop/Documents/Downloads 접근 시 재승인 프롬프트를
         띄우지 못해 조용히 EPERM 으로 막는다(자식 셸의 ls/git/brew 가 cwd 를 못 읽음). -->
    <key>NSDesktopFolderUsageDescription</key>
    <string>kasaterm 이 데스크톱의 파일에 접근해 명령을 실행하고 프로젝트를 엽니다.</string>
    <key>NSDocumentsFolderUsageDescription</key>
    <string>kasaterm 이 문서 폴더의 파일에 접근해 명령을 실행하고 프로젝트를 엽니다.</string>
    <key>NSDownloadsFolderUsageDescription</key>
    <string>kasaterm 이 다운로드 폴더의 파일에 접근해 명령을 실행하고 프로젝트를 엽니다.</string>
    <key>NSRequiresAquaSystemAppearance</key>
    <false/>
    <key>SUFeedURL</key>
    <string>https://2rami.github.io/kasaterm/appcast.xml</string>
    <key>SUPublicEDKey</key>
    <string>E4tFAb2UND+0QhgTSv2pFYKIC3ReT/dLia20KHfZxKw=</string>
    <key>SUEnableAutomaticChecks</key>
    <true/>
    <key>SUScheduledCheckInterval</key>
    <integer>86400</integer>
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

# Sign with a stable self-signed identity when one exists, so macOS keeps
# TCC permissions (screen recording / automation) across rebuilds instead
# of re-prompting every time. Ad-hoc (`-`) changes the code hash on every
# build, which macOS treats as a brand-new app. Create the identity once:
#   Keychain Access → Certificate Assistant → Create a Certificate
#   Name: kasaterm-dev, Type: Self-Signed Root, Certificate Type: Code Signing
# CI(release.yml)는 Secrets 의 kasaterm-ci 인증서를 임포트하고 이 env 로 지정한다.
SIGN_ID="${KASATERM_SIGN_ID:-kasaterm-dev}"
# No -v: a self-signed cert is valid-but-untrusted (CSSMERR_TP_NOT_TRUSTED),
# which -v filters out. codesign still signs with it, and TCC keys
# permissions off the signing identity, so untrusted is fine for local use.
if security find-identity -p codesigning 2>/dev/null | grep -q "$SIGN_ID"; then
  SIGN="$SIGN_ID"
  SIGN_MSG="signed with '$SIGN_ID' — TCC permissions persist across rebuilds"
else
  SIGN="-"  # ad-hoc
  SIGN_MSG="signed ad-hoc — create a '$SIGN_ID' code-signing cert to stop the permission re-prompts"
fi
# Sparkle.framework 는 nested(XPC·Updater·Autoupdate·dylib)부터 → framework → 마지막
# app 순으로 서명한다. `--deep` 한 방은 nested XPC 의 서명 일관성을 보장 못 해 실행 시
# XPC 로드가 실패하고 자동 업데이트가 깨질 수 있다(안쪽→바깥쪽이 정석).
FW="$APP/Contents/Frameworks/Sparkle.framework"
if [[ -d "$FW" ]]; then
  for nested in \
    "$FW/Versions/B/XPCServices/Downloader.xpc" \
    "$FW/Versions/B/XPCServices/Installer.xpc" \
    "$FW/Versions/B/Updater.app" \
    "$FW/Versions/B/Autoupdate"; do
    [[ -e "$nested" ]] && codesign --force --sign "$SIGN" "$nested" 2>/dev/null || true
  done
  codesign --force --sign "$SIGN" "$FW" 2>/dev/null || true
fi
# kasaterm-cli 는 별도 실행 바이너리 — app 서명(--deep 제거)이 안 덮으므로 개별 서명.
codesign --force --sign "$SIGN" "$APP/Contents/MacOS/kasaterm-cli" 2>/dev/null || true
codesign --force --sign "$SIGN" "$APP" 2>/dev/null \
  && echo "$SIGN_MSG" \
  || echo "warning: signing '$APP' failed; app left unsigned"

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
  # LaunchServices 에 .md 문서 타입(CFBundleDocumentTypes)을 재등록 — 안 하면
  # 더블클릭이 캐시된 옛 핸들러로 가서 kasaterm 이 "다음으로 열기" 후보에 안 뜬다.
  LSREGISTER="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"
  [[ -x "$LSREGISTER" ]] && "$LSREGISTER" -f "$HOME/Applications/kasaterm.app" \
    && echo "re-registered with LaunchServices (.md handler)"
fi
