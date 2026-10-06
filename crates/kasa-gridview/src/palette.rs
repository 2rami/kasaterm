//! 셀 색을 화면 색으로 푸는 팔레트. 호스트가 테마에서 값을 채워 넘긴다 — 엔진은 테마를 모른다.

use kasa_screen::{Cell, Color};

/// 선택 영역 띠. 커서와 달리 테마를 따르지 않는 차분한 파랑.
pub const SELECTION: [u8; 4] = [49, 99, 139, 0x99];
/// 인라인 자동완성(유령 글자). 확정된 글자 뒤로 물러나 보이는 흐린 회청색 — fish/zsh 식.
pub const GHOST_FG: [u8; 4] = [120, 132, 148, 0xff];

/// 셀 색을 풀 때 쓰는 값 전부. 프레임마다 테마에서 새로 채워도 될 만큼 작다.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    /// ANSI 0~15. 16~231 은 xterm 6×6×6 큐브, 232~255 는 회색 24단으로 계산한다.
    pub ansi16: [[u8; 3]; 16],
    /// `Color::Default` 전경.
    pub fg: [u8; 4],
    /// `Color::Default` 배경이자 칸 바탕.
    pub bg: [u8; 4],
    pub cursor: [u8; 4],
    /// 셀이 스스로 고른 색(256 큐브·truecolor)에만 거는 WCAG 대비 하한. 1 = 끔.
    pub min_contrast: f32,
}

impl Default for Palette {
    /// 본판 어두운 기본 테마(Tomorrow Night 계열 ANSI).
    fn default() -> Self {
        Self {
            ansi16: [
                [0x1D, 0x1F, 0x21], [0xCC, 0x66, 0x66], [0xB5, 0xBD, 0x68], [0xF0, 0xC6, 0x74],
                [0x81, 0xA2, 0xBE], [0xB2, 0x94, 0xBB], [0x8A, 0xBE, 0xB7], [0xC5, 0xC8, 0xC6],
                [0x66, 0x66, 0x66], [0xD5, 0x4E, 0x53], [0xB9, 0xCA, 0x4A], [0xE7, 0xC5, 0x47],
                [0x7A, 0xA6, 0xDA], [0xC3, 0x97, 0xD8], [0x70, 0xC0, 0xB1], [0xEA, 0xEA, 0xEA],
            ],
            fg: [255, 255, 255, 255],
            bg: [37, 44, 53, 255],
            cursor: [90, 140, 230, 255],
            min_contrast: 2.5,
        }
    }
}

impl Palette {
    /// ANSI 256색 표. 0~15 만 팔레트에서, 나머지는 계산한다(표를 안 만든다).
    pub fn ansi_color(&self, i: u8) -> [u8; 3] {
        match i {
            0..=15 => self.ansi16[i as usize],
            16..=231 => {
                let steps = [0u8, 95, 135, 175, 215, 255];
                let n = i as usize - 16;
                [steps[n / 36], steps[(n / 6) % 6], steps[n % 6]]
            }
            _ => {
                let v = 8 + (i - 232) * 10;
                [v, v, v]
            }
        }
    }

    pub fn color_to_rgba(&self, c: &Color, default: [u8; 4]) -> [u8; 4] {
        match c {
            Color::Default => default,
            Color::Idx(i) => {
                let p = self.ansi_color(*i);
                [p[0], p[1], p[2], 0xff]
            }
            Color::Rgb(r, g, b) => [*r, *g, *b, 0xff],
        }
    }

    /// 셀 전경색 — tmux `window-style fg=<색>` 등가 칸 틴트 지원. `dfg` 가 이 칸의
    /// 「기본 전경색」: 기본 전경을 쓰는 셀만 이 색이 되고, 명시 색(ANSI 16/256/
    /// truecolor) 셀은 그대로다. inverse 셀의 dim 혼합에 쓰이는 fg 참조도 같은 규칙 —
    /// tmux 와 같이 reverse 도 틴트를 따른다. 틴트 없는 칸은 `self.fg` 를 넘긴다.
    pub fn cell_fg_with(&self, cell: &Cell, dfg: [u8; 4]) -> [u8; 4] {
        let mut fg = self.color_to_rgba(&cell.fg, dfg);
        if cell.inverse {
            fg = self.color_to_rgba(&cell.bg, self.bg);
        }
        // SGR 2(흐림). Claude Code 가 자동완성 유령 글자에 쓴다 — 없으면 제안이 확정 입력처럼
        // 읽힌다. 바탕 쪽으로 55% 섞어 읽히되 뒤로 물러나게.
        if cell.dim {
            let bg = if cell.inverse {
                self.color_to_rgba(&cell.fg, dfg)
            } else {
                self.color_to_rgba(&cell.bg, self.bg)
            };
            let t = 0.55_f32;
            for i in 0..3 {
                fg[i] = (fg[i] as f32 * (1.0 - t) + bg[i] as f32 * t).round() as u8;
            }
            // 흐림은 물러나겠다는 뜻이다. 대비 하한으로 다시 끌어올리면 앱이 그은 구분이 지워진다.
            return fg;
        }
        if names_own_color(if cell.inverse { &cell.bg } else { &cell.fg }) {
            fg = enforce_contrast_at(fg, self.cell_bg_with(cell, dfg), self.min_contrast);
        }
        fg
    }

