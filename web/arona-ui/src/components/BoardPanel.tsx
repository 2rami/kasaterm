import { useEffect, useState } from 'react';
import { useStore, isAwaitingTeacher, isUnconfirmed } from '@/store';
import { saveSession } from '@/lib/mcp';
import { SpritePortrait } from './SpritePortrait';
import { assignSprites } from '@/lib/sprites';
import { fetchPaneTasks, type PaneTask } from '@/lib/mcp';
import { isBuildCmd, BUILD_COLOR, GearIcon, SpinIcon, ForkIcon } from './activity';
import { PaneToolTimeline } from './PaneToolTimeline';

const taskRank = (s: string) => (s === 'in_progress' ? 0 : s === 'completed' ? 2 : 1);

// 한 pane 줄에 완료 태스크를 몇 개까지 보일지. 저장소는 **방 단위로 공유**돼서 바쁜
// 방은 하루 이틀 만에 완료가 수십 개 쌓인다(실측 2026-08-06: 34개 중 29개 완료,
// 전부 어제·오늘 것) — 거노: "아루 태스크는 왜 저렇게 돼 있어". 그 줄에서 봐야 할 건
// **지금 뭘 하는가**지 오늘 끝낸 것 전부가 아니라, 열린 것은 다 보이고 완료는 최근
// 몇 개만 남긴 뒤 나머지는 개수로 접는다. 오래된 파일 자체는 앱이 부팅 때 지운다
// (main.rs `prune_finished_tasks`) — 그건 어제 것을 못 지우니 이 상한이 따로 필요하다.
const DONE_SHOWN = 3;

// 확인 대기 알림 종 — 이모지 금지 SVG.
function BellGlyph() {
  return (
    <svg width="13" height="13" viewBox="0 0 16 16" style={{ display: 'block', flexShrink: 0 }}>
      <path d="M8 1.6a1 1 0 0 1 1 1v.5a4 4 0 0 1 3 3.87V9l1.2 2.2a.6.6 0 0 1-.53.9H3.33a.6.6 0 0 1-.53-.9L4 9V6.97a4 4 0 0 1 3-3.87v-.5a1 1 0 0 1 1-1Z" fill="currentColor" />
      <path d="M6.4 13.2a1.7 1.7 0 0 0 3.2 0" stroke="currentColor" strokeWidth="1.1" fill="none" strokeLinecap="round" />
    </svg>
  );
}

// 완료 보고 경과 — 절대 시각은 읽는 사람이 매번 뺄셈해야 한다(거노: 상대 시간으로).
const agoLabel = (secs?: number) => {
  if (secs == null) return '';
  if (secs < 60) return '방금';
  if (secs < 3600) return `${Math.floor(secs / 60)}분 전`;
  return `${Math.floor(secs / 3600)}시간 전`;
};

const SectionLabel = ({ children }: { children: string }) => (
  <div style={{
    fontFamily: 'var(--cth-font-ui)', fontSize: 10, fontWeight: 700, color: 'var(--cth-ink-500)',
    textTransform: 'uppercase', letterSpacing: 0.5, margin: '2px 4px 8px',
  }}>{children}</div>
);

