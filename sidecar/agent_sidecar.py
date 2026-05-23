#!/usr/bin/env python3
"""kasaspace agent sidecar — Claude Agent SDK driver that streams as JSONL.

Pipeline role: this is the "brain" process. The Rust kasaterm host (later)
spawns it, writes a prompt to its stdin, and reads a stream of JSONL message
records from its stdout to render natively. Each line of stdout is exactly
one JSON object describing one SDK message (assistant text, tool call, tool
result, final result, etc.).

It reuses the kasaspace MCP bridge (../mcp/kasaspace_mcp.py) as an external
stdio MCP server so the model can drive panes — split / list / focus / send /
run_job — i.e. the model can decide on its own "this is parallel work, give
it its own pane."

Run:
    echo "<prompt>" | python3 sidecar/agent_sidecar.py
    python3 sidecar/agent_sidecar.py --prompt "<prompt>"

Auth: the Claude Agent SDK reads ANTHROPIC_API_KEY from the environment.
If it is unset, the SDK falls back to whatever auth the bundled/installed
`claude` CLI already has (e.g. a logged-in subscription on this machine).

Output: newline-delimited JSON on stdout. Diagnostics go to stderr only, so
stdout stays a clean JSONL stream for the Rust consumer.
"""

import asyncio
import dataclasses
import json
import os
import sys

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
KASASPACE_MCP = os.path.join(REPO_ROOT, "mcp", "kasaspace_mcp.py")

# Permission mode: a sidecar is non-interactive, so a permission prompt would
# hang forever waiting on a TTY. Default to bypass so tool calls flow; the
# host can override via env when it wants stricter behavior.
PERMISSION_MODE = os.environ.get("KASASPACE_PERMISSION_MODE", "bypassPermissions")

SYSTEM_PROMPT = (
    "You are an agent running inside kasaterm, a GUI terminal with split panes. "
    "You can control panes through the kasaspace tools: split the view, list "
    "surfaces, focus a pane, send text to a pane, and run a long job in its own "
    "new pane. When work naturally fans out into parallel tracks, or when a "
    "command will run for a while (builds, deploys, dev servers, sub-tasks), "
    "open a separate pane for it with kasaspace_run_job so the user can watch "
    "progress live instead of blocking the main pane."
)


def emit(record):
    """Write one JSONL record to stdout and flush so the consumer sees it
    in real time (the whole point of streaming)."""
    sys.stdout.write(json.dumps(record, ensure_ascii=False, default=_fallback) + "\n")
    sys.stdout.flush()


def _fallback(obj):
    """json default= hook for anything not natively serializable."""
    if dataclasses.is_dataclass(obj) and not isinstance(obj, type):
        return dataclasses.asdict(obj)
    if hasattr(obj, "__dict__"):
        return vars(obj)
    return str(obj)


# SDK message classes carry their discriminator in the Python class name,
# not a populated `type` field (observed: top-level `type` is often None).
# Map class names to stable wire strings so the Rust consumer can branch on
# one reliable key. Unknown classes fall back to a lowercased name.
_TYPE_BY_CLASS = {
    "SystemMessage": "system",
    "AssistantMessage": "assistant",
    "UserMessage": "user",
    "ResultMessage": "result",
    "StreamEvent": "stream_event",
    "RateLimitEvent": "rate_limit",
    "HookEventMessage": "hook",
}


def normalize(message):
    """Turn an SDK message object into a plain dict for JSONL.

    SDK messages are dataclasses; dataclasses.asdict recurses through nested
    content-block dataclasses and dicts/lists, giving us a faithful tree
    (content blocks keep their own correct `type`: text/tool_use/tool_result).
    We overwrite the top-level `type` with a class-derived discriminator so
    the Rust side can branch on it without isinstance.
    """
    cls = type(message).__name__
    if dataclasses.is_dataclass(message) and not isinstance(message, type):
        data = dataclasses.asdict(message)
    elif hasattr(message, "__dict__"):
        data = dict(vars(message))
    else:
        data = {"value": str(message)}
    data["type"] = _TYPE_BY_CLASS.get(cls, cls.removesuffix("Message").lower() or cls.lower())
    data["_cls"] = cls
    return data


def read_prompt(argv):
    """Prompt comes from --prompt <text>, or (default) all of stdin."""
    if "--prompt" in argv:
        i = argv.index("--prompt")
        if i + 1 < len(argv):
            return argv[i + 1]
        raise SystemExit("--prompt needs a value")
    if not sys.stdin.isatty():
        text = sys.stdin.read().strip()
        if text:
            return text
    raise SystemExit("no prompt: pipe one on stdin or pass --prompt <text>")


async def run(prompt):
    # Imported lazily so a missing SDK produces a clean JSONL error record
    # instead of an import traceback on stderr that the host can't parse.
    try:
        from claude_agent_sdk import query, ClaudeAgentOptions
    except Exception as e:  # ImportError or version-guard failure
        emit({
            "type": "sidecar_error",
            "error": "claude_agent_sdk import failed",
            "detail": str(e),
            "hint": "pip install claude-agent-sdk (Python 3.10-3.13)",
        })
        return 1

    options = ClaudeAgentOptions(
        system_prompt=SYSTEM_PROMPT,
        permission_mode=PERMISSION_MODE,
        mcp_servers={
            "kasaspace": {
                "command": sys.executable,
                "args": [KASASPACE_MCP],
            }
        },
        allowed_tools=["mcp__kasaspace__*"],
    )

    emit({"type": "sidecar_start", "prompt": prompt, "mcp": KASASPACE_MCP,
          "permission_mode": PERMISSION_MODE})
    try:
        async for message in query(prompt=prompt, options=options):
            emit(normalize(message))
    except Exception as e:
        emit({"type": "sidecar_error", "error": "query failed", "detail": str(e)})
        return 1
    emit({"type": "sidecar_done"})
    return 0


def main():
    prompt = read_prompt(sys.argv[1:])
    rc = asyncio.run(run(prompt))
    sys.exit(rc)


if __name__ == "__main__":
    main()
