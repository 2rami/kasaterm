# Native mirror viewer boundaries

A mirror owns its layout and reading position, not the source workspace.

- Split/tab on a mirror creates a local shell. Resolve the viewer's mapped local
  directory; never spawn on the source machine or its currently selected room.
- A newly created source pane follows into an existing matching mirror room.
  The initial snapshot is a baseline, not a request to open historical panes.
  Locally dismissed mirrors stay dismissed; mirrors are never mirrored again.
- Explicit source closure sends `source-closed` immediately, including when the
  source PTY is retained for undo. A fresh `closed: true` snapshot is a fallback.
  Missing rows, offline machines and restart `gone` frames are not closure.
- Saved and discovered routes merge only after confirming the same machine ID.
  Keep the saved label, directory mappings and SSH configuration, retaining the
  alternate transport for recovery. Device tints remain visible in active maps.
- Codex numbered patch fills use the viewer palette; additions/deletions retain
  their meaning. Cursor-painted continuations join after the +/- gutter, not
  after the code's indentation. Preserve source cells and coordinate mappings.
- Prompt navigation changes only the viewer's local history/projection. A raw
  source anchor is aligned to the reflowed viewport, and manual scrolling/input
  clears it. No synthetic source mouse wheel is used for mirror prompt clicks.
- Initial raw history retains up to 10,000 rows / one million cells, rather
  than the obsolete 1,000-row xterm default. History outside this bounded window
  is not silently reconstructed or claimed available.

Verification uses isolated source/viewer apps and scoped sockets. Live user
sources may only be attached with `KASATERM_AUTORESTORE_OBSERVE_ONLY=1`, which
disables restore-probe keystrokes. Capture the composed window, not raw pane
cells, when checking palette or wrapping. Never quit production apps for tests.
