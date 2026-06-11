import { useEffect, useState } from 'react';
import { SpritePortrait } from './SpritePortrait';

const BASE = import.meta.env.BASE_URL || '/';
// 워크 프레임이 있는 캐릭터 슬러그(codex 워크시트 → sheet_to_frames.py 로 추출).
// 나머지는 정적 SpritePortrait 폴백(onError 로도 자동 폴백).
const SLUG: Record<string, string> = {
  '아로나': 'arona', '유우카': 'yuuka', '시로코': 'shiroko',
  '아리스': 'arisu', '호시노': 'hoshino', '코하루': 'koharu',
};
const FRAMES = 4;

export interface SpriteWalkProps {
  character: string;
  /** true 면 4프레임 순환(걷는 중), false 면 0프레임 고정(서있음). */
  walking: boolean;
  width?: number;
  height?: number;
  /** 진행 방향 — true 면 좌우 반전(왼쪽 보기). */
  flip?: boolean;
}

// 워크사이클 스프라이트 — walk-<slug>-<0..3>.png 를 순환. 프레임 없으면 정적 폴백.
export function SpriteWalk({ character, walking, width = 72, height = 100, flip = false }: SpriteWalkProps) {
  const slug = SLUG[character];
  const [frame, setFrame] = useState(0);
  const [failed, setFailed] = useState(false);

  useEffect(() => { setFailed(false); setFrame(0); }, [character]);
  useEffect(() => {
    if (!walking || failed) return;
    const iv = setInterval(() => setFrame((f) => (f + 1) % FRAMES), 150);
    return () => clearInterval(iv);
  }, [walking, failed]);

  if (!slug || failed) {
    return (
      <div style={{ width, height, display: 'flex', alignItems: 'flex-end', justifyContent: 'center' }}>
        <SpritePortrait character={character} scale={3.4} />
      </div>
    );
  }
  return (
    <img
      src={`${BASE}assets/walk-${slug}-${walking ? frame : 0}.png`}
      alt={character}
      onError={() => setFailed(true)}
      style={{
        width, height, objectFit: 'contain', objectPosition: 'center bottom',
        transform: flip ? 'scaleX(-1)' : 'none', display: 'block',
        filter: 'drop-shadow(0 4px 6px rgba(21,41,74,0.2))',
        imageRendering: 'auto',
      }}
    />
  );
}
