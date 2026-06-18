"use client";

import type { ToolRenderer, ToolStat } from "./index";
import { CopyButton } from "../copy-button";

export const ReadRenderer: ToolRenderer = {
  stats(_input, tur) {
    const r = tur as { file?: { numLines?: number; totalLines?: number; startLine?: number } } | null;
    const out: ToolStat[] = [];
    if (r?.file?.numLines != null) {
      const tl = r.file.totalLines;
      out.push({
        label: "lines",
        value:
          tl && tl !== r.file.numLines
            ? `${r.file.numLines}/${tl}`
            : `${r.file.numLines}`,
      });
    }
    return out;
  },
  summary(input) {
    const i = input as { file_path?: string; offset?: number; limit?: number };
    if (!i?.file_path) return "(no file)";
    const range =
      i.offset || i.limit ? ` [${i.offset ?? 0}..${i.limit ?? "EOF"}]` : "";
    return `${i.file_path}${range}`;
  },
  inputView(input) {
    const i = input as { file_path?: string; offset?: number; limit?: number; pages?: string };
    return (
      <div className="space-y-1 px-3 py-2 font-mono text-[11px]">
        <div className="group/cb flex items-center gap-1">
          <span className="text-muted-foreground">file: </span>
          <span className="text-brand">{i.file_path}</span>
          {i.file_path && (
            <CopyButton
              text={i.file_path}
              size="xs"
              className="opacity-0 transition-opacity group-hover/cb:opacity-100"
              title="Copy path"
            />
          )}
        </div>
        {i.offset != null && (
          <div>
            <span className="text-muted-foreground">offset: </span>
            {i.offset}
          </div>
        )}
        {i.limit != null && (
          <div>
            <span className="text-muted-foreground">limit: </span>
            {i.limit}
          </div>
        )}
        {i.pages && (
          <div>
            <span className="text-muted-foreground">pages: </span>
            {i.pages}
          </div>
        )}
      </div>
    );
  },
  resultView(result, isError) {
    const text = result || "";
    return (
      <div className="group/cb relative">
        <pre
          className={[
            "max-h-[600px] overflow-auto rounded bg-background p-3 font-mono text-[11px] leading-relaxed",
            isError ? "text-red-700 dark:text-red-300" : "text-foreground/85",
          ].join(" ")}
        >
          {text || "(empty)"}
        </pre>
        {text && (
          <CopyButton
            text={text}
            className="absolute right-2 top-2 bg-card/80 opacity-0 backdrop-blur transition-opacity group-hover/cb:opacity-100"
            title="Copy file content"
          />
        )}
      </div>
    );
  },
};
