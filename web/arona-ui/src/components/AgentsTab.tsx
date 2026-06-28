import { useStore } from '@/store';
import { resumeSession, type BackgroundAgent } from '@/lib/mcp';

// 에이전트 탭 — `claude agents` 가 보고하는, pane 밖에서 도는 background 세션
// (+현재 떠 있는 interactive 세션). 데이터/폴링은 App 이 store.backgroundAgents 로
// 채운다(3초 주기). background 행의 '이어받기'는 resumeSession → 새 pane 에서
// `claude --resume <id>` 로 그 세션을 foreground 로 승격(거느리던 학생 호출).

const STATE_STYLE: Record<string, { bg: string; label: string }> = {
  done: { bg: 'var(--cth-mint)', label: '완료' },
  idle: { bg: 'var(--cth-sky)', label: '대기' },
  blocked: { bg: 'var(--cth-coral, #e0794a)', label: '막힘' },
  running: { bg: 'var(--cth-lilac)', label: '작업중' },
};

function shortCwd(p: string): string {
  const segs = p.split('/').filter(Boolean);
  return (segs.length > 2 ? '…/' : '') + segs.slice(-2).join('/');
}

function fmtAgo(ts: number): string {
  const s = Math.max(0, Math.floor((Date.now() - ts) / 1000));
  if (s < 60) return `${s}초 전`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}분 전`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}시간 전`;
  return `${Math.floor(h / 24)}일 전`;
}

function SectionLabel({ text }: { text: string }) {
  return (
    <div style={{
      fontSize: 10, fontWeight: 700, letterSpacing: 0.5, textTransform: 'uppercase',
      color: 'var(--cth-ink-500)', opacity: 0.7, margin: '6px 2px 0',
    }}>{text}</div>
  );
}

function AgentRow({ a }: { a: BackgroundAgent }) {
  const st = STATE_STYLE[a.state] ?? STATE_STYLE[a.status] ?? {
    bg: 'var(--cth-ink-500)', label: a.state || a.status || '?',
  };
  const isBg = a.kind === 'background';
  return (
    <div style={{
      display: 'flex', alignItems: 'center', gap: 8, padding: '8px 10px', borderRadius: 10,
      background: 'var(--cth-cream-100, rgba(255,255,255,0.5))',
      boxShadow: 'inset 0 0 0 1px var(--cth-cream-200, rgba(21,41,74,0.08))',
    }}>
      <div style={{ flex: 1, minWidth: 0 }}>
        <div style={{
          fontSize: 13, fontWeight: 600, color: 'var(--cth-ink-700, #1c2b4a)',
          overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap',
        }}>{a.name || a.id}</div>
        <div style={{ fontSize: 11, color: 'var(--cth-ink-500)', marginTop: 2 }}>
          {fmtAgo(a.startedAt)} · {shortCwd(a.cwd)}
        </div>
      </div>
      <span style={{
        fontSize: 10, fontWeight: 700, color: '#fff', background: isBg ? st.bg : 'var(--cth-lilac)',
        padding: '2px 7px', borderRadius: 999, whiteSpace: 'nowrap',
      }}>{isBg ? st.label : '활성'}</span>
      {isBg && (
        <button onClick={() => void resumeSession(a.sessionId, a.cwd)} style={{
          fontSize: 11, fontWeight: 600, color: '#fff', background: 'var(--cth-sky)',
          border: 'none', borderRadius: 8, padding: '5px 10px', cursor: 'pointer', whiteSpace: 'nowrap',
        }}>이어받기</button>
      )}
    </div>
  );
}

export function AgentsTab() {
  const agents = useStore((s) => s.backgroundAgents);
  const bg = agents.filter((a) => a.kind === 'background');
  const live = agents.filter((a) => a.kind === 'interactive');

  if (!agents.length) {
    return (
      <div style={{ padding: 24, textAlign: 'center', color: 'var(--cth-ink-500)', fontSize: 13, lineHeight: 1.7 }}>
        도는 background 에이전트가 없어요.<br />
        <span style={{ opacity: 0.7, fontSize: 12 }}>{'claude --bg 로 위임하면 여기 모여요.'}</span>
      </div>
    );
  }

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 8, padding: 12, overflowY: 'auto' }}>
      {bg.length > 0 && <SectionLabel text={`background · ${bg.length}`} />}
      {bg.map((a) => <AgentRow key={a.sessionId} a={a} />)}
      {live.length > 0 && <SectionLabel text={`interactive · ${live.length}`} />}
      {live.map((a) => <AgentRow key={a.sessionId} a={a} />)}
    </div>
  );
}
