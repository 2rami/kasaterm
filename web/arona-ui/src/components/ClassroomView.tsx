import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useStore, type Agent } from '@/store';
import { SpriteWalk } from './SpriteWalk';
import {
  CLASSROOM_FURNITURE, buildGrid, deskSeats, cafeSpots, findPath,
  type Furniture, type CafeSpot,
} from './classroomSpace';
import { pickExchange } from './cafeteriaLines';
import { isBuildCmd, BUILD_COLOR, GearIcon, SpinIcon } from './activity';

const ROOT = import.meta.env.BASE_URL || '/';
const SPEED = 24; // 이동 속도(%/초) — 방 가로지르기 ≈ 3.5초

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
// 완료 스파클 — 이모지 대신 SVG(currentColor 로 글리프 색 상속).
function SparkleGlyph() {
  return (
    <svg width="15" height="15" viewBox="0 0 16 16" style={{ display: 'block' }}>
      <path d="M8 1.5 9.3 6 14 7.3 9.3 8.6 8 13.5 6.7 8.6 2 7.3 6.7 6 8 1.5Z" fill="currentColor" />
      <circle cx="13" cy="2.5" r="1" fill="currentColor" />
      <circle cx="2.5" cy="12.5" r="0.9" fill="currentColor" />
    </svg>
  );
}
// munder 식 상태 글리프 — 머리 위 한눈 표시.
const GLYPH: Record<string, { t: React.ReactNode; c: string; pulse?: boolean }> = {
  blocked: { t: '!', c: 'var(--cth-coral)', pulse: true },
  waiting: { t: '?', c: 'var(--cth-sky)', pulse: true },
  success: { t: <SparkleGlyph />, c: 'var(--cth-lemon)' },
};

type Pt = { x: number; y: number };

// 카페 잡담 디렉터(munder) — idle 학생 2명+ 모이면 주기적으로 둘이 짝지어 2.6초
// 간격으로 대사를 주고받는다. 반환 = { agentId: 지금 띄울 대사 }.
function useCafeChat(agents: Agent[]): Record<string, string> {
  const [bubbles, setBubbles] = useState<Record<string, string>>({});
  const idle = agents.filter((a) => a.status === 'idle' && !a.isGod).map((a) => a.id);
  const key = idle.join(',');
  const idleRef = useRef(idle);
  idleRef.current = idle;
  useEffect(() => {
    if (idleRef.current.length < 2) { setBubbles({}); return; }
    let alive = true;
    let round = 0;
    const timers: number[] = [];
    const runExchange = () => {
      if (!alive) return;
      const ids = idleRef.current;
      if (ids.length < 2) { setBubbles({}); timers.push(window.setTimeout(runExchange, 4000)); return; }
      const shuffled = [...ids].sort(() => Math.random() - 0.5);
      const a = shuffled[0], b = shuffled[1];
      const lines = pickExchange(round++ + Math.floor(Math.random() * 7));
      let i = 0;
      const beat = () => {
        if (!alive) return;
        if (i >= lines.length) {
          setBubbles({});
          timers.push(window.setTimeout(runExchange, 4500 + Math.random() * 4000));
          return;
        }
        const ln = lines[i++];
        setBubbles({ [ln.who === 'a' ? a : b]: ln.text });
        timers.push(window.setTimeout(beat, 2600));
      };
      beat();
    };
    timers.push(window.setTimeout(runExchange, 2200));
    return () => { alive = false; timers.forEach((t) => window.clearTimeout(t)); };
  }, [key]);
  return bubbles;
}

