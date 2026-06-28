import { useStore, type Agent, type SubagentInfo } from '@/store';
import type { AccentColorName } from '@/design/tokens';
import type { StatusKind } from '@/components/PixelBadge';
import type { SessionEvent } from './types';
import { parseJsonlSync } from './jsonl';

// munder 는 electron IPC(window.cth.*)로 hive 와 통신했지만, 우리는 kasaterm 의
// kasaspace MCP HTTP 를 fetch 로 폴링한다. dist 는 MCP 가 /arona-ui/ 로 정적
// 서빙하므로 페이지·API 가 **same-origin** → BASE='' (relative). 8765 가 점유돼
// 랜덤 포트로 폴백해도 안 끊긴다(유우카 P6c-2차). vite dev 로 띄울 때만 절대주소
// (개발 서버는 5173, API 는 별도 포트라 same-origin 이 아님). 기본 8765 지만
// `VITE_MCP_PORT` 로 덮어쓸 수 있다 — 8765 점유돼 폴백 포트로 뜬 인스턴스에 붙어
// 검증할 때 쓴다.
const MCP_PORT = import.meta.env.VITE_MCP_PORT || '8765';
const BASE = import.meta.env.DEV ? `http://127.0.0.1:${MCP_PORT}` : '';

// 워커 accent — pane id 숫자 해시(god-elect 워커 팔레트와 같은 결). god 은 lemon.
const ACCENTS: AccentColorName[] = ['sky', 'mint', 'coral', 'lilac', 'peach'];

interface BoardRow {
  surface_id: string;
  status?: string;
  intent?: string;
  title?: string;
  is_god?: boolean;
  tokens_in?: number;
  tokens_out?: number;
  /** 학생이 선생님 입력을 기다리는 진짜 이유(AskUserQuestion 질문·권한 'permission').
   *  이게 있을 때만 '확인 필요'(빨강). 없는 waiting 은 단순 응답 대기·완료 보고다(거노). */
  waiting_for?: string;
  /** 마지막 답변/질문 첫마디 — waiting/idle 생각 구름 텍스트 소스. */
  last_reply?: string;
  last_prompt?: string;
  /** 누적 토큰/비용 — 재화 치환 + 카드 표시(이미 /board 에 직렬화됨, UI 가 안 읽던 값). */
  cost_usd?: number;
  cache_read?: number;
  /** tail 윈도 tool 사용 분포 [["Bash",5],...] — Task 추적/말풍선 tool 이름. */
  tool_counts?: [string, number][];
  /** 진행 중 서브에이전트 description 목록(Task/Agent tool_use 미완료). */
  subagents?: string[];
  /** 최근 완료된 서브에이전트(이름) · 진행 중 백그라운드 셸 · 도구 흐름(최신순). */
  subagents_done?: string[];
  background?: string[];
  recent_tools?: string[];
  /** 학생 메타 — 모델/작업경로/컨텍스트한도/git 브랜치(PaneActivity). */
  model?: string;
  /** claude 프로세스 실행 경로 — pid_cwd(lsof) 라이브. 내부 cd 는 안 보인다. */
  cwd?: string;
  /** statusLine 이 보고한 "현재 보는 경로"(report-cwd) — claude 내부 cd 반영.
   *  cwd(실행 경로)와 함께 푸터에 "실행/현재 보는" 두 경로로 표시(거노). */
  view_cwd?: string;
  /** claude saved default effort(~/.claude/settings.json effortLevel) — resume 직후 effort 카드
   *  폴백값. ultracode 는 session-only 라 여기 안 들어온다. */
  effort_default?: string;
  context_limit?: number;
  /** 컨텍스트 사용량 % — claude TUI 상태바 파싱(robust). */
  context_pct?: number;
  branch?: string;
  /** 이 pane 이 속한 윈도우(방) 인덱스 — sessions.labels 인덱스와 정렬. 좌측 방별 트리. */
  window_idx?: number;
  /** 유우카가 character-<N> 마커를 읽어 노출(후속). 있으면 도트칩 이니셜·이름이
   *  캐릭터명(아로나/시로코/아리스…)으로, 없으면 title(ai-title) 폴백. */
  character?: string;
}

function toStatus(s?: string): StatusKind {
  switch (s) {
    case 'working': return 'working';
    case 'waiting': return 'waiting';
    case 'blocked': return 'blocked';
    default: return 'idle';
  }
}

function accentFor(id: string): AccentColorName {
  const n = parseInt(id.replace(/\D/g, '') || '0', 10);
  return ACCENTS[n % ACCENTS.length];
}

