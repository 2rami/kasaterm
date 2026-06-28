import { resumeSession, type BackgroundAgent } from '@/lib/mcp';

// claude agents (pane 밖에서 도는 daemon 세션) 한 행 — Board 의 '백그라운드' 섹션이 쓴다.
// 데이터/폴링은 App 이 store.backgroundAgents 로 채운다(3s). '이어받기'→resumeSession
// 으로 그 세션을 새 pane 의 foreground 로 승격(거느리던 학생 호출).

const STATE_STYLE: Record<string, { bg: string; label: string }> = {
  done: { bg: 'var(--cth-mint)', label: '완료' },
  idle: { bg: 'var(--cth-sky)', label: '대기' },
  blocked: { bg: 'var(--cth-coral, #e0794a)', label: '막힘' },
  running: { bg: 'var(--cth-lilac)', label: '작업중' },
  working: { bg: 'var(--cth-lilac)', label: '작업중' },
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

export function AgentRow({ a, onView }: { a: BackgroundAgent; onView?: (a: BackgroundAgent) => void }) {
  const st = STATE_STYLE[a.state] ?? STATE_STYLE[a.status] ?? {
    bg: 'var(--cth-ink-500)', label: a.state || a.status || '?',
  };
  const isBg = a.kind === 'background';
  return (
    <div style={{
      display: 'flex', alignItems: 'center', gap: 8, padding: '7px 9px', borderRadius: 9,
      background: 'var(--cth-cream-100, rgba(255,255,255,0.5))',
      boxShadow: 'inset 0 0 0 1px var(--cth-cream-200, rgba(21,41,74,0.08))',
    }}>
      <div onClick={() => onView?.(a)} title="대화 보기" style={{ flex: 1, minWidth: 0, cursor: onView ? 'pointer' : 'default' }}>
        <div style={{
          fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: 600, color: 'var(--cth-ink-900, #1c2b4a)',
          overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap',
        }}>{a.name || a.id}</div>
        <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 10, color: 'var(--cth-ink-500)', marginTop: 1 }}>
          {fmtAgo(a.startedAt)} · {shortCwd(a.cwd)}
        </div>
      </div>
      <span style={{
        fontFamily: 'var(--cth-font-ui)', fontSize: 9, fontWeight: 700, color: '#fff',
        background: isBg ? st.bg : 'var(--cth-lilac)', padding: '2px 6px', borderRadius: 999, whiteSpace: 'nowrap',
      }}>{isBg ? st.label : '활성'}</span>
      {isBg && (
        <button onClick={() => void resumeSession(a.sessionId, a.cwd)} style={{
          fontFamily: 'var(--cth-font-ui)', fontSize: 10, fontWeight: 600, color: '#fff', background: 'var(--cth-sky)',
          border: 'none', borderRadius: 7, padding: '4px 9px', cursor: 'pointer', whiteSpace: 'nowrap',
        }}>이어받기</button>
      )}
    </div>
  );
}
