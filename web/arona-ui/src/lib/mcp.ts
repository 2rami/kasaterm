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
