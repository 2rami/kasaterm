# 카사넷 — 기기끼리 직통 길

기기 사이 길을 ssh 터널·Cloudflare 에서 **카사텀이 직접 여는 P2P QUIC** 으로 옮긴다.
직통이 안 되면 중계로, 그것도 안 되면 지금 ssh 터널로 떨어진다.

## 왜

2026-09-28 실측(회사 맥북 → 맥미니, 거울 키 입력→에코):

| 길 | p50 | 비고 |
|---|---|---|
| Cloudflare(`cloudflared access ssh`) | 106ms | 양 끝 서울 edge 까지는 1~9ms, 나머지는 경유 비용 |
| 넷버드 릴레이 | 9~15ms | 밤사이 로그인 만료(NeedsLogin)로 끊긴다 |

STUN 판정으로는 두 기기 모두 목적지 무관 매핑 NAT 라 구멍 뚫기가 되는 쪽이다(실제 뚫기는 미측정).

## 부품

- `iroh` 1.x(MIT/Apache, MSRV 1.91) — 기기 주소 = 공개키(EndpointId), 구멍 뚫기·중계 폴백·QUIC 암호화를 한 번에.
- 중계: **국내 자체 중계**(네이버 클라우드 서울, `relay-kr.debimarlene.com`)와 iroh 기본 공용 중계(n0)를 같이 둔다. 데이터는
  국내 중계로만 싣고 n0 는 구멍 뚫기 신호에만 쓴다. 내용은 종단 암호화라 중계는 메타데이터만 본다 — 아래 「국내 중계」.
  (Cloudflare 터널 너머 맥미니 중계는 이득이 없어 걷었다 — 아래 「자체 중계」.)

## 계약

- **신원**: 기기 비밀키는 `~/.config/kasaterm/kasanet.key`(0600). 로그·응답·세션 파일에 싣지 않는다.
  격리 인스턴스(검증 앱)는 별도 경로를 쓴다 — 본판과 같은 키로 뜨면 상대가 두 기기를 한 기기로 본다.
- **허용 목록**: 들어오는 연결은 기존 기기 채널(명부·ssh 길·계정 로그인)로 배운 EndpointId 만 받는다.
  모르는 EndpointId 는 스트림을 열기 전에 끊는다.
- **포워드**: 받는 쪽은 항상 `127.0.0.1:<port>` 로만 잇는다. 허용 포트는 카사텀(8765)·카사크롬(8777)과
  사용자가 명시로 공유한 포트뿐이다. 보내는 쪽은 `127.0.0.1:<로컬 포트>` 로 연다 — 개발 서버 bind,
  secure context, OAuth 되돌아오기 주소가 localhost 그대로 동작하게.
- **길 선택**: 기기 base 는 바꾸지 않는다. 요청하는 자리가 base 대신 그 base 의 로컬 입구로 가고, 입구가
  연결마다 **직통이나 국내 중계면 카사넷, 아니면 원래 base(ssh 터널)** 로 잇는다. 공용 중계(n0)로는 데이터를 싣지 않는다.
  경로 종류(카사넷·ssh)와 왕복 시간을 기기 상태에 싣는다. (처음 계약은 「base 를 로컬 포트로 바꿔 끼운다」였으나
  거울이 붙을 때의 base 를 평생 들고 다니고 세션 복원·여러 판정이 base 로 기기를 찾아서 바꿀 수 없었다 — P2 참고)
- **양쪽 새 판**: EndpointId 를 모르는 옛 판 상대는 ssh 길 그대로다. 옛 판과의 동작을 깨지 않는다.

## 단계

| 단계 | 내용 | 끝 조건 |
|---|---|---|
| P0 | `crates/kasa-net` + 시험 바이너리. 두 기기 사이 직통 성립 여부·성립까지 걸린 시간·왕복·처리량 | 맥북↔미니 수치(직통/중계 각각)를 이 문서에 기록 |
| P1 | 핵심: 신원·허용 목록·TCP 포워드(ALPN `kasa/fwd/1`) + 단위 테스트 | 루프백 두 노드로 포워드 왕복 테스트 통과 |
| P2 | 연결: `/version` 에 EndpointId 공개, `machines.rs` 길 선택, 기기 상태에 경로 표시 | 격리 앱 둘이 카사넷으로 거울 연결 |
| P3 | 포트 공유: `kasaterm-cli net forward <기기> <port>`, 다른 데스크톱으로 페이지 보여 주기 | 개발 서버를 다른 기기 localhost 로 열기 |
| P4 | 거울 화면 동기화를 QUIC datagram 으로(mosh 식) | 패킷 손실 중에도 입력 에코가 안 멈춤 |
| P5 | 폰: 앱 안 Safari 화면이 카사넷으로 페이지 받기 | 폰에서 맥북 개발 서버 보기 — 시뮬레이터 확인, 실기는 TestFlight 뒤 |

## P3~P5 계약 (2026-09-29)

- **P3 포트 공유**: 앱 HTTP 서버에 `/net/tcp?port=N` 웹소켓(바이너리 = `127.0.0.1:N` 과 양방향). 루프백 peer 만 토큰 없이,
  그 밖은 기존 원격 토큰 규칙. 끌어오는 쪽은 앱이 `127.0.0.1:L` 을 듣고 연결마다 `route_base(기기 base)` 로 이 웹소켓을
  연다 — P2 입구가 직통이면 카사넷, 아니면 ssh 로 고르므로 폴백이 따로 필요 없다. 루프백 base 로 붙는 기기는 이미 셸을
  띄울 수 있는 신뢰(ssh -L 과 같음)라 `net forward` 명령 자체를 명시 공유로 본다.
  CLI `kasaterm-cli net forward <기기> <port> [--local L]` → `http://localhost:L`, `net list`, `net stop <L>`.
  「보여 주기」 대상이 다른 **데스크톱**이고 주소가 이 기기의 localhost 면, 공개 임시 터널 대신 그 기기가 이 포트를
  끌어가 `http://localhost:L` 을 연다. 폰 대상은 P5 전까지 임시 터널 그대로.
- **P4 datagram**: 보류. 연결마다 QUIC 스트림이라 창끼리 막지 않고, 직통 왕복 6~8ms 에서 재전송 한 번은 몇 ms 다. 느리고
  손실 많은 길은 이미 ssh 로 보낸다. 실사용에서 끊김이 재지면 다시 연다.
- **P5 폰**: `crates/kasa-net-ffi`(iOS 정적 라이브러리)로 폰 앱이 카사넷 엔드포인트를 든다. 폰 키는 앱 안에만.
  데스크톱은 폰 id 를 **관문 계정 채널**(이미 로그인된 폰)로만 배운다. 폰이 데스크톱에 가는 HTTP·웹소켓은 데스크톱과 같은
  입구 규칙(직통이면 카사넷, 아니면 관문). 데스크톱 개발 서버는 그 입구와 P3 `/net/tcp` 로 폰 localhost 에 끌어와 앱 안
  Safari 화면(`SFSafariViewController`, 안드로이드는 크롬 커스텀 탭)으로 연다.

## P0 실측 (2026-09-29)

맥북 Pro(공유기 NAT 192.168.0.x, 공인 180.224.72.73) ↔ 맥미니(다른 망 10.1.x, 공인 218.153.32.129).
iroh 1.3.0 release 빌드, n0 공용 중계(두 쪽 홈 중계 모두 `aps1`). 시도마다 새 키·새 엔드포인트라 앞 시도의 길 기억이 없다.

