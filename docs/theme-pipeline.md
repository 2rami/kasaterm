# 테마 만들기 — 캐릭터 세트를 통째로 갈아끼우는 절차

「명조 테마 만들어」 한 마디에서 학생 67명의 페르소나·스프라이트·색까지 나오게 하는
파이프라인이다. 블루아카이브 로스터 77명을 이 절차로 만들었고, 그때 실제로 밟은
지뢰를 각 단계에 붙여 뒀다.

로스터의 **필드 규칙과 페르소나 6부 구조**는 [theme-roster.md](theme-roster.md) 에 있다.
여기서는 그것을 **어떤 순서로 어떻게 채우는가**만 다룬다.

```
① 조사        어떤 IP인가 · 캐릭터가 몇 명인가
② 선정        몇 명을 넣을지 · 누구를 넣을지 (사용자에게 묻는다)
③ 캐릭터 조사  scripts/theme-wiki.py     → theme-src/<slug>/wiki.json
④ 페르소나     characters.json 작성       (에이전트 여럿에 나눠 준다)
⑤ 묘사        theme-src/<slug>/desc.txt  (에이전트 여럿에 나눠 준다)
⑥ 스프라이트   scripts/theme-sprites.py gen
⑦ 설치        scripts/theme-sprites.py install
```

---

## ① 조사 · ② 선정

**사용자에게 반드시 묻는다**: 몇 명을 넣을지, 특정 캐릭터를 꼭 넣어야 하는지.

인원수는 취향이 아니라 **기능**이다. pane 수보다 로스터가 작으면 비둘기집 원리로 중복이
확정되고, 그건 어떤 배정 알고리즘으로도 못 막는다. 12명이던 시절 pane 15개에서 같은
학생이 겹쳤던 게 그 이유다. **최소 40명, 넉넉히 70명 이상**을 권한다.

## ③ 캐릭터 조사 — `theme-wiki.py`

```bash
python3 scripts/theme-wiki.py index     # 한글 이름 → 위키 문서 제목 (한 번만)
python3 scripts/theme-wiki.py collect   # 로스터 전원 수집
```

`index` 가 따로 있는 이유는 **이름 매칭이 어려워서**다. 로스터엔 「케이」인데 문서
제목은 `Tendou Kei` 이고, 로마자 변환으로는 못 맞춘다 — 아리스는 `Alice` 다. 위키
인포박스의 `name_kr` 필드로 표를 한 번 만들어 두면 이 문제가 통째로 사라진다(BA 77명
전원 매칭됐다). 다른 IP 로 갈 때는 `THEME_WIKI_API` 로 위키를 바꾸고, 그 위키에
현지화 이름 필드가 없으면 `theme-src/_index.json` 을 손으로 채운다.

> ⚠️ **WebFetch 로 fandom 을 못 읽는다**(402). MediaWiki API 를 직접 쳐야 한다 —
> 스크립트가 그렇게 한다.

## ④ 페르소나 · ⑤ 묘사 — 에이전트에 나눠 준다

둘 다 「손이 많고 판단이 적은」 일이라 학생 pane 여럿에 나눠 준다. 오케스트레이터가
직접 하면 문서 67개가 컨텍스트로 들어와 말라 죽는다.

`desc.txt` 는 스프라이트 생성기에 그대로 들어가는 **영문 한 줄**이다. 이 한 줄의
품질이 결과를 정한다 — 「분홍 머리 소녀」면 아무나 나오고, 아래처럼 쓰면 그 캐릭터가
나온다(케이로 실측):

```
Kei (Tendou Kei) from Blue Archive, chibi Millennium game-dev-club student: very long
straight white hair with center bangs parted to the right, sharp pink tsurime eyes, pale
skin, glowing pink halo, dark gray blazer under a jacket with fluorescent pink inner
lining, short skirt, black thigh-high stockings. No weapon.
```

- **`No weapon.` 으로 끝낸다.** 안 쓰면 총이 딸려 나온다
- **헤일로(모양·색)를 반드시 넣는다.** BA 캐릭터의 최대 식별자다
- 근거는 `wiki.json` 의 `appearance`/`halo`/`uniform` **만**. 기억으로 쓰면 틀린다
- 한 줄, 400자 이내, 외형만(성격·스토리 금지)

## ⑥ 스프라이트 — `theme-sprites.py gen`

```bash
PPGEN=/path/to/ppgen python3 scripts/theme-sprites.py gen --jobs 4
```

