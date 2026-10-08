//! 셸 카드 칸의 배치와 그리기. 치수는 `docs/design.md` 「거울 셸 명령 묶음」에 있다.
//!
//! 결과 줄은 터미널과 같은 글꼴·칸 폭으로 이 칸 폭에 맞춰 다시 접는다. 접기는 블록이
//! 바뀌거나 폭·펼침이 바뀔 때만 한다(`Layout`) — 긴 출력을 프레임마다 접으면 그 값이
//! 프레임을 먹는다. 색은 그릴 때 테마로 푼다(테마를 바꿔도 다시 접을 일이 없게).

use kasa_bridge::screen::Color;

use super::{Block, Feed, ShellPane};
use crate::{cells, gpu, native_controls, theme, GridCell};

type Rect = (f32, f32, f32, f32);

/// 한 프레임에 카드로 그릴 pane 하나 — 그리는 쪽이 `self` 를 못 빌려 렌더 루프가 미리 뜬다.
pub(crate) struct Slot {
    pub(crate) pane: String,
    pub(crate) rect: Rect,
    pub(crate) focused: bool,
    /// 원본 격자의 커서 줄(감긴 줄 이어 붙임)과 그 안의 커서 칸.
    pub(crate) prompt: Option<(Vec<GridCell>, usize)>,
    pub(crate) caret_on: bool,
    pub(crate) now_ms: u64,
    /// 이 기기 셸 칸(명령으로 보기) — 거울이 아니라 「원본」이라 부를 것이 없다.
    pub(crate) local: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Hit {
    /// 접힌 결과 펼치기·접기. 값은 블록 id.
    Fold(u64),
    Copy(u64),
    Bottom,
    /// 결과 속 폴더 고리 — 그 셸을 그 폴더로 옮긴다. 값은 절대 경로.
    Dir(String),
}

const PAD_X: f32 = 10.0;
const PAD_TOP: f32 = 8.0;
const CARD_GAP: f32 = 6.0;
const CARD_PAD: f32 = 10.0;
const HEAD_H: f32 = 26.0;
const OUT_PAD: f32 = 6.0;
const FOLD_H: f32 = 22.0;
const NOTE_H: f32 = 18.0;
const DOT: f32 = 8.0;
/// 끝난 결과가 이 행을 넘으면 앞 `HEAD_ROWS`·뒤 `TAIL_ROWS` 만 두고 접는다.
const FOLD_OVER: usize = 24;
const HEAD_ROWS: usize = 10;
const TAIL_ROWS: usize = 6;
/// 도는 명령은 끝이 중요하다 — 뒤 이만큼만.
const LIVE_ROWS: usize = 16;
const PROMPT_ROWS: usize = 4;

/// (글자색, 바탕색, 꾸밈, 고리 번호) — 고리 번호는 `Layout::links` 의 1 기반 자리, 0 이면 고리 아님.
type Style = (Color, Color, u8, u16);

pub(super) struct Card {
    id: u64,
    rows: Vec<Vec<(char, u16)>>,
    /// 이 행 앞에 접기 줄을 끼운다.
    fold_at: Option<usize>,
    fold_label: String,
    note: Option<String>,
    height: f32,
}

#[derive(Default)]
pub(super) struct Layout {
    key: Option<(u64, u32, u64)>,
    styles: Vec<Style>,
    links: Vec<String>,
    cards: Vec<Card>,
}

fn style_id(styles: &mut Vec<Style>, s: Style) -> u16 {
    match styles.iter().position(|x| *x == s) {
        Some(i) => i as u16,
        None => {
            styles.push(s);
            (styles.len() - 1) as u16
        }
    }
}

/// 글자 묶음을 `cols` 칸으로 접는다. 한글 같은 넓은 글자는 두 칸.
fn wrap(chars: impl IntoIterator<Item = (char, u16)>, cols: usize, out: &mut Vec<Vec<(char, u16)>>) {
    let mut row: Vec<(char, u16)> = Vec::new();
    let mut used = 0;
    for (ch, st) in chars {
        let w = if gpu::is_wide_char(ch) { 2 } else { 1 };
        if used + w > cols && !row.is_empty() {
            out.push(std::mem::take(&mut row));
            used = 0;
        }
        row.push((ch, st));
        used += w;
    }
    out.push(row);
}

fn block_rows(
    b: &Block,
    cols: usize,
    styles: &mut Vec<Style>,
    links: &mut Vec<String>,
) -> (Vec<Vec<(char, u16)>>, Option<usize>) {
    let mut rows = Vec::new();
    let mut gap_row = None;
    for (i, line) in b.lines.iter().enumerate() {
        if b.gap > 0 && b.gap_at == Some(i) {
            gap_row = Some(rows.len());
        }
        let mut here: Vec<(usize, usize, u16)> = Vec::new();
        for (_, start, len, path) in b.links.iter().filter(|l| l.0 == i) {
            links.push(path.clone());
            here.push((*start, start + len, links.len() as u16));
        }
        let link_at = |k: usize| here.iter().find(|(a, z, _)| (*a..*z).contains(&k)).map_or(0, |l| l.2);
        let mut k = 0;
        let mut chars: Vec<(char, u16)> = Vec::new();
        for s in line {
            for c in s.text.chars() {
                let id = style_id(styles, (s.fg.clone(), s.bg.clone(), s.flags, link_at(k)));
                chars.push((c, id));
                k += 1;
            }
        }
        wrap(chars, cols, &mut rows);
    }
    (rows, gap_row)
}

fn ago_label(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        _ if ms < 1_000 => format!("{:.2}초", ms as f64 / 1000.0),
        _ if ms < 10_000 => format!("{:.1}초", ms as f64 / 1000.0),
        0..=59 => format!("{s}초"),
        60..=3599 => format!("{}분 {}초", s / 60, s % 60),
        _ => format!("{}시간 {}분", s / 3600, s % 3600 / 60),
    }
}

