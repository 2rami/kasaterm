"use client";

import type { ToolRenderer, ToolStat } from "./index";
import { formatDuration, truncate } from "@/lib/utils";

export const GlobRenderer: ToolRenderer = {
  stats(_input, tur) {
    const r = tur as { numFiles?: number; durationMs?: number; truncated?: boolean } | null;
    const out: ToolStat[] = [];
    if (r?.numFiles != null) out.push({ label: "files", value: String(r.numFiles) });
    if (r?.durationMs != null) out.push({ label: "took", value: formatDuration(r.durationMs), tone: "muted" });
    if (r?.truncated) out.push({ label: "", value: "truncated", tone: "warn" });
    return out;
  },
  summary(input) {
    const i = input as { pattern?: string; path?: string };
    return truncate(`${i?.pattern || ""}  in  ${i?.path || "."}`, 120);
  },
  inputView(input) {
    const i = input as { pattern?: string; path?: string };
    return (
      <div className="px-3 py-2 font-mono text-[11px]">
        <div>
          <span className="text-muted-foreground">pattern: </span>
          <span className="text-foreground">{i.pattern}</span>
        </div>
        {i.path && (
          <div>
            <span className="text-muted-foreground">path: </span>
            <span className="text-foreground">{i.path}</span>
          </div>
        )}
      </div>
    );
  },
};
