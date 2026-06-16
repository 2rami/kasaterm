import { useEffect, useState } from 'react';
import { useStore, type Agent } from './store';
import { AgentCard } from './components/AgentCard';
import { AddAgentModal } from './components/AddAgentModal';
import { ModePicker } from './components/ModePicker';
import { ClassroomView } from './components/ClassroomView';
import { CommandCenter } from './components/CommandCenter';
import { StudentGrid } from './components/StudentGrid';
import { Footer } from './components/Footer';
import { TitleBar } from './components/TitleBar';
import { RoomChip } from './components/RoomChip';
import { RoomPathModal } from './components/RoomPathModal';
import { RoomMap } from './components/RoomMap';
import { ResizeHandle } from './components/ResizeHandle';
import { PixelButton } from './components/PixelButton';
import { SegmentedTabs } from './components/GameKit';
import { startBoardPolling, fetchMode, focusPane, revealTerminal, setMode, fetchBoard, fetchCharacters, spawnAgent, fetchClaudeUsage, fetchSessions, switchSession, newRoom, closeRoom, type ClaudeUsage, type SessionsInfo } from './lib/mcp';

type ViewMode = 'classroom' | 'grid';

// dev 디자인 검증용 목 학생(URL ?mock=1). board 비어도 풀 화면을 본다.
const MOCK_AGENTS: Agent[] = [
  { id: '%1', name: '아로나', character: '아로나', accent: 'sky', status: 'idle', project: 'tmuxify', progress: 2, contextTokens: 30000, tokensIn: 24000, tokensOut: 6000, costUsd: 0.18, contextLimit: 200000, model: 'claude-opus-4-8', cwd: '/Users/kasa/Desktop/momewomo/tmuxify', branch: 'main', isGod: true, lastReply: '선생님, 오늘 의뢰 정리했어요!' },
  { id: '%2', name: '시로코', character: '시로코', accent: 'coral', status: 'working', currentTool: 'Bash', project: 'API 장애 분석', action: 'log_01.txt 원인 추적 중', progress: 5, contextTokens: 90000, tokensIn: 72000, tokensOut: 18000, costUsd: 0.42, subagents: ['로그 패턴 분석', '메트릭 수집'], contextLimit: 200000 },
  { id: '%3', name: '유우카', character: '유우카', accent: 'lemon', status: 'working', currentTool: 'Edit', project: '자동화 스크립트', action: '빌드 파이프라인 작성', progress: 4, contextTokens: 64000, tokensIn: 50000, tokensOut: 14000, costUsd: 0.31 },
  { id: '%4', name: '아리스', character: '아리스', accent: 'lilac', status: 'waiting', project: '일일 보고서', progress: 3, contextTokens: 45000, tokensIn: 38000, tokensOut: 7000, costUsd: 0.15, lastReply: '이 방향이 맞을까요?' },
  { id: '%5', name: '호시노', character: '호시노', accent: 'peach', status: 'idle', project: '시스템 테스트', progress: 1, contextTokens: 12000, tokensIn: 10000, tokensOut: 2000, costUsd: 0.05 },
  { id: '%6', name: '코하루', character: '코하루', accent: 'mint', status: 'blocked', currentTool: 'Read', project: '사용자 로그 분석', progress: 6, contextTokens: 110000, tokensIn: 90000, tokensOut: 20000, costUsd: 0.55, lastReply: '접근 권한이 필요해요' },
];

