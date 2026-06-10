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
mod stream;
mod theme;
mod transcript;
mod chrome;
mod testkit;
mod session;
mod layout;
mod markdown;
mod input;
mod settings;
mod links;

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
/// Code-block copy button (overlay) size in logical px. Small chip that
/// sits at a detected block's top-right corner.
const COPY_BTN_W: f32 = 26.0;
const COPY_BTN_H: f32 = 18.0;
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
// 워커 색 — pane id 숫자(%5 → 5) 기반. god-elect.sh worker_color 와 같은 팔레트·
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
    const isGod = !!p.is_god;
    d.style.borderLeftColor = isGod ? '#FFD400' : workerColor(p.surface_id);
    const crown = isGod
      ? `<svg width="13" height="11" viewBox="0 0 24 20" style="margin-right:5px;vertical-align:-1px"><path d="M2 6l4 4 6-8 6 8 4-4v11H2z" fill="${'#FFD400'}" stroke="${'#1a1d23'}" stroke-width="1.2"/></svg>`
      : "";
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
      `<div class="row1">${crown}<span class="sid">${esc(p.surface_id)}</span>` +
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

/// A detected "code block": a run of consecutive rows that share one
/// non-page background color, the way Claude Code paints fenced code /
/// command boxes. `(start_row, end_row_inclusive, left_col, right_col)`.
type CodeBlock = (usize, usize, usize, usize);

/// Scan a pane's composed grid for code blocks. The shell / TUI gives us
/// no markdown metadata (fences arrive as styled cells), so we lean on the
/// one signal that survives: Claude Code renders code/command blocks with
/// a solid background box. We find the pane's dominant ("page") bg, then
/// any contiguous rows carrying a *different* uniform bg across a wide
/// enough span are a block. Purely heuristic — when it doesn't match
/// (no bg box) it just returns nothing, so the caller shows no button.
fn detect_code_blocks(rows: &[Vec<GridCell>]) -> Vec<CodeBlock> {
    use kasa_bridge::screen::Color;
    // Minimum horizontal run of one bg color for a row to count as "inside
    // a box". Filters single-token inline highlights and 1-2 cell artifacts.
    const MIN_RUN: usize = 10;
    // Dominant bg across the grid = the page background. Treated like
    // Default so a TUI that paints every cell doesn't read as one block.
    let mut counts: Vec<(Color, usize)> = Vec::new();
    for row in rows {
        for cell in row {
            match counts.iter_mut().find(|(c, _)| *c == cell.bg) {
                Some((_, n)) => *n += 1,
                None => counts.push((cell.bg.clone(), 1)),
            }
        }
    }
    let page_bg = counts
        .into_iter()
        .max_by_key(|(_, n)| *n)
        .map(|(c, _)| c)
        .unwrap_or(Color::Default);
    // Longest contiguous run of a single bg that is neither Default nor the
    // page bg. Returns (bg, left, right) when it clears MIN_RUN.
    let row_box = |row: &[GridCell]| -> Option<(Color, usize, usize)> {
        let mut best: Option<(Color, usize, usize)> = None;
        let mut i = 0;
        while i < row.len() {
            let bg = row[i].bg.clone();
            if bg == Color::Default || bg == page_bg {
                i += 1;
                continue;
            }
            let start = i;
            while i < row.len() && row[i].bg == bg {
                i += 1;
            }
            let len = i - start;
            if len >= MIN_RUN
                && best.as_ref().map(|(_, bs, be)| be - bs + 1 < len).unwrap_or(true)
            {
                best = Some((bg, start, i - 1));
            }
        }
        best
    };
    let mut blocks = Vec::new();
    let mut r = 0;
    while r < rows.len() {
        let Some((bg, l, rr)) = row_box(&rows[r]) else {
            r += 1;
            continue;
        };
        let start = r;
        let (mut left, mut right) = (l, rr);
        r += 1;
        while r < rows.len() {
            match row_box(&rows[r]) {
                Some((b2, l2, r2)) if b2 == bg => {
                    left = left.min(l2);
                    right = right.max(r2);
                    r += 1;
                }
                _ => break,
            }
        }
        blocks.push((start, r - 1, left, right));
    }
    blocks
}

/// Extract the text inside a detected code block: columns `left..=right`
/// of rows `start..=end`, trailing spaces trimmed per row, joined with
/// `\n`, blank edge lines stripped. Mirrors `extract_selection`'s trims.
fn extract_block(rows: &[Vec<GridCell>], block: CodeBlock) -> String {
    let (start, end, left, right) = block;
    let mut lines: Vec<String> = Vec::new();
    for row in rows.iter().take(end + 1).skip(start) {
        let mut line = String::new();
        append_cells_text(row.iter().take(right + 1).skip(left), &mut line);
        while line.ends_with(' ') {
            line.pop();
        }
        lines.push(line);
    }
    while lines.first().map(|l| l.trim().is_empty()).unwrap_or(false) {
        lines.remove(0);
    }
    while lines.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
        lines.pop();
    }
    // Strip the common leading whitespace the bg box padded each line with,
    // so a copied command pastes ready-to-run. Leading pad is ASCII spaces,
    // so byte slicing is safe; blank lines don't constrain the amount.
    let indent = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    if indent > 0 {
        for l in &mut lines {
            if l.len() >= indent {
                *l = l[indent..].to_string();
            }
        }
    }
    lines.join("\n")
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
    /// Quit the app (window red-light / Cmd+W on the last pane).
    Window,
}

/// A pending "something's running — close anyway?" confirmation. `proc` is the
/// foreground process name that triggered it (for the message); `action` is
/// what the 닫기 button runs.
#[derive(Clone)]
struct ConfirmClose {
    proc: String,
    action: PendingClose,
}

/// The two buttons in the confirm-close modal.
#[derive(Clone, Copy, PartialEq)]
enum ConfirmBtn {
    Cancel,
    Close,
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
    edit_lines: Vec<String>,
    /// Edit cursor: line index + column in chars.
    cur_line: usize,
    cur_col: usize,
    /// Scroll offset in logical px (both Render and Raw).
    scroll: usize,
    /// Raw-mode horizontal scroll in logical px. Long code lines (checksums,
    /// URLs) overflow the pane — this pans the text under a fixed line-number
    /// gutter. 0 = flush left. Render mode ignores it (markdown wraps).
    h_scroll: f32,
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
    /// Frame-dirty flag; cleared after the next render. When every pane is
    /// clean and no chrome anim is pending, the render loop skips the GPU pass.
    dirty: bool,
}

