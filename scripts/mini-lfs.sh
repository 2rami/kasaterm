#!/bin/bash
# 미니 LFS 서버(tools/release/mini_lfs.py) — 이 저장소 git LFS 의 정본. .lfsconfig 가 가리킨다. 받기는 누구나, 올리기는 토큰.
#
# 미니에서:
#   install [공개 주소]    서버 복사 · 토큰 파일 · launchd 상주. 주소는 한 번 주면 기억한다
#   sync [폴더…]           다른 LFS 객체 폴더(기본: 이 저장소)에서 서버에 없는 것만 sha256 을 확인하며 복제
#   token add <이름>       올리기 토큰 하나를 만들어 표준출력으로만 낸다(터미널이면 거부 — 파이프로 받는다)
#   token list|revoke <이름>
#   status                 상주·무토큰 받기 200·무토큰 올리기 401·토큰 올리기 200(토큰은 안 찍는다)
#   daemon                 sudo 로. 로그인 없이 재부팅해도 뜨게 LaunchDaemon 으로 옮긴다
# 올리는 기기에서:
#   login <기기 이름> [ssh 호스트|local]   미니에서 토큰을 받아 git 자격 도우미에 넣고 올리기를 확인한다(기본 nachoneko)
#
# 서버와 객체는 ~/.local/share/kasaterm-lfs 에 둔다 — launchd 가 부르는 파이썬은 ~/Desktop 을 못 읽는다(TCC). 이 폴더는
# preview controller 의 lfs.storage 이기도 하다(객체가 objects/ab/cd/<oid> 로 git-lfs 와 같은 모양이다).
# 토큰은 ~/.config/kasaterm-lfs/token(600, 한 줄에 「이름 토큰」)에만 있다. 서버는 요청마다 다시 읽어 추가·폐기가 바로 먹는다.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LABEL=com.geono.kasaterm-lfs
PORT="${KASATERM_LFS_PORT:-8794}"
PY=/usr/bin/python3
OWNER="${SUDO_USER:-$(id -un)}"
OWNER_HOME="$(eval echo "~$OWNER")"
BASE="$OWNER_HOME/.local/share/kasaterm-lfs"
SERVER="$BASE/mini_lfs.py"
STORE="$BASE/objects"
INCOMING="$BASE/incoming"
CONF="$OWNER_HOME/.config/kasaterm-lfs"
TOKEN="$CONF/token"
URLFILE="$CONF/public-url"
LOG="$OWNER_HOME/Library/Logs/kasaterm-lfs.log"
AGENT="$OWNER_HOME/Library/LaunchAgents/$LABEL.plist"
DAEMON="/Library/LaunchDaemons/$LABEL.plist"
OID_RE='^[0-9a-f]{64}$'

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
# 이름만 — 예전 한 줄 형식이면 토큰 자체가 첫 칸이라 「default」로 적는다.
names() { awk 'NF==2{print $1} NF==1{print "default"}' "$TOKEN" 2>/dev/null || true; }
count_objects() { find "$STORE" -type f 2>/dev/null | grep -cE '/[0-9a-f]{64}$' || true; }

