//! 칸 위 덧그림 — 커서·자동완성 유령 글자·조합 중 글자·선택 띠. 호스트가 칸 상태에서 값을 떠
//! [`PaneOverlay`] 로 넘기면, 칸 셀을 그린 뒤 그 위에 얹는다.

use crate::cursor::{cursor_primitives, CursorShape};
use crate::palette::SELECTION;
use crate::renderer::GridRenderer;

/// (col, row) anchor + end for drag selection. Both ends in cell units.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: (u16, u16),
    pub end: (u16, u16),
}

/// 덧그림이 읽는 값 전부. 렌더러를 빌리기 전에 떠 두어 호스트 상태와 렌더러 빌림이 부딪치지 않게 한다.
pub struct PaneOverlay {
    pub cell_w: f32,
    pub cell_h: f32,
    pub pad_x: f32,
    pub pad_y: f32,
    pub cursor_row: u16,
    pub cursor_col: u16,
    /// 커서가 덮는 칸 수. 한글·CJK 는 두 칸을 차지하는데 한 칸만 칠하면 커서가
    /// 글자의 왼쪽 절반에만 걸려, 그 자리에 뭐가 있는지가 아니라 커서가 깨진 것처럼
    /// 보인다. 나머지는 전부 1 이다.
    pub cursor_w: u16,
    pub cursor_shape: CursorShape,
    pub cursor_thickness: f32,
    /// 지금 보이는 탭의 캐릭터색. 미배정 셸만 테마 커서색으로 떨어진다.
    pub cursor_color: [u8; 4],
    pub cursor_visible: bool,
    pub cols: u16,
    pub blink_on: bool,
    pub preedit: String,
    /// Where the preedit box anchors. Resolved via find_prompt_anchor so
    /// a TUI (Claude Code) that parks its cursor on a statusline still
    /// gets the composing Hangul drawn on the prompt row. Mirrors the
    /// sugarloaf render_frame path.
    pub preedit_row: u16,
    pub preedit_col: u16,
    /// Active pane's font multiplier (pane-local zoom). The cursor /
    /// preedit / selection / ghost overlays must scale their cell size
    /// by this — `cell_w`/`cell_h` are the base (1.0) metrics, so a
    /// zoomed pane would otherwise anchor them on the un-zoomed grid and
    /// drift the composing Hangul off the prompt cell.
    pub font_scale: f32,
    pub selection: Option<Selection>,
    /// Inline autosuggestion ghost text (empty = none). Drawn dim,
    /// starting at the cursor cell, clipped to the row's right edge.
    pub suggestion: String,
}

/// 칸 셀 위에 덧그림을 얹는다.
pub fn paint(g: &mut GridRenderer, ov: &PaneOverlay) {
    // Effective cell size for THIS pane: base metric × pane zoom. The
    // anchor (pad_x/pad_y) stays on the base grid because the pane's
    // top-left lives there, but every per-column/row step must use the
    // zoomed size or the cursor/preedit/selection drift right & down
    // as the pane is shrunk.
    let cw = ov.cell_w * ov.font_scale;
    let ch = ov.cell_h * ov.font_scale;
    if ov.cursor_visible && ov.blink_on && ov.preedit.is_empty() {
        let cx = ov.pad_x + ov.cursor_col as f32 * cw;
        let cy = ov.pad_y + ov.cursor_row as f32 * ch;
        let mut c = ov.cursor_color;
        c[3] = 140; // ~0.55 alpha
        for quad in cursor_primitives(
            ov.cursor_shape,
            cx,
            cy,
            cw,
            ch,
            ov.cursor_w,
            ov.cursor_thickness,
        )
        .as_slice()
        {
            g.rect(quad.x, quad.y, quad.width, quad.height, c);
        }
    }
    // Inline autosuggestion ghost text — dim, on the same baseline as
    // committed cells, starting at the cursor and clipped to the row's
    // right edge so it never wraps. Drawn only when not composing.
    if ov.preedit.is_empty() && !ov.suggestion.is_empty() {
        let gx = ov.pad_x + ov.cursor_col as f32 * cw;
        let gy = ov.pad_y + ov.cursor_row as f32 * ch;
        let max_cells = ov.cols.saturating_sub(ov.cursor_col) as u32;
        if max_cells > 0 {
            g.draw_ghost(gx, gy, &ov.suggestion, max_cells, ov.font_scale);
        }
    }
    if !ov.preedit.is_empty() {
        let px = ov.pad_x + ov.preedit_col as f32 * cw;
        let py = ov.pad_y + ov.preedit_row as f32 * ch;
        // Route preedit through the cell-grid path so the composing
        // syllable sits on the same baseline as committed text
        // instead of floating above the row.
        g.draw_preedit(px, py, &ov.preedit, ov.cursor_color, ov.font_scale);
    }
    if let Some(sel) = ov.selection {
        let (start, stop) = if (sel.anchor.1, sel.anchor.0) <= (sel.end.1, sel.end.0) {
            (sel.anchor, sel.end)
        } else {
            (sel.end, sel.anchor)
        };
        let color = SELECTION;
        if start.1 == stop.1 {
            let x = ov.pad_x + start.0 as f32 * cw;
            let y = ov.pad_y + start.1 as f32 * ch;
            let w = (stop.0 - start.0 + 1) as f32 * cw;
            g.rect(x, y, w, ch, color);
        } else {
            let x = ov.pad_x + start.0 as f32 * cw;
            let y = ov.pad_y + start.1 as f32 * ch;
            let row_w = (ov.cols - start.0) as f32 * cw;
            g.rect(x, y, row_w, ch, color);
            for r in (start.1 + 1)..stop.1 {
                let yy = ov.pad_y + r as f32 * ch;
                g.rect(ov.pad_x, yy, ov.cols as f32 * cw, ch, color);
            }
            let yy = ov.pad_y + stop.1 as f32 * ch;
            let last_w = (stop.0 + 1) as f32 * cw;
            g.rect(ov.pad_x, yy, last_w, ch, color);
        }
    }
}