// 교실 캐릭터 — working/waiting/blocked = 자기 책상으로 BFS 경로 이동해 앉음, idle =
// 카페 구역을 어슬렁(가구 피해 다님). 이동은 waypoint 단위 CSS transition + 워크
// 애니메이션 + 진행방향 flip. 도착하면 멈춤. 머리 위 말풍선/글리프. 클릭 → 대화.
function ClassroomCharacter(
  { agent, seat, grid, cafe, chatLine, onSelect }:
  { agent: Agent; seat?: { x: number; y: number; facing: string }; grid: boolean[][]; cafe: CafeSpot[]; chatLine?: string; onSelect?: (id: string, title: string) => void },
) {
  const atDesk = agent.status !== 'idle';
  const home = seat ? { x: seat.x, y: seat.y } : { x: 50, y: 75 };
  const [pos, setPos] = useState<Pt>(home);
  const [segMs, setSegMs] = useState(0);
  const [moving, setMoving] = useState(false);
  const [flip, setFlip] = useState(false);
  const posRef = useRef(pos);
  posRef.current = pos;
  const pathRef = useRef<Pt[]>([]);
  const timer = useRef<number | undefined>(undefined);

  const step = useCallback(() => {
    const path = pathRef.current;
    if (!path.length) { setMoving(false); return; }
    const next = path.shift()!;
    const cur = posRef.current;
    const dist = Math.hypot(next.x - cur.x, next.y - cur.y);
    if (dist < 0.4) { posRef.current = next; setPos(next); step(); return; }
    if (next.x < cur.x - 0.3) setFlip(true);
    else if (next.x > cur.x + 0.3) setFlip(false);
    const ms = Math.max(140, (dist / SPEED) * 1000);
    setSegMs(ms);
    setMoving(true);
    posRef.current = next;
    setPos(next);
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(step, ms);
  }, []);

  const walkTo = useCallback((target: Pt) => {
    pathRef.current = findPath(grid, posRef.current, target);
    step();
  }, [grid, step]);

  useEffect(() => {
    if (atDesk && seat) { walkTo({ x: seat.x, y: seat.y }); return; }
    // idle → 카페 구역 머무름 지점 근처를 어슬렁(가구 피해 BFS).
    const wander = () => {
      const spot = cafe.length ? cafe[Math.floor(Math.random() * cafe.length)] : null;
      const t: Pt = spot
        ? { x: spot.x + (Math.random() * 6 - 3), y: spot.y + (Math.random() * 4 - 2) }
        : { x: 20 + Math.random() * 24, y: 78 + Math.random() * 10 };
      walkTo(t);
    };
    wander();
    const iv = window.setInterval(wander, 5200);
    return () => { window.clearInterval(iv); window.clearTimeout(timer.current); };
  }, [atDesk, seat?.x, seat?.y, walkTo, cafe]);

  const chatting = chatLine && agent.status === 'idle';
  const thought = chatting ? chatLine : thoughtFor(agent);
  const glyph = GLYPH[agent.status];

  return (
    <button
      onClick={() => onSelect?.(agent.id, agent.character)}
      title={`${agent.character} — 클릭하면 대화`}
      style={{
        position: 'absolute', left: `${pos.x}%`, top: `${pos.y}%`,
        transform: 'translate(-50%, -100%)',
        transition: `left ${segMs}ms linear, top ${segMs}ms linear`,
        display: 'flex', flexDirection: 'column', alignItems: 'center', gap: 3,
        border: 'none', background: 'transparent', cursor: 'pointer', padding: 0,
        width: 132, zIndex: Math.round(pos.y * 10),
      }}
    >
      {glyph && (
        <div style={{
          fontSize: 14, fontWeight: 900, color: glyph.c, lineHeight: 1, marginBottom: 1,
          animation: glyph.pulse ? 'schale-glyph-pulse 1s ease-in-out infinite' : undefined,
          textShadow: '0 1px 2px rgba(255,255,255,0.9)',
        }}>{glyph.t}</div>
      )}

      {thought && (
        <div style={{
          maxWidth: 150, padding: '6px 10px', borderRadius: 12,
          background: '#fff', border: '1px solid var(--cth-cream-200)',
          boxShadow: '0 2px 8px rgba(21, 41, 74, 0.14)',
          fontFamily: 'var(--cth-font-ui)', fontSize: 11, fontWeight: 500, lineHeight: 1.4,
          color: 'var(--cth-ink-900)', textAlign: 'center',
          whiteSpace: 'normal', wordBreak: 'break-word',
        }}>
          {agent.status === 'working' ? (
            <span style={{ display: 'inline-flex', alignItems: 'center', gap: 4, flexWrap: 'wrap', justifyContent: 'center' }}>
              {isBuildCmd(agent.action) ? (
                <b style={{ color: BUILD_COLOR, display: 'inline-flex', alignItems: 'center', gap: 3 }}><GearIcon />빌드 중</b>
              ) : agent.currentTool ? (
                <b style={{ color: 'var(--cth-sky)' }}>{agent.currentTool}</b>
              ) : <span>{thought}</span>}
              {agent.subagents?.length ? <span style={{ color: 'var(--cth-lilac)' }}>· 서브 {agent.subagents.length}</span> : null}
              {agent.background?.length ? <span style={{ color: BUILD_COLOR, display: 'inline-flex', alignItems: 'center', gap: 2 }}><SpinIcon size={10} />{agent.background.length}</span> : null}
            </span>
          ) : thought}
        </div>
      )}

      <div style={{ display: 'flex', alignItems: 'flex-end', justifyContent: 'center' }}>
        <SpriteWalk character={agent.character} walking={moving} flip={flip} width={72} height={100} />
      </div>

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

// 가구 스프라이트 — 개별 img 를 좌표 배치. y-zindex 라 캐릭터와 한 레이어에서 앞뒤가
// 가려진다(발밑 y 큰 게 앞). 스프라이트 없으면(아직 미생성) 조용히 숨김.
function FurnitureSprite({ f }: { f: Furniture }) {
  const [ok, setOk] = useState(true);
  if (!ok) return null;
  return (
    <img
      src={`${ROOT}assets/${f.sprite}`}
      alt=""
      onError={() => setOk(false)}
      draggable={false}
      style={{
        position: 'absolute', left: `${f.x}%`, top: `${f.y}%`,
        width: `${f.w}%`, transform: 'translate(-50%, -100%)',
        zIndex: Math.round(f.y * 10), pointerEvents: 'none', userSelect: 'none',
        imageRendering: 'pixelated',
      }}
    />
  );
}

export interface ClassroomViewProps {
  onSelect?: (surfaceId: string, title: string) => void;
  /** 활성 워크스페이스 학생만(장소이동). 없으면 store 전체. */
  agents?: Agent[];
  /** 바닥 배경 파일명(워크스페이스별). 없으면 빈 교실 바닥. */
  background?: string;
  /** 배치 가구. 기본 교실 세트. 가구 그림이 박힌 배경(카페/오피스)이면 [] 로 끔. */
  furniture?: Furniture[];
}

// 샬레 교실 — 빈 바닥 배경 위에 가구를 개별 배치(munder 식). 학생은 가구를 피해
// 책상(working)이나 카페(idle)로 BFS 경로 이동. 가구·학생이 한 z-레이어라 앞뒤가림.
export function ClassroomView({ onSelect, agents: agentsProp, background, furniture = CLASSROOM_FURNITURE }: ClassroomViewProps) {
  const storeAgents = useStore((s) => s.agents);
  const agents = agentsProp ?? storeAgents;
  const sorted = [...agents].sort((a, b) => Number(b.isGod) - Number(a.isGod));

  const grid = useMemo(() => buildGrid(furniture), [furniture]);
  const seats = useMemo(() => deskSeats(furniture), [furniture]);
  const cafe = useMemo(() => cafeSpots(furniture), [furniture]);
  const chat = useCafeChat(agents);

  return (
    <div style={{
      position: 'relative', width: '100%', maxWidth: 960, margin: '0 auto',
      aspectRatio: '3 / 2',
      borderRadius: 16, overflow: 'hidden',
      backgroundImage: `url(${ROOT}assets/${background ?? 'classroom-floor.png'})`,
      backgroundSize: 'cover', backgroundPosition: 'center',
      imageRendering: 'pixelated',
      transition: 'background-image 0.3s ease',
      boxShadow: '0 6px 20px rgba(21, 41, 74, 0.12), inset 0 0 0 1px var(--cth-cream-200)',
    }}>
      {furniture.map((f) => <FurnitureSprite key={f.id} f={f} />)}

      {sorted.slice(0, Math.max(seats.length, 6)).map((a, i) => (
        <ClassroomCharacter key={a.id} agent={a} seat={seats[i] ?? undefined} grid={grid} cafe={cafe} chatLine={chat[a.id]} onSelect={onSelect} />
      ))}

      {sorted.length === 0 && (
        <div style={{ position: 'absolute', inset: 0, display: 'flex', alignItems: 'center', justifyContent: 'center', color: 'var(--cth-ink-500)', fontFamily: 'var(--cth-font-ui)', fontSize: 14, background: 'rgba(255,255,255,0.4)' }}>
          학생들을 기다리는 중…
        </div>
      )}
    </div>
  );
}
