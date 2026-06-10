import { useEffect, useState } from 'react';
import type { Agent } from '@/store';
import { PixelBadge } from './PixelBadge';
import { SpritePortrait } from './SpritePortrait';
import { focusPane } from '@/lib/mcp';

export interface StudentGridProps {
  agents: Agent[];
  onSelect?: (id: string, title: string) => void;
}

type SortKey = 'god-first' | 'status' | 'name';
type ViewKind = 'grid' | 'list';

const STATUS_ORDER: Record<string, number> = {
  working: 0, thinking: 0, blocked: 1, waiting: 2, idle: 3, ghost: 4
};

function sorted(agents: Agent[], key: SortKey): Agent[] {
  return [...agents].sort((a, b) => {
    if (key === 'god-first') {
      if (a.isGod !== b.isGod) return Number(b.isGod) - Number(a.isGod);
      return (STATUS_ORDER[a.status] ?? 5) - (STATUS_ORDER[b.status] ?? 5);
    }
    if (key === 'name') return a.name.localeCompare(b.name);
    return (STATUS_ORDER[a.status] ?? 5) - (STATUS_ORDER[b.status] ?? 5);
  });
}

function ProgressBar({ agent }: { agent: Agent }) {
  const { status, progress = 0, contextTokens, contextLimit } = agent;

  if (status === 'working' || status === 'thinking') {
    if (contextTokens && contextLimit) {
      const pct = Math.round((contextTokens / contextLimit) * 100);
      const fill = pct >= 87 ? 'var(--cth-coral)' : pct >= 75 ? 'var(--cth-lemon)' : 'var(--cth-sky)';
      return (
        <div style={{ display: 'flex', alignItems: 'center', gap: 3 }}>
          <div style={{
            flex: 1, height: 5,
            background: 'var(--cth-cream-200)',
            boxShadow: 'inset 0 0 0 1px var(--cth-ink-900)',
            position: 'relative', overflow: 'hidden'
          }}>
            <div style={{ position: 'absolute', inset: 0, right: `${100 - pct}%`, background: fill }} />
          </div>
          <span style={{ fontSize: 7, fontFamily: 'var(--cth-font-display)', color: 'var(--cth-ink-500)', flexShrink: 0 }}>
            {pct}%
          </span>
        </div>
      );
    }
    return (
      <div style={{
        height: 5,
        background: 'var(--cth-cream-200)',
        boxShadow: 'inset 0 0 0 1px var(--cth-ink-900)',
        overflow: 'hidden', position: 'relative'
      }}>
        <div className="cth-pulse" style={{
          position: 'absolute', left: 0, top: 0, bottom: 0, width: '55%',
          background: 'var(--cth-lemon)'
        }} />
      </div>
    );
  }

  if (progress > 0) {
    const filled = Math.min(8, Math.round(progress));
    return (
      <div style={{ display: 'flex', gap: 1 }}>
        {Array.from({ length: 8 }).map((_, i) => (
          <div key={i} style={{
            flex: 1, height: 5,
            background: i < filled ? 'var(--cth-sky)' : 'var(--cth-cream-200)',
            boxShadow: 'inset 0 0 0 1px var(--cth-ink-900)'
          }} />
        ))}
      </div>
    );
  }

  return (
    <div style={{
      height: 5,
      background: 'var(--cth-cream-200)',
      boxShadow: 'inset 0 0 0 1px var(--cth-ink-900)'
    }} />
  );
}

function StudentCard({ agent, onSelect }: { agent: Agent; onSelect?: (id: string, title: string) => void }) {
  return (
    <button
      onClick={() => { void focusPane(agent.id); onSelect?.(agent.id, agent.name); }}
      className="cth-titlebar-nodrag"
      style={{
        width: 100, minWidth: 100,
        border: 'none', background: 'transparent', cursor: 'pointer', textAlign: 'left',
        padding: 0, flexShrink: 0
      }}
    >
      <div style={{
        height: '100%',
        boxShadow: agent.isGod
          ? 'inset 0 0 0 2px var(--cth-ink-900), 0 0 0 2px var(--cth-lemon)'
          : 'inset 0 0 0 2px var(--cth-ink-900)',
        background: 'var(--cth-cream-100)',
        display: 'flex', flexDirection: 'column'
      }}>
        {/* 일러스트 */}
        <div style={{
          height: 68,
          background: `var(--cth-${agent.accent}-light)`,
          display: 'flex', alignItems: 'flex-end', justifyContent: 'center',
          overflow: 'hidden', position: 'relative', flexShrink: 0
        }}>
          {agent.isGod && (
            <span style={{
              position: 'absolute', top: 3, left: 3,
              fontFamily: 'var(--cth-font-display)', fontSize: 6,
              background: 'var(--cth-lemon)', color: 'var(--cth-ink-900)',
              padding: '1px 4px', boxShadow: 'inset 0 0 0 1px var(--cth-ink-900)',
              lineHeight: '10px'
            }}>GOD</span>
          )}
          <SpritePortrait character={agent.character} scale={2} />
        </div>

        {/* 정보 */}
        <div style={{ padding: '4px 5px 5px', display: 'flex', flexDirection: 'column', gap: 2, flex: 1 }}>
          <div style={{
            fontFamily: 'var(--cth-font-display)', fontSize: 7, lineHeight: '11px',
            color: 'var(--cth-ink-900)',
            whiteSpace: 'nowrap', overflow: 'hidden', textOverflow: 'ellipsis'
          }}>{agent.name.toUpperCase()}</div>

          <div style={{
            fontSize: 9, fontFamily: 'var(--cth-font-ui)', lineHeight: '13px',
            color: 'var(--cth-ink-500)',
            whiteSpace: 'nowrap', overflow: 'hidden', textOverflow: 'ellipsis'
          }}>{agent.project}</div>

          <PixelBadge status={agent.status} style={{ fontSize: 8, padding: '1px 5px 0', lineHeight: '16px' }} />

          <div style={{
            fontSize: 9, fontFamily: 'var(--cth-font-ui)', lineHeight: '13px',
            color: 'var(--cth-ink-900)',
            whiteSpace: 'nowrap', overflow: 'hidden', textOverflow: 'ellipsis',
            minHeight: 13
          }}>{agent.status === 'idle' ? '' : (agent.action ?? '')}</div>

          <ProgressBar agent={agent} />
        </div>
      </div>
    </button>
  );
}

