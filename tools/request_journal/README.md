# Request journal

A separate, loopback-only request journal. It does not restart kasaterm or agent
sessions. Student completion reports and actual application status remain separate.
The database is operational app data, not a MEMORY vault note.

## Chat and restart checks

The main web surface asks Nacho about this project's requests and restart checks;
raw journal records remain in a collapsed evidence section. The native pet uses
the same server-owned `pet` conversation. Neither surface executes model output.

The baseline is the observed operating-system app process start time, never the
journal service's start time. A separate background RuntimeObserver records that
epoch and artifact evidence. A verified historical manifest is not a currently
ready build when the present bundle no longer matches its hashes. A main process
match does not certify the separate pet process or user-visible behavior.

Every request in the runtime window participates, without a recent-50/500 cutoff.
Long prompts and final reports remain intact in the core journal. A request's
existing Nacho summary is reused only when its source hash still matches; otherwise
the note is explicitly labelled an unverified excerpt. Source/session, original
timestamp, and the previous request in that same source travel with each note.
Notes are cached independently of app epochs and build classification. If the
compact complete context fits 28,000 characters, it is synthesized in one call;
only larger contexts use hierarchical batches. Coverage includes every request.
Code changes use the observer's fixed git evidence and validated revision queries.
The model's checklist JSON is parsed and its request/evidence IDs checked against
known records. It may refine grouping and observation steps, never promote a
state to built, running, or user-confirmed. Partial failures retain default checks.
During model calls only, long evidence/request hashes use compact job-local
aliases. Replies are expanded back to the original IDs before validation or
storage; original text remains in the journal and source identity is preserved. This prevents
repeated hashes from exhausting the model's response and merge budgets.

Only previously generated unresolved checklist items and explicitly pending older
requests carry across app runs. `chat_pending_checks` preserves them through the
second and subsequent questions and journal restarts; old unknown requests are
not all converted into pending work. All chat tables use a `chat_` prefix and do
not change SQLite `user_version` or overwrite core request evidence.

| Route | Contract |
| --- | --- |
| `POST /api/chat` | `{text,conversation_id?,client_request_id?}` → 202 `{job_id,conversation_id,status,user_message_id}` |
| `GET /api/chat/jobs/<id>` | Bounded status/progress, text preview, provider, context, `checklist_url`, `history_after` |
| `DELETE /api/chat/jobs/<id>` | Empty JSON object cancels that job; question history remains |
| `GET /api/chat/history` | `conversation_id`, `limit` up to 20, `before` or `after`; returns messages plus `next_before`/`next_after` |
| `GET /api/checklist` | `job_id?`, `offset`, `limit` up to 20; returns items, compact coverage, context and `next_offset` |
| `GET /api/checklist/evidence` | `job_id?`, `item_id`, `offset`; pages all source/evidence IDs in groups of 100 |

POST and DELETE require the same JSON, Host, Origin, and `X-Journal-Request: 1`
checks as acknowledgements. One inference job runs at a time and four may wait.
Repeated client request IDs are idempotent within their project/conversation.
Cancellation and a 180-second time budget signal the provider to clean up its
temporary work. Cache keys cover app epoch, build evidence, and changed requests.

Restart answers always save concrete checklist titles and steps into assistant
history, including fallback/error answers. Large answers are preserved as ordered
messages of at most about 32 KiB JSON each. After a job completes, clients read
history forward from `after=user_message_id-1` until `next_after` is null. Older
conversation pages use `before`/`next_before`. Job metadata never carries the full
checklist; the checklist/evidence endpoints page it separately so a large UTF-8
conversation cannot exceed the native pet's response budget.

## Run

From the repository root, Python 3.10 or newer, no third-party packages:

```sh
python3 -m tools.request_journal run --project "$PWD"
```

The service prints its URL and records it in `service.json`. Port 0 (the default)
asks the operating system for an unused port; no existing tunnel or listener is
stopped. A fixed port can be requested with `--port`. This mode reads without collecting live
transcripts. Add `--collect` to explicitly enable the project-scoped poller.
The collector's kasaterm endpoint defaults to `http://127.0.0.1:8765`; override it
with `--base-url`. Structured summaries run in a separate 30-second worker.
External LLM calls are disabled unless `--llm` is explicitly passed. That mode
uses only already-injected process environment credentials; it never opens a
key file. `--nacho-repo` optionally selects Nacho's existing transport repository.
The UI explicitly shows when the Nacho summary provider is unavailable.
Alternatively, `--nacho-http http://127.0.0.1:18795` explicitly uses Nacho through
an existing local tunnel. This does not require `--llm` or copying a key; the
remote bot's existing client handles authentication. The two provider options
are mutually exclusive. A failed provider falls back to structured summaries.
`--data-dir` defaults to `~/.config/kasaterm/request-journal`. `--interval` defaults
to 5 seconds. Tests use temporary databases and an ephemeral loopback port.