function toAgent(r: BoardRow): Agent {
  const tokens = (r.tokens_in ?? 0) + (r.tokens_out ?? 0);
  // 이름표(name)는 터미널 pane 탭과 통일한 "미도리 · 작업명" 라벨. 단 character 에는
  // 합성 라벨을 넣지 않는다(거노: 프사 안 뜸) — SpritePortrait 의 SLUG 매칭은 순수
  // 캐릭터명이 필요한데, 라벨이 character 로 새면 SLUG 미스 → 프사 대신 이니셜('모')이
  // 떴다. 그래서 name=라벨 / character=순수 캐릭터명(없으면 작업명·surface_id 폴백)으로 분리.
  const label = r.character
    ? (r.title ? `${r.character} · ${r.title}` : r.character)
    : (r.title || r.surface_id);
  const pureCharacter = r.character || r.title || r.surface_id;
  // 현재 tool = intent 라벨 첫 토큰("Edit auth.ts"→"Edit"). tool_use 없는 turn 직후의
  // 'active' 폴백은 tool 이름이 아니므로 버린다(말풍선엔 진짜 tool명만).
  const head = r.intent ? r.intent.split(' ')[0] : '';
  const currentTool = head && head !== 'active' ? head : undefined;
  // working 인데 아직 도구를 안 쓴 turn = 응답 생성 중(거노: active 로딩중 = thinking).
  let status = toStatus(r.status);
  if (status === 'working' && !currentTool) status = 'thinking';
  return {
    id: r.surface_id,
    name: label,
    character: pureCharacter,
    title: r.title,
    accent: r.is_god ? 'lemon' : accentFor(r.surface_id),
    status,
    project: r.intent ?? '',
    action: r.intent,
    lastReply: r.last_reply,
    lastPrompt: r.last_prompt,
    waitingFor: r.waiting_for,
    isGod: !!r.is_god,
    contextTokens: tokens > 0 ? tokens : undefined,
    currentTool,
    toolCounts: r.tool_counts,
    costUsd: r.cost_usd,
    tokensIn: r.tokens_in,
    tokensOut: r.tokens_out,
    subagents: r.subagents,
    subagentsDone: r.subagents_done,
    background: r.background,
    recentTools: r.recent_tools,
    model: r.model,
    cwd: r.cwd,
    viewCwd: r.view_cwd,
    savedEffort: r.effort_default,
    contextLimit: r.context_limit,
    contextPct: r.context_pct,
    branch: r.branch,
    windowIdx: r.window_idx ?? 0
  };
}

export async function fetchBoard(): Promise<BoardRow[]> {
  try {
    const res = await fetch(`${BASE}/board`);
    if (!res.ok) return [];
    const data = await res.json();
    // /board 가 {board:[...]} 든 JSON-RPC {result:{board:[...]}} 든 흡수.
    const rows = data?.board ?? data?.result?.board ?? [];
    return Array.isArray(rows) ? (rows as BoardRow[]) : [];
  } catch {
    return [];
  }
}

/** board 를 `intervalMs` 마다 폴링해 store 에 반영. 정리 함수를 반환(useEffect). */
export function startBoardPolling(intervalMs = 1000): () => void {
  let stopped = false;
  const tick = async () => {
    if (stopped) return;
    const rows = await fetchBoard();
    useStore.getState().setAgents(rows.map(toAgent));
  };
  void tick();
  const iv = setInterval(tick, intervalMs);
  return () => { stopped = true; clearInterval(iv); };
}

// ── 캐릭터 / 모드 / 스폰 / 포커스 (유우카 %3 의 HTTP 엔드포인트) ────────────────

export interface CharacterDef {
  name: string;
  claude_color?: string;
  header_color?: string;
  persona?: string;
  greeting?: string;
  /** 거노 아트 교체 구조 — 스프라이트 시트 경로(옵셔널). 채워지면
   *  ClassroomCharacter/SpritePortrait 가 placeholder 도트 대신 이 시트를 쓴다. */
  sprite?: string;
}
export interface Characters {
  theme?: string;
  user_title?: string;
  leader: CharacterDef;
  /** 리더 풀(아로나·프라나) — 학생 추가/교체 시 god 도 고를 수 있게(거노). */
  leaders?: CharacterDef[];
  members: CharacterDef[];
}

/** 캐릭터 선택 풀 — leaders(아로나·프라나) + members(미도리~아리스), 이름 중복 제거. */
export function characterPool(c: Characters | null): CharacterDef[] {
  if (!c) return [];
  const pool = [...(c.leaders ?? (c.leader ? [c.leader] : [])), ...(c.members ?? [])];
  const seen = new Set<string>();
  return pool.filter((m) => m && m.name && !seen.has(m.name) && seen.add(m.name));
}

/** GET /characters — ~/.config 우선→번들, 없으면 404(null). */
export async function fetchCharacters(): Promise<Characters | null> {
  try {
    const r = await fetch(`${BASE}/characters`);
    if (!r.ok) return null;
    return (await r.json()) as Characters;
  } catch {
    return null;
  }
}

export interface ModeInfo { mode: string | null; cwd: string | null; configured: boolean; }

/** GET /mode → {mode:'solo'|'god'|null, cwd, configured}. 응답 {…}/평문/{result} 흡수.
 *  configured=false = 마커 자체 없는 미설정 방(첫 실행) → ModePicker 온보딩 분기 기준
 *  (mode 만 보면 미설정이 solo 로 뭉개진다, 유우카). cwd 는 '이 방' 경로 칩. */
export async function fetchMode(): Promise<ModeInfo> {
  try {
    const r = await fetch(`${BASE}/mode`);
    if (!r.ok) return { mode: null, cwd: null, configured: false };
    const d = await r.json().catch(() => null);
    if (typeof d === 'string') return { mode: d, cwd: null, configured: true };
    const src = d?.result ?? d ?? {};
    return {
      mode: src.mode ?? null,
      cwd: src.cwd ?? null,
      // 구 백엔드(필드 부재)는 mode 존재 여부로 추정 — 미존재만 false.
      configured: src.configured ?? (src.mode != null)
    };
  } catch {
    return { mode: null, cwd: null, configured: false };
  }
}