    /// 셀 배경색 — bg 자체는 틴트를 안 받지만, inverse 셀의 배경 채움은 정의상 fg 색이므로
    /// 기본 전경 셀이면 칸 틴트 `dfg` 를 따른다(claude 블록 커서 = inverse 공백).
    pub fn cell_bg_with(&self, cell: &Cell, dfg: [u8; 4]) -> [u8; 4] {
        let mut bg = self.color_to_rgba(&cell.bg, self.bg);
        if cell.inverse {
            bg = self.color_to_rgba(&cell.fg, dfg);
        }
        bg
    }
}

/// 셀이 테마가 정하는 색을 물려받지 않고 스스로 고른 색인지. 그런 색만 대비 하한을 건다 —
/// `Default` 는 테마의 전경이고 ANSI 0~15 는 팔레트마다 다시 칠해지므로 이미 읽히게 정해져 있다.
fn names_own_color(c: &Color) -> bool {
    match c {
        Color::Default => false,
        Color::Idx(i) => *i >= 16,
        Color::Rgb(..) => true,
    }
}

/// sRGB 바이트 → 선형, 캐시. 대비 검사는 셀마다 돌아서, 색 있는 글자마다 여섯 채널에 `powf` 를
/// 거는 것은 256개짜리 답표에 비해 진짜 일이다.
fn srgb_lut() -> &'static [f32; 256] {
    static LUT: std::sync::OnceLock<[f32; 256]> = std::sync::OnceLock::new();
    LUT.get_or_init(|| {
        let mut t = [0.0f32; 256];
        for (i, v) in t.iter_mut().enumerate() {
            let c = i as f32 / 255.0;
            *v = if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
        }
        t
    })
}

pub fn luminance(c: [u8; 4]) -> f32 {
    let l = srgb_lut();
    0.2126 * l[c[0] as usize] + 0.7152 * l[c[1] as usize] + 0.0722 * l[c[2] as usize]
}

pub fn contrast_of(a: f32, b: f32) -> f32 {
    let (hi, lo) = if a > b { (a, b) } else { (b, a) };
    (hi + 0.05) / (lo + 0.05)
}

