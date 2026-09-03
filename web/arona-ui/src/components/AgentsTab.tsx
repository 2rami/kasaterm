import { useStore } from '@/store';
import { resumeSession, killBackgroundAgent, type BackgroundAgent } from '@/lib/mcp';
import { X } from 'lucide-react';

// claude agents (pane 밖에서 도는 daemon 세션) 한 행 — '에이전트' 탭이 쓴다.
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
      <button type="button" onClick={() => onView?.(a)} title="대화 보기" style={{ flex: 1, minWidth: 0, cursor: onView ? 'pointer' : 'default', border: 'none', background: 'transparent', padding: 0, textAlign: 'left' }}>
        <div style={{
          fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: 600, color: 'var(--cth-ink-900, #1c2b4a)',
          overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap',
        }}>{a.name || a.id}</div>
        <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 10, color: 'var(--cth-ink-500)', marginTop: 1 }}>
          {fmtAgo(a.startedAt)} · {shortCwd(a.cwd)}
        </div>
      </button>
      {isBg && (
        // 돌아갈 pane 이 살아있나(parentSurface) vs 터미널과 분리돼 떠도나(orphan). 터미널이
        // 꺼지거나 다른 kasaterm 이면 parentSurface 가 비어 '독립' — shim 도 dangling 상태.
        <span title={a.parentSurface
          ? `이 터미널의 pane ${a.parentSurface} 에서 넘어옴 — 이어받기 가능`
          : '터미널과 분리돼 도는 세션 — 새 pane 으로만 이어받기'}
          style={{
            display: 'inline-flex', alignItems: 'center', gap: 3, fontFamily: 'var(--cth-font-ui)',
            fontSize: 9, fontWeight: 700, whiteSpace: 'nowrap', padding: '2px 6px', borderRadius: 999,
            color: a.parentSurface ? 'var(--cth-mint-text-bg)' : 'var(--cth-ink-500)',
            background: a.parentSurface ? 'color-mix(in srgb, var(--cth-mint) 16%, var(--cth-cream-50))' : 'var(--cth-cream-200)',
          }}>
          <span style={{ width: 5, height: 5, borderRadius: 999, flexShrink: 0, background: a.parentSurface ? 'var(--cth-mint)' : 'var(--cth-ink-300)' }} />
          {a.parentSurface ? `연결 ${a.parentSurface}` : '독립'}
        </span>
      )}
      <span style={{
        fontFamily: 'var(--cth-font-ui)', fontSize: 9, fontWeight: 700, color: 'var(--cth-on-color)',
        background: isBg ? st.bg : 'var(--cth-lilac)', padding: '2px 6px', borderRadius: 999, whiteSpace: 'nowrap',
      }}>{isBg ? st.label : '활성'}</span>
      {isBg && (
        // attach 는 daemon 이 부여한 short-id(a.id, 8자)를 받는다 — full sessionId(UUID)를
        // 넘기면 claude 가 'attach' 를 프롬프트로 먹어 재진입 실패(2.1.197 실측). --resume 은
        // background 세션에 아예 불가("running as background agent, use claude agents to attach").
        <button onClick={() => void resumeSession(a.id, a.cwd, false, true)} style={{
          fontFamily: 'var(--cth-font-ui)', fontSize: 10, fontWeight: 600, color: 'var(--cth-on-sky)', background: 'var(--cth-sky)',
          border: 'none', borderRadius: 7, padding: '4px 9px', cursor: 'pointer', whiteSpace: 'nowrap',
        }}>이어받기</button>
      )}
      {isBg && (
        <button
          onClick={() => { if (confirm(`'${a.name || a.id}' 세션을 종료할까요?`)) void killBackgroundAgent(a.pid); }}
          title="세션 종료(정리)"
          aria-label="세션 종료"
          style={{
            fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: 700, lineHeight: 1, color: 'var(--cth-ink-500)',
            background: 'transparent', border: '1px solid var(--cth-cream-200, rgba(21,41,74,0.14))',
            borderRadius: 7, padding: '4px 7px', cursor: 'pointer', whiteSpace: 'nowrap',
          }}
        ><X size={13} aria-hidden="true" /></button>
      )}
    </div>
  );
}

// '에이전트' 탭 본문 — pane 밖에서 도는 daemon claude 세션 목록(거노: 보드 현황에서
// 분리해 별도 탭으로). 데이터는 App 이 store.backgroundAgents 로 채운다(3s 폴링).
// 모모이처럼 다른 방·detach 된 세션도 cwd 무관 전체가 여기 모인다(claude agents --all).
export function AgentsPanel({ onOpenBackground }: { onOpenBackground?: (a: BackgroundAgent) => void }) {
  const backgroundAgents = useStore((s) => s.backgroundAgents);
  return (
    <div style={{ flex: 1, display: 'flex', flexDirection: 'column', minHeight: 0, background: 'var(--cth-cream-100)' }}>
      <div style={{ flex: 1, overflowY: 'auto', padding: 10, minHeight: 0 }}>
        {backgroundAgents.length === 0 ? (
          <div style={{ color: 'var(--cth-ink-300)', fontFamily: 'var(--cth-font-ui)', fontSize: 11, textAlign: 'center', padding: '28px 0', lineHeight: 1.7 }}>
            pane 밖에서 도는<br />백그라운드 세션이 없어요
          </div>
        ) : (
          <div style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
            {backgroundAgents.map((a) => <AgentRow key={a.sessionId} a={a} onView={onOpenBackground} />)}
          </div>
        )}
      </div>
    </div>
  );
}