/** POST /terminal-reveal?show=0|1[&pane=%N] — 교실의 숨긴 메인 터미널 토글(빨간약).
 *  show=1 에 pane 을 주면 그 pane 포커스까지(유우카). 미구현 백엔드면 404 → false. */
export async function revealTerminal(show = 1, pane?: string): Promise<boolean> {
  const q = new URLSearchParams({ show: show ? '1' : '0' });
  if (pane) q.set('pane', pane);
  try {
    const r = await fetch(`${BASE}/terminal-reveal?${q}`, { method: 'POST' });
    return r.ok;
  } catch {
    return false;
  }
}

/** POST /git-panel — 터미널 GUI git 소스컨트롤 패널 토글(아로나 타이틀바 버튼). */
export async function openGitPanel(): Promise<boolean> {
  try {
    const r = await fetch(`${BASE}/git-panel`, { method: 'POST' });
    return r.ok;
  } catch {
    return false;
  }
}

// ── git 소스컨트롤(아로나 탭) — 활성 pane cwd 기준 ──────────────────────────
export interface GitStatus {
  branch?: string; ahead?: number; behind?: number;
  staged?: string[]; modified?: string[]; untracked?: string[];
  clean?: boolean; insertions?: number; deletions?: number;
  no_repo?: boolean; error?: string; path?: string;
}
/** GET /git-status — 활성 pane cwd 의 git 상태(브랜치·변경파일). */
export async function fetchGitStatus(): Promise<GitStatus> {
  try {
    const r = await fetch(`${BASE}/git-status`);
    if (!r.ok) return {};
    return (await r.json()) as GitStatus;
  } catch {
    return {};
  }
}
/** POST /git-commit — 지정 파일 stage 후 commit. */
export async function gitCommit(files: string[], message: string): Promise<{ ok: boolean; output: string }> {
  try {
    const r = await fetch(`${BASE}/git-commit`, {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ files, message }),
    });
    const d = (await r.json().catch(() => ({}))) as { ok?: boolean; output?: string };
    return { ok: !!d.ok, output: d.output ?? '' };
  } catch {
    return { ok: false, output: '네트워크 오류' };
  }
}
/** POST /git-push — 현재 브랜치 push. */
export async function gitPush(): Promise<{ ok: boolean; output: string }> {
  try {
    const r = await fetch(`${BASE}/git-push`, { method: 'POST' });
    const d = (await r.json().catch(() => ({}))) as { ok?: boolean; output?: string };
    return { ok: !!d.ok, output: d.output ?? '' };
  } catch {
    return { ok: false, output: '네트워크 오류' };
  }
}

export interface SessionsInfo { count: number; active: number; labels: string[]; saved: string[]; }

/** GET /sessions — 로컬 PTY '방' = kasaterm 윈도우 목록(count/active/labels). 좌측 방 네비용. */
export async function fetchSessions(): Promise<SessionsInfo> {
  const empty: SessionsInfo = { count: 0, active: 0, labels: [], saved: [] };
  try {
    const r = await fetch(`${BASE}/sessions`);
    if (!r.ok) return empty;
    const d = await r.json();
    return { count: d.count ?? 0, active: d.active ?? 0, labels: d.labels ?? [], saved: d.saved ?? [] };
  } catch {
    return empty;
  }
}

/** POST /session-switch?idx=N — 그 방(윈도우)으로 터미널 전환(거노: 방=윈도우 클릭). */
export async function switchSession(idx: number): Promise<boolean> {
  try {
    const r = await fetch(`${BASE}/session-switch?idx=${idx}`, { method: 'POST' });
    return r.ok;
  } catch {
    return false;
  }
}

/** POST /session-new?god=<name> — 새 방(윈도우) + 선택 god(아로나/프라나) 자동 스폰(거노). */
export async function newRoom(god: string): Promise<boolean> {
  try {
    const r = await fetch(`${BASE}/session-new?god=${encodeURIComponent(god)}`, { method: 'POST' });
    return r.ok;
  } catch {
    return false;
  }
}

/** POST /spawn-student?character=<name> — 현재 방에 캐릭터 지정 학생 추가(아로나/프라나
 *  포함). 자동 빈슬롯 배정 대신 고른 캐릭터로 split. */
export async function spawnStudent(character: string): Promise<boolean> {
  try {
    const r = await fetch(`${BASE}/spawn-student?character=${encodeURIComponent(character)}`, { method: 'POST' });
    return r.ok;
  } catch {
    return false;
  }
}

/** POST /swap-character?surface=<id>&character=<name> — pane 캐릭터 교체(PTY respawn,
 *  대화 리셋). persona 가 셸 spawn 시 고정이라 그 pane 을 새 persona 로 다시 띄운다. */
export async function swapCharacter(surface: string, character: string): Promise<boolean> {
  try {
    const r = await fetch(`${BASE}/swap-character?surface=${encodeURIComponent(surface)}&character=${encodeURIComponent(character)}`, { method: 'POST' });
    return r.ok;
  } catch {
    return false;
  }
}

export interface RecentSession { id: string; label: string; mtime: number; cwd: string; }

/** GET /recent-sessions?cwd=<abs> — 최근 claude 세션 목록(이어가기 후보, 최신순).
 *  cwd 생략 시 active 방 cwd. fail-soft 빈 배열. */
