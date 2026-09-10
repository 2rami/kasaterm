#!/usr/bin/env python3
"""Ask this pane's app for one name/persona snapshot before the harness execs.

Stdout is only an immutable launch directory; shell consumers never eval text.
Failure stops bootstrap instead of quietly launching the previous student's voice.
"""
import json
import os
from pathlib import Path
import re
import sys
import tempfile
import urllib.parse
import urllib.request


def session_id(harness, args, anchor):
    if harness == "claude":
        for i, arg in enumerate(args[:-1]):
            if arg in ("--resume", "-r", "--session-id"):
                value = args[i + 1]
                if not value.startswith("-"):
                    return Path(value).stem if value.endswith(".jsonl") else value
        return anchor
    # Codex's positional resume/fork UUID, including exec resume. Do not mistake
    # a model/config value or an arbitrary prompt for the selected conversation.
    for i, arg in enumerate(args[:-1]):
        if arg in ("resume", "fork"):
            for value in args[i + 1:]:
                if re.fullmatch(r"[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}", value):
                    return value
            return ""
    return ""


def main():
    harness, shim, anchor, *args = sys.argv[1:]
    pane = os.environ.get("KASATERM_PANE_ID", "")
    if not pane:
        raise ValueError("missing pane identity")
    shim = Path(shim)
    override = shim / f"repersona-{pane}.character"
    requested = override.read_text().strip() if override.is_file() else ""
    socket = os.environ.get("KASATERM_SOCKET_PATH", "")
    portfile = Path((socket[:-5] if socket.endswith(".sock") else socket) + ".mcp_port")
    port = portfile.read_text().strip() if socket and portfile.is_file() else os.environ.get("KASASPACE_MCP_PORT", "")
    if not port.isdigit():
        raise ValueError("pane app port unavailable")
    query = urllib.parse.urlencode({"surface": pane, "sid": session_id(harness, args, anchor), "character": requested})
    request = urllib.request.Request(f"http://127.0.0.1:{int(port)}/agent-identity?{query}", data=b"", method="POST")
    with urllib.request.urlopen(request, timeout=5) as response:
        identity = json.load(response)
    if not all(isinstance(identity.get(key), str) for key in ("character", "persona", "slug")) or not identity["character"]:
        raise ValueError("incomplete launch identity")
    destination = Path(tempfile.mkdtemp(prefix="agent-identity-", dir=shim))
    for key in ("character", "persona", "slug"):
        (destination / key).write_text(identity[key], encoding="utf-8")
    print(destination)


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"kasaterm: 학생 지침을 맞추지 못해 실행을 멈췄어요 ({error}).", file=sys.stderr)
        sys.exit(1)
