#!/usr/bin/env bash
# DMG 창 겉모습 — 배경과 아이콘 자리(assets/dmg)를 스테이징 폴더 맨 위에 놓는다. hdiutil -volname 은 kasaterm 이어야 한다:
# DS_Store 의 배경 별칭이 「kasaterm:.background.tiff」를 가리킨다. 그림은 scripts/installer-art.py, 자리는 dmgbuild 로 한 번
# 만든 DS_Store 다(창 660×400, 아이콘 112, 앱 (170,228)·Applications (490,228) — 배경의 화살표와 맞춘다).
# 같은 일을 미리보기 발행기는 tools/release/macsign.py stage_dmg_layout 으로 한다.
set -euo pipefail
stage="${1:?스테이징 폴더}"
root="$(cd "$(dirname "$0")/.." && pwd)"
cp "$root/assets/dmg/background.tiff" "$stage/.background.tiff"
cp "$root/assets/dmg/DS_Store" "$stage/.DS_Store"
