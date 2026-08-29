#!/usr/bin/env bash
# 맥북에서 구운 kasaterm 을 맥미니에 갈아입힌다.
#
# 미니는 스스로 못 따라온다. 이유가 둘이다: ①자기설치는 「새 판이 어디 있나」를
# 빌드 시각의 절대경로(`CARGO_MANIFEST_DIR`)로 기억하는데 미니엔 그 경로가 없다
# (/Users/kasa/… vs /Users/miku/…) ②미니엔 cargo 도 node 도 없어 거기서 굽지도
# 못한다. 그래서 구운 번들을 부쳐 주는 것 말고는 길이 없다.
#
# 도는 앱은 덮어쓸 수 없다 — 번들을 갈면 서명 페이지가 무효가 되어 macOS 가 앱을
# SIGKILL 한다. 그래서 반드시 세우고 갈아야 하고, 그 사이 학생이 죽지 않게 먼저
# 상주 데몬으로 승격시킨다(`promote`). 승격이 실패한 pane 도 잃지는 않는다 —
# 저장된 세션으로 `--resume` 되살아난다. 잠깐 끊길 뿐이다.
#
#   scripts/sync-mini.sh --dry-run   무엇을 할지만 보여준다
#   scripts/sync-mini.sh --canary    학생 하나만 승격하고 멈춘다(교체 안 함)
#   scripts/sync-mini.sh --no-promote  승격 없이 그냥 껐다 켠다
#   scripts/sync-mini.sh             전부: 부치기 → 승격 → 교체 → 되띄우기
set -euo pipefail

HOST="${KASATERM_MINI_HOST:-macmini-nacho}"
LABEL="com.geono.kasaterm"
STAGE="kasaterm-dist"          # 미니 홈 아래. 레포 밖에 둔다 — 레포를 더럽히면
                               # 세션 시작 훅이 「미커밋이 있다」며 pull 을 멈춘다.
DRY=0; CANARY=0; PROMOTE=1
for a in "$@"; do
  case "$a" in
    --dry-run) DRY=1 ;;
    --canary) CANARY=1 ;;
    --no-promote) PROMOTE=0 ;;
    *) echo "모르는 인자: $a" >&2; exit 2 ;;
  esac
done

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="$ROOT/dist/kasaterm.app"
say() { printf '[sync-mini] %s\n' "$*"; }
run() { if [ "$DRY" = 1 ]; then printf '  (안 함) %s\n' "$*"; else eval "$@"; fi; }
rmt() { ssh -o ConnectTimeout=10 "$HOST" "$1"; }

[ -x "$SRC/Contents/MacOS/kasaterm" ] || { echo "먼저 구워야 해요: bash scripts/build-app.sh" >&2; exit 1; }
rmt 'true' >/dev/null 2>&1 || { echo "$HOST 에 못 붙었어요" >&2; exit 1; }

# 이미 같은 판이면 아무것도 안 한다. 판정은 굽힌 시각(mtime)이다 — 버전 문자열은
# 태그를 올리기 전까지 수백 번 구워도 같은 값이라 판을 못 가른다.
LOCAL_TS=$(stat -f %m "$SRC/Contents/MacOS/kasaterm")
REMOTE_TS=$(rmt 'stat -f %m ~/Applications/kasaterm.app/Contents/MacOS/kasaterm 2>/dev/null || echo 0')
say "맥북 $(date -r "$LOCAL_TS" '+%m-%d %H:%M')  ·  미니 $( [ "$REMOTE_TS" = 0 ] && echo 없음 || date -r "$REMOTE_TS" '+%m-%d %H:%M')"
if [ "$LOCAL_TS" -le "$REMOTE_TS" ]; then say "미니가 이미 같거나 더 새것 — 할 일 없음"; exit 0; fi

say "부치는 중…"
run "rsync -a --delete -e ssh '$SRC/' '$HOST:$STAGE/kasaterm.app/'"

# 미니 앱의 조종 소켓은 인스턴스마다 다르다(`/tmp/cmux.sock` 은 미니에 없다).
# 도는 pid 에서 찾아 쓴다 — pid 는 교체 뒤 바뀌므로 매번 다시 찾는다.
FIND_SOCK='P=$(launchctl list | awk "/'"$LABEL"'/{print \$1}"); [ -n "$P" ] && [ "$P" != "-" ] && lsof -p $P 2>/dev/null | grep -o "/var/folders/[^ ]*kasaterm-$P.sock" | head -1'
CLI='~/Applications/kasaterm.app/Contents/MacOS/kasaterm-cli'

