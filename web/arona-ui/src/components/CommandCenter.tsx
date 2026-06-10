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
      borderLeft: '2px solid var(--cth-ink-900)',
      background: 'var(--cth-cream-100)',
      overflow: 'hidden'
    }}>
      {/* 헤더 */}
      <div style={{
        padding: '10px 12px 8px',
        borderBottom: '1px solid var(--cth-ink-900)',
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
            color: 'var(--cth-ink-900)',
            lineHeight: 1.2
          }}>Command Center</div>
        </div>
        <div style={{
          padding: '3px 8px',
          background: 'var(--cth-sky)',
          color: 'var(--cth-ink-900)',
          fontFamily: 'var(--cth-font-display)',
          fontSize: 'var(--cth-text-display-sm)',
          boxShadow: 'inset 0 0 0 1px var(--cth-ink-900)'
        }}>SCHALE</div>
      </div>

      {/* 탭 */}
      <div style={{
        display: 'flex',
        borderBottom: '1px solid var(--cth-ink-900)',
        overflowX: 'auto'
      }}>
        {(Object.keys(TAB_LABELS) as CenterTab[]).map((t) => (
          <button
            key={t}
            onClick={() => setTab(t)}
            style={{
              flexShrink: 0,
              padding: '5px 8px',
              fontFamily: 'var(--cth-font-display)',
              fontSize: 10,
              border: 'none',
              borderRight: '1px solid var(--cth-ink-900)',
              background: tab === t ? 'var(--cth-ink-900)' : 'transparent',
              color: tab === t ? 'var(--cth-cream-50)' : 'var(--cth-ink-700)',
              cursor: 'pointer',
              whiteSpace: 'nowrap'
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
              padding: '1px 5px',
              background: 'var(--cth-coral)',
              color: 'var(--cth-cream-50)',
              fontFamily: 'var(--cth-font-display)',
              fontSize: 9,
              boxShadow: 'inset 0 0 0 1px var(--cth-ink-900)'
            }}>LIVE</span>
          </div>
          <div
            ref={feedRef}
            style={{
              flex: '0 0 160px',
              overflowY: 'auto',
              padding: '6px 10px',
              borderBottom: '1px solid var(--cth-ink-900)',
              background: 'var(--cth-cream-50)'
            }}
          >
            {events.length === 0 ? (
              <span style={{ fontFamily: 'monospace', fontSize: 10, color: 'var(--cth-ink-400)' }}>이벤트 없음</span>
            ) : events.slice(-20).map((e, i) => (
              <div key={i} style={{ fontFamily: 'monospace', fontSize: 10, color: 'var(--cth-ink-700)', marginBottom: 2, wordBreak: 'break-all' }}>
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
              <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 11, color: 'var(--cth-ink-400)' }}>메시지 없음</span>
            ) : messages.map((m, i) => (
              <div key={i} style={{ marginBottom: 8 }}>
                <div style={{ fontFamily: 'var(--cth-font-display)', fontSize: 9, color: 'var(--cth-ink-500)', marginBottom: 2 }}>
                  {m.from ?? '?'} {m.time ? `· ${m.time}` : ''}
                </div>
                <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 11, color: 'var(--cth-ink-900)', lineHeight: 1.4 }}>
                  {m.text}
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
            <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 11, color: 'var(--cth-ink-400)' }}>대기 중인 의뢰 없음</span>
          ) : workers.map((a, i) => (
            <div key={a.id} style={{
              display: 'flex', alignItems: 'center', gap: 6,
              padding: '5px 0',
              borderBottom: '1px solid var(--cth-cream-300)'
            }}>
              <span style={{ fontFamily: 'var(--cth-font-display)', fontSize: 10, color: 'var(--cth-ink-400)', width: 16 }}>{i + 1}</span>
              <span style={{ flex: 1, fontFamily: 'var(--cth-font-ui)', fontSize: 11, color: 'var(--cth-ink-900)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                {a.project || a.name}
              </span>
              <span style={{
                padding: '1px 5px', fontSize: 9,
                fontFamily: 'var(--cth-font-display)',
                background: a.status === 'working' ? 'var(--cth-mint)' : a.status === 'waiting' ? 'var(--cth-sky)' : 'var(--cth-cream-300)',
                boxShadow: 'inset 0 0 0 1px var(--cth-ink-900)'
              }}>{a.status}</span>
            </div>
          ))}
        </div>
      ) : (
        <div style={{ flex: 1, display: 'flex', alignItems: 'center', justifyContent: 'center' }}>
          <span style={{ fontFamily: 'var(--cth-font-display)', fontSize: 11, color: 'var(--cth-ink-400)' }}>준비 중</span>
        </div>
      )}
    </div>
  );
}