/// `fg` 를 바탕이 아닌 쪽(검정이나 흰색)으로 밀어 대비 하한 `min` 을 넘게 한다. 색상은 다시 계산하지
/// 않고 섞임을 따라가므로, 빛바랜 주황은 회색 글자가 아니라 짙은 주황이 된다.
pub fn enforce_contrast_at(fg: [u8; 4], bg: [u8; 4], min: f32) -> [u8; 4] {
    if min <= 1.0 {
        return fg;
    }
    let l_bg = luminance(bg);
    if contrast_of(luminance(fg), l_bg) >= min {
        return fg;
    }
    let target = if l_bg > 0.18 { 0.0f32 } else { 255.0 };
    let mix = |t: f32| {
        let mut c = fg;
        for i in 0..3 {
            c[i] = (fg[i] as f32 + (target - fg[i] as f32) * t).round() as u8;
        }
        c
    };
    // 하한에 못 닿을 수 있다(중간 회색 바탕은 어느 쪽으로도 멀리 못 간다). 끝없이 쫓지 않고
    // 찾은 가장 가까운 값에서 멈춘다.
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..8 {
        let mid = (lo + hi) * 0.5;
        if contrast_of(luminance(mix(mid)), l_bg) >= min {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    mix(hi)
}

/// 거울 칸의 **원본 기기** 팔레트(그쪽 `/design-tokens` 의 `palette.bg`·`fg`).
///
/// 원본에서 도는 claude 는 그 기기 밝기의 테마로 색을 **절대값(truecolor)** 으로 찍는다 —
/// 예약 메시지 칩 배경 `rgb(240,240,240)`, 글자 `rgb(76,79,105)` 처럼. 원본 화면에선 그
/// 색이 칸 배경과 거의 같아 칩이 묻히는데, 바이트를 그대로 받는 어두운 거울에선 흰
/// 덩어리가 된다(2026-09-17 실측, catppuccin-latte 원본 → 어두운 거울). 원본이 「내
/// 배경/글자색」이라고 찍은 색만 기본색으로 되돌려 보는 쪽 팔레트를 따르게 한다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourcePalette {
    pub bg: [u8; 3],
    pub fg: [u8; 3],
}

/// 「원본 배경과 같은 색」으로 볼 채널당 오차. claude 라이트 칩 240 vs latte 배경
/// 239·241·245 처럼 테마가 조금 어긋난 값을 잡되, diff 블록(220,255,220) 같은
/// 의미색은 건드리지 않는 폭.
pub const SOURCE_MATCH_TOLERANCE: u8 = 16;

fn near(c: &Color, target: [u8; 3]) -> bool {
    match c {
        Color::Rgb(r, g, b) => [*r, *g, *b]
            .iter()
            .zip(target.iter())
            .all(|(a, b)| a.abs_diff(*b) <= SOURCE_MATCH_TOLERANCE),
        _ => false,
    }
}

/// 원본 팔레트에 맞춰 찍힌 명시색을 보는 쪽 기본색으로. 바꿀 게 없으면 빌려만 준다(셀당
/// 복제 0). `source` 가 없는 칸(로컬·팔레트 미수신)은 그대로다.
pub fn adapt_to_viewer<'c>(
    cell: &'c Cell,
    source: Option<&SourcePalette>,
) -> std::borrow::Cow<'c, Cell> {
    let Some(sp) = source else { return std::borrow::Cow::Borrowed(cell) };
    let bg_hit = near(&cell.bg, sp.bg);
    let fg_hit = near(&cell.fg, sp.fg);
    if !bg_hit && !fg_hit {
        return std::borrow::Cow::Borrowed(cell);
    }
    let mut out = cell.clone();
    if bg_hit {
        out.bg = Color::Default;
    }
    if fg_hit {
        out.fg = Color::Default;
    }
    std::borrow::Cow::Owned(out)
}
#[cfg(test)]
mod source_palette_tests {
    use super::*;

    fn cell(fg: Color, bg: Color) -> Cell {
        Cell { fg, bg, ..Cell::blank() }
    }

    const LATTE: SourcePalette = SourcePalette { bg: [0xef, 0xf1, 0xf5], fg: [0x4c, 0x4f, 0x69] };

    #[test]
    fn queued_chip_from_light_host_falls_back_to_viewer_colors() {
        // 실측 바이트: ESC[0;38;2;76;79;105;48;2;240;240;240m — 원본(latte) 배경·글자와 오차 안.
        let c = cell(Color::Rgb(76, 79, 105), Color::Rgb(240, 240, 240));
        let out = adapt_to_viewer(&c, Some(&LATTE));
        assert!(matches!(out, std::borrow::Cow::Owned(_)));
        assert_eq!(out.bg, Color::Default);
        assert_eq!(out.fg, Color::Default);
    }

    #[test]
    fn semantic_colors_and_indexed_colors_are_left_alone() {
        // diff 블록 배경(연두)·ANSI 색·원본과 먼 회색은 의미색이라 그대로.
        for c in [
            cell(Color::Default, Color::Rgb(220, 255, 220)),
            cell(Color::Idx(7), Color::Idx(0)),
            cell(Color::Rgb(140, 143, 161), Color::Rgb(200, 200, 200)),
        ] {
            let out = adapt_to_viewer(&c, Some(&LATTE));
            assert!(matches!(out, std::borrow::Cow::Borrowed(_)), "{c:?}");
            assert_eq!(*out, c);
        }
    }

    #[test]
    fn local_panes_are_untouched() {
        let c = cell(Color::Rgb(76, 79, 105), Color::Rgb(240, 240, 240));
        assert!(matches!(adapt_to_viewer(&c, None), std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn only_the_matching_side_is_replaced() {
        let c = cell(Color::Rgb(215, 119, 87), Color::Rgb(239, 241, 245));
        let out = adapt_to_viewer(&c, Some(&LATTE));
        assert_eq!(out.bg, Color::Default);
        assert_eq!(out.fg, Color::Rgb(215, 119, 87));
    }
}
