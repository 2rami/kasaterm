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
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use std::collections::{HashMap, VecDeque};
use std::error::Error;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use sugarloaf::layout::RootStyle;
use sugarloaf::{Sugarloaf, SugarloafRenderer, SugarloafWindow, SugarloafWindowSize};
use tmux_bridge::layout::{parse_layout, Layout};
use tmux_bridge::screen::Cell as GridCell;
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

const FONT_SIZE: f32 = 14.0;

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
/// Line spacing multiplier passed to `compute_cell_metrics`. 1.0 keeps
/// rows at font ascent+descent so the cell aspect ratio stays close to
/// 1:2 — that's what makes half-block sprite art (Claude Code's mascot,
/// `▀▄▌▐` characters) read as squares instead of tall rectangles. The
/// earlier 1.3 stretched cells to 1:3 and made the sprite look
/// elongated next to Ghostty / iTerm2.
const LINE_HEIGHT_MULT: f32 = 1.0;
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
const PANE_HEADER_HEIGHT: f32 = 28.0;
/// Inner padding between a pane's box edges and its cell grid, in logical
/// pixels. Keeps text off the divider / window edge and gives abutting
/// panes visible breathing room. The PTY's usable cols/rows shrink by the
/// equivalent cell count so the grid still fits inside the inset box, and
/// every render origin + click-to-cell map applies the same offset.
const PANE_INNER_X: f32 = 3.0;
const PANE_INNER_Y: f32 = 2.0;
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
const WHEEL_THROTTLE_MS: u64 = 8;
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
    // Only offer close when more than one session exists — the last
    // session can't be closed (the terminal always needs one).
    if (count > 1) {
      const x = document.createElement("button");
      x.className = "close"; x.textContent = "×"; x.title = "세션 닫기";
      x.onclick = (e) => { e.stopPropagation(); closeSession(i); };
      li.appendChild(x);
    }
    if (i !== active) li.onclick = () => switchTo(i);
    ul.appendChild(li);
  }
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

