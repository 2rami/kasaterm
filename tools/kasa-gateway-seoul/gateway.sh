#!/usr/bin/env bash
# 관문(kasa-relay)을 서울 서버(네이버 클라우드, 국내 중계와 같은 기계)에서 돌린다 — 이 맥에서 ssh 로. docs/kasanet.md 「서울 관문」.
#
#   gateway.sh install <kasa-relay 리눅스 바이너리> <caddy 리눅스 바이너리>
#                         nginx(443 SNI)·Caddy(TLS)·관문 systemd 를 올린다. 관문은 아직 안 켠다.
#   gateway.sh env        미니 launchd plist 의 관문 환경을 /etc/kasa-relay/env 로 옮긴다(값은 화면·로그에 안 나온다).
#   gateway.sh state      미니 관문 상태(계정·기기·봉인 저장소·설치 판)를 서울로 복사한다. 관문이 꺼진 채로 부른다.
#   gateway.sh sync-install   미니 relay-install/ 만 서울로 — 폰 새 판을 올린 뒤(latest·관리 화면이 서울에서 그것을 본다).
#   gateway.sh status
#
# 서울 서버는 ssh 별칭 `kasanet-relay`, 미니는 `nacho-neko`(KASANET_RELAY_SSH·KASA_MINI_SSH 로 바꾼다).
set -euo pipefail

SEOUL="${KASANET_RELAY_SSH:-kasanet-relay}"
MINI="${KASA_MINI_SSH:-nacho-neko}"
HERE="$(cd "$(dirname "$0")" && pwd)"
STATE=/var/lib/kasa-relay
MINI_CFG='$HOME/.config/kasaterm'
# 관문이 --state 옆에서 읽고 쓰는 것 전부. mobile-users.json·relay.json·relay-token 은 데스크톱 쪽이라 뺀다.
STATE_ITEMS="relay-state.json relay-accounts.json relay-oauth-identities.json relay-usage.json account-sync workspace-assistant connections relay-install"

seoul() { ssh -o BatchMode=yes "$SEOUL" "$@"; }
mini() { ssh -o BatchMode=yes "$MINI" "$@"; }

fix_owner() {
  # 봉인 저장소·account-sync 는 소유자·0700/0600 이 아니면 열기를 거부한다.
  seoul "sudo chown -R kasa-relay:kasa-relay $STATE && sudo find $STATE -type d -exec chmod 700 {} + && sudo find $STATE -type f -exec chmod 600 {} +"
}

