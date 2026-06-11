import { CSSProperties, useState } from 'react';
import { StatPill } from './GameKit';

const APP_VERSION = 'v0.2.4';

interface IconBtnProps {
  title: string;
  badge?: number;
  onClick?: () => void;
  children: React.ReactNode;
}

function IconBtn({ title, badge, onClick, children }: IconBtnProps) {
  const [hover, setHover] = useState(false);
  return (
    <button
      title={title}
      onClick={onClick}
      onMouseEnter={() => setHover(true)}
      onMouseLeave={() => setHover(false)}
      style={{
        position: 'relative',
        width: 26, height: 26,
        display: 'inline-flex', alignItems: 'center', justifyContent: 'center',
        border: 'none', cursor: 'pointer',
        background: hover ? 'var(--cth-sky-light)' : 'transparent',
        borderRadius: 7,
        color: 'var(--cth-ink-500)',
      }}
    >
      {children}
      {badge != null && badge > 0 && (
        <span style={{
          position: 'absolute', top: -2, right: -2,
          minWidth: 13, height: 13, padding: '0 2px',
          boxSizing: 'border-box',
          background: 'var(--cth-coral)',
          color: '#fff',
          fontFamily: 'var(--cth-font-ui)', fontSize: 8, fontWeight: 700,
          borderRadius: 999,
          lineHeight: '13px', textAlign: 'center',
        }}>{badge > 9 ? '9+' : badge}</span>
      )}
    </button>
  );
}

const stroke: CSSProperties = { fill: 'none', stroke: 'currentColor', strokeWidth: 1.6 } as CSSProperties;

function BellIcon() {
  return (
    <svg width="15" height="15" viewBox="0 0 16 16">
      <path style={stroke} d="M4 7a4 4 0 0 1 8 0c0 3 1 4 1 4H3s1-1 1-4Z" />
      <path style={stroke} d="M6.5 13a1.5 1.5 0 0 0 3 0" />
    </svg>
  );
}
function MailIcon() {
  return (
    <svg width="15" height="15" viewBox="0 0 16 16">
      <rect style={stroke} x="2.5" y="3.5" width="11" height="9" />
      <path style={stroke} d="m2.5 4.5 5.5 4 5.5-4" />
    </svg>
  );
}
function GearIcon() {
  return (
    <svg width="15" height="15" viewBox="0 0 16 16">
      <circle style={stroke} cx="8" cy="8" r="2.2" />
      <path style={stroke} d="M8 1.5v2M8 12.5v2M1.5 8h2M12.5 8h2M3.4 3.4l1.4 1.4M11.2 11.2l1.4 1.4M12.6 3.4l-1.4 1.4M4.8 11.2l-1.4 1.4" />
    </svg>
  );
}
function BoltIcon() {
  return (
    <svg width="14" height="14" viewBox="0 0 16 16" style={{ flexShrink: 0 }}>
      <path d="M9.2 1.3 3.3 9.2H7l-.9 5.5 6-8.1H8.4l.8-5.3Z" fill="#FFC83D" stroke="var(--cth-ink-900)" strokeWidth="0.7" strokeLinejoin="round" />
    </svg>
  );
}
const fmtTokens = (n: number) => (n >= 1000 ? `${Math.round(n / 1000)}k` : String(n));

export interface TitleBarProps {
  notifications?: number;
  mail?: number;
  /** ⚡ 번개 = 전 학생 총 컨텍스트 토큰(재화 치환). */
  contextTokens?: number;
  onBell?: () => void;
  onMail?: () => void;
  onSettings?: () => void;
}

export function TitleBar({ notifications = 0, mail = 0, contextTokens = 0, onBell, onMail, onSettings }: TitleBarProps) {
  return (
    <div style={{
      display: 'flex', alignItems: 'center', gap: 8,
      height: 36, padding: '0 12px', flexShrink: 0, boxSizing: 'border-box',
      background: 'linear-gradient(180deg, var(--cth-sky-light), var(--cth-cream-50))',
      borderBottom: '1px solid var(--cth-cream-200)',
    }}>
      {/* 로고 마크 — SCHALE 삼각 심볼 */}
      <svg width="20" height="20" viewBox="0 0 20 20" style={{ flexShrink: 0 }}>
        <path d="M10 2.5 17.5 16H2.5L10 2.5Z" fill="none" stroke="var(--cth-sky)" strokeWidth="1.7" strokeLinejoin="round" />
        <circle cx="10" cy="11.5" r="2.3" fill="var(--cth-sky)" />
      </svg>

      <span style={{
        fontFamily: 'var(--cth-font-display)', fontSize: 14, fontWeight: 800,
        letterSpacing: 0.5,
        color: 'var(--cth-ink-900)', whiteSpace: 'nowrap',
      }}>SCHALE OS</span>

      <span style={{
        fontFamily: 'var(--cth-font-ui)', fontSize: 11,
        color: 'var(--cth-ink-500)', whiteSpace: 'nowrap',
      }}>{APP_VERSION} · Sensei Mode</span>

      <div style={{ flex: 1 }} />

      {contextTokens > 0 && (
        <StatPill icon={<BoltIcon />} value={fmtTokens(contextTokens)} style={{ marginRight: 4 }} />
      )}
      <IconBtn title="알림" badge={notifications} onClick={onBell}><BellIcon /></IconBtn>
      <IconBtn title="메일" badge={mail} onClick={onMail}><MailIcon /></IconBtn>
      <IconBtn title="설정" onClick={onSettings}><GearIcon /></IconBtn>
    </div>
  );
}
