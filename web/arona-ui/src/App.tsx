import { useEffect, useState } from 'react';
import { useStore, type Agent } from './store';
import { AgentCard } from './components/AgentCard';
import { AddAgentModal } from './components/AddAgentModal';
import { ModePicker } from './components/ModePicker';
import { ClassroomView } from './components/ClassroomView';
import { ClassroomChatInput } from './components/ClassroomChatInput';
import { CommandCenter } from './components/CommandCenter';
import { StudentGrid } from './components/StudentGrid';
import { Footer } from './components/Footer';
import { TitleBar } from './components/TitleBar';
import { TerminalPeekPanel } from './components/TerminalPeekPanel';
import { RoomChip } from './components/RoomChip';
import { PixelButton } from './components/PixelButton';
import { SegmentedTabs } from './components/GameKit';
import { startBoardPolling, fetchMode, focusPane, revealTerminal, fetchSchaleState, type SchaleState } from './lib/mcp';

type ViewMode = 'classroom' | 'grid';

// dev 디자인 검증용 목 학생(URL ?mock=1). board 비어도 풀 화면을 본다.
const MOCK_AGENTS: Agent[] = [
  { id: '%1', name: '아로나', character: '아로나', accent: 'sky', status: 'idle', project: 'tmuxify', progress: 2, contextTokens: 30000, tokensIn: 24000, tokensOut: 6000, costUsd: 0.18, contextLimit: 200000, isGod: true, lastReply: '선생님, 오늘 의뢰 정리했어요!' },
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
  const [showChatInput, setShowChatInput] = useState(false);
  const [schaleState, setSchaleState] = useState<SchaleState | null>(null);

  const forcePicker = new URLSearchParams(location.search).get('picker') === '1';
  const forceMock = new URLSearchParams(location.search).get('mock') === '1';

  useEffect(() => {
    if (forceMock) { setModeState('god'); setCwd('/Users/kasa/Desktop/momewomo/tmuxify'); setConfigured(true); return; }
    fetchMode().then(({ mode, cwd, configured }) => {
      setModeState(mode); setCwd(cwd); setConfigured(configured);
    });
  }, []);
  useEffect(() => {
    if (forceMock) { useStore.getState().setAgents(MOCK_AGENTS); return; }
    if (mode === 'god') return startBoardPolling(1000);
  }, [mode]);
  useEffect(() => {
    if (mode !== 'god') return;
    const tick = () => { fetchSchaleState().then(setSchaleState); };
    tick();
    const iv = setInterval(tick, 5000);
    return () => clearInterval(iv);
  }, [mode]);

  if (mode === undefined) {
    return <div style={{ padding: 24, color: 'var(--cth-ink-500)' }}>로딩…</div>;
  }
  if (!configured || mode !== 'god' || forcePicker) {
    return (
      <ModePicker
        cwd={cwd}
        onboarding={!configured}
        onPicked={(m) => { setModeState(m); setConfigured(true); }}
      />
    );
  }

  const sorted = [...agents].sort((a, b) => Number(b.isGod) - Number(a.isGod));

  // 재화 = claude 토큰 지표(선생님): 💎입력토큰 · 🪙비용$ (전 학생 합산).
  const totalInputTokens = sorted.reduce((s, a) => s + (a.tokensIn ?? 0), 0);
  const totalCostUsd = sorted.reduce((s, a) => s + (a.costUsd ?? 0), 0);
  const totalContextTokens = sorted.reduce((s, a) => s + (a.contextTokens ?? 0), 0);

  const reveal = async () => {
    setRevealing(true);
    await revealTerminal(1);
    setTimeout(() => setRevealing(false), 600);
  };

  return (
    // SCHALE OS 전체 셸: 세로 100% + 가로 100%
    <div style={{ height: '100%', display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>

      {/* 타이틀바 */}
      <TitleBar
        notifications={agents.filter((a) => a.status === 'waiting' || a.status === 'blocked').length}
        mail={agents.filter((a) => a.status === 'success').length}
        contextTokens={totalContextTokens}
        onBell={() => { const a = sorted.find((x) => x.status === 'waiting' || x.status === 'blocked'); if (a) setPeek({ id: a.id, title: a.character }); }}
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
          <RoomChip cwd={cwd} />
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

        {/* 우측 액션 */}
        <PixelButton variant="secondary" size="sm" onClick={reveal}>
          {revealing ? '여는 중…' : '터미널 보기'}
        </PixelButton>
        <PixelButton variant="primary" size="sm" onClick={() => setShowAdd(true)}>학생 부르기</PixelButton>
      </div>

      {/* 바디: 메인 영역 + 우측 CommandCenter */}
      <div style={{ flex: 1, display: 'flex', overflow: 'hidden' }}>

        {/* 메인 컬럼 */}
        <div style={{ flex: 1, display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>

          {/* 교실 씬 or 카드 그리드 */}
          <div style={{ flex: 1, overflow: 'auto', padding: 'var(--cth-space-4)' }}>
            {view === 'classroom' ? (
              <ClassroomView onSelect={(id, title) => setPeek({ id, title })} />
            ) : sorted.length === 0 ? (
              <p style={{ color: 'var(--cth-ink-500)' }}>학생들을 기다리는 중… (board 폴링 · MCP)</p>
            ) : (
              <div style={{ display: 'flex', flexWrap: 'wrap', gap: 'var(--cth-space-4)' }}>
                {sorted.map((a) => (
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

          {/* 학생 카드 그리드 (아리스 구현) */}
          <div style={{
            flexShrink: 0,
            borderTop: '1px solid var(--cth-cream-200)',
            background: 'var(--cth-cream-50)'
          }}>
            <StudentGrid agents={sorted} onSelect={(id, title) => setPeek({ id, title })} />
          </div>

          {/* 풋터 (아리스 구현) */}
          <div style={{
            flexShrink: 0,
            borderTop: '1px solid var(--cth-cream-200)',
            background: 'var(--cth-cream-50)'
          }}>
            <Footer
              onManage={() => setShowAdd(true)}
              onNewRequest={() => setShowChatInput((v) => !v)}
              inputTokens={totalInputTokens}
              costUsd={totalCostUsd}
              affinityLv={schaleState?.affinity_lv}
              exp={schaleState?.exp}
            />
          </div>
        </div>

        {/* 우측 Command Center */}
        <CommandCenter />
      </div>

      {/* 교실 채팅 입력 (Footer CTA 클릭 or 교실 뷰에서 항상 노출) */}
      {(view === 'classroom' || showChatInput) && <ClassroomChatInput />}

      {peek && (
        <TerminalPeekPanel
          surfaceId={peek.id}
          title={peek.title}
          onClose={() => setPeek(null)}
        />
      )}

      {showAdd && <AddAgentModal onClose={() => setShowAdd(false)} />}
    </div>
  );
}
