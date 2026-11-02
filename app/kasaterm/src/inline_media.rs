use crate::{gpu, theme, GridCell, UserEvent};
use kasa_gridview::images::{InlineFit, InlineSlot};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use winit::event_loop::EventLoopProxy;
pub(crate) mod cache;

pub(crate) type Rect = (f32, f32, f32, f32);
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct Reference {
    pub id: String,
    pub source: Option<String>,
}
impl Reference {
    pub fn key(&self) -> String {
        format!(
            "media:{:x}:{}",
            Sha256::digest(self.source.as_deref().unwrap_or("local").as_bytes()),
            self.id
        )
    }
    pub fn from_slot(slot: &InlineSlot) -> Option<Self> {
        slot.key
            .starts_with("media:")
            .then(|| serde_json::from_str(&slot.path).ok())
            .flatten()
    }
}
pub(crate) fn display_block(id: &str, video: bool) -> String {
    let count = if video { 12 } else { 4 };
    let mut out = String::from("```text\n");
    for row in 0..count {
        out.push_str(&format!("[[km:{id}:{row:02}/{count:02}]]\n"));
    }
    out.push_str("```\n");
    out
}
pub(crate) fn run(args: &[std::ffi::OsString]) -> anyhow::Result<bool> {
    if args.first().is_none_or(|a| a != "--inline-media") {
        return Ok(false);
    }
    let block = args.get(1).is_some_and(|a| a == "--block");
    anyhow::ensure!(
        args.len() == if block { 3 } else { 2 },
        "usage: mediaopen [--block] FILE"
    );
    let path = std::path::Path::new(&args[if block { 2 } else { 1 }]);
    let probe = crate::media_player::probe(path).map_err(anyhow::Error::msg)?;
    let media = kasa_socket::inline_media::register(
        path,
        kasa_socket::inline_media::Probe {
            duration_ms: probe.duration_ms,
            width: probe.width,
            height: probe.height,
            poster: probe.poster,
        },
    )?;
    use std::io::Write;
    std::io::stdout()
        .lock()
        .write_all(display_block(&media.id, media.kind == "video").as_bytes())?;
    Ok(true)
}
pub(crate) fn compose(
    rows: &mut [Vec<GridCell>],
    origin: (f32, f32),
    cell: (f32, f32),
    cols: usize,
    source: Option<&str>,
    proxy: &EventLoopProxy<UserEvent>,
) -> Vec<InlineSlot> {
    let stripes: Vec<_> = rows
        .iter()
        .enumerate()
        .filter_map(|(i, row)| crate::inline_result::stripe_for(i, row, "[[km:"))
        .collect();
    let mut slots = Vec::new();
    let mut i = 0;
    while let Some(first) = stripes.get(i) {
        let mut end = i + 1;
        while let Some(next) = stripes.get(end) {
            let prev = &stripes[end - 1];
            if next.id != first.id
                || next.total != first.total
                || next.col != first.col
                || next.index != prev.index + 1
                || next.row != prev.row + 1
            {
                break;
            }
            end += 1;
        }
        let reference = Reference {
            id: first.id.clone(),
            source: source.map(str::to_owned),
        };
        cache::request(&reference, proxy, false);
        let width = (cols.saturating_sub(first.col) as f32 * cell.0).min(560.0);
        let x = origin.0 + first.col as f32 * cell.0;
        let y = origin.1 + (first.row as f32 - first.index as f32) * cell.1;
        slots.push(InlineSlot {
            key: reference.key(),
            path: serde_json::to_string(&reference).unwrap(),
            rect: (x, y, width, first.total as f32 * cell.1),
            clip: (
                x,
                origin.1 + first.row as f32 * cell.1,
                width,
                (end - i) as f32 * cell.1,
            ),
            fit: InlineFit::Contain,
            below: false,
            previewable: false,
        });
        for stripe in &stripes[i..end] {
            for c in &mut rows[stripe.row][stripe.col..] {
                if !matches!(c.ch, '│' | '┃') {
                    *c = GridCell::blank();
                }
            }
        }
        i = end;
    }
    slots
}
pub(crate) fn fit_content(slots: &mut [InlineSlot], right: f32) {
    for slot in slots {
        // 대화 줄 바깥의 스크롤바는 그리는 폭과 클릭 폭 모두에서 비워 둔다.
        slot.rect.2 = slot.rect.2.min((right - slot.rect.0).max(0.0));
        slot.clip.2 = slot.clip.2.min((right - slot.clip.0).max(0.0));
    }
}

