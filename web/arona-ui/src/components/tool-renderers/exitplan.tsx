"use client";

import type { ToolRenderer } from "./index";
import { Markdown } from "../Markdown";

// ExitPlanMode — input.plan(markdown)을 그대로 렌더. REGISTRY 누락 시 FALLBACK 한줄/JSON
// 덤프로 plan 이 깨지던 것(거노 plan mode 자주 씀). result("User approved…")는 기본 뷰.
export const ExitPlanRenderer: ToolRenderer = {
  summary(input) {
    const plan = (input as { plan?: string })?.plan || "";
    const first = plan.split("\n").find((l) => l.trim()) || "계획 제안";
    return first.replace(/^#+\s*/, "").replace(/\s+/g, " ").slice(0, 90);
  },
  inputView(input) {
    const plan = (input as { plan?: string })?.plan || "";
    if (!plan) {
      return <div className="px-3 py-2 text-xs italic text-muted-foreground">(no plan)</div>;
    }
    return (
      <div className="px-3 py-2 text-[13px] leading-relaxed">
        <Markdown text={plan} />
      </div>
    );
  },
};
