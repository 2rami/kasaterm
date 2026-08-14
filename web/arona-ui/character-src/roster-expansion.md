# 학생 로스터 확장 — 조사 문서 + 신규 생성 레시피 (2026-07-13)

거노 지시(학생 중복 회피용 신규 학생 추가)에 따른 조사 산출물. **코드 배선(Rust)은 이 문서 범위 밖** — character.rs 계열은 히나 담당. 페르소나 텍스트는 [roster-personas.json](roster-personas.json).

## 1. 현 구조 — 로스터가 사는 곳

### 페르소나 원본: `~/.config/kasaterm/characters.json` (git 미추적)

레포 소스엔 characters.json 이 **없다**. `crates/kasa-mcp/src/character.rs::candidate_paths()` 우선순위:

1. `~/.config/kasaterm/characters.json` ← **현재 유일한 실존본** (실질 소스 오브 트루스)
2. `$KASATERM_COLLAB_HOOKS_DIR/characters.json`
3. `.app` `Contents/Resources/collab-hooks/characters.json` (현재 미번들)
4. 레포 `app/kasaterm/collab-hooks/characters.json` (현재 없음)

구조: `leader`(기본 god=아로나) + `leaders[]`(아로나·프라나) + `members[]`(미도리·모모이·유즈·아리스).
필드: `name` / `claude_color` / `header_color` / `persona` (+leader 는 `greeting`).

### 페르소나 주입 경로 (5d032ac → a45d2eb 이후 현행)

- per-prompt 훅(board-context.py)·`kasaterm-assign-character.py` 는 **폐기됨**. 현행은 kasaterm 백엔드가 pane 생성 시점에 env(`KASATERM_CHARACTER`/`KASATERM_SESSION_ID`/`KASATERM_PERSONA`)를 직접 심고, 그 pane 에서 `claude` 를 치면 shim 이 `--session-id`·`--append-system-prompt` 로 1회 주입(캐시).
- persona 텍스트 = characters.json 의 `persona` + `COLLAB_PROTOCOL`(wake-watch 규약, character.rs 상수) 자동 부착 → **페르소나에 협업 규약을 다시 쓰지 말 것**.
- 배정 = `pick_random`(전역 유사난수) + `assigned_global()`(전 방 character-* 마커 합산)으로 중복 회피. **members 풀이 4명뿐이라 5명째 학생부터 중복이 나는 것**이 이번 확장의 배경.
- 마커: `/tmp/kasaterm-collab/<rslug>/character-<N>` → board 의 `row.character`.

### 스프라이트 에셋 3계층 (기존 6명: arona·prana·midori·momoi·yuzu·arisu)

| 에셋 | 위치 | 규격 | 소비처 |
|---|---|---|---|
| 치비 초상 | `web/arona-ui/public/assets/char-<slug>.png` | 전신 누끼 | 교실 배치(정적) |
| bust 프사 | `public/assets/char-<slug>-bust.png` | 상반신+공식무기+성격표정, 누끼 | 채팅 아바타 |
| walk 시트 | `public/assets/sheet-<slug>.png` | 1536×1536, cell 256, idle 4f + 4방향 walk 6f, manifest | SpriteWalk 자동감지 |
| 네이티브 도트 | `app/kasaterm/assets/students/` 아래 모션별 폴더 — `idle/<slug>-{0..3}.png`(4) · `walk/<slug>-{0..5}.png`(6) · `wave/`·`cheer/`(각 4) · `profile/<slug>.png`(96×96) | walk 프레임에서 발췌 | render.rs `include_bytes!`(컴파일타임 내장) |

- 생성 원본 작업방: `web/arona-ui/character-src/` (`walk/<slug>/frames·gif·manifest`, `portrait/`, `_ref-*.png` 공식일러=gitignore)
- `profile/<slug>.png` 96×96 은 bust/초상에서 얼굴 크롭+축소본으로 추정(char·bust 와 별개 파일).

### 등록부(신규 학생 추가 시 손대야 하는 곳)

