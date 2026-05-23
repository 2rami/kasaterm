// kasaspace sidecar (the "brain"). This program OWNS the run: it calls the
// Claude Agent SDK's query() to drive Claude, and registers our in-process
// kasaspace tools so Claude can manipulate kasaterm panes directly. Claude
// is the worker; this sidecar is the foreman that starts it and hands it
// the tools.
//
// Run:  node src/index.ts --prompt "오른쪽에 pane 만들어줘"
//   or  echo "..." | node src/index.ts
//
// Auth: the SDK drives the Claude Code engine, which uses whatever auth the
// local `claude` install already has (logged-in subscription). If that
// can't be found, set ANTHROPIC_API_KEY or pathToClaudeCodeExecutable.

import { query, createSdkMcpServer } from "@anthropic-ai/claude-agent-sdk";

import { kasaspaceTools } from "./tools.ts";

const SYSTEM_PROMPT = [
  "You are an agent running inside kasaterm, a GUI terminal with split panes.",
  "You control the workspace through the kasaspace tools: list, focus, split,",
  "send, send_key, close, rename, set_color, swap, workspace_list,",
  "workspace_current, and run_job. Prefer USING these tools to manipulate the",
  "workspace directly over describing what to do. When work fans out into",
  "parallel tracks, or a command will run for a while (builds, deploys, dev",
  "servers, sub-tasks), open a separate pane with kasaspace_run_job so the user",
  "watches progress live instead of blocking the main pane.",
].join(" ");

async function readPrompt(): Promise<string> {
  const i = process.argv.indexOf("--prompt");
  if (i >= 0 && process.argv[i + 1]) return process.argv[i + 1];
  if (!process.stdin.isTTY) {
    const chunks: Buffer[] = [];
    for await (const c of process.stdin) chunks.push(c as Buffer);
    const t = Buffer.concat(chunks).toString("utf8").trim();
    if (t) return t;
  }
  throw new Error("no prompt: pipe one on stdin or pass --prompt <text>");
}

async function main(): Promise<void> {
  const prompt = await readPrompt();
  const server = createSdkMcpServer({
    name: "kasaspace",
    version: "0.1.0",
    tools: kasaspaceTools,
  });

  for await (const msg of query({
    prompt,
    options: {
      mcpServers: { kasaspace: server },
      allowedTools: ["mcp__kasaspace__*"],
      systemPrompt: SYSTEM_PROMPT,
      permissionMode: "bypassPermissions",
    },
  })) {
    // Human-readable for now; the Rust host will consume this as JSONL later.
    if (msg.type === "assistant") {
      for (const block of msg.message.content) {
        if (block.type === "text") {
          process.stdout.write(block.text + "\n");
        } else if (block.type === "tool_use") {
          process.stderr.write(`[tool] ${block.name} ${JSON.stringify(block.input)}\n`);
        }
      }
    } else if (msg.type === "result") {
      process.stderr.write(`[result] ${(msg as { subtype?: string }).subtype ?? "?"}\n`);
    } else if (msg.type === "system" && (msg as { subtype?: string }).subtype === "init") {
      const m = msg as { session_id?: string; mcp_servers?: unknown };
      process.stderr.write(
        `[init] session=${m.session_id ?? "?"} mcp=${JSON.stringify(m.mcp_servers ?? [])}\n`,
      );
    }
  }
}

main().catch((e) => {
  process.stderr.write(`sidecar error: ${e instanceof Error ? e.message : String(e)}\n`);
  process.exit(1);
});
