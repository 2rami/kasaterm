import { PixelPanel } from './PixelPanel';
import { PixelBadge, StatusKind } from './PixelBadge';
import { SpritePortrait } from './SpritePortrait';
import { AccentColorName } from '@/design/tokens';

export interface AgentCardProps {
  name: string;
  character: string;
  accent: AccentColorName;
  status: StatusKind;
  project: string;
  action?: string;
  /** Context gauge: 0..8 segments filled (session context ÷ context limit). */
  progress?: number;
  /** Live context size (tokens) — shown in the gauge tooltip. */
  contextTokens?: number;
  /** Context-window limit (tokens) assumed for the agent's model. */
  contextLimit?: number;
  selected?: boolean;
  /** The orchestrator — gets a persistent accent frame + GOD tag so it stands out. */
  isGod?: boolean;
  onClick?: () => void;
  /** Number of ledger tasks this agent is actively DOING — rendered as a blue
   *  sticky note stuck to the card. Clicking it opens the first task's detail. */
  doingCount?: number;
  onTaskNoteClick?: () => void;
}

const fmtK = (n: number): string => `${Math.round(n / 1000)}k`;

export function AgentCard({
  name, character, accent, status, project, action, progress = 0,
  contextTokens, contextLimit, selected, isGod, onClick,
  doingCount = 0, onTaskNoteClick
}: AgentCardProps) {
  // The god is always framed (stands out from the row); others only when selected.
  const framed = isGod || selected;

  return (
    <button
      onClick={onClick}
      className="cth-titlebar-nodrag"
      style={{
        width: 220, minWidth: 220, height: 96,
        padding: 0, border: 'none', background: 'transparent', cursor: 'pointer', textAlign: 'left',
        position: 'relative'
      }}
    >
      {/* The taken note, stuck to the card like on the desk: this worker is
          actively DOING a ledger task. Click → the task's detail overlay. */}
      {doingCount > 0 && (
        <span
          title={`actively working ${doingCount} task${doingCount === 1 ? '' : 's'} — click to open`}
          onClick={(e) => { e.stopPropagation(); onTaskNoteClick?.(); }}
          style={{
            position: 'absolute', right: -4, bottom: -5, zIndex: 2,
            width: 22, height: 20,
            background: 'var(--cth-sky)', borderRadius: 6,
            boxShadow: '0 2px 6px rgba(74, 144, 226, 0.35)',
            transform: 'rotate(4deg)',
            display: 'flex', alignItems: 'center', justifyContent: 'center',
            fontFamily: 'var(--cth-font-ui)', fontSize: 11, fontWeight: 700, color: '#fff',
            cursor: 'pointer'
          }}
        >
          {doingCount > 1 ? doingCount : '✎'}
        </span>
      )}
      <PixelPanel
        variant={framed ? 'active' : 'default'}
        accent={framed ? accent : undefined}
        style={{ height: '100%', padding: 8 }}
        noPadding
      >
        <div style={{ display: 'flex', gap: 8, height: '100%' }}>
          <div style={{
            width: 44, height: 64, borderRadius: 8,
            background: `var(--cth-${accent}-light)`,
            boxShadow: 'inset 0 0 0 1px var(--cth-cream-300)',
            display: 'flex', alignItems: 'flex-end', justifyContent: 'center', overflow: 'hidden',
            flexShrink: 0
          }}>
            <SpritePortrait character={character} scale={2} />
          </div>
          <div style={{ flex: 1, display: 'flex', flexDirection: 'column', gap: 2, minWidth: 0 }}>
            <div style={{ display: 'flex', alignItems: 'center', gap: 6, justifyContent: 'space-between' }}>
              <span style={{
                fontFamily: 'var(--cth-font-display)',
                fontSize: 'var(--cth-text-display-sm)',
                lineHeight: 'var(--cth-lh-display-sm)',
                color: 'var(--cth-ink-900)',
                whiteSpace: 'nowrap',
                overflow: 'hidden',
                textOverflow: 'ellipsis'
              }}>{name.toUpperCase()}</span>
              <PixelBadge status={status} />
            </div>

            <div style={{
              display: 'flex', alignItems: 'center', gap: 5,
              fontSize: 'var(--cth-text-body-sm)',
              lineHeight: '16px',
              color: 'var(--cth-ink-500)',
              whiteSpace: 'nowrap', overflow: 'hidden', textOverflow: 'ellipsis'
            }}>
              {isGod && (
                <span style={{
                  fontFamily: 'var(--cth-font-ui)', fontSize: 9, fontWeight: 700, lineHeight: '14px',
                  background: `var(--cth-${accent})`, color: '#fff',
                  padding: '0 6px', borderRadius: 5, flexShrink: 0
                }}>GOD</span>
              )}
              <span style={{ overflow: 'hidden', textOverflow: 'ellipsis' }}>{project}</span>
            </div>

            <div style={{
              fontSize: 'var(--cth-text-body-sm)',
              lineHeight: '16px',
              // Reserve the line even when empty (idle has no action text) —
              // otherwise it collapses and the context gauge below jumps up.
              minHeight: 16,
              color: 'var(--cth-ink-900)',
              whiteSpace: 'nowrap',
              overflow: 'hidden',
              textOverflow: 'ellipsis'
            }}>{/* The "idle" badge already conveys idle — don't echo "awaiting". */
              (status === 'idle' ? '' : action) || ' '}</div>

            {/* Context gauge: how full the session's context window is. Accent
                while comfortable, lemon from 6/8 (~75%), coral from 7/8 —
                "compaction imminent". Pinned to the card's bottom line so it
                never moves, whatever the lines above do. */}
            <div
              style={{ display: 'flex', gap: 2, marginTop: 'auto' }}
              title={contextTokens !== undefined && contextLimit
                ? `Context: ${fmtK(contextTokens)} / ${fmtK(contextLimit)} tokens (${Math.round((contextTokens / contextLimit) * 100)}%)`
                : 'Context gauge — fills once the agent reports activity'}
            >
              {Array.from({ length: 8 }).map((_, i) => (
                <div key={i} style={{
                  width: 14, height: 6, borderRadius: 3,
                  background: i < progress
                    ? (progress >= 7 ? 'var(--cth-coral)' : progress >= 6 ? 'var(--cth-lemon)' : `var(--cth-${accent})`)
                    : 'var(--cth-cream-200)'
                }}/>
              ))}
            </div>
          </div>
        </div>
      </PixelPanel>
    </button>
  );
}
