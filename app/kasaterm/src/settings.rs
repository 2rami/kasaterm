//! 웹 설정 화면의 값·동작 배관. 화면과 입력은 `web/arona-ui`가 소유하고,
//! 여기서는 저장된 값의 검증과 앱 런타임 반영만 맡는다.

use super::*;

type Rect = (f32, f32, f32, f32);

fn inside(r: Rect, p: (f32, f32)) -> bool {
    p.0 >= r.0 && p.0 <= r.0 + r.2 && p.1 >= r.1 && p.1 <= r.1 + r.3
}

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
    /// 로스터·클로드 노브가 바뀌면 pane PATH 의 셰임을 다시 굽는다.
    ///
    /// 학생 이름 셰임(`시로코`)까지 함께 굽는 것이 중요하다 — 그 스크립트에는
    /// 그 학생의 성격과 모델이 **구워져** 있어서, 클로드 셰임만 다시 만들면
    /// 설정에서 고친 값이 이름 명령으로 뜬 pane 에는 영영 안 붙는다.
    fn regen_pane_shims(&self) {
        if let Ok(dir) = std::env::var("KASATERM_TMUX_SHIM_DIR") {
            let dir = std::path::Path::new(&dir);
            install_claude_hook_shim(dir);
            install_student_shims(dir);
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
        socket::write_setting("status_bar_h", serde_json::Value::from(self.set_status_h));
        socket::write_setting("pane_footer_h", serde_json::Value::from(self.set_pane_footer_h));
        self.regen_pane_shims();
        // codex 는 래퍼를 다시 굽지 않는다 — 값이 하나도 안 박힌 정적 문자열이라
        // 다시 구울 이유가 없고, 활성 슬롯 경로만 파일로 갈아 끼우면 **이미 떠 있는
        // pane 도 다음 codex 실행부터** 그 계정으로 뜬다.
        if let Ok(dir) = std::env::var("KASATERM_TMUX_SHIM_DIR") {
            crate::write_codex_account_file(std::path::Path::new(&dir));
        }
    }

    /// Claude 계정 조작을 본진으로 넘긴다 — 넘겼으면 `true`.
    ///
    /// 계정 등록·자동 전환은 **학생이 실제로 도는 기계**에서 일어나야 한다. 본진
    /// 디스패치를 켠 뒤로 순정 `claude` 는 본진에서 태어나는데, 계정 목록과 사용량
    /// 감시는 기계마다 따로인 로컬 상태라 그대로 갈라져 있었다(2026-09-05 실측:
    /// 작업대 슬롯 4개·자동전환 켜짐·80%, 본진 슬롯 0개·꺼짐·90%). 등록을 아무리
    /// 반복해도 실제로 한도가 차는 기계에는 아무것도 없어 자동 전환이 영영 일어나지
    /// 않았다 — 거노 "계정 등록만 100번 한 것 같은데".
    ///
    /// codex 슬롯은 아직 로컬 그대로 둔다. 지금 본진에서 도는 건 claude 뿐이다.
    fn route_account_action_to_home(&mut self, action: &SettingsAction) -> bool {
        use crate::homeaccounts;
        // **고른 칸이 본진일 때만** 넘긴다. 본진이 켜져 있다고 이 기계 계정까지
        // 원격으로 보내면, 여기서 도는 claude 의 로그인을 고칠 자리가 화면에서
        // 사라진다 — 2026-09-05 에 실제로 그렇게 만들어 거노가 「설정창이랑 하단이랑
        // 왜 다르냐」고 물었다.
        if !self.set_account_scope_home || homeaccounts::home_target().is_none() {
            return false;
        }
        match action {
            SettingsAction::AddClaudeAccount => {
                homeaccounts::act("add-claude-account", None, None);
                self.set_toast("본진에서 로그인을 시작했어요".to_string());
            }
            SettingsAction::RemoveClaudeAccount(id) => {
                homeaccounts::act("remove-claude-account", Some(id.clone()), None);
            }
            SettingsAction::SwitchAccount(AccountProvider::Claude, id) => {
                homeaccounts::act("claude-account", Some(id.clone()), None);
            }
            SettingsAction::ReauthAccount(AccountProvider::Claude, id, browser) => {
                let act = match browser {
                    LoginBrowser::Isolated => "reauth-account-isolated",
                    LoginBrowser::Default => "reauth-account",
                };
                homeaccounts::act(act, Some(id.clone()), Some("claude".to_string()));
                self.set_toast("본진에서 로그인을 다시 시작했어요".to_string());
            }
            SettingsAction::ToggleAccountAutoswitch => {
                homeaccounts::act("toggle-account-autoswitch", None, None);
            }
            SettingsAction::AccountAutoswitchPct(pct) => {
                homeaccounts::act("autoswitch-pct", Some(pct.to_string()), None);
            }
            SettingsAction::CancelLogin => {
                homeaccounts::act("cancel-login", None, None);
                self.login_code_edit.clear();
            }
            SettingsAction::SubmitLoginCode => {
                let code = self.login_code_edit.trim().to_string();
                if code.is_empty() {
                    self.set_toast("브라우저에 나온 코드를 붙여넣어 주세요".to_string());
                    return true;
                }
                homeaccounts::act("login-code", Some(code), None);
                self.login_code_edit.clear();
                self.settings_caret = 0;
                self.set_toast("코드를 보냈어요 — 확인 중이에요".to_string());
            }
            _ => return false,
        }
        true
    }

    /// 본진 조작이 남긴 실패 말풍선을 화면에 올린다. 렌더 틱에서 부른다 — 동작이
    /// 백그라운드 스레드에서 끝나 그 자리에서는 `App` 에 못 쓴다.
    pub(crate) fn drain_home_account_toasts(&mut self) {
        for msg in crate::homeaccounts::drain_toasts() {
            self.set_toast(msg);
        }
    }

    /// 검사 하네스가 조건부 칸을 세울 때 **이것들을 쓴다**(자유함수를 직접 부르지
    /// 말 것).
    ///
    /// 심는 값이 전역 셀이라, 그것만 바꾸면 렌더가 「다시 그릴 이유가 없다」며
    /// 프레임을 통째로 건너뛴다(`render_frame` 의 `rebuild` 게이트). 그러면 세운
    /// 칸이 영영 안 그려지고, 감사는 그걸 「그 칸이 화면에 없다」로 읽는다
    /// (2026-09-05 실측: seed 는 성공하는데 「첫 프레임 0ms」로 세 번 다 건너뛰었다).
    /// 표시를 함께 세워야 그 프레임이 나간다.
    #[allow(dead_code)]
    pub(crate) fn seed_login_probe(&mut self, id: &str, state: LoginState) -> bool {
        let ok = seed_login_state_for_probe(id, state);
        if ok {
            self.chrome_dirty = true;
        }
        ok
    }

    /// 본진 계정 칸을 세운다. 위와 같은 이유로 표시를 함께 세운다.
    #[allow(dead_code)]
    pub(crate) fn seed_home_accounts_probe(
        &mut self,
        label: &str,
        value: kasa_mcp::remote::RemoteAccounts,
    ) {
        crate::homeaccounts::seed_for_probe(label, value);
        self.chrome_dirty = true;
    }

    /// 심어 둔 것을 **전부** 걷는다. 검사 끝에 반드시 부를 것 — 심은 상태에는
    /// 타임아웃이 없어 안 걷으면 그 창이 가짜 화면에 머문다.
    #[allow(dead_code)]
    pub(crate) fn clear_account_probes(&mut self) {
        cancel_hidden_login();
        crate::homeaccounts::clear_probe();
        self.login_code_edit.clear();
        self.chrome_dirty = true;
    }

    /// 붙여넣은 OAuth 코드를 로그인 중인 CLI 로 보낸다. 엔터와 「확인」 버튼이 같은
    /// 길을 탄다.
    /// 고치던 기계 칸을 명부에 되쓴다. **다른 필드는 손대지 않는다** — 손으로 적은
    /// 옛 항목(터널 설정이 딸린 것)이 있어서, 화면이 아는 두 칸만 갈아 끼우고
    /// 나머지는 읽은 그대로 되돌려 쓴다.
    pub(crate) fn flush_machine_field(&mut self) {
        let Some((idx, ssh, value)) = self.machine_edit.take() else {
            return;
        };
        let value = value.trim().to_string();
        let mut list = kasa_mcp::machines::entries();
        let Some(entry) = list.get_mut(idx).and_then(|e| e.as_object_mut()) else {
            return;
        };
        let key = if ssh { "ssh" } else { "label" };
        if entry.get(key).and_then(|v| v.as_str()).unwrap_or_default() == value {
            return;
        }
        entry.insert(key.to_string(), serde_json::Value::String(value));
        if kasa_mcp::machines::save_entries(&list).is_err() {
            self.set_toast("명부를 저장하지 못했어요".to_string());
        }
        self.chrome_dirty = true;
    }

    pub(crate) fn submit_login_code_field(&mut self) {
        let code = self.login_code_edit.trim().to_string();
        if code.is_empty() {
            self.set_toast("브라우저에 나온 코드를 붙여넣어 주세요".to_string());
            return;
        }
        if submit_login_code(&code) {
            self.login_code_edit.clear();
            self.settings_caret = 0;
            self.set_toast("코드를 보냈어요 — 확인 중이에요".to_string());
        } else {
            self.set_toast("보낼 로그인이 없어요 — 다시 시작해 주세요".to_string());
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
        if hidden_login_running() {
            self.set_toast("다른 로그인이 끝난 뒤 다시 시도해 주세요".to_string());
            return;
        }
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
        spawn_hidden_login(
            id.clone(),
            "claude auth login --claudeai".to_string(),
            "CLAUDE_SECURESTORAGE_CONFIG_DIR",
            dir,
            login_browser_default(),
        );
        self.set_toast(add_account_toast());
    }

    /// 같은 것의 codex 판 — 슬롯을 만들고 그 홈을 얹은 `codex login` 을 새 pane 에
    /// 띄운다. claude 와 마찬가지로 **추가만 하고 활성 전환은 안 한다**(아직 아무도
    /// 로그인 안 한 슬롯으로 갈아타면 그 뒤 codex 가 전부 로그아웃 상태로 뜬다).
    ///
    /// id 접두사를 `codex-` 로 갈라 두는 건 OAuth 브라우저 프로필 때문이다 —
    /// `oauth_profile_dir` 은 id 하나로 자리를 잡으므로 claude 의 `acct-1` 과 겹치면
    /// 두 서비스가 같은 브라우저 프로필을 나눠 쓰게 된다.
    fn add_codex_account(&mut self) {
        if hidden_login_running() {
            self.set_toast("다른 로그인이 끝난 뒤 다시 시도해 주세요".to_string());
            return;
        }
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
            id.clone(),
            "codex login".to_string(),
            "CODEX_HOME",
            dir,
            login_browser_default(),
        );
        self.set_toast(add_account_toast());
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
        if socket::read_character_theme() == id {
            return;
        }
        // 편집 중이던 persona 는 **먼저** 옛 테마 파일에 흘려보낸다. 순서를 바꾸면
        // 활성 테마가 이미 새것이라, 옛 테마에서 고친 글이 새 테마 파일에 쓰인다.
        self.flush_student_persona();
        socket::write_setting("character_theme", serde_json::Value::String(id.clone()));
        kasa_mcp::character::invalidate_active_theme();
        theme::invalidate_roster();
        // 목록의 "쓰는 중" 배지가 어느 카드에 붙는지가 여기서 바뀐다.
        socket::invalidate_theme_rows();
        self.students_selected = None;
        self.students_persona.clear();
        self.students_theme = id;
        self.students_slug.clear();
        self.students_model.clear();
        self.students_backend.clear();
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
                // 활성이 아닌 테마를 치웠으면 위 `select_theme` 갈래를 안 타므로 이름·그림
                // 합집합이 안 비워진다 — 치운 테마의 캐릭터가 계속 조회에 잡힌다.
                theme::invalidate_roster();
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

    /// 네이티브 팔레트 이름 칸을 저장한다. 웹 액션도 같은 순수 서비스를 써서
    /// 어느 화면에서 이름을 바꾸든 검증과 파일 모양이 갈리지 않는다.
    pub(crate) fn flush_custom_theme_label(&mut self) {
        let Some((slug, label)) = self.custom_theme_label_edit.clone() else { return };
        match rename_custom_theme(&slug, &label) {
            Ok(()) => self.custom_theme_label_edit = None,
            Err(error) => self.set_toast(error),
        }
    }

    /// 네이티브 계정 카드의 별명 편집을 계정 목록에 굳힌다.
    pub(crate) fn flush_account_label(&mut self) {
        let Some((provider, id, label)) = self.account_label_edit.clone() else { return };
        if provider == AccountProvider::Claude
            && self.set_account_scope_home
            && crate::homeaccounts::home_target().is_some()
        {
            crate::homeaccounts::act(
                "claude-account-label",
                Some(id),
                Some(label.trim().to_string()),
            );
            self.account_label_edit = None;
            return;
        }
        let found = match provider {
            AccountProvider::Claude => self
                .set_claude_accounts
                .iter_mut()
                .find(|account| account.id == id)
                .map(|account| account.label = label.trim().to_string())
                .is_some(),
            AccountProvider::Codex => self
                .set_codex_accounts
                .iter_mut()
                .find(|account| account.id == id)
                .map(|account| account.label = label.trim().to_string())
                .is_some(),
        };
        if found {
            self.settings_save();
            self.account_label_edit = None;
        } else {
            self.set_toast("그 계정 슬롯이 더는 없어요".to_string());
        }
    }

    /// 테마 목록 `(폴더 id, 표시명)` — 번들은 목록에 없다(폴더가 없어서).
    fn settings_snapshot_themes(&self) -> Vec<(String, String)> {
        kasa_mcp::character::list_themes()
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

    /// 설정 항목 하나를 실행한다. `settings_click` 에서 갈라 둔 건 히트렉트 없이도
    /// (헤드리스 검증·단축키) 같은 경로를 탈 수 있게 하기 위한 것이다.
    pub(crate) fn settings_apply(&mut self, action: SettingsAction) {
        if self.route_account_action_to_home(&action) {
            return;
        }
        match action {
            SettingsAction::UiLanguage(language) => {
                socket::write_setting("language", serde_json::json!(language));
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
            SettingsAction::ThemeMode(m) => {
                self.begin_theme_fx();
                theme::set_theme(&m);
                socket::write_setting("theme", serde_json::Value::String(m));
                self.repaint_all();
            }
            SettingsAction::ThemeSystemSlot(light, key) => {
                socket::write_setting(
                    if light { "theme_system_light" } else { "theme_system_dark" },
                    serde_json::Value::String(key),
                );
                if theme::theme_name() == "system" {
                    self.begin_theme_fx();
                    theme::set_theme("system");
                    self.repaint_all();
                }
            }
            SettingsAction::StartCustomTheme => {
                // 언제나 **새** 팔레트를 더한다 — 하던 편집은 목록에 그대로 남고,
                // 이어서 고치려면 그 카드를 고르면 된다(2026-08-15 지시: 커스텀을
                // 여러 개). 하나뿐이던 시절엔 이 버튼이 「이어서 편집」을 겸했다.
                let s = socket::read_settings();
                let entry = theme::clone_current_custom(&s);
                let key = format!("custom:{}", theme::custom_slug(&entry));
                let mut list = theme::custom_themes(&s);
                list.push(entry);
                write_custom_themes(list);
                self.settings_input = None;
                self.begin_theme_fx();
                theme::set_theme(&key);
                socket::write_setting("theme", serde_json::Value::String(key));
                self.repaint_all();
            }
            SettingsAction::ResetCustomTheme => {
                // 지금 편집 중인 것만 되돌린다. 이름은 지키고 색만 base 로 —
                // 되돌리기 한 번에 카드 이름까지 바뀌면 목록에서 그것을 잃는다.
                let s = socket::read_settings();
                let mut list = theme::custom_themes(&s);
                let want = theme::active_custom_slug().unwrap_or_default();
                if let Some(e) = list.iter_mut().find(|e| {
                    want.is_empty() || theme::custom_slug(e) == want
                }) {
                    let base = e.get("base").and_then(|x| x.as_str()).unwrap_or("dark").to_string();
                    *e = theme::custom_theme_seed(
                        &base,
                        &theme::custom_slug(e),
                        &theme::custom_label(e),
                    );
                }
                write_custom_themes(list);
                self.settings_input = None;
                self.begin_theme_fx();
                theme::set_theme(&theme::theme_name());
                self.repaint_all();
            }
            SettingsAction::DeleteCustomTheme(slug) => {
                // system 밝기 슬롯이 이것을 가리키는지는 **지우기 전에** 본다.
                // 파일에 적힌 값은 옛 `"custom"` 일 수도 있어 문자열 그대로는 못
                // 견준다 — `system_slot_theme` 이 실재 카드 키로 굳혀 준다.
                let doomed = format!("custom:{slug}");
                let orphaned: Vec<&str> = [("theme_system_light", true), ("theme_system_dark", false)]
                    .into_iter()
                    .filter(|(_, light)| theme::system_slot_theme(*light) == doomed)
                    .map(|(k, _)| k)
                    .collect();
                let s = socket::read_settings();
                let mut list = theme::custom_themes(&s);
                list.retain(|e| theme::custom_slug(e) != slug);
                write_custom_themes(list);
                // 배정이 떴으면 내장으로 되돌린다. 그냥 두면 팔레트는 프리셋으로
                // 폴백해 도는데 배정해 둔 사실만 화면에서 사라진다.
                for key in orphaned {
                    let fallback = if key.ends_with("light") { "light" } else { "dark" };
                    socket::write_setting(key, serde_json::Value::String(fallback.to_string()));
                }
                self.settings_input = None;
                self.begin_theme_fx();
                // 지우던 것을 입고 있었으면 갈아입어야 한다 — 남은 첫 커스텀,
                // 그것도 없으면 그 팔레트의 base 로. `set_theme` 이 빈 목록을
                // dark 로 떨어뜨리므로 여기서는 키만 다시 세운다.
                if theme::active_custom_slug().is_some_and(|a| a == slug || a.is_empty()) {
                    theme::set_theme("custom");
                    let key = theme::theme_name();
                    socket::write_setting("theme", serde_json::Value::String(key));
                } else {
                    theme::set_theme(&theme::theme_name());
                }
                self.repaint_all();
            }
            SettingsAction::FocusCustomThemeLabel(slug) => {
                self.flush_custom_theme_label();
                let label = theme::custom_themes(&socket::read_settings())
                    .iter()
                    .find(|entry| theme::custom_slug(entry) == slug)
                    .map(theme::custom_label)
                    .unwrap_or_else(|| slug.clone());
                self.settings_caret = label.chars().count();
                self.custom_theme_label_edit = Some((slug, label));
                self.settings_input = Some(SettingsInput::CustomThemeLabel);
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
            SettingsAction::PaletteEyedropper(slot) => {
                if crate::eyedropper::supported() {
                    crate::eyedropper::pick_screen_color(slot);
                } else {
                    self.set_toast("이 운영체제에선 화면 집기를 아직 못 써요".to_string());
                }
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
            SettingsAction::CursorShape(shape) => {
                if self.cursor_shape != shape {
                    self.cursor_shape = shape;
                    socket::write_setting(
                        "cursor_shape",
                        serde_json::Value::String(shape.as_str().to_string()),
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
            SettingsAction::SwitchAccount(provider, id) => match provider {
                AccountProvider::Claude => self.ask_or_switch_claude_account(
                    &id,
                    crate::session::ConfirmSurface::Main,
                ),
                AccountProvider::Codex => self.ask_or_switch_codex_account(
                    &id,
                    crate::session::ConfirmSurface::Main,
                ),
            },
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
            // 높이가 바뀌면 격자에서 예약하는 띠도 달라진다 — PTY 를 다시 재지
            // 않으면 셸이 옛 행수로 그리고, 그 차이만큼 화면이 어긋난 채 남는다.
            // 1px 스테퍼라 끝에서 계속 눌린다 — 자르고 나서 **값이 그대로면 아무
            // 것도 하지 않는다**. 안 그러면 누를 때마다 저장·PTY 재조정이 도는데,
            // 화면은 하나도 안 바뀌어서 그 비용이 어디서 나는지 보이지도 않는다.
            SettingsAction::StatusBarH(px) => {
                let want = (px as f32).clamp(socket::STATUS_H_MIN, socket::STATUS_H_MAX);
                if (want - self.set_status_h).abs() > 0.01 {
                    self.set_status_h = want;
                    self.settings_save();
                    let (cols, rows) = self.window_cells();
                    self.resize_backend(cols, rows);
                    self.chrome_dirty = true;
                }
            }
            SettingsAction::PaneFooterH(px) => {
                let want =
                    (px as f32).clamp(socket::PANE_FOOTER_H_MIN, socket::PANE_FOOTER_H_MAX);
                if (want - self.set_pane_footer_h).abs() > 0.01 {
                    self.set_pane_footer_h = want;
                    self.settings_save();
                    let (cols, rows) = self.window_cells();
                    self.resize_backend(cols, rows);
                    self.chrome_dirty = true;
                }
            }
            SettingsAction::AddClaudeAccount => self.add_claude_account(),
            // 있는 슬롯에 로그인을 다시 돌린다 — 슬롯 dir 을 그대로 쓰므로 그 계정에
            // 붙은 한도 이력이 남는다. 새로 만들었다 지우는 것과 여기가 갈린다.
            SettingsAction::ReauthAccount(p, id, browser) => {
                if hidden_login_running() {
                    self.set_toast("다른 로그인이 끝난 뒤 다시 시도해 주세요".to_string());
                    return;
                }
                if hidden_login_running() {
                    self.set_toast("다른 로그인이 끝난 뒤 다시 시도해 주세요".to_string());
                    return;
                }
                let (argv, key, dir) = match p {
                    AccountProvider::Claude => (
                        "claude auth login --claudeai",
                        "CLAUDE_SECURESTORAGE_CONFIG_DIR",
                        socket::claude_account_dir(&id),
                    ),
                    AccountProvider::Codex => {
                        ("codex login", "CODEX_HOME", socket::codex_account_dir(&id))
                    }
                };
                let Some(dir) = dir else {
                    self.set_toast("계정 폴더 경로를 만들 수 없습니다".to_string());
                    return;
                };
                let _ = std::fs::create_dir_all(&dir);
                spawn_hidden_login(id, argv.to_string(), key, dir, browser);
                self.set_toast(
                    match browser {
                        LoginBrowser::Isolated => "빈 브라우저 창에서 로그인하세요",
                        // 쓰던 브라우저는 이미 붙어 있는 계정으로 승인된다 — 다른
                        // 계정을 붙이려던 사람이 결과를 보고 놀라지 않게 미리 말한다.
                        LoginBrowser::Default => "쓰던 브라우저에서 승인하세요 — 지금 로그인된 계정으로 붙어요",
                    }
                    .to_string(),
                );
            }
            SettingsAction::CancelLogin => {
                cancel_hidden_login();
                self.login_code_edit.clear();
                self.set_toast("로그인을 취소했어요".to_string());
            }
            SettingsAction::SubmitLoginCode => self.submit_login_code_field(),
            SettingsAction::AccountScopeHome(home) => {
                self.set_account_scope_home = home && crate::homeaccounts::home_target().is_some();
            }
            SettingsAction::FocusAccountLabel(provider, id) => {
                self.flush_account_label();
                let label = match provider {
                    AccountProvider::Claude => self
                        .set_claude_accounts
                        .iter()
                        .find(|account| account.id == id)
                        .map(|account| account.label.clone()),
                    AccountProvider::Codex => self
                        .set_codex_accounts
                        .iter()
                        .find(|account| account.id == id)
                        .map(|account| account.label.clone()),
                };
                if let Some(label) = label {
                    self.settings_caret = label.chars().count();
                    self.account_label_edit = Some((provider, id, label));
                    self.settings_input = Some(SettingsInput::AccountLabel);
                }
            }
            SettingsAction::AddMachine => {
                let mut list = kasa_mcp::machines::entries();
                let idx = list.len();
                list.push(serde_json::json!({ "label": "", "ssh": "" }));
                if kasa_mcp::machines::save_entries(&list).is_ok() {
                    self.settings_caret = 0;
                    self.machine_edit = Some((idx, false, String::new()));
                    self.settings_input = Some(SettingsInput::MachineField);
                } else {
                    self.set_toast("명부를 저장하지 못했어요".to_string());
                }
                self.chrome_dirty = true;
            }
            SettingsAction::RemoveMachine(idx) => {
                let mut list = kasa_mcp::machines::entries();
                if idx < list.len() {
                    list.remove(idx);
                    if kasa_mcp::machines::save_entries(&list).is_err() {
                        self.set_toast("명부를 저장하지 못했어요".to_string());
                    }
                }
                self.machine_edit = None;
                self.settings_input = None;
                self.chrome_dirty = true;
            }
            SettingsAction::FocusMachineField(idx, ssh) => {
                // 고치기 전에 열려 있던 칸을 먼저 확정한다 — 안 그러면 옆 칸을
                // 누르는 것만으로 방금 친 글자가 사라진다.
                if self.machine_edit.is_some() {
                    self.flush_machine_field();
                }
                let key = if ssh { "ssh" } else { "label" };
                // ssh 칸은 옛 항목의 `host` 도 초기값으로 받는다 — 빈칸으로 열면
                // 사람이 이미 적어 둔 주소를 다시 치게 된다.
                let entries = kasa_mcp::machines::entries();
                let entry = entries.get(idx);
                let value = entry
                    .and_then(|e| e.get(key))
                    .and_then(|v| v.as_str())
                    .filter(|v| !v.is_empty())
                    .or_else(|| {
                        ssh.then(|| entry.and_then(|e| e.get("host")).and_then(|v| v.as_str()))
                            .flatten()
                    })
                    .unwrap_or_default()
                    .to_string();
                self.settings_caret = value.chars().count();
                self.machine_edit = Some((idx, ssh, value));
                self.settings_input = Some(SettingsInput::MachineField);
                self.chrome_dirty = true;
            }
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
            SettingsAction::OpenStudentsDir => self.open_students_dir(),
            SettingsAction::OpenCharactersJson => self.open_characters_json(),
            SettingsAction::RefreshStudentAssets => self.refresh_student_assets(),
            SettingsAction::SelectTheme(id) => self.select_theme(id),
            SettingsAction::ExportTheme => self.export_theme(),
            SettingsAction::OpenThemeDir(id) => self.open_theme_dir(&id),
            SettingsAction::DeleteTheme(id) => self.delete_theme(&id),
            SettingsAction::FocusThemeLabel(id) => self.focus_theme_label(id),
            SettingsAction::InspectTheme(id) => self.settings_scene.toggle_inspected_theme(id),
            SettingsAction::ThemePickAll(theme, on) => {
                if let Err(error) = self.apply_theme_pick_all(&theme, on) {
                    self.set_toast(error);
                }
            }
            SettingsAction::CharacterPick(theme, name, on) => {
                if let Err(error) = self.apply_character_pick(&theme, Some(&name), on) {
                    self.set_toast(error);
                }
            }
            SettingsAction::SelectStudent(name) => self.select_student_for_edit(name),
            SettingsAction::SelectStudentInTheme(theme, name) => {
                self.select_student_for_edit_in_theme(theme, name)
            }
            SettingsAction::CloseStudent => self.close_student_edit(),
            SettingsAction::FocusStudentName => {
                self.settings_caret = self.students_name.chars().count();
                self.settings_input = Some(SettingsInput::StudentName);
            }
            SettingsAction::ToggleFeedbackDiag => self.feedback_diag = !self.feedback_diag,
            SettingsAction::SaveFeedback => self.save_feedback(),
            SettingsAction::OpenFeedbackDir => {
                let dir = feedback_dir();
                let _ = std::fs::create_dir_all(&dir);
                open_path(&dir);
            }
            SettingsAction::ToggleStudentRaw(on) => {
                self.students_raw.open = on;
                self.settings_input = None;
                if on {
                    self.reload_student_raw();
                }
            }
            SettingsAction::StudentRawFormat(yaml) => {
                if self.students_raw.yaml != yaml {
                    self.students_raw.yaml = yaml;
                    self.reload_student_raw();
                }
            }
            SettingsAction::SaveStudentRaw => self.save_student_raw(),
            SettingsAction::StudentModel(model, backend) => {
                let Some(name) = self.students_selected.clone() else { return };
                // 빈 값도 그대로 쓴다(키를 지우지 않는다) — 읽는 쪽이 빈 문자열을
                // "지정 없음"으로 걸러내므로 결과가 같고, 삭제 경로를 따로 두면
                // 로스터가 테마 파일일 때와 홈 override 일 때 두 곳을 맞춰야 한다.
                let _ = kasa_mcp::character::update_member_in_theme(
                    &self.students_theme, &name, "model", serde_json::Value::String(model.clone()));
                let _ = kasa_mcp::character::update_member_in_theme(
                    &self.students_theme, &name, "backend", serde_json::Value::String(backend.clone()));
                self.students_model = model;
                self.students_backend = backend;
                self.regen_pane_shims();
            }
            SettingsAction::ThemeGenProvider(p) => {
                socket::write_setting("theme_gen_provider", serde_json::Value::String(p));
                self.settings_input = None;
            }
            SettingsAction::ThemeGenStart => {
                if self.students_selected.is_none() {
                    return;
                }
                let slug = self.students_slug.clone();
                if slug.is_empty() {
                    self.set_toast("이 캐릭터의 그림 이름을 못 찾았어요".to_string());
                    return;
                }
                let theme_id = self.students_theme.clone();
                let Some((path, _)) = themegen_ref_info(&theme_id, &slug) else {
                    self.set_toast("먼저 참조 그림을 놓아 주세요".to_string());
                    return;
                };
                self.themegen_start(&theme_id, &slug, &path, None);
            }
            SettingsAction::SelectMotionFrame(motion, frame) => {
                self.settings_scene.toggle_sprite_slot(motion, frame);
            }
            SettingsAction::ResetMotion(motion) => {
                if self.students_selected.is_none() {
                    return;
                }
                let slug = self.students_slug.clone();
                match socket::clear_character_sprite_files_in_theme(
                    &self.students_theme,
                    &slug,
                    &motion,
                ) {
                    Ok(_) => {
                        self.refresh_student_assets();
                        self.set_toast("기본 그림으로 되돌렸어요".to_string());
                    }
                    Err(error) => self.set_toast(format!("그림을 못 되돌렸어요: {error}")),
                }
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
                self.close_settings_inline();
                Ok(!self.settings_room_active())
            }

            // ── First install ───────────────────────────────────────────
            "terminal-profile-import" => {
                let applied = crate::onboarding::apply_terminal_profile(id)?;
                if let Some(size) = applied.font_size {
                    self.font_size = size.clamp(9.0, 32.0);
                }
                theme::apply_from_settings();
                self.apply_effective_scale();
                self.begin_theme_fx();
                self.repaint_all();
                if applied.restart_required {
                    self.set_toast("재시작하면 폰트까지 적용돼요".to_string());
                }
                Ok(true)
            }
            "font-family" => {
                crate::onboarding::apply_font_family(id)?;
                self.set_toast("재시작하면 폰트가 적용돼요".to_string());
                Ok(true)
            }
            "font-path" => {
                crate::onboarding::apply_font_path(&arg)?;
                self.set_toast("재시작하면 폰트가 적용돼요".to_string());
                Ok(true)
            }
            "default-shell" => {
                self.set_shell = crate::onboarding::apply_default_shell(id)?;
                Ok(true)
            }
            "complete-onboarding" => {
                let provider = (!id.is_empty()).then_some(id);
                if let Some(provider) = provider {
                    match onboarding_provider_logged_in(provider) {
                        Some(true) => {}
                        None => return Err("로그인 상태를 확인하고 있어요".to_string()),
                        Some(false) => return Err("먼저 로그인해 주세요".to_string()),
                    }
                }
                crate::onboarding::complete(provider)?;
                Ok(true)
            }
            "skip-onboarding" => {
                crate::onboarding::skip()?;
                Ok(true)
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
                let s = crate::cursor::CursorShape::from_str(id).ok_or_else(|| unknown(id))?;
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
            // 값을 여기서 한 번 더 거르는 건 이 경로가 **HTTP** 라서다 — 화면이
            // 보내는 것만 오리라고 믿으면 안 되고, 범위 밖 높이는 띠를 깨뜨린다.
            "status-bar-h" => {
                let px: u32 = id.parse().map_err(|_| unknown(id))?;
                let f = px as f32;
                if !(socket::STATUS_H_MIN..=socket::STATUS_H_MAX).contains(&f) {
                    return Err(unknown(id));
                }
                self.settings_apply(SettingsAction::StatusBarH(px));
                Ok(self.set_status_h.round() as u32 == px)
            }
            "pane-footer-h" => {
                let px: u32 = id.parse().map_err(|_| unknown(id))?;
                let f = px as f32;
                if !(socket::PANE_FOOTER_H_MIN..=socket::PANE_FOOTER_H_MAX).contains(&f) {
                    return Err(unknown(id));
                }
                self.settings_apply(SettingsAction::PaneFooterH(px));
                Ok(self.set_pane_footer_h.round() as u32 == px)
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
                    "system".to_string()
                } else {
                    theme_key_or_reject(id)?
                };
                self.settings_apply(SettingsAction::ThemeMode(key.clone()));
                Ok(theme::theme_name() == key)
            }
            // system 모드의 밝기 슬롯에 테마 배정 — id 는 프리셋 키 또는
            // `custom:<slug>`. "system" 자신은 못 들어간다(자기참조). 지금 system
            // 으로 보는 중이면 그 자리에서 다시 해석해 갈아입는다.
            "theme-system-light" | "theme-system-dark" => {
                let light = action == "theme-system-light";
                let key = theme_key_or_reject(id)?;
                socket::write_setting(
                    if light { "theme_system_light" } else { "theme_system_dark" },
                    serde_json::Value::String(key.clone()),
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
                Ok(theme::active_custom_slug().is_some())
            }
            // 커스텀 팔레트 하나 치우기 — id 는 slug.
            "delete-custom-theme" => {
                let s = socket::read_settings();
                if !theme::custom_themes(&s).iter().any(|e| theme::custom_slug(e) == id) {
                    return Err(reject(
                        "custom_theme_absent",
                        "그 커스텀 팔레트가 없어요".to_string(),
                    ));
                }
                self.settings_apply(SettingsAction::DeleteCustomTheme(id.to_string()));
                Ok(!theme::custom_themes(&socket::read_settings())
                    .iter()
                    .any(|e| theme::custom_slug(e) == id))
            }
            // 이름 바꾸기 — id 는 slug, 새 이름은 label. 이름만 바뀌므로 팔레트를
            // 다시 적용할 것이 없다(색은 그대로다).
            "rename-custom-theme" => {
                rename_custom_theme(id, &arg)?;
                self.chrome_dirty = true;
                Ok(true)
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
            // 화면 어디서나 색 집기. 스포이드를 띄우기만 하고 끝난다 — 사용자가
            // 클릭하는 시점을 여기서 기다릴 수 없어(GUI 스레드를 잡으면 앱이 멈춘다),
            // 집힌 색은 GUI 틱이 받아 칸에 넣는다.
            "palette-eyedropper" => {
                let i: usize = id.parse().map_err(|_| unknown(id))?;
                if i >= theme::PALETTE_KEYS.len() + 16 {
                    return Err(reject("palette_slot_missing", "없는 색 칸이에요".to_string()));
                }
                if !crate::eyedropper::supported() {
                    return Err(reject(
                        "eyedropper_unsupported",
                        "이 운영체제에선 화면 집기를 아직 못 써요".to_string(),
                    ));
                }
                crate::eyedropper::pick_screen_color(i);
                Ok(true)
            }
            // 색 패널을 여는 동안 오는 미리보기. 화면만 갈고 파일은 안 건드리므로
            // 굳히려면 손을 뗄 때 `palette-hex` 가 한 번 더 와야 한다 — 화면이 그
            // 짝을 지킨다(blur 에서 커밋).
            "palette-preview" => {
                let i: usize = id.parse().map_err(|_| unknown(id))?;
                if i >= theme::PALETTE_KEYS.len() + 16 {
                    return Err(reject("palette_slot_missing", "없는 색 칸이에요".to_string()));
                }
                let Some(c) = theme::parse_hex(&arg) else {
                    return Err(reject("hex_invalid", "#rrggbb 꼴로 적어 주세요".to_string()));
                };
                self.preview_palette_edit(i, c);
                Ok(true)
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
                let confirm = self.request_web_account_switch(
                    crate::session::AccountSwitchProvider::Claude,
                    id,
                );
                let awaiting = confirm.is_some();
                if let Some(confirm) = confirm {
                    put_web_code("confirm", confirm);
                }
                Ok(awaiting || self.set_claude_account == id)
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
                let confirm = self.request_web_account_switch(
                    crate::session::AccountSwitchProvider::Codex,
                    id,
                );
                let awaiting = confirm.is_some();
                if let Some(confirm) = confirm {
                    put_web_code("confirm", confirm);
                }
                Ok(awaiting || self.set_codex_account == id)
            }
            "confirm-account-switch" | "cancel-account-switch" => {
                let Some((provider_key, nonce)) = arg.split_once(':') else {
                    return Err(reject(
                        "account_confirm_invalid",
                        "계정 전환 확인값이 올바르지 않아요".to_string(),
                    ));
                };
                let provider = match provider_key {
                    "claude" => crate::session::AccountSwitchProvider::Claude,
                    "codex" => crate::session::AccountSwitchProvider::Codex,
                    other => return Err(unknown(other)),
                };
                if let Err(error) = self.resolve_web_account_switch(
                    provider,
                    id,
                    nonce,
                    action == "confirm-account-switch",
                ) {
                    if let Some(confirm) = self.current_web_account_confirmation() {
                        put_web_code("confirm", confirm);
                    }
                    return Err(error);
                }
                Ok(true)
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
            // 이름 없는 쪽이 기본 = **쓰던 브라우저**고, `-isolated` 가 쿠키 없는 창이다.
            "reauth-account" | "reauth-account-isolated" => {
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
                let browser = if action.ends_with("-isolated") {
                    LoginBrowser::Isolated
                } else {
                    LoginBrowser::Default
                };
                self.settings_apply(SettingsAction::ReauthAccount(
                    provider,
                    id.to_string(),
                    browser,
                ));
                Ok(true)
            }
            // 원격 설정창이 보내는 OAuth 코드. 이 기계에서 도는 로그인 CLI 의
            // stdin 으로 그대로 흘러간다.
            "login-code" => {
                let code = id.trim();
                if code.is_empty() {
                    return Err(reject("login_code_empty", "코드가 비어 있어요".to_string()));
                }
                if !submit_login_code(code) {
                    return Err(reject(
                        "login_not_running",
                        "코드를 기다리는 로그인이 없어요".to_string(),
                    ));
                }
                Ok(true)
            }
            "cancel-login" => {
                cancel_hidden_login();
                self.login_code_edit.clear();
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
            // ── 캐릭터 생성 ───────────────────────────────────────────────
            "theme-gen-provider" => {
                let k = pick(&["opengateway", "codex", "nanobanana"], id)
                    .ok_or_else(|| unknown(id))?;
                self.settings_apply(SettingsAction::ThemeGenProvider(k.to_string()));
                Ok(socket::read_settings().get("theme_gen_provider").and_then(|v| v.as_str())
                    == Some(k))
            }
            "theme-gen-start" => {
                let slug = id.trim();
                if slug.is_empty() {
                    return Err(reject("slug_empty", "누구를 구울지 골라 주세요".to_string()));
                }
                // 이미 도는 잡을 「눌렸다」고 답하면 안 된다 — 화면은 새로 시작된 줄
                // 알고 진행을 처음부터 다시 그린다.
                if self.themegen_view(slug).is_some_and(|v| {
                    !matches!(v.phase, themegen::GenPhase::Done | themegen::GenPhase::Failed)
                }) {
                    return Err(reject("themegen_busy", "이미 굽는 중이에요".to_string()));
                }
                let theme_id = socket::read_character_theme();
                // 참조 그림이 있어야 굽는다. 없을 때 조용히 실패하면 화면은 눌리긴
                // 눌렀는데 아무 일도 안 나는 상태로 남는다.
                let Some((path, _)) = themegen_ref_info(&theme_id, slug) else {
                    return Err(reject(
                        "themegen_ref_missing",
                        "참조 그림을 먼저 넣어 주세요".to_string(),
                    ));
                };
                self.themegen_start(&theme_id, slug, &path, None);
                Ok(self.themegen_view(slug).is_some())
            }
            "gemini-key" => {
                // 정본은 settings.json 이고 네이티브도 blur 때 거기 굳힌다 — 버퍼를
                // 함께 맞춰 둬야 설정 창을 네이티브로 열었을 때 옛 값이 안 보인다.
                self.themegen.key_edit = arg.clone();
                socket::write_setting("gemini_api_key", serde_json::Value::String(arg.clone()));
                Ok(socket::read_settings().get("gemini_api_key").and_then(|v| v.as_str())
                    == Some(arg.as_str()))
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

        // 테마 카드 한 장의 미리보기 재료. `pal` 이 없으면 지금 화면에 적용된
        // 색으로 그린다.
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
        // 커스텀들은 **각자의 팔레트로** 그린다 — 라이브 색으로 그리면 지금 입은 한
        // 벌만 제 색이고 나머지가 전부 같은 카드로 보인다.
        let customs = theme::custom_themes(&s);
        themes.extend(customs.iter().map(|e| {
            let p = theme::custom_palette(e);
            card(
                &format!("custom:{}", theme::custom_slug(e)),
                theme::custom_label(e),
                Some(&p),
            )
        }));

        // 계정 한 행. "기본" 행(슬롯 아님, `slot: false`)은 **계정을 아직 안
        // 골랐을 때만** — 작업대 시대에는 기본 자리가 곧 활성 계정의 작업대라,
        // 슬롯이 활성인 동안 이 행을 주면 같은 로그인이 두 줄로 떠 계정이 하나
        // 더 있는 것처럼 읽힌다(2026-08-17 「왜 다섯개로 떠」 — 네이티브 카드
        // 목록·상태바 서브메뉴와 같은 규칙).
        // 계정별 한도 — 하단바가 쓰는 우물(표: 폴러가 채움, 활성: 활성 게이지)
        // 그대로. 설정을 열 때 따로 묻지 않아 즉시 뜬다(2026-08-31 지적 「하단바랑
        // 다르게 사용량 바로 안 뜨고」). 값이 없는 슬롯은 null — 0% 로 그리면
        // 여유 있다는 거짓말이 된다(하단바와 같은 규칙).
        let usage_table =
            self.claude_usage_all.lock().ok().map(|g| g.clone()).unwrap_or_default();
        let active_usage = self.claude_usage.lock().ok().and_then(|g| g.clone());
        let active_acct = self.set_claude_account.clone();
        let claude_rows: Vec<serde_json::Value> = self
            .set_claude_account
            .is_empty()
            .then(|| (String::new(), String::new(), None))
            .into_iter()
            .chain(
                self.set_claude_accounts
                    .iter()
                    .enumerate()
                    .map(|(i, a)| (a.id.clone(), a.label.clone(), Some(i))),
            )
            .map(|(id, label, idx)| {
                let probe = auth_probe(&id);
                let usage = (!id.is_empty())
                    .then(|| {
                        let dir = crate::claude_auth::runtime_dir_for_cached(&id, &active_acct)
                            .map_or(String::new(), |p| p.to_string_lossy().into_owned());
                        usage_table.get(&dir).cloned().or_else(|| {
                            // 활성 계정은 활성 게이지와 같은 원천 — 계정 전환 직후
                            // 옛 값을 새 계정 것으로 보이지 않게 하는 account_dir
                            // 대조까지 하단바와 같은 규칙이다.
                            active_usage
                                .clone()
                                .filter(|b| id == active_acct && b.account_dir == dir)
                        })
                    })
                    .flatten();
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
                    "usage": usage.as_ref().map(|b| b.pct),
                    "usage_stale": usage.as_ref().map(|b| b.stale),
                    "usage_label": usage.as_ref().map(|b| b.label.clone()),
                    "usage_resets": usage.as_ref().and_then(|b| crate::resets_in_label(b.resets_at)),
                    "logged_in": probe.as_ref().map(|p| p.logged_in),
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
                let logged_in = codex_logged_in(&id);
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
                    "logged_in": logged_in,
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
                "cursor_shape": self.cursor_shape.as_str(),
                "cursor_thickness": self.cursor_thickness,
                "mouse_cursor": self.mouse_cursor,
                "wheel_gain_x100": (self.set_wheel_pixel_gain * 100.0).round() as u32,
                "status_bar_h": self.set_status_h.round() as u32,
                "pane_footer_h": self.set_pane_footer_h.round() as u32,
            },
            "appearance": {
                "theme": theme::theme_name(),
                "themes": themes,
                // system 모드가 밝기별로 입을 테마(프리셋 키 또는 `custom:<slug>`) —
                // OS 는 밝기만 알려 주고 팔레트는 사용자가 배정한다(2026-08-15).
                "theme_system_light": theme::system_slot_theme(true),
                "theme_system_dark": theme::system_slot_theme(false),
                // 카드는 위 `themes` 에 이미 섞여 있다. 이 목록은 이름 바꾸기·치우기
                // 처럼 커스텀에만 있는 조작을 그리는 데 쓴다.
                "custom_themes": customs
                    .iter()
                    .map(|e| {
                        let slug = theme::custom_slug(e);
                        serde_json::json!({
                            "key": format!("custom:{slug}"),
                            "slug": slug,
                            "label": theme::custom_label(e),
                        })
                    })
                    .collect::<Vec<_>>(),
                "custom_active": theme::active_custom_slug().unwrap_or_default(),
                "eyedropper": crate::eyedropper::supported(),
                "palette_keys": theme::PALETTE_KEYS.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
                "palette_hex": palette_hex_list(&s, theme::active_custom_slug().as_deref()),
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
                "accounts": claude_rows,
                "account": self.set_claude_account,
                "codex_accounts": codex_rows,
                "codex_account": self.set_codex_account,
                "autoswitch": self.set_account_autoswitch,
                "autoswitch_pct": self.set_account_autoswitch_pct.round() as u32,
                // 진행 중인 로그인. 다른 기계의 설정창이 이걸 보고 「코드를
                // 기다리는 중」을 그리고, 그 코드를 `login-code` 로 돌려보낸다.
                "login": hidden_login_job().map(|job| serde_json::json!({
                    "id": job.id,
                    "state": match job.state {
                        LoginState::Running => "running",
                        LoginState::NeedCode => "need_code",
                        LoginState::Ok => "ok",
                        LoginState::Err(_) => "error",
                    },
                    "error": match job.state {
                        LoginState::Err(ref why) => Some(why.clone()),
                        _ => None,
                    },
                })),
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

    /// 그 캐릭터의 상세 화면을 연다 — 편집 중이던 것은 먼저 저장하고, 원본
    /// persona·이름을 버퍼로 로드한다. 카드 클릭과 별도창 딥링크(프사 클릭 →
    /// Theme 페이지 + 그 캐릭터)가 공유한다.
    ///
    /// 포커스는 **주지 않는다**. 상세는 목록을 통째로 덮는 화면이라, 열자마자
    /// 커서가 성격 칸에 있으면 뒤로 가려고 누른 키가 본문에 박힌다.
    pub(crate) fn select_student_for_edit(&mut self, name: String) {
        self.select_student_for_edit_in_theme(socket::read_character_theme(), name);
    }

    pub(crate) fn select_student_for_edit_in_theme(&mut self, theme_id: String, name: String) {
        self.flush_student_persona();
        self.flush_student_name();
        let theme_id = if theme_id == kasa_mcp::character::BASE_THEME_KEY {
            String::new()
        } else {
            theme_id
        };
        let roster = student_roster_for_theme(&theme_id);
        let def = roster
            .as_ref()
            .and_then(|value| kasa_mcp::character::member_def(value, &name));
        let persona = def
            .as_ref()
            .and_then(|value| value.get("persona"))
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string();
        self.students_caret = persona.chars().count();
        self.students_persona = persona;
        self.students_theme = theme_id;
        self.students_slug = def
            .as_ref()
            .and_then(|value| value.get("slug"))
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| theme::agent_slug(&name));
        self.students_model = def
            .as_ref()
            .and_then(|value| value.get("model"))
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string();
        self.students_backend = def
            .as_ref()
            .and_then(|value| value.get("backend"))
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string();
        self.students_name = name.clone();
        self.students_selected = Some(name);
        self.settings_input = None;
        self.refresh_native_settings_media_cache();
    }

    /// 상세를 닫고 목록으로. 편집 중이던 것은 여기서 굳힌다(persona 를 **먼저** —
    /// 저장 키가 이름이라, 이름부터 바꾸면 옛 이름 자리에 쓰려다 못 찾는다).
    pub(crate) fn close_student_edit(&mut self) {
        self.flush_student_persona();
        self.flush_student_name();
        self.students_selected = None;
        self.students_persona.clear();
        self.students_name.clear();
        self.students_slug.clear();
        self.students_model.clear();
        self.students_backend.clear();
        self.settings_input = None;
        self.refresh_native_settings_media_cache();
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
        } else if student_roster_for_theme(&self.students_theme)
            .as_ref()
            .is_some_and(|roster| {
                kasa_mcp::character::member_names(roster)
                    .iter()
                    .any(|name| name == &new && name != &old)
            })
        {
            Some(format!("{new} 은(는) 이미 있어요"))
        } else if kasa_mcp::character::update_member_in_theme(
            &self.students_theme,
            &old,
            "name",
            serde_json::Value::String(new.clone()),
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
        self.regen_pane_shims();
        self.repaint_all();
    }

    /// 팔레트 칸 `i` 의 지금 유효값(#rrggbb) — custom_theme 에 적힌 값, 없으면
    /// base 프리셋 값. 포커스 시드·리스트 표시가 같은 곳을 읽어야 「눌렀더니
    /// 다른 값이 뜨는」 어긋남이 없다.
    fn palette_hex_at(&self, i: usize) -> String {
        palette_hex_list(&socket::read_settings(), theme::active_custom_slug().as_deref())
            .into_iter()
            .nth(i)
            .unwrap_or_else(|| "#000000".to_string())
    }

    /// 색 선택기의 한 픽. rect 밖 커서는 클램프해 가장자리 값으로 잇는다: 드래그
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

    /// 드래그 중 색 미리보기. `picker_pick`과 HSV 계산은 같지만 파일에는 쓰지
    /// 않고, 손을 놓을 때 네이티브 설정이 `apply_palette_edit`을 한 번 부른다.
    pub(crate) fn picker_preview(
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
            SettingsAction::PickerHue => ((rx * 360.0).min(359.9), s, v),
            _ => (h, rx, 1.0 - ry),
        };
        let (h, s, v) = self.set_picker_hsv;
        let rgb = hsv_to_rgb(h, s, v);
        self.set_palette_edit = theme::hex_str(rgb);
        self.settings_caret = self.set_palette_edit.chars().count();
        self.preview_palette_edit(i, rgb);
        self.chrome_dirty = true;
    }

    /// 팔레트 칸 `i` 를 `c` 로 바꾼 커스텀 목록과 그중 편집 대상 인덱스, 그리고
    /// 편집 전에 목록이 비어 **새로 만들어야 했는지**.
    ///
    /// 저장(`apply_palette_edit`)과 미리보기(`preview_palette_edit`)가 같은 조립을
    /// 쓰라고 뽑았다 — 둘이 갈리면 색을 고르는 동안 보이는 색과 손을 뗀 뒤 굳는
    /// 색이 달라진다.
    fn palette_edited_list(&self, i: usize, c: [u8; 3]) -> (Vec<serde_json::Value>, usize, bool) {
        let s = socket::read_settings();
        // 편집 대상은 지금 입고 있는 커스텀. 아직 하나도 없으면(설정 파일을 손으로
        // 비운 경우) 새로 하나 만들어 그것을 고친다 — 색을 골랐는데 아무 데도
        // 안 적히는 편이 더 나쁘다.
        let want = theme::active_custom_slug().unwrap_or_default();
        let mut list = theme::custom_themes(&s);
        let seeded = list.is_empty();
        if seeded {
            list.push(theme::clone_current_custom(&s));
        }
        let idx = list
            .iter()
            .position(|e| !want.is_empty() && theme::custom_slug(e) == want)
            .unwrap_or(0);
        let mut obj = match list[idx].as_object() {
            Some(m) => m.clone(),
            None => serde_json::Map::new(),
        };
        let hex = theme::hex_str(c);
        let n = theme::PALETTE_KEYS.len();
        if i < n {
            obj.insert(theme::PALETTE_KEYS[i].0.to_string(), serde_json::Value::String(hex));
        } else {
            // ansi 배열이 없거나 짧을 수 있다 — 지금 유효값으로 16칸을 다 채운
            // 뒤 한 칸만 바꾼다. 부분 배열을 그대로 두면 인덱스가 어긋난다.
            let cur = palette_hex_list(&s, Some(&theme::custom_slug(&list[idx])));
            let mut arr: Vec<serde_json::Value> = (0..16)
                .map(|k| serde_json::Value::String(cur[n + k].clone()))
                .collect();
            let j = (i - n).min(15);
            arr[j] = serde_json::Value::String(hex);
            obj.insert("ansi".to_string(), serde_json::Value::Array(arr));
        }
        list[idx] = serde_json::Value::Object(obj);
        (list, idx, seeded)
    }

    /// 피커 핸들을 이 색에 맞춘다. 타이핑으로 들어온 새 색이면 따라오고, 피커
    /// 픽이면 지금 HSV 가 이미 이 색이라(변환 일치) 건드리지 않는다 — 무조건
    /// 역산하면 s=0·v=0 을 지날 때마다 색상(H)이 0 으로 튄다.
    fn sync_picker_hsv(&mut self, c: [u8; 3]) {
        let (h, s, v) = self.set_picker_hsv;
        if hsv_to_rgb(h, s, v) != c {
            self.set_picker_hsv = rgb_to_hsv(c);
        }
    }

    /// 팔레트 hex 버퍼를 검증해 **지금 입고 있는 커스텀**에 반영하고 즉시 다시
    /// 칠한다. 6자리 hex 가 아직 아니면(타이핑 중) 아무것도 안 한다 — 반쯤 친
    /// 값으로 화면이 튀는 것보다 완성되는 순간에만 따라오는 쪽이 읽기 좋다.
    pub(crate) fn apply_palette_edit(&mut self, i: usize) {
        let Some(c) = theme::parse_hex(&self.set_palette_edit) else { return };
        self.sync_picker_hsv(c);
        let (list, idx, _) = self.palette_edited_list(i, c);
        let key = format!("custom:{}", theme::custom_slug(&list[idx]));
        write_custom_themes(list);
        theme::set_theme(&key);
        self.settings_scene.refresh_palette_cache();
        self.repaint_all();
    }

    /// 파일에 굳히지 않고 **화면 색만** 바꾼다 — OS 색 패널의 휠을 돌리는 동안
    /// 매 움직임이 이리로 오는데, 그때마다 settings.json 과 claude 설정까지 쓰면
    /// 파일 쓰기가 폭주한다. 손을 뗄 때 `palette-hex` 가 같은 값을 굳힌다.
    ///
    /// 아직 커스텀이 하나도 없으면 저장 경로로 넘긴다: 미리보기는 목록에 없는
    /// slug 를 「지금 입은 테마」로 세우게 되고, 그 뒤 팔레트를 다시 읽는 경로
    /// (OS 밝기 폴링 등)가 그것을 사라진 테마로 보고 기본색으로 되돌린다.
    pub(crate) fn preview_palette_edit(&mut self, i: usize, c: [u8; 3]) {
        let (list, idx, seeded) = self.palette_edited_list(i, c);
        if seeded {
            self.set_palette_edit = theme::hex_str(c);
            self.apply_palette_edit(i);
            return;
        }
        self.sync_picker_hsv(c);
        let slug = theme::custom_slug(&list[idx]);
        theme::preview_custom_theme(&serde_json::json!({ "custom_themes": list }), &slug);
        self.repaint_all();
    }

    /// 스포이드가 집어 둔 색이 있으면 그 칸에 굳힌다. GUI 틱이 부른다 — 시스템
    /// 콜백은 `App` 을 못 들고 오므로 색만 통에 놓고 가고, 꺼내는 건 여기다.
    pub(crate) fn pump_eyedropper(&mut self) {
        let Some((slot, rgb)) = crate::eyedropper::take_picked() else { return };
        self.set_palette_edit = theme::hex_str(rgb);
        self.apply_palette_edit(slot);
    }

    /// persona 편집 버퍼를 characters.json 에 저장(선택 캐릭터가 있고 실제로
    /// 바뀌었을 때만). blur·선택 변경·설정 닫기 시 호출. 저장 후 shim 을 재생성해
    /// 그 캐릭터 pane 의 다음 claude 실행이 새 persona 를 집게 한다.
    /// 「원본」 버퍼를 저장된 정의로 다시 채운다(뷰를 열 때·형식을 바꿀 때).
    pub(crate) fn reload_student_raw(&mut self) {
        // 폼에서 고치던 성격이 아직 파일에 안 갔을 수 있다 — 먼저 굳히지 않으면
        // 원본 뷰가 옛 성격을 보여 주고, 그걸 저장하는 순간 방금 친 글이 날아간다.
        self.flush_student_persona();
        let Some(name) = self.students_selected.clone() else { return };
        let def = student_roster_for_theme(&self.students_theme)
            .and_then(|c| kasa_mcp::character::member_def(&c, &name));
        self.students_raw.text = match def {
            Some(d) if self.students_raw.yaml => kasa_mcp::character::member_to_yaml(&d),
            Some(d) => serde_json::to_string_pretty(&d).unwrap_or_default(),
            None => String::new(),
        };
        self.students_raw.caret = 0;
        self.students_raw.err = None;
    }

    /// 「원본」 버퍼를 로스터에 굳힌다. 형식이 틀리면 저장하지 않고 이유를 남긴다 —
    /// 반쯤 읽어 저장하면 적지 않은 필드가 통째로 사라진다.
    pub(crate) fn save_student_raw(&mut self) {
        let Some(name) = self.students_selected.clone() else { return };
        let parsed = if self.students_raw.yaml {
            kasa_mcp::character::member_from_yaml(&self.students_raw.text)
        } else {
            serde_json::from_str::<serde_json::Value>(&self.students_raw.text)
                .map_err(|e| e.to_string())
        };
        let def = match parsed {
            Ok(d) => d,
            Err(e) => {
                self.students_raw.err = Some(e);
                return;
            }
        };
        if let Err(e) =
            kasa_mcp::character::replace_member_in_theme(&self.students_theme, &name, &def)
        {
            self.students_raw.err = Some(e.to_string());
            return;
        }
        // 원본에서 이름을 고쳤으면 선택도 따라가야 한다 — 안 그러면 옛 이름으로
        // 조회해 상세가 빈 화면이 된다.
        if let Some(n) = def.get("name").and_then(|x| x.as_str()) {
            self.students_selected = Some(n.to_string());
            self.students_name = n.to_string();
        }
        self.students_slug = def
            .get("slug")
            .and_then(|value| value.as_str())
            .unwrap_or(&self.students_slug)
            .to_string();
        self.students_model = def
            .get("model")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string();
        self.students_backend = def
            .get("backend")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string();
        // 폼 버퍼도 새 정의로 맞춘다. 안 맞추면 폼으로 돌아가 편집기를 벗어나는
        // 순간 옛 성격이 다시 저장돼, 원본에서 고친 것이 조용히 되돌아간다.
        self.students_persona =
            def.get("persona").and_then(|x| x.as_str()).unwrap_or_default().to_string();
        self.students_caret = 0;
        self.students_raw.err = None;
        self.regen_pane_shims();
        self.refresh_native_settings_media_cache();
        self.set_toast("원본을 저장했어요".to_string());
    }

    pub(crate) fn flush_student_persona(&mut self) {
        let Some(name) = self.students_selected.clone() else { return };
        let cur = student_roster_for_theme(&self.students_theme)
            .and_then(|c| kasa_mcp::character::raw_persona_for(&c, &name))
            .unwrap_or_default();
        if cur == self.students_persona {
            return;
        }
        let _ = kasa_mcp::character::update_member_in_theme(
            &self.students_theme,
            &name,
            "persona",
            serde_json::Value::String(self.students_persona.clone()),
        );
        self.regen_pane_shims();
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
        match std::fs::write(&path, &doc) {
            Ok(()) => {
                self.feedback_body.clear();
                self.feedback_caret = 0;
                self.settings_input = None;
                socket::write_setting("feedback_draft", serde_json::Value::String(String::new()));
                self.set_toast(if post_feedback_to_nacho(&doc) {
                    "피드백을 저장하고 나쵸에게 보내는 중이에요".to_string()
                } else {
                    "피드백을 저장했어요".to_string()
                });
            }
            Err(e) => self.set_toast(format!("저장 실패: {e}")),
        }
    }
}

/// 방금 굳힌 제보를 나쵸네코에게 넘긴다. 호스트 설정이 비어 있으면 아무 일도 하지
/// 않고 `false` 를 낸다 — **기본은 「안 보냄」**이다.
///
/// 넘기는 곳은 그 기계의 `nacho-tell` 인박스다(`echo <본문> | ssh <host>
/// 'python3 ~/nacho-neko/bin/nacho-tell.py <이름>'`). 나쵸가 몇 초 안에 집어 가고,
/// **같은 대화가 거노의 디스코드 DM 스레드에도 남는다** — 앱이 디스코드로 직접
/// 보내려면 봇 토큰이 있어야 하는데 그건 설정 파일에 평문으로 둘 것이 못 되고,
/// DM 채널에는 webhook 도 못 만든다.
///
/// 보내기는 백그라운드다. ssh 왕복 동안 GUI 가 멈추면 버튼을 누른 손이 먼저
/// 눈치챈다. 그래서 토스트도 「보냈다」가 아니라 「보내는 중」이다 — 결과를 안
/// 기다리고 하는 말이라 단정하면 거짓이 된다. 실패해도 파일은 이미 디스크에 있어
/// 제보 자체는 안 잃는다.
fn post_feedback_to_nacho(doc: &str) -> bool {
    let host = socket::read_feedback_nacho_host();
    if host.is_empty() {
        return false;
    }
    let text = doc.to_string();
    std::thread::spawn(move || {
        use std::io::Write;
        use std::process::Stdio;
        // GUI 프로세스의 PATH 는 로그인 셸의 것이 아니다 — 절대경로로 부른다.
        let spawned = crate::proc::command("/usr/bin/ssh")
            .arg("-o")
            .arg("ConnectTimeout=10")
            .arg("-o")
            .arg("BatchMode=yes")
            .arg(&host)
            .arg("python3 ~/nacho-neko/bin/nacho-tell.py 카사텀")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(e) => {
                eprintln!("[feedback] ssh 를 띄우지 못했다: {e}");
                return;
            }
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        match child.wait_with_output() {
            Ok(out) if out.status.success() => {}
            Ok(out) => eprintln!(
                "[feedback] 나쵸 전달 실패: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
            Err(e) => eprintln!("[feedback] ssh 를 기다리지 못했다: {e}"),
        }
    });
    true
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

/// `None` means an installed Claude CLI is still answering its async probe.
pub(crate) fn onboarding_provider_logged_in(provider: &str) -> Option<bool> {
    match provider {
        "claude" => {
            let mut ids = vec![String::new()];
            ids.extend(socket::read_claude_accounts().into_iter().map(|a| a.id));
            let probes: Vec<Option<AuthProbe>> = ids.iter().map(|id| auth_probe(id)).collect();
            if probes.iter().flatten().any(|p| p.logged_in) {
                Some(true)
            } else if crate::onboarding::command_available("claude") && probes.iter().any(Option::is_none) {
                None
            } else {
                Some(false)
            }
        }
        "codex" => {
            let mut ids = vec![String::new()];
            ids.extend(socket::read_codex_accounts().into_iter().map(|a| a.id));
            Some(ids.iter().any(|id| codex_logged_in(id)))
        }
        _ => Some(false),
    }
}

fn onboarding_auth_provider(provider: &str) -> serde_json::Value {
    match provider {
        "claude" => {
            let mut ids = vec![String::new()];
            ids.extend(socket::read_claude_accounts().into_iter().map(|a| a.id));
            let probes: Vec<Option<AuthProbe>> = ids.iter().map(|id| auth_probe(id)).collect();
            let logged = probes.iter().flatten().find(|p| p.logged_in);
            let installed = crate::onboarding::command_available("claude");
            let status = if logged.is_some() { "logged_in" } else if installed && probes.iter().any(Option::is_none) { "checking" } else if installed { "logged_out" } else { "not_installed" };
            let account = logged.and_then(|p| (!p.email.is_empty()).then_some(p.email.clone()));
            let detail = logged.and_then(|p| team_org(&p.email, &p.org));
            serde_json::json!({ "status": status, "account": account, "detail": detail })
        }
        "codex" => {
            let mut ids = vec![String::new()];
            ids.extend(socket::read_codex_accounts().into_iter().map(|a| a.id));
            let logged_id = ids.iter().find(|id| codex_logged_in(id));
            let installed = crate::onboarding::command_available("codex");
            let status = if logged_id.is_some() { "logged_in" } else if installed { "logged_out" } else { "not_installed" };
            let account = logged_id.and_then(|id| codex_identity(id));
            serde_json::json!({ "status": status, "account": account, "detail": null })
        }
        _ => serde_json::json!({ "status": "not_installed", "account": null, "detail": null }),
    }
}

/// `GET /onboarding/state` payload. Tokens and credential paths never enter it.
pub(crate) fn onboarding_state_json() -> serde_json::Value {
    let claude = onboarding_auth_provider("claude");
    let codex = onboarding_auth_provider("codex");
    let mut state = crate::onboarding::base_state_json();
    if let Some(root) = state.as_object_mut() {
        let stored = root.get("preferred_agent").and_then(|v| v.as_str()).map(str::to_string);
        let detected = stored.or_else(|| {
            (claude.get("status").and_then(|v| v.as_str()) == Some("logged_in"))
                .then_some("claude".to_string())
                .or_else(|| (codex.get("status").and_then(|v| v.as_str()) == Some("logged_in")).then_some("codex".to_string()))
        });
        root.insert("preferred_agent".to_string(), serde_json::json!(detected));
        root.insert("auth".to_string(), serde_json::json!({ "claude": claude, "codex": codex }));
    }
    state
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
/// 웹에서 온 테마 id 를 실재하는 키로 굳힌다 — 프리셋 키이거나 `custom:<slug>`.
/// 옛 `"custom"` 도 받는다(첫 커스텀). 없는 것을 고르면 화면만 안 바뀌고 이유를
/// 모르므로, 여기서 이름 붙은 거절로 돌려보낸다.
fn theme_key_or_reject(id: &str) -> Result<String, String> {
    if let Some(slug) = theme::custom_key(id) {
        let list = theme::custom_themes(&socket::read_settings());
        // 빈 slug(옛 `"custom"`)만 첫 항목으로 받아 준다. `find_custom` 의 폴백을
        // 여기까지 들이면 사라진 팔레트를 골랐을 때 **다른 팔레트가 입혀지고
        // 성공이라 답한다** — 창을 둘 띄워 한쪽에서 지우면 실제로 일어난다.
        let found = if slug.is_empty() {
            list.first()
        } else {
            list.iter().find(|e| theme::custom_slug(e) == slug)
        };
        return match found {
            Some(e) => Ok(format!("custom:{}", theme::custom_slug(e))),
            None => Err(reject(
                "custom_theme_absent",
                "그 커스텀 팔레트가 없어요".to_string(),
            )),
        };
    }
    theme::THEME_PRESETS
        .iter()
        .find(|(k, _, _)| *k == id)
        .map(|(k, _, _)| (*k).to_string())
        .ok_or_else(|| {
            reject_with(
                "theme_missing",
                serde_json::json!({ "theme": id }),
                format!("'{id}' 테마가 없어요"),
            )
        })
}

pub(crate) fn reject(code: &'static str, msg: String) -> String {
    put_web_code("error_code", serde_json::Value::String(code.to_string()));
    msg
}

/// 인자가 붙는 거부. 자리 인자가 아니라 **이름 붙인 객체**로 넘긴다 — 영어는 어순이
/// 달라 자리로 맞추면 문장이 어긋난다.
fn reject_with(code: &'static str, args: serde_json::Value, msg: String) -> String {
    put_web_code("error_args", args);
    reject(code, msg)
}

/// 같은 것을 이 모듈 밖에서. 거부 문구를 만드는 자리가 settings.rs 하나가 아니게
/// 되면서(캐릭터 고르기는 session.rs) 통로가 필요해졌다.
pub(crate) fn reject_with_args(
    code: &'static str,
    args: serde_json::Value,
    msg: String,
) -> String {
    reject_with(code, args, msg)
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
        "쓰던 브라우저에서 승인하세요 — 지금 로그인된 계정으로 붙어요" => "login_in_default_browser",
        "쓰던 브라우저에서 로그인하세요 — 다른 계정이면 브라우저에서 먼저 계정을 바꾸세요" => {
            "login_new_slot_in_default_browser"
        }
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
        if k.starts_with("error") || k == "confirm" {
            obj.insert(k, v);
        }
    }
    serde_json::Value::Object(obj)
}

/// 캐릭터의 참조 그림 자리 — `themes/<id>/gen/<slug>/ref.png`. 번들(id 빈 문자열)은
/// 폴더가 없어 생성 대상이 아니다. 이 규약은 themegen(전처리·굽기)과 공유한다.
fn themegen_ref_path(theme_id: &str, slug: &str) -> Option<std::path::PathBuf> {
    if theme_id.is_empty() || slug.is_empty() {
        return None;
    }
    Some(
        kasa_mcp::character::themes_root()?
            .join(theme_id)
            .join("gen")
            .join(slug)
            .join("ref.png"),
    )
}

fn student_roster_for_theme(theme_id: &str) -> Option<serde_json::Value> {
    if theme_id.is_empty() || theme_id == kasa_mcp::character::BASE_THEME_KEY {
        kasa_mcp::character::base_characters_json()
    } else {
        kasa_mcp::character::theme_characters_json(theme_id)
    }
}

/// 참조 그림의 `(경로, mtime초)` — mtime 이 미리보기 텍스처 캐시 키에 들어가므로
/// 그림을 갈아 끼우면 키가 바뀌어 낡은 그림이 화면에 눌어붙지 않는다.
pub(crate) fn themegen_ref_info(theme_id: &str, slug: &str) -> Option<(std::path::PathBuf, u64)> {
    let p = themegen_ref_path(theme_id, slug)?;
    let t = std::fs::metadata(&p)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some((p, t))
}

/// macOS 파일 선택 대화상자. 고를 때까지 **블록**되므로 GUI 스레드에서 부르면
/// 화면이 통째로 멎는다 — 반드시 스레드에서. 취소하면 osascript 가 비정상 종료해
/// stdout 이 비고, 그대로 None 이 된다.
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
pub(crate) fn diag_line() -> String {
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

fn write_custom_themes(list: Vec<serde_json::Value>) {
    let named: Vec<serde_json::Value> = list
        .into_iter()
        .map(|mut e| {
            let (slug, label) = (theme::custom_slug(&e), theme::custom_label(&e));
            if let Some(o) = e.as_object_mut() {
                o.entry("slug").or_insert(serde_json::Value::String(slug));
                o.entry("label").or_insert(serde_json::Value::String(label));
            }
            e
        })
        .collect();
    socket::write_setting("custom_themes", serde_json::Value::Array(named));
}

pub(crate) fn rename_custom_theme(slug: &str, label: &str) -> Result<(), String> {
    let label = label.trim();
    if label.is_empty() {
        return Err(reject("label_empty", "이름을 적어 주세요".to_string()));
    }
    let mut list = theme::custom_themes(&socket::read_settings());
    let Some(entry) = list
        .iter_mut()
        .find(|entry| theme::custom_slug(entry) == slug)
    else {
        return Err(reject(
            "custom_theme_absent",
            "그 커스텀 팔레트가 없어요".to_string(),
        ));
    };
    let Some(object) = entry.as_object_mut() else {
        return Err("커스텀 팔레트 정의가 깨졌어요".to_string());
    };
    object.insert(
        "label".to_string(),
        serde_json::Value::String(label.to_string()),
    );
    write_custom_themes(list);
    Ok(())
}

pub(crate) fn palette_hex_list(s: &serde_json::Value, slug: Option<&str>) -> Vec<String> {
    let list = theme::custom_themes(s);
    let obj = theme::find_custom(&list, slug.unwrap_or(""));
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

/// 진행 중인 숨은 로그인 한 건. **동시에 하나만** — 두 슬롯을 같이 로그인하면
/// 브라우저 창이 둘 뜨고 어느 창이 어느 슬롯인지 알 수가 없다.
#[derive(Clone)]
pub(crate) struct LoginJob {
    /// 로그인 중인 슬롯 id(`acct-2` · `codex-1`).
    pub(crate) id: String,
    pub(crate) state: LoginState,
}

#[derive(Clone, PartialEq)]
pub(crate) enum LoginState {
    Running,
    /// CLI 가 `Paste code here if prompted >` 에서 멈춰 **코드를 기다린다**.
    ///
    /// 지금 `claude auth login` 의 redirect_uri 는 localhost 가 아니라
    /// `platform.claude.com/oauth/code/callback` 이다 — 브라우저가 승인해도 CLI 로
    /// 자동으로 돌아오는 길이 없고, 사람이 화면의 코드를 stdin 에 붙여넣어야 끝난다.
    /// 그 칸이 없던 동안 이 로그인은 **한 번도 성공할 수 없었고**, 3분 타임아웃까지
    /// 매달렸다가 실패했다(2026-09-05 실측: 슬롯만 늘고 키체인 항목이 116개까지
    /// 쌓였다 — 거노 "계정 등록만 100번 한 것 같은데").
    NeedCode,
    Ok,
    /// 실패 이유 한 줄. 사용자에게 그대로 보인다.
    Err(String),
}

/// 진행 중인 로그인 한 건의 손잡이 셋.
///
/// - `0` 표시용 상태(`LoginJob`)
/// - `1` CLI 프로세스의 그룹 id — 취소가 브라우저 손자까지 걷어내야 한다.
/// - `2` 그 프로세스의 stdin — **OAuth 코드를 여기로 밀어 넣는다.** 예전엔 핸들을
///   그냥 붙들고만 있었는데(닫으면 CLI 가 EOF 로 죽어서), 그래서 코드를 넣을 길이
///   없었다. 셀에 두면 설정창이 사용자가 붙여넣은 코드를 그대로 전달할 수 있다.
type LoginCell =
    std::sync::Mutex<(Option<LoginJob>, Option<u32>, Option<std::process::ChildStdin>)>;
fn login_cell() -> &'static LoginCell {
    static CELL: std::sync::OnceLock<LoginCell> = std::sync::OnceLock::new();
    CELL.get_or_init(|| std::sync::Mutex::new((None, None, None)))
}

pub(crate) fn hidden_login_job() -> Option<LoginJob> {
    login_cell().lock().ok().and_then(|cell| cell.0.clone())
}

/// 「다른 로그인을 시작하면 안 되는 상태」. 코드를 기다리는 중도 포함한다 —
/// 그 사이 새 로그인을 띄우면 앞의 CLI 가 stdin 을 잃고 조용히 3분 뒤 실패한다.
pub(crate) fn hidden_login_running() -> bool {
    hidden_login_job()
        .is_some_and(|job| matches!(job.state, LoginState::Running | LoginState::NeedCode))
}

/// 코드 입력 칸을 띄울 때인가.
pub(crate) fn hidden_login_needs_code() -> bool {
    hidden_login_job().is_some_and(|job| job.state == LoginState::NeedCode)
}

/// CLI 가 코드를 요구하기 시작했다 — 출력 리더 스레드가 부른다.
fn mark_login_needs_code(id: &str) {
    if let Ok(mut c) = login_cell().lock() {
        if let Some(job) = c.0.as_mut() {
            // 늦게 도착한 옛 작업의 출력이 새 작업 표시를 갈아치우면 안 된다.
            if job.id == id && job.state == LoginState::Running {
                job.state = LoginState::NeedCode;
            }
        }
    }
}



/// 클립보드 글자가 OAuth 코드처럼 생겼나.
///
/// claude 가 승인 뒤 주는 것은 `<코드>#<state>` 한 덩이다 — 공백이 없고, 양쪽이
/// URL-safe 글자로만 되어 있으며 둘 다 길다. 이 셋을 다 만족하는 글자가 사람 손으로
/// 클립보드에 우연히 들어갈 일은 사실상 없다.
///
/// **느슨하게 잡으면 안 된다.** 로그인이 코드를 기다리는 동안 복사한 아무 글자나
/// stdin 으로 밀어 넣으면, CLI 는 그걸 틀린 코드로 받아 로그인을 통째로 실패시킨다.
pub(crate) fn looks_like_login_code(s: &str) -> bool {
    let s = s.trim();
    let Some((code, state)) = s.split_once('#') else {
        return false;
    };
    code.len() >= 16
        && state.len() >= 16
        && !state.contains('#')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '#'))
}

#[cfg(test)]
mod login_code_shape_tests {
    use super::looks_like_login_code;

    /// 승인 뒤 실제로 오는 모양 — `<코드>#<state>`.
    #[test]
    fn takes_a_real_looking_code() {
        assert!(looks_like_login_code(
            "aBcD1234efGH5678ijKL#9PpVOsDxzsxTsoCn-OyJnCFa3gYOXdUKjRGAN9GoOr0"
        ));
        // 앞뒤 공백은 사람이 드래그로 복사하면 흔히 붙는다.
        assert!(looks_like_login_code(
            "  aBcD1234efGH5678ijKL#9PpVOsDxzsxTsoCn-OyJnCFa3gYOXdUKjRGAN9GoOr0\n"
        ));
    }

    /// **여기가 이 함수의 존재 이유다.** 코드를 기다리는 동안 복사한 아무 글자나
    /// stdin 으로 밀어 넣으면 CLI 가 틀린 코드로 받아 로그인을 통째로 실패시킨다.
    #[test]
    fn refuses_everything_else_people_copy() {
        for s in [
            "",
            "안녕하세요",
            "https://claude.com/cai/oauth/authorize?code=true#frag",
            "short#short",
            "aBcD1234efGH5678ijKL",                       // # 이 없다
            "aBcD1234efGH5678ijKL#short",                 // 뒤가 짧다
            "short#9PpVOsDxzsxTsoCn-OyJnCFa3gYOXdUKjRG",  // 앞이 짧다
            "aBcD1234efGH5678ijKL#9PpVOsDxzsxTsoCn#extra", // # 이 둘
            "aBcD1234efGH5678ijKL #9PpVOsDxzsxTsoCnOyJn", // 공백이 있다
            "git commit -m 'aBcD1234efGH5678ijKL#9PpVOsDxzsxTsoCnOyJn'",
        ] {
            assert!(!looks_like_login_code(s), "이건 코드가 아니다: {s:?}");
        }
    }
}

/// 검사 하네스가 **로그인 중 화면을 세우기 위해** 상태만 심는다.
///
/// 조건부로 나타나는 칸(코드 입력·취소 버튼)은 그 조건이 안 서면 아예 안 그려져,
/// 클릭 영역 감사가 「겹치지 않는다」가 아니라 「못 봤다」로 끝난다(2026-09-05
/// 코하루 지적). 그 조건을 리그에서 세우는 유일한 손잡이다.
///
/// **실제 로그인과 엉키지 않는다:**
/// - 이미 도는 로그인이 있으면 **거부**한다(`false`). 진짜 승인 흐름을 가짜 상태로
///   덮어 사람이 브라우저에서 누른 승인을 잃는 일이 없어야 한다.
/// - 프로세스도 stdin 도 만들지 않는다. 그래서 이 상태에서 코드가 들어와도
///   `submit_login_code` 가 `false` 를 돌려줄 뿐 아무 데도 안 보낸다.
///
/// ⚠️ **걷는 것은 부른 쪽 몫이다.** 심은 상태에는 프로세스가 없어 타임아웃이 돌지
/// 않으므로, 안 걷으면 그 창은 「로그인 중」에 영영 머문다. 검사 끝에
/// `cancel_hidden_login()` 을 반드시 부를 것.
// 검사 하네스(testkit)가 부를 자리다. 그쪽 배선이 아직 안 붙어 죽은 것으로
// 보이지만, 지우면 조건부 칸을 세울 손잡이가 사라진다.
#[allow(dead_code)]
pub(crate) fn seed_login_state_for_probe(id: &str, state: LoginState) -> bool {
    let Ok(mut cell) = login_cell().lock() else {
        return false;
    };
    if cell.1.is_some() || cell.2.is_some() {
        return false;
    }
    if cell
        .0
        .as_ref()
        .is_some_and(|job| matches!(job.state, LoginState::Running | LoginState::NeedCode))
    {
        return false;
    }
    cell.0 = Some(LoginJob { id: id.to_string(), state });
    true
}

/// 코드를 기다리는 동안 클립보드를 살펴 **복사만 해도 로그인이 끝나게** 한다
/// (거노 2026-09-05 「설정창에 붙여넣는거 없이도 가능하게 해봐」).
///
/// 브라우저가 승인 뒤 코드를 화면에 띄우면 사람은 그걸 복사한다 — 거기서 창을
/// 옮겨 칸을 찾아 누르고 붙여넣는 세 동작이 남는데, 그 세 동작이 실제로 사람을
/// 멈춰 세운다. 복사까지는 어차피 하므로 그 지점에서 받는다.
///
/// **로그인이 코드를 기다리는 동안에만** 본다. 그 밖에서는 한 번도 읽지 않는다.
/// 시작 시점의 값은 미리 기억해 두고 **달라졌을 때만** 쓴다 — 전에 복사해 둔 옛
/// 코드를 집어 실패시키지 않기 위해서다.
fn clipboard_login_code(seen: &mut Option<String>) -> Option<String> {
    let mut cb = arboard::Clipboard::new().ok()?;
    let now = cb.get_text().ok()?;
    if seen.as_deref() == Some(now.as_str()) {
        return None;
    }
    seen.replace(now.clone());
    looks_like_login_code(&now).then(|| now.trim().to_string())
}

/// 사용자가 붙여넣은 OAuth 코드를 CLI stdin 으로 보낸다.
///
/// 보낸 뒤 상태를 `Running` 으로 되돌린다 — 코드가 맞는지는 CLI 가 판정하고, 그
/// 결과는 프로세스 종료 코드로 온다. 틀린 코드면 CLI 가 다시 물어보므로 리더가
/// 곧 `NeedCode` 로 되돌린다.
pub(crate) fn submit_login_code(code: &str) -> bool {
    use std::io::Write;
    let code = code.trim();
    if code.is_empty() {
        return false;
    }
    let Ok(mut c) = login_cell().lock() else { return false };
    let Some(stdin) = c.2.as_mut() else { return false };
    if writeln!(stdin, "{code}").is_err() {
        return false;
    }
    let _ = stdin.flush();
    if let Some(job) = c.0.as_mut() {
        job.state = LoginState::Running;
    }
    true
}

pub(crate) fn cancel_hidden_login() {
    let pgid = login_cell().lock().ok().and_then(|mut cell| {
        cell.0 = None;
        // stdin 을 놓아 EOF 를 준다 — 코드를 기다리던 CLI 는 이걸로 즉시 끝난다.
        cell.2 = None;
        cell.1.take()
    });
    #[cfg(unix)]
    if let Some(pgid) = pgid {
        unsafe {
            libc::kill(-(pgid as i32), libc::SIGTERM);
        }
    }
    #[cfg(windows)]
    if let Some(pid) = pgid {
        let _ = crate::proc::command("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .status();
    }
}

/// 로그인 승인을 **어느 브라우저**에서 받을지. 두 길이 다 필요하다.
///
/// 격리만 있던 동안 불편이 실재했다(거노 2026-08-15 「계정 로그인이 쓰던 브라우저로
/// 안 열림」): 만료된 슬롯을 **그 계정 그대로** 되살릴 때도 빈 크롬이 떠서 비밀번호와
/// 2단계 인증을 처음부터 다시 쳐야 했다. 반대로 쓰던 브라우저만 있으면 새 슬롯이
/// 전부 같은 계정으로 붙는 옛 버그로 돌아간다. 그래서 고르게 둔다.
///
/// **다시 로그인의 기본은 `Default`(쓰던 브라우저)다**(거노 2026-08-15 「orca 처럼
/// 기본이 쓰던 브라우저로」). 있는 슬롯을 다시 로그인하는 일은 거의 전부 「그 계정
/// 그대로 되살리기」라, 흔한 쪽이 한 번에 끝나야 한다. `Isolated` 는 그 슬롯에 다른
/// 계정을 붙일 때의 보조 선택지로 남는다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LoginBrowser {
    /// 쿠키 없는 크롬 프로필. URL 을 우리가 주워 그 창으로 연다.
    Isolated,
    /// 사용자가 쓰던 기본 브라우저. CLI 가 직접 연다 — 지금 로그인된 claude.ai
    /// 세션이 그대로 승인되므로 **다른 계정을 붙일 수는 없다**.
    Default,
}

/// 로그인을 처음 시작할 때 여는 브라우저. **새 슬롯 추가도 쓰던 브라우저다**
/// (거노 2026-08-15 「다시 로그인 말고 추가여도 그냥 쓰던 브라우저여도 되고」).
///
/// 여기는 원래 격리 고정이었다. 이유가 있었다 — 쓰던 브라우저를 열면 그 창의
/// claude.ai 세션이 그대로 승인돼 슬롯 전부가 같은 계정이 됐다(거노: "계정추가하면
/// 1,2 같은계정으로 되는데"). 그 함정을 **아는 상태의 재지시**라 뒤집는다: 대개는
/// 브라우저에 붙은 그 계정을 추가하려는 것이고, 빈 크롬에서 비밀번호와 2단계를
/// 처음부터 치는 값이 매번 나가는 쪽이 더 비쌌다.
///
/// 다른 계정을 붙이려면 두 길이 있다 — 브라우저에서 계정을 먼저 바꾸거나, 슬롯이
/// 생긴 뒤 그 카드의 「빈 창으로」로 다시 로그인하거나. 격리 경로는 그래서 남아
/// 있다(`LoginBrowser::Isolated`).
fn login_browser_default() -> LoginBrowser {
    LoginBrowser::Default
}

/// 슬롯을 새로 만들고 띄우는 토스트. claude·codex 가 같은 말을 해야 한다 —
/// 갈리는 건 어느 서비스인가뿐이고, 사용자가 할 일은 똑같다.
fn add_account_toast() -> String {
    "쓰던 브라우저에서 로그인하세요 — 다른 계정이면 브라우저에서 먼저 계정을 바꾸세요"
        .to_string()
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
    id: String,
    argv: String,
    env_key: &'static str,
    dir: std::path::PathBuf,
    browser: LoginBrowser,
) {
    use std::io::Read;
    use std::process::Stdio;
    let profile = login_profile(browser, &id);
    let Ok(mut cell) = login_cell().lock() else { return };
    if cell
        .0
        .as_ref()
        .is_some_and(|job| job.state == LoginState::Running)
    {
        return;
    }
    cell.0 = Some(LoginJob { id: id.clone(), state: LoginState::Running });
    cell.1 = None;
    drop(cell);
    std::thread::spawn(move || {
        // 로그인 셸을 거치는 이유는 `auth_probe` 와 같다 — Finder 로 뜬 .app 의
        // PATH 에는 claude·codex 가 없어 직접 spawn 하면 항상 실패한다.
        let shell = resolve_default_shell().unwrap_or_else(|| "/bin/sh".to_string());
        let mut cmd = crate::proc::command(shell);
        cmd.arg("-lc")
            .arg(&argv)
            .env(env_key, &dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        {
            // **항상** 가로챈다. 예전엔 「쓰던 브라우저」를 고르면 CLI 가 직접 열게
            // 뒀는데, 이제는 주소의 `redirect_uri` 를 우리 창구로 고쳐야 하므로
            // (`with_local_redirect`) 여는 일도 우리가 해야 한다. CLI 가 직접 열면
            // 옛 주소가 열려 승인해도 앱으로 돌아오지 못한다.
            cmd.env("BROWSER", "/usr/bin/true");
        }
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
        // stdin 을 셀에 맡긴다 — 붙들고만 있으면(옛 코드) 코드를 넣을 길이 없고,
        // 떨어뜨리면 CLI 가 EOF 로 즉시 죽는다. 취소·완료 때 셀이 놓아 준다.
        if let Ok(mut c) = login_cell().lock() {
            c.1 = Some(child.id());
            c.2 = child.stdin.take();
        }
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
            let job_id = id.clone();
            readers.push(std::thread::spawn(move || {
                // **줄 단위로 읽으면 안 된다.** 코드를 묻는 `Paste code here if
                // prompted >` 는 개행 없이 오므로 `lines()` 는 그 줄을 EOF 까지
                // 내주지 않는다 — 프로세스가 죽어야 비로소 보이니 「기다리는 중」을
                // 영영 못 잡는다(2026-09-05 실측). 바이트로 읽어 꼬리까지 본다.
                let mut pipe = pipe;
                let mut chunk = [0u8; 1024];
                let mut tail = String::new();
                let mut opened = false;
                let mut asked = false;
                loop {
                    let n = match pipe.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    let text = String::from_utf8_lossy(&chunk[..n]).to_string();
                    if let Ok(mut b) = buf.lock() {
                        b.push_str(&text);
                    }
                    tail.push_str(&text);
                    // 완성된 줄에서만 URL 을 찾는다 — 반쯤 온 URL 로 브라우저를
                    // 열면 끊긴 주소가 뜬다.
                    while let Some(pos) = tail.find('\n') {
                        let line: String = tail.drain(..=pos).collect();
                        if opened {
                            continue;
                        }
                        if let Some(url) = login_url_in(&line) {
                            // ⚠️ **주소를 고치지 않는다.** 한동안 `redirect_uri` 를
                            // 우리 창구로 바꿔치기했는데, OAuth 는 코드를 토큰으로
                            // 바꿀 때 **authorize 에 쓴 것과 같은 redirect_uri** 를
                            // 다시 보내야 한다. CLI 는 자기가 만든 원래 주소로
                            // 교환하니 서버가 불일치로 거부했다 — 승인까지 마치고도
                            // 400 이 뜨던 원인이 이것이다(2026-09-07 「인증도 안돼
                            // 400떠」). 승인이 끝나면 화면에 코드가 뜨고, 그걸
                            // 복사하는 것만으로 들어간다(아래 클립보드 경로).
                            match profile.as_deref() {
                                Some(prof) => {
                                    let _ = std::fs::create_dir_all(prof);
                                    open_isolated_browser(&url, prof);
                                }
                                None => open_default_browser(&url),
                            }
                            opened = true;
                        }
                    }
                    // 남은 꼬리가 코드를 묻고 있나. CLI 가 다시 물으면(틀린 코드)
                    // 그때도 잡아야 하므로 꼬리를 비운 뒤 플래그를 되돌린다.
                    if tail.contains("Paste code") {
                        if !asked {
                            asked = true;
                            mark_login_needs_code(&job_id);
                        }
                    } else if tail.is_empty() {
                        asked = false;
                    }
                }
            }));
        }
        // 3분. 브라우저에서 계정을 새로 만드는 사람도 있어 넉넉히 두지만, 무한정
        // 두면 취소를 안 누른 사용자가 「로그인 중」에 영구히 갇힌다.
        //
        // **코드를 묻기 시작하면 10분으로 늘린다** — 그때부터는 사람이 브라우저에서
        // 승인하고 코드를 복사해 창을 옮겨 붙여넣는 시간이라, 3분은 실제로 모자란다.
        let mut deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
        let mut extended = false;
        let mut clip_seen: Option<String> = None;
        let mut clip_at = std::time::Instant::now();
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
            if hidden_login_needs_code() {
                if !extended {
                    extended = true;
                    deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
                    // 기다리기 **시작한 순간**의 클립보드를 기준으로 잡는다. 이 값과
                    // 같으면 사람이 아직 아무것도 복사하지 않은 것이다.
                    clip_seen = arboard::Clipboard::new()
                        .ok()
                        .and_then(|mut c| c.get_text().ok());
                }
                // ② 그래도 코드가 손에 남는 환경이 있다(되돌림 창구가 못 섰거나
                //    브라우저가 localhost 를 막는 경우). 복사만 해도 들어가게 둔다.
                //    1초에 한 번만 — 사람이 복사하고 창을 돌아오는 데 그보다 짧게
                //    걸리지 않는다.
                if clip_at.elapsed() >= std::time::Duration::from_secs(1) {
                    clip_at = std::time::Instant::now();
                    if let Some(code) = clipboard_login_code(&mut clip_seen) {
                        submit_login_code(&code);
                    }
                }
            }
            if std::time::Instant::now() > deadline {
                let _ = child.kill();
                let why = if extended {
                    "코드를 기다리다 10분이 지났어요 — 다시 해 주세요"
                } else {
                    "로그인이 3분 안에 안 끝났어요"
                };
                finish_login(&id, LoginState::Err(why.into()));
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
    // 성공했으면 **그 자리에서** 신원과 한도를 다시 읽는다. 둘 다 캐시가 있어
    // (신원 20초·한도는 폴러 주기) 가만두면 「로그인을 마쳤어요」가 뜬 뒤에도
    // 한동안 옛 계정과 빈 한도가 화면에 남는다(2026-09-07 「로그인을 마쳤어요 뜨고
    // 바로 바뀐계정으로 나와야지 한도랑」).
    if state == LoginState::Ok {
        probe_cache().lock().unwrap().remove(id);
        // 활성 계정은 작업대를 물으므로 그쪽 키도 함께 비운다.
        probe_cache().lock().unwrap().remove("");
        crate::handler::usage_poke().store(true, std::sync::atomic::Ordering::Relaxed);
    }
    if let Ok(mut c) = login_cell().lock() {
        if c.0.as_ref().is_some_and(|j| j.id == id) {
            if let Some(j) = c.0.as_mut() {
                j.state = state;
            }
            c.1 = None;
            // 끝난 프로세스의 stdin 을 계속 들고 있으면, 다음 로그인의 코드가
            // 죽은 파이프로 가 조용히 사라진다.
            c.2 = None;
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

/// 이 로그인이 쓸 격리 프로필. **`None` 이면 URL 가로채기를 통째로 끈다** — 그
/// 하나가 「우리가 크롬을 연다」와 「CLI 가 기본 브라우저를 연다」를 가른다. 값이
/// `Some` 인데 `BROWSER` 를 안 덮으면 창이 둘 뜨고, `None` 인데 덮으면 아무 창도
/// 안 떠 3분을 갇힌다. 격리를 골랐는데 자리를 못 잡는 극단(홈을 못 찾음)에서는
/// 아무것도 못 여느니 쓰던 브라우저로 떨어뜨린다.
fn login_profile(browser: LoginBrowser, id: &str) -> Option<std::path::PathBuf> {
    match browser {
        LoginBrowser::Isolated => socket::oauth_profile_dir(id),
        LoginBrowser::Default => None,
    }
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

    /// 「쓰던 브라우저」를 고르면 프로필이 없어야 한다. 이 값 하나가 `BROWSER`
    /// 가로채기와 크롬 실행을 **동시에** 끄므로, 여기가 `Some` 으로 새면 사용자는
    /// 빈 크롬 창을 다시 보게 된다(거노 2026-08-15 「쓰던 브라우저로 안 열림」).
    #[test]
    fn default_browser_takes_no_profile() {
        assert!(super::login_profile(super::LoginBrowser::Default, "acct-2").is_none());
    }

    /// 격리는 슬롯마다 다른 자리를 잡는다 — 같은 자리를 나눠 쓰면 두 슬롯이 한
    /// 계정으로 붙는다(이 기능이 존재하는 이유인 그 버그).
    #[test]
    fn isolated_takes_a_per_slot_profile() {
        let a = super::login_profile(super::LoginBrowser::Isolated, "acct-2");
        let b = super::login_profile(super::LoginBrowser::Isolated, "codex-1");
        // 홈을 못 찾는 환경에서는 둘 다 `None` 이라 비교할 것이 없다.
        if let (Some(a), Some(b)) = (a, b) {
            assert!(a.ends_with("acct-2"), "슬롯 자리가 아니다: {}", a.display());
            assert_ne!(a, b, "두 슬롯이 같은 브라우저 프로필을 쓴다");
        }
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


/// 사용자가 쓰던 브라우저로 연다. CLI 에게 맡기지 않는 이유는 **주소를 우리가
/// 고쳐야** 하기 때문이다(`with_local_redirect`) — CLI 가 직접 열면 옛 주소가 열려
/// 앱으로 돌아오는 길이 없다.
fn open_default_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let r = crate::proc::command("open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let r = crate::proc::command("cmd").args(["/C", "start", "", url]).spawn();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let r = crate::proc::command("xdg-open").arg(url).spawn();
    if let Err(e) = r {
        eprintln!("[account] 브라우저 실행 실패: {e}");
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
pub(crate) struct AuthProbe {
    pub(crate) logged_in: bool,
    pub(crate) email: String,
    /// 그 슬롯 토큰이 속한 조직. 이메일 하나에 개인 조직과 팀 조직이 둘 다 달려
    /// 있으면 슬롯 둘이 **같은 이메일로** 보여 어느 쪽이 회사 것인지 알 수 없다
    /// (거노: "팀플랜인지 구분하게 돼?"). 한도가 따로 도는 별개 계정이라 이걸
    /// 못 가르면 자동 전환이 어디로 갔는지도 못 읽는다.
    pub(crate) org: String,
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
type ProbeCache = std::sync::Mutex<
    std::collections::HashMap<String, (std::time::Instant, Option<AuthProbe>)>,
>;

fn probe_cache() -> &'static ProbeCache {
    static CACHE: std::sync::OnceLock<ProbeCache> = std::sync::OnceLock::new();
    CACHE.get_or_init(ProbeCache::default)
}

/// 검증 리그에 슬롯 신원을 심는다. 실제 값은 슬롯 토큰을 물어야 나오는데 격리
/// 리그에는 로그인이 없어, 심지 않으면 「이 줄이 누구인지」를 화면으로 확인할
/// 길이 자체가 없다.
pub(crate) fn seed_auth_probe(id: &str, probe: Option<AuthProbe>) {
    probe_cache()
        .lock()
        .unwrap()
        .insert(id.to_string(), (std::time::Instant::now(), probe));
}

pub(crate) fn auth_probe(id: &str) -> Option<AuthProbe> {
    use std::time::{Duration, Instant};
    let cache = probe_cache();
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
    // ⚠️ 활성 계정은 금고가 아니라 **작업대**를 물어야 한다(env 를 안 붙인다).
    // 금고 env 로 `claude auth status` 를 띄우면 그 claude 가 금고에 고정돼,
    // 만료 상태면 금고 사본으로 refresh 를 시도해 **작업대와 공유하는 1회용
    // refresh token 을 소비**한다 — 재시작 전-pane 로그아웃(2026-08-18 22:04)의
    // 1순위 방아쇠가 이 프로브였다(부팅 +23초에 금고가 쓰인 keychain 기록).
    // 활성 계정의 로그인 상태는 작업대가 정본이라 표시도 이쪽이 정확하다.
    let dir = if crate::claude_auth::workbench_account().as_deref() == Some(id) {
        None
    } else {
        socket::claude_account_dir(id)
    };
    std::thread::spawn(move || {
        // ⚠️ `claude auth status` 를 더 이상 띄우지 않는다. 그 명령은 access token 이
        // 만료돼 있으면 저장소의 refresh token 을 **소비**해 회전시키는데, 안 쓰는
        // 슬롯은 사용량 폴러(프록시 `/claude-usage`)도 같은 일을 한다 — 둘이 겹치면
        // 한쪽이 죽은 refresh token 으로 실패하고, claude 는 실패하면 그 자리에
        // 로그아웃 껍데기를 쓴다(2026-08-19 사고의 물증이 그 껍데기였고, 2026-09-03
        // 에도 네이버 슬롯이 그 꼴로 발견됐다). 안 쓰는 슬롯의 갱신은 프록시 하나만
        // 하게 두고, 여기서는 슬롯 토큰으로 **신원만** 묻는다 — 「토큰 없음」이면
        // 로그인이 필요한 슬롯, 신원이 오면 로그인된 슬롯이다.
        //
        // 신원의 출처는 슬롯 토큰뿐이다. `auth status` 의 email·orgName 은 공유
        // 캐시(`~/.claude.json`)라 마지막에 로그인한 계정이 모든 슬롯에 찍힌다 —
        // 2026-08-31 에 그걸 1순위로 올렸다가 슬롯 다섯이 전부 지메일로 보였다.
        // 그때의 「활성 행이 빈손」은 키체인 동명 껍데기 항목 탓이었고(http.rs
        // `read_claude_credentials` 가 계정 칸까지 맞춰 해결), 일시 실패는 표에 남은
        // 마지막 진짜 신원으로 그린다 — 틀린 이메일보다 옛 진짜 값이 낫다.
        let probe = match slot_identity_full(dir.as_deref()) {
            SlotIdentity::Known { email, org } => Some(AuthProbe { logged_in: true, email, org }),
            SlotIdentity::NoToken => Some(AuthProbe {
                logged_in: false,
                email: String::new(),
                org: String::new(),
            }),
            SlotIdentity::Unavailable => {
                let (email, org) = remembered_identity(&key);
                (!email.is_empty()).then(|| AuthProbe { logged_in: true, email, org })
            }
        };
        {
            let mut m = probe_cache().lock().unwrap();
            // 조회 자체가 실패했으면(셸이 안 뜸·JSON 이 아님) 알던 값을 유지한다 —
            // 답을 못 받은 것과 "로그인 안 됐다" 는 답을 받은 것은 다르다.
            let v = probe.or_else(|| m.get(&key).and_then(|(_, v)| v.clone()));
            if let Some(p) = v.as_ref().filter(|p| !p.email.is_empty()) {
                remember_account_identity(&key, &p.email, &p.org);
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
enum SlotIdentity {
    Known { email: String, org: String },
    /// 저장소에 토큰이 없다 — 로그인이 필요한 슬롯(껍데기).
    NoToken,
    /// 답을 못 받았다(막 만료돼 갱신 전·upstream 막힘). 로그아웃과 다르다.
    Unavailable,
}

fn slot_identity_full(dir: Option<&std::path::Path>) -> SlotIdentity {
    let port = crate::mcp_panel_port();
    let d = dir.map(|p| p.display().to_string()).unwrap_or_default();
    // -G + --data-urlencode: 경로에 공백이나 한글이 섞여도 쿼리로 안전하게 실린다.
    let Ok(out) = crate::proc::command("curl")
        .args(["-s", "--max-time", "12", "-G", "--data-urlencode", &format!("dir={d}")])
        .arg(format!("http://127.0.0.1:{port}/claude-identity"))
        .output()
    else {
        return SlotIdentity::Unavailable;
    };
    let field = |v: &serde_json::Value, k: &str| {
        v.get(k).and_then(|s| s.as_str()).unwrap_or_default().to_string()
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else {
        return SlotIdentity::Unavailable;
    };
    if v.get("ok").and_then(|b| b.as_bool()) == Some(true) {
        return SlotIdentity::Known { email: field(&v, "email"), org: field(&v, "org") };
    }
    if field(&v, "error") == "no token" {
        SlotIdentity::NoToken
    } else {
        SlotIdentity::Unavailable
    }
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
fn codex_auth_path(id: &str) -> Option<std::path::PathBuf> {
    match socket::codex_account_dir(id) {
        Some(d) => Some(d.join("auth.json")),
        None => Some(kasa_socket::home_dir()?.join(".codex/auth.json")),
    }
}

/// 토큰 값은 직렬화·저장·로그하지 않고 알려진 자리가 비어 있지 않은지만 본다.
pub(crate) fn codex_logged_in(id: &str) -> bool {
    let Some(path) = codex_auth_path(id) else { return false };
    let Ok(text) = std::fs::read_to_string(path) else { return false };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return false };
    let nonempty = |p: &str| v.pointer(p).and_then(|x| x.as_str()).is_some_and(|s| !s.is_empty());
    nonempty("/tokens/access_token")
        || nonempty("/tokens/id_token")
        || nonempty("/tokens/refresh_token")
        || v.get("OPENAI_API_KEY").and_then(|x| x.as_str()).is_some_and(|s| !s.is_empty())
}

pub(crate) fn codex_identity(id: &str) -> Option<String> {
    use base64::Engine as _;
    let path = codex_auth_path(id)?;
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
fn remember_account_identity(id: &str, email: &str, org: &str) {
    let settings = socket::read_settings();
    for (key, val) in [("claude_account_emails", email), ("claude_account_orgs", org)] {
        let mut m = match settings.get(key) {
            Some(serde_json::Value::Object(m)) => m.clone(),
            _ => serde_json::Map::new(),
        };
        if m.get(id).and_then(|v| v.as_str()) == Some(val) {
            continue; // 값이 그대로면 쓰지 않는다 — 20초마다 파일을 다시 쓸 이유가 없다
        }
        m.insert(id.to_string(), serde_json::Value::String(val.to_string()));
        socket::write_setting(key, serde_json::Value::Object(m));
    }
}

/// 마지막에 알아낸 그 슬롯의 진짜 신원(이메일, 조직). 슬롯 토큰으로 못 물을 때
/// (활성 계정의 access token 이 막 만료돼 pane 이 갱신하기 전 같은 순간) 화면이
/// 「확인 중」으로 굳는 대신 이걸 그린다. 공유 캐시는 절대 여기 안 들어온다.
fn remembered_identity(id: &str) -> (String, String) {
    let settings = socket::read_settings();
    let get = |key: &str| {
        settings
            .get(key)
            .and_then(|m| m.get(id))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    (get("claude_account_emails"), get("claude_account_orgs"))
}

/// 슬롯을 지울 때 그 이메일 기록도 지운다. 안 지우면 목록에 없는 유령 키가 남고,
/// 같은 번호를 다시 쓰는 슬롯이 **옛 계정의 이메일로 불린다** — statusline 은 이 표만
/// 보므로 화면이 조용히 거짓말을 한다.
fn forget_account_email(id: &str) {
    let settings = socket::read_settings();
    for key in ["claude_account_emails", "claude_account_orgs"] {
        let Some(serde_json::Value::Object(mut m)) = settings.get(key).cloned() else {
            continue;
        };
        if m.remove(id).is_none() {
            continue;
        }
        socket::write_setting(key, serde_json::Value::Object(m));
    }
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

pub(crate) fn toggle(g: &mut gpu::GpuRenderer, r: Rect, on: bool, cursor: (f32, f32)) {
    let hover = inside(r, cursor);
    g.hover_pointer |= hover;
    let track = if on {
        theme::accent()
    } else if hover {
        theme::with_alpha(theme::text_mute(), 0x66)
    } else {
        theme::with_alpha(theme::text_mute(), 0x4d)
    };
    pill_rect(g, r.0, r.1, r.2, r.3, track);
    let knob = r.3 - 6.0;
    let kx = if on { r.0 + r.2 - knob - 3.0 } else { r.0 + 3.0 };
    circle_rect(g, kx, r.1 + 3.0, knob, theme::text());
}

#[cfg(test)]
mod account_label_tests {
    use super::{label_is_auto, merge_web_codes, put_web_code};

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

    #[test]
    fn 웹_확인_자료는_토스트가_없어도_응답에_남는다() {
        put_web_code("confirm", serde_json::json!({ "id": "acct-2" }));
        let out = merge_web_codes(serde_json::json!({ "ok": true, "message": null }));
        assert_eq!(out["confirm"]["id"], "acct-2");
    }
}
