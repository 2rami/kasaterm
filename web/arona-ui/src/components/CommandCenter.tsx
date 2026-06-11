import { useState } from 'react';
import { useStore } from '@/store';
import { MomoTalk } from './MomoTalk';

type CenterTab = 'tasks' | 'momotalk' | 'council' | 'schedule';

const TAB_LABELS: Record<CenterTab, string> = {
  tasks: '업무',
  momotalk: '모모톡',
  council: '의뢰',
  schedule: '스케줄',
};

// SCHALE OS 우측 Command Center 패널.
export function CommandCenter() {
  const [tab, setTab] = useState<CenterTab>('tasks');
  const agents = useStore((s) => s.agents);

  const workers = agents.filter((a) => !a.isGod);

  return (
    <div style={{
      width: 300,
      flexShrink: 0,
      height: '100%',
      display: 'flex',
      flexDirection: 'column',
      borderLeft: '1px solid var(--cth-cream-200)',
      background: 'var(--cth-cream-50)',
      overflow: 'hidden'
    }}>
      {/* 헤더 */}
      <div style={{
        padding: '12px 14px 10px',
        borderBottom: '1px solid var(--cth-cream-200)',
        display: 'flex',
        alignItems: 'center',
        gap: 8
      }}>
        <div style={{ flex: 1, minWidth: 0 }}>
          <div style={{
            fontFamily: 'var(--cth-font-display)',
            fontSize: 'var(--cth-text-display-sm)',
            color: 'var(--cth-ink-500)',
            lineHeight: 1
          }}>선생님</div>
          <div style={{
            fontFamily: 'var(--cth-font-display)',
            fontSize: 'var(--cth-text-display-md)',
            color: 'var(--cth-ink-900)', fontWeight: 700,
            lineHeight: 1.2
          }}>Command Center</div>
        </div>
        <div style={{
          padding: '4px 10px',
          background: 'var(--cth-sky)',
          color: '#fff',
          fontFamily: 'var(--cth-font-ui)',
          fontSize: 11, fontWeight: 700, letterSpacing: 0.5,
          borderRadius: 6
        }}>SCHALE</div>
      </div>

      {/* 탭 */}
      <div className="cth-tabbar" style={{
        display: 'flex',
        borderBottom: '1px solid var(--cth-cream-200)',
        overflowX: 'auto', gap: 2, padding: '5px 6px'
      }}>
        {(Object.keys(TAB_LABELS) as CenterTab[]).map((t) => (
          <button
            key={t}
            onClick={() => setTab(t)}
            style={{
              flexShrink: 0,
              padding: '6px 12px',
              fontFamily: 'var(--cth-font-ui)',
              fontSize: 12, fontWeight: 600,
              border: 'none', borderRadius: 7,
              background: tab === t ? 'var(--cth-sky)' : 'transparent',
              color: tab === t ? '#fff' : 'var(--cth-ink-500)',
              cursor: 'pointer',
              whiteSpace: 'nowrap',
              transition: 'background 120ms ease, color 120ms ease'
            }}
          >
            {TAB_LABELS[t]}
          </button>
        ))}
      </div>

      {/* 본문 — 대시보드 탭 제거(대화는 학생 클릭 시 우측 인라인, 이벤트는 기록 탭) */}
      {tab === 'momotalk' ? (
        /* 모모톡 — 선생님·아로나·학생 전체 소통 단톡방(messages.jsonl 단일 피드) */
        <MomoTalk />
      ) : tab === 'council' ? (
        /* 의뢰 대기열 — board working 워커들 */
        <div style={{ flex: 1, overflowY: 'auto', padding: 10 }}>
          <div style={{ fontFamily: 'var(--cth-font-display)', fontSize: 10, color: 'var(--cth-ink-500)', marginBottom: 8 }}>
            의뢰 대기열 {workers.length} / 10
          </div>
          {workers.length === 0 ? (
            <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 11, color: 'var(--cth-ink-300)' }}>대기 중인 의뢰 없음</span>
          ) : workers.map((a, i) => (
            <div key={a.id} style={{
              display: 'flex', alignItems: 'center', gap: 6,
              padding: '5px 0',
              borderBottom: '1px solid var(--cth-cream-300)'
            }}>
              <span style={{ fontFamily: 'var(--cth-font-display)', fontSize: 10, color: 'var(--cth-ink-300)', width: 16 }}>{i + 1}</span>
              <span style={{ flex: 1, fontFamily: 'var(--cth-font-ui)', fontSize: 11, color: 'var(--cth-ink-900)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                {a.project || a.name}
              </span>
              <span style={{
                padding: '2px 7px', fontSize: 10, fontWeight: 600, borderRadius: 5,
                fontFamily: 'var(--cth-font-ui)', color: '#fff',
                background: a.status === 'working' ? 'var(--cth-mint)' : a.status === 'waiting' ? 'var(--cth-sky)' : 'var(--cth-ink-300)',
              }}>{a.status}</span>
            </div>
          ))}
        </div>
      ) : tab === 'tasks' ? (
        /* 업무 — 학생별 현재 작업(현재 tool + 서브에이전트) */
        <div style={{ flex: 1, overflowY: 'auto', padding: 10 }}>
          {agents.length === 0 ? (
            <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 11, color: 'var(--cth-ink-300)' }}>학생 없음</span>
          ) : agents.map((a) => (
            <div key={a.id} style={{ display: 'flex', alignItems: 'center', gap: 8, padding: '7px 0', borderBottom: '1px solid var(--cth-cream-200)' }}>
              <span style={{ width: 8, height: 8, borderRadius: 999, flexShrink: 0, background: a.status === 'working' ? 'var(--cth-mint)' : a.status === 'waiting' || a.status === 'blocked' ? 'var(--cth-coral)' : 'var(--cth-ink-300)' }} />
              <div style={{ flex: 1, minWidth: 0 }}>
                <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: 600, color: 'var(--cth-ink-900)' }}>{a.character}</div>
                <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 11, color: 'var(--cth-ink-500)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{a.project || '대기 중'}</div>
              </div>
              {a.currentTool && (
                <span style={{ fontFamily: 'var(--cth-font-mono)', fontSize: 10, fontWeight: 700, color: 'var(--cth-sky)', background: 'color-mix(in srgb, var(--cth-sky) 12%, #fff)', padding: '2px 7px', borderRadius: 6 }}>{a.currentTool}</span>
              )}
              {!!a.subagents?.length && (
                <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 10, fontWeight: 700, color: 'var(--cth-lilac)', background: 'color-mix(in srgb, var(--cth-lilac) 14%, #fff)', padding: '2px 7px', borderRadius: 6 }}>서브 {a.subagents.length}</span>
              )}
            </div>
          ))}
        </div>
      ) : (
        /* 스케줄 — 컨텍스트 예산 관리. 학생을 컨텍스트 사용량(상태바 %) 높은 순으로
           정렬해 누가 곧 한계라 compact/마무리가 필요한지 한눈에. 75%↑ 주의·90%↑ 임박. */
        <div style={{ flex: 1, overflowY: 'auto', padding: 10 }}>
          <div style={{ fontFamily: 'var(--cth-font-display)', fontSize: 10, color: 'var(--cth-ink-500)', marginBottom: 8 }}>
            컨텍스트 예산 — 임박 순
          </div>
          {agents.length === 0 ? (
            <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 11, color: 'var(--cth-ink-300)' }}>학생 없음</span>
          ) : [...agents].sort((a, b) => (b.contextPct ?? 0) - (a.contextPct ?? 0)).map((a) => {
            const pct = Math.min(100, Math.round(a.contextPct ?? 0));
            const level = pct >= 90 ? { c: 'var(--cth-coral)', t: 'compact 임박' }
              : pct >= 75 ? { c: 'var(--cth-lemon)', t: '주의' }
              : { c: 'var(--cth-mint)', t: '여유' };
            return (
              <div key={a.id} style={{ padding: '7px 0', borderBottom: '1px solid var(--cth-cream-200)' }}>
                <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 4, gap: 6 }}>
                  <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: 600, color: 'var(--cth-ink-900)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{a.character}</span>
                  <span style={{ display: 'flex', alignItems: 'center', gap: 6, flexShrink: 0 }}>
                    <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 9, fontWeight: 700, color: '#fff', background: level.c, padding: '1px 6px', borderRadius: 5 }}>{level.t}</span>
                    <span style={{ fontFamily: 'var(--cth-font-mono)', fontSize: 11, fontWeight: 700, color: 'var(--cth-ink-700)' }}>{pct}%</span>
                  </span>
                </div>
                <div style={{ height: 6, borderRadius: 999, background: 'var(--cth-cream-200)', overflow: 'hidden' }}>
                  <div style={{ height: '100%', width: `${pct}%`, borderRadius: 999, background: level.c, transition: 'width 0.4s ease' }} />
                </div>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
