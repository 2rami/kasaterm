import { useEffect, useState } from 'react';
import { useStore, type Agent } from './store';
import { ModePicker } from './components/ModePicker';
import { ClassroomView } from './components/ClassroomView';
import { CommandCenter } from './components/CommandCenter';
import { TitleBar } from './components/TitleBar';
import { RoomMap } from './components/RoomMap';
import { startBoardPolling, fetchMode, focusPane, fetchClaudeUsage, fetchSessions, fetchBackgroundAgents, switchSession, newRoom, closeRoom, fetchLayout, type ClaudeUsage, type SessionsInfo, type RecentSession, type PaneRect, type BackgroundAgent } from './lib/mcp';
import { TerminalPeekPanel } from './components/TerminalPeekPanel';
import { TerminalBlockCard } from './components/TerminalBlockCard';
import { assignSprites } from './lib/sprites';

type ViewMode = 'terminal' | 'classroom';
// 중앙 멀티뷰 한 칸 = 살아있는 학생(surface id) 또는 오프라인 과거 세션(offline=true).
type PeekItem = { id: string; title: string; offline?: boolean; cwd?: string };

// dev 디자인 검증용 목 학생(URL ?mock=1). board 비어도 풀 화면을 본다.
const MOCK_AGENTS: Agent[] = [
  { id: '%1', name: '아로나', character: '아로나', accent: 'sky', status: 'idle', project: 'tmuxify', progress: 2, contextTokens: 30000, tokensIn: 24000, tokensOut: 6000, costUsd: 0.18, contextLimit: 200000, model: 'claude-opus-4-8', cwd: '/Users/kasa/Desktop/momewomo/tmuxify', branch: 'main', isGod: true, lastReply: '선생님, 오늘 의뢰 정리했어요!' },
  { id: '%2', name: '모모이', character: '모모이', accent: 'coral', status: 'working', currentTool: 'Bash', project: 'API 장애 분석', action: 'log_01.txt 원인 추적 중', progress: 5, contextTokens: 90000, tokensIn: 72000, tokensOut: 18000, costUsd: 0.42, subagents: ['로그 패턴 분석', '메트릭 수집'], contextLimit: 200000 },
  { id: '%3', name: '유즈', character: '유즈', accent: 'lemon', status: 'working', currentTool: 'Edit', project: '자동화 스크립트', action: '빌드 파이프라인 작성', progress: 4, contextTokens: 64000, tokensIn: 50000, tokensOut: 14000, costUsd: 0.31 },
  { id: '%4', name: '아리스', character: '아리스', accent: 'lilac', status: 'waiting', project: '일일 보고서', progress: 3, contextTokens: 45000, tokensIn: 38000, tokensOut: 7000, costUsd: 0.15, lastReply: '이 방향이 맞을까요?' },
  { id: '%5', name: '미도리', character: '미도리', accent: 'mint', status: 'idle', project: '시스템 테스트', progress: 1, contextTokens: 12000, tokensIn: 10000, tokensOut: 2000, costUsd: 0.05 },
];

