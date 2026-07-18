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
    /// Form scroll offset in logical px (wheel-driven). 0 = top.
    pub scroll: f32,
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
    /// (표시명, 에셋 슬러그) — Students 카테고리 목록·프사 썸네일용. slug 가
    /// None 이면 아직 도트 에셋이 없는 캐릭터(썸네일 자리표시).
    pub characters: Vec<(String, Option<&'static str>)>,
    /// Students 인라인 편집 — 선택 캐릭터·persona 버퍼·캐럿(문자 인덱스).
    pub student_selected: Option<String>,
    pub student_persona: String,
    pub student_caret: usize,
}

fn inside(r: Rect, p: (f32, f32)) -> bool {
    p.0 >= r.0 && p.0 <= r.0 + r.2 && p.1 >= r.1 && p.1 <= r.1 + r.3
}

/// 문자열+캐럿 편집 코어 — 캐럿은 문자(char) 인덱스. 단일·멀티라인 공용이라
/// persona 멀티라인 편집과 아리스의 단일라인 캐럿 개선(P4-12)이 같이 쓴다.
/// 모든 조작은 캐럿을 유효 범위로 유지한다.
pub(crate) mod textedit {
    /// char 인덱스 → byte offset(경계 밖이면 문자열 끝). 한글 등 멀티바이트 안전.
    fn char_byte(s: &str, ci: usize) -> usize {
        s.char_indices().nth(ci).map(|(b, _)| b).unwrap_or(s.len())
    }
    pub fn insert(buf: &mut String, caret: &mut usize, ch: char) {
        let b = char_byte(buf, *caret);
        buf.insert(b, ch);
        *caret += 1;
    }
    pub fn backspace(buf: &mut String, caret: &mut usize) {
        if *caret == 0 {
            return;
        }
        let b0 = char_byte(buf, *caret - 1);
        let b1 = char_byte(buf, *caret);
        buf.replace_range(b0..b1, "");
        *caret -= 1;
    }
    pub fn left(caret: &mut usize) {
        *caret = caret.saturating_sub(1);
    }
    pub fn right(buf: &str, caret: &mut usize) {
        if *caret < buf.chars().count() {
            *caret += 1;
        }
    }
}

/// persona 평문을 편집 박스 폭에 맞춰 시각 라인들로 접는다(word-wrap).
/// 각 원소 = (그 시각 라인 문자열, 그 라인 첫 글자의 전역 char 인덱스).
/// '\n' 은 강제 개행, 그 외엔 폭 초과 시 공백 우선(없으면 글자 단위) 분할.
/// 빈 텍스트도 시각 라인 1개(빈 줄)를 돌려준다 — 캐럿 그릴 자리가 필요하다.
/// 평문 persona 는 보통 줄바꿈 없는 긴 문단이라 wrap 없이는 박스 밖으로 잘린다.
fn wrap_persona(
    g: &mut gpu::GpuRenderer,
    text: &str,
    max_w: f32,
    font: f32,
) -> Vec<(String, usize)> {
    let mut out: Vec<(String, usize)> = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0.0f32;
    let mut cur_start = 0usize;
    let mut sp_at: Option<usize> = None; // cur 내 '공백 다음' char 수(분할점)
    let mut sp_w = 0.0f32; // 그 분할점까지의 폭
    for (gi, ch) in text.chars().enumerate() {
        if ch == '\n' {
            out.push((std::mem::take(&mut cur), cur_start));
            cur_w = 0.0;
            cur_start = gi + 1;
            sp_at = None;
            sp_w = 0.0;
            continue;
        }
        let cw = g.measure_chrome_text(&ch.to_string(), font, false);
        if cur_w + cw > max_w && !cur.is_empty() {
            if let Some(wa) = sp_at {
                // 공백 경계에서 접기 — head 는 확정, tail 은 다음 줄로 이어간다.
                let head: String = cur.chars().take(wa).collect();
                let tail: String = cur.chars().skip(wa).collect();
                out.push((head, cur_start));
                cur_start += wa;
                cur_w -= sp_w;
                cur = tail;
            } else {
                // 공백 없는 긴 토막(한글 등) — 글자 단위 강제 개행.
                out.push((std::mem::take(&mut cur), cur_start));
                cur_start = gi;
                cur_w = 0.0;
            }
            sp_at = None;
            sp_w = 0.0;
        }
        cur.push(ch);
        cur_w += cw;
        if ch == ' ' {
            sp_at = Some(cur.chars().count());
            sp_w = cur_w;
        }
    }
    out.push((cur, cur_start));
    out
}

