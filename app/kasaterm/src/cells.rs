//! Convert tmux-bridge cell grids into sugarloaf draw calls.
//!
//! MVP path uses `Sugarloaf::rect` (per-cell bg) plus `Text::draw`
//! (per-cell glyph). That's one draw call per cell which is wasteful
//! compared with Rio's `Grid::write_row` batched pipeline — but it
//! proves the conversion logic before we invest in the bigger Grid
//! integration.
//!
//! Color conversion lives here too: tmux-bridge `Color::Idx(u8)`
//! indexes into a 256-entry ANSI palette derived from the iTerm2
//! Default Dark theme (matches what kasaterm has been using, so the
//! A/B comparison stays apples-to-apples).

use kasa_bridge::screen::{Cell, Color};

/// ANSI 256-color lookup. 0-15 come from the active theme (see
/// `theme::ansi16` — runtime-swappable so a theme switch recolors the
/// terminal body); 16-231 is the standard xterm 6×6×6 cube and 232-255
/// the 24-step grayscale ramp, both computed. O(1), no table build.
///
/// (Earlier we hardcoded a handful of 256-palette overrides to
///  reverse claude code's nearest-cube quantisation. Once we found
///  that the real cause was `TMUX` being set in the child env —
///  which made chalk fall back to ANSI-256 mode — and removed it,
///  claude code emits truecolor escapes directly and the standard
///  xterm 6×6×6 cube is correct again.)
fn ansi_color(i: u8) -> [u8; 3] {
    match i {
        0..=15 => crate::theme::ansi16(i as usize),
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

/// Default foreground / background when a cell has `Color::Default`.
/// Decoded from the user's Terminal.app `GitHub Dark Dimmed` profile
/// (the active Default Window Settings): bg `#252c35`, fg `#bbc6d1`.
// Default foreground = pure white, matching Ghostty's default
// (#FFFFFF). The old [187,198,209] came from the user's Terminal.app
// "GitHub Dark Dimmed" profile and read noticeably greyer than
// Ghostty's body text.
#[inline]
pub fn default_fg() -> [u8; 4] {
    crate::theme::fg()
}
/// Terminal body background — the single source is the theme token so chrome
/// and body share one palette.
#[inline]
pub fn default_bg() -> [u8; 4] {
    crate::theme::bg()
}

/// Cursor + selection accents. Cursor uses the shared accent so it matches the
/// focus ring / links across the whole UI; selection is a muted blue.
#[inline]
pub fn iterm_cursor() -> [u8; 4] {
    crate::theme::accent()
}
pub const ITERM_SELECTION: [u8; 4] = [49, 99, 139, 0x99];
/// Inline-autosuggestion ghost text. A dim, low-contrast grey-blue that
/// sits clearly behind committed foreground text — fish/zsh style.
pub const GHOST_FG: [u8; 4] = [120, 132, 148, 0xff];

fn color_to_rgba(c: &Color, default: [u8; 4]) -> [u8; 4] {
    match c {
        Color::Default => default,
        Color::Idx(i) => {
            let p = ansi_color(*i);
            [p[0], p[1], p[2], 0xff]
        }
        Color::Rgb(r, g, b) => [*r, *g, *b, 0xff],
    }
}


/// 셀 전경색 — tmux `window-style fg=<색>` 등가 pane 틴트 지원. `dfg` 가 이 pane 의
/// "기본 전경색": 테마 default fg 를 쓰는 셀만 이 색이 되고, 명시 색(ANSI 16/256/
/// truecolor) 셀은 그대로다(`color_to_rgba` 의 Default 분기만 타므로). inverse 셀의
/// dim 혼합에 쓰이는 fg 참조도 같은 규칙 — tmux 와 동일하게 reverse 도 틴트를 따른다.
/// 무틴트 pane 은 `default_fg()` 를 넘긴다.
pub fn cell_fg_with(cell: &Cell, dfg: [u8; 4]) -> [u8; 4] {
    let mut fg = color_to_rgba(&cell.fg, dfg);
    if cell.inverse {
        fg = color_to_rgba(&cell.bg, default_bg());
    }
    // SGR 2 (faint). Claude Code uses this for ghost-text autosuggestions;
    // without it the suggestion reads as committed input. Mix toward bg by
    // ~55% so the glyph stays legible but visibly secondary.
    if cell.dim {
        let bg = if cell.inverse {
            color_to_rgba(&cell.fg, dfg)
        } else {
            color_to_rgba(&cell.bg, default_bg())
        };
        let t = 0.55_f32;
        for i in 0..3 {
            fg[i] = (fg[i] as f32 * (1.0 - t) + bg[i] as f32 * t).round() as u8;
        }
        // Faint is a deliberate request to recede. Lifting it back to a
        // contrast floor would erase the distinction the app just drew.
        return fg;
    }
    if names_own_color(if cell.inverse { &cell.bg } else { &cell.fg }) {
        fg = crate::theme::enforce_min_contrast(fg, cell_bg_with(cell, dfg));
    }
    fg
}

/// Whether the cell picked this color itself rather than inheriting one the
/// theme controls. Only those need the contrast guard: `Default` is the theme's
/// own fg, and ANSI 0-15 are remapped per palette, so both are already legible
/// by construction — running the guard on them would second-guess the theme.
fn names_own_color(c: &Color) -> bool {
    match c {
        Color::Default => false,
        Color::Idx(i) => *i >= 16,
        Color::Rgb(..) => true,
    }
}

/// 셀 배경색 — bg 자체는 무틴트(스펙)지만, inverse 셀의 배경 채움은 정의상 fg 색이므로
/// default-fg 셀이면 pane 틴트 `dfg` 를 따른다(claude 블록커서 = inverse 공백).
pub fn cell_bg_with(cell: &Cell, dfg: [u8; 4]) -> [u8; 4] {
    let mut bg = color_to_rgba(&cell.bg, default_bg());
    if cell.inverse {
        bg = color_to_rgba(&cell.fg, dfg);
    }
    bg
}

/// Draw one row of cells. `origin_y` is the row's top in logical pixels.
/// `text_baseline_offset` shifts where the glyph baseline lands relative
/// to the row top — sugarloaf's `text.draw` uses the baseline as the
/// reference y, so we add roughly the cell ascent.
/// Sub-rect fills (in cell-fraction coords 0..1) for the unicode
/// Block Elements range (U+2580..U+259F) plus shade variants. Each
/// entry is `(x0, y0, x1, y1, alpha_multiplier)` so a single block
/// glyph can decompose into 1–2 rects without any font involvement.
///
/// Why this is here instead of in the font: D2Coding's block glyphs
/// (and most monospace fonts) don't extend to the full cell advance
/// width — they leave a 1–2px gap on the right side of each glyph.
/// A run like `██████` becomes a striped bar instead of a solid
/// rectangle. Drawing the cell as a GPU quad sized to the actual
/// `cell_w` × `cell_h` fixes this without depending on font choice.
pub fn block_rects(ch: char) -> Option<&'static [(f32, f32, f32, f32, f32)]> {
    // Macro-ish: each constant is a slice of (x0, y0, x1, y1, alpha)
    // rectangles. Multi-quadrant blocks use two entries.
    const HALF_TOP: &[(f32, f32, f32, f32, f32)] = &[(0.0, 0.0, 1.0, 0.5, 1.0)];
    const HALF_BOTTOM: &[(f32, f32, f32, f32, f32)] = &[(0.0, 0.5, 1.0, 1.0, 1.0)];
    const HALF_LEFT: &[(f32, f32, f32, f32, f32)] = &[(0.0, 0.0, 0.5, 1.0, 1.0)];
    const HALF_RIGHT: &[(f32, f32, f32, f32, f32)] = &[(0.5, 0.0, 1.0, 1.0, 1.0)];
    const FULL: &[(f32, f32, f32, f32, f32)] = &[(0.0, 0.0, 1.0, 1.0, 1.0)];
    const SHADE_25: &[(f32, f32, f32, f32, f32)] = &[(0.0, 0.0, 1.0, 1.0, 0.25)];
    const SHADE_50: &[(f32, f32, f32, f32, f32)] = &[(0.0, 0.0, 1.0, 1.0, 0.5)];
    const SHADE_75: &[(f32, f32, f32, f32, f32)] = &[(0.0, 0.0, 1.0, 1.0, 0.75)];
    Some(match ch {
        // Lower N/8 blocks (U+2581..U+2587) — bottom anchored.
        '\u{2581}' => &[(0.0, 7.0 / 8.0, 1.0, 1.0, 1.0)],
        '\u{2582}' => &[(0.0, 3.0 / 4.0, 1.0, 1.0, 1.0)],
        '\u{2583}' => &[(0.0, 5.0 / 8.0, 1.0, 1.0, 1.0)],
        '\u{2584}' => HALF_BOTTOM,
        '\u{2585}' => &[(0.0, 3.0 / 8.0, 1.0, 1.0, 1.0)],
        '\u{2586}' => &[(0.0, 1.0 / 4.0, 1.0, 1.0, 1.0)],
        '\u{2587}' => &[(0.0, 1.0 / 8.0, 1.0, 1.0, 1.0)],
        // Full / left N/8 / right blocks.
        '\u{2588}' => FULL,
        '\u{2589}' => &[(0.0, 0.0, 7.0 / 8.0, 1.0, 1.0)],
        '\u{258A}' => &[(0.0, 0.0, 3.0 / 4.0, 1.0, 1.0)],
        '\u{258B}' => &[(0.0, 0.0, 5.0 / 8.0, 1.0, 1.0)],
        '\u{258C}' => HALF_LEFT,
        '\u{258D}' => &[(0.0, 0.0, 3.0 / 8.0, 1.0, 1.0)],
        '\u{258E}' => &[(0.0, 0.0, 1.0 / 4.0, 1.0, 1.0)],
        '\u{258F}' => &[(0.0, 0.0, 1.0 / 8.0, 1.0, 1.0)],
        '\u{2590}' => HALF_RIGHT,
        // Shades — full block at reduced alpha.
        '\u{2591}' => SHADE_25,
        '\u{2592}' => SHADE_50,
        '\u{2593}' => SHADE_75,
        // Upper 1/8, right 1/8.
        '\u{2580}' => HALF_TOP,
        '\u{2594}' => &[(0.0, 0.0, 1.0, 1.0 / 8.0, 1.0)],
        '\u{2595}' => &[(7.0 / 8.0, 0.0, 1.0, 1.0, 1.0)],
        // Quadrants — corners + multi-corner combinations.
        '\u{2596}' => &[(0.0, 0.5, 0.5, 1.0, 1.0)],
        '\u{2597}' => &[(0.5, 0.5, 1.0, 1.0, 1.0)],
        '\u{2598}' => &[(0.0, 0.0, 0.5, 0.5, 1.0)],
        '\u{2599}' => &[(0.0, 0.0, 0.5, 1.0, 1.0), (0.5, 0.5, 1.0, 1.0, 1.0)],
        '\u{259A}' => &[(0.0, 0.0, 0.5, 0.5, 1.0), (0.5, 0.5, 1.0, 1.0, 1.0)],
        '\u{259B}' => &[(0.0, 0.0, 1.0, 0.5, 1.0), (0.0, 0.5, 0.5, 1.0, 1.0)],
        '\u{259C}' => &[(0.0, 0.0, 1.0, 0.5, 1.0), (0.5, 0.5, 1.0, 1.0, 1.0)],
        '\u{259D}' => &[(0.5, 0.0, 1.0, 0.5, 1.0)],
        '\u{259E}' => &[(0.5, 0.0, 1.0, 0.5, 1.0), (0.0, 0.5, 0.5, 1.0, 1.0)],
        '\u{259F}' => &[(0.5, 0.0, 1.0, 0.5, 1.0), (0.0, 0.5, 1.0, 1.0, 1.0)],
        // Box Drawing(U+2500~257F)은 여기가 아니라 `box_line_rects`(px 단위) —
        // 이 표는 0..1 비율이라 획 두께를 물리 px 로 못 세운다.
        _ => return None,
    })
}

/// Box Drawing(U+2500~257F)을 칸 크기(px) 기준 채움 사각형으로 뿜는다. 그렸으면
/// true, 폰트로 보낼 글자면 false.
///
/// 폰트에 맡길 수 없는 이유: 글리프는 자기 advance 폭까지만 그리는데 칸이 그보다
/// 넓으면(실측 2026-08-15: 칸 9px vs JetBrains Mono ─ 글리프 ≈7.8px) 글리프
/// 사이마다 틈이 남아 표 가로줄이 점선이 된다(「표 아직 이상해」). 자간·폰트 어떤
/// 조합에서도 이어지려면 렌더러가 칸 끝까지 직접 긋는 수밖에 없다 — ghostty 도
/// 같은 이유로 이 범위를 폰트에 안 맡긴다(src/font/sprite/draw/box.zig).
///
/// 앞서 두 번 접힌 지점을 둘 다 피한다: ①모서리·교차·이중선까지 같이 그려서
/// 폰트와 반반일 때의 어긋남이 없고 ②둥근 모서리(╭╮╰╯)는 사분원을 실제로 그려서
/// claude 입력창이 각지지 않는다. 획 두께는 가로·세로 모두 **칸 높이의 비율**
/// (가는 6%·굵은 12%)로 같은 물리 두께 — 폭 비율로 세우면 세로획이 1px 미만으로
/// 흩어져 사라진다(07:00 실측 14×35px 칸에서 0.84px).
///
/// 혼합 굵기 교차(┍┭…)·단일/이중 혼합(╒╞…)·대각(╱╲╳)만 폰트에 남는다 — 변마다
/// 다른 굵기 조합이라 표가 길어지는데 실사용에서 본 적이 없다.
pub fn box_line_rects(
    ch: char,
    w: f32,
    h: f32,
    push: &mut impl FnMut(f32, f32, f32, f32, f32),
) -> bool {
    if !('\u{2500}'..='\u{257F}').contains(&ch) {
        return false;
    }
    let t = (h * 0.06).max(1.0); // 가는 획
    let tt = (h * 0.12).max(2.0); // 굵은 획
    let (cx, cy) = (w / 2.0, h / 2.0);
    // 가는/굵은 획이 지나는 띠. 반획은 교차 획의 **바깥** 끝까지 가야 모서리에
    // 홈이 안 판다 — 중앙에서 끝내면 획 두께의 절반이 빈다.
    let (lx0, lx1) = (cx - t / 2.0, cx + t / 2.0);
    let (ly0, ly1) = (cy - t / 2.0, cy + t / 2.0);
    let (hx0, hx1) = (cx - tt / 2.0, cx + tt / 2.0);
    let (hy0, hy1) = (cy - tt / 2.0, cy + tt / 2.0);
    // (x0,y0,x1,y1) 튜플 목록으로 모아 한 번에 push — match 팔이 짧아진다.
    let rects: &[(f32, f32, f32, f32)] = match ch {
        // 직선. 점선 변형(┄┈╌ 계열)도 이어진 선으로 — 낱개 점선을 그리면 이웃 칸과
        // 만나는 자리에 지금 고치는 그 틈이 도로 생긴다.
        '\u{2500}' | '\u{2504}' | '\u{2508}' | '\u{254C}' => &[(0.0, ly0, w, ly1)],
        '\u{2501}' | '\u{2505}' | '\u{2509}' | '\u{254D}' => &[(0.0, hy0, w, hy1)],
        '\u{2502}' | '\u{2506}' | '\u{250A}' | '\u{254E}' => &[(lx0, 0.0, lx1, h)],
        '\u{2503}' | '\u{2507}' | '\u{250B}' | '\u{254F}' => &[(hx0, 0.0, hx1, h)],
        // 가는 모서리·교차.
        '\u{250C}' => &[(lx0, ly0, w, ly1), (lx0, ly0, lx1, h)],
        '\u{2510}' => &[(0.0, ly0, lx1, ly1), (lx0, ly0, lx1, h)],
        '\u{2514}' => &[(lx0, ly0, w, ly1), (lx0, 0.0, lx1, ly1)],
        '\u{2518}' => &[(0.0, ly0, lx1, ly1), (lx0, 0.0, lx1, ly1)],
        '\u{251C}' => &[(lx0, 0.0, lx1, h), (lx0, ly0, w, ly1)],
        '\u{2524}' => &[(lx0, 0.0, lx1, h), (0.0, ly0, lx1, ly1)],
        '\u{252C}' => &[(0.0, ly0, w, ly1), (lx0, ly0, lx1, h)],
        '\u{2534}' => &[(0.0, ly0, w, ly1), (lx0, 0.0, lx1, ly1)],
        '\u{253C}' => &[(0.0, ly0, w, ly1), (lx0, 0.0, lx1, h)],
        // 굵은 모서리·교차.
        '\u{250F}' => &[(hx0, hy0, w, hy1), (hx0, hy0, hx1, h)],
        '\u{2513}' => &[(0.0, hy0, hx1, hy1), (hx0, hy0, hx1, h)],
        '\u{2517}' => &[(hx0, hy0, w, hy1), (hx0, 0.0, hx1, hy1)],
        '\u{251B}' => &[(0.0, hy0, hx1, hy1), (hx0, 0.0, hx1, hy1)],
        '\u{2523}' => &[(hx0, 0.0, hx1, h), (hx0, hy0, w, hy1)],
        '\u{252B}' => &[(hx0, 0.0, hx1, h), (0.0, hy0, hx1, hy1)],
        '\u{2533}' => &[(0.0, hy0, w, hy1), (hx0, hy0, hx1, h)],
        '\u{253B}' => &[(0.0, hy0, w, hy1), (hx0, 0.0, hx1, hy1)],
        '\u{254B}' => &[(0.0, hy0, w, hy1), (hx0, 0.0, hx1, h)],
        // 반획(칸 중앙에서 끊기는 외줄).
        '\u{2574}' => &[(0.0, ly0, lx1, ly1)],
        '\u{2575}' => &[(lx0, 0.0, lx1, ly1)],
        '\u{2576}' => &[(lx0, ly0, w, ly1)],
        '\u{2577}' => &[(lx0, ly0, lx1, h)],
        '\u{2578}' => &[(0.0, hy0, hx1, hy1)],
        '\u{2579}' => &[(hx0, 0.0, hx1, hy1)],
        '\u{257A}' => &[(hx0, hy0, w, hy1)],
        '\u{257B}' => &[(hx0, hy0, hx1, h)],
        // 가는↔굵은 이음 직선.
        '\u{257C}' => &[(0.0, ly0, lx1, ly1), (hx0, hy0, w, hy1)],
        '\u{257D}' => &[(lx0, 0.0, lx1, ly1), (hx0, hy0, hx1, h)],
        '\u{257E}' => &[(0.0, hy0, hx1, hy1), (lx0, ly0, w, ly1)],
        '\u{257F}' => &[(hx0, 0.0, hx1, hy1), (lx0, ly0, lx1, h)],
        _ => &[],
    };
    if !rects.is_empty() {
        for &(x0, y0, x1, y1) in rects {
            push(x0, y0, x1, y1, 1.0);
        }
        return true;
    }
    // 이중선. 두 줄 각각 가는 획 두께, 사이 간격도 한 획 — 칸 중앙 기준
    // 바깥쪽(a)·안쪽 아님 단순히 위/왼쪽(a), 아래/오른쪽(b) 두 띠다.
    let (xa0, xa1, xb0, xb1) = (cx - 1.5 * t, cx - 0.5 * t, cx + 0.5 * t, cx + 1.5 * t);
    let (ya0, ya1, yb0, yb1) = (cy - 1.5 * t, cy - 0.5 * t, cy + 0.5 * t, cy + 1.5 * t);
    let rects: &[(f32, f32, f32, f32)] = match ch {
        '\u{2550}' => &[(0.0, ya0, w, ya1), (0.0, yb0, w, yb1)],
        '\u{2551}' => &[(xa0, 0.0, xa1, h), (xb0, 0.0, xb1, h)],
        // 모서리 = 바깥 ㄱ자 + 안쪽 ㄱ자. 각 획은 제 짝 세로줄의 바깥 끝까지.
        '\u{2554}' => &[
            (xa0, ya0, w, ya1),
            (xa0, ya0, xa1, h),
            (xb0, yb0, w, yb1),
            (xb0, yb0, xb1, h),
        ],
        '\u{2557}' => &[
            (0.0, ya0, xb1, ya1),
            (xb0, ya0, xb1, h),
            (0.0, yb0, xa1, yb1),
            (xa0, yb0, xa1, h),
        ],
        '\u{255A}' => &[
            (xa0, yb0, w, yb1),
            (xa0, 0.0, xa1, yb1),
            (xb0, ya0, w, ya1),
            (xb0, 0.0, xb1, ya1),
        ],
        '\u{255D}' => &[
            (0.0, yb0, xb1, yb1),
            (xb0, 0.0, xb1, yb1),
            (0.0, ya0, xa1, ya1),
            (xa0, 0.0, xa1, ya1),
        ],
        // T자 — 세로 두 줄 중 트인 쪽만 끊는다.
        '\u{2560}' => &[
            (xa0, 0.0, xa1, h),
            (xb0, 0.0, xb1, ya1),
            (xb0, yb0, xb1, h),
            (xb0, ya0, w, ya1),
            (xb0, yb0, w, yb1),
        ],
        '\u{2563}' => &[
            (xb0, 0.0, xb1, h),
            (xa0, 0.0, xa1, ya1),
            (xa0, yb0, xa1, h),
            (0.0, ya0, xa1, ya1),
            (0.0, yb0, xa1, yb1),
        ],
        '\u{2566}' => &[
            (0.0, ya0, w, ya1),
            (0.0, yb0, xa1, yb1),
            (xb0, yb0, w, yb1),
            (xa0, yb0, xa1, h),
            (xb0, yb0, xb1, h),
        ],
        '\u{2569}' => &[
            (0.0, yb0, w, yb1),
            (0.0, ya0, xa1, ya1),
            (xb0, ya0, w, ya1),
            (xa0, 0.0, xa1, ya1),
            (xb0, 0.0, xb1, ya1),
        ],
        // 십자 — 네 귀퉁이 ㄱ자 여덟 조각.
        '\u{256C}' => &[
            (xa0, 0.0, xa1, ya1),
            (xb0, 0.0, xb1, ya1),
            (xa0, yb0, xa1, h),
            (xb0, yb0, xb1, h),
            (0.0, ya0, xa1, ya1),
            (0.0, yb0, xa1, yb1),
            (xb0, ya0, w, ya1),
            (xb0, yb0, w, yb1),
        ],
        _ => &[],
    };
    if !rects.is_empty() {
        for &(x0, y0, x1, y1) in rects {
            push(x0, y0, x1, y1, 1.0);
        }
        return true;
    }
    // 둥근 모서리 — 사분원을 획 두께의 짧은 정사각 스탬프로 근사한다. 반지름이
    // 칸 반폭(레티나에서도 10px 안팎)이라 표본 간격을 획의 절반 이하로 두면
    // 이음매가 안 보인다. 스탬프는 칸 밖으로 나가지 않게 잘라 이웃을 안 건드린다.
    let (sx, sy) = match ch {
        '\u{256D}' => (1.0f32, 1.0f32), // ╭ 원 중심이 칸 우하 방향
        '\u{256E}' => (-1.0, 1.0),      // ╮ 좌하
        '\u{256F}' => (-1.0, -1.0),     // ╯ 좌상
        '\u{2570}' => (1.0, -1.0),      // ╰ 우상
        _ => return false,
    };
    let r = cx.min(cy);
    let (ox, oy) = (cx + sx * r, cy + sy * r);
    let n = ((r * std::f32::consts::FRAC_PI_2) / (t * 0.4)).ceil().max(6.0) as usize;
    for i in 0..=n {
        let a = std::f32::consts::FRAC_PI_2 * (i as f32 / n as f32);
        let (px, py) = (ox - sx * r * a.cos(), oy - sy * r * a.sin());
        let (x0, y0) = ((px - t / 2.0).max(0.0), (py - t / 2.0).max(0.0));
        let (x1, y1) = ((px + t / 2.0).min(w), (py + t / 2.0).min(h));
        if x1 > x0 && y1 > y0 {
            push(x0, y0, x1, y1, 1.0);
        }
    }
    // 곡선 끝에서 칸 변까지의 직선 꼬리(칸이 정사각이 아니면 한쪽만 남는다).
    let (vy0, vy1) = if sy > 0.0 { (cy + r, h) } else { (0.0, cy - r) };
    if vy1 > vy0 {
        push(lx0, vy0, lx1, vy1, 1.0);
    }
    let (hx0t, hx1t) = if sx > 0.0 { (cx + r, w) } else { (0.0, cx - r) };
    if hx1t > hx0t {
        push(hx0t, ly0, hx1t, ly1, 1.0);
    }
    true
}

#[cfg(test)]
mod box_line_tests {
    use super::*;

    fn collect(ch: char, w: f32, h: f32) -> Vec<(f32, f32, f32, f32, f32)> {
        let mut v = Vec::new();
        assert!(box_line_rects(ch, w, h, &mut |a, b, c, d, e| v.push((a, b, c, d, e))));
        v
    }

    /// 이 함수의 존재 이유 그 자체 — 가로선이 칸 폭을 다 채워야 이웃과 이어진다.
    #[test]
    fn horizontal_line_spans_full_cell_width() {
        for ch in ['\u{2500}', '\u{2501}', '\u{2550}'] {
            let v = collect(ch, 9.0, 22.0);
            assert!(v.iter().all(|r| r.0 == 0.0 && r.2 == 9.0), "{ch:?}: {v:?}");
        }
    }

    /// 세로획이 폭 비율로 서면 1px 미만으로 사라진다(07:00 실측) — 가로획과 같은
    /// 물리 두께여야 한다.
    #[test]
    fn vertical_stroke_matches_horizontal_thickness() {
        let hline = collect('\u{2500}', 9.0, 22.0)[0];
        let vline = collect('\u{2502}', 9.0, 22.0)[0];
        let h_thick = hline.3 - hline.1;
        let v_thick = vline.2 - vline.0;
        assert!((h_thick - v_thick).abs() < 0.01, "h={h_thick} v={v_thick}");
        assert!(v_thick >= 1.0);
    }

    /// 모서리의 두 획은 겹쳐야 한다 — 중앙에서 각각 끝나면 귀퉁이에 홈이 판다.
    #[test]
    fn corner_strokes_overlap_without_notch() {
        for ch in ['\u{250C}', '\u{2510}', '\u{2514}', '\u{2518}', '\u{250F}'] {
            let v = collect(ch, 9.0, 22.0);
            let (a, b) = (v[0], v[1]);
            let ox = a.0.max(b.0) < a.2.min(b.2);
            let oy = a.1.max(b.1) < a.3.min(b.3);
            assert!(ox && oy, "{ch:?} 두 획이 안 겹친다: {v:?}");
        }
    }

    /// 둥근 모서리는 직선 이웃과 만나는 두 변(오른쪽·아래) 끝까지 닿아야 하고,
    /// 스탬프가 칸 밖으로 새면 이웃 칸을 물들인다.
    #[test]
    fn rounded_corner_touches_both_edges_within_cell() {
        let (w, h) = (9.0, 22.0);
        let v = collect('\u{256D}', w, h);
        assert!(v.iter().all(|r| r.0 >= 0.0 && r.1 >= 0.0 && r.2 <= w && r.3 <= h));
        let touches_right = v.iter().any(|r| r.2 >= w - 0.01);
        let touches_bottom = v.iter().any(|r| r.3 >= h - 0.01);
        assert!(touches_right && touches_bottom, "{v:?}");
    }

    /// 혼합 굵기 교차는 일부러 폰트로 — 여기서 그리면 변마다 굵기가 틀린다.
    #[test]
    fn mixed_weight_junctions_fall_back_to_font() {
        let mut n = 0;
        let drew = box_line_rects('\u{250D}', 9.0, 22.0, &mut |_, _, _, _, _| n += 1);
        assert!(!drew);
        assert_eq!(n, 0);
    }
}

