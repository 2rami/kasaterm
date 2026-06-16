import { create } from 'zustand';
import type { AccentColorName } from '@/design/tokens';
import type { StatusKind } from '@/components/PixelBadge';

/** 화면이 그리는 에이전트 = kasaterm board 한 행을 UI 모델로 정규화한 것.
 *  munder 의 거대한 Agent 타입에서 AgentCard 가 실제로 쓰는 필드만 추렸다. */
export interface Agent {
  id: string;
  name: string;
  character: string;
  accent: AccentColorName;
  status: StatusKind;
  project: string;
  /** 현재 행동(board.intent — tool 기반). 생각 구름의 working 텍스트. */
  action?: string;
  /** 마지막 답변/질문 첫마디(board.last_reply). waiting/idle 구름 텍스트 소스. */
  lastReply?: string;
  progress?: number;
  contextTokens?: number;
  contextLimit?: number;
  /** 컨텍스트 사용량 % — claude TUI 상태바 파싱(board.context_pct). transcript
   *  토큰이 0 이어도 robust. 인연 바·메타에 사용. */
  contextPct?: number;
  isGod?: boolean;
  /** 현재(최신) 사용 중인 tool 이름 — board.intent 첫 토큰(Bash/Read/Edit). 말풍선용. */
  currentTool?: string;
  /** tail 윈도 tool 사용 분포 — [["Bash",5],["Edit",3]]. 카드 tool 칩 / Task 추적용. */
  toolCounts?: [string, number][];
  /** 누적 비용(USD) — board.cost_usd. 재화 치환용. */
  costUsd?: number;
  /** 입력/출력 토큰 분리 — 재화(크리스탈=입력) 치환용. contextTokens 는 합. */
  tokensIn?: number;
  tokensOut?: number;
  /** 진행 중 서브에이전트 description 목록 — "서브에이전트 N 실행 중" 표시. */
  subagents?: string[];
  /** 최근 완료된 서브에이전트(이름) — 잠깐 "✓ 완료"로 흔적. */
  subagentsDone?: string[];
  /** 진행 중 백그라운드 셸(설명/명령) — "백그라운드 N개 실행 중". */
  background?: string[];
  /** 최근 도구 사용 흐름(라벨, 최신순) — 도구 활동 타임라인. */
  recentTools?: string[];
  /** 학생 메타 — 모델명/작업경로/git 브랜치(board PaneActivity). contextLimit 은 위. */
  model?: string;
  cwd?: string;
  branch?: string;
  /** 마지막 사용자 프롬프트 — 대화 미리보기용. */
  lastPrompt?: string;
}

/** 학생이 question/선택지로 막혀 선생님 입력을 기다리는 상태(blocked=권한·입력
 *  프롬프트, waiting=응답 대기). '확인 대기'·'미확인' 판정의 기준. */
export function isAwaitingTeacher(a: Agent): boolean {
  return a.status === 'blocked' || a.status === 'waiting';
}

interface AppState {
  agents: Agent[];
  setAgents: (a: Agent[]) => void;
  /** 선생님이 '확인'(대화 열기)한, 현재 대기 에피소드의 학생 id 들. 학생이 대기를
   *  벗어나면 setAgents 가 자동으로 빼서, 다음 질문 때 다시 '미확인'이 된다. */
  acked: string[];
  ackStudent: (id: string) => void;
}

export const useStore = create<AppState>((set) => ({
  agents: [],
  acked: [],
  setAgents: (agents) =>
    set((s) => ({
      agents,
      // 대기 상태를 벗어난(또는 사라진) 학생은 확인플래그 리셋 — 다음 대기 때 다시 미확인.
      acked: s.acked.filter((id) => {
        const a = agents.find((x) => x.id === id);
        return !!a && isAwaitingTeacher(a);
      }),
    })),
  ackStudent: (id) =>
    set((s) => (s.acked.includes(id) ? s : { acked: [...s.acked, id] })),
}));

/** blocked/waiting 인데 선생님이 아직 그 대화를 안 열어봄 = 머리 위 코랄 '확인 필요!'. */
export function isUnconfirmed(a: Agent, acked: string[]): boolean {
  return isAwaitingTeacher(a) && !acked.includes(a.id);
}
