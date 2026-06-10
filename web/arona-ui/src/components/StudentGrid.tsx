import type { Agent } from '@/store';

// StudentGrid props 인터페이스 — 시로코 정의, 아리스가 내부를 채운다.
// App.tsx 가 agents 배열을 그대로 내려줌. onSelect = 카드 클릭 시 peek 패널 열기.
export interface StudentGridProps {
  agents: Agent[];
  onSelect?: (id: string, title: string) => void;
}

// stub — 아리스가 내부 구현을 채울 때까지 빈 fragment.
export function StudentGrid(_props: StudentGridProps) {
  return null;
}
