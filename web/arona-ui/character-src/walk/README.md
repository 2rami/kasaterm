# walk — 게임개발부 학생 워크 스프라이트 세트

`public/assets/char-*.png` 정적 초상을 reference 로 물려 생성한 **4방향 워크 애니메이션** 보관소.
교실(RoomMap)에서 학생이 걸어다니게 하는 통합 단계 전의 산출물.

## 보유 세트

| 학생 | 정체성 | 폴더 |
|---|---|---|
| 미도리 | 금발 bob · 초록 고양이귀 후드 | `midori/` |
| 모모이 | 주황금발 · 검은 고양이귀 후드 | `momoi/` |
| 유즈 | 긴 적발 · 흰 후드 (무기 제외, `-nomirror`) | `yuzu/` |
| 아리스 | 짙은 청록 트윈테일 · 시안 게임패드 후광 | `arisu/` |
| 아로나 | 라이트블루 단발 · 토끼귀 · 파란 후광 | `arona/` |
| 프라나 | 은백발 · 흰 토끼귀 리본 · 핑크 후광 (`-nomirror`) | `prana/` |

## 규격 (각 폴더 공통 — perfectpixel.sprite/2)

- `sprite-sheet.png` — 1536×1536, **cell 256×256**, 투명 배경(매팅 완료)
- `manifest.json` — 엔진용(pivot·fps·rects). 상태 6개: `idle`(4프레임) · `walk`(=정면 6) · `walk-south/east/north/west`(각 6)
- `sprite-sheet.json` — Aseprite 포맷
- `frames/<state>/frame-NN.png` · `gif/<state>.gif` · `apng/<state>.png` · `base.png`

## 생성 레시피 (ppgen codex — API 키 0)

```bash
# perfectpixel-studio 빌드(ppgen) 필요. codex OAuth = ChatGPT 구독 quota
ppgen -provider codex \
  -ref <public/assets/char-X.png> \
  -desc "X from Blue Archive, chibi game-dev-club student: <외형>. No weapon." \
  -dirset walk -dirs south,east,north,west [-nomirror] \
  -out ./X-walk
```

- **`-ref` + `-desc` 둘 다 필수** — ref 만 주면 기본 기사(knight) 프롬프트가 섞임
- **비대칭 캐릭터(사이드테일/한쪽눈 가림)는 `-nomirror`** — 안 주면 west 가 east 미러라 쏠린 머리가 뒤집힘. 유즈·프라나가 해당
- codex hang 은 간헐적 — Monitor 로 감시, 멈추면 kill·재시도
- **한 방향이 codex hang 으로 0점 처리되면 그 row 가 시트에서 통째로 빠진다**(프라나 north 사례). 복구 = 그 방향만 `-states walk -dirset walk -dirs <방향> -out X-north` 단독 재생성 → PIL 로 메인 시트에 row 삽입(표준 순서 north=4·west=5 유지) + 후광/색 일탈 시 hue 보정. 합성 스크립트는 `/tmp/merge-north.py` 패턴
- 상세 메모리: `reference_sprite_gen_perfectpixel` · `reference_pixel_sprite_pipeline`

## TODO — 교실 통합

1. ~~승격~~ **완료** — `public/assets/sheet-<slug>.png`(walk 통합시트) + `char-<slug>-bust.png`(대화창 초상). SLUG/KNOWN(`src/lib/sprites.ts`) 에 6명 모두 등록 → `SpriteWalk` 가 `sheet-<slug>.png` 존재를 자동 감지해 walk 애니 렌더
2. `RoomMap.tsx` 에서 학생 이동 방향(상=back/하=front/좌우=side)을 `SpriteWalk` 의 `facing`·`walking` 으로 연동 — 실제 교실 배치/이동 로직
