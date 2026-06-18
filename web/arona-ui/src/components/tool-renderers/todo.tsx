"use client";

import { CheckSquare, Square, Loader2 } from "lucide-react";
import type { ToolRenderer } from "./index";

interface Todo {
  content?: string;
  activeForm?: string;
  status?: "pending" | "in_progress" | "completed" | string;
}

export const TodoWriteRenderer: ToolRenderer = {
  summary(input) {
    const i = input as { todos?: Todo[] };
    const total = i?.todos?.length ?? 0;
    const done = i?.todos?.filter((t) => t.status === "completed").length ?? 0;
    return `${done}/${total} todos`;
  },
  inputView(input) {
    const i = input as { todos?: Todo[] };
    const todos = i.todos || [];
    return (
      <ul className="space-y-1 px-3 py-2 text-xs">
        {todos.map((t, idx) => (
          <li key={idx} className="flex items-start gap-2">
            <span className="mt-0.5 shrink-0">
              {t.status === "completed" ? (
                <CheckSquare className="h-3 w-3 text-emerald-700 dark:text-emerald-400" />
              ) : t.status === "in_progress" ? (
                <Loader2 className="h-3 w-3 animate-spin text-brand" />
              ) : (
                <Square className="h-3 w-3 text-muted-foreground" />
              )}
            </span>
            <span
              className={
                t.status === "completed"
                  ? "text-muted-foreground line-through"
                  : t.status === "in_progress"
                    ? "text-foreground"
                    : "text-foreground/80"
              }
            >
              {t.status === "in_progress" && t.activeForm
                ? t.activeForm
                : t.content}
            </span>
          </li>
        ))}
      </ul>
    );
  },
};