export async function fetchRecentSessions(cwd?: string): Promise<RecentSession[]> {
  try {
    const q = cwd ? `?cwd=${encodeURIComponent(cwd)}` : '';
    const r = await fetch(`${BASE}/recent-sessions${q}`);
    if (!r.ok) return [];
    const d = (await r.json().catch(() => ({}))) as { sessions?: RecentSession[] };
    return Array.isArray(d?.sessions) ? d.sessions : [];
  } catch {
    return [];
  }
}

/** POST /session-resume?id=<uuid>&cwd=<abs>&newroom=<bool> — 새 pane 을 열고 셸
 *  프롬프트가 뜨면 `claude --resume <id>` 주입(이어가기). newroom=true 면 새 방. */
export async function resumeSession(id: string, cwd?: string, newroom = false): Promise<boolean> {
  if (!id) return false;
  try {
    const q = new URLSearchParams({ id });
    if (cwd) q.set('cwd', cwd);
    if (newroom) q.set('newroom', '1');
    const r = await fetch(`${BASE}/session-resume?${q}`, { method: 'POST' });
    if (!r.ok) return false;
    const d = (await r.json().catch(() => ({}))) as { ok?: boolean };
    return d?.ok !== false;
  } catch {
    return false;
  }
}

/** POST /session-close?idx=N — 그 방(윈도우) 닫기(거노). 마지막 윈도우는 백엔드가 거부. */
export async function closeRoom(idx: number): Promise<boolean> {
  try {
    const r = await fetch(`${BASE}/session-close?idx=${idx}`, { method: 'POST' });
    return r.ok;
  } catch {
    return false;
  }
}

/** POST /arona-close — 아로나 창을 닫고 터미널로 복귀. ModePicker 에서 'solo'
 *  선택 완료 시 호출(아로나 선택은 교실 진입이라 호출 안 함). 네이티브 미구현
 *  동안 404 → false 허용. */
export async function closeArona(): Promise<boolean> {
  try {
    const r = await fetch(`${BASE}/arona-close`, { method: 'POST' });
    return r.ok;
  } catch {
    return false;
  }
}

/** POST /mode?set=solo|god — 시작 선택 화면에서 호출. */
export async function setMode(mode: 'solo' | 'god'): Promise<boolean> {
  try {
    const r = await fetch(`${BASE}/mode?set=${mode}`, { method: 'POST' });
    return r.ok;
  } catch {
    return false;
  }
}

// 학생 생성은 백엔드(kasaterm)가 pane 생성 시 캐릭터/session-id/persona 를 직접 박는다
// (거노: /spawn 폐기, 솔로 일관). arona-ui 는 board 폴링으로 표시만 한다 — spawnAgent 제거.

/** POST /focus?surface=<id> — 카드 클릭 시 해당 pane 포커스. 유우카: 쿼리 파라미터
 *  (body 아님 — 다른 POST 와 같은 preflight 회피). encodeURIComponent 가 표준 경로.
 *  응답 {ok:true,surface_id} / {ok:false,error}. fail-soft(false 반환). */
export async function focusPane(surfaceId: string): Promise<boolean> {
  try {
    const r = await fetch(`${BASE}/focus?surface=${encodeURIComponent(surfaceId)}`, { method: 'POST' });
    if (!r.ok) return false;
    const d = (await r.json().catch(() => ({}))) as { ok?: boolean };
    return d?.ok === true;
  } catch {
    return false;
  }
}

/** POST /close-pane?surface=<id> — 학생(워커) pane 종료(close_surface→close_pane). */
export async function closeAgent(surfaceId: string): Promise<boolean> {
  if (!surfaceId) return false;
  try {
    const r = await fetch(`${BASE}/close-pane?surface=${encodeURIComponent(surfaceId)}`, { method: 'POST' });
    if (!r.ok) return false;
    const d = (await r.json().catch(() => ({}))) as { ok?: boolean };
    return d?.ok !== false;
  } catch {
    return false;
  }
}

/** POST /send?surface=<id> body:{text,submit} — 특정 pane PTY에 텍스트 주입.
 *  submit=true(기본)이면 개행 추가(제출), false면 타이핑만. fail-soft(false).
 *  persist=false(`&nopersist=1`): 모모톡 단톡방에 안 남긴다 — 학생별 대화 패널의 개인
 *  지시는 그 학생 대화에만 떠야 하는데 persist 하면 모모톡에까지 노란버블로 샜다(거노). */
export async function sendToPane(surfaceId: string, text: string, submit = true, persist = true): Promise<boolean> {
  if (!text || !surfaceId) return false;
  try {
    const q = persist ? '' : '&nopersist=1';
    const r = await fetch(`${BASE}/send?surface=${encodeURIComponent(surfaceId)}${q}`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ text, submit })
    });
    if (!r.ok) return false;
    const d = (await r.json().catch(() => ({}))) as { ok?: boolean };
    return d?.ok !== false;
  } catch {
    return false;
  }
}

/** POST /send?surface=<id>&inbox=1 — 모모톡 inbox 발신. PTY 에 주입하지 않고
 *  messages.jsonl 에 read=false 로만 적는다(거노: 모모톡은 프롬프트가 아니라 에이전트
 *  inbox). 받는 에이전트는 drain_unread 로 컨텍스트에 받고, idle 이면 god-loop nudge
 *  가 깨운다. fail-soft(false). */
