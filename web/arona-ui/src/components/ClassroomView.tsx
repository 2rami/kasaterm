import { useStore, type Agent } from '@/store';
import { SpritePortrait } from './SpritePortrait';

const ROOT = import.meta.env.BASE_URL || '/';

// classroom-bg(SCHALE 본부) 위 학생 자리 — 배경 책상 위치에 맞춘 % 좌표.
// leader(아로나)→members 순서로 0..5 에 배치. (앵커=발밑: translate(-50%,-100%))
const SEATS = [
  { x: 43, y: 60 }, { x: 63, y: 60 },
  { x: 50, y: 78 }, { x: 28, y: 74 },
  { x: 72, y: 74 }, { x: 25, y: 56 },
];

function firstLine(s?: string): string {
  if (!s) return '';
  const head = s.trim().split(/[\n。.!?！？]/)[0].trim();
  return head.length > 36 ? head.slice(0, 35).trimEnd() + '…' : head;
}
function shortenAction(s?: string): string {
  if (!s) return '';
  return s.replace(/\/\S*\/([^\s/]+)/g, '…/$1');
}
// 학생 경로 표시 — 마지막 1~2 세그먼트만(…/parent/folder).
function shortCwd(p?: string): string {
  if (!p) return '';
  const segs = p.split('/').filter(Boolean);
  return (segs.length > 2 ? '…/' : '') + segs.slice(-2).join('/');
}
function thoughtFor(a: Agent): string {
  switch (a.status) {
    case 'working': {
      const tool = a.currentTool || shortenAction(a.action);
      const n = a.subagents?.length ?? 0;
      return n > 0 ? `${tool} · 서브 ${n}` : tool;
    }
    case 'waiting':
    case 'blocked': return firstLine(a.lastReply) || shortenAction(a.action) || '대기 중';
    case 'idle': return firstLine(a.lastReply);
    default: return '';
  }
}

const STATUS_COLOR: Record<string, string> = {
  working: 'var(--cth-mint)', waiting: 'var(--cth-sky)', blocked: 'var(--cth-coral)',
  idle: 'var(--cth-ink-300)',
};

export interface ClassroomViewProps {
  onSelect?: (surfaceId: string, title: string) => void;
}

// 샬레 교실 — 배경 일러(CSS) 위에 학생을 책상 자리에 세운다. 캐릭터=네이티브 img,
// 말풍선·이름표=HTML(반응형·또렷). 클릭 → 우측 대화뷰(onSelect). Pixi 없음.
export function ClassroomView({ onSelect }: ClassroomViewProps) {
  const agents = useStore((s) => s.agents);
  const sorted = [...agents].sort((a, b) => Number(b.isGod) - Number(a.isGod));

  return (
    <div style={{
      position: 'relative', width: '100%', maxWidth: 960, margin: '0 auto',
      aspectRatio: '3 / 2',
      borderRadius: 16, overflow: 'hidden',
      backgroundImage: `url(${ROOT}assets/classroom-bg.png)`,
      backgroundSize: 'cover', backgroundPosition: 'center',
      boxShadow: '0 6px 20px rgba(21, 41, 74, 0.12), inset 0 0 0 1px var(--cth-cream-200)',
    }}>
      {sorted.slice(0, SEATS.length).map((a, i) => {
        const seat = SEATS[i];
        const thought = thoughtFor(a);
        return (
          <button
            key={a.id}
            onClick={() => onSelect?.(a.id, a.character)}
            title={`${a.character} — 클릭하면 대화`}
            style={{
              position: 'absolute', left: `${seat.x}%`, top: `${seat.y}%`,
              transform: 'translate(-50%, -100%)',
              display: 'flex', flexDirection: 'column', alignItems: 'center', gap: 3,
              border: 'none', background: 'transparent', cursor: 'pointer', padding: 0,
              width: 132,
            }}
          >
            {/* 말풍선 — 캐릭터 머리 위, 또렷하게 */}
            {thought && (
              <div style={{
                maxWidth: 150, padding: '6px 10px', borderRadius: 12,
                background: '#fff', border: '1px solid var(--cth-cream-200)',
                boxShadow: '0 2px 8px rgba(21, 41, 74, 0.14)',
                fontFamily: 'var(--cth-font-ui)', fontSize: 11, fontWeight: 500, lineHeight: 1.4,
                color: 'var(--cth-ink-900)', textAlign: 'center',
                whiteSpace: 'normal', wordBreak: 'break-word',
                ...(a.status === 'working' && { color: 'var(--cth-ink-900)' }),
              }}>
                {a.currentTool && a.status === 'working' ? (
                  <span><b style={{ color: 'var(--cth-sky)' }}>{a.currentTool}</b>{a.subagents?.length ? ` · 서브 ${a.subagents.length}` : ''}</span>
                ) : thought}
              </div>
            )}

            {/* 캐릭터 — 네이티브 img(또렷) */}
            <div style={{ width: 72, height: 100, display: 'flex', alignItems: 'flex-end', justifyContent: 'center', filter: 'drop-shadow(0 4px 6px rgba(21,41,74,0.2))' }}>
              <SpritePortrait character={a.character} scale={3.4} />
            </div>

            {/* 이름표 + 상태 점 */}
            <div style={{
              display: 'flex', alignItems: 'center', gap: 5,
              background: 'rgba(255,255,255,0.9)', borderRadius: 9, padding: '2px 9px',
              boxShadow: '0 1px 4px rgba(21, 41, 74, 0.12)',
              fontFamily: 'var(--cth-font-ui)', fontSize: 11, fontWeight: 700, color: 'var(--cth-ink-900)',
            }}>
              <span style={{ width: 7, height: 7, borderRadius: 999, background: STATUS_COLOR[a.status] ?? 'var(--cth-ink-300)' }} />
              {a.isGod && <span style={{ fontSize: 9, color: 'var(--cth-lemon)', fontWeight: 800 }}>★</span>}
              {a.character}
            </div>

            {/* 경로(cwd) — 학생이 작업 중인 디렉터리 */}
            {a.cwd && (
              <div style={{
                marginTop: 2, padding: '1px 7px', borderRadius: 7,
                background: 'rgba(255,255,255,0.82)', boxShadow: '0 1px 3px rgba(21,41,74,0.1)',
                fontFamily: 'var(--cth-font-mono)', fontSize: 9, color: 'var(--cth-ink-500)',
                maxWidth: 130, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap',
              }}>{shortCwd(a.cwd)}</div>
            )}
          </button>
        );
      })}

      {sorted.length === 0 && (
        <div style={{ position: 'absolute', inset: 0, display: 'flex', alignItems: 'center', justifyContent: 'center', color: 'var(--cth-ink-500)', fontFamily: 'var(--cth-font-ui)', fontSize: 14, background: 'rgba(255,255,255,0.4)' }}>
          학생들을 기다리는 중…
        </div>
      )}
    </div>
  );
}
