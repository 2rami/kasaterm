//! tmuxify — sugarloaf-rendered terminal driven by
//! tmux-bridge. Multi-pane: tmux's split-window creates additional
//! panes, layout-change events tell us how to lay them out, and we
//! render each pane inside its rect from the parsed Layout tree.
//! Phase A Task #13/14: wheel + scrollback, IME, selection + clipboard,
//! cursor blink, OSC titles, multi-pane render + focus routing.

mod autosuggest;
mod cells;
mod gpu;
mod socket;
mod theme;

use anyhow::Result;
use std::collections::{HashMap, VecDeque};
use std::error::Error;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tmux_bridge::layout::{parse_layout, Layout};
use tmux_bridge::screen::Cell as GridCell;
use tmux_bridge::screen::Row;
use tmux_bridge::{ScreenUpdate, StartOptions, TmuxEvent, TmuxSession};
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
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    // Middle block (no rounding needed between the two caps).
    g.rect(x, y + r, w, (h - 2.0 * r).max(0.0), col);
    if r <= 0.0 {
        return;
    }
    let steps = r.ceil() as i32;
    for k in 0..steps {
        let yy = k as f32; // distance inward from the cap's outer edge
        // x-inset so the corner traces a circle of radius r.
        let dx = r - (r * r - (r - yy) * (r - yy)).max(0.0).sqrt();
        let rw = (w - 2.0 * dx).max(0.0);
        g.rect(x + dx, y + yy, rw, 1.0, col); // top cap row
        g.rect(x + dx, y + h - 1.0 - yy, rw, 1.0, col); // bottom cap row
    }
}

/// Glyph shown in a sidebar tab's icon chip, chosen from the window label.
fn tab_icon_glyph(name: &str) -> &'static str {
    let l = name.to_ascii_lowercase();
    if name.contains('✳') || l.contains("claude") {
        "✳"
    } else if l.ends_with(".md") {
        "M"
    } else {
        ">"
    }
}

/// Draw a chrome icon glyph centered inside a square chip whose top-left is
/// (`chip_x`, `chip_y`). One place owns icon sizing / centering / hover so every
/// icon button (title bar, sidebar, pane header, image controls) reads
/// identically. The clickable area is `theme::ICON_CHIP` square — hit-test
/// against the same rect.
fn draw_icon(
    g: &mut gpu::GpuRenderer,
    chip_x: f32,
    chip_y: f32,
    glyph: &str,
    color: [u8; 4],
    hovered: bool,
) {
    if hovered {
        round_rect(
            g,
            chip_x,
            chip_y,
            theme::ICON_CHIP,
            theme::ICON_CHIP,
            theme::ICON_CHIP_RADIUS,
            theme::ICON_HOVER_BG,
        );
    }
    let iw = g.measure_chrome_text(glyph, theme::ICON_SIZE, false);
    let gx = chip_x + (theme::ICON_CHIP - iw) / 2.0;
    let gy = chip_y + (theme::ICON_CHIP - theme::ICON_SIZE) / 2.0;
    g.draw_text(
        gx,
        gy,
        glyph,
        gpu::DrawOpts { font_size: theme::ICON_SIZE, color, bold: false, italic: false },
    );
}
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