| 길 | 성립 | 핸드셰이크 | 직통까지 | datagram p50/p90 | 스트림 p50/p90 | 20MB 올리기/내리기 |
|---|---|---|---|---|---|---|
| 직통 맥북→미니 | 3/3 | 173ms | 380~387ms | 6.4/7.0ms | 6.4/7.7ms | 98/136 Mbps |
| 직통 미니→맥북 | 2/2 | 195ms | 420~424ms | 8.4/8.9ms | 8.4/8.9ms | 135/71 Mbps |
| 직통 미니→맥북, EndpointId 만(DNS 찾기) | 2/2 | 484·230ms | 716·457ms | 6.7/8.5ms | 7.2/10.7ms | (2MB) 100/78 Mbps |
| 중계 강제 맥북→미니 | 3/3 | 172~194ms | — | 171.8/173.3ms | 172.3/174.6ms | 8.1/8.9 Mbps |

- 직통은 양쪽 **공인 주소**로 잡혔다 — 같은 LAN 이 아니라 인터넷 너머 구멍 뚫기가 된 것이다. 첫 패킷은 중계로 가고
  약 200ms 뒤 직통으로 옮겨 간다. 왕복 6~8ms 로 Cloudflare 길(106ms)의 1/15 이다. datagram 유실 0.
- 홈 중계에 붙기까지 맥북 0.7~0.8s, 미니 3.3s(원인 미확인 — 미니 쪽 인터페이스가 여섯 개다).
- 허용 목록: 미니가 다른 id 만 허용하게 띄우자 거는 쪽이 `closed by peer: kasanet: unknown endpoint (code 403)` 로
  끊겼고, 제 id 를 허용하면 직통으로 붙었다.
- **자체 중계 판단**: n0 공용 중계는 가장 가까운 것이 `aps1` 이라 172ms·8Mbps 로 **Cloudflare 보다 느리다**.
  직통이 안 될 때 공용 중계로 떨어뜨리면 지금보다 나빠진다 — 국내에 자체 중계를 두거나, 그 전까지는 직통 실패 시
  ssh 길을 공용 중계보다 앞에 둔다.

다시 재려면 `kasa-net-probe serve` 가 찍는 `KASANET_ADDR` 값을 다른 기기에서
`kasa-net-probe dial '<값>' --mode direct|relay` 로 넘긴다. relay 모드는 거는 쪽 IP 전송을 걷어 중계 말고 길이 없게 한다.

## P1 구현 (`crates/kasa-net`)

- **신원** `identity::load_or_create` — 비밀키 32바이트를 0600 으로. 다 쓴 임시 파일을 `hard_link` 로 붙여 동시에
  만들어도 키 하나로 모이고 반쯤 쓴 키를 읽는 틈이 없다. 느슨한 권한은 조이고, 깨진 파일은 새 키로 덮지 않고 오류다
  (덮으면 기기 신원이 조용히 바뀐다). 격리 인스턴스는 `KASATERM_KASANET_KEY` 로 경로를 가른다.
- **허용 목록** `AllowList::hook()` — iroh `after_handshake` 훅. 들어오는 연결의 EndpointId 가 목록에 없으면 403 으로
  닫아 어떤 프로토콜 처리기도 연결을 못 본다. 모든 엔드포인트는 `kasa_net::builder` 로 만들어 훅이 빠지지 않게 한다.
- **포워드** ALPN `kasa/fwd/1` — 스트림 하나가 TCP 연결 하나. 여는 쪽이 포트 2바이트(BE), 받는 쪽이 상태 1바이트
  (0 연결됨·1 허용 안 된 포트·2 아무도 안 듣는 포트)로 답한 뒤 날 바이트를 양방향으로. 받는 쪽 `FwdServer` 는
  `127.0.0.1` 허용 포트로만 잇고 허용 집합은 실행 중에 바꾼다. 보내는 쪽 `Forward::start` 는 `127.0.0.1` 에서 받아
  QUIC 연결 하나를 같이 쓴다.
- 검사 `cargo test -p kasa-net` — 신원 3개, 루프백 두 노드 3개(포워드 왕복·반쯤 닫기 / 모르는 id 403·뒤쪽 TCP 안 닿음 /
  허용 포트만·허용 거두기·빈 포트). 허용 목록·포트 검사를 끄면 뒤의 둘이 실패하는 것을 확인했다.

## P2 배선 (`crates/kasa-mcp/src/kasanet.rs`)

- **띄우기**: 앱 HTTP 서버가 뜰 때 기기 키로 엔드포인트 하나(`presets::Minimal` + n0 중계). 주소 찾기(pkarr)는 안 쓴다 —
  상대 주소는 `/version` 으로 받고 기기 IP 를 공용 DNS 에 올리지 않는다. 받는 포트는 이 앱의 HTTP 포트와 8777 뿐.
  `KASATERM_KASANET=off` 로 끈다.
- **알리기**: `/version` 과 손님 announce 에 `kasanet: {id, relay, addrs, port}`. 옛 판은 이 칸이 없어 ssh 그대로다.
- **배우기(허용 목록)**: 폴링이 **루프백 base**(앱이 든 ssh 터널·손으로 든 터널·손님 -R)로 받은 `/version` 의 id 만
  허용 목록에 넣는다. LAN 주소를 적은 항목의 평문 HTTP 나, 토큰만 있으면 부를 수 있는 announce 본문은 믿지 않는다
  (announce 는 한 바퀴 앞당기는 신호로만 쓴다). 카사넷으로 들어온 연결은 받는 쪽에서 `127.0.0.1` 로 이어져 ssh -L 과
  같은 신뢰를 얻기 때문이다. 계정 로그인 채널로 배우는 길은 아직 없다.
- **길 고르기** `kasa_net::Route` — base 마다 `127.0.0.1` 입구 하나. 요청하는 자리(기기 폴링·`/term/changes`·거울 ws·
  layout ws·클립보드·announce)가 `route_base(base)` 로 입구를 받는다. 입구는 연결마다 직통이면 카사넷, 아니면 원래 base.
  직통이 2초 버티면 원래 길에 남은 3초 넘은 연결을 1초마다 쓸어 끊어 옮기고(거울은 스스로 다시 붙는다), 직통을
  잃으면(공용 중계만·끊김) 카사넷 연결을 바로 끊어 원래 길로 보낸다. 한 번만 옮기면 그 순간 막 열린 HTTP 재사용 연결이
  끝까지 ssh 에 남았다.
- **믿는 중계**: 데이터를 실어도 되는 중계는 `RelayTrust` 로 정한다. 기본은 국내 중계(`kasa_net::relay::KASA_RELAY`)이고
  `KASATERM_KASANET_TRUSTED_RELAYS`(쉼표로 URL, `off` 면 없음 — 예전과 같다)로 바꾼다. 그 중계로 가는 동안도 카사넷에 싣는다. 호스트·
  포트로 맞추므로 끝 점(`host.`)·기본 포트 차이는 같게 본다. 직통↔믿는 중계는 한 QUIC 연결 안의 경로 바꿈이라 흐르던
  연결을 끊지 않는다.
- **상태**: `/machines` 기기 줄에 `path`(`kasanet`·`kasanet-relay`·`ssh`)와 `path_rtt_ms`(QUIC 경로 왕복). 앱 로그에
  길이 바뀔 때마다 `[kasanet] <base> (<id>) 직통 Nms / 자체 중계 Nms / 공용 중계만 / 끊김` 과 못 붙은 까닭.