case "${1:-}" in
  install)
    relay_bin="${2:?kasa-relay 리눅스 바이너리 경로}"
    caddy_bin="${3:?caddy 리눅스 바이너리 경로}"
    contact="${KASA_EDGE_CONTACT:-$(git -C "$HERE" config user.email)}"
    scp -q "$relay_bin" "$SEOUL:/tmp/kasa-relay"
    scp -q "$caddy_bin" "$SEOUL:/tmp/caddy"
    COPYFILE_DISABLE=1 tar -C "$HERE" -cf - nginx-sni.conf Caddyfile sites kasa-edge.service kasa-relay.service | seoul 'rm -rf /tmp/kasa-gw && mkdir /tmp/kasa-gw && tar -C /tmp/kasa-gw -xf -'
    seoul "set -e
      id -u kasa-relay >/dev/null 2>&1 || sudo useradd --system --home $STATE --shell /usr/sbin/nologin kasa-relay
      id -u kasa-edge >/dev/null 2>&1 || sudo useradd --system --home /var/lib/kasa-edge --shell /usr/sbin/nologin kasa-edge
      sudo install -d -m 0700 -o kasa-relay -g kasa-relay $STATE
      sudo install -d -m 0700 -o kasa-edge -g kasa-edge /var/lib/kasa-edge
      sudo install -d -m 0750 -o root -g kasa-relay /etc/kasa-relay
      sudo install -d -m 0755 /etc/kasa-edge /etc/kasa-edge/sites /etc/nginx/stream.d /opt/kasa-relay/jev
      sudo install -m 0755 /tmp/kasa-relay /usr/local/bin/kasa-relay
      sudo install -m 0755 /tmp/caddy /usr/local/bin/caddy
      sudo install -m 0644 /tmp/kasa-gw/nginx-sni.conf /etc/nginx/stream.d/kasa-sni.conf
      grep -q '^include /etc/nginx/stream.d/' /etc/nginx/nginx.conf || echo 'include /etc/nginx/stream.d/*.conf;' | sudo tee -a /etc/nginx/nginx.conf >/dev/null
      sudo install -m 0644 /tmp/kasa-gw/Caddyfile /etc/kasa-edge/Caddyfile
      for f in /tmp/kasa-gw/sites/*.caddy; do sudo install -m 0644 \$f /etc/kasa-edge/sites/; done
      echo 'KASA_EDGE_CONTACT=$contact' | sudo tee /etc/kasa-edge/env >/dev/null
      sudo install -m 0644 /tmp/kasa-gw/kasa-edge.service /tmp/kasa-gw/kasa-relay.service /etc/systemd/system/
      rm -rf /tmp/kasa-gw /tmp/kasa-relay /tmp/caddy
      sudo systemctl daemon-reload
      sudo nginx -t -q
      sudo systemctl enable --now nginx kasa-edge >/dev/null 2>&1
      sudo systemctl reload nginx
      sudo systemctl restart kasa-edge
      systemctl is-active nginx kasa-edge"
    ;;
  env)
    # 서명기는 macOS 키체인이라 서울에선 안 쓴다(설치 링크 등록은 미니 관문이 그대로 맡는다). Jev·피드백은 경로만 바꾼다.
    mini 'python3 - <<EOF
import json, os, plistlib
env = plistlib.load(open(os.path.expanduser("~/Library/LaunchAgents/com.geono.kasa-relay.plist"), "rb"))["EnvironmentVariables"]
drop = {"HOME", "PATH", "KASA_INSTALL_SIGNER", "KASA_INSTALL_DAILY_CAP"}
paths = {"KASA_JEV_NODE": "/usr/bin/node", "KASA_JEV_CLIENT": "/opt/kasa-relay/jev/client.mjs",
         "KASATERM_FEEDBACK_ENV_FILE": "/etc/kasa-relay/feedback.env"}
for k, v in sorted(env.items()):
    if k in drop:
        continue
    v = paths.get(k, v)
    print(k + "=" + json.dumps(v))
EOF' | seoul 'sudo tee /etc/kasa-relay/env >/dev/null && sudo chown root:kasa-relay /etc/kasa-relay/env && sudo chmod 640 /etc/kasa-relay/env'
    mini 'cat "$HOME/.config/kasaterm/feedback.env"' | seoul 'sudo tee /etc/kasa-relay/feedback.env >/dev/null && sudo chown root:kasa-relay /etc/kasa-relay/feedback.env && sudo chmod 640 /etc/kasa-relay/feedback.env'
    mini 'cat "$HOME/.local/share/kasa-relay/jev/client.mjs"' | seoul 'sudo tee /opt/kasa-relay/jev/client.mjs >/dev/null && sudo chmod 644 /opt/kasa-relay/jev/client.mjs'
    seoul 'sudo sed "s/=.*//" /etc/kasa-relay/env | tr "\n" " "; echo'
    ;;
  state)
    if seoul 'systemctl is-active -q kasa-relay'; then
      echo "서울 관문이 켜져 있다 — 끄고 부른다(sudo systemctl stop kasa-relay)" >&2
      exit 1
    fi
    mini "cd $MINI_CFG && tar -cf - \$(ls -d $STATE_ITEMS 2>/dev/null)" | seoul "sudo tar -C $STATE -xf - --no-same-owner"
    fix_owner
    seoul "sudo ls -la $STATE | awk '{print \$1, \$9}' | tail -n +2"
    ;;
  sync-install)
    mini "cd $MINI_CFG && tar -cf - relay-install" | seoul "sudo rm -rf $STATE/relay-install.new && sudo mkdir $STATE/relay-install.new && sudo tar -C $STATE/relay-install.new -xf - --no-same-owner && sudo rm -rf $STATE/relay-install && sudo mv $STATE/relay-install.new/relay-install $STATE/relay-install && sudo rmdir $STATE/relay-install.new"
    fix_owner
    seoul "sudo cat $STATE/relay-install/latest; echo"
    ;;
  status)
    seoul 'systemctl is-active nginx kasa-edge kasa-relay kasanet-relay; free -m | sed -n 2,3p; sudo journalctl -u kasa-relay -u kasa-edge -n 15 --no-pager -o cat'
    ;;
  *)
    sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'
    exit 1
    ;;
esac
