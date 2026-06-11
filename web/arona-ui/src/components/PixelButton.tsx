import { CSSProperties, ReactNode, useState } from 'react';

// SCHALE 게임 UI 키트 버튼 — 라운드 + 상단 베벨 하이라이트 + 하단 입체 그림자 +
// 흰 외곽선 + 세로 그라데이션. (옛 픽셀 하드섀도 버튼 대체, 2026-06-11)
type Variant = 'primary' | 'secondary' | 'ghost' | 'destructive' | 'special' | 'purple';
type Size = 'sm' | 'md' | 'lg';

export interface PixelButtonProps {
  variant?: Variant;
  size?: Size;
  children?: ReactNode;
  onClick?: () => void;
  disabled?: boolean;
  fullWidth?: boolean;
  style?: CSSProperties;
}

const heightBySize: Record<Size, number> = { sm: 30, md: 38, lg: 48 };
const padBySize:    Record<Size, string> = { sm: '0 14px', md: '0 18px', lg: '0 24px' };
const radiusBySize: Record<Size, number> = { sm: 10, md: 13, lg: 16 };
const fontBySize:   Record<Size, number> = { sm: 13, md: 15, lg: 18 };

interface Pal { light: string; base: string; dark: string; text: string; border: string; }
const palettes: Record<Variant, Pal> = {
  primary:     { light: '#6BB0F0', base: '#4A90E2', dark: '#2E6BB8', text: '#ffffff', border: 'rgba(255,255,255,0.9)' },
  special:     { light: '#FFDD6E', base: '#F5C13B', dark: '#D49A1E', text: '#5A4205', border: 'rgba(255,255,255,0.9)' },
  purple:      { light: '#C29FF0', base: '#A77BE0', dark: '#7E52C0', text: '#ffffff', border: 'rgba(255,255,255,0.9)' },
  destructive: { light: '#FF9FA8', base: '#F56B7D', dark: '#D8485C', text: '#ffffff', border: 'rgba(255,255,255,0.9)' },
  secondary:   { light: '#FFFFFF', base: '#EFF6FD', dark: '#C3D8EE', text: '#25406B', border: '#D6E6F5' },
  ghost:       { light: 'transparent', base: 'transparent', dark: 'transparent', text: '#4A638F', border: '#D6E6F5' },
};

export function PixelButton({
  variant = 'primary',
  size = 'md',
  children,
  onClick,
  disabled = false,
  fullWidth = false,
  style
}: PixelButtonProps) {
  const [pressed, setPressed] = useState(false);
  const p = palettes[variant];
  const isGhost = variant === 'ghost';
  const lift = pressed && !disabled;
  const lightText = variant === 'primary' || variant === 'purple' || variant === 'destructive';

  return (
    <button
      onClick={disabled ? undefined : onClick}
      onMouseDown={() => setPressed(true)}
      onMouseUp={() => setPressed(false)}
      onMouseLeave={() => setPressed(false)}
      disabled={disabled}
      style={{
        height: heightBySize[size],
        padding: padBySize[size],
        borderRadius: radiusBySize[size],
        border: `2px solid ${p.border}`,
        background: isGhost
          ? (pressed ? 'var(--cth-cream-100)' : 'transparent')
          : `linear-gradient(180deg, ${p.light} 0%, ${p.base} 100%)`,
        color: p.text,
        boxShadow: isGhost
          ? 'none'
          : (lift
            ? `inset 0 1px 0 rgba(255,255,255,0.4), 0 1px 0 ${p.dark}`
            : `inset 0 2px 0 rgba(255,255,255,0.55), 0 3px 0 ${p.dark}, 0 5px 10px rgba(46,107,184,0.18)`),
        transform: lift ? 'translateY(2px)' : 'none',
        fontFamily: 'var(--cth-font-ui)', fontWeight: 700,
        fontSize: fontBySize[size],
        textShadow: lightText ? '0 1px 1px rgba(0,0,0,0.18)' : 'none',
        cursor: disabled ? 'not-allowed' : 'pointer',
        opacity: disabled ? 0.5 : 1,
        width: fullWidth ? '100%' : 'auto',
        whiteSpace: 'nowrap',
        userSelect: 'none',
        display: 'inline-flex', alignItems: 'center', justifyContent: 'center', gap: 6,
        transition: 'transform 60ms ease, box-shadow 60ms ease',
        ...style
      }}
    >
      {children}
    </button>
  );
}
