import { useState } from 'react';
import type { Agent } from '@/store';
import { SpritePortrait } from './SpritePortrait';
import { focusPane } from '@/lib/mcp';
import { isBuildCmd, BUILD_COLOR, GearIcon, SpinIcon, ForkIcon } from './activity';

export interface StudentGridProps {
  agents: Agent[];
  onSelect?: (id: string, title: string) => void;
}

type SortKey = 'god-first' | 'status' | 'name';
type ViewKind = 'grid' | 'list';

const STATUS_ORDER: Record<string, number> = {
  working: 0, thinking: 0, blocked: 1, waiting: 2, idle: 3, ghost: 4,
};
const STATUS_COLOR: Record<string, string> = {
  working: 'var(--cth-mint)', thinking: 'var(--cth-mint)', waiting: 'var(--cth-sky)',
  blocked: 'var(--cth-coral)', success: 'var(--cth-lemon)', idle: 'var(--cth-ink-300)',
};
const STATUS_LABEL: Record<string, string> = {
  working: '작업', thinking: '생각', waiting: '대기', blocked: '막힘', success: '완료', idle: '쉬는 중',
};

function sortAgents(agents: Agent[], key: SortKey): Agent[] {
  return [...agents].sort((a, b) => {
    if (key === 'god-first') {
      if (a.isGod !== b.isGod) return Number(b.isGod) - Number(a.isGod);
      return (STATUS_ORDER[a.status] ?? 5) - (STATUS_ORDER[b.status] ?? 5);
    }
    if (key === 'name') return a.name.localeCompare(b.name);
    return (STATUS_ORDER[a.status] ?? 5) - (STATUS_ORDER[b.status] ?? 5);
  });
}

function StatusPill({ status }: { status: string }) {
  return (
    <span style={{
      display: 'inline-flex', alignItems: 'center', gap: 4,
      fontFamily: 'var(--cth-font-ui)', fontSize: 10, fontWeight: 700,
      color: 'var(--cth-ink-700)', whiteSpace: 'nowrap',
    }}>
      <span style={{ width: 7, height: 7, borderRadius: 999, background: STATUS_COLOR[status] ?? 'var(--cth-ink-300)' }} />
      {STATUS_LABEL[status] ?? status}
    </span>
  );
}

// 활동 칩 — 빌드/현재도구 + 백그라운드 + 서브에이전트(가시화 파이프 재사용).
function ActivityChips({ agent }: { agent: Agent }) {
  const building = agent.status === 'working' && isBuildCmd(agent.action);
  const chip = (bg: string, color: string, key: string, body: React.ReactNode) => (
    <span key={key} style={{
      display: 'inline-flex', alignItems: 'center', gap: 3,
      fontFamily: 'var(--cth-font-ui)', fontSize: 9, fontWeight: 700, color,
      background: bg, padding: '1px 6px', borderRadius: 5, whiteSpace: 'nowrap',
    }}>{body}</span>
  );
  return (
    <div style={{ display: 'flex', flexWrap: 'wrap', gap: 3, minHeight: 16 }}>
      {building
        ? chip('color-mix(in srgb, #E5923A 14%, #fff)', BUILD_COLOR, 'b', <><GearIcon size={10} />빌드</>)
        : agent.currentTool
          ? chip('color-mix(in srgb, var(--cth-sky) 12%, #fff)', 'var(--cth-sky)', 't', agent.currentTool)
          : null}
      {!!agent.background?.length && chip('color-mix(in srgb, #E5923A 14%, #fff)', BUILD_COLOR, 'bg',
        <><SpinIcon size={9} />bg {agent.background.length}</>)}
      {!!agent.subagents?.length && chip('color-mix(in srgb, var(--cth-lilac) 14%, #fff)', 'var(--cth-lilac)', 's',
        <><ForkIcon size={9} />{agent.subagents.length}</>)}
    </div>
  );
}

function ContextBar({ agent }: { agent: Agent }) {
  const pct = agent.contextPct ?? (agent.contextTokens && agent.contextLimit
    ? Math.round((agent.contextTokens / agent.contextLimit) * 100) : 0);
  if (!pct) return <div style={{ height: 6 }} />;
  const fill = pct >= 87 ? 'var(--cth-coral)' : pct >= 75 ? 'var(--cth-lemon)' : 'var(--cth-sky)';
  return (
    <div style={{ display: 'flex', alignItems: 'center', gap: 5 }}>
      <div style={{ flex: 1, height: 6, borderRadius: 999, background: 'var(--cth-cream-200)', overflow: 'hidden' }}>
        <div style={{ height: '100%', width: `${pct}%`, background: fill, borderRadius: 999, transition: 'width 0.4s ease' }} />
      </div>
      <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 9, color: 'var(--cth-ink-500)', flexShrink: 0 }}>{pct}%</span>
    </div>
  );
}

