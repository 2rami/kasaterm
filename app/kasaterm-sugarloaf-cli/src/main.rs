//! kasaterm-sugarloaf-cli — sugarloaf-rendered terminal driven by
//! tmux-bridge. Phase A Task #13: wheel scroll + scrollback, mouse drag
//! selection + clipboard, Korean IME preedit, Cmd+C / Cmd+V. Stays
//! framework-agnostic — no iced or kasaterm in the dep tree.

mod cells;

use anyhow::Result;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use std::collections::VecDeque;
use std::error::Error;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use sugarloaf::layout::RootStyle;
use sugarloaf::{Sugarloaf, SugarloafRenderer, SugarloafWindow, SugarloafWindowSize};
use tmux_bridge::screen::Cell as GridCell;
use tmux_bridge::{ScreenUpdate, StartOptions, TmuxSession};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{
    ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowAttributes, WindowId};

const FONT_SIZE: f32 = 14.0;
const LINE_HEIGHT_MULT: f32 = 1.3;
const SCROLLBACK_MAX: usize = 5000;
const WHEEL_THROTTLE_MS: u64 = 8;
/// Half-period of the cursor blink in milliseconds. macOS uses 530 by
/// default; iTerm2 uses 500. 530 matches the platform feel.
const BLINK_HALF_PERIOD_MS: u64 = 530;
/// While the user is actively typing we keep the cursor solid for this
/// long after the last keystroke so it's easy to follow the caret. Same
/// idea as iTerm2's "smart cursor" pause.
const BLINK_PAUSE_AFTER_INPUT_MS: u64 = 700;

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
    out
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

#[derive(Default)]
struct Screen {
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
    /// OSC 0/2 title — `printf '\e]0;hello\a'` lands here. Applied to
    /// the winit window's chrome whenever it changes.
    title: Option<String>,
}

struct App {
    window: Option<Arc<Window>>,
    sugarloaf: Option<Sugarloaf<'static>>,
    tmux: Option<Arc<TmuxSession>>,
    screen: Arc<Mutex<Screen>>,
    /// Measured cell geometry from sugarloaf — see `measure_cell_geom`.
    cell: CellGeom,
    preedit: String,
    in_preedit: bool,
    selection: Option<Selection>,
    drag_anchor: Option<(u16, u16)>,
    cursor_px: (f32, f32),
    modifiers: ModifiersState,
    scroll_offset: usize,
    wheel_accum_y: f32,
    last_wheel_emit: Instant,
    /// Last keystroke / IME / mouse press timestamp. Resets the blink
    /// phase so the cursor stays solid while the user is actively
    /// interacting and only fades back to blinking on idle.
    last_input_at: Instant,
}

impl App {
    fn new() -> Self {
        Self {
            window: None,
            sugarloaf: None,
            tmux: None,
            screen: Arc::new(Mutex::new(Screen::default())),
            cell: CellGeom::default(),
            preedit: String::new(),
            in_preedit: false,
            selection: None,
            drag_anchor: None,
            cursor_px: (0.0, 0.0),
            modifiers: ModifiersState::empty(),
            scroll_offset: 0,
            wheel_accum_y: 0.0,
            last_wheel_emit: Instant::now() - std::time::Duration::from_secs(1),
            last_input_at: Instant::now(),
        }
    }

    /// True when the cursor block should be visible this frame.
    /// Solid for `BLINK_PAUSE_AFTER_INPUT_MS` after any input event, then
    /// toggles every `BLINK_HALF_PERIOD_MS`.
    fn cursor_blink_on(&self, now: Instant) -> bool {
        let since_input = now.saturating_duration_since(self.last_input_at);
        if since_input.as_millis() < BLINK_PAUSE_AFTER_INPUT_MS as u128 {
            return true;
        }
        let elapsed = since_input.as_millis() - BLINK_PAUSE_AFTER_INPUT_MS as u128;
        (elapsed / BLINK_HALF_PERIOD_MS as u128) % 2 == 0
    }

