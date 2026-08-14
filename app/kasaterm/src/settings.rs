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
//! Segmented controls (`segmented`), the two-column row primitives (`row2`,
//! `row_wide`) and the single-line text fields (`text_field`, char-index caret)
//! are shared helpers so every page reads consistently. Single-line fields
//! borrow `students_caret` as their caret store since only one field is
//! focused at a time.

use super::*;

type Rect = (f32, f32, f32, f32);

const CAT_W: f32 = 200.0;

/// 테마 카드가 이보다 좁아지면 열을 줄인다. 미리보기 얼굴 셋이 겹치지 않는
/// 최소 폭이다.
const THEME_CARD_MIN_W: f32 = 218.0;
const THEME_CARD_H: f32 = 136.0;
const THEME_GAP: f32 = 12.0;
/// 미리보기 얼굴 한 칸의 높이. 프사 원본이 전신이라 세로로 길다 — 정사각으로
/// 잡으면 contain-fit 이 가로 여백을 크게 남겨 얼굴이 작아진다.
const THEME_FACE_H: f32 = 72.0;

/// 캐릭터 카드의 최소 폭. 79명이 격자로 늘어서므로 한 장이 작아야 한 화면에
/// 여러 줄이 들어오고, 그래도 얼굴이 누구인지 알아볼 만큼은 커야 한다.
const STU_CARD_MIN_W: f32 = 88.0;
const STU_CARD_H: f32 = 128.0;
const STU_GAP: f32 = 10.0;

/// 남는 폭 없이 꽉 채우는 격자 — `(열 수, 카드 폭)`.
///
/// 카드 폭을 고정하면 오른쪽에 늘 어중간한 빈 띠가 남아, 카드가 카드 안에서
/// 왼쪽으로 쏠린 것처럼 보인다(2026-08-13 지적: "박스 제대로 안맞아"). 열 수만
/// 폭에서 정하고 남는 자리는 카드들이 나눠 갖게 하면 양끝이 폼에 딱 맞는다.
fn grid_fit(avail: f32, min_w: f32, gap: f32) -> (usize, f32) {
    let cols = (((avail + gap) / (min_w + gap)).floor() as usize).max(1);
    (cols, (avail - gap * (cols - 1) as f32) / cols as f32)
}