fn layout(pane: &mut ShellPane, feed: &Feed, cols: usize) {
    let key = (feed.version, cols as u32, pane.open_gen);
    if pane.layout.key == Some(key) {
        return;
    }
    let mut styles = std::mem::take(&mut pane.layout.styles);
    styles.clear();
    let mut links = Vec::new();
    let ch = 1.0;
    let cards = feed
        .blocks
        .iter()
        .map(|b| {
            let (all, gap_row) = block_rows(b, cols, &mut styles, &mut links);
            let all: Vec<_> = if all.len() == 1 && all[0].is_empty() { Vec::new() } else { all };
            let open = pane.open.contains(&b.id);
            let hidden_extra = b.gap;
            let (rows, fold_at, fold_label) = if b.running && !open && all.len() > LIVE_ROWS {
                let cut = all.len() - LIVE_ROWS;
                (all[cut..].to_vec(), Some(0), format!("앞 {}줄", cut + hidden_extra))
            } else if !b.running && !open && all.len() > FOLD_OVER {
                let tail = all.len() - TAIL_ROWS;
                let mut rows = all[..HEAD_ROWS].to_vec();
                rows.extend_from_slice(&all[tail..]);
                (rows, Some(HEAD_ROWS), format!("가운데 {}줄 더 보기", tail - HEAD_ROWS + hidden_extra))
            } else if open && (all.len() > FOLD_OVER || b.gap > 0) {
                let at = if b.gap > 0 { gap_row.unwrap_or(all.len()) } else { all.len() };
                let label = if b.gap > 0 { format!("{}줄 받는 중…", b.gap) } else { "접기".to_string() };
                (all, Some(at), label)
            } else {
                (all, None, String::new())
            };
            let note = if b.tui {
                Some("전체 화면 프로그램이었어요 — 그 화면은 원본 칸에만 있어요".to_string())
            } else if b.dropped > 0 {
                Some(format!("너무 길어 앞 {}줄은 원본도 버렸어요", b.dropped))
            } else {
                None
            };
            let body = !rows.is_empty() || fold_at.is_some() || note.is_some();
            let height = HEAD_H
                + if body {
                    OUT_PAD * 2.0
                        + rows.len() as f32 * ch
                        + fold_at.map_or(0.0, |_| FOLD_H)
                        + note.as_ref().map_or(0.0, |_| NOTE_H)
                } else {
                    0.0
                };
            Card { id: b.id, rows, fold_at, fold_label, note, height }
        })
        .collect();
    pane.layout = Layout { key: Some(key), styles, links, cards };
}

