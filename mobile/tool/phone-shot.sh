#!/usr/bin/env bash
# 케이블로 붙은 실기 아이폰 화면을 720px JPEG 로 찍는다(기본 $TMPDIR/phone.jpg).
# iOS 17+ 는 옛 screenshotr 서비스가 없어 idevicescreenshot·idb 가 못 찍는다 — pymobiledevice3 의
# 터널을 탄다. 터널 데몬은 루트가 필요해 사람이 한 번 띄운다(재부팅 전까지 산다):
#   sudo ~/.local/bin/pymobiledevice3 remote tunneld -d -p tcp
# 처음 붙을 때 폰에 「이 컴퓨터를 신뢰」 암호 창이 뜬다 — 그건 사람이 넣는다.
set -euo pipefail
out=${1:-${TMPDIR:-/tmp}/phone.jpg}
pmd=${PYMOBILEDEVICE3:-$HOME/.local/bin/pymobiledevice3}
# 터널 데몬의 목록이 곧 「지금 찍을 수 있는 기기」다 — 키가 UDID.
list=$(curl -sf http://127.0.0.1:49151/ 2>/dev/null) || {
  echo "터널 데몬이 없다 — 사람이 한 번 띄워야 한다:  sudo $pmd remote tunneld -d -p tcp" >&2
  exit 1
}
udid=${KASA_PHONE_UDID:-$(printf '%s' "$list" | python3 -c 'import json, sys; print(next(iter(json.load(sys.stdin)), ""))')}
[ -n "$udid" ] || { echo "터널에 잡힌 아이폰이 없다 — 케이블과 잠금을 확인해 달라" >&2; exit 1; }
png=$(mktemp -t phone).png
"$pmd" developer dvt screenshot --tunnel "$udid" "$png" >/dev/null 2>&1
sips -s format jpeg -s formatOptions 60 -Z 720 "$png" --out "$out" >/dev/null 2>&1
rm -f "$png"
echo "$out"
