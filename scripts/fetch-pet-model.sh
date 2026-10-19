#!/usr/bin/env bash
# 바탕화면 펫이 쓸 캐릭터를 하나씩 받아 온다 — 레포에 담지 않는 이유가 라이선스다.
#
# Live2D 공식 캐릭터는 Free Material License Agreement 4.1.1 이 재배포를 금지한다.
# 그래서 kasaterm 레포에는 모델을 넣지 않고, 쓰는 사람이 자기 기계로 **권리자(Live2D
# 자신이 공개한 CubismWebSamples)에게서 직접** 받는다. 받는 것은 재배포가 아니다.
# 쓰기 전에 아래 약관에 동의한 것으로 본다:
#   https://www.live2d.com/eula/live2d-free-material-license-agreement_en.html
# 전부 Live2D Inc. 의 오리지널 캐릭터다 — 이름과 설정을 바꾸지 말 것.
#
# 기본 캐릭터 곽향(huohuo)은 모델 약관이 2차 배포를 금지해 앱 번들에도 못 싣는다. 원작자 BOOTH
# (https://bailyovo.booth.pm/items/5288339)에서 사람이 받은 zip 이나 그 주소·풀어 둔 폴더를
# KASATERM_PET_SOURCE 로 넘겨야 받는다. 앱은 펫 설정 폴더의 sources.json 에서 그 값을 읽어 넘긴다.
#
#   fetch-pet-model.sh [받을 폴더] [캐릭터…]     캐릭터를 안 적으면 huohuo
#   KASATERM_PET_SOURCE 가 있으면 어느 캐릭터든 그 자리(zip 주소·zip 파일·폴더)에서 들인다.
#
# 받는 중엔 `.downloading-<이름>.*` 임시 폴더에 받고, 다 받은 뒤에 옮긴다 — 반쯤 받은 폴더가
# 설치된 캐릭터로 보이면 안 된다. 이미 있는 폴더의 파일은 덮지 않는다. `current`(사람이 고른
# 캐릭터)는 쓰지 않는다.
# 표준 출력의 `@progress <받은 양> <전체>` 줄은 앱이 진행 막대로 읽는다(단위는 캐릭터마다 다르다).
#
# 종료 코드: 0 받음 · 1 네트워크·풀기 실패 · 3 받을 자리가 정해지지 않음
set -euo pipefail

