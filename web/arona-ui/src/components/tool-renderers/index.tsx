"use client";

import { CopyButton } from "../copy-button";
import { extractText, stringifyToolInput, truncate } from "@/lib/utils";

export interface ToolStat {
  label: string;
  value: string;
  tone?: "muted" | "ok" | "warn" | "danger" | "info";
}

export interface ToolRenderer {
  /** One-line label for the collapsed card. */
  summary: (input: unknown) => string;
  /** Expanded view; receives input + result text. */
  body?: (input: unknown, result: string, isError: boolean) => React.ReactNode;
  /** Optional input view override. */
  inputView?: (input: unknown) => React.ReactNode;
  /** Optional result view override. */
  resultView?: (result: string, isError: boolean) => React.ReactNode;
  /** Chips derived from the structured toolUseResult — shown in the card header. */
  stats?: (input: unknown, toolUseResult: unknown) => ToolStat[];
  /** Optional structured-result expanded section. */
  resultExtraView?: (toolUseResult: unknown) => React.ReactNode;
}

export const defaultResultView = (
  result: string,
  isError: boolean,
): React.ReactNode => {
  if (!result) {
    return (
      <div className="px-3 py-2 text-xs italic text-muted-foreground">
        (no result)
      </div>
    );
  }
  return (
    <div className="group/cb relative">
      <pre
        className={[
          "max-h-[400px] overflow-auto rounded bg-background p-3 font-mono text-[11px] leading-relaxed",
          isError ? "text-red-700 dark:text-red-300" : "text-foreground/85",
        ].join(" ")}
      >
        {result}
      </pre>
      <CopyButton
        text={result}
        className="absolute right-2 top-2 bg-card/80 opacity-0 backdrop-blur transition-opacity group-hover/cb:opacity-100"
        title="Copy result"
      />
    </div>
  );
};

export const defaultInputView = (input: unknown): React.ReactNode => {
  const text = stringifyToolInput(input);
  return (
    <div className="group/cb relative">
      <pre className="max-h-[400px] overflow-auto rounded bg-background p-3 font-mono text-[11px] leading-relaxed text-foreground/85">
        {text}
      </pre>
      <CopyButton
        text={text}
        className="absolute right-2 top-2 bg-card/80 opacity-0 backdrop-blur transition-opacity group-hover/cb:opacity-100"
        title="Copy input"
      />
    </div>
  );
};

import { BashRenderer } from "./bash";
import { ReadRenderer } from "./read";
import { EditRenderer } from "./edit";
import { WriteRenderer } from "./write";
import { GrepRenderer } from "./grep";
import { GlobRenderer } from "./glob";
import { TaskRenderer } from "./task";
import { TodoWriteRenderer } from "./todo";
import { WebFetchRenderer, WebSearchRenderer } from "./web";

const REGISTRY: Record<string, ToolRenderer> = {
  Bash: BashRenderer,
  Read: ReadRenderer,
  Edit: EditRenderer,
  MultiEdit: EditRenderer,
  Write: WriteRenderer,
  NotebookEdit: WriteRenderer,
  Grep: GrepRenderer,
  Glob: GlobRenderer,
  Task: TaskRenderer,
  Agent: TaskRenderer,
  TodoWrite: TodoWriteRenderer,
  WebFetch: WebFetchRenderer,
  WebSearch: WebSearchRenderer,
  // Codex tools — borrow Claude renderers; real name shown on the card.
  exec_command: BashRenderer,
  write_stdin: BashRenderer,
  apply_patch: EditRenderer,
};

const FALLBACK: ToolRenderer = {
  summary: (input) => {
    const text = stringifyToolInput(input);
    return truncate(text.replace(/\s+/g, " "), 90);
  },
};

export function getRenderer(name: string | undefined): ToolRenderer {
  if (!name) return FALLBACK;
  return REGISTRY[name] || FALLBACK;
}

export { extractText };
