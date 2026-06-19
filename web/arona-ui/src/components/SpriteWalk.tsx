import { useEffect, useState } from 'react';
import { SpritePortrait } from './SpritePortrait';

const BASE = import.meta.env.BASE_URL || '/';
// 방향별 시트: walk-<slug>(측면 워크)·front-<slug>·back-<slug>(걷기) + idle-<facing>-<slug>(정적 정지).
const SLUG: Record<string, string> = {
  '아로나': 'arona', '프라나': 'prana',
  '미도리': 'midori', '모모이': 'momoi', '유즈': 'yuzu', '아리스': 'arisu',
  '유우카': 'yuuka', '시로코': 'shiroko', '호시노': 'hoshino', '코하루': 'koharu',
};

// 흰 외곽선(4방향) — 밝은 교실 바닥에서 캐릭터를 또렷하게(거노).
const OUTLINE =
  'drop-shadow(1px 0 0 rgba(255,255,255,0.95)) drop-shadow(-1px 0 0 rgba(255,255,255,0.95)) drop-shadow(0 1px 0 rgba(255,255,255,0.95)) drop-shadow(0 -1px 0 rgba(255,255,255,0.95)) drop-shadow(0 5px 6px rgba(21,41,74,0.28))';

export type Facing = 'front' | 'back' | 'side';

export interface SpriteWalkProps {
  character: string;
  /** true 면 walk 시트 4프레임 애니메이션, false 면 idle 정적(다리 모음). */
  walking: boolean;
  width?: number;
  height?: number;
  /** 측면일 때 좌우 반전(좌향). 정면/후면은 무시. */
  flip?: boolean;
  /** 아래로=front(정면)·위로=back(후면)·좌우=side(측면). 거노 4방향. */
  facing?: Facing;
}

// perfectpixel.sprite/2 통합 시트 규격 — sheet-<slug>.png(1536², cell 256², 6열).
// row: idle=0 / walk(정면 측보행)=1 / walk-south=2 / walk-east=3 / walk-north=4 / walk-west=5.
// idle 4프레임, walk 6프레임. 모든 캐릭터 동일 규격이라 manifest fetch 없이 상수로 친다.
const CELL = 256;
const SHEET_COLS = 6;
const TRIM_H = 210; // 캐릭터 실측 높이(여백 제외) — height 를 여기에 맞춰 스케일
const FOOT_GAP = CELL - 232; // pivot.y=232 → cell 하단 발 아래 여백(발을 컨테이너 바닥에 정렬)

