#!/usr/bin/env python3
"""PreToolUse(Agent) — kasaterm pane 안에서는 일을 서브에이전트 대신 학생에게 맡기게 한다.

서브에이전트는 보드·화면에 안 보여 사람이 진행을 못 지켜본다. 그런데 Agent 도구가 한 번에 끝나고
결과까지 돌아오니 Claude 가 그쪽으로 흘렀다(2026-09-28). 학생 소환은 `kasaterm-cli summon` 한 줄이고
완료 보고는 부른 창 입력으로 들어오므로, 여기서 막고 그 길을 알려 준다.

읽기만 하는 짧은 탐색·설계(Explore·Plan)와 안내(claude-code-guide·statusline-setup)는 통과시킨다.
사람이 서브에이전트를 직접 청했으면 description 에 [subagent] 를 넣어 다시 부르면 통과한다.
kasaterm pane 밖이면 no-op, 입력을 못 읽으면 통과.
"""
import json
import os
import sys

ALLOWED = {"Explore", "Plan", "claude-code-guide", "statusline-setup"}
BYPASS = "[subagent]"

REASON = (
    "kasaterm pane 안에서는 서브에이전트 대신 학생을 소환해요 — 서브에이전트는 보드·화면에 안 보여 "
    "사람이 진행을 못 지켜봐요. "
    "`kasaterm-cli summon --cwd <레포> \"<배경·파일·완료 기준>\"` 한 줄이면 옆에 학생이 떠서 브리프를 받고, "
    "끝나면 done 보고가 이 창 입력으로 들어와요. 막고 기다리려면 `kasaterm-cli board --wait <이름>` 을 백그라운드로 돌려요. "
    "여러 명이면 겹치지 않는 영역으로 나눠 한 명씩 summon 해요. "
    "사람이 서브에이전트를 직접 청했을 때만 description 에 " + BYPASS + " 를 넣어 다시 부르세요."
)


def main():
    if not os.environ.get("KASATERM_PANE_ID"):
        return
    try:
        payload = json.load(sys.stdin)
    except Exception:
        return
    if payload.get("tool_name") not in ("Agent", "Task"):
        return
    tool_input = payload.get("tool_input") or {}
    if (tool_input.get("subagent_type") or "general-purpose") in ALLOWED:
        return
    if BYPASS in (tool_input.get("description") or ""):
        return
    print(json.dumps({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": REASON,
        }
    }, ensure_ascii=False))


if __name__ == "__main__":
    main()