// 라우팅: mode 미설정/solo/?picker=1 → 시작 선택. god → SCHALE OS 교실.
export function App() {
  const agents = useStore((s) => s.agents);
  const [mode, setModeState] = useState<string | null | undefined>(undefined);
  const [cwd, setCwd] = useState<string | null>(null);
  const [configured, setConfigured] = useState(true);
  const [view, setView] = useState<ViewMode>('classroom');
  const [showAdd, setShowAdd] = useState(false);
  const [revealing, setRevealing] = useState(false);
  const [peek, setPeek] = useState<{ id: string; title: string } | null>(null);
  // 학생 대화를 열면 = 그 학생의 현재 대기('확인 필요')를 '확인'한 것 → 코랄 표시 해제.
  const openStudent = (id: string, title: string) => { setPeek({ id, title }); useStore.getState().ackStudent(id); };
  const [showRoomModal, setShowRoomModal] = useState(false);
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
      const { mode, cwd, configured } = await fetchMode();
      setCwd(cwd);
      // ?picker=1 디버그 진입에서만 선택 화면을 띄운다.
      if (forcePicker) { setModeState(mode); setConfigured(configured); return; }
      // 아로나 창을 연 것 자체가 god 의도 — 다시 묻지 않고 바로 진입한다.
      // 아직 god 이 아니면 모드 마커를 god 으로 쓰고, 교실이 비어 있으면
      // leader(아로나)를 자동 등판시킨다(옛 ModePicker.pickGod 로직 이관).
      if (mode !== 'god') {
        await setMode('god');
        const board = await fetchBoard();
        if (board.length === 0) {
          const chars = await fetchCharacters();
          const leader = chars?.leader?.name;
          if (leader) await spawnAgent({ character: leader });
        }
      }
      setConfigured(true);
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

  if (mode === undefined) {
    return <div style={{ padding: 24, color: 'var(--cth-ink-500)' }}>로딩…</div>;
  }
  // 평시엔 ModePicker 없이 god 직행(부트 effect 가 god 으로 만든다). 선택
  // 화면은 ?picker=1 디버그에서만.
  if (forcePicker) {
    return (
      <ModePicker
        cwd={cwd}
        onboarding={!configured}
        onPicked={(m) => { setModeState(m); setConfigured(true); }}
      />
    );
  }

  const sorted = [...agents].sort((a, b) => Number(b.isGod) - Number(a.isGod));
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
    setPeek(null); // 방 바뀌면 우측 학생별 대화도 초기화 — 다른 방 학생 대화 stale 방지(거노)
  };

  return (
    // SCHALE OS 전체 셸: 세로 100% + 가로 100%
    <div style={{ height: '100%', display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>

      {/* 타이틀바 */}
      <TitleBar
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
          if (a) setPeek({ id: a.id, title: a.character });
        }}
        onMail={() => { const a = sorted.find((x) => x.status === 'success'); if (a) setPeek({ id: a.id, title: a.character }); }}
        onSettings={reveal}
      />

      {/* 헤더 */}
      <div style={{
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
          <RoomChip cwd={cwd} onClick={() => setShowRoomModal(true)} />
        </div>

        {/* 뷰 탭 */}
        <SegmentedTabs<ViewMode>
          options={[{ value: 'classroom', label: '교실' }, { value: 'grid', label: '카드' }]}
          value={view}
          onChange={setView}
          size="sm"
          style={{ marginLeft: 8 }}
        />

        <div style={{ flex: 1 }} />

        {/* 우측 액션 — 학생 부르기는 교실 빈 자리 버튼으로 이동(거노) */}
        <PixelButton variant="secondary" size="sm" onClick={reveal}>
          {revealing ? '여는 중…' : '터미널 보기'}
        </PixelButton>
      </div>

      {/* 바디: 좌측 장소 네비 + 메인 영역 + 우측 CommandCenter */}
      <div style={{ flex: 1, display: 'flex', overflow: 'hidden' }}>

        {/* 좌측 장소(워크스페이스) 네비 — 방 여러 개일 때만 */}
        <RoomMap sessions={sessions} onSwitch={selectRoom} onNewRoom={(god) => { void newRoom(god); }} onCloseRoom={(i) => { void closeRoom(i); }} />

        {/* 메인 컬럼 */}
        <div style={{ flex: 1, display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>

          {/* 교실 씬 or 카드 그리드 */}
          <div style={{ flex: 1, overflow: 'auto', padding: view === 'classroom' ? 8 : 'var(--cth-space-4)' }}>
            {view === 'classroom' ? (
              <ClassroomView agents={shown} background={roomBg} onAdd={() => setShowAdd(true)} onSelect={openStudent} selectedId={peek?.id} />
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

          {/* 교실↔하단 경계 — 세로 드래그로 하단 영역 높이 조절(거노: 나눠진 곳 모두). */}
          <ResizeHandle dir="row" onDrag={(dy) => setBottomH((h) => Math.min(440, Math.max(110, h - dy)))} />

          {/* 하단(학생카드 + 풋터) — 높이 드래그 조절 */}
          <div style={{ height: bottomH, flexShrink: 0, overflowY: 'auto', display: 'flex', flexDirection: 'column' }}>
            <div style={{ flexShrink: 0, borderTop: '1px solid var(--cth-cream-200)', background: 'var(--cth-cream-50)' }}>
              <StudentGrid agents={shown} onSelect={openStudent} />
            </div>
            <div style={{ flexShrink: 0, borderTop: '1px solid var(--cth-cream-200)', background: 'var(--cth-cream-50)' }}>
              <Footer
                onManage={() => setShowAdd(true)}
                onNewRequest={() => setShowAdd(true)}
                inputTokens={totalInputTokens}
                costUsd={totalCostUsd}
                contextPct={contextPct}
              />
            </div>
          </div>
        </div>

        {/* 우측: Command Center 항상 — 학생 클릭 시 '학생별 대화' 탭에 대화 통합(거노).
            좌측 핸들 드래그로 폭 조절. */}
        <ResizeHandle dir="col" onDrag={(dx) => setCcWidth((w) => Math.min(640, Math.max(260, w - dx)))} />
        <div style={{ width: ccWidth, flexShrink: 0, display: 'flex', minHeight: 0 }}>
          <CommandCenter selected={peek} onClearDialog={() => setPeek(null)} onPickStudent={openStudent} />
        </div>
      </div>

      {showAdd && <AddAgentModal onClose={() => setShowAdd(false)} defaultCwd={cwd} />}

      {showRoomModal && cwd && (
        <RoomPathModal
          initialPath={cwd}
          onClose={() => setShowRoomModal(false)}
          onChanged={(p) => setCwd(p)}
        />
      )}
    </div>
  );
}
