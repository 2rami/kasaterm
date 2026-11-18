//! 하단바 「학생 쪽지」 — 폰 허브의 종 목록과 같은 기록(`kasa_students::notes`)을 PC 에서 연다. 안 읽은 것만
//! 서고, 줄을 누르면 읽음으로 두고 그 칸으로 간다. ×는 지우고 「모두 읽음」은 전부 읽음 — 폰과 한 파일이라 한쪽에서
//! 읽으면 다른 쪽 수도 준다. 치수는 `docs/design.md` 「하단바 학생 쪽지」.

use super::*;
use kasa_students::notes::Note;
use std::sync::Mutex;
use std::time::{Duration, Instant};

type Rect = (f32, f32, f32, f32);

const POP_W: f32 = 380.0;
const HEAD_H: f32 = 40.0;
const FACE: f32 = 28.0;
const LINE_H: f32 = 17.0;
const SUMMARY_LINES: usize = 2;
const EMPTY_H: f32 = 64.0;
/// 칩이 프레임마다 수를 묻는다 — 파일은 이 박자로만 읽는다. 폰이 읽음으로 바꾼 것도 이 안에 따라온다.
const POLL: Duration = Duration::from_secs(2);

static CACHE: Mutex<Option<(Instant, Vec<Note>)>> = Mutex::new(None);

fn with_unread<R>(f: impl FnOnce(&[Note]) -> R) -> R {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if cache.as_ref().is_none_or(|(at, _)| at.elapsed() >= POLL) {
        let notes = kasa_students::notes::list().into_iter().filter(|n| !n.read).collect();
        *cache = Some((Instant::now(), notes));
    }
    f(cache.as_ref().map_or(&[][..], |(_, notes)| notes))
}

pub(crate) fn count() -> usize {
    with_unread(<[Note]>::len)
}

