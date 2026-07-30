//! Settings screen — Warp-style full view reached from the titlebar gear (and,
//! in side-tab mode, the sidebar's "Settings" entry). Replaces the pane grid
//! while open; the session sidebar and titlebar stay live. Left category nav
//! (General / Appearance / Shell / Claude / Students) + a right-hand form laid
//! out on a shared spacing rhythm. Each control writes through to
//! `settings.json` immediately and mirrors into the in-memory `set_*` fields so
//! `resolve_*`/`App::new` pick the value up on the next spawn/launch.
//!
//! Rendering runs inside `render_frame_gpu`'s `self.gpu.as_mut()` block where
//! `&self` is off-limits, so the paint is a free function fed a snapshot
//! (`SettingsCtx`) and returns the frame's clickable rects for hit-testing.
//!
//! Segmented controls (`segmented`), field titles (`field_header`) and the
//! single-line text fields (`text_field`, char-index caret) are shared helpers
//! so every page reads consistently. Single-line fields borrow `students_caret`
//! as their caret store since only one field is focused at a time.

use super::*;

type Rect = (f32, f32, f32, f32);

const CAT_W: f32 = 200.0;
const ROW_GAP: f32 = 28.0;

/// Snapshot captured before the gpu borrow so the paint never touches `&self`.
pub(crate) struct SettingsCtx {
    pub area: Rect,
    pub cat: SettingsCat,
    pub cwd_mode: String,
    /// "builtin" · "app" · "terminal" — 파일을 무엇으로 열지.
    pub file_open_mode: String,
    /// `app` 모드가 쓸 앱 이름. 비어 있으면 OS 연결 프로그램.
    pub file_open_app: String,
    /// `terminal` 모드의 명령줄. 비어 있으면 자동 감지된 편집기를 쓴다.
    pub file_open_cmd: String,
    pub file_tree_default: bool,
    pub footer_default: bool,
    /// Editor autosave quiet period in ms; 0 = off.
    pub autosave_ms: u64,
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
    /// 전환 가능한 Claude 로그인들. 기본 로그인(지금 `claude` 가 쓰는 것)은 이
    /// 목록에 없다 — 목록 위에 암묵적 첫 행으로 그린다.
    pub claude_accounts: Vec<socket::ClaudeAccount>,
    /// 활성 계정 id, `""` = 기본 로그인.
    pub claude_account: String,
    /// (표시명, 에셋 슬러그) — Students 카테고리 목록·프사 썸네일용. slug 가
    /// None 이면 아직 도트 에셋이 없는 캐릭터(썸네일 자리표시).
    pub characters: Vec<(String, Option<&'static str>)>,
    /// 단일라인 텍스트 필드(경로·셸·claude extra)의 캐럿(문자 인덱스).
    /// persona 멀티라인 캐럿(`student_caret`)과 분리 — 한 번에 한 필드만
    /// 포커스되지만, 저장소를 나눠 포커스 이동 시 캐럿이 튀지 않게 한다.
    pub settings_caret: usize,
    /// Students 인라인 편집 — 선택 캐릭터·persona 버퍼·persona 캐럿(문자 인덱스).
    pub student_selected: Option<String>,
    pub student_persona: String,
    pub student_caret: usize,
    /// 설정 화면 위에 덮어 그릴 토스트 (메시지, 알파). 설정 오버레이가 chrome
    /// 토스트를 가리므로 여기서 다시 그린다 — 출처는 동일한 collab.toast 슬롯.
    pub toast: Option<(String, f32)>,
}

/// 폼 컨트롤이 퍼지는 최대 가로폭. 넓은 창에서 컨트롤이 오른쪽 허공으로
/// 흩어지지 않게 왼쪽 열로 모은다(페이지 헤더 밑줄·설명 줄 기준폭).
const CONTENT_W: f32 = 600.0;

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
    pub(crate) fn settings_save(&self) {
        socket::write_setting("default_cwd", serde_json::Value::String(self.set_cwd_mode.clone()));
        socket::write_setting("file_open_mode", serde_json::Value::String(self.set_file_open_mode.clone()));
        socket::write_setting("file_open_app", serde_json::Value::String(self.set_file_open_app.clone()));
        socket::write_setting("file_open_cmd", serde_json::Value::String(self.set_file_open_cmd.clone()));
        socket::write_setting("file_tree_default", serde_json::Value::Bool(self.set_file_tree_default));
        socket::write_setting("pane_footer_default", serde_json::Value::Bool(self.set_footer_default));
        socket::write_setting(
            "editor_autosave_ms",
            serde_json::Value::from(self.set_autosave.map_or(0, |d| d.as_millis() as u64)),
        );
        socket::write_setting("default_shell", serde_json::Value::String(self.set_shell.clone()));
        socket::write_setting("claude_persona", serde_json::Value::Bool(self.set_claude_persona));
        socket::write_setting("shim_inject", serde_json::Value::Bool(self.set_shim_inject));
        socket::write_setting("claude_model", serde_json::Value::String(self.set_claude_model.clone()));
        socket::write_setting("claude_effort", serde_json::Value::String(self.set_claude_effort.clone()));
        socket::write_setting("claude_extra", serde_json::Value::String(self.set_claude_extra.clone()));
        socket::write_setting(
            "claude_accounts",
            serde_json::to_value(&self.set_claude_accounts).unwrap_or(serde_json::Value::Null),
        );
        socket::write_setting("claude_account", serde_json::Value::String(self.set_claude_account.clone()));
        self.regen_claude_shim();
    }