/// 설명 문구에 박히는 주 수식키 이름. 실제 바인딩은 이미 갈려 있는데
/// (`zoom_mod = macos ? Cmd : Ctrl`, 편집기 저장도 같다) 문구만 Cmd 로 고정돼
/// 있어서, Windows 사용자에게 **키보드에 없는 키**를 안내하고 있었다.
const PRIMARY_MOD: &str = if cfg!(target_os = "macos") { "Cmd" } else { "Ctrl" };

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
    /// 팔레트 hex 편집 버퍼 — 포커스된 칸의 타이핑 중 값. 완성 전 반쪽 값은
    /// custom_theme 에 없고 여기에만 있다.
    pub palette_edit: String,
    /// custom_theme 의 지금 유효값 27칸(#rrggbb) — `theme::PALETTE_KEYS` 11개
    /// 뒤에 ANSI 16개. 스냅샷에서 한 번 계산해 paint 가 파일을 다시 안 읽게 한다.
    pub palette_hex: Vec<String>,
    /// 색 선택기의 HSV 상태(h: 0..360, s·v: 0..1) — SV 사각형·Hue 마커 위치.
    pub picker_hsv: (f32, f32, f32),
    pub accent: String,
    pub font_size: f32,
    /// 전체 UI 배율(1.0 = 100%). 화면에 숫자로 보여야 얼마나 틀어졌는지 안다.
    pub ui_zoom: f32,
    /// PixelDelta 스크롤 감도 배율(트랙패드·고해상도 마우스휠 공용).
    pub wheel_pixel_gain: f32,
    pub tabs_on_top: bool,
    /// 터미널 커서 모양 — `"block"` · `"bar"` · `"underline"`.
    pub cursor_shape: String,
    /// bar·underline 커서 굵기(논리 px).
    pub cursor_thickness: f32,
    /// 터미널 셀 위 마우스 포인터 — `"arrow"` · `"ibeam"`.
    pub mouse_cursor: String,
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
    /// 같은 것의 codex(ChatGPT) 판.
    pub codex_accounts: Vec<socket::CodexAccount>,
    pub codex_account: String,
    /// 한도가 차면 다음 계정으로 알아서 넘어간다 + 그 임계 사용률(%).
    pub account_autoswitch: bool,
    pub account_autoswitch_pct: f32,
    /// (표시명, 에셋 슬러그) — Theme 카테고리 목록·프사 썸네일용. slug 가
    /// None 이면 아직 도트 에셋이 없는 캐릭터(썸네일 자리표시).
    pub characters: Vec<(String, Option<&'static str>)>,
    /// 고를 수 있는 캐릭터 테마들 — 번들이 맨 앞이고, 그 뒤가
    /// `~/.config/kasaterm/themes/` 에서 찾은 것들. 미리보기 얼굴까지 실어 온다.
    pub themes: Vec<socket::ThemeRow>,
    /// 지금 고른 테마 id. 빈 문자열이면 번들.
    pub theme_active: String,
    /// 이름을 고치는 중인 테마 `(폴더 id, 편집 버퍼)`.
    pub theme_label_edit: Option<(String, String)>,
    /// 단일라인 텍스트 필드(경로·셸·claude extra)의 캐럿(문자 인덱스).
    /// persona 멀티라인 캐럿(`student_caret`)과 분리 — 한 번에 한 필드만
    /// 포커스되지만, 저장소를 나눠 포커스 이동 시 캐럿이 튀지 않게 한다.
    pub settings_caret: usize,
    /// 캐릭터 상세 — 열려 있는 캐릭터(None 이면 목록 화면)·persona 버퍼·캐럿.
    pub student_selected: Option<String>,
    pub student_persona: String,
    pub student_caret: usize,
    /// 상세 화면의 이름 편집 버퍼. `student_selected` 는 **저장된** 이름이라
    /// 타이핑 중에는 둘이 어긋난다 — 그림·persona 조회는 저장된 쪽을 쓴다.
    pub student_name: String,
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

/// 라벨 폭에 맞춰 늘어나는 작은 액션 버튼. 그린 사각형을 돌려주므로 부르는 쪽이
/// 그대로 `rects` 에 담아 클릭 대상으로 쓴다.
fn chip(g: &mut gpu::GpuRenderer, x: f32, y: f32, label: &str, cursor: (f32, f32)) -> Rect {
    let r = (x, y, g.measure_chrome_text(label, 13.0, false) + 28.0, 34.0);
    let hover = inside(r, cursor);
    g.hover_pointer |= hover;
    round_rect(
        g, r.0, r.1, r.2, r.3, theme::radius_md(),
        if hover { theme::surface_hover() } else { theme::surface_active() },
    );
    g.draw_text(
        r.0 + 14.0, r.1 + 9.0, label,
        gpu::DrawOpts { font_size: 13.0, color: theme::text(), bold: false, italic: false },
    );
    r
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
            "codex_accounts",
            serde_json::to_value(&self.set_codex_accounts).unwrap_or(serde_json::Value::Null),
        );
        socket::write_setting("codex_account", serde_json::Value::String(self.set_codex_account.clone()));
        socket::write_setting("usage_compact", serde_json::Value::Bool(self.set_usage_compact));
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
        // codex 는 래퍼를 다시 굽지 않는다 — 값이 하나도 안 박힌 정적 문자열이라
        // 다시 구울 이유가 없고, 활성 슬롯 경로만 파일로 갈아 끼우면 **이미 떠 있는
        // pane 도 다음 codex 실행부터** 그 계정으로 뜬다.
        if let Ok(dir) = std::env::var("KASATERM_TMUX_SHIM_DIR") {
            crate::write_codex_account_file(std::path::Path::new(&dir));
        }
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
        // 목록에 없어도 **폴더가 남아 있으면 쓰지 않는다.** 슬롯을 지울 때 키체인
        // 항목과 폴더는 일부러 두는데(자격은 되돌릴 수 없다), 번호만 보고 고르면
        // 지운 계정의 토큰을 새 계정이 그대로 물려받아 「새로 만들었는데 옛 계정으로
        // 로그인돼 있는」 상태가 된다.
        let id = (1..)
            .map(|n| format!("acct-{n}"))
            .find(|c| {
                self.set_claude_accounts.iter().all(|a| &a.id != c)
                    && socket::claude_account_dir(c).is_none_or(|d| !d.exists())
            })
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

        // 로그인은 **터미널 없이** 돈다(`spawn_hidden_login` 주석). 그래서 설정창을
        // 닫지도, pane 을 띄우지도 않는다 — 이 자리에 「로그인 중… / 취소」가 뜬다.
        // 슬롯마다 쿠키 없는 브라우저 프로필을 갈라 주는 건 그대로다: `claude auth
        // login` 이 기본 브라우저를 열면 지금 계정의 claude.ai 세션이 그대로 승인돼
        // 슬롯 전부가 같은 계정이 됐다(거노: "계정추가하면 1,2 같은계정으로 되는데").
        spawn_hidden_login(
            "Claude",
            id.clone(),
            "claude auth login --claudeai".to_string(),
            "CLAUDE_SECURESTORAGE_CONFIG_DIR",
            dir,
            socket::oauth_profile_dir(&id),
        );
        self.set_toast("빈 브라우저 창에서 새 계정으로 로그인하세요".to_string());
    }

    /// 같은 것의 codex 판 — 슬롯을 만들고 그 홈을 얹은 `codex login` 을 새 pane 에
    /// 띄운다. claude 와 마찬가지로 **추가만 하고 활성 전환은 안 한다**(아직 아무도
    /// 로그인 안 한 슬롯으로 갈아타면 그 뒤 codex 가 전부 로그아웃 상태로 뜬다).
    ///
    /// id 접두사를 `codex-` 로 갈라 두는 건 OAuth 브라우저 프로필 때문이다 —
    /// `oauth_profile_dir` 은 id 하나로 자리를 잡으므로 claude 의 `acct-1` 과 겹치면
    /// 두 서비스가 같은 브라우저 프로필을 나눠 쓰게 된다.
    fn add_codex_account(&mut self) {
        let id = (1..)
            .map(|n| format!("codex-{n}"))
            .find(|c| self.set_codex_accounts.iter().all(|a| &a.id != c))
            .expect("1.. is infinite");
        let Some(dir) = socket::codex_account_dir(&id) else {
            self.set_toast("계정 폴더 경로를 만들 수 없습니다".to_string());
            return;
        };
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.set_toast(format!("계정 폴더 생성 실패: {e}"));
            return;
        }
        self.set_codex_accounts
            .push(socket::CodexAccount { id: id.clone(), label: String::new() });
        self.settings_save();

        // claude 와 같은 숨은 로그인. `login` 은 shim 이 **순정으로 통과**시키는 관리
        // 서브커맨드라(우리 홈을 씌우면 엉뚱한 자리를 본다), 여기서 준 CODEX_HOME 이
        // 그대로 진짜 codex 에 닿아 이 슬롯에 auth.json 을 쓴다.
        spawn_hidden_login(
            "Codex",
            id.clone(),
            "codex login".to_string(),
            "CODEX_HOME",
            dir,
            socket::oauth_profile_dir(&id),
        );
        self.set_toast("빈 브라우저 창에서 새 ChatGPT 계정으로 로그인하세요".to_string());
    }

    /// 학생 이미지 override 폴더(`~/.config/kasaterm/students/`)를 OS 파일
    /// 매니저로 연다 — 없으면 먼저 만든다.
    /// 학생 그림 폴더를 연다. **비어 있으면 지금 쓰는 그림을 그 자리에 풀고** 연다.
    ///
    /// 번들 그림은 `include_bytes` 라 파일로 존재하지 않는다 — 그래서 예전엔 폴더를
    /// 만들어 열기만 했고, 사용자는 텅 빈 창을 봤다(2026-08-13 지적: "이미지 폴더
    /// 열면 아무것도없어"). 무엇을 어떤 이름으로 넣어야 하는지 알 방법이 그 창에
    /// 하나도 없었다는 뜻이다. `open_characters_json` 이 빈 파일 대신 현재 정본을
    /// seed 하는 것과 같은 원칙을 그림에도 적용한다.
    fn open_students_dir(&mut self) {
        let Some(dir) = socket::students_dir() else { return };
        let _ = std::fs::create_dir_all(&dir);
        let empty = std::fs::read_dir(&dir).map_or(true, |mut it| it.next().is_none());
        if empty {
            match render::export_student_sprites(&dir) {
                Ok(n) => self.set_toast(format!("지금 그림 {n}장을 풀었어요 — 고친 뒤 새로고침")),
                Err(e) => self.set_toast(format!("본보기를 못 풀었어요: {e}")),
            }
        } else {
            // 이미 그림이 든 폴더는 통째로 다시 풀지 않는다(사용자 파일을 덮는다).
            // 그래도 사용법은 있어야 한다 — 옛 평면 구조로 채워 둔 사람이 폴더가
            // 나뉜 걸 알 방법이 이 파일 말고 없다.
            let _ = render::write_sprite_readme(&dir);
        }
        open_path(&dir);
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
            // 테마 카드 미리보기는 키가 갈려 있어 위 한 줄로는 안 걷힌다 — 남겨
            // 두면 그림을 갈아 끼운 테마의 카드만 옛 얼굴로 남는다.
            g.drop_images_with_prefix("theme:");
            g.drop_image("schale:logo");
        }
        // 대기 gif 는 GPU 텍스처가 아니라 **디코딩된 프레임**으로 따로 캐시된다 —
        // 위 축출만으로는 안 걷혀, 새로 넣은 gif 가 화면에 안 온다.
        render::invalidate_idle_anim();
        // 이 버튼의 뜻이 곧 "파일을 다시 봐라"다. 손으로 폴더를 넣거나 지운 경우는
        // 캐시가 알 길이 없으니, 목록 자체도 여기서 함께 다시 읽는다.
        socket::invalidate_theme_rows();
        kasa_mcp::character::invalidate_active_theme();
        theme::invalidate_roster();
        self.repaint_all();
    }

    /// 캐릭터 테마를 갈아 끼운다(빈 id = 번들).
    ///
    /// 세 캐시를 **함께** 비워야 한다 — 테마 폴더 해석, 이름↔슬러그·색 로스터,
    /// GPU 스프라이트 텍스처. 하나라도 남으면 화면이 두 테마를 섞어 보여 주는데,
    /// 그건 "덜 바뀐 것" 이 아니라 사용자에겐 그냥 고장으로 읽힌다.
    ///
    /// 이미 도는 pane 의 persona 는 안 바뀐다 — 셸 spawn 시 env 로 고정되기
    /// 때문이다(session.rs). 그래서 토스트로 그 사실을 알린다. 조용히 안 바뀌면
    /// 사용자는 전환이 실패했다고 읽는다.
    fn select_theme(&mut self, id: String) {
        if self.settings_cat == SettingsCat::Theme && socket::read_character_theme() == id {
            return;
        }
        // 편집 중이던 persona 는 **먼저** 옛 테마 파일에 흘려보낸다. 순서를 바꾸면
        // 활성 테마가 이미 새것이라, 옛 테마에서 고친 글이 새 테마 파일에 쓰인다.
        self.flush_student_persona();
        socket::write_setting("character_theme", serde_json::Value::String(id));
        kasa_mcp::character::invalidate_active_theme();
        theme::invalidate_roster();
        // 목록의 "쓰는 중" 배지가 어느 카드에 붙는지가 여기서 바뀐다.
        socket::invalidate_theme_rows();
        self.students_selected = None;
        self.students_persona.clear();
        self.students_caret = 0;
        self.refresh_student_assets();
        self.set_toast("테마를 바꿨어요 — 새로 여는 pane 부터 적용돼요".to_string());
    }

    /// 지금 로스터와 그림을 `themes/<id>/` 로 복제한다 — 새 테마를 만들 본보기.
    ///
    /// 이게 없으면 사용자는 80명치 JSON 과 파일명 규칙을 맨손으로 맞춰야 한다.
    /// 복제한 테마를 곧바로 활성화하지는 않는다 — 내용이 번들과 똑같아서, 켜 봤자
    /// 아무것도 안 바뀐 것처럼만 보인다. 편집하고 나서 고르는 순서가 맞다.
    fn export_theme(&mut self) {
        match socket::create_theme("") {
            Ok(dir) => {
                socket::invalidate_theme_rows();
                let name = dir.file_name().and_then(|s| s.to_str()).unwrap_or("theme").to_string();
                // 만들자마자 이름 칸에 포커스를 준다 — 새 테마에서 사용자가 제일
                // 먼저 하려는 게 이름 짓기고, 안 그러면 `my-theme` 이 그대로 굳는다.
                self.focus_theme_label(name.clone());
                self.set_toast(format!("'{name}' 을 만들었어요 — 이름부터 지으세요"));
            }
            Err(e) => self.set_toast(format!("새 테마를 못 만들었어요: {e}")),
        }
    }

    fn open_theme_dir(&mut self, id: &str) {
        match socket::theme_dir(id) {
            Some(d) => open_path(&d),
            // 번들은 폴더가 없다(그림이 바이너리 안에 있다) — 여기서 조용히
            // 아무것도 안 하면 버튼이 고장 난 것으로 읽히므로 갈 길을 알려 준다.
            None => self.set_toast("기본 테마는 폴더가 없어요 — 새 테마를 만들면 생겨요".into()),
        }
    }

    /// 테마를 목록에서 치운다. 지우던 게 지금 쓰는 테마면 번들로 되돌린다 —
    /// 안 그러면 없는 폴더를 가리킨 채로 남아, 화면은 번들인데 설정은 사라진
    /// 테마 이름을 보여 주는 어긋난 상태가 된다.
    fn delete_theme(&mut self, id: &str) {
        match socket::delete_theme(id) {
            Ok(dest) => {
                socket::invalidate_theme_rows();
                if socket::read_character_theme() == id {
                    self.select_theme(String::new());
                }
                if self.theme_label_edit.as_ref().is_some_and(|(t, _)| t == id) {
                    self.theme_label_edit = None;
                    self.settings_input = None;
                }
                kasa_mcp::character::invalidate_active_theme();
                let where_to = dest.parent().and_then(|p| p.file_name()).and_then(|s| s.to_str())
                    .unwrap_or("_trash").to_string();
                self.set_toast(format!("치웠어요 — 지운 건 아니고 {where_to}/ 에 있어요"));
            }
            Err(e) => self.set_toast(format!("못 치웠어요: {e}")),
        }
    }

    /// 테마 이름 칸에 포커스를 준다 — 지금 이름을 버퍼로 읽어 온다.
    fn focus_theme_label(&mut self, id: String) {
        // 다른 테마를 고치던 중이었으면 그것부터 굳힌다. 안 그러면 버퍼가 새 테마
        // 것으로 갈아 끼워지면서 앞에서 친 이름이 어디에도 안 남고 사라진다.
        self.flush_theme_label();
        let label = self
            .settings_snapshot_themes()
            .into_iter()
            .find(|(t, _)| *t == id)
            .map(|(_, l)| l)
            .unwrap_or_else(|| id.clone());
        self.settings_caret = label.chars().count();
        self.theme_label_edit = Some((id, label));
        self.settings_input = Some(SettingsInput::ThemeLabel);
    }

    /// 편집 중이던 테마 이름을 그 테마의 `theme.json` 에 굳힌다.
    pub(crate) fn flush_theme_label(&mut self) {
        let Some((id, label)) = self.theme_label_edit.clone() else { return };
        if let Err(e) = socket::rename_theme(&id, &label) {
            self.set_toast(format!("이름을 못 바꿨어요: {e}"));
            return;
        }
        // 목록 표시명은 활성 테마 해석과 같은 캐시를 타므로 함께 비워야 한다.
        kasa_mcp::character::invalidate_active_theme();
        socket::invalidate_theme_rows();
        self.theme_label_edit = None;
    }

    /// 테마 목록 `(폴더 id, 표시명)` — 번들은 목록에 없다(폴더가 없어서).
    fn settings_snapshot_themes(&self) -> Vec<(String, String)> {
        kasa_mcp::character::list_themes()
    }

    /// Build the render snapshot for the settings paint. `area` is the logical
    /// rect the form draws into (the whole aux settings-window client area) and
    /// `cursor` is that window's local cursor — both supplied by the caller so
    /// this stays a pure `&self` snapshot taken outside any gpu borrow.
    pub(crate) fn settings_snapshot(&self, area: Rect, cursor: (f32, f32)) -> SettingsCtx {
        let s = socket::read_settings();
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
            has_custom_theme: s.get("custom_theme").is_some(),
            palette_edit: self.set_palette_edit.clone(),
            palette_hex: palette_hex_list(&s),
            picker_hsv: self.set_picker_hsv,
            accent: theme::accent_name().to_string(),
            shape: theme::shape_name().to_string(),
            font_size: self.font_size,
            ui_zoom: self.ui_zoom,
            wheel_pixel_gain: self.set_wheel_pixel_gain,
            tabs_on_top: self.tabs_on_top,
            cursor_shape: self.cursor_shape.clone(),
            cursor_thickness: self.cursor_thickness,
            mouse_cursor: self.mouse_cursor.clone(),
            claude_persona: self.set_claude_persona,
            shim_inject: self.set_shim_inject,
            claude_model: self.set_claude_model.clone(),
            claude_effort: self.set_claude_effort.clone(),
            claude_extra: self.set_claude_extra.clone(),
            claude_accounts: self.set_claude_accounts.clone(),
            claude_account: self.set_claude_account.clone(),
            codex_accounts: self.set_codex_accounts.clone(),
            codex_account: self.set_codex_account.clone(),
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
            themes: socket::theme_rows(),
            theme_active: socket::read_character_theme(),
            theme_label_edit: self.theme_label_edit.clone(),
            settings_caret: self.settings_caret,
            student_selected: self.students_selected.clone(),
            student_persona: self.students_persona.clone(),
            student_caret: self.students_caret,
            student_name: self.students_name.clone(),
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
            .map(|(a, r)| (a.clone(), *r));
        let Some((action, rect)) = hit else {
            // A click anywhere inside the settings area that misses a control
            // just drops text focus; clicks land here only while the screen is
            // open, so this never eats terminal input.
            self.flush_student_persona();
            self.flush_theme_label();
            self.settings_input = None;
            self.chrome_dirty = true;
            return true;
        };
        // 피커 면은 dispatch 전에 가로챈다 — 클릭 지점이 곧 값인데(연속 좌표)
        // 액션 enum 은 그걸 못 싣는다. press 에서 드래그를 잡아 release 까지
        // 커서를 따라간다(auxwin CursorMoved/Released 가 짝).
        if matches!(action, SettingsAction::PickerSV | SettingsAction::PickerHue) {
            self.settings_drag = Some((action.clone(), rect));
            self.picker_pick(&action, rect, (cx, cy));
            return true;
        }
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
                        self.set_cwd_mode = kasa_socket::home_dir()
                            .map(|p| p.to_string_lossy().into_owned())
                            .unwrap_or_default();
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
            SettingsAction::StartCustomTheme => {
                // 이미 편집하던 custom_theme 이 있으면 그대로 이어 간다 — 입구
                // 버튼을 다시 눌렀다고 하던 편집이 날아가면 안 된다. 리셋은
                // ResetCustomTheme 이 따로 맡는다.
                if socket::read_settings().get("custom_theme").is_none() {
                    let base = if theme::theme_name() == "system" {
                        theme::system_theme_key()
                    } else {
                        theme::theme_name()
                    };
                    socket::write_setting("custom_theme", theme::custom_theme_seed(base));
                }
                self.begin_theme_fx();
                theme::set_theme("custom");
                socket::write_setting("theme", serde_json::Value::String("custom".to_string()));
                self.repaint_all();
            }
            SettingsAction::ResetCustomTheme => {
                let base = socket::read_settings()
                    .get("custom_theme")
                    .and_then(|o| o.get("base"))
                    .and_then(|x| x.as_str())
                    .unwrap_or("dark")
                    .to_string();
                socket::write_setting("custom_theme", theme::custom_theme_seed(&base));
                self.settings_input = None;
                self.begin_theme_fx();
                theme::set_theme("custom");
                self.repaint_all();
            }
            SettingsAction::FocusPaletteHex(i) => {
                self.set_palette_edit = self.palette_hex_at(i);
                self.settings_caret = self.set_palette_edit.chars().count();
                // 피커 시드 — RGB→HSV 역산은 여기 한 번뿐이다. 매 픽마다
                // 역산하면 s=0·v=0 에서 색상(H)이 소실돼 핸들이 튄다.
                if let Some(c) = theme::parse_hex(&self.set_palette_edit) {
                    self.set_picker_hsv = rgb_to_hsv(c);
                }
                self.settings_input = Some(SettingsInput::PaletteHex(i));
            }
            // 피커 면은 settings_click 이 좌표와 함께 가로챈다(picker_pick) —
            // 좌표 없는 이 경로로 오면 고를 값이 없다.
            SettingsAction::PickerSV | SettingsAction::PickerHue => {}
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
            SettingsAction::CursorShape(shape) => {
                if self.cursor_shape != shape {
                    self.cursor_shape = shape.to_string();
                    socket::write_setting(
                        "cursor_shape",
                        serde_json::Value::String(shape.to_string()),
                    );
                    // 커서는 셀 그리드 위에 그려지므로 chrome 만 더럽히면 안 바뀐다.
                    self.chrome_dirty = true;
                }
            }
            SettingsAction::MouseCursor(kind) => {
                if self.mouse_cursor != kind {
                    self.mouse_cursor = kind.to_string();
                    socket::write_setting(
                        "mouse_cursor",
                        serde_json::Value::String(kind.to_string()),
                    );
                    // 포인터는 다음 마우스 이동에서 갱신된다. 지금 커서가 터미널 위에
                    // 있으면 그 전환을 놓치므로(판정이 값이 **바뀔 때만** set_cursor 를
                    // 부른다) 표시 상태를 풀어 다음 move 가 반드시 다시 세우게 한다.
                    self.text_cursor_shown = false;
                    self.chrome_dirty = true;
                }
            }
            SettingsAction::CursorThickness(px) => {
                let want = (px as f32).clamp(1.0, 6.0);
                if (self.cursor_thickness - want).abs() > 0.01 {
                    self.cursor_thickness = want;
                    socket::write_setting("cursor_thickness", serde_json::Value::from(px));
                    self.chrome_dirty = true;
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
            // 있는 슬롯에 로그인을 다시 돌린다 — 슬롯 dir 을 그대로 쓰므로 그 계정에
            // 붙은 한도 이력이 남는다. 새로 만들었다 지우는 것과 여기가 갈린다.
            SettingsAction::ReauthAccount(p, id) => {
                let (provider, argv, key, dir) = match p {
                    AccountProvider::Claude => (
                        "Claude",
                        "claude auth login --claudeai",
                        "CLAUDE_SECURESTORAGE_CONFIG_DIR",
                        socket::claude_account_dir(&id),
                    ),
                    AccountProvider::Codex => (
                        "Codex",
                        "codex login",
                        "CODEX_HOME",
                        socket::codex_account_dir(&id),
                    ),
                };
                let Some(dir) = dir else {
                    self.set_toast("계정 폴더 경로를 만들 수 없습니다".to_string());
                    return;
                };
                let _ = std::fs::create_dir_all(&dir);
                // id 접두사가 `acct-`/`codex-` 로 갈려 있어 프로필도 그대로 갈린다.
                let profile = socket::oauth_profile_dir(&id);
                spawn_hidden_login(provider, id, argv.to_string(), key, dir, profile);
                self.set_toast("빈 브라우저 창에서 로그인하세요".to_string());
            }
            SettingsAction::CancelLogin => cancel_login(),
            SettingsAction::DismissLogin => clear_login_job(),
            SettingsAction::RemoveClaudeAccount(id) => {
                self.set_claude_accounts.retain(|a| a.id != id);
                // 지운 계정이 활성이었으면 기본 로그인으로 — 아무도 로그인할 수
                // 없는 저장소를 계속 가리키면 pane 이 통째로 로그아웃 상태로 뜬다.
                if self.set_claude_account == id {
                    self.set_claude_account = String::new();
                }
                // 슬롯에 딸린 곁 기록도 함께 지운다. 목록에서만 빼면 이메일 표와
                // 쿨다운에 유령 키가 남아, 지웠는데도 뭔가 계속 남아 있는 것처럼
                // 보인다(거노 2026-08-13: "슬롯정리가 안 되던데" — 실제로 지운
                // acct-2 의 키가 두 표에 다 남아 있었다).
                //
                // ⚠️ 키체인 항목과 슬롯 폴더는 **일부러 안 지운다**. 자격은 되돌릴 수
                // 없고, 실수로 지운 슬롯을 같은 번호로 다시 만들면 로그인이 그대로
                // 살아 있는 편이 낫다. 대신 번호 재사용을 막아(`add_claude_account`)
                // 남의 토큰을 물려받는 사고를 없앴다.
                forget_account_email(&id);
                socket::forget_account_cooldown(&id);
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
            SettingsAction::CodexAccount(id) => {
                self.set_codex_account = id;
                self.settings_input = None;
                self.settings_save();
            }
            SettingsAction::AddCodexAccount => self.add_codex_account(),
            SettingsAction::RemoveCodexAccount(id) => {
                self.set_codex_accounts.retain(|a| a.id != id);
                // claude 판과 같은 이유 — 사라진 슬롯을 가리키면 codex 가 아무도
                // 로그인 못 하는 자리를 본다. auth.json 은 남긴다(재로그인 말고는
                // 복구가 없고, 남겨 둬도 해가 없다).
                if self.set_codex_account == id {
                    self.set_codex_account = String::new();
                }
                self.settings_input = None;
                self.settings_save();
            }
            SettingsAction::FocusCodexAccountLabel(i) => {
                self.settings_caret = self
                    .set_codex_accounts
                    .get(i)
                    .map_or(0, |a| a.label.chars().count());
                self.settings_input = Some(SettingsInput::CodexAccountLabel(i));
            }
            SettingsAction::OpenStudentsDir => self.open_students_dir(),
            SettingsAction::OpenCharactersJson => self.open_characters_json(),
            SettingsAction::RefreshStudentAssets => self.refresh_student_assets(),
            SettingsAction::SelectTheme(id) => self.select_theme(id),
            SettingsAction::ExportTheme => self.export_theme(),
            SettingsAction::OpenThemeDir(id) => self.open_theme_dir(&id),
            SettingsAction::DeleteTheme(id) => self.delete_theme(&id),
            SettingsAction::FocusThemeLabel(id) => self.focus_theme_label(id),
            SettingsAction::SelectStudent(name) => self.select_student_for_edit(name),
            SettingsAction::CloseStudent => self.close_student_edit(),
            SettingsAction::FocusStudentName => {
                self.settings_caret = self.students_name.chars().count();
                self.settings_input = Some(SettingsInput::StudentName);
            }
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

    /// 웹뷰 설정 화면이 누른 컨트롤 하나 → 네이티브 액션 하나.
    ///
    /// `handler.rs` 의 `SocketSettingsAction` 이 Theme 탭 액션만 알고 나머지는
    /// "모르는 액션" 으로 돌려보내던 자리에서 여기로 떨어진다. 값을 직접 쓰지 않고
    /// `settings_apply` 를 태우는 이유는 저장이 파일 쓰기로 끝나지 않기 때문이다 —
    /// shim 재생성 · PTY reshape · 캐시 무효화가 액션마다 짝으로 붙어 있고, 웹뷰용
    /// 저장 경로를 따로 파면 그 뒤처리가 둘로 갈린다.
    ///
    /// 반환값은 「반영됐는가」. 네이티브에 없는 검사(빈 값 · 없는 프리셋)가 여기만
    /// 붙는 것은 HTTP 가 아무 문자열이나 보낼 수 있어서다 — 네이티브에선 그려진
    /// 칸만 눌린다.
    pub(crate) fn settings_web_action(
        &mut self,
        action: &str,
        id: &str,
        label: Option<&str>,
    ) -> Result<bool, String> {
        /// 프리셋 목록에서 `&'static str` 를 되찾는다 — 액션 enum 이 `&'static str`
        /// 를 실으므로 요청 문자열을 그대로 넣을 수 없다. 목록에 없으면 거부라, 이
        /// 조회가 곧 화이트리스트 노릇을 한다.
        fn pick(list: &[&'static str], want: &str) -> Option<&'static str> {
            list.iter().copied().find(|k| *k == want)
        }
        // 토글은 「눌렀다」만으로는 반영을 알 수 없다 — 파일에 남은 값이 메모리와
        // 같은지로 판정한다(`toggle-persona` 와 같은 규칙).
        let saved_bool = |key: &str| socket::read_settings().get(key).and_then(|v| v.as_bool());
        let unknown = |v: &str| {
            reject_with(
                "value_not_allowed",
                serde_json::json!({ "value": v }),
                format!("'{v}' 은(는) 고를 수 없는 값이에요"),
            )
        };
        let no_slot = |v: &str| {
            reject_with(
                "account_missing",
                serde_json::json!({ "account": v }),
                format!("'{v}' 계정이 없어요"),
            )
        };
        let arg = label.unwrap_or("").trim().to_string();
        match action {
            // ── 화면 데이터 ──────────────────────────────────────────────
            "values" => {
                publish_web_values(self.settings_values_json());
                Ok(true)
            }
            // Esc 로 창 닫기. 네이티브 설정 화면은 키 핸들러가 직접 닫는데, 웹뷰
            // 창은 키가 WKWebView 로 가 그 경로에 닿지 않는다(거노 2026-08-15
            // "esc왜안되냐") — 페이지가 keydown 을 잡아 이 액션으로 되돌린다.
            "close-settings" => {
                self.close_settings_web_window();
                Ok(self.settings_web_window.is_none())
            }

            // ── General ──────────────────────────────────────────────────
            "cwd-mode" => {
                let m = pick(&["last", "home", "custom"], id).ok_or_else(|| unknown(id))?;
                self.settings_apply(SettingsAction::CwdMode(m));
                // custom 은 네이티브에서 경로 칸에 커서를 세운다 — 웹에는 그 커서가
                // 없으니 걷어내야 다음 키가 엉뚱한 필드로 새지 않는다.
                self.settings_input = None;
                Ok(true)
            }
            "cwd-path" => {
                if arg.is_empty() {
                    return Err(reject("cwd_path_empty", "경로를 비울 수 없어요".to_string()));
                }
                self.set_cwd_mode = arg.clone();
                self.settings_save();
                Ok(self.set_cwd_mode == arg)
            }
            "file-open-mode" => {
                let m = pick(&["builtin", "app", "terminal"], id).ok_or_else(|| unknown(id))?;
                self.settings_apply(SettingsAction::FileOpenMode(m));
                self.settings_input = None;
                Ok(self.set_file_open_mode == m)
            }
            "file-open-app" => {
                // 빈 문자열 = OS 연결 프로그램(네이티브의 "기본 앱" 칸).
                if !id.is_empty() && !crate::proc::open_with_apps().iter().any(|(n, _)| n == id) {
                    return Err(reject_with(
                        "app_not_found",
                        serde_json::json!({ "app": id }),
                        format!("'{id}' 앱을 못 찾았어요"),
                    ));
                }
                self.settings_apply(SettingsAction::FileOpenApp(id.to_string()));
                Ok(self.set_file_open_app == id)
            }
            "file-open-cmd" => {
                if arg.is_empty() {
                    return Err(reject("file_open_cmd_empty", "명령을 비울 수 없어요".to_string()));
                }
                self.set_file_open_cmd = arg.clone();
                self.settings_save();
                Ok(self.set_file_open_cmd == arg)
            }
            "toggle-file-tree" => {
                self.settings_apply(SettingsAction::ToggleFileTree);
                Ok(saved_bool("file_tree_default") == Some(self.set_file_tree_default))
            }
            "toggle-footer" => {
                self.settings_apply(SettingsAction::ToggleFooter);
                Ok(saved_bool("pane_footer_default") == Some(self.set_footer_default))
            }
            "autosave-delay" => {
                let ms: u64 = id.parse().map_err(|_| unknown(id))?;
                if !matches!(ms, 0 | 1000 | 3000 | 10000) {
                    return Err(unknown(id));
                }
                self.settings_apply(SettingsAction::AutosaveDelay(ms));
                Ok(self.set_autosave.map_or(0, |d| d.as_millis() as u64) == ms)
            }
            "tab-position" => {
                let p = pick(&["top", "side"], id).ok_or_else(|| unknown(id))?;
                self.settings_apply(SettingsAction::TabPosition(p));
                Ok(self.tabs_on_top == (p == "top"))
            }
            "cursor-shape" => {
                let s = pick(&["block", "bar", "underline"], id).ok_or_else(|| unknown(id))?;
                self.settings_apply(SettingsAction::CursorShape(s));
                Ok(self.cursor_shape == s)
            }
            "cursor-thickness" => {
                let px: u8 = id.parse().map_err(|_| unknown(id))?;
                if !(1..=4).contains(&px) {
                    return Err(unknown(id));
                }
                self.settings_apply(SettingsAction::CursorThickness(px));
                Ok((self.cursor_thickness - px as f32).abs() < 0.01)
            }
            "mouse-cursor" => {
                let k = pick(&["arrow", "ibeam"], id).ok_or_else(|| unknown(id))?;
                self.settings_apply(SettingsAction::MouseCursor(k));
                Ok(self.mouse_cursor == k)
            }
            "wheel-gain" => {
                let x100: u32 = id.parse().map_err(|_| unknown(id))?;
                if !matches!(x100, 30 | 60 | 100 | 150) {
                    return Err(unknown(id));
                }
                self.settings_apply(SettingsAction::WheelPixelGain(x100));
                Ok((self.set_wheel_pixel_gain * 100.0).round() as u32 == x100)
            }

            // ── Appearance ───────────────────────────────────────────────
            "theme-mode" => {
                let key = if id == "system" {
                    "system"
                } else if id == "custom" {
                    // 편집본이 없으면 고를 수 없다 — 네이티브도 그때는 카드를 아예
                    // 안 그린다. 만드는 건 `start-custom-theme` 의 몫이다.
                    if socket::read_settings().get("custom_theme").is_none() {
                        return Err(reject(
                            "custom_theme_absent",
                            "커스텀 팔레트를 아직 만들지 않았어요".to_string(),
                        ));
                    }
                    "custom"
                } else {
                    theme::THEME_PRESETS
                        .iter()
                        .find(|(k, _, _)| *k == id)
                        .map(|(k, _, _)| *k)
                        .ok_or_else(|| {
                            reject_with(
                                "theme_missing",
                                serde_json::json!({ "theme": id }),
                                format!("'{id}' 테마가 없어요"),
                            )
                        })?
                };
                self.settings_apply(SettingsAction::ThemeMode(key));
                Ok(theme::theme_name() == key)
            }
            // system 모드의 밝기 슬롯에 테마 배정 — id 는 프리셋 키 또는 "custom".
            // "system" 자신은 못 들어간다(자기참조). 지금 system 으로 보는 중이면
            // 그 자리에서 다시 해석해 갈아입는다.
            "theme-system-light" | "theme-system-dark" => {
                let light = action == "theme-system-light";
                let key = if id == "custom" {
                    if socket::read_settings().get("custom_theme").is_none() {
                        return Err(reject(
                            "custom_theme_absent",
                            "커스텀 팔레트를 아직 만들지 않았어요".to_string(),
                        ));
                    }
                    "custom"
                } else {
                    theme::THEME_PRESETS
                        .iter()
                        .find(|(k, _, _)| *k == id)
                        .map(|(k, _, _)| *k)
                        .ok_or_else(|| {
                            reject_with(
                                "theme_missing",
                                serde_json::json!({ "theme": id }),
                                format!("'{id}' 테마가 없어요"),
                            )
                        })?
                };
                socket::write_setting(
                    if light { "theme_system_light" } else { "theme_system_dark" },
                    serde_json::Value::String(key.to_string()),
                );
                if theme::theme_name() == "system" {
                    self.begin_theme_fx();
                    theme::set_theme("system");
                    self.repaint_all();
                }
                Ok(theme::system_slot_theme(light) == key)
            }
            "start-custom-theme" => {
                self.settings_apply(SettingsAction::StartCustomTheme);
                Ok(theme::theme_name() == "custom")
            }
            "reset-custom-theme" => {
                self.settings_apply(SettingsAction::ResetCustomTheme);
                Ok(true)
            }
            "palette-hex" => {
                let i: usize = id.parse().map_err(|_| unknown(id))?;
                if i >= theme::PALETTE_KEYS.len() + 16 {
                    return Err(reject("palette_slot_missing", "없는 색 칸이에요".to_string()));
                }
                if theme::parse_hex(&arg).is_none() {
                    return Err(reject("hex_invalid", "#rrggbb 꼴로 적어 주세요".to_string()));
                }
                // 네이티브는 타이핑 버퍼(`set_palette_edit`)를 거쳐 굳힌다. 웹에는 그
                // 버퍼가 없으니 완성된 값을 심고 같은 커밋을 태운다.
                self.set_palette_edit = arg.clone();
                self.apply_palette_edit(i);
                self.settings_input = None;
                Ok(self.palette_hex_at(i).eq_ignore_ascii_case(&arg))
            }
            "accent" => {
                let name = theme::ACCENT_PRESETS
                    .iter()
                    .find(|(n, _)| *n == id)
                    .map(|(n, _)| *n)
                    .ok_or_else(|| unknown(id))?;
                self.settings_apply(SettingsAction::Accent(name.to_string()));
                Ok(theme::accent_name() == name)
            }
            "shape" => {
                let key = theme::SHAPE_PRESETS
                    .iter()
                    .find(|(k, _, _)| *k == id)
                    .map(|(k, _, _)| *k)
                    .ok_or_else(|| unknown(id))?;
                self.settings_apply(SettingsAction::Shape(key));
                Ok(theme::shape_name() == key)
            }
            "min-contrast" => {
                let (l, v) = theme::CONTRAST_PRESETS
                    .iter()
                    .find(|(l, _)| *l == id)
                    .ok_or_else(|| unknown(id))?;
                self.settings_apply(SettingsAction::MinContrast(l));
                Ok((theme::min_contrast() - v).abs() < 0.001)
            }
            "font-size-delta" | "ui-zoom-delta" => {
                let d: i8 = id.parse().map_err(|_| unknown(id))?;
                if !matches!(d, -1 | 1) {
                    return Err(reject("step_out_of_range", "한 칸씩만 움직일 수 있어요".to_string()));
                }
                let font = action == "font-size-delta";
                let before = if font { self.font_size } else { self.ui_zoom };
                self.settings_apply(if font {
                    SettingsAction::FontSizeDelta(d)
                } else {
                    SettingsAction::UiZoomDelta(d)
                });
                let after = if font { self.font_size } else { self.ui_zoom };
                // 끝값(9..32px · 50..300%)에 닿으면 안 움직인다. 그건 고장이 아니라
                // 더 갈 곳이 없다는 뜻이라, 조용한 실패 대신 문구로 말한다.
                if (after - before).abs() < 0.001 {
                    return Err(reject("step_at_limit", "더는 못 가요".to_string()));
                }
                Ok(true)
            }
            "reset-scale" => {
                self.settings_apply(SettingsAction::ResetScale);
                Ok(true)
            }

            // ── Shell ────────────────────────────────────────────────────
            "shell-preset" => {
                // 빈 문자열 = 시스템 $SHELL. 나머지는 네이티브 칸과 같은 둘뿐이다.
                if !matches!(id, "" | "/bin/zsh" | "/bin/bash") {
                    return Err(unknown(id));
                }
                self.settings_apply(SettingsAction::ShellPreset(id.to_string()));
                Ok(self.set_shell == id)
            }
            "shell-custom" => {
                if arg.is_empty() {
                    return Err(reject("shell_path_empty", "셸 경로를 비울 수 없어요".to_string()));
                }
                self.set_shell = arg.clone();
                self.settings_input = None;
                self.settings_save();
                Ok(self.set_shell == arg)
            }

            // ── Claude ───────────────────────────────────────────────────
            "toggle-shim-inject" => {
                self.settings_apply(SettingsAction::ToggleShimInject);
                Ok(saved_bool("shim_inject") == Some(self.set_shim_inject))
            }
            "claude-model" => {
                if !matches!(id, "" | "opus" | "sonnet" | "haiku") {
                    return Err(unknown(id));
                }
                self.settings_apply(SettingsAction::ClaudeModel(id.to_string()));
                Ok(self.set_claude_model == id)
            }
            "claude-effort" => {
                if !matches!(id, "" | "low" | "medium" | "high" | "xhigh") {
                    return Err(unknown(id));
                }
                self.settings_apply(SettingsAction::ClaudeEffort(id.to_string()));
                Ok(self.set_claude_effort == id)
            }
            "claude-extra" => {
                // 여기만 빈 값이 정상이다 — 붙일 플래그가 없다는 뜻이다.
                self.set_claude_extra = arg.clone();
                self.settings_save();
                Ok(self.set_claude_extra == arg)
            }
            "claude-account" => {
                if !id.is_empty() && !self.set_claude_accounts.iter().any(|a| a.id == id) {
                    return Err(no_slot(id));
                }
                self.settings_apply(SettingsAction::ClaudeAccount(id.to_string()));
                Ok(self.set_claude_account == id)
            }
            "add-claude-account" => {
                let before = self.set_claude_accounts.len();
                self.settings_apply(SettingsAction::AddClaudeAccount);
                Ok(self.set_claude_accounts.len() > before)
            }
            "remove-claude-account" => {
                if !self.set_claude_accounts.iter().any(|a| a.id == id) {
                    return Err(no_slot(id));
                }
                self.settings_apply(SettingsAction::RemoveClaudeAccount(id.to_string()));
                Ok(!self.set_claude_accounts.iter().any(|a| a.id == id))
            }
            "claude-account-label" => {
                let i = self
                    .set_claude_accounts
                    .iter()
                    .position(|a| a.id == id)
                    .ok_or_else(|| no_slot(id))?;
                // 라벨은 비워도 된다 — 그러면 그 슬롯의 진짜 이메일로 불린다
                // (`account_display`). 그래서 빈 값 검사가 없다.
                self.set_claude_accounts[i].label = arg.clone();
                self.settings_save();
                Ok(self.set_claude_accounts[i].label == arg)
            }
            "codex-account" => {
                if !id.is_empty() && !self.set_codex_accounts.iter().any(|a| a.id == id) {
                    return Err(no_slot(id));
                }
                self.settings_apply(SettingsAction::CodexAccount(id.to_string()));
                Ok(self.set_codex_account == id)
            }
            "add-codex-account" => {
                let before = self.set_codex_accounts.len();
                self.settings_apply(SettingsAction::AddCodexAccount);
                Ok(self.set_codex_accounts.len() > before)
            }
            "remove-codex-account" => {
                if !self.set_codex_accounts.iter().any(|a| a.id == id) {
                    return Err(no_slot(id));
                }
                self.settings_apply(SettingsAction::RemoveCodexAccount(id.to_string()));
                Ok(!self.set_codex_accounts.iter().any(|a| a.id == id))
            }
            "codex-account-label" => {
                let i = self
                    .set_codex_accounts
                    .iter()
                    .position(|a| a.id == id)
                    .ok_or_else(|| no_slot(id))?;
                self.set_codex_accounts[i].label = arg.clone();
                self.settings_save();
                Ok(self.set_codex_accounts[i].label == arg)
            }
            // 로그인 수단이 갈리므로(claude 는 `claude auth login`, codex 는
            // `CODEX_HOME=<슬롯> codex login`) 어느 쪽인지를 label 로 받는다.
            "reauth-account" => {
                let claude = match arg.as_str() {
                    "claude" => true,
                    "codex" => false,
                    other => return Err(unknown(other)),
                };
                let known = if claude {
                    self.set_claude_accounts.iter().any(|a| a.id == id)
                } else {
                    self.set_codex_accounts.iter().any(|a| a.id == id)
                };
                if !known {
                    return Err(no_slot(id));
                }
                let provider =
                    if claude { AccountProvider::Claude } else { AccountProvider::Codex };
                self.settings_apply(SettingsAction::ReauthAccount(provider, id.to_string()));
                Ok(true)
            }
            "toggle-account-autoswitch" => {
                self.settings_apply(SettingsAction::ToggleAccountAutoswitch);
                Ok(saved_bool("claude_account_autoswitch") == Some(self.set_account_autoswitch))
            }
            "autoswitch-pct" => {
                let p: u32 = id.parse().map_err(|_| unknown(id))?;
                if !matches!(p, 80 | 85 | 90 | 95) {
                    return Err(unknown(id));
                }
                self.settings_apply(SettingsAction::AccountAutoswitchPct(p));
                Ok(self.set_account_autoswitch_pct.round() as u32 == p)
            }

            // ── Feedback ─────────────────────────────────────────────────
            "toggle-feedback-diag" => {
                self.settings_apply(SettingsAction::ToggleFeedbackDiag);
                Ok(true)
            }
            "save-feedback" => {
                if arg.is_empty() {
                    return Err(reject("feedback_empty", "무엇이 불편했는지 적어 주세요".to_string()));
                }
                // 네이티브는 편집 버퍼를 저장한다 — 웹에는 그 버퍼가 없으니 본문을
                // 심고 같은 저장을 태운다. 성공하면 `save_feedback` 이 버퍼를 비우므로
                // 비었는지가 곧 판정이다.
                self.feedback_body = arg;
                self.feedback_caret = self.feedback_body.chars().count();
                self.settings_apply(SettingsAction::SaveFeedback);
                Ok(self.feedback_body.is_empty())
            }
            "open-feedback-dir" => {
                self.settings_apply(SettingsAction::OpenFeedbackDir);
                Ok(true)
            }
            other => Err(reject_with(
                "action_unknown",
                serde_json::json!({ "action": other }),
                format!("모르는 액션이에요: {other}"),
            )),
        }
    }

    /// 웹뷰 설정 화면이 그릴 값 전부 — 카테고리마다 하위 객체 하나씩이라 탭이
    /// 늘어도 라우트는 늘지 않는다.
    ///
    /// 파일(`settings.json`)이 아니라 **메모리 값**을 싣는다. 둘은 대개 같지만 UI
    /// 배율처럼 애초에 저장되지 않는 값이 있어(세션 한정), 파일에서 읽으면 그 칸이
    /// 늘 기본값을 보여 준다.
    ///
    /// 계정 행의 이름·부제는 여기서 조립해 넘긴다. 그 규칙(라벨이 없으면 이메일,
    /// 팀 조직이면 이어 붙이기, 이름과 겹치면 걷어내기)은 네이티브 폼이 이미 쥐고
    /// 있어, 웹에서 다시 짜면 두 화면이 같은 계정을 다르게 부르게 된다.
    fn settings_values_json(&self) -> serde_json::Value {
        let hex = |c: [u8; 4]| format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]);
        let hex3 = |c: [u8; 3]| format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]);
        let s = socket::read_settings();

        // 테마 카드 한 장의 미리보기 재료. `pal` 이 없으면(custom) 지금 화면에
        // 적용된 색으로 그린다 — 고른 상태에서는 그게 곧 그 팔레트다.
        let card = |key: &str, label: String, pal: Option<&theme::Palette>| {
            let ansi: Vec<String> = (1..7)
                .map(|i| match pal {
                    Some(p) => hex3(p.ansi[i]),
                    None => hex3(theme::ansi16(i)),
                })
                .collect();
            serde_json::json!({
                "key": key,
                "label": label,
                "bg": pal.map_or_else(|| hex(theme::bg()), |p| hex(p.bg)),
                "text": pal.map_or_else(|| hex(theme::text()), |p| hex(p.text)),
                "dim": pal.map_or_else(|| hex(theme::text_mute()), |p| hex(p.text_mute)),
                "ansi": ansi,
            })
        };
        let sys_key = theme::system_theme_key();
        let sys = theme::THEME_PRESETS.iter().find(|(k, _, _)| *k == sys_key);
        let mut themes = vec![card(
            "system",
            match sys {
                Some((_, l, _)) => format!("System · {l}"),
                None => "System".to_string(),
            },
            sys.map(|(_, _, p)| *p),
        )];
        themes.extend(
            theme::THEME_PRESETS.iter().map(|(k, l, p)| card(k, l.to_string(), Some(p))),
        );
        if s.get("custom_theme").is_some() {
            themes.push(card("custom", "Custom".to_string(), None));
        }

        // 계정 한 행. 첫 행은 언제나 "기본"(슬롯이 아니라 지금 로그인)이라 지울
        // 것도 이름 붙일 것도 없다 — `slot: false` 가 그 뜻이다.
        let claude_rows: Vec<serde_json::Value> = std::iter::once((String::new(), String::new(), None))
            .chain(
                self.set_claude_accounts
                    .iter()
                    .enumerate()
                    .map(|(i, a)| (a.id.clone(), a.label.clone(), Some(i))),
            )
            .map(|(id, label, idx)| {
                let probe = auth_probe(&id);
                // 답이 아직 없는 두 경우(첫 조회 중 · 토큰 갱신 중)에 비우지 않는다.
                // 비우면 계정이 사라진 것처럼 보인다 — 없다고 말하지 말고 아직
                // 모른다고 말한다.
                //
                // 코드가 붙는 것은 우리가 지어낸 말 셋뿐이다. 이메일·조직명은
                // 데이터라 옮길 것이 없다.
                let (mut sub, kind, mut sub_code) = match &probe {
                    Some(p) if !p.logged_in => {
                        ("로그인 필요".to_string(), "danger", Some("account_login_required"))
                    }
                    Some(p) if !p.email.is_empty() => (p.email.clone(), "mute", None),
                    _ => ("확인 중…".to_string(), "faint", Some("account_checking")),
                };
                if let Some(org) = probe.as_ref().and_then(|p| team_org(&p.email, &p.org)) {
                    sub = format!("{sub} · {org}");
                    // 조직명이 붙으면 더는 통문장이 아니다 — 코드로 갈면 조직이
                    // 사라지므로 그때는 서버 문구를 그대로 쓰게 둔다.
                    sub_code = None;
                }
                let numbered = idx.map(|i| format!("계정 {}", i + 2));
                let name = match (&idx, &numbered) {
                    (None, _) => "기본".to_string(),
                    (Some(_), Some(fb)) => account_display(&id, &label, fb),
                    (Some(_), None) => String::new(),
                };
                // 라벨도 이메일도 없어 번호로 부르는 경우만 옮길 말이다.
                let name_code = match (&idx, &numbered) {
                    (None, _) => Some("account_default"),
                    (Some(_), Some(fb)) if *fb == name => Some("account_numbered"),
                    _ => None,
                };
                let sub = match sub.strip_prefix(name.as_str()) {
                    Some(rest) => rest.trim_start_matches(" · ").to_string(),
                    None => sub,
                };
                if sub.is_empty() {
                    sub_code = None;
                }
                serde_json::json!({
                    "id": id, "label": label, "name": name,
                    "name_code": name_code,
                    "name_args": idx.map(|i| serde_json::json!({ "n": i + 2 })),
                    "sub": sub, "sub_kind": kind, "sub_code": sub_code,
                    "slot": idx.is_some(),
                })
            })
            .collect();

        let codex_rows: Vec<serde_json::Value> = std::iter::once((String::new(), String::new(), None))
            .chain(
                self.set_codex_accounts
                    .iter()
                    .enumerate()
                    .map(|(i, a)| (a.id.clone(), a.label.clone(), Some(i))),
            )
            .map(|(id, label, idx)| {
                // claude 판과 달리 "확인 중" 이 없다 — 신원이 파일 하나에 들어 있어
                // 즉시 읽힌다. 값이 없으면 정말로 로그인 안 한 슬롯이다.
                let ident = codex_identity(&id);
                let name = match (idx, label.is_empty()) {
                    (None, _) => "기본".to_string(),
                    (Some(i), true) => ident.clone().unwrap_or_else(|| format!("계정 {}", i + 2)),
                    (Some(_), false) => label.clone(),
                };
                let name_code = match (idx, label.is_empty()) {
                    (None, _) => Some("account_default"),
                    // 이메일도 라벨도 없어 번호로 부르는 경우만 옮길 말이다.
                    (Some(_), true) if ident.is_none() => Some("account_numbered"),
                    _ => None,
                };
                let sub = if idx.is_some() && label.is_empty() && ident.is_some() {
                    String::new()
                } else {
                    ident.clone().unwrap_or_else(|| "로그인 필요".to_string())
                };
                serde_json::json!({
                    "id": id,
                    "label": label,
                    "name": name,
                    "name_code": name_code,
                    "name_args": idx.map(|i| serde_json::json!({ "n": i + 2 })),
                    "sub": sub,
                    "sub_kind": if ident.is_some() { "mute" } else { "danger" },
                    "sub_code": (ident.is_none() && !sub.is_empty())
                        .then_some("account_login_required"),
                    "slot": idx.is_some(),
                })
            })
            .collect();

        // 최소 대비 칸의 샘플 글자색. 카드 배경에서 글자색 쪽으로 아주 조금 민
        // 색에서 출발한다 — 고정 회색으로 두면 다크 팔레트에선 이미 잘 보여 네 칸이
        // 전부 같아 보인다. 끌어올린 **결과만** 넘기는 이유는 대비 계산을 웹으로
        // 옮기면 두 화면의 판정이 갈려서다.
        let (sf, tx) = (theme::surface(), theme::text());
        let mut sample = [0u8, 0, 0, 0xFF];
        for i in 0..3 {
            sample[i] = (sf[i] as f32 + (tx[i] as f32 - sf[i] as f32) * 0.18).round() as u8;
        }
        let contrasts: Vec<serde_json::Value> = theme::CONTRAST_PRESETS
            .iter()
            .map(|(l, v)| {
                serde_json::json!({
                    "label": l,
                    "value": v,
                    "sample": hex(theme::enforce_contrast_at(sample, sf, *v)),
                })
            })
            .collect();

        serde_json::json!({
            "general": {
                "cwd_mode": self.set_cwd_mode,
                "file_open_mode": self.set_file_open_mode,
                "file_open_app": self.set_file_open_app,
                "file_open_cmd": self.set_file_open_cmd,
                // 설치된 것만 뜬다. 이 목록에 없는 앱을 쓰는 사람의 탈출구가
                // 빈 문자열(OS 연결 프로그램)이라, 그 칸은 웹이 직접 붙인다.
                "apps": crate::proc::open_with_apps()
                    .iter()
                    .map(|(name, _)| serde_json::json!({
                        "name": name, "short": crate::info::short_app_name(name),
                    }))
                    .collect::<Vec<_>>(),
                "file_tree_default": self.set_file_tree_default,
                "footer_default": self.set_footer_default,
                "autosave_ms": self.set_autosave.map_or(0, |d| d.as_millis() as u64),
                "tabs_on_top": self.tabs_on_top,
                "cursor_shape": self.cursor_shape,
                "cursor_thickness": self.cursor_thickness,
                "mouse_cursor": self.mouse_cursor,
                "wheel_gain_x100": (self.set_wheel_pixel_gain * 100.0).round() as u32,
            },
            "appearance": {
                "theme": theme::theme_name(),
                "themes": themes,
                // system 모드가 밝기별로 입을 테마(프리셋 키 또는 "custom") —
                // OS 는 밝기만 알려 주고 팔레트는 사용자가 배정한다(2026-08-15).
                "theme_system_light": theme::system_slot_theme(true),
                "theme_system_dark": theme::system_slot_theme(false),
                "has_custom_theme": s.get("custom_theme").is_some(),
                "palette_keys": theme::PALETTE_KEYS.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
                "palette_hex": palette_hex_list(&s),
                "accent": theme::accent_name(),
                "accents": theme::ACCENT_PRESETS
                    .iter()
                    .map(|(n, c)| serde_json::json!({ "name": n, "hex": hex(*c) }))
                    .collect::<Vec<_>>(),
                "shape": theme::shape_name(),
                // 카드가 **자기 실루엣으로** 그려져야 고르기 전에 형태가 눈에
                // 보인다(네이티브 카드와 같은 재료다) — 그래서 라벨만이 아니라
                // 모서리·테두리·그림자·둥글기를 함께 넘긴다.
                "shapes": theme::SHAPE_PRESETS
                    .iter()
                    .map(|(k, l, s)| serde_json::json!({
                        "key": k,
                        "label": l,
                        "radius_md": s.radius_md,
                        "border_w": s.border_w,
                        "shadow_offset": s.shadow_offset,
                        "roundness": s.roundness,
                    }))
                    .collect::<Vec<_>>(),
                "min_contrast": theme::min_contrast(),
                "contrasts": contrasts,
                "font_size": self.font_size,
                "font_size_default": socket::DEFAULT_FONT_SIZE,
                "ui_zoom": self.ui_zoom,
            },
            "shell": { "shell": self.set_shell },
            "claude": {
                "shim_inject": self.set_shim_inject,
                "persona": self.set_claude_persona,
                "accounts": claude_rows,
                "account": self.set_claude_account,
                "codex_accounts": codex_rows,
                "codex_account": self.set_codex_account,
                "autoswitch": self.set_account_autoswitch,
                "autoswitch_pct": self.set_account_autoswitch_pct.round() as u32,
                "model": self.set_claude_model,
                "effort": self.set_claude_effort,
                "extra": self.set_claude_extra,
            },
            "feedback": {
                "diag": diag_line(),
                "diag_on": self.feedback_diag,
            },
        })
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
                SettingsInput::CodexAccountLabel(i) => match self.set_codex_accounts.get_mut(i) {
                    Some(a) => &mut a.label,
                    None => return true,
                },
                SettingsInput::PaletteHex(_) => &mut self.set_palette_edit,
                // 계정 라벨과 같은 이유로 대상이 사라졌을 수 있다(다른 곳에서
                // 테마를 지웠거나 폴더가 없어졌거나) — 그땐 키를 삼키고 흘린다.
                SettingsInput::ThemeLabel => match self.theme_label_edit.as_mut() {
                    Some((_, buf)) => buf,
                    None => return true,
                },
                SettingsInput::StudentName => &mut self.students_name,
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
            // 팔레트 hex 는 settings_save 대상이 아니다 — 버퍼가 App 설정 필드가
            // 아니라서 저장할 곳이 custom_theme 쪽이고, 완성된 값만 나간다.
            if let SettingsInput::PaletteHex(i) = field {
                self.apply_palette_edit(i);
            } else if field != SettingsInput::ThemeLabel {
                // 테마 이름만 여기서 빠진다 — 그 값은 settings.json 이 아니라 79명치
                // 로스터가 든 theme.json 에 사는데, 매 키마다 그 파일을 통째로 읽고
                // 다시 쓰면 타이핑이 눈에 띄게 끌린다. 화면엔 편집 버퍼가 그대로
                // 보이므로 파일에 굳히는 건 blur·Enter 한 번이면 된다.
                self.settings_save();
            }
        }
        if (blur || commit) && field == SettingsInput::ThemeLabel {
            self.flush_theme_label();
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

    /// 그 캐릭터의 상세 화면을 연다 — 편집 중이던 것은 먼저 저장하고, 원본
    /// persona·이름을 버퍼로 로드한다. 카드 클릭과 별도창 딥링크(프사 클릭 →
    /// Theme 페이지 + 그 캐릭터)가 공유한다.
    ///
    /// 포커스는 **주지 않는다**. 상세는 목록을 통째로 덮는 화면이라, 열자마자
    /// 커서가 성격 칸에 있으면 뒤로 가려고 누른 키가 본문에 박힌다.
    pub(crate) fn select_student_for_edit(&mut self, name: String) {
        self.flush_student_persona();
        self.flush_student_name();
        let persona = kasa_mcp::character::characters_json()
            .and_then(|c| kasa_mcp::character::raw_persona_for(&c, &name))
            .unwrap_or_default();
        self.students_caret = persona.chars().count();
        self.students_persona = persona;
        self.students_name = name.clone();
        self.students_selected = Some(name);
        self.settings_input = None;
        // 목록을 한참 내려서 골랐어도 상세는 맨 위부터 — 이어받으면 빈 화면이 뜬다.
        self.settings_scroll = 0.0;
    }

    /// 상세를 닫고 목록으로. 편집 중이던 것은 여기서 굳힌다(persona 를 **먼저** —
    /// 저장 키가 이름이라, 이름부터 바꾸면 옛 이름 자리에 쓰려다 못 찾는다).
    pub(crate) fn close_student_edit(&mut self) {
        self.flush_student_persona();
        self.flush_student_name();
        self.students_selected = None;
        self.students_persona.clear();
        self.students_name.clear();
        self.settings_input = None;
        self.settings_scroll = 0.0;
    }

    /// 이름 버퍼를 로스터에 굳힌다. 이름은 로스터의 **키**라 바꾸면 그 캐릭터의
    /// persona·색·그림 조회가 통째로 새 이름을 따라간다 — 그래서 되돌릴 수 없는
    /// 두 경우를 먼저 막는다: 빈 이름(그 캐릭터가 로스터에서 사라진다)과 중복
    /// (로스터 빌드가 뒤엣것을 통째로 버려 한 명이 증발한다).
    pub(crate) fn flush_student_name(&mut self) {
        let Some(old) = self.students_selected.clone() else { return };
        let new = self.students_name.trim().to_string();
        if new == old {
            return;
        }
        let reject = if new.is_empty() {
            Some("이름은 비울 수 없어요".to_string())
        } else if theme::character_slugs().iter().any(|(n, _)| *n == new) {
            Some(format!("{new} 은(는) 이미 있어요"))
        } else if kasa_mcp::character::update_member(
            &old, "name", serde_json::Value::String(new.clone()),
        )
        .is_err()
        {
            Some("이름을 못 저장했어요".to_string())
        } else {
            None
        };
        if let Some(msg) = reject {
            // 버퍼를 되돌린다 — 안 되돌리면 화면엔 새 이름이 남아 저장된 것처럼
            // 보이는데 파일은 옛 이름 그대로다.
            self.students_name = old;
            self.set_toast(msg);
            return;
        }
        // 로스터는 `&'static` 으로 구운 캐시라, 무효화하지 않으면 이름·색·슬러그가
        // 옛 값 그대로 남는다.
        theme::invalidate_roster();
        socket::invalidate_theme_rows();
        self.students_selected = Some(new);
        self.regen_claude_shim();
        self.repaint_all();
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
            SettingsInput::CodexAccountLabel(i) => match self.set_codex_accounts.get_mut(i) {
                Some(a) => &mut a.label,
                None => return,
            },
            SettingsInput::PaletteHex(_) => &mut self.set_palette_edit,
            SettingsInput::ThemeLabel => match self.theme_label_edit.as_mut() {
                Some((_, buf)) => buf,
                None => return,
            },
            SettingsInput::StudentName => &mut self.students_name,
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
        if let SettingsInput::PaletteHex(i) = field {
            self.apply_palette_edit(i);
        } else if !matches!(field, SettingsInput::ThemeLabel | SettingsInput::StudentName) {
            // 이 둘은 settings.json 이 아니라 로스터에 산다 — 키 입력 경로와 같은
            // 이유로 blur·Enter 때 한 번만 굳힌다.
            self.settings_save();
        }
        self.chrome_dirty = true;
    }

    /// 팔레트 칸 `i` 의 지금 유효값(#rrggbb) — custom_theme 에 적힌 값, 없으면
    /// base 프리셋 값. 포커스 시드·리스트 표시가 같은 곳을 읽어야 「눌렀더니
    /// 다른 값이 뜨는」 어긋남이 없다.
    fn palette_hex_at(&self, i: usize) -> String {
        palette_hex_list(&socket::read_settings())
            .into_iter()
            .nth(i)
            .unwrap_or_else(|| "#000000".to_string())
    }

    /// 색 선택기의 한 픽 — press(settings_click)와 드래그(auxwin CursorMoved)
    /// 양쪽이 부른다. rect 밖 커서는 클램프해 가장자리 값으로 잇는다: 드래그
    /// 중 손이 살짝 벗어났다고 픽이 끊기면 끝값(0·1·순수 원색)을 잡을 수 없다.
    pub(crate) fn picker_pick(
        &mut self,
        action: &SettingsAction,
        r: (f32, f32, f32, f32),
        p: (f32, f32),
    ) {
        let Some(SettingsInput::PaletteHex(i)) = self.settings_input else { return };
        let rx = ((p.0 - r.0) / r.2.max(1.0)).clamp(0.0, 1.0);
        let ry = ((p.1 - r.1) / r.3.max(1.0)).clamp(0.0, 1.0);
        let (h, s, v) = self.set_picker_hsv;
        self.set_picker_hsv = match action {
            // 360.0 은 0.0 과 같은 색이지만 마커가 왼쪽 끝으로 감겨 보인다.
            SettingsAction::PickerHue => ((rx * 360.0).min(359.9), s, v),
            _ => (h, rx, 1.0 - ry),
        };
        let (h, s, v) = self.set_picker_hsv;
        self.set_palette_edit = theme::hex_str(hsv_to_rgb(h, s, v));
        self.settings_caret = self.set_palette_edit.chars().count();
        self.apply_palette_edit(i);
        self.chrome_dirty = true;
    }

    /// 팔레트 hex 버퍼를 검증해 custom_theme 에 반영하고 즉시 다시 칠한다.
    /// 6자리 hex 가 아직 아니면(타이핑 중) 아무것도 안 한다 — 반쯤 친 값으로
    /// 화면이 튀는 것보다 완성되는 순간에만 따라오는 쪽이 읽기 좋다.
    fn apply_palette_edit(&mut self, i: usize) {
        let Some(c) = theme::parse_hex(&self.set_palette_edit) else { return };
        // 타이핑으로 들어온 새 색이면 피커 핸들도 따라온다. 피커 픽이면 지금
        // HSV 가 이미 이 색이라(변환 일치) 건드리지 않는다 — 무조건 역산하면
        // s=0·v=0 을 지날 때마다 색상(H)이 0 으로 튄다.
        {
            let (h, s, v) = self.set_picker_hsv;
            if hsv_to_rgb(h, s, v) != c {
                self.set_picker_hsv = rgb_to_hsv(c);
            }
        }
        let s = socket::read_settings();
        let mut obj = match s.get("custom_theme").cloned() {
            Some(serde_json::Value::Object(m)) => m,
            _ => serde_json::Map::new(),
        };
        let hex = theme::hex_str(c);
        let n = theme::PALETTE_KEYS.len();
        if i < n {
            obj.insert(theme::PALETTE_KEYS[i].0.to_string(), serde_json::Value::String(hex));
        } else {
            // ansi 배열이 없거나 짧을 수 있다 — 지금 유효값으로 16칸을 다 채운
            // 뒤 한 칸만 바꾼다. 부분 배열을 그대로 두면 인덱스가 어긋난다.
            let list = palette_hex_list(&s);
            let mut arr: Vec<serde_json::Value> = (0..16)
                .map(|k| serde_json::Value::String(list[n + k].clone()))
                .collect();
            let j = (i - n).min(15);
            arr[j] = serde_json::Value::String(hex);
            obj.insert("ansi".to_string(), serde_json::Value::Array(arr));
        }
        socket::write_setting("custom_theme", serde_json::Value::Object(obj));
        theme::set_theme("custom");
        self.repaint_all();
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

/// 웹뷰가 가져갈 설정 값 스냅샷이 잠깐 놓이는 자리.
///
/// 값의 정본은 GUI 스레드의 `App` 인데(UI 배율처럼 파일에 없는 값이 있다), 그
/// 스레드로 가는 유일한 창구인 `SocketSettingsAction` 의 회신 봉투가
/// `{ok, message}` 로 고정돼 있어 값을 실을 칸이 없다. 그래서 액션이 여기에 굽고
/// HTTP 쪽이 곧바로 집어 간다 — 봉투를 넓히려면 다른 pane 이 작업 중인
/// `handler.rs` 를 더 만져야 해서, 그쪽 대신 이 우회로를 골랐다(2026-08-15).
fn web_values_cell() -> &'static std::sync::Mutex<Option<serde_json::Value>> {
    static CELL: std::sync::OnceLock<std::sync::Mutex<Option<serde_json::Value>>> =
        std::sync::OnceLock::new();
    CELL.get_or_init(Default::default)
}

fn publish_web_values(v: serde_json::Value) {
    if let Ok(mut c) = web_values_cell().lock() {
        *c = Some(v);
    }
}

/// 방금 구운 스냅샷을 집어 온다. **한 번만** 나간다 — 남겨 두면 GUI 왕복이 실패한
/// 다음 요청이 옛 값을 받아 가, 화면이 조용히 낡는다.
pub(crate) fn take_web_values() -> Option<serde_json::Value> {
    web_values_cell().lock().ok().and_then(|mut c| c.take())
}

/// 이번 액션이 남긴 문구 코드. 값 스냅샷과 같은 이유로 전역을 지난다 — 회신 봉투에
/// 실을 칸이 없어서다.
///
/// 코드는 **문구를 대신하지 않고 곁들인다.** 웹은 코드가 있으면 자기 사전에서
/// 문구를 만들고, 없으면 서버 문구를 그대로 쓴다(2026-08-15 형식 합의) — 그래서
/// 코드가 안 붙은 자리도 화면이 안 깨지고, 코드화를 한 칸씩 늘려 갈 수 있다.
fn web_codes_cell() -> &'static std::sync::Mutex<serde_json::Map<String, serde_json::Value>> {
    static CELL: std::sync::OnceLock<
        std::sync::Mutex<serde_json::Map<String, serde_json::Value>>,
    > = std::sync::OnceLock::new();
    CELL.get_or_init(Default::default)
}

fn put_web_code(key: &str, value: serde_json::Value) {
    if let Ok(mut c) = web_codes_cell().lock() {
        c.insert(key.to_string(), value);
    }
}

/// 거부 문구와 그 코드를 한 번에 만든다. 반환값은 **사람이 읽을 문구** — 코드는
/// 곁으로 빠져나가므로 호출부는 지금처럼 문자열만 다루면 된다.
fn reject(code: &'static str, msg: String) -> String {
    put_web_code("error_code", serde_json::Value::String(code.to_string()));
    msg
}

/// 인자가 붙는 거부. 자리 인자가 아니라 **이름 붙인 객체**로 넘긴다 — 영어는 어순이
/// 달라 자리로 맞추면 문장이 어긋난다.
fn reject_with(code: &'static str, args: serde_json::Value, msg: String) -> String {
    put_web_code("error_args", args);
    reject(code, msg)
}

/// 네이티브 토스트 문구 → 코드. 문구를 만드는 자리와 같은 파일에 둬야 둘이 어긋나도
/// 곧 눈에 띈다.
///
/// 표에 없는 문구는 코드 없이 나간다(웹이 서버 문구를 그대로 쓴다) — 토스트는
/// 여기저기서 뜨므로 전수를 붙잡으려 들면 표가 늘 낡는다.
fn toast_code(msg: &str) -> Option<&'static str> {
    Some(match msg {
        "재시작하면 적용돼요" => "restart_to_apply",
        "배율 100% · 폰트 기본값" => "scale_reset",
        "빈 브라우저 창에서 로그인하세요" => "login_in_browser",
        "터미널 편집기를 못 찾았어요 — 명령을 직접 적어 주세요" => "terminal_editor_not_found",
        "계정 폴더 경로를 만들 수 없습니다" => "account_dir_failed",
        "피드백을 저장했어요" => "feedback_saved",
        "저장됐어요" => "saved",
        _ => return None,
    })
}

/// GUI 회신에 이번 액션의 코드를 얹는다. 성공 회신이 지나는 자리.
///
/// `message` 가 비면 코드도 안 붙인다 — 토스트가 안 떴다는 뜻이라, 직전 액션이
/// 남긴 코드를 여기 붙이면 화면이 엉뚱한 말을 한다.
pub(crate) fn merge_web_codes(mut v: serde_json::Value) -> serde_json::Value {
    let codes = web_codes_cell().lock().map(|mut c| std::mem::take(&mut *c)).unwrap_or_default();
    let Some(obj) = v.as_object_mut() else { return v };
    let has_message = obj.get("message").is_some_and(|m| !m.is_null());
    if has_message {
        if let Some(code) = obj.get("message").and_then(|m| m.as_str()).and_then(toast_code) {
            obj.insert("message_code".to_string(), serde_json::Value::String(code.to_string()));
        }
    }
    // 성공 회신에 error_* 가 섞이면 웹이 거부로 읽는다 — 코드는 이번 호출 것만 쓰고
    // 나머지는 버린다.
    for (k, val) in codes {
        if k == "message_code" && !has_message {
            continue;
        }
        if k.starts_with("error") {
            continue;
        }
        obj.insert(k, val);
    }
    v
}

/// 거부 회신을 JSON 으로 만든다. 오류를 `Err` 로 올려보내면 문자열 하나만 남아
/// 코드를 실을 자리가 없다 — 형식은 HTTP 쪽이 만들던 것과 같다.
pub(crate) fn reject_json(msg: String) -> serde_json::Value {
    let codes = web_codes_cell().lock().map(|mut c| std::mem::take(&mut *c)).unwrap_or_default();
    let mut obj = serde_json::Map::new();
    obj.insert("ok".to_string(), serde_json::Value::Bool(false));
    obj.insert("error".to_string(), serde_json::Value::String(msg));
    for (k, v) in codes {
        if k.starts_with("error") {
            obj.insert(k, v);
        }
    }
    serde_json::Value::Object(obj)
}

/// 저장된 피드백이 쌓이는 폴더. 홈을 못 찾으면 temp 로 — 빈 PathBuf 에 join 하면
/// **상대경로**가 되어 피드백이 그때그때의 cwd 에 흩뿌려진다(Windows GUI 프로세스는
/// HOME 이 없어 항상 그랬다).
fn feedback_dir() -> std::path::PathBuf {
    kasa_socket::home_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(".config/kasaterm/feedback")
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
        (SettingsCat::Theme, "Theme", "users"),
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
    // 캐릭터 상세는 페이지가 통째로 바뀐 것이라 제목도 그 캐릭터 것이어야 한다 —
    // "Theme" 이 그대로 걸려 있으면 화면만 바뀌고 어디로 왔는지는 안 알려 주는 꼴이다.
    let detail = ctx
        .student_selected
        .as_deref()
        .filter(|_| ctx.cat == SettingsCat::Theme);
    g.draw_text(
        fx, ay + 22.0, detail.unwrap_or(active_label),
        gpu::DrawOpts { font_size: 24.0, color: theme::text(), bold: true, italic: false },
    );
    // 제목 아래 설명 한 줄. 없으면 24px 제목이 허공에 뜬다 — Orca 는 섹션마다
    // `text-sm leading-6 text-muted-foreground` 문단을 두고, 그게 「이 페이지에서
    // 무엇을 정하는가」를 미리 말해 준다.
    g.draw_text(
        fx, ay + 52.0,
        if detail.is_some() { "사진과 이름, 성격을 여기서 정합니다" } else { cat_blurb(ctx.cat) },
        gpu::DrawOpts { font_size: 13.0, color: theme::text_mute(), bold: false, italic: false },
    );
    g.rect(fx, ay + 78.0, fw, 1.0, theme::border());
    // ── Scrollable form ── the wheel shifts everything below the page header
    // up by ctx.scroll. The renderer has no scissor, so the coarse clip rule
    // is: a control whose TOP is above the header hairline isn't painted at
    // all (and its rect isn't pushed, so it isn't clickable either). Popping
    // whole controls at the boundary beats controls bleeding over the header
    // and title bar.
    let fy = ay + 110.0 - ctx.scroll;
    let clip = ay + 82.0;
    // 본문을 담는 카드는 내용보다 **먼저** 그려야 한다(나중이면 글자를 덮는다).
    // 높이는 직전 프레임 값이다 — `form_card` 주석 참고.
    form_card(g, fx, fy, fw, clip);
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
            // `"system"` 은 `"app"` 의 옛 저장값 — 앱 미지정과 뜻이 같아 같은 칸으로.
            let open_is = |m: &str| {
                ctx.file_open_mode == m || (m == "app" && ctx.file_open_mode == "system")
            };
            let x100 = (ctx.wheel_pixel_gain * 100.0).round() as u32;
            // 두 칸 행 하나 = 세그먼트 한 벌. 폭을 미리 재야 오른쪽에 붙일 수 있다.
            let seg_row = |g: &mut gpu::GpuRenderer,
                               rects: &mut Vec<(SettingsAction, Rect)>,
                               y: f32,
                               label: &str,
                               desc: &[&str],
                               cells: &[(&str, bool, SettingsAction)]| {
                let sw = seg_width(g, cells);
                let (cr, ny) = row2(g, fx, y, fw, clip, label, desc, (sw, SEG_H));
                if ny > clip {
                    segmented(g, rects, cr.0, cr.1, cells, ctx.cursor);
                }
                ny
            };
            y = seg_row(g, &mut rects, y, "Startup folder", &["새 창과 탭이 열리는 위치"], &[
                ("Last folder", cwd_is("last"), SettingsAction::CwdMode("last")),
                ("Home", cwd_is("home"), SettingsAction::CwdMode("home")),
                ("Custom", cwd_is("custom"), SettingsAction::CwdMode("custom")),
            ]);
            // 조건부로 딸려 나오는 필드는 폭을 다 쓰므로 그 행 아래로 내린다 —
            // 오른쪽 칸에 밀어 넣으면 라벨과 겹친다.
            if cwd_is("custom") {
                if y > clip {
                    let r = (fx, y, fw.min(420.0), 32.0);
                    let focused = ctx.input == Some(SettingsInput::CwdPath);
                    text_field(g, r, &ctx.cwd_mode, ctx.settings_caret, focused, ctx.caret_on, ctx.cursor, if focused { &ctx.preedit } else { "" });
                    rects.push((SettingsAction::FocusCwdPath, r));
                }
                y += 32.0 + 8.0;
            }
            {
                let (cr, ny) = row2(g, fx, y, fw, clip, "File tree by default",
                    &["시작할 때 파일 트리 사이드바 열기"], TOGGLE);
                if ny > clip {
                    toggle(g, cr, ctx.file_tree_default, ctx.cursor);
                    rects.push((SettingsAction::ToggleFileTree, cr));
                }
                y = ny;
            }
            {
                let (cr, ny) = row2(g, fx, y, fw, clip, "Pane status bar by default",
                    &["각 pane 아래 경로 · 브랜치 · diff 바 표시"], TOGGLE);
                if ny > clip {
                    toggle(g, cr, ctx.footer_default, ctx.cursor);
                    rects.push((SettingsAction::ToggleFooter, cr));
                }
                y = ny;
            }
            y = seg_row(g, &mut rects, y, "File open",
                &["파일 트리에서 파일을 열 때 무엇으로 열지"], &[
                ("Built-in", open_is("builtin"), SettingsAction::FileOpenMode("builtin")),
                ("App", open_is("app"), SettingsAction::FileOpenMode("app")),
                ("Terminal", open_is("terminal"), SettingsAction::FileOpenMode("terminal")),
            ]);
            if open_is("app") {
                // 설치된 것만 뜬다(`open_with_apps`). 마지막 "기본 앱" 은 OS 연결
                // 프로그램 — 목록에 없는 앱을 쓰는 사람의 탈출구다. 칸이 여럿이라
                // 오른쪽 칸에 안 들어가므로 아래 줄을 통째로 쓴다.
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
                if y > clip {
                    segmented(g, &mut rects, fx, y, &cells, ctx.cursor);
                }
                y += SEG_H + 8.0;
            }
            if open_is("terminal") {
                if y > clip {
                    help_text(g, fx, y, "새 pane 에서 CLI 편집기 — {} 는 파일 경로 자리");
                    let r = (fx, y + 16.0, fw.min(420.0), 32.0);
                    let focused = ctx.input == Some(SettingsInput::FileOpenCmd);
                    text_field(g, r, &ctx.file_open_cmd, ctx.settings_caret, focused, ctx.caret_on, ctx.cursor, if focused { &ctx.preedit } else { "" });
                    rects.push((SettingsAction::FocusFileOpenCmd, r));
                }
                y += 16.0 + 32.0 + 8.0;
            }
            y = seg_row(g, &mut rects, y, "Editor autosave",
                &[&format!("타자가 멎으면 조용히 저장 ({PRIMARY_MOD}+S 는 그대로)")], &[
                ("Off", ctx.autosave_ms == 0, SettingsAction::AutosaveDelay(0)),
                ("1s", ctx.autosave_ms == 1000, SettingsAction::AutosaveDelay(1000)),
                ("3s", ctx.autosave_ms == 3000, SettingsAction::AutosaveDelay(3000)),
                ("10s", ctx.autosave_ms == 10000, SettingsAction::AutosaveDelay(10000)),
            ]);
            y = seg_row(g, &mut rects, y, "Tab position",
                &["윈도우 탭을 타이틀바 또는 사이드바에 표시"], &[
                ("Top", ctx.tabs_on_top, SettingsAction::TabPosition("top")),
                ("Side", !ctx.tabs_on_top, SettingsAction::TabPosition("side")),
            ]);
            y = seg_row(g, &mut rects, y, "Cursor shape",
                &["셀을 채우는 블록, Ghostty 식 세로선, 또는 밑줄"], &[
                ("Block", ctx.cursor_shape == "block", SettingsAction::CursorShape("block")),
                ("Bar", ctx.cursor_shape == "bar", SettingsAction::CursorShape("bar")),
                ("Underline", ctx.cursor_shape == "underline", SettingsAction::CursorShape("underline")),
            ]);
            // 굵기는 bar·underline 에만 쓰인다 — block 은 셀을 통째로 채우므로 고를 게
            // 없다. 줄 자체를 감추면 「왜 사라졌지」가 되므로, block 일 때도 두되 무엇에
            // 쓰이는지 곁글로 밝힌다.
            y = seg_row(g, &mut rects, y, "Cursor thickness",
                &[if ctx.cursor_shape == "block" {
                    "Bar·Underline 을 고르면 적용돼요"
                } else {
                    "세로선·밑줄의 굵기"
                }], &[
                ("1px", (ctx.cursor_thickness - 1.0).abs() < 0.01, SettingsAction::CursorThickness(1)),
                ("2px", (ctx.cursor_thickness - 2.0).abs() < 0.01, SettingsAction::CursorThickness(2)),
                ("3px", (ctx.cursor_thickness - 3.0).abs() < 0.01, SettingsAction::CursorThickness(3)),
                ("4px", (ctx.cursor_thickness - 4.0).abs() < 0.01, SettingsAction::CursorThickness(4)),
            ]);
            y = seg_row(g, &mut rects, y, "Mouse pointer",
                &["터미널 위 마우스 포인터 모양 (입력칸 위 I-beam 은 그대로예요)"], &[
                ("Arrow", ctx.mouse_cursor != "ibeam", SettingsAction::MouseCursor("arrow")),
                ("I-beam", ctx.mouse_cursor == "ibeam", SettingsAction::MouseCursor("ibeam")),
            ]);
            // 트랙패드와 고해상도 마우스휠은 같은 델타로 들어와 자동으로 못 가른다 —
            // 그래서 한쪽에 맞추면 다른 쪽이 어긋난다. 고르는 몫을 사람에게 넘긴다.
            y = seg_row(g, &mut rects, y, "Scroll sensitivity",
                &["트랙패드 기준이 기본이에요. 휠이 굼뜨면 올리세요"], &[
                ("트랙패드", x100 == 30, SettingsAction::WheelPixelGain(30)),
                ("보통", x100 == 60, SettingsAction::WheelPixelGain(60)),
                ("마우스", x100 == 100, SettingsAction::WheelPixelGain(100)),
                ("빠르게", x100 == 150, SettingsAction::WheelPixelGain(150)),
            ]);
            content_bottom = y;
        }
        SettingsCat::Appearance => {
            let mut y = fy;
            // 테마 — 프리셋 카드 그리드. 카드 하나 = 그 팔레트의 미니 프리뷰
            // (bg 칠 + 프롬프트 샘플 + ANSI 도트 + 라벨)라서 고르기 전에 색이
            // 보인다. UI 토큰과 터미널 ANSI 16색이 함께 바뀐다.
            y = row_wide(g, fx, y, clip, "Theme",
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
                card(g, &mut rects, "custom", "Custom", None);
            }
            let rows = idx.div_ceil(per_row);
            y += rows as f32 * (card_h + gap) + 12.0;
            // 팔레트 — 프리셋을 복제해 색을 한 칸씩 고치는 자리(2026-08-13 지시:
            // 라이트가 너무 밝다 → 팔레트 커스텀). custom 테마일 때만 편집 칸이
            // 열린다 — 프리셋 위에 직접 덧칠하게 하면 원래 색으로 돌아갈 길이
            // 없다: 프리셋은 불변, 편집은 복제본에.
            if ctx.theme == "custom" {
                y = row_wide(g, fx, y, clip, "Palette",
                    &["칸을 고르면 선택기가 열려요 — 마우스로 고르거나 #rrggbb 를 쳐도 돼요"]);
                let n = theme::PALETTE_KEYS.len();
                let (cw, ch, pgap) = (294.0_f32, 30.0_f32, 8.0_f32);
                // 색 선택기 — 포커스된 칸 바로 아래 뜬다(UI 색이면 격자 밑,
                // ANSI 면 공유 필드 밑). SV 사각형은 셀 격자로 근사한다:
                // 렌더러에 그라데이션이 없고, 4px 셀이면 눈에는 연속으로 읽힌다.
                // hex 필드는 남긴다 — 정밀값 입력·복붙은 키보드가 빠르다
                // (2026-08-13 지시: "마우스로 선택 … 키보드로 색을 어케쳐").
                let picker = |g: &mut gpu::GpuRenderer,
                              rects: &mut Vec<(SettingsAction, Rect)>,
                              mut y: f32|
                 -> f32 {
                    let (ph, ps, pv) = ctx.picker_hsv;
                    let (pw, sh) = (216.0_f32, 144.0_f32);
                    if y > clip {
                        let cell = 4.0_f32;
                        let (cols, rows) = ((pw / cell) as i32, (sh / cell) as i32);
                        for cyi in 0..rows {
                            for cxi in 0..cols {
                                let s = (cxi as f32 + 0.5) / cols as f32;
                                let v = 1.0 - (cyi as f32 + 0.5) / rows as f32;
                                let c = hsv_to_rgb(ph, s, v);
                                g.rect(
                                    fx + cxi as f32 * cell, y + cyi as f32 * cell,
                                    cell, cell, [c[0], c[1], c[2], 255],
                                );
                            }
                        }
                        // 핸들 — 밝은 자리선 검정 링, 어두운 자리선 흰 링.
                        let hx = fx + ps * pw;
                        let hy = y + (1.0 - pv) * sh;
                        let ring = if pv > 0.5 { [0, 0, 0, 255] } else { [255, 255, 255, 255] };
                        circle_rect(g, hx - 6.0, hy - 6.0, 12.0, ring);
                        let cc = hsv_to_rgb(ph, ps, pv);
                        circle_rect(g, hx - 4.0, hy - 4.0, 8.0, [cc[0], cc[1], cc[2], 255]);
                        g.hover_pointer |= inside((fx, y, pw, sh), ctx.cursor);
                        rects.push((SettingsAction::PickerSV, (fx, y, pw, sh)));
                    }
                    y += sh + 8.0;
                    let hh = 16.0_f32;
                    if y > clip {
                        let step = 3.0_f32;
                        let nsteps = (pw / step) as i32;
                        for k in 0..nsteps {
                            let c = hsv_to_rgb(k as f32 / nsteps as f32 * 360.0, 1.0, 1.0);
                            g.rect(fx + k as f32 * step, y, step, hh, [c[0], c[1], c[2], 255]);
                        }
                        let mx = (fx + (ph / 360.0) * pw).min(fx + pw - 1.5);
                        g.rect(mx - 1.5, y - 2.0, 3.0, hh + 4.0, theme::text());
                        g.hover_pointer |= inside((fx, y, pw, hh), ctx.cursor);
                        rects.push((SettingsAction::PickerHue, (fx, y, pw, hh)));
                    }
                    y + hh + 12.0
                };
                for (i, (key, _)) in theme::PALETTE_KEYS.iter().enumerate() {
                    let x = fx + (i % 2) as f32 * (cw + pgap);
                    let cy = y + (i / 2) as f32 * (ch + pgap);
                    if cy <= clip {
                        continue;
                    }
                    let r = (x, cy, cw, ch);
                    let focused = ctx.input == Some(SettingsInput::PaletteHex(i));
                    let hover = inside(r, ctx.cursor);
                    g.hover_pointer |= hover;
                    if hover && !focused {
                        round_rect(g, x, cy, cw, ch, theme::radius_sm(), theme::surface_hover());
                    }
                    let hex = ctx.palette_hex.get(i).map(String::as_str).unwrap_or("#000000");
                    let c = theme::parse_hex(hex).unwrap_or([0, 0, 0]);
                    // 견본이 배경과 같은 색일 수 있어 테두리 판을 깔고 색을 얹는다.
                    round_rect(g, x + 4.0, cy + 5.0, 20.0, 20.0, 5.0, theme::border());
                    round_rect(g, x + 5.0, cy + 6.0, 18.0, 18.0, 4.0, [c[0], c[1], c[2], 255]);
                    g.draw_text(
                        x + 34.0, cy + 8.0, key,
                        gpu::DrawOpts { font_size: 13.0, color: theme::text(), bold: false, italic: false },
                    );
                    let fr = (x + cw - 96.0, cy + 3.0, 92.0, 24.0);
                    if focused {
                        text_field(g, fr, &ctx.palette_edit, ctx.settings_caret, true, ctx.caret_on, ctx.cursor, &ctx.preedit);
                    } else {
                        g.draw_text(
                            fr.0 + 6.0, cy + 8.0, hex,
                            gpu::DrawOpts { font_size: 12.5, color: theme::text_dim(), bold: false, italic: false },
                        );
                    }
                    rects.push((SettingsAction::FocusPaletteHex(i), r));
                }
                y += n.div_ceil(2) as f32 * (ch + pgap) + 4.0;
                if matches!(ctx.input, Some(SettingsInput::PaletteHex(i)) if i < n) {
                    y = picker(g, &mut rects, y);
                }
                y = row_wide(g, fx, y, clip, "Terminal ANSI",
                    &["터미널 본문 16색 — 윗줄 0..7, 아랫줄 8..15 (bright)"]);
                let sz = 30.0_f32;
                for j in 0..16usize {
                    let x = fx + (j % 8) as f32 * (sz + pgap);
                    let cy = y + (j / 8) as f32 * (sz + pgap);
                    if cy <= clip {
                        continue;
                    }
                    let r = (x, cy, sz, sz);
                    let pi = n + j;
                    let focused = ctx.input == Some(SettingsInput::PaletteHex(pi));
                    let hover = inside(r, ctx.cursor);
                    g.hover_pointer |= hover;
                    if focused || hover {
                        let ring = if focused { theme::accent() } else { theme::surface_hover() };
                        round_rect(g, x - 2.0, cy - 2.0, sz + 4.0, sz + 4.0, theme::radius_sm() + 2.0, ring);
                    }
                    let hex = ctx.palette_hex.get(pi).map(String::as_str).unwrap_or("#000000");
                    let c = theme::parse_hex(hex).unwrap_or([0, 0, 0]);
                    round_rect(g, x, cy, sz, sz, theme::radius_sm(), theme::border());
                    round_rect(g, x + 1.0, cy + 1.0, sz - 2.0, sz - 2.0, (theme::radius_sm() - 1.0).max(0.0), [c[0], c[1], c[2], 255]);
                    rects.push((SettingsAction::FocusPaletteHex(pi), r));
                }
                y += 2.0 * (sz + pgap) + 4.0;
                // ANSI 그리드 안엔 글자 자리가 없다 — 고른 칸의 편집 필드를 아래
                // 한 줄로 공유한다(어느 칸인지는 인덱스 라벨과 accent 링이 말한다).
                if let Some(SettingsInput::PaletteHex(pi)) = ctx.input {
                    if pi >= n {
                        if y > clip {
                            g.draw_text(
                                fx, y + 6.0, &format!("ansi {}", pi - n),
                                gpu::DrawOpts { font_size: 13.0, color: theme::text_dim(), bold: false, italic: false },
                            );
                            let fr = (fx + 64.0, y, 110.0, 26.0);
                            text_field(g, fr, &ctx.palette_edit, ctx.settings_caret, true, ctx.caret_on, ctx.cursor, &ctx.preedit);
                            rects.push((SettingsAction::FocusPaletteHex(pi), fr));
                        }
                        y += 26.0 + pgap;
                        y = picker(g, &mut rects, y);
                    }
                }
                if y > clip {
                    let label = "베이스 값으로 되돌리기";
                    let bw = g.measure_chrome_text(label, 13.0, false) + 28.0;
                    let r = (fx, y, bw, 30.0);
                    let hov = inside(r, ctx.cursor);
                    g.hover_pointer |= hov;
                    round_rect(g, r.0, r.1, r.2, r.3, theme::radius_md(),
                        if hov { theme::surface_hover() } else { theme::surface_active() });
                    g.draw_text(
                        r.0 + 14.0, r.1 + 7.0, label,
                        gpu::DrawOpts { font_size: 13.0, color: theme::text(), bold: false, italic: false },
                    );
                    rects.push((SettingsAction::ResetCustomTheme, r));
                }
                y += 30.0 + 12.0;
            } else {
                y = row_wide(g, fx, y, clip, "Palette",
                    &["지금 테마를 복제해 색을 한 칸씩 고칠 수 있어요 (custom_theme 으로 저장)"]);
                if y > clip {
                    let label = if ctx.has_custom_theme {
                        "커스텀 팔레트 이어서 편집"
                    } else {
                        "지금 테마를 복제해 시작"
                    };
                    let bw = g.measure_chrome_text(label, 13.0, false) + 28.0;
                    let r = (fx, y, bw, 30.0);
                    let hov = inside(r, ctx.cursor);
                    g.hover_pointer |= hov;
                    round_rect(g, r.0, r.1, r.2, r.3, theme::radius_md(),
                        if hov { theme::surface_hover() } else { theme::surface_active() });
                    g.draw_text(
                        r.0 + 14.0, r.1 + 7.0, label,
                        gpu::DrawOpts { font_size: 13.0, color: theme::text(), bold: false, italic: false },
                    );
                    rects.push((SettingsAction::StartCustomTheme, r));
                }
                y += 30.0 + 12.0;
            }
            // 형태 — 팔레트와 독립된 축. 각 카드가 *자기* 실루엣으로 그려진다
            // (모서리 반경 · 테두리 두께 · 그림자 · 점과 캡슐의 둥글기) — 테마
            // 카드가 팔레트를 미리 보여주는 것과 같은 규칙이라, 고르기 전에
            // 형태가 눈에 보인다.
            y = row_wide(g, fx, y, clip, "Shape", &["모서리 · 점 · 토글의 실루엣 (팔레트와 별개 축)"]);
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
            y += 58.0 + 12.0;
            y = row_wide(g, fx, y, clip, "Accent color", &["선택 영역 · 커서 · 링크 색"]);
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
            y += 30.0 + 12.0;
            // 최소 대비 — 앱이 스스로 이름 붙인 색만 대상이다. 각 버튼은 자기
            // 임계를 적용한 샘플을 그려서, 고르기 전에 그 값이 실제로 얼마나
            // 끌어올리는지 눈으로 비교된다.
            y = row_wide(
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
            y += 44.0 + 12.0;
            // 폰트 크기 스테퍼 — 값은 즉시 적용(그리드 리플로우)되고
            // settings.json 에 저장돼 재시작에도 유지된다.
            y = row_wide(g, fx, y, clip, "Font size", &[&format!("터미널 셀 폰트 크기 — {PRIMARY_MOD}+/− 배율과는 별개인 기준값이에요")]);
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
            y += 30.0 + 12.0;
            // UI 배율 — 여태 Cmd+/− 키에만 있었다. 키로만 있으면 지금 몇 %인지
            // 화면 어디에도 안 적혀서, 폰트 크기와 배율을 번갈아 만지다 UI 가
            // 어긋나도 어느 쪽이 범인인지 알 수가 없다(거노). 숫자를 보여 주고,
            // 둘을 한 번에 되돌릴 자리를 옆에 둔다.
            y = row_wide(g, fx, y, clip, "UI 배율",
                &[&format!("크롬·사이드바·pane 이 함께 커져요 ({PRIMARY_MOD}+/− 와 같은 축)")]);
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
            // Preset 칸들 + 자유입력 필드로 포커스를 주는 "Custom" 칸.
            let presets: [(&str, &str); 3] =
                [("", "System default"), ("/bin/zsh", "zsh"), ("/bin/bash", "bash")];
            let shell_is_preset = presets.iter().any(|(v, _)| *v == ctx.shell);
            let cells = [
                ("System default", ctx.shell.is_empty(), SettingsAction::ShellPreset(String::new())),
                ("zsh", ctx.shell == "/bin/zsh", SettingsAction::ShellPreset("/bin/zsh".to_string())),
                ("bash", ctx.shell == "/bin/bash", SettingsAction::ShellPreset("/bin/bash".to_string())),
                ("Custom", !shell_is_preset, SettingsAction::FocusShell),
            ];
            let sw = seg_width(g, &cells);
            let (cr, ny) = row2(g, fx, y, fw, clip, "Default shell",
                &["새 pane 의 셸 (비우면 시스템 $SHELL)"], (sw, SEG_H));
            if ny > clip {
                segmented(g, &mut rects, cr.0, cr.1, &cells, ctx.cursor);
            }
            y = ny;
            if !shell_is_preset {
                if y > clip {
                    let r = (fx, y, fw.min(420.0), 34.0);
                    let focused = ctx.input == Some(SettingsInput::Shell);
                    text_field(g, r, &ctx.shell, ctx.settings_caret, focused, ctx.caret_on, ctx.cursor, if focused { &ctx.preedit } else { "" });
                    rects.push((SettingsAction::FocusShell, r));
                }
                y += 34.0 + 8.0;
            }
            content_bottom = y;
        }
        SettingsCat::Claude => {
            // Page 헤더가 이미 "Claude" 를 크게 쓰므로 별도 브랜드 워드마크는
            // 중복이라 뺐다 — 좌측 nav 에도 claude 아이콘이 있다.
            let mut y = fy;
            // Shim injection — global. off = install_pane_shims never makes the shim
            // dir, so claude runs vanilla (no persona/proxy/hooks). Read once at boot,
            // so a change needs a restart.
            {
                let (cr, ny) = row2(g, fx, y, fw, clip, "Shim injection",
                    &["끄면 순정 Claude — 페르소나 · 프록시 · 훅 없음 (재시작 필요)"], TOGGLE);
                if ny > clip {
                    toggle(g, cr, ctx.shim_inject, ctx.cursor);
                    rects.push((SettingsAction::ToggleShimInject, cr));
                }
                y = ny;
            }
            {
                let (cr, ny) = row2(g, fx, y, fw, clip, "Persona injection",
                    &["이 pane 의 캐릭터를 Claude 시스템 프롬프트에 붙여요"], TOGGLE);
                if ny > clip {
                    toggle(g, cr, ctx.claude_persona, ctx.cursor);
                    rects.push((SettingsAction::ToggleClaudePersona, cr));
                }
                y = ny;
            }
            y = row_wide(g, fx, y, clip, "Account",
                &["다음에 뜨는 claude 부터 이 계정으로 — 돌고 있는 세션은 그대로예요"]);
            // 첫 행은 언제나 "기본"(활성 계정 `""` = env 미설정 = 지금 로그인). 이 행은
            // 우리가 만든 슬롯이 아니라 지울 것도, 이름 붙일 것도 없다.
            let acct_rows = std::iter::once((String::new(), "기본".to_string(), None))
                .chain(
                    ctx.claude_accounts
                        .iter()
                        .enumerate()
                        .map(|(i, a)| (a.id.clone(), a.label.clone(), Some(i))),
                );
            for (id, label, idx) in acct_rows {
                if y > clip {
                    let active = ctx.claude_account == id;
                    // 2줄은 그 슬롯의 진짜 신원 — 라벨은 거노가 붙인 별명이라 로그인이
                    // 실제로 됐는지는 말해 주지 않는다. 빈칸으로 두지 않는다: 답이 아직
                    // 없는 두 경우(첫 조회 중 / 안 쓰던 슬롯의 토큰 갱신 중)에 비우면
                    // 계정이 사라진 것처럼 보였다(거노: "계정 재시작할때마다 또
                    // 없어지냐"). 없다고 말하지 말고 아직 모른다고 말한다.
                    let probe = auth_probe(&id);
                    let (mut sub, sub_col) = match &probe {
                        Some(p) if !p.logged_in => ("로그인 필요".to_string(), theme::danger()),
                        Some(p) if !p.email.is_empty() => (p.email.clone(), theme::text_mute()),
                        _ => ("확인 중…".to_string(), theme::with_alpha(theme::text_mute(), 0x99)),
                    };
                    // 팀 조직을 같은 줄에 이어 붙인다. 같은 이메일이 두 슬롯에 걸릴 때
                    // 이게 유일한 구분점이다(거노: "팀플랜인지 구분하게 돼?").
                    if let Some(org) = probe.as_ref().and_then(|p| team_org(&p.email, &p.org)) {
                        sub = format!("{sub} · {org}");
                    }
                    let editing = idx
                        .is_some_and(|i| ctx.input == Some(SettingsInput::ClaudeAccountLabel(i)));
                    let slot = idx.map(|i| AcctSlot {
                        editing,
                        label: &label,
                        caret: ctx.settings_caret,
                        caret_on: ctx.caret_on,
                        preedit: if editing { &ctx.preedit } else { "" },
                        focus: SettingsAction::FocusClaudeAccountLabel(i),
                        reauth: SettingsAction::ReauthAccount(AccountProvider::Claude, id.clone()),
                        remove: SettingsAction::RemoveClaudeAccount(id.clone()),
                    });
                    let name = if idx.is_none() {
                        "기본".to_string()
                    } else {
                        account_display(&id, &label, &format!("계정 {}", idx.unwrap_or(0) + 2))
                    };
                    // 이름 없는 슬롯은 `account_display` 가 이메일을 이름으로 쓴다 —
                    // 그러면 2줄이 같은 값을 되풀이한다(캡처에서 한 카드가 이메일을
                    // 두 번 적고 있었다). 이름이 이미 그 값이면 2줄에서 걷어내고,
                    // 조직만 남으면 그것을 남긴다.
                    let sub = match sub.strip_prefix(name.as_str()) {
                        Some(rest) => rest.trim_start_matches(" · ").to_string(),
                        None => sub,
                    };
                    let view = AcctRowView { name, sub, sub_col, active, slot };
                    account_card(
                        g, &mut rects, (fx, y, fw, ACCT_H), ctx.cursor, &view,
                        SettingsAction::ClaudeAccount(id.clone()),
                    );
                }
                y += ACCT_H + 6.0;
            }
            if y > clip {
                add_account_row(
                    g, &mut rects, fx, y, ctx.cursor,
                    SettingsAction::AddClaudeAccount, "Claude",
                );
            }
            y += 34.0 + 12.0;
            // 자동 전환. 계정이 하나뿐이면 갈 곳이 없어 아무 일도 안 일어나므로
            // 그 상태를 설명 줄로 미리 알려 준다 — 켜 놓고 "안 되네" 하는 게 이
            // 기능에서 제일 흔한 오해다.
            let lone = ctx.claude_accounts.is_empty();
            {
                let (cr, ny) = row2(g, fx, y, fw, clip, "Auto switch",
                    &[if lone { "계정이 하나뿐이라 지금은 넘어갈 곳이 없어요" }
                      else { "한도가 차면 다음에 뜨는 claude 부터 다음 계정으로 — 떠난 계정은 풀릴 때까지 쉬어요" }],
                    TOGGLE);
                if ny > clip {
                    toggle(g, cr, ctx.account_autoswitch, ctx.cursor);
                    rects.push((SettingsAction::ToggleAccountAutoswitch, cr));
                }
                y = ny;
            }
            if ctx.account_autoswitch {
                let pct = ctx.account_autoswitch_pct.round() as u32;
                let cells = [
                    ("80%", pct == 80, SettingsAction::AccountAutoswitchPct(80)),
                    ("85%", pct == 85, SettingsAction::AccountAutoswitchPct(85)),
                    ("90%", pct == 90, SettingsAction::AccountAutoswitchPct(90)),
                    ("95%", pct == 95, SettingsAction::AccountAutoswitchPct(95)),
                ];
                let sw = seg_width(g, &cells);
                let (cr, ny) = row2(g, fx, y, fw, clip, "Switch at",
                    &["이 사용률을 넘으면 다음 계정으로 넘어가요"], (sw, SEG_H));
                if ny > clip {
                    segmented(g, &mut rects, cr.0, cr.1, &cells, ctx.cursor);
                }
                y = ny;
            }
            // Codex(ChatGPT) 계정 — claude 슬롯 바로 아래 둔다. pane 에서 codex 를
            // 띄우는 것도 같은 손이라, 두 로그인이 설정의 다른 층에 흩어져 있으면
            // 「지금 어느 계정으로 돌고 있나」를 두 군데서 확인해야 한다.
            y = row_wide(g, fx, y, clip, "Codex account",
                &["다음에 뜨는 codex 부터 이 계정으로 — 돌고 있는 세션은 그대로예요"]);
            let codex_rows = std::iter::once((String::new(), "기본".to_string(), None))
                .chain(
                    ctx.codex_accounts
                        .iter()
                        .enumerate()
                        .map(|(i, a)| (a.id.clone(), a.label.clone(), Some(i))),
                );
            for (id, label, idx) in codex_rows {
                if y > clip {
                    let active = ctx.codex_account == id;
                    // claude 판과 달리 "확인 중…" 이 없다 — 신원이 파일 하나에 들어
                    // 있어 즉시 읽힌다(HTTP 왕복이 아니다). 그래서 값이 없다는 건
                    // 정말로 아직 로그인 안 한 슬롯이라는 뜻이다.
                    let ident = codex_identity(&id);
                    let (sub, sub_col) = match &ident {
                        Some(e) => (e.clone(), theme::text_mute()),
                        None => ("로그인 필요".to_string(), theme::danger()),
                    };
                    let editing = idx
                        .is_some_and(|i| ctx.input == Some(SettingsInput::CodexAccountLabel(i)));
                    let slot = idx.map(|i| AcctSlot {
                        editing,
                        label: &label,
                        caret: ctx.settings_caret,
                        caret_on: ctx.caret_on,
                        preedit: if editing { &ctx.preedit } else { "" },
                        focus: SettingsAction::FocusCodexAccountLabel(i),
                        reauth: SettingsAction::ReauthAccount(AccountProvider::Codex, id.clone()),
                        remove: SettingsAction::RemoveCodexAccount(id.clone()),
                    });
                    let view = AcctRowView {
                        name: match (idx, label.is_empty()) {
                            (None, _) => "기본".to_string(),
                            // 라벨이 없으면 이메일이 이름이 된다. 그러면 2줄이 같은
                            // 값을 되풀이하므로 claude 쪽 `account_display` 규칙을
                            // 그대로 쓰되, 폴백은 슬롯 번호다.
                            (Some(i), true) => ident
                                .clone()
                                .unwrap_or_else(|| format!("계정 {}", i + 2)),
                            (Some(_), false) => label.clone(),
                        },
                        sub: if idx.is_some() && label.is_empty() && ident.is_some() {
                            String::new()
                        } else {
                            sub
                        },
                        sub_col,
                        active,
                        slot,
                    };
                    account_card(
                        g, &mut rects, (fx, y, fw, ACCT_H), ctx.cursor, &view,
                        SettingsAction::CodexAccount(id.clone()),
                    );
                }
                y += ACCT_H + 6.0;
            }
            if y > clip {
                add_account_row(
                    g, &mut rects, fx, y, ctx.cursor,
                    SettingsAction::AddCodexAccount, "Codex",
                );
            }
            y += 34.0 + 12.0;
            {
                let cells = [
                    ("Default", ctx.claude_model.is_empty(), SettingsAction::ClaudeModel(String::new())),
                    ("opus", ctx.claude_model == "opus", SettingsAction::ClaudeModel("opus".to_string())),
                    ("sonnet", ctx.claude_model == "sonnet", SettingsAction::ClaudeModel("sonnet".to_string())),
                    ("haiku", ctx.claude_model == "haiku", SettingsAction::ClaudeModel("haiku".to_string())),
                ];
                let sw = seg_width(g, &cells);
                let (cr, ny) = row2(g, fx, y, fw, clip, "Model",
                    &["Claude 모델 덮어쓰기 (Default = 원래대로 유지)"], (sw, SEG_H));
                if ny > clip {
                    segmented(g, &mut rects, cr.0, cr.1, &cells, ctx.cursor);
                }
                y = ny;
            }
            {
                let cells = [
                    ("Default", ctx.claude_effort.is_empty(), SettingsAction::ClaudeEffort(String::new())),
                    ("low", ctx.claude_effort == "low", SettingsAction::ClaudeEffort("low".to_string())),
                    ("medium", ctx.claude_effort == "medium", SettingsAction::ClaudeEffort("medium".to_string())),
                    ("high", ctx.claude_effort == "high", SettingsAction::ClaudeEffort("high".to_string())),
                    ("xhigh", ctx.claude_effort == "xhigh", SettingsAction::ClaudeEffort("xhigh".to_string())),
                ];
                let sw = seg_width(g, &cells);
                let (cr, ny) = row2(g, fx, y, fw, clip, "Effort",
                    &["추론 강도 — Default 는 그대로 둬요"], (sw, SEG_H));
                if ny > clip {
                    segmented(g, &mut rects, cr.0, cr.1, &cells, ctx.cursor);
                }
                y = ny;
            }
            y = row_wide(g, fx, y, clip, "Extra args", &["claude 실행에 항상 붙는 플래그 (예: --verbose)"]);
            if y > clip {
                let r = (fx, y, fw.min(420.0), 34.0);
                let focused = ctx.input == Some(SettingsInput::ClaudeExtra);
                text_field(g, r, &ctx.claude_extra, ctx.settings_caret, focused, ctx.caret_on, ctx.cursor, if focused { &ctx.preedit } else { "" });
                rects.push((SettingsAction::FocusClaudeExtra, r));
            }
            content_bottom = y + 34.0;
        }
        // 캐릭터를 고른 동안은 목록 대신 그 캐릭터만 — guard 로 가르는 건 아래
        // 목록 arm 을 통째로 한 단 더 들여쓰지 않으려는 것뿐이다.
        SettingsCat::Theme if ctx.student_selected.is_some() => {
            let sel = ctx.student_selected.clone().unwrap_or_default();
            content_bottom = student_detail(g, &mut rects, ctx, fx, fy, fw, clip, &sel);
        }
        SettingsCat::Theme => {
            let mut y = fy;
            // ── 테마 고르기 ──────────────────────────────────────────────
            y = row_wide(g, fx, y, clip, "Theme",
                &["폴더 하나가 테마 하나 — 이름·색·그림이 한 벌로 바뀝니다"]);
            // 카드 격자. 얼굴이 보여야 고를 수 있다 — 이름만 늘어놓은 목록은
            // "이터널리턴" 이 무슨 그림인지 켜 보기 전엔 알 수 없다.
            let (cols, tcw) = grid_fit(fw, THEME_CARD_MIN_W, THEME_GAP);
            // 얼굴 셋이 카드 폭을 나눠 갖는다 — 카드가 넓어지면 얼굴도 같이 큰다.
            let tface_w = ((tcw - 28.0 - 8.0) / 3.0).floor();
            let row_top = y;
            for (i, t) in ctx.themes.iter().enumerate() {
                let cx0 = fx + (i % cols) as f32 * (tcw + THEME_GAP);
                let cy0 = row_top + (i / cols) as f32 * (THEME_CARD_H + THEME_GAP);
                y = cy0 + THEME_CARD_H;
                // 카드 **위쪽**이 기준이다(폼 전체의 클립 규약). 바닥으로 재면
                // 반쯤 걸친 카드가 통째로 그려져 헤더 위로 얼굴이 삐져나온다 —
                // 렌더러에 시저가 없어 잘라 낼 방법이 없다.
                if cy0 <= clip {
                    continue;
                }
                let card = (cx0, cy0, tcw, THEME_CARD_H);
                let selected = ctx.theme_active == t.id;
                let hover = inside(card, ctx.cursor);
                g.hover_pointer |= hover;
                // 안 고른 카드도 폼 카드보다는 밝아야 한다 — 어둡게 깔면 격자가
                // 판에 뚫린 구멍처럼 보이고, 정작 고른 카드가 안 도드라진다.
                round_rect(
                    g, card.0, card.1, card.2, card.3, theme::radius_md(),
                    if selected { theme::surface_active() }
                    else if hover { theme::surface_hover() }
                    else { theme::panel_bg() },
                );
                if selected {
                    // 왼쪽 띠 하나로 "이걸 쓰는 중"을 말한다 — 체크 아이콘을 얹으면
                    // 얼굴 미리보기를 가린다.
                    g.rect(card.0, card.1 + 8.0, 3.0, card.3 - 16.0, theme::accent());
                }
                // 미리보기 얼굴 — 셋이 겹치지 않게 나란히.
                let mut face_x = cx0 + 14.0;
                for (slug, src) in &t.faces {
                    render::draw_theme_face(
                        g, &t.id, slug, src.as_deref(),
                        face_x, cy0 + 10.0, tface_w, THEME_FACE_H,
                    );
                    face_x += tface_w + 4.0;
                }
                let name_y = cy0 + THEME_CARD_H - 46.0;
                let editing = ctx.theme_label_edit.as_ref().filter(|(id, _)| *id == t.id);
                match editing {
                    Some((_, buf)) => {
                        let r = (cx0 + 12.0, name_y - 4.0, tcw - 24.0, 26.0);
                        let focused = ctx.input == Some(SettingsInput::ThemeLabel);
                        text_field(
                            g, r, buf, ctx.settings_caret, focused, ctx.caret_on, ctx.cursor,
                            if focused { &ctx.preedit } else { "" },
                        );
                        rects.push((SettingsAction::FocusThemeLabel(t.id.clone()), r));
                    }
                    None => {
                        g.draw_text(
                            cx0 + 14.0, name_y, &t.label,
                            gpu::DrawOpts { font_size: 14.0, color: theme::text(), bold: selected, italic: false },
                        );
                    }
                }
                let sub = if selected {
                    format!("{}명 · 쓰는 중", t.count)
                } else {
                    format!("{}명", t.count)
                };
                g.draw_text(
                    cx0 + 14.0, cy0 + THEME_CARD_H - 24.0, &sub,
                    gpu::DrawOpts { font_size: 11.0, color: theme::text_mute(), bold: false, italic: false },
                );
                // 관리 버튼은 hover 일 때만. 늘 떠 있으면 카드 열두 장에 버튼이
                // 서른여섯 개라 정작 "고르기"가 안 보인다. 번들은 폴더도 없고
                // 지울 수도 없어 버튼 자체를 안 그린다.
                if hover && !t.id.is_empty() {
                    let mut bx = cx0 + tcw - 12.0;
                    for (label, action) in [
                        ("치우기", SettingsAction::DeleteTheme(t.id.clone())),
                        ("폴더", SettingsAction::OpenThemeDir(t.id.clone())),
                        ("이름", SettingsAction::FocusThemeLabel(t.id.clone())),
                    ] {
                        let w = g.measure_chrome_text(label, 11.0, false) + 16.0;
                        bx -= w;
                        let r = (bx, cy0 + THEME_CARD_H - 50.0, w, 22.0);
                        let bh = inside(r, ctx.cursor);
                        round_rect(
                            g, r.0, r.1, r.2, r.3, theme::radius_sm(),
                            if bh { theme::surface_hover() } else { theme::panel_bg() },
                        );
                        g.draw_text(
                            r.0 + 8.0, r.1 + 5.0, label,
                            gpu::DrawOpts { font_size: 11.0, color: theme::text(), bold: false, italic: false },
                        );
                        // 카드보다 **먼저** 담아야 이긴다 — hit-test 가 첫 매치를 쓴다.
                        rects.push((action, r));
                        bx -= 4.0;
                    }
                }
                rects.push((SettingsAction::SelectTheme(t.id.clone()), card));
            }
            // 「+ 새 테마」도 같은 격자의 한 칸 — 목록 밖 버튼으로 빼면 테마가
            // 늘어날수록 멀어져서, 정작 만들려는 사람이 못 찾는다.
            {
                let i = ctx.themes.len();
                let cx0 = fx + (i % cols) as f32 * (tcw + THEME_GAP);
                let cy0 = row_top + (i / cols) as f32 * (THEME_CARD_H + THEME_GAP);
                y = y.max(cy0 + THEME_CARD_H);
                if cy0 + THEME_CARD_H > clip {
                    let card = (cx0, cy0, tcw, THEME_CARD_H);
                    let hover = inside(card, ctx.cursor);
                    g.hover_pointer |= hover;
                    round_rect(
                        g, card.0, card.1, card.2, card.3, theme::radius_md(),
                        if hover { theme::surface_hover() } else { theme::panel_bg() },
                    );
                    let label = "+ 새 테마";
                    let lw = g.measure_chrome_text(label, 14.0, true);
                    g.draw_text(
                        cx0 + (tcw - lw) * 0.5, cy0 + THEME_CARD_H * 0.5 - 16.0, label,
                        gpu::DrawOpts { font_size: 14.0, color: theme::text(), bold: true, italic: false },
                    );
                    let hint = "지금 것을 복제해 시작해요";
                    let hw = g.measure_chrome_text(hint, 11.0, false);
                    g.draw_text(
                        cx0 + (tcw - hw) * 0.5, cy0 + THEME_CARD_H * 0.5 + 6.0, hint,
                        gpu::DrawOpts { font_size: 11.0, color: theme::text_mute(), bold: false, italic: false },
                    );
                    rects.push((SettingsAction::ExportTheme, card));
                }
            }
            y += 20.0;

            // ── 페르소나를 쓸지 ──────────────────────────────────────────
            // 「테마로만 쓸지, 말투까지 쓸지」 — 이 스위치가 그 갈림길이라 테마
            // 바로 아래 둔다(설정은 Claude 쪽과 같은 `claude_persona` 하나다).
            {
                // 「새로 여는 pane 부터」는 빼면 안 된다 — 이미 도는 pane 은 persona 가
                // spawn 시 고정이라 안 바뀌는데, 그걸 안 알리면 전환이 실패한 것으로 읽힌다.
                let (cr, ny) = row2(g, fx, y, fw, clip, "Persona",
                    &["켜면 캐릭터 말투로 대답해요 — 새로 여는 pane 부터"], TOGGLE);
                if ny > clip {
                    toggle(g, cr, ctx.claude_persona, ctx.cursor);
                    rects.push((SettingsAction::ToggleClaudePersona, cr));
                }
                y = ny;
            }

            // 파일명은 줄일 수 없다 — 이걸 안 보고 맞출 방법이 없고, 한 모션이라도
            // 빠지면 그 모션만 번들로 떨어져 한 캐릭터가 두 그림으로 갈린다.
            // wave·cheer 가 빠져 있었다(2026-08-13): 안내대로 idle·walk 만 넣은 사용자는
            // 승인 대기·턴 완료 때만 옛 그림이 튀어나오는 이유를 알 길이 없었다.
            y = row_wide(g, fx, y, clip, "Character images",
                &["테마 폴더의 sprites/ 에 모션별로: idle/<slug>-0..3 · walk/<slug>-0..5",
                  "wave/<slug>-0..3 · cheer/<slug>-0..3 · profile/<slug>.png (폴더 안 README 참고)"]);
            // 액션 버튼 — 지금 쓰는 테마의 그림 폴더 / 로스터 json / 텍스처 재로드.
            // 「새 테마로 복제」는 여기 있었는데 위 격자의 `+ 새 테마` 카드와 같은
            // 동작이라 뺐다. 같은 일을 하는 입구가 한 화면에 둘이면 다른 일을 하는
            // 것처럼 보인다.
            if y > clip {
                let mut bx = fx;
                for (label, action) in [
                    ("이미지 폴더 열기", SettingsAction::OpenStudentsDir),
                    ("로스터 열기", SettingsAction::OpenCharactersJson),
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
            y += 34.0 + 12.0;
            y = row_wide(g, fx, y, clip, "Characters",
                &[&format!("{}명 — 캐릭터를 눌러 성격과 그림을 고치세요", ctx.characters.len())]);
            // 얼굴이 붙은 카드 격자. 한 줄짜리 색 점 목록이던 것을 바꾼 이유는
            // 단순하다 — 79명 중 하나를 고르는 데 이름만 읽어서는 못 찾는다.
            let (scols, scw) = grid_fit(fw, STU_CARD_MIN_W, STU_GAP);
            let sface = (scw - 24.0).floor();
            let srow_top = y;
            for (i, (name, slug)) in ctx.characters.iter().enumerate() {
                let cx0 = fx + (i % scols) as f32 * (scw + STU_GAP);
                let cy0 = srow_top + (i / scols) as f32 * (STU_CARD_H + STU_GAP);
                y = cy0 + STU_CARD_H;
                if cy0 <= clip {
                    continue;
                }
                let card = (cx0, cy0, scw, STU_CARD_H);
                let selected = ctx.student_selected.as_deref() == Some(name.as_str());
                let hover = inside(card, ctx.cursor);
                g.hover_pointer |= hover;
                round_rect(
                    g, card.0, card.1, card.2, card.3, theme::radius_md(),
                    if selected { theme::surface_active() }
                    else if hover { theme::surface_hover() }
                    else { theme::panel_bg() },
                );
                let accent = theme::character_accent(name).unwrap_or([128, 128, 128, 255]);
                if selected {
                    // 강조는 그 캐릭터의 색으로 — 어느 색이 누구 것인지 여기서 배운다.
                    g.rect(cx0, cy0 + STU_CARD_H - 3.0, scw, 3.0, accent);
                }
                if !render::draw_student_face(g, name, cx0 + 12.0, cy0 + 8.0, sface) {
                    // 그림이 없는 캐릭터는 색 판으로 자리를 지킨다 — 칸을 비우면
                    // 격자에 구멍이 뚫려 목록이 끊긴 것처럼 보인다.
                    round_rect(
                        g, cx0 + (scw - 34.0) * 0.5, cy0 + 8.0 + (sface - 34.0) * 0.5,
                        34.0, 34.0, theme::radius_sm(), accent,
                    );
                }
                let nw = g.measure_chrome_text(name, 13.0, selected);
                g.draw_text(
                    cx0 + (scw - nw) * 0.5, cy0 + STU_CARD_H - 34.0, name,
                    gpu::DrawOpts { font_size: 13.0, color: theme::text(), bold: selected, italic: false },
                );
                let sl = slug.unwrap_or("에셋 없음");
                let sw = g.measure_chrome_text(sl, 10.0, false);
                g.draw_text(
                    cx0 + (scw - sw) * 0.5, cy0 + STU_CARD_H - 17.0, sl,
                    gpu::DrawOpts { font_size: 10.0, color: theme::text_mute(), bold: false, italic: false },
                );
                rects.push((SettingsAction::SelectStudent(name.clone()), card));
            }
            content_bottom = y;
        }
        SettingsCat::Feedback => {
            let mut y = fy;
            y = row_wide(g, fx, y, clip, "무엇이 불편했나요",
                &["버그 · 이상한 동작 · 있었으면 하는 것 — 아무 형식이나 괜찮아요",
                  "Enter=줄바꿈, Esc=포커스 해제"]);
            y += multiline_field(
                g, &mut rects, ctx, (fx, y, fw.min(560.0)), &ctx.feedback_body,
                ctx.feedback_caret, SettingsInput::FeedbackBody,
                SettingsAction::FocusFeedbackBody, clip,
            );
            y += 12.0;
            let diag = diag_line();
            {
                let (cr, ny) = row2(g, fx, y, fw, clip, "진단 정보 함께 남기기", &[diag.as_str()], TOGGLE);
                if ny > clip {
                    toggle(g, cr, ctx.feedback_diag, ctx.cursor);
                    rects.push((SettingsAction::ToggleFeedbackDiag, cr));
                }
                y = ny;
            }
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

    // 이번 프레임 높이를 남긴다 — 다음 프레임의 카드가 이 값으로 그려진다.
    form_card_end(content_bottom - fy);
    (rects, content_bottom - fy)
}

/// 카테고리 제목 아래 한 줄. 「이 페이지에서 무엇을 정하는가」를 미리 말한다 —
/// Orca 의 `SettingsSection description` 자리고, 문구도 그쪽 대응 섹션에서 옮겼다
/// (General = "Workspace defaults, app setup, and maintenance." 등).
fn cat_blurb(cat: SettingsCat) -> &'static str {
    match cat {
        SettingsCat::General => "새 창 기본값, 파일 열기, 스크롤 · 탭 배치",
        SettingsCat::Appearance => "테마와 폰트, 사이드바와 상태줄, 창 모양",
        SettingsCat::Shell => "새 pane 이 띄울 셸",
        SettingsCat::Claude => "계정과 자동 전환, 모델 · 추론 강도, 훅",
        SettingsCat::Theme => "학생 그림과 페르소나, 캐릭터 목록",
        SettingsCat::Feedback => "버그와 건의를 보냅니다",
    }
}

/// 세그먼트 컨트롤의 고정 높이. Orca 의 md 사이즈(`px-3 py-1 text-sm`)에 트랙
/// 여백(`p-0.5`)을 더한 값 — 전에 34 였던 것이 다른 컨트롤과 눈금이 안 맞았다.
const SEG_H: f32 = 28.0;


/// 계정 한 줄이 화면에 어떻게 보일지. 값 준비(신원 조회·라벨 폴백)는 부르는 쪽이
/// 하고, 여기는 그리기만 한다 — claude 와 codex 가 신원을 얻는 경로가 전혀 달라
/// (HTTP 왕복 vs 파일 한 개) 그걸 헬퍼 안으로 넣으면 두 갈래가 다시 생긴다.
struct AcctRowView<'a> {
    /// 1줄에 쓸 이름. 라벨이 있으면 라벨, 없으면 이메일, 둘 다 없으면 폴백.
    name: String,
    /// 2줄 — 그 슬롯의 진짜 신원(이메일·조직). 비면 2줄을 안 그린다.
    sub: String,
    sub_col: [u8; 4],
    active: bool,
    /// `None` = 「기본」 행. 우리가 만든 슬롯이 아니라 이름 붙일 것도 지울 것도 없다.
    slot: Option<AcctSlot<'a>>,
}

struct AcctSlot<'a> {
    editing: bool,
    label: &'a str,
    caret: usize,
    caret_on: bool,
    preedit: &'a str,
    /// 이름을 눌렀을 때 — 라벨 편집. Orca 엔 rename 이 없지만(이름=이메일) kasaterm 은
    /// 거노가 붙인 별명이 곧 이름이라, 그 이름을 직접 누르는 것이 가장 짧은 길이다.
    focus: SettingsAction,
    reauth: SettingsAction,
    remove: SettingsAction,
}

/// 계정 한 줄 = **카드 하나**. Orca AccountsPane 의 행 문법 그대로다: 테두리 있는
/// 둥근 카드, 왼쪽에 이름 + `Active` 배지, 그 아래 작은 글씨로 진짜 신원, 오른쪽에
/// 행 단위 액션. 라디오를 뺀 것이 핵심 변화다 — 행 전체가 「이 계정으로」라서 과녁이
/// 16px 점에서 카드 전체로 커진다.
///
/// 히트박스는 **좁은 것부터** push 한다. `settings_click` 이 첫 일치를 쓰므로,
/// 행 전체를 먼저 넣으면 그 안의 버튼이 영원히 안 눌린다.
fn account_card(
    g: &mut gpu::GpuRenderer,
    rects: &mut Vec<(SettingsAction, Rect)>,
    r: Rect,
    cursor: (f32, f32),
    v: &AcctRowView,
    select: SettingsAction,
) {
    let hov = inside(r, cursor);
    let (x, y, w, h) = r;
    // 활성은 채움 + 테두리 둘 다 준다. 채움만으로는 흑백에서 겨우 보이고, 여기는
    // 「지금 어느 계정으로 도는가」라 잘못 읽으면 엉뚱한 계정으로 claude 를 띄운다.
    let fill = if v.active {
        theme::with_alpha(theme::accent(), 0x26)
    } else if hov {
        theme::with_alpha(theme::accent(), 0x14)
    } else {
        theme::with_alpha(theme::surface_active(), 0x40)
    };
    let edge = if v.active { theme::with_alpha(theme::text(), 0x33) } else { theme::border() };
    outline_rect(g, x, y, w, h, theme::radius_md(), edge, 1.0, fill);
    let pad = 12.0_f32;
    // 신원 줄이 없는 슬롯(이름이 곧 이메일이라 2줄이 같은 값이 되는 경우)은 이름을
    // 세로 중앙에 둔다 — 2줄 자리에 1줄만 그리면 카드 위쪽에 붙어 떠 보인다.
    let line1 = if v.sub.is_empty() { y + (h - 18.0) / 2.0 } else { y + 9.0 };
    let line2 = y + 27.0;
    // 오른쪽 액션부터 — 자리를 먹은 만큼 이름이 쓸 폭이 줄어든다.
    let mut right = x + w - pad;
    if let Some(s) = v.slot.as_ref() {
        for (glyph, act) in [("x", &s.remove), ("rotate-cw", &s.reauth)] {
            let br = (right - 24.0, y + (h - 24.0) / 2.0, 24.0, 24.0);
            let bh = inside(br, cursor);
            g.hover_pointer |= bh;
            if bh {
                round_rect(g, br.0, br.1, br.2, br.3, theme::radius_sm(), theme::surface_hover());
            }
            let ic = theme::ICON_SIZE;
            g.queue_icon(glyph, br.0 + (24.0 - ic) / 2.0, br.1 + (24.0 - ic) / 2.0, ic,
                if bh { theme::text() } else { theme::text_mute() });
            rects.push((act.clone(), br));
            right = br.0 - 4.0;
        }
    }
    // 이름 — 편집 중인 슬롯은 그 자리가 텍스트 필드가 된다.
    let name_w = (right - (x + pad) - 8.0).max(60.0);
    match v.slot.as_ref().filter(|s| s.editing) {
        Some(s) => {
            let fr = (x + pad, y + (h - 28.0) / 2.0, name_w.min(240.0), 28.0);
            text_field(g, fr, s.label, s.caret, true, s.caret_on, cursor, s.preedit);
            rects.push((s.focus.clone(), fr));
        }
        None => {
            let nw = g.measure_chrome_text(&v.name, 14.0, v.active);
            g.draw_text(
                x + pad, line1, &v.name,
                gpu::DrawOpts { font_size: 14.0, color: theme::text(), bold: v.active, italic: false },
            );
            if v.active {
                // Orca 와 같은 자리·같은 크기의 `Active` 배지. 라디오를 뺐으니
                // 활성 표시는 이것뿐이라 이름 바로 옆에 붙인다.
                let bx = x + pad + nw + 8.0;
                let bw = g.measure_chrome_text("Active", 10.0, true) + 12.0;
                round_rect(g, bx, line1 - 1.0, bw, 16.0, theme::radius_sm(),
                    theme::with_alpha(theme::text(), 0x1a));
                g.draw_text(
                    bx + 6.0, line1 + 2.0, "Active",
                    gpu::DrawOpts { font_size: 10.0, color: theme::text_dim(), bold: true, italic: false },
                );
            }
            if let Some(s) = v.slot.as_ref() {
                let nr = (x + pad, y, nw.max(40.0) + 8.0, h / 2.0);
                g.hover_pointer |= inside(nr, cursor);
                rects.push((s.focus.clone(), nr));
            }
        }
    }
    if !v.sub.is_empty() {
        g.draw_text(
            x + pad, line2, &v.sub,
            gpu::DrawOpts { font_size: 11.0, color: v.sub_col, bold: false, italic: false },
        );
    }
    // 행 전체 = 이 계정으로 전환. 활성 행은 갈 곳이 없어 손모양도 안 준다.
    g.hover_pointer |= hov && !v.active;
    if !v.active {
        rects.push((select, r));
    }
}

/// 계정 카드의 높이. 두 줄(14px 이름 + 11px 신원)에 Orca 의 py-2.5 를 얹은 값.
const ACCT_H: f32 = 46.0;

/// 「+ 계정 추가」 줄. 로그인이 도는 중이면 그 자리에 진행·결과와 취소를 둔다 —
/// Orca 도 Add Account 를 스피너로 바꾸고 옆에 Cancel 을 붙인다. 이 줄이 버튼 하나로
/// 안 끝나는 이유는 로그인이 이제 **이 화면 안에서** 벌어지기 때문이다: 예전처럼
/// 설정창을 닫고 pane 으로 보내면 결과를 알릴 자리가 없다.
fn add_account_row(
    g: &mut gpu::GpuRenderer,
    rects: &mut Vec<(SettingsAction, Rect)>,
    fx: f32,
    y: f32,
    cursor: (f32, f32),
    add: SettingsAction,
    provider: &str,
) {
    let btn = |g: &mut gpu::GpuRenderer,
               rects: &mut Vec<(SettingsAction, Rect)>,
               x: f32,
               label: &str,
               act: SettingsAction| {
        let bw = g.measure_chrome_text(label, 13.0, false) + 28.0;
        let r = (x, y, bw, 34.0);
        let hov = inside(r, cursor);
        g.hover_pointer |= hov;
        round_rect(g, r.0, r.1, r.2, r.3, theme::radius_md(),
            if hov { theme::surface_hover() } else { theme::surface_active() });
        g.draw_text(
            r.0 + 14.0, r.1 + 9.0, label,
            gpu::DrawOpts { font_size: 13.0, color: theme::text(), bold: false, italic: false },
        );
        rects.push((act, r));
        bw
    };
    let job = login_job().filter(|j| j.provider == provider);
    match job.as_ref().map(|j| &j.state) {
        None => {
            btn(g, rects, fx, "+ 계정 추가", add);
        }
        Some(LoginState::Running) => {
            let t = "로그인 중… 브라우저에서 승인하세요";
            g.draw_text(
                fx, y + 10.0, t,
                gpu::DrawOpts { font_size: 13.0, color: theme::text_dim(), bold: false, italic: false },
            );
            let w = g.measure_chrome_text(t, 13.0, false);
            btn(g, rects, fx + w + 12.0, "취소", SettingsAction::CancelLogin);
        }
        Some(LoginState::Ok) => {
            g.draw_text(
                fx, y + 10.0, "로그인 완료",
                gpu::DrawOpts { font_size: 13.0, color: theme::success(), bold: true, italic: false },
            );
            let w = g.measure_chrome_text("로그인 완료", 13.0, true);
            btn(g, rects, fx + w + 12.0, "확인", SettingsAction::DismissLogin);
        }
        Some(LoginState::Err(msg)) => {
            g.draw_text(
                fx, y + 10.0, msg,
                gpu::DrawOpts { font_size: 13.0, color: theme::danger(), bold: false, italic: false },
            );
            let w = g.measure_chrome_text(msg, 13.0, false);
            btn(g, rects, fx + w + 12.0, "닫기", SettingsAction::DismissLogin);
        }
    }
}

/// 진행 중인 숨은 로그인 한 건. **동시에 하나만** — 두 슬롯을 같이 로그인하면
/// 브라우저 창이 둘 뜨고 어느 창이 어느 슬롯인지 알 수가 없다.
#[derive(Clone)]
pub(crate) struct LoginJob {
    /// 로그인 중인 슬롯 id(`acct-2` · `codex-1`).
    pub(crate) id: String,
    pub(crate) provider: &'static str,
    pub(crate) state: LoginState,
}

#[derive(Clone, PartialEq)]
pub(crate) enum LoginState {
    Running,
    Ok,
    /// 실패 이유 한 줄. 사용자에게 그대로 보인다.
    Err(String),
}

/// 로그인 중인 CLI 프로세스의 그룹 id — 취소가 브라우저 손자까지 걷어내야 한다.
type LoginCell = std::sync::Mutex<(Option<LoginJob>, Option<u32>)>;
fn login_cell() -> &'static LoginCell {
    static CELL: std::sync::OnceLock<LoginCell> = std::sync::OnceLock::new();
    CELL.get_or_init(|| std::sync::Mutex::new((None, None)))
}

