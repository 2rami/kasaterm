import { CSSProperties, useState } from 'react';
import { useIsPhone } from '@/lib/useIsPhone';
import { usagePressure, type ClaudeUsage } from '@/lib/mcp';

// 슬림 아이콘 바 — 모든 버튼을 IconBtn 으로 통일(거노: 디자인 통일), 로고·기어 제거.
// 좌측엔 방 하나, 나머지(업무·교실·집중)는 전부 우측에 모았다(거노: task 아이콘 우측으로).
// 전역 바라 옅은 sky 틴트+그림자로 pane 하단바(plain cream)와 시각 구분.

interface IconBtnProps {
  title: string;
  badge?: number;
  active?: boolean;
  onClick?: () => void;
  children: React.ReactNode;
}

function IconBtn({ title, badge, active, onClick, children }: IconBtnProps) {
  const [hover, setHover] = useState(false);
  const isPhone = useIsPhone();
  const lit = active || hover;
  // 폰에선 44 — 26px 상자는 손가락으로 못 누른다(iOS HIG 44). 아이콘 크기는 그대로 두고
  // 누를 수 있는 상자만 키운다.
  const box = isPhone ? 44 : 36;
  return (
    <button
      title={title}
      onClick={onClick}
      onMouseEnter={() => setHover(true)}
      onMouseLeave={() => setHover(false)}
      style={{
        position: 'relative',
        width: box, height: box, flexShrink: 0,
        display: 'inline-flex', alignItems: 'center', justifyContent: 'center',
        border: 'none', cursor: 'pointer',
        background: lit ? 'var(--cth-sky-light)' : 'transparent',
        borderRadius: 7,
        color: active ? 'var(--cth-sky)' : 'var(--cth-ink-500)',
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

// 방(좌 패널) — 집. 업무(우 패널) — 체크리스트.
function RoomIcon() {
  return (
    <svg width="15" height="15" viewBox="0 0 16 16">
      <path style={stroke} d="M2.5 7.5 8 3l5.5 4.5M4 6.8V13h8V6.8" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}
function TasksIcon() {
  return (
    <svg width="15" height="15" viewBox="0 0 16 16">
      <path style={stroke} d="M6 4.5h7M6 8h7M6 11.5h7" strokeLinecap="round" />
      <path style={stroke} d="m2.5 4 1 1 1.2-1.6M2.5 7.6l1 1 1.2-1.6M2.5 11.1l1 1 1.2-1.6" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}
// 교실(캐릭터) 보기 — 액자 속 그림(산·해). 거노: 터미널 아이콘 자리에 그림 아이콘=교실 기능.
function ClassroomIcon() {
  return (
    <svg width="15" height="15" viewBox="0 0 16 16">
      <rect style={stroke} x="2" y="3" width="12" height="10" rx="1.5" />
      <circle style={stroke} cx="5.6" cy="6.4" r="1.1" />
      <path style={stroke} d="M2.6 11.2 6 8.4l2.4 1.9 2-1.5 3 2.4" strokeLinecap="round" strokeLinejoin="round" />
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
// 집중 모드(패널 전부 숨기기) — 네 모서리 안쪽 화살표.
function FocusIcon() {
  return (
    <svg width="15" height="15" viewBox="0 0 16 16">
      <path style={stroke} d="M6 2.5H3.5V5M10 2.5h2.5V5M6 13.5H3.5V11M10 13.5h2.5V11" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}

// 리셋까지 남은 시간 — "2h 15m 후".
function fmtReset(iso: string): string {
  const ms = new Date(iso).getTime() - Date.now();
  if (ms <= 0) return '곧';
  const h = Math.floor(ms / 3600000);
  const m = Math.floor((ms % 3600000) / 60000);
  return h > 0 ? `${h}h ${m}m 후` : `${m}m 후`;
}

// claude 사용량 미니 게이지 — 5시간/주간 한도를 막대+%로. 70%↑ 호박, 90%↑ 산호.
function UsagePill({ label, pct, resetsAt, stale }: { label: string; pct: number; resetsAt: string | null; stale?: boolean }) {
  const color = pct >= 90 ? 'var(--cth-coral)' : pct >= 70 ? '#FFB020' : 'var(--cth-sky)';
  const reset = resetsAt ? ` · 리셋 ${fmtReset(resetsAt)}` : '';
  return (
    <div
      title={`claude ${label} 한도 ${pct.toFixed(0)}%${reset}${stale ? ' (지금 값이 아님 — 사용량 조회가 막혀 마지막 값)' : ''}`}
      style={{
        display: 'inline-flex', alignItems: 'center', gap: 5,
        padding: '3px 8px', borderRadius: 999, background: 'var(--cth-cream-50)',
        border: '1px solid var(--cth-cream-200)', marginRight: 4,
        fontFamily: 'var(--cth-font-ui)', fontSize: 10, fontWeight: 700, color: 'var(--cth-ink-500)',
        // stale 은 흐리게만 — 숨기면 빈칸이 "한도 여유"로 읽힌다.
        opacity: stale ? 0.55 : 1,
      }}
    >
      <span>{stale ? `~${label}` : label}</span>
      <span style={{ width: 30, height: 4, borderRadius: 2, background: 'var(--cth-cream-200)', overflow: 'hidden' }}>
        <span style={{ display: 'block', width: `${Math.min(100, pct)}%`, height: '100%', background: color, borderRadius: 2 }} />
      </span>
      <span style={{ color: 'var(--cth-ink-900)' }}>{pct.toFixed(0)}%</span>
    </div>
  );
}

export interface TitleBarProps {
  /** claude oauth 한도 — 본문 + 그 값이 지금 것인지(stale) + 어느 계정 것인지. */
  usage?: { usage: ClaudeUsage; stale: boolean; accountDir: string } | null;
  /** 현재 테마 — 태양/달 버튼 표시. */
  theme?: 'light' | 'dark';
  onToggleTheme?: () => void;
  /** 좌측 방·학생 / 우측 업무 패널 팝오버 토글. */
  onToggleLeft?: () => void;
  onToggleRight?: () => void;
  leftOpen?: boolean;
  rightOpen?: boolean;
  /** 좌 패널 아이콘 배지 — 주의(대기/막힘) 학생 수. */
  leftBadge?: number;
  /** 교실(캐릭터) 뷰 토글 — classroom 이면 active. */
  classroom?: boolean;
  onToggleClassroom?: () => void;
  /** 집중 모드 진입(패널 전부 숨김). */
  onFocus?: () => void;
}

export function TitleBar({ usage, theme = 'light', onToggleTheme, onToggleLeft, onToggleRight, leftOpen, rightOpen, leftBadge = 0, classroom, onToggleClassroom, onFocus }: TitleBarProps) {
  const isPhone = useIsPhone();
  const divider = <div style={{ width: 1, height: 16, background: 'var(--cth-cream-200)', flexShrink: 0, margin: '0 2px' }} />;
  const pressure = usagePressure(usage?.usage ?? null);
  return (
    <div style={{
      display: 'flex', alignItems: 'center', gap: 7,
      // 폰은 48 — 44px 버튼이 34px 바에 안 들어간다.
      // 노치 여백은 **높이에 더한다**. box-sizing 이 border-box 라 paddingTop 만 주면 그만큼
      // 안쪽이 깎여 버튼이 다시 눌리기 때문이다. 브라우저·데스크톱 webview 에선 inset 이 0 이라
      // 정확히 48 로 남는다.
      padding: '0 10px', flexShrink: 0, boxSizing: 'border-box',
      height: isPhone ? 'calc(48px + env(safe-area-inset-top, 0px))' : 44,
      paddingTop: isPhone ? 'env(safe-area-inset-top, 0px)' : undefined,
      // 전역 바 — 옅은 sky 그라데이션 + 아래 그림자로 pane 하단바(plain cream)와 구분(거노).
      background: 'linear-gradient(180deg, var(--cth-sky-light), var(--cth-cream-50))',
      borderBottom: '1px solid var(--cth-cream-200)',
      boxShadow: '0 1px 4px rgba(21,41,74,0.06)', zIndex: 5,
    }}>
      {/* 좌측 — 방·학생 팝오버 진입 하나만 */}
      <IconBtn title="방·캐릭터" active={leftOpen} badge={leftBadge} onClick={onToggleLeft}><RoomIcon /></IconBtn>

      <div style={{ flex: 1 }} />

      {/* 우측 — 사용량·테마·업무·교실·집중 */}
      {/* 창을 하나만 — **가장 먼저 닫히는** 것. 전에는 five_hour·seven_day 를 각각
          그렸는데 oauth/usage 는 seven_day 를 안 주고(limits[] 로 옮겨 갔다) five_hour
          는 세 계정 모두 0 이라, 실제로는 주간 95% 인데 pill 이 「5h 0%」 하나만
          떴다(거노 2026-08-05: "info에는 다 0퍼로뜨는데"). */}
      {pressure && <UsagePill label={pressure.label} pct={pressure.pct} resetsAt={pressure.resetsAt} stale={usage?.stale} />}
      <IconBtn title={theme === 'dark' ? '라이트 모드로' : '다크 모드로'} onClick={onToggleTheme}>{theme === 'dark' ? <SunIcon /> : <MoonIcon />}</IconBtn>
      <IconBtn title="업무·소스 컨트롤·스케줄" active={rightOpen} onClick={onToggleRight}><TasksIcon /></IconBtn>
      <IconBtn title={classroom ? '대화 보기로' : '교실(캐릭터) 보기'} active={classroom} onClick={onToggleClassroom}><ClassroomIcon /></IconBtn>
      {divider}
      <IconBtn title="패널 전부 숨기기 (⌘\)" onClick={onFocus}><FocusIcon /></IconBtn>
    </div>
  );
}
