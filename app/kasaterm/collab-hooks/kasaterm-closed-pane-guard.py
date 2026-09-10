#!/usr/bin/env python3
"""PreToolUse/UserPromptSubmit closed-pane guard.

닫힌 pane 은 10초의 되살리기 유예 동안 PTY 를 보관한다. 그런데 claude 하네스의
세션 명부(ListAgents)에는 **닫힘이 안 보인다** — 그래서
학생이 명부에서 이름을 보고 일을 시키고, 그 작업이 사용자 눈에 없는 곳에서 돌아간다
(거노 2026-08-15 「내가 pane 닫아도 너네한텐 안 보이고 살아있어서 거기다가 시킨다」).

board 에는 `detached` 로 이미 있지만, 그건 **보러 가야** 보인다. 안 보고 보내는 것이
사고의 형태라, 보내는 그 순간에 막는다. kasaterm pane 밖이면 no-op.

Closing panes also reject terminal input. There is no tell bypass; reopening
is the deliberate way to resume accepting work. Stashes are not close-grace targets.

⚠️ 판정이 안 서면 **통과시킨다.** kasaterm-cli 가 없든 board 가 느리든 이름이 안
맞든, 가드 자신의 사정으로 멀쩡한 메시지를 막는 것이 놓치는 것보다 나쁘다.
"""
import json
import os
import subprocess
import sys
from pathlib import Path

# board 는 세션마다 transcript 꼬리를 읽어 0.5초쯤 걸린다. 이보다 늦으면 판정을
# 포기하고 통과 — SendMessage 가 가드 때문에 멎는 것이 더 나쁘다.
TIMEOUT = float(os.environ.get("KASATERM_CLOSED_GUARD_TIMEOUT", "3"))


def board_rows():
    try:
        out = subprocess.run(
            ["kasaterm-cli", "board"],
            capture_output=True,
            text=True,
            timeout=TIMEOUT,
        ).stdout
        return json.loads(out).get("result", {}).get("board", []) or []
    except Exception:
        return []


def target_names(to):
    """`to` 가 가리킬 수 있는 이름들. 목록이 보여 준 이름을 그대로 쓰는 것이
    규칙이라 정규화는 최소로 — 겹칠 때만 붙이는 ` [ref]` 꼬리만 뗀다."""
    to = (to or "").strip()
    if not to:
        return []
    if to.endswith("]") and " [" in to:
        to = to[: to.rindex(" [")].strip()
    return [to]


def main():
    if not os.environ.get("KASATERM_PANE_ID"):
        return
    try:
        payload = json.load(sys.stdin)
    except Exception:
        return
    # Receive-side guard: a stale peer roster or a cross-session delivery must
    # not restart work during the close grace. No board scan on ordinary tools.
    pane = os.environ.get("KASATERM_PANE_ID", "")
    socket = os.environ.get("KASATERM_SOCKET_PATH", "")
    if socket and pane.startswith("%") and pane[1:] and all(c.isalnum() or c in "_-" for c in pane[1:]):
        if (Path(socket + ".closing") / pane[1:]).is_file():
            print(json.dumps({"continue": False, "stopReason": "이 창은 닫혔어요. 되살리기 전에는 새 작업을 시작하지 마세요."}, ensure_ascii=False))
            return
    if payload.get("tool_name") != "SendMessage":
        return
    names = target_names(payload.get("tool_input", {}).get("to"))
    if not names:
        return

    row = None
    for r in board_rows():
        # 이름은 명부가 부르는 대로(`agent_name`)가 정본이고, surface id 로 보내는
        # 사람도 있어 그것도 본다. 캐릭터 이름(`유우카`)은 **안 쓴다** — 여러 방에
        # 같은 학생이 있을 수 있어 엉뚱한 pane 을 짚는다.
        keys = {r.get("agent_name"), r.get("peer_name"), r.get("surface_id")}
        if keys & set(names):
            row = r
            break
    # 못 찾았으면 우리 pane 밖의 무언가다(main, 서브에이전트, 다른 기계). 판정 못 함 → 통과.
    if row is None or not row.get("detached"):
        return

    who = row.get("character") or row.get("agent_name") or names[0]
    surface = row.get("surface_id") or "%?"
    reason = (
        f"{who}({surface}) 의 pane 은 **화면에 없어요** — 사용자가 닫았거나 숨긴 자리라, "
        f"거기 시킨 작업은 사용자가 못 보는 곳에서 돌아갑니다. 명부(ListAgents)에는 "
        f"닫힘이 안 보여서 멀쩡해 보였을 거예요. "
        f"① 이어받을 일이면 새 pane 을 쪼개 띄우세요(kasaterm-cli split). "
        f"② 그 대화를 꼭 이어야 하면 사용자에게 되살리기로 꺼내 달라고 하세요. "
        f"새 지시는 화면에 되살린 뒤 보내세요."
    )
    print(
        json.dumps(
            {
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "deny",
                    "permissionDecisionReason": reason,
                }
            },
            ensure_ascii=False,
        )
    )


if __name__ == "__main__":
    main()
