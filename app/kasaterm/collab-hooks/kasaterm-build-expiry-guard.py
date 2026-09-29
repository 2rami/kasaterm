#!/usr/bin/env python3
"""PreToolUse(Bash) — 학생이 TestFlight·App Store Connect 빌드를 만료시키지 못하게 막는다.

빌드 만료는 되돌릴 수 없고, 그 빌드를 깔아 둔 폰은 앱이 「빌드가 제거됨」으로 멈춘다. 2026-09-28 08:21 에
카사텀 폰 앱의 옛 빌드 14개가 한꺼번에 만료돼 사람 폰의 앱이 멈췄다. 만료는 사람이 App Store Connect
웹에서 직접 한다. 스크립트 파일 안의 요청까지는 못 보므로 명령줄에 드러난 것만 막는다.
kasaterm pane 밖이면 no-op, 입력을 못 읽으면 통과.
"""
import json
import os
import re
import sys

EXPIRE = re.compile(r"""["']?\bexpired["']?\s*[:=]\s*["']?(true|True|1)\b|\bexpire[_-]?(all[_-]?)?builds?\b""")
TARGET = re.compile(r"/v1/builds|appstoreconnect|\basc\b|asc\.py|testflight|pilot", re.IGNORECASE)

REASON = (
    "TestFlight·App Store Connect 빌드 만료는 학생이 하지 않아요 — 되돌릴 수 없고, 그 빌드를 깐 폰의 앱이 "
    "「빌드가 제거됨」으로 멈춰요. 잘못 올린 빌드가 있으면 번호와 까닭을 사람에게 알리고 "
    "App Store Connect 웹에서 직접 만료해 달라고 부탁하세요. 새 빌드를 올리는 것은 막지 않아요."
)


def main():
    if not os.environ.get("KASATERM_PANE_ID"):
        return
    try:
        payload = json.load(sys.stdin)
    except Exception:
        return
    if payload.get("tool_name") != "Bash":
        return
    command = str((payload.get("tool_input") or {}).get("command") or "")
    if not (EXPIRE.search(command) and TARGET.search(command)):
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
