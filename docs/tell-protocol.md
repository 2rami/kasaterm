# Safe collaboration tell

`collab.tell` / `POST /collab/tell` accepts
`{message_id:"kt1.<unix-ms>.<unique-hex-nonce>", address:{machine_id,surface_key,surface_id,session_id,instance_id}, body,
policy:"queue"|"reject", ttl_seconds:3600, title?, notify?}`. Optional `title` (one line, at most 60 characters) is the
receiver's current work: once the message is `submitted`, the receiving machine renames that pane to it (pinned,
so the sidebar, pane header and board `title` all follow) and hands it to that pane's next turn-start hook, which
returns it as the UserPromptSubmit `sessionTitle` — Claude's own session name (`/resume`, agents list, the rename
slot on the input box) follows without typing `/rename` into the input. It is not part of the message identity. CLI:
`tell … --title "지금 일"`; `summon` sends `--name` or the brief's `목적:` line as the title. Local CLI `%N` is resolved once to
the current local address. Automated callers should supply the complete address
from the collaboration board. The receiver validates every identity component;
missing session or instance evidence never authorizes injection. Remote requests
use a known machine route, the existing HTTP authentication/origin guard, and the
same complete address. No Claude registry, peer socket, or attach is involved.
CLI accepts `tell [--id ID] --address '<JSON>' --stdin` and retains local `%N`. Options go between the target and
the body; an unknown `--option` there, or a known one after the body, is an error — an older CLI that did not know
`--title` sent `--title … --stdin` as the body and dropped the real one (2026-10-01). A body that begins with `--`
goes after `--`. Receivers reject a body that starts with `--title`, `--stdin`, `--id`, `--address` or `--force`
(after an optional `⟦sender⟧` marker), so a CLI too old to know an option fails loudly instead of delivering it. It also
accepts a name (`tell 이름 …`, `tell 이름@기계 …`, `tell %N@기계 …`): the CLI resolves it
against `board --all` (machine labels compare with any whitespace equal — macOS names use U+00A0) and refuses
ambiguous matches by listing the candidates. It records
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
A Claude pane whose in-session bridge mod is live (`docs/claude-mod-bridge.md`) is pasted into like any other.
From 2026-10-02 to 2026-10-06 such a pane got the message through the mod's `$.prompt.submit` instead. The engine
runs a plugin's prompt only once the session is idle, so a working pane held every message as `waiting:busy` until
its turn ended, and the receiver's transcript showed it under a "The kasaterm-bridge plugin sent a message" header.
A paste with Enter starts a turn in a resting pane and joins the running turn in a working one, the way a person's
mid-turn message does. Older receivers may still report `waiting:busy`; the CLI keeps reading it.
Working/thinking/building alone does not defer a message. Approval/question
screens defer it until the selection UI is gone. An existing draft or IME
composition is preserved; an independent message cannot be submitted through
that occupied input without changing the user's text, so it remains queued.
The screen decides whether the input is empty. A keystroke that is not a separate
Enter marks a draft that is trusted for 5 seconds only (the echo or an image
attachment lands well inside that); after that the screen is authoritative. Mouse
reports never mark a draft. Before this, the mark cleared only on Enter, so one Esc,
Ctrl+C, arrow or click held every later message until the 15-minute expiry while
the input box was visibly empty — 21 of 61 messages on one machine in a day.

Each deferral writes a specific reason, `waiting:<word> — …`, with `<word>` one of
`draft` (input box has text), `typing` (keystroke in the last 5 s), `composition`,
`approval`, `closed`, `paste_mode` or `identity` (`busy` only from older receivers, above).
Messages to one pane go in order and only the oldest is examined; a deferral copies its
reason onto the messages queued behind it on that pane, so every receipt, the sender
notice and the waiting count tell the same story. Before this, a later message kept its
`stored; waiting for safe empty input` until its turn and looked like it had taken
another path (2026-10-06: a name tell said `waiting:busy`, an address tell sent two
seconds later to the same pane still said `stored`, and its sender notice blamed an
old receiver). The CLI prints the reason in Korean with
the expiry time. The receiving pane shows `쪽지 N 대기 · <how to release>` on the
bottom border of its input box while any of its messages has been deferred.

Queue TTL defaults to 3600 s, the maximum. On 2026-10-01 a day of receipts had
21 of 61 messages expire at the old 900 s; the cause was the Enter-only draft mark
above, and the late deliveries were released at the moment someone pressed Enter.
What remains is a person's real draft or an approval screen, which lasts as long
as that person is away, and 9 of the 21 were completion reports whose meaning does
not age. Waiting is visible in the receipt and on the receiving pane, so the longer
queue does not hide anything.

`notify:{surface,label}` is accepted only on the local socket (the CLI adds the
sender's `$KASATERM_PANE_ID`; never with `--api`). The sending app removes it before
routing and watches the receipt every 20 s. It never puts anything into the sender
pane as a prompt: those notices (`[쪽지 대기]` after 2 minutes queued, `[쪽지 못 감]`,
`[쪽지 확인 못 함]`) were submitted turns, and on 2026-10-06 they woke senders several
times an hour mid-work. Waiting and `uncertain` stay in the receipt (`tell --status`)
and on the receiver's `쪽지 N 대기` label. Only `failed` (dropped) is shown, as a
20 s toast in the sender pane through its connection mod (`/claude-mod/notices`,
`docs/claude-mod-bridge.md`): `쪽지 못 감 → <receiver> — <why>. «<first line>»`.
A toast leaves the transcript and the model untouched, so no turn starts. A status
line entry was not used because it stays until something clears it and no one owns
that; a board mark was not used because board changes stream to `board-watch`
subscribers, often Monitor-driven sessions, which would move the wake-up elsewhere.
A sender without the mod (codex, claude without it) gets no notice; the receipt is
the record. `submitted` and `uncertain` end the watch silently.
A guarded
input revision detects intervening writes between paste and Enter. Input is never
cleared; `--force` cannot bypass the checks. A delayed Enter also rechecks the
session, receiver process and prompt.

Enter follows a paste only once the paste is seen inside the input box: the first or
the last 24 non-whitespace characters of the body, or a harness's collapsed-paste
marker (`[Pasted text #`, `[...Truncated text #`, Codex `[Pasted Content`), compared
with whitespace removed so wrapping cannot split them. The tail matters in narrow
panes: Claude shows only the last lines of a long input, around the cursor, so a
395-character body in a 52-column pane had its head scrolled out of view and a
head-only check withheld Enter with the whole body sitting in the box (2026-10-05:
21 of 35 receipts on one machine were withheld, and each leftover body then held the
next message as `waiting:draft`). While the input revision is unchanged the receiver
waits up to 15 s for the echo, because a freshly started Claude drew its first paste
0–15 s late (an idle pane with history draws it in 0.1–0.4 s); a person's keystrokes
for that pane are held meanwhile and replayed in order afterwards. The identity is
proven again once the echo is seen. A proof that is late (older than 1 s when the GUI
judges it) or that found the proof workers busy is renewed instead of withholding the
pasted body; a changed identity, changed input, approval screen or expiry still
withholds at once.

Claude identity must match the live command's
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
