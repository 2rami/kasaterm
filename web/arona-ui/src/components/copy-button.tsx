"use client";

import { Check, Copy } from "lucide-react";
import { useState } from "react";
import { cn } from "@/lib/utils";

export function CopyButton({
  text,
  className,
  size = "sm",
  label,
  title = "Copy",
}: {
  text: string | (() => string);
  className?: string;
  size?: "xs" | "sm" | "md";
  label?: string;
  title?: string;
}) {
  const [copied, setCopied] = useState(false);
  const dim = size === "xs" ? "h-2.5 w-2.5" : size === "md" ? "h-3.5 w-3.5" : "h-3 w-3";

  return (
    <button
      type="button"
      onClick={async (e) => {
        e.stopPropagation();
        const value = typeof text === "function" ? text() : text;
        try {
          await navigator.clipboard.writeText(value);
          setCopied(true);
          setTimeout(() => setCopied(false), 1200);
        } catch {
          // fallback for non-secure contexts
          const ta = document.createElement("textarea");
          ta.value = value;
          ta.style.position = "fixed";
          ta.style.opacity = "0";
          document.body.appendChild(ta);
          ta.focus();
          ta.select();
          try {
            document.execCommand("copy");
            setCopied(true);
            setTimeout(() => setCopied(false), 1200);
          } catch {}
          document.body.removeChild(ta);
        }
      }}
      title={copied ? "Copied" : title}
      aria-label={copied ? "Copied" : title}
      aria-live="polite"
      className={cn(
        "inline-flex items-center gap-1 rounded text-muted-foreground transition-colors hover:bg-muted hover:text-foreground",
        size === "md" ? "px-2 py-1 text-xs" : "p-1 text-[10px]",
        className,
      )}
    >
      {copied ? (
        <Check aria-hidden="true" className={cn(dim, "text-emerald-600 dark:text-emerald-400")} />
      ) : (
        <Copy aria-hidden="true" className={dim} />
      )}
      {label && <span>{copied ? "Copied" : label}</span>}
    </button>
  );
}

/**
 * A wrapper that shows children and reveals a copy button on hover (right-aligned).
 * Use for inline inspectable values like file paths or session IDs.
 */
export function Copyable({
  text,
  children,
  className,
  always,
}: {
  text: string;
  children: React.ReactNode;
  className?: string;
  /** show copy button always, not only on hover */
  always?: boolean;
}) {
  return (
    <span className={cn("group/copy inline-flex items-center gap-1", className)}>
      <span className="min-w-0">{children}</span>
      <CopyButton
        text={text}
        size="xs"
        className={cn(
          always
            ? "opacity-60 hover:opacity-100 focus-visible:opacity-100"
            : "opacity-0 transition-opacity group-hover/copy:opacity-100 focus-visible:opacity-100",
        )}
      />
    </span>
  );
}
