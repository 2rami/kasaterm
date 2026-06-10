import { useEffect, useRef, useState } from 'react';
import { PixelButton } from './PixelButton';

export interface FooterProps {
  onManage?: () => void;
  onNewRequest?: () => void;
  credits?: number;
  gold?: number;
  affinityLv?: number;
  exp?: number;
  expToNext?: number;
}

const PLACEHOLDER_CREDITS = 12_480;
const PLACEHOLDER_GOLD    = 8_320_100;
const EXP_PER_LEVEL       = 100;

function fmt(n: number): string {
  return n.toLocaleString('ko-KR');
}

// 값이 바뀔 때 이전값→새값으로 부드럽게 굴러가는 카운터 (재화 증가 체감).
function useCountUp(target: number, ms = 600): number {
  const [val, setVal] = useState(target);
  const fromRef = useRef(target);
  const rafRef = useRef(0);
  useEffect(() => {
    const from = fromRef.current;
    if (from === target) return;
    const start = performance.now();
    cancelAnimationFrame(rafRef.current);
    const step = (now: number) => {
      const t = Math.min(1, (now - start) / ms);
      const eased = 1 - Math.pow(1 - t, 3);
      setVal(Math.round(from + (target - from) * eased));
      if (t < 1) rafRef.current = requestAnimationFrame(step);
      else fromRef.current = target;
    };
    rafRef.current = requestAnimationFrame(step);
    return () => cancelAnimationFrame(rafRef.current);
  }, [target, ms]);
  return val;
}

function GemIcon() {
  return (
    <svg width="15" height="15" viewBox="0 0 16 16" style={{ flexShrink: 0 }}>
      <path d="M5 2h6l3 4-6 8-6-8 3-4Z" fill="var(--cth-sky)" stroke="var(--cth-ink-900)" strokeWidth="1.2" strokeLinejoin="round" />
      <path d="M2 6h12M8 2v0M5 2 8 14M11 2 8 14" fill="none" stroke="var(--cth-ink-900)" strokeWidth="0.9" opacity="0.55" />
      <path d="M5 2 4 6h2L5 2Z" fill="var(--cth-sky-light)" />
    </svg>
  );
}
function CoinIcon() {
  return (
    <svg width="15" height="15" viewBox="0 0 16 16" style={{ flexShrink: 0 }}>
      <circle cx="8" cy="8" r="6.2" fill="var(--cth-lemon)" stroke="var(--cth-ink-900)" strokeWidth="1.2" />
      <circle cx="8" cy="8" r="4" fill="none" stroke="var(--cth-ink-900)" strokeWidth="0.9" opacity="0.5" />
      <path d="M8 5.5v5M6.5 8h3" stroke="var(--cth-ink-900)" strokeWidth="1.1" opacity="0.7" />
      <circle cx="6" cy="6" r="1" fill="var(--cth-lemon-light)" />
    </svg>
  );
}
function HeartIcon({ size = 13 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" style={{ flexShrink: 0 }}>
      <path d="M8 13.5 2.5 8a3 3 0 0 1 4.2-4.2L8 5l1.3-1.2A3 3 0 0 1 13.5 8L8 13.5Z"
        fill="var(--cth-coral)" stroke="var(--cth-ink-900)" strokeWidth="1.2" strokeLinejoin="round" />
    </svg>
  );
}

function Currency({ icon, value }: { icon: React.ReactNode; value: number }) {
  const shown = useCountUp(value);
  return (
    <div style={{ display: 'flex', alignItems: 'center', gap: 5 }}>
      {icon}
      <span style={{
        fontFamily: 'var(--cth-font-display)', fontSize: 8,
        color: 'var(--cth-ink-900)', fontVariantNumeric: 'tabular-nums',
      }}>{fmt(shown)}</span>
    </div>
  );
}

export function Footer({
  onManage,
  onNewRequest,
  credits = PLACEHOLDER_CREDITS,
  gold    = PLACEHOLDER_GOLD,
  affinityLv = 23,
  exp = 0,
  expToNext = EXP_PER_LEVEL,
}: FooterProps) {
  const pct = Math.max(0, Math.min(100, Math.round((exp / Math.max(1, expToNext)) * 100)));
  return (
    <div style={{
      display: 'flex', alignItems: 'center', gap: 12,
      padding: '0 12px', height: 48, boxSizing: 'border-box'
    }}>
      {/* 학생 관리 */}
      <button
        onClick={onManage}
        style={{
          fontFamily: 'var(--cth-font-display)', fontSize: 7,
          padding: '5px 10px', border: 'none', cursor: 'pointer',
          background: 'var(--cth-cream-200)', color: 'var(--cth-ink-900)',
          boxShadow: 'inset 0 0 0 1px var(--cth-ink-900)', whiteSpace: 'nowrap'
        }}
      >
        학생 관리
      </button>

      {/* 인연 레벨 + EXP 진행바 */}
      <div style={{ display: 'flex', alignItems: 'center', gap: 7, minWidth: 0 }}>
        <HeartIcon />
        <span style={{
          fontFamily: 'var(--cth-font-display)', fontSize: 7,
          color: 'var(--cth-ink-900)', whiteSpace: 'nowrap',
        }}>인연 Lv.{affinityLv}</span>
        <div title={`EXP ${exp} / ${expToNext}`} style={{
          position: 'relative', width: 96, height: 9,
          background: 'var(--cth-cream-200)',
          boxShadow: 'inset 0 0 0 1px var(--cth-ink-900)', flexShrink: 0,
        }}>
          <div style={{
            position: 'absolute', inset: '1px',
            width: `calc(${pct}% - 2px)`,
            background: 'var(--cth-coral)',
            transition: 'width 0.5s cubic-bezier(0.22,1,0.36,1)',
          }} />
        </div>
        <span style={{
          fontFamily: 'var(--cth-font-ui)', fontSize: 10,
          color: 'var(--cth-ink-500)', whiteSpace: 'nowrap',
        }}>{pct}%</span>
      </div>

      <div style={{ flex: 1 }} />

      {/* 재화 */}
      <div style={{ display: 'flex', alignItems: 'center', gap: 16, flexShrink: 0 }}>
        <Currency icon={<GemIcon />} value={credits} />
        <Currency icon={<CoinIcon />} value={gold} />
      </div>

      {/* CTA */}
      <PixelButton variant="primary" size="sm" onClick={onNewRequest}>
        + 새 의뢰 작성
      </PixelButton>
    </div>
  );
}