/// 지금 진행 중이거나 방금 끝난 로그인. 설정 화면이 매 프레임 읽는다.
pub(crate) fn login_job() -> Option<LoginJob> {
    login_cell().lock().ok()?.0.clone()
}

/// 로그인 표시를 지운다 — 결과를 읽은 사용자가 닫을 때.
pub(crate) fn clear_login_job() {
    if let Ok(mut c) = login_cell().lock() {
        c.0 = None;
    }
}

/// 진행 중인 로그인을 죽인다. **프로세스 그룹째** 죽이는 게 핵심이다 — 로그인
/// 프로세스는 콜백 서버와 브라우저 자식을 남기고, 그 서버가 살아 있으면 다음 시도가
/// 같은 포트에서 막힌다(Orca 도 같은 이유로 POSIX 프로세스 그룹을 쓴다).
pub(crate) fn cancel_login() {
    let pgid = {
        let Ok(mut c) = login_cell().lock() else { return };
        c.0 = None;
        c.1.take()
    };
    if let Some(pgid) = pgid {
        // 셸을 안 거친다 — `kill -TERM -<pgid>` 를 직접 부른다.
        let _ = crate::proc::command("kill")
            .args(["-TERM", &format!("-{pgid}")])
            .status();
    }
}

/// CLI 로그인을 **터미널 없이** 돌린다. pane 을 띄워 사용자가 직접 진행하게 하던
/// 것을 Orca 방식으로 바꾼 것이다(거노 2026-08-13 「로그인방식도 ㄱ」).
///
/// pane 방식은 실제로 두 가지가 나빴다. ①로그인하려면 설정창을 닫고 본창으로
/// 넘어가야 해서, 설정에서 시작한 일이 다른 창에서 끝났다. ②그 pane 이 로그인이
/// 끝난 뒤에도 남아 사용자가 손으로 닫아야 했다. 여기서는 설정 화면이 그 자리에서
/// 「로그인 중… / 취소」를 보이고, 끝나면 그 자리에 결과가 뜬다.
///
/// TTY 없이도 되는 게 확인됐다(2026-08-13 실측): claude CLI 자식이 localhost 에
/// 콜백 서버를 띄우므로 브라우저 승인만 하면 코드를 붙여넣을 일이 없다. 화면에
/// 찍히는 "Paste code here if prompted" 는 콜백이 실패했을 때의 폴백이다. 그래서
/// **stdin 을 열어둔 채** 둔다 — 닫으면 그 폴백 경로가 통째로 죽는다.
fn spawn_hidden_login(
    provider: &'static str,
    id: String,
    argv: String,
    env_key: &'static str,
    dir: std::path::PathBuf,
    profile: Option<std::path::PathBuf>,
) {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    if let Ok(mut c) = login_cell().lock() {
        c.0 = Some(LoginJob { id: id.clone(), provider, state: LoginState::Running });
    }
    std::thread::spawn(move || {
        // 로그인 셸을 거치는 이유는 `auth_probe` 와 같다 — Finder 로 뜬 .app 의
        // PATH 에는 claude·codex 가 없어 직접 spawn 하면 항상 실패한다.
        let shell = resolve_default_shell().unwrap_or_else(|| "/bin/sh".to_string());
        let mut cmd = crate::proc::command(shell);
        cmd.arg("-lc")
            .arg(&argv)
            .env(env_key, &dir)
            // CLI 가 **기본** 브라우저를 열면 지금 계정의 세션이 그대로 승인돼
            // 슬롯을 아무리 갈라도 전부 같은 계정이 붙는다. URL 은 우리가 주워
            // 쿠키 없는 프로필로 연다.
            .env("BROWSER", "/usr/bin/true")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // 자기 프로세스 그룹을 갖게 해 취소가 트리째 걷어낼 수 있게 한다.
            cmd.process_group(0);
        }
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                finish_login(&id, LoginState::Err(format!("로그인 실행 실패: {e}")));
                return;
            }
        };
        if let Ok(mut c) = login_cell().lock() {
            c.1 = Some(child.id());
        }
        // stdin 핸들을 **떨어뜨리지 않는다**(위 주석). 스레드가 끝날 때 닫힌다.
        let _stdin = child.stdin.take();
        // 두 파이프를 각각 읽어 한 버퍼에 모은다 — URL 이 어느 쪽으로 오는지는
        // CLI 버전에 따라 다르고, 실패 이유는 대개 stderr 로 온다.
        let buf = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let mut readers = Vec::new();
        for pipe in [
            child.stdout.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
            child.stderr.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
        ]
        .into_iter()
        .flatten()
        {
            let buf = buf.clone();
            let profile = profile.clone();
            readers.push(std::thread::spawn(move || {
                let mut opened = false;
                for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                    if let Ok(mut b) = buf.lock() {
                        b.push_str(&line);
                        b.push('\n');
                    }
                    if opened {
                        continue;
                    }
                    // 프로세스 출력은 PTY 와 달리 접히지 않아 URL 이 한 줄에 온다.
                    if let (Some(url), Some(prof)) = (login_url_in(&line), profile.as_deref()) {
                        let _ = std::fs::create_dir_all(prof);
                        open_isolated_browser(&url, prof);
                        opened = true;
                    }
                }
            }));
        }
        // 3분. 브라우저에서 계정을 새로 만드는 사람도 있어 넉넉히 두지만, 무한정
        // 두면 취소를 안 누른 사용자가 「로그인 중」에 영구히 갇힌다.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
        let code = loop {
            match child.try_wait() {
                Ok(Some(st)) => break st.success(),
                Ok(None) => {}
                Err(_) => break false,
            }
            // 사용자가 취소하면 pgid 가 비워지고 프로세스는 이미 죽었다.
            if login_cell().lock().is_ok_and(|c| c.0.is_none()) {
                return;
            }
            if std::time::Instant::now() > deadline {
                let _ = child.kill();
                finish_login(&id, LoginState::Err("로그인이 3분 안에 안 끝났어요".into()));
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(300));
        };
        for r in readers {
            let _ = r.join();
        }
        let out = buf.lock().map(|b| b.clone()).unwrap_or_default();
        let state = if code {
            LoginState::Ok
        } else {
            LoginState::Err(login_error_line(&out))
        };
        finish_login(&id, state);
    });
}

