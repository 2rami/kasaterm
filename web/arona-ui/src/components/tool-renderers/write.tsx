"use client";

import type { ToolRenderer } from "./index";
import { CopyButton } from "../copy-button";

export const WriteRenderer: ToolRenderer = {
  summary(input) {
    const i = input as { file_path?: string; content?: string };
    const lines = (i?.content || "").split("\n").length;
    return `${i?.file_path || "(unknown)"} · ${lines} lines`;
  },
  inputView(input) {
    const i = input as { file_path?: string; content?: string };
    const content = i.content || "";
    return (
      <div className="space-y-2">
        <div className="group/cb flex items-center gap-1 px-3 pt-2 font-mono text-[11px]">
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
        <div className="group/cb relative">
          <pre className="max-h-[500px] overflow-auto rounded border border-border/40 bg-background p-3 font-mono text-[11px] leading-relaxed text-emerald-900 dark:text-emerald-200/80">
            {content}
          </pre>
          {content && (
            <CopyButton
              text={content}
              className="absolute right-2 top-2 bg-card/80 opacity-0 backdrop-blur transition-opacity group-hover/cb:opacity-100"
              title="Copy content"
            />
          )}
        </div>
      </div>
    );
  },
};
