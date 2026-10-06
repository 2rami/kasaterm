//! vt100 셀을 공용 낱말(`kasa_screen`)로 옮긴다 — 옛 tmux 브리지 경로만 쓴다.

use crate::screen::{Cell, Color};

pub(crate) fn vt_color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Default,
        vt100::Color::Idx(i) => Color::Idx(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

pub(crate) fn vt_cell(c: &vt100::Cell) -> Cell {
    let contents = c.contents();
    // vt100 returns the cell's grapheme cluster; we keep only the base char
    // (the renderer already shapes a single char per cell). Empty → blank.
    let ch = contents.chars().next().unwrap_or(' ');
    Cell {
        ch,
        fg: vt_color(c.fgcolor()),
        bg: vt_color(c.bgcolor()),
        bold: c.bold(),
        italic: c.italic(),
        underline: c.underline(),
        inverse: c.inverse(),
        dim: false,
        // vt100 crate 는 conceal 미노출 — 이 경로(레거시 브리지)는 마커 채널 없음.
        hidden: false,
        // vt100 crate 는 줄넘김 표식도 안 준다 — 링크 감지는 채움 휴리스틱으로 잇는다.
        wrapped: false,
        // Legacy vt100 cells do not expose leading-wide padding metadata.
        leading_wide_spacer: false,
    }
}
