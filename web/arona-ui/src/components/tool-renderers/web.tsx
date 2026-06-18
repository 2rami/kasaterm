"use client";

import type { ToolRenderer, ToolStat } from "./index";
import { formatBytes, formatDuration, truncate } from "@/lib/utils";

export const WebFetchRenderer: ToolRenderer = {
  stats(_input, tur) {
    const r = tur as
      | { code?: number; codeText?: string; bytes?: number; durationMs?: number }
      | null;
    const out: ToolStat[] = [];
    if (r?.code != null) {
      out.push({
        label: "",
        value: `${r.code}${r.codeText ? " " + r.codeText : ""}`,
        tone: r.code >= 400 ? "danger" : r.code >= 300 ? "warn" : "ok",
      });
    }
    if (r?.bytes != null) out.push({ label: "size", value: formatBytes(r.bytes), tone: "muted" });
    if (r?.durationMs != null) out.push({ label: "took", value: formatDuration(r.durationMs), tone: "muted" });
    return out;
  },
  summary(input) {
    const i = input as { url?: string; prompt?: string };
    return truncate(i?.url || "(no url)", 120);
  },
  inputView(input) {
    const i = input as { url?: string; prompt?: string };
    return (
      <div className="space-y-2 px-3 py-2 font-mono text-[11px]">
        <div>
          <span className="text-muted-foreground">url: </span>
          <a
            href={i.url}
            target="_blank"
            rel="noreferrer"
            className="text-brand underline-offset-2 hover:underline"
          >
            {i.url}
          </a>
        </div>
        {i.prompt && (
          <div>
            <span className="text-muted-foreground">prompt: </span>
            <span className="text-foreground">{i.prompt}</span>
          </div>
        )}
      </div>
    );
  },
};

export const WebSearchRenderer: ToolRenderer = {
  stats(_input, tur) {
    const r = tur as { results?: unknown[]; durationSeconds?: number } | null;
    const out: ToolStat[] = [];
    if (Array.isArray(r?.results)) {
      out.push({ label: "results", value: String(r.results.length) });
    }
    if (r?.durationSeconds != null) {
      out.push({ label: "took", value: formatDuration(r.durationSeconds * 1000), tone: "muted" });
    }
    return out;
  },
  summary(input) {
    const i = input as { query?: string };
    return truncate(i?.query || "", 120);
  },
};
