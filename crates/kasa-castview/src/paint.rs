//! 보기 그리기 — 새로 풀린 장을 올리고 칸 본문 안에 비율을 지켜 그린 뒤, 그 위에 「조작」·HUD·키 메뉴를 얹는다. 글자는 부르는
//! 쪽 글꼴로 그린다([`Canvas`]) — 본 앱은 크롬 글꼴, 보기 앱은 격자 글꼴. 나머지(사각형·그림·클립)는 `GridRenderer` 그대로다.

use kasa_gridview::GridRenderer;

use crate::{contains, Layout, Rect, ScreenView, Surface};

const STATUS_FONT: f32 = 13.0;
const HUD_FONT: f32 = 11.0;
const HUD_PAD: f32 = 6.0;
const EDGE: f32 = 2.0;
const KEY_LABEL: &str = "키";
const WHITE: [u8; 4] = [255, 255, 255, 235];
const DIM: [u8; 4] = [0, 0, 0, 150];

/// 그릴 판 — 격자 렌더러와 그 위의 글자.
pub trait Canvas {
    fn grid(&mut self) -> &mut GridRenderer;
    /// 글 폭(LOGICAL px).
    fn measure(&mut self, text: &str, size: f32) -> f32;
    fn text(&mut self, x: f32, y: f32, text: &str, size: f32, color: [u8; 4]);
}

/// 크롬 글꼴이 따로 없는 호스트(보기 앱)는 격자 글꼴로 그린다.
impl Canvas for GridRenderer {
    fn grid(&mut self) -> &mut GridRenderer {
        self
    }
    fn measure(&mut self, text: &str, size: f32) -> f32 {
        self.measure_label(text, size)
    }
    fn text(&mut self, x: f32, y: f32, text: &str, size: f32, color: [u8; 4]) {
        self.draw_label(x, y, text, size, color);
    }
}

/// 부르는 쪽 색과 덧붙일 단추.
pub struct Style<'a> {
    /// 켠 단추(「조작 중」·열린 「키」)와 키 메뉴의 고른 줄.
    pub accent: [u8; 4],
    /// 장이 오기 전 상태 글.
    pub mute: [u8; 4],
    /// 장 둘레 테 — 그 기기 색이라 이 기기 화면과 다른 기기 화면이 한눈에 갈린다.
    pub edge: [u8; 4],
    /// HUD 왼쪽 끝에 더 얹을 단추 글(보기 앱의 「기기」·「전체 화면」). 그린 자리는 `ScreenView::extra` 에 같은 차례로 남는다.
    pub extra: &'a [&'a str],
}

