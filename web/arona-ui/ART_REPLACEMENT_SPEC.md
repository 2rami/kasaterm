# 샬레 교실 아트 교체 규격 (거노용)

> arona-ui 교실의 타일·캐릭터를 거노가 **직접 그린 PNG**로 갈아끼우기 위한 명세.
> 현재는 전부 **코드 생성 placeholder**(외부 에셋 0). 이 문서대로 PNG를 만들면 끼워진다.

## 0. 현재 상태 (뭘 교체하나)
| 대상 | 지금 | 파일 |
|---|---|---|
| 타일(바닥/벽/책상/칠판/의자) | canvas 런타임 생성(단색+픽셀보더) | `src/scene/placeholderTileset.ts` |
| 캐릭터(학생 도트) | 색블록 18×22 + 이니셜 글자 | `src/scene/ClassroomCharacter.ts` |
| 맵 배치 | 코드(14×10 그리드) | `src/scene/classroomMap.ts` |
| 렌더러 | **art-agnostic**(텍스처만 주입받음, 무수정) | `src/scene/TiledMapRenderer.ts` |

렌더러는 손 안 댄다 — 타일셋 텍스처와 캐릭터 시트만 PNG로 바꿔 넣는 구조.

## 1. 타일맵 PNG 규격
- **타일 크기**: 32 × 32 px (`TS=32`).
- **타일셋 시트**: 가로 1행, **8칸** → **256 × 32 px** PNG 1장.
- **gid 순서(왼→오, firstgid=1 고정)**:
  | 칸(x) | gid | 타일 |
  |---|---|---|
  | 0 | 1 | 바닥 (FLOOR) |
  | 1 | 2 | 벽 (WALL) |
  | 2 | 3 | 책상 (DESK) |
  | 3 | 4 | 칠판 (BOARD) |
  | 4 | 5 | 의자 (CHAIR) |
  | 5~7 | 6~8 | 예비(미사용, 비워도 됨) |
- **포맷**: PNG, 투명 배경 허용(빈 칸=투명). 픽셀아트면 **안티앨리어싱 끔**(렌더러가 nearest).
- **주의**: 각 타일은 32×32 칸에 꽉 채워 그린다(칸 경계=타일 경계). 칸 순서·크기가 위 표와 다르면 어긋난다.

## 2. 캐릭터 스프라이트 시트 규격
- **프레임 크기**: 32 × 32 px 권장(타일 1칸. 더 크게 그리려면 코드에서 앵커·오프셋 조정 필요).
- **시트 배치**: **행 = 모션, 열 = 애니 프레임**.
  | 행(y) | 모션 | 트리거(board status) |
  |---|---|---|
  | 0 | idle | 평상 |
  | 1 | working | 작업 중(현재는 좌우 흔들림으로 표현) |
  | 2 | waiting | 입력 대기(현재 점멸 도트) |
  | 3 | blocked | 막힘(현재 ⚠) |
- **열(프레임 수)**: 모션당 2~4프레임 권장(루프 애니). 예: 4프레임이면 시트 = **128 × 128 px**(4열×4행).
- **포맷**: PNG, 투명 배경, 픽셀아트 nearest.
- **색 구분**: 현재는 캐릭터별 accent 색으로 구분. 스프라이트로 가면 캐릭터마다 **별도 시트**(characters.json `sprite` 경로)로 외형 자체를 다르게.

## 3. characters.json 연결
- 위치: `~/.config/kasaterm/characters.json`(우선) 또는 번들 `collab-hooks/characters.json`.
- 각 캐릭터(leader/members[])에 **`sprite` 필드**(옵셔널)에 시트 경로를 넣으면 그 캐릭터가 그 시트를 쓴다.
  ```json
  { "name": "아로나", "claude_color": "...", "sprite": "sprites/arona.png" }
  ```
- 경로는 arona-ui가 접근 가능한 정적 경로(번들 Resources 하위 권장). 비우면 placeholder 색블록 유지.
- 타입은 이미 정의됨: `CharacterDef.sprite?: string` (`src/lib/mcp.ts`). **단, 아래 4의 로딩은 아직 미구현.**

## 4. 현재 스프라이트 로딩 구현 여부 — **미구현**. 필요 작업
PNG를 넣어도 지금은 안 뜬다. 끼우려면 코드 작업 필요(난이도 표기):

1. **타일셋 PNG 로더** [小, ~30분]: `makePlaceholderTileset()`(canvas 생성)을 `Assets.load(url)` →
   `Texture`로 교체. `ClassroomView`가 `new TiledMapRenderer(map, [tileTexture])`에 넘기는 한 줄만 바뀜.
   렌더러는 `textureForGid`로 이미 32px 칸을 슬라이스하므로 무수정.
2. **캐릭터 AnimatedSprite** [中, ~반나절]: `ClassroomCharacter`의 `body`(Graphics 색블록)를
   pixi `AnimatedSprite`로 교체. 시트 텍스처를 32×32로 슬라이스 → 행별 모션 프레임 배열 구성 →
   `setStatus(s)`가 해당 모션 행으로 전환. 이니셜 글자는 유지하거나 숨김 선택.
3. **characters.json sprite 배선** [小, ~1시간]: `fetchCharacters()`로 읽은 `sprite` 경로를
   `ClassroomView`가 `Assets.load` → 캐릭터별 텍스처를 `ClassroomCharacter`에 주입(생성자 인자 추가).
   경로 없으면 placeholder 폴백.

→ 거노가 **타일셋 PNG 1장 + 캐릭터 시트(캐릭터당 1장)** 를 위 규격으로 그려주면, 위 1~3 작업으로 끼운다.
타일만 먼저(작업 1)도 가능 — 캐릭터는 색블록 둔 채 배경만 교체.

## 5. 권장 작업 순서
1. 타일셋 PNG(256×32) 먼저 — 교실 분위기가 가장 크게 바뀌고 작업 1만으로 적용.
2. 캐릭터 시트는 1캐릭터(아로나)로 파일럿 → 모션 4행·프레임 수 확정 후 나머지 동일 규격으로 양산.
