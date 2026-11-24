//! 앱 안 확인 카드 한 모양 — 기기 간 이사·학생 교체·계정 전환·칸 닫기·문서 저장 확인이 함께 쓴다.
//!
//! 단추는 공용 `native_controls::text_button` 을 카드 판을 바탕으로 둔 설정 층(`theme::settings_layer`)
//! 에서 그린다. 카드마다 단추를 따로 칠하던 동안 설정 창의 단추 규칙(모양·물들임·주 동작 표시)이
//! 바뀌어도 카드는 옛 모양으로 남았다(2026-11-24 지시: 설정 개편을 정본으로 카드도 맞추기).

use crate::{gpu, native_controls as controls, theme};

type Rect = (f32, f32, f32, f32);

pub(crate) struct Card<'a, B> {
    pub(crate) title: &'a str,
    /// 설명 줄과 그 글자색.
    pub(crate) lines: &'a [(String, [u8; 4])],
    /// 오른쪽부터 놓을 단추.
    pub(crate) buttons: &'a [(&'a str, B, controls::Style)],
    /// Enter 가 누르는 단추 — 포커스 테로 보인다.
    pub(crate) focus: Option<B>,
    pub(crate) min_w: f32,
}

const PAD: f32 = 24.0;
const TITLE_Y: f32 = 28.0;
const BODY_Y: f32 = 56.0;
const LINE_H: f32 = 20.0;
/// 마지막 설명 줄과 단추 사이.
const BODY_GAP: f32 = 14.0;
const BOTTOM: f32 = 20.0;
const BUTTON_GAP: f32 = 8.0;

/// 설명 `lines` 줄 카드의 높이.
pub(crate) fn height(lines: usize) -> f32 {
    BODY_Y + LINE_H * lines as f32 + BODY_GAP + controls::CONTROL_HEIGHT + BOTTOM
}

pub(crate) fn paint<B: Copy + PartialEq>(
    g: &mut gpu::GpuRenderer,
    (win_w, win_h): (f32, f32),
    cursor: (f32, f32),
    card: &Card<'_, B>,
) -> Vec<(B, Rect)> {
    g.rect(0.0, 0.0, win_w, win_h, theme::with_alpha([0, 0, 0, 255], 0xB0));
    let btn_h = controls::CONTROL_HEIGHT;
    let widths: Vec<f32> = card
        .buttons
        .iter()
        .map(|(label, _, style)| controls::button_width(g, label, btn_h, style.primary || style.active))
        .collect();
    let buttons_w = widths.iter().sum::<f32>() + BUTTON_GAP * widths.len().saturating_sub(1) as f32;
    let title_w = g.measure_chrome_text(card.title, 15.0, true);
    let text_w = card
        .lines
        .iter()
        .map(|(line, _)| g.measure_chrome_text(line, 13.0, false))
        .fold(title_w, f32::max);
    // 잰 폭에 2px 를 더 둔다 — 반올림으로 1px 만 모자라도 `fit_text` 가 줄 끝을 「…」로 자른다.
    let card_w = (text_w.max(buttons_w) + PAD * 2.0 + 2.0).ceil().max(card.min_w).min((win_w - 48.0).max(1.0));
    let card_h = height(card.lines.len());
    let x0 = ((win_w - card_w) / 2.0).round();
    let y0 = ((win_h - card_h) / 2.0).round();
    let fill = theme::surface_active();
    crate::panel_rect_outlined(g, x0, y0, card_w, card_h, theme::radius_md(), fill);
    let inner = (card_w - PAD * 2.0).max(0.0);
    let title = crate::info::fit_text(g, card.title, inner, 15.0, true);
    g.draw_text(
        x0 + PAD,
        y0 + TITLE_Y,
        &title,
        gpu::DrawOpts { font_size: 15.0, color: theme::text(), bold: true, italic: false },
    );
    for (i, (line, color)) in card.lines.iter().enumerate() {
        let line = crate::info::fit_text(g, line, inner, 13.0, false);
        g.draw_text(
            x0 + PAD,
            y0 + BODY_Y + i as f32 * LINE_H,
            &line,
            gpu::DrawOpts { font_size: 13.0, color: *color, bold: false, italic: false },
        );
    }
    let _layer = theme::settings_layer(fill);
    let y = y0 + card_h - BOTTOM - btn_h;
    let mut right = x0 + card_w - PAD;
    card.buttons
        .iter()
        .zip(widths)
        .map(|(&(label, button, style), w)| {
            let x = right - w;
            right = x - BUTTON_GAP;
            let rect = controls::text_button(g, (x, y, w, btn_h), cursor, label, style);
            if card.focus == Some(button) {
                controls::focus_ring(g, rect);
            }
            (button, rect)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_buttons_sit_below_the_last_line_with_a_gap() {
        for lines in 0..4 {
            let last_line_bottom = super::BODY_Y + super::LINE_H * lines as f32;
            let buttons_top = super::height(lines) - super::BOTTOM - crate::native_controls::CONTROL_HEIGHT;
            assert!(buttons_top - last_line_bottom >= 14.0, "{lines} lines");
        }
    }
}
