//! 하단바 「학생 쪽지」 — 폰 허브의 종 목록과 같은 기록(`kasa_students::notes`)을 PC 에서 연다. 이 기기 것과 붙은
//! 기기들의 것(기기 명부 폴러가 받아 둔 `/term/notes?unread=1`)을 한 목록에 안 읽은 것만 세운다. 쪽지마다 정본은
//! 그 학생이 도는 기기 한 곳이라, 읽음·지움·「모두 읽음」은 그 기기에 적는다 — 그래서 한 기기에서 읽으면 다른 PC 와
//! 폰의 수도 같이 준다. 치수는 `docs/design.md` 「하단바 학생 쪽지」.

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
/// 칩이 프레임마다 수를 묻는다 — 파일·캐시는 이 박자로만 읽는다. 폰이 읽음으로 바꾼 것도 이 안에 따라온다.
const POLL: Duration = Duration::from_secs(2);
/// 다른 기기에 읽음·지움을 보낸 뒤 그 기기의 다음 답(폴러 5초)이 오기 전까지 그 줄을 다시 세우지 않는 시간.
const SENT_HOLD: Duration = Duration::from_secs(30);

/// 줄 하나. `machine` 은 그 쪽지가 사는 기기의 명부 이름 — 이 기기 것은 None.
#[derive(Clone)]
pub(crate) struct Entry {
    pub(crate) machine: Option<String>,
    pub(crate) note: Note,
}

static CACHE: Mutex<Option<(Instant, Vec<Entry>)>> = Mutex::new(None);
static SENT: Mutex<Vec<(String, u64, Instant)>> = Mutex::new(Vec::new());

/// 이 기기 것 + 다른 기기 것, 최근 것부터. 보낸 지 얼마 안 된 줄과 명부에 섞여 든 이 기기 자신은 뺀다.
fn merged(local: Vec<Note>, remote: Vec<(String, Vec<Note>)>, sent: &[(String, u64, Instant)]) -> Vec<Entry> {
    let mut out: Vec<Entry> = local.into_iter().filter(|n| !n.read).map(|note| Entry { machine: None, note }).collect();
    for (label, notes) in remote {
        out.extend(notes.into_iter()
            .filter(|n| !n.read && kasa_students::notes::KINDS.contains(&n.kind.as_str()))
            .filter(|n| !sent.iter().any(|(l, id, _)| *l == label && *id == n.id))
            .map(|note| Entry { machine: Some(label.clone()), note }));
    }
    out.sort_by(|a, b| b.note.when.cmp(&a.note.when).then(b.note.id.cmp(&a.note.id)));
    out
}

fn gather() -> Vec<Entry> {
    let me = kasa_collab::board_service::local_id().ok();
    let remote = kasa_device::machines::cached_notes().into_iter()
        .filter(|(label, _)| me.is_none() || kasa_device::machines::find(label).and_then(|m| m.machine_id) != me)
        .filter_map(|(label, notes)| serde_json::from_value::<Vec<Note>>(notes).ok().map(|n| (label, n)))
        .collect();
    let mut sent = SENT.lock().unwrap_or_else(|e| e.into_inner());
    sent.retain(|(_, _, at)| at.elapsed() < SENT_HOLD);
    merged(kasa_students::notes::list(), remote, &sent)
}

fn with_unread<R>(f: impl FnOnce(&[Entry]) -> R) -> R {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if cache.as_ref().is_none_or(|(at, _)| at.elapsed() >= POLL) {
        *cache = Some((Instant::now(), gather()));
    }
    f(cache.as_ref().map_or(&[][..], |(_, entries)| entries))
}

pub(crate) fn count() -> usize {
    with_unread(<[Entry]>::len)
}

/// 지금 목록 — 「모두 읽음」이 기기마다 무엇을 읽음으로 보낼지 가른다.
pub(crate) fn unread() -> Vec<Entry> {
    with_unread(<[Entry]>::to_vec)
}