- **격리**: 격리 env(`KASATERM_SESSION_FILE`·`_SETTINGS_FILE`·`_COLLAB_ROOT`)가 걸리면 키 경로를 안 줘도 이번 실행 전용
  키로 뜬다. 검증 전용 `KASATERM_KASANET_BIND=127.0.0.1:0`(UDP 를 루프백에만)·`KASATERM_KASANET_STOP_MS`(N ms 뒤 닫기).

### P2 검증 (2026-09-29, 격리 앱 둘, 같은 맥)

- 서명 안 된 디버그 앱이 `0.0.0.0` UDP 를 열자 **중계만** 잡혔다. 방화벽(켜짐·은신 모드) 허용 목록에 P0 측정기만 있고
  리그 바이너리는 없었다 — 묻기 창에 답하기 전까지 들어오는 UDP 가 막힌 것으로 본다. 서명된 설치본은 이 제약이 없고
  (P0 측정기는 허용된 뒤 인터넷 너머 직통이 섰다), 리그는 `KASATERM_KASANET_BIND=127.0.0.1:0` 으로 돌렸다.
- A 명부에 B(루프백 base), B 명부에 A. 5초 안에 서로 배우고 직통(`path=kasanet`, 경로 왕복 0~2ms).
- A 에서 `window-new --machine kyb` 로 B 방을 거울로 열자 A 입구로 들어간 연결 6개가 **전부 카사넷**(B 포트로 들어온
  연결의 여는 쪽이 모두 B 자신의 FwdServer, A 에서 곧장 간 연결 0). B pane 의 `echo` 가 A 거울에 그대로 떴다.
- B 가 카사넷을 닫자(`STOP_MS`) A 가 0.2초 안에 끊김을 보고 카사넷 연결을 끊었다. 5초 뒤 `path=ssh`, 연결 7개가 모두
  원래 길이었고, B pane 의 새 출력이 A 거울에 떴다.
- 남은 것: 두 기기 **양쪽이 새 판**이어야 효과다. 이 맥북↔미니 실사용 측정은 두 기기에 새 판이 깔린 뒤.

## P3 포트 공유 (`crates/kasa-mcp/src/netfwd.rs`)

- **받는 쪽** `GET /net/tcp?port=N` 웹소켓 — 업그레이드 **전에** `127.0.0.1:N` 에 붙어 보고(거부되면 `[::1]:N` 한 번 더.
  `localhost` 로 뜬 Node 17+ 개발 서버는 `::1` 에만 묶인다) 아무도 안 들으면 502, 포트가 틀리면 400. 문은 다른 라우트와
  같은 `origin_guard_mw`(루프백 peer 는 그대로, 원격은 토큰)에 `ws_origin_ok`(남의 페이지가 여는 웹소켓)와 손님 폰 주소
  거부를 더했다. 주인 폰 주소(`MobileAuth` owner)는 통과 — P5 가 관문·폰 입구로 여기를 연다. 포트 목록은 두지 않는다:
  루프백 base·토큰·주인은 이미 셸을 띄울 수 있는 신뢰다.
- **틀**: 바이너리 = 날 바이트. 텍스트 `eof` = 보낸 쪽 TCP 가 쓰기를 닫았다(반쯤 닫기) — 받으면 제 쪽 쓰기를 닫고 계속 읽는다.
  양쪽이 다 `eof` 를 내면 받는 쪽이 Close. 그 밖의 텍스트는 무시, Close·끊김은 전체 닫기. 받는 쪽이 30초마다 Ping(관문
  유휴 끊김). 두 방향은 따로 돈다 — 한쪽 쓰기가 막힌 동안 반대쪽 읽기까지 멈추면 양쪽 받는 버퍼가 차 둘 다 멈춘다.
- **끌어오는 쪽** — 앱 전용 런타임에서 `127.0.0.1:L` 과 `[::1]:L` 을 **같이** 듣는다(v4 만 잡으면 `localhost` 를 `::1` 로
  푸는 브라우저가 그 자리의 다른 서비스로 간다). L 은 원래 번호가 비었으면 그 번호(주소가 같아 OAuth 되돌아오기·쿠키가
  그대로), 쓰이면 빈 번호. 「비었나」는 묶기 성공이 아니라 **붙어 보기**로 본다 — macOS 는 `0.0.0.0:P` 로 뜬 서버가 있어도
  `127.0.0.1:P` 를 묶게 해 줘서 묶기만 보면 그 서버를 가로챈다. `--local` 로 못박은 번호가 쓰이면 오류.
  연결마다 `route_base(기기 base)` 로 상대 `/net/tcp` 를 연다 — P2 입구가 직통이면 카사넷, 아니면 원래 base(ssh). 명부
  이름으로 연 것은 연결마다 base 를 다시 찾는다. 열기 전에 한 번 붙어 봐서 옛 판(404)·빈 포트(502)·거부(403)를 그 자리에서
  말하고, 같은 기기·포트는 다시 쓴다(상한 32개). 끌어오기는 앱 메모리에만 — 앱을 끄면 닫힌다.
- **CLI** `kasaterm-cli net forward <기기> <port> [--local L]` → `http://localhost:L ← 기기:port (카사넷 직통 Nms|ssh 길)`,
  `net list`(길·지금/누적 연결 수, `by` = cli·show), `net stop <L>`(듣던 소켓이 닫힌 뒤 답한다). 소켓 메서드 `net.forward`.
- **보여 주기**: 다른 데스크톱이 이 기기의 localhost 주소를 열 때 그 기기가 포트를 끌어와 호스트 모양은 그대로 두고 포트만
  바꿔 연다. ①거울 되돌림 — 원본 학생의 `open` 이 거울로 밀린 `open-url` 을 보는 쪽 `remote.rs` 가 끌어온 뒤 연다(전에는
  보는 기기 자기 localhost 를 열었다). ②브라우저 기기 — `/browser/resolve-localhost` 가 원본 base 로 먼저 끌어오고, 원본이
  옛 판이면 기존 ssh -L 로 떨어진다(ssh 설정이 없는 기기, 손님 -R 로만 닿는 기기도 이제 된다). 폰은 P5 전까지 임시 터널.
- 검사 `cargo test -p kasa-mcp --lib netfwd` — 왕복·반쯤 닫기 / 업그레이드 전 502·400 / 원격 무토큰·남의 Origin 403 /
  손님 폰 403·주인 통과 / 끌어오기 왕복(300KB)·반쯤 닫기·`::1`·다시 쓰기·닫은 뒤 거부 / 빈 포트·쓰이는 `--local` 오류 /
  주소 고쳐 쓰기.

### P3 검증 (2026-09-29, 격리 앱 둘, 같은 맥)

`KASATERM_KASANET_BIND=127.0.0.1:0`, A 명부에 B·B 명부에 A(루프백 base), B 쪽 더미 HTTP(`python3 -m http.server`).

- 5초 안에 직통(`path=kasanet`). A 에서 `net forward kyb 47913` → 같은 맥이라 47913 이 쓰여 빈 번호 `localhost:61285`.
  `localhost`·`127.0.0.1`·`[::1]` 셋 다 200. 붙들어 둔 연결 중 B HTTP 포트로 들어온 것의 여는 쪽이 **전부 B 자신**(카사넷
  수신기), A 에서 곧장 간 연결 0.
