//! 칸 안 결과 카드(`[[ki:]]` 그림·`[[km:]]` 음성·영상·PDF·웹·파일)의 가림. 그림·영상·PDF·파일은 가린 채, 음성·웹은
//! 보인 채 서고([`note_kind`]), 눈 단추·띠로 이 실행 동안 뒤집는다(같은 카드는 어느 칸·창에서든 같이 바뀐다).
//!
//! 가려도 카드가 잡은 줄은 그대로다 — 그 줄의 주인은 칸의 프로그램(Claude Code·Codex)이라 앱이 줄 수를 못 바꾼다.
//! 가린 카드는 보이는 부분 맨 위에 머리 띠 하나만 그린다. 띠 어디를 눌러도 보이고, 보이는 카드는 눈만 눌러 가린다.

use crate::{gpu, theme};
use kasa_gridview::images::InlineSlot;
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::window::WindowId;

type Rect = (f32, f32, f32, f32);

pub(crate) const BAR_H: f32 = 44.0;
const EYE: f32 = 28.0;
/// 가린 그림 띠의 최소 폭 — 세로로 긴 그림은 상자가 좁아 글자가 안 들어간다.
const BAR_MIN_W: f32 = 200.0;

/// 처음 모습에서 뒤집은 카드.
fn flipped_set() -> &'static Mutex<HashSet<String>> {
    static FLIPPED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    FLIPPED.get_or_init(Default::default)
}

/// 처음부터 보이는 카드 — 음성·웹. 가릴 큰 그림이 없어 가리면 누름만 한 번 늘었다. 종류는 카드 정보를 받아야 알아,
/// 받기 전에는 가린 띠(「불러오는 중」)로 선다.
fn open_set() -> &'static Mutex<HashSet<String>> {
    static OPEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    OPEN.get_or_init(Default::default)
}

/// 미디어 카드의 종류를 알린다 — 음성·웹이면 처음부터 보인다.
pub(crate) fn note_kind(key: &str, kind: &str) {
    if matches!(kind, "audio" | "web") {
        if let Ok(mut open) = open_set().lock() {
            if !open.contains(key) {
                open.insert(key.to_owned());
            }
        }
    }
}

/// 창마다 이번 프레임에 그린 누름 자리(카드 열쇠, 자리).
fn targets() -> &'static Mutex<HashMap<WindowId, Vec<(String, Rect)>>> {
    static TARGETS: OnceLock<Mutex<HashMap<WindowId, Vec<(String, Rect)>>>> = OnceLock::new();
    TARGETS.get_or_init(Default::default)
}

fn pressed() -> &'static Mutex<Option<(WindowId, String)>> {
    static PRESSED: OnceLock<Mutex<Option<(WindowId, String)>>> = OnceLock::new();
    PRESSED.get_or_init(Default::default)
}

pub(crate) fn is_card(key: &str) -> bool {
    key.starts_with("result:") || key.starts_with("media:")
}

pub(crate) fn shown(key: &str) -> bool {
    let open = open_set().lock().is_ok_and(|s| s.contains(key));
    open != flipped_set().lock().is_ok_and(|s| s.contains(key))
}

fn toggle(key: &str) {
    if let Ok(mut s) = flipped_set().lock() {
        if !s.remove(key) {
            s.insert(key.to_owned());
        }
    }
}

/// 그림 칠하기·미리보기·움직이는 그림에 넘길 칸 그림 — 가린 결과 그림을 뺀다(띠는 [`paint`] 가 그린다).
pub(crate) fn visible_images(slots: &[InlineSlot]) -> Vec<InlineSlot> {
    slots.iter().filter(|s| !s.key.starts_with("result:") || shown(&s.key)).cloned().collect()
}

fn visible(slot: &InlineSlot) -> Rect {
    crate::inline_media::intersection(slot.rect, slot.clip)
}

/// 가린 카드의 머리 띠 — 보이는 부분의 맨 위. 위가 스크롤로 잘려도 띠는 보이는 첫 줄에 선다.
pub(crate) fn bar(slot: &InlineSlot) -> Rect {
    let v = visible(slot);
    let w = if slot.key.starts_with("result:") { slot.rect.2.max(BAR_MIN_W) } else { v.2 };
    (v.0, v.1, w, BAR_H.min(v.3))
}

/// 눈 단추 자리. 포스터 없는 낮은 카드(음성 등)는 아래 단추 줄 오른쪽 끝, 나머지는 보이는 부분의 오른쪽 위.
pub(crate) fn eye(slot: &InlineSlot, shown: bool) -> Rect {
    if !shown {
        let b = bar(slot);
        return (b.0 + b.2 - EYE - 8.0, b.1 + (b.3 - EYE) / 2.0, EYE, EYE);
    }
    let (x, y, w, h) = slot.rect;
    if slot.key.starts_with("media:") && h <= 100.0 {
        let line_y = (y + h - 52.0).max(y + 8.0);
        return (x + w - EYE - 8.0, line_y + 4.0 + (32.0 - EYE) / 2.0, EYE, EYE);
    }
    let v = visible(slot);
    (x + w - EYE - 8.0, v.1 + 8.0, EYE, EYE)
}

