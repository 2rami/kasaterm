# 굽기와 설치 — 거노 앱에 반영하기

프로젝트 지침(`CLAUDE.md`)의 「거노 앱에 반영하기」가 여기를 가리킨다.

## 거노 앱에 반영하기 — 굽고, 껐다 켜면 끝

거노가 쓰는 건 `~/Applications/kasaterm.app` 이고, 그건 `dist/kasaterm.app` 의 **복사본**이다. `cargo build` 도 `build-app.sh` 도 그 복사를 하지 않으니, **빌드했다고 반영된 게 아니다**(설치본 mtime 을 확인하면 바로 보인다).

너는 여기까지만 한다:

```bash
bash scripts/build-app.sh      # dist/kasaterm.app 을 새로 굽는다
```

⚠️ **다른 pane 이 이 레포의 Rust 를 고치는 중이면 이 스크립트는 거부한다** — 굽기는 워킹트리를 통째로 담으므로 남의 반쯤 만든 기능이 함께 들어가고, 운이 나쁘면 컴파일조차 안 된다(2026-08-11 지시). 누가 무엇을 만지는지 이름과 파일이 찍히니 **기다렸다가 다시 부르면 된다.** `--force` 는 그걸 알고도 강행할 때만.

그리고 **네 커밋을 반영하려고 급히 구울 필요가 없다.** 워킹트리는 공유라 나중에 누가 굽든 네 변경이 함께 실린다. 굽기는 "이제 다 됐으니 화면으로 확인하자"는 시점에 한 번이면 충분하다.

### 다른 기기(맥미니)에서 작업 중이어도 굽는다 — 「맥북에서 구워 주세요」로 넘기지 마라

이사로 미니에 와 있든 처음부터 미니에서 시작했든, 굽기는 네 몫이다. 맥북에는 나쵸 역터널의 관문
(`kasaterm-remote`)이 있고, 그 `bake` 동사가 맥북 레포의 `scripts/remote-bake.sh` 를 돌린다(2026-09-17).

```bash
scripts/macbook-bake.sh status    # 맥북 판: HEAD·미커밋·dist/설치본 시각·펫·서비스
scripts/macbook-bake.sh pet       # pull → 펫만 갈아 끼우고 다시 띄움 — 즉시 반영
scripts/macbook-bake.sh journal   # pull → request-journal(펫의 뇌) 재시작 — 즉시 반영
scripts/macbook-bake.sh app       # pull → build-app.sh (다른 pane 이 Rust 를 만지면 거부한다, --force 로 강행)
```

- **먼저 push 해라.** 맥북은 `git pull --ff-only origin main` 으로 받으므로 안 올린 커밋은 안 구워진다.
- **앱 껐다 켜기(자기설치)는 네가 하지 않는다** — 거노나 나쵸가 한다. `app` 을 구웠으면 「구웠다, 껐다 켜면 반영」까지만 보고한다.
- 셋 중 어느 것이 필요한지는 고친 자리로 정한다: 펫 그림·말풍선·메뉴 → `pet`, 펫의 뇌·나쵸 말투·집컴 전원(`tools/request_journal`) → `journal`, 앱 본체 → `app`.
- 맥북에 있을 때의 펫 전용 길은 `scripts/pet-reload.sh` 다(앱 굽기·앱 재시작 없이 펫만).

그 다음은 **거노가 앱을 껐다 켜면 끝난다.** 종료 시 `arm_self_install`(main.rs)이 도우미를 남겨, 프로세스가 완전히 사라진 뒤 `dist` 를 설치본 자리에 복사한다. 그래서 다음에 켜는 것이 새 바이너리다. 다시 띄워 주지는 않는다 — 끄려고 끈 것일 수도 있어서다. 결과는 `$TMPDIR/kasaterm-selfinstall.log`.

- **`scripts/relaunch.sh` 는 이제 선택**이다(quit→설치→재실행→inode 검증까지 한 번에 하고 싶을 때). ⚠️ **pane 안에서 돌리지 마라** — 앱을 quit 하는 순간 네 PTY 째 죽는다. 거노가 `! scripts/relaunch.sh --no-build` 로 돌린다.
- 자기 설치는 **그 설치본으로 도는 앱**에서만, **빌드 트리의 번들이 더 새로울 때만** 움직인다. `cargo run` 개발 실행과 배포된 남의 머신에서는 아무 일도 안 한다.
- ⚠️ **앱을 claude 세션 안에서 띄우지 마라**(pane 에서 `open`·relaunch). 그 앱이 claude 의 `CLAUDE_CODE_CHILD_SESSION`·`TEAMMATE_MODE`·`SESSION_ID` 를 물려받고, 그러면 그 앱이 낳는 **모든 pane** 의 claude 가 transcript 저장을 끈다. `scrub_inherited_claude_markers`(main.rs, 부팅 첫 줄)가 이제 그걸 지우지만, 애초에 안 물리는 게 낫다.

