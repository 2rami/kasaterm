#!/usr/bin/env bash
# 카사넷 국내 중계(iroh-relay) 운영 — 이 맥에서 ssh 로 서버를 만진다. 설계·까닭은 docs/kasanet.md 「국내 중계」.
#
#   relay.sh install           바이너리(공식 릴리스, sha256 확인)·설정·systemd 를 올리고 켠다. 다시 불러도 된다.
#   relay.sh allow <id|앞자리> <이름>   허용 목록에 한 줄 넣는다. 중계는 다시 안 켠다(access.py 가 접속마다 읽는다).
#                               앞자리(앱 로그의 「폰 3e6bdd4f52 허용」 같은 10자리)면 최근 거절 기록에서 전체 id 를 찾는다.
#   relay.sh deny <id>          허용 목록에서 뺀다(이미 붙은 연결은 다음 접속부터).
#   relay.sh denied             최근 하루 거절된 id — 새 기기(폰)를 넣을 때 고른다.
#   relay.sh apply              설정 틀(relay.toml.in)·access.py 를 고친 뒤 다시 올리고 켠다.
#   relay.sh ids                이 맥의 카사넷 id 와 이 맥에 등록한 폰 id — allow 에 넘길 값.
#   relay.sh status             서비스 상태·허용 목록·최근 로그.
#
# 서버는 ssh 별칭 `kasanet-relay`(~/.ssh/config)로 부른다. KASANET_RELAY_SSH 로 바꾼다.
set -euo pipefail

HOST_NAME="relay-kr.debimarlene.com"
VERSION="v1.3.0"
ASSET="iroh-relay-${VERSION}-x86_64-unknown-linux-musl.tar.gz"
SSH_TARGET="${KASANET_RELAY_SSH:-kasanet-relay}"
HERE="$(cd "$(dirname "$0")" && pwd)"

remote() { ssh -o BatchMode=yes "$SSH_TARGET" "$@"; }

allowlist() { remote 'sudo cat /etc/kasanet-relay/allowlist 2>/dev/null || true'; }

write_allowlist() {
  printf '%s\n' "$1" | sed '/^$/d' | remote 'sudo tee /etc/kasanet-relay/allowlist.new >/dev/null && sudo chmod 644 /etc/kasanet-relay/allowlist.new && sudo mv /etc/kasanet-relay/allowlist.new /etc/kasanet-relay/allowlist'
}

# relay.toml·access.py 를 새로 올리고 둘 다 다시 켠다(붙은 기기는 몇 초 끊겼다 다시 붙는다).
push_config() {
  local contact
  contact="${KASANET_RELAY_CONTACT:-$(git -C "$HERE" config user.email)}"
  [ -n "$contact" ] || { echo "ACME 연락 주소가 없다 — KASANET_RELAY_CONTACT 를 줘라" >&2; exit 1; }
  sed -e "s|@HOST@|$HOST_NAME|" -e "s|@CONTACT@|$contact|" "$HERE/relay.toml.in" \
    | remote 'sudo tee /etc/kasanet-relay/relay.toml >/dev/null'
  scp -q "$HERE/access.py" "$HERE/kasanet-relay-access.service" "$SSH_TARGET:/tmp/"
  remote 'set -e
    sudo install -d -m 0755 /usr/local/lib/kasanet-relay
    sudo install -m 0644 /tmp/access.py /usr/local/lib/kasanet-relay/access.py
    sudo install -m 0644 /tmp/kasanet-relay-access.service /etc/systemd/system/kasanet-relay-access.service
    rm -f /tmp/access.py /tmp/kasanet-relay-access.service
    sudo systemctl daemon-reload
    sudo systemctl enable kasanet-relay-access >/dev/null 2>&1
    sudo systemctl restart kasanet-relay-access kasanet-relay && sleep 2 && systemctl is-active kasanet-relay-access kasanet-relay'
}

# 최근 하루 거절된 id(마지막 때). 앞자리를 주면 그것만.
denied() {
  remote "sudo journalctl -u kasanet-relay-access --since -1d --no-pager -o short-iso | grep ' 거절 ' | awk '{print \$NF, \$1}' | sort -k1,1 -k2,2r | sort -u -k1,1" | grep -E "^${1:-}" || true
}

valid_id() { [[ "$1" =~ ^[0-9a-f]{64}$ ]] || { echo "id 는 64자리 hex 다: $1" >&2; exit 1; }; }

