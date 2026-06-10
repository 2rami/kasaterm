import { useEffect, useState } from 'react';
import { useStore } from './store';
import { AgentCard } from './components/AgentCard';
import { AddAgentModal } from './components/AddAgentModal';
import { ModePicker } from './components/ModePicker';
import { PixelButton } from './components/PixelButton';
import { startBoardPolling, fetchMode, focusPane } from './lib/mcp';

// 라우팅: mode 미설정(또는 ?picker=1) → 시작 선택 화면. god → 샬레 교실(AgentCard
// 그리드 + 학생 부르기). solo 면 arona-ui 가 뜰 일이 없지만(터미널이 뜸), 안전하게
// picker 로 보낸다. 카드 클릭 → 해당 pane 포커스(MCP /focus).
export function App() {
  const agents = useStore((s) => s.agents);
  const [mode, setModeState] = useState<string | null | undefined>(undefined); // undefined=로딩
  const [showAdd, setShowAdd] = useState(false);

  const forcePicker = new URLSearchParams(location.search).get('picker') === '1';

  useEffect(() => {
    fetchMode().then(setModeState);
  }, []);

  useEffect(() => {
    if (mode === 'god') return startBoardPolling(1000);
  }, [mode]);

  if (mode === undefined) {
    return <div style={{ padding: 24, color: 'var(--cth-ink-500)' }}>로딩…</div>;
  }

  if (mode == null || mode === 'solo' || forcePicker) {
    return <ModePicker onPicked={(m) => setModeState(m)} />;
  }

  const sorted = [...agents].sort((a, b) => Number(b.isGod) - Number(a.isGod));

  return (
    <div style={{ height: '100%', overflow: 'auto', padding: 'var(--cth-space-5)' }}>
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', marginBottom: 'var(--cth-space-5)' }}>
        <h1
          style={{
            fontFamily: 'var(--cth-font-display)',
            fontSize: 'var(--cth-text-display-lg)',
            lineHeight: 'var(--cth-lh-display-lg)',
            color: 'var(--cth-ink-900)', margin: 0
          }}
        >
          샬레 교실
        </h1>
        <PixelButton variant="primary" onClick={() => setShowAdd(true)}>학생 부르기</PixelButton>
      </div>

      {sorted.length === 0 ? (
        <p style={{ color: 'var(--cth-ink-500)' }}>
          학생들을 기다리는 중… (board 폴링 중 · MCP 8765)
        </p>
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
