# 공유방

공유방은 기기 목록과 별도인 계정 간 협업 공간이다. 첫 구현은 초대·읽기 공유·독립 포크다.
초대받은 사람에게 원본 기기의 터미널 제어 권한을 주지 않는다. 보기 닫기·방 나가기는
원본 프로세스에 영향을 주지 않는다. 예전 KASA-share 폴더 동기화와 관계없다.

## 화면과 소유

- PC와 모바일에서 기기 목록과 구분한 `공유방` 섹션에 내가 만든 방과 참여한 방을 함께 둔다.
- 새 공유방에는 기존 작업방의 선택한 대화를 게시한다. 공유방을 보거나 참여할 때 셸을 만들지 않는다.
- 방 소유자만 게시·초대·초대 취소·참여자 제외·방 삭제를 한다. 참여자는 읽기와 방 나가기를 한다.
- 포크 허용은 방의 명시적 설정이다. 수신자는 자기 로그인과 자기 기기로 독립 세션을 만든다.
- 공유방의 보기는 PTY 없는 화면이다. 키·IME·붙여넣기·파일 드롭을 원본으로 보내지 않는다.
- 초대 코드는 로그인한 계정으로 수락한다. 서버에는 코드 해시와 만료만 남기고 코드 원문은 생성 응답에만 둔다.

프로토콜 DTO 정본은 `crates/kasa-socket/src/shared_rooms.rs`다.

## 관문 계약

기존 네이버 클라우드 `kasa-relay`에 `/relay/shared-rooms`를 추가한다. 인증은 기존 `AccountAccess`를 쓰며
계정은 Bearer 자격에서 정한다. 계정의 고정 키를 ACL에 쓰고 표시명·이메일을 권한 키로 쓰지 않는다.
방과 포크 자료는 `--state` 옆 `shared-rooms/`에 저장한다. 기존 relay-state와 개인 대화 보관은 유지한다.

| 요청 | 역할 |
|---|---|
| `GET /relay/shared-rooms` | 내가 소유·참여한 방 |
| `POST /relay/shared-rooms` | `{title, allow_fork, request_id?}`으로 생성, 같은 요청 재시도는 같은 방 |
| `GET /relay/shared-rooms/{room}` | 방·멤버·게시 대화 목록 |
| `PATCH /relay/shared-rooms/{room}` | 소유자의 제목·포크 허용 변경 |
| `DELETE /relay/shared-rooms/{room}` | 소유자의 공유 종료 |
| `POST /relay/shared-rooms/{room}/invites` | 만료 있는 초대 코드 발급 |
| `DELETE /relay/shared-rooms/{room}/invites/{invite}` | 초대 폐기 |
| `POST /relay/shared-room-invites/accept` | `{token}`을 현재 계정으로 수락 |
| `DELETE /relay/shared-rooms/{room}/members/{account}` | 소유자의 참여자 제외 또는 본인의 나가기 |
| `POST /relay/shared-rooms/{room}/sessions` | 게시 대화 메타 생성 |
| `DELETE /relay/shared-rooms/{room}/sessions/{session}` | 원본 작업을 유지한 채 게시 종료 |
| `POST /relay/shared-rooms/{room}/sessions/{session}/append` | 게시 기기의 대화 증분 업로드 |
| `GET /relay/shared-rooms/{room}/sessions/{session}/content` | 세대·끝 위치를 고정한 읽기 |
| `POST /relay/shared-rooms/{room}/sessions/{session}/checkpoints` | 소유자의 불변 포크 자료 게시 |
| `POST /relay/shared-rooms/{room}/sessions/{session}/fork` | 허용된 checkpoint를 수신자 소유 사본으로 고정 |
| `GET /relay/shared-forks/{fork}` | 수신자만 내려받는 포크 자료 |
| `GET /relay/shared-forks` | 수신자 소유 사본 목록, 방 삭제·탈퇴 후에도 유지 |
| `DELETE /relay/shared-forks/{fork}` | 수신자의 관문 사본 삭제 |

목록은 revision/갱신 시각을 포함하고 클라이언트가 짧게 폴링한다. 같은 세대의 본문은
offset/end로 이어 읽는다. 초기 버전에서 입력·resize를 전달하는 WebSocket은 제공하지 않는다.

메타 생성은 `{source_key,title,harness}`다. 게시 계정과 기기는 인증에서 가져오며 desktop만 게시한다.
본문 업로드는 기존 대화 보관의 `{expected_revision,generation,from,next,reset,raw,caught_up}` 형태를 따른다.
세대·revision·UTF-8 행 경계를 확인하고, 본문 fsync 뒤 manifest를 원자 교체한다. 비멤버는 404,
역할 위반은 403, 세대·revision 충돌은 409다. 읽기·쓰기 완료 직전에 자격 폐기와 최신 멤버십을 다시 검사한다.

checkpoint 게시는 `{expected_generation, expected_revision, checkpoint}`로 현재 본문 세대에 결속한다.
원본 재설정 전에 준비한 자료가 새 세대의 포크 자료로 올라가지 않게 한다.