- A 의 `/browser/resolve-localhost`(원본 kyb, ssh 설정 없음) → 기존 끌어오기를 다시 써서 `http://localhost:61285/…?q=1`.
- A 에서 `window-new --machine kyb` 로 B 방을 거울로 열고 B pane 에서 `kasaterm-cli open 'http://localhost:47913/…'`(지금은 `kasaterm-cli share open`) →
  A 가 `http://localhost:61285/…` 를 열었다(B 는 아무것도 안 열었다). 새 포트 `http://127.0.0.1:47914/` 는 새 끌어오기
  `http://127.0.0.1:61900/` 로 — 호스트 모양 유지.
- B HTTP 에 직접: 루프백 무토큰 101, `X-Forwarded-For` 무토큰·틀린 토큰 403, `Origin: https://evil.example.com` 403,
  빈 포트 502.
- `net stop 61900` 뒤 v4·v6 둘 다 연결 거부, 두 번째 stop 은 「끌어오는 것이 없어요」.
- B 가 카사넷을 닫자(`STOP_MS`) `path=ssh`, 같은 끌어오기가 그대로 200 이고 B 포트로 들어온 연결의 여는 쪽이 전부 A
  (원래 base 길). `--local 47915` 로 못박은 번호도 그 번호로 열렸다.

## P5 폰 (`crates/kasa-net-ffi` · `mobile/lib/kasanet.dart`)

- **iOS 정적 라이브러리** `crates/kasa-net-ffi` — C ABI `kasanet_start(키 경로)`·`kasanet_id`·`kasanet_open(/version 의
  kasanet JSON)` → 입구 로컬 포트·`kasanet_state(포트)` → `{path: direct|relay|down, rtt_ms, error}`·`kasanet_close`·
  `kasanet_network_changed`·`kasanet_stop`. 폰은 거는 쪽뿐 — 허용 목록이 비어 들어오는 연결은 다 끊는다. 데스크톱마다
  `127.0.0.1` 입구 하나(`Route::direct_only`): 직통일 때만 싣고, 직통을 잃으면 실던 연결을 끊는다. 관문은 HTTPS 라
  TCP 폴백을 입구 안에 둘 수 없어 길 고르기는 앱이 한다. `mobile/tool/kasanet.sh` 가 기기(arm64)·시뮬레이터(arm64+x86_64
  합본) 조각을 `ios/KasaNet/KasaNet.xcframework` 로 굽고(커밋하지 않는다) `sim.sh`·`phone.sh`·`testflight.sh` 가 먼저 부른다.
  Dart 는 `DynamicLibrary.process()` 로 찾으므로 podspec 이 기호를 `-u` 로 묶고 `STRIP_STYLE=non-global` 로 둔다.
- **키**: 앱 컨테이너 `Library/Application Support/kasanet/kasanet.key`(0600)에만. iOS 앱 환경에는 `HOME` 이 없어
  `NSTemporaryDirectory` 의 위(컨테이너)에서 찾는다. 폰 id 는 데스크톱이 등록마다 새로 배우므로 키가 바뀌어도(백업
  복원·재설치) 다시 등록하면 된다.
- **데스크톱 주소**: 관문 계정 길로 받은 `/version` 의 `kasanet` 칸(기본 기계는 `version`, 다른 기계는 `m/~id/version`).
- **등록(데스크톱이 폰 id 를 배우는 길)** `POST /kasanet/phone {id}` → `{ok, ttl_secs}`. 받는 조건은 셋 — 업링크 입구로 들어와
  (`ViaUplink`, 관문을 거쳤다) 주인 폰 자격(`MobileAuth` owner)이고 카사넷 폰 입구로 온 것이 아닐 것. 관문은 계정 길에서
  기기 토큰을 확인하고 주인 주소(`/u/<주인>/`)로 되쏘므로 이것이 「관문 계정 채널」이다(주인 주소 자체도 같은 무게의 자격 —
  관문을 바꾸지 않고 여기까지 가른다). 로컬·원격 토큰·손님 주소·카사넷으로 온 등록은 403. 등록은 **메모리에만, 수명
  15분**이고 폰이 그 3분의 1마다 관문으로 다시 등록한다 — 관문에서 폐기된 폰은 다시 등록을 못 해 수명 안에 허용 목록에서
  빠지고, 그 폰의 연결은 그때 끊는다(`FwdServer::forget`). 기기(명부)로 배운 id 를 폰으로 덮지 않는다.
- **폰 입구(데스크톱)**: 폰 id 는 `FwdServer::redirect` 로 **이 앱 HTTP 포트만, 폰 입구로** 돌린다 — 카사크롬 다리 등 다른
  포트는 못 연다. 폰 입구 리스너(`via_phone_mw`)는 업링크 되쏘기와 똑같이 주인 주소 아래로 고쳐 쓰고 `ViaUplink` 를 달며
  쿠키·Authorization·원격 토큰·Origin 을 걷는다. 그래서 카사넷으로 온 폰은 관문 경유와 **같은 자격**이다 — 루프백 peer 가
  아니다. 연결을 받을 때 돌리기에 든 상대는 나중에 잊혀도 기본 허용 포트로 새지 않는다.
- **길 고르기(폰)** `KasanetRouter` — 요청마다 `Server.uri` 가 그 기계 입구가 직통이면 `http://127.0.0.1:L/…`, 아니면 관문
  (`relay/account/[m/~id/]…`). 관문이 확인해 주는 값(`machines` 의 살아 있는 기계 목록, `nacho/`)과 배우는 길(`version`·
  `kasanet/`)은 늘 관문. 입구로는 관문 토큰·소켓 인증 부프로토콜을 싣지 않는다. 입구로 간 GET 이 길에서 끊기면 관문으로
  한 번 더 간다(POST 는 다시 안 보낸다 — 키 입력이 두 번 간다). 1초마다 입구 상태를 읽어 직통↔관문이 바뀌면 알리고, 학생
  화면 소켓은 관문에 붙어 있다가 직통이 서면 다시 붙어 옮긴다. 앱이 깨어나면 `network_changed` 와 재등록. 데스크톱이 다시
  떠 폰을 잊었으면(403) 수명을 기다리지 않고 다시 등록하고, 재등록마다 `/version` 부터 다시 받아 포트가 바뀐 데스크톱에도
  입구가 따라간다.
- **앱 안 Safari 화면** 설정 「데스크톱 개발 서버 열기」(포트·경로) → `NetTcpBridge` 가 폰 `localhost:L`(v4·v6, 데스크톱과
  같은 번호를 먼저)을 듣고 연결마다 P3 `/net/tcp?port=N` 웹소켓을 연다 — 길은 위 규칙 그대로(직통이면 입구, 아니면 관문). 틀은
  P3 와 같다(바이너리·`eof`). 그 주소를 `url_launcher` `LaunchMode.inAppBrowserView` 로 연다(iOS `SFSafariViewController`,
  안드로이드는 같은 코드가 크롬 커스텀 탭). 설정 계정 칸에 「데스크톱 길」.
  - **왜 임베디드 웹뷰가 아닌가**(2026-09-29 결정): Safari 화면은 카사텀이 앞에 있는 채로 떠서 카사넷 입구(앱 프로세스의
    Dart 소켓)가 살아 있고, 로그인(쿠키·저장소)이 앱별로 남고, iCloud 비밀번호 자동 채우기가 되고, 구글 OAuth 가 막히지
    않는다(구글은 임베디드 웹뷰 로그인을 거절한다). `webview_flutter` 는 뺐다.
  - **입구 수명 = 받침 화면(`DevServerScreen`)**. `url_launcher` 는 Safari 화면이 닫혀도 알려 주지 않는다(첫 로드가 끝나거나
    그 전에 닫혔을 때만 답한다). 그래서 입구를 세운 작은 Flutter 화면을 밑에 깔고 그 위에 Safari 화면을 띄운다 — 이 화면이
    스택에 있는 동안 입구가 살고, 뒤로 나가면 닫는다. Safari 화면을 닫으면 이 화면으로 돌아와 「다시 열기」를 누를 수 있다.
  - **길 표시**: Safari 화면에는 우리 글을 못 넣는다. 받침 화면이 열기 전 0.7초 동안 「데스크톱 직통 · Nms」/「관문 경유」를
    보이고(번호가 다르면 「이 폰 localhost:L」), 닫고 돌아와서도 같은 줄이 보인다. 첫 로드가 실패하면 그 아래 한 줄.
  - **같은 번호 먼저가 로그인 유지의 조건**: 쿠키는 호스트(`localhost`)에 묶여 포트가 바뀌어도 가지만, localStorage·
    IndexedDB·OAuth 돌아올 주소는 출처(`localhost:포트`)에 묶인다. 폰 번호가 데스크톱과 같아야 앱을 껐다 켜도 같은 출처다.
