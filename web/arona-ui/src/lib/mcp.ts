import { useStore, type Agent } from '@/store';
import type { AccentColorName } from '@/design/tokens';
import type { StatusKind } from '@/components/PixelBadge';

// munder 는 electron IPC(window.cth.*)로 hive 와 통신했지만, 우리는 kasaterm 의
// kasaspace MCP HTTP(127.0.0.1:8765)를 fetch 로 폴링한다. 1차 화면은 /board 한
// 엔드포인트만 — 살아있는 pane 의 status/intent/title/tokens/is_god. spawn·
// characters·mode 등 신규 엔드포인트는 P6c(%3 작업)로 들어오면 여기 붙인다.
const BASE = 'http://127.0.0.1:8765';

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
  return {
    id: r.surface_id,
    name: r.title || r.surface_id,
    character: r.title || r.surface_id,
    accent: r.is_god ? 'lemon' : accentFor(r.surface_id),
    status: toStatus(r.status),
    project: r.intent ?? '',
    action: r.intent,
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

/** GET /mode — 'solo' | 'god' | null. 응답이 {mode} 든 평문이든 흡수. */
export async function fetchMode(): Promise<string | null> {
  try {
    const r = await fetch(`${BASE}/mode`);
    if (!r.ok) return null;
    const d = await r.json().catch(() => null);
    if (typeof d === 'string') return d;
    return d?.mode ?? d?.result?.mode ?? null;
  } catch {
    return null;
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