/// wrap 결과에서 캐럿의 (시각 라인 번호, 라인 내 열)을 찾는다. 소프트 wrap
/// 경계(공백/글자 접힘)에선 아랫줄 맨 앞에 둔다 — 강제 '\n' 경계에선 윗줄 끝에.
fn persona_visual_caret(vis: &[(String, usize)], caret: usize) -> (usize, usize) {
    let mut result = (
        vis.len().saturating_sub(1),
        vis.last().map(|(s, _)| s.chars().count()).unwrap_or(0),
    );
    for (i, (s, start)) in vis.iter().enumerate() {
        let len = s.chars().count();
        let end = start + len;
        if caret < *start {
            break;
        }
        if caret <= end {
            let soft_next = i + 1 < vis.len() && vis[i + 1].1 == end;
            if caret == end && soft_next {
                continue; // 접힌 줄 끝 == 다음 줄 앞: 아랫줄에 그린다
            }
            result = (i, caret - start);
            break;
        }
    }
    result
}

/// 경로를 OS 기본 앱/파일 매니저로 연다(폴더=매니저, 파일=연결 앱).
fn open_path(path: &std::path::Path) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(path).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer").arg(path).spawn();
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
        self.settings_scroll = 0.0;
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
        self.flush_student_persona();
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

    /// 학생 이미지 override 폴더(`~/.config/kasaterm/students/`)를 OS 파일
    /// 매니저로 연다 — 없으면 먼저 만든다.
    fn open_students_dir(&self) {
        if let Some(dir) = socket::students_dir() {
            let _ = std::fs::create_dir_all(&dir);
            open_path(&dir);
        }
    }

    /// characters.json(사용자 override 슬롯)을 기본 앱으로 연다. 아직 없으면
    /// 현재 활성 정본을 그 자리에 복사해 seed 한다 — 빈 파일 대신 채워진 걸 편집.
    fn open_characters_json(&self) {
        let Some(home) = std::env::var_os("HOME") else { return };
        let p = std::path::PathBuf::from(home).join(".config/kasaterm/characters.json");
        if !p.exists() {
            if let Some(parent) = p.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Some(v) = kasa_mcp::character::characters_json() {
                if let Ok(txt) = serde_json::to_string_pretty(&v) {
                    let _ = std::fs::write(&p, txt);
                }
            }
        }
        open_path(&p);
    }

    /// 편집된 학생 이미지가 다음 페인트에 다시 로드되도록 캐시된 캐릭터 텍스처를
    /// 통째로 evict 한다 — upload_image 는 키가 있으면 no-op 이라 이 축출 없이는
    /// 교체가 화면에 안 먹는다.
    fn refresh_student_assets(&mut self) {
        if let Some(g) = self.gpu.as_mut() {
            g.drop_images_with_prefix("student:");
            g.drop_image("schale:logo");
        }
        self.repaint_all();
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
            scroll: self.settings_scroll,
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
            characters: kasa_mcp::character::characters_json()
                .map(|c| {
                    kasa_mcp::character::member_names(&c)
                        .into_iter()
                        .map(|n| {
                            let slug = theme::character_slug(&n);
                            (n, slug)
                        })
                        .collect()
                })
                .unwrap_or_default(),
            student_selected: self.students_selected.clone(),
            student_persona: self.students_persona.clone(),
            student_caret: self.students_caret,
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
            self.flush_student_persona();
            self.settings_input = None;
            self.chrome_dirty = true;
            return true;
        };
        match action {
            SettingsAction::Category(c) => {
                self.flush_student_persona();
                self.settings_cat = c;
                self.settings_input = None;
                // 페이지가 바뀌면 스크롤은 맨 위부터.
                self.settings_scroll = 0.0;
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
            SettingsAction::OpenStudentsDir => self.open_students_dir(),
            SettingsAction::OpenCharactersJson => self.open_characters_json(),
            SettingsAction::RefreshStudentAssets => self.refresh_student_assets(),
            SettingsAction::SelectStudent(name) => {
                // 다른 캐릭터를 편집 중이었으면 먼저 저장하고, 새 캐릭터의 원본
                // persona 를 버퍼로 로드. 캐럿은 끝으로.
                self.flush_student_persona();
                let persona = kasa_mcp::character::characters_json()
                    .and_then(|c| kasa_mcp::character::raw_persona_for(&c, &name))
                    .unwrap_or_default();
                self.students_caret = persona.chars().count();
                self.students_persona = persona;
                self.students_selected = Some(name);
                self.settings_input = Some(SettingsInput::StudentPersona);
            }
            SettingsAction::FocusStudentPersona => {
                self.settings_input = Some(SettingsInput::StudentPersona);
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
        // persona 는 캐럿·개행이 있는 멀티라인이라 단일라인 append 경로와 분리.
        if field == SettingsInput::StudentPersona {
            return self.student_persona_key(event);
        }
        let buf = match field {
            SettingsInput::CwdPath => &mut self.set_cwd_mode,
            SettingsInput::Shell => &mut self.set_shell,
            SettingsInput::ClaudeExtra => &mut self.set_claude_extra,
            SettingsInput::StudentPersona => unreachable!("handled above"),
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

    /// persona 멀티라인 편집 키 라우팅. Enter=개행, Esc=저장+blur, 방향키 좌우로
    /// 캐럿 이동, 문자/Space 삽입, Backspace 삭제. 저장은 blur 시(flush)라 매 키
    /// characters.json 쓰기는 하지 않는다.
    fn student_persona_key(&mut self, event: &winit::event::KeyEvent) -> bool {
        use winit::event::ElementState;
        use winit::keyboard::{Key, NamedKey};
        if event.state != ElementState::Pressed {
            return true;
        }
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                self.flush_student_persona();
                self.settings_input = None;
            }
            Key::Named(NamedKey::Enter) => {
                textedit::insert(&mut self.students_persona, &mut self.students_caret, '\n');
            }
            Key::Named(NamedKey::Backspace) => {
                textedit::backspace(&mut self.students_persona, &mut self.students_caret);
            }
            Key::Named(NamedKey::ArrowLeft) => textedit::left(&mut self.students_caret),
            Key::Named(NamedKey::ArrowRight) => {
                textedit::right(&self.students_persona, &mut self.students_caret)
            }
            Key::Named(NamedKey::Space) => {
                textedit::insert(&mut self.students_persona, &mut self.students_caret, ' ');
            }
            Key::Character(t) => {
                for ch in t.chars() {
                    textedit::insert(&mut self.students_persona, &mut self.students_caret, ch);
                }
            }
            _ => return true,
        }
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
        true
    }

    /// persona 편집 버퍼를 characters.json 에 저장(선택 캐릭터가 있고 실제로
    /// 바뀌었을 때만). blur·선택 변경·설정 닫기 시 호출. 저장 후 shim 을 재생성해
    /// 그 캐릭터 pane 의 다음 claude 실행이 새 persona 를 집게 한다.
    fn flush_student_persona(&mut self) {
        let Some(name) = self.students_selected.clone() else { return };
        let cur = kasa_mcp::character::characters_json()
            .and_then(|c| kasa_mcp::character::raw_persona_for(&c, &name))
            .unwrap_or_default();
        if cur == self.students_persona {
            return;
        }
        let _ = kasa_mcp::character::update_member(
            &name,
            "persona",
            serde_json::Value::String(self.students_persona.clone()),
        );
        self.regen_claude_shim();
    }
}

/// Paint the full settings screen. Returns the frame's clickable rects plus
/// the unscrolled form-content height (px) so the caller can clamp the wheel
/// scroll. Free function so it can run inside the `self.gpu.as_mut()` borrow.
pub(crate) fn paint_settings(
    g: &mut gpu::GpuRenderer,
    ctx: &SettingsCtx,
) -> (Vec<(SettingsAction, Rect)>, f32) {
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
        (SettingsCat::Students, "Students", "users"),
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
    // ── Scrollable form ── the wheel shifts everything below the page header
    // up by ctx.scroll. The renderer has no scissor, so the coarse clip rule
    // is: a control whose TOP is above the header hairline isn't painted at
    // all (and its rect isn't pushed, so it isn't clickable either). Popping
    // whole controls at the boundary beats controls bleeding over the header
    // and title bar.
    let fy = ay + 84.0 - ctx.scroll;
    let clip = ay + 66.0;
    // Every match arm must set this to its last element's bottom edge — the
    // compiler enforces it (no initializer), so a new category can't silently
    // break the scroll clamp.
    let content_bottom: f32;
    match ctx.cat {
        SettingsCat::General => {
            let mut y = fy;
            // 시작 작업 폴더
            if y > clip {
                section_label(g, fx, y, "Startup folder");
            }
            y += 24.0;
            if y > clip {
                help_text(g, fx, y, "새 창과 탭이 열리는 위치");
            }
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
            if y > clip {
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
            }
            y += 44.0;
            // Custom path field, only when "직접 지정" is active.
            if cwd_is("custom") {
                if y > clip {
                    let r = (fx, y, fw.min(420.0), 34.0);
                    let focused = ctx.input == Some(SettingsInput::CwdPath);
                    text_field(g, r, &ctx.cwd_mode, focused, ctx.caret_on, ctx.cursor);
                    rects.push((SettingsAction::FocusCwdPath, r));
                }
                y += 34.0;
            }
            y += ROW_GAP;
            // 파일트리 기본 표시
            if y > clip {
                section_label(g, fx, y, "File tree by default");
            }
            y += 24.0;
            if y > clip {
                help_text(g, fx, y, "시작할 때 파일 트리 사이드바 열기");
            }
            y += 28.0;
            if y > clip {
                let tr = (fx, y, 52.0, 30.0);
                toggle(g, tr, ctx.file_tree_default, ctx.cursor);
                rects.push((SettingsAction::ToggleFileTree, tr));
            }
            y += 30.0 + ROW_GAP;
            // pane 하단바 기본 표시
            if y > clip {
                section_label(g, fx, y, "Pane status bar by default");
            }
            y += 24.0;
            if y > clip {
                help_text(g, fx, y, "각 pane 아래 경로 · 브랜치 · diff 바 표시");
            }
            y += 28.0;
            if y > clip {
                let fr = (fx, y, 52.0, 30.0);
                toggle(g, fr, ctx.footer_default, ctx.cursor);
                rects.push((SettingsAction::ToggleFooter, fr));
            }
            y += 30.0 + ROW_GAP;
            // 윈도우 탭 위치
            if y > clip {
                section_label(g, fx, y, "Tab position");
            }
            y += 24.0;
            if y > clip {
                help_text(g, fx, y, "윈도우 탭을 상단 타이틀바 또는 좌측 사이드바에 표시");
            }
            y += 26.0;
            let tab_segs: [(&'static str, &str); 2] = [("top", "Top"), ("side", "Side")];
            if y > clip {
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
            content_bottom = y + 32.0;
        }
        SettingsCat::Appearance => {
            let mut y = fy;
            // 테마 — 프리셋 카드 그리드. 카드 하나 = 그 팔레트의 미니 프리뷰
            // (bg 칠 + 프롬프트 샘플 + ANSI 도트 + 라벨)라서 고르기 전에 색이
            // 보인다. UI 토큰과 터미널 ANSI 16색이 함께 바뀐다.
            if y > clip {
                section_label(g, fx, y, "Theme");
            }
            y += 24.0;
            if y > clip {
                help_text(g, fx, y, "UI + 터미널 ANSI 팔레트가 함께 바뀌어요");
            }
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
                idx += 1;
                // 스크롤로 헤더 위로 올라간 카드는 통째로 스킵(coarse clip) —
                // idx 는 이미 올렸으니 그리드 자리는 유지된다.
                if cy <= clip {
                    return;
                }
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
            if y > clip {
                section_label(g, fx, y, "Accent color");
            }
            y += 24.0;
            if y > clip {
                help_text(g, fx, y, "선택 영역 · 커서 · 링크 색");
            }
            y += 32.0;
            if y > clip {
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
            }
            y += 30.0 + ROW_GAP;
            // 폰트 크기 스테퍼 — 값은 즉시 적용(그리드 리플로우)되고
            // settings.json 에 저장돼 재시작에도 유지된다.
            if y > clip {
                section_label(g, fx, y, "Font size");
            }
            y += 24.0;
            if y > clip {
                help_text(g, fx, y, "터미널 셀 폰트 크기 (기본 16 · Cmd+/- 줌과 별개인 기준값)");
            }
            y += 28.0;
            if y > clip {
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
            content_bottom = y + 30.0;
        }
        SettingsCat::Shell => {
            let mut y = fy;
            if y > clip {
                section_label(g, fx, y, "Default shell");
            }
            y += 24.0;
            if y > clip {
                help_text(g, fx, y, "새 pane 의 셸 (비우면 시스템 $SHELL)");
            }
            y += 26.0;
            let presets: [(&str, &str); 3] =
                [("", "System default"), ("/bin/zsh", "zsh"), ("/bin/bash", "bash")];
            let shell_is_preset = presets.iter().any(|(v, _)| *v == ctx.shell);
            if y > clip {
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
            }
            y += 44.0;
            if !shell_is_preset {
                if y > clip {
                    let r = (fx, y, fw.min(420.0), 34.0);
                    let focused = ctx.input == Some(SettingsInput::Shell);
                    text_field(g, r, &ctx.shell, focused, ctx.caret_on, ctx.cursor);
                    rects.push((SettingsAction::FocusShell, r));
                }
                y += 34.0;
            }
            content_bottom = y;
        }
        SettingsCat::Claude => {
            // Brand logo above the form — fixed 50px tall (size 30 + 20 gap),
            // so the hidden branch advances y by the same amount.
            let mut y = fy
                + if fy > clip {
                    claude_logo(g, fx, fy)
                } else {
                    50.0
                };
            // Shim injection — global. off = install_pane_shims never makes the shim
            // dir, so claude runs vanilla (no persona/proxy/hooks). Read once at boot,
            // so a change needs a restart.
            if y > clip {
                section_label(g, fx, y, "Shim injection");
            }
            y += 24.0;
            if y > clip {
                help_text(g, fx, y, "끄면 순정 Claude — 페르소나 · 캡처 프록시 · 훅 전부 없음");
            }
            y += 19.0;
            if y > clip {
                help_text(g, fx, y, "재시작해야 적용돼요 — 시작할 때 한 번만 설치돼서");
            }
            y += 27.0;
            if y > clip {
                let sr = (fx, y, 52.0, 30.0);
                toggle(g, sr, ctx.shim_inject, ctx.cursor);
                rects.push((SettingsAction::ToggleShimInject, sr));
            }
            y += 30.0 + ROW_GAP;
            // Persona injection (toggle)
            if y > clip {
                section_label(g, fx, y, "Persona injection");
            }
            y += 24.0;
            if y > clip {
                help_text(g, fx, y, "이 pane 의 캐릭터를 Claude 시스템 프롬프트에 붙여요");
            }
            y += 28.0;
            if y > clip {
                let pr = (fx, y, 52.0, 30.0);
                toggle(g, pr, ctx.claude_persona, ctx.cursor);
                rects.push((SettingsAction::ToggleClaudePersona, pr));
            }
            y += 30.0 + ROW_GAP;
            // 모델 (세그먼트)
            if y > clip {
                section_label(g, fx, y, "Model");
            }
            y += 24.0;
            if y > clip {
                help_text(g, fx, y, "Claude 모델 덮어쓰기 (Default = 원래대로 유지)");
            }
            y += 26.0;
            let models: [(&str, &str); 4] =
                [("", "Default"), ("opus", "opus"), ("sonnet", "sonnet"), ("haiku", "haiku")];
            if y > clip {
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
            }
            y += 44.0 + ROW_GAP;
            // Effort (세그먼트) — CLAUDE_EFFORT env
            if y > clip {
                section_label(g, fx, y, "Effort");
            }
            y += 24.0;
            if y > clip {
                help_text(g, fx, y, "추론 강도 (CLAUDE_EFFORT). Default = 그대로 둠");
            }
            y += 26.0;
            let efforts: [(&str, &str); 5] =
                [("", "Default"), ("low", "low"), ("medium", "medium"), ("high", "high"), ("xhigh", "xhigh")];
            if y > clip {
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
            }
            y += 44.0 + ROW_GAP;
            // 추가 인자 (텍스트) — 매 실행 덧붙이는 자유 플래그
            if y > clip {
                section_label(g, fx, y, "Extra args");
            }
            y += 24.0;
            if y > clip {
                help_text(g, fx, y, "claude 실행에 항상 붙는 플래그 (예: --verbose)");
            }
            y += 28.0;
            if y > clip {
                let r = (fx, y, fw.min(420.0), 34.0);
                let focused = ctx.input == Some(SettingsInput::ClaudeExtra);
                text_field(g, r, &ctx.claude_extra, focused, ctx.caret_on, ctx.cursor);
                rects.push((SettingsAction::FocusClaudeExtra, r));
            }
            content_bottom = y + 34.0;
        }
        SettingsCat::Students => {
            let mut y = fy;
            if y > clip {
                section_label(g, fx, y, "Character images");
            }
            y += 24.0;
            if y > clip {
                help_text(g, fx, y, "~/.config/kasaterm/students/ 에 이미지를 넣으면 학생 그림이 바뀌어요");
            }
            y += 22.0;
            if y > clip {
                help_text(g, fx, y, "파일명: <slug>-profile.png · <slug>-0..3.png · <slug>-walk-0..5.png · schale-logo.png");
            }
            y += 32.0;
            // 액션 버튼 3개 — 폴더 열기 / json 열기 / 새로고침(텍스처 재로드).
            if y > clip {
                let mut bx = fx;
                for (label, action) in [
                    ("이미지 폴더 열기", SettingsAction::OpenStudentsDir),
                    ("characters.json 열기", SettingsAction::OpenCharactersJson),
                    ("새로고침", SettingsAction::RefreshStudentAssets),
                ] {
                    let bw = g.measure_chrome_text(label, 13.0, false) + 28.0;
                    let r = (bx, y, bw, 34.0);
                    let hover = inside(r, ctx.cursor);
                    round_rect(
                        g, r.0, r.1, r.2, r.3, theme::RADIUS_MD,
                        if hover { theme::surface_hover() } else { theme::surface_active() },
                    );
                    g.draw_text(
                        r.0 + 14.0, r.1 + 9.0, label,
                        gpu::DrawOpts { font_size: 13.0, color: theme::text(), bold: false, italic: false },
                    );
                    rects.push((action, r));
                    bx += bw + 8.0;
                }
            }
            y += 34.0 + ROW_GAP;
            if y > clip {
                section_label(g, fx, y, "Characters");
            }
            y += 24.0;
            if y > clip {
                help_text(g, fx, y, "캐릭터를 눌러 성격(persona)을 바로 편집하세요");
            }
            y += 30.0;
            // 색 점 + 이름 + slug 한 줄, 행 전체가 클릭 대상(→ persona 편집). 실제
            // 프사·전신은 statusline·배너에서 보인다(설정 오버레이는 배경 rect 가
            // 이미지 z-order 를 가려 인라인 썸네일이 안 뜸). 스크롤로 전 인원 도달.
            let row_h = 30.0_f32;
            for (name, slug) in &ctx.characters {
                if y > clip {
                    let r = (fx - 6.0, y - 2.0, fw.min(380.0), row_h);
                    let selected = ctx.student_selected.as_deref() == Some(name.as_str());
                    let hover = inside(r, ctx.cursor);
                    if selected {
                        round_rect(g, r.0, r.1, r.2, r.3, theme::RADIUS_SM, theme::surface_active());
                    } else if hover {
                        round_rect(g, r.0, r.1, r.2, r.3, theme::RADIUS_SM, theme::surface_hover());
                    }
                    let sw = theme::character_accent(name).unwrap_or([128, 128, 128, 255]);
                    round_rect(g, fx, y + 3.0, 14.0, 14.0, theme::RADIUS_SM, sw);
                    g.draw_text(
                        fx + 24.0, y, name,
                        gpu::DrawOpts { font_size: 14.0, color: theme::text(), bold: selected, italic: false },
                    );
                    let nw = g.measure_chrome_text(name, 14.0, false);
                    g.draw_text(
                        fx + 24.0 + nw + 12.0, y + 2.0, slug.unwrap_or("(에셋 없음)"),
                        gpu::DrawOpts { font_size: 11.0, color: theme::text_mute(), bold: false, italic: false },
                    );
                    rects.push((SettingsAction::SelectStudent(name.clone()), r));
                }
                y += row_h;
            }
            // 선택된 캐릭터의 persona 멀티라인 편집기 — 줄 단위 클립.
            if let Some(sel) = &ctx.student_selected {
                y += ROW_GAP;
                if y > clip {
                    section_label(g, fx, y, &format!("{sel} · persona"));
                }
                y += 24.0;
                if y > clip {
                    help_text(g, fx, y, "성격·말투를 평문으로. Enter=줄바꿈, 바깥 클릭·Esc=저장");
                }
                y += 26.0;
                let box_w = fw.min(560.0);
                let line_h = 18.0_f32;
                let vis = wrap_persona(g, &ctx.student_persona, box_w - 24.0, 13.0);
                let box_h = (vis.len() as f32 * line_h + 16.0).max(56.0);
                let focused = ctx.input == Some(SettingsInput::StudentPersona);
                if y < ay + ah && y + box_h > clip {
                    let bg = if focused { theme::surface_hover() } else { theme::surface_active() };
                    round_rect(g, fx, y, box_w, box_h, theme::RADIUS_MD, bg);
                    rects.push((SettingsAction::FocusStudentPersona, (fx, y, box_w, box_h)));
                }
                let (caret_vl, caret_col) = persona_visual_caret(&vis, ctx.student_caret);
                let mut ly = y + 9.0;
                for (vi, (line, _)) in vis.iter().enumerate() {
                    if ly > clip && ly < ay + ah - line_h {
                        g.draw_text(
                            fx + 12.0, ly, line,
                            gpu::DrawOpts { font_size: 13.0, color: theme::text(), bold: false, italic: false },
                        );
                        if focused && ctx.caret_on && vi == caret_vl {
                            let pre: String = line.chars().take(caret_col).collect();
                            let cx = fx + 12.0 + g.measure_chrome_text(&pre, 13.0, false);
                            g.rect(cx, ly, 1.5, line_h - 2.0, theme::accent());
                        }
                    }
                    ly += line_h;
                }
                y += box_h;
            }
            content_bottom = y;
        }
    }

    (rects, content_bottom - fy)
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

#[cfg(test)]
mod textedit_tests {
    use super::textedit::*;

    #[test]
    fn hangul_insert_backspace() {
        let (mut s, mut c) = (String::new(), 0usize);
        for ch in "안녕".chars() {
            insert(&mut s, &mut c, ch);
        }
        assert_eq!((s.as_str(), c), ("안녕", 2));
        backspace(&mut s, &mut c);
        assert_eq!((s.as_str(), c), ("안", 1));
    }

    #[test]
    fn caret_mid_insert_and_bounds() {
        let (mut s, mut c) = ("ab".to_string(), 2usize);
        left(&mut c);
        left(&mut c);
        insert(&mut s, &mut c, 'X'); // 맨 앞 삽입
        assert_eq!((s.as_str(), c), ("Xab", 1));
        for _ in 0..9 {
            right(&s, &mut c); // 끝에서 더 못 감
        }
        assert_eq!(c, 3);
        for _ in 0..9 {
            left(&mut c); // 0 밑으로 안 감
        }
        assert_eq!(c, 0);
    }

    #[test]
    fn newline_is_a_char() {
        let (mut s, mut c) = ("ab".to_string(), 1usize);
        insert(&mut s, &mut c, '\n');
        assert_eq!((s.as_str(), c), ("a\nb", 2));
    }

    #[test]
    fn visual_caret_soft_vs_hard_break() {
        use super::persona_visual_caret as vc;
        let one = vec![("hello".to_string(), 0usize)];
        assert_eq!(vc(&one, 0), (0, 0));
        assert_eq!(vc(&one, 3), (0, 3));
        assert_eq!(vc(&one, 5), (0, 5));

        // 소프트 wrap "abc def" → ["abc ", "def"]: 경계는 아랫줄 맨 앞.
        let soft = vec![("abc ".to_string(), 0usize), ("def".to_string(), 4usize)];
        assert_eq!(vc(&soft, 3), (0, 3));
        assert_eq!(vc(&soft, 4), (1, 0));
        assert_eq!(vc(&soft, 7), (1, 3));

        // 하드 '\n' "a\nb" → ["a"(0), "b"(2)]: 경계는 윗줄 끝.
        let hard = vec![("a".to_string(), 0usize), ("b".to_string(), 2usize)];
        assert_eq!(vc(&hard, 1), (0, 1));
        assert_eq!(vc(&hard, 2), (1, 0));

        assert_eq!(vc(&[(String::new(), 0usize)], 0), (0, 0));
    }
}
