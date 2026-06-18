"use client";

import { diffLines } from "diff";
import type { ToolRenderer, ToolStat } from "./index";
import { truncate } from "@/lib/utils";
import { CopyButton } from "../copy-button";

interface EditInput {
  file_path?: string;
  old_string?: string;
  new_string?: string;
  replace_all?: boolean;
  edits?: Array<{
    old_string?: string;
    new_string?: string;
    replace_all?: boolean;
  }>;
}

export const EditRenderer: ToolRenderer = {
  stats(_input, tur) {
    const r = tur as
      | {
          userModified?: boolean;
          structuredPatch?: Array<{
            oldLines?: number;
            newLines?: number;
            lines?: string[];
          }>;
        }
      | null;
    const out: ToolStat[] = [];
    if (r?.userModified) {
      out.push({ label: "user", value: "modified", tone: "warn" });
    }
    if (r?.structuredPatch && Array.isArray(r.structuredPatch)) {
      let add = 0,
        del = 0;
      for (const hunk of r.structuredPatch) {
        if (Array.isArray(hunk.lines)) {
          for (const l of hunk.lines) {
            if (l.startsWith("+")) add++;
            else if (l.startsWith("-")) del++;
          }
        }
      }
      if (add || del) {
        out.push({ label: "diff", value: `+${add} −${del}`, tone: "info" });
      }
    }
    return out;
  },
  summary(input) {
    const i = input as EditInput;
    const path = i?.file_path || "(unknown)";
    if (i?.edits) {
      return `${path} · ${i.edits.length} edits`;
    }
    const oldLen = i?.old_string?.length ?? 0;
    const newLen = i?.new_string?.length ?? 0;
    return `${path}  −${oldLen} +${newLen}${i?.replace_all ? " (all)" : ""}`;
  },
  inputView(input) {
    const i = input as EditInput;
    const edits =
      i.edits ||
      (i.old_string != null && i.new_string != null
        ? [
            {
              old_string: i.old_string,
              new_string: i.new_string,
              replace_all: i.replace_all,
            },
          ]
        : []);
    return (
      <div className="space-y-3">
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
        {edits.map((e, idx) => (
          <DiffView
            key={idx}
            oldStr={e.old_string ?? ""}
            newStr={e.new_string ?? ""}
            replaceAll={e.replace_all}
          />
        ))}
      </div>
    );
  },
};

function DiffView({
  oldStr,
  newStr,
  replaceAll,
}: {
  oldStr: string;
  newStr: string;
  replaceAll?: boolean;
}) {
  const parts = diffLines(oldStr, newStr);
  return (
    <div className="group/cb relative overflow-hidden rounded border border-border/40 bg-background">
      <div className="absolute right-1 top-1 z-10 flex gap-1 opacity-0 transition-opacity group-hover/cb:opacity-100">
        <CopyButton text={oldStr} className="bg-card/80 backdrop-blur" title="Copy old" label="old" size="xs" />
        <CopyButton text={newStr} className="bg-card/80 backdrop-blur" title="Copy new" label="new" size="xs" />
      </div>
      {replaceAll && (
        <div className="border-b border-border/40 bg-muted/30 px-3 py-1 font-mono text-[10px] text-muted-foreground">
          replace_all
        </div>
      )}
      <pre className="overflow-x-auto font-mono text-[11px] leading-relaxed">
        {parts.map((p, i) => {
          const lines = p.value.split("\n");
          // remove trailing empty caused by terminal newline
          if (lines[lines.length - 1] === "") lines.pop();
          return (
            <span key={i}>
              {lines.map((line, li) => (
                <span
                  key={li}
                  className={[
                    "block px-3 py-px",
                    p.added
                      ? "bg-emerald-500/15 text-emerald-800 dark:text-emerald-300"
                      : p.removed
                        ? "bg-red-500/15 text-red-800 dark:text-red-300"
                        : "text-muted-foreground/80",
                  ].join(" ")}
                >
                  <span className="mr-2 select-none text-muted-foreground/40">
                    {p.added ? "+" : p.removed ? "−" : " "}
                  </span>
                  {line || " "}
                </span>
              ))}
            </span>
          );
        })}
      </pre>
    </div>
  );
}

// keep linter happy
export const _truncate = truncate;
