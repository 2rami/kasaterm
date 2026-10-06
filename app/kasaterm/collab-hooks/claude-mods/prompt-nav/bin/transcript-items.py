#!/usr/bin/env python3
"""claude 대화 기록(jsonl)에서 화면에 그려지는 줄을 그 순서대로 뽑는다.

mod 가 세션을 시작할 때 한 번 부른다. 기록은 수십 MB 라 mod 의 파일 읽기(4 MiB
한도)로는 못 읽고, 화면 순서는 파일 순서가 아니라 parentUuid 사슬(되감기로 버린
가지 제외, 압축 경계에서 끊김)이라 여기서 사슬을 걷는다.

출력은 한 줄 JSON 배열: [id, 종류, 줄 수, 글자 폭, 첫 줄] — 종류 u=프롬프트,
n=그려지되 프롬프트가 아닌 사람 줄, a=답 글, t=도구 줄, s=압축 요약.

n 을 빼면 안 된다: 그 줄이 화면에 그려질 때 mod 는 모르는 줄이라 대화 끝에 붙여, 그
뒤 줄의 자리와 맨 윗줄 판정이 통째로 어긋난다.
"""
import json
import re
import sys
import unicodedata

# 사람이 친 것이 아닌 user 줄 — kasaterm transcript.rs 의 is_injected_user_text 와 같은 표식.
INJECTED = (
    "<command-name>", "<command-message>", "<command-args>", "<local-command-stdout>",
    "<local-command-stderr>", "<local-command-caveat>", "<bash-input>", "<bash-stdout>",
    "<bash-stderr>", "<task-notification>", "<user-memory-input>", "Caveat:",
    "Your tool call was malformed", "[Request interrupted",
)
# 화면에 줄로 그려지지 않는 것.
HIDDEN = ("<local-command-caveat>", "Caveat:")
# 턴을 여는 줄은 누가 보냈든 프롬프트다(사람·tell·다른 세션·예약). 이 출처만 아니다 — mod 의
# NOT_PROMPT(hooks/register.tsx)와 같은 표.
NOT_PROMPT = {"task-notification", "peer-send-message", "observer", "observer-activity", "auto-continuation"}
# plugin·다른 세션이 보낸 줄은 기록에 엔진의 머리말이 붙는다. 화면 첫 줄은 그 뒤 본문이다.
FRAMING = (" plugin sent a message:", "Another Claude session sent a message:")


def width(s):
    return sum(2 if unicodedata.east_asian_width(ch) in "WF" else 1 for ch in s)


def measure(text):
    lines = text.split("\n")
    return len(lines), sum(width(line) for line in lines)


def user_text(content):
    if isinstance(content, str):
        return content
    if not isinstance(content, list):
        return None
    parts = []
    for block in content:
        if not isinstance(block, dict):
            continue
        if block.get("type") == "tool_result":
            return None
        if block.get("type") == "text":
            parts.append(block.get("text") or "")
    return "\n".join(parts) if parts else None


def main(path):
    rows = {}
    last = None
    with open(path, "rb") as f:
        for raw in f:
            if b'"uuid"' not in raw:
                continue
            try:
                row = json.loads(raw)
            except ValueError:
                continue
            uuid = row.get("uuid")
            # 첨부·시스템 줄도 사슬의 고리라 담는다 — 빼면 사슬이 거기서 끊긴다.
            if not uuid or row.get("isSidechain"):
                continue
            rows[uuid] = row
            last = uuid
    chain = []
    seen = set()
    cur = last
    while cur and cur in rows and cur not in seen:
        seen.add(cur)
        chain.append(rows[cur])
        cur = rows[cur].get("parentUuid")
    chain.reverse()
    out = []
    for row in chain:
        msg = row.get("message") or {}
        # 슬래시 명령과 그 출력은 system 줄로 남지만 화면에는 사람 줄처럼 그려진다.
        local = row.get("type") == "system" and row.get("subtype") == "local_command"
        if row.get("type") == "user" or local:
            if row.get("isMeta"):
                continue
            text = row.get("content") if local else user_text(msg.get("content"))
            if not isinstance(text, str):
                continue
            if text is None:
                continue
            if row.get("isCompactSummary"):
                out.append([row["uuid"], "s", 2, 0, ""])
                continue
            stripped = text.strip()
            if not stripped or stripped.startswith(HIDDEN):
                continue
            kind = (row.get("origin") or {}).get("kind")
            # 사람이 친 슬래시 명령은 화면에 `❯ /이름 인자` 로 남는다 — 터미널 스크롤백 쪽 눈금과 같게 센다.
            command = re.match(r"<command-name>(.*?)</command-name>", stripped, re.S)
            # 이동 명령 자신은 턴이 아니다(hooks/register.tsx 의 NAV_COMMAND).
            if command and command.group(1).strip() == "/prompt-nav":
                out.append([row["uuid"], "n", 1, 0, ""])
                continue
            if command and kind not in NOT_PROMPT:
                args = re.search(r"<command-args>(.*?)</command-args>", stripped, re.S)
                line = " ".join(filter(None, [command.group(1).strip(), args and args.group(1).strip()]))
                out.append([row["uuid"], "u", 1, width(line), line[:80]])
                continue
            if stripped.startswith(INJECTED) or kind in NOT_PROMPT:
                out.append([row["uuid"], "n", 1, 0, ""])
                continue
            head, _, rest = stripped.partition("\n")
            if rest and head.endswith(FRAMING):
                stripped = rest.strip()
            n, w = measure(stripped)
            out.append([row["uuid"], "u", n, w, stripped.split("\n", 1)[0][:80]])
        elif row.get("type") == "assistant":
            for block in msg.get("content") or []:
                if not isinstance(block, dict):
                    continue
                if block.get("type") == "text" and (block.get("text") or "").strip():
                    n, w = measure(block["text"].strip())
                    out.append([row["uuid"], "a", n, w, ""])
                elif block.get("type") == "tool_use" and block.get("id"):
                    out.append([block["id"], "t", 2, 0, ""])
    sys.stdout.write(json.dumps(out, ensure_ascii=False, separators=(",", ":")))


if __name__ == "__main__":
    main(sys.argv[1])
