import { useEffect, useState } from 'react';
import { characterPool, fetchCharacters, fetchRecentSessions, type Harness, type RecentSession, type SessionsInfo } from '@/lib/mcp';
import type { Agent } from '@/store';
import { SpritePortrait } from './SpritePortrait';

// 학생 상태 → 작은 점 색(좌측 트리). 3구분(거노: 작업중·노는중·선택지대기 구분 강화):
// 작업중(working/thinking)=하늘, 선택지대기(waiting/blocked)=빨강, 노는중·완료=초록.
function statusDot(status: Agent['status']): string {
  switch (status) {
    case 'working':
    case 'thinking': return 'var(--cth-sky)';
    case 'waiting':
    case 'blocked': return 'var(--cth-coral)';
    default: return 'var(--cth-status-success)';
  }
}
// 색만으론 약해(거노) 움직임을 더함: 작업중=부드러운 호흡 펄스, 대기=깜빡(주의), 그 외=정적.
function statusAnim(status: Agent['status']): string | undefined {
  if (status === 'working' || status === 'thinking') return 'cth-dot-pulse 1.3s ease-in-out infinite';
  if (status === 'waiting' || status === 'blocked') return 'cth-blink 0.9s ease-in-out infinite';
  return undefined;
}

// 방 추가 시 고를 첫 학생(거노: 처음은 아로나 고정, 새 방은 선택). leaders 풀과 일치.
const STARTERS = ['아로나', '프라나'];

