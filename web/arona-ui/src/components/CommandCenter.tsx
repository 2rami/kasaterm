import { useEffect, useRef, useState } from 'react';
import { useStore } from '@/store';

const BASE = import.meta.env.DEV ? 'http://127.0.0.1:8765' : '';

type CenterTab = 'dashboard' | 'tasks' | 'council' | 'schedule' | 'log';

const TAB_LABELS: Record<CenterTab, string> = {
  dashboard: '대시보드',
  tasks: '업무',
  council: '의회',
  schedule: '스케줄 관리',
  log: '기록',
};

interface FeedEvent {
  type?: string;
  surface?: string;
  status?: string;
  timestamp?: string;
  [key: string]: unknown;
}

interface ChatMessage {
  from?: string;
  text?: string;
  time?: string;
}

async function fetchEvents(): Promise<FeedEvent[]> {
  try {
    const r = await fetch(`${BASE}/events`);
    if (!r.ok) return [];
    const d = await r.json().catch(() => null);
    return Array.isArray(d?.events) ? d.events : Array.isArray(d) ? d : [];
  } catch { return []; }
}

async function fetchMessages(): Promise<ChatMessage[]> {
  try {
    const r = await fetch(`${BASE}/messages`);
    if (!r.ok) return [];
    const d = await r.json().catch(() => null);
    return Array.isArray(d?.messages) ? d.messages : Array.isArray(d) ? d : [];
  } catch { return []; }
}