export async function sendToInbox(surfaceId: string, text: string): Promise<boolean> {
  if (!text.trim() || !surfaceId) return false;
  try {
    const r = await fetch(`${BASE}/send?surface=${encodeURIComponent(surfaceId)}&inbox=1`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ text: text.trim(), submit: false })
    });
    if (!r.ok) return false;
    const d = (await r.json().catch(() => ({}))) as { ok?: boolean };
    return d?.ok !== false;
  } catch {
    return false;
  }
}

/** POST /tell-god body:{text} — 사용자 지시를 god pane 의 PTY 에 send_text+submit.
 *  백엔드가 lead 마커로 god pane 을 직접 탐색하므로 surface 파라미터 불필요.
 *  응답 {ok:true} / {ok:false,error}. fail-soft(false 반환). */
export async function sendToGod(text: string): Promise<boolean> {
  if (!text.trim()) return false;
  try {
    const r = await fetch(`${BASE}/tell-god`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ text: text.trim() })
    });
    if (!r.ok) return false;
    const d = (await r.json().catch(() => ({}))) as { ok?: boolean };
    return d?.ok !== false;
  } catch {
    return false;
  }
}

export interface SchaleState {
  credits: number;
  gold: number;
  affinity_lv: number;
  exp: number;
}

/** GET /schale-state — SCHALE OS 재화/Exp 영속 스냅샷. 파일 없으면 초기값 반환. */
export async function fetchSchaleState(): Promise<SchaleState> {
  const defaults: SchaleState = { credits: 0, gold: 0, affinity_lv: 1, exp: 0 };
  try {
    const r = await fetch(`${BASE}/schale-state`);
    if (!r.ok) return defaults;
    const d = (await r.json().catch(() => null)) as Partial<SchaleState> | null;
    if (!d) return defaults;
    return {
      credits: d.credits ?? 0,
      gold: d.gold ?? 0,
      affinity_lv: d.affinity_lv ?? 1,
      exp: d.exp ?? 0,
    };
  } catch {
    return defaults;
  }
}

export interface MessageEntry {
  id: string;
  ts: number;
  from_pane: string;
  from_name: string;
  to_pane: string;
  to_name: string;
  text: string;
  read: boolean;
}

/** GET /messages?n=N — messages.jsonl(선생님 지시 + 학생간 소통)을 캐릭터명 해석
 *  포함해 최근 N 개(ts 내림차순). 모모톡 단톡방 단일 피드 소스. fail-soft 빈 배열. */
export async function fetchMessages(n = 50): Promise<MessageEntry[]> {
  try {
    const r = await fetch(`${BASE}/messages?n=${n}`);
    if (!r.ok) return [];
    const d = (await r.json().catch(() => ({}))) as { messages?: MessageEntry[] };
    return Array.isArray(d?.messages) ? d.messages : [];
  } catch {
    return [];
  }
}

/** GET /peek?surface=<id>&lines=<n> — pane 의 보이는 화면 텍스트(모노). transcript
 *  jsonl 이 비어있을 때(claude 가 라이브 기록 안 함) 대화를 PTY 화면에서 직접
 *  파싱하는 fallback 소스. fail-soft 빈 문자열. */
export async function fetchPeek(surfaceId: string, lines = 40, ansi = false): Promise<string> {
  try {
    const q = ansi ? '&ansi=1' : '';
    const r = await fetch(`${BASE}/peek?surface=${encodeURIComponent(surfaceId)}&lines=${lines}${q}`);
    if (!r.ok) return '';
    const d = (await r.json().catch(() => ({}))) as { text?: string };
    return d?.text ?? '';
  } catch {
    return '';
  }
}

export interface ClaudeUsageWindow { utilization: number; resets_at: string; }
export interface ClaudeUsage {
  five_hour?: ClaudeUsageWindow | null;
  seven_day?: ClaudeUsageWindow | null;
  seven_day_opus?: ClaudeUsageWindow | null;
  seven_day_sonnet?: ClaudeUsageWindow | null;
}
/** GET /claude-usage — claude oauth usage(5시간/주간 사용률·리셋). 토큰 없거나 실패면 null. */
export async function fetchClaudeUsage(): Promise<ClaudeUsage | null> {
  try {
    const r = await fetch(`${BASE}/claude-usage`);
    if (!r.ok) return null;
    const d = (await r.json()) as { ok?: boolean; usage?: ClaudeUsage };
    return d?.ok ? (d.usage ?? null) : null;
  } catch {
    return null;
  }
}

/** GET /slash-commands — 디스크 스캔 동적 슬래시(스킬·커스텀·플러그인). 정적 내장 목록과 병합. */
export async function fetchSlashCommands(): Promise<{ cmd: string; desc: string }[]> {
  try {
    const r = await fetch(`${BASE}/slash-commands`);
    if (!r.ok) return [];
    const d = (await r.json().catch(() => ({}))) as { commands?: { cmd: string; desc: string }[] };
    return Array.isArray(d?.commands) ? d.commands : [];
  } catch {
    return [];
  }
}

export interface ScheduleItem {
  id: string;
  kind: 'loop' | 'cron' | 'timer';
  surface: string;
  text: string;
  interval_sec?: number;
  at_ts?: number;
  next_ts?: number;
  enabled: boolean;
  label?: string;
}

/** GET /schedule — 스케줄 목록(반복 루프·예약·타이머). fail-soft 빈 배열. */
export async function fetchSchedule(): Promise<ScheduleItem[]> {
  try {
    const r = await fetch(`${BASE}/schedule`);
    if (!r.ok) return [];
    const d = (await r.json().catch(() => ({}))) as { items?: ScheduleItem[] };
    return Array.isArray(d?.items) ? d.items : [];
  } catch {
    return [];
  }
}

