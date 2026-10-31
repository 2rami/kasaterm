# 키체인 비밀 읽기 — 폰 Face ID 한 번으로

학생이 맥에서 일하다 비밀(토큰·암호·API 키)이 필요할 때, **그 맥 로그인 키체인의 항목**을 **폰 Face ID 승인 한 번**으로
그 요청 하나만 꺼내 준다. 2026-10-06 에 1Password 서비스 계정으로 세웠던 길을 2026-10-19 에 키체인으로 바꿨다 —
1Password CLI·서비스 계정·전용 금고·Go 실행기(`kasa-op`)를 맥마다 갖추는 것이 너무 무거웠고, 비밀은 이미 각 맥의 키체인에 있다.
폰 승인·서명·재확인 길은 그대로다.

## 길

```
학생 칸  kasaterm-cli secret read <서비스>[#계정]            (secret run -e NAME=<서비스>[#계정] -- 명령 도 같다)
  → CLI 가 참조를 keychain://<서비스>[#계정] 꼴로 바꿔 앱 소켓 op.secret 에 — 연결을 붙든 채 기다린다
  → 앱: 참조 모양을 보고 도전값을 만든다
        {v, kind:"keychain.read", nonce, account, device, student, pane, cwd, command, refs[], exp}
  → 관문 POST /relay/approvals {kind:"secret", challenge} ─→ 폰 APNs + 승인 화면
  ← 폰: 사람이 내용을 보고 [Face ID 로 허락] → Secure Enclave 키가 "kasaterm-secret/1\n"+도전값에 서명
        → POST …/decide {decision, digest, key, sig}
  관문: 결정한 기기가 폰인지·그 폰이 맡긴 키인지·서명·지문·만료를 보고서만 allowed
  → 앱: 올린 도전값 그대로인지·사람이 맥에서 믿은 키인지·서명·nonce 처음인지·만료 전인지 다시 본다
  → 앱이 키체인에서 꺼내 소켓으로 요청한 프로세스에만(read: stdout, run: 자식 환경)
```

값은 관문·폰을 지나지 않는다. 관문과 폰이 보는 것은 명령·참조(`keychain://서비스#계정`)·학생·칸뿐이다.

## 키체인 항목 고르기

- 참조는 `keychain://<서비스>[#계정]`. 계정은 마지막 `#` 뒤다. CLI 에서는 `서비스` · `서비스#계정` 만 쳐도 된다.
- 앱은 먼저 일반 암호(`kSecClassGenericPassword`, 서비스=`서비스`)를, 없으면 인터넷 암호(서버=`서비스`)를 찾는다.
  계정을 안 주면 그 서비스의 첫 항목이다. `security add-generic-password -s 서비스 -a 계정 -w` 로 넣은 것이 그대로 읽힌다.
- **처음 꺼내는 항목은 그 맥에 키체인 접근 허용 창이 뜬다**(항목을 만든 앱이 kasaterm 이 아니면). 사람이 맥 앞에서
  「항상 허용」을 눌러야 다음부터 창 없이 나온다. 폰 승인과 별개다 — 폰에서 허락해도 맥 창이 거절되면 `keychain_denied`.
  「항상 허용」은 앱 서명에 묶이므로 같은 서명의 업데이트 판은 다시 묻지 않는다.
- 값은 글자(UTF-8)만 넘긴다. 아니면 `not_text`.

### Windows — 자격 증명 관리자

- 같은 참조를 자격 증명 관리자의 **일반 자격 증명**(대상=`서비스`, 사용자=`계정`)으로 읽는다. 넣는 길:
  `cmdkey /generic:서비스 /user:계정 /pass` (값을 묻는다). 계정을 안 주면 그 대상의 값이다.
- 대상 하나에 사용자는 하나뿐이라, 같은 서비스에 계정이 여럿이면 `계정@서비스` 대상으로 둔 것도 본다(Python keyring 꼴).
- 남이 넣은 값은 UTF-16 으로 적힌 것이 많아(cmdkey·keyring·Git 자격 증명 관리자) 0 바이트를 품으면 UTF-16 으로 읽어
  UTF-8 로 넘긴다. Windows 자격 증명(도메인 암호)·웹 자격 증명은 앱이 읽을 수 없어 대상이 아니다.
- **접근 허용 창 같은 둘째 문은 없다** — 같은 사용자의 프로세스면 묻지 않고 읽힌다. 값은 폰 Face ID 허락 뒤에만 꺼낸다.
- 폰 열쇠 믿음은 일반 자격 증명 `kasaterm.1password/trusted-keys`·`kasaterm.1password/refused-keys` 다. 한 자격 증명은
  2560바이트까지라 넘으면 `#2`·`#3` … 대상에 잇고 머리의 설명 칸에 조각 수(`parts=N`)를 적는다.
