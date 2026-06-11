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
  /** 학생 메타 — 모델명/작업경로/git 브랜치(board PaneActivity). contextLimit 은 위. */
  model?: string;
  cwd?: string;
  branch?: string;
  /** 마지막 사용자 프롬프트 — 대화 미리보기용. */
  lastPrompt?: string;
}

interface AppState {
  agents: Agent[];
  setAgents: (a: Agent[]) => void;
}

export const useStore = create<AppState>((set) => ({
  agents: [],
  setAgents: (agents) => set({ agents })
}));
