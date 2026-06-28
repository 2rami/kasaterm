import { useEffect, useState } from 'react';
import { useStore, isAwaitingTeacher, isUnconfirmed } from '@/store';
import { AgentRow } from './AgentsTab';
import { SpritePortrait } from './SpritePortrait';
import { assignSprites } from '@/lib/sprites';
import { fetchPaneTasks, type PaneTask } from '@/lib/mcp';
import { isBuildCmd, BUILD_COLOR, GearIcon, SpinIcon, ForkIcon } from './activity';
import { PaneToolTimeline } from './PaneToolTimeline';

const taskRank = (s: string) => (s === 'in_progress' ? 0 : s === 'completed' ? 2 : 1);

// 확인 대기 알림 종 — 이모지 금지 SVG.
function BellGlyph() {
  return (
    <svg width="13" height="13" viewBox="0 0 16 16" style={{ display: 'block', flexShrink: 0 }}>
      <path d="M8 1.6a1 1 0 0 1 1 1v.5a4 4 0 0 1 3 3.87V9l1.2 2.2a.6.6 0 0 1-.53.9H3.33a.6.6 0 0 1-.53-.9L4 9V6.97a4 4 0 0 1 3-3.87v-.5a1 1 0 0 1 1-1Z" fill="currentColor" />
      <path d="M6.4 13.2a1.7 1.7 0 0 0 3.2 0" stroke="currentColor" strokeWidth="1.1" fill="none" strokeLinecap="round" />
    </svg>
  );
}

const SectionLabel = ({ children }: { children: string }) => (
  <div style={{
    fontFamily: 'var(--cth-font-ui)', fontSize: 10, fontWeight: 700, color: 'var(--cth-ink-500)',
    textTransform: 'uppercase', letterSpacing: 0.5, margin: '2px 4px 8px',
  }}>{children}</div>
);

