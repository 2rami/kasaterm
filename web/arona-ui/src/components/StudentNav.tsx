import type { Agent } from '@/store';
import { SpritePortrait } from './SpritePortrait';
import { focusPane } from '@/lib/mcp';

// 좌측 세로 학생 네비 — 터미널 뷰가 메인이 되면서(거노: "애들 돌아다니는 비율을
// 줄이자") 교실/하단 카드 대신 좁은 사이드 리스트로 학생을 건다. 작은 상반신 아바타
// + 이름 + 상태점. 클릭 → 그 pane 포커스(터미널) + 우측 디테일(peek) 동기화.
export interface StudentNavProps {
  agents: Agent[];
  selectedId?: string;
  onSelect?: (id: string, title: string) => void;
}

const STATUS_ORDER: Record<string, number> = {
  working: 0, thinking: 0, blocked: 1, waiting: 2, success: 3, idle: 4, ghost: 5,
};
const STATUS_COLOR: Record<string, string> = {
  working: 'var(--cth-mint)', thinking: 'var(--cth-mint)', waiting: 'var(--cth-sky)',
  blocked: 'var(--cth-coral)', success: 'var(--cth-lemon)', idle: 'var(--cth-ink-300)',
};
const STATUS_LABEL: Record<string, string> = {
  working: '작업', thinking: '생각', waiting: '대기', blocked: '막힘', success: '완료', idle: '쉬는 중',
};

export function StudentNav({ agents, selectedId, onSelect }: StudentNavProps) {
  const list = [...agents].sort((a, b) => {
    if (a.isGod !== b.isGod) return Number(b.isGod) - Number(a.isGod);
    return (STATUS_ORDER[a.status] ?? 6) - (STATUS_ORDER[b.status] ?? 6);
  });
  return (
    <div style={{
      width: 150, flexShrink: 0, height: '100%', overflowY: 'auto',
      borderRight: '1px solid var(--cth-cream-200)', background: 'var(--cth-cream-50)',
      padding: '8px 6px', display: 'flex', flexDirection: 'column', gap: 3,
    }}>
      <div style={{ fontFamily: 'var(--cth-font-display)', fontSize: 10, color: 'var(--cth-ink-500)', padding: '0 4px 4px' }}>
        학생 <b style={{ color: 'var(--cth-sky)' }}>{agents.length}</b>
      </div>
      {list.length === 0 ? (
        <div style={{ padding: '10px 4px', fontFamily: 'var(--cth-font-ui)', fontSize: 10, color: 'var(--cth-ink-300)' }}>
          board 폴링 중…
        </div>
      ) : list.map((a) => {
        const sel = selectedId === a.id;
        return (
          <button
            key={a.id}
            onClick={() => { void focusPane(a.id); onSelect?.(a.id, a.character); }}
            className="cth-titlebar-nodrag"
            style={{
              width: '100%', border: 'none', cursor: 'pointer', textAlign: 'left',
              display: 'flex', alignItems: 'center', gap: 8, padding: '6px 7px', borderRadius: 9,
              background: sel ? 'var(--cth-sky-light)' : 'transparent',
              boxShadow: sel ? 'inset 0 0 0 1.5px var(--cth-sky)' : 'none',
            }}
          >
            <div style={{
              width: 32, height: 32, flexShrink: 0, borderRadius: 7, overflow: 'hidden',
              background: `var(--cth-${a.accent}-light)`,
              display: 'flex', alignItems: 'center', justifyContent: 'center', position: 'relative',
            }}>
              {a.isGod && (
                <span style={{ position: 'absolute', top: 1, left: 2, fontSize: 7, color: 'var(--cth-lemon)', fontWeight: 800 }}>★</span>
              )}
              <SpritePortrait character={a.character} scale={1.5} bust />
            </div>
            <div style={{ flex: 1, minWidth: 0, display: 'flex', flexDirection: 'column', gap: 2 }}>
              <div style={{
                fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: 700, color: 'var(--cth-ink-900)',
                whiteSpace: 'nowrap', overflow: 'hidden', textOverflow: 'ellipsis',
              }}>{a.character}</div>
              <span style={{
                display: 'inline-flex', alignItems: 'center', gap: 4,
                fontFamily: 'var(--cth-font-ui)', fontSize: 9, fontWeight: 700, color: 'var(--cth-ink-500)',
              }}>
                <span style={{ width: 6, height: 6, borderRadius: 999, background: STATUS_COLOR[a.status] ?? 'var(--cth-ink-300)' }} />
                {STATUS_LABEL[a.status] ?? a.status}
              </span>
            </div>
          </button>
        );
      })}
    </div>
  );
}
