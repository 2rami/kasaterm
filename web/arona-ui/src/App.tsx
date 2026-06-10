import { useEffect, useState } from 'react';
import { useStore } from './store';
import { AgentCard } from './components/AgentCard';
import { AddAgentModal } from './components/AddAgentModal';
import { ModePicker } from './components/ModePicker';
import { ClassroomView } from './components/ClassroomView';
import { ClassroomChatInput } from './components/ClassroomChatInput';
import { CommandCenter } from './components/CommandCenter';
import { StudentGrid } from './components/StudentGrid';
import { Footer } from './components/Footer';
import { TerminalPeekPanel } from './components/TerminalPeekPanel';
import { RoomChip } from './components/RoomChip';
import { PixelButton } from './components/PixelButton';
import { startBoardPolling, fetchMode, focusPane, revealTerminal } from './lib/mcp';

type ViewMode = 'classroom' | 'grid';

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

  const forcePicker = new URLSearchParams(location.search).get('picker') === '1';

  useEffect(() => {
    fetchMode().then(({ mode, cwd, configured }) => {
      setModeState(mode); setCwd(cwd); setConfigured(configured);
    });
  }, []);
  useEffect(() => { if (mode === 'god') return startBoardPolling(1000); }, [mode]);

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

  const reveal = async () => {
    setRevealing(true);
    await revealTerminal(1);
    setTimeout(() => setRevealing(false), 600);
  };

  return (
    // SCHALE OS 전체 셸: 세로 100% + 가로 100%
    <div style={{ height: '100%', display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>

      {/* 헤더 */}
      <div style={{
        display: 'flex', alignItems: 'center', gap: 8,
        padding: '8px 12px',
        borderBottom: '2px solid var(--cth-ink-900)',
        background: 'var(--cth-cream-100)',
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
        <div style={{ display: 'flex', marginLeft: 8 }}>
          {(['classroom', 'grid'] as ViewMode[]).map((v) => (
            <button
              key={v}
              onClick={() => setView(v)}
              style={{
                fontFamily: 'var(--cth-font-display)', fontSize: 'var(--cth-text-display-sm)',
                padding: '5px 12px', cursor: 'pointer', border: 'none',
                background: view === v ? 'var(--cth-sky)' : 'transparent',
                color: 'var(--cth-ink-900)',
                boxShadow: view === v ? 'inset 0 0 0 1px var(--cth-ink-900)' : 'none'
              }}
            >
              {v === 'classroom' ? '교실' : '카드'}
            </button>
          ))}
        </div>

        <div style={{ flex: 1 }} />

        {/* 우측 액션 */}
        <button
          onClick={reveal}
          title="메인 터미널 다시 보기"
          style={{
            fontFamily: 'var(--cth-font-display)', fontSize: 'var(--cth-text-display-sm)',
            padding: '5px 10px', cursor: 'pointer', border: 'none',
            background: 'var(--cth-coral)', color: 'var(--cth-cream-50)',
            boxShadow: 'inset 0 0 0 1px var(--cth-ink-900)'
          }}
        >
          {revealing ? '여는 중…' : '터미널 보기'}
        </button>
        <PixelButton variant="primary" onClick={() => setShowAdd(true)}>학생 부르기</PixelButton>
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
            borderTop: '2px solid var(--cth-ink-900)',
            background: 'var(--cth-cream-100)'
          }}>
            <StudentGrid agents={sorted} onSelect={(id, title) => setPeek({ id, title })} />
          </div>

          {/* 풋터 (아리스 구현) */}
          <div style={{
            flexShrink: 0,
            borderTop: '1px solid var(--cth-ink-900)',
            background: 'var(--cth-cream-50)'
          }}>
            <Footer
              onManage={() => setShowAdd(true)}
              onNewRequest={() => setShowChatInput((v) => !v)}
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
