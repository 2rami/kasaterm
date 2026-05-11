import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./App.css";

type TmuxEvent =
  | { kind: "begin" | "end" | "error"; ts: string; id: string; flags: string }
  | { kind: "output"; pane_id: string; data: string }
  | { kind: "window-add" | "window-close"; window_id: string }
  | { kind: "window-renamed"; window_id: string; name: string }
  | { kind: "session-changed"; session_id: string; name: string }
  | { kind: "layout-change"; window_id: string; layout: string }
  | { kind: "pane-mode-changed"; pane_id: string }
  | { kind: "client-detached" | "exit" }
  | { kind: "unknown" | "non-protocol-line"; raw: string };

function App() {
  const [events, setEvents] = useState<TmuxEvent[]>([]);
  const [cmd, setCmd] = useState("");
  const [running, setRunning] = useState(false);
  const logRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const unlisten = listen<TmuxEvent>("tmux-event", (e) => {
      setEvents((prev) => [...prev.slice(-499), e.payload]);
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  useEffect(() => {
    logRef.current?.scrollTo({ top: logRef.current.scrollHeight });
  }, [events]);

  async function start() {
    try {
      const name = await invoke<string>("start_tmux", { sessionName: "main" });
      setRunning(true);
      console.log("attached:", name);
    } catch (e) {
      console.error(e);
    }
  }

  async function send() {
    if (!cmd.trim()) return;
    await invoke("send_tmux", { cmd });
    setCmd("");
  }

  async function stop() {
    await invoke("stop_tmux");
    setRunning(false);
  }

  return (
    <main style={{ padding: "1rem", fontFamily: "ui-monospace,monospace" }}>
      <h2>tmuxify — tmux -CC PoC</h2>

      <div style={{ display: "flex", gap: "0.5rem", marginBottom: "1rem" }}>
        <button onClick={start} disabled={running}>start (attach/new main)</button>
        <button onClick={stop} disabled={!running}>stop</button>
      </div>

      <form
        onSubmit={(e) => { e.preventDefault(); send(); }}
        style={{ display: "flex", gap: "0.5rem", marginBottom: "1rem" }}
      >
        <input
          value={cmd}
          onChange={(e) => setCmd(e.target.value)}
          placeholder="tmux command (e.g. list-windows -F '#{window_id} #{window_name}')"
          style={{ flex: 1, fontFamily: "inherit", padding: "0.3rem" }}
          disabled={!running}
        />
        <button type="submit" disabled={!running}>send</button>
      </form>

      <div
        ref={logRef}
        style={{
          height: "60vh",
          overflowY: "auto",
          background: "#111",
          color: "#ddd",
          padding: "0.5rem",
          fontSize: "12px",
          whiteSpace: "pre-wrap",
        }}
      >
        {events.map((e, i) => (
          <div key={i} style={{ marginBottom: 2 }}>
            <span style={{ color: colorFor(e.kind) }}>%{e.kind}</span>{" "}
            <span style={{ color: "#888" }}>{summarize(e)}</span>
          </div>
        ))}
      </div>
    </main>
  );
}

function colorFor(kind: string): string {
  switch (kind) {
    case "output": return "#7dcfff";
    case "begin":
    case "end": return "#9ece6a";
    case "error": return "#f7768e";
    case "window-add":
    case "window-close":
    case "window-renamed": return "#bb9af7";
    case "layout-change": return "#e0af68";
    case "exit":
    case "client-detached": return "#f7768e";
    default: return "#888";
  }
}

function summarize(e: TmuxEvent): string {
  switch (e.kind) {
    case "output": return `${e.pane_id} ${JSON.stringify(e.data.slice(0, 60))}`;
    case "begin":
    case "end":
    case "error": return `ts=${e.ts} id=${e.id} flags=${e.flags}`;
    case "window-add":
    case "window-close": return e.window_id;
    case "window-renamed": return `${e.window_id} → ${e.name}`;
    case "session-changed": return `${e.session_id} ${e.name}`;
    case "layout-change": return `${e.window_id} ${e.layout}`;
    case "pane-mode-changed": return e.pane_id;
    case "exit":
    case "client-detached": return "";
    default: return e.raw;
  }
}

export default App;
