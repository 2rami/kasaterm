import { CSSProperties, useState } from 'react';
import { StatPill } from './GameKit';
import type { ClaudeUsage } from '@/lib/mcp';

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
// 설정 — 슬라이더 아이콘. 옛 톱니(원+8빛살)는 다크모드 태양 토글과 똑같이 보여
// "버튼 두개 모드체인지" 혼동(거노). 슬라이더로 바꿔 명확히 구분.
function GearIcon() {
  return (
    <svg width="15" height="15" viewBox="0 0 16 16">
      <path style={stroke} d="M2.5 4.5h11M2.5 8h11M2.5 11.5h11" strokeLinecap="round" />
      <circle cx="6" cy="4.5" r="1.8" fill="var(--cth-cream-50)" stroke="currentColor" strokeWidth="1.4" />
      <circle cx="10.5" cy="8" r="1.8" fill="var(--cth-cream-50)" stroke="currentColor" strokeWidth="1.4" />
      <circle cx="5" cy="11.5" r="1.8" fill="var(--cth-cream-50)" stroke="currentColor" strokeWidth="1.4" />
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
function SunIcon() {
  return (
    <svg width="15" height="15" viewBox="0 0 16 16">
      <circle cx="8" cy="8" r="3" fill="currentColor" />
      <path style={stroke} d="M8 1v1.6M8 13.4V15M1 8h1.6M13.4 8H15M3.05 3.05l1.13 1.13M11.82 11.82l1.13 1.13M12.95 3.05l-1.13 1.13M4.18 11.82l-1.13 1.13" />
    </svg>
  );
}
function MoonIcon() {
  return (
    <svg width="15" height="15" viewBox="0 0 16 16">
      <path style={stroke} d="M13 9.5A5.5 5.5 0 0 1 6.5 3a5.5 5.5 0 1 0 6.5 6.5Z" strokeLinejoin="round" />
    </svg>
  );
}
const fmtTokens = (n: number) => (n >= 1000 ? `${Math.round(n / 1000)}k` : String(n));

// 리셋까지 남은 시간 — "2h 15m 후".
function fmtReset(iso: string): string {
  const ms = new Date(iso).getTime() - Date.now();
  if (ms <= 0) return '곧';
  const h = Math.floor(ms / 3600000);
  const m = Math.floor((ms % 3600000) / 60000);
  return h > 0 ? `${h}h ${m}m 후` : `${m}m 후`;
}

// claude 사용량 미니 게이지 — OpenUsage 메뉴바처럼 5시간/주간 한도를 막대+% 로(거노:
// ba모드 사용량 내장). 사용률 70%↑ 호박, 90%↑ 산호. 툴팁에 리셋 시각.
function UsagePill({ label, pct, resetsAt }: { label: string; pct: number; resetsAt: string }) {
  const color = pct >= 90 ? 'var(--cth-coral)' : pct >= 70 ? '#FFB020' : 'var(--cth-sky)';
  return (
    <div
      title={`claude ${label} 한도 ${pct.toFixed(0)}% · 리셋 ${fmtReset(resetsAt)}`}
      style={{
        display: 'inline-flex', alignItems: 'center', gap: 5,
        padding: '3px 8px', borderRadius: 999, background: 'var(--cth-cream-50)',
        border: '1px solid var(--cth-cream-200)', marginRight: 4,
        fontFamily: 'var(--cth-font-ui)', fontSize: 10, fontWeight: 700, color: 'var(--cth-ink-500)',
      }}
    >
      <span>{label}</span>
      <span style={{ width: 30, height: 4, borderRadius: 2, background: 'var(--cth-cream-200)', overflow: 'hidden' }}>
        <span style={{ display: 'block', width: `${Math.min(100, pct)}%`, height: '100%', background: color, borderRadius: 2 }} />
      </span>
      <span style={{ color: 'var(--cth-ink-900)' }}>{pct.toFixed(0)}%</span>
    </div>
  );
}

export interface TitleBarProps {
  notifications?: number;
  mail?: number;
  /** ⚡ 번개 = 전 학생 총 컨텍스트 토큰(재화 치환). */
  contextTokens?: number;
  /** claude oauth usage — 5시간/주간 한도 게이지. */
  usage?: ClaudeUsage | null;
  /** 현재 테마 — 태양/달 버튼 표시. */
  theme?: 'light' | 'dark';
  onToggleTheme?: () => void;
  onBell?: () => void;
  onMail?: () => void;
  onSettings?: () => void;
}

export function TitleBar({ notifications = 0, mail = 0, contextTokens = 0, usage, theme = 'light', onToggleTheme, onBell, onMail, onSettings }: TitleBarProps) {
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

      {usage?.five_hour && <UsagePill label="5h" pct={usage.five_hour.utilization} resetsAt={usage.five_hour.resets_at} />}
      {usage?.seven_day && <UsagePill label="7d" pct={usage.seven_day.utilization} resetsAt={usage.seven_day.resets_at} />}
      {contextTokens > 0 && (
        <StatPill icon={<BoltIcon />} value={fmtTokens(contextTokens)} style={{ marginRight: 4 }} />
      )}
      <IconBtn title={theme === 'dark' ? '라이트 모드로' : '다크 모드로'} onClick={onToggleTheme}>{theme === 'dark' ? <SunIcon /> : <MoonIcon />}</IconBtn>
      <IconBtn title="알림" badge={notifications} onClick={onBell}><BellIcon /></IconBtn>
      <IconBtn title="메일" badge={mail} onClick={onMail}><MailIcon /></IconBtn>
      <IconBtn title="설정" onClick={onSettings}><GearIcon /></IconBtn>
    </div>
  );
}
