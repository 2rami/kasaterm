import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

/**
 * Escape a string for safe use inside a CSS attribute selector. Used when
 * looking up DOM elements by uuid that came from the URL — otherwise a
 * crafted ?e=foo"] would throw DOMException on querySelector.
 */
export function cssEscape(s: string): string {
  if (typeof CSS !== "undefined" && (CSS as { escape?: (s: string) => string }).escape) {
    return (CSS as { escape: (s: string) => string }).escape(s);
  }
  return s.replace(/[^a-zA-Z0-9_-]/g, (c) => `\\${c}`);
}

export function formatRelative(ts: string | null | undefined): string {
  if (!ts) return "";
  const d = new Date(ts);
  const diffMs = Date.now() - d.getTime();
  const sec = Math.floor(diffMs / 1000);
  if (sec < 60) return `${sec}s ago`;
  const min = Math.floor(sec / 60);
  if (min < 60) return `${min}m ago`;
  const hr = Math.floor(min / 60);
  if (hr < 24) return `${hr}h ago`;
  const day = Math.floor(hr / 24);
  if (day < 30) return `${day}d ago`;
  return d.toLocaleDateString();
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

export function truncate(s: string, n: number): string {
  if (!s) return "";
  if (s.length <= n) return s;
  return s.slice(0, n) + "…";
}

/**
 * Detect Claude Code's "background task started" tool_result text.
 *
 * The shape is something like:
 *   Command running in background with ID: bt23chx3y. Output is being written to:
 *   /tmp/claude-1016/-home-eric-lee/<session>/tasks/bt23chx3y.output
 *
 * Returns { taskId, path } if matched; null otherwise.
 */
export function parseBackgroundRef(
  text: string,
): { taskId: string; path: string } | null {
  if (!text) return null;
  // Anchor on the .output suffix so the trailing sentence separator (".") in
  //   "...tasks/abc.output. You will be notified when it completes."
  // doesn't get swallowed into the captured path.
  const m = text.match(
    /Command running in background with ID:\s*([^\s.]+)\.?\s+Output is being written to:\s*(\S+?\.output)\b/,
  );
  if (!m) return null;
  return { taskId: m[1], path: m[2] };
}

export function formatTokens(n: number | undefined | null): string {
  if (!n || n < 0) return "0";
  if (n < 1000) return String(n);
  if (n < 1_000_000) return (n / 1000).toFixed(n < 10_000 ? 1 : 0) + "k";
  return (n / 1_000_000).toFixed(2) + "M";
}

export function formatDuration(ms: number | undefined | null): string {
  if (ms == null || ms < 0) return "";
  if (ms < 1000) return `${Math.round(ms)}ms`;
  const s = ms / 1000;
  if (s < 60) return `${s.toFixed(s < 10 ? 2 : 1)}s`;
  const m = s / 60;
  if (m < 60) return `${m.toFixed(1)}m`;
  return `${(m / 60).toFixed(1)}h`;
}

export function stringifyToolInput(input: unknown): string {
  if (input == null) return "";
  if (typeof input === "string") return input;
  try {
    return JSON.stringify(input, null, 2);
  } catch {
    return String(input);
  }
}

export function extractText(content: unknown): string {
  if (typeof content === "string") return content;
  if (Array.isArray(content)) {
    const parts: string[] = [];
    for (const block of content) {
      if (!block || typeof block !== "object") continue;
      const b = block as { type?: string; text?: string; content?: unknown };
      if (b.type === "text" && typeof b.text === "string") parts.push(b.text);
      else if (b.type === "tool_result") {
        if (typeof b.content === "string") parts.push(b.content);
        else if (Array.isArray(b.content)) {
          for (const sub of b.content) {
            if (sub && typeof sub === "object" && typeof (sub as { text?: string }).text === "string") {
              parts.push((sub as { text: string }).text);
            }
          }
        }
      }
    }
    return parts.join("\n");
  }
  return "";
}
