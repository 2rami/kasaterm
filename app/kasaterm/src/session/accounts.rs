//! Claude·Codex 계정 전환 — 칸마다 지금 계정을 재고, 바꾸면 무엇이 다시 뜨는지 묻고,
//! 조용해진 칸부터 다시 띄운다. 웹(폰) 확인 카드도 같은 대기표를 쓴다.
use super::*;

impl App {
    /// agent 프로세스의 환경을 읽는다. 계정 전환의 판단은 설정값이 아니라 이 값처럼
    /// 이미 떠 있는 process가 실제로 물고 있는 인증 경로를 기준으로 해야 한다.
    pub(super) fn pane_agent_ps_env(&self, pane: &str, expected: kasa_pty::AgentKind) -> Option<String> {
        let shell = self.pty.get(pane)?.shell_pid()?;
        let (kind, pid) = kasa_pty::agent_pid_for_shell(&kasa_pty::process_table_shared(), shell)?;
        if kind != expected {
            return None;
        }
        let out = crate::proc::command("ps")
            .args(["eww", "-o", "command=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        if !out.status.success() || out.stdout.is_empty() {
            return None;
        }
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// 그 pane 의 claude 가 **실제로 어느 계정으로 떠 있는지** 실측한다 — 프로세스
    /// env 의 자격증명 저장소 경로를 읽는다. `Some("")` = 기본 로그인으로 떠 있음,
    /// `None` = 실측 실패(claude 가 없거나 `ps` 가 안 되는 플랫폼).
    ///
    /// 전환 이벤트를 기록해 두고 추정하는 방식은 앱이 못 본 전환(재시작 전의 전환,
    /// 손으로 띄운 claude)에서 어긋난다 — 도는 프로세스의 env 가 유일한 진실이다.
    pub(crate) fn pane_claude_boot_account_dir(&self, pane: &str) -> Option<String> {
        self.pane_agent_ps_env(pane, kasa_pty::AgentKind::Claude)
            .map(|line| parse_securestorage_dir(&line))
    }

    /// Codex pane의 실제 인증 슬롯. `CODEX_HOME`은 pane별 임시 홈이므로 그 자체를
    /// 계정으로 읽으면 안 된다. 그 안 `auth.json` 링크가 가리키는 **대상 디렉터리**가
    /// 현재 로그인 슬롯의 정본이다.
    pub(crate) fn pane_codex_boot_account_dir(&self, pane: &str) -> Option<String> {
        let line = self.pane_agent_ps_env(pane, kasa_pty::AgentKind::Codex)?;
        let codex_home = parse_env_value(&line, "CODEX_HOME")?;
        let auth_link = std::path::PathBuf::from(codex_home).join("auth.json");
        auth_link_target_dir(&auth_link)
    }

    /// 계정 id → 화면 이름. 자동 전환 토스트가 쓰던 규칙 그대로다 — 이름 없는
    /// 슬롯은 「계정 N」, 목록에 없는 id(기본 로그인 포함)는 기본 계정 표기.
    pub(crate) fn claude_account_display(&self, id: &str) -> String {
        match self.set_claude_accounts.iter().position(|a| a.id == id) {
            Some(i) => crate::settings::account_display(
                id,
                &self.set_claude_accounts[i].label,
                &format!("계정 {}", i + 1),
            ),
            None => "계정 선택 필요".to_string(),
        }
    }

    /// Codex 슬롯에도 Claude와 같은 표기 규칙을 적용한다. 슬롯 id는 구현 세부라
    /// 화면에는 사람이 붙인 라벨이나 순번만 보인다.
    pub(crate) fn codex_account_display(&self, id: &str) -> String {
        match self
            .set_codex_accounts
            .iter()
            .position(|account| account.id == id)
        {
            Some(index) => crate::settings::codex_account_display(
                id,
                &self.set_codex_accounts[index].label,
                &format!("계정 {}", index + 2),
            ),
            None => crate::settings::codex_account_display("", "", "기본 계정"),
        }
    }

    /// 확인 없이 지금 바꾼다. 옛 `SettingsAction::ClaudeAccount` 팔의 본문 그대로다.
    pub(crate) fn claude_account_switch_now(&mut self, id: &str) -> bool {
        self.settings_input = None;
        let same = id == self.set_claude_account;
        // 바뀐 자리를 반짝여 준다 — 우상단 토스트만으로는 정작 계정 칩이 아무 변화
        // 없이 그대로라 「바뀐 줄 모르겠다」가 된다. 같은 계정을 다시 누른 경우엔
        // 켜지 않는다(아무것도 안 바뀌었는데 축포를 터뜨리는 꼴이다).
        let (_, to_label, restarted, deferred, focused, live) =
            self.apply_claude_account_switch(id);
        if !live {
            self.set_toast("계정을 전환하지 못했어요. 해당 계정의 로그인을 확인해 주세요".to_string());
            return false;
        }
        if !same {
            self.account_flash = Some(std::time::Instant::now());
        }
        self.set_toast(crate::session::account_switch_toast(
            &to_label, same, restarted, deferred, focused, live,
        ));
        true
    }

    /// 두 공급자가 같은 확인 카드와 안전 규칙을 쓰게 모은다. 카드에만 상태를 담아
    /// 취소하면 작업대, pane, 설정 어느 것도 바뀌지 않는다.
    pub(super) fn show_account_switch_confirm(
        &mut self,
        provider: AccountSwitchProvider,
        to: &str,
        to_label: String,
        impact: AccountSwitchImpact,
        surface: ConfirmSurface,
    ) -> String {
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        self.account_switch_confirm = Some(PendingAccountSwitch {
            provider,
            nonce: nonce.clone(),
            to_label,
            to: to.to_string(),
            impact,
            surface,
            rects: Vec::new(),
        });
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
        nonce
    }

    /// 다시 뜰 Claude pane이 있으면 물어보고, 없으면 지금 바꾼다.
    pub(crate) fn ask_or_switch_claude_account(&mut self, to: &str, surface: ConfirmSurface) {
        let impact = self.preview_claude_account_switch(to);
        if !impact.needs_confirm() {
            self.claude_account_switch_now(to);
            return;
        }
        let _ = self.show_account_switch_confirm(
            AccountSwitchProvider::Claude,
            to,
            self.claude_account_display(to),
            impact,
            surface,
        );
    }

    /// 확인 없이 Codex 계정을 지금 바꾼다. 이미 실행 중인 Codex는 auth link를 다시
    /// 읽지 않으므로, 적용 경로는 Claude와 같이 stale 표시와 안전 재시작을 탄다.
    pub(crate) fn codex_account_switch_now(&mut self, id: &str) {
        self.settings_input = None;
        let same = id == self.set_codex_account;
        if !same {
            self.account_flash = Some(std::time::Instant::now());
        }
        let (_, to_label, restarted, deferred, focused, live) = self.apply_codex_account_switch(id);
        self.set_toast(account_switch_toast_for(
            "Codex", &to_label, same, restarted, deferred, focused, live,
        ));
    }

    /// Codex 계정 선택의 공개 진입점. handler와 settings는 이 함수만 부르면 확인,
    /// 현재 pane 보호, 조용해진 뒤 재시작 규칙을 Claude와 동일하게 얻는다.
    pub(crate) fn ask_or_switch_codex_account(&mut self, to: &str, surface: ConfirmSurface) {
        let impact = self.preview_codex_account_switch(to);
        if !impact.needs_confirm() {
            self.codex_account_switch_now(to);
            return;
        }
        let _ = self.show_account_switch_confirm(
            AccountSwitchProvider::Codex,
            to,
            self.codex_account_display(to),
            impact,
            surface,
        );
    }

    /// 웹 설정에서 계정을 고른 첫 단계. 재시작 대상이 없으면 바로 바꾸고, 있으면
    /// 아무 상태도 바꾸지 않은 채 웹이 그릴 확인 자료만 돌려준다.
    pub(crate) fn request_web_account_switch(
        &mut self,
        provider: AccountSwitchProvider,
        to: &str,
    ) -> Result<Option<serde_json::Value>, String> {
        if self
            .account_switch_confirm
            .as_ref()
            .is_some_and(|p| p.surface == ConfirmSurface::Web)
        {
            self.account_switch_confirm = None;
        }
        let (impact, to_label) = match provider {
            AccountSwitchProvider::Claude => (
                self.preview_claude_account_switch(to),
                self.claude_account_display(to),
            ),
            AccountSwitchProvider::Codex => (
                self.preview_codex_account_switch(to),
                self.codex_account_display(to),
            ),
        };
        if !impact.needs_confirm() {
            match provider {
                AccountSwitchProvider::Claude => {
                    if !self.claude_account_switch_now(to) {
                        return Err("계정을 전환하지 못했어요. 해당 계정의 로그인을 확인해 주세요".to_string());
                    }
                }
                AccountSwitchProvider::Codex => self.codex_account_switch_now(to),
            }
            return Ok(None);
        }
        let nonce = self.show_account_switch_confirm(
            provider,
            to,
            to_label.clone(),
            impact,
            ConfirmSurface::Web,
        );
        Ok(Some(web_account_confirm_json(
            provider,
            to,
            &to_label,
            &impact,
            &nonce,
        )))
    }

    /// 웹 확인 카드의 두 번째 단계. 응답이 현재 대기표와 정확히 맞을 때만 소비한다.
    pub(crate) fn resolve_web_account_switch(
        &mut self,
        provider: AccountSwitchProvider,
        to: &str,
        nonce: &str,
        accept: bool,
    ) -> Result<(), String> {
        let picked = take_web_account_switch(
            &mut self.account_switch_confirm,
            provider,
            to,
            nonce,
            accept,
        )?;
        if let Some((provider, to)) = picked {
            match provider {
                AccountSwitchProvider::Claude => {
                    if !self.claude_account_switch_now(&to) {
                        return Err("계정을 전환하지 못했어요. 해당 계정의 로그인을 확인해 주세요".to_string());
                    }
                }
                AccountSwitchProvider::Codex => self.codex_account_switch_now(&to),
            }
        }
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
        Ok(())
    }

    pub(crate) fn current_web_account_confirmation(&self) -> Option<serde_json::Value> {
        let pending = self
            .account_switch_confirm
            .as_ref()
            .filter(|pending| pending.surface == ConfirmSurface::Web)?;
        Some(web_account_confirm_json(
            pending.provider,
            &pending.to,
            &pending.to_label,
            &pending.impact,
            &pending.nonce,
        ))
    }

    /// 확인 카드에서 고른 결과. 취소는 **아무 일도 안 한다**.
    pub(crate) fn account_switch_pick(&mut self, btn: AccountSwitchBtn) {
        let Some(p) = self.account_switch_confirm.take() else {
            return;
        };
        if btn == AccountSwitchBtn::Switch {
            match p.provider {
                AccountSwitchProvider::Claude => { self.claude_account_switch_now(&p.to); }
                AccountSwitchProvider::Codex => self.codex_account_switch_now(&p.to),
            }
        }
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// 전환 **뒤** 목표 저장소 경로. `apply` 안에 인라인돼 있던 계산을 들어낸 것이라
    /// 동작은 그대로다.
    pub(crate) fn claude_target_dir(&self, to: &str) -> String {
        crate::claude_auth::runtime_dir_for(to, to)
            .or_else(|| socket::claude_account_dir(to))
            .map_or(String::new(), |p| p.to_string_lossy().into_owned())
    }

    /// 선택한 Codex 슬롯의 실제 auth 디렉터리. 기본 로그인은 `~/.codex/auth.json`의
    /// 부모이고, 별도 슬롯은 설정 아래 `codex-accounts/<id>`다. pane 쪽도 auth link
    /// 대상의 부모를 돌려주므로 문자열 비교가 같은 실체를 가리킨다.
    pub(crate) fn codex_target_dir(&self, to: &str) -> String {
        socket::codex_account_dir(to)
            .or_else(|| kasa_socket::home_dir().map(|home| home.join(".codex")))
            .map_or(String::new(), |path| path.to_string_lossy().into_owned())
    }

    /// 전환 **전** 예측 — `swap_active` 를 안 돌리고 같은 답을 낸다(`predicted_target_dir`).
    pub(crate) fn predict_claude_target_dir(&self, to: &str) -> String {
        predicted_target_dir(
            to,
            crate::claude_auth::active_dir().is_some(),
            crate::claude_auth::vault_ready(to, socket::claude_account_dir),
            socket::claude_account_dir(to).as_deref(),
        )
    }

    /// 지금 떠 있는 claude pane 들의 판정 재료. `ps` 를 pane 마다 한 번 도므로
    /// **클릭에서만** 부른다(프레임마다 부르면 안 된다).
    pub(crate) fn claude_pane_facts(&mut self) -> Vec<PaneAccountFact> {
        let focused = {
            let ws = self.ws.lock().unwrap();
            account_switch_focused_tab(&ws)
        };
        let ids: Vec<String> = self
            .pty
            .iter()
            .filter(|(_, p)| p.active_agent() == Some(kasa_pty::AgentKind::Claude))
            .map(|(id, _)| id.clone())
            .collect();
        ids.into_iter()
            .map(|id| {
                let boot_dir = self.pane_claude_boot_account_dir(&id);
                let resumable = self
                    .pane_claude_sid
                    .get(&id)
                    .is_some_and(|s| socket::transcript_path_for_session(s).is_some());
                PaneAccountFact {
                    focused: focused.as_deref() == Some(id.as_str()),
                    closed: self.stashed_record(&id).is_some(),
                    busy: account_restart_busy(
                        self.pane_activity.get(&id).is_some_and(|a| a.status == "waiting"),
                        self.pane_activity
                            .get(&id)
                            .map(|a| (a.status.as_str(), a.bg_active)),
                    ),
                    boot_dir,
                    resumable,
                    id,
                }
            })
            .collect()
    }

    /// Codex pane의 계정 전환 판정 재료. `CODEX_HOME`은 실행마다 새로 생기는 pane
    /// 전용 홈이라 직접 비교하지 않고, 그 안 auth link의 실제 대상을 `boot_dir`로 둔다.
    pub(crate) fn codex_pane_facts(&mut self) -> Vec<PaneAccountFact> {
        let focused = {
            let ws = self.ws.lock().unwrap();
            account_switch_focused_tab(&ws)
        };
        let ids: Vec<String> = self
            .pty
            .iter()
            .filter(|(_, pane)| pane.active_agent() == Some(kasa_pty::AgentKind::Codex))
            .map(|(id, _)| id.clone())
            .collect();
        ids.into_iter()
            .map(|id| {
                let boot_dir = self.pane_codex_boot_account_dir(&id);
                let resumable = self
                    .pane_claude_sid
                    .get(&id)
                    .is_some_and(|sid| socket::codex_rollout_for_session(sid).is_some());
                PaneAccountFact {
                    focused: focused.as_deref() == Some(id.as_str()),
                    closed: self.stashed_record(&id).is_some(),
                    busy: account_restart_busy(
                        self.pane_activity.get(&id).is_some_and(|a| a.status == "waiting"),
                        self.pane_activity
                            .get(&id)
                            .map(|activity| (activity.status.as_str(), activity.bg_active)),
                    ),
                    boot_dir,
                    resumable,
                    id,
                }
            })
            .collect()
    }

    /// 「이 전환을 누르면 무슨 일이 일어나나」 — 아무것도 바꾸지 않고 세기만 한다.
    pub(crate) fn preview_claude_account_switch(&mut self, to: &str) -> AccountSwitchImpact {
        let target = self.predict_claude_target_dir(to);
        let facts = self.claude_pane_facts();
        account_switch_impact(&facts, &target)
    }

    /// Codex에는 Claude 작업대처럼 실행 중 pane의 auth를 갈아 끼우는 통로가 없다.
    /// 따라서 선택 슬롯의 실제 auth 디렉터리와 모두 대조해 재시작 영향을 미리 센다.
    pub(crate) fn preview_codex_account_switch(&mut self, to: &str) -> AccountSwitchImpact {
        let target = self.codex_target_dir(to);
        let facts = self.codex_pane_facts();
        account_switch_impact(&facts, &target)
    }

    pub(crate) fn apply_claude_account_switch(
        &mut self,
        to: &str,
    ) -> (String, String, usize, usize, bool, bool) {
        let from_label = self.claude_account_display(&self.set_claude_account.clone());
        let to_label = self.claude_account_display(to);
        if to.is_empty() || !self.set_claude_accounts.iter().any(|account| account.id == to) {
            return (from_label, to_label, 0, 0, false, false);
        }
        // ① 작업대를 새 계정으로 갈아 끼운다. 이것만으로 **작업대를 보고 도는 pane 은
        // 전부** 다음 요청부터 새 계정이 된다 — 재시작도, 대화 끊김도 없다. claude 가
        // 요청 직전마다 저장소를 다시 읽고, 자기 것과 다른 토큰이 있으면 그대로
        // 채택하기 때문이다(claude_auth 모듈 머리말).
        let swapped = crate::claude_auth::swap_active(to, socket::claude_account_dir);
        if matches!(swapped, crate::claude_auth::SwapOutcome::VaultEmpty | crate::claude_auth::SwapOutcome::WriteFailed) {
            return (from_label, to_label, 0, 0, false, false);
        }
        // /status 가 보여주는 신원 캐시(~/.claude.json oauthAccount)도 함께 갈아
        // 끼운다 — 저장소만 바꾸면 과금은 새 계정인데 /status 는 옛말을 한다.
        // AlreadyActive 여도 부른다: 캐시는 다른 로그인(밖에서 친 claude /login)이
        // 언제든 덮을 수 있어, 「맞추기」 클릭이 그걸 바로잡는 손이 된다.
        crate::claude_auth::adopt_oauth_account_cache(
            crate::mcp_panel_port(),
            socket::claude_account_dir(to),
        );
        // ② 재시작이 필요한 pane 은 **작업대를 안 보는** 것들뿐이다 — 이 기능이 생기기
        // 전에 뜬 pane 은 특정 금고에 못 박혀 있어 갈아 끼우기가 안 닿는다.
        let target_dir = self.claude_target_dir(to);
        // 칩을 달지 말지는 **확인 카드가 세는 것과 같은 함수**로 가른다 — 두 곳에
        // 따로 판정하면 「3개가 다시 떠요」라고 물어 놓고 5개가 뜨는 일이 생긴다.
        for f in self.claude_pane_facts() {
            if pane_account_fate(&f, &target_dir) == PaneAccountFate::Unchanged {
                // 이미 목표 계정으로 도는 pane — 남아 있던 표시도 걷는다(A→B→A 복귀).
                self.pane_account_stale.remove(&f.id);
                continue;
            }
            // 칩의 「A → B」 표기. 실측이 안 되면(다른 플랫폼) 어긋났을 수 있다는
            // 쪽으로 보수 판정하고, 표기는 전환 전 활성 계정으로 쓴다.
            let boot_label = match f.boot_dir.as_deref() {
                Some(d) => self.claude_account_display(account_id_of_dir(d)),
                None => from_label.clone(),
            };
            self.pane_account_stale
                .insert(f.id, (boot_label, to_label.clone()));
        }
        self.set_claude_account = to.to_string();
        // shim 재굽기가 재시작보다 **먼저**여야 새로 뜨는 claude 가 새 계정을 탄다.
        self.settings_save();
        let restarted = self.run_pending_account_restarts();
        let (deferred, focused_pending) =
            self.pending_account_restart_state(kasa_pty::AgentKind::Claude);
        // 연결된 기기들도 같은 계정으로 — 본진에서 바꾸면 작업대가, 작업대에서 바꾸면
        // 본진이 따라온다(2026-09-18 지시). 자격증명은 기계마다 따로라 옮기지 않고
        // 신원(이메일·조직)과 별명만 보내며, 받는 쪽은 자기 슬롯 중 같은 것을 고른다.
        if !self.account_switch_from_peer {
            let identity = crate::settings::account_identity(to).unwrap_or_default();
            let label = self.set_claude_accounts.iter().find(|a| a.id == to)
                .map(|a| a.label.clone()).filter(|l| !l.is_empty())
                .unwrap_or_else(|| to_label.clone());
            propagate_account_switch(identity, label);
        }
        (
            from_label,
            to_label,
            restarted,
            deferred,
            focused_pending,
            true,
        )
    }

    /// 다른 기기가 보낸 신원·별명에 맞는 내 슬롯. 신원이 같은 슬롯이 먼저, 없으면 별명이
    /// 같은 슬롯. 비어 있거나 중복된 후보는 고르지 않는다.
    pub(crate) fn peer_account_slot(&self, identity: &str, label: &str) -> Option<String> {
        let slots: Vec<(String, String)> = self.set_claude_accounts.iter()
            .filter(|a| !a.id.is_empty()).map(|a| (a.id.clone(), a.label.clone()))
            .collect();
        peer_account_slot_in(
            &slots,
            |id| crate::settings::account_identity(id),
            |id| self.claude_account_display(id),
            identity,
            label,
        )
    }

    /// Codex 계정 전환을 현재 pane에도 적용한다. Codex는 실행 중인 process의 auth link를
    /// 다시 읽지 않으므로 선택 파일만 바꾸고 끝내면 "다음 실행만"처럼 보인다. 실제 link
    /// 대상이 다른 pane에는 Claude와 같은 stale/restart 규칙을 적용한다.
    pub(crate) fn apply_codex_account_switch(
        &mut self,
        to: &str,
    ) -> (String, String, usize, usize, bool, bool) {
        let from_label = self.codex_account_display(&self.set_codex_account.clone());
        let to_label = self.codex_account_display(to);
        let target_dir = self.codex_target_dir(to);
        self.set_codex_account = to.to_string();
        // 이 파일은 Codex shim이 다음 실행 때 읽는다. 먼저 저장해야 restart가 새 슬롯의
        // auth.json 링크를 만들고, 옛 슬롯으로 다시 뜨지 않는다.
        self.settings_save();
        for fact in self.codex_pane_facts() {
            if pane_account_fate(&fact, &target_dir) == PaneAccountFate::Unchanged {
                self.pane_account_stale.remove(&fact.id);
                continue;
            }
            let boot_label = match fact.boot_dir.as_deref() {
                Some(dir) => self.codex_account_display(account_id_of_dir(dir)),
                None => from_label.clone(),
            };
            self.pane_account_stale
                .insert(fact.id, (boot_label, to_label.clone()));
        }
        let restarted = self.run_pending_account_restarts();
        let (deferred, focused_pending) =
            self.pending_account_restart_state(kasa_pty::AgentKind::Codex);
        // `settings_save`가 shim의 선택 파일을 썼으므로 새 pane과 재시작 pane은 즉시 이
        // 슬롯을 쓴다. 실행 중인 pane은 위 stale 경로에서만 남는다.
        (
            from_label,
            to_label,
            restarted,
            deferred,
            focused_pending,
            true,
        )
    }

    /// stale 칩은 공급자별 전환을 같은 map에 보관한다. 토스트와 확인 결과는 이번
    /// 공급자의 pane만 세야, Claude와 Codex 전환이 겹쳐도 숫자가 섞이지 않는다.
    pub(super) fn pending_account_restart_state(&self, agent: kasa_pty::AgentKind) -> (usize, bool) {
        let focused = {
            let ws = self.ws.lock().unwrap();
            account_switch_focused_tab(&ws)
        };
        let is_matching = |id: &str| {
            self.pane_account_stale.contains_key(id)
                && self
                    .pty
                    .get(id)
                    .is_some_and(|pane| pane.active_agent() == Some(agent))
        };
        let deferred = self
            .pane_account_stale
            .keys()
            .filter(|id| is_matching(id))
            .count();
        let focused_pending = focused.as_deref().is_some_and(|id| is_matching(id));
        (deferred, focused_pending)
    }

    /// 「⟳ 재시작」 표시가 남은 pane 중 지금 쉬는 것을 새 계정으로 되띄운다.
    /// 300ms 활동 스캔 끝에 불려, 전환 때 일하던 pane 도 턴이 끝나는 대로 따라온다.
    /// 반환은 이번에 되띄운 수.
    pub(crate) fn run_pending_account_restarts(&mut self) -> usize {
        if self.pane_account_stale.is_empty() {
            self.pane_account_quiet_since.clear();
            return 0;
        }
        // Claude 작업대는 현재 활성 금고를 빈 경로로 읽는 별도 규칙이 있고, Codex는
        // pane별 auth link의 target을 그대로 비교한다. 나머지 안전 대기 규칙은 같다.
        let claude_target_dir =
            crate::claude_auth::runtime_dir_for(&self.set_claude_account, &self.set_claude_account)
                .map_or(String::new(), |p| p.to_string_lossy().into_owned());
        let codex_target_dir = self.codex_target_dir(&self.set_codex_account);
        // 지금 사용자가 보고 있는 pane 은 자동으로 안 끊는다. 화면 밖 pane 이 조용히
        // 갈리는 것과, 대화하던 상대가 눈앞에서 사라지는 것은 전혀 다른 일이다
        // (2026-08-15 "하다가 계정전환하니까 너가 없어졌어"). 표시는 남으므로 헤더
        // 칩을 누르면 그때 갈린다 — 그 pane 만은 사용자가 시점을 고른다.
        let focused = {
            let ws = self.ws.lock().unwrap();
            account_switch_focused_tab(&ws)
        };
        let pending: Vec<String> = self.pane_account_stale.keys().cloned().collect();
        let now = std::time::Instant::now();
        let mut restarted = 0usize;
        for id in pending {
            if focused.as_deref() == Some(id.as_str()) {
                self.pane_account_quiet_since.remove(&id);
                continue;
            }
            // 하네스가 이미 내려갔거나 계정 전환을 지원하지 않는 pane은 표시만 걷는다.
            let agent = self.pty.get(&id).and_then(|pane| pane.active_agent());
            let target_dir = match agent {
                Some(kasa_pty::AgentKind::Claude) => claude_target_dir.as_str(),
                Some(kasa_pty::AgentKind::Codex) => codex_target_dir.as_str(),
                _ => {
                    self.pane_account_stale.remove(&id);
                    continue;
                }
            };
            // 닫힌 pane 은 사용자 눈 밖에서 되띄우지 않는다 — 표시를 남겨 두면
            // 되살렸을 때 칩이 안내한다.
            if self.stashed_record(&id).is_some() {
                continue;
            }
            // 일하는 중·승인 대기·백그라운드 작업 중이면 끊지 않는다 — resume 은
            // 대화를 잇지만 진행 중이던 턴은 죽는다. 활동 기록이 아직 없는 pane 도
            // 다음 틱(300ms)까지 미룬다.
            let busy = account_restart_busy(
                self.pane_activity.get(&id).is_some_and(|a| a.status == "waiting"),
                self.pane_activity
                    .get(&id)
                    .map(|a| (a.status.as_str(), a.bg_active)),
            );
            if busy {
                self.pane_account_quiet_since.remove(&id);
                continue;
            }
            // 조용해진 지 얼마나 됐나. 스피너가 도구 결과 사이에서 한 틱 사라지는
            // 틈은 이 문턱을 못 넘는다 — 그 틈에 끊으면 하던 턴이 통째로 죽는다.
            let quiet_since = *self
                .pane_account_quiet_since
                .entry(id.clone())
                .or_insert(now);
            if now.duration_since(quiet_since) < Self::ACCOUNT_RESTART_QUIET {
                continue;
            }
            // 그 사이 손으로 되띄웠을 수 있다 — 죽이기 직전 실측으로 한 번 더 확인.
            let boot_dir = match agent {
                Some(kasa_pty::AgentKind::Claude) => self.pane_claude_boot_account_dir(&id),
                Some(kasa_pty::AgentKind::Codex) => self.pane_codex_boot_account_dir(&id),
                _ => None,
            };
            if boot_dir.as_deref() == Some(target_dir) {
                self.pane_account_stale.remove(&id);
                continue;
            }
            if self.restart_pane_agent(&id) {
                self.pane_account_stale.remove(&id);
                self.pane_account_quiet_since.remove(&id);
                restarted += 1;
            }
        }
        // 표시가 걷힌 pane 의 조용 기록은 남길 이유가 없다.
        self.pane_account_quiet_since
            .retain(|k, _| self.pane_account_stale.contains_key(k));
        restarted
    }

    /// 되띄우기 전에 요구하는 **연속 조용 시간**. 화면 판독은 300ms 박자라 이 값이면
    /// 열 번 넘게 연속으로 조용한 것을 본 셈이다. 짧으면 턴 중간의 깜빡임에 걸리고,
    /// 길면 전환이 굼떠 보인다.
    pub(super) const ACCOUNT_RESTART_QUIET: std::time::Duration = std::time::Duration::from_secs(4);
}

/// 경로 앞의 홈을 `~` 로 접는다. 홈을 못 찾으면 원본 그대로.
///
/// 같은 코드가 pane 라벨·상태바·미리보기에 네 벌 흩어져 있었고, 전부 `HOME` 을
/// 직접 읽어 Windows(GUI 프로세스엔 HOME 이 없다)에서 한 곳도 안 접혔다.
/// `home_dir()` 은 `USERPROFILE` 까지 본다.
/// `ps eww` 한 줄의 환경 변수 하나. 공백이 든 경로는 첫 토막만 잡혀 어긋난 값이
/// 되지만, 계정 전환에서는 "다르다" 쪽으로 보수 판정되어 안전 재시작으로 수습된다.
pub(super) fn parse_env_value(ps_line: &str, name: &str) -> Option<String> {
    let prefix = format!("{name}=");
    ps_line
        .split_whitespace()
        .find_map(|token| token.strip_prefix(&prefix))
        .map(str::to_string)
}

/// `ps eww` 한 줄에서 Claude 자격증명 저장소 경로를 뽑는다. 변수가 아예 없으면
/// `""`(= 기본 로그인으로 떠 있음)이다.
pub(super) fn parse_securestorage_dir(ps_line: &str) -> String {
    parse_env_value(ps_line, "CLAUDE_SECURESTORAGE_CONFIG_DIR").unwrap_or_default()
}

/// pane 전용 `CODEX_HOME/auth.json` 링크의 대상 디렉터리. 링크가 상대 경로여도
/// CODEX_HOME 기준으로 풀어 실제 슬롯과 비교한다.
pub(super) fn auth_link_target_dir(auth_link: &std::path::Path) -> Option<String> {
    let target = std::fs::read_link(auth_link).ok()?;
    let target = if target.is_absolute() {
        target
    } else {
        auth_link.parent()?.join(target)
    };
    target
        .parent()
        .map(|dir| dir.to_string_lossy().into_owned())
}

/// 저장소 경로 → 계정 id(`…/claude-accounts/<id>` 의 꼬리). `""` 는 기본 로그인이라
/// 그대로 — `claude_account_display` 가 목록에 없는 id 를 기본 계정으로 접는다.
pub(super) fn account_id_of_dir(dir: &str) -> &str {
    if dir.is_empty() {
        return "";
    }
    dir.rsplit(['/', '\\']).next().unwrap_or(dir)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum AccountSwitchBtn {
    Cancel,
    Switch,
}

/// 확인 카드를 누른 뒤 어느 설정을 실제로 바꿀지. 문자열로 두면 Claude 확인을
/// Codex 전환으로 잘못 이어도 컴파일러가 못 막으므로 닫힌 enum으로 둔다.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum AccountSwitchProvider {
    Claude,
    Codex,
}

impl AccountSwitchProvider {
    pub(crate) fn web_key(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

/// 대기 중인 계정 전환 확인.
///
/// **여기 담긴 것 말고는 아무 상태도 미리 바뀌지 않는다** — 작업대 갈아 끼우기·신원
/// 캐시·⟳ 칩·설정 저장·재시작은 [전환] 을 눌러야 그때 돈다. 취소하면 이 값을
/// 버리는 것으로 끝이다.
pub(crate) struct PendingAccountSwitch {
    pub provider: AccountSwitchProvider,
    pub nonce: String,
    pub to: String,
    pub to_label: String,
    pub impact: AccountSwitchImpact,
    pub surface: ConfirmSurface,
    /// 그 창의 render 가 매 프레임 채운다. hit rect 를 별도 App 필드로 두는 것이
    /// 레포 관례지만, `struct App` 정의는 병렬 작업 충돌 핫스팟이라 여기 담아 필드
    /// 하나로 줄였다(CLAUDE.md 병렬 규칙).
    pub rects: Vec<(AccountSwitchBtn, (f32, f32, f32, f32))>,
}

pub(super) fn take_web_account_switch(
    pending: &mut Option<PendingAccountSwitch>,
    provider: AccountSwitchProvider,
    to: &str,
    nonce: &str,
    accept: bool,
) -> Result<Option<(AccountSwitchProvider, String)>, String> {
    let matches = pending.as_ref().is_some_and(|p| {
        p.surface == ConfirmSurface::Web
            && p.provider == provider
            && p.to == to
            && p.nonce == nonce
    });
    if !matches {
        return Err("확인할 계정 전환이 없거나 대상이 달라졌어요".to_string());
    }
    let p = pending.take().expect("위에서 확인한 대기표");
    Ok(accept.then_some((p.provider, p.to)))
}

pub(super) fn web_account_confirm_json(
    provider: AccountSwitchProvider,
    to: &str,
    to_label: &str,
    impact: &AccountSwitchImpact,
    nonce: &str,
) -> serde_json::Value {
    let (title, lines) = account_switch_confirm_text(to_label, impact);
    serde_json::json!({
        "provider": provider.web_key(),
        "id": to,
        "nonce": nonce,
        "title": title,
        "lines": lines,
        "dangerous": impact.fresh > 0,
    })
}

#[cfg(test)]
mod web_account_confirm_tests {
    use super::*;

    fn pending(provider: AccountSwitchProvider, to: &str) -> Option<PendingAccountSwitch> {
        Some(PendingAccountSwitch {
            provider,
            nonce: "nonce-a".to_string(),
            to: to.to_string(),
            to_label: "다음 계정".to_string(),
            impact: AccountSwitchImpact {
                restart_when_quiet: 1,
                restart_after_turn: 1,
                fresh: 1,
                ..Default::default()
            },
            surface: ConfirmSurface::Web,
            rects: Vec::new(),
        })
    }

    #[test]
    fn 웹_취소는_대기표만_소비하고_전환을_만들지_않는다() {
        let mut p = pending(AccountSwitchProvider::Claude, "acct-2");
        let picked = take_web_account_switch(
            &mut p,
            AccountSwitchProvider::Claude,
            "acct-2",
            "nonce-a",
            false,
        )
        .unwrap();
        assert!(picked.is_none());
        assert!(p.is_none());
    }

    #[test]
    fn 웹_확정만_혼합된_재시작_대상을_전환으로_내보낸다() {
        let mut p = pending(AccountSwitchProvider::Codex, "codex-2");
        assert_eq!(p.as_ref().unwrap().impact.torn_down(), 2);
        assert_eq!(p.as_ref().unwrap().impact.fresh, 1);
        let picked = take_web_account_switch(
            &mut p,
            AccountSwitchProvider::Codex,
            "codex-2",
            "nonce-a",
            true,
        )
        .unwrap();
        assert_eq!(picked, Some((AccountSwitchProvider::Codex, "codex-2".to_string())));
        assert!(p.is_none());
    }

    #[test]
    fn 웹_확인은_공급자와_대상이_다르면_소비되지_않는다() {
        let mut p = pending(AccountSwitchProvider::Claude, "acct-2");
        assert!(take_web_account_switch(
            &mut p,
            AccountSwitchProvider::Codex,
            "acct-2",
            "nonce-a",
            true,
        )
        .is_err());
        assert!(p.is_some());
        let mut gone = None;
        assert!(take_web_account_switch(
            &mut gone,
            AccountSwitchProvider::Claude,
            "acct-2",
            "nonce-a",
            false,
        )
        .is_err());
    }

    #[test]
    fn 같은_계정의_옛_확인은_새_대기표를_소비하지_않는다() {
        let mut p = pending(AccountSwitchProvider::Claude, "acct-2");
        p.as_mut().unwrap().nonce = "nonce-new".to_string();
        assert!(take_web_account_switch(
            &mut p,
            AccountSwitchProvider::Claude,
            "acct-2",
            "nonce-old",
            true,
        )
        .is_err());
        assert_eq!(p.as_ref().unwrap().nonce, "nonce-new");
    }

    #[test]
    fn 웹_확인_자료는_재시작과_새대화_위험을_그대로_싣는다() {
        let impact = AccountSwitchImpact {
            restart_when_quiet: 1,
            restart_after_turn: 2,
            fresh: 1,
            ..Default::default()
        };
        let prompt = web_account_confirm_json(
            AccountSwitchProvider::Claude,
            "acct-2",
            "팀 계정",
            &impact,
            "nonce-a",
        );
        assert_eq!(prompt["provider"], "claude");
        assert_eq!(prompt["id"], "acct-2");
        assert_eq!(prompt["nonce"], "nonce-a");
        assert_eq!(prompt["dangerous"], true);
        assert!(prompt["title"].as_str().unwrap().contains('3'));
        assert!(prompt["lines"]
            .as_array()
            .unwrap()
            .iter()
            .any(|line| line.as_str().is_some_and(|line| line.contains("새로 떠요"))));
    }
}

/// 계정 전환이 pane 하나에 무엇을 하는지 정하는 데 필요한 **사실만**. `App` 을 안
/// 들고 다니므로 테스트가 된다.
pub(crate) struct PaneAccountFact {
    pub id: String,
    /// `ps` 로 실측한 부팅 저장소. `None` = 실측 실패(claude 가 아니거나 Windows) —
    /// 어긋났을 수 있다는 쪽으로 보수 판정한다.
    pub boot_dir: Option<String>,
    pub focused: bool,
    pub closed: bool,
    pub busy: bool,
    /// `--resume` 할 transcript 가 실재하나. false 면 대화를 잃고 새로 뜬다.
    pub resumable: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PaneAccountFate {
    /// 이미 목표 계정 — 아무 일도 안 일어난다.
    Unchanged,
    /// 쉬는 pane — 조용해진 지 4초가 지나면 되띄운다.
    RestartWhenQuiet,
    /// 일하는 중 — 턴이 끝난 뒤에 되띄운다.
    RestartAfterTurn,
    /// 보고 있는 pane — 자동으로 안 끊고 ⟳ 칩만 단다.
    ChipFocused,
    /// 닫힌 pane — 되살릴 때 칩이 안내한다.
    ChipClosed,
}

/// ⚠️ **판정 순서가 `run_pending_account_restarts` 와 같아야 한다.** 어긋나면 물어본
/// 내용과 실제로 벌어지는 일이 갈린다 — 「3개가 다시 떠요」라고 해 놓고 5개가 뜨는 식.
pub(crate) fn pane_account_fate(f: &PaneAccountFact, target_dir: &str) -> PaneAccountFate {
    if f.boot_dir.as_deref() == Some(target_dir) {
        return PaneAccountFate::Unchanged;
    }
    if f.focused {
        return PaneAccountFate::ChipFocused;
    }
    if f.closed {
        return PaneAccountFate::ChipClosed;
    }
    if f.busy {
        return PaneAccountFate::RestartAfterTurn;
    }
    PaneAccountFate::RestartWhenQuiet
}

/// 되띄우기를 미뤄야 하는 pane 인가. 러너와 계산기가 **같은 이 함수**를 쓴다.
///
/// 활동 기록이 아직 없는 pane(`None`)도 바쁜 것으로 친다 — 방금 뜬 pane 을 그 자리에서
/// 끊지 않으려는 기존 규칙 그대로다.
pub(crate) fn account_restart_busy(prompt_wait: bool, activity: Option<(&str, bool)>) -> bool {
    prompt_wait || activity.is_none_or(|(status, bg)| status != "idle" || bg)
}

/// 레이아웃의 활성 pane은 바깥 leaf id지만, PTY와 계정 전환 상태는 탭 pid를 키로 쓴다.
/// 계정 전환에서는 지금 **보이는 탭**을 보호해야 하므로 두 id를 여기서 한 번만 맞춘다.
pub(super) fn account_switch_focused_tab(ws: &crate::Workspace) -> Option<String> {
    ws.active_pane
        .as_deref()
        .map(|outer| ws.active_tab_pid(outer))
}

/// 전환 한 번이 지금 떠 있는 pane 들에 하는 일의 총계.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct AccountSwitchImpact {
    pub unchanged: usize,
    pub restart_when_quiet: usize,
    pub restart_after_turn: usize,
    pub chip_focused: usize,
    pub chip_closed: usize,
    /// 위 재시작 대상 중 **이어붙일 대화가 없어** 새로 뜨는 수.
    pub fresh: usize,
    /// 어느 계정으로 떴는지 실측이 안 된 수(Windows 등).
    pub unmeasured: usize,
}

impl AccountSwitchImpact {
    /// 실제로 뜯겼다 다시 뜨는 pane 수.
    pub fn torn_down(&self) -> usize {
        self.restart_when_quiet + self.restart_after_turn
    }

    /// 물어봐야 하는가. **보고 있는 pane 과 닫힌 pane 은 세지 않는다** — 둘 다 자동으로
    /// 아무 일도 안 당하고, 보고 있는 pane 은 ⟳ 칩 자체가 이미 확인이라 게이트에 넣으면
    /// 한 가지를 두 번 묻는 꼴이 된다.
    pub fn needs_confirm(&self) -> bool {
        self.torn_down() > 0
    }
}

pub(crate) fn account_switch_impact(
    facts: &[PaneAccountFact],
    target_dir: &str,
) -> AccountSwitchImpact {
    let mut i = AccountSwitchImpact::default();
    for f in facts {
        let fate = pane_account_fate(f, target_dir);
        match fate {
            PaneAccountFate::Unchanged => i.unchanged += 1,
            PaneAccountFate::RestartWhenQuiet => i.restart_when_quiet += 1,
            PaneAccountFate::RestartAfterTurn => i.restart_after_turn += 1,
            PaneAccountFate::ChipFocused => i.chip_focused += 1,
            PaneAccountFate::ChipClosed => i.chip_closed += 1,
        }
        // 대화를 잃는 것은 **되띄우는 pane** 에서만 일어난다. 칩만 다는 pane 을 세면
        // 「대화가 날아간다」는 경고가 아무 일도 안 당하는 pane 때문에 켜진다.
        let restarting = matches!(
            fate,
            PaneAccountFate::RestartWhenQuiet | PaneAccountFate::RestartAfterTurn
        );
        if restarting && !f.resumable {
            i.fresh += 1;
        }
        if restarting && f.boot_dir.is_none() {
            i.unmeasured += 1;
        }
    }
    i
}

/// 확인 카드 문구 — (제목, 부제).
///
/// ⚠️ **「진행 중인 작업이 끊겨요」라고 쓰지 마라. 거짓말이다.**
/// `run_pending_account_restarts` 가 일하는 pane 을 일곱 겹으로 걸러 턴이 끝난 뒤에만
/// 되띄운다. 실제로 잃는 것은 셋뿐이다 — 그 pane 의 **화면(스크롤백)**, 입력창에 쳐
/// 놓고 안 보낸 글, 그리고 이어붙일 대화가 없는 pane 의 **대화 전체**.
pub(crate) fn account_switch_confirm_text(
    to_label: &str,
    i: &AccountSwitchImpact,
) -> (String, Vec<String>) {
    let title = format!("에이전트 {}개가 다시 떠요", i.torn_down());
    // 절을 가운뎃점으로 잇지 않고 **줄로 나눈다** — 사정이 셋만 겹쳐도 한 줄이
    // 카드 밖으로 나가고, 그러면 정작 읽어야 할 마지막 절이 잘린다.
    let mut lines = vec![format!(
        "{to_label} 로 바꾸면 대화는 이어지지만 그 pane 화면은 비워져요"
    )];
    if i.restart_after_turn > 0 {
        lines.push(format!(
            "작업 중 {}개는 턴이 끝난 뒤에 떠요",
            i.restart_after_turn
        ));
    }
    if i.fresh > 0 {
        lines.push(format!("{}개는 이어붙일 대화가 없어 새로 떠요", i.fresh));
    }
    if i.chip_focused > 0 {
        lines.push("지금 보는 pane 은 ⟳ 를 눌러야 바뀌어요".to_string());
    }
    if i.unmeasured > 0 {
        lines.push(format!(
            "{}개는 어느 계정인지 확인이 안 돼 함께 띄워요",
            i.unmeasured
        ));
    }
    (title, lines)
}

/// 전환 **전에** 목표 저장소 경로를 예측한다.
///
/// `runtime_dir_for(to, to)` 를 그냥 부르면 안 되는 이유: 그 함수는 지문이 이미 `to` 를
/// 가리킬 때만 빈 경로(작업대)를 준다. 전환 전에는 지문이 아직 옛 계정이라 금고 경로가
/// 나오고, 그러면 작업대로 뜬 pane 이 **전부** 어긋남으로 잡혀 영향 수가 과대계상된다.
///
/// 예측이 틀릴 때는 **많이 세는 쪽**으로 틀린다 — 더 물어보는 것은 안전하고, 덜 묻는
/// 것은 사고다.
pub(crate) fn predicted_target_dir(
    to: &str,
    workbench_live: bool,
    vault_ready: bool,
    vault_dir: Option<&std::path::Path>,
) -> String {
    let vault = || vault_dir.map_or(String::new(), |p| p.to_string_lossy().into_owned());
    if to.is_empty() {
        // 기본 슬롯은 금고 경로 자체가 없다 — 작업대가 곧 그 자리다.
        return String::new();
    }
    if !workbench_live || !vault_ready {
        // `swap_active` 가 WriteFailed / VaultEmpty 로 물러날 자리 — 작업대는 안 갈리고
        // 재시작 폴백만 돈다.
        return vault();
    }
    String::new()
}

pub(crate) fn account_switch_toast(
    to_label: &str,
    same: bool,
    restarted: usize,
    deferred: usize,
    focused_pending: bool,
    live: bool,
) -> String {
    account_switch_toast_for(
        "claude",
        to_label,
        same,
        restarted,
        deferred,
        focused_pending,
        live,
    )
}

/// Claude와 Codex 전환이 같은 결과 문장을 쓰되, 실제로 다시 뜨는 하네스 이름은
/// 정확히 적는다. 설정 선택만 바꾸고 pane은 그대로라는 오해를 피하는 자리다.
pub(crate) fn account_switch_toast_for(
    provider: &str,
    to_label: &str,
    same: bool,
    restarted: usize,
    deferred: usize,
    focused_pending: bool,
    live: bool,
) -> String {
    let tail = if focused_pending {
        " · 지금 이 pane 은 ⟳ 를 누르면"
    } else {
        ""
    };
    if restarted == 0 && deferred == 0 {
        return if same {
            format!("{to_label} 그대로예요 — 떠 있는 {provider} 도 전부 이 계정이에요")
        } else if live {
            format!("{to_label} 로 전환했어요 — 떠 있는 {provider} 도 다음 메시지부터예요{tail}")
        } else {
            format!("{to_label} 로 전환했어요 (다음에 뜨는 {provider} 부터){tail}")
        };
    }
    let head = if same {
        format!("{to_label} 로 맞추는 중")
    } else {
        format!("{to_label} 로 전환")
    };
    let mut parts = Vec::new();
    if restarted > 0 {
        parts.push(format!("{provider} {restarted}개 대화 이어서 다시 띄움"));
    }
    // 보고 있는 pane 은 이 수에 들어가도 자동으로 안 돈다 — 그래서 문장 끝에서
    // 따로 말한다. 「끝나면 자동」만 적으면 기다리다 영영 안 바뀌는 것으로 보인다.
    let auto_deferred = deferred.saturating_sub(usize::from(focused_pending));
    if auto_deferred > 0 {
        parts.push(format!("작업 중 {auto_deferred}개는 끝나면 자동"));
    }
    format!("{head} — {}{tail}", parts.join(" · "))
}

/// 빈 id는 미선택이므로 다른 기기의 계정 요청과 매칭하지 않는다.
pub(crate) fn peer_account_slot_in(
    slots: &[(String, String)],
    identity_of: impl Fn(&str) -> Option<String>,
    display_of: impl Fn(&str) -> String,
    identity: &str,
    label: &str,
) -> Option<String> {
    if !identity.is_empty() {
        let mut matches = slots.iter().filter(|(id, _)| !id.is_empty() && identity_of(id).as_deref() == Some(identity));
        if let Some((id, _)) = matches.next() {
            if matches.next().is_some() {
                return None;
            }
            return Some(id.clone());
        }
    }
    if label.is_empty() {
        return None;
    }
    let mut matches = slots.iter()
        .filter(|(id, l)| !id.is_empty() && ((!l.is_empty() && l == label) || display_of(id) == label));
    let id = matches.next()?.0.clone();
    matches.next().is_none().then_some(id)
}

/// 계정 전환을 명부의 다른 기기 전부에 알린다 — `claude-account-identity` 액션. 닿지 않는
/// 기기는 건너뛴다(그쪽이 켜지면 사람이 한 번 맞추면 된다).
pub(super) fn propagate_account_switch(identity: String, label: String) {
    let peers: Vec<kasa_mcp::machines::Machine> = kasa_mcp::machines::machines()
        .into_iter()
        .filter(|m| !m.base.trim().is_empty())
        .collect();
    if peers.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        for m in peers {
            let r = kasa_mcp::remote::settings_action(
                &m.base, "claude-account-identity", Some(&identity), Some(&label), None,
            );
            match r {
                Ok(v) if v.get("ok").and_then(|b| b.as_bool()) == Some(false) => eprintln!(
                    "[account] {} 는 같은 계정을 못 골랐다: {}", m.label,
                    v.get("error").and_then(|e| e.as_str()).unwrap_or("?")
                ),
                Ok(_) => {}
                Err(e) => eprintln!("[account] {} 에 계정 전환을 못 전했다: {e:#}", m.label),
            }
        }
    });
}

#[cfg(test)]
mod account_switch_tests {
    use super::{
        account_id_of_dir, account_switch_toast, auth_link_target_dir, parse_env_value,
        parse_securestorage_dir,
    };

    #[test]
    fn parses_securestorage_dir_from_ps_env_line() {
        // ps eww 실물 모양: command 뒤에 env 가 공백으로 이어진다.
        let line = "claude --resume abc TERM=xterm-256color \
             CLAUDE_SECURESTORAGE_CONFIG_DIR=/Users/kasa/.config/kasaterm/claude-accounts/acct-1 \
             HOME=/Users/kasa";
        assert_eq!(
            parse_securestorage_dir(line),
            "/Users/kasa/.config/kasaterm/claude-accounts/acct-1"
        );
        // 변수가 없으면 기본 로그인 — 실측 실패(None)와 구분되는 확정값이다.
        assert_eq!(parse_securestorage_dir("claude TERM=xterm HOME=/x"), "");
    }

    #[test]
    fn parses_codex_home_from_the_running_process_env() {
        let line = "node codex TERM=xterm CODEX_HOME=/tmp/kasaterm/codex-home-%8 HOME=/Users/kasa";
        assert_eq!(
            parse_env_value(line, "CODEX_HOME").as_deref(),
            Some("/tmp/kasaterm/codex-home-%8")
        );
        assert_eq!(parse_env_value(line, "MISSING"), None);
    }

    #[cfg(unix)]
    #[test]
    fn codex_account_is_the_auth_link_target_not_the_pane_home() {
        use std::os::unix::fs::symlink;
        let unique = format!(
            "kasaterm-codex-account-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        let pane_home = root.join("codex-home-%8");
        let slot = root.join("codex-accounts/codex-1");
        std::fs::create_dir_all(&pane_home).unwrap();
        std::fs::create_dir_all(&slot).unwrap();
        let target = slot.join("auth.json");
        symlink(&target, pane_home.join("auth.json")).unwrap();
        assert_eq!(
            auth_link_target_dir(&pane_home.join("auth.json")),
            Some(slot.display().to_string())
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn account_id_comes_from_dir_tail() {
        assert_eq!(account_id_of_dir("/a/b/claude-accounts/acct-3"), "acct-3");
        assert_eq!(account_id_of_dir(""), "");
    }

    #[test]
    fn toast_covers_all_shapes() {
        // 작업대 전환이 성공하면 도는 pane 도 즉시 따라온다 — 「다음에 뜨는」이라고
        // 말하면 거짓말이다(2026-08-17 「토스트에 다음세션부터라고 뜨는데」).
        assert!(
            account_switch_toast("사이오닉", false, 0, 0, false, true).contains("다음 메시지부터")
        );
        // 작업대 실패(금고 비었음 등)면 재시작 폴백뿐이라 옛 문장이 맞다.
        assert!(account_switch_toast("사이오닉", false, 0, 0, false, false).contains("다음에 뜨는"));
        assert!(account_switch_toast("사이오닉", true, 0, 0, false, true).contains("그대로"));
        // 되띄운 것과 기다리는 것이 한 문장에 같이 온다.
        let t = account_switch_toast("사이오닉", false, 2, 1, false, true);
        assert!(t.contains("2개") && t.contains("1개"), "{t}");
        // 보고 있는 pane 만 남았으면 「끝나면 자동」이 아니라 눌러야 한다고 말한다.
        let f = account_switch_toast("사이오닉", false, 2, 1, true, true);
        assert!(f.contains("⟳") && !f.contains("작업 중"), "{f}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_account_slot_prefers_identity_then_label_and_never_guesses() {
        let slots = vec![
            (String::new(), String::new()),
            ("acct-1".to_string(), "지메일".to_string()),
            ("acct-4".to_string(), "사이오닉팀".to_string()),
            ("acct-9".to_string(), String::new()),
        ];
        let identity_of = |id: &str| match id {
            "" | "acct-1" => Some("me@gmail.com".to_string()),
            "acct-4" => Some("Sionic".to_string()),
            _ => None,
        };
        let display_of = |id: &str| match id { "acct-9" => "계정 5".to_string(), "" => "기본 계정".to_string(), _ => id.to_string() };
        let pick = |identity: &str, label: &str| super::peer_account_slot_in(&slots, identity_of, display_of, identity, label);
        assert_eq!(pick("Sionic", "엉뚱한 별명").as_deref(), Some("acct-4"), "신원이 별명보다 먼저");
        assert_eq!(pick("me@gmail.com", "").as_deref(), Some("acct-1"), "같은 신원의 기본 작업대는 후보가 아니다");
        assert_eq!(pick("nobody@x", "지메일").as_deref(), Some("acct-1"), "신원이 없으면 별명");
        assert_eq!(pick("", "계정 5").as_deref(), Some("acct-9"), "별명이 비면 표시 이름");
        assert_eq!(pick("", "기본 계정"), None, "미선택은 계정 후보가 아니다");
        assert_eq!(pick("nobody@x", ""), None, "아무것도 안 맞으면 안 바꾼다");
    }

    fn fact(boot: Option<&str>) -> PaneAccountFact {
        PaneAccountFact {
            id: "%1".into(),
            boot_dir: boot.map(str::to_string),
            focused: false,
            closed: false,
            busy: false,
            resumable: true,
        }
    }

    /// 판정 순서가 `run_pending_account_restarts` 와 같아야 한다. 어긋나면 물어본
    /// 내용과 실제로 벌어지는 일이 갈린다.
    #[test]
    fn a_pane_already_on_the_target_is_left_alone() {
        assert_eq!(
            pane_account_fate(&fact(Some("")), ""),
            PaneAccountFate::Unchanged
        );
        assert_eq!(
            pane_account_fate(&fact(Some("/v/acct-2")), ""),
            PaneAccountFate::RestartWhenQuiet
        );
        // 맞는 계정이면 보고 있어도 칩조차 안 뜬다 — Unchanged 가 focused 를 이긴다.
        let mut f = fact(Some(""));
        f.focused = true;
        assert_eq!(pane_account_fate(&f, ""), PaneAccountFate::Unchanged);
    }

    /// 실측이 안 된 pane 을 「그대로다」로 읽으면, 실제로는 재시작되는데 아무 말도
    /// 안 하고 넘어간다. 모를 때는 어긋난 쪽으로 센다.
    #[test]
    fn an_unmeasured_pane_is_never_counted_as_unchanged() {
        assert_ne!(
            pane_account_fate(&fact(None), ""),
            PaneAccountFate::Unchanged
        );
        let i = account_switch_impact(&[fact(None)], "");
        assert_eq!(i.unmeasured, 1);
        assert!(i.needs_confirm());
    }

    /// 보고 있는 pane 은 ⟳ 칩만 달리고 자동으로 안 끊긴다 — busy 여부보다 먼저다.
    #[test]
    fn the_pane_you_are_watching_is_only_chipped() {
        let mut f = fact(Some("/v/acct-2"));
        f.focused = true;
        f.busy = true;
        assert_eq!(pane_account_fate(&f, ""), PaneAccountFate::ChipFocused);
        let mut c = fact(Some("/v/acct-2"));
        c.closed = true;
        c.busy = true;
        assert_eq!(pane_account_fate(&c, ""), PaneAccountFate::ChipClosed);
    }

    /// `Workspace.active_pane`은 바깥 leaf이고 계정 전환 map은 탭 pid를 쓴다. 그대로
    /// 비교하면 보이는 Codex 탭이 쉬는 순간 자동 재시작돼, 사용자가 보는 대화가 사라진다.
    #[test]
    fn account_switch_protects_the_visible_tab_pid_not_its_outer_pane() {
        let mut ws = Workspace::default();
        ws.active_pane = Some("%outer".to_string());
        ws.panes.insert(
            "%outer".to_string(),
            PaneState {
                tabs: vec![
                    PaneTab {
                        pid: Some("%outer".to_string()),
                        ..Default::default()
                    },
                    PaneTab {
                        pid: Some("%codex-visible".to_string()),
                        ..Default::default()
                    },
                ],
                active_tab: 1,
                ..Default::default()
            },
        );
        let focused = account_switch_focused_tab(&ws);
        assert_eq!(focused.as_deref(), Some("%codex-visible"));

        let mut visible = fact(Some("/v/old-account"));
        visible.id = "%codex-visible".to_string();
        visible.focused = focused.as_deref() == Some(visible.id.as_str());
        assert_eq!(
            pane_account_fate(&visible, "/v/new-account"),
            PaneAccountFate::ChipFocused
        );
    }

    /// 칩만 다는 pane 으로는 묻지 않는다 — 아무 일도 안 당하므로 물으면 한 가지를
    /// 두 번 묻는 꼴이다.
    #[test]
    fn only_panes_that_actually_restart_trigger_the_question() {
        let mut focused = fact(Some("/v/acct-2"));
        focused.focused = true;
        let mut closed = fact(Some("/v/acct-2"));
        closed.closed = true;
        assert!(!account_switch_impact(&[focused, closed], "").needs_confirm());

        assert!(!account_switch_impact(&[fact(Some(""))], "").needs_confirm());

        let mut busy = fact(Some("/v/acct-2"));
        busy.busy = true;
        assert!(account_switch_impact(&[busy], "").needs_confirm());
    }

    /// 「대화가 날아간다」 경고는 **되띄우는 pane** 에서만 켜져야 한다. 칩만 다는
    /// pane 이 그걸 켜면 아무 일도 안 당하는 pane 때문에 빨간 버튼이 뜬다.
    #[test]
    fn losing_the_conversation_is_counted_only_where_it_happens() {
        let mut chipped = fact(Some("/v/acct-2"));
        chipped.focused = true;
        chipped.resumable = false;
        assert_eq!(account_switch_impact(&[chipped], "").fresh, 0);

        let mut restarting = fact(Some("/v/acct-2"));
        restarting.resumable = false;
        assert_eq!(account_switch_impact(&[restarting], "").fresh, 1);
    }

    /// ⚠️ 문구가 사실과 갈리는 것을 막는 자물쇠. 일하는 pane 은 턴이 끝난 뒤에
    /// 되띄우므로 「작업이 끊긴다」는 **거짓말**이다.
    #[test]
    fn the_confirm_text_never_claims_work_gets_killed() {
        let i = AccountSwitchImpact {
            restart_when_quiet: 3,
            ..Default::default()
        };
        let (title, lines) = account_switch_confirm_text("지메일", &i);
        let sub = lines.join(" ");
        assert!(title.contains('3'));
        for lie in ["끊겨", "끊깁", "중단", "죽"] {
            assert!(!sub.contains(lie), "사실과 다른 문구: {sub}");
        }
        assert!(sub.contains("대화는 이어지지만"));
        // 없는 사정은 말하지 않는다.
        assert!(!sub.contains("새로 떠요"));
        assert!(!sub.contains("⟳"));

        let full = AccountSwitchImpact {
            restart_when_quiet: 1,
            restart_after_turn: 2,
            chip_focused: 1,
            fresh: 1,
            unmeasured: 1,
            ..Default::default()
        };
        let sub = account_switch_confirm_text("팀", &full).1.join(" ");
        assert!(sub.contains("턴이 끝난 뒤에"));
        assert!(sub.contains("새로 떠요"));
        assert!(sub.contains("⟳"));
        assert!(sub.contains("확인이 안 돼"));
    }

    /// 예측이 틀릴 때는 **많이 세는 쪽**으로 틀려야 한다 — 더 묻는 것은 안전하고
    /// 덜 묻는 것은 사고다.
    #[test]
    fn the_target_prediction_errs_toward_asking() {
        let vault = std::path::Path::new("/v/acct-2");
        assert_eq!(predicted_target_dir("", true, true, None), "");
        assert_eq!(predicted_target_dir("acct-2", true, true, Some(vault)), "");
        // 작업대를 못 쓰거나 금고가 껍데기면 갈아 끼우기가 안 되고 재시작으로만 반영된다.
        assert_eq!(
            predicted_target_dir("acct-2", false, true, Some(vault)),
            "/v/acct-2"
        );
        assert_eq!(
            predicted_target_dir("acct-2", true, false, Some(vault)),
            "/v/acct-2"
        );
    }

    /// 러너와 계산기가 같은 판정을 쓰는지. 활동 기록이 없는 pane 도 바쁜 것으로 친다.
    #[test]
    fn a_pane_with_no_activity_yet_is_treated_as_busy() {
        assert!(account_restart_busy(true, Some(("idle", false))));
        assert!(account_restart_busy(false, None));
        assert!(account_restart_busy(false, Some(("running", false))));
        assert!(account_restart_busy(false, Some(("idle", true))));
        assert!(!account_restart_busy(false, Some(("idle", false))));
    }
}
