# Mobile Codex fixtures

`codex_mobile.dart` is synthetic, sanitized test content. It is not a live Codex session capture and
does not send commands to a user terminal. Screen captures carry the same synthetic-data label.

The contracts are grounded in repository examples:

- `crates/kasa-mcp/src/http.rs`, `term_panes_handler`: `harness` indicates the live agent;
  `name` comes from an optional character assignment and may be null. Status, model, effort,
  attention kind, and idle seconds are separate fields.
- `app/kasaterm/src/input.rs`, `codex_live_choices_are_distinct_from_history_above_the_composer`:
  Codex's `› 1. Yes, proceed (y)` / `2. No, and tell Codex what to do differently (esc)` menu,
  and the ordinary composer below historical choices that must invalidate them.
- `mobile/test/claude_style_test.dart`: the raw `gpt-5.6-sol xhigh · … · Context 16% used`
  footer and its provider-icon rendering. The model label is fixture data, not a claim about the latest model.
- `mobile/test/conversation_test.dart`: Codex `event_msg` and `response_item` rollout envelopes,
  tool call IDs, arguments, output, and completion messages.

`codex_mobile_test.dart` covers 320/390/430px light/dark terminal states (working, completed,
approval), native chat approval buttons, stale-choice rejection before repaint, tool output expansion,
Korean long-response wrapping, provider identity, and fixed-width Korean cells.
Progress snapshots freeze animation through Flutter's `TickerMode`; they do not measure animation performance.
Human OS keyboard composition and actual account/server integration remain separate checks.