    /// 계정 슬롯을 하나 만들고, 그 인증 저장소를 얹은 `claude` 를 새 pane 에 띄운다.
    /// 실제 로그인은 OAuth 브라우저 흐름이라 거노가 그 pane 에서 `/login` 을 한 번
    /// 눌러야 끝난다.
    ///
    /// **추가만 하고 활성 전환은 하지 않는다** — 아직 아무도 로그인하지 않은 저장소로
    /// 즉시 갈아타면 그 뒤에 뜨는 모든 claude 가 로그아웃 상태로 뜬다. 전환은 로그인이
    /// 끝난 뒤 거노가 목록에서 직접 고른다.
    fn add_claude_account(&mut self) {
        // dir 이름이 곧 Keychain 서비스명 해시의 입력이라 계정마다 유일하고 그 뒤로
        // 안 변해야 한다 — 재사용하면 지운 계정의 토큰을 새 계정이 물려받는다.
        let id = (1..)
            .map(|n| format!("acct-{n}"))
            .find(|c| self.set_claude_accounts.iter().all(|a| &a.id != c))
            .expect("1.. is infinite");
        let Some(dir) = socket::claude_account_dir(&id) else {
            self.set_toast("계정 폴더 경로를 만들 수 없습니다".to_string());
            return;
        };
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.set_toast(format!("계정 폴더 생성 실패: {e}"));
            return;
        }
        let label = format!("계정 {}", self.set_claude_accounts.len() + 2);
        self.set_claude_accounts.push(socket::ClaudeAccount { id, label });
        self.settings_save();

        // env 를 명령 앞에 붙여 그 claude 프로세스에만 새 저장소를 물린다. shim 의
        // export 는 `${VAR+x}` 가드라 이미 설정된 값을 덮지 않는다.
        let pane = self
            .split_active_pane(kasa_pty::SplitDir::Horizontal)
            .unwrap_or_default();
        let Some(sess) = self.pty.get(&pane).cloned() else {
            self.set_toast("계정 추가됨 — pane 을 열지 못해 로그인은 수동으로".to_string());
            return;
        };
        let q = dir.display().to_string().replace('\'', "'\\''");
        // 900ms = swap_character 와 같은 "셸 프롬프트가 뜰 즈음" 대기.
        let at = std::time::Instant::now() + std::time::Duration::from_millis(900);
        self.pending_restores.push((
            sess,
            format!("CLAUDE_SECURESTORAGE_CONFIG_DIR='{q}' claude\r"),
            at,
        ));
        self.set_toast("새 pane 에서 로그인하세요 — 끝나면 설정에서 계정을 고르면 돼요".to_string());
        // 설정은 **별도 창**이라, 이걸 안 닫으면 로그인 pane 도 토스트도 전부 그
        // 창 뒤에서 벌어진다 — 거노 눈엔 버튼이 먹통인 것과 구별이 안 됐다.
        // 어차피 다음 할 일이 터미널에서 로그인하는 것이니 본창으로 넘긴다.
        if let Some(i) = self.settings_window_idx() {
            self.close_settings_window(i);
        }
        if let Some(w) = self.window.as_ref() {
            w.focus_window();
        }
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

