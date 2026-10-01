#!/usr/bin/env bash
# App Store Connect 로 올리는 판 — TestFlight 용. 주소를 굽지 않는다(phone.sh 와 다르다):
# 설치한 폰은 하단바 「모바일」 QR → 안내 페이지의 「앱으로 열기」로 주소를 받는다.
#
#   tool/testflight.sh            아카이브 → 서명 → 업로드까지. 빌드 번호는 시각.
#   tool/testflight.sh --manual   배포 서명만 손으로 — 클라우드 관리 인증서 대신 전용 키체인의
#                                 Apple Distribution 인증서와 App Store 프로파일로 내보낸다.
#
# --manual 준비(기기마다 한 번): asc.py dist-cert 로 인증서를 받아 ~/Library/Keychains/ios-dist.keychain-db
# (암호는 ~/.config/kasaterm/asc/ios-dist.pw)에 키와 함께 넣고 set-key-partition-list 로 codesign 에 열어 둔 뒤,
# asc.py store-profile 로 아래 두 이름의 프로파일을 받는다. 로그인 키체인에 넣으면 파티션 목록을 로그인 암호로만
# 열 수 있어 codesign 이 창에서 멈춘다(-A 로 넣어도 같다, 10-01 실측).
set -euo pipefail
cd "$(dirname "$0")/.."
manual=0
[ "${1:-}" = --manual ] && manual=1

# 키·팀·아카이브는 adhoc.sh 와 같다.
. tool/ios-archive.sh

signing='<key>signingStyle</key><string>automatic</string>'
if [ "$manual" = 1 ]; then
  dkc=$HOME/Library/Keychains/ios-dist.keychain-db
  [ -f "$dkc" ] || { echo "--manual: $dkc 가 없다 — 위 준비를 먼저 해 달라" >&2; exit 1; }
  security unlock-keychain -p "$(cat "$(dirname "$cfg")/ios-dist.pw")" "$dkc"
  # 내보내기의 재서명은 --keychain 을 못 받아 검색 목록에서만 신원을 찾는다. 목록은 통째로 덮어쓰는
  # 명령이라 기존 항목을 함께 넘긴다.
  if ! security list-keychains -d user | tr -d '" ' | grep -qx "$dkc"; then
    kcs=()
    while read -r k; do kcs+=("$k"); done < <(security list-keychains -d user | tr -d '"' | sed 's/^ *//')
    security list-keychains -d user -s "${kcs[@]}" "$dkc"
  fi
  signing='<key>signingStyle</key><string>manual</string>
  <key>signingCertificate</key><string>Apple Distribution</string>
  <key>provisioningProfiles</key><dict>
    <key>com.debimarlene.kasaterm</key><string>kasaterm appstore app</string>
    <key>com.debimarlene.kasaterm.NotificationService</key><string>kasaterm appstore notif</string>
  </dict>'
fi

opts=$(mktemp -t export.XXXX.plist)
trap 'rm -f "$opts"' EXIT
cat > "$opts" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>method</key><string>app-store-connect</string>
  <key>destination</key><string>upload</string>
  <key>teamID</key><string>$team</string>
  $signing
  <key>uploadSymbols</key><true/>
  <key>manageAppVersionAndBuildNumber</key><false/>
</dict></plist>
PLIST
xcodebuild -exportArchive -archivePath "$arch" -exportOptionsPlist "$opts" -exportPath build/ios/export "${api_auth[@]}" \
  | tail -5
echo "올렸다 — 빌드 $build. 처리는 App Store Connect 에서 몇 분에서 수십 분."
echo "$build" > build/ios/last-testflight-build
