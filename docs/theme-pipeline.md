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

**프로바이더는 `codex` 를 기본으로 쓴다**(ChatGPT 구독 인증이라 위 과금과 별개).
`openai` 는 api.openai.com 직결이라 위 표대로 돈이 나간다.

⚠️ **사내 OpenGateway 로는 못 굽는다.** `/v1/images/generations` 만 있고
`/v1/images/edits` 가 없어 **동작 프레임을 못 만든다** — 동작은 base 를 참조 이미지로
넘겨 그리므로 편집 경로가 필수다. 게다가 게이트웨이는 **필드 검증을 전혀 안 해서**
참조를 실어 보내도 200 을 주면서 조용히 무시하고 딴 그림을 준다. 404 로 막히는 게
아니라 그럴듯한 결과가 와서 **눈으로 보기 전엔 모른다.**

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
같아 리사이즈는 없다. 앱 자산 경로는 `app/kasaterm/assets/students/` 기준이다.

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
python3 scripts/theme-sprites.py sheet --out /tmp/theme-sheet.png
```

**왼쪽 위키 원본 / 오른쪽 구운 프로필**을 한 줄에 6명씩 붙여 한 장으로 만든다.
「누가 안 닮았나」는 한 명씩 열어 보면 기준이 흔들려서 못 고른다 — 나란히 놓고 훑어야
튀는 애가 보인다. 열어 보려면 pane 에 직접 띄우면 된다:

```bash
curl "http://127.0.0.1:8765/open-image?path=/tmp/theme-sheet.png&pane=$KASATERM_PANE_ID"
```

⚠️ 이 시트는 **디스크에 있는 것을 그린다** — 다시 구운 학생과 옛 그림이 섞여 있어도
구분해 주지 않는다. 화풍이 튀는 칸이 보이면 `stat` 으로 날짜를 확인할 것.

---

## 테마를 통째로 바꾼다는 것

지금 구조에서 캐릭터 세트를 정하는 것은 **`characters.json` 하나**다. `build.rs` 가
거기서 `CHARACTER_SLUGS` 를 생성하므로 이름↔슬러그 표가 두 벌이 될 수 없고, 슬러그
중복·형식 위반은 **빌드가 거부한다**. 그래서 새 테마는 이 JSON 과 `assets/students/`
자산만 갈아끼우면 된다.

**아직 없는 것**: 런타임 전환. 지금은 빌드 시점에 한 세트가 박히므로 테마를 바꾸려면
다시 굽는다. 설정 화면에서 고르게 하려면 `themes/<name>/` 아래로 JSON 과 자산을 옮기고
`CHARACTER_SLUGS` 를 빌드 상수에서 런타임 로딩으로 바꿔야 한다 — 그때 `build.rs` 의
검증(중복·형식)은 로딩 시점 검증으로 함께 옮겨야 한다. 그 검증이 없으면 두 학생이 같은
inbox 를 쓰는 일이 **오류 없이** 생긴다.
