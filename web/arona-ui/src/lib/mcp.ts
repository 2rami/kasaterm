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
  // character 마커가 실리면 캐릭터명(아로나/시로코/아리스…)으로, 없으면 필드 자체가
  // 빠지므로 ?? 로 title(ai-title) 폴백(유우카 권장 — 빈문자 아닌 미존재 분기).
  const display = r.character ?? (r.title || r.surface_id);
  return {
    id: r.surface_id,
    name: display,
    character: display,
    accent: r.is_god ? 'lemon' : accentFor(r.surface_id),
    status: toStatus(r.status),
    project: r.intent ?? '',
    action: r.intent,
    lastReply: r.last_reply,
    isGod: !!r.is_god,
    contextTokens: tokens > 0 ? tokens : undefined
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

/** GET /peek?surface=<id>&lines=<n> — 그 pane 의 보이는 화면 텍스트(모노). 교실
 *  터미널 뷰 패널이 1s 폴링해 학생이 지금 뭘 보고 있는지 그대로 띄운다.
 *  응답 {ok,surface_id,text}. 실패 시 빈 문자열(fail-soft). */
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