- 검사 `cargo test -p kasa-net`(돌리기: 원래 포트 안 닿음·표 밖 포트 막힘·잊으면 연결 끊김 / `direct_only` 가 못 실을 때
  닫음) · `cargo test -p kasa-net-ffi`(C ABI 를 앱 순서로: 루프백 데스크톱에 직통으로 싣고, 직통을 잃으면 닫음) ·
  `cargo test -p kasa-mcp --lib phone_ingress`(폰 입구 = 관문 자격·쿠키 걷기 / 카사넷·로컬로 온 등록 403 / 관문 주인 등록 통과)
  · `flutter test test/kasanet_test.dart test/net_tcp_test.dart`(길 고르기·토큰 안 싣기·GET 한 번 더·POST 안 다시·옛 판·거절·
  다른 기계·깨어남·새 포트 따라가기 / 다리 반쯤 닫기·빈 포트).

### P5 검증 (2026-09-29, 로컬 관문 + 격리 데스크톱 + 시뮬레이터, 같은 맥)

`kasa-relay --port 18792`(시험 계정), 격리 앱 `KASATERM_GATEWAY=http://127.0.0.1:18792`·`KASATERM_KASANET_BIND=127.0.0.1:0`
(계정 로그인), 전용 시뮬레이터에 `SIMCTL_CHILD_KASATERM_KASANET_BIND=127.0.0.1:0`(맥 방화벽 묻기 창을 안 띄우려고 — 폰 FFI 도
같은 스위치를 읽는다)로 앱을 띄워 계정 로그인.

- 폰이 관문으로 `/version`·등록 → 데스크톱 로그 `폰 95ff7bac4d 허용`, 설정 「카사넷 직통 · 1ms」. 폰에서 친 명령이 격리
  데스크톱 셸에서 돌았고(`p5-via-kasanet 42`), 붙든 연결이 폰 입구 쪽 6개·업링크 입구 쪽 5개(관문 전용 길과 직통 전 요청).
  같은 등록을 데스크톱 HTTP 에 직접 보내면 403.
- 데스크톱이 카사넷을 닫자(`STOP_MS`) 폰 입구 연결 0, 학생 화면이 관문으로 다시 붙어 명령이 그대로 돌았다(`VIA-GATEWAY`),
  설정 「관문 경유 — 직통을 찾는 중」. 데스크톱을 새 포트로 다시 띄우자 24초 뒤 스스로 다시 직통(403 → 재등록 → 새 포트).
- 웹뷰: 격리 데스크톱 쪽 `127.0.0.1:4719`(더미 개발 서버, 200KB JSON fetch) → 「데스크톱 직통 · 1ms」로 열림. 데스크톱을
  카사넷 끈 채로 다시 띄우자 같은 화면이 「관문 경유」로 열림. 시뮬레이터는 맥과 망을 같이 써 4719 가 이미 맥 쪽에 잡혀
  폰 쪽 번호는 빈 번호였다 — 실기에서는 같은 번호가 된다.
- 남은 것: 실기 아이폰(셀룰러·다른 망)에서 직통 성립·왕복 실측은 TestFlight 판으로.

### 집 밖(셀룰러) 직통이 왜 안 서나 — 맥북 집 망 실측 (2026-10-02)

- **맥북 집 망은 이중 NAT 다.** `traceroute` 첫 두 홉이 `192.168.0.1`(ipTIME) → `192.168.219.254`(통신사 공유기),
  ipTIME 이 UPnP 로 아는 바깥 주소가 `192.168.219.101`(사설). 공인 `180.224.72.73` 은 바깥 공유기의 것이다.
- iroh 포트매퍼는 ipTIME 에 UPnP 구멍을 낸다(`iroh-portmap`, 맥북 `53151` → 바깥 `45381`·`36224`). 그러나 그 바깥은 아직
  사설망이라 인터넷에서 안 닿는다 — `/version` 의 `192.168.219.101:45381` 이 그 주소다. 맥미니(다른 망)에서 맥북 공인
  `180.224.72.73:53151` 로 직통 시험을 걸면 20초 무응답이다.
- 그래서 셀룰러 폰은 **양쪽이 동시에 구멍 뚫기**가 되어야만 직통이다: 집 쪽 두 겹 NAT + 통신사 CGNAT. 통신사 NAT 이
  목적지마다 포트를 바꾸면(대칭) 실패하고 관문으로 간다 — 이 판정은 실기 폰에서만 볼 수 있다(설정 계정 칸 「데스크톱 길」).
- ipTIME 표에 `iroh-portmap` 이 수명 0(영구)으로 40개 넘게 쌓여 있었다(맥북 24·192.168.0.8 16). 포트매퍼는 2시간 임대를
  청하지만 ipTIME 은 수명을 무시하고, 매퍼는 앱이 꺼질 때 지우지 않으며, 갱신(1시간) 때 같은 바깥 포트를 못 얻으면 새로
  하나 더 만든다(받는 포트 하나에 13칸까지 쌓였다). 표가 차면 새 매핑이 실패한다.
  → `kasa_net::portmap` 이 데스크톱·폰 엔드포인트 곁에서 90초 뒤 한 번, 그 뒤 30분마다 치운다: 이 기기 사설 IP·이름
  `iroh-portmap`·UDP 칸만 보고, 지금 받는 포트는 광고 중인 바깥 포트 하나만 남기며, 이 기기에서 아무도 안 쥔 받는 포트의
  칸은 지운다(다른 프로세스가 쥔 포트는 둔다). 실측 공유기에서 표 48칸 → 25칸, 맥북 몫 24칸 → 1칸(광고 중인 것).
- 집 밖 직통을 세우는 길: ① 통신사 공유기를 브리지로 두거나 ipTIME(`192.168.219.101`)을 DMZ 로 지정 — 이중 NAT 이 풀리면
  UPnP 구멍이 그대로 인터넷에 열린다. ② ipTIME 을 AP 모드로. ③ 집 망을 못 만지면 포트가 열린 국내 자리(서울 VPS)에 데이터를
  싣는 자체 중계 — 10-02 에 세웠다(아래 「국내 중계」). 직통이 안 서도 관문 대신 국내 중계로 간다.

### P5 보여 주기 (폰 대상, 2026-09-29)

