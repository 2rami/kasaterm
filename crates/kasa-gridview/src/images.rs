//! 칸 안 그림(OSC 1337·kitty) — PTY 가 준 자리를 화면 상자로 옮기고, 파일을 읽어 텍스처로 올려 그린다.

use std::collections::{HashMap, HashSet};

use crate::renderer::GridRenderer;

/// 인라인 그림 한 장의 이번 프레임 자리. 좌표는 LOGICAL px(queue_image 관례).
#[derive(Clone, Debug)]
pub struct InlineSlot {
    /// 텍스처 키.
    pub key: String,
    pub path: String,
    /// 그림 상자.
    pub rect: (f32, f32, f32, f32),
    /// 보이는 영역 — 이 밖은 잘린다.
    pub clip: (f32, f32, f32, f32),
    pub fit: InlineFit,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum InlineFit {
    /// 상자 안 가운데, 원본 크기까지만(OSC 1337 — PTY 가 칸 수를 재어 준다).
    Native,
    /// 원본 크기까지만, 상자를 그림 비율로 좁혀 왼쪽에 붙인다(글 흐름 그림 —
    /// 박스가 그림보다 넓으면 그림만 한가운데로 떨어져 나온다).
    Hug,
    /// 비율을 지켜 상자에 꽉 맞춘다, 작으면 키운다(kitty 놓기 규칙).
    Contain,
}

/// PTY 가 준 그림 배치 한 장을 칸 글자판 원점(`origin`)·셀 크기에 놓는다. 화면 프레임과
/// 화면 밖 캡처가 같은 자리에 그리도록 한곳에 둔다. `dy` 는 입력창을 바닥으로 내린 칸의
/// 줄 옮김, `rows` 는 칸 높이(줄) — 그 밖은 잘린다.
pub fn inline_slot(
    v: &kasa_screen::screen::InlineImageView,
    key: String,
    (left, top): (f32, f32),
    (cw, ch): (f32, f32),
    rows: usize,
    dy: i32,
) -> InlineSlot {
    let (clip_y0, clip_y1) = (top, top + rows as f32 * ch);
    let rect = (
        left + v.col as f32 * cw,
        top + (v.row + dy) as f32 * ch,
        v.cols as f32 * cw,
        v.rows as f32 * ch,
    );
    let (clip, fit) = match v.clip {
        Some(c) => {
            let y0 = (top + (c.row + dy) as f32 * ch).max(clip_y0);
            let y1 = (top + (c.row + dy + c.rows as i32) as f32 * ch).min(clip_y1);
            let x0 = left + c.col as f32 * cw;
            ((x0, y0, c.cols as f32 * cw, (y1 - y0).max(0.0)), InlineFit::Contain)
        }
        None => ((rect.0, clip_y0, rect.2, clip_y1 - clip_y0), InlineFit::Native),
    };
    InlineSlot { key, path: v.path.clone(), rect, clip, fit }
}

/// 인라인 이미지(OSC 1337·kitty)를 올리고 그린다 — 파일에서 한 번 디코드해 텍스처로
/// 올리고, 이번 프레임 배치에 없는 키는 텍스처를 놓는다. PTY 쪽이 뷰포트에
/// 겹치는 그림만 보내므로, 스크롤로 벗어난 그림의 GPU 메모리가 여기서 함께
/// 회수된다(안 놓으면 샌다). 디코드에 실패한 키는 false 로 남겨 매 프레임
/// 재시도하지 않는다.
///
/// 키 집합은 이번 프레임에 **렌더된 pane** 기준이다 — 워크스페이스를 전환하면
/// 그쪽 그림 텍스처가 놓였다가 돌아올 때 다시 디코드된다(전환은 드물고
/// 디코드는 ms 급이라 캐시를 창 넘어 유지할 이유가 없다).
/// `InlineFit::Hug` 면 박스를 그림 비율만큼 좁혀 **왼쪽에 붙인다**. `queue_image` 의
/// contain-fit 은 박스 안 중앙 정렬이라, 준 박스가 그림보다 넓으면 글 흐름에서
/// 그림만 한가운데로 떨어져 나온다. OSC 1337 경로는 PTY 가 셀 수를 재어 주므로
/// 박스가 이미 맞아 이 손질이 필요 없다.
#[derive(Default)]
pub struct InlineImages {
    /// 값은 디코드한 픽셀 크기 — `Hug` 가 상자를 좁히는 데 쓴다. `None` 은 디코드 실패라, 장마다 같은
    /// 파일을 다시 열지 않게 남겨 둔다.
    uploaded: HashMap<String, Option<(u32, u32)>>,
}

impl InlineImages {
    pub fn paint(&mut self, g: &mut GridRenderer, slots: &[InlineSlot]) {
        let live: HashSet<&str> = slots.iter().map(|s| s.key.as_str()).collect();
        self.uploaded.retain(|k, _| {
            let keep = live.contains(k.as_str());
            if !keep {
                g.drop_image(k);
            }
            keep
        });
        for slot in slots {
            if !self.uploaded.contains_key(&slot.key) {
                self.uploaded.insert(slot.key.clone(), upload(g, slot));
            }
            if let Some(Some(dims)) = self.uploaded.get(&slot.key).copied() {
                queue(g, slot, dims);
            }
        }
    }
}

/// 화면 밖 캡처 한 번 몫 — 위 캐시를 안 거치고 올려 그린다. 캐시는 「이번 프레임에 없는
/// 키는 놓는다」라서 여기서 건드리면 본 화면 텍스처가 지워진다. 올린 텍스처는 부른 쪽이
/// 캡처 뒤 키 머리로 놓는다.
pub fn paint_once(g: &mut GridRenderer, slots: &[InlineSlot]) {
    for slot in slots {
        if let Some(dims) = upload(g, slot) {
            queue(g, slot, dims);
        }
    }
}

fn upload(
    g: &mut GridRenderer,
    slot: &InlineSlot,
) -> Option<(u32, u32)> {
    let img = image::load_from_memory(&std::fs::read(&slot.path).ok()?).ok()?;
    let rgba = img.to_rgba8();
    let (iw, ih) = rgba.dimensions();
    g.upload_image(&slot.key, &rgba, iw, ih);
    Some((iw, ih))
}

fn queue(
    g: &mut GridRenderer,
    slot: &InlineSlot,
    (iw, ih): (u32, u32),
) {
    let key = &slot.key;
    let (x, y, w, h) = slot.rect;
    let (cx, cy, cw, ch) = slot.clip;
    g.push_clip(cx, cy, cw, ch);
    match slot.fit {
        InlineFit::Contain => g.queue_image_contain(key, x, y, w, h),
        InlineFit::Native => g.queue_image(key, x, y, w, h, 1.0, 0.0, 0.0),
        InlineFit::Hug => {
            // no-upscale 캡이 있어 그림이 박스보다 작으면 원본 크기로 그려진다 —
            // 좁힐 폭도 그 실제 크기를 넘지 않아야 왼쪽에 붙는다.
            let bw = if ih > 0 { (h * iw as f32 / ih as f32).min(iw as f32).min(w) } else { w };
            g.queue_image(key, x, y, bw, h, 1.0, 0.0, 0.0);
        }
    }
    g.pop_clip();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_slot_keeps_kitty_box_and_clips_to_the_visible_tile() {
        use kasa_screen::screen::{CellClip, InlineImageView};
        // 4칸×2줄 얼굴의 윗줄이 칸 위로 밀려 나가고 아랫줄만 첫 줄에 남은 자리.
        let face = InlineImageView {
            id: 7,
            path: "/tmp/face.png".into(),
            row: -1,
            col: 2,
            cols: 4,
            rows: 2,
            clip: Some(CellClip { row: 0, col: 2, cols: 4, rows: 1 }),
        };
        let s = inline_slot(&face, "k".into(), (10.0, 20.0), (8.0, 16.0), 24, 0);
        assert_eq!(s.rect, (26.0, 4.0, 32.0, 32.0));
        assert_eq!(s.clip, (26.0, 20.0, 32.0, 16.0));
        assert_eq!(s.fit, InlineFit::Contain);
        let shifted = inline_slot(&face, "k".into(), (10.0, 20.0), (8.0, 16.0), 24, 3);
        assert_eq!((shifted.rect.1, shifted.clip.1), (4.0 + 48.0, 20.0 + 48.0));
        let osc = InlineImageView { row: 22, clip: None, ..face };
        let s = inline_slot(&osc, "o".into(), (10.0, 20.0), (8.0, 16.0), 24, 0);
        assert_eq!(s.clip, (26.0, 20.0, 32.0, 24.0 * 16.0));
        assert_eq!(s.fit, InlineFit::Native);
    }

}
