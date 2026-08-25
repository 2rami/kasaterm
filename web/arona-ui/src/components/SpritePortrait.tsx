import { useEffect, useState } from 'react';
import { characterFaceUrl } from '@/lib/mcp';
import { useCharacterSlug } from '@/lib/sprites';

/** 캐릭터 초상. 세 단계로 내려간다 — 번들 도트 초상 → 테마 얼굴 → 이름 이니셜.
 *  props 는 munder 호환 유지. */
export interface SpritePortraitProps {
  character: string;
  scale?: number;
  background?: string;
  /** 대화창(모모톡 등)용 — 프로필 사진처럼 정사각 상반신(char-<slug>-bust.png).
   *  없으면 전신 도트로 폴백. 교실/워크는 false(전신) 유지(거노). */
  bust?: boolean;
}

const BASE = import.meta.env.BASE_URL || '/';

// 번들에 실제로 도트 초상 파일이 있는 학생. 로스터는 79명이지만 그림은 이 12명뿐이라,
// 나머지를 여기 넣으면 매 렌더마다 404 를 두 번 맞고 나서야 테마 얼굴로 간다.
const BUNDLED = new Set([
  'arona', 'prana', 'midori', 'momoi', 'yuzu', 'arisu',
  'yuuka', 'shiroko', 'hoshino', 'koharu', 'himari', 'aru',
]);

// 0 번들 상반신 · 1 번들 전신 · 2 테마 얼굴(/character-face) · 3 이니셜.
const enum Step { Bust, Full, Face, Initial }

export function SpritePortrait({ character, scale = 2, background = 'transparent', bust = false }: SpritePortraitProps) {
  const ref = useCharacterSlug(character);
  const slug = ref?.slug;
  const bundled = !!slug && !ref?.theme && BUNDLED.has(slug);
  const first = bundled ? (bust ? Step.Bust : Step.Full) : Step.Face;
  const [step, setStep] = useState<Step>(first);
  // 캐릭터·모드가 바뀌면(또는 로스터가 늦게 도착해 slug 가 생기면) 처음부터 다시 시도.
  useEffect(() => setStep(first), [character, bust, first]);

  const raw = (character || '').trim();
  // 캐릭터명 해석 실패 시 백엔드가 pane id('%3')·숫자를 넘긴다 → 이니셜이 '%'/숫자로 떠
  // 깨져 보였다(거노: 프사에 %). 그런 값은 사람 실루엣으로 폴백.
  const isPaneId = raw === '' || /^%?\d+$/.test(raw);
  const initial = isPaneId ? '' : raw.charAt(0).toUpperCase();
  const w = 20 * scale;
  // 대화창(bust)은 프로필 사진처럼 정사각 상반신, 교실/워크는 전신 도트(거노).
  const h = bust ? 20 * scale : 28 * scale;

  if (slug && step !== Step.Initial) {
    // 테마 얼굴은 도트가 아니라 사진에 가까워서, 상자를 채우고(cover) 픽셀 확대를 안 건다.
    const face = step === Step.Face;
    const src = face
      ? characterFaceUrl(slug, ref?.theme)
      : `${BASE}assets/char-${slug}${step === Step.Bust ? '-bust' : ''}.png`;
    return (
      <img
        key={src}
        src={src}
        alt={character}
        onError={() => setStep((s) => s + 1)}
        style={{
          width: w, height: h,
          objectFit: face ? 'cover' : 'contain',
          objectPosition: bust || face ? 'center center' : 'center bottom',
          display: 'block',
          imageRendering: face ? 'auto' : 'pixelated',
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