DEST="${1:-$HOME/.config/kasaterm/pet}"
shift || true
CHARS=("$@")
[ ${#CHARS[@]} -gt 0 ] || CHARS=(huohuo)
REPO="Live2D/CubismWebSamples"
REF="develop"
ROOT="Samples/Resources"
SAMPLES=" Mao Hiyori Haru Natori Rice Ren Wanko Mark "

mkdir -p "$DEST"
TMP=""
trap '[ -z "$TMP" ] || rm -rf "$TMP"' EXIT

# 받은 폴더를 제자리로. 같은 이름 폴더가 이미 있으면(사람이 넣은 파일) 없는 파일만 채운다.
install_dir() {
  local src="$1" name="$2"
  ls "$src"/*.model3.json >/dev/null 2>&1 || { echo "$name — 받은 것에 model3.json 이 없다" >&2; return 1; }
  if [ -e "$DEST/$name" ]; then
    # macOS cp -n 은 건너뛴 파일이 있으면 1 로 끝난다 — 끝난 뒤 모델 파일로 판정한다.
    cp -Rn "$src/." "$DEST/$name/" || true
    ls "$DEST/$name"/*.model3.json >/dev/null 2>&1
  else
    mv "$src" "$DEST/$name"
  fi
}

fetch_sample() {
  local name="$1"
  [ -n "${TREE:-}" ] || TREE=$(curl -fsSL --max-time 60 \
    "https://api.github.com/repos/$REPO/git/trees/$REF?recursive=1")
  # 응답은 들여쓴 JSON 이라 path 와 type 이 다른 줄에 있다. awk 로 짝지어 blob 만 남긴다.
  local paths
  paths=$(printf '%s' "$TREE" | awk -v pre="$ROOT/$name/" '
    /"path"/ {
      match($0, /"path"[ ]*:[ ]*"[^"]*"/)
      s = substr($0, RSTART, RLENGTH)
      gsub(/^"path"[ ]*:[ ]*"|"$/, "", s)
      cur = s
    }
    /"type"[ ]*:[ ]*"blob"/ { if (index(cur, pre) == 1) print cur }
  ')
  [ -n "$paths" ] || { echo "$name — 목록에 없다" >&2; return 1; }
  local total n=0 rel
  total=$(printf '%s\n' "$paths" | wc -l | tr -d ' ')
  mkdir -p "$TMP/m"
  echo "@progress 0 $total"
  while IFS= read -r p; do
    rel="${p#"$ROOT/$name/"}"
    case "$rel" in */*) mkdir -p "$TMP/m/$(dirname "$rel")";; esac
    curl -fsSL --max-time 120 \
      "https://raw.githubusercontent.com/$REPO/$REF/$p" -o "$TMP/m/$rel"
    n=$((n + 1))
    echo "@progress $n $total"
  done <<< "$paths"
  install_dir "$TMP/m" "$name"
}

# 바이트 단위 진행 — curl 을 뒤에 돌리고 받은 크기를 읽는다.
fetch_archive() {
  local url="$1" out="$2" total size pid
  total=$(curl -fsSIL --max-time 30 "$url" 2>/dev/null \
    | awk 'tolower($1)=="content-length:" { n=$2 } END { gsub(/\r/, "", n); print n+0 }') || total=0
  curl -fsSL --max-time 900 -o "$out" "$url" & pid=$!
  while kill -0 "$pid" 2>/dev/null; do
    size=$(stat -f%z "$out" 2>/dev/null || stat -c%s "$out" 2>/dev/null || echo 0)
    echo "@progress $size $total"
    sleep 0.5
  done
  wait "$pid"
}

fetch_local() {
  local name="$1" src="${KASATERM_PET_SOURCE:-}"
  [ -n "$src" ] || { echo "$name — 받을 자리가 이 기기에 정해지지 않았다" >&2; return 3; }
  local from
  if [ -d "$src" ]; then
    from="$src"
  else
    if [ -f "$src" ]; then cp "$src" "$TMP/pkg.zip"; else fetch_archive "$src" "$TMP/pkg.zip"; fi
    mkdir -p "$TMP/x"
    unzip -q "$TMP/pkg.zip" -d "$TMP/x"
    from=$(find "$TMP/x" -maxdepth 3 -name '*.model3.json' -not -path '*/__MACOSX/*' | awk 'NR == 1')
    [ -n "$from" ] || { echo "$name — 묶음 안에 model3.json 이 없다" >&2; return 1; }
    from=$(dirname "$from")
  fi
  mkdir -p "$TMP/m"
  cp -R "$from/." "$TMP/m/"
  install_dir "$TMP/m" "$name"
}

for name in "${CHARS[@]}"; do
  if ls "$DEST/$name"/*.model3.json >/dev/null 2>&1; then
    echo "$name — 이미 있다" >&2
    continue
  fi
  TMP=$(mktemp -d "$DEST/.downloading-$name.XXXXXX")
  # 함수를 `||` 뒤에서 부르면 그 안의 set -e 가 꺼져 실패한 curl 을 지나친다 — 부속 셸로 돌린다.
  set +e
  (
    set -e
    # 받을 자리를 넘겼으면 그것이 먼저다 — 사람이 고른 파일로 들이는 길이다.
    if [ -n "${KASATERM_PET_SOURCE:-}" ]; then fetch_local "$name"
    else
      case "$SAMPLES" in
        *" $name "*) fetch_sample "$name" ;;
        *) fetch_local "$name" ;;
      esac
    fi
  )
  status=$?
  set -e
  rm -rf "$TMP"; TMP=""
  [ "$status" -eq 0 ] || exit "$status"
  echo "$name — 받음: $DEST/$name" >&2
done
