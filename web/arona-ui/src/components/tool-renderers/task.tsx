"use client";

import type { ToolRenderer, ToolStat } from "./index";
import { formatDuration, formatTokens, parseBackgroundRef, truncate } from "@/lib/utils";
import { Markdown } from "../Markdown";
import { CopyButton } from "../copy-button";
import { BackgroundOutput } from "../background-output";

export const TaskRenderer: ToolRenderer = {
  stats(_input, tur) {
    const r = tur as
      | {
          status?: string;
          agentType?: string;
          totalDurationMs?: number;
          totalTokens?: number;
          totalToolUseCount?: number;
        }
      | null;
    const out: ToolStat[] = [];
    if (r?.status) {
      out.push({
        label: "",
        value: r.status,
        tone:
          r.status === "completed"
            ? "ok"
            : r.status === "failed" || r.status === "error"
              ? "danger"
              : "info",
      });
    }
    if (r?.totalToolUseCount != null) out.push({ label: "tools", value: String(r.totalToolUseCount) });
    if (r?.totalTokens != null) out.push({ label: "tok", value: formatTokens(r.totalTokens), tone: "muted" });
    if (r?.totalDurationMs != null) out.push({ label: "took", value: formatDuration(r.totalDurationMs), tone: "muted" });
    return out;
  },
  summary(input) {
    const i = input as { description?: string; subagent_type?: string };
    const tag = i?.subagent_type ? `(${i.subagent_type}) ` : "";
    return truncate(`${tag}${i?.description || "(sub-agent)"}`, 140);
  },
  inputView(input) {
    const i = input as {
      description?: string;
      subagent_type?: string;
      prompt?: string;
      isolation?: string;
      model?: string;
      run_in_background?: boolean;
    };
    return (
      <div className="space-y-2 px-3 py-2 text-[11px]">
        <div className="flex flex-wrap gap-3 font-mono">
          {i.subagent_type && (
            <span>
              <span className="text-muted-foreground">subagent: </span>
              <span className="text-purple-700 dark:text-purple-400">{i.subagent_type}</span>
            </span>
          )}
          {i.model && (
            <span>
              <span className="text-muted-foreground">model: </span>
              {i.model}
            </span>
          )}
          {i.isolation && (
            <span>
              <span className="text-muted-foreground">isolation: </span>
              {i.isolation}
            </span>
          )}
          {i.run_in_background && (
            <span className="text-amber-700 dark:text-amber-400">background</span>
          )}
        </div>
        {i.description && (
          <div className="font-medium">{i.description}</div>
        )}
        {i.prompt && (
          <div className="group/cb relative">
            <pre className="max-h-[400px] overflow-auto rounded bg-background p-3 font-mono text-[11px] leading-relaxed text-foreground/85 whitespace-pre-wrap">
              {i.prompt}
            </pre>
            <CopyButton
              text={i.prompt}
              className="absolute right-2 top-2 bg-card/80 opacity-0 backdrop-blur transition-opacity group-hover/cb:opacity-100"
              title="Copy prompt"
            />
          </div>
        )}
      </div>
    );
  },
  // Sub-agent final response is usually markdown — render it that way
  // instead of the default <pre> dump. Background-mode invocations get a
  // BackgroundOutput section so the actual log is visible inline.
  resultView(result, isError) {
    if (!result) {
      return (
        <div className="px-3 py-2 text-xs italic text-muted-foreground">
          (no result)
        </div>
      );
    }
    const bg = parseBackgroundRef(result);
    if (isError) {
      return (
        <div>
          <pre className="max-h-[500px] overflow-auto rounded bg-background p-3 font-mono text-[11px] leading-relaxed text-red-700 dark:text-red-300 whitespace-pre-wrap">
            {result}
          </pre>
          {bg && <BackgroundOutput taskId={bg.taskId} path={bg.path} />}
        </div>
      );
    }
    return (
      <div className="group/cb relative rounded bg-background p-3">
        <div className="max-h-[600px] overflow-auto pr-6">
          <Markdown text={result} />
        </div>
        <CopyButton
          text={result}
          className="absolute right-2 top-2 bg-card/80 opacity-0 backdrop-blur transition-opacity group-hover/cb:opacity-100"
          title="Copy response"
        />
        {bg && <BackgroundOutput taskId={bg.taskId} path={bg.path} />}
      </div>
    );
  },
};
