import { CSSProperties, useState } from 'react';

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
        boxShadow: hover ? 'inset 0 0 0 1px var(--cth-ink-900)' : 'none',
        color: 'var(--cth-ink-700)',
      }}
    >
      {children}
      {badge != null && badge > 0 && (
        <span style={{
          position: 'absolute', top: -2, right: -2,
          minWidth: 13, height: 13, padding: '0 2px',
          boxSizing: 'border-box',
          background: 'var(--cth-coral)',
          color: 'var(--cth-cream-50)',
          fontFamily: 'var(--cth-font-display)', fontSize: 7,
          lineHeight: '13px', textAlign: 'center',
          boxShadow: 'inset 0 0 0 1px var(--cth-ink-900)',
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

export interface TitleBarProps {
  notifications?: number;
  mail?: number;
  onBell?: () => void;
  onMail?: () => void;
  onSettings?: () => void;
}

export function TitleBar({ notifications = 0, mail = 0, onBell, onMail, onSettings }: TitleBarProps) {
  return (
    <div style={{
      display: 'flex', alignItems: 'center', gap: 8,
      height: 30, padding: '0 10px', flexShrink: 0, boxSizing: 'border-box',
      background: 'linear-gradient(var(--cth-sky-light), var(--cth-cream-100))',
      borderBottom: '2px solid var(--cth-ink-900)',
    }}>
      {/* 로고 마크 */}
      <span style={{
        width: 18, height: 18, flexShrink: 0,
        display: 'inline-flex', alignItems: 'center', justifyContent: 'center',
        background: 'var(--cth-ink-900)', color: 'var(--cth-sky)',
        fontFamily: 'var(--cth-font-display)', fontSize: 8,
      }}>A</span>

      <span style={{
        fontFamily: 'var(--cth-font-display)', fontSize: 8,
        color: 'var(--cth-ink-900)', whiteSpace: 'nowrap',
      }}>SCHALE OS</span>

      <span style={{
        fontFamily: 'var(--cth-font-ui)', fontSize: 11,
        color: 'var(--cth-ink-500)', whiteSpace: 'nowrap',
      }}>{APP_VERSION} · Sensei Mode</span>

      <div style={{ flex: 1 }} />

      <IconBtn title="알림" badge={notifications} onClick={onBell}><BellIcon /></IconBtn>
      <IconBtn title="메일" badge={mail} onClick={onMail}><MailIcon /></IconBtn>
      <IconBtn title="설정" onClick={onSettings}><GearIcon /></IconBtn>
    </div>
  );
}