/// 결과를 기록한다 — 단, 그 사이 사용자가 취소했거나 다른 슬롯을 시작했으면
/// 덮지 않는다(늦게 끝난 옛 작업이 새 작업의 표시를 갈아치우면 안 된다).
fn finish_login(id: &str, state: LoginState) {
    if let Ok(mut c) = login_cell().lock() {
        if c.0.as_ref().is_some_and(|j| j.id == id) {
            if let Some(j) = c.0.as_mut() {
                j.state = state;
            }
            c.1 = None;
        }
    }
}

/// 실패 출력에서 사용자에게 보일 한 줄을 고른다. 마지막 비공백 줄이 대개 원인이고,
/// 승인 거부는 문구가 정해져 있어 따로 잡는다(Orca 도 같은 패턴을 특별 취급한다).
fn login_error_line(out: &str) -> String {
    let low = out.to_ascii_lowercase();
    if low.contains("access_denied") || low.contains("denied") {
        return "브라우저에서 승인이 거부됐어요".to_string();
    }
    out.lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.chars().take(120).collect())
        .unwrap_or_else(|| "로그인이 실패했어요".to_string())
}

/// 프로세스 한 줄에서 로그인 URL 을 뽑는다. claude·codex 를 같이 받는다 — 접히지
/// 않은 한 줄이라 공백까지 자르면 끝이고, PTY 화면을 폴링하며 접힌 URL 을 이어 붙이던
/// 곡예(state 를 정확히 43자로 끊어야 다음 줄 첫 단어가 안 딸려왔다)가 필요 없다.
fn login_url_in(line: &str) -> Option<String> {
    let at = line.find("https://")?;
    let url: String = line[at..].chars().take_while(|c| !c.is_whitespace()).collect();
    let low = url.to_ascii_lowercase();
    (low.contains("authorize") || low.contains("oauth")).then_some(url)
}

