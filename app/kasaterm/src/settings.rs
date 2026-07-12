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
    pub footer_default: bool,
    pub shell: String,
    pub input: Option<SettingsInput>,
    pub cursor: (f32, f32),
    pub caret_on: bool,
    /// Active theme key ("dark", "catppuccin-mocha", "custom"…).
    pub theme: String,
    /// settings.json has a `custom_theme` object → show the Custom card.
    pub has_custom_theme: bool,
    pub accent: String,
    pub font_size: f32,
    pub tabs_on_top: bool,
    pub claude_persona: bool,
    pub shim_inject: bool,
    pub claude_model: String,
    pub claude_effort: String,
    pub claude_extra: String,
}

fn inside(r: Rect, p: (f32, f32)) -> bool {
    p.0 >= r.0 && p.0 <= r.0 + r.2 && p.1 >= r.1 && p.1 <= r.1 + r.3
}

impl App {
    /// Sidebar "Settings" entry — same tab-box style as the session tabs, sat
    /// just below the "+" new-window button so it reads as the last item in the
    /// tab list (Warp-style). Logical px; mirrors `sidebar_layout`'s geometry.
    pub(crate) fn settings_btn_rect(&self, _win_h_logical: f32) -> Rect {
        // 이 버튼은 세션 사이드바 탭 리스트의 마지막 항목이라, 사이드바가 없으면
        // (top 모드 또는 사이드바 접힘) 그려지지도 않는다. 그런데 rect 는 매 프레임
        // 저장돼(render) 클릭 hit-test 에 남는다 — top 모드에서 이 유령 rect 가
        // 설정 화면 좌측 카테고리 nav(같은 좌상단 영역)의 Appearance·Shell 클릭을
        // 가로채 페이지 전환이 안 됐다(거노). 안 그려질 땐 hit 대상도 없어야 하므로
        // 무효 rect 를 돌려준다. 설정 진입점은 타이틀바 톱니(settings_toggle)가 담당.
        if self.tabs_on_top || !self.sidebar_visible {
            return (0.0, 0.0, 0.0, 0.0);
        }
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
        // undo it; a sidebar the user opened themselves stays put. With tabs on
        // top there is no side strip to expand.
        if !self.tabs_on_top && !self.sidebar_visible {
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

    /// Re-emit the claude wrapper into the live shim dir so an already-open pane
    /// picks up a knob change on its next `claude` run (the shim path is stable
    /// for the process lifetime, so overwriting the file is enough — no relaunch).
    fn regen_claude_shim(&self) {
        if let Ok(dir) = std::env::var("KASATERM_TMUX_SHIM_DIR") {
            install_claude_hook_shim(std::path::Path::new(&dir));
        }
    }

    /// Persist the current in-memory settings to `settings.json`. Called after
    /// every control change so the choice survives a relaunch.
    fn settings_save(&self) {
        socket::write_setting("default_cwd", serde_json::Value::String(self.set_cwd_mode.clone()));
        socket::write_setting("file_tree_default", serde_json::Value::Bool(self.set_file_tree_default));
        socket::write_setting("pane_footer_default", serde_json::Value::Bool(self.set_footer_default));
        socket::write_setting("default_shell", serde_json::Value::String(self.set_shell.clone()));
        socket::write_setting("claude_persona", serde_json::Value::Bool(self.set_claude_persona));
        socket::write_setting("shim_inject", serde_json::Value::Bool(self.set_shim_inject));
        socket::write_setting("claude_model", serde_json::Value::String(self.set_claude_model.clone()));
        socket::write_setting("claude_effort", serde_json::Value::String(self.set_claude_effort.clone()));
        socket::write_setting("claude_extra", serde_json::Value::String(self.set_claude_extra.clone()));
        self.regen_claude_shim();
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
            footer_default: self.set_footer_default,
            shell: self.set_shell.clone(),
            input: self.settings_input,
            cursor: self.cursor_px,
            caret_on: self.last_blink_on,
            theme: theme::theme_name().to_string(),
            has_custom_theme: socket::read_settings().get("custom_theme").is_some(),
            accent: theme::accent_name().to_string(),
            font_size: self.font_size,
            tabs_on_top: self.tabs_on_top,
            claude_persona: self.set_claude_persona,
            shim_inject: self.set_shim_inject,
            claude_model: self.set_claude_model.clone(),
            claude_effort: self.set_claude_effort.clone(),
            claude_extra: self.set_claude_extra.clone(),
        }
    }

    /// Mark every pane + chrome dirty so a theme/accent change repaints the
    /// whole window (cell backgrounds pick up the new palette too).
    fn repaint_all(&mut self) {
        if let Ok(mut ws) = self.ws.lock() {
            for p in ws.panes.values_mut() {
                p.dirty = true;
            }
        }
        self.chrome_dirty = true;
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
                // 토글을 라이브 트리에도 즉시 반영 — 안 그러면 설정에선 껐는데
                // 화면 트리는 그대로라 "안 먹힌다"고 느낀다.
                if self.file_tree.visible != self.set_file_tree_default {
                    self.file_tree.visible = self.set_file_tree_default;
                    if self.file_tree.visible {
                        self.refresh_file_tree();
                    }
                    let (cols, rows) = self.window_cells();
                    self.resize_backend(cols, rows);
                }
            }
            SettingsAction::ToggleFooter => {
                self.set_footer_default = !self.set_footer_default;
                self.settings_save();
                // 전역 기본을 뒤집으면 per-pane 예외(⋮로 끄거나 켠 pane)는 리셋해
                // 모든 pane 이 새 기본으로 통일된다. footer 표시 여부는 셀 그리드
                // 높이(statusbar_px)에 들어가므로 PTY 도 reshape.
                self.statusbar.hidden.clear();
                self.statusbar.shown.clear();
                let (cols, rows) = self.window_cells();
                self.resize_backend(cols, rows);
            }
            SettingsAction::ShellPreset(s) => {
                self.set_shell = s;
                self.settings_input = None;
                self.settings_save();
            }
            SettingsAction::FocusShell => {
                self.settings_input = Some(SettingsInput::Shell);
            }
            SettingsAction::ThemeMode(m) => {
                theme::set_theme(m);
                socket::write_setting("theme", serde_json::Value::String(m.to_string()));
                self.repaint_all();
            }
            SettingsAction::Accent(name) => {
                theme::set_accent(&name);
                socket::write_setting("accent", serde_json::Value::String(name));
                self.repaint_all();
            }
            SettingsAction::TabPosition(pos) => {
                let want_top = pos == "top";
                if self.tabs_on_top != want_top {
                    self.tabs_on_top = want_top;
                    socket::write_setting("tab_position", serde_json::Value::String(pos.to_string()));
                    // The side strip appearing/disappearing changes usable cols.
                    let (cols, rows) = self.window_cells();
                    self.resize_backend(cols, rows);
                }
            }
            SettingsAction::FontSizeDelta(d) => {
                let new = (self.font_size + d as f32).clamp(9.0, 32.0);
                if (new - self.font_size).abs() > 0.01 {
                    self.font_size = new;
                    socket::write_setting("font_size", serde_json::json!(new));
                    // Live-apply: same reflow path Cmd+/- zoom uses, so the
                    // grid + PTY resize immediately.
                    self.apply_effective_scale();
                }
            }
            SettingsAction::ToggleClaudePersona => {
                self.set_claude_persona = !self.set_claude_persona;
                self.settings_save();
            }
            SettingsAction::ToggleShimInject => {
                self.set_shim_inject = !self.set_shim_inject;
                self.settings_save();
            }
            SettingsAction::ClaudeModel(m) => {
                self.set_claude_model = m;
                self.settings_input = None;
                self.settings_save();
            }
            SettingsAction::ClaudeEffort(e) => {
                self.set_claude_effort = e;
                self.settings_input = None;
                self.settings_save();
            }
            SettingsAction::FocusClaudeExtra => {
                self.settings_input = Some(SettingsInput::ClaudeExtra);
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
            SettingsInput::ClaudeExtra => &mut self.set_claude_extra,
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
    g.rect(ax, ay, aw, ah, theme::bg());

    // ── Left category nav ────────────────────────────────────────────────
    g.rect(ax, ay, CAT_W, ah, theme::bg());
    g.rect(ax + CAT_W - 1.0, ay, 1.0, ah, theme::border());
    g.draw_text(
        ax + 20.0,
        ay + 20.0,
        "Settings",
        gpu::DrawOpts { font_size: 13.0, color: theme::text_mute(), bold: true, italic: false },
    );
    let cats = [
        (SettingsCat::General, "General", "settings-2"),
        (SettingsCat::Appearance, "Appearance", "sparkles"),
        (SettingsCat::Shell, "Shell", "terminal"),
        (SettingsCat::Claude, "Claude", "claude"),
    ];
    let mut cy = ay + 52.0;
    let mut active_label = "General";
    for (cat, label, icon) in cats {
        let r = (ax + 10.0, cy, CAT_W - 20.0, 36.0);
        let selected = cat == ctx.cat;
        if selected {
            active_label = label;
        }
        let hover = inside(r, ctx.cursor);
        if selected {
            round_rect(g, r.0, r.1, r.2, r.3, theme::RADIUS_MD, theme::surface_active());
        } else if hover {
            round_rect(g, r.0, r.1, r.2, r.3, theme::RADIUS_MD, theme::surface_hover());
        }
        let icon_c = if selected { theme::text() } else { theme::text_mute() };
        g.queue_icon(icon, r.0 + 12.0, r.1 + (r.3 - 15.0) / 2.0, 15.0, icon_c);
        g.draw_text(
            r.0 + 36.0,
            r.1 + 10.0,
            label,
            gpu::DrawOpts {
                font_size: 14.0,
                color: if selected { theme::text() } else { theme::text_dim() },
                bold: selected,
                italic: false,
            },
        );
        rects.push((SettingsAction::Category(cat), r));
        cy += 40.0;
    }

    // ── Right form pane ──────────────────────────────────────────────────
    // Page header: the active category as a title + hairline, so the form
    // reads as its own page instead of floating controls.
    let fx = ax + CAT_W + 40.0;
    let fw = (aw - CAT_W - 80.0).max(120.0);
    g.draw_text(
        fx, ay + 28.0, active_label,
        gpu::DrawOpts { font_size: 20.0, color: theme::text(), bold: true, italic: false },
    );
    g.rect(fx, ay + 62.0, fw, 1.0, theme::border());
    let fy = ay + 84.0;
    match ctx.cat {
        SettingsCat::General => {
            let mut y = fy;
            // 시작 작업 폴더
            section_label(g, fx, y, "Startup folder");
            y += 24.0;
            help_text(g, fx, y, "새 창과 탭이 열리는 위치");
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
                [("last", "Last folder"), ("home", "Home"), ("custom", "Custom")];
            let mut sx = fx;
            for (val, label) in segs {
                let tw = g.measure_chrome_text(label, 13.0, false) + 28.0;
                let r = (sx, y, tw, 32.0);
                let sel = cwd_is(val);
                let hover = inside(r, ctx.cursor);
                round_rect(
                    g, r.0, r.1, r.2, r.3, theme::RADIUS_MD,
                    if sel { theme::accent() } else if hover { theme::surface_hover() } else { theme::surface_active() },
                );
                g.draw_text(
                    r.0 + 14.0,
                    r.1 + 8.0,
                    label,
                    gpu::DrawOpts {
                        font_size: 13.0,
                        color: if sel { theme::bg() } else { theme::text_dim() },
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
            section_label(g, fx, y, "File tree by default");
            y += 24.0;
            help_text(g, fx, y, "시작할 때 파일 트리 사이드바 열기");
            y += 28.0;
            let tr = (fx, y, 52.0, 30.0);
            toggle(g, tr, ctx.file_tree_default, ctx.cursor);
            rects.push((SettingsAction::ToggleFileTree, tr));
            y += 30.0 + ROW_GAP;
            // pane 하단바 기본 표시
            section_label(g, fx, y, "Pane status bar by default");
            y += 24.0;
            help_text(g, fx, y, "각 pane 아래 경로 · 브랜치 · diff 바 표시");
            y += 28.0;
            let fr = (fx, y, 52.0, 30.0);
            toggle(g, fr, ctx.footer_default, ctx.cursor);
            rects.push((SettingsAction::ToggleFooter, fr));
            y += 30.0 + ROW_GAP;
            // 윈도우 탭 위치
            section_label(g, fx, y, "Tab position");
            y += 24.0;
            help_text(g, fx, y, "윈도우 탭을 상단 타이틀바 또는 좌측 사이드바에 표시");
            y += 26.0;
            let tab_segs: [(&'static str, &str); 2] = [("top", "Top"), ("side", "Side")];
            let mut sx = fx;
            for (val, label) in tab_segs {
                let tw = g.measure_chrome_text(label, 13.0, false) + 28.0;
                let r = (sx, y, tw, 32.0);
                let sel = (val == "top") == ctx.tabs_on_top;
                let hover = inside(r, ctx.cursor);
                round_rect(
                    g, r.0, r.1, r.2, r.3, theme::RADIUS_MD,
                    if sel { theme::accent() } else if hover { theme::surface_hover() } else { theme::surface_active() },
                );
                g.draw_text(
                    r.0 + 14.0,
                    r.1 + 8.0,
                    label,
                    gpu::DrawOpts {
                        font_size: 13.0,
                        color: if sel { theme::bg() } else { theme::text_dim() },
                        bold: sel,
                        italic: false,
                    },
                );
                rects.push((SettingsAction::TabPosition(val), r));
                sx += tw + 8.0;
            }
        }
        SettingsCat::Appearance => {
            let mut y = fy;
            // 테마 — 프리셋 카드 그리드. 카드 하나 = 그 팔레트의 미니 프리뷰
            // (bg 칠 + 프롬프트 샘플 + ANSI 도트 + 라벨)라서 고르기 전에 색이
            // 보인다. UI 토큰과 터미널 ANSI 16색이 함께 바뀐다.
            section_label(g, fx, y, "Theme");
            y += 24.0;
            help_text(g, fx, y, "UI + 터미널 ANSI 팔레트가 함께 바뀌어요");
            y += 30.0;
            let (card_w, card_h, gap) = (158.0_f32, 96.0_f32, 12.0_f32);
            let per_row = (((fw + gap) / (card_w + gap)).floor() as usize).max(1);
            let mut idx = 0usize;
            let mut card = |g: &mut gpu::GpuRenderer,
                            rects: &mut Vec<(SettingsAction, Rect)>,
                            key: &'static str,
                            label: &str,
                            pal: Option<&theme::Palette>| {
                let col = idx % per_row;
                let row = idx / per_row;
                let x = fx + col as f32 * (card_w + gap);
                let cy = y + row as f32 * (card_h + gap);
                let r = (x, cy, card_w, card_h);
                let sel = ctx.theme == key;
                let hover = inside(r, ctx.cursor);
                // Selection / hover ring — a slightly larger plate behind the
                // card (same halo trick as the accent swatches).
                if sel {
                    round_rect(g, x - 2.0, cy - 2.0, card_w + 4.0, card_h + 4.0, theme::RADIUS_MD + 2.0, theme::accent());
                } else if hover {
                    round_rect(g, x - 2.0, cy - 2.0, card_w + 4.0, card_h + 4.0, theme::RADIUS_MD + 2.0, theme::surface_hover());
                }
                // Custom card has no static palette — preview with the live
                // colors instead (it IS the applied palette when selected).
                let (bg, text, dim, ansi) = match pal {
                    Some(p) => (p.bg, p.text, p.text_mute, p.ansi),
                    None => {
                        let mut a = [[0u8; 3]; 16];
                        for (i, s) in a.iter_mut().enumerate() {
                            *s = theme::ansi16(i);
                        }
                        (theme::bg(), theme::text(), theme::text_mute(), a)
                    }
                };
                round_rect(g, x, cy, card_w, card_h, theme::RADIUS_MD, bg);
                // Prompt sample in the theme's own text color.
                g.draw_text(
                    x + 12.0, cy + 12.0, "❯ ls -la",
                    gpu::DrawOpts { font_size: 12.0, color: text, bold: false, italic: false },
                );
                // ANSI 1..=6 dots (red green yellow blue magenta cyan).
                for i in 0..6 {
                    let c = ansi[i + 1];
                    round_rect(
                        g, x + 12.0 + i as f32 * 16.0, cy + 36.0, 10.0, 10.0, 5.0,
                        [c[0], c[1], c[2], 255],
                    );
                }
                g.draw_text(
                    x + 12.0, cy + card_h - 26.0, label,
                    gpu::DrawOpts { font_size: 12.0, color: dim, bold: sel, italic: false },
                );
                rects.push((SettingsAction::ThemeMode(key), r));
                idx += 1;
            };
            for (key, label, pal) in theme::THEME_PRESETS {
                card(g, &mut rects, key, label, Some(pal));
            }
            if ctx.has_custom_theme {
                card(g, &mut rects, "custom", "Custom (settings.json)", None);
            }
            let rows = idx.div_ceil(per_row);
            y += rows as f32 * (card_h + gap) + ROW_GAP;
            // 강조색
            section_label(g, fx, y, "Accent color");
            y += 24.0;
            help_text(g, fx, y, "선택 영역 · 커서 · 링크 색");
            y += 32.0;
            let mut cxp = fx;
            for (name, col) in theme::ACCENT_PRESETS {
                let sz = 30.0_f32;
                let r = (cxp, y, sz, sz);
                let sel = *name == ctx.accent;
                // Selected ring: a slightly larger text-colored disc behind the
                // swatch reads as a halo.
                if sel {
                    round_rect(g, r.0 - 3.0, r.1 - 3.0, sz + 6.0, sz + 6.0, (sz + 6.0) / 2.0, theme::text());
                }
                round_rect(g, r.0, r.1, sz, sz, sz / 2.0, *col);
                rects.push((SettingsAction::Accent(name.to_string()), r));
                cxp += sz + 14.0;
            }
            y += 30.0 + ROW_GAP;
            // 폰트 크기 스테퍼 — 값은 즉시 적용(그리드 리플로우)되고
            // settings.json 에 저장돼 재시작에도 유지된다.
            section_label(g, fx, y, "Font size");
            y += 24.0;
            help_text(g, fx, y, "터미널 셀 폰트 크기 (기본 16 · Cmd+/- 줌과 별개인 기준값)");
            y += 28.0;
            let bs = 30.0_f32;
            let minus = (fx, y, bs, bs);
            stepper_btn(g, minus, "minus", ctx.cursor);
            rects.push((SettingsAction::FontSizeDelta(-1), minus));
            let num = format!("{:.0}", ctx.font_size);
            let num_w = g.measure_chrome_text(&num, 15.0, true);
            let num_span = 52.0_f32;
            g.draw_text(
                fx + bs + (num_span - num_w) / 2.0,
                y + (bs - 15.0) / 2.0,
                &num,
                gpu::DrawOpts { font_size: 15.0, color: theme::text(), bold: true, italic: false },
            );
            let plus = (fx + bs + num_span, y, bs, bs);
            stepper_btn(g, plus, "plus", ctx.cursor);
            rects.push((SettingsAction::FontSizeDelta(1), plus));
        }
        SettingsCat::Shell => {
            let mut y = fy;
            section_label(g, fx, y, "Default shell");
            y += 24.0;
            help_text(g, fx, y, "새 pane 의 셸 (비우면 시스템 $SHELL)");
            y += 26.0;
            let presets: [(&str, &str); 3] =
                [("", "System default"), ("/bin/zsh", "zsh"), ("/bin/bash", "bash")];
            let shell_is_preset = presets.iter().any(|(v, _)| *v == ctx.shell);
            let mut sx = fx;
            for (val, label) in presets {
                let tw = g.measure_chrome_text(label, 13.0, false) + 28.0;
                let r = (sx, y, tw, 32.0);
                let sel = ctx.shell == val;
                let hover = inside(r, ctx.cursor);
                round_rect(
                    g, r.0, r.1, r.2, r.3, theme::RADIUS_MD,
                    if sel { theme::accent() } else if hover { theme::surface_hover() } else { theme::surface_active() },
                );
                g.draw_text(
                    r.0 + 14.0,
                    r.1 + 8.0,
                    label,
                    gpu::DrawOpts {
                        font_size: 13.0,
                        color: if sel { theme::bg() } else { theme::text_dim() },
                        bold: sel,
                        italic: false,
                    },
                );
                rects.push((SettingsAction::ShellPreset(val.to_string()), r));
                sx += tw + 8.0;
            }
            // "직접" chip → focuses the free-text field below.
            {
                let label = "Custom";
                let tw = g.measure_chrome_text(label, 13.0, false) + 28.0;
                let r = (sx, y, tw, 32.0);
                let sel = !shell_is_preset;
                let hover = inside(r, ctx.cursor);
                round_rect(
                    g, r.0, r.1, r.2, r.3, theme::RADIUS_MD,
                    if sel { theme::accent() } else if hover { theme::surface_hover() } else { theme::surface_active() },
                );
                g.draw_text(
                    r.0 + 14.0,
                    r.1 + 8.0,
                    label,
                    gpu::DrawOpts {
                        font_size: 13.0,
                        color: if sel { theme::bg() } else { theme::text_dim() },
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
        SettingsCat::Claude => {
            // Brand logo above the form — set by paint's Claude header block.
            let mut y = fy + claude_logo(g, fx, fy);
            // Shim injection — global. off = install_pane_shims never makes the shim
            // dir, so claude runs vanilla (no persona/proxy/hooks). Read once at boot,
            // so a change needs a restart.
            section_label(g, fx, y, "Shim injection");
            y += 24.0;
            help_text(g, fx, y, "끄면 순정 Claude — 페르소나 · 캡처 프록시 · 훅 전부 없음");
            y += 19.0;
            help_text(g, fx, y, "재시작해야 적용돼요 — 시작할 때 한 번만 설치돼서");
            y += 27.0;
            let sr = (fx, y, 52.0, 30.0);
            toggle(g, sr, ctx.shim_inject, ctx.cursor);
            rects.push((SettingsAction::ToggleShimInject, sr));
            y += 30.0 + ROW_GAP;
            // Persona injection (toggle)
            section_label(g, fx, y, "Persona injection");
            y += 24.0;
            help_text(g, fx, y, "이 pane 의 캐릭터를 Claude 시스템 프롬프트에 붙여요");
            y += 28.0;
            let pr = (fx, y, 52.0, 30.0);
            toggle(g, pr, ctx.claude_persona, ctx.cursor);
            rects.push((SettingsAction::ToggleClaudePersona, pr));
            y += 30.0 + ROW_GAP;
            // 모델 (세그먼트)
            section_label(g, fx, y, "Model");
            y += 24.0;
            help_text(g, fx, y, "Claude 모델 덮어쓰기 (Default = 원래대로 유지)");
            y += 26.0;
            let models: [(&str, &str); 4] =
                [("", "Default"), ("opus", "opus"), ("sonnet", "sonnet"), ("haiku", "haiku")];
            let mut sx = fx;
            for (val, label) in models {
                let tw = g.measure_chrome_text(label, 13.0, false) + 28.0;
                let r = (sx, y, tw, 32.0);
                let sel = ctx.claude_model == val;
                let hover = inside(r, ctx.cursor);
                round_rect(
                    g, r.0, r.1, r.2, r.3, theme::RADIUS_MD,
                    if sel { theme::accent() } else if hover { theme::surface_hover() } else { theme::surface_active() },
                );
                g.draw_text(
                    r.0 + 14.0, r.1 + 8.0, label,
                    gpu::DrawOpts { font_size: 13.0, color: if sel { theme::bg() } else { theme::text_dim() }, bold: sel, italic: false },
                );
                rects.push((SettingsAction::ClaudeModel(val.to_string()), r));
                sx += tw + 8.0;
            }
            y += 44.0 + ROW_GAP;
            // Effort (세그먼트) — CLAUDE_EFFORT env
            section_label(g, fx, y, "Effort");
            y += 24.0;
            help_text(g, fx, y, "추론 강도 (CLAUDE_EFFORT). Default = 그대로 둠");
            y += 26.0;
            let efforts: [(&str, &str); 5] =
                [("", "Default"), ("low", "low"), ("medium", "medium"), ("high", "high"), ("xhigh", "xhigh")];
            let mut sx = fx;
            for (val, label) in efforts {
                let tw = g.measure_chrome_text(label, 13.0, false) + 28.0;
                let r = (sx, y, tw, 32.0);
                let sel = ctx.claude_effort == val;
                let hover = inside(r, ctx.cursor);
                round_rect(
                    g, r.0, r.1, r.2, r.3, theme::RADIUS_MD,
                    if sel { theme::accent() } else if hover { theme::surface_hover() } else { theme::surface_active() },
                );
                g.draw_text(
                    r.0 + 14.0, r.1 + 8.0, label,
                    gpu::DrawOpts { font_size: 13.0, color: if sel { theme::bg() } else { theme::text_dim() }, bold: sel, italic: false },
                );
                rects.push((SettingsAction::ClaudeEffort(val.to_string()), r));
                sx += tw + 8.0;
            }
            y += 44.0 + ROW_GAP;
            // 추가 인자 (텍스트) — 매 실행 덧붙이는 자유 플래그
            section_label(g, fx, y, "Extra args");
            y += 24.0;
            help_text(g, fx, y, "claude 실행에 항상 붙는 플래그 (예: --verbose)");
            y += 28.0;
            let r = (fx, y, fw.min(420.0), 34.0);
            let focused = ctx.input == Some(SettingsInput::ClaudeExtra);
            text_field(g, r, &ctx.claude_extra, focused, ctx.caret_on, ctx.cursor);
            rects.push((SettingsAction::FocusClaudeExtra, r));
        }
    }

    rects
}

/// Claude brand header for the Claude settings tab — sunburst mark + wordmark.
/// Drawn through the chrome icon layer (queue_icon) so it sits over the panel.
/// Returns the vertical space consumed so the caller can offset the first
/// section below it.
fn claude_logo(g: &mut gpu::GpuRenderer, x: f32, y: f32) -> f32 {
    // queue_icon draws through the chrome icon layer (over the settings panel);
    // queue_image would paint beneath it and get covered. Tinted Claude orange.
    let size = 30.0;
    g.queue_icon("claude", x, y, size, [217, 119, 87, 255]);
    g.draw_text(
        x + size + 12.0,
        y + (size - 22.0) / 2.0,
        "Claude",
        gpu::DrawOpts { font_size: 22.0, color: theme::text(), bold: true, italic: false },
    );
    size + 20.0
}

fn section_label(g: &mut gpu::GpuRenderer, x: f32, y: f32, text: &str) {
    g.draw_text(
        x,
        y,
        text,
        gpu::DrawOpts { font_size: 15.0, color: theme::text(), bold: true, italic: false },
    );
}

fn help_text(g: &mut gpu::GpuRenderer, x: f32, y: f32, text: &str) {
    g.draw_text(
        x,
        y,
        text,
        gpu::DrawOpts { font_size: 12.0, color: theme::text_mute(), bold: false, italic: false },
    );
}

/// Square icon button for the font-size stepper (− / +).
fn stepper_btn(g: &mut gpu::GpuRenderer, r: Rect, glyph: &str, cursor: (f32, f32)) {
    let hover = inside(r, cursor);
    round_rect(
        g, r.0, r.1, r.2, r.3, theme::RADIUS_SM,
        if hover { theme::surface_hover() } else { theme::surface_active() },
    );
    let isz = 14.0;
    g.queue_icon(
        glyph,
        r.0 + (r.2 - isz) / 2.0,
        r.1 + (r.3 - isz) / 2.0,
        isz,
        if hover { theme::text() } else { theme::text_dim() },
    );
}

fn toggle(g: &mut gpu::GpuRenderer, r: Rect, on: bool, cursor: (f32, f32)) {
    let hover = inside(r, cursor);
    let track = if on {
        theme::accent()
    } else if hover {
        theme::surface_hover()
    } else {
        theme::surface_active()
    };
    round_rect(g, r.0, r.1, r.2, r.3, r.3 / 2.0, track);
    let knob = r.3 - 8.0;
    let kx = if on { r.0 + r.2 - knob - 4.0 } else { r.0 + 4.0 };
    round_rect(g, kx, r.1 + 4.0, knob, knob, knob / 2.0, theme::text());
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
    round_rect(g, r.0, r.1, r.2, r.3, theme::RADIUS_SM, theme::surface_active());
    let border = if focused {
        theme::accent()
    } else if hover {
        theme::text_mute()
    } else {
        theme::border()
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
        gpu::DrawOpts { font_size: 13.0, color: theme::text(), bold: false, italic: false },
    );
    if focused && caret_on {
        g.rect(tx + adv + 1.0, r.1 + 7.0, 1.5, r.3 - 14.0, theme::text());
    }
}
