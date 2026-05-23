---
name: kasapane
description: kasaterm/kasaspace 터미널의 pane을 자유자재로 제어한다 — 새 pane 분할, 백그라운드 로그/모니터 pane 띄우기(dev server·build·test watch 등을 별도 화면에서 돌려 사용자와 같이 보기), pane 이름·색상 지정, 위치 교환, 닫기, 팀원 pane 정리. 사용자가 "모니터 띄워줘", "로그 따로 보게 해줘", "dev 서버 옆에 띄워", "pane 쪼개/닫아/이름 바꿔/색 칠해", "팀원 pane 정리해" 같은 요청을 하거나, 긴 백그라운드 작업을 별도 화면에서 돌려야 할 때 사용.
version: 0.1.0
user-invocable: true
argument-hint: "[모니터할 명령 또는 pane 작업, 예: npm run dev]"
---

# kasapane — kasaterm pane 제어

kasaterm(=kasaspace) 본체는 `cmux-compat` 셸 CLI로 pane을 제어한다. 이 스킬은
claude가 그 명령들을 **언제·어떻게 조합하는지**를 담은 지침서다. Rust 코드를
건드리지 않고, 셸 명령만으로 pane을 만들고 꾸미고 모니터 화면을 띄운다.

## 전제: kasaterm 셸 안에서만 동작

`cmux-compat`는 `$KASATERM_SOCKET_PATH`(없으면 `$CMUX_SOCKET_PATH`)로 어느
kasaterm 인스턴스에 붙을지 정한다. 본체가 띄운 셸에는 이 env가 자동으로
export돼 있으므로(`pty-backend/src/state.rs`), **kasaterm 안에서 도는 claude는
별도 설정 없이** 명령이 통한다.

확인: 명령 전에 한 번 점검한다.

```bash
echo "$KASATERM_SOCKET_PATH"          # 비어 있으면 kasaterm 셸이 아님 → 중단
cmux-compat list surfaces             # 현재 pane 목록(JSON)이 나오면 정상
```

env가 비어 있거나 `list`가 실패하면 kasaterm 밖이라는 뜻이니, pane 조작을
시도하지 말고 사용자에게 "kasaterm 안에서 실행해야 한다"고 알린다.

## 명령 레퍼런스

모든 명령은 JSON으로 응답한다(`{"ok":true,...}`). pane id는 `%0`, `%1` … 형식.

| 명령 | 동작 |
|---|---|
| `cmux-compat list surfaces` | 현재 pane 목록 + id |
| `cmux-compat split <left\|right\|up\|down>` | 현재 pane을 해당 방향으로 분할. 새 pane id 반환 |
| `cmux-compat focus <id>` | 포커스 이동 |
| `cmux-compat close <id>` | pane 닫기(셸 종료) |
| `cmux-compat rename <id> <제목>` | 헤더 제목 설정 |
| `cmux-compat color <id> <#rrggbb>` | 헤더 accent 색상 |
| `cmux-compat swap <a> <b>` | 두 pane 위치 교환(내용 유지, 자리만 맞바꿈) |
| `cmux-compat send --surface <id> <텍스트>` | 특정 pane에 텍스트 입력 |
| `cmux-compat key <id> …` | 특정 pane에 키 전송 |

`split`이 반환하는 JSON의 `result.surface.id`가 새 pane id다. 이어지는
rename/color/send는 그 id를 쓴다.

```bash
NEW=$(cmux-compat split right | python3 -c 'import sys,json;print(json.load(sys.stdin)["result"]["surface"]["id"])')
echo "$NEW"   # 예: %1
```

## 패턴 A — 모니터 pane (백그라운드 로그)

이 스킬의 핵심 용도. 오래 도는 명령(dev server·build·test watch)을 **별도
pane에서 돌려 로그를 흐르게** 하고, 메인 pane은 자유롭게 둔다. 사용자와 claude가
같은 화면을 본다.

```bash
# 1) 오른쪽에 새 pane
NEW=$(cmux-compat split right | python3 -c 'import sys,json;print(json.load(sys.stdin)["result"]["surface"]["id"])')
# 2) 한눈에 알아보게 이름 + 색
cmux-compat rename "$NEW" "dev log"
cmux-compat color  "$NEW" "#58a6ff"
# 3) 그 pane에서 명령 실행 (반드시 끝에 개행 \n — 엔터)
cmux-compat send --surface "$NEW" $'npm run dev\n'
```

원칙:
- **백그라운드 작업은 항상 새 pane에서.** `&`로 메인 셸에 묻지 말고, 모니터
  pane을 띄워 로그가 보이게 한다. 사용자가 진행 상황을 직접 본다.
- 명령 텍스트는 `$'...\n'`(ANSI-C 따옴표)로 보내 **개행이 엔터로 전달**되게
  한다. `"...\n"`이나 `$(printf ...)`는 trailing 개행이 먹혀 엔터가 안 가는
  경우가 있다.
- 색은 용도별로 일관되게: 로그=파랑 `#58a6ff`, 빌드=주황 `#d29922`,
  테스트=초록 `#3fb950`, 위험/에러=빨강 `#f85149`.

## 패턴 B — 레이아웃 구성

사용자가 "왼쪽 코드, 오른쪽 위아래로 로그랑 테스트" 같은 배치를 요청하면
split 방향을 조합한다. 분할은 **현재 포커스된 pane**을 기준으로 일어나므로,
다음 분할 전에 `focus`로 기준 pane을 옮긴다.

```bash
RIGHT=$(cmux-compat split right | ...id...)   # 오른쪽 생성
cmux-compat focus "$RIGHT"                     # 그 pane으로 이동
cmux-compat split down                         # 오른쪽을 다시 위/아래로
```

자리를 잘못 잡았으면 `swap`으로 교환(내용은 유지된다).

## 패턴 C — 팀원 pane 정리

claude 팀원은 `TeamCreate`/`Agent`(teammateMode=tmux)로 만들면 자동으로 새
pane에 뜬다. 이 스킬은 그렇게 생긴 **팀원 pane을 알아보기 쉽게 꾸미는** 역할:

```bash
cmux-compat list surfaces                       # 새로 생긴 팀원 pane id 확인
cmux-compat rename "%2" "scout"                  # 역할 이름
cmux-compat color  "%2" "#a371f7"                # 팀원별 색 구분
```

역할별 색 컨벤션 예: 탐색=보라 `#a371f7`, 구현=파랑 `#58a6ff`, 검증=초록
`#3fb950`, 리드=노랑 `#d29922`.

## 주의

- **`close`는 셸을 종료**시킨다. 사용자가 그 pane에서 작업 중일 수 있으니,
  명시적 요청이나 본인이 방금 띄운 모니터 pane이 아니면 함부로 닫지 않는다.
- pane id는 split/close에 따라 바뀐다. 연속 작업 전에 `list surfaces`로
  현재 id를 다시 확인한다.
- 셸 밖(일반 터미널, CI 등)에선 socket을 못 찾는다. 전제 점검을 먼저 한다.