#[cfg(test)]
mod login_url_tests {
    use super::login_url_in;

    /// 2026-08-13 실측 출력 그대로. URL 을 못 뽑으면 격리 브라우저가 안 열리고,
    /// 그러면 CLI 가 기본 브라우저를 열어 지금 계정으로 그대로 승인돼 버린다 —
    /// 슬롯이 전부 같은 계정이 되는 그 버그로 되돌아간다. 조용히 실패하는 자리라
    /// 테스트로 못 박는다.
    #[test]
    fn takes_the_claude_authorize_url() {
        let line = "If the browser didn't open, visit: https://claude.com/cai/oauth/authorize?code=true&client_id=9d1c&state=abc";
        assert_eq!(
            login_url_in(line).as_deref(),
            Some("https://claude.com/cai/oauth/authorize?code=true&client_id=9d1c&state=abc")
        );
    }

    /// 프로세스 출력은 PTY 와 달리 접히지 않아, 뒤에 무슨 말이 붙어도 공백에서 끊긴다.
    #[test]
    fn stops_at_whitespace() {
        let u = login_url_in("visit https://auth.openai.com/oauth/authorize?x=1 and paste the code")
            .expect("URL");
        assert!(u.ends_with("x=1"), "뒷말이 딸려 왔다: {u}");
    }

