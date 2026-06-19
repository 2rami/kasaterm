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

interface PatchHunk {
  oldStart?: number;
  newStart?: number;
  lines?: string[];
}

type HunkLine = {
  sign: "+" | "-" | " ";
  oldNo?: number;
  newNo?: number;
  content: string;
};

// ccsv convertPatchToHunks: walk a structuredPatch hunk assigning real old/new
// line numbers per sign so the gutter matches the actual file, unlike a
// recomputed diffLines that always starts at 1.
function convertPatch(hunks: PatchHunk[]): HunkLine[][] {
  return hunks.map((hunk) => {
    let oldLine = hunk.oldStart ?? 1;
    let newLine = hunk.newStart ?? 1;
    return (hunk.lines ?? []).map((line): HunkLine => {
      const sign = line[0];
      const content = line.slice(1);
      if (sign === "-") return { sign: "-", oldNo: oldLine++, content };
      if (sign === "+") return { sign: "+", newNo: newLine++, content };
      return { sign: " ", oldNo: oldLine++, newNo: newLine++, content };
    });
  });
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
  resultView(result, isError, tur) {
    const patch = (tur as { structuredPatch?: PatchHunk[] } | null)
      ?.structuredPatch;
    // Real applied patch carries file line numbers + context — prefer it over
    // the input-side diffLines fallback (which only knows the changed slice).
    if (!isError && Array.isArray(patch) && patch.length > 0) {
      const filePath = (tur as { filePath?: string }).filePath;
      const hunks = convertPatch(patch);
      return <HunkDiffView filePath={filePath} hunks={hunks} />;
    }
    if (!result) {
      return (
        <div className="px-3 py-2 text-xs italic text-muted-foreground">
          (no result)
        </div>
      );
    }
    return (
      <pre
        className={[
          "max-h-[400px] overflow-auto rounded bg-background p-3 font-mono text-[11px] leading-relaxed",
          isError ? "text-red-700 dark:text-red-300" : "text-foreground/85",
        ].join(" ")}
      >
        {result}
      </pre>
    );
  },
};

function HunkDiffView({
  filePath,
  hunks,
}: {
  filePath?: string;
  hunks: HunkLine[][];
}) {
  let added = 0,
    deleted = 0;
  for (const h of hunks)
    for (const l of h) {
      if (l.sign === "+") added++;
      else if (l.sign === "-") deleted++;
    }
  return (
    <div className="overflow-hidden rounded border border-border/40 bg-background">
      <div className="flex items-center gap-3 border-b border-border/40 bg-muted/30 px-3 py-1 font-mono text-[10px]">
        <span className="min-w-0 flex-1 truncate text-muted-foreground" title={filePath}>
          {filePath ?? "patch"}
        </span>
        {added > 0 && <span className="text-emerald-700 dark:text-emerald-400">+{added}</span>}
        {deleted > 0 && <span className="text-red-700 dark:text-red-400">−{deleted}</span>}
      </div>
      <div className="max-h-[400px] overflow-auto font-mono text-[11px] leading-relaxed">
        {hunks.map((lines, hi) => (
          <div key={hi} className={hi > 0 ? "border-t border-border/30" : undefined}>
            {lines.map((l, li) => (
              <div
                key={li}
                className={[
                  "flex",
                  l.sign === "+"
                    ? "bg-emerald-500/10"
                    : l.sign === "-"
                      ? "bg-red-500/10"
                      : "",
                ].join(" ")}
              >
                <span className="w-10 shrink-0 select-none border-r border-border/30 px-1 text-right text-muted-foreground/40 tabular-nums">
                  {l.oldNo ?? " "}
                </span>
                <span className="w-10 shrink-0 select-none border-r border-border/30 px-1 text-right text-muted-foreground/40 tabular-nums">
                  {l.newNo ?? " "}
                </span>
                <span
                  className={[
                    "w-4 shrink-0 select-none text-center",
                    l.sign === "+"
                      ? "text-emerald-700 dark:text-emerald-400"
                      : l.sign === "-"
                        ? "text-red-700 dark:text-red-400"
                        : "text-muted-foreground/40",
                  ].join(" ")}
                >
                  {l.sign === " " ? " " : l.sign}
                </span>
                <span
                  className={[
                    "min-w-0 flex-1 whitespace-pre px-1",
                    l.sign === "+"
                      ? "text-emerald-800 dark:text-emerald-300"
                      : l.sign === "-"
                        ? "text-red-800 dark:text-red-300"
                        : "text-foreground/80",
                  ].join(" ")}
                >
                  {l.content || " "}
                </span>
              </div>
            ))}
          </div>
        ))}
      </div>
    </div>
  );
}

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
