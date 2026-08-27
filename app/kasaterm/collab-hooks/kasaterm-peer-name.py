#!/usr/bin/env python3
"""UserPromptSubmit — 도착한 cross-session 메시지의 발신자를 캐릭터 이름으로 풀어 준다.

`<cross-session-message from-name="...">` 의 그 이름은 **세션 이름**이다. 사용자가
`/rename` 으로 붙인 작업 메모이거나 셰임이 지은 로마자 슬러그라, 어느 쪽이든 화면에
뜨는 캐릭터 이름과 다르다. 그래서 학생이 서로를 「부작용 메모리기록」·「theme」 같은
이름으로 부르고, 사용자는 자기 화면의 어느 창을 말하는지 못 알아본다
(2026-08-27 지시 「캐릭터들끼리도 캐릭터이름으로 알아야해」).

태그는 **claude 가 직접 만든다** — 앱이 끼어들 자리가 없다(바이너리에 `from-name` 이
박혀 있다). 명부(`~/.claude/sessions/*.json`)를 밖에서 고치는 것도 안 먹는다: claude 가
자기 이름을 안에 들고 있어, 파일만 바꾸면 `ListAgents` 는 여전히 옛 이름을 부른다
(실측). 남는 자리가 **받는 쪽 훅**이라 여기서 푼다.

⚠️ **이 훅은 옛 `board-context.py` 의 되돌이가 아니다.** 그건 프롬프트마다 persona +
board 전체 + inbox 를 밀어 넣어 워커 컨텍스트를 부풀렸고 그래서 걷어냈다(거노 06-14).
여기는 반대로 좁다 — ①`cross-session-message` 가 든 턴에만 돌고 ②그 메시지의 **발신자
한 줄만** 낸다 ③사람이 친 프롬프트에는 아무것도 안 붙는다. 이 셋 중 하나라도 풀면 그때
걷어낸 그 비용이 그대로 돌아온다.

두 자리에서 돈다. **UserPromptSubmit** 은 위의 도착 순간이고, **PostToolUse(ListAgents)**
는 보내려고 목록을 볼 때다 — 거기 뜨는 이름도 전부 세션 이름이라, 학생이 그 이름을 그대로
사람 이름처럼 쓰게 된다. 뒤쪽은 목록 자체를 건드리지 않고 옆에 표만 붙인다:
**부를 때는 캐릭터 이름, `to:` 에 넣는 주소는 목록의 그 이름**이라는 구분이 요점이다.

⚠️ 판정이 안 서면 **아무것도 안 낸다.** 이름을 못 찾든 board 가 느리든, 조용히 지나가는
쪽이 잘못된 이름을 확신에 차서 알려 주는 것보다 낫다.
"""
import json
import os
import re
import subprocess
import sys

# closed-pane-guard 와 같은 근거의 상한 — board 는 세션마다 transcript 꼬리를 읽어
# 0.5~1초쯤 걸린다. 늦으면 포기한다.
TIMEOUT = float(os.environ.get("KASATERM_PEER_NAME_TIMEOUT", "3"))

TAG = re.compile(r'<cross-session-message\b[^>]*?\bfrom-name="([^"]*)"')


def board_rows():
    cli = os.environ.get("KASATERM_CLI", "kasaterm-cli")
    try:
        out = subprocess.run(
            [cli, "board"],
            capture_output=True,
            text=True,
            timeout=TIMEOUT,
        ).stdout
        return json.loads(out).get("result", {}).get("board", []) or []
    except Exception:
        return []


def name_table():
    by_name = {}
    for row in board_rows():
        peer = row.get("peer_name")
        char = row.get("character")
        if isinstance(peer, str) and peer and isinstance(char, str) and char:
            by_name[peer] = char
    return by_name


def label_of(peer, char, by_name):
    """같은 캐릭터가 두 pane 에 배정돼 있으면 캐릭터 이름만으로는 사람을 못 가른다
    (실측: 코유키가 둘이었다). 그때만 세션 이름을 괄호로 덧붙인다."""
    twins = sum(1 for c in by_name.values() if c == char)
    return f"{char}({peer})" if twins > 1 else char


def emit(event, ctx):
    print(
        json.dumps(
            {"hookSpecificOutput": {"hookEventName": event, "additionalContext": ctx}},
            ensure_ascii=False,
        )
    )


def on_list_agents(d):
    """목록을 본 직후 — 이름↔캐릭터 표를 옆에 붙인다(목록 자체는 안 건드린다)."""
    if d.get("tool_name") != "ListAgents":
        return
    table = name_table()
    if not table:
        return
    pairs = " · ".join(f"{n}={c}" for n, c in table.items() if n != c)
    if not pairs:
        return
    twins = sorted({c for c in table.values() if list(table.values()).count(c) > 1})
    warn = (
        f"\n⚠️ {'·'.join(twins)} 는 두 창에 겹쳐 있다 — 캐릭터 이름만으로는 안 갈리니 "
        "세션 이름을 함께 말해라."
        if twins
        else ""
    )
    emit(
        "PostToolUse",
        "위 목록에 뜨는 이름은 **세션 이름**이다(사용자가 붙인 작업 메모이거나 자동 슬러그). "
        "이 기계에서 캐릭터가 잡히는 것들:\n"
        + pairs
        + "\n**부를 때는 캐릭터 이름**을 쓰고, `to:` 에 넣는 주소는 **위 목록의 그 이름 그대로**다 "
        "— 캐릭터 이름으로 보내면 닿지 않는다." + warn,
    )


def main():
    try:
        d = json.load(sys.stdin)
    except Exception:
        return

    if d.get("hook_event_name") == "PostToolUse":
        on_list_agents(d)
        return

    # title-sync 가 제목을 지으려고 띄우는 claude 는 **대화 전문**을 프롬프트로 받는다.
    # 그 안에 지난 cross-session 메시지가 섞여 있어 여기가 헛돌고, 그 세션에는 알려 줄
    # 상대도 없다(실측: 로그에 title-gen 턴이 그대로 잡혔다). 같은 가드를
    # ultracode-mark.py·kasaterm-bind-transcript.sh 가 이미 갖고 있다.
    junk = str(d.get("transcript_path") or "") + str(d.get("cwd") or "") + os.getcwd()
    if "kasaterm-title-gen" in junk:
        return

    prompt = d.get("prompt")
    if not isinstance(prompt, str) or "cross-session-message" not in prompt:
        return

    seen, names = set(), []
    for n in TAG.findall(prompt):
        if n and n not in seen:
            seen.add(n)
            names.append(n)
    if not names:
        return

    by_name = name_table()

    lines = []
    for n in names:
        c = by_name.get(n)
        if c and c != n:
            lines.append(f'- `from-name="{n}"` 은 **{label_of(n, c, by_name)}** 다.')
    if not lines:
        return

    ctx = (
        "방금 도착한 cross-session 메시지의 `from-name` 은 **세션 이름**이다 — 사용자가 "
        "`/rename` 으로 붙인 작업 메모이거나 자동으로 지어진 슬러그라, 사람 이름이 아니다. "
        "실제 상대는 이렇다:\n"
        + "\n".join(lines)
        + "\n답하거나 남에게 언급할 때는 **캐릭터 이름**으로 불러라. 세션 이름은 사용자가 "
        "그 창에 붙여 둔 메모니 참고만 하면 된다."
    )
    emit("UserPromptSubmit", ctx)


if __name__ == "__main__":
    main()