/** POST /schedule — 항목 추가. */
export async function addSchedule(item: {
  kind: string; surface: string; text: string; interval_sec?: number; at_ts?: number; label?: string;
}): Promise<boolean> {
  try {
    const r = await fetch(`${BASE}/schedule`, {
      method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(item),
    });
    if (!r.ok) return false;
    const d = (await r.json().catch(() => ({}))) as { ok?: boolean };
    return d?.ok !== false;
  } catch {
    return false;
  }
}

/** POST /schedule-delete?id=<id>[&toggle=1] — 삭제 또는 enabled 토글. */
export async function deleteSchedule(id: string, toggle = false): Promise<boolean> {
  try {
    const q = `id=${encodeURIComponent(id)}${toggle ? '&toggle=1' : ''}`;
    const r = await fetch(`${BASE}/schedule-delete?${q}`, { method: 'POST' });
    return r.ok;
  } catch {
    return false;
  }
}

/** GET /sent-images?surface=<id>&n=N — 이 학생이 SendUserFile 로 보낸 이미지 경로
 *  최근 N(auto-imgopen 훅 기록). 대화창 인라인 이미지 소스. fail-soft 빈 배열. */
export async function fetchSentImages(surfaceId: string, n = 12, since?: number): Promise<string[]> {
  try {
    const sq = since ? `&since=${since}` : '';
    const r = await fetch(`${BASE}/sent-images?surface=${encodeURIComponent(surfaceId)}&n=${n}${sq}`);
    if (!r.ok) return [];
    const d = (await r.json().catch(() => ({}))) as { images?: string[] };
    return Array.isArray(d?.images) ? d.images : [];
  } catch {
    return [];
  }
}

/** GET /pane-tasks — claude TaskCreate 태스크(~/.claude/tasks/<session>/<n>.json)를
 *  pane 별로. surface 주면 그 pane 만. arona 업무 탭이 학생별 진행을 보여준다. fail-soft. */
export interface PaneTask { pane: string; id: string; subject: string; status: string }
export async function fetchPaneTasks(surface?: string): Promise<PaneTask[]> {
  try {
    const q = surface ? `?surface=${encodeURIComponent(surface)}` : '';
    const r = await fetch(`${BASE}/pane-tasks${q}`);
    if (!r.ok) return [];
    const d = (await r.json().catch(() => ({}))) as { tasks?: PaneTask[] };
    return Array.isArray(d?.tasks) ? d.tasks : [];
  } catch {
    return [];
  }
}

/** POST /paste-image?surface=<id> (body=이미지 raw 바이트) — 아로나 프롬프트 입력창에
 *  드롭한 이미지를 그 학생 claude 에 첨부(kasaterm 이 시스템 클립보드 비트맵+Ctrl+V). 성공 bool. */
export async function pasteImageToPane(surface: string, file: Blob): Promise<boolean> {
  try {
    const r = await fetch(`${BASE}/paste-image?surface=${encodeURIComponent(surface)}`, {
      method: 'POST',
      body: file,
    });
    if (!r.ok) return false;
    const d = (await r.json().catch(() => ({}))) as { ok?: boolean };
    return !!d?.ok;
  } catch {
    return false;
  }
}

/** 로컬 이미지 경로 → 백엔드 서빙 URL(/image-file). 대화창 <img src>. */
export function imageFileUrl(path: string): string {
  return `${BASE}/image-file?path=${encodeURIComponent(path)}`;
}

/** POST /open-file?path — OS 기본 뷰어(macOS Preview 등)로 파일 열기. 대화창 이미지 클릭. */
export async function openFile(path: string): Promise<boolean> {
  try {
    const r = await fetch(`${BASE}/open-file?path=${encodeURIComponent(path)}`, { method: 'POST' });
    return r.ok;
  } catch {
    return false;
  }
}

export interface DirListing { path: string; parent: string | null; dirs: string[]; }

/** GET /list-dir?path=<path> — 경로의 하위 디렉터리 목록(방 경로 변경 모달). path
 *  생략 시 active 방 cwd. parent 로 상위 이동. fail-soft. */
export async function listDir(path?: string): Promise<DirListing> {
  const fallback: DirListing = { path: path ?? '', parent: null, dirs: [] };
  try {
    const q = path ? `?path=${encodeURIComponent(path)}` : '';
    const r = await fetch(`${BASE}/list-dir${q}`);
    if (!r.ok) return fallback;
    const d = (await r.json().catch(() => ({}))) as Partial<DirListing>;
    return {
      path: d.path ?? path ?? '',
      parent: d.parent ?? null,
      dirs: Array.isArray(d.dirs) ? d.dirs : [],
    };
  } catch {
    return fallback;
  }
}

/** POST /room-cd?path=<path> — active pane 셸을 그 경로로 cd(터미널 백엔드). */
export async function roomCd(path: string): Promise<boolean> {
  try {
    const r = await fetch(`${BASE}/room-cd?path=${encodeURIComponent(path)}`, { method: 'POST' });
    if (!r.ok) return false;
    const d = (await r.json().catch(() => ({}))) as { ok?: boolean };
    return d?.ok !== false;
  } catch {
    return false;
  }
}

export interface Turn { role: string; text: string; images?: string[] }