function StudentRow({ agent, onSelect }: { agent: Agent; onSelect?: (id: string, title: string) => void }) {
  return (
    <button
      onClick={() => { void focusPane(agent.id); onSelect?.(agent.id, agent.name); }}
      className="cth-titlebar-nodrag"
      style={{
        width: '100%', border: 'none', background: 'transparent', cursor: 'pointer', textAlign: 'left',
        padding: '4px 10px', display: 'flex', alignItems: 'center', gap: 8,
        boxShadow: 'inset 0 -1px 0 var(--cth-ink-100)'
      }}
    >
      <SpritePortrait character={agent.character} scale={1} />
      <span style={{
        fontFamily: 'var(--cth-font-display)', fontSize: 8, color: 'var(--cth-ink-900)',
        minWidth: 80, whiteSpace: 'nowrap', overflow: 'hidden', textOverflow: 'ellipsis'
      }}>{agent.name.toUpperCase()}</span>
      <PixelBadge status={agent.status} style={{ fontSize: 8, flexShrink: 0 }} />
      <span style={{
        flex: 1, fontSize: 10, fontFamily: 'var(--cth-font-ui)',
        color: 'var(--cth-ink-500)',
        whiteSpace: 'nowrap', overflow: 'hidden', textOverflow: 'ellipsis'
      }}>
        {agent.project}{agent.action ? ` — ${agent.action}` : ''}
      </span>
      <div style={{ width: 72, flexShrink: 0 }}>
        <ProgressBar agent={agent} />
      </div>
    </button>
  );
}

export function StudentGrid({ agents, onSelect }: StudentGridProps) {
  const [sort, setSort] = useState<SortKey>('god-first');
  const [view, setView] = useState<ViewKind>('grid');

  useEffect(() => {
    const id = 'cth-pulse-style';
    if (!document.getElementById(id)) {
      const s = document.createElement('style');
      s.id = id;
      s.textContent =
        '@keyframes cth-pulse{0%,100%{opacity:1}50%{opacity:.25}}' +
        '.cth-pulse{animation:cth-pulse 1.4s ease-in-out infinite}';
      document.head.appendChild(s);
    }
  }, []);

  const list = sorted(agents, sort);

  return (
    <div>
      {/* 컨트롤 바 */}
      <div style={{
        display: 'flex', alignItems: 'center', gap: 8,
        padding: '4px 10px',
        borderBottom: '1px solid var(--cth-ink-900)'
      }}>
        <span style={{
          fontFamily: 'var(--cth-font-display)', fontSize: 7,
          color: 'var(--cth-ink-500)', whiteSpace: 'nowrap'
        }}>
          학생 ({agents.length})
        </span>

        <select
          value={sort}
          onChange={(e) => setSort(e.target.value as SortKey)}
          style={{
            fontFamily: 'var(--cth-font-display)', fontSize: 7,
            padding: '2px 6px', border: 'none',
            boxShadow: 'inset 0 0 0 1px var(--cth-ink-900)',
            background: 'var(--cth-cream-100)', color: 'var(--cth-ink-900)',
            cursor: 'pointer'
          }}
        >
          <option value="god-first">God 우선</option>
          <option value="status">상태순</option>
          <option value="name">이름순</option>
        </select>

        <div style={{ flex: 1 }} />

        {(['grid', 'list'] as ViewKind[]).map((v) => (
          <button
            key={v}
            onClick={() => setView(v)}
            title={v === 'grid' ? '그리드' : '리스트'}
            style={{
              width: 24, height: 20, padding: 0, border: 'none', cursor: 'pointer',
              background: view === v ? 'var(--cth-sky)' : 'var(--cth-cream-200)',
              boxShadow: 'inset 0 0 0 1px var(--cth-ink-900)',
              fontFamily: 'var(--cth-font-display)', fontSize: 9, color: 'var(--cth-ink-900)'
            }}
          >
            {v === 'grid' ? '▦' : '≡'}
          </button>
        ))}
      </div>

      {/* 카드/리스트 */}
      {view === 'grid' ? (
        <div style={{
          display: 'flex', gap: 6, padding: '6px 10px',
          overflowX: 'auto', overflowY: 'hidden',
          alignItems: 'stretch'
        }}>
          {list.length === 0 ? (
            <span style={{
              fontFamily: 'var(--cth-font-ui)', fontSize: 11,
              color: 'var(--cth-ink-300)', padding: '10px 0'
            }}>
              학생 없음 — board 폴링 중
            </span>
          ) : list.map((a) => (
            <StudentCard key={a.id} agent={a} onSelect={onSelect} />
          ))}
        </div>
      ) : (
        <div style={{ maxHeight: 200, overflowY: 'auto' }}>
          {list.length === 0 ? (
            <div style={{
              padding: '10px', fontFamily: 'var(--cth-font-ui)',
              fontSize: 11, color: 'var(--cth-ink-300)'
            }}>학생 없음</div>
          ) : list.map((a) => (
            <StudentRow key={a.id} agent={a} onSelect={onSelect} />
          ))}
        </div>
      )}
    </div>
  );
}
