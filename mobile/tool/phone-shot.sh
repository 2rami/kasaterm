#!/usr/bin/env bash
# 케이블로 붙은 실기 아이폰 화면을 본다.
#   tool/phone-shot.sh [out.jpg]      720px JPEG 파일로(기본 $TMPDIR/phone.jpg). 판정은 이 파일을 Read 로 직접 본다
# iOS 17+ 는 옛 screenshotr 서비스가 없어 idevicescreenshot·idb 가 못 찍는다 — pymobiledevice3 의
# 터널을 탄다. 터널 데몬은 루트가 필요하다. 손으로 띄운 것은 앱을 껐다 켤 때 같이 죽어
# 매번 다시 치게 되므로(2026-09-06) launchd 데몬으로 둔다 — 부팅 때 뜨고 죽으면 되살아난다:
#   sudo cp tool/com.geono.pymobiledevice3-tunneld.plist /Library/LaunchDaemons/ \
#     && sudo launchctl bootstrap system /Library/LaunchDaemons/com.geono.pymobiledevice3-tunneld.plist
# (⚠️ `sudo python3 -m pymobiledevice3 …` 는 안 된다 — 루트 파이썬엔 그 모듈이 없다. uv 셔틀 경로를 쓴다.)
# 데몬이 처음 붙을 때 폰에 「이 컴퓨터를 신뢰」 창이 뜬다 — 그건 사람이 누른다.
set -euo pipefail
out=${1:-${TMPDIR:-/tmp}/phone.jpg}
pmd=${PYMOBILEDEVICE3:-$HOME/.local/bin/pymobiledevice3}
# 터널 데몬의 목록이 곧 「지금 찍을 수 있는 기기」다 — 키가 UDID.
list=$(curl -sf http://127.0.0.1:49151/ 2>/dev/null) || {
  echo "터널 데몬이 없다 — launchd 에 올려야 한다(파일 머리말 참고): sudo launchctl bootstrap system /Library/LaunchDaemons/com.geono.pymobiledevice3-tunneld.plist" >&2
  exit 1
}
udid=${KASA_PHONE_UDID:-$(printf '%s' "$list" | python3 -c 'import json, sys; print(next(iter(json.load(sys.stdin)), ""))')}
[ -n "$udid" ] || { echo "터널에 잡힌 아이폰이 없다 — 케이블과 잠금을 확인해 달라" >&2; exit 1; }
png=$(mktemp -t phone).png
"$pmd" developer dvt screenshot --tunnel "$udid" "$png" >/dev/null 2>&1
sips -s format jpeg -s formatOptions 60 -Z 720 "$png" --out "$out" >/dev/null 2>&1
echo "$out"
rm -f "$png"
