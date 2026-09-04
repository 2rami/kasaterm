#!/usr/bin/env bash
# 이 맥의 폰 주소를 구워 넣은 릴리스판을 케이블로 붙은 아이폰에 설치하고 켠다.
# 앱이 주소를 처음부터 알고 있어 폰에서는 아무것도 입력하지 않는다.
#
#   tool/phone.sh [기기 UUID]      기본은 연결된 첫 아이폰(xcrun devicectl list devices)
#
# 주소는 로컬 카사텀(GET /mobile/users)의 주인 항목에서 읽는다 — 앱이 켜져 있고
# 「● 바깥」이 켜져 있어야 완성 주소가 온다. KASA_ROOT 로 직접 줄 수도 있다.
# 팀 ID 는 Xcode 에 넣어 둔 Apple ID 에서 읽는다(FLUTTER_XCODE_DEVELOPMENT_TEAM 으로 덮어쓴다).
# 무료 계정이면 7일마다 이 스크립트를 다시 돌린다.
set -euo pipefail
cd "$(dirname "$0")/.."

root=${KASA_ROOT:-}
if [ -z "$root" ]; then
  root=$(curl -sf --noproxy '*' http://127.0.0.1:8765/mobile/users | python3 -c '
import json, sys
users = [u for u in json.load(sys.stdin).get("users", []) if u.get("owner")]
print((users[0].get("url") or "") if users else "")')
fi
[ -n "$root" ] || { echo "폰 주소가 없다 — 카사텀이 켜져 있고 「● 바깥」이 켜져 있어야 한다" >&2; exit 1; }

team=${FLUTTER_XCODE_DEVELOPMENT_TEAM:-$(defaults read com.apple.dt.Xcode IDEProvisioningTeamByIdentifier 2>/dev/null \
  | sed -n 's/.*teamID = \([A-Z0-9]*\);.*/\1/p' | head -1)}
[ -n "$team" ] || { echo "Xcode 에 Apple ID 가 없다 — Settings → Accounts 에 한 번 넣어 달라" >&2; exit 1; }

device=${1:-$(xcrun devicectl list devices 2>/dev/null | awk '/connected/ && /iPhone/ { print $3; exit }')}
[ -n "$device" ] || { echo "케이블로 붙은 아이폰이 없다" >&2; exit 1; }

# 주소는 자격이라 argv 에 싣지 않는다 — 파일(0600)로 넘기고 끝나면 지운다.
defines=$(mktemp)
trap 'rm -f "$defines"' EXIT
python3 -c 'import json, sys; print(json.dumps({"KASA_ROOT": sys.argv[1]}))' "$root" > "$defines"

NO_PROXY='127.0.0.1,localhost' FLUTTER_XCODE_DEVELOPMENT_TEAM="$team" \
  flutter build ios --release --dart-define-from-file="$defines"
xcrun devicectl device install app --device "$device" build/ios/iphoneos/Runner.app >/dev/null
xcrun devicectl device process launch --device "$device" com.debimarlene.kasatermMobile >/dev/null
echo "폰에 올렸다 — 앱이 켜져 있을 것이다"