/// 높이 계산은 행 수로 하고 픽셀은 그릴 때 곱한다 — 접기 결과는 글꼴 크기에 매이지 않는다.
fn card_px(card: &Card, line_h: f32) -> f32 {
    card.height + card.rows.len() as f32 * (line_h - 1.0)
}

fn cell_of(styles: &[Style], id: u16, ch: char) -> GridCell {
    let (fg, bg, flags, link) = styles.get(id as usize).cloned().unwrap_or((Color::Default, Color::Default, 0, 0));
    let mut c = GridCell::blank();
    c.ch = ch;
    c.fg = fg;
    c.bg = bg;
    c.bold = flags & kasa_pty::block_lines::SPAN_BOLD != 0;
    c.dim = flags & kasa_pty::block_lines::SPAN_DIM != 0;
    c.italic = flags & kasa_pty::block_lines::SPAN_ITALIC != 0;
    c.underline = flags & kasa_pty::block_lines::SPAN_UNDERLINE != 0 || link != 0;
    c.inverse = flags & kasa_pty::block_lines::SPAN_INVERSE != 0;
    if link != 0 {
        let [r, g, b, _] = theme::accent();
        c.fg = Color::Rgb(r, g, b);
        c.dim = false;
    }
    c
}

/// 한 행의 폴더 고리 자리 — (고리 번호, 첫 칸, 칸 수). 넓은 글자는 두 칸.
fn link_runs(styles: &[Style], row: &[(char, u16)]) -> Vec<(u16, usize, usize)> {
    let mut runs: Vec<(u16, usize, usize)> = Vec::new();
    let mut col = 0;
    for &(ch, st) in row {
        let w = if gpu::is_wide_char(ch) { 2 } else { 1 };
        let link = styles.get(st as usize).map_or(0, |s| s.3);
        if link != 0 {
            match runs.last_mut() {
                Some(run) if run.0 == link && run.1 + run.2 == col => run.2 += w,
                _ => runs.push((link, col, w)),
            }
        }
        col += w;
    }
    runs
}

/// 한 행: 바탕 칠 → 글자 → 밑줄. 칸 폭은 터미널과 같다.
fn draw_row(g: &mut gpu::GpuRenderer, cells: &[GridCell], x: f32, y: f32, line_h: f32, clip: (f32, f32)) {
    let cw = g.cell_w;
    let size = g.term_font_size();
    let pal = cells::palette();
    let (dfg, dbg) = (pal.fg, pal.bg);
    let mut col = 0.0;
    let mut glyphs: Vec<(char, [u8; 4])> = Vec::with_capacity(cells.len());
    for c in cells {
        let w = if gpu::is_wide_char(c.ch) { 2.0 } else { 1.0 };
        let bg = pal.cell_bg_with(c, dfg);
        if bg != dbg {
            g.rect(x + col * cw, y, w * cw, line_h, bg);
        }
        let fg = pal.cell_fg_with(c, dfg);
        if c.underline {
            g.rect(x + col * cw, y + line_h - 2.0, w * cw, 1.0, fg);
        }
        glyphs.push((if c.ch == '\0' { ' ' } else { c.ch }, fg));
        col += w;
    }
    g.draw_editor_cells(&glyphs, x, y + (line_h - size) * 0.5, size, clip.0, clip.1);
}

fn label(g: &mut gpu::GpuRenderer, x: f32, y: f32, text: &str, size: f32, color: [u8; 4]) {
    g.draw_text(x, y, text, gpu::DrawOpts { font_size: size, color, bold: false, italic: false });
}

