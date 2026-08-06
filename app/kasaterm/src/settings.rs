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
/// 항목과 항목 **사이**. `field_header` 안쪽 간격보다 넓게 유지할 것 — 안쪽이
/// 더 벌어지면 설명이 제 제목을 떠나 위 항목에 붙어 읽힌다.
const ROW_GAP: f32 = 24.0;

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
    /// Active silhouette key ("rounded" · "sharp" · "pixel").
    pub shape: String,
    /// settings.json has a `custom_theme` object → show the Custom card.
    pub has_custom_theme: bool,
    pub accent: String,
    pub font_size: f32,
    /// 전체 UI 배율(1.0 = 100%). 화면에 숫자로 보여야 얼마나 틀어졌는지 안다.
    pub ui_zoom: f32,
    /// PixelDelta 스크롤 감도 배율(트랙패드·고해상도 마우스휠 공용).
    pub wheel_pixel_gain: f32,
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
    /// 한도가 차면 다음 계정으로 알아서 넘어간다 + 그 임계 사용률(%).
    pub account_autoswitch: bool,
    pub account_autoswitch_pct: f32,
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
    /// Feedback 본문 버퍼·캐럿·진단 첨부 스위치.
    pub feedback_body: String,
    pub feedback_caret: usize,
    pub feedback_diag: bool,
    /// 설정 화면 위에 덮어 그릴 토스트 (메시지, 알파). 설정 오버레이가 chrome
    /// 토스트를 가리므로 여기서 다시 그린다 — 출처는 동일한 collab.toast 슬롯.
    pub toast: Option<(String, f32)>,
    /// 조합 중인 한글 음절. macOS 는 OS IME 를 꺼 두고 자모를 직접 조합하는데,
    /// 이걸 안 그리면 완성될 때까지 화면에 아무 일도 일어나지 않는다 — 거노가
    /// "조합도 안 보이고 계가 ㄱㅖ로 된다"고 한 것의 절반이 이거다(친 자모가
    /// 사라진 것처럼 보이니 다시 치게 된다).
    pub preedit: String,
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

