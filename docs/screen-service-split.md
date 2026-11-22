# 화면 보내기 서비스 분리 — 설계

상태: **설계 확정(2026-11-21), 단계별 구현 중**. 화면 스트림의 보내는 쪽(뜨기·압축·조작 넣기·소리·카사넷 입구)을 카사텀 앱에서 떼어 따로 도는
작은 서비스로 옮기는 길이다. 계약·실측은 [screen-stream.md](screen-stream.md), 길은 [kasanet.md](kasanet.md), JPEG 거울은
[screen-mirror.md](screen-mirror.md), 업데이트는 [automatic-preview-updates.md](automatic-preview-updates.md)가 정본이다.

표시: **[확인]** 은 코드·문서·실측으로 확인한 사실, **[추측]** 은 확인하지 않은 판단, **[잴 것]** 은 단계 P0 에서 실측으로 가를 것이다.

## 왜

- **앱을 업데이트·재시작해도 화면 공유가 안 끊긴다.** 지금은 화면 입구가 앱의 카사넷 엔드포인트에 붙어 있어 앱이 다시 뜨면
  스트림이 같이 죽는다.
- **권한이 자주 안 바뀌는 작은 실행 파일에 붙는다.** 화면 기록·손쉬운 사용 권한을 앱 상태와 따로 쥔다(맥미니가 권한 거부로 안
  보이던 일). 권한을 켠 뒤 다시 켜야 할 것도 앱이 아니라 서비스다.
- **카사텀 창을 안 띄운 기계도 화면만 보낸다**(맥미니 같은).
- 순서: **Windows 먼저**(이미 SYSTEM 서비스 `KasatermScreen`·도우미가 있다), 그다음 맥. 보는 쪽 별도 앱은 그 뒤 선택이다(끝 절).

## 지금 — 확인한 사실

| 일 | macOS | Windows |
|---|---|---|
| 뜨기·압축 | 앱 프로세스 `host.rs`(SCK 420v → VT HEVC/H.264) | SYSTEM 도우미 `kasaterm.exe --screen-agent`(DXGI 복제·MF). 도우미가 없으면 앱이 직접(`win.rs start_local`) |
| 조작 넣기 | 앱 `screen_input`(CGEvent) | 도우미(SendInput·`SendSAS`), 없으면 앱 |
| 시스템 소리 | 앱(SCK `capturesAudio`) | 앱(WASAPI 루프백, 사용자 세션) |
| 원본 스피커 끄기 | 앱(`source_mute.rs`, 표 `stream-muted` 는 앱 설정 폴더) | 앱 |
| 카사넷 입구 `kasa/screen/1` | 앱 엔드포인트(`kasa-device kasanet.rs ScreenAccept`) | 앱 엔드포인트 |
| `/screen/ws`(폰, JPEG·영상) | 앱 HTTP | 앱 HTTP |
| 화면 깨워 두기 | 앱(`IOPMAssertionDeclareUserActivity`) | 없음 |

- [확인] 입구는 앱 엔드포인트 하나에 `fwd` 와 나란히 걸려 있고, 허용 목록 훅을 지난 뒤에도 **명부로 배운 기기(`links`)와 등록된 주인
  폰(`phones`)만** 받는다. `/version` 의 `kasanet.screen` 은 입구를 꽂았을 때만 참이다. 보는 쪽은 `learn` 이 적은 그 값과
  앱 링크의 길(직통·믿는 중계)로 `can_view` 를 가른다.
- [확인] Windows 서비스·도우미도 `kasaterm.exe` 다. 그래서 이름·경로로 앱을 세는 곳(`app_restart::ps_running_fn`, 기기에 둔 로그인
  자동 실행 스크립트)이 `--screen-service`·`--screen-agent` 를 따로 빼야 했다. 도우미 파이프는 같은 exe(`same_exe`)만 받는다.
- [확인] MSI 는 `MajorUpgrade Schedule='afterInstallInitialize'`(옛 판을 먼저 다 지우고 새 판을 깐다) + 서비스 구성요소의
  `ServiceControl Stop='both' Remove='uninstall'` 이다. **지금은 MSI 업데이트마다 서비스가 멈추고 지워졌다가 다시 깔린다.**
- [확인] 맥에서 칸 셸·학생의 책임 프로세스는 `kasaterm.app` 이다. TCC 는 허락할 때의 서명 요구조건(csreq)을 함께 적어 두고, 서명
  신원이 바뀌면 설정 스위치가 켜진 채로 거부한다(자체 서명 → Developer ID `L366799VND` 로 바뀐 때 실측). 권한을 켠 뒤에는 그
  프로세스를 다시 열어야 판정이 바뀐다.
- [확인] 앱 최소 macOS 는 11.0(`build-app.sh`). SCK 소리는 13 부터, `SMAppService` 도 13 부터다.

## 목표와 범위 밖

- 목표: **보내는 쪽 전부를 서비스로**. 보는 쪽과의 선 위 계약(`kasa/screen/1` 의 제어·장·소리 데이터그램)은 바꾸지 않는다.
- 앱에 남는 것: 보는 칸(`view.rs`·`pane.rs`·`window.rs`·`input.rs`·풀기), 명부·카사넷 `fwd`, 설정 화면, 폰 `/screen/ws` 입구(서비스에서 장을
  받아 넘긴다).
- 범위 밖: 카사넷 `fwd`·PTY 를 앱 밖으로 내기(그것은 「카사넷 데몬」이고 layout 권한 이중화로 접은 적이 있다), 안드로이드 영상 풀기,
  맥 로그인 창 화면.
- 학생 도구의 화면 뜨기(`screencapture`·`kasaterm-cli` 화면 판독)는 책임 프로세스가 앱이라 **앱의 화면 기록 권한은 계속 필요하다**.
  분리는 스트림이 그 권한·앱 수명에 기대지 않게 하는 것이지, 앱 권한을 없애는 것이 아니다.

## 1. 프로세스 배치

### 새 실행 파일 `kasaterm-screen`(가칭)

앱과 다른 이름의 작은 실행 파일 하나다. `kasa-device`(계정·명부·관문)에 기대지 않고 `kasa-net` 과 아래 두 크레이트만 쓴다 —
의존이 작아야 판이 덜 바뀌고, 판이 덜 바뀌어야 업데이트로 스트림이 끊기는 일이 드물다.

| 크레이트(가칭) | 층 | 담는 것 | 옮겨 오는 곳 |
|---|---|---|---|
| `kasa-cast` | 전송(3), 창·GPU 금지 | 선 위 계약(`ctl`·`Frame`·`Config`·`serve`·`Rate`·소리 `Hub`·`Jitter`), 조작 낱말(`Input`·`Denied`), 앱↔서비스 제어 규약 | `kasa-remote` `screen_stream.rs`·`screen_stream/audio.rs`, `screen_mirror.rs` 의 조작 낱말 |
| `kasa-cast-host` | 4, GPU 허용 | 보내는 쪽 OS 부분 — SCK·VT 압축, DXGI·MF, WASAPI·SCK 소리, 스피커 끄기, CGEvent·SendInput, 화면 깨우기 | 앱 `screen_stream/{host,capture,codec(압축 쪽),annexb,audio_mac,audio_win,source_mute,scroll_probe,win,win_agent}.rs`, `screen_input.rs`, `screen_capture.rs` 의 화면 목록·깨우기 |
| `kasaterm-screen`(앱 층) | 12 | 실행 파일. 서비스·도우미 모드, 카사넷 입구, 제어 길 | 새로 |

- `kasa-remote` 는 `kasa-cast` 를 다시 내보내 지금 부르는 자리(`watch`·`serve_ws`)를 그대로 둔다. 앱은 보는 쪽(`kasa-cast`)과, 이행
  동안만 앱 안 보내기(`kasa-cast-host`)에 기댄다. 층 표와 `scripts/check-workspace.py` 의 `LAYERS` 를 같이 고친다.