/// 읽음·지움 뒤에 — 다음 프레임이 옛 목록을 2초 더 그리지 않게.
pub(crate) fn forget() {
    *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

#[derive(Clone, Copy)]
pub(crate) enum Change { Read, Delete }

/// 다른 기기의 쪽지를 그 기기에 읽음·지움으로 적는다. 답이 오기 전까지 그 줄은 세우지 않고, 못 적었으면 다시
/// 세우고 까닭을 토스트로 알린다. 원격 왕복은 그리기 스레드 밖에서.
pub(crate) fn tell_origin(proxy: winit::event_loop::EventLoopProxy<crate::UserEvent>, label: String, change: Change, ids: Vec<u64>) {
    if ids.is_empty() { return; }
    SENT.lock().unwrap_or_else(|e| e.into_inner()).extend(ids.iter().map(|id| (label.clone(), *id, Instant::now())));
    forget();
    std::thread::spawn(move || {
        let path = match change { Change::Read => "/term/notes/read", Change::Delete => "/term/notes/delete" };
        let result = kasa_device::machines::find(&label)
            .ok_or_else(|| anyhow::anyhow!("명부에 없어요"))
            .and_then(|m| kasa_remote::remote::remote_post_json(&m.base, path, &serde_json::json!({ "ids": ids })));
        match result {
            Ok(v) if v.get("ok").and_then(serde_json::Value::as_bool) == Some(true) => kasa_device::machines::poke(),
            outcome => {
                SENT.lock().unwrap_or_else(|e| e.into_inner()).retain(|(l, id, _)| *l != label || !ids.contains(id));
                forget();
                let why = match outcome { Err(e) => format!("{e:#}"), Ok(v) => v.to_string() };
                let _ = proxy.send_event(crate::UserEvent::SocketToast(format!("{label} 의 쪽지를 고치지 못했어요 — {why}")));
            }
        }
    });
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
    let entries = unread();
    let now = now_secs();
    let w = POP_W.min((viewport.0 - 16.0).max(1.0));
    let text_x = 16.0 + FACE + 10.0;
    let text_w = (w - text_x - 16.0).max(1.0);
    let rows: Vec<(Entry, Vec<String>)> = entries.into_iter().map(|e| {
        let lines = summary_lines(g, &e.note.summary, text_w);
        (e, lines)
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
    for (entry, lines) in &rows {
        let note = &entry.note;
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
            sb.popover_hits.push((state::StatusbarHit::DeleteNote(entry.machine.clone(), note.id), close));
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
        let room = (right - side - 12.0 - kind_w - 6.0 - tx).max(0.0);
        let who = crate::info::fit_text(g, who, room, 12.0, true);
        text(g, tx, oy + 10.0, &who, 12.0, theme::text(), true);
        let mut pill_x = tx + g.measure_chrome_text(&who, 12.0, true) + 6.0;
        if let Some(machine) = &entry.machine {
            let place = crate::info::fit_text(g, &format!("· {machine}"), (tx + room - pill_x).max(0.0), 11.0, false);
            text(g, pill_x - 2.0, oy + 11.0, &place, 11.0, theme::text_mute(), false);
            pill_x += g.measure_chrome_text(&place, 11.0, false) + 4.0;
        }
        round_rect(g, pill_x, oy + 10.0, kind_w, 17.0, 8.5, theme::with_alpha(tint, 0x29));
        text(g, pill_x + 6.0, oy + 12.5, kind, 10.0, tint, true);
        for (i, line) in lines.iter().enumerate() {
            text(g, tx, oy + 32.0 + i as f32 * LINE_H, line, 11.5, theme::text_dim(), false);
        }
        if let Some(row) = g.clip_hit(row) {
            sb.popover_hits.push((state::StatusbarHit::OpenNote(entry.machine.clone(), note.id, note.pane.clone()), row));
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

    fn note(id: u64, kind: &str, when: u64, read: bool) -> Note {
        Note { id, pane: format!("%{id}"), character: "학생".into(), kind: kind.into(), summary: "본문".into(), when, read }
    }

    #[test]
    fn other_devices_notes_join_the_list_in_time_order() {
        let local = vec![note(1, "done", 100, false), note(2, "done", 300, true)];
        let remote = vec![
            ("맥미니".to_string(), vec![note(1, "question", 200, false), note(7, "page", 400, false)]),
            ("집PC".to_string(), vec![note(3, "permission", 50, false), note(4, "blocked", 500, true)]),
        ];
        let got: Vec<(Option<String>, u64)> = merged(local, remote, &[]).into_iter().map(|e| (e.machine, e.note.id)).collect();
        assert_eq!(got, vec![(Some("맥미니".into()), 1), (None, 1), (Some("집PC".into()), 3)],
            "읽은 것·모르는 종류는 빠지고, 같은 번호라도 기기가 다르면 다른 줄이다");
    }

    #[test]
    fn a_note_sent_to_its_device_stays_down_until_that_device_answers() {
        let remote = || vec![("맥미니".to_string(), vec![note(1, "done", 10, false), note(2, "done", 20, false)])];
        let sent = [("맥미니".to_string(), 2, Instant::now())];
        let ids: Vec<u64> = merged(Vec::new(), remote(), &sent).iter().map(|e| e.note.id).collect();
        assert_eq!(ids, vec![1]);
        let other = [("집PC".to_string(), 2, Instant::now())];
        assert_eq!(merged(Vec::new(), remote(), &other).len(), 2, "다른 기기에 보낸 같은 번호는 상관없다");
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
