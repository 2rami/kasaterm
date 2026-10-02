#!/usr/bin/env bash
# 등록된 기기에만 깔리는 Ad Hoc 판 — TestFlight 없이 폰·아이패드에 설치 링크로 넣는다.
# 주소를 굽지 않는다(testflight.sh 와 같다). 서명은 testflight.sh 와 같은 API 키 자동 서명이다.
#
#   tool/adhoc.sh               아카이브 → Ad Hoc ipa → 관문에 올리고 설치 링크를 KASA-share 에 남긴다
#   tool/adhoc.sh --show        위에 더해 링크를 사람이 보는 기기(폰이면 폰)로 띄운다
#   tool/adhoc.sh --no-publish  ipa 까지만(build/ios/adhoc/)
#   tool/adhoc.sh --publish     이미 만든 ipa 를 다시 올린다
#
# 기기: asc.py devices 로 보고 asc.py device <UDID> <이름> 으로 더한다. 자동 서명이 내보낼 때 그때 등록된 기기
# 전부로 프로파일을 새로 받으므로, 기기를 더했으면 다시 구워야 그 기기에 깔린다.
#
# 올리기: 관문 기계(KASA_INSTALL_HOST, 기본 nachoneko)의 ~/.config/kasaterm/relay-install/<token>/ 에 ssh 로
# 쓰고, 관문(gateway_install.rs)이 /relay/install/<token>/ 으로 내준다. token 은 판마다 새로 뽑는 추측 불가
# 문자열이고 그 자체가 링크의 자격이다 — itms-services 는 인증 헤더를 못 싣는다. 지금 판과 바로 앞 판만 남긴다.
#
# 관문 기계에 서명기(adhoc_sign.py, 올릴 때마다 asc.py 와 함께 부친다)가 준비돼 있으면 공개 전에 거기서 다시
# 서명한다 — 링크로 새로 등록된 기기까지 든 프로파일로, 그 기계의 키로. 준비는 그 기계에서 한 번:
#   ~/.local/share/kasa-relay/adhoc/adhoc_sign.py setup
set -euo pipefail
cd "$(dirname "$0")/.."
mode=all show=0
for a in "$@"; do
  case "$a" in
    --no-publish) mode=build ;;
    --publish) mode=publish ;;
    --show) show=1 ;;
    *) echo "모르는 인자: $a" >&2; exit 2 ;;
  esac