    fn schedule_autocapture(&self) {
        let Ok(ms_str) = std::env::var("TMUXIFY_AUTOCAPTURE_MS") else { return; };
        let Ok(ms) = ms_str.parse::<u64>() else { return; };
        let path = std::env::var("TMUXIFY_AUTOCAPTURE_PATH")
            .unwrap_or_else(|_| "/tmp/kasaterm-sugarloaf-cli.png".into());
        eprintln!("[autocapture] in {ms}ms → {path}");
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(ms));
            let pid = std::process::id();
            // Force our window to the front, then capture just its region.
            // Full-desktop screencapture grabs whatever app is on top —
            // useless in headless verify runs where another app may have
            // focus. Window-bounded capture sidesteps that.
            let _ = std::process::Command::new("osascript")
                .args([
                    "-e",
                    &format!(
                        "tell application \"System Events\" to set frontmost of (first process whose unix id is {pid}) to true"
                    ),
                ])
                .status();
            std::thread::sleep(std::time::Duration::from_millis(400));
            // Query window bounds {x, y, w, h} via System Events.
            let bounds_script = format!(
                "tell application \"System Events\" to tell (first process whose unix id is {pid}) to get {{position, size}} of window 1"
            );
            let bounds_out = std::process::Command::new("osascript")
                .args(["-e", &bounds_script])
                .output();
            let region = bounds_out.ok().and_then(|o| {
                let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                // Format: "x, y, w, h" (System Events returns flattened list).
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
        });
    }

    fn schedule_autosend(&self) {
        let Ok(text) = std::env::var("TMUXIFY_AUTOSEND") else { return; };
        let ms: u64 = std::env::var("TMUXIFY_AUTOSEND_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2000);
        eprintln!("[autosend] in {ms}ms: {text:?}");
        let tmux = self.tmux.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(ms));
            if let Some(t) = tmux.as_ref() {
                let mut payload = text.clone();
                if !payload.ends_with('\n') {
                    payload.push('\n');
                }
                let hex: String = payload
                    .bytes()
                    .map(|b| format!("{b:02x}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                let _ = t.send_keys_hex(None, &hex);
            }
        });
    }

    fn start_tmux(&mut self) -> Result<()> {
        let window = self.window.as_ref().expect("window before tmux");
        let size = window.inner_size();
        let scale = window.scale_factor() as f32;
        let lw = size.width as f32 / scale;
        let lh = size.height as f32 / scale;
        let cols = (lw / self.cell.w).floor().max(40.0) as u16;
        let rows = (lh / self.cell.h).floor().max(10.0) as u16;
        let cwd = std::env::current_dir().ok().and_then(|p| p.to_str().map(String::from));
        let tmux = TmuxSession::start(StartOptions {
            cwd: cwd.as_deref(),
            socket_name: Some("kasaterm-sugarloaf-cli"),
            cols,
            rows,
            ..Default::default()
        })?;
        let screens = tmux.screens.clone();
        let screen = self.screen.clone();
        let win = self.window.clone();
        std::thread::spawn(move || {
            // Last-applied snapshot — used to detect rows that scrolled
            // off the top so we can preserve them in history.
            let mut prev_cells: Vec<Vec<GridCell>> = Vec::new();
            while let Ok(ScreenUpdate {
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
                let mut s = screen.lock().unwrap();
                let resized = s.cols != cols
                    || s.rows != rows
                    || s.cells.len() != rows as usize;
                if resized {
                    s.cols = cols;
                    s.rows = rows;
                    s.cells = (0..rows as usize)
                        .map(|_| vec![GridCell::blank(); cols as usize])
                        .collect();
                    prev_cells.clear();
                }
                for (r, row) in dirty {
                    if let Some(dst) = s.cells.get_mut(r as usize) {
                        *dst = row;
                    }
                }
                // Shift detection: a prefix of prev appearing as a suffix
                // of new (offset by k rows) means k rows scrolled off the
                // top. Push them into the history ring. Skipped in
                // alt-screen (claude / vim manage own scrollback) and
                // right after resize.
                if !alt_screen && !prev_cells.is_empty() && prev_cells.len() == s.cells.len() {
                    let n = prev_cells.len();
                    let mut shifted = 0usize;
                    for k in 1..n {
                        if prev_cells[k..] == s.cells[..n - k] {
                            shifted = k;
                            break;
                        }
                    }
                    if shifted > 0 {
                        for row in &prev_cells[..shifted] {
                            s.history.push_back(row.clone());
                        }
                        while s.history.len() > SCROLLBACK_MAX {
                            s.history.pop_front();
                        }
                    }
                }
                prev_cells = s.cells.clone();
                s.cursor_row = cursor_row;
                s.cursor_col = cursor_col;
                s.cursor_visible = cursor_visible;
                s.alt_screen = alt_screen;
                s.mouse_enabled = mouse_enabled;
                s.mouse_sgr = mouse_sgr;
                // Title comparison so we only call set_title when it
                // actually changes — otherwise the window-server fights
                // us on every ScreenUpdate.
                let new_title = title.filter(|t| !t.is_empty());
                let title_changed = s.title != new_title;
                if title_changed {
                    s.title = new_title.clone();
                }
                drop(s);
                if let Some(w) = win.as_ref() {
                    if title_changed {
                        let display = new_title.unwrap_or_else(|| "kasaterm-sugarloaf-cli".into());
                        w.set_title(&display);
                    }
                    w.request_redraw();
                }
            }
        });
        self.tmux = Some(Arc::new(tmux));
        Ok(())
    }

    /// Convert logical-pixel cursor position into a (col, row) cell.
    /// Origin of grid is offset by 8px on both axes (see render path).
    fn px_to_cell(&self, px: f32, py: f32) -> Option<(u16, u16)> {
        let s = self.screen.lock().unwrap();
        if s.cols == 0 || s.rows == 0 {
            return None;
        }
        let col = ((px - 8.0).max(0.0) / self.cell.w).floor() as u16;
        let row = ((py - 8.0).max(0.0) / self.cell.h).floor() as u16;
        Some((col.min(s.cols - 1), row.min(s.rows - 1)))
    }

    fn send_bytes(&self, bytes: &[u8]) {
        let Some(tmux) = self.tmux.as_ref() else { return; };
        if bytes.is_empty() {
            return;
        }
        let hex: String = bytes
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(" ");
        let _ = tmux.send_keys_hex(None, &hex);
    }

    fn copy_selection(&self) {
        let Some(sel) = self.selection else { return; };
        let rows = self.screen.lock().unwrap().cells.clone();
        let text = extract_selection(&rows, sel);
        if text.is_empty() {
            return;
        }
        match arboard::Clipboard::new() {
            Ok(mut cb) => {
                if let Err(e) = cb.set_text(text) {
                    eprintln!("[kasaterm-sugarloaf-cli] clipboard write failed: {e}");
                }
            }
            Err(e) => eprintln!("[kasaterm-sugarloaf-cli] clipboard open failed: {e}"),
        }
    }

    fn paste_clipboard(&self) {
        let text = match arboard::Clipboard::new().and_then(|mut cb| cb.get_text()) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("[kasaterm-sugarloaf-cli] clipboard read failed: {e}");
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
        let (alt, hist_len, mouse_on, mouse_sgr) = {
            let s = self.screen.lock().unwrap();
            (s.alt_screen, s.history.len(), s.mouse_enabled, s.mouse_sgr)
        };
        // Best path: SGR mouse-mode wheel events. Apps that opt in
        // (claude, vim, lazygit, htop) get smooth per-line scroll.
        if mouse_on && mouse_sgr {
            let (col, row) = self
                .px_to_cell(self.cursor_px.0, self.cursor_px.1)
                .unwrap_or((1, 1));
            let button = if lines > 0 { 64 } else { 65 };
            let count = lines.unsigned_abs().min(8) as usize;
            let single = format!("\x1b[<{button};{};{}M", col + 1, row + 1);
            let payload: Vec<u8> = single.as_bytes().repeat(count.max(1));
            self.send_bytes(&payload);
            return;
        }
        if alt {
            // alt-screen apps without mouse mode: send PgUp/PgDn.
            let esc: &[u8] = if lines > 0 { b"\x1b[5~" } else { b"\x1b[6~" };
            self.send_bytes(esc);
            return;
        }
        // Normal screen: walk our own history ring.
        let step = lines.unsigned_abs().min(8) as usize;
        if lines > 0 {
            self.scroll_offset = (self.scroll_offset + step).min(hist_len);
        } else {
            self.scroll_offset = self.scroll_offset.saturating_sub(step);
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
        // Typing always snaps to live tail.
        if self.scroll_offset != 0 {
            self.scroll_offset = 0;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
        let is_cmd = self.modifiers.super_key() || self.modifiers.control_key();
        if is_cmd {
            if let Key::Character(s) = &event.logical_key {
                if s.eq_ignore_ascii_case("c") && self.selection.is_some() {
                    self.copy_selection();
                    return;
                }
                if s.eq_ignore_ascii_case("v") {
                    self.paste_clipboard();
                    return;
                }
            }
        }
        let bytes: Vec<u8> = match &event.logical_key {
            Key::Named(NamedKey::Enter) => b"\r".to_vec(),
            Key::Named(NamedKey::Backspace) => b"\x7f".to_vec(),
            Key::Named(NamedKey::Tab) => b"\t".to_vec(),
            Key::Named(NamedKey::Escape) => b"\x1b".to_vec(),
            Key::Named(NamedKey::ArrowUp) => b"\x1b[A".to_vec(),
            Key::Named(NamedKey::ArrowDown) => b"\x1b[B".to_vec(),
            Key::Named(NamedKey::ArrowRight) => b"\x1b[C".to_vec(),
            Key::Named(NamedKey::ArrowLeft) => b"\x1b[D".to_vec(),
            _ => match event.text.as_ref() {
                Some(t) => {
                    // Drop non-ASCII Character events ONLY while the IME
                    // is actively composing — those are the jamo that
                    // Preedit/Commit will deliver again. Outside an active
                    // preedit (e.g. very first key), the Character event
                    // is the only channel, so let it pass.
                    let non_ascii_during_preedit = self.in_preedit
                        && !t.chars().all(|c| c.is_ascii() && !c.is_control());
                    if non_ascii_during_preedit {
                        return;
                    }
                    t.as_bytes().to_vec()
                }
                None => return,
            },
        };
        self.send_bytes(&bytes);
    }

    fn render_frame(&mut self) {
        // Snapshot anything that needs `&self` access before grabbing the
        // sugarloaf `&mut` — the borrow checker won't let us re-enter
        // immutable self methods while sugarloaf is mutably borrowed.
        let now = Instant::now();
        let blink_on = self.cursor_blink_on(now);
        let Some(window) = self.window.as_ref() else { return; };
        let Some(sugarloaf) = self.sugarloaf.as_mut() else { return; };
        let size = window.inner_size();
        sugarloaf.rect(
            None,
            0.0,
            0.0,
            size.width as f32,
            size.height as f32,
            [
                cells::DEFAULT_BG[0] as f32 / 255.0,
                cells::DEFAULT_BG[1] as f32 / 255.0,
                cells::DEFAULT_BG[2] as f32 / 255.0,
                1.0,
            ],
            0.0,
            0,
        );

        // Compose visible rows from (history slice + live cells) using
        // scroll_offset. Snapshot under lock, render outside.
        let (rows, cur_r, cur_c, cur_vis) = {
            let s = self.screen.lock().unwrap();
            let total = s.rows.max(1) as usize;
            let offset = self.scroll_offset.min(s.history.len());
            let composed: Vec<Vec<GridCell>> = if offset == 0 {
                s.cells.clone()
            } else {
                let mut out: Vec<Vec<GridCell>> = Vec::with_capacity(total);
                let hist_start = s.history.len() - offset;
                for row in s.history.iter().skip(hist_start) {
                    out.push(row.clone());
                    if out.len() >= total {
                        break;
                    }
                }
                let need = total.saturating_sub(out.len());
                for row in s.cells.iter().take(need) {
                    out.push(row.clone());
                }
                out
            };
            let cur_vis = offset == 0 && s.cursor_visible;
            (composed, s.cursor_row, s.cursor_col, cur_vis)
        };

        if rows.is_empty() {
            sugarloaf.render();
            return;
        }

        cells::render_screen(
            sugarloaf,
            &rows,
            8.0,
            8.0,
            self.cell.w,
            self.cell.h,
            FONT_SIZE,
            self.cell.baseline,
        );

        // Selection highlight — translucent overlay on top of cells.
        if let Some(sel) = self.selection {
            cells::render_selection_overlay(sugarloaf, sel.anchor, sel.end, 8.0, 8.0, self.cell.w, self.cell.h);
        }

        // Cursor block (only when at live tail and blink-on phase).
        // Preedit always forces solid so users can see what they're
        // composing — the conditional below skips the rect, but the
        // preedit overlay below still draws.
        if cur_vis && (blink_on || !self.preedit.is_empty()) {
            let cursor_x = 8.0 + cur_c as f32 * self.cell.w;
            let cursor_y = 8.0 + cur_r as f32 * self.cell.h;
            sugarloaf.rect(
                None,
                cursor_x,
                cursor_y,
                self.cell.w,
                self.cell.h,
                [
                    cells::DEFAULT_FG[0] as f32 / 255.0,
                    cells::DEFAULT_FG[1] as f32 / 255.0,
                    cells::DEFAULT_FG[2] as f32 / 255.0,
                    0.55,
                ],
                0.0,
                0,
            );
        }

        // Preedit overlay — paint over the cursor cell so Hangul
        // composition is visible while still typing.
        if cur_vis && !self.preedit.is_empty() {
            let px = 8.0 + cur_c as f32 * self.cell.w;
            let py = 8.0 + cur_r as f32 * self.cell.h;
            cells::render_preedit(sugarloaf, &self.preedit, px, py, self.cell.w, self.cell.h, FONT_SIZE);
        }

        sugarloaf.render();
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        // WaitUntil so the cursor blink ticks even when no terminal output
        // is arriving — the redraw inside RedrawRequested re-arms the
        // schedule. Pure Wait would freeze the blink mid-phase, Poll would
        // burn CPU on idle.
        event_loop.set_control_flow(ControlFlow::WaitUntil(
            Instant::now() + std::time::Duration::from_millis(BLINK_HALF_PERIOD_MS),
        ));
        let attrs = WindowAttributes::default()
            .with_title("kasaterm-sugarloaf-cli")
            .with_inner_size(LogicalSize::new(960.0, 600.0));
        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .expect("create window"),
        );
        // Without IME enabled, Hangul / kana would arrive as raw key
        // events instead of composing into 안 / 한 / 글.
        window.set_ime_allowed(true);
        let sg_window = SugarloafWindow {
            handle: window.window_handle().unwrap().as_raw(),
            display: window.display_handle().unwrap().as_raw(),
            scale: window.scale_factor() as f32,
            size: SugarloafWindowSize {
                width: window.inner_size().width as f32,
                height: window.inner_size().height as f32,
            },
        };
        let font_library = sugarloaf::font::FontLibrary::default();
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
        self.cell = CellGeom {
            w: (metrics.cell_width as f32) / scale,
            h: (metrics.cell_height as f32) / scale,
            baseline: (metrics.cell_baseline as f32) / scale,
        };
        eprintln!(
            "[startup] cell_geom w={:.2} h={:.2} baseline={:.2} (scale={scale})",
            self.cell.w, self.cell.h, self.cell.baseline
        );
        self.sugarloaf = Some(sugarloaf);
        self.window = Some(window);
        if let Err(e) = self.start_tmux() {
            eprintln!("[kasaterm-sugarloaf-cli] tmux start failed: {e}");
        }
        self.schedule_autosend();
        self.schedule_autocapture();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: WindowId,
        event: WindowEvent,
    ) {
        let Some(window) = self.window.clone() else { return; };
        let Some(sugarloaf) = self.sugarloaf.as_mut() else { return; };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                sugarloaf.rescale(scale_factor as f32);
                let size = window.inner_size();
                sugarloaf.resize(size.width, size.height);
                window.request_redraw();
            }
            WindowEvent::Resized(size) => {
                sugarloaf.resize(size.width, size.height);
                if let Some(tmux) = self.tmux.as_ref() {
                    let scale = window.scale_factor() as f32;
                    let lw = size.width as f32 / scale;
                    let lh = size.height as f32 / scale;
                    let cols = (lw / self.cell.w).floor().max(40.0) as u16;
                    let rows = (lh / self.cell.h).floor().max(10.0) as u16;
                    let _ = tmux.resize_client(cols, rows);
                }
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
                if let (Some(anchor), Some(cell)) = (
                    self.drag_anchor,
                    self.px_to_cell(self.cursor_px.0, self.cursor_px.1),
                ) {
                    self.selection = Some(Selection { anchor, end: cell });
                    window.request_redraw();
                }
            }
            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                match state {
                    ElementState::Pressed => {
                        if let Some(cell) = self.px_to_cell(self.cursor_px.0, self.cursor_px.1) {
                            self.drag_anchor = Some(cell);
                            self.selection = Some(Selection { anchor: cell, end: cell });
                        }
                    }
                    ElementState::Released => {
                        self.drag_anchor = None;
                        if let Some(sel) = self.selection {
                            if sel.anchor == sel.end {
                                self.selection = None;
                            }
                        }
                    }
                }
                window.request_redraw();
            }
            WindowEvent::Ime(ime) => {
                match ime {
                    Ime::Preedit(text, _range) => {
                        self.in_preedit = !text.is_empty();
                        self.preedit = text;
                    }
                    Ime::Commit(text) => {
                        self.in_preedit = false;
                        self.preedit.clear();
                        self.send_bytes(text.as_bytes());
                    }
                    Ime::Disabled | Ime::Enabled => {
                        self.in_preedit = false;
                        self.preedit.clear();
                    }
                }
                window.request_redraw();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                self.forward_key(&event);
            }
            WindowEvent::RedrawRequested => {
                self.render_frame();
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Re-arm the blink deadline every loop turn so the cursor toggles
        // even on an idle terminal. WaitUntil(deadline) keeps the runtime
        // asleep — no CPU until either the deadline lands or fresh input
        // arrives. The actual redraw happens in new_events on the
        // ResumeTimeReached branch — about_to_wait should NOT request a
        // redraw here, since that would queue work and turn the wait
        // into a busy loop.
        event_loop.set_control_flow(ControlFlow::WaitUntil(
            Instant::now() + std::time::Duration::from_millis(BLINK_HALF_PERIOD_MS),
        ));
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
    let event_loop = EventLoop::new()?;
    let mut app = App::new();
    event_loop.run_app(&mut app)?;
    Ok(())
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
