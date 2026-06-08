//! Settings screen — Warp-style full view reached from the sidebar's bottom
//! "Settings" entry. Replaces the pane grid while open; the session sidebar and
//! titlebar stay live. Left category nav (General / Appearance / Shell) + a
//! right-hand form. Each control writes through to `settings.json` immediately
//! and mirrors into the in-memory `set_*` fields so `resolve_*`/`App::new`
//! pick the value up on the next spawn/launch.
//!
//! Rendering runs inside `render_frame_gpu`'s `self.gpu.as_mut()` block where
//! `&self` is off-limits, so the paint is a free function fed a snapshot
//! (`SettingsCtx`) and returns the frame's clickable rects for hit-testing.

use super::*;

type Rect = (f32, f32, f32, f32);

const CAT_W: f32 = 200.0;
const ROW_GAP: f32 = 28.0;

/// Snapshot captured before the gpu borrow so the paint never touches `&self`.
pub(crate) struct SettingsCtx {
    pub area: Rect,
    pub cat: SettingsCat,
    pub cwd_mode: String,
    pub file_tree_default: bool,
    pub shell: String,
    pub input: Option<SettingsInput>,
    pub cursor: (f32, f32),
    pub caret_on: bool,
}

fn inside(r: Rect, p: (f32, f32)) -> bool {
    p.0 >= r.0 && p.0 <= r.0 + r.2 && p.1 >= r.1 && p.1 <= r.1 + r.3
}

impl App {
    /// Sidebar "Settings" entry — same tab-box style as the session tabs, sat
    /// just below the "+" new-window button so it reads as the last item in the
    /// tab list (Warp-style). Logical px; mirrors `sidebar_layout`'s geometry.
    pub(crate) fn settings_btn_rect(&self, _win_h_logical: f32) -> Rect {
        let n = self.windows.len();
        let tab_x = SIDEBAR_TAB_INSET;
        let tab_w = (self.sidebar_w_logical - 2.0 * SIDEBAR_TAB_INSET).max(0.0);
        let top = TITLE_HEIGHT + 8.0;
        let stride = SIDEBAR_TAB_H + SIDEBAR_TAB_GAP;
        let plus_y = top + n as f32 * stride;
        // Below the 28px "+" button, with a slightly larger gap as a divider.
        let y = plus_y + 28.0 + SIDEBAR_TAB_GAP + 6.0;
        (tab_x, y, tab_w, SIDEBAR_TAB_H)
    }

