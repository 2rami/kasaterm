# Request journal

A separate, loopback-only request journal. It does not restart kasaterm or agent
sessions. Student completion reports and actual application status remain separate.
The database is operational app data, not a MEMORY vault note.

## Run

From the repository root, Python 3.10 or newer, no third-party packages:

```sh
python3 -m tools.request_journal run --project "$PWD"
```

Open `http://127.0.0.1:18769`. This mode reads the journal without collecting live
transcripts. Add `--collect` to explicitly enable the project-scoped poller.
The collector's kasaterm endpoint defaults to `http://127.0.0.1:8765`; override it
with `--base-url`. Structured summaries run in a separate 30-second worker.
External LLM calls are disabled unless `--llm` is explicitly passed. That mode
uses only already-injected process environment credentials; it never opens a
key file. `--nacho-repo` optionally selects Nacho's existing transport repository.
The UI explicitly shows when the Nacho summary provider is unavailable.
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

The list accepts `limit` (1–100), `before` (opaque request ID),
`reported_status`, and `project`. The project must match the service's configured
project; one running service does not expose other projects from its database.
The summary's `counts` comes from `Store.stats`; `needs_confirmation` is the
latest 50 requests with a completion report but no confirmed application. This
endpoint is intended for later native pet integration; it sends no pet messages.
It also returns `text` and `waiting_text` (each at most 250 characters), a `url`
for the detailed journal, and `summarizer` provider status. `/api/ask` chooses
between these cached-status answers; it does not issue LLM calls. On startup,
`service.json` in the data directory records `{version:1,base_url,project}` with
owner-only permissions (0600), allowing a native pet to discover this service.

Acknowledgement body:

```json
{"applied_status":"applied","evidence":"Observed the requested menu in the app"}
```

The UI's “아직이에요” uses `pending`. Acknowledgements never change student
reported status, and a final answer never automatically confirms application.
Request text is inserted through DOM `textContent`, never interpreted as HTML.
HTTP access logs are suppressed, and service errors do not contain transcript
text. Local processes and the current user's browser can read the journal;
this is not an authentication boundary against other local software.
