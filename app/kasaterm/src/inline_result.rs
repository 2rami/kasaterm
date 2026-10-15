use crate::GridCell;
use kasa_gridview::images::{InlineFit, InlineSlot};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub(crate) const INSTRUCTIONS: &str = "To show a result image inside this terminal, run `imgopen --block /absolute/path/to/image.png`, then include the returned fenced display block verbatim in your user-facing response. Do not omit, indent, wrap, summarize or escape its marker rows. Add a normal file link outside the block. Kasaterm draws the image in those reserved rows for both Claude Code and Codex; view_image/Read only lets the model inspect it. Do not open an image tab/window unless the user explicitly asks. Do not emit Kitty escapes from agent tool output.";

use kasa_socket::inline_results::{root, valid_id};

pub(crate) fn register(image: &image::DynamicImage) -> anyhow::Result<String> {
    let root = root().ok_or_else(|| anyhow::anyhow!("이미지 저장 위치를 찾을 수 없습니다"))?;
    register_in(&root, image)
}

fn register_in(root: &Path, image: &image::DynamicImage) -> anyhow::Result<String> {
    let preview = if image.width() > 1600 || image.height() > 1600 {
        image.thumbnail(1600, 1600)
    } else {
        image.clone()
    };
    let mut png = std::io::Cursor::new(Vec::new());
    preview.write_to(&mut png, image::ImageFormat::Png)?;
    let bytes = png.into_inner();
    let id = format!("{:x}", Sha256::digest(&bytes))[..16].to_string();
    std::fs::create_dir_all(root)?;
    let path = root.join(format!("{id}.png"));
    if !std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file()) {
        let temp = root.join(format!("{}.part", uuid::Uuid::new_v4()));
        std::fs::write(&temp, bytes)?;
        if let Err(error) = std::fs::rename(&temp, &path) {
            let _ = std::fs::remove_file(&temp);
            return Err(error.into());
        }
    }
    Ok(id)
}

pub(crate) fn display_block(id: &str, width: u32, height: u32) -> String {
    let count = ((64.0 * height as f32 / width.max(1) as f32 / 2.2).ceil() as usize).clamp(6, 18);
    let mut block = String::from("```text\n");
    for row in 0..count {
        block.push_str(&format!("[[ki:{id}:{row:02}/{count:02}]]\n"));
    }
    block.push_str("```\n");
    block
}

#[derive(Debug, PartialEq, Eq)]
struct Stripe {
    row: usize,
    col: usize,
    id: String,
    index: usize,
    total: usize,
}

fn stripe(row: usize, cells: &[GridCell]) -> Option<Stripe> {
    let start = cells
        .windows(5)
        .position(|w| w.iter().map(|c| c.ch).eq("[[ki:".chars()))?;
    // 설명이나 명령 속의 표식을 그림으로 바꾸면 실제 본문을 덮는다.
    let gutter = |c: &GridCell| matches!(c.ch, ' ' | '\0' | '│' | '┃' | '⏺' | '•' | '●' | '›');
    if !cells[..start].iter().all(gutter) {
        return None;
    }
    let text: String = cells[start..].iter().map(|c| c.ch).collect();
    let end = text.find("]]")? + 2;
    if !text[end..]
        .chars()
        .all(|c| matches!(c, ' ' | '\0' | '│' | '┃'))
    {
        return None;
    }
    let (id, position) = text[5..end - 2].split_once(':')?;
    if !valid_id(id) {
        return None;
    }
    let (index, total) = position.split_once('/')?;
    if index.len() != 2 || total.len() != 2 {
        return None;
    }
    let (index, total) = (index.parse::<usize>().ok()?, total.parse::<usize>().ok()?);
    if !(1..=24).contains(&total) || index >= total {
        return None;
    }
    Some(Stripe {
        row,
        col: start,
        id: id.into(),
        index,
        total,
    })
}

fn asset_path(id: &str) -> Option<(PathBuf, u32, u32)> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Option<(u32, u32)>>>> = OnceLock::new();
    let path = root()?.join(format!("{id}.png"));
    let mut cache = CACHE.get_or_init(Default::default).lock().ok()?;
    if cache.len() > 512 {
        cache.clear();
    }
    let dims = *cache.entry(path.clone()).or_insert_with(|| {
        if !std::fs::symlink_metadata(&path)
            .is_ok_and(|m| m.file_type().is_file() && m.len() <= 12 * 1024 * 1024)
        {
            return None;
        }
        image::image_dimensions(&path)
            .ok()
            .filter(|(w, h)| *w > 0 && *h > 0 && *w <= 1600 && *h <= 1600)
    });
    dims.map(|(w, h)| (path, w, h))
}

pub(crate) fn compose(
    rows: &mut [Vec<GridCell>],
    origin: (f32, f32),
    cell: (f32, f32),
    cols: usize,
) -> Vec<InlineSlot> {
    compose_with(rows, origin, cell, cols, asset_path)
}

