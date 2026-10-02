# 원격 승인 — Claude Code 권한 요청을 폰·다른 맥에서

학생(claude) 칸이 도구 권한을 물으면 같은 계정의 폰과 다른 맥에도 그 요청이 뜨고, 어디서든 [허락]·[거절] 할 수 있다.
요청마다 한 번이고 「항상 허락」·규칙 추가는 없다. 엔진의 권한 창은 처음부터 함께 떠 있어 자리의 사람은 늘 거기서 답할 수
있고, 먼저 온 답이 이긴다(mod 쪽 계약 [claude-mod-bridge.md](claude-mod-bridge.md) 「권한 요청」).

## 길

```
mod(tool.check · classic.PermissionRequest)
  → 앱 브로커 kasa_mcp::claude_mod (loopback, 인증 없음)
  → 앱 다리 kasa_mcp::approval_bridge — 비밀을 가려 관문에 올린다
  → 관문 /relay/approvals (계정 기기 토큰) ─┬→ 폰: APNs 푸시 + 승인 화면
                                          └→ 다른 맥: 오른쪽 위 알림 + 원문 시트
  ← 결정(지문 동봉) → 관문 → 다리 → claude_mod::decide → mod 가 { decision } 으로 엔진에
```

| 자리 | 파일 |
|---|---|
| 관문 창구·푸시 | `crates/kasa-mcp/src/gateway_approvals.rs` |
| 표시 칸·비밀 가리기·지문 | `crates/kasa-mcp/src/approval_text.rs` |
| 데스크톱 관문 호출(결정은 앱 화면만) | `crates/kasa-mcp/src/device_approvals.rs` |
| 브로커 ↔ 관문 다리 | `crates/kasa-mcp/src/approval_bridge.rs` |
| 데스크톱 알림·시트 | `app/kasaterm/src/remote_approval.rs` |
| 폰 | `mobile/lib/approvals.dart` · `mobile/lib/screens/approval_screen.dart` · `mobile/lib/push.dart` |

## 관문 창구

전부 `Authorization: Bearer <기기 토큰>`. 계정은 토큰이 정한다 — 본문은 계정·기기를 말하지 못한다.

| 창구 | 누가 | 하는 일 |
|---|---|---|
| `POST /relay/approvals` | 데스크톱 기기만 | 요청 열기 `{student, pane, cwd, tool, input, truncated?, ttl?}`. 관문이 한 번 더 가려 칸으로 펴고, 기기 이름은 **자기 기록**에서 붙인다 |
| `GET /relay/approvals?since=&wait=` | 계정 기기 | 열린 요청 + 닫힌 지 2분 안 된 것. `since` 가 지금 판과 같으면 바뀔 때까지 최대 25초 붙든다 |
| `GET /relay/approvals/{id}?wait=` | 계정 기기 | 한 건. 닫히거나 만료될 때까지 붙든다(요청한 기기의 다리가 쓴다) |
| `POST /relay/approvals/{id}/decide` | 계정 기기 + `x-kasa-approver` | `{decision: allow\|deny, digest}`. 첫 결정 하나만 받는다 |
| `POST /relay/approvals/{id}/cancel` | 요청한 기기만 | `{reason: local\|gone\|timeout}` — 자리에서 답했거나 칸이 사라졌을 때 |
| `POST /relay/approvals/approver` | 계정 기기 | 이 관문 실행 동안 그 앱 화면의 승인 열쇠(43자) 해시를 맡긴다 |
| `POST·DELETE /relay/approvals/push` | 폰 | APNs 토큰 맡기기·거두기(기기 하나에 토큰 하나) |
| `GET /relay/approvals/audit?limit=` | 계정 기기 | 감사 줄, 최근 것부터 |

거절 코드: `desktop_only` · `already_closed`(409, 지금 상태 동봉) · `expired`(410) · `digest_mismatch` ·
`truncated_cannot_allow` · `approver_required` · `not_origin` · `too_many_pending`(계정당 열린 요청 32).

## 지키는 것

- **계정 주인 기기만.** 기기 토큰으로만 열리고 다른 계정의 요청은 보이지도 결정되지도 않는다. 폐기한 기기의 토큰은 바로
  막히고, 그 폰에는 푸시도 안 간다(보낼 때마다 기기 기록을 다시 본다).
- **일회용·2분.** 요청 id 는 관문이 만든 128비트 난수. 첫 결정이 이기고 나머지는 「어디서 닫혔나」를 받는다. 2분이 지나면
  원격 결정을 받지 않는다 — 칸에는 엔진 창이 남아 있다.
- **보인 것 그대로.** 관문이 칸·기기·학생·만료를 묶은 지문을 만들고, 결정은 그 지문을 실어 와야 받는다. 요청한 기기의 다리는
  올릴 때 받은 지문과 결정의 지문이 다르면 결정을 버린다. 원문이 상한(칸 48KB·요청 64KB)을 넘어 잘린 요청은 **허락할 수
  없다**(거절만) — 안 보인 꼬리를 허락하게 두지 않는다.