    /// 로그인과 무관한 링크는 무시한다 — 안내문에 도움말 URL 이 섞여 나온다.
    #[test]
    fn ignores_unrelated_links() {
        assert_eq!(login_url_in("see https://docs.claude.com/help for details"), None);
        assert_eq!(login_url_in("no url here"), None);
    }
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
        let mut c = crate::proc::command(shell);
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
    let Ok(out) = crate::proc::command("curl")
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
    account_identity(id).unwrap_or_else(|| fallback.to_string())
}

/// 그 슬롯이 **실제로** 어느 계정인지 한 줄로 — 진짜 팀 조직이면 조직명, 아니면
/// 이메일. 아직 못 알아냈으면 None.
///
/// 이름 없는 슬롯은 **어느 한도를 쓰는지**로 불러야 한다. 한 이메일에 팀 조직과 개인
/// 조직이 둘 다 달려 있으면 이메일만으로는 두 슬롯이 똑같아 보이는데 정작 한도는 따로
/// 돈다 — 실제로 슬롯 셋이 전부 `2rami@sionic.ai` 로 보이면서 그중 아무도 팀플랜
/// 한도를 안 쓰고 있었다(거노 2026-08-07: "3개가 모두 같을리가없는데").
///
/// 개인 조직은 이름이 `<이메일>'s Organization` 이라 조직명으로 부르면 이메일을 두 번
/// 쓰는 꼴이 된다 — 그래서 **이메일을 품지 않은 조직명**(=진짜 팀)만 조직으로 부르고
/// 나머지는 이메일로 둔다.
///
/// 라벨과 따로 내주는 이유: 라벨은 사람이 붙인 이름이라 **낡는다**. 재로그인으로 슬롯
/// 셋이 전부 같은 개인 계정이 됐는데도 라벨은 여전히 "사이오닉팀플랜" 이라, 세 한도가
/// 하나로 합쳐진 걸 아무도 몰랐다(2026-08-11 실측: 세 슬롯의 사용률이 session 17 ·
/// weekly_all 68 · weekly_scoped 50 으로 완전히 같았다).
pub(crate) fn account_identity(id: &str) -> Option<String> {
    match auth_probe(id) {
        Some(p) if !p.org.is_empty() && !p.org.contains(&p.email) => Some(p.org),
        Some(p) if !p.email.is_empty() => Some(p.email),
        _ => None,
    }
}

