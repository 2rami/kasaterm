import { useStore, type Agent } from '@/store';
import type { AccentColorName } from '@/design/tokens';
import type { StatusKind } from '@/components/PixelBadge';

// munder 는 electron IPC(window.cth.*)로 hive 와 통신했지만, 우리는 kasaterm 의
// kasaspace MCP HTTP 를 fetch 로 폴링한다. dist 는 MCP 가 /arona-ui/ 로 정적
// 서빙하므로 페이지·API 가 **same-origin** → BASE='' (relative). 8765 가 점유돼
// 랜덤 포트로 폴백해도 안 끊긴다(유우카 P6c-2차). vite dev 로 띄울 때만 8765
// 절대주소(개발 서버는 5173, API 는 별도 포트라 same-origin 이 아님).
const BASE = import.meta.env.DEV ? 'http://127.0.0.1:8765' : '';

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
  /** 마지막 답변/질문 첫마디 — waiting/idle 생각 구름 텍스트 소스. board 에는
   *  waiting_for 필드가 없어, 대기 시 직전에 던진 질문/제안이 담긴 이 값을 쓴다. */
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
  cwd?: string;
  context_limit?: number;
  /** 컨텍스트 사용량 % — claude TUI 상태바 파싱(robust). */
  context_pct?: number;
  branch?: string;
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
  const display = r.character ?? (r.title || r.surface_id);
  // 현재 tool = intent 라벨 첫 토큰("Edit auth.ts"→"Edit"). tool_use 없는 turn 직후의
  // 'active' 폴백은 tool 이름이 아니므로 버린다(말풍선엔 진짜 tool명만).
  const head = r.intent ? r.intent.split(' ')[0] : '';
  const currentTool = head && head !== 'active' ? head : undefined;
  // working 인데 아직 도구를 안 쓴 turn = 응답 생성 중(거노: active 로딩중 = thinking).
  let status = toStatus(r.status);
  if (status === 'working' && !currentTool) status = 'thinking';
  return {
    id: r.surface_id,
    name: display,
    character: display,
    accent: r.is_god ? 'lemon' : accentFor(r.surface_id),
    status,
    project: r.intent ?? '',
    action: r.intent,
    lastReply: r.last_reply,
    lastPrompt: r.last_prompt,
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
    contextLimit: r.context_limit,
    contextPct: r.context_pct,
    branch: r.branch
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
  members: CharacterDef[];
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

export interface SpawnReq { character?: string; model?: string; cwd?: string; }
export interface SpawnRes { ok: boolean; surface_id?: string; command?: string; notes?: string; }

/** POST /spawn{character?,model?,cwd?} — 학생(워커) 기동. cwd 빈 문자열은 키 자체
 *  omit(유우카: cwd:"" 는 절대경로 검증에서 거부). 응답 {ok,surface_id,command,notes}. */
export async function spawnAgent(req: SpawnReq): Promise<SpawnRes> {
  const body: SpawnReq = {};
  if (req.character) body.character = req.character;
  if (req.model) body.model = req.model;
  if (req.cwd && req.cwd.trim()) body.cwd = req.cwd.trim();
  try {
    const r = await fetch(`${BASE}/spawn`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(body)
    });
    const d = (await r.json().catch(() => ({}))) as SpawnRes;
    return { ok: r.ok && d.ok !== false, surface_id: d.surface_id, command: d.command, notes: d.notes };
  } catch (e) {
    return { ok: false, notes: String(e) };
  }
}

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
 *  submit=true(기본)이면 개행 추가(제출), false면 타이핑만. fail-soft(false). */
export async function sendToPane(surfaceId: string, text: string, submit = true): Promise<boolean> {
  if (!text || !surfaceId) return false;
  try {
    const r = await fetch(`${BASE}/send?surface=${encodeURIComponent(surfaceId)}`, {
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
export async function fetchPeek(surfaceId: string, lines = 40): Promise<string> {
  try {
    const r = await fetch(`${BASE}/peek?surface=${encodeURIComponent(surfaceId)}&lines=${lines}`);
    if (!r.ok) return '';
    const d = (await r.json().catch(() => ({}))) as { text?: string };
    return d?.text ?? '';
  } catch {
    return '';
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
export async function fetchSentImages(surfaceId: string, n = 12): Promise<string[]> {
  try {
    const r = await fetch(`${BASE}/sent-images?surface=${encodeURIComponent(surfaceId)}&n=${n}`);
    if (!r.ok) return [];
    const d = (await r.json().catch(() => ({}))) as { images?: string[] };
    return Array.isArray(d?.images) ? d.images : [];
  } catch {
    return [];
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

export interface Turn { role: string; text: string; }

export interface Conversation { turns: Turn[]; streaming: string; model: string; }

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
      model: typeof d.model === 'string' ? d.model : '',
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
