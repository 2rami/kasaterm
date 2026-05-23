#!/usr/bin/env python3
"""kasaspace MCP bridge — stdio MCP server that relays tool calls to the
kasaterm agent-socket (unix socket JSON-RPC).

Why this exists: Claude Code talks MCP over stdin/stdout. kasaterm exposes
pane control over a unix-domain socket speaking its own newline-delimited
JSON-RPC (surface.split / surface.list / surface.focus / surface.send_text).
This script is the thin shim between the two so Claude can split/drive panes
as explicit tools.

Zero third-party deps — stdlib only, single file. Run via `.mcp.json`.

Socket resolution mirrors the host exactly:
    KASATERM_SOCKET_PATH > CMUX_SOCKET_PATH > $TMPDIR/kasaterm-<pid>.sock
The host (app/kasaterm) binds the socket and exports KASATERM_SOCKET_PATH
into every child shell, so a Claude process running inside a pane already
has the right value in its environment.
"""

import json
import os
import socket
import sys
import tempfile
import time

# ---------------------------------------------------------------------------
# agent-socket client (kasaterm side)
# ---------------------------------------------------------------------------

# Default protocol version we advertise to the MCP client. We echo the
# client's requested version when it sends one in `initialize`.
DEFAULT_PROTOCOL_VERSION = "2024-11-05"

SERVER_NAME = "kasaspace"
SERVER_VERSION = "0.1.0"


def resolve_socket_path():
    """Same precedence the kasaterm host uses. The per-pid temp fallback
    only matters when running outside a pane (manual testing); inside a
    pane KASATERM_SOCKET_PATH is always set."""
    path = os.environ.get("KASATERM_SOCKET_PATH") or os.environ.get("CMUX_SOCKET_PATH")
    if path:
        return path
    # Fallback can't know the host pid, so this is best-effort only.
    return os.path.join(tempfile.gettempdir(), f"kasaterm-{os.getpid()}.sock")


def agent_rpc(method, params, timeout=5.0):
    """One request/response round trip against the agent-socket.

    Connects, writes one JSON line, reads one JSON line back, closes.
    Raises RuntimeError on transport failure or an `ok: false` reply so
    the caller can fold it into an MCP tool error.
    """
    sock_path = resolve_socket_path()
    req = {
        "id": f"mcp-{int(time.time() * 1000)}",
        "method": method,
        "params": params or {},
    }
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
            s.settimeout(timeout)
            s.connect(sock_path)
            s.sendall((json.dumps(req) + "\n").encode("utf-8"))
            # Read until newline — the server frames one response per line.
            buf = bytearray()
            while b"\n" not in buf:
                chunk = s.recv(4096)
                if not chunk:
                    break
                buf.extend(chunk)
    except FileNotFoundError:
        raise RuntimeError(
            f"kasaterm socket not found at {sock_path!r}. "
            "Is this running inside a kasaterm pane?"
        )
    except (ConnectionRefusedError, socket.timeout, OSError) as e:
        raise RuntimeError(f"agent-socket transport error on {sock_path!r}: {e}")

    line = bytes(buf).split(b"\n", 1)[0].strip()
    if not line:
        raise RuntimeError("agent-socket closed connection without a response")
    try:
        resp = json.loads(line)
    except json.JSONDecodeError as e:
        raise RuntimeError(f"bad JSON from agent-socket: {e}")

    if not resp.get("ok", False):
        err = resp.get("error") or {}
        raise RuntimeError(
            f"agent-socket error {err.get('code', '?')}: {err.get('message', 'unknown')}"
        )
    return resp.get("result", {})


# ---------------------------------------------------------------------------
# MCP tool definitions + handlers
# ---------------------------------------------------------------------------