function StudentCard({ agent, onSelect, bust }: { agent: Agent; onSelect?: (id: string, title: string) => void; bust?: boolean }) {
  const accent = `var(--cth-${agent.accent})`;
  return (
    <button
      onClick={() => { void focusPane(agent.id); onSelect?.(agent.id, agent.name); }}
      className="cth-titlebar-nodrag"
      style={{
        width: 168, minWidth: 168, flexShrink: 0,
        border: 'none', padding: 0, cursor: 'pointer', textAlign: 'left',
        background: 'var(--cth-cream-50)', borderRadius: 12, overflow: 'hidden',
        boxShadow: agent.isGod
          ? '0 2px 10px rgba(21,41,74,0.12), inset 0 0 0 2px var(--cth-lemon)'
          : '0 2px 10px rgba(21,41,74,0.1), inset 0 0 0 1px var(--cth-cream-200)',
        display: 'flex', flexDirection: 'column',
      }}
    >
      <div style={{ height: 4, background: accent, flexShrink: 0 }} />
      <div style={{ display: 'flex', gap: 8, padding: '8px 9px 0' }}>
        <div style={{
          width: 46, height: 56, flexShrink: 0, borderRadius: 8,
          background: `var(--cth-${agent.accent}-light)`,
          display: 'flex', alignItems: 'flex-end', justifyContent: 'center', overflow: 'hidden', position: 'relative',
        }}>
          {agent.isGod && (
            <span style={{ position: 'absolute', top: 2, left: 2, fontFamily: 'var(--cth-font-display)', fontSize: 7, color: 'var(--cth-lemon)', fontWeight: 800 }}>★</span>
          )}
          <SpritePortrait character={agent.character} scale={1.8} bust={bust} />
        </div>
        <div style={{ flex: 1, minWidth: 0, display: 'flex', flexDirection: 'column', gap: 2 }}>
          <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: 700, color: 'var(--cth-ink-900)', whiteSpace: 'nowrap', overflow: 'hidden', textOverflow: 'ellipsis' }}>{agent.name}</div>
          <StatusPill status={agent.status} />
          <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 10, color: 'var(--cth-ink-500)', whiteSpace: 'nowrap', overflow: 'hidden', textOverflow: 'ellipsis', minHeight: 13 }}>{agent.project || '대기 중'}</div>
        </div>
      </div>
      <div style={{ padding: '5px 9px 8px', display: 'flex', flexDirection: 'column', gap: 5 }}>
        <ActivityChips agent={agent} />
        <ContextBar agent={agent} />
      </div>
    </button>
  );
}

function StudentRow({ agent, onSelect, bust }: { agent: Agent; onSelect?: (id: string, title: string) => void; bust?: boolean }) {
  return (
    <button
      onClick={() => { void focusPane(agent.id); onSelect?.(agent.id, agent.name); }}
      className="cth-titlebar-nodrag"
      style={{
        width: '100%', border: 'none', background: 'transparent', cursor: 'pointer', textAlign: 'left',
        padding: '6px 10px', display: 'flex', alignItems: 'center', gap: 10,
        borderBottom: '1px solid var(--cth-cream-200)',
      }}
    >
      <div style={{ width: 30, height: 36, flexShrink: 0, display: 'flex', alignItems: 'flex-end', justifyContent: 'center' }}>
        <SpritePortrait character={agent.character} scale={1.3} bust={bust} />
      </div>
      <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: 700, color: 'var(--cth-ink-900)', minWidth: 70, whiteSpace: 'nowrap', overflow: 'hidden', textOverflow: 'ellipsis' }}>
        {agent.isGod && <span style={{ color: 'var(--cth-lemon)' }}>★ </span>}{agent.name}
      </span>
      <div style={{ width: 60, flexShrink: 0 }}><StatusPill status={agent.status} /></div>
      <span style={{ flex: 1, fontSize: 11, fontFamily: 'var(--cth-font-ui)', color: 'var(--cth-ink-500)', whiteSpace: 'nowrap', overflow: 'hidden', textOverflow: 'ellipsis' }}>
        {agent.project}{agent.action ? ` — ${agent.action}` : ''}
      </span>
      <div style={{ flexShrink: 0 }}><ActivityChips agent={agent} /></div>
    </button>
  );
}