## 포크 자료

화면용 archive는 native 재개 파일이 아니다. 포크에는 별도로 고정한 자료와 명시적인 형식을 쓴다.

```text
ForkCheckpointV1
  format_version: 1
  native: {harness, parent_session_id, boundary_id, boundary_label, native_version, jsonl, sha256}
  captured_at_ms
  display_raw
  project?: {repo_url?, commit?, captured_at_ms, patch?, files[]}
  files[]: {path, bytes_base64, sha256}
  omissions[]
```

native 자료에는 인증 파일·계정 슬롯·기기 자격·원본의 권한 우회 옵션을 넣지 않는다. 원본의 비공개
실행 지침이나 숨겨진 추론을 초대 상대에게 노출하는 형식으로 만들지 않는다. 지원하지 않는 native
형식이나 완전하게 고정하지 못한 경계는 포크 가능으로 표시하지 않는다. 표시용 archive를 native로
설치하거나 같은 세션 ID를 목적지에 덮어쓰지 않는다.

포크는 원본을 멈추지 않는다. 공유 권한을 확인하고 고정 자료를 받은 뒤, 수신자의 별도 작업 위치에서
공식 native fork로 새 ID를 만들고 그 ID만 실행한다. 원본 ID·파일·PID는 유지한다. 실행에는 수신자의
현재 로그인·권한 설정을 쓰며, 목적지에 쓰던 입력이나 작업이 생기면 준비한 실행을 취소한다.
원본과 같은 ID로 resume하는 기존 이사 경로를 포크로 재사용하지 않는다.

`POST .../fork`는 `{checkpoint_id,request_id}`를 받는다. 같은 수신자·request_id의 재시도는 같은 사본을 돌려준다.
완료된 수신자 사본은 이후 원본의 추가 대화·삭제와 독립적이다. 다른 수신자는 서로 다른 사본을 받는다.
수신자의 실제 child_session_id와 원본 방·대화·checkpoint의 관계를 로컬에 보존한다.

Claude는 정제한 파일을 `--resume <파일> --fork-session --session-id <새 ID>`로 연다.
Codex는 설치된 app-server가 실험 `thread/fork.path`를 지원하는지 확인하고, 목적지 세션 폴더 밖의
임시 기록에서 공식 fork를 만든다. 원본 ID를 목적지 세션 목록에 설치하지 않는다.
Codex의 계정에 묶인 암호화 압축 요약은 제외하고 기록에 남아 있는 압축 전후 공개 대화를 유지한다.
부모 기록을 외부에서 참조하는 형식은 지원하지 않는다. native 자료가 16MiB를 넘으면 자르지 않고 거부한다.
제외한 항목은 포크 안내에 표시한다. 대화만 받으면서 폴더를 비우면 요청별 새 작업 폴더를 만든다.
프로젝트 자료를 함께 받을 때는 목적지 저장소와 기준 commit이 필요하다.

## 목적지 PC와 모바일

자기 폰은 기존 소유자 인증으로 `GET /shared-rooms/status`, `GET /shared-rooms/sources`,
`POST /shared-rooms/publish`, `POST /shared-rooms/fork`를 호출한다. 타인의 원격 연결에는 허용하지 않는다.
`GET /shared-rooms/forks/{operation}`은 게시·포크 작업의 `preparing`, `starting`, `ready`,
`needs_input`, `failed` 상태를 반환한다. POST 성공에는 `ok:true`가 있고, 미지원 PC는 status에서
`available:false`를 돌려준다. 폰은 자기 기기에서 반환된 주소인지 확인한 뒤 연다.
연결된 PC가 없어도 관문에 로그인해 공유 대화를 읽을 수 있다. 새 native 세션을 열 때는 자기 PC가 필요하다.

프로젝트 자료는 대화와 별도다. commit과 파일 캡처 시각을 표시하고, 과거 대화 시점에 없던 파일을
그때의 파일처럼 표시하지 않는다. 기존 작업 트리를 덮지 않고 새 디렉터리 또는 별도 worktree에서
검증한다. 외부 경로·symlink·절대 경로·`.git`·자격 파일은 파일 묶음으로 설치하지 않는다.

## 검증

가짜 두 계정과 격리한 두 앱으로 초대·읽기·멤버 제거·공유 종료·포크를 검증한다. 로그인 이름 변경,
초대 만료/재사용, 진행 중 업로드의 권한 폐기, 재시작, 손상 저장소, 세대 충돌을 포함한다.
공유 보기의 모든 입력과 닫기가 원본 PID·대화·파일을 바꾸지 않아야 한다. 포크는 정확히 하나의 새 ID를
만들며 재시도에 중복 생성하지 않는다. source에 이후 글을 추가해도 고정 자료와 child는 바뀌지 않는다.
UI는 작은 창·모바일에서 실제 캡처하며, 사용자 계정과 세션을 검증용으로 공유하지 않는다.
