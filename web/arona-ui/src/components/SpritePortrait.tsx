import { useEffect, useState } from 'react';
import { characterFaceUrl, characterStandingUrl } from '@/lib/mcp';
import { useCharacterSlug } from '@/lib/sprites';

export interface SpritePortraitProps {
  character: string;
  scale?: number;
  background?: string;
  bust?: boolean;
}

export function SpritePortrait({ character, scale = 2, background = 'transparent', bust = false }: SpritePortraitProps) {
  const ref = useCharacterSlug(character);
  const slug = ref?.slug;
  const [step, setStep] = useState(0);
  useEffect(() => setStep(0), [character, slug, bust, ref?.theme]);

  const raw = (character || '').trim();
  // 캐릭터명 해석 실패 시 백엔드가 pane id('%3')·숫자를 넘긴다 → 이니셜이 '%'/숫자로 떠
  // 깨져 보였다(사용자: 프사에 %). 그런 값은 사람 실루엣으로 폴백.
  const isPaneId = raw === '' || /^%?\d+$/.test(raw);
  const initial = isPaneId ? '' : raw.charAt(0).toUpperCase();
  const w = 20 * scale;
  // 대화창(bust)은 프로필 사진처럼 정사각 상반신, 교실/워크는 전신 도트(사용자).
  const h = bust ? 20 * scale : 28 * scale;

  if (slug && step < (bust ? 1 : 2)) {
    const face = bust || step === 1;
    const src = face ? characterFaceUrl(slug, ref.theme) : characterStandingUrl(slug, ref.theme);
    return (
      <img
        key={src}
        src={src}
        alt={character}
        onError={() => setStep((current) => current + 1)}
        style={{
          width: w, height: h,
          objectFit: 'contain',
          objectPosition: face ? 'center' : 'center bottom',
          display: 'block',
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
      {initial || (
        <svg width={12 * scale} height={12 * scale} viewBox="0 0 24 24" fill="var(--cth-ink-300)" aria-hidden style={{ opacity: 0.55 }}>
          <circle cx="12" cy="8.5" r="4.2" />
          <path d="M3.5 21c0-4.7 3.8-8.5 8.5-8.5s8.5 3.8 8.5 8.5z" />
        </svg>
      )}
    </div>
  );
}