export function StudentGrid({ agents, onSelect }: StudentGridProps) {
  const [sort, setSort] = useState<SortKey>('god-first');
  const [view, setView] = useState<ViewKind>('grid');
  const [bust, setBust] = useState(false);
  const list = sortAgents(agents, sort);

  const ViewBtn = ({ v, children }: { v: ViewKind; children: React.ReactNode }) => (
    <button
      onClick={() => setView(v)}
      title={v === 'grid' ? '카드' : '리스트'}
      style={{
        width: 26, height: 22, padding: 0, border: 'none', cursor: 'pointer', borderRadius: 6,
        background: view === v ? 'var(--cth-sky)' : 'var(--cth-cream-100)',
        color: view === v ? '#fff' : 'var(--cth-ink-500)',
        display: 'inline-flex', alignItems: 'center', justifyContent: 'center',
      }}
    >{children}</button>
  );

  return (
    <div>
      {/* 컨트롤 바 */}
      <div style={{ display: 'flex', alignItems: 'center', gap: 8, padding: '6px 12px', borderBottom: '1px solid var(--cth-cream-200)' }}>
        <span style={{ fontFamily: 'var(--cth-font-display)', fontSize: 11, color: 'var(--cth-ink-700)', whiteSpace: 'nowrap' }}>
          학생 <b style={{ color: 'var(--cth-sky)' }}>{agents.length}</b>
        </span>
        <select
          value={sort}
          onChange={(e) => setSort(e.target.value as SortKey)}
          style={{
            fontFamily: 'var(--cth-font-ui)', fontSize: 11, padding: '3px 8px', borderRadius: 7,
            border: '1px solid var(--cth-cream-200)', background: 'var(--cth-cream-50)', color: 'var(--cth-ink-700)', cursor: 'pointer',
          }}
        >
          <option value="god-first">God 우선</option>
          <option value="status">상태순</option>
          <option value="name">이름순</option>
        </select>
        <div style={{ flex: 1 }} />
        <button
          onClick={() => setBust((v) => !v)}
          title={bust ? '전신' : '상반신'}
          style={{
            height: 22, padding: '0 8px', border: 'none', cursor: 'pointer', borderRadius: 6,
            background: bust ? 'var(--cth-sky)' : 'var(--cth-cream-100)',
            color: bust ? '#fff' : 'var(--cth-ink-500)',
            fontFamily: 'var(--cth-font-ui)', fontSize: 10, fontWeight: 700,
          }}
        >{bust ? '상반신' : '전신'}</button>
        <ViewBtn v="grid"><svg width="13" height="13" viewBox="0 0 14 14"><rect x="1" y="1" width="5" height="5" rx="1" fill="currentColor" /><rect x="8" y="1" width="5" height="5" rx="1" fill="currentColor" /><rect x="1" y="8" width="5" height="5" rx="1" fill="currentColor" /><rect x="8" y="8" width="5" height="5" rx="1" fill="currentColor" /></svg></ViewBtn>
        <ViewBtn v="list"><svg width="13" height="13" viewBox="0 0 14 14"><rect x="1" y="2" width="12" height="2" rx="1" fill="currentColor" /><rect x="1" y="6" width="12" height="2" rx="1" fill="currentColor" /><rect x="1" y="10" width="12" height="2" rx="1" fill="currentColor" /></svg></ViewBtn>
      </div>

      {list.length === 0 ? (
        <div style={{ padding: '14px 12px', fontFamily: 'var(--cth-font-ui)', fontSize: 11, color: 'var(--cth-ink-300)' }}>학생 없음 — board 폴링 중</div>
      ) : view === 'grid' ? (
        <div style={{ display: 'flex', gap: 8, padding: '8px 12px', overflowX: 'auto', overflowY: 'hidden', alignItems: 'stretch' }}>
          {list.map((a) => <StudentCard key={a.id} agent={a} onSelect={onSelect} bust={bust} />)}
        </div>
      ) : (
        <div style={{ maxHeight: 196, overflowY: 'auto' }}>
          {list.map((a) => <StudentRow key={a.id} agent={a} onSelect={onSelect} bust={bust} />)}
        </div>
      )}
    </div>
  );
}
