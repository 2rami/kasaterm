#!/usr/bin/env bash
# 바탕화면 펫이 쓸 캐릭터를 받아 온다 — 레포에 담지 않는 이유가 라이선스다.
#
# Live2D 공식 캐릭터는 Free Material License Agreement 4.1.1 이 재배포를 금지한다.
# 그래서 kasaterm 레포에는 모델을 넣지 않고, 쓰는 사람이 자기 기계로 **권리자(Live2D
# 자신이 공개한 CubismWebSamples)에게서 직접** 받는다. 받는 것은 재배포가 아니다.
# 쓰기 전에 아래 약관에 동의한 것으로 본다:
#   https://www.live2d.com/eula/live2d-free-material-license-agreement_en.html
#
# 전부 Live2D Inc. 의 오리지널 캐릭터다 — 이름과 설정을 바꾸지 말 것.
#
#   fetch-pet-model.sh [받을 폴더] [캐릭터…]
#     캐릭터를 안 적으면 여덟 전부(24MB). 하나만 받으려면 이름을 적는다.
set -euo pipefail

DEST="${1:-$HOME/.config/kasaterm/pet}"
shift || true
REPO="Live2D/CubismWebSamples"
REF="develop"
ROOT="Samples/Resources"
# 기본 캐릭터가 마오인 이유는 표정·모션이 가장 많이 딸려 있어서다(Open-LLM-VTuber 도
# 같은 이유로 시즈쿠에서 갈아탔다). 목록의 첫 칸이 처음 켤 때 뜬다.
CHARS=("$@")
[ ${#CHARS[@]} -gt 0 ] || CHARS=(Mao Hiyori Haru Natori Rice Ren Wanko Mark)

mkdir -p "$DEST"

# 트리 API 로 목록을 한 번에 받고 raw 로 하나씩 내려받는다. 레포 tarball 은 100MB 가
# 넘어 모델 몇 개 받자고 끌어올 물건이 아니다.
tree=$(curl -fsSL --max-time 60 \
  "https://api.github.com/repos/$REPO/git/trees/$REF?recursive=1")

got=0
for name in "${CHARS[@]}"; do
  if ls "$DEST/$name"/*.model3.json >/dev/null 2>&1; then
    echo "$name — 이미 있다"
    got=$((got + 1))
    continue
  fi
  # 응답은 들여쓴 JSON 이라 path 와 type 이 다른 줄에 있다. awk 로 짝지어 blob 만 남긴다.
  paths=$(printf '%s' "$tree" | awk -v pre="$ROOT/$name/" '
    /"path"/ {
      match($0, /"path"[ ]*:[ ]*"[^"]*"/)
      s = substr($0, RSTART, RLENGTH)
      gsub(/^"path"[ ]*:[ ]*"|"$/, "", s)
      cur = s
    }
    /"type"[ ]*:[ ]*"blob"/ { if (index(cur, pre) == 1) print cur }
  ')
  if [ -z "$paths" ]; then
    echo "$name — 목록에 없다, 건너뛴다" >&2
    continue
  fi
  n=0
  while IFS= read -r p; do
    rel="${p#"$ROOT/$name/"}"
    case "$rel" in */*) mkdir -p "$DEST/$name/$(dirname "$rel")";; esac
    curl -fsSL --max-time 120 \
      "https://raw.githubusercontent.com/$REPO/$REF/$p" -o "$DEST/$name/$rel"
    n=$((n + 1))
  done <<< "$paths"
  echo "$name — $n 개"
  got=$((got + 1))
done

[ "$got" -gt 0 ] || { echo "하나도 못 받았다 — 네트워크를 확인해라" >&2; exit 1; }

# 지금 띄울 캐릭터. 이미 골라 둔 것이 있으면 안 덮는다 — 두 번째 실행이 사람의 선택을
# 되돌리면 안 된다.
[ -f "$DEST/current" ] || printf '%s' "${CHARS[0]}" > "$DEST/current"
echo "받은 곳: $DEST · 지금 캐릭터: $(cat "$DEST/current")"
