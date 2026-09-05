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
    /// 현재 오류 복구 대기 상태. 미니맵의 빨간 경고 삼각형만 이 값을 읽는다.
    #[serde(default)]
    pub has_error: bool,
    /// A `run_in_background` shell or `Monitor` is in-flight for this pane even
    /// though no spinner shows on screen. Drives the header pulse bar; derived
    /// from the transcript tail, not the glyph scan.
    #[serde(default)]
    pub bg_active: bool,
    /// 이 pane 이 **쉬지 않고 돈 지 얼마나 됐나**의 기준점. 배치도 칸이 「14분째」를
    /// 그리는 값이고, 도는 것이 멈추면 지운다. 오래 도는 일과 잠깐 도는 일이 화면에서
    /// 똑같이 보이던 것을 가른다(2026-08-24 지시). 직렬화 대상이 아니다 — 시각은
    /// 이 프로세스 안에서만 뜻이 있다.
    #[serde(skip)]
    pub busy_since: Option<std::time::Instant>,
    /// compact 진행률 — claude 가 화면에 찍는 `▰▰▱ N%` 를 파싱한 값(0..=100).
    /// claude 는 이 값을 화면에만 내놓고 API 로 주지 않으므로 글자에서 읽는 수밖에
    /// 없다. None = compact 중이 아니거나 퍼센트 행이 안 보임(그땐 바가 시간 루프로
    /// 돈다). status 와 같은 틱에 같은 화면에서 읽어 어긋나지 않는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact_pct: Option<u8>,
    /// 연결이 끊겨 멈췄다면 그 사연(「연결 끊김」·「재시도 중」…). None 이면 멀쩡하다.
    ///
    /// claude 는 인터넷이 끊겨도 조용히 서지 않고 화면에 문구를 남기는데, 그건
    /// 스크롤백의 글자일 뿐이라 옆에서 보면 도는 pane 과 구별이 안 된다. 헤더를
    /// 빨갛게 두르는 근거가 이 값이다(2026-08-26 지시).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stalled: Option<String>,
}

/// 사람을 기다린다는 표시 — 이유(훅의 message)와 종류(`attention_kind` 참고).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AttentionFlag {
    pub reason: String,
    pub kind: String,
}
