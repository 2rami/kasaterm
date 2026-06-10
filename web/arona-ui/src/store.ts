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
  action?: string;
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
