// Release builds hide the console window so kasaterm launches as a pure GUI
// app (Start menu / .msi install). Debug builds keep the console so stderr
// startup/IME logs stay visible for the self-test cycle.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! kasaterm — sugarloaf-rendered terminal driven by
//! tmux-bridge. Multi-pane: tmux's split-window creates additional
//! panes, layout-change events tell us how to lay them out, and we
//! render each pane inside its rect from the parsed Layout tree.
//! Phase A Task #13/14: wheel + scrollback, IME, selection + clipboard,
//! cursor blink, OSC titles, multi-pane render + focus routing.

mod autosuggest;
mod cells;
mod gpu;
mod render;
mod handler;
mod socket;
#[cfg(unix)]
mod bridge;
mod stream;
mod theme;
mod transcript;
mod chrome;
mod testkit;
mod session;
mod layout;
mod markdown;
mod auxwin;
mod input;
mod settings;
mod syntax;
mod lsp;
mod links;
mod proc;
mod info;
mod state;
// macOS `.md` 더블클릭(odoc Apple Event) 핸들러. 다른 OS 엔 파일오픈 이벤트가
// 이 경로로 안 와서 macos 전용.
#[cfg(target_os = "macos")]
mod macos_open;
#[cfg(target_os = "macos")]
mod macos_sparkle;
// Windows 자동 업데이트 — WinSparkle.dll 런타임 로드(macos_sparkle 대칭).
// 모듈 자체는 전 플랫폼 컴파일: 토스트 센티널·버전 파서(+테스트)·no-op 스텁은
// 공용이고 FFI/체커 스레드만 cfg(windows) 게이트 — mac 에서도 헬퍼 테스트가 돈다.
mod win_sparkle;

use anyhow::Result;
use std::collections::{HashMap, VecDeque};
use std::error::Error;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use kasa_bridge::layout::{parse_layout, Layout};
use kasa_bridge::screen::Cell as GridCell;
use kasa_bridge::screen::Row;
use kasa_bridge::{ScreenUpdate, StartOptions, TmuxEvent, TmuxSession};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{
    ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{CursorIcon, Theme, Window, WindowAttributes, WindowId};
#[cfg(target_os = "macos")]
use winit::platform::macos::WindowAttributesExtMacOS;

// Match ghostty's default font-size=13. We were at 14, which made every
// cell ~7.7% taller than ghostty's — visible side-by-side as a "slightly
// larger" terminal even though the chrome looked identical.
const FONT_SIZE: f32 = 16.0;

/// Display columns a char occupies in a proportional-ish label: CJK /
/// Hangul / fullwidth glyphs are double-width, everything else single.
/// Used to budget sidebar tab text so a Hangul title doesn't overflow the
/// strip into the cell grid.
fn cjk_display_w(c: char) -> usize {
    let u = c as u32;
    let wide = matches!(u,
        0x1100..=0x115F   // Hangul Jamo
        | 0x2E80..=0x303E // CJK radicals, Kangxi, CJK symbols
        | 0x3041..=0x33FF // Hiragana, Katakana, CJK compat
        | 0x3400..=0x4DBF // CJK ext A
        | 0x4E00..=0x9FFF // CJK unified
        | 0xA000..=0xA4CF // Yi
        | 0xAC00..=0xD7A3 // Hangul syllables
        | 0xF900..=0xFAFF // CJK compat ideographs
        | 0xFE30..=0xFE4F // CJK compat forms
        | 0xFF00..=0xFF60 // Fullwidth forms
        | 0xFFE0..=0xFFE6
        | 0x20000..=0x3FFFD // CJK ext B+ / supplementary ideographs
    );
    if wide { 2 } else { 1 }
}

/// Draw a rounded-corner rect with the sharp-quad renderer: a full-width
/// middle block plus per-row caps whose horizontal inset follows a circle of
/// radius `r`, giving genuine rounded corners. `r` is small (≤~10px) so this
/// is only a handful of extra quads; rendering is throttled anyway.
/// Quote a filesystem path for safe pasting into a shell prompt. Bare when it
/// holds only shell-safe characters, single-quoted (with embedded quotes
/// escaped) otherwise — covers spaces, parens, and non-ASCII (e.g. 한글) paths.
fn shell_quote_path(p: &str) -> String {
    let safe = !p.is_empty()
        && p.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-/~+=,@:".contains(&b));
    if safe {
        p.to_string()
    } else {
        format!("'{}'", p.replace('\'', "'\\''"))
    }
}

fn round_rect(g: &mut gpu::GpuRenderer, x: f32, y: f32, w: f32, h: f32, r: f32, col: [u8; 4]) {
    // Single anti-aliased implementation lives on the renderer so the chrome
    // and the markdown code-block chips round identically.
    g.round_rect_fill(x, y, w, h, r, col);
}

/// Paint the git-column header dropdowns (repo path picker + branch switcher)
/// and fill their click rects. A free fn, not a method, so it can run inside
/// the `&mut self.gpu` block (which can't re-borrow `&self`): the caller hands
/// it `g` plus the disjoint rect Vecs. Drawn last so it overlays the list.
#[allow(clippy::too_many_arguments)]
fn git_paint_dropdowns(
    g: &mut gpu::GpuRenderer,
    col_x: f32,
    col_w: f32,
    _title_h: f32,
    path_hdr: Option<(f32, f32, f32, f32)>,
    branch_hdr: Option<(f32, f32, f32, f32)>,
    path_open: bool,
    branch_open: bool,
    repos: &[std::path::PathBuf],
    pinned: &Option<std::path::PathBuf>,
    branches: &[String],
    current_branch: &str,
    path_rects: &mut Vec<(Option<std::path::PathBuf>, (f32, f32, f32, f32))>,
    branch_rects: &mut Vec<(String, (f32, f32, f32, f32))>,
) {
    let item_h = 28.0_f32;
    let pad = 6.0_f32;
    let px = col_x + 6.0;
    let pw = (col_w - 12.0).max(0.0);
    // A raised menu panel with a 1px border so it reads above the list.
    let panel = |g: &mut gpu::GpuRenderer, y: f32, h: f32| {
        round_rect(g, px - 1.0, y - 1.0, pw + 2.0, h + 2.0, theme::RADIUS_MD, theme::border());
        round_rect(g, px, y, pw, h, theme::RADIUS_MD, theme::surface_active());
    };
    let row = |g: &mut gpu::GpuRenderer, iy: f32, label: &str, on: bool| {
        if on {
            round_rect(g, px + 4.0, iy + 1.0, pw - 8.0, item_h - 2.0, theme::RADIUS_SM, theme::with_alpha(theme::accent(), 0x40));
        }
        let col = if on { theme::text() } else { theme::text_dim() };
        g.draw_text(px + 12.0, iy + (item_h - 12.0) / 2.0, label, gpu::DrawOpts { font_size: 12.0, color: col, bold: false, italic: false });
    };
    if path_open {
        if let Some((_hx, hy, _hw, hh)) = path_hdr {
            let n = repos.len() + 1; // +1 for the "자동 추적" toggle
            let menu_h = n as f32 * item_h + pad * 2.0;
            let my = hy + hh + 2.0;
            panel(g, my, menu_h);
            let mut iy = my + pad;
            // "자동 추적" — selected when nothing is pinned.
            row(g, iy, "자동 추적 (활성 pane)", pinned.is_none());
            path_rects.push((None, (px, iy, pw, item_h)));
            iy += item_h;
            for r in repos {
                let name = r.file_name().and_then(|s| s.to_str()).unwrap_or("?");
                let sel = pinned.as_ref() == Some(r);
                row(g, iy, name, sel);
                path_rects.push((Some(r.clone()), (px, iy, pw, item_h)));
                iy += item_h;
            }
        }
    }
    if branch_open {
        if let Some((_hx, hy, _hw, hh)) = branch_hdr {
            let n = branches.len().max(1);
            let menu_h = n as f32 * item_h + pad * 2.0;
            let my = hy + hh + 2.0;
            panel(g, my, menu_h);
            let mut iy = my + pad;
            if branches.is_empty() {
                row(g, iy, "(브랜치 없음)", false);
            }
            for b in branches {
                row(g, iy, b, b == current_branch);
                branch_rects.push((b.clone(), (px, iy, pw, item_h)));
                iy += item_h;
            }
        }
    }
}

/// Lucide icon name for a sidebar tab's chip, chosen from the window label.
/// claude panes get the sparkle, markdown docs a file, everything else the
/// terminal glyph — keeps window identity readable after the SVG switch.
fn tab_icon_glyph(name: &str) -> &'static str {
    let l = name.to_ascii_lowercase();
    if name.contains('✳') || l.contains("claude") {
        "sparkles"
    } else if l.ends_with(".md") {
        "file-text"
    } else {
        "terminal"
    }
}

/// claude Code 가 OSC 제목에 붙이는 선행 활동 글리프(✳/✶/✻/✽ … dingbat 별표류
/// + ∗ ＊ * + 브라유 스피너 ⠂⠐… U+2800 블록)와 공백 run 을 벗긴다. 타이틀바·
/// 헤더·board 라벨이 "아로나 · ⠂ 요약" 대신 "아로나 · 요약" 을 보이게(거노).
/// 활동 글리프로 시작 안 하면 원문 그대로(rename 사용자 값 보호).
pub(crate) fn strip_activity_prefix(s: &str) -> &str {
    s.trim_start_matches(|c: char| {
        c.is_whitespace()
            || matches!(c,
                '*' | '\u{2217}' | '\u{FF0A}'   // ASCII * / ∗ / ＊
                | '\u{2721}'..='\u{2749}'        // ✢..❉ dingbat asterisks·stars
                | '\u{2800}'..='\u{28FF}'        // braille 스피너 프레임
            )
    })
}

/// 내장 이미지 뷰로 열 수 있는 확장자인가. 이미지는 "파일 열기" 설정을 타지
/// 않는다 — CLI 편집기에 넘기면 바이너리 쓰레기만 뜬다.
pub(crate) fn is_image_path(path: &std::path::Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tiff" | "tif" | "ico"
    )
}

/// File-type icon (assets/icons/ft, 브랜드컬러 filled)를 파일명에서 고른다.
/// 특수 파일명(README, Dockerfile, tsconfig…)이 확장자보다 우선. `None` 이면
/// 매핑이 없다는 뜻 — 호출부는 기존 모노크롬 "file" 글리프로 폴백한다.
fn file_icon(name: &str) -> Option<&'static str> {
    let l = name.to_ascii_lowercase();
    // Well-known file names first — these outrank their extension.
    let by_name = match l.as_str() {
        "readme" | "readme.md" | "readme.txt" => Some("ft/readme"),
        "license" | "license.md" | "license.txt" | "licence" => Some("ft/license"),
        "todo" | "todo.md" => Some("ft/todo"),
        "dockerfile" | ".dockerignore" | "docker-compose.yml" | "docker-compose.yaml" => {
            Some("ft/docker")
        }
        "tsconfig.json" => Some("ft/tsconfig"),
        "package.json" | "package-lock.json" => Some("ft/nodejs"),
        ".gitignore" | ".gitattributes" | ".gitmodules" | ".gitconfig" => Some("ft/git"),
        _ => None,
    };
    if by_name.is_some() {
        return by_name;
    }
    if l.starts_with(".env") {
        return Some("ft/settings");
    }
    let ext = l.rsplit_once('.').map(|(_, e)| e)?;
    Some(match ext {
        "rs" => "ft/rust",
        "ts" | "mts" | "cts" => "ft/typescript",
        "tsx" | "jsx" => "ft/react",
        "js" | "mjs" | "cjs" => "ft/javascript",
        "json" | "jsonc" => "ft/json",
        "md" | "markdown" | "mdx" => "ft/markdown",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "ico" | "bmp" | "avif" | "heic" => "ft/image",
        "svg" => "ft/svg",
        "toml" | "ini" | "conf" | "cfg" | "plist" | "properties" => "ft/settings",
        "yaml" | "yml" => "ft/yaml",
        "sh" | "bash" | "zsh" | "fish" | "bat" | "cmd" => "ft/console",
        "ps1" | "psm1" => "ft/powershell",
        "py" | "pyi" => "ft/python",
        "c" | "h" => "ft/c",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" => "ft/cpp",
        "cs" => "ft/csharp",
        "css" => "ft/css",
        "scss" | "sass" | "less" => "ft/sass",
        "html" | "htm" | "xhtml" => "ft/html",
        "go" => "ft/go",
        "java" => "ft/java",
        "kt" | "kts" => "ft/kotlin",
        "lua" => "ft/lua",
        "php" => "ft/php",
        "rb" | "erb" => "ft/ruby",
        "swift" => "ft/swift",
        "vue" => "ft/vue",
        "graphql" | "gql" => "ft/graphql",
        "prisma" => "ft/prisma",
        "sql" | "db" | "sqlite" | "sqlite3" => "ft/database",
        "pdf" => "ft/pdf",
        "zip" | "tar" | "gz" | "bz2" | "xz" | "7z" | "rar" | "tgz" => "ft/zip",
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" => "ft/audio",
        "mp4" | "mov" | "mkv" | "avi" | "webm" => "ft/video",
        "ttf" | "otf" | "woff" | "woff2" => "ft/font",
        "lock" => "ft/lock",
        "txt" | "rtf" | "log" => "ft/document",
        _ => return None,
    })
}