// board = 모든 pane 작업 현황·상세(업무 흡수) + pane 간 tell 소통 피드. 모모톡·inbox 대체(거노).
// 빨강 '확인 필요'는 waiting_for(AskUserQuestion·권한) 있는 것만 — isAwaitingTeacher 가 그 판정.
export function BoardPanel({ onPickStudent }: { onPickStudent?: (id: string, title: string) => void }) {
  const agents = useStore((s) => s.agents);
  const acked = useStore((s) => s.acked);
  const backgroundAgents = useStore((s) => s.backgroundAgents);
  const sprited = assignSprites(agents);
  const spriteOf = new Map(sprited.map((a) => [a.id, a.spriteChar || a.character]));
  const [paneTasks, setPaneTasks] = useState<Record<string, PaneTask[]>>({});
  const [expandedPane, setExpandedPane] = useState<string | null>(null);

  useEffect(() => {
    let stop = false;
    const tick = () => {
      void fetchPaneTasks().then((ts) => {
        if (stop) return;
        const by: Record<string, PaneTask[]> = {};
        for (const t of ts) (by[t.pane] ??= []).push(t);
        setPaneTasks(by);
      });
    };
    tick();
    const iv = setInterval(tick, 2500);
    return () => { stop = true; clearInterval(iv); };
  }, []);

  return (
    <div style={{ flex: 1, display: 'flex', flexDirection: 'column', minHeight: 0, background: 'var(--cth-cream-100)' }}>
      <div style={{ flex: 1, overflowY: 'auto', padding: 10, minHeight: 0 }}>
        {/* 확인 대기 — waiting_for(AskUserQuestion·권한) 있는 학생만(거노: 빨강 남발 방지) */}
        {(() => {
          const awaiting = agents.filter(isAwaitingTeacher);
          if (!awaiting.length) return null;
          const pending = awaiting.filter((a) => isUnconfirmed(a, acked)).length;
          return (
            <div style={{
              marginBottom: 12, padding: 10, borderRadius: 10,
              background: pending ? 'color-mix(in srgb, var(--cth-coral) 10%, var(--cth-cream-50))' : 'var(--cth-cream-100)',
              border: `1px solid ${pending ? 'var(--cth-coral)' : 'var(--cth-cream-200)'}`,
            }}>
              <div style={{ display: 'flex', alignItems: 'center', gap: 6, marginBottom: 8, fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: 800, color: pending ? 'var(--cth-coral)' : 'var(--cth-ink-500)' }}>
                {pending ? <BellGlyph /> : null}
                {pending ? `확인 안 한 게 ${pending}건 있어요, 선생님` : '확인 대기 (모두 확인함)'}
              </div>
              <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                {awaiting.map((a) => {
                  const un = isUnconfirmed(a, acked);
                  return (
                    <button key={a.id} onClick={() => onPickStudent?.(a.id, a.character)} style={{
                      display: 'flex', alignItems: 'center', gap: 8, padding: '6px 8px', borderRadius: 8,
                      border: 'none', cursor: 'pointer', textAlign: 'left',
                      background: un ? 'var(--cth-coral)' : '#fff', color: un ? '#fff' : 'var(--cth-ink-900)',
                      fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: un ? 800 : 600,
                      boxShadow: '0 1px 3px rgba(21,41,74,0.1)',
                    }}>
                      <span style={{ width: 7, height: 7, borderRadius: 999, flexShrink: 0, background: un ? '#fff' : 'var(--cth-coral)' }} />
                      <span style={{ flex: 1, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{a.name}</span>
                      <span style={{ fontSize: 11, fontWeight: 600, opacity: 0.85 }}>{un ? '확인 필요' : '확인함'}</span>
                    </button>
                  );
                })}
              </div>
            </div>
          );
        })()}

        {/* 현황 — 학생별 작업 상세(status·도구·태스크·서브에이전트·도구 흐름). 업무 탭 흡수. */}
        <SectionLabel>현황</SectionLabel>
        {agents.length === 0 ? (
          <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 11, color: 'var(--cth-ink-300)' }}>학생 없음</span>
        ) : agents.map((a) => {
          const building = a.status === 'working' && isBuildCmd(a.action);
          const awaiting = isAwaitingTeacher(a);
          const busy = a.status === 'working' || a.status === 'thinking';
          return (
            <div key={a.id} style={{ padding: '7px 0', borderBottom: '1px solid var(--cth-cream-200)' }}>
              {/* 헤더 클릭 → 그 학생 대화 탭으로(거노). */}
              <div onClick={() => onPickStudent?.(a.id, a.name)} title="대화 열기" style={{ display: 'flex', alignItems: 'center', gap: 8, cursor: 'pointer' }}>
                <div style={{ width: 26, height: 26, borderRadius: 7, overflow: 'hidden', flexShrink: 0, background: 'var(--cth-cream-100)', display: 'flex', alignItems: 'flex-end', justifyContent: 'center' }}>
                  <SpritePortrait character={spriteOf.get(a.id) || a.character} scale={1.2} bust />
                </div>
                {/* 작업중=sky(펄스) / 확인필요(waiting_for)=coral(깜빡) / 그 외=초록(정적) */}
                <span style={{ width: 8, height: 8, borderRadius: 999, flexShrink: 0, background: awaiting ? 'var(--cth-coral)' : busy ? 'var(--cth-sky)' : 'var(--cth-status-success)', animation: busy ? 'cth-dot-pulse 1.3s ease-in-out infinite' : awaiting ? 'cth-blink 0.9s ease-in-out infinite' : undefined }} />
                <div style={{ flex: 1, minWidth: 0 }}>
                  <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: 600, color: 'var(--cth-ink-900)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{a.name}</div>
                  <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 11, color: 'var(--cth-ink-500)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{a.project || '대기 중'}</div>
                </div>
                {building ? (
                  <span style={{ display: 'inline-flex', alignItems: 'center', gap: 3, fontFamily: 'var(--cth-font-ui)', fontSize: 10, fontWeight: 700, color: BUILD_COLOR, background: 'color-mix(in srgb, #E5923A 14%, #fff)', padding: '2px 7px', borderRadius: 6 }}><GearIcon size={11} />빌드 중</span>
                ) : a.currentTool ? (
                  <span style={{ flexShrink: 0, maxWidth: 130, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', fontFamily: 'var(--cth-font-mono)', fontSize: 10, fontWeight: 700, color: 'var(--cth-sky)', background: 'color-mix(in srgb, var(--cth-sky) 12%, #fff)', padding: '2px 7px', borderRadius: 6 }}>{a.currentTool}</span>
                ) : null}
                {!!a.background?.length && (
                  <span title={a.background.join('\n')} style={{ display: 'inline-flex', alignItems: 'center', gap: 3, fontFamily: 'var(--cth-font-ui)', fontSize: 10, fontWeight: 700, color: BUILD_COLOR, background: 'color-mix(in srgb, #E5923A 14%, #fff)', padding: '2px 7px', borderRadius: 6 }}><SpinIcon size={10} />bg {a.background.length}</span>
                )}
                {!!a.subagents?.length && (
                  <span title={a.subagents.join('\n')} style={{ display: 'inline-flex', alignItems: 'center', gap: 3, fontFamily: 'var(--cth-font-ui)', fontSize: 10, fontWeight: 700, color: 'var(--cth-lilac)', background: 'color-mix(in srgb, var(--cth-lilac) 14%, #fff)', padding: '2px 7px', borderRadius: 6 }}><ForkIcon size={10} />{a.subagents.length}</span>
                )}
              </div>
              {/* claude TaskCreate 태스크 — 진행중(◉) 먼저. */}
              {!!paneTasks[a.id]?.length && (
                <div style={{ marginLeft: 16, marginTop: 5, display: 'flex', flexDirection: 'column', gap: 2 }}>
                  {[...paneTasks[a.id]]
                    .sort((x, y) => taskRank(x.status) - taskRank(y.status))
                    .map((t) => {
                      const done = t.status === 'completed';
                      const active = t.status === 'in_progress';
                      return (
                        <div key={t.id} style={{ display: 'flex', alignItems: 'center', gap: 5, fontFamily: 'var(--cth-font-ui)', fontSize: 11, fontWeight: active ? 700 : 500, color: done ? 'var(--cth-ink-300)' : active ? 'var(--cth-mint)' : 'var(--cth-ink-700)' }}>
                          <span style={{ flexShrink: 0, width: 10, textAlign: 'center' }}>{done ? '✓' : active ? '◉' : '○'}</span>
                          <span style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', textDecoration: done ? 'line-through' : 'none' }}>{t.subject}</span>
                        </div>
                      );
                    })}
                </div>
              )}
              {/* 백그라운드/서브에이전트 이름 + 완료 흔적 */}
              {(!!a.background?.length || !!a.subagents?.length || !!a.subagentsDone?.length) && (
                <div style={{ marginLeft: 16, marginTop: 3, display: 'flex', flexDirection: 'column', gap: 1 }}>
                  {a.background?.map((b, i) => (
                    <div key={'b' + i} style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 10, color: BUILD_COLOR, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>⟳ {b}</div>
                  ))}
                  {a.subagents?.map((s, i) => (
                    <div key={'s' + i} style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 10, color: 'var(--cth-lilac)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>↳ {s}</div>
                  ))}
                  {a.subagentsDone?.map((s, i) => (
                    <div key={'d' + i} style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 10, color: 'var(--cth-ink-300)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', textDecoration: 'line-through' }}>✓ {s}</div>
                  ))}
                </div>
              )}
              {/* 도구 활동(오래된→최근) — 접힘: 칩 요약, 펼침: PaneToolTimeline 카드. */}
              {!!a.recentTools?.length && (
                <div
                  onClick={() => setExpandedPane((p) => (p === a.id ? null : a.id))}
                  title={expandedPane === a.id ? '접기' : '도구·서브에이전트 상세 펼치기'}
                  style={{ marginLeft: 16, marginTop: 4, display: 'flex', flexWrap: 'wrap', gap: 3, alignItems: 'center', cursor: 'pointer' }}
                >
                  <svg width="9" height="9" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3" strokeLinecap="round" strokeLinejoin="round" style={{ color: 'var(--cth-ink-300)', flexShrink: 0, transform: expandedPane === a.id ? 'rotate(90deg)' : 'none', transition: 'transform .1s' }}><polyline points="9 6 15 12 9 18" /></svg>
                  {expandedPane === a.id ? (
                    <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 10, fontWeight: 700, color: 'var(--cth-ink-500)' }}>도구 상세 · 접기</span>
                  ) : [...a.recentTools].reverse().map((t, i, arr) => (
                    <span key={i} style={{ display: 'inline-flex', alignItems: 'center', gap: 3 }}>
                      <span title={t} style={{ fontFamily: 'var(--cth-font-mono)', fontSize: 9, color: 'var(--cth-ink-500)', background: 'var(--cth-cream-100)', padding: '1px 5px', borderRadius: 5, whiteSpace: 'nowrap', maxWidth: 200, overflow: 'hidden', textOverflow: 'ellipsis' }}>{t}</span>
                      {i < arr.length - 1 && <span style={{ fontSize: 8, color: 'var(--cth-ink-300)' }}>→</span>}
                    </span>
                  ))}
                </div>
              )}
              {expandedPane === a.id && <PaneToolTimeline paneId={a.id} />}
            </div>
          );
        })}

        {/* 백그라운드 에이전트 — claude agents (pane 밖 daemon 세션). 이어받기로 foreground 승격. */}
        {backgroundAgents.length > 0 && (
          <div style={{ marginTop: 16 }}>
            <SectionLabel>백그라운드 에이전트</SectionLabel>
            <div style={{ display: 'flex', flexDirection: 'column', gap: 6, marginTop: 6 }}>
              {backgroundAgents.map((a) => <AgentRow key={a.sessionId} a={a} />)}
            </div>
          </div>
        )}

        {/* 소통 — tell 로그 연결 후 채워진다(모모이 버그 후 Rust 로그 작업). */}
        <div style={{ marginTop: 16 }}>
          <SectionLabel>소통</SectionLabel>
          <div style={{ color: 'var(--cth-ink-300)', fontFamily: 'var(--cth-font-ui)', fontSize: 11, textAlign: 'center', padding: '12px 0' }}>
            tell 기록이 여기 흘러요
          </div>
        </div>
      </div>
    </div>
  );
}