- **비밀은 가린다.** `approval_text::mask` 가 이름에 TOKEN·SECRET·PASSWORD·API_KEY 따위가 든 값, `--password`·`--token`
  깃발 값, `Authorization: Bearer …`, 공급자 머리(`sk-`·`ghp_`·`xoxb-`·`AKIA` …)가 붙은 토큰, JWT, URL 비밀번호, 개인키
  블록을 `••••••` 로 바꾼다. 나머지 글자는 한 글자도 바꾸지 않는다. 데스크톱이 가린 뒤 관문이 한 번 더 가린다. 관문은 요청을
  메모리에만 둔다.
- **결정은 사람이 누른 화면에서만.** 결정 창구는 앱 실행 동안만 메모리에 있는 승인 열쇠를 요구한다. 데스크톱은 시트의 단추만
  그 열쇠로 보낸다 — 소켓·`kasaterm-cli` 에는 결정 동작이 없다. (같은 사용자 권한으로 도는 프로그램은 `device.json` 을
  읽어 스스로 열쇠를 맡길 수 있다. 이 장치는 정해진 길로 새는 것을 막을 뿐이고, 같은 기계의 키 입력 흉내와 같은 급의 한계다.)
- **감사.** 관문 `relay-approvals-audit.jsonl`(0600, 2MB 에서 한 번 돌림) — 요청·허락·거절·만료·닫힘마다 시각·계정·요청 id·
  도구·가린 첫 줄 160자·지문·요청한 기기·결정한 기기(id·이름·종류)·출처 IP. 브로커도 자기 감사 줄을 남긴다.

## 알림

- **폰**: 관문이 계정 폰들에 APNs 로 직접 보낸다(열쇠는 관문 기계의 `~/.config/kasaterm/apns/key.json` 또는
  `KASATERM_APNS_KEY_ID`·`_TEAM_ID`·`_KEY_PATH`). 알림 `apns-collapse-id` 는 요청 id — 다른 곳에서 닫히면 같은 자리를 소리·배너
  없이(`interruption-level: passive`) 「처리됨」으로 갈아 끼운다. 토큰 환경이 틀리면(개발 서명판·
  시뮬레이터) 반대쪽에 한 번 더 보내고 맞는 쪽으로 고쳐 적는다. 앱이 앞에 있으면 시스템 배너 대신 앱의 위쪽 띠가 선다.
  알림을 누르면 승인 화면 — 기기·학생·도구, 폴더, 입력 칸 전부(고정폭, 고를 수 있음), 남은 시간, [거절]·[허락].
- **다른 맥**: 오른쪽 위 결정 알림 「학생 승인 요청 · 기기 · 도구 · 첫 줄」 + [보기][나중에]. [보기]는 원문 전부를 고정폭 칸에 담은
  시트를 연다. 시트의 Return 은 **거절**이다(터미널에서 치던 Enter 가 새도 허락이 나가지 않게). 다른 곳에서 닫히면 알림·시트를
  거두고 「…에서 허락했어요」를 띄운다. 이 기기 학생의 요청은 띄우지 않는다 — 그 칸의 엔진 창과 기존 승인 알림이 이미 묻는다.

## 검증 리그

로컬 관문(`kasa-relay --port <p> --state <폴더>/relay-state.json`)에 시험 계정을 만들고, 리그 앱마다 그 관문으로 로그인한
기기 파일을 준다. 격리 실행은 사람 계정으로 떨어지지 않게 막혀 있어 기기 파일을 **직접** 줘야 원격 승인이 켜진다.

```bash
KASATERM_DEVICE_FILE=/tmp/<리그>/device.json   # {relay, account, device_id, token}
KASATERM_GATEWAY=http://127.0.0.1:<p>          # 리그도 업링크를 시험 관문으로
KASATERM_AUTOSHEET=allow|deny                  # 검증 실행에서만: 시트가 선 뒤 그 단추를 누른다
KASATERM_AUTOSHEET_MS=6000  KASATERM_AUTOSHEET_SHOT=/tmp/<리그>/sheet.png
```

mod 는 `POST /claude-mod/permission`(리그 앱의 HTTP 포트)로 흉내 낸다. 폰은 가상 아이폰에서 「고급 설정 → 계정 서버 주소」를
`http://127.0.0.1:<p>` 로 두고 로그인한다(시뮬레이터 자판이 한글이면 `xcrun simctl pbcopy` 로 붙여 넣는다). 시뮬레이터도
APNs 샌드박스 토큰을 받아 실제 푸시가 온다.

## 운영 관문에 올릴 때

관문 바이너리 교체(`tools/kasa-gateway-seoul/gateway.sh build` → `upgrade`)에 더해 **APNs 열쇠를 서울 관문에 둬야** 폰
알림이 간다. 열쇠 없이도 창구·화면은 돈다(폰이 앱을 열면 띠가 선다). 열쇠는 `/etc/kasa-relay/apns/AuthKey_<id>.p8`
(소유 `kasa-relay`, 0600), `/etc/kasa-relay/env` 에 `KASATERM_APNS_KEY_ID`·`KASATERM_APNS_TEAM_ID`·`KASATERM_APNS_KEY_PATH`.
열쇠 파일을 다른 기계로 옮기는 일이라 관문 교체와 함께 사람 승인 뒤에 한다([seoul-gateway.md](seoul-gateway.md)).
