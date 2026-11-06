//! 웹 터미널이 받는 grid 프레임 — 셀 런 인코더 자체는 엔진(`kasa_screen::wire`)에 있어
//! 창 없는 호스트(`kasa-host`)도 같은 꼴을 낸다. 여기엔 본판의 장면 합성(scene)과 raw
//! visual 되돌리기만 남는다.

use kasa_screen::screen::ScreenUpdate;
use serde_json::{json, Value};

pub use kasa_screen::wire::{encode, encode_row, encode_row_with_glyphs, encode_with_glyphs};

pub fn encode_scene(frame: &crate::visual::PaneVisualFrame) -> Value {
    encode_scene_with_glyphs(frame, false)
}

pub fn encode_scene_with_glyphs(frame: &crate::visual::PaneVisualFrame, glyphs: bool) -> Value {
    let mut composed = frame.raw_snapshot.clone();
    composed.dirty = frame.composed_cells.iter().cloned().enumerate()
        .map(|(index, row)| (index as u16, row)).collect();
    let mut result = encode_with_glyphs(&composed, glyphs);
    result["sourceKey"] = json!(frame.source_key);
    result["sceneRevision"] = json!(frame.scene_revision);
    result["scene"] = json!({
        "paneId": frame.pane_id, "sourceKey": frame.source_key, "revision": frame.scene_revision,
        "cols": frame.cols, "rows": frame.rows, "offset": frame.offset, "overlays": frame.overlays,
    });
    result
}

/// The full raw fallback restores cells erased by an earlier native composition.
pub fn encode_raw_visual(source: &ScreenUpdate) -> Value {
    encode_raw_visual_with_glyphs(source, false)
}

pub fn encode_raw_visual_with_glyphs(source: &ScreenUpdate, glyphs: bool) -> Value {
    let mut result = encode_with_glyphs(source, glyphs);
    result["sourceKey"] = json!(crate::visual::source_key(source, 0));
    result["sceneRevision"] = Value::Null;
    result["scene"] = Value::Null;
    result
}