if [ "$PROMOTE" = 1 ]; then
  PANES=$(rmt "S=\$($FIND_SOCK); [ -n \"\$S\" ] && KASATERM_SOCKET_PATH=\"\$S\" $CLI board 2>/dev/null | python3 -c 'import json,sys; d=json.load(sys.stdin); print(\" \".join(p[\"surface_id\"] for p in d[\"result\"][\"board\"] if p.get(\"harness\")==\"claude\"))'" || true)
  say "학생: ${PANES:-없음}"
  [ "$CANARY" = 1 ] && PANES=$(echo "$PANES" | awk '{print $1}')
  for p in $PANES; do
    printf '  승격 %s → ' "$p"
    if [ "$DRY" = 1 ]; then echo "(안 함)"; else
      rmt "S=\$($FIND_SOCK); KASATERM_SOCKET_PATH=\"\$S\" $CLI promote '$p' 2>&1 | tail -1" | cut -c1-120
    fi
  done
  if [ "$CANARY" = 1 ]; then
    say "카나리아까지만 — 교체는 안 했어요. 그 학생이 멀쩡하면 인자 없이 다시 부르세요."
    exit 0
  fi
fi

say "세우고 · 갈아입히고 · 되띄우는 중…"
# 넷을 한 번의 ssh 안에서 한다. 왕복마다 나가면 세운 뒤 갈아입기 전에 launchd 가
# 먼저 되띄울 틈이 생기고, 그 상태로 번들을 갈면 도는 앱의 서명이 무효가 되어
# macOS 가 앱을 SIGKILL 한다 — 사람 눈엔 「업데이트했더니 앱이 안 뜬다」로 보인다.
SWAP=$(cat <<'REMOTE'
set -e
U=$(id -u); L=com.geono.kasaterm
RUN=~/Applications/kasaterm.app/Contents/MacOS/kasaterm
test -x ~/kasaterm-dist/kasaterm.app/Contents/MacOS/kasaterm || { echo "부쳐진 판이 없어요"; exit 1; }
launchctl bootout "gui/$U/$L" 2>/dev/null || true
for _ in $(seq 1 40); do pgrep -f "$RUN" >/dev/null || break; sleep 0.5; done
if pgrep -f "$RUN" >/dev/null; then
  launchctl bootstrap "gui/$U" ~/Library/LaunchAgents/$L.plist 2>/dev/null || true
  echo "앱이 20초 안에 안 꺼져 교체를 세웠어요 (되띄웠습니다)"; exit 1
fi
rm -rf ~/Applications/kasaterm.app
cp -R ~/kasaterm-dist/kasaterm.app ~/Applications/
touch ~/Applications/kasaterm.app
launchctl bootstrap "gui/$U" ~/Library/LaunchAgents/$L.plist
echo ok
REMOTE
)
run "rmt \"\$SWAP\""

if [ "$DRY" = 0 ]; then
  sleep 6
  NEW=$(rmt 'stat -f %m ~/Applications/kasaterm.app/Contents/MacOS/kasaterm')
  UP=$(rmt "pgrep -f 'Applications/kasaterm.app/Contents/MacOS/kasaterm' | head -1")
  say "미니 판 $(date -r "$NEW" '+%m-%d %H:%M')  ·  pid ${UP:-안 뜸}"
  [ -n "$UP" ] || { echo "앱이 다시 안 떴어요 — 미니에서 launchctl 로그(/tmp/kasaterm-agent.log)를 보세요" >&2; exit 1; }
  rmt "sleep 4; S=\$($FIND_SOCK); [ -n \"\$S\" ] && KASATERM_SOCKET_PATH=\"\$S\" $CLI board 2>/dev/null | python3 -c 'import json,sys; d=json.load(sys.stdin); b=d[\"result\"][\"board\"]; print(\"[sync-mini] 돌아온 학생 %d명: %s\" % (len(b), \", \".join(p.get(\"character\") or \"?\" for p in b)))'" || say "학생 확인은 못 했어요(앱이 아직 뜨는 중일 수 있어요)"
fi
