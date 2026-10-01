# testflight.sh·adhoc.sh 가 함께 쓰는 앞부분 — source 로 부른다(mobile/ 에서).
# API 키를 읽고 서명 인자를 골라 아카이브까지 한다. 빌드 번호는 시각이고, 앱이 「새 판」을 가리도록
# KASA_BUILD 로도 굽는다. 끝나면 KEY_ID·ISSUER·P8·team·build·arch 와
# 내보내기에 그대로 붙일 api_auth 배열이 남는다.
#
# 키: ~/.config/kasaterm/asc/key.json ({"key_id","issuer_id","p8"}). 팀 ID 는 거기서 읽는다
# (ASC_TEAM_ID 로 덮어쓴다). xcodebuild 가 API 키로 프로파일·인증서를 알아서 만든다.

cfg=$HOME/.config/kasaterm/asc/key.json
[ -f "$cfg" ] || { echo "키가 없다 — $cfg 에 key_id·issuer_id·p8 를 적어 달라" >&2; exit 1; }
read -r KEY_ID ISSUER P8 < <(python3 -c 'import json,os,sys; c=json.load(open(sys.argv[1])); print(c["key_id"], c["issuer_id"], os.path.expanduser(c["p8"]))' "$cfg")
team=${ASC_TEAM_ID:-$(python3 tool/asc.py team | awk 'NR==1{print $1}')}
[ -n "$team" ] || { echo "팀 ID 를 못 받았다 — API 키 권한을 확인해 달라" >&2; exit 1; }

api_auth=(-allowProvisioningUpdates -authenticationKeyPath "$P8" -authenticationKeyID "$KEY_ID" -authenticationKeyIssuerID "$ISSUER")
sign=(DEVELOPMENT_TEAM="$team" CODE_SIGN_STYLE=Automatic "${api_auth[@]}")
# 맥미니: 로그인 키체인에 개발 키가 없고 옛 개발 키체인은 암호가 안 맞는다. 자동 서명에 맡기면 그 잠긴
# 신원을 골라 암호창에서 멈추므로, 전용 키체인의 인증서만 든 수동 프로파일로 아카이브한다(배포 서명은 그대로 클라우드).
kc=$HOME/Library/Keychains/ios-signing.keychain-db
if [ -f "$kc" ]; then
  security unlock-keychain -p "$(cat "$(dirname "$cfg")/ios-signing.pw")" "$kc"
  sign=(DEVELOPMENT_TEAM="$team" CODE_SIGN_STYLE=Manual CODE_SIGN_IDENTITY="Apple Development"
    OTHER_CODE_SIGN_FLAGS="--keychain $kc"
    'PROVISIONING_PROFILE_SPECIFIER=$(KASA_PROFILE_$(PRODUCT_BUNDLE_IDENTIFIER:identifier))'
    KASA_PROFILE_com_debimarlene_kasaterm="kasaterm mini dev app"
    KASA_PROFILE_com_debimarlene_kasaterm_NotificationService="kasaterm mini dev notif")
fi
# 수동 배포 키체인(testflight.sh --manual)이 검색 목록에 있으면 자동 서명 내보내기도 그 키를 집다 잠금
# 암호창에서 멈춘다(10-01 실측) — 미리 연다.
dkc=$HOME/Library/Keychains/ios-dist.keychain-db
if [ -f "$dkc" ] && security list-keychains -d user | tr -d '" ' | grep -qx "$dkc"; then
  security unlock-keychain -p "$(cat "$(dirname "$cfg")/ios-dist.pw")" "$dkc"
fi

build=$(date +%y%m%d%H%M)
tool/kasanet.sh
NO_PROXY='127.0.0.1,localhost' flutter build ios --release --no-codesign --build-number="$build" \
  --dart-define=KASA_BUILD="$build"

arch=build/ios/archive/Runner.xcarchive
rm -rf "$arch"
xcodebuild -workspace ios/Runner.xcworkspace -scheme Runner -configuration Release \
  -destination 'generic/platform=iOS' -archivePath "$arch" archive "${sign[@]}" \
  | tail -3
