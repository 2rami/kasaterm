# 1Password 비밀 읽기 — 폰 Face ID 한 번으로

학생이 맥에서 1Password 비밀이 필요할 때, 맥의 Touch ID 창 대신 **폰 Face ID 승인 한 번**으로 그 요청 하나만
풀리게 한다. 2026-10-06 결정으로 [nacho-orchestrator.md](nacho-orchestrator.md) 「앱 안 승인(Face ID) 설계」
3절의 「1Password 잠금 해제는 안 한다」를 이 길로 바꾼다. 1Password 데스크톱 앱의 잠금을 밖에서 푸는 공식 길은
없으므로, 잠금을 풀지 않고 **전용 금고만 읽는 서비스 계정 토큰**을 맥에 두고 그 토큰을 쓰는 순간마다 폰 서명을 받는다.

## 길

```
학생 칸  kasaterm-cli op read op://<금고>/<항목>/<필드>        (op run -e NAME=op://… -- 명령 도 같다)
  → 앱 소켓 op.request — 연결을 붙든 채 기다린다
  → 앱: 참조가 허용 금고인지 보고 도전값을 만든다
        {v, kind:"op.read", nonce, account, device, student, pane, cwd, command, refs[], exp}
  → 관문 POST /relay/approvals {kind:"secret", challenge, input:{command, refs}} ─→ 폰 APNs + 승인 화면
  ← 폰: 사람이 내용을 보고 [Face ID 로 허락] → Secure Enclave 키가 "kasaterm-secret/1\n"+도전값에 서명
        → POST …/decide {decision, digest, key, sig}
  관문: 결정한 기기가 폰인지·그 폰이 맡긴 키인지·서명·지문·만료를 보고서만 allowed
  → 앱: 올린 도전값 그대로인지·사람이 맥에서 믿은 키인지·서명·nonce 처음인지·만료 전인지 다시 본다
  → 실행기에 토큰을 파이프로 넘겨 값 받기 → 소켓으로 요청한 프로세스에만(read: stdout, run: 자식 환경)
```

값은 관문·폰을 지나지 않는다. 관문과 폰이 보는 것은 명령·참조(`op://금고/항목/필드`)·학생·칸뿐이다.

## 지키는 것

- **토큰은 사람만 넣는다.** 맥 앱 설정의 암호 칸(NSSecureTextField)에 붙여 넣으면 앱이 로그인 키체인에
  `kasaterm.1password.service-account` 로 넣고, 접근 목록은 kasaterm 앱 하나다. 학생 프로세스가
  `security find-generic-password` 로 읽으려 하면 macOS 가 키체인 암호 창을 띄운다. 소켓·`kasaterm-cli` 에는
  토큰 넣기·읽기 동작이 없다. 토큰은 대화·로그·레포·감사 줄 어디에도 안 적는다.
- **범위는 1Password 가 정본.** 서비스 계정은 전용 금고 하나에 읽기 권한만 준다(개인 금고는 서비스 계정이 원래 못
  연다). 앱은 참조의 금고가 허용 목록에 있는지 한 번 더 본다 — 틀린 참조는 폰에 띄우기 전에 거절한다.