// board = 모든 pane 작업 현황·상세(업무 흡수) + pane 간 tell 소통 피드. 모모톡·inbox 대체(거노).
// 빨강 '확인 필요'는 waiting_for(AskUserQuestion·권한) 있는 것만 — isAwaitingTeacher 가 그 판정.
export function BoardPanel({ onPickStudent, onSaved }: { onPickStudent?: (id: string, title: string) => void; onSaved?: (surface: string) => void }) {
  const agents = useStore((s) => s.agents);
  const acked = useStore((s) => s.acked);
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
                <button
                  onClick={async (e) => { e.stopPropagation(); const ok = await saveSession(a.id); if (ok) onSaved?.(a.id); }}
                  title="대화 저장 — background daemon 으로 보내 터미널이 꺼져도 유지(←← detach)"
                  style={{ flexShrink: 0, fontFamily: 'var(--cth-font-ui)', fontSize: 10, fontWeight: 700, color: 'var(--cth-ink-500)', background: 'transparent', border: '1px solid var(--cth-cream-200)', borderRadius: 6, padding: '2px 7px', cursor: 'pointer' }}
                >저장</button>
                {/* 명시적 완료 보고 — idle 추정이 아니라 학생이 직접 선언한 결과.
                    요약·경과는 툴팁에(칩은 한 눈에 성패만). */}
                {a.doneOutcome && (
                  <span
                    title={[a.doneSummary, agoLabel(a.doneAgoSecs)].filter(Boolean).join(' — ')}
                    style={{
                      flexShrink: 0, fontFamily: 'var(--cth-font-ui)', fontSize: 10, fontWeight: 800,
                      color: a.doneOutcome === 'succeeded' ? 'var(--cth-status-success)' : 'var(--cth-coral)',
                      background: a.doneOutcome === 'succeeded'
                        ? 'color-mix(in srgb, var(--cth-status-success) 13%, #fff)'
                        : 'color-mix(in srgb, var(--cth-coral) 13%, #fff)',
                      padding: '2px 7px', borderRadius: 6,
                    }}
                  >{a.doneOutcome === 'succeeded' ? '✓ 완료 보고' : '✗ 실패 보고'}</span>
                )}
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
                  {(() => {
                    // 목록은 방 하나를 여럿이 나눠 쓴다 — 그대로 그리면 같은 방 pane 카드마다
                    // **같은 태스크 뭉치가 통째로 반복**된다(거노 2026-08-06: "각자 태스크가
                    // 있으면 좋겠네"). 남이 주인인 것은 그 사람 카드에만 두고, 여기선 개수로만
                    // 알린다. 주인 없는 것도 마찬가지 — 방 저장소엔 그 cwd 에서 돌았던 옛
                    // 세션이 전부 쌓여 있어서(실측 59개 중 55개가 주인 없는 유령) 그걸 카드에
                    // 풀면 지금 뭘 하는지가 통째로 묻힌다. 개수로만 알리고 툴팁에 담는다.
                    const room = [...paneTasks[a.id]].sort((x, y) => taskRank(x.status) - taskRank(y.status));
                    const all = room.filter((t) => t.mine !== false);
                    // 미배정은 아무도 안 잡은 일 — 끝난 것까지 셀 이유는 없다(주인 없이 끝난
                    // 건 이미 지나간 일이고, 여기서 봐야 할 건 「누가 집어 가야 하나」다).
                    const idle = room.filter((t) => t.mine === false && !t.owner && t.status !== 'completed');
                    const others = room.filter((t) => t.mine === false && !!t.owner).length;
                    // 정렬이 완료를 뒤로 몰아 두므로 앞에서 자르면 열린 것은 하나도 안 잘린다.
                    const open = all.filter((t) => t.status !== 'completed');
                    const doneAll = all.filter((t) => t.status === 'completed');
                    const shown = [...open, ...doneAll.slice(0, DONE_SHOWN)];
                    const folded = doneAll.length - Math.min(doneAll.length, DONE_SHOWN);
                    return (
                      <>
                        {shown.map((t) => {
                          const done = t.status === 'completed';
                          const active = t.status === 'in_progress';
                          return (
                            <div key={t.id} style={{ display: 'flex', alignItems: 'center', gap: 5, fontFamily: 'var(--cth-font-ui)', fontSize: 11, fontWeight: active ? 700 : 500, color: done ? 'var(--cth-ink-300)' : active ? 'var(--cth-mint)' : 'var(--cth-ink-700)' }}>
                              <span style={{ flexShrink: 0, width: 10, textAlign: 'center' }}>{done ? '✓' : active ? '◉' : '○'}</span>
                              <span style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', textDecoration: done ? 'line-through' : 'none' }}>{t.subject}</span>
                            </div>
                          );
                        })}
                        {folded > 0 && (
                          <div
                            title={doneAll.slice(DONE_SHOWN).map((t) => t.subject).join('\n')}
                            style={{ marginLeft: 15, fontFamily: 'var(--cth-font-ui)', fontSize: 10, color: 'var(--cth-ink-300)' }}
                          >
                            완료 {folded}개 더
                          </div>
                        )}
                        {idle.length > 0 && (
                          <div
                            title={idle.map((t) => t.subject).join('\n')}
                            style={{ marginLeft: 15, fontFamily: 'var(--cth-font-ui)', fontSize: 10, color: 'var(--cth-ink-300)' }}
                          >
                            미배정 {idle.length}개
                          </div>
                        )}
                        {others > 0 && (
                          <div
                            title={room.filter((t) => t.mine === false && !!t.owner).map((t) => `${t.owner} · ${t.subject}`).join('\n')}
                            style={{ marginLeft: 15, fontFamily: 'var(--cth-font-ui)', fontSize: 10, color: 'var(--cth-ink-300)' }}
                          >
                            같은 방 다른 학생 {others}개
                          </div>
                        )}
                      </>
                    );
                  })()}
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