/// 장이 흐르면 오른쪽 위에 「조작」 단추와 `fps · 지연 · 길` 을 작게 얹고, 확대했으면 그 왼쪽에 배율(누르면 맞춤으로)을 둔다.
/// 아직 장이 없으면 상태 글을 가운데에. 장은 그 기기의 기기색 테로 두른다. 조작은 열면 늘 켜져 있어 테 색으로 가르지 않고
/// 「조작 중」 알약이 말한다. 조작을 못 넣으면 그 까닭을 아래에 띄운다. 그린 자리는 누름 판정에 쓰도록 보기에 적어 둔다.
pub fn paint<C: Canvas>(c: &mut C, view: &mut ScreenView, key: &str, slot: Rect, on: Surface, cursor: (f32, f32), style: &Style<'_>) {
    let (x, y, w, h) = slot;
    view.surface = on;
    upload(c.grid(), view, key);
    let box_h = HUD_FONT + HUD_PAD * 1.5;
    let chip = |c: &mut C, right: f32, text: &str, color: [u8; 4], fill: [u8; 4]| -> Rect {
        let cw = c.measure(text, HUD_FONT) + HUD_PAD * 2.0;
        let r = (right - HUD_PAD - cw, y + HUD_PAD, cw, box_h);
        c.grid().round_rect_fill(r.0, r.1, r.2, r.3, 4.0, fill);
        c.text(r.0 + HUD_PAD, r.1 + HUD_PAD * 0.75, text, HUD_FONT, color);
        r
    };
    if !view.has_frame() {
        view.layout = None;
        view.audio.pill = None;
        c.grid().round_rect_stroke(x, y, w, h, 0.0, EDGE, style.edge);
        let text = view.status();
        let tw = c.measure(&text, STATUS_FONT);
        c.text(x + (w - tw) / 2.0, y + (h - STATUS_FONT) / 2.0, &text, STATUS_FONT, style.mute);
        let mut right = x + w;
        view.extra = style.extra.iter().rev().map(|t| {
            let r = chip(c, right, t, WHITE, DIM);
            right = r.0;
            r
        }).collect();
        view.extra.reverse();
        return;
    }
    let frame = view.frame_size();
    let (image, uv) = crate::zoomed(slot, frame, view.zoom, view.focus);
    let g = c.grid();
    g.push_clip(x, y, w, h);
    g.queue_image_region(key, image, uv);
    g.pop_clip();
    g.round_rect_stroke(image.0, image.1, image.2, image.3, 0.0, EDGE, style.edge);
    let hud = view.hud();
    let hud_box = chip(c, x + w, &hud, WHITE, DIM);
    let label = if view.control { "조작 중" } else { "조작" };
    let toggle = chip(c, hud_box.0, label, WHITE, if view.control { style.accent } else { DIM });
    let key_button = view.control.then(|| chip(c, toggle.0, KEY_LABEL, WHITE, if view.key_menu.is_some() { style.accent } else { DIM }));
    if !view.control {
        view.key_menu = None;
    }
    let mut left_of = key_button.unwrap_or(toggle).0;
    let mut next = |c: &mut C, text: &str, color: [u8; 4]| {
        let r = chip(c, left_of, text, color, DIM);
        left_of = r.0;
        r
    };
    let display_chip = view.display_cycle().map(|(at, n, _)| next(c, &format!("화면 {at}/{n}"), WHITE));
    let zoom_chip = (view.zoom > 1.001).then(|| next(c, &format!("{:.0}%", view.zoom * 100.0), WHITE));
    let audio_text = view.audio.shown().then(|| (view.audio.text(), if view.audio.on { WHITE } else { [255, 255, 255, 150] }));
    view.audio.pill = audio_text.map(|(text, color)| next(c, text, color));
    let mut extra: Vec<Rect> = style.extra.iter().rev().map(|t| next(c, t, WHITE)).collect();
    extra.reverse();
    view.extra = extra;
    if view.control && !view.preedit.is_empty() {
        let pw = c.measure(&view.preedit, STATUS_FONT) + HUD_PAD * 2.0;
        let ph = STATUS_FONT + HUD_PAD * 1.5;
        let (px, py) = (image.0 + HUD_PAD, image.1 + image.3 - ph - HUD_PAD);
        c.grid().round_rect_fill(px, py, pw, ph, 4.0, DIM);
        let preedit = view.preedit.clone();
        c.text(px + HUD_PAD, py + HUD_PAD * 0.75, &preedit, STATUS_FONT, WHITE);
        c.grid().rect(px + HUD_PAD, py + ph - HUD_PAD * 0.5, pw - HUD_PAD * 2.0, 1.0, WHITE);
    }
    if let Some(note) = view.input_note().filter(|_| view.control) {
        let nw = c.measure(&note, HUD_FONT) + HUD_PAD * 2.0;
        let (nx, ny) = (x + (w - nw) / 2.0, y + h - box_h - HUD_PAD);
        c.grid().round_rect_fill(nx, ny, nw, box_h, 4.0, DIM);
        c.text(nx + HUD_PAD, ny + HUD_PAD * 0.75, &note, HUD_FONT, WHITE);
    }
    if let (Some(b), Some(_)) = (key_button, view.key_menu.as_ref()) {
        let keys = view.special_keys();
        let row_h = STATUS_FONT + HUD_PAD * 2.0;
        let mw = keys.iter().map(|(t, _, _)| c.measure(t, STATUS_FONT)).fold(0.0, f32::max) + HUD_PAD * 4.0;
        let (mx, my) = ((b.0 + b.2 - mw).max(x + HUD_PAD), b.1 + b.3 + 4.0);
        c.grid().round_rect_fill(mx, my, mw, row_h * keys.len() as f32 + HUD_PAD, 6.0, [0, 0, 0, 210]);
        let mut rows = Vec::with_capacity(keys.len());
        for (i, (text, _, _)) in keys.iter().enumerate() {
            let r = (mx, my + HUD_PAD / 2.0 + row_h * i as f32, mw, row_h);
            if contains(r, (f64::from(cursor.0), f64::from(cursor.1))) {
                c.grid().round_rect_fill(r.0 + 3.0, r.1, r.2 - 6.0, r.3, 4.0, style.accent);
            }
            c.text(r.0 + HUD_PAD * 2.0, r.1 + HUD_PAD, text, STATUS_FONT, WHITE);
            rows.push((r, i));
        }
        view.key_menu = Some(rows);
    }
    view.layout = Some(Layout { image, uv, slot, frame, toggle, zoom_chip, key_button, display_chip });
}

/// 새로 풀린 장을 올린다. 칸을 다른 창으로 옮기면 그 창 렌더러에는 장이 없다 — 멈춘 화면이면 새 장이 안 오니 마지막 장을
/// 다시 올린다. 복사 없이 디코더 표면(IOSurface·D3D11 공유 텍스처)을 텍스처로 삼고, 못 하면(다른 백엔드 등) CPU 로 한 번 올린다.
fn upload(g: &mut GridRenderer, view: &mut ScreenView, key: &str) {
    let fresh = view.new_surface().or_else(|| if g.has_image(key) { None } else { view.current_surface() });
    let Some((surface, surface_key, fw, fh)) = fresh else { return };
    let started = std::time::Instant::now();
    #[cfg(target_os = "macos")]
    // SAFETY: 표면은 `view.held` 가 쥔 디코더 장의 IOSurface 다 — GPU 가 그릴 동안 살아 있다.
    let zero_copy = {
        let _ = surface_key;
        unsafe { g.set_image_iosurface(key, surface, fw, fh) }
    };
    #[cfg(windows)]
    // SAFETY: 공유 핸들은 `view.held` 가 쥔 디코더 장의 D3D11 텍스처 것이다 — GPU 가 그릴 동안 살아 있다.
    let zero_copy = unsafe { g.set_image_d3d11(key, surface, surface_key, fw, fh) };
    #[cfg(not(any(target_os = "macos", windows)))]
    let zero_copy = {
        let _ = (surface, surface_key, fw, fh);
        false
    };
    if !zero_copy {
        view.copy_latest(|bgra, w, h, stride| g.update_image_bgra(key, bgra, w, h, stride));
    }
    view.note_upload(started.elapsed(), zero_copy);
}