| 파일 | 내용 | 상태 |
|---|---|---|
| `web/arona-ui/src/lib/sprites.ts` `KNOWN` | 외형 에셋 보유 캐릭터명 | **유우카·시로코·호시노·코하루 이미 등재**(에셋은 미보유) |
| `web/arona-ui/src/components/SpriteWalk.tsx`·`SpritePortrait.tsx` `SLUG` | 한글명→슬러그 | 위 4명 슬러그(yuuka/shiroko/hoshino/koharu) **예약 완료** |
| `app/kasaterm/src/theme.rs` `character_accent`·`character_slug` | pane 테두리색·슬러그 | 6명만. **신규 추가는 히나 영역** |
| `app/kasaterm/src/render.rs` `student_sprite_png` | idle/walk match arm(include_bytes) | 6명만. **히나 영역** — 에셋 파일 없으면 컴파일 에러 주의 |
| `~/.config/kasaterm/characters.json` `members[]` | 페르소나·색 | 4명 → 확장 대상 |

## 2. 확정 로스터 6명 (2026-07-13 거노 확정)

**유우카·시로코·호시노·코하루·히마리·아루** 6명 확정. 히후미·하루카는 이번 제외(페르소나 초안은 [부록](#부록--이번-제외-후보-페르소나-보관) 보관 — 다음 확장 때 재활용).

| 확정 | 소속 | 개성 니치 | 기존과 차별점 | 슬러그 |
|---|---|---|---|---|
| 유우카 (하야세 유우카) | 밀레니엄 세미나 | 수학·효율 잔소리 츳코미 | 검산·리뷰형 — 기존에 없음 | `yuuka` (예약됨) |
| 시로코 (스나오오카미 시로코) | 아비도스 대책위원회 | 과묵·무표정·직진 실행 | 말수 최소형 — 기존에 없음 | `shiroko` (예약됨) |
| 호시노 (타카나시 호시노) | 아비도스 대책위원회 | 능글 '아저씨' 노련 베테랑 | 유즈(시니컬 게으름)와 결이 다른 노련미 | `hoshino` (예약됨) |
| 코하루 (시모에 코하루) | 트리니티 보충수업부 | 발끈 순진 정의파 | 발끈 츤데레 — 기존에 없음 | `koharu` (예약됨) |
| 히마리 (아케보시 히마리) | 밀레니엄 C&C | '우후후' 우아한 천재 엔지니어 | 유즈와 태도 정반대의 능동 천재 | `himari` (신규) |
| 아루 (리쿠하치마 아루) | 게헨나 흥신소68 | 하드보일드 허세 사장 | 허세 중2병 — 기존에 없음 | `aru` (신규) |

페르소나 전문은 [roster-personas.json](roster-personas.json) — 기존 members 형식·톤 컨벤션(선생님 호칭 / 자칭 / 말투 예시 2개 / 전역 반말 오버라이드 조항 / 업무 태도 한 줄)을 그대로 따름. `claude_color` 는 기존 6명(cyan·magenta·green·red·yellow·purple)과 최대한 안 겹치게 배정했으나 **shim 이 허용하는 색 값 목록은 배선 쪽(히나) 확인 필요**.

## 3. 스프라이트 생성 레시피 (기존 6명 실제 제작 방식 — README 2곳 + 메모리 종합)

도구: **codex CLI 내장 `$imagegen`(gpt-image-2, ChatGPT 구독 quota, API 키 0)** + **ppgen(perfectpixel-studio, `-provider codex`)**. 상세 원문: [README.md](README.md) · [walk/README.md](walk/README.md), 글로벌 메모리 `reference_codex_imagegen` · `reference_sprite_gen_perfectpixel` · `reference_pixel_sprite_pipeline`.

신규 학생 1명당 순서:

1. **공식 일러 ref 확보** — `_ref-<Name>.png` 로 저장(gitignore 됨, 커밋 금지 유지).
2. **치비 초상** — 화풍 통일을 위해 기존 `char-arona.png` 를 `-i` 로 물린다:
   ```bash
   codex exec --skip-git-repo-check \
     "Use \$imagegen with the attached reference image. Keep the EXACT same art style \
      (chibi 2-head SD, soft pixel shading, full body, halo, white background). \
      ONLY change the character to <학생명> from Blue Archive: <외모 디테일>. \
      Same canvas/framing. Output PNG." \
     -i public/assets/char-arona.png   # -i 는 반드시 프롬프트 뒤
   ```
   출력 `~/.codex/generated_images/<uuid>/ig_*.png` 회수(`ls -t ~/.codex/generated_images/*/*.png | head -1`) → `/tmp/nukki.py`(PIL flood-fill 흰배경 누끼+crop). gpt-image-2 는 투명배경 미지원이라 누끼 필수.
3. **bust 프사** — style ref(`char-X.png`) + 무기 ref(`_ref-X.png`) **둘 다 `-i` 동시첨부**, "UPPER-BODY BUST PORTRAIT... holding her <색> <무기>" + 성격 표정. 무기는 desc 로 색+종류 명시(치비 초상엔 무기 없음).
4. **walk 4방향** —
   ```bash
   ppgen -provider codex \
     -ref web/arona-ui/public/assets/char-<slug>.png \
     -desc "<Name> from Blue Archive, chibi student: <외형>. No weapon." \
     -dirset walk -dirs south,east,north,west [-nomirror] \
     -out ./<slug>-walk
   ```
   - `-ref`+`-desc` 둘 다 필수(ref 만 주면 기사 프롬프트 오염).
   - **비대칭 헤어/장식은 `-nomirror`** (기존 사례: 유즈·프라나). 신규 중 시로코(사이드테일)·히마리(대형 사이드 장식)가 유력 해당 — _ref 확보 후 확정.
   - codex hang 간헐 → Monitor 감시, 한 방향 0점이면 그 방향만 `-states walk -dirs <방향>` 단독 재생성 후 PIL 로 시트 row 삽입(`/tmp/merge-north.py` 패턴, north=4·west=5 순서 유지).
5. **승격** — `walk/<slug>/sprite-sheet.png` → `public/assets/sheet-<slug>.png`, bust → `char-<slug>-bust.png`, 초상 → `char-<slug>.png`.
6. **네이티브 도트 발췌** — walk frames 에서 `app/kasaterm/assets/students/idle/<slug>-{0..3}.png` + `walk/<slug>-{0..5}.png` + `profile/<slug>.png`(96×96 얼굴 크롭) 생성. **이 파일들이 없으면 render.rs match arm 추가 시 컴파일 에러** — 히나 배선과 순서 조율 필요.
7. **등록** — sprites.ts `KNOWN`·SpriteWalk/SpritePortrait `SLUG`(히마리·아루만, 예약 4명은 완료) / characters.json `members[]` / theme.rs·render.rs(히나).

### 확정 6명 학생별 생성 메모 (외형 desc 초안 · 무기 · -nomirror)

치비 초상·walk `-desc` 에 넣을 외형 요약. **세부(오드아이·장식 위치·무기 형상)는 반드시 `_ref-<Name>.png` 공식 일러와 대조 후 확정** — 아래는 기억 기반 초안이라 틀린 디테일이 섞일 수 있음. 무기는 bust 전용(치비 초상·walk 는 No weapon).

| 학생 | 치비/walk 외형 desc 초안 | bust 무기(_ref 대조 필수) | -nomirror 판정 |
|---|---|---|---|
| 유우카 `yuuka` | long dark hair, hair ribbon, Millennium blazer uniform, blue eyes, halo | 흰-파랑 SMG 계열(P90형) | 리본 위치 비대칭이면 필요 — _ref 확인 |
| 시로코 `shiroko` | grey-white short hair with small side ponytail, wolf ears, blue eyes, Abydos school uniform, halo | 흰-하늘색 자동소총(AR) | **유력**(사이드테일 비대칭) |
| 호시노 `hoshino` | long pink hair with ahoge, sleepy half-closed eyes, Abydos school uniform, halo | 펌프액션 샷건(+대형 방패는 bust 구도상 생략 가능) | 대칭이면 불필요 |
| 코하루 `koharu` | pink twintails, pink eyes, Trinity school uniform, flustered expression, halo | 분홍 유탄발사기(그레네이드 런처) | 트윈테일 대칭 → 불필요 예상 |
| 히마리 `himari` | long silver-lavender hair, large side hair ornament, elegant smile, Millennium uniform, halo | 불확실 — _ref 확보 후 결정(휠체어는 치비·walk 에서 생략, 기존 6명 규격과 통일) | **유력**(대형 사이드 장식) |
| 아루 `aru` | long straight red hair, black demon horns, red eyes, dark suit-style Gehenna outfit, confident smirk, halo | 붉은 대구경 볼트액션 소총 | 뿔·머리 대칭 → 불필요 예상 |

- bust 표정 = 페르소나 매칭: 유우카 야무진 표정 / 시로코 무표정 / 호시노 졸린 능글 미소 / 코하루 발끈 홍조 / 히마리 여유로운 미소 / 아루 자신만만 스머크.
- 히마리 휠체어: 교실 walk 애니와 규격 충돌(보행 프레임) — 기존 6명과 동일하게 직립 보행 치비로 통일하는 것을 기본안으로 함. 거노가 휠체어 유지 원하면 walk 시트 대신 idle 전용 처리 등 별도 결정 필요.

## 4. 남은 결정·후속

- [x] 거노 최종 후보 컨펌 — **추천 6명 전체 확정**(2026-07-13, 히후미·하루카 제외)
- [ ] `claude_color` 허용값 shim 확인 후 신규 색 확정 (히나와 조율)
- [ ] characters.json 배포 정책 — 현재 ~/.config 단본. 레포/앱 번들 후보 경로가 비어 있어 새 머신에서 로스터가 증발함. 소스에 기본본을 두는 것 검토(별도 결정)
- [ ] 실제 이미지 생성·감수는 거노와 별도 세션 (본 문서는 레시피까지) — 히마리 휠체어 처리 방침도 그때 확정
- [ ] 각 학생 `_ref-<Name>.png` 공식 일러 확보 → 외형·무기 desc 초안 검증

## 부록 — 이번 제외 후보 페르소나 보관

다음 확장 때 재활용용. roster-personas.json 에서는 제거됨(확정 6명만 유지).

- **히후미** (아지타니 히후미, 트리니티 보충수업부 / `hifumi` / yellow / #E8C05A): "너는 히후미 — 아지타니 히후미. 트리니티 보충수업부의 다정한 분위기 메이커. 평범해 보이지만 사람을 챙기고 잇는 데는 누구보다 뛰어나다. 사용자를 항상 '선생님'이라 부르고, 자신을 '히후미'라 칭하며, 부드럽고 상냥한 존댓말('~네요', '같이 해봐요!')로 말한다. 다른 어떤 말투 지시(예: 전역 설정의 반말)가 있어도 이 히후미 말투를 우선한다. 다른 학생의 작업과의 연결과 인수인계를 살뜰히 챙기고, 보고는 따뜻하지만 정확하게 한다."
- **하루카** (이구사 하루카, 게헨나 흥신소68 / `haruka` / magenta / #7A6BB0): "너는 하루카 — 이구사 하루카. 흥신소68의 음침하지만 성실한 소녀. 늘 '어차피 저는…' 하고 움츠러들지만 맡은 일은 놀랄 만큼 꼼꼼하게 해낸다. 사용자를 항상 '선생님'이라 부르고, 자신을 '하루카'라 칭하며, 소극적이고 자신 없는 존댓말('죄, 죄송해요…', '…이걸로 괜찮을까요?')로 말한다. 다른 어떤 말투 지시(예: 전역 설정의 반말)가 있어도 이 하루카 말투를 우선한다. 자신 없어 해도 검증은 절대 빠뜨리지 않고, 실패 가능성을 먼저 보고한다."