/// Draw a chrome icon glyph centered inside a square chip whose top-left is
/// (`chip_x`, `chip_y`). One place owns icon sizing / centering / hover so every
/// icon button (title bar, sidebar, pane header, image controls) reads
/// identically. The clickable area is `theme::ICON_CHIP` square — hit-test
/// against the same rect.
/// Line spacing multiplier passed to `compute_cell_metrics`. 1.0 keeps
/// rows at font ascent+descent so the cell aspect ratio stays close to
/// 1:2 — that's what makes half-block sprite art (Claude Code's mascot,
/// `▀▄▌▐` characters) read as squares instead of tall rectangles. The
/// earlier 1.3 stretched cells to 1:3 and made the sprite look
/// elongated next to Ghostty / iTerm2.
/// Logical-pixel padding between the window edge and the cell grid on
/// every side. Mirrors what Terminal.app / Ghostty give the content so
/// text doesn't jam against the chrome. Must match `render_frame`'s
/// origin and `px_to_pane_cell`'s offset.
const WINDOW_PADDING: f32 = 0.0;
/// Logical-pixel height of the custom chrome strip that sits above the
/// cell grid (traffic light row + future tab bar). macOS's traffic
/// light buttons end around y ≈ 28 in logical units, so 38 leaves a
/// few pixels of breathing room. Cells start at y = TITLE_HEIGHT (not
/// at WINDOW_PADDING) so the title strip is fully clear of glyph
/// drawing.
const TITLE_HEIGHT: f32 = 36.0;
/// Width of the macOS traffic-light cluster (close/min/zoom) measured
/// from the window's left edge. Mouse events inside this rectangle are
/// reserved for the native buttons; our drag handler ignores them so a
/// click on the red dot still closes the window.
const TRAFFIC_LIGHT_WIDTH: f32 = 78.0;
/// iTerm-style per-pane header height in logical pixels. Each split
/// pane gets one of these strips above its cell grid; a single
/// un-split window renders no header at all (matches the iTerm
/// behavior the user pointed at).
const PANE_HEADER_HEIGHT: f32 = 30.0;
/// Per-pane status bar height (logical px) — the strip below a pane's cell grid
/// holding the cwd / git-branch / diff chips. Mirrors the header band: when a
/// pane shows its bar, the PTY's usable rows shrink by the equivalent cell
/// count so the grid still fits above it. Toggled per pane (see
/// `statusbar_hidden`); a hidden pane reserves nothing.
const PANE_FOOTER_HEIGHT: f32 = 24.0;
/// Bottom dock bar height (logical px) — folded-pane chips. Reserved from the
/// grid only when the dock is non-empty.
const DOCK_HEIGHT: f32 = 40.0;
/// 활성 탭 상단 accent 선 두께(logical px). BORDER stroke(1px)보다 살짝 굵게.
const ACTIVE_ACCENT_STROKE: f32 = 2.0;
/// Inner padding between a pane's box edges and its cell grid, in logical
/// pixels. Keeps text off the divider / window edge and gives abutting
/// panes visible breathing room. The PTY's usable cols/rows shrink by the
/// equivalent cell count so the grid still fits inside the inset box, and
/// every render origin + click-to-cell map applies the same offset.
const PANE_INNER_X: f32 = 6.0;
const PANE_INNER_Y: f32 = 0.0;
/// Left sidebar width in logical pixels. Hosts the vertical tab list
/// (one row per tab) plus the new-tab "+" button. The cell grid origin
/// shifts right by this amount so pane contents never overlap the
/// sidebar. Sidebar is always shown — including single-tab sessions —
/// so the layout doesn't reflow when a second tab appears.
// Warp-style narrow vertical tab strip on the left, one tab per window
// in the current session. Logical px (the renderer multiplies by scale).
// Every `col`/`origin_x` calc already adds `WINDOW_PADDING + SIDEBAR_W`,
// so bumping this off 0 shifts the whole cell grid right automatically.
const SIDEBAR_W: f32 = 200.0;
/// Sidebar layout (logical px), Warp-style. Non-selected tabs are flat
/// (icon + two text lines, no box); the selected tab gets a subtle rounded
/// highlight inset from the strip edges. Tabs sit close together.
const SIDEBAR_TAB_H: f32 = 54.0;
const SIDEBAR_TAB_GAP: f32 = 3.0;
const SIDEBAR_TAB_INSET: f32 = 8.0;
/// File-tree column, parked just right of the session-tab sidebar (VSCode
/// explorer layout). Its own width + visibility, independent of the tab
/// strip, so the tree can be toggled / resized without touching the tabs.
/// `effective_sidebar_w()` folds this in, so the cell grid origin shifts
/// right by tabs+tree together and the terminal reflows automatically.
const FILE_TREE_W: f32 = 220.0;
const FILE_TREE_W_MIN: f32 = 140.0;
const FILE_TREE_W_MAX: f32 = 480.0;
/// Right-hand git column, mirroring the file-tree column on the left:
/// `effective_right_chrome_w()` folds its width in so the cell grid reflows
/// and panes never overlap it.
const GIT_COL_W: f32 = 420.0;
const GIT_COL_W_MIN: f32 = 220.0;
const GIT_COL_W_MAX: f32 = 720.0;
const SCROLLBACK_MAX: usize = 5000;
/// Min ms between wheel emits. Default 0 = pass every macOS scroll event
/// straight through to `pty.scroll`; the try_send-based reader pipeline
/// absorbs the burst without back-pressuring bash, so throttling here just
/// muddies the inertia feel. Raise via `KASATERM_WHEEL_THROTTLE_MS=<n>` to
/// dampen if you want a smoother (lenis-style) scroll.
fn wheel_throttle_ms() -> u64 {
    static CACHED: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *CACHED.get_or_init(|| {
        std::env::var("KASATERM_WHEEL_THROTTLE_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    })
}
/// PixelDelta(트랙패드·고해상도 마우스휠) 스크롤 감도 배율. 구 기본 0.3 은 트랙패드
/// 관성엔 부드러웠지만 한 노치 p.y 가 작은 마우스휠에선 굼떠(거노: 드르륵 한 번에
/// 안 넘어감) 1.0 으로 올렸다. `KASATERM_WHEEL_PIXEL_GAIN=<f>` 로 재시작 후 감도 조절.
fn wheel_pixel_gain() -> f32 {
    static CACHED: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *CACHED.get_or_init(|| {
        std::env::var("KASATERM_WHEEL_PIXEL_GAIN")
            .ok()
            .and_then(|s| s.parse().ok())
            .filter(|v: &f32| *v > 0.0)
            .unwrap_or(1.0)
    })
}
/// PixelDelta 최소 스텝 floor(px). 이 픽셀 이상 굴린 델타는 배율 결과가 1셀 미만
/// 이어도 최소 1셀 emit 해 "드르륵 한 번"이 즉시 넘어간다. 트랙패드 관성 미세꼬리
/// (sub-2px)는 floor 밖이라 과민해지지 않는다.
const WHEEL_PIXEL_FLOOR_PX: f32 = 2.0;
/// Half-period of the cursor blink in milliseconds. macOS uses 530 by
/// default; iTerm2 uses 500. 530 matches the platform feel.
const BLINK_HALF_PERIOD_MS: u64 = 530;
/// Launch build banner: hold the `v…·<rev>` corner label fully visible
/// for HOLD ms, then fade it out over FADE ms so you can tell builds
/// apart at startup without it nagging afterwards.
const VERSION_HOLD_MS: u128 = 4000;
const VERSION_FADE_MS: u128 = 1200;
/// While the user is actively typing we keep the cursor solid for this
/// long after the last keystroke so it's easy to follow the caret. Same
/// idea as iTerm2's "smart cursor" pause.
const BLINK_PAUSE_AFTER_INPUT_MS: u64 = 700;


/// Session panel: a tmux-style list of sessions in its own OS window driving
/// a wry webview, mirroring the git panel. Polls `/sessions` once a second to
/// draw one row per session, highlights the active one, click → switch,
/// "+ 새 세션" → create. Kept fully separate from the wgpu terminal window.
const SESSION_PANEL_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>
  :root { color-scheme: dark; }
  * { box-sizing: border-box; }
  body {
    margin: 0; padding: 14px;
    font: 13px/1.5 -apple-system, "SF Pro Text", system-ui, sans-serif;
    background: #1a1d23; color: #ecedf3;
    -webkit-user-select: none; user-select: none;
  }
  .title { font-weight: 600; font-size: 14px; color: #ecedf3; margin-bottom: 10px; }
  ul { margin: 0; padding: 0; list-style: none; display: flex; flex-direction: column; gap: 6px; }
  .sess {
    display: flex; align-items: center; gap: 10px;
    padding: 9px 11px; border-radius: 9px; cursor: pointer;
    background: #22262e; border: 1px solid #22262e; color: #a0a6b0;
  }
  .sess:hover { background: #22262e; }
  .sess.active { background: #2e323b; border-color: #5a8ce6; color: #ecedf3; }
  .sess .dot { width: 8px; height: 8px; border-radius: 50%; flex: 0 0 auto; background: #787e8a; }
  .sess.active .dot { background: #5a8ce6; box-shadow: 0 0 6px #5a8ce666; }
  .sess .label { font-weight: 600; }
  .sess .badge { margin-left: auto; font-size: 11px; color: #787e8a; }
  .sess.active .badge { color: #5a8ce6; }
  .sess .close { background: none; border: none; color: #787e8a; font-size: 16px;
    line-height: 1; padding: 0 2px; margin-left: 8px; cursor: pointer; flex: 0 0 auto; }
  .sess .close:hover { color: #f85149; }
  /* Pencil (rename) — only visible on row hover so the row stays clean. */
  .sess .edit { background: none; border: none; color: #787e8a; padding: 0 2px;
    margin-left: 8px; cursor: pointer; flex: 0 0 auto; display: inline-flex;
    align-items: center; opacity: 0; transition: opacity .12s; }
  .sess:hover .edit { opacity: 1; }
  .sess .edit:hover { color: #5a8ce6; }
  .sess .edit svg { width: 13px; height: 13px; }
  /* Inline name editor, swapped in for the label span on pencil-click. */
  .sess input.rename { flex: 1 1 auto; min-width: 0; font: inherit; font-weight: 600;
    color: #ecedf3; background: #15171c; border: 1px solid #5a8ce6; border-radius: 5px;
    padding: 1px 6px; outline: none; }
  .new {
    margin-top: 12px; width: 100%; padding: 9px 0; border-radius: 9px;
    background: #22262e; color: #ecedf3; border: 1px dashed #2e323b;
    font-size: 13px; cursor: pointer;
  }
  .new:hover:not(:disabled) { background: #2a313b; border-color: #4a525e; }
  .new:disabled { opacity: .5; cursor: default; }
  .reset {
    margin-top: 8px; width: 100%; padding: 8px 0; border-radius: 9px;
    background: none; color: #787e8a; border: 1px solid #2e323b;
    font-size: 12px; cursor: pointer;
  }
  .reset:hover:not(:disabled) { background: #2a1d1f; border-color: #f85149; color: #f85149; }
  .reset:disabled { opacity: .5; cursor: default; }
  .err { color: #f85149; font-size: 12px; margin-top: 10px; }
</style>
</head>
<body>
  <div class="title">세션</div>
  <ul id="list"></ul>
  <button id="btn-new" class="new">+ 새 세션</button>
  <button id="btn-reset" class="reset">전체 초기화</button>
  <div id="err" class="err" style="display:none"></div>
<script>
const PORT = "__PORT__";
const $ = (id) => document.getElementById(id);
const base = "http://127.0.0.1:" + PORT;
let busy = false;
// While inline-renaming, skip the 1s poll re-render so the open <input> isn't
// yanked out from under the user mid-edit.
let editing = false;

function render(d) {
  $("err").style.display = "none";
  const count = d.count || 1, active = d.active || 0;
  const saved = Array.isArray(d.saved) ? d.saved : [];
  // Live per-session folder labels from the daemon (parallel to count).
  const labels = Array.isArray(d.labels) ? d.labels : [];
  if (editing) return;
  const ul = $("list");
  ul.innerHTML = "";
  for (let i = 0; i < count; i++) {
    const li = document.createElement("li");
    li.className = "sess" + (i === active ? " active" : "");
    const dot = document.createElement("span"); dot.className = "dot";
    const label = document.createElement("span"); label.className = "label";
    label.textContent = (labels[i] && labels[i].length) ? labels[i] : ("세션 " + (i + 1));
    const badge = document.createElement("span"); badge.className = "badge";
    if (i === active) badge.textContent = "활성";
    li.appendChild(dot); li.appendChild(label); li.appendChild(badge);
    const pen = document.createElement("button");
    pen.className = "edit"; pen.title = "이름 변경";
    pen.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 20h9"/><path d="M16.5 3.5a2.121 2.121 0 0 1 3 3L7 19l-4 1 1-4 12.5-12.5z"/></svg>';
    pen.onclick = (e) => { e.stopPropagation(); startRename(i, li, label); };
    li.appendChild(pen);
    if (count > 1) {
      const x = document.createElement("button");
      x.className = "close"; x.textContent = "×"; x.title = "세션 닫기";
      x.onclick = (e) => { e.stopPropagation(); closeSession(i); };
      li.appendChild(x);
    }
    if (i !== active) li.onclick = () => switchTo(i);
    ul.appendChild(li);
  }
  // Saved sessions from the last shutdown — light-launch keeps them on disk
  // instead of auto-restoring. Click to restore in-place; we surface them
  // here so they're never lost behind the always-fresh first pane.
  if (saved.length) {
    const hr = document.createElement("li");
    hr.style.cssText = "margin-top:14px;color:#787e8a;font-size:11px;letter-spacing:.04em;text-transform:uppercase;border-top:1px solid #2e323b;padding-top:10px;list-style:none;";
    hr.textContent = "저장됨";
    ul.appendChild(hr);
    saved.forEach((name, i) => {
      const li = document.createElement("li");
      li.className = "sess";
      li.style.opacity = "0.78";
      const dot = document.createElement("span"); dot.className = "dot";
      const label = document.createElement("span"); label.className = "label";
      label.textContent = name || ("세션 " + (i + 1));
      const badge = document.createElement("span"); badge.className = "badge";
      badge.textContent = "저장";
      li.appendChild(dot); li.appendChild(label); li.appendChild(badge);
      li.title = "이 세션 복원";
      li.onclick = () => restoreSaved(i);
      ul.appendChild(li);
    });
  }
}

async function restoreSaved(idx) {
  if (busy) return;
  busy = true;
  try {
    await fetch(base + "/session-restore?idx=" + idx, { method: "POST" });
  } catch (e) {}
  busy = false;
  poll();
}

async function switchTo(idx) {
  if (busy) return;
  busy = true;
  // idx in the query string, no JSON body → no CORS preflight (the panel's
  // null origin would otherwise trip an OPTIONS the server 405s).
  try {
    await fetch(base + "/session-switch?idx=" + idx, { method: "POST" });
  } catch (e) {}
  busy = false;
  poll();
}

async function closeSession(idx) {
  if (busy) return;
  busy = true;
  try {
    await fetch(base + "/session-close?idx=" + idx, { method: "POST" });
  } catch (e) {}
  busy = false;
  poll();
}

// Swap the label span for an inline <input>. Enter saves, Esc/blur leaves it.
function startRename(idx, li, labelEl) {
  if (editing) return;
  editing = true;
  const input = document.createElement("input");
  input.className = "rename";
  input.value = labelEl.textContent;
  li.replaceChild(input, labelEl);
  input.focus(); input.select();
  let done = false;
  const finish = (save) => {
    if (done) return; done = true;
    editing = false;
    if (save) renameSession(idx, input.value.trim());
    else poll();
  };
  input.onkeydown = (e) => {
    if (e.key === "Enter") { e.preventDefault(); finish(true); }
    else if (e.key === "Escape") { e.preventDefault(); finish(false); }
  };
  input.onblur = () => finish(true);
  input.onclick = (e) => e.stopPropagation();
}

async function renameSession(idx, name) {
  busy = true;
  try {
    await fetch(base + "/session-rename?idx=" + idx + "&name=" + encodeURIComponent(name), { method: "POST" });
  } catch (e) {}
  busy = false;
  poll();
}

async function doReset() {
  if (busy) return;
  if (!confirm("모든 세션과 pane을 닫고 새 세션 하나로 초기화할까요?")) return;
  busy = true;
  $("btn-reset").disabled = true;
  try {
    await fetch(base + "/session-reset", { method: "POST" });
  } catch (e) {}
  busy = false;
  $("btn-reset").disabled = false;
  poll();
}

async function doNew() {
  if (busy) return;
  busy = true;
  $("btn-new").disabled = true;
  try {
    await fetch(base + "/session-new", { method: "POST" });
  } catch (e) {}
  busy = false;
  $("btn-new").disabled = false;
  poll();
}

async function poll() {
  try {
    const r = await fetch(base + "/sessions", { cache: "no-store" });
    render(await r.json());
  } catch (e) {
    $("err").style.display = "block";
    $("err").textContent = "server unreachable :" + PORT;
  }
}

$("btn-new").onclick = doNew;
$("btn-reset").onclick = doReset;
poll();
setInterval(poll, 1000);
</script>
</body>
</html>"#;

/// Board panel: a read-only monitor of each pane's live activity (surface_id ·
/// status · intent · files), auto-filled from the transcript watcher, in its
/// own OS window driving a wry webview. Mirrors the session panel. Polls
/// `/board` once a second. No input here on purpose — the user and the panes
/// just *watch* it; talking to a pane happens through that pane's own prompt
/// (claude↔claude uses `kasaterm-cli tell`).
const BOARD_PANEL_HTML: &str = r#"<!DOCTYPE html>
<html lang="ko">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>
  :root { color-scheme: dark; }
  * { box-sizing: border-box; }
  body {
    margin: 0; padding: 14px;
    font: 13px/1.5 -apple-system, "SF Pro Text", system-ui, sans-serif;
    background: #1a1d23; color: #ecedf3;
    -webkit-user-select: none; user-select: none;
  }
  .title { font-weight: 600; font-size: 14px; margin-bottom: 10px; }
  .pane { background: #22262e; border: 1px solid #2e323b; border-left: 4px solid #2e323b; border-radius: 9px; padding: 10px; margin-bottom: 8px; }
  .badges { margin-top: 6px; display: flex; flex-wrap: wrap; gap: 5px; }
  .badge { font-size: 10px; padding: 2px 6px; border-radius: 5px; background: #2e323b; color: #a0a6b0; }
  .badge.tok { color: #e3b341; }
  .badge.cost { color: #70AD47; }
  .badge.chg { color: #f0883e; }
  .tools { margin-top: 4px; font-size: 10px; color: #5a5f6b; word-break: break-word; }
  .row1 { display: flex; align-items: center; gap: 8px; }
  .sid { font-weight: 600; color: #5a8ce6; }
  .status { margin-left: auto; font-size: 11px; padding: 2px 8px; border-radius: 6px; background: #2e323b; color: #a0a6b0; }
  .status.working { color: #5a8ce6; }
  .status.building { color: #e3b341; }
  .status.idle { color: #787e8a; }
  .status.blocked { color: #f85149; }
  .status.waiting { color: #f0883e; font-weight: 600; }
  .row1 { display: flex; align-items: center; }
  .ptitle { font-weight: 600; color: #ecedf3; margin-left: 8px; flex: 1 1 auto; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .ptitle.empty-title { color: #5a5f6b; font-weight: 400; font-style: italic; }
  .prompt { margin-top: 7px; color: #c9cedb; word-break: break-word; }
  .prompt::before { content: "▸ "; color: #5a8ce6; }
  .reply { margin-top: 3px; color: #8b91a0; font-size: 12px; word-break: break-word; }
  .intent { margin-top: 4px; font-size: 11px; color: #5a5f6b; word-break: break-word; }
  .files { margin-top: 3px; font-size: 11px; color: #787e8a; word-break: break-all; }
  .empty { color: #787e8a; font-size: 12px; padding: 8px 2px; }
  .err { color: #f85149; font-size: 12px; margin-top: 10px; }
</style>
</head>
<body>
  <div class="title">작업 현황</div>
  <div id="list"></div>
  <div id="err" class="err" style="display:none"></div>
<script>
const PORT = "__PORT__";
const base = "http://127.0.0.1:" + PORT;
const $ = (id) => document.getElementById(id);

function esc(s) { return (s || "").replace(/[&<>"]/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;'}[c])); }
function leaf(p) { const i = p.lastIndexOf('/'); return i >= 0 ? p.slice(i + 1) : p; }
// 워커 색 — pane id 숫자(%5 → 5) 기반. 고정 팔레트·
// 같은 식이라 헤더 색과 board 카드 색이 일치한다.
function workerColor(sid) {
  const palette = ['#5B9BD5','#70AD47','#C00000','#7030A0','#ED7D31','#1F9D8E','#E84393'];
  const num = parseInt((sid || "").replace(/\D/g, "")) || 0;
  return palette[num % palette.length];
}
function fmtTok(n) { return n >= 1000 ? (n / 1000).toFixed(n >= 10000 ? 0 : 1) + "k" : "" + n; }

function render(board) {
  $("err").style.display = "none";
  const list = $("list");
  list.innerHTML = "";
  if (!board || !board.length) {
    const e = document.createElement("div");
    e.className = "empty"; e.textContent = "활성 pane 없음";
    list.appendChild(e);
    return;
  }
  board.forEach(p => {
    const st = (p.status || "").toLowerCase();
    const files = (p.files || []).map(leaf).join(", ");
    // "waiting" = claude blocked on a permission/input prompt. The transcript
    // can't see this, so it's the one status worth flagging hard.
    const statusLabel = st === "waiting"
      ? "⚠ 권한 대기중" + (p.waiting_for ? ` (${esc(p.waiting_for)})` : "")
      : esc(p.status || "");
    const d = document.createElement("div");
    d.className = "pane";
    d.style.borderLeftColor = workerColor(p.surface_id);
    const title = p.title
      ? `<span class="ptitle">${esc(p.title)}</span>`
      : `<span class="ptitle empty-title">제목 없음</span>`;
    const intent = (p.intent && p.intent !== "active")
      ? `<div class="intent">${esc(p.intent)}</div>` : "";
    // P3 — tail 윈도 누적: 토큰/비용/변경파일/도구 뱃지.
    const tot = (p.tokens_in || 0) + (p.tokens_out || 0);
    const changed = (p.changed_files || []).length;
    const cost = p.cost_usd || 0;
    const bg = [];
    if (tot) bg.push(`<span class="badge tok">${fmtTok(tot)} tok</span>`);
    if (cost) bg.push(`<span class="badge cost">$${cost < 0.01 ? cost.toFixed(4) : cost.toFixed(2)}</span>`);
    if (changed) bg.push(`<span class="badge chg">변경 ${changed}</span>`);
    const badges = bg.length ? `<div class="badges">${bg.join("")}</div>` : "";
    const tools = (p.tool_counts || []).map(t => `${esc(t[0])}×${t[1]}`).join(" · ");
    const toolsDiv = tools ? `<div class="tools">${tools}</div>` : "";
    d.innerHTML =
      `<div class="row1"><span class="sid">${esc(p.surface_id)}</span>` +
      title +
      `<span class="status ${esc(st)}">${statusLabel}</span></div>` +
      (p.last_prompt ? `<div class="prompt">${esc(p.last_prompt)}</div>` : "") +
      (p.last_reply ? `<div class="reply">${esc(p.last_reply)}</div>` : "") +
      intent +
      (files ? `<div class="files">${esc(files)}</div>` : "") +
      badges + toolsDiv;
    list.appendChild(d);
  });
}

async function poll() {
  try {
    const r = await fetch(base + "/board", { cache: "no-store" });
    render((await r.json()).board);
  } catch (e) {
    $("err").style.display = "block";
    $("err").textContent = "server unreachable :" + PORT;
  }
}

poll();
setInterval(poll, 1000);
</script>
</body>
</html>"#;

/// Minimal HTML-escape for text dropped into a preview page's markup
/// (the filename shown in the title strip). Covers the five characters
/// that would otherwise break out of text content / an attribute.
#[allow(dead_code)] // used by IMAGE_VIEWER_HTML / MARKDOWN_EDITOR_HTML preview path (wry webview spike, kept for future)
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Image-viewer window page. `__NAME__` is the filename (title strip),
/// `__SRC__` a self-contained `data:` URI of the image bytes (injected at
/// open time, so the page is fully offline). Fit-to-window by default;
/// clicking the image toggles 1:1 actual size with scroll-to-pan.
#[allow(dead_code)]
const IMAGE_VIEWER_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>
  :root { color-scheme: dark; }
  * { box-sizing: border-box; }
  html, body { height: 100%; margin: 0; }
  body {
    display: flex; flex-direction: column;
    background: #1a1d23; color: #ecedf3;
    font: 12px/1.4 -apple-system, "SF Pro Text", system-ui, sans-serif;
    -webkit-user-select: none; user-select: none;
  }
  .bar {
    display: flex; align-items: center; gap: 10px;
    padding: 8px 12px; background: #101217; border-bottom: 1px solid #101217;
    flex: 0 0 auto;
  }
  .name { font-weight: 600; white-space: nowrap; overflow: hidden;
    text-overflow: ellipsis; }
  .dim { color: #787e8a; margin-left: auto; }
  .stage {
    flex: 1 1 auto; overflow: auto; display: flex;
    align-items: center; justify-content: center; padding: 12px;
  }
  /* checkerboard so transparent PNGs read clearly */
  .stage {
    background-image:
      linear-gradient(45deg, #20242c 25%, transparent 25%),
      linear-gradient(-45deg, #20242c 25%, transparent 25%),
      linear-gradient(45deg, transparent 75%, #20242c 75%),
      linear-gradient(-45deg, transparent 75%, #20242c 75%);
    background-size: 20px 20px;
    background-position: 0 0, 0 10px, 10px -10px, -10px 0;
  }
  img { display: block; }
  img.fit { max-width: 100%; max-height: 100%; object-fit: contain; cursor: zoom-in; }
  img.actual { max-width: none; max-height: none; cursor: zoom-out; }
</style>
</head>
<body>
  <div class="bar">
    <span class="name">__NAME__</span>
    <span class="dim" id="hint">클릭: 원본 크기 ↔ 맞춤</span>
  </div>
  <div class="stage" id="stage">
    <img id="img" class="fit" src="__SRC__" alt="__NAME__">
  </div>
<script>
  const img = document.getElementById('img');
  img.addEventListener('click', () => {
    if (img.classList.contains('fit')) {
      img.classList.remove('fit'); img.classList.add('actual');
    } else {
      img.classList.remove('actual'); img.classList.add('fit');
    }
  });
</script>
</body>
</html>"#;

/// Markdown editor/preview window page. `__NAME__` filename, `__PATH__` the
/// absolute path (JSON string), `__CONTENT__` the file text (JSON string),
/// `__PORT__` the MCP server port for the save POST. Split textarea + live
/// preview rendered by a tiny inline parser (no CDN, works offline). Save
/// POSTs `{path, content}` as text/plain (a CORS "simple" request — no
/// preflight, same trick the git-commit panel uses).
#[allow(dead_code)]
const MARKDOWN_EDITOR_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>
  :root { color-scheme: dark; }
  * { box-sizing: border-box; }
  html, body { height: 100%; margin: 0; }
  body {
    display: flex; flex-direction: column;
    background: #1a1d23; color: #ecedf3;
    font: 13px/1.5 -apple-system, "SF Pro Text", system-ui, sans-serif;
  }
  .bar {
    display: flex; align-items: center; gap: 10px;
    padding: 8px 12px; background: #101217; flex: 0 0 auto;
  }
  .name { font-weight: 600; white-space: nowrap; overflow: hidden;
    text-overflow: ellipsis; }
  .actions { margin-left: auto; display: flex; align-items: center; gap: 10px; }
  #status { font-size: 11px; color: #787e8a; }
  #status.ok { color: #3fb950; }
  #status.bad { color: #f85149; }
  button {
    background: #238636; border: 1px solid #2ea043; color: #fff;
    border-radius: 6px; padding: 5px 12px; font-size: 12px; cursor: pointer;
  }
  button:hover:not(:disabled) { background: #2ea043; }
  button:disabled { opacity: .5; cursor: default; }
  .split { flex: 1 1 auto; display: flex; min-height: 0; }
  .pane { flex: 1 1 50%; min-width: 0; overflow: auto; }
  .pane.edit { border-right: 1px solid #101217; }
  textarea {
    width: 100%; height: 100%; resize: none; border: 0; outline: 0;
    background: #1a1d23; color: #ecedf3; padding: 14px;
    font: 13px/1.6 ui-monospace, "SF Mono", Menlo, monospace;
    -webkit-user-select: text; user-select: text;
  }
  .preview { padding: 14px 18px; }
  .preview h1, .preview h2, .preview h3 { line-height: 1.25; margin: 1em 0 .5em; }
  .preview h1 { font-size: 1.7em; border-bottom: 1px solid #22262e; padding-bottom: .25em; }
  .preview h2 { font-size: 1.4em; border-bottom: 1px solid #22262e; padding-bottom: .2em; }
  .preview h3 { font-size: 1.15em; }
  .preview p { margin: .6em 0; }
  .preview a { color: #5a8ce6; }
  .preview code {
    background: #101217; border-radius: 5px; padding: .12em .4em;
    font: .9em ui-monospace, Menlo, monospace;
  }
  .preview pre {
    background: #101217; border-radius: 9px; padding: 10px 12px; overflow-x: auto;
  }
  .preview pre code { background: none; padding: 0; }
  .preview blockquote {
    margin: .6em 0; padding: .2em .9em; border-left: 3px solid #2e323b; color: #a0a6b0;
  }
  .preview ul, .preview ol { padding-left: 1.4em; margin: .5em 0; }
  .preview img { max-width: 100%; }
  .preview hr { border: 0; border-top: 1px solid #22262e; margin: 1.2em 0; }
  .preview table { border-collapse: collapse; }
  .preview td, .preview th { border: 1px solid #22262e; padding: 4px 8px; }
</style>
</head>
<body>
  <div class="bar">
    <span class="name">__NAME__</span>
    <span class="actions">
      <span id="status"></span>
      <button id="save">저장</button>
    </span>
  </div>
  <div class="split">
    <div class="pane edit"><textarea id="src" spellcheck="false"></textarea></div>
    <div class="pane"><div class="preview" id="preview"></div></div>
  </div>
<script>
  const PORT = "__PORT__";
  const PATH = __PATH__;
  const INITIAL = __CONTENT__;
  const src = document.getElementById('src');
  const preview = document.getElementById('preview');
  const status = document.getElementById('status');
  const saveBtn = document.getElementById('save');

  function esc(s) {
    return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
  }
  // Inline spans: code, bold, italic, links, images. `code` first so its
  // contents aren't re-parsed for emphasis.
  function inline(s) {
    s = esc(s);
    s = s.replace(/`([^`]+)`/g, (_, c) => '<code>' + c + '</code>');
    s = s.replace(/!\[([^\]]*)\]\(([^)\s]+)\)/g, '<img alt="$1" src="$2">');
    s = s.replace(/\[([^\]]+)\]\(([^)\s]+)\)/g, '<a href="$2">$1</a>');
    s = s.replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>');
    s = s.replace(/__([^_]+)__/g, '<strong>$1</strong>');
    s = s.replace(/\*([^*]+)\*/g, '<em>$1</em>');
    s = s.replace(/_([^_]+)_/g, '<em>$1</em>');
    return s;
  }
  // Block-level: fenced code, headings, hr, blockquote, lists, paragraphs.
  function render(md) {
    const lines = md.replace(/\r\n/g, '\n').split('\n');
    let html = '', i = 0;
    while (i < lines.length) {
      let line = lines[i];
      if (/^```/.test(line)) {
        let body = []; i++;
        while (i < lines.length && !/^```/.test(lines[i])) { body.push(lines[i]); i++; }
        i++;
        html += '<pre><code>' + esc(body.join('\n')) + '</code></pre>';
        continue;
      }
      let h = line.match(/^(#{1,6})\s+(.*)$/);
      if (h) { const n = h[1].length; html += '<h' + n + '>' + inline(h[2]) + '</h' + n + '>'; i++; continue; }
      if (/^\s*([-*_])\s*\1\s*\1[\s\1]*$/.test(line)) { html += '<hr>'; i++; continue; }
      if (/^\s*>/.test(line)) {
        let body = [];
        while (i < lines.length && /^\s*>/.test(lines[i])) { body.push(lines[i].replace(/^\s*>\s?/, '')); i++; }
        html += '<blockquote>' + render(body.join('\n')) + '</blockquote>';
        continue;
      }
      if (/^\s*[-*+]\s+/.test(line)) {
        let items = [];
        while (i < lines.length && /^\s*[-*+]\s+/.test(lines[i])) { items.push(lines[i].replace(/^\s*[-*+]\s+/, '')); i++; }
        html += '<ul>' + items.map(t => '<li>' + inline(t) + '</li>').join('') + '</ul>';
        continue;
      }
      if (/^\s*\d+\.\s+/.test(line)) {
        let items = [];
        while (i < lines.length && /^\s*\d+\.\s+/.test(lines[i])) { items.push(lines[i].replace(/^\s*\d+\.\s+/, '')); i++; }
        html += '<ol>' + items.map(t => '<li>' + inline(t) + '</li>').join('') + '</ol>';
        continue;
      }
      if (line.trim() === '') { i++; continue; }
      let para = [];
      while (i < lines.length && lines[i].trim() !== '' && !/^(#{1,6}\s|```|\s*>|\s*[-*+]\s|\s*\d+\.\s)/.test(lines[i])) {
        para.push(lines[i]); i++;
      }
      html += '<p>' + inline(para.join('\n')).replace(/\n/g, '<br>') + '</p>';
    }
    return html;
  }
  function refresh() { preview.innerHTML = render(src.value); }

  src.value = INITIAL;
  refresh();
  let dirty = false;
  src.addEventListener('input', () => { refresh(); dirty = true; status.textContent = '편집됨'; status.className = ''; });

  async function save() {
    saveBtn.disabled = true; status.textContent = '저장 중…'; status.className = '';
    try {
      const r = await fetch('http://127.0.0.1:' + PORT + '/save-markdown', {
        method: 'POST', headers: { 'Content-Type': 'text/plain' },
        body: JSON.stringify({ path: PATH, content: src.value }),
      });
      const j = await r.json();
      if (j.ok) { status.textContent = '저장됨'; status.className = 'ok'; dirty = false; }
      else { status.textContent = j.error || '저장 실패'; status.className = 'bad'; }
    } catch (e) {
      status.textContent = '저장 실패: ' + e; status.className = 'bad';
    } finally { saveBtn.disabled = false; }
  }
  saveBtn.addEventListener('click', save);
  window.addEventListener('keydown', (e) => {
    if ((e.metaKey || e.ctrlKey) && e.key === 's') { e.preventDefault(); save(); }
  });
</script>
</body>
</html>"#;

/// Cell width / height / baseline in logical pixels. Filled at startup
/// from `Sugarloaf::compute_cell_metrics` so columns align with the
/// actual font advance instead of a hardcoded guess. Falls back to a
/// reasonable default before the first measurement lands.
#[derive(Copy, Clone, Debug)]
struct CellGeom {
    w: f32,
    h: f32,
    #[allow(dead_code)]
    baseline: f32,
}

impl Default for CellGeom {
    fn default() -> Self {
        Self { w: 8.6, h: 18.0, baseline: 14.0 }
    }
}

/// (col, row) anchor + end for drag selection. Both ends in cell units.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct Selection {
    anchor: (u16, u16),
    end: (u16, u16),
}

/// Snapshot of every field `paint_gpu_overlays` reads. Built before
/// we hand a `&mut gpu::GpuRenderer` to the painter so the borrow
/// checker sees the snapshot and the mutable borrow as independent.
#[allow(dead_code)] // some fields (font_size) are snapshot-only, never read after construction
struct GpuOverlay {
    cell_w: f32,
    cell_h: f32,
    pad_x: f32,
    pad_y: f32,
    cursor_row: u16,
    cursor_col: u16,
    cursor_visible: bool,
    cols: u16,
    blink_on: bool,
    preedit: String,
    /// Where the preedit box anchors. Resolved via find_prompt_anchor so
    /// a TUI (Claude Code) that parks its cursor on a statusline still
    /// gets the composing Hangul drawn on the prompt row. Mirrors the
    /// sugarloaf render_frame path.
    preedit_row: u16,
    preedit_col: u16,
    font_size: f32,
    /// Active pane's font multiplier (pane-local zoom). The cursor /
    /// preedit / selection / ghost overlays must scale their cell size
    /// by this — `cell_w`/`cell_h` are the base (1.0) metrics, so a
    /// zoomed pane would otherwise anchor them on the un-zoomed grid and
    /// drift the composing Hangul off the prompt cell.
    font_scale: f32,
    selection: Option<Selection>,
    /// Inline autosuggestion ghost text (empty = none). Drawn dim,
    /// starting at the cursor cell, clipped to the row's right edge.
    suggestion: String,
}

/// Normalise so (start.row, start.col) <= (end.row, end.col) in reading
/// order. Used both for highlight rendering and clipboard extraction.
fn normalise(sel: Selection) -> ((u16, u16), (u16, u16)) {
    let a = sel.anchor;
    let b = sel.end;
    if (a.1, a.0) <= (b.1, b.0) {
        (a, b)
    } else {
        (b, a)
    }
}

/// Append a run of grid cells to `out` as text, dropping the blank
/// "spacer" cell that trails every full-width (CJK) glyph. The grid
/// stores a wide character as TWO cells — the glyph in the first, an
/// empty placeholder in the second — so a naive copy emits a stray space
/// after each syllable ("한글" → "한 글"). We peek: a cell right after a
/// wide char is its spacer and gets skipped. Genuine blanks (empty cell
/// not following a wide char) still render as a single space.
fn append_cells_text<'a>(cells: impl IntoIterator<Item = &'a GridCell>, out: &mut String) {
    let mut skip_spacer = false;
    for cell in cells {
        if skip_spacer {
            skip_spacer = false;
            // The spacer is blank by construction; only swallow it when it
            // really is empty/space so a glitch never eats real text.
            if cell.ch == '\0' || cell.ch == ' ' {
                continue;
            }
        }
        if cell.ch == '\0' {
            out.push(' ');
        } else {
            out.push(cell.ch);
            skip_spacer = gpu::is_wide_char(cell.ch);
        }
    }
}

/// Most recent scrollback lines to persist per pane on exit. Caps the saved
/// session file so a pane with a huge history doesn't bloat session.json.
const SCROLLBACK_SAVE_MAX: usize = 500;

/// 자동 스냅샷 주기. 강제 종료 시 잃는 최대치가 이 값이다. 짧을수록 안전하지만
/// pane 마다 500줄을 직렬화하므로, 체감되지 않으면서 손실이 충분히 작은 값으로.
pub(crate) const SESSION_AUTOSAVE_PERIOD: std::time::Duration = std::time::Duration::from_secs(5);

/// Capture a pane's scrollback (history + current screen) as trimmed text
/// lines for session restore, newest-biased: keeps the last SCROLLBACK_SAVE_MAX
/// lines and drops the trailing blank rows so a restored pane doesn't carry an
/// empty tail. v1 saves text only (color/attrs dropped) — the content is what
/// "what I typed/saw is still there" needs.
fn scrollback_lines(pane: &PaneState) -> Vec<String> {
    let Some(t) = pane.term() else {
        return Vec::new();
    };
    let mut lines: Vec<String> = Vec::new();
    for row in t.history.iter().chain(t.cells.iter()) {
        let mut s = String::new();
        append_cells_text(row.iter(), &mut s);
        lines.push(s.trim_end().to_string());
    }
    while lines.last().map_or(false, |l| l.is_empty()) {
        lines.pop();
    }
    if lines.len() > SCROLLBACK_SAVE_MAX {
        lines.drain(0..lines.len() - SCROLLBACK_SAVE_MAX);
    }
    lines
}

/// Pull the selected text out of the visible row grid. Joined with `\n`,
/// trailing spaces trimmed per row. Mirrors kasaterm::extract_selection.
fn extract_selection(rows: &[Vec<GridCell>], sel: Selection) -> String {
    let (start, end) = normalise(sel);
    let mut out = String::new();
    for (r, row) in rows.iter().enumerate() {
        let r = r as u16;
        if r < start.1 || r > end.1 {
            continue;
        }
        let (cs, ce) = if start.1 == end.1 {
            (start.0 as usize, end.0 as usize)
        } else if r == start.1 {
            (start.0 as usize, row.len().saturating_sub(1))
        } else if r == end.1 {
            (0, end.0 as usize)
        } else {
            (0, row.len().saturating_sub(1))
        };
        append_cells_text(row.iter().take(ce + 1).skip(cs), &mut out);
        while out.ends_with(' ') {
            out.pop();
        }
        out.push('\n');
    }
    if out.ends_with('\n') {
        out.pop();
    }
    // Trim blank lines at both ends. A drag across an alt-screen TUI
    // (Claude Code, less, etc.) usually picks up empty padding rows
    // above/below the visible text — strip those so what lands in the
    // clipboard matches what the user actually highlighted.
    let trimmed: Vec<&str> = out
        .lines()
        .skip_while(|l| l.trim().is_empty())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .skip_while(|l| l.trim().is_empty())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    trimmed.join("\n")
}

/// True for any codepoint the Hangul IME composes. Covers the four
/// Unicode blocks that hold Korean syllables and jamo. Used to drop
/// keyboard-side Character events when the IME is the authoritative
/// channel — winit emits both `KeyboardInput.text` and `Ime::Preedit`
/// for the first keystroke after a script switch, and forwarding both
/// would echo the jamo twice (or, more often, leak the first 자모 to
/// the shell as raw `ㅎ` / `ㅇ` before the Commit lands).
fn is_hangul_codepoint(c: char) -> bool {
    let cp = c as u32;
    (0x1100..=0x11FF).contains(&cp) // Hangul Jamo
        || (0x3130..=0x318F).contains(&cp) // Hangul Compat Jamo
        || (0xA960..=0xA97F).contains(&cp) // Hangul Jamo Extended-A
        || (0xAC00..=0xD7A3).contains(&cp) // Hangul Syllables
        || (0xD7B0..=0xD7FF).contains(&cp) // Hangul Jamo Extended-B
}

/// Wheel accumulator. Returns Some(lines) when an emit fires, None while
/// accumulating sub-cell ticks or while the throttle window is open.
/// Mirrors kasaterm-cli's wheel_step semantics exactly.
fn wheel_step(
    accum: &mut f32,
    dy_cells: f32,
    last_emit: &mut Instant,
    now: Instant,
) -> Option<i32> {
    if accum.signum() != dy_cells.signum() && *accum != 0.0 && dy_cells != 0.0 {
        *accum = 0.0;
    }
    *accum += dy_cells;
    let lines = accum.trunc() as i32;
    if lines == 0 {
        return None;
    }
    if now.duration_since(*last_emit) < std::time::Duration::from_millis(wheel_throttle_ms()) {
        return None;
    }
    *accum -= lines as f32;
    *last_emit = now;
    Some(lines)
}

/// Per-pane render state. One of these per tmux pane (`%N`). Holds the
/// cell grid, scrollback ring, cursor, and the flags we need to route
/// wheel events correctly (alt-screen / SGR mouse mode are per-pane in
/// real terminals — claude in pane 0 can be in alt-screen while a
/// shell prompt sits in pane 1).
/// Direction for spatial pane focus / swap (Cmd+Option+Arrow).
#[derive(Clone, Copy)]
enum FocusDir {
    Left,
    Right,
    Up,
    Down,
}

/// Which edge of the drop-target pane the cursor is over during a header
/// drag. Determines where the dragged pane lands: a Left/Right drop
/// splits horizontally, Up/Down splits vertically. `Center` means the
/// cursor sits in the inner 50% square — drop merges the tab into the
/// target pane's tab strip instead of splitting.
#[derive(Clone, Copy, PartialEq, Debug)]
enum DropZone {
    Left,
    Right,
    Up,
    Down,
    Center,
}

/// State for an in-flight header drag-and-drop relocation.
struct HeaderDrag {
    /// Pane being dragged (its tree leaf id).
    pane: String,
    /// Press position in logical px, to measure the click→drag threshold.
    start: (f32, f32),
    /// True once the cursor moved far enough to count as a drag rather
    /// than a click.
    active: bool,
    /// True when the drag began on a ⋮ handle (header-less pane). A release
    /// under the drag threshold then toggles that pane's handle menu; a
    /// header-bar drag just focuses.
    from_handle: bool,
}

/// Image-pane action button kinds, drawn in place of the terminal-action
/// cluster on the right side of an image pane's header.
#[derive(Clone, Copy, PartialEq)]
enum ImageBtn {
    ZoomOut,
    ZoomIn,
    Rotate,
    Reset,
}

/// What a confirmed close-dialog should actually close.
#[derive(Clone)]
enum PendingClose {
    /// Drop one tab of a multi-tab pane.
    Tab { pane: String, idx: usize },
    /// Drop a whole pane (its last tab).
    Pane { pane: String },
    /// Close one sidebar session (window `idx`) — the app stays open. Distinct
    /// from `Window` (whole-app quit): only this session's panes are killed.
    Session(usize),
    /// Quit the app (window red-light / Cmd+W on the last pane).
    Window,
    /// Close a popped-out editor window. Identified by window id, not by index:
    /// `aux_windows` is a Vec and another window closing shifts every index
    /// after it, so a stored index can point at the wrong window by the time
    /// the dialog resolves.
    AuxEditor(winit::window::WindowId),
}

/// Where one unsaved editor lives, so the dialog can go back and save it (or
/// drop its changes) once the user has decided.
#[derive(Clone)]
enum DirtyDoc {
    /// Tab `tab` of pane `pane` in the main window.
    Tab { pane: String, tab: usize },
    /// A popped-out editor window.
    Aux(winit::window::WindowId),
}

/// Why a close is being held up.
#[derive(Clone)]
enum CloseWhy {
    /// A real foreground job is running — the process name, for the message.
    Busy(String),
    /// Editors with unsaved changes: where each one is, and its file name.
    Dirty(Vec<(DirtyDoc, String)>),
}

/// A pending close confirmation: `why` it was raised, `action` is what
/// proceeding actually closes.
#[derive(Clone)]
struct ConfirmClose {
    why: CloseWhy,
    action: PendingClose,
}

/// Buttons in the confirm-close modal. A busy dialog shows 취소/닫기; an
/// unsaved-changes dialog shows 취소/저장 안 함/저장 — `Close` is the
/// "proceed without saving" button in both.
#[derive(Clone, Copy, PartialEq)]
enum ConfirmBtn {
    Cancel,
    Close,
    Save,
}

/// The two buttons in the Chrome-style session-restore prompt shown at launch
/// when a saved layout is found.
#[derive(Clone, Copy, PartialEq)]
enum RestoreBtn {
    /// Rebuild the saved layout and resume each pane's claude session.
    Restore,
    /// Discard the saved state and keep the fresh single-pane session.
    Fresh,
}

/// A login shell (vs. a real foreground job like claude / vim / a build).
/// Closing a pane whose foreground is just a shell needs no confirmation.
fn is_shell_name(name: &str) -> bool {
    let base = name.strip_prefix('-').unwrap_or(name);
    matches!(base, "zsh" | "bash" | "fish" | "sh" | "dash" | "tcsh" | "ksh")
}

/// Terminal-pane action button kinds, painted on the right side of a
/// terminal pane's header (split-v / split-h). New-terminal and web were
/// dropped — the +button covers "new shell" and the web overlay added
/// complexity for little payoff. Wired to per-frame `pane_action_hits`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActionKind {
    SplitV,
    SplitH,
    /// Toggle this pane's bottom status bar (cwd / branch / diff). Lives next to
    /// the split buttons; collapsing the bar gives the cell grid its rows back.
    ToggleStatusbar,
    /// Markdown panes only: the "Rendered | Raw" header segmented toggle.
    /// `MdRender` shows the laid-out doc; `MdRaw` opens the wgpu source editor
    /// (and `MdRender` from Raw writes the buffer back to disk + re-parses).
    MdRender,
    MdRaw,
    /// ghostty ⋮ 메뉴의 "닫기" — 이 pane을 닫는다.
    Close,
    /// ··· 메뉴의 "새 탭" — 이 pane(outer)에 in-pane 탭을 추가한다. 탭이 둘
    /// 이상이 되면 has_header()가 켜져 탭 스트립이 보인다.
    NewTab,
    /// ⋮ 메뉴의 상단바(pane 헤더) 토글. 하단 상태바 토글과 대칭 — 헤더는 지금까지
    /// 탭 여러 개·이미지·md 일 때만 자동으로 떴고 사용자가 켤 방법이 없었다.
    ToggleHeader,
    /// ⋮ 메뉴의 최대화 — 헤더/탭 더블클릭과 같은 tmux 식 줌. 제스처를 모르거나
    /// 더블클릭이 안 잡히는 경우를 위한 보이는 경로.
    ToggleZoom,
    /// ⋮ 메뉴의 화면 새로고침 — Cmd+Shift+R 과 같은 `refresh_renderer()`.
    /// pane 스코프 메뉴에 있지만 동작은 창 전체다. 모니터를 옮겨 화면이 깨졌을 때
    /// 단축키를 모르는 채로도 닿을 수 있는 자리가 필요했다.
    RefreshRenderer,
}

/// 타이틀바 사용량 pill 드롭다운의 한 줄.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum AccountMenuItem {
    /// 이 계정으로 전환. 빈 문자열 = 기본 로그인(env 를 아예 안 붙임).
    Select(String),
    /// 설정 → Claude 로 보낸다. 계정이 하나뿐일 때 드롭다운이 막다른 골목이
    /// 되지 않게 항상 맨 아래에 둔다 — 실제 추가는 거기서 /login 까지 간다.
    AddInSettings,
}

/// State for an in-flight in-pane tab reorder drag. A press on a tab arms
/// this; it only becomes a real drag past the threshold, so a plain press
/// just switches the active tab on release.
struct TabDrag {
    /// Pane whose tab strip owns the drag.
    pane: String,
    /// Index of the grabbed tab at press time.
    from: usize,
    /// Press position in logical px, for the click→drag threshold.
    start: (f32, f32),
    /// True once the cursor moved past the threshold.
    active: bool,
    /// Current insertion index (0..=tabs.len()) the tab would drop into.
    target: usize,
    /// Pane the cursor is currently hovering. Equals `pane` for in-place
    /// reorder; differs when the user dragged onto another pane's tab strip
    /// — release commits a cross-pane move into `drop_pane.tabs[target]`.
    drop_pane: String,
}

/// In-flight file-tree → terminal drag. Armed on a press over a tree row;
/// once the cursor moves past the threshold (`active`), releasing over a
/// terminal pane types that file's path into the dropped-on shell. A press
/// that never moves stays a plain click (the row's expand/preview already
/// fired on press), so this is take-and-ignore on release.
struct FileTreeDrag {
    /// Absolute path of the grabbed row.
    path: std::path::PathBuf,
    /// Press position in logical px, for the click→drag threshold.
    start: (f32, f32),
    /// True once the cursor moved past the threshold.
    active: bool,
}

/// A terminal pane's screen state: the PTY-backed cell grid, cursor,
/// scrollback, and the modes the emulator reports. Lives inside
/// `PaneContent::Terminal`.
#[derive(Default)]
struct TerminalPane {
    rows: u16,
    cols: u16,
    cells: Vec<Vec<GridCell>>,
    cursor_row: u16,
    cursor_col: u16,
    cursor_visible: bool,
    alt_screen: bool,
    mouse_enabled: bool,
    mouse_sgr: bool,
    /// DECCKM (application cursor keys). When true, plain arrows go out
    /// as SS3 (`ESC O A`) instead of CSI (`ESC [ A`) so claude code /
    /// vim line navigation works. See the arrow-key send path.
    app_cursor: bool,
    /// DECSET 2004 (bracketed paste). Only when the app turned this on may a
    /// paste be wrapped in `ESC[200~ … ESC[201~` — otherwise those bytes land
    /// in the app's input buffer as if typed.
    bracketed_paste: bool,
    history: VecDeque<Vec<GridCell>>,
    /// Scrollback offset in rows. `0` = live tail; positive = N rows
    /// back into history visible at the top.
    scroll_offset: usize,
    /// Cached previous cells used by the shift-detection heuristic that
    /// promotes scrolled-off rows into `history`.
    prev_cells: Vec<Vec<GridCell>>,
    /// Last OSC 133 `B` mark (prompt end / command-input start), (row, col).
    prompt_end: Option<(u16, u16)>,
}

/// A markdown pane's state: the parsed doc plus the Raw editor buffer/cursor.
struct MarkdownPane {
    doc: Arc<MarkdownDoc>,
    /// true only for actual `.md`/`.markdown` files. Code/text files reuse this
    /// pane as a plain Raw editor but get no "Rendered | Raw" header toggle —
    /// rendering a `.toml` as markdown would just mangle it.
    is_md_doc: bool,
    /// false = Render (laid-out view), true = Raw (wgpu text editor).
    raw_mode: bool,
    /// Raw-mode edit buffer, one entry per line.
    ///
    /// Shared, not owned: the renderer takes a copy every frame (it reads under
    /// the ws lock and draws after releasing it) and every undo snapshot takes
    /// another. As a plain `Vec` that was a full deep copy of the file each
    /// time — thousands of `String` allocations per frame on a big document.
    /// Behind an `Arc` those are pointer bumps, and `Arc::make_mut` (see
    /// `lines_mut`) pays for exactly one real copy per edit run: the first
    /// keystroke after a snapshot, which is the copy `push_undo` was making
    /// anyway.
    edit_lines: Arc<Vec<String>>,
    /// Edit cursor: line index + column in chars.
    cur_line: usize,
    cur_col: usize,
    /// Scroll offset in logical px (both Render and Raw). f32, not an integer:
    /// a trackpad frame carries sub-pixel tails, and rounding each one away
    /// made a slow swipe stall instead of glide.
    scroll: f32,
    /// Raw-mode horizontal scroll in logical px. Long code lines (checksums,
    /// URLs) overflow the pane — this pans the text under a fixed line-number
    /// gutter. 0 = flush left. Render mode ignores it (markdown wraps).
    h_scroll: f32,
    /// Buffer has edits not yet written to `doc.path`. Cleared by Cmd+S and
    /// the .md Raw→Render save; drives the ● unsaved dot on the tab label.
    modified: bool,
    /// When the buffer was last touched. Autosave waits for this to go quiet
    /// (VS Code's afterDelay), so a typing run writes once at the end instead
    /// of once per keystroke. Set with `touch`, cleared on save.
    edited_at: Option<Instant>,
    /// Selection anchor as (line, col) in chars; the cursor is the head, so
    /// anchor..cursor is the selection either direction. None = no selection.
    sel_anchor: Option<(usize, usize)>,
    /// Undo/redo: whole-buffer snapshots pushed at edit-run boundaries (start
    /// of a typing run / delete run, Enter, paste, selection replace). Whole
    /// snapshots are fine at this editor's file sizes and can't drift the way
    /// operation logs do. Capped at `markdown::UNDO_CAP`.
    undo_stack: Vec<EditSnapshot>,
    redo_stack: Vec<EditSnapshot>,
    /// Kind of the last mutation — consecutive same-kind edits (a typing run,
    /// a backspace run) coalesce into one undo unit; any caret move breaks it.
    last_edit: EditKind,
    /// Find/replace bar. Some == open, and while it is open it owns typing
    /// (Esc closes and hands the keyboard back to the buffer).
    find: Option<FindState>,
    /// 자동완성 팝업. Some == 열림, 그동안 ↑↓·Tab·Enter·Esc 를 팝업이 먼저 먹는다.
    complete: Option<CompleteState>,
    /// 가장 긴 줄의 칸 수 — 가로 스크롤 상한. `None` 이면 다음에 물어볼 때 다시
    /// 센다. 트랙패드 제스처는 프레임마다 상한을 묻는데, 캐시가 없으면 그때마다
    /// 버퍼 전체를 훑었다.
    longest_cache: Option<usize>,
    /// 긴 줄을 본문 폭에서 접어 내릴지. 끄면 가로로 스크롤해 본다.
    wrap: bool,
    /// 접힌 구간들 — `(머리 줄, 마지막 숨은 줄)`. 비어 있는 게 보통이고, 그때
    /// 화면 행 ↔ 버퍼 줄 변환은 전부 항등이라 비용이 0 이다.
    folds: crate::markdown::Folds,
    /// `folds` 를 마지막으로 검증한 버퍼 세대. 편집으로 블록 모양이 바뀌면
    /// 접힘이 엉뚱한 줄을 가리키므로 세대가 다를 때 한 번 훑어 걷어낸다.
    folds_gen: u64,
    /// 버퍼가 바뀔 때마다 오르는 세대 번호. LSP 재전송 디바운스가 "달라졌나"를
    /// 이 정수 하나로 판정한다 — 매 틱 5천 줄을 이어 붙여 해시하면 그 자체가
    /// 프레임 예산이고, 저장 플래그(`dirty`)는 Cmd+S 에 꺼져서 못 쓴다.
    edit_gen: u64,
}

/// 자동완성 팝업 상태.
///
/// 후보는 버퍼 안 낱말에서 만든다(`markdown::word_completions`). LSP 가 붙은
/// 뒤에도 이 목록은 남는다 — 서버 응답은 왕복이 있어서, 그 사이 한 프레임을
/// 이걸로 메워야 타이핑이 멈춘 것처럼 보이지 않는다.
struct CompleteState {
    /// 후보 목록. 캐럿에서 가까운 줄이 앞.
    items: Vec<String>,
    /// 고른 후보.
    sel: usize,
    /// 채워 넣을 낱말이 시작하는 열. 확정할 때 여기부터 캐럿까지를 후보로
    /// 갈아끼운다 — 캐럿 위치만 들고 있으면 이미 친 앞부분이 남아 `cocost`
    /// 처럼 겹쳐 들어간다.
    from_col: usize,
    /// 서버에 보낸 자동완성 요청 id. 응답은 비동기로 오므로, 도착한 것이 **이
    /// 팝업의** 답인지 이걸로 가린다. `None` = 버퍼 낱말만으로 채운 상태.
    lsp_req: Option<i64>,
}

/// 호버 툴팁 상태. 마우스가 편집기 위에서 **멎으면** 서버에 묻고, 답이 오면 그
/// 자리에 띄운다. 움직이는 동안 묻지 않는 이유는 지나가는 글자마다 요청을 쏘면
/// 서버가 취소·재시작만 반복하기 때문이다.
struct HoverState {
    /// 마우스가 멎은 화면 좌표(logical px).
    at: (f32, f32),
    /// 그 자리에 멎은 시각.
    since: std::time::Instant,
    /// 보낸 요청 id. `Some` 이면 답을 기다리는 중 — 같은 자리를 다시 묻지 않는다.
    req: Option<i64>,
    /// 서버가 준 글. 있으면 툴팁을 그린다.
    text: Option<String>,
}

/// Clickable control on the find bar. Every one has a keyboard equivalent —
/// the mouse is for the hand that's already there, not the only way in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FindBtn {
    /// Expand/collapse the replace row.
    ToggleReplace,
    Prev,
    Next,
    Close,
    ReplaceOne,
    ReplaceAll,
}

/// Find/replace bar state for one raw editor pane.
#[derive(Clone)]
struct FindState {
    query: String,
    replace: String,
    /// The replace row is showing (Cmd+Opt+F). Find-only otherwise.
    replacing: bool,
    /// Typing lands in the replace field rather than the query; Tab flips it.
    focus_replace: bool,
    /// Matches as (line, start col, end col) in chars, document order. Rebuilt
    /// when the query or buffer changes — the bar owns the keyboard, so the
    /// buffer only moves under it via replace.
    hits: Vec<(usize, usize, usize)>,
    /// Index of the highlighted match in `hits`; meaningless when empty.
    idx: usize,
}

/// One undo/redo unit for the raw editor: the full buffer + cursor.
#[derive(Clone)]
struct EditSnapshot {
    lines: Arc<Vec<String>>,
    cur: (usize, usize),
}

/// Coalescing class for `MarkdownPane::last_edit`. `Break` = no run in
/// progress (cursor moved, undo happened, pane just opened).
#[derive(Clone, Copy, PartialEq, Debug)]
enum EditKind {
    Break,
    Typing,
    Deleting,
    Other,
}

/// What a pane shows. Terminal panes drive a PTY + cell grid; image/markdown
/// panes are PTY-less and rendered directly with wgpu. Keeping these in an enum
/// stops terminal-assuming code (cursor, scrollback, PTY resize) from silently
/// touching non-terminal panes — the compiler forces a match. A future
/// `Browser(BrowserPane)` (webview overlay) slots in here too.
enum PaneContent {
    Terminal(TerminalPane),
    Image(Arc<ImagePane>),
    Markdown(MarkdownPane),
}

impl Default for PaneContent {
    fn default() -> Self {
        PaneContent::Terminal(TerminalPane::default())
    }
}

/// One tab inside a pane. Stage 3: each tab carries its own content + PTY pid +
/// title — switching tabs swaps which one drives the pane's header label,
/// terminal grid, and input routing. `pid` is the key into `App.pty`; `None`
/// for image / markdown tabs that have no shell behind them.
#[derive(Default)]
struct PaneTab {
    content: PaneContent,
    /// OSC 0/2 title — `printf '\e]0;hello\a'` from this tab's shell, or a
    /// pinned label. Falls back to the live process name in the header paint
    /// when None.
    title: Option<String>,
    /// Sticky-title flag (set by `surface.rename`): while pinned, OSC titles
    /// from the inner program are ignored.
    title_pinned: bool,
    /// Image-pane view state. `image_zoom < 1.0` clamps to fit; >= 1 zooms
    /// past native (image overflows the box, centered). `image_rot` is the
    /// number of 90° CW rotations applied (0..4). Ignored for non-image tabs.
    image_zoom: f32,
    image_rot: u8,
    /// Pan offset (logical px) of the zoomed image's center from the pane
    /// center, set by dragging the image body. Only has an effect while the
    /// image overflows its box (zoomed in); `queue_image` clamps it so the
    /// crop window never leaves the texture. 0,0 = centered.
    image_pan_x: f32,
    image_pan_y: f32,
    /// PtySession key in `App.pty` for terminal tabs. `None` for image /
    /// markdown tabs. The outer pane id (the layout-tree key) equals the
    /// pid of the *first* tab; secondary tabs have their own pid and a
    /// reverse-map entry in `Workspace.pid_to_pane`.
    pid: Option<String>,
    /// Daemon preview id when this tab is a host-attached image/markdown
    /// preview (`imgopen` inside this pane). The GUI reconciles these tabs
    /// against `view.pane_previews` each broadcast; closing the tab fires
    /// `close_surface(preview_id)` so the daemon drops it (else it resurrects).
    /// `None` for normal terminal / leaf tabs.
    preview_id: Option<String>,
    /// Source file path for a locally-opened image/markdown preview (file-tree
    /// double-click). Markdown also carries its path in `doc.path`, but image
    /// tabs have nowhere else to keep it; this single field lets the file-tree
    /// highlight + de-dup logic ask "which file is this tab showing" uniformly.
    preview_path: Option<std::path::PathBuf>,
}

impl PaneTab {
    fn term(&self) -> Option<&TerminalPane> {
        if let PaneContent::Terminal(t) = &self.content { Some(t) } else { None }
    }
    fn term_mut(&mut self) -> Option<&mut TerminalPane> {
        if let PaneContent::Terminal(t) = &mut self.content { Some(t) } else { None }
    }
    /// The visible screen as text — the last `lines` non-blank rows of the
    /// current grid (0 = all). Trailing blank rows are dropped so a peek of a
    /// half-empty screen doesn't return a wall of whitespace. Backs
    /// `surface.peek` / the board's `screen_lines` so a sibling can read what
    /// this pane is currently showing (a prompt, a menu, build output).
    pub(crate) fn visible_text(&self, lines: usize) -> String {
        let Some(t) = self.term() else {
            return String::new();
        };
        let mut out: Vec<String> = Vec::new();
        for row in t.cells.iter() {
            let mut s = String::new();
            append_cells_text(row.iter(), &mut s);
            out.push(s.trim_end().to_string());
        }
        while out.last().map_or(false, |l| l.is_empty()) {
            out.pop();
        }
        if lines > 0 && out.len() > lines {
            out.drain(0..out.len() - lines);
        }
        out.join("\n")
    }
    /// Same as `visible_text` but serializes cell fg/bg/attributes as ANSI SGR
    /// escape codes so a viewer can reproduce the terminal colors. Each row is
    /// trimmed to the last non-blank cell before encoding; trailing blank rows
    /// are dropped. Used by `GET /peek?ansi=1`.
    pub(crate) fn visible_text_ansi(&self, lines: usize) -> String {
        use kasa_bridge::screen::Color;
        let Some(t) = self.term() else { return String::new(); };

        fn render_row(row: &[GridCell]) -> String {
            let last_vis = row
                .iter()
                .rposition(|c| c.ch != ' ' && c.ch != '\0')
                .map_or(0, |i| i + 1);
            if last_vis == 0 {
                return String::new();
            }
            let row = &row[..last_vis];
            let mut s = String::new();
            let mut any_attr = false;
            let mut p_fg = Color::Default;
            let mut p_bg = Color::Default;
            let mut p_bold = false;
            let mut p_italic = false;
            let mut p_under = false;
            let mut p_dim = false;
            let mut p_inv = false;
            for cell in row {
                let ch = if cell.ch == '\0' { ' ' } else { cell.ch };
                if cell.fg != p_fg
                    || cell.bg != p_bg
                    || cell.bold != p_bold
                    || cell.italic != p_italic
                    || cell.underline != p_under
                    || cell.dim != p_dim
                    || cell.inverse != p_inv
                {
                    s.push_str("\x1b[0m");
                    if cell.bold { s.push_str("\x1b[1m"); }
                    if cell.dim { s.push_str("\x1b[2m"); }
                    if cell.italic { s.push_str("\x1b[3m"); }
                    if cell.underline { s.push_str("\x1b[4m"); }
                    if cell.inverse { s.push_str("\x1b[7m"); }
                    match &cell.fg {
                        Color::Default => {}
                        Color::Idx(n) => s.push_str(&format!("\x1b[38;5;{n}m")),
                        Color::Rgb(r, g, b) => s.push_str(&format!("\x1b[38;2;{r};{g};{b}m")),
                    }
                    match &cell.bg {
                        Color::Default => {}
                        Color::Idx(n) => s.push_str(&format!("\x1b[48;5;{n}m")),
                        Color::Rgb(r, g, b) => s.push_str(&format!("\x1b[48;2;{r};{g};{b}m")),
                    }
                    p_fg = cell.fg.clone();
                    p_bg = cell.bg.clone();
                    p_bold = cell.bold;
                    p_italic = cell.italic;
                    p_under = cell.underline;
                    p_dim = cell.dim;
                    p_inv = cell.inverse;
                    any_attr = p_fg != Color::Default
                        || p_bg != Color::Default
                        || p_bold || p_italic || p_under || p_dim || p_inv;
                }
                s.push(ch);
            }
            if any_attr {
                s.push_str("\x1b[0m");
            }
            s
        }

        let mut rows: Vec<String> = t.cells.iter().map(|r| render_row(r)).collect();
        while rows.last().map_or(false, |l| l.is_empty()) {
            rows.pop();
        }
        if lines > 0 && rows.len() > lines {
            rows.drain(0..rows.len() - lines);
        }
        rows.join("\n")
    }

    fn markdown(&self) -> Option<&MarkdownPane> {
        if let PaneContent::Markdown(m) = &self.content { Some(m) } else { None }
    }
    fn markdown_mut(&mut self) -> Option<&mut MarkdownPane> {
        if let PaneContent::Markdown(m) = &mut self.content { Some(m) } else { None }
    }
    fn image_view_zoom(&self) -> f32 {
        if self.image_zoom < 1.0 { 1.0 } else { self.image_zoom }
    }
    fn image(&self) -> Option<&Arc<ImagePane>> {
        if let PaneContent::Image(i) = &self.content { Some(i) } else { None }
    }
}

struct PaneState {
    /// In-pane tabs. Always non-empty — single-tab panes have `tabs.len() == 1`.
    /// `active_tab` indexes the visually-active tab; its state is exposed via
    /// the `Deref`/`DerefMut` impls so the rest of the code can keep writing
    /// `pane.title`, `pane.content`, `pane.term()` as if the pane were single.
    tabs: Vec<PaneTab>,
    /// Index into `tabs` of the visually-active tab.
    active_tab: usize,
    /// Accent color for this pane's header band (RGBA). None = default.
    /// Pane-level, not per-tab — the band is shared above all tabs.
    color: Option<[u8; 4]>,
    /// Tab-strip overflow windowing: index of the first tab drawn in this
    /// pane's header (whole-tab run, no partial clipping). Stepped by the
    /// wheel; the render pass clamps and writes the effective value back.
    tab_first: usize,
    /// How many tabs the strip fit last frame — render-written, wheel-read.
    tab_vis: usize,
    /// `active_tab` as of the last frame. The render pass compares to spot
    /// a tab switch (from any of its many call sites) and auto-reveals the
    /// newly active tab, without touching free wheel scrolling.
    tab_last_active: usize,
    /// 배정된 캐릭터명(미도리 등). 진실 소스는 Workspace.pane_character,
    /// apply_screen_update 가 매 업데이트 동기. 타이틀바(render)가 claude 실행 중
    /// (agents --json)일 때만 이 이름을 그린다 — 작업 중 표시는 pane_activity 로딩바.
    character: Option<String>,
    /// Frame-dirty flag; cleared after the next render. When every pane is
    /// clean and no chrome anim is pending, the render loop skips the GPU pass.
    dirty: bool,
    /// ⋮ 메뉴의 상단바 토글. `None` = 자동(탭 여러 개·이미지·md 면 켬), `Some(b)` =
    /// 사용자가 직접 정함. 하단바처럼 App 쪽 HashSet 으로 두지 않고 pane 에 붙인
    /// 이유는 `has_header()`/`header_px()` 가 이미 pane 만 보고 답하기 때문이다 —
    /// 여기 두면 렌더·히트테스트·resize 의 모든 기존 호출부가 그대로 맞는다.
    /// 헤더 유무는 셀 그리드를 밀므로 세 곳이 어긋나면 클릭이 행 하나씩 밀린다.
    header_override: Option<bool>,
}

impl Default for PaneState {
    fn default() -> Self {
        Self {
            tabs: vec![PaneTab::default()],
            active_tab: 0,
            color: None,
            tab_first: 0,
            tab_vis: usize::MAX,
            tab_last_active: 0,
            character: None,
            dirty: false,
            header_override: None,
        }
    }
}

impl PaneState {
    /// 탭이 둘 이상이면 탭 스트립을 위해, 이미지·마크다운(.md)이면 전용 컨트롤을
    /// 위해 헤더 띠를 유지한다. 그 외 일반 터미널은 hover ⋮ 만 쓴다. image()/
    /// markdown()은 Deref로 active 탭을 본다.
    fn has_header(&self) -> bool {
        // 학생 헤더 띠 폐기(거노) — 학생 이름은 상단 타이틀바(claude 실행 시),
        // 로딩바는 pane 위 별도. 헤더 띠는 멀티탭·이미지·md 전용 컨트롤만 남긴다.
        // 코드/텍스트 raw 편집기도 헤더를 가진다 — 파일명 + ● 미저장 도트의 자리.
        // ⋮ 에서 직접 정했으면 그게 우선 — 탭이 하나인 터미널에도 띠를 띄워
        // 제목·pop-out 을 쓸 수 있고, md 처럼 자동으로 뜨는 pane 은 접을 수 있다.
        if let Some(forced) = self.header_override {
            return forced;
        }
        self.tabs.len() > 1
            || self.image().is_some()
            || self.markdown().is_some()
    }
    /// 셀 그리드를 아래로 미는 헤더 높이(logical px). render/layout 양쪽이
    /// 같은 값을 써야 PTY 그리드↔셀 클립이 어긋나지 않는다.
    fn header_px(&self) -> f32 {
        if self.has_header() { PANE_HEADER_HEIGHT } else { 0.0 }
    }
}

impl std::ops::Deref for PaneState {
    type Target = PaneTab;
    fn deref(&self) -> &PaneTab {
        // `tabs` is always non-empty (constructed via Default with 1 tab,
        // close keeps the last tab) — index is clamped on tab mutations.
        &self.tabs[self.active_tab.min(self.tabs.len() - 1)]
    }
}

impl std::ops::DerefMut for PaneState {
    fn deref_mut(&mut self) -> &mut PaneTab {
        let i = self.active_tab.min(self.tabs.len() - 1);
        &mut self.tabs[i]
    }
}

/// One frame of an image pane. `rgba` is tightly-packed RGBA8 (`w * h * 4`).
/// Static images (png/jpg/…) are a single frame with zero delay; animated
/// gifs carry one entry per frame with its inter-frame delay.
struct ImageFrame {
    rgba: Vec<u8>,
    delay: std::time::Duration,
}

/// A decoded image bound to a pane. Each frame is uploaded once into a wgpu
/// texture keyed by `(pane, rotation, frame)`. `cur`/`last` drive gif playback
/// (advanced in `about_to_wait`); they stay put for single-frame images.
struct ImagePane {
    frames: Vec<ImageFrame>,
    w: u32,
    h: u32,
    cur: std::sync::atomic::AtomicUsize,
    last: std::sync::Mutex<std::time::Instant>,
}

impl ImagePane {
    fn cur_idx(&self) -> usize {
        self.cur
            .load(std::sync::atomic::Ordering::Relaxed)
            .min(self.frames.len().saturating_sub(1))
    }
    fn cur_rgba(&self) -> &[u8] {
        &self.frames[self.cur_idx()].rgba
    }
    /// Advance to the next frame when the current one's delay has elapsed.
    /// Returns true when the frame changed (caller requests a redraw).
    fn tick(&self, now: std::time::Instant) -> bool {
        if self.frames.len() < 2 {
            return false;
        }
        let cur = self.cur_idx();
        let mut last = self.last.lock().unwrap();
        if now.duration_since(*last) >= self.frames[cur].delay {
            self.cur
                .store((cur + 1) % self.frames.len(), std::sync::atomic::Ordering::Relaxed);
            *last = now;
            true
        } else {
            false
        }
    }
    /// When the current frame is due to flip — feeds the loop's WaitUntil so
    /// playback ticks without busy-waiting. None for single-frame images.
    fn next_deadline(&self) -> Option<std::time::Instant> {
        if self.frames.len() < 2 {
            return None;
        }
        Some(*self.last.lock().unwrap() + self.frames[self.cur_idx()].delay)
    }
}

/// Largest texture edge we upload. Comfortably under every backend's
/// max-texture-dimension and keeps a huge screenshot from eating VRAM;
/// the pane fits the image anyway so the downscale is invisible.
const MAX_IMAGE_EDGE: u32 = 4096;

/// Decode an image file to RGBA8, downscaling so neither edge exceeds
/// `MAX_IMAGE_EDGE`. Returns an error the `imgopen` caller surfaces on a
/// path that isn't a decodable image.
fn decode_image_rgba(path: &std::path::Path) -> anyhow::Result<ImagePane> {
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    // Animated gif: decode every frame + delay so about_to_wait can cycle them.
    // A single-frame gif falls through to the static path below.
    let is_gif = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("gif"))
        .unwrap_or(false);
    if is_gif {
        use image::AnimationDecoder;
        let file = std::fs::File::open(path)?;
        let dec = image::codecs::gif::GifDecoder::new(std::io::BufReader::new(file))?;
        let raw = dec.into_frames().collect_frames()?;
        if raw.len() > 1 {
            let mut frames = Vec::with_capacity(raw.len());
            let (mut w, mut h) = (0u32, 0u32);
            for f in raw {
                let delay: Duration = f.delay().into();
                let buf = f.into_buffer();
                w = buf.width();
                h = buf.height();
                frames.push(ImageFrame {
                    rgba: buf.into_raw(),
                    // Floor near-zero delays to ~100ms (as browsers do) so a
                    // 0-delay frame doesn't spin the loop.
                    delay: if delay.is_zero() { Duration::from_millis(100) } else { delay },
                });
            }
            return Ok(ImagePane {
                frames,
                w,
                h,
                cur: AtomicUsize::new(0),
                last: Mutex::new(Instant::now()),
            });
        }
    }
    let img = image::open(path).map_err(|e| anyhow::anyhow!("decode failed: {e}"))?;
    let (w0, h0) = (img.width(), img.height());
    let img = if w0 > MAX_IMAGE_EDGE || h0 > MAX_IMAGE_EDGE {
        img.resize(
            MAX_IMAGE_EDGE,
            MAX_IMAGE_EDGE,
            image::imageops::FilterType::Lanczos3,
        )
    } else {
        img
    };
    let rgba = img.to_rgba8();
    let (w, h) = (rgba.width(), rgba.height());
    Ok(ImagePane {
        frames: vec![ImageFrame {
            rgba: rgba.into_raw(),
            delay: Duration::ZERO,
        }],
        w,
        h,
        cur: AtomicUsize::new(0),
        last: Mutex::new(Instant::now()),
    })
}

/// Rotate RGBA8 image data by `quarters` * 90° clockwise. Returns the new
/// pixel buffer plus its new (w, h). quarters=0 returns the input untouched.
fn rotate_rgba_cw(rgba: &[u8], w: u32, h: u32, quarters: u8) -> (Vec<u8>, u32, u32) {
    let q = quarters % 4;
    if q == 0 || rgba.is_empty() {
        return (rgba.to_vec(), w, h);
    }
    let (nw, nh) = if q % 2 == 1 { (h, w) } else { (w, h) };
    let mut out = vec![0u8; rgba.len()];
    for y in 0..h {
        for x in 0..w {
            let src = ((y * w + x) * 4) as usize;
            let (nx, ny) = match q {
                1 => (h - 1 - y, x),         // 90° CW
                2 => (w - 1 - x, h - 1 - y), // 180°
                3 => (y, w - 1 - x),         // 270° CW
                _ => unreachable!(),
            };
            let dst = ((ny * nw + nx) * 4) as usize;
            out[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
        }
    }
    (out, nw, nh)
}

/// Byte offset of the `col`-th char in `s` (clamped to `s.len()`). Used by the
/// Raw markdown editor to translate a char-column cursor into a byte index.
/// `col` 번째 글자가 시작하는 바이트 오프셋. 줄보다 크면 줄 끝.
///
/// backspace·insert·delete·newline 마다 두 번씩 불리는 자리라 두 단계로 짧게
/// 끊는다. ASCII 줄(코드에서 대부분)은 바이트=글자라 곱셈 하나로 끝나고,
/// 그 밖에는 UTF-8 **선두 바이트만 세는 바이트 스캔**을 쓴다 — 원래의
/// `char_indices().nth()` 는 줄을 실제로 디코딩하며 걸어갔다.
///
/// 실측(20만 회, release): 짧은 ASCII 5.2배 · 긴 ASCII 5.8배 · 긴 한글 1.8배
/// 빠르다. 짧은 한글 줄만 `is_ascii` 검사값 때문에 회당 2.3ns 손해인데, 편집 한
/// 번에 두 번 불려도 5ns 라 긴 줄에서 버는 것에 비하면 없는 값이다.
fn char_byte(s: &str, col: usize) -> usize {
    if s.is_ascii() {
        return col.min(s.len());
    }
    let mut n = 0;
    for (i, &b) in s.as_bytes().iter().enumerate() {
        // 0b10xxxxxx 는 이어지는 바이트 — 글자 시작이 아니다.
        if b & 0xC0 != 0x80 {
            if n == col {
                return i;
            }
            n += 1;
        }
    }
    s.len()
}

/// One styled inline run inside a markdown block. The renderer picks the
/// font weight/slant and (for `code`) a mono face + chip background from
/// these flags.
#[derive(Clone, Debug)]
struct MdSpan {
    text: String,
    bold: bool,
    italic: bool,
    code: bool,
    /// `~~취소선~~`. 렌더는 밑줄과 같은 선을 x-height 중간에 긋는다.
    strike: bool,
    /// `Some(dest)` for the text inside a `[text](dest)` link. The renderer
    /// draws it accented + underlined; a click resolves `dest` to a file
    /// (Finder) or URL (browser).
    link: Option<String>,
}

/// A laid-out-able markdown block. Parsed once on open; the renderer walks
/// this every frame (cheap) rather than re-parsing the source text.
#[derive(Clone)]
enum MdBlock {
    Heading { level: u8, spans: Vec<MdSpan> },
    Para { spans: Vec<MdSpan> },
    /// Fenced/indented code block — raw text with embedded newlines. `lang`
    /// is the fence info string (e.g. "rust"), empty for indented blocks.
    Code { code: String, lang: String },
    /// `task` = `Some(checked)` for a `- [ ]` / `- [x]` item; the renderer draws
    /// a checkbox instead of `marker`.
    ListItem { depth: u8, marker: String, spans: Vec<MdSpan>, task: Option<bool> },
    Quote { spans: Vec<MdSpan> },
    /// `> [!NOTE]` 알림. 인용문과 같은 문단 평탄화를 쓰되 `first`/`last` 로 여러
    /// 문단이 한 상자로 이어지게 한다 — 문단마다 상자를 닫으면 이어진 글이
    /// 토막토막 끊겨 읽힌다.
    ///
    /// `list` 는 알림 안 목록 항목의 (깊이, 표식). 목록을 `ListItem` 으로 내보내면
    /// 상자 밖에 매달려 경고에 딸린 목록이 경고 밖의 글로 읽힌다. 알림 안
    /// 체크박스(`- [ ]`)는 표식으로만 떨어진다 — 상자 안 체크박스는 실제 문서에서
    /// 거의 안 쓰여, 렌더 코드를 겹쳐 둘 값이 없다.
    Callout {
        kind: MdCallout,
        spans: Vec<MdSpan>,
        first: bool,
        last: bool,
        list: Option<(u8, String)>,
    },
    Rule,
    /// YAML frontmatter, as label/value rows. Kept as its own block rather than
    /// parsed into the body: without it the closing `---` reads as a setext
    /// heading and the whole header lands on screen as huge bold text.
    Meta { rows: Vec<(String, String)> },
    /// `![alt](path)` — rendered as a wgpu texture inline (same path as the
    /// image pane). `key` is the texture cache id, `w`/`h` the decoded pixel
    /// size for aspect layout; all three are filled in after parse when the
    /// image is decoded (0/empty until then). `path` is kept for alt fallback.
    Image { path: String, alt: String, key: String, w: u32, h: u32 },
    /// GFM table. `head` is the header row (empty for a headerless table),
    /// `rows` the body; every cell carries its own inline spans so a cell can
    /// hold bold/code/links like any other block. `align` is per column.
    Table { head: Vec<MdCell>, rows: Vec<Vec<MdCell>>, align: Vec<MdAlign> },
}

/// A drag selection inside a rendered markdown pane. Coordinates are **document
/// space** = screen px + the pane's scroll offset. Keeping the scroll folded in
/// is what lets a selection survive scrolling mid-drag; with plain screen y the
/// whole range would slide along with the wheel.
struct MdRenderSel {
    pane: String,
    anchor: (f32, f32),
    end: (f32, f32),
    /// Mouse still held. Cleared on release so the selection stays visible (and
    /// copyable) afterwards without the cursor dragging it around.
    dragging: bool,
}

/// One table cell's inline content.
type MdCell = Vec<MdSpan>;

/// GFM 알림 종류(`> [!NOTE]`) — 노션 콜아웃 자리. 인용문과 달리 "이걸 조심해라"
/// 는 신호라, 색과 표지로 본문 흐름에서 튀어나와야 한다.
#[derive(Clone, Copy, PartialEq, Debug)]
enum MdCallout {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
}

impl MdCallout {
    /// 표지 아이콘·제목·색. 짝은 GitHub 이 쓰는 그대로 둔다 — 같은 문서를 딴 데서
    /// 볼 때와 뜻이 어긋나면 안 된다. 색은 전부 테마 토큰이라 테마를 바꿔도 따라간다.
    fn face(self) -> (&'static str, &'static str, [u8; 4]) {
        match self {
            Self::Note => ("info", "Note", theme::accent()),
            Self::Tip => ("lightbulb", "Tip", theme::success()),
            Self::Important => ("message-square-warning", "Important", theme::syn_keyword()),
            Self::Warning => ("triangle-alert", "Warning", theme::syn_type()),
            Self::Caution => ("octagon-alert", "Caution", theme::danger()),
        }
    }
}

/// Per-column alignment from a table's `|:--:|` delimiter row.
#[derive(Clone, Copy, PartialEq, Debug)]
enum MdAlign {
    Left,
    Center,
    Right,
}

/// A decoded image referenced by a markdown document, uploaded to the GPU
/// under `key` and drawn where its `MdBlock::Image` sits.
struct MdDocImage {
    key: String,
    rgba: Vec<u8>,
    w: u32,
    h: u32,
}

/// A parsed markdown document bound to a pane (same lifetime discipline as
/// `ImagePane`). The renderer lays the blocks out into the pane box. `path`
/// is the source file (for the edit button); `images` are decoded inline
/// images the renderer uploads + draws.
struct MarkdownDoc {
    /// Serial number, unique per parse. The renderer memoizes block heights
    /// against it — a pointer/path wouldn't do, since a reparsed doc can land
    /// on the same address and would then be served the old layout.
    gen: u64,
    blocks: Vec<MdBlock>,
    /// 0-based source line of each block, index-aligned with `blocks`. Lets the
    /// Raw↔Render toggle keep the line you were reading on screen.
    block_lines: Vec<usize>,
    path: String,
    images: Vec<MdDocImage>,
    /// Original source text — seeds the Raw editor buffer and is rewritten on
    /// save, then re-parsed into `blocks`.
    raw: String,
}

fn heading_level(l: pulldown_cmark::HeadingLevel) -> u8 {
    use pulldown_cmark::HeadingLevel::*;
    match l {
        H1 => 1,
        H2 => 2,
        H3 => 3,
        H4 => 4,
        H5 => 5,
        H6 => 6,
    }
}

/// Parse markdown source into a flat block list. Nesting beyond list depth
/// is flattened (a quote's paragraphs collapse into Quote blocks, list-item
/// paragraphs into the item) — enough structure for a document-style reader
/// without a full layout tree.
///
/// The second return is each block's 0-based source line, index-aligned with
/// the blocks. Raw↔Render mode switching uses it to keep the line you were
/// looking at on screen; without it the toggle can only guess.
/// frontmatter 를 화면에 세울 라벨/값 줄로 눕힌다. 진짜 YAML 파서를 붙일 이유는
/// 없다 — 속성 줄로 보여줄 만큼만 읽는다. 중첩 키는 `부모.자식`으로 펴고, `- `
/// 목록은 바로 위 키의 값에 이어 붙인다.
fn parse_frontmatter_rows(src: &str) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = Vec::new();
    let mut parent: Option<String> = None;
    for line in src.lines() {
        let body = line.trim();
        if body.is_empty() {
            continue;
        }
        let indented = line.starts_with(' ') || line.starts_with('\t');
        if let Some(item) = body.strip_prefix("- ") {
            if let Some(last) = rows.last_mut() {
                if last.1.is_empty() {
                    last.1 = item.trim().to_string();
                } else {
                    last.1.push_str(", ");
                    last.1.push_str(item.trim());
                }
            }
            continue;
        }
        let Some((k, v)) = body.split_once(':') else { continue };
        let (k, v) = (k.trim(), v.trim());
        if k.is_empty() {
            continue;
        }
        // 값이 빈 최상위 키(`metadata:`)는 뒤따르는 들여쓰기 줄들의 부모다.
        if v.is_empty() && !indented {
            parent = Some(k.to_string());
            continue;
        }
        let label = match (indented, parent.as_deref()) {
            (true, Some(p)) => format!("{p}.{k}"),
            _ => k.to_string(),
        };
        if !indented {
            parent = None;
        }
        rows.push((label, v.to_string()));
    }
    rows
}

/// 블록이 확정될 때 `[[토픽이름]]` 을 링크 스팬으로 바꾼다.
///
/// 파싱 도중에 못 하는 이유: cmark 는 짝이 안 맞는 대괄호를 **낱개 이벤트**로
/// 흘린다(`[`·`[`·이름·`]`·`]` 다섯 개). 이벤트 하나만 보면 표기가 완성돼 보이는
/// 순간이 없어, 스팬이 다 모인 뒤 이어 붙여야 비로소 눈에 띈다.
///
/// 스타일이 같은 평문 스팬만 이어 붙인다 — 인라인 코드나 이미 링크인 조각을
/// 삼키면 `` `[[a]]` `` 처럼 일부러 표기를 보여 주는 글까지 링크가 된다.
fn wikilinked(spans: &mut Vec<MdSpan>) -> Vec<MdSpan> {
    let src = std::mem::take(spans);
    if !src.iter().any(|s| s.link.is_none() && !s.code && s.text.contains('[')) {
        return src;
    }
    let mut out: Vec<MdSpan> = Vec::with_capacity(src.len());
    let mut run = String::new();
    let mut style = (false, false, false);
    fn flush(out: &mut Vec<MdSpan>, run: &mut String, st: (bool, bool, bool)) {
        if !run.is_empty() {
            push_wikilinked(out, run, st.0, st.1, st.2);
            run.clear();
        }
    }
    for s in src {
        let st = (s.bold, s.italic, s.strike);
        if s.link.is_none() && !s.code {
            if !run.is_empty() && st != style {
                flush(&mut out, &mut run, style);
            }
            style = st;
            run.push_str(&s.text);
        } else {
            flush(&mut out, &mut run, style);
            out.push(s);
        }
    }
    flush(&mut out, &mut run, style);
    out
}

/// `[[토픽이름]]` 을 클릭 가능한 링크로 쪼갠다. 메모리 볼트는 문서를 이 표기로
/// 엮는데(인덱스 한 줄이 곧 한 토픽) 마크다운 표준이 아니라 여태 죽은 글자였다 —
/// 노션의 페이지 링크에 해당하는 자리다. 목적지는 `wiki:` 스킴으로 넘겨 클릭할 때
/// 파일을 찾고, 화면에는 대괄호를 벗긴 이름만 남긴다.
fn push_wikilinked(spans: &mut Vec<MdSpan>, t: &str, bold: bool, italic: bool, strike: bool) {
    let plain = |spans: &mut Vec<MdSpan>, s: &str| {
        if !s.is_empty() {
            spans.push(MdSpan {
                text: s.to_string(),
                bold,
                italic,
                code: false,
                strike,
                link: None,
            });
        }
    };
    let mut rest = t;
    while let Some(i) = rest.find("[[") {
        let after = &rest[i + 2..];
        let Some(j) = after.find("]]") else { break };
        let name = &after[..j];
        // 이름에 대괄호나 줄바꿈이 섞이면 위키링크가 아니다 — 여는 괄호까지만
        // 평문으로 흘리고 그 뒤에서 다시 찾는다.
        if name.is_empty() || name.contains(['[', ']', '\n']) {
            plain(spans, &rest[..i + 2]);
            rest = after;
            continue;
        }
        plain(spans, &rest[..i]);
        spans.push(MdSpan {
            text: name.to_string(),
            bold,
            italic,
            code: false,
            strike,
            link: Some(format!("wiki:{name}")),
        });
        rest = &after[j + 2..];
    }
    plain(spans, rest);
}

/// 원시 HTML 조각에서 사람이 읽을 글자만 뽑는다. 렌더뷰엔 HTML 엔진이 없어
/// 태그를 그릴 수는 없지만, 태그에 감싸였다는 이유로 본문을 버리면 문서 내용이
/// 조용히 사라진다 — `<div>`·`<summary>`·`<system-reminder>` 안의 문장이 실제로
/// 화면에서 없어졌다. 태그 이름에 밑줄이 든 `<critical_rule>` 은 HTML 태그
/// 규칙에 어긋나 애초에 평문으로 흐르니 이 경로를 타지 않는다.
///
/// 주석(`<!-- -->`)은 반대로 지우는 게 맞다. 감춰 두려고 쓴 표기라 드러내면
/// 글쓴이의 뜻이 뒤집힌다.
fn html_visible_text(raw: &str) -> String {
    let mut out = String::new();
    let mut rest = raw;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let skip = if tail.starts_with("<!--") {
            // 닫히지 않은 주석은 문서 끝까지 주석이다(CommonMark).
            match tail[4..].find("-->") {
                Some(k) => 4 + k + 3,
                None => break,
            }
        } else {
            match tail.find('>') {
                Some(k) => k + 1,
                None => break,
            }
        };
        rest = &tail[skip..];
    }
    out.push_str(rest);
    // 엔티티는 자주 쓰는 것만 푼다. 전체 표를 들일 만큼 마크다운 문서에
    // 엔티티가 잦지 않다. `&amp;` 를 맨 뒤에 두는 건 `&amp;lt;` 가 `<` 로
    // 두 번 풀리지 않게 하기 위한 것이다.
    let out = out
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn parse_markdown(text: &str) -> (Vec<MdBlock>, Vec<usize>) {
    use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
    let mut blocks: Vec<MdBlock> = Vec::new();
    let mut block_lines: Vec<usize> = Vec::new();
    // Byte offset of every line start, so a block's byte offset becomes a line
    // number by binary search. Counting newlines per block would be O(n) each.
    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(text.match_indices('\n').map(|(i, _)| i + 1))
        .collect();
    let line_of = |off: usize| line_starts.partition_point(|&s| s <= off).saturating_sub(1);
    let mut spans: Vec<MdSpan> = Vec::new();
    let mut bold = 0i32;
    let mut italic = 0i32;
    let mut strike = 0i32;
    let mut in_meta = false;
    let mut meta_buf = String::new();
    let mut item_task: Option<bool> = None;
    let mut heading: Option<u8> = None;
    let mut in_code = false;
    let mut code_buf = String::new();
    let mut code_lang = String::new();
    // Each open list level: Some(next_number) for ordered, None for bullet.
    let mut list_stack: Vec<Option<u64>> = Vec::new();
    let mut in_item = false;
    let mut item_marker = String::new();
    let mut in_quote = false;
    // 열려 있는 인용문이 알림(`> [!NOTE]`)이면 그 종류. `quote_first` 는 상자
    // 머리(아이콘·제목)를 첫 문단에만 그리기 위한 것이다.
    let mut quote_kind: Option<MdCallout> = None;
    let mut quote_first = true;
    let mut in_image = false;
    let mut img_url = String::new();
    let mut img_alt = String::new();
    let mut link_url: Option<String> = None;
    // Table accumulators. Cells reuse `spans` (they hold inline content only —
    // pulldown emits no Paragraph inside a cell), flushed at each TagEnd.
    let mut tbl_align: Vec<MdAlign> = Vec::new();
    let mut tbl_head: Vec<MdCell> = Vec::new();
    let mut tbl_rows: Vec<Vec<MdCell>> = Vec::new();
    let mut tbl_row: Vec<MdCell> = Vec::new();

    let push_span = |spans: &mut Vec<MdSpan>,
                     t: &str,
                     b: bool,
                     i: bool,
                     s: bool,
                     c: bool,
                     link: Option<String>| {
        if !t.is_empty() {
            spans.push(MdSpan {
                text: t.to_string(),
                bold: b,
                italic: i,
                code: c,
                strike: s,
                link,
            });
        }
    };

    // GFM 확장을 켜 둔다: 켜지 않으면 `~~취소선~~`·`- [ ]` 가 평문으로 떨어지고,
    // 무엇보다 frontmatter 의 닫는 `---` 가 setext heading 으로 읽혀 YAML 머리가
    // 문서 첫 화면을 거대한 굵은 글씨로 덮는다. `ENABLE_GFM` 은 알림 태그
    // (`> [!NOTE]`) 하나만 켠다 — 다른 파싱 규칙은 건드리지 않는다.
    let opts = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_GFM
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS;
    for (ev, rng) in Parser::new_ext(text, opts).into_offset_iter() {
        match ev {
            Event::Start(tag) => match tag {
                Tag::Heading { level, .. } => {
                    heading = Some(heading_level(level));
                    spans.clear();
                }
                Tag::Paragraph => spans.clear(),
                Tag::CodeBlock(kind) => {
                    in_code = true;
                    code_buf.clear();
                    code_lang = match kind {
                        pulldown_cmark::CodeBlockKind::Fenced(info) => {
                            info.split_whitespace().next().unwrap_or("").to_string()
                        }
                        pulldown_cmark::CodeBlockKind::Indented => String::new(),
                    };
                }
                Tag::List(start) => {
                    // 중첩 리스트가 열리면 부모 항목을 **여기서** 밀어 넣는다.
                    // TagEnd::Item 까지 미루면 자식이 먼저 블록 목록에 들어가
                    // 화면에서 부모 위로 올라오고, 게다가 자식의 Tag::Item 이
                    // 공유 버퍼인 spans 를 비워 부모 텍스트가 통째로 사라진다
                    // (`- outer` / `  - inner` 가 inner, "" 순으로 나왔다).
                    if in_item && !spans.is_empty() {
                        blocks.push(MdBlock::ListItem {
                            depth: list_stack.len().saturating_sub(1) as u8,
                            marker: std::mem::take(&mut item_marker),
                            spans: wikilinked(&mut spans),
                            task: item_task.take(),
                        });
                        in_item = false;
                    }
                    list_stack.push(start)
                }
                Tag::Item => {
                    in_item = true;
                    item_task = None;
                    spans.clear();
                    item_marker = match list_stack.last_mut() {
                        Some(Some(n)) => {
                            let m = format!("{n}.");
                            *n += 1;
                            m
                        }
                        _ => "•".to_string(),
                    };
                }
                Tag::Emphasis => italic += 1,
                Tag::Strong => bold += 1,
                Tag::Strikethrough => strike += 1,
                Tag::MetadataBlock(_) => {
                    in_meta = true;
                    meta_buf.clear();
                }
                Tag::BlockQuote(kind) => {
                    in_quote = true;
                    quote_kind = kind.map(|k| match k {
                        pulldown_cmark::BlockQuoteKind::Note => MdCallout::Note,
                        pulldown_cmark::BlockQuoteKind::Tip => MdCallout::Tip,
                        pulldown_cmark::BlockQuoteKind::Important => MdCallout::Important,
                        pulldown_cmark::BlockQuoteKind::Warning => MdCallout::Warning,
                        pulldown_cmark::BlockQuoteKind::Caution => MdCallout::Caution,
                    });
                    quote_first = true;
                    spans.clear();
                }
                Tag::Image { dest_url, .. } => {
                    in_image = true;
                    img_url = dest_url.to_string();
                    img_alt.clear();
                }
                Tag::Link { dest_url, .. } => {
                    link_url = Some(dest_url.to_string());
                }
                Tag::Table(aligns) => {
                    tbl_align = aligns
                        .iter()
                        .map(|a| match a {
                            pulldown_cmark::Alignment::Center => MdAlign::Center,
                            pulldown_cmark::Alignment::Right => MdAlign::Right,
                            _ => MdAlign::Left,
                        })
                        .collect();
                    tbl_head.clear();
                    tbl_rows.clear();
                    tbl_row.clear();
                }
                Tag::TableHead | Tag::TableRow => tbl_row.clear(),
                Tag::TableCell => spans.clear(),
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Heading(_) => blocks.push(MdBlock::Heading {
                    level: heading.take().unwrap_or(1),
                    spans: wikilinked(&mut spans),
                }),
                TagEnd::Paragraph => {
                    // Skip empty paragraphs (e.g. a paragraph that held only an
                    // image, which was emitted as its own Image block).
                    if in_quote {
                        if !spans.is_empty() {
                            let spans = wikilinked(&mut spans);
                            blocks.push(match quote_kind {
                                Some(kind) => {
                                    let first = std::mem::take(&mut quote_first);
                                    // `last` 는 인용문이 닫힐 때 되짚어 세운다 —
                                    // 여기선 다음 문단이 있을지 알 수 없다.
                                    MdBlock::Callout {
                                        kind,
                                        spans,
                                        first,
                                        last: false,
                                        list: None,
                                    }
                                }
                                None => MdBlock::Quote { spans },
                            });
                        }
                    } else if !in_item && !spans.is_empty() {
                        blocks.push(MdBlock::Para { spans: wikilinked(&mut spans) });
                    }
                    if !in_item {
                        spans.clear();
                    }
                    // in_item: keep spans; flushed at TagEnd::Item.
                }
                TagEnd::CodeBlock => {
                    in_code = false;
                    blocks.push(MdBlock::Code {
                        code: std::mem::take(&mut code_buf),
                        lang: std::mem::take(&mut code_lang),
                    });
                }
                TagEnd::List(_) => {
                    list_stack.pop();
                }
                TagEnd::Item => {
                    // `in_item` 이 false 면 중첩 리스트가 열릴 때 이미 내보낸
                    // 항목이다 — 여기서 또 밀면 빈 항목이 하나 더 생긴다.
                    if in_item {
                        let depth = list_stack.len().saturating_sub(1) as u8;
                        let marker = std::mem::take(&mut item_marker);
                        let spans = wikilinked(&mut spans);
                        let task = item_task.take();
                        blocks.push(match quote_kind {
                            Some(kind) => MdBlock::Callout {
                                kind,
                                spans,
                                first: std::mem::take(&mut quote_first),
                                last: false,
                                list: Some((depth, marker)),
                            },
                            None => MdBlock::ListItem { depth, marker, spans, task },
                        });
                        in_item = false;
                    }
                }
                TagEnd::Emphasis => italic -= 1,
                TagEnd::Strong => bold -= 1,
                TagEnd::Strikethrough => strike -= 1,
                TagEnd::MetadataBlock(_) => {
                    in_meta = false;
                    let rows = parse_frontmatter_rows(&std::mem::take(&mut meta_buf));
                    if !rows.is_empty() {
                        blocks.push(MdBlock::Meta { rows });
                    }
                }
                TagEnd::Link => link_url = None,
                TagEnd::BlockQuote(_) => {
                    in_quote = false;
                    // 상자 아래를 닫는다. `quote_first` 가 아직 서 있으면 이 알림엔
                    // 문단이 하나도 없던 것(목록만 든 알림)이라 닫을 상자가 없다.
                    // 뒤에서부터 찾는 이유: 알림 안 목록은 상자 밖으로 평탄화돼
                    // `blocks` 맨 끝이 ListItem 일 수 있다.
                    if quote_kind.take().is_some() && !quote_first {
                        if let Some(MdBlock::Callout { last, .. }) = blocks
                            .iter_mut()
                            .rev()
                            .find(|b| matches!(b, MdBlock::Callout { .. }))
                        {
                            *last = true;
                        }
                    }
                }
                TagEnd::Image => {
                    blocks.push(MdBlock::Image {
                        path: std::mem::take(&mut img_url),
                        alt: std::mem::take(&mut img_alt),
                        key: String::new(),
                        w: 0,
                        h: 0,
                    });
                    in_image = false;
                }
                TagEnd::TableCell => tbl_row.push(wikilinked(&mut spans)),
                TagEnd::TableHead => tbl_head = std::mem::take(&mut tbl_row),
                TagEnd::TableRow => tbl_rows.push(std::mem::take(&mut tbl_row)),
                TagEnd::Table => blocks.push(MdBlock::Table {
                    head: std::mem::take(&mut tbl_head),
                    rows: std::mem::take(&mut tbl_rows),
                    align: std::mem::take(&mut tbl_align),
                }),
                _ => {}
            },
            Event::Text(t) => {
                if in_meta {
                    meta_buf.push_str(&t);
                } else if in_image {
                    img_alt.push_str(&t);
                } else if in_code {
                    code_buf.push_str(&t);
                } else if link_url.is_some() {
                    push_span(
                        &mut spans,
                        &t,
                        bold > 0,
                        italic > 0,
                        strike > 0,
                        false,
                        link_url.clone(),
                    );
                } else {
                    push_span(
                        &mut spans,
                        &t,
                        bold > 0,
                        italic > 0,
                        strike > 0,
                        false,
                        None,
                    );
                }
            }
            Event::Code(t) => push_span(
                &mut spans,
                &t,
                bold > 0,
                italic > 0,
                strike > 0,
                true,
                link_url.clone(),
            ),
            Event::Html(raw) => {
                // 블록 HTML. 태그만 든 줄은 아무것도 남기지 않고, 태그에 감싸인
                // 문장만 문단으로 살아난다. cmark 가 이 블록을 한 덩어리로 주든
                // 줄마다 쪼개 주든 결과가 같아, 어느 쪽이어도 글이 안 사라진다.
                let text = html_visible_text(&raw);
                if !text.is_empty() {
                    let mut s = vec![MdSpan {
                        text,
                        bold: false,
                        italic: false,
                        code: false,
                        strike: false,
                        link: None,
                    }];
                    blocks.push(MdBlock::Para { spans: wikilinked(&mut s) });
                }
            }
            Event::InlineHtml(raw) => {
                // 인라인 태그. 통째로 버리면 강조가 사라지고 글자로 그리면 문서에
                // 없던 꺾쇠가 생긴다 — 아는 태그는 서체로 옮기고 모르는 태그만
                // 조용히 지운다. `<br>` 는 띄어쓰기로 떨어진다(스팬에 줄바꿈
                // 표기가 없다). 붙여 버리면 앞뒤 낱말이 한 덩어리가 된다.
                let t = raw.trim();
                let closing = t.starts_with("</");
                let name = t
                    .trim_start_matches(['<', '/'])
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric())
                    .flat_map(|c| c.to_lowercase())
                    .collect::<String>();
                let bump = |n: &mut i32| {
                    if closing {
                        *n = (*n - 1).max(0)
                    } else {
                        *n += 1
                    }
                };
                match name.as_str() {
                    "b" | "strong" => bump(&mut bold),
                    "i" | "em" => bump(&mut italic),
                    "s" | "del" | "strike" => bump(&mut strike),
                    "br" => push_span(
                        &mut spans,
                        " ",
                        bold > 0,
                        italic > 0,
                        strike > 0,
                        false,
                        link_url.clone(),
                    ),
                    _ => {}
                }
            }
            Event::TaskListMarker(checked) => item_task = Some(checked),
            Event::SoftBreak | Event::HardBreak => {
                if in_meta {
                    meta_buf.push('\n');
                } else if in_code {
                    code_buf.push('\n');
                } else {
                    push_span(
                        &mut spans,
                        " ",
                        bold > 0,
                        italic > 0,
                        strike > 0,
                        false,
                        link_url.clone(),
                    );
                }
            }
            Event::Rule => blocks.push(MdBlock::Rule),
            _ => {}
        }
        // 이 이벤트가 블록을 밀어 넣었으면 소스 줄을 짝지어 둔다. End 이벤트의
        // 범위는 요소 전체라 `rng.start` 가 곧 그 블록이 시작한 줄이다. 푸시
        // 지점마다 적지 않고 여기 한 곳에서 채우는 건, 그래야 두 벡터의 길이가
        // 구조적으로 어긋날 수 없어서다.
        while block_lines.len() < blocks.len() {
            block_lines.push(line_of(rng.start));
        }
    }
    (blocks, block_lines)
}

/// Parse + decode a markdown document: parse blocks, then decode each inline
/// image (resolving relative paths against the md file's dir, skipping remote
/// URLs) under path-keyed textures. Shared by initial open and post-edit
/// re-parse.
fn build_markdown_doc(p: &std::path::Path, text: &str) -> MarkdownDoc {
    let (mut blocks, block_lines) = parse_markdown(text);
    let md_dir = p.parent().map(|d| d.to_path_buf());
    let mut images: Vec<MdDocImage> = Vec::new();
    for block in blocks.iter_mut() {
        if let MdBlock::Image { path: ipath, key, w, h, .. } = block {
            if ipath.starts_with("http://") || ipath.starts_with("https://") {
                continue;
            }
            let resolved = if std::path::Path::new(&ipath).is_absolute() {
                std::path::PathBuf::from(&*ipath)
            } else if let Some(dir) = &md_dir {
                dir.join(&*ipath)
            } else {
                std::path::PathBuf::from(&*ipath)
            };
            if let Ok(img) = decode_image_rgba(&resolved) {
                // 텍스처 id 는 **이미지 파일 경로**다. 예전엔 `{pane}#img{블록번호}`
                // 였는데, 이미지 위에 문단 하나만 끼워 넣어도 번호가 밀려 새 키가
                // 되고 옛 텍스처는 `Gpu::images` 에 주인 없이 남았다(누수). 게다가
                // 같은 파일을 두 pane 에서 열면 같은 그림을 두 번 올렸다.
                // 경로로 잡으면 문서를 다시 파싱해도, pane 이 몇 개든 하나다.
                let canon = std::fs::canonicalize(&resolved).unwrap_or_else(|_| resolved.clone());
                let k = format!("mdimg:{}", canon.display());
                *key = k.clone();
                *w = img.w;
                *h = img.h;
                images.push(MdDocImage { key: k, rgba: img.cur_rgba().to_vec(), w: img.w, h: img.h });
            }
        }
    }
    static GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    MarkdownDoc {
        gen: GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        blocks,
        block_lines,
        path: p.to_string_lossy().into_owned(),
        images,
        raw: text.to_string(),
    }
}

/// Whole-window state: HashMap of panes keyed by tmux pane id, the
/// most recently parsed Layout tree, and which pane is active for
/// keyboard / selection / cursor display.
struct Workspace {
    panes: HashMap<String, PaneState>,
    layout: Option<Layout>,
    active_pane: Option<String>,
    /// Reverse map: PtySession id (a tab's `pid`) → outer pane id (the layout
    /// key in `panes`). Stage 3 lets one pane host multiple tabs each with
    /// their own PTY; this lookup lets `pump_pty_screens` find the right
    /// `(PaneState, tab_index)` from a backend update keyed by its own pid.
    /// The first tab's pid equals the outer pane id, so single-tab panes
    /// don't need an entry — but secondary tabs always insert/remove here.
    pid_to_pane: HashMap<String, String>,
    /// 방별 분리(거노): pane → 방(윈도우) 식별자. 새 방 pane 만 들어가고, 기본 방은
    /// 없음 → cwd-slug 그대로. ws 에 둬서 GUI(spawn)와 PtyBackend(collab_board) 가
    /// 같은 매핑을 본다(별 스레드라 App 필드는 socket.rs 가 못 봄).
    pane_room: HashMap<String, String>,
    /// pane → 배정 캐릭터명(미도리 등). pane_room 과 같은 이유로 ws 에 둔다 —
    /// pump 스레드(apply_screen_update)가 PaneState.character 를 동기하고, 헤더
    /// 렌더(render.rs)가 같은 매핑을 본다. assign_character_env 가 spawn 시 채운다.
    pane_character: HashMap<String, String>,
    /// 활성 윈도우(보이는 방)의 leaf pane id 집합. `publish_pty_layout` 이 갱신한다.
    /// collab_board 가 이걸로 bound pane 을 필터해 *활성 방 학생만* board 에 올린다
    /// (거노: 아로나 방 + 프라나 방이 한 교실에 같이 뜨던 문제 — 방별 격리).
    active_window_panes: std::collections::HashSet<String>,
    /// pane → 속한 윈도우(방) 인덱스. 전 윈도우 leaf 를 `publish_pty_layout` 이 채운다.
    /// collab_board(PtyBackend, 별 스레드)가 App 의 windows/pty_layout 을 못 봐서 ws 로
    /// 미러 — board 가 전 방 학생을 window_idx 와 함께 실어 arona 좌측 방별 트리를 영속한다.
    pane_window: HashMap<String, usize>,
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            panes: HashMap::new(),
            layout: None,
            active_pane: None,
            pid_to_pane: HashMap::new(),
            pane_room: HashMap::new(),
            pane_character: HashMap::new(),
            active_window_panes: std::collections::HashSet::new(),
            pane_window: HashMap::new(),
        }
    }
}

impl Workspace {
    fn pane_mut(&mut self, id: &str) -> &mut PaneState {
        self.panes
            .entry(id.to_string())
            .or_insert_with(PaneState::default)
    }

    fn active(&self) -> Option<&PaneState> {
        self.active_pane
            .as_deref()
            .and_then(|id| self.panes.get(id))
    }

    fn active_mut(&mut self) -> Option<&mut PaneState> {
        let id = self.active_pane.clone()?;
        self.panes.get_mut(&id)
    }

    /// Rebuild `pid_to_pane` from the current `panes` so it's the
    /// authoritative pid→outer lookup. Cheap (one HashMap rebuild per
    /// layout/tab mutation) and lets the hot `outer_for_pty` path stay
    /// O(1) — important because every PTY ScreenUpdate calls it.
    fn rebuild_pid_map(&mut self) {
        let mut m: HashMap<String, String> = HashMap::with_capacity(self.pid_to_pane.len());
        for (outer, pane) in &self.panes {
            for tab in &pane.tabs {
                if let Some(pid) = tab.pid.as_deref() {
                    m.insert(pid.to_string(), outer.clone());
                }
            }
        }
        self.pid_to_pane = m;
    }

    /// O(1) pid → outer pane lookup. `pid_to_pane` is maintained on every
    /// layout/tab mutation; the panes-contains-key fallback only fires
    /// in the brief window before the first `ScreenUpdate` for a fresh
    /// shell has populated the tab's pid.
    fn outer_for_pty(&self, pty_id: &str) -> Option<String> {
        if let Some(outer) = self.pid_to_pane.get(pty_id) {
            return Some(outer.clone());
        }
        if self.panes.contains_key(pty_id) {
            return Some(pty_id.to_string());
        }
        None
    }

    /// Locate `(outer_pane, tab_index)` for a backend pty id. Used by
    /// `pump_pty_screens` to write the right tab's content even when the
    /// update came from a non-active or secondary-tab shell.
    fn find_tab_by_pty<'a>(&'a mut self, pty_id: &str) -> Option<(&'a mut PaneState, usize)> {
        let outer = self.outer_for_pty(pty_id)?;
        let pane = self.panes.get_mut(&outer)?;
        let idx = pane
            .tabs
            .iter()
            .position(|t| t.pid.as_deref() == Some(pty_id))
            .unwrap_or(0);
        Some((pane, idx))
    }
}

/// Cross-thread wakeup. The PTY ScreenUpdate thread can't reliably wake
/// a parked `WaitUntil` via `request_redraw` on macOS (winit defers the
/// paint to the deadline), so it sends this through the EventLoopProxy
/// instead — winit delivers it as a `user_event` that wakes the loop
/// immediately, so a committed Hangul echo / backspace / space paints
/// without the ~0.5s blink-cadence lag.
#[derive(Debug, Clone)]
enum UserEvent {
    Redraw,
    /// bg-agents 폴러가 `sessionId→parentSessionId` 맵을 갱신했다 — 포크/백그라운드
    /// 세션의 부모 학생 상속을 재적용하라는 신호. 폴러는 3초 주기라 세션 바인딩
    /// (SocketSessionBound) 시점엔 맵이 비어 상속을 놓친다 → 맵이 채워지면 이
    /// 이벤트로 pane_claude_sid 를 다시 훑어 뒤늦게 부모를 물려준다.
    BgAgentsChanged,
    /// A background git op (push/pull/commit) finished — clears the panel's
    /// spinner. Carries nothing: only one git op runs at a time.
    GitOpDone,
    /// Local cmux socket backend → GUI delegation. The socket server runs on
    /// its own thread and can't touch `self.pty` (not Arc<Mutex>), so it routes
    /// pane writes / split / focus to the GUI thread via the proxy. `surface_id`
    /// None = active pane.
    SocketBytes(Option<String>, Vec<u8>),
    /// Split delegated from the socket thread. The `Sender` carries the new
    /// pane's real id back so `split_surface` can return it instead of the old
    /// `"pane-new"` placeholder — without it the teammate launcher targets a
    /// non-existent pane and its `send-keys` payload is dropped. The `bool` is
    /// `focus`: false (CLI/automation default) keeps focus on the current pane,
    /// true follows into the new one.
    SocketSplit(kasa_pty::SplitDir, bool, std::sync::mpsc::Sender<String>),
    SocketFocus(String),
    /// 활성 pane 의 shell OS pid 질의(socket 스레드 → GUI 동기 RPC,
    /// SocketSplit 의 Sender 패턴). GET /mode 등 방 판정이 쓰는
    /// `Backend::active_cwd` 용 — GUI 는 메모리 조회(active_pane→shell_pid)만
    /// 즉답하고, 느린 lsof(cwd 해석)는 backend 스레드가 한다(GUI 블록 금지).
    SocketQueryActivePid(std::sync::mpsc::Sender<Option<u32>>),
    /// 모든 pane 의 `(surface_id, shell_pid)` 질의(SocketQueryActivePid 의 전체판).
    /// hook-free transcript 발견(`collab_board`)이 쓴다 — 우리는 PTY 를 소유하니
    /// 셸 pid 만 알면 backend 스레드가 claude 자식·cwd·session 을 직접 찾아 bind 한다
    /// (claude 훅에 의존하지 않음). GUI 는 메모리 조회만 즉답, lsof/ps 는 backend.
    SocketQueryPanePids(std::sync::mpsc::Sender<Vec<(String, u32)>>),
    /// `GET /sessions` 위임 — 로컬 PTY 모드의 '방' = App 윈도우. PtyBackend 는 App
    /// 상태를 직접 못 봐(별 스레드) `SocketQueryPanePids` 패턴으로 질의: 응답은
    /// (윈도우 수, 활성 idx, [(name, cwd)] 라벨). arona-ui 좌측 방 네비가 쓴다.
    SocketQuerySessions(std::sync::mpsc::Sender<(usize, usize, Vec<(String, String)>)>),
    /// `POST /session-switch?idx=N` 위임 — 보이는 윈도우를 idx 로 전환(거노: GUI 에서
    /// 방=윈도우 클릭 시 그 터미널 윈도우로). `switch_window` 가 resize·redraw 자체 처리.
    SocketSwitchSession(usize),
    /// `POST /session-new?character=<name>` 위임 — 새 방(윈도우, 빈 셸) + 선택 캐릭터 라벨.
    /// 자동통솔 폐기(06-18)로 claude 자동 스폰 없음. `new_room_with_character` 가 처리.
    SocketNewRoom(String),
    /// `POST /spawn-student?character=<name>` 위임 — 현재 방에 캐릭터 지정 학생 추가
    /// (split + pending_character). 아로나/프라나도 학생처럼 고를 수 있다(거노).
    /// Sender 로 새 pane id 를 돌려준다(SocketSplit 패턴) — 디스패처가 스폰 직후
    /// 그 pane 에 브리프를 쏘려면 주소가 필요하다. 빈 문자열 = pane 미생성.
    SocketSpawnStudent(String, std::sync::mpsc::Sender<String>),
    /// `POST /swap-character?surface=<id>&character=<name>` 위임 — (pane, 캐릭터).
    /// 그 pane PTY 를 새 persona 로 respawn(대화 리셋, persona 는 셸 spawn 시 고정).
    SocketSwapCharacter(String, String),
    /// `GET /repersona?surface=<id>&character=<name>` 위임 — (pane, 캐릭터).
    /// respawn 없는 재배정: 학생 명령(`시로코`)이 claude 실행 직전에 호출, persona
    /// 는 래퍼의 override 파일이 싣고 GUI 는 헤더·마커·세션바인딩만 갱신.
    SocketRepersona(String, String),
    /// `POST /session-close?idx=N` 위임 — 방(윈도우) 닫기(거노). `close_window` 가
    /// 마지막 윈도우 가드·pane 정리. 닫기 실패(마지막)는 무시(프론트가 가드).
    SocketCloseRoom(usize),
    /// `GET /open-image`·`/open-markdown`(imgopen/mdopen 셰임·SendUserFile 훅)이
    /// 위임 — 그 경로를 미리보기(이미지/마크다운/텍스트)로 연다. `(path, target)`:
    /// `target` = 요청자 pane id(=pid, `$KASATERM_PANE_ID`). 있으면 그 pane 의 보조
    /// 탭으로(크롬 탭, 멀티뷰 빈-pane 회피), 없으면 active pane split 으로 폴백.
    /// 데몬 제거 때 빠졌던 open_preview 의 로컬 재구현. `open_file` 이 확장자로 분기.
    SocketOpenPreview(String, Option<String>),
    /// `collab.bind_transcript`(SessionStart 훅) 위임 — (pane, 세션 id). transcript
    /// 파일명(stem) = claude 세션 id. 세션→캐릭터 영속 매핑을 조회/저장해 --resume 시
    /// 캐릭터 둔갑을 막는다(거노: 재시작하면 프라나가 미도리로). `apply_session_character`.
    SocketSessionBound(String, String),
    /// pane 이 "보고 있는" 경로 — statusline report-cwd(claude 내부 cd 포함) 또는
    /// transcript bind 시 jsonl tail 의 cwd. 셸 pid cwd 와 달리 pane **내용**의
    /// 프로젝트를 가리켜, 파일트리 루트가 이걸 우선한다(bg-attach pane 은 셸이
    /// ~/Desktop 이라 파일트리가 pane 과 달랐던 것). `(pane, cwd)`.
    SocketViewCwd(String, std::path::PathBuf),
    /// stale statusline 재실행 강제 — 구버전 claude(≤2.1.209 실측)는 attach 에서
    /// statusline 을 재실행하지 않아 세션 id 마커가 프롬프트 전까지 안 흐른다(거노:
    /// 들어오자마자 바뀌게). PTY 1행 지글(줄였다 원복)로 SIGWINCH 재레이아웃을 유도.
    /// 발동 게이트·rate-limit 은 backend(rebind_agents_panes)가 진다.
    NudgePaneResize(String),
    /// `surface.close` delegated from the socket thread → `close_pane`. Local
    /// PTY mode only; the old tmux/daemon backend left this unsupported.
    SocketClose(String),
    /// `POST /paste-image?surface=%N` — 아로나 프롬프트 입력창에 이미지 드롭(webview).
    /// 이미지 바이트를 시스템 클립보드에 비트맵으로 싣고 그 pane 에 Ctrl+V(0x16)를 보내
    /// claude 가 [Image] 칩으로 첨부하게 한다(터미널 DroppedFile 과 같은 경로). `(surface, bytes)`.
    SocketPasteImage(String, Vec<u8>),
    /// `POST /git-panel` — 아로나 타이틀바 버튼 → 터미널 GUI 의 git 소스컨트롤 패널 열기.
    /// 메인 터미널 창을 띄우고(숨겨져 있으면) git 컬럼을 토글한다(거노: 그 버튼=소스컨트롤).
    SocketToggleGit,
    /// Show/hide the main terminal window, delegated from the socket thread
    /// (`POST /terminal-reveal` — the arona classroom's red-pill button).
    /// `(show, focus_pane)`: a reveal may also focus a specific pane so the
    /// classroom can jump the user to a character's seat.
    SocketRevealTerminal(bool, Option<String>),
    /// Close the arona classroom window (`POST /arona-close` — the
    /// ModePicker's "터미널로" choice). No-op when it isn't open.
    SocketAronaClose,
    /// `surface.swap` delegated from the socket thread — exchange two leaves'
    /// tree positions (PTYs stay put, ids trade slots). `(a, b)`, both
    /// pre-validated to exist by the backend.
    SocketSwap(String, String),
    /// `surface.set_ratio` delegated from the socket thread — make a pane
    /// take `ratio` of its immediate split container ("오케스트레이터 pane 크게" 자동화).
    SocketSetRatio(String, f32),
    /// `surface.rename` / `surface.set_color` delegated from the socket thread.
    /// Pane header title / accent band live in `ws.panes` which only the GUI
    /// thread may touch, so the backend routes them here. `(surface_id, title)`
    /// / `(surface_id, rgba)`.
    SocketRename(String, String),
    /// `window.rename` delegated from the socket thread. `(surface_id, title)`:
    /// rename the window/session the pane belongs to (sidebar label), not the
    /// pane header. Used by the rename override.
    SocketRenameWindow(String, String),
    SocketColor(String, [u8; 4]),
    /// `POST /session-resume` from arona-ui — open a pane and queue
    /// `claude --resume <id>` once its shell prompt is up. `newroom` opens a
    /// fresh window; otherwise it splits the active one. `cwd` (when set) is the
    /// session's project dir so resume lands in the right place.
    ResumeSession {
        id: String,
        cwd: Option<String>,
        newroom: bool,
        /// true → `claude attach <id>`(daemon background 세션 연결, 세션 background 유지).
        /// false → `claude --resume <id>`(jsonl 새 프로세스, 과거 세션 이어가기).
        attach: bool,
    },
    /// "대화 저장하기" — surface pane 의 foreground claude 를 ←←(agents view = bg-detach)
    /// 주입으로 background daemon 으로 detach. surface 없으면 active pane. 터미널이 꺼져도
    /// daemon 이 세션을 들고 살아남아 웹뷰에서 계속 보인다(거노 핵심).
    SaveSession {
        surface: Option<String>,
    },
    /// A pane's claude finished (Stop hook → `kasaterm-cli notify`). Raise a
    /// desktop alert unless that pane is already the focused one, cmux-style.
    Notify {
        surface_id: String,
        title: String,
        body: String,
    },
    /// A pane's claude is blocked on a permission / input prompt (its
    /// `Notification` hook → `kasaterm-cli attention`). Toast + flash the pane
    /// and, unless it's the focused pane, raise a desktop alert — the case cmux
    /// treats as its headline feature (a backgrounded agent stuck on "approve
    /// Bash?" that the transcript-board can't see).
    Attention {
        surface_id: String,
        reason: String,
    },
    /// 사용량 폴러가 "지금 계정이 임계를 넘었고, 갈 만한 다른 계정이 있다"고
    /// 판정했다. 실제 전환은 GUI 스레드 몫이다 — `settings_save` 가 shim 을 다시
    /// 깔아야 이미 열려 있는 pane 도 다음 claude 부터 새 계정으로 뜬다.
    /// 페이로드 = (옮겨갈 id, 떠나는 창이 풀리는 시각 epoch, 그때 사용률).
    ClaudeAccountAutoswitch {
        to: String,
        cooldown_until: Option<u64>,
        pct: f32,
    },
    /// macOS `.md` 더블클릭(odoc Apple Event) 또는 argv → 새 워크스페이스에
    /// 마크다운 풀 뷰어. `SocketOpenPreview`(현재 창 split)와 달리 별도 탭의
    /// 단독 pane 으로 띄워 기존 작업 워크스페이스를 안 건드린다. 페이로드 = 경로.
    OpenMarkdownWindow(String),
}

/// One terminal session: its own pane set, layout, and workspace. The visible
/// session lives in App.{pty,pty_layout,ws}; the rest sit in
/// App.stashed_sessions. Each session's `ws` is its own Arc so its
/// pump_pty_screens threads keep updating it in the background even while
/// another session is on screen (tmux-style detached sessions).
#[allow(dead_code)]
struct Session {
    pty: HashMap<String, Arc<kasa_pty::PtySession>>,
    /// Layout of this session's *active* window. The other windows' layouts
    /// sit in `windows` (active slot `None`) — same stash-swap shape the
    /// session list uses one level up.
    pty_layout: Option<kasa_pty::PtyLayout>,
    /// All windows in this session by index. The active window's slot is
    /// `None` (its layout lives in `pty_layout`). Switching windows swaps a
    /// slot in/out; every window shares this session's `pty`/`ws`, so window
    /// switches never tear down panes.
    windows: Vec<Option<kasa_pty::PtyLayout>>,
    /// Index into `windows` of this session's active window.
    active_window: usize,
    ws: Arc<Mutex<Workspace>>,
}

/// One flattened row of the sidebar file tree (the expanded tree is walked
/// into a flat Vec for rendering + hit-testing). `depth` drives indentation.
struct FileNode {
    path: std::path::PathBuf,
    name: String,
    is_dir: bool,
    depth: usize,
    /// Gitignored or a dotfile — rendered italic + dim (VSCode/cursor cue).
    /// Set in a second pass by `rebuild_file_tree_nodes` (one batched
    /// `git check-ignore`), so `walk_dir` leaves it `false`.
    ignored: bool,
}

/// Parsed `git status` snapshot for the right-hand git column. The background
/// poller fills it from `kasa_mcp::git::git_status` (off the main thread);
/// the render reads it. Kept as a flat, render-ready struct so the gpu block
/// (which can't re-borrow `&self` to call helpers) paints straight from it.
#[derive(Clone, Default, PartialEq)]
struct GitColView {
    /// cwd this snapshot was computed for — so a stale repo's rows aren't
    /// shown after a pane switch until the poller catches the new cwd.
    cwd: Option<std::path::PathBuf>,
    /// `git_status` found no repo here (home / arbitrary dir): render a soft
    /// notice instead of a branch + file list.
    no_repo: bool,
    branch: String,
    ahead: u32,
    behind: u32,
    insertions: u32,
    deletions: u32,
    clean: bool,
    /// Index (staged) changes — VSCode's "Staged Changes". `(marker, path)`
    /// where marker is `A`/`M`/`D`. Each row's - button unstages it.
    staged: Vec<(char, String)>,
    /// Worktree changes not yet staged — VSCode's "Changes". `(marker, path)`
    /// where marker is `M` modified · `?` untracked. Each row's + button
    /// stages it. A partially-staged file appears in BOTH lists.
    unstaged: Vec<(char, String)>,
    /// Local branch names for the switcher dropdown (current one is `branch`).
    branches: Vec<String>,
    /// Per-file `(insertions, deletions)` for the row's `+N -M` count, keyed by
    /// path. Filled from `git diff --numstat` (+ `--cached`).
    numstat: HashMap<String, (u32, u32)>,
    /// Most recent commits `(short_hash, subject)` for the panel's preview list.
    recent_commits: Vec<(String, String)>,
}

/// 파일트리 우클릭 컨텍스트 메뉴 항목. `NewFile`/`NewFolder`/`Rename` 은 인라인
/// 입력행을 열고, `CopyPath` 는 절대경로를 클립보드로, `Reveal` 은 OS 파일매니저
/// (Finder/탐색기)에서 보여주고, `Delete` 는 선택 전체를 휴지통으로 보낸다.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FtMenuAction {
    NewFile,
    NewFolder,
    Rename,
    CopyPath,
    Reveal,
    Delete,
    /// "<앱>에서 열기" — `proc::open_with_apps()` 의 인덱스. 이름이 아니라
    /// 인덱스인 건 후보가 기기마다 다르기 때문이다(설치된 것만 노출한다).
    OpenWith(usize),
    /// OS 기본 연결 프로그램으로 열기.
    OpenDefault,
}

/// 한글 조합기(`App::hangul`)를 쓰는 입력 문맥. 조합기는 App 에 **하나뿐인데**
/// 이걸 쓰는 입구는 아홉 곳이라, 문맥이 바뀌어도 조합 상태가 그대로 남아 다음
/// 문맥으로 새어 나간다. 그 주인을 이 값으로 들고 다니며 바뀌는 순간 정리한다.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum ImeFocus {
    /// 터미널 pane(PTY). 메인창·별도창 공통 — pane id 가 곧 보낼 곳이다.
    Pane(String),
    /// 메인창 raw 편집기(pane id).
    Editor(String),
    /// 별도창 raw 편집기(aux 인덱스).
    AuxEditor(usize),
    GitCommit,
    PathSearch,
    TreeSearch,
    TreeNew,
    Settings,
}

/// Action buttons at the foot of the git column. `Commit` hands the commit to
/// the active claude pane; `Pull`/`Push` sync the current branch with its
/// upstream. All shell out through `kasa_mcp::git` on a worker thread so the UI
/// never blocks. (전체 stage 버튼은 cursor 개조 때 사라졌다 — 파일 행마다
/// 개별 stage 하는 모델로 바뀌어서다.)
#[derive(Clone, Copy, PartialEq, Eq)]
enum GitColBtn {
    Commit,
    Pull,
    Push,
}

/// Items in the Commit-button split dropdown (cursor-style): plain commit,
/// commit + push, or open a PR. Wired to `git_commit_menu_rects`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GitCommitAction {
    Commit,
    Push,
    Pull,
    CreatePr,
}

/// Clickable targets inside the Commit modal (screenshot #5).
#[derive(Clone, Copy, PartialEq, Eq)]
enum GitModalBtn {
    Close,
    IncludeUnstaged,
    Commit,
    CommitAndPush,
    Cancel,
    Confirm,
}

/// Left-nav category in the settings screen (Warp-style: list on the left,
/// form on the right). `Appearance` is the theme placeholder for phase 2.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsCat {
    General,
    Appearance,
    Shell,
    Claude,
    Students,
}

/// The two free-text fields in the settings form. Tracks which one (if any)
/// has keyboard focus so keystrokes route to its buffer.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsInput {
    CwdPath,
    /// 터미널 편집기 명령줄 필드("파일 열기"가 `terminal` 일 때만 보인다).
    FileOpenCmd,
    Shell,
    ClaudeExtra,
    /// Students 카테고리 persona 멀티라인 편집 필드가 포커스됨.
    StudentPersona,
    /// Claude 계정 라벨 필드. 목록이라 어느 행인지 실어야 하는데, 이 enum 은
    /// `Copy` 로 여기저기 값 복사돼 돌아서 String 을 못 넣는다 — 행 인덱스로 잡고
    /// 계정 삭제 시 포커스를 푼다(인덱스가 밀려 엉뚱한 행을 가리키지 않게).
    ClaudeAccountLabel(usize),
}

/// Clickable targets painted into the settings screen, collected each frame for
/// hit-testing. String-carrying variants (shell presets) keep this `Clone`.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum SettingsAction {
    Category(SettingsCat),
    CwdMode(&'static str),
    FocusCwdPath,
    /// 파일트리에서 파일을 열 때 쓸 방식 — `"builtin"` · `"app"` · `"terminal"`.
    FileOpenMode(&'static str),
    /// `"app"` 모드가 쓸 앱 이름. 빈 문자열 = OS 연결 프로그램.
    FileOpenApp(String),
    FocusFileOpenCmd,
    ToggleFileTree,
    ToggleFooter,
    /// Editor autosave quiet period in ms; 0 = off.
    AutosaveDelay(u64),
    ShellPreset(String),
    FocusShell,
    ThemeMode(&'static str),
    Accent(String),
    /// Font-size stepper: −1 / +1 logical px on the base cell font.
    FontSizeDelta(i8),
    /// Window-tab placement: "top" (title-strip tabs) or "side" (Warp strip).
    TabPosition(&'static str),
    ToggleClaudePersona,
    ToggleShimInject,
    ClaudeModel(String),
    ClaudeEffort(String),
    FocusClaudeExtra,
    /// 활성 Claude 계정 선택. 빈 문자열 = 기본 로그인(env 를 아예 안 붙임).
    ClaudeAccount(String),
    /// 계정 추가 — 저장소 dir 을 만들고, 그 dir 을 가리킨 채 `claude` 를 띄운
    /// 새 pane 을 연다. 로그인은 거노가 그 pane 에서 한 번 한다(OAuth 브라우저
    /// 흐름이라 우리가 대신 못 한다).
    AddClaudeAccount,
    /// 계정을 목록에서 뺀다. Keychain 항목은 건드리지 않는다 — 지우면 재로그인
    /// 말고는 복구가 없고, 남겨 둬도 해가 없다.
    RemoveClaudeAccount(String),
    /// 한도가 차면 다음 계정으로 알아서 넘어가는 스위치.
    ToggleAccountAutoswitch,
    /// 그 전환을 부르는 사용률(%).
    AccountAutoswitchPct(u32),
    /// 계정 라벨 텍스트 필드에 포커스(행 인덱스 — `SettingsInput` 이 Copy 라
    /// id 를 못 싣는다). 선택·삭제는 인덱스가 밀려도 안전하도록 id 로 받는다.
    FocusClaudeAccountLabel(usize),
    /// Open `~/.config/kasaterm/students/` in the OS file manager so the user
    /// can drop replacement character images there.
    OpenStudentsDir,
    /// Open `~/.config/kasaterm/characters.json` in the default editor to edit
    /// names / colors / persona text.
    OpenCharactersJson,
    /// Evict cached character textures so edited images reload on next paint.
    RefreshStudentAssets,
    /// Select a character in the Students list → load its persona into the edit
    /// buffer. Carries the character's display name.
    SelectStudent(String),
    /// Focus the persona multiline editor for the selected character.
    FocusStudentPersona,
}

/// Which dropdown a pane's status bar has open. `Path` lists the cwd's sibling
/// directories (click → cd that pane); `Branch` lists local branches (click →
/// checkout in that pane's repo).
#[derive(Clone, Copy, PartialEq, Eq)]
enum StatusbarMenu {
    Path,
    Branch,
}

/// Recompose conjoining Hangul jamo (NFD) back into precomposed syllables
/// (NFC). macOS returns filenames decomposed, so a Korean name like "한글"
/// arrives as `ㅎ ㅏ ㄴ ㄱ ㅡ ㄹ` and renders as scattered jamo / boxes. This
/// is the canonical Hangul composition (L+V[+T]) only — no full Unicode table,
/// since the visible breakage is Hangul-specific. Non-Hangul codepoints pass
/// through untouched, so a string with no jamo returns an identical copy.
fn nfc_hangul(s: &str) -> String {
    const S_BASE: u32 = 0xAC00;
    const L_BASE: u32 = 0x1100;
    const V_BASE: u32 = 0x1161;
    const T_BASE: u32 = 0x11A7;
    const L_COUNT: u32 = 19;
    const V_COUNT: u32 = 21;
    const T_COUNT: u32 = 28;
    const N_COUNT: u32 = V_COUNT * T_COUNT;
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        let c = ch as u32;
        // Trailing jamo onto an already-composed LV syllable → LVT.
        if (T_BASE + 1..T_BASE + T_COUNT).contains(&c) {
            if let Some(prev) = out.chars().last() {
                let p = prev as u32;
                if p >= S_BASE && (p - S_BASE) % T_COUNT == 0 && p < S_BASE + L_COUNT * N_COUNT {
                    out.pop();
                    if let Some(syl) = char::from_u32(p + (c - T_BASE)) {
                        out.push(syl);
                        continue;
                    }
                }
            }
        }
        // Vowel jamo onto a leading consonant → LV syllable.
        if (V_BASE..V_BASE + V_COUNT).contains(&c) {
            if let Some(prev) = out.chars().last() {
                let l = prev as u32;
                if (L_BASE..L_BASE + L_COUNT).contains(&l) {
                    out.pop();
                    let syl = S_BASE + ((l - L_BASE) * V_COUNT + (c - V_BASE)) * T_COUNT;
                    if let Some(syl) = char::from_u32(syl) {
                        out.push(syl);
                        continue;
                    }
                }
            }
        }
        out.push(ch);
    }
    out
}

/// Map a file extension to the syntax-highlighter language name (the same
/// names `syn_keywords`/`syn_line_comment` match on). Unknown → "" (the
/// highlighter still colors strings/numbers/comments generically).
fn code_lang_for_path(p: &std::path::Path) -> &'static str {
    match p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "rs" => "rust",
        "py" | "pyi" | "pyw" => "python",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "ts" | "tsx" => "typescript",
        "go" => "go",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" => "c++",
        "json" | "jsonc" => "json",
        "sh" | "bash" | "zsh" | "fish" => "bash",
        "sql" => "sql",
        "toml" | "lock" => "toml", // Cargo.lock is TOML
        "yml" | "yaml" => "yaml",
        "html" | "htm" => "html",
        "css" | "scss" | "sass" => "css",
        _ => "",
    }
}

/// A pane's cwd + git badge, published by the GUI for the socket thread's
/// `/layout` so the BA GUI can draw a Warp-style status bar on plain terminal
/// tiles. Both fields come from caches the GUI already maintains off the
/// lsof/git hot path (`pane_cwd_cache` / `window_git`).
#[derive(Clone, Debug)]
pub(crate) struct PaneStatus {
    pub(crate) cwd: std::path::PathBuf,
    pub(crate) badge: Option<kasa_mcp::git::GitBadge>,
    /// Shared handle to the pane's OSC 133 command blocks. The socket `/blocks`
    /// reads it directly — no clone, no `App.pty` access. None for non-terminal
    /// tiles or panes whose PTY isn't tracked.
    pub(crate) blocks:
        Option<std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<kasa_pty::CommandBlock>>>>,
}

struct App {
    window: Option<Arc<Window>>,
    /// Set when `KASATERM_RENDERER=gpu`. Mutually exclusive with
    /// `sugarloaf` — both own a wgpu Surface, only one can present.
    gpu: Option<gpu::GpuRenderer>,
    /// 살아 있는 rust-analyzer 하나 — **프로젝트 루트당 하나**. 첫 rust 파일을
    /// 편집기로 열 때 뜨고, 그 전엔 아예 띄우지 않는다: 인덱싱에 수십 초와 수 GB
    /// 를 쓰는 프로세스라 쓸지 모르는 채로 켜 두면 안 된다.
    lsp: Option<lsp::LspClient>,
    /// 마우스가 편집기 위에 멎어 있는 자리. 없으면 툴팁도 없다.
    hover: Option<HoverState>,
    /// 답을 기다리는 정의 이동 요청 id. 응답은 왕복이라 클릭한 그 자리에서
    /// 기다릴 수 없어, 틱이 `lsp_goto_pump` 로 받아 파일을 연다.
    lsp_goto: Option<i64>,
    tmux: Option<Arc<TmuxSession>>,
    /// Phase C backend. Mutually exclusive with `tmux` — exactly one
    /// is `Some` after `start_backend`. Selection driven by the
    /// KASATERM_BACKEND env var; defaults to PTY now that the Phase C
    /// path is the recommended one (no tmux daemon, no focus-events
    /// warnings from Claude Code).
    /// All live PTY sessions, keyed by pane id. Empty when running in
    /// tmux mode. Multi-pane PTY mode inserts one entry per split.
    pty: HashMap<String, Arc<kasa_pty::PtySession>>,
    /// BSP layout tree for multi-pane PTY mode. `None` in tmux mode —
    /// the tmux daemon owns the layout there and ships it via
    /// `%layout-change` instead.
    pty_layout: Option<kasa_pty::PtyLayout>,
    /// Monotonic counter for the next "%N" pane id when splitting.
    next_pane_id: u32,
    /// Queued `claude --resume …\n` injections for restored panes, one per
    /// claude pane, fired once each pane's shell prompt is up. Holds the
    /// PtySession Arc directly so it works for panes in any session (active or
    /// stashed background). (session, command, time-to-send).
    pending_restores: Vec<(Arc<kasa_pty::PtySession>, String, std::time::Instant)>,
    /// 지글 원복 큐 — NudgePaneResize 가 1행 줄인 pane 을 (원 cols, 원 rows)로 되돌릴
    /// 시각. pending_restores 와 같은 drain 사이클에서 시간 도달분만 발사.
    pending_unjiggle: Vec<(String, u16, u16, std::time::Instant)>,
    /// Headless verification: clean-exit (runs `exiting` → save_session_state)
    /// at this instant when KASATERM_AUTOQUIT_MS is set. None disables it.
    autoquit_at: Option<std::time::Instant>,
    /// Pending GPU self-captures `(deadline, png path)` from KASATERM_AUTOCAPTURE_MS
    /// (콤마로 여러 시각 지정 가능 — 애니메이션 프레임 비교 검증용).
    /// `about_to_wait` arms `gpu.capture_next` once a deadline passes so the
    /// next render reads the frame back to a PNG — no screen-record permission.
    pending_capture: Vec<(std::time::Instant, String)>,
    /// Headless git-panel demo `(deadline, action)` from KASATERM_AUTOGIT —
    /// "diff" expands the first changed file's inline diff, "modal" opens the
    /// commit modal, so those states can be self-captured without clicking.
    pending_autogit: Option<(std::time::Instant, String)>,
    /// Queued split directions driven by KASATERM_AUTOSPLIT — headless
    /// repro for the multi-pane render path. Empty in normal use.
    autosplit_plan: Vec<kasa_pty::SplitDir>,
    autosplit_at: Option<Instant>,
    /// Headless file-open repro. KASATERM_AUTOOPEN=<path> fires
    /// `open_file_split` after AUTOOPEN_MS so the preview-pane + file-tree
    /// highlight path can be screenshotted without a real double-click.
    autoopen_path: Option<std::path::PathBuf>,
    autoopen_at: Option<Instant>,
    /// Headless confirm-modal repro: deadline to fire `confirm_or_close_window`.
    autoconfirm_at: Option<Instant>,
    /// Headless tab-drag simulation. KASATERM_AUTODRAG="src:from:dst"
    /// (e.g. "%2:0:%0") fires `simulate_tab_merge` after AUTODRAG_MS so
    /// the cross-pane merge path can be verified without a real mouse.
    autodrag_plan: Option<(String, usize, String)>,
    autodrag_at: Option<Instant>,
    /// Headless repro for a cross-window pane move (KASATERM_AUTOPANEMOVE=<dst
    /// window idx>): relocates the active window's first leaf next to that
    /// window's first leaf via `move_pane`, exercising the sidebar-chip drop
    /// path without a real drag.
    autopanemove_dst: Option<usize>,
    autopanemove_at: Option<Instant>,
    /// Headless repro for the drag *preview* (KASATERM_FORCE_DRAG="%N"): parks
    /// the named leaf in an active header_drag with the cursor over a sibling,
    /// then stops — so a capture shows the floating ghost + vacated-slot scrim
    /// without committing the drop.
    force_drag_leaf: Option<String>,
    force_drag_at: Option<Instant>,
    /// Headless repro for the window sidebar: number of extra windows left to
    /// spawn (KASATERM_AUTOWINDOWS) and when the next one fires. 0 disables.
    autowindow_left: usize,
    autowindow_at: Option<Instant>,
    /// Headless repro for the sidebar toggle (KASATERM_AUTOTOGGLE_SIDEBAR_MS):
    /// flips the sidebar once at this instant so a screenshot can capture the
    /// collapsed-grid state without a human clicking the title-bar button.
    autotoggle_sidebar_at: Option<Instant>,
    /// Headless arona-panel toggle deadline (KASATERM_AUTOARONA_MS).
    autoarona_at: Option<Instant>,
    /// Extra sidebar flips queued after the first (KASATERM_AUTOTOGGLE_SIDEBAR_N),
    /// 1.5s apart, to stress hide↔show reflow without a human.
    autotoggle_left: u32,
    /// Headless repro for the in-pane tab bar (KASATERM_AUTOTABS=N): pushes N
    /// dummy tabs onto the active pane once so a screenshot can capture the
    /// multi-tab header without a human clicking the "+". 0 disables.
    autotabs_n: usize,
    autotabs_at: Option<Instant>,
    /// Pane ids whose PTY reader thread has disconnected (shell exited
    /// or PTY closed). Drained on the main thread in `about_to_wait`
    /// so the tree mutation runs without holding the workspace lock
    /// across a session drop.
    dead_panes: Arc<Mutex<Vec<String>>>,
    /// Set when running as a GUI attached to a daemon (KASATERM_DAEMON=1):
    /// PTY input / resize / scroll route to the daemon over this client
    /// instead of a local PtySession. None in the default in-process mode.
    /// Arc so background timers (autosend) can hold a handle too.
    ws: Arc<Mutex<Workspace>>,
    /// All sessions (tmux-style tabs) by tab index. The visible session's slot
    /// holds `None` — its live state is the fields above (pty/pty_layout/ws).
    /// Switching swaps a slot in/out; background sessions keep running via
    /// their own ws Arc, captured by their pump_pty_screens threads.
    sessions: Vec<Option<Session>>,
    /// Index into `sessions` of the visible session (its slot is None).
    active_session: usize,
    /// Windows of the *visible* session, by index. The active window's slot
    /// holds `None` — its live layout is `pty_layout` above. A window is a
    /// pane grouping (one BSP tree); a session can hold several. Switching
    /// windows swaps `pty_layout` ↔ `windows[idx]` while `pty`/`ws` stay put,
    /// so the panes' shells keep running across the switch (same session).
    /// When the visible session is stashed, these move into its `Session`.
    windows: Vec<Option<kasa_pty::PtyLayout>>,
    /// Index into `windows` of the visible window.
    active_window: usize,
    /// Measured cell geometry from sugarloaf — see `compute_cell_metrics`.
    cell: CellGeom,
    preedit: String,
    in_preedit: bool,
    /// (committed text, cursor-at-commit). gpu paints frames so fast it
    /// draws the moment AFTER a syllable commits but BEFORE the shell's
    /// echo arrives, so the preedit ("ㄴ") briefly shows where the
    /// committed glyph ("안") will land. We overlay the committed text
    /// in front of the preedit until the echo lands (cursor advances ⇒
    /// `cursor != stored`), which is what sugarloaf got for free by
    /// being slow enough to wait for the echo.
    commit_overlay: Option<(String, (u16, u16))>,
    /// True between `Ime::Enabled` and `Ime::Disabled`. Tracks whether
    /// the OS IME owns this keyboard at all — when active, Hangul (and
    /// other CJK) keystrokes are double-delivered (KeyboardInput.text
    /// + Ime::Preedit/Commit) and we have to drop the keyboard side
    /// even before the first Preedit lands.
    ime_active: bool,
    /// In-process Hangul jamo → syllable composer. We drive this from
    /// the KeyboardInput path whenever the OS keyboard layout hands us
    /// a Hangul jamo — macOS's NSTextInputContext doesn't fire
    /// Ime::Preedit for the *first* keystroke after a script switch
    /// (the jamo arrives only via KeyboardInput.text), so to compose
    /// "ㄱ + ㅏ → 가" reliably from the very first key we route every
    /// jamo through our own Composer instead of trusting macOS to
    /// queue it for us.
    hangul: kasa_ime::Composer,
    /// 위 조합기를 지금 쓰고 있는 입력 문맥. `ime_retarget` 이 이 값이 바뀌는
    /// 순간 조합 중이던 음절을 **떠나는 쪽에** 확정시킨다 — 안 그러면 터미널에서
    /// 치던 글자가 편집기에 떨어지고, Backspace 는 그 잔재만 갉는다.
    ime_focus: Option<ImeFocus>,
    /// (pane_id, close_rect) for every visible pane header. Populated
    /// by `render_frame` and consumed by the MouseInput handler so a
    /// click on the × button closes that pane.
    pane_header_rects: Vec<(String, (f32, f32, f32, f32))>,
    /// ghostty식 pane 핸들(⋮) hit rect: (pane id, logical rect). 상단 중앙에
    /// 평소 흐릿하게 상시 표시, 클릭=컨트롤 메뉴(Phase 3)·드래그=pane 이동(Phase 4).
    pane_handle_rects: Vec<(String, (f32, f32, f32, f32))>,
    /// pane 상단 띠(box 높이 30%) hit rect: (pane id, logical rect). 이 영역에
    /// 커서가 들어오면 ⋮ 가 흐릿하게 등장한다(평소엔 완전히 숨김).
    pane_top_zones: Vec<(String, (f32, f32, f32, f32))>,
    /// ⋮ 핸들 위 커서 여부 — 손모양 커서 transition + ⋮ 강조 redraw 트리거.
    handle_hovered: bool,
    /// pane 상단 띠(top_zone) 안에 커서가 있는지 — ⋮ 등장/소멸 redraw 트리거.
    handle_zone_hovered: bool,
    /// ghostty ⋮ 메뉴가 열린 pane id(None=닫힘). ⋮ 클릭으로 토글.
    handle_menu: Option<String>,
    /// ⋮ 메뉴 버튼 hit rect: (액션, logical rect). render가 매 프레임 채움.
    handle_menu_hits: Vec<(ActionKind, (f32, f32, f32, f32))>,
    /// 타이틀바 Claude 계정 칩의 드롭다운이 열려 있는지. ⋮ 핸들 메뉴와 같은 짝 —
    /// render 가 매 프레임 rect 를 채우고 handler 가 이전 프레임 rect 로 힛테스트.
    account_menu: bool,
    /// 사용량 pill 의 rect(클릭 = 계정 드롭다운 토글). pill 을 안 그리는 프레임엔 None.
    account_chip_rect: Option<(f32, f32, f32, f32)>,
    /// 드롭다운 항목 hit rect.
    account_menu_hits: Vec<(AccountMenuItem, (f32, f32, f32, f32))>,
    /// Rendered markdown content height (logical px) per pane id, published by
    /// the renderer each frame. The scroll handler clamps scroll_offset to
    /// (content_h - visible_h) so a markdown pane can't over-scroll.
    md_content_h: HashMap<String, f32>,
    /// Document-space y of each rendered block per pane id, published by the
    /// renderer each frame (see `Gpu::md_block_ys`). `set_md_mode` reads it to
    /// carry the scroll position across a Raw↔Render toggle.
    md_block_ys: HashMap<String, Vec<f32>>,
    /// Pending "put this source line at the top" request per pane id, set by a
    /// Raw→Render toggle. The new layout's block positions only exist after a
    /// draw, so the renderer consumes this once `md_block_ys` is fresh.
    md_scroll_anchor: HashMap<String, usize>,
    /// Screen-space rects of every word the rendered view drew last frame, per
    /// pane id: (x, y, w, h, text). A document view has no cell grid, so this is
    /// what a drag selection and its copy resolve against.
    md_word_rects: HashMap<String, Vec<(f32, f32, f32, f32, String)>>,
    /// Live drag selection in the rendered view. `None` = nothing selected.
    md_render_sel: Option<MdRenderSel>,
    /// 마우스 노치 스크롤의 목표 위치와 마지막 보간 시각(pane id 별). 트랙패드
    /// 픽셀 델타는 그 자체가 부드러워 즉시 반영한다 — 보간하면 손가락보다 늦게
    /// 미끄러진다. 노치는 한 번에 세 줄을 뛰어 계단으로 읽히니 목표만 받아 두고
    /// `tick_md_scroll` 이 프레임마다 지수로 따라간다. 비어 있으면 애니 없음.
    /// (MarkdownPane 이 아니라 여기 두는 이유: pane 구조체에 필드를 더하면 생성부
    /// 다섯 곳이 동시에 깨져 병렬 작업이 서로를 막는다.)
    md_scroll_anim: HashMap<String, (f32, Instant)>,
    /// 클릭 연타 기억: (마지막 클릭 시각, x, y, 연타 횟수). 더블클릭 단어선택 ·
    /// 트리플클릭 줄선택 판정용. 레포에 더블클릭 판정이 아예 없어(터미널조차)
    /// 새로 두는 자리다.
    md_click_streak: Option<(Instant, f32, f32, u8)>,
    /// Find-bar button hit boxes (pane id, button, logical-px rect), rebuilt by
    /// the renderer each frame. Tested before the body box below, since the bar
    /// floats over the editor — a click on it must not also move the caret.
    md_find_rects: Vec<(String, FindBtn, (f32, f32, f32, f32))>,
    /// Raw-editor body box (logical px) per pane id, published by the renderer
    /// each frame. A click in this box hit-tests to a caret position so the
    /// mouse can place the edit cursor (see `md_click_caret`).
    md_body_rects: HashMap<String, (f32, f32, f32, f32)>,
    /// In-pane tab hit rects: (pane id, tab index, logical rect). Click
    /// switches that pane's active_tab. Rebuilt each header paint.
    pane_tab_rects: Vec<(String, usize, (f32, f32, f32, f32))>,
    /// Per-tab × close hit rects: (pane id, tab index, logical rect).
    pane_tab_close_rects: Vec<(String, usize, (f32, f32, f32, f32))>,
    /// Per-tab pop-out (external-link) hit rects for file tabs: (pane id, tab
    /// index, logical rect). Click moves that tab's editor into its own wgpu
    /// window (auxwin.rs). Rebuilt each header paint.
    pane_tab_popout_rects: Vec<(String, usize, (f32, f32, f32, f32))>,
    /// "+" new-tab button hit rect per pane: (pane id, logical rect).
    pane_plus_rects: Vec<(String, (f32, f32, f32, f32))>,
    /// Panes folded into the active session's dock (bottom-bar chips). Mirror of
    /// the daemon's per-session docked list, so a session switch shows only this
    /// 지구's dock — render-only.
    docked: Vec<stream::DockedView>,
    /// True once the first daemon State has been fully applied. Until then every
    /// State runs the full layout-adopt path; afterwards a State whose active-
    /// window leaves (in order) + dock are unchanged (a cwd-only 1s poll) skips
    /// the heavy resize/repaint so idle stays at 0 GPU passes (ghostty-fast).
    /// Dock chip hit rects: (pane id, logical rect). Click restores (undock).
    dock_chip_rects: Vec<(String, (f32, f32, f32, f32))>,
    /// Dock chip × hit rects: (pane id, logical rect). Click kills the pane.
    dock_chip_close_rects: Vec<(String, (f32, f32, f32, f32))>,
    /// When the "복사됨" copy toast started animating. Drives its fade in the
    /// overlay pass; `None` once faded out. Set on a successful block copy.
    copy_toast_at: Option<Instant>,
    /// Throttle for `refresh_pane_activity`: the working-bar/completion-toast
    /// busy scan walks every pane's grid, so it runs at most a few times a
    /// second rather than per frame. `None` until the first scan.
    pane_busy_check: Option<Instant>,
    /// Last time each pane's grid showed a spinner glyph. The claude spinner
    /// blanks/scrolls between frames, so a raw per-scan check flickers
    /// working↔idle and fires a bogus "완료" toast every blink. We hold `busy`
    /// for `BUSY_GRACE` after the last spinner sighting so only a real stop
    /// (grace elapsed) counts as completion.
    pane_last_busy: HashMap<String, Instant>,
    /// pane id → (transcript mtime, bg_active) — an mtime-gated cache for the
    /// header pulse bar. An idle pane's transcript rarely changes, so the bar's
    /// "background/Monitor running" check reads the tail only when mtime moves.
    pane_bg_mtime: HashMap<String, (std::time::SystemTime, bool)>,
    /// (window index, rect) for every window tab in the left sidebar.
    /// Populated by the render path, consumed by the MouseInput handler so
    /// a click switches windows. Logical px.
    window_tab_rects: Vec<(usize, (f32, f32, f32, f32))>,
    /// (window index, close-× rect) for each window tab. Only present when
    /// there's more than one window (the last window can't be closed).
    window_tab_close_rects: Vec<(usize, (f32, f32, f32, f32))>,
    /// Window-tab overflow windowing: index of the first tab shown in the
    /// strip. The strip shows a contiguous run of whole tabs (no partial
    /// clipping — the renderer has no scissor); the wheel steps this, and
    /// switch/new reveal the active tab via `win_tab_reveal`.
    win_tab_first: usize,
    /// How many window tabs the strip fit last frame. Written by the render
    /// pass (sidebar_layout output), read by the wheel handler for clamping.
    win_tab_vis: usize,
    /// Sub-step wheel accumulator for the tab strip (px; one tab per 48).
    win_tab_wheel_accum: f32,
    /// Sidebar "+" new-window button rect, logical px. None before first paint.
    new_window_btn_rect: Option<(f32, f32, f32, f32)>,
    /// Whether the "+" shell picker popup is open. Toggled by clicking the
    /// sidebar "+"; dismissed on item-click or an outside click.
    shell_menu_open: bool,
    /// Popup item hit rects `(shell_command, rect)`, logical px. Rebuilt each
    /// paint while the menu is open; consumed by the MouseInput handler.
    shell_menu_hits: Vec<(String, (f32, f32, f32, f32))>,
    /// Shell command the next `spawn_session_pane` should launch instead of
    /// the default. Set by a shell-picker selection, consumed (taken) once.
    pending_shell: Option<String>,
    /// Per-window (name, cwd) tab labels, by window index. Refreshed on a
    /// throttle (cwd resolution shells out to lsof, so never per-frame).
    window_labels: Vec<(String, String)>,
    window_labels_at: Option<Instant>,
    /// Explicit window/session name overrides by window index (`window.rename`).
    /// `refresh_window_labels` derives labels from the representative pane, but
    /// rename override 는 세션 라벨이 어떤 leaf 가 대표든 유지돼야 한다
    /// pane is the representative — this map wins over the derived name. Not
    /// persisted: 호출자가 매번 재적용하므로, 재시작 후
    /// 재지정되면 윈도우가 다시 마킹된다.
    window_name_override: HashMap<usize, String>,
    selection: Option<Selection>,
    drag_anchor: Option<(u16, u16)>,
    /// A left-press that landed on a detected URL. Holds (url, press_px) so a
    /// release that stayed put (a click, not a drag) opens it; a drag clears it.
    link_armed: Option<(String, (f32, f32))>,
    /// In-flight pane-divider drag: the BSP tree path of the split being
    /// resized plus its axis. `Some` while the user holds the mouse on a
    /// seam; each motion event re-derives the ratio from the cursor.
    resize_drag: Option<(Vec<u8>, kasa_pty::SplitDir)>,
    /// Last cell-quantised divider position fired through `resize_backend`.
    /// Lets the divider-drag handler skip the heavy PTY reshape on every
    /// sub-cell wiggle of the cursor — only crossing a cell boundary
    /// SIGWINCHes the panes (Claude Code reflows are very expensive).
    last_divider_pos: Option<u16>,
    /// Instant of the most recent PTY reshape fired during a divider drag.
    /// Layout ratio still updates on every cursor move (live seam) but
    /// SIGWINCH is throttled to ~10 Hz here — Claude Code's full-screen
    /// repaint on every SIGWINCH otherwise turns into a melted-glass feel.
    last_divider_pty_resize: Option<std::time::Instant>,
    /// In-flight header drag-and-drop: which pane the user grabbed by its
    /// header, the press position, and whether the cursor has moved past
    /// the threshold (only then does releasing relocate, so a plain click
    /// still just focuses the pane).
    header_drag: Option<HeaderDrag>,
    /// In-flight in-pane tab reorder. `Some` while the user holds the mouse
    /// on a tab; releasing either reorders (if it became a drag) or switches
    /// the active tab (if it stayed a click).
    tab_drag: Option<TabDrag>,
    /// 라이브 드래그 이동의 원본 레이아웃 백업. active 드래그가 처음 layout을
    /// 실제로 건드릴 때 캡처되고, 드롭이 무효(빈 곳·Center·자기 자신)일 때 이걸로
    /// 복원한다. 드래그가 끝나면 None.
    drag_orig_layout: Option<kasa_pty::PtyLayout>,
    /// 라이브 드래그가 마지막으로 실제 적용한 `(target, zone)`. 같으면 reshape를
    /// 건너뛰어(throttle) zone이 바뀔 때만 SIGWINCH가 나가게 한다.
    drag_live_applied: Option<(String, DropZone)>,
    /// In-flight image-pane pan drag: `(pane_id, start_cursor_px, base_pan)`.
    /// `Some` while dragging a zoomed image's body; CursorMoved updates the
    /// active tab's `image_pan_*` from `base_pan + (cursor - start)`.
    image_pan_drag: Option<(String, (f32, f32), (f32, f32))>,
    /// In-flight file-tree → terminal path drag. `Some` while a tree row is
    /// held; releasing over a pane types the path into that shell.
    /// Inline "new file / folder" entry. `Some((is_dir, name_buffer))` while
    /// the user is naming a freshly-requested entry; Enter creates it under
    /// the tree root, Esc cancels. Keystrokes route here like the search box.
    /// Hit rects for the new-folder / new-file buttons beside the search box,
    /// refreshed each frame.
    /// Row rect of the inline new-entry naming box (for the I-beam hit-test).
    /// Tree row the user last clicked — the Cmd+Delete target.
    /// Whether the text (I-beam) mouse cursor is currently shown, so we only
    /// flip the OS cursor on the transition in/out of an input box.
    text_cursor_shown: bool,
    /// Active "close while a process is running?" modal. While `Some`, the
    /// dialog is painted over everything and swallows input until the user
    /// picks 취소/닫기.
    confirm_close: Option<ConfirmClose>,
    /// Confirm-modal button hit rects, refreshed each frame: `(btn, rect)`.
    confirm_btn_rects: Vec<(ConfirmBtn, (f32, f32, f32, f32))>,
    /// Chrome-style restore prompt shown at launch: the saved session state
    /// awaiting the user's 복원/새로 시작 decision. While `Some`, a modal is
    /// painted over the fresh session and swallows input.
    restore_prompt: Option<serde_json::Value>,
    /// Restore-prompt button hit rects, refreshed each frame: `(btn, rect)`.
    restore_btn_rects: Vec<(RestoreBtn, (f32, f32, f32, f32))>,
    /// 자동 스냅샷(강제 종료 대비) 상태 — 마지막 저장 시각, 그 뒤로 깨어난 적이
    /// 있는지, 그때 쓴 내용의 해시. `autosave_session` 참조.
    session_saved_at: std::time::Instant,
    session_touched: bool,
    session_saved_hash: Option<u64>,
    /// Currently hovered in-pane tab `(pane_id, tab_idx)`. Drives the
    /// hover-only × and brighter text on inactive tabs.
    pane_tab_hover: Option<(String, usize)>,
    /// Image pane action button hit rects (zoom in/out, rotate, reset),
    /// rebuilt each header paint. Mouse handler dispatches on these.
    image_btn_rects: Vec<(String, ImageBtn, (f32, f32, f32, f32))>,
    /// Live width of the left window-tab sidebar (logical px). User-draggable
    /// via the right edge; defaults to `SIDEBAR_W`. `effective_sidebar_w()`
    /// reads this when visible.
    sidebar_w_logical: f32,
    /// In-flight sidebar resize drag — `(start_cursor_x, start_width)`.
    sidebar_resize: Option<(f32, f32)>,
    /// Cell grid size last published to the PTY backend by the window-resize
    /// path. Live-resize fires Resized at ~60Hz with pixel-granular sizes,
    /// but only crosses a cell boundary every 16-32px — without this guard
    /// we re-shape every pane PTY (alacritty Term::resize + SIGWINCH) and
    /// re-publish layout on every wiggle of the pointer.
    last_resized_cells: (u16, u16),
    /// During a macOS live-resize we deliberately do NOT call surface.configure
    /// or render — the CAMetalLayer keeps its old IOSurface and gravity=topLeft
    /// anchors it to the top-left while AppKit stretches the layer bounds
    /// (ghostty's trick on top of wgpu). The final size that came in during
    /// the drag is stashed here so we can flush it once the user lets go.
    pending_resize: Option<winit::dpi::PhysicalSize<u32>>,
    /// Pane that owns the in-flight mouse reporting drag. `Some(pane_id)`
    /// when we forwarded a button-press into a mouse-reporting TUI and
    /// are now relaying motion + release into the same pane. None means
    /// no mouse-reporting drag is active; selection logic owns the
    /// pointer.
    mouse_forward_pane: Option<String>,
    /// Last left-click timestamp + position. Used only for the
    /// title-strip double-click → window-zoom shortcut. macOS handles
    /// this for us when the OS owns the titlebar, but our
    /// fullsize_content_view setup means we intercept those clicks.
    last_left_click: Option<(Instant, (f32, f32))>,
    /// Last file-tree row click (time + path). A second click on the *same*
    /// file row within the double-click window opens it in a split — folders
    /// keep their single-click expand, so files need their own gate.
    last_tree_click: Option<(Instant, std::path::PathBuf)>,
    /// Pane id currently zoomed (tmux-style): rendered alone filling the work
    /// area with the other panes hidden, until toggled off. GUI-local render
    /// state — the daemon's layout tree is untouched (like a divider ratio).
    zoomed_pane: Option<String>,
    /// Pre-maximize window frame (Cocoa screen coords: x, y, w, h), stashed
    /// when a title-strip double-click maximizes so the next double-click can
    /// snap the window back instantly. `None` = currently un-maximized. See
    /// `gpu::toggle_maximize_no_anim` — we drive the frame ourselves with
    /// `animate:NO` to kill AppKit's slow zoom animation.
    saved_window_frame: Option<(f64, f64, f64, f64)>,
    /// A titlebar press that hasn't yet decided between "click" and "drag".
    /// We defer `window.drag_window()` (which enters AppKit's modal move
    /// loop and would eat the second click of a double-click) until the
    /// pointer actually moves past a small threshold. Holds the press
    /// position; cleared on release or once the drag starts.
    titlebar_drag_pending: Option<(f32, f32)>,
    /// Cached value of the OS window title — `window.set_title` is
    /// cheap but not free, so we only call it when the resolved
    /// label actually changes.
    last_window_title: Option<String>,
    /// Deadline keeping the Claude "busy" anim alive after the
    /// spinner row briefly disappears from the grid. Without this,
    /// fast redraws toggle between "✱ claude" and the live status
    /// every frame because Claude Code repaints the spinner phase
    /// across separate cells. 800ms of stickiness smooths it out.
    #[allow(dead_code)]
    claude_busy_until: Option<Instant>,
    /// Most recent claude status line we lifted from the grid. Kept
    /// so the titlebar stays on the last "✻ Brewed for Ns" frame
    /// while Claude Code is mid-repaint and the marker row briefly
    /// vanishes. Cleared when the busy window expires.
    #[allow(dead_code)]
    last_claude_status: Option<String>,
    /// Per-pane collab activity (status + intent) from the daemon's StateView —
    /// the cross-window busy source. A "working" pane draws a header bar + a
    /// sidebar-window dot; a working→idle flip fires the completion toast. Keyed
    /// by surface id, replaced wholesale on each StateView.
    pane_activity: HashMap<String, crate::stream::PaneStatusView>,
    /// Whether our window currently has OS focus. Drives notification
    /// suppression: a completion alert for the already-focused active pane is
    /// pointless (the user is looking right at it), so we skip the desktop
    /// alert when `window_focused && notify target == active pane`.
    window_focused: bool,
    /// Pane id → instant a completion notification flashed its header, so the
    /// render pass can pulse it for a beat then let it settle.
    notify_flash: HashMap<String, std::time::Instant>,
    /// Panes whose last turn finished and the user hasn't typed into since —
    /// drives the student's cheer (arms-up) standing pose. Set on turn
    /// completion, cleared on the next `forward_key` into that pane, so the
    /// cheer persists until the user re-engages (not a brief flash).
    turn_done_panes: std::collections::HashSet<String>,
    /// Window indices with an *unseen* notification (a pane finished / needs
    /// attention while that window was in the background). The sidebar tab
    /// pulses until the user switches to that window, which clears the entry —
    /// a persistent "you missed this" cue, unlike the brief `notify_flash`.
    window_alert: std::collections::HashSet<usize>,
    /// Panes that fired a notify/attention while not being looked at and
    /// haven't been opened since — drives the Dock badge count. Cleared when
    /// the pane becomes the focused active pane (`sync_dock_badge`).
    unread_panes: std::collections::HashSet<String>,
    /// Last value pushed to the Dock badge, so AppKit is only touched on change.
    dock_badge_n: usize,
    /// Collab completion toast + approval card, grouped into a sub-struct
    /// (state.rs) so collab-UI work touches one file — CLAUDE.md 병렬 규칙.
    collab: state::CollabState,
    /// 승인 프롬프트가 떠 있는 pane → "사용자 직행(단독)인가". 그리드 스캔
    /// (`route_approval_prompts`)의 edge-trigger 상태: 새로 뜨면 라우팅 1회,
    /// 풀리면 board waiting 플래그까지 함께 걷는다.
    pane_prompt_wait: HashMap<String, bool>,
    /// When we last recomputed the macOS window title. Rate-limits
    /// `maybe_update_window_title` to ~200ms because it locks the
    /// workspace + calls `ps -A` (process-tree lookup) on every hit,
    /// and a wheel burst fires `RedrawRequested` 60+ times per
    /// second.
    last_window_title_check: Option<Instant>,
    /// Per-pane shell cwd cache (pane id → working dir), feeding the header
    /// breadcrumb. Refreshed on a timer off the render path: `pid_cwd` shells
    /// out to `lsof`, so resolving it per pane on every frame would spawn a
    /// burst during a scroll/hover storm. See `refresh_pane_cwds`.
    /// 아로나 자동 시작(P5): characters 있는 방의 첫 pane 에 띄울
    /// claude 명령. start_pty 에서 가드 통과 시 세팅, 셸 prompt-end(OSC133) 감지
    /// 또는 타임아웃 시 1회 주입 후 None. solo·무테마면 애초에 None(무동작).
    pane_cwd_cache: HashMap<String, std::path::PathBuf>,
    /// pane 이 "보고 있는" 경로 오버라이드(SocketViewCwd 로 도착) — 파일트리 루트가
    /// 셸 cwd 보다 우선한다. 셸이 cd 로 움직이면(refresh_pane_cwds 에서 감지) 해당
    /// pane 의 오버라이드를 버려 stale 고착을 막는다(claude 살아 있으면 statusline
    /// 이 곧 재보고).
    pane_view_cwd: HashMap<String, std::path::PathBuf>,
    /// 방별 collab 분리(거노). `pending_room`: 다음 spawn 할 pane 의 방 id(셸 env
    /// KASATERM_ROOM 주입 + ws.pane_room 기록용). pane→방 매핑은 ws.pane_room(공유).
    pending_room: Option<String>,
    next_room_seq: u32,
    /// 다음 spawn 할 pane 에 강제할 캐릭터(new_room_with_character 가 세팅). None 이면
    /// 빈 슬롯 순환 배정(미도리→모모이→…). 배정 결과는 KASATERM_CHARACTER env + /tmp 마커.
    pending_character: Option<String>,
    /// pane id → claude --session-id(백엔드가 spawn 시 생성). shim 이 env 로 받아 고정,
    /// transcript jsonl 파일명 안정화 → resume 시 같은 대화 복원.
    pane_session_id: HashMap<String, String>,
    /// pane id → claude 실제 sessionId(transcript stem, `SocketSessionBound` 로 도착).
    /// pane_session_id(백엔드 발급)와 달리 fork/detach 시 갈라진 진짜 세션이라, 이걸로
    /// bg_agents 를 조회해 포크/백그라운드 배지를 판정한다.
    pane_claude_sid: HashMap<String, String>,
    /// cmux socket backend 핸들 — ResumeSession(attach/재개)이 세션 id 를 아는 유일한
    /// 시점에 pane↔transcript 를 bind_transcript 로 즉석 확정하기 위해 보관. attach 뷰는
    /// bind hook 이 안 떠서 board discovery 의 recent-jsonl 추측이 남의 활성 세션에
    /// 오귀속됐다(거노: 왼쪽 pane 둘 다 프라나).
    socket_backend: Option<std::sync::Arc<socket::PtyBackend>>,
    /// claude sessionId → parentSessionId(background kind 세션만). `claude agents
    /// --json --all` 폴러(handler.rs resumed)가 3초마다 갱신. 타이틀바 배지·학생 유지
    /// (부모 캐릭터 상속)가 읽는다. 백그라운드 세션이 아니면 키 없음.
    bg_agents: std::sync::Arc<std::sync::Mutex<HashMap<String, Option<String>>>>,
    /// claude 5시간 사용량 창 사용률(%). handler.rs resumed 의 폴러가 로컬
    /// `/claude-usage`(oauth/usage 프록시)를 60초마다 조회해 채운다. 타이틀바
    /// 우상단 사용량 pill 이 읽는다. 토큰 없음/실패면 None → pill 숨김.
    claude_usage: std::sync::Arc<std::sync::Mutex<Option<f32>>>,
    /// Per-pane controlling tty short name (pane id → "ttys004") from the
    /// daemon's StateView. Shown in the pane header; fixed per pane.
    pane_tty_cache: HashMap<String, String>,
    /// Sidebar git badge cache (cwd → branch/+ins/-del). A background thread
    /// polls each window's cwd directly (not via the daemon), so this stays
    /// off `%0`'s daemon path. Render reads it; the poller writes it.
    window_git: std::sync::Arc<std::sync::Mutex<HashMap<std::path::PathBuf, kasa_mcp::git::GitBadge>>>,
    /// cwds the git poller should refresh, set from the current windows' repr
    /// cwd just before the sidebar paints. Shared with the poll thread.
    git_poll_cwds: std::sync::Arc<std::sync::Mutex<Vec<std::path::PathBuf>>>,
    /// surface_id → {cwd, git badge} snapshot for the BA GUI's `/layout` (read
    /// off-thread by `socket::PtyBackend::window_layout`). The GUI fills it each
    /// frame from `pane_cwd_cache` + `window_git` (both already off the lsof/git
    /// hot path), so the socket thread never shells out — it lets the BA GUI
    /// draw a Warp-style cwd/branch/diff bar on plain (non-claude) terminal
    /// tiles, which carry no board row to read cwd from. CLAUDE.md 병렬 규칙.
    pane_status_pub: std::sync::Arc<std::sync::Mutex<HashMap<String, PaneStatus>>>,
    /// Right-hand git column + commit modal + path/branch dropdowns, grouped
    /// into a sub-struct (state.rs) so git-UI work touches one file, not this
    /// App definition — CLAUDE.md 병렬 규칙. (badge poller `git_poll_cwds` and
    /// the file-tree `git_ignore_*` stay separate — different domains.)
    git: state::GitState,
    /// 우측 칼럼의 Info 탭 — 활성 pane 셸 아래 프로세스 + listen 포트. 칼럼
    /// 폭/닫기는 `git` 과 공유하고 본문과 갱신 스레드만 여기 있다(state.rs).
    info: state::InfoState,
    /// Per-pane status bar (cwd/branch/diff chips at each pane's foot) + the
    /// open dropdown's state. Grouped into a sub-struct (state.rs) so statusbar
    /// work touches one file, not this App definition — CLAUDE.md 병렬 규칙.
    statusbar: state::StatusbarState,
    /// Last `refresh_pane_cwds` sweep — rate-limits the lsof calls.
    pane_cwd_check: Option<Instant>,
    /// Preview panes (image/markdown) already materialized from the daemon's
    /// StateView, keyed by pane id → the path we built it from. Guards against
    /// re-decoding the image on every (frequent) State broadcast; a changed
    /// path rebuilds. See the `UserEvent::DaemonState` handler.
    /// True while Alt/Option is held — draws each pane's %N big + centered
    /// (tmux `display-panes`), so the user can read a pane id for `tell %N` /
    /// focus without it cluttering the header normally. Toggled in
    /// `ModifiersChanged`.
    show_pane_numbers: bool,
    /// Sidebar file-tree column + its in-flight interactions, grouped into a
    /// sub-struct (state.rs) so file-tree work touches one file — CLAUDE.md 병렬.
    file_tree: state::FileTreeState,
    /// `git check-ignore` runs off-GUI-thread: spawning git from the unsigned
    /// kasaterm.exe triggers a Defender full-scan (~5s/call on Windows) that
    /// would freeze the toggle if run inline. The worker reads a (root, paths)
    /// request, fills the file-tree ignored set (`file_tree.ignored`), and wakes
    /// the loop so the next rebuild dims gitignored rows; until then, un-dimmed.
    git_ignore_req: std::sync::Arc<std::sync::Mutex<Option<(std::path::PathBuf, Vec<String>)>>>,
    git_ignore_started: bool,
    /// Settings screen (Warp-style full-view, reached from the sidebar). When
    /// open it replaces the pane grid; the sidebar/titlebar stay live.
    settings_open: bool,
    settings_cat: SettingsCat,
    /// In-memory mirror of settings.json, edited live and written on each
    /// change so the next launch (and `resolve_*`) pick it up.
    set_cwd_mode: String,
    /// 파일트리에서 파일을 열 때의 기본 동작, `app` 모드가 쓸 앱 이름,
    /// `terminal` 모드가 실행할 명령줄.
    set_file_open_mode: String,
    set_file_open_app: String,
    set_file_open_cmd: String,
    set_file_tree_default: bool,
    set_footer_default: bool,
    /// Editor autosave quiet period. `None` = off, which is the default and
    /// what VS Code ships — writing a user's file without being asked is a
    /// surprise, so it stays opt-in. Cmd+S keeps its `✓ 저장됨` toast either way.
    set_autosave: Option<std::time::Duration>,
    set_shell: String,
    /// Per-pane claude wrapper injection (the shim reads these): persona on/off,
    /// model/effort overrides, and free-form extra args. Invariants
    /// (session-id/settings/task-list) stay hardcoded and are never exposed here.
    set_claude_persona: bool,
    set_shim_inject: bool,
    set_claude_model: String,
    set_claude_effort: String,
    set_claude_extra: String,
    /// 전환 가능한 Claude 로그인 목록과 활성 계정 id(`""` = 기본 로그인). 설정
    /// 스냅샷이 프레임마다 만들어지므로 파일을 그때 읽지 않고 여기 들고 있는다.
    set_claude_accounts: Vec<socket::ClaudeAccount>,
    set_claude_account: String,
    /// 한도가 차면 다음 계정으로 알아서 넘어간다(기본 off) + 그 임계 사용률(%).
    set_account_autoswitch: bool,
    set_account_autoswitch_pct: f32,
    /// Which form text field has focus (cwd custom path / shell), if any.
    settings_input: Option<SettingsInput>,
    /// Caret (char index) for the focused single-line settings field
    /// (cwd path / shell / claude extra). Kept apart from `students_caret`
    /// (the persona multiline caret): only one is ever focused at a time, but
    /// sharing one store made the caret jump when focus crossed between a
    /// single-line field and the persona box.
    settings_caret: usize,
    /// Clickable targets collected during the settings paint, for hit-testing.
    settings_rects: Vec<(SettingsAction, (f32, f32, f32, f32))>,
    /// statusline 학생 프사 클릭 hit-test: (학생 이름, rect). 렌더가 매 프레임
    /// 재구축 → 프사 클릭 시 학생 설정 별도창(Students 카테고리 + 그 학생 선택)을
    /// 연다. 프사 hover 확대와 같은 slot(profile_face_hits)에서 나온다.
    face_hit_rects: Vec<(String, (f32, f32, f32, f32))>,
    /// 커서가 statusline 프사 위인지 — 진입/이탈 시에만 재페인트해 hover 확대
    /// 팝업이 뜨고 사라지게 한다(이벤트 기반 루프라 이동만으론 재렌더 안 됨).
    face_hover: bool,
    /// Students 카테고리 인라인 편집: 선택된 캐릭터(이름) + persona 편집 버퍼 +
    /// 캐럿(문자 인덱스). 선택 시 raw_persona 를 버퍼로 로드하고, blur/선택변경 시
    /// characters.json 에 flush 한다.
    students_selected: Option<String>,
    students_persona: String,
    students_caret: usize,
    /// Debounced window-frame save deadline: set 1s after every Moved/Resized,
    /// written by about_to_wait. Exit-only persistence lost the frame on a
    /// crash/force-quit.
    window_frame_save_due: Option<std::time::Instant>,
    /// Raw-editor mouse selection in progress: the pane id whose editor owns
    /// the drag (armed on body press, released on mouse-up).
    md_select_drag: Option<String>,
    /// Settings form wheel-scroll offset (logical px). Reset on open and on
    /// category switch.
    settings_scroll: f32,
    /// Max scroll for the current category — content height minus the visible
    /// form area, computed by the render pass (paint_settings returns the
    /// content height). The wheel handler clamps against this.
    settings_scroll_max: f32,
    /// Sidebar "Settings" entry rect (bottom-anchored), for hit-testing.
    settings_btn_rect: (f32, f32, f32, f32),
    /// Cursor-blink phase captured at the last successful render.
    /// Used by `render_frame`'s early-return: a blink toggle counts
    /// as "something changed" and forces the GPU pass even when
    /// every pane is clean.
    last_blink_on: bool,
    /// Chrome-level dirty flag. Set on any non-PTY state change that
    /// needs the next frame to repaint (selection, preedit, focus
    /// shifts, resize, mouse hover, etc.). PTY changes set the
    /// per-pane `PaneState::dirty` instead.
    chrome_dirty: bool,
    cursor_px: (f32, f32),
    /// Headless hover testing: KASATERM_AUTOHOVER="x,y" (logical px) pins
    /// the cursor so a screenshot can capture a hover state without a real
    /// mouse (cliclick needs Accessibility perms). Real CursorMoved events
    /// are ignored while set.
    autohover: Option<(f32, f32)>,
    modifiers: ModifiersState,
    wheel_accum_y: f32,
    last_wheel_emit: Instant,
    /// Last keystroke / IME / mouse press timestamp. Resets the blink
    /// phase so the cursor stays solid while the user is actively
    /// interacting and only fades back to blinking on idle.
    last_input_at: Instant,
    /// Currently-applied logical font size. Mutated by the host_mod+=
    /// / Ctrl+- shortcuts (see `change_font_size`). Starts at the
    /// `FONT_SIZE` constant so first-frame layout matches the original
    /// behavior before any zoom.
    font_size: f32,
    /// Whole-UI zoom multiplier folded into the effective render scale
    /// (`effective_scale = DPI scale × ui_zoom`). 1.0 = native. Ctrl +/-/0
    /// drive this so chrome, sidebar, and every pane scale together.
    ui_zoom: f32,
    /// Per-pane font multiplier, keyed by the pane's pty id (the BSP leaf id
    /// the renderer + resize path use). Absent = 1.0. Keyed here rather than
    /// on PaneState because split leaves don't all get a ws.panes entry.
    pane_font_scales: std::collections::HashMap<String, f32>,
    /// Wakes the event loop from background threads (PTY snapshots,
    /// socket commands) so a parked WaitUntil repaints immediately.
    proxy: EventLoopProxy<UserEvent>,
    /// Session panel: a second OS window/webview listing the tmux-style
    /// sessions. Same lifetime discipline as the git panel — webview must
    /// outlive its window, so both are owned here.
    session_panel_window: Option<Arc<Window>>,
    session_panel_webview: Option<wry::WebView>,
    /// Board panel: a second OS window/webview showing each pane's live
    /// activity (collab board) with a per-pane message box. Same lifetime
    /// discipline as the git/session panels — webview must outlive its window.
    board_panel_window: Option<Arc<Window>>,
    board_panel_webview: Option<wry::WebView>,
    /// 아로나 전면 UI — 별도 OS 창 + arona-ui dist 를 MCP HTTP 로 로드.
    arona_panel_window: Option<Arc<Window>>,
    arona_panel_webview: Option<wry::WebView>,
    /// Open preview windows (image viewer / markdown editor), each a
    /// separate OS window + webview spawned by `imgopen` / `mdopen` (or the
    /// MCP `/open-image` `/open-markdown` endpoints). A Vec, not a single
    /// slot, so the user can have several open at once; each entry is
    /// dropped when its own window is closed. Webview must outlive its
    /// window, so both are owned together.
    preview_windows: Vec<(Arc<Window>, wry::WebView)>,
    /// Per-frame hit rects for the terminal-pane right-side action cluster
    /// (new-terminal / web / split-v / split-h). Re-built each chrome
    /// paint alongside `image_btn_rects`; the mouse handler matches a
    /// click against it before falling through to tab/plus/cell tests.
    pane_action_hits: Vec<(String, ActionKind, (f32, f32, f32, f32))>,
    /// When the launch build banner began animating. Drives the
    /// hold-then-fade alpha and keeps the frame loop awake (WaitUntil)
    /// only while the banner is still visible.
    version_anim_start: Instant,
    /// macOS menu bar (muda). Held here because the menu must outlive the
    /// app; `git_menu_item` is matched against incoming MenuEvent ids to
    /// toggle the git panel from the menu.
    menu: Option<muda::Menu>,
    git_menu_item: Option<muda::MenuItem>,
    arona_menu_item: Option<muda::MenuItem>,
    /// "세션 패널" menu item id, matched against MenuEvents to toggle the
    /// session panel.
    session_menu_item: Option<muda::MenuItem>,
    /// "board 패널" menu item id, matched against MenuEvents to toggle the
    /// collab board panel.
    board_menu_item: Option<muda::MenuItem>,
    /// 편집 메뉴 복사/붙여넣기 — 네이티브 PredefinedMenuItem 은 Cmd+C/Cmd+V
    /// keyDown 을 가로채 터미널까지 안 내려보낸다(먹통). 커스텀 항목으로 만들어
    /// MenuEvent id 로 매칭, webview 우선 위임 후 폴백으로 직접 클립보드 처리.
    copy_menu_item: Option<muda::MenuItem>,
    paste_menu_item: Option<muda::MenuItem>,
    /// "업데이트 확인" 메뉴 — MenuEvent id 로 매칭해 Sparkle checkForUpdates 를 부른다.
    update_menu_item: Option<muda::MenuItem>,
    /// "kasaterm 종료"(⌘Q) 메뉴 — MenuEvent id 로 매칭해 종료 확인 NSAlert 를 띄운다.
    quit_menu_item: Option<muda::MenuItem>,
    /// Sparkle SPUStandardUpdaterController — 보관해야 백그라운드 자동 체크가 유지된다(드롭=정지).
    #[cfg(target_os = "macos")]
    sparkle_updater: Option<objc2::rc::Retained<objc2::runtime::AnyObject>>,
    /// History store for inline autosuggestion. See autosuggest.rs.
    autosuggest: autosuggest::History,
    /// What the user has typed at the current shell prompt since the last
    /// Enter / line-reset. The source of truth for the suggestion prefix;
    /// validated each frame against the grid (see `update_suggestion`) so
    /// shell-side edits we can't see (Tab-complete, paste) just suppress
    /// the suggestion instead of showing a wrong one.
    input_buf: String,
    /// The remainder currently drawn as ghost text (None = nothing shown).
    /// Recomputed at render time in `update_suggestion`; read by the key
    /// handler so → / Ctrl-E can accept exactly what's on screen.
    current_suggestion: Option<String>,
    /// Whether the left window-tab sidebar is shown. Toggled by the
    /// title-bar button (next to the traffic lights). When false the cell
    /// grid reflows to full width — every origin/layout calc reads
    /// `effective_sidebar_w()` instead of the `SIDEBAR_W` const directly,
    /// so flipping this is all it takes to collapse the strip.
    sidebar_visible: bool,
    /// Window tabs live in the title strip (Windows Terminal-style horizontal
    /// tabs) instead of the left sidebar. `sidebar_layout` swaps its rect math
    /// and `tab_strip_w()` pins the side strip to 0, so render + click routing
    /// follow automatically. Persisted as settings.json `tab_position`.
    tabs_on_top: bool,
    /// macOS `.md` 더블클릭이 cold-launch(앱 꺼진 채)로 들어오면 odoc 이벤트가
    /// `resumed()`(window·pty_layout 생성) 전에 도착할 수 있다. 그때 경로를 여기
    /// 쌓아두고 start_pty 직후 flush 한다(빈손이면 무비용). 앱 켜진 채 더블클릭은
    /// 디퍼 없이 바로 `open_markdown_window`.
    pending_open_md: Vec<std::path::PathBuf>,
    /// 편집기/파일뷰를 떼어낸 별도 OS 창들(각자 자체 wgpu GpuRenderer). 메인 창과
    /// 독립적으로 렌더/입력 라우팅되며 window id 로 handler 가 분기한다(auxwin.rs).
    aux_windows: Vec<auxwin::AuxWindow>,
}

impl App {
    fn new(proxy: EventLoopProxy<UserEvent>) -> Self {
        Self {
            window: None,
            gpu: None,
            lsp: None,
            hover: None,
            lsp_goto: None,
            tmux: None,
            pty: HashMap::new(),
            pty_layout: None,
            next_pane_id: 1, // %0 is the initial pane created in start_pty
            pending_restores: Vec::new(),
            pending_unjiggle: Vec::new(),
            autoquit_at: None,
            pending_capture: Vec::new(),
            pending_autogit: None,
            autosplit_plan: Vec::new(),
            autosplit_at: None,
            autoopen_path: None,
            autoopen_at: None,
            autoconfirm_at: None,
            autodrag_plan: None,
            autodrag_at: None,
            autopanemove_dst: None,
            autopanemove_at: None,
            force_drag_leaf: None,
            force_drag_at: None,
            autowindow_left: 0,
            autowindow_at: None,
            autotoggle_sidebar_at: None,
            autoarona_at: None,
            autotoggle_left: 0,
            autotabs_n: 0,
            autotabs_at: None,
            dead_panes: Arc::new(Mutex::new(Vec::new())),
            ws: Arc::new(Mutex::new(Workspace::default())),
            sessions: vec![None],
            active_session: 0,
            windows: vec![None],
            active_window: 0,
            cell: CellGeom::default(),
            preedit: String::new(),
            in_preedit: false,
            commit_overlay: None,
            ime_active: false,
            hangul: kasa_ime::Composer::new(),
            ime_focus: None,
            pane_header_rects: Vec::new(),
            pane_handle_rects: Vec::new(),
            pane_top_zones: Vec::new(),
            handle_hovered: false,
            handle_zone_hovered: false,
            // testkit: KASATERM_FORCE_HANDLE_MENU=%N 으로 그 pane ⋮ 메뉴를
            // 부팅 시 강제로 열어 자동캡처로 메뉴 레이아웃을 검증한다. 미설정이면 None.
            handle_menu: std::env::var("KASATERM_FORCE_HANDLE_MENU").ok(),
            handle_menu_hits: Vec::new(),
            // KASATERM_FORCE_HANDLE_MENU 와 같은 헤드리스 검증용 — 클릭 합성 없이
            // 드롭다운이 열린 프레임을 캡처한다.
            account_menu: std::env::var_os("KASATERM_FORCE_ACCOUNT_MENU").is_some(),
            account_chip_rect: None,
            account_menu_hits: Vec::new(),
            md_content_h: HashMap::new(),
            md_block_ys: HashMap::new(),
            md_scroll_anchor: HashMap::new(),
            md_word_rects: HashMap::new(),
            md_render_sel: None,
            md_scroll_anim: HashMap::new(),
            md_click_streak: None,
            md_find_rects: Vec::new(),
            md_body_rects: HashMap::new(),
            pane_tab_rects: Vec::new(),
            pane_tab_close_rects: Vec::new(),
            pane_tab_popout_rects: Vec::new(),
            pane_plus_rects: Vec::new(),
            docked: Vec::new(),
            dock_chip_rects: Vec::new(),
            dock_chip_close_rects: Vec::new(),
            copy_toast_at: None,
            pane_busy_check: None,
            pane_last_busy: HashMap::new(),
            pane_bg_mtime: HashMap::new(),
            window_tab_rects: Vec::new(),
            window_tab_close_rects: Vec::new(),
            win_tab_first: 0,
            win_tab_vis: usize::MAX,
            win_tab_wheel_accum: 0.0,
            new_window_btn_rect: None,
            shell_menu_open: false,
            shell_menu_hits: Vec::new(),
            pending_shell: None,
            window_labels: Vec::new(),
            window_labels_at: None,
            window_name_override: HashMap::new(),
            selection: None,
            drag_anchor: None,
            link_armed: None,
            resize_drag: None,
            last_divider_pos: None,
            last_divider_pty_resize: None,
            header_drag: None,
            tab_drag: None,
            drag_orig_layout: None,
            drag_live_applied: None,
            image_pan_drag: None,
            text_cursor_shown: false,
            confirm_close: None,
            confirm_btn_rects: Vec::new(),
            restore_prompt: None,
            restore_btn_rects: Vec::new(),
            session_saved_at: std::time::Instant::now(),
            session_touched: false,
            session_saved_hash: None,
            pane_tab_hover: None,
            image_btn_rects: Vec::new(),
            sidebar_w_logical: SIDEBAR_W,
            sidebar_resize: None,
            last_resized_cells: (0, 0),
            pending_resize: None,
            mouse_forward_pane: None,
            last_left_click: None,
            last_tree_click: None,
            zoomed_pane: None,
            saved_window_frame: None,
            titlebar_drag_pending: None,
            last_window_title: None,
            claude_busy_until: None,
            last_claude_status: None,
            pane_activity: HashMap::new(),
            window_focused: true,
            notify_flash: HashMap::new(),
            turn_done_panes: std::collections::HashSet::new(),
            window_alert: std::collections::HashSet::new(),
            unread_panes: std::collections::HashSet::new(),
            dock_badge_n: 0,
            collab: Default::default(),
            pane_prompt_wait: HashMap::new(),
            last_window_title_check: None,
            pane_cwd_cache: HashMap::new(),
            pane_view_cwd: HashMap::new(),
            pending_room: None,
            next_room_seq: 1,
            pending_character: None,
            pane_session_id: HashMap::new(),
            pane_claude_sid: HashMap::new(),
            socket_backend: None,
            bg_agents: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
            claude_usage: std::sync::Arc::new(std::sync::Mutex::new(None)),
            pane_tty_cache: HashMap::new(),
            window_git: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
            git_poll_cwds: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            pane_status_pub: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
            git: state::GitState {
                col_visible: std::env::var("KASASPACE_GIT_PANEL")
                    .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                    .unwrap_or(false),
                col_w_logical: GIT_COL_W,
                commit_modal_include_unstaged: true,
                ..Default::default()
            },
            info: state::InfoState {
                // 헤드리스 검증용 — 시작 탭을 Info 로. 탭 전환은 클릭이라
                // PTY-only autosend 로는 재현할 수 없다(KASATERM_TEST_FILETREE
                // 가 사이드바를 강제로 여는 것과 같은 이유).
                tab: if std::env::var("KASATERM_TEST_INFO").is_ok() {
                    state::SideTab::Info
                } else {
                    state::SideTab::Git
                },
                ..Default::default()
            },
            statusbar: Default::default(),
            pane_cwd_check: None,
            show_pane_numbers: false,
            file_tree: state::FileTreeState {
                // Headless test override (KASATERM_TEST_FILETREE) forces the
                // sidebar open at launch so quick-files/tree captures render
                // without needing a chrome click the PTY-only autosend can't do.
                visible: socket::read_file_tree_default()
                    || std::env::var("KASATERM_TEST_FILETREE").is_ok(),
                w_logical: FILE_TREE_W,
                ..Default::default()
            },
            git_ignore_req: std::sync::Arc::new(std::sync::Mutex::new(None)),
            git_ignore_started: false,
            // 설정은 별도창(auxwin)이라 부팅 시엔 항상 닫힘 — settings_open 은
            // 그 창의 존재와 동기화되는 플래그. 헤드리스 초기 열림은 이제
            // KASATERM_AUTOSETTINGS(testkit)가 event_loop 위에서 담당한다.
            settings_open: false,
            settings_cat: SettingsCat::General,
            set_cwd_mode: socket::read_default_cwd_mode(),
            set_file_open_mode: socket::read_file_open_mode(),
            set_file_open_app: socket::read_file_open_app(),
            set_file_open_cmd: socket::read_file_open_cmd(),
            set_file_tree_default: socket::read_file_tree_default(),
            set_footer_default: socket::read_footer_default(),
            set_autosave: socket::read_editor_autosave(),
            set_shell: socket::read_default_shell().unwrap_or_default(),
            set_claude_persona: socket::read_claude_persona(),
            set_shim_inject: socket::read_shim_inject(),
            set_claude_model: socket::read_claude_model(),
            set_claude_effort: socket::read_claude_effort(),
            set_claude_extra: socket::read_claude_extra(),
            set_claude_accounts: socket::read_claude_accounts(),
            set_claude_account: socket::read_claude_account(),
            set_account_autoswitch: socket::read_account_autoswitch(),
            set_account_autoswitch_pct: socket::read_account_autoswitch_pct(),
            settings_input: None,
            settings_rects: Vec::new(),
            // KASATERM_TEST_STUDENT=<이름> 이면 그 캐릭터를 선택 상태로 시드해
            // persona 편집기 렌더를 헤드리스로 캡처할 수 있게 한다(테스트 전용).
            students_selected: std::env::var("KASATERM_TEST_STUDENT").ok().filter(|s| !s.is_empty()),
            students_persona: std::env::var("KASATERM_TEST_STUDENT")
                .ok()
                .filter(|s| !s.is_empty())
                .and_then(|n| {
                    kasa_mcp::character::characters_json()
                        .and_then(|c| kasa_mcp::character::raw_persona_for(&c, &n))
                })
                .unwrap_or_default(),
            students_caret: 0,
            settings_caret: 0,
            face_hit_rects: Vec::new(),
            face_hover: false,
            window_frame_save_due: None,
            md_select_drag: None,
            // KASATERM_TEST_SETTINGS_SCROLL: 헤드리스 스크린샷으로 폼 스크롤을
            // 검증하는 시드(휠 주입 불가) — 렌더 패스가 max 로 클램프해 준다.
            settings_scroll: std::env::var("KASATERM_TEST_SETTINGS_SCROLL")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.0),
            settings_scroll_max: 0.0,
            settings_btn_rect: (0.0, 0.0, 0.0, 0.0),
            last_blink_on: false,
            chrome_dirty: true,
            cursor_px: std::env::var("KASATERM_AUTOHOVER")
                .ok()
                .and_then(|s| {
                    let (a, b) = s.split_once(',')?;
                    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
                })
                .unwrap_or((0.0, 0.0)),
            autohover: std::env::var("KASATERM_AUTOHOVER").ok().and_then(|s| {
                let (a, b) = s.split_once(',')?;
                Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
            }),
            modifiers: ModifiersState::empty(),
            wheel_accum_y: 0.0,
            last_wheel_emit: Instant::now() - std::time::Duration::from_secs(1),
            last_input_at: Instant::now(),
            font_size: socket::read_font_size(),
            ui_zoom: 1.0,
            pane_font_scales: std::collections::HashMap::new(),
            proxy,
            session_panel_window: None,
            session_panel_webview: None,
            board_panel_window: None,
            board_panel_webview: None,
            arona_panel_window: None,
            arona_panel_webview: None,
            preview_windows: Vec::new(),
            pane_action_hits: Vec::new(),
            version_anim_start: Instant::now(),
            menu: None,
            git_menu_item: None,
            arona_menu_item: None,
            session_menu_item: None,
            board_menu_item: None,
            copy_menu_item: None,
            paste_menu_item: None,
            update_menu_item: None,
            quit_menu_item: None,
            #[cfg(target_os = "macos")]
            sparkle_updater: None,
            autosuggest: autosuggest::History::new(),
            input_buf: String::new(),
            current_suggestion: None,
            // Default closed — single-pane, no chrome reads as a plain
            // terminal at first launch. User toggles via the title-bar
            // button or the "보기 → 세션 패널" menu item.
            sidebar_visible: false,
            // 기본은 side(사이드바 탭) — read_tab_position 이 "top" 만 top 으로,
            // 그 외/키없음은 side 로 폴백한다.
            tabs_on_top: socket::read_tab_position() == "top",
            pending_open_md: Vec::new(),
            aux_windows: Vec::new(),
        }
    }


















































































































































}

fn main() -> Result<(), Box<dyn Error>> {
    // `open`(1) doesn't forward shell env to the launched .app, but the
    // .app's screen-recording TCC permission only applies when launched
    // via `open` (not when the binary runs directly). So a capture/test
    // config file is how we still drive autocapture/autosplit through an
    // `open`-launched instance. Loaded (and deleted) before anything
    // reads KASATERM_* vars.
    load_capture_config();
    // Apply the persisted theme + accent into the global color slots before any
    // window or pane paints, so the first frame is already in the right palette.
    theme::apply_from_settings();
    // Install pane shims before anything spawns a shell — every PtySession
    // reads KASATERM_TMUX_SHIM_DIR we set here (kasaterm-cli/preview/OSC133).
    // best-effort: failures just log and skip, the rest still works.
    install_pane_shims();
    // 죽은 인스턴스가 남긴 소켓 잔재 청소(재시작·빌드 반복 누적). 살아있는
    // 소켓은 connect 로 가려 건드리지 않으므로 멀티 인스턴스에서도 안전.
    // 다른 인스턴스가 하나도 없으면 collab 캐릭터 마커도 전부 stale — 같이 청소.
    #[cfg(unix)]
    if !sweep_dead_kasaterm_sockets() {
        cleanup_stale_collab_markers();
    }
    // 헤드리스 검증 실행이 거노 화면을 뺏지 않게 한다. 스스로 종료하는 실행
    // (`KASATERM_AUTOQUIT_MS`)은 정의상 테스트라 자동으로 배경에 띄운다 —
    // Accessory 정책이면 Dock/⌘Tab 에도 안 올라오고 활성 앱도 안 바뀌므로,
    // 캡처를 도는 동안 거노가 하던 창에 그대로 머문다. `KASATERM_NO_FOCUS`
    // 로 직접 켜고 끌 수도 있다(0/false 면 강제로 평소처럼 뜬다).
    let mut builder = EventLoop::<UserEvent>::with_user_event();
    #[cfg(target_os = "macos")]
    if background_launch() {
        use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
        builder
            .with_activation_policy(ActivationPolicy::Accessory)
            .with_activate_ignoring_other_apps(false);
    }
    let event_loop = builder.build()?;
    let proxy = event_loop.create_proxy();
    // argv 폴백: `kasaterm file.md` / 커맨드라인. `.md` 인자면 새 워크스페이스
    // 마크다운으로 위임(resumed 전이면 디퍼됐다 start_pty 후 flush). `open`(1)은
    // odoc 로만 오므로 둘이 겹쳐도 open_markdown_window 의 dedup 이 흡수한다.
    for arg in std::env::args().skip(1) {
        let p = std::path::PathBuf::from(&arg);
        let is_md = p
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"))
            .unwrap_or(false);
        if is_md && p.is_file() {
            if let Ok(abs) = std::fs::canonicalize(&p) {
                let _ = proxy.send_event(UserEvent::OpenMarkdownWindow(
                    abs.to_string_lossy().into_owned(),
                ));
            }
        }
    }
    let mut app = App::new(proxy);
    event_loop.run_app(&mut app)?;
    Ok(())
}

/// Load `$TMPDIR/kasaterm-capture.env` (KEY=VALUE lines) into the
/// process environment, then delete it. This is the bridge for capture:
/// `open` strips shell env, so a capture script drops KASATERM_* here
/// and the `open`-launched .app picks them up on startup. One-shot
/// (deleted on read) so a normal launch is never affected, and a real
/// env var still wins — we only fill in keys that aren't already set.
fn load_capture_config() {
    let path = std::env::temp_dir().join("kasaterm-capture.env");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return;
    };
    let _ = std::fs::remove_file(&path);
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        if std::env::var_os(k).is_none() {
            std::env::set_var(k, v);
        }
    }
}

/// pane 자식 셸이 쓸 보조 바이너리/설정을 private dir 에 깔고 그 dir 를
/// `KASATERM_TMUX_SHIM_DIR` 로 넘긴다(pty-backend 가 PATH/ZDOTDIR 에 반영):
/// kasaterm-cli(pane 간 협업)·imgopen/mdopen(preview)·zsh OSC133 prompt-mark
/// (입력줄 감지). teammate-mode tmux 위장은 제거됨 — pane 생성은 오케스트레이터가
/// `kasaterm-cli split` 로 한다. best-effort: 실패해도 본체는 동작한다.
/// 창을 활성화하지 않고 배경에 띄울지. 자동 종료하는 검증 실행이면 기본 on.
pub(crate) fn background_launch() -> bool {
    match std::env::var("KASATERM_NO_FOCUS").as_deref() {
        Ok("0") | Ok("false") | Ok("") => false,
        Ok(_) => true,
        Err(_) => std::env::var_os("KASATERM_AUTOQUIT_MS").is_some(),
    }
}

fn install_pane_shims() {
    // 전역 shim 스위치 OFF → shim dir 자체를 안 만든다. KASATERM_TMUX_SHIM_DIR 이 미설정
    // 이면 pty-backend(state.rs)가 PATH prepend·ZDOTDIR 를 건드리지 않아 자식 셸이 순정
    // 이 된다 — claude wrapper·imgopen·훅·프록시 배선 전무(진짜 독립). 기본 ON(하위호환).
    // install 은 부팅 1회라 이 스위치 변경은 재시작 후 적용된다.
    if !socket::read_shim_inject() {
        eprintln!("[shim] shim_inject=off — 순정 모드, pane shim 미설치");
        return;
    }
    // 렌더러 capability 공표 — statusline.py 가 이걸 보고서만 SGR8(conceal) 세션 id
    // 마커를 내보낸다. 게이트가 없으면 .app 설치 직후(재시작 전) 구버전 렌더러가
    // 마커를 그대로 그려 화면에 `⟦a1b2c3d4⟧` 가 노출된다(conceal 미지원).
    {
        let caps = kasa_socket::home_dir().unwrap_or_default().join(".config/kasaterm/caps.json");
        if let Some(d) = caps.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(&caps, "{\"sgr_conceal\":true}\n");
    }
    let shim_dir = std::env::temp_dir().join(format!("kasaterm-shim-{}", std::process::id()));
    if let Err(e) = std::fs::create_dir_all(&shim_dir) {
        eprintln!("[shim] mkdir {shim_dir:?} failed: {e}");
        return;
    }
    // Cross-pane RPC: stage kasaterm-cli on the child shell's PATH so it is
    // discoverable on the child shell's PATH. A pane can then run
    // `kasaterm-cli send --surface %1 "..."` to drive a sibling pane
    // without needing to know the absolute target/debug path. Failure
    // is non-fatal — the shim already works without it.
    if let Some(cmux_src) = locate_cmux_compat_binary() {
        let cmux_name = if cfg!(windows) {
            "kasaterm-cli.exe"
        } else {
            "kasaterm-cli"
        };
        let cmux_target = shim_dir.join(cmux_name);
        let _ = std::fs::remove_file(&cmux_target);
        if let Err(e) = stage_shim(&cmux_src, &cmux_target) {
            eprintln!("[shim] stage kasaterm-cli {cmux_src:?} -> {cmux_target:?} failed: {e}");
        }
    }
    // Drop `imgopen` / `mdopen` on the pane PATH so the user (or Claude) can
    // pop an image viewer / markdown editor into its own window with zero
    // install — each just curls the host's MCP open-preview endpoint.
    install_preview_shims(&shim_dir);
    // Stage a `claude` wrapper that injects the collab hooks session-scoped
    // (`--settings`) so ~/.claude/settings.json is never modified.
    install_claude_hook_shim(&shim_dir);
    // 학생 이름 자체를 명령으로(`시로코`/`shiroko`) — 이 pane 을 그 학생으로
    // 재배정하고 claude 를 띄운다. characters.json 기준 부팅 1회 생성.
    install_student_shims(&shim_dir);
    // Force our shim dir to the FRONT of PATH even after the user's rc
    // files run. A login+interactive zsh sources brew's zprofile, which
    // prepends /opt/homebrew/bin (the real tmux) ahead of the PATH we
    // hand the shell — so `tmux` resolves to brew's, not ours, and
    // claude teammate's `split-window` misses the shim. We point ZDOTDIR
    // (set in pty-backend) at this dir and drop thin rc files that source
    // the real ones first, then re-prepend our dir LAST in .zshrc — so
    // it wins over brew. Non-zsh shells ignore ZDOTDIR and rely on the
    // plain PATH prepend pty-backend still does.
    let write_rc = |name: &str, body: String| {
        if let Err(e) = std::fs::write(shim_dir.join(name), body) {
            eprintln!("[shim] write rc {name} failed: {e}");
        }
    };
    write_rc(
        ".zshenv",
        "[ -f \"${HOME}/.zshenv\" ] && source \"${HOME}/.zshenv\"\n".to_string(),
    );
    write_rc(
        ".zprofile",
        "[ -f \"${HOME}/.zprofile\" ] && source \"${HOME}/.zprofile\"\n".to_string(),
    );
    // After sourcing the user's real .zshrc we (1) re-prepend our shim
    // dir to PATH so it wins over brew, and (2) install the full OSC 133
    // prompt-mark protocol (A prompt-start / B input-start / C output-start
    // / D;exit command-end). A/B wrap PS1 with zero-width (`%{..%}`) marks;
    // the `B` mark is what pty-backend sniffs to locate the editable command
    // line. C/D delimit a command block (Warp-style): preexec emits C right
    // before a command runs and marks `_kasaterm_ran`; the next precmd emits
    // D with that command's exit code and clears the mark. Gating D on the
    // preexec mark keeps a bare Enter (no preexec) from leaking a C-less D,
    // so every D pairs with a real C. `$?` is captured on precmd's FIRST line
    // — any command after it (even `[[ ]]`) clobbers it. The PS1 guard skips
    // re-wrapping a static PS1 while still re-wrapping themes that rebuild it
    // each precmd (powerlevel10k / starship). zsh-only — other shells ignore
    // ZDOTDIR and just get the PATH prepend.
    write_rc(
        ".zshrc",
        format!(
            "[ -f \"${{HOME}}/.zshrc\" ] && source \"${{HOME}}/.zshrc\"\n\
             export PATH=\"{}:${{PATH}}\"\n\
             _kasaterm_osc133(){{ local __ec=$?; \
             [[ -n $_kasaterm_ran ]] && {{ printf $'\\e]133;D;%d\\a' \"$__ec\"; _kasaterm_ran=; }}; \
             [[ \"$PS1\" == *$'\\e]133;B'* ]] && return; \
             PS1=$'%{{\\e]133;A\\a%}}'\"$PS1\"$'%{{\\e]133;B\\a%}}'; }}\n\
             _kasaterm_preexec133(){{ printf $'\\e]133;C\\a'; _kasaterm_ran=1; }}\n\
             autoload -Uz add-zsh-hook 2>/dev/null && {{ \
             add-zsh-hook precmd _kasaterm_osc133 2>/dev/null; \
             add-zsh-hook preexec _kasaterm_preexec133 2>/dev/null; }}\n",
            shim_dir.display()
        ),
    );
    write_rc(
        ".zlogin",
        "[ -f \"${HOME}/.zlogin\" ] && source \"${HOME}/.zlogin\"\n".to_string(),
    );
    std::env::set_var("KASATERM_TMUX_SHIM_DIR", &shim_dir);
    eprintln!("[shim] pane shim dir={shim_dir:?}");
}

/// Write `imgopen` and `mdopen` into the shim dir (on the pane PATH). Each
/// resolves its argument to an absolute path and curls the host's MCP
/// open-preview endpoint, which spawns a separate wry window. No dependency
/// beyond `curl` (ships on macOS/Linux); the port comes from
/// KASASPACE_MCP_PORT (default 8765), inherited from the host process.
fn install_preview_shims(shim_dir: &std::path::Path) {
    // Windows shells can't run /bin/sh scripts and the pane PATH there isn't
    // a POSIX shell; skip rather than drop broken files.
    if cfg!(windows) {
        return;
    }
    // `--get --data-urlencode` lets curl build the query string with the
    // path properly percent-encoded (spaces, unicode, etc.) — no hand-rolled
    // URL escaping in sh.
    let mk = |cmd: &str, endpoint: &str| -> String {
        format!(
            "#!/bin/sh\n\
# kasaterm {cmd} — open a file in a separate preview window.\n\
if [ \"$#\" -lt 1 ]; then echo \"usage: {cmd} FILE\" >&2; exit 1; fi\n\
f=$1\n\
if command -v realpath >/dev/null 2>&1; then abs=$(realpath \"$f\"); \
else case \"$f\" in /*) abs=\"$f\";; *) abs=\"$PWD/$f\";; esac; fi\n\
port=${{KASASPACE_MCP_PORT:-8765}}\n\
curl -s --get --data-urlencode \"path=$abs\" \
--data-urlencode \"pane=${{KASATERM_PANE_ID:-}}\" \
\"http://127.0.0.1:$port/{endpoint}\" >/dev/null \
|| {{ echo \"{cmd}: failed to reach kasaterm\" >&2; exit 1; }}\n"
        )
    };
    for (name, endpoint) in [("imgopen", "open-image"), ("mdopen", "open-markdown")] {
        let path = shim_dir.join(name);
        if let Err(e) = std::fs::write(&path, mk(name, endpoint)) {
            eprintln!("[shim] write {name} failed: {e}");
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(e) =
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            {
                eprintln!("[shim] chmod {name} failed: {e}");
            }
        }
    }
}

/// Locate the canonical collab-hooks directory the generated hook settings
/// point at. The scripts resolve their siblings via `dirname $0`, so pointing
/// at any one complete copy works.
fn locate_collab_hooks_dir() -> Option<std::path::PathBuf> {
    resolve_collab_hooks_dir(
        std::env::current_exe().ok().as_deref(),
        std::env::var("KASATERM_COLLAB_HOOKS_DIR").ok().as_deref(),
    )
}

/// Pure resolution (split out so the priority is unit-testable). Priority:
/// 1. the **.app bundle's own Resources** — a release binary must run the hooks
///    it shipped with, so this WINS over the env override. Otherwise a leaked
///    `KASATERM_COLLAB_HOOKS_DIR` (e.g. inherited from a dev shell) would point
///    a release `.app` at version-skewed repo hooks (the bug this guards).
///    Windows MSI 는 exe 옆 `bin\collab-hooks\` — arona-ui 번들과 같은 자리,
///    같은 이유로 env 보다 우선.
/// 2. `KASATERM_COLLAB_HOOKS_DIR` — dev convenience for non-bundle `cargo run`,
///    where no bundle Resources sits next to `target/{debug,release}/kasaterm`.
/// 3. the repo source next to this crate (`CARGO_MANIFEST_DIR`) — plain dev run.
fn resolve_collab_hooks_dir(
    current_exe: Option<&std::path::Path>,
    env_dir: Option<&str>,
) -> Option<std::path::PathBuf> {
    if let Some(exe) = current_exe {
        // <bundle>/Contents/MacOS/kasaterm → <bundle>/Contents/Resources/collab-hooks
        if let Some(res) = exe
            .parent()
            .and_then(|m| m.parent())
            .map(|c| c.join("Resources/collab-hooks"))
        {
            if res.is_dir() {
                return Some(res);
            }
        }
        // Windows MSI: <bin>\kasaterm.exe → <bin>\collab-hooks\
        if let Some(adj) = exe.parent().map(|d| d.join("collab-hooks")) {
            if adj.is_dir() {
                return Some(adj);
            }
        }
    }
    if let Some(p) = env_dir {
        let p = std::path::PathBuf::from(p);
        if p.is_dir() {
            return Some(p);
        }
    }
    let dev = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("collab-hooks");
    if dev.is_dir() {
        return Some(dev);
    }
    None
}

/// 캡처 프록시로 claude API 라우팅 — pane 별 깨끗한 대화 캡처(ccglass 방식). claude 가
/// 이 base 로 `/v1/messages` 를 보내면 kasa-mcp 프록시가 본문 messages[] 를 캡처(peek·
/// jsonl 없이 구조화 대화)하고 api.anthropic.com 으로 투명 포워드한다. MCP 서버 포트
/// (KASASPACE_MCP_PORT, pane spawn 전에 동기 설정)를 쓴다. 포트 미설정이면 빈 env →
/// claude 가 api.anthropic.com 직행(안전 폴백, 프록시 의존 안 함).
pub(crate) fn proxy_env(pane_id: &str) -> Vec<(String, String)> {
    match std::env::var("KASASPACE_MCP_PORT") {
        Ok(port) if !port.is_empty() => vec![(
            "ANTHROPIC_BASE_URL".to_string(),
            format!(
                "http://127.0.0.1:{port}/p/{}",
                pane_id.trim_start_matches('%')
            ),
        )],
        _ => Vec::new(),
    }
}

/// The one shim line that switches Claude accounts, or `""` for the default
/// login.
///
/// `CLAUDE_SECURESTORAGE_CONFIG_DIR` moves *only* the credential store — Claude
/// Code hashes it into its Keychain item name — while `CLAUDE_CONFIG_DIR` stays
/// unset so `~/.claude` keeps holding transcripts, agents, teams and MCP config.
/// Switching the config dir instead would fracture all of that, since kasaterm
/// hardcodes `~/.claude` in the statusline, the board reader and the shim's own
/// `--continue` inference.
///
/// No account selected emits **nothing**: the default login is the absence of
/// the override, so an untouched install behaves exactly as before. An inherited
/// value wins, which is what lets the add-account flow log in to a brand new
/// store by exporting it explicitly.
///
/// The guard tests `${VAR+x}`, not `$VAR`, because **empty is not unset here**.
/// Claude Code reads a defined-but-empty value as "use the unsuffixed store" and
/// deliberately forwards it to child processes (its env allowlist special-cases
/// this one name so the empty string survives), so a claude that was told to
/// stay on the default login must keep that instruction. `[ -z "$VAR" ]` cannot
/// tell the two apart and silently re-points such a child at our account —
/// verified against 2.1.220: with the value set to `""`, `claude auth status`
/// reports the default login as signed in.
fn claude_account_export_line(dir: Option<&std::path::Path>) -> String {
    let Some(dir) = dir else { return String::new() };
    let q = dir.display().to_string().replace('\'', "'\\''");
    format!(
        "[ -z \"${{CLAUDE_SECURESTORAGE_CONFIG_DIR+x}}\" ] && \
         export CLAUDE_SECURESTORAGE_CONFIG_DIR='{q}'\n"
    )
}

/// Stage a `claude` wrapper + a session-scoped hook settings file on the pane
/// PATH (munder-difflin pattern). Collab hooks ride in via `claude --settings`
/// instead of edits to ~/.claude/settings.json, so claude outside a kasaterm
/// pane runs exactly as the user configured it and install-hooks.sh is no
/// longer needed.
pub(crate) fn install_claude_hook_shim(shim_dir: &std::path::Path) {
    let Some(hooks_dir) = locate_collab_hooks_dir() else {
        eprintln!("[shim] collab-hooks dir not found — claude hook shim skipped");
        return;
    };
    // Windows pane 셸은 Git bash(sh 있음) — wrapper 는 그대로 쓴다. sh 더블쿼트 안
    // 백슬래시는 케이스별로 씹히므로 경로는 슬래시로 통일(Git bash 는 C:/ 혼용 허용).
    let hd = if cfg!(windows) {
        hooks_dir.display().to_string().replace('\\', "/")
    } else {
        hooks_dir.display().to_string()
    };
    // Windows 는 claude 가 훅/statusLine 커맨드를 cmd 로 돌리므로 .sh 직접 exec 이
    // 안 된다 — `sh "<경로>"` 로 명시 실행(pane env PATH 에 Git usr/bin 의 sh.exe 가
    // 있다). unix 는 종전대로 직접 exec.
    let cmd = |script: &str, timeout: u64| {
        let run = if cfg!(windows) {
            format!("sh \"{hd}/{script}\"")
        } else {
            format!("\"{hd}/{script}\"")
        };
        serde_json::json!({ "type": "command", "command": run, "timeout": timeout })
    };
    // statusline: unix 는 검증된 py(uv/python3 존재), Windows 는 python3 부재라
    // kasaterm-cli 서브커맨드(같은 출력, 골든 diff 검증)를 쓴다 — shim dir 에
    // 스테이징된 exe 가 pane PATH 로 잡힌다.
    let statusline_cmd = if cfg!(windows) {
        "kasaterm-cli statusline".to_string()
    } else {
        format!("\"{hd}/statusline.py\"")
    };
    // Mirrors what install-hooks.sh used to register globally — same matcher
    // and timeouts, so in-pane behavior is unchanged.
    let mut settings = serde_json::json!({
        "hooks": {
            // 세션 시작/재개 즉시 bind → 첫 프롬프트 전에도 board 에 뜬다. SessionStart 는
            // startup·resume·clear 에 모두 발화하므로 relaunch 후 claude --resume 재바인딩도
            // 커버. transcript 자체는 discover_transcript(cwd→projects, --session-id)로
            // hook-free 라 이 bind 는 roster(복구)·즉시성 보조일 뿐.
            "SessionStart": [{ "hooks": [cmd("kasaterm-bind-transcript.sh", 5000)] }],
            // UserPromptSubmit(board-context.py) 제거 — 프롬프트마다 persona+board+inbox 를
            // additionalContext 로 주입해 소넷 워커 컨텍스트가 누적·과대했다(거노 06-14).
            // persona 는 스폰 시 `--append-system-prompt`로 1회(캐시돼 per-turn 0) 대체.
            // board/inbox 자동인지는 폐기 — 조율은 GUI(SCHALE OS) 와 명시적 kasacollab 으로.
            // 같은 방 다른 pane 이 같은 파일을 작업 중이면 Edit 직전에 막는다
            // (transcript 직접 비교, 데몬 무관). 모든 pane 공통 안전망.
            "PreToolUse": [{ "matcher": "Edit|Write|MultiEdit", "hooks": [cmd("kasaterm-conflict-guard.py", 5000)] }],
            "PostToolUse": [
                { "matcher": "SendUserFile", "hooks": [cmd("auto-imgopen.sh", 10)] },
                { "hooks": [cmd("kasaterm-steer-hook.sh", 5000)] }
            ],
            "Stop": [{ "hooks": [cmd("kasaterm-stop-drain.sh", 5000)] }],
            "Notification": [{ "hooks": [cmd("kasaterm-notify-attention.sh", 5000)] }],
        },
        // statusLine 도 세션 스코프 --settings 로 주입 — 배정 학생 프사(U+FFFC)·model·git·
        // ctx%·effort + 내부 cd 보고(report-cwd). pane 안에서만 우리 것, 밖 claude 는
        // 사용자 ~/.claude/settings.json statusLine 그대로(--settings 는 pane PATH 한정).
        "statusLine": { "type": "command", "command": statusline_cmd, "padding": 0 },
    });
    if cfg!(windows) {
        // conflict-guard 는 python3 의존 — 기본 Windows 엔 python3 가 없어 훅이 매
        // Edit 마다 실패 노이즈를 낸다. python3 가 PATH 에 있을 때만 유지.
        let has_py3 = proc::command("python3")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !has_py3 {
            settings["hooks"].as_object_mut().unwrap().remove("PreToolUse");
        }
    }
    let settings_path = shim_dir.join("claude-hooks-settings.json");
    {
        match serde_json::to_string_pretty(&settings) {
            Ok(s) => {
                if let Err(e) = std::fs::write(&settings_path, s) {
                    eprintln!("[shim] write claude-hooks-settings.json failed: {e}");
                    return;
                }
            }
            Err(e) => {
                eprintln!("[shim] serialize claude hook settings failed: {e}");
                return;
            }
        }
    }
    // 설정창 "클로드" 탭 노브를 shim 에 인라인 — 파싱은 여기 Rust 가 하고 shim 은 순수 sh 로
    // 남긴다(hot path 가벼움). 아래 라인들은 전부 PERSONA_OK 게이트를 공유하므로
    // attach/agents/subcommand(case 가 PERSONA_OK 를 비움)엔 안 붙어 서브커맨드를 오염 안
    // 시킨다. 불변식(session-id/--settings/task-list)은 노브가 아니라 계속 하드코딩.
    let persona_on = socket::read_claude_persona();
    let model = socket::read_claude_model();
    let effort = socket::read_claude_effort();
    let extra = socket::read_claude_extra();
    let extra = extra.trim();
    let persona_line = if persona_on {
        "[ -n \"$PERSONA_OK\" ] && [ -n \"$KASATERM_PERSONA\" ] && set -- --append-system-prompt \"$KASATERM_PERSONA\" \"$@\"\n".to_string()
    } else {
        String::new()
    };
    let model_line = if model.is_empty() {
        String::new()
    } else {
        // 모델 문자열은 작은따옴표로 감싼다 — `claude-opus-5[1m]` 의 `[1m]` 이
        // zsh 글롭이라 무인용이면 "no matches found" 로 set 이 통째 실패해
        // --model 이 아예 안 붙고 claude 가 기본 모델(구세대 Opus)로 떨어졌다
        // (거노 2026-07-27 실사고: 학생이 전부 4.8). 작은따옴표 이스케이프로
        // 임의 모델 문자열도 안전하게 리터럴 전달한다.
        let q = model.replace('\'', "'\\''");
        format!("[ -n \"$PERSONA_OK\" ] && set -- --model '{q}' \"$@\"\n")
    };
    let effort_line = if effort.is_empty() {
        String::new()
    } else {
        format!("[ -n \"$PERSONA_OK\" ] && export CLAUDE_EFFORT={effort}\n")
    };
    let extra_line = if extra.is_empty() {
        String::new()
    } else {
        format!("[ -n \"$PERSONA_OK\" ] && set -- {extra} \"$@\"\n")
    };
    let persona_block = format!("{persona_line}{model_line}{effort_line}{extra_line}");
    // 계정 전환. persona/model 노브와 달리 **PERSONA_OK 게이트 밖**이다 — 저것들은
    // attach·agents·-p·stop/logs 에서 일부러 빠지지만, 인증이 서브커맨드마다 다른
    // 계정을 보면 그건 그냥 고장이다. 디렉터리는 여기서 만들어 둔다: macOS 는 경로를
    // 해시해 Keychain 항목명만 가르지만, 다른 OS 는 이 안에 .credentials.json 을 쓴다.
    let account_dir = socket::claude_account_dir(&socket::read_claude_account());
    if let Some(ref d) = account_dir {
        if let Err(e) = std::fs::create_dir_all(d) {
            eprintln!("[shim] claude account dir 생성 실패: {e}");
        }
    }
    // 사용량 pill 도 같은 계정을 봐야 한다. `/claude-usage` 핸들러는 이 프로세스 안에서
    // 돌므로 우리 env 로 알린다 — shim 을 다시 깔 때마다(=계정을 바꿀 때마다) 갱신되니
    // pill 이 다음 폴링부터 새 계정 한도를 읽는다. 자식 pane 도 이 값을 물려받지만
    // 읽는 쪽이 없고, shim 이 보는 이름과 달라 서로 간섭하지 않는다.
    std::env::set_var(
        "KASATERM_CLAUDE_ACCOUNT_DIR",
        account_dir.as_deref().map_or(String::new(), |d| d.display().to_string()),
    );
    let account_block = claude_account_export_line(account_dir.as_deref());
    // teammate 트리플 자동 부착은 제거됐다(거노 2026-07-24): kasaterm 재시작으로 복원된
    // 세션(claude --resume)은 인박스 폴러가 안 돌아 SendMessage 가 조용히 유실되고(파일에만
    // 쌓임, 아루 실측), tell 이 발신 학생 프사·색으로 렌더되면서 크로스 pane 통신의 정식
    // 경로가 됐다. 팀 채널(SendMessage)은 오케스트레이터가 학생을 **명시 트리플로 스폰**한
    // 세트 안에서만 연다 — 사용자/스폰 다이얼로그의 --agent-* 는 이전처럼 그대로 통과된다.
    // 아래 블록은 트리플과 무관하게 유지되는 resume 연속성 처리만 남긴 것:
    // - TSID 파싱(--session-id/--resume 값, --continue 는 cwd 프로젝트 최신 transcript 추론)
    // - KASATERM_RESUMED_SID/RESUME_PICKER 마커(statusline ⑂bg 오발화 방지)
    // - resume 부팅 캐릭터 정합 교정(거노: 모모이 세션이 프라나 배지·persona 로 부팅)
    let team_block = format!(
        "TSID=\"$SID\"; prev=\"\"\n\
for a in \"$@\"; do case \"$prev\" in --session-id|--resume) case \"$a\" in -*) ;; *) TSID=\"$a\" ;; esac ;; esac; prev=\"$a\"; done\n\
# id 없는 --continue 는 claude 와 같은 기준(cwd 프로젝트 최신 transcript)으로 sid 를\n\
# 추론해 캐릭터 정합·RESUMED_SID 마커의 연속성을 유지한다(추론 실패는 마커 없이 부팅).\n\
case \" $* \" in\n\
*\" --continue \"*|*\" -c \"*) if [ -z \"$TSID\" ]; then\n\
  RSLUG=$(printf %s \"$PWD\" | sed 's![/.]!-!g')\n\
  LATEST=$(ls -t \"$HOME/.claude/projects/$RSLUG\"/*.jsonl 2>/dev/null | head -1)\n\
  [ -n \"$LATEST\" ] && TSID=$(basename \"$LATEST\" .jsonl)\n\
  case \"$TSID\" in ????????-????-????-????-????????????) ;; *) TSID=\"\" ;; esac\n\
fi ;;\n\
esac\n\
# 사용자 주도 resume 마커 — statusline 의 ⑂bg 배지가 anchor 불일치 휴리스틱이라\n\
# resume 세션 전부에 오발화한다(거노). id 있으면 그 sid 를, 피커/continue 는 플래그를\n\
# export 해 statusline 이 포크/attach 뷰(마커 없음)와 구분하게 한다. anchor\n\
# (KASATERM_SESSION_ID) 자체는 state.rs 캐릭터 복원이 원본을 요구해 안 덮는다.\n\
[ -n \"$TSID\" ] && export KASATERM_RESUMED_SID=\"$TSID\"\n\
case \" $* \" in *\" --resume \"*|*\" --continue \"*|*\" -c \"*) [ -z \"$TSID\" ] && export KASATERM_RESUME_PICKER=1 ;; esac\n\
# resume/명시 sid 부팅 — pane 상속 캐릭터 대신 그 세션의 정본(바인딩) 캐릭터로 정체성 교정\n\
# (거노: 모모이 세션이 프라나 배지·persona 로 부팅). 서버 죽으면 빈 응답 → pane env 폴백.\n\
if [ -n \"$PERSONA_OK\" ] && [ -z \"$SID\" ] && [ -n \"$TSID\" ]; then\n\
  RC=$(curl -s --max-time 2 --get --data-urlencode \"sid=$TSID\" \"http://127.0.0.1:${{KASASPACE_MCP_PORT:-8765}}/character\" 2>/dev/null)\n\
  if [ -n \"$RC\" ]; then\n\
    export KASATERM_CHARACTER=\"$RC\"\n\
    KASATERM_PERSONA=$(curl -s --max-time 2 --get --data-urlencode \"sid=$TSID\" \"http://127.0.0.1:${{KASASPACE_MCP_PORT:-8765}}/persona\" 2>/dev/null)\n\
  fi\n\
fi\n"
    );
    let wrapper = format!("#!/bin/sh\n\
# kasaterm pane-only claude wrapper — injects the collab hooks session-scoped\n\
# (--settings) so ~/.claude/settings.json stays untouched. Outside a pane this\n\
# wrapper isn't on PATH and claude runs exactly as the user configured it.\n\
HOOKS_DIR=\"{hd}\"\n\
SELF_DIR=$(CDPATH= cd -- \"$(dirname -- \"$0\")\" && pwd)\n\
CLEAN_PATH=$(printf '%s' \"$PATH\" | tr ':' '\\n' | grep -vxF \"$SELF_DIR\" | paste -sd: -)\n\
REAL=$(PATH=\"$CLEAN_PATH\" command -v claude 2>/dev/null)\n\
if [ -z \"$REAL\" ]; then\n\
  echo \"kasaterm claude shim: real claude not found on PATH\" >&2\n\
  exit 127\n\
fi\n\
{ablk}\
SETTINGS=\"$SELF_DIR/claude-hooks-settings.json\"\n\
# 백엔드가 이 pane 에 심은 캐릭터 정체성 적용(거노): persona = 시스템프롬프트 prefix(캐시,\n\
# per-turn 0), session-id = transcript 파일명 고정. 사용자가 --session-id/--resume 를\n\
# 직접 주면 그게 우선(우리 건 생략). --settings 도 사용자 지정이면 우리 걸 안 얹는다.\n\
USER_SETTINGS=0\n\
for a in \"$@\"; do [ \"$a\" = \"--settings\" ] && USER_SETTINGS=1 && break; done\n\
PERSONA_OK=1\n\
BGSUF=\"\"\n\
# attach/agents 는 서브커맨드, -p/--print 는 헤드리스 일회성 — persona·session-id 얹으면 깨진다(거노: 이어받기\n\
# 안 붙던 원인 · Bash 도구의 claude -p 가 pane session-id 강탈→board 가 그 pane 을 학생으로 둔갑·⑂bg 오발화).\n\
# --bg 는 session-id 를 자기가 관리(명시 지정은 무시+경고 실측)하지만 persona 는 새 세션이라 붙이고,\n\
# --agent-* 트리플은 데몬 스폰까지 전달된다(07-16 실측) — 이름 접미사만 랜덤 BGSUF(비-hex, bridge 매칭 회피).\n\
case \" $* \" in *\" attach \"*|*\" agents \"*|*\" -p \"*|*\" --print \"*) SID=\"\"; PERSONA_OK=\"\" ;; *\" --bg \"*|*\" --background \"*) SID=\"\"; BGSUF=$(od -An -N2 -tx1 /dev/urandom | tr -d ' \\n' | tr '0123456789abcdef' 'ghjkmnpqrstvwxyz') ;; *\" --session-id \"*|*\" --resume \"*|*\" --continue \"*|*\" -c \"*) SID=\"\" ;; *) SID=\"$KASATERM_SESSION_ID\" ;; esac\n\
# stop/logs 도 세션 지정 서브커맨드 — session-id/persona/트리플을 얹으면 claude 가 서브커맨드를\n\
# 프롬프트 positional 로 소비해 유령 세션 부팅/\"already in use\"(실측 07-16). $1 정확 일치는\n\
# zshrc claude() 알리아스(--dangerously-skip-permissions prepend)에 깨진다(실측) — 첫 non-flag\n\
# 인자로 판정. 값 받는 플래그가 stop/logs 앞에 오는 조합은 비현실적이라 허용 리스크.\n\
SUB=\"\"; for a in \"$@\"; do case \"$a\" in -*) ;; *) SUB=\"$a\"; break ;; esac; done\n\
case \"$SUB\" in stop|logs) SID=\"\"; PERSONA_OK=\"\" ;; esac\n\
# 학생 명령(`시로코`)의 pane 별 정체성 override — env 는 셸 spawn 시 고정이라 재배정은\n\
# 파일로 온다. 있으면 persona/character 를 덮는다(빈 파일 = persona 없는 학생 = 미적용).\n\
OVP=\"$SELF_DIR/repersona-${{KASATERM_PANE_ID}}.persona\"\n\
if [ -n \"$KASATERM_PANE_ID\" ] && [ -f \"$OVP\" ]; then\n\
  KASATERM_PERSONA=$(cat \"$OVP\")\n\
  [ -f \"${{OVP%.persona}}.character\" ] && export KASATERM_CHARACTER=\"$(cat \"${{OVP%.persona}}.character\")\"\n\
fi\n\
{tblk}\
{pblk}\
[ -n \"$SID\" ] && set -- --session-id \"$SID\" \"$@\"\n\
# task store(~/.claude/tasks/<id>)를 transcript session 과 같은 키로 묶는다 — 없으면 claude\n\
# 가 매 실행 임의 session-<hex8> 로 task 를 저장해 pane↔task 매핑이 끊긴다(거노: 유즈\n\
# 업무탭 빔). SID 비면(사용자 --resume) claude 기본.\n\
[ -n \"$SID\" ] && export CLAUDE_TASK_LIST_ID=\"$SID\"\n\
if [ \"$USER_SETTINGS\" = 1 ] || [ ! -f \"$SETTINGS\" ]; then\n\
  exec \"$REAL\" \"$@\"\n\
fi\n\
exec \"$REAL\" --settings \"$SETTINGS\" \"$@\"\n",
        hd = hd, tblk = team_block, pblk = persona_block, ablk = account_block);
    let wrapper_path = shim_dir.join("claude");
    if let Err(e) = std::fs::write(&wrapper_path, wrapper) {
        eprintln!("[shim] write claude wrapper failed: {e}");
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) =
            std::fs::set_permissions(&wrapper_path, std::fs::Permissions::from_mode(0o755))
        {
            eprintln!("[shim] chmod claude wrapper failed: {e}");
        }
    }
    // kasacollab(협업 CLI)도 pane PATH 에 스테이징 — 훅이 아니라 셸/claude 가
    // 직접 부르는 명령이라 settings 주입으로는 못 싣는다. 예전엔 ~/.local/bin
    // 수동 설치(개인 설정 오염 + 정본 이동 시 무음 고장)였다.
    let collab = format!(
        "#!/bin/sh\nexec python3 \"{hd}/kasacollab.py\" \"$@\"\n"
    );
    let collab_path = shim_dir.join("kasacollab");
    if let Err(e) = std::fs::write(&collab_path, collab) {
        eprintln!("[shim] write kasacollab wrapper failed: {e}");
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) =
            std::fs::set_permissions(&collab_path, std::fs::Permissions::from_mode(0o755))
        {
            eprintln!("[shim] chmod kasacollab wrapper failed: {e}");
        }
    }
}

/// 학생 이름을 pane 명령으로 스테이징 — `시로코`(또는 슬러그 `shiroko`)를 치면
/// 그 pane 을 해당 학생으로 재배정하고 claude 를 실행한다. persona 는 override
/// 파일(`repersona-<pane>.persona`)로 claude 래퍼에 전달(env 는 셸 spawn 시
/// 고정이라 늦게 못 바꿈), GUI 상태(헤더·테두리·board 마커·세션바인딩)는
/// `/repersona` 엔드포인트가 갱신한다. 중복 허용 — 같은 학생 pane 은 색 변주
/// (theme::accent_variant)로 구분. characters.json 기준 부팅 1회 생성(다른 shim
/// 노브와 동일하게 변경은 재시작 후 적용).
fn install_student_shims(shim_dir: &std::path::Path) {
    // POSIX sh 스크립트 — Windows pane 셸도 Git bash 라 그대로 동작(curl 포함).
    let Some(chars) = kasa_mcp::character::characters_json() else {
        return;
    };
    let sq = |s: &str| s.replace('\'', "'\\''");
    for name in kasa_mcp::character::member_names(&chars) {
        let persona = kasa_mcp::character::persona_for(&chars, &name).unwrap_or_default();
        let script = format!(
            "#!/bin/sh\n\
# kasaterm 학생 런처 — 이 pane 을 '{name}' 로 재배정하고 claude 실행.\n\
SELF_DIR=$(CDPATH= cd -- \"$(dirname -- \"$0\")\" && pwd)\n\
if [ -n \"$KASATERM_PANE_ID\" ]; then\n\
  printf '%s' '{persona_sq}' > \"$SELF_DIR/repersona-$KASATERM_PANE_ID.persona\"\n\
  printf '%s' '{name_sq}' > \"$SELF_DIR/repersona-$KASATERM_PANE_ID.character\"\n\
  curl -s --get --data-urlencode \"surface=$KASATERM_PANE_ID\" \\\n\
    --data-urlencode \"character={name_sq}\" \\\n\
    \"http://127.0.0.1:${{KASASPACE_MCP_PORT:-8765}}/repersona\" >/dev/null 2>&1\n\
fi\n\
[ \"$1\" = claude ] && shift\n\
exec claude \"$@\"\n",
            name_sq = sq(&name),
            persona_sq = sq(&persona),
        );
        // 한글 정식 이름 + 로마자 슬러그 별칭(IME 전환 없이도 실행) 둘 다 스테이징.
        let mut cmd_names: Vec<String> = vec![name.clone()];
        if let Some(slug) = theme::character_slug(&name) {
            cmd_names.push(slug.to_string());
        }
        for cmd in cmd_names {
            let path = shim_dir.join(&cmd);
            if let Err(e) = std::fs::write(&path, &script) {
                eprintln!("[shim] write student shim {cmd} failed: {e}");
                continue;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Err(e) =
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                {
                    eprintln!("[shim] chmod student shim {cmd} failed: {e}");
                }
            }
        }
    }
}

/// Pick the shell to spawn inside a PTY. claude code's teammate mode
/// emits Unix-quoted commands (`cd 'path' && env VAR=val cmd`), so a
/// cmd.exe default leaves teammate spawns dead on arrival. Honor
/// KASATERM_SHELL / SHELL when set, otherwise auto-discover Git for
/// Windows' bash so users with a stock setup get a working unix-style
/// shell without configuration. Returns None to let portable-pty's
/// `new_default_prog` pick (cmd.exe on Windows, $SHELL on Unix).
/// Prefix well-known interactive programs with a small sigil so the
/// pane header reads at a glance. Mirrors how the programs themselves
/// brand their own OSC titles in other terminals (Claude Code ships
/// "✱ Claude Code", vim/less label themselves with their name). For
/// anything we don't have an opinion on, just return the comm as-is.
fn decorate_process_name(comm: &str) -> String {
    match comm {
        "claude" => "✱ claude".to_string(),
        "node" | "deno" | "bun" => format!("⬢ {comm}"),
        "vim" | "nvim" => format!("⌨ {comm}"),
        "less" | "more" => format!("☰ {comm}"),
        "git" => format!("⎇ {comm}"),
        _ => comm.to_string(),
    }
}


/// Where the first shell of a fresh session starts. No spawning pane exists
/// yet, so the `"last"` mode falls back to home — same as every terminal's
/// very first window.
pub(crate) fn resolve_initial_cwd() -> Option<String> {
    resolve_spawn_cwd(None)
}

/// Resolve the cwd for a newly spawned shell, honoring the user's `default_cwd`
/// setting like other terminals' "working directory" option:
///   - `"last"` (default) with a spawning `prev` pane → inherit its cwd. This
///     wins over `KASATERM_CWD` on purpose: that env is a *first-pane* launch
///     override (a launcher saying "start this instance here"), and it leaks
///     into child shells via env inheritance, so letting it beat the split
///     inheritance would pin every sibling to the launch dir instead of the
///     pane the user split off.
///   - `KASATERM_CWD` env (explicit launch override) — first pane / fixed modes.
///   - `"home"` → `$HOME`.
///   - an absolute or `~`-prefixed path → that directory if it exists.
/// Anything unresolved falls through to home, then the process cwd.
pub(crate) fn resolve_spawn_cwd(prev: Option<std::path::PathBuf>) -> Option<String> {
    let mode = socket::read_default_cwd_mode();
    if mode == "last" {
        if let Some(p) = prev.and_then(|p| p.to_str().map(String::from)) {
            return Some(p);
        }
    }
    if let Ok(dir) = std::env::var("KASATERM_CWD") {
        if !dir.is_empty() {
            return Some(dir);
        }
    }
    match mode.as_str() {
        // "last" with no prev (first boot) falls through to home.
        "last" | "home" => {}
        path => {
            let expanded = match path.strip_prefix("~/") {
                Some(rest) => match std::env::var("HOME") {
                    Ok(home) if !home.is_empty() => format!("{home}/{rest}"),
                    _ => path.to_string(),
                },
                None => path.to_string(),
            };
            if std::path::Path::new(&expanded).is_dir() {
                return Some(expanded);
            }
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return Some(home);
        }
    }
    std::env::current_dir()
        .ok()
        .and_then(|p| p.to_str().map(String::from))
}

fn resolve_default_shell() -> Option<String> {
    if let Ok(s) = std::env::var("KASATERM_SHELL") {
        if !s.is_empty() {
            return Some(s);
        }
    }
    // User's explicit choice in the settings screen wins over `$SHELL` so it
    // overrides the inherited login shell, but stays below the env launch
    // override above.
    if let Some(s) = socket::read_default_shell() {
        return Some(s);
    }
    if let Ok(s) = std::env::var("SHELL") {
        if !s.is_empty() {
            return Some(s);
        }
    }
    #[cfg(windows)]
    {
        // PowerShell 7 (pwsh) preferred, then the OS-bundled Windows
        // PowerShell (always present), then Git Bash. The settings-screen /
        // env overrides above still win, so this is only the out-of-box pick.
        let pwsh7 = r"C:\Program Files\PowerShell\7\pwsh.exe";
        if std::path::Path::new(pwsh7).is_file() {
            return Some(pwsh7.to_string());
        }
        if let Some(bash) = git_bash_path() {
            return Some(bash);
        }
        return Some("powershell.exe".to_string());
    }
    #[allow(unreachable_code)]
    None
}

/// First installed Git Bash, if any. Git for Windows ships a Unix-like
/// shell that's the closest match to the macOS zsh workflow kasaterm was
/// built around (so `ls`/`grep`/`claude` etc. just work).
#[cfg(windows)]
fn git_bash_path() -> Option<String> {
    for candidate in &[
        r"C:\Program Files\Git\bin\bash.exe",
        r"C:\Program Files\Git\usr\bin\bash.exe",
        r"C:\Program Files (x86)\Git\bin\bash.exe",
    ] {
        if std::path::Path::new(candidate).is_file() {
            return Some((*candidate).to_string());
        }
    }
    None
}

/// Shells offered by the sidebar "+" picker: `(label, icon_svg name,
/// shell command)`. Windows 전용 — 설치된 셸(PowerShell/CMD/Git Bash/WSL)만 나열,
/// 없는 셸은 조용히 빠진다. macOS/Linux 는 빈 목록 → "+" 가 즉시 기본 셸 스폰.
pub(crate) fn available_shells() -> Vec<(&'static str, &'static str, String)> {
    #[allow(unused_mut)]
    let mut out: Vec<(&'static str, &'static str, String)> = Vec::new();
    #[cfg(windows)]
    {
        let pwsh7 = r"C:\Program Files\PowerShell\7\pwsh.exe";
        if std::path::Path::new(pwsh7).is_file() {
            out.push(("PowerShell 7", "terminal", pwsh7.to_string()));
        }
        // Windows PowerShell ships with the OS — always present.
        out.push(("Windows PowerShell", "terminal", "powershell.exe".to_string()));
        out.push(("Command Prompt", "terminal", "cmd.exe".to_string()));
        if let Some(bash) = git_bash_path() {
            out.push(("Git Bash", "terminal", bash));
        }
        if std::path::Path::new(r"C:\Windows\System32\wsl.exe").is_file() {
            out.push(("WSL", "terminal", "wsl.exe".to_string()));
        }
    }
    out
}

/// Decide where the agent-socket should live. Honors caller-supplied
/// overrides first (`KASATERM_SOCKET_PATH`, then the cmux convention),
/// and falls back to a per-pid socket under the system temp dir. Used
/// in two places — the early env-var seed in `start_pty` so the very
/// first shell sees a stable value, and the actual server bind in
/// `start_socket_with` — and must return the same path in both.
/// Path of the file the http-serving process (daemon, or in-process GUI)
/// writes its ACTUAL bound MCP port into, so the panel webviews poll the right
/// port even when the preferred one was taken and `spawn_http_server` fell back
/// to an OS-assigned one. Lives beside the control socket (same
/// `KASATERM_SOCKET_PATH` the daemon inherits) so GUI and daemon resolve the
/// same file.
pub(crate) fn mcp_port_file_path() -> std::path::PathBuf {
    let sock = std::env::var("KASATERM_SOCKET_PATH").unwrap_or_else(|_| {
        format!(
            "{}/.config/kasaterm/daemon.sock",
            std::env::var("HOME").unwrap_or_default()
        )
    });
    std::path::Path::new(&sock)
        .parent()
        .map(|p| p.join("mcp_port"))
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp/kasaterm-mcp-port"))
}

/// Port the panel webviews should poll. This process's own bound port
/// (`KASASPACE_MCP_PORT`, set by `start_socket_with` right after the server
/// binds) wins — env is per-process, so each instance's panels always reach
/// their own server. `mcp_port_file_path` is a single global file that any
/// concurrent instance overwrites; trusting it first stranded a second
/// instance's panel webview on a dead/foreign port (the panel window stays
/// hidden forever waiting for a load that never finishes). So the file is
/// only a fallback for callers without the env, else the 8765 default.
fn mcp_panel_port() -> String {
    let trimmed_nonempty = |s: String| {
        let s = s.trim().to_string();
        (!s.is_empty()).then_some(s)
    };
    std::env::var("KASASPACE_MCP_PORT")
        .ok()
        .and_then(trimmed_nonempty)
        .or_else(|| std::fs::read_to_string(mcp_port_file_path()).ok().and_then(trimmed_nonempty))
        .unwrap_or_else(|| "8765".to_string())
}

/// 부팅 시 temp_dir 의 죽은 `kasaterm-<pid>.sock` 잔재를 청소한다. 소켓 경로가
/// PID 별이라 인스턴스마다 다른 파일을 만드는데, `Server::bind` 의 stale 정리는
/// *자기 경로* 만 치워서 죽은 다른 인스턴스 소켓이 영영 남는다(재시작·빌드 반복
/// 시 누적). 여기서 connect 가 실패하는(=리스너 없는) 소켓 파일만 지운다 —
/// 살아있는 인스턴스 소켓은 절대 건드리지 않으므로 멀티 인스턴스에서도 안전.
/// 자기 PID 소켓은 아직 bind 전이라 connect 가 실패할 수 있으니 제외한다.
#[cfg(unix)]
/// 죽은 인스턴스의 소켓 잔재를 지우고, *살아있는 다른 인스턴스*가 있는지 돌려준다
/// (connect 성공 = 살아있는 리스너). 반환값은 stale collab 마커 청소의 게이트.
fn sweep_dead_kasaterm_sockets() -> bool {
    let own = format!("kasaterm-{}.sock", std::process::id());
    let mut other_alive = false;
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return other_alive;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("kasaterm-") || !name.ends_with(".sock") || name == own {
            continue;
        }
        let path = entry.path();
        if std::os::unix::net::UnixStream::connect(&path).is_err() {
            let _ = std::fs::remove_file(&path);
        } else {
            other_alive = true;
        }
    }
    other_alive
}

/// 부팅 시 collab 캐릭터 마커 전면 청소 — 마커는 live pane 추적용인데 강제종료·크래시가
/// 남긴 stale 이 assigned_global 의 유령 taken 을 만들어 랜덤 배정 풀을 쪼그라뜨린다
/// (거노: 새 학생이 안 나옴). 우리 pane 은 아직 스폰 전이고(restore 가 마커를 다시 쓴다)
/// 다른 인스턴스가 살아 있으면 그쪽 live 마커를 구분할 수 없어 통째로 건너뛴다(보수적).
#[cfg(unix)]
fn cleanup_stale_collab_markers() {
    let Ok(rooms) = std::fs::read_dir("/tmp/kasaterm-collab") else {
        return;
    };
    for room in rooms.flatten() {
        let Ok(files) = std::fs::read_dir(room.path()) else {
            continue;
        };
        for f in files.flatten() {
            if f.file_name().to_string_lossy().starts_with("character-") {
                let _ = std::fs::remove_file(f.path());
            }
        }
    }
}

fn resolve_kasaterm_socket_path() -> String {
    let own = || {
        format!(
            "{}/kasaterm-{}.sock",
            std::env::temp_dir().to_string_lossy(),
            std::process::id()
        )
    };
    let inherited = std::env::var("KASATERM_SOCKET_PATH")
        .or_else(|_| std::env::var("CMUX_SOCKET_PATH"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    match inherited {
        // 부모 pane이 물려준 소켓 경로가 *이미 살아있는 다른 인스턴스*를 가리키면
        // (우리가 claude pane 안에서 `cargo run`으로 띄워진 자식인 경우), 그 경로에
        // bind 하면 Server::bind 가 기존 소켓 파일을 지우고 덮어써 메인 앱 소켓을
        // 탈취한다 — 그 결과 모든 pane 의 kasaterm-cli 가 빈 자식 인스턴스로 붙어
        // board 가 텅 빈다. connect 가 성공하면(=살아있는 리스너) 탈취하지 말고 우리
        // PID 경로로 격리한다. start_socket_with 가 resolved 경로를 자식 pane 에 다시
        // export 하므로 자식 창 pane 들도 자동으로 우리 소켓을 따라온다.
        Some(p) => {
            #[cfg(unix)]
            if std::os::unix::net::UnixStream::connect(&p).is_ok() {
                return own();
            }
            p
        }
        None => own(),
    }
}

/// Locate the kasaterm-cli binary so we can stage it on the pane PATH
/// (install_pane_shims). Env override first, then sibling of the current exe.
fn locate_cmux_compat_binary() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("KASATERM_CMUX_COMPAT_BIN") {
        let p = std::path::PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    for name in ["kasaterm-cli.exe", "kasaterm-cli"] {
        let c = dir.join(name);
        if c.is_file() {
            return Some(c);
        }
    }
    None
}

/// Place the shim binary at `target` so child shells can find it.
/// Symlink first, fall back to a plain copy when the platform refuses
/// (Windows without Developer Mode or admin will reject CreateSymbolicLink).
fn stage_shim(src: &std::path::Path, target: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Copy the bytes (not a symlink) so the staged helper is a
        // standalone copy in $TMPDIR, fully decoupled from the app
        // bundle. That makes a *running* app survive an in-place bundle
        // replace (rm -rf + cp during `build-app.sh --install`): the
        // already-spawned panes keep exec'ing this stable copy, and the
        // next launch re-stages a fresh copy from the new bundle. A
        // symlink would dangle the instant the bundle's inode changed,
        // breaking `tmux` split / `imgcat` mid-session. Re-copying every
        // start (caller removes the old target first) also kills any
        // stale helper from a previous build.
        std::fs::copy(src, target)?;
        std::fs::set_permissions(target, std::fs::Permissions::from_mode(0o755))?;
        Ok(())
    }
    #[cfg(windows)]
    {
        match std::os::windows::fs::symlink_file(src, target) {
            Ok(()) => Ok(()),
            Err(_) => {
                // Symlink path failed (likely a non-admin, non-dev-mode
                // user). Copy the bytes so we still end up with a
                // working tmux.exe in the shim dir.
                std::fs::copy(src, target).map(|_| ())
            }
        }
    }
}

/// PrintWindow + GDI capture of our own HWND, encoded as PNG.
/// PW_RENDERFULLCONTENT pulls the wgpu/DXGI swap-chain contents that
/// plain BitBlt would miss; we fall back to a BitBlt from the window
/// DC if PrintWindow returns 0 (rare, but seen on some legacy GPUs).
#[cfg(windows)]
fn capture_window_to_png_windows(
    hwnd_val: isize,
    path: &str,
) -> std::io::Result<(i32, i32)> {
    use std::io::Error;
    use windows_sys::Win32::Foundation::{HWND, RECT};
    use windows_sys::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
        GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
        DIB_RGB_COLORS, SRCCOPY,
    };
    use windows_sys::Win32::Storage::Xps::PrintWindow;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetClientRect, SetForegroundWindow, PW_RENDERFULLCONTENT,
    };

    let hwnd: HWND = hwnd_val as *mut std::ffi::c_void;
    unsafe { SetForegroundWindow(hwnd) };
    std::thread::sleep(std::time::Duration::from_millis(200));

    let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    if unsafe { GetClientRect(hwnd, &mut rect) } == 0 {
        return Err(Error::other("GetClientRect failed"));
    }
    let w = rect.right - rect.left;
    let h = rect.bottom - rect.top;
    if w <= 0 || h <= 0 {
        return Err(Error::other(format!("client rect zero: {w}x{h}")));
    }

    let pixels = unsafe {
        let hdc_window = GetDC(hwnd);
        if hdc_window.is_null() {
            return Err(Error::other("GetDC returned null"));
        }
        let hdc_mem = CreateCompatibleDC(hdc_window);
        let hbm = CreateCompatibleBitmap(hdc_window, w, h);
        let old = SelectObject(hdc_mem, hbm as _);

        let ok = PrintWindow(hwnd, hdc_mem, PW_RENDERFULLCONTENT);
        if ok == 0 {
            // Fallback path. Only useful if the window is actually on
            // screen and not occluded — PrintWindow usually wins.
            BitBlt(hdc_mem, 0, 0, w, h, hdc_window, 0, 0, SRCCOPY);
        }

        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = w;
        // Negative height = top-down DIB so row 0 sits at the top, which
        // is what PNG expects.
        bmi.bmiHeader.biHeight = -h;
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB as u32;

        let mut buf = vec![0u8; (w as usize) * (h as usize) * 4];
        GetDIBits(
            hdc_mem,
            hbm,
            0,
            h as u32,
            buf.as_mut_ptr() as *mut _,
            &mut bmi,
            DIB_RGB_COLORS,
        );

        SelectObject(hdc_mem, old);
        DeleteObject(hbm as _);
        DeleteDC(hdc_mem);
        ReleaseDC(hwnd, hdc_window);

        // GDI hands us BGRA with alpha frequently zeroed. Swap to RGBA
        // and stamp alpha = 0xFF so PNG viewers don't render us as fully
        // transparent.
        for px in buf.chunks_exact_mut(4) {
            px.swap(0, 2);
            px[3] = 0xFF;
        }
        buf
    };

    let file = std::fs::File::create(path)?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|e| Error::other(format!("png header: {e}")))?;
    writer
        .write_image_data(&pixels)
        .map_err(|e| Error::other(format!("png data: {e}")))?;
    Ok((w, h))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn ms(t: Instant, n: u64) -> Instant {
        t + Duration::from_millis(n)
    }

    fn spans_of(src: &str) -> Vec<(String, Option<String>)> {
        let (blocks, _) = parse_markdown(src);
        blocks
            .iter()
            .filter_map(|b| match b {
                MdBlock::ListItem { spans, .. }
                | MdBlock::Para { spans }
                | MdBlock::Heading { spans, .. }
                | MdBlock::Callout { spans, .. }
                | MdBlock::Quote { spans } => Some(spans),
                _ => None,
            })
            .flatten()
            .map(|s| (s.text.clone(), s.link.clone()))
            .collect()
    }

    /// 파서 전체를 거쳐야 하는 테스트 — cmark 가 `[[` 를 낱개 이벤트로 흘리므로
    /// `push_wikilinked` 단독 테스트만으론 실제 문서에서 링크가 죽는 걸 못 잡는다
    /// (실제로 그렇게 놓쳐서 화면에 대괄호가 그대로 나왔다).
    #[test]
    fn wikilink_survives_the_whole_parser() {
        for src in [
            "- [[topic_a]] — 뒤에 설명\n",
            "[[topic_a]] 문단\n",
            "# [[topic_a]] 제목\n",
            "> [[topic_a]] 인용\n",
        ] {
            let got = spans_of(src);
            assert_eq!(
                got.first().map(|(t, l)| (t.as_str(), l.as_deref())),
                Some(("topic_a", Some("wiki:topic_a"))),
                "{src:?} 에서 링크가 죽었다: {got:?}"
            );
        }
    }

    /// 알림 종류별 블록 모양. `first`/`last` 가 상자를 그리는 기준이라, 이게
    /// 어긋나면 배경이 안 그려지거나 문단마다 상자가 끊긴다.
    #[test]
    fn callout_tags_become_callout_blocks() {
        for (src, want) in [
            ("> [!NOTE]\n> 알림\n", MdCallout::Note),
            ("> [!TIP]\n> 팁\n", MdCallout::Tip),
            ("> [!IMPORTANT]\n> 중요\n", MdCallout::Important),
            ("> [!WARNING]\n> 경고\n", MdCallout::Warning),
            ("> [!CAUTION]\n> 주의\n", MdCallout::Caution),
        ] {
            let (blocks, _) = parse_markdown(src);
            match blocks.as_slice() {
                [MdBlock::Callout { kind, first, last, spans, .. }] => {
                    assert_eq!(*kind, want, "{src:?}");
                    assert!(*first && *last, "{src:?} 한 문단이면 상자를 열고 닫아야 한다");
                    // 태그 줄은 표지로 그려지므로 본문에 남으면 두 번 보인다.
                    let text: String = spans.iter().map(|s| s.text.as_str()).collect();
                    assert!(!text.contains("[!"), "{src:?} 본문에 태그가 남았다: {text:?}");
                }
                other => panic!("{src:?} → 콜아웃이 아니다: {} 블록", other.len()),
            }
        }
    }

    /// 알림이 아닌 것은 알림이 되지 말아야 한다 — 인용문은 인용문으로 남는다.
    #[test]
    fn plain_and_unknown_quotes_stay_quotes() {
        for src in ["> 그냥 인용문\n", "> [!HELLO]\n> 없는 종류\n"] {
            let (blocks, _) = parse_markdown(src);
            assert!(
                blocks.iter().all(|b| !matches!(b, MdBlock::Callout { .. })),
                "{src:?} 가 콜아웃으로 잡혔다"
            );
            assert!(
                blocks.iter().any(|b| matches!(b, MdBlock::Quote { .. })),
                "{src:?} 가 인용문으로도 안 남았다"
            );
        }
    }

    /// 여러 문단 알림은 첫 조각만 상자를 열고 마지막 조각만 닫는다 — 조각마다
    /// 상자를 그리면 이음새에서 배경이 겹쳐 그 띠만 색이 진해진다.
    #[test]
    fn multi_paragraph_callout_opens_once_and_closes_once() {
        let (blocks, _) = parse_markdown("> [!WARNING]\n> 첫 문단\n>\n> 둘째 문단\n");
        let flags: Vec<(bool, bool)> = blocks
            .iter()
            .filter_map(|b| match b {
                MdBlock::Callout { first, last, .. } => Some((*first, *last)),
                _ => None,
            })
            .collect();
        assert_eq!(flags, vec![(true, false), (false, true)], "조각 표시가 어긋났다");
    }

    /// 알림 안 목록은 상자 안에 남아야 한다 — `ListItem` 으로 새면 경고에 딸린
    /// 목록이 경고 밖의 글로 읽힌다(실제로 상자 아래에 매달려 나왔다).
    #[test]
    fn list_inside_callout_stays_in_the_box() {
        let (blocks, _) = parse_markdown("> [!WARNING]\n> 확인해라.\n>\n> - 첫째\n> - 둘째\n");
        assert!(
            blocks.iter().all(|b| !matches!(b, MdBlock::ListItem { .. })),
            "목록이 상자 밖으로 샜다"
        );
        let depths: Vec<Option<u8>> = blocks
            .iter()
            .filter_map(|b| match b {
                MdBlock::Callout { list, .. } => Some(list.as_ref().map(|(d, _)| *d)),
                _ => None,
            })
            .collect();
        assert_eq!(depths, vec![None, Some(0), Some(0)], "문단 하나 + 목록 둘이어야 한다");
        assert!(
            matches!(blocks.last(), Some(MdBlock::Callout { last: true, .. })),
            "마지막 조각이 상자를 닫지 않았다"
        );
    }

    /// 알림 안에서도 문서 사이 링크는 살아 있어야 한다 — 경고문에 관련 문서를
    /// 달아 두는 게 이 볼트의 실제 사용법이다.
    #[test]
    fn wikilink_works_inside_callout() {
        let got = spans_of("> [!WARNING]\n> 자세히는 [[topic_a]] 참고\n");
        assert!(
            got.iter().any(|(t, l)| t == "topic_a" && l.as_deref() == Some("wiki:topic_a")),
            "알림 안 링크가 죽었다: {got:?}"
        );
    }

    /// 인라인 코드 안의 표기는 링크가 아니다 — 문서에서 표기 자체를 설명할 때
    /// 쓰는 자리다(이 레포 주석·메모리 문서가 실제로 그렇게 쓴다).
    #[test]
    fn wikilink_inside_inline_code_stays_literal() {
        let got = spans_of("`[[topic_a]]` 는 표기다\n");
        assert!(
            got.iter().all(|(_, l)| l.is_none()),
            "코드 안 표기가 링크가 됐다: {got:?}"
        );
    }

    /// HTML 태그에 감싸인 본문은 살아야 한다. 옛 렌더는 `Event::Html` 을 통째로
    /// 버려서, 유효한 태그 이름을 만나면 그 안 문장이 화면에서 사라졌다 —
    /// GitHub 접기 절(`<details>`)의 제목과 `<div>` 안 문장이 실제로 그랬다.
    #[test]
    fn html_tag_bodies_survive() {
        let got = spans_of(
            "<details>\n<summary>접히는 제목</summary>\n</details>\n\n\
             <div align=\"center\">\n가운데 문장.\n</div>\n\n\
             <system-reminder>\n하이픈 태그 안.\n</system-reminder>\n",
        );
        let texts: Vec<&str> = got.iter().map(|(t, _)| t.as_str()).collect();
        for want in ["접히는 제목", "가운데 문장.", "하이픈 태그 안."] {
            assert!(texts.iter().any(|t| t.contains(want)), "{want} 이 사라졌다: {got:?}");
        }
        // 태그 자체는 문서에 없던 글자다.
        assert!(
            texts.iter().all(|t| !t.contains('<') && !t.contains('>')),
            "태그가 글자로 그려졌다: {got:?}"
        );
    }

    /// 주석은 반대로 감춰져야 한다 — 안 보이게 하려고 쓴 표기라, 태그만 벗겨
    /// 내용을 드러내면 글쓴이의 뜻이 뒤집힌다.
    #[test]
    fn html_comments_stay_hidden() {
        let got = spans_of("앞 문단.\n\n<!-- 감춰 둔 메모 -->\n\n뒤 문단.\n");
        assert!(
            got.iter().all(|(t, _)| !t.contains("감춰 둔 메모")),
            "주석이 드러났다: {got:?}"
        );
    }

    /// 인라인 태그는 서체로 옮긴다. 태그를 지우기만 하면 강조가 사라지고,
    /// 글자로 그리면 문서에 없던 꺾쇠가 생긴다.
    #[test]
    fn inline_html_maps_to_styles() {
        let (blocks, _) = parse_markdown("평범 <b>굵게</b> 와 <em>기울게</em> 끝\n");
        let spans = match &blocks[0] {
            MdBlock::Para { spans } => spans,
            _ => panic!("문단이 아니다"),
        };
        let shape: Vec<(&str, bool, bool)> = spans
            .iter()
            .map(|s| (s.text.as_str(), s.bold, s.italic))
            .collect();
        assert!(
            shape.iter().any(|&(t, b, _)| t == "굵게" && b),
            "b 태그가 굵게로 안 옮겨졌다: {shape:?}"
        );
        assert!(
            shape.iter().any(|&(t, _, i)| t == "기울게" && i),
            "em 태그가 기울게로 안 옮겨졌다: {shape:?}"
        );
        assert!(
            shape.iter().all(|&(t, ..)| !t.contains('<')),
            "태그가 글자로 남았다: {shape:?}"
        );
    }

    /// `<br>` 는 스팬에 줄바꿈 표기가 없어 띄어쓰기로 떨어진다 — 그냥 지우면
    /// 앞뒤 낱말이 한 덩어리로 붙는다(표 셀에서 자주 쓰는 표기다).
    #[test]
    fn inline_br_becomes_space() {
        let got = spans_of("여러<br>줄\n");
        let joined: String = got.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(joined, "여러 줄");
    }

    /// `[[이름]]` 은 대괄호를 벗긴 링크 스팬이 되고 앞뒤 글은 평문으로 남아야 한다 —
    /// 메모리 인덱스가 통째로 이 표기라, 여기서 어긋나면 문서 사이 이동이 죽는다.
    #[test]
    fn wikilink_becomes_link_span() {
        let mut spans = Vec::new();
        push_wikilinked(&mut spans, "앞 [[topic_a]] 뒤", false, false, false);
        let shape: Vec<(&str, Option<&str>)> = spans
            .iter()
            .map(|s| (s.text.as_str(), s.link.as_deref()))
            .collect();
        assert_eq!(
            shape,
            vec![
                ("앞 ", None),
                ("topic_a", Some("wiki:topic_a")),
                (" 뒤", None)
            ]
        );
    }

    /// 닫히지 않은 `[[` 나 대괄호가 섞인 이름은 링크가 아니다. 링크로 만들면 문서에
    /// 없는 파일을 가리키는 죽은 링크가 생긴다.
    #[test]
    fn malformed_wikilink_stays_plain() {
        for src in ["[[열린 채", "[[a[b]]", "[[]]"] {
            let mut spans = Vec::new();
            push_wikilinked(&mut spans, src, false, false, false);
            assert!(
                spans.iter().all(|s| s.link.is_none()),
                "{src} 가 링크로 잡혔다"
            );
            let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
            assert_eq!(joined, src, "{src} 의 글자가 유실됐다");
        }
    }

    /// 한 줄에 여러 개, 그리고 붙어 있는 경우까지. 메모리 인덱스는 한 줄에 두세 개를
    /// 예사로 쓴다.
    #[test]
    fn multiple_wikilinks_in_one_run() {
        let mut spans = Vec::new();
        push_wikilinked(&mut spans, "[[a]]·[[b]]", false, false, false);
        let links: Vec<&str> = spans.iter().filter_map(|s| s.link.as_deref()).collect();
        assert_eq!(links, vec!["wiki:a", "wiki:b"]);
        let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, "a·b");
    }

    /// No account selected must emit *nothing*. An `export` with an empty value
    /// would not be inert — Claude Code treats a defined-but-empty
    /// `CLAUDE_SECURESTORAGE_CONFIG_DIR` as an explicit "use the unsuffixed
    /// store", which is a different code path from leaving it unset.
    #[test]
    fn no_claude_account_emits_no_shim_line() {
        assert_eq!(claude_account_export_line(None), "");
    }

    /// The guard must key on `+x` (set-ness), not the value. A child claude that
    /// inherited an explicit empty value is being told to stay on the default
    /// login; `[ -z "$VAR" ]` would read that as "unset" and hijack it.
    #[test]
    fn claude_account_line_guards_on_set_ness_not_emptiness() {
        let line = claude_account_export_line(Some(std::path::Path::new("/tmp/acct/a1")));
        assert_eq!(
            line,
            "[ -z \"${CLAUDE_SECURESTORAGE_CONFIG_DIR+x}\" ] && \
             export CLAUDE_SECURESTORAGE_CONFIG_DIR='/tmp/acct/a1'\n"
        );
        // The behaviour the string is there for, exercised through a real shell.
        let probe = format!("{line}printf %s \"${{CLAUDE_SECURESTORAGE_CONFIG_DIR-UNSET}}\"");
        let run = |env: Option<&str>| {
            let mut c = std::process::Command::new("sh");
            c.arg("-c").arg(&probe).env_remove("CLAUDE_SECURESTORAGE_CONFIG_DIR");
            if let Some(v) = env {
                c.env("CLAUDE_SECURESTORAGE_CONFIG_DIR", v);
            }
            String::from_utf8(c.output().expect("sh").stdout).expect("utf8")
        };
        assert_eq!(run(None), "/tmp/acct/a1", "미설정이면 우리 계정을 심어야 한다");
        assert_eq!(run(Some("")), "", "빈 값 = '기본 저장소' 지시라 존중해야 한다");
        assert_eq!(run(Some("/other")), "/other", "명시 값이 우선이어야 한다");
    }

    /// Paths are single-quoted, so an apostrophe in a directory name would end
    /// the quote and let the rest of the path run as shell words.
    #[test]
    fn claude_account_line_escapes_a_quote_in_the_path() {
        let line = claude_account_export_line(Some(std::path::Path::new("/tmp/geo'no/a1")));
        assert!(line.ends_with("='/tmp/geo'\\''no/a1'\n"), "{line}");
    }

    /// Tables only reach the renderer when `ENABLE_TABLES` is on — without it
    /// pulldown emits the rows as plain text that the block builder drops on the
    /// floor, which is exactly how CLAUDE.md's 3-layer table went missing.
    #[test]
    fn table_parses_into_head_rows_and_alignment() {
        let md = "| 층 | 코드네임 |\n|:---|---:|\n| ① 엔진 | **kasaterm** |\n| ② 작업환경 | `kasaspace` |\n";
        let (blocks, _) = parse_markdown(md);
        let table = blocks
            .iter()
            .find_map(|b| match b {
                MdBlock::Table { head, rows, align } => Some((head, rows, align)),
                _ => None,
            })
            .expect("표가 블록으로 나오지 않았다");
        let (head, rows, align) = table;
        assert_eq!(head.len(), 2);
        assert_eq!(head[0][0].text, "층");
        assert_eq!(align, &vec![MdAlign::Left, MdAlign::Right]);
        // A trailing empty row would render as a phantom band under the table.
        assert_eq!(rows.len(), 2, "본문 행 개수");
        assert!(rows.iter().all(|r| r.len() == 2), "빈 행이 섞였다: {rows:?}");
        assert!(rows[0][1][0].bold, "셀 안 인라인 스타일 보존");
        assert!(rows[1][1][0].code, "셀 안 인라인 코드 보존");
    }

    /// 중첩 리스트의 부모 항목은 자식 리스트가 열릴 때 나가야 한다. TagEnd::Item
    /// 까지 미루면 ① 자식이 먼저 블록에 들어가 화면에서 부모 위로 올라오고
    /// ② 자식의 Tag::Item 이 공유 spans 를 비워 부모 텍스트가 사라졌다.
    #[test]
    fn nested_list_keeps_parent_text_and_order() {
        let (blocks, _) = parse_markdown("- outer\n  - inner\n");
        let items: Vec<(u8, String)> = blocks
            .iter()
            .filter_map(|b| match b {
                MdBlock::ListItem { depth, spans, .. } => {
                    Some((*depth, spans.iter().map(|s| s.text.as_str()).collect()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            items,
            vec![(0, "outer".to_string()), (1, "inner".to_string())],
            "부모가 먼저, 텍스트를 지닌 채로 나와야 한다"
        );
    }

    /// `block_lines` 는 블록과 개수가 같고 **오름차순**이어야 한다 — Raw↔Render
    /// 토글이 이진 탐색(`partition_point`)으로 줄↔블록을 짝지으므로, 순서가
    /// 뒤집히면 엉뚱한 위치로 점프한다.
    #[test]
    fn block_lines_align_with_blocks_and_ascend() {
        let md = "# 제목\n\n문단 하나.\n\n- outer\n  - inner\n- 둘째\n\n\
                  > 인용\n\n```rust\nfn main() {}\n```\n\n\
                  | a | b |\n|---|---|\n| 1 | 2 |\n\n---\n\n마지막 문단.\n";
        let (blocks, lines) = parse_markdown(md);
        assert_eq!(blocks.len(), lines.len(), "두 벡터의 길이가 어긋났다");
        assert!(lines.windows(2).all(|w| w[0] <= w[1]), "줄 번호가 역행한다: {lines:?}");
        assert_eq!(lines[0], 0, "첫 블록은 0줄");
        assert_eq!(*lines.last().unwrap(), 20, "마지막 문단의 줄");
    }

    #[test]
    fn claude_wrapper_is_valid_sh_without_auto_teammate_triple() {
        // 실제 생성물(팀모드 블록 포함)이 POSIX sh 로 파싱되는지 — 문자열 조립이라
        // 이스케이프 하나로 전체 pane claude 부팅이 깨질 수 있는 지점의 안전망.
        let dir = std::env::temp_dir().join(format!("kt-shim-syntax-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        install_claude_hook_shim(&dir);
        let wrapper = dir.join("claude");
        let Ok(body) = std::fs::read_to_string(&wrapper) else {
            // collab-hooks 미해석 환경(번들 밖 CI)이면 생성 자체가 스킵된다.
            return;
        };
        let ok = std::process::Command::new("sh")
            .arg("-n")
            .arg(&wrapper)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "generated claude wrapper failed sh -n");
        // 자동 teammate 트리플은 제거됐다(거노 2026-07-24) — 부활하면 재시작 복원
        // 세션의 SendMessage 조용한 유실 클래스가 돌아온다. 팀 채널은 스폰 다이얼로그/
        // 사용자 명시 --agent-* 만. wrapper 에 자동 트리플 흔적이 없어야 한다.
        assert!(!body.contains("--agent-id"), "auto teammate triple resurfaced in shim");
        assert!(!body.contains("/teamname"), "auto team computation resurfaced in shim");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn count_claude_panes_walks_nested_splits_and_windows() {
        // Mirrors save_session_state's schema: nested split leaves + a second
        // window, mixed was_claude. Only claude leaves count; a null leaf
        // (unresolved pane at save) and a plain shell are ignored.
        let leaf = |claude: bool| {
            serde_json::json!({ "leaf": {
                "cwd": "/repo",
                "was_claude": claude,
                "session_id": if claude { Some("abcd-1234") } else { None },
                "scrollback": ["$ claude", "hello"],
            }})
        };
        let state = serde_json::json!({
            "active_session": 0,
            "sessions": [{
                "active_window": 0,
                "windows": [
                    // Window 0: split( claude , split( shell , claude ) ) = 2 claude
                    { "split": {
                        "dir": "h", "ratio": 0.5,
                        "a": leaf(true),
                        "b": { "split": {
                            "dir": "v", "ratio": 0.4,
                            "a": leaf(false),
                            "b": leaf(true),
                        }},
                    }},
                    // Window 1: a lone claude leaf + a null leaf (dropped)
                    { "split": {
                        "dir": "h", "ratio": 0.5,
                        "a": leaf(true),
                        "b": { "leaf": serde_json::Value::Null },
                    }},
                ],
            }],
        });
        assert_eq!(App::count_claude_panes(&state), 3);
        // Degenerate inputs never panic and count zero.
        assert_eq!(App::count_claude_panes(&serde_json::json!({})), 0);
        assert_eq!(
            App::count_claude_panes(&serde_json::json!({ "sessions": [] })),
            0
        );
        // 캐릭터는 claude 증거가 아니다 — assign_character_env 가 spawn 때 모든
        // pane 에 배정하므로, 이걸 세면 순수 셸 3개짜리 창이 "claude 세션 3개"로
        // 표시된다(실제 발생). 이 단언이 그 회귀를 막는다.
        let char_only = serde_json::json!({ "sessions": [{ "windows": [{
            "leaf": { "cwd": "/repo", "was_claude": false, "session_id": null, "character": "아루" }
        }]}]});
        assert_eq!(App::count_claude_panes(&char_only), 0, "캐릭터만으론 claude 아님");
        // was_claude 감지 실패(저장 순간 claude 가 포그라운드가 아님) 보정은
        // session_id 로 한다 — claude 가 실제로 세션을 바인딩했을 때만 붙는다.
        let sid_only = serde_json::json!({ "sessions": [{ "windows": [{
            "leaf": { "cwd": "/repo", "was_claude": false, "session_id": "abcd-1234" }
        }]}]});
        assert_eq!(App::count_claude_panes(&sid_only), 1, "바인딩된 세션은 claude");
        // 프롬프트를 띄울 기준은 전체 pane 수 — claude 가 0이어도 레이아웃과
        // 스크롤백은 되살릴 값이 있다.
        assert_eq!(App::count_panes(&state), 5, "null leaf 포함 전체 leaf");
        assert_eq!(App::count_panes(&char_only), 1);
        assert_eq!(App::count_panes(&serde_json::json!({})), 0);
    }

    #[test]
    fn strip_activity_prefix_removes_claude_glyphs() {
        // claude OSC 제목 "✳ 요약" → "요약"; 별표류·∗·＊·* 접두 + 공백 제거.
        assert_eq!(strip_activity_prefix("✳ 학생 프사 개선"), "학생 프사 개선");
        assert_eq!(strip_activity_prefix("✻  Brewed for 5s"), "Brewed for 5s");
        assert_eq!(strip_activity_prefix("* build"), "build");
        assert_eq!(strip_activity_prefix("＊작업"), "작업");
        // 브라유 스피너(U+2800 블록) 접두 — 연속 run 도 한 번에.
        assert_eq!(strip_activity_prefix("⠂ 세션 요약 디버깅"), "세션 요약 디버깅");
        assert_eq!(strip_activity_prefix("⠐⠑ 이름"), "이름");
        // 별표로 시작 안 하면 원문 그대로(rename 사용자 값 보호).
        assert_eq!(strip_activity_prefix("학생 프사 개선"), "학생 프사 개선");
        assert_eq!(strip_activity_prefix("main.rs · vim"), "main.rs · vim");
    }

    #[test]
    fn resolve_collab_hooks_dir_prefers_bundle_over_env() {
        // Build a throwaway .app-shaped tree + a separate env-pointed dir so the
        // priority is proven on real filesystem state (is_dir checks).
        let base = std::env::temp_dir().join(format!("kt-hooks-prio-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let exe = base.join("Bundle.app/Contents/MacOS/kasaterm");
        let bundle_res = base.join("Bundle.app/Contents/Resources/collab-hooks");
        let env_dir = base.join("repo/collab-hooks");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, "x").unwrap();
        std::fs::create_dir_all(&bundle_res).unwrap();
        std::fs::create_dir_all(&env_dir).unwrap();
        let env_str = env_dir.to_str().unwrap();

        // 1. In a bundle, Resources WINS over the env override (the leak guard).
        assert_eq!(
            resolve_collab_hooks_dir(Some(&exe), Some(env_str)).as_deref(),
            Some(bundle_res.as_path()),
            "bundle Resources must beat a leaked KASATERM_COLLAB_HOOKS_DIR",
        );

        // 1b. Windows MSI 모양(bin\kasaterm.exe + bin\collab-hooks\) — exe 옆
        //     번들이 env 를 이긴다(Resources 와 같은 leak 가드).
        let msi_exe = base.join("bin/kasaterm");
        let msi_adj = base.join("bin/collab-hooks");
        std::fs::create_dir_all(&msi_adj).unwrap();
        std::fs::write(&msi_exe, "x").unwrap();
        assert_eq!(
            resolve_collab_hooks_dir(Some(&msi_exe), Some(env_str)).as_deref(),
            Some(msi_adj.as_path()),
            "exe-adjacent collab-hooks must beat the env override",
        );

        // 2. No bundle Resources (dev exe under target/) → env override applies.
        let dev_exe = base.join("target/debug/kasaterm");
        std::fs::create_dir_all(dev_exe.parent().unwrap()).unwrap();
        std::fs::write(&dev_exe, "x").unwrap();
        assert_eq!(
            resolve_collab_hooks_dir(Some(&dev_exe), Some(env_str)).as_deref(),
            Some(env_dir.as_path()),
            "without bundle Resources, the env override should win",
        );

        // 3. Neither bundle nor env → repo dev fallback (CARGO_MANIFEST_DIR,
        //    which really exists for this crate).
        let dev_fallback = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("collab-hooks");
        assert_eq!(
            resolve_collab_hooks_dir(Some(&dev_exe), None).as_deref(),
            Some(dev_fallback.as_path()),
            "with no bundle and no env, fall back to the repo source",
        );

        // A bogus env dir that doesn't exist is ignored (falls through to dev).
        assert_eq!(
            resolve_collab_hooks_dir(Some(&dev_exe), Some("/nonexistent/kt/hooks")).as_deref(),
            Some(dev_fallback.as_path()),
            "a non-existent env dir must not be returned",
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn cleanup_collab_markers_removes_pane_files() {
        // %987 keeps the test clear of any real pane's markers in /tmp.
        let bound = std::path::PathBuf::from("/tmp/kasaterm-bound-_987");
        let room = std::path::PathBuf::from("/tmp/kasaterm-collab/test-marker-cleanup");
        std::fs::create_dir_all(&room).unwrap();
        let character = room.join("character-987");
        let nudged = room.join("god-nudged-%987");
        let other = room.join("character-988");
        for f in [&bound, &character, &nudged, &other] {
            std::fs::write(f, "x").unwrap();
        }
        // cwd=None → 폴백(전체 방 순회). 같은 번호 마커는 지우고 다른 번호는 보존.
        App::cleanup_collab_markers("%987", None);
        assert!(!bound.exists(), "bound marker should be deleted");
        assert!(!character.exists(), "character marker should be deleted");
        assert!(!nudged.exists(), "god-nudged marker should be deleted");
        assert!(other.exists(), "another pane's marker must survive");
        std::fs::remove_dir_all(&room).unwrap();
    }

    #[test]
    fn cleanup_collab_markers_spares_other_rooms() {
        // 같은 pane 번호라도 *다른 방*의 마커는 살아남아야 한다(거노: 캐릭터 유실 근본).
        let mine = std::path::PathBuf::from("/tmp/kasaterm-collab/-tmp-room-mine");
        let other = std::path::PathBuf::from("/tmp/kasaterm-collab/-tmp-room-other");
        std::fs::create_dir_all(&mine).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let my_char = mine.join("character-1");
        let other_char = other.join("character-1");
        std::fs::write(&my_char, "미도리").unwrap();
        std::fs::write(&other_char, "아리스").unwrap();
        App::cleanup_collab_markers("%1", Some(std::path::Path::new("/tmp/room/mine")));
        assert!(!my_char.exists(), "내 방의 닫힌 pane 마커는 삭제");
        assert!(other_char.exists(), "다른 방의 같은 번호 마커는 보존");
        std::fs::remove_dir_all(&mine).unwrap();
        std::fs::remove_dir_all(&other).unwrap();
    }

    #[test]
    fn wheel_sub_cell_ticks_accumulate() {
        let mut accum = 0.0;
        let mut last = Instant::now();
        let t0 = ms(last, 100);
        assert_eq!(wheel_step(&mut accum, 0.3, &mut last, ms(t0, 0)), None);
        assert_eq!(wheel_step(&mut accum, 0.3, &mut last, ms(t0, 20)), None);
        assert_eq!(wheel_step(&mut accum, 0.3, &mut last, ms(t0, 40)), None);
        assert_eq!(wheel_step(&mut accum, 0.3, &mut last, ms(t0, 60)), Some(1));
    }

    #[test]
    fn wheel_direction_flip_drops_residual() {
        let mut accum = 0.0;
        let mut last = Instant::now();
        let t0 = ms(last, 100);
        wheel_step(&mut accum, 0.6, &mut last, ms(t0, 0));
        let out = wheel_step(&mut accum, -1.0, &mut last, ms(t0, 50));
        assert_eq!(out, Some(-1));
    }

    #[test]
    fn selection_extract_single_row() {
        let mut row = vec![GridCell::blank(); 10];
        for (i, c) in "hello".chars().enumerate() {
            row[i] = GridCell {
                ch: c,
                ..GridCell::blank()
            };
        }
        let sel = Selection { anchor: (0, 0), end: (4, 0) };
        let s = extract_selection(&[row], sel);
        assert_eq!(s, "hello");
    }

    #[test]
    fn selection_normalise_reverses_when_needed() {
        let sel = Selection { anchor: (5, 2), end: (1, 0) };
        let (a, b) = normalise(sel);
        assert_eq!(a, (1, 0));
        assert_eq!(b, (5, 2));
    }

    /// Build a grid row from a string, expanding each full-width (CJK)
    /// char into a glyph cell + a blank spacer cell — exactly how the PTY
    /// backend stores them. Pads to `width` with blanks.
    fn wide_row(s: &str, width: usize) -> Vec<GridCell> {
        let mut row = Vec::new();
        for ch in s.chars() {
            row.push(GridCell { ch, ..GridCell::blank() });
            if gpu::is_wide_char(ch) {
                row.push(GridCell { ch: ' ', ..GridCell::blank() });
            }
        }
        while row.len() < width {
            row.push(GridCell::blank());
        }
        row
    }

    #[test]
    fn copy_drops_wide_char_spacer() {
        // Selection copy: "한글" must not become "한 글".
        let row = wide_row("한글", 12);
        let sel = Selection { anchor: (0, 0), end: (3, 0) };
        assert_eq!(extract_selection(&[row], sel), "한글");

        // Mixed ASCII + CJK keeps real spaces, drops only spacers.
        let row = wide_row("a한 b", 12);
        let sel = Selection { anchor: (0, 0), end: (5, 0) };
        assert_eq!(extract_selection(&[row], sel), "a한 b");
    }

    #[test]
    fn tear_off_only_fires_outside_the_window() {
        // Phase 3: 파일 탭 tear-off 트리거는 커서가 창 콘텐츠 사각형 밖일 때만.
        // 창 안(패널 body·탭 스트립 포함) 어디에 놓든 false → 기존 split/dock 경로가 처리.
        let (w, h) = (1200.0_f32, 800.0_f32);
        assert!(!App::drag_left_window(0.0, 0.0, w, h)); // 좌상단 모서리 = 안
        assert!(!App::drag_left_window(600.0, 400.0, w, h)); // 정중앙 = 안
        assert!(!App::drag_left_window(w, h, w, h)); // 우하단 모서리(경계) = 안
        // 네 방향 밖 — 각각 tear-off.
        assert!(App::drag_left_window(-1.0, 400.0, w, h)); // 왼쪽 밖
        assert!(App::drag_left_window(w + 1.0, 400.0, w, h)); // 오른쪽 밖
        assert!(App::drag_left_window(600.0, -1.0, w, h)); // 위쪽 밖(탭바 위로 뜯음)
        assert!(App::drag_left_window(600.0, h + 1.0, w, h)); // 아래쪽 밖
    }
}
