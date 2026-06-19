import { useEffect, useState } from 'react';
import { fetchRecentSessions, resumeSession, type RecentSession, type SessionsInfo } from '@/lib/mcp';

// 방 추가 시 고를 god(거노: 처음은 아로나 고정, 새 방은 선택). leaders 풀과 일치.
const GODS = ['아로나', '프라나'];

/** unix secs → "방금/N분 전/N시간 전/N일 전". */
function relativeTime(secs: number): string {
  const diff = Math.max(0, Date.now() / 1000 - secs);
  if (diff < 60) return '방금';
  if (diff < 3600) return `${Math.floor(diff / 60)}분 전`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}시간 전`;
  return `${Math.floor(diff / 86400)}일 전`;
}

export interface RoomMapProps {
  sessions: SessionsInfo;
  onSwitch: (idx: number) => void;
  /** 새 방 + 선택 god 스폰. */
  onNewRoom?: (god: string) => void;
  /** 방(윈도우) 닫기. 윈도우 2개+ 일 때만. */
  onCloseRoom?: (idx: number) => void;
}

function RoomIcon({ active }: { active: boolean }) {
  const c = active ? '#fff' : 'var(--cth-sky)';
  return (
    <svg width="16" height="16" viewBox="0 0 18 18" style={{ flexShrink: 0 }}>
      <path d="M2 8 9 3l7 5v6a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1V8Z" fill="none" stroke={c} strokeWidth="1.4" strokeLinejoin="round" />
      <rect x="6.5" y="10" width="5" height="4" fill="none" stroke={c} strokeWidth="1.2" />
    </svg>
  );
}

// 좌측 방 네비 — 방 = kasaterm 윈도우(거노). 목록 + "+ 방 추가"(god 선택). 첫 방은
// 아로나 고정, 새 방은 아로나/프라나 선택해 그 god 으로 스폰. × 로 방 닫기.
export function RoomMap({ sessions, onSwitch, onNewRoom, onCloseRoom }: RoomMapProps) {
  const n = sessions.count;
  const [adding, setAdding] = useState(false);
  const [collapsed, setCollapsed] = useState(false);
  const [showRecent, setShowRecent] = useState(false);
  const [recent, setRecent] = useState<RecentSession[]>([]);
  const [resuming, setResuming] = useState<string | null>(null);

  // 최근 세션 패널을 펼칠 때(또는 펼친 채로 10초마다) 목록을 가져온다 — 항상
  // 폴링하면 닫혀있을 때도 낭비라, 펼침 상태에서만 새로고침.
  useEffect(() => {
    if (!showRecent) return;
    let alive = true;
    const load = () => { void fetchRecentSessions().then((s) => { if (alive) setRecent(s); }); };
    load();
    const iv = setInterval(load, 10000);
    return () => { alive = false; clearInterval(iv); };
  }, [showRecent]);

  const onResume = async (s: RecentSession) => {
    setResuming(s.id);
    await resumeSession(s.id, s.cwd, false);
    setResuming(null);
    setShowRecent(false);
  };

  if (n < 1) return null;
  // 접힌 상태 — 얇은 띠 + 펼치기 버튼(거노: 좌측 패널 접기).
  if (collapsed) {
    return (
      <div style={{
        width: 30, flexShrink: 0, height: '100%', borderRight: '1px solid var(--cth-cream-200)',
        background: 'var(--cth-cream-50)', padding: '10px 0', display: 'flex', flexDirection: 'column', alignItems: 'center', gap: 8,
      }}>
        <button onClick={() => setCollapsed(false)} title="방 패널 펼치기" style={{
          width: 22, height: 22, borderRadius: 6, border: 'none', cursor: 'pointer', background: 'var(--cth-cream-100)',
          color: 'var(--cth-ink-500)', display: 'flex', alignItems: 'center', justifyContent: 'center',
        }}>
          <svg width="12" height="12" viewBox="0 0 16 16"><path d="M6 3l5 5-5 5" stroke="currentColor" strokeWidth="2" fill="none" strokeLinecap="round" strokeLinejoin="round" /></svg>
        </button>
      </div>
    );
  }
  return (
    <div style={{
      width: 184, flexShrink: 0, height: '100%', overflowY: 'auto',
      borderRight: '1px solid var(--cth-cream-200)', background: 'var(--cth-cream-50)',
      padding: '10px 8px', display: 'flex', flexDirection: 'column', gap: 4,
    }}>
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', padding: '0 2px 4px' }}>
        <span style={{ fontFamily: 'var(--cth-font-display)', fontSize: 10, color: 'var(--cth-ink-500)' }}>방 (터미널 윈도우)</span>
        <button onClick={() => setCollapsed(true)} title="방 패널 접기" style={{
          width: 18, height: 18, borderRadius: 5, border: 'none', cursor: 'pointer', background: 'transparent', color: 'var(--cth-ink-300)',
          display: 'flex', alignItems: 'center', justifyContent: 'center', flexShrink: 0,
        }}>
          <svg width="12" height="12" viewBox="0 0 16 16"><path d="M10 3l-5 5 5 5" stroke="currentColor" strokeWidth="2" fill="none" strokeLinecap="round" strokeLinejoin="round" /></svg>
        </button>
      </div>
      {Array.from({ length: n }, (_, i) => {
        const on = i === sessions.active;
        return (
          <div key={i} style={{
            display: 'flex', alignItems: 'center', gap: 4, borderRadius: 8,
            background: on ? 'var(--cth-sky)' : 'transparent', color: on ? '#fff' : 'var(--cth-ink-700)',
          }}>
            <button onClick={() => { if (!on) onSwitch(i); }} style={{
              flex: 1, display: 'flex', alignItems: 'center', gap: 7, padding: '7px 9px', borderRadius: 8,
              border: 'none', cursor: on ? 'default' : 'pointer', textAlign: 'left', background: 'transparent', color: 'inherit',
              fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: 600,
            }}>
              <RoomIcon active={on} />
              <span style={{ flex: 1, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{sessions.labels[i] || `방 ${i + 1}`}</span>
              {on && <span style={{ fontSize: 9, fontWeight: 800 }}>●</span>}
            </button>
            {n > 1 && onCloseRoom && (
              <button onClick={(e) => { e.stopPropagation(); onCloseRoom(i); }} title="방 닫기" style={{
                flexShrink: 0, width: 18, height: 18, marginRight: 5, borderRadius: 5, border: 'none', cursor: 'pointer',
                background: on ? 'rgba(255,255,255,0.25)' : 'var(--cth-cream-100)', color: on ? '#fff' : 'var(--cth-ink-500)',
                fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: 800, lineHeight: 1, display: 'flex', alignItems: 'center', justifyContent: 'center',
              }}>×</button>
            )}
          </div>
        );
      })}

      {/* 방 추가 — 누르면 god(아로나/프라나) 선택 펼침 */}
      {onNewRoom && (
        adding ? (
          <div style={{ marginTop: 4, padding: 7, borderRadius: 8, background: 'var(--cth-cream-100)', display: 'flex', flexDirection: 'column', gap: 4 }}>
            <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 10, fontWeight: 700, color: 'var(--cth-ink-500)', padding: '0 2px 2px' }}>god 선택</div>
            {GODS.map((g) => (
              <button key={g} onClick={() => { onNewRoom(g); setAdding(false); }} style={{
                display: 'flex', alignItems: 'center', gap: 6, padding: '7px 9px', borderRadius: 7, border: 'none', cursor: 'pointer',
                background: '#fff', color: 'var(--cth-ink-900)', fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: 700,
                boxShadow: '0 1px 3px rgba(21,41,74,0.1)',
              }}>
                <img src={`${import.meta.env.BASE_URL || '/'}assets/idle-front-${g === '아로나' ? 'arona' : 'prana'}.png`} alt="" style={{ width: 20, height: 20, objectFit: 'contain', imageRendering: 'pixelated' }} />
                {g}
              </button>
            ))}
            <button onClick={() => setAdding(false)} style={{ padding: '5px', borderRadius: 7, border: 'none', cursor: 'pointer', background: 'transparent', color: 'var(--cth-ink-300)', fontFamily: 'var(--cth-font-ui)', fontSize: 11 }}>취소</button>
          </div>
        ) : (
          <button onClick={() => setAdding(true)} style={{
            marginTop: 4, display: 'flex', alignItems: 'center', justifyContent: 'center', gap: 6, padding: '8px', borderRadius: 8,
            border: '1.5px dashed var(--cth-cream-200)', cursor: 'pointer', background: 'transparent', color: 'var(--cth-sky)',
            fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: 700,
          }}>
            <svg width="14" height="14" viewBox="0 0 16 16"><path d="M8 3v10M3 8h10" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" /></svg>
            방 추가
          </button>
        )
      )}

      {/* 최근 세션 이어가기 — 펼치면 ~/.claude/projects 의 최근 claude 세션 목록.
          클릭하면 새 pane 에 claude --resume <id> 가 주입돼 그 대화를 잇는다. */}
      <button onClick={() => setShowRecent((v) => !v)} style={{
        marginTop: 6, display: 'flex', alignItems: 'center', gap: 6, padding: '7px 9px', borderRadius: 8,
        border: 'none', cursor: 'pointer', background: 'transparent', color: 'var(--cth-ink-500)',
        fontFamily: 'var(--cth-font-ui)', fontSize: 11, fontWeight: 700,
      }}>
        <svg width="13" height="13" viewBox="0 0 16 16" style={{ transform: showRecent ? 'rotate(90deg)' : 'none', transition: 'transform .12s' }}>
          <path d="M6 3l5 5-5 5" stroke="currentColor" strokeWidth="2" fill="none" strokeLinecap="round" strokeLinejoin="round" />
        </svg>
        최근 세션 이어가기
      </button>
      {showRecent && (
        <div style={{ display: 'flex', flexDirection: 'column', gap: 3, padding: '2px 2px 4px' }}>
          {recent.length === 0 ? (
            <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 10, color: 'var(--cth-ink-300)', padding: '4px 6px' }}>최근 세션 없음</div>
          ) : (
            recent.map((s) => (
              <button key={s.id} onClick={() => onResume(s)} disabled={resuming === s.id} title={`${s.label}\n${s.cwd}`} style={{
                display: 'flex', flexDirection: 'column', gap: 2, padding: '6px 8px', borderRadius: 7, border: 'none',
                cursor: resuming === s.id ? 'default' : 'pointer', textAlign: 'left',
                background: 'var(--cth-cream-100)', color: 'var(--cth-ink-700)', opacity: resuming === s.id ? 0.5 : 1,
              }}>
                <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 11, fontWeight: 600, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                  {s.label}
                </span>
                <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 9, color: 'var(--cth-ink-300)' }}>
                  {resuming === s.id ? '여는 중…' : relativeTime(s.mtime)}
                </span>
              </button>
            ))
          )}
        </div>
      )}
    </div>
  );
}
