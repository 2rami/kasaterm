import { useEffect, useState } from 'react';
import { SpritePortrait } from './SpritePortrait';

const BASE = import.meta.env.BASE_URL || '/';
// 방향별 시트: walk-<slug>(측면 워크)·front-<slug>·back-<slug>(걷기) + idle-<facing>-<slug>(정적 정지).
const SLUG: Record<string, string> = {
  '아로나': 'arona', '유우카': 'yuuka', '시로코': 'shiroko',
  '아리스': 'arisu', '호시노': 'hoshino', '코하루': 'koharu',
  '프라나': 'prana',
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

// 4방향 스프라이트 — 걸으면 walk-cycle 시트(steps 4), 멈추면 idle 정적(다리 모은 standing).
// 거노: 멈춤 이미지 다리 모은 앞뒤옆. idle 에셋 없으면 walk 시트 첫 프레임 정지로 폴백.
export function SpriteWalk({ character, walking, width = 72, height = 100, flip = false, facing = 'front' }: SpriteWalkProps) {
  const slug = SLUG[character];
  const prefer = facing === 'front' ? `front-${slug}` : facing === 'back' ? `back-${slug}` : `walk-${slug}`;
  const [file, setFile] = useState<string | null>(null); // 실제 로드된 walk 시트
  const [sheet, setSheet] = useState<{ fw: number; fh: number } | null>(null);
  const [idleFailed, setIdleFailed] = useState(false);

  useEffect(() => {
    setFile(null); setSheet(null); setIdleFailed(false);
    if (!slug) return;
    let alive = true;
    const load = (name: string, fallback?: () => void) => {
      const img = new Image();
      img.onload = () => { if (alive) { setFile(name); setSheet({ fw: img.naturalWidth / 4, fh: img.naturalHeight }); } };
      img.onerror = () => { if (alive) fallback?.(); };
      img.src = `${BASE}assets/${name}.png`;
    };
    load(prefer, () => load(`walk-${slug}`)); // 방향 walk 시트 → 측면 폴백
    return () => { alive = false; };
  }, [slug, prefer]);

  const box: React.CSSProperties = { width, height, display: 'flex', alignItems: 'flex-end', justifyContent: 'center' };
  const sideFlip = facing === 'side' && flip;

  // 멈춤 + idle 정적 에셋 있음 → 다리 모은 standing(단일). 측면만 flip.
  if (!walking && !idleFailed && slug) {
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
  if (slug && file && sheet) {
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