- 브라우저 기기가 「폰」이고 주소가 **이 기기 localhost**(`localhost`·`127.x`·`[::1]`·`0.0.0.0` → `localhost`)면 임시 터널을
  세우지 않고 주소를 그대로 쪽지(link)에 넣는다 — `quicktunnel::in_app_url`. HTTP `/open-url`(카사크롬 `browser_show_human`·
  `open` 셰임, 답에 `in_app: true`)과 앱의 `open_url_to_phone`(CLI `open`) 둘 다 이것을 먼저 본다. 사설망 주소·다른 호스트는
  예전처럼 임시 터널이다(P3 도 이 기기 localhost 만 끌어온다).
- **옛 판 폰 가르기**: 새 판 폰 앱만 카사넷 등록을 하므로, 등록이 오면 그 시각을 키 옆 `kasanet-phone-app.json` 에 남기고
  30일 안에 등록한 적이 있을 때만 터널을 건너뛴다(`kasanet::phone_app_opens_localhost`). 등록 수명(15분)으로 가르면 폰 앱이
  잠든 사이의 보여 주기가 터널로 떨어진다. 격리 인스턴스가 이번 실행 전용 키로 떴으면 파일 없이 메모리에만.
- **폰**: 쪽지·알림 링크가 localhost 면(`desktopLocal`) 입구를 세운 받침 화면(`DevServerScreen`)에서 앱 안 Safari 화면으로 그
  쪽지를 낸 기계의 포트를 연다(그 밖의 주소는 입구 없이 바로 앱 안 Safari 화면). 폰에는 제 localhost 서버가 없으니 쪽지의
  localhost 는 늘 그 데스크톱이다. 길은 위와 같다(직통이면 카사넷, 아니면 관문).
- 검사 `cargo test -p kasa-mcp --lib quicktunnel`(등록 전에는 터널, 뒤에는 localhost·`0.0.0.0`·`[::1]` 만 그대로, 사설망·
  `.local`·바깥 주소·http 아닌 것은 터널) · `flutter test test/shown_link_test.dart test/dev_server_test.dart`(바깥 주소도 앱 안
  Safari 모드, localhost 는 길 표시 뒤 데스크톱과 같은 번호로 열고 받침 화면을 닫으면 입구도 닫힘).
- 검증(격리 리그, 브라우저 기기 = 폰): 폰 등록 뒤 `/open-url?url=http://localhost:4719/?from=show-http` → 답 `in_app: true`,
  CLI `open http://0.0.0.0:4719/?from=cli` → 쪽지 둘 다 `http://localhost:4719/…`(「폰 앱 안에서 연다」). 폰 쪽지를 누르자 웹뷰가
  「데스크톱 직통」으로 `?from=show-http` 를 열었다. 그동안 cloudflared 0개.

### P5 앱 안 Safari 화면 검증 (2026-09-29, 로컬 관문 + 격리 데스크톱 + 전용 시뮬레이터)

리그는 위 P5 검증과 같다(`kasa-relay --port 18793`, 시험 계정, 격리 앱 `KASATERM_KASANET_BIND=127.0.0.1:0`, 전용 시뮬레이터에
`SIMCTL_CHILD_KASATERM_KASANET_BIND`). ⚠️ `kasa-relay account add` 는 `$HOME/.config/kasaterm/` 에 적지만 관문은 **`--state`
파일 옆**의 `relay-accounts.json` 을 읽는다 — 리그는 계정 파일을 상태 파일 옆에 두어야 로그인이 된다.

- 더미 개발 서버(맥 `127.0.0.1:4721`, 방문마다 `Max-Age` 쿠키를 심고 받은 쿠키·localStorage 를 보임) → 설정 「데스크톱 개발
  서버 열기」: 받침 화면 「데스크톱 직통 · 1ms」 뒤 Safari 화면이 「NEW VISIT」. 느린 자원을 붙든 동안 더미에 붙은 쪽은 격리
  데스크톱 프로세스, 그 `/net/tcp` 는 폰 입구 리스너(카사넷)로 들어왔고 관문 연결은 데스크톱 업링크 1개뿐(폰 → 관문 0).
- Safari 화면을 닫으면 받침 화면(길 줄·「다시 열기」)으로 돌아옴. 앱을 끝내고(`simctl terminate`) 다시 켜 같은 서버를 열자
  「COOKIE KEPT」 — 앱 안 Safari 화면의 쿠키는 앱을 껐다 켜도 남는다. localStorage 는 비었다: 시뮬레이터는 맥과 루프백을 같이
  써 4721 이 맥 쪽에 잡혀 있어 폰 번호가 매번 빈 번호(52664 → 55220)였다 — 출처가 바뀐 것. 실기는 같은 번호가 된다.
- 쪽지 경로: 격리 앱 `/open-url?url=http://localhost:4721/?from=note`(브라우저 기기 = 폰) → `in_app: true`, 폰 쪽지를 누르자
  받침 화면 「데스크톱 직통 · 0ms」 뒤 Safari 화면이 `?from=note` 를 쿠키와 함께 열었다.
- ⚠️ 시뮬레이터 착시: **빈 포트**를 열면 폰 입구가 그 번호를 잡고(맥에 비어 있으니), 데스크톱 `/net/tcp` 가 그 번호로 붙어 폰
  입구로 되돌아가는 고리가 돈다(Safari 가 계속 로딩). 폰과 데스크톱이 루프백을 같이 쓰는 시뮬레이터에서만 생긴다. 받침
  화면을 닫으면 입구가 닫혀 고리가 끝나고, 돌아온 화면에 「첫 화면을 못 받았어요」가 뜬다.
- 남은 것: 실기 아이폰에서 같은 번호·iCloud 비밀번호 자동 채우기·구글 로그인 확인(TestFlight 판).

## 국내 중계 (2026-10-02, 네이버 클라우드 서울)

직통이 안 서는 자리(셀룰러 폰, 이중 NAT 집 망)에서 관문(Cloudflare, 홍콩·도쿄 edge) 대신 서울에 둔 iroh 중계로 데이터를
싣는다. 직통 → 국내 중계 → 관문(폰)·ssh(데스크톱) 순서다.

### 구성

- 서버: 네이버 클라우드 한국 VPC `kasanet`(10.0.0.0/16) · 공개 서브넷 `kasanet-pub`(10.0.1.0/24, KR-1) · Micro `mi1-g3`
  (vCPU 1, 1GB, Ubuntu 24.04, 기본 스토리지 10GB) `kasanet-relay-kr`, 공인 IP `211.233.212.132`. 요금: 서버는 2027-05 말까지
  무료(그 뒤 월 10,850원), 공인 IP 월 4,032원.
- 주소 `https://relay-kr.debimarlene.com/` — Cloudflare DNS A 레코드, **프록시 끔(DNS only)**. 프록시를 켜면 UDP 7842 가 안
  닿고 TLS 를 Cloudflare 가 끝내 ACME 가 실패한다.
- ACG `kasanet-relay`: 들어오는 것은 443/tcp(중계·ACME TLS-ALPN-01)·80/tcp(사로잡힌 포털 확인 `/generate_204`)·7842/udp(QUIC
  주소 찾기)·22/tcp 뿐. 나가는 것은 tcp/udp 전체(ACME·apt).
- SSH: 서버 생성 때 Init Script `kasanet-relay-ssh` 가 사용자 `kasanet` 에 이 맥북 공개 키(`~/.ssh/kasanet_relay.pub`)만 넣고
  비밀번호·root 로그인을 끈다. 이 맥 `~/.ssh/config` 별칭 `kasanet-relay`. 서버 생성에 필수인 네이버 인증키(관리자 비밀번호
  해독용)는 `~/.ssh/kasanet-relay.pem`(0600) — 쓰지 않지만 콘솔 접속 복구용으로 둔다.
