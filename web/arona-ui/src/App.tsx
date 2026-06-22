import { useEffect, useState } from 'react';
import { useStore, type Agent } from './store';
import { AgentCard } from './components/AgentCard';
import { ModePicker } from './components/ModePicker';
import { ClassroomView } from './components/ClassroomView';
import { CommandCenter } from './components/CommandCenter';
import { StudentGrid } from './components/StudentGrid';
import { Footer } from './components/Footer';
import { TitleBar } from './components/TitleBar';
import { RoomMap } from './components/RoomMap';
import { ResizeHandle } from './components/ResizeHandle';
import { PixelButton } from './components/PixelButton';
import { SegmentedTabs } from './components/GameKit';
import { startBoardPolling, fetchMode, focusPane, revealTerminal, fetchClaudeUsage, fetchSessions, switchSession, newRoom, closeRoom, fetchLayout, type ClaudeUsage, type SessionsInfo, type RecentSession, type PaneRect } from './lib/mcp';
import { TerminalPeekPanel } from './components/TerminalPeekPanel';
import { StudentNav } from './components/StudentNav';
import { assignSprites } from './lib/sprites';

type ViewMode = 'terminal' | 'classroom' | 'grid';
// 중앙 멀티뷰 한 칸 = 살아있는 학생(surface id) 또는 오프라인 과거 세션(offline=true).
type PeekItem = { id: string; title: string; offline?: boolean; cwd?: string };

