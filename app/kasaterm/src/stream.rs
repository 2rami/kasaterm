//! GUI-side view types retained from the (now removed) daemon stream protocol.
//! `DockedView` types `App.docked`, `PaneStatusView` types `App.pane_activity`;
//! both are read by the renderer. In local mode they stay empty until the dock
//! UX and the transcript-driven working indicator are re-wired locally
//! (follow-up). The daemon, its client, and the screen-frame socket are gone.

use serde::{Deserialize, Serialize};

/// A pane folded into the dock: its id + a display label (cwd basename).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DockedView {
    pub id: String,
    pub label: String,
}

/// One pane's coarse activity for the GUI's working indicator + completion
/// toast. `status != "idle"` means busy. Derives `PartialEq` so the repaint
/// gate can tell when a pane flips working↔idle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct PaneStatusView {
    /// "working" | "building" | "blocked" | "idle" | "waiting".
    pub status: String,
    /// Free-text "what + why" shown in the completion toast.
    pub intent: String,
    /// Why `status == "waiting"` (claude blocked on a prompt). None unless waiting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_for: Option<String>,
    /// A `run_in_background` shell or `Monitor` is in-flight for this pane even
    /// though no spinner shows on screen. Drives the header pulse bar; derived
    /// from the transcript tail, not the glyph scan.
    #[serde(default)]
    pub bg_active: bool,
}