/** 인터랙티브 도구 호출(AskUserQuestion 등) — 캡처 프록시가 SSE tool_use 에서 재구성.
 *  peek 화면 추정 없이 질문/선택지를 API 그대로(거노). */
export interface ConvToolUse {
  name: string;
  input: {
    questions?: Array<{
      question: string;
      header?: string;
      multiSelect?: boolean;
      options: Array<{ label: string; description?: string; preview?: string }>;
    }>;
  } & Record<string, unknown>;
}
export interface Conversation { turns: Turn[]; streaming: string; tool_uses?: ConvToolUse[]; model: string; effort?: string; tokens_out?: number; }

/** GET /conversation?surface=<id> — 캡처 프록시(ccglass 방식)가 claude 의 Anthropic API
 *  호출에서 가로챈 깨끗한 대화. peek(화면)·jsonl(지연) 없이 구조화·라이브. `streaming`
 *  은 진행 중 어시스턴트 응답(SSE 라이브). 프록시 안 탄 pane 은 빈 배열(폴백). */
export async function fetchConversation(surfaceId: string): Promise<Conversation> {
  try {
    const r = await fetch(`${BASE}/conversation?surface=${encodeURIComponent(surfaceId)}`);
    if (!r.ok) return { turns: [], streaming: '', model: '' };
    const d = (await r.json().catch(() => ({}))) as Partial<Conversation>;
    return {
      turns: Array.isArray(d.turns) ? d.turns : [],
      streaming: typeof d.streaming === 'string' ? d.streaming : '',
      tool_uses: Array.isArray(d.tool_uses) ? d.tool_uses : [],
      model: typeof d.model === 'string' ? d.model : '',
      effort: typeof d.effort === 'string' ? d.effort : '',
      tokens_out: typeof d.tokens_out === 'number' ? d.tokens_out : 0,
    };
  } catch {
    return { turns: [], streaming: '', model: '' };
  }
}

/** GET /transcript?surface=<id>&turns=<n> — 구조화된 대화(프롬프트/답변, tool 노이즈
 *  제거). 학생 클릭 시 raw 터미널 대신 대화 채팅뷰 소스(선생님 ②). fail-soft 빈 배열. */
export async function fetchTranscript(surfaceId: string, turns = 20): Promise<Turn[]> {
  try {
    const q = new URLSearchParams({ surface: surfaceId, turns: String(turns) });
    const r = await fetch(`${BASE}/transcript?${q}`);
    if (!r.ok) return [];
    const d = (await r.json().catch(() => ({}))) as { turns?: Turn[] };
    return Array.isArray(d?.turns) ? d.turns : [];
  } catch {
    return [];
  }
}

/** GET /transcript-raw?surface=<id> — pane 의 bound jsonl 을 raw 로 받아 SessionEvent[]
 *  로 파싱. tool_use/tool_result/structuredPatch 가 다 살아있어 ccsv per-tool 렌더가
 *  먹는다(/transcript 는 텍스트만 남김). fail-soft 빈 배열. */
export async function fetchTranscriptRaw(surfaceId: string): Promise<SessionEvent[]> {
  try {
    const r = await fetch(`${BASE}/transcript-raw?surface=${encodeURIComponent(surfaceId)}`);
    if (!r.ok) return [];
    const d = (await r.json().catch(() => ({}))) as { raw?: string };
    if (typeof d?.raw !== 'string' || !d.raw) return [];
    return parseJsonlSync(d.raw);
  } catch {
    return [];
  }
}

export interface TranscriptChunk { events: SessionEvent[]; offset: number; reset: boolean }

/** GET /transcript-raw?surface=<id>&offset=<n> — 증분 읽기(라이브 채팅뷰용). offset=0
 *  → tail 윈도(reset=true, 버퍼 통째 교체), >0 → 그 바이트 이후 append 된 **완전한 줄만**
 *  (reset=false, 뒤에 append). 안 바뀌면 events=[]·offset 그대로라 프론트가 재파싱·리렌더를
 *  통째로 건너뛴다. offset 을 누적해, 매 폴마다 수십 MB 전체를 다시 읽고 파싱하던 걸 없앤다.
 *  (offset 무시·전체 tail 1회면 기존 fetchTranscriptRaw 를 그대로 쓰면 된다.) */
export async function fetchTranscriptChunk(surfaceId: string, offset = 0): Promise<TranscriptChunk> {
  if (!surfaceId) return { events: [], offset, reset: false };
  try {
    const q = new URLSearchParams({ surface: surfaceId, offset: String(offset) });
    const r = await fetch(`${BASE}/transcript-raw?${q}`);
    if (!r.ok) return { events: [], offset, reset: false };
    const d = (await r.json().catch(() => ({}))) as { raw?: string; offset?: number; reset?: boolean };
    const raw = typeof d?.raw === 'string' ? d.raw : '';
    return {
      events: raw ? parseJsonlSync(raw) : [],
      offset: typeof d?.offset === 'number' ? d.offset : offset,
      reset: !!d?.reset,
    };
  } catch {
    return { events: [], offset, reset: false };
  }
}

/** GET /session-transcript-raw?id=<uuid>&cwd=<abs> — 과거(오프라인) 세션의 jsonl 을
 *  uuid+cwd 로 직접 읽어 SessionEvent[] 로 파싱(라이브 pane 불필요). 최근 세션 이어가기
 *  뷰어가 죽은 세션을 읽기 전용으로 미리보는 경로. fail-soft 빈 배열. */
