#!/usr/bin/env bash
# 다른 기계(맥미니)의 학생이 이 맥북에서 굽게 하는 창구 — 터널 관문 너머에서 온다.
#
# 맥미니 → 맥북은 나쵸 역터널(localhost:2222)뿐이고, 그 열쇠는 `~/.local/bin/nacho-tunnel-guard`
# 가 argv[0] 이 `kasaterm-remote` 일 때만 셸 없이 execv 로 통과시킨다. 그래서 굽기도 그 창구의
# 한 동사다: `kasaterm-remote bake <동사>` → 이 파일. 여기서도 동사는 아래 고정 목록과만
# 대조하고 인자를 셸에 넘기지 않는다 — 관문이 지키는 「임의 명령 실행 금지」를 여기서 열면 안 된다
# (2026-09-17 「맥미니에서 작업하고 알아서 맥북 접근해서 굽게 하자」).
#
#   status   지금 판(HEAD·미커밋·dist/설치본 시각·펫·서비스)
#   pull     git pull --ff-only 만
#   app      pull → build-app.sh (앱 전체; 반영은 사람이 앱을 껐다 켜야 한다)
#   pet      pull → pet-reload.sh (펫만, 즉시 반영)
#   journal  pull → request-journal 서비스 재시작(펫의 뇌·나쵸 말투·집컴 전원)
#   log      최근 기록(이 스크립트·펫 띄우기·펫의 뇌) 꼬리 — 실패 원인을 터널 너머에서 본다
#   --force  app 에서 다른 pane 이 Rust 를 만지는 중이어도 강행
#
# ⚠️전체를 main() 에 넣고 맨 끝에서 부른다 — pull 이 이 파일 자체를 바꾸는데, bash 는 스크립트를
# 읽어 가며 실행하므로 도중에 파일이 바뀌면 옛 줄과 새 줄이 섞여 돈다(2026-09-17 실측: pull 뒤
# status 가 옛 코드로 돌았다). 함수 하나로 감싸면 실행 전에 전부 읽는다.
#
# ssh 세션엔 로그인 셸 PATH 가 없다 — cargo·brew·kasaterm-cli 를 여기서 채운다. 굽기 가드가
# board 를 읽으므로 살아 있는 앱 소켓도 골라 준다. 기록은 ~/.local/state/remote-bake.log.
set -uo pipefail
main() {
REPO="${KASATERM_REPO:-$(cd "$(dirname "$0")/.." && pwd)}"
cd "$REPO" || { echo "[remote-bake] 레포가 없다: $REPO" >&2; exit 1; }
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$HOME/.local/bin:$HOME/bin:$HOME/Applications/kasaterm.app/Contents/MacOS:$PATH"
if [[ -z "${KASATERM_SOCKET_PATH:-}" ]]; then
  tmp="$(getconf DARWIN_USER_TEMP_DIR 2>/dev/null || echo /tmp/)"
  newest="$(ls -t "${tmp}"kasaterm-*.sock 2>/dev/null | head -1 || true)"
  [[ -n "$newest" ]] && export KASATERM_SOCKET_PATH="$newest"
fi
LOG="$HOME/.local/state/remote-bake.log"
mkdir -p "$(dirname "$LOG")"

VERB="${1:-status}"; [[ $# -gt 0 ]] && shift
FORCE=()
for a in "$@"; do
  case "$a" in
    --force) FORCE=(--force) ;;
    *) echo "[remote-bake] 모르는 옵션: $a" >&2; exit 2 ;;
  esac
done
case "$VERB" in status|pull|app|pet|journal|log) ;; *)
  echo "[remote-bake] 동사는 status·pull·app·pet·journal·log 중 하나다 (받은 것: $VERB)" >&2; exit 2 ;;
esac

say() { printf '[remote-bake] %s\n' "$*"; }
mtime() { [[ -e "$1" ]] && stat -f '%Sm' -t '%m-%d %H:%M' "$1" || echo "없음"; }

pull() {
  say "HEAD $(git rev-parse --short HEAD) ($(git branch --show-current)) → git pull --ff-only origin main"
  if ! git pull --ff-only origin main 2>&1 | tail -2; then
    say "pull 실패 — 워킹트리에 충돌하는 변경이 있거나 갈라진 커밋이 있다. 맥북 pane 에서 손봐야 한다"
    return 1
  fi
  say "HEAD $(git rev-parse --short HEAD)"
}