## macOS lifecycle

```sh
python3 -m tools.request_journal install --project "$PWD"
python3 -m tools.request_journal install --project "$PWD" --apply
python3 -m tools.request_journal stop --apply
```

Install without `--apply` prints the proposed launch agent without writing it.
Installation starts collection and loads `com.kasaterm.request-journal` into the
current user's launchd domain. Stop unloads that exact service; it does not kill
kasaterm, agents, or delete journal data. A retained launch agent loads again at
the next login. Existing installations are not overwritten implicitly.
Use `install --replace --apply` to reload this journal with changed provider
options. Replacement checks the existing label, module, project, and data path;
another installation is refused. The database and collection offsets remain in
place. If loading the new configuration fails, the previous configuration is
restored and launchd is asked to restart it. No app, student, or bot is restarted.

## API v1

All responses are JSON except static UI assets. Bind address is always
`127.0.0.1`. Only the bound port's `127.0.0.1` and `localhost` Host headers are
accepted. An Origin, if present, must match that exact HTTP origin. No CORS
permission is granted. Writes require both `Content-Type: application/json`
and `X-Journal-Request: 1`. Bodies are limited to 64 KiB.

| Route | Response |
| --- | --- |
| `GET /health` | `{ok:true, service:"request-journal", version:1}` |
| `GET /api/requests` | `{requests:[], next_before:null, project:"..."}` |
| `GET /api/requests/<id>` | Full request or 404 |
| `GET /api/summary` | `{version:1, project, counts, latest, needs_confirmation:[]}` |
| `GET /api/ask?q=...` | `{version:1, project, text, url}` |
| `POST /api/requests/<id>/ack` | Updated full request |
| `POST /api/pet-summary` | `{ok,pet_running,state,message}` |

The list accepts `limit` (1–100), `before` (opaque request ID),
`reported_status`, and `project`. The project must match the service's configured
project; one running service does not expose other projects from its database.
The summary's `counts` comes from `Store.stats`; `needs_confirmation` contains
at most 5 matches from the latest 50 requests with a completion report but no
confirmed application. `latest` and these matches are bounded objects containing
only `id`, `summary` (120 characters), `prompt_preview` (80 characters),
`created_at`, `reported_status`, and `applied_status`. Full prompts, final reports,
and update evidence remain available from `/api/requests/<id>`. This
endpoint is intended for later native pet integration; it sends no pet messages.
It also returns `text` and `waiting_text` (each at most 250 characters), a `url`
for the detailed journal, and `summarizer` provider status. `/api/ask` chooses
between these cached-status answers; it does not issue LLM calls. On startup,
`service.json` in the data directory records `{version:1,base_url,project}` with
owner-only permissions (0600), allowing a native pet to discover this service.
Only explicit `pending` and `restart_required` evidence is called application or
restart waiting. `unknown` is described as needing confirmation, never as proof
that restarting the app will implement a request.

Acknowledgement body:

```json
{"applied_status":"applied","evidence":"Observed the requested menu in the app"}
```

The UI's “아직이에요” uses `pending`. Acknowledgements never change student
reported status, and a final answer never automatically confirms application.
Request text is inserted through DOM `textContent`, never interpreted as HTML.
The “곽향에 요약 표시” button explicitly posts an empty JSON object to
`/api/pet-summary`. Only that user action sends the saved, source-labelled
summary through the existing local `kasaterm-cli pet-say` path. Collection and
summarization never send pet messages automatically. When the pet is stopped,
the result says the summary is queued, not visible. It never starts or restarts
the pet and never sends Slack messages. `text_source` in the summary response
distinguishes Nacho-generated excerpts from the structured fallback.
HTTP access logs are suppressed, and service errors do not contain transcript
text. Local processes and the current user's browser can read the journal;
this is not an authentication boundary against other local software.