/// codex 슬롯이 실제로 어느 ChatGPT 계정인지. 빈 id = 기본 로그인(`~/.codex`).
///
/// claude 판(`auth_probe`)이 HTTP 를 도는 것과 달리 **파일 하나면 끝난다** — `auth.json`
/// 의 id_token 은 JWT 고 payload 에 이메일이 평문으로 들어 있다. 그래서 "확인 중" 같은
/// 중간 상태가 없고, 값이 없으면 정말로 로그인이 안 된 슬롯이다.
///
/// 서명은 확인하지 않는다. 이 값의 쓰임은 «이 슬롯이 누구인가» 를 화면에 적는 것뿐이고,
/// 토큰이 유효한지는 codex 가 그걸로 API 를 부를 때 판가름난다.
pub(crate) fn codex_identity(id: &str) -> Option<String> {
    use base64::Engine as _;
    let path = match socket::codex_account_dir(id) {
        Some(d) => d.join("auth.json"),
        None => std::path::PathBuf::from(std::env::var_os("HOME")?).join(".codex/auth.json"),
    };
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let tok = v.get("tokens")?.get("id_token")?.as_str()?;
    // JWT = header.payload.signature — 가운데만 필요하다. 패딩 없는 URL-safe base64.
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(tok.split('.').nth(1)?)
        .ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    let email = claims.get("email")?.as_str()?;
    (!email.is_empty()).then(|| email.to_string())
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

/// 슬롯을 지울 때 그 이메일 기록도 지운다. 안 지우면 목록에 없는 유령 키가 남고,
/// 같은 번호를 다시 쓰는 슬롯이 **옛 계정의 이메일로 불린다** — statusline 은 이 표만
/// 보므로 화면이 조용히 거짓말을 한다.
fn forget_account_email(id: &str) {
    let Some(serde_json::Value::Object(mut m)) =
        socket::read_settings().get("claude_account_emails").cloned()
    else {
        return;
    };
    if m.remove(id).is_none() {
        return;
    }
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

/// 설정 항목 한 줄 = **두 칸**. 왼쪽에 이름과 설명, 오른쪽에 컨트롤.
///
/// 이 화면이 못생겼던 첫 번째 이유가 이것이었다(거노 2026-08-13 「왤케 못생겼을까」).
/// 전에는 제목 → 설명 → 컨트롤을 위에서 아래로만 쌓았다. 폭을 600 잡아 놓고 왼쪽
/// 정렬만 하니 오른쪽 절반이 늘 비었고, 화면이 「설명 문단 + 작은 컨트롤」의 반복이
/// 되어 읽을 것이 누를 것보다 많았다. 두 칸으로 가르면 그 빈 절반이 컨트롤 자리가
/// 되고, 눈이 왼쪽을 훑다 오른쪽에서 멈춘다 — Orca `SettingsRow` 의 문법이다.
///
/// 반환은 (컨트롤이 놓일 rect, 다음 줄 y). 컨트롤 크기는 부르는 쪽이 준다.
fn row2(
    g: &mut gpu::GpuRenderer,
    x: f32,
    y: f32,
    w: f32,
    clip: f32,
    label: &str,
    desc: &[&str],
    ctrl: (f32, f32),
) -> (Rect, f32) {
    // Orca: 설명이 있으면 py-3, 없으면 py-2. 이 안쪽 여백이 곧 행 사이 간격이라
    // `ROW_GAP` 같은 바깥 간격을 따로 두지 않는다 — 그 둘을 한 값으로 처리한 것이
    // 「항목 안」과 「항목 사이」가 구분되지 않던 원인이었다.
    let pad = if desc.is_empty() { 8.0 } else { 12.0 };
    let text_h = 18.0 + desc.len() as f32 * 16.0;
    let h = pad * 2.0 + text_h.max(ctrl.1);
    if y + h > clip {
        g.draw_text(
            x, y + pad, label,
            gpu::DrawOpts { font_size: 14.0, color: theme::text(), bold: true, italic: false },
        );
        let mut dy = y + pad + 20.0;
        for line in desc {
            g.draw_text(
                x, dy, line,
                gpu::DrawOpts { font_size: 12.0, color: theme::text_mute(), bold: false, italic: false },
            );
            dy += 16.0;
        }
    }
    let cr = (x + w - ctrl.0, y + (h - ctrl.1) / 2.0, ctrl.0, ctrl.1);
    (cr, y + h)
}

/// HSV(h: 0..360, s·v: 0..1) → RGB. 색 선택기 전용 — 표준 육분면 공식.
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [u8; 3] {
    let h = h.rem_euclid(360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [
        ((r + m) * 255.0).round() as u8,
        ((g + m) * 255.0).round() as u8,
        ((b + m) * 255.0).round() as u8,
    ]
}

/// RGB → HSV. 무채색(d≈0)은 h=0 으로 둔다 — 호출부(포커스 시드)가 그 손실을
/// 알고, 이후로는 HSV 쪽(`set_picker_hsv`)을 정본으로 유지한다.
fn rgb_to_hsv(c: [u8; 3]) -> (f32, f32, f32) {
    let (r, g, b) = (c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d <= f32::EPSILON {
        0.0
    } else if (max - r).abs() <= f32::EPSILON {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if (max - g).abs() <= f32::EPSILON {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    let s = if max <= f32::EPSILON { 0.0 } else { d / max };
    (h, s, max)
}

/// custom_theme 의 지금 유효값 27칸(#rrggbb) — `theme::PALETTE_KEYS` 11개 뒤에
/// ANSI 16개. 파일에 적힌(파싱되는) 값이 우선이고, 없는 키는 base 프리셋 값.
/// 포커스 시드(`palette_hex_at`)·화면 표시·ansi 배열 재구성이 전부 이 하나를
/// 읽어야 「누른 칸과 뜬 값이 다른」 어긋남이 없다.
fn palette_hex_list(s: &serde_json::Value) -> Vec<String> {
    let obj = s.get("custom_theme");
    let base_key = obj
        .and_then(|o| o.get("base"))
        .and_then(|x| x.as_str())
        .unwrap_or("dark");
    let (_, _, base) = theme::THEME_PRESETS
        .iter()
        .find(|(k, _, _)| *k == base_key)
        .unwrap_or(&theme::THEME_PRESETS[0]);
    let mut out = Vec::with_capacity(theme::PALETTE_KEYS.len() + 16);
    for (key, get) in theme::PALETTE_KEYS {
        let c = obj
            .and_then(|o| o.get(*key))
            .and_then(|x| x.as_str())
            .and_then(theme::parse_hex)
            .unwrap_or_else(|| {
                let c = get(base);
                [c[0], c[1], c[2]]
            });
        out.push(theme::hex_str(c));
    }
    let arr = obj.and_then(|o| o.get("ansi")).and_then(|x| x.as_array());
    for j in 0..16 {
        let c = arr
            .and_then(|a| a.get(j))
            .and_then(|x| x.as_str())
            .and_then(theme::parse_hex)
            .unwrap_or(base.ansi[j]);
        out.push(theme::hex_str(c));
    }
    out
}

/// 두 칸에 안 담기는 항목(계정 목록·긴 텍스트 필드처럼 폭을 다 쓰는 것)의 머리.
/// 라벨·설명만 그리고 그 아래를 통째로 내준다 — 오른쪽 칸에 밀어 넣으면 컨트롤이
/// 찌그러지므로, 두 칸을 억지로 지키는 것보다 이 층을 하나 두는 편이 낫다.
fn row_wide(
    g: &mut gpu::GpuRenderer,
    x: f32,
    y: f32,
    clip: f32,
    label: &str,
    desc: &[&str],
) -> f32 {
    let pad = 12.0_f32;
    if y > clip {
        g.draw_text(
            x, y + pad, label,
            gpu::DrawOpts { font_size: 14.0, color: theme::text(), bold: true, italic: false },
        );
        let mut dy = y + pad + 20.0;
        for line in desc {
            g.draw_text(
                x, dy, line,
                gpu::DrawOpts { font_size: 12.0, color: theme::text_mute(), bold: false, italic: false },
            );
            dy += 16.0;
        }
    }
    y + pad + 20.0 + desc.len() as f32 * 16.0 + 4.0
}

/// 폼 본문을 담는 카드. Orca 는 섹션 본문을 둥근 카드(`rounded-xl border bg-card/50
/// px-7 py-6`)에 담아 「이 안이 한 묶음」을 형태로 말한다 — 간격만으로 나열하면
/// 어디까지가 한 덩어리인지 눈이 못 끊는다.
///
/// 높이는 **직전 프레임의 콘텐츠 높이**를 쓴다. 조건부 항목(Custom 경로 필드처럼
/// 켤 때만 나오는 것)이 있어 그리기 전에는 높이를 모르고, 카드는 내용보다 먼저
/// 그려야 하기 때문이다(나중에 그리면 방금 쓴 글자를 덮는다). 항목이 늘거나 줄는
/// 그 한 프레임만 어긋나고, 설정 화면은 매 프레임 다시 그리므로 눈에 안 띈다.
fn form_card(g: &mut gpu::GpuRenderer, x: f32, y: f32, w: f32, clip: f32) {
    let h = FORM_H.load(std::sync::atomic::Ordering::Relaxed) as f32;
    if h < 8.0 {
        return;
    }
    let p = CARD_PAD;
    // 스크롤하면 카드 위쪽이 헤더 위로 올라간다. 안에 든 컨트롤은 `clip` 으로
    // 걸러지지만 카드 자신은 배경이라 그냥 그리면 반투명 판이 제목을 덮어
    // 흐려진다 — 헤더 선에서 잘라 낸다(시저가 없어 이렇게만 자를 수 있다).
    let top = (y - p / 2.0).max(clip);
    let bottom = y - p / 2.0 + h + p;
    if bottom <= top {
        return;
    }
    outline_rect(
        g, x - p, top, w + p * 2.0, bottom - top,
        theme::radius_md(), theme::border(), 1.0,
        theme::with_alpha(theme::panel_bg(), 0x99),
    );
}

/// 이번 프레임에 실제로 그린 콘텐츠 높이 — 다음 프레임의 카드가 이 값을 쓴다.
fn form_card_end(h: f32) {
    FORM_H.store(h.clamp(0.0, 20000.0) as u32, std::sync::atomic::Ordering::Relaxed);
}

static FORM_H: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// 카드 안쪽 여백. Orca 의 `px-7 py-6`(28/24)을 하나로 눌러 20 으로 쓴다 — 좌우를
/// 더 벌리면 폼 폭(600)에서 오른쪽 칸이 설 자리가 줄어든다.
const CARD_PAD: f32 = 20.0;

/// 토글이 오른쪽 칸에서 차지하는 크기. Orca `h-5 w-9`(20×36) 그대로 — 전에 52×30
/// 이었던 것이 다른 컨트롤보다 유난히 커서, 같은 행에 서면 눈금이 안 맞았다.
const TOGGLE: (f32, f32) = (36.0, 20.0);

/// 트랙 안쪽 여백(Orca `p-0.5`) — 폭 계산과 그리기가 같은 값을 써야 한다.
const SEG_PAD: f32 = 2.0;
/// 칸 좌우 텍스트 여백(Orca `px-3`).
const SEG_CELL_PAD: f32 = 12.0;

/// 세그먼트가 차지할 폭. `row2` 는 컨트롤 크기를 미리 알아야 오른쪽 정렬을 할 수
/// 있는데, 세그먼트 폭은 라벨 길이로 정해지므로 그리기 전에 한 번 재야 한다.
fn seg_width(g: &mut gpu::GpuRenderer, cells: &[(&str, bool, SettingsAction)]) -> f32 {
    SEG_PAD * 2.0
        + cells
            .iter()
            .map(|(l, _, _)| g.measure_chrome_text(l, 13.0, true) + SEG_CELL_PAD * 2.0)
            .sum::<f32>()
}

/// 세그먼트 컨트롤 — 하나의 트랙 안에 옵션 칸들이 붙어 있는 형태. 칸 폭은 라벨을
/// **bold** 로 재서(선택 시 bold 라 더 넓어짐) 글자가 칸 밖으로 넘치지 않게 한다 —
/// 예전엔 non-bold 로 재고 bold 로 그려 선택 칸 글자가 잘렸다.
///
/// 트랙에 테두리를 두르는 건 Orca 를 따른 것이다(`border border-border
/// bg-background/50`). 채움만으로는 배경과 한 톤 차이라 「여기가 고르는 자리」가
/// 안 읽혔고, 항목이 여럿 쌓이면 어느 칸 묶음이 한 컨트롤인지 흐려졌다.
fn segmented(
    g: &mut gpu::GpuRenderer,
    rects: &mut Vec<(SettingsAction, Rect)>,
    x: f32,
    y: f32,
    cells: &[(&str, bool, SettingsAction)],
    cursor: (f32, f32),
) {
    let pad = SEG_PAD;
    let cell_pad = SEG_CELL_PAD;
    let widths: Vec<f32> = cells
        .iter()
        .map(|(label, _, _)| g.measure_chrome_text(label, 13.0, true) + cell_pad * 2.0)
        .collect();
    let total: f32 = pad * 2.0 + widths.iter().sum::<f32>();
    outline_rect(
        g, x, y, total, SEG_H, theme::radius_md(), theme::border(), 1.0,
        theme::with_alpha(theme::surface_active(), 0x80),
    );
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
    // 꺼진 트랙은 **카드 배경보다 확실히 밝아야** 한다. `surface_active` 는 폼 카드와
    // 같은 대역이라, 카드 안에 놓이자마자 트랙이 사라지고 흰 손잡이만 공중에 떠
    // 보였다(2026-08-13 캡처). Orca 의 `bg-muted-foreground/30` 과 같은 뜻이다.
    let track = if on {
        theme::accent()
    } else if hover {
        theme::with_alpha(theme::text_mute(), 0x66)
    } else {
        theme::with_alpha(theme::text_mute(), 0x4d)
    };
    pill_rect(g, r.0, r.1, r.2, r.3, track);
    // Orca 비율(w9 h5 knob 3.5 = 36×20, 손잡이 14)에 맞춘 여백 3. 전에는 4 여서
    // 20 높이 트랙에 손잡이가 12 로 앉아 유난히 작아 보였다.
    let knob = r.3 - 6.0;
    let kx = if on { r.0 + r.2 - knob - 3.0 } else { r.0 + 3.0 };
    circle_rect(g, kx, r.1 + 3.0, knob, theme::text());
}

/// 캐릭터 상세 — 카드를 누르면 목록 **대신** 이 화면이 뜬다. 사진·이름·성격이
/// 한 자리에 있어야 "이 캐릭터를 고친다"가 한 화면에서 끝난다.
///
/// 목록 아래에 이어 붙이지 않고 통째로 갈아 끼우는 이유: 79명 격자 밑에 편집
/// 패널이 달리면, 고르려고 한참 내려간 사람이 고친 뒤 무엇을 눌렀는지 보려고
/// 다시 올라가야 한다(거노 2026-08-13: "누르면 창이 바뀌던해서").
///
/// 반환값은 콘텐츠 바닥 y — 호출부의 스크롤 클램프가 그걸 쓴다.
#[allow(clippy::too_many_arguments)]
fn student_detail(
    g: &mut gpu::GpuRenderer,
    rects: &mut Vec<(SettingsAction, Rect)>,
    ctx: &SettingsCtx,
    fx: f32,
    fy: f32,
    fw: f32,
    clip: f32,
    sel: &str,
) -> f32 {
    let mut y = fy;
    // 돌아가는 길을 맨 위에. 상세가 목록을 덮으므로 이게 없으면 설정 창을 닫았다
    // 다시 여는 것 말고는 나올 방법이 없다.
    if y > clip {
        let r = chip(g, fx, y, "← 캐릭터 목록", ctx.cursor);
        rects.push((SettingsAction::CloseStudent, r));
    }
    y += 34.0 + 18.0;

    let slug = ctx
        .characters
        .iter()
        .find(|(n, _)| n == sel)
        .and_then(|(_, s)| *s)
        .unwrap_or("");
    // 프사는 크게 — 목록 카드에서 이미 작게 봤는데 여기서도 작으면 넘어온 값이
    // 없다. 좁은 창에서는 오른쪽 열이 밀려나지 않을 만큼만 줄인다.
    let face = 144.0f32.min((fw - 300.0).max(72.0));
    let col_x = fx + face + 20.0;
    let col_w = (fw - face - 20.0).max(160.0);
    if y > clip {
        if !render::draw_student_face(g, sel, fx, y, face) {
            // 그림이 없으면 그 캐릭터의 색 판 — 칸을 비우면 이름 칸이 허공에 뜬다.
            let accent = theme::character_accent(sel).unwrap_or([128, 128, 128, 255]);
            round_rect(g, fx, y, face, face, theme::radius_md(), accent);
        }
        g.draw_text(
            col_x, y + 2.0, "이름",
            gpu::DrawOpts { font_size: 12.0, color: theme::text_mute(), bold: false, italic: false },
        );
        let r = (col_x, y + 22.0, col_w.min(280.0), 34.0);
        let focused = ctx.input == Some(SettingsInput::StudentName);
        text_field(
            g, r, &ctx.student_name, ctx.settings_caret, focused, ctx.caret_on, ctx.cursor,
            if focused { &ctx.preedit } else { "" },
        );
        rects.push((SettingsAction::FocusStudentName, r));
        // 슬러그는 고칠 수 없다 — 그림 파일 이름이 이걸로 정해져 있어서, 바꾸면
        // 넣어 둔 그림이 통째로 안 붙는다. 그래서 보여만 준다.
        let sub = if slug.is_empty() {
            "그림 없음 — 로스터에 slug 가 비어 있어요".to_string()
        } else {
            format!("그림 파일은 {slug} 로 시작해요")
        };
        g.draw_text(
            col_x, y + 64.0, &sub,
            gpu::DrawOpts { font_size: 11.0, color: theme::text_mute(), bold: false, italic: false },
        );
        let r1 = chip(g, col_x, y + 84.0, "이 폴더에 넣기", ctx.cursor);
        rects.push((SettingsAction::OpenStudentsDir, r1));
        let r2 = chip(g, col_x + r1.2 + 8.0, y + 84.0, "새로고침", ctx.cursor);
        rects.push((SettingsAction::RefreshStudentAssets, r2));
    }
    y += face + 22.0;

    y = row_wide(g, fx, y, clip, "성격",
        &["말투·성격을 평문으로. Enter=줄바꿈, 바깥 클릭·Esc=저장"]);
    y += multiline_field(
        g, rects, ctx, (fx, y, fw.min(560.0)), &ctx.student_persona,
        ctx.student_caret, SettingsInput::StudentPersona,
        SettingsAction::FocusStudentPersona, clip,
    );

    if !slug.is_empty() {
        y += 14.0;
        // 전체 규칙이 아니라 **이 캐릭터의 실제 파일명**을 적는다 — `<slug>` 를
        // 자기 이름으로 바꿔 적는 그 한 단계에서 사람은 틀린다.
        y = row_wide(g, fx, y, clip, "그림 파일 이름",
            &[&format!("idle/{slug}-0..3.png · walk/{slug}-0..5.png · wave/{slug}-0..3.png"),
              &format!("cheer/{slug}-0..3.png · profile/{slug}.png")]);
    }
    y
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
