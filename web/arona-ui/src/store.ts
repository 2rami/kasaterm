import { create } from 'zustand';
import type { AccentColorName } from '@/design/tokens';
import type { StatusKind } from '@/components/PixelBadge';
import type { BackgroundAgent } from './lib/mcp';

/** 화면이 그리는 에이전트 = kasaterm board 한 행을 UI 모델로 정규화한 것.
 *  munder 의 거대한 Agent 타입에서 AgentCard 가 실제로 쓰는 필드만 추렸다. */
export interface Agent {
  id: string;
  name: string;
  character: string;
  /** 작업제목(board.title, ai-title) — 헤더에 캐릭터명과 분리해 표시(name 은 합본 라벨). */
  title?: string;
  /** 외형 전용 캐릭터명 — 교실 스프라이트가 char/슬러그를 이걸로 찾는다(이름표는
   *  character 그대로). 백엔드 character 마커가 BA 학생명이면 동일, 작업명이면
   *  게임개발부 학생(미도리/모모이/유즈/아리스)을 폴백 배정. assignSprites 가 채움. */
  spriteChar?: string;
  accent: AccentColorName;
  /** 캐릭터 고정 accent(hex, kasaterm theme.rs 와 동일) — 있으면 순환색(accent)을
   *  이긴다. 테두리·버블 이름색이 네이티브 pane 색과 일치하게. */
  accentHex?: string;
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
  /** claude 프로세스 실행 경로 — pid_cwd 라이브(내부 cd 안 보임). */
  cwd?: string;
  /** statusLine 이 보고한 "현재 보는 경로"(report-cwd, 내부 cd 반영). 푸터 "현재 보는 경로". */
  viewCwd?: string;
  /** claude saved default effort(settings.json) — resume 직후 effort 카드 폴백값. */
  savedEffort?: string;
  branch?: string;
  /** 속한 윈도우(방) 인덱스 — 좌측 방별 학생 트리 그룹핑. board window_idx. */
  windowIdx?: number;
  /** 마지막 사용자 프롬프트 — 대화 미리보기용. */
  lastPrompt?: string;
  /** 선생님 입력을 기다리는 진짜 이유(AskUserQuestion·권한). 있을 때만 '확인 필요'(빨강).
   *  단순 waiting(완료 보고·응답 대기)은 이게 없다(거노: 빨강 남발 방지). */
  waitingFor?: string;
  /** 명시적 완료 보고(`kasaterm-cli done`) — "succeeded" | "failed". idle 추정과 달리
   *  학생이 직접 선언한 정본. 새 브리프로 다시 일하면 백엔드가 걷는다. */
  doneOutcome?: string;
  doneSummary?: string;
  doneAgoSecs?: number;
  /** 이사 간 학생이 실제로 도는 기계 라벨(board.machine) — 로컬 학생은 없음.
   *  이사 탭 섹션 분류·보드 칩이 이걸로 가른다. */
  machine?: string;
}

/** 한 학생(pane)이 소환한 서브에이전트(Task/Agent) — 백엔드 /subagents 가
 *  subagents/agent-<id>.meta.json 에서 모아 준다. agentId 로 그 서브에이전트
 *  대화(jsonl)를 따로 불러온다. mtime = 마지막 활동(unix secs). */
export interface SubagentInfo {
  agentId: string;
  agentType: string;
  description: string;
  mtime: number;
}

/** 빨강 '확인 필요' 판정 — AskUserQuestion·권한 선택지처럼 waiting_for(질문 내용)가
 *  있는 것만. 단순 waiting(완료 보고·응답 대기)은 제외한다(거노: 빨강이 위험하게 느껴짐). */
export function isAwaitingTeacher(a: Agent): boolean {
  return !!a.waitingFor || a.status === 'blocked';
}

interface AppState {
  agents: Agent[];
  setAgents: (a: Agent[]) => void;
  /** `claude agents` 가 보고하는 pane 밖 background 세션(+interactive). 교실에 별도 표시. */
  backgroundAgents: BackgroundAgent[];
  setBackgroundAgents: (a: BackgroundAgent[]) => void;
  /** 선생님이 '확인'(대화 열기)한, 현재 대기 에피소드의 학생 id 들. 학생이 대기를
   *  벗어나면 setAgents 가 자동으로 빼서, 다음 질문 때 다시 '미확인'이 된다. */
  acked: string[];
  ackStudent: (id: string) => void;
}

export const useStore = create<AppState>((set) => ({
  agents: [],
  backgroundAgents: [],
  setBackgroundAgents: (backgroundAgents) => set({ backgroundAgents }),
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
