import { CSSProperties, ReactNode, useState } from 'react';

// SCHALE 게임 UI 키트 (Image #3 재현, 2026-06-11). 공통 인터랙션 컴포넌트 묶음:
// 세그먼트 탭 · ON/OFF 토글 · 난이도 뱃지 · 재화 칩 · 아이콘 버튼 · 상단 스탯 칩.

const GRAD = (l: string, b: string) => `linear-gradient(180deg, ${l} 0%, ${b} 100%)`;
const BEVEL = 'inset 0 2px 0 rgba(255,255,255,0.55)';
const WHITE_RING = '2px solid rgba(255,255,255,0.9)';

/* ── 세그먼트 탭 (임무 정보|적 정보, ALL|1T|2T…) ───────────────────────── */
export interface SegOption<T extends string> { value: T; label: ReactNode; }
export function SegmentedTabs<T extends string>({
  options, value, onChange, size = 'md', style
}: {
  options: SegOption<T>[];
  value: T;
  onChange: (v: T) => void;
  size?: 'sm' | 'md';
  style?: CSSProperties;
}) {
  return (
    <div style={{
      display: 'inline-flex', padding: 3, gap: 2,
      borderRadius: 12, background: '#fff',
      border: '1.5px solid var(--cth-cream-200)',
      boxShadow: '0 1px 3px rgba(21,41,74,0.06)',
      ...style
    }}>
      {options.map((o) => {
        const active = o.value === value;
        return (
          <button
            key={o.value}
            onClick={() => onChange(o.value)}
            style={{
              padding: size === 'sm' ? '5px 12px' : '7px 16px',
              borderRadius: 9, border: 'none', cursor: 'pointer',
              fontFamily: 'var(--cth-font-ui)', fontSize: size === 'sm' ? 12 : 13, fontWeight: 700,
              whiteSpace: 'nowrap',
              background: active ? GRAD('#6BB0F0', '#4A90E2') : 'transparent',
              color: active ? '#fff' : 'var(--cth-ink-300)',
              boxShadow: active ? `${BEVEL}, 0 2px 5px rgba(74,144,226,0.28)` : 'none',
              textShadow: active ? '0 1px 1px rgba(0,0,0,0.15)' : 'none',
              transition: 'color 100ms ease',
            }}
          >
            {o.label}
          </button>
        );
      })}
    </div>
  );
}

/* ── ON/OFF 토글 스위치 ───────────────────────────────────────────────── */
export function ToggleSwitch({ on, onChange, style }: {
  on: boolean;
  onChange: (v: boolean) => void;
  style?: CSSProperties;
}) {
  return (
    <button
      onClick={() => onChange(!on)}
      aria-pressed={on}
      style={{
        width: 72, height: 32, borderRadius: 999, border: WHITE_RING,
        background: on ? GRAD('#6BB0F0', '#4A90E2') : GRAD('#B8C4D4', '#92A1B5'),
        position: 'relative', cursor: 'pointer', padding: 0,
        boxShadow: `${BEVEL}, 0 2px 4px rgba(0,0,0,0.10)`,
        transition: 'background 150ms ease',
        ...style
      }}
    >
      <span style={{
        position: 'absolute', top: '50%', transform: 'translateY(-50%)',
        [on ? 'left' : 'right']: 11,
        fontFamily: 'var(--cth-font-ui)', fontSize: 11, fontWeight: 800, color: '#fff',
        textShadow: '0 1px 1px rgba(0,0,0,0.22)',
      }}>{on ? 'ON' : 'OFF'}</span>
      <span style={{
        position: 'absolute', top: '50%', transform: 'translateY(-50%)',
        [on ? 'right' : 'left']: 3,
        width: 24, height: 24, borderRadius: 999, background: '#fff',
        boxShadow: '0 1px 3px rgba(0,0,0,0.25)',
        transition: 'left 150ms ease, right 150ms ease',
      }} />
    </button>
  );
}

