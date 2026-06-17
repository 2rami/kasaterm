import { useEffect, useState } from 'react';

/** 캐릭터 초상 — `public/assets/char-<이름>.png` 가 있으면 그 도트 초상을, 없으면
 *  이름 이니셜 placeholder 로 폴백한다(거노가 캐릭터별 PNG 를 채우는 대로 자동 노출).
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

// 한글 파일명은 Rust 정적 서빙(percent-decode 미지원)에서 404 → ASCII 슬러그로.
const SLUG: Record<string, string> = {
  // god: 아로나·프라나. 학생(밀레니엄 게임개발부): 미도리·모모이·유즈·아리스(거노 2026-06-17).
  '아로나': 'arona', '프라나': 'prana',
  '미도리': 'midori', '모모이': 'momoi', '유즈': 'yuzu', '아리스': 'arisu',
  // 구 로스터(호환) — 에셋 남아 있으면 계속 표시.
  '유우카': 'yuuka', '시로코': 'shiroko', '호시노': 'hoshino', '코하루': 'koharu',
};

export function SpritePortrait({ character, scale = 2, background = 'transparent', bust = false }: SpritePortraitProps) {
  const [failed, setFailed] = useState(false);
  useEffect(() => setFailed(false), [character, bust]); // 캐릭터/모드 바뀌면 다시 시도
  const initial = (character || '?').trim().charAt(0).toUpperCase() || '?';
  const slug = SLUG[character];
  const w = 20 * scale;
  // 대화창(bust)은 프로필 사진처럼 정사각 상반신, 교실/워크는 전신 도트(거노).
  const h = bust ? 20 * scale : 28 * scale;
  const objPos = bust ? 'center center' : 'center bottom';

  if (slug && !failed) {
    const src = bust ? `${BASE}assets/char-${slug}-bust.png` : `${BASE}assets/char-${slug}.png`;
    return (
      <img
        src={src}
        alt={character}
        onError={(e) => {
          // bust 이미지(생성 전)면 전신 도트로 1회 폴백 — 깨지지 않게. 그것도 없으면 이니셜.
          const img = e.currentTarget;
          if (bust && img.dataset.fellback !== '1') {
            img.dataset.fellback = '1';
            img.src = `${BASE}assets/char-${slug}.png`;
          } else {
            setFailed(true);
          }
        }}
        style={{
          width: w, height: h,
          objectFit: 'contain', objectPosition: objPos,
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
