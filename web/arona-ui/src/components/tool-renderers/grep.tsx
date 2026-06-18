"use client";

import type { ToolRenderer, ToolStat } from "./index";
import { truncate } from "@/lib/utils";

export const GrepRenderer: ToolRenderer = {
  stats(_input, tur) {
    const r = tur as { numFiles?: number; mode?: string } | null;
    const out: ToolStat[] = [];
    if (r?.numFiles != null) out.push({ label: "matches", value: String(r.numFiles) });
    if (r?.mode && r.mode !== "files_with_matches") out.push({ label: "mode", value: r.mode, tone: "muted" });
    return out;
  },
  summary(input) {
    const i = input as { pattern?: string; path?: string; glob?: string };
    const where = i?.path || i?.glob || ".";
    return truncate(`/${i?.pattern || ""}/  in  ${where}`, 120);
  },
  inputView(input) {
    const i = input as Record<string, unknown>;
    return (
      <div className="space-y-1 px-3 py-2 font-mono text-[11px]">
        {Object.entries(i).map(([k, v]) => (
          <div key={k}>
            <span className="text-muted-foreground">{k}: </span>
            <span className="text-foreground">
              {typeof v === "string" ? v : JSON.stringify(v)}
            </span>
          </div>
        ))}
      </div>
    );
  },
};
