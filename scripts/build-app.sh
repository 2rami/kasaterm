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
#   scripts/build-app.sh --force    # 다른 pane 이 코드를 만지는 중이어도 강행

set -euo pipefail

PROFILE="release"
INSTALL=0
FORCE=0
for arg in "$@"; do
  case "$arg" in
    --debug)   PROFILE="debug" ;;
    --install) INSTALL=1 ;;
    --force)   FORCE=1 ;;
    *) echo "unknown arg: $arg" >&2; exit 2 ;;
  esac
done

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# 굽기는 워킹트리를 **통째로** 담는다 — 여러 pane 이 한 워킹트리를 공유하므로,
# 남이 Rust 를 고치는 중에 구우면 그 반쯤 만든 기능이 함께 들어가고 운이 나쁘면
# 컴파일조차 안 된다(2026-08-11 지시: "다른애들이 수정하면 안굽게하자").
# board 를 못 읽으면(앱이 꺼짐·CLI 없음) 조용히 통과한다 — 이 가드가 사람이
# 직접 굽는 길까지 막아서는 안 된다.
#
# 판정의 정본은 **워킹트리**다. board 의 `changed_files` 는 transcript 누적이라
# 커밋한 뒤에도 남아서, 그것만 보면 오늘 그 파일을 만진 pane 이 하나라도 있으면
# 영영 못 굽는다(2026-08-11 실측: 아루가 커밋을 끝내 워킹트리가 깨끗한데도 계속
# 막혔다). 미커밋 Rust 가 없으면 물어볼 것도 없이 통과하고, 있을 때만 board 로
# "그게 누구 것인지" 를 묻는다 — 파일이 안 겹치는 작업끼리 서로 기다리지 않는다.
if [[ $FORCE -eq 0 ]] && command -v kasaterm-cli >/dev/null 2>&1; then
  DIRTY="$(git status --porcelain -- '*.rs' 2>/dev/null | sed -e 's/^...//' -e 's/.* -> //')"
  BUSY=""
  if [[ -n "$DIRTY" ]]; then
  BUSY="$(kasaterm-cli board 2>/dev/null | ROOT="$ROOT" DIRTY="$DIRTY" python3 -c '
import json, os, sys
try:
    board = (json.load(sys.stdin).get("result") or {}).get("board") or []
except Exception:
    sys.exit(0)
root = os.environ["ROOT"].rstrip("/") + "/"
me = os.environ.get("KASATERM_PANE_ID", "")
dirty = {l.strip().strip("\"") for l in os.environ.get("DIRTY", "").splitlines() if l.strip()}
for r in board:
    if r.get("surface_id") == me or r.get("status") != "working":
        continue
    # 이 레포의 Rust 중 **아직 커밋 안 된 것**만. board 에는 다른 레포 pane 도 실린다.
    hits = [f for f in (r.get("changed_files") or [])
            if f.startswith(root) and f.endswith(".rs") and f[len(root):] in dirty]
    if hits:
        sid = str(r.get("surface_id") or "?")
        who = str(r.get("character") or sid)
        names = ", ".join(os.path.basename(f) for f in hits[:3])
        print("  " + who + " (" + sid + "): " + names)
' || true)"
  fi
  if [[ -n "$BUSY" ]]; then
    echo "[build-app] 다른 pane 이 이 레포의 Rust 를 고치는 중이라 굽지 않는다:" >&2
    echo "$BUSY" >&2
    echo "" >&2
    echo "  구우면 그쪽 미완성이 함께 들어간다. 끝나길 기다리거나 --force." >&2
    exit 1
  fi
fi

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
    <!-- 이게 없으면 pane 안의 녹음 도구가 조용히 실패한다(2026-08-18 실측: sox 는
         정상 크기의 무음 wav, ffmpeg avfoundation 은 SIGABRT). 권한 팝업도 안 뜨고
         시스템 설정 마이크 목록에 앱이 아예 안 나타나 사용자가 켤 방법조차 없다. -->
    <key>NSMicrophoneUsageDescription</key>
    <string>kasaterm 이 회의 녹음과 음성 입력을 위해 마이크를 사용합니다.</string>
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
