#!/bin/bash
# 미니의 읽기 전용 LFS 창구(tools/release/mini_lfs.py)를 깔고 채운다 — CI Windows 굽기가 GitHub LFS 대신 여기서 그림을 받는다.
#
#   install <공개 주소>  서버 복사 · 객체 동기화 · 토큰(없을 때만 생성) · launchd 상주. 주소는 한 번 주면 기억한다
#   sync                 이 저장소 LFS 객체 중 창구에 없는 것만 복제 — 새 그림이 든 판을 CI 가 굽기 전에
#   status               상주·무토큰 401·토큰 200 확인(토큰은 어디에도 찍지 않는다)
#   daemon               sudo 로. 로그인 없이 재부팅해도 뜨게 LaunchDaemon 으로 옮긴다
#
# 서버와 객체는 ~/.local/share/kasaterm-lfs 에 둔다 — launchd 가 부르는 파이썬은 ~/Desktop 을 못 읽는다(TCC).
# 토큰은 ~/.config/kasaterm-lfs/token(600) 에만 있다. GitHub 에는 파이프로 넣는다:
#   gh secret set MINI_LFS_TOKEN -R 2rami/kasaterm < ~/.config/kasaterm-lfs/token
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LABEL=com.geono.kasaterm-lfs
PORT="${KASATERM_LFS_PORT:-8794}"
PY=/usr/bin/python3
OWNER="${SUDO_USER:-$(id -un)}"
OWNER_HOME="$(dscl . -read "/Users/$OWNER" NFSHomeDirectory | awk '{print $2}')"
BASE="$OWNER_HOME/.local/share/kasaterm-lfs"
SERVER="$BASE/mini_lfs.py"
STORE="$BASE/objects"
CONF="$OWNER_HOME/.config/kasaterm-lfs"
TOKEN="$CONF/token"
URLFILE="$CONF/public-url"
LOG="$OWNER_HOME/Library/Logs/kasaterm-lfs.log"
AGENT="$OWNER_HOME/Library/LaunchAgents/$LABEL.plist"
DAEMON="/Library/LaunchDaemons/$LABEL.plist"

die() { echo "mini-lfs: $*" >&2; exit 1; }

# 이름으로 죽이지 않는다(pkill 은 그 경로를 인자로 든 남의 셸까지 잡는다) — 이 포트를 듣는 우리 서버만 고른다.
server_pid() {
  local pid
  for pid in $(lsof -t -nP -iTCP:"$PORT" -sTCP:LISTEN 2>/dev/null); do
    ps -o command= -p "$pid" | grep -qF "$SERVER" && echo "$pid"
  done
  return 0
}

public_url() { [[ -s "$URLFILE" ]] || die "공개 주소가 없다 — install <주소> 로 한 번 준다"; cat "$URLFILE"; }
prefix() { local u; u="$(public_url)"; u="${u#*://}"; echo "/${u#*/}" | sed 's:/*$::'; }

sync_objects() {
  local src f rel dst tmp added=0
  src="$(git -C "$ROOT" lfs env 2>/dev/null | sed -n 's/^LocalMediaDir=//p')"
  [[ -d "$src" ]] || die "이 저장소의 LFS 객체 폴더를 못 찾았다($ROOT)"
  mkdir -p "$STORE"
  while IFS= read -r f; do
    rel="${f#"$src"/}"
    dst="$STORE/$rel"
    [[ -e "$dst" ]] && continue
    mkdir -p "$(dirname "$dst")"
    tmp="$dst.part.$$"
    cp -c "$f" "$tmp" 2>/dev/null || cp "$f" "$tmp"
    mv "$tmp" "$dst"
    added=$((added + 1))
  done < <(find "$src" -type f -name '[0-9a-f]*' | grep -E '/[0-9a-f]{64}$')
  echo "창구 객체 $(find "$STORE" -type f | grep -cE '/[0-9a-f]{64}$')개 (이번에 ${added}개 추가)"
}

write_plist() {
  local out="$1" user_key="$2"
  cat > "$out" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key>
  <array>
    <string>$PY</string>
    <string>$SERVER</string>
    <string>--store</string><string>$STORE</string>
    <string>--token-file</string><string>$TOKEN</string>
    <string>--public-url</string><string>$(public_url)</string>
    <string>--port</string><string>$PORT</string>
  </array>
$user_key  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ThrottleInterval</key><integer>10</integer>
  <key>StandardOutPath</key><string>$LOG</string>
  <key>StandardErrorPath</key><string>$LOG</string>
</dict>
</plist>
PLIST
  plutil -lint "$out" >/dev/null
}

