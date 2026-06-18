import type { SessionEvent } from "./types";

// ccsv conversation-view.tsx 의 buildToolMap 발췌(순수 함수). jsonl SessionEvent[]
// 를 tool_use_id → { toolUse, toolResult, attachments[], toolUseResult } 로 페어링.
// structuredPatch 등 구조화 결과는 user 이벤트 top-level 의 toolUseResult 에 들어있다.
export function buildToolMap(events: SessionEvent[]) {
  const map = new Map<
    string,
    {
      toolUse: { name?: string; id?: string; input?: unknown } | null;
      toolResult: { content?: unknown; is_error?: boolean } | null;
      toolUseResult: unknown;
      attachments: SessionEvent[];
    }
  >();
  const ensure = (id: string) => {
    if (!map.has(id))
      map.set(id, {
        toolUse: null,
        toolResult: null,
        toolUseResult: null,
        attachments: [],
      });
    return map.get(id)!;
  };
  for (const ev of events) {
    if (ev.type === "assistant" || ev.type === "user") {
      const content = (ev as { message?: { content?: unknown } }).message
        ?.content;
      let lastToolResultId: string | undefined;
      if (Array.isArray(content)) {
        for (const block of content) {
          if (!block || typeof block !== "object") continue;
          const b = block as {
            type?: string;
            id?: string;
            tool_use_id?: string;
            name?: string;
            input?: unknown;
            content?: unknown;
            is_error?: boolean;
          };
          if (b.type === "tool_use" && b.id) {
            const e = ensure(b.id);
            e.toolUse = { id: b.id, name: b.name, input: b.input };
          } else if (b.type === "tool_result" && b.tool_use_id) {
            const e = ensure(b.tool_use_id);
            e.toolResult = { content: b.content, is_error: b.is_error };
            lastToolResultId = b.tool_use_id;
          }
        }
      }
      // Structured tool result lives at the user-event top level.
      if (ev.type === "user") {
        const tur = (ev as { toolUseResult?: unknown }).toolUseResult;
        const stid =
          (ev as { sourceToolUseID?: string }).sourceToolUseID ||
          lastToolResultId;
        if (tur != null && stid) {
          const e = ensure(stid);
          e.toolUseResult = tur;
        }
      }
    } else if (ev.type === "attachment") {
      const id = (ev as { attachment?: { toolUseID?: string } }).attachment
        ?.toolUseID;
      if (id) {
        const e = ensure(id);
        e.attachments.push(ev);
      }
    }
  }
  return map;
}

export type ToolMap = ReturnType<typeof buildToolMap>;
