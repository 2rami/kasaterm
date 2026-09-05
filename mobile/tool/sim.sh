#!/usr/bin/env bash
# 가상 아이폰(시뮬레이터)에서 앱을 보고 누르는 루프. 사람 화면의 포커스를 안 뺏는다 —
# 실기는 iOS 26 에서 화면을 찍을 길이 막혀 있어(screenshotr 없음) 이쪽이 「보며 일하기」의 정본이다.
#
#   tool/sim.sh up [pane] [machine]   부팅·디버그 빌드·설치·실행. pane 을 주면 켜자마자 그 학생 화면
#   tool/sim.sh shot [out.jpg]        화면을 720px JPEG 로(기본 $TMPDIR/sim.jpg) — 창에 싣기 전에 줄인다
#   tool/sim.sh tap X Y               포인트 좌표(402x874)로 탭. shot 의 JPEG 는 세로 720 이라 ×874/720
#   tool/sim.sh type "글"             포커스된 입력창에 타이핑
#   tool/sim.sh key <코드>            키 하나(idb ui key 코드) — enter 는 40
#   tool/sim.sh text                  화면의 접근성 트리(글자로 판정할 때 스샷 대신)
#   tool/sim.sh reset                 시스템 알림이 화면을 덮었을 때 껐다 켠다
#
# 주소는 로컬 카사텀(KASA_ROOT, 기본 http://127.0.0.1:8765/). 탭·타이핑은 idb(brew idb-companion)가 필요하다.
set -euo pipefail
cd "$(dirname "$0")/.."

sim=${KASA_SIM:-EEB8BE7C-B2C3-4EB9-9570-86FA79B8FF9B}
app=com.debimarlene.kasatermMobile
cmd=${1:-}; shift || true

case "$cmd" in
  up)
    xcrun simctl bootstatus "$sim" -b >/dev/null 2>&1 || xcrun simctl boot "$sim"
    defines=$(mktemp); trap 'rm -f "$defines"' EXIT
    python3 -c 'import json, sys
d = {"KASA_ROOT": sys.argv[1]}
if len(sys.argv) > 2 and sys.argv[2]: d["KASA_OPEN_PANE"] = sys.argv[2]
if len(sys.argv) > 3 and sys.argv[3]: d["KASA_OPEN_MACHINE"] = sys.argv[3]
print(json.dumps(d))' "${KASA_ROOT:-http://127.0.0.1:8765/}" "${1:-}" "${2:-}" > "$defines"
    NO_PROXY='127.0.0.1,localhost' flutter build ios --simulator --debug --dart-define-from-file="$defines" \
      | grep -E "Built|Error|error:" || true
    xcrun simctl terminate "$sim" "$app" >/dev/null 2>&1 || true
    xcrun simctl install "$sim" build/ios/iphonesimulator/Runner.app
    xcrun simctl launch "$sim" "$app" >/dev/null
    echo "가상 아이폰에 올렸다 — tool/sim.sh shot 으로 본다"
    ;;
  shot)
    out=${1:-${TMPDIR:-/tmp}/sim.jpg}
    png=$(mktemp -t sim).png
    xcrun simctl io "$sim" screenshot "$png" >/dev/null 2>&1
    sips -s format jpeg -s formatOptions 60 -Z 720 "$png" --out "$out" >/dev/null 2>&1
    rm -f "$png"
    echo "$out"
    ;;
  tap) idb ui tap "$1" "$2" --udid "$sim" >/dev/null ;;
  type) idb ui text "$1" --udid "$sim" >/dev/null ;;
  key) idb ui key "$1" --udid "$sim" >/dev/null ;;
  text)
    # 접근성 트리를 「라벨 @ x,y 폭x높이」 한 줄씩으로 — tap 좌표를 스샷 없이 여기서 읽는다.
    idb ui describe-all --udid "$sim" | python3 -c 'import json, sys
for e in json.load(sys.stdin):
    label = e.get("AXLabel") or e.get("AXValue")
    if not label: continue
    f = e.get("frame", {})
    cx, cy = f.get("x", 0) + f.get("width", 0) / 2, f.get("y", 0) + f.get("height", 0) / 2
    print(f"{label}  @ {cx:.0f},{cy:.0f}")'
    ;;
  reset) xcrun simctl shutdown "$sim" >/dev/null 2>&1 || true; xcrun simctl boot "$sim"; xcrun simctl launch "$sim" "$app" >/dev/null ;;
  *) sed -n '2,13p' "$0"; exit 1 ;;
esac
