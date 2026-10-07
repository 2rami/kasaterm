## 공개 레포다

`2rami/kasaterm` (public). 키·토큰은 커밋에도 로그에도 넣지 마라. 주석·문서·커밋 메시지에
「선생님」·「거노」 같은 개인 호칭·페르소나 표현 금지.

**워킹트리를 pane 여럿이 함께 쓴다.** 브랜치 전환·checkout 은 남의 pane 을 통째로 끌고 가고,
force push·history 재작성은 남의 커밋을 지운다. 하기 전에 물어라.

## 커밋 공동저자 — 나쵸네코를 함께 단다

모델 줄은 그대로 두고(코드를 실제로 쓴 것이 무엇인지가 기록에서 사라진다) 그 아래에 한 줄 더 단다.

```
Co-Authored-By: NachoNekoBot <322779791+NachoNekoBot@users.noreply.github.com>
```

GitHub 은 계정에 연결된 이메일만 기여자로 센다 — 모델 줄의 `noreply@anthropic.com` 은 어느
계정에도 안 걸려 목록에 안 뜬다(2026-08-30 확인). 이 주소는 이 프로젝트의 기계용 계정이다.

## 자율 테스트 우선

사용자에게 "테스트 해보세요"라고 떠넘기지 말고 **너가 직접** 실행·확인·수정 사이클을 돌려라.
스크린샷에서 어색한 데를 봤으면 "어때보여요?" 묻지 말고 네 판단으로 고쳐라.

⛔ **검증용 앱을 띄우기 전에 [`docs/verify-app.md`](docs/verify-app.md) 를 읽고 그 부팅 줄을 그대로 써라.**
격리 변수를 빼먹으면 사용자의 세션·설정·창크기를 덮고, `pkill`·`killall` 로 거두면 사용자 창의
claude 가 전부 죽는다(2026-08-15 에 pane 9개가 날아갔다). 거둘 때는 잡아 둔 PID 만 `kill`.

- 체감(스크롤·입력 지연)은 **반드시 release**. 디버그 빌드는 원래 버벅인다 — 디버그로 "느리다" 판단 금지.
- 스크린샷은 `Read` 로 직접 본다. 먼저 줄여서 연다(`sips -s format jpeg -s formatOptions 60 -Z 1200 a.png --out a.jpg`) — 이미지는 대화에 박혀 빼는 수단이 없고 한 세션에 스무 장쯤 쌓이면 32MB 벽에 걸려 그 세션은 못 살린다. 볼 것을 정한 뒤 한 장씩.
- 한글 IME 조합 버그는 자동 입력으로 재현 못 한다 — 사용자가 직접 타이핑해야 한다.

## 앱 변경 완료 기준

앱 동작에 영향을 주는 수정은 크기와 관계없이 **검사 → 커밋·main 푸시 → 빠른 업데이트 등록**까지 한다.
정확한 커밋을 `origin/main`에 올린 뒤 `python3 -m tools.release.auto enqueue <40자리 SHA>`로 등록한다.
남의 미완성 변경·미검증 커밋은 포함하지 않고, 갈라진 main을 강제 push하지 않는다. 문서만 바뀐 경우는 등록하지 않는다.
등록은 배포 완료가 아니다. controller가 검사·서명·공증·preview 피드 검증을 끝냈는지 따로 보고하며,
실패·대기는 이유와 함께 남긴다. 일반 사용자 stable 게시는 별도 승인을 유지한다.
앱을 직접 종료·재실행하지 않는다. 선택된 Mac은 preview 새 판을 알림으로 띄우고, 사람이 [업데이트]를 눌렀을 때만 받아서 설치·재실행한다.
절차·최초 설정·중단 범위는 [`docs/automatic-preview-updates.md`](docs/automatic-preview-updates.md).

폰 앱(`mobile/`)이 바뀌면 main 푸시 뒤 `mobile/tool/adhoc.sh`로 설치 링크 새 판을 올린다(자체 배포).
TestFlight는 쓰지 않는다 — 2026-10-01 개인 팀 TestFlight 설치가 애플 쪽에서 막혀 바꿨다. 남의 미커밋 변경이 굽히지
않게 origin/main 깨끗한 worktree에서 굽고, 판 번호·링크를 보고한다. 절차는 [`docs/ios-adhoc-install.md`](docs/ios-adhoc-install.md).

`bash scripts/build-app.sh`의 `dist/kasaterm.app`은 로컬 검증·최초 설치용이다. 로컬 빌드만으로 다른 기기에 배포됐다고 보고하지 않는다.

⚠️ 다른 pane 이 Rust 를 고치는 중이면 스크립트가 거부한다. 기다렸다 다시 불러라.
⚠️ claude 세션 안에서 앱을 띄우지 마라(pane 에서 `open`·relaunch) — 그 앱이 낳는 **모든 pane** 의
transcript 저장이 꺼진다.

로컬 굽기·기존 자기설치 경로 → [`docs/bake-and-install.md`](docs/bake-and-install.md)

## 디자인 정본

색·모양·치수와 그 뒤의 규칙은 [`docs/design.md`](docs/design.md) 한 곳에 있다. **화면을 만들거나
고치기 전에 읽고, 거기 있는 값을 써라.** 없는 값이 필요하면 파일 안에 `const` 를 새로 만들지 말고
그 문서에 추가한 뒤 쓴다 — 행 높이가 파일마다 22·40·44·58 로 갈린 것이 그렇게 생겼다.

눈으로 판정하기 전에 **px 로 재라.** 판독은 틀린다.

**시안도 실제 화면으로 만든다.** 카사텀 PC·모바일 화면을 HTML 페이지나 Claude 아티팩트로 설계하지
마라 — PC 는 Rust+wgpu/winit, 모바일은 Flutter 위젯을 격리 실행해 스크린샷으로 보인다. 이미 있는
별도 웹 제품은 이 규칙을 이유로 지우지 않는다. 학생 협업 규약 쪽 정본은
`skills/kasapane/collab.md` 의 「카사텀 화면 만들기」다(학생 프롬프트엔 말투만 실리고 규약은 스킬로 읽는다).

## 코드 맵

`main.rs` 는 타입 정의·자유함수·`fn main`·tests 만. **App 메서드는 기능별 모듈**에 있다
(`impl App` 확장 + `use super::*`, cross-module 호출은 `pub(crate)`). 새 메서드도 도메인 맞는 모듈에.

어느 모듈이 무엇을 맡는지 → [`docs/code-map.md`](docs/code-map.md). Rust 를 만지기 전에 읽어라.