sync_objects() {
  local src f oid dst tmp added=0 bad=0
  local sources=("$@")
  if [[ ${#sources[@]} -eq 0 ]]; then
    sources=("$(git -C "$ROOT" lfs env 2>/dev/null | sed -n 's/^LocalMediaDir=//p')")
  fi
  mkdir -p "$STORE" "$INCOMING"
  for src in "${sources[@]}"; do
    [[ -d "$src" ]] || die "LFS 객체 폴더가 없다: $src"
    [[ "$(cd "$src" && pwd -P)" != "$(cd "$STORE" && pwd -P)" ]] || continue
    while IFS= read -r f; do
      oid="${f##*/}"
      [[ "$oid" =~ $OID_RE ]] || continue
      dst="$STORE/${oid:0:2}/${oid:2:2}/$oid"
      [[ -e "$dst" ]] && continue
      # 이름이 곧 내용의 해시다 — 어긋난 파일을 정본에 들이지 않는다.
      if [[ "$(shasum -a 256 "$f" | cut -d' ' -f1)" != "$oid" ]]; then
        echo "해시가 이름과 다르다 — 건너뜀: $f" >&2
        bad=$((bad + 1))
        continue
      fi
      mkdir -p "$(dirname "$dst")"
      tmp="$INCOMING/sync.$$.$oid"
      cp "$f" "$tmp"
      chmod 644 "$tmp"
      mv "$tmp" "$dst"
      added=$((added + 1))
    done < <(find "$src" -type f)
  done
  echo "서버 객체 $(count_objects)개 (이번에 ${added}개 추가, 해시 불일치 ${bad}개)"
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

# 헤더 파일은 프로세스 치환으로만 넘긴다 — 토큰이 인자(ps)에도 화면에도 안 나온다.
batch_code() {
  local url="$1" op="$2" auth="${3:-}" body
  body="{\"operation\":\"$op\",\"transfers\":[\"basic\"],\"objects\":[{\"oid\":\"$(printf '0%.0s' {1..64})\",\"size\":1}]}"
  if [[ -n "$auth" ]]; then
    curl -s -o /dev/null -w '%{http_code}' -m 15 --retry 2 -X POST -H 'Content-Type: application/vnd.git-lfs+json' \
      -H @<(printf 'Authorization: Bearer %s\n' "$auth") -d "$body" "$url/objects/batch" || true
  else
    curl -s -o /dev/null -w '%{http_code}' -m 15 --retry 2 -X POST -H 'Content-Type: application/vnd.git-lfs+json' \
      -d "$body" "$url/objects/batch" || true
  fi
}

probe() {
  local url="http://127.0.0.1:$PORT$(prefix)" anon_get anon_put authed=- any
  anon_get="$(batch_code "$url" download)"
  anon_put="$(batch_code "$url" upload)"
  any="$(awk 'NF{print $NF; exit}' "$TOKEN" 2>/dev/null || true)"
  [[ -n "$any" ]] && authed="$(batch_code "$url" upload "$any")"
  echo "무토큰 받기 $anon_get · 무토큰 올리기 $anon_put · 토큰 올리기 $authed"
  [[ "$anon_get" == 200 && "$anon_put" == 401 && ( "$authed" == 200 || "$authed" == - ) ]]
}

wait_up() {
  for _ in $(seq 1 20); do probe >/dev/null 2>&1 && { probe; return 0; }; sleep 0.5; done
  probe || die "서버가 안 떴다 — $LOG"
}

ensure_token_file() {
  mkdir -p "$CONF"; chmod 700 "$CONF"
  [[ -e "$TOKEN" ]] || (umask 077; : > "$TOKEN")
  chmod 600 "$TOKEN"
  # 예전 창구의 한 줄짜리 토큰은 CI 비밀(MINI_LFS_TOKEN)과 같은 값이다 — 「ci」라는 이름을 붙여 둔다.
  if [[ "$(awk 'NF' "$TOKEN" | wc -l | tr -d ' ')" == 1 && "$(awk 'NF{print NF}' "$TOKEN")" == 1 ]]; then
    (umask 077; awk 'NF{print "ci", $1}' "$TOKEN" > "$TOKEN.new") && mv "$TOKEN.new" "$TOKEN"
  fi
}

cmd_install() {
  if [[ -n "${1:-}" ]]; then
    [[ "$1" =~ ^https?://[^/]+/.+ ]] || die "주소는 경로가 붙은 http(s) 주소여야 한다(예: https://host/lfs)"
    mkdir -p "$CONF"; chmod 700 "$CONF"
    printf '%s\n' "$1" > "$URLFILE"
  fi
  public_url >/dev/null
  mkdir -p "$BASE" "$STORE" "$(dirname "$LOG")"
  cp "$ROOT/tools/release/mini_lfs.py" "$SERVER.new" && mv "$SERVER.new" "$SERVER"
  ensure_token_file
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
  [[ -s "$SERVER" && -e "$TOKEN" ]] || die "먼저 sudo 없이 install 한다"
  write_plist "$DAEMON" "  <key>UserName</key><string>$OWNER</string>
"
  chown root:wheel "$DAEMON"; chmod 644 "$DAEMON"
  launchctl bootout "gui/$(id -u "$OWNER")/$LABEL" 2>/dev/null || true
  [[ -f "$AGENT" ]] && mv "$AGENT" "$AGENT.replaced-by-daemon"
  launchctl bootout "system/$LABEL" 2>/dev/null || true
  launchctl bootstrap system "$DAEMON"
  wait_up
}

cmd_token() {
  local sub="${1:-}" name="${2:-}"
  case "$sub" in
    add)
      [[ "$name" =~ ^[A-Za-z0-9_.-]{1,40}$ ]] || die "이름은 영문·숫자·_.- 40자 이내"
      [[ -t 1 ]] && die "토큰을 터미널에 찍지 않는다 — login 으로 받거나 파이프로 넘긴다"
      ensure_token_file
      awk -v n="$name" '$1 == n {found=1} END {exit !found}' "$TOKEN" && die "$name 은 이미 있다 — revoke 뒤 다시"
      local t
      t="$("$PY" -c 'import secrets,sys; sys.stdout.write(secrets.token_urlsafe(48))')"
      (umask 077; { cat "$TOKEN"; printf '%s %s\n' "$name" "$t"; } > "$TOKEN.new") && mv "$TOKEN.new" "$TOKEN"
      printf '%s\n' "$t"
      ;;
    list) names ;;
    revoke)
      awk -v n="$name" '$1 == n {found=1} END {exit !found}' "$TOKEN" || die "$name 토큰이 없다"
      (umask 077; awk -v n="$name" '$1 != n' "$TOKEN" > "$TOKEN.new") && mv "$TOKEN.new" "$TOKEN"
      echo "$name 폐기 — 서버가 다음 요청부터 거절한다"
      ;;
    *) die "token add|list|revoke <이름>" ;;
  esac
}

cmd_login() {
  local name="${1:-}" via="${2:-nachoneko}" url host token got code
  [[ "$name" =~ ^[A-Za-z0-9_.-]{1,40}$ ]] || die "login <기기 이름> [ssh 호스트|local]"
  url="$(git -C "$ROOT" config -f "$ROOT/.lfsconfig" --get lfs.url)" || die ".lfsconfig 에 lfs.url 이 없다"
  host="${url#*://}"; host="${host%%/*}"
  if [[ "$via" == local ]]; then
    token="$(cmd_token add "$name")"
  else
    token="$(ssh -o BatchMode=yes "$via" bash -s -- token add "$name" < "$0")"
  fi
  [[ ${#token} -ge 32 ]] || die "토큰을 못 받았다"
  printf 'protocol=https\nhost=%s\nusername=lfs\npassword=%s\n\n' "$host" "$token" | git credential approve
  got="$(printf 'protocol=https\nhost=%s\n\n' "$host" | GIT_TERMINAL_PROMPT=0 git credential fill 2>/dev/null | sed -n 's/^password=//p' || true)"
  [[ "$got" == "$token" ]] || die "git 자격 도우미가 토큰을 되돌려 주지 않는다 — git config credential.helper 를 본다(토큰 $name 은 미니에서 revoke)"
  code="$(batch_code "$url" upload "$token")"
  [[ "$code" == 200 ]] || die "토큰으로 올리기 확인이 $code 다"
  echo "$host 올리기 토큰 「${name}」을 git 자격 도우미에 넣었다 — 올리기 확인 200"
}

cmd_status() {
  if [[ -f "$DAEMON" ]]; then echo "상주: LaunchDaemon $DAEMON (로그인 없이 뜬다)"
  elif [[ -f "$AGENT" ]]; then echo "상주: LaunchAgent $AGENT (로그인해야 뜬다 — 로그인 없이 띄우려면 sudo bash $0 daemon)"
  else echo "상주: 없음"; fi
  echo "pid: $(server_pid | tr '\n' ' ')"
  echo "주소: $(public_url)"
  echo "토큰: $(stat -f '%Sp' "$TOKEN" 2>/dev/null || echo 없음) · $(names | tr '\n' ' ')"
  echo "서버 객체: $(count_objects)개 · $(du -sh "$STORE" 2>/dev/null | cut -f1) · 디스크 여유 $(df -h "$BASE" | awk 'NR==2{print $4}')"
  probe
}

case "${1:-}" in
  install) shift; cmd_install "$@" ;;
  sync) shift; sync_objects "$@" ;;
  token) shift; cmd_token "$@" ;;
  login) shift; cmd_login "$@" ;;
  status) cmd_status ;;
  daemon) cmd_daemon ;;
  *) sed -n '2,18p' "$0"; exit 2 ;;
esac