// 라우팅: mode 미설정/solo/?picker=1 → 시작 선택. god → SCHALE OS 교실.
export function App() {
  const agents = useStore((s) => s.agents);
  const [mode, setModeState] = useState<string | null | undefined>(undefined);
  const [cwd, setCwd] = useState<string | null>(null);
  const [configured, setConfigured] = useState(true);
  // 기본 뷰 = 터미널 pane 그리드(세션 뷰어). 교실(캐릭터)·카드는 토글로.
  const [view, setView] = useState<ViewMode>('terminal');
  // 중앙 멀티뷰 = 터미널 layout 미러(거노: 터미널이랑 pane 위치 동기화). fetchLayout 의
  // % 배치를 그대로 absolute 로 그린다. activeId = 마지막 포커스 학생(우측·교실 강조).
  // offlinePeek = 과거 세션 단독 보기(layout 과 별개, 읽기 전용).
  const [layoutRects, setLayoutRects] = useState<PaneRect[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  // 멀티뷰 타일 임시 전체화면 — 제목 더블클릭 토글(터미널 toggle_pane_zoom 의 arona 판).
  // 그 surface 하나만 풀커버로, 나머지는 가린다. 방 전환·layout 에서 사라지면 자동 해제.
  const [zoomedSurface, setZoomedSurface] = useState<string | null>(null);
  const [offlinePeek, setOfflinePeek] = useState<PeekItem | null>(null);
  // wry webview(WKWebView)는 브라우저와 달리 ⌘R/F5 기본 새로고침이 없다(거노: webview도
  // 새로고침되게). dev 서버(5173) 브라우저 기본과 겹쳐도 무해.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'F5' || ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'r')) {
        e.preventDefault();
        location.reload();
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, []);
  // 학생 대화를 열면 = 그 학생의 현재 대기('확인 필요')를 '확인'한 것 → 코랄 표시 해제.
  // 학생 클릭 = 터미널에서 그 pane 포커스(layout 미러라 위치는 터미널이 결정 — 거노: pane
  // 위치 동기화). 우측·교실 강조도 그 학생으로. 과거 세션 단독 보기 중이면 라이브로 복귀.
  const openStudent = (id: string, _title: string) => {
    void focusPane(id);
    setActiveId(id);
    setOfflinePeek(null);
    useStore.getState().ackStudent(id);
  };
  // 최근 세션 클릭 → 바로 resume 하지 않고, 그 세션의 대화를 오프라인(읽기 전용) 뷰어로
  // 메인에 단독 띄운다(layout 미러와 별개). 이어가기는 뷰어 하단 '현재 터미널에 입력'.
  const openOfflineSession = (s: RecentSession) => {
    setOfflinePeek({ id: s.id, title: s.label, offline: true, cwd: s.cwd });
    setView('terminal');
  };
  // 보드의 background daemon 세션 클릭 → 그 session_id 의 transcript 를 중앙에 읽기 전용으로
  // 띄운다(offlinePeek). resume(pane spawn) 과 별개라 daemon 세션을 안전하게 들여다보기.
  const openBackgroundSession = (a: BackgroundAgent) => {
    setOfflinePeek({ id: a.sessionId, title: a.name || a.id, offline: true, cwd: a.cwd });
    setView('terminal');
  };
  // 서브에이전트 드릴인 — 학생 메타칸의 ↳ 칩 클릭 → 그 서브에이전트 대화를 부모 옆 별도
  // 타일로(거노: "따로 볼수있게"). layout 미러엔 없는 가상 타일이라 별도 배열로 관리.
  // 같은 agentId 다시 누르면 토글로 닫힌다.
  const [subPeeks, setSubPeeks] = useState<{ parentSurface: string; agentId: string; agentType: string; label: string }[]>([]);
  const openSubagent = (parentSurface: string, agentId: string, agentType: string, label: string) => {
    setSubPeeks((prev) => prev.some((p) => p.agentId === agentId)
      ? prev.filter((p) => p.agentId !== agentId)
      : [...prev, { parentSurface, agentId, agentType, label }]);
    setView('terminal');
  };
  // 방 = kasaterm 윈도우(거노). GET /sessions 폴링 → 좌측 방 네비. 클릭하면 그 윈도우로.
  const [sessions, setSessions] = useState<SessionsInfo>({ count: 0, active: 0, labels: [], saved: [] });
  useEffect(() => {
    let stop = false;
    const tick = async () => { const s = await fetchSessions(); if (!stop) setSessions(s); };
    void tick();
    const iv = setInterval(tick, 1500);
    return () => { stop = true; clearInterval(iv); };
  }, []);
  const [usage, setUsage] = useState<ClaudeUsage | null>(null); // claude oauth 사용량(5h/주간)
  // 라이트/다크 테마 — 태양 버튼 토글, localStorage 영속(거노). data-theme 로 토큰 재매핑.
  const [theme, setTheme] = useState<'light' | 'dark'>(() => (localStorage.getItem('schale-theme') === 'dark' ? 'dark' : 'light'));
  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    localStorage.setItem('schale-theme', theme);
  }, [theme]);
  // 좌(방/학생)·우(업무/소스/스케줄) 패널 — 상단 아이콘 클릭 시 팝오버 오버레이로 펼침(거노:
  // 상단 아이콘+팝오버, 평소엔 멀티뷰만 꽉 차게). 기본 닫힘, 멀티뷰 공간을 안 뺏는다.
  const [leftOpen, setLeftOpen] = useState(false);
  const [rightOpen, setRightOpen] = useState(false);
  // 집중 모드 — 타이틀바·헤더·좌우 패널·풋터까지 전부 접고 중앙(멀티뷰/대화)만 꽉 채운다
  // (거노: 평소에도 다 닫을 수 있게). 헤더 버튼·⌘\ 토글, 우상단 떠있는 버튼·Esc 로 복귀.
  const [focusMode, setFocusMode] = useState(() => localStorage.getItem('schale-focus') === '1');
  useEffect(() => { localStorage.setItem('schale-focus', focusMode ? '1' : '0'); }, [focusMode]);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === '\\') { e.preventDefault(); setFocusMode((v) => !v); }
      else if (e.key === 'Escape') { setLeftOpen(false); setRightOpen(false); setFocusMode((v) => (v ? false : v)); }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, []);
  // claude oauth 사용량(5시간/주간 한도·리셋)을 1분마다 폴링 → TitleBar 게이지.
  // 실패(토큰만료·429·네트워크)면 null 이 오는데, 그때 칩을 지우면 1분마다 깜빡인다(거노).
  // 마지막 성공값을 유지하고, 실패는 그냥 무시 — 다음 성공 틱이 갱신.
  useEffect(() => {
    let stop = false;
    const tick = async () => { const u = await fetchClaudeUsage(); if (!stop) setUsage((prev) => u ?? prev); };
    void tick();
    const iv = setInterval(tick, 60_000);
    return () => { stop = true; clearInterval(iv); };
  }, []);

  const forcePicker = new URLSearchParams(location.search).get('picker') === '1';
  const forceMock = new URLSearchParams(location.search).get('mock') === '1';
  useEffect(() => {
    if (forceMock) { setModeState('god'); setCwd('/Users/kasa/Desktop/momewomo/tmuxify'); setConfigured(true); return; }
    (async () => {
      const { cwd } = await fetchMode();
      setCwd(cwd);
      setConfigured(true);
      // BA GUI = 세션 무접촉 시각 레이어 → 켜면 바로 교실(현재 터미널 cwd 그대로).
      // 폴더 온보딩(ModePicker pathOnly) 폐기: god 자율통솔 청산으로 leader 스폰이
      // 사라져 "폴더 골라 god 스폰" 단계가 무의미해졌다(거노). 교실로 직진.
      setModeState('god');
    })();
  }, []);
  useEffect(() => {
    if (forceMock) { useStore.getState().setAgents(MOCK_AGENTS); return; }
    if (mode === 'god') return startBoardPolling(1000);
  }, [mode]);
  // claude agents(pane 밖 background 학생) 폴링 → 교실에 별도 표시. claude 프로세스
  // spawn 비용이 있어 3초로 느슨하게(background 세션은 자주 안 바뀜).
  useEffect(() => {
    if (forceMock || mode !== 'god') return;
    let stop = false;
    const tick = async () => { const a = await fetchBackgroundAgents(); if (!stop) useStore.getState().setBackgroundAgents(a); };
    void tick();
    const iv = setInterval(tick, 3000);
    return () => { stop = true; clearInterval(iv); };
  }, [mode]);
  // 방 경로 라이브 반영 — 터미널에서 cd 하면 active_cwd(pid_cwd)가 바뀌니 폴링해
  // RoomChip 을 즉시 갱신(거노: 터미널 경로 변경이 방 경로에 바로 반영되게).
  useEffect(() => {
    if (forceMock || mode !== 'god') return;
    const iv = setInterval(() => {
      fetchMode().then(({ cwd }) => { if (cwd) setCwd(cwd); });
    }, 2000);
    return () => clearInterval(iv);
  }, [mode]);

  // 터미널 뷰 기본(A안: 중앙 단일 대화) — 선택된 학생이 없으면 god(아로나)·첫 학생을
  // 자동으로 띄워 중앙이 비지 않게. peek 있으면 그대로 둔다(거노).
  useEffect(() => {
    if (activeId || !agents.length) return;
    const first = [...agents].sort((a, b) => Number(b.isGod) - Number(a.isGod))[0];
    if (first) setActiveId(first.id);
  }, [agents, activeId]);
  // 터미널 layout(% 배치) 폴링 → 중앙 멀티뷰가 터미널 split 을 그대로 미러(거노: pane 위치
  // 동기화). 과거 세션 단독 보기 중이거나 터미널 뷰가 아니면 폴링 불필요.
  useEffect(() => {
    if (forceMock || mode !== 'god' || view !== 'terminal' || offlinePeek) return;
    let stop = false;
    const tick = async () => { const r = await fetchLayout(); if (!stop) setLayoutRects(r); };
    void tick();
    const iv = setInterval(tick, 1500);
    return () => { stop = true; clearInterval(iv); };
  }, [mode, view, offlinePeek]);
  // 줌된 pane 이 split/close 로 layout 에서 사라지면 전체화면 자동 해제(유령 줌 방지).
  useEffect(() => {
    if (zoomedSurface && !layoutRects.some((r) => r.surface_id === zoomedSurface)) setZoomedSurface(null);
  }, [layoutRects, zoomedSurface]);

  if (mode === undefined) {
    return <div style={{ padding: 24, color: 'var(--cth-ink-500)' }}>로딩…</div>;
  }
  // ?picker=1 디버그 — solo/god 선택 화면 전체.
  if (forcePicker) {
    return (
      <ModePicker
        cwd={cwd}
        onboarding={!configured}
        onPicked={(m) => { setModeState(m); setConfigured(true); }}
      />
    );
  }

  // god 먼저 정렬 + 작업명 학생에게 게임개발부 외형(spriteChar) 폴백 배정(교실 스프라이트용).
  const sorted = assignSprites([...agents].sort((a, b) => Number(b.isGod) - Number(a.isGod)));
  // 방 = kasaterm 윈도우. board(collab_board)는 활성 윈도우의 panes 만 주므로 보이는
  // 학생이 곧 그 방 학생 — 클라이언트 cwd 필터 없이 그대로 그린다(거노).
  const shown = sorted;
  // 배경 = 기본 교실바닥 하나로(거노: 평면도/방별맵 실험 접고 처음꺼 하나만).
  const roomBg = 'classroom-floor.png';

  // 우측 CommandCenter·교실 강조에 넘길 active 학생(과거 세션 보기 중이면 그걸 우선).
  const activeSelected: PeekItem | null = offlinePeek
    ?? (activeId ? { id: activeId, title: sorted.find((x) => x.id === activeId)?.name ?? activeId } : null);

  // 방(윈도우) 전환 — POST /session-switch?idx → 그 터미널 윈도우로(거노: "gui에서
  // 방바꾸면 터미널 윈도우 바뀌게"). board 폴링이 그 윈도우 학생으로 따라온다.
  const selectRoom = (idx: number) => {
    void switchSession(idx);
    setSessions((s) => ({ ...s, active: idx })); // 폴링 전 즉시 하이라이트
    // 방 바뀌면 active·과거세션 초기화 — layout 폴링이 새 방 pane 으로 미러를 갱신(거노)
    setActiveId(null);
    setOfflinePeek(null);
    setLayoutRects([]);
    setSubPeeks([]);
    setZoomedSurface(null);
  };

  return (
    // SCHALE OS 전체 셸: 세로 100% + 가로 100%
    <div style={{ height: '100%', display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>

      {/* 슬림 아이콘 바 — 집중 모드면 숨김. 좌=방, 우=업무·교실토글·집중(거노: 종·메일·터미널/교실탭 제거). */}
      {!focusMode && (<TitleBar
        leftBadge={agents.filter((a) => a.status === 'waiting' || a.status === 'blocked').length}
        usage={usage}
        theme={theme}
        onToggleTheme={() => setTheme((t) => (t === 'dark' ? 'light' : 'dark'))}
        onToggleLeft={() => { setLeftOpen((v) => !v); setRightOpen(false); }}
        onToggleRight={() => { setRightOpen((v) => !v); setLeftOpen(false); }}
        leftOpen={leftOpen}
        rightOpen={rightOpen}
        classroom={view === 'classroom'}
        onToggleClassroom={() => setView((v) => (v === 'classroom' ? 'terminal' : 'classroom'))}
        onFocus={() => setFocusMode(true)}
      />)}

      {/* 바디: 멀티뷰가 100% 점유 + 좌·우 패널은 팝오버 오버레이(거노: 패널이 공간 안 뺏게).
          relative = 팝오버 absolute 기준. */}
      <div style={{ flex: 1, display: 'flex', overflow: 'hidden', position: 'relative' }}>

        {/* 메인 컬럼 — 멀티뷰 */}
        <div style={{ flex: 1, display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>

          {/* 중앙: 터미널 pane 그리드(기본 뷰어) / 교실(캐릭터) / 카드 */}
          <div style={{ flex: 1, overflow: view === 'terminal' ? 'hidden' : 'auto', padding: view === 'terminal' ? 0 : view === 'classroom' ? 8 : 'var(--cth-space-4)' }}>
            {view === 'terminal' ? (
              offlinePeek ? (
                // 과거 세션 단독 보기(읽기 전용) — layout 미러와 별개로 전체를 덮는다.
                <TerminalPeekPanel
                  surfaceId=""
                  title={offlinePeek.title}
                  onClose={() => setOfflinePeek(null)}
                  embedded
                  session={{ id: offlinePeek.id, cwd: offlinePeek.cwd ?? '', label: offlinePeek.title }}
                />
              ) : (
                // layout 미러(좌, flex) + 서브에이전트 드릴인 타일(우, 별도 열) — 부모 옆에
                // 서브에이전트 대화를 동시에(거노: "따로 볼수있게").
                <div style={{ height: '100%', display: 'flex' }}>
                  <div style={{ flex: 1, minWidth: 0, position: 'relative' }}>
                    {layoutRects.length ? (
                      <div style={{ position: 'relative', height: '100%', background: 'var(--cth-cream-100)' }}>
                        {layoutRects.map((r) => {
                          const a = agents.find((x) => x.id === r.surface_id);
                          const isZoom = zoomedSurface === r.surface_id;
                          const hidden = zoomedSurface != null && !isZoom; // 다른 타일이 줌이면 가린다
                          return (
                            <div key={r.surface_id} onMouseDownCapture={() => setActiveId(r.surface_id)}
                              style={isZoom
                                ? { position: 'absolute', left: 0, top: 0, width: '100%', height: '100%', padding: 1, boxSizing: 'border-box', zIndex: 10, outline: '2px solid var(--cth-sky)', outlineOffset: -2 }
                                : { position: 'absolute', left: `${r.x}%`, top: `${r.y}%`, width: `${r.w}%`, height: `${r.h}%`, padding: 1, boxSizing: 'border-box', display: hidden ? 'none' : undefined, outline: activeId === r.surface_id ? '2px solid var(--cth-sky)' : 'none', outlineOffset: -2 }}>
                              {a ? (
                                <TerminalPeekPanel
                                  surfaceId={r.surface_id}
                                  title={a.name ?? r.surface_id}
                                  onClose={() => void focusPane(r.surface_id)}
                                  onOpenSubagent={openSubagent}
                                  onToggleZoom={() => setZoomedSurface((z) => (z === r.surface_id ? null : r.surface_id))}
                                  zoomed={isZoom}
                                  embedded
                                />
                              ) : (
                                /* claude 가 아닌 plain 터미널 — board 행이 없어 빈창이던 걸 Warp 명령블록 스택으로. */
                                <TerminalBlockCard
                                  surfaceId={r.surface_id}
                                  rect={r}
                                  onClose={() => void focusPane(r.surface_id)}
                                  onToggleZoom={() => setZoomedSurface((z) => (z === r.surface_id ? null : r.surface_id))}
                                  zoomed={isZoom}
                                  activeAgentId={agents[0]?.id}
                                />
                              )}
                            </div>
                          );
                        })}
                      </div>
                    ) : (
                      <div style={{ height: '100%', display: 'flex', alignItems: 'center', justifyContent: 'center', color: 'var(--cth-ink-300)', fontFamily: 'var(--cth-font-ui)', fontSize: 13 }}>
                        터미널 pane 을 기다리는 중…
                      </div>
                    )}
                  </div>
                  {subPeeks.length > 0 && (
                    <div style={{ width: 360, flexShrink: 0, display: 'flex', flexDirection: 'column', borderLeft: '1px solid var(--cth-cream-200)', background: 'var(--cth-cream-100)' }}>
                      {/* 서브에이전트 뷰 열 — 부모(학생) 세션에 속한 영역임을 헤더로 못박는다(거노:
                          다른 pane 과 헷갈리지 않게). 각 타일은 부모 이름 prefix + 스카이 좌측 띠로 묶음. */}
                      <div style={{ flexShrink: 0, padding: '6px 12px', fontFamily: 'var(--cth-font-display)', fontSize: 10, fontWeight: 700, color: 'var(--cth-sky)', borderBottom: '1px solid var(--cth-cream-200)', display: 'flex', alignItems: 'center', gap: 6 }}>
                        <svg width="12" height="12" viewBox="0 0 16 16"><path d="M4 3v6a3 3 0 0 0 3 3h6" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" /></svg>
                        서브에이전트 뷰
                      </div>
                      {subPeeks.map((p) => {
                        const parentName = agents.find((a) => a.id === p.parentSurface)?.name ?? p.parentSurface;
                        return (
                          <div key={p.agentId} style={{ flex: 1, minHeight: 0, borderBottom: '1px solid var(--cth-cream-200)', borderLeft: '3px solid var(--cth-sky)', boxSizing: 'border-box' }}>
                            <TerminalPeekPanel
                              surfaceId=""
                              title={`${parentName} ↳ ${p.label}`}
                              embedded
                              subagent={p}
                              onClose={() => setSubPeeks((prev) => prev.filter((x) => x.agentId !== p.agentId))}
                            />
                          </div>
                        );
                      })}
                    </div>
                  )}
                </div>
              )
            ) : (
              <ClassroomView agents={shown} background={roomBg} onSelect={openStudent} selectedId={activeId ?? undefined} />
            )}
          </div>
        </div>

        {/* 좌 패널 팝오버 — 방·학생. 딤 클릭/Esc 닫힘. 선택 시 자동 닫힘(거노: 상단 아이콘+팝오버). */}
        {!focusMode && leftOpen && (
          <>
            <div onClick={() => setLeftOpen(false)} style={{ position: 'absolute', inset: 0, zIndex: 30, background: 'rgba(21,41,74,0.16)' }} />
            <div style={{ position: 'absolute', left: 0, top: 0, bottom: 0, zIndex: 31, display: 'flex', boxShadow: '3px 0 16px rgba(21,41,74,0.20)' }}>
              <RoomMap
                sessions={sessions}
                onSwitch={(i) => { selectRoom(i); setLeftOpen(false); }}
                agents={shown}
                selectedId={activeId ?? undefined}
                onSelectStudent={(id, t) => { openStudent(id, t); setLeftOpen(false); }}
                onNewRoom={(god) => { void newRoom(god); }}
                onCloseRoom={(i) => { void closeRoom(i); }}
                onOpenSession={(s) => { openOfflineSession(s); setLeftOpen(false); }}
                onCollapse={() => setLeftOpen(false)}
              />
            </div>
          </>
        )}

        {/* 우 패널 팝오버 — 업무·소스 컨트롤·스케줄. 학생 픽은 패널 유지(계속 보게). */}
        {!focusMode && rightOpen && (
          <>
            <div onClick={() => setRightOpen(false)} style={{ position: 'absolute', inset: 0, zIndex: 30, background: 'rgba(21,41,74,0.16)' }} />
            <div style={{ position: 'absolute', right: 0, top: 0, bottom: 0, zIndex: 31, width: 340, display: 'flex', minHeight: 0, boxShadow: '-3px 0 16px rgba(21,41,74,0.20)' }}>
              <CommandCenter selected={activeSelected} onClearDialog={() => setOfflinePeek(null)} onPickStudent={openStudent} onOpenBackground={openBackgroundSession} openGitTab={0} onCollapse={() => setRightOpen(false)} />
            </div>
          </>
        )}
      </div>

      {/* 집중 모드 복귀 — 패널 다 숨겼을 때만 우상단 떠있는 버튼(거노). Esc·⌘\ 로도 해제. */}
      {focusMode && (
        <button onClick={() => setFocusMode(false)} title="패널 보이기 (Esc · ⌘\)"
          style={{ position: 'fixed', top: 8, right: 8, zIndex: 50, width: 30, height: 30, borderRadius: 9, cursor: 'pointer', border: '1px solid var(--cth-cream-200)', background: 'var(--cth-cream-50)', color: 'var(--cth-ink-500)', display: 'inline-flex', alignItems: 'center', justifyContent: 'center', boxShadow: '0 2px 8px rgba(21,41,74,0.16)' }}>
          <svg width="16" height="16" viewBox="0 0 16 16"><path d="M2.5 6V2.5H6M14 6V2.5h-3.5M2.5 10v3.5H6M14 10v3.5h-3.5" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" /></svg>
        </button>
      )}

    </div>
  );
}