case "${1:-}" in
  install)
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT
    gh release download "$VERSION" --repo n0-computer/iroh --pattern "$ASSET" --dir "$tmp"
    # 공식 릴리스가 sha256 을 따로 안 싣는다 — GitHub 가 알려 주는 자산 digest 와 맞춘다.
    want="$(gh api "repos/n0-computer/iroh/releases/tags/$VERSION" --jq ".assets[] | select(.name==\"$ASSET\") | .digest" | sed 's/^sha256://')"
    got="$(shasum -a 256 "$tmp/$ASSET" | cut -d' ' -f1)"
    [ -n "$want" ] && [ "$want" = "$got" ] || { echo "sha256 이 다르다: 릴리스 $want / 받은 것 $got" >&2; exit 1; }
    tar -xzf "$tmp/$ASSET" -C "$tmp"
    bin="$(find "$tmp" -type f -name iroh-relay | head -1)"
    scp -q "$bin" "$SSH_TARGET:/tmp/iroh-relay"
    scp -q "$HERE/kasanet-relay.service" "$SSH_TARGET:/tmp/kasanet-relay.service"
    remote 'set -e
      id -u kasanet-relay >/dev/null 2>&1 || sudo useradd --system --home /var/lib/kasanet-relay --shell /usr/sbin/nologin kasanet-relay
      sudo install -m 0755 /tmp/iroh-relay /usr/local/bin/iroh-relay
      sudo install -m 0644 /tmp/kasanet-relay.service /etc/systemd/system/kasanet-relay.service
      sudo install -d -m 0755 /etc/kasanet-relay
      sudo install -d -m 0700 -o kasanet-relay -g kasanet-relay /var/lib/kasanet-relay /var/lib/kasanet-relay/certs
      sudo touch /etc/kasanet-relay/allowlist
      rm -f /tmp/iroh-relay /tmp/kasanet-relay.service
      sudo systemctl daemon-reload
      sudo systemctl enable kasanet-relay >/dev/null 2>&1'
    remote '/usr/local/bin/iroh-relay --version'
    push_config
    ;;
  allow)
    id="${2:-}"; name="${3:-}"
    [ -n "$name" ] || { echo "이름을 붙여라(어느 기기인지)" >&2; exit 1; }
    if [[ "$id" =~ ^[0-9a-f]{6,63}$ ]]; then
      found="$(denied "$id" | awk '{print $1}')"
      [ "$(printf '%s' "$found" | grep -c .)" = 1 ] || { echo "거절 기록에서 $id 로 시작하는 id 를 하나로 못 찾았다(그 기기가 중계에 한 번 붙어 봐야 남는다):" >&2; printf '%s\n' "$found" >&2; exit 1; }
      id="$found"
    fi
    valid_id "$id"
    list="$(allowlist | awk -v id="$id" '$1 != id')"
    write_allowlist "$(printf '%s\n%s # %s' "$list" "$id" "$name")"
    echo "넣었다 — $id ($name)"
    ;;
  apply)
    push_config
    ;;
  deny)
    id="${2:-}"
    valid_id "$id"
    write_allowlist "$(allowlist | awk -v id="$id" '$1 != id')"
    ;;
  denied)
    denied "${2:-}"
    ;;
  ids)
    # 이 맥, 명부의 다른 기기(루프백 base 로 받은 /version — 앱이 허용 목록을 배우는 길과 같다), 이 맥에 등록한 폰.
    python3 - <<'EOF'
import json, os, time, urllib.request

def get(url):
    try:
        with urllib.request.urlopen(url, timeout=4) as r:
            return json.load(r)
    except Exception:
        return None

def kid(v):
    return ((v or {}).get("kasanet") or {}).get("id")

print(f"이 맥: {kid(get('http://127.0.0.1:8765/version')) or '카사넷 꺼짐'}")
ms = get("http://127.0.0.1:8765/machines") or []
for m in ms if isinstance(ms, list) else ms.get("machines", []):
    base = (m.get("base") or "").rstrip("/")
    if base.startswith(("http://127.0.0.1:", "http://localhost:")):
        print(f"기기 {m.get('label')}: {kid(get(base + '/version')) or '꺼짐·옛 판·안 닿음'}")
f = os.path.expanduser("~/.config/kasaterm/kasanet-phone-app.json")
if os.path.exists(f):
    for pid, at in (json.load(open(f)).get("phones") or {}).items():
        print(f"폰: {pid} (마지막 등록 {time.strftime('%Y-%m-%d %H:%M', time.localtime(at))})")
EOF
    ;;
  status)
    remote 'systemctl is-active kasanet-relay kasanet-relay-access; echo "--- 허용 목록"; sudo cat /etc/kasanet-relay/allowlist; echo "--- 접근 확인"; sudo journalctl -u kasanet-relay-access -n 10 --no-pager -o cat; echo "--- 로그"; sudo journalctl -u kasanet-relay -n 10 --no-pager -o cat'
    ;;
  *)
    sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'
    exit 1
    ;;
esac
