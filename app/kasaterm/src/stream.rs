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
    /// compact 진행률 — claude 가 화면에 찍는 `▰▰▱ N%` 를 파싱한 값(0..=100).
    /// claude 는 이 값을 화면에만 내놓고 API 로 주지 않으므로 글자에서 읽는 수밖에
    /// 없다. None = compact 중이 아니거나 퍼센트 행이 안 보임(그땐 바가 시간 루프로
    /// 돈다). status 와 같은 틱에 같은 화면에서 읽어 어긋나지 않는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact_pct: Option<u8>,
}
