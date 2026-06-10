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
}

interface AppState {
  agents: Agent[];
  setAgents: (a: Agent[]) => void;
}

export const useStore = create<AppState>((set) => ({
  agents: [],
  setAgents: (agents) => set({ agents })
}));