/// Git-status panel page. `__PORT__` is substituted at window-open time
/// with the live MCP port. Polls `/git-status` every second and renders
/// progressive disclosure: a small green dot when clean, an expanded file
/// list when there are changes. Self-contained (inline CSS/JS) so it can
/// load via `with_html` without a served asset path; the cross-origin
/// fetch relies on the endpoint's `Access-Control-Allow-Origin: *`.
const GIT_PANEL_HTML: &str = r#"<!DOCTYPE html>
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
  .branch { display: flex; align-items: center; gap: 8px; font-weight: 600; font-size: 14px; color: #ecedf3; }
  .dot { width: 9px; height: 9px; border-radius: 50%; flex: 0 0 auto; }
  .dot.clean { background: #3fb950; box-shadow: 0 0 6px #3fb95066; }
  .dot.dirty { background: #d29922; box-shadow: 0 0 6px #d2992266; }
  .dot.error { background: #f85149; }
  .dot.none { background: #787e8a; box-shadow: none; }
  .hint { margin-top: 8px; font-size: 11px; color: #787e8a; }
  .ab { margin-left: auto; display: flex; gap: 10px; font-size: 12px; color: #a0a6b0; }
  .ab b { color: #ecedf3; font-weight: 600; }
  .summary { margin-top: 4px; font-size: 12px; color: #787e8a; }
  .all-wrap { display: none; margin-top: 10px; font-size: 12px; color: #a0a6b0;
    align-items: center; gap: 6px; cursor: pointer; user-select: none; }
  .all-wrap input { margin: 0; cursor: pointer; }
  .groups { margin-top: 12px; display: flex; flex-direction: column; gap: 12px; }
  .group { display: none; }
  .group.show { display: block; }
  .group h4 {
    margin: 0 0 6px; font-size: 11px; text-transform: uppercase;
    letter-spacing: .04em; display: flex; align-items: center; gap: 6px;
  }
  .group h4 .n { color: #787e8a; font-weight: 500; }
  .staged h4 { color: #3fb950; }
  .modified h4 { color: #d29922; }
  .untracked h4 { color: #5a8ce6; }
  ul { margin: 0; padding: 0; list-style: none; }
  .file { margin-bottom: 1px; }
  .file-row { display: flex; align-items: center; gap: 6px;
    font: 12px/1.7 ui-monospace, "SF Mono", Menlo, monospace; }
  .file-row input { margin: 0; flex: 0 0 auto; cursor: pointer; }
  .toggle { color: #787e8a; cursor: pointer; flex: 0 0 auto; width: 10px; text-align: center; }
  .fname { color: #a0a6b0; cursor: pointer; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .diff { margin: 3px 0 6px 22px; padding: 6px 8px; background: #101217; border-radius: 9px;
    font: 11px/1.5 ui-monospace, Menlo, monospace; white-space: pre; overflow-x: auto; max-height: 220px; }
  .diff .add { color: #3fb950; }
  .diff .del { color: #f85149; }
  .diff .hunk { color: #5a8ce6; }
  .diff .ctx { color: #787e8a; }
  .commit { margin-top: 14px; border-top: 1px solid #22262e; padding-top: 12px; }
  .commit textarea { width: 100%; background: #101217; color: #ecedf3; border: 1px solid #22262e;
    border-radius: 9px; padding: 6px 8px; resize: vertical;
    font: 12px/1.4 -apple-system, system-ui, sans-serif; }
  .msg-wrap { position: relative; }
  .msg-wrap textarea { padding-right: 30px; }
  .ai-btn { position: absolute; top: 7px; right: 7px; display: flex; padding: 2px; line-height: 0;
    background: none; border: none; color: #a0a6b0; cursor: pointer; }
  .ai-btn:hover:not(:disabled) { color: #c8a6ff; }
  .ai-btn:disabled { opacity: .5; cursor: default; }
  .actions { display: flex; gap: 8px; margin-top: 8px; }
  .actions button { flex: 1; background: #22262e; color: #ecedf3; border: 1px solid #2e323b;
    border-radius: 9px; padding: 6px 0; font-size: 12px; cursor: pointer; }
  .actions button:hover { background: #2e323b; }
  .actions button:disabled { opacity: .5; cursor: default; }
  #btn-commit { background: #238636; border-color: #2ea043; color: #fff; }
  #btn-commit:hover:not(:disabled) { background: #2ea043; }
  .result { margin-top: 8px; font-size: 11px; white-space: pre-wrap; word-break: break-all; }
  .result.ok { color: #3fb950; }
  .result.bad { color: #f85149; }
  .err { color: #f85149; font-size: 12px; margin-top: 10px; }
</style>
</head>
<body>
  <div class="branch">
    <span id="dot" class="dot clean"></span>
    <span id="branch">…</span>
    <span class="ab">
      <span id="ahead" title="unpushed commits">↑0</span>
      <span id="behind" title="commits behind">↓0</span>
    </span>
  </div>
  <div id="summary" class="summary">connecting…</div>
  <label id="all-wrap" class="all-wrap"><input type="checkbox" id="check-all"> 전체 선택</label>
  <div class="groups">
    <div class="group staged" id="g-staged"><h4>staged <span class="n" id="n-staged"></span></h4><ul id="l-staged"></ul></div>
    <div class="group modified" id="g-modified"><h4>modified <span class="n" id="n-modified"></span></h4><ul id="l-modified"></ul></div>
    <div class="group untracked" id="g-untracked"><h4>untracked <span class="n" id="n-untracked"></span></h4><ul id="l-untracked"></ul></div>
  </div>
  <div id="hint" class="hint" style="display:none">폴더로 이동하면 상태가 떠요</div>
  <div id="err" class="err" style="display:none"></div>
  <div id="commit" class="commit" style="display:none">
    <div class="msg-wrap">
      <textarea id="msg" placeholder="커밋 메시지" rows="2"></textarea>
      <button id="btn-ai" class="ai-btn" title="AI가 변경사항 보고 커밋">
        <svg width="15" height="15" viewBox="0 0 16 16" fill="currentColor"><path d="M8 1l1.3 3.9a2 2 0 0 0 1.3 1.3L14.5 7.5a.3.3 0 0 1 0 .6l-3.9 1.3a2 2 0 0 0-1.3 1.3L8 14.5a.3.3 0 0 1-.6 0l-1.3-3.9a2 2 0 0 0-1.3-1.3L1.5 8.1a.3.3 0 0 1 0-.6l3.9-1.3a2 2 0 0 0 1.3-1.3z"/><path d="M13 1l.4 1.1 1.1.4-1.1.4L13 4l-.4-1.1-1.1-.4 1.1-.4z"/></svg>
      </button>
    </div>
    <div class="actions">
      <button id="btn-commit">commit</button>
      <button id="btn-push">push</button>
    </div>
    <div id="result" class="result" style="display:none"></div>
  </div>
<script>
const PORT = "__PORT__";
const $ = (id) => document.getElementById(id);
const base = "http://127.0.0.1:" + PORT;
// 사용자 선택/펼침 상태. 1초 폴링이 DOM을 다시 그려도 여기서 복원한다.
const checked = new Set();
const expanded = new Set();
const diffCache = new Map();

function colorize(text) {
  const frag = document.createDocumentFragment();
  for (const line of text.split("\n")) {
    const div = document.createElement("div");
    if (line.startsWith("+") && !line.startsWith("+++")) div.className = "add";
    else if (line.startsWith("-") && !line.startsWith("---")) div.className = "del";
    else if (line.startsWith("@@")) div.className = "hunk";
    else div.className = "ctx";
    div.textContent = line || " ";
    frag.appendChild(div);
  }
  return frag;
}

async function loadDiff(path, box) {
  let text = diffCache.get(path);
  if (text === undefined) {
    box.textContent = "로딩…";
    try {
      const r = await fetch(base + "/git-diff?path=" + encodeURIComponent(path), { cache: "no-store" });
      text = (await r.json()).diff || "(변경 없음)";
    } catch (e) { text = "diff 로드 실패"; }
    diffCache.set(path, text);
  }
  box.textContent = "";
  box.appendChild(colorize(text));
}

function fileRow(path) {
  const row = document.createElement("div"); row.className = "file";
  const head = document.createElement("div"); head.className = "file-row";
  const cb = document.createElement("input"); cb.type = "checkbox";
  cb.checked = checked.has(path);
  cb.dataset.path = path;
  cb.onchange = () => { cb.checked ? checked.add(path) : checked.delete(path); };
  const tog = document.createElement("span"); tog.className = "toggle";
  tog.textContent = expanded.has(path) ? "▾" : "▸";
  const name = document.createElement("span"); name.className = "fname";
  name.textContent = path; name.title = path;
  const box = document.createElement("pre"); box.className = "diff";
  box.style.display = expanded.has(path) ? "block" : "none";
  const toggle = () => {
    if (expanded.has(path)) {
      expanded.delete(path); box.style.display = "none"; tog.textContent = "▸";
    } else {
      expanded.add(path); box.style.display = "block"; tog.textContent = "▾"; loadDiff(path, box);
    }
  };
  tog.onclick = toggle; name.onclick = toggle;
  if (expanded.has(path)) loadDiff(path, box);
  head.appendChild(cb); head.appendChild(tog); head.appendChild(name);
  row.appendChild(head); row.appendChild(box);
  return row;
}

function fill(group, files) {
  const n = files.length;
  $("g-" + group).classList.toggle("show", n > 0);
  $("n-" + group).textContent = n ? n : "";
  const ul = $("l-" + group);
  ul.innerHTML = "";
  for (const f of files) ul.appendChild(fileRow(f));
}

function render(d) {
  $("err").style.display = "none";
  $("hint").style.display = "none";
  $("commit").style.display = "none";
  $("all-wrap").style.display = "none";
  if (d.no_repo) {
    $("dot").className = "dot none";
    $("branch").textContent = d.path || "—";
    $("ahead").textContent = ""; $("behind").textContent = "";
    $("summary").textContent = "git 저장소 아님";
    $("hint").style.display = "block";
    fill("staged", []); fill("modified", []); fill("untracked", []);
    return;
  }
  if (d.error) {
    $("dot").className = "dot error";
    $("branch").textContent = "git";
    $("summary").textContent = "";
    $("err").style.display = "block";
    $("err").textContent = d.error;
    return;
  }
  $("branch").textContent = d.branch || "(detached)";
  const ahead = d.ahead || 0, behind = d.behind || 0;
  $("ahead").textContent = "↑" + ahead;
  $("behind").textContent = "↓" + behind;
  const staged = d.staged || [], modified = d.modified || [], untracked = d.untracked || [];
  const total = staged.length + modified.length + untracked.length;
  $("dot").className = "dot " + (d.clean ? "clean" : "dirty");
  $("summary").textContent = d.clean
    ? "working tree clean"
    : total + (total === 1 ? " change" : " changes");
  fill("staged", staged);
  fill("modified", modified);
  fill("untracked", untracked);
  // 변경이 있거나 보낼 커밋이 있으면 커밋/푸시 영역 노출.
  $("commit").style.display = (total > 0 || ahead > 0) ? "block" : "none";
  $("all-wrap").style.display = total > 0 ? "flex" : "none";
  $("btn-push").textContent = ahead > 0 ? ("push ↑" + ahead) : "push";
  $("btn-commit").disabled = total === 0;
}

function showResult(msg, ok) {
  const el = $("result");
  el.textContent = msg;
  el.className = "result " + (ok ? "ok" : "bad");
  el.style.display = "block";
}

async function doCommit() {
  const files = [...checked];
  const message = $("msg").value;
  if (!files.length) { showResult("커밋할 파일을 체크하세요", false); return; }
  if (!message.trim()) { showResult("커밋 메시지를 입력하세요", false); return; }
  $("btn-commit").disabled = true;
  try {
    // No Content-Type header → browser sends text/plain (CORS-safe), so no
    // preflight from this null-origin webview. Server parses the JSON string.
    const r = await fetch(base + "/git-commit", {
      method: "POST",
      body: JSON.stringify({ files: files, message: message })
    });
    const d = await r.json();
    showResult(d.output || (d.ok ? "커밋됨" : "실패"), d.ok);
    if (d.ok) { $("msg").value = ""; checked.clear(); expanded.clear(); diffCache.clear(); }
  } catch (e) { showResult("커밋 요청 실패", false); }
  poll();
}

async function doPush() {
  $("btn-push").disabled = true;
  showResult("푸시 중…", true);
  try {
    const r = await fetch(base + "/git-push", { method: "POST" });
    const d = await r.json();
    showResult(d.output || (d.ok ? "푸시됨" : "실패"), d.ok);
  } catch (e) { showResult("푸시 요청 실패", false); }
  $("btn-push").disabled = false;
  poll();
}

async function doAiCommit() {
  const files = [...checked];
  $("btn-ai").disabled = true;
  showResult("AI에게 커밋 요청 중…", true);
  try {
    // text/plain (no Content-Type) → no CORS preflight; see doCommit.
    const r = await fetch(base + "/git-ai-commit", {
      method: "POST",
      body: JSON.stringify({ files: files })
    });
    const d = await r.json();
    showResult(d.output || (d.ok ? "요청됨" : "실패"), d.ok);
  } catch (e) { showResult("AI 커밋 요청 실패", false); }
  $("btn-ai").disabled = false;
}

async function poll() {
  try {
    const r = await fetch(base + "/git-status", { cache: "no-store" });
    render(await r.json());
  } catch (e) {
    $("dot").className = "dot error";
    $("summary").textContent = "";
    $("commit").style.display = "none";
    $("err").style.display = "block";
    $("err").textContent = "server unreachable :" + PORT;
  }
}

$("btn-commit").onclick = doCommit;
$("btn-push").onclick = doPush;
$("btn-ai").onclick = doAiCommit;
$("check-all").onchange = () => {
  const on = $("check-all").checked;
  document.querySelectorAll(".file-row input[type=checkbox]").forEach((cb) => {
    cb.checked = on;
    const p = cb.dataset.path;
    if (p) { on ? checked.add(p) : checked.delete(p); }
  });
};
poll();
setInterval(poll, 1000);
</script>
</body>
</html>"#;

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
  .new {
    margin-top: 12px; width: 100%; padding: 9px 0; border-radius: 9px;
    background: #22262e; color: #ecedf3; border: 1px dashed #2e323b;
    font-size: 13px; cursor: pointer;
  }
  .new:hover:not(:disabled) { background: #2a313b; border-color: #4a525e; }
  .new:disabled { opacity: .5; cursor: default; }
  .err { color: #f85149; font-size: 12px; margin-top: 10px; }
</style>
</head>
<body>
  <div class="title">세션</div>
  <ul id="list"></ul>
  <button id="btn-new" class="new">+ 새 세션</button>
  <div id="err" class="err" style="display:none"></div>
<script>
const PORT = "__PORT__";
const $ = (id) => document.getElementById(id);
const base = "http://127.0.0.1:" + PORT;
let busy = false;

function render(d) {
  $("err").style.display = "none";
  const count = d.count || 1, active = d.active || 0;
  const saved = Array.isArray(d.saved) ? d.saved : [];
  const ul = $("list");
  ul.innerHTML = "";
  for (let i = 0; i < count; i++) {
    const li = document.createElement("li");
    li.className = "sess" + (i === active ? " active" : "");
    const dot = document.createElement("span"); dot.className = "dot";
    const label = document.createElement("span"); label.className = "label";
    label.textContent = "세션 " + (i + 1);
    const badge = document.createElement("span"); badge.className = "badge";
    if (i === active) badge.textContent = "활성";
    li.appendChild(dot); li.appendChild(label); li.appendChild(badge);
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
    use tmux_bridge::screen::Color;
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
#[derive(Debug, Clone, Copy)]
enum UserEvent {
    Redraw,
}

/// One terminal session: its own pane set, layout, and workspace. The visible
/// session lives in App.{pty,pty_layout,ws}; the rest sit in
/// App.stashed_sessions. Each session's `ws` is its own Arc so its
/// pump_pty_screens threads keep updating it in the background even while
/// another session is on screen (tmux-style detached sessions).
#[allow(dead_code)]
struct Session {
    pty: HashMap<String, Arc<pty_backend::PtySession>>,
    /// Layout of this session's *active* window. The other windows' layouts
    /// sit in `windows` (active slot `None`) — same stash-swap shape the
    /// session list uses one level up.
    pty_layout: Option<pty_backend::PtyLayout>,
    /// All windows in this session by index. The active window's slot is
    /// `None` (its layout lives in `pty_layout`). Switching windows swaps a
    /// slot in/out; every window shares this session's `pty`/`ws`, so window
    /// switches never tear down panes.
    windows: Vec<Option<pty_backend::PtyLayout>>,
    /// Index into `windows` of this session's active window.
    active_window: usize,
    ws: Arc<Mutex<Workspace>>,
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
    pty: HashMap<String, Arc<pty_backend::PtySession>>,
    /// BSP layout tree for multi-pane PTY mode. `None` in tmux mode —
    /// the tmux daemon owns the layout there and ships it via
    /// `%layout-change` instead.
    pty_layout: Option<pty_backend::PtyLayout>,
    /// Monotonic counter for the next "%N" pane id when splitting.
    next_pane_id: u32,
    /// Queued `claude --resume …\n` injections for restored panes, one per
    /// claude pane, fired once each pane's shell prompt is up. Holds the
    /// PtySession Arc directly so it works for panes in any session (active or
    /// stashed background). (session, command, time-to-send).
    pending_restores: Vec<(Arc<pty_backend::PtySession>, String, std::time::Instant)>,
    /// Headless verification: clean-exit (runs `exiting` → save_session_state)
    /// at this instant when KASATERM_AUTOQUIT_MS is set. None disables it.
    autoquit_at: Option<std::time::Instant>,
    /// Queued split directions driven by KASATERM_AUTOSPLIT — headless
    /// repro for the multi-pane render path. Empty in normal use.
    autosplit_plan: Vec<pty_backend::SplitDir>,
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
    /// Bridge to the cmux-compatible socket worker. The socket thread
    /// pushes commands here; the main thread drains them in
    /// `about_to_wait`. None until `start_socket_pty` wires it up.
    socket_handle: Option<socket::PtyBackendHandle>,
    ws: Arc<Mutex<Workspace>>,
    /// All sessions (tmux-style tabs) by tab index. The visible session's slot
    /// holds `None` — its live state is the fields above (pty/pty_layout/ws).
    /// Switching swaps a slot in/out; background sessions keep running via
    /// their own ws Arc, captured by their pump_pty_screens threads.
    sessions: Vec<Option<Session>>,
    /// Labels for sessions persisted on disk at last shutdown. Surfaced in
    /// the session panel as one-click restore rows. Filled once at startup
    /// from `session.json`; cleared as the user manually restores each.
    saved_session_labels: Vec<String>,
    /// On-disk sessions loaded at startup but not yet restored. Parallel to
    /// `saved_session_labels` (same index), so a restore-row click maps to
    /// the right session. Drained as the user restores each.
    saved_sessions_restore: Vec<socket::SessionRestore>,
    /// Index into `sessions` of the visible session (its slot is None).
    active_session: usize,
    /// Windows of the *visible* session, by index. The active window's slot
    /// holds `None` — its live layout is `pty_layout` above. A window is a
    /// pane grouping (one BSP tree); a session can hold several. Switching
    /// windows swaps `pty_layout` ↔ `windows[idx]` while `pty`/`ws` stay put,
    /// so the panes' shells keep running across the switch (same session).
    /// When the visible session is stashed, these move into its `Session`.
    windows: Vec<Option<pty_backend::PtyLayout>>,
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
    hangul: hangul_ime::Composer,
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
    resize_drag: Option<(Vec<u8>, pty_backend::SplitDir)>,
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
    /// When we last recomputed the macOS window title. Rate-limits
    /// `maybe_update_window_title` to ~200ms because it locks the
    /// workspace + calls `ps -A` (process-tree lookup) on every hit,
    /// and a wheel burst fires `RedrawRequested` 60+ times per
    /// second.
    last_window_title_check: Option<Instant>,
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
    /// Git-status panel: a second OS window driving a wry webview, kept
    /// fully separate from the terminal's wgpu window so it can never
    /// disturb the render/damage path. `None` unless KASASPACE_GIT_PANEL
    /// opted in at startup. The webview must outlive its window, so both
    /// are owned here.
    git_panel_window: Option<Arc<Window>>,
    git_panel_webview: Option<wry::WebView>,
    /// Session panel: a second OS window/webview listing the tmux-style
    /// sessions. Same lifetime discipline as the git panel — webview must
    /// outlive its window, so both are owned here.
    session_panel_window: Option<Arc<Window>>,
    session_panel_webview: Option<wry::WebView>,
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
            socket_handle: None,
            ws: Arc::new(Mutex::new(Workspace::default())),
            sessions: vec![None],
            saved_session_labels: Vec::new(),
            saved_sessions_restore: Vec::new(),
            active_session: 0,
            windows: vec![None],
            active_window: 0,
            cell: CellGeom::default(),
            preedit: String::new(),
            in_preedit: false,
            commit_overlay: None,
            ime_active: false,
            hangul: hangul_ime::Composer::new(),
            pane_header_rects: Vec::new(),
            copy_btn_rects: Vec::new(),
            md_content_h: HashMap::new(),
            md_toggle_rects: Vec::new(),
            pane_tab_rects: Vec::new(),
            pane_tab_close_rects: Vec::new(),
            pane_plus_rects: Vec::new(),
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
            titlebar_drag_pending: None,
            last_window_title: None,
            claude_busy_until: None,
            last_claude_status: None,
            last_window_title_check: None,
            last_blink_on: false,
            chrome_dirty: true,
            cursor_px: (0.0, 0.0),
            modifiers: ModifiersState::empty(),
            wheel_accum_y: 0.0,
            last_wheel_emit: Instant::now() - std::time::Duration::from_secs(1),
            last_input_at: Instant::now(),
            font_size: FONT_SIZE,
            ui_zoom: 1.0,
            pane_font_scales: std::collections::HashMap::new(),
            proxy,
            git_panel_window: None,
            git_panel_webview: None,
            session_panel_window: None,
            session_panel_webview: None,
            preview_windows: Vec::new(),
            pane_action_hits: Vec::new(),
            version_anim_start: Instant::now(),
            menu: None,
            git_menu_item: None,
            session_menu_item: None,
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
        if self.sidebar_visible {
            self.sidebar_w_logical
        } else {
            0.0
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

    /// Open the git-status panel in its own OS window when
    /// KASASPACE_GIT_PANEL is set (=1/true). Best-effort: any failure
    /// (window or webview) just logs and leaves the terminal untouched —
    /// the panel is auxiliary and must never block startup. The page polls
    /// `/git-status` on the MCP server (KASASPACE_MCP_PORT, default 8765).
    fn open_git_panel(&mut self, event_loop: &ActiveEventLoop, force: bool) {
        if self.git_panel_window.is_some() {
            return;
        }
        // The menu toggle passes force=true (explicit user action); startup
        // otherwise gates on the KASASPACE_GIT_PANEL env opt-in.
        if !force {
            let on = std::env::var("KASASPACE_GIT_PANEL")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
            if !on {
                return;
            }
        }
        let port =
            std::env::var("KASASPACE_MCP_PORT").unwrap_or_else(|_| "8765".to_string());
        let attrs = WindowAttributes::default()
            .with_title("git status")
            .with_theme(Some(Theme::Dark))
            .with_inner_size(LogicalSize::new(300.0, 460.0));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("[git-panel] window create failed: {e}");
                return;
            }
        };
        let html = GIT_PANEL_HTML.replace("__PORT__", &port);
        // build_as_child (not build): keep winit's own content view. build()
        // replaces it with the webview, so winit's macOS delegate touches a
        // freed view on focus changes (window_did_resign_key) → use-after-
        // free crash. As a child, the webview fills the panel window while
        // winit keeps its view. This is a separate window from the terminal,
        // so there's no wgpu surface to overlap.
        let webview = match wry::WebViewBuilder::new()
            .with_html(html)
            .with_bounds(wry::Rect {
                position: wry::dpi::LogicalPosition::new(0.0, 0.0).into(),
                size: wry::dpi::LogicalSize::new(300.0, 460.0).into(),
            })
            .build_as_child(window.as_ref())
        {
            Ok(wv) => wv,
            Err(e) => {
                eprintln!("[git-panel] webview build failed: {e}");
                return;
            }
        };
        eprintln!("[git-panel] open; polling 127.0.0.1:{port}/git-status");
        self.git_panel_window = Some(window);
        self.git_panel_webview = Some(webview);
    }

    /// Toggle the git panel from the menu: close if open, force-open if not.
    /// Bypasses the env gate since the menu click is an explicit user action.
    fn toggle_git_panel(&mut self, event_loop: &ActiveEventLoop) {
        if self.git_panel_window.is_some() {
            // Drop the webview before the window it borrows from.
            self.git_panel_webview = None;
            self.git_panel_window = None;
        } else {
            self.open_git_panel(event_loop, true);
        }
    }

    /// Open the session panel in its own OS window. Mirrors open_git_panel:
    /// the page polls `/sessions` on the MCP server. Best-effort — any failure
    /// just logs and leaves the terminal untouched.
    fn open_session_panel(&mut self, event_loop: &ActiveEventLoop) {
        if self.session_panel_window.is_some() {
            return;
        }
        let port =
            std::env::var("KASASPACE_MCP_PORT").unwrap_or_else(|_| "8765".to_string());
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

    /// Open a separate preview window (image viewer / markdown editor) for
    /// `path`. Reads the file on the main thread (it's tiny relative to a
    /// frame and only fires on an explicit user action) and injects its
    /// content into a self-contained HTML page — no served asset path, same
    /// `with_html` + `build_as_child` pattern as the git/session panels.
    /// Returns an error (surfaced to the `imgopen`/`mdopen` caller) on a bad
    /// path or any window/webview build failure; the terminal is untouched.
    #[allow(dead_code)]
    fn open_preview_window(
        &mut self,
        event_loop: &ActiveEventLoop,
        kind: socket::PreviewKind,
        path: &str,
    ) -> anyhow::Result<()> {
        let p = std::path::Path::new(path);
        if path.is_empty() || !p.is_file() {
            anyhow::bail!("no such file: {path}");
        }
        let name = p
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string());

        let (title, html, size) = match kind {
            socket::PreviewKind::Image => {
                let bytes = std::fs::read(p)
                    .map_err(|e| anyhow::anyhow!("read failed: {e}"))?;
                let mime = match p
                    .extension()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_ascii_lowercase())
                    .as_deref()
                {
                    Some("png") => "image/png",
                    Some("jpg") | Some("jpeg") => "image/jpeg",
                    Some("gif") => "image/gif",
                    Some("webp") => "image/webp",
                    Some("bmp") => "image/bmp",
                    Some("svg") => "image/svg+xml",
                    _ => "image/png",
                };
                let b64 = base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    &bytes,
                );
                let src = format!("data:{mime};base64,{b64}");
                let html = IMAGE_VIEWER_HTML
                    .replace("__NAME__", &html_escape(&name))
                    .replace("__SRC__", &src);
                (format!("{name} — 이미지"), html, (820.0, 620.0))
            }
            socket::PreviewKind::Markdown => {
                let text = std::fs::read_to_string(p)
                    .map_err(|e| anyhow::anyhow!("read failed: {e}"))?;
                let port = std::env::var("KASASPACE_MCP_PORT")
                    .unwrap_or_else(|_| "8765".to_string());
                // JSON-encode path + content so they drop into the page as
                // safe JS string literals (handles quotes, newlines, unicode).
                let path_js = serde_json::to_string(path).unwrap_or_else(|_| "\"\"".into());
                let content_js =
                    serde_json::to_string(&text).unwrap_or_else(|_| "\"\"".into());
                let html = MARKDOWN_EDITOR_HTML
                    .replace("__NAME__", &html_escape(&name))
                    .replace("__PORT__", &port)
                    .replace("__PATH__", &path_js)
                    .replace("__CONTENT__", &content_js);
                (format!("{name} — 마크다운"), html, (980.0, 680.0))
            }
        };

        let attrs = WindowAttributes::default()
            .with_title(title)
            .with_theme(Some(Theme::Dark))
            .with_inner_size(LogicalSize::new(size.0, size.1));
        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .map_err(|e| anyhow::anyhow!("window create failed: {e}"))?,
        );
        // build_as_child for the same use-after-free reason as the git panel
        // (winit keeps its content view; the webview fills the window).
        let webview = wry::WebViewBuilder::new()
            .with_html(html)
            .with_bounds(wry::Rect {
                position: wry::dpi::LogicalPosition::new(0.0, 0.0).into(),
                size: wry::dpi::LogicalSize::new(size.0, size.1).into(),
            })
            .build_as_child(window.as_ref())
            .map_err(|e| anyhow::anyhow!("webview build failed: {e}"))?;
        eprintln!("[preview] open {kind:?} {path}");
        self.preview_windows.push((window, webview));
        Ok(())
    }

    /// Adjust the live font size by `delta` (in logical points) and
    /// reflow the cell grid + PTY size accordingly. Clamped to a sane
    /// terminal range so the user can't shrink past readability or
    /// blow the window contents out by accident.
    fn change_font_size(&mut self, delta: f32) {
        let new = (self.font_size + delta).clamp(8.0, 40.0);
        if (new - self.font_size).abs() < 0.05 {
            return;
        }
        self.font_size = new;
        // gpu (cell-renderer) path: resize the GpuRenderer's cached cell
        // metrics so layout sees the new size immediately.
        if let Some(gpu) = self.gpu.as_mut() {
            let (cw, ch) = gpu.set_font_size(new);
            self.cell = CellGeom { w: cw, h: ch, baseline: 0.0 };
            if let Some(window) = self.window.as_ref() {
                let (cols, rows) = self.window_cells();
                self.resize_backend(cols, rows);
                self.chrome_dirty = true;
                window.request_redraw();
            }
            return;
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
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(ms));
            let mut payload = text.clone();
            if !payload.ends_with('\n') {
                payload.push('\n');
            }
            if let Some(t) = tmux.as_ref() {
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
    fn pump_pty_screens(
        &self,
        screens: pty_backend::ScreenReceiver<tmux_bridge::screen::ScreenUpdate>,
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
                            update = tmux_bridge::screen::ScreenUpdate {
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
                // conversation summary, vim filename, etc.). Carry it
                // through to PaneState so the chrome header + the
                // macOS window title see the freshest value.
                // Pinned panes (renamed via surface.rename / run_job) keep
                // their agent-set label; only unpinned panes track OSC.
                if let Some(t) = update.title.clone() {
                    if !tab.title_pinned {
                        tab.title = Some(t);
                    }
                }
                // Any tab receiving output flips the pane dirty so the
                // next frame's chrome (tab label, indicator) ticks promptly.
                // Only the active tab's grid is painted; background-tab
                // grids stay in `pane.tabs[i]` for when the user switches.
                let _ = tab;
                pane.dirty = true;
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
        let session = pty_backend::PtySession::start(pty_backend::PtyOptions {
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
        self.pty_layout = Some(pty_backend::PtyLayout::single(&id));
        self.ws.lock().unwrap().active_pane = Some(id);
        Ok(())
    }

    /// Create a new tmux-style session: stash the visible one into its slot,
    /// start a fresh empty session with its own ws/pty/layout, bring up its
    /// first pane.
    fn new_session(&mut self) {
        self.sessions[self.active_session] = Some(Session {
            pty: std::mem::take(&mut self.pty),
            pty_layout: self.pty_layout.take(),
            windows: std::mem::take(&mut self.windows),
            active_window: self.active_window,
            ws: self.ws.clone(),
        });
        self.ws = Arc::new(Mutex::new(Workspace::default()));
        self.pty = HashMap::new();
        self.pty_layout = None;
        self.windows = vec![None];
        self.active_window = 0;
        self.sessions.push(None);
        self.active_session = self.sessions.len() - 1;
        if let Err(e) = self.spawn_session_pane() {
            eprintln!("[session] new session pane spawn failed: {e:#}");
        }
        self.refresh_socket_snapshot();
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// Switch the visible session to tab `idx`: swap the current out to its
    /// slot, swap the target in. Background sessions stay alive (their ws Arc
    /// is still updated by their pump threads).
    fn switch_session(&mut self, idx: usize) {
        if idx == self.active_session || idx >= self.sessions.len() {
            return;
        }
        if self.sessions[idx].is_none() {
            return;
        }
        self.sessions[self.active_session] = Some(Session {
            pty: std::mem::take(&mut self.pty),
            pty_layout: self.pty_layout.take(),
            windows: std::mem::take(&mut self.windows),
            active_window: self.active_window,
            ws: self.ws.clone(),
        });
        let next = self.sessions[idx].take().unwrap();
        self.pty = next.pty;
        self.pty_layout = next.pty_layout;
        self.windows = next.windows;
        self.active_window = next.active_window;
        self.ws = next.ws;
        self.active_session = idx;
        // Reflow PTYs to the current window and repaint.
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        self.refresh_socket_snapshot();
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// Close the session at `idx`. The last session can't be closed (the
    /// terminal always needs one live session). Closing a background session
    /// just drops it; closing the visible one first swaps a neighbor in so the
    /// terminal keeps painting. Dropping a `Session` drops its `PtySession`
    /// Arcs, which kills the child shells — same teardown path as remove_pane.
    fn close_session(&mut self, idx: usize) -> Result<()> {
        if self.sessions.len() <= 1 {
            anyhow::bail!("cannot close the last session");
        }
        if idx >= self.sessions.len() {
            anyhow::bail!("no such session: {idx}");
        }
        if idx == self.active_session {
            // Pick a neighbor to become visible, then drop the active one.
            let target = if idx == 0 { 1 } else { idx - 1 };
            // Drop the active session's live PTYs (kills their shells).
            self.pty.clear();
            self.pty_layout = None;
            // Drop the active session's stashed windows too — their layouts
            // only reference the panes we're killing here.
            self.windows = vec![None];
            self.active_window = 0;
            let next = self.sessions[target]
                .take()
                .expect("neighbor session slot must be occupied");
            self.pty = next.pty;
            self.pty_layout = next.pty_layout;
            self.windows = next.windows;
            self.active_window = next.active_window;
            self.ws = next.ws;
            self.sessions.remove(idx);
            // After removing slot `idx`, every index above it shifts down one.
            self.active_session = if target > idx { target - 1 } else { target };
            let (cols, rows) = self.window_cells();
            self.resize_backend(cols, rows);
            self.publish_pty_layout();
        } else {
            // Background session: drop it (kills its shells) and drop the slot.
            self.sessions[idx] = None;
            self.sessions.remove(idx);
            if idx < self.active_session {
                self.active_session -= 1;
            }
        }
        self.refresh_socket_snapshot();
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
        Ok(())
    }

    /// Restore a saved (on-disk) session at `idx` in the saved-session list:
    /// stash the visible session, spawn the saved session's panes into fresh
    /// live fields, and switch to it. The saved entry is consumed (removed
    /// from both the parallel restore vec and the label list) so a session is
    /// restored at most once per launch. claude panes queue a `--resume`.
    fn restore_saved_session(&mut self, idx: usize) -> Result<()> {
        if idx >= self.saved_sessions_restore.len() {
            anyhow::bail!("no such saved session: {idx}");
        }
        let saved = self.saved_sessions_restore.remove(idx);
        if idx < self.saved_session_labels.len() {
            self.saved_session_labels.remove(idx);
        }
        let (cols, rows) = self.window_cells();
        // Stash the visible session (same swap invariant as new_session).
        self.sessions[self.active_session] = Some(Session {
            pty: std::mem::take(&mut self.pty),
            pty_layout: self.pty_layout.take(),
            windows: std::mem::take(&mut self.windows),
            active_window: self.active_window,
            ws: self.ws.clone(),
        });
        // Fresh live fields for the restored session; pump threads spawned by
        // build_restore_node capture this ws.
        let ws = Arc::new(Mutex::new(Workspace::default()));
        self.ws = ws.clone();
        self.pty = HashMap::new();
        let mut resume = Vec::new();
        let mut window_layouts = Vec::new();
        for node in &saved.windows {
            window_layouts.push(self.build_restore_node(node, cols, rows, &mut resume)?);
        }
        if window_layouts.is_empty() {
            anyhow::bail!("saved session {idx} had no windows");
        }
        let active_window = saved.active_window.min(window_layouts.len() - 1);
        let mut windows: Vec<Option<pty_backend::PtyLayout>> =
            window_layouts.into_iter().map(Some).collect();
        let active_layout = windows[active_window].take();
        if let Some(first) = active_layout
            .as_ref()
            .and_then(|l| l.leaves().first().map(|s| s.to_string()))
        {
            ws.lock().unwrap().active_pane = Some(first);
        }
        self.pty_layout = active_layout;
        self.windows = windows;
        self.active_window = active_window;
        self.sessions.push(None);
        self.active_session = self.sessions.len() - 1;
        self.pending_restores.extend(resume);
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        self.refresh_socket_snapshot();
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
        Ok(())
    }

    /// Create a new window inside the *current* session: stash the visible
    /// window's layout, then bring up a fresh window with a single new pane.
    /// The new pane's PTY joins the session's shared `pty` map and runs in the
    /// same `ws`, so it's a sibling of the existing windows — switching between
    /// them never tears a pane down. Windows are this session's tmux-style
    /// "windows"; the session list one level up is tmux "sessions".
    fn new_window(&mut self) {
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
        self.refresh_socket_snapshot();
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// Switch the visible window to `idx` within the current session: park the
    /// visible window's layout, swap the target's in. `pty`/`ws` are shared
    /// across the session's windows, so no PTY is touched — only which BSP tree
    /// the renderer draws. Focus lands on the target window's first pane.
    fn switch_window(&mut self, idx: usize) {
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
        self.refresh_socket_snapshot();
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
        self.refresh_socket_snapshot();
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

    /// Compress a cwd for the sidebar: home → `~`, then keep the tail if it
    /// runs past `max` chars so the meaningful (deepest) part stays visible.
    /// 탭/헤더 라벨용. 셸이 idle이면 cwd의 마지막 폴더명, 명령 실행 중이면
    /// 그 프로세스명. zsh 4개로 안 보이고 위치/작업이 드러나게.
    fn smart_pane_label(sess: &pty_backend::PtySession) -> Option<String> {
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
        let (cols, rows) = self.window_cells();
        // Export the agent-socket path BEFORE the first PtySession spawn so the
        // initial shell inherits a working KASATERM_SOCKET_PATH. start_socket_pty
        // (called below) binds the actual server at this same path — set_var here
        // wins the race against PtyBackend's env::var lookup at spawn time.
        let socket_path = resolve_kasaterm_socket_path();
        std::env::set_var("KASATERM_SOCKET_PATH", &socket_path);
        std::env::set_var("CMUX_SOCKET_PATH", &socket_path);
        // Light-launch: always start with one fresh pane. The saved session
        // (if any) only contributes its last-active leaf's cwd so the user
        // lands in the same project directory they left from. The full
        // layout tree / claude-resume queue stays on disk for an explicit
        // "restore" action from the session menu (not done on launch).
        let saved_state = socket::load_session_state();
        let saved_cwd: Option<String> = saved_state.as_ref().and_then(|state| {
            let active_session = state.sessions.get(state.active_session)?;
            let win = active_session
                .windows
                .get(active_session.active_window)
                .or_else(|| active_session.windows.first())?;
            first_leaf_cwd(win)
        });
        // Surface the saved sessions in the session panel — each row labelled
        // by the basename of its first leaf's cwd (or "세션 N" as a fallback).
        self.saved_session_labels = saved_state
            .as_ref()
            .map(|state| {
                state
                    .sessions
                    .iter()
                    .enumerate()
                    .map(|(i, s)| {
                        s.windows
                            .first()
                            .and_then(first_leaf_cwd)
                            .map(|p| {
                                std::path::Path::new(&p)
                                    .file_name()
                                    .map(|s| s.to_string_lossy().into_owned())
                                    .unwrap_or(p)
                            })
                            .unwrap_or_else(|| format!("세션 {}", i + 1))
                    })
                    .collect()
            })
            .unwrap_or_default();
        // Keep the parsed sessions in memory (parallel to the labels) so a
        // later restore-row click rebuilds the right on-disk session.
        self.saved_sessions_restore = saved_state.map(|s| s.sessions).unwrap_or_default();
        let cwd = saved_cwd.or_else(resolve_initial_cwd);
        let session = pty_backend::PtySession::start(pty_backend::PtyOptions {
            shell: resolve_default_shell(),
            cwd,
            cols,
            rows,
            env: Vec::new(),
            pane_id: "%0".to_string(),
            initial_scrollback: Vec::new(),
        })?;
        self.pump_pty_screens(session.screens.clone(), "%0".to_string());
        self.pty.insert("%0".to_string(), Arc::new(session));
        self.pty_layout = Some(pty_backend::PtyLayout::single("%0"));
        // Seed active_pane immediately so split / focus shortcuts work
        // before the first ScreenUpdate lands. pump_pty_screens won't
        // overwrite a non-None active_pane.
        self.ws.lock().unwrap().active_pane = Some("%0".to_string());
        // Bring up the kasaterm-cli socket *after* the initial pane(s) are
        // wired so the very first surface.list call sees them.
        self.start_socket_pty();
        Ok(())
    }

    /// Rebuild every saved session (A3 restore). Each session's panes are
    /// spawned into a fresh workspace and laid out per the saved BSP tree;
    /// claude panes get a queued `--resume`. Sessions are built into stashed
    /// slots, then the saved active session is swapped into the live fields —
    /// mirroring the stash-swap invariant new_session/switch_session use.
    #[allow(dead_code)]
    fn restore_sessions(
        &mut self,
        state: socket::RestoreState,
        cols: u16,
        rows: u16,
    ) -> Result<()> {
        let mut resume = Vec::new();
        let mut pane_count = 0usize;
        let mut window_count = 0usize;
        self.sessions.clear();
        for sess in &state.sessions {
            // Fresh workspace per session so each pane's pump thread (which
            // captures self.ws at spawn time) updates the right one. Every
            // window in this session shares this ws/pty map.
            let ws = Arc::new(Mutex::new(Workspace::default()));
            self.ws = ws.clone();
            self.pty = HashMap::new();
            // Rebuild each window's layout tree (spawns its panes).
            let mut window_layouts = Vec::new();
            for node in &sess.windows {
                let layout = self.build_restore_node(node, cols, rows, &mut resume)?;
                pane_count += layout.leaves().len();
                window_layouts.push(layout);
            }
            if window_layouts.is_empty() {
                continue;
            }
            window_count += window_layouts.len();
            let active_window = sess.active_window.min(window_layouts.len() - 1);
            // Active window's layout → pty_layout slot; the rest stay in the
            // windows vec (active slot taken to None to match the live invariant).
            let mut windows: Vec<Option<pty_backend::PtyLayout>> =
                window_layouts.into_iter().map(Some).collect();
            let active_layout = windows[active_window].take();
            // Focus the active window's first leaf so split/focus shortcuts
            // work pre-paint.
            if let Some(first) = active_layout
                .as_ref()
                .and_then(|l| l.leaves().first().map(|s| s.to_string()))
            {
                ws.lock().unwrap().active_pane = Some(first);
            }
            self.sessions.push(Some(Session {
                pty: std::mem::take(&mut self.pty),
                pty_layout: active_layout,
                windows,
                active_window,
                ws,
            }));
        }
        // Swap the active session out of its slot into the live fields.
        let active = state.active_session.min(self.sessions.len() - 1);
        let s = self.sessions[active]
            .take()
            .expect("active session slot occupied");
        self.pty = s.pty;
        self.pty_layout = s.pty_layout;
        self.windows = s.windows;
        self.active_window = s.active_window;
        self.ws = s.ws;
        self.active_session = active;
        eprintln!(
            "[restore] {} session(s), {} window(s), {} pane(s), {} claude resume(s), active={}",
            self.sessions.len(),
            window_count,
            pane_count,
            resume.len(),
            active
        );
        self.pending_restores = resume;
        // SIGWINCH every restored pane to its real rect + publish the layout so
        // the renderer draws the splits (single-pane uses the fallback anyway).
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        Ok(())
    }

    /// Recursively spawn the panes for one restore node and return the matching
    /// live PtyLayout. Leaves spawn a PtySession at the saved cwd (queueing a
    /// claude resume when needed); splits recurse and preserve dir + ratio.
    #[allow(dead_code)]
    fn build_restore_node(
        &mut self,
        node: &socket::RestoreNode,
        cols: u16,
        rows: u16,
        resume: &mut Vec<(Arc<pty_backend::PtySession>, String, std::time::Instant)>,
    ) -> Result<pty_backend::PtyLayout> {
        match node {
            socket::RestoreNode::Leaf(p) => {
                let id = format!("%{}", self.next_pane_id);
                self.next_pane_id += 1;
                let cwd = p
                    .cwd
                    .as_ref()
                    .map(|c| c.to_string_lossy().into_owned())
                    .or_else(resolve_initial_cwd);
                let session = pty_backend::PtySession::start(pty_backend::PtyOptions {
                    shell: resolve_default_shell(),
                    cwd,
                    cols,
                    rows,
                    env: Vec::new(),
                    pane_id: id.clone(),
                    // Restored scrollback → seeded into alacritty so scroll-up
                    // shows the pre-restart screen. The renderer reads
                    // alacritty's own scrollback (display_offset), not our
                    // PaneState.history, so seeding here is what actually shows.
                    initial_scrollback: p.scrollback.clone(),
                })?;
                self.pump_pty_screens(session.screens.clone(), id.clone());
                let arc = Arc::new(session);
                self.pty.insert(id.clone(), arc.clone());
                if p.was_claude {
                    let cmd = match &p.session_id {
                        Some(sid) => format!("claude --resume {sid}\n"),
                        None => "claude --continue\n".to_string(),
                    };
                    // Stagger slightly past the shell prompt (rc files + OSC133
                    // hook). Sent too early the shell eats it.
                    resume.push((
                        arc,
                        cmd,
                        std::time::Instant::now() + std::time::Duration::from_millis(1200),
                    ));
                }
                Ok(pty_backend::PtyLayout::single(id))
            }
            socket::RestoreNode::Split { horizontal, ratio, a, b } => {
                let a_l = self.build_restore_node(a, cols, rows, resume)?;
                let b_l = self.build_restore_node(b, cols, rows, resume)?;
                let dir = if *horizontal {
                    pty_backend::SplitDir::Horizontal
                } else {
                    pty_backend::SplitDir::Vertical
                };
                Ok(pty_backend::PtyLayout::Split {
                    dir,
                    ratio: *ratio,
                    a: Box::new(a_l),
                    b: Box::new(b_l),
                })
            }
        }
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
        layout: &pty_backend::PtyLayout,
        pty: &HashMap<String, Arc<pty_backend::PtySession>>,
        ws: &Workspace,
    ) -> serde_json::Value {
        match layout {
            pty_backend::PtyLayout::Leaf { pane_id } => {
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
            pty_backend::PtyLayout::Split { dir, ratio, a, b } => {
                let dir = match dir {
                    pty_backend::SplitDir::Horizontal => "h",
                    pty_backend::SplitDir::Vertical => "v",
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
        let dirs: Vec<pty_backend::SplitDir> = plan
            .chars()
            .filter_map(|c| match c {
                'h' | 'H' => Some(pty_backend::SplitDir::Horizontal),
                'v' | 'V' => Some(pty_backend::SplitDir::Vertical),
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
    fn start_socket_with(&self, backend: Arc<dyn agent_socket::Backend>) {
        // Model-invoked tools for the claude running inside a pane: the
        // same Backend, exposed over MCP-on-HTTP. Replaces the external
        // python bridge (mcp/kasaspace_mcp.py).
        match kasaspace_mcp::spawn_http_server(backend.clone(), 8765) {
            Ok(port) => {
                eprintln!("[kasaspace-mcp] HTTP MCP on 127.0.0.1:{port}/mcp");
                std::env::set_var("KASASPACE_MCP_PORT", port.to_string());
                // No MCP auto-discovery: write our address into each AI
                // client's config so any agent on this machine finds us.
                kasaspace_mcp::register_clients(port);
            }
            Err(e) => eprintln!("[kasaspace-mcp] HTTP MCP start failed: {e}"),
        }
        let path = resolve_kasaterm_socket_path();
        let server = match agent_socket::Server::bind(&path) {
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

    fn start_socket_tmux(&self, tmux: Arc<tmux_bridge::TmuxSession>) {
        self.start_socket_with(Arc::new(socket::TmuxBackend::new(tmux)));
    }

    /// PTY-mode socket wiring. Builds the shared inbox + snapshot,
    /// stores the handle on self so the main loop can drain commands,
    /// then spawns the server with a PtyBackend that routes through
    /// that same handle.
    fn start_socket_pty(&mut self) {
        let window = self.window.clone();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if let Some(w) = window.as_ref() {
                w.request_redraw();
            }
        });
        let handle = socket::PtyBackendHandle {
            inbox: Arc::new(Mutex::new(Vec::new())),
            snapshot: Arc::new(Mutex::new(socket::PtySnapshot::default())),
            wake,
        };
        self.socket_handle = Some(handle.clone());
        self.refresh_socket_snapshot();
        self.start_socket_with(Arc::new(socket::PtyBackend::new(handle)));
    }

    /// Publish the current pane state to the shared snapshot the
    /// PtyBackend reads from. Call after every mutation that adds /
    /// removes a pane or shifts focus, so external agents see fresh
    /// `surface.list` results on the very next poll.
    fn refresh_socket_snapshot(&self) {
        let Some(handle) = self.socket_handle.as_ref() else { return; };
        let ws = self.ws.lock().unwrap();
        let leaves: Vec<String> = self
            .pty_layout
            .as_ref()
            .map(|t| t.leaves().iter().map(|s| s.to_string()).collect())
            .unwrap_or_else(|| self.pty.keys().cloned().collect());
        let surfaces = leaves
            .iter()
            .map(|id| agent_socket::backend::SurfaceInfo {
                id: id.clone(),
                workspace_id: "local-0".to_string(),
                title: ws
                    .panes
                    .get(id)
                    .and_then(|p| p.title.clone()),
            })
            .collect();
        let mut snap = handle.snapshot.lock().unwrap();
        snap.surfaces = surfaces;
        snap.active_pane = ws.active_pane.clone();
        // Active pane's shell pid → the git panel resolves its cwd from this
        // so it follows whatever directory the focused terminal is in.
        snap.active_shell_pid = ws
            .active_pane
            .as_ref()
            .and_then(|id| self.pty.get(id))
            .and_then(|s| s.shell_pid());
        snap.session_count = self.sessions.len();
        snap.active_session = self.active_session;
        snap.saved_sessions = self.saved_session_labels.clone();
    }

    /// Drain pending socket commands and run them on the main thread.
    /// Called once per loop turn from `about_to_wait`.
    fn drain_socket_inbox(&mut self, event_loop: &ActiveEventLoop) {
        let cmds: Vec<socket::PtyCommand> = match self.socket_handle.as_ref() {
            Some(h) => std::mem::take(&mut *h.inbox.lock().unwrap()),
            None => return,
        };
        if cmds.is_empty() {
            return;
        }
        for cmd in cmds {
            match cmd {
                socket::PtyCommand::Focus { pane_id, reply } => {
                    let known = self.pty.contains_key(&pane_id);
                    if known {
                        self.ws.lock().unwrap().active_pane = Some(pane_id);
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                        let _ = reply.send(Ok(()));
                    } else {
                        let _ = reply.send(Err(anyhow::anyhow!(
                            "no such surface: {pane_id}"
                        )));
                    }
                }
                socket::PtyCommand::Split { axis, reply } => {
                    let dir = match axis {
                        socket::PtySplitAxis::Horizontal => pty_backend::SplitDir::Horizontal,
                        socket::PtySplitAxis::Vertical => pty_backend::SplitDir::Vertical,
                    };
                    let split_res = self.split_active_pane(dir);
                    let answer = split_res.map(|_| {
                        // split_active_pane sets active_pane to the new
                        // leaf; that's the id the client wants back.
                        self.ws
                            .lock()
                            .unwrap()
                            .active_pane
                            .clone()
                            .unwrap_or_default()
                    });
                    let _ = reply.send(answer);
                }
                socket::PtyCommand::SendBytes { pane_id, bytes, reply } => {
                    let target = pane_id.or_else(|| {
                        self.ws.lock().unwrap().active_pane.clone()
                    });
                    let res = match target.and_then(|id| self.pty.get(&id).cloned()) {
                        Some(pty) => pty.send_bytes(&bytes).map_err(anyhow::Error::from),
                        None => Err(anyhow::anyhow!("no surface to send to")),
                    };
                    let _ = reply.send(res);
                }
                socket::PtyCommand::Close { pane_id, reply } => {
                    // Close if the pane is in the pty map OR still a leaf in
                    // the layout tree. The second case is the "zombie": a pane
                    // whose PTY died (or never registered) but whose leaf
                    // lingers in the tree — list_surfaces reads the tree so it
                    // shows up, yet a pty-map-only guard rejected close,
                    // leaving the slot frozen and un-closable. remove_pane is
                    // tree-driven and pty.remove() no-ops when absent, so it
                    // cleans up either case.
                    let in_tree = self
                        .pty_layout
                        .as_ref()
                        .map_or(false, |t| t.leaves().iter().any(|l| l == &pane_id));
                    if self.pty.contains_key(&pane_id) || in_tree {
                        // remove_pane kills the PTY, drops the leaf from
                        // the BSP layout, reassigns focus, and redraws —
                        // same path Cmd+W uses.
                        self.remove_pane(&pane_id);
                        let _ = reply.send(Ok(()));
                    } else {
                        let _ = reply.send(Err(anyhow::anyhow!(
                            "no such surface: {pane_id}"
                        )));
                    }
                }
                socket::PtyCommand::Rename { pane_id, title, reply } => {
                    // Existence via self.pty (layout/pty truth) — ws.panes
                    // may not have the leaf yet right after a split (no
                    // shell output landed). pane_mut creates it so the
                    // title sticks until the first ScreenUpdate fills it.
                    if self.pty.contains_key(&pane_id) {
                        {
                            let mut ws = self.ws.lock().unwrap();
                            let pane = ws.pane_mut(&pane_id);
                            pane.title = Some(title);
                            // Explicit rename pins the label so later OSC
                            // titles from the inner program don't overwrite it.
                            pane.title_pinned = true;
                        }
                        self.chrome_dirty = true;
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                        let _ = reply.send(Ok(()));
                    } else {
                        let _ = reply.send(Err(anyhow::anyhow!(
                            "no such surface: {pane_id}"
                        )));
                    }
                }
                socket::PtyCommand::SetColor { pane_id, color, reply } => {
                    if self.pty.contains_key(&pane_id) {
                        self.ws.lock().unwrap().pane_mut(&pane_id).color = Some(color);
                        self.chrome_dirty = true;
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                        let _ = reply.send(Ok(()));
                    } else {
                        let _ = reply.send(Err(anyhow::anyhow!(
                            "no such surface: {pane_id}"
                        )));
                    }
                }
                socket::PtyCommand::Swap { a, b, reply } => {
                    let both = self.pty.contains_key(&a) && self.pty.contains_key(&b);
                    let swapped = both
                        && self
                            .pty_layout
                            .as_mut()
                            .map(|l| l.swap_leaves(&a, &b))
                            .unwrap_or(false);
                    if swapped {
                        // Re-SIGWINCH each pane to its new rect and
                        // republish the layout so the renderer + socket
                        // snapshot reflect the swapped positions.
                        let (cols, rows) = self.window_cells();
                        self.resize_backend(cols, rows);
                        self.publish_pty_layout();
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                        let _ = reply.send(Ok(()));
                    } else {
                        let _ = reply.send(Err(anyhow::anyhow!(
                            "swap: surface {a} or {b} not found"
                        )));
                    }
                }
                socket::PtyCommand::SwitchSession { idx, reply } => {
                    self.switch_session(idx);
                    let _ = reply.send(Ok(()));
                }
                socket::PtyCommand::SwitchWindow { idx, reply } => {
                    self.switch_window(idx);
                    let _ = reply.send(Ok(()));
                }
                socket::PtyCommand::NewSession { reply } => {
                    self.new_session();
                    let _ = reply.send(Ok(()));
                }
                socket::PtyCommand::CloseSession { idx, reply } => {
                    let res = self.close_session(idx);
                    let _ = reply.send(res);
                }
                socket::PtyCommand::RestoreSession { idx, reply } => {
                    let res = self.restore_saved_session(idx);
                    let _ = reply.send(res);
                }
                socket::PtyCommand::OpenPreview { kind, path, reply } => {
                    // Both images and markdown open as in-window split panes
                    // (wgpu-rendered). Markdown images (![](...)) render as wgpu
                    // textures inline, same path as the image pane.
                    let _ = event_loop;
                    let res = match kind {
                        socket::PreviewKind::Image => {
                            self.split_image_pane(&path, pty_backend::SplitDir::Horizontal)
                        }
                        socket::PreviewKind::Markdown => {
                            self.split_markdown_pane(&path, pty_backend::SplitDir::Horizontal)
                        }
                    };
                    let _ = reply.send(res);
                }
                socket::PtyCommand::SetPanel { which, open, reply } => {
                    self.set_panel_window(event_loop, which, open);
                    let _ = reply.send(Ok(()));
                }
                socket::PtyCommand::ResizePanel { which, w, h, reply } => {
                    let res = self.resize_panel_window(which, w, h);
                    let _ = reply.send(res);
                }
                socket::PtyCommand::PanelInfo { which, reply } => {
                    let res = self.panel_window_info(which);
                    let _ = reply.send(res);
                }
                socket::PtyCommand::Peek { pane_id, lines, reply } => {
                    // Reuse scrollback_lines (history + current screen as
                    // trimmed text) and hand back just the requested tail.
                    let ws = self.ws.lock().unwrap();
                    let res = match ws.panes.get(&pane_id) {
                        Some(pane) => {
                            let mut all = scrollback_lines(pane);
                            let start = all.len().saturating_sub(lines);
                            Ok(all.split_off(start).join("\n"))
                        }
                        None => Err(anyhow::anyhow!("no such surface: {pane_id}")),
                    };
                    let _ = reply.send(res);
                }
            }
        }
        self.refresh_socket_snapshot();
    }

    /// Open or close a panel window by kind (MCP-driven, mirrors the menu
    /// toggles). `open=true` is idempotent — `open_*` already no-ops when the
    /// window exists.
    fn set_panel_window(
        &mut self,
        event_loop: &ActiveEventLoop,
        which: agent_socket::backend::PanelKind,
        open: bool,
    ) {
        use agent_socket::backend::PanelKind;
        match (which, open) {
            (PanelKind::Git, true) => self.open_git_panel(event_loop, true),
            (PanelKind::Git, false) => {
                self.git_panel_webview = None;
                self.git_panel_window = None;
            }
            (PanelKind::Session, true) => self.open_session_panel(event_loop),
            (PanelKind::Session, false) => {
                self.session_panel_webview = None;
                self.session_panel_window = None;
            }
        }
    }

    /// Resize a panel window and re-bound its webview to fill it. Same
    /// full-bleed rebound the Resized handler does, but driven by MCP so a
    /// caller can exercise the responsive path without a mouse drag.
    fn resize_panel_window(
        &self,
        which: agent_socket::backend::PanelKind,
        w: u32,
        h: u32,
    ) -> anyhow::Result<()> {
        use agent_socket::backend::PanelKind;
        let (win, wv) = match which {
            PanelKind::Git => (self.git_panel_window.as_ref(), self.git_panel_webview.as_ref()),
            PanelKind::Session => (
                self.session_panel_window.as_ref(),
                self.session_panel_webview.as_ref(),
            ),
        };
        let win = win.ok_or_else(|| anyhow::anyhow!("panel not open"))?;
        let _ = win.request_inner_size(winit::dpi::LogicalSize::new(w as f64, h as f64));
        if let Some(wv) = wv {
            wv.set_bounds(wry::Rect {
                position: wry::dpi::LogicalPosition::new(0.0, 0.0).into(),
                size: wry::dpi::LogicalSize::new(w as f64, h as f64).into(),
            })?;
        }
        Ok(())
    }

    /// Report a panel window's geometry (window inner size + webview bounds,
    /// both physical px) so an MCP caller can verify the webview tracks the
    /// window after a resize — no screenshot needed.
    fn panel_window_info(
        &self,
        which: agent_socket::backend::PanelKind,
    ) -> anyhow::Result<agent_socket::backend::PanelGeom> {
        use agent_socket::backend::{PanelGeom, PanelKind};
        let (win, wv) = match which {
            PanelKind::Git => (self.git_panel_window.as_ref(), self.git_panel_webview.as_ref()),
            PanelKind::Session => (
                self.session_panel_window.as_ref(),
                self.session_panel_webview.as_ref(),
            ),
        };
        let open = win.is_some();
        let (win_w, win_h) = win
            .map(|w| {
                let s = w.inner_size();
                (s.width, s.height)
            })
            .unwrap_or((0, 0));
        let scale = win.map(|w| w.scale_factor()).unwrap_or(1.0);
        let (view_w, view_h) = wv
            .and_then(|v| v.bounds().ok())
            .map(|b| {
                let s = b.size.to_physical::<u32>(scale);
                (s.width, s.height)
            })
            .unwrap_or((0, 0));
        Ok(PanelGeom { open, win_w, win_h, view_w, view_h })
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
            let grow = ((py - TITLE_HEIGHT).max(0.0) / self.cell.h).floor() as i32;
            for leaf in layout.leaves() {
                if let Layout::Pane { id, x, y, w, h } = leaf {
                    let (bx, by, bw, bh) = (*x as i32, *y as i32, *w as i32, *h as i32);
                    if gcol >= bx && gcol < bx + bw && grow >= by && grow < by + bh {
                        // Local cell uses the body origin: box edge + header
                        // band + inner inset, matching the render origin.
                        let box_left = sb + WINDOW_PADDING + bx as f32 * self.cell.w;
                        let box_top = TITLE_HEIGHT + by as f32 * self.cell.h;
                        let lc = ((px - box_left - PANE_INNER_X).max(0.0) / self.cell.w).floor()
                            as u16;
                        let lr = ((py - box_top - header_h - PANE_INNER_Y).max(0.0)
                            / self.cell.h)
                            .floor() as u16;
                        let pid = format!("%{id}");
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
        let lc =
            ((px - sb - WINDOW_PADDING - PANE_INNER_X).max(0.0) / self.cell.w).floor() as u16;
        let lr = ((py - TITLE_HEIGHT - PANE_INNER_Y).max(0.0) / self.cell.h).floor() as u16;
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

    /// The PtySession that currently has keyboard focus, if any. Used
    /// by every routing-by-active-pane code path in PTY mode.
    /// PtySession of a pane's currently-active tab. Use this instead of
    /// `self.pty.get(outer_id)` — after a cross-pane tab drag the layout
    /// id and the active tab's pid diverge, and the direct lookup misses.
    /// Drives wheel scroll / mouse-reporting / pane-targeted send_bytes.
    fn pty_for_pane(&self, outer_id: &str) -> Option<&Arc<pty_backend::PtySession>> {
        let ws = self.ws.lock().ok()?;
        let pid = ws
            .panes
            .get(outer_id)
            .and_then(|p| p.tabs.get(p.active_tab).and_then(|t| t.pid.clone()))
            .unwrap_or_else(|| outer_id.to_string());
        drop(ws);
        self.pty.get(&pid)
    }

    fn active_pty(&self) -> Option<&Arc<pty_backend::PtySession>> {
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
        let lw = (raw_lw - self.effective_sidebar_w() - 2.0 * WINDOW_PADDING).max(0.0);
        // Top: TITLE_HEIGHT (chrome strip). Bottom: WINDOW_PADDING. The
        // asymmetry is intentional — the strip replaces the top padding.
        let lh = (raw_lh - TITLE_HEIGHT - WINDOW_PADDING).max(0.0);
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
        self.refresh_socket_snapshot();
    }

    /// Resize every backend session so its grid matches the new window
    /// size. In tmux mode the daemon redistributes for us. In PTY mode
    /// we walk the BSP tree and SIGWINCH each leaf to its own rect.
    fn resize_backend(&self, cols: u16, rows: u16) {
        if let Some(tmux) = self.tmux.as_ref() {
            let _ = tmux.resize_client(cols, rows);
            return;
        }
        let Some(tree) = self.pty_layout.as_ref() else { return; };
        // When the workspace is split, every pane wears a per-pane
        // header strip. That strip eats a few cell rows at the top of
        // each pane's bounding box, so the PTY's usable grid shrinks
        // by the same amount — otherwise claude code paints its
        // statusline / `bypass…` row off the bottom edge.
        let leaves = tree.leaves().len();
        // Header strip (only when split) eats off the top of each pane box;
        // the rest is shared with the cell inset below.
        let header_px = if leaves > 1 { PANE_HEADER_HEIGHT } else { 0.0 };
        // Per-leaf usable cells, indexed by outer pane id (the layout key,
        // which after cross-pane drag is NOT guaranteed to equal any tab's
        // pid). Walk ws.panes and resize each pane's tab PtySessions to its
        // outer rect — single source of truth, works for both primary and
        // in-pane secondary tabs.
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
        // Per-pane font scale shrinks/grows that pane's usable cells: bigger
        // glyphs ⇒ fewer cols/rows in the same box. 1.0 panes keep the exact
        // integer-cell math; scaled panes divide the base cell span by the
        // factor (the box stays on the base grid, matching the per-slot render
        // which sizes glyphs by the same factor). Keyed by pty/leaf id.
        let scale_of = self.pane_font_scales.clone();
        let cw = self.cell.w.max(1.0);
        let ch = self.cell.h.max(1.0);
        let mut leaf_cells: HashMap<String, (u16, u16)> = HashMap::new();
        for (id, _x, _y, w, h) in tree.leaf_rects(cols, rows) {
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
    fn divider_at_px(&self, x: f32, y: f32) -> Option<(Vec<u8>, pty_backend::SplitDir)> {
        let tree = self.pty_layout.as_ref()?;
        if tree.leaves().len() <= 1 {
            return None;
        }
        let (cols, rows) = self.window_cells();
        let pad = WINDOW_PADDING + self.effective_sidebar_w();
        let tol = 6.0_f32;
        for d in tree.dividers(cols, rows) {
            match d.dir {
                pty_backend::SplitDir::Horizontal => {
                    let seam_x = pad + d.edge as f32 * self.cell.w;
                    let y0 = TITLE_HEIGHT + d.span_start as f32 * self.cell.h;
                    let y1 = y0 + d.span_len as f32 * self.cell.h;
                    if (x - seam_x).abs() <= tol && y >= y0 && y <= y1 {
                        return Some((d.path, d.dir));
                    }
                }
                pty_backend::SplitDir::Vertical => {
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
    fn split_active_pane(&mut self, dir: pty_backend::SplitDir) -> Result<()> {
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
        let session = pty_backend::PtySession::start(pty_backend::PtyOptions {
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
        let session = pty_backend::PtySession::start(pty_backend::PtyOptions {
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

    /// Drop a non-primary tab: kill its PTY, remove the pid map entry, drop
    /// the slot. The primary tab (index 0, pid == outer pane id) can't be
    /// closed this way — callers fall through to `remove_pane` for that.
    fn close_tab(&mut self, outer: &str, idx: usize) {
        let pid_opt: Option<String> = {
            let ws = self.ws.lock().unwrap();
            ws.panes
                .get(outer)
                .and_then(|p| p.tabs.get(idx))
                .and_then(|t| t.pid.clone())
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

    /// Split the active pane and fill the new pane with a decoded image
    /// instead of a shell. Mirrors `split_active_pane` but skips the
    /// PtySession: the pane has no PTY, so `active_pty()` returns None (key
    /// input is dropped) and the render loop paints the texture into the
    /// pane box. Backs the `imgopen` shim (OpenPreview → Image).
    fn split_image_pane(&mut self, path: &str, dir: pty_backend::SplitDir) -> Result<()> {
        if self.tmux.is_some() {
            anyhow::bail!("image pane unsupported on tmux backend");
        }
        let p = std::path::Path::new(path);
        if path.is_empty() || !p.is_file() {
            anyhow::bail!("no such file: {path}");
        }
        let image = decode_image_rgba(p)?;
        let name = p
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string());
        let Some(active) = self.ws.lock().unwrap().active_pane.clone() else {
            anyhow::bail!("no active pane to split");
        };
        let new_id = format!("%{}", self.next_pane_id);
        self.next_pane_id += 1;
        {
            let mut ws = self.ws.lock().unwrap();
            let pane = ws.pane_mut(&new_id);
            pane.content = PaneContent::Image(Arc::new(image));
            // Pin the filename as the header label so the OSC-title path
            // (which only fires for PTY panes anyway) can't clobber it.
            pane.title = Some(name);
            pane.title_pinned = true;
            pane.dirty = true;
            // Headless-test handles: seed the image-view state from env so a
            // verify cycle can capture zoomed/rotated frames without a key
            // synthesis path. Real users still drive zoom/rot via +/-/r/0.
            if let Ok(z) = std::env::var("KASATERM_TEST_IMG_ZOOM").map(|s| s.parse::<f32>()) {
                if let Ok(z) = z {
                    pane.image_zoom = z.clamp(1.0, 8.0);
                }
            }
            if let Ok(r) = std::env::var("KASATERM_TEST_IMG_ROT").map(|s| s.parse::<u8>()) {
                if let Ok(r) = r {
                    pane.image_rot = r % 4;
                }
            }
        }
        let layout = self
            .pty_layout
            .as_mut()
            .expect("pty_layout set in start_pty");
        if !layout.split_leaf(&active, dir, new_id.clone()) {
            self.ws.lock().unwrap().panes.remove(&new_id);
            self.next_pane_id -= 1;
            anyhow::bail!("active pane not found in layout");
        }
        // Keep focus on the shell that opened the image — the image pane takes
        // no keyboard input, so handing it focus would just force the user to
        // click back to keep typing.
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        Ok(())
    }

    /// Split the active pane and fill the new pane with a rendered markdown
    /// document. Like `split_image_pane` but the new pane *does* become active
    /// so the user can scroll it (markdown can exceed the pane height); key
    /// input is still dropped (no PTY) — only wheel/scroll keys act on it.
    fn split_markdown_pane(&mut self, path: &str, dir: pty_backend::SplitDir) -> Result<()> {
        if self.tmux.is_some() {
            anyhow::bail!("markdown pane unsupported on tmux backend");
        }
        let p = std::path::Path::new(path);
        if path.is_empty() || !p.is_file() {
            anyhow::bail!("no such file: {path}");
        }
        let text = std::fs::read_to_string(p)
            .map_err(|e| anyhow::anyhow!("read failed: {e}"))?;
        let name = p
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string());
        let Some(active) = self.ws.lock().unwrap().active_pane.clone() else {
            anyhow::bail!("no active pane to split");
        };
        let new_id = format!("%{}", self.next_pane_id);
        self.next_pane_id += 1;
        let doc = build_markdown_doc(&new_id, p, &text);
        {
            let mut ws = self.ws.lock().unwrap();
            // Test hook: open straight into Raw mode (for screenshots).
            let raw_mode = std::env::var_os("KASATERM_MD_RAW").is_some();
            let mut edit_lines: Vec<String> = if raw_mode {
                text.split('\n').map(String::from).collect()
            } else {
                Vec::new()
            };
            if raw_mode && edit_lines.is_empty() {
                edit_lines.push(String::new());
            }
            let pane = ws.pane_mut(&new_id);
            pane.content = PaneContent::Markdown(MarkdownPane {
                doc: Arc::new(doc),
                raw_mode,
                edit_lines,
                cur_line: 0,
                cur_col: 0,
                scroll: 0,
            });
            pane.title = Some(name);
            pane.title_pinned = true;
            pane.dirty = true;
        }
        let layout = self
            .pty_layout
            .as_mut()
            .expect("pty_layout set in start_pty");
        if !layout.split_leaf(&active, dir, new_id.clone()) {
            self.ws.lock().unwrap().panes.remove(&new_id);
            self.next_pane_id -= 1;
            anyhow::bail!("active pane not found in layout");
        }
        // Markdown panes take focus so the wheel/PageDown path scrolls them.
        self.ws.lock().unwrap().active_pane = Some(new_id);
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        Ok(())
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
        // expect from a regular terminal.
        if self.tmux.is_none() && self.pty.is_empty() {
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
        let (cols, rows) = self.pane_cells(source).unwrap_or_else(|| self.window_cells());
        let cwd = resolve_initial_cwd();
        let new_id = format!("%{}", self.next_pane_id);
        self.next_pane_id += 1;
        let session = pty_backend::PtySession::start(pty_backend::PtyOptions {
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
            DropZone::Right => (pty_backend::SplitDir::Horizontal, true),
            DropZone::Left => (pty_backend::SplitDir::Horizontal, false),
            DropZone::Down => (pty_backend::SplitDir::Vertical, true),
            DropZone::Up => (pty_backend::SplitDir::Vertical, false),
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
            DropZone::Left => (pty_backend::SplitDir::Horizontal, true),
            DropZone::Right => (pty_backend::SplitDir::Horizontal, false),
            DropZone::Up => (pty_backend::SplitDir::Vertical, true),
            DropZone::Down => (pty_backend::SplitDir::Vertical, false),
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
            self.pty_layout = Some(pty_backend::PtyLayout::single(&new_outer));
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

    /// Remove the focused pane from the BSP tree and drop its PTY
    /// session. Focus moves to the next pane in document order
    /// (wrapping to the previous when we just closed the last one).
    /// Last-pane close is a no-op — quitting the window is the
    /// user's exit there.
    fn close_active_pane(&mut self) {
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
        ws.active_pane = Some(leaves[new_idx].clone());
        drop(ws);
        self.refresh_socket_snapshot();
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
            self.ws.lock().unwrap().active_pane = Some(id);
            self.refresh_socket_snapshot();
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
        let tree = self.pty_layout.as_ref()?;
        let (cols, rows) = self.window_cells();
        let rects = tree.leaf_rects(cols, rows);
        if rects.len() <= 1 {
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
        let rects = tree.leaf_rects(cols, rows);
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

    /// Relocate `moving` next to `target` along the edge given by `zone`.
    /// Detaches the moving leaf (its PTY stays alive) and re-attaches it
    /// beside the target, then resizes every pane to its new rect. No-op
    /// when source and target are the same pane.
    fn move_pane(&mut self, moving: &str, target: &str, zone: DropZone) {
        if moving == target {
            return;
        }
        let (dir, before) = match zone {
            DropZone::Left => (pty_backend::SplitDir::Horizontal, true),
            DropZone::Right => (pty_backend::SplitDir::Horizontal, false),
            DropZone::Up => (pty_backend::SplitDir::Vertical, true),
            DropZone::Down => (pty_backend::SplitDir::Vertical, false),
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
        // Dispatch by which backend is wired up. The hex encoding is
        // a tmux send-keys quirk (the daemon decodes hex pairs back
        // to bytes itself); for the pty backend we hand the raw bytes
        // straight to the PTY writer.
        if let Some(tmux) = self.tmux.as_ref() {
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
        pty: &HashMap<String, Arc<pty_backend::PtySession>>,
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
                if let Some(pty) = self.pty_for_pane(id) {
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
                if let Some(pty) = self.pty_for_pane(id) {
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
            if self.tmux.is_some() {
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
                            pty_backend::SplitDir::Vertical
                        } else {
                            pty_backend::SplitDir::Horizontal
                        };
                        if let Err(e) = self.split_active_pane(dir) {
                            eprintln!("[tmuxify] split failed: {e}");
                        }
                        return;
                    }
                    if code == KeyCode::KeyE {
                        if let Err(e) = self.split_active_pane(pty_backend::SplitDir::Vertical) {
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
                        self.new_window();
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

    /// Phase 2a path. Collects every pane's live cell grid and hands
    /// it to the cell-renderer pipeline. Chrome (sidebar, tabs,
    /// headers, cursor block, selection, preedit) is intentionally
    /// not drawn yet — Phase 2b+ will reattach those via the same
    /// pipeline / atlas.
    /// Self-only snapshot used by `paint_gpu_overlays`. Built before
    /// we borrow `self.gpu` mutably so the renderer pass can run
    /// without a re-entrant `&self` read. All coordinates here are
    /// already cell-space — the renderer-side helper applies cell
    /// metric multiplication.
    fn gpu_overlay_snapshot(&self) -> GpuOverlay {
        let preedit_text = self.preedit.clone();
        let commit_overlay = self.commit_overlay.clone();
        // Active pane's font multiplier — the overlay anchors to this same
        // pane (see pane_origin below), so its cell size must match the
        // pane's zoomed glyphs, not the base grid.
        let pane_font_scale = self
            .target_pane()
            .and_then(|id| self.pane_font_scales.get(&id).copied())
            .unwrap_or(1.0);
        let snap = {
            let ws = self.ws.lock().unwrap();
            // Active pane's top-left in cell units. When the workspace is
            // split the cursor/preedit overlay must anchor to THIS pane,
            // not the global origin (which is the left/top pane).
            let pane_origin = ws
                .active_pane
                .as_ref()
                .and_then(|aid| {
                    ws.layout.as_ref().and_then(|l| {
                        l.leaves().into_iter().find_map(|n| match n {
                            Layout::Pane { id, x, y, .. } if format!("%{id}") == *aid => {
                                Some((*x, *y))
                            }
                            _ => None,
                        })
                    })
                })
                .unwrap_or((0u16, 0u16));
            ws.active_pane.clone().and_then(|id| {
                ws.panes.get(&id).map(|pane| {
                    // Preedit sits exactly on the reported PTY cursor —
                    // that's where the next char lands. We used to bump
                    // the column to the row's last filled cell to dodge
                    // tail padding, but a TUI's grey placeholder ("Type
                    // something") counts as filled, so that dragged the
                    // composing syllable past it to the line's end. The
                    // cursor column is already correct (incl. trailing
                    // spaces the PTY echoes), so trust it directly.
                    // Image/markdown panes have no PTY cursor — their terminal
                    // block cursor stays hidden (the Raw editor draws its own).
                    let (cur_row, cur_col, cur_vis, cols) = match pane.term() {
                        Some(t) => (
                            t.cursor_row,
                            t.cursor_col,
                            t.cursor_visible,
                            t.cells.first().map(|r| r.len()).unwrap_or(80) as u16,
                        ),
                        None => (0, 0, false, 80),
                    };
                    let (base_row, base_col) = (cur_row, cur_col);
                    // Until the committed syllable's echo lands (cursor
                    // still where it was at commit time), draw the
                    // committed text in front of the preedit at that spot.
                    let (display, prow, pcol) = match &commit_overlay {
                        Some((ctext, before)) if *before == (cur_row, cur_col) => {
                            (format!("{ctext}{preedit_text}"), before.0, before.1)
                        }
                        _ => (preedit_text.clone(), base_row, base_col),
                    };
                    (
                        cur_row,
                        cur_col,
                        cur_vis,
                        cols,
                        prow,
                        pcol,
                        display,
                        pane_origin.0,
                        pane_origin.1,
                    )
                })
            })
        };
        let (
            cursor_row,
            cursor_col,
            cursor_visible,
            cols,
            preedit_row,
            preedit_col,
            preedit,
            pane_x,
            pane_y,
        ) = snap.unwrap_or((0, 0, false, 80, 0, 0, preedit_text.clone(), 0, 0));
        // When split OR any pane is multi-tab, every pane body is pushed
        // down by its header band. The cursor / preedit / selection
        // overlays anchor off the same origin as the cells, so they must
        // apply the identical shift — otherwise the cursor floats up into
        // the header row (which is exactly what made it appear one line
        // above the actual prompt after a cross-pane tab drop).
        let show_headers = self
            .pty_layout
            .as_ref()
            .is_some_and(|t| t.leaves().len() > 1)
            || self
                .ws
                .lock()
                .ok()
                .map(|ws| ws.panes.values().any(|p| p.tabs.len() > 1))
                .unwrap_or(false);
        let header_shift = if show_headers { PANE_HEADER_HEIGHT } else { 0.0 };
        GpuOverlay {
            cell_w: self.cell.w,
            cell_h: self.cell.h,
            pad_x: WINDOW_PADDING + self.effective_sidebar_w() + pane_x as f32 * self.cell.w + PANE_INNER_X,
            pad_y: TITLE_HEIGHT + pane_y as f32 * self.cell.h + header_shift + PANE_INNER_Y,
            cursor_row,
            cursor_col,
            cursor_visible,
            cols,
            blink_on: self.cursor_blink_on(Instant::now()),
            preedit,
            preedit_row,
            preedit_col,
            font_size: self.font_size,
            font_scale: pane_font_scale,
            selection: self.selection,
            suggestion: self.current_suggestion.clone().unwrap_or_default(),
        }
    }

    /// Phase 2d overlays — pure free function on the snapshot so it
    /// doesn't fight a mutable borrow on `self.gpu`.
    fn paint_gpu_overlays(g: &mut gpu::GpuRenderer, ov: &GpuOverlay) {
        // Effective cell size for THIS pane: base metric × pane zoom. The
        // anchor (pad_x/pad_y) stays on the base grid because the pane's
        // top-left lives there, but every per-column/row step must use the
        // zoomed size or the cursor/preedit/selection drift right & down
        // as the pane is shrunk.
        let cw = ov.cell_w * ov.font_scale;
        let ch = ov.cell_h * ov.font_scale;
        if ov.cursor_visible && ov.blink_on && ov.preedit.is_empty() {
            let cx = ov.pad_x + ov.cursor_col as f32 * cw;
            let cy = ov.pad_y + ov.cursor_row as f32 * ch;
            let mut c = cells::ITERM_CURSOR;
            c[3] = 140; // ~0.55 alpha
            g.rect(cx, cy, cw, ch, c);
        }
        // Inline autosuggestion ghost text — dim, on the same baseline as
        // committed cells, starting at the cursor and clipped to the row's
        // right edge so it never wraps. Drawn only when not composing.
        if ov.preedit.is_empty() && !ov.suggestion.is_empty() {
            let gx = ov.pad_x + ov.cursor_col as f32 * cw;
            let gy = ov.pad_y + ov.cursor_row as f32 * ch;
            let max_cells = ov.cols.saturating_sub(ov.cursor_col) as u32;
            if max_cells > 0 {
                g.draw_ghost(gx, gy, &ov.suggestion, max_cells, ov.font_scale);
            }
        }
        if !ov.preedit.is_empty() {
            let px = ov.pad_x + ov.preedit_col as f32 * cw;
            let py = ov.pad_y + ov.preedit_row as f32 * ch;
            // Route preedit through the cell-grid path so the composing
            // syllable sits on the same baseline as committed text
            // instead of floating above the row.
            g.draw_preedit(px, py, &ov.preedit, cells::ITERM_CURSOR, ov.font_scale);
        }
        if let Some(sel) = ov.selection {
            let (start, stop) = if (sel.anchor.1, sel.anchor.0) <= (sel.end.1, sel.end.0) {
                (sel.anchor, sel.end)
            } else {
                (sel.end, sel.anchor)
            };
            let color = cells::ITERM_SELECTION;
            if start.1 == stop.1 {
                let x = ov.pad_x + start.0 as f32 * cw;
                let y = ov.pad_y + start.1 as f32 * ch;
                let w = (stop.0 - start.0 + 1) as f32 * cw;
                g.rect(x, y, w, ch, color);
            } else {
                let x = ov.pad_x + start.0 as f32 * cw;
                let y = ov.pad_y + start.1 as f32 * ch;
                let row_w = (ov.cols - start.0) as f32 * cw;
                g.rect(x, y, row_w, ch, color);
                for r in (start.1 + 1)..stop.1 {
                    let yy = ov.pad_y + r as f32 * ch;
                    g.rect(ov.pad_x, yy, ov.cols as f32 * cw, ch, color);
                }
                let yy = ov.pad_y + stop.1 as f32 * ch;
                let last_w = (stop.0 + 1) as f32 * cw;
                g.rect(ov.pad_x, yy, last_w, ch, color);
            }
        }
    }

    fn render_frame_gpu(&mut self, scale: f32) {
        let Some(window) = self.window.as_ref() else { return };
        // Snapshot for the launch banner before the &mut self.gpu borrow
        // below (which rules out re-borrowing &self inside that block).
        let win_size = window.inner_size();
        let win_px = (win_size.width as f32, win_size.height as f32);
        let version_alpha = self.version_alpha();
        let cell_w_px = self.cell.w * scale;
        let cell_h_px = self.cell.h * scale;
        // Snapshot per-pane cell grids while we hold the workspace
        // lock so the render call below can run without re-locking
        // (matches the sugarloaf path's design).
        struct PaneSlot {
            rows: Vec<Vec<GridCell>>,
            origin_px: (f32, f32),
            dim: bool,
            font_scale: f32,
        }
        // Header chrome carried in LOGICAL px — gpu.rect/draw_text
        // promote to physical internally, matching the cell pass.
        #[allow(dead_code)]
        struct HeaderInfo {
            id: String,
            x: f32,
            y: f32,
            w: f32,
            /// Full pane box height (header + body) in logical px, used
            /// to draw the divider / active-focus ring around the pane.
            box_h: f32,
            label: String,
            is_active: bool,
            color: Option<[u8; 4]>,
            /// Markdown panes get Render/Raw toggle pills in the header.
            is_markdown: bool,
            /// Current markdown mode (true = Raw editor) for pill highlighting.
            md_raw_mode: bool,
            /// Image panes get zoom/rotate buttons instead of the terminal-action cluster.
            is_image: bool,
            /// In-pane tab labels (empty = single-tab; header shows `label`).
            tabs: Vec<String>,
            /// Active tab index into `tabs`.
            active_tab: usize,
        }
        // Captured once so the &mut self.gpu block below (which can't
        // re-borrow &self) can still see the collapsed/expanded width.
        let sidebar_w = self.effective_sidebar_w();
        let pad_px = (WINDOW_PADDING + sidebar_w) * scale;
        let title_px = TITLE_HEIGHT * scale;
        // Per-pane font multipliers (keyed by pty/leaf id), so each pane's
        // glyphs can be sized independently of the shared base cell.
        let pane_scales = self.pane_font_scales.clone();
        // Code-block copy buttons (text + logical rect), filled per pane in
        // the loop below and handed to both the mouse handler and overlay.
        let mut copy_btns: Vec<(String, (f32, f32, f32, f32))> = Vec::new();
        // Image panes collected here (id, pixels, body box in LOGICAL px) so
        // the gpu block below can upload + queue them after the cell pass.
        // (pid, image_data, body_box, zoom, rotation_quarters)
        let mut image_slots: Vec<(String, Arc<ImagePane>, (f32, f32, f32, f32), f32, u8)> = Vec::new();
        // Markdown panes: (id, doc, body box, scroll px, raw_mode, edit lines,
        // cursor). Render mode draws blocks; Raw mode draws the editor buffer.
        #[allow(clippy::type_complexity)]
        let mut md_slots: Vec<(
            String,
            Arc<MarkdownDoc>,
            (f32, f32, f32, f32),
            f32,
            bool,
            Option<Vec<String>>,
            (usize, usize),
        )> = Vec::new();
        // Per-pane body rect (header-excluded) in logical px, collected for
        // every pane so in-pane WebViews and other overlays can be snapped
        // to their pane after the borrow scope ends.
        let mut body_rects: Vec<(String, (f32, f32, f32, f32))> = Vec::new();
        let (slots, headers): (Vec<PaneSlot>, Vec<HeaderInfo>) = {
            let ws = self.ws.lock().unwrap();
            let active_id = ws.active_pane.clone();
            // Total grid rows/cols — used to detect the bottom-row / right-col
            // pane so it can stretch to the window's true edge (window_cells
            // floors both, leaving a sub-cell remainder otherwise).
            let (grid_cols, grid_rows) = self.window_cells();
            let leaves: Vec<(String, u16, u16, u16, u16)> = if let Some(layout) = ws.layout.as_ref() {
                layout
                    .leaves()
                    .into_iter()
                    .filter_map(|n| match n {
                        Layout::Pane { id, x, y, w, h } => {
                            Some((format!("%{id}"), *x, *y, *w, *h))
                        }
                        _ => None,
                    })
                    .collect()
            } else {
                // Single-pane fallback (no split tree). `ws.panes` holds EVERY
                // window's pane (a session shares one pane map across its
                // windows), so picking `.iter().next()` would draw an arbitrary
                // HashMap entry — switching windows would leave the body on the
                // same pane. Honor the active pane so a window switch repaints
                // the body; fall back to any pane only if active is unset/gone.
                let active = active_id
                    .as_ref()
                    .filter(|id| ws.panes.contains_key(*id))
                    .cloned()
                    .or_else(|| ws.panes.keys().next().cloned());
                match active {
                    Some(id) => vec![(id, 0, 0, 0, 0)],
                    None => Vec::new(),
                }
            };
            // Header bar when split OR when any pane carries multiple tabs.
            // A lone pane with a single tab stays header-less so the first
            // session reads as a plain terminal; but a lone pane with two or
            // more tabs (after a cross-pane drag, or a +button add) MUST
            // keep its strip so the tabs stay reachable.
            let any_multitab = leaves
                .iter()
                .any(|(id, _, _, _, _)| ws.panes.get(id).map_or(false, |p| p.tabs.len() > 1));
            let show_headers = leaves.len() > 1 || any_multitab;
            let header_shift_px = if show_headers {
                PANE_HEADER_HEIGHT * scale
            } else {
                0.0
            };
            let mut slots = Vec::new();
            let mut headers = Vec::new();
            for (id, x_cells, y_cells, w_cells, h_cells) in leaves {
                let Some(pane) = ws.panes.get(&id) else { continue };
                // pane.cells already holds the correct view: the PTY
                // backend snapshots through alacritty's display_offset,
                // so a scrolled-up frame arrives here pre-composed with
                // real scrollback (scroll-region TUIs included). Just
                // normalise each row to the current width so the GPU
                // pipeline emits exactly `cols` cells per row.
                // During a divider drag we DEFER the PTY reshape (SIGWINCH +
                // shell repaint is what causes the flicker), so the PTY's
                // reported cols/rows are stale. Clip the rendered cells to
                // the layout's CURRENT pane rect — overflow gets dropped at
                // the new edge instead of bleeding into the neighbouring
                // pane. After release, the final resize_backend lets the
                // shell catch up and the clip is a no-op.
                //
                // Single-pane fallback path (no layout tree yet) passes
                // (0,0,0,0) as a placeholder — that would clip everything
                // to nothing, so skip the layout clip entirely when w_cells
                // or h_cells is 0 and just trust the PTY dims.
                let pty_cols = pane.term().map_or(1, |t| t.cols).max(1) as usize;
                let pty_rows = pane.term().map_or(0, |t| t.cells.len());
                let (cols_now, rows_now) = if w_cells == 0 || h_cells == 0 {
                    (pty_cols, pty_rows)
                } else {
                    // Mirror resize_backend EXACTLY: pane box in base-grid px,
                    // minus real insets/header, divided by the ZOOMED cell.
                    // The clip has to land on the same count the PTY was sized
                    // to, or a zoomed-out pane (more cols/rows in the PTY) gets
                    // truncated back to the base-grid count and the TUI's
                    // layout tears.
                    let fs = pane_scales
                        .get(id.as_str())
                        .copied()
                        .unwrap_or(1.0)
                        .max(0.1);
                    let cw = self.cell.w.max(1.0);
                    let ch = self.cell.h.max(1.0);
                    let scaled_cw = cw * fs;
                    let scaled_ch = ch * fs;
                    let header_px_now = if show_headers { PANE_HEADER_HEIGHT } else { 0.0 };
                    let usable_w = (w_cells as f32 * cw - 2.0 * PANE_INNER_X).max(scaled_cw);
                    let usable_h =
                        (h_cells as f32 * ch - header_px_now - 2.0 * PANE_INNER_Y).max(scaled_ch);
                    let layout_cols = (usable_w / scaled_cw).floor() as usize;
                    let layout_rows = (usable_h / scaled_ch).floor() as usize;
                    (layout_cols.min(pty_cols).max(1), layout_rows.min(pty_rows))
                };
                let normalise = |row: &Vec<GridCell>| -> Vec<GridCell> {
                    let mut r = row.clone();
                    if r.len() < cols_now {
                        r.resize(cols_now, GridCell::blank());
                    } else if r.len() > cols_now {
                        r.truncate(cols_now);
                    }
                    r
                };
                // Image/markdown panes carry no PTY grid; an empty rows vec
                // makes draw_cells a no-op and the content (texture or laid-out
                // document) is painted into the pane box instead (queued below).
                let img = pane.image().cloned();
                let img_zoom = pane.image_view_zoom();
                let img_rot = pane.image_rot % 4;
                // Snapshot markdown render data: (doc, raw_mode, edit lines if
                // raw, cursor, scroll px).
                let md: Option<(Arc<MarkdownDoc>, bool, Option<Vec<String>>, (usize, usize), f32)> =
                    pane.markdown().map(|m| {
                        (
                            m.doc.clone(),
                            m.raw_mode,
                            if m.raw_mode {
                                Some(m.edit_lines.clone())
                            } else {
                                None
                            },
                            (m.cur_line, m.cur_col),
                            m.scroll as f32,
                        )
                    });
                let composed: Vec<Vec<GridCell>> = match pane.term() {
                    Some(t) => t.cells.iter().take(rows_now).map(normalise).collect(),
                    None => Vec::new(),
                };
                // Cells start below the header band when split, and are
                // inset inside the pane box so text never jams the divider
                // or window edge.
                let origin_px = (
                    pad_px + x_cells as f32 * cell_w_px + PANE_INNER_X * scale,
                    title_px
                        + y_cells as f32 * cell_h_px
                        + header_shift_px
                        + PANE_INNER_Y * scale,
                );
                // Code-block copy buttons: scan this pane's grid for bg
                // boxes (Claude Code code/command blocks) and stash a copy
                // button at each block's top-right. Logical px so the mouse
                // handler and the overlay pass agree on the hit area.
                let header_shift_logical = if show_headers {
                    PANE_HEADER_HEIGHT
                } else {
                    0.0
                };
                let body_left = WINDOW_PADDING
                    + sidebar_w
                    + x_cells as f32 * self.cell.w
                    + PANE_INNER_X;
                let body_top = TITLE_HEIGHT
                    + y_cells as f32 * self.cell.h
                    + header_shift_logical
                    + PANE_INNER_Y;
                // Code-block scan is O(cells × distinct-colours) and walks
                // the whole grid twice per frame. It only makes sense on the
                // normal screen — TUIs in alt-screen (claude code TUI mode,
                // vim, less, full-screen apps) get a copy chip per pseudo-
                // block which is never useful. Skipping there reclaims most
                // of render_frame_gpu's time at high update rates.
                let pane_alt = pane.term().map(|t| t.alt_screen).unwrap_or(false);
                if !pane_alt {
                    for block in detect_code_blocks(&composed) {
                        let text = extract_block(&composed, block);
                        if text.trim().is_empty() {
                            continue;
                        }
                        let (start, _end, _left, right) = block;
                        let block_top = body_top + start as f32 * self.cell.h;
                        let block_right = body_left + (right as f32 + 1.0) * self.cell.w;
                        let bx = (block_right - COPY_BTN_W - 4.0).max(body_left);
                        let by = block_top + 3.0;
                        copy_btns.push((text, (bx, by, COPY_BTN_W, COPY_BTN_H)));
                    }
                }
                let pane_font_scale = pane_scales.get(id.as_str()).copied().unwrap_or(1.0);
                slots.push(PaneSlot {
                    rows: composed,
                    origin_px,
                    // Unfocused panes dim their text only (no box veil). Single
                    // un-split pane is never dimmed.
                    dim: show_headers && active_id.as_deref() != Some(id.as_str()),
                    font_scale: pane_font_scale,
                });
                // Body box (header band excluded, inset by the same
                // PANE_INNER margins the cell grid uses) in logical px.
                // Bottom-row stretch mirrors the header's box_h so the
                // content fills to the window edge with no seam.
                // Computed for EVERY pane (not just image/md) — in-pane
                // WebViews need it too.
                let bx = WINDOW_PADDING
                    + sidebar_w
                    + x_cells as f32 * self.cell.w
                    + PANE_INNER_X;
                let by = TITLE_HEIGHT
                    + y_cells as f32 * self.cell.h
                    + header_shift_logical
                    + PANE_INNER_Y;
                let base_w = w_cells as f32 * self.cell.w;
                let full_w = if x_cells + w_cells >= grid_cols {
                    let extra = self.window.as_ref().map_or(0.0, |w| {
                        let s = w.scale_factor() as f32 * self.ui_zoom;
                        let raw_lw = w.inner_size().width as f32 / s;
                        (raw_lw
                            - (WINDOW_PADDING + sidebar_w + grid_cols as f32 * self.cell.w))
                            .max(0.0)
                    });
                    base_w + extra
                } else {
                    base_w
                };
                let bw = (full_w - 2.0 * PANE_INNER_X).max(1.0);
                let base_h = h_cells as f32 * self.cell.h;
                let full_h = if y_cells + h_cells >= grid_rows {
                    let extra = self.window.as_ref().map_or(0.0, |w| {
                        let s = w.scale_factor() as f32 * self.ui_zoom;
                        let raw_lh = w.inner_size().height as f32 / s;
                        (raw_lh - (TITLE_HEIGHT + grid_rows as f32 * self.cell.h)).max(0.0)
                    });
                    base_h + extra
                } else {
                    base_h
                };
                let bh = (full_h - header_shift_logical - 2.0 * PANE_INNER_Y).max(1.0);
                body_rects.push((id.clone(), (bx, by, bw, bh)));
                if let Some(image) = img {
                    image_slots.push((id.clone(), image, (bx, by, bw, bh), img_zoom, img_rot));
                }
                if let Some((doc, raw_mode, lines, cursor, scroll)) = md {
                    md_slots.push((
                        id.clone(),
                        doc,
                        (bx, by, bw, bh),
                        scroll,
                        raw_mode,
                        lines,
                        cursor,
                    ));
                }
                if show_headers {
                    // Custom title (rename / OSC) wins; otherwise show the
                    // live foreground process (vim, claude, zsh …); only
                    // fall back to the raw "%N" pane id if both are empty.
                    let smart = self.pty.get(&id).and_then(|p| Self::smart_pane_label(p));
                    let label = pane
                        .title
                        .clone()
                        .filter(|t| !t.is_empty())
                        .or(smart)
                        .unwrap_or_else(|| id.clone());
                    headers.push(HeaderInfo {
                        id: id.clone(),
                        x: WINDOW_PADDING + sidebar_w + x_cells as f32 * self.cell.w,
                        y: TITLE_HEIGHT + y_cells as f32 * self.cell.h,
                        // Right-col pane stretches to the window's true right
                        // edge, mirroring box_h's bottom stretch, so the
                        // floored sub-cell remainder doesn't read as a seam.
                        w: {
                            let base = w_cells as f32 * self.cell.w;
                            if x_cells + w_cells >= grid_cols {
                                let extra = self.window.as_ref().map_or(0.0, |w| {
                                    let s = w.scale_factor() as f32 * self.ui_zoom;
                                    let raw_lw = w.inner_size().width as f32 / s;
                                    (raw_lw
                                        - (WINDOW_PADDING
                                            + sidebar_w
                                            + grid_cols as f32 * self.cell.w))
                                        .max(0.0)
                                });
                                base + extra
                            } else {
                                base
                            }
                        },
                        // Bottom-row pane stretches to the window's true bottom.
                        // window_cells() floors rows, so the last sub-cell of
                        // window height falls outside the grid; without this the
                        // bottom border floats ~a cell above the edge and that
                        // gap reads as a seam between the pane and the window.
                        box_h: {
                            let base = h_cells as f32 * self.cell.h;
                            if y_cells + h_cells >= grid_rows {
                                let extra = self.window.as_ref().map_or(0.0, |w| {
                                    let s = w.scale_factor() as f32 * self.ui_zoom;
                                    let raw_lh = w.inner_size().height as f32 / s;
                                    (raw_lh
                                        - (TITLE_HEIGHT + grid_rows as f32 * self.cell.h))
                                        .max(0.0)
                                });
                                base + extra
                            } else {
                                base
                            }
                        },
                        label,
                        is_active: active_id.as_deref() == Some(id.as_str()),
                        color: pane.color,
                        is_markdown: pane.markdown().is_some(),
                        md_raw_mode: pane.markdown().map_or(false, |m| m.raw_mode),
                        is_image: pane.image().is_some(),
                        tabs: pane
                            .tabs
                            .iter()
                            .enumerate()
                            .map(|(i, t)| {
                                t.title
                                    .clone()
                                    .filter(|s| !s.is_empty())
                                    .or_else(|| {
                                        // 각 탭의 pid로 스마트 라벨(셸=cwd, 명령=프로세스).
                                        t.pid
                                            .as_deref()
                                            .and_then(|p| self.pty.get(p))
                                            .and_then(|s| Self::smart_pane_label(s))
                                    })
                                    .unwrap_or_else(|| {
                                        if i == 0 { id.clone() } else { format!("탭 {}", i + 1) }
                                    })
                            })
                            .collect(),
                        active_tab: pane.active_tab,
                    });
                }
            }
            // Fallback: if nothing is marked active (e.g. active_pane not yet
            // set right after a split), make the first header active so the
            // focused-tab box/accent always shows on exactly one pane.
            if !headers.is_empty() && !headers.iter().any(|h| h.is_active) {
                headers[0].is_active = true;
            }
            (slots, headers)
        };
        // Publish copy-button hit rects for the mouse handler; snapshot the
        // bare rects (+ hover state) for the overlay draw below. Both read
        // from the same numbers so a click lands on what the user sees.
        self.copy_btn_rects = copy_btns;
        let copy_btns_draw: Vec<(f32, f32, f32, f32, bool)> = self
            .copy_btn_rects
            .iter()
            .map(|(_, r)| {
                let hover = self.cursor_px.0 >= r.0
                    && self.cursor_px.0 <= r.0 + r.2
                    && self.cursor_px.1 >= r.1
                    && self.cursor_px.1 <= r.1 + r.3;
                (r.0, r.1, r.2, r.3, hover)
            })
            .collect();
        let toast_alpha = self.copy_toast_alpha();
        let slot_views: Vec<gpu::PaneSlot<'_>> = slots
            .iter()
            .map(|s| gpu::PaneSlot {
                rows: &s.rows,
                origin_px: s.origin_px,
                dim: s.dim,
                font_scale: s.font_scale,
            })
            .collect();
        // Recompute the inline suggestion against the freshly-applied
        // grid before snapshotting it into the overlay.
        self.update_suggestion();
        let overlay = self.gpu_overlay_snapshot();
        // Cache the × close-button hit rects (logical) for the mouse
        // handler, even before the GPU borrow below.
        let chrome_font = 14.0_f32;
        let close_size = chrome_font + 4.0;
        // × close sits inside the left tab, after [icon + title]. Approximate
        // the proportional label width (wide glyphs ~1em, ascii ~0.55em) so
        // the hit rect tracks the drawn glyph.
        self.pane_header_rects = headers
            .iter()
            .map(|h| {
                let label_w: f32 = h
                    .label
                    .chars()
                    .map(|c| {
                        if (c as u32) > 0x2000 {
                            chrome_font
                        } else {
                            chrome_font * 0.55
                        }
                    })
                    .sum();
                let close_x = h.x + 8.0 + (chrome_font + 6.0) + 6.0 + label_w + 8.0;
                let close = (
                    close_x,
                    h.y + (PANE_HEADER_HEIGHT - close_size) / 2.0,
                    close_size,
                    close_size,
                );
                (h.id.clone(), close)
            })
            .collect();
        // Markdown Render/Raw toggle now lives in the pane action buttons
        // (drawn in the header loop); the old right-aligned pills are gone.
        self.md_toggle_rects = Vec::new();
        // Session tabs live in a wry webview panel (like the git panel), not
        // the native title bar — drawing them here collided with the OSC title.
        // Drop-zone overlay: while a header drag is active, highlight the
        // half of the target pane the dragged pane would land in. Computed
        // here (immutable self borrow) so the gpu block below only touches
        // the cached rect.
        // Drop zone shows for BOTH header drags (whole pane → quadrant)
        // and tab drags whose cursor is over a pane BODY (split + place
        // moved tab as new pane). Tab drag over a strip is handled by
        // tab_drag_info's insertion bar instead.
        let header_drag_active = self
            .header_drag
            .as_ref()
            .map(|hd| hd.active)
            .unwrap_or(false);
        let tab_drag_active = self
            .tab_drag
            .as_ref()
            .map(|d| d.active)
            .unwrap_or(false);
        // The strip-only insertion bar gets replaced by the zone overlay
        // — without it the user sees no preview when hovering the header,
        // which is exactly the spot most people aim for when intending
        // "merge into this pane".
        let show_drop_zone = header_drag_active || tab_drag_active;
        // Indicator policy:
        //   - header band (cursor_on_header) → strip insertion bar only
        //                                       (overlay 안 그림)
        //   - body Center / split            → rectangle overlay
        // 두 인디케이터가 동시에 뜨지 않게 mutually exclusive.
        let current_zone = self.drop_target_at(self.cursor_px.0, self.cursor_px.1);
        let cursor_on_header = matches!(current_zone, Some((_, DropZone::Center))) && {
            // 헤더 = pane_top ~ pane_top + header_band. body_top
            // 10px 위까지 관대 (좁은 헤더에서 마우스 못 맞추는 거 방지).
            let cur_y = self.cursor_px.1;
            let leaves = self.pty_layout.as_ref().map(|t| t.leaves().len()).unwrap_or(1);
            let header_band = if leaves > 1 { PANE_HEADER_HEIGHT } else { 0.0 };
            current_zone
                .as_ref()
                .and_then(|(id, _)| {
                    let tree = self.pty_layout.as_ref()?;
                    let (cols, rows) = self.window_cells();
                    tree.leaf_rects(cols, rows)
                        .into_iter()
                        .find(|(i, ..)| i == id)
                        .map(|(_, _, cy, _, _)| TITLE_HEIGHT + cy as f32 * self.cell.h)
                })
                .map(|pane_top| cur_y < pane_top + header_band + 10.0)
                .unwrap_or(false)
        };
        // Overlay shows when cursor is over a pane BODY (split zone or
        // body-Center). Header-Center routes to the strip insertion bar.
        let zone_overlay_active = tab_drag_active && current_zone.is_some() && !cursor_on_header;
        let drop_zone_rect: Option<(f32, f32, f32, f32)> = show_drop_zone
            .then_some(current_zone)
            .flatten()
            .filter(|_| !cursor_on_header)
            .and_then(|(target, zone)| {
                let tree = self.pty_layout.as_ref()?;
                let leaves = tree.leaves().len();
                let (cols, rows) = self.window_cells();
                let pad = WINDOW_PADDING + self.effective_sidebar_w();
                let (_, cx, cy, cw, ch) = tree
                    .leaf_rects(cols, rows)
                    .into_iter()
                    .find(|(id, ..)| *id == target)?;
                let bx = pad + cx as f32 * self.cell.w;
                let pane_top = TITLE_HEIGHT + cy as f32 * self.cell.h;
                let bw = cw as f32 * self.cell.w;
                let bh = ch as f32 * self.cell.h;
                let header_band = if leaves > 1 { PANE_HEADER_HEIGHT } else { 0.0 };
                // Split overlay는 body 영역만 색칠 (헤더 띠 침범 X).
                let body_top = pane_top + header_band;
                let body_h = (bh - header_band).max(1.0);
                Some(match zone {
                    DropZone::Left => (bx, body_top, bw / 2.0, body_h),
                    DropZone::Right => (bx + bw / 2.0, body_top, bw / 2.0, body_h),
                    DropZone::Up => (bx, body_top, bw, body_h / 2.0),
                    DropZone::Down => (bx, body_top + body_h / 2.0, bw, body_h / 2.0),
                    DropZone::Center => return None,
                })
            });
        // Ghostty-style split seams: one 1px hairline per interior split
        // boundary instead of a 4-side border around every pane (which
        // doubled up into a thick seam between abutting panes). Coords match
        // divider_at_px so drag hit-testing lines up with the drawn line.
        let pane_seams: Vec<(f32, f32, f32, f32)> = self
            .pty_layout
            .as_ref()
            .map(|tree| {
                let (cols, rows) = self.window_cells();
                let pad = WINDOW_PADDING + self.effective_sidebar_w();
                // True window edges (logical). window_cells floors the grid,
                // so a seam spanning the last row/col must reach past the grid
                // to the real edge — otherwise it stops short like box_h did.
                let (win_right, win_bottom) = self.window.as_ref().map_or(
                    (
                        pad + cols as f32 * self.cell.w,
                        TITLE_HEIGHT + rows as f32 * self.cell.h,
                    ),
                    |w| {
                        let s = w.scale_factor() as f32 * self.ui_zoom;
                        (
                            w.inner_size().width as f32 / s,
                            w.inner_size().height as f32 / s,
                        )
                    },
                );
                tree.dividers(cols, rows)
                    .into_iter()
                    .map(|d| match d.dir {
                        pty_backend::SplitDir::Horizontal => {
                            let x = pad + d.edge as f32 * self.cell.w;
                            let y0 = TITLE_HEIGHT + d.span_start as f32 * self.cell.h;
                            let y1 = if d.span_start + d.span_len >= rows {
                                win_bottom
                            } else {
                                TITLE_HEIGHT
                                    + (d.span_start + d.span_len) as f32 * self.cell.h
                            };
                            (x, y0, 1.0, (y1 - y0).max(0.0))
                        }
                        pty_backend::SplitDir::Vertical => {
                            let y = TITLE_HEIGHT + d.edge as f32 * self.cell.h;
                            let x0 = pad + d.span_start as f32 * self.cell.w;
                            let x1 = if d.span_start + d.span_len >= cols {
                                win_right
                            } else {
                                pad + (d.span_start + d.span_len) as f32 * self.cell.w
                            };
                            (x0, y, (x1 - x0).max(0.0), 1.0)
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        // Left window-tab sidebar geometry. Cache the hit rects for the
        // mouse handler; the gpu block below paints from the same numbers so
        // a click always lands on what the user sees.
        let sb_win_h = win_px.1 / scale;
        self.refresh_window_labels();
        let sb_labels = self.window_labels.clone();
        let (sb_tabs, sb_closes, sb_plus) = self.sidebar_layout(sb_win_h);
        self.window_tab_rects = sb_tabs.clone();
        self.window_tab_close_rects = sb_closes.clone();
        self.new_window_btn_rect = Some(sb_plus);
        // Shell picker popup layout, computed here (no GPU borrow) so the
        // click hit-list and the painted boxes share one source of truth.
        // Items stack directly under the "+" button.
        let menu_open = self.shell_menu_open;
        let shell_items: Vec<(&'static str, &'static str, String)> =
            if menu_open { available_shells() } else { Vec::new() };
        const SHELL_ITEM_H: f32 = 34.0;
        let menu_w_for_paint = sb_plus.2.max(210.0);
        let shell_menu_layout: Vec<(String, &'static str, &'static str, (f32, f32, f32, f32))> = {
            let (px, py, _, ph) = sb_plus;
            let mut iy = py + ph + 4.0;
            shell_items
                .iter()
                .map(|(label, icon, cmd)| {
                    let r = (px, iy, menu_w_for_paint, SHELL_ITEM_H);
                    iy += SHELL_ITEM_H;
                    (cmd.clone(), *label, *icon, r)
                })
                .collect()
        };
        self.shell_menu_hits = shell_menu_layout
            .iter()
            .map(|(cmd, _, _, r)| (cmd.clone(), *r))
            .collect();
        let sb_active = self.active_window;
        // Which tab the cursor is over (for hover affordance + showing × only
        // where the user is pointing, Warp-style).
        let sb_cursor = self.cursor_px;
        let sb_hover = sb_tabs
            .iter()
            .find(|(_, r)| {
                sb_cursor.0 >= r.0
                    && sb_cursor.0 <= r.0 + r.2
                    && sb_cursor.1 >= r.1
                    && sb_cursor.1 <= r.1 + r.3
            })
            .map(|(i, _)| *i);
        let md_preedit = self.preedit.clone();
        // In-pane tab hit rects, collected during the header paint (needs the
        // measured tab widths) and published to self after the gpu borrow.
        let mut tab_hits: Vec<(String, usize, (f32, f32, f32, f32))> = Vec::new();
        let mut tab_close_hits: Vec<(String, usize, (f32, f32, f32, f32))> = Vec::new();
        let mut plus_hits: Vec<(String, (f32, f32, f32, f32))> = Vec::new();
        let mut image_btn_hits: Vec<(String, ImageBtn, (f32, f32, f32, f32))> = Vec::new();
        // Terminal-pane right-action cluster hit rects. Rebuilt every frame
        // so a stale rect can't outlive its glyph after a layout change.
        let mut pane_action_hits: Vec<(String, ActionKind, (f32, f32, f32, f32))> = Vec::new();
        if let Some(g) = self.gpu.as_mut() {
            g.clear_chrome();
            // Upload any image pane's pixels once, then queue each for this
            // frame. The image pass (in g.render) paints under the chrome so
            // pane headers / focus ring / dim overlay land on top.
            for (id, image, _, _, rot) in &image_slots {
                // Per-rotation cache key — rotated pixels uploaded once per
                // (pane, rotation) pair so toggling between rotations doesn't
                // re-rotate every frame.
                let key = format!("{id}-r{rot}");
                if !g.has_image(&key) {
                    let (rgba, w, h) = rotate_rgba_cw(&image.rgba, image.w, image.h, *rot);
                    g.upload_image(&key, &rgba, w, h);
                }
            }
            g.draw_cells(&slot_views);
            for (id, _, (bx, by, bw, bh), zoom, rot) in &image_slots {
                let key = format!("{id}-r{rot}");
                g.queue_image(&key, *bx, *by, *bw, *bh, *zoom);
            }
            // Markdown is laid out into chrome glyphs/rects here — after the
            // (empty) cell pass, before pane headers/borders so those land on
            // top. The returned content height feeds scroll clamping.
            for (id, doc, (bx, by, bw, bh), scroll, raw_mode, lines, cursor) in &md_slots {
                let content_h = if *raw_mode {
                    let lines = lines.as_deref().unwrap_or(&[]);
                    g.draw_raw_editor(lines, *cursor, *bx, *by, *bw, *bh, *scroll, &md_preedit)
                } else {
                    // Upload this doc's inline images once (keyed per block).
                    for im in &doc.images {
                        if !g.has_image(&im.key) {
                            g.upload_image(&im.key, &im.rgba, im.w, im.h);
                        }
                    }
                    g.draw_markdown(&doc.blocks, *bx, *by, *bw, *bh, *scroll)
                };
                self.md_content_h.insert(id.clone(), content_h);
            }
            // Title strip fill: the unified BG so the top bar reads as one
            // surface with the sidebar and terminal body (no depth seam).
            g.rect(0.0, 0.0, win_px.0 / scale, TITLE_HEIGHT, theme::BG);
            // Sidebar-toggle button, just right of the traffic lights.
            // VSCode / Warp-style glyph: an outlined panel with its left
            // column filled when the sidebar is shown, hollow when hidden.
            {
                let (bx, by, bw, bh) = Self::sidebar_toggle_rect();
                let hover = sb_cursor.0 >= bx
                    && sb_cursor.0 <= bx + bw
                    && sb_cursor.1 >= by
                    && sb_cursor.1 <= by + bh;
                if hover {
                    round_rect(g, bx, by, bw, bh, theme::RADIUS_SM, theme::SURFACE_HOVER);
                }
                let fg = if hover { theme::TEXT } else { theme::TEXT_DIM };
                // Icon box centered in the button.
                let iw = 16.0;
                let ih = 12.0;
                let ix = bx + (bw - iw) / 2.0;
                let iy = by + (bh - ih) / 2.0;
                let t = 1.5;
                // Rounded-ish outline via four hairline edges.
                g.rect(ix, iy, iw, t, fg);
                g.rect(ix, iy + ih - t, iw, t, fg);
                g.rect(ix, iy, t, ih, fg);
                g.rect(ix + iw - t, iy, t, ih, fg);
                // Left column divider + fill (the "sidebar"): solid when
                // shown so the button reads as a state indicator.
                let split_x = ix + iw * 0.36;
                g.rect(split_x, iy, t, ih, fg);
                if sidebar_w > 0.0 {
                    g.rect(ix + t, iy + t, split_x - ix - t, ih - 2.0 * t, theme::with_alpha(fg, 0x66));
                }
            }
            // Top bar: folder icon + current working directory, just right of
            // the sidebar toggle (Warp-style location chip).
            {
                let (tbx, _, tbw, _) = Self::sidebar_toggle_rect();
                let px0 = tbx + tbw + 12.0;
                let isz = theme::ICON_SIZE;
                let iy = (TITLE_HEIGHT - isz) / 2.0;
                let ty = (TITLE_HEIGHT - chrome_font) / 2.0;
                let after = g.draw_text(
                    px0,
                    iy,
                    "\u{f07b}",
                    gpu::DrawOpts {
                        font_size: isz,
                        color: theme::TEXT_DIM,
                        bold: false,
                        italic: false,
                    },
                );
                // Title-bar cwd chip follows the FOCUSED pane's shell cwd —
                // resolved via the shell's pid + /proc-style lookup. Falls
                // back to kasaterm's own cwd when the pane has no PTY (image
                // / markdown) or the pid couldn't be sniffed.
                let cwd_str = {
                    let active = self.ws.lock().ok().and_then(|w| w.active_pane.clone());
                    active
                        .and_then(|id| self.pty.get(&id).and_then(|p| p.shell_pid()))
                        .and_then(socket::pid_cwd)
                        .or_else(|| std::env::current_dir().ok())
                        .map(|p| Self::shorten_cwd(&p))
                        .unwrap_or_default()
                };
                g.draw_text(
                    after + 6.0,
                    ty,
                    &cwd_str,
                    gpu::DrawOpts {
                        font_size: chrome_font,
                        color: theme::TEXT,
                        bold: false,
                        italic: false,
                    },
                );
                // Active pane title (OSC 0/2 or shell process name) drawn
                // centered in the title strip — Terminal.app / iTerm UX
                // for single-pane mode. When the workspace is split, each
                // pane carries its own header, so the centered title is
                // redundant but still useful as "which pane has focus".
                let title_text: String = {
                    let ws = self.ws.lock().unwrap();
                    ws.active_pane
                        .as_deref()
                        .and_then(|id| ws.panes.get(id).map(|p| (id.to_string(), p.title.clone())))
                        .and_then(|(id, osc)| {
                            osc.filter(|s| !s.is_empty()).or_else(|| {
                                self.pty
                                    .get(&id)
                                    .and_then(|p| p.active_process_name())
                                    .filter(|s| !s.is_empty())
                            })
                        })
                        .unwrap_or_default()
                };
                if !title_text.is_empty() {
                    let tw = g.measure_chrome_text(&title_text, chrome_font, true);
                    let win_w_logical = win_px.0 / scale;
                    let center_x = (win_w_logical / 2.0) - tw / 2.0;
                    // Don't collide with the left chip cluster.
                    let left_edge = after + 6.0
                        + g.measure_chrome_text(&cwd_str, chrome_font, false)
                        + 24.0;
                    let tx = center_x.max(left_edge);
                    g.draw_text(
                        tx,
                        ty,
                        &title_text,
                        gpu::DrawOpts {
                            font_size: chrome_font,
                            color: theme::TEXT,
                            bold: true,
                            italic: false,
                        },
                    );
                }
            }
            // Window-tab sidebar, Warp-style. Painted first so per-pane
            // headers / rings layer on top at the seam.
            if sidebar_w > 0.0 {
                // Strip background: the unified BG, set apart from the cell
                // grid only by the right hairline below — not a darker fill.
                g.rect(
                    0.0,
                    TITLE_HEIGHT,
                    sidebar_w,
                    (sb_win_h - TITLE_HEIGHT).max(0.0),
                    theme::BG,
                );
                // Right hairline so the strip reads as a distinct column.
                g.rect(
                    sidebar_w - 1.0,
                    TITLE_HEIGHT,
                    1.0,
                    (sb_win_h - TITLE_HEIGHT).max(0.0),
                    theme::BORDER,
                );
                // Truncate a label to a *display-width* budget (CJK glyphs are
                // double-width) with a trailing ellipsis, so long Hangul/CJK
                // titles never bleed past the tab into the cell grid.
                let clip = |s: &str, budget: usize| -> String {
                    let total: usize = s.chars().map(cjk_display_w).sum();
                    if total <= budget {
                        return s.to_string();
                    }
                    let mut used = 0usize;
                    let mut out = String::new();
                    for c in s.chars() {
                        let w = cjk_display_w(c);
                        if used + w > budget.saturating_sub(1) {
                            break;
                        }
                        used += w;
                        out.push(c);
                    }
                    out.push('…');
                    out
                };
                let multi = sb_tabs.len() > 1;
                for (i, (tx, ty, tw, th)) in &sb_tabs {
                    let is_active = *i == sb_active;
                    let is_hover = sb_hover == Some(*i);
                    // Selected tab: subtle rounded highlight box (no left
                    // accent bar). Non-selected: flat, only a faint box on
                    // hover. Warp-style.
                    if is_active {
                        round_rect(g, *tx, *ty, *tw, *th, theme::RADIUS_MD, theme::SURFACE_ACTIVE);
                    } else if is_hover {
                        round_rect(g, *tx, *ty, *tw, *th, theme::RADIUS_MD, theme::SURFACE_HOVER);
                    }
                    // Icon chip: small rounded square with a glyph.
                    let (name, cwd) = sb_labels
                        .get(*i)
                        .cloned()
                        .unwrap_or_else(|| (format!("win {}", i + 1), String::new()));
                    let icon = 30.0_f32;
                    let icon_x = *tx + 9.0;
                    let icon_y = *ty + (*th - icon) / 2.0;
                    // Chip contrasts with its backdrop either way: lighter than
                    // the strip on a flat tab, a hair darker than the active box.
                    let chip_bg = if is_active {
                        theme::SURFACE_HOVER
                    } else {
                        theme::SURFACE_ACTIVE
                    };
                    round_rect(g, icon_x, icon_y, icon, icon, icon / 2.0, chip_bg);
                    let chip_glyph = tab_icon_glyph(&name);
                    let cg_w = g.measure_chrome_text(chip_glyph, theme::ICON_SIZE, true);
                    g.draw_text(
                        icon_x + (icon - cg_w) / 2.0,
                        icon_y + (icon - theme::ICON_SIZE) / 2.0,
                        chip_glyph,
                        gpu::DrawOpts {
                            font_size: theme::ICON_SIZE,
                            color: theme::TEXT_DIM,
                            bold: true,
                            italic: false,
                        },
                    );
                    // Two-line label to the right of the icon.
                    let text_x = icon_x + icon + 10.0;
                    let name_fg: [u8; 4] = if is_active {
                        theme::TEXT
                    } else {
                        theme::TEXT_DIM
                    };
                    let cwd_fg: [u8; 4] = theme::TEXT_MUTE;
                    let show_close = multi && (is_active || is_hover);
                    // Display-width budget derived from the live sidebar width:
                    // ~8.4 logical px per CJK glyph, minus icon (~50) and the
                    // close × slot (~26 when shown). Reflows on drag-resize.
                    let avail = (self.sidebar_w_logical - 60.0 - if show_close { 26.0 } else { 0.0 }).max(0.0);
                    let name_max = (avail / 8.4).floor().max(2.0) as usize;
                    g.draw_text(
                        text_x,
                        *ty + 11.0,
                        &clip(&name, name_max),
                        gpu::DrawOpts {
                            font_size: 13.5,
                            color: name_fg,
                            bold: is_active,
                            italic: false,
                        },
                    );
                    if !cwd.is_empty() {
                        g.draw_text(
                            text_x,
                            *ty + 30.0,
                            &clip(&cwd, ((self.sidebar_w_logical - 60.0).max(0.0) / 6.5).floor().max(4.0) as usize),
                            gpu::DrawOpts {
                                font_size: 11.0,
                                color: cwd_fg,
                                bold: false,
                                italic: false,
                            },
                        );
                    }
                    // × close — only on the active or hovered tab (where the
                    // cursor is), so the strip stays clean otherwise. Hit
                    // rects exist for every tab; you hover before you click.
                    if show_close {
                        if let Some((_, (cx, cy, cw, ch))) =
                            sb_closes.iter().find(|(ci, _)| ci == i)
                        {
                            let xg = "\u{2715}";
                            let xw = g.measure_chrome_text(xg, theme::ICON_SIZE, false);
                            g.draw_text(
                                *cx + (*cw - xw) / 2.0,
                                *cy + (*ch - theme::ICON_SIZE) / 2.0,
                                xg,
                                gpu::DrawOpts {
                                    font_size: theme::ICON_SIZE,
                                    color: theme::TEXT_MUTE,
                                    bold: false,
                                    italic: false,
                                },
                            );
                        }
                    }
                }
                // "+" new-window button under the last tab: flat, faint box on
                // hover, centred glyph.
                let (px, py, pw, ph) = sb_plus;
                let plus_hover = sb_cursor.0 >= px
                    && sb_cursor.0 <= px + pw
                    && sb_cursor.1 >= py
                    && sb_cursor.1 <= py + ph;
                if plus_hover {
                    round_rect(g, px, py, pw, ph, theme::RADIUS_MD, theme::SURFACE_HOVER);
                }
                let plus_g = "+";
                let plus_gw = g.measure_chrome_text(plus_g, theme::ICON_SIZE, false);
                g.draw_text(
                    px + (pw - plus_gw) / 2.0,
                    py + (ph - theme::ICON_SIZE) / 2.0,
                    plus_g,
                    gpu::DrawOpts {
                        font_size: theme::ICON_SIZE,
                        color: theme::TEXT_MUTE,
                        bold: false,
                        italic: false,
                    },
                );
                // Shell picker popup, stacked under the "+" button. Layout
                // (shell_menu_layout) and hit rects were computed before the
                // GPU borrow so clicks land on the same boxes we paint.
                if menu_open && !shell_menu_layout.is_empty() {
                    let backdrop_h = shell_menu_layout.len() as f32 * SHELL_ITEM_H + 8.0;
                    round_rect(
                        g,
                        px - 4.0,
                        py + ph,
                        menu_w_for_paint + 8.0,
                        backdrop_h,
                        theme::RADIUS_MD,
                        theme::SURFACE_ACTIVE,
                    );
                    for (_, label, icon, (ix, iy, iw, ih)) in &shell_menu_layout {
                        let hov = sb_cursor.0 >= *ix
                            && sb_cursor.0 <= *ix + *iw
                            && sb_cursor.1 >= *iy
                            && sb_cursor.1 <= *iy + *ih;
                        if hov {
                            round_rect(g, *ix, *iy, *iw, *ih, theme::RADIUS_MD, theme::SURFACE_HOVER);
                        }
                        g.draw_text(
                            *ix + 12.0,
                            *iy + (*ih - theme::ICON_SIZE) / 2.0,
                            icon,
                            gpu::DrawOpts { font_size: theme::ICON_SIZE, color: theme::TEXT_DIM, bold: false, italic: false },
                        );
                        g.draw_text(
                            *ix + 38.0,
                            *iy + (*ih - 14.0) / 2.0,
                            label,
                            gpu::DrawOpts { font_size: 14.0, color: theme::TEXT, bold: false, italic: false },
                        );
                    }
                }
            }
            // Per-pane header bar. The band is the unified BG (same as the
            // body) so there's no depth seam; a bottom hairline separates it
            // from the cell grid. The active tab is marked by a raised pill +
            // a top accent strip — not a darker "cage" — and only the active
            // tab carries a × (so clicking any inactive tab just switches).
            // (drop_pane, target) — drives the insertion bar; updated to
            // the pane the cursor is currently over (cross-pane drag).
            // Suppressed whenever the zone-overlay rectangle is showing
            // for the same drag — two simultaneous indicators is what
            // the "pane 이동이랑 같이 떠" report was about. Falls back
            // to the bar only when the cursor is outside every pane box
            // (gap / window edge).
            let tab_drag_info: Option<(String, usize)> = self
                .tab_drag
                .as_ref()
                .filter(|d| d.active && !zone_overlay_active)
                .map(|d| (d.drop_pane.clone(), d.target));
            // (source_pane, source_idx) — the tab being lifted. The source
            // tab is drawn at reduced alpha so it reads as "in transit"
            // while the user drags it into another strip.
            let tab_drag_src: Option<(String, usize)> = self
                .tab_drag
                .as_ref()
                .filter(|d| d.active)
                .map(|d| (d.pane.clone(), d.from));
            let hover_info: Option<(String, usize)> = self.pane_tab_hover.clone();
            // Active-tab top accents we need to repaint after the pane
            // dividers (BORDER) draw, so a horizontal split's seam doesn't
            // wipe the accent of the lower pane's active tab.
            let mut deferred_accents: Vec<(f32, f32, f32, [u8; 4])> = Vec::new();
            for h in &headers {
                g.rect(h.x, h.y, h.w, PANE_HEADER_HEIGHT, theme::BG);
                // No bottom hairline: the band == body, and the active tab
                // flows straight into the cell grid (browser-tab feel).
                // Compact glyphs — a touch bigger than the label so icons
                // read, but no longer the bulky +10 of the old design.
                let icon_size = theme::ICON_SIZE;
                let text_y = h.y + (PANE_HEADER_HEIGHT - chrome_font) / 2.0;
                let icon_y = h.y + (PANE_HEADER_HEIGHT - icon_size) / 2.0;
                let act_fg: [u8; 4] = if h.is_active {
                    theme::TEXT_DIM
                } else {
                    theme::with_alpha(theme::TEXT_DIM, 0x6B)
                };
                // Right action button cluster. Terminal panes get
                // split-v / split-h (new-terminal and web were dropped —
                // the +button already opens a new shell, and the web
                // overlay added complexity for little payoff). Image panes
                // keep the 4-button zoom/rotate set.
                let abw = icon_size + 2.0;
                let agap = 2.0;
                let n_btn: f32 = if h.is_image { 4.0 } else { 2.0 };
                let btn_cluster = abw * n_btn + agap * (n_btn - 1.0) + 12.0;
                // ── In-pane tab bar ── empty tabs = single tab from `label`.
                let tab_list: Vec<&str> = if h.tabs.is_empty() {
                    vec![h.label.as_str()]
                } else {
                    h.tabs.iter().map(|s| s.as_str()).collect()
                };
                let _icon_w = g.measure_chrome_text("\u{f489}", icon_size, false);
                let close_w = g.measure_chrome_text("\u{2715}", icon_size, false);
                let plus_w = g.measure_chrome_text("\u{ea60}", icon_size, false);
                // Each tab's title gets an equal share of the leftover width.
                let tabs_area = (h.w - 8.0 - btn_cluster - plus_w - 16.0).max(0.0);
                let per_tab = if tab_list.len() == 1 {
                    tabs_area
                } else {
                    (tabs_area / tab_list.len() as f32).clamp(56.0, 320.0)
                };
                // Left edge of each tab's pill, for the drag insertion bar.
                let mut tab_edges: Vec<f32> = Vec::with_capacity(tab_list.len());
                // Geometry for the post-loop structural border pass.
                let mut tabs_left: Option<f32> = None;
                let mut tabs_right_edge: f32 = 0.0;
                let mut inter_boundaries: Vec<f32> = Vec::new();
                let mut active_tab_box: Option<(f32, f32)> = None;
                let gap = 6.0_f32;
                let mut tx = h.x + 8.0;
                for (i, tab) in tab_list.iter().enumerate() {
                    let tab_x0 = tx;
                    // This pane's active tab — gets the pill + focus strip + ×.
                    let active = tab_list.len() == 1 || i == h.active_tab;
                    let is_hover = hover_info
                        .as_ref()
                        .map(|(p, hi)| p == &h.id && *hi == i)
                        .unwrap_or(false);
                    // × on the active tab always; on inactive only while
                    // hovered. The width is reserved either way so hover
                    // doesn't shift the surrounding layout.
                    let show_x = active || is_hover;
                    let reserve_x = true;
                    let bright = active || is_hover;
                    // Tab being lifted in a cross-pane / reorder drag is
                    // drawn faint — reads as "in transit" against the
                    // insertion bar at the drop position.
                    let being_dragged = tab_drag_src
                        .as_ref()
                        .map(|(p, idx)| p == &h.id && *idx == i)
                        .unwrap_or(false);
                    let alpha_mul = if being_dragged { 0x55 } else { 0xFF };
                    let combine = |a: u8| ((a as u16 * alpha_mul as u16) / 0xFF) as u8;
                    let t_fg = if bright {
                        theme::with_alpha(theme::TEXT, combine(0xFF))
                    } else {
                        theme::with_alpha(theme::TEXT, combine(0x82))
                    };
                    let t_icon = if bright {
                        theme::with_alpha(theme::TEXT_DIM, combine(0xFF))
                    } else {
                        theme::with_alpha(theme::TEXT_DIM, combine(0x82))
                    };
                    // Truncate this tab's title to its share of the bar.
                    // × space is reserved on every tab — see `reserve_x`.
                    // No per-tab terminal glyph: the +button already signals
                    // "new shell"; doubling that icon on every tab was noise.
                    let x_reserve = if reserve_x { close_w + 8.0 } else { 0.0 };
                    let budget = (per_tab - x_reserve - 14.0).max(0.0);
                    let mut label = tab.to_string();
                    let mut lw = g.measure_chrome_text(&label, chrome_font, active);
                    if lw > budget {
                        while label.chars().count() > 1 {
                            label.pop();
                            lw = g.measure_chrome_text(&format!("{label}…"), chrome_font, active);
                            if lw <= budget {
                                break;
                            }
                        }
                        label.push('…');
                    }
                    // Pill geometry: label + reserved × slot (terminal icon
                    // removed — +button covers "new shell" duty).
                    let content_w = lw + x_reserve;
                    // First tab sits flush with the pane's left edge so the
                    // active tab's accent strip joins the pane divider with
                    // no visible gap.
                    let box_x = if i == 0 { h.x } else { tab_x0 - 6.0 };
                    let box_right = tab_x0 + content_w + 6.0;
                    let tw = (box_right - box_x).max(0.0);
                    tab_edges.push(box_x);
                    if tabs_left.is_none() {
                        tabs_left = Some(box_x);
                    } else {
                        inter_boundaries.push(box_x);
                    }
                    tabs_right_edge = box_x + tw;
                    if active {
                        active_tab_box = Some((box_x, tw));
                    }
                    // Active tab keeps the band BG (= terminal body) — no
                    // fill — so the tab reads as continuous with the content
                    // below it. The accent top + broken bottom are what
                    // differentiate it. Structural lines drawn post-loop.
                    let stroke = 1.0_f32;
                    let _ = stroke;
                    let _ = t_icon;
                    let cx = g.draw_text(
                        tx,
                        text_y,
                        &label,
                        gpu::DrawOpts { font_size: chrome_font, color: t_fg, bold: active, italic: false },
                    );
                    if show_x {
                        let close_x = cx + 8.0;
                        let cxe = g.draw_text(
                            close_x,
                            icon_y,
                            "\u{2715}",
                            gpu::DrawOpts { font_size: icon_size, color: t_icon, bold: false, italic: false },
                        );
                        // × close hit (widen a little for an easy target).
                        let cw = (cxe - close_x).max(icon_size * 0.6);
                        tab_close_hits.push((h.id.clone(), i, (close_x - 2.0, h.y, cw + 4.0, PANE_HEADER_HEIGHT)));
                    }
                    // Whole-pill click/drag hit. Inactive tabs have no × inside,
                    // so the entire pill switches; the active tab's × is checked
                    // first by the handler.
                    tab_hits.push((h.id.clone(), i, (box_x, h.y, tw, PANE_HEADER_HEIGHT)));
                    tx = box_right + gap;
                }
                // Structural borders. Browser-tab pattern:
                //   - Top BORDER across the strip, with the active tab's
                //     segment painted in the focus color (same thickness).
                //   - Bottom BORDER across the strip but BROKEN under the
                //     active tab so the active opens straight into the body.
                //   - Vertical BORDER at each inter-tab boundary (single line
                //     shared between neighbours).
                // No outer left/right of the strip — the pane dividers fill
                // those roles, so leftmost-active never gets two stacked lines.
                if let Some(left) = tabs_left {
                    let stroke = 1.0_f32;
                    let band_w = (tabs_right_edge - left).max(0.0);
                    g.rect(left, h.y, band_w, stroke, theme::BORDER);
                    // Bottom BORDER across the WHOLE pane header (tabs + plus
                    // button + action cluster), broken only under the active
                    // tab so it flows into the body.
                    let by = h.y + PANE_HEADER_HEIGHT - stroke;
                    let h_right = h.x + h.w;
                    if let Some((ax, aw)) = active_tab_box {
                        let lw = (ax - h.x).max(0.0);
                        g.rect(h.x, by, lw, stroke, theme::BORDER);
                        let rx = ax + aw;
                        let rw = (h_right - rx).max(0.0);
                        g.rect(rx, by, rw, stroke, theme::BORDER);
                    } else {
                        g.rect(h.x, by, h.w, stroke, theme::BORDER);
                    }
                    for b in &inter_boundaries {
                        g.rect(*b, h.y, stroke, PANE_HEADER_HEIGHT, theme::BORDER);
                    }
                    // Right edge of the strip — gives the last tab (often the
                    // active one when only the trailing tab is selected) a
                    // visible right boundary. Left edge is left to the pane
                    // divider so it never doubles up.
                    g.rect(tabs_right_edge - stroke, h.y, stroke, PANE_HEADER_HEIGHT, theme::BORDER);
                    if let Some((ax, aw)) = active_tab_box {
                        let accent_col = if h.is_active { theme::ACCENT } else { theme::TEXT };
                        // accent 선은 BORDER stroke(1px)보다 살짝 굵게 — 활성 pane 강조.
                        g.rect(ax, h.y, aw, ACTIVE_ACCENT_STROKE, accent_col);
                        deferred_accents.push((ax, h.y, aw, accent_col));
                    }
                }
                // Drag insertion bar: 6px accent line spanning the strip.
                // 옛 2px는 Retina+at-speed drag에서 사실상 안 보였음.
                if let Some((ref dpane, target)) = tab_drag_info {
                    if *dpane == h.id {
                        let bar_x = tab_edges.get(target).copied().unwrap_or(tx - gap);
                        g.rect(bar_x - 3.0, h.y + 1.0, 6.0, PANE_HEADER_HEIGHT - 2.0, theme::ACCENT);
                    }
                }
                let (cur_x, cur_y) = self.cursor_px;
                let inside =
                    |rx: f32, ry: f32, rw: f32, rh: f32| cur_x >= rx && cur_x <= rx + rw && cur_y >= ry && cur_y <= ry + rh;
                // [+] new-tab button right after the tabs. Hover chip is a
                // tight rounded square centered on the glyph so the glow
                // hugs the icon instead of stretching across a tall band.
                // Hidden while a tab drag is active so the +button doesn't
                // sit on top of the insertion bar / accept a stray drop.
                let dragging_tab = tab_drag_src.is_some();
                let plus_iw = g.measure_chrome_text("\u{ea60}", icon_size, false);
                let chip_size = (icon_size + 6.0).max(plus_iw + 6.0);
                let chip_x = tx + (plus_iw - chip_size) / 2.0;
                let chip_y = h.y + (PANE_HEADER_HEIGHT - chip_size) / 2.0;
                let plus_rect = (chip_x, chip_y, chip_size, chip_size);
                let plus_hover = !dragging_tab && inside(plus_rect.0, plus_rect.1, plus_rect.2, plus_rect.3);
                if plus_hover {
                    round_rect(g, plus_rect.0, plus_rect.1, plus_rect.2, plus_rect.3,
                        theme::RADIUS_SM, theme::with_alpha(theme::TEXT, 0x22));
                }
                let plus_color = if plus_hover { theme::TEXT } else { act_fg };
                if !dragging_tab {
                    g.draw_text(
                        tx,
                        icon_y,
                        "\u{ea60}",
                        gpu::DrawOpts { font_size: icon_size, color: plus_color, bold: false, italic: false },
                    );
                    plus_hits.push((h.id.clone(), plus_rect));
                }
                // ── Right action buttons ── per-kind cluster: terminal panes
                // get new-terminal/web/split-v/split-h; image panes get
                // zoom-out / zoom-in / rotate / reset wired to the in-pane
                // image-view state mutated by forward_key as well.
                // Per cluster we carry either an ImageBtn (image pane) or an
                // ActionKind (terminal pane). Keeping both as Option in one
                // tuple keeps the paint loop unified.
                let action_set: Vec<(&str, Option<ImageBtn>, Option<ActionKind>)> = if h.is_image {
                    vec![
                        ("−", Some(ImageBtn::ZoomOut), None),
                        ("+", Some(ImageBtn::ZoomIn), None),
                        ("↻", Some(ImageBtn::Rotate), None),
                        ("0", Some(ImageBtn::Reset), None),
                    ]
                } else {
                    vec![
                        ("\u{eb57}", None, Some(ActionKind::SplitV)),
                        ("\u{eb56}", None, Some(ActionKind::SplitH)),
                    ]
                };
                let mut bx = h.x + h.w - 8.0 - (abw * n_btn + agap * (n_btn - 1.0));
                for (ic, kind, action) in action_set {
                    let iw = g.measure_chrome_text(ic, icon_size, false);
                    let chip_size = icon_size + 6.0;
                    let chip_y = h.y + (PANE_HEADER_HEIGHT - chip_size) / 2.0;
                    let chip_x = bx + (abw - chip_size) / 2.0;
                    let hover = inside(chip_x, chip_y, chip_size, chip_size);
                    if hover {
                        round_rect(g, chip_x, chip_y, chip_size, chip_size,
                            theme::RADIUS_SM, theme::with_alpha(theme::TEXT, 0x22));
                    }
                    let color = if hover { theme::TEXT } else { act_fg };
                    g.draw_text(
                        bx + (abw - iw) / 2.0,
                        icon_y,
                        ic,
                        gpu::DrawOpts { font_size: icon_size, color, bold: false, italic: false },
                    );
                    if let Some(k) = kind {
                        image_btn_hits.push((h.id.clone(), k, (chip_x, chip_y, chip_size, chip_size)));
                    }
                    if let Some(a) = action {
                        pane_action_hits.push((h.id.clone(), a, (chip_x, chip_y, chip_size, chip_size)));
                    }
                    bx += abw + agap;
                }
            }
            // Focus by contrast: unfocused panes fade their text only (via
            // PaneSlot.dim in draw_cells), not the whole box — no dark veil.
            // Ghostty-style: one hairline per interior split boundary, drawn
            // after the veil so the seam stays crisp on top. No per-pane box
            // border (that doubled into a thick seam between abutting panes
            // and read as caged tiles).
            for (sx, sy, sw, sh) in &pane_seams {
                g.rect(*sx, *sy, *sw, *sh, theme::BORDER);
            }
            // Re-paint the active-tab accent strips so a horizontal pane
            // divider just above a pane doesn't wipe its accent color.
            for (ax, ay, aw, ac) in &deferred_accents {
                g.rect(*ax, *ay, *aw, ACTIVE_ACCENT_STROKE, *ac);
            }
            Self::paint_gpu_overlays(g, &overlay);
            // Code-block copy buttons, painted on top of the inactive-pane
            // veil so they stay legible everywhere. The icon is two
            // overlapping squares drawn from rects (font glyphs map
            // unreliably in this renderer — see CLAUDE.md box-drawing note).
            for (bx, by, bw, bh, hover) in &copy_btns_draw {
                let (bx, by, bw, bh) = (*bx, *by, *bw, *bh);
                let bg = if *hover {
                    theme::SURFACE_HOVER
                } else {
                    theme::with_alpha(theme::SURFACE_ACTIVE, 0xE0)
                };
                round_rect(g, bx, by, bw, bh, theme::RADIUS_SM, bg);
                let fg = if *hover { theme::TEXT } else { theme::TEXT_DIM };
                let s = 8.0; // square side
                let off = 2.5; // overlap offset
                let t = 1.3; // stroke
                let gx = bx + (bw - (s + off)) / 2.0;
                let gy = by + (bh - (s + off)) / 2.0;
                // Back square (up-right), outline only.
                let (r1x, r1y) = (gx + off, gy);
                g.rect(r1x, r1y, s, t, fg);
                g.rect(r1x, r1y + s - t, s, t, fg);
                g.rect(r1x, r1y, t, s, fg);
                g.rect(r1x + s - t, r1y, t, s, fg);
                // Front square (down-left): refill the chip bg first so it
                // reads as sitting on top, then outline.
                let (r2x, r2y) = (gx, gy + off);
                g.rect(r2x, r2y, s, s, bg);
                g.rect(r2x, r2y, s, t, fg);
                g.rect(r2x, r2y + s - t, s, t, fg);
                g.rect(r2x, r2y, t, s, fg);
                g.rect(r2x + s - t, r2y, t, s, fg);
            }
            // "복사됨" toast, bottom-center, brief fade after a block copy.
            if toast_alpha > 0.0 {
                let msg = "복사됨";
                let t_font = 13.0_f32;
                let win_w = win_px.0 / scale;
                let win_h = win_px.1 / scale;
                // CJK glyphs ~1em wide; pad generously so the pill never clips.
                let text_w = msg.chars().count() as f32 * t_font;
                let (px, py) = (14.0_f32, 8.0_f32);
                let box_w = text_w + px * 2.0;
                let box_h = t_font + py * 2.0;
                let bx = (win_w - box_w) / 2.0;
                let by = win_h - box_h - 24.0;
                let a = (235.0 * toast_alpha).round() as u8;
                round_rect(
                    g,
                    bx,
                    by,
                    box_w,
                    box_h,
                    theme::RADIUS_MD,
                    theme::with_alpha(theme::SURFACE_ACTIVE, a),
                );
                let ta = (255.0 * toast_alpha).round() as u8;
                g.draw_text(
                    bx + px,
                    by + py,
                    msg,
                    gpu::DrawOpts {
                        font_size: t_font,
                        color: theme::with_alpha(theme::SUCCESS, ta),
                        bold: true,
                        italic: false,
                    },
                );
            }
            // Drop-zone highlight sits on top of everything during a drag.
            if let Some((zx, zy, zw, zh)) = drop_zone_rect {
                g.rect(zx, zy, zw, zh, theme::with_alpha(theme::ACCENT, 90));
            }
            // Launch build banner, bottom-right, painted last so it sits
            // on top. Faint and short-lived — fades out after a few
            // seconds. Coords are logical px (gpu promotes to physical).
            let v_alpha = version_alpha;
            if v_alpha > 0.0 {
                let label = Self::version_label();
                let v_font = 11.0_f32;
                let win_w = win_px.0 / scale;
                let win_h = win_px.1 / scale;
                // Proportional glyphs, so estimate the run width to right-
                // align: ~0.5em per char is a safe over-estimate for this
                // mono-ish label, padded so it never clips the edge.
                let text_w = label.chars().count() as f32 * v_font * 0.52;
                let margin = 8.0;
                let x = (win_w - text_w - margin).max(margin);
                let y = win_h - v_font - margin;
                let a = (170.0 * v_alpha).round() as u8;
                g.draw_text(
                    x,
                    y,
                    &label,
                    gpu::DrawOpts {
                        font_size: v_font,
                        color: theme::with_alpha(theme::TEXT_DIM, a),
                        bold: false,
                        italic: false,
                    },
                );
            }
            if let Err(e) = g.render(&slot_views, scale) {
                eprintln!("[gpu] render error: {e:?}");
            }
        }
        self.pane_tab_rects = tab_hits;
        self.pane_tab_close_rects = tab_close_hits;
        self.pane_plus_rects = plus_hits;
        self.image_btn_rects = image_btn_hits;
        self.pane_action_hits = pane_action_hits;
        // body_rects collected per pane in case future overlays need them.
        let _ = body_rects;
        // Damage flags get cleared here (parity with sugarloaf path
        // below) so successive frames short-circuit on idle.
        if let Ok(mut ws) = self.ws.lock() {
            for pane in ws.panes.values_mut() {
                pane.dirty = false;
            }
        }
        self.chrome_dirty = false;
    }

    fn render_frame(&mut self) {
        // commit_overlay's job ends the moment the echo lands and moves
        // the cursor. Retire it permanently then — otherwise erasing
        // back to the commit position re-satisfies `cursor == stored`
        // and the stale "안" reappears.
        if let Some(before) = self.commit_overlay.as_ref().map(|(_, b)| *b) {
            let cur = self.ws.lock().ok().and_then(|ws| {
                ws.active_pane.clone().and_then(|id| {
                    ws.panes
                        .get(&id)
                        .and_then(|p| p.term())
                        .map(|t| (t.cursor_row, t.cursor_col))
                })
            });
            if cur != Some(before) {
                self.commit_overlay = None;
            }
        }
        let t0 = Instant::now();
        let trace = std::env::var_os("KASATERM_PROFILE").is_some();
        let now = Instant::now();
        let blink_on = self.cursor_blink_on(now);
        // Damage gate: skip the GPU pass when nothing changed since
        // the last frame. winit keeps showing the previous swapchain
        // image, so the user sees the same picture without us
        // emitting 10k+ sugarloaf calls. PTY updates flag the per-
        // pane dirty bit; chrome events flag `self.chrome_dirty`;
        // cursor blink phase toggles count separately.
        let blink_changed = blink_on != self.last_blink_on;
        let pty_dirty = self.ws.lock().unwrap().panes.values().any(|p| p.dirty);
        // The launch banner fade is its own animation source: while it's
        // still visible the picture changes every frame, so force the GPU
        // pass even when panes are clean (about_to_wait re-arms WaitUntil
        // to keep waking us through the fade).
        let version_animating = self.version_alpha() > 0.0;
        // Same for the copy toast: its fade changes the picture every frame.
        let toast_animating = self.copy_toast_alpha() > 0.0;
        if !pty_dirty
            && !self.chrome_dirty
            && !blink_changed
            && !version_animating
            && !toast_animating
        {
            return;
        }
        self.last_blink_on = blink_on;
        if self.window.is_none() { return; }
        let scale = self.effective_scale();
        // gpu path takes over the whole frame — no chrome yet, just
        // the cell grid through the cell-renderer pipeline.
        if self.gpu.is_some() {
            self.render_frame_gpu(scale);
            if trace {
                eprintln!(
                    "[render-gpu] {}us since_input={}ms",
                    t0.elapsed().as_micros(),
                    now.saturating_duration_since(self.last_input_at).as_millis()
                );
            }
            return;
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    /// A background thread (PTY snapshot, socket) asked us to repaint.
    /// Delivered even while a WaitUntil is parked, so this is what makes
    /// committed-Hangul echo / backspace / space show up without lag.
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: UserEvent) {
        // Render directly here instead of request_redraw → (next loop)
        // RedrawRequested. The PTY echo already paid a thread hop +
        // channel to reach us; bouncing through request_redraw adds
        // another winit cycle of latency. Painting inline gets the echo
        // on screen this turn.
        self.render_frame();
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // Persist every session's layout + pane cwds + claude sessions so the
        // next launch restores the full workspace (A3).
        self.save_session_state();
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        // macOS menu bar: app submenu (About/Quit) + a "보기" submenu with
        // the "Git 패널" toggle. Built once (NSApp exists by resumed). Clicks
        // arrive on muda's global channel, drained in about_to_wait. Stored
        // on self so the menu outlives this function.
        #[cfg(target_os = "macos")]
        if self.menu.is_none() {
            use muda::{Menu, MenuItem, PredefinedMenuItem, Submenu};
            let menu = Menu::new();
            let app_m = Submenu::new("kasaterm", true);
            let _ = app_m.append_items(&[
                &PredefinedMenuItem::about(None, None),
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::quit(None),
            ]);
            let view_m = Submenu::new("보기", true);
            let git_item = MenuItem::new("Git 패널 켜기/끄기", true, None);
            let session_item = MenuItem::new("세션 패널 켜기/끄기", true, None);
            let _ = view_m.append(&git_item);
            let _ = view_m.append(&session_item);
            let _ = menu.append(&app_m);
            let _ = menu.append(&view_m);
            menu.init_for_nsapp();
            self.git_menu_item = Some(git_item);
            self.session_menu_item = Some(session_item);
            self.menu = Some(menu);
        }
        // WaitUntil so the cursor blink ticks even when no terminal output
        // is arriving — the redraw inside RedrawRequested re-arms the
        // schedule. Pure Wait would freeze the blink mid-phase, Poll would
        // burn CPU on idle.
        event_loop.set_control_flow(ControlFlow::WaitUntil(
            Instant::now() + std::time::Duration::from_millis(BLINK_HALF_PERIOD_MS),
        ));
        let attrs = WindowAttributes::default()
            .with_title("kasaterm")
            // Force dark appearance so the system titlebar paints its
            // text in light gray. Default is "follow OS", which would
            // give black text on our dark content view and make the
            // process-name label nearly invisible in light mode.
            .with_theme(Some(Theme::Dark))
            .with_inner_size(LogicalSize::new(1100.0, 860.0));
        // Custom chrome: traffic-light row sits inside the content view
        // so we can paint tabs and drag handles right next to the
        // native buttons. OS still owns the traffic lights themselves
        // and the resize edges — we only paint and route drag from the
        // strip above the cell grid.
        #[cfg(target_os = "macos")]
        let attrs = attrs
            .with_titlebar_transparent(true)
            // Hide the OS-drawn window title (the centered OSC/process
            // label) — the title strip stays clean, just traffic lights +
            // our sidebar-toggle button.
            .with_title_hidden(true)
            .with_fullsize_content_view(true);
        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .expect("create window"),
        );
        // Start the launch banner clock when the window actually appears,
        // not at struct construction (which can precede the first frame).
        self.version_anim_start = Instant::now();
        // Without IME enabled, Hangul / kana would arrive as raw key
        // events instead of composing into 안 / 한 / 글.
        // We compose Hangul ourselves via the in-process hangul-ime
        // Composer, so the OS IME stays out of the way. Leaving the
        // platform IME on means macOS fires its own Preedit one key
        // late (the very first jamo after a script switch comes only
        // through KeyboardInput), which produced the "조합이 첫 글자만
        // 안 돼" symptom. With the platform IME disabled we still
        // receive the Hangul jamo on KeyboardInput.text because the
        // selected keyboard layout produces them — we just take the
        // composition into our own hands from there.
        // IME ownership splits per-platform:
        //   macOS: NSTextInputContext drops the first jamo after a
        //     script switch (only KeyboardInput.text fires), so we
        //     refuse OS IME and run hangul-ime/Composer ourselves.
        //   Windows / Linux: the OS IME is the only path that gets us
        //     completed Hangul syllables — set_ime_allowed(true) so
        //     Ime::Preedit / Ime::Commit drive composition.
        #[cfg(target_os = "macos")]
        window.set_ime_allowed(false);
        #[cfg(not(target_os = "macos"))]
        window.set_ime_allowed(true);
        // Cursor-blink timer thread. Ticks every blink half-period and
        // wakes the loop through the proxy, so about_to_wait can sit on
        // ControlFlow::Wait — no WaitUntil timer in the hot path for
        // macOS to coalesce. sleep() drift is irrelevant; the actual
        // phase is computed from last_input_at in cursor_blink_on.
        {
            let blink_proxy = self.proxy.clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_millis(BLINK_HALF_PERIOD_MS));
                if blink_proxy.send_event(UserEvent::Redraw).is_err() {
                    break;
                }
            });
        }
        // cell-renderer GPU path is the only path. The old sugarloaf
        // opt-in branch (KASATERM_RENDERER=sugarloaf) was removed once
        // cell-renderer absorbed P3 colour reproduction (shader
        // sRGB→DisplayP3 + root metal layer install). sugarloaf never
        // had the chrome UI ported across; keeping the branch in was
        // bloating the binary for no user-facing benefit.
        let renderer = gpu::GpuRenderer::new(window.clone(), FONT_SIZE)
            .expect("GpuRenderer init");
        self.cell = CellGeom {
            w: renderer.cell_w,
            h: renderer.cell_h,
            baseline: 0.0,
        };
        let scale = window.scale_factor() as f32 * self.ui_zoom;
        eprintln!(
            "[startup] gpu renderer; cell_geom w={:.2} h={:.2} (scale={scale})",
            self.cell.w, self.cell.h,
        );
        self.gpu = Some(renderer);
        self.window = Some(window);
        // Backend selection. Defaults to the Phase C direct-PTY path —
        // no tmux daemon, no `set -g focus-events` warnings inside
        // Claude Code, no kasaterm-cli's tmux quirks. KASATERM_BACKEND=tmux
        // opts back into the tmux-bridge multiplexer when the user wants
        // the multi-pane layout features that the in-process pty
        // multiplexer doesn't have yet.
        let want_tmux = std::env::var("KASATERM_BACKEND")
            .map(|v| v.eq_ignore_ascii_case("tmux"))
            .unwrap_or(false);
        let backend_result = if want_tmux {
            self.start_tmux()
        } else {
            self.start_pty()
        };
        if let Err(e) = backend_result {
            eprintln!("[tmuxify] backend start failed: {e}");
        }
        self.schedule_autosend();
        self.schedule_autocapture();
        self.arm_autosplit();
        self.arm_autowindows();
        self.arm_autotoggle();
        self.arm_autotabs();
        self.arm_autodrag();
        self.schedule_autoquit();
        self.open_git_panel(event_loop, false);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        id: WindowId,
        event: WindowEvent,
    ) {
        // The git panel is a second window with its own webview. Its events
        // must never reach the terminal logic below — in particular a panel
        // CloseRequested only drops the panel, it doesn't exit the app. The
        // webview paints itself, so we ignore everything else for it.
        if self.git_panel_window.as_ref().map(|w| w.id()) == Some(id) {
            match &event {
                WindowEvent::CloseRequested => {
                    self.git_panel_webview = None;
                    self.git_panel_window = None;
                }
                // The webview is pinned to a fixed (0,0)+size rect at build
                // time. Without re-bounding on resize it keeps its original
                // size while the NSView's bottom-left origin shoves it into
                // the corner (the empty-space-plus-corner bug). Track the
                // window so the panel stays full-bleed.
                WindowEvent::Resized(size) => {
                    if let Some(wv) = self.git_panel_webview.as_ref() {
                        let _ = wv.set_bounds(wry::Rect {
                            position: wry::dpi::PhysicalPosition::new(0, 0).into(),
                            size: wry::dpi::PhysicalSize::new(size.width, size.height).into(),
                        });
                    }
                }
                _ => {}
            }
            return;
        }
        // Same guard for the session panel. Without it the panel window's
        // Resized/ScaleFactorChanged events fall through to the terminal
        // handler below and call gpu.resize() with the panel's tiny size,
        // shrinking the main wgpu viewport uniform → everything renders
        // ~2x zoomed. (git panel had this guard, session panel did not.)
        if self.session_panel_window.as_ref().map(|w| w.id()) == Some(id) {
            match &event {
                WindowEvent::CloseRequested => {
                    self.session_panel_webview = None;
                    self.session_panel_window = None;
                }
                WindowEvent::Resized(size) => {
                    if let Some(wv) = self.session_panel_webview.as_ref() {
                        let _ = wv.set_bounds(wry::Rect {
                            position: wry::dpi::PhysicalPosition::new(0, 0).into(),
                            size: wry::dpi::PhysicalSize::new(size.width, size.height).into(),
                        });
                    }
                }
                _ => {}
            }
            return;
        }
        // Preview windows (image viewer / markdown editor): same isolation as
        // the panels above. A CloseRequested drops just that one entry
        // (window + its webview together); everything else is swallowed so a
        // preview window's resize never touches the terminal's wgpu surface.
        if let Some(pos) = self
            .preview_windows
            .iter()
            .position(|(w, _)| w.id() == id)
        {
            if matches!(event, WindowEvent::CloseRequested) {
                self.preview_windows.remove(pos);
            }
            return;
        }
        let Some(window) = self.window.clone() else { return; };
        // gpu path uses our own wgpu surface, sugarloaf path keeps
        // its renderer. Only resize / rescale touch the surface
        // owner — everything else (keyboard, mouse, IME, wheel,
        // redraw) is renderer-agnostic.
        let gpu_mode = self.gpu.is_some();
        // Any winit event that *isn't* RedrawRequested counts as a
        // chrome change for the damage gate. RedrawRequested itself
        // never sets the flag — otherwise the early-return at the
        // top of render_frame could never short-circuit a pure-PTY
        // burst.
        if !matches!(event, WindowEvent::RedrawRequested) {
            self.chrome_dirty = true;
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::ScaleFactorChanged { scale_factor: _, .. } => {
                let size = window.inner_size();
                if gpu_mode {
                    if let Some(g) = self.gpu.as_mut() {
                        g.resize(size.width, size.height);
                    }
                }
                // macOS live-resize coalesces queued RedrawRequested, so paint
                // synchronously here — otherwise the window frame leads and the
                // grid catches up a frame later (ghostty parity). Wrap in a
                // CATransaction with implicit animations off so AppKit doesn't
                // interpolate stale contents to the new bounds on zoom.
                self.chrome_dirty = true;
                gpu::with_disabled_layer_actions(|| {
                    self.render_frame();
                });
            }
            WindowEvent::Resized(size) => {
                if std::env::var_os("KASATERM_RESIZE_DEBUG").is_some() {
                    eprintln!(
                        "[rsz {}ms] Resized {}x{} live={}",
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_millis()
                            % 100000,
                        size.width,
                        size.height,
                        gpu::is_in_live_resize(&window)
                    );
                }
                // Beats-ghostty live-resize: chrome + cells reflow EVERY
                // Resized event. wgpu surface.configure + render_frame
                // happen every frame; PTY reshape (SIGWINCH + alacritty +
                // cell reflow) only fires when the integer cell count
                // actually shifted past a boundary — typically 5-10 times
                // per drag, cheap enough that the shell stays current
                // without spamming itself between cell-edge crossings.
                if gpu::is_in_live_resize(&window) {
                    self.pending_resize = Some(size);
                    gpu::with_disabled_layer_actions(|| {
                        if gpu_mode {
                            if let Some(g) = self.gpu.as_mut() {
                                g.resize(size.width, size.height);
                            }
                        }
                        let (cols, rows) = self.window_cells();
                        if (cols, rows) != self.last_resized_cells {
                            self.last_resized_cells = (cols, rows);
                            // Reshape the PTY on every cell-boundary crossing
                            // during a live drag. The (cols,rows) guard above
                            // already coalesces sub-cell pixel moves, so the
                            // shell reflows the instant the integer grid grows
                            // — no throttle, the divider path does the same.
                            self.resize_backend(cols, rows);
                        }
                        self.chrome_dirty = true;
                        self.render_frame();
                    });
                    return;
                }
                self.pending_resize = None;
                gpu::with_disabled_layer_actions(|| {
                    if gpu_mode {
                        if let Some(g) = self.gpu.as_mut() {
                            g.resize(size.width, size.height);
                        }
                    }
                    let (cols, rows) = self.window_cells();
                    if (cols, rows) != self.last_resized_cells {
                        self.last_resized_cells = (cols, rows);
                        self.resize_backend(cols, rows);
                    }
                    self.chrome_dirty = true;
                    self.render_frame();
                });
            }
            WindowEvent::ModifiersChanged(mods) => {
                self.modifiers = mods.state();
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.handle_wheel(delta);
            }
            WindowEvent::CursorMoved { position, .. } => {
                let scale = self.effective_scale();
                self.cursor_px = (position.x as f32 / scale, position.y as f32 / scale);
                // A deferred titlebar press turns into a window move once the
                // pointer travels past the threshold (so a stationary press
                // stays a click and the double-click path keeps working).
                if let Some((px, py)) = self.titlebar_drag_pending {
                    let (cx, cy) = self.cursor_px;
                    if (cx - px).abs() > 4.0 || (cy - py).abs() > 4.0 {
                        self.titlebar_drag_pending = None;
                        let _ = window.drag_window();
                        return;
                    }
                }
                // In-pane tab hover tracking — drives the hover-only × +
                // brightened text on inactive tabs. Updated on every move but
                // only redraws when the hovered tab actually changes.
                {
                    let (cx, cy) = self.cursor_px;
                    let new_hover = self
                        .pane_tab_rects
                        .iter()
                        .find(|(_, _, r)| cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3)
                        .map(|(id, idx, _)| (id.clone(), *idx));
                    if new_hover != self.pane_tab_hover {
                        self.pane_tab_hover = new_hover;
                        self.chrome_dirty = true;
                        window.request_redraw();
                    }
                }
                // Sidebar resize drag in progress: update width and reflow.
                if let Some((start_x, start_w)) = self.sidebar_resize {
                    let new_w = (start_w + (self.cursor_px.0 - start_x)).clamp(140.0, 520.0);
                    if (new_w - self.sidebar_w_logical).abs() > 0.5 {
                        self.sidebar_w_logical = new_w;
                        let (cols, rows) = self.window_cells();
                        self.resize_backend(cols, rows);
                        self.chrome_dirty = true;
                        window.set_cursor(CursorIcon::ColResize);
                        window.request_redraw();
                    }
                    return;
                }
                // Divider drag in progress: ghostty parity — visually
                // update on every cursor move (so the seam tracks the
                // cursor pixel-by-pixel), AND fire `resize_backend` on
                // every cell-boundary crossing so the shells reflow live.
                // The flicker that used to come with this is gone because:
                //   1. pump_pty_screens preserves cell content across a
                //      resize (no blank-then-fill gap)
                //   2. the render path clips cells to the layout pane
                //      rect, so any stale dims that bleed past the seam
                //      get truncated before the user sees them
                if let Some((path, dir)) = self.resize_drag.clone() {
                    let (cols, rows) = self.window_cells();
                    let pad = WINDOW_PADDING + self.effective_sidebar_w();
                    let pos = match dir {
                        pty_backend::SplitDir::Horizontal => (((self.cursor_px.0 - pad)
                            / self.cell.w.max(1.0))
                        .round() as i32)
                            .clamp(0, cols as i32) as u16,
                        pty_backend::SplitDir::Vertical => (((self.cursor_px.1 - TITLE_HEIGHT)
                            / self.cell.h.max(1.0))
                        .round() as i32)
                            .clamp(0, rows as i32) as u16,
                    };
                    if Some(pos) != self.last_divider_pos {
                        if let Some(tree) = self.pty_layout.as_mut() {
                            tree.resize_divider(&path, pos, cols, rows);
                        }
                        self.last_divider_pos = Some(pos);
                        self.publish_pty_layout();
                        // PTY reshape is the expensive bit (Claude Code does
                        // a full TUI repaint on every SIGWINCH). Layout
                        // updates every cursor move for the live seam, but
                        // SIGWINCH only fires at ~10 Hz so the shells don't
                        // melt down. The render-time clip hides the
                        // mismatch between layout dims and PTY dims.
                        let now = std::time::Instant::now();
                        let pty_throttle = self
                            .last_divider_pty_resize
                            .map(|t| now.duration_since(t)
                                >= std::time::Duration::from_millis(100))
                            .unwrap_or(true);
                        if pty_throttle {
                            self.resize_backend(cols, rows);
                            self.last_divider_pty_resize = Some(now);
                        }
                    }
                    window.request_redraw();
                    return;
                }
                // Tab reorder drag: flip to active past the threshold, then
                // re-derive the drop index from the cursor's x over this
                // pane's tab pills. The insertion bar is painted from
                // `tab_drag.target`.
                if self.tab_drag.is_some() {
                    let (px, py) = self.cursor_px;
                    let (start, src_pane) = {
                        let d = self.tab_drag.as_ref().unwrap();
                        (d.start, d.pane.clone())
                    };
                    let dx = self.cursor_px.0 - start.0;
                    let dy = self.cursor_px.1 - start.1;
                    // Per-pane horizontal extent of the tab strip, derived
                    // from each pane's tab pills (min(x) .. max(x+w)). The
                    // cursor counts as "over pane P" when its y is inside
                    // any of P's pills *and* its x is inside that x-range —
                    // crucially this still holds while the cursor sits over
                    // the + button or the action cluster (which interrupt
                    // the pill row), so the drop_pane doesn't flicker back
                    // to source mid-flight.
                    let mut drop_pane = src_pane.clone();
                    let mut strip_y: HashMap<String, (f32, f32)> = HashMap::new();
                    let mut strip_x: HashMap<String, (f32, f32)> = HashMap::new();
                    for (pid, _i, (rx, ry, rw, rh)) in &self.pane_tab_rects {
                        let y = strip_y
                            .entry(pid.clone())
                            .or_insert((*ry, ry + rh));
                        y.0 = y.0.min(*ry);
                        y.1 = y.1.max(ry + rh);
                        let x = strip_x
                            .entry(pid.clone())
                            .or_insert((*rx, rx + rw));
                        x.0 = x.0.min(*rx);
                        x.1 = x.1.max(rx + rw);
                    }
                    // Body-hit first — drop_target_at extends the hit box
                    // to include the strip, so the same pane stays the
                    // drop target when the cursor slides between body and
                    // strip. Strip y-range scan is a fallback for cursors
                    // that drop_target_at can't catch (e.g. between
                    // panes' gap).
                    if let Some((target_pane, _)) =
                        self.drop_target_at(px, py)
                    {
                        drop_pane = target_pane;
                    } else {
                        for (pid, (y0, y1)) in &strip_y {
                            if py >= *y0 && py <= *y1 {
                                drop_pane = pid.clone();
                                break;
                            }
                        }
                    }
                    // Insertion index = #pills of drop_pane whose midpoint sits
                    // left of cursor. Resets to 0 when the cursor enters a new
                    // pane's strip so the bar starts at that pane's left edge.
                    let mut target = 0usize;
                    for (pid, idx, (rx, _, rw, _)) in &self.pane_tab_rects {
                        if pid == &drop_pane && px > rx + rw / 2.0 {
                            target = idx + 1;
                        }
                    }
                    if let Some(d) = self.tab_drag.as_mut() {
                        if !d.active && dx * dx + dy * dy > 9.0 {
                            d.active = true;
                        }
                        d.target = target;
                        d.drop_pane = drop_pane;
                    }
                    if self.tab_drag.as_ref().map(|d| d.active).unwrap_or(false) {
                        window.set_cursor(CursorIcon::Grabbing);
                        self.chrome_dirty = true;
                        window.request_redraw();
                    }
                    return;
                }
                // Header drag in progress: flip to active once past the
                // threshold, then keep redrawing so the drop-zone overlay
                // tracks the cursor.
                if let Some(hd) = self.header_drag.as_mut() {
                    let dx = self.cursor_px.0 - hd.start.0;
                    let dy = self.cursor_px.1 - hd.start.1;
                    if !hd.active && dx * dx + dy * dy > 25.0 {
                        hd.active = true;
                    }
                    if hd.active {
                        window.set_cursor(CursorIcon::Grabbing);
                        window.request_redraw();
                    }
                    return;
                }
                // Drag inside a mouse-reporting TUI: relay motion as
                // SGR button-32 (left button held) into the same pane
                // we sent the press to, so Claude Code / vim / less
                // sees a continuous drag.
                if let Some(pane_id) = self.mouse_forward_pane.clone() {
                    if let Some((col, row)) =
                        self.px_to_cell_active(self.cursor_px.0, self.cursor_px.1)
                    {
                        self.send_mouse_sgr(&pane_id, 32, col, row, true);
                    }
                } else if let (Some(anchor), Some(cell)) = (
                    self.drag_anchor,
                    self.px_to_cell_active(self.cursor_px.0, self.cursor_px.1),
                ) {
                    self.selection = Some(Selection { anchor, end: cell });
                    window.request_redraw();
                } else {
                    // Hover feedback: show a resize cursor over a seam or the
                    // sidebar's right edge so they read as draggable.
                    let (cx, cy) = self.cursor_px;
                    let on_sidebar_edge = self.sidebar_visible
                        && cy > TITLE_HEIGHT
                        && (cx - self.sidebar_w_logical).abs() <= 3.0;
                    let icon = if on_sidebar_edge {
                        CursorIcon::ColResize
                    } else {
                        match self
                            .divider_at_px(self.cursor_px.0, self.cursor_px.1)
                            .map(|(_, d)| d)
                        {
                            Some(pty_backend::SplitDir::Horizontal) => CursorIcon::ColResize,
                            Some(pty_backend::SplitDir::Vertical) => CursorIcon::RowResize,
                            None => CursorIcon::Default,
                        }
                    };
                    window.set_cursor(icon);
                    // Hover glow on chrome buttons (+ / action cluster) needs
                    // a redraw on every move — paint reads self.cursor_px to
                    // decide which button is under the cursor.
                    self.chrome_dirty = true;
                    window.request_redraw();
                }
            }
            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                // Pane header × close button. Catches clicks anywhere
                // in the multi-pane workspace before we drop into the
                // cell-grid click path.
                if matches!(state, ElementState::Pressed) {
                    let cx = self.cursor_px.0;
                    let cy = self.cursor_px.1;
                    // Shell picker popup. While open it owns the next click:
                    // hit an item → spawn that shell in a new window; click
                    // anywhere else → dismiss. Checked first so it captures
                    // clicks before the sidebar / cell grid underneath.
                    if self.shell_menu_open {
                        let pick = self
                            .shell_menu_hits
                            .iter()
                            .find(|(_, r)| {
                                cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3
                            })
                            .map(|(s, _)| s.clone());
                        self.shell_menu_open = false;
                        self.chrome_dirty = true;
                        if let Some(shell) = pick {
                            self.pending_shell = Some(shell);
                            self.new_window();
                        }
                        return;
                    }
                    // Sidebar-toggle button in the title strip (right of the
                    // traffic lights). Caught before the title-bar drag path
                    // so the click toggles instead of moving the window.
                    {
                        let (bx, by, bw, bh) = Self::sidebar_toggle_rect();
                        if cx >= bx && cx <= bx + bw && cy >= by && cy <= by + bh {
                            self.toggle_sidebar();
                            return;
                        }
                    }
                    // Sidebar resize grip — a 6px hot zone straddling the
                    // sidebar's right edge below the title strip. Caught
                    // before the sidebar click path so dragging the edge
                    // resizes instead of clicking the last sidebar column.
                    if self.sidebar_visible && cy > TITLE_HEIGHT {
                        let edge = self.sidebar_w_logical;
                        if cx >= edge - 3.0 && cx <= edge + 3.0 {
                            self.sidebar_resize = Some((cx, self.sidebar_w_logical));
                            return;
                        }
                    }
                    // Left window-tab sidebar. Caught first — it owns the whole
                    // left strip, so a click there never falls through to the
                    // cell grid. Order: close-× (sits on top of a tab) → tab →
                    // "+" new-window button.
                    if self.sidebar_visible && cx < self.sidebar_w_logical {
                        let inside =
                            |r: &(f32, f32, f32, f32)| cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3;
                        if let Some(idx) = self
                            .window_tab_close_rects
                            .iter()
                            .find(|(_, r)| inside(r))
                            .map(|(i, _)| *i)
                        {
                            if let Err(e) = self.close_window(idx) {
                                eprintln!("[window] close failed: {e:#}");
                            }
                            return;
                        }
                        if let Some(idx) = self
                            .window_tab_rects
                            .iter()
                            .find(|(_, r)| inside(r))
                            .map(|(i, _)| *i)
                        {
                            self.switch_window(idx);
                            return;
                        }
                        if self.new_window_btn_rect.map(|r| inside(&r)).unwrap_or(false) {
                            // The shell picker only has entries on Windows
                            // (PowerShell/CMD/Git Bash/WSL). On macOS/Linux
                            // `available_shells()` is empty, so toggling the
                            // menu would just swallow the click and never open
                            // a tab — spawn a default window directly instead.
                            if available_shells().is_empty() {
                                self.new_window();
                            } else {
                                self.shell_menu_open = !self.shell_menu_open;
                            }
                            self.chrome_dirty = true;
                            return;
                        }
                        // Empty sidebar space — swallow the click.
                        return;
                    }
                    // Code-block copy button. Checked before the cell-grid /
                    // mouse-forward path so a click lands on the button even
                    // inside a mouse-reporting TUI (Claude Code), the same
                    // way the Shift escape hatch steals selection.
                    if let Some(text) = self
                        .copy_btn_rects
                        .iter()
                        .find(|(_, r)| {
                            cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3
                        })
                        .map(|(t, _)| t.clone())
                    {
                        self.copy_block_text(&text);
                        window.request_redraw();
                        return;
                    }
                    // Markdown header Render/Raw toggle → switch that pane's mode.
                    // Entering Raw seeds the edit buffer from the doc source.
                    if let Some((id, is_raw)) = self
                        .md_toggle_rects
                        .iter()
                        .find(|(_, _, r)| {
                            cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3
                        })
                        .map(|(id, raw, _)| (id.clone(), *raw))
                    {
                        {
                            let mut ws = self.ws.lock().unwrap();
                            if let Some(pane) = ws.panes.get_mut(&id) {
                                pane.dirty = true;
                                if is_raw {
                                    // Render → Raw: seed the edit buffer.
                                    if let Some(m) = pane.markdown_mut() {
                                        if !m.raw_mode {
                                            m.edit_lines =
                                                m.doc.raw.split('\n').map(String::from).collect();
                                            if m.edit_lines.is_empty() {
                                                m.edit_lines.push(String::new());
                                            }
                                            m.cur_line = 0;
                                            m.cur_col = 0;
                                            m.scroll = 0;
                                        }
                                        m.raw_mode = true;
                                    }
                                } else {
                                    // Raw → Render: write the file + re-parse so
                                    // the rendered view reflects the edits.
                                    let save = pane
                                        .markdown()
                                        .filter(|m| m.raw_mode)
                                        .map(|m| (m.edit_lines.join("\n"), m.doc.path.clone()));
                                    if let Some((text, path)) = save {
                                        let _ = std::fs::write(&path, &text);
                                        let doc = build_markdown_doc(
                                            &id,
                                            std::path::Path::new(&path),
                                            &text,
                                        );
                                        if let Some(m) = pane.markdown_mut() {
                                            m.doc = Arc::new(doc);
                                            m.scroll = 0;
                                        }
                                    }
                                    if let Some(m) = pane.markdown_mut() {
                                        m.raw_mode = false;
                                    }
                                }
                            }
                        }
                        window.request_redraw();
                        return;
                    }
                    // Terminal-pane right-action cluster (new-terminal /
                    // web / split-v / split-h). Web spawns a separate OS
                    // window with a wry browser; the other variants are
                    // wired by the main pane-model.
                    if let Some((pid, action)) = self
                        .pane_action_hits
                        .iter()
                        .find(|(_, _, r)| cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3)
                        .map(|(id, a, _)| (id.clone(), *a))
                    {
                        // Focus the clicked pane so splits/new-tabs target it.
                        self.ws.lock().unwrap().active_pane = Some(pid.clone());
                        match action {
                            ActionKind::SplitV => {
                                if let Err(e) = self
                                    .split_active_pane(pty_backend::SplitDir::Vertical)
                                {
                                    eprintln!("[split-v] {e}");
                                }
                            }
                            ActionKind::SplitH => {
                                if let Err(e) = self
                                    .split_active_pane(pty_backend::SplitDir::Horizontal)
                                {
                                    eprintln!("[split-h] {e}");
                                }
                            }
                        }
                        window.request_redraw();
                        return;
                    }
                    // Image-pane action buttons (zoom-out/in, rotate, reset).
                    // Checked before the tab/plus path so the image-only
                    // chrome cluster is never swallowed by tab hit-tests.
                    if let Some((pid, kind)) = self
                        .image_btn_rects
                        .iter()
                        .find(|(_, _, r)| cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3)
                        .map(|(id, k, _)| (id.clone(), *k))
                    {
                        if let Ok(mut ws) = self.ws.lock() {
                            if let Some(pane) = ws.panes.get_mut(&pid) {
                                let z = pane.image_view_zoom();
                                match kind {
                                    ImageBtn::ZoomIn => {
                                        pane.image_zoom = (z * 1.25).clamp(1.0, 8.0);
                                    }
                                    ImageBtn::ZoomOut => {
                                        pane.image_zoom = (z / 1.25).max(1.0);
                                    }
                                    ImageBtn::Rotate => {
                                        pane.image_rot = (pane.image_rot + 1) % 4;
                                    }
                                    ImageBtn::Reset => {
                                        pane.image_zoom = 1.0;
                                        pane.image_rot = 0;
                                    }
                                }
                                pane.dirty = true;
                            }
                            ws.active_pane = Some(pid);
                        }
                        self.chrome_dirty = true;
                        window.request_redraw();
                        return;
                    }
                    // In-pane tab bar: + new-tab, per-tab × close, tab switch.
                    // Checked before the cell grid so a header click never
                    // selects text. (Stage 2: tabs are visual labels; each
                    // tab's real PTY/content lands in stage 3.)
                    if let Some(pid) = self
                        .pane_plus_rects
                        .iter()
                        .find(|(_, r)| cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3)
                        .map(|(id, _)| id.clone())
                    {
                        // Stage 3: spawn a real PTY-backed tab. spawn_new_tab
                        // pushes a PaneTab with its own pid and sets active.
                        if let Err(e) = self.spawn_new_tab(&pid) {
                            eprintln!("[spawn_new_tab] {e}");
                        }
                        window.request_redraw();
                        return;
                    }
                    if let Some((pid, idx)) = self
                        .pane_tab_close_rects
                        .iter()
                        .find(|(_, _, r)| cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3)
                        .map(|(id, i, _)| (id.clone(), *i))
                    {
                        let tabs_len = self
                            .ws
                            .lock()
                            .unwrap()
                            .panes
                            .get(&pid)
                            .map(|p| p.tabs.len())
                            .unwrap_or(0);
                        if tabs_len <= 1 {
                            // Single tab → closing it closes the pane.
                            self.remove_pane(&pid);
                        } else {
                            self.close_tab(&pid, idx);
                        }
                        window.request_redraw();
                        return;
                    }
                    if let Some((pid, idx)) = self
                        .pane_tab_rects
                        .iter()
                        .find(|(_, _, r)| cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3)
                        .map(|(id, i, _)| (id.clone(), *i))
                    {
                        // Focus the pane now; arm a tab drag. A plain press
                        // (no movement) switches to this tab on release; a
                        // drag past the threshold reorders instead.
                        if let Ok(mut ws) = self.ws.lock() {
                            ws.active_pane = Some(pid.clone());
                        }
                        self.tab_drag = Some(TabDrag {
                            pane: pid.clone(),
                            from: idx,
                            start: self.cursor_px,
                            active: false,
                            target: idx,
                            drop_pane: pid,
                        });
                        window.request_redraw();
                        return;
                    }
                    // Grab a split seam → start a divider drag. Checked
                    // before the cell-grid click so dragging the boundary
                    // never doubles as a text selection in the pane under
                    // it.
                    if let Some((path, dir)) =
                        self.divider_at_px(self.cursor_px.0, self.cursor_px.1)
                    {
                        self.resize_drag = Some((path, dir));
                        return;
                    }
                    // Press on a pane header (not the × button) → focus it
                    // and arm a drag-and-drop relocation. It only becomes
                    // a real drag once the cursor passes the threshold, so
                    // a plain header click just focuses.
                    if let Some(pane) =
                        self.header_at_px(self.cursor_px.0, self.cursor_px.1)
                    {
                        self.ws.lock().unwrap().active_pane = Some(pane.clone());
                        self.header_drag = Some(HeaderDrag {
                            pane,
                            start: self.cursor_px,
                            active: false,
                        });
                        self.refresh_socket_snapshot();
                        window.request_redraw();
                        return;
                    }
                }
                // Title bar (above the cell grid, right of the traffic
                // lights) → double-click toggles maximize, a single
                // drag moves the window — the macOS native chrome we
                // lost when we turned on fullsize_content_view. macOS
                // owns the traffic-light cluster, so we only act past
                // its width.
                if matches!(state, ElementState::Pressed)
                    && self.cursor_px.1 < TITLE_HEIGHT
                    && self.cursor_px.0 > TRAFFIC_LIGHT_WIDTH
                {
                    let (cx, cy) = self.cursor_px;
                    let now = Instant::now();
                    let is_double = match self.last_left_click {
                        Some((t, (x, y)))
                            if now.duration_since(t).as_millis() < 400
                                && (x - cx).abs() < 5.0
                                && (y - cy).abs() < 5.0 =>
                        {
                            true
                        }
                        _ => false,
                    };
                    self.last_left_click = Some((now, (cx, cy)));
                    if is_double {
                        window.set_maximized(!window.is_maximized());
                        if std::env::var_os("KASATERM_RESIZE_DEBUG").is_some() {
                            eprintln!(
                                "[rsz {}ms] set_maximized -> {}",
                                std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap()
                                    .as_millis()
                                    % 100000,
                                window.is_maximized()
                            );
                        }
                        self.last_left_click = None;
                        self.titlebar_drag_pending = None;
                        return;
                    }
                    // Defer the actual window-move until the pointer moves —
                    // calling drag_window() here would enter AppKit's modal
                    // loop and swallow the second click of a double-click.
                    self.titlebar_drag_pending = Some((cx, cy));
                    return;
                }
                match state {
                    ElementState::Pressed => {
                        if let Some((pane_id, col, row)) =
                            self.px_to_pane_cell(self.cursor_px.0, self.cursor_px.1)
                        {
                            let switched = {
                                let mut ws = self.ws.lock().unwrap();
                                let switched =
                                    ws.active_pane.as_deref() != Some(pane_id.as_str());
                                ws.active_pane = Some(pane_id.clone());
                                switched
                            };
                            if switched {
                                self.selection = None;
                                self.drag_anchor = None;
                                self.mouse_forward_pane = None;
                            } else if self.pane_takes_mouse(&pane_id) {
                                // Hand the press to the TUI. Its own
                                // selection / copy-on-select kicks in
                                // (Claude Code spawns `pbcopy`).
                                self.selection = None;
                                self.drag_anchor = None;
                                self.send_mouse_sgr(&pane_id, 0, col, row, true);
                                self.mouse_forward_pane = Some(pane_id.clone());
                            } else if !self.pane_is_terminal(&pane_id) {
                                // Markdown / image panes are document views,
                                // not terminals — a drag here must not start a
                                // cell text-selection. Focus already switched.
                                // A click on a code-block copy button copies it;
                                // otherwise a click on a link opens it.
                                self.selection = None;
                                self.drag_anchor = None;
                                if !self.try_copy_md_block() {
                                    self.try_open_md_link();
                                }
                            } else {
                                self.drag_anchor = Some((col, row));
                                self.selection = Some(Selection {
                                    anchor: (col, row),
                                    end: (col, row),
                                });
                            }
                            self.last_input_at = Instant::now();
                            if let Some(tmux) = self.tmux.as_ref() {
                                let _ =
                                    tmux.send_cmd(&format!("select-pane -t '{pane_id}'"));
                            }
                        }
                    }
                    ElementState::Released => {
                        // A titlebar press that never moved past the drag
                        // threshold: just a click, drop the deferred move.
                        self.titlebar_drag_pending = None;
                        // End a tab drag: a real drag reorders the pane's tab
                        // list; a plain press just switches to that tab.
                        if let Some(mut td) = self.tab_drag.take() {
                            window.set_cursor(CursorIcon::Default);
                            // Tab → pane BODY drop: split the target pane in
                            // the quadrant the cursor landed in and place the
                            // moved tab as the new leaf. Eats the old
                            // header-drag UX (drop in body = relocate) but
                            // unified into the tab drag so the user never has
                            // to find non-tab space on the header.
                            // drop_target_at already covers the strip area
                            // (box extends up to the pane's tab strip), so
                            // we no longer need the over_strip fallback —
                            // it was the source of body↔strip flicker.
                            let body_drop: Option<(String, DropZone)> = if td.active {
                                self.drop_target_at(self.cursor_px.0, self.cursor_px.1)
                            } else {
                                None
                            };
                            if let Some((target, zone)) = body_drop {
                                // Center on header = tab merge — route
                                // through the cross-pane path below by
                                // rewriting drop_pane; Center on self
                                // cancels (drop on own header is a no-op).
                                if zone == DropZone::Center {
                                    if target != td.pane {
                                        let dst_len = self
                                            .ws
                                            .lock()
                                            .unwrap()
                                            .panes
                                            .get(&target)
                                            .map(|p| p.tabs.len())
                                            .unwrap_or(0);
                                        td.drop_pane = target.clone();
                                        td.target = dst_len;
                                    } else {
                                        self.chrome_dirty = true;
                                        window.request_redraw();
                                        return;
                                    }
                                    // Fall through to cross_pane merge.
                                } else {
                                let src_tab_count = self
                                    .ws
                                    .lock()
                                    .unwrap()
                                    .panes
                                    .get(&td.pane)
                                    .map(|p| p.tabs.len())
                                    .unwrap_or(0);
                                if target == td.pane && src_tab_count == 1 {
                                    // Single-tab pane dropped on its own body
                                    // half: the user "threw" the pane to that
                                    // side. Spawn a fresh shell on the
                                    // OPPOSITE side so the original sits where
                                    // it was dropped.
                                    if let Err(e) =
                                        self.split_pane_opposite(&td.pane, zone)
                                    {
                                        eprintln!("[split-opposite] {e}");
                                    }
                                    self.chrome_dirty = true;
                                    window.request_redraw();
                                    return;
                                }
                                if target != td.pane || src_tab_count > 1 {
                                    // Multi-tab same-pane → lift dragged tab
                                    // into a new pane on the drop side.
                                    // Cross-pane → moved tab in a new pane on
                                    // target's drop side.
                                    self.drop_tab_into_body(&td, &target, zone);
                                    self.chrome_dirty = true;
                                    window.request_redraw();
                                    return;
                                }
                                }
                            }
                            let cross_pane = td.active && td.drop_pane != td.pane;
                            if cross_pane {
                                // Move the tab to another pane. We do this in
                                // 3 steps:
                                //   1. lift the PaneTab out of source.tabs
                                //   2. update pid_to_pane so future PTY output
                                //      routes to the destination pane
                                //   3. insert at the target index in dest.tabs;
                                //      if source ends up empty, collapse the
                                //      source pane out of the layout entirely
                                let mut moved_pid: Option<String> = None;
                                let mut moved: Option<PaneTab> = None;
                                let mut src_empty = false;
                                {
                                    let mut ws = self.ws.lock().unwrap();
                                    if let Some(src) = ws.panes.get_mut(&td.pane) {
                                        let n = src.tabs.len();
                                        if td.from < n {
                                            let tab = src.tabs.remove(td.from);
                                            moved_pid = tab.pid.clone();
                                            moved = Some(tab);
                                            if td.from < src.active_tab && src.active_tab > 0 {
                                                src.active_tab -= 1;
                                            }
                                            if src.active_tab >= src.tabs.len()
                                                && !src.tabs.is_empty()
                                            {
                                                src.active_tab = src.tabs.len() - 1;
                                            }
                                            src.dirty = true;
                                            src_empty = src.tabs.is_empty();
                                        }
                                    }
                                    if let (Some(tab), Some(pid)) =
                                        (moved.take(), moved_pid.clone())
                                    {
                                        // Re-bind the pid to the new outer.
                                        ws.pid_to_pane.insert(pid, td.drop_pane.clone());
                                        if let Some(dst) = ws.panes.get_mut(&td.drop_pane) {
                                            let to = td.target.min(dst.tabs.len());
                                            dst.tabs.insert(to, tab);
                                            dst.active_tab = to;
                                            dst.dirty = true;
                                        }
                                    }
                                    if src_empty {
                                        // Source has no tabs left — drop the
                                        // outer entry so remove_pane below can
                                        // collapse the layout cleanly.
                                        ws.panes.remove(&td.pane);
                                    }
                                }
                                if src_empty {
                                    // Source is empty because every tab — INCLUDING the
                                    // primary whose pid equalled the outer id — went to
                                    // dest. `remove_pane` would kill self.pty[outer]
                                    // here, which is the very PtySession we just handed
                                    // to dest. Use a layout-only collapse that leaves
                                    // self.pty / image textures / markdown untouched
                                    // since those resources now belong to dest.
                                    self.collapse_layout_only(&td.pane);
                                }
                                // Focus the destination pane so the moved
                                // tab is immediately interactive.
                                self.ws.lock().unwrap().active_pane =
                                    Some(td.drop_pane.clone());
                            } else if let Ok(mut ws) = self.ws.lock() {
                                if let Some(pane) = ws.panes.get_mut(&td.pane) {
                                    let n = pane.tabs.len();
                                    if td.active && n > 1 {
                                        let from = td.from.min(n - 1);
                                        let mut to = td.target.min(n);
                                        if to > from {
                                            to -= 1;
                                        }
                                        let item = pane.tabs.remove(from);
                                        let to = to.min(pane.tabs.len());
                                        pane.tabs.insert(to, item);
                                        // Dragging a tab selects it at its new spot.
                                        pane.active_tab = to;
                                    } else {
                                        // Plain click → switch to the pressed tab.
                                        pane.active_tab = td.from.min(n.saturating_sub(1));
                                    }
                                    pane.dirty = true;
                                }
                            }
                            self.chrome_dirty = true;
                            window.request_redraw();
                            return;
                        }
                        // End a sidebar resize drag (no other commit needed —
                        // the live width is already in self.sidebar_w_logical).
                        if self.sidebar_resize.take().is_some() {
                            window.set_cursor(CursorIcon::Default);
                            return;
                        }
                        // End a divider drag without falling through to the
                        // selection-release path under it.
                        if let Some((path, dir)) = self.resize_drag.take() {
                            // Final flush — the throttle may have suppressed
                            // the cursor's last cell-crossing, leaving the
                            // divider at a stale pos. Re-derive from the
                            // current cursor and apply once authoritatively.
                            let (cols, rows) = self.window_cells();
                            let pad = WINDOW_PADDING + self.effective_sidebar_w();
                            let pos = match dir {
                                pty_backend::SplitDir::Horizontal => (((self.cursor_px.0
                                    - pad)
                                    / self.cell.w.max(1.0))
                                .round() as i32)
                                    .clamp(0, cols as i32)
                                    as u16,
                                pty_backend::SplitDir::Vertical => (((self.cursor_px.1
                                    - TITLE_HEIGHT)
                                    / self.cell.h.max(1.0))
                                .round() as i32)
                                    .clamp(0, rows as i32)
                                    as u16,
                            };
                            if let Some(tree) = self.pty_layout.as_mut() {
                                tree.resize_divider(&path, pos, cols, rows);
                            }
                            self.resize_backend(cols, rows);
                            self.last_divider_pos = None;
                            self.last_divider_pty_resize = None;
                            window.request_redraw();
                            return;
                        }
                        // Drop a header drag: relocate onto the target
                        // pane's edge. A non-active drag was just a click
                        // (focus already happened on press), so we only
                        // reset the cursor.
                        if let Some(hd) = self.header_drag.take() {
                            window.set_cursor(CursorIcon::Default);
                            if hd.active {
                                if let Some((target, zone)) =
                                    self.drop_target_at(self.cursor_px.0, self.cursor_px.1)
                                {
                                    self.move_pane(&hd.pane, &target, zone);
                                }
                            }
                            return;
                        }
                        // Mouse-reporting drag end: forward the release
                        // so the TUI can finalize its selection /
                        // copy-on-select.
                        if let Some(pane_id) = self.mouse_forward_pane.take() {
                            if let Some((col, row)) =
                                self.px_to_cell_active(self.cursor_px.0, self.cursor_px.1)
                            {
                                self.send_mouse_sgr(&pane_id, 0, col, row, false);
                            }
                        } else {
                            self.drag_anchor = None;
                            if let Some(sel) = self.selection {
                                if sel.anchor == sel.end {
                                    self.selection = None;
                                } else {
                                    self.copy_selection();
                                }
                            }
                        }
                    }
                }
                window.request_redraw();
            }
            WindowEvent::Ime(ime) => {
                if std::env::var_os("KASATERM_IME_DEBUG").is_some() {
                    eprintln!("[ime] event={ime:?}");
                }
                match ime {
                    Ime::Enabled => {
                        // OS IME just took ownership of the keyboard
                        // (script switch / app focus). Mark active so
                        // the KeyboardInput branch drops any echo of
                        // text the IME will deliver via Preedit/Commit.
                        self.ime_active = true;
                        self.in_preedit = false;
                        self.preedit.clear();
                    }
                    Ime::Disabled => {
                        self.ime_active = false;
                        self.in_preedit = false;
                        self.preedit.clear();
                    }
                    Ime::Preedit(text, _range) => {
                        // Receiving a Preedit implies the IME is
                        // active — winit doesn't always emit Enabled
                        // first on macOS, so we set both flags here.
                        self.ime_active = true;
                        self.in_preedit = !text.is_empty();
                        self.preedit = text;
                    }
                    Ime::Commit(text) => {
                        self.in_preedit = false;
                        self.preedit.clear();
                        self.send_bytes(text.as_bytes());
                    }
                }
                // Preedit is chrome, not PTY grid — flag it so the damage
                // gate actually paints the composing text this frame.
                self.chrome_dirty = true;
                window.request_redraw();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                // KASATERM_KEY_DEBUG=1 → dump every key event with its
                // modifier snapshot. Used to debug "Cmd+= doesn't zoom"
                // class issues where it's unclear whether the OS even
                // forwards the chord to us or our handler ignores it.
                if std::env::var_os("KASATERM_KEY_DEBUG").is_some() {
                    eprintln!(
                        "[key] state={:?} physical={:?} logical={:?} text={:?} super={} ctrl={} shift={} alt={}",
                        event.state,
                        event.physical_key,
                        event.logical_key,
                        event.text,
                        self.modifiers.super_key(),
                        self.modifiers.control_key(),
                        self.modifiers.shift_key(),
                        self.modifiers.alt_key(),
                    );
                }
                self.forward_key(&event);
            }
            WindowEvent::DroppedFile(path) => {
                // Drag-and-drop → type the file's shell-quoted path into
                // the active pane (iTerm behavior). claude code reads an
                // image path dropped this way and attaches it. The
                // trailing space separates it from whatever the user
                // types next. Single-quote so spaces in the path stay
                // one token; embedded quotes get the '\'' escape.
                let p = path.to_string_lossy();
                let quoted = format!("'{}' ", p.replace('\'', "'\\''"));
                self.send_bytes(quoted.as_bytes());
            }
            WindowEvent::RedrawRequested => {
                self.render_frame();
                self.maybe_update_window_title();
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Live-resize flush: if a Resized arrived while the user was dragging
        // an edge we stashed it and skipped the actual resize work. Once the
        // user lets go, inLiveResize flips false and we replay the final size
        // here — surface.configure + PTY reshape + render happen once,
        // off the critical path of the live-resize tracking loop.
        if let (Some(window), Some(size)) =
            (self.window.clone(), self.pending_resize)
        {
            if !gpu::is_in_live_resize(&window) {
                if std::env::var_os("KASATERM_RESIZE_DEBUG").is_some() {
                    eprintln!(
                        "[rsz {}ms] about_to_wait flush {}x{}",
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_millis()
                            % 100000,
                        size.width,
                        size.height
                    );
                }
                self.pending_resize = None;
                let gpu_mode = self.gpu.is_some();
                gpu::with_disabled_layer_actions(|| {
                    if gpu_mode {
                        if let Some(g) = self.gpu.as_mut() {
                            g.resize(size.width, size.height);
                        }
                    }
                    let (cols, rows) = self.window_cells();
                    if (cols, rows) != self.last_resized_cells {
                        self.last_resized_cells = (cols, rows);
                        self.resize_backend(cols, rows);
                    }
                    self.chrome_dirty = true;
                    self.render_frame();
                });
            }
        }
        // Drain menu clicks from muda's global channel. The "Git 패널" item
        // toggles the panel (open/close), bypassing the env gate.
        while let Ok(ev) = muda::MenuEvent::receiver().try_recv() {
            if self.git_menu_item.as_ref().map(|m| m.id()) == Some(&ev.id) {
                self.toggle_git_panel(event_loop);
            } else if self.session_menu_item.as_ref().map(|m| m.id()) == Some(&ev.id) {
                self.toggle_session_panel(event_loop);
            }
        }
        // Headless verification: clean-exit once the autoquit deadline passes
        // so save-on-exit (and the next launch's restore) can be tested.
        if let Some(at) = self.autoquit_at {
            if std::time::Instant::now() >= at {
                event_loop.exit();
                return;
            }
        }
        // Fire any queued session-restore commands whose delay has elapsed.
        // Each carries its own PtySession so a resume reaches the right pane in
        // any session (active or stashed background).
        if !self.pending_restores.is_empty() {
            let now = std::time::Instant::now();
            self.pending_restores.retain(|(sess, cmd, at)| {
                if now >= *at {
                    let _ = sess.send_bytes(cmd.as_bytes());
                    false
                } else {
                    true
                }
            });
        }
        // Reap dead pty sessions before anything else — a closed shell
        // should disappear from the layout on the very next loop turn
        // so the user sees the gap collapse immediately.
        self.reap_dead_panes(event_loop);
        // Drain socket commands from external cmux clients. These run
        // through the same split/focus/send paths Cmd+D etc use, so
        // visible behavior is identical regardless of whether the
        // trigger came from a keystroke or a JSON-RPC call.
        self.drain_socket_inbox(event_loop);
        // Fire any due KASATERM_AUTOSPLIT step before parking. No-op
        // when no plan is queued.
        self.run_pending_autosplits();
        self.run_pending_autowindows();
        self.run_pending_autodrag();
        self.run_pending_autotoggle();
        self.run_pending_autotabs();
        // Pure event-driven loop, like Ghostty. A WaitUntil timer poll
        // gets coalesced by macOS, so a cross-thread wake (PTY echo via
        // the proxy) landed anywhere from 6ms to ~290ms late — that was
        // the inconsistent input lag. With `Wait` the loop sleeps with
        // zero latency until a real event arrives:
        //   - keystrokes  → window_event
        //   - PTY echo     → proxy UserEvent (ScreenUpdate thread)
        //   - cursor blink → proxy UserEvent (dedicated blink thread)
        // Each of those drives a redraw directly, so there's no timer in
        // the hot path to be coalesced.
        //
        // Exception: while the launch build banner is still fading we DO
        // need a timer, since nothing else is producing frames. Re-arm a
        // ~30fps WaitUntil until the fade finishes, then fall back to the
        // idle Wait. (new_events → request_redraw on the timer fire.)
        // The copy toast fade needs the same treatment as the launch banner.
        if self.version_alpha() > 0.0 || self.copy_toast_alpha() > 0.0 {
            event_loop.set_control_flow(ControlFlow::WaitUntil(
                Instant::now() + std::time::Duration::from_millis(33),
            ));
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }

    fn new_events(
        &mut self,
        _event_loop: &ActiveEventLoop,
        cause: winit::event::StartCause,
    ) {
        // The blink-timer fire path. When winit wakes us because the
        // WaitUntil deadline elapsed (no other events arrived), repaint
        // so the cursor block toggles its phase. Other wake causes
        // (input, redraw, init) drive their own redraws.
        if matches!(cause, winit::event::StartCause::ResumeTimeReached { .. }) {
            if let Some(w) = &self.window {
                w.request_redraw();
            }
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

/// Working directory for a freshly spawned shell. Terminals open new
/// sessions in the user's HOME by default (Terminal.app, iTerm), so a
/// double-clicked kasaterm.app — whose process cwd is `/` — would
/// otherwise leave the shell at root, where `cd Desktop` fails. Prefer
/// HOME; fall back to the process cwd only when HOME is unset.
/// Recursively walk a RestoreNode looking for the first leaf with a cwd.
/// Used at launch to inherit the previous session's working directory
/// without spinning up the rest of its layout.
fn first_leaf_cwd(node: &socket::RestoreNode) -> Option<String> {
    match node {
        socket::RestoreNode::Leaf(p) => p
            .cwd
            .as_ref()
            .and_then(|c| c.to_str().map(String::from)),
        socket::RestoreNode::Split { a, b, .. } => {
            first_leaf_cwd(a).or_else(|| first_leaf_cwd(b))
        }
    }
}

fn resolve_initial_cwd() -> Option<String> {
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
