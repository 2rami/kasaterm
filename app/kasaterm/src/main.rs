//! tmuxify — sugarloaf-rendered terminal driven by
//! tmux-bridge. Multi-pane: tmux's split-window creates additional
//! panes, layout-change events tell us how to lay them out, and we
//! render each pane inside its rect from the parsed Layout tree.
//! Phase A Task #13/14: wheel + scrollback, IME, selection + clipboard,
//! cursor blink, OSC titles, multi-pane render + focus routing.

mod autosuggest;
mod cells;
mod daemon;
mod gpu;
mod render;
mod handler;
mod socket;
mod stream;
mod theme;
mod transcript;

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
        round_rect(g, px - 1.0, y - 1.0, pw + 2.0, h + 2.0, theme::RADIUS_MD, theme::BORDER);
        round_rect(g, px, y, pw, h, theme::RADIUS_MD, theme::SURFACE_ACTIVE);
    };
    let row = |g: &mut gpu::GpuRenderer, iy: f32, label: &str, on: bool| {
        if on {
            round_rect(g, px + 4.0, iy + 1.0, pw - 8.0, item_h - 2.0, theme::RADIUS_SM, theme::with_alpha(theme::ACCENT, 0x40));
        }
        let col = if on { theme::TEXT } else { theme::TEXT_DIM };
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
const GIT_COL_W: f32 = 264.0;
const GIT_COL_W_MIN: f32 = 190.0;
const GIT_COL_W_MAX: f32 = 460.0;
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
  .pane { background: #22262e; border: 1px solid #2e323b; border-radius: 9px; padding: 10px; margin-bottom: 8px; }
  .row1 { display: flex; align-items: center; gap: 8px; }
  .sid { font-weight: 600; color: #5a8ce6; }
  .status { margin-left: auto; font-size: 11px; padding: 2px 8px; border-radius: 6px; background: #2e323b; color: #a0a6b0; }
  .status.working { color: #5a8ce6; }
  .status.building { color: #e3b341; }
  .status.idle { color: #787e8a; }
  .status.blocked { color: #f85149; }
  .status.waiting { color: #f0883e; font-weight: 600; }
  .intent { margin-top: 5px; color: #ecedf3; word-break: break-word; }
  .files { margin-top: 3px; font-size: 11px; color: #787e8a; word-break: break-all; }
  .bell { background: none; border: 0; padding: 2px 4px; margin-left: 6px; cursor: pointer; color: #5a8ce6; display: inline-flex; align-items: center; border-radius: 6px; flex: none; }
  .bell:hover { background: #2e323b; }
  .bell.off { color: #5a5f6b; }
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
const BELL_ON = '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9"/><path d="M10.3 21a1.94 1.94 0 0 0 3.4 0"/></svg>';
const BELL_OFF = '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M8.7 3A6 6 0 0 1 18 8a21.3 21.3 0 0 0 .6 5"/><path d="M17 17H3s3-2 3-9a4.7 4.7 0 0 1 .3-1.7"/><path d="M10.3 21a1.94 1.94 0 0 0 3.4 0"/><line x1="1" y1="1" x2="23" y2="23"/></svg>';

function esc(s) { return (s || "").replace(/[&<>"]/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;'}[c])); }
function leaf(p) { const i = p.lastIndexOf('/'); return i >= 0 ? p.slice(i + 1) : p; }

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
    const muted = !!p.muted;
    // "waiting" = claude blocked on a permission/input prompt (agents --json).
    // The transcript can't see this, so it's the one status worth flagging hard.
    const statusLabel = st === "waiting"
      ? "⚠ 권한 대기중" + (p.waiting_for ? ` (${esc(p.waiting_for)})` : "")
      : esc(p.status || "");
    const d = document.createElement("div");
    d.className = "pane";
    d.innerHTML =
      `<div class="row1"><span class="sid">${esc(p.surface_id)}</span>` +
      `<span class="status ${esc(st)}">${statusLabel}</span>` +
      `<button class="bell ${muted ? "off" : ""}" data-sid="${esc(p.surface_id)}" data-muted="${muted}" ` +
      `title="${muted ? "알림 꺼짐 — 클릭해서 켜기" : "알림 켜짐 — 클릭해서 끄기"}">${muted ? BELL_OFF : BELL_ON}</button></div>` +
      `<div class="intent">${esc(p.intent || "")}</div>` +
      (files ? `<div class="files">${esc(files)}</div>` : "");
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

// Mute toggle: event-delegated so it survives the list.innerHTML rebuild each
// poll. `data-muted` is the CURRENT state, so the POST flips it.
$("list").addEventListener("click", async (e) => {
  const btn = e.target.closest(".bell");
  if (!btn) return;
  const sid = btn.dataset.sid;
  const on = btn.dataset.muted === "true" ? "false" : "true";
  try {
    await fetch(base + "/board-mute?surface=" + encodeURIComponent(sid) + "&on=" + on, { method: "POST" });
    poll();
  } catch (e) {}
});
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

/// Terminal-pane action button kinds, painted on the right side of a
/// terminal pane's header (split-v / split-h). New-terminal and web were
/// dropped — the +button covers "new shell" and the web overlay added
/// complexity for little payoff. Wired to per-frame `pane_action_hits`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActionKind {
    SplitV,
    SplitH,
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
    /// false = Render (laid-out view), true = Raw (wgpu text editor).
    raw_mode: bool,
    /// Raw-mode edit buffer, one entry per line.
    edit_lines: Vec<String>,
    /// Edit cursor: line index + column in chars.
    cur_line: usize,
    cur_col: usize,
    /// Scroll offset in logical px (both Render and Raw).
    scroll: usize,
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
}

impl PaneTab {
    fn term(&self) -> Option<&TerminalPane> {
        if let PaneContent::Terminal(t) = &self.content { Some(t) } else { None }
    }
    fn term_mut(&mut self) -> Option<&mut TerminalPane> {
        if let PaneContent::Terminal(t) = &mut self.content { Some(t) } else { None }
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
    /// Daemon pushed its full session>window>pane structure (split/close/new/
    /// switch happened on its side); the GUI adopts the active window's layout
    /// and refreshes session/window metadata.
    DaemonState(stream::StateView),
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
    /// One row per changed file: `(marker, path)` where marker is
    /// `A` staged · `M` modified · `?` untracked. Ordered staged→modified→new.
    files: Vec<(char, String)>,
    /// Local branch names for the switcher dropdown (current one is `branch`).
    branches: Vec<String>,
}

/// Action buttons at the foot of the git column. `StageAll` runs `git add -A`;
/// `Commit` hands the commit to the active claude pane; `Push` pushes the
/// current branch. All shell out through `kasa_mcp::git` on a worker thread so
/// the UI never blocks.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GitColBtn {
    StageAll,
    Commit,
    Push,
}

/// File-tree icon (Material Icon Theme name) for a file, chosen by name then
/// extension. The SVG is multi-color (authored fills) so it draws through
/// `queue_ft_icon` untinted — VS Code-style language logos that read at a
/// glance. Whole-name matches (Dockerfile, package.json…) win over extension.
fn file_icon(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "dockerfile" | ".dockerignore" | "docker-compose.yml" | "compose.yaml" => return "docker",
        ".gitignore" | ".gitattributes" | ".gitmodules" => return "git",
        "package.json" | "package-lock.json" => return "nodejs",
        "tsconfig.json" => return "tsconfig",
        "readme.md" | "readme" | "readme.txt" => return "readme",
        "license" | "license.md" | "license.txt" | "copying" => return "license",
        "todo.md" | "todo" | "todo.txt" => return "todo",
        _ => {}
    }
    let ext = lower.rsplit('.').next().unwrap_or("");
    match ext {
        "rs" => "rust",
        "py" | "pyi" | "pyw" => "python",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" | "tsx" => "react",
        "ts" => "typescript",
        "go" => "go",
        "rb" => "ruby",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "swift" => "swift",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" => "cpp",
        "cs" => "csharp",
        "php" => "php",
        "lua" => "lua",
        "vue" => "vue",
        "prisma" => "prisma",
        "graphql" | "gql" => "graphql",
        "sql" => "database",
        "html" | "htm" | "xml" => "html",
        "css" => "css",
        "scss" | "sass" | "less" => "sass",
        "json" | "jsonc" => "json",
        "yml" | "yaml" => "yaml",
        "toml" | "ini" | "conf" | "cfg" | "env" => "settings",
        "lock" => "lock",
        "md" | "markdown" | "rst" => "markdown",
        "txt" | "log" => "document",
        "sh" | "bash" | "zsh" | "fish" => "console",
        "ps1" => "powershell",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "tiff" | "tif" => "image",
        "svg" => "svg",
        "pdf" => "pdf",
        "ttf" | "otf" | "woff" | "woff2" => "font",
        "zip" | "tar" | "gz" | "tgz" | "rar" | "7z" | "xz" => "zip",
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" => "audio",
        "mp4" | "mov" | "avi" | "mkv" | "webm" => "video",
        _ => "document",
    }
}

/// File-tree folder icon (Material Icon Theme name) chosen by folder name, so
/// common dirs (src, test, .github…) carry a tinted variant like VS Code.
/// Unknown folders fall back to the plain blue `folder-base`.
fn folder_icon(name: &str) -> &'static str {
    match name.to_ascii_lowercase().as_str() {
        "src" | "lib" | "app" | "source" | "crates" => "folder-src",
        "test" | "tests" | "__tests__" | "spec" | "specs" => "folder-test",
        "node_modules" => "folder-node",
        "dist" | "build" | "out" | "release" => "folder-dist",
        "target" => "folder-target",
        "public" | "static" | "www" => "folder-public",
        "assets" | "images" | "img" | "icons" | "media" => "folder-images",
        "docs" | "doc" | "documentation" => "folder-docs",
        ".github" => "folder-github",
        "config" | ".config" | "conf" | "settings" => "folder-config",
        _ => "folder-base",
    }
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
        "toml" => "toml",
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
    /// Queued split directions driven by KASATERM_AUTOSPLIT — headless
    /// repro for the multi-pane render path. Empty in normal use.
    autosplit_plan: Vec<kasa_pty::SplitDir>,
    autosplit_at: Option<Instant>,
    /// Headless tab-drag simulation. KASATERM_AUTODRAG="src:from:dst"
    /// (e.g. "%2:0:%0") fires `simulate_tab_merge` after AUTODRAG_MS so
    /// the cross-pane merge path can be verified without a real mouse.
    autodrag_plan: Option<(String, usize, String)>,
    autodrag_at: Option<Instant>,
    /// Headless repro for the window sidebar: number of extra windows left to
    /// spawn (KASATERM_AUTOWINDOWS) and when the next one fires. 0 disables.
    autowindow_left: usize,
    autowindow_at: Option<Instant>,
    /// Headless repro for the sidebar toggle (KASATERM_AUTOTOGGLE_SIDEBAR_MS):
    /// flips the sidebar once at this instant so a screenshot can capture the
    /// collapsed-grid state without a human clicking the title-bar button.
    autotoggle_sidebar_at: Option<Instant>,
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
    daemon_client: Option<Arc<stream::DaemonClient>>,
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
    /// Render/Raw toggle pill hit rects for markdown pane headers:
    /// (pane id, is_raw, logical rect). A click sets that pane's md_raw_mode.
    md_toggle_rects: Vec<(String, bool, (f32, f32, f32, f32))>,
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
    daemon_synced: bool,
    /// Dock chip hit rects: (pane id, logical rect). Click restores (undock).
    dock_chip_rects: Vec<(String, (f32, f32, f32, f32))>,
    /// Dock chip × hit rects: (pane id, logical rect). Click kills the pane.
    dock_chip_close_rects: Vec<(String, (f32, f32, f32, f32))>,
    /// When the "복사됨" copy toast started animating. Drives its fade in the
    /// overlay pass; `None` once faded out. Set on a successful block copy.
    copy_toast_at: Option<Instant>,
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
    selection: Option<Selection>,
    drag_anchor: Option<(u16, u16)>,
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
    /// Active completion toast (message + start instant) for a sibling pane's
    /// working→idle flip — "✓ %3 완료 · git 패널". Fades like `copy_toast_at`.
    /// Replaced by the newest flip; a brief overlap just shows the latest.
    collab_toast: Option<(String, Instant)>,
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
    git_col_file_rects: Vec<(String, (f32, f32, f32, f32))>,
    git_col_btn_rects: Vec<(GitColBtn, (f32, f32, f32, f32))>,
    /// `git status` snapshot for the active pane's cwd: the poller writes it
    /// off the main thread, the render reads it. `git_col_cwd` is the cwd the
    /// poller should refresh (render publishes the active pane's cwd into it).
    /// Same pattern as `window_git` / `git_poll_cwds`.
    git_col_data: std::sync::Arc<std::sync::Mutex<GitColView>>,
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
    /// Single-pane Cmd+W in daemon mode sets this to close the GUI window
    /// (ghostty-style) on the next about_to_wait, instead of killing the pane.
    /// The daemon keeps the session alive so relaunch restores it.
    should_exit: bool,
    /// Last `refresh_pane_cwds` sweep — rate-limits the lsof calls.
    pane_cwd_check: Option<Instant>,
    /// Preview panes (image/markdown) already materialized from the daemon's
    /// StateView, keyed by pane id → the path we built it from. Guards against
    /// re-decoding the image on every (frequent) State broadcast; a changed
    /// path rebuilds. See the `UserEvent::DaemonState` handler.
    applied_previews: HashMap<String, String>,
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
    file_tree_hover: Option<std::path::PathBuf>,
    file_tree_scroll: f32,
    file_tree_rects: Vec<(std::path::PathBuf, (f32, f32, f32, f32))>,
    /// File-tree column visibility + live width (logical px), independent of
    /// the session-tab sidebar. `effective_sidebar_w()` adds this when shown.
    file_tree_visible: bool,
    file_tree_w_logical: f32,
    /// In-flight tree-column resize drag — `(start_cursor_x, start_width)`.
    file_tree_resize: Option<(f32, f32)>,
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
            autosplit_plan: Vec::new(),
            autosplit_at: None,
            autodrag_plan: None,
            autodrag_at: None,
            autowindow_left: 0,
            autowindow_at: None,
            autotoggle_sidebar_at: None,
            autotoggle_left: 0,
            autotabs_n: 0,
            autotabs_at: None,
            dead_panes: Arc::new(Mutex::new(Vec::new())),
            daemon_client: None,
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
            md_toggle_rects: Vec::new(),
            pane_tab_rects: Vec::new(),
            pane_tab_close_rects: Vec::new(),
            pane_plus_rects: Vec::new(),
            docked: Vec::new(),
            daemon_synced: false,
            dock_chip_rects: Vec::new(),
            dock_chip_close_rects: Vec::new(),
            copy_toast_at: None,
            window_tab_rects: Vec::new(),
            window_tab_close_rects: Vec::new(),
            new_window_btn_rect: None,
            shell_menu_open: false,
            shell_menu_hits: Vec::new(),
            pending_shell: None,
            window_labels: Vec::new(),
            window_labels_at: None,
            selection: None,
            drag_anchor: None,
            resize_drag: None,
            last_divider_pos: None,
            last_divider_pty_resize: None,
            header_drag: None,
            tab_drag: None,
            pane_tab_hover: None,
            image_btn_rects: Vec::new(),
            sidebar_w_logical: SIDEBAR_W,
            sidebar_resize: None,
            last_resized_cells: (0, 0),
            pending_resize: None,
            mouse_forward_pane: None,
            last_left_click: None,
            zoomed_pane: None,
            saved_window_frame: None,
            titlebar_drag_pending: None,
            last_window_title: None,
            claude_busy_until: None,
            last_claude_status: None,
            pane_activity: HashMap::new(),
            collab_toast: None,
            collab_unread: 0,
            last_window_title_check: None,
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
            git_col_data: std::sync::Arc::new(std::sync::Mutex::new(GitColView::default())),
            git_col_cwd: std::sync::Arc::new(std::sync::Mutex::new(None)),
            git_col_pinned_cwd: None,
            git_path_menu_open: false,
            git_branch_menu_open: false,
            git_path_hdr_rect: None,
            git_branch_hdr_rect: None,
            git_path_menu_rects: Vec::new(),
            git_branch_menu_rects: Vec::new(),
            should_exit: false,
            pane_cwd_check: None,
            applied_previews: HashMap::new(),
            show_pane_numbers: false,
            file_tree_root: None,
            file_tree_expanded: std::collections::HashSet::new(),
            file_tree_nodes: Vec::new(),
            file_tree_hover: None,
            file_tree_scroll: 0.0,
            file_tree_rects: Vec::new(),
            file_tree_visible: true,
            file_tree_w_logical: FILE_TREE_W,
            file_tree_resize: None,
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
            preview_windows: Vec::new(),
            pane_action_hits: Vec::new(),
            version_anim_start: Instant::now(),
            menu: None,
            git_menu_item: None,
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

    /// Sidebar width that layout math should actually use: the full
    /// `SIDEBAR_W` when the strip is shown, 0 when collapsed. Every
    /// origin_x / window_cells / hit-test calc routes through here so a
    /// single `sidebar_visible` flip reflows the whole grid.
    fn effective_sidebar_w(&self) -> f32 {
        self.tab_strip_w() + self.file_tree_col_w()
    }

    /// Width of the session-tab strip alone (0 when collapsed).
    fn tab_strip_w(&self) -> f32 {
        if self.sidebar_visible {
            self.sidebar_w_logical
        } else {
            0.0
        }
    }

    /// File-tree column width (0 when hidden). Independent of the tab strip.
    fn file_tree_col_w(&self) -> f32 {
        if self.file_tree_visible {
            self.file_tree_w_logical
        } else {
            0.0
        }
    }

    /// Left edge (logical x) of the file-tree column — right after the tab
    /// strip. The column sits between the tabs and the cell grid.
    fn file_tree_col_x(&self) -> f32 {
        self.tab_strip_w()
    }

    /// Right-hand chrome width (the git column), mirroring `effective_sidebar_w`
    /// on the left. Folded into `window_cells` so the cell grid reflows and no
    /// pane ever overlaps the column.
    fn effective_right_chrome_w(&self) -> f32 {
        self.git_col_w()
    }

    /// Git-column width (0 when hidden).
    fn git_col_w(&self) -> f32 {
        if self.git_col_visible {
            self.git_col_w_logical
        } else {
            0.0
        }
    }

    /// Left edge (logical x) of the git column — flush against the window's
    /// right edge. 0 before the window exists (no paint yet).
    fn git_col_x(&self) -> f32 {
        let w = self.git_col_w();
        self.window.as_ref().map_or(0.0, |win| {
            let scale = self.effective_scale();
            win.inner_size().width as f32 / scale - w
        })
    }

    /// Git-column-toggle button rect, parked at the right end of the title
    /// strip (mirrors the file-tree toggle on the left). Needs the window
    /// width, so it returns `None` before the first paint.
    fn git_col_toggle_rect(&self) -> Option<(f32, f32, f32, f32)> {
        let w = 26.0;
        let h = 22.0;
        let win_w = self.window.as_ref().map(|win| {
            let scale = self.effective_scale();
            win.inner_size().width as f32 / scale
        })?;
        let x = win_w - w - 8.0;
        let y = (TITLE_HEIGHT - h) / 2.0;
        Some((x, y, w, h))
    }

    /// Show/hide the git column. Same reflow path as `toggle_sidebar`: flip the
    /// flag, resize the PTYs to the new usable cols, repaint. Publishes the
    /// active cwd so the poller has something to refresh the moment it opens.
    fn toggle_git_col(&mut self) {
        self.git_col_visible = !self.git_col_visible;
        if self.git_col_visible {
            self.publish_git_col_cwd();
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// Push the active pane's cwd into the shared `git_col_cwd` so the git
    /// poller refreshes the right repo. Cheap string clone; called from the
    /// render right before the column paints (mirrors `git_poll_cwds`).
    fn publish_git_col_cwd(&self) {
        if !self.git_col_visible {
            return;
        }
        // A user-pinned repo (picked from the path dropdown) overrides the
        // active-pane follow — the column stays on that repo until unpinned.
        if let Some(pinned) = self.git_col_pinned_cwd.clone() {
            if let Ok(mut guard) = self.git_col_cwd.lock() {
                *guard = Some(pinned);
            }
            return;
        }
        let active = self.ws.lock().ok().and_then(|w| w.active_pane.clone());
        let resolved = active
            .as_ref()
            .and_then(|id| self.pane_cwd_cache.get(id).cloned());
        if let Ok(mut guard) = self.git_col_cwd.lock() {
            match resolved {
                // A confidently-resolved pane cwd always wins.
                Some(cwd) => *guard = Some(cwd),
                // Cache miss (e.g. right after a pane switch, before the cwd
                // sniffer catches up): keep the last good cwd instead of
                // flashing the launch dir — which is often a non-repo and
                // would read as "not a repo". Seed from current_dir only on
                // the very first frame, when nothing is known yet.
                None if guard.is_none() => *guard = std::env::current_dir().ok(),
                None => {}
            }
        }
    }

    /// Run a git-column button. Push shells out on a worker thread so the UI
    /// never blocks on the network; Commit hands the work to the claude in the
    /// active pane (native commit-message input is phase 2), mirroring the old
    /// webview panel's AI-commit. Both read the column's repo from the poller's
    /// snapshot so the action always targets what the user sees.
    fn run_git_col_action(&mut self, btn: GitColBtn) {
        let cwd = self.git_col_data.lock().ok().and_then(|g| g.cwd.clone());
        let Some(cwd) = cwd else { return };
        match btn {
            GitColBtn::StageAll => {
                // `git add -A` off-thread; the poller's next tick flips the
                // rows to staged.
                let proxy = self.proxy.clone();
                std::thread::spawn(move || {
                    let _ = kasa_mcp::git::git_stage_all(&cwd);
                    let _ = proxy.send_event(UserEvent::Redraw);
                });
            }
            GitColBtn::Push => {
                let proxy = self.proxy.clone();
                std::thread::spawn(move || {
                    let _ = kasa_mcp::git::git_push(&cwd);
                    // Wake the loop so the poller's next tick repaints ahead/behind.
                    let _ = proxy.send_event(UserEvent::Redraw);
                });
            }
            GitColBtn::Commit => {
                if self.active_pane_is_claude() {
                    self.send_bytes(
                        "git 패널에서 커밋을 눌렀어. 지금 작업 디렉토리의 변경사항을 검토하고 적절한 한국어 커밋 메시지로 git add + commit 해줘.\n"
                            .as_bytes(),
                    );
                }
            }
        }
    }

    /// Check out `branch` in the column's repo (off-thread). A dirty tree makes
    /// git refuse with a clear message — we don't stash/force, just let the
    /// poller repaint whatever git did. Closes the branch dropdown.
    fn run_git_checkout(&mut self, branch: String) {
        self.git_branch_menu_open = false;
        let cwd = self.git_col_data.lock().ok().and_then(|g| g.cwd.clone());
        let Some(cwd) = cwd else { return };
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let _ = kasa_mcp::git::git_checkout(&cwd, &branch);
            let _ = proxy.send_event(UserEvent::Redraw);
        });
    }

    /// True when the active pane runs claude — gates the AI-commit injection so
    /// a multi-line instruction never lands (and auto-submits) on a bare shell.
    fn active_pane_is_claude(&self) -> bool {
        let Some(id) = self.target_surface() else {
            return false;
        };
        if let Some(p) = self.pty.get(&id) {
            if let Some(l) = Self::smart_pane_label(p) {
                return l.to_lowercase().contains("claude");
            }
        }
        // Daemon-owned pane: fall back to the title the daemon pushed.
        self.ws
            .lock()
            .ok()
            .and_then(|ws| ws.panes.get(&id).and_then(|p| p.title.clone()))
            .map(|t| t.to_lowercase().contains("claude"))
            .unwrap_or(false)
    }

    /// Preview a changed file from the git column (image/code/markdown by
    /// extension), resolved against the column's repo cwd. A native diff view
    /// is phase 2; opening the file is the useful v1. Daemon-only, like the
    /// file-tree's file-click path.
    fn open_git_file(&mut self, rel: &str) {
        let cwd = self.git_col_data.lock().ok().and_then(|g| g.cwd.clone());
        let Some(cwd) = cwd else { return };
        let abs = cwd.join(rel);
        let ext = abs
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        let kind = match ext.as_str() {
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tiff" | "tif" | "ico" => "image",
            "md" | "markdown" | "txt" | "log" | "" => "markdown",
            _ => "code",
        };
        let ps = abs.to_string_lossy().into_owned();
        if let Some(client) = self.daemon_client.as_ref() {
            client.open_preview(kind, &ps);
        }
    }

    /// Title-bar sidebar-toggle button rect (logical px), parked just
    /// right of the macOS traffic lights. Fixed position (doesn't depend
    /// on state) so the renderer and the click handler share one source.
    fn sidebar_toggle_rect() -> (f32, f32, f32, f32) {
        let w = 26.0;
        let h = 22.0;
        let x = TRAFFIC_LIGHT_WIDTH + 6.0;
        let y = (TITLE_HEIGHT - h) / 2.0;
        (x, y, w, h)
    }

    /// File-tree-toggle button rect, parked just right of the sidebar toggle.
    fn file_tree_toggle_rect() -> (f32, f32, f32, f32) {
        let (sx, sy, sw, sh) = Self::sidebar_toggle_rect();
        (sx + sw + 2.0, sy, sw, sh)
    }

    /// Show/hide the left window-tab sidebar. The cell grid reflows to the
    /// new usable width (every layout calc reads `effective_sidebar_w()`),
    /// so we just flip the flag, resize the PTYs to the new cols/rows, and
    /// repaint.
    fn toggle_sidebar(&mut self) {
        self.sidebar_visible = !self.sidebar_visible;
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// Show/hide the file-tree column. Same reflow path as `toggle_sidebar`.
    fn toggle_file_tree(&mut self) {
        self.file_tree_visible = !self.file_tree_visible;
        if self.file_tree_visible {
            self.refresh_file_tree();
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// Drop a word off the end of the input buffer (for Ctrl-W /
    /// Alt-Backspace): eat trailing spaces, then non-spaces.
    fn buf_pop_word(&mut self) {
        while self.input_buf.ends_with(' ') {
            self.input_buf.pop();
        }
        while let Some(c) = self.input_buf.chars().last() {
            if c == ' ' {
                break;
            }
            self.input_buf.pop();
        }
    }

    /// Recompute the inline suggestion against the live grid. Called once
    /// per frame from the render path (so the grid reflects the latest
    /// shell echo). Only runs at a shell prompt (active pane, not
    /// alt-screen).
    ///
    /// Two ways to find the editable command line:
    ///   1. **OSC 133 mark** (primary) — the shell's precmd hook emits a
    ///      `B` mark at prompt end; pty-backend tags the cursor there. We
    ///      read the grid from that column to the cursor, which is the
    ///      ground truth: it survives Tab-completion, paste, RPROMPT and
    ///      wide (CJK) chars that the typed-buffer heuristic can't see.
    ///   2. **typed buffer** (fallback) — when there's no usable mark yet
    ///      (tmux backend, pre-first-prompt, or a scrolled-away mark), we
    ///      trust `input_buf` but only if it's still the tail of the
    ///      cursor row, which auto-suppresses on edits we can't track.
    fn update_suggestion(&mut self) {
        if !self.autosuggest.enabled() || !self.preedit.is_empty() {
            self.current_suggestion = None;
            return;
        }
        let line: Option<String> = {
            let ws = self.ws.lock().unwrap();
            match ws.active().and_then(|p| p.term()) {
                Some(t) if !t.alt_screen => {
                    let crow = t.cursor_row as usize;
                    let ccol = t.cursor_col as usize;
                    let row_cells = t.cells.get(crow);
                    let cell_str = |r: &[GridCell], from: usize, to: usize| -> String {
                        r.iter()
                            .take(to)
                            .skip(from)
                            .map(|c| if c.ch == '\0' { ' ' } else { c.ch })
                            .collect()
                    };
                    // Primary: OSC 133 mark still on the cursor's row.
                    let from_mark = match t.prompt_end {
                        Some((pr, pc))
                            if pr as usize == crow && (pc as usize) <= ccol =>
                        {
                            row_cells.map(|r| cell_str(r, pc as usize, ccol))
                        }
                        _ => None,
                    };
                    if from_mark.is_some() {
                        from_mark
                    } else if !self.input_buf.is_empty() {
                        let synced = row_cells
                            .map(|r| cell_str(r, 0, ccol).ends_with(&self.input_buf))
                            .unwrap_or(false);
                        synced.then(|| self.input_buf.clone())
                    } else {
                        None
                    }
                }
                _ => None,
            }
        };
        let Some(line) = line else {
            self.current_suggestion = None;
            return;
        };
        // Nothing to complete from an empty / whitespace-only line.
        if line.trim().is_empty() {
            self.current_suggestion = None;
            return;
        }
        self.autosuggest.maybe_refresh();
        self.current_suggestion = self.autosuggest.suggest(&line);
    }

    /// Build banner shown bottom-right on launch: `v<pkg>·<git rev>`
    /// (rev carries a trailing '+' when built dirty). Stamped at compile
    /// time by build.rs.
    fn version_label() -> String {
        format!(
            "v{}·{}",
            env!("CARGO_PKG_VERSION"),
            env!("KASATERM_GIT_REV")
        )
    }

    /// 0.0..1.0 opacity for the launch banner: solid through
    /// VERSION_HOLD_MS, then a linear fade across VERSION_FADE_MS, then
    /// gone. Also the single source of truth for "is the banner still
    /// animating" (alpha > 0).
    fn version_alpha(&self) -> f32 {
        let e = self.version_anim_start.elapsed().as_millis();
        if e < VERSION_HOLD_MS {
            1.0
        } else if e < VERSION_HOLD_MS + VERSION_FADE_MS {
            1.0 - (e - VERSION_HOLD_MS) as f32 / VERSION_FADE_MS as f32
        } else {
            0.0
        }
    }

    /// 0.0..1.0 opacity for the "복사됨" copy toast: solid for a brief hold
    /// after a block copy, then a quick fade. Mirrors `version_alpha`.
    fn copy_toast_alpha(&self) -> f32 {
        const HOLD: u128 = 900;
        const FADE: u128 = 500;
        let Some(at) = self.copy_toast_at else { return 0.0 };
        let e = at.elapsed().as_millis();
        if e < HOLD {
            1.0
        } else if e < HOLD + FADE {
            1.0 - (e - HOLD) as f32 / FADE as f32
        } else {
            0.0
        }
    }

    /// 0.0..1.0 opacity for a collab completion toast: a longer hold than the
    /// copy toast (a sibling finishing is worth a real glance) then a fade.
    /// Returns 0 with no active toast, so callers gate paint + frame-loop wake.
    fn collab_toast_alpha(&self) -> f32 {
        const HOLD: u128 = 2400;
        const FADE: u128 = 600;
        let Some((_, at)) = self.collab_toast.as_ref() else { return 0.0 };
        let e = at.elapsed().as_millis();
        if e < HOLD {
            1.0
        } else if e < HOLD + FADE {
            1.0 - (e - HOLD) as f32 / FADE as f32
        } else {
            0.0
        }
    }

    /// Copy a detected code block's text to the clipboard and arm the
    /// toast. Reuses arboard like `copy_selection`. Best-effort: a
    /// clipboard failure just logs (the toast still fires on success).
    fn copy_block_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        match arboard::Clipboard::new() {
            Ok(mut cb) => {
                if let Err(e) = cb.set_text(text.to_string()) {
                    eprintln!("[tmuxify] clipboard write failed: {e}");
                    return;
                }
            }
            Err(e) => {
                eprintln!("[tmuxify] clipboard open failed: {e}");
                return;
            }
        }
        self.copy_toast_at = Some(Instant::now());
    }


    /// Open the session panel in its own OS window. Mirrors open_git_panel:
    /// the page polls `/sessions` on the MCP server. Best-effort — any failure
    /// just logs and leaves the terminal untouched.
    fn open_session_panel(&mut self, event_loop: &ActiveEventLoop) {
        if self.session_panel_window.is_some() {
            return;
        }
        let port = mcp_panel_port();
        let attrs = WindowAttributes::default()
            .with_title("sessions")
            .with_theme(Some(Theme::Dark))
            .with_inner_size(LogicalSize::new(260.0, 360.0));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("[session-panel] window create failed: {e}");
                return;
            }
        };
        let html = SESSION_PANEL_HTML.replace("__PORT__", &port);
        // build_as_child for the same use-after-free reason as the git panel.
        let webview = match wry::WebViewBuilder::new()
            .with_html(html)
            .with_bounds(wry::Rect {
                position: wry::dpi::LogicalPosition::new(0.0, 0.0).into(),
                size: wry::dpi::LogicalSize::new(260.0, 360.0).into(),
            })
            .build_as_child(window.as_ref())
        {
            Ok(wv) => wv,
            Err(e) => {
                eprintln!("[session-panel] webview build failed: {e}");
                return;
            }
        };
        eprintln!("[session-panel] open; polling 127.0.0.1:{port}/sessions");
        self.session_panel_window = Some(window);
        self.session_panel_webview = Some(webview);
    }

    /// Toggle the session panel from the menu: close if open, open if not.
    fn toggle_session_panel(&mut self, event_loop: &ActiveEventLoop) {
        if self.session_panel_window.is_some() {
            // Drop the webview before the window it borrows from.
            self.session_panel_webview = None;
            self.session_panel_window = None;
        } else {
            self.open_session_panel(event_loop);
        }
    }

    /// Open the board panel in its own OS window. Mirrors open_session_panel:
    /// the page polls `/board` on the MCP server. Best-effort — any failure
    /// just logs and leaves the terminal untouched.
    fn open_board_panel(&mut self, event_loop: &ActiveEventLoop) {
        if self.board_panel_window.is_some() {
            return;
        }
        let port = mcp_panel_port();
        let attrs = WindowAttributes::default()
            .with_title("board")
            .with_theme(Some(Theme::Dark))
            .with_inner_size(LogicalSize::new(320.0, 440.0));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("[board-panel] window create failed: {e}");
                return;
            }
        };
        let html = BOARD_PANEL_HTML.replace("__PORT__", &port);
        // build_as_child for the same use-after-free reason as the git panel.
        let webview = match wry::WebViewBuilder::new()
            .with_html(html)
            .with_bounds(wry::Rect {
                position: wry::dpi::LogicalPosition::new(0.0, 0.0).into(),
                size: wry::dpi::LogicalSize::new(320.0, 440.0).into(),
            })
            .build_as_child(window.as_ref())
        {
            Ok(wv) => wv,
            Err(e) => {
                eprintln!("[board-panel] webview build failed: {e}");
                return;
            }
        };
        eprintln!("[board-panel] open; polling 127.0.0.1:{port}/board");
        self.board_panel_window = Some(window);
        self.board_panel_webview = Some(webview);
    }

    /// Toggle the board panel from the menu: close if open, open if not.
    fn toggle_board_panel(&mut self, event_loop: &ActiveEventLoop) {
        if self.board_panel_window.is_some() {
            // Drop the webview before the window it borrows from.
            self.board_panel_webview = None;
            self.board_panel_window = None;
        } else {
            self.open_board_panel(event_loop);
        }
    }



    /// Effective render scale = DPI scale × whole-UI zoom. Everything that
    /// converts logical↔physical (cell metrics, chrome coords, cursor px,
    /// window→cols) routes through this so a single `ui_zoom` change scales
    /// the entire UI uniformly.
    fn effective_scale(&self) -> f32 {
        let dpi = self
            .window
            .as_ref()
            .map(|w| w.scale_factor() as f32)
            .unwrap_or(1.0);
        dpi * self.ui_zoom
    }

    /// Adjust the whole-UI zoom by `delta` (additive on the multiplier).
    /// Clamped to a sane range; chrome + sidebar + every pane scale together.
    fn change_ui_zoom(&mut self, delta: f32) {
        let new = (self.ui_zoom + delta).clamp(0.5, 3.0);
        if (new - self.ui_zoom).abs() < 0.01 {
            return;
        }
        self.ui_zoom = new;
        self.apply_effective_scale();
    }

    /// Reset whole-UI zoom to native (1.0).
    fn reset_ui_zoom(&mut self) {
        if (self.ui_zoom - 1.0).abs() < 0.01 {
            return;
        }
        self.ui_zoom = 1.0;
        self.apply_effective_scale();
    }

    /// Push the current effective scale into the GPU renderer and reflow the
    /// cell grid + PTY size. Shared by zoom changes and (future) DPI
    /// scale-factor changes when the window moves between monitors.
    fn apply_effective_scale(&mut self) {
        let eff = self.effective_scale();
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.set_scale(eff);
            let (cw, ch) = gpu.set_font_size(self.font_size);
            self.cell = CellGeom { w: cw, h: ch, baseline: 0.0 };
        }
        if self.window.is_some() {
            let (cols, rows) = self.window_cells();
            self.resize_backend(cols, rows);
            self.chrome_dirty = true;
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
        }
    }

    /// Adjust the focused pane's font multiplier (pane-local zoom). Only that
    /// pane's glyphs + PTY grid change; the BSP layout and other panes stay
    /// put. Delta is additive on the multiplier; clamped to a sane range.
    fn change_pane_font(&mut self, delta: f32) {
        let Some(active) = self.target_pane() else { return };
        let cur = self.pane_font_scales.get(&active).copied().unwrap_or(1.0);
        let new = (cur + delta).clamp(0.5, 3.0);
        if (new - cur).abs() < 0.01 {
            return;
        }
        self.pane_font_scales.insert(active, new);
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// Reset the focused pane's font multiplier to match the rest of the UI.
    fn reset_pane_font(&mut self) {
        let Some(active) = self.target_pane() else { return };
        if self.pane_font_scales.remove(&active).is_none() {
            return;
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// True when the cursor block should be visible this frame.
    /// Solid for `BLINK_PAUSE_AFTER_INPUT_MS` after any input event, then
    /// toggles every `BLINK_HALF_PERIOD_MS`.
    fn cursor_blink_on(&self, now: Instant) -> bool {
        // Debug: KASATERM_NOBLINK=1 keeps the cursor solid so a
        // screenshot can verify cursor position/visibility without
        // racing the blink phase.
        if std::env::var_os("KASATERM_NOBLINK").is_some() {
            return true;
        }
        let since_input = now.saturating_duration_since(self.last_input_at);
        if since_input.as_millis() < BLINK_PAUSE_AFTER_INPUT_MS as u128 {
            return true;
        }
        let elapsed = since_input.as_millis() - BLINK_PAUSE_AFTER_INPUT_MS as u128;
        (elapsed / BLINK_HALF_PERIOD_MS as u128) % 2 == 0
    }

    /// "Host modifier" chord that opens the kasaterm shortcut layer
    /// (split / close / focus / copy-paste). macOS conventions reserve
    /// Cmd for this; Windows and Linux terminals overwhelmingly use
    /// Ctrl+Shift instead so Ctrl+letter stays free to deliver control
    /// bytes to the shell.
    fn host_mod(&self) -> bool {
        if cfg!(target_os = "macos") {
            self.modifiers.super_key()
        } else {
            self.modifiers.control_key() && self.modifiers.shift_key()
        }
    }

    /// Secondary modifier that flips a host shortcut into its alternate
    /// behavior (e.g. `Cmd+Shift+D` = stacked split on macOS). The host
    /// chord on Windows/Linux already owns Shift, so Alt fills the same
    /// role there.
    fn host_mod_alt(&self) -> bool {
        if cfg!(target_os = "macos") {
            self.modifiers.shift_key()
        } else {
            self.modifiers.alt_key()
        }
    }

    /// Headless verification: arm a clean exit after KASATERM_AUTOQUIT_MS so a
    /// background run exercises the save-on-exit path (and thus the next
    /// launch's restore). No-op when the env var is unset.
    fn schedule_autoquit(&mut self) {
        let Ok(ms_str) = std::env::var("KASATERM_AUTOQUIT_MS") else { return; };
        let Ok(ms) = ms_str.parse::<u64>() else { return; };
        eprintln!("[autoquit] clean exit in {ms}ms");
        self.autoquit_at = Some(std::time::Instant::now() + std::time::Duration::from_millis(ms));
    }

    fn schedule_autocapture(&self) {
        let Ok(ms_str) = std::env::var("KASATERM_AUTOCAPTURE_MS") else { return; };
        let Ok(ms) = ms_str.parse::<u64>() else { return; };
        let path = std::env::var("KASATERM_AUTOCAPTURE_PATH").unwrap_or_else(|_| {
            std::env::temp_dir()
                .join("tmuxify.png")
                .to_string_lossy()
                .into_owned()
        });
        eprintln!("[autocapture] in {ms}ms → {path}");

        // macOS: derive the capture region from winit's own window geometry
        // (logical px = screen points) so `screencapture -R` is bounded to the
        // window even when osascript / System Events permission is denied. The
        // old AppleScript bounds query returned None in headless runs, which
        // fell back to grabbing the whole desktop.
        #[cfg(target_os = "macos")]
        let region: Option<String> = self.window.as_ref().and_then(|w| {
            let scale = w.scale_factor();
            let pos = w.outer_position().ok()?;
            let size = w.outer_size();
            let x = (pos.x as f64 / scale).round() as i64;
            let y = (pos.y as f64 / scale).round() as i64;
            let ww = (size.width as f64 / scale).round() as i64;
            let hh = (size.height as f64 / scale).round() as i64;
            Some(format!("{x},{y},{ww},{hh}"))
        });

        // Windows: pull HWND on the main thread (raw-window-handle isn't
        // Send), pass the address into the timer thread as isize.
        #[cfg(windows)]
        let hwnd_isize: Option<isize> = self.window.as_ref().and_then(|w| {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            w.window_handle().ok().and_then(|h| match h.as_raw() {
                RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
                _ => None,
            })
        });

        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(ms));

            #[cfg(target_os = "macos")]
            {
                let pid = std::process::id();
                // Force our window to the front so nothing overlaps the region,
                // then capture the winit-derived bounds. Frontmost is best-
                // effort (needs Accessibility permission); the region itself
                // comes from winit, not osascript, so capture works regardless.
                let _ = std::process::Command::new("osascript")
                    .args([
                        "-e",
                        &format!(
                            "tell application \"System Events\" to set frontmost of (first process whose unix id is {pid}) to true"
                        ),
                    ])
                    .status();
                std::thread::sleep(std::time::Duration::from_millis(400));
                let mut cmd = std::process::Command::new("screencapture");
                cmd.args(["-x", "-t", "png"]);
                if let Some(r) = region.as_deref() {
                    cmd.args(["-R", r]);
                }
                cmd.arg(&path);
                let _ = cmd.status();
                eprintln!("[autocapture] captured {path} region={:?}", region);
            }

            #[cfg(windows)]
            {
                let Some(hwnd) = hwnd_isize else {
                    eprintln!("[autocapture] no HWND available");
                    return;
                };
                match capture_window_to_png_windows(hwnd, &path) {
                    Ok((w, h)) => eprintln!("[autocapture] captured {path} ({w}x{h})"),
                    Err(e) => eprintln!("[autocapture] failed: {e}"),
                }
            }
        });
    }

    fn schedule_autosend(&self) {
        let Ok(text) = std::env::var("KASATERM_AUTOSEND") else { return; };
        let ms: u64 = std::env::var("KASATERM_AUTOSEND_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2000);
        eprintln!("[autosend] in {ms}ms: {text:?}");
        // Capture whichever backend is wired so we don't need access
        // to self inside the timer thread.
        let tmux = self.tmux.clone();
        // Autosend always targets the currently-focused pane. In tmux
        // mode we leave pane targeting to the daemon; in pty mode we
        // grab the active session here so the closure doesn't need
        // self access.
        let pty = self.active_pty().cloned();
        let daemon = self.daemon_client.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(ms));
            let mut payload = text.clone();
            if !payload.ends_with('\n') {
                payload.push('\n');
            }
            if let Some(d) = daemon.as_ref() {
                d.send_raw(None, payload.as_bytes());
            } else if let Some(t) = tmux.as_ref() {
                let hex: String = payload
                    .bytes()
                    .map(|b| format!("{b:02x}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                let _ = t.send_keys_hex(None, &hex);
            } else if let Some(p) = pty.as_ref() {
                let _ = p.send_bytes(payload.as_bytes());
            }
        });
    }

    /// Drain a PtySession's screen-update channel into shared workspace
    /// state. Used both by `start_pty` (initial pane) and by
    /// `split_active_pane` (every additional pane), so the per-pane
    /// state arrives through the same path no matter when the session
    /// was spawned.
    /// Apply one decoded ScreenUpdate to the workspace: route to the right
    /// tab, reflow on size change, blit dirty rows, carry cursor/mode/title.
    /// Shared by the in-process channel pump (`pump_pty_screens`) and the
    /// daemon stream pump (`pump_daemon_stream`). The caller holds the ws lock
    /// and fires the redraw; this only mutates ws.
    fn apply_screen_update(ws: &mut Workspace, update: kasa_bridge::screen::ScreenUpdate) {
        if ws.active_pane.is_none() {
            ws.active_pane = Some(update.pane_id.clone());
        }
        // Route the update to the *tab* whose pid matches this stream.
        // Single-tab panes round-trip through the outer id; secondary
        // tabs spawned via the in-pane + button route through
        // `pid_to_pane`. Falls back to creating an outer pane entry
        // when the first update from a freshly-spawned shell arrives.
        let (pane, tab_idx) = match ws.find_tab_by_pty(&update.pane_id) {
            Some(p) => p,
            None => {
                // Brand-new pty id → create the outer PaneState with a
                // single tab that owns this pid. Seed pid_to_pane so
                // subsequent updates hit the O(1) path.
                let pane = ws.pane_mut(&update.pane_id);
                pane.tabs[0].pid = Some(update.pane_id.clone());
                ws.pid_to_pane
                    .insert(update.pane_id.clone(), update.pane_id.clone());
                let pane = ws.panes.get_mut(&update.pane_id).expect("just inserted");
                (pane, 0usize)
            }
        };
        let tab = &mut pane.tabs[tab_idx];
        let tp = tab.term_mut().expect("pty pane must be terminal");
        let resized = tp.cols != update.cols
            || tp.rows != update.rows
            || tp.cells.len() != update.rows as usize;
        if resized {
            // Preserve existing rows / columns through a resize so
            // the user sees their old content during the brief gap
            // between SIGWINCH and the shell's reflowed repaint —
            // otherwise the grid blanks for one frame and the
            // divider drag flickers visibly on every cell crossing.
            // Truncate / extend in place; the shell's subsequent
            // `update.dirty` overwrites the affected rows.
            tp.cols = update.cols;
            tp.rows = update.rows;
            let nr = update.rows as usize;
            let nc = update.cols as usize;
            tp.cells.truncate(nr);
            while tp.cells.len() < nr {
                tp.cells.push(vec![GridCell::blank(); nc]);
            }
            for row in &mut tp.cells {
                row.truncate(nc);
                while row.len() < nc {
                    row.push(GridCell::blank());
                }
            }
            tp.prev_cells.clear();
        }
        for (r, row) in update.dirty {
            if let Some(dst) = tp.cells.get_mut(r as usize) {
                *dst = row;
            }
        }
        // Shift detection on the pty side is retired — alacritty handles
        // scrollback natively via display_offset. Hand-rolled detection
        // breaks scroll-region TUIs (like Claude Code) when they write to sync.
        tp.cursor_row = update.cursor_row;
        tp.cursor_col = update.cursor_col;
        tp.cursor_visible = update.cursor_visible;
        tp.alt_screen = update.alt_screen;
        tp.mouse_enabled = update.mouse_enabled;
        tp.mouse_sgr = update.mouse_sgr;
        tp.app_cursor = update.app_cursor;
        // Carry the OSC 133 prompt-end mark only on frames that
        // actually emitted one; keep the last otherwise so a
        // mid-typing frame doesn't erase it.
        if let Some(pe) = update.prompt_end {
            tp.prompt_end = Some(pe);
        }
        // OSC 0/2 title from the inner program (Claude Code's
        // conversation summary, vim filename, etc.). Pinned panes
        // (renamed via surface.rename / run_job) keep their agent-set
        // label; only unpinned panes track OSC.
        if let Some(t) = update.title.clone() {
            if !tab.title_pinned {
                tab.title = Some(t);
            }
        }
        let _ = tab;
        pane.dirty = true;
    }

    fn pump_pty_screens(
        &self,
        screens: kasa_pty::ScreenReceiver<kasa_bridge::screen::ScreenUpdate>,
        pane_id: String,
    ) {
        let ws_screens = self.ws.clone();
        let win_screens = self.window.clone();
        let dead = self.dead_panes.clone();
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            // winit's `request_redraw` is itself idempotent — repeated
            // calls within one frame coalesce into a single
            // RedrawRequested. The previous code added a 16ms throttle
            // on top of that, which had a sharp edge: a *single*
            // ScreenUpdate (the user hitting space, echoed once by the
            // PTY) that landed inside the 16ms window would be
            // dropped, and nothing would fire the next redraw until
            // the *next* update arrived — which for a space character
            // could be ~never. Result was a ~1s perceived cursor lag
            // after spacebar. Letting winit own the coalescing keeps
            // streaming-burst CPU bounded while making every dirty
            // frame visible.
            while let Ok(mut update) = screens.recv() {
                // EOF sentinel: the PTY reader died (shell/claude exited).
                // The PtySession keeps a Sender alive for scroll/resize, so
                // the channel never closes on its own — without this signal
                // the pane would linger as a zombie. Flag it dead and wake
                // the loop so reap_dead_panes drops it on the next turn.
                if update.eof {
                    dead.lock().unwrap().push(update.pane_id.clone());
                    if let Some(w) = win_screens.as_ref() {
                        w.request_redraw();
                    }
                    let _ = proxy.send_event(UserEvent::Redraw);
                    return;
                }
                // Coalesce: drain every other ScreenUpdate currently sitting
                // in the channel and merge them into one. Scroll inertia /
                // bursty Claude Code output can stuff hundreds of frames in
                // the queue between render cycles; processing each
                // separately means N ws-locks + N redraws + N renders. With
                // the merge we do ONE lock per burst, so direction reversals
                // and other late inputs aren't stuck behind a queue.
                loop {
                    match screens.try_recv() {
                        Ok(next) if !next.eof => {
                            let mut row_map: std::collections::HashMap<u16, Row> =
                                update.dirty.into_iter().collect();
                            for (r, row) in next.dirty {
                                row_map.insert(r, row);
                            }
                            let merged_dirty: Vec<(u16, Row)> =
                                row_map.into_iter().collect();
                            update = kasa_bridge::screen::ScreenUpdate {
                                dirty: merged_dirty,
                                ..next
                            };
                        }
                        Ok(next) => {
                            // EOF mid-burst: handle the current merge then
                            // signal death so reap fires next turn.
                            dead.lock().unwrap().push(next.pane_id.clone());
                            break;
                        }
                        Err(_) => break,
                    }
                }
                let mut ws = ws_screens.lock().unwrap();
                Self::apply_screen_update(&mut ws, update);
                drop(ws);
                if let Some(w) = win_screens.as_ref() {
                    w.request_redraw();
                }
                // Wake the loop even if it's parked on a WaitUntil —
                // request_redraw alone doesn't do that reliably on macOS.
                let _ = proxy.send_event(UserEvent::Redraw);
            }
            // Channel disconnected — the reader thread exited because
            // the PTY hit EOF (shell quit) or errored. Flag this pane
            // for the main thread to remove on its next tick.
            dead.lock().unwrap().push(pane_id);
            if let Some(w) = win_screens.as_ref() {
                w.request_redraw();
            }
            let _ = proxy.send_event(UserEvent::Redraw);
        });
    }

    /// Phase C path. Spawns the shell into a direct PTY (no tmux),
    /// hooks the screens channel into the same per-pane state the
    /// renderer expects. Single-pane MVP — the workspace holds one
    /// PaneState keyed "%0" and the layout is `None` (the render path
    /// falls back to single-pane when no layout has arrived).
    /// Spawn the first shell pane for the *current* (already-cleared) session.
    /// Mirrors start_pty's pane bring-up with a fresh pane id and no socket
    /// (re)init — used by new_session.
    fn spawn_session_pane(&mut self) -> Result<()> {
        let (cols, rows) = self.window_cells();
        let cwd = resolve_initial_cwd();
        let id = format!("%{}", self.next_pane_id);
        self.next_pane_id += 1;
        let session = kasa_pty::PtySession::start(kasa_pty::PtyOptions {
            shell: self.pending_shell.take().or_else(resolve_default_shell),
            cwd,
            cols,
            rows,
            env: Vec::new(),
            pane_id: id.clone(),
            initial_scrollback: Vec::new(),
        })?;
        self.pump_pty_screens(session.screens.clone(), id.clone());
        self.pty.insert(id.clone(), Arc::new(session));
        self.pty_layout = Some(kasa_pty::PtyLayout::single(&id));
        self.ws.lock().unwrap().active_pane = Some(id);
        Ok(())
    }





    /// Create a new window inside the *current* session: stash the visible
    /// window's layout, then bring up a fresh window with a single new pane.
    /// The new pane's PTY joins the session's shared `pty` map and runs in the
    /// same `ws`, so it's a sibling of the existing windows — switching between
    /// them never tears a pane down. Windows are this session's tmux-style
    /// "windows"; the session list one level up is tmux "sessions".
    fn new_window(&mut self) {
        if let Some(client) = self.daemon_client.as_ref() {
            client.new_window();
            return; // daemon creates it + pushes State
        }
        // Active window's slot is None — its layout lives in pty_layout. Park
        // it back into the slot before opening a new window.
        self.windows[self.active_window] = self.pty_layout.take();
        self.windows.push(None);
        self.active_window = self.windows.len() - 1;
        // spawn_session_pane sets pty_layout to a fresh single-pane tree,
        // inserts the PTY into the shared map, and points ws.active_pane at it.
        if let Err(e) = self.spawn_session_pane() {
            eprintln!("[window] new window pane spawn failed: {e:#}");
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// Switch the visible window to `idx` within the current session: park the
    /// visible window's layout, swap the target's in. `pty`/`ws` are shared
    /// across the session's windows, so no PTY is touched — only which BSP tree
    /// the renderer draws. Focus lands on the target window's first pane.
    fn switch_window(&mut self, idx: usize) {
        if let Some(client) = self.daemon_client.as_ref() {
            client.switch_window(idx);
            return; // daemon switches + pushes State
        }
        if idx == self.active_window || idx >= self.windows.len() {
            return;
        }
        if self.windows[idx].is_none() {
            return;
        }
        self.windows[self.active_window] = self.pty_layout.take();
        self.pty_layout = self.windows[idx].take();
        self.active_window = idx;
        // Swapping in a stashed window produces no new PTY output, so nothing
        // would flip a pane's `dirty` and the damage-tracked render would skip
        // the frame — the screen stays on the old window. Mark every leaf of
        // the incoming window dirty (plus chrome for the sidebar highlight) so
        // the next redraw actually repaints.
        let leaves: Vec<String> = self
            .pty_layout
            .as_ref()
            .map(|l| l.leaves().iter().map(|s| s.to_string()).collect())
            .unwrap_or_default();
        if !leaves.is_empty() {
            let mut ws = self.ws.lock().unwrap();
            ws.active_pane = Some(leaves[0].clone());
            for leaf in &leaves {
                if let Some(p) = ws.panes.get_mut(leaf) {
                    p.dirty = true;
                }
            }
        }
        self.chrome_dirty = true;
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        // The sidebar highlight + window body are chrome state. Without
        // flagging chrome_dirty, `about_to_wait` parks on WaitUntil(blink)
        // and the switch only paints on the next blink tick (or not at all
        // if the redraw request is coalesced) — the tab looks unresponsive.
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// Close the window at `idx`. The last window can't be closed (a session
    /// always needs one). Every pane in the closed window is torn down — its
    /// PTY Arc dropped (kills the shell) and its render state removed — same
    /// teardown remove_pane uses. Closing the visible window swaps a neighbor
    /// in so the terminal keeps painting.
    fn close_window(&mut self, idx: usize) -> Result<()> {
        if self.windows.len() <= 1 {
            anyhow::bail!("cannot close the last window");
        }
        if idx >= self.windows.len() {
            anyhow::bail!("no such window: {idx}");
        }
        // Daemon mode: the daemon owns the window tree. Delegate the whole
        // close so it drops the window from its own session and reaps the
        // PTYs. Closing panes one-by-one off our *local* layout drifted from
        // the daemon and left windows that resurrected on the next state push
        // (the window-increment bug). The daemon's broadcast repaints us.
        if let Some(client) = self.daemon_client.clone() {
            client.close_window(idx);
            return Ok(());
        }
        // Pull the closing window's layout (active one lives in pty_layout) and
        // kill every pane it owns.
        let layout = if idx == self.active_window {
            self.pty_layout.take()
        } else {
            self.windows[idx].take()
        };
        if let Some(layout) = layout {
            let mut ws = self.ws.lock().unwrap();
            for pane_id in layout.leaves() {
                self.pty.remove(pane_id);
                ws.panes.remove(pane_id);
            }
        }
        if idx == self.active_window {
            let target = if idx == 0 { 1 } else { idx - 1 };
            self.pty_layout = self.windows[target].take();
            self.windows.remove(idx);
            self.active_window = if target > idx { target - 1 } else { target };
            if let Some(first) = self
                .pty_layout
                .as_ref()
                .and_then(|l| l.leaves().first().map(|s| s.to_string()))
            {
                self.ws.lock().unwrap().active_pane = Some(first);
            }
        } else {
            self.windows.remove(idx);
            if idx < self.active_window {
                self.active_window -= 1;
            }
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
        Ok(())
    }

    /// Refresh the per-window tab labels (window name + cwd). cwd resolution
    /// shells out to `lsof`, so this is throttled to ~1s and also re-runs
    /// whenever the window count changes (new/switch/close). The render path
    /// calls this each frame; the throttle keeps it cheap.
    fn refresh_window_labels(&mut self) {
        let now = Instant::now();
        let fresh = self.window_labels.len() == self.windows.len()
            && self
                .window_labels_at
                .is_some_and(|t| now.duration_since(t).as_millis() < 1000);
        if fresh {
            return;
        }
        let n = self.windows.len();
        let mut out = Vec::with_capacity(n);
        let ws = self.ws.lock().unwrap();
        for i in 0..n {
            // Representative pane = first leaf of the window's layout. The
            // active window's tree lives in pty_layout; the rest in windows[i].
            let repr = {
                let layout = if i == self.active_window {
                    self.pty_layout.as_ref()
                } else {
                    self.windows.get(i).and_then(|o| o.as_ref())
                };
                layout.and_then(|l| l.leaves().first().map(|s| s.to_string()))
            };
            let name = repr
                .as_ref()
                .and_then(|id| {
                    ws.panes
                        .get(id)
                        .and_then(|p| p.title.clone())
                        .filter(|t| !t.is_empty())
                        .or_else(|| {
                            self.pty
                                .get(id)
                                .and_then(|p| p.active_process_name())
                                .filter(|t| !t.is_empty())
                        })
                })
                .unwrap_or_else(|| format!("win {}", i + 1));
            let cwd = repr
                .as_ref()
                .and_then(|id| self.pty.get(id))
                .and_then(|p| p.shell_pid())
                .and_then(socket::pid_cwd)
                .map(|p| Self::shorten_cwd(&p))
                .unwrap_or_default();
            out.push((name, cwd));
        }
        drop(ws);
        self.window_labels = out;
        self.window_labels_at = Some(now);
    }

    /// Window `i`'s representative-pane cwd (first leaf of its layout). Daemon
    /// mode reads the broadcast `pane_cwd_cache`; local mode resolves the shell
    /// pid. Targets the sidebar git-badge poller and the badge lookup at paint.
    fn window_repr_cwd(&self, i: usize) -> Option<std::path::PathBuf> {
        let layout = if i == self.active_window {
            self.pty_layout.as_ref()
        } else {
            self.windows.get(i).and_then(|o| o.as_ref())
        };
        let id = layout.and_then(|l| l.leaves().first().map(|s| s.to_string()))?;
        if let Some(p) = self.pane_cwd_cache.get(&id) {
            return Some(p.clone());
        }
        self.pty
            .get(&id)
            .and_then(|p| p.shell_pid())
            .and_then(socket::pid_cwd)
    }

    /// Compress a cwd for the sidebar: home → `~`, then keep the tail if it
    /// runs past `max` chars so the meaningful (deepest) part stays visible.
    /// 탭/헤더 라벨용. 셸이 idle이면 cwd의 마지막 폴더명, 명령 실행 중이면
    /// 그 프로세스명. zsh 4개로 안 보이고 위치/작업이 드러나게.
    fn smart_pane_label(sess: &kasa_pty::PtySession) -> Option<String> {
        let proc = sess.active_process_name().filter(|t| !t.is_empty());
        let is_shell = proc.as_deref().map_or(false, |p| {
            let base = p.strip_prefix('-').unwrap_or(p);
            matches!(base, "zsh" | "bash" | "fish" | "sh" | "dash" | "tcsh" | "ksh")
        });
        if is_shell {
            sess.shell_pid()
                .and_then(socket::pid_cwd)
                .map(|p| Self::cwd_basename(&p))
                .or(proc)
        } else {
            proc
        }
    }

    /// cwd의 마지막 폴더명. 홈 디렉토리면 `~`.
    fn cwd_basename(p: &std::path::Path) -> String {
        if let Ok(h) = std::env::var("HOME") {
            if !h.is_empty() && p == std::path::Path::new(&h) {
                return "~".to_string();
            }
        }
        p.file_name()
            .and_then(|s| s.to_str())
            .map(str::to_string)
            .unwrap_or_else(|| "/".to_string())
    }

    fn shorten_cwd(p: &std::path::Path) -> String {
        let raw = p.to_string_lossy().to_string();
        let s = match std::env::var("HOME") {
            Ok(h) if !h.is_empty() && raw.starts_with(&h) => format!("~{}", &raw[h.len()..]),
            _ => raw,
        };
        let max = 26usize;
        let chars: Vec<char> = s.chars().collect();
        if chars.len() > max {
            let tail: String = chars[chars.len() - (max - 1)..].iter().collect();
            format!("…{tail}")
        } else {
            s
        }
    }

    /// Refresh the per-pane shell cwd cache that feeds the header breadcrumb.
    /// `pid_cwd` shells out to `lsof`, so resolving it per pane on every frame
    /// would spawn a burst during a scroll/hover storm. Rate-limited to
    /// ~700ms — a breadcrumb only moves on `cd`, so the lag is imperceptible.
    fn refresh_pane_cwds(&mut self) {
        // Daemon-attached mode keeps self.pty empty — the breadcrumb cache is
        // filled from the daemon's StateView instead (see UserEvent::DaemonState).
        // Bail so we never wipe that; only the in-process PTY backend fills
        // self.pty and needs this lsof sweep.
        if self.pty.is_empty() {
            return;
        }
        if let Some(t) = self.pane_cwd_check {
            if t.elapsed() < std::time::Duration::from_millis(700) {
                return;
            }
        }
        self.pane_cwd_check = Some(Instant::now());
        let mut cache = HashMap::new();
        for (id, sess) in &self.pty {
            if let Some(cwd) = sess.shell_pid().and_then(socket::pid_cwd) {
                cache.insert(id.clone(), cwd);
            }
        }
        self.pane_cwd_cache = cache;
    }

    /// Path → breadcrumb segments ("app", "kasaterm", "src"), collapsing the
    /// home prefix to "~". The header joins them with " › " and elides from
    /// the front when space runs out, so the current folder always survives.
    fn breadcrumb_segs(p: &std::path::Path) -> Vec<String> {
        use std::path::Component;
        let mut segs: Vec<String> = Vec::new();
        let home = std::env::var("HOME").ok().filter(|h| !h.is_empty());
        if let Some(h) = &home {
            if let Ok(rest) = p.strip_prefix(h) {
                segs.push("~".to_string());
                for c in rest.components() {
                    if let Component::Normal(s) = c {
                        segs.push(s.to_string_lossy().into_owned());
                    }
                }
                return segs;
            }
        }
        for c in p.components() {
            if let Component::Normal(s) = c {
                segs.push(s.to_string_lossy().into_owned());
            }
        }
        if segs.is_empty() {
            segs.push("/".to_string());
        }
        segs
    }

    /// Recompute the sidebar file tree when its root (the active pane's cwd)
    /// changes — pane switch or `cd`. Cheap string compare per frame; the
    /// read_dir walk only runs on a real change (or after expand/collapse,
    /// which calls `rebuild_file_tree_nodes` directly).
    fn refresh_file_tree(&mut self) {
        let active = self.ws.lock().ok().and_then(|w| w.active_pane.clone());
        let root = active
            .as_ref()
            .and_then(|id| self.pane_cwd_cache.get(id).cloned())
            .or_else(|| std::env::current_dir().ok());
        if root == self.file_tree_root {
            return;
        }
        self.file_tree_root = root;
        self.file_tree_scroll = 0.0;
        self.rebuild_file_tree_nodes();
    }

    /// Walk the root + every expanded folder into the flat `file_tree_nodes`.
    fn rebuild_file_tree_nodes(&mut self) {
        self.file_tree_nodes.clear();
        if let Some(root) = self.file_tree_root.clone() {
            Self::walk_dir(&root, 0, &self.file_tree_expanded, &mut self.file_tree_nodes);
        }
    }

    /// Recursive read_dir: folders first then files (case-insensitive), dotfiles
    /// skipped, descending only into expanded folders.
    fn walk_dir(
        dir: &std::path::Path,
        depth: usize,
        expanded: &std::collections::HashSet<std::path::PathBuf>,
        out: &mut Vec<FileNode>,
    ) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<FileNode> = rd
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') {
                    return None;
                }
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                Some(FileNode { path: e.path(), name, is_dir, depth })
            })
            .collect();
        entries.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        for node in entries {
            let (is_dir, path) = (node.is_dir, node.path.clone());
            out.push(node);
            if is_dir && expanded.contains(&path) {
                Self::walk_dir(&path, depth + 1, expanded, out);
            }
        }
    }

    /// Geometry of the left window-tab sidebar, in logical px. Returns
    /// `(tab_rects, close_rects, plus_rect)`:
    ///   - one `(window_idx, rect)` tab per window, stacked under the title
    ///     strip,
    ///   - one `(window_idx, ×-rect)` per window *only* when more than one
    ///     window exists (the last window can't be closed),
    ///   - the "+" new-window button rect under the last tab.
    /// Pure read of `windows.len()` so the render path and the mouse
    /// hit-test agree on every rect. `win_h` is the logical window height
    /// (unused today but kept so a future scroll/overflow clamp has it).
    fn sidebar_layout(
        &self,
        _win_h: f32,
    ) -> (
        Vec<(usize, (f32, f32, f32, f32))>,
        Vec<(usize, (f32, f32, f32, f32))>,
        (f32, f32, f32, f32),
    ) {
        let n = self.windows.len();
        let tab_x = SIDEBAR_TAB_INSET;
        let tab_w = (self.sidebar_w_logical - 2.0 * SIDEBAR_TAB_INSET).max(0.0);
        let top = TITLE_HEIGHT + 8.0;
        let stride = SIDEBAR_TAB_H + SIDEBAR_TAB_GAP;
        let mut tabs = Vec::with_capacity(n);
        let mut closes = Vec::new();
        for i in 0..n {
            let y = top + i as f32 * stride;
            tabs.push((i, (tab_x, y, tab_w, SIDEBAR_TAB_H)));
            if n > 1 {
                let cs = 14.0;
                closes.push((i, (tab_x + tab_w - cs - 3.0, y + 3.0, cs, cs)));
            }
        }
        let plus_y = top + n as f32 * stride;
        let plus = (tab_x, plus_y, tab_w, 28.0);
        (tabs, closes, plus)
    }

    fn start_pty(&mut self) -> Result<()> {
        let _window = self.window.as_ref().expect("window before pty");
        // In-process PTY 경로 제거 — 데몬 전용. 데몬을 discover/spawn해 attach
        // 하고 PTY를 위임한다(화면은 stream, 입력/resize/scroll은 RPC). attach가
        // 실패하면 그대로 에러 — fallback 없음(데몬 spawn이 보장되어야 함).
        self.attach_daemon()
    }

    /// Daemon-mode startup: discover a running daemon (or spawn one), open the
    /// control + stream sockets, and start rendering the daemon's pane. The
    /// daemon owns the PTY, so this GUI holds no PtySession — input/resize/
    /// scroll go out as RPC, screen frames come in over the stream socket.
    /// Fixed control-socket path for the daemon. Unlike the pid-based
    /// `resolve_kasaterm_socket_path` (per-process cmux socket), this stays
    /// constant across GUI restarts so discovery finds the running daemon.
    /// Deliberately NOT keyed off `KASATERM_SOCKET_PATH` (the per-process cmux
    /// socket a parent kasaterm injects into child shells) so a nested kasaterm
    /// doesn't mistake its parent's socket for the daemon. `KASATERM_DAEMON_
    /// SOCKET` overrides it (tests use this for isolation).
    fn daemon_control_path() -> std::path::PathBuf {
        if let Ok(p) = std::env::var("KASATERM_DAEMON_SOCKET") {
            if !p.is_empty() {
                return std::path::PathBuf::from(p);
            }
        }
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        // A debug build (cargo run / target/debug) runs its own daemon on a
        // separate socket so building & launching a dev kasaterm never hijacks
        // the release .app's daemon — the single-subscriber stream would
        // otherwise steal the working window's screen ("작업 kasaterm 멈춤").
        let sock = if cfg!(debug_assertions) {
            "daemon-dev.sock"
        } else {
            "daemon.sock"
        };
        std::path::PathBuf::from(home).join(".config/kasaterm").join(sock)
    }

    fn attach_daemon(&mut self) -> Result<()> {
        let ctrl_path = Self::daemon_control_path();
        if let Some(parent) = ctrl_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let ctrl = ctrl_path.to_string_lossy().into_owned();
        std::env::set_var("KASATERM_SOCKET_PATH", &ctrl);
        std::env::set_var("CMUX_SOCKET_PATH", &ctrl);
        // Session-panel webview polls 127.0.0.1:<KASASPACE_MCP_PORT>/sessions.
        // A parent in-process kasaterm (.app) injects KASASPACE_MCP_PORT=8765
        // into child shells; if the daemon inherited that it would clash with
        // the parent's server (bind fails) and the panel would show the
        // parent's sessions. Pin a daemon-only port — the spawned daemon
        // inherits this same var and serves /sessions there, and the session
        // panel below reads the same var, so both agree on the daemon.
        // A debug build uses its own port too (it runs its own daemon on
        // daemon-dev.sock): sharing 8766 would clash with the release .app's
        // daemon server (bind fails), undoing the socket split above.
        let mcp_port = if cfg!(debug_assertions) { "8767" } else { "8766" };
        std::env::set_var("KASASPACE_MCP_PORT", mcp_port);
        // Discovery: connect to a live daemon, else spawn one and wait for it.
        // `existing` = we connected to a daemon that was already up (app
        // restart while the daemon survived in the background). A freshly
        // spawned daemon is `false` — it already starts with one empty
        // session, so we leave that as-is.
        let (client, existing) = match stream::DaemonClient::connect(&ctrl_path) {
            Ok(c) => (c, true),
            Err(_) => {
                stream::spawn_daemon(&ctrl_path).map_err(anyhow::Error::from)?;
                if !stream::wait_for_socket(&ctrl_path, std::time::Duration::from_secs(3)) {
                    anyhow::bail!("daemon control socket never came up");
                }
                (
                    stream::DaemonClient::connect(&ctrl_path).map_err(anyhow::Error::from)?,
                    false,
                )
            }
        };
        self.daemon_client = Some(Arc::new(client));
        // The daemon binds the stream socket just after the control socket;
        // retry briefly to dodge that startup gap.
        let spath = stream::stream_path(&ctrl_path);
        let mut conn = None;
        for _ in 0..75 {
            if let Ok(s) = kasa_socket::transport::LocalStream::connect(&spath) {
                conn = Some(s);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
        let conn = conn.ok_or_else(|| anyhow::anyhow!("daemon stream socket unavailable"))?;
        self.pump_daemon_stream(conn, "%0".to_string());
        // Attached to a daemon that was ALREADY running with live sessions
        // (the app restarted while the daemon stayed up). Start a fresh
        // session so the user lands on a clean pane, while the previous
        // sessions keep running in the background and surface in the session
        // panel. The daemon answers new_session() by broadcasting a StateView
        // that the DaemonState handler applies — installing the real layout
        // and resizing every leaf. So we must NOT resize "%0" ourselves here:
        // after new_session, "%0" belongs to a *background* session, and
        // resizing it to this window would corrupt that session's layout.
        if existing {
            if let Some(c) = self.daemon_client.as_ref() {
                c.new_session();
            }
        }
        // Placeholder until the first StateView lands and the DaemonState
        // handler installs the authoritative layout + sizes every leaf.
        self.pty_layout = Some(kasa_pty::PtyLayout::single("%0"));
        self.ws.lock().unwrap().active_pane = Some("%0".to_string());
        Ok(())
    }

    /// Daemon stream pump: decode bincode frames off the stream socket and
    /// apply them through the same `apply_screen_update` path the in-process
    /// pump uses. Mirrors `pump_pty_screens` but reads from the socket.
    fn pump_daemon_stream(&self, conn: kasa_socket::transport::LocalStream, pane_id: String) {
        let ws_screens = self.ws.clone();
        let win_screens = self.window.clone();
        let dead = self.dead_panes.clone();
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let mut conn = conn;
            loop {
                match stream::read_msg(&mut conn) {
                    Ok(Some(stream::StreamMsg::Frame(update))) => {
                        if update.eof {
                            dead.lock().unwrap().push(update.pane_id.clone());
                            let _ = proxy.send_event(UserEvent::Redraw);
                            continue;
                        }
                        let mut ws = ws_screens.lock().unwrap();
                        Self::apply_screen_update(&mut ws, update);
                        drop(ws);
                        if let Some(w) = win_screens.as_ref() {
                            w.request_redraw();
                        }
                        let _ = proxy.send_event(UserEvent::Redraw);
                    }
                    // Structure change (split/close/new/switch on the daemon) —
                    // hand to the main thread, which owns pty_layout + publish.
                    Ok(Some(stream::StreamMsg::State(view))) => {
                        let _ = proxy.send_event(UserEvent::DaemonState(view));
                    }
                    // Clean EOF (daemon gone) or decode error — flag the pane
                    // dead so the main thread reaps it on its next tick.
                    Ok(None) | Err(_) => {
                        dead.lock().unwrap().push(pane_id);
                        if let Some(w) = win_screens.as_ref() {
                            w.request_redraw();
                        }
                        let _ = proxy.send_event(UserEvent::Redraw);
                        return;
                    }
                }
            }
        });
    }



    /// Serialize every session (active + stashed) as a layout tree so the next
    /// launch can restore the full multi-pane, multi-session workspace. Written
    /// on exit by save_session_state.
    fn save_session_state(&self) {
        let mut sessions_json = Vec::new();
        for i in 0..self.sessions.len() {
            // Each session contributes all its windows. The active session's
            // live state is in self.{pty,pty_layout,windows,active_window};
            // stashed sessions carry the same fields on their Session.
            let (pty, active_layout, windows, active_window, ws_arc) = if i == self.active_session {
                (
                    &self.pty,
                    self.pty_layout.as_ref(),
                    &self.windows,
                    self.active_window,
                    &self.ws,
                )
            } else {
                match self.sessions[i].as_ref() {
                    Some(s) => (
                        &s.pty,
                        s.pty_layout.as_ref(),
                        &s.windows,
                        s.active_window,
                        &s.ws,
                    ),
                    None => continue,
                }
            };
            // Lock this session's workspace once so each leaf can read its
            // pane scrollback while serializing the window trees.
            let ws_guard = ws_arc.lock().unwrap();
            // Serialize every window. The active window's tree lives in
            // active_layout; the rest sit in `windows[j]` (active slot None).
            let mut windows_json = Vec::new();
            let mut new_active = 0usize;
            for (j, slot) in windows.iter().enumerate() {
                let layout = if j == active_window {
                    active_layout
                } else {
                    slot.as_ref()
                };
                let Some(layout) = layout else { continue };
                if j == active_window {
                    new_active = windows_json.len();
                }
                windows_json.push(Self::layout_to_json(layout, pty, &ws_guard));
            }
            if windows_json.is_empty() {
                continue;
            }
            sessions_json.push(serde_json::json!({
                "windows": windows_json,
                "active_window": new_active,
            }));
        }
        if sessions_json.is_empty() {
            return;
        }
        let state = serde_json::json!({
            "active_session": self.active_session,
            "sessions": sessions_json,
        });
        socket::write_session_state(&state);
    }

    /// Walk a live PtyLayout into the nested JSON the restore loader reads,
    /// resolving each leaf's pane id to its cwd/claude record.
    fn layout_to_json(
        layout: &kasa_pty::PtyLayout,
        pty: &HashMap<String, Arc<kasa_pty::PtySession>>,
        ws: &Workspace,
    ) -> serde_json::Value {
        match layout {
            kasa_pty::PtyLayout::Leaf { pane_id } => {
                let mut rec = pty
                    .get(pane_id)
                    .map(|s| socket::pane_record(s))
                    .unwrap_or(serde_json::Value::Null);
                // Attach the pane's scrollback (text lines) so restore can
                // repaint what was on screen. Only when we have a real record.
                if let Some(obj) = rec.as_object_mut() {
                    let sb = ws
                        .panes
                        .get(pane_id)
                        .map(scrollback_lines)
                        .unwrap_or_default();
                    obj.insert("scrollback".to_string(), serde_json::json!(sb));
                }
                serde_json::json!({ "leaf": rec })
            }
            kasa_pty::PtyLayout::Split { dir, ratio, a, b } => {
                let dir = match dir {
                    kasa_pty::SplitDir::Horizontal => "h",
                    kasa_pty::SplitDir::Vertical => "v",
                };
                serde_json::json!({ "split": {
                    "dir": dir,
                    "ratio": ratio,
                    "a": Self::layout_to_json(a, pty, ws),
                    "b": Self::layout_to_json(b, pty, ws),
                }})
            }
        }
    }

    /// Headless verification helper. Reads `KASATERM_AUTOSPLIT` ("h" / "v"
    /// / "hv" / "vh" ...) and fires the matching splits from
    /// `about_to_wait` after `KASATERM_AUTOSPLIT_MS` (default 2500ms),
    /// so a background `cargo run` can prove multi-pane rendering
    /// without a human pressing Cmd+D.
    fn run_pending_autosplits(&mut self) {
        if self.autosplit_plan.is_empty() {
            return;
        }
        let now = Instant::now();
        let due = match self.autosplit_at {
            Some(t) => t,
            None => return,
        };
        if now < due {
            return;
        }
        let dir = self.autosplit_plan.remove(0);
        if let Err(e) = self.split_active_pane(dir) {
            eprintln!("[autosplit] split failed: {e}");
        }
        // Chain the next split 500ms later so the renderer has time to
        // settle and a screenshot can capture intermediate states.
        self.autosplit_at = if self.autosplit_plan.is_empty() {
            None
        } else {
            Some(now + std::time::Duration::from_millis(500))
        };
    }

    /// Headless repro for the window sidebar: spawn KASATERM_AUTOWINDOWS extra
    /// windows, one every 600ms, so a screenshot can capture the multi-tab
    /// sidebar without a human pressing Cmd+T.
    fn run_pending_autowindows(&mut self) {
        if self.autowindow_left == 0 {
            return;
        }
        let now = Instant::now();
        let Some(due) = self.autowindow_at else { return };
        if now < due {
            return;
        }
        self.new_window();
        self.autowindow_left -= 1;
        self.autowindow_at = if self.autowindow_left == 0 {
            None
        } else {
            Some(now + std::time::Duration::from_millis(600))
        };
    }

    fn arm_autowindows(&mut self) {
        let Ok(n_str) = std::env::var("KASATERM_AUTOWINDOWS") else { return };
        let Ok(n) = n_str.parse::<usize>() else { return };
        if n == 0 {
            return;
        }
        let ms: u64 = std::env::var("KASATERM_AUTOWINDOWS_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2500);
        eprintln!("[autowindow] armed: {n} window(s) in {ms}ms");
        self.autowindow_left = n;
        self.autowindow_at = Some(Instant::now() + std::time::Duration::from_millis(ms));
    }

    fn run_pending_autotoggle(&mut self) {
        let Some(due) = self.autotoggle_sidebar_at else { return };
        if Instant::now() < due {
            return;
        }
        self.toggle_sidebar();
        eprintln!(
            "[autotoggle] flipped → visible={} remaining={}",
            self.sidebar_visible, self.autotoggle_left
        );
        if self.autotoggle_left > 0 {
            self.autotoggle_left -= 1;
            self.autotoggle_sidebar_at =
                Some(Instant::now() + std::time::Duration::from_millis(1500));
        } else {
            self.autotoggle_sidebar_at = None;
        }
    }

    fn arm_autotoggle(&mut self) {
        let Ok(ms_str) = std::env::var("KASATERM_AUTOTOGGLE_SIDEBAR_MS") else { return };
        let Ok(ms) = ms_str.parse::<u64>() else { return };
        self.autotoggle_left = std::env::var("KASATERM_AUTOTOGGLE_SIDEBAR_N")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        eprintln!("[autotoggle] sidebar flip in {ms}ms (repeat={})", self.autotoggle_left);
        self.autotoggle_sidebar_at = Some(Instant::now() + std::time::Duration::from_millis(ms));
    }

    fn arm_autosplit(&mut self) {
        let Ok(plan) = std::env::var("KASATERM_AUTOSPLIT") else { return; };
        let ms: u64 = std::env::var("KASATERM_AUTOSPLIT_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2500);
        let dirs: Vec<kasa_pty::SplitDir> = plan
            .chars()
            .filter_map(|c| match c {
                'h' | 'H' => Some(kasa_pty::SplitDir::Horizontal),
                'v' | 'V' => Some(kasa_pty::SplitDir::Vertical),
                _ => None,
            })
            .collect();
        if dirs.is_empty() {
            return;
        }
        eprintln!("[autosplit] armed: {plan:?} in {ms}ms");
        self.autosplit_plan = dirs;
        self.autosplit_at = Some(Instant::now() + std::time::Duration::from_millis(ms));
    }

    /// Headless cross-pane tab-merge simulation. Reads
    /// KASATERM_AUTODRAG="src:from:dst" (e.g. "%2:0:%0") and fires
    /// `simulate_tab_merge` after KASATERM_AUTODRAG_MS (default 5500).
    fn arm_autodrag(&mut self) {
        let Ok(env) = std::env::var("KASATERM_AUTODRAG") else { return };
        let parts: Vec<&str> = env.split(':').collect();
        if parts.len() < 3 {
            eprintln!("[autodrag] expected src:from:dst, got {env:?}");
            return;
        }
        let from: usize = parts[1].parse().unwrap_or(0);
        let ms: u64 = std::env::var("KASATERM_AUTODRAG_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(5500);
        self.autodrag_plan = Some((parts[0].to_string(), from, parts[2].to_string()));
        self.autodrag_at = Some(Instant::now() + std::time::Duration::from_millis(ms));
        eprintln!("[autodrag] armed: src={} from={} dst={} fire_in={ms}ms",
            parts[0], from, parts[2]);
    }

    fn run_pending_autodrag(&mut self) {
        let Some(t) = self.autodrag_at else { return };
        if Instant::now() < t { return; }
        self.autodrag_at = None;
        let Some((src, from, dst)) = self.autodrag_plan.take() else { return };
        self.simulate_tab_merge(&src, from, &dst);
    }

    /// Pane header centre in logical px, mirroring `drop_target_at`'s box
    /// expansion. Used by `simulate_tab_merge` to land the synthetic
    /// cursor exactly where a user would aim "drop on header band".
    fn pane_header_center(&self, id: &str) -> Option<(f32, f32)> {
        let tree = self.pty_layout.as_ref()?;
        let leaves = tree.leaves().len();
        let (cols, rows) = self.window_cells();
        let rects = tree.leaf_rects(cols, rows);
        let pad = WINDOW_PADDING + self.effective_sidebar_w();
        let header_band = if leaves > 1 { PANE_HEADER_HEIGHT } else { 0.0 };
        let (_, cx, cy, cw, _) = rects.into_iter().find(|(i, ..)| i == id)?;
        let bx = pad + cx as f32 * self.cell.w;
        let by = TITLE_HEIGHT + cy as f32 * self.cell.h;
        let bw = cw as f32 * self.cell.w;
        Some((bx + bw / 2.0, by - header_band / 2.0))
    }

    /// Simulate dragging `src.tabs[from]` onto `dst`'s header. Mirrors the
    /// release-handler's cross_pane merge branch so we can verify the
    /// path without a real mouse. Logs to stderr.
    fn simulate_tab_merge(&mut self, src: &str, from: usize, dst: &str) {
        let Some((mx, my)) = self.pane_header_center(dst) else {
            eprintln!("[autodrag] no rect for dst={dst}");
            return;
        };
        eprintln!("[autodrag] simulate src={src} from={from} dst={dst} mouse=({mx:.0},{my:.0})");
        let mut moved_pid: Option<String> = None;
        let mut moved: Option<PaneTab> = None;
        let mut src_empty = false;
        {
            let mut ws = self.ws.lock().unwrap();
            if let Some(s) = ws.panes.get_mut(src) {
                if from < s.tabs.len() {
                    let tab = s.tabs.remove(from);
                    moved_pid = tab.pid.clone();
                    moved = Some(tab);
                    if s.active_tab >= s.tabs.len() && !s.tabs.is_empty() {
                        s.active_tab = s.tabs.len() - 1;
                    }
                    src_empty = s.tabs.is_empty();
                    s.dirty = true;
                }
            }
            if let (Some(tab), Some(pid)) = (moved.take(), moved_pid.clone()) {
                ws.pid_to_pane.insert(pid, dst.to_string());
                if let Some(d) = ws.panes.get_mut(dst) {
                    let to = d.tabs.len();
                    d.tabs.insert(to, tab);
                    d.active_tab = to;
                    d.dirty = true;
                }
            }
            if src_empty {
                ws.panes.remove(src);
            }
        }
        if src_empty {
            self.collapse_layout_only(src);
        }
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        let dst_tabs = self.ws.lock().unwrap()
            .panes.get(dst).map(|p| p.tabs.len()).unwrap_or(0);
        eprintln!("[autodrag] done; src_empty={src_empty} dst_tabs={dst_tabs}");
    }

    /// Headless repro for the in-pane tab header: queue N dummy tabs on the
    /// active pane KASATERM_AUTOTABS_MS (default 3200, after autosplit) later.
    fn arm_autotabs(&mut self) {
        let Ok(n_str) = std::env::var("KASATERM_AUTOTABS") else { return };
        let Ok(n) = n_str.parse::<usize>() else { return };
        if n == 0 {
            return;
        }
        let ms: u64 = std::env::var("KASATERM_AUTOTABS_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(3200);
        eprintln!("[autotabs] armed: {n} tab(s) in {ms}ms");
        self.autotabs_n = n;
        self.autotabs_at = Some(Instant::now() + std::time::Duration::from_millis(ms));
    }

    fn run_pending_autotabs(&mut self) {
        if self.autotabs_n == 0 {
            return;
        }
        let Some(due) = self.autotabs_at else { return };
        if Instant::now() < due {
            return;
        }
        let n = self.autotabs_n;
        // Spawn N real PTY-backed tabs so the headless verify cycle exercises
        // the stage-3 path (each tab has its own shell behind it). Falls back
        // to dummy label-only tabs if the spawn fails (e.g. tmux mode).
        let active = self.ws.lock().unwrap().active_pane.clone();
        if let Some(outer) = active {
            for i in 1..=n {
                if self.spawn_new_tab(&outer).is_err() {
                    if let Some(pane) = self.ws.lock().unwrap().panes.get_mut(&outer) {
                        let mut t = PaneTab::default();
                        t.title = Some(format!("탭 {}", i + 1));
                        pane.tabs.push(t);
                        pane.dirty = true;
                    }
                }
            }
            if let Some(pane) = self.ws.lock().unwrap().panes.get_mut(&outer) {
                pane.active_tab = 0;
                pane.dirty = true;
            }
        }
        eprintln!("[autotabs] added {n} tab(s) to active pane");
        self.autotabs_n = 0;
        self.autotabs_at = None;
        self.chrome_dirty = true;
    }

    fn start_tmux(&mut self) -> Result<()> {
        let _window = self.window.as_ref().expect("window before tmux");
        let (cols, rows) = self.window_cells();
        let cwd = resolve_initial_cwd();
        let tmux = TmuxSession::start(StartOptions {
            cwd: cwd.as_deref(),
            socket_name: Some("kasaterm"),
            cols,
            rows,
            ..Default::default()
        })?;
        // Screens thread: each ScreenUpdate carries a pane_id; routes to
        // the matching PaneState in the workspace. New pane ids appear
        // automatically when tmux split-window creates them.
        let screens = tmux.screens.clone();
        let ws_screens = self.ws.clone();
        let win_screens = self.window.clone();
        std::thread::spawn(move || {
            while let Ok(ScreenUpdate {
                pane_id,
                rows,
                cols,
                dirty,
                cursor_row,
                cursor_col,
                cursor_visible,
                alt_screen,
                mouse_enabled,
                mouse_sgr,
                title,
                ..
            }) = screens.recv()
            {
                let mut ws = ws_screens.lock().unwrap();
                // First-seen pane becomes the active one so the user
                // doesn't open into a workspace with no focus.
                if ws.active_pane.is_none() {
                    ws.active_pane = Some(pane_id.clone());
                }
                let is_active = ws.active_pane.as_deref() == Some(pane_id.as_str());
                let pane = ws.pane_mut(&pane_id);
                let tp = pane.term_mut().expect("tmux pane must be terminal");
                let resized = tp.cols != cols
                    || tp.rows != rows
                    || tp.cells.len() != rows as usize;
                if resized {
                    // Preserve content across resize — see the PTY-path
                    // copy of this branch for the rationale.
                    tp.cols = cols;
                    tp.rows = rows;
                    let nr = rows as usize;
                    let nc = cols as usize;
                    tp.cells.truncate(nr);
                    while tp.cells.len() < nr {
                        tp.cells.push(vec![GridCell::blank(); nc]);
                    }
                    for row in &mut tp.cells {
                        row.truncate(nc);
                        while row.len() < nc {
                            row.push(GridCell::blank());
                        }
                    }
                    tp.prev_cells.clear();
                }
                for (r, row) in dirty {
                    if let Some(dst) = tp.cells.get_mut(r as usize) {
                        *dst = row;
                    }
                }
                // Shift detection per pane — alt-screen apps manage their
                // own scrollback so we skip there.
                if !alt_screen
                    && !tp.prev_cells.is_empty()
                    && tp.prev_cells.len() == tp.cells.len()
                {
                    let n = tp.prev_cells.len();
                    let mut shifted = 0usize;
                    for k in 1..n {
                        if tp.prev_cells[k..] == tp.cells[..n - k] {
                            shifted = k;
                            break;
                        }
                    }
                    if shifted > 0 {
                        for row in &tp.prev_cells[..shifted] {
                            tp.history.push_back(row.clone());
                        }
                        while tp.history.len() > SCROLLBACK_MAX {
                            tp.history.pop_front();
                        }
                    }
                }
                tp.prev_cells = tp.cells.clone();
                tp.cursor_row = cursor_row;
                tp.cursor_col = cursor_col;
                tp.cursor_visible = cursor_visible;
                tp.alt_screen = alt_screen;
                tp.mouse_enabled = mouse_enabled;
                tp.mouse_sgr = mouse_sgr;
                let new_title = title.filter(|t| !t.is_empty());
                // Pinned panes (renamed via surface.rename / run_job) ignore
                // OSC titles so the agent-set label stays put.
                let title_changed = !pane.title_pinned && pane.title != new_title;
                if title_changed {
                    pane.title = new_title.clone();
                }
                drop(ws);
                if let Some(w) = win_screens.as_ref() {
                    // Only the active pane's title shows in the window
                    // chrome — background panes change silently.
                    if title_changed && is_active {
                        let display =
                            new_title.unwrap_or_else(|| "kasaterm".into());
                        w.set_title(&display);
                    }
                    w.request_redraw();
                }
            }
        });
        // Events thread: parses %layout-change messages so render_frame
        // can lay panes out. Without this, splits would create panes
        // we have screen state for but no rect to draw them at.
        let events = tmux.events.clone();
        let ws_events = self.ws.clone();
        let win_events = self.window.clone();
        std::thread::spawn(move || {
            while let Ok(evt) = events.recv() {
                match evt {
                    TmuxEvent::LayoutChange { layout, .. } => {
                        // tmux's %layout-change emits both the visible
                        // and default layouts in one message,
                        // space-separated, plus a trailing flag.
                        // parse_layout wants exactly one layout
                        // string, so take the first token.
                        let first = layout
                            .split_whitespace()
                            .next()
                            .unwrap_or(&layout);
                        match parse_layout(first) {
                            Ok(parsed) => {
                                let mut ws = ws_events.lock().unwrap();
                                ws.layout = Some(parsed);
                                drop(ws);
                                if let Some(w) = win_events.as_ref() {
                                    w.request_redraw();
                                }
                            }
                            Err(e) => {
                                eprintln!("[layout] parse failed: {e} ({first:?})");
                            }
                        }
                    }
                    TmuxEvent::WindowPaneChanged { pane_id, .. } => {
                        // tmux flipped the active pane (most commonly:
                        // a split-window just landed and the new pane
                        // grabbed focus). Mirror that into our state
                        // so the cursor + active border + outgoing key
                        // target all move together.
                        let mut ws = ws_events.lock().unwrap();
                        if ws.active_pane.as_deref() != Some(pane_id.as_str()) {
                            ws.active_pane = Some(pane_id);
                            drop(ws);
                            if let Some(w) = win_events.as_ref() {
                                w.request_redraw();
                            }
                        }
                    }
                    _ => {}
                }
            }
        });
        let tmux_arc = Arc::new(tmux);
        self.tmux = Some(tmux_arc.clone());
        self.start_socket_tmux(tmux_arc);
        Ok(())
    }

    /// Bring up the cmux-compatible JSON-RPC server so external agents
    /// (Claude Code teammateMode, ad-hoc CLI scripts) can drive this
    /// pane. The server is best-effort — a bind failure logs and the
    /// rest of the binary keeps working without it. Two env names are
    /// exported on the spawned shell:
    ///   - KASATERM_SOCKET_PATH (our brand)
    ///   - CMUX_SOCKET_PATH (so cmux-aware clients auto-detect us)
    /// Both point at the same socket; the second is the cmux-protocol
    /// convention from issue anthropics/claude-code#36926.
    /// Bind the unix socket + export env vars. Common to both backend
    /// modes — the caller decides which concrete `Backend` impl to plug
    /// in (TmuxBackend in tmux mode, PtyBackend in PTY mode).
    fn start_socket_with(&self, backend: Arc<dyn kasa_socket::Backend>) {
        // Model-invoked tools for the claude running inside a pane: the
        // same Backend, exposed over MCP-on-HTTP. Replaces the external
        // python bridge (mcp/kasa_mcp.py).
        match kasa_mcp::spawn_http_server(backend.clone(), 8765) {
            Ok(port) => {
                eprintln!("[kasaspace-mcp] HTTP MCP on 127.0.0.1:{port}/mcp");
                std::env::set_var("KASASPACE_MCP_PORT", port.to_string());
                let _ = std::fs::write(mcp_port_file_path(), port.to_string());
                // No MCP auto-discovery: write our address into each AI
                // client's config so any agent on this machine finds us.
                kasa_mcp::register_clients(port);
            }
            Err(e) => eprintln!("[kasaspace-mcp] HTTP MCP start failed: {e}"),
        }
        let path = resolve_kasaterm_socket_path();
        let server = match kasa_socket::Server::bind(&path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[agent-socket] bind {path:?} failed: {e:#}");
                return;
            }
        };
        let resolved = server.socket_path().to_string_lossy().to_string();
        eprintln!("[agent-socket] listening on {resolved}");
        std::env::set_var("KASATERM_SOCKET_PATH", &resolved);
        std::env::set_var("CMUX_SOCKET_PATH", &resolved);
        let _join = server.spawn(backend);
    }

    fn start_socket_tmux(&self, tmux: Arc<kasa_bridge::TmuxSession>) {
        self.start_socket_with(Arc::new(socket::TmuxBackend::new(tmux)));
    }







    /// Convert logical-pixel position into a (pane_id, col, row) cell
    /// inside the pane the click landed in. Multi-pane aware: walks the
    /// parsed Layout to find the pane whose rect contains the click,
    /// then translates the pixel into that pane's cell-local coords.
    /// Returns None when the workspace has no panes or the click missed
    /// every pane (gutter between split borders, padding, etc).
    fn px_to_pane_cell(&self, px: f32, py: f32) -> Option<(String, u16, u16)> {
        let sb = self.effective_sidebar_w();
        let ws = self.ws.lock().unwrap();
        if let Some(layout) = ws.layout.as_ref() {
            let split = layout.leaves().len() > 1;
            let header_h = if split { PANE_HEADER_HEIGHT } else { 0.0 };
            // Box hit-test runs in whole-grid cells (header included, no
            // inset) so a click anywhere in the pane box selects it.
            let gcol = ((px - sb - WINDOW_PADDING).max(0.0) / self.cell.w).floor() as i32;
            // Render shifts every split pane down by the header band (origin_y
            // += header_shift, see render_frame_gpu). The box hit-test must
            // subtract the same band, or the lower pane's rows map ~one header
            // above where they're actually drawn — clicks / scroll there miss
            // the pane entirely.
            let grow = ((py - TITLE_HEIGHT - header_h).max(0.0) / self.cell.h).floor() as i32;
            for leaf in layout.leaves() {
                if let Layout::Pane { id, x, y, w, h } = leaf {
                    let (bx, by, bw, bh) = (*x as i32, *y as i32, *w as i32, *h as i32);
                    if gcol >= bx && gcol < bx + bw && grow >= by && grow < by + bh {
                        // Local cell uses the body origin: box edge + header
                        // band + inner inset, matching the render origin.
                        let pid = format!("%{id}");
                        // Per-pane font zoom: glyphs render at cell × fs, so the
                        // pixel→cell divisor must use the same zoomed cell or a
                        // font-bumped pane maps the cursor to the wrong row/col
                        // (selection + mouse-report drift). The box origin stays
                        // on the shared grid — only the in-pane step scales.
                        let fs = self
                            .pane_font_scales
                            .get(&pid)
                            .copied()
                            .unwrap_or(1.0)
                            .max(0.1);
                        let box_left = sb + WINDOW_PADDING + bx as f32 * self.cell.w;
                        let box_top = TITLE_HEIGHT + by as f32 * self.cell.h;
                        let lc = ((px - box_left - PANE_INNER_X).max(0.0) / (self.cell.w * fs))
                            .floor() as u16;
                        let lr = ((py - box_top - header_h - PANE_INNER_Y).max(0.0)
                            / (self.cell.h * fs))
                            .floor() as u16;
                        let (mc, mr) = ws
                            .panes
                            .get(&pid)
                            .and_then(|p| p.term())
                            .map_or((lc, lr), |t| {
                                (
                                    lc.min(t.cols.saturating_sub(1)),
                                    lr.min(t.rows.saturating_sub(1)),
                                )
                            });
                        return Some((pid, mc, mr));
                    }
                }
            }
            return None;
        }
        // No layout yet — single pane fills the window (inset only).
        let id = ws.active_pane.clone().or_else(|| ws.panes.keys().next().cloned())?;
        let pane = ws.panes.get(&id)?;
        let t = pane.term()?;
        if t.cols == 0 || t.rows == 0 {
            return None;
        }
        let fs = self.pane_font_scales.get(&id).copied().unwrap_or(1.0).max(0.1);
        let lc = ((px - sb - WINDOW_PADDING - PANE_INNER_X).max(0.0) / (self.cell.w * fs))
            .floor() as u16;
        let lr =
            ((py - TITLE_HEIGHT - PANE_INNER_Y).max(0.0) / (self.cell.h * fs)).floor() as u16;
        Some((id, lc.min(t.cols - 1), lr.min(t.rows - 1)))
    }

    /// Convenience wrapper that returns only the active pane's local
    /// cell coords. Most callers (wheel, selection drag) only care
    /// about the active pane.
    fn px_to_cell_active(&self, px: f32, py: f32) -> Option<(u16, u16)> {
        let (pane_id, col, row) = self.px_to_pane_cell(px, py)?;
        let ws = self.ws.lock().unwrap();
        let active_match = ws.active_pane.as_deref() == Some(pane_id.as_str());
        active_match.then_some((col, row))
    }

    /// Target pane for outgoing key/text. When the workspace has an
    /// active pane, we name it explicitly so tmux doesn't fall back to
    /// "last-active" semantics that disagree with our UI.
    fn target_pane(&self) -> Option<String> {
        self.ws.lock().unwrap().active_pane.clone()
    }

    /// Surface id that should receive keyboard input — the active pane's
    /// *active tab*'s pid, not the outer pane id. `target_pane()` returns
    /// the layout key (== first tab's pid), so once the user switches tabs
    /// the daemon keeps routing keystrokes to the first tab. The daemon's
    /// PTY map is keyed by tab pid, so input must name the active tab
    /// explicitly. Falls back to the outer id for single-tab / tmux panes
    /// whose tabs carry no explicit pid (same fallback as `active_pty`).
    fn target_surface(&self) -> Option<String> {
        let ws = self.ws.lock().ok()?;
        let outer = ws.active_pane.clone()?;
        let pid = ws
            .panes
            .get(&outer)
            .and_then(|p| p.tabs.get(p.active_tab).and_then(|t| t.pid.clone()))
            .unwrap_or(outer);
        Some(pid)
    }

    /// The PtySession that currently has keyboard focus, if any. Used
    /// by every routing-by-active-pane code path in PTY mode.
    /// PtySession of a pane's currently-active tab. Use this instead of
    /// `self.pty.get(outer_id)` — after a cross-pane tab drag the layout
    /// id and the active tab's pid diverge, and the direct lookup misses.
    /// Drives wheel scroll / mouse-reporting / pane-targeted send_bytes.
    fn pty_for_pane(&self, outer_id: &str) -> Option<&Arc<kasa_pty::PtySession>> {
        let ws = self.ws.lock().ok()?;
        let pid = ws
            .panes
            .get(outer_id)
            .and_then(|p| p.tabs.get(p.active_tab).and_then(|t| t.pid.clone()))
            .unwrap_or_else(|| outer_id.to_string());
        drop(ws);
        self.pty.get(&pid)
    }

    fn active_pty(&self) -> Option<&Arc<kasa_pty::PtySession>> {
        // The active *tab*'s pid drives input/scroll/title — falling back
        // to the outer pane id (== first-tab pid) for single-tab panes
        // whose tabs haven't been initialised with an explicit pid yet
        // (e.g. tmux-mode panes, where the outer key is what `pty` keys on).
        let ws = self.ws.lock().unwrap();
        let outer = ws.active_pane.clone()?;
        let pid = ws
            .panes
            .get(&outer)
            .and_then(|p| p.tabs.get(p.active_tab).and_then(|t| t.pid.clone()))
            .unwrap_or(outer);
        drop(ws);
        self.pty.get(&pid)
    }

    /// Window size in cell coordinates. Source of truth for resize
    /// distribution + new-pane sizing. The grid lives inside
    /// `WINDOW_PADDING` on every side, so subtract 2× padding from the
    /// logical viewport before dividing — otherwise we tell the PTY it
    /// has N rows but only N-1 fit before clipping, and the last row
    /// (where most TUIs paint their statusline) gets cut in half.
    /// Falls back to (80, 24) when the window isn't ready yet.
    fn window_cells(&self) -> (u16, u16) {
        let Some(window) = self.window.as_ref() else {
            return (80, 24);
        };
        let size = window.inner_size();
        let scale = self.effective_scale();
        let raw_lw = size.width as f32 / scale;
        let raw_lh = size.height as f32 / scale;
        let lw = (raw_lw
            - self.effective_sidebar_w()
            - self.effective_right_chrome_w()
            - 2.0 * WINDOW_PADDING)
            .max(0.0);
        // Top: TITLE_HEIGHT (chrome strip). Bottom: WINDOW_PADDING. The
        // asymmetry is intentional — the strip replaces the top padding.
        // Reserve the dock bar from the grid only when it carries chips.
        let dock = if self.docked.is_empty() { 0.0 } else { DOCK_HEIGHT };
        let lh = (raw_lh - TITLE_HEIGHT - WINDOW_PADDING - dock).max(0.0);
        let cols = (lw / self.cell.w).floor().max(40.0) as u16;
        let rows = (lh / self.cell.h).floor().max(10.0) as u16;
        if std::env::var_os("KASATERM_LOG_LAYOUT").is_some() {
            eprintln!(
                "[layout] win=({raw_lw:.0}x{raw_lh:.0}) usable=({lw:.0}x{lh:.0}) cell=({:.1}x{:.1}) cells=({cols}x{rows})",
                self.cell.w, self.cell.h
            );
        }
        (cols, rows)
    }

    /// Push the current PtyLayout into `ws.layout` so the renderer
    /// (which only knows the tmux Layout shape) picks up the splits.
    /// A single-leaf tree leaves `ws.layout` empty — the render path's
    /// single-pane fallback handles that case.
    fn publish_pty_layout(&self) {
        if let Some(tree) = self.pty_layout.as_ref() {
            let (cols, rows) = self.window_cells();
            let mut ws = self.ws.lock().unwrap();
            if tree.leaves().len() <= 1 {
                ws.layout = None;
            } else {
                ws.layout = Some(tree.to_tmux_layout(cols, rows));
            }
        }
        // Keep the socket snapshot in lockstep with the renderer view —
        // every code path that adds/removes panes or moves focus goes
        // through publish_pty_layout, so this is the one spot we have
        // to wire the cmux mirror.
    }

    /// Resize every backend session so its grid matches the new window
    /// size. In tmux mode the daemon redistributes for us. In PTY mode
    /// we walk the BSP tree and SIGWINCH each leaf to its own rect.
    fn resize_backend(&self, cols: u16, rows: u16) {
        if let Some(tmux) = self.tmux.as_ref() {
            let _ = tmux.resize_client(cols, rows);
            return;
        }
        // The window is the single source of truth for size. Derive every
        // leaf's usable grid from the window cell box here, then push those
        // sizes to whoever owns the PTY — the daemon over RPC, or local
        // sessions. Panes themselves only carry BSP ratios, never absolute
        // rows/cols, so this one computation feeds both backends.
        let Some(tree) = self.pty_layout.as_ref() else {
            // No layout yet (very first daemon frame): the lone pane fills
            // the whole window.
            if let Some(client) = self.daemon_client.as_ref() {
                client.resize("%0", cols, rows);
            }
            return;
        };
        // When the workspace is split, every pane wears a per-pane header
        // strip that eats a few cell rows off the top of its box, so the
        // PTY's usable grid shrinks by the same amount — otherwise claude
        // code paints its statusline / `bypass…` row off the bottom edge.
        let leaves = tree.leaves().len();
        let header_px = if leaves > 1 { PANE_HEADER_HEIGHT } else { 0.0 };
        // Per-pane font scale shrinks/grows that pane's usable cells: bigger
        // glyphs ⇒ fewer cols/rows in the same box. 1.0 panes keep the exact
        // integer-cell math; scaled panes divide the base cell span by the
        // factor (the box stays on the base grid, matching the per-slot render
        // which sizes glyphs by the same factor). Keyed by pty/leaf id.
        let scale_of = self.pane_font_scales.clone();
        let cw = self.cell.w.max(1.0);
        let ch = self.cell.h.max(1.0);
        let mut leaf_cells: HashMap<String, (u16, u16)> = HashMap::new();
        for (id, _x, _y, w, h) in self.effective_leaf_rects(cols, rows) {
            let fs = scale_of.get(&id).copied().unwrap_or(1.0).max(0.1);
            // Work in logical px on the BASE grid — exactly the span the
            // renderer fills (origin + w·cell), then subtract the real px
            // insets/header and divide by the ZOOMED cell. The old path
            // rounded the inset to whole base cells and divided by fs, so a
            // shrunk pane (small fs) amplified that ceil error ∝ 1/fs and
            // told the PTY a grid that no longer matched the drawn area —
            // that's the "비율 안 맞음" past a certain zoom-out.
            let box_w_px = w as f32 * cw;
            let box_h_px = h as f32 * ch;
            let scaled_cw = cw * fs;
            let scaled_ch = ch * fs;
            let usable_w = (box_w_px - 2.0 * PANE_INNER_X).max(scaled_cw);
            let usable_h = (box_h_px - header_px - 2.0 * PANE_INNER_Y).max(scaled_ch);
            let pcols = (usable_w / scaled_cw).floor().max(1.0) as u16;
            let prows = (usable_h / scaled_ch).floor().max(1.0) as u16;
            leaf_cells.insert(id, (pcols, prows));
        }
        // Daemon owns the PTYs: the layout leaf id IS the pane id, so resize
        // each leaf directly. This replaces the old M1 stub that only ever
        // resized "%0" to the full window — which left freshly split panes
        // stuck at their 80×24 spawn size and clipped claude's bottom rows.
        if let Some(client) = self.daemon_client.as_ref() {
            for (id, (pc, pr)) in &leaf_cells {
                client.resize(id, *pc, *pr);
            }
            self.publish_pty_layout();
            return;
        }
        // Local mode: the outer pane id (layout key) is NOT guaranteed to
        // equal any tab's pid after a cross-pane drag, so map each outer
        // rect onto its tabs' PtySessions — works for primary + in-pane
        // secondary tabs alike.
        let snapshot: Vec<(String, Vec<String>)> = {
            let ws = self.ws.lock().unwrap();
            ws.panes
                .iter()
                .map(|(outer, p)| {
                    let pids: Vec<String> =
                        p.tabs.iter().filter_map(|t| t.pid.clone()).collect();
                    (outer.clone(), pids)
                })
                .collect()
        };
        for (outer, pids) in snapshot {
            let Some(&(pc, pr)) = leaf_cells.get(&outer) else { continue };
            for pid in pids {
                if let Some(sess) = self.pty.get(&pid) {
                    let _ = sess.resize(pc, pr);
                }
            }
        }
        // Re-publish the layout because rect proportions may have
        // shifted (rounding) and the renderer caches the previous tree.
        self.publish_pty_layout();
    }

    /// If the cursor (logical px) rests on a split seam, return the BSP
    /// tree path of that split plus its axis. A few px of tolerance makes
    /// the thin seam easy to grab. None when not over any divider.
    fn divider_at_px(&self, x: f32, y: f32) -> Option<(Vec<u8>, kasa_pty::SplitDir)> {
        let tree = self.pty_layout.as_ref()?;
        if tree.leaves().len() <= 1 {
            return None;
        }
        let (cols, rows) = self.window_cells();
        let pad = WINDOW_PADDING + self.effective_sidebar_w();
        let tol = 6.0_f32;
        for d in tree.dividers(cols, rows) {
            match d.dir {
                kasa_pty::SplitDir::Horizontal => {
                    let seam_x = pad + d.edge as f32 * self.cell.w;
                    let y0 = TITLE_HEIGHT + d.span_start as f32 * self.cell.h;
                    let y1 = y0 + d.span_len as f32 * self.cell.h;
                    if (x - seam_x).abs() <= tol && y >= y0 && y <= y1 {
                        return Some((d.path, d.dir));
                    }
                }
                kasa_pty::SplitDir::Vertical => {
                    let seam_y = TITLE_HEIGHT + d.edge as f32 * self.cell.h;
                    let x0 = pad + d.span_start as f32 * self.cell.w;
                    let x1 = x0 + d.span_len as f32 * self.cell.w;
                    if (y - seam_y).abs() <= tol && x >= x0 && x <= x1 {
                        return Some((d.path, d.dir));
                    }
                }
            }
        }
        None
    }

    /// Split the focused pane in PTY mode. Spawns a new shell into a
    /// fresh PTY, inserts it into the BSP tree on the right (Horizontal)
    /// or bottom (Vertical) of the focused leaf, then resizes every
    /// session so each one matches its new rect. Becomes a no-op in
    /// tmux mode — splits there go through the cmux socket / tmux
    /// `split-window` instead.
    fn split_active_pane(&mut self, dir: kasa_pty::SplitDir) -> Result<()> {
        if let Some(client) = self.daemon_client.as_ref() {
            // Daemon owns the layout: it spawns the pane, splits its tree, and
            // pushes the new Layout back (applied in user_event). Sync the
            // daemon's active pane to ours first so the split lands on the
            // pane the user is actually focused on.
            if let Some(id) = self.ws.lock().unwrap().active_pane.clone() {
                client.focus(&id);
            }
            client.split_dir(dir);
            return Ok(());
        }
        if self.tmux.is_some() {
            return Ok(());
        }
        let Some(active) = self.ws.lock().unwrap().active_pane.clone() else {
            return Ok(());
        };
        let new_id = format!("%{}", self.next_pane_id);
        self.next_pane_id += 1;

        // Spawn the new session at a placeholder size — the resize
        // pass right after `split_leaf` puts every leaf at its real
        // rect, so the initial cols/rows here only matters for the
        // first bytes the shell prints before SIGWINCH lands.
        let (win_cols, win_rows) = self.window_cells();
        let cwd = resolve_initial_cwd();
        let session = kasa_pty::PtySession::start(kasa_pty::PtyOptions {
            shell: resolve_default_shell(),
            cwd,
            cols: win_cols,
            rows: win_rows,
            env: Vec::new(),
            pane_id: new_id.clone(),
            initial_scrollback: Vec::new(),
        })?;
        self.pump_pty_screens(session.screens.clone(), new_id.clone());
        self.pty.insert(new_id.clone(), Arc::new(session));

        let layout = self.pty_layout.as_mut().expect("pty_layout set in start_pty");
        if !layout.split_leaf(&active, dir, new_id.clone()) {
            // Active pane isn't in the tree — shouldn't happen, but
            // bail without leaking the spawned session entry.
            self.pty.remove(&new_id);
            self.next_pane_id -= 1;
            return Ok(());
        }
        self.ws.lock().unwrap().active_pane = Some(new_id);
        self.resize_backend(win_cols, win_rows);
        self.publish_pty_layout();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        Ok(())
    }

    /// Stage-3 in-pane tab spawn. Creates a fresh PtySession with its own
    /// pid, registers it in `pid_to_pane` so output streams find the right
    /// (outer pane, tab) pair, and appends a `PaneTab` whose `pid` points at
    /// the new shell. The new tab becomes active. Outer pane id and layout
    /// don't change — adding a tab never reshapes the BSP tree.
    fn spawn_new_tab(&mut self, outer: &str) -> Result<()> {
        if self.tmux.is_some() {
            anyhow::bail!("in-pane tabs not supported on tmux backend");
        }
        // Outer pane must already exist in the layout (it's the user's focused
        // pane). Use its size for the initial pty so the shell starts at the
        // right cols/rows — `resize_backend` after re-applies it anyway, but a
        // sane initial size keeps the welcome banner from wrapping weird.
        let (cols, rows) = self.pane_cells(outer).unwrap_or_else(|| self.window_cells());
        let cwd = resolve_initial_cwd();
        let new_pid = format!("%{}", self.next_pane_id);
        self.next_pane_id += 1;
        let session = kasa_pty::PtySession::start(kasa_pty::PtyOptions {
            shell: resolve_default_shell(),
            cwd,
            cols,
            rows,
            env: Vec::new(),
            pane_id: new_pid.clone(),
            initial_scrollback: Vec::new(),
        })?;
        self.pump_pty_screens(session.screens.clone(), new_pid.clone());
        self.pty.insert(new_pid.clone(), Arc::new(session));
        {
            let mut ws = self.ws.lock().unwrap();
            ws.pid_to_pane.insert(new_pid.clone(), outer.to_string());
            if let Some(pane) = ws.panes.get_mut(outer) {
                let mut tab = PaneTab::default();
                tab.pid = Some(new_pid.clone());
                pane.tabs.push(tab);
                pane.active_tab = pane.tabs.len() - 1;
                pane.dirty = true;
            }
        }
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        Ok(())
    }

    /// Cell extent of `outer` inside the current `pty_layout`. Used by
    /// `spawn_new_tab` to size a brand-new shell at the pane's real bounds.
    /// Returns `None` when the layout is in single-pane fallback or the id
    /// isn't a leaf.
    fn pane_cells(&self, outer: &str) -> Option<(u16, u16)> {
        let (cols, rows) = self.window_cells();
        let tree = self.pty_layout.as_ref()?;
        for (id, _x, _y, w, h) in tree.leaf_rects(cols, rows) {
            if id == outer {
                return Some((w.max(1), h.max(1)));
            }
        }
        None
    }

    /// Leaf rects for render / hit-test / resize, honoring a tmux-style zoom.
    /// When a pane is zoomed it fills the whole work area and the others are
    /// hidden; the daemon's layout tree is untouched (zoom is GUI-local render
    /// state). If the zoomed pane is gone (closed or moved out by a broadcast)
    /// this falls back to the real layout, so a stale zoom never paints a
    /// phantom pane.
    fn effective_leaf_rects(&self, cols: u16, rows: u16) -> Vec<(String, u16, u16, u16, u16)> {
        if let Some(z) = self.zoomed_pane.as_ref() {
            if let Some(tree) = self.pty_layout.as_ref() {
                if tree.leaves().iter().any(|l| *l == z.as_str()) {
                    return vec![(z.clone(), 0, 0, cols, rows)];
                }
            }
        }
        self.pty_layout
            .as_ref()
            .map(|t| t.leaf_rects(cols, rows))
            .unwrap_or_default()
    }

    /// Toggle tmux-style zoom on `pane`: zoom fills the work area with just that
    /// pane; toggling again (or the pane already being zoomed) restores the
    /// split. Reflows the backend so the PTY matches its new extent.
    fn toggle_pane_zoom(&mut self, pane: &str) {
        if self.zoomed_pane.as_deref() == Some(pane) {
            self.zoomed_pane = None;
        } else {
            self.zoomed_pane = Some(pane.to_string());
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// Drop a non-primary tab: kill its PTY, remove the pid map entry, drop
    /// the slot. The primary tab (index 0, pid == outer pane id) can't be
    /// closed this way — callers fall through to `remove_pane` for that.
    fn close_tab(&mut self, outer: &str, idx: usize) {
        let (pid_opt, preview_opt): (Option<String>, Option<String>) = {
            let ws = self.ws.lock().unwrap();
            let tab = ws.panes.get(outer).and_then(|p| p.tabs.get(idx));
            (
                tab.and_then(|t| t.pid.clone()),
                tab.and_then(|t| t.preview_id.clone()),
            )
        };
        if let Some(pid) = pid_opt.as_deref() {
            if pid != outer {
                // Secondary tab — drop its session entry; reader thread sees
                // the channel close and pushes EOF to `dead_panes`, but with
                // the pid_to_pane entry gone the reap pass routes through
                // remove_pane(pid) which is a no-op (pty already gone). Fine.
                self.pty.remove(pid);
                self.ws.lock().unwrap().pid_to_pane.remove(pid);
            }
        }
        // Host-attached preview tab: tell the daemon to forget the preview, else
        // the next broadcast's reconcile re-adds the tab. The local removal below
        // is immediate feedback; the daemon drop keeps it from resurrecting.
        if let Some(prev_id) = preview_opt {
            if let Some(client) = self.daemon_client.clone() {
                client.close(&prev_id);
            }
        }
        let mut ws = self.ws.lock().unwrap();
        if let Some(pane) = ws.panes.get_mut(outer) {
            if idx < pane.tabs.len() {
                pane.tabs.remove(idx);
            }
            if idx < pane.active_tab {
                pane.active_tab -= 1;
            }
            if pane.active_tab >= pane.tabs.len() {
                pane.active_tab = pane.tabs.len() - 1;
            }
            pane.dirty = true;
        }
    }



    /// Insert text at the active markdown editor's cursor (committed Hangul or
    /// pasted text). Multi-char safe; advances the cursor by char count.
    fn md_editor_insert(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let mut ws = self.ws.lock().unwrap();
        let Some(pane) = ws.active_mut() else { return };
        pane.dirty = true;
        let Some(m) = pane.markdown_mut() else { return };
        if m.edit_lines.is_empty() {
            m.edit_lines.push(String::new());
        }
        let line = m.cur_line.min(m.edit_lines.len() - 1);
        let col = m.cur_col;
        let s = &mut m.edit_lines[line];
        let b = char_byte(s, col);
        s.insert_str(b, text);
        m.cur_line = line;
        m.cur_col = col + text.chars().count();
    }

    /// Raw-editor key entry point with Hangul composition. macOS hands jamo
    /// (U+3130..318F) through `event.text`; we feed the local composer (same as
    /// the terminal path), insert committed syllables, and keep the preedit in
    /// `self.preedit` for the editor overlay. Non-jamo flushes then edits.
    fn md_editor_input(&mut self, event: &KeyEvent) {
        use winit::keyboard::{Key, NamedKey};
        #[cfg(target_os = "macos")]
        if let Some(t) = &event.text {
            if t.chars().count() == 1 {
                if let Some(c) = t.chars().next() {
                    if (0x3130..=0x318F).contains(&(c as u32)) {
                        if let Some(commit) = self.hangul.feed(c) {
                            self.md_editor_insert(&commit);
                        }
                        self.preedit = self.hangul.preedit().unwrap_or_default();
                        self.in_preedit = !self.preedit.is_empty();
                        self.chrome_dirty = true;
                        return;
                    }
                }
            }
        }
        // Mid-composition backspace chips a jamo off the preedit.
        if matches!(event.logical_key, Key::Named(NamedKey::Backspace)) && self.hangul.backspace() {
            self.preedit = self.hangul.preedit().unwrap_or_default();
            self.in_preedit = !self.preedit.is_empty();
            self.chrome_dirty = true;
            return;
        }
        // Any other key: flush the pending syllable into the buffer first.
        if let Some(flushed) = self.hangul.flush() {
            self.md_editor_insert(&flushed);
        }
        self.preedit.clear();
        self.in_preedit = false;
        self.md_editor_key(event);
    }

    /// Handle a keypress in a Raw-mode markdown editor pane: char insert,
    /// backspace, enter (line split), arrow navigation. Hangul composition is
    /// handled by `md_editor_input` before this. Edits the active pane buffer.
    fn md_editor_key(&mut self, event: &KeyEvent) {
        use winit::keyboard::{Key, NamedKey};
        let mut ws = self.ws.lock().unwrap();
        let Some(pane) = ws.active_mut() else { return };
        pane.dirty = true;
        let Some(m) = pane.markdown_mut() else { return };
        if m.edit_lines.is_empty() {
            m.edit_lines.push(String::new());
        }
        let mut line = m.cur_line.min(m.edit_lines.len() - 1);
        let mut col = m.cur_col;
        match &event.logical_key {
            Key::Named(NamedKey::Backspace) => {
                if col > 0 {
                    let s = &mut m.edit_lines[line];
                    let b0 = char_byte(s, col - 1);
                    let b1 = char_byte(s, col);
                    s.replace_range(b0..b1, "");
                    col -= 1;
                } else if line > 0 {
                    let cur = m.edit_lines.remove(line);
                    line -= 1;
                    col = m.edit_lines[line].chars().count();
                    m.edit_lines[line].push_str(&cur);
                }
            }
            Key::Named(NamedKey::Enter) => {
                let s = &mut m.edit_lines[line];
                let b = char_byte(s, col);
                let rest = s.split_off(b);
                m.edit_lines.insert(line + 1, rest);
                line += 1;
                col = 0;
            }
            Key::Named(NamedKey::ArrowLeft) => {
                if col > 0 {
                    col -= 1;
                } else if line > 0 {
                    line -= 1;
                    col = m.edit_lines[line].chars().count();
                }
            }
            Key::Named(NamedKey::ArrowRight) => {
                let len = m.edit_lines[line].chars().count();
                if col < len {
                    col += 1;
                } else if line + 1 < m.edit_lines.len() {
                    line += 1;
                    col = 0;
                }
            }
            Key::Named(NamedKey::ArrowUp) => {
                if line > 0 {
                    line -= 1;
                    col = col.min(m.edit_lines[line].chars().count());
                }
            }
            Key::Named(NamedKey::ArrowDown) => {
                if line + 1 < m.edit_lines.len() {
                    line += 1;
                    col = col.min(m.edit_lines[line].chars().count());
                }
            }
            Key::Named(NamedKey::Space) => {
                let s = &mut m.edit_lines[line];
                let b = char_byte(s, col);
                s.insert(b, ' ');
                col += 1;
            }
            Key::Character(txt) => {
                let s = &mut m.edit_lines[line];
                let b = char_byte(s, col);
                s.insert_str(b, txt);
                col += txt.chars().count();
            }
            _ => {}
        }
        m.cur_line = line;
        m.cur_col = col;
    }

    /// Drain `dead_panes` and remove each from the BSP tree + pty map.
    /// Called on the main thread from `about_to_wait` so the mutation
    /// runs without competing with the per-session reader threads.
    /// If removing all panes empties the tree, exit the event loop.
    fn reap_dead_panes(&mut self, event_loop: &ActiveEventLoop) {
        let ids: Vec<String> = std::mem::take(&mut *self.dead_panes.lock().unwrap());
        if ids.is_empty() {
            return;
        }
        for id in ids {
            if !self.pty.contains_key(&id) {
                continue;
            }
            self.remove_pane(&id);
        }
        // Last pane closed (e.g. user typed `exit` in the only shell):
        // shut the window so tmuxify exits cleanly the way users
        // expect from a regular terminal. NOT in daemon mode: there self.pty
        // is always empty (the daemon owns the PTYs), so a closed pane's eof
        // frame lands in dead_panes and this would quit the whole app on every
        // window/pane close. The daemon always keeps a pane alive (close spawns
        // a fresh shell when it'd empty out), so the GUI never self-exits —
        // quit goes through Cmd+Q / the menu.
        if self.tmux.is_none() && self.pty.is_empty() && self.daemon_client.is_none() {
            event_loop.exit();
        }
    }

    /// Drag a single-tab pane onto its own body half. Spawns a fresh shell
    /// next to `source` on the side OPPOSITE the drop, so the original
    /// pane visually "lands" on the side the user threw it to. Distinct
    /// from `drop_tab_into_body` (which lifts a tab into a new pane on the
    /// drop side) — this one keeps the source intact and adds a sibling.
    fn split_pane_opposite(&mut self, source: &str, zone: DropZone) -> Result<()> {
        if self.tmux.is_some() {
            anyhow::bail!("split via drag unsupported on tmux backend");
        }
        // Daemon mode: the daemon owns PTYs/layout. A local PTY spawn + local
        // tree mutation desyncs — the next State overwrites it and the pane goes
        // dead. Delegate: focus source, split along the drop axis; the daemon
        // spawns the pane and broadcasts the new layout back.
        if let Some(client) = self.daemon_client.clone() {
            if matches!(zone, DropZone::Center) {
                return Ok(());
            }
            client.focus(source);
            let dir = match zone {
                DropZone::Left | DropZone::Right => kasa_pty::SplitDir::Horizontal,
                _ => kasa_pty::SplitDir::Vertical,
            };
            client.split_dir(dir);
            return Ok(());
        }
        let (cols, rows) = self.pane_cells(source).unwrap_or_else(|| self.window_cells());
        let cwd = resolve_initial_cwd();
        let new_id = format!("%{}", self.next_pane_id);
        self.next_pane_id += 1;
        let session = kasa_pty::PtySession::start(kasa_pty::PtyOptions {
            shell: resolve_default_shell(),
            cwd,
            cols,
            rows,
            env: Vec::new(),
            pane_id: new_id.clone(),
            initial_scrollback: Vec::new(),
        })?;
        self.pump_pty_screens(session.screens.clone(), new_id.clone());
        self.pty.insert(new_id.clone(), Arc::new(session));
        // `before=true` means the new leaf becomes the LEFT/TOP child, so
        // the source ends up on the RIGHT/BOTTOM. We want source on the
        // dropped side → new on the opposite side.
        let (dir, before) = match zone {
            DropZone::Right => (kasa_pty::SplitDir::Horizontal, true),
            DropZone::Left => (kasa_pty::SplitDir::Horizontal, false),
            DropZone::Down => (kasa_pty::SplitDir::Vertical, true),
            DropZone::Up => (kasa_pty::SplitDir::Vertical, false),
            // Center is handled by the caller as a tab merge — splitting
            // would lose the "drop into this pane's tabs" intent.
            DropZone::Center => return Ok(()),
        };
        let inserted = self
            .pty_layout
            .as_mut()
            .map(|t| t.insert_beside(source, dir, before, new_id.clone()))
            .unwrap_or(false);
        if !inserted {
            // Source vanished mid-drag — bail and clean up the spawned shell.
            self.pty.remove(&new_id);
            self.next_pane_id -= 1;
            return Ok(());
        }
        let (win_cols, win_rows) = self.window_cells();
        self.resize_backend(win_cols, win_rows);
        self.publish_pty_layout();
        // Focus the freshly-spawned pane so the user is typing into it.
        self.ws.lock().unwrap().active_pane = Some(new_id);
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        Ok(())
    }

    /// Tab drag dropped onto another pane's BODY. Splits the target pane
    /// in the matching quadrant and makes the moved tab the new leaf — the
    /// dragged shell now lives in its own pane next to `target`. Unifies
    /// the old "drag pane header" semantics into the tab drag so there's
    /// one drop UX.
    fn drop_tab_into_body(&mut self, td: &TabDrag, target: &str, zone: DropZone) {
        // 1. Lift the tab out of source.
        let (moved, src_empty): (Option<PaneTab>, bool) = {
            let mut ws = self.ws.lock().unwrap();
            let Some(src) = ws.panes.get_mut(&td.pane) else { return };
            if td.from >= src.tabs.len() { return }
            let t = src.tabs.remove(td.from);
            if td.from < src.active_tab && src.active_tab > 0 {
                src.active_tab -= 1;
            }
            if src.active_tab >= src.tabs.len() && !src.tabs.is_empty() {
                src.active_tab = src.tabs.len() - 1;
            }
            src.dirty = true;
            let empty = src.tabs.is_empty();
            (Some(t), empty)
        };
        let Some(moved) = moved else { return };
        // 2. If source emptied, drop it from layout (PtySession survives —
        //    it's the very shell we're about to re-attach as a new leaf).
        if src_empty {
            self.ws.lock().unwrap().panes.remove(&td.pane);
            self.collapse_layout_only(&td.pane);
        }
        // 3. Allocate a fresh layout id for the new pane. Layout ids and
        //    pty ids decoupled from stage-3 onward, so this avoids any
        //    clash with the moved tab's pid (which may have been the old
        //    source's outer id).
        let new_outer = format!("%{}", self.next_pane_id);
        self.next_pane_id += 1;
        let (dir, before) = match zone {
            DropZone::Left => (kasa_pty::SplitDir::Horizontal, true),
            DropZone::Right => (kasa_pty::SplitDir::Horizontal, false),
            DropZone::Up => (kasa_pty::SplitDir::Vertical, true),
            DropZone::Down => (kasa_pty::SplitDir::Vertical, false),
            // Caller routes Center to the cross-pane tab-merge path; if it
            // slips through here, abort the split so we don't double-spawn.
            DropZone::Center => return,
        };
        if let Some(tree) = self.pty_layout.as_mut() {
            if !tree.insert_beside(target, dir, before, new_outer.clone()) {
                // Target gone — fall back to inserting at the first leaf.
                if let Some(anchor) = tree.leaves().first().map(|s| s.to_string()) {
                    tree.insert_beside(&anchor, dir, before, new_outer.clone());
                }
            }
        } else {
            self.pty_layout = Some(kasa_pty::PtyLayout::single(&new_outer));
        }
        // 4. Build the new PaneState with the moved tab as its only tab.
        let moved_pid = moved.pid.clone();
        {
            let mut ws = self.ws.lock().unwrap();
            let mut ps = PaneState::default();
            ps.tabs.clear();
            ps.tabs.push(moved);
            ps.active_tab = 0;
            ps.dirty = true;
            ws.panes.insert(new_outer.clone(), ps);
            if let Some(pid) = moved_pid {
                // Rebind the pid map so future ScreenUpdates / find_tab_by_pty
                // route to new_outer even when pid != new_outer.
                ws.pid_to_pane.insert(pid, new_outer.clone());
            }
            ws.active_pane = Some(new_outer.clone());
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Cross-pane drag aftermath. The source pane lost every tab to dest;
    /// we just need its layout slot gone — *not* the PtySession (now owned
    /// by dest under the same pid key) or the image / markdown caches the
    /// moved tabs depend on. Picks a survivor focus exactly like
    /// `remove_pane` so the chrome doesn't blink to "no active".
    fn collapse_layout_only(&mut self, target: &str) {
        let leaves: Vec<String> = match self.pty_layout.as_ref() {
            Some(t) => t.leaves().iter().map(|s| s.to_string()).collect(),
            None => return,
        };
        let was_active = self
            .ws
            .lock()
            .unwrap()
            .active_pane
            .as_deref()
            .map(|a| a == target)
            .unwrap_or(false);
        let next_focus: Option<String> = if was_active && leaves.len() > 1 {
            let cur_idx = leaves.iter().position(|l| l == target).unwrap_or(0);
            Some(if cur_idx + 1 < leaves.len() {
                leaves[cur_idx + 1].clone()
            } else {
                leaves[cur_idx - 1].clone()
            })
        } else {
            None
        };
        if leaves.len() > 1 {
            if let Some(tree) = self.pty_layout.as_mut() {
                tree.remove_leaf(target);
            }
        } else {
            self.pty_layout = None;
        }
        {
            let mut ws = self.ws.lock().unwrap();
            ws.panes.remove(target);
            ws.rebuild_pid_map();
            if was_active && next_focus.is_some() {
                ws.active_pane = next_focus;
            }
            for pane in ws.panes.values_mut() {
                pane.dirty = true;
            }
        }
        self.chrome_dirty = true;
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Internal: drop a pane regardless of whether it's the active one.
    /// Used by both `close_active_pane` (Cmd+W) and `reap_dead_panes`
    /// (shell exit). Picks a survivor focus when removing the focused
    /// pane.
    fn remove_pane(&mut self, target: &str) {
        let was_active = self
            .ws
            .lock()
            .unwrap()
            .active_pane
            .as_deref()
            .map(|a| a == target)
            .unwrap_or(false);
        let leaves: Vec<String> = match self.pty_layout.as_ref() {
            Some(t) => t.leaves().iter().map(|s| s.to_string()).collect(),
            None => return,
        };
        let next_focus: Option<String> = if was_active && leaves.len() > 1 {
            let cur_idx = leaves.iter().position(|l| l == target).unwrap_or(0);
            if cur_idx + 1 < leaves.len() {
                Some(leaves[cur_idx + 1].clone())
            } else {
                Some(leaves[cur_idx - 1].clone())
            }
        } else {
            None
        };
        if leaves.len() > 1 {
            if let Some(tree) = self.pty_layout.as_mut() {
                tree.remove_leaf(target);
            }
        } else {
            // Last leaf — drop the tree entirely so single-pane
            // fallback re-engages if a future split repopulates it.
            self.pty_layout = None;
        }
        self.pty.remove(target);
        // Free the GPU texture if this was an image pane (no-op otherwise).
        if let Some(g) = self.gpu.as_mut() {
            g.drop_image(target);
        }
        self.md_content_h.remove(target);
        // Drop secondary-tab ptys hosted by this pane and prune the reverse
        // map. Without this, an in-pane tab's shell would linger past its
        // container pane and `find_tab_by_pty` would point at a dead outer.
        let secondary_pids: Vec<String> = {
            let ws = self.ws.lock().unwrap();
            ws.pid_to_pane
                .iter()
                .filter_map(|(pid, outer)| (outer == target).then(|| pid.clone()))
                .collect()
        };
        for pid in &secondary_pids {
            self.pty.remove(pid);
        }
        {
            let mut ws = self.ws.lock().unwrap();
            ws.panes.remove(target);
            ws.rebuild_pid_map();
            if was_active {
                ws.active_pane = next_focus;
            }
            // Layout shrank — every survivor needs a repaint, else the render
            // loop sees pane.dirty=false and skips the GPU pass, leaving the
            // closed pane's slot blank until the next dirty signal.
            for pane in ws.panes.values_mut() {
                pane.dirty = true;
            }
        }
        self.chrome_dirty = true;
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Daemon-mode pane close shared by Cmd+W and the header × button so the
    /// two never diverge. Single pane (only leaf in the only window) → ghostty:
    /// flag the GUI window to exit and let the daemon keep the session alive on
    /// disk for next-launch restore (a daemon close here would fresh-spawn a
    /// replacement shell and desync the GUI). Otherwise delegate the close to
    /// the daemon, which removes the pane and broadcasts the layout back.
    fn daemon_close_pane(&mut self, pid: &str) {
        let leaves = self.pty_layout.as_ref().map_or(0, |t| t.leaves().len());
        if leaves <= 1 {
            // Window's last pane → don't dock (that would leave an empty
            // window). The whole session's last pane (single window) → ghostty
            // window-close, the daemon keeps the session for next-launch
            // restore; otherwise close just this window's pane.
            if self.windows.len() <= 1 {
                self.should_exit = true;
            } else if let Some(client) = self.daemon_client.as_ref() {
                client.close(pid);
            }
        } else if let Some(client) = self.daemon_client.as_ref() {
            client.dock(pid); // split state → fold into the dock (kill-free)
        }
    }

    /// Remove the focused pane from the BSP tree and drop its PTY
    /// session. Focus moves to the next pane in document order
    /// (wrapping to the previous when we just closed the last one).
    /// Last-pane close is a no-op — quitting the window is the
    /// user's exit there.
    fn close_active_pane(&mut self) {
        if self.daemon_client.is_some() {
            // Bind first so the MutexGuard drops before daemon_close_pane takes
            // &mut self (else the lock holds an immutable borrow across it).
            let active = self.ws.lock().unwrap().active_pane.clone();
            if let Some(id) = active {
                self.daemon_close_pane(&id);
            }
            return;
        }
        if self.tmux.is_some() {
            return;
        }
        let Some(active) = self.ws.lock().unwrap().active_pane.clone() else {
            return;
        };
        // Last-pane Cmd+W is a no-op — the OS close button is how
        // users quit a single-pane window. The shell-exit path takes
        // care of the cascade close when the last shell `exit`s.
        let leaves = match self.pty_layout.as_ref() {
            Some(t) => t.leaves().len(),
            None => 0,
        };
        if leaves <= 1 {
            return;
        }
        self.remove_pane(&active);
    }

    /// Cycle focus to the previous (delta=-1) or next (delta=+1) pane
    /// in document order. No-op when there's only one pane.
    fn cycle_focus(&self, delta: i32) {
        let Some(tree) = self.pty_layout.as_ref() else { return; };
        let leaves: Vec<String> = tree.leaves().iter().map(|s| s.to_string()).collect();
        if leaves.len() < 2 {
            return;
        }
        let mut ws = self.ws.lock().unwrap();
        let cur_idx = ws
            .active_pane
            .as_deref()
            .and_then(|id| leaves.iter().position(|l| l == id))
            .unwrap_or(0);
        let n = leaves.len() as i32;
        let new_idx = ((cur_idx as i32 + delta).rem_euclid(n)) as usize;
        let new_active = leaves[new_idx].clone();
        ws.active_pane = Some(new_active.clone());
        drop(ws);
        // Sync the daemon's active pointer (see body-click note) — else a
        // cwd poll reverts this keyboard focus on the next `cd`.
        if let Some(client) = self.daemon_client.as_ref() {
            client.focus(&new_active);
        }
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Pane whose rectangle lies immediately in `dir` of the active pane
    /// and overlaps it on the perpendicular axis. Picks the nearest by
    /// centre distance so a tall neighbour split into several panes still
    /// resolves to the one the user is pointing at. None when there is no
    /// pane on that side.
    fn adjacent_pane(&self, dir: FocusDir) -> Option<String> {
        let tree = self.pty_layout.as_ref()?;
        let (cols, rows) = self.window_cells();
        let rects = tree.leaf_rects(cols, rows);
        if rects.len() < 2 {
            return None;
        }
        let active = self.ws.lock().unwrap().active_pane.clone()?;
        let cur = rects.iter().find(|(id, ..)| id == &active)?;
        let (cx, cy, cw, ch) = (cur.1 as f32, cur.2 as f32, cur.3 as f32, cur.4 as f32);
        let (acx, acy) = (cx + cw / 2.0, cy + ch / 2.0);
        let mut best: Option<(String, f32)> = None;
        for (id, x, y, w, h) in &rects {
            if id == &active {
                continue;
            }
            let (x, y, w, h) = (*x as f32, *y as f32, *w as f32, *h as f32);
            let overlap_y = y < cy + ch && y + h > cy;
            let overlap_x = x < cx + cw && x + w > cx;
            let ok = match dir {
                FocusDir::Left => x + w <= cx + 1.0 && overlap_y,
                FocusDir::Right => x >= cx + cw - 1.0 && overlap_y,
                FocusDir::Up => y + h <= cy + 1.0 && overlap_x,
                FocusDir::Down => y >= cy + ch - 1.0 && overlap_x,
            };
            if !ok {
                continue;
            }
            let dist = (x + w / 2.0 - acx).abs() + (y + h / 2.0 - acy).abs();
            if best.as_ref().is_none_or(|(_, d)| dist < *d) {
                best = Some((id.clone(), dist));
            }
        }
        best.map(|(id, _)| id)
    }

    /// Move keyboard focus to the adjacent pane in `dir`.
    fn focus_dir(&self, dir: FocusDir) {
        if let Some(id) = self.adjacent_pane(dir) {
            self.ws.lock().unwrap().active_pane = Some(id.clone());
            // Sync the daemon's active pointer (see body-click note).
            if let Some(client) = self.daemon_client.as_ref() {
                client.focus(&id);
            }
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
    }

    /// Swap the active pane with its neighbour in `dir`. The BSP tree
    /// exchanges the two leaves' ids, so each pane's content moves into
    /// the other's slot while the PTYs stay put; focus rides along with
    /// the active id into its new position.
    fn swap_dir(&mut self, dir: FocusDir) {
        // No daemon swap RPC exists yet, so a local swap_leaves would be
        // reverted by the next State broadcast (a silent no-op in daemon mode).
        // Block here until a surface.swap RPC is added (daemon.rs/methods.rs/
        // stream.rs, mirroring move_surface). Non-daemon/tmux: local swap is fine.
        if self.daemon_client.is_some() {
            return;
        }
        let Some(active) = self.ws.lock().unwrap().active_pane.clone() else {
            return;
        };
        let Some(target) = self.adjacent_pane(dir) else {
            return;
        };
        if let Some(tree) = self.pty_layout.as_mut() {
            tree.swap_leaves(&active, &target);
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Pane whose header band contains the cursor (logical px), or None.
    /// Headers only exist when the workspace is split.
    fn header_at_px(&self, x: f32, y: f32) -> Option<String> {
        let (cols, rows) = self.window_cells();
        let rects = self.effective_leaf_rects(cols, rows);
        // A zoomed pane is a single rect but still has a header (to un-zoom),
        // so only bail on a lone pane when nothing is zoomed.
        if rects.len() <= 1 && self.zoomed_pane.is_none() {
            return None;
        }
        let pad = WINDOW_PADDING + self.effective_sidebar_w();
        for (id, cx, cy, cw, _ch) in rects {
            let bx = pad + cx as f32 * self.cell.w;
            let by = TITLE_HEIGHT + cy as f32 * self.cell.h;
            let bw = cw as f32 * self.cell.w;
            if x >= bx && x <= bx + bw && y >= by && y <= by + PANE_HEADER_HEIGHT {
                return Some(id);
            }
        }
        None
    }

    /// Pane + edge the cursor is over, for header drag-and-drop. The zone
    /// is the dominant axis from the pane box centre, so the cursor always
    /// resolves to one of the four edges. None when off every pane.
    fn drop_target_at(&self, x: f32, y: f32) -> Option<(String, DropZone)> {
        let tree = self.pty_layout.as_ref()?;
        let leaves_count = tree.leaves().len();
        let (cols, rows) = self.window_cells();
        let rects = self.effective_leaf_rects(cols, rows);
        let pad = WINDOW_PADDING + self.effective_sidebar_w();
        // Per-pane tab-strip top: where the pill row starts. Combined
        // with the header-band height below, this gives the full
        // pane-header region (even when a single-tab pane has a tiny
        // strip).
        let mut strip_top: HashMap<String, f32> = HashMap::new();
        for (pid, _, (_, ry, _, _)) in &self.pane_tab_rects {
            strip_top
                .entry(pid.clone())
                .and_modify(|t| { if *ry < *t { *t = *ry; } })
                .or_insert(*ry);
        }
        // When the layout has >1 leaf every pane gets a 30 logical-px
        // header band — including single-tab panes — so the box must
        // extend up by at least that amount or a drop onto a single-tab
        // header falls into the body's Up zone (split-up) instead of
        // Center (tab-merge), which was the "drag→merge gives split"
        // bug.
        let header_band = if leaves_count > 1 { PANE_HEADER_HEIGHT } else { 0.0 };
        for (id, cx, cy, cw, ch) in rects {
            let bx = pad + cx as f32 * self.cell.w;
            // pane_top = pane 시작 (헤더 띠 시작, chrome 포함). 기존엔
            // 이걸 본문 시작으로 잘못 가정해 box_top을 한 칸 위로
            // 잡았고 그래서 hit-test가 전부 30px 위로 shift됨 — 헤더 띠
            // 안 마우스가 본문 판정, 헤더 위(title bar 영역)가 헤더 판정.
            let pane_top = TITLE_HEIGHT + cy as f32 * self.cell.h;
            let bw = (cw as f32 * self.cell.w).max(1.0);
            let bh = (ch as f32 * self.cell.h).max(1.0);
            let body_top = pane_top + header_band;
            if x >= bx && x <= bx + bw && y >= pane_top && y <= pane_top + bh {
                // 헤더 띠 (pane_top ~ body_top) = Center (tab merge).
                // 본문 (body_top ~ pane_top+bh) = 4방향 split.
                if y < body_top {
                    return Some((id, DropZone::Center));
                }
                let dist_left = x - bx;
                let dist_right = bx + bw - x;
                let dist_top = y - body_top;
                let dist_bottom = (pane_top + bh) - y;
                let zone = if dist_left.min(dist_right) < dist_top.min(dist_bottom) {
                    if dist_left < dist_right { DropZone::Left } else { DropZone::Right }
                } else if dist_top < dist_bottom {
                    DropZone::Up
                } else {
                    DropZone::Down
                };
                return Some((id, zone));
            }
        }
        None
    }

    /// Window chip in the left sidebar under the cursor, resolved to that
    /// window's anchor leaf — the drop target for a cross-window header drag.
    /// Returns None when off every chip or over the already-active window (its
    /// panes are on screen, so an in-window drop is `drop_target_at`'s job).
    /// The daemon's `move_surface` does the actual cross-window detach/insert.
    fn sidebar_window_drop_target(&self, x: f32, y: f32) -> Option<String> {
        let inside =
            |r: &(f32, f32, f32, f32)| x >= r.0 && x <= r.0 + r.2 && y >= r.1 && y <= r.1 + r.3;
        let idx = self
            .window_tab_rects
            .iter()
            .find(|(_, r)| inside(r))
            .map(|(i, _)| *i)?;
        if idx == self.active_window {
            return None;
        }
        self.windows
            .get(idx)
            .and_then(|w| w.as_ref())
            .and_then(|l| l.leaves().first().map(|s| s.to_string()))
    }

    /// Relocate `moving` next to `target` along the edge given by `zone`.
    /// Detaches the moving leaf (its PTY stays alive) and re-attaches it
    /// beside the target, then resizes every pane to its new rect. No-op
    /// when source and target are the same pane.
    fn move_pane(&mut self, moving: &str, target: &str, zone: DropZone) {
        if moving == target {
            return;
        }
        // Daemon authority: a header-drag relocation MUST go through the daemon
        // (surface.move RPC). A local pty_layout change here is overwritten by
        // the next State broadcast → the pane goes dead (the header-drag
        // "아무반응없음"). Mirrors the tab-drag single-tab path + split_active_pane.
        if let Some(client) = self.daemon_client.clone() {
            let dir = match zone {
                DropZone::Left => "left",
                DropZone::Right => "right",
                DropZone::Up => "up",
                DropZone::Down => "down",
                DropZone::Center => return, // ambiguous for a whole-pane move
            };
            client.move_pane(moving, target, dir);
            self.chrome_dirty = true;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            return;
        }
        let (dir, before) = match zone {
            DropZone::Left => (kasa_pty::SplitDir::Horizontal, true),
            DropZone::Right => (kasa_pty::SplitDir::Horizontal, false),
            DropZone::Up => (kasa_pty::SplitDir::Vertical, true),
            DropZone::Down => (kasa_pty::SplitDir::Vertical, false),
            // Header drag onto a target's centre = ambiguous for a
            // whole-pane move; ignore rather than picking a random edge.
            DropZone::Center => return,
        };
        if let Some(tree) = self.pty_layout.as_mut() {
            if !tree.remove_leaf(moving) {
                return;
            }
            if !tree.insert_beside(target, dir, before, moving.to_string()) {
                // Target vanished (shouldn't happen) — re-attach beside
                // the first surviving leaf so the pane isn't orphaned.
                if let Some(anchor) = tree.leaves().first().map(|s| s.to_string()) {
                    tree.insert_beside(&anchor, dir, before, moving.to_string());
                }
            }
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.ws.lock().unwrap().active_pane = Some(moving.to_string());
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn send_bytes(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        // Route to whichever backend owns the *active tab*. In-pane tabs
        // (`spawn_new_tab`) are always GUI-local PtySessions in `self.pty`,
        // even when the GUI is daemon-attached: the daemon owns only the
        // primary tab (pid == outer id). So a GUI-local hit must win over the
        // daemon path — otherwise keystrokes for a secondary tab get sent to
        // the daemon, which has no such surface and routes them to the primary
        // tab instead (the "typing in another tab lands in the first tab" bug).
        let surface = self.target_surface();
        if let Some(pid) = surface.as_deref() {
            if let Some(pty) = self.pty.get(pid) {
                let _ = pty.send_bytes(bytes);
                return;
            }
        }
        // Dispatch by which backend is wired up. The hex encoding is
        // a tmux send-keys quirk (the daemon decodes hex pairs back
        // to bytes itself); for the pty backend we hand the raw bytes
        // straight to the PTY writer.
        if let Some(client) = self.daemon_client.as_ref() {
            // Daemon-attached GUI: input goes over the control socket; the
            // daemon owns the PTY writer.
            client.send_raw(surface.as_deref(), bytes);
        } else if let Some(tmux) = self.tmux.as_ref() {
            let hex: String = bytes
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(" ");
            let target = self.target_pane();
            let _ = tmux.send_keys_hex(target.as_deref(), &hex);
        } else if let Some(pty) = self.active_pty() {
            let _ = pty.send_bytes(bytes);
        }
    }

    /// True when the pane has mouse reporting + SGR encoding enabled
    /// (claude code / vim / less in alt-screen). Shift-held overrides
    /// to false so the user has an iTerm-style escape hatch back to
    /// our own selection logic.
    fn pane_takes_mouse(&self, pane_id: &str) -> bool {
        if self.modifiers.shift_key() {
            return false;
        }
        let ws = self.ws.lock().unwrap();
        ws.panes
            .get(pane_id)
            .and_then(|p| p.term())
            .map(|t| t.mouse_enabled && t.mouse_sgr)
            .unwrap_or(false)
    }

    /// True if the pane shows a terminal (vs a markdown / image document
    /// view). Document panes are scrolled with the wheel, not dragged —
    /// terminal cell text-selection must not start on them. Unknown pane
    /// (e.g. a leaf that hasn't produced a ScreenUpdate yet) defaults to
    /// terminal so the normal split flow is never blocked.
    fn pane_is_terminal(&self, pane_id: &str) -> bool {
        let ws = self.ws.lock().unwrap();
        ws.panes
            .get(pane_id)
            .map(|p| matches!(p.content, PaneContent::Terminal(_)))
            .unwrap_or(true)
    }

    /// Directory of the active markdown pane's source file, for resolving
    /// relative link destinations.
    fn active_markdown_dir(&self) -> Option<std::path::PathBuf> {
        let ws = self.ws.lock().unwrap();
        let active = ws.active_pane.as_ref()?;
        let md = ws.panes.get(active)?.markdown()?;
        std::path::Path::new(&md.doc.path)
            .parent()
            .map(|d| d.to_path_buf())
    }

    /// Hit-test the cursor against the markdown code-block copy buttons; copy
    /// the block's text if one is under it. Returns true if a copy happened.
    fn try_copy_md_block(&mut self) -> bool {
        let (cx, cy) = self.cursor_px;
        let code = {
            let Some(g) = self.gpu.as_ref() else { return false };
            g.md_copy_rects
                .iter()
                .find(|(x, y, w, h, _)| cx >= *x && cx <= *x + *w && cy >= *y && cy <= *y + *h)
                .map(|(_, _, _, _, c)| c.clone())
        };
        match code {
            Some(c) => {
                self.copy_block_text(&c);
                true
            }
            None => false,
        }
    }

    /// Hit-test the cursor against the link rects the renderer recorded for
    /// the last markdown frame; open the destination if one is under it.
    /// Returns true if a link was opened (so the caller skips other handling).
    fn try_open_md_link(&self) -> bool {
        let (cx, cy) = self.cursor_px;
        let Some(g) = self.gpu.as_ref() else { return false };
        let dest = g
            .md_link_rects
            .iter()
            .find(|(x, y, w, h, _)| cx >= *x && cx <= *x + *w && cy >= *y && cy <= *y + *h)
            .map(|(_, _, _, _, d)| d.clone());
        match dest {
            Some(d) => {
                self.open_md_dest(&d);
                true
            }
            None => false,
        }
    }

    /// Open a markdown link destination: http(s)/mailto go to the default
    /// app (browser/mail); a local path is revealed in Finder (`open -R`),
    /// resolving relative paths against the markdown file's directory.
    fn open_md_dest(&self, dest: &str) {
        if dest.starts_with("http://")
            || dest.starts_with("https://")
            || dest.starts_with("mailto:")
        {
            let _ = std::process::Command::new("open").arg(dest).spawn();
            return;
        }
        let raw = dest.strip_prefix("file://").unwrap_or(dest);
        let mut path = std::path::PathBuf::from(raw);
        if path.is_relative() {
            if let Some(dir) = self.active_markdown_dir() {
                path = dir.join(raw);
            }
        }
        if path.exists() {
            let _ = std::process::Command::new("open").arg("-R").arg(&path).spawn();
        } else {
            // Unknown scheme or missing file — let the OS try to interpret it.
            let _ = std::process::Command::new("open").arg(dest).spawn();
        }
    }

    /// Encode an SGR mouse event and ship it to the pane. `button` is
    /// the SGR button code (0 = left press/motion/release, +32 for
    /// motion-with-button-held). `press` toggles the final byte
    /// between `M` (press / motion) and `m` (release).
    fn send_mouse_sgr(&self, pane_id: &str, button: u8, col: u16, row: u16, press: bool) {
        let final_byte = if press { 'M' } else { 'm' };
        let payload = format!("\x1b[<{button};{};{}{final_byte}", col + 1, row + 1);
        if let Some(tmux) = self.tmux.as_ref() {
            let hex: String = payload
                .as_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(" ");
            let _ = tmux.send_keys_hex(Some(pane_id), &hex);
        } else if let Some(client) = self.daemon_client.as_ref() {
            client.send_raw(Some(pane_id), payload.as_bytes());
        } else if let Some(pty) = self.pty_for_pane(pane_id) {
            let _ = pty.send_bytes(payload.as_bytes());
        }
    }

    /// Resolve the user-visible label for a pane: OSC 0/2 title set
    /// by the shell or a TUI → foreground process comm → cwd. Used by
    /// both the per-pane header strip and the single-pane native
    /// window title so the two stay consistent.
    /// Scan the bottom of the active pane's grid for a Braille
    /// spinner glyph (U+2800..U+28FF). Tools like Claude Code, oh-my-
    /// zsh's pure-prompt, npm, etc. paint these one cell at a time
    /// to animate progress — picking the glyph straight from the
    /// grid lets us mirror their phase in the window title without
    /// any extra timing math. Returns None when no spinner is
    /// currently visible.
    /// Pull Claude Code's progress line ("✻ Brewed for 5s",
    /// "✶ Thinking…", etc.) straight out of the cell grid. We scan
    /// the bottom of the active pane for a row that starts with a
    /// star/asterisk glyph and trim that row to its text. The
    /// rendered grid is the only signal Claude Code gives us — it
    /// doesn't push these as OSC titles — so this is how we mirror
    /// the live status into the macOS titlebar.
    #[allow(dead_code)]
    fn active_claude_status(&self) -> Option<String> {
        let ws = self.ws.lock().unwrap();
        let id = ws.active_pane.as_deref()?;
        let pane = ws.panes.get(id)?;
        let t = pane.term()?;
        let rows = t.cells.len();
        let start = rows.saturating_sub(10);
        for row in t.cells[start..].iter() {
            let mut text = String::new();
            let mut has_marker = false;
            for cell in row {
                if cell.ch == '\0' {
                    text.push(' ');
                } else {
                    text.push(cell.ch);
                    let cp = cell.ch as u32;
                    if (0x2731..=0x274F).contains(&cp) {
                        has_marker = true;
                    }
                }
            }
            if has_marker {
                let trimmed = text.trim();
                if trimmed.len() > 4 && trimmed.len() < 80 {
                    return Some(trimmed.to_string());
                }
            }
        }
        None
    }

    #[allow(dead_code)]
    fn active_spinner_glyph(&self) -> Option<char> {
        let ws = self.ws.lock().unwrap();
        let id = ws.active_pane.as_deref()?;
        let pane = ws.panes.get(id)?;
        let t = pane.term()?;
        let rows = t.cells.len();
        let start = rows.saturating_sub(8);
        for row in &t.cells[start..] {
            for cell in row {
                let c = cell.ch;
                let cp = c as u32;
                // Braille spinners (npm, pure-prompt, etc.) +
                // Dingbats asterisks/stars (Claude Code uses
                // ✻/✶/✷/✸/✹/✺ as its "thinking" indicator).
                if (0x2800..=0x28FF).contains(&cp) || (0x2731..=0x274F).contains(&cp) {
                    return Some(c);
                }
            }
        }
        None
    }

    /// Sync the macOS window title (Dock label, ⌘-Tab switcher) with
    /// the active pane's resolved label when only one pane is open.
    /// Skipped for multi-pane workspaces — there the per-pane header
    /// strip carries the same information and the OS title gets
    /// noisy as the user shuffles focus between splits.
    fn maybe_update_window_title(&mut self) {
        // Throttle: scroll bursts can fire RedrawRequested 60+ times
        // per second, and every call here takes a workspace lock and
        // may shell out to `ps` for the process name. 200ms is fast
        // enough for "title follows focus" but cheap enough that a
        // wheel sweep stays smooth.
        let now = Instant::now();
        if let Some(t) = self.last_window_title_check {
            if now.duration_since(t).as_millis() < 200 {
                return;
            }
        }
        self.last_window_title_check = Some(now);
        // Native window title always tracks the focused pane. In a
        // split workspace this means macOS's Dock / ⌘-Tab label
        // updates when the user clicks a different split — matching
        // iTerm / Terminal.app.
        let active = {
            let ws = self.ws.lock().unwrap();
            let id = ws
                .active_pane
                .clone()
                .or_else(|| ws.panes.keys().next().cloned());
            let osc = id.as_ref().and_then(|i| ws.panes.get(i)).and_then(|p| p.title.clone());
            id.map(|i| (i, osc))
        };
        let Some((id, osc)) = active else { return };
        let _ = osc;
        let label = self
            .pty
            .get(&id)
            .and_then(|p| p.shell_pid())
            .and_then(socket::pid_cwd)
            .map(|p| {
                let s = p.to_string_lossy().into_owned();
                match std::env::var("HOME").ok() {
                    Some(home) if s.starts_with(&home) => s.replacen(&home, "~", 1),
                    _ => s,
                }
            })
            .unwrap_or_else(|| Self::resolve_pane_label(&self.pty, &id, None));
        // Claude Code response indicator. Priority:
        //   1. Lift Claude Code's own status line straight from the
        //      cell grid ("✻ Brewed for 5s") so the user sees the
        //      same words they would on the prompt — including the
        //      live elapsed time.
        //   2. Fallback when only a spinner glyph is detected but no
        //      status text: cycle our own asterisk sequence
        //      ✶ ✳ ✶ · ✽ next to the "claude" label.
        // Note: we intentionally do NOT scrape the grid for the
        // "✻ Brewed for Ns" status line here. iTerm-style behavior is
        // to let the inner program drive the title via OSC 0/2 only
        // — Claude Code sends the conversation summary that way, and
        // the per-question status is meant to stay inside the pane.
        // Scraping the grid would clobber the conversation title
        // every few hundred ms with whatever Claude was rendering.
        if self.last_window_title.as_deref() == Some(&label) {
            return;
        }
        if let Some(w) = self.window.as_ref() {
            w.set_title(&label);
        }
        self.last_window_title = Some(label);
    }

    fn resolve_pane_label(
        pty: &HashMap<String, Arc<kasa_pty::PtySession>>,
        pane_id: &str,
        osc_title: Option<&str>,
    ) -> String {
        if let Some(t) = osc_title.filter(|s| !s.is_empty()) {
            return t.to_string();
        }
        if let Some(name) = pty.get(pane_id).and_then(|p| p.active_process_name()) {
            return decorate_process_name(&name);
        }
        std::env::current_dir()
            .ok()
            .map(|p| {
                let s = p.to_string_lossy().into_owned();
                match std::env::var("HOME").ok() {
                    Some(home) if s.starts_with(&home) => s.replacen(&home, "~", 1),
                    _ => s,
                }
            })
            .unwrap_or_else(|| "shell".to_string())
    }

    fn copy_selection(&self) {
        let Some(sel) = self.selection else { return; };
        let rows = {
            let ws = self.ws.lock().unwrap();
            match ws.active().and_then(|p| p.term()) {
                Some(t) => t.cells.clone(),
                None => return,
            }
        };
        let text = extract_selection(&rows, sel);
        if text.is_empty() {
            return;
        }
        match arboard::Clipboard::new() {
            Ok(mut cb) => {
                if let Err(e) = cb.set_text(text) {
                    eprintln!("[tmuxify] clipboard write failed: {e}");
                }
            }
            Err(e) => eprintln!("[tmuxify] clipboard open failed: {e}"),
        }
    }

    fn paste_clipboard(&self) {
        let text = match arboard::Clipboard::new().and_then(|mut cb| cb.get_text()) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("[tmuxify] clipboard read failed: {e}");
                return;
            }
        };
        let mut payload = Vec::with_capacity(text.len() + 12);
        payload.extend_from_slice(b"\x1b[200~");
        payload.extend_from_slice(text.as_bytes());
        payload.extend_from_slice(b"\x1b[201~");
        self.send_bytes(&payload);
    }

    fn handle_wheel(&mut self, delta: MouseScrollDelta) {
        let wdbg = std::env::var_os("KASATERM_WHEEL_DEBUG").is_some();
        let dy_cells = match delta {
            MouseScrollDelta::LineDelta(_, y) => y * 0.3,
            MouseScrollDelta::PixelDelta(p) => (p.y as f32) / self.cell.h.max(1.0) * 0.3,
        };
        if wdbg {
            eprintln!(
                "[wheel] delta={delta:?} dy_cells={dy_cells:.4} accum_before={:.4} cursor_px=({:.1},{:.1})",
                self.wheel_accum_y, self.cursor_px.0, self.cursor_px.1
            );
        }
        let lines = match wheel_step(
            &mut self.wheel_accum_y,
            dy_cells,
            &mut self.last_wheel_emit,
            Instant::now(),
        ) {
            Some(l) => l,
            None => {
                if wdbg {
                    eprintln!(
                        "[wheel]   -> None (accum_after={:.4}, no emit)",
                        self.wheel_accum_y
                    );
                }
                return;
            }
        };
        // File-tree column: the pointer is over the tree, not a terminal, so
        // scroll the rows instead of delegating to a pane (px_to_pane_cell
        // returns None here and would otherwise fall through to the active
        // pane). Clamp so it can't scroll above the top or past the last row.
        if self.file_tree_visible
            && self.cursor_px.1 > TITLE_HEIGHT
            && self.cursor_px.0 >= self.file_tree_col_x()
            && self.cursor_px.0 < self.file_tree_col_x() + self.file_tree_w_logical
        {
            let item_h = 22.0_f32;
            let win_h = self.window.as_ref().map_or(800.0, |w| {
                w.inner_size().height as f32 / self.effective_scale()
            });
            let start_y = TITLE_HEIGHT + 10.0;
            let content_h = self.file_tree_nodes.len() as f32 * item_h;
            let max_scroll = (content_h - (win_h - start_y).max(0.0)).max(0.0);
            // lines>0 = wheel up = toward the top = less scroll.
            let delta_px = lines as f32 * item_h;
            let next = (self.file_tree_scroll - delta_px).clamp(0.0, max_scroll);
            if (next - self.file_tree_scroll).abs() > 0.01 {
                self.file_tree_scroll = next;
                self.chrome_dirty = true;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            return;
        }
        // Git column: scroll the change list when the pointer is over it. Same
        // clamp idea as the file tree; the visible height is the band between
        // the header and the bottom button zone.
        if self.git_col_visible
            && self.cursor_px.1 > TITLE_HEIGHT
            && self.cursor_px.0 >= self.git_col_x()
        {
            let item_h = 22.0_f32;
            let n = self
                .git_col_data
                .lock()
                .map(|g| g.files.len())
                .unwrap_or(0);
            let win_h = self.window.as_ref().map_or(800.0, |w| {
                w.inner_size().height as f32 / self.effective_scale()
            });
            let dock_h = if self.docked.is_empty() { 0.0 } else { DOCK_HEIGHT };
            // Header (branch + summary + rule) ≈ 68px; button zone ≈ 44px.
            let list_top = TITLE_HEIGHT + 68.0;
            let visible_h = (win_h - dock_h - list_top - 44.0).max(0.0);
            let content_h = n as f32 * item_h;
            let max_scroll = (content_h - visible_h).max(0.0);
            let delta_px = lines as f32 * item_h;
            let next = (self.git_col_scroll - delta_px).clamp(0.0, max_scroll);
            if (next - self.git_col_scroll).abs() > 0.01 {
                self.git_col_scroll = next;
                self.chrome_dirty = true;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            return;
        }
        // Decide which pane handles this wheel: the pane the pointer is
        // hovering over. Falls back to the active pane if the pointer
        // is in a gutter. Multi-pane lets the user scroll inside any
        // pane regardless of which one currently has keyboard focus.
        let target_pane_id = self
            .px_to_pane_cell(self.cursor_px.0, self.cursor_px.1)
            .map(|(id, _, _)| id)
            .or_else(|| self.target_pane());
        if wdbg {
            eprintln!(
                "[wheel]   lines={lines} target_pane={:?} active={:?}",
                target_pane_id,
                self.ws.lock().unwrap().active_pane
            );
        }
        // Markdown pane: scroll the laid-out document by pixels (it has no PTY
        // history to delegate to). Clamp to the content height the renderer
        // last published so it can't scroll past the end.
        let is_md = {
            let ws = self.ws.lock().unwrap();
            target_pane_id
                .as_deref()
                .and_then(|id| ws.panes.get(id))
                .map_or(false, |p| p.markdown().is_some())
        };
        if is_md {
            if let Some(id) = target_pane_id.as_deref() {
                let visible_h = self.window.as_ref().map_or(400.0, |w| {
                    w.inner_size().height as f32 / (w.scale_factor() as f32 * self.ui_zoom)
                }) - TITLE_HEIGHT
                    - PANE_HEADER_HEIGHT
                    - 2.0 * PANE_INNER_Y;
                let content_h = self.md_content_h.get(id).copied().unwrap_or(0.0);
                let max_scroll = (content_h - visible_h).max(0.0);
                // lines>0 = wheel up = toward the top of the doc = less scroll.
                let delta_px = lines as f32 * self.cell.h;
                if let Ok(mut ws) = self.ws.lock() {
                    if let Some(pane) = ws.panes.get_mut(id) {
                        pane.dirty = true;
                        if let Some(m) = pane.markdown_mut() {
                            let cur = m.scroll as f32;
                            let next = (cur - delta_px).clamp(0.0, max_scroll);
                            m.scroll = next.round() as usize;
                        }
                    }
                }
            }
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            return;
        }
        let (alt, hist_len, mouse_on, mouse_sgr) = {
            let ws = self.ws.lock().unwrap();
            let pane = target_pane_id
                .as_deref()
                .and_then(|id| ws.panes.get(id));
            match pane.and_then(|p| p.term()) {
                Some(t) => (t.alt_screen, t.history.len(), t.mouse_enabled, t.mouse_sgr),
                None => return,
            }
        };
        if mouse_on && mouse_sgr {
            let (col, row) = self
                .px_to_cell_active(self.cursor_px.0, self.cursor_px.1)
                .unwrap_or((1, 1));
            let button = if lines > 0 { 64 } else { 65 };
            let count = lines.unsigned_abs().min(8) as usize;
            let single = format!("\x1b[<{button};{};{}M", col + 1, row + 1);
            let payload: Vec<u8> = single.as_bytes().repeat(count.max(1));
            // For the tmux backend we name the pane explicitly so an
            // inactive-but-hovered pane scrolls instead of the focused
            // one. The pty backend is single-pane: the pane id is
            // already implicit.
            if let Some(tmux) = self.tmux.as_ref() {
                if let Some(target) = target_pane_id.as_deref() {
                    let hex: String = payload
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<Vec<_>>()
                        .join(" ");
                    let _ = tmux.send_keys_hex(Some(target), &hex);
                }
            } else if let Some(id) = target_pane_id.as_deref() {
                if let Some(client) = self.daemon_client.as_ref() {
                    client.send_raw(Some(id), &payload);
                } else if let Some(pty) = self.pty_for_pane(id) {
                    let _ = pty.send_bytes(&payload);
                }
            }
            return;
        }
        if alt {
            let esc: &[u8] = if lines > 0 { b"\x1b[5~" } else { b"\x1b[6~" };
            if let Some(tmux) = self.tmux.as_ref() {
                if let Some(target) = target_pane_id.as_deref() {
                    let hex: String = esc
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<Vec<_>>()
                        .join(" ");
                    let _ = tmux.send_keys_hex(Some(target), &hex);
                }
            } else if let Some(id) = target_pane_id.as_deref() {
                if let Some(client) = self.daemon_client.as_ref() {
                    client.send_raw(Some(id), esc);
                } else if let Some(pty) = self.pty_for_pane(id) {
                    let _ = pty.send_bytes(esc);
                }
            }
            return;
        }
        // Normal screen scrollback. PTY backend delegates to
        // alacritty's own scrollback (display_offset) — it tracks
        // scroll-region TUIs (claude code's pinned input) correctly,
        // unlike the old frame-diff shift heuristic. tmux backend
        // keeps the local history composition.
        let step = lines.unsigned_abs().min(8) as i32;
        let _ = hist_len;
        if let Some(id) = target_pane_id.as_deref() {
            if let Some(client) = self.daemon_client.as_ref() {
                // Daemon owns the scrollback; it re-snapshots and streams the
                // scrolled grid back to us. Positive `lines` = toward history.
                client.scroll(id, if lines > 0 { step } else { -step });
            } else if self.tmux.is_some() {
                if let Ok(mut ws) = self.ws.lock() {
                    if let Some(pane) = ws.panes.get_mut(id) {
                        pane.dirty = true;
                        if let Some(t) = pane.term_mut() {
                            let s = step as usize;
                            if lines > 0 {
                                t.scroll_offset = (t.scroll_offset + s).min(hist_len);
                            } else {
                                t.scroll_offset = t.scroll_offset.saturating_sub(s);
                            }
                        }
                    }
                }
            } else if let Some(pty) = self.pty_for_pane(id) {
                // Positive `lines` = scroll up = toward older history.
                let off = pty.scroll(if lines > 0 { step } else { -step });
                if wdbg {
                    eprintln!("[wheel]   pty.scroll step={step} -> display_offset={off}");
                }
            } else if wdbg {
                eprintln!("[wheel]   no pty_for_pane({id}) -> NO-OP");
            }
        } else if wdbg {
            eprintln!("[wheel]   target_pane_id=None -> NO-OP");
        }
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn forward_key(&mut self, event: &KeyEvent) {
        if event.state != ElementState::Pressed {
            return;
        }
        // Touch the input timer so the cursor stays solid for a beat and
        // the blink phase re-starts from "on" once it kicks in.
        self.last_input_at = Instant::now();
        // Image panes have no PTY — repurpose keys for view control.
        //   +/=      zoom in     -    zoom out
        //   0        reset       r/R  rotate 90° CW
        // Every other key is swallowed (no shell to receive them).
        let is_image = {
            let ws = self.ws.lock().unwrap();
            ws.active().map(|p| p.image().is_some()).unwrap_or(false)
        };
        if is_image {
            let mut changed = false;
            if let Key::Character(s) = &event.logical_key {
                match s.as_str() {
                    "+" | "=" => {
                        if let Ok(mut ws) = self.ws.lock() {
                            if let Some(pane) = ws.active_mut() {
                                let z = pane.image_view_zoom();
                                pane.image_zoom = (z * 1.25).clamp(1.0, 8.0);
                                changed = true;
                            }
                        }
                    }
                    "-" | "_" => {
                        if let Ok(mut ws) = self.ws.lock() {
                            if let Some(pane) = ws.active_mut() {
                                let z = pane.image_view_zoom();
                                pane.image_zoom = (z / 1.25).max(1.0);
                                changed = true;
                            }
                        }
                    }
                    "0" => {
                        if let Ok(mut ws) = self.ws.lock() {
                            if let Some(pane) = ws.active_mut() {
                                pane.image_zoom = 1.0;
                                pane.image_rot = 0;
                                changed = true;
                            }
                        }
                    }
                    "r" | "R" => {
                        if let Ok(mut ws) = self.ws.lock() {
                            if let Some(pane) = ws.active_mut() {
                                pane.image_rot = (pane.image_rot + 1) % 4;
                                changed = true;
                            }
                        }
                    }
                    _ => {}
                }
            }
            if changed {
                self.chrome_dirty = true;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            return;
        }
        // Markdown panes have no PTY. In Raw mode keys edit the buffer; in
        // Render mode they're swallowed (scrolling is wheel-driven).
        let (is_md, is_raw) = {
            let ws = self.ws.lock().unwrap();
            ws.active().map_or((false, false), |p| match p.markdown() {
                Some(m) => (true, m.raw_mode),
                None => (false, false),
            })
        };
        if is_md {
            if is_raw {
                self.md_editor_input(event);
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            return;
        }
        // Typing snaps the active pane back to live tail. Other panes'
        // scroll offsets are left alone — switching focus by clicking
        // doesn't disturb where the user was reading.
        if let Ok(mut ws) = self.ws.lock() {
            if let Some(t) = ws.active_mut().and_then(|p| p.term_mut()) {
                if t.scroll_offset != 0 {
                    t.scroll_offset = 0;
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
        }
        // Inline-autosuggestion accept. When a ghost suggestion is on
        // screen the cursor is necessarily at the end of the typed line
        // (we clear the suggestion on any left/up/down motion), so it's
        // safe to repurpose → / End / Ctrl-E to accept it and Alt-F to
        // accept one word — matching zsh-autosuggestions / fish. Tab is
        // deliberately left to the shell's own completion. We send the
        // remainder to the PTY and grow input_buf so the next frame keeps
        // suggesting from the extended prefix.
        if let Some(sugg) = self.current_suggestion.clone() {
            if !sugg.is_empty() {
                use winit::keyboard::{KeyCode, PhysicalKey};
                let plain = !self.modifiers.alt_key() && !self.modifiers.super_key();
                let phys = match event.physical_key {
                    PhysicalKey::Code(c) => Some(c),
                    _ => None,
                };
                let accept_full = (matches!(event.logical_key, Key::Named(NamedKey::ArrowRight))
                    && plain)
                    || (matches!(event.logical_key, Key::Named(NamedKey::End)) && plain)
                    || (self.modifiers.control_key() && phys == Some(KeyCode::KeyE));
                let accept_word =
                    self.modifiers.alt_key() && phys == Some(KeyCode::KeyF) && !sugg.is_empty();
                if accept_full {
                    self.send_bytes(sugg.as_bytes());
                    self.input_buf.push_str(&sugg);
                    self.current_suggestion = None;
                    return;
                }
                if accept_word {
                    // One word = leading spaces + the run up to the next
                    // space boundary, so repeated Alt-F walks the line.
                    let mut end = 0usize;
                    let bytes = sugg.as_bytes();
                    while end < bytes.len() && bytes[end] == b' ' {
                        end += 1;
                    }
                    while end < bytes.len() && bytes[end] != b' ' {
                        end += 1;
                    }
                    let word = &sugg[..end];
                    self.send_bytes(word.as_bytes());
                    self.input_buf.push_str(word);
                    self.current_suggestion = None;
                    return;
                }
            }
        }
        // Modifier-bearing keys must NEVER reach the Hangul composer.
        // In Korean keyboard layout the C key still produces 'ㅊ' as
        // text, but Ctrl+C is meant for SIGINT / "copy" and Cmd+V for
        // paste — we look at the *physical* key for these, not the
        // IME-resolved logical key. While we're here, also forward the
        // standard control-letter byte for any Ctrl+<letter> combo
        // (Ctrl+L clears, Ctrl+D = EOF, etc), so shells and TUI apps
        // behave as users expect regardless of which keyboard layout
        // happens to be active.
        let host = self.host_mod();
        let ctrl = self.modifiers.control_key();
        if host || ctrl {
            use winit::keyboard::{KeyCode, PhysicalKey};
            if let PhysicalKey::Code(code) = event.physical_key {
                // Host-modifier shortcuts. macOS uses Cmd, Windows/Linux
                // use Ctrl+Shift — see `host_mod()`.
                if host {
                    // OS 키 자동반복(키를 누르고 있을 때 반복 발사되는 Pressed
                    // 이벤트, repeat=true)은 무시한다. Cmd 단축키는 전부 단발성
                    // 동작이라, 안 거르면 Cmd+D를 살짝 길게 누르는 것만으로
                    // split이 우르르 나가 pane이 증식하고 Cmd+W는 여러 pane을
                    // 한꺼번에 닫는다. 글자 타이핑 반복은 이 블록 밖이라 무관.
                    if event.repeat {
                        return;
                    }
                    if code == KeyCode::KeyC && self.selection.is_some() {
                        self.copy_selection();
                        return;
                    }
                    if code == KeyCode::KeyV {
                        // Pasted text bypasses our key path, so we can't
                        // mirror it into input_buf — drop the suggestion
                        // prefix so we never suggest off a stale line.
                        self.input_buf.clear();
                        self.current_suggestion = None;
                        self.paste_clipboard();
                        return;
                    }
                    // Terminal.app-style split shortcuts. PTY mode only
                    // — tmux mode lets the daemon handle its own keys.
                    //   D       → horizontal (stacked, default)
                    //   Shift+D → vertical (side-by-side, macOS chord)
                    //   E       → vertical (Windows-friendly chord that
                    //              avoids the Shift-on-Shift conflict)
                    // On macOS host_mod_alt resolves to Shift so
                    // Cmd+Shift+D still flips to vertical. On
                    // Windows/Linux host_mod already owns Shift, so the
                    // dedicated KeyE binding is the practical one.
                    if code == KeyCode::KeyD {
                        let dir = if self.host_mod_alt() {
                            kasa_pty::SplitDir::Vertical
                        } else {
                            kasa_pty::SplitDir::Horizontal
                        };
                        if let Err(e) = self.split_active_pane(dir) {
                            eprintln!("[tmuxify] split failed: {e}");
                        }
                        return;
                    }
                    if code == KeyCode::KeyE {
                        if let Err(e) = self.split_active_pane(kasa_pty::SplitDir::Vertical) {
                            eprintln!("[tmuxify] split failed: {e}");
                        }
                        return;
                    }
                    // Close the focused pane. Last-pane close is left
                    // to the OS close button.
                    if code == KeyCode::KeyW {
                        self.close_active_pane();
                        return;
                    }
                    // Cmd+T → new window in the current session (PTY backend
                    // only; tmux owns its own windows). Cmd+1..9 switch to
                    // that window. Digit0 is font-reset above, so windows
                    // start at 1.
                    if code == KeyCode::KeyT && self.tmux.is_none() {
                        if self.modifiers.shift_key() {
                            // Cmd+Shift+T → restore the most recently docked pane
                            // (ghostty reopen-closed-tab). No-op if dock empty.
                            if let (Some(client), Some(d)) =
                                (self.daemon_client.as_ref(), self.docked.last())
                            {
                                client.undock(&d.id);
                            }
                        } else {
                            self.new_window();
                        }
                        return;
                    }
                    let win_digit = match code {
                        KeyCode::Digit1 | KeyCode::Numpad1 => Some(0),
                        KeyCode::Digit2 | KeyCode::Numpad2 => Some(1),
                        KeyCode::Digit3 | KeyCode::Numpad3 => Some(2),
                        KeyCode::Digit4 | KeyCode::Numpad4 => Some(3),
                        KeyCode::Digit5 | KeyCode::Numpad5 => Some(4),
                        KeyCode::Digit6 | KeyCode::Numpad6 => Some(5),
                        KeyCode::Digit7 | KeyCode::Numpad7 => Some(6),
                        KeyCode::Digit8 | KeyCode::Numpad8 => Some(7),
                        KeyCode::Digit9 | KeyCode::Numpad9 => Some(8),
                        _ => None,
                    };
                    if let Some(idx) = win_digit {
                        if self.tmux.is_none() {
                            self.switch_window(idx);
                            return;
                        }
                    }
                    // `[` / `]` cycle focus through panes in document
                    // order.
                    if code == KeyCode::BracketLeft {
                        self.cycle_focus(-1);
                        return;
                    }
                    if code == KeyCode::BracketRight {
                        self.cycle_focus(1);
                        return;
                    }
                    // Cmd+Option+Arrow → move focus to the spatially
                    // adjacent pane; add Shift to swap the two panes.
                    // iTerm uses the same chord. Gated on Option so plain
                    // Cmd+Arrow still reaches the shell (line start/end).
                    if self.modifiers.alt_key() {
                        let fdir = match code {
                            KeyCode::ArrowLeft => Some(FocusDir::Left),
                            KeyCode::ArrowRight => Some(FocusDir::Right),
                            KeyCode::ArrowUp => Some(FocusDir::Up),
                            KeyCode::ArrowDown => Some(FocusDir::Down),
                            _ => None,
                        };
                        if let Some(d) = fdir {
                            if self.modifiers.shift_key() {
                                self.swap_dir(d);
                            } else {
                                self.focus_dir(d);
                            }
                            return;
                        }
                    }
                }
                // Font zoom. macOS gates on Cmd (= host_mod); Windows/Linux
                // on plain Ctrl (Ctrl+= / Ctrl+- / Ctrl+0, matching Windows
                // Terminal, VS Code, and browsers). Ctrl+Shift+= also lands
                // here since `+` is Shift+`=`. macOS deliberately stays on
                // Cmd so plain Ctrl+letter still reaches the shell as a
                // control byte. Match BOTH the physical key (US layout
                // assumption) AND the logical key text — Korean / European
                // layouts may emit the same character from a different
                // physical position.
                let zoom_mod = if cfg!(target_os = "macos") { host } else { ctrl };
                if zoom_mod {
                    use winit::keyboard::Key;
                    let logical_str = match &event.logical_key {
                        Key::Character(s) => Some(s.as_str()),
                        _ => None,
                    };
                    let is_plus = code == KeyCode::Equal
                        || code == KeyCode::NumpadAdd
                        || logical_str == Some("=")
                        || logical_str == Some("+");
                    let is_minus = code == KeyCode::Minus
                        || code == KeyCode::NumpadSubtract
                        || logical_str == Some("-")
                        || logical_str == Some("_");
                    let is_zero = code == KeyCode::Digit0
                        || code == KeyCode::Numpad0
                        || logical_str == Some("0");
                    // host_mod_alt (Win: Alt, mac: Shift) narrows the zoom to
                    // just the focused pane; without it, the whole UI zooms.
                    let pane_only = self.host_mod_alt();
                    if is_plus {
                        if pane_only { self.change_pane_font(0.1); } else { self.change_ui_zoom(0.1); }
                        return;
                    }
                    if is_minus {
                        if pane_only { self.change_pane_font(-0.1); } else { self.change_ui_zoom(-0.1); }
                        return;
                    }
                    if is_zero {
                        if pane_only { self.reset_pane_font(); } else { self.reset_ui_zoom(); }
                        return;
                    }
                }
                // Ctrl+letter → the corresponding ASCII control byte.
                // This covers Ctrl+C → 0x03 (SIGINT), Ctrl+D → 0x04 (EOF),
                // Ctrl+L → 0x0c (clear), Ctrl+R → 0x12 (reverse search), etc.
                // Suppressed when host is engaged so Ctrl+Shift+letter
                // shortcuts on Windows/Linux don't double-fire as both a
                // shortcut and a control byte.
                if ctrl && !host {
                    let letter = match code {
                        KeyCode::KeyA => Some(b'\x01'),
                        KeyCode::KeyB => Some(b'\x02'),
                        KeyCode::KeyC => Some(b'\x03'),
                        KeyCode::KeyD => Some(b'\x04'),
                        KeyCode::KeyE => Some(b'\x05'),
                        KeyCode::KeyF => Some(b'\x06'),
                        KeyCode::KeyG => Some(b'\x07'),
                        KeyCode::KeyH => Some(b'\x08'),
                        KeyCode::KeyI => Some(b'\x09'),
                        KeyCode::KeyJ => Some(b'\x0a'),
                        KeyCode::KeyK => Some(b'\x0b'),
                        KeyCode::KeyL => Some(b'\x0c'),
                        KeyCode::KeyM => Some(b'\x0d'),
                        KeyCode::KeyN => Some(b'\x0e'),
                        KeyCode::KeyO => Some(b'\x0f'),
                        KeyCode::KeyP => Some(b'\x10'),
                        KeyCode::KeyQ => Some(b'\x11'),
                        KeyCode::KeyR => Some(b'\x12'),
                        KeyCode::KeyS => Some(b'\x13'),
                        KeyCode::KeyT => Some(b'\x14'),
                        KeyCode::KeyU => Some(b'\x15'),
                        KeyCode::KeyV => Some(b'\x16'),
                        KeyCode::KeyW => Some(b'\x17'),
                        KeyCode::KeyX => Some(b'\x18'),
                        KeyCode::KeyY => Some(b'\x19'),
                        KeyCode::KeyZ => Some(b'\x1a'),
                        _ => None,
                    };
                    if let Some(b) = letter {
                        // Flush any pending Hangul syllable before
                        // sending the control byte — typing Enter
                        // mid-syllable already does this; control
                        // letters should too.
                        if let Some(flushed) = self.hangul.flush() {
                            self.send_bytes(flushed.as_bytes());
                            self.preedit.clear();
                            self.in_preedit = false;
                        }
                        // Keep the autosuggest line buffer in sync with
                        // the control byte the shell is about to act on.
                        match b {
                            0x15 | 0x03 | 0x01 => self.input_buf.clear(), // Ctrl-U / Ctrl-C / Ctrl-A
                            0x17 => self.buf_pop_word(),                  // Ctrl-W
                            _ => {}
                        }
                        self.send_bytes(&[b]);
                        return;
                    }
                }
            }
        }
        // Backspace special: when the in-process Hangul composer is
        // mid-syllable, eat the backspace to chip a jamo off the
        // preedit rather than forwarding `\x7f` to the shell (which
        // would erase already-committed text instead).
        if matches!(event.logical_key, Key::Named(NamedKey::Backspace)) {
            if self.hangul.backspace() {
                self.preedit = self.hangul.preedit().unwrap_or_default();
                self.in_preedit = !self.preedit.is_empty();
                self.chrome_dirty = true;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
                return;
            }
        }
        // Any non-character control flushes the composer first so a
        // pending 자모 doesn't get stranded when the user hits Enter /
        // arrow / escape mid-syllable.
        let is_control_key = matches!(
            event.logical_key,
            Key::Named(NamedKey::Enter)
                | Key::Named(NamedKey::Tab)
                | Key::Named(NamedKey::Escape)
                | Key::Named(NamedKey::ArrowUp)
                | Key::Named(NamedKey::ArrowDown)
                | Key::Named(NamedKey::ArrowLeft)
                | Key::Named(NamedKey::ArrowRight)
        );
        // Stash the committed syllable instead of writing it now, so it
        // ships in the SAME write as the key's own bytes below. Two
        // separate writes let an async TUI (claude code / Ink) submit
        // on the trailing \r before it has applied the multibyte
        // syllable — the last char gets dropped. One atomic write keeps
        // "녕" and "\r" in the same stdin chunk.
        let mut commit_prefix: Vec<u8> = Vec::new();
        if is_control_key {
            if let Some(flushed) = self.hangul.flush() {
                commit_prefix.extend_from_slice(flushed.as_bytes());
            }
            self.preedit.clear();
            self.in_preedit = false;
        }
        // Readline-style delete shortcuts. Defaults match iTerm2 /
        // Terminal.app on macOS and Windows Terminal on Windows:
        //   Option/Alt+Backspace → `\e\x7f`  (backward-kill-word)
        //   host_mod+Backspace   → `\x15`    (unix-line-discard, Ctrl+U)
        // host_mod resolves to Cmd on macOS, Ctrl+Shift on Windows/Linux.
        // We match physical key so the Korean layout's mapped char
        // ('ㅣ' etc.) doesn't interfere.
        if matches!(event.logical_key, Key::Named(NamedKey::Backspace)) {
            if self.host_mod() {
                self.input_buf.clear();
                self.send_bytes(b"\x15");
                return;
            }
            if self.modifiers.alt_key() {
                self.buf_pop_word();
                self.send_bytes(b"\x1b\x7f");
                return;
            }
        }
        let bytes: Vec<u8> = match &event.logical_key {
            // Shift+Enter / Option(Alt)+Enter insert a newline instead of
            // submitting. claude code reads a bare LF (0x0a, the byte
            // Ctrl+J sends) as a newline; plain Enter stays CR (0x0d),
            // which submits. We used to send ESC+CR here, but current
            // claude code / Ink doesn't treat that as a newline — so
            // multiline never engaged and the up-arrow fell through to
            // command history instead of moving between lines. claude
            // never negotiates the kitty keyboard protocol (no `CSI ? u`
            // in its startup modes), so CSI 13;2u wouldn't reach it
            // either; a raw LF is the portable answer.
            Key::Named(NamedKey::Enter) => {
                if self.modifiers.shift_key() || self.modifiers.alt_key() {
                    b"\n".to_vec()
                } else {
                    // Plain Enter submits the line: remember it for
                    // instant suggestions and reset the buffer.
                    if !self.input_buf.is_empty() {
                        self.autosuggest.record(&self.input_buf);
                    }
                    self.input_buf.clear();
                    self.current_suggestion = None;
                    b"\r".to_vec()
                }
            }
            Key::Named(NamedKey::Backspace) => {
                self.input_buf.pop();
                b"\x7f".to_vec()
            }
            Key::Named(NamedKey::Tab) => b"\t".to_vec(),
            Key::Named(NamedKey::Escape) => b"\x1b".to_vec(),
            Key::Named(
                nk @ (NamedKey::ArrowUp
                | NamedKey::ArrowDown
                | NamedKey::ArrowRight
                | NamedKey::ArrowLeft),
            ) => {
                // Any cursor motion ends a suggestion: it only makes
                // sense while the cursor sits at the end of the typed
                // line. (→ at end-of-line is intercepted earlier as
                // accept when a suggestion is showing.)
                self.input_buf.clear();
                self.current_suggestion = None;
                let letter = match nk {
                    NamedKey::ArrowUp => 'A',
                    NamedKey::ArrowDown => 'B',
                    NamedKey::ArrowRight => 'C',
                    _ => 'D', // ArrowLeft
                };
                // Carry modifiers so claude code (Ink) / zsh see word-wise
                // and line-wise motion instead of a bare one-cell arrow.
                //   Option(Alt)+←/→ → CSI modifier 3 = backward/forward-word
                //   Cmd(super)+←/→  → Home / End  = line start/end
                // Cmd+Option+arrow never reaches here — it's consumed above
                // as the pane-focus shortcut.
                if self.modifiers.super_key() {
                    match letter {
                        'D' => b"\x1b[H".to_vec(),
                        'C' => b"\x1b[F".to_vec(),
                        _ => format!("\x1b[{letter}").into_bytes(),
                    }
                } else if self.modifiers.alt_key() {
                    format!("\x1b[1;3{letter}").into_bytes()
                } else {
                    // Plain arrow: honor the active pane's DECCKM. When the
                    // inner app (claude code / vim / readline) set
                    // application-cursor mode it expects SS3 (`ESC O A`);
                    // sending CSI (`ESC [ A`) there silently fails, which
                    // is why up/down line-navigation in the prompt did
                    // nothing while modified arrows still worked.
                    let app_cursor = self
                        .ws
                        .lock()
                        .unwrap()
                        .active()
                        .and_then(|p| p.term())
                        .map(|t| t.app_cursor)
                        .unwrap_or(false);
                    if app_cursor {
                        format!("\x1bO{letter}").into_bytes()
                    } else {
                        format!("\x1b[{letter}").into_bytes()
                    }
                }
            }
            _ => match event.text.as_ref() {
                Some(t) => {
                    if std::env::var_os("KASATERM_IME_DEBUG").is_some() {
                        eprintln!(
                            "[key] text={t:?} logical_key={:?} ime_active={} in_preedit={}",
                            event.logical_key, self.ime_active, self.in_preedit
                        );
                    }
                    // Hangul branch (macOS only). On macOS our
                    // set_ime_allowed(false) means the Korean keyboard
                    // layout hands jamo codepoints straight through on
                    // KeyboardInput.text; we feed them into the local
                    // Composer to dodge the NSTextInputContext first-
                    // key drop. Windows / Linux let the OS IME handle
                    // the whole composition (Ime::Preedit/Commit), so
                    // we skip this branch and forward whatever text the
                    // keyboard layer produced as-is.
                    #[cfg(target_os = "macos")]
                    if t.chars().count() == 1 {
                        if let Some(c) = t.chars().next() {
                            if (0x3130..=0x318F).contains(&(c as u32)) {
                                if let Some(commit) = self.hangul.feed(c) {
                                    // Remember the committed text + cursor
                                    // so the overlay can show it until the
                                    // shell echo catches up (cursor moves).
                                    let before = self.ws.lock().ok().and_then(|ws| {
                                        ws.active_pane.clone().and_then(|id| {
                                            ws.panes
                                                .get(&id)
                                                .and_then(|p| p.term())
                                                .map(|t| (t.cursor_row, t.cursor_col))
                                        })
                                    });
                                    self.commit_overlay =
                                        before.map(|b| (commit.clone(), b));
                                    self.input_buf.push_str(&commit);
                                    self.send_bytes(commit.as_bytes());
                                }
                                self.preedit = self.hangul.preedit().unwrap_or_default();
                                self.in_preedit = !self.preedit.is_empty();
                                // Preedit lives in the chrome overlay, not the
                                // PTY grid — without flagging chrome_dirty the
                                // damage gate skips the frame and the composing
                                // syllable only flickers in on blink ticks.
                                self.chrome_dirty = true;
                                if let Some(w) = &self.window {
                                    w.request_redraw();
                                }
                                return;
                            }
                        }
                    }
                    // Non-Hangul ASCII / control characters: flush any
                    // pending Hangul syllable to the shell first, then
                    // forward the new character verbatim.
                    if !t.chars().all(|c| c.is_ascii() && !c.is_control()) {
                        if (self.ime_active || self.in_preedit)
                            && t.chars().any(is_hangul_codepoint)
                        {
                            return;
                        }
                    }
                    if let Some(flushed) = self.hangul.flush() {
                        commit_prefix.extend_from_slice(flushed.as_bytes());
                        self.input_buf.push_str(&flushed);
                        self.preedit.clear();
                        self.in_preedit = false;
                    }
                    // Mirror printable text into the autosuggest buffer.
                    // Control chars (e.g. a lone ESC sequence) don't grow
                    // the visible line, so they don't belong in the prefix.
                    if t.chars().all(|c| !c.is_control()) {
                        self.input_buf.push_str(t);
                    }
                    t.as_bytes().to_vec()
                }
                None => return,
            },
        };
        if std::env::var_os("KASATERM_IME_DEBUG").is_some() {
            eprintln!(
                "[send] prefix={:?} bytes={:?} preedit={:?} in_preedit={} ime_active={}",
                String::from_utf8_lossy(&commit_prefix),
                String::from_utf8_lossy(&bytes),
                self.preedit,
                self.in_preedit,
                self.ime_active
            );
        }
        if commit_prefix.is_empty() {
            self.send_bytes(&bytes);
        } else if is_control_key {
            // claude code (Ink) reads stdin asynchronously and can act on
            // a trailing control byte (\r submit, arrow nav) before it has
            // applied the multibyte syllable in front of it — even when
            // both arrive in one write. Send the committed syllable, let
            // it land, then the control byte. The gap is below human
            // perception and only happens on the (rare) control keypress.
            self.send_bytes(&commit_prefix);
            std::thread::sleep(std::time::Duration::from_millis(12));
            self.send_bytes(&bytes);
        } else {
            // Plain text after a flush has no submit race — ship it in one
            // write so the syllable and the next char stay together.
            commit_prefix.extend_from_slice(&bytes);
            self.send_bytes(&commit_prefix);
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
    // Wire up the tmux shim before anything spawns a shell — every
    // PtySession reads the env vars we set here. install_tmux_shim is
    // best-effort: a missing shim binary just logs and skips, the rest
    // of the binary still works (tmux calls inside the PTY fall back to
    // the real tmux on the user's PATH).
    install_tmux_shim();
    // Headless daemon mode: own the PTY + sockets, no winit/GPU. The GUI
    // spawns `self --daemon` and attaches over the socket; the daemon
    // outlives the GUI so the shell keeps running across GUI restarts.
    let mut args = std::env::args().skip(1);
    let mut is_daemon = false;
    let mut sock_arg: Option<String> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--daemon" => is_daemon = true,
            "--socket" => sock_arg = args.next(),
            _ => {}
        }
    }
    if is_daemon {
        let path = sock_arg.unwrap_or_else(resolve_kasaterm_socket_path);
        return daemon::run_daemon(std::path::PathBuf::from(path))
            .map_err(|e| Box::<dyn Error>::from(e.to_string()));
    }
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

/// Find the bundled tmux shim binary and stage it in a private dir we
/// prepend to child shells' PATH. Also fakes `$TMUX` so a TUI that
/// checks "am I inside tmux?" answers yes, which makes Claude Code's
/// teammateMode route through `tmux split-window` etc — landing every
/// call on our shim instead of going down its own path-finding logic.
fn install_tmux_shim() {
    let shim_src = locate_shim_binary();
    let Some(shim_src) = shim_src else {
        eprintln!("[shim] tmux shim binary not found near {:?}; skipping", std::env::current_exe().ok());
        return;
    };
    let shim_dir = std::env::temp_dir().join(format!("kasaterm-shim-{}", std::process::id()));
    if let Err(e) = std::fs::create_dir_all(&shim_dir) {
        eprintln!("[shim] mkdir {shim_dir:?} failed: {e}");
        return;
    }
    let shim_target_name = if cfg!(windows) { "tmux.exe" } else { "tmux" };
    let target = shim_dir.join(shim_target_name);
    let _ = std::fs::remove_file(&target);
    // Symlink first so we don't pay for a copy each launch and so
    // updates to the shim binary propagate without a reinstall. On
    // Windows symlinks need Developer Mode or admin — fall back to a
    // plain copy so we always end up with a usable shim binary.
    if let Err(e) = stage_shim(&shim_src, &target) {
        eprintln!("[shim] stage {shim_src:?} -> {target:?} failed: {e}");
        return;
    }
    // Cross-pane RPC: stage kasaterm-cli next to the tmux shim so it is
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
    let trace = std::env::var("KASATERM_TMUX_TRACE").unwrap_or_else(|_| {
        std::env::temp_dir()
            .join("kasaterm-tmux-calls.log")
            .to_string_lossy()
            .into_owned()
    });
    let real = std::env::var("KASATERM_REAL_TMUX").unwrap_or_else(|_| {
        real_tmux_candidates()
            .iter()
            .copied()
            .find(|p| std::path::Path::new(p).is_file())
            .unwrap_or("")
            .to_string()
    });
    let fake_tmux = format!(
        "{},{},0",
        std::env::temp_dir()
            .join(format!("kasaterm-tmux-{}.sock", std::process::id()))
            .display(),
        std::process::id()
    );
    std::env::set_var("KASATERM_TMUX_SHIM_DIR", &shim_dir);
    std::env::set_var("KASATERM_TMUX_SHIM_TMUX", &fake_tmux);
    std::env::set_var("KASATERM_TMUX_TRACE", &trace);
    if !real.is_empty() {
        std::env::set_var("KASATERM_REAL_TMUX", &real);
    }
    eprintln!(
        "[shim] dir={shim_dir:?} trace={trace} real_tmux={real:?} fake_tmux={fake_tmux}"
    );
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

/// Look for the `tmux` shim binary next to our own executable. Covers
/// both the dev case (target/debug/tmux sibling to target/debug/kasaterm)
/// and the .app bundle case (Contents/MacOS/tmux sibling to kasaterm).
fn locate_shim_binary() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("KASATERM_TMUX_SHIM_BIN") {
        let p = std::path::PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    // Match the bare binary on Unix and the .exe-suffixed binary that
    // cargo produces on Windows.
    let candidates = if cfg!(windows) {
        ["tmux.exe", "tmux"]
    } else {
        ["tmux", "tmux"]
    };
    for name in candidates {
        let c = dir.join(name);
        if c.is_file() {
            return Some(c);
        }
    }
    // dev fallback: target/debug/tmux when running via `cargo run`
    // from somewhere odd. current_exe usually already points there
    // but be defensive.
    None
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


pub(crate) fn resolve_initial_cwd() -> Option<String> {
    if let Ok(dir) = std::env::var("KASATERM_CWD") {
        if !dir.is_empty() {
            return Some(dir);
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

fn resolve_kasaterm_socket_path() -> String {
    std::env::var("KASATERM_SOCKET_PATH")
        .or_else(|_| std::env::var("CMUX_SOCKET_PATH"))
        .unwrap_or_else(|_| {
            format!(
                "{}/kasaterm-{}.sock",
                std::env::temp_dir().to_string_lossy(),
                std::process::id()
            )
        })
}

/// Locate the kasaterm-cli binary so we can stage it alongside the
/// tmux shim. Same lookup pattern as `locate_shim_binary` — env
/// override first, then sibling of the current exe.
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

/// Common install locations for the real tmux binary. Used when no
/// `KASATERM_REAL_TMUX` env override is provided.
#[cfg(unix)]
fn real_tmux_candidates() -> &'static [&'static str] {
    &["/opt/homebrew/bin/tmux", "/usr/local/bin/tmux", "/usr/bin/tmux"]
}

#[cfg(windows)]
fn real_tmux_candidates() -> &'static [&'static str] {
    // Windows has no canonical tmux install path; rely on the env
    // override when the user has wired up WSL-tmux or a custom build.
    &[]
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
