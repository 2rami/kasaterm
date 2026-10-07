# Collaboration board contract

The native board, CLI and HTTP clients share `Backend::collab_snapshot`,
`collab_changes` and `collab_inspect` (JSON parameters and results).
`collab_board_source()` is the local, harness-independent producer. It never
queries Claude's agent registry or remote peers.

`collab.snapshot {scope:"all"|"local"}` / `GET /collab/board?scope=all` returns:

```json
{"schema_version":1,"scope":"all","cursor":"epoch:sequence","observed_at_ms":0,
 "sources":[{"machine_id":"stable-machine","label":"Machine","state":"online",
             "observed_at_ms":0,"complete":true,"is_local":true,"source_kind":"desktop",
             "capabilities":["live_places","rooms","bounded_activity"]}],
 "panes":[{"id":"stable-machine/stable-surface","address":{
     "machine_id":"stable-machine","surface_key":"stable-surface","surface_id":"%1",
     "session_id":"optional-session","instance_id":"optional-process-identity"},
   "machine_label":"Machine","room_id":"room","room_label":"Room",
   "character":null,"harness":null,"title":"","request":"","progress":"",
   "status":"unknown","status_reason":"no supported activity evidence",
   "observed_at_ms":0,"freshness":"fresh"}],"recent_changes":[]}
```

Pane identity is machine ID plus persistent surface key. Session and instance
IDs validate control, not identity. A closed/detached seat is not proof that its
agent exited. Unsupported activity stays `unknown`. No connection tokens appear
in this contract. Source failures retain the last pane rows with stale freshness;
only a complete, successful source can prove that its own pane disappeared.

Optional `origin_task_env` names the nacho task ID that was in the pane's env when
its session was bound (`nacho-origins.json`). It is display provenance only — it
can point at an older task after a live pane is handed new work, so never use it
to register or accept reports; the task↔pane source of truth is nacho's ledger
`(machine_id, surface_key)`. Conversation IDs and bodies are not carried.

Optional `background` and `subagents` list the pane's still-running background
shells/monitors and subagents (short labels, at most 8, secret-looking labels
become `[redacted]`). A pane whose turn closed (`waiting`/`idle`) but carries a
non-empty list is still working in the background — do not hand it new work.

`collab.changes {since:"epoch:sequence",limit:100,scope:"all"}` /
`GET /collab/changes?since=...` returns `cursor`, `changes`, `reset_required`,
`reset_reason`, `has_more`. Changes carry `cursor`, `at_ms`, `kind`, optional
`pane_id`, `machine_id`, `summary`, and changed field names. Restart, corruption
or an expired cursor requires a new snapshot. The bounded private journal stores
only short metadata summaries, never transcript bodies, tool arguments or images.
This is periodic observation, not a complete audit of every tool invocation.

`collab.inspect {address:{...},limit:20}` / `GET /collab/inspect` returns only the
selected pane's bounded activity. Parameters require machine, surface key,
surface ID and (when present on the pane) matching session/instance identity.
HTTP uses the existing terminal authentication and origin guard. Remote source
requests always use `scope=local`, preventing recursive federation.

Optional `spawned_by` is the `surface_key` of the pane that split or tabbed this one
(`summon`, `split`, `tab` from a pane), read from that machine's `spawned-by.json`;
`null` when unknown. Mirror rows never carry it — the source machine's row is canonical.

`collab.peek {address,lines}`, `collab.capture {address,path,max_width}` and
`collab.where {machine_id}` / `GET /collab/peek|capture|where?params=...` read a pane's
screen text, a pane's PNG, and a machine's room layout. They take the same row
`address` as inspection (`where` takes a machine ID; empty means this machine) and
follow the same route: verified direct → roster `base` → gateway relay. The source
validates surface key and identity, answers only its own panes (`local_only`), caps
text at 256 KiB and PNGs at 2.5 MB. Over HTTP a capture always returns
`png_base64` and ignores any `path`; the requesting machine writes the file. CLI:
`peek|capture %N@machine` (or `name@machine`, or `--address JSON`) and
`where --machine <label|id>`.

`board --all|--local` selects this API. `board-watch --all --json --since CURSOR`
streams changes and reports reset requirements explicitly. Legacy invocations
without the new switches retain their output format.

An observer (including Nacho) first reads `board --all`, keeps its cursor, then
uses `board-watch --all --json --since CURSOR`. The stream includes the observer's
own pane. Without `--since`, it emits an initial snapshot with its cursor.
An expired or changed epoch emits `reset_required` and exits unsuccessfully;
read a new snapshot to resume. Inspect only a selected row with
`activity --address '<address JSON from that row>' [limit]` (maximum 50).
These commands provide observations; they do not enable automatic instructions
or restart any Nacho service.

Sources identify their origin with `is_local`, `source_kind` and `capabilities`.
Standalone hosts expose owned live terminals with unknown activity, explicitly
advertising unsupported activity and room bindings. Unsupported old APIs,
authentication failures, identity mismatches and stale observations have short
error categories; connection URLs and authentication tokens are not journaled.
The journal holds at most 1,000 metadata changes and 256 KiB, independently of
session restore files, under the session storage directory's `collaboration/`
subdirectory. A collector restart preserves retained history but marks an
observation gap and starts a new epoch. Summary text is cached in memory only.

Verification processes use ephemeral machine IDs, isolated journal paths and no
remote observer. A validated native board fixture supplies a synthetic local
source without querying PTYs, transcripts or the operating machine roster.
`CollectorConfig` can inject a journal path and synthetic source for tests.
Headless production servers still collect real managed terminals and remotes;
disabling the scheduler alone does not enable verification mode.

Pane change events include `room_id` and `room_label`; removal events retain the
last observed room. Machine-level events have no `pane_id` or room fields.

HTTP-only hosts need no Unix socket. Use the explicit global option
`kasaterm-cli --api http://127.0.0.1:8765 board --all`, with the same prefix for
`board-watch`, `rooms`, `activity --address`, `peek`, `capture`, `where`, `tell`, and `tell-status`. The CLI maps only
the new collaboration RPCs to their guarded HTTP endpoints, using system curl
with redirects disabled and bounded responses. `--api-token-file FILE`, before
the command, reuses an existing server token when authentication is required;
the token and message body travel through stdin rather than process arguments.
There is no automatic server discovery or fallback to legacy sending routes.

`rooms` renders this same snapshot by machine and room. It never treats a peer
name as a contact address. Self/room markers require a unique local source ID,
matching caller machine ID and current pane, plus a known room ID. An explicit
HTTP target does not establish the caller's own server instance, so it does not
infer self markers. Use the selected row's complete `address` from `board --all`
when contacting a pane; a displayed pane number is not a global address.

Each desktop row captures its binding path, session identity, surface key and PTY
instance before reading text. It checks these again before publication without
holding locks during file I/O. Changed bindings publish no old text or completion
summary and make that source observation incomplete until the next stable pass.
Codex activity accepts both legacy messages and `item_completed` messages. Tool
results admit named text fields only; image/binary blocks and unknown payload
fields are never serialized into a text summary.

Inspection and delivery share `known_route(machine_id)`, which accepts a route
learned from the local-source board even when the configured machine had only a
base URL. Removed routes, stale source observations and contradictory identity
claims fail closed. Authentication remains in the existing connection layer.
