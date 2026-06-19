import { useEffect, useRef, useState } from 'react';
import { CurrencyChip } from './GameKit';

export interface FooterProps {
  /** 💎 크리스탈 = 전 학생 누적 입력 토큰. */
  inputTokens?: number;
  /** 🪙 골드 = 전 학생 누적 비용(USD). */
  costUsd?: number;
  /** 인연(호감도) = 전 학생 컨텍스트 사용량 % (거노: 인연=컨텍스트량). */
  contextPct?: number;
}

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

function Currency({ icon, value, money }: { icon: React.ReactNode; value: number; money?: boolean }) {
  // 비용($)은 소수라 ×10000 정수로 count-up 후 환산. 토큰은 정수 그대로 굴린다.
  const shown = useCountUp(money ? Math.round(value * 10000) : value);
  const text = money ? '$' + (shown / 10000).toFixed(2) : fmt(shown);
  return <CurrencyChip icon={icon} amount={text} />;
}

export function Footer({
  inputTokens = 0,
  costUsd = 0,
  contextPct = 0,
}: FooterProps) {
  const pct = Math.max(0, Math.min(100, Math.round(contextPct)));
  return (
    <div style={{
      display: 'flex', alignItems: 'center', gap: 12,
      padding: '0 12px', height: 48, boxSizing: 'border-box'
    }}>
      {/* 인연(호감도) = 전 학생 컨텍스트 사용량 % */}
      <div style={{ display: 'flex', alignItems: 'center', gap: 7, minWidth: 0 }}>
        <HeartIcon />
        <span style={{
          fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: 600,
          color: 'var(--cth-ink-700)', whiteSpace: 'nowrap',
        }}>인연</span>
        <div title={`컨텍스트 사용량 ${pct}%`} style={{
          position: 'relative', width: 110, height: 10, borderRadius: 999,
          background: 'var(--cth-cream-200)', overflow: 'hidden', flexShrink: 0,
        }}>
          <div style={{
            position: 'absolute', inset: 0,
            width: `${pct}%`,
            background: 'linear-gradient(90deg, #FF8FB1, #FF6B6B)',
            borderRadius: 999,
            transition: 'width 0.5s cubic-bezier(0.22,1,0.36,1)',
          }} />
        </div>
        <span style={{
          fontFamily: 'var(--cth-font-ui)', fontSize: 10,
          color: 'var(--cth-ink-500)', whiteSpace: 'nowrap',
        }}>{pct}%</span>
      </div>

      <div style={{ flex: 1 }} />

      {/* 재화 = claude 토큰 지표 (💎입력 토큰 · 🪙누적 비용$) */}
      <div style={{ display: 'flex', alignItems: 'center', gap: 12, flexShrink: 0 }}>
        <Currency icon={<GemIcon />} value={inputTokens} />
        <Currency icon={<CoinIcon />} value={costUsd} money />
      </div>
    </div>
  );
}
