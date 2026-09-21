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
- 스크린샷 판정은 `askimg <png> "질문"` 으로. `Read` 로 열면 대화에 박혀 빼는 수단이 없다.
- 한글 IME 조합 버그는 자동 입력으로 재현 못 한다 — 사용자가 직접 타이핑해야 한다.

## 거노 앱에 반영하기

`bash scripts/build-app.sh` 로 `dist/kasaterm.app` 을 굽는 데까지가 네 몫이고, **거노가 앱을 껐다
켜면 반영된다**(종료 시 자기설치). 다시 띄워 주지 않으니 「구웠다, 껐다 켜면 반영」까지만 보고해라.
맥미니에서 작업 중이어도 네가 굽는다 — `scripts/macbook-bake.sh app|pet|journal`(먼저 push 해야 받는다).

⚠️ 다른 pane 이 Rust 를 고치는 중이면 스크립트가 거부한다. 기다렸다 다시 불러라.
⚠️ claude 세션 안에서 앱을 띄우지 마라(pane 에서 `open`·relaunch) — 그 앱이 낳는 **모든 pane** 의
transcript 저장이 꺼진다.

상세(무엇을 언제 굽나·자기설치·`relaunch.sh`) → [`docs/bake-and-install.md`](docs/bake-and-install.md)

## 코드 맵

`main.rs` 는 타입 정의·자유함수·`fn main`·tests 만. **App 메서드는 기능별 모듈**에 있다
(`impl App` 확장 + `use super::*`, cross-module 호출은 `pub(crate)`). 새 메서드도 도메인 맞는 모듈에.

어느 모듈이 무엇을 맡는지 → [`docs/code-map.md`](docs/code-map.md). Rust 를 만지기 전에 읽어라.
