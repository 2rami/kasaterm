# 계정에 구글·깃허브 일 권한 붙이기

관문 계정 로그인은 구글·깃허브로 **신원만** 확인한다(`openid email`, `read:user` —
[account-oauth.md](account-oauth.md)). 이 문서는 같은 계정에 메일·PR 같은 **일 권한**을
따로 붙여, 어느 기기·어느 에이전트든 계정 이름으로 그 일을 하게 하는 설계다.

지금 나쵸는 맥미니에서 `gws`·`gh` CLI 로 한 사람 계정만 쓴다. 그 자격증명은 미니 한 대에
묶여 있고, 누가 무엇을 보냈는지 남는 곳이 없고, 다른 기기·폰은 같은 일을 못 한다.

## 결정 요약

| 갈림길 | 고른 것 | 근거 절 |
|---|---|---|
| 토큰 보관 | A. 관문 보관 + 관문이 대신 부름 | [토큰을 어디 두나](#토큰을-어디-두나) |
| 구글 메일 범위 제약 | 게시·미검증(경고 한 번, 평생 100명, 만료 없음) | [구글 메일 범위 제약](#구글-메일-범위-제약) |
| 깃허브 앱 종류 | GitHub App 새로 | [깃허브 OAuth 앱 대 GitHub App](#깃허브-oauth-앱-대-github-app) |
| 쓰기 확인선 | 에이전트가 낸 쓰기는 전부 앱 화면 승인 | [쓰기는 사람 확인을 거친다](#쓰기는-사람-확인을-거친다) |

## 누가 그 권한을 쓰나

| 쓰는 쪽 | 어디서 | 관문에 닿는 자격 |
|---|---|---|
| 나쵸 | 맥미니 상주 프로세스 | 미니의 기기 자격증명(`kasa-device login`) |
| 나쵸 앱 | 폰·맥(nachochat) | 그 앱의 기기 자격증명(`nachochat://oauth` 로 받은 것) |
| 학생 | 각 기기 pane 의 CLI | 그 기기의 `device.json` |
| 사람 | PC 설정 → 계정, 폰 설정 | 그 기기의 자격증명 + 앱 화면에서만 나는 승인 |

네 쪽 모두 **기기 자격증명 하나로 관문에 묻는 한 길**만 쓴다. 쓰는 쪽마다 따로 구글·깃허브
로그인을 들고 다니지 않는다. 계정은 요청 본문이 아니라 bearer 가 정한다(`gateway_workspace.rs`
와 같은 규칙) — 본문에 다른 계정 이름을 적어도 소용없다.

## 토큰을 어디 두나

| 안 | 모습 | 얻는 것 | 잃는 것 |
|---|---|---|---|
| **A. 관문 보관 + 관문이 대신 부름** | 갱신 토큰은 관문 봉인함에만. 기기는 「메일 목록」「PR 만들기」 같은 **동작**을 관문에 요청하고, 관문이 공급자를 부른다 | 토큰이 기기로 안 나간다. 쓰기 확인·감사 기록을 관문 한 곳에서 강제. 나쵸·폰·CLI 가 같은 길 | 관문(미니)이 모든 토큰을 쥔다. 관문이 꺼지면 일도 멈춘다. 공급자 API 를 동작 단위로 하나씩 감싸야 한다 |
| B. 관문 보관 + 짧은 토큰을 기기에 내줌 | 갱신 토큰은 관문, 1시간짜리 access token 을 기기에 준다 | `gh`·`gws` 를 거의 그대로 쓴다 | 받은 토큰으로 무엇이든 보낼 수 있어 쓰기 확인을 못 강제한다. 감사 기록도 비는 구간이 생긴다 |
| C. 각 기기 키체인 | 기기마다 따로 연결 | 관문이 토큰을 안 쥔다 | 기기마다 동의를 다시 받고, 나쵸·폰은 따로 연결. 「계정 하나로 어디서든」이 아니다 |

### A 일 때의 보관·갱신·폐기·감사

- **보관**: 관문 상태 폴더 옆 `connections/` 에 계정별 봉인 파일(`<계정>.sealed`). 봉인은
  개인비서와 같은 `sealed.rs`(AES-256-GCM, 폴더 0700, `master.key` 0600, AAD 에 저장소 이름과 계정 이름)
  이지만 **폴더와 열쇠는 따로** 둔다 — 한쪽 열쇠가 새도 다른 쪽은 안 열린다.
  봉인 안에는 공급자·공급자 쪽 신원(sub/id)·표시 이름(이메일/로그인)·받은 범위·갱신 토큰·
  access token 과 만료 시각만. 메일 본문·PR 내용은 쓰기 대기 동안만 넣고 끝나면 지운다.
- **갱신**: 구글 access token 은 1시간. 만료 60초 전부터 갱신 토큰으로 새로 받는다.
  GitHub App 사용자 토큰은 8시간·갱신 토큰 6개월이고 **갱신할 때마다 갱신 토큰이 바뀐다** —
  새 것을 봉인에 먼저 쓰고 나서 쓴다. 갱신이 `invalid_grant` 면 연결을 「다시 연결 필요」로
  표시하고 토큰을 지운다(재시도로 잠긴 계정을 두드리지 않는다).
- **폐기**: 사람이 「연결 끊기」를 누르면 공급자에 먼저 철회를 보낸다(구글 `oauth2.googleapis.com/revoke`,
  깃허브 `DELETE /applications/{client_id}/grant`). 철회가 실패해도 관문 봉인은 지운다 —
  화면에는 「공급자 쪽 철회 확인 못 함, 구글·깃허브 보안 설정에서 지울 수 있음」을 남긴다.
  관문 계정이 비활성화되면 그 계정의 연결 동작은 바로 거절한다(봉인은 남긴다, 다시 켜면 산다).
- **감사**: `connections/audit.jsonl` 에 동작마다 한 줄 — 시각·계정·기기 id·쓰는 쪽(사람/에이전트)·
  동작(`mail.list`·`mail.read`·`mail.send`·`pr.create`·`connect`·`disconnect`·`approve`·`reject`)·
  대상 요약(메일 id, 받는 사람 **수**, 레포·브랜치)·결과. 본문·제목·주소·토큰은 안 적는다.
  2 MB 를 넘으면 `audit.jsonl.1` 로 한 번 돌린다. `GET /relay/connections/audit` 으로 계정 것만 본다.

## 범위를 필요할 때 더 받는다

로그인은 지금처럼 신원 범위만 받는다. 일 권한은 **기능을 켤 때** 따로 동의받는다.

| 기능 | 구글 범위 | 깃허브 |
|---|---|---|
| `mail.read` | `gmail.readonly` | — |
| `mail.send` | `gmail.send` | — |
| `github.pr` | — | (OAuth 앱) `repo` / (GitHub App) Pull requests·Contents 쓰기 |

- 구글은 `include_granted_scopes=true`·`access_type=offline`·`prompt=consent` 로 부른다.
  `openid email` 을 같이 받아 어느 메일 계정이 연결됐는지 화면에 보인다.
- 구글은 사용자가 동의 화면에서 범위 일부만 체크할 수 있다. 토큰 응답의 `scope` 를 보고
  **받은 것만** 켠다(읽기만 받았으면 보내기는 꺼진 채 「보내기 권한 없음」).
- 메일 연결은 로그인 신원과 같은 구글 계정일 필요가 없다(개인 계정으로 로그인하고 회사 메일을
  연결할 수 있다). 연결은 `(공급자, 공급자 쪽 신원)` 으로 구분하고 계정당 여럿 둘 수 있다.
  연결은 로그인 수단이 되지 않는다 — 로그인 연결과 일 연결은 서로 다른 장부다.
- 흐름은 기존 `/relay/oauth/start` 에 `connect: ["mail.read", …]` 만 더한다. 기기 자격증명(`link:true`)과
  PKCE 앱 리다이렉트가 둘 다 있어야 시작된다 — 공급자 토큰은 그 요청을 낸 기기가 verifier 로 받아 갈 때만
  그 계정 봉인함에 들어간다(남에게 넘긴 링크로는 남의 계정에 못 붙는다). 연결은 로그인 신원으로 연결되지 않는다.

## 구글 메일 범위 제약

[Gmail 범위 분류](https://developers.google.com/workspace/gmail/api/auth/scopes): `gmail.send` 는
**민감**, `gmail.readonly`·`gmail.compose`·`gmail.modify`·`gmail.metadata` 는 **제한** 범위다.
제한 범위를 서버로 받아 쓰는 앱이 공개 검증을 받으려면 연 1회 제3자 보안 평가(CASA)까지 가야 한다.
그래서 쓰는 범위는 위 표의 둘(`readonly`·`send`)로 좁힌다 — 초안·라벨·삭제는 안 한다.

[앱 공개 상태별 동작](https://developers.google.com/identity/protocols/oauth2/production-readiness/overview):

| 상태 | 누가 | 메일 연결 시 | 연결 수명 |
|---|---|---|---|
| 테스트(외부) | 등록한 테스트 사용자 100명까지 | 「테스트 중인 앱」 경고 | **동의 7일 뒤 만료**(갱신 토큰도) — 매주 다시 연결 |
| 게시·미검증(외부) | 누구나, 단 이 범위를 받는 사용자는 **프로젝트 평생 100명** | 「확인되지 않은 앱」 경고 | 만료 없음 |
| 내부(Workspace 조직 소유 프로젝트) | 그 조직 사람만 | 경고 없음 | 만료 없음 |
| 조직 관리자가 「신뢰함」 표시 | 그 조직 사람 | 경고 없음 | 7일·100명 제한이 그 조직엔 안 걸림 |
| 검증 완료 | 누구나 | 앱 이름 표시 | 만료 없음. 수개월·연 1회 보안 평가 |

로그인은 신원 범위만 쓰므로 어느 상태든 지금처럼 누구나 된다. 상태는 메일 연결에만 걸린다.
Workspace 조직은 관리자가 미검증 앱의 민감·제한 범위를 막아 둘 수 있다 — 회사 메일 연결이
「관리자가 차단」으로 끝나면 그 조직 관리자에게 이 OAuth 클라이언트를 신뢰 목록에 올려 달라고 해야 한다.

## 깃허브 OAuth 앱 대 GitHub App

| | OAuth 앱 `repo` 범위 | GitHub App |
|---|---|---|
| 권한 폭 | 사용자가 닿는 **모든** 비공개 레포 읽기·쓰기 | 설치한 레포만, Pull requests·Contents 처럼 고른 권한만 |
| 토큰 | 만료 없음(1년 안 쓰면 철회) | 8시간 + 갱신 토큰 6개월 |
| 준비 | 지금 로그인용 앱에 범위만 더함 | 새 앱 만들기 + 계정·조직에 설치 |
| 조직 레포 | 조직이 OAuth 앱 제한을 켰으면 조직 주인 승인 | 조직 주인이 설치(또는 설치 요청 승인) |
| PR 작성자 | 사용자 | 사용자(앱 표시가 함께 붙음) |

PR 만들기는 **이미 올라간 브랜치**로 PR 을 여는 것까지다. 브랜치 푸시는 지금처럼 각 기기 git 이 한다.

## 쓰기는 사람 확인을 거친다

메일 보내기·PR 만들기는 밖으로 나가는 일이라 되돌리기 어렵다. 확인선은 다음과 같다.

- 에이전트(나쵸·학생 CLI·나쵸 앱의 모델 턴)가 낸 쓰기는 관문에 **대기 쓰기**로 쌓인다.
  내용 전체(받는 사람·제목·본문 / 레포·base·head·제목·본문)를 봉인해 두고 24시간 뒤 버린다.
- 계정 기기(PC·폰)에 알림이 가고, 사람이 **내용을 그대로 본 화면에서** [보내기]/[만들기] 또는
  [버리기]를 누른다. 관문은 그 승인을 받은 뒤에만 공급자를 부른다.
- 승인은 앱 화면에서만 난다. 앱(PC·폰)은 프로세스 메모리에만 있는 승인 열쇠를 처음 승인할 때 관문에
  등록하고(`POST /relay/connections/approver`), 승인 요청 머리 `x-kasa-approver` 에 싣는다. 관문은 기기마다
  열쇠 해시 하나를 메모리에만 둔다 — 관문이 재시작하면 앱이 다시 등록한다. 승인 함수는 소켓
  (`relay.account`)·`kasa-device` 어디서도 부를 수 없게 화면 코드에서만 부른다. 그래서 같은 기기의 CLI 도
  정해진 길로는 승인하지 못한다. 같은 사용자 권한의 프로세스가 작정하고 등록 길을 직접 부르는 것까지는
  막지 못한다 — 막는 것은 에이전트가 쓰기를 내고 스스로 승인까지 해 버리는 사고다.
- 승인은 화면이 보여 준 내용의 해시(`digest`)에 묶인다. 열어 본 뒤 내용이 바뀌었으면 `content_changed` 로
  거절한다. 한 번 승인한 쓰기는 대기에서 빠지고 두 번 실행되지 않는다. 화면은 받는 사람 전부·제목·본문
  전부를 보여 주며, 본문이 40줄을 넘는 쓰기는 승인 단추를 세우지 않는다.
- 실행이 공급자에 닿기 전에 막히면(토큰 갱신 실패·앱 미설치·공급자의 거절) 대기로 되돌려 다시 승인할 수
  있다. 요청이 나간 뒤 답을 못 받으면 `result_unknown` — 다시 보내지 않는다.
- 읽기(`mail.list`·`mail.read`)는 확인 없이 되고 감사 기록에만 남는다.

## 관문 API

모두 기기 bearer 가 필요하고(계정은 bearer 로만 정해진다) 응답은 `no-store` 다.

| 길 | 하는 일 |
|---|---|
| `GET /relay/oauth/providers` | `connect: {google, github}` — 이 관문에서 연결할 수 있는 것 |
| `POST /relay/oauth/start` + `link:true`, `connect:[기능]`, PKCE 리다이렉트 | 연결 시작. 끝은 `token` 이 `{"status":"connected","connection":…}` 를 준다(기기 자격증명은 안 준다) |
| `GET /relay/connections` | 연결 목록(공급자·표시 이름·기능·`state`)·승인 대기 전체·`available`·`github_install_url`. 토큰 없음 |
| `DELETE /relay/connections/{id}` | 끊기(공급자 철회 + 봉인 삭제 + 그 연결의 대기 버림). `provider_revoked` 로 철회 확인 여부 |
| `POST /relay/connections/mail/list` | `{connection?, query?, max?(1~25)}` → 보낸 이·받는 이·제목·날짜·미리보기·안 읽음 |
| `POST /relay/connections/mail/read` | `{connection?, id}` → 머리·본문 글(평문 우선, 없으면 HTML 을 글로)·첨부 이름 |
| `POST /relay/connections/mail/send` | `{connection?, to[], cc[], subject, body, reply_to?}` → 대기 쓰기 |
| `POST /relay/connections/pr/create` | `{connection?, repo, base, head, title, body, draft}` → 대기 쓰기 |
| `POST /relay/connections/approver` | `{key}` — 이 기기 앱 화면의 승인 열쇠 등록 |
| `POST /relay/connections/pending/{id}/approve` | 머리 `x-kasa-approver`, `{digest}` → 실행 결과 |
| `POST /relay/connections/pending/{id}/reject` | 버리기 |
| `GET /relay/connections/audit?limit=` | 이 계정의 최근 감사 기록 |

연결이 여럿이면(회사·개인 Gmail) `connection` 을 안 주면 `connection_required` 로 거절한다. 오류 코드:
`feature_missing`(그 권한 없음)·`reconnect_required`(갱신 토큰이 죽음)·`repo_not_accessible`(앱이 그 레포에 설치 안 됨)·
`provider_rejected`+`detail`(공급자의 이유)·`approver_required`·`content_changed`·`result_unknown`.

## 화면과 CLI

- PC 설정 → 계정 「일 권한」: 연결 줄(Gmail·GitHub, 권한, 「다시 연결 필요」)과 [끊기]→[정말 끊기],
  [Gmail 연결]·[GitHub 연결]·[GitHub 앱 설치], 「승인을 기다리는 일」 목록 → [보기] 로 펼쳐 내용 전부 →
  [보내기]/[PR 만들기]·[버리기]. 앱은 로그인돼 있으면 1분마다 목록을 받고 새 대기가 오면 데스크톱 알림을 띄운다
  (앱을 켤 때 이미 있던 대기는 알리지 않는다).
- 학생: `kasaterm-cli mail [list] [--query …] [--max N]`, `mail read <id>`,
  `mail send --to a@x,b@y [--cc …] --subject … --body 본문|- [--reply-to <id>]`,
  `pr create --repo 주인/레포 --head 브랜치 [--base main] --title … [--body 본문|-] [--draft]`, `mail connections`.
  모두 `--connection <id>` 로 연결을 고른다. 쓰기는 대기 id 를 돌려주고 끝난다.
- 앱 없는 기기·나쵸: `kasa-device work <동작> '<JSON>'`(동작 `connections`·`mail_list`·`mail_read`·`mail_send`·
  `pr_create`·`reject`·`connections_audit`). 나쵸는 미니의 기기 자격증명(`kasa-device login 2rami`)으로 같은 길을 쓴다.
- PR 은 이미 올라간 브랜치로 연다. 브랜치 푸시는 지금처럼 각 기기 git 이 한다.

## 배포와 처음 한 번 할 일

관문 환경(launchd plist `EnvironmentVariables`, 바꾸면 `bootout`→`bootstrap`):

- `KASA_GITHUB_APP_CLIENT_ID`·`KASA_GITHUB_APP_CLIENT_SECRET` — GitHub App 의 OAuth 자격. 없으면 GitHub 연결 단추가 꺼진다.
- `KASA_GITHUB_APP_SLUG` — 앱 주소 이름. 「GitHub 앱 설치」가 `https://github.com/apps/<slug>/installations/new` 를 연다.
- Gmail 은 로그인용 Google 클라이언트를 그대로 쓴다(새 환경 없음).

Google Cloud(로그인 OAuth 클라이언트가 있는 프로젝트):

1. API 라이브러리에서 **Gmail API** 사용 설정.
2. 데이터 액세스에 `gmail.readonly`·`gmail.send` 범위 추가.
3. 대상(Audience)에서 **앱 게시**(게시·미검증). 메일 연결 때 「확인되지 않은 앱」 경고가 한 번 뜬다 —
   「고급 → 이동」으로 넘어간다. 평생 100명 제한은 메일을 연결한 사람만 센다.

GitHub App(설정 → Developer settings → GitHub Apps → New):

1. Callback URL `https://kasaterm.debimarlene.com/relay/oauth/github/callback`, 「Expire user authorization tokens」 켬,
   「Request user authorization (OAuth) during installation」 끔, Webhook 끔.
2. 권한: Repository → **Pull requests: Read and write**, **Contents: Read-only**(메타데이터는 자동).
3. 만든 뒤 Client ID·새 Client secret 을 위 환경에, 앱 이름(slug)을 `KASA_GITHUB_APP_SLUG` 에.
4. PR 을 올릴 계정·조직의 레포에 설치한다. 조직 레포는 조직 주인이 승인해야 한다.

교체 전 확인: `/relay/connections` 무토큰 401, `/relay/oauth/providers` 의 `connect`, 기존 로그인·연결 흐름 회귀 없음.