// SCHALE OS 우측 Command Center 패널.
// GET /events, GET /messages 1s 폴링(백엔드 없으면 404 → 빈 리스트).
export function CommandCenter() {
  const [tab, setTab] = useState<CenterTab>('dashboard');
  const [events, setEvents] = useState<FeedEvent[]>([]);
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const feedRef = useRef<HTMLDivElement>(null);
  const agents = useStore((s) => s.agents);

  useEffect(() => {
    let stopped = false;
    const tick = async () => {
      if (stopped) return;
      const [ev, msg] = await Promise.all([fetchEvents(), fetchMessages()]);
      if (!stopped) { setEvents(ev); setMessages(msg); }
    };
    void tick();
    const iv = setInterval(tick, 1000);
    return () => { stopped = true; clearInterval(iv); };
  }, []);

  // 이벤트 추가 시 자동 스크롤
  useEffect(() => {
    if (feedRef.current) feedRef.current.scrollTop = feedRef.current.scrollHeight;
  }, [events]);

  const god = agents.find((a) => a.isGod);
  const workers = agents.filter((a) => !a.isGod);

  return (
    <div style={{
      width: 300,
      flexShrink: 0,
      height: '100%',
      display: 'flex',
      flexDirection: 'column',
      borderLeft: '1px solid var(--cth-cream-200)',
      background: 'var(--cth-cream-50)',
      overflow: 'hidden'
    }}>
      {/* 헤더 */}
      <div style={{
        padding: '12px 14px 10px',
        borderBottom: '1px solid var(--cth-cream-200)',
        display: 'flex',
        alignItems: 'center',
        gap: 8
      }}>
        <div style={{ flex: 1, minWidth: 0 }}>
          <div style={{
            fontFamily: 'var(--cth-font-display)',
            fontSize: 'var(--cth-text-display-sm)',
            color: 'var(--cth-ink-500)',
            lineHeight: 1
          }}>선생님</div>
          <div style={{
            fontFamily: 'var(--cth-font-display)',
            fontSize: 'var(--cth-text-display-md)',
            color: 'var(--cth-ink-900)', fontWeight: 700,
            lineHeight: 1.2
          }}>Command Center</div>
        </div>
        <div style={{
          padding: '4px 10px',
          background: 'var(--cth-sky)',
          color: '#fff',
          fontFamily: 'var(--cth-font-ui)',
          fontSize: 11, fontWeight: 700, letterSpacing: 0.5,
          borderRadius: 6
        }}>SCHALE</div>
      </div>

      {/* 탭 */}
      <div className="cth-tabbar" style={{
        display: 'flex',
        borderBottom: '1px solid var(--cth-cream-200)',
        overflowX: 'auto', gap: 2, padding: '5px 6px'
      }}>
        {(Object.keys(TAB_LABELS) as CenterTab[]).map((t) => (
          <button
            key={t}
            onClick={() => setTab(t)}
            style={{
              flexShrink: 0,
              padding: '6px 12px',
              fontFamily: 'var(--cth-font-ui)',
              fontSize: 12, fontWeight: 600,
              border: 'none', borderRadius: 7,
              background: tab === t ? 'var(--cth-sky)' : 'transparent',
              color: tab === t ? '#fff' : 'var(--cth-ink-500)',
              cursor: 'pointer',
              whiteSpace: 'nowrap',
              transition: 'background 120ms ease, color 120ms ease'
            }}
          >
            {TAB_LABELS[t]}
          </button>
        ))}
      </div>

      {/* 본문 */}
      {tab === 'dashboard' ? (
        <div style={{ flex: 1, display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>
          {/* 이벤트 피드 */}
          <div style={{
            display: 'flex', alignItems: 'center', gap: 6,
            padding: '6px 10px 4px',
            borderBottom: '1px solid var(--cth-cream-300)'
          }}>
            <span style={{ fontFamily: 'var(--cth-font-display)', fontSize: 10, color: 'var(--cth-ink-700)' }}>이벤트 피드</span>
            <span style={{
              padding: '2px 7px',
              background: 'var(--cth-coral)',
              color: '#fff',
              fontFamily: 'var(--cth-font-ui)',
              fontSize: 10, fontWeight: 700, borderRadius: 5,
              display: 'inline-flex', alignItems: 'center', gap: 4
            }}>● LIVE</span>
          </div>
          <div
            ref={feedRef}
            style={{
              flex: '0 0 160px',
              overflowY: 'auto',
              padding: '8px 10px',
              borderBottom: '1px solid var(--cth-cream-200)',
              background: 'var(--cth-ink-900)'
            }}
          >
            {events.length === 0 ? (
              <span style={{ fontFamily: 'monospace', fontSize: 10, color: 'var(--cth-ink-300)' }}>이벤트 없음</span>
            ) : events.slice(-20).map((e, i) => (
              <div key={i} style={{ fontFamily: 'var(--cth-font-mono)', fontSize: 11, color: '#9DC1E8', marginBottom: 3, wordBreak: 'break-all', lineHeight: 1.5 }}>
                {JSON.stringify(e)}
              </div>
            ))}
          </div>

          {/* 채팅 메시지 */}
          <div style={{
            fontFamily: 'var(--cth-font-display)', fontSize: 10, color: 'var(--cth-ink-700)',
            padding: '6px 10px 4px',
            borderBottom: '1px solid var(--cth-cream-300)'
          }}>
            {god ? `${god.character} 채널` : '대화'}
          </div>
          <div style={{ flex: 1, overflowY: 'auto', padding: '6px 10px' }}>
            {messages.length === 0 ? (
              <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 11, color: 'var(--cth-ink-300)' }}>메시지 없음</span>
            ) : messages.map((m, i) => (
              <div key={i} style={{ marginBottom: 10, display: 'flex', gap: 8 }}>
                <span style={{
                  width: 28, height: 28, borderRadius: 999, flexShrink: 0,
                  background: 'var(--cth-sky-light)', color: 'var(--cth-ink-700)',
                  display: 'flex', alignItems: 'center', justifyContent: 'center',
                  fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: 700
                }}>{(m.from ?? '?').charAt(0)}</span>
                <div style={{ flex: 1, minWidth: 0 }}>
                  <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 11, fontWeight: 600, color: 'var(--cth-ink-700)', marginBottom: 3 }}>
                    {m.from ?? '?'}{m.time ? <span style={{ color: 'var(--cth-ink-300)', fontWeight: 400 }}> · {m.time}</span> : null}
                  </div>
                  <div style={{
                    fontFamily: 'var(--cth-font-ui)', fontSize: 12, color: 'var(--cth-ink-900)', lineHeight: 1.5,
                    background: 'var(--cth-cream-100)', padding: '7px 10px', borderRadius: 10
                  }}>{m.text}</div>
                </div>
              </div>
            ))}
          </div>
        </div>
      ) : tab === 'council' ? (
        /* 의뢰 대기열 — board working 워커들 */
        <div style={{ flex: 1, overflowY: 'auto', padding: 10 }}>
          <div style={{ fontFamily: 'var(--cth-font-display)', fontSize: 10, color: 'var(--cth-ink-500)', marginBottom: 8 }}>
            의뢰 대기열 {workers.length} / 10
          </div>
          {workers.length === 0 ? (
            <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 11, color: 'var(--cth-ink-300)' }}>대기 중인 의뢰 없음</span>
          ) : workers.map((a, i) => (
            <div key={a.id} style={{
              display: 'flex', alignItems: 'center', gap: 6,
              padding: '5px 0',
              borderBottom: '1px solid var(--cth-cream-300)'
            }}>
              <span style={{ fontFamily: 'var(--cth-font-display)', fontSize: 10, color: 'var(--cth-ink-300)', width: 16 }}>{i + 1}</span>
              <span style={{ flex: 1, fontFamily: 'var(--cth-font-ui)', fontSize: 11, color: 'var(--cth-ink-900)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                {a.project || a.name}
              </span>
              <span style={{
                padding: '2px 7px', fontSize: 10, fontWeight: 600, borderRadius: 5,
                fontFamily: 'var(--cth-font-ui)', color: '#fff',
                background: a.status === 'working' ? 'var(--cth-mint)' : a.status === 'waiting' ? 'var(--cth-sky)' : 'var(--cth-ink-300)',
              }}>{a.status}</span>
            </div>
          ))}
        </div>
      ) : tab === 'tasks' ? (
        /* 업무 — 학생별 현재 작업(현재 tool + 서브에이전트) */
        <div style={{ flex: 1, overflowY: 'auto', padding: 10 }}>
          {agents.length === 0 ? (
            <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 11, color: 'var(--cth-ink-300)' }}>학생 없음</span>
          ) : agents.map((a) => (
            <div key={a.id} style={{ display: 'flex', alignItems: 'center', gap: 8, padding: '7px 0', borderBottom: '1px solid var(--cth-cream-200)' }}>
              <span style={{ width: 8, height: 8, borderRadius: 999, flexShrink: 0, background: a.status === 'working' ? 'var(--cth-mint)' : a.status === 'waiting' || a.status === 'blocked' ? 'var(--cth-coral)' : 'var(--cth-ink-300)' }} />
              <div style={{ flex: 1, minWidth: 0 }}>
                <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: 600, color: 'var(--cth-ink-900)' }}>{a.character}</div>
                <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 11, color: 'var(--cth-ink-500)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{a.project || '대기 중'}</div>
              </div>
              {a.currentTool && (
                <span style={{ fontFamily: 'var(--cth-font-mono)', fontSize: 10, fontWeight: 700, color: 'var(--cth-sky)', background: 'color-mix(in srgb, var(--cth-sky) 12%, #fff)', padding: '2px 7px', borderRadius: 6 }}>{a.currentTool}</span>
              )}
              {!!a.subagents?.length && (
                <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 10, fontWeight: 700, color: 'var(--cth-lilac)', background: 'color-mix(in srgb, var(--cth-lilac) 14%, #fff)', padding: '2px 7px', borderRadius: 6 }}>서브 {a.subagents.length}</span>
              )}
            </div>
          ))}
        </div>
      ) : tab === 'log' ? (
        /* 기록 — 이벤트 전체 로그(콘솔) */
        <div style={{ flex: 1, overflowY: 'auto', padding: 10, background: 'var(--cth-ink-900)' }}>
          {events.length === 0 ? (
            <span style={{ fontFamily: 'var(--cth-font-mono)', fontSize: 11, color: 'var(--cth-ink-300)' }}>기록 없음</span>
          ) : events.map((e, i) => (
            <div key={i} style={{ fontFamily: 'var(--cth-font-mono)', fontSize: 11, color: '#9DC1E8', marginBottom: 4, wordBreak: 'break-all', lineHeight: 1.5 }}>{JSON.stringify(e)}</div>
          ))}
        </div>
      ) : (
        /* 스케줄 관리 — 학생별 컨텍스트 진척(소진율) */
        <div style={{ flex: 1, overflowY: 'auto', padding: 10 }}>
          {agents.length === 0 ? (
            <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 11, color: 'var(--cth-ink-300)' }}>학생 없음</span>
          ) : agents.map((a) => {
            const ctx = a.contextTokens ?? 0;
            const pct = Math.min(100, Math.round((ctx / 200000) * 100));
            return (
              <div key={a.id} style={{ padding: '7px 0', borderBottom: '1px solid var(--cth-cream-200)' }}>
                <div style={{ display: 'flex', justifyContent: 'space-between', marginBottom: 4 }}>
                  <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: 600, color: 'var(--cth-ink-900)' }}>{a.character}</span>
                  <span style={{ fontFamily: 'var(--cth-font-mono)', fontSize: 10, color: 'var(--cth-ink-300)' }}>{Math.round(ctx / 1000)}k</span>
                </div>
                <div style={{ height: 6, borderRadius: 999, background: 'var(--cth-cream-200)', overflow: 'hidden' }}>
                  <div style={{ height: '100%', width: `${pct}%`, borderRadius: 999, background: pct > 75 ? 'var(--cth-coral)' : 'var(--cth-sky)' }} />
                </div>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