fn inside(c: (f32, f32), r: Rect) -> bool {
    c.0 >= r.0 && c.0 < r.0 + r.2 && c.1 >= r.1 && c.1 < r.1 + r.3
}

fn status(b: &Block, now_ms: u64) -> ([u8; 4], String) {
    if b.running {
        let ran = now_ms.saturating_sub(b.start_ms);
        return (theme::accent(), format!("도는 중 · {}", ago_label(ran)));
    }
    // 눈 깜짝할 새 끝난 명령은 시간을 안 적는다 — 「0.00초」는 읽을 게 없다.
    let took = b.ms.filter(|ms| *ms >= 10).map(ago_label);
    let with = |head: String| match &took {
        Some(t) if head.is_empty() => t.clone(),
        Some(t) => format!("{head} · {t}"),
        None => head,
    };
    match b.exit {
        Some(0) => (theme::success(), with(String::new())),
        Some(code) => (theme::danger(), with(format!("종료 {code}"))),
        None => (theme::text_mute(), with("멈춤".into())),
    }
}

pub(super) fn paint(g: &mut gpu::GpuRenderer, cursor: (f32, f32), slot: &Slot, pane: &mut ShellPane, feed: &Feed) -> Vec<(Hit, Rect)> {
    let mut hits = Vec::new();
    let (x, y, w, h) = slot.rect;
    if w < 80.0 || h < 60.0 {
        return hits;
    }
    let line_h = g.cell_h;
    let cw = g.cell_w.max(1.0);
    let card_x = x + PAD_X;
    let card_w = (w - PAD_X * 2.0).max(40.0);
    let text_x = card_x + CARD_PAD;
    let text_w = (card_w - CARD_PAD * 2.0).max(cw);
    let cols = ((text_w / cw).floor() as usize).max(8);
    g.push_clip(x, y, w, h);

    // ── 입력 줄(아래 고정) — 셸이 프롬프트에서 기다릴 때 원본 커서 줄 ───────────
    let running = feed.blocks.iter().any(|b| b.running);
    let mut prompt_rows: Vec<Vec<GridCell>> = Vec::new();
    let mut caret: Option<(usize, usize)> = None;
    if let Some((cells, at)) = slot.prompt.as_ref().filter(|_| !running && feed.loaded) {
        let mut rows: Vec<Vec<(char, u16)>> = Vec::new();
        wrap(cells.iter().enumerate().map(|(i, c)| (c.ch, i as u16)), cols, &mut rows);
        let mut n = 0;
        for (r, row) in rows.iter().enumerate() {
            let mut used = 0;
            for &(ch, i) in row {
                if i as usize == *at {
                    caret = Some((r, used));
                }
                used += if gpu::is_wide_char(ch) { 2 } else { 1 };
                n = i as usize + 1;
            }
            if caret.is_none() && *at >= n && r + 1 == rows.len() {
                caret = Some((r, used + (*at - n)));
            }
        }
        prompt_rows = rows
            .iter()
            .map(|row| row.iter().map(|&(_, i)| cells[i as usize].clone()).collect())
            .collect();
        if let Some((r, c)) = caret {
            if c >= cols {
                prompt_rows.push(Vec::new());
                caret = Some((r + 1, 0));
            }
        }
        let keep = prompt_rows.len().saturating_sub(PROMPT_ROWS);
        prompt_rows.drain(..keep);
        caret = caret.and_then(|(r, c)| r.checked_sub(keep).map(|r| (r, c)));
    }
    let input_h = if prompt_rows.is_empty() { 0.0 } else { 1.0 + 6.0 * 2.0 + prompt_rows.len() as f32 * line_h };
    let input_y = y + h - input_h;
    if input_h > 0.0 {
        g.rect(x, input_y, w, 1.0, theme::with_alpha(theme::border(), 140));
        let ty = input_y + 1.0 + 6.0;
        for (r, row) in prompt_rows.iter().enumerate() {
            draw_row(g, row, text_x, ty + r as f32 * line_h, line_h, (x, x + w));
        }
        if let Some((r, c)) = caret.filter(|_| slot.focused && slot.caret_on) {
            g.rect(text_x + c as f32 * cw, ty + r as f32 * line_h, 2.0, line_h, theme::accent());
        }
    }

    // ── 카드 목록(아래부터 쌓기) ──────────────────────────────────────────
    let list_top = y + PAD_TOP;
    let list_bottom = input_y - if input_h > 0.0 { 6.0 } else { PAD_TOP };
    let list_h = (list_bottom - list_top).max(0.0);
    let center = |g: &mut gpu::GpuRenderer, title: &str, sub: &str| {
        let tw = g.measure_chrome_text(title, 12.0, false);
        label(g, x + (w - tw) / 2.0, list_top + list_h / 2.0 - 20.0, title, 12.0, theme::text_dim());
        let sw = g.measure_chrome_text(sub, 10.5, false);
        label(g, x + (w - sw) / 2.0, list_top + list_h / 2.0 + 2.0, sub, 10.5, theme::text_mute());
    };
    if !feed.loaded {
        let (title, sub) = if slot.local {
            ("명령을 받는 중…", "이 칸 셸의 OSC 133 구간을 읽어요")
        } else {
            ("원본 칸의 명령을 받는 중…", "원본 기기에서 OSC 133 구간을 읽어요")
        };
        center(g, title, feed.error.as_deref().unwrap_or(sub));
        g.pop_clip();
        return hits;
    }
    layout(pane, feed, cols);
    if pane.layout.cards.is_empty() {
        let sub = if slot.local {
            "아래 줄에서 치면 명령마다 카드로 쌓여요"
        } else {
            "여기서 치면 원본 칸에서 돌아요 · 원본 칸 크기는 그대로예요"
        };
        center(g, "아직 친 명령이 없어요", sub);
    }
    let total: f32 = pane.layout.cards.iter().map(|c| card_px(c, line_h)).sum::<f32>()
        + CARD_GAP * pane.layout.cards.len().saturating_sub(1) as f32;
    pane.scroll_max = (total - list_h).max(0.0);
    pane.scroll = pane.scroll.min(pane.scroll_max);
    let mut bottom = list_bottom + pane.scroll;
    g.push_clip(x, list_top - PAD_TOP, w, list_h + PAD_TOP);
    let styles = std::mem::take(&mut pane.layout.styles);
    for card in pane.layout.cards.iter().rev() {
        let ch_px = card_px(card, line_h);
        let top = bottom - ch_px;
        bottom = top - CARD_GAP;
        if top > list_bottom || top + ch_px < list_top - PAD_TOP {
            continue;
        }
        let Some(b) = feed.blocks.iter().find(|b| b.id == card.id) else { continue };
        let edge = if b.running { theme::accent() } else { theme::with_alpha(theme::border(), 140) };
        g.round_rect_stroke(card_x, top, card_w, ch_px, theme::radius_md(), theme::border_w().max(1.0), edge);
        // 머리: 상태 점 · 명령 · 걸린 시간(올리면 복사)
        let (dot, meta) = status(b, slot.now_ms);
        crate::circle_rect(g, card_x + CARD_PAD, top + (HEAD_H - DOT) / 2.0, DOT, dot);
        let head = (card_x, top, card_w, HEAD_H);
        let meta_w = g.measure_chrome_text(&meta, 10.5, false);
        let right = card_x + card_w - CARD_PAD;
        let copy_r = (right - 22.0, top + 2.0, 22.0, 22.0);
        let meta_x = if inside(cursor, head) { copy_r.0 - 6.0 - meta_w } else { right - meta_w };
        let cmd_x = card_x + CARD_PAD + DOT + 8.0;
        let cmd_w = (meta_x - 10.0 - cmd_x).max(0.0);
        let cmd = if b.cmd.is_empty() { "(명령 줄을 못 읽었어요)" } else { b.cmd.as_str() };
        let shown = crate::info::fit_text(g, cmd, cmd_w, 12.0, false);
        g.draw_code_text(cmd_x, top + (HEAD_H - 12.0) / 2.0, &shown, 12.0, if b.cmd.is_empty() { theme::text_mute() } else { theme::text() });
        label(g, meta_x, top + (HEAD_H - 10.5) / 2.0, &meta, 10.5, theme::text_mute());
        if inside(cursor, head) {
            native_controls::icon_button(g, copy_r, cursor, "copy", native_controls::Style::default());
            hits.push((Hit::Copy(b.id), copy_r));
        }
        // 결과
        let mut ry = top + HEAD_H + OUT_PAD;
        if let Some(note) = &card.note {
            label(g, text_x, ry + 2.0, note, 10.5, theme::text_mute());
            ry += NOTE_H;
        }
        for (i, row) in card.rows.iter().enumerate() {
            if card.fold_at == Some(i) {
                ry = fold_row(g, cursor, &mut hits, b.id, &card.fold_label, (text_x, ry, text_w));
            }
            if ry + line_h >= list_top - PAD_TOP && ry <= list_bottom {
                for (link, at, n) in link_runs(&styles, row) {
                    let r = (text_x + at as f32 * cw, ry, n as f32 * cw, line_h);
                    let Some(path) = pane.layout.links.get(link as usize - 1) else { continue };
                    if inside(cursor, r) && r.1 >= list_top - PAD_TOP && r.1 + r.3 <= list_bottom + PAD_TOP {
                        g.round_rect_fill(r.0 - 2.0, r.1, r.2 + 4.0, r.3, theme::radius_sm(), theme::surface_hover());
                        g.hover_pointer = true;
                    }
                    hits.push((Hit::Dir(path.clone()), r));
                }
                let cells: Vec<GridCell> = row.iter().map(|&(ch, st)| cell_of(&styles, st, ch)).collect();
                draw_row(g, &cells, text_x, ry, line_h, (card_x, card_x + card_w));
            }
            ry += line_h;
        }
        if card.fold_at.is_some_and(|at| at >= card.rows.len()) {
            fold_row(g, cursor, &mut hits, b.id, &card.fold_label, (text_x, ry, text_w));
        }
    }
    pane.layout.styles = styles;
    g.pop_clip();
    if pane.scroll > 240.0 {
        let r = (x + w - PAD_X - 26.0, list_bottom - 26.0 - 4.0, 26.0, 26.0);
        native_controls::icon_button(g, r, cursor, "chevron-down", native_controls::Style::default());
        hits.push((Hit::Bottom, r));
    }
    g.pop_clip();
    hits
}