    /// Build the render snapshot for the settings paint. `area` is the logical
    /// rect the form draws into (the whole aux settings-window client area) and
    /// `cursor` is that window's local cursor — both supplied by the caller so
    /// this stays a pure `&self` snapshot taken outside any gpu borrow.
    pub(crate) fn settings_snapshot(&self, area: Rect, cursor: (f32, f32)) -> SettingsCtx {
        SettingsCtx {
            area,
            cat: self.settings_cat,
            cwd_mode: self.set_cwd_mode.clone(),
            file_open_mode: self.set_file_open_mode.clone(),
            file_open_app: self.set_file_open_app.clone(),
            file_open_cmd: self.set_file_open_cmd.clone(),
            file_tree_default: self.set_file_tree_default,
            footer_default: self.set_footer_default,
            autosave_ms: self.set_autosave.map_or(0, |d| d.as_millis() as u64),
            shell: self.set_shell.clone(),
            input: self.settings_input,
            cursor,
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
            claude_accounts: self.set_claude_accounts.clone(),
            claude_account: self.set_claude_account.clone(),
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
            settings_caret: self.settings_caret,
            student_selected: self.students_selected.clone(),
            student_persona: self.students_persona.clone(),
            student_caret: self.students_caret,
            toast: {
                let a = self.collab_toast_alpha();
                if a > 0.0 {
                    self.collab.toast.as_ref().map(|(m, _)| (m.clone(), a))
                } else {
                    None
                }
            },
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
                    self.settings_caret = self.set_cwd_mode.chars().count();
                    self.settings_input = Some(SettingsInput::CwdPath);
                } else {
                    self.set_cwd_mode = m.to_string();
                    self.settings_input = None;
                }
                self.settings_save();
            }
            SettingsAction::FocusCwdPath => {
                self.settings_caret = self.set_cwd_mode.chars().count();
                self.settings_input = Some(SettingsInput::CwdPath);
            }
            SettingsAction::FileOpenMode(m) => {
                self.set_file_open_mode = m.to_string();
                // "App" 을 처음 고르면 설치된 편집기 중 첫 번째로 채운다. 빈 값은
                // "OS 연결 프로그램" 인데, 이 기기의 기본은 거노가 목록에서 일부러
                // 뺀 앱이었다 — 고르자마자 그게 뜨면 설정이 배신처럼 느껴진다.
                if m == "app" && self.set_file_open_app.is_empty() {
                    if let Some((name, _)) = crate::proc::open_with_apps().first() {
                        self.set_file_open_app = name.clone();
                    }
                }
                // 처음 "Terminal" 을 고르는 순간 감지된 편집기로 필드를 채운다 —
                // 빈 칸을 주고 알아서 적으라 하면 뭘 적어야 하는지 알 수 없다
                // (CwdMode("custom") 이 $HOME 을 시드하는 것과 같은 배려).
                if m == "terminal" {
                    if self.set_file_open_cmd.trim().is_empty() {
                        match socket::resolve_terminal_editor() {
                            Some(cmd) => self.set_file_open_cmd = cmd,
                            None => self.set_toast(
                                "터미널 편집기를 못 찾았어요 — 명령을 직접 적어 주세요".to_string(),
                            ),
                        }
                    }
                    self.settings_caret = self.set_file_open_cmd.chars().count();
                    self.settings_input = Some(SettingsInput::FileOpenCmd);
                } else {
                    self.settings_input = None;
                }
                self.settings_save();
            }
            SettingsAction::FileOpenApp(name) => {
                self.set_file_open_app = name;
                self.settings_save();
            }
            SettingsAction::FocusFileOpenCmd => {
                self.settings_caret = self.set_file_open_cmd.chars().count();
                self.settings_input = Some(SettingsInput::FileOpenCmd);
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
            SettingsAction::AutosaveDelay(ms) => {
                self.set_autosave = (ms > 0).then(|| std::time::Duration::from_millis(ms));
                self.settings_save();
            }
            SettingsAction::ShellPreset(s) => {
                self.set_shell = s;
                self.settings_input = None;
                self.settings_save();
            }
            SettingsAction::FocusShell => {
                self.settings_caret = self.set_shell.chars().count();
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
                // 시작할 때 한 번만 설치되는 shim 이라 라이브 pane 엔 안 먹는다.
                self.set_toast("재시작하면 적용돼요".to_string());
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
                self.settings_caret = self.set_claude_extra.chars().count();
                self.settings_input = Some(SettingsInput::ClaudeExtra);
            }
            SettingsAction::ClaudeAccount(id) => {
                self.set_claude_account = id;
                self.settings_input = None;
                self.settings_save();
            }
            SettingsAction::AddClaudeAccount => self.add_claude_account(),
            SettingsAction::RemoveClaudeAccount(id) => {
                self.set_claude_accounts.retain(|a| a.id != id);
                // 지운 계정이 활성이었으면 기본 로그인으로 — 아무도 로그인할 수
                // 없는 저장소를 계속 가리키면 pane 이 통째로 로그아웃 상태로 뜬다.
                if self.set_claude_account == id {
                    self.set_claude_account = String::new();
                }
                // 라벨 포커스는 행 인덱스라 목록이 줄면 다른 행을 가리킨다.
                self.settings_input = None;
                self.settings_save();
            }
            SettingsAction::FocusClaudeAccountLabel(i) => {
                self.settings_caret = self
                    .set_claude_accounts
                    .get(i)
                    .map_or(0, |a| a.label.chars().count());
                self.settings_input = Some(SettingsInput::ClaudeAccountLabel(i));
            }
            SettingsAction::OpenStudentsDir => self.open_students_dir(),
            SettingsAction::OpenCharactersJson => self.open_characters_json(),
            SettingsAction::RefreshStudentAssets => self.refresh_student_assets(),
            SettingsAction::SelectStudent(name) => self.select_student_for_edit(name),
            SettingsAction::FocusStudentPersona => {
                // 캐럿 저장소를 단일라인 필드와 공유하므로, 다른 필드가 만졌을 수
                // 있는 캐럿을 persona 끝으로 되돌린다.
                self.students_caret = self.students_persona.chars().count();
                self.settings_input = Some(SettingsInput::StudentPersona);
            }
        }
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
        true
    }

    /// Route a keystroke into the focused single-line settings text field.
    /// Returns true if consumed. Full char-index caret: ←→ 이동, 중간
    /// 삽입/삭제, Home/End, host+V 붙여넣기. Enter=커밋(blur+저장 토스트),
    /// Esc=blur. persona 멀티라인은 별도 경로.
    pub(crate) fn settings_key(&mut self, event: &winit::event::KeyEvent) -> bool {
        use winit::event::ElementState;
        use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};
        let Some(field) = self.settings_input else { return false };
        if field == SettingsInput::StudentPersona {
            return self.student_persona_key(event);
        }
        if event.state != ElementState::Pressed {
            return true;
        }
        // host+V 붙여넣기 — 글자 삽입 분기보다 먼저 걸러 "v"가 찍히지 않게 한다.
        // 단일라인이라 개행은 공백으로 눕힌다. 다른 host 단축키는 소비만 하고 무시.
        let host = self.host_mod();
        let paste = if host && matches!(event.physical_key, PhysicalKey::Code(KeyCode::KeyV)) {
            arboard::Clipboard::new()
                .ok()
                .and_then(|mut c| c.get_text().ok())
                .map(|t| t.replace(['\n', '\r'], " "))
        } else if host {
            return true;
        } else {
            None
        };
        let mut changed = false;
        let mut blur = false;
        let mut commit = false;
        {
            // buf 와 caret 은 서로 다른 App 필드라 동시 &mut 가 허용된다(메서드
            // 경유가 아니라 직접 필드 접근이라 disjoint borrow).
            let caret = &mut self.settings_caret;
            let buf = match field {
                SettingsInput::CwdPath => &mut self.set_cwd_mode,
                SettingsInput::FileOpenCmd => &mut self.set_file_open_cmd,
                SettingsInput::Shell => &mut self.set_shell,
                SettingsInput::ClaudeExtra => &mut self.set_claude_extra,
                // 행 인덱스라 목록이 줄면 가리키던 행이 사라질 수 있다 — 그땐 키를
                // 소비만 하고 흘린다(엉뚱한 행에 글자가 찍히는 것보다 낫다).
                SettingsInput::ClaudeAccountLabel(i) => match self.set_claude_accounts.get_mut(i) {
                    Some(a) => &mut a.label,
                    None => return true,
                },
                SettingsInput::StudentPersona => unreachable!("handled above"),
            };
            if *caret > buf.chars().count() {
                *caret = buf.chars().count();
            }
            if let Some(p) = &paste {
                for ch in p.chars() {
                    textedit::insert(buf, caret, ch);
                }
                changed = true;
            } else {
                match &event.logical_key {
                    Key::Named(NamedKey::Backspace) => {
                        textedit::backspace(buf, caret);
                        changed = true;
                    }
                    Key::Named(NamedKey::Enter) => {
                        blur = true;
                        commit = true;
                    }
                    Key::Named(NamedKey::Escape) => blur = true,
                    Key::Named(NamedKey::ArrowLeft) => textedit::left(caret),
                    Key::Named(NamedKey::ArrowRight) => textedit::right(buf, caret),
                    Key::Named(NamedKey::Home) => *caret = 0,
                    Key::Named(NamedKey::End) => *caret = buf.chars().count(),
                    Key::Named(NamedKey::Space) => {
                        textedit::insert(buf, caret, ' ');
                        changed = true;
                    }
                    Key::Character(t) => {
                        for ch in t.chars() {
                            textedit::insert(buf, caret, ch);
                        }
                        changed = true;
                    }
                    _ => {}
                }
            }
        }
        if blur {
            self.settings_input = None;
        }
        if changed {
            self.settings_save();
        }
        if commit {
            self.set_toast("저장됐어요".to_string());
        }
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

    /// 다른 캐릭터를 편집 중이었으면 먼저 저장하고, 새 캐릭터의 원본 persona 를
    /// 버퍼로 로드(캐럿은 끝으로). settings_click 의 학생 행 클릭과 별도창 딥링크
    /// (프사 클릭 → Students 페이지 + 해당 학생 선택)가 공유한다.
    pub(crate) fn select_student_for_edit(&mut self, name: String) {
        self.flush_student_persona();
        let persona = kasa_mcp::character::characters_json()
            .and_then(|c| kasa_mcp::character::raw_persona_for(&c, &name))
            .unwrap_or_default();
        self.students_caret = persona.chars().count();
        self.students_persona = persona;
        self.students_selected = Some(name);
        self.settings_input = Some(SettingsInput::StudentPersona);
    }

    /// 현재 포커스된 설정 필드에 텍스트를 삽입(IME commit·한글 조합 완성 경로 공용).
    /// persona 는 students_caret, 단일라인 필드는 settings_caret 를 캐럿으로 쓴다.
    pub(crate) fn settings_insert_text(&mut self, text: &str) {
        let Some(field) = self.settings_input else { return };
        if field == SettingsInput::StudentPersona {
            for ch in text.chars() {
                textedit::insert(&mut self.students_persona, &mut self.students_caret, ch);
            }
            self.chrome_dirty = true;
            return;
        }
        let caret = &mut self.settings_caret;
        let buf = match field {
            SettingsInput::CwdPath => &mut self.set_cwd_mode,
            SettingsInput::FileOpenCmd => &mut self.set_file_open_cmd,
            SettingsInput::Shell => &mut self.set_shell,
            SettingsInput::ClaudeExtra => &mut self.set_claude_extra,
            SettingsInput::ClaudeAccountLabel(i) => match self.set_claude_accounts.get_mut(i) {
                Some(a) => &mut a.label,
                None => return,
            },
            SettingsInput::StudentPersona => unreachable!("handled above"),
        };
        if *caret > buf.chars().count() {
            *caret = buf.chars().count();
        }
        for ch in text.chars() {
            textedit::insert(buf, caret, ch);
        }
        self.settings_save();
        self.chrome_dirty = true;
    }

    /// persona 편집 버퍼를 characters.json 에 저장(선택 캐릭터가 있고 실제로
    /// 바뀌었을 때만). blur·선택 변경·설정 닫기 시 호출. 저장 후 shim 을 재생성해
    /// 그 캐릭터 pane 의 다음 claude 실행이 새 persona 를 집게 한다.
    pub(crate) fn flush_student_persona(&mut self) {
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
    let fw = (aw - CAT_W - 80.0).max(120.0).min(CONTENT_W);
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
            let cwd_is = |m: &str| {
                if m == "last" {
                    ctx.cwd_mode == "last"
                } else if m == "home" {
                    ctx.cwd_mode == "home"
                } else {
                    ctx.cwd_mode != "last" && ctx.cwd_mode != "home"
                }
            };
            y = field_header(g, fx, y, clip, "Startup folder", &["새 창과 탭이 열리는 위치"]);
            if y > clip {
                let cells = [
                    ("Last folder", cwd_is("last"), SettingsAction::CwdMode("last")),
                    ("Home", cwd_is("home"), SettingsAction::CwdMode("home")),
                    ("Custom", cwd_is("custom"), SettingsAction::CwdMode("custom")),
                ];
                segmented(g, &mut rects, fx, y, &cells, ctx.cursor);
            }
            y += SEG_H;
            // Custom path field, only when "Custom" is active.
            if cwd_is("custom") {
                y += 10.0;
                if y > clip {
                    let r = (fx, y, fw.min(420.0), 34.0);
                    let focused = ctx.input == Some(SettingsInput::CwdPath);
                    text_field(g, r, &ctx.cwd_mode, ctx.settings_caret, focused, ctx.caret_on, ctx.cursor);
                    rects.push((SettingsAction::FocusCwdPath, r));
                }
                y += 34.0;
            }
            y += ROW_GAP;
            y = field_header(g, fx, y, clip, "File tree by default", &["시작할 때 파일 트리 사이드바 열기"]);
            if y > clip {
                let tr = (fx, y, 52.0, 30.0);
                toggle(g, tr, ctx.file_tree_default, ctx.cursor);
                rects.push((SettingsAction::ToggleFileTree, tr));
            }
            y += 30.0 + ROW_GAP;
            y = field_header(g, fx, y, clip, "Pane status bar by default", &["각 pane 아래 경로 · 브랜치 · diff 바 표시"]);
            if y > clip {
                let fr = (fx, y, 52.0, 30.0);
                toggle(g, fr, ctx.footer_default, ctx.cursor);
                rects.push((SettingsAction::ToggleFooter, fr));
            }
            y += 30.0 + ROW_GAP;
            y = field_header(
                g,
                fx,
                y,
                clip,
                "File open",
                &[
                    "파일 트리에서 파일을 열 때 무엇으로 열지",
                    "App = VS Code 같은 GUI 편집기로 열기",
                    "Terminal = 새 pane 에서 CLI 편집기 ({} 는 파일 경로 자리)",
                ],
            );
            // `"system"` 은 `"app"` 의 옛 저장값 — 앱 미지정과 뜻이 같아 같은 칸으로.
            let open_is = |m: &str| {
                ctx.file_open_mode == m || (m == "app" && ctx.file_open_mode == "system")
            };
            if y > clip {
                let cells = [
                    ("Built-in", open_is("builtin"), SettingsAction::FileOpenMode("builtin")),
                    ("App", open_is("app"), SettingsAction::FileOpenMode("app")),
                    ("Terminal", open_is("terminal"), SettingsAction::FileOpenMode("terminal")),
                ];
                segmented(g, &mut rects, fx, y, &cells, ctx.cursor);
            }
            y += SEG_H;
            if open_is("app") {
                y += 10.0;
                if y > clip {
                    // 설치된 것만 뜬다(`open_with_apps`). 마지막 "기본 앱" 은 OS
                    // 연결 프로그램 — 목록에 없는 앱을 쓰는 사람의 탈출구다.
                    let apps = crate::proc::open_with_apps();
                    let mut cells: Vec<(&str, bool, SettingsAction)> = apps
                        .iter()
                        .map(|(name, _)| {
                            (
                                crate::info::short_app_name(name),
                                ctx.file_open_app == *name,
                                SettingsAction::FileOpenApp(name.clone()),
                            )
                        })
                        .collect();
                    cells.push((
                        "기본 앱",
                        ctx.file_open_app.is_empty(),
                        SettingsAction::FileOpenApp(String::new()),
                    ));
                    segmented(g, &mut rects, fx, y, &cells, ctx.cursor);
                }
                y += SEG_H;
            }
            if open_is("terminal") {
                y += 10.0;
                if y > clip {
                    let r = (fx, y, fw.min(420.0), 34.0);
                    let focused = ctx.input == Some(SettingsInput::FileOpenCmd);
                    text_field(g, r, &ctx.file_open_cmd, ctx.settings_caret, focused, ctx.caret_on, ctx.cursor);
                    rects.push((SettingsAction::FocusFileOpenCmd, r));
                }
                y += 34.0;
            }
            y += ROW_GAP;
            y = field_header(
                g,
                fx,
                y,
                clip,
                "Editor autosave",
                &["타자가 멎으면 편집기가 조용히 저장 (Cmd+S 는 그대로)"],
            );
            if y > clip {
                let cells = [
                    ("Off", ctx.autosave_ms == 0, SettingsAction::AutosaveDelay(0)),
                    ("1s", ctx.autosave_ms == 1000, SettingsAction::AutosaveDelay(1000)),
                    ("3s", ctx.autosave_ms == 3000, SettingsAction::AutosaveDelay(3000)),
                    ("10s", ctx.autosave_ms == 10000, SettingsAction::AutosaveDelay(10000)),
                ];
                segmented(g, &mut rects, fx, y, &cells, ctx.cursor);
            }
            y += SEG_H + ROW_GAP;
            y = field_header(g, fx, y, clip, "Tab position", &["윈도우 탭을 상단 타이틀바 또는 좌측 사이드바에 표시"]);
            if y > clip {
                let cells = [
                    ("Top", ctx.tabs_on_top, SettingsAction::TabPosition("top")),
                    ("Side", !ctx.tabs_on_top, SettingsAction::TabPosition("side")),
                ];
                segmented(g, &mut rects, fx, y, &cells, ctx.cursor);
            }
            content_bottom = y + SEG_H;
        }
        SettingsCat::Appearance => {
            let mut y = fy;
            // 테마 — 프리셋 카드 그리드. 카드 하나 = 그 팔레트의 미니 프리뷰
            // (bg 칠 + 프롬프트 샘플 + ANSI 도트 + 라벨)라서 고르기 전에 색이
            // 보인다. UI 토큰과 터미널 ANSI 16색이 함께 바뀐다.
            y = field_header(g, fx, y, clip, "Theme", &["UI + 터미널 ANSI 팔레트가 함께 바뀌어요"]);
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
            y = field_header(g, fx, y, clip, "Accent color", &["선택 영역 · 커서 · 링크 색"]);
            if y > clip {
                let mut cxp = fx;
                for (name, col) in theme::ACCENT_PRESETS {
                    let sz = 30.0_f32;
                    let r = (cxp, y, sz, sz);
                    let sel = *name == ctx.accent;
                    let hover = inside(r, ctx.cursor);
                    // Halo behind the swatch: text-colored disc when selected,
                    // muted disc on hover — same feedback the other controls give.
                    if sel {
                        round_rect(g, r.0 - 3.0, r.1 - 3.0, sz + 6.0, sz + 6.0, (sz + 6.0) / 2.0, theme::text());
                    } else if hover {
                        round_rect(g, r.0 - 3.0, r.1 - 3.0, sz + 6.0, sz + 6.0, (sz + 6.0) / 2.0, theme::text_mute());
                    }
                    round_rect(g, r.0, r.1, sz, sz, sz / 2.0, *col);
                    rects.push((SettingsAction::Accent(name.to_string()), r));
                    cxp += sz + 14.0;
                }
            }
            y += 30.0 + ROW_GAP;
            // 폰트 크기 스테퍼 — 값은 즉시 적용(그리드 리플로우)되고
            // settings.json 에 저장돼 재시작에도 유지된다.
            y = field_header(g, fx, y, clip, "Font size", &["터미널 셀 폰트 크기 (기본 16 · Cmd+/- 줌과 별개인 기준값)"]);
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
            y = field_header(g, fx, y, clip, "Default shell", &["새 pane 의 셸 (비우면 시스템 $SHELL)"]);
            let presets: [(&str, &str); 3] =
                [("", "System default"), ("/bin/zsh", "zsh"), ("/bin/bash", "bash")];
            let shell_is_preset = presets.iter().any(|(v, _)| *v == ctx.shell);
            if y > clip {
                // Preset 칸들 + 자유입력 필드로 포커스를 주는 "Custom" 칸.
                let cells = [
                    ("System default", ctx.shell.is_empty(), SettingsAction::ShellPreset(String::new())),
                    ("zsh", ctx.shell == "/bin/zsh", SettingsAction::ShellPreset("/bin/zsh".to_string())),
                    ("bash", ctx.shell == "/bin/bash", SettingsAction::ShellPreset("/bin/bash".to_string())),
                    ("Custom", !shell_is_preset, SettingsAction::FocusShell),
                ];
                segmented(g, &mut rects, fx, y, &cells, ctx.cursor);
            }
            y += SEG_H;
            if !shell_is_preset {
                y += 10.0;
                if y > clip {
                    let r = (fx, y, fw.min(420.0), 34.0);
                    let focused = ctx.input == Some(SettingsInput::Shell);
                    text_field(g, r, &ctx.shell, ctx.settings_caret, focused, ctx.caret_on, ctx.cursor);
                    rects.push((SettingsAction::FocusShell, r));
                }
                y += 34.0;
            }
            content_bottom = y + ROW_GAP;
        }
        SettingsCat::Claude => {
            // Page 헤더가 이미 "Claude" 를 크게 쓰므로 별도 브랜드 워드마크는
            // 중복이라 뺐다 — 좌측 nav 에도 claude 아이콘이 있다.
            let mut y = fy;
            // Shim injection — global. off = install_pane_shims never makes the shim
            // dir, so claude runs vanilla (no persona/proxy/hooks). Read once at boot,
            // so a change needs a restart.
            y = field_header(g, fx, y, clip, "Shim injection",
                &["끄면 순정 Claude — 페르소나 · 캡처 프록시 · 훅 전부 없음",
                  "재시작해야 적용돼요 — 시작할 때 한 번만 설치돼서"]);
            if y > clip {
                let sr = (fx, y, 52.0, 30.0);
                toggle(g, sr, ctx.shim_inject, ctx.cursor);
                rects.push((SettingsAction::ToggleShimInject, sr));
            }
            y += 30.0 + ROW_GAP;
            y = field_header(g, fx, y, clip, "Persona injection", &["이 pane 의 캐릭터를 Claude 시스템 프롬프트에 붙여요"]);
            if y > clip {
                let pr = (fx, y, 52.0, 30.0);
                toggle(g, pr, ctx.claude_persona, ctx.cursor);
                rects.push((SettingsAction::ToggleClaudePersona, pr));
            }
            y += 30.0 + ROW_GAP;
            y = field_header(g, fx, y, clip, "Account",
                &["로그인 계정을 골라요 — 다음에 뜨는 claude 부터 그 계정으로",
                  "이미 돌고 있는 세션은 원래 계정 그대로예요",
                  "지워도 로그인 자체는 남아요 — 목록에서만 빠져요"]);
            let row_h = 34.0_f32;
            // 첫 행은 언제나 "기본"(활성 계정 `""` = env 미설정 = 지금 로그인). 이 행은
            // 우리가 만든 슬롯이 아니라 지울 것도, 이름 붙일 것도 없다.
            let acct_rows = std::iter::once((String::new(), "기본 (지금 로그인된 계정)".to_string(), None))
                .chain(
                    ctx.claude_accounts
                        .iter()
                        .enumerate()
                        .map(|(i, a)| (a.id.clone(), a.label.clone(), Some(i))),
                );
            for (id, label, idx) in acct_rows {
                if y > clip {
                    let active = ctx.claude_account == id;
                    // 라디오 — 켜지면 accent 링 + 가운데 점.
                    let dot = (fx, y + (row_h - 16.0) / 2.0, 16.0, 16.0);
                    round_rect(g, dot.0, dot.1, dot.2, dot.3, 8.0,
                        if active { theme::accent() } else { theme::surface_active() });
                    if active {
                        round_rect(g, dot.0 + 5.0, dot.1 + 5.0, 6.0, 6.0, 3.0, theme::bg());
                    }
                    let hit = (fx - 4.0, y, 24.0, row_h);
                    if inside(hit, ctx.cursor) && !active {
                        round_rect(g, dot.0, dot.1, dot.2, dot.3, 8.0, theme::surface_hover());
                    }
                    rects.push((SettingsAction::ClaudeAccount(id.clone()), hit));
                    // 오른쪽 끝에 그 슬롯의 진짜 신원 — 라벨은 거노가 붙인 별명이라
                    // 로그인이 실제로 됐는지는 말해 주지 않는다.
                    let status_x = match idx {
                        None => {
                            g.draw_text(
                                fx + 28.0, y + (row_h - 14.0) / 2.0, &label,
                                gpu::DrawOpts { font_size: 14.0, color: theme::text_mute(), bold: active, italic: false },
                            );
                            fx + 28.0 + g.measure_chrome_text(&label, 14.0, active) + 12.0
                        }
                        Some(i) => {
                            let lr = (fx + 28.0, y, fw.min(240.0), row_h);
                            let focused = ctx.input == Some(SettingsInput::ClaudeAccountLabel(i));
                            text_field(g, lr, &label, ctx.settings_caret, focused, ctx.caret_on, ctx.cursor);
                            rects.push((SettingsAction::FocusClaudeAccountLabel(i), lr));
                            let dr = (lr.0 + lr.2 + 8.0, y, 30.0, row_h);
                            stepper_btn(g, dr, "x", ctx.cursor);
                            rects.push((SettingsAction::RemoveClaudeAccount(id.clone()), dr));
                            dr.0 + dr.2 + 12.0
                        }
                    };
                    if let Some(p) = auth_probe(&id) {
                        let (txt, col) = if p.logged_in {
                            (p.email, theme::text_mute())
                        } else {
                            ("로그인 필요".to_string(), theme::danger())
                        };
                        g.draw_text(
                            status_x, y + (row_h - 12.0) / 2.0, &txt,
                            gpu::DrawOpts { font_size: 12.0, color: col, bold: false, italic: false },
                        );
                    }
                }
                y += row_h + 6.0;
            }
            if y > clip {
                let label = "+ 계정 추가";
                let bw = g.measure_chrome_text(label, 13.0, false) + 28.0;
                let r = (fx, y, bw, 34.0);
                round_rect(g, r.0, r.1, r.2, r.3, theme::RADIUS_MD,
                    if inside(r, ctx.cursor) { theme::surface_hover() } else { theme::surface_active() });
                g.draw_text(
                    r.0 + 14.0, r.1 + 9.0, label,
                    gpu::DrawOpts { font_size: 13.0, color: theme::text(), bold: false, italic: false },
                );
                rects.push((SettingsAction::AddClaudeAccount, r));
            }
            y += 34.0 + ROW_GAP;
            y = field_header(g, fx, y, clip, "Model", &["Claude 모델 덮어쓰기 (Default = 원래대로 유지)"]);
            if y > clip {
                let cells = [
                    ("Default", ctx.claude_model.is_empty(), SettingsAction::ClaudeModel(String::new())),
                    ("opus", ctx.claude_model == "opus", SettingsAction::ClaudeModel("opus".to_string())),
                    ("sonnet", ctx.claude_model == "sonnet", SettingsAction::ClaudeModel("sonnet".to_string())),
                    ("haiku", ctx.claude_model == "haiku", SettingsAction::ClaudeModel("haiku".to_string())),
                ];
                segmented(g, &mut rects, fx, y, &cells, ctx.cursor);
            }
            y += SEG_H + ROW_GAP;
            y = field_header(g, fx, y, clip, "Effort", &["추론 강도 (CLAUDE_EFFORT). Default = 그대로 둠"]);
            if y > clip {
                let cells = [
                    ("Default", ctx.claude_effort.is_empty(), SettingsAction::ClaudeEffort(String::new())),
                    ("low", ctx.claude_effort == "low", SettingsAction::ClaudeEffort("low".to_string())),
                    ("medium", ctx.claude_effort == "medium", SettingsAction::ClaudeEffort("medium".to_string())),
                    ("high", ctx.claude_effort == "high", SettingsAction::ClaudeEffort("high".to_string())),
                    ("xhigh", ctx.claude_effort == "xhigh", SettingsAction::ClaudeEffort("xhigh".to_string())),
                ];
                segmented(g, &mut rects, fx, y, &cells, ctx.cursor);
            }
            y += SEG_H + ROW_GAP;
            y = field_header(g, fx, y, clip, "Extra args", &["claude 실행에 항상 붙는 플래그 (예: --verbose)"]);
            if y > clip {
                let r = (fx, y, fw.min(420.0), 34.0);
                let focused = ctx.input == Some(SettingsInput::ClaudeExtra);
                text_field(g, r, &ctx.claude_extra, ctx.settings_caret, focused, ctx.caret_on, ctx.cursor);
                rects.push((SettingsAction::FocusClaudeExtra, r));
            }
            content_bottom = y + 34.0;
        }
        SettingsCat::Students => {
            let mut y = fy;
            y = field_header(g, fx, y, clip, "Character images",
                &["~/.config/kasaterm/students/ 에 이미지를 넣으면 학생 그림이 바뀌어요",
                  "파일명: <slug>-profile.png · <slug>-0..3.png · <slug>-walk-0..5.png · schale-logo.png"]);
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
            y = field_header(g, fx, y, clip, "Characters", &["캐릭터를 눌러 성격(persona)을 바로 편집하세요"]);
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
                y = field_header(g, fx, y, clip, &format!("{sel} · persona"),
                    &["성격·말투를 평문으로. Enter=줄바꿈, 바깥 클릭·Esc=저장"]);
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

    // Toast inside the settings window (top-right of the form). The chrome toast
    // lives on the main window; this redraws the same source slot here so
    // save/restart feedback stays visible while the settings window has focus.
    if let Some((msg, alpha)) = &ctx.toast {
        let t_font = 13.0_f32;
        let (px, py) = (14.0_f32, 8.0_f32);
        let box_w = g.measure_chrome_text(msg, t_font, true) + px * 2.0;
        let box_h = t_font + py * 2.0;
        let bx = ax + aw - box_w - 16.0;
        let by = ay + 12.0;
        let a = (235.0 * alpha).round() as u8;
        round_rect(g, bx, by, box_w, box_h, theme::RADIUS_MD, theme::with_alpha(theme::surface_active(), a));
        let ta = (255.0 * alpha).round() as u8;
        g.draw_text(
            bx + px, by + py, msg,
            gpu::DrawOpts { font_size: t_font, color: theme::with_alpha(theme::text(), ta), bold: true, italic: false },
        );
    }

    (rects, content_bottom - fy)
}