`ppgen` 은 [perfectpixel-studio](https://github.com/2rami/perfectpixel-studio) 의
헤드리스 CLI 다(`go build -o /tmp/ppgen ./cmd/ppgen`). 1명에 2분 남짓 걸린다.

### 돈이 나가는 지점 — 1명당 API 호출은 **2회**다

`out/` 에 24장이 쌓이지만 **과금되는 건 2장뿐**이다. 나머지는 로컬 가공이다.

| 산출물 | 크기 | API |
|---|---|---|
| `base.png` | 1024×1024 | **호출 1** |
| `sprite-sheet.png` | 1536×1024 (18프레임을 한 장에) | **호출 2** — base 를 참조로 |
| `frames/**`, `apng/`, `gif/` | — | 시트를 **잘라낸 것**, 호출 없음 |

`gpt-image-2` 는 출력 토큰 과금($30/1M)이라 품질에 따라 갈린다. 77명 = 151회 기준:
low $1.5 · medium $6 · high $24, `-attempts`(기본 3) 재시도가 붙으면 최대 그 2배.
**한 테마를 굽는 건 한 자릿수~수십 달러짜리 작업이다** — 돌리기 전에 어느 계정
크레딧으로 나가는지 확인할 것. 2026-08-11 에 확인 없이 151회를 쏴서 개인 OpenAI
크레딧을 다 태웠다.

**프로바이더는 사내 OpenGateway 를 기본으로 쓴다**(거노 확정 2026-08-24 「og로
하잖아」 — 과금이 개인 크레딧이 아니라 회사 게이트웨이로 나간다):

```bash
THEME_PROVIDER=openai \
OPENAI_BASE_URL=https://apis.opengateway.ai/v1 \
THEME_KEY=$(cat ~/.config/opengateway.key) \
THEME_MODEL=openai/gpt-image-2 \
PPGEN=/tmp/ppgen THEME_SRC=theme-src-<id> THEME_ROSTER=theme-src-<id>/roster.json \
  python3 scripts/theme-sprites.py gen --jobs 4
```

- ⚠️**키 값을 커밋·로그·화면에 찍지 마라** — 공개 레포다. 키 파일은 `~/.config/opengateway.key`.
- `input_fidelity` 는 보내지 마라 — `gpt-image-2` 가 400 을 준다(항상 high fidelity).
- `--ref` 를 쓸 때만: 게이트웨이 업로드 상한 때문에 1024px PNG 는 413 이다 — 참조는
  **512px·100KB 이하 JPEG** 로 줄여 보낸다(2026-08-20 실측).

대안 프로바이더: `codex`(ChatGPT 구독 인증, 과금 별개 — 단 구독 사용량 한도가 있다.
2026-08-24 소진돼 9/10 까지 막힌 적이 있다) · `openai` 는 api.openai.com 직결이라 위
표대로 개인 돈이 나간다 — 쓰기 전에 반드시 확인.

> **「OpenGateway 로는 못 굽는다」는 옛 문장이다(2026-08-11 실측).** 당시엔
> `/v1/images/edits` 가 없어 동작 프레임(편집 경로 필수)을 못 만들었고, 참조를 실어도
> 200 과 함께 조용히 무시됐다. 그 뒤 게이트웨이에 편집 경로가 정식 배포됐고 참조
> 반영도 실측으로 확인됐다(`theme-sprites.py` docstring 2026-08-20, live 415 응답
> 2026-08-24 — **404=엔드포인트 없음 / 415=있는데 형식 문제**로 가른다). 이 문서만
> 낡은 채 남아 2026-08-24 에 두 학생이 「OG 불가」로 오판하고 굽기를 통째로 세웠다.

> ⚠️ **키는 `~/Library/Application Support/perfectpixel/config.json` 이 최우선**이다.
> macOS 의 `os.UserConfigDir()` 은 `~/.config` 가 아니다 — ppgen 문서의 경로는 리눅스
> 기준이고, 그걸 믿고 환경변수만 넣으면 config 의 옛 키가 이겨서 401 이 난다. 설정을
> 건드리지 않고 환경변수를 쓰려면 `HOME` 을 빈 디렉토리로 바꿔 실행한다.

### 색이 어긋나면 — `--ref`

묘사만으로도 대개 정확히 나오지만 **가끔 색이 통째로 어긋난다**. 케이는 desc 에
`very long straight white hair` 라고 썼는데도 같은 문장의 pink(헤일로·재킷 안감)에
끌려 **분홍 머리**로 나왔다. 픽셀 검수는 이걸 못 잡는다 — 그림 자체는 멀쩡하기 때문이다.

```bash
python3 scripts/theme-sprites.py gen kei --ref --force
```

`③ 캐릭터 조사` 가 받아 둔 공식 포트레이트(`theme-src/<slug>/ref.png`)를 정체성 참조로
넘긴다. 실측에서 흰 머리·분홍 눈·안감까지 공식 원본과 일치했다. 다만 **4배 느리다**
(base+idle 기준 204초 vs 45초). 그래서 기본값이 아니다 — **묘사로 전부 만든 뒤 프로필을
한 장에 모아 눈으로 보고, 어긋난 애만 이걸로 다시 굽는 것**이 가장 싸다. 정체성은
자동 검사보다 사람 눈이 빠르다.

> ⚠️ **ppgen 의 품질 점수를 믿지 마라.** 점수 69로 통과한 케이 프레임이 고유색 10개·
> 런 14px 로 형체를 알아볼 수 없었다(정상은 색 28~29·런 2). 그 점수는 프레임 수와 모션
> 다양성을 보지 **픽셀 양자화 붕괴는 안 본다**. 같은 프롬프트를 다시 돌리면 멀쩡히
> 나오는 생성 편차라 배치로 수십 명을 돌리면 반드시 몇 명이 이렇게 나온다. 그래서
> `gen` 은 자체 검수(`check()`)를 걸고 실패하면 최대 3회까지 다시 만든다.

## ⑦ 설치 — `theme-sprites.py install`

```bash
python3 scripts/theme-sprites.py install
python3 scripts/theme-sprites.py status   # 어디까지 됐는지
```

`frames/<state>/frame-NN.png` 를 앱이 찾는 자리로 옮긴다. 크기가 이미 256px 로
같아 리사이즈는 없다.

⚠️ **기본 목적지는 번들(`app/kasaterm/assets/students/`)이다 — 런타임 테마 자리가
아니다.** 번들은 빌드에 박혀 나가는 기본 세트(블루 아카이브)고, 새로 구운 작품은
거기가 아니라 사용자 설정 폴더로 가야 한다. 그냥 `install` 을 돌리면 기본 세트를
덮어쓴다. 목적지는 `THEME_DST` 로 지정한다:

```bash
THEME_SRC=theme-src-<작품> \
THEME_DST=~/.config/kasaterm/themes/<작품>/sprites \
THEME_ROSTER=theme-src-<작품>/roster.json \
  python3 scripts/theme-sprites.py install
cp theme-src-<작품>/roster.json ~/.config/kasaterm/themes/<작품>/theme.json
```

**`theme.json` 이 없으면 그 폴더는 없는 것과 같다** — `active_theme_dir_in`
(`crates/kasa-mcp/src/character.rs`)이 `theme.json` 이 실재할 때만 폴더를 돌려주고,
없으면 오류 없이 번들로 떨어진다. 스프라이트만 넣고 화면이 안 바뀌면 여기를 보라.
`roster.json` 과 `theme.json` 은 스키마가 같아 그대로 복사하면 된다.

| ppgen 산출 | 앱 자산 | 개수 |
|---|---|---|
| `frames/idle/frame-NN.png` | `idle/<slug>-N.png` | 4 |
| `frames/walk/frame-NN.png` | `walk/<slug>-N.png` | 6 |
| `frames/wave/frame-NN.png` | `wave/<slug>-N.png` | 4 |
| `frames/cheer/frame-NN.png` | `cheer/<slug>-N.png` | 4 |
| `gif/idle.gif` | `gif/<slug>.gif` | 1 |
| (idle 첫 프레임에서 잘라 만듦) | `profile/<slug>.png` | 1 |

> 폴더는 2026-08-14 에 갈랐다. 그 전에는 1500장이 `<slug>-walk-N.png` 처럼 한
> 자리에 평평하게 쌓여 있었다. 앱의 로더는 **옛 이름도 계속 읽는다** — 사용자가
> 자기 그림을 넣어 둔 override 폴더가 구조 변경 하나로 죽으면 안 되기 때문이다.

> ⚠️ **프로필은 알파 경계 맨 위에서 자르면 안 된다.** BA 캐릭터는 **헤일로가 머리 위에
> 떠 있어** 경계 상단이 헤일로 꼭대기다. 거기서 자르면 얼굴이 아래 가장자리로 밀려
> 머리카락만 찬 그림이 나온다. 「처음으로 폭이 넉넉해지는 줄」을 머리 시작으로 잡는다.

## ⑧ 대조 — `theme-sprites.py sheet`

```bash
THEME_SRC=theme-src-<작품> \
THEME_DST=~/.config/kasaterm/themes/<작품>/sprites \
THEME_ROSTER=theme-src-<작품>/roster.json \
  python3 scripts/theme-sprites.py sheet --out /tmp/theme-sheet.png
```

⚠️ **`THEME_DST` 를 ⑦ 과 똑같이 줘야 한다.** 안 주면 기본값인 번들을 읽어 **오른쪽
「구운 프로필」 칸이 전원 빈 채로** 나온다 — 설치는 멀쩡히 됐는데 시트만 비어서, 설치가
실패한 것처럼 읽힌다(2026-08-24 프로세카에서 실제로 한 번 헛돌았다).

**왼쪽 위키 원본 / 오른쪽 구운 프로필**을 한 줄에 6명씩 붙여 한 장으로 만든다.
「누가 안 닮았나」는 한 명씩 열어 보면 기준이 흔들려서 못 고른다 — 나란히 놓고 훑어야
튀는 애가 보인다. 열어 보려면 pane 에 직접 띄우면 된다:

```bash
curl "http://127.0.0.1:8765/open-image?path=/tmp/theme-sheet.png&pane=$KASATERM_PANE_ID"
```

프로필 배경이 통째로 한 색으로 보여도 알파 문제가 아닐 때가 많다 — 트윈테일처럼 머리가
96×96 을 꽉 채우면 그렇게 보인다. 의심되면 투명을 자홍으로 합성해 한 번 확인하고,
번들과 모서리 알파를 비교하는 식으로는 가르지 마라(번들은 크롭이 더 여유로워 0장이다).

⚠️ 이 시트는 **디스크에 있는 것을 그린다** — 다시 구운 학생과 옛 그림이 섞여 있어도
구분해 주지 않는다. 화풍이 튀는 칸이 보이면 `stat` 으로 날짜를 확인할 것.

---

## 테마를 통째로 바꾼다는 것

**폴더 하나 = 테마 하나다.** `~/.config/kasaterm/themes/<id>/` 아래 `theme.json`
(로스터·팔레트)과 `sprites/`(그림)가 있으면 그게 한 벌이고, 폴더째 주고받으면 그게
곧 배포다. 경로는 `KASATERM_THEMES_DIR` 로 옮길 수 있다.

고르는 것은 **설정 화면**이다 — 목록은 `theme.json` 이 있는 폴더만 잡고(`list_themes`),
카드에 로스터 인원수와 미리보기 얼굴 셋(`sprites/profile/<slug>.png`)을 띄운다.
고르면 `settings.json` 의 `character_theme` 에 폴더 이름이 적히고, 그 뒤 그림을 찾는
곳이 `<테마>/sprites` 로 바뀐다(`students_dir`, `app/kasaterm/src/socket.rs`). 빈
문자열이면 번들이다. 환경변수 `KASATERM_CHARACTER_THEME` 가 설정보다 세다.

빌드 상수 `CHARACTER_SLUGS`(`build.rs` 가 `characters.json` 에서 생성)는 **번들
전용 폴백으로 남아 있다.** 활성 로스터는 런타임에 굽고(`theme.rs` 의 `build_roster`),
쓸 만한 항목이 하나도 없으면 번들로 떨어진다. 테마를 갈아 끼운 뒤에는
`invalidate_roster()` 를 불러야 다음 조회가 새 로스터를 본다.

## 구운 그림은 레포에 넣지 않는다

`.gitignore` 가 원화(`theme-src-*/*/ref.png`)와 생성 프레임(`theme-src-*/*/out*/`)을
배제한다. 커밋하는 것은 **묘사(`desc.txt`)와 로스터(`roster.json`)뿐**이고, 완성된
스프라이트도 레포 밖(`~/.config/kasaterm/themes/`)에 둔다. 이유 셋:

- **이 레포는 공개다.** 네 작품 모두 타사 IP 의 2차 창작물이다.
- 작품당 400장 · 3~5MB 이고 png 는 delta 가 안 먹는다. 다시 구울 때마다 히스토리에
  통째로 쌓인다.
- 배포 단위가 폴더라 레포에 있을 이유가 없다 — 건넬 때는 폴더를 zip 으로 묶는다.

⚠️ **대신 그림은 잃으면 못 되돌린다.** ppgen 은 비결정적이라 같은 묘사로 다시 구워도
다른 그림이 나온다. 재생성 가능한 산출물이 아니라 일회성 결과물로 다뤄야 한다 —
`/tmp` 에 두지 말고, 백업은 폴더째 zip 으로.
