import { useEffect, useRef, useState } from 'react';
import { BoardPanel } from './BoardPanel';
import { ScheduleTab } from './ScheduleTab';
import { GitTab } from './GitTab';
import type { BackgroundAgent } from '@/lib/mcp';

type CenterTab = 'board' | 'schedule' | 'git';

const TAB_LABELS: Record<CenterTab, string> = {
  board: '보드',
  schedule: '스케줄',
  git: '소스 컨트롤',
};

export interface CommandCenterProps {
  /** 교실/카드에서 클릭한 학생 — '학생별 대화' 탭에 그 대화를 띄운다. */
  selected?: { id: string; title: string } | null;
  onClearDialog?: () => void;
  /** 모모톡에서 학생/아로나에게 보냈을 때 — 그 학생 '학생별 대화' 탭으로 전환(거노). */
  onPickStudent?: (id: string, title: string) => void;
  onOpenBackground?: (a: BackgroundAgent) => void;
  /** 타이틀바 소스컨트롤 버튼 클릭 신호(증가) — git 탭으로 전환(거노). */
  openGitTab?: number;
  /** 우측 패널 접기 — 부모(App)가 rightHidden 으로 레일 전환(거노: 가장자리 접기). */
  onCollapse?: () => void;
}

// SCHALE OS 우측 Command Center — 대화창이 따로 안 뜨고 여기 '학생별 대화' 탭에 통합(거노).

export function CommandCenter({ onPickStudent, onOpenBackground, openGitTab, onCollapse }: CommandCenterProps) {
  const [tab, setTab] = useState<CenterTab>('board'); // 우측 기본 = 보드(현황·소통)
  // 탭 순서 — 드래그로 재정렬(거노). 기본 보드/모모톡/스케줄/소스컨트롤.
  const [tabOrder, setTabOrder] = useState<CenterTab[]>(['board', 'schedule', 'git']);
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
  // 타이틀바 소스컨트롤 버튼 → git 탭으로(거노).
  useEffect(() => { if (openGitTab) setTab('git'); }, [openGitTab]);

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
        {/* 우측 패널 접기 — 가장자리 접기 일원화(거노). ▶ = 오른쪽으로 접어 레일로. */}
        <button onClick={() => onCollapse?.()} title="우측 패널 접기" style={{
          width: 18, height: 18, borderRadius: 5, border: 'none', cursor: 'pointer', background: 'transparent',
          color: 'var(--cth-ink-300)', display: 'flex', alignItems: 'center', justifyContent: 'center', flexShrink: 0,
        }}>
          <svg width="12" height="12" viewBox="0 0 16 16"><path d="M6 3l5 5-5 5" stroke="currentColor" strokeWidth="2" fill="none" strokeLinecap="round" strokeLinejoin="round" /></svg>
        </button>
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
      {tab === 'board' ? (
        /* 보드 — pane 작업 현황·상세 + 백그라운드 에이전트 + tell 소통(업무·모모톡·agents 흡수) */
        <BoardPanel onPickStudent={onPickStudent} onOpenBackground={onOpenBackground} />
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