/// 평문을 편집 박스 폭에 맞춰 시각 라인들로 접는다(word-wrap).
/// 각 원소 = (그 시각 라인 문자열, 그 라인 첫 글자의 전역 char 인덱스).
/// '\n' 은 강제 개행, 그 외엔 폭 초과 시 공백 우선(없으면 글자 단위) 분할.
/// 빈 텍스트도 시각 라인 1개(빈 줄)를 돌려준다 — 캐럿 그릴 자리가 필요하다.
/// 사람이 쓴 문단은 보통 줄바꿈 없이 길어서, wrap 없이는 박스 밖으로 잘린다.
fn wrap_lines(
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
fn visual_caret(vis: &[(String, usize)], caret: usize) -> (usize, usize) {
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
    pub(crate) fn settings_btn_rect(&self, win_h_logical: f32) -> Rect {
        // 사이드바 하단 트레이의 오른쪽 끝. 사이드바가 없으면(top 모드 또는 접힘)
        // 그려지지도 않는데 rect 는 매 프레임 저장돼(render) hit-test 에 남는다 —
        // top 모드에서 이 유령 rect 가 설정 화면 좌측 카테고리 nav(같은 좌상단
        // 영역)의 Appearance·Shell 클릭을 가로채 페이지 전환이 안 됐다(거노).
        // 안 그려질 땐 hit 대상도 없어야 하므로 무효 rect 를 돌려준다 — 그때의
        // 설정 진입점은 우측 패널 Info 탭이 담당한다.
        self.sidebar_tray_rects(win_h_logical)
            .map_or((0.0, 0.0, 0.0, 0.0), |(_, _, _, s)| s)
    }
    /// 트레이의 피드백 버튼. 설정 바로 왼쪽 — 둘 다 "앱에 말을 거는" 쪽이라 묶어
    /// 두고, 새 세션(`+`)과는 트레이 양 끝으로 갈라 성격을 구분한다.
    pub(crate) fn feedback_btn_rect(&self, win_h_logical: f32) -> Rect {
        self.sidebar_tray_rects(win_h_logical)
            .map_or((0.0, 0.0, 0.0, 0.0), |(_, _, f, _)| f)
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
        socket::write_setting(
            "claude_account_autoswitch",
            serde_json::Value::Bool(self.set_account_autoswitch),
        );
        socket::write_setting(
            "claude_account_autoswitch_pct",
            serde_json::Value::from(self.set_account_autoswitch_pct),
        );
        socket::write_setting(
            "wheel_pixel_gain",
            serde_json::Value::from(self.set_wheel_pixel_gain),
        );
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
        // 라벨은 비워 둔다 — 이름을 안 붙인 슬롯은 `account_display` 가 그 슬롯의
        // 진짜 이메일로 부른다. "계정 3" 을 미리 박아 두면 거노가 직접 친 별명과
        // 구별이 안 돼 이메일로 대체할 수가 없다.
        self.set_claude_accounts
            .push(socket::ClaudeAccount { id: id.clone(), label: String::new() });
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
        // 예전엔 맨 `claude` 를 띄웠다. 그러면 로그인은 거노가 그 안에서 `/login` 을
        // 찾아 눌러야 하는 숨은 한 단계였고(거노: "계정추가 누르면 그냥 클로드가
        // 켜지는데?"), 정작 계정을 가르는 일은 아무것도 안 했다.
        self.pending_restores.push((
            sess,
            format!("CLAUDE_SECURESTORAGE_CONFIG_DIR='{q}' claude auth login --claudeai\r"),
            at,
        ));
        // 슬롯이 자꾸 같은 계정으로 겹친 진짜 원인은 여기다: `claude auth login` 이
        // 기본 브라우저를 여는데 거기엔 지금 계정의 claude.ai 세션이 살아 있어
        // 그대로 승인돼 버린다. 슬롯마다 **쿠키 없는 브라우저 프로필**을 갈라 두면
        // 새 창은 로그인 화면부터 뜨고, 그제서야 다른 계정을 넣을 수 있다.
        spawn_oauth_browser_watch(self.ws.clone(), pane.clone(), socket::oauth_profile_dir(&id));
        self.set_toast(
            "빈 브라우저 창에서 새 계정으로 로그인하세요 — 기본 브라우저 탭은 닫으시고요"
                .to_string(),
        );
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
            shape: theme::shape_name().to_string(),
            font_size: self.font_size,
            ui_zoom: self.ui_zoom,
            wheel_pixel_gain: self.set_wheel_pixel_gain,
            tabs_on_top: self.tabs_on_top,
            claude_persona: self.set_claude_persona,
            shim_inject: self.set_shim_inject,
            claude_model: self.set_claude_model.clone(),
            claude_effort: self.set_claude_effort.clone(),
            claude_extra: self.set_claude_extra.clone(),
            claude_accounts: self.set_claude_accounts.clone(),
            claude_account: self.set_claude_account.clone(),
            account_autoswitch: self.set_account_autoswitch,
            account_autoswitch_pct: self.set_account_autoswitch_pct,
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
            feedback_body: self.feedback_body.clone(),
            feedback_caret: self.feedback_caret,
            feedback_diag: self.feedback_diag,
            preedit: self.hangul.preedit().unwrap_or_default(),
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
    /// 테마를 갈기 **직전에** 부른다 — 지금 배경색을 붙잡아 둬야 그게 흩어질 옛
    /// 화면이 된다. 전환이 이미 돌고 있으면 시작 시각만 되감아, 빠르게 여러 번
    /// 눌러도 화면이 옛 색 여러 겹으로 두꺼워지지 않는다.
    pub(crate) fn begin_theme_fx(&mut self) {
        let keep = self.theme_fx.map(|(_, bg)| bg).unwrap_or_else(theme::bg);
        self.theme_fx = Some((std::time::Instant::now(), keep));
    }

    pub(crate) fn repaint_all(&mut self) {
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
        self.settings_apply(action);
        true
    }

    /// 설정 항목 하나를 실행한다. `settings_click` 에서 갈라 둔 건 히트렉트 없이도
    /// (헤드리스 검증·단축키) 같은 경로를 탈 수 있게 하기 위한 것이다.
    pub(crate) fn settings_apply(&mut self, action: SettingsAction) {
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
                self.begin_theme_fx();
                theme::set_theme(m);
                socket::write_setting("theme", serde_json::Value::String(m.to_string()));
                self.repaint_all();
            }
            SettingsAction::Accent(name) => {
                theme::set_accent(&name);
                socket::write_setting("accent", serde_json::Value::String(name));
                self.repaint_all();
            }
            SettingsAction::Shape(s) => {
                theme::set_shape(s);
                socket::write_setting("shape", serde_json::Value::String(s.to_string()));
                self.repaint_all();
            }
            SettingsAction::MinContrast(label) => {
                let v = theme::CONTRAST_PRESETS
                    .iter()
                    .find(|(l, _)| *l == label)
                    .map_or(2.5, |(_, v)| *v);
                theme::set_min_contrast(v);
                socket::write_setting("min_contrast", serde_json::json!(v));
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
            SettingsAction::UiZoomDelta(d) => {
                self.change_ui_zoom(d as f32 * 0.1);
            }
            SettingsAction::ResetScale => {
                self.font_size = socket::DEFAULT_FONT_SIZE;
                socket::write_setting("font_size", serde_json::json!(self.font_size));
                // `reset_ui_zoom` 은 이미 1.0 이면 일찍 빠져나가므로, 폰트만 바뀐
                // 경우에도 격자가 다시 서도록 여기서 한 번 더 부른다.
                self.reset_ui_zoom();
                self.apply_effective_scale();
                self.set_toast("배율 100% · 폰트 기본값".to_string());
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
            SettingsAction::ToggleAccountAutoswitch => {
                self.set_account_autoswitch = !self.set_account_autoswitch;
                // 켜는 순간 옛 쿨다운은 버린다 — 며칠 전 소진 기록이 남아 있으면
                // 켜자마자 "갈 곳이 없다"로 조용히 아무 일도 안 하게 된다.
                if self.set_account_autoswitch {
                    socket::clear_account_cooldowns();
                }
                self.settings_save();
            }
            SettingsAction::AccountAutoswitchPct(p) => {
                self.set_account_autoswitch_pct = p as f32;
                self.settings_save();
            }
            SettingsAction::WheelPixelGain(x100) => {
                self.set_wheel_pixel_gain = x100 as f32 / 100.0;
                self.settings_save();
                // 휠 경로는 값을 캐시해 둔다(매 이벤트 파일을 읽을 수 없다) —
                // 비워 주지 않으면 다음 스크롤까지 옛 감도로 굴러간다.
                crate::invalidate_wheel_pixel_gain();
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
            SettingsAction::FocusFeedbackBody => {
                self.feedback_caret = self.feedback_body.chars().count();
                self.settings_input = Some(SettingsInput::FeedbackBody);
            }
            SettingsAction::ToggleFeedbackDiag => self.feedback_diag = !self.feedback_diag,
            SettingsAction::SaveFeedback => self.save_feedback(),
            SettingsAction::OpenFeedbackDir => {
                let dir = feedback_dir();
                let _ = std::fs::create_dir_all(&dir);
                open_path(&dir);
            }
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
    }

    /// Route a keystroke into the focused single-line settings text field.
    /// Returns true if consumed. Full char-index caret: ←→ 이동, 중간
    /// 삽입/삭제, Home/End, host+V 붙여넣기. Enter=커밋(blur+저장 토스트),
    /// Esc=blur. 멀티라인 필드는 별도 경로.
    pub(crate) fn settings_key(&mut self, event: &winit::event::KeyEvent) -> bool {
        use winit::event::ElementState;
        use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};
        let Some(field) = self.settings_input else { return false };
        if matches!(field, SettingsInput::StudentPersona | SettingsInput::FeedbackBody) {
            return self.multiline_key(field, event);
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
                SettingsInput::StudentPersona | SettingsInput::FeedbackBody => {
                    unreachable!("multiline_key 가 먼저 가로챈다")
                }
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
                            // 낱자는 조합기 것이다. macOS 한글 배열은 자모를
                            // `event.text` 로 주지만 그게 비고 `logical_key` 만
                            // 오는 프레임이 있는데, 그때 여기로 새면 "계"가
                            // "ㄱㅖ"로 박힌다 — 조합을 거치지 않은 자모는 그 자체로
                            // 잘못 온 것이라 넣지 않는다.
                            if is_jamo(ch) {
                                continue;
                            }
                            textedit::insert(buf, caret, ch);
                            changed = true;
                        }
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

    /// 멀티라인 편집 키 라우팅(persona · 피드백 본문). Enter=개행, 방향키 좌우로
    /// 캐럿 이동, 문자/Space 삽입, Backspace 삭제.
    ///
    /// Esc 는 blur 인데 뒤처리가 필드마다 다르다 — persona 는 blur 가 곧 저장이라
    /// characters.json 을 쓰고(매 키가 아니라 여기서만), 피드백은 저장 버튼이
    /// 따로 있으니 버퍼만 남긴다. 그래서 Esc 만 borrow 전에 먼저 처리한다.
    fn multiline_key(&mut self, field: SettingsInput, event: &winit::event::KeyEvent) -> bool {
        use winit::event::ElementState;
        use winit::keyboard::{Key, NamedKey};
        if event.state != ElementState::Pressed {
            return true;
        }
        if matches!(event.logical_key, Key::Named(NamedKey::Escape)) {
            if field == SettingsInput::StudentPersona {
                self.flush_student_persona();
            }
            self.settings_input = None;
            self.chrome_dirty = true;
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
            return true;
        }
        let (buf, caret) = if field == SettingsInput::FeedbackBody {
            (&mut self.feedback_body, &mut self.feedback_caret)
        } else {
            (&mut self.students_persona, &mut self.students_caret)
        };
        match &event.logical_key {
            Key::Named(NamedKey::Enter) => textedit::insert(buf, caret, '\n'),
            Key::Named(NamedKey::Backspace) => textedit::backspace(buf, caret),
            Key::Named(NamedKey::ArrowLeft) => textedit::left(caret),
            Key::Named(NamedKey::ArrowRight) => textedit::right(buf, caret),
            Key::Named(NamedKey::Space) => textedit::insert(buf, caret, ' '),
            Key::Character(t) => {
                for ch in t.chars() {
                    textedit::insert(buf, caret, ch);
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
    /// 멀티라인 필드는 각자의 캐럿을, 단일라인 필드는 settings_caret 를 쓴다.
    /// 자모 하나를 조합기에 먹인다. 완성된 음절이 떨어지면 포커스 필드에 넣고,
    /// 조합 중이면 아무것도 넣지 않는다(그 사이 화면에 보이는 건 `SettingsCtx.preedit`).
    /// 반환값 = 이 키를 조합기가 삼켰는가 — true 면 필드 편집으로 넘기면 안 된다.
    ///
    /// 키 핸들러에서 이 판단을 인라인으로 하던 것을 함수로 뺀 건 헤드리스로
    /// 확인할 자리를 만들기 위해서다 — 조합은 실제 IME 없이는 재현이 안 돼
    /// 지금까지 이 경로만 검증 사각이었다.
    pub(crate) fn settings_hangul_char(&mut self, c: char) -> bool {
        if self.settings_input.is_none() || !is_jamo(c) {
            return false;
        }
        if let Some(commit) = self.hangul.feed(c) {
            self.settings_insert_text(&commit);
        }
        true
    }

    /// 조합 중이던 음절을 확정해 필드에 넣는다. 자모가 아닌 키·포커스 이동처럼
    /// 조합이 끝나는 자리마다 불러야 마지막 글자가 증발하지 않는다.
    pub(crate) fn settings_hangul_flush(&mut self) {
        if let Some(flushed) = self.hangul.flush() {
            self.settings_insert_text(&flushed);
        }
    }

    pub(crate) fn settings_insert_text(&mut self, text: &str) {
        let Some(field) = self.settings_input else { return };
        if matches!(field, SettingsInput::StudentPersona | SettingsInput::FeedbackBody) {
            let (buf, caret) = if field == SettingsInput::FeedbackBody {
                (&mut self.feedback_body, &mut self.feedback_caret)
            } else {
                (&mut self.students_persona, &mut self.students_caret)
            };
            for ch in text.chars() {
                textedit::insert(buf, caret, ch);
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
            SettingsInput::StudentPersona | SettingsInput::FeedbackBody => {
                unreachable!("multiline_key 가 먼저 가로챈다")
            }
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

    /// 피드백 본문을 `~/.config/kasaterm/feedback/` 에 마크다운 한 장으로 굳힌다.
    ///
    /// 보낼 곳이 아직 없다 — 그래서 "전송"이 아니라 "저장"이고, 파일로 남기는
    /// 것까지가 이 기능의 전부다. 나중에 받는 창구가 생기면 이 폴더를 그대로
    /// 올리면 되도록 한 건=한 파일로 둔다.
    pub(crate) fn save_feedback(&mut self) {
        let body = self.feedback_body.trim().to_string();
        if body.is_empty() {
            return;
        }
        let dir = feedback_dir();
        if std::fs::create_dir_all(&dir).is_err() {
            self.set_toast("피드백 폴더를 못 만들었어요".to_string());
            return;
        }
        let stamp = local_stamp();
        let mut doc = format!("# {stamp}\n\n{body}\n");
        if self.feedback_diag {
            doc.push_str(&format!("\n---\n{}\n", diag_line()));
        }
        // 같은 분 안에 두 번 저장해도 덮어쓰지 않게 뒤에 번호를 붙인다.
        let base = stamp.replace([' ', ':'], "-");
        let mut path = dir.join(format!("{base}.md"));
        let mut n = 2;
        while path.exists() {
            path = dir.join(format!("{base}-{n}.md"));
            n += 1;
        }
        match std::fs::write(&path, doc) {
            Ok(()) => {
                self.feedback_body.clear();
                self.feedback_caret = 0;
                self.settings_input = None;
                self.set_toast("피드백을 저장했어요".to_string());
            }
            Err(e) => self.set_toast(format!("저장 실패: {e}")),
        }
    }
}

/// 저장된 피드백이 쌓이는 폴더.
fn feedback_dir() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    std::path::PathBuf::from(home).join(".config/kasaterm/feedback")
}

/// 제보에 붙는 진단 한 줄. 이게 없으면 대부분의 제보가 "어느 버전에서요?"로
/// 한 번 더 왕복한다.
fn diag_line() -> String {
    format!(
        "kasaterm {} · {} {}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
    )
}

/// 파일명·머리글에 쓸 지역시각 `YYYY-MM-DD HH:MM`.
///
/// 시간대 변환은 OS 에 맡긴다 — 직접 하려면 tz 데이터베이스를 들여야 하고,
/// 9시간 어긋난 시각은 시각이 없는 것보다 나쁘다. libc 를 macOS 타깃에만
/// 붙여 둬서 그 밖에서는 epoch 초로 물러난다(정렬은 그대로 된다).
fn local_stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    #[cfg(target_os = "macos")]
    {
        let t = secs as libc::time_t;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        unsafe { libc::localtime_r(&t, &mut tm) };
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        secs.to_string()
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
    g.rect(ax, ay, CAT_W, ah, theme::panel_bg());
    g.rect(ax + CAT_W - 1.0, ay, 1.0, ah, theme::border());
    g.draw_text(
        ax + 20.0,
        ay + 20.0,
        "Settings",
        gpu::DrawOpts { font_size: 12.0, color: theme::text_dim(), bold: true, italic: false },
    );
    let cats = [
        (SettingsCat::General, "General", "settings-2"),
        (SettingsCat::Appearance, "Appearance", "sparkles"),
        (SettingsCat::Shell, "Shell", "terminal"),
        (SettingsCat::Claude, "Claude", "claude"),
        (SettingsCat::Students, "Students", "users"),
        (SettingsCat::Feedback, "Feedback", "message-square-warning"),
    ];
    let mut cy = ay + 48.0;
    let mut active_label = "General";
    for (cat, label, icon) in cats {
        let r = (ax + 10.0, cy, CAT_W - 20.0, 32.0);
        let selected = cat == ctx.cat;
        if selected {
            active_label = label;
        }
        let hover = inside(r, ctx.cursor);
        g.hover_pointer |= hover;
        if selected {
            round_rect(g, r.0, r.1, r.2, r.3, theme::radius_md(), theme::surface_active());
            // 왼쪽 띠 — 채움만으로는 흑백에서 선택이 겨우 보인다. 색을 걷어내도
            // 어느 항목에 있는지 읽혀야 하니 형태를 하나 더 준다.
            g.rect(r.0, r.1 + 6.0, 2.0, r.3 - 12.0, theme::accent());
        } else if hover {
            round_rect(g, r.0, r.1, r.2, r.3, theme::radius_md(), theme::surface_hover());
        }
        let icon_c = if selected { theme::text() } else { theme::text_mute() };
        g.queue_icon(icon, r.0 + 12.0, r.1 + (r.3 - 14.0) / 2.0, 14.0, icon_c);
        g.draw_text(
            r.0 + 34.0,
            r.1 + (r.3 - 13.0) / 2.0 - 1.0,
            label,
            gpu::DrawOpts {
                font_size: 13.0,
                color: if selected { theme::text() } else { theme::text_dim() },
                bold: selected,
                italic: false,
            },
        );
        rects.push((SettingsAction::Category(cat), r));
        cy += 34.0;
    }

    // ── Right form pane ──────────────────────────────────────────────────
    // Page header: the active category as a title + hairline, so the form
    // reads as its own page instead of floating controls.
    let fx = ax + CAT_W + 40.0;
    let fw = (aw - CAT_W - 80.0).max(120.0).min(CONTENT_W);
    g.draw_text(
        fx, ay + 26.0, active_label,
        gpu::DrawOpts { font_size: 24.0, color: theme::text(), bold: true, italic: false },
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
                    text_field(g, r, &ctx.cwd_mode, ctx.settings_caret, focused, ctx.caret_on, ctx.cursor, if focused { &ctx.preedit } else { "" });
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
                    text_field(g, r, &ctx.file_open_cmd, ctx.settings_caret, focused, ctx.caret_on, ctx.cursor, if focused { &ctx.preedit } else { "" });
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
            y += SEG_H + ROW_GAP;
            // 트랙패드와 고해상도 마우스휠은 같은 델타로 들어와 자동으로 못 가른다 —
            // 그래서 한쪽에 맞추면 다른 쪽이 어긋난다. 고르는 몫을 사람에게 넘긴다.
            y = field_header(
                g,
                fx,
                y,
                clip,
                "Scroll sensitivity",
                &["트랙패드 기준이 기본이에요. 마우스 휠이 굼뜨면 올리세요"],
            );
            if y > clip {
                let x100 = (ctx.wheel_pixel_gain * 100.0).round() as u32;
                let cells = [
                    ("트랙패드", x100 == 30, SettingsAction::WheelPixelGain(30)),
                    ("보통", x100 == 60, SettingsAction::WheelPixelGain(60)),
                    ("마우스", x100 == 100, SettingsAction::WheelPixelGain(100)),
                    ("빠르게", x100 == 150, SettingsAction::WheelPixelGain(150)),
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
            y = field_header(g, fx, y, clip, "Theme",
                &["UI + 터미널 ANSI 팔레트가 함께 바뀌어요",
                  "System 은 OS 의 밝게/어둡게를 따라가요 — 바꾸면 알아서 넘어가요"]);
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
                g.hover_pointer |= hover;
                // Selection / hover ring — a slightly larger plate behind the
                // card (same halo trick as the accent swatches).
                if sel {
                    round_rect(g, x - 2.0, cy - 2.0, card_w + 4.0, card_h + 4.0, theme::radius_md() + 2.0, theme::accent());
                } else if hover {
                    round_rect(g, x - 2.0, cy - 2.0, card_w + 4.0, card_h + 4.0, theme::radius_md() + 2.0, theme::surface_hover());
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
                panel_rect(g, x, cy, card_w, card_h, theme::radius_md(), bg);
                // Prompt sample in the theme's own text color.
                // 미리보기 줄은 터미널 폰트 고정 — 이 카드가 대변하는 건 UI 크롬이
                // 아니라 터미널 본문이고, 본문 폰트는 형태 축이 건드리지 않는다.
                g.draw_text_mono(
                    x + 12.0, cy + 12.0, "❯ ls -la",
                    gpu::DrawOpts { font_size: 12.0, color: text, bold: false, italic: false },
                );
                // ANSI 1..=6 dots (red green yellow blue magenta cyan).
                for i in 0..6 {
                    let c = ansi[i + 1];
                    circle_rect(
                        g, x + 12.0 + i as f32 * 16.0, cy + 36.0, 10.0,
                        [c[0], c[1], c[2], 255],
                    );
                }
                g.draw_text(
                    x + 12.0, cy + card_h - 26.0, label,
                    gpu::DrawOpts { font_size: 12.0, color: dim, bold: sel, italic: false },
                );
                rects.push((SettingsAction::ThemeMode(key), r));
            };
            // System 이 맨 앞 — Dark·Light 와 함께 기본 셋을 이룬다. 미리보기는
            // 지금 OS 가 가리키는 팔레트로 그린다(라이브 색을 쓰면 다른 테마를
            // 고른 상태에서 그 색이 비쳐 시스템이 무엇인지 거짓말을 한다).
            let sys_key = theme::system_theme_key();
            let sys = theme::THEME_PRESETS.iter().find(|(k, _, _)| *k == sys_key);
            // 라벨에 지금 따르는 쪽을 적는다 — 안 적으면 System 카드가 Light 카드와
            // 똑같이 생겨서 둘의 차이가 화면에 없다.
            let sys_label = match sys {
                Some((_, l, _)) => format!("System · {l}"),
                None => "System".to_string(),
            };
            card(g, &mut rects, "system", &sys_label, sys.map(|(_, _, p)| *p));
            for (key, label, pal) in theme::THEME_PRESETS {
                card(g, &mut rects, key, label, Some(pal));
            }
            if ctx.has_custom_theme {
                card(g, &mut rects, "custom", "Custom (settings.json)", None);
            }
            let rows = idx.div_ceil(per_row);
            y += rows as f32 * (card_h + gap) + ROW_GAP;
            // 형태 — 팔레트와 독립된 축. 각 카드가 *자기* 실루엣으로 그려진다
            // (모서리 반경 · 테두리 두께 · 그림자 · 점과 캡슐의 둥글기) — 테마
            // 카드가 팔레트를 미리 보여주는 것과 같은 규칙이라, 고르기 전에
            // 형태가 눈에 보인다.
            y = field_header(g, fx, y, clip, "Shape", &["모서리 · 점 · 토글의 실루엣 (팔레트와 별개 축)"]);
            if y > clip {
                let (sw, sh) = (108.0_f32, 58.0_f32);
                let mut sxp = fx;
                for (key, label, sp) in theme::SHAPE_PRESETS {
                    let r = (sxp, y, sw, sh);
                    let sel = ctx.shape.as_str() == *key;
                    let hover = inside(r, ctx.cursor);
                    g.hover_pointer |= hover;
                    if sel || hover {
                        let ring = if sel { theme::accent() } else { theme::surface_hover() };
                        round_rect(g, sxp - 2.0, y - 2.0, sw + 4.0, sh + 4.0, sp.radius_md + 2.0, ring);
                    }
                    if sp.shadow_offset > 0.0 {
                        round_rect(g, sxp + sp.shadow_offset, y + sp.shadow_offset, sw, sh, sp.radius_md, [0, 0, 0, 0x66]);
                    }
                    // 두꺼운 테두리는 곧 픽셀 아웃라인 — panel_rect 와 같은 검정을
                    // 써야 카드가 실제로 그려질 모습을 보여준다.
                    let outline = if sp.border_w > 1.0 { [0, 0, 0, 0xE0] } else { theme::border() };
                    round_rect(g, sxp, y, sw, sh, sp.radius_md, outline);
                    let b = sp.border_w;
                    round_rect(g, sxp + b, y + b, sw - b * 2.0, sh - b * 2.0, (sp.radius_md - b).max(0.0), theme::surface());
                    // 점과 캡슐 — roundness 가 0 이면 사각으로 떨어진다.
                    round_rect(g, sxp + 12.0, y + 13.0, 10.0, 10.0, 5.0 * sp.roundness, theme::accent());
                    round_rect(g, sxp + 28.0, y + 13.0, 28.0, 10.0, 5.0 * sp.roundness, theme::text_mute());
                    g.draw_text(
                        sxp + 12.0,
                        y + sh - 22.0,
                        label,
                        gpu::DrawOpts {
                            font_size: 11.5,
                            color: if sel { theme::text() } else { theme::text_dim() },
                            bold: sel,
                            italic: false,
                        },
                    );
                    rects.push((SettingsAction::Shape(*key), r));
                    sxp += sw + 12.0;
                }
            }
            y += 58.0 + ROW_GAP;
            y = field_header(g, fx, y, clip, "Accent color", &["선택 영역 · 커서 · 링크 색"]);
            if y > clip {
                let mut cxp = fx;
                for (name, col) in theme::ACCENT_PRESETS {
                    let sz = 30.0_f32;
                    let r = (cxp, y, sz, sz);
                    let sel = *name == ctx.accent;
                    let hover = inside(r, ctx.cursor);
                    g.hover_pointer |= hover;
                    // Halo behind the swatch: text-colored disc when selected,
                    // muted disc on hover — same feedback the other controls give.
                    if sel {
                        circle_rect(g, r.0 - 3.0, r.1 - 3.0, sz + 6.0, theme::text());
                    } else if hover {
                        circle_rect(g, r.0 - 3.0, r.1 - 3.0, sz + 6.0, theme::text_mute());
                    }
                    circle_rect(g, r.0, r.1, sz, *col);
                    rects.push((SettingsAction::Accent(name.to_string()), r));
                    cxp += sz + 14.0;
                }
            }
            y += 30.0 + ROW_GAP;
            // 최소 대비 — 앱이 스스로 이름 붙인 색만 대상이다. 각 버튼은 자기
            // 임계를 적용한 샘플을 그려서, 고르기 전에 그 값이 실제로 얼마나
            // 끌어올리는지 눈으로 비교된다.
            y = field_header(
                g, fx, y, clip, "Minimum contrast",
                &["앱이 직접 지정한 색이 배경에 묻힐 때만 끌어올린다 (dim 은 제외)"],
            );
            if y > clip {
                let (bw, bh) = (86.0_f32, 44.0_f32);
                let mut bxp = fx;
                let cur = theme::min_contrast();
                // 샘플은 카드 배경에서 글자색 쪽으로 아주 조금 민 색 — 고정
                // 회색으로 두면 다크 팔레트에선 이미 잘 보여서 네 버튼이 전부
                // 같아 보인다. 배경 기준이라 어느 테마에서든 "거의 안 보이는"
                // 지점에서 출발한다.
                let (sf, tx) = (theme::surface(), theme::text());
                let mut sample = [0u8, 0, 0, 0xFF];
                for i in 0..3 {
                    sample[i] = (sf[i] as f32 + (tx[i] as f32 - sf[i] as f32) * 0.18).round() as u8;
                }
                for (label, v) in theme::CONTRAST_PRESETS {
                    let r = (bxp, y, bw, bh);
                    // 사용자가 settings.json 에 임의 값을 넣을 수 있으니 동등이
                    // 아니라 가장 가까운 프리셋을 선택으로 본다.
                    let sel = theme::CONTRAST_PRESETS
                        .iter()
                        .min_by(|a, b| (a.1 - cur).abs().total_cmp(&(b.1 - cur).abs()))
                        .is_some_and(|(l, _)| l == label);
                    let hover = inside(r, ctx.cursor);
                    g.hover_pointer |= hover;
                    if sel || hover {
                        let ring = if sel { theme::accent() } else { theme::surface_hover() };
                        round_rect(g, bxp - 2.0, y - 2.0, bw + 4.0, bh + 4.0, theme::radius_md() + 2.0, ring);
                    }
                    round_rect(g, bxp, y, bw, bh, theme::radius_md(), theme::surface());
                    g.draw_text(
                        bxp + 10.0, y + 8.0, "Aa 가나",
                        gpu::DrawOpts {
                            font_size: 12.0,
                            color: theme::enforce_contrast_at(sample, theme::surface(), *v),
                            bold: false,
                            italic: false,
                        },
                    );
                    g.draw_text(
                        bxp + 10.0, y + bh - 18.0, label,
                        gpu::DrawOpts {
                            font_size: 11.0,
                            color: if sel { theme::text() } else { theme::text_dim() },
                            bold: sel,
                            italic: false,
                        },
                    );
                    rects.push((SettingsAction::MinContrast(label), r));
                    bxp += bw + 10.0;
                }
            }
            y += 44.0 + ROW_GAP;
            // 폰트 크기 스테퍼 — 값은 즉시 적용(그리드 리플로우)되고
            // settings.json 에 저장돼 재시작에도 유지된다.
            y = field_header(g, fx, y, clip, "Font size", &["터미널 셀 폰트 크기 — Cmd+/− 배율과는 별개인 기준값이에요"]);
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
            y += 30.0 + ROW_GAP;
            // UI 배율 — 여태 Cmd+/− 키에만 있었다. 키로만 있으면 지금 몇 %인지
            // 화면 어디에도 안 적혀서, 폰트 크기와 배율을 번갈아 만지다 UI 가
            // 어긋나도 어느 쪽이 범인인지 알 수가 없다(거노). 숫자를 보여 주고,
            // 둘을 한 번에 되돌릴 자리를 옆에 둔다.
            y = field_header(g, fx, y, clip, "UI 배율",
                &["크롬·사이드바·pane 이 함께 커져요 (Cmd+/− 와 같은 축)"]);
            if y > clip {
                let bs = 30.0_f32;
                let minus = (fx, y, bs, bs);
                stepper_btn(g, minus, "minus", ctx.cursor);
                rects.push((SettingsAction::UiZoomDelta(-1), minus));
                let num = format!("{:.0}%", ctx.ui_zoom * 100.0);
                let num_w = g.measure_chrome_text(&num, 15.0, true);
                let num_span = 62.0_f32;
                g.draw_text(
                    fx + bs + (num_span - num_w) / 2.0,
                    y + (bs - 15.0) / 2.0,
                    &num,
                    gpu::DrawOpts { font_size: 15.0, color: theme::text(), bold: true, italic: false },
                );
                let plus = (fx + bs + num_span, y, bs, bs);
                stepper_btn(g, plus, "plus", ctx.cursor);
                rects.push((SettingsAction::UiZoomDelta(1), plus));
                // 되돌리기 — 이미 기본값이면 누를 것이 없으므로 흐리게 두고
                // 히트렉트도 안 만든다(눌러도 아무 일 없는 버튼은 고장으로 읽힌다).
                let at_default = (ctx.ui_zoom - 1.0).abs() < 0.01
                    && (ctx.font_size - socket::DEFAULT_FONT_SIZE).abs() < 0.01;
                let label = "1:1 로 되돌리기";
                let bw = g.measure_chrome_text(label, 12.5, false) + 26.0;
                let r = (fx + bs * 2.0 + num_span + 14.0, y, bw, bs);
                let hov = !at_default && inside(r, ctx.cursor);
                if at_default {
                    round_rect(g, r.0, r.1, r.2, r.3, theme::radius_md(), theme::surface());
                } else {
                    g.hover_pointer |= hov;
                    panel_rect_outlined(
                        g, r.0, r.1, r.2, r.3, theme::radius_md(),
                        theme::raised_on(theme::panel_bg(), hov),
                    );
                    rects.push((SettingsAction::ResetScale, r));
                }
                g.draw_text(
                    r.0 + 13.0, r.1 + (bs - 12.5) / 2.0, label,
                    gpu::DrawOpts {
                        font_size: 12.5,
                        color: if at_default { theme::text_mute() } else { theme::text() },
                        bold: false,
                        italic: false,
                    },
                );
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
                    text_field(g, r, &ctx.shell, ctx.settings_caret, focused, ctx.caret_on, ctx.cursor, if focused { &ctx.preedit } else { "" });
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
                    circle_rect(g, dot.0, dot.1, dot.2,
                        if active { theme::accent() } else { theme::surface_active() });
                    if active {
                        circle_rect(g, dot.0 + 5.0, dot.1 + 5.0, 6.0, theme::bg());
                    }
                    let hit = (fx - 4.0, y, 24.0, row_h);
                    if inside(hit, ctx.cursor) && !active {
                        circle_rect(g, dot.0, dot.1, dot.2, theme::surface_hover());
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
                            text_field(g, lr, &label, ctx.settings_caret, focused, ctx.caret_on, ctx.cursor, if focused { &ctx.preedit } else { "" });
                            rects.push((SettingsAction::FocusClaudeAccountLabel(i), lr));
                            let dr = (lr.0 + lr.2 + 8.0, y, 30.0, row_h);
                            stepper_btn(g, dr, "x", ctx.cursor);
                            rects.push((SettingsAction::RemoveClaudeAccount(id.clone()), dr));
                            dr.0 + dr.2 + 12.0
                        }
                    };
                    // 빈칸을 남기지 않는다. 답이 아직 없는 두 경우 — 첫 조회 중이거나,
                    // 로그인은 됐는데 안 쓰던 슬롯이라 토큰이 만료돼 갱신이 도는 중 —
                    // 이 자리를 비워 두면 계정이 사라진 것처럼 보인다(거노: "계정
                    // 재시작할때마다 또 없어지냐"). 실제로는 몇 초 뒤 채워지므로,
                    // 없다고 말하지 말고 아직 모른다고 말한다.
                    let probe = auth_probe(&id);
                    let team = probe.as_ref().and_then(|p| team_org(&p.email, &p.org));
                    let (txt, col) = match &probe {
                        Some(p) if !p.logged_in => ("로그인 필요".to_string(), theme::danger()),
                        Some(p) if !p.email.is_empty() => (p.email.clone(), theme::text_mute()),
                        _ => ("확인 중…".to_string(), theme::with_alpha(theme::text_mute(), 0x99)),
                    };
                    g.draw_text(
                        status_x, y + (row_h - 12.0) / 2.0, &txt,
                        gpu::DrawOpts { font_size: 12.0, color: col, bold: false, italic: false },
                    );
                    // 팀 조직만 배지로. 같은 이메일이 두 슬롯에 걸릴 때 이게 유일한
                    // 구분점이라 이메일 옆에 붙여 한 눈에 같이 읽히게 둔다.
                    if let Some(org) = team {
                        let bx = status_x + g.measure_chrome_text(&txt, 12.0, false) + 8.0;
                        let bw = g.measure_chrome_text(&org, 11.0, false) + 14.0;
                        round_rect(g, bx, y + (row_h - 18.0) / 2.0, bw, 18.0,
                            theme::radius_sm(), theme::surface_active());
                        g.draw_text(
                            bx + 7.0, y + (row_h - 11.0) / 2.0, &org,
                            gpu::DrawOpts { font_size: 11.0, color: theme::accent(), bold: false, italic: false },
                        );
                    }
                }
                y += row_h + 6.0;
            }
            if y > clip {
                let label = "+ 계정 추가";
                let bw = g.measure_chrome_text(label, 13.0, false) + 28.0;
                let r = (fx, y, bw, 34.0);
                round_rect(g, r.0, r.1, r.2, r.3, theme::radius_md(),
                    if inside(r, ctx.cursor) { theme::surface_hover() } else { theme::surface_active() });
                g.draw_text(
                    r.0 + 14.0, r.1 + 9.0, label,
                    gpu::DrawOpts { font_size: 13.0, color: theme::text(), bold: false, italic: false },
                );
                rects.push((SettingsAction::AddClaudeAccount, r));
            }
            y += 34.0 + ROW_GAP;
            // 자동 전환. 계정이 하나뿐이면 갈 곳이 없어 아무 일도 안 일어나므로
            // 그 상태를 설명 줄로 미리 알려 준다 — 켜 놓고 "안 되네" 하는 게 이
            // 기능에서 제일 흔한 오해다.
            let lone = ctx.claude_accounts.is_empty();
            y = field_header(g, fx, y, clip, "Auto switch",
                &["한도가 차면 다음 계정으로 알아서 넘어가요 — 다음에 뜨는 claude 부터",
                  if lone { "계정이 하나뿐이라 지금은 넘어갈 곳이 없어요" }
                  else { "떠난 계정은 그 한도가 풀릴 때까지 후보에서 빠져요" }]);
            if y > clip {
                let ar = (fx, y, 52.0, 30.0);
                toggle(g, ar, ctx.account_autoswitch, ctx.cursor);
                rects.push((SettingsAction::ToggleAccountAutoswitch, ar));
            }
            y += 30.0 + ROW_GAP;
            if ctx.account_autoswitch {
                y = field_header(g, fx, y, clip, "Switch at",
                    &["이 사용률을 넘으면 넘어가요 — 5시간 창과 주간 한도 중 높은 쪽 기준"]);
                if y > clip {
                    let pct = ctx.account_autoswitch_pct.round() as u32;
                    let cells = [
                        ("80%", pct == 80, SettingsAction::AccountAutoswitchPct(80)),
                        ("85%", pct == 85, SettingsAction::AccountAutoswitchPct(85)),
                        ("90%", pct == 90, SettingsAction::AccountAutoswitchPct(90)),
                        ("95%", pct == 95, SettingsAction::AccountAutoswitchPct(95)),
                    ];
                    segmented(g, &mut rects, fx, y, &cells, ctx.cursor);
                }
                y += SEG_H + ROW_GAP;
            }
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
                text_field(g, r, &ctx.claude_extra, ctx.settings_caret, focused, ctx.caret_on, ctx.cursor, if focused { &ctx.preedit } else { "" });
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
                    g.hover_pointer |= hover;
                    round_rect(
                        g, r.0, r.1, r.2, r.3, theme::radius_md(),
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
                    g.hover_pointer |= hover;
                    if selected {
                        round_rect(g, r.0, r.1, r.2, r.3, theme::radius_sm(), theme::surface_active());
                    } else if hover {
                        round_rect(g, r.0, r.1, r.2, r.3, theme::radius_sm(), theme::surface_hover());
                    }
                    let sw = theme::character_accent(name).unwrap_or([128, 128, 128, 255]);
                    round_rect(g, fx, y + 3.0, 14.0, 14.0, theme::radius_sm(), sw);
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
                y += multiline_field(
                    g, &mut rects, ctx, (fx, y, fw.min(560.0)), &ctx.student_persona,
                    ctx.student_caret, SettingsInput::StudentPersona,
                    SettingsAction::FocusStudentPersona, clip,
                );
            }
            content_bottom = y;
        }
        SettingsCat::Feedback => {
            let mut y = fy;
            y = field_header(g, fx, y, clip, "무엇이 불편했나요",
                &["버그 · 이상한 동작 · 있었으면 하는 것 — 아무 형식이나 괜찮아요",
                  "Enter=줄바꿈, Esc=포커스 해제"]);
            y += multiline_field(
                g, &mut rects, ctx, (fx, y, fw.min(560.0)), &ctx.feedback_body,
                ctx.feedback_caret, SettingsInput::FeedbackBody,
                SettingsAction::FocusFeedbackBody, clip,
            );
            y += ROW_GAP;
            let diag = diag_line();
            y = field_header(g, fx, y, clip, "진단 정보 함께 남기기", &[diag.as_str()]);
            if y > clip {
                let tr = (fx, y, 52.0, 30.0);
                toggle(g, tr, ctx.feedback_diag, ctx.cursor);
                rects.push((SettingsAction::ToggleFeedbackDiag, tr));
            }
            y += 30.0 + ROW_GAP;
            // 보내는 게 아니라 쌓는 거라 "저장". 받는 곳이 생기기 전에 "보내기"라고
            // 쓰면 안 간 걸 갔다고 말하는 셈이다.
            if y > clip {
                let mut bx = fx;
                let empty = ctx.feedback_body.trim().is_empty();
                for (label, action, primary) in [
                    ("저장", SettingsAction::SaveFeedback, true),
                    ("저장된 피드백 열기", SettingsAction::OpenFeedbackDir, false),
                ] {
                    let bw = g.measure_chrome_text(label, 13.0, false) + 28.0;
                    let r = (bx, y, bw, 34.0);
                    let live = !(primary && empty);
                    let hover = live && inside(r, ctx.cursor);
                    g.hover_pointer |= hover;
                    let bg = match (primary, live, hover) {
                        (true, true, true) => theme::accent(),
                        (true, true, false) => theme::with_alpha(theme::accent(), 200),
                        (_, false, _) => theme::surface_active(),
                        (false, _, true) => theme::surface_hover(),
                        _ => theme::surface_active(),
                    };
                    round_rect(g, r.0, r.1, r.2, r.3, theme::radius_md(), bg);
                    let fg = if primary && live { theme::bg() } else if live {
                        theme::text()
                    } else {
                        theme::text_mute()
                    };
                    g.draw_text(
                        r.0 + 14.0, r.1 + 9.0, label,
                        gpu::DrawOpts { font_size: 13.0, color: fg, bold: primary, italic: false },
                    );
                    if live {
                        rects.push((action, r));
                    }
                    bx += bw + 8.0;
                }
            }
            content_bottom = y + 34.0;
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
        round_rect(g, bx, by, box_w, box_h, theme::radius_md(), theme::with_alpha(theme::surface_active(), a));
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

/// 로그인 pane 이 뱉는 OAuth URL 을 주워 **쿠키 없는 별도 브라우저 창**으로 넘긴다.
///
/// `claude auth login` 은 기본 브라우저를 여는데, 거기엔 지금 계정의 claude.ai
/// 세션이 살아 있어 그대로 승인된다 — 슬롯을 아무리 갈라도 전부 같은 계정이 붙는
/// 이유가 이것뿐이었다(거노: "계정추가하면 1,2 같은계정으로 되는데"). 프로필이 빈
/// 창은 로그인 화면부터 뜨므로 그제서야 다른 계정을 넣을 수 있다.
///
/// 화면을 폴링하는 이유: 우리는 이 pane 의 PTY 를 소유하니 URL 이 찍히는 걸 그냥
/// 읽으면 된다. 30초 안에 못 찾으면 조용히 포기한다 — 그때는 pane 에 URL 이
/// 그대로 남아 있으니 사용자가 직접 열 수 있다.
fn spawn_oauth_browser_watch(
    ws: Arc<Mutex<Workspace>>,
    pane: String,
    profile: Option<std::path::PathBuf>,
) {
    let Some(profile) = profile else { return };
    std::thread::spawn(move || {
        for _ in 0..75 {
            std::thread::sleep(std::time::Duration::from_millis(400));
            let screen = {
                let Ok(w) = ws.lock() else { return };
                match w.panes.get(&pane) {
                    Some(p) => p.visible_text(40),
                    None => return,
                }
            };
            let Some(url) = oauth_url_in(&screen) else { continue };
            let _ = std::fs::create_dir_all(&profile);
            open_isolated_browser(&url, &profile);
            return;
        }
    });
}

/// 화면에서 OAuth authorize URL 을 뽑는다. 화면은 폭에 맞춰 접혀 있으므로 개행을
/// 걷어내고 이어 붙인다.
///
/// ⚠️ `state` 를 **정확히 43자**로 끊는 게 핵심이다. 공백까지 먹게 두면 접힌 URL
/// 뒤에 이어진 다음 줄의 첫 단어("Paste code here …")가 그대로 붙어 `…-r8Paste`
/// 가 되고, 그 링크는 invalid state 로 튕긴다. state 는 32바이트 base64url 이라
/// 길이가 항상 43이다.
fn oauth_url_in(screen: &str) -> Option<String> {
    const HEAD: &str = "https://claude.com/cai/oauth/authorize";
    const STATE_LEN: usize = 43;
    let joined: String = screen.chars().filter(|c| *c != '\n' && *c != '\r').collect();
    let tail = &joined[joined.rfind(HEAD)?..];
    let end = tail.find("state=")? + "state=".len() + STATE_LEN;
    let url = tail.get(..end)?;
    // 화면이 아직 덜 찍혔으면 state 가 43자를 못 채운다 — 그때는 다음 폴에 다시.
    let state = &url[url.len() - STATE_LEN..];
    let ok = state.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    (ok && !url.contains(' ')).then(|| url.to_string())
}

/// 프로필을 갈라 브라우저를 띄운다. 크롬이 없으면 아무것도 안 한다 — 그 경우
/// pane 에 URL 이 남아 있으니 사용자가 직접 시크릿 창에 붙여넣으면 된다.
fn open_isolated_browser(url: &str, profile: &std::path::Path) {
    #[cfg(target_os = "macos")]
    const CHROME: &str = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
    #[cfg(target_os = "windows")]
    const CHROME: &str = r"C:\Program Files\Google\Chrome\Application\chrome.exe";
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    const CHROME: &str = "google-chrome";
    if !std::path::Path::new(CHROME).exists() {
        eprintln!("[account] 크롬을 못 찾음 — URL 을 직접 시크릿 창에 여세요");
        return;
    }
    let r = std::process::Command::new(CHROME)
        .arg(format!("--user-data-dir={}", profile.display()))
        .args(["--no-first-run", "--no-default-browser-check"])
        .arg(url)
        .spawn();
    if let Err(e) = r {
        eprintln!("[account] 격리 브라우저 실행 실패: {e}");
    }
}

#[cfg(test)]
mod oauth_url_tests {
    use super::oauth_url_in;

    /// 화면에 실제로 찍히는 모양 — URL 이 폭에 맞춰 세 줄로 접히고 바로 다음 줄에
    /// 프롬프트가 온다. 접힌 걸 이어 붙이면 `state` 뒤에 `Paste` 가 그대로 달라붙는데,
    /// 그 링크는 invalid state 로 튕긴다. 오늘 로그인이 두 번 깨진 원인이 이거였다.
    const SCREEN: &str = "\
Opening browser to sign in…
If the browser didn't open, visit: https://claude.com/cai/oauth/authorize?code=true&client_id=9d1c&response_type=code&r
edirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback&code_challenge_method=S256&state=VI3QV7OJRM5
WNUMLZqCgXipW1WXeZQhoWTHm3R3Smtw
Paste code here if prompted > ";

    #[test]
    fn joins_the_wrapped_url_without_swallowing_the_next_line() {
        let url = oauth_url_in(SCREEN).expect("접힌 URL 도 복원돼야 한다");
        assert!(url.ends_with("state=VI3QV7OJRM5WNUMLZqCgXipW1WXeZQhoWTHm3R3Smtw"));
        assert!(!url.contains("Paste"), "다음 줄이 딸려 들어왔다: {url}");
    }

    #[test]
    fn waits_while_the_state_is_still_being_printed() {
        // 43자를 아직 다 못 받은 프레임은 URL 로 치지 않는다 — 반쪽 링크를 열면
        // 그 시도는 그대로 죽고 사용자는 다시 처음부터 해야 한다.
        let half = SCREEN.split("WNUMLZ").next().unwrap();
        assert_eq!(oauth_url_in(half), None);
    }

    #[test]
    fn ignores_a_screen_with_no_login_url() {
        assert_eq!(oauth_url_in("kasa@mac ~ % ls\nCargo.toml  src"), None);
    }

    #[test]
    fn takes_the_newest_url_when_an_earlier_attempt_is_still_on_screen() {
        let two = format!("{SCREEN}\n{}", SCREEN.replace("VI3QV7OJRM5", "ZZ9QV7OJRM5"));
        let url = oauth_url_in(&two).expect("두 번째 시도의 URL");
        assert!(url.contains("state=ZZ9QV7OJRM5WNUMLZ"), "옛 URL 을 골랐다: {url}");
    }
}

/// 계정 슬롯 하나의 실제 로그인 상태.
///
/// **`claude auth status` 의 email 은 쓰면 안 된다** — 그 필드는 슬롯별 저장소가
/// 아니라 공유 캐시 `~/.claude.json` 에서 온다(실측: 공유 캐시를 치우면 `loggedIn:
/// true` 인데 email 이 `null`). 그래서 어느 슬롯에 로그인하든 **모든 슬롯의 표시
/// 이메일이 방금 로그인한 계정으로** 바뀌었다(거노: "계정추가하면 1도 그거로 바뀌어").
/// 저장소는 실제로 갈려 있었고 표시만 거짓말을 하던 것이다 — 이 화면을 보고 "슬롯이
/// 겹쳤다"고 판단해 몇 시간을 엉뚱한 데 썼다.
///
/// 그래서 `logged_in` 만 `claude auth status` 에서 받고, 신원은 로컬
/// `/claude-identity`(그 슬롯의 토큰으로 `oauth/profile` 조회)에서 받는다.
#[derive(Clone)]
struct AuthProbe {
    logged_in: bool,
    email: String,
    /// 그 슬롯 토큰이 속한 조직. 이메일 하나에 개인 조직과 팀 조직이 둘 다 달려
    /// 있으면 슬롯 둘이 **같은 이메일로** 보여 어느 쪽이 회사 것인지 알 수 없다
    /// (거노: "팀플랜인지 구분하게 돼?"). 한도가 따로 도는 별개 계정이라 이걸
    /// 못 가르면 자동 전환이 어디로 갔는지도 못 읽는다.
    org: String,
}

/// 계정별 probe 캐시. 렌더가 매 프레임 도는 자리라 캐시 없이는 subprocess 폭주가
/// 된다. 값이 `None` 이면 **아직 한 번도 못 물어봤다**는 뜻 — 그동안은 아무것도
/// 안 그린다. TTL 을 두는 이유: 거노가 pane 에서 `/login` 을 마치면 클릭 없이
/// 저절로 반영돼야 한다. 함수-로컬 static 이라 `struct App` 은 안 건드린다.
///
/// 재조회를 걸 때 **옛 값을 지우지 않는다.** 예전엔 `None` 으로 덮어 "조회 중" 을
/// 표시했는데, `claude auth status` 는 로그인 셸을 거쳐 claude 를 띄우는 일이라
/// 초 단위로 걸린다 — TTL 20초마다 그만큼 계정 칸이 빈칸이 되어, 가만히 보고 있으면
/// 계정이 주기적으로 풀리는 것처럼 깜빡였다(거노 2026-08-03). git 폴러가 일시적
/// 실패에 마지막 값을 붙드는 것과 같은 이유로, 새 답이 올 때까지는 알던 값을 보인다.
fn auth_probe(id: &str) -> Option<AuthProbe> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};
    type Cache = Mutex<HashMap<String, (Instant, Option<AuthProbe>)>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let cache = CACHE.get_or_init(Cache::default);
    let stale = {
        let mut m = cache.lock().unwrap();
        let prev = match m.get(id) {
            Some((at, v)) if at.elapsed() < Duration::from_secs(20) => return v.clone(),
            Some((_, v)) => v.clone(),
            None => None,
        };
        // 시각만 새로 박아 다음 프레임이 또 스폰하지 않게 하고, 값은 그대로 둔다.
        m.insert(id.to_string(), (Instant::now(), prev.clone()));
        prev
    };
    let key = id.to_string();
    let dir = socket::claude_account_dir(id);
    std::thread::spawn(move || {
        // pane 과 같은 PATH 를 보려면 로그인 셸을 거쳐야 한다 — Finder 로 뜬 .app 의
        // PATH 에는 claude 가 없어서 직접 spawn 하면 항상 실패한다.
        let shell = resolve_default_shell().unwrap_or_else(|| "/bin/sh".to_string());
        let mut c = std::process::Command::new(shell);
        c.arg("-lc").arg("claude auth status");
        if let Some(d) = dir.as_deref() {
            c.env("CLAUDE_SECURESTORAGE_CONFIG_DIR", d);
        }
        let logged_in = c
            .output()
            .ok()
            .and_then(|o| serde_json::from_slice::<serde_json::Value>(&o.stdout).ok())
            .map(|v| v.get("loggedIn").and_then(|x| x.as_bool()).unwrap_or(false));
        let probe = logged_in.map(|logged_in| {
            // 로그인 안 된 슬롯은 물어볼 토큰이 없다 — 호출을 아낀다.
            let (email, org) = if logged_in {
                slot_identity(dir.as_deref())
            } else {
                (String::new(), String::new())
            };
            AuthProbe { logged_in, email, org }
        });
        if let Some(cache) = CACHE.get() {
            let mut m = cache.lock().unwrap();
            // 조회 자체가 실패했으면(셸이 안 뜸·JSON 이 아님) 알던 값을 유지한다 —
            // 답을 못 받은 것과 "로그인 안 됐다" 는 답을 받은 것은 다르다.
            let v = probe.or_else(|| m.get(&key).and_then(|(_, v)| v.clone()));
            if let Some(email) = v.as_ref().map(|p| p.email.as_str()).filter(|e| !e.is_empty()) {
                remember_account_email(&key, email);
            }
            m.insert(key, (Instant::now(), v));
        }
    });
    stale
}

/// 그 슬롯이 **정말로** 어느 계정인지. 로컬 `/claude-identity` 가 슬롯 토큰으로
/// `oauth/profile` 을 물어 답한다 — 공유 캐시(`~/.claude.json`)를 안 거치는 유일한
/// 경로다. 토큰은 서버 프로세스가 키체인에서 읽으니 argv 에 안 실린다.
///
/// 실패하면 빈 문자열 — 화면은 그때 이메일 자리를 비운다. **틀린 이메일을 그리는
/// 것보다 아무것도 안 그리는 게 낫다**(그 표시를 믿고 슬롯이 겹쳤다고 오판했다).
///
/// 반환은 (이메일, 조직명). 조직명은 팀 슬롯을 가르는 유일한 단서라 같이 받는다.
fn slot_identity(dir: Option<&std::path::Path>) -> (String, String) {
    let port = crate::mcp_panel_port();
    let d = dir.map(|p| p.display().to_string()).unwrap_or_default();
    // -G + --data-urlencode: 경로에 공백이나 한글이 섞여도 쿼리로 안전하게 실린다.
    let Ok(out) = std::process::Command::new("curl")
        .args(["-s", "--max-time", "12", "-G", "--data-urlencode", &format!("dir={d}")])
        .arg(format!("http://127.0.0.1:{port}/claude-identity"))
        .output()
    else {
        return (String::new(), String::new());
    };
    let field = |v: &serde_json::Value, k: &str| {
        v.get(k).and_then(|s| s.as_str()).unwrap_or_default().to_string()
    };
    serde_json::from_slice::<serde_json::Value>(&out.stdout)
        .ok()
        .filter(|v| v.get("ok").and_then(|b| b.as_bool()) == Some(true))
        .map(|v| (field(&v, "email"), field(&v, "org")))
        .unwrap_or_default()
}

/// 계정 슬롯을 부를 이름. 거노가 별명을 직접 붙였으면 그것, 아니면 그 슬롯의
/// **진짜 이메일**.
///
/// "기본"·"계정 2" 같은 자동 이름은 슬롯을 하나도 구별해 주지 않는다 — 계정
/// 행에도 드롭다운에도 전환 토스트에도 그 이름이 떠서, 정작 어느 계정으로
/// 갈아탔는지는 아무 데도 안 적혀 있었다(거노 요청 2026-08-06). 이메일은
/// `auth_probe` 캐시에서 오므로 첫 프레임엔 아직 없다 — 그동안만 `fallback`.
pub(crate) fn account_display(id: &str, label: &str, fallback: &str) -> String {
    if !label_is_auto(label) {
        return label.trim().to_string();
    }
    match auth_probe(id) {
        Some(p) if !p.email.is_empty() => p.email,
        _ => fallback.to_string(),
    }
}

/// 슬롯 이메일을 `settings.json` 에 남긴다 — **statusline 이 읽을 유일한 경로**다.
///
/// 그 스크립트는 claude 가 초당 한 번 부르는 파이썬이라 슬롯 신원을 직접 물을 수
/// 없다(로그인 셸 + HTTP 두 번). 여기서 이미 알아낸 값을 흘려 두면 공짜로 읽는다.
/// 안 남기면 이름 없는 슬롯의 statusline 이 `acct-1` 같은 내부 id 를 그린다.
fn remember_account_email(id: &str, email: &str) {
    let mut m = match socket::read_settings().get("claude_account_emails") {
        Some(serde_json::Value::Object(m)) => m.clone(),
        _ => serde_json::Map::new(),
    };
    if m.get(id).and_then(|v| v.as_str()) == Some(email) {
        return; // 값이 그대로면 쓰지 않는다 — 20초마다 파일을 다시 쓸 이유가 없다
    }
    m.insert(id.to_string(), serde_json::Value::String(email.to_string()));
    socket::write_setting("claude_account_emails", serde_json::Value::Object(m));
}

/// 우리가 붙인 이름인가. 빈 라벨과 옛 기본값 `계정 N` 을 자동으로 본다 — 후자는
/// 예전 버전이 추가할 때 파일에 박아 둔 것이라 거노가 친 별명이 아니다.
fn label_is_auto(label: &str) -> bool {
    let t = label.trim();
    t.is_empty()
        || t.strip_prefix("계정 ")
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// 화면에 띄울 조직명 — 팀 조직일 때만 Some. 개인 조직은 이름이 `<이메일>'s
/// Organization` 이라 이메일을 한 번 더 읽는 것과 다름없어 노이즈만 된다.
fn team_org(email: &str, org: &str) -> Option<String> {
    let personal = format!("{email}'s Organization");
    (!org.is_empty() && org != personal).then(|| org.to_string())
}

/// 설정 항목의 제목과 (있으면) 설명 줄들을 그리고, 컨트롤이 놓일 y 를 돌려준다.
/// 스크롤로 clip 위로 올라간 줄은 그리지 않되 자리(y 전진)는 유지한다 — 렌더러에
/// scissor 가 없어 헤더/타이틀바를 침범하지 않으려면 통째로 스킵해야 한다.
/// 항목 하나의 머리(제목 + 설명 줄). 다음 요소가 설 y 를 돌려준다.
///
/// 간격이 **제목 쪽으로 쏠려 있다** — 제목·설명·컨트롤은 한 덩어리고, 덩어리와
/// 덩어리 사이(`ROW_GAP`)가 그보다 넓어야 눈이 항목 단위로 끊어 읽는다. 예전엔
/// 제목→설명 24 · 설명→컨트롤 10 · 항목 사이 28 이라 안쪽이 바깥보다 벌어져,
/// 설명이 제 제목이 아니라 위 항목의 꼬리처럼 붙어 보였다.
fn field_header(g: &mut gpu::GpuRenderer, x: f32, y: f32, clip: f32, title: &str, help: &[&str]) -> f32 {
    if y > clip {
        section_label(g, x, y, title);
    }
    if help.is_empty() {
        return y + 26.0;
    }
    let mut hy = y + 20.0;
    for line in help {
        if hy > clip {
            help_text(g, x, hy, line);
        }
        hy += 16.0;
    }
    hy + 8.0
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
    round_rect(g, x, y, total, SEG_H, theme::radius_md(), theme::surface_active());
    let mut cxp = x + pad;
    let cell_h = SEG_H - pad * 2.0;
    for (i, (label, sel, action)) in cells.iter().enumerate() {
        let cw = widths[i];
        let cell = (cxp, y + pad, cw, cell_h);
        let hover = inside(cell, cursor);
        g.hover_pointer |= hover;
        if *sel {
            round_rect(g, cell.0, cell.1, cell.2, cell.3, theme::radius_sm(), theme::accent());
        } else if hover {
            round_rect(g, cell.0, cell.1, cell.2, cell.3, theme::radius_sm(), theme::surface_hover());
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

/// 항목 제목. 화면 제목(24)과 **두 단 아래**여야 한다 — 15 였을 때는 둘이 거의
/// 같은 크기라, 페이지 안에 항목이 나열된 게 아니라 제목만 여럿 흩어져 보였다.
fn section_label(g: &mut gpu::GpuRenderer, x: f32, y: f32, text: &str) {
    g.draw_text(
        x,
        y,
        text,
        gpu::DrawOpts { font_size: 13.5, color: theme::text(), bold: true, italic: false },
    );
}

fn help_text(g: &mut gpu::GpuRenderer, x: f32, y: f32, text: &str) {
    g.draw_text(
        x,
        y,
        text,
        gpu::DrawOpts { font_size: 11.5, color: theme::text_mute(), bold: false, italic: false },
    );
}

/// Square icon button for the font-size stepper (− / +).
fn stepper_btn(g: &mut gpu::GpuRenderer, r: Rect, glyph: &str, cursor: (f32, f32)) {
    let hover = inside(r, cursor);
    g.hover_pointer |= hover;
    round_rect(
        g, r.0, r.1, r.2, r.3, theme::radius_sm(),
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
    g.hover_pointer |= hover;
    let track = if on {
        theme::accent()
    } else if hover {
        theme::surface_hover()
    } else {
        theme::surface_active()
    };
    pill_rect(g, r.0, r.1, r.2, r.3, track);
    let knob = r.3 - 8.0;
    let kx = if on { r.0 + r.2 - knob - 4.0 } else { r.0 + 4.0 };
    circle_rect(g, kx, r.1 + 4.0, knob, theme::text());
}

/// 멀티라인 평문 편집기 — 배경 박스 + 접힌 줄 + 캐럿. 자란 높이를 돌려주므로
/// 호출부는 `y += multiline_field(..)` 로 다음 요소를 민다.
///
/// 폼이 스크롤되므로 줄 단위로 클립한다 — 화면 밖 줄까지 그리면 긴 글에서 매
/// 프레임 수백 줄을 헛그린다.
#[allow(clippy::too_many_arguments)]
fn multiline_field(
    g: &mut gpu::GpuRenderer,
    rects: &mut Vec<(SettingsAction, Rect)>,
    ctx: &SettingsCtx,
    (x, y, w): (f32, f32, f32),
    text: &str,
    caret: usize,
    field: SettingsInput,
    action: SettingsAction,
    clip: f32,
) -> f32 {
    let (_, ay, _, ah) = ctx.area;
    let line_h = 18.0_f32;
    let vis = wrap_lines(g, text, w - 24.0, 13.0);
    let box_h = (vis.len() as f32 * line_h + 16.0).max(56.0);
    let focused = ctx.input == Some(field);
    if y < ay + ah && y + box_h > clip {
        let bg = if focused { theme::surface_hover() } else { theme::surface_active() };
        round_rect(g, x, y, w, box_h, theme::radius_md(), bg);
        rects.push((action, (x, y, w, box_h)));
    }
    let (caret_vl, caret_col) = visual_caret(&vis, caret);
    let mut ly = y + 9.0;
    for (vi, (line, _)) in vis.iter().enumerate() {
        if ly > clip && ly < ay + ah - line_h {
            g.draw_text(
                x + 12.0, ly, line,
                gpu::DrawOpts { font_size: 13.0, color: theme::text(), bold: false, italic: false },
            );
            if focused && ctx.caret_on && vi == caret_vl {
                let pre: String = line.chars().take(caret_col).collect();
                let cx = x + 12.0 + g.measure_chrome_text(&pre, 13.0, false);
                g.rect(cx, ly, 1.5, line_h - 2.0, theme::accent());
            }
        }
        ly += line_h;
    }
    box_h
}

/// 단일라인 텍스트 필드. `caret` 는 문자(char) 인덱스라 문자열 중간에도 캐럿을
/// 그린다 — 캐럿 앞 부분 폭을 재서 그 x 에 1.5px 세로 막대를 세운다.
/// 한글 낱자(호환 자모 블록). 완성 음절 "계"(U+ACC4)는 여기 안 든다 — 조합을
/// 마친 글자와 조합 재료를 가르는 선이 이 함수다.
fn is_jamo(c: char) -> bool {
    (0x3130..=0x318F).contains(&(c as u32))
}

/// `preedit` 는 조합 중인 한글 음절 — 포커스된 필드에서만, 캐럿 자리에 밑줄을 깔고
/// 그린다. 아직 버퍼에 없는 글자라 캐럿은 그 오른쪽으로 밀어 둔다.
fn text_field(
    g: &mut gpu::GpuRenderer,
    r: Rect,
    text: &str,
    caret: usize,
    focused: bool,
    caret_on: bool,
    cursor: (f32, f32),
    preedit: &str,
) {
    let hover = inside(r, cursor);
    g.hover_pointer |= hover;
    round_rect(g, r.0, r.1, r.2, r.3, theme::radius_sm(), theme::surface_active());
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
    let pre: String = text.chars().take(caret).collect();
    let mut cx = tx + g.measure_chrome_text(&pre, 13.0, false);
    if focused && !preedit.is_empty() {
        let pw = g.measure_chrome_text(preedit, 13.0, false);
        // 캐럿 뒤 글자를 덮으므로 배경부터 깐다. accent 를 옅게 쓰는 건 밑줄과
        // 한 덩어리로 읽히게 하려는 것 — 회색 판이면 필드 배경에 가라앉는다.
        g.rect(cx, r.1 + 4.0, pw, r.3 - 8.0, theme::with_alpha(theme::accent(), 0x33));
        g.draw_text(cx, ty, preedit,
            gpu::DrawOpts { font_size: 13.0, color: theme::text(), bold: false, italic: false });
        g.rect(cx, ty + 15.0, pw, 1.0, theme::accent());
        cx += pw;
    }
    // 조합 중에는 깜빡임을 멈춘다 — 조합 글자와 캐럿이 번갈아 사라지면 어디까지
    // 쳤는지 읽을 수가 없다.
    if focused && (caret_on || !preedit.is_empty()) {
        g.rect(cx, r.1 + 7.0, 1.5, r.3 - 14.0, theme::text());
    }
}

#[cfg(test)]
mod account_label_tests {
    use super::label_is_auto;

    #[test]
    fn auto_labels_yield_to_email() {
        // 우리가 붙인 것 — 이메일로 대체돼야 한다
        for l in ["", "  ", "계정 2", "계정 10"] {
            assert!(label_is_auto(l), "자동으로 안 봤다: {l:?}");
        }
        // 거노가 친 별명 — 이메일이 있어도 이게 이긴다
        for l in ["개인계정", "사이오닉팀플랜", "계정", "계정 팀", "계정 2호"] {
            assert!(!label_is_auto(l), "별명을 자동으로 봤다: {l:?}");
        }
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
        use super::visual_caret as vc;
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
