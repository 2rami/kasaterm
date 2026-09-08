#!/usr/bin/env bash
# App Store Connect 로 올리는 판 — TestFlight 용. 주소를 굽지 않는다(phone.sh 와 다르다):
# 설치한 폰은 하단바 「모바일」 QR → 안내 페이지의 「앱으로 열기」로 주소를 받는다.
#
#   tool/testflight.sh            아카이브 → 서명 → 업로드까지. 빌드 번호는 시각.
#
# 키: ~/.config/kasaterm/asc/key.json ({"key_id","issuer_id","p8"}). 팀 ID 는 거기서 읽는다
# (ASC_TEAM_ID 로 덮어쓴다). xcodebuild 가 API 키로 프로파일·인증서를 알아서 만든다.
set -euo pipefail
cd "$(dirname "$0")/.."

cfg=$HOME/.config/kasaterm/asc/key.json
[ -f "$cfg" ] || { echo "키가 없다 — $cfg 에 key_id·issuer_id·p8 를 적어 달라" >&2; exit 1; }
read -r KEY_ID ISSUER P8 < <(python3 -c 'import json,os,sys; c=json.load(open(sys.argv[1])); print(c["key_id"], c["issuer_id"], os.path.expanduser(c["p8"]))' "$cfg")
team=${ASC_TEAM_ID:-$(python3 tool/asc.py team | awk 'NR==1{print $1}')}
[ -n "$team" ] || { echo "팀 ID 를 못 받았다 — API 키 권한을 확인해 달라" >&2; exit 1; }

build=$(date +%y%m%d%H%M)
NO_PROXY='127.0.0.1,localhost' flutter build ios --release --no-codesign --build-number="$build"

arch=build/ios/archive/Runner.xcarchive
rm -rf "$arch"
xcodebuild -workspace ios/Runner.xcworkspace -scheme Runner -configuration Release \
  -destination 'generic/platform=iOS' -archivePath "$arch" archive \
  DEVELOPMENT_TEAM="$team" CODE_SIGN_STYLE=Automatic \
  -allowProvisioningUpdates -authenticationKeyPath "$P8" -authenticationKeyID "$KEY_ID" -authenticationKeyIssuerID "$ISSUER" \
  | tail -3

opts=$(mktemp -t export.XXXX.plist)
trap 'rm -f "$opts"' EXIT
cat > "$opts" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>method</key><string>app-store-connect</string>
  <key>destination</key><string>upload</string>
  <key>teamID</key><string>$team</string>
  <key>signingStyle</key><string>automatic</string>
  <key>uploadSymbols</key><true/>
  <key>manageAppVersionAndBuildNumber</key><false/>
</dict></plist>
PLIST
xcodebuild -exportArchive -archivePath "$arch" -exportOptionsPlist "$opts" -exportPath build/ios/export \
  -allowProvisioningUpdates -authenticationKeyPath "$P8" -authenticationKeyID "$KEY_ID" -authenticationKeyIssuerID "$ISSUER" \
  | tail -5
echo "올렸다 — 빌드 $build. 처리는 App Store Connect 에서 몇 분에서 수십 분."
echo "$build" > build/ios/last-testflight-build