    pub(crate) fn open_settings(&mut self) {
        self.settings_open = true;
        self.settings_input = None;
        // Auto-expand a collapsed sidebar so Settings reads as the selected
        // entry in the tab list (Warp-style). Remember we did it so closing can
        // undo it; a sidebar the user opened themselves stays put.
        if !self.sidebar_visible {
            self.sidebar_visible = true;
            self.settings_expanded_sidebar = true;
            let (cols, rows) = self.window_cells();
            self.resize_backend(cols, rows);
        }
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    pub(crate) fn close_settings(&mut self) {
        self.settings_open = false;
        self.settings_input = None;
        if self.settings_expanded_sidebar {
            self.sidebar_visible = false;
            self.settings_expanded_sidebar = false;
            let (cols, rows) = self.window_cells();
            self.resize_backend(cols, rows);
        }
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// Persist the current in-memory settings to `settings.json`. Called after
    /// every control change so the choice survives a relaunch.
    fn settings_save(&self) {
        socket::write_setting("default_cwd", serde_json::Value::String(self.set_cwd_mode.clone()));
        socket::write_setting("file_tree_default", serde_json::Value::Bool(self.set_file_tree_default));
        socket::write_setting("default_shell", serde_json::Value::String(self.set_shell.clone()));
    }

    /// Build the render snapshot. Caller is responsible for being outside the
    /// gpu borrow.
    pub(crate) fn settings_snapshot(&self, win_px: (f32, f32), scale: f32) -> SettingsCtx {
        let sidebar_w = self.tab_strip_w();
        let x0 = sidebar_w;
        let y0 = TITLE_HEIGHT;
        let w = (win_px.0 / scale - x0).max(0.0);
        let h = (win_px.1 / scale - y0).max(0.0);
        SettingsCtx {
            area: (x0, y0, w, h),
            cat: self.settings_cat,
            cwd_mode: self.set_cwd_mode.clone(),
            file_tree_default: self.set_file_tree_default,
            shell: self.set_shell.clone(),
            input: self.settings_input,
            cursor: self.cursor_px,
            caret_on: self.last_blink_on,
        }
    }

    /// Hit-test a click against the settings screen. Returns true if the click
    /// was consumed (so the caller stops further routing).
    pub(crate) fn settings_click(&mut self, cx: f32, cy: f32) -> bool {
        let hit = self
            .settings_rects
            .iter()
            .find(|(_, r)| inside(*r, (cx, cy)))
            .map(|(a, _)| a.clone());
        let Some(action) = hit else {
            // A click anywhere inside the settings area that misses a control
            // just drops text focus; clicks land here only while the screen is
            // open, so this never eats terminal input.
            self.settings_input = None;
            self.chrome_dirty = true;
            return true;
        };
        match action {
            SettingsAction::Category(c) => {
                self.settings_cat = c;
                self.settings_input = None;
            }
            SettingsAction::CwdMode(m) => {
                // "last"/"home" are literal; "custom" keeps any existing path or
                // seeds $HOME so the field isn't empty.
                if m == "custom" {
                    if self.set_cwd_mode == "last" || self.set_cwd_mode == "home" {
                        self.set_cwd_mode =
                            std::env::var("HOME").unwrap_or_default();
                    }
                    self.settings_input = Some(SettingsInput::CwdPath);
                } else {
                    self.set_cwd_mode = m.to_string();
                    self.settings_input = None;
                }
                self.settings_save();
            }
            SettingsAction::FocusCwdPath => {
                self.settings_input = Some(SettingsInput::CwdPath);
            }
            SettingsAction::ToggleFileTree => {
                self.set_file_tree_default = !self.set_file_tree_default;
                self.settings_save();
            }
            SettingsAction::ShellPreset(s) => {
                self.set_shell = s;
                self.settings_input = None;
                self.settings_save();
            }
            SettingsAction::FocusShell => {
                self.settings_input = Some(SettingsInput::Shell);
            }
        }
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
        true
    }

    /// Route a keystroke into the focused settings text field. Returns true if
    /// it was consumed. Backspace/char/Esc only; Enter just blurs.
    pub(crate) fn settings_key(&mut self, event: &winit::event::KeyEvent) -> bool {
        use winit::keyboard::{Key, NamedKey};
        let Some(field) = self.settings_input else { return false };
        let buf = match field {
            SettingsInput::CwdPath => &mut self.set_cwd_mode,
            SettingsInput::Shell => &mut self.set_shell,
        };
        match &event.logical_key {
            Key::Named(NamedKey::Backspace) => {
                buf.pop();
            }
            Key::Named(NamedKey::Escape) | Key::Named(NamedKey::Enter) => {
                self.settings_input = None;
            }
            Key::Named(NamedKey::Space) => buf.push(' '),
            Key::Character(t) => buf.push_str(t),
            _ => return true,
        }
        self.settings_save();
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
        true
    }
}

/// Paint the full settings screen and return its clickable rects. Free function
/// so it can run inside the `self.gpu.as_mut()` borrow.
pub(crate) fn paint_settings(
    g: &mut gpu::GpuRenderer,
    ctx: &SettingsCtx,
) -> Vec<(SettingsAction, Rect)> {
    let mut rects: Vec<(SettingsAction, Rect)> = Vec::new();
    let (ax, ay, aw, ah) = ctx.area;
    // Opaque backdrop over the pane grid.
    g.rect(ax, ay, aw, ah, theme::BG);

    // ── Left category nav ────────────────────────────────────────────────
    g.rect(ax, ay, CAT_W, ah, theme::BG);
    g.rect(ax + CAT_W - 1.0, ay, 1.0, ah, theme::BORDER);
    g.draw_text(
        ax + 20.0,
        ay + 20.0,
        "Settings",
        gpu::DrawOpts { font_size: 13.0, color: theme::TEXT_MUTE, bold: true, italic: false },
    );
    let cats = [
        (SettingsCat::General, "일반"),
        (SettingsCat::Appearance, "모양"),
        (SettingsCat::Shell, "셸"),
    ];
    let mut cy = ay + 52.0;
    for (cat, label) in cats {
        let r = (ax + 10.0, cy, CAT_W - 20.0, 36.0);
        let selected = cat == ctx.cat;
        let hover = inside(r, ctx.cursor);
        if selected {
            round_rect(g, r.0, r.1, r.2, r.3, theme::RADIUS_MD, theme::SURFACE_ACTIVE);
        } else if hover {
            round_rect(g, r.0, r.1, r.2, r.3, theme::RADIUS_MD, theme::SURFACE_HOVER);
        }
        g.draw_text(
            r.0 + 14.0,
            r.1 + 10.0,
            label,
            gpu::DrawOpts {
                font_size: 14.0,
                color: if selected { theme::TEXT } else { theme::TEXT_DIM },
                bold: selected,
                italic: false,
            },
        );
        rects.push((SettingsAction::Category(cat), r));
        cy += 40.0;
    }

    // ── Right form pane ──────────────────────────────────────────────────
    let fx = ax + CAT_W + 40.0;
    let fy = ay + 36.0;
    let fw = (aw - CAT_W - 80.0).max(120.0);
    match ctx.cat {
        SettingsCat::General => {
            let mut y = fy;
            // 시작 작업 폴더
            section_label(g, fx, y, "시작 작업 폴더");
            y += 24.0;
            help_text(g, fx, y, "새 창·탭의 셸이 시작할 위치");
            y += 26.0;
            let cwd_is = |m: &str| {
                if m == "last" {
                    ctx.cwd_mode == "last"
                } else if m == "home" {
                    ctx.cwd_mode == "home"
                } else {
                    ctx.cwd_mode != "last" && ctx.cwd_mode != "home"
                }
            };
            let segs: [(&'static str, &str); 3] =
                [("last", "마지막 폴더"), ("home", "홈"), ("custom", "직접 지정")];
            let mut sx = fx;
            for (val, label) in segs {
                let tw = g.measure_chrome_text(label, 13.0, false) + 28.0;
                let r = (sx, y, tw, 32.0);
                let sel = cwd_is(val);
                let hover = inside(r, ctx.cursor);
                round_rect(
                    g, r.0, r.1, r.2, r.3, theme::RADIUS_MD,
                    if sel { theme::ACCENT } else if hover { theme::SURFACE_HOVER } else { theme::SURFACE_ACTIVE },
                );
                g.draw_text(
                    r.0 + 14.0,
                    r.1 + 8.0,
                    label,
                    gpu::DrawOpts {
                        font_size: 13.0,
                        color: if sel { theme::BG } else { theme::TEXT_DIM },
                        bold: sel,
                        italic: false,
                    },
                );
                rects.push((SettingsAction::CwdMode(val), r));
                sx += tw + 8.0;
            }
            y += 44.0;
            // Custom path field, only when "직접 지정" is active.
            if cwd_is("custom") {
                let r = (fx, y, fw.min(420.0), 34.0);
                let focused = ctx.input == Some(SettingsInput::CwdPath);
                text_field(g, r, &ctx.cwd_mode, focused, ctx.caret_on, ctx.cursor);
                rects.push((SettingsAction::FocusCwdPath, r));
                y += 34.0;
            }
            y += ROW_GAP;
            // 파일트리 기본 표시
            section_label(g, fx, y, "파일트리 기본 표시");
            y += 24.0;
            help_text(g, fx, y, "켜면 실행할 때 파일트리 사이드바가 열린 채로 시작");
            y += 28.0;
            let tr = (fx, y, 52.0, 30.0);
            toggle(g, tr, ctx.file_tree_default, ctx.cursor);
            rects.push((SettingsAction::ToggleFileTree, tr));
        }
        SettingsCat::Appearance => {
            section_label(g, fx, fy, "테마 · 색");
            help_text(g, fx, fy + 28.0, "준비 중 — 다음 업데이트에서 다크/라이트 + 강조색 프리셋이 추가됩니다.");
        }
        SettingsCat::Shell => {
            let mut y = fy;
            section_label(g, fx, y, "기본 셸");
            y += 24.0;
            help_text(g, fx, y, "새 pane이 띄울 셸 (비우면 시스템 기본 $SHELL)");
            y += 26.0;
            let presets: [(&str, &str); 3] =
                [("", "시스템 기본"), ("/bin/zsh", "zsh"), ("/bin/bash", "bash")];
            let shell_is_preset = presets.iter().any(|(v, _)| *v == ctx.shell);
            let mut sx = fx;
            for (val, label) in presets {
                let tw = g.measure_chrome_text(label, 13.0, false) + 28.0;
                let r = (sx, y, tw, 32.0);
                let sel = ctx.shell == val;
                let hover = inside(r, ctx.cursor);
                round_rect(
                    g, r.0, r.1, r.2, r.3, theme::RADIUS_MD,
                    if sel { theme::ACCENT } else if hover { theme::SURFACE_HOVER } else { theme::SURFACE_ACTIVE },
                );
                g.draw_text(
                    r.0 + 14.0,
                    r.1 + 8.0,
                    label,
                    gpu::DrawOpts {
                        font_size: 13.0,
                        color: if sel { theme::BG } else { theme::TEXT_DIM },
                        bold: sel,
                        italic: false,
                    },
                );
                rects.push((SettingsAction::ShellPreset(val.to_string()), r));
                sx += tw + 8.0;
            }
            // "직접" chip → focuses the free-text field below.
            {
                let label = "직접";
                let tw = g.measure_chrome_text(label, 13.0, false) + 28.0;
                let r = (sx, y, tw, 32.0);
                let sel = !shell_is_preset;
                let hover = inside(r, ctx.cursor);
                round_rect(
                    g, r.0, r.1, r.2, r.3, theme::RADIUS_MD,
                    if sel { theme::ACCENT } else if hover { theme::SURFACE_HOVER } else { theme::SURFACE_ACTIVE },
                );
                g.draw_text(
                    r.0 + 14.0,
                    r.1 + 8.0,
                    label,
                    gpu::DrawOpts {
                        font_size: 13.0,
                        color: if sel { theme::BG } else { theme::TEXT_DIM },
                        bold: sel,
                        italic: false,
                    },
                );
                rects.push((SettingsAction::FocusShell, r));
            }
            y += 44.0;
            if !shell_is_preset {
                let r = (fx, y, fw.min(420.0), 34.0);
                let focused = ctx.input == Some(SettingsInput::Shell);
                text_field(g, r, &ctx.shell, focused, ctx.caret_on, ctx.cursor);
                rects.push((SettingsAction::FocusShell, r));
            }
        }
    }

    rects
}

fn section_label(g: &mut gpu::GpuRenderer, x: f32, y: f32, text: &str) {
    g.draw_text(
        x,
        y,
        text,
        gpu::DrawOpts { font_size: 15.0, color: theme::TEXT, bold: true, italic: false },
    );
}

fn help_text(g: &mut gpu::GpuRenderer, x: f32, y: f32, text: &str) {
    g.draw_text(
        x,
        y,
        text,
        gpu::DrawOpts { font_size: 12.0, color: theme::TEXT_MUTE, bold: false, italic: false },
    );
}

fn toggle(g: &mut gpu::GpuRenderer, r: Rect, on: bool, cursor: (f32, f32)) {
    let hover = inside(r, cursor);
    let track = if on {
        theme::ACCENT
    } else if hover {
        theme::SURFACE_HOVER
    } else {
        theme::SURFACE_ACTIVE
    };
    round_rect(g, r.0, r.1, r.2, r.3, r.3 / 2.0, track);
    let knob = r.3 - 8.0;
    let kx = if on { r.0 + r.2 - knob - 4.0 } else { r.0 + 4.0 };
    round_rect(g, kx, r.1 + 4.0, knob, knob, knob / 2.0, theme::TEXT);
}

fn text_field(
    g: &mut gpu::GpuRenderer,
    r: Rect,
    text: &str,
    focused: bool,
    caret_on: bool,
    cursor: (f32, f32),
) {
    let hover = inside(r, cursor);
    round_rect(g, r.0, r.1, r.2, r.3, theme::RADIUS_SM, theme::SURFACE_ACTIVE);
    let border = if focused {
        theme::ACCENT
    } else if hover {
        theme::TEXT_MUTE
    } else {
        theme::BORDER
    };
    // 1px frame via four hairlines.
    g.rect(r.0, r.1, r.2, 1.0, border);
    g.rect(r.0, r.1 + r.3 - 1.0, r.2, 1.0, border);
    g.rect(r.0, r.1, 1.0, r.3, border);
    g.rect(r.0 + r.2 - 1.0, r.1, 1.0, r.3, border);
    let tx = r.0 + 12.0;
    let ty = r.1 + (r.3 - 13.0) / 2.0;
    let adv = g.draw_text(
        tx,
        ty,
        text,
        gpu::DrawOpts { font_size: 13.0, color: theme::TEXT, bold: false, italic: false },
    );
    if focused && caret_on {
        g.rect(tx + adv + 1.0, r.1 + 7.0, 1.5, r.3 - 14.0, theme::TEXT);
    }
}