status() {
  say "기계 $(hostname) · 레포 $REPO"
  say "HEAD $(git log --oneline -1)"
  local dirty; dirty="$(git status --short | head -5)"
  [[ -n "$dirty" ]] && say "미커밋:"$'\n'"$dirty" || say "미커밋 없음"
  say "dist 번들   $(mtime dist/kasaterm.app/Contents/MacOS/kasaterm)"
  say "설치본      $(mtime "$HOME/Applications/kasaterm.app/Contents/MacOS/kasaterm")"
  say "설치본 펫   $(mtime "$HOME/Applications/kasaterm.app/Contents/Resources/kasapet")"
  local pid=""; [[ -f "$HOME/.config/kasaterm/pet.pid" ]] && pid="$(tr -dc 0-9 < "$HOME/.config/kasaterm/pet.pid")"
  if [[ -n "$pid" ]] && ps -p "$pid" >/dev/null 2>&1; then say "펫 도는 중(pid $pid)"; else say "펫 꺼짐"; fi
  local health; health="$(curl -s --max-time 3 "$(python3 -c 'import json;print(json.load(open("'"$HOME"'/.config/kasaterm/request-journal/service.json"))["base_url"])' 2>/dev/null)/health" 2>/dev/null | head -c 80 || true)"
  say "펫의 뇌(request-journal) ${health:-응답 없음}"
}

# ssh 세션은 로그인 세션이 아니라 키체인의 애플 인증서 키를 못 쓴다 — build-app.sh 의 최종 서명이
# `errSecInternalComponent` 로 죽는다(2026-09-17 실측; 펫 하나 서명은 됐는데 번들은 안 됐다). 그래서
# 굽기는 도는 앱의 탭 하나를 빌려 그 셸(로그인 세션)에서 돌리고, 끝 표식을 화면에서 읽는다.
# 탭 셸은 KASATERM_PANE_ID 를 갖고 있어 pane 가드가 자기 자신을 빼고 판정한다.
tab_run() {
  local cmd="$1" marker="REMOTE_BAKE_RC" outer tab text rc
  command -v kasaterm-cli >/dev/null 2>&1 || { say "kasaterm-cli 가 없다 — 앱 탭을 못 빌린다"; return 1; }
  outer="$(kasaterm-cli board 2>/dev/null | python3 -c 'import json,sys
b=(json.load(sys.stdin).get("result") or {}).get("board") or []
print(b[0].get("surface_id","") if b else "")' 2>/dev/null || true)"
  tab="$(kasaterm-cli tab ${outer:+"$outer"} 2>/dev/null | python3 -c 'import json,sys
print((((json.load(sys.stdin).get("result") or {}).get("surface") or {}).get("id")) or "")' 2>/dev/null || true)"
  [[ -n "$tab" ]] || { say "앱 탭을 못 열었다 — 맥북 앱이 꺼져 있나"; return 1; }
  say "앱 탭 $tab 에서 돌린다: $cmd"
  kasaterm-cli send --surface "$tab" "cd '$REPO' && $cmd; echo $marker=\$?"$'\n' >/dev/null
  for _ in $(seq 1 1440); do
    sleep 0.5
    text="$(kasaterm-cli peek "$tab" 2>/dev/null | python3 -c 'import json,sys
print(((json.load(sys.stdin).get("result") or {}).get("text")) or "")' 2>/dev/null || true)"
    if [[ "$text" == *"$marker="* ]]; then
      printf '%s\n' "$text" | grep -vE '^\s*$' | tail -14
      rc="$(printf '%s\n' "$text" | sed -n "s/.*$marker=\([0-9]*\).*/\1/p" | tail -1)"
      kasaterm-cli dismiss "$tab" >/dev/null 2>&1 || true
      return "${rc:-1}"
    fi
  done
  say "탭 $tab 에서 12분 안에 안 끝났다 — 탭은 그대로 둔다"
  return 1
}

logs() {
  local tmp; tmp="$(getconf DARWIN_USER_TEMP_DIR 2>/dev/null || echo /tmp/)"
  for f in "$LOG" "${tmp}kasapet-reload.log" "$HOME/.config/kasaterm/request-journal/service.log"; do
    say "── $f"
    [[ -f "$f" ]] && tail -n 15 "$f" | cut -c1-200 || say "(없음)"
  done
}

run() {
  case "$VERB" in
    status) status ;;
    log) logs ;;
    pull) pull ;;
    app)
      pull || return 1
      say "build-app.sh ${FORCE[*]:-} 시작 — 몇 분 걸린다"
      if [[ -n "${SSH_CONNECTION:-}" ]]; then
        tab_run "bash scripts/build-app.sh ${FORCE[*]:-}" || return 1
      else
        bash scripts/build-app.sh ${FORCE[@]+"${FORCE[@]}"} || return 1
      fi
      say "구움. 반영은 앱을 껐다 켜야 한다(자기설치). 펫까지 바꿨으면 그 뒤 펫도 껐다 켠다" ;;
    pet)
      pull || return 1
      bash scripts/pet-reload.sh ;;
    journal)
      pull || return 1
      launchctl kickstart -k "gui/$(id -u)/com.kasaterm.request-journal" || return 1
      sleep 3
      status | grep '펫의 뇌' ;;
  esac
}

{ say "── $(date '+%m-%d %H:%M:%S') $VERB ${FORCE[*]:-} (from ${SSH_CONNECTION%% *})"; run; rc=$?; say "끝 rc=$rc"; exit $rc; } 2>&1 | tee -a "$LOG"
exit "${PIPESTATUS[0]}"
}
main "$@"
