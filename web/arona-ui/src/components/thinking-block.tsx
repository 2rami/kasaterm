"use client";

import { useEffect, useState } from "react";
import { Brain, ChevronRight } from "lucide-react";
import { cn, truncate } from "@/lib/utils";
import { Markdown } from "./Markdown";
import { CopyButton } from "./copy-button";
import { useSettings } from "@/lib/settings";

export function ThinkingBlock({ thinking }: { thinking: string }) {
  const { expandThinking, setExpandThinking } = useSettings();
  const [open, setOpen] = useState(expandThinking);
  // Sync with the global toggle so flipping the setting immediately
  // applies to every already-rendered block. User per-block clicks set
  // state locally and persist until the setting changes again.
  useEffect(() => {
    setOpen(expandThinking);
  }, [expandThinking]);
  return (
    <div className="my-2 rounded-md border border-border/40 bg-muted/20">
      {/* 전역 토글: 한 블록에서 켜면 store 가 모든 ThinkingBlock 을 펼친다. */}
      <label
        style={{
          display: "flex",
          alignItems: "center",
          gap: 5,
          justifyContent: "flex-end",
          padding: "3px 8px 0",
          fontSize: 10,
          fontFamily: "var(--cth-font-ui)",
          color: "var(--cth-ink-300)",
          cursor: "pointer",
          userSelect: "none",
        }}
      >
        <input
          type="checkbox"
          checked={expandThinking}
          onChange={(e) => setExpandThinking(e.target.checked)}
          style={{ accentColor: "var(--cth-sky)", width: 11, height: 11 }}
        />
        모두 펼침
      </label>
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
        className="flex w-full items-start gap-2 px-3 py-2 text-left transition-colors hover:bg-muted/40"
      >
        <ChevronRight
          aria-hidden="true"
          className={cn(
            "mt-0.5 h-3 w-3 shrink-0 text-muted-foreground transition-transform motion-reduce:transition-none",
            open && "rotate-90",
          )}
        />
        <Brain
          aria-hidden="true"
          className="mt-0.5 h-3 w-3 shrink-0 text-purple-700 dark:text-purple-400"
        />
        <div className="min-w-0 flex-1">
          <span className="text-[11px] font-medium uppercase tracking-wider text-purple-700 dark:text-purple-400">
            Thinking
          </span>
          {!open && (
            <p className="mt-0.5 line-clamp-2 text-xs italic text-muted-foreground">
              {truncate(thinking, 200)}
            </p>
          )}
        </div>
      </button>
      {open && (
        <div className="group/cb relative border-t border-border/40 px-4 py-3 text-xs italic text-muted-foreground">
          <Markdown text={thinking} />
          <CopyButton
            text={thinking}
            className="absolute right-2 top-2 bg-card/80 opacity-0 backdrop-blur transition-opacity group-hover/cb:opacity-100"
            title="Copy thinking"
          />
        </div>
      )}
    </div>
  );
}
