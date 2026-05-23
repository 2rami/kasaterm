// agent-socket client. The kasaterm host binds a unix-domain socket and
// exports KASATERM_SOCKET_PATH into every child shell, so a sidecar running
// inside a pane already has the path in its env. One request/response round
// trip per call: connect, write one JSON line, read one JSON line, close.
//
// Wire shape mirrors crates/agent-socket/src/protocol.rs:
//   request:  { id, method, params }
//   response: { id, ok, result?, error? }

import net from "node:net";

function resolveSocketPath(): string | undefined {
  return process.env.KASATERM_SOCKET_PATH || process.env.CMUX_SOCKET_PATH;
}

interface RpcResponse {
  id: unknown;
  ok: boolean;
  result?: unknown;
  error?: { code?: number; message?: string };
}

/** One JSON-RPC round trip against the kasaterm agent-socket. Rejects on
 *  transport failure or an `ok: false` reply so callers can fold it into a
 *  tool error the model can see. */
export function agentRpc(
  method: string,
  params: Record<string, unknown> = {},
  timeoutMs = 5000,
): Promise<Record<string, unknown>> {
  return new Promise((resolve, reject) => {
    const sockPath = resolveSocketPath();
    if (!sockPath) {
      reject(new Error("KASATERM_SOCKET_PATH not set — is the sidecar running inside a kasaterm pane?"));
      return;
    }
    const payload = JSON.stringify({ id: `ts-${Date.now()}`, method, params }) + "\n";
    const sock = net.createConnection(sockPath);
    let buf = "";
    const timer = setTimeout(() => {
      sock.destroy();
      reject(new Error(`agent-socket timeout on ${method}`));
    }, timeoutMs);

    sock.on("connect", () => sock.write(payload));
    sock.on("data", (chunk) => {
      buf += chunk.toString("utf8");
      const nl = buf.indexOf("\n");
      if (nl < 0) return; // wait for the full line
      clearTimeout(timer);
      sock.end();
      try {
        const resp = JSON.parse(buf.slice(0, nl)) as RpcResponse;
        if (!resp.ok) {
          reject(new Error(`agent-socket ${resp.error?.code ?? "?"}: ${resp.error?.message ?? "unknown"}`));
          return;
        }
        resolve((resp.result as Record<string, unknown>) ?? {});
      } catch (e) {
        reject(new Error(`bad JSON from agent-socket: ${(e as Error).message}`));
      }
    });
    sock.on("error", (e) => {
      clearTimeout(timer);
      reject(new Error(`agent-socket transport error: ${e.message}`));
    });
  });
}