fn compose_with(
    rows: &mut [Vec<GridCell>],
    origin: (f32, f32),
    cell: (f32, f32),
    cols: usize,
    resolve: impl Fn(&str) -> Option<(PathBuf, u32, u32)>,
) -> Vec<InlineSlot> {
    let stripes: Vec<_> = rows
        .iter()
        .enumerate()
        .filter_map(|(r, cells)| stripe(r, cells))
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
        if let Some((path, image_w, image_h)) = resolve(&first.id) {
            let x = origin.0 + first.col as f32 * cell.0;
            let y = origin.1 + (first.row as f32 - first.index as f32) * cell.1;
            let height = first.total as f32 * cell.1;
            let ratio = image_w as f32 / image_h as f32;
            let width = (cols.saturating_sub(first.col) as f32 * cell.0).min(height * ratio);
            let height = width / ratio;
            let clip_y = origin.1 + first.row as f32 * cell.1;
            let clip_h = (end - i) as f32 * cell.1;
            slots.push(InlineSlot {
                key: format!("result:{}", first.id),
                path: path.to_string_lossy().into_owned(),
                rect: (x, y, width, height),
                clip: (x, clip_y, width, clip_h),
                fit: InlineFit::Contain,
                below: false,
            });
            for stripe in &stripes[i..end] {
                // 코드 블록의 명시 배경이 그림을 덮지 않도록 예약된 빈 영역도 비운다.
                for cell in &mut rows[stripe.row][stripe.col..] {
                    if !matches!(cell.ch, '│' | '┃') {
                        *cell = GridCell::blank();
                    }
                }
            }
        }
        i = end;
    }
    slots
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cells(text: &str) -> Vec<GridCell> {
        text.chars()
            .map(|ch| GridCell {
                ch,
                ..GridCell::blank()
            })
            .collect()
    }
    #[test]
    fn markers_are_short_and_every_row_identifies_its_slice() {
        let block = display_block("0123456789abcdef", 800, 500);
        let lines: Vec<_> = block.lines().skip(1).take_while(|l| *l != "```").collect();
        assert_eq!(lines.len(), 18);
        for (i, line) in lines.iter().enumerate() {
            assert_eq!(line.len(), 29);
            let value = stripe(i, &cells(&format!("  {line}"))).unwrap();
            assert_eq!((value.index, value.total, value.col), (i, 18, 2));
        }
    }
    #[test]
    fn ordinary_text_commands_and_invalid_ids_are_never_reserved_rows() {
        for text in [
            "echo [[ki:0123456789abcdef:00/12]]",
            "[[ki:0123456789abcdef:00/12]] explanation",
            "[[ki:../../etc/passwd:00/12]]",
            "[[ki:0123456789abcdef:12/12]]",
            "[[ki:0123456789abcdef:00/99]]",
        ] {
            assert!(stripe(0, &cells(text)).is_none(), "{text}");
        }
    }
    #[test]
    fn partial_scrollback_can_start_in_the_middle_of_an_image() {
        let marker = stripe(0, &cells("  [[ki:0123456789abcdef:07/12]]")).unwrap();
        assert_eq!((marker.row, marker.index, marker.total), (0, 7, 12));
    }
    #[test]
    fn cache_is_content_addressed_and_survives_the_source_path() {
        let dir = std::env::temp_dir().join(format!("inline-result-test-{}", uuid::Uuid::new_v4()));
        let image = image::DynamicImage::new_rgb8(64, 32);
        let a = register_in(&dir, &image).unwrap();
        let b = register_in(&dir, &image).unwrap();
        assert_eq!(a, b);
        assert!(valid_id(&a));
        assert_eq!(
            image::image_dimensions(dir.join(format!("{a}.png"))).unwrap(),
            (64, 32)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn cropping_and_folding_never_blank_neighboring_text() {
        let mut rows = vec![
            cells("BEFORE"),
            cells("  [[ki:0123456789abcdef:07/12]]"),
            cells("  [[ki:0123456789abcdef:08/12]]"),
            cells("AFTER"),
            cells("  [[ki:0123456789abcdef:11/12]]"),
        ];
        let slots = compose_with(&mut rows, (10.0, 20.0), (8.0, 18.0), 80, |_| {
            Some(("fixture.png".into(), 640, 240))
        });
        assert_eq!(slots.len(), 2);
        assert_eq!(slots[0].rect.1, 20.0 + (1.0 - 7.0) * 18.0);
        assert_eq!(slots[0].clip, (26.0, 38.0, 576.0, 36.0));
        assert_eq!(slots[1].clip.3, 18.0);
        assert_eq!(rows[0], cells("BEFORE"));
        assert_eq!(rows[3], cells("AFTER"));
        assert!(rows[1].iter().all(|c| c.ch == ' '));
    }

    #[test]
    fn unknown_assets_leave_the_answer_readable() {
        let mut rows = vec![cells("  [[ki:0123456789abcdef:00/12]]"), cells("AFTER")];
        let original = rows.clone();
        assert!(compose_with(&mut rows, (0.0, 0.0), (8.0, 18.0), 80, |_| None).is_empty());
        assert_eq!(rows, original);
    }
}

pub(crate) fn verify_frame(g: &kasa_gridview::GridRenderer, slots: &[InlineSlot]) {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        OnceLock,
    };
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    static DONE: AtomicBool = AtomicBool::new(false);
    if !crate::verification_run()
        || std::env::var("KASATERM_INLINE_RESULT_VERIFY").as_deref() != Ok("1")
        || START
            .get_or_init(std::time::Instant::now)
            .elapsed()
            .as_millis()
            < 7000
        || DONE.swap(true, Ordering::Relaxed)
    {
        return;
    }
    if let Some(root) = std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .filter(|p| p.starts_with("/tmp") || p.starts_with("/private/tmp"))
    {
        let result:Vec<_>=slots.iter().map(|s|serde_json::json!({"key":s.key,"path":s.path,"rect":s.rect,"clip":s.clip,"uploaded":g.has_image(&s.key),"drawn":g.drawn_image_rects(&s.key)})).collect();
        let _ = std::fs::write(
            root.join("inline-result-frame.json"),
            serde_json::to_vec(&result).unwrap(),
        );
    }
}