- 카사넷 노드를 띄우는 공통 부분(중계 지도·믿는 중계·허용 목록 거절 뒤 물러나기·UPnP 치우기, 지금 `kasa-device kasanet::start`)은
  `kasa-net` 에 작은 도우미로 내려 앱과 서비스가 같이 쓴다.

### Windows

| 프로세스 | 계정·세션 | 맡는 것 |
|---|---|---|
| 서비스 `kasaterm-screen.exe --screen-service`(이름 `KasatermScreen` 그대로) | LocalSystem, 세션 0 | 도우미 감독(세션 바뀜·죽음·물러서기, 잡 오브젝트 그대로), **카사넷 화면 엔드포인트**, 제어 파이프, 허용 목록·설정 저장, 새 판으로 갈아타기 |
| 도우미 `kasaterm-screen.exe --screen-agent` | SYSTEM, 콘솔 세션 | 뜨기·압축·조작(보안 데스크톱까지)·**시스템 소리·스피커 끄기** — 지금 `win_agent.rs` 에 소리를 더한다. 장·소리를 세션 파이프로 서비스에 |
| 앱 `kasaterm.exe` | 사용자 | 설정·명부를 제어 파이프로 넘김, 폰 `/screen/ws`·옛 보는 쪽 중계 |

- 엔드포인트를 세션 0 서비스에 두는 까닭: 도우미는 세션이 바뀔 때마다 옮겨 뜨므로 거기 두면 빠른 사용자 전환·원격 데스크톱 세션에서
  연결이 끊긴다. 세션 0 은 화면·입력·소리는 못 하지만 네트워크는 된다. 도우미 → 서비스 파이프 한 번을 더 건너는 값은 30–60Mbps 라
  작다고 본다 — [잴 것] 화면→풀림 p50 이 늘지 않는지(지금 6–7ms).
- **소리·입력은 사용자 세션이 필요하다.** 세션 0 은 사용자 데스크톱도 소리 장치 세션도 없다. 입력은 이미 콘솔 세션의 SYSTEM 도우미가
  넣는다(보안 데스크톱까지 가려면 SYSTEM 이어야 한다 — S3b). 소리는 지금 앱(사용자)이 뜬다 — 도우미 문서의 까닭은 「SYSTEM 도우미는
  보안 데스크톱 때문에 있는 것」이었고, SYSTEM 이 루프백을 못 뜬다고 잰 것은 아니다. [확인] P0(2026-11-21 집 PC): 콘솔 세션에
  SYSTEM 으로 띄운 시험 프로세스가 기본 출력 장치 WASAPI 루프백을 열어 3초에 꾸러미 300개, 사람 세션 쪽 `chimes.wav` 를 봉우리
  −16.4dBFS 로 떴고, `IAudioEndpointVolume::SetMute` 켜기·되돌리기도 됐다. 그래서 **소리도 SYSTEM 도우미가 뜬다** — 사용자 토큰
  소리 도우미는 두지 않는다.
- 아무도 로그인하지 않은 때(로그인 화면)도 도우미는 콘솔 세션에 SYSTEM 으로 떠 있어 **로그인 화면을 보고 누를 수 있다** — 앱이 없어
  지금은 못 하던 일이다. 소리는 없다. **기본 켬**이고 명부 기기·주인 폰만 붙는다(결정 5).
- 이름으로 앱을 세던 예외(`--screen-service`·`--screen-agent` 빼기)는 실행 파일 이름이 갈려 필요 없어진다.

### macOS

| 프로세스 | 맡는 것 |
|---|---|
| 도우미 `KasatermScreen.app`(번들 id `com.kasa.kasaterm.screen`, `kasaterm.app/Contents/Library/LoginItems/` 안) — launchd 가 띄우는 사용자 LaunchAgent | 뜨기·압축·조작·소리·스피커 끄기·화면 깨우기·**카사넷 화면 엔드포인트**·제어 소켓 |
| 앱 `kasaterm.app` | 설정·명부를 제어 소켓으로, 폰 `/screen/ws`·옛 보는 쪽 중계, 도우미 등록·권한 안내 |

- 맥은 한 프로세스면 된다 — 사용자 세션의 LaunchAgent 는 화면·입력·소리를 다 할 수 있고 보안 데스크톱 같은 갈래가 없다.
- 도우미는 **launchd 가 띄워야 한다.** 앱이 자식으로 띄우면 책임 프로세스가 앱이 되어 TCC 가 앱 권한으로 판정한다(칸 셸과 같은 까닭) —
  권한을 따로 쥐려는 목적이 사라진다.

## 2. 카사넷

### 키 — 기기 키를 나눠 쓰지 않고 서비스가 자기 키를 갖는다

