import type { SessionEvent } from "./types";

// 브라우저(arona)에서 도는 순수 파서. ccsv 원본의 fs 기반 readJsonl 은 제거하고
// raw jsonl 텍스트를 받는 parseJsonlSync 만 남긴다 — kasa-mcp 가 raw 를 준다.
export function parseJsonlSync(text: string): SessionEvent[] {
  const out: SessionEvent[] = [];
  for (const line of text.split("\n")) {
    if (!line.trim()) continue;
    try {
      out.push(JSON.parse(line) as SessionEvent);
    } catch {
      // skip
    }
  }
  return out;
}