/// Cell width / height / baseline in logical pixels. Filled at startup
/// from `Sugarloaf::compute_cell_metrics` so columns align with the
/// actual font advance instead of a hardcoded guess. Falls back to a
/// reasonable default before the first measurement lands.
#[derive(Copy, Clone, Debug)]
struct CellGeom {
    w: f32,
    h: f32,
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
        for cell in row.iter().take(ce + 1).skip(cs) {
            if cell.ch.is_empty() {
                out.push(' ');
            } else {
                out.push_str(&cell.ch);
            }
        }
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
        for cell in row.iter().take(right + 1).skip(left) {
            if cell.ch.is_empty() {
                line.push(' ');
            } else {
                line.push_str(&cell.ch);
            }
        }
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
    if now.duration_since(*last_emit) < std::time::Duration::from_millis(WHEEL_THROTTLE_MS) {
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
/// splits horizontally, Up/Down splits vertically.
#[derive(Clone, Copy, PartialEq)]
enum DropZone {
    Left,
    Right,
    Up,
    Down,
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

#[derive(Default)]
struct PaneState {
    rows: u16,
    cols: u16,
    cells: Vec<Vec<GridCell>>,
    cursor_row: u16,
    cursor_col: u16,
    cursor_visible: bool,
    alt_screen: bool,
    mouse_enabled: bool,
    mouse_sgr: bool,
    history: VecDeque<Vec<GridCell>>,
    /// Scrollback offset in rows. `0` = live tail; positive = N rows
    /// back into history visible at the top.
    scroll_offset: usize,
    /// Cached previous cells used by the shift-detection heuristic that
    /// promotes scrolled-off rows into `history`. Per-pane because the
    /// shifts are pane-local.
    prev_cells: Vec<Vec<GridCell>>,
    /// OSC 0/2 title — `printf '\e]0;hello\a'` from a shell inside this
    /// pane lands here. The active pane's title is applied to the
    /// window chrome.
    title: Option<String>,
    /// Accent color for this pane's header band (RGBA), set via
    /// `surface.set_color`. None = default theme band color.
    color: Option<[u8; 4]>,
    /// Sticky-title flag. Set true only when `surface.rename` is called
    /// (run_job / explicit rename). While pinned, OSC 0/2 titles from the
    /// inner program are ignored so an agent-driven label can't be
    /// clobbered by the shell/TUI re-emitting its own title. Panes that
    /// were never renamed (e.g. the main Claude pane) leave this false so
    /// their dynamic OSC summary keeps flowing through.
    title_pinned: bool,
    /// Frame-dirty flag. Set whenever a PTY update lands new bytes,
    /// the user scrolls, or focus switches; cleared after the next
    /// render. When *every* pane is clean and no chrome-level anim
    /// is pending, the render loop skips the GPU pass entirely —
    /// matches Rio's `TerminalDamage::Noop` short-circuit.
    dirty: bool,
    /// Last OSC 133 `B` mark (prompt end / command-input start) seen on
    /// this pane, as (row, col). Set from the pty backend; the host reads
    /// the editable command line from here to the cursor for inline
    /// autosuggestion. Only trusted while it's still on the cursor's row
    /// (see `update_suggestion`); a new prompt overwrites it.
    prompt_end: Option<(u16, u16)>,
    /// Inline images (iTerm2 OSC 1337) visible in this pane right now,
    /// already mapped to viewport cells by the backend for the current
    /// scroll position. Replaced wholesale every frame, so an image that
    /// scrolls out simply stops being listed.
    images: Vec<tmux_bridge::screen::ImagePlacement>,
}

/// Whole-window state: HashMap of panes keyed by tmux pane id, the
/// most recently parsed Layout tree, and which pane is active for
/// keyboard / selection / cursor display.
#[derive(Default)]
struct Workspace {
    panes: HashMap<String, PaneState>,
    layout: Option<Layout>,
    active_pane: Option<String>,
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
    sugarloaf: Option<Sugarloaf<'static>>,
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
    /// In-flight header drag-and-drop: which pane the user grabbed by its
    /// header, the press position, and whether the cursor has moved past
    /// the threshold (only then does releasing relocate, so a plain click
    /// still just focuses the pane).
    header_drag: Option<HeaderDrag>,
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
    /// Cached value of the OS window title — `window.set_title` is
    /// cheap but not free, so we only call it when the resolved
    /// label actually changes.
    last_window_title: Option<String>,
    /// Deadline keeping the Claude "busy" anim alive after the
    /// spinner row briefly disappears from the grid. Without this,
    /// fast redraws toggle between "✱ claude" and the live status
    /// every frame because Claude Code repaints the spinner phase
    /// across separate cells. 800ms of stickiness smooths it out.
    claude_busy_until: Option<Instant>,
    /// Most recent claude status line we lifted from the grid. Kept
    /// so the titlebar stays on the last "✻ Brewed for Ns" frame
    /// while Claude Code is mid-repaint and the marker row briefly
    /// vanishes. Cleared when the busy window expires.
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
            sugarloaf: None,
            gpu: None,
            tmux: None,
            pty: HashMap::new(),
            pty_layout: None,
            next_pane_id: 1, // %0 is the initial pane created in start_pty
            pending_restores: Vec::new(),
            autoquit_at: None,
            autosplit_plan: Vec::new(),
            autosplit_at: None,
            autowindow_left: 0,
            autowindow_at: None,
            autotoggle_sidebar_at: None,
            autotoggle_left: 0,
            dead_panes: Arc::new(Mutex::new(Vec::new())),
            socket_handle: None,
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
            hangul: hangul_ime::Composer::new(),
            pane_header_rects: Vec::new(),
            copy_btn_rects: Vec::new(),
            copy_toast_at: None,
            window_tab_rects: Vec::new(),
            window_tab_close_rects: Vec::new(),
            new_window_btn_rect: None,
            window_labels: Vec::new(),
            window_labels_at: None,
            selection: None,
            drag_anchor: None,
            resize_drag: None,
            header_drag: None,
            mouse_forward_pane: None,
            last_left_click: None,
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
            proxy,
            git_panel_window: None,
            git_panel_webview: None,
            session_panel_window: None,
            session_panel_webview: None,
            version_anim_start: Instant::now(),
            menu: None,
            git_menu_item: None,
            session_menu_item: None,
            autosuggest: autosuggest::History::new(),
            input_buf: String::new(),
            current_suggestion: None,
            sidebar_visible: true,
        }
    }

    /// Sidebar width that layout math should actually use: the full
    /// `SIDEBAR_W` when the strip is shown, 0 when collapsed. Every
    /// origin_x / window_cells / hit-test calc routes through here so a
    /// single `sidebar_visible` flip reflows the whole grid.
    fn effective_sidebar_w(&self) -> f32 {
        if self.sidebar_visible {
            SIDEBAR_W
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
            match ws.active() {
                Some(pane) if !pane.alt_screen => {
                    let crow = pane.cursor_row as usize;
                    let ccol = pane.cursor_col as usize;
                    let row_cells = pane.cells.get(crow);
                    let cell_str = |r: &[GridCell], from: usize, to: usize| -> String {
                        r.iter()
                            .take(to)
                            .skip(from)
                            .map(|c| if c.ch.is_empty() { " " } else { c.ch.as_str() })
                            .collect()
                    };
                    // Primary: OSC 133 mark still on the cursor's row.
                    let from_mark = match pane.prompt_end {
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

    /// Physical pixels per cell, for `PtyOptions.cell_px` (inline-image
    /// span math in the pty backend). Derived from the live font metrics ×
    /// the window scale, with a Retina-ish fallback before the window exists.
    fn host_cell_px(&self) -> (u16, u16) {
        let scale = self
            .window
            .as_ref()
            .map(|w| w.scale_factor() as f32)
            .unwrap_or(2.0);
        let w = (self.cell.w * scale).round().max(1.0) as u16;
        let h = (self.cell.h * scale).round().max(1.0) as u16;
        (w, h)
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
        if let (Some(window), Some(sugarloaf)) = (self.window.as_ref(), self.sugarloaf.as_ref()) {
            let scale = window.scale_factor() as f32;
            let (_dim, metrics) =
                sugarloaf.compute_cell_metrics(new, LINE_HEIGHT_MULT, scale);
            self.cell = CellGeom {
                w: (metrics.cell_width as f32) / scale,
                h: (metrics.cell_height as f32) / scale,
                baseline: 0.0,
            };
            let (cols, rows) = self.window_cells();
            self.resize_backend(cols, rows);
            window.request_redraw();
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
                // Force our window to the front, then capture just its
                // region. Full-desktop screencapture grabs whatever app
                // is on top — useless in headless verify runs where
                // another app may have focus. Window-bounded capture
                // sidesteps that.
                let _ = std::process::Command::new("osascript")
                    .args([
                        "-e",
                        &format!(
                            "tell application \"System Events\" to set frontmost of (first process whose unix id is {pid}) to true"
                        ),
                    ])
                    .status();
                std::thread::sleep(std::time::Duration::from_millis(400));
                let bounds_script = format!(
                    "tell application \"System Events\" to tell (first process whose unix id is {pid}) to get {{position, size}} of window 1"
                );
                let bounds_out = std::process::Command::new("osascript")
                    .args(["-e", &bounds_script])
                    .output();
                let region = bounds_out.ok().and_then(|o| {
                    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                    let parts: Vec<i32> = s
                        .split(',')
                        .filter_map(|p| p.trim().parse::<i32>().ok())
                        .collect();
                    if parts.len() == 4 {
                        Some(format!("{},{},{},{}", parts[0], parts[1], parts[2], parts[3]))
                    } else {
                        None
                    }
                });
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
            while let Ok(update) = screens.recv() {
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
                let mut ws = ws_screens.lock().unwrap();
                if ws.active_pane.is_none() {
                    ws.active_pane = Some(update.pane_id.clone());
                }
                let pane = ws.pane_mut(&update.pane_id);
                let resized = pane.cols != update.cols
                    || pane.rows != update.rows
                    || pane.cells.len() != update.rows as usize;
                if resized {
                    pane.cols = update.cols;
                    pane.rows = update.rows;
                    pane.cells = (0..update.rows as usize)
                        .map(|_| vec![GridCell::blank(); update.cols as usize])
                        .collect();
                    pane.prev_cells.clear();
                }
                for (r, row) in update.dirty {
                    if let Some(dst) = pane.cells.get_mut(r as usize) {
                        *dst = row;
                    }
                }
                // Shift detection on the pty side is retired — alacritty handles
                // scrollback natively via display_offset. Hand-rolled detection
                // breaks scroll-region TUIs (like Claude Code) when they write to sync.
                pane.cursor_row = update.cursor_row;
                pane.cursor_col = update.cursor_col;
                pane.cursor_visible = update.cursor_visible;
                pane.alt_screen = update.alt_screen;
                pane.mouse_enabled = update.mouse_enabled;
                pane.mouse_sgr = update.mouse_sgr;
                // Carry the OSC 133 prompt-end mark only on frames that
                // actually emitted one; keep the last otherwise so a
                // mid-typing frame doesn't erase it.
                if let Some(pe) = update.prompt_end {
                    pane.prompt_end = Some(pe);
                }
                // Inline images: the backend recomputes the full visible
                // set every snapshot (already scroll-mapped), so replace
                // wholesale — an empty list means everything scrolled out.
                pane.images = update.images;
                // OSC 0/2 title from the inner program (Claude Code's
                // conversation summary, vim filename, etc.). Carry it
                // through to PaneState so the chrome header + the
                // macOS window title see the freshest value.
                // Pinned panes (renamed via surface.rename / run_job) keep
                // their agent-set label; only unpinned panes track OSC.
                if let Some(t) = update.title.clone() {
                    if !pane.title_pinned {
                        pane.title = Some(t);
                    }
                }
                // Mark this pane dirty so the next render frame
                // actually emits cells. render_frame short-circuits
                // when every pane is clean, which is what makes
                // wheel-scroll feel smooth during Claude Code
                // streaming bursts: the PTY thread keeps pushing
                // updates but the GPU only redraws once per 16ms.
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
            shell: resolve_default_shell(),
            cwd,
            cols,
            rows,
            env: Vec::new(),
            pane_id: id.clone(),
            cell_px: self.host_cell_px(),
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
        if let Some(first) = self
            .pty_layout
            .as_ref()
            .and_then(|l| l.leaves().first().map(|s| s.to_string()))
        {
            self.ws.lock().unwrap().active_pane = Some(first);
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        self.refresh_socket_snapshot();
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
        let tab_w = SIDEBAR_W - 2.0 * SIDEBAR_TAB_INSET;
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
        // A3 restore: rebuild the saved session(s) — full layout tree, each
        // pane's cwd, and a queued `claude --resume` for panes that were on
        // claude. Falls back to a fresh single pane when there's nothing saved.
        match socket::load_session_state() {
            Some(state) if !state.sessions.is_empty() => {
                self.restore_sessions(state, cols, rows)?;
            }
            _ => {
                let cwd = resolve_initial_cwd();
                let session = pty_backend::PtySession::start(pty_backend::PtyOptions {
                    shell: resolve_default_shell(),
                    cwd,
                    cols,
                    rows,
                    env: Vec::new(),
                    pane_id: "%0".to_string(),
                    cell_px: self.host_cell_px(),
                })?;
                self.pump_pty_screens(session.screens.clone(), "%0".to_string());
                self.pty.insert("%0".to_string(), Arc::new(session));
                self.pty_layout = Some(pty_backend::PtyLayout::single("%0"));
                // Seed active_pane immediately so split / focus shortcuts work
                // before the first ScreenUpdate lands. pump_pty_screens won't
                // overwrite a non-None active_pane.
                self.ws.lock().unwrap().active_pane = Some("%0".to_string());
            }
        }
        // Bring up the cmux-compat socket *after* the initial pane(s) are
        // wired so the very first surface.list call sees them.
        self.start_socket_pty();
        Ok(())
    }

    /// Rebuild every saved session (A3 restore). Each session's panes are
    /// spawned into a fresh workspace and laid out per the saved BSP tree;
    /// claude panes get a queued `--resume`. Sessions are built into stashed
    /// slots, then the saved active session is swapped into the live fields —
    /// mirroring the stash-swap invariant new_session/switch_session use.
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
                    cell_px: self.host_cell_px(),
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
            let (pty, active_layout, windows, active_window) = if i == self.active_session {
                (
                    &self.pty,
                    self.pty_layout.as_ref(),
                    &self.windows,
                    self.active_window,
                )
            } else {
                match self.sessions[i].as_ref() {
                    Some(s) => (&s.pty, s.pty_layout.as_ref(), &s.windows, s.active_window),
                    None => continue,
                }
            };
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
                windows_json.push(Self::layout_to_json(layout, pty));
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
    ) -> serde_json::Value {
        match layout {
            pty_backend::PtyLayout::Leaf { pane_id } => {
                let rec = pty
                    .get(pane_id)
                    .map(|s| socket::pane_record(s))
                    .unwrap_or(serde_json::Value::Null);
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
                    "a": Self::layout_to_json(a, pty),
                    "b": Self::layout_to_json(b, pty),
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
                let resized = pane.cols != cols
                    || pane.rows != rows
                    || pane.cells.len() != rows as usize;
                if resized {
                    pane.cols = cols;
                    pane.rows = rows;
                    pane.cells = (0..rows as usize)
                        .map(|_| vec![GridCell::blank(); cols as usize])
                        .collect();
                    pane.prev_cells.clear();
                }
                for (r, row) in dirty {
                    if let Some(dst) = pane.cells.get_mut(r as usize) {
                        *dst = row;
                    }
                }
                // Shift detection per pane — alt-screen apps manage their
                // own scrollback so we skip there.
                if !alt_screen
                    && !pane.prev_cells.is_empty()
                    && pane.prev_cells.len() == pane.cells.len()
                {
                    let n = pane.prev_cells.len();
                    let mut shifted = 0usize;
                    for k in 1..n {
                        if pane.prev_cells[k..] == pane.cells[..n - k] {
                            shifted = k;
                            break;
                        }
                    }
                    if shifted > 0 {
                        for row in &pane.prev_cells[..shifted] {
                            pane.history.push_back(row.clone());
                        }
                        while pane.history.len() > SCROLLBACK_MAX {
                            pane.history.pop_front();
                        }
                    }
                }
                pane.prev_cells = pane.cells.clone();
                pane.cursor_row = cursor_row;
                pane.cursor_col = cursor_col;
                pane.cursor_visible = cursor_visible;
                pane.alt_screen = alt_screen;
                pane.mouse_enabled = mouse_enabled;
                pane.mouse_sgr = mouse_sgr;
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
    }

    /// Drain pending socket commands and run them on the main thread.
    /// Called once per loop turn from `about_to_wait`.
    fn drain_socket_inbox(&mut self) {
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
                    if self.pty.contains_key(&pane_id) {
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
                socket::PtyCommand::NewSession { reply } => {
                    self.new_session();
                    let _ = reply.send(Ok(()));
                }
                socket::PtyCommand::CloseSession { idx, reply } => {
                    let res = self.close_session(idx);
                    let _ = reply.send(res);
                }
            }
        }
        self.refresh_socket_snapshot();
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
                        let (mc, mr) = ws.panes.get(&pid).map_or((lc, lr), |p| {
                            (
                                lc.min(p.cols.saturating_sub(1)),
                                lr.min(p.rows.saturating_sub(1)),
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
        if pane.cols == 0 || pane.rows == 0 {
            return None;
        }
        let lc =
            ((px - sb - WINDOW_PADDING - PANE_INNER_X).max(0.0) / self.cell.w).floor() as u16;
        let lr = ((py - TITLE_HEIGHT - PANE_INNER_Y).max(0.0) / self.cell.h).floor() as u16;
        Some((id, lc.min(pane.cols - 1), lr.min(pane.rows - 1)))
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
    fn active_pty(&self) -> Option<&Arc<pty_backend::PtySession>> {
        let id = self.ws.lock().unwrap().active_pane.clone()?;
        self.pty.get(&id)
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
        let scale = window.scale_factor() as f32;
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
        let header_cells: u16 = if leaves > 1 {
            ((PANE_HEADER_HEIGHT / self.cell.h.max(1.0)).ceil() as u16).max(1)
        } else {
            0
        };
        // Inset eats a couple of cells per axis so the grid fits inside
        // the padded box. Done in cells (ceil) here to match the px inset
        // the render origin applies — a hair of slack is fine, it just
        // lands as extra trailing margin.
        let inset_x_cells = (2.0 * PANE_INNER_X / self.cell.w.max(1.0)).ceil() as u16;
        let inset_y_cells = (2.0 * PANE_INNER_Y / self.cell.h.max(1.0)).ceil() as u16;
        for (id, _x, _y, w, h) in tree.leaf_rects(cols, rows) {
            if let Some(sess) = self.pty.get(&id) {
                let pcols = w.saturating_sub(inset_x_cells).max(1);
                let prows = h.saturating_sub(header_cells + inset_y_cells).max(1);
                let _ = sess.resize(pcols, prows);
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
            cell_px: self.host_cell_px(),
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
        {
            let mut ws = self.ws.lock().unwrap();
            ws.panes.remove(target);
            if was_active {
                ws.active_pane = next_focus;
            }
        }
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
        let (cols, rows) = self.window_cells();
        let rects = tree.leaf_rects(cols, rows);
        let pad = WINDOW_PADDING + self.effective_sidebar_w();
        for (id, cx, cy, cw, ch) in rects {
            let bx = pad + cx as f32 * self.cell.w;
            let by = TITLE_HEIGHT + cy as f32 * self.cell.h;
            let bw = (cw as f32 * self.cell.w).max(1.0);
            let bh = (ch as f32 * self.cell.h).max(1.0);
            if x >= bx && x <= bx + bw && y >= by && y <= by + bh {
                let dx = (x - bx) / bw - 0.5;
                let dy = (y - by) / bh - 0.5;
                let zone = if dx.abs() > dy.abs() {
                    if dx < 0.0 { DropZone::Left } else { DropZone::Right }
                } else if dy < 0.0 {
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
            .map(|p| p.mouse_enabled && p.mouse_sgr)
            .unwrap_or(false)
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
        } else if let Some(pty) = self.pty.get(pane_id) {
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
    fn active_claude_status(&self) -> Option<String> {
        let ws = self.ws.lock().unwrap();
        let id = ws.active_pane.as_deref()?;
        let pane = ws.panes.get(id)?;
        let rows = pane.cells.len();
        let start = rows.saturating_sub(10);
        for row in pane.cells[start..].iter() {
            let mut text = String::new();
            let mut has_marker = false;
            for cell in row {
                if cell.ch.is_empty() {
                    text.push(' ');
                } else {
                    text.push_str(&cell.ch);
                    if let Some(c) = cell.ch.chars().next() {
                        let cp = c as u32;
                        if (0x2731..=0x274F).contains(&cp) {
                            has_marker = true;
                        }
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

    fn active_spinner_glyph(&self) -> Option<char> {
        let ws = self.ws.lock().unwrap();
        let id = ws.active_pane.as_deref()?;
        let pane = ws.panes.get(id)?;
        let rows = pane.cells.len();
        let start = rows.saturating_sub(8);
        for row in &pane.cells[start..] {
            for cell in row {
                if let Some(c) = cell.ch.chars().next() {
                    let cp = c as u32;
                    // Braille spinners (npm, pure-prompt, etc.) +
                    // Dingbats asterisks/stars (Claude Code uses
                    // ✻/✶/✷/✸/✹/✺ as its "thinking" indicator).
                    if (0x2800..=0x28FF).contains(&cp)
                        || (0x2731..=0x274F).contains(&cp)
                    {
                        return Some(c);
                    }
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
        let mut label = Self::resolve_pane_label(&self.pty, &id, osc.as_deref());
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
            match ws.active() {
                Some(p) => p.cells.clone(),
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
        let dy_cells = match delta {
            MouseScrollDelta::LineDelta(_, y) => y * 0.3,
            MouseScrollDelta::PixelDelta(p) => (p.y as f32) / self.cell.h.max(1.0) * 0.3,
        };
        let lines = match wheel_step(
            &mut self.wheel_accum_y,
            dy_cells,
            &mut self.last_wheel_emit,
            Instant::now(),
        ) {
            Some(l) => l,
            None => return,
        };
        // Decide which pane handles this wheel: the pane the pointer is
        // hovering over. Falls back to the active pane if the pointer
        // is in a gutter. Multi-pane lets the user scroll inside any
        // pane regardless of which one currently has keyboard focus.
        let target_pane_id = self
            .px_to_pane_cell(self.cursor_px.0, self.cursor_px.1)
            .map(|(id, _, _)| id)
            .or_else(|| self.target_pane());
        let (alt, hist_len, mouse_on, mouse_sgr) = {
            let ws = self.ws.lock().unwrap();
            let pane = target_pane_id
                .as_deref()
                .and_then(|id| ws.panes.get(id));
            match pane {
                Some(p) => (p.alt_screen, p.history.len(), p.mouse_enabled, p.mouse_sgr),
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
                if let Some(pty) = self.pty.get(id) {
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
                if let Some(pty) = self.pty.get(id) {
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
                        let s = step as usize;
                        if lines > 0 {
                            pane.scroll_offset = (pane.scroll_offset + s).min(hist_len);
                        } else {
                            pane.scroll_offset = pane.scroll_offset.saturating_sub(s);
                        }
                        pane.dirty = true;
                    }
                }
            } else if let Some(pty) = self.pty.get(id) {
                // Positive `lines` = scroll up = toward older history.
                pty.scroll(if lines > 0 { step } else { -step });
            }
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
        // Typing snaps the active pane back to live tail. Other panes'
        // scroll offsets are left alone — switching focus by clicking
        // doesn't disturb where the user was reading.
        if let Ok(mut ws) = self.ws.lock() {
            if let Some(pane) = ws.active_mut() {
                if pane.scroll_offset != 0 {
                    pane.scroll_offset = 0;
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
                    // Font zoom: host_mod + = (or Shift = +) increases,
                    // host_mod + - (or Shift = _) decreases. `0` resets
                    // to the default. Layout the same as VS Code,
                    // Windows Terminal, and most browsers.
                    if code == KeyCode::Equal || code == KeyCode::NumpadAdd {
                        self.change_font_size(1.0);
                        return;
                    }
                    if code == KeyCode::Minus || code == KeyCode::NumpadSubtract {
                        self.change_font_size(-1.0);
                        return;
                    }
                    if code == KeyCode::Digit0 || code == KeyCode::Numpad0 {
                        self.change_font_size(FONT_SIZE - self.font_size);
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
            // Shift+Enter → ESC+CR, which claude code / Ink reads as a
            // newline instead of submitting. Plain Enter stays \r.
            // Terminals can't distinguish the two by default (both send
            // \r), so we encode the modifier ourselves.
            Key::Named(NamedKey::Enter) => {
                if self.modifiers.shift_key() {
                    b"\x1b\r".to_vec()
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
                    format!("\x1b[{letter}").into_bytes()
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
                                                .map(|p| (p.cursor_row, p.cursor_col))
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
                    let (base_row, base_col) = (pane.cursor_row, pane.cursor_col);
                    // Until the committed syllable's echo lands (cursor
                    // still where it was at commit time), draw the
                    // committed text in front of the preedit at that
                    // spot so "ㄴ" never shows alone on the "안" cell.
                    let (display, prow, pcol) = match &commit_overlay {
                        Some((ctext, before))
                            if *before == (pane.cursor_row, pane.cursor_col) =>
                        {
                            (format!("{ctext}{preedit_text}"), before.0, before.1)
                        }
                        _ => (preedit_text.clone(), base_row, base_col),
                    };
                    (
                        pane.cursor_row,
                        pane.cursor_col,
                        pane.cursor_visible,
                        pane.cells.first().map(|r| r.len()).unwrap_or(80) as u16,
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
        // When split, every pane body is pushed down by its header band.
        // The cursor / preedit / selection overlays anchor off the same
        // origin as the cells, so they must apply the identical shift —
        // otherwise the cursor floats up into the header row.
        let header_shift = if self
            .pty_layout
            .as_ref()
            .is_some_and(|t| t.leaves().len() > 1)
        {
            PANE_HEADER_HEIGHT
        } else {
            0.0
        };
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
            selection: self.selection,
            suggestion: self.current_suggestion.clone().unwrap_or_default(),
        }
    }

    /// Phase 2d overlays — pure free function on the snapshot so it
    /// doesn't fight a mutable borrow on `self.gpu`.
    fn paint_gpu_overlays(g: &mut gpu::GpuRenderer, ov: &GpuOverlay) {
        if ov.cursor_visible && ov.blink_on && ov.preedit.is_empty() {
            let cx = ov.pad_x + ov.cursor_col as f32 * ov.cell_w;
            let cy = ov.pad_y + ov.cursor_row as f32 * ov.cell_h;
            let mut c = cells::ITERM_CURSOR;
            c[3] = 140; // ~0.55 alpha
            g.rect(cx, cy, ov.cell_w, ov.cell_h, c);
        }
        // Inline autosuggestion ghost text — dim, on the same baseline as
        // committed cells, starting at the cursor and clipped to the row's
        // right edge so it never wraps. Drawn only when not composing.
        if ov.preedit.is_empty() && !ov.suggestion.is_empty() {
            let gx = ov.pad_x + ov.cursor_col as f32 * ov.cell_w;
            let gy = ov.pad_y + ov.cursor_row as f32 * ov.cell_h;
            let max_cells = ov.cols.saturating_sub(ov.cursor_col) as u32;
            if max_cells > 0 {
                g.draw_ghost(gx, gy, &ov.suggestion, max_cells);
            }
        }
        if !ov.preedit.is_empty() {
            let px = ov.pad_x + ov.preedit_col as f32 * ov.cell_w;
            let py = ov.pad_y + ov.preedit_row as f32 * ov.cell_h;
            // Route preedit through the cell-grid path so the composing
            // syllable sits on the same baseline as committed text
            // instead of floating above the row.
            g.draw_preedit(px, py, &ov.preedit, cells::ITERM_CURSOR);
        }
        if let Some(sel) = ov.selection {
            let (start, stop) = if (sel.anchor.1, sel.anchor.0) <= (sel.end.1, sel.end.0) {
                (sel.anchor, sel.end)
            } else {
                (sel.end, sel.anchor)
            };
            let color = cells::ITERM_SELECTION;
            if start.1 == stop.1 {
                let x = ov.pad_x + start.0 as f32 * ov.cell_w;
                let y = ov.pad_y + start.1 as f32 * ov.cell_h;
                let w = (stop.0 - start.0 + 1) as f32 * ov.cell_w;
                g.rect(x, y, w, ov.cell_h, color);
            } else {
                let x = ov.pad_x + start.0 as f32 * ov.cell_w;
                let y = ov.pad_y + start.1 as f32 * ov.cell_h;
                let row_w = (ov.cols - start.0) as f32 * ov.cell_w;
                g.rect(x, y, row_w, ov.cell_h, color);
                for r in (start.1 + 1)..stop.1 {
                    let yy = ov.pad_y + r as f32 * ov.cell_h;
                    g.rect(ov.pad_x, yy, ov.cols as f32 * ov.cell_w, ov.cell_h, color);
                }
                let yy = ov.pad_y + stop.1 as f32 * ov.cell_h;
                let last_w = (stop.0 + 1) as f32 * ov.cell_w;
                g.rect(ov.pad_x, yy, last_w, ov.cell_h, color);
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
        }
        // Header chrome carried in LOGICAL px — gpu.rect/draw_text
        // promote to physical internally, matching the cell pass.
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
        }
        // Captured once so the &mut self.gpu block below (which can't
        // re-borrow &self) can still see the collapsed/expanded width.
        let sidebar_w = self.effective_sidebar_w();
        let pad_px = (WINDOW_PADDING + sidebar_w) * scale;
        let title_px = TITLE_HEIGHT * scale;
        // Code-block copy buttons (text + logical rect), filled per pane in
        // the loop below and handed to both the mouse handler and overlay.
        let mut copy_btns: Vec<(String, (f32, f32, f32, f32))> = Vec::new();
        // Inline images to draw this frame, in PHYSICAL pixels, already
        // clipped to each pane's body. Filled inside the slot loop below
        // (which holds the workspace lock) and handed to the GPU after the
        // cell pass. Each keeps an Arc to the decoded pixels so the quad
        // can borrow them without copying.
        struct CollectedImage {
            id: u64,
            image: std::sync::Arc<tmux_bridge::screen::DecodedImage>,
            px: [f32; 4],
            uv_min: [f32; 2],
            uv_max: [f32; 2],
        }
        let mut collected_images: Vec<CollectedImage> = Vec::new();
        let (slots, headers): (Vec<PaneSlot>, Vec<HeaderInfo>) = {
            let ws = self.ws.lock().unwrap();
            let active_id = ws.active_pane.clone();
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
                match ws.panes.iter().next() {
                    Some((id, _)) => vec![(id.clone(), 0, 0, 0, 0)],
                    None => Vec::new(),
                }
            };
            // Header bar only when split — a lone pane stays header-less
            // so the first session reads as a plain terminal.
            let show_headers = leaves.len() > 1;
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
                let cols_now = pane.cols.max(1) as usize;
                let normalise = |row: &Vec<GridCell>| -> Vec<GridCell> {
                    let mut r = row.clone();
                    if r.len() < cols_now {
                        r.resize(cols_now, GridCell::blank());
                    } else if r.len() > cols_now {
                        r.truncate(cols_now);
                    }
                    r
                };
                let composed: Vec<Vec<GridCell>> =
                    pane.cells.iter().map(normalise).collect();
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
                slots.push(PaneSlot {
                    rows: composed,
                    origin_px,
                });
                // Inline images for this pane. The backend already mapped
                // each to a viewport cell row/col (scroll-aware); here we
                // turn that into a physical-pixel rect and clip it to the
                // pane's body so a partly-scrolled or oversized image is
                // cropped against the cell area instead of bleeding over
                // the header / window chrome.
                if !pane.images.is_empty() {
                    let body_x0 = origin_px.0;
                    let body_y0 = origin_px.1;
                    let body_x1 = origin_px.0 + cols_now as f32 * cell_w_px;
                    let body_y1 = origin_px.1 + pane.rows as f32 * cell_h_px;
                    for img in &pane.images {
                        let ix0 = origin_px.0 + img.col as f32 * cell_w_px;
                        let iy0 = origin_px.1 + img.row as f32 * cell_h_px;
                        let iw = img.cols as f32 * cell_w_px;
                        let ih = img.rows as f32 * cell_h_px;
                        if iw <= 0.0 || ih <= 0.0 {
                            continue;
                        }
                        let cx0 = ix0.max(body_x0);
                        let cy0 = iy0.max(body_y0);
                        let cx1 = (ix0 + iw).min(body_x1);
                        let cy1 = (iy0 + ih).min(body_y1);
                        if cx1 <= cx0 || cy1 <= cy0 {
                            continue;
                        }
                        collected_images.push(CollectedImage {
                            id: img.id,
                            image: img.image.clone(),
                            px: [cx0, cy0, cx1 - cx0, cy1 - cy0],
                            uv_min: [(cx0 - ix0) / iw, (cy0 - iy0) / ih],
                            uv_max: [(cx1 - ix0) / iw, (cy1 - iy0) / ih],
                        });
                    }
                }
                if show_headers {
                    // Custom title (rename / OSC) wins; otherwise show the
                    // live foreground process (vim, claude, zsh …); only
                    // fall back to the raw "%N" pane id if both are empty.
                    let proc_name = self
                        .pty
                        .get(&id)
                        .and_then(|p| p.active_process_name())
                        .filter(|t| !t.is_empty());
                    let label = pane
                        .title
                        .clone()
                        .filter(|t| !t.is_empty())
                        .or(proc_name)
                        .unwrap_or_else(|| id.clone());
                    headers.push(HeaderInfo {
                        id: id.clone(),
                        x: WINDOW_PADDING + sidebar_w + x_cells as f32 * self.cell.w,
                        y: TITLE_HEIGHT + y_cells as f32 * self.cell.h,
                        w: w_cells as f32 * self.cell.w,
                        box_h: h_cells as f32 * self.cell.h,
                        label,
                        is_active: active_id.as_deref() == Some(id.as_str()),
                        color: pane.color,
                    });
                }
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
        self.pane_header_rects = headers
            .iter()
            .map(|h| {
                let close = (
                    h.x + 6.0,
                    h.y + (PANE_HEADER_HEIGHT - close_size) / 2.0,
                    close_size,
                    close_size,
                );
                (h.id.clone(), close)
            })
            .collect();
        // Session tabs live in a wry webview panel (like the git panel), not
        // the native title bar — drawing them here collided with the OSC title.
        // Drop-zone overlay: while a header drag is active, highlight the
        // half of the target pane the dragged pane would land in. Computed
        // here (immutable self borrow) so the gpu block below only touches
        // the cached rect.
        let drop_zone_rect: Option<(f32, f32, f32, f32)> = self
            .header_drag
            .as_ref()
            .filter(|hd| hd.active)
            .and_then(|_| self.drop_target_at(self.cursor_px.0, self.cursor_px.1))
            .and_then(|(target, zone)| {
                let tree = self.pty_layout.as_ref()?;
                let (cols, rows) = self.window_cells();
                let pad = WINDOW_PADDING + self.effective_sidebar_w();
                let (_, cx, cy, cw, ch) = tree
                    .leaf_rects(cols, rows)
                    .into_iter()
                    .find(|(id, ..)| *id == target)?;
                let bx = pad + cx as f32 * self.cell.w;
                let by = TITLE_HEIGHT + cy as f32 * self.cell.h;
                let bw = cw as f32 * self.cell.w;
                let bh = ch as f32 * self.cell.h;
                Some(match zone {
                    DropZone::Left => (bx, by, bw / 2.0, bh),
                    DropZone::Right => (bx + bw / 2.0, by, bw / 2.0, bh),
                    DropZone::Up => (bx, by, bw, bh / 2.0),
                    DropZone::Down => (bx, by + bh / 2.0, bw, bh / 2.0),
                })
            });
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
        if let Some(g) = self.gpu.as_mut() {
            g.clear_chrome();
            g.draw_cells(&slot_views);
            // Stage inline images (drawn on top of the cell grid in
            // `render`). Called unconditionally — an empty slice clears
            // any image that just scrolled out.
            let image_quads: Vec<gpu::ImageQuad<'_>> = collected_images
                .iter()
                .map(|c| gpu::ImageQuad {
                    id: c.id,
                    rgba: &c.image.rgba,
                    width: c.image.width,
                    height: c.image.height,
                    px: c.px,
                    uv_min: c.uv_min,
                    uv_max: c.uv_max,
                })
                .collect();
            g.set_images(&image_quads);
            // Title strip fill: chrome surface across the top so the area above
            // the sidebar / traffic lights matches the sidebar instead of
            // showing the terminal body color.
            g.rect(0.0, 0.0, win_px.0 / scale, TITLE_HEIGHT, theme::SURFACE);
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
            // Window-tab sidebar, Warp-style. Painted first so per-pane
            // headers / rings layer on top at the seam.
            if sidebar_w > 0.0 {
                // Strip background, below the title strip to the bottom.
                g.rect(
                    0.0,
                    TITLE_HEIGHT,
                    sidebar_w,
                    (sb_win_h - TITLE_HEIGHT).max(0.0),
                    theme::SURFACE,
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
                    g.draw_text(
                        icon_x + 9.0,
                        icon_y + 7.0,
                        tab_icon_glyph(&name),
                        gpu::DrawOpts {
                            font_size: 14.0,
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
                    let name_max = if show_close { 15 } else { 18 };
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
                            &clip(&cwd, 21),
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
                            g.draw_text(
                                *cx + (*cw - 11.0) / 2.0,
                                *cy + (*ch - 11.0) / 2.0,
                                "x",
                                gpu::DrawOpts {
                                    font_size: 11.0,
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
                g.draw_text(
                    px + pw / 2.0 - 5.0,
                    py + (ph - 17.0) / 2.0,
                    "+",
                    gpu::DrawOpts {
                        font_size: 18.0,
                        color: theme::TEXT_MUTE,
                        bold: false,
                        italic: false,
                    },
                );
            }
            // Per-pane header bar: band + bottom hairline + × close
            // glyph + title. Active pane gets a brighter band and bold
            // title, matching the sugarloaf path's iTerm-style chrome.
            for h in &headers {
                let bg = match h.color {
                    Some(c) => c,
                    None if h.is_active => theme::SURFACE_ACTIVE,
                    None => theme::SURFACE,
                };
                g.rect(h.x, h.y, h.w, PANE_HEADER_HEIGHT, bg);
                g.rect(
                    h.x,
                    h.y + PANE_HEADER_HEIGHT - 1.0,
                    h.w,
                    1.0,
                    theme::BORDER,
                );
                let fg: [u8; 4] = if h.is_active {
                    theme::TEXT
                } else {
                    theme::TEXT_DIM
                };
                let text_y = h.y + (PANE_HEADER_HEIGHT - chrome_font) / 2.0;
                g.draw_text(
                    h.x + 6.0,
                    text_y,
                    "x",
                    gpu::DrawOpts {
                        font_size: close_size,
                        color: fg,
                        bold: false,
                        italic: false,
                    },
                );
                g.draw_text(
                    h.x + 6.0 + close_size + 8.0,
                    text_y,
                    &h.label,
                    gpu::DrawOpts {
                        font_size: chrome_font,
                        color: fg,
                        bold: h.is_active,
                        italic: false,
                    },
                );
            }
            // Focus by contrast (Warp / iTerm style): no accent ring.
            // Every inactive pane gets a dark translucent veil so the
            // active pane is the only fully-lit one. Single un-split panes
            // have no headers, so this loop is empty and nothing dims.
            for h in headers.iter().filter(|h| !h.is_active) {
                g.rect(h.x, h.y, h.w, h.box_h, [0, 0, 0, 0x55]);
            }
            // 1px hairline divider on every pane box edge so abutting
            // panes read as separate tiles. Drawn after the veil so the
            // seam stays crisp on top.
            for h in &headers {
                g.rect(h.x, h.y, h.w, 1.0, theme::BORDER);
                g.rect(h.x, h.y + h.box_h - 1.0, h.w, 1.0, theme::BORDER);
                g.rect(h.x, h.y, 1.0, h.box_h, theme::BORDER);
                g.rect(h.x + h.w - 1.0, h.y, 1.0, h.box_h, theme::BORDER);
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
                    ws.panes.get(&id).map(|p| (p.cursor_row, p.cursor_col))
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
        let Some(window) = self.window.as_ref() else { return; };
        let scale = window.scale_factor() as f32;
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
        // Captured before the long-lived &mut self.sugarloaf borrow below.
        let sidebar_w = self.effective_sidebar_w();
        let Some(sugarloaf) = self.sugarloaf.as_mut() else { return; };
        let size = window.inner_size();
        let win_w = size.width as f32 / scale;
        let win_h = size.height as f32 / scale;
        sugarloaf.rect(
            None,
            0.0,
            0.0,
            win_w,
            win_h,
            [
                cells::DEFAULT_BG[0] as f32 / 255.0,
                cells::DEFAULT_BG[1] as f32 / 255.0,
                cells::DEFAULT_BG[2] as f32 / 255.0,
                1.0,
            ],
            0.0,
            0,
        );
        // Snapshot the per-pane render data under one lock so the
        // sugarloaf draw calls below can run without re-locking. Each
        // entry carries the pane's resolved rect (in cells), the cell
        // grid we'll actually paint (history + live composed), and the
        // cursor / title info the renderer reads.
        struct PaneFrame {
            id: String,
            x_cells: u16,
            y_cells: u16,
            w_cells: u16,
            h_cells: u16,
            rows: Vec<Vec<GridCell>>,
            cursor_row: u16,
            cursor_col: u16,
            cursor_visible: bool,
            title: Option<String>,
        }
        // Hold the workspace lock across the entire render so we can
        // `mem::take` each pane's cell grid into the PaneFrame
        // without the PTY thread observing an empty Vec. The lock
        // is released at the end of the function, after we restore
        // the grids — sugarloaf.render() inside the held region
        // pauses the PTY pump for one frame, well below the 16 ms
        // budget.
        let mut ws_guard = self.ws.lock().unwrap();
        let (pane_frames, active_id) = {
            let ws = &mut *ws_guard;
            let active_id = ws.active_pane.clone();
            let leaves: Vec<(String, u16, u16, u16, u16)> =
                if let Some(layout) = ws.layout.as_ref() {
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
                    // No layout yet: fall back to one full-window pane.
                    // First key in the map keeps things deterministic so
                    // the active-pane lookup below points at the same one.
                    match ws.panes.iter().next() {
                        Some((id, p)) => vec![(id.clone(), 0, 0, p.cols, p.rows)],
                        None => Vec::new(),
                    }
                };
            let mut frames = Vec::with_capacity(leaves.len());
            for (id, x, y, w, h) in leaves {
                // Pre-fetch pane metadata under an immutable borrow,
                // then drop it so we can take a mutable borrow to
                // move the cell grid out without cloning. The pump
                // thread can't observe the gap because it would need
                // the same ws lock we already hold.
                let (total, offset, cursor_row, cursor_col, cursor_visible, title) = {
                    let Some(pane) = ws.panes.get(&id) else { continue };
                    (
                        pane.rows.max(1) as usize,
                        pane.scroll_offset.min(pane.history.len()),
                        pane.cursor_row,
                        pane.cursor_col,
                        pane.cursor_visible,
                        pane.title.clone(),
                    )
                };
                let composed: Vec<Vec<GridCell>> = if offset == 0 {
                    // Hot path: move (not clone) the live grid out
                    // for rendering. ~10 000 GridCells used to be
                    // cloned every frame; now it's a Vec pointer
                    // swap. The grid goes back into the PaneState
                    // after the for-loop (`mem::swap` below).
                    std::mem::take(&mut ws.panes.get_mut(&id).unwrap().cells)
                } else {
                    let pane = ws.panes.get(&id).unwrap();
                    let mut out: Vec<Vec<GridCell>> = Vec::with_capacity(total);
                    let hist_start = pane.history.len() - offset;
                    for row in pane.history.iter().skip(hist_start) {
                        out.push(row.clone());
                        if out.len() >= total {
                            break;
                        }
                    }
                    let need = total.saturating_sub(out.len());
                    for row in pane.cells.iter().take(need) {
                        out.push(row.clone());
                    }
                    out
                };
                frames.push(PaneFrame {
                    id,
                    x_cells: x,
                    y_cells: y,
                    w_cells: w,
                    h_cells: h,
                    rows: composed,
                    cursor_row,
                    cursor_col,
                    cursor_visible: offset == 0 && cursor_visible,
                    title,
                });
            }
            (frames, active_id)
        };

        if pane_frames.is_empty() {
            sugarloaf.render();
            return;
        }

        // Resolve every pane's display label up front so the header
        // rendering loop doesn't need to call back into `self`
        // (resolve_pane_label borrows self.pty, which conflicts with
        // the long-lived sugarloaf mutable borrow).
        let pane_labels: Vec<String> = pane_frames
            .iter()
            .map(|f| Self::resolve_pane_label(&self.pty, &f.id, f.title.as_deref()))
            .collect();

        // Origin offset: TITLE_HEIGHT replaces the top padding so the
        // cell grid starts immediately below the custom chrome strip.
        // Add a small breathing margin so the first text row never
        // bleeds into the strip rect on systems where sugarloaf
        // interprets these coordinates slightly differently.
        let origin_x = sidebar_w + WINDOW_PADDING;
        let origin_y = TITLE_HEIGHT + 6.0;

        // Pass 1: walk each pane and render its cell grid at its rect.
        let log_layout = std::env::var_os("KASATERM_LOG_LAYOUT").is_some();
        let show_headers = pane_frames.len() > 1;
        let header_shift = if show_headers { PANE_HEADER_HEIGHT } else { 0.0 };
        for frame in &pane_frames {
            let pane_px_x = origin_x + frame.x_cells as f32 * self.cell.w;
            let pane_px_y =
                origin_y + frame.y_cells as f32 * self.cell.h + header_shift;
            if log_layout {
                let total = frame.rows.len();
                eprintln!(
                    "[render] pane={} rows={total} cols={} px=({pane_px_x:.0},{pane_px_y:.0})",
                    frame.id,
                    frame.rows.first().map(|r| r.len()).unwrap_or(0),
                );
                for (i, row) in frame.rows.iter().enumerate().rev().take(8) {
                    let preview: String = row
                        .iter()
                        .take(80)
                        .map(|c| match c.ch.chars().next() {
                            Some(ch) if !ch.is_whitespace() => ch,
                            _ => '.',
                        })
                        .collect();
                    let nonblank = row
                        .iter()
                        .filter(|c| !c.ch.is_empty() && c.ch != " ")
                        .count();
                    eprintln!("[render]   row[{i:>2}] non={nonblank:>3} {preview}");
                }
            }
            cells::render_screen(
                sugarloaf,
                &frame.rows,
                pane_px_x,
                pane_px_y,
                self.cell.w,
                self.cell.h,
                self.font_size,
                self.cell.baseline,
            );
        }

        // Pass 2: per-pane iTerm-style header bar. Only when the
        // workspace is actually split — a single pane stays
        // header-less so the first session reads as a plain terminal.
        // The header sits *above* the cell grid (cell origin already
        // shifted by `header_shift` in Pass 1), so painting here
        // covers the gap between the pane top and the first text row.
        if show_headers {
            self.pane_header_rects = Vec::with_capacity(pane_frames.len());
            for (idx, frame) in pane_frames.iter().enumerate() {
                let is_active = active_id.as_deref() == Some(frame.id.as_str());
                let pane_px_x = origin_x + frame.x_cells as f32 * self.cell.w;
                let pane_top = origin_y + frame.y_cells as f32 * self.cell.h;
                let pane_px_w = frame.w_cells as f32 * self.cell.w;
                // Same tokens as the gpu path (B) — no separate drift.
                let bg = if is_active {
                    theme::f32_rgba(theme::SURFACE_ACTIVE)
                } else {
                    theme::f32_rgba(theme::SURFACE)
                };
                sugarloaf.rect(
                    None,
                    pane_px_x,
                    pane_top,
                    pane_px_w,
                    PANE_HEADER_HEIGHT,
                    bg,
                    0.0,
                    0,
                );
                // Hairline at the bottom of the header so it reads as
                // a separate band from the cell grid.
                sugarloaf.rect(
                    None,
                    pane_px_x,
                    pane_top + PANE_HEADER_HEIGHT - 1.0,
                    pane_px_w,
                    1.0,
                    theme::f32_rgba(theme::BORDER),
                    0.0,
                    0,
                );
                // Close button + title share the same font size and
                // y baseline so they read as one row of chrome
                // controls. Sugarloaf draws text from the bitmap
                // top-left; we anchor it 8px below the header top so
                // a ~0.85× cell-height glyph sits visually centered
                // in the 28px strip.
                // Match font size between close glyph and title so
                // their bitmap tops sit on the same y. Centering math:
                // a `chrome_font` glyph is ~chrome_font * 1.0 logical
                // tall in this font, so the vertical inset that
                // visually centers it in PANE_HEADER_HEIGHT is
                // (PANE_HEADER_HEIGHT - chrome_font) / 2.
                let chrome_font = 14.0;
                let text_y = pane_top + (PANE_HEADER_HEIGHT - chrome_font) / 2.0;
                let close_size = chrome_font + 4.0;
                let close_rect = (
                    pane_px_x + 6.0,
                    pane_top + (PANE_HEADER_HEIGHT - close_size) / 2.0,
                    close_size,
                    close_size,
                );
                let chrome_color: [u8; 4] = if is_active {
                    theme::TEXT
                } else {
                    theme::TEXT_DIM
                };
                sugarloaf.text_mut().draw(
                    close_rect.0,
                    text_y,
                    "x",
                    &sugarloaf::text::DrawOpts {
                        font_size: chrome_font,
                        color: chrome_color,
                        bold: false,
                        italic: false,
                        font_id: None,
                    },
                );
                let title = pane_labels[idx].clone();
                sugarloaf.text_mut().draw(
                    close_rect.0 + close_rect.2 + 8.0,
                    text_y,
                    &title,
                    &sugarloaf::text::DrawOpts {
                        font_size: chrome_font,
                        color: chrome_color,
                        bold: is_active,
                        italic: false,
                        font_id: None,
                    },
                );
                self.pane_header_rects.push((frame.id.clone(), close_rect));
            }
        } else {
            self.pane_header_rects.clear();
        }

        // Pass 3: selection overlay + cursor block + preedit on the
        // active pane only. Inactive panes show no cursor — matches the
        // tmux / iTerm2 convention where the unfocused split fades its
        // caret.
        let active_frame = active_id
            .as_deref()
            .and_then(|id| pane_frames.iter().find(|f| f.id == id));
        if let Some(frame) = active_frame {
            let pane_px_x = origin_x + frame.x_cells as f32 * self.cell.w;
            let pane_px_y =
                origin_y + frame.y_cells as f32 * self.cell.h + header_shift;
            if let Some(sel) = self.selection {
                cells::render_selection_overlay(
                    sugarloaf,
                    sel.anchor,
                    sel.end,
                    pane_px_x,
                    pane_px_y,
                    self.cell.w,
                    self.cell.h,
                );
            }
            // Skip the cursor rect while preedit is active — the
            // preedit overlay below paints its own opaque background +
            // accent underline, which would be hidden underneath the
            // translucent cursor and produce the "한글 합치는 중에
            // 안 보이는" symptom the user reported.
            if frame.cursor_visible && blink_on && self.preedit.is_empty() {
                let cursor_x = pane_px_x + frame.cursor_col as f32 * self.cell.w;
                let cursor_y = pane_px_y + frame.cursor_row as f32 * self.cell.h;
                sugarloaf.rect(
                    None,
                    cursor_x,
                    cursor_y,
                    self.cell.w,
                    self.cell.h,
                    [
                        cells::ITERM_CURSOR[0] as f32 / 255.0,
                        cells::ITERM_CURSOR[1] as f32 / 255.0,
                        cells::ITERM_CURSOR[2] as f32 / 255.0,
                        0.55,
                    ],
                    0.0,
                    0,
                );
            }
            // Preedit must render regardless of `cursor_visible` —
            // alt-screen TUIs (Claude Code, vim, lazygit, htop) hide
            // the terminal cursor with `\e[?25l` while they draw their
            // own input chrome. Gating on cursor_visible there caused
            // the in-progress Hangul to disappear entirely. The
            // reported cursor row/col still points at the active
            // input position, so use it unconditionally.
            if !self.preedit.is_empty() {
                // Preedit sits exactly on the reported PTY cursor. We
                // used to scan for a prompt sigil and snap to the row's
                // last filled cell, but a TUI's grey placeholder ("Type
                // something") counts as filled and dragged the composing
                // syllable past it to the line's end. The cursor row/col
                // already points at the active input position (incl.
                // trailing spaces the PTY echoes), so trust it directly.
                let (anchor_row, anchor_col) = (frame.cursor_row, frame.cursor_col);
                let px = pane_px_x + anchor_col as f32 * self.cell.w;
                let py = pane_px_y + anchor_row as f32 * self.cell.h;
                if std::env::var_os("KASATERM_IME_DEBUG").is_some() {
                    eprintln!(
                        "[preedit] text={:?} cursor=(row={}, col={}) anchor=(row={anchor_row}, col={anchor_col}) px=({px:.1},{py:.1}) cell=({:.1}x{:.1})",
                        self.preedit, frame.cursor_row, frame.cursor_col, self.cell.w, self.cell.h
                    );
                }
                cells::render_preedit(
                    sugarloaf,
                    &self.preedit,
                    px,
                    py,
                    self.cell.w,
                    self.cell.h,
                    self.font_size,
                    cells::ITERM_CURSOR,
                    self.cell.baseline,
                );
            }
        }

        // Overlay re-pass dropped: with the coordinate-unit fix above
        // (logical pixels everywhere), the strip already paints in the
        // right place on the first pass and doesn't need an overdraw
        // to mask cell-grid bleed.

        let t_emit = t0.elapsed().as_micros();
        sugarloaf.render();
        if trace {
            let t_total = t0.elapsed().as_micros();
            eprintln!(
                "[render] emit={t_emit}us render={t_present}us total={t_total}us frames={n} since_input={si}ms",
                t_present = t_total - t_emit,
                n = pane_frames.len(),
                si = now.saturating_duration_since(self.last_input_at).as_millis(),
            );
        }
        // Move the cell grids back and clear damage flags under the
        // same lock we held throughout the render.
        for frame in pane_frames {
            if let Some(pane) = ws_guard.panes.get_mut(&frame.id) {
                if pane.cells.is_empty() {
                    pane.cells = frame.rows;
                }
            }
        }
        for pane in ws_guard.panes.values_mut() {
            pane.dirty = false;
        }
        drop(ws_guard);
        self.chrome_dirty = false;
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
        // Default = the cell-renderer GPU path (fast, fit-to-cell
        // icons, sRGB-correct). `KASATERM_RENDERER=sugarloaf` opts
        // back into the legacy sugarloaf path for A/B comparison.
        let use_gpu = std::env::var("KASATERM_RENDERER")
            .map(|v| !v.eq_ignore_ascii_case("sugarloaf"))
            .unwrap_or(true);
        let sg_window = SugarloafWindow {
            handle: window.window_handle().unwrap().as_raw(),
            display: window.display_handle().unwrap().as_raw(),
            scale: window.scale_factor() as f32,
            size: SugarloafWindowSize {
                width: window.inner_size().width as f32,
                height: window.inner_size().height as f32,
            },
        };
        // Match the user's active macOS Terminal.app profile
        // (Default Window Settings = "GitHub Dark Dimmed"). The plist
        // stores the font as the NSKeyedArchiver bytes of an NSFont
        // whose fontName() is "D2CodingLigatureNFM" — the PostScript
        // name of D2CodingLigature Nerd Font Mono. CoreText / swash
        // resolve through family names, so we point at the family the
        // system registry lists for that .ttf and fall through to
        // sugarloaf's bundled Cascadia when the face isn't installed.
        let mut fonts = sugarloaf::font::fonts::SugarloafFonts::default();
        // D2CodingLigature Nerd Font Mono — the macOS profile font,
        // ships with the full Hangul / Latin / Nerd-icon glyph
        // coverage we want on Windows too. dhnam/d2coding-nerd-font
        // hosts the patched TTFs; install them into
        // %LOCALAPPDATA%\Microsoft\Windows\Fonts and sugarloaf picks
        // up the face by name. Falls back to sugarloaf's bundled
        // Cascadia when the face isn't installed.
        fonts.family = Some("D2CodingLigature Nerd Font Mono".to_string());
        // Symbols-Only Nerd Font as an extra fallback so users who
        // ship only the small Symbols variant still get the PUA
        // icons claude code's statusline expects. The primary
        // D2CodingLigature Nerd Font Mono already has these, so this
        // is a safety net rather than the main path.
        //
        // Segoe UI Symbol carries U+23F5 ⏵ (the chevron in front of
        // bypass-permissions) — no Nerd Font ships that glyph. cells.rs
        // already breaks the run-batch on the U+2300–U+27BF range so
        // the proportional glyph gets its own draw call instead of
        // dragging neighbour ASCII through propo advances.
        fonts.symbol_map = Some(vec![
            sugarloaf::font::fonts::SymbolMap {
                start: "2300".to_string(),
                end: "23FF".to_string(),
                font_family: "Segoe UI Symbol".to_string(),
            },
            sugarloaf::font::fonts::SymbolMap {
                start: "E000".to_string(),
                end: "F8FF".to_string(),
                font_family: "Symbols Nerd Font Mono".to_string(),
            },
            sugarloaf::font::fonts::SymbolMap {
                start: "F0000".to_string(),
                end: "1FFFD".to_string(),
                font_family: "Symbols Nerd Font Mono".to_string(),
            },
        ]);
        // gpu path: skip sugarloaf init entirely, build the cell
        // renderer and reuse the tail of this function (backend
        // selection, sockets, autosend/autocapture/autosplit) for the
        // sugarloaf path's bookkeeping. cell_geom uses our shaper.
        if use_gpu {
            let _ = sg_window; // not used on this path
            let renderer = gpu::GpuRenderer::new(window.clone(), FONT_SIZE)
                .expect("GpuRenderer init");
            self.cell = CellGeom {
                w: renderer.cell_w,
                h: renderer.cell_h,
                baseline: 0.0,
            };
            let scale = window.scale_factor() as f32;
            eprintln!(
                "[startup] gpu renderer; cell_geom w={:.2} h={:.2} (scale={scale})",
                self.cell.w, self.cell.h,
            );
            self.gpu = Some(renderer);
            self.window = Some(window);
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
            self.schedule_autoquit();
            self.open_git_panel(event_loop, false);
            return;
        }
        let (font_library, font_err) = sugarloaf::font::FontLibrary::new(fonts);
        if let Some(err) = font_err {
            if !err.fonts_not_found.is_empty() {
                eprintln!(
                    "[font] requested fonts not found, sugarloaf will fall back: {:?}",
                    err.fonts_not_found
                );
            }
        }
        let sugarloaf = Sugarloaf::new(
            sg_window,
            SugarloafRenderer::default(),
            &font_library,
            RootStyle::default(),
        )
        .expect("Sugarloaf instance");
        // Replace the CellGeom default with the actual font advance /
        // ascent so columns align right of col ~80, where the 8.6
        // estimate started drifting visibly.
        let scale = window.scale_factor() as f32;
        // line_height here is a multiplier (1.0 = font ascent+descent only),
        // *not* a pixel value — rio's default is 1.0. Pass the multiplier
        // directly; passing pixels produces absurd cell sizes.
        let (_dim, metrics) =
            sugarloaf.compute_cell_metrics(FONT_SIZE, LINE_HEIGHT_MULT, scale);
        // compute_cell_metrics returns u32 physical pixels — divide by
        // scale to land back in logical units the rest of the renderer
        // works with.
        // sugarloaf's `text.draw(x, y, ...)` treats `y` as the
        // **text bounding box top-left**, not the baseline (see
        // sugarloaf::components::text::TextInstance docs: bearings
        // shift down to the bitmap top from `pos`). Passing row_top
        // directly is enough — the per-glyph bearings already place
        // the bitmap at the right vertical offset inside the cell.
        // Stored as 0 so cells::render_screen / render_preedit's
        // `y = origin_y + baseline_offset` formula collapses to
        // `y = origin_y`.
        self.cell = CellGeom {
            w: (metrics.cell_width as f32) / scale,
            h: (metrics.cell_height as f32) / scale,
            baseline: 0.0,
        };
        eprintln!(
            "[startup] cell_geom w={:.2} h={:.2} baseline={:.2} (scale={scale})",
            self.cell.w, self.cell.h, self.cell.baseline
        );
        self.sugarloaf = Some(sugarloaf);
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
            if matches!(event, WindowEvent::CloseRequested) {
                self.git_panel_webview = None;
                self.git_panel_window = None;
            }
            return;
        }
        // Same guard for the session panel. Without it the panel window's
        // Resized/ScaleFactorChanged events fall through to the terminal
        // handler below and call gpu.resize() with the panel's tiny size,
        // shrinking the main wgpu viewport uniform → everything renders
        // ~2x zoomed. (git panel had this guard, session panel did not.)
        if self.session_panel_window.as_ref().map(|w| w.id()) == Some(id) {
            if matches!(event, WindowEvent::CloseRequested) {
                self.session_panel_webview = None;
                self.session_panel_window = None;
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
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                let size = window.inner_size();
                if gpu_mode {
                    if let Some(g) = self.gpu.as_mut() {
                        g.resize(size.width, size.height);
                    }
                } else if let Some(sg) = self.sugarloaf.as_mut() {
                    sg.rescale(scale_factor as f32);
                    sg.resize(size.width, size.height);
                }
                window.request_redraw();
            }
            WindowEvent::Resized(size) => {
                if gpu_mode {
                    if let Some(g) = self.gpu.as_mut() {
                        g.resize(size.width, size.height);
                    }
                } else if let Some(sg) = self.sugarloaf.as_mut() {
                    sg.resize(size.width, size.height);
                }
                // window_cells() already subtracts WINDOW_PADDING on
                // both sides — using inline raw math here told the PTY
                // there were 2 more rows than we actually paint, and
                // the last two lines (Claude Code's `bypass…` row)
                // landed past our grid bottom.
                let (cols, rows) = self.window_cells();
                self.resize_backend(cols, rows);
                window.request_redraw();
            }
            WindowEvent::ModifiersChanged(mods) => {
                self.modifiers = mods.state();
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.handle_wheel(delta);
            }
            WindowEvent::CursorMoved { position, .. } => {
                let scale = window.scale_factor() as f32;
                self.cursor_px = (position.x as f32 / scale, position.y as f32 / scale);
                // Divider drag in progress: re-derive the split ratio from
                // the cursor and resize every affected PTY. Takes priority
                // over selection / mouse-forwarding so the grab stays sticky.
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
                    if let Some(tree) = self.pty_layout.as_mut() {
                        tree.resize_divider(&path, pos, cols, rows);
                    }
                    self.resize_backend(cols, rows);
                    window.request_redraw();
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
                    // Hover feedback: show a resize cursor over a seam so
                    // the divider reads as draggable.
                    let icon = match self
                        .divider_at_px(self.cursor_px.0, self.cursor_px.1)
                        .map(|(_, d)| d)
                    {
                        Some(pty_backend::SplitDir::Horizontal) => CursorIcon::ColResize,
                        Some(pty_backend::SplitDir::Vertical) => CursorIcon::RowResize,
                        None => CursorIcon::Default,
                    };
                    window.set_cursor(icon);
                }
            }
            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                // Pane header × close button. Catches clicks anywhere
                // in the multi-pane workspace before we drop into the
                // cell-grid click path.
                if matches!(state, ElementState::Pressed) {
                    let cx = self.cursor_px.0;
                    let cy = self.cursor_px.1;
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
                    // Left window-tab sidebar. Caught first — it owns the whole
                    // left strip, so a click there never falls through to the
                    // cell grid. Order: close-× (sits on top of a tab) → tab →
                    // "+" new-window button.
                    if self.sidebar_visible && cx < SIDEBAR_W {
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
                            self.new_window();
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
                    let hit = self
                        .pane_header_rects
                        .iter()
                        .find(|(_, r)| {
                            cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3
                        })
                        .map(|(id, _)| id.clone());
                    if let Some(id) = hit {
                        // × button → close that pane directly (drop the
                        // leaf + kill the PTY via remove_pane), same path
                        // as Cmd+W and socket close. Beats sending the
                        // shell `exit` and waiting for the reap pass.
                        self.remove_pane(&id);
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
                        self.last_left_click = None;
                        return;
                    }
                    let _ = window.drag_window();
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
                        // End a divider drag without falling through to the
                        // selection-release path under it.
                        if self.resize_drag.take().is_some() {
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
        self.drain_socket_inbox();
        // Fire any due KASATERM_AUTOSPLIT step before parking. No-op
        // when no plan is queued.
        self.run_pending_autosplits();
        self.run_pending_autowindows();
        self.run_pending_autotoggle();
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
    // Cross-pane RPC: stage cmux-compat next to the tmux shim so it is
    // discoverable on the child shell's PATH. A pane can then run
    // `cmux-compat send --surface %1 "..."` to drive a sibling pane
    // without needing to know the absolute target/debug path. Failure
    // is non-fatal — the shim already works without it.
    if let Some(cmux_src) = locate_cmux_compat_binary() {
        let cmux_name = if cfg!(windows) {
            "cmux-compat.exe"
        } else {
            "cmux-compat"
        };
        let cmux_target = shim_dir.join(cmux_name);
        let _ = std::fs::remove_file(&cmux_target);
        if let Err(e) = stage_shim(&cmux_src, &cmux_target) {
            eprintln!("[shim] stage cmux-compat {cmux_src:?} -> {cmux_target:?} failed: {e}");
        }
    }
    // Drop a self-contained `imgcat` on the pane PATH so the user (or
    // Claude) can show an image inline with zero install — it just
    // base64-encodes the file into an iTerm2 OSC 1337 sequence, which the
    // pty backend intercepts and the renderer draws over the cell grid.
    install_imgcat(&shim_dir);
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

/// Write a tiny `imgcat` into the shim dir (which is on the pane PATH).
/// It encodes a file as an iTerm2 OSC 1337 inline-image sequence; the
/// pty backend diverts that and the renderer draws it over the grid.
/// No external dependency beyond `base64`, which ships on macOS/Linux.
fn install_imgcat(shim_dir: &std::path::Path) {
    // Windows shells can't run this /bin/sh script and the pane PATH
    // there isn't a POSIX shell; skip rather than drop a broken file.
    if cfg!(windows) {
        return;
    }
    let script = "#!/bin/sh\n\
# kasaterm imgcat — show image(s) inline via iTerm2 OSC 1337.\n\
if [ \"$#\" -lt 1 ]; then echo \"usage: imgcat FILE [FILE...]\" >&2; exit 1; fi\n\
for f in \"$@\"; do\n\
  if [ ! -f \"$f\" ]; then echo \"imgcat: no such file: $f\" >&2; continue; fi\n\
  b64=$(base64 < \"$f\" | tr -d '\\n')\n\
  nm=$(printf '%s' \"$(basename \"$f\")\" | base64 | tr -d '\\n')\n\
  printf '\\033]1337;File=name=%s;inline=1:%s\\a\\n' \"$nm\" \"$b64\"\n\
done\n";
    let path = shim_dir.join("imgcat");
    if let Err(e) = std::fs::write(&path, script) {
        eprintln!("[shim] write imgcat failed: {e}");
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)) {
            eprintln!("[shim] chmod imgcat failed: {e}");
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
        for candidate in &[
            r"C:\Program Files\Git\bin\bash.exe",
            r"C:\Program Files\Git\usr\bin\bash.exe",
            r"C:\Program Files (x86)\Git\bin\bash.exe",
        ] {
            if std::path::Path::new(candidate).is_file() {
                return Some((*candidate).to_string());
            }
        }
    }
    None
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

/// Locate the cmux-compat binary so we can stage it alongside the
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
    for name in ["cmux-compat.exe", "cmux-compat"] {
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
                ch: c.to_string(),
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
}
