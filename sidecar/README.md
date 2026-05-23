# kasaspace agent sidecar

Claude Agent SDK driver. The "brain" process for the kasaspace harness: it
takes a prompt, runs the Claude Agent SDK `query()` loop, and streams every
SDK message to **stdout as JSONL** (one JSON object per line). The Rust
kasaterm host will later spawn this and render the stream natively.

It reuses the kasaspace MCP bridge (`../mcp/kasaspace_mcp.py`) as an external
stdio MCP server, so the model can drive panes on its own — split / list /
focus / send / run_job. That's the whole point: the model decides "this is
parallel work" and opens a pane for it.

## Layout

```
sidecar/
  agent_sidecar.py   # entry: stdin/--prompt -> query() -> JSONL stdout
  requirements.txt   # claude-agent-sdk
  README.md
../mcp/kasaspace_mcp.py   # MCP bridge to the kasaterm agent-socket (reused)
```

## Setup

```bash
cd sidecar
python3 -m venv .venv
. .venv/bin/activate
pip install -r requirements.txt
```

Installs cleanly on Python 3.14 too (docs say 3.10–3.13, but 0.2.87 worked).

## Run

```bash
# prompt on stdin
echo "open a second pane on the right for parallel work" | python agent_sidecar.py

# or as an arg
python agent_sidecar.py --prompt "split right, then say done"
```

Each stdout line is one message record. Top-level `type` is a stable
discriminator derived from the SDK class (`system`, `assistant`, `user`,
`result`, `hook`, `rate_limit`, ...); `_cls` keeps the raw SDK class name.
Assistant `content` blocks keep their own `type` (`text` / `tool_use` /
`tool_result`). Sidecar lifecycle records: `sidecar_start`, `sidecar_done`,
`sidecar_error`. Diagnostics go to **stderr** so stdout stays clean JSONL.

## Auth

The SDK shells out to a bundled/installed `claude` CLI.

- Primary: `ANTHROPIC_API_KEY` env var (read automatically).
- Fallback (verified on this machine): if the key is unset but the local
  `claude` CLI is logged in (subscription), `query()` runs anyway using that
  auth. A trivial `--prompt "say pong"` returned `pong` with **no API key set**.

## Pane control

Needs a live kasaterm so the MCP socket exists. The host exports
`KASATERM_SOCKET_PATH` into the sidecar's environment; the MCP bridge reads it
(fallback `CMUX_SOCKET_PATH`, then `$TMPDIR/kasaterm-<pid>.sock`). Tools are
exposed as `mcp__kasaspace__<tool>` and auto-allowed via `mcp__kasaspace__*`.

`KASASPACE_PERMISSION_MODE` (default `bypassPermissions`) — a sidecar is
non-interactive, so permission prompts would hang; bypass keeps tool calls
flowing.
