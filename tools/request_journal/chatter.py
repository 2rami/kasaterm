"""펫이 먼저 거는 말 — 묻지 않아도 몇 초에 한 줄씩 툭 던진다.

`ask.py` 는 사람이 물어야 답하고 도구까지 실행하는 길이다. 이쪽은 아무도 묻지 않았고
아무것도 실행하지 않는다 — 지금 보고 있는 창 하나를 보고 짧은 한 줄을 지어 줄 뿐이다.

한 번 부를 때 여러 줄을 받아 두는 이유는 값이다. 10초마다 모델을 부르면 하루 8640번이고,
그 한 번이 판과 화면을 통째로 싣는다. 다섯 줄을 미리 받아 하나씩 풀면 같은 체감에
호출은 1/5 이 된다(2026-09-18 지시).
"""
from __future__ import annotations

import asyncio
import subprocess

from .ask import PEEK_LINES, _clip, _json_result, llm_client, persona, plain, run_cli

# 말풍선 한 줄이 한눈에 읽히는 길이. 넘으면 사람은 그것을 읽을거리로 보고 미룬다.
LINE_CHARS = 60
LINES = 5
SCREEN_CHARS = 2200

CHATTER_RULES = """
# 지금 자리 — 카사텀 바탕화면 펫이 먼저 거는 말
아무도 너에게 묻지 않았다. 사람은 제 일을 하는 중이고, 너는 그 곁에서 지금 보고 있는 창
하나를 흘깃 보고 혼잣말처럼 한 줄 던진다.
- 한 줄에 한 이야기, 한 문장, 40자 안. 여러 줄을 한 덩어리로 쓰지 마라.
- 정확히 {count} 줄을 내라. 줄마다 다른 이야기여야 한다 — 같은 말을 고쳐 쓴 줄은 버려진다.
- 앞줄부터 지금 벌어지는 일 순서로. 첫 줄이 제일 최근 이야기다.
- 말 거는 투로. 「~다넹」·「~라냥~」·「~겡」 같은 네 어미를 살려 가볍게 던진다.
- 사람이 봐야 할 게 있으면 그 줄에서 슬쩍 권한다(「한번 봐 보라냥~」). 없으면 권하지 마라.
- 근거는 아래 자료뿐이다. 자료에 없는 것은 지어내지 마라. 학생이 보고한 것과 실제 반영은
  다르니 「~했다넹」처럼 귀속해서 말한다.
- 코드 식별자·경로·명령·창 번호는 빼고 사람 말로 옮긴다.
- 마크다운 기호(**, #, -, 코드펜스)와 따옴표로 감싼 줄, 번호 매김은 쓰지 마라. 글자만 낸다.
- 설명·머리말·맺음말 없이 줄만 낸다.
"""


def machine_label() -> str:
    """이 기계를 사람이 부르는 이름. 못 읽으면 말하지 않는 편이 낫다 — 호스트 이름을
    그대로 내면 사람이 쓰지 않는 말이 말풍선에 뜬다."""
    try:
        name = subprocess.run(["scutil", "--get", "ComputerName"], capture_output=True, text=True, timeout=3)
    except (OSError, subprocess.SubprocessError):
        return ""
    return name.stdout.strip() if name.returncode == 0 else ""


def context(pane: str) -> dict:
    """모델에 실을 재료 — 지금 보는 창의 판 줄과 화면 꼬리. 모든 기기 판은 싣지 않는다.
    이 말은 한 창을 두고 하는 혼잣말이고, 판 전체는 한 줄에 담기지도 않는다."""
    row: dict = {}
    ok, raw = run_cli("board")
    if ok:
        for entry in _json_result(raw).get("board", []) or []:
            if isinstance(entry, dict) and str(entry.get("surface_id")) == pane:
                row = entry
                break
    screen = ""
    ok, raw = run_cli("peek", pane)
    if ok:
        lines = [line.rstrip() for line in str(_json_result(raw).get("text", "")).splitlines() if line.strip()]
        screen = "\n".join(lines[-PEEK_LINES:])[-SCREEN_CHARS:]
    return {"who": row, "screen": screen}


def prompt(ctx: dict, label: str, count: int) -> str:
    who = ctx["who"]
    tools = ", ".join(str(name) for name in (who.get("recent_tools") or [])[:6] if name)
    return (
        f"[지금 보는 창] 기계 {label or '이 기기'} · 학생 {who.get('character') or '미배정'} · "
        f"상태 {who.get('status') or '-'} · 하는 일 {_clip(who.get('title'), 120) or '-'}\n"
        f"[마지막 지시] {_clip(who.get('last_prompt'), 500)}\n"
        f"[마지막 답] {_clip(who.get('last_reply'), 500)}\n"
        f"[방금 한 것] {_clip(who.get('intent'), 240)}\n"
        f"[최근 도구] {_clip(tools, 240)}\n\n"
        f"[화면 끝 부분]\n{ctx['screen']}\n\n"
        f"이 창을 두고 {count} 줄을 내라."
    )


def split(reply: str, count: int) -> list[str]:
    """모델이 낸 덩어리를 말풍선 한 줄들로. 번호·글머리·따옴표를 걷고 같은 줄은 버린다."""
    out: list[str] = []
    for raw in plain(reply).splitlines():
        line = raw.strip().lstrip("0123456789.)·-*• \t").strip().strip('"“”「」')
        if not line or line in out:
            continue
        out.append(_clip(line, LINE_CHARS))
        if len(out) == count:
            break
    return out


def chatter(provider_factory, body: dict) -> tuple[int, dict]:
    pane = str(body.get("pane", "")).strip()
    if not pane.startswith("%"):
        return 400, {"error": "pane_required"}
    count = body.get("count", LINES)
    if not isinstance(count, int) or not 1 <= count <= 8:
        return 400, {"error": "invalid_count"}
    client = llm_client(provider_factory)
    if client is None:
        return 503, {"error": "llm_unavailable"}
    ctx = context(pane)
    if not ctx["who"] and not ctx["screen"]:
        return 200, {"lines": []}

    async def call():
        return await asyncio.wait_for(
            client.messages(
                system=persona() + "\n" + CHATTER_RULES.format(count=count),
                messages=[{"role": "user", "content": prompt(ctx, machine_label(), count)}],
                max_tokens=400,
                temperature=0.8,
                # 한 줄짜리 혼잣말에 긴 추론을 물리면 값과 시간만 늘고 말은 그대로다.
                reasoning_effort="low",
            ),
            timeout=35.0,
        )

    try:
        response = asyncio.run(call())
    except Exception as exc:
        return 502, {"error": "model_failed", "detail": str(exc)[:200]}
    return 200, {"lines": split(client.extract_text(response) or "", count)}