pub(crate) fn intersection(a: Rect, b: Rect) -> Rect {
    let x = a.0.max(b.0);
    let y = a.1.max(b.1);
    (
        x,
        y,
        (a.0 + a.2 - x).min(b.0 + b.2 - x).max(0.0),
        (a.1 + a.3 - y).min(b.1 + b.3 - y).max(0.0),
    )
}
pub(crate) fn time(seconds: f64) -> String {
    let s = if seconds.is_finite() {
        seconds.max(0.0) as u64
    } else {
        0
    };
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}
pub(crate) fn text(g: &mut gpu::GpuRenderer, r: Rect, text: &str, dim: bool) {
    let safe: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    g.draw_text_clipped(
        r.0,
        r.1,
        &safe,
        gpu::DrawOpts {
            font_size: 13.0,
            color: if dim {
                theme::text_dim()
            } else {
                theme::text()
            },
            bold: false,
            italic: false,
        },
        r.0,
        r.0 + r.2,
    );
}
/// 카드 안에서 도는 재생 하나의 지금 모습 — 재생기는 `media_preview` 가 쥐고, 여기는 그리기만 한다.
pub(crate) struct Live<'a> {
    pub reference: &'a Reference,
    /// 영상 프레임 텍스처. 첫 프레임 전이면 포스터를 그대로 둔다.
    pub frame: Option<&'a str>,
    pub playing: bool,
    pub position: f64,
    pub duration: Option<f64>,
    /// 준비 중·버퍼링·실패를 시각 뒤에 덧붙인다.
    pub note: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Control {
    Toggle,
    Seek,
    Mute,
    Volume,
    Expand,
    Save,
}

/// 그린 카드 하나 — 누름 판정이 그린 자리를 그대로 쓴다. 자리는 모두 보이는 부분으로 잘랐다.
pub(crate) struct Card {
    pub reference: Reference,
    pub rect: Rect,
    pub controls: Vec<(Control, Rect)>,
    /// 탐색 막대 전체 폭 — 끌 때 위치를 비율로 바꾼다.
    pub track: Rect,
    pub volume_track: Rect,
}

pub(crate) fn paint_volume(g: &mut gpu::GpuRenderer, area: Rect, volume: f32, cursor: (f32, f32)) -> Rect {
    text(g, (area.0, area.1 + 3.0, 34.0, 20.0), "볼륨", true);
    paint_volume_track(g, (area.0 + 40.0, area.1, (area.2 - 40.0).max(0.0), area.3), volume, cursor)
}

fn paint_volume_track(g: &mut gpu::GpuRenderer, area: Rect, volume: f32, cursor: (f32, f32)) -> Rect {
    let value = volume.clamp(0.0, 1.0);
    let track = (area.0 + 4.0, area.1, (area.2 - 44.0).max(0.0), area.3);
    let y = area.1 + area.3 / 2.0;
    g.rect(track.0, y - 1.5, track.2, 3.0, theme::border());
    g.rect(track.0, y - 1.5, track.2 * value, 3.0, theme::accent());
    g.round_rect_fill(track.0 + track.2 * value - 4.0, y - 4.0, 8.0, 8.0, 4.0, theme::accent());
    text(g, (area.0 + area.2 - 34.0, area.1 + 3.0, 34.0, 20.0), &format!("{:.0}%", value * 100.0), true);
    g.hover_pointer |= cursor.0 >= track.0 && cursor.0 <= track.0 + track.2
        && cursor.1 >= track.1 && cursor.1 <= track.1 + track.3;
    track
}

const BUTTON: f32 = 32.0;
/// 오른쪽 단추 사이 간격과, 이름 줄이 남겨 둘 최소 폭.
const GAP: f32 = 8.0;
const TEXT_MIN: f32 = 72.0;