/// 세그먼트 컨트롤의 고정 높이(트랙 + 내부 패딩).
const SEG_H: f32 = 34.0;

/// 계정 슬롯 하나의 실제 로그인 상태. `~/.claude.json` 의 `oauthAccount` 캐시는
/// config dir 소속이라 계정끼리 **공유**돼서 전환 직후 옛 계정 이름이 남는다 —
/// 그래서 캐시 대신 계정별 저장소를 얹은 `claude auth status` 의 답을 쓴다.
#[derive(Clone)]
struct AuthProbe {
    logged_in: bool,
    email: String,
}

/// 계정별 probe 캐시. 렌더가 매 프레임 도는 자리라 캐시 없이는 subprocess 폭주가
/// 된다. 값이 `None` 이면 "조회 중"(또는 실패) — 그동안은 아무것도 안 그린다.
/// TTL 을 두는 이유: 거노가 pane 에서 `/login` 을 마치면 클릭 없이 저절로 반영돼야
/// 한다. 함수-로컬 static 이라 `struct App` 은 안 건드린다(병렬 작업 규칙).
fn auth_probe(id: &str) -> Option<AuthProbe> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};
    type Cache = Mutex<HashMap<String, (Instant, Option<AuthProbe>)>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let cache = CACHE.get_or_init(Cache::default);
    {
        let mut m = cache.lock().unwrap();
        if let Some((at, v)) = m.get(id) {
            if at.elapsed() < Duration::from_secs(20) {
                return v.clone();
            }
        }
        // 조회 중 표시를 먼저 박아 다음 프레임이 또 스폰하지 않게 한다.
        m.insert(id.to_string(), (Instant::now(), None));
    }
    let key = id.to_string();
    let dir = socket::claude_account_dir(id);
    std::thread::spawn(move || {
        // pane 과 같은 PATH 를 보려면 로그인 셸을 거쳐야 한다 — Finder 로 뜬 .app 의
        // PATH 에는 claude 가 없어서 직접 spawn 하면 항상 실패한다.
        let shell = resolve_default_shell().unwrap_or_else(|| "/bin/sh".to_string());
        let mut c = std::process::Command::new(shell);
        c.arg("-lc").arg("claude auth status");
        if let Some(d) = dir {
            c.env("CLAUDE_SECURESTORAGE_CONFIG_DIR", d);
        }
        let probe = c
            .output()
            .ok()
            .and_then(|o| serde_json::from_slice::<serde_json::Value>(&o.stdout).ok())
            .map(|v| AuthProbe {
                logged_in: v.get("loggedIn").and_then(|x| x.as_bool()).unwrap_or(false),
                email: v.get("email").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
            });
        if let Some(cache) = CACHE.get() {
            cache.lock().unwrap().insert(key, (Instant::now(), probe));
        }
    });
    None
}