| | 기기 키 공유 | 서비스 자기 키(권장) |
|---|---|---|
| 한 EndpointId 에 엔드포인트 둘 | 두 프로세스가 같은 id 로 홈 중계에 붙고, 보는 쪽이 그 id 의 주소로 걸면 어느 쪽에 닿을지 모른다(ALPN 이 없는 쪽이면 실패) | 없음 |
| `learn` | 같은 id 를 「이 기기」로 보고 버린다([확인] `kasanet.rs learn`) | 영향 없음 |
| Windows 권한 | SYSTEM 서비스가 사용자 프로필의 `kasanet.key` 를 읽어야 한다 | 서비스 키는 `%ProgramData%\kasaterm\screen\` 에 SYSTEM·Administrators 만 읽게 — 사용자 권한 코드는 못 읽는다 |
| 국내 중계 허용 목록 | 그대로 | 서비스 id 를 한 줄 더 넣어야 한다(`relay.sh allow <id> <기기>-screen`) |

- [추측] 같은 키로 홈 중계에 둘이 붙으면 중계가 앞 연결을 밀어내 둘이 번갈아 끊길 것이다. 재지 않았다 — 자기 키로 가면 잴 필요가 없다.
- 키 자리: Windows `%ProgramData%\kasaterm\screen\kasanet-screen.key`(ACL SYSTEM·Administrators), 맥은 기기 키 옆
  `~/.config/kasaterm/kasanet-screen.key`(0600, 도우미는 사용자로 돈다). 격리 인스턴스 규칙(`KASATERM_KASANET_KEY` 가 없고 격리 env 면
  이번 실행 전용 키)은 서비스에도 그대로 — 환경 변수 이름 `KASATERM_SCREEN_KEY`.
- 그 뒤 「기기 키가 서비스 id 에 서명」(위임)을 붙이면 루프백 밖 길(계정 채널)로도 서비스 id 를 믿을 수 있다. 지금은 아래처럼 루프백
  `/version` 으로만 배우므로 필요 없다.

### 포트

- 서비스 엔드포인트는 앱과 따로 UDP 포트를 묶는다(앱처럼 고정하지 않는다, `0.0.0.0:0`). 받는 TCP 포트는 없다 — 화면은 ALPN
  `kasa/screen/1` 만 받고 `fwd` 는 걸지 않는다.
- UPnP 매핑이 기기에 둘이 된다. [확인] `kasa_net::portmap` 은 다른 프로세스가 쥔 포트의 매핑은 남긴다(묶어 보고 판정). [잴 것] Windows
  사용자 앱이 SYSTEM 서비스가 쥔 UDP 포트를 「쥐어 있음」으로 보는가(서로 지우지 않는가).
- Windows 방화벽: 서비스는 세션 0 이라 묻기 창이 안 뜨고 들어오는 UDP 가 조용히 막힐 수 있다. MSI 가 `kasaterm-screen.exe` 에 들어오는
  UDP 예외를 건다. [잴 것] 예외 없이도 구멍 뚫기(먼저 나가는 꾸러미)로 직통이 서는지, 같은 망 직통이 서는지.

### 알리기 — `/version`

```json
"kasanet": {
  "id": "<앱 id>", "relay": "...", "addrs": [...], "port": 8765,
  "screen": true,
  "screen_node": { "id": "<서비스 id>", "relay": "...", "addrs": [...], "proto": 1, "version": "0.3.0" }
}
```

- `screen` 은 지금 뜻(이 기기 **앱 엔드포인트**가 `kasa/screen/1` 을 받는다) 그대로 둔다 — 옛 보는 쪽이 이것만 본다. 앱이 서비스를
  대신 중계하는 동안 참이다(아래 「옛 판 호환」).
- `screen_node` 는 서비스가 살아 있고 제어 길로 「준비됨」을 알렸을 때만 싣는다. 주소는 서비스가 제어 길로 알린 엔드포인트 주소다.
- 앱이 없으면 `/version` 도 없다. 그때는 보는 쪽이 지난번에 배운 `screen_node` 를 쓴다(아래 「앱이 없을 때」).

### 보는 쪽이 붙는 곳

- `learn` 이 `screen_node` 를 보면 그 id 를 따로 적고 **서비스용 `Link` 를 하나 더** 세운다(앱 링크와 길 판정이 다르다 — UDP 포트·NAT
  매핑이 다르다). `can_view` 와 `connect` 는 `screen_node` 가 있으면 그 링크의 길(직통·믿는 중계)로, 없으면 지금처럼 앱 링크로 간다.
- 나가는 연결은 허용 목록 훅이 막지 않으므로([확인] `AllowHook` 은 들어오는 쪽만) 보는 쪽은 서비스 id 를 허용 목록에 넣을 필요가 없다.
- 믿는 근거는 지금과 같다 — 루프백 base(ssh 길)로 물은 `/version` 이 알려 준 id 만 쓴다.
- 서비스 id 가 국내 중계 허용 목록에 없으면 서비스는 n0 를 홈으로 쓰고([확인] `fall_back_when_denied`), 데이터는 직통으로만 간다.
  `relay.sh ids` 가 명부 기기의 `screen_node.id` 도 보이게 고친다.

### 누가 붙나 — 허용 목록

- 서비스는 명부를 모른다. 앱이 제어 길로 **통째 목록**을 넘긴다 — 명부 기기 id(`links`)와 등록된 주인 폰 id·만료(`phones`). 바뀔 때마다
  보내고, 서비스는 그것을 디스크에 둔다(Windows `%ProgramData%\kasaterm\screen\allow.json`, 맥 `~/Library/Application Support/kasaterm/screen/`).
  앱이 재시작하는 동안·없는 동안도 마지막 목록으로 받는다.
- 판정은 지금 `ScreenAccept` 그대로 한 단계다: 허용 목록 훅(핸드셰이크 직후) = 그 목록. 폰은 만료가 지나면 빠진다 — 앱이 없으면 폰이 다시
  등록할 곳이 없으니 15분 뒤 폰은 못 붙는다.
- 목록이 빈 서비스(앱이 한 번도 안 넘긴)는 아무도 받지 않는다.

### 옛 판 호환

| 보는 쪽 \ 보내는 쪽 | 옛 판(앱이 보냄) | 새 판(서비스) |
|---|---|---|
| 옛 판 | 지금 그대로 | 앱 엔드포인트로 붙는다 → **앱이 서비스에서 장을 받아 중계**(`screen: true` 유지). 앱이 재시작하면 끊기는 것은 지금과 같다 |
| 새 판 | `screen_node` 가 없으니 앱 엔드포인트로 | 서비스로 직접. 앱이 재시작해도 이어진다 |

- 중계는 새 코드가 거의 없다: 앱이 꽂는 `Capture` 를 「서비스에 붙는 클라이언트」로 바꾸면 된다 — 지금 Windows 앱이 도우미 파이프로 장을 받는
  `AgentSession` 과 같은 꼴이다. 그러면 옛 보는 쪽(QUIC)과 폰 `/screen/ws` 영상 방식이 지금 코드(`serve_conn`·`serve_ws_with`) 그대로 돈다.
- 중계로 보는 옛 보는 쪽은 소리를 못 받게 둘 수 있다(앱 소리 `Hub` 를 서비스 덩이로 채우는 일은 덤). preview 기기는 한 시간 안에 새 판이
  되므로 중계는 한두 판만 둔다(결정 4).

### 앱이 없을 때 — 찾기

- 보는 쪽은 배운 `screen_node`(id·중계·주소)를 기기 상태 캐시에 남긴다. 앱이 없는 기계는 `/version` 이 안 오니 그 캐시로 건다 — 주소가
  바뀌었어도 같은 중계를 홈으로 쓰면 중계 신호로 구멍을 뚫는다. [추측] 홈 중계가 바뀌면(국내 중계 허용 목록 밖 등) 못 찾는다.
- 앱을 한 번도 안 띄운 기계는 허용 목록도, 알린 적도 없다. 첫 짝짓기는 앱을 한 번 띄우는 것으로 한다(맥미니라면 처음 한 번). 앱 없이
  짝짓는 CLI 는 두지 않는다(결정 7).

## 3. 앱 ↔ 서비스 제어 길

### 길과 인증

| | macOS | Windows |
|---|---|---|
| 길 | 유닉스 소켓 `~/Library/Application Support/kasaterm/screen/ctl.sock`(폴더 0700) | 이름 파이프 `\\.\pipe\kasaterm-screen-ctl`(서비스가 만든다, `PIPE_REJECT_REMOTE_CLIENTS`) |
| 받는 쪽 확인 | `getpeereid` 로 같은 uid, 감사 토큰으로 상대 서명을 `anchor apple generic and certificate leaf[subject.OU] = "L366799VND"` 에 맞춘다 | DACL SYSTEM + 대화형 사용자, 붙은 프로세스의 실행 파일이 설치 폴더(관리자만 쓰는 Program Files)의 `kasaterm.exe` 이고 세션이 지금 콘솔 세션 |
| 거는 쪽 확인 | 소켓 주인 uid 가 나와 같고, 상대 서명이 같은 팀·`com.kasa.kasaterm.screen` | 파이프 주인이 SYSTEM(`owned_by_system` 그대로) |

- 틀은 지금 도우미 파이프와 같은 `[종류 u8][길이 u32 LE][본문]` 이다. 한 연결은 한 방향으로만 흐른다(동기 파이프의 막힘, S3b) — 제어
  연결(앱 → 서비스 요청, 서비스 → 앱 답)과 장 연결(서비스 → 앱 장·숨)을 가른다.
- 개발 빌드(`kasaterm-dev`·애드혹 서명)는 서명 확인을 못 지나므로 검증 실행(`verification_run`)에서만 경로 확인으로 물러난다.
- 남는 위험은 지금과 같다 — 사용자 권한 악성 코드가 서명된 앱 프로세스를 조종하면 허용 목록을 바꿀 수 있다.

### 메시지

| 종류 | 방향 | 내용 |
|---|---|---|
| `hello` | 앱 → | `{proto, app_version}` → 답 `{proto, version, node: {id, relay, addrs}, ready, capture, input, audio}` — `capture`·`input` 은 `ok`·`permission`·`unavailable` |
| `allow` | 앱 → | `{machines: [{id, label}], phones: [{id, expires_unix}]}` 통째. 서비스는 디스크에 쓰고 허용 목록을 바꾼다 |
| `settings` | 앱 → | 아래 표의 값 통째 |
| `status` | 앱 → | 구독. 바뀔 때마다 `{viewers: [{id, label, since, audio, input}], node, capture, input}` — 앱이 「보는 중」 표시를 세운다 |
| `ask_permission` | 앱 → | `screen`·`accessibility` — 맥 권한 창을 **서비스 프로세스가** 띄운다(TCC 는 띄운 프로세스에 붙는다) |
| `kick` | 앱 → | 그 보는 쪽 연결을 닫는다 |
| `restart_when_idle` | 앱 → | (macOS, P4) 보는 사람이 없을 때 스스로 끝낸다 — launchd 가 새 판으로 다시 띄운다. Windows 는 서비스가 등록 경로를 보고 스스로 한다 |
| `watch` | 앱 → (장 연결) | `Config` → 지금 Windows 장 연결의 `S`/`N`·`D`/`F`·`H` 그대로 — 앱의 중계(옛 보는 쪽·폰)용 |
| `input`·`keyframe` | 앱 → | 지금 조작 연결의 `C`/`I`·`K` 그대로 |

### 설정과 앱이 없을 때의 기본값

| 설정 | 누가 정하나 | 서비스가 쥐는 기본값(앱이 한 번도 안 넘겼을 때) |
|---|---|---|
| 누가 볼 수 있나 | 앱 `allow`(명부 기기·주인 폰) | 빈 목록 = 아무도 못 붙는다 |
| 조작 허용 | 앱 `settings.input` | 켬(지금처럼 주인 기기만 붙으니 그 기기 앞의 손쉬운 사용 권한이 동의) |
| 소리 보내기 | 보는 쪽 `Config.audio`·`A`, 앱 `settings.audio` 가 끄면 늘 거절 | 켬 |
| 원본 스피커 끄기 | 앱 `settings.source_mute` | 켬(지금 동작). 검증 리그는 지금처럼 `KASATERM_TEST_SCREEN_SOURCE_MUTE=1` 일 때만 |
| 화면 고르기 | 보는 쪽 `Config.display`(연결마다) | 주 화면 |
| 로그인 전 화면(Windows) | 앱 `settings.logon_screen` | 켬(결정 5) — 허용 목록은 그대로 명부 기기·주인 폰 |
| 서비스 켜고 끄기 | 앱 설정 → 맥 `SMAppService.unregister`, Windows 서비스 시작 유형 | 설치하면 켬 |

- 앱은 설정 파일(`settings.json`)의 `screen_service` 칸(`{input, audio, source_mute, logon_screen}`)을 그대로 넘긴다 — 아직 설정 화면은 없다.
- 서비스는 마지막으로 받은 설정을 디스크에 둔다. 앱이 없을 때도 그대로 쓴다. 스피커 표(`stream-muted`)는 서비스 상태 폴더로 옮기고, 서비스가
  뜰 때 남아 있으면 되켠다. 앱의 `exiting` 은 서비스가 보낼 때는 스피커를 건드리지 않는다.
- 비트레이트·fps·코덱은 지금처럼 보는 쪽 `Config` 가 정하고 서비스가 범위로 깎는다(`Config::from_json`). 설정 칸에 따로 두지 않는다.

### 폰

- 폰은 지금처럼 앱 HTTP `/screen/ws` 로 온다. 영상 방식은 앱이 서비스 `watch` 로 받은 장을 그대로 넘긴다 — 앱이 재시작하면 폰 화면은 끊기고
  폰이 다시 붙는다(지금과 같다).
- JPEG 방식(안드로이드·관문 길)은 앱의 `screen_capture.rs`(`CGDisplayCreateImage`·`StretchBlt`)가 계속 뜬다 — 앱 화면 기록 권한에 기댄다.
  P5 에서 서비스로 옮긴다(결정 8).
- 폰이 서비스에 카사넷으로 직접 붙는 길(FFI 가 `kasa/screen/1` 을 받기)은 범위 밖이다.

## 4. 업데이트

### 판 둘

- **서비스 판**(`kasaterm-screen` 의 판): 서비스 코드가 바뀔 때만 오른다. 앱 판과 따로 센다.
- **제어 규약 판**(`proto`, 정수): 앱 ↔ 서비스 메시지가 바뀔 때만. 서비스는 받는 범위 `[min, max]` 를 `hello` 에 싣고, 앱은 둘 다 아는 가장 큰
  값을 쓴다. 보는 쪽과의 계약(`kasa/screen/1`)은 이 판과 상관없다.

### Windows — MSI

[확인] P0(2026-11-21 집 PC, 시험 제품 `KasaP0Test`·시험 서비스 `KasatermScreenP0`, WiX 3.14)로 정한 배치다.

| 구성요소 | GUID | 담는 것 |
|---|---|---|
| 서비스 실행 파일 | **서비스 판마다** 고정(판으로 지은 값), 폴더 `bin\screen\<서비스 판>\`, **`Permanent='yes'`** | `kasaterm-screen.exe` + `ServiceInstall`(`KasatermScreen`, `--screen-service`) + `ServiceControl Start='install'` — 멈춤·지움은 걸지 않는다 |
| 서비스 정책 | 늘 같다 | `SoftwareSASGeneration`(키 경로) + 방화벽 예외(서비스 실행 파일, 들어오는 UDP) |
| 지우기 | 사용자 지정 동작 `ScreenServiceRemove`(WixQuietExec, `REMOVE="ALL" AND NOT UPGRADINGPRODUCTCODE`) | 서비스 멈추고 지우기, `bin\screen`·`%ProgramData%\kasaterm\screen` 걷기 |

- `MajorUpgrade` 예약은 **지금 그대로 `afterInstallInitialize`**(옛 판을 다 지우고 새 판을 깐다).
- 서비스는 **스스로 갈아탄다**: MSI 는 새 판을 옆 폴더에 깔고 등록 경로(`ImagePath`)만 바꾼다. 서비스가 2초마다 등록 경로와 제 실행 파일을
  견줘 다르고 보는 사람이 없으면 「실패」로 끝나고, 서비스 관리자가 복구 동작(1·5·30초 뒤 다시 시작, 「실패로 끝남」에도)으로 새 판을 띄운다.
  새 판은 지난 판 폴더를 치운다. 그래서 **결정 9(보는 사람이 떠날 때까지 미룸)를 서비스가 지킨다** — 업데이트 도우미는 할 일이 없다.
- 잰 것(서비스 PID·`ImagePath`·파일, 시험 MSI):

| 업데이트 | 결과 |
|---|---|
| 지금 설치판 꼴(`kasaterm.exe` 구성요소에 서비스, `Stop='both'`)에서 넘어감 | 옛 제품이 서비스를 멈추고 지운 뒤 새 판이 깔고 띄운다 — **한 번 다시 켜짐**(넘어갈 때 한 번) |
| 앱만 바뀜(서비스 판 같음) | **서비스 PID 그대로** |
| 서비스 판이 바뀜 | **서비스 PID 그대로**(옛 판이 계속 돈다), `ImagePath` 는 새 폴더, 두 판 폴더가 다 있다 → 다시 켜면 새 판 |
| 지우기 | 서비스·`bin\screen` 사라짐, 재부팅 대기 없음 |

- 처음 세운 꼴(`afterInstallExecute` + 판 붙은 구성요소)은 쓰지 않는다 — 잰 것과 까닭:
  - `ServiceControl Stop·Remove` 까지 걸면 서비스 판이 바뀔 때 옛 제품 제거가 **같은 이름의 서비스를 지워 버렸다**.
  - `Stop='install'` 을 걸면 서비스 판이 같아도 업데이트마다 다시 켜졌다.
  - 멈춤을 빼면 서비스 판이 바뀔 때 재시작 관리자가 옛 파일을 쥔 서비스를 다시 켰다(`MSIRESTARTMANAGERCONTROL=Disable` 이어도 다시 켜지고
    `Config.Msi\*.rbf` 재부팅 대기가 남았다).
  - 무엇보다 **굽기 스크립트의 `heat -gg` 가 웹 UI·협업 훅 파일의 구성요소 GUID 를 빌드마다 새로 짓는다.** `afterInstallExecute` 면 새 판을 깐
    뒤 옛 제품 제거가 같은 경로의 새 파일을 지운다(구성요소 규칙 위반). 예약을 그대로 두면 이 위험이 없다.
- 서비스 전용 MSI(결정 2 의 물러설 길)는 쓰지 않는다 — 한 MSI 로 위를 다 맞췄다.
- 앱 [다시 켜기] 흐름(`swap_powershell` → 앱 종료 기다림 → `msiexec /passive` → 앱 다시 띄움)은 그대로다. 앱이 꺼져 있는 동안 보는 쪽은
  서비스에 붙어 있다.
- 서비스 판은 `app/kasaterm-screen/Cargo.toml` 의 판이고 실행 파일 판(FileVersion)으로도 박힌다. 서비스 코드(그 크레이트·`kasa-cast`·
  `kasa-cast-host`·`kasa-net`)를 바꾸면 판을 올리고 `source-hash.txt` 를 고친다 — 시험 `service_version_follows_its_source` 가 지킨다. 판이 같으면
  설치된 서비스는 그대로라 새 코드가 안 간다.
- 집 PC 관리자 ssh 에서 빌드할 때: `%USERPROFILE%\.cargo\bin` 의 rustup 대리 실행 파일은 심볼릭 링크라 관리자(높은 무결성) 프로세스가 못 따라간다
  (`os error 448`, 신뢰하지 않는 탑재 지점). 툴체인의 `cargo.exe` 를 바로 부르고 `RUSTC`·`RUSTDOC` 을 같은 폴더의 것으로 준다.

### macOS — Sparkle

- 도우미는 `kasaterm.app` 안에 들어 있어 Sparkle 이 번들째 바꾼다. 갈아 끼우기는 이름 바꾸기라 [추측] 돌던 도우미는 옛 파일(지워진 아이노드)로
  계속 돈다 — 스트림은 이어진다. 다음에 launchd 가 띄울 때 새 판이 뜬다.
- 도우미가 2초마다 제 번들의 `Info.plist` 판(`CFBundleShortVersionString` = 서비스 판)을 읽어 제 판과 다르고 보는 사람이 없으면 끝 코드 75 로 끝나고,
  launchd(`KeepAlive`)가 새 판을 띄운다. 보는 중에는 갈아 끼우지 않는다(결정 9). 서비스 판이 같으면 번들이 바뀌어도 돌던 도우미를 그대로 둔다.
  앱 쪽 `restart_when_idle` 은 두지 않았다 — Windows 와 같이 서비스가 스스로 본다.
- [잴 것] (1) Sparkle 갈아 끼우기 동안·뒤에 도우미 프로세스가 살아 있는가(백그라운드 작업 관리 BTM 이 등록을 지우거나 죽이지 않는가), (2) 새 판
  도우미가 같은 TCC 허락을 쓰는가(지정 요구조건이 번들 id·팀이라 같아야 한다), (3) Sparkle 설치기가 도우미를 끄지 않는가. 서명된 preview 판 두 개로
  잰다 — 사람의 로그인 항목을 건드리므로 맥미니 같은 별도 기기·별도 사용자에서, 승인 뒤에.

### macOS 구현 (P4)

- 번들: `kasaterm.app/Contents/Library/LoginItems/KasatermScreen.app`(번들 id `com.kasa.kasaterm.screen`, `LSUIElement`, 최소 13.0, 판 = 서비스 판)과
  `Contents/Library/LaunchAgents/com.kasa.kasaterm.screen.plist`(`BundleProgram`, `--launch-agent`, `RunAtLoad`·`KeepAlive`, `Aqua`,
  `AssociatedBundleIdentifiers = com.kasa.kasaterm`). `scripts/build-app.sh` 가 넣고 안쪽부터 서명한다.
- 앱이 부팅 때 `SMAppService.agentServiceWithPlistName` 으로 등록한다(이미 켜졌으면 그대로, 「허용 필요」면 기록만). 검증 실행·번들 밖·13 이전은 등록하지
  않는다. 설정 `screen_service.enabled: false` 면 등록을 푼다.
- 제어 길은 상태 폴더(`~/Library/Application Support/kasaterm/screen`, 0700)의 `ctl.sock`(0600). 도우미는 붙은 상대가 같은 uid(`getpeereid`)이고 실행 파일이
  같은 번들의 `Contents/MacOS/kasaterm`(`LOCAL_PEERPID` → `proc_pidpath`)인지 본다. 검증 실행은 uid 만 본다.
- 도우미 안에서 뜨기·압축(SCK·VT)·소리(SCK 소리·스피커 끄기)·조작(CGEvent)·카사넷 노드가 한 프로세스다.
- 번들에 Mach-O 를 더하면 릴리스 도구의 공증 사전 검사 목록(`tools/release/macsign.py` `HARDENED_SIGNED`)에도 넣는다 — 빠지면 preview 굽기가
  「hardened 서명 목록 밖」으로 막힌다(2026-11-21 P2–P5 판이 그렇게 한 번 막혔다).

### 판이 어긋날 때

| 경우 | 동작 |
|---|---|
| 앱 새 판, 서비스 옛 판(같은 `proto` 범위) | 그대로 쓴다. 서비스 판이 다르면 보는 사람이 없을 때 갈아 끼운다 |
| 앱이 아는 `proto` 와 서비스 범위가 안 겹침 | 앱은 서비스를 안 쓰고 앱 안 보내기로 물러난다(이행 동안), `screen_node` 를 안 싣고, 설정 칸에 「화면 서비스 판이 맞지 않아요」 |
| 앱을 옛 판으로 되돌림 | 서비스는 옛 `proto` 를 받는 범위로 계속 받는다. 아주 옛 판(서비스를 모름)은 앱 안에서 보내고, 서비스도 따로 보낸다 — 아래 「둘이 같이 보낼 때」 |
| 서비스만 깨진 판 | 서비스가 죽으면 SCM 복구·launchd 가 다시 띄우고, 거듭 죽으면 앱이 「화면 서비스가 멈췄어요」와 앱 안 보내기로 물러난다(이행 동안) |

## 5. 권한

### macOS TCC

- 화면 기록·손쉬운 사용은 **도우미 번들**(`com.kasa.kasaterm.screen`)에 붙인다. 지정 요구조건은
  `identifier "com.kasa.kasaterm.screen" and anchor apple generic and certificate leaf[subject.OU] = "L366799VND"` 꼴(Developer ID) —
  번들 id·팀이 같으면 판이 바뀌어도 허락이 이어진다([확인] 앱 번들도 같은 규칙으로 재빌드 뒤 유지됐다).
- 권한 창은 도우미가 띄운다(`CGRequestScreenCaptureAccess`·`AXIsProcessTrustedWithOptions`). 설정에 「KasatermScreen」 이 따로 선다. 켠 뒤
  다시 켜야 할 것은 도우미뿐이다 — 도우미가 스스로 끝나면 launchd 가 바로 다시 띄운다. 칸·학생은 안 끊긴다.
- 서명: `build-app.sh` 가 안쪽부터(도우미 → 앱) Developer ID·hardened runtime 으로 서명하고 공증에 함께 싣는다. 도우미 entitlements 는 따로 둔다.
  애드혹·`kasaterm-dev` 서명 판은 지정 요구조건이 달라 허락이 따로 쌓인다 — 검증은 아래 리그처럼 칸 안에서 띄워 앱 권한을 빌린다.
- 맥미니가 권한 거부로 안 보이던 까닭은 [추측]이다. 후보: 서명 신원이 바뀐 뒤의 옛 csreq, macOS 의 주기적 화면 기록 재확인(지켜보는 사람이
  없는 기계라 창을 아무도 안 누른다). [잴 것] 맥미니에서 TCC.db 의 `kTCCServiceScreenCapture` csreq 를 지금 번들에 `codesign -v -R` 로 맞춰 보고,
  `log show --predicate 'subsystem == "com.apple.TCC"'` 로 거부 까닭을 본다. 주기적 재확인이 까닭이면 **분리로는 안 풀린다** — 도우미도 같은
  재확인을 받는다. 그때 분리가 주는 것은 「다시 켜기가 싸다」와 「앱 서명·판 바뀜에 안 휘둘린다」뿐이다.

### 등록 방법

| | `SMAppService.agent`(권장) | `~/Library/LaunchAgents` plist 직접 | LaunchDaemon(root) |
|---|---|---|---|
| macOS | 13+ | 모두 | 모두 |
| 화면 뜨기 | 로그인한 사용자 세션에서 됨 | 됨 | 안 됨 — 윈도 서버 세션이 없다 |
| 실행 파일 | 번들 안 상대 경로(`BundleProgram`) — 번들과 같이 업데이트 | plist 가 가리키는 경로 — 번들을 옮기면 깨진다 | — |
| 사람 승인 | 「로그인 항목」 백그라운드 허용 알림 | 같은 알림(13+) | 관리자 |
| 지우기 | `unregister`, 앱을 지우면 항목이 꺼진다 | 파일을 지워야 한다 | — |

- 13 이전(앱 최소 11)은 서비스 없이 지금처럼 앱 안에서 보낸다. SCK 소리도 13 부터라 잃는 것이 적다.
- **로그인 전 화면의 한계**: 사용자 LaunchAgent 는 로그인한 뒤에야 뜨고 로그아웃하면 죽는다. 로그인 창은 `LimitLoadToSessionType = LoginWindow`
  에이전트(`/Library/LaunchAgents`, 관리자 설치)가 필요하고 그 자리의 TCC 는 [추측] 막혀 있을 것이다 — 범위 밖이다. FileVault 부팅 전 화면은 불가.
  자동 로그인이 꺼진 맥미니는 재부팅 뒤 로그인 전까지 서비스도 없다.

### Windows

- 서비스 `KasatermScreen` 은 LocalSystem·세션 0, 도우미는 SYSTEM·콘솔 세션 — 지금 S3b 그대로다. 바뀌는 것은 실행 파일(`kasaterm-screen.exe`)과
  도우미의 장을 받는 쪽(앱 → 서비스, 이행 동안은 둘 다)이다. 도우미 파이프의 `same_exe` 는 「같은 설치 폴더의 `kasaterm-screen.exe`·`kasaterm.exe`」로
  넓힌다(서비스와 앱 둘이 붙는다).
- 서비스 키·허용 목록·설정은 `%ProgramData%\kasaterm\screen\` 에 SYSTEM·Administrators 만 쓰게 ACL 을 건다(`%ProgramData%` 하위 기본 ACL 은 사용자가
  만들 수 있어 그대로 두면 안 된다).
- 서비스 복구 동작(`sc failure`: 1·5·30초 뒤 다시 시작)을 MSI 가 건다(`util:ServiceConfig`).

## 6. 실패·되돌리기

### 실패

| 실패 | 보는 쪽 | 앱·서비스 |
|---|---|---|
| 서비스 없음(옛 설치·맥 13 이전) | `screen_node` 가 없어 앱 엔드포인트로 — 지금과 같다 | 앱이 앱 안에서 보낸다 |
| 서비스 권한 없음(맥) | `X permission` — 칸에 「그 기기 화면 서비스에 화면 기록 권한이 없어요」 | 앱 설정·상태에 「권한 열기」, `ask_permission` |
| 서비스 죽음 | 연결이 닫히고 칸이 3초마다 다시 붙는다(`pane.rs RETRY_AFTER`) | SCM 복구·launchd `KeepAlive` |
| 서비스 직통 안 섬(방화벽·중계 허용 목록 밖) | 서비스 링크가 직통·믿는 중계가 아니면 앱 엔드포인트(중계)로 물러난다 | 로그 `[screen-svc] 직통 아님 — 앱 중계로` |
| 둘이 같이 보낼 때(앱 안 보내기 + 서비스) | — | 서비스가 살아 있으면 앱은 앱 안 보내기를 꽂지 않고 중계를 꽂는다. Windows 복제는 프로세스마다 따로라 [잴 것] 두 프로세스가 같은 출력을 복제할 수 있는가 |
| 스피커 끈 채 죽음 | — | 서비스가 다시 뜰 때 표를 보고 되켠다 |

### 되돌리기

- 보는 쪽과의 계약이 그대로라 **보내는 쪽 판을 되돌리면 `screen_node` 만 사라지고 새 보는 쪽은 앱 엔드포인트로 물러난다.** 그래서 보는 쪽의 「앱
  엔드포인트로 물러나기」는 이행이 끝나도 지우지 않는다(작다).
- 끄는 스위치: 앱 `KASATERM_SCREEN_SERVICE=off`(설정 값도 같은 이름) — 앱이 서비스를 안 쓰고 앱 안에서 보낸다. 앱 안 보내기를 지우는 단계(P3·P5)
  전까지 산다.
- Windows 는 MSI 실패 때 Windows Installer 가 옛 판을 그대로 두고(구성요소째), 서비스 판 폴더가 갈려 있어 옛 서비스가 남는다.

## W1 실측 (2026-11-21 집 PC, 시험 서비스)

- 시험 서비스 `KasatermScreenTest`(세션 0) → 시험 도우미(SYSTEM, 세션 1, 파이프 `kasaterm-screen-test-1`) → 탐침(`--probe-watch`, `127.0.0.2`
  루프백). **앱 없이** 15초에 장 860장(57fps, 3840×1600 HEVC, `agent:true`), 소리 덩이 2949개(5ms 덩이, 초당 약 197), 뜸→받음 p50 6.5ms·p90 8.5ms,
  압축 끝→받음 p50 0.28ms. 사람 서비스(`KasatermScreen`)·도우미 파이프(`kasaterm-screen-1`)는 그대로 돌았다.
- 서비스 키는 국내 중계 허용 목록에 없어 `not authorized` 로 거절되고 n0 를 홈으로 썼다 — 실기에서는 `relay.sh allow <서비스 id> <기기>-screen` 이 있어야
  믿는 중계로 데이터를 싣는다(직통은 상관없다).
- 처음엔 탐침이 장을 하나도 못 받았다: 탐침이 연결 퓨처 안으로 옮긴 엔드포인트가 연결 직후 버려져 소켓이 닫혔다(서비스는 핑을 기다림). 엔드포인트는
  끝까지 쥔다.

## P5 구현·실측 (2026-11-21)

- 중계 길(`kasa_cast_host::local`, 맥 도우미 `local.sock`): 장 연결(`S` → `N`·`D` → `F`·`H`)과 요청 연결(`C` 뒤 `K`·`R`·`M`·`I`·`J`·`L`·`P`·`Q`·`W`,
  요청 하나에 답 하나). 앱은 `Hybrid` 로 꽂는다 — 도우미가 있으면 도우미에서, 없으면(13 이전·개발 실행) 앱 안에서. 도우미가 `permission` 으로
  거절하면 도우미 권한 창을 한 번 띄우고 그동안 앱 안에서 뜬다(앱이 권한을 쥔 기기가 넘어가는 동안 끊기지 않게). 앱의 소리 허브는 도우미가
  있으면 뜨지 않는다 — 서비스로 붙는 보는 쪽은 도우미가 소리를 보내고, 앱으로 중계되는 옛 보는 쪽은 소리 없이 본다.
- 실측(같은 맥, 디버그 판, 도우미 직접 실행 + 격리 앱): 앱이 도우미 제어 소켓에 붙고, 폰 영상 소켓(`/screen/ws?codec=hevc`)이 도우미 화면
  3024×1964 HEVC 를 4초에 106장(청한 30fps), JPEG 소켓이 3초에 22장(8fps, ack)을 받았다. 앱 프로세스는 `local.sock` 연결 둘을 쥐었다.
- 넘어가기 실측(집 PC, 실제 실행 파일, 시험 제품 `kasaterm-wtest`·서비스 `KasatermScreenT`): 옛 꼴(설치판 `kasaterm.exe --screen-service`)에서
  P2·P3 꼴 MSI 로 올리자 옛 서비스가 걷히고 `kasaterm-screen` 서비스(세션 0)·도우미(세션 1)가 섰다. 탐침 16초 900장·소리 3139덩이·지연 p50 6.5ms,
  조작(Esc) 접수. 사람 세션의 일반 권한 작업으로 띄운 UAC 창(consent.exe)에 장이 그 초 59→44 로 줄었다 이어졌다(보안 데스크톱을 따라감).
  창이 2초 안에 닫혔고 그 PC 를 사람이 쓰는 중(콘솔 유휴 없음)이라 원격 Esc 로 닫기는 다시 재지 않았다 — S3b 실측이 그 길이다.
- UAC 원격 닫기(2026-11-22, 집 PC, 콘솔 입력 없음 2시간, 시험 서비스 1.3.0): 일반 권한 UAC 창이 원격 Esc 로 약 5초 만에 닫혔고, 뜨기는
  Winlogon 데스크톱으로 옮겼다 돌아왔다(창이 떠 있는 동안 새 장 0 — 정지 화면이라 복제가 새 장을 안 낸 것으로 추정). 끝난 뒤 창·서비스·프로세스 없음.
- 끝까지 실측(2026-11-22, 집 PC, 1.3.1, 시험 서비스 + 새 키 원본 리그 + 새 키 탐침 — 「S3 리그」의 경고대로 리그 앱은 하나): 리그 앱이
  고친 제어 파이프로 붙어(「화면 서비스 1.3.1 붙음」) 탐침 id 를 허락 목록으로 넘겼고, 탐침이 서비스에 바로 붙어(서비스 로그 「보기 시작」)
  20초 1140장(57fps)·소리 3929덩이·뜨기→받기 p50 6.5ms·p90 11.5ms. 10초에 리그 앱을 끝냈는데 장이 초당 58장으로 끊김 없이 이어졌다.
  조작은 맨 앞 창이 사람 앱이라 Esc 를 보내지 않았다(사람 칸의 학생이 받는다) — 같은 입력 길은 위 UAC 실측이다.

## 구현 메모 (P2, Windows)

- 제어 규약은 `kasa_cast::service`(`PROTO`·틀·`ask`), 서비스 쪽 처리는 `app/kasaterm-screen/src/ctl.rs`, 파이프는 `win.rs`, 앱 쪽은
  `app/kasaterm/src/screen_service.rs`(2초마다 `allow`(바뀔 때만)·`settings`(바뀔 때만)·`status` → `kasanet::set_screen_node`).
- 함정(1.3.0 → 1.3.1): 앱은 붙은 파이프의 소유자가 SYSTEM 인지 본다. LocalSystem 이 만든 개체의 기본 소유자는 Administrators 라
  보안 설명자에 `O:SY` 를 안 적으면 앱이 제어 파이프를 거절하고 「화면 서비스가 없다」로 남는다(허락 목록을 못 넘겨 서비스의 보는 쪽 0).
  도우미 파이프는 처음부터 `O:SY` 였다. 시험 서비스·탐침은 카사넷으로만 붙어 이 길을 안 지났다 — 설치판에서 처음 드러났다.
- 함정(앱이 켜진 채 MSI): 서비스의 상대 확인(`trusted_app`)은 `QueryFullProcessImageNameW` 로 상대 실행 파일을 본다. 이 값은 이름 바꾸기를
  따라가서, 쓰는 중인 `bin\kasaterm.exe` 를 MSI 가 `C:\Windows\Installer\Config.Msi\*.rbf` 로 옮기면 도는 옛 앱이 그 경로로 보인다 — 서비스가
  파이프를 끊고 앱은 5초마다 「hello: failed to fill whole buffer」, `screen_node` 없이 앱 길로 보낸다. 앱을 다시 띄우면 붙는다. 앱 안 업데이트는
  앱이 꺼진 뒤 설치해(`win_update.rs`) 이 꼴이 안 나고, 켜 둔 채 `msiexec /qn` 으로 손수 깔 때만 난다.
- 설치판(2026-11-22, 집 PC, v0.2.265 — 위 까닭으로 앱을 다시 띄운 뒤): 「화면 서비스 1.3.1 붙음」, `/version` 에 `screen_node`(국내 중계),
  서비스 허락 목록에 명부 기기 3(폰 0), 국내 중계가 서비스 노드를 허용. 사람이 여는 화면 칸 연결은 이 기록에 없다.
- 보는 쪽 길: `kasanet::learn` 이 `/version` 의 `screen_node` 를 보면 그 노드와 길을 따로 쥔다(`Link::start_alpn` + `kasa/screen-link/1` —
  서비스 노드는 `fwd` 를 안 받는다). `kasanet::connect_screen` 은 그 길이 직통·믿는 중계면 서비스로, 아니면 앱으로 붙는다. `can_view` 는
  `kasanet::screen_path` 를 본다.
- 소리: 도우미 파이프에 소리 연결(`A` → `N` 뒤 `a` 덩이·1초 숨 `H`)을 더했다. 세션 0 서비스의 소리 허브는 그 연결을 쓴다
  (`kasa_cast_host::service::audio`). 스피커 끄기는 도우미가 하고 서비스 설정 `source_mute` 를 따른다.
- 로그인 화면: 콘솔 세션에 사용자가 없으면 도우미 파이프를 SYSTEM 만 열게 만들고, 사용자가 생기면 도우미가 끝나 서비스가 그 사용자까지 받게
  다시 띄운다.
- 도우미 파이프의 상대 확인은 「같은 실행 파일 또는 같은 설치의 `bin\kasaterm.exe`」다 — 앱의 폰 웹소켓·옛 보는 쪽 중계가 지금처럼 도우미 파이프로
  장을 받는다(결정 4 의 「앱이 중계」).
- 리그용 이름 가르기: `KASATERM_SCREEN_SERVICE_NAME`(서비스 이름 — 복구 동작·등록 경로를 그 이름으로), `KASATERM_SCREEN_PIPE`(도우미 파이프),
  `KASATERM_SCREEN_CTL`(제어 파이프), `KASATERM_SCREEN_STATE`(상태 폴더, 기록은 그 옆 `screen-service.log`), `KASATERM_SCREEN_VERIFY=1`(검증 실행).

## 이행 단계

| 단계 | 내용 | 대략 크기 | 끝 조건 |
|---|---|---|---|
| P0 실측 | 아래 「재야 할 것」 1–7. 시험 MSI·시험 서비스·서명된 시험 도우미로 잰다. 제품 코드 변경 없음 | 시험 스크립트 수백 줄, 1–2일 | 이 문서에 수치·결론 |
| P1 떼어 옮기기 | `kasa-cast`·`kasa-cast-host` 로 옮긴다. 동작 그대로 | 옮김 약 4,500줄, 새 약 200줄 | 앱 시험·`kasa-remote screen_stream` 시험·`check-workspace` 통과, 리그 S1 수치 그대로 |
| P2 Windows 서비스가 보냄 | `kasaterm-screen.exe`(서비스·도우미, 소리는 도우미), 서비스 키·엔드포인트, 제어 파이프, 허용 목록 넘기기·저장, `/version screen_node`, 보는 쪽 서비스 링크·물러나기, 앱 중계(`Capture` = 서비스 클라이언트), MSI(판마다 Permanent 서비스 구성요소·정책·방화벽·지우기 동작)·서비스가 스스로 갈아타기·복구 동작 | 새 약 2,000–2,500줄, MSI 약 60줄 | 아래 리그 W1–W3 |
| P3 Windows 정리 | `kasaterm.exe --screen-service`·`--screen-agent`(앱 실행 파일의 서비스·도우미 모드)와 업데이트·재시작 도우미의 이름 세기 예외를 지웠다. 앱 안 보내기(도우미가 없을 때 앱이 직접 뜨기)는 휴대판(zip)·개발 실행이 쓰므로 둔다 | 지움 약 30줄 | 앱 시험, 집 PC 설치판 |
| P4 macOS 도우미 | `KasatermScreen.app`, `SMAppService`, 유닉스 소켓 제어·서명 확인, 권한 창·상태 표시, `build-app.sh` 안쪽 서명·공증, 보는 사람 없을 때 갈아 끼우기 | 새 약 1,500–2,000줄, 굽기 약 80줄 | 아래 리그 M1–M3 |
| P5 앱 없이 | 보는 쪽이 배운 서비스 노드를 키 옆 `kasanet-screens.json` 에 남겨 다음 실행이 그 노드로 붙는다, `relay.sh ids` 가 서비스 id 도 보인다, 맥 앱은 폰 웹소켓(영상·JPEG)·옛 보는 쪽을 도우미 화면으로 중계한다(`kasa_cast_host::local`, 도우미가 권한이 아직 없으면 도우미 권한 창 한 번 + 그동안 앱 안에서). Windows JPEG 는 권한 문제가 없어 앱이 그대로 뜬다 | 새 약 500줄 | 앱이 꺼진 기계를 칸으로 본다 |

각 단계는 앱 동작 변경이라 개발 절차(검사·main·preview 등록·게시 확인)를 따른다. P0 은 문서만 바뀐다.

## 검증 리그

[verify-app.md](verify-app.md) 격리 규칙 그대로, 직접 띄운 PID 만 정리한다. 서비스도 격리 env(키·상태 폴더·소켓 이름을 실행마다 새 하위 폴더)로 띄운다.

- **M1 맥 — 앱 재시작에도 이어짐**: S1 리그(같은 맥, 앱 A 보기·앱 B 원본) + B 쪽 `kasaterm-screen` 을 카사텀 칸에서 직접 띄운다(책임 프로세스가
  카사텀이라 화면 권한을 빌린다 — 사람 로그인 항목에 등록하지 않는다). `KASATERM_SCREEN_KEY`·`KASATERM_SCREEN_STATE=<리그 폴더>`·
  `KASATERM_KASANET_BIND=127.0.0.1:0`. A 가 보는 중에 B 앱 PID 만 끝냈다 다시 띄운다 — `[screen-view-test]` 의 fps 가 그 사이 떨어지지 않고
  끊김이 0 인지, B 가 다시 떠 `allow`·`settings` 를 다시 넘기는지.
- **M2 맥 — 옛 보는 쪽**: A 를 P1 판(서비스 모름)으로 띄워 B 앱 중계로 보이는지.
- **M3 맥 — 등록·업데이트·TCC**: 서명된 preview 판 둘로 `SMAppService` 등록 → 권한 → Sparkle 갈아 끼우기 → 보는 사람 없을 때 갈아 끼우기. 사람의
  로그인 항목을 바꾸므로 별도 기기·별도 사용자에서, 사람 승인 뒤.
- **W1 Windows — 앱 없이 보내기**: S3b 처럼 관리자 ssh 에서 시험 서비스(`sc.exe create KasatermScreenTest binPath= "\"<리그>\bin\screen\0\kasaterm-screen.exe\" --screen-service"`,
  서비스 환경 `KASATERM_SCREEN_SERVICE_NAME=KasatermScreenTest`·`KASATERM_SCREEN_PIPE`·`KASATERM_SCREEN_CTL`·`KASATERM_SCREEN_STATE`·`KASATERM_KASANET_BIND` 로 사람
  서비스·도우미 파이프와 가른다),
  리그 폴더 exe, 시험 뒤 바로 `sc.exe delete`. 보는 쪽은 같은 실행 파일의 탐침(`kasaterm-screen --probe-watch <키> <screen-status.json> <초>`)이다 —
  탐침 id(`--probe-id`)를 리그 상태 폴더 `allow.json` 에 미리 넣고, 루프백 밖 UDP 를 안 열게 둘 다 `KASATERM_KASANET_BIND=127.0.0.2:0`. 앱 없이 장·소리가
  오는지, 지연(뜸→받음)이 앱 직결과 견줘 늘지 않는지 잰다.
- **W2 Windows — MSI 업데이트**: P0 의 시험 MSI 셋을 실제로 깔아 본다(사람 설치판과 다른 ProductCode·UpgradeCode·서비스 이름으로).
- **W3 Windows — 소리·보안 데스크톱**: UAC 창이 장에 실리고 원격 Esc 로 닫히는 S3b 시험을 서비스 길로 다시. 소리는 세션 0 이 튼 소리를 뜨던 S4 실측을
  서비스 길로 다시.
- 시험: `kasa-cast` 제어 규약(허용 목록 통째 바꿈·저장·다시 읽기, 폰 만료, 빈 목록이면 거절, `proto` 고르기), 보는 쪽이 `screen_node` 를 먼저 쓰고
  직통이 아니면 앱 엔드포인트로 물러나기, 앱 중계가 옛 보는 쪽에 장·조작을 그대로 잇기(로컬 엔드포인트 둘).

## 재야 할 것 — P0 결과

| # | 모르는 것 | 결과 |
|---|---|---|
| 1 | MSI 가 앱만 바뀐 업데이트에서 서비스를 안 멈추는가 | **[확인] 된다** — 위 「Windows — MSI」 꼴(Permanent 서비스 구성요소, 예약 그대로)로. 서비스 판이 바뀌어도 안 멈추고 서비스가 스스로 갈아탄다 |
| 2 | SYSTEM 도우미가 사용자 세션 소리를 뜨고 장치를 음소거하나 | **[확인] 된다** — 꾸러미 300개/3초·봉우리 −16.4dBFS, 음소거 켜기·되돌리기 |
| 3 | Sparkle 갈아 끼우기 뒤 도우미가 살고 등록·TCC 가 이어지나 | P4 에서 잰다 — 서명된 preview 판 둘과 사람의 로그인 항목 승인이 필요하다 |
| 4 | 맥미니 거부의 까닭 | 못 쟀다 — 2026-11-21 맥미니 ssh 길(직접·Cloudflare·경유)이 다 닫혀 있었다. P4 에서 다시 본다 |
| 5 | 도우미 → 서비스 파이프가 지연을 더하나 | **[확인] 거의 안 더한다** — W1(2026-11-21 집 PC): 뜸→받음 p50 6.5·p90 8.5ms(앱 직결 화면→풀림 6–7ms), 압축 끝→받음 p50 0.28ms |
| 6 | 세션 0 서비스의 방화벽·UPnP | 루프백 리그만 쟀다(방화벽 무관). MSI 가 서비스 실행 파일에 들어오는 UDP 예외를 건다. 실기 설치판에서 같은 망 직통을 본다 |
| 7 | 두 프로세스가 같은 출력을 DXGI 복제할 수 있나 | **[확인] 된다** — SYSTEM 프로세스 둘이 같은 출력을 함께 복제해 4초에 476장씩 받았다(그 PC 주사율) |

## 결정 (2026-11-21)

모두 권장안으로 정했다. 단계 사이에 사람 확인을 기다리지 않고 P0 부터 P5 까지 잇는다.

1. 서비스 키 — **서비스 자기 키.** 국내 중계 허용 목록에 기기마다 서비스 id 를 한 줄 더 넣는다.
2. Windows 설치 — **한 MSI 안 판 붙은 구성요소.** P0 의 재야 할 것 1 이 실패하면 서비스 전용 MSI 로 바꾸고 그 까닭을 이 문서에 적는다.
3. 맥 등록 — **`SMAppService.agent`**(13+). 13 이전은 앱 안에서 보낸다.
4. 옛 보는 쪽 — **앱이 한두 판 중계**한다.
5. Windows 로그인 전 화면 — **기본 켬.** 붙는 쪽은 그대로 명부 기기·주인 폰뿐이다.
6. 누가 볼 수 있나 — **지금처럼 명부 기기 전부·주인 폰.** 기기별 칸은 두지 않는다.
7. 첫 짝짓기 — **앱을 한 번 띄우는 것으로 한다.** 앱 없이 짝짓는 CLI 는 두지 않는다.
8. 폰 JPEG — **P5 에서 서비스로 옮긴다.**
9. 서비스 판이 바뀌었는데 누가 보고 있으면 — **보는 사람이 다 떠날 때까지 미룬다.**

## 그 뒤: 보는 쪽 작은 실행 파일(선택)

같은 워크스페이스에서 보는 칸만 담은 작은 실행 파일을 굽는 길이다. 보는 쪽 코드(`view.rs`·풀기·`kasa-cast` 의 `watch`)는 P1 뒤 이미 앱 밖
크레이트에 있어 창 하나(wgpu)와 기기 고르기만 더하면 된다. 그 실행 파일도 자기 카사넷 id 가 있어야 하고 보내는 쪽 허용 목록에 들어야 하므로,
명부에 「보기 전용 기기」로 올리는 길이 먼저다. 문서 보기 앱 KasaViewer 와는 다른 것이다.
