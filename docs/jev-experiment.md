# Jev advisory experiment

`kasa-jev` reads existing observations and asks OpenGateway's pinned
`typesafe/jev-1.13` model to select a declared option. It does not execute a
selected command, send messages, click, type, mark work done, or change the board.
The native application does not need rebuilding or restarting.

## Run

Node.js 20 or newer, `kasaterm-cli`, and the existing OpenGateway key are required.
`OPENGATEWAY_API_KEY` overrides `~/.config/opengateway.key`; never put a key in argv.

```sh
node scripts/jev/cli.mjs smoke
node scripts/jev/cli.mjs board --all
node scripts/jev/cli.mjs board --local --dry-run
node scripts/jev/cli.mjs board --api http://127.0.0.1:8765
```

An optional `~/.local/bin/kasa-jev` symlink can point to `scripts/jev/cli.mjs`.
Install it only when that name is unused; the script resolves module imports
relative to its real location. This installation is per machine.

The board adapter sends clipped title/request/progress summaries and typed status
fields, not transcripts, connection configuration or pane addresses. Known token
patterns are scrubbed, but arbitrary secrets in summaries may remain. It only
admits identified agent panes with fresh observations from complete online
sources. Excluded or stale observations remain unconfirmed. Full addresses and
the source cursor are retained locally in the advisory output; use a new board
snapshot before any later activity inspection or control action.
Completion reports lack a turn timestamp in this snapshot. The adapter omits
them on working panes and treats their age elsewhere as unverified.

## Browser and arbitrary CLI decisions

```sh
kc list_profiles
kc list_tabs
kasa-jev browser --tab-id 123 --goal "Find the documentation link" --dry-run
kasa-jev browser --tab-id 123 --goal "Find the documentation link"
kasa-jev choose --file request.json
```

Use a freshly listed tab you are authorized to inspect. The browser adapter only
calls `list_profiles` and `read_page`, preserves Kasachrome's device routing, and
fails if no extension profile is connected. It supplies observed button/link/tab
refs plus read/wait/manual/verify choices. Hidden tabs cannot yield click targets.
The result is a suggestion, not a browser executor. Inputs, purchases, login,
messages, deletion and approvals remain manual. Textbox/searchbox/combobox lines
are omitted because an accessible name can contain the filled value too.

`choose` also accepts JSON on stdin with this shape:

```json
{
  "state": "The build reports two failing tests.",
  "instructions": "Choose the next read-only step.",
  "choices": {
    "inspect": "Read the failing test details.",
    "wait": "No further inspection is needed."
  }
}
```

Observation text is untrusted data. `--dry-run` shows the exact model request
without loading a key or making an inference call. Review it before sending
sensitive task or browser content. Redaction is best effort, not a data-loss
prevention boundary. Generic `choose` content is supplied by its caller.
Inference refuses input containing the actual API key rather than modifying it.

## Validation and measurements

```sh
node --test scripts/jev/jev.test.mjs
```

Requests go to `POST https://apis.opengateway.ai/v1/decisions`, with typed Choice
questions; Chat Completions is not used. Responses must contain a supplied choice,
probabilities for exactly those choices, a normalized distribution, and bounded
confidence. Invalid responses, HTTP errors, redirects and timeouts fail closed.
There are no automatic retries or fallback models.

Both selected-option probability and confidence are exposed. The initial
`0.7` probability / `0.8` confidence thresholds only flag `needs_review`; they
are experimental heuristics, not measured accuracy guarantees or permissions.
`selected_candidate` identifies the model's pick for manual review even when
`suggested` is withheld for low confidence.
Jev does not supply a prose explanation. Board state and completion reports remain
the source of truth, and a model's `done` choice still requires verification.

`decision.latency_ms` includes the gateway network round trip and response
validation. `timing.total_ms` includes local observation too. These measurements
do not establish improvement over a previous model or the native board refresh.
No continuous watcher, native board badge, or automatic browser loop is installed.

Initial checks on 2026-09-27: 72 local tests passed. A synthetic smoke decision
took 592 ms (confidence 0.98); a real board with 10 eligible panes took 403 ms
(confidence 0.33, `needs_review`), and a three-pane local board took 906 ms
(confidence 0.76, `needs_review`). These are individual samples, not a benchmark.
The local board test exposed a retained previous-turn completion report; working
panes now omit it. Browser inference has only fixture coverage because the local
bridge reported zero connected extension profiles; a real-page test is pending.

API semantics: [Jev documentation](https://openrouter.ai/docs/guides/community/jev),
[TypeSafe Choice](https://docs.typesafe.ai/primitives/choice).