/// 설정 항목의 제목과 (있으면) 설명 줄들을 그리고, 컨트롤이 놓일 y 를 돌려준다.
/// 스크롤로 clip 위로 올라간 줄은 그리지 않되 자리(y 전진)는 유지한다 — 렌더러에
/// scissor 가 없어 헤더/타이틀바를 침범하지 않으려면 통째로 스킵해야 한다.
fn field_header(g: &mut gpu::GpuRenderer, x: f32, y: f32, clip: f32, title: &str, help: &[&str]) -> f32 {
    if y > clip {
        section_label(g, x, y, title);
    }
    if help.is_empty() {
        return y + 32.0;
    }
    let mut hy = y + 24.0;
    for line in help {
        if hy > clip {
            help_text(g, x, hy, line);
        }
        hy += 18.0;
    }
    hy + 10.0
}

/// 세그먼트 컨트롤 — 하나의 트랙(pill) 안에 옵션 칸들이 붙어 있는 형태. 선택된
/// 칸만 accent 로 채우고, hover 칸은 옅게 밝힌다. 칸 폭은 라벨을 **bold** 로 재서
/// (선택 시 bold 라 더 넓어짐) 글자가 칸 밖으로 넘치지 않게 한다 — 예전엔 non-bold
/// 로 재고 bold 로 그려 선택 칸 글자가 잘렸다. 각 칸이 클릭 rect 로 등록된다.
fn segmented(
    g: &mut gpu::GpuRenderer,
    rects: &mut Vec<(SettingsAction, Rect)>,
    x: f32,
    y: f32,
    cells: &[(&str, bool, SettingsAction)],
    cursor: (f32, f32),
) {
    let pad = 4.0_f32; // 트랙 안쪽 여백 (칸이 트랙 테두리에 붙지 않게)
    let cell_pad = 16.0_f32; // 칸 좌우 텍스트 여백
    let widths: Vec<f32> = cells
        .iter()
        .map(|(label, _, _)| g.measure_chrome_text(label, 13.0, true) + cell_pad * 2.0)
        .collect();
    let total: f32 = pad * 2.0 + widths.iter().sum::<f32>();
    round_rect(g, x, y, total, SEG_H, theme::RADIUS_MD, theme::surface_active());
    let mut cxp = x + pad;
    let cell_h = SEG_H - pad * 2.0;
    for (i, (label, sel, action)) in cells.iter().enumerate() {
        let cw = widths[i];
        let cell = (cxp, y + pad, cw, cell_h);
        let hover = inside(cell, cursor);
        if *sel {
            round_rect(g, cell.0, cell.1, cell.2, cell.3, theme::RADIUS_SM, theme::accent());
        } else if hover {
            round_rect(g, cell.0, cell.1, cell.2, cell.3, theme::RADIUS_SM, theme::surface_hover());
        }
        let tw = g.measure_chrome_text(label, 13.0, *sel);
        g.draw_text(
            cxp + (cw - tw) / 2.0,
            y + (SEG_H - 13.0) / 2.0 - 1.0,
            label,
            gpu::DrawOpts {
                font_size: 13.0,
                color: if *sel { theme::bg() } else { theme::text_dim() },
                bold: *sel,
                italic: false,
            },
        );
        // Hit rect = full track height so the whole cell is clickable.
        rects.push((action.clone(), (cxp, y, cw, SEG_H)));
        cxp += cw;
    }
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

/// 단일라인 텍스트 필드. `caret` 는 문자(char) 인덱스라 문자열 중간에도 캐럿을
/// 그린다 — 캐럿 앞 부분 폭을 재서 그 x 에 1.5px 세로 막대를 세운다.
fn text_field(
    g: &mut gpu::GpuRenderer,
    r: Rect,
    text: &str,
    caret: usize,
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
    g.draw_text(
        tx,
        ty,
        text,
        gpu::DrawOpts { font_size: 13.0, color: theme::text(), bold: false, italic: false },
    );
    if focused && caret_on {
        let pre: String = text.chars().take(caret).collect();
        let cx = tx + g.measure_chrome_text(&pre, 13.0, false);
        g.rect(cx, r.1 + 7.0, 1.5, r.3 - 14.0, theme::text());
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
    fn mid_string_paste_and_delete() {
        // 캐럿을 중간에 두고 여러 글자(붙여넣기)를 순서대로 삽입 → 캐럿이 삽입
        // 문자열 끝으로 따라간다. 이어서 Backspace 로 중간 글자만 지운다.
        let (mut s, mut c) = ("ac".to_string(), 1usize); // a|c
        for ch in "-경로-".chars() {
            insert(&mut s, &mut c, ch);
        }
        assert_eq!((s.as_str(), c), ("a-경로-c", 5));
        backspace(&mut s, &mut c); // '-' 삭제 → a-경로|c
        assert_eq!((s.as_str(), c), ("a-경로c", 4));
        left(&mut c); // a-경|로c
        backspace(&mut s, &mut c); // '경' 삭제 → a-|로c
        assert_eq!((s.as_str(), c), ("a-로c", 2));
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
