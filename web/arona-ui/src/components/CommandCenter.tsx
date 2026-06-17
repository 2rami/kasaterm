import { useEffect, useRef, useState } from 'react';
import { useStore, isAwaitingTeacher, isUnconfirmed } from '@/store';
import { MomoTalk } from './MomoTalk';
import { ScheduleTab } from './ScheduleTab';
import { GitTab } from './GitTab';
import { TerminalPeekPanel } from './TerminalPeekPanel';
import { isBuildCmd, BUILD_COLOR, GearIcon, SpinIcon, ForkIcon } from './activity';
import { fetchPaneTasks, type PaneTask } from '@/lib/mcp';

type CenterTab = 'momotalk' | 'dialog' | 'schedule' | 'tasks' | 'git';

// 태스크 정렬 순위 — 진행중 먼저, 그다음 대기, 완료는 맨 뒤(거노).
const taskRank = (s: string) => (s === 'in_progress' ? 0 : s === 'completed' ? 2 : 1);

const TAB_LABELS: Record<CenterTab, string> = {
  momotalk: '모모톡',
  dialog: '대화',
  schedule: '스케줄',
  tasks: '업무',
  git: '소스 컨트롤',
};

export interface CommandCenterProps {
  /** 교실/카드에서 클릭한 학생 — '학생별 대화' 탭에 그 대화를 띄운다. */
  selected?: { id: string; title: string } | null;
  onClearDialog?: () => void;
  /** 모모톡에서 학생/아로나에게 보냈을 때 — 그 학생 '학생별 대화' 탭으로 전환(거노). */
  onPickStudent?: (id: string, title: string) => void;
  /** 타이틀바 소스컨트롤 버튼 클릭 신호(증가) — git 탭으로 전환(거노). */
  openGitTab?: number;
}

// SCHALE OS 우측 Command Center — 대화창이 따로 안 뜨고 여기 '학생별 대화' 탭에
// 통합(거노). 탭: 모모톡 / 학생별 대화 / 스케줄 / 업무(AskQuestion·선택지).
// 확인 대기 알림 종(이모지 금지 → SVG).
function BellGlyph() {
  return (
    <svg width="13" height="13" viewBox="0 0 16 16" style={{ display: 'block', flexShrink: 0 }}>
      <path d="M8 1.6a1 1 0 0 1 1 1v.5a4 4 0 0 1 3 3.87V9l1.2 2.2a.6.6 0 0 1-.53.9H3.33a.6.6 0 0 1-.53-.9L4 9V6.97a4 4 0 0 1 3-3.87v-.5a1 1 0 0 1 1-1Z" fill="currentColor" />
      <path d="M6.4 13.2a1.7 1.7 0 0 0 3.2 0" stroke="currentColor" strokeWidth="1.1" fill="none" strokeLinecap="round" />
    </svg>
  );
}