- 중계: `iroh-relay` 1.3.0(앱의 iroh 와 같은 판, 공식 릴리스 바이너리 — sha256 을 GitHub 자산 digest 와 맞춰 받는다),
  systemd `kasanet-relay`(사용자 `kasanet-relay`, `CAP_NET_BIND_SERVICE` 만), 설정 `/etc/kasanet-relay/relay.toml`, 인증서는
  Let's Encrypt 를 중계가 직접(TLS-ALPN-01) 받아 `/var/lib/kasanet-relay/certs` 에 두고 스스로 갱신한다. QUIC 주소 찾기를 켠다 —
  예전 터널 너머 중계는 이것이 없어 직통이 안 섰다. 서버 IPv6 가 꺼져 있어 `0.0.0.0` 으로만 묶는다(`[::]` 면 안 뜬다).
  같은 날 관문도 이 서버로 옮겨 443 은 nginx 가 SNI 로 가른다 — 중계 https 는 `127.0.0.1:8443`, TLS 는 풀지 않고 넘기므로 중계의
  ACME 도 그대로다(`docs/seoul-gateway.md`). 그 뒤로 잰 강제 중계 왕복 15ms 로 같다.
- 운영은 `tools/kasanet-relay/relay.sh`(이 맥에서): `install`(바이너리·설정·systemd), `allow <id> <이름>`·`deny <id>`(허용 목록을
  고치고 다시 켠다 — 붙은 기기는 몇 초 끊겼다 다시 붙는다), `ids`(이 맥·명부 기기·이 맥에 등록한 폰의 id), `status`.

### 허용 목록 — 아무나 못 쓰게

iroh-relay 문서의 접근 제어. 중계 접속 때 기기 비밀키로 서명하므로 id 를 흉내 낼 수 없다. 모르는 키는
`The relay denied our authentication (not authorized)` 로 끊긴다(실서버 확인). 처음엔 `access.allowlist` 였으나 중계 로그에 거절된
id 가 안 남고 고칠 때마다 중계를 다시 켜야 해서, 같은 날 `access.http` → 서버 안 `access.py`(127.0.0.1:9101, systemd
`kasanet-relay-access`)로 바꿨다 — 접속마다 `/etc/kasanet-relay/allowlist` 를 읽고 허용·거절을 journal 에 남긴다(어느 쪽이든
실패하면 거절, 앱은 n0 로 돌아간다).

- 지금 목록: 회사 맥북 `85ad3935…`, 맥미니 `4d0e17c0…`, 폰 `3e6bdd4f…`(10-02 14:40 넣음, 14:40:37 허용 접속). 개인 맥북·윈도우는 아직 없다.
- 넣는 법: `relay.sh ids` 로 id 를 보고 `relay.sh allow <id> <이름>`. 다른 기기 id 는 이 맥의 루프백 base(`/machines`)로 받은
  `/version` 에서 — 앱이 허용 목록을 배우는 길과 같다.
- **폰**: 데스크톱이 관문 계정 채널로 확인한 폰 등록(`POST /kasanet/phone`)마다 그 id 를 `kasanet-phone-app.json` 의
  `phones` 에 남긴다(새 판 데스크톱). 옛 판 데스크톱이면 앱 로그의 `[kasanet] 폰 3e6bdd4f52 허용`(앞 10자리)과 중계 거절 기록을
  맞춘다 — `relay.sh denied` 로 보고 `relay.sh allow 3e6bdd4f52 폰` 처럼 앞자리로 넣으면 거절 기록에서 전체 id 를 찾는다.
- 폰이 국내 중계로 **데이터를 싣는** 것은 상대 데스크톱도 새 판(홈 중계가 국내 중계)일 때다 — 폰은 데스크톱이 알린 중계 주소로
  가므로, 옛 판 데스크톱(홈 aps1)과는 직통이 아니면 관문이다.
- 목록에 없는 기기(`kasa_net::relay::fall_back_when_denied`): 거절을 보면 국내 중계를 지도에서 빼 n0 를 홈으로 쓰고 30분 뒤 다시
  넣어 본다 — 목록 밖 기기는 이 중계를 넣기 전과 똑같이 동작하고, 목록에 막 든 기기는 앱을 다시 켜지 않아도 옮겨 온다.
  거절된 채로 같은 중계를 계속 두드리면 구멍 뚫기 신호까지 잃는다.

### 앱

- 지도 `kasa_net::relay::relay_mode` = n0 기본 4곳 + 국내 중계(QUIC 주소 찾기 켬). iroh 는 잰 지연으로 홈 중계를 고르니 국내
  중계(왕복 6~8ms)가 aps1(170ms)을 이긴다. 믿는 중계(`RelayTrust`) 기본값도 국내 중계 — 직통이 아니어도 국내 중계로 가면 싣는다.
  `KASATERM_KASANET_TRUSTED_RELAYS`(쉼표로 URL, `off`) 로 바꾼다.
- 데스크톱: `/machines` 의 `path` 가 `kasanet-relay`, 앱 로그 `[kasanet] <base> (<id>) 자체 중계 Nms`.
- 폰: FFI `kasanet_state` 의 `path` 가 `kasa_relay`(입구로 싣는다). 설정 계정 칸 「카사넷 국내 중계 · Nms」, 개발 서버 받침 화면
  「데스크톱 국내 중계 · Nms」. 직통이면 예전처럼 「직통」.
- 검사 `cargo test -p kasa-net relay`(로컬 iroh-relay 를 띄워: 믿지 않는 중계로는 붙어도 안 싣고 믿는 중계로는 입구가 싣는다 /
  허용 목록 밖이면 지도에서 빼고 다시 넣어 또 거절) · `flutter test test/kasanet_test.dart`(국내 중계도 입구로, 직통↔중계 바뀜 알림).

### 실측 (2026-10-02, 회사 맥북 = 집 망 이중 NAT ↔ 맥미니 = 다른 망)

`kasa-net-probe` 시험 키(측정 뒤 허용 목록에서 뺐다). 「국내 중계 강제」는 거는 쪽 IP 전송을 걷어 중계 말고 길이 없게 했다.

| 길 | 성립 | 핸드셰이크 | datagram p50/p90 | 스트림 p50/p90 | 올리기/내리기 |
|---|---|---|---|---|---|
| 국내 중계 강제, 맥북→미니 | 3/3 | 16~25ms | 14.4/26.1ms (유실 0) | 19.0/35.5ms | 32.2/27.6 Mbps(20MB) |
| n0 aps1 강제, 같은 시각 | 2/2 | 180~218ms | 176.6/205.6ms | 181.4/243.8ms | 11.0/2.6 Mbps(5MB) |
| 직통, 국내 중계만 둠 | 3/3, 직통까지 65~100ms | 14~15ms | 13.9/39.3ms | 13.2/24.4ms | 37.5/17.5 Mbps(5MB) |

- 국내 중계만 둬도 직통이 선다 — QUIC 주소 찾기로 공인 주소를 배워서다(터널 너머 중계는 0/3). 직통까지 걸린 시간도 aps1 신호
  때(380ms 대)보다 짧다.
- 중계 서버까지 TCP 연결 6~9ms(맥북)·4~7ms(미니), 따뜻한 HTTPS 왕복 8ms. 관문(`kasaterm.debimarlene.com`) 따뜻한 왕복은 91ms.

앱 길(같은 시각, `app_lat.py` — `/version` 왕복, 기존 pane 거울 첫 화면, 새 웹 셸 키→화면 반향. 웹 셸은 재고 지웠다):

