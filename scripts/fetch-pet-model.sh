#!/usr/bin/env bash
# 바탕화면 펫이 쓸 기본 모델을 받아 온다 — 레포에 담지 않는 이유가 라이선스다.
#
# Live2D 공식 캐릭터(마오·시즈쿠 등)는 Free Material License Agreement 4.1.1 이
# 재배포를 금지한다. 그래서 kasaterm 레포에는 모델을 넣지 않고, 쓰는 사람이 자기
# 기계로 **권리자(Live2D 자신의 GitHub)에게서 직접** 받는다. 받는 것은 재배포가
# 아니다. 쓰기 전에 아래 약관에 동의한 것으로 본다:
#   https://www.live2d.com/eula/live2d-free-material-license-agreement_en.html
#
# 마오(Niziiro Mao)는 Live2D Inc. 의 오리지널 캐릭터다. 이름과 설정을 바꾸지 말 것.
set -euo pipefail

DEST="${1:-$HOME/.config/kasaterm/pet}"
REPO="Live2D/CubismWebSamples"
REF="develop"
SUB="Samples/Resources/Mao"

if ls "$DEST"/*.model3.json >/dev/null 2>&1; then
  echo "이미 있다: $DEST"
  exit 0
fi

echo "마오 모델을 받는다 → $DEST"
mkdir -p "$DEST"

# 트리 API 로 파일 목록을 한 번에 받고 raw 로 하나씩 내려받는다. 레포 tarball 은
# 100MB 가 넘어 모델 하나 받자고 끌어올 물건이 아니다.
tree=$(curl -fsSL --max-time 60 \
  "https://api.github.com/repos/$REPO/git/trees/$REF?recursive=1")

# 응답은 들여쓴 JSON 이라 path 와 type 이 다른 줄에 있다. awk 로 짝지어 blob 만 남긴다.
paths=$(printf '%s' "$tree" | awk -v pre="$SUB/" '
  /"path"/ {
    match($0, /"path"[ ]*:[ ]*"[^"]*"/)
    s = substr($0, RSTART, RLENGTH)
    gsub(/^"path"[ ]*:[ ]*"|"$/, "", s)
    cur = s
  }
  /"type"[ ]*:[ ]*"blob"/ { if (index(cur, pre) == 1) print cur }
')

if [ -z "$paths" ]; then
  echo "목록을 못 받았다 — 네트워크나 경로($SUB)를 확인해라" >&2
  exit 1
fi

n=0
while IFS= read -r p; do
  rel="${p#"$SUB/"}"
  case "$rel" in */*) mkdir -p "$DEST/$(dirname "$rel")";; esac
  curl -fsSL --max-time 120 \
    "https://raw.githubusercontent.com/$REPO/$REF/$p" -o "$DEST/$rel"
  n=$((n + 1))
done <<< "$paths"

echo "$n 개 받았다: $DEST"
ls "$DEST"/*.model3.json
