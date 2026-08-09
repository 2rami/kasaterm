#!/usr/bin/env python3
"""UserPromptSubmit 훅 — 이 턴이 ultracode 인지 마커로 남긴다.

claude 는 statusline 에 ultracode 를 실어 주지 않는다(payload 스펙의 effort 는
low|medium|high|xhigh|max 뿐이고 ultracode 는 별개 appState 다). 그래서 화면에는
effort 만 뜨고, 여러 에이전트를 풀어 놓는 턴인지가 안 보인다. 그 한 칸을 여기서 만든다.

**턴 단위인 것이 핵심이다.** ultracode 는 키워드가 들어간 그 턴만 켜지므로(하네스가
"opting this turn into" 라고 말한다) 마커도 프롬프트마다 다시 쓰고 지운다 — 세션 토글로
만들면 껐다는 사실을 아무도 안 알려 줘서 화면이 조용히 거짓말을 하게 된다.

경로를 session_id 로 잡는 이유: statusline 도 같은 값을 stdin 으로 받으므로 pane env 나
cwd slug 처럼 양쪽이 따로 계산하다 어긋날 여지가 없다.
"""
import json
import os
import pathlib
import sys

MARK_DIR = pathlib.Path("/tmp/kasaterm-collab/ultracode")


def main() -> None:
    try:
        d = json.load(sys.stdin)
    except Exception:
        return
    sid = d.get("session_id")
    if not isinstance(sid, str) or not sid:
        return
    # 경로 조작 방지 — session_id 는 uuid 지만 남이 준 값이므로 그대로 믿지 않는다.
    safe = "".join(c for c in sid if c.isalnum() or c in "-_")
    if not safe:
        return

    prompt = d.get("prompt")
    on = isinstance(prompt, str) and "ultracode" in prompt.lower()

    path = MARK_DIR / f"{safe}.on"
    try:
        if on:
            MARK_DIR.mkdir(parents=True, exist_ok=True)
            path.write_text("1")
        else:
            path.unlink(missing_ok=True)
    except OSError:
        # 마커는 표시용이라 실패해도 턴을 막지 않는다.
        pass


if __name__ == "__main__":
    main()
