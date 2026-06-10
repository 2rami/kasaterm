import { useEffect, useState } from 'react';
import { useStore } from './store';
import { AgentCard } from './components/AgentCard';
import { AddAgentModal } from './components/AddAgentModal';
import { ModePicker } from './components/ModePicker';
import { ClassroomView } from './components/ClassroomView';
import { RoomChip } from './components/RoomChip';
import { PixelButton } from './components/PixelButton';
import { startBoardPolling, fetchMode, focusPane, revealTerminal } from './lib/mcp';

type ViewMode = 'classroom' | 'grid';

// 라우팅: mode 미설정/solo/?picker=1 → 시작 선택. god → 샬레 교실(기본) ↔ 그리드.
export function App() {
  const agents = useStore((s) => s.agents);
  const [mode, setModeState] = useState<string | null | undefined>(undefined);
  const [cwd, setCwd] = useState<string | null>(null);
  const [view, setView] = useState<ViewMode>('classroom');
  const [showAdd, setShowAdd] = useState(false);
  const [revealing, setRevealing] = useState(false);

  const forcePicker = new URLSearchParams(location.search).get('picker') === '1';

  useEffect(() => {
    fetchMode().then(({ mode, cwd }) => { setModeState(mode); setCwd(cwd); });
  }, []);
  useEffect(() => { if (mode === 'god') return startBoardPolling(1000); }, [mode]);

  if (mode === undefined) {
    return <div style={{ padding: 24, color: 'var(--cth-ink-500)' }}>로딩…</div>;
  }
  if (mode == null || mode === 'solo' || forcePicker) {
    return <ModePicker cwd={cwd} onPicked={(m) => setModeState(m)} />;
  }

  const sorted = [...agents].sort((a, b) => Number(b.isGod) - Number(a.isGod));

  const reveal = async () => {
    setRevealing(true);
    await revealTerminal();
    setTimeout(() => setRevealing(false), 600);
  };

  return (
    <div style={{ height: '100%', overflow: 'auto', padding: 'var(--cth-space-5)' }}>
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', marginBottom: 'var(--cth-space-5)', gap: 'var(--cth-space-4)' }}>
        <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--cth-space-4)', minWidth: 0 }}>
          <h1
            style={{
              fontFamily: 'var(--cth-font-display)',
              fontSize: 'var(--cth-text-display-lg)',
              lineHeight: 'var(--cth-lh-display-lg)',
              color: 'var(--cth-ink-900)', margin: 0, whiteSpace: 'nowrap'
            }}
          >
            샬레 교실
          </h1>
          {/* 교실 ↔ 카드 탭 */}
          <div style={{ display: 'flex' }}>
            {(['classroom', 'grid'] as ViewMode[]).map((v) => (
              <button
                key={v}
                onClick={() => setView(v)}
                style={{
                  fontFamily: 'var(--cth-font-display)', fontSize: 'var(--cth-text-display-sm)',
                  padding: '6px 12px', cursor: 'pointer', border: 'none',
                  background: view === v ? 'var(--cth-sky)' : 'var(--cth-cream-200)',
                  color: 'var(--cth-ink-900)', boxShadow: 'inset 0 0 0 1px var(--cth-ink-900)'
                }}
              >
                {v === 'classroom' ? '교실' : '카드'}
              </button>
            ))}
          </div>
          {/* 이 방(cwd) 칩 — 어느 폴더를 god 으로 다루는지 표시(엉뚱한 방 god 방지) */}
          <RoomChip cwd={cwd} />
        </div>
        <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
          {/* 빨간약: 교실이 숨긴 메인 터미널을 다시 보이게 */}
          <button
            onClick={reveal}
            title="메인 터미널 다시 보기"
            style={{
              fontFamily: 'var(--cth-font-display)', fontSize: 'var(--cth-text-display-sm)',
              padding: '7px 12px', cursor: 'pointer', border: 'none',
              background: 'var(--cth-coral)', color: 'var(--cth-cream-50)',
              boxShadow: 'inset 0 0 0 1px var(--cth-ink-900)'
            }}
          >
            {revealing ? '여는 중…' : '터미널 보기'}
          </button>
          <PixelButton variant="primary" onClick={() => setShowAdd(true)}>학생 부르기</PixelButton>
        </div>
      </div>

      {view === 'classroom' ? (
        <ClassroomView />
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

      {showAdd && <AddAgentModal onClose={() => setShowAdd(false)} />}
    </div>
  );
}