/// 카드 아래 줄: 재생 단추 · 이름과 시각 · (재생 중이면 소리) · 크게 보기 · 저장. 재생 중이면 그 위에 탐색 막대.
/// 영상은 위 칸에 포스터, 재생이 시작되면 그 자리에 프레임을 그린다.
pub(crate) fn paint(
    g: &mut gpu::GpuRenderer,
    slots: &[InlineSlot],
    cursor: (f32, f32),
    live: Option<&Live>,
    volume: f32,
    volume_open: Option<&Reference>,
    saving: &dyn Fn(&Reference) -> bool,
) -> Vec<Card> {
    let live_keys: std::collections::HashSet<_> = slots
        .iter()
        .filter(|s| s.key.starts_with("media:"))
        .map(|s| s.key.clone())
        .collect();
    g.images
        .retain(|key, _| !key.starts_with("media:") || live_keys.contains(key));
    let mut cards = Vec::new();
    for slot in slots {
        let Some(reference) = Reference::from_slot(slot) else {
            continue;
        };
        let (x, y, w, h) = slot.rect;
        let clip = intersection(slot.rect, slot.clip);
        if clip.2 <= 0.0 || clip.3 <= 0.0 {
            continue;
        }
        let live = live.filter(|l| *l.reference == reference);
        g.push_clip(clip.0, clip.1, clip.2, clip.3);
        crate::native_controls::panel(g, (x, y, w, h), theme::surface());
        let asset = cache::get(&reference);
        let line_y = (y + h - 52.0).max(y + 8.0);
        let button_y = line_y + 4.0;
        if h > 100.0 {
            let area = (x + 8.0, y + 8.0, (w - 16.0).max(0.0), (line_y - 14.0 - y - 8.0).max(0.0));
            let frame = live.and_then(|l| l.frame).filter(|k| g.has_image(k));
            match (frame, &asset) {
                (Some(key), _) => g.queue_image_contain(key, area.0, area.1, area.2, area.3),
                (None, Ok(Some(asset))) => {
                    if let Some(p) = &asset.poster {
                        if !g.has_image(&slot.key) {
                            g.upload_image(&slot.key, &p.rgba, p.width, p.height);
                        }
                        g.queue_image_contain(&slot.key, area.0, area.1, area.2, area.3);
                    }
                }
                _ => {}
            }
        }
        let ready = matches!(asset, Ok(Some(_)));
        let mut controls = Vec::new();
        let toggle = crate::native_controls::icon_button(
            g,
            (x + 12.0, button_y, BUTTON, BUTTON),
            cursor,
            if live.is_some_and(|l| l.playing) { "pause" } else { "play" },
            Default::default(),
        );
        controls.push((Control::Toggle, toggle));
        let mut right = x + w - 12.0;
        let capacity = ((w - 12.0 - 52.0 - TEXT_MIN) / (BUTTON + GAP)).floor().clamp(0.0, 3.0) as usize;
        let mut wanted = Vec::new();
        if capacity >= 2 { wanted.push((Control::Save, "download", ready && !saving(&reference))); }
        if capacity >= 3 { wanted.push((Control::Expand, "maximize", true)); }
        wanted.push((Control::Mute, if volume == 0.0 { "volume-x" } else { "volume-2" }, true));
        for (control, icon, enabled) in wanted {
            if right - BUTTON - GAP < x + 52.0 + TEXT_MIN {
                break;
            }
            let style = crate::native_controls::Style { enabled, ..Default::default() };
            let r = crate::native_controls::icon_button(g, (right - BUTTON, button_y, BUTTON, BUTTON), cursor, icon, style);
            if enabled {
                controls.push((control, r));
            }
            right -= BUTTON + GAP;
        }
        let mut text_w = (right - x - 52.0).max(0.0);
        let mut volume_track = (0.0, 0.0, 0.0, 0.0);
        if let Some((_, mute)) = controls.iter().find(|(c, _)| *c == Control::Mute) {
            let hovered = cursor.0 >= mute.0 && cursor.0 <= mute.0 + mute.2
                && cursor.1 >= mute.1 && cursor.1 <= mute.1 + mute.3;
            if hovered || volume_open == Some(&reference) {
                let left = (mute.0 - 168.0).max(x + 52.0);
                let area = (left, button_y + 4.0, (mute.0 - 8.0 - left).max(0.0), 24.0);
                if area.2 >= 64.0 {
                    g.round_rect_fill(area.0 - 4.0, button_y, area.2 + 8.0, BUTTON, 4.0, theme::surface_hover());
                    volume_track = paint_volume_track(g, area, volume, cursor);
                    text_w = (left - x - 60.0).max(0.0);
                }
            }
        }
        if volume_track.2 > 0.0 { controls.push((Control::Volume, volume_track)); }
        let (name, detail) = match &asset {
            Ok(Some(asset)) => {
                let detail = match live {
                    Some(l) => format!(
                        "{} / {}{}",
                        time(l.position),
                        l.duration.map(time).unwrap_or_else(|| "—".into()),
                        l.note.as_deref().map(|n| format!(" · {n}")).unwrap_or_default()
                    ),
                    None => format!(
                        "{}{}{}",
                        if asset.media.kind == "video" { "영상" } else { "음성" },
                        asset
                            .media
                            .duration_ms
                            .map(|ms| format!(" · {}", time(ms as f64 / 1000.0)))
                            .unwrap_or_default(),
                        if saving(&reference) { " · 저장하는 중…" } else { "" }
                    ),
                };
                (asset.media.name.clone(), detail)
            }
            Ok(None) => ("미디어 불러오는 중…".into(), String::new()),
            Err(_) => ("미디어를 불러오지 못했어요".into(), "눌러 다시 받기".into()),
        };
        text(g, (x + 52.0, line_y, text_w, 20.0), &name, false);
        text(g, (x + 52.0, line_y + 22.0, text_w, 20.0), &detail, true);
        let mut track = (0.0, 0.0, 0.0, 0.0);
        if let Some(l) = live {
            if let Some(d) = l.duration.filter(|d| *d > 0.0) {
                let bar_y = (line_y - 6.0).max(y + 4.0);
                track = (x + 12.0, bar_y, (w - 24.0).max(0.0), 3.0);
                g.rect(track.0, track.1, track.2, track.3, theme::border());
                g.rect(track.0, track.1, track.2 * (l.position / d).clamp(0.0, 1.0) as f32, track.3, theme::accent());
                controls.push((Control::Seek, (track.0, bar_y - 6.0, track.2, (line_y - bar_y + 6.0).min(15.0))));
            }
        }
        g.pop_clip();
        cards.push(Card {
            reference,
            rect: clip,
            controls: controls.into_iter().map(|(c, r)| (c, intersection(r, clip))).filter(|(_, r)| r.2 > 0.0 && r.3 > 0.0).collect(),
            track,
            volume_track,
        });
    }
    cards
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_namespaces_and_partial_clipping_are_distinct() {
        let a = Reference {
            id: "0123456789abcdef".into(),
            source: None,
        };
        let b = Reference {
            source: Some("http://remote".into()),
            ..a.clone()
        };
        assert_ne!(a.key(), b.key());
        assert_eq!(
            intersection((0.0, -40.0, 80.0, 100.0), (0.0, 0.0, 80.0, 40.0)),
            (0.0, 0.0, 80.0, 40.0)
        );
        assert_eq!(
            intersection((0.0, 0.0, 20.0, 20.0), (50.0, 50.0, 20.0, 20.0)).2,
            0.0
        );
    }
    #[test]
    fn narrow_chat_artwork_and_click_bounds_end_before_the_scrollbar() {
        let mut slots = vec![InlineSlot {
            key: "media:fixture".into(),
            path: String::new(),
            rect: (16.0, 20.0, 304.0, 240.0),
            clip: (16.0, 40.0, 304.0, 180.0),
            fit: InlineFit::Contain,
            below: false,
            previewable: false,
        }];
        let mut image = slots[0].clone();
        image.key = "result:fixture".into();
        image.previewable = true;
        slots.push(image);
        let bar_left = (40 - crate::chat_view::BAR_COLS) as f32 * 8.0;
        fit_content(&mut slots, bar_left);
        assert!(slots
            .iter()
            .all(|s| s.rect.0 + s.rect.2 == bar_left && s.clip.0 + s.clip.2 == bar_left));
        let hit = intersection(slots[0].rect, slots[0].clip);
        assert_eq!(hit.0 + hit.2, bar_left);
        assert_eq!((hit.1, hit.3), (40.0, 180.0));
    }

    #[test]
    fn media_markers_do_not_replace_commands_or_image_blocks() {
        let cells = |text: &str| {
            text.chars()
                .map(|ch| GridCell {
                    ch,
                    ..GridCell::blank()
                })
                .collect::<Vec<_>>()
        };
        for video in [false, true] {
            for line in display_block("0123456789abcdef", video)
                .lines()
                .skip(1)
                .take(if video { 12 } else { 4 })
            {
                assert_eq!(line.len(), 29);
                assert!(crate::inline_result::stripe_for(0, &cells(line), "[[km:").is_some());
                assert!(crate::inline_result::stripe_for(
                    0,
                    &cells(&format!("echo {line}")),
                    "[[km:"
                )
                .is_none());
                assert!(crate::inline_result::stripe_for(0, &cells(line), "[[ki:").is_none());
            }
        }
    }
}