| 길 | 데스크톱 요청 왕복 | 학생 화면 첫 장 | 키→화면 반향 p50 / p90 / 최대 |
|---|---|---|---|
| 관문(맥북→Cloudflare→미니 관문→Cloudflare→맥북), 2회 | 254~261ms | 399~440ms | 184~194 / 260~282 / 305~409ms |
| 국내 중계(미니→서울 중계→맥북 앱, 앱과 같은 `Link`·`Route` 입구), 3회 | 14~20ms | 44~125ms | 17~18 / 35~113 / 88~424ms |
| 참고: 맥북 안 루프백 | 0ms | 62ms | 1 / 69 / 222ms |

- 국내 중계 길은 루프백보다 15~20ms 더 들고, 관문 길은 190~250ms 더 든다. 첫 화면은 데스크톱 쪽 화면 만들기(루프백 62ms)가 섞여
  흔들린다.
- 셀룰러 폰은 폰→서울 중계 구간이 LTE 몫(수십 ms)만큼 늘어난다 — 실기 확인은 폰 설정 계정 칸 「데스크톱 길」.

## 자체 중계 (2026-09-29, 맥미니)

**2026-09-29 걷음** — 미니 launchd·바이너리·설정, 터널 ingress, DNS 레코드를 모두 지웠다. 국내 중계는 포트가 직접 열린
자리에 세웠다(위 「국내 중계」). 아래는 그때의 구성과 실측 기록이다.

**결론: Cloudflare 터널 너머 중계는 동작하지만(웹소켓·허용 목록 확인) 국내 저지연 중계가 못 된다. 앱에 넣지 않는다** —
릴레이 맵은 n0 기본 그대로, `KASATERM_KASANET_TRUSTED_RELAYS` 에도 넣지 않는다. 국내 중계는 공인 UDP·TCP 포트가 직접
열린 자리(공유기 포트포워딩한 집 기기, 서울 VPS)에 TLS 와 QUIC 주소 찾기를 켜서 세워야 한다.

### 구성

- 주소 `https://relay.debimarlene.com/` — **끝 점 없이**. `relay.debimarlene.com.` 로 부르면 터널 호스트 규칙이 안 맞아 404.
- 미니 launchd `com.geono.kasanet-relay` → `~/.cargo/bin/iroh-relay` 1.3.0(`cargo install iroh-relay --version 1.3.0
  --features server --locked`), 설정 `~/.config/kasanet-relay/relay.toml`, `127.0.0.1:8796` 평문 HTTP, 로그 `/tmp/kasanet-relay.err`.
  TLS 는 Cloudflare 가 끝낸다. QUIC 주소 찾기(UDP 7842)는 터널이 UDP 를 못 넘겨 끄고, metrics 도 끈다.
- 미니 터널 `~/.cloudflared/kasaterm-gateway.yml` 의 404 줄 앞에 `relay.debimarlene.com → http://127.0.0.1:8796`
  (백업 `.bak-20260929-relay`). 바꾼 뒤 `launchctl kickstart -k gui/$(id -u)/com.geono.kasaterm-gateway-tunnel` — 폰 관문·
  ssh 입구가 5초쯤 끊긴다. DNS CNAME 은 `cert.pem` 이 있는 회사 맥북에서
  `cloudflared tunnel --config /dev/null route dns 613e1da6-1ee5-4a78-ba55-90ad5b432875 relay.debimarlene.com`.
- 걷을 때: `launchctl bootout gui/$(id -u)/com.geono.kasanet-relay`, 터널 ingress 두 줄 빼고 kickstart, DNS 레코드 삭제.

### 허용 목록

중계는 `[access] allowlist` 의 EndpointId 만 받는다. 중계 접속 때 비밀키로 서명하므로 id 를 흉내 낼 수 없다.

1. 기기에서 `kasa-net-probe id --key ~/.config/kasaterm/kasanet.key` — 앱이 쓰는 기기 키의 공개키. 없으면 만든다(0600, 앱이 그대로 쓴다).
2. 미니 `relay.toml` 의 `allowlist` 에 한 줄 + 기기 이름 주석, `launchctl kickstart -k gui/$(id -u)/com.geono.kasanet-relay`.

지금 목록: 회사 맥북 `85ad3935…`, 맥미니 `4d0e17c0…`. 개인 맥북·윈도우는 아직 없다.
실측: 모르는 키는 중계 로그에 `The relay denied our authentication`, 거는 쪽은 중계에 못 붙고 연결이 시간 초과로 끝났다.
임시 키를 넣었다 빼고 kickstart 하자 다시 거부됐다.

### 실측

`kasa-net-probe … --relay https://relay.debimarlene.com/ [--n0]` — `--relay` 는 그 중계 하나만, `--n0` 은 n0 중계도 함께.
같은 시각에 aps1 과 나란히 쟀다(회사 맥북, 20MB).

| 길 | 성립 | datagram p50/p90 | 스트림 p50/p90 | 올리기/내리기 |
|---|---|---|---|---|
| 자체 중계 강제, 맥북→미니 | 3/3 | 454/564ms (유실 2/200) | 496/813ms | 6.1/6.3 Mbps |
| aps1 강제, 맥북→미니 | 2/2 | 172/174ms | 173/181ms | 8.9/8.8 Mbps |
| 자체 중계 강제, 맥북↔맥북 | 2/2 | 167/169ms | 167/170ms | 18.3/18.8 Mbps |
| aps1 강제, 맥북↔맥북 | 2/2 | 182/184ms | 182/184ms | 8.9/8.7 Mbps |
| 직통, 자체 중계만 | **0/3**(15초) | 계속 중계 691ms | | |
| 직통, 자체 중계+n0 | 3/3, 380~1111ms | 8.7/9.8ms | 8.6/9.5ms | (5MB) 88/119 Mbps |

- **Cloudflare 가 서울 edge 를 안 준다.** 터널 커넥터는 icn 인데, 들어오는 쪽 edge 는 회사 맥북이 HKG·NRT(요청 왕복 85ms),
  미니(KT 백본 `112.174.x`)는 미국 ATL·MIA(traceroute 135ms, 요청 왕복 567ms)다. 중계 한 번은 양 끝이 각자 edge 를
  왕복하므로, 미니가 끼면 aps1 의 세 배 가까이 느리다. 둘 다 아시아 edge 로 가는 기기 쌍만 aps1 과 비슷하고 처리량이 두 배.
- **자체 중계만 두면 직통이 안 선다.** 공인 주소는 중계의 QUIC 주소 찾기로 배우는데 터널 너머라 그것이 없다 — 알리는
  주소가 LAN 주소뿐이라 구멍 뚫기 후보가 없다(n0 를 섞으면 미니 알림에 `218.153.32.129` 가 붙고 직통이 섰다).
- **섞으면 자체 중계는 홈 중계로 안 뽑힌다.** iroh 1.3 은 중계마다 잰 지연으로 홈을 고르는데(`net_report`
  `add_report_history_and_set_preferred_relay`), 주소 찾기가 없는 중계는 HTTPS 탐침(매번 새 TLS 연결 + `/ping`)으로만 재
  200ms 를 넘고 UDP 로 재는 aps1 을 못 이긴다. 맥북·미니 모두 3/3 aps1 을 홈으로 골랐다 — 자체 중계로는 아무것도 안 지난다.
- 그래서 믿는 중계로 넣어도 쓰이지 않거나(섞을 때), 직통을 잃고 ssh 길보다 느린 중계에 묶인다(혼자 둘 때).