/* ── 난이도 뱃지 (NORMAL/HARD/VERY HARD) ──────────────────────────────── */
const DIFF: Record<string, { l: string; b: string }> = {
  NORMAL:      { l: '#6BB0F0', b: '#4A90E2' },
  HARD:        { l: '#C29FF0', b: '#A77BE0' },
  'VERY HARD': { l: '#FF9FB0', b: '#F56B8A' },
};
export function DifficultyBadge({ level, style }: { level: 'NORMAL' | 'HARD' | 'VERY HARD'; style?: CSSProperties }) {
  const c = DIFF[level];
  return (
    <span style={{
      display: 'inline-block', padding: '5px 14px', borderRadius: 999, border: WHITE_RING,
      background: GRAD(c.l, c.b), color: '#fff',
      fontFamily: 'var(--cth-font-ui)', fontSize: 13, fontWeight: 800, letterSpacing: 0.3,
      textShadow: '0 1px 1px rgba(0,0,0,0.2)',
      boxShadow: `${BEVEL}, 0 2px 5px rgba(0,0,0,0.12)`,
      ...style
    }}>{level}</span>
  );
}

/* ── 재화 칩 (아이콘 + 수량) ──────────────────────────────────────────── */
export function CurrencyChip({ icon, amount, style }: {
  icon: ReactNode;
  amount: ReactNode;
  style?: CSSProperties;
}) {
  return (
    <div style={{
      display: 'inline-flex', alignItems: 'center', gap: 6,
      padding: '4px 12px 4px 8px', borderRadius: 10,
      background: '#fff', border: '1.5px solid var(--cth-cream-200)',
      boxShadow: '0 1px 3px rgba(21,41,74,0.06)',
      ...style
    }}>
      {icon}
      <span style={{
        fontFamily: 'var(--cth-font-ui)', fontSize: 14, fontWeight: 700,
        color: 'var(--cth-ink-900)', fontVariantNumeric: 'tabular-nums',
      }}>{amount}</span>
    </div>
  );
}

/* ── 아이콘 버튼 (흰 라운드 사각 + 알림 점) ───────────────────────────── */
export function GameIconButton({ children, onClick, dot, title, style }: {
  children: ReactNode;
  onClick?: () => void;
  dot?: boolean;
  title?: string;
  style?: CSSProperties;
}) {
  const [hover, setHover] = useState(false);
  return (
    <button
      title={title}
      onClick={onClick}
      onMouseEnter={() => setHover(true)}
      onMouseLeave={() => setHover(false)}
      style={{
        position: 'relative', width: 38, height: 38, borderRadius: 11,
        border: '1.5px solid var(--cth-cream-200)', cursor: 'pointer',
        background: hover ? 'var(--cth-cream-100)' : '#fff',
        boxShadow: hover ? '0 2px 6px rgba(21,41,74,0.10)' : '0 1px 3px rgba(21,41,74,0.06)',
        display: 'inline-flex', alignItems: 'center', justifyContent: 'center',
        color: 'var(--cth-ink-500)',
        transition: 'background 100ms ease, box-shadow 100ms ease',
        ...style
      }}
    >
      {children}
      {dot && (
        <span style={{
          position: 'absolute', top: 6, right: 6, width: 8, height: 8,
          borderRadius: 999, background: 'var(--cth-coral)', border: '1.5px solid #fff',
        }} />
      )}
    </button>
  );
}

/* ── 상단바 스탯 칩 (번개/골드/크리스탈 + +버튼) ──────────────────────── */
export function StatPill({ icon, value, onPlus, style }: {
  icon: ReactNode;
  value: ReactNode;
  onPlus?: () => void;
  style?: CSSProperties;
}) {
  return (
    <div style={{
      display: 'inline-flex', alignItems: 'center', gap: 7,
      padding: '3px 4px 3px 10px', borderRadius: 999,
      background: '#fff', border: '1.5px solid var(--cth-cream-200)',
      boxShadow: '0 1px 2px rgba(21,41,74,0.05)',
      ...style
    }}>
      {icon}
      <span style={{
        fontFamily: 'var(--cth-font-ui)', fontSize: 14, fontWeight: 700,
        color: 'var(--cth-ink-900)', fontVariantNumeric: 'tabular-nums',
      }}>{value}</span>
      {onPlus && (
        <button onClick={onPlus} style={{
          width: 20, height: 20, borderRadius: 999, border: 'none', cursor: 'pointer',
          background: GRAD('#6BB0F0', '#4A90E2'), color: '#fff',
          fontFamily: 'var(--cth-font-ui)', fontSize: 14, fontWeight: 700, lineHeight: 1,
          display: 'inline-flex', alignItems: 'center', justifyContent: 'center',
          boxShadow: 'inset 0 1px 0 rgba(255,255,255,0.5)',
        }}>+</button>
      )}
    </div>
  );
}