// 접힌 사이드 패널 = 얇은 레일 + 펼치기 화살표(거노: 가장자리에서 접고 펴기). 좌/우 공용 —
// 테두리 방향과 화살표만 side 로 분기. RoomMap 의 옛 내부 접기 레일을 좌우 존 단위로 일반화.
function EdgeRail({ side, onExpand }: { side: 'left' | 'right'; onExpand: () => void }) {
  return (
    <div style={{
      width: 30, flexShrink: 0, height: '100%',
      ...(side === 'left'
        ? { borderRight: '1px solid var(--cth-cream-200)' }
        : { borderLeft: '1px solid var(--cth-cream-200)' }),
      background: 'var(--cth-cream-50)', padding: '10px 0',
      display: 'flex', flexDirection: 'column', alignItems: 'center',
    }}>
      <button onClick={onExpand} title="패널 펼치기" style={{
        width: 22, height: 22, borderRadius: 6, border: 'none', cursor: 'pointer',
        background: 'var(--cth-cream-100)', color: 'var(--cth-ink-500)',
        display: 'flex', alignItems: 'center', justifyContent: 'center',
      }}>
        <svg width="12" height="12" viewBox="0 0 16 16"><path d={side === 'left' ? 'M6 3l5 5-5 5' : 'M10 3l-5 5 5 5'} stroke="currentColor" strokeWidth="2" fill="none" strokeLinecap="round" strokeLinejoin="round" /></svg>
      </button>
    </div>
  );
}

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
  const [revealing, setRevealing] = useState(false);
  // 중앙 멀티뷰 = 터미널 layout 미러(거노: 터미널이랑 pane 위치 동기화). fetchLayout 의
  // % 배치를 그대로 absolute 로 그린다. activeId = 마지막 포커스 학생(우측·교실 강조).
  // offlinePeek = 과거 세션 단독 보기(layout 과 별개, 읽기 전용).
  const [layoutRects, setLayoutRects] = useState<PaneRect[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [offlinePeek, setOfflinePeek] = useState<PeekItem | null>(null);
  const [gitNonce, setGitNonce] = useState(0); // 타이틀바 소스컨트롤 버튼 → CommandCenter git 탭 전환 신호
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
  // 우측 Command Center 폭 — 드래그 조절(거노: 각 영역 크기조절), localStorage 영속.
  const [ccWidth, setCcWidth] = useState(() => Number(localStorage.getItem('schale-cc-width')) || 320);
  useEffect(() => { localStorage.setItem('schale-cc-width', String(ccWidth)); }, [ccWidth]);
  // 하단(학생카드+풋터) 영역 높이 — 교실과의 경계 드래그 조절(거노: 나눠진 곳 모두).
  const [bottomH, setBottomH] = useState(() => Number(localStorage.getItem('schale-bottom-h')) || 200);
  useEffect(() => { localStorage.setItem('schale-bottom-h', String(bottomH)); }, [bottomH]);
  // 좌(방/학생목록)·우(커맨드센터) 패널 숨김 — 멀티뷰 공간 확보(거노). localStorage 영속.
  const [leftHidden, setLeftHidden] = useState(() => localStorage.getItem('schale-left-hidden') === '1');
  const [rightHidden, setRightHidden] = useState(() => localStorage.getItem('schale-right-hidden') === '1');
  useEffect(() => { localStorage.setItem('schale-left-hidden', leftHidden ? '1' : '0'); }, [leftHidden]);
  useEffect(() => { localStorage.setItem('schale-right-hidden', rightHidden ? '1' : '0'); }, [rightHidden]);
  // 집중 모드 — 타이틀바·헤더·좌우 패널·풋터까지 전부 접고 중앙(멀티뷰/대화)만 꽉 채운다
  // (거노: 평소에도 다 닫을 수 있게). 헤더 버튼·⌘\ 토글, 우상단 떠있는 버튼·Esc 로 복귀.
  const [focusMode, setFocusMode] = useState(() => localStorage.getItem('schale-focus') === '1');
  useEffect(() => { localStorage.setItem('schale-focus', focusMode ? '1' : '0'); }, [focusMode]);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === '\\') { e.preventDefault(); setFocusMode((v) => !v); }
      else if (e.key === 'Escape') setFocusMode((v) => (v ? false : v));
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, []);
  // 반응형: 창 폭이 좁으면(작은 모니터) 좌우 패널을 자동으로 접어 대화창을 넓힌다(거노).
  // 토글 버튼은 그 위에서 수동 override — 같은 폭 구간에선 유지되고, 넓음↔좁음이 바뀔 때만
  // 자동값으로 재설정된다(멀티뷰 2칸+좌우 패널이 안 들어가는 1100px 가 경계).
  const [narrow, setNarrow] = useState(() => window.innerWidth < 1100);
  useEffect(() => {
    const onResize = () => setNarrow(window.innerWidth < 1100);
    window.addEventListener('resize', onResize);
    return () => window.removeEventListener('resize', onResize);
  }, []);
  useEffect(() => { setLeftHidden(narrow); setRightHidden(narrow); }, [narrow]);

  // claude oauth 사용량(5시간/주간 한도·리셋)을 1분마다 폴링 → TitleBar 게이지.
  useEffect(() => {
    let stop = false;
    const tick = async () => { const u = await fetchClaudeUsage(); if (!stop) setUsage(u); };
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

  // 재화 = claude 토큰 지표(선생님): 💎입력토큰 · 🪙비용$ (전 학생 합산).
  const totalInputTokens = sorted.reduce((s, a) => s + (a.tokensIn ?? 0), 0);
  const totalCostUsd = sorted.reduce((s, a) => s + (a.costUsd ?? 0), 0);
  const totalContextTokens = sorted.reduce((s, a) => s + (a.contextTokens ?? 0), 0);
  // 인연(호감도) = 학생들 컨텍스트 사용량 % 평균(claude TUI 상태바 파싱 — transcript
  // 토큰이 0 이어도 robust). % 있는 학생만 집계, 없으면 0.
  const ctxStudents = sorted.filter((a) => (a.contextPct ?? 0) > 0);
  const contextPct = ctxStudents.length
    ? ctxStudents.reduce((s, a) => s + (a.contextPct ?? 0), 0) / ctxStudents.length
    : 0;
  // 우측 CommandCenter·교실 강조에 넘길 active 학생(과거 세션 보기 중이면 그걸 우선).
  const activeSelected: PeekItem | null = offlinePeek
    ?? (activeId ? { id: activeId, title: sorted.find((x) => x.id === activeId)?.name ?? activeId } : null);

  const reveal = async () => {
    setRevealing(true);
    await revealTerminal(1);
    setTimeout(() => setRevealing(false), 600);
  };

  // 방(윈도우) 전환 — POST /session-switch?idx → 그 터미널 윈도우로(거노: "gui에서
  // 방바꾸면 터미널 윈도우 바뀌게"). board 폴링이 그 윈도우 학생으로 따라온다.
  const selectRoom = (idx: number) => {
    void switchSession(idx);
    setSessions((s) => ({ ...s, active: idx })); // 폴링 전 즉시 하이라이트
    // 방 바뀌면 active·과거세션 초기화 — layout 폴링이 새 방 pane 으로 미러를 갱신(거노)
    setActiveId(null);
    setOfflinePeek(null);
    setLayoutRects([]);
  };

  return (
    // SCHALE OS 전체 셸: 세로 100% + 가로 100%
    <div style={{ height: '100%', display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>

      {/* 타이틀바 — 집중 모드면 숨김 */}
      {!focusMode && (<TitleBar
        notifications={agents.filter((a) => a.status === 'waiting' || a.status === 'blocked').length}
        mail={agents.filter((a) => a.status === 'success').length}
        contextTokens={totalContextTokens}
        usage={usage}
        theme={theme}
        onToggleTheme={() => setTheme((t) => (t === 'dark' ? 'light' : 'dark'))}
        onBell={() => {
          // 주의(대기/막힘) 학생 우선, 없으면 작업 중, 그것도 없으면 첫 학생 — 종은
          // 항상 무언가 연다(거노: 클릭해도 무반응이던 것).
          const a = sorted.find((x) => x.status === 'waiting' || x.status === 'blocked')
            || sorted.find((x) => x.status === 'working' || x.status === 'thinking')
            || sorted[0];
          if (a) { void focusPane(a.id); setActiveId(a.id); setOfflinePeek(null); }
        }}
        onMail={() => { const a = sorted.find((x) => x.status === 'success'); if (a) { void focusPane(a.id); setActiveId(a.id); setOfflinePeek(null); } }}
        onSettings={() => setGitNonce((n) => n + 1)}
      />)}

      {/* 헤더 — 집중 모드면 숨김 */}
      {!focusMode && (<div style={{
        display: 'flex', alignItems: 'center', gap: 8,
        padding: '10px 16px',
        borderBottom: '1px solid var(--cth-cream-200)',
        background: 'var(--cth-cream-50)',
        boxShadow: '0 1px 3px rgba(21, 41, 74, 0.04)',
        flexShrink: 0
      }}>
        {/* 제목 + 방 칩 */}
        <div style={{ display: 'flex', alignItems: 'baseline', gap: 8, minWidth: 0 }}>
          <h1 style={{
            fontFamily: 'var(--cth-font-display)',
            fontSize: 'var(--cth-text-display-lg)',
            lineHeight: 'var(--cth-lh-display-lg)',
            color: 'var(--cth-ink-900)', margin: 0, whiteSpace: 'nowrap'
          }}>
            SCHALE Headquarters
          </h1>
          {/* 경로는 BA GUI 에서 바꾸지 않는다 — 터미널에서 그 방(pane) 켤 때의 cwd 로 고정(거노). */}
        </div>

        {/* 뷰 탭 */}
        <SegmentedTabs<ViewMode>
          options={[{ value: 'terminal', label: '터미널' }, { value: 'classroom', label: '교실' }, { value: 'grid', label: '카드' }]}
          value={view}
          onChange={setView}
          size="sm"
          style={{ marginLeft: 8 }}
        />

        <div style={{ flex: 1 }} />

        {/* 집중 모드 — 패널 가장자리 접기와 별개로, 헤더·타이틀바·풋터까지 전부 한 번에
            닫는 단일 컨트롤(거노: 평소에도 다 닫기). 좌/우 개별 접기는 각 패널 가장자리에서. */}
        <button onClick={() => setFocusMode(true)} title="패널 전부 숨기기 (⌘\)"
          style={{ width: 28, height: 28, borderRadius: 8, cursor: 'pointer', border: '1px solid var(--cth-cream-200)', background: 'var(--cth-cream-100)', color: 'var(--cth-ink-500)', display: 'inline-flex', alignItems: 'center', justifyContent: 'center' }}>
          <svg width="15" height="15" viewBox="0 0 16 16"><path d="M6 2.5H3.5V5M10 2.5h2.5V5M6 13.5H3.5V11M10 13.5h2.5V11" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" /></svg>
        </button>

        {/* 우측 액션 — 학생 부르기는 교실 빈 자리 버튼으로 이동(거노) */}
        <PixelButton variant="secondary" size="sm" onClick={reveal}>
          {revealing ? '여는 중…' : '터미널 보기'}
        </PixelButton>
      </div>)}

      {/* 바디: 좌측 장소 네비 + 메인 영역 + 우측 CommandCenter */}
      <div style={{ flex: 1, display: 'flex', overflow: 'hidden' }}>

        {/* 좌측 존(방 + 학생) — 한 단위로 접힘(거노: 가장자리 접기). 접히면 얇은 레일+▶,
            펼치면 RoomMap 헤더의 ◀ 로 접는다. 교실 뷰는 캐릭터가 중앙이라 학생 네비 숨김.
            집중 모드면 레일째 사라짐. */}
        {!focusMode && (leftHidden ? (
          <EdgeRail side="left" onExpand={() => setLeftHidden(false)} />
        ) : (
          <>
            <RoomMap sessions={sessions} onSwitch={selectRoom} onNewRoom={(god) => { void newRoom(god); }} onCloseRoom={(i) => { void closeRoom(i); }} onOpenSession={openOfflineSession} onCollapse={() => setLeftHidden(true)} />
            {view !== 'classroom' && (
              <StudentNav agents={shown} selectedId={activeId ?? undefined} onSelect={openStudent} />
            )}
          </>
        ))}

        {/* 메인 컬럼 */}
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
              ) : layoutRects.length ? (
                // 터미널 layout 미러 — 각 pane 을 터미널과 같은 % 위치/크기로 absolute 배치(거노).
                <div style={{ position: 'relative', height: '100%', background: 'var(--cth-cream-200)' }}>
                  {layoutRects.map((r) => {
                    const a = agents.find((x) => x.id === r.surface_id);
                    return (
                      <div key={r.surface_id} onMouseDownCapture={() => setActiveId(r.surface_id)}
                        style={{ position: 'absolute', left: `${r.x}%`, top: `${r.y}%`, width: `${r.w}%`, height: `${r.h}%`, padding: 1, boxSizing: 'border-box', outline: activeId === r.surface_id ? '2px solid var(--cth-sky)' : 'none', outlineOffset: -2 }}>
                        <TerminalPeekPanel
                          surfaceId={r.surface_id}
                          title={a?.name ?? r.surface_id}
                          onClose={() => void focusPane(r.surface_id)}
                          embedded
                        />
                      </div>
                    );
                  })}
                </div>
              ) : (
                <div style={{ height: '100%', display: 'flex', alignItems: 'center', justifyContent: 'center', color: 'var(--cth-ink-300)', fontFamily: 'var(--cth-font-ui)', fontSize: 13 }}>
                  터미널 pane 을 기다리는 중…
                </div>
              )
            ) : view === 'classroom' ? (
              <ClassroomView agents={shown} background={roomBg} onSelect={openStudent} selectedId={activeId ?? undefined} />
            ) : shown.length === 0 ? (
              <p style={{ color: 'var(--cth-ink-500)' }}>학생들을 기다리는 중… (board 폴링 · MCP)</p>
            ) : (
              <div style={{ display: 'flex', flexWrap: 'wrap', gap: 'var(--cth-space-4)' }}>
                {shown.map((a) => (
                  <AgentCard
                    key={a.id}
                    name={a.name}
                    character={a.character}
                    accent={a.accent}
                    status={a.status}
                    project={a.project}
                    action={a.action}
                    progress={a.progress}
                    contextTokens={a.contextTokens}
                    contextLimit={a.contextLimit}
                    isGod={a.isGod}
                    onClick={() => { void focusPane(a.id); }}
                  />
                ))}
              </div>
            )}
          </div>

          {/* 학생 카드 그리드 — 터미널 뷰는 좌측 StudentNav 로 학생을 대체하므로 접어
              중앙 PaneGrid 를 풀높이로(거노: 뷰어 비율 ↑·캐릭터 ↓). 교실/카드 뷰는
              기존대로 카드 그리드 + 높이 드래그. */}
          {view !== 'terminal' && (
            <>
              <ResizeHandle dir="row" onDrag={(dy) => setBottomH((h) => Math.min(440, Math.max(110, h - dy)))} />
              <div style={{ height: bottomH, flexShrink: 0, overflowY: 'auto', borderTop: '1px solid var(--cth-cream-200)', background: 'var(--cth-cream-50)' }}>
                <StudentGrid agents={shown} onSelect={openStudent} />
              </div>
            </>
          )}

          {/* 재화 풋터 — 집중 모드면 숨김(거노: 평소에도 다 닫기). */}
          {!focusMode && (<div style={{ flexShrink: 0, borderTop: '1px solid var(--cth-cream-200)', background: 'var(--cth-cream-50)' }}>
            <Footer
              inputTokens={totalInputTokens}
              costUsd={totalCostUsd}
              contextPct={contextPct}
            />
          </div>)}
        </div>

        {/* 우측: Command Center — 가장자리 접기(거노). 접히면 얇은 레일+◀, 펼치면 CC 헤더의
            ▶ 로 접는다. 학생 클릭 시 '학생별 대화' 탭에 active 학생 통합. 집중 모드면 레일째 사라짐. */}
        {!focusMode && (rightHidden ? (
          <EdgeRail side="right" onExpand={() => setRightHidden(false)} />
        ) : (
          <>
            <ResizeHandle dir="col" onDrag={(dx) => setCcWidth((w) => Math.min(640, Math.max(260, w - dx)))} />
            <div style={{ width: ccWidth, flexShrink: 0, display: 'flex', minHeight: 0 }}>
              <CommandCenter selected={activeSelected} onClearDialog={() => setOfflinePeek(null)} onPickStudent={openStudent} openGitTab={gitNonce} onCollapse={() => setRightHidden(true)} />
            </div>
          </>
        ))}
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
