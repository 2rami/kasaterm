// In-process kasaspace tools. Each tool() handler is plain TypeScript that
// reaches the kasaterm host over the agent-socket (see socket.ts) — no
// external MCP process. The SDK exposes these to Claude as
// `mcp__kasaspace__<name>`; Claude reads each `description` to decide when
// to call them.

import { tool } from "@anthropic-ai/claude-agent-sdk";
import { z } from "zod";

import { agentRpc } from "./socket.ts";

const JOB_COLOR = "#5b7fa6";

function ok(text: string) {
  return { content: [{ type: "text" as const, text }] };
}
// Tool-level failure: surfaced to Claude via isError so it can react,
// not thrown as a transport error.
function fail(text: string) {
  return { content: [{ type: "text" as const, text }], isError: true };
}
function errText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

const DIRECTION = z.enum(["left", "right", "up", "down"]);

export const kasaspaceTools = [
  tool(
    "kasaspace_list",
    "List kasaterm surfaces (panes) and workspaces. Returns surface ids you pass to the other kasaspace tools.",
    {},
    async () => {
      try {
        const surfaces = await agentRpc("surface.list");
        let workspaces: unknown = [];
        try {
          workspaces = (await agentRpc("workspace.list")).workspaces ?? [];
        } catch {
          /* workspace.list optional */
        }
        return ok(
          JSON.stringify({ surfaces: surfaces.surfaces ?? surfaces, workspaces }, null, 2),
        );
      } catch (e) {
        return fail(`list failed: ${errText(e)}`);
      }
    },
  ),

  tool(
    "kasaspace_focus",
    "Focus a specific kasaterm pane by its surface id.",
    { surface_id: z.string().describe("Surface id from kasaspace_list.") },
    async (a) => {
      try {
        await agentRpc("surface.focus", { surface_id: a.surface_id });
        return ok(`Focused ${a.surface_id}`);
      } catch (e) {
        return fail(`focus failed: ${errText(e)}`);
      }
    },
  ),

  tool(
    "kasaspace_split",
    "Split the current pane to create a new pane in the given direction. Use when work fans out into parallel tracks.",
    { direction: DIRECTION.describe("Where the new pane opens relative to the current one.") },
    async (a) => {
      try {
        const r = await agentRpc("surface.split", { direction: a.direction });
        return ok(`Split ${a.direction}. New surface: ${JSON.stringify(r.surface ?? r)}`);
      } catch (e) {
        return fail(`split failed: ${errText(e)}`);
      }
    },
  ),

  tool(
    "kasaspace_send",
    "Send text to a pane (e.g. a shell command — add a trailing newline to run it). Targets the focused pane unless surface_id is given.",
    {
      text: z.string().describe("Text to type into the pane. Add a trailing newline to submit."),
      surface_id: z.string().optional().describe("Optional target surface id; defaults to the focused pane."),
    },
    async (a) => {
      try {
        const params: Record<string, unknown> = { text: a.text };
        if (a.surface_id) params.surface_id = a.surface_id;
        await agentRpc("surface.send_text", params);
        return ok(`Sent ${a.text.length} chars to ${a.surface_id ?? "focused pane"}`);
      } catch (e) {
        return fail(`send failed: ${errText(e)}`);
      }
    },
  ),

  tool(
    "kasaspace_send_key",
    "Send a named key (enter/tab/escape/backspace/delete/up/down/left/right) to a pane. Targets the focused pane unless surface_id is given.",
    {
      key: z.string().describe("Key name: enter|tab|escape|backspace|delete|up|down|left|right."),
      surface_id: z.string().optional().describe("Optional target surface id; defaults to the focused pane."),
    },
    async (a) => {
      try {
        const params: Record<string, unknown> = { key: a.key };
        if (a.surface_id) params.surface_id = a.surface_id;
        await agentRpc("surface.send_key", params);
        return ok(`Sent key ${a.key} to ${a.surface_id ?? "focused pane"}`);
      } catch (e) {
        return fail(`send_key failed: ${errText(e)}`);
      }
    },
  ),

  tool(
    "kasaspace_close",
    "Close (kill) a pane by its surface id, removing it from the layout.",
    { surface_id: z.string().describe("Surface id from kasaspace_list.") },
    async (a) => {
      try {
        await agentRpc("surface.close", { surface_id: a.surface_id });
        return ok(`Closed ${a.surface_id}`);
      } catch (e) {
        return fail(`close failed: ${errText(e)}`);
      }
    },
  ),

  tool(
    "kasaspace_rename",
    "Rename a pane's header title.",
    {
      surface_id: z.string().describe("Surface id from kasaspace_list."),
      title: z.string().describe("New header title for the pane."),
    },
    async (a) => {
      try {
        await agentRpc("surface.rename", { surface_id: a.surface_id, title: a.title });
        return ok(`Renamed ${a.surface_id} -> ${a.title}`);
      } catch (e) {
        return fail(`rename failed: ${errText(e)}`);
      }
    },
  ),

  tool(
    "kasaspace_set_color",
    "Set a pane's accent color (header band) as #rrggbb.",
    {
      surface_id: z.string().describe("Surface id from kasaspace_list."),
      color: z.string().describe("Accent color as #rrggbb."),
    },
    async (a) => {
      try {
        await agentRpc("surface.set_color", { surface_id: a.surface_id, color: a.color });
        return ok(`Set ${a.surface_id} color to ${a.color}`);
      } catch (e) {
        return fail(`set_color failed: ${errText(e)}`);
      }
    },
  ),

  tool(
    "kasaspace_swap",
    "Swap two panes' positions in the layout (content stays, positions trade).",
    {
      a: z.string().describe("First surface id."),
      b: z.string().describe("Second surface id — its position is swapped with `a`."),
    },
    async (a) => {
      try {
        await agentRpc("surface.swap", { a: a.a, b: a.b });
        return ok(`Swapped ${a.a} <-> ${a.b}`);
      } catch (e) {
        return fail(`swap failed: ${errText(e)}`);
      }
    },
  ),

  tool(
    "kasaspace_workspace_list",
    "List workspaces (tmux sessions / cmux workspaces).",
    {},
    async () => {
      try {
        const r = await agentRpc("workspace.list");
        return ok(JSON.stringify(r.workspaces ?? r, null, 2));
      } catch (e) {
        return fail(`workspace_list failed: ${errText(e)}`);
      }
    },
  ),

  tool(
    "kasaspace_workspace_current",
    "Get the current (focused) workspace.",
    {},
    async () => {
      try {
        const r = await agentRpc("workspace.current");
        return ok(JSON.stringify(r.workspace ?? r, null, 2));
      } catch (e) {
        return fail(`workspace_current failed: ${errText(e)}`);
      }
    },
  ),

  tool(
    "kasaspace_run_job",
    "Run a long-running command in a NEW labelled pane so the user watches progress live. Splits a fresh pane, labels its header with the job title + accent color, then types the command. Use for builds, deploys, dev servers, sub-agents. Output stays visual in the pane — it is not streamed back to you.",
    {
      command: z.string().describe("Shell command to run in the new pane (no trailing newline needed)."),
      title: z.string().optional().describe("Short human label for the pane header (e.g. 'build', 'tests')."),
      direction: DIRECTION.optional().describe("Where the job pane opens. Defaults to 'down'."),
      color: z.string().optional().describe("Optional #rrggbb accent for the header. Defaults to a blue-gray."),
      auto_close: z
        .boolean()
        .optional()
        .describe("If true, the pane closes itself when the command finishes (appends '; exit'). Use for sub-agents that should vanish when done."),
    },
    async (a) => {
      try {
        const dir = a.direction ?? "down";
        const split = await agentRpc("surface.split", { direction: dir });
        const surf = split.surface as { id?: string } | undefined;
        const id = surf?.id;
        if (!id) return fail("run_job: split returned no surface id");

        const title = a.title ?? a.command.split("\n")[0].slice(0, 40);
        const labels: string[] = [];
        try {
          await agentRpc("surface.rename", { surface_id: id, title });
          labels.push(`title=${title}`);
        } catch (e) {
          labels.push(`rename skipped (${errText(e)})`);
        }
        const color = a.color ?? JOB_COLOR;
        try {
          await agentRpc("surface.set_color", { surface_id: id, color });
          labels.push(`color=${color}`);
        } catch (e) {
          labels.push(`color skipped (${errText(e)})`);
        }

        let cmd = a.command.replace(/\n+$/, "");
        if (a.auto_close) {
          cmd += "; exit";
          labels.push("auto_close");
        }
        await agentRpc("surface.send_text", { surface_id: id, text: cmd + "\n" });
        return ok(`Started job in pane ${id} (${dir}; ${labels.join(", ")}): ${a.command}`);
      } catch (e) {
        return fail(`run_job failed: ${errText(e)}`);
      }
    },
  ),
];
