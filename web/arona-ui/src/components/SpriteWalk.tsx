import { useEffect, useState } from 'react';
import { SpritePortrait } from './SpritePortrait';

const BASE = import.meta.env.BASE_URL || '/';
// 캐릭터 슬러그 — char-<slug>.png(픽셀 정면 스프라이트). 없으면 SpritePortrait 폴백.
const SLUG: Record<string, string> = {
  '아로나': 'arona', '유우카': 'yuuka', '시로코': 'shiroko',
  '아리스': 'arisu', '호시노': 'hoshino', '코하루': 'koharu',
};

export interface SpriteWalkProps {
  character: string;
  /** true 면 걷는 느낌(수직 bob), false 면 정지 정면. */
  walking: boolean;
  width?: number;
  height?: number;
  /** 진행 방향 — true 면 좌우 반전(걷는 중에만 적용). */
  flip?: boolean;
}

// 픽셀 스프라이트 — 항상 정면 char-<slug>.png. 멈추면 정면(거노: idle 정면), 걸으면
// 수직 bob(워크프레임 없이 이동감) + 방향 flip. imageRendering=pixelated 로 또렷한 픽셀.
export function SpriteWalk({ character, walking, width = 72, height = 100, flip = false }: SpriteWalkProps) {
  const slug = SLUG[character];
  const [failed, setFailed] = useState(false);
  useEffect(() => { setFailed(false); }, [character]);

  if (!slug || failed) {
    return (
      <div style={{ width, height, display: 'flex', alignItems: 'flex-end', justifyContent: 'center' }}>
        <SpritePortrait character={character} scale={3.4} />
      </div>
    );
  }
  return (
    <div style={{
      width, height, display: 'flex', alignItems: 'flex-end', justifyContent: 'center',
      animation: walking ? 'schale-bob 0.45s ease-in-out infinite' : undefined,
    }}>
      <img
        src={`${BASE}assets/char-${slug}.png`}
        alt={character}
        onError={() => setFailed(true)}
        style={{
          maxWidth: '100%', maxHeight: '100%', objectFit: 'contain', objectPosition: 'center bottom',
          transform: walking && flip ? 'scaleX(-1)' : 'none', display: 'block',
          // 흰 외곽선(4방향 drop-shadow) — 밝은 교실 바닥에서 캐릭터를 또렷하게(거노).
          filter: 'drop-shadow(1px 0 0 rgba(255,255,255,0.95)) drop-shadow(-1px 0 0 rgba(255,255,255,0.95)) drop-shadow(0 1px 0 rgba(255,255,255,0.95)) drop-shadow(0 -1px 0 rgba(255,255,255,0.95)) drop-shadow(0 5px 6px rgba(21,41,74,0.28))',
          imageRendering: 'pixelated',
        }}
      />
    </div>
  );
}