/// 읽음·지움 뒤에 — 다음 프레임이 옛 목록을 2초 더 그리지 않게.
pub(crate) fn forget() {
    *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// 폰 쪽지 줄의 알약과 같은 말·색(`notes_sheet.dart noteKindStyle`).
fn kind_style(kind: &str) -> (&'static str, [u8; 4]) {
    match kind {
        "permission" => ("승인", theme::attention()),
        "question" => ("질문", theme::attention()),
        "blocked" => ("막힘", theme::attention()),
        "done" => ("끝냄", theme::success()),
        _ => ("알림", theme::text_dim()),
    }
}

fn ago(when: u64, now: u64) -> String {
    let secs = now.saturating_sub(when);
    match secs {
        0..60 => "방금".into(),
        60..3600 => format!("{}분 전", secs / 60),
        3600..86400 => format!("{}시간 전", secs / 3600),
        _ => format!("{}일 전", secs / 86400),
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn text(g: &mut gpu::GpuRenderer, x: f32, y: f32, value: &str, size: f32, color: [u8; 4], bold: bool) {
    g.draw_text(x, y, value, gpu::DrawOpts { font_size: size, color, bold, italic: false });
}

/// 본문 두 줄 — 넘치면 둘째 줄 끝을 줄임표로.
fn summary_lines(g: &mut gpu::GpuRenderer, summary: &str, width: f32) -> Vec<String> {
    let mut lines = crate::clipboard::wrap_text(summary, width, |line| g.measure_chrome_text(line, 11.5, false));
    lines.retain(|line| !line.trim().is_empty());
    if lines.len() > SUMMARY_LINES {
        let rest = lines[SUMMARY_LINES - 1..].join(" ");
        lines.truncate(SUMMARY_LINES - 1);
        lines.push(crate::info::fit_text(g, &format!("{rest}…"), width, 11.5, false));
    }
    lines
}

fn row_height(lines: usize) -> f32 {
    10.0 + 18.0 + lines.max(1) as f32 * LINE_H + 10.0
}

pub(super) fn paint(g: &mut gpu::GpuRenderer, sb: &mut state::StatusbarState, cursor: (f32, f32), anchor: Rect, viewport: (f32, f32)) {
    let notes = with_unread(<[Note]>::to_vec);
    let now = now_secs();
    let w = POP_W.min((viewport.0 - 16.0).max(1.0));
    let text_x = 16.0 + FACE + 10.0;
    let text_w = (w - text_x - 16.0).max(1.0);
    let rows: Vec<(Note, Vec<String>)> = notes.into_iter().map(|n| {
        let lines = summary_lines(g, &n.summary, text_w);
        (n, lines)
    }).collect();
    let body = if rows.is_empty() { EMPTY_H } else { rows.iter().map(|(_, lines)| row_height(lines.len())).sum() };
    let inner = body.min((anchor.1 - HEAD_H - 22.0).max(1.0)).min((viewport.1 * 0.65).max(1.0));
    let h = HEAD_H + inner + 6.0;
    let x = (anchor.0 + anchor.2 - w).clamp(8.0, (viewport.0 - w - 8.0).max(8.0));
    let y = (anchor.1 - h - 6.0).max(8.0);
    sb.popover_rect = Some((x, y, w, h));
    panel_rect_outlined(g, x, y, w, h, theme::radius_md(), theme::surface());
    text(g, x + 14.0, y + 13.0, "학생 쪽지", 12.5, theme::text(), true);
    if !rows.is_empty() {
        let label = "모두 읽음";
        let bw = crate::native_controls::button_width(g, label, 28.0, false);
        let slot = (x + w - 8.0 - bw, y + 6.0, bw, 28.0);
        let rect = crate::native_controls::text_button(g, slot, cursor, label, Default::default());
        sb.popover_hits.push((state::StatusbarHit::ReadAllNotes, rect));
    }
    sb.popover_scroll = sb.popover_scroll.clamp(0.0, (body - inner).max(0.0));
    let viewport_rect = (x + 6.0, y + HEAD_H, (w - 12.0).max(0.0), inner);
    g.push_clip(viewport_rect.0, viewport_rect.1, viewport_rect.2, viewport_rect.3);
    if rows.is_empty() {
        let empty = "안 읽은 쪽지가 없어요";
        let ew = g.measure_chrome_text(empty, 11.5, false);
        text(g, x + (w - ew) / 2.0, y + HEAD_H + (EMPTY_H - 14.0) / 2.0, empty, 11.5, theme::text_mute(), false);
    }
    let mut oy = y + HEAD_H - sb.popover_scroll;
    for (note, lines) in &rows {
        let rh = row_height(lines.len());
        let row = (x + 6.0, oy, w - 12.0, rh);
        let hover = hit(cursor, &row) && hit(cursor, &viewport_rect);
        if hover {
            round_rect(g, row.0, row.1, row.2, row.3, theme::radius_sm(), theme::surface_hover());
        }
        if !crate::sprites::draw_student_face(g, &note.character, x + 16.0, oy + 10.0, FACE) {
            g.queue_icon("bell", x + 16.0 + 7.0, oy + 17.0, FACE - 14.0, theme::text_mute());
        }
        let tx = x + text_x;
        let right = x + w - 16.0;
        let (kind, tint) = kind_style(&note.kind);
        let kind_w = g.measure_chrome_text(kind, 10.0, true) + 12.0;
        // ×는 올렸을 때만 그리지만 자리는 늘 잡는다 — 옮기자마자 누르면 그린 프레임보다 클릭이 먼저 와 줄이 열린다.
        let close = (right - 22.0, oy + 7.0, 22.0, 22.0);
        if let Some(close) = g.clip_hit(close) {
            sb.popover_hits.push((state::StatusbarHit::DeleteNote(note.id), close));
        }
        let side = if hover {
            crate::native_controls::icon_button(g, close, cursor, "x", Default::default());
            22.0
        } else {
            let when = ago(note.when, now);
            let ww = g.measure_chrome_text(&when, 10.0, false);
            text(g, right - ww, oy + 12.0, &when, 10.0, theme::text_mute(), false);
            ww
        };
        let who = if note.character.is_empty() { "학생" } else { note.character.as_str() };
        let who = crate::info::fit_text(g, who, (right - side - 12.0 - kind_w - 6.0 - tx).max(0.0), 12.0, true);
        text(g, tx, oy + 10.0, &who, 12.0, theme::text(), true);
        let pill_x = tx + g.measure_chrome_text(&who, 12.0, true) + 6.0;
        round_rect(g, pill_x, oy + 10.0, kind_w, 17.0, 8.5, theme::with_alpha(tint, 0x29));
        text(g, pill_x + 6.0, oy + 12.5, kind, 10.0, tint, true);
        for (i, line) in lines.iter().enumerate() {
            text(g, tx, oy + 32.0 + i as f32 * LINE_H, line, 11.5, theme::text_dim(), false);
        }
        if let Some(row) = g.clip_hit(row) {
            sb.popover_hits.push((state::StatusbarHit::OpenNote(note.id, note.pane.clone()), row));
        }
        oy += rh;
    }
    g.pop_clip();
    if body > inner && inner > 0.0 {
        let thumb = (inner * inner / body).max(16.0).min(inner);
        let top = y + HEAD_H + (inner - thumb) * sb.popover_scroll / (body - inner);
        round_rect(g, x + w - 5.0, top, 3.0, thumb, 1.5, theme::text_mute());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ages_read_like_the_phone_list() {
        assert_eq!(ago(1_000, 1_030), "방금");
        assert_eq!(ago(1_000, 1_000 + 5 * 60), "5분 전");
        assert_eq!(ago(1_000, 1_000 + 3 * 3600), "3시간 전");
        assert_eq!(ago(1_000, 1_000 + 2 * 86400), "2일 전");
        assert_eq!(ago(2_000, 1_000), "방금", "시계가 뒤로 가도 음수가 안 된다");
    }

    #[test]
    fn kinds_keep_the_phone_words() {
        assert_eq!(kind_style("permission").0, "승인");
        assert_eq!(kind_style("question").0, "질문");
        assert_eq!(kind_style("blocked").0, "막힘");
        assert_eq!(kind_style("done").0, "끝냄");
        for kind in kasa_students::notes::KINDS {
            assert_ne!(kind_style(kind).0, "알림", "{kind}");
        }
    }
}
