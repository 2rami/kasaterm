/** 캐릭터 초상 — munder 의 SpritePortrait 는 LimeZu 스프라이트(portraitArt)를
 *  recolor 해 그렸는데 그 아트는 차용 금지라, 여기선 **placeholder 픽셀 블록**
 *  으로 대체한다(이름 이니셜 + 픽셀 보더). 오리지널 도트 초상은 후속(거노가 채움).
 *  props 는 munder 와 호환되게 유지해 교체 시 이 파일만 갈아끼우면 된다. */
export interface SpritePortraitProps {
  character: string;
  scale?: number;
  background?: string;
}

export function SpritePortrait({ character, scale = 2, background = 'transparent' }: SpritePortraitProps) {
  const initial = (character || '?').trim().charAt(0).toUpperCase() || '?';
  return (
    <div
      aria-label={character}
      style={{
        width: 20 * scale,
        height: 28 * scale,
        background,
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        fontFamily: 'var(--cth-font-display)',
        fontSize: 10 * scale,
        lineHeight: 1,
        color: 'var(--cth-ink-700)',
        imageRendering: 'pixelated',
        userSelect: 'none'
      }}
    >
      {initial}
    </div>
  );
}