TOOLS = [
    {
        "name": "kasaspace_split",
        "description": (
            "Split the current kasaterm pane to create a new pane (surface) "
            "in the given direction. Use this when work fans out into "
            "parallel tracks and you want a separate pane to drive."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "direction": {
                    "type": "string",
                    "enum": ["left", "right", "up", "down"],
                    "description": "Where the new pane opens relative to the current one.",
                }
            },
            "required": ["direction"],
        },
    },
    {
        "name": "kasaspace_list",
        "description": (
            "List kasaterm surfaces (panes) and workspaces. Returns surface "
            "ids you can pass to kasaspace_focus / kasaspace_send."
        ),
        "inputSchema": {"type": "object", "properties": {}},
    },
    {
        "name": "kasaspace_focus",
        "description": "Focus a specific kasaterm pane by its surface id.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "surface_id": {
                    "type": "string",
                    "description": "Surface id from kasaspace_list.",
                }
            },
            "required": ["surface_id"],
        },
    },
    {
        "name": "kasaspace_send",
        "description": (
            "Send text to a kasaterm pane (e.g. a shell command — include a "
            "trailing newline to run it). Targets the focused pane unless "
            "surface_id is given."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "Text to type into the pane. Add '\\n' to submit.",
                },
                "surface_id": {
                    "type": "string",
                    "description": "Optional target surface id; defaults to focused pane.",
                },
            },
            "required": ["text"],
        },
    },
    {
        "name": "kasaspace_run_job",
        "description": (
            "Run a long-running command in a NEW labelled pane so the user "
            "watches its progress live. Splits a fresh pane, labels its header "
            "with the job title and an accent color, then types the command "
            "into it. Use this for background jobs (builds, deploys, dev "
            "servers, sub-agents) instead of blocking your own pane. Returns "
            "the new surface id. Note: output stays visual in the pane — it is "
            "not streamed back to you."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Shell command to run in the new pane (no trailing newline needed).",
                },
                "title": {
                    "type": "string",
                    "description": "Short human label for the pane header (e.g. 'build', 'deploy api', 'tests'). Defaults to the command.",
                },
                "direction": {
                    "type": "string",
                    "enum": ["left", "right", "up", "down"],
                    "description": "Where the job pane opens. Defaults to 'down'.",
                },
                "color": {
                    "type": "string",
                    "description": "Optional #rrggbb accent for the header. Defaults to a blue-gray that marks background jobs.",
                },
                "auto_close": {
                    "type": "boolean",
                    "description": "If true, the pane closes itself when the command finishes (appends '; exit' so the shell quits and the pane is reaped). Use for teammates/sub-agents that should disappear when done; leave false for jobs whose output the user wants to keep reading.",
                },
            },
            "required": ["command"],
        },
    },
]

# Default header accent for background jobs — a calm blue-gray so job panes
# read as "machine-driven, watch me" without competing with the user's
# foreground pane.
JOB_COLOR = "#5b7fa6"