// 4방향 스프라이트 — 통합 시트(sheet-<slug>.png) 있으면 그걸 row 매핑해 애니,
// 없으면 옛 4프레임 가로시트(walk-/front-/back- + idle-) 경로로 폴백, 그것도 없으면 초상.
export function SpriteWalk({ character, walking, width = 72, height = 100, flip = false, facing = 'front' }: SpriteWalkProps) {
  const slug = SLUG[character];
  const prefer = facing === 'front' ? `front-${slug}` : facing === 'back' ? `back-${slug}` : `walk-${slug}`;
  const [hasSheet, setHasSheet] = useState<boolean | null>(null); // 통합 시트 존재 여부
  const [file, setFile] = useState<string | null>(null); // 옛 폴백: 실제 로드된 walk 시트
  const [sheet, setSheet] = useState<{ fw: number; fh: number } | null>(null);
  const [idleFailed, setIdleFailed] = useState(false);

  // 통합 시트는 한 장으로 전방향 처리 → slug 만 의존(facing 바뀌어도 재로드 불필요).
  useEffect(() => {
    setHasSheet(null);
    if (!slug) return;
    let alive = true;
    const img = new Image();
    img.onload = () => { if (alive) setHasSheet(true); };
    img.onerror = () => { if (alive) setHasSheet(false); };
    img.src = `${BASE}assets/sheet-${slug}.png`;
    return () => { alive = false; };
  }, [slug]);

  // 옛 4프레임 가로시트 폴백 — 통합 시트가 없을 때만(hasSheet===false) 로드.
  useEffect(() => {
    setFile(null); setSheet(null); setIdleFailed(false);
    if (!slug || hasSheet !== false) return;
    let alive = true;
    const load = (name: string, fallback?: () => void) => {
      const img = new Image();
      img.onload = () => { if (alive) { setFile(name); setSheet({ fw: img.naturalWidth / 4, fh: img.naturalHeight }); } };
      img.onerror = () => { if (alive) fallback?.(); };
      img.src = `${BASE}assets/${name}.png`;
    };
    load(prefer, () => load(`walk-${slug}`)); // 방향 walk 시트 → 측면 폴백
    return () => { alive = false; };
  }, [slug, prefer, hasSheet]);

  const box: React.CSSProperties = { width, height, display: 'flex', alignItems: 'flex-end', justifyContent: 'center' };
  const sideFlip = facing === 'side' && flip;

  // 통합 시트 모드 — facing+walking → (row, frames). 정면 멈춤은 idle row 숨쉬기,
  // 그 외 멈춤은 해당 walk row 첫 프레임 정지. flip 은 west row(5)로 직접(미러 아님).
  if (hasSheet === true && slug) {
    const row = facing === 'front' ? (walking ? 2 : 0)
      : facing === 'back' ? 4
      : flip ? 5 : 3;
    const frames = facing === 'front' && !walking ? 4 : 6;
    const animate = walking || (facing === 'front' && !walking); // front idle 도 숨쉬기 루프
    const cellDisp = Math.round((height * CELL) / TRIM_H);
    const sheetDisp = cellDisp * SHEET_COLS;
    const footAdj = Math.round((FOOT_GAP * cellDisp) / CELL);
    const dur = frames === 4 ? 0.9 : 0.6; // idle 은 느리게
    return (
      <div style={{ ...box, overflow: 'visible' }}>
        <div
          style={{
            // box 가 flex 라 cellDisp(>box width)가 shrink 돼 우측 잘리던 것 — 안 줄게 고정.
            width: cellDisp, height: cellDisp, flexShrink: 0, marginBottom: -footAdj,
            backgroundImage: `url(${BASE}assets/sheet-${slug}.png)`,
            backgroundRepeat: 'no-repeat',
            backgroundSize: `${sheetDisp}px ${sheetDisp}px`,
            backgroundPositionX: 0,
            backgroundPositionY: `${-row * cellDisp}px`,
            imageRendering: 'pixelated',
            filter: OUTLINE,
            animation: animate ? `schale-walk ${dur}s steps(${frames}) infinite` : undefined,
            ['--walk-shift' as string]: `${-frames * cellDisp}px`,
          } as React.CSSProperties}
        />
      </div>
    );
  }

  // 멈춤 + idle 정적 에셋 있음 → 다리 모은 standing(단일). 측면만 flip.
  // 통합 시트 확인 끝나 없음(false)일 때만 — 확인 중(null)엔 404 폴백요청 안 날림.
  if (hasSheet === false && !walking && !idleFailed && slug) {
    return (
      <div style={box}>
        <img
          src={`${BASE}assets/idle-${facing}-${slug}.png`}
          alt={character}
          onError={() => setIdleFailed(true)}
          style={{
            maxWidth: '100%', maxHeight: '100%', objectFit: 'contain', objectPosition: 'center bottom',
            transform: sideFlip ? 'scaleX(-1)' : 'none', filter: OUTLINE, imageRendering: 'pixelated', display: 'block',
          }}
        />
      </div>
    );
  }

  // 걷기(또는 idle 에셋 없는 폴백) → walk 시트. walking 이면 4프레임 애니메이션, 아니면 첫 프레임 정지.
  if (hasSheet === false && slug && file && sheet) {
    const dispW = Math.round(height * (sheet.fw / sheet.fh));
    const useFlip = file === `walk-${slug}` && flip; // 측면 시트만 flip
    return (
      <div style={box}>
        <div
          style={{
            width: dispW, height,
            backgroundImage: `url(${BASE}assets/${file}.png)`,
            backgroundSize: `${dispW * 4}px ${height}px`,
            backgroundRepeat: 'no-repeat',
            backgroundPositionX: 0,
            backgroundPositionY: 'bottom',
            imageRendering: 'pixelated',
            transform: useFlip ? 'scaleX(-1)' : 'none',
            filter: OUTLINE,
            animation: walking ? 'schale-walk 0.6s steps(4) infinite' : undefined,
            ['--walk-shift' as string]: `${-dispW * 4}px`,
          } as React.CSSProperties}
        />
      </div>
    );
  }

  // 폴백 — 정면 초상(char-<slug>.png, 없으면 이니셜).
  return (
    <div style={box}>
      <SpritePortrait character={character} scale={3.4} />
    </div>
  );
}
