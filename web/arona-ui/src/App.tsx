import { useEffect } from 'react';
import { useStore } from './store';
import { AgentCard } from './components/AgentCard';
import { startBoardPolling } from './lib/mcp';

// 1차 화면: board 데이터로 그리는 AgentCard 그리드. god(아로나)이 먼저, 워커들이
// 뒤따른다. 샬레 교실 맵(pixi)·spawn·캐릭터 초상은 후속 단계에서 얹는다.
export function App() {
  const agents = useStore((s) => s.agents);

  useEffect(() => startBoardPolling(1000), []);

  const sorted = [...agents].sort((a, b) => Number(b.isGod) - Number(a.isGod));

  return (
    <div style={{ height: '100%', overflow: 'auto', padding: 'var(--cth-space-5)' }}>
      <h1
        style={{
          fontFamily: 'var(--cth-font-display)',
          fontSize: 'var(--cth-text-display-lg)',
          lineHeight: 'var(--cth-lh-display-lg)',
          color: 'var(--cth-ink-900)',
          margin: '0 0 var(--cth-space-5)'
        }}
      >
        샬레 교실
      </h1>
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
            />
          ))}
        </div>
      )}
    </div>
  );
}