def call_tool(name, args):
    """Map an MCP tool call to an agent-socket method and return a
    human-readable result string. Raises RuntimeError on bad input or
    backend failure."""
    args = args or {}
    if name == "kasaspace_split":
        direction = args.get("direction")
        if direction not in ("left", "right", "up", "down"):
            raise RuntimeError("direction must be one of left/right/up/down")
        result = agent_rpc("surface.split", {"direction": direction})
        surf = result.get("surface", {})
        return f"Split {direction}. New surface: {json.dumps(surf)}"

    if name == "kasaspace_list":
        surfaces = agent_rpc("surface.list", {}).get("surfaces", [])
        try:
            workspaces = agent_rpc("workspace.list", {}).get("workspaces", [])
        except RuntimeError:
            workspaces = []
        return json.dumps({"surfaces": surfaces, "workspaces": workspaces}, indent=2)

    if name == "kasaspace_focus":
        surface_id = args.get("surface_id")
        if not surface_id:
            raise RuntimeError("surface_id is required")
        agent_rpc("surface.focus", {"surface_id": surface_id})
        return f"Focused {surface_id}"

    if name == "kasaspace_send":
        text = args.get("text")
        if text is None:
            raise RuntimeError("text is required")
        params = {"text": text}
        if args.get("surface_id"):
            params["surface_id"] = args["surface_id"]
        agent_rpc("surface.send_text", params)
        target = args.get("surface_id", "focused pane")
        return f"Sent {len(text)} chars to {target}"

    if name == "kasaspace_run_job":
        command = args.get("command")
        if not command:
            raise RuntimeError("command is required")
        direction = args.get("direction", "down")
        if direction not in ("left", "right", "up", "down"):
            raise RuntimeError("direction must be one of left/right/up/down")
        title = args.get("title") or command.strip().splitlines()[0][:40]
        color = args.get("color") or JOB_COLOR
        auto_close = bool(args.get("auto_close", False))
        # Option-1 composition: split a pane, then label it, then type the
        # command into it. The split reply carries the new surface id; we
        # target everything explicitly so a focus race can't mislabel or
        # misfire into the wrong pane.
        surf = agent_rpc("surface.split", {"direction": direction}).get("surface", {})
        surface_id = surf.get("id")
        if not surface_id:
            raise RuntimeError("split did not return a surface id; cannot label/target job pane")
        # Label + color are best-effort: a backend that doesn't support them
        # (e.g. the tmux backend) shouldn't sink the whole job. The pane and
        # its command still run.
        labels = []
        try:
            agent_rpc("surface.rename", {"surface_id": surface_id, "title": title})
            labels.append(f"title={title!r}")
        except RuntimeError as e:
            labels.append(f"rename skipped ({e})")
        try:
            agent_rpc("surface.set_color", {"surface_id": surface_id, "color": color})
            labels.append(f"color={color}")
        except RuntimeError as e:
            labels.append(f"color skipped ({e})")
        # auto_close: append '; exit' so the shell quits when the command
        # finishes → PTY EOF → kasaterm reaps the pane. Without it the shell
        # lingers and the pane stays (good for jobs whose output you re-read,
        # bad for teammates that should vanish when done).
        run_cmd = command.rstrip("\n")
        if auto_close:
            run_cmd += "; exit"
            labels.append("auto_close")
        agent_rpc("surface.send_text", {
            "surface_id": surface_id,
            "text": run_cmd + "\n",
        })
        return f"Started job in pane {surface_id} ({direction}; {', '.join(labels)}): {command}"

    raise RuntimeError(f"unknown tool: {name}")


# ---------------------------------------------------------------------------
# MCP stdio loop (JSON-RPC 2.0, newline-delimited)
# ---------------------------------------------------------------------------


def send_message(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def make_response(req_id, result):
    return {"jsonrpc": "2.0", "id": req_id, "result": result}


def make_error(req_id, code, message):
    return {"jsonrpc": "2.0", "id": req_id, "error": {"code": code, "message": message}}


def handle_request(msg):
    """Return a response dict, or None for notifications (no reply)."""
    method = msg.get("method")
    req_id = msg.get("id")
    params = msg.get("params") or {}

    # Notifications carry no id and expect no response.
    if method == "notifications/initialized" or req_id is None:
        return None

    if method == "initialize":
        client_version = params.get("protocolVersion") or DEFAULT_PROTOCOL_VERSION
        return make_response(
            req_id,
            {
                "protocolVersion": client_version,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": SERVER_NAME, "version": SERVER_VERSION},
            },
        )

    if method == "ping":
        return make_response(req_id, {})

    if method == "tools/list":
        return make_response(req_id, {"tools": TOOLS})

    if method == "tools/call":
        name = params.get("name")
        arguments = params.get("arguments") or {}
        try:
            text = call_tool(name, arguments)
            return make_response(
                req_id,
                {"content": [{"type": "text", "text": text}], "isError": False},
            )
        except RuntimeError as e:
            # Tool-level errors are reported in the result with isError so
            # the model can see and react, not as a JSON-RPC error.
            return make_response(
                req_id,
                {"content": [{"type": "text", "text": str(e)}], "isError": True},
            )

    return make_error(req_id, -32601, f"method not found: {method}")


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        resp = handle_request(msg)
        if resp is not None:
            send_message(resp)


if __name__ == "__main__":
    main()