done
out=build/ios/adhoc
# 관문 기계 자신에서 돌면(서명 준비가 여기 있으면) ssh 없이 바로 쓰고, 아카이브도 서명 없이 해 둔 뒤 여기서 서명한다.
if [ -z "${KASA_INSTALL_HOST:-}" ] && [ -f "$HOME/.config/kasaterm/asc/adhoc/cert.json" ]; then host=local; else host=${KASA_INSTALL_HOST:-nachoneko}; fi
origin=${KASA_INSTALL_ORIGIN:-https://kasaterm.debimarlene.com}
remote=.config/kasaterm/relay-install

if [ "$mode" != publish ] && [ "$host" = local ]; then
  KASA_ARCHIVE_UNSIGNED=1
  . tool/ios-archive.sh
  rm -rf "$out" && mkdir -p "$out/x/Payload"
  ditto "$arch/Products/Applications/Runner.app" "$out/x/Payload/Runner.app"
  (cd "$out/x" && zip -qry ../kasaterm.ipa Payload)
  rm -rf "$out/x"
elif [ "$mode" != publish ]; then
  . tool/ios-archive.sh
  opts=$(mktemp -t export.XXXX.plist)
  trap 'rm -f "$opts"' EXIT
  cat > "$opts" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>method</key><string>release-testing</string>
  <key>teamID</key><string>$team</string>
  <key>signingStyle</key><string>automatic</string>
  <key>thinning</key><string>&lt;none&gt;</string>
  <key>manageAppVersionAndBuildNumber</key><false/>
</dict></plist>
PLIST
  rm -rf "$out"
  xcodebuild -exportArchive -archivePath "$arch" -exportOptionsPlist "$opts" -exportPath "$out" "${api_auth[@]}" \
    | tail -3
  ipa=$(ls "$out"/*.ipa)
  mv "$ipa" "$out/kasaterm.ipa"
fi
if [ "$mode" != publish ]; then
  info=$arch/Products/Applications/Runner.app/Info.plist
  python3 - "$info" "$out/meta.json" <<'PY'
import json, plistlib, sys, time
p = plistlib.load(open(sys.argv[1], "rb"))
json.dump({"bundle_id": p["CFBundleIdentifier"], "version": p["CFBundleShortVersionString"],
           "build": p["CFBundleVersion"], "title": p.get("CFBundleDisplayName") or p["CFBundleName"],
           "uploaded": int(time.time())}, open(sys.argv[2], "w"))
PY
  echo "ipa — $out/kasaterm.ipa ($(du -h "$out/kasaterm.ipa" | cut -f1))"
fi
[ "$mode" = build ] && exit 0

[ -f "$out/kasaterm.ipa" ] && [ -f "$out/meta.json" ] || { echo "올릴 ipa 가 없다 — 먼저 tool/adhoc.sh --no-publish" >&2; exit 1; }
token=$(python3 -c 'import secrets; print(secrets.token_urlsafe(24))')
up=".up-$token"
tooldir=.local/share/kasa-relay/adhoc
# 원격 기본 셸(zsh)은 빈 글롭에서 멈추므로 bash 로 돈다. 경로는 관문 기계의 홈 기준.
on_host() { if [ "$host" = local ]; then (cd "$HOME" && bash -s -- "$@"); else ssh -o ConnectTimeout=10 "$host" bash -s -- "$@"; fi; }
put() { local dest=$1; shift; if [ "$host" = local ]; then cp "$@" "$HOME/$dest/"; else scp -q "$@" "$host:$dest/"; fi; }
on_host "$remote/$up" "$tooldir" "$remote" <<'SH'
mkdir -p "$1" "$2" && chmod 700 "$3"
SH
put "$remote/$up" "$out/kasaterm.ipa" "$out/meta.json"
put "$tooldir" tool/asc.py tool/adhoc_sign.py
# 판 바꾸기는 디렉터리 이름 바꾸기 → latest 갈아 끼우기 순이라, 관문은 반쯤 올라간 판을 보지 않는다.
on_host "$remote" "$up" "$token" "$tooldir" <<'SH'
set -euo pipefail
shopt -s nullglob dotglob
signer=$HOME/$4/adhoc_sign.py
chmod +x "$signer"
cd "$1"
if [ -f "$HOME/.config/kasaterm/asc/adhoc/cert.json" ]; then
  "$signer" resign "$PWD/$2"
else
  echo "관문 기계에 서명 준비가 없어 이 맥의 서명 그대로 올린다 — 링크 자동 등록 기기는 못 깐다" >&2
fi
mv "$2" "$3"
prev=$(cat latest 2>/dev/null || true)
printf %s "$3" > latest.tmp && mv latest.tmp latest
for d in */; do
  d=${d%/}
  [ "$d" = "$3" ] || [ "$d" = "$prev" ] || rm -rf -- "$d"
done
SH

# 관문은 서울(네이버)에 있고 설치 창구만 미니에 남았다 — 폰 앱의 새 판 알림(latest)·관리 화면은 서울이 기기 토큰과
# 함께 보므로 판을 서울에도 놓는다(docs/seoul-gateway.md).
seoul_sync="$(cd "$(dirname "$0")/../.." && pwd)/tools/kasa-gateway-seoul/gateway.sh"
if [ "$host" != local ] && [ -x "$seoul_sync" ]; then
  KASA_MINI_SSH="$host" "$seoul_sync" sync-install >/dev/null \
    || echo "주의: 서울 관문에 판을 못 놓았다 — tools/kasa-gateway-seoul/gateway.sh sync-install" >&2
fi

url=$origin/relay/install/$token/
code=$(curl -s -o /dev/null -w '%{http_code}' "${url}manifest.plist")
[ "$code" = 200 ] || echo "주의: 관문이 manifest 를 $code 로 답했다 — 관문이 설치 창구를 아는 판인지 봐 달라" >&2
build=$(python3 -c 'import json,sys; m=json.load(open(sys.argv[1])); print(m["version"], m["build"])' "$out/meta.json")
echo "$url" > "$out/link"
echo "올렸다 — $build"
echo "설치 링크: $url"

if command -v kasaterm-cli >/dev/null; then
  dir=$(kasaterm-cli share new "iOS-설치-링크" 2>/dev/null || true)
  if [ -n "$dir" ] && [ -d "$dir" ]; then
    cat > "$dir/설치 링크.md" <<MD
# KASATERM $build

등록된 폰·아이패드의 Safari 에서 열고 「설치」를 누른다.

$url

다음 판을 올리면 이 링크는 하나 뒤 판까지만 남고 그다음엔 사라진다.
MD
    echo "KASA-share: $dir"
  fi
  [ "$show" = 1 ] && kasaterm-cli share open "$url"
fi
exit 0
