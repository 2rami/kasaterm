# 서울 관문 — kasaterm.debimarlene.com 을 네이버 클라우드 서울에서

폰·데스크톱이 지나는 관문(`kasa-relay`)을 맥미니 + Cloudflare 터널에서 서울 서버(국내 중계와 같은 기계)로 옮겼다(2026-10-02).
설치 링크의 앱 재서명은 macOS 키체인이라 그 창구와 LFS 저장소만 맥미니에 남았다.
2026-10-08 에는 Cloudflare 를 거치던 나머지(뎁마 웹·나쵸 웹·디자인 미리보기·geono·집 기계 ssh)도 이 서버 경유로 옮겼다 —
[Cloudflare 에서 옮기기](#cloudflare-에서-옮기기-2026-10-08).

## 왜

Cloudflare 무료 요금제가 한국 회선을 홍콩·도쿄 edge 로 보내, 폰 → 관문 → 데스크톱 한 번에 한국↔홍콩을 네 번 건넜다
(`KASA-share/2026-10-02-폰-연결-느림`). 같은 맥북에서 같은 시각에 잰 앱 길(`KASA-share/2026-10-02-카사넷-국내-중계/측정/app_lat.py`):

| 길 | 데스크톱 요청 왕복 | 학생 화면 첫 장 | 키→화면 반향 p50 |
|---|---|---|---|
| 전 — Cloudflare 터널 + 미니 관문 | 254~261ms | 399~440ms | 184~194ms |
| 뒤 — 서울 관문, 지금 맥북 앱(업링크 Nagle 켜짐) | 63~67ms | 63~84ms | 14~20ms |
| 서울 시험 관문 + 업링크 Nagle 끈 데스크톱(61aae23e) | 19ms | 52~54ms | 17~20ms |

관문만 따뜻한 HTTPS 왕복은 91ms → 8ms.

### 업링크 Nagle

관문을 거치는 HTTP 는 웹소켓 프레임 여럿(머리·몸·끝)으로 쪼개 가는데, 업링크(데스크톱 `connect_async`)와 관문 소켓(axum)의
Nagle 이 프레임마다 상대의 지연 ACK(40ms)를 기다리게 해 요청 하나가 80ms 늘었다. 관문 안에서 루프백으로 재도 90ms 였다.
둘 다 끈다(`uplink.rs` `connect_async_tls_with_config(.., disable_nagle = true, ..)`, `relay.rs` `tap_io(set_nodelay)`).
키 반향은 프레임 하나라 이 영향이 없었다.

## 구성

```
폰·데스크톱 ── 443 ─► nginx stream(SNI, TLS 안 풂)
                       ├─ relay-kr.debimarlene.com ─► 127.0.0.1:8442(PROXY 벗김) ─► iroh-relay 127.0.0.1:8443
                       └─ 그 밖 ─ PROXY ─► Caddy :9443(TLS, Let's Encrypt TLS-ALPN)
                                           ├─ kasaterm·gw-kr ─┬─ /relay/install/*(latest 빼고) ─► 127.0.0.1:18790 ┐
                                           │                  ├─ /lfs/* ─────────────────────────► 127.0.0.1:18794 ┤ ssh 역터널
                                           │                  └─ 나머지 ─► kasa-relay 127.0.0.1:8790             │ (미니가 건다)
                                           └─ 웹 주소(sites/web.caddy) ─► 역터널 18890·18891·18892 ─ 「웹 주소」
맥미니: kasa-relay 설치 전용(8790) · mini_lfs(8794) ◄──────────────────────────────────────────────────────────┘
      ── 80 ──► nginx http: relay-kr 만 iroh-relay 127.0.0.1:8480(사로잡힌 포털), 나머지 301 https
```

- **80/tcp** 는 nginx(`nginx-http.conf`)가 받아 http → https 로 돌린다 — Cloudflare 가장자리가 하던 일. 중계의 사로잡힌 포털
  확인(`/generate_204`)만 relay-kr 이름으로 iroh-relay(`127.0.0.1:8480`)에 넘긴다. Caddy 는 HTTP 챌린지를 끄고 TLS-ALPN 만 쓴다.
- **실제 접속자 주소**: nginx 가 Caddy 에 PROXY 프로토콜을 싣고, Caddy 가 관문에 `CF-Connecting-IP` 를 덮어써 넘긴다.
  관문은 루프백 상대면 그 머리를 접속자로 본다 — 로그인·OAuth·피드백 횟수 제한이 접속자마다 걸린다. 접속자가 보낸 값은 덮인다.
- **미니 역터널** 미니 launchd `com.geono.kasa-seoul-tunnel`(`ssh -N -R 127.0.0.1:18790:… -R 127.0.0.1:18794:… -R 127.0.0.1:18722:127.0.0.1:22`).
  서울 쪽 사용자 `kasa-tunnel` 의 키는 `restrict,port-forwarding,permitlisten=…,command="/bin/false"` 로 그 세 포트 열기만 된다
  (`/var/lib/kasa-tunnel/.ssh/authorized_keys`). 포트를 더하면 키의 `permitlisten` 도 같이 더한다 — 빠지면 `ExitOnForwardFailure`
  로 역터널 전체가 내려가 설치·LFS 까지 끊긴다.
- **미니 ssh**: 18722 는 서울 루프백에만 열린 미니 22 번이다. 다른 기기는 `ProxyJump` 로 서울 운영 계정을 거쳐 붙는다
  (`HostName 127.0.0.1`·`Port 18722`·`HostKeyAlias` 로 미니 호스트키를 따로 둔다). Cloudflare `access ssh` 길이 흔들릴 때 쓰는 기본 길.
  Cloudflare 터널 주소 `kasaterm-mini.debimarlene.com`(미니 터널 ingress 맨 위)도 같은 곳을 가리키지만 서울(ICN)에서 미니
  커넥터까지 0.5MB/s 라 안 쓴다(역터널 100MB/s, 폰 설치 파일 8MB/s). 역터널이 죽으면 Caddyfile 두 곳을 그 주소로 바꿔 넘긴다.
- **미니 관문은 설치 전용**: plist 에 `--state ~/.config/kasaterm/install-only/relay-state.json` 을 더해 빈 계정 상태로 돈다.
  `install-only/relay-install` 은 `../relay-install` 로 이어 `adhoc.sh` 가 올리는 자리를 그대로 읽는다. 등록 프로파일 서명·
  기기 등록·ipa 는 여기서, 폰 앱의 새 판 알림(`/relay/install/latest`)은 기기 토큰을 보므로 서울에서 — 그래서 `adhoc.sh` 가
  올린 뒤 `gateway.sh sync-install` 로 서울에도 판을 놓는다. 관리 화면(서울)의 설치 칸은 그 동기화 때 기준이다.
- **서울 관문**: systemd `kasa-relay`(사용자 `kasa-relay`, `--bind 127.0.0.1 --port 8790 --state /var/lib/kasa-relay/relay-state.json`),
  환경 `/etc/kasa-relay/env`(미니 plist 에서 옮김, 서명기는 뺌, Jev 는 `/usr/bin/node` + `/opt/kasa-relay/jev/client.mjs`,
  피드백은 `/etc/kasa-relay/feedback.env`). Caddy 는 공식 2.11.6(우분투 2.6.2 는 PROXY 프로토콜 리스너가 없다), systemd `kasa-edge`.
  원격 승인 폰 알림용 APNs 열쇠는 `/etc/kasa-relay/apns/AuthKey_<id>.p8`(디렉터리 root:kasa-relay 750, 파일 kasa-relay 0600),
  env 에 `KASATERM_APNS_KEY_ID`·`KASATERM_APNS_TEAM_ID`·`KASATERM_APNS_KEY_PATH`([remote-approval.md](remote-approval.md)).
  관문이 새로 쓰는 파일은 상태 옆 `relay-push.json`(폰 푸시 토큰)·`relay-approvals-audit.jsonl`(승인 감사).
- 운영 `tools/kasa-gateway-seoul/gateway.sh`(이 맥): `build`(도커 교차 컴파일) · `upgrade <바이너리>` · `install` · `env` ·
  `state` · `sync-install` · `edge`(Caddy·nginx 설정만, 재시작) · `allow <이름>`(on-demand 인증서 허가) · `status`.
  중계는 `tools/kasanet-relay/relay.sh`.

## Cloudflare 에서 옮기기 (2026-10-08)

Cloudflare 터널·`access ssh` 로 닿던 길을 전부 이 서버 경유로 옮겼다. 각 기계가 서울로 ssh 역터널을 걸고(기계마다 제한 키),
웹은 서울 Caddy 가, ssh 는 `ProxyJump` 가 그 포트로 넘긴다. DNS 는 Cloudflare 에 두고 레코드만 A `211.233.212.132`
(DNS only)로 바꿨다. Cloudflare 터널은 하루 예비로 그대로 돈다(되돌리기 = 레코드를 터널 CNAME 으로).

### 무엇이 어디로

| 주소·길 | 전(Cloudflare) | 뒤(서울 루프백 포트 ← 거는 것) | 옮긴 때 |
|---|---|---|---|
| 미니 ssh `nachoneko` | `macmini` ingress(미니 kasaterm 터널) | 18722 ← 미니 `com.geono.kasa-seoul-tunnel` | 10-08 오전 |
| 거점 맥북 ssh `homehub-wan` | 거점 LaunchDaemon `com.geono.homehub-tunnel` | 18723 ← 거점 LaunchAgent `com.geono.kasa-seoul-tunnel` | 18:36 |
| 집 데스크톱 ssh `windesk-wan` | 윈도우 서비스 `cloudflared` | 18724 ← 예약 작업 `kasa-seoul-tunnel`(SYSTEM, 부팅 때) | 18:38 |
| `debimarlene.com`·`www`·`panel`(뎁마 봇 웹), `nacho`, `mm` | 미니 터널 `debimarlene`(LaunchDaemon) → 미니 Caddy 8090 | 18890 ← 미니 LaunchAgent `com.geono.kasa-seoul-web-tunnel` | 18:54~19:06 |
| 디자인 미리보기 `<이름>-<4자>` | 미니 터널 `design-preview` → 문지기 4791 | 18891 ← 같은 웹 터널 | 18:58~19:03 |
| `geono` | 맥북 터널 `geono-hub` → 4749 | 18892 ← 맥북 LaunchAgent `com.geono.kasa-seoul-tunnel` | 18:53 |
| `kasaterm-mini` | 미니 kasaterm 터널 → 관문·LFS | 안 옮김 — 서울이 역터널 18790·18794 로 직접 닿는다. 그 역터널의 예비라 Cloudflare 를 끌 때 같이 없어진다 | — |

DNS 레코드는 `homehub`·`homepc`·`macmini`·`kasaterm-mini` 만 터널 CNAME 으로 남았다. ssh 는 DNS 를 안 보므로(127.0.0.1 로 간다)
안 바꿨다. 옮기기 전 레코드 전부는 맥북 `~/.config/kasa-seoul/cf-dns-debimarlene-20261008-before.json`.
그 밖에 서울 쪽 내부 포트: 18899(Caddy on-demand 허가 질의), 8480(iroh-relay http).

### 역터널 키

서울 `/var/lib/kasa-tunnel/.ssh/authorized_keys` 한 줄에 한 기계, 모두 `restrict,port-forwarding,permitlisten="127.0.0.1:<자기 포트>",
permitopen="127.0.0.1:1",command="/bin/false"`. 미니 키만 permitlisten 이 여럿(18790·18794·18722·18890·18891)이고 permitopen 이
18723·18724 다 — 미니가 집 기계로 ssh 할 때 이 키로 `ProxyJump`(미니 `~/.ssh/config` 의 `kasa-seoul-jump`)한다.

- ⚠️ authorized_keys 의 `permitopen` 은 `none` 을 못 받는다 — 쓰면 그 키 줄이 통째로 무효가 되어 「Permission denied (publickey)」만
  남는다(sshd_config 의 PermitOpen 은 받는다). -L 을 막으려면 아무도 안 듣는 `127.0.0.1:1` 을 준다.
- 개인 키는 그 기계 밖으로 안 나간다: 거점·맥북·미니 `~/.ssh/kasa_seoul_tunnel`, 집 데스크톱 `C:\ProgramData\kasa-seoul-tunnel\id_ed25519`
  (폴더와 키를 SYSTEM·Administrators 만 — SYSTEM 이 돌리는 `run.ps1` 을 다른 사용자가 못 고치게, 윈도우 OpenSSH 는 키에 다른 사용자
  권한이 있으면 거부한다). 서울 호스트키는 각 기계 known_hosts 에 미리 넣고 `StrictHostKeyChecking=yes`.
- 확인한 제한(거점 키): 다른 포트 `-R` 거부, `-W 127.0.0.1:18722` 「administratively prohibited」, 셸 「not available」.

### ssh

- 맥북 `~/.ssh/config`: `homehub-wan`·`windesk-wan` = `HostName 127.0.0.1`, `Port 18723`/`18724`, `ProxyJump kasanet-relay`,
  `HostKeyAlias homehub.debimarlene.com`/`homepc.debimarlene.com` — 옛 주소를 별칭으로 두어 이미 아는 호스트키로 검사한다.
  `windesk-hub`(거점 경유 랜)는 `homehub-wan` 을 거치므로 같이 서울로 간다. 옛 길은 `homehub-cf`·`windesk-cf`.
- 미니 `~/.ssh/config`: 같은 별칭, `ProxyJump kasa-seoul-jump`. 미니 카사텀의 집 데스크톱 기계 연결과 나쵸 homepc 도구가 쓴다.
  이미 붙어 있던 연결은 끊겨 다시 붙을 때까지 Cloudflare 로 남는다.
- 집 기계가 꺼져 있으면 길이 없다(전과 같다). 거점 맥북·미니의 터널은 LaunchAgent(두 기계 다 sudo 가 없다)라 재부팅 뒤 GUI 로그인
  전에는 안 뜬다 — 옛 cloudflared 는 LaunchDaemon 이었다. 집 데스크톱은 SYSTEM 예약 작업이라 로그인 없이 뜬다.
- 미니 → 거점 맥북(`homehub-wan`, 사용자 `momyomoui`)은 옮기기 전부터 거부된다 — 거점에 그 계정이 없고 미니 키가 거점 `kasa`
  authorized_keys 에 없다. 길만 옮겼다.

### 웹 주소

- 서울 Caddy `sites/web.caddy`: 18890(미니 Caddy 가 Host 로 가른다), 다른 사이트에 안 걸린 이름 전부 18891(미리보기 문지기),
  `geono` 18892. `CF-Connecting-IP` 를 접속자 주소로 덮어쓴다 — 대시보드 포트폴리오 챗봇·문지기 로그인·geono 로그인의 횟수 제한이
  그 값을 본다(Cloudflare 가 하던 일, 접속자가 보낸 값은 버린다).
- 나쵸 웹(kasa-serve-web)의 토큰 관문은 그대로다: 미니 Caddy 가 붙이는 `X-Forwarded-For` 로 원격 판정 → 무토큰 403, 로컬 헤더를
  위조해도 403, 토큰이면 200(미니 로컬과 같은 응답).
- 인증서는 on-demand 다. 고정 이름은 서울 `/etc/kasa-edge/on-demand/<이름>` 이 있을 때(`gateway.sh allow`), 미리보기는 미니
  문지기 `/__dp/ask` 가 등록 목록으로 답할 때만 받는다. 첫 접속이 발급을 기다렸다 붙는다(4~7초). 미리보기 DNS 는 와일드카드
  `*.debimarlene.com` A 하나라 `dp up` 은 더 이상 DNS 를 안 붙인다(dotfiles `design-preview` 스킬).
- ⚠️ **미리보기 사이트를 `*.debimarlene.com` 으로 쓰면 관문이 깨진다.** 그 on-demand 정책이 `kasaterm`·`gw-kr` 까지 덮어, 저장된
  인증서를 안 읽고 허가 질의로 가서 거절된다 — 18:44:15 에 관문 TLS 가 통째로 끊겼고 20초 만에 되돌렸다(데스크톱 둘은 30초 뒤
  재접속). 주소 없는 `https://` 사이트로 받으면 다른 사이트에 안 걸린 이름만 그 정책을 탄다. Caddy 설정을 바꿀 때는 운영 반영
  전에 서울에서 인증서 저장소 사본으로 다른 포트에 따로 띄워 `kasaterm`·`gw-kr` 이 200 인지 본다(사본은 개인키라 끝나면 지운다).
- ⚠️ **프록시(주황) 레코드를 DNS only 로 바꾸면 옛 Cloudflare IP 를 캐시한 접속자는 최대 5분 리다이렉트 루프에 빠진다** — 가장자리가
  더는 프록시하지 않는 이름에 같은 주소로 301 을 돌려준다. 프록시 레코드 TTL 은 300초로 고정이라 미리 낮출 수 없다. 뎁마 웹은
  19:04~19:11 사이 캐시가 남은 접속자에게 그렇게 보였다. 다음에 운영 주소를 옮길 때는 이용이 적은 시간에 한다.
- 미니 웹 터널도 LaunchAgent 라 미니 재부팅 뒤 GUI 로그인 전에는 웹이 안 뜬다(뎁마 봇 dockbot 도 LaunchAgent 라 어차피 그때는
  안 돈다). 미니에서 sudo 로 plist 를 `/Library/LaunchDaemons`(`UserName nachoneko`)로 옮기면 로그인 없이 뜬다.

### DNS 를 하나 더 옮길 때

1. 그 기계가 역터널 포트를 열게 하고(서울 키 `permitlisten` 먼저 — 빠지면 `ExitOnForwardFailure` 로 그 터널 전체가 내려간다),
   `sites/web.caddy` 에 사이트를 더해 `gateway.sh edge`(재시작 — 관문 업링크가 30초쯤 끊겼다 붙는다).
2. `gateway.sh allow <이름>` — DNS 를 돌리는 그때. 먼저 열면 누가 그 이름으로 여기 붙을 때 발급이 실패해 Let's Encrypt 실패
   한도(이름당 시간 5회)를 먹는다.
3. Cloudflare 대시보드에서 레코드를 A `211.233.212.132`·DNS only 로(편집 칸에서 종류를 바꾸면 지우고 새로 만든다고 경고하고 그대로
   된다). 대시보드 API 를 직접 부르면 쓰기는 403 이다 — 화면으로 한다.
4. `dig @heather.ns.cloudflare.com` 으로 바뀐 것을 보고 `curl --resolve <이름>:443:211.233.212.132` 로 첫 접속(발급)·응답을 본다.

### 되돌리기

- 웹 DNS: 그 레코드를 지우고 이 맥에서 `cloudflared tunnel --config /dev/null route dns <터널> <이름>` — 터널은 뎁마·나쵸·mm 이
  `debimarlene`, 미리보기 `design-preview`, geono `geono-hub`(대시보드에서는 CNAME `<터널 id>.cfargotunnel.com`·프록시 켬, id 는
  위 백업 파일에). 와일드카드 `*` 레코드는 지우면 된다. 터널이 아직 도는 하루 안에만 바로 된다. 되돌릴 때도 위 5분 루프가 거꾸로 난다.
- ssh: `*-cf` 별칭을 쓰거나 `~/.ssh/config.bak-20261008-seoul-home`(맥북·미니 같은 이름)으로. 집 기계 역터널을 걷으려면 거점
  `launchctl bootout gui/$(id -u)/com.geono.kasa-seoul-tunnel`, 집 데스크톱 `Unregister-ScheduledTask kasa-seoul-tunnel`, 서울 키 줄 삭제.
- 서울: Caddy 는 `/etc/kasa-edge.bak-20261008-184357`(옮기기 전)으로 `/etc/kasa-edge` 를 바꾸고 `systemctl restart kasa-edge`.
  80 은 `/etc/nginx/conf.d/kasa-http.conf` 를 지우고 nginx reload 뒤 `relay.toml.in` 의 `http_bind_addr` 를 `0.0.0.0:80` 으로 돌려
  `relay.sh apply`(서버 백업 `/etc/kasanet-relay/relay.toml.bak-20261008-http`). 키는 `authorized_keys.bak-20261008-*`.

### Cloudflare 끄기 — DNS 다 옮기고 하루 뒤, 소유자 확인 뒤

미니 LaunchDaemon `com.geono.debimarlene-tunnel`(sudo)·LaunchAgent `com.geono.design-preview-tunnel`·`com.geono.kasaterm-gateway-tunnel`,
거점 LaunchDaemon `com.geono.homehub-tunnel`(sudo), 집 데스크톱 서비스 `cloudflared`, 맥북 geono-hub `scripts/serve-public.sh` 의
cloudflared 줄. 그 뒤 남은 터널 CNAME 네 개를 지운다. 앱 안의 cloudflared(폰 빠른 터널 trycloudflare·옛 바깥주소 `tunnel.rs`)는
이 일 밖이다.

## 전환 기록 (2026-10-02)

1. 13:43:14 미니 관문을 설치 전용으로 다시 띄움(운영 상태가 더 안 바뀌게) → `gateway.sh state` → 13:43:41 서울 관문 켬.
2. 13:45:01 Cloudflare DNS `kasaterm` 을 터널 CNAME(프록시) → A `211.233.212.132`(DNS only). 6초 뒤 Caddy 가 인증서를 받음.
3. 13:45:44 미니 터널에 `kasaterm-mini` 를 더하고 다시 켬. 뒤이어 역터널로 바꿈.
4. 데스크톱 셋(맥북·모묘모의 MacBook Pro·맥미니)이 서울에 다시 붙음. 설치 페이지·manifest·ipa·LFS 배치 API·객체 받기(해시 일치) 확인.

⚠️ **1 과 2 사이에 DNS 가 아직 터널을 가리켜 데스크톱이 설치 전용(빈 상태) 관문에 붙었다.** 기기 토큰이 `device_token_invalid` 로
거절됐고, 앱은 그 토큰을 메모리에 「거절됨」으로 들고 토큰 없이(폰 주소만) 붙는다 — 서울 상태에는 토큰이 멀쩡하지만 **앱을 다시
켜기 전까지 계정 없이 붙어 있다**(계정 길로 보는 폰 기계 목록·계정 동기화가 빈다). 다음 전환에서는 설치 전용 인스턴스를 DNS 를
바꾼 뒤에 띄우거나, 그 사이 관문을 아예 내려 둔다.

## 원격 승인 교체 기록 (2026-10-05)

8c185712 관문으로 교체(sha256 6dd6d12d…), APNs 열쇠·env 셋 추가. 교체 전 백업: `/usr/local/bin/kasa-relay.bak-20261005`,
`/etc/kasa-relay/env.bak-20261005`, `/var/lib/kasa-relay-bak-20261005.tar`. 되돌리기는 그 바이너리·env 를 제자리로 놓고
`sudo systemctl restart kasa-relay`(열쇠 파일은 지워도 되고 남겨도 옛 관문은 안 읽는다).

## 연결 한 번에 교체 기록 (2026-10-05)

99337a7b 관문으로 교체(sha256 a9bad99a…, Google·GitHub 연결 한 번에 로그인과 일 권한). 8c185712 에서 관문 쪽 변경은 이것뿐, env 변경 없음.
교체 전 백업 `/usr/local/bin/kasa-relay.bak-20261005b`(=8c185712). 되돌리기는 그 파일을 제자리로 놓고 `sudo systemctl restart kasa-relay`.
확인: `/relay/health` 200, `providers` 의 `connect` 둘 다 참, 무토큰 `/relay/connections` 401, 맥미니·맥북 재접속.

## Gmail 걷기 교체 기록 (2026-10-07)

472d2b9d 관문으로 교체(13:53:13, sha256 f69933a2…, Gmail 일 권한 걷기 — [account-connections.md](account-connections.md)). env 변경 없음.
켜면서 봉인함의 Google 연결 1개를 Google 에서 철회·삭제했다(로그 `gmail connections retired=1 revoked=1`, 감사 `gmail.retire`).
교체 전 백업 `/usr/local/bin/kasa-relay.bak-20261007`(sha256 73c25cc0…), `/var/lib/kasa-relay-bak-20261007-connections.tar`
(철회된 토큰이라 되살려도 메일은 안 열린다). 되돌리기는 그 바이너리를 제자리로 놓고 `sudo systemctl restart kasa-relay`.
확인: `/relay/health` 200, `providers` 의 `connect` google 거짓·github 참, 무토큰 `/relay/connections` 401, 메일 기능을 실은
`oauth/start` 400, 설정 Google 연결의 동의 URL scope `openid email profile` 뿐(교체 전엔 `gmail.readonly`·`gmail.send` 가 함께),
기기 whoami·GitHub 연결 정상, 맥미니·맥북 재접속.

## Claude 사슬 관문 갱신 교체 기록 (2026-10-07)

ee19de7b 관문으로 교체(15:05:52, sha256 f82c1c78…, Claude 로그인 사슬 맡기기·1분 갱신·끊김/막힘 폰 푸시 —
[agent-chains.md](agent-chains.md)). env 변경 없음. 켜면서 상태 옆에 `agent-chains/`(봉인 저장소·`audit.jsonl`)가 생긴다.
교체 전 백업 `/usr/local/bin/kasa-relay.bak-20261007b`(=472d2b9d, sha256 f69933a2…, 옆에 `.info`). 되돌리기는 그 파일을
제자리로 놓고 `sudo systemctl restart kasa-relay`(옛 관문은 `agent-chains/` 를 안 읽는다. 맡긴 사슬이 있었다면 기기들은
그 계정을 다시 로그인해야 한다).
전후 같음: `/relay/health` 200, 무토큰 whoami·connections 401, 기기 whoami 200, 없는 계정 로그인 401, `providers` connect
github 참·google 거짓, GitHub 연결 ok, 폰 `/u/<slug>/` 200, 맥미니·맥북 재접속(계정 2rami). 새 창구 `/relay/agent-chains` 는
404 → 200(빈 목록), 무토큰 401. 서울 IP 에서 토큰 창구는 가짜 갱신 토큰에 400 `invalid_grant`(막힘 없음 — curl 기본
UA 만 429, 관문은 `KASA-AgentChains/1`), 프로필 창구는 무토큰 401.
첫 사슬은 새 앱 판(ee19de7b 이상)을 받은 기기가 맡기고, 첫 실제 갱신은 그 뒤 0~5시간 안이다 — 확인 방법은
[agent-chains.md](agent-chains.md) 「첫 실제 갱신 확인」.

## 되돌리기

1. Cloudflare DNS `kasaterm` 을 A → 터널 CNAME 으로: 레코드를 지우고 이 맥에서
   `cloudflared tunnel --config /dev/null route dns 613e1da6-1ee5-4a78-ba55-90ad5b432875 kasaterm.debimarlene.com`.
   미니 터널 ingress 의 `kasaterm.debimarlene.com` 줄(/lfs·관문)은 그대로 남아 있다.
2. 서울 상태를 미니로: 서울 관문을 내리고(`sudo systemctl stop kasa-relay`) `/var/lib/kasa-relay` 의 같은 파일들을 미니
   `~/.config/kasaterm/` 로(봉인 저장소·account-sync 는 소유자·0700/0600). 전환 직전 미니 상태는 `relay-backup-20261002-seoul.tar`.
3. 미니 plist 를 `com.geono.kasa-relay.plist.bak-20261002-seoul` 로 되돌리고 `launchctl bootout` → `bootstrap`.