export async function fetchSessionTranscriptRaw(id: string, cwd?: string): Promise<SessionEvent[]> {
  if (!id) return [];
  try {
    const q = new URLSearchParams({ id });
    if (cwd) q.set('cwd', cwd);
    const r = await fetch(`${BASE}/session-transcript-raw?${q}`);
    if (!r.ok) return [];
    const d = (await r.json().catch(() => ({}))) as { raw?: string };
    if (typeof d?.raw !== 'string' || !d.raw) return [];
    return parseJsonlSync(d.raw);
  } catch {
    return [];
  }
}

/** GET /subagents?surface=<id> — 그 pane 의 claude 가 소환한 서브에이전트 목록(최신순).
 *  subagents/agent-*.meta.json 에서 모은다. 메타칸 드릴인 진입 목록 소스. fail-soft. */
export async function fetchSubagents(surfaceId: string): Promise<SubagentInfo[]> {
  if (!surfaceId) return [];
  try {
    const r = await fetch(`${BASE}/subagents?surface=${encodeURIComponent(surfaceId)}`);
    if (!r.ok) return [];
    // 백엔드는 snake_case(agent_id/agent_type)로 직렬화 — camelCase 로 정규화(양쪽 키 방어).
    const d = (await r.json().catch(() => ({}))) as { subagents?: Record<string, unknown>[] };
    const arr = Array.isArray(d?.subagents) ? d.subagents : [];
    return arr.map((s) => ({
      agentId: String(s.agentId ?? s.agent_id ?? ''),
      agentType: String(s.agentType ?? s.agent_type ?? ''),
      description: String(s.description ?? ''),
      mtime: Number(s.mtime ?? 0),
    })).filter((s) => s.agentId);
  } catch {
    return [];
  }
}

/** GET /subagent-transcript-raw?surface=<id>&agentId=<id> — 한 서브에이전트의 jsonl 을
 *  raw 로 받아 SessionEvent[] 로 파싱(메인 transcript 와 동일 포맷·렌더). fail-soft. */
export async function fetchSubagentTranscriptRaw(surfaceId: string, agentId: string): Promise<SessionEvent[]> {
  if (!surfaceId || !agentId) return [];
  try {
    const q = new URLSearchParams({ surface: surfaceId, agentId });
    const r = await fetch(`${BASE}/subagent-transcript-raw?${q}`);
    if (!r.ok) return [];
    const d = (await r.json().catch(() => ({}))) as { raw?: string };
    if (typeof d?.raw !== 'string' || !d.raw) return [];
    return parseJsonlSync(d.raw);
  } catch {
    return [];
  }
}

/** POST /paste-active body:{text,submit} — 활성(포커스) 터미널 pane 에 텍스트 주입.
 *  surface 지정 없이 현재 pane 으로 간다. submit=false(기본)면 개행 없이 타이핑만 —
 *  사용자가 직접 엔터. 오프라인 세션 '현재 터미널에 입력' 버튼이 쓴다. fail-soft.
 *  body 는 raw JSON 문자열로 보내되 content-type 헤더를 안 붙인다 — application/json 은
 *  CORS preflight(OPTIONS)를 유발하고 axum post() 가 405 로 답해 죽는다(git_commit 동일 패턴). */
export async function pasteToActiveTerminal(text: string, submit = false): Promise<boolean> {
  if (!text) return false;
  try {
    const r = await fetch(`${BASE}/paste-active`, {
      method: 'POST',
      body: JSON.stringify({ text, submit }),
    });
    if (!r.ok) return false;
    const d = (await r.json().catch(() => ({}))) as { ok?: boolean };
    return d?.ok !== false;
  } catch {
    return false;
  }
}

/** 터미널 pane 의 % 배치(window_layout). BA GUI 중앙 그리드가 이걸로 터미널 split 을
 *  그대로 미러한다(position:absolute left/top/w/h %). 단일 pane 은 전체(0,0,100,100). */
export interface PaneRect {
  surface_id: string; x: number; y: number; w: number; h: number;
  /** plain(비-claude) 터미널 타일의 Warp 상태바용 — 백엔드 window_layout 이 채움. */
  cwd?: string; branch?: string; files?: number; insertions?: number; deletions?: number;
}
export async function fetchLayout(): Promise<PaneRect[]> {
  try {
    const r = await fetch(`${BASE}/layout`);
    if (!r.ok) return [];
    const d = (await r.json().catch(() => ({}))) as { panes?: PaneRect[] };
    return Array.isArray(d?.panes) ? d.panes : [];
  } catch {
    return [];
  }
}

/** /blocks 의 한 명령 블록(OSC 133 C/D 경계). plain 터미널 타일을 Warp 처럼 명령
 *  블록 스택으로 그린다. exit_code/duration_ms 없으면 실행 중. */
export interface PaneBlock {
  id: number;
  command: string;
  output: string;
  exit_code?: number;
  started_ms: number;
  duration_ms?: number;
  is_tui?: boolean;
}
export async function fetchBlocks(surfaceId: string, limit = 50): Promise<PaneBlock[]> {
  try {
    const r = await fetch(`${BASE}/blocks?surface=${encodeURIComponent(surfaceId)}&limit=${limit}`);
    if (!r.ok) return [];
    const d = (await r.json().catch(() => ({}))) as { blocks?: PaneBlock[] };
    return Array.isArray(d?.blocks) ? d.blocks : [];
  } catch {
    return [];
  }
}
