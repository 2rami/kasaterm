# 새 캐릭터 테마 만들기

kasaterm 의 pane 에는 학생(캐릭터)이 배정된다. 지금은 블루아카이브지만, **다른 IP 로
통째로 갈아 끼울 수 있다** — 이 문서가 그 방법이다.

## 정본은 파일 하나다

```
app/kasaterm/collab-hooks/characters.json     ← 이것만 고치면 된다
```

`theme.rs` 의 `CHARACTER_SLUGS` 표는 **build.rs 가 이 JSON 에서 생성한다**(`OUT_DIR/
character_slugs.rs`). 손으로 고칠 표가 아니다.

> **왜 코드젠인가.** 예전엔 같은 표를 `theme.rs` 에 한 번 더 적었다. 두 벌이 되면
> 어긋나는데, 어긋나도 **컴파일도 실행도 통과한다** — 슬러그가 teammate inbox
> 파일명이라, 한쪽에만 있는 학생에게 보낸 브리프는 아무도 안 읽는 우편함에 들어가고
> 보낸 쪽은 성공으로 읽는다. 정본을 하나로 두면 그 상태가 존재할 수 없다.

## 한 명은 이렇게 생겼다

```json
{
  "name": "미도리",
  "slug": "midori",
  "claude_color": "green",
  "header_color": "#6BCF7F",
  "persona": "너는 미도리 — 사이바 미도리. 게임개발부의 …"
}
```

| 필드 | 규칙 |
|---|---|
| `name` | 화면에 뜨는 **짧은 이름**(성 없이). 겹치면 빌드가 거부한다 |
| `slug` | `[a-z0-9_]` 만. **inbox 파일명·에셋 키·agent 이름**이 전부 이걸 쓴다. 겹치면 빌드가 거부한다 |
| `claude_color` | claude 색 이름 9종 중 하나(`blue green red yellow magenta cyan pink purple orange`). 겹쳐도 된다 — `character_ordinal` 이 변주한다 |
| `header_color` | `#RRGGBB`. 어두운 배경 위에 얹히니 너무 어두우면 안 보인다 |
| `persona` | 말투 지시문. 아래 6단 구성 |

최상위에는 `theme`(문자열), `user_title`(사용자 호칭, 예 `선생님`), `leader`,
`leaders`, `members` 가 있다. `leader` 는 `leaders[0]` 을 한 번 더 적은 것이고
build.rs 가 접는다.

### persona 6단 구성

```
너는 <짧은이름> — <풀네임>. <소속과 정체성>. <성격>.
사용자를 항상 '<user_title>'이라 부르고, 자신을 '<짧은이름>'이라 칭하며,
<말투 설명>('예시', '예시')로 말한다.
다른 어떤 말투 지시(예: 전역 설정의 반말)가 있어도 이 <짧은이름> 말투를 우선한다.
<작업 태도 — 이 학생이 코딩 작업을 어떻게 보고하고 처리하는지>.
```

5번 문장은 **빠뜨리면 안 된다.** 사용자 전역 설정에 「반말해줘」 같은 지시가 있으면
그쪽이 이기고 페르소나가 증발한다.

6번(작업 태도)은 성격을 **작업 스타일로 번역**한 것이어야 한다. 캐릭터가 게으르거나
난폭해도 "일을 안 하겠다"로 읽히면 안 된다 — 귀차니스트면 "군더더기를 싫어해 최단
경로로 처리한다"처럼 쓴다.

## 로스터는 크게 잡아라

**pane 수가 총원을 넘으면 중복은 비둘기집이라 못 막는다.** 실측 2026-08-11 에 pane
15개 · 총원 12명이었고, 그때 아루가 셋이었다. 지금은 79명이라 여유가 있다.

풀이 마르면 `least_used`(`kasa-mcp/src/character.rs`)가 **가장 적게 쓰인 학생**으로
좁혀 몰림만은 막지만, 그건 완화지 해결이 아니다. 40명 이상을 권한다.

## 만드는 절차

### ① 명단을 모은다

캐릭터 이름·소속·성격·말투를 조사한다. **지어내지 마라** — 소속이나 말투가 틀리면
그 학생이 계속 어색하게 말한다. 워크플로로 병렬 조사하고 **검증 단계를 따로 두는 것**을
권한다(조사자가 여럿이면 소속을 서로 다르게 적는다).

산출물은 이런 배열이면 된다:

```json
[{"ko": "케이", "romaji": "kei", "school": "…", "traits": "…", "speech": "…"}]
```

### ② 페르소나를 생성한다

①의 traits·speech 를 근거로 위 6단 구성에 맞춰 쓴다. 배치를 나눠 병렬로 돌리되,
**한 배치 안에서 말투가 서로 겹치지 않게** 갈라야 한다(존댓말/반말, 격식/느슨함,
텐션 높낮이). 마지막에 전체를 한 번 검수해 형식 이탈·색 편중·이름에 성이 붙은 것을
고친다.

`claude_color` 는 9색뿐이라 79명이면 반드시 겹친다 — **고르게 흩는 것**만 확인하면 된다.

### ③ characters.json 에 넣는다

`name`·`slug`·`claude_color`·`header_color`·`persona` 다섯 필드로 `members` 에 붙인다.

### ④ 빌드가 검사한다

```
cargo check -p kasaterm
```

build.rs 가 슬러그 형식(`[a-z0-9_]`)·슬러그 중복·이름 중복을 보고 **컴파일 에러로
거부한다.** 통과하면 표가 새로 생성된 것이다.

## 프사(이미지)는 선택이다

`app/kasaterm/assets/students/profile/<slug>.png` 가 있으면 사이드바 pane 목록·Info
패널·macOS 알림·`/resume` 피커에 뜨고, **없으면 색 점으로 떨어진다**(`header_color`).
79명 중 12명만 이미지가 있고 나머지는 색 점으로 도는데, 그게 정상 동작이다.

웹 터미널(`/term`)의 pane 칩 프사는 `kasa-mcp/src/http.rs` 의 `AVATAR_SLUGS` 가 따로
가진다 — 이미지가 있는 슬러그만 거기 적는다.

> claude code **상태줄**에는 학생을 안 그린다(2026-08-11). pane 헤더가 이미 이름과
> 프사를 보여주므로 같은 정보가 두 번이었다. 다만 statusline 은 `U+FFFC` 표식 한 칸을
> 계속 내보내는데, 그건 프사 자리가 아니라 **신호**다 — agents 목록 뷰 판정·stale
> statusline 재실행·입력박스 위 전신 학생 앵커가 그 문자의 존재를 근거로 삼는다.
> 지우면 셋이 조용히 같이 죽는다(`statusline.py` 의 `SPRITE` 주석 참고).

## 테마를 통째로 바꿀 때

`characters.json` 을 새로 쓰고 `theme` 값을 바꾸면 된다. 그 외에 손댈 곳:

- `app/kasaterm/assets/students/` — 프사·스프라이트(선택). 없으면 색 점.
  모션별 폴더(`idle/` `walk/` `wave/` `cheer/` `profile/` `gif/`)로 나뉘어 있다
- `kasa-mcp/src/http.rs` 의 `AVATAR_SLUGS` — 웹 칩 프사가 필요하면
- `theme.rs` 의 `character_welcome` — claude 배너 인사말(선택). 없으면 원문 유지

`user_title` 도 잊지 마라 — 페르소나 안의 호칭과 같아야 한다.