fn fold_row(g: &mut gpu::GpuRenderer, cursor: (f32, f32), hits: &mut Vec<(Hit, Rect)>, id: u64, text: &str, at: (f32, f32, f32)) -> f32 {
    let (x, y, w) = at;
    let r = (x, y, w, FOLD_H);
    let hover = inside(cursor, r);
    if hover {
        g.round_rect_fill(r.0 - 4.0, r.1, r.2 + 8.0, r.3, theme::radius_sm(), theme::surface_hover());
    }
    g.queue_icon("chevron-down", x, y + (FOLD_H - 12.0) / 2.0, 12.0, theme::text_dim());
    label(g, x + 18.0, y + (FOLD_H - 11.0) / 2.0, text, 11.0, if hover { theme::text() } else { theme::text_dim() });
    g.hover_pointer |= hover;
    hits.push((Hit::Fold(id), r));
    y + FOLD_H
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_counts_wide_glyphs_as_two() {
        let mut out = Vec::new();
        wrap("가나다ab".chars().map(|c| (c, 0)), 5, &mut out);
        let text: Vec<String> = out.iter().map(|r| r.iter().map(|(c, _)| *c).collect()).collect();
        assert_eq!(text, vec!["가나", "다ab"]);
    }

    #[test]
    fn durations_read_like_a_person() {
        assert_eq!(ago_label(11), "0.01초");
        assert_eq!(ago_label(1_420), "1.4초");
        assert_eq!(ago_label(12_000), "12초");
        assert_eq!(ago_label(184_000), "3분 4초");
        assert_eq!(ago_label(3_720_000), "1시간 2분");
    }
}
