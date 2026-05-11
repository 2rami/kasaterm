# tmuxify-rs

Native Rust GUI terminal for non-developers using Claude Code team mode.
Successor to the Tauri PoC at [2rami/tmuxify](https://github.com/2rami/tmuxify).

## Layout

- `crates/tmux-bridge` — tmux `-C` (control mode) subprocess + vt100 buffer (port from Tauri PoC)
- `spikes/{iced,egui,gpui,warpui}-term` — minimal terminal-pane spikes per GUI framework, picked sequentially

See `~/내 드라이브/MEMORY/experiments/project_tmuxify_vision.md` for product vision.