export function CommandCenter({ selected, onClearDialog, onPickStudent, openGitTab }: CommandCenterProps) {
  const [tab, setTab] = useState<CenterTab>('dialog'); // 처음 열릴 때 대화 탭(거노)
  // 탭 순서 — 드래그로 재정렬(거노). 기본 대화/업무/모모톡/스케줄/소스컨트롤.
  const [tabOrder, setTabOrder] = useState<CenterTab[]>(['dialog', 'tasks', 'momotalk', 'schedule', 'git']);
  const [dragOverTab, setDragOverTab] = useState<CenterTab | null>(null); // 드래그 중 삽입선 위치
  const dragTabRef = useRef<CenterTab | null>(null);
  const reorderTab = (target: CenterTab) => {
    const from = dragTabRef.current;
    dragTabRef.current = null;
    if (!from || from === target) return;
    setTabOrder((order) => {
      const next = order.filter((t) => t !== from);
      next.splice(next.indexOf(target), 0, from);
      return next;
    });
  };
  const agents = useStore((s) => s.agents);
  const acked = useStore((s) => s.acked);
  // 학생을 클릭하면 자동으로 '학생별 대화' 탭으로 전환.
  useEffect(() => { if (selected) setTab('dialog'); }, [selected?.id]);
  // 타이틀바 소스컨트롤 버튼 → git 탭으로(거노).
  useEffect(() => { if (openGitTab) setTab('git'); }, [openGitTab]);

  // claude TaskCreate 태스크(~/.claude/tasks) — 업무 탭 볼 때만 폴링, pane 별 그룹.
  const [paneTasks, setPaneTasks] = useState<Record<string, PaneTask[]>>({});
  useEffect(() => {
    if (tab !== 'tasks') return;
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
  }, [tab]);

  return (
    <div style={{
      width: '100%', // 폭은 App 의 wrapper(드래그 조절)가 제어
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
        {tabOrder.map((t) => (
          <button
            key={t}
            onClick={() => setTab(t)}
            draggable
            onDragStart={(e) => { dragTabRef.current = t; e.dataTransfer.effectAllowed = 'move'; e.dataTransfer.setData('text/plain', t); }}
            onDragEnter={(e) => { e.preventDefault(); if (dragTabRef.current && dragTabRef.current !== t) setDragOverTab(t); }}
            onDragOver={(e) => { e.preventDefault(); e.dataTransfer.dropEffect = 'move'; }}
            onDragEnd={() => { dragTabRef.current = null; setDragOverTab(null); }}
            onDrop={(e) => { e.preventDefault(); reorderTab(t); setDragOverTab(null); }}
            style={{
              flexShrink: 0,
              padding: '6px 12px',
              fontFamily: 'var(--cth-font-ui)',
              fontSize: 12, fontWeight: 600,
              border: 'none', borderRadius: 7,
              background: tab === t ? 'var(--cth-sky)' : 'transparent',
              color: tab === t ? '#fff' : 'var(--cth-ink-500)',
              cursor: 'grab',
              whiteSpace: 'nowrap',
              // 드래그 삽입선 — 이 탭 앞에 떨굴 위치면 좌측에 파란 선(거노: 위치 선으로).
              boxShadow: dragOverTab === t ? 'inset 3px 0 0 0 var(--cth-sky)' : 'none',
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
      ) : tab === 'dialog' ? (
        /* 학생별 대화 — 클릭한 학생 대화를 여기 인라인(옛 우측 peek 패널 통합). */
        selected ? (
          <TerminalPeekPanel surfaceId={selected.id} title={selected.title} onClose={onClearDialog ?? (() => {})} embedded />
        ) : (
          <div style={{ flex: 1, display: 'flex', alignItems: 'center', justifyContent: 'center', padding: 20, textAlign: 'center', color: 'var(--cth-ink-300)', fontFamily: 'var(--cth-font-ui)', fontSize: 12, lineHeight: 1.6 }}>
            교실에서 학생을 클릭하면<br />여기에 대화가 떠요
          </div>
        )
      ) : tab === 'tasks' ? (
        /* 업무 — 학생별 현재 작업(빌드/도구 + 백그라운드 + 서브에이전트 + 도구 흐름) */
        <div style={{ flex: 1, overflowY: 'auto', padding: 10 }}>
          {/* 확인 대기 — question/선택지로 막혀 선생님 입력을 기다리는 학생(거노: 업무 패널에). */}
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
                        <span style={{ flex: 1, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{a.character}</span>
                        <span style={{ fontSize: 11, fontWeight: 600, opacity: 0.85 }}>{un ? '확인 필요' : '확인함'}</span>
                      </button>
                    );
                  })}
                </div>
              </div>
            );
          })()}
          {agents.length === 0 ? (
            <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 11, color: 'var(--cth-ink-300)' }}>학생 없음</span>
          ) : agents.map((a) => {
            const building = a.status === 'working' && isBuildCmd(a.action);
            return (
            <div key={a.id} style={{ padding: '7px 0', borderBottom: '1px solid var(--cth-cream-200)' }}>
              {/* 헤더 클릭 → 그 학생 '대화' 탭(프롬프트·명령어 흐름)으로(거노). */}
              <div onClick={() => onPickStudent?.(a.id, a.character)} title="대화 열기" style={{ display: 'flex', alignItems: 'center', gap: 8, cursor: 'pointer' }}>
                <span style={{ width: 8, height: 8, borderRadius: 999, flexShrink: 0, background: a.status === 'working' ? 'var(--cth-mint)' : a.status === 'waiting' || a.status === 'blocked' ? 'var(--cth-coral)' : 'var(--cth-ink-300)' }} />
                <div style={{ flex: 1, minWidth: 0 }}>
                  <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: 600, color: 'var(--cth-ink-900)' }}>{a.character}</div>
                  <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 11, color: 'var(--cth-ink-500)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{a.project || '대기 중'}</div>
                </div>
                {building ? (
                  <span style={{ display: 'inline-flex', alignItems: 'center', gap: 3, fontFamily: 'var(--cth-font-ui)', fontSize: 10, fontWeight: 700, color: BUILD_COLOR, background: 'color-mix(in srgb, #E5923A 14%, #fff)', padding: '2px 7px', borderRadius: 6 }}><GearIcon size={11} />빌드 중</span>
                ) : a.currentTool ? (
                  <span style={{ fontFamily: 'var(--cth-font-mono)', fontSize: 10, fontWeight: 700, color: 'var(--cth-sky)', background: 'color-mix(in srgb, var(--cth-sky) 12%, #fff)', padding: '2px 7px', borderRadius: 6 }}>{a.currentTool}</span>
                ) : null}
                {!!a.background?.length && (
                  <span title={a.background.join('\n')} style={{ display: 'inline-flex', alignItems: 'center', gap: 3, fontFamily: 'var(--cth-font-ui)', fontSize: 10, fontWeight: 700, color: BUILD_COLOR, background: 'color-mix(in srgb, #E5923A 14%, #fff)', padding: '2px 7px', borderRadius: 6 }}><SpinIcon size={10} />bg {a.background.length}</span>
                )}
                {!!a.subagents?.length && (
                  <span title={a.subagents.join('\n')} style={{ display: 'inline-flex', alignItems: 'center', gap: 3, fontFamily: 'var(--cth-font-ui)', fontSize: 10, fontWeight: 700, color: 'var(--cth-lilac)', background: 'color-mix(in srgb, var(--cth-lilac) 14%, #fff)', padding: '2px 7px', borderRadius: 6 }}><ForkIcon size={10} />{a.subagents.length}</span>
                )}
              </div>
              {/* claude TaskCreate 태스크 — 진행중(◉) 먼저, 학생 작업 진행상황(거노: 맨 위에). */}
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
              {/* 도구 활동 타임라인(오래된→최근) */}
              {!!a.recentTools?.length && (
                <div style={{ marginLeft: 16, marginTop: 4, display: 'flex', flexWrap: 'wrap', gap: 3, alignItems: 'center' }}>
                  {[...a.recentTools].reverse().map((t, i, arr) => (
                    <span key={i} style={{ display: 'inline-flex', alignItems: 'center', gap: 3 }}>
                      <span title={t} style={{ fontFamily: 'var(--cth-font-mono)', fontSize: 9, color: 'var(--cth-ink-500)', background: 'var(--cth-cream-100)', padding: '1px 5px', borderRadius: 5, whiteSpace: 'nowrap', maxWidth: 200, overflow: 'hidden', textOverflow: 'ellipsis' }}>{t}</span>
                      {i < arr.length - 1 && <span style={{ fontSize: 8, color: 'var(--cth-ink-300)' }}>→</span>}
                    </span>
                  ))}
                </div>
              )}
            </div>
            );
          })}
        </div>
      ) : tab === 'git' ? (
        /* 소스 컨트롤 — 활성 pane cwd 의 git 상태·커밋·푸시(스케줄 옆, 거노). */
        <GitTab />
      ) : (
        /* 스케줄/루프 — 반복 지시 루프 · 예약(크론) · 타이머/리마인더. */
        <ScheduleTab />
      )}
    </div>
  );
}
