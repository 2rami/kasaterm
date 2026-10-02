#!/usr/bin/env bash
# 카사넷 국내 중계(iroh-relay) 운영 — 이 맥에서 ssh 로 서버를 만진다. 설계·까닭은 docs/kasanet.md 「국내 중계」.
#
#   relay.sh install           바이너리(공식 릴리스, sha256 확인)·설정·systemd 를 올리고 켠다. 다시 불러도 된다.
#   relay.sh allow <id> <이름>   허용 목록에 한 줄 넣고 다시 켠다(연결이 몇 초 끊겼다 다시 붙는다).
#   relay.sh deny <id>          허용 목록에서 뺀다.
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

# allowlist 줄 → relay.toml. 빈 목록이면 실패 — 빈 허용 목록은 아무도 못 쓰는 중계다.
render() {
  python3 - "$HERE/relay.toml.in" "$HOST_NAME" "$2" "$1" <<'EOF'
import sys
template, host, contact, listing = sys.argv[1:5]
rows = []
for line in listing.splitlines():
    line = line.strip()
    if not line or line.startswith("#"):
        continue
    pid, _, name = line.partition("#")
    rows.append(f'  "{pid.strip()}", # {name.strip()}')
if not rows:
    sys.exit("허용 목록이 비면 아무도 못 쓴다 — 먼저 allow 로 기기를 넣어라")
out = open(template).read().replace("@HOST@", host).replace("@CONTACT@", contact)
print(out.replace("@ALLOWLIST@", "\n".join(rows)), end="")
EOF
}

# allowlist(`<id> # 이름` 줄)로 relay.toml 을 새로 짜서 올리고 다시 켠다.
push_config() {
  local list="$1" contact
  contact="${KASANET_RELAY_CONTACT:-$(git -C "$HERE" config user.email)}"
  [ -n "$contact" ] || { echo "ACME 연락 주소가 없다 — KASANET_RELAY_CONTACT 를 줘라" >&2; exit 1; }
  # 먼저 다 짠 뒤에 올린다 — 짜다 실패한 채로 파이프를 이으면 서버 설정이 빈 파일로 덮인다.
  local toml
  toml="$(render "$list" "$contact")"
  printf '%s\n' "$toml" | remote 'sudo tee /etc/kasanet-relay/relay.toml >/dev/null'
  printf '%s\n' "$list" | remote 'sudo tee /etc/kasanet-relay/allowlist >/dev/null'
  remote 'sudo systemctl restart kasanet-relay && sleep 2 && systemctl is-active kasanet-relay'
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
    list="$(allowlist)"
    if [ -n "$(printf '%s' "$list" | tr -d '[:space:]')" ]; then
      push_config "$list"
    else
      echo "설치만 했다 — allow 로 첫 기기를 넣으면 켜진다"
    fi
    ;;
  allow)
    id="${2:-}"; name="${3:-}"
    valid_id "$id"
    [ -n "$name" ] || { echo "이름을 붙여라(어느 기기인지)" >&2; exit 1; }
    list="$(allowlist | awk -v id="$id" '$1 != id')"
    push_config "$(printf '%s\n%s # %s' "$list" "$id" "$name" | sed '/^$/d')"
    ;;
  deny)
    id="${2:-}"
    valid_id "$id"
    push_config "$(allowlist | awk -v id="$id" '$1 != id')"
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
    remote 'systemctl is-active kasanet-relay; echo "--- 허용 목록"; sudo cat /etc/kasanet-relay/allowlist; echo "--- 로그"; sudo journalctl -u kasanet-relay -n 20 --no-pager'
    ;;
  *)
    sed -n '2,10p' "$0" | sed 's/^# \{0,1\}//'
    exit 1
    ;;
esac
