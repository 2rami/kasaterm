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
- 중계: iroh 기본 공용 중계. 내용은 종단 암호화라 중계는 메타데이터만 본다. Cloudflare 터널 너머 자체 중계는 세워 재 봤으나
  이득이 없어 앱에 넣지 않았다 — 아래 「자체 중계」.

## 계약

- **신원**: 기기 비밀키는 `~/.config/kasaterm/kasanet.key`(0600). 로그·응답·세션 파일에 싣지 않는다.
  격리 인스턴스(검증 앱)는 별도 경로를 쓴다 — 본판과 같은 키로 뜨면 상대가 두 기기를 한 기기로 본다.
- **허용 목록**: 들어오는 연결은 기존 기기 채널(명부·ssh 길·계정 로그인)로 배운 EndpointId 만 받는다.
  모르는 EndpointId 는 스트림을 열기 전에 끊는다.
- **포워드**: 받는 쪽은 항상 `127.0.0.1:<port>` 로만 잇는다. 허용 포트는 카사텀(8765)·카사크롬(8777)과
  사용자가 명시로 공유한 포트뿐이다. 보내는 쪽은 `127.0.0.1:<로컬 포트>` 로 연다 — 개발 서버 bind,
  secure context, OAuth 되돌아오기 주소가 localhost 그대로 동작하게.
- **길 선택**: 기기 base 는 바꾸지 않는다. 요청하는 자리가 base 대신 그 base 의 로컬 입구로 가고, 입구가
  연결마다 **직통이면 카사넷, 아니면 원래 base(ssh 터널)** 로 잇는다. 공용 중계로는 데이터를 싣지 않는다.
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
| P5 | 폰: 앱 안 웹뷰가 카사넷으로 페이지 받기 | 폰에서 맥북 개발 서버 보기 |

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
  입구 규칙(직통이면 카사넷, 아니면 관문). 앱 안 웹뷰는 그 입구와 P3 `/net/tcp` 로 데스크톱 개발 서버를 연다.

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
- **믿는 중계**: 데이터를 실어도 되는 중계는 `RelayTrust`(기본 비어 있음)로 정한다. `KASATERM_KASANET_TRUSTED_RELAYS`
  (쉼표로 URL) 또는 `kasanet::trust_relays` 로 자체 중계 주소를 넣으면 그 중계로 가는 동안도 카사넷에 싣는다. 호스트·
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
- A 에서 `window-new --machine kyb` 로 B 방을 거울로 열고 B pane 에서 `kasaterm-cli open 'http://localhost:47913/…'` →
  A 가 `http://localhost:61285/…` 를 열었다(B 는 아무것도 안 열었다). 새 포트 `http://127.0.0.1:47914/` 는 새 끌어오기
  `http://127.0.0.1:61900/` 로 — 호스트 모양 유지.
- B HTTP 에 직접: 루프백 무토큰 101, `X-Forwarded-For` 무토큰·틀린 토큰 403, `Origin: https://evil.example.com` 403,
  빈 포트 502.
- `net stop 61900` 뒤 v4·v6 둘 다 연결 거부, 두 번째 stop 은 「끌어오는 것이 없어요」.
- B 가 카사넷을 닫자(`STOP_MS`) `path=ssh`, 같은 끌어오기가 그대로 200 이고 B 포트로 들어온 연결의 여는 쪽이 전부 A
  (원래 base 길). `--local 47915` 로 못박은 번호도 그 번호로 열렸다.

## 자체 중계 (2026-09-29, 맥미니)

**2026-09-29 걷음** — 미니 launchd·바이너리·설정, 터널 ingress, DNS 레코드를 모두 지웠다. 국내 중계는 포트가 직접 열린
자리에서 세운다. 아래는 그때의 구성과 실측 기록이다.

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