/// 모든 카드 위에 눈 단추를, 가린 그림 카드엔 머리 띠를 그린다. 음성·영상 카드의 띠는 `inline_media::paint` 몫이다.
pub(crate) fn paint(g: &mut gpu::GpuRenderer, owner: WindowId, slots: &[InlineSlot], cursor: (f32, f32)) {
    let mut drawn = Vec::new();
    for slot in slots.iter().filter(|s| is_card(&s.key)) {
        let v = visible(slot);
        if v.2 <= 0.0 || v.3 <= 0.0 {
            continue;
        }
        let shown = shown(&slot.key);
        let b = bar(slot);
        g.push_clip(v.0, v.1, b.2.max(v.2), v.3);
        if !shown && slot.key.starts_with("result:") {
            crate::native_controls::panel(g, b, theme::surface());
            g.queue_icon("image", b.0 + 14.0, b.1 + (b.3 - 16.0) / 2.0, 16.0, theme::text_dim());
            crate::inline_media::text(g, (b.0 + 40.0, b.1 + (b.3 - 20.0) / 2.0 + 3.0, (b.2 - 40.0 - EYE - 16.0).max(0.0), 20.0), "그림 · 가려짐", true);
        }
        let e = eye(slot, shown);
        // 그림 위에서도 보이게 바탕을 깐다.
        g.round_rect_fill(e.0, e.1, e.2, e.3, 6.0, theme::with_alpha(theme::surface(), 230));
        crate::native_controls::icon_button(g, e, cursor, if shown { "eye" } else { "eye-off" }, Default::default());
        g.pop_clip();
        let target = if shown { e } else { b };
        let target = crate::inline_media::intersection(target, (v.0, v.1, b.2.max(v.2), v.3));
        if target.2 > 0.0 && target.3 > 0.0 {
            drawn.push((slot.key.clone(), target));
        }
    }
    if let Ok(mut t) = targets().lock() {
        t.insert(owner, drawn);
    }
}

fn hit(owner: WindowId, point: (f32, f32)) -> Option<String> {
    let t = targets().lock().ok()?;
    t.get(&owner)?.iter().rev().find(|(_, r)| contains(*r, point)).map(|(k, _)| k.clone())
}

fn contains(r: Rect, p: (f32, f32)) -> bool {
    p.0 >= r.0 && p.0 <= r.0 + r.2 && p.1 >= r.1 && p.1 <= r.1 + r.3
}

/// 눈·띠 누름이면 삼키고 참. 누른 자리에서 놓아야 바뀐다 — 끌기 시작점이 우연히 눈 위여도 선택은 그대로 간다.
pub(crate) fn event(owner: WindowId, point: (f32, f32), event: &WindowEvent) -> bool {
    let WindowEvent::MouseInput { state, button: MouseButton::Left, .. } = event else {
        return false;
    };
    let Ok(mut held) = pressed().lock() else { return false };
    match state {
        ElementState::Pressed => match hit(owner, point) {
            Some(key) => {
                *held = Some((owner, key));
                true
            }
            None => false,
        },
        ElementState::Released => match held.take() {
            Some((w, key)) if w == owner => {
                if hit(owner, point).as_deref() == Some(key.as_str()) {
                    toggle(&key);
                }
                true
            }
            other => {
                *held = other;
                false
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kasa_gridview::images::InlineFit;

    fn slot(key: &str, rect: Rect, clip: Rect) -> InlineSlot {
        InlineSlot { key: key.into(), path: String::new(), rect, clip, fit: InlineFit::Contain, below: false, previewable: true }
    }

    #[test]
    fn cards_start_hidden_and_toggle_per_key() {
        let a = slot("result:aaaaaaaaaaaaaaaa", (0.0, 0.0, 100.0, 300.0), (0.0, 0.0, 100.0, 300.0));
        let kitty = slot("kitty:7", (0.0, 0.0, 10.0, 10.0), (0.0, 0.0, 10.0, 10.0));
        assert!(!shown(&a.key));
        assert_eq!(visible_images(&[a.clone(), kitty.clone()]).iter().map(|s| s.key.as_str()).collect::<Vec<_>>(), ["kitty:7"], "카드 아닌 그림은 그대로");
        toggle(&a.key);
        assert!(shown(&a.key));
        assert_eq!(visible_images(std::slice::from_ref(&a)).len(), 1);
        toggle(&a.key);
        assert!(!shown(&a.key));
    }

    #[test]
    fn audio_and_web_cards_start_shown_and_others_stay_hidden() {
        for (key, kind, open) in [("media:k:audio0000000000000", "audio", true), ("media:k:web00000000000000", "web", true),
            ("media:k:video000000000000", "video", false), ("media:k:pdf00000000000000", "pdf", false)] {
            note_kind(key, kind);
            assert_eq!(shown(key), open, "{kind}");
            toggle(key);
            assert_eq!(shown(key), !open, "{kind} 은 눈으로 뒤집힌다");
        }
    }

    #[test]
    fn hidden_bars_sit_on_the_first_visible_row_and_short_cards_keep_the_eye_off_the_controls() {
        let scrolled = slot("result:bbbbbbbbbbbbbbbb", (10.0, -100.0, 120.0, 400.0), (10.0, 40.0, 120.0, 200.0));
        assert_eq!(bar(&scrolled), (10.0, 40.0, BAR_MIN_W, BAR_H));
        let e = eye(&scrolled, false);
        assert!(e.1 >= 40.0 && e.0 + e.2 <= 10.0 + BAR_MIN_W);
        let audio = slot("media:x:cccccccccccccccc", (0.0, 0.0, 400.0, 72.0), (0.0, 0.0, 400.0, 72.0));
        let e = eye(&audio, true);
        assert_eq!((e.0 + e.2, e.1), (392.0, 24.0 + 2.0), "단추 줄 높이, 오른쪽 끝");
        let video = slot("media:x:dddddddddddddddd", (0.0, 0.0, 400.0, 216.0), (0.0, 0.0, 400.0, 216.0));
        assert_eq!(eye(&video, true).1, 8.0, "포스터 카드는 오른쪽 위");
    }
}