/** unix secs → "방금/N분 전/N시간 전/N일 전". */
// 작업폴더 경로 축약 — 홈은 ~, 깊으면 마지막 2 세그먼트만(거노: 최근 세션을 폴더로 구분).
function shortPath(p?: string): string {
  if (!p) return '';
  const h = p.replace(/^\/(?:Users|home)\/[^/]+/, '~');
  const segs = h.split('/').filter(Boolean);
  return segs.length > 3 ? `…/${segs.slice(-2).join('/')}` : h;
}
// 하네스 배지 — 목록에 세 프로그램의 세션이 섞이면 제목만으론 무엇으로 여는지 알 수
// 없다. 이어가는 명령이 셋 다 달라서, 고르기 전에 눈으로 갈라 보여야 한다.
const HARNESS_STYLE: Record<Harness, { label: string; fg: string; bg: string }> = {
  claude: { label: 'claude', fg: 'var(--cth-sky)', bg: 'var(--cth-sky-light)' },
  codex: { label: 'codex', fg: 'var(--cth-ink-700)', bg: 'var(--cth-cream-200)' },
  agy: { label: 'agy', fg: 'var(--cth-coral)', bg: 'var(--cth-cream-200)' },
};
function HarnessBadge({ harness }: { harness?: Harness }) {
  // 옛 기록엔 이 칸이 없다 — 그 시절은 전부 claude 였다.
  const s = HARNESS_STYLE[harness ?? 'claude'] ?? HARNESS_STYLE.claude;
  return (
    <span style={{
      flexShrink: 0, padding: '0 4px', borderRadius: 4, background: s.bg, color: s.fg,
      fontSize: 8, fontWeight: 800, letterSpacing: 0.2, lineHeight: '13px',
    }}>{s.label}</span>
  );
}
function relativeTime(secs: number): string {
  const diff = Math.max(0, Date.now() / 1000 - secs);
  if (diff < 60) return '방금';
  if (diff < 3600) return `${Math.floor(diff / 60)}분 전`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}시간 전`;
  return `${Math.floor(diff / 86400)}일 전`;
}

export interface RoomMapProps {
  sessions: SessionsInfo;
  onSwitch: (idx: number) => void;
  /** 전 방 학생(board) — windowIdx 로 방별 그룹핑해 각 방 아래 중첩(거노: 방안 학생 영속). */
  agents: Agent[];
  /** 현재 선택된 학생 — 트리 하이라이트. */
  selectedId?: string;
  /** 학생 클릭 — 그 pane 포커스(다른 방이면 윈도우 전환). */
  onSelectStudent?: (id: string, name: string) => void;
  /** 새 방 + 선택 학생 스폰. */
  onNewRoom?: (character: string) => void;
  /** 방(윈도우) 닫기. 윈도우 2개+ 일 때만. */
  onCloseRoom?: (idx: number) => void;
  /** 최근 세션 클릭 — 바로 resume 하지 않고 오프라인(읽기 전용) 뷰어로 띄운다. */
  onOpenSession?: (s: RecentSession) => void;
  /** 좌측 존 접기 — 부모(App)가 leftHidden 으로 레일 전환(거노: 가장자리 접기 일원화). */
  onCollapse?: () => void;
}

function RoomIcon({ active }: { active: boolean }) {
  const c = active ? '#fff' : 'var(--cth-sky)';
  return (
    <svg width="16" height="16" viewBox="0 0 18 18" style={{ flexShrink: 0 }}>
      <path d="M2 8 9 3l7 5v6a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1V8Z" fill="none" stroke={c} strokeWidth="1.4" strokeLinejoin="round" />
      <rect x="6.5" y="10" width="5" height="4" fill="none" stroke={c} strokeWidth="1.2" />
    </svg>
  );
}

// 좌측 방 네비 — 방 = kasaterm 윈도우(거노). 목록 + "+ 방 추가"(첫 학생 선택). 첫 방은
// 아로나 고정, 새 방은 아로나/프라나 중 골라 그 학생으로 스폰. × 로 방 닫기.
export function RoomMap({ sessions, onSwitch, agents, selectedId, onSelectStudent, onNewRoom, onCloseRoom, onOpenSession, onCollapse }: RoomMapProps) {
  const n = sessions.count;
  // 방별 학생 — pane id 순 안정 정렬.
  const byRoom = new Map<number, Agent[]>();
  for (const a of agents) {
    const w = a.windowIdx ?? 0;
    if (!byRoom.has(w)) byRoom.set(w, []);
    byRoom.get(w)!.push(a);
  }
  for (const list of byRoom.values()) {
    list.sort((x, y) => x.id.localeCompare(y.id));
  }
  const [adding, setAdding] = useState(false);
  const [showRecent, setShowRecent] = useState(false);
  const [recent, setRecent] = useState<RecentSession[]>([]);
  // 기본은 이 방 폴더 것만. 켜면 폴더를 넘어 전체를 훑는다 — 어제 다른 레포에서
  // 하던 대화로 돌아갈 때 폴더를 먼저 옮길 필요가 없어진다.
  const [scopeAll, setScopeAll] = useState(false);
  // 학생 이름 → header_color(학생색). 최근 세션 행의 색 바에 쓴다 — 어느 학생의
  // 세션인지 이름 읽기 전에 색으로 먼저 구분(거노). 펼칠 때 1회 로드.
  const [charColor, setCharColor] = useState<Record<string, string>>({});

  // 최근 세션 패널을 펼칠 때(또는 펼친 채로 10초마다) 목록을 가져온다 — 항상
  // 폴링하면 닫혀있을 때도 낭비라, 펼침 상태에서만 새로고침.
  useEffect(() => {
    if (!showRecent) return;
    let alive = true;
    const load = () => {
      void fetchRecentSessions(undefined, scopeAll ? 'all' : 'cwd').then((s) => { if (alive) setRecent(s); });
    };
    load();
    void fetchCharacters().then((c) => {
      if (!alive || !c) return;
      const map: Record<string, string> = {};
      for (const m of characterPool(c)) {
        if (m.header_color) map[m.name] = m.header_color;
      }
      setCharColor(map);
    });
    const iv = setInterval(load, 10000);
    return () => { alive = false; clearInterval(iv); };
  }, [showRecent, scopeAll]);

  const onPick = (s: RecentSession) => {
    onOpenSession?.(s);
    setShowRecent(false);
  };

  if (n < 1) return null;
  return (
    <div style={{
      width: 184, flexShrink: 0, height: '100%', overflowY: 'auto',
      borderRight: '1px solid var(--cth-cream-200)', background: 'var(--cth-cream-50)',
      padding: '10px 8px', display: 'flex', flexDirection: 'column', gap: 4,
    }}>
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', padding: '0 2px 4px' }}>
        <span style={{ fontFamily: 'var(--cth-font-display)', fontSize: 10, color: 'var(--cth-ink-500)' }}>방 (터미널 윈도우)</span>
        <button onClick={() => onCollapse?.()} title="좌측 패널 접기" style={{
          width: 18, height: 18, borderRadius: 5, border: 'none', cursor: 'pointer', background: 'transparent', color: 'var(--cth-ink-300)',
          display: 'flex', alignItems: 'center', justifyContent: 'center', flexShrink: 0,
        }}>
          <svg width="12" height="12" viewBox="0 0 16 16"><path d="M10 3l-5 5 5 5" stroke="currentColor" strokeWidth="2" fill="none" strokeLinecap="round" strokeLinejoin="round" /></svg>
        </button>
      </div>
      {Array.from({ length: n }, (_, i) => {
        const on = i === sessions.active;
        const students = byRoom.get(i) ?? [];
        return (
          <div key={i} style={{ display: 'flex', flexDirection: 'column', gap: 2 }}>
            <div style={{
              display: 'flex', alignItems: 'center', gap: 4, borderRadius: 8,
              background: on ? 'var(--cth-sky)' : 'transparent', color: on ? '#fff' : 'var(--cth-ink-700)',
            }}>
              <button onClick={() => { if (!on) onSwitch(i); }} style={{
                flex: 1, display: 'flex', alignItems: 'center', gap: 7, padding: '7px 9px', borderRadius: 8,
                border: 'none', cursor: on ? 'default' : 'pointer', textAlign: 'left', background: 'transparent', color: 'inherit',
                fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: 600,
              }}>
                <RoomIcon active={on} />
                <span style={{ flex: 1, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{sessions.labels[i] || `방 ${i + 1}`}</span>
                {on && <span style={{ fontSize: 9, fontWeight: 800 }}>●</span>}
              </button>
              {n > 1 && onCloseRoom && (
                <button onClick={(e) => { e.stopPropagation(); onCloseRoom(i); }} title="방 닫기" style={{
                  flexShrink: 0, width: 18, height: 18, marginRight: 5, borderRadius: 5, border: 'none', cursor: 'pointer',
                  background: on ? 'rgba(255,255,255,0.25)' : 'var(--cth-cream-100)', color: on ? '#fff' : 'var(--cth-ink-500)',
                  fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: 800, lineHeight: 1, display: 'flex', alignItems: 'center', justifyContent: 'center',
                }}>×</button>
              )}
            </div>
            {/* 방 안 학생 — windowIdx 로 그룹핑. 활성 방뿐 아니라 전 방 학생이 영속(거노). 클릭=
                그 pane 포커스(다른 방이면 윈도우 전환). 프사+이름+상태점. */}
            {students.map((a) => {
              const sel = a.id === selectedId;
              const pure = a.character && !/^%?\d+$/.test(a.character) ? a.character : a.name;
              return (
                <button key={a.id} onClick={() => onSelectStudent?.(a.id, a.name)} title={a.name} style={{
                  display: 'flex', alignItems: 'center', gap: 6, margin: '0 0 0 12px', padding: '4px 7px', borderRadius: 7,
                  border: 'none', cursor: 'pointer', textAlign: 'left',
                  background: sel ? 'var(--cth-sky-light)' : 'transparent', color: 'var(--cth-ink-700)',
                  fontFamily: 'var(--cth-font-ui)', fontSize: 11, fontWeight: sel ? 700 : 500,
                }}>
                  <span style={{ width: 20, height: 20, borderRadius: 6, overflow: 'hidden', flexShrink: 0, background: 'var(--cth-cream-100)', display: 'flex', alignItems: 'flex-end', justifyContent: 'center' }}>
                    <SpritePortrait character={pure} scale={0.9} bust />
                  </span>
                  <span style={{ flex: 1, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', fontWeight: sel ? 700 : 500 }}>{pure}</span>
                  <span style={{ flexShrink: 0, width: 6, height: 6, borderRadius: 999, background: statusDot(a.status), animation: statusAnim(a.status) }} />
                </button>
              );
            })}
          </div>
        );
      })}

      {/* 방 추가 — 누르면 첫 학생(아로나/프라나) 선택 펼침 */}
      {onNewRoom && (
        adding ? (
          <div style={{ marginTop: 4, padding: 7, borderRadius: 8, background: 'var(--cth-cream-100)', display: 'flex', flexDirection: 'column', gap: 4 }}>
            <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 10, fontWeight: 700, color: 'var(--cth-ink-500)', padding: '0 2px 2px' }}>첫 학생 선택</div>
            {STARTERS.map((g) => (
              <button key={g} onClick={() => { onNewRoom(g); setAdding(false); }} style={{
                display: 'flex', alignItems: 'center', gap: 6, padding: '7px 9px', borderRadius: 7, border: 'none', cursor: 'pointer',
                background: '#fff', color: 'var(--cth-ink-900)', fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: 700,
                boxShadow: '0 1px 3px rgba(21,41,74,0.1)',
              }}>
                <img src={`${import.meta.env.BASE_URL || '/'}assets/idle-front-${g === '아로나' ? 'arona' : 'prana'}.png`} alt="" style={{ width: 20, height: 20, objectFit: 'contain', imageRendering: 'pixelated' }} />
                {g}
              </button>
            ))}
            <button onClick={() => setAdding(false)} style={{ padding: '5px', borderRadius: 7, border: 'none', cursor: 'pointer', background: 'transparent', color: 'var(--cth-ink-300)', fontFamily: 'var(--cth-font-ui)', fontSize: 11 }}>취소</button>
          </div>
        ) : (
          <button onClick={() => setAdding(true)} style={{
            marginTop: 4, display: 'flex', alignItems: 'center', justifyContent: 'center', gap: 6, padding: '8px', borderRadius: 8,
            border: '1.5px dashed var(--cth-cream-200)', cursor: 'pointer', background: 'transparent', color: 'var(--cth-sky)',
            fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: 700,
          }}>
            <svg width="14" height="14" viewBox="0 0 16 16"><path d="M8 3v10M3 8h10" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" /></svg>
            방 추가
          </button>
        )
      )}

      {/* 최근 세션 보기 — 펼치면 ~/.claude/projects 의 최근 claude 세션 목록. 클릭하면
          바로 열지 않고 그 대화를 오프라인(읽기 전용) 뷰어로 띄운다. 이어가기는 뷰어
          하단 '현재 터미널에 입력' 버튼(거노: 먼저 보고 직접 이어가기). */}
      <button onClick={() => setShowRecent((v) => !v)} style={{
        marginTop: 6, display: 'flex', alignItems: 'center', gap: 6, padding: '7px 9px', borderRadius: 8,
        border: 'none', cursor: 'pointer', background: 'transparent', color: 'var(--cth-ink-500)',
        fontFamily: 'var(--cth-font-ui)', fontSize: 11, fontWeight: 700,
      }}>
        <svg width="13" height="13" viewBox="0 0 16 16" style={{ transform: showRecent ? 'rotate(90deg)' : 'none', transition: 'transform .12s' }}>
          <path d="M6 3l5 5-5 5" stroke="currentColor" strokeWidth="2" fill="none" strokeLinecap="round" strokeLinejoin="round" />
        </svg>
        최근 세션 보기
      </button>
      {showRecent && (
        <div style={{ display: 'flex', flexDirection: 'column', gap: 3, padding: '2px 2px 4px' }}>
          {/* 범위 토글 — 이 방 폴더만 / 전체. 폴더를 넘어 찾을 때가 잦아 목록 바로 위에 둔다. */}
          <div style={{ display: 'flex', gap: 2, padding: '0 2px 2px' }}>
            {([['cwd', '이 폴더'], ['all', '전체']] as const).map(([k, txt]) => {
              const on = (k === 'all') === scopeAll;
              return (
                <button key={k} onClick={() => setScopeAll(k === 'all')} style={{
                  flex: 1, padding: '3px 0', borderRadius: 5, border: 'none', cursor: 'pointer',
                  background: on ? 'var(--cth-sky)' : 'var(--cth-cream-100)',
                  color: on ? '#fff' : 'var(--cth-ink-500)',
                  fontFamily: 'var(--cth-font-ui)', fontSize: 9, fontWeight: 700,
                }}>{txt}</button>
              );
            })}
          </div>
          {recent.length === 0 ? (
            <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 10, color: 'var(--cth-ink-300)', padding: '4px 6px' }}>최근 세션 없음</div>
          ) : (
            recent.map((s) => (
              // 학생색 좌측 바 + 세션 이름 뒤 프사 — 어느 학생의 세션인지 즉시 구분
              // (거노). 미바인딩 세션은 색 바 없이(투명) 이름만.
              <button key={s.id} onClick={() => onPick(s)} title={`${s.label}${s.character ? `\n${s.character}` : ''}\n${s.harness ?? 'claude'} · ${s.cwd}`} style={{
                display: 'flex', flexDirection: 'column', gap: 2, padding: '6px 8px', borderRadius: 7, border: 'none',
                borderLeft: `3px solid ${(s.character && charColor[s.character]) || 'transparent'}`,
                cursor: 'pointer', textAlign: 'left',
                background: 'var(--cth-cream-100)', color: 'var(--cth-ink-700)',
              }}>
                <span style={{ display: 'flex', alignItems: 'center', gap: 5, minWidth: 0 }}>
                  <span style={{ flex: 1, fontFamily: 'var(--cth-font-ui)', fontSize: 11, fontWeight: 600, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                    {s.label}
                  </span>
                  {s.character && (
                    <span style={{ width: 18, height: 18, borderRadius: 5, overflow: 'hidden', flexShrink: 0, background: 'var(--cth-cream-50)', display: 'flex', alignItems: 'flex-end', justifyContent: 'center' }}>
                      <SpritePortrait character={s.character} scale={0.9} bust />
                    </span>
                  )}
                </span>
                {/* 마지막 대화 한 줄. 서버가 아직 안 보내므로 대개 없다 — 생기면 여기 붙는다. */}
                {s.preview && (
                  <span style={{
                    fontFamily: 'var(--cth-font-ui)', fontSize: 9, color: 'var(--cth-ink-500)',
                    overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap',
                  }}>{s.preview}</span>
                )}
                {/* 메타 한 줄: [하네스] · 시각 · 프로젝트. 셋을 따로 쌓으면 행이 길어져
                    한 화면에 담기는 세션 수가 줄어든다. 폭이 모자라면 경로부터 줄인다. */}
                <span style={{
                  display: 'flex', alignItems: 'center', gap: 4, minWidth: 0,
                  fontFamily: 'var(--cth-font-ui)', fontSize: 9, color: 'var(--cth-ink-300)',
                }}>
                  <HarnessBadge harness={s.harness} />
                  <span style={{ flexShrink: 0 }}>{relativeTime(s.mtime)}</span>
                  {s.cwd && (
                    <span style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                      · {shortPath(s.cwd)}
                    </span>
                  )}
                </span>
              </button>
            ))
          )}
        </div>
      )}
    </div>
  );
}
