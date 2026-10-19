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
pub(crate) fn paint(g: &mut gpu::GpuRenderer, slots: &[InlineSlot], cursor: (f32, f32)) {
    let live: std::collections::HashSet<_> = slots
        .iter()
        .filter(|s| s.key.starts_with("media:"))
        .map(|s| s.key.clone())
        .collect();
    g.images
        .retain(|key, _| !key.starts_with("media:") || live.contains(key));
    for slot in slots {
        let Some(reference) = Reference::from_slot(slot) else {
            continue;
        };
        let (x, y, w, h) = slot.rect;
        let clip = intersection(slot.rect, slot.clip);
        if clip.2 <= 0.0 || clip.3 <= 0.0 {
            continue;
        }
        g.push_clip(clip.0, clip.1, clip.2, clip.3);
        crate::native_controls::panel(g, (x, y, w, h), theme::surface());
        let asset = cache::get(&reference);
        let (name, detail) = match &asset {
            Ok(Some(asset)) => {
                if let Some(p) = &asset.poster {
                    if !g.has_image(&slot.key) {
                        g.upload_image(&slot.key, &p.rgba, p.width, p.height);
                    }
                    if h > 100.0 {
                        g.queue_image_contain(
                            &slot.key,
                            x + 8.0,
                            y + 8.0,
                            (w - 16.0).max(0.0),
                            (h - 64.0).max(0.0),
                        );
                    }
                }
                (
                    asset.media.name.clone(),
                    format!(
                        "{}{} · 눌러 재생",
                        if asset.media.kind == "video" {
                            "영상"
                        } else {
                            "음성"
                        },
                        asset
                            .media
                            .duration_ms
                            .map(|ms| format!(" · {}", time(ms as f64 / 1000.0)))
                            .unwrap_or_default()
                    ),
                )
            }
            Ok(None) => ("미디어 불러오는 중…".into(), "".into()),
            Err(_) => ("미디어를 불러오지 못했어요".into(), "눌러 다시 받기".into()),
        };
        let line_y = (y + h - 52.0).max(y + 8.0);
        crate::native_controls::icon_button(
            g,
            (x + 12.0, line_y + 4.0, 32.0, 32.0),
            cursor,
            "play",
            Default::default(),
        );
        text(
            g,
            (x + 52.0, line_y, (w - 64.0).max(0.0), 20.0),
            &name,
            false,
        );
        text(
            g,
            (x + 52.0, line_y + 22.0, (w - 64.0).max(0.0), 20.0),
            &detail,
            true,
        );
        g.pop_clip();
    }
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
