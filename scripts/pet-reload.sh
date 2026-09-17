#!/usr/bin/env bash
# 펫(kasapet)만 새로 굽고 갈아 끼운다 — 앱 전체 굽기(build-app.sh)도, 앱 껐다 켜기도 없이.
#
# 펫은 앱 번들 Resources 에 든 별개 실행 파일이고 앱과 수명이 다르다(앱을 꺼도 살아 있고,
# 앱을 다시 켜도 도는 펫을 안 바꾼다). 그래서 펫 코드만 고쳤을 때 앱 전체를 굽고 앱을
# 껐다 켜고 펫도 껐다 켜는 세 단계는 낭비다(2026-09-17 「펫 굽는 플로우 따로 쉽게 하자」).
# 여기서는 ① kasapet 만 빌드 ② 설치본·dist 번들의 Resources/kasapet 을 바꿔 끼우고 서명
# ③ 도는 펫을 pid 파일로 내리고 새 파일로 다시 띄운다.
#
#   scripts/pet-reload.sh            # release 로 굽는다(체감은 release 가 기준이다)
#   scripts/pet-reload.sh --debug    # 디버그 빌드로. 빠르지만 Live2D 가 버벅인다
#   scripts/pet-reload.sh --no-build # 이미 빌드한 파일만 갈아 끼운다
#
# ⚠️설치본의 앱 본체(Contents/MacOS/kasaterm)는 건드리지 않는다 — 도는 실행 파일의 서명을
# 다시 쓰면 그 앱이 죽는다. 그래서 설치본 번들의 봉인(CodeResources)은 다음 build-app.sh
# 까지 옛 kasapet 해시를 가리킨다. 로컬 빌드(격리 속성 없음)라 실행엔 지장이 없고, dist 쪽은
# 도는 것이 아니라 통째로 다시 봉인해 둔다 — 다음에 앱을 껐다 켜며 자기 설치가 돌아도
# 옛 펫으로 되돌아가지 않는다.
# ⚠️pkill·killall 은 안 쓴다. pid 파일의 그 번호가 kasapet 인지 확인하고 그것만 내린다.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

PROFILE=release
BUILD=1
for a in "$@"; do
  case "$a" in
    --debug) PROFILE=debug ;;
    --no-build) BUILD=0 ;;
    *) echo "모르는 옵션: $a" >&2; exit 2 ;;
  esac
done

if [[ $BUILD = 1 ]]; then
  if [[ $PROFILE = release ]]; then cargo build --release -p kasapet; else cargo build -p kasapet; fi
fi
BIN="target/$PROFILE/kasapet"
[[ -x "$BIN" ]] || { echo "[pet] $BIN 이 없다 — 먼저 빌드해라" >&2; exit 1; }

# KASATERM_APP 은 검증용 — 스크래치에 만든 가짜 번들을 가리켜 사람 설치본을 안 건드린다.
INSTALLED="${KASATERM_APP:-$HOME/Applications/kasaterm.app}"
DIST="${KASATERM_DIST:-$ROOT/dist/kasaterm.app}"
[[ -d "$INSTALLED" ]] || { echo "[pet] 설치본이 없다: $INSTALLED (앱을 한 번 설치한 뒤에 쓴다)" >&2; exit 1; }

# 서명 주체는 설치본이 이미 쓰는 것 그대로 — build-app.sh 가 고른 애플 인증서다. 키체인에
# 없으면(다른 기계) ad-hoc 으로 내려간다. 펫은 앱이 낳는 자식이라 알림센터 같은 권한은
# 앱 쪽 서명이 쥐고 있어, 펫 자체는 어느 쪽이든 뜬다.
AUTH="$(codesign -dvv "$INSTALLED" 2>&1 | sed -n 's/^Authority=//p' | head -1 || true)"
sign() {
  if [[ -n "$AUTH" ]] && codesign --force --sign "$AUTH" "$1" 2>/dev/null; then return; fi
  codesign --force --sign - "$1"
}

for app in "$DIST" "$INSTALLED"; do
  [[ -d "$app" ]] || continue
  cp "$BIN" "$app/Contents/Resources/kasapet"
  cp "assets/fonts/Maplestory Bold.ttf" "$app/Contents/Resources/"
  sign "$app/Contents/Resources/kasapet"
done
# dist 는 도는 것이 아니니 번들째 다시 봉인한다(중첩 프레임워크 서명은 그대로 둔다).
[[ -d "$DIST" ]] && sign "$DIST"
echo "[pet] 갈아 끼움: ${AUTH:-ad-hoc}"

# 앱과 같은 규칙: KASATERM_PET_DIR 이 있으면 그 폴더가 모델 폴더이자 pid 자리, 없으면
# ~/.config/kasaterm/pet 과 ~/.config/kasaterm/pet.pid.
if [[ -n "${KASATERM_PET_DIR:-}" ]]; then MODELDIR="$KASATERM_PET_DIR"; PIDFILE="$KASATERM_PET_DIR/pet.pid"
else MODELDIR="$HOME/.config/kasaterm/pet"; PIDFILE="$HOME/.config/kasaterm/pet.pid"; fi
MODEL="${KASATERM_PET_MODEL:-}"
if [[ -z "$MODEL" ]]; then
  NAME="$(cat "$MODELDIR/current" 2>/dev/null || true)"
  MODEL="$(ls "$MODELDIR/${NAME:-x}"/*.model3.json 2>/dev/null | head -1 || true)"
  [[ -n "$MODEL" ]] || MODEL="$(ls "$MODELDIR"/*/*.model3.json 2>/dev/null | head -1 || true)"
fi
[[ -n "$MODEL" ]] || { echo "[pet] 띄울 모델이 없다 — scripts/fetch-pet-model.sh 로 받아 둬라" >&2; exit 1; }

if [[ -f "$PIDFILE" ]]; then
  PID="$(tr -dc 0-9 < "$PIDFILE" || true)"
  if [[ -n "$PID" ]] && ps -o comm= -p "$PID" 2>/dev/null | grep -q kasapet; then
    kill "$PID"
    for _ in $(seq 1 25); do ps -p "$PID" >/dev/null 2>&1 || break; sleep 0.2; done
    echo "[pet] 옛 펫($PID) 내림"
  fi
  rm -f "$PIDFILE"
fi

LOG="${TMPDIR:-/tmp}/kasapet-reload.log"
# 클로드 pane 에서 불러도 그 표식을 펫에 물리지 않는다.
env -u CLAUDE_CODE_CHILD_SESSION -u TEAMMATE_MODE -u SESSION_ID \
  nohup "$INSTALLED/Contents/Resources/kasapet" "$MODEL" >"$LOG" 2>&1 &
echo $! > "$PIDFILE"
disown
sleep 1
if ps -p "$(cat "$PIDFILE")" >/dev/null 2>&1; then
  echo "[pet] 새 펫 띄움(pid $(cat "$PIDFILE")) — $MODEL · 로그 $LOG"
else
  echo "[pet] 새 펫이 바로 죽었다 — $LOG 를 봐라" >&2; tail -20 "$LOG" >&2; exit 1
fi