probe() {
  local url="http://127.0.0.1:$PORT$(prefix)/objects/batch" body='{"operation":"download","transfers":["basic"],"objects":[]}'
  local anon authed
  anon="$(curl -s -o /dev/null -w '%{http_code}' -m 5 -X POST -H 'Content-Type: application/vnd.git-lfs+json' -d "$body" "$url" || true)"
  authed="$(curl -s -o /dev/null -w '%{http_code}' -m 5 -X POST -H 'Content-Type: application/vnd.git-lfs+json' \
    -H @<(printf 'Authorization: Bearer %s\n' "$(cat "$TOKEN")") -d "$body" "$url" || true)"
  echo "무토큰 $anon · 토큰 $authed"
  [[ "$anon" == 401 && "$authed" == 200 ]]
}

wait_up() {
  for _ in $(seq 1 20); do probe >/dev/null 2>&1 && { probe; return 0; }; sleep 0.5; done
  probe || die "창구가 안 떴다 — $LOG"
}

cmd_install() {
  if [[ -n "${1:-}" ]]; then
    [[ "$1" =~ ^https?://[^/]+/.+ ]] || die "주소는 경로가 붙은 http(s) 주소여야 한다(예: https://host/lfs)"
    mkdir -p "$CONF"; chmod 700 "$CONF"
    printf '%s\n' "$1" > "$URLFILE"
  fi
  public_url >/dev/null
  mkdir -p "$BASE" "$CONF" "$(dirname "$LOG")"
  chmod 700 "$CONF"
  cp "$ROOT/tools/release/mini_lfs.py" "$SERVER.new" && mv "$SERVER.new" "$SERVER"
  sync_objects
  if [[ ! -s "$TOKEN" ]]; then
    (umask 077; "$PY" -c 'import secrets,sys; sys.stdout.write(secrets.token_urlsafe(48))' > "$TOKEN")
    echo "토큰을 새로 만들었다 — GitHub 에 넣는다: gh secret set MINI_LFS_TOKEN -R 2rami/kasaterm < $TOKEN"
  fi
  chmod 600 "$TOKEN"
  if [[ -f "$DAEMON" ]]; then
    # 데몬은 이 사용자로 돈다 — 제 프로세스를 끊으면 KeepAlive 가 새 파일로 다시 띄운다(sudo 불필요).
    for pid in $(server_pid); do kill "$pid"; done
  else
    mkdir -p "$(dirname "$AGENT")"
    write_plist "$AGENT" ""
    launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
    launchctl bootstrap "gui/$(id -u)" "$AGENT"
  fi
  wait_up
}

cmd_daemon() {
  [[ $EUID -eq 0 && -n "${SUDO_USER:-}" ]] || die "sudo bash $0 daemon 으로 실행한다"
  [[ -s "$SERVER" && -s "$TOKEN" ]] || die "먼저 sudo 없이 install 한다"
  write_plist "$DAEMON" "  <key>UserName</key><string>$OWNER</string>
"
  chown root:wheel "$DAEMON"; chmod 644 "$DAEMON"
  launchctl bootout "gui/$(id -u "$OWNER")/$LABEL" 2>/dev/null || true
  [[ -f "$AGENT" ]] && mv "$AGENT" "$AGENT.replaced-by-daemon"
  launchctl bootout "system/$LABEL" 2>/dev/null || true
  launchctl bootstrap system "$DAEMON"
  wait_up
}

cmd_status() {
  if [[ -f "$DAEMON" ]]; then echo "상주: LaunchDaemon $DAEMON (로그인 없이 뜬다)"
  elif [[ -f "$AGENT" ]]; then echo "상주: LaunchAgent $AGENT (로그인해야 뜬다 — 로그인 없이 띄우려면 sudo bash $0 daemon)"
  else echo "상주: 없음"; fi
  echo "pid: $(server_pid | tr '\n' ' ')"
  echo "주소: $(public_url)"
  echo "토큰 파일: $(stat -f '%Sp' "$TOKEN" 2>/dev/null || echo 없음)"
  echo "창구 객체: $(find "$STORE" -type f 2>/dev/null | grep -cE '/[0-9a-f]{64}$')개"
  probe
}

case "${1:-}" in
  install) shift; cmd_install "$@" ;;
  sync) sync_objects ;;
  status) cmd_status ;;
  daemon) cmd_daemon ;;
  *) sed -n '2,12p' "$0"; exit 2 ;;
esac