- 요청한 칸은 이름 붙은 파이프의 상대 pid(`GetNamedPipeClientProcessId`)에서 부모를 따라 찾는다. 믿기 시트는 결정 시트
  ([design.md](design.md) 「결정 시트」)다.

## 지키는 것

- **학생은 키체인에 직접 손대지 않는다.** 학생 프로세스가 `security find-generic-password -w` 로 읽으려 하면 macOS 가
  그 맥에 허용 창을 띄운다. 소켓·`kasaterm-cli` 에는 키체인 쓰기·목록 동작이 없다. 값은 대화·로그·레포·감사 줄 어디에도 안 적는다.
- **범위는 「이 맥 키체인의 아무 항목」이다**(2026-10-19 결정). 금고 같은 울타리가 없으니, 폰 승인 화면의 참조를
  사람이 보고 판단한다. 참조는 16개·256자까지, 제어 문자 없이.
- **요청마다 한 번.** nonce 128비트, 만료는 관문 승인과 같은 2분, 결정 하나. 쓴 nonce·요청 id 는 만료까지 앱이
  기억해 같은 허락을 두 번 쓰지 않는다. 연달아 읽기는 `secret run -e A=… -e B=…` 한 요청에 참조 여럿으로.
- **서명은 두 곳에서 본다.** 관문은 폰이 맡긴 공개키로, 맥은 **맥이 믿은** 공개키로. 맥은 그 키가 관문 목록에도
  살아 있어야 쓴다(관문에서 폐기된 폰은 맥에서도 바로 막힌다). 처음 들어온 폰 열쇠는 사람 확인 없이 믿으므로
  ([연결](#연결)), **관문이 열쇠를 끼워 넣으면 막지 못한다**(선택한 대가). 끼워 넣지 않는 한 관문 혼자서는 허락을 지어낼 수 없다.
- **보인 것 그대로.** 서명 대상은 도전값 전체(정규 JSON — 키 정렬, 공백 없음)다. 관문 지문(digest)도 도전값을
  묶는다. 명령·참조·학생·칸·만료 중 하나라도 바뀌면 서명이 안 맞는다. 명령 안의 다른 비밀 꼴(`sk-…` 등)은 가려 보낸다.
- **Face ID 는 폰 안에서만.** 「Face ID 성공」 참/거짓을 서버가 믿지 않는다. 키가 `.biometryCurrentSet` 으로 묶여
  Face ID 가 풀어야만 서명이 나온다. 취소·실패면 아무것도 안 보낸다. 생체 정보가 바뀌면 키가 무효가 되어 다시 등록한다.
- **감사.** 관문 감사 줄(원격 승인과 같은 파일, `kind:"secret"`·참조 목록) + 맥 `~/.config/kasaterm/op-audit.jsonl`
  (0600, 2MB 에서 한 번 돌림): 시각·요청 id·학생·칸·명령·참조·결정 기기·키 id·결과(꺼냄/거부 이유). 값 없음.
- **한계.** 같은 사용자 권한으로 작정한 프로세스가 키 입력을 흉내 내 사람 대신 맥 화면 단추를 누르는 것까지는
  막지 못한다(원격 승인 문서의 한계와 같은 급). 그 경우에도 폰 Face ID 없이는 서명이 안 나온다.

## 이름에 남은 1Password

폰 열쇠 믿음은 키체인 서비스 `kasaterm.1password`(계정 `trusted-keys`·`refused-keys`)에 그대로 둔다 — 이름을 바꾸면 연결해 둔
폰을 다시 믿어야 한다. 소켓 방법 `op.secret`·CLI 별칭 `op`·모듈 `op_approval` 도 옛 이름이다.

관문과 폰은 한동안 옛 맥이 보내는 `kind:"op.read"`·`op://` 도전값도 받는다(종류와 참조 꼴이 짝이어야 한다). 모든 맥이 키체인 판으로
오르면 그 갈래를 걷는다.

## CLI

| 명령 | 하는 일 |
|---|---|
| `kasaterm-cli secret read <서비스>[#계정]` | 승인되면 값을 표준 출력으로(끝 줄바꿈 없음). 터미널에 바로 찍히면 거절(`--reveal` 로만) |
| `kasaterm-cli secret run -e NAME=<서비스>[#계정] [-e …] -- <명령…>` | 승인 한 번으로 여러 참조. 값은 자식 환경으로만 가고, 자식 출력에 그 값이 나오면 `<concealed>` 로 가린다 |
| `kasaterm-cli secret status` | 믿는 폰 열쇠(값 없음) |

`op` 는 `secret` 의 별칭이다. `op://…` 참조는 「1Password 는 걷었어요」로 거절한다.

거절 코드: `bad_ref` · `no_trusted_key`(믿는 폰 열쇠 없음) · `denied` · `expired` · `cancelled` · `bad_signature` · `replayed` ·
`not_found`(그 키체인 항목 없음) · `keychain_denied`(맥의 허용 창에서 거절) · `not_text`.

카사크롬 로그인 채우기(`kasachrome/README.md` 「로그인 채우기」)가 `secret read web:<host>[#계정]` 을 부르고 stderr 마지막 줄 끝의
`(코드)` 로 거절 사유를 가른다(`kasachrome/mcp/login.mjs`). 이 꼴을 바꾸면 그쪽도 맞춘다.

## 관문 창구

| 창구 | 누가 | 하는 일 |
|---|---|---|
| `POST /relay/approvals` + `kind:"secret"`, `challenge` | 데스크톱 | 비밀 요청 열기. 도전값 4KB 상한, 지문에 묶임. 승인 화면의 출처 이름(`tool`)은 「키체인」 |
| `POST /relay/approvals/{id}/decide` + `key`, `sig` | 폰만(비밀 요청 허락) | 거절은 서명 없이 된다. 허락은 그 폰이 맡긴 키·서명이 맞아야 |
| `POST /relay/approvals/keys` `{pub}` | 폰 | Secure Enclave 공개키(X9.63, 65바이트) 맡기기. 기기당 하나, 갈아 끼우기 |
| `GET /relay/approvals/keys` | 계정 기기 | 살아 있는 폰의 키 목록(기기·이름·키 id·지문·맡긴 시각) |
| `DELETE /relay/approvals/keys` | 폰 | 자기 키 지우기 |

거절 코드: `signature_required` · `bad_signature` · `phone_only` · `key_unknown` · `challenge_invalid`.

## 연결

맥에서 할 일은 없다(넣을 토큰이 없다). 폰에서 한 번:

**폰 — 「Face ID 로 연결」**(설정 → 계정 → 비밀 승인, `approval_key.dart`): Secure Enclave 열쇠를 만들고(없을 때만) Face ID 로
`kasaterm-link/1\n` 에 한 번 서명해 본다(이 머리는 승인 머리와 달라 어떤 허락으로도 안 읽히고 어디에도 안 보낸다). 공개키를
관문에 맡긴 뒤 계정의 켜진 맥마다 설정 동작 `op-link` 를 부른다 → 맥이 아래 규칙을 지금 돌리고 믿는 열쇠 지문을 준다.
폰은 맥마다 「연결됨」을 줄로 보인다.

**맥이 폰 열쇠를 믿는 규칙**(`op_approval::auto_pick`, 화면 1분 주기·`op-link`·학생 요청 직전에 돈다):
- 이미 믿은 기기가 갈아 끼운 열쇠(Face ID 재등록)는 늘 믿는다.
- 그런 열쇠가 없고 믿는 열쇠 중 관문에 살아 있는 것도 없으면, 들어온 열쇠를 다 믿는다(처음 연결).
- 그 밖의 새 기기(믿는 폰이 이미 있는데 들어온 다른 기기)는 맥 설정 → 계정 → 비밀 승인의 지문 시트로 사람이 믿는다.
- 사람이 맥에서 거둔 열쇠 id 는 기억해 다시 집지 않는다(`refused-keys`, 최근 64개).

거두기: 폰에서 「연결 끊기」 · 관문에서 기기 폐기 · 맥 설정에서 믿음 거두기 — 어느 하나면 막힌다.

## 시험

- 관문(`cargo test -p kasa-mcp gateway_approvals`): 정상 허락·서명 없음·틀린 서명·다른 폰 키·데스크톱이 허락 시도·
  변조(도전값/지문)·만료·nonce 재사용·취소 뒤 결정·폐기 기기의 키·키 갈아 끼운 뒤 옛 서명·옛 `op.read` 도전값·종류와 참조 꼴 엇갈림.
- 맥(`cargo test -p kasa-mcp op_approval`): 같은 허락 두 번(재사용), 관문이 바꾼 도전값, 믿지 않은 키, 관문 목록에서 빠진 키,
  만료, 참조 모양.
- CLI(`cargo test -p kasa-socket keychain_refs`): 짧은 꼴 → `keychain://`, `op://` 거절.
- 폰: Face ID 취소·실패 시 아무것도 안 보냄, 서명 형식(DER) — 맥 `ring` 으로 검증되는지, 키체인·옛 1Password 요청 둘 다 읽는지.
- 격리 리그 + 시험 관문 + 가상 아이폰(SE 가 없어 시뮬레이터 전용 시험 키, 실기판에는 안 들어감).
  Face ID 실기와 맥 키체인 허용 창은 사람이 확인한다.

## 결정

| 갈림길 | 고른 것 |
|---|---|
| 비밀이 사는 곳(2026-10-19) | 그 맥의 로그인 키체인. 1Password 서비스 계정·실행기는 걷었다 |
| 범위(2026-10-19) | 키체인 아무 항목. 폰 승인 화면에서 참조를 보고 사람이 판단한다 |
| 승인(2026-10-19) | 폰 Face ID 유지. 맥 Touch ID 로 바꾸지 않는다 — 사람이 맥 앞에 없을 때 쓰는 길이다 |
| 연달아 읽기 | 묶음 없음 — 요청마다 Face ID. 대신 한 요청에 참조 여럿(최대 16) |
| 폰 열쇠 믿기(2026-10-13) | 지문 맞춰 보기 대신 처음 들어온 열쇠를 자동으로 믿는다 — 관문 끼워 넣기를 못 막는 대가를 받아들였다 |

## 자리

| 자리 | 파일 |
|---|---|
| 관문 비밀 요청·폰 키·서명 확인 | `crates/kasa-mcp/src/gateway_approvals.rs` |
| 서명 머리·키 id·지문·서명 검증(관문·맥 공용) | `crates/kasa-mcp/src/approval_text.rs` |
| 맥: 도전값·재확인·nonce·키체인 꺼내기·감사 | `crates/kasa-mcp/src/op_approval.rs` |
| 맥: 관문 호출(`create_secret`·`keys`) | `crates/kasa-mcp/src/device_approvals.rs` |
| 맥: 소켓 `op.secret`(칸·명령·폴더를 소켓 상대 pid 에서) | `app/kasaterm/src/socket.rs` · `crates/kasa-socket/src/server.rs`(`peer_pid`) |
| 맥: 설정 「계정」 비밀 승인(믿는 폰·믿기 시트) | `app/kasaterm/src/native_op_approval.rs` |
| 맥: 폰이 부르는 `op-link`(자동 믿기 지금 돌리기) | `app/kasaterm/src/socket.rs` 설정 동작 |
| CLI | `crates/kasa-socket/src/cli.rs`(`secret read|run|status`, 별칭 `op`) |
| 폰 | `mobile/lib/secure_key.dart` · `mobile/lib/approvals.dart` · `mobile/lib/screens/approval_screen.dart` · `mobile/lib/screens/approval_key.dart` · `mobile/ios/Runner/AppDelegate.swift`(`ApprovalKey`) |

## 검증 리그

[remote-approval.md](remote-approval.md) 「검증 리그」의 시험 관문·기기 파일에 더해, 리그 앱에

```bash
KASATERM_OP_STORE_DIR=/tmp/<리그>/op        # 키체인 대신 이 폴더(격리 실행은 키체인을 절대 안 쓴다)
                                            # 꺼낼 값은 그 안 keychain.json {"서비스#계정": "값", "서비스": "값"}
KASATERM_OP_AUTOTRUST_MS=5000 KASATERM_OP_AUTOTRUST_SHOT=/tmp/<리그>/trust.png   # 검증 실행: 새 열쇠 믿기 시트를 찍고 누른다
```

를 준다. 설정 화면만 볼 때는 `KASATERM_AUTOSETTINGS=accounts KASATERM_AUTOSETTINGS_ACTION=device-op`(믿는 폰·새 열쇠) 또는
`device-op-empty`(처음 연결 전) 로 띄운다. 학생 요청은 리그 칸 안에서 `kasaterm-cli secret read …` 로 낸다(칸 밖 프로세스는
`not_in_pane`). 가상 아이폰은 Secure Enclave 가 없어 시뮬레이터 전용 시험 키(접근 제어 없음)를 만들고 서명 전에 Face ID 를 따로
묻는다 — Features → Face ID → Matching Face 로 흉내 낸다. 이 갈래는 `#if targetEnvironment(simulator)` 라 실기 판에는 안 들어간다.

## 운영에 올릴 때

순서: 관문 → 폰 → 맥. 관문이 `keychain.read` 를 모르면 맥의 요청이 `challenge_invalid` 로 거절되고, 폰이 모르면 승인 화면이
허락 단추 없이 뜬다.

- 관문: `tools/kasa-gateway-seoul/gateway.sh build` → `gateway.sh upgrade ./out/kasa-relay`. 폰 키는 상태 폴더의
  `relay-approval-keys.json`(0600)에 남는다.
- 사람이 처음 한 번: 폰 설정 → 계정 → 비밀 승인 → 「Face ID 로 연결」. 꺼낼 비밀은 맥 키체인에 넣어 둔다
  (`security add-generic-password -s 서비스 -a 계정 -w`, 또는 키체인 접근 앱). 항목마다 처음 한 번 맥의 허용 창에서 「항상 허용」.
