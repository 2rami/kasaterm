import type { Agent } from '@/store';

export interface Workspace { cwd: string; name: string; count: number; }

// 학생들의 cwd 로 워크스페이스(장소/방) 목록을 만든다 — distinct cwd = 분리된 방.
export function workspacesFromAgents(agents: Agent[]): Workspace[] {
  const map = new Map<string, number>();
  for (const a of agents) {
    const cwd = a.cwd || '';
    if (!cwd) continue;
    map.set(cwd, (map.get(cwd) ?? 0) + 1);
  }
  return [...map.entries()]
    .map(([cwd, count]) => ({ cwd, count, name: cwd.split('/').filter(Boolean).slice(-1)[0] || cwd }))
    .sort((a, b) => a.name.localeCompare(b.name));
}

function RoomIcon({ active }: { active: boolean }) {
  const c = active ? '#fff' : 'var(--cth-sky)';
  return (
    <svg width="18" height="18" viewBox="0 0 18 18" style={{ flexShrink: 0 }}>
      <path d="M2 8 9 3l7 5v6a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1V8Z" fill="none" stroke={c} strokeWidth="1.4" strokeLinejoin="round" />
      <rect x="6.5" y="10" width="5" height="4" fill="none" stroke={c} strokeWidth="1.2" />
    </svg>
  );
}

export interface WorkspaceNavProps {
  workspaces: Workspace[];
  active: string | null; // null = 전체
  onSelect: (cwd: string | null) => void;
}

// 좌측 방 네비 — 워크스페이스(cwd)별 장소 전환. '전체' + 방마다 학생 수.
export function WorkspaceNav({ workspaces, active, onSelect }: WorkspaceNavProps) {
  if (workspaces.length <= 1) return null; // 방 하나면 네비 숨김
  const Item = ({ cwd, name, count }: { cwd: string | null; name: string; count: number }) => {
    const on = active === cwd;
    return (
      <button
        onClick={() => onSelect(cwd)}
        title={cwd ?? '전체'}
        style={{
          display: 'flex', alignItems: 'center', gap: 8, width: '100%',
          padding: '8px 10px', border: 'none', borderRadius: 10, cursor: 'pointer', textAlign: 'left',
          background: on ? 'var(--cth-sky)' : 'transparent', color: on ? '#fff' : 'var(--cth-ink-700)',
          fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: 600,
        }}
        onMouseEnter={(e) => { if (!on) e.currentTarget.style.background = 'var(--cth-cream-100)'; }}
        onMouseLeave={(e) => { if (!on) e.currentTarget.style.background = 'transparent'; }}
      >
        <RoomIcon active={on} />
        <span style={{ flex: 1, minWidth: 0, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{name}</span>
        <span style={{ fontSize: 10, fontWeight: 700, opacity: 0.8 }}>{count}</span>
      </button>
    );
  };
  const total = workspaces.reduce((s, w) => s + w.count, 0);
  return (
    <div style={{
      width: 168, flexShrink: 0, height: '100%', overflowY: 'auto',
      borderRight: '1px solid var(--cth-cream-200)', background: 'var(--cth-cream-50)',
      padding: '10px 8px', display: 'flex', flexDirection: 'column', gap: 3,
    }}>
      <div style={{ fontFamily: 'var(--cth-font-display)', fontSize: 10, color: 'var(--cth-ink-500)', padding: '2px 8px 6px' }}>
        장소 (워크스페이스)
      </div>
      <Item cwd={null} name="전체" count={total} />
      {workspaces.map((w) => <Item key={w.cwd} cwd={w.cwd} name={w.name} count={w.count} />)}
    </div>
  );
}
