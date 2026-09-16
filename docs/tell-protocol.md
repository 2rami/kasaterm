# Safe collaboration tell

`collab.tell` / `POST /collab/tell` accepts
`{message_id:"kt1.<unix-ms>.<unique-hex-nonce>", address:{machine_id,surface_key,surface_id,session_id,instance_id}, body,
policy:"queue"|"reject", ttl_seconds:900}`. Local CLI `%N` is resolved once to
the current local address. Automated callers should supply the complete address
from the collaboration board. The receiver validates every identity component;
missing session or instance evidence never authorizes injection. Remote requests
use a known machine route, the existing HTTP authentication/origin guard, and the
same complete address. No Claude registry, peer socket, or attach is involved.
CLI accepts `tell [--id ID] --address '<JSON>' --stdin` and retains local `%N`. It also
accepts a name (`tell 이름 …`, `tell 이름@기계 …`, `tell %N@기계 …`): the CLI resolves it
against `board --all` and refuses ambiguous matches by listing the candidates. It records
each sent ID's address locally so `tell-status ID` works without `--address`.
It prints the ID before dispatch so a disconnected caller can inspect/retry that
same ID. An old server produces an unsupported-method error; there is no fallback.

Initial HTTP requests may proxy to a known remote machine; they are not forced
to be local-only. Routing uses `known_route(machine_id)` or a configured matching
machine ID, then validates the receiver's `scope=local` snapshot and, for tell,
the full pane address. Every forwarded tell/status request sets `local_only:true`;
it must terminate at the addressed receiver, which owns the receipt.

`collab.tell_status` / `POST /collab/tell/status` accepts `{message_id,address}`.
Receipts include the ID, exact address, body hash, state, reason, and timestamps.
`accepted` means durable private storage, `dispatching` means an attempt has begun,
`submitted` means body and Enter writes succeeded, `failed` means a definite
failure without submission, and `uncertain` means submission cannot be established.
None of these states means the model read the message. A repeated ID with a
different normalized body or address is rejected; an identical retry only returns
its receipt. Never automatically retry an uncertain receipt with a new ID.

Under queue policy, `dispatching` may return to `accepted` only when the last
check before the first write detects changed input/readiness and proves no bytes
were written. Once a write is attempted, uncertainty is never requeued or retried
automatically; reject policy fails instead of requeuing a zero-write deferral.

Bodies allow LF and TAB, normalize CRLF to LF, reject remaining C0/C1/DEL controls,
and are limited to 16 KiB. The fingerprint is FNV-1a-64, with exact body comparison
as the deduplication authority. Records are capped at 1,024 and 4 MiB; exhausted
storage rejects new messages rather than forgetting live idempotency keys.
Receipts expire 24 hours after the issuance timestamp embedded in the ID. Their
storage is reclaimed, and IDs older than that window are permanently refused
with `receipt_expired`, so pruning never makes an old retry eligible to inject.
Queue TTL is at most one hour and never extends beyond receipt expiry.
Restarted dispatching records become uncertain. Expired or
replaced sessions are never retargeted. Queue policy is independent of readiness.

Delivery requires a live Claude/Codex PTY, full current identity,
an empty supported input prompt, and no approval/question or IME composition.
Working/thinking/building alone does not defer a message. Approval/question
screens defer it until the selection UI is gone. An existing draft or IME
composition is preserved; an independent message cannot be submitted through
that occupied input without changing the user's text, so it remains queued.
A guarded
input revision detects intervening writes between paste and Enter. Input is never
cleared; `--force` cannot bypass the checks. A delayed Enter also rechecks the
session, receiver process and prompt. Claude identity must match the live command's
full session UUID. Codex must expose exactly one matching root rollout through
its current open files; ambiguous or unavailable evidence defers no guess.
The receiver holds an exclusive storage lock to prevent another process from
overwriting live receipts. Unmanaged standalone background Claude sessions are unsupported.
Local injection into standalone managed web PTYs also remains unsupported until
full conversation and IME/input evidence is available. These restrictions do not
prevent a standalone or Windows sender from forwarding a complete remote address
to a known supported receiver. Remote routing runs before local injection/storage
checks. POSIX receipt storage enforces private permissions; Windows local receipt
storage is unsupported until private ACL enforcement is implemented.

## Native integration fixture

`app/kasaterm/tests/fixtures/tell_harness.rs` is a byte-recording fake harness
whose executable must be named `claude` or `codex`; it is not an external
`PtySession` with missing process metadata. The executable and fixture directory
must share a fresh temporary `kasaterm-board-*` root.
Create `ALLOW_TELL_FIXTURE` there and launch it inside an isolated native app pane
with `--session-id <full UUID> --fixture-dir <directory>`. The normal process tree
recognizes either executable. Claude identity uses that command-line UUID;
Codex keeps one root rollout file open with matching CLI session metadata.
Bind the created transcript through the existing transcript-binding API:
`<UUID>.jsonl` for Claude, `rollout-2026-09-16T00-00-00-<UUID>.jsonl` for Codex.
Both modes keep that file open and use their own empty bracketed-paste prompt.

Read the address from the real `collab.snapshot` response and pass that unchanged
to `collab.tell`. Inspect both receipt transitions and `input.bin`/`submitted.json`:
accepted alone is insufficient; `submitted` must accompany one recorded Enter and
the exact body. Reusing the same ID must not increase the submit count. `state.txt`
selects states including `busy`, `approval`, `question`, `draft`,
`draft-multiline`, `attachment`, `attachment-below`, `placeholder-draft`, and
`quit-after-paste`. Busy must submit; approval/question/draft/attachment states
must retain an accepted receipt and no injected bytes. Changing the fixture state
back to busy permits delivery. `quit-after-paste` exits after paste and must
result in uncertain without automatic reinjection.
Restarting a different fixture process in the seat must invalidate the old queue.
This exercise uses the normal Backend, GUI event loop and managed PTY without
adding a test bypass to production identity or input checks. No real account,
model executable, transcript directory, application instance or CLI is involved.
