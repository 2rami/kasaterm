# character-src — SCHALE OS 캐릭터 에셋 생성 작업방

아로나 UI 학생 초상(`public/assets/char-*.png`)의 **생성 원본(누끼 전 흰배경)** 보관소.
완성 누끼본만 `public/assets/`로 승격한다.

## 생성 도구 — codex 내장 `$imagegen` (gpt-image-2)

별도 플러그인·OpenAI API 키 없이 **codex CLI(0.117+)** 가 ChatGPT 구독 quota로 그린다.
상세: 글로벌 메모리 `reference_codex_imagegen`.

```bash
# 화풍 통일: 기존 char-arona.png 를 reference 로 물려 같은 치비 픽셀 화풍 유지
codex exec --skip-git-repo-check \
  "Use \$imagegen with the attached reference image. Keep the EXACT same art style \
   (chibi 2-head SD, soft pixel shading, full body, halo, white background). \
   ONLY change the character to <학생명> from Blue Archive: <외모 디테일>. \
   Same canvas/framing. Output PNG." \
  -i public/assets/char-arona.png        # ← -i 는 반드시 프롬프트 뒤 (가변인자라 순서 중요)
```

- 출력: `~/.codex/generated_images/<uuid>/ig_*.png` (codex sandbox read-only라 cp 막힘 → 직접 복사)
- 흰배경 → 누끼는 `/tmp/nukki.py`(PIL 흰배경 투명+crop) 패턴 그대로
- gpt-image-2 는 `background=transparent` 미지원 → 흰/크로마 배경 생성 후 누끼

## 산출물 종류

| 종류 | 위치 | 용도 | 비고 |
|---|---|---|---|
| 정적 초상 | `public/assets/char-*.png` | 교실 배치 | arona·prana·midori·momoi·yuzu·arisu 누끼 완료 |
| 워크 스프라이트 | `walk/<학생>/` → `public/assets/sheet-<slug>.png` | 교실 이동 애니 | 미도리·모모이·유즈·아리스·아로나·프라나 (4방향×6f + manifest). [walk/README](walk/README.md) |
| 대화창 프사 | `portrait/<학생>.png` → `public/assets/char-<slug>-bust.png` | chat bust 아바타 | 미도리·모모이·유즈·아리스·아로나·프라나 (상반신+공식무기+성격표정, 누끼 투명) |

## 대화창 프사 (`portrait/`)

`public/assets/char-*.png` 치비 화풍을 reference 로 물려 **상반신 bust + 각자 공식 무기 + 성격 표정**으로 새로 생성(base 크롭 아님 — base 는 정면 직립·무표정·무기없음이라 프사로 밋밋).

- 미도리 = 초록 저격소총·차분 / 모모이 = 분홍 저격소총·활발 / 유즈 = 검은 **포톤런처**(시안 광자포신+주황 게임LED)·졸린눈
- 아리스 = 흰/회색 SF **빔캐논**(게임패드 후광)·침착 / 아로나 = **무기없음**(두손 모음)·밝은미소 / 프라나 = 검은 SF **라이플**·앞머리 한쪽눈 가림·차분
- **무기는 `desc` 로 색+종류 명시** (치비 초상엔 무기 없음). 디테일은 `_ref-*.png` 공식 풀일러 참조해 묘사 — 무기 모양이 ref 와 다르면 **style ref(char-X.png) + 무기 ref(_ref-X.png) 둘 다 `-i` 로 동시 첨부**(유즈 포톤런처 교정 사례)
- 레시피: `codex exec --output-schema /tmp/ig-schema.json --color never -i char-X.png [-i _ref-X.png] --skip-git-repo-check "...UPPER-BODY BUST PORTRAIT...holding her <색> <무기>..." </dev/null` → 출력은 `~/.codex/generated_images/`(`/mnt/data/0.png` 는 환각경로, `ls -t ~/.codex/generated_images/*/*.png | head -1` 로 회수) → flood-fill 누끼(`/tmp/nukki.py`)
- raw 흰배경본은 보관 안 함(codex 재생성 가능). 6명 전원 생성 완료

`_rejected/` = 화풍 미채택(반신 리얼 등) 참고 보관. `_*.png` 는 gitignore(공식 IP ref).
