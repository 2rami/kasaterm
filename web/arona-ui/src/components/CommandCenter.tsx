import { useState } from 'react';
import { useStore } from '@/store';
import { MomoTalk } from './MomoTalk';
import { ScheduleTab } from './ScheduleTab';
import { isBuildCmd, BUILD_COLOR, GearIcon, SpinIcon, ForkIcon } from './activity';

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
        /* 업무 — 학생별 현재 작업(빌드/도구 + 백그라운드 + 서브에이전트 + 도구 흐름) */
        <div style={{ flex: 1, overflowY: 'auto', padding: 10 }}>
          {agents.length === 0 ? (
            <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 11, color: 'var(--cth-ink-300)' }}>학생 없음</span>
          ) : agents.map((a) => {
            const building = a.status === 'working' && isBuildCmd(a.action);
            return (
            <div key={a.id} style={{ padding: '7px 0', borderBottom: '1px solid var(--cth-cream-200)' }}>
              <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
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
                      <span style={{ fontFamily: 'var(--cth-font-mono)', fontSize: 9, color: 'var(--cth-ink-500)', background: 'var(--cth-cream-100)', padding: '1px 5px', borderRadius: 5, whiteSpace: 'nowrap', maxWidth: 120, overflow: 'hidden', textOverflow: 'ellipsis' }}>{t.split(' ')[0]}</span>
                      {i < arr.length - 1 && <span style={{ fontSize: 8, color: 'var(--cth-ink-300)' }}>→</span>}
                    </span>
                  ))}
                </div>
              )}
            </div>
            );
          })}
        </div>
      ) : (
        /* 스케줄/루프 — 반복 지시 루프 · 예약(크론) · 타이머/리마인더. */
        <ScheduleTab />
      )}
    </div>
  );
}
