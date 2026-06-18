"use client";

import type { ToolRenderer } from "./index";
import { parseBackgroundRef, truncate } from "@/lib/utils";
import { CopyButton } from "../copy-button";
import { BackgroundOutput } from "../background-output";

export const BashRenderer: ToolRenderer = {
  summary(input) {
    const cmd = (input as { command?: string })?.command || "";
    return truncate(cmd, 140);
  },
  inputView(input) {
    const i = (input as { command?: string; description?: string; timeout?: number; run_in_background?: boolean }) || {};
    const cmd = i.command || "";
    return (
      <div className="space-y-2">
        {i.description && (
          <div className="px-3 pt-2 text-[11px] italic text-muted-foreground">
            {i.description}
          </div>
        )}
        <div className="group/cb relative">
          <pre className="overflow-x-auto rounded bg-background p-3 font-mono text-[11px] leading-relaxed">
            <span
              aria-hidden="true"
              className="select-none text-emerald-700 dark:text-emerald-400"
            >
              {"$ "}
            </span>
            <span className="text-foreground" translate="no">
              {cmd}
            </span>
          </pre>
          <CopyButton
            text={cmd}
            className="absolute right-2 top-2 bg-card/80 opacity-0 backdrop-blur transition-opacity group-hover/cb:opacity-100"
            title="Copy command"
          />
        </div>
        {(i.timeout || i.run_in_background) && (
          <div className="flex gap-3 px-3 pb-1 font-mono text-[10px] text-muted-foreground">
            {i.timeout && <span>timeout: {i.timeout}ms</span>}
            {i.run_in_background && <span>background: true</span>}
          </div>
        )}
      </div>
    );
  },
  resultView(result, isError) {
    const text = result || "";
    const bg = parseBackgroundRef(text);
    return (
      <div className="group/cb relative">
        <pre
          className={[
            "max-h-[500px] overflow-auto rounded bg-background p-3 font-mono text-[11px] leading-relaxed whitespace-pre-wrap",
            isError
              ? "text-red-700 dark:text-red-300"
              : "text-emerald-800 dark:text-emerald-200/90",
          ].join(" ")}
        >
          {text || "(no output)"}
        </pre>
        {text && (
          <CopyButton
            text={text}
            className="absolute right-2 top-2 bg-card/80 opacity-0 backdrop-blur transition-opacity group-hover/cb:opacity-100"
            title="Copy output"
          />
        )}
        {bg && <BackgroundOutput taskId={bg.taskId} path={bg.path} />}
      </div>
    );
  },
};
