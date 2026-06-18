import { TerminalPeekPanel } from './TerminalPeekPanel';
import type { Agent } from '@/store';
import type { PaneRect } from '@/lib/mcp';

// 터미널 pane split 배치를 그대로 미러한 그리드 — 각 pane = 그 세션의 TerminalPeekPanel.
// PaneRect 는 window_layout 이 준 % 좌표라(터미널 분할 비율 그대로) position:absolute 로
// 배치만 하면 터미널이 좌우 2분할이면 BA 도 좌우 2칸, 같은 비율로 미러된다.
export function PaneGrid({ panes, agents, selectedId, onSelect }: {
  panes: PaneRect[];
  agents: Agent[];
  selectedId?: string;
  onSelect?: (id: string, title: string) => void;
}) {
  if (!panes.length) {
    return (
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', height: '100%', color: 'var(--cth-ink-300)', fontFamily: 'var(--cth-font-ui)', fontSize: 13 }}>
        터미널 pane 을 기다리는 중…
      </div>
    );
  }
  return (
    <div style={{ position: 'relative', width: '100%', height: '100%' }}>
      {panes.map((p) => {
        const agent = agents.find((a) => a.id === p.surface_id);
        const title = agent?.character || agent?.name || p.surface_id;
        const sel = selectedId === p.surface_id;
        return (
          <div
            key={p.surface_id}
            style={{
              position: 'absolute',
              left: `${p.x}%`, top: `${p.y}%`, width: `${p.w}%`, height: `${p.h}%`,
              padding: 3, boxSizing: 'border-box',
            }}
          >
            <div
              onMouseDown={() => onSelect?.(p.surface_id, title)}
              style={{
                display: 'flex', width: '100%', height: '100%',
                borderRadius: 10, overflow: 'hidden',
                border: sel ? '2px solid var(--cth-sky)' : '1px solid var(--cth-cream-200)',
                boxShadow: '0 1px 4px rgba(21,41,74,0.08)',
                background: 'var(--cth-cream-50)',
              }}
            >
              <TerminalPeekPanel surfaceId={p.surface_id} title={title} embedded onClose={() => {}} />
            </div>
          </div>
        );
      })}
    </div>
  );
}
