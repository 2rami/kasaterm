import { useEffect, useState } from 'react';

/** 캐릭터 초상 — `public/assets/char-<이름>.png` 가 있으면 그 도트 초상을, 없으면
 *  이름 이니셜 placeholder 로 폴백한다(거노가 캐릭터별 PNG 를 채우는 대로 자동 노출).
 *  props 는 munder 호환 유지. */
export interface SpritePortraitProps {
  character: string;
  scale?: number;
  background?: string;
}

const BASE = import.meta.env.BASE_URL || '/';

// 한글 파일명은 Rust 정적 서빙(percent-decode 미지원)에서 404 → ASCII 슬러그로.
const SLUG: Record<string, string> = {
  '아로나': 'arona', '유우카': 'yuuka', '시로코': 'shiroko',
  '아리스': 'arisu', '호시노': 'hoshino', '코하루': 'koharu',
};

export function SpritePortrait({ character, scale = 2, background = 'transparent' }: SpritePortraitProps) {
  const [failed, setFailed] = useState(false);
  useEffect(() => setFailed(false), [character]); // 캐릭터 바뀌면 다시 시도
  const initial = (character || '?').trim().charAt(0).toUpperCase() || '?';
  const slug = SLUG[character];
  const w = 20 * scale;
  const h = 28 * scale;

  if (slug && !failed) {
    return (
      <img
        src={`${BASE}assets/char-${slug}.png`}
        alt={character}
        onError={() => setFailed(true)}
        style={{
          width: w, height: h,
          objectFit: 'contain', objectPosition: 'center bottom',
          display: 'block', imageRendering: 'pixelated',
        }}
      />
    );
  }
  return (
    <div
      aria-label={character}
      style={{
        width: w, height: h, background,
        display: 'flex', alignItems: 'center', justifyContent: 'center',
        fontFamily: 'var(--cth-font-display)', fontSize: 10 * scale, lineHeight: 1,
        color: 'var(--cth-ink-700)', imageRendering: 'pixelated', userSelect: 'none',
      }}
    >
      {initial}
    </div>
  );
}
