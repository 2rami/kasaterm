import { useCallback, useEffect, useRef, useState } from 'react';
import { useStore, type Agent } from '@/store';
import { SpriteWalk } from './SpriteWalk';

const ROOT = import.meta.env.BASE_URL || '/';

// classroom-bg(SCHALE 본부) 위 책상 자리 — leader(아로나)→members 0..5. 발밑 앵커.
const SEATS = [
  { x: 43, y: 60 }, { x: 63, y: 60 },
  { x: 50, y: 78 }, { x: 28, y: 74 },
  { x: 72, y: 74 }, { x: 25, y: 56 },
];
// idle 배회 영역(책상 아래 열린 바닥).
const FLOOR = { x0: 18, x1: 82, y0: 64, y1: 86 };
const MOVE_MS = 2200;

function firstLine(s?: string): string {
  if (!s) return '';
  const head = s.trim().split(/[\n。.!?！？]/)[0].trim();
  return head.length > 36 ? head.slice(0, 35).trimEnd() + '…' : head;
}
function shortenAction(s?: string): string {
  if (!s) return '';
  return s.replace(/\/\S*\/([^\s/]+)/g, '…/$1');
}
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
  success: 'var(--cth-lemon)', idle: 'var(--cth-ink-300)',
};
// munder 식 상태 글리프 — 머리 위 한눈 표시.
const GLYPH: Record<string, { t: string; c: string; pulse?: boolean }> = {
  blocked: { t: '!', c: 'var(--cth-coral)', pulse: true },
  waiting: { t: '?', c: 'var(--cth-sky)', pulse: true },
  success: { t: '✨', c: 'var(--cth-lemon)' },
};