- **요청마다 한 번.** nonce 128비트, 만료는 관문 승인과 같은 2분, 결정 하나. 쓴 nonce·요청 id 는 만료까지 앱이
  기억해 같은 허락을 두 번 쓰지 않는다. 연달아 읽기는 [묶음](#정할-것)에서 정한다.
- **서명은 두 곳에서 본다.** 관문은 폰이 맡긴 공개키로, 맥은 **사람이 맥 화면에서 믿은** 공개키로. 맥은 그 키가
  관문 목록에도 살아 있어야 쓴다(관문에서 폐기된 폰은 맥에서도 바로 막힌다). 그래서 관문 혼자서는 허락을 지어낼
  수 없고, 관문이 키를 지우는 쪽으로만 영향을 준다.
- **보인 것 그대로.** 서명 대상은 도전값 전체(정규 JSON — 키 정렬, 공백 없음)다. 관문 지문(digest)도 도전값을
  묶는다. 명령·참조·학생·칸·만료 중 하나라도 바뀌면 서명이 안 맞는다. 잘린 요청은 원격 승인과 같이 허락할 수 없다.
- **Face ID 는 폰 안에서만.** 「Face ID 성공」 참/거짓을 서버가 믿지 않는다. 키가 `.biometryCurrentSet` 으로 묶여
  Face ID 가 풀어야만 서명이 나온다. 취소·실패면 아무것도 안 보낸다. 생체 정보가 바뀌면 키가 무효가 되어 다시
  등록한다.
- **실행기 검증.** 토큰을 넘기기 전에 실행기 프로세스의 코드 서명을 pid 로 확인한다([실행기](#실행기)).
- **감사.** 관문 감사 줄(원격 승인과 같은 파일, `kind:"secret"`·참조 목록) + 맥 `~/.config/kasaterm/op-audit.jsonl`
  (0600, 2MB 에서 한 번 돌림): 시각·요청 id·학생·칸·명령·참조·결정 기기·키 id·결과(실행/거부 이유). 값·토큰 없음.
- **한계.** 같은 사용자 권한으로 작정한 프로세스가 키 입력을 흉내 내 사람 대신 맥 화면 단추를 누르는 것까지는
  막지 못한다(원격 승인 문서의 한계와 같은 급). 그 경우에도 폰 Face ID 없이는 서명이 안 나온다.

## 실행기

`op` CLI 는 서비스 계정 토큰을 **환경 변수(`OP_SERVICE_ACCOUNT_TOKEN`)로만** 받는다. 2026-10-06 실측:

- 같은 사용자 프로세스가 `ps eww`·`sysctl KERN_PROCARGS2` 로 실행 중인 `op` 의 환경을 읽을 수 있다
  (애플 플랫폼 바이너리만 가려진다 — `/bin/sleep` 은 안 보이고 `op` 는 보였다).
- `op` 는 처음 실행 때 캐시 데몬(`op daemon`)을 띄우고, 그 데몬이 **토큰이 든 환경을 물려받아 계속 산다**.

그래서 기본안은 1Password 공식 Go SDK(`onepassword-sdk-go`)로 만든 작은 실행기 `kasa-op` 를 앱 번들에 넣는 것이다.
앱이 실행기를 띄우고, 그 pid 의 코드 서명(우리 팀 ID, hardened runtime)을 확인한 뒤 **표준 입력으로** 토큰과 참조를
넘긴다. 환경·인자에 토큰이 없고, hardened 프로세스라 같은 사용자도 메모리를 못 붙든다. 대가는 빌드에 Go 가 필요하고
번들이 약 20MB 커진다(시험 빌드 20.4MB).

## CLI

| 명령 | 하는 일 |
|---|---|
| `kasaterm-cli op read <op://…>` | 승인되면 값을 표준 출력으로(끝 줄바꿈 없음) |
| `kasaterm-cli op run -e NAME=<op://…> [-e …] -- <명령…>` | 승인 한 번으로 여러 참조. 값은 자식 환경으로만 가고, 자식 출력에 그 값이 나오면 `<concealed>` 로 가린다 |
| `kasaterm-cli op status` | 토큰 있음/없음·허용 금고·믿는 폰 열쇠 수(값 없음) |

학생 지침(`collab-protocol.md`)에 「1Password 비밀은 `op` 를 직접 부르지 말고 `kasaterm-cli op`」을 넣는다.
`op` 자체를 가로채는 셔틀은 두지 않는다.

거절 코드: `op_token_missing`(맥에 토큰 없음) · `vault_not_allowed` · `no_trusted_key`(믿는 폰 열쇠 없음) ·
`denied` · `expired` · `cancelled` · `bad_signature` · `replayed` · `resolve_failed`(1Password 가 거절, 이유 동봉).

## 관문 창구(더하는 것)

| 창구 | 누가 | 하는 일 |
|---|---|---|
| `POST /relay/approvals` + `kind:"secret"`, `challenge` | 데스크톱 | 비밀 요청 열기. 도전값 4KB 상한, 지문에 묶임 |
| `POST /relay/approvals/{id}/decide` + `key`, `sig` | 폰만(비밀 요청 허락) | 거절은 서명 없이 된다. 허락은 그 폰이 맡긴 키·서명이 맞아야 |
| `POST /relay/approvals/keys` `{pub}` | 폰 | Secure Enclave 공개키(X9.63, 65바이트) 맡기기. 기기당 하나, 갈아 끼우기 |
| `GET /relay/approvals/keys` | 계정 기기 | 살아 있는 폰의 키 목록(기기·이름·키 id·지문·맡긴 시각) |
| `DELETE /relay/approvals/keys` | 폰 | 자기 키 지우기 |

새 거절 코드: `signature_required` · `bad_signature` · `phone_only` · `key_unknown` · `challenge_invalid`.

## 등록 — 폰 열쇠를 맥이 믿기까지

1. 폰 설정 → 계정 → 「Face ID 승인 열쇠」 [만들기]: Secure Enclave 에 P-256 키(`.privateKeyUsage` +
   `.biometryCurrentSet`)를 만들고 공개키를 관문에 맡긴다. 화면에 지문(공개키 SHA-256 앞 12자, `ABCD-EFGH-IJKL`).
2. 맥 앱은 관문 키 목록에서 아직 안 믿은 키를 보면 알림 「새 Face ID 열쇠 · 폰 이름」 → [보기] 시트에 같은 지문.
   사람이 폰 화면과 같은지 보고 [믿기]. 시트의 Return 은 [취소]다. 믿은 키는 맥 키체인(앱 전용)에 둔다.
3. 거두기: 폰에서 열쇠 지우기 · 관문에서 기기 폐기 · 맥 설정에서 믿음 거두기 — 어느 하나면 막힌다.

## 시험

- 관문: 정상 허락·서명 없음·틀린 서명·다른 폰 키·데스크톱이 허락 시도·변조(도전값/지문)·만료·nonce 재사용·
  취소 뒤 결정·폐기 기기의 키·키 갈아 끼운 뒤 옛 서명.
- 맥: 같은 허락 두 번(재사용), 관문이 바꾼 도전값, 믿지 않은 키, 관문 목록에서 빠진 키, 만료, 요청 프로세스가
  사라짐(취소), 금고 밖 참조, 토큰 없음, 실행기 서명 불일치.
- 폰: Face ID 취소·실패 시 아무것도 안 보냄, 서명 형식(DER) — 맥 `ring` 으로 검증되는지.
- 격리 리그 + 시험 관문 + 가상 아이폰(SE 가 없어 시뮬레이터 전용 시험 키, 실기판에는 안 들어감).
  Face ID 실기는 사람이 확인한다.

## 결정(2026-10-06)

| 갈림길 | 고른 것 |
|---|---|
| 금고 범위 | 전용 금고 하나(읽기). 토큰을 넣을 때 실행기로 그 토큰이 보는 금고를 물어 **하나일 때만** 받고, 그 금고 밖 참조는 폰에 띄우기 전에 거절한다 |
| 토큰 저장 자리 | 로그인 키체인, 접근은 kasaterm 앱 하나(설정 → 계정 → 1Password 의 암호 칸) |
| 연달아 읽기 | 묶음 없음 — 요청마다 Face ID. 대신 `op run -e A=… -e B=…` 한 요청에 참조 여럿(최대 16) |
| 실행기 | Go SDK 실행기 `kasa-op`(앱 번들 `Contents/MacOS/kasa-op`, 토큰은 표준 입력) |

## 자리

| 자리 | 파일 |
|---|---|
| 관문 비밀 요청·폰 키·서명 확인 | `crates/kasa-mcp/src/gateway_approvals.rs` |
| 서명 머리·키 id·지문·서명 검증(관문·맥 공용) | `crates/kasa-mcp/src/approval_text.rs` |
| 맥: 도전값·재확인·nonce·실행기·키체인·감사 | `crates/kasa-mcp/src/op_secret.rs` |
| 맥: 관문 호출(`create_secret`·`keys`) | `crates/kasa-mcp/src/device_approvals.rs` |
| 맥: 소켓 `op.secret`(칸·명령·폴더를 소켓 상대 pid 에서) | `app/kasaterm/src/socket.rs` · `crates/kasa-socket/src/server.rs`(`peer_pid`) |
| 맥: 설정 「계정」 1Password(토큰·믿기 시트) | `app/kasaterm/src/native_op_secrets.rs` |
| CLI | `crates/kasa-socket/src/bin/kasaterm-cli.rs`(`op read|run|status`) |
| 실행기 | `tools/kasa-op/`(Go, `scripts/build-app.sh` 가 굽고 서명) |
| 폰 | `mobile/lib/secure_key.dart` · `mobile/lib/approvals.dart` · `mobile/lib/screens/approval_screen.dart` · `mobile/lib/screens/approval_key.dart` · `mobile/ios/Runner/AppDelegate.swift`(`ApprovalKey`) |

## 검증 리그

[remote-approval.md](remote-approval.md) 「검증 리그」의 시험 관문·기기 파일에 더해, 리그 앱에

```bash
KASATERM_OP_STORE_DIR=/tmp/<리그>/op        # 키체인 대신 이 폴더(격리 실행은 키체인을 절대 안 쓴다)
KASATERM_OP_HELPER=/tmp/<리그>/fake-op      # 가짜 실행기(이때만 서명 확인을 건너뛴다)
KASATERM_OP_AUTOTRUST_MS=5000 KASATERM_OP_AUTOTRUST_SHOT=/tmp/<리그>/trust.png   # 검증 실행: 새 열쇠 믿기 시트를 찍고 누른다
```

를 준다. 학생 요청은 리그 칸 안에서 `kasaterm-cli op read …` 로 낸다(칸 밖 프로세스는 `not_in_pane`). 가상 아이폰은 Secure
Enclave 가 없어 시뮬레이터 전용 시험 키(접근 제어 없음)를 만들고 서명 전에 Face ID 를 따로 묻는다 — Features → Face ID →
Matching Face 로 흉내 낸다. 이 갈래는 `#if targetEnvironment(simulator)` 라 실기 판에는 안 들어간다. 실기 Face ID 는 사람이 확인한다.

## 운영에 올릴 때

- 관문: 새 창구(`/relay/approvals/keys`, 비밀 요청)는 관문 바이너리 교체가 있어야 선다. 폰 키는 상태 폴더의
  `relay-approval-keys.json`(0600)에 남는다.
- 맥: 실행기는 `scripts/build-app.sh` 가 Go 로 굽는다. Go 가 없는 기계에서 구운 판에는 실행기가 없고, 설정이 「이 판에 실행기가
  없어요」라고 말한다. 빠른 업데이트를 굽는 controller 기계에 Go 가 있어야 preview 판에 들어간다.
- 사람이 처음 한 번: 1Password 에서 전용 금고를 만들고 그 금고 「읽기」만 가진 서비스 계정을 만든다 → 맥 설정 → 계정 →
  1Password → 토큰 넣기 → 폰 설정 → 계정 → Face ID 승인 열쇠 → 열쇠 만들기 → 맥에 뜬 새 열쇠의 지문을 맞춰 보고 믿기.