impl Default for PaneState {
    fn default() -> Self {
        Self { tabs: vec![PaneTab::default()], active_tab: 0, color: None, dirty: false }
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

/// A decoded image bound to a pane. `rgba` is tightly-packed RGBA8
/// (`w * h * 4` bytes), uploaded once into a wgpu texture keyed by pane
/// id the first frame the pane is drawn.
struct ImagePane {
    rgba: Vec<u8>,
    w: u32,
    h: u32,
}

/// Largest texture edge we upload. Comfortably under every backend's
/// max-texture-dimension and keeps a huge screenshot from eating VRAM;
/// the pane fits the image anyway so the downscale is invisible.
const MAX_IMAGE_EDGE: u32 = 4096;

/// Decode an image file to RGBA8, downscaling so neither edge exceeds
/// `MAX_IMAGE_EDGE`. Returns an error the `imgopen` caller surfaces on a
/// path that isn't a decodable image.
fn decode_image_rgba(path: &std::path::Path) -> anyhow::Result<ImagePane> {
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
        rgba: rgba.into_raw(),
        w,
        h,
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
fn char_byte(s: &str, col: usize) -> usize {
    s.char_indices().nth(col).map(|(i, _)| i).unwrap_or(s.len())
}

/// One styled inline run inside a markdown block. The renderer picks the
/// font weight/slant and (for `code`) a mono face + chip background from
/// these flags.
#[derive(Clone)]
struct MdSpan {
    text: String,
    bold: bool,
    italic: bool,
    code: bool,
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
    ListItem { depth: u8, marker: String, spans: Vec<MdSpan> },
    Quote { spans: Vec<MdSpan> },
    Rule,
    /// `![alt](path)` — rendered as a wgpu texture inline (same path as the
    /// image pane). `key` is the texture cache id, `w`/`h` the decoded pixel
    /// size for aspect layout; all three are filled in after parse when the
    /// image is decoded (0/empty until then). `path` is kept for alt fallback.
    Image { path: String, alt: String, key: String, w: u32, h: u32 },
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
    blocks: Vec<MdBlock>,
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
fn parse_markdown(text: &str) -> Vec<MdBlock> {
    use pulldown_cmark::{Event, Parser, Tag, TagEnd};
    let mut blocks: Vec<MdBlock> = Vec::new();
    let mut spans: Vec<MdSpan> = Vec::new();
    let mut bold = 0i32;
    let mut italic = 0i32;
    let mut heading: Option<u8> = None;
    let mut in_code = false;
    let mut code_buf = String::new();
    let mut code_lang = String::new();
    // Each open list level: Some(next_number) for ordered, None for bullet.
    let mut list_stack: Vec<Option<u64>> = Vec::new();
    let mut in_item = false;
    let mut item_marker = String::new();
    let mut in_quote = false;
    let mut in_image = false;
    let mut img_url = String::new();
    let mut img_alt = String::new();
    let mut link_url: Option<String> = None;

    let push_span =
        |spans: &mut Vec<MdSpan>, t: &str, b: bool, i: bool, c: bool, link: Option<String>| {
            if !t.is_empty() {
                spans.push(MdSpan { text: t.to_string(), bold: b, italic: i, code: c, link });
            }
        };

    for ev in Parser::new(text) {
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
                Tag::List(start) => list_stack.push(start),
                Tag::Item => {
                    in_item = true;
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
                Tag::BlockQuote(_) => {
                    in_quote = true;
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
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Heading(_) => blocks.push(MdBlock::Heading {
                    level: heading.take().unwrap_or(1),
                    spans: std::mem::take(&mut spans),
                }),
                TagEnd::Paragraph => {
                    // Skip empty paragraphs (e.g. a paragraph that held only an
                    // image, which was emitted as its own Image block).
                    if in_quote {
                        if !spans.is_empty() {
                            blocks.push(MdBlock::Quote { spans: std::mem::take(&mut spans) });
                        }
                    } else if !in_item && !spans.is_empty() {
                        blocks.push(MdBlock::Para { spans: std::mem::take(&mut spans) });
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
                    let depth = list_stack.len().saturating_sub(1) as u8;
                    blocks.push(MdBlock::ListItem {
                        depth,
                        marker: std::mem::take(&mut item_marker),
                        spans: std::mem::take(&mut spans),
                    });
                    in_item = false;
                }
                TagEnd::Emphasis => italic -= 1,
                TagEnd::Strong => bold -= 1,
                TagEnd::Link => link_url = None,
                TagEnd::BlockQuote(_) => in_quote = false,
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
                _ => {}
            },
            Event::Text(t) => {
                if in_image {
                    img_alt.push_str(&t);
                } else if in_code {
                    code_buf.push_str(&t);
                } else {
                    push_span(&mut spans, &t, bold > 0, italic > 0, false, link_url.clone());
                }
            }
            Event::Code(t) => {
                push_span(&mut spans, &t, bold > 0, italic > 0, true, link_url.clone())
            }
            Event::SoftBreak | Event::HardBreak => {
                if in_code {
                    code_buf.push('\n');
                } else {
                    push_span(&mut spans, " ", bold > 0, italic > 0, false, link_url.clone());
                }
            }
            Event::Rule => blocks.push(MdBlock::Rule),
            _ => {}
        }
    }
    blocks
}

/// Parse + decode a markdown document: parse blocks, then decode each inline
/// image (resolving relative paths against the md file's dir, skipping remote
/// URLs) under `key_prefix`-scoped texture keys. Shared by initial open and
/// post-edit re-parse.
fn build_markdown_doc(key_prefix: &str, p: &std::path::Path, text: &str) -> MarkdownDoc {
    let mut blocks = parse_markdown(text);
    let md_dir = p.parent().map(|d| d.to_path_buf());
    let mut images: Vec<MdDocImage> = Vec::new();
    for (idx, block) in blocks.iter_mut().enumerate() {
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
                let k = format!("{key_prefix}#img{idx}");
                *key = k.clone();
                *w = img.w;
                *h = img.h;
                images.push(MdDocImage { key: k, rgba: img.rgba, w: img.w, h: img.h });
            }
        }
    }
    MarkdownDoc {
        blocks,
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
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            panes: HashMap::new(),
            layout: None,
            active_pane: None,
            pid_to_pane: HashMap::new(),
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
    /// `surface.close` delegated from the socket thread → `close_pane`. Local
    /// PTY mode only; the old tmux/daemon backend left this unsupported.
    SocketClose(String),
    /// Show/hide the main terminal window, delegated from the socket thread
    /// (`POST /terminal-reveal` — the arona classroom's red-pill button).
    /// `(show, focus_pane)`: a reveal may also focus a specific pane so the
    /// classroom can jump the user to a character's seat.
    SocketRevealTerminal(bool, Option<String>),
    /// Close the arona classroom window (`POST /arona-close` — the
    /// ModePicker's "터미널로" choice). No-op when it isn't open.
    SocketAronaClose,
    /// `surface.rename` / `surface.set_color` delegated from the socket thread.
    /// Pane header title / accent band live in `ws.panes` which only the GUI
    /// thread may touch, so the backend routes them here. `(surface_id, title)`
    /// / `(surface_id, rgba)`.
    SocketRename(String, String),
    /// `window.rename` delegated from the socket thread. `(surface_id, title)`:
    /// rename the window/session the pane belongs to (sidebar label), not the
    /// pane header. Used by the god marker.
    SocketRenameWindow(String, String),
    SocketColor(String, [u8; 4]),
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

/// Action buttons at the foot of the git column. `StageAll` runs `git add -A`;
/// `Commit` hands the commit to the active claude pane; `Pull`/`Push` sync the
/// current branch with its upstream. All shell out through `kasa_mcp::git` on a
/// worker thread so the UI never blocks.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GitColBtn {
    StageAll,
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
}

/// The two free-text fields in the settings form. Tracks which one (if any)
/// has keyboard focus so keystrokes route to its buffer.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsInput {
    CwdPath,
    Shell,
}

/// Clickable targets painted into the settings screen, collected each frame for
/// hit-testing. String-carrying variants (shell presets) keep this `Clone`.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum SettingsAction {
    Category(SettingsCat),
    CwdMode(&'static str),
    FocusCwdPath,
    ToggleFileTree,
    ShellPreset(String),
    FocusShell,
    ThemeMode(&'static str),
    Accent(String),
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

struct App {
    window: Option<Arc<Window>>,
    /// Set when `KASATERM_RENDERER=gpu`. Mutually exclusive with
    /// `sugarloaf` — both own a wgpu Surface, only one can present.
    gpu: Option<gpu::GpuRenderer>,
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
    /// Headless verification: clean-exit (runs `exiting` → save_session_state)
    /// at this instant when KASATERM_AUTOQUIT_MS is set. None disables it.
    autoquit_at: Option<std::time::Instant>,
    /// Pending GPU self-capture `(deadline, png path)` from KASATERM_AUTOCAPTURE_MS.
    /// `about_to_wait` arms `gpu.capture_next` once the deadline passes so the
    /// next render reads the frame back to a PNG — no screen-record permission.
    pending_capture: Option<(std::time::Instant, String)>,
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
    /// First-run onboarding check deadline — set at boot, fires once after
    /// the shell settles; opens the arona ModePicker when this room has no
    /// collab-mode marker yet (KASATERM_NO_ONBOARD opts out).
    onboard_check_at: Option<Instant>,
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
    /// (pane_id, close_rect) for every visible pane header. Populated
    /// by `render_frame` and consumed by the MouseInput handler so a
    /// click on the × button closes that pane.
    pane_header_rects: Vec<(String, (f32, f32, f32, f32))>,
    /// (block text, copy-button rect) for every code block detected in the
    /// visible panes this frame. Logical px. Populated by the render path,
    /// consumed by the MouseInput handler so a click on the button copies
    /// the block. Cleared+rebuilt every paint.
    copy_btn_rects: Vec<(String, (f32, f32, f32, f32))>,
    /// Rendered markdown content height (logical px) per pane id, published by
    /// the renderer each frame. The scroll handler clamps scroll_offset to
    /// (content_h - visible_h) so a markdown pane can't over-scroll.
    md_content_h: HashMap<String, f32>,
    /// Raw-editor body box (logical px) per pane id, published by the renderer
    /// each frame. A click in this box hit-tests to a caret position so the
    /// mouse can place the edit cursor (see `md_click_caret`).
    md_body_rects: HashMap<String, (f32, f32, f32, f32)>,
    /// In-pane tab hit rects: (pane id, tab index, logical rect). Click
    /// switches that pane's active_tab. Rebuilt each header paint.
    pane_tab_rects: Vec<(String, usize, (f32, f32, f32, f32))>,
    /// Per-tab × close hit rects: (pane id, tab index, logical rect).
    pane_tab_close_rects: Vec<(String, usize, (f32, f32, f32, f32))>,
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
    /// (window index, rect) for every window tab in the left sidebar.
    /// Populated by the render path, consumed by the MouseInput handler so
    /// a click switches windows. Logical px.
    window_tab_rects: Vec<(usize, (f32, f32, f32, f32))>,
    /// (window index, close-× rect) for each window tab. Only present when
    /// there's more than one window (the last window can't be closed).
    window_tab_close_rects: Vec<(usize, (f32, f32, f32, f32))>,
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
    /// the god marker needs the session to read "● god" regardless of which
    /// pane is the representative — this map wins over the derived name. Not
    /// persisted: god-elect.sh re-applies it every turn, so a restart that
    /// re-elects a god re-marks the window.
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
    /// In-flight image-pane pan drag: `(pane_id, start_cursor_px, base_pan)`.
    /// `Some` while dragging a zoomed image's body; CursorMoved updates the
    /// active tab's `image_pan_*` from `base_pan + (cursor - start)`.
    image_pan_drag: Option<(String, (f32, f32), (f32, f32))>,
    /// In-flight file-tree → terminal path drag. `Some` while a tree row is
    /// held; releasing over a pane types the path into that shell.
    file_tree_drag: Option<FileTreeDrag>,
    /// Inline "new file / folder" entry. `Some((is_dir, name_buffer))` while
    /// the user is naming a freshly-requested entry; Enter creates it under
    /// the tree root, Esc cancels. Keystrokes route here like the search box.
    file_tree_new: Option<(bool, String)>,
    /// Hit rects for the new-folder / new-file buttons beside the search box,
    /// refreshed each frame.
    file_tree_new_folder_rect: (f32, f32, f32, f32),
    file_tree_new_file_rect: (f32, f32, f32, f32),
    /// Row rect of the inline new-entry naming box (for the I-beam hit-test).
    file_tree_new_row_rect: (f32, f32, f32, f32),
    /// Tree row the user last clicked — the Cmd+Delete target.
    file_tree_selected: Option<std::path::PathBuf>,
    /// Whether the text (I-beam) mouse cursor is currently shown, so we only
    /// flip the OS cursor on the transition in/out of an input box.
    text_cursor_shown: bool,
    /// Active "close while a process is running?" modal. While `Some`, the
    /// dialog is painted over everything and swallows input until the user
    /// picks 취소/닫기.
    confirm_close: Option<ConfirmClose>,
    /// Confirm-modal button hit rects, refreshed each frame: `(btn, rect)`.
    confirm_btn_rects: Vec<(ConfirmBtn, (f32, f32, f32, f32))>,
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
    /// Window indices with an *unseen* notification (a pane finished / needs
    /// attention while that window was in the background). The sidebar tab
    /// pulses until the user switches to that window, which clears the entry —
    /// a persistent "you missed this" cue, unlike the brief `notify_flash`.
    window_alert: std::collections::HashSet<usize>,
    /// Active completion toast (message + start instant) for a sibling pane's
    /// working→idle flip — "✓ %3 완료 · git 패널". Fades like `copy_toast_at`.
    /// Replaced by the newest flip; a brief overlap just shows the latest.
    collab_toast: Option<(String, Instant)>,
    /// The completion toast's logical-px rect while it's visible, so a click
    /// dismisses it. None when the toast isn't drawn. Set by the render path,
    /// consumed by the MouseInput handler.
    collab_toast_rect: Option<(f32, f32, f32, f32)>,
    /// 승인 토스트 모드(munder식 god 승인 카드): Some(pane id)면 `collab_toast`가
    /// 페이드 없이 고정되고 승인/거부 칩이 함께 렌더된다. 칩 클릭이 이 pane 의
    /// PTY 로 응답 키를 보낸다 (`respond_approval`). 프롬프트가 풀리면 해제.
    collab_toast_action: Option<String>,
    /// 승인 토스트의 승인/거부 칩 hit-rect (logical px). 렌더 패스가 쓰고
    /// MouseInput 이 소비 — 일반 토스트 dismiss 보다 먼저 검사해야 한다.
    collab_toast_approve_rect: Option<(f32, f32, f32, f32)>,
    collab_toast_deny_rect: Option<(f32, f32, f32, f32)>,
    /// 승인 프롬프트가 떠 있는 pane → "사용자 직행(god/단독)인가". 그리드 스캔
    /// (`route_approval_prompts`)의 edge-trigger 상태: 새로 뜨면 라우팅 1회,
    /// 풀리면 board waiting 플래그까지 함께 걷는다.
    pane_prompt_wait: HashMap<String, bool>,
    /// 협업 board 의 `waiting` 플래그 — socket `PtyBackend` 와 공유(Arc). hook
    /// (`kasaterm-cli attention`) 경로와 그리드 감지 경로가 같은 맵에 쓰므로
    /// god 이 board 어디서 보든 워커 막힘이 보인다.
    collab_attention: Arc<Mutex<HashMap<String, String>>>,
    /// Unread completion count, for a badge on the collab board entry. Bumped on
    /// each completion toast; the badge render + clear land with the sidebar
    /// work (P3) — the board has no GUI button yet (menu-toggle only), so the
    /// sidebar window list is its natural home.
    #[allow(dead_code)]
    collab_unread: u32,
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
    /// 아로나 자동 시작(P5): god 모드 && characters 있는 방의 첫 pane 에 띄울
    /// claude 명령. start_pty 에서 가드 통과 시 세팅, 셸 prompt-end(OSC133) 감지
    /// 또는 타임아웃 시 1회 주입 후 None. solo·무테마면 애초에 None(무동작).
    pending_autoleader: Option<String>,
    pending_autoleader_at: Option<Instant>,
    pane_cwd_cache: HashMap<String, std::path::PathBuf>,
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
    /// Right-hand git column (the in-window replacement for the old floating
    /// webview git panel). Visibility + live width, an in-flight resize drag,
    /// a scroll offset, and the per-frame file-row + button hit rects — all
    /// mirroring the file-tree column on the left.
    git_col_visible: bool,
    git_col_w_logical: f32,
    git_col_resize: Option<(f32, f32)>,
    git_col_scroll: f32,
    git_col_file_rects: Vec<(bool, String, (f32, f32, f32, f32))>,
    git_col_btn_rects: Vec<(GitColBtn, (f32, f32, f32, f32))>,
    /// Files whose inline unified-diff is expanded in the panel, keyed by
    /// `(staged, path)` so a partially-staged file can expand each side
    /// independently. `git_col_diff_cache` holds the parsed rows, loaded when a
    /// row is expanded and cleared whenever the status snapshot changes.
    git_col_expanded: std::collections::HashSet<(bool, String)>,
    git_col_diff_cache: HashMap<(bool, String), Vec<kasa_mcp::git::DiffLine>>,
    /// cursor-style header chrome: panel close/expand buttons, the split Commit
    /// button + its caret, and the caret's dropdown (Commit / Push / Create PR).
    /// All rebuilt each paint like the other git-column hit rects.
    git_col_close_rect: Option<(f32, f32, f32, f32)>,
    git_col_expand_rect: Option<(f32, f32, f32, f32)>,
    git_commit_btn_rect: Option<(f32, f32, f32, f32)>,
    git_commit_caret_rect: Option<(f32, f32, f32, f32)>,
    git_commit_menu_open: bool,
    git_commit_menu_rects: Vec<(GitCommitAction, (f32, f32, f32, f32))>,
    /// Commit modal (screenshot #5): full-panel overlay with branch, an
    /// include-unstaged toggle, the file list, a message box, and the
    /// Commit / Commit-and-push + Cancel/Confirm actions.
    git_commit_modal_open: bool,
    git_commit_modal_include_unstaged: bool,
    git_commit_modal_rects: Vec<(GitModalBtn, (f32, f32, f32, f32))>,
    /// Per-row stage/unstage button hit rects: `(stage, path, rect)` where
    /// `stage == true` means the + button (git add), `false` the − (unstage).
    /// Rebuilt each paint; only the hovered row's button is present.
    git_col_stage_rects: Vec<(bool, String, (f32, f32, f32, f32))>,
    /// Hovered-row action buttons beside stage: ↩ discard (path, untracked) and
    /// ⤴ open-in-preview (path). Rebuilt each paint like `git_col_stage_rects`.
    git_col_discard_rects: Vec<(String, bool, (f32, f32, f32, f32))>,
    git_col_open_rects: Vec<(String, (f32, f32, f32, f32))>,
    /// VSCode-style commit message input above the action buttons. The buffer
    /// is single-line (commit subject); `cursor` is a char index into it.
    /// `focused` routes keystrokes here (see `forward_key`) instead of the PTY.
    /// `input_rect` is the per-paint hit target for click-to-focus.
    git_commit_msg: String,
    git_commit_cursor: usize,
    git_commit_focused: bool,
    git_commit_input_rect: Option<(f32, f32, f32, f32)>,
    /// `git status` snapshot for the active pane's cwd: the poller writes it
    /// off the main thread, the render reads it. `git_col_cwd` is the cwd the
    /// poller should refresh (render publishes the active pane's cwd into it).
    /// Same pattern as `window_git` / `git_poll_cwds`.
    git_col_data: std::sync::Arc<std::sync::Mutex<GitColView>>,
    /// Label of the in-flight git op (push/pull/commit…) for the panel spinner,
    /// or `None` when idle. Set on the GUI thread when the op starts, cleared by
    /// `UserEvent::GitOpDone` when the worker finishes.
    git_op: Option<&'static str>,
    git_col_cwd: std::sync::Arc<std::sync::Mutex<Option<std::path::PathBuf>>>,
    /// User-pinned repo for the column. `Some` = show this repo regardless of
    /// the focused pane (picked from the path dropdown); `None` = follow the
    /// active pane's cwd. `publish_git_col_cwd` honours it.
    git_col_pinned_cwd: Option<std::path::PathBuf>,
    /// Open dropdowns in the git-column header (path picker / branch switcher).
    /// Only one is meaningfully open at a time. The per-frame hit rects are
    /// rebuilt by the render (like `shell_menu_*`).
    git_path_menu_open: bool,
    git_branch_menu_open: bool,
    /// Header click targets + dropdown item rects, rebuilt each paint. The path
    /// menu's `None` entry is the "자동 추적" toggle; branch items carry the
    /// branch name to check out.
    git_path_hdr_rect: Option<(f32, f32, f32, f32)>,
    git_branch_hdr_rect: Option<(f32, f32, f32, f32)>,
    git_path_menu_rects: Vec<(Option<std::path::PathBuf>, (f32, f32, f32, f32))>,
    git_branch_menu_rects: Vec<(String, (f32, f32, f32, f32))>,
    /// Per-pane status bar (cwd / branch / diff chips at the foot of each pane).
    /// `statusbar_hidden` holds the pane ids whose bar is collapsed — default is
    /// shown, so absence means visible. A hidden pane reserves no footer rows.
    statusbar_hidden: std::collections::HashSet<String>,
    /// Per-frame status-bar hit rects, rebuilt each paint (like the git column's
    /// header rects). `path`/`branch` carry the pane id so a click resolves the
    /// repo; `toggle` is the eye button in the pane header that hides the bar.
    statusbar_path_rects: Vec<(String, (f32, f32, f32, f32))>,
    statusbar_branch_rects: Vec<(String, (f32, f32, f32, f32))>,
    statusbar_toggle_rects: Vec<(String, (f32, f32, f32, f32))>,
    /// diff chip hit rects (pane id → rect). Clicking opens the git column for
    /// that pane's repo.
    statusbar_diff_rects: Vec<(String, (f32, f32, f32, f32))>,
    /// Open status-bar dropdown: `(pane_id, kind)` where kind picks the path
    /// picker or branch switcher. The item rects + their backing data are
    /// rebuilt by the render while the menu is open.
    statusbar_menu: Option<(String, StatusbarMenu)>,
    statusbar_menu_dir_rects: Vec<(std::path::PathBuf, (f32, f32, f32, f32))>,
    statusbar_menu_branch_rects: Vec<(String, (f32, f32, f32, f32))>,
    /// Backing data for the open status-bar dropdown, snapshotted when it opens
    /// (read_dir / git_branches are off the render path). `dirs` is `..` + the
    /// cwd's child directories; `branches` is the repo's local branches.
    statusbar_menu_dirs: Vec<std::path::PathBuf>,
    statusbar_menu_branches: Vec<String>,
    /// Vertical scroll (logical px) of the open status-bar dropdown. The list
    /// caps its visible rows, so a cwd with many subdirs needs the wheel to
    /// reach the rest. Reset to 0 each time a menu opens.
    statusbar_menu_scroll: f32,
    /// Full menu rect (logical px) of the open dropdown, cached each frame so
    /// the wheel handler can tell when the cursor is hovering it.
    statusbar_menu_rect: Option<(f32, f32, f32, f32)>,
    /// Live search query for the open path dropdown — typing while it's open
    /// filters the rows (cursor-style quick-nav). Reset when a menu opens.
    statusbar_menu_search: String,
    /// File-tree column search box: active flag + query. When active a search
    /// field shows atop the tree and the rows are filtered to name matches.
    file_tree_search_active: bool,
    file_tree_search_query: String,
    /// Search-box rect (logical px), cached each frame so the mouse handler can
    /// tell when a click lands on it (→ focus the file-tree search).
    file_tree_search_rect: (f32, f32, f32, f32),
    /// Single-pane Cmd+W in daemon mode sets this to close the GUI window
    /// (ghostty-style) on the next about_to_wait, instead of killing the pane.
    /// The daemon keeps the session alive so relaunch restores it.
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
    /// Sidebar file-tree state. Root = the active pane's cwd (recomputed when
    /// it changes — follows pane switch + `cd`). `nodes` is the flattened
    /// expanded tree, rebuilt only on root/expand change (no per-frame
    /// read_dir). `rects`/`hover` mirror the window-tab hit-test pattern.
    file_tree_root: Option<std::path::PathBuf>,
    file_tree_expanded: std::collections::HashSet<std::path::PathBuf>,
    file_tree_nodes: Vec<FileNode>,
    /// Live file-tree refresh. A background thread polls the dirs in
    /// `file_tree_watch` (root + expanded folders) ~800ms apart and, on any
    /// add/remove/rename/mtime change, sets `file_tree_fs_dirty` + wakes the
    /// loop. `refresh_file_tree` rebuilds when the flag is set. Off-GUI-thread
    /// so the event-driven loop stays idle until the FS actually changes.
    file_tree_fs_dirty: std::sync::Arc<std::sync::atomic::AtomicBool>,
    file_tree_watch: std::sync::Arc<std::sync::Mutex<Vec<std::path::PathBuf>>>,
    file_tree_watch_started: bool,
    /// `git check-ignore` runs off-GUI-thread: spawning git from the unsigned
    /// kasaterm.exe triggers a Defender full-scan (~5s/call on Windows) that
    /// would freeze the toggle if run inline. The worker reads a (root, paths)
    /// request, fills `file_tree_ignored`, and wakes the loop so the next
    /// rebuild dims gitignored rows. Until it lands, rows show un-dimmed.
    file_tree_ignored: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    git_ignore_req: std::sync::Arc<std::sync::Mutex<Option<(std::path::PathBuf, Vec<String>)>>>,
    git_ignore_started: bool,
    file_tree_hover: Option<std::path::PathBuf>,
    file_tree_scroll: f32,
    file_tree_rects: Vec<(std::path::PathBuf, (f32, f32, f32, f32))>,
    /// File-tree column visibility + live width (logical px), independent of
    /// the session-tab sidebar. `effective_sidebar_w()` adds this when shown.
    file_tree_visible: bool,
    file_tree_w_logical: f32,
    /// In-flight tree-column resize drag — `(start_cursor_x, start_width)`.
    file_tree_resize: Option<(f32, f32)>,
    /// Settings screen (Warp-style full-view, reached from the sidebar). When
    /// open it replaces the pane grid; the sidebar/titlebar stay live.
    settings_open: bool,
    settings_cat: SettingsCat,
    /// In-memory mirror of settings.json, edited live and written on each
    /// change so the next launch (and `resolve_*`) pick it up.
    set_cwd_mode: String,
    set_file_tree_default: bool,
    set_shell: String,
    /// True when opening settings auto-expanded a collapsed sidebar, so closing
    /// can restore it (but leaves a sidebar the user opened themselves alone).
    settings_expanded_sidebar: bool,
    /// Which form text field has focus (cwd custom path / shell), if any.
    settings_input: Option<SettingsInput>,
    /// Clickable targets collected during the settings paint, for hit-testing.
    settings_rects: Vec<(SettingsAction, (f32, f32, f32, f32))>,
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
    /// 아로나(god 모드) 전면 UI — 별도 OS 창 + arona-ui dist 를 MCP HTTP 로 로드.
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
}

impl App {
    fn new(proxy: EventLoopProxy<UserEvent>) -> Self {
        Self {
            window: None,
            gpu: None,
            tmux: None,
            pty: HashMap::new(),
            pty_layout: None,
            next_pane_id: 1, // %0 is the initial pane created in start_pty
            pending_restores: Vec::new(),
            autoquit_at: None,
            pending_capture: None,
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
            autowindow_left: 0,
            autowindow_at: None,
            autotoggle_sidebar_at: None,
            autoarona_at: None,
            onboard_check_at: None,
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
            pane_header_rects: Vec::new(),
            copy_btn_rects: Vec::new(),
            md_content_h: HashMap::new(),
            md_body_rects: HashMap::new(),
            pane_tab_rects: Vec::new(),
            pane_tab_close_rects: Vec::new(),
            pane_plus_rects: Vec::new(),
            docked: Vec::new(),
            dock_chip_rects: Vec::new(),
            dock_chip_close_rects: Vec::new(),
            copy_toast_at: None,
            pane_busy_check: None,
            pane_last_busy: HashMap::new(),
            window_tab_rects: Vec::new(),
            window_tab_close_rects: Vec::new(),
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
            image_pan_drag: None,
            file_tree_drag: None,
            file_tree_new: None,
            file_tree_new_folder_rect: (0.0, 0.0, 0.0, 0.0),
            file_tree_new_file_rect: (0.0, 0.0, 0.0, 0.0),
            file_tree_new_row_rect: (0.0, 0.0, 0.0, 0.0),
            file_tree_selected: None,
            text_cursor_shown: false,
            confirm_close: None,
            confirm_btn_rects: Vec::new(),
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
            window_alert: std::collections::HashSet::new(),
            collab_toast: None,
            collab_toast_rect: None,
            collab_toast_action: None,
            collab_toast_approve_rect: None,
            collab_toast_deny_rect: None,
            pane_prompt_wait: HashMap::new(),
            collab_attention: Arc::new(Mutex::new(HashMap::new())),
            collab_unread: 0,
            last_window_title_check: None,
            pending_autoleader: None,
            pending_autoleader_at: None,
            pane_cwd_cache: HashMap::new(),
            pane_tty_cache: HashMap::new(),
            window_git: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
            git_poll_cwds: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            git_col_visible: std::env::var("KASASPACE_GIT_PANEL")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            git_col_w_logical: GIT_COL_W,
            git_col_resize: None,
            git_col_scroll: 0.0,
            git_col_file_rects: Vec::new(),
            git_col_btn_rects: Vec::new(),
            git_col_expanded: std::collections::HashSet::new(),
            git_col_diff_cache: HashMap::new(),
            git_col_close_rect: None,
            git_col_expand_rect: None,
            git_commit_btn_rect: None,
            git_commit_caret_rect: None,
            git_commit_menu_open: false,
            git_commit_menu_rects: Vec::new(),
            git_commit_modal_open: false,
            git_commit_modal_include_unstaged: true,
            git_commit_modal_rects: Vec::new(),
            git_col_stage_rects: Vec::new(),
            git_col_discard_rects: Vec::new(),
            git_col_open_rects: Vec::new(),
            git_commit_msg: String::new(),
            git_commit_cursor: 0,
            git_commit_focused: false,
            git_commit_input_rect: None,
            git_col_data: std::sync::Arc::new(std::sync::Mutex::new(GitColView::default())),
            git_op: None,
            git_col_cwd: std::sync::Arc::new(std::sync::Mutex::new(None)),
            git_col_pinned_cwd: None,
            git_path_menu_open: false,
            git_branch_menu_open: false,
            git_path_hdr_rect: None,
            git_branch_hdr_rect: None,
            git_path_menu_rects: Vec::new(),
            git_branch_menu_rects: Vec::new(),
            statusbar_hidden: std::collections::HashSet::new(),
            statusbar_path_rects: Vec::new(),
            statusbar_branch_rects: Vec::new(),
            statusbar_toggle_rects: Vec::new(),
            statusbar_diff_rects: Vec::new(),
            statusbar_menu: None,
            statusbar_menu_dir_rects: Vec::new(),
            statusbar_menu_branch_rects: Vec::new(),
            statusbar_menu_dirs: Vec::new(),
            statusbar_menu_branches: Vec::new(),
            statusbar_menu_scroll: 0.0,
            statusbar_menu_rect: None,
            statusbar_menu_search: String::new(),
            file_tree_search_active: false,
            file_tree_search_query: String::new(),
            file_tree_search_rect: (0.0, 0.0, 0.0, 0.0),
            pane_cwd_check: None,
            show_pane_numbers: false,
            file_tree_root: None,
            file_tree_fs_dirty: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            file_tree_watch: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            file_tree_watch_started: false,
            file_tree_ignored: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
            git_ignore_req: std::sync::Arc::new(std::sync::Mutex::new(None)),
            git_ignore_started: false,
            file_tree_expanded: std::collections::HashSet::new(),
            file_tree_nodes: Vec::new(),
            file_tree_hover: None,
            file_tree_scroll: 0.0,
            file_tree_rects: Vec::new(),
            file_tree_visible: socket::read_file_tree_default(),
            file_tree_w_logical: FILE_TREE_W,
            file_tree_resize: None,
            settings_open: std::env::var("KASATERM_OPEN_SETTINGS").is_ok(),
            settings_cat: match std::env::var("KASATERM_OPEN_SETTINGS").as_deref() {
                Ok("shell") => SettingsCat::Shell,
                Ok("appearance") => SettingsCat::Appearance,
                _ => SettingsCat::General,
            },
            set_cwd_mode: socket::read_default_cwd_mode(),
            set_file_tree_default: socket::read_file_tree_default(),
            set_shell: socket::read_default_shell().unwrap_or_default(),
            settings_expanded_sidebar: false,
            settings_input: None,
            settings_rects: Vec::new(),
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
            font_size: FONT_SIZE,
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
            autosuggest: autosuggest::History::new(),
            input_buf: String::new(),
            current_suggestion: None,
            // Default closed — single-pane, no chrome reads as a plain
            // terminal at first launch. User toggles via the title-bar
            // button or the "보기 → 세션 패널" menu item.
            sidebar_visible: false,
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
    #[cfg(unix)]
    sweep_dead_kasaterm_sockets();
    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
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
/// (입력줄 감지). teammate-mode tmux 위장은 제거됨 — pane 생성은 god 이
/// `kasaterm-cli split` 로 한다. best-effort: 실패해도 본체는 동작한다.
fn install_pane_shims() {
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
    // dir to PATH so it wins over brew, and (2) install an OSC 133
    // prompt-mark hook for inline autosuggestion. The hook wraps PS1 with
    // zero-width (`%{..%}`) `A` (prompt start) / `B` (input start) marks;
    // the `B` mark is what pty-backend sniffs to locate the editable
    // command line. The guard skips re-wrapping a static PS1 (no
    // accumulation) while still re-wrapping themes that rebuild PS1 each
    // precmd (powerlevel10k / starship). zsh-only — other shells ignore
    // ZDOTDIR and just get the PATH prepend.
    write_rc(
        ".zshrc",
        format!(
            "[ -f \"${{HOME}}/.zshrc\" ] && source \"${{HOME}}/.zshrc\"\n\
             export PATH=\"{}:${{PATH}}\"\n\
             _kasaterm_osc133(){{ [[ \"$PS1\" == *$'\\e]133;B'* ]] && return; \
             PS1=$'%{{\\e]133;A\\a%}}'\"$PS1\"$'%{{\\e]133;B\\a%}}'; }}\n\
             autoload -Uz add-zsh-hook 2>/dev/null && \
             add-zsh-hook precmd _kasaterm_osc133 2>/dev/null\n",
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
/// point at. Env override first, then the .app bundle's Resources, then the
/// repo source next to this crate (cargo run). The scripts resolve their
/// siblings via `dirname $0`, so pointing at any one complete copy works.
fn locate_collab_hooks_dir() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("KASATERM_COLLAB_HOOKS_DIR") {
        let p = std::path::PathBuf::from(p);
        if p.is_dir() {
            return Some(p);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
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
    }
    let dev = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("collab-hooks");
    if dev.is_dir() {
        return Some(dev);
    }
    None
}

/// characters.json 의 leader.name (~/.config 우선 → 번들). 아로나 자동 시작·테마
/// 가드가 쓴다. 파일 없거나 leader 없으면 None(기능 skip).
pub(crate) fn load_leader_name() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let mut paths = vec![std::path::PathBuf::from(format!(
        "{home}/.config/kasaterm/characters.json"
    ))];
    if let Some(hd) = locate_collab_hooks_dir() {
        paths.push(hd.join("characters.json"));
    }
    for p in &paths {
        let Ok(s) = std::fs::read_to_string(p) else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) else { continue };
        if let Some(n) = v
            .get("leader")
            .and_then(|l| l.get("name"))
            .and_then(|x| x.as_str())
        {
            if !n.is_empty() {
                return Some(n.to_string());
            }
        }
    }
    None
}

/// 현재 프로세스 cwd 방의 협업 모드 — kasacollab `mode_path` 와 동일 slug
/// ('/'·'.'→'-')·마커(~/.config/kasaterm/collab-mode/<slug>). 기본 solo.
/// 아로나 자동 시작·아로나 패널 god 게이트가 공유한다.
pub(crate) fn current_collab_mode() -> &'static str {
    let (Ok(home), Ok(cwd)) = (std::env::var("HOME"), std::env::current_dir()) else {
        return "solo";
    };
    let slug: String = cwd
        .to_string_lossy()
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect();
    match std::fs::read_to_string(format!("{home}/.config/kasaterm/collab-mode/{slug}")) {
        Ok(m) if m.trim() == "god" => "god",
        _ => "solo",
    }
}

/// 아로나 자동 시작(P5) 명령 — god 모드 && characters 있는 방에서만 첫 pane 에
/// 띄울 `claude --resume <leader> || claude`. solo·무테마·`KASATERM_NO_AUTOLEADER`
/// 면 None(기존 유저 무영향). cwd slug 로 모드 마커를 본다(kasacollab 과 동일 규칙).
pub(crate) fn autoleader_command() -> Option<String> {
    if std::env::var_os("KASATERM_NO_AUTOLEADER").is_some() {
        return None;
    }
    if current_collab_mode() != "god" {
        return None;
    }
    let leader = load_leader_name()?;
    Some(format!("claude --resume {leader} || claude"))
}

/// Stage a `claude` wrapper + a session-scoped hook settings file on the pane
/// PATH (munder-difflin pattern). Collab hooks ride in via `claude --settings`
/// instead of edits to ~/.claude/settings.json, so claude outside a kasaterm
/// pane runs exactly as the user configured it and install-hooks.sh is no
/// longer needed.
fn install_claude_hook_shim(shim_dir: &std::path::Path) {
    // The wrapper is a POSIX sh script; the Windows pane PATH has no sh.
    if cfg!(windows) {
        return;
    }
    let Some(hooks_dir) = locate_collab_hooks_dir() else {
        eprintln!("[shim] collab-hooks dir not found — claude hook shim skipped");
        return;
    };
    let hd = hooks_dir.display();
    let cmd = |script: &str, timeout: u64| {
        serde_json::json!({ "type": "command", "command": format!("\"{hd}/{script}\""), "timeout": timeout })
    };
    // Mirrors what install-hooks.sh used to register globally — same matcher
    // and timeouts, so in-pane behavior is unchanged.
    let settings = serde_json::json!({
        "hooks": {
            "UserPromptSubmit": [{ "hooks": [
                cmd("kasaterm-bind-transcript.sh", 5000),
                cmd("kasaterm-board-context.py", 5000),
            ]}],
            // 같은 방 다른 pane 이 같은 파일을 작업 중이면 Edit 직전에 막는다
            // (transcript 직접 비교, 데몬 무관). solo·god 모드 공통 안전망.
            "PreToolUse": [{ "matcher": "Edit|Write|MultiEdit", "hooks": [cmd("kasaterm-conflict-guard.py", 5000)] }],
            "PostToolUse": [{ "matcher": "SendUserFile", "hooks": [cmd("auto-imgopen.sh", 10)] }],
            "Stop": [{ "hooks": [cmd("kasaterm-stop-drain.sh", 5000)] }],
            "Notification": [{ "hooks": [cmd("kasaterm-notify-attention.sh", 5000)] }],
        }
    });
    let settings_path = shim_dir.join("claude-hooks-settings.json");
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
SETTINGS=\"$SELF_DIR/claude-hooks-settings.json\"\n\
# A user-supplied --settings wins; ours is skipped to avoid a duplicate flag.\n\
for a in \"$@\"; do\n\
  [ \"$a\" = \"--settings\" ] && exec \"$REAL\" \"$@\"\n\
done\n\
[ -f \"$SETTINGS\" ] || exec \"$REAL\" \"$@\"\n\
# god 모드 방 && 사용자가 --append-system-prompt 를 직접 안 줬을 때만 캐릭터\n\
# persona 를 주입한다. persona 는 세션 고정값이라 프롬프트 캐시를 안 깨고, solo\n\
# 거나 characters.json 이 없으면 assign 이 빈 출력 → 무주입(현행 그대로).\n\
HAS_APPEND=\n\
for a in \"$@\"; do [ \"$a\" = \"--append-system-prompt\" ] && HAS_APPEND=1; done\n\
PERSONA=\n\
if [ -z \"$HAS_APPEND\" ] && [ \"$(python3 \"$HOOKS_DIR/kasacollab.py\" mode show 2>/dev/null)\" = god ]; then\n\
  PERSONA=$(python3 \"$HOOKS_DIR/kasaterm-assign-character.py\" 2>/dev/null)\n\
fi\n\
if [ -n \"$PERSONA\" ]; then\n\
  exec \"$REAL\" --settings \"$SETTINGS\" --append-system-prompt \"$PERSONA\" \"$@\"\n\
fi\n\
exec \"$REAL\" --settings \"$SETTINGS\" \"$@\"\n",
        hd = hooks_dir.display());
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
        "#!/bin/sh\nexec python3 \"{}/kasacollab.py\" \"$@\"\n",
        hooks_dir.display()
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
        if let Some(bash) = git_bash_path() {
            return Some(bash);
        }
    }
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

/// Shells offered by the sidebar "+" picker: `(label, nerd-font icon,
/// shell command)`. Only installed shells are listed, so WSL / PowerShell 7
/// quietly drop off machines that lack them. Windows-only content for now;
/// returns empty elsewhere so the render path stays cfg-free.
fn available_shells() -> Vec<(&'static str, &'static str, String)> {
    #[allow(unused_mut)]
    let mut out: Vec<(&'static str, &'static str, String)> = Vec::new();
    #[cfg(windows)]
    {
        let pwsh7 = r"C:\Program Files\PowerShell\7\pwsh.exe";
        if std::path::Path::new(pwsh7).is_file() {
            out.push(("PowerShell 7", "\u{ebc7}", pwsh7.to_string()));
        }
        // Windows PowerShell ships with the OS — always present.
        out.push(("Windows PowerShell", "\u{ebc7}", "powershell.exe".to_string()));
        out.push(("Command Prompt", "\u{ebc4}", "cmd.exe".to_string()));
        if let Some(bash) = git_bash_path() {
            out.push(("Git Bash", "\u{f489}", bash));
        }
        if std::path::Path::new(r"C:\Windows\System32\wsl.exe").is_file() {
            out.push(("WSL", "\u{f17c}", "wsl.exe".to_string()));
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

/// Port the panel webviews should poll: the actual bound port recorded in
/// `mcp_port_file_path` if present, else the env hint, else the 8765 default.
/// The file beats the env so a daemon that fell back off its preferred port
/// still lines up with the panels.
fn mcp_panel_port() -> String {
    std::fs::read_to_string(mcp_port_file_path())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("KASASPACE_MCP_PORT").ok())
        .unwrap_or_else(|| "8765".to_string())
}

/// 부팅 시 temp_dir 의 죽은 `kasaterm-<pid>.sock` 잔재를 청소한다. 소켓 경로가
/// PID 별이라 인스턴스마다 다른 파일을 만드는데, `Server::bind` 의 stale 정리는
/// *자기 경로* 만 치워서 죽은 다른 인스턴스 소켓이 영영 남는다(재시작·빌드 반복
/// 시 누적). 여기서 connect 가 실패하는(=리스너 없는) 소켓 파일만 지운다 —
/// 살아있는 인스턴스 소켓은 절대 건드리지 않으므로 멀티 인스턴스에서도 안전.
/// 자기 PID 소켓은 아직 bind 전이라 connect 가 실패할 수 있으니 제외한다.
#[cfg(unix)]
fn sweep_dead_kasaterm_sockets() {
    let own = format!("kasaterm-{}.sock", std::process::id());
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
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
        App::cleanup_collab_markers("%987");
        assert!(!bound.exists(), "bound marker should be deleted");
        assert!(!character.exists(), "character marker should be deleted");
        assert!(!nudged.exists(), "god-nudged marker should be deleted");
        assert!(other.exists(), "another pane's marker must survive");
        std::fs::remove_dir_all(&room).unwrap();
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
    fn block_extract_drops_wide_char_spacer() {
        let row = wide_row("코드복사", 16);
        // block = (start, end, left, right) inclusive, full CJK run is cols 0..=7.
        let out = extract_block(&[row], (0, 0, 0, 7));
        assert_eq!(out, "코드복사");
    }
}