// 교실 캐릭터(munder Character 이식) — working/waiting/blocked = 자기 책상으로
// 이동해 일함, idle = 바닥을 돌아다님(주기적 배회). 이동 중엔 워크 애니메이션 +
// 진행방향 flip. 머리 위 말풍선(현재 tool/상태) + 상태 글리프. 클릭 → 대화.
function ClassroomCharacter({ agent, seat, onSelect }: { agent: Agent; seat: { x: number; y: number }; onSelect?: (id: string, title: string) => void }) {
  const atDesk = agent.status !== 'idle'; // idle 만 배회, 그 외(work/wait/blocked)는 자리
  const [pos, setPos] = useState(seat);
  const [moving, setMoving] = useState(false);
  const [flip, setFlip] = useState(false);
  const posRef = useRef(pos);
  posRef.current = pos;
  const timer = useRef<number | undefined>(undefined);

  const goTo = useCallback((t: { x: number; y: number }) => {
    const cur = posRef.current;
    if (Math.abs(t.x - cur.x) < 0.6 && Math.abs(t.y - cur.y) < 0.6) return;
    setFlip(t.x < cur.x);
    setMoving(true);
    setPos(t);
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setMoving(false), MOVE_MS);
  }, []);

  useEffect(() => {
    if (atDesk) { goTo(seat); return; }
    const wander = () => goTo({
      x: FLOOR.x0 + Math.random() * (FLOOR.x1 - FLOOR.x0),
      y: FLOOR.y0 + Math.random() * (FLOOR.y1 - FLOOR.y0),
    });
    wander();
    const iv = window.setInterval(wander, 4200);
    return () => { window.clearInterval(iv); window.clearTimeout(timer.current); };
  }, [atDesk, seat.x, seat.y, goTo]);

  const thought = thoughtFor(agent);
  const glyph = GLYPH[agent.status];

  return (
    <button
      onClick={() => onSelect?.(agent.id, agent.character)}
      title={`${agent.character} — 클릭하면 대화`}
      style={{
        position: 'absolute', left: `${pos.x}%`, top: `${pos.y}%`,
        transform: 'translate(-50%, -100%)',
        transition: `left ${MOVE_MS}ms ease-in-out, top ${MOVE_MS}ms ease-in-out`,
        display: 'flex', flexDirection: 'column', alignItems: 'center', gap: 3,
        border: 'none', background: 'transparent', cursor: 'pointer', padding: 0,
        width: 132, zIndex: Math.round(pos.y),
      }}
    >
      {/* 상태 글리프 */}
      {glyph && (
        <div style={{
          fontSize: 14, fontWeight: 900, color: glyph.c, lineHeight: 1, marginBottom: 1,
          animation: glyph.pulse ? 'schale-glyph-pulse 1s ease-in-out infinite' : undefined,
          textShadow: '0 1px 2px rgba(255,255,255,0.9)',
        }}>{glyph.t}</div>
      )}

      {/* 말풍선 */}
      {thought && (
        <div style={{
          maxWidth: 150, padding: '6px 10px', borderRadius: 12,
          background: '#fff', border: '1px solid var(--cth-cream-200)',
          boxShadow: '0 2px 8px rgba(21, 41, 74, 0.14)',
          fontFamily: 'var(--cth-font-ui)', fontSize: 11, fontWeight: 500, lineHeight: 1.4,
          color: 'var(--cth-ink-900)', textAlign: 'center',
          whiteSpace: 'normal', wordBreak: 'break-word',
        }}>
          {agent.currentTool && agent.status === 'working' ? (
            <span><b style={{ color: 'var(--cth-sky)' }}>{agent.currentTool}</b>{agent.subagents?.length ? ` · 서브 ${agent.subagents.length}` : ''}</span>
          ) : thought}
        </div>
      )}

      {/* 캐릭터 — 이동 중 워크 애니메이션 */}
      <div style={{ display: 'flex', alignItems: 'flex-end', justifyContent: 'center' }}>
        <SpriteWalk character={agent.character} walking={moving} flip={flip} width={72} height={100} />
      </div>

      {/* 이름표 + 상태 점 */}
      <div style={{
        display: 'flex', alignItems: 'center', gap: 5,
        background: 'rgba(255,255,255,0.9)', borderRadius: 9, padding: '2px 9px',
        boxShadow: '0 1px 4px rgba(21, 41, 74, 0.12)',
        fontFamily: 'var(--cth-font-ui)', fontSize: 11, fontWeight: 700, color: 'var(--cth-ink-900)',
      }}>
        <span style={{ width: 7, height: 7, borderRadius: 999, background: STATUS_COLOR[agent.status] ?? 'var(--cth-ink-300)' }} />
        {agent.isGod && <span style={{ fontSize: 9, color: 'var(--cth-lemon)', fontWeight: 800 }}>★</span>}
        {agent.character}
      </div>

      {/* 경로(cwd) */}
      {agent.cwd && (
        <div style={{
          marginTop: 2, padding: '1px 7px', borderRadius: 7,
          background: 'rgba(255,255,255,0.82)', boxShadow: '0 1px 3px rgba(21,41,74,0.1)',
          fontFamily: 'var(--cth-font-mono)', fontSize: 9, color: 'var(--cth-ink-500)',
          maxWidth: 130, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap',
        }}>{shortCwd(agent.cwd)}</div>
      )}
    </button>
  );
}

export interface ClassroomViewProps {
  onSelect?: (surfaceId: string, title: string) => void;
}

// 샬레 교실 — 배경 일러 위에 학생들이 일하러 책상에 가거나(working) 바닥을 돌아다님
// (idle). munder Office 패턴(상태→위치/애니/말풍선/글리프) 이식. 클릭 → 우측 대화.
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
      {sorted.slice(0, SEATS.length).map((a, i) => (
        <ClassroomCharacter key={a.id} agent={a} seat={SEATS[i]} onSelect={onSelect} />
      ))}

      {sorted.length === 0 && (
        <div style={{ position: 'absolute', inset: 0, display: 'flex', alignItems: 'center', justifyContent: 'center', color: 'var(--cth-ink-500)', fontFamily: 'var(--cth-font-ui)', fontSize: 14, background: 'rgba(255,255,255,0.4)' }}>
          학생들을 기다리는 중…
        </div>
      )}
    </div>
  );
}
