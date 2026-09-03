import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useStore, isUnconfirmed, type Agent } from '@/store';
import { SpriteWalk, type Facing } from './SpriteWalk';
import {
  CLASSROOM_FURNITURE, buildGrid, deskSeats, cafeSpots, findPath,
  type Furniture, type CafeSpot,
} from './classroomSpace';
import { pickExchange } from './cafeteriaLines';
import { isBuildCmd, BUILD_COLOR, GearIcon, SpinIcon } from './activity';

const ROOT = import.meta.env.BASE_URL || '/';
const SPEED = 24; // 이동 속도(%/초) — 방 가로지르기 ≈ 3.5초

function shortenAction(s?: string): string {
  if (!s) return '';
  return s.replace(/\/\S*\/([^\s/]+)/g, '…/$1');
}
function shortCwd(p?: string): string {
  if (!p) return '';
  const segs = p.split('/').filter(Boolean);
  return (segs.length > 2 ? '…/' : '') + segs.slice(-2).join('/');
}
// 머리 위 말풍선 = 상태 + 쓰는 툴만(거노). 답변/질문 본문(lastReply)은 안 띄움 —
// 그건 학생을 클릭해 '학생별 대화'에서 본다. waiting/blocked 는 짧은 상태어만.
function thoughtFor(a: Agent): string {
  switch (a.status) {
    case 'working': {
      const tool = a.currentTool || shortenAction(a.action) || '작업 중';
      const n = a.subagents?.length ?? 0;
      return n > 0 ? `${tool} · 서브 ${n}` : tool;
    }
    case 'thinking': return '생각 중';
    case 'waiting': return '입력 대기';
    case 'blocked': return '확인 필요';
    // idle = 답변(lastReply) 안 띄움(거노: 답변말고 상태만). 말풍선은 비고, 한가하면
    // 카페 잡담(chatLine)만 뜬다. 상태는 이름표 점 색으로 표시.
    case 'idle': return '';
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
  thinking: { t: '…', c: 'var(--cth-status-thinking)', pulse: true },
  success: { t: <SparkleGlyph />, c: 'var(--cth-lemon)' },
};

type Pt = { x: number; y: number };

// 카페 잡담 디렉터(munder) — idle 학생 2명+ 모이면 주기적으로 둘이 짝지어 2.6초
// 간격으로 대사를 주고받는다. 반환 = { agentId: 지금 띄울 대사 }.
function useCafeChat(agents: Agent[]): Record<string, string> {
  const [bubbles, setBubbles] = useState<Record<string, string>>({});
  const idle = agents.filter((a) => a.status === 'idle').map((a) => a.id);
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
  { agent, seat, grid, cafe, chatLine, onSelect, index, selected, unconfirmed }:
  { agent: Agent; seat?: { x: number; y: number; facing: string }; grid: boolean[][]; cafe: CafeSpot[]; chatLine?: string; onSelect?: (id: string, title: string) => void; index: number; selected?: boolean; unconfirmed?: boolean },
) {
  // working/waiting/blocked = 자기 책상으로. idle 은 카페 배회 — god 개념 폐기
  // (거노 2026-07-13)로 전원 동일 규칙.
  const atDesk = agent.status !== 'idle';
  const home = seat ? { x: seat.x, y: seat.y } : { x: 50, y: 75 };
  const [pos, setPos] = useState<Pt>(home);
  const [segMs, setSegMs] = useState(0);
  const [moving, setMoving] = useState(false);
  const [flip, setFlip] = useState(false);
  const [facing, setFacing] = useState<Facing>('front'); // 이동 방향 4방향 스프라이트
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
    // 이동 방향으로 4방향 결정 — 세로가 크면 위=back/아래=front, 가로가 크면 side(+flip).
    const dx = next.x - cur.x, dy = next.y - cur.y;
    if (Math.abs(dy) > Math.abs(dx)) setFacing(dy < 0 ? 'back' : 'front');
    else { setFacing('side'); if (dx < -0.3) setFlip(true); else if (dx > 0.3) setFlip(false); }
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
    // 자율 이동(거노: 방향키 수동조종 폐기) — 일하면 자기 책상, 쉬면 소파/카페 거점으로
    // 스스로 걸어가 정착. 상태(atDesk)가 바뀌면 책상↔소파 자동 전환. 도착하면 멈춤
    // (계속 배회하던 옛 wander 는 산만해서 폐기 — 거점은 인덱스로 결정적 분배).
    if (atDesk && seat) { walkTo({ x: seat.x, y: seat.y }); return; }
    const home = cafe.length ? cafe[index % cafe.length] : null;
    const tier = cafe.length ? Math.floor(index / cafe.length) : 0;
    const t: Pt = home
      ? { x: home.x + (tier ? (tier % 2 ? 7 : -7) : 0), y: home.y }
      : { x: 22 + (index % 5) * 13, y: 82 };
    walkTo(t);
    return () => { window.clearTimeout(timer.current); };
  }, [atDesk, seat?.x, seat?.y, walkTo, cafe, index]);

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
        width: 132, zIndex: selected ? 9999 : Math.round(pos.y * 10),
        filter: selected ? 'drop-shadow(0 0 3px var(--cth-sky)) drop-shadow(0 0 7px var(--cth-sky))' : undefined,
      }}
    >
      {glyph && (
        <div style={{
          fontSize: 14, fontWeight: 900, color: glyph.c, lineHeight: 1, marginBottom: 1,
          animation: glyph.pulse ? 'schale-glyph-pulse 1s ease-in-out infinite' : undefined,
          textShadow: '0 1px 2px rgba(255,255,255,0.9)',
        }}>{glyph.t}</div>
      )}

      {(thought || unconfirmed) && (
        <div style={{
          maxWidth: 150, padding: '6px 10px', borderRadius: 12,
          // 미확인(선생님 확인 대기) = 코랄 강조 말풍선(거노). 그 외엔 흰 말풍선.
          background: unconfirmed ? 'var(--cth-coral)' : '#fff',
          border: unconfirmed ? '1px solid var(--cth-coral)' : '1px solid var(--cth-cream-200)',
          boxShadow: unconfirmed
            ? '0 3px 12px color-mix(in srgb, var(--cth-coral) 55%, transparent)'
            : '0 2px 8px rgba(21, 41, 74, 0.14)',
          fontFamily: 'var(--cth-font-ui)', fontSize: 11, fontWeight: unconfirmed ? 800 : 500, lineHeight: 1.4,
          // 교실 바닥 위 고정 흰 말풍선 — 다크에서도 흰색이라 텍스트는 고정 어두운색으로.
          color: unconfirmed ? '#fff' : '#15294A', textAlign: 'center',
          whiteSpace: 'normal', wordBreak: 'break-word',
          animation: unconfirmed ? 'schale-glyph-pulse 1.1s ease-in-out infinite' : undefined,
        }}>
          {unconfirmed ? '확인 필요!' : agent.status === 'working' ? (
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

      {/* 등장 모션 — 처음 교실에 나타날 때 아래서 통통 튀어오르며 페이드인(munder식, 거노).
          mount 1회 재생. button 위치 transform 과 분리된 내부 레이어라 이동 transition 무간섭. */}
      <div style={{ display: 'flex', alignItems: 'flex-end', justifyContent: 'center', animation: 'schale-enter 0.5s ease-out both' }}>
        <SpriteWalk character={agent.spriteChar ?? agent.character} walking={moving} flip={flip} facing={facing} width={72} height={100} />
      </div>

      <div style={{
        display: 'flex', alignItems: 'center', gap: 5,
        background: 'rgba(255,255,255,0.9)', borderRadius: 9, padding: '2px 9px',
        boxShadow: '0 1px 4px rgba(21, 41, 74, 0.12)',
        // 흰 이름표 — 다크에서도 흰 바닥 위라 텍스트는 고정 어두운색.
        fontFamily: 'var(--cth-font-ui)', fontSize: 11, fontWeight: 700, color: '#15294A',
      }}>
        <span style={{ width: 7, height: 7, borderRadius: 999, background: STATUS_COLOR[agent.status] ?? 'var(--cth-ink-300)' }} />
        {agent.character}
      </div>

      {agent.cwd && (
        <div style={{
          marginTop: 2, padding: '1px 7px', borderRadius: 7,
          background: 'rgba(255,255,255,0.82)', boxShadow: '0 1px 3px rgba(21,41,74,0.1)',
          // 흰 cwd칩 — 다크에서도 흰 바닥 위라 텍스트는 고정 어두운색.
          fontFamily: 'var(--cth-font-mono)', fontSize: 9, color: '#4A638F',
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
  /** 자리 override — 가구 없이 배경 이미지의 책상에 직접 앉힐 때(SCHALE 교실 이미지). */
  seats?: { x: number; y: number; facing: string }[];
  /** idle 배회 구역 override. */
  cafe?: CafeSpot[];
  /** 빈 자리 클릭 시 — 학생 부르기(멀리 있는 버튼 대신 빈 책상에서 바로 소환). */
  onAdd?: () => void;
  /** 클릭으로 선택된 학생 — 교실에서 글로우 강조 + 방향키로 직접 이동(거노). */
  selectedId?: string | null;
}

// 빈 책상 자리 — 학생 없을 때 그 자리에 '+ 부르기' 버튼. 클릭 → 학생 부르기 모달.
// 밝은 SCHALE 바닥에서도 보이게 솔리드 하늘색 + 가벼운 펄스.
function EmptySeat({ seat, onAdd }: { seat: { x: number; y: number }; onAdd?: () => void }) {
  return (
    <button
      onClick={onAdd}
      title="이 자리에 캐릭터 부르기"
      className="cth-emptyseat"
      style={{
        position: 'absolute', left: `${seat.x}%`, top: `${seat.y}%`,
        transform: 'translate(-50%, -100%)', zIndex: Math.round(seat.y * 10) - 1,
        display: 'flex', flexDirection: 'column', alignItems: 'center', gap: 3,
        border: 'none', background: 'transparent', cursor: 'pointer', padding: 0,
      }}
    >
      <span style={{
        width: 38, height: 38, borderRadius: 999,
        background: 'linear-gradient(180deg, #6BB0F0, #4A90E2)', color: '#fff',
        border: '2px solid #fff', display: 'flex', alignItems: 'center', justifyContent: 'center',
        boxShadow: '0 3px 10px rgba(74,144,226,0.5)',
        animation: 'schale-glyph-pulse 1.6s ease-in-out infinite',
      }}>
        <svg width="18" height="18" viewBox="0 0 16 16"><path d="M8 3v10M3 8h10" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" /></svg>
      </span>
      <span style={{
        fontFamily: 'var(--cth-font-ui)', fontSize: 10, fontWeight: 800, color: '#fff',
        background: 'var(--cth-ink-900)', padding: '1px 8px', borderRadius: 7,
        boxShadow: '0 1px 4px rgba(21,41,74,0.3)',
      }}>부르기</span>
    </button>
  );
}

// 샬레 교실 — 빈 바닥 배경 위에 가구를 개별 배치(munder 식). 학생은 가구를 피해
// 책상(working)이나 카페(idle)로 BFS 경로 이동. 가구·학생이 한 z-레이어라 앞뒤가림.
export function ClassroomView({ onSelect, agents: agentsProp, background, furniture = CLASSROOM_FURNITURE, seats: seatsProp, cafe: cafeProp, onAdd, selectedId }: ClassroomViewProps) {
  const storeAgents = useStore((s) => s.agents);
  const acked = useStore((s) => s.acked);
  const agents = agentsProp ?? storeAgents;
  const sorted = [...agents];

  const grid = useMemo(() => buildGrid(furniture), [furniture]);
  const seats = useMemo(() => seatsProp ?? deskSeats(furniture), [furniture, seatsProp]);
  const cafe = useMemo(() => cafeProp ?? cafeSpots(furniture), [furniture, cafeProp]);
  const chat = useCafeChat(agents);

  return (
    <div style={{
      // 부모(메인 컬럼)를 꽉 채운다 — 옛 maxWidth:960 + aspectRatio 고정이 좌우
      // 여백을 만들던 것 제거(거노: 꽉차게). 캐릭터는 % 좌표라 비율 따라 분포.
      position: 'relative', width: '100%', height: '100%',
      borderRadius: 16, overflow: 'hidden',
      // 가구/캐릭터 zIndex(발밑 y*10, 최대 ~960)를 교실 안에 가둔다 — 안 그러면
      // 문서 레벨에서 모달(z 낮음) 위로 책상이 뚫고 올라온다(거노 학생부르기 버그).
      isolation: 'isolate',
      backgroundImage: `url(${ROOT}assets/${background ?? 'classroom-floor.png'})`,
      backgroundSize: 'cover', backgroundPosition: 'center',
      imageRendering: 'pixelated',
      transition: 'background-image 0.3s ease',
      boxShadow: '0 6px 20px rgba(21, 41, 74, 0.12), inset 0 0 0 1px var(--cth-cream-200)',
    }}>
      {furniture.map((f) => <FurnitureSprite key={f.id} f={f} />)}

      {/* 빈 교실(첫 부팅, 첫 학생 자동 스폰 전) — 화면 어두워지며 로딩 스피너만(거노). */}
      {sorted.length === 0 && (
        <div style={{
          position: 'absolute', inset: 0, zIndex: 9999,
          display: 'flex', flexDirection: 'column', alignItems: 'center', justifyContent: 'center', gap: 18,
          background: 'rgba(12, 19, 38, 0.58)', backdropFilter: 'blur(2px)',
        }}>
          <div style={{
            width: 46, height: 46, borderRadius: '50%',
            border: '4px solid rgba(255,255,255,0.22)', borderTopColor: '#fff',
            animation: 'schale-spin 0.8s linear infinite',
          }} />
          <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 14, fontWeight: 600, color: '#fff', letterSpacing: 0.3 }}>
            아로나 오는 중…
          </div>
        </div>
      )}

      {sorted.slice(0, Math.max(seats.length, 6)).map((a, i) => (
        <ClassroomCharacter key={a.id} agent={a} seat={seats[i] ?? undefined} grid={grid} cafe={cafe} chatLine={chat[a.id]} onSelect={onSelect} index={i} selected={!!selectedId && a.id === selectedId} unconfirmed={isUnconfirmed(a, acked)} />
      ))}

      {/* 빈 자리마다 '부르기' 버튼 — 학생 없는 책상에서 바로 소환(멀리 있는 버튼 대신) */}
      {onAdd && seats.slice(sorted.length).map((seat, i) => seat && (
        <EmptySeat key={`empty-${i}`} seat={seat} onAdd={onAdd} />
      ))}
    </div>
  );
}
