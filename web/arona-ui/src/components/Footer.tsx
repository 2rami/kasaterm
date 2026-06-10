import { PixelButton } from './PixelButton';

export interface FooterProps {
  onManage?: () => void;
  onNewRequest?: () => void;
  credits?: number;
  gold?: number;
  bondBonus?: string;
}

const PLACEHOLDER_CREDITS = 12_480;
const PLACEHOLDER_GOLD    = 8_320_100;

function fmt(n: number): string {
  return n.toLocaleString('ko-KR');
}

function CoinIcon({ label, bg }: { label: string; bg: string }) {
  return (
    <span style={{
      width: 16, height: 16,
      background: bg,
      boxShadow: 'inset 0 0 0 1px var(--cth-ink-900)',
      display: 'inline-flex', alignItems: 'center', justifyContent: 'center',
      fontFamily: 'var(--cth-font-display)', fontSize: 7,
      color: 'var(--cth-ink-900)', flexShrink: 0
    }}>{label}</span>
  );
}

export function Footer({
  onManage,
  onNewRequest,
  credits = PLACEHOLDER_CREDITS,
  gold    = PLACEHOLDER_GOLD,
  bondBonus = '장전 관리부 +12%'
}: FooterProps) {
  return (
    <div style={{
      display: 'flex', alignItems: 'center', gap: 10,
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

      {/* 인연 레벨 보너스 */}
      <div style={{
        fontFamily: 'var(--cth-font-ui)', fontSize: 10,
        color: 'var(--cth-ink-500)', whiteSpace: 'nowrap',
        overflow: 'hidden', textOverflow: 'ellipsis', maxWidth: 200
      }}>
        인연 레벨 보너스 · {bondBonus}
      </div>

      <div style={{ flex: 1 }} />

      {/* 재화 */}
      <div style={{ display: 'flex', alignItems: 'center', gap: 14, flexShrink: 0 }}>
        <div style={{ display: 'flex', alignItems: 'center', gap: 5 }}>
          <CoinIcon label="C" bg="var(--cth-sky)" />
          <span style={{
            fontFamily: 'var(--cth-font-display)', fontSize: 8,
            color: 'var(--cth-ink-900)'
          }}>{fmt(credits)}</span>
        </div>
        <div style={{ display: 'flex', alignItems: 'center', gap: 5 }}>
          <CoinIcon label="G" bg="var(--cth-lemon)" />
          <span style={{
            fontFamily: 'var(--cth-font-display)', fontSize: 8,
            color: 'var(--cth-ink-900)'
          }}>{fmt(gold)}</span>
        </div>
      </div>

      {/* CTA */}
      <PixelButton variant="primary" size="sm" onClick={onNewRequest}>
        + 새 의뢰 작성
      </PixelButton>
    </div>
  );
}
