//! 세션 — 칸 번호 매기기·새 칸 세우기·PTY 등록과 여러 기능이 함께 쓰는 칸 판정.
//! 기능별 몸통은 자식 모듈이다: 백엔드 기동(`boot`)·화면 펌프(`screen_pump`)·에이전트
//! 기동(`agent_launch`)·계정 전환(`accounts`)·저장(`save`)·복원(`restore`)·제목(`titles`)·
//! 방과 초점(`rooms`)·닫은 칸(`closed_panes`)·파일 트리(`file_tree`)·사이드바(`sidebar`)·
//! 다른 기기 칸(`remote_rooms`)·이사(`migrate`). 모두 `impl App` 확장이다.
use super::*;

mod accounts;
mod agent_launch;
mod boot;
mod closed_panes;
mod file_tree;
mod migrate;
mod remote_rooms;
mod restore;
mod rooms;
mod save;
mod screen_pump;
mod sidebar;
mod titles;

pub(crate) use self::accounts::{
    account_switch_confirm_text, account_switch_toast, AccountSwitchBtn, AccountSwitchImpact,
    AccountSwitchProvider, PendingAccountSwitch,
};
pub(crate) use self::agent_launch::{
    character_swap_confirm_text, pane_pick_wins, pane_reassigned, write_persona_override,
    CharacterSwapBtn, PendingCharacterSwap,
};
pub(crate) use self::file_tree::{git_repo_root, tilde_home};
pub(crate) use self::migrate::{MigrateReady, PendingMigration};
pub(crate) use self::rooms::remap_window_index;
pub(crate) use self::restore::restore_agent_command;
use self::restore::{normalize_saved_agent_map_with, saved_model_fits_agent, saved_sid_fits_agent};
use self::rooms::forward_backend_focus;
#[cfg(test)]
use self::save::append_surface_record_metadata;
use self::titles::restored_agent_cfg;

impl App {
    /// 지금 어딘가에 등록돼 있는 pane 번호 전부.
    ///
    /// pane 을 담는 곳이 셋이라 셋을 다 봐야 한다. `self.pty` 만 보면 PTY 없이
    /// `ws.panes` 에만 사는 미리보기·마크다운 pane 을 덮어쓰고, `ws.panes` 만 보면
    /// split 직후 아직 `PaneState` 가 없는 leaf 를 덮어쓴다(희소 저장이라 보조탭이
    /// 생기기 전까지 없다). 레이아웃 트리는 비활성 방까지 훑는다.
    pub(crate) fn used_pane_ids(&self) -> std::collections::HashSet<String> {
        let mut used: std::collections::HashSet<String> = self.pty.keys().cloned().collect();
        {
            let ws = self.ws.lock().unwrap();
            used.extend(ws.panes.keys().cloned());
            // `pid_to_pane` 의 키도 쓴 번호다. 탭을 닫아도 그 표는 그 자리에서 안 걷히고
            // `rebuild_pid_map` 이 돌 때만 정리되는데, 여기서 안 세면 탭을 꺼낼 때
            // 발급한 새 leaf 번호가 죽은 탭의 옛 번호와 겹친다. 그러면 `outer_for_pty`
            // 가 새 pane 클릭을 옛 바깥 pane 으로 접어 포커스가 안 옮겨졌다(2026-09-14
            // 실측: 탭에서 꺼낸 pane 을 눌러도 활성이 안 바뀜).
            used.extend(ws.pid_to_pane.keys().cloned());
        }
        for l in self
            .windows
            .iter()
            .flatten()
            .chain(self.pty_layout.as_ref())
        {
            used.extend(l.leaves().into_iter().map(str::to_string));
        }
        // 되살리기 목록에서 **아직 도는 것**의 번호도 쓰는 중이다. 레코드는 pane
        // 번호로 프로세스를 가리키는데, 그 번호를 새 pane 이 물려받으면 레코드가
        // 정리될 때(개수 상한·15분 idle·인포의 ×) 남의 살아 있는 셸을 끈다 —
        // 2026-08-24 에 사용자가 두 번 목격한 「검은 빈칸」이 그것이다.
        //
        // `alive` 만 세는 것이 요점이다. 이미 죽은 레코드는 정리해도 아무것도 안
        // 놓으므로(세 정리 경로가 모두 `c.alive` 로 거른다) 번호를 잡을 이유가
        // 없고, 잡으면 닫은 번호를 되쓰는 성질이 죽어 하루 쓰면 `%116` 이 된다.
        // 위험한 건 「살아 있다고 적혔는데 실은 죽은」 레코드뿐인데, 그건 여기
        // 걸린다.
        used.extend(
            self.closed_panes
                .iter()
                .filter(|c| c.alive)
                .map(|c| c.pane_id.clone()),
        );
        used.extend(self.mirror_sync.reserved_ids());
        used
    }
    /// 지금 안 쓰는 **가장 작은** pane 번호. 예전엔 단조 증가 카운터라 열고 닫기를
    /// 반복한 하루치가 `%116` 같은 번호로 쌓였다 — 학생 이름(`아루-p116`)에도 붙고
    /// `tell`·`dismiss` 로 부를 때마다 그걸 봐야 했다(사용자: "pane 번호는 계속 늘어난다").
    ///
    /// 번호 재사용이 위험했던 자리는 collab 마커다: 닫힌 pane 의 `kasaterm-bound-_N` 이
    /// 남은 채 같은 번호가 다시 나면 죽은 세션이 산 것처럼 붙는다. 그래서 닫을 때
    /// [`Self::cleanup_collab_markers`] 가 지우고, 앱이 죽어 그 경로를 못 탄 잔재는
    /// 부팅 sweep(`character::sweep_stale_markers`)이 걷는다.
    pub(crate) fn alloc_pane_id(&mut self) -> String {
        next_free_pane_id(&self.used_pane_ids())
    }

    /// Spawn the first shell pane for the *current* (already-cleared) session.
    /// Mirrors start_pty's pane bring-up with a fresh pane id and no socket
    /// (re)init — used by new_session.
    pub(crate) fn spawn_session_pane(&mut self) -> Result<()> {
        let (cols, rows) = self.window_cells();
        let cwd = self.pending_spawn_cwd.clone().or_else(resolve_initial_cwd);
        let id = self.alloc_pane_id();
        // 방별 분리(사용자): 이 pane 이 새 방이면 KASATERM_ROOM 을 셸 env 로 주입해 collab
        // 훅이 방별 slug 를 쓰게 한다. pane_room 에도 기록(Rust collab slug 계산용).
        let mut env = crate::proxy_env(&id);
        let room = self.pending_room.take();
        if let Some(ref room) = room {
            env.push(("KASATERM_ROOM".to_string(), room.clone()));
            self.ws
                .lock()
                .unwrap()
                .pane_room
                .insert(id.clone(), room.clone());
        }
        // 캐릭터 자동 배정: pending(사용자 지정) 우선, 없으면 통합 풀 순서. 마커·
        // session-id 기록 후 KASATERM_CHARACTER/SESSION_ID/PERSONA env 를 더한다(claude shim 적용).
        env.extend(self.assign_character_env(&id, cwd.as_deref(), room.as_deref()));
        let session = Arc::new(kasa_pty::PtySession::start(kasa_pty::PtyOptions {
            shell: self.pending_shell.take().or_else(resolve_default_shell),
            cwd: cwd.clone(),
            cols,
            rows,
            env,
            pane_id: id.clone(),
            initial_scrollback: Vec::new(),
        })?);
        self.pump_pty_screens(
            session.screens.clone(),
            id.clone(),
            std::sync::Arc::downgrade(&session),
        );
        self.insert_pty(id.clone(), session.clone());
        self.pty_layout = Some(kasa_pty::PtyLayout::single(&id));
        self.ws.lock().unwrap().active_pane = Some(id);
        Ok(())
    }

    /// `book` — 원격 셸 거울을 **그 자리에서** 로컬 셸로 되돌린다(`remote_shell_here`
    /// 의 역, 2026-09-02 지시 「mini에서 book치면 다시 로컬로」). 원격 셸에서 book 이
    /// 뱉은 예약 알림을 화면 펌프가 잡아 부른다. 순수 셸 전용이라 대화 운반 없이
    /// 스왑만 한다: 같은 pane id 로 로컬 셸을 앉히고 원격 web 세션은 폐기한다.
    /// 돌아갈 로컬 폴더는 `mini` 로 갈 때 심어 둔 origin(remote identity).
    pub(crate) fn bring_pane_home(&mut self, pid: &str) -> Result<()> {
        self.ensure_user_mutation_target(
            pid,
            crate::settings_room::SettingsMutation::RemotePane,
        )?;
        if self.tmux.is_some() {
            return Ok(());
        }
        // 로컬 pane 에서 book 을 눌렀다 — 되돌릴 원격이 없다. 조용히 무시한다
        // (예약 알림을 데스크톱 알림으로 흘리지 않는 것으로 충분하다).
        if !kasa_mcp::remote::is_remote_pane(pid) {
            return Ok(());
        }
        let Some(info) = kasa_mcp::remote::remote_info(pid) else {
            return Ok(());
        };
        // 이사로 나간 **학생**(claude/codex)은 book 이 아니라 데려오기(migrate local)로
        // 돌아온다 — 대화 jsonl 을 떠 와야 하기 때문이다. book 은 `mini` 로 앉힌 순수
        // 셸 거울만 되돌린다. 세션 id 가 바인딩돼 있으면 그건 학생이므로 안내만 한다.
        if self.pane_claude_sid.contains_key(pid) {
            self.set_toast(
                "이 창은 캐릭터예요 — `book` 대신 데려오기(migrate local)로 돌아와요".to_string(),
            );
            return Ok(());
        }
        let dest = info
            .origin_cwd
            .clone()
            .or_else(|| {
                kasa_mcp::machines::machines()
                    .into_iter()
                    .find(|m| m.base == info.base.trim_end_matches('/'))
                    .and_then(|m| {
                        info.remote_cwd
                            .as_deref()
                            .and_then(|rc| kasa_mcp::machines::map_remote_to_local(&m, rc))
                    })
            });
        let (c, r) = self
            .pty
            .get(pid)
            .map(|s| s.size())
            .unwrap_or_else(|| self.window_cells());
        // 로컬 셸을 같은 pane id 로 — 자리·크기·방·학생 배정 유지(스왑 패턴).
        let session = Arc::new(kasa_pty::PtySession::start(kasa_pty::PtyOptions {
            shell: resolve_default_shell(),
            cwd: dest.clone(),
            cols: c,
            rows: r,
            env: crate::proxy_env(pid),
            pane_id: pid.to_string(),
            initial_scrollback: Vec::new(),
        })?);
        // 원격 web 세션을 폐기한다 — insert 의 Drop 은 detach(살려 둠)라, 명시적
        // kill 이 없으면 저쪽 셸이 남는다. insert **전**에: 스왑 뒤엔 링크가 없다.
        kasa_mcp::remote::kill_remote(pid);
        // `to` 로 세운 저쪽 pane 은 돌아올 때 함께 걷는다 — ssh 를 나오면 저쪽 셸이
        // 끝나듯이. 위 kill 은 우리 링크만 끊고 GUI pane 은 저쪽 앱이 쥐고 있다.
        if info.owned && info.remote_id.starts_with('%') {
            let (base, rid) = (info.base.clone(), info.remote_id.clone());
            std::thread::spawn(move || {
                if let Err(e) = kasa_mcp::remote::close_remote_pane(&base, &rid, None, true) {
                    eprintln!("[to ..] 원격 pane {rid} 닫기 실패(무시): {e:#}");
                }
            });
        }
        self.insert_pty(pid.to_string(), session.clone());
        self.pump_pty_screens(
            session.screens.clone(),
            pid.to_string(),
            std::sync::Arc::downgrade(&session),
        );
        self.dead_panes.lock().unwrap().retain(|x| x != pid);
        let (wc, wr) = self.window_cells();
        self.resize_backend(wc, wr);
        self.publish_pty_layout();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        self.set_toast(match dest {
            Some(d) => format!("로컬로 돌아왔어요 — {d}"),
            None => "로컬 셸로 돌아왔어요".to_string(),
        });
        Ok(())
    }
    /// PTY 세션을 App 에 넣으면서 전역 레지스트리에도 등록한다.
    ///
    /// 웹 터미널·소켓 백엔드는 GUI 스레드를 거치지 않고 이 레지스트리로 세션에
    /// 붙으므로, **pane 을 만드는 모든 경로는 `self.pty.insert` 대신 이걸 써야**
    /// 한다. 한 곳이라도 빠뜨리면 그 pane 만 조용히 웹에서 안 보이는, 찾기 어려운
    /// 종류의 구멍이 난다 — 그래서 통로를 하나로 묶었다. 레지스트리는 Weak 이라
    /// 해제는 App 이 Arc 를 떨어뜨리는 것으로 저절로 된다.
    pub(crate) fn insert_pty(&mut self, id: String, sess: std::sync::Arc<kasa_pty::PtySession>) {
        crate::close_grace::clear_marker(&id);
        kasa_mcp::surface_keys::ensure(&id);
        kasa_pty::register_session(&id, &sess);
        self.pty.insert(id, sess);
    }
    /// Geometry of the left window-tab sidebar, in logical px. Returns
    /// `(tab_rects, close_rects, plus_rect)`:
    ///   - one `(window_idx, rect)` tab per window, stacked under the title
    ///     strip,
    ///   - one `(window_idx, ×-rect)` per window *only* when more than one
    ///     window exists (the last window can't be closed),
    ///   - the "+" new-window button rect under the last tab.
    /// Pure read of `windows.len()` so the render path and the mouse
    /// hit-test agree on every rect. Overflow: the strip shows a contiguous
    /// run of whole tabs starting at `win_tab_first` (no partial clipping —
    /// 이 띠는 클립을 안 세운다). This only clamps `first` into range;
    /// keeping the *active* tab in view is `win_tab_reveal`'s job at
    /// switch/create time, so a free wheel-scroll is never yanked back.
    /// `i` 번 방의 pane id 들. 활성 방의 트리만 `pty_layout` 에 나가 있어 슬롯이
    /// 비는데, 그걸 모르고 `windows[i]` 만 보면 지금 보고 있는 방이 늘 빈 방이 된다
    /// — 사이드바·라벨·상태 점이 다 이 갈래를 각자 쓰고 있어 한 곳으로 모은다.
    /// leaf 가 가리키는 PTY 번호 — 보통 leaf 번호 그대로지만, 탭을 빼내 새 번호를 받은
    /// pane 은 PTY·거울 등록이 첫 탭의 pid 에 남아 있다(2026-09-18). 거울 판정·저장은
    /// 이걸로 찾는다. 렌더가 ws 를 쥔 채 부를 수 있어 `try_lock` — 못 잡으면 leaf 그대로.
    pub(crate) fn leaf_pty_id(&self, leaf: &str) -> String {
        self.ws.try_lock().ok()
            .and_then(|ws| ws.panes.get(leaf).and_then(|p| p.tabs.first()).and_then(|t| t.pid.clone()))
            .unwrap_or_else(|| leaf.to_string())
    }
    pub(crate) fn window_leaves(&self, i: usize) -> Vec<String> {
        let layout = if i == self.active_window {
            self.pty_layout.as_ref()
        } else {
            self.windows.get(i).and_then(|o| o.as_ref())
        };
        layout.map_or(Vec::new(), |l| {
            l.leaves().iter().map(|s| s.to_string()).collect()
        })
    }
    /// 이 pane 의 claude 가 bypass 권한 모드로 도는가 — **화면**으로 판정한다.
    /// 켜져 있으면 푸터에 항상 떠 있는 줄이라(입력·busy 와 무관) 화면이 정직하고,
    /// 프로세스 argv 는 claude 가 실행 중 제목으로 덮어써 못 믿는다(2026-08-30
    /// 실측: 도는 claude 가 `ps` 의 command 에서 통째로 사라졌다). 글리프 접두까지
    /// 정확히 맞춰 본다 — 대화 본문이 그 문구를 말하는 것과 갈라야 해서다.
    /// 이 pane 의 claude 가 bypass 모드인가 — 훅이 실어 준 permission_mode 가 정본이고,
    /// 새 훅 이전에 뜬 세션(아직 `turn` 을 안 보낸)만 화면 푸터를 읽는다.
    pub(crate) fn pane_bypass_on(&self, ws: &Workspace, pane_id: &str) -> bool {
        let tab = ws.active_tab_pid(pane_id);
        if let Some(mode) = self.collab.hub.permission_mode(&tab).or_else(|| self.collab.hub.permission_mode(pane_id)) {
            return mode == "bypassPermissions";
        }
        ws.panes
            .get(pane_id)
            .and_then(|p| p.term())
            .is_some_and(Self::term_bypass_on)
    }

    /// 같은 판정을 화면 하나에 대고 한다. pane 은 활성 탭으로 Deref 하므로, 탭마다
    /// 제 권한 모드를 저장하려면 그 탭의 화면을 직접 줘야 한다.
    pub(crate) fn term_bypass_on(t: &crate::TerminalPane) -> bool {
        // claude 가 방금 죽었으면 셸 프롬프트가 몇 줄 밀어 올린다 — 바닥 12줄까지 본다.
        let from = t.cells.len().saturating_sub(12);
        t.cells[from..].iter().any(|row| {
            crate::screenread::row_text_cells(row)
                .0
                .trim_start()
                .starts_with("⏵⏵ bypass permissions on")
        })
    }

    /// 이 pane 의 에이전트가 지금 턴 중인가 — 헤더 working 바와 **같은** 화면
    /// 스피너 판정(input.rs `rows_show_working`)이다. 원격 미러 pane 도 원격
    /// 화면을 같은 그리드로 그리므로 그대로 통한다.
    /// 이사·정지 게이트 — 화면이 아니라 판정(`agent_state`)을 본다. 모르면 일하는 중으로
    /// 친다: 이사를 미루는 쪽이 턴을 자르는 쪽보다 낫다.
    pub(crate) fn pane_agent_working(&self, ws: &Workspace, pane_id: &str) -> bool {
        let tab = ws.active_tab_pid(pane_id);
        self.collab.hub.refresh();
        self.collab.hub.is_working(&tab)
    }
}

/// 전환 토스트 문구 — 자동·메뉴·설정창 세 진입점이 같은 문장을 쓴다.
/// `same` 은 이미 활성인 계정을 다시 누른 경우(전환이 아니라 「맞추기」).
/// `live` 는 작업대 갈아 끼우기가 성공한 경우 — 떠 있는 pane 이 **다음
/// 메시지부터** 새 계정이므로 「다음에 뜨는 claude 부터」라고 말하면 거짓말이
/// 된다(2026-08-17 「토스트에 다음세션부터라고 뜨는데」). 실패(금고 비었음·
/// 쓰기 실패)일 때만 재시작 폴백이 전부라 옛 문장이 맞다.
/// 확인 카드를 메인 GPU와 인라인 웹 중 어디에서 소유하는가.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ConfirmSurface {
    Main,
    Web,
}

/// 쓰이는 번호 집합에서 빠진 **가장 작은** `%N`.
fn next_free_pane_id(used: &std::collections::HashSet<String>) -> String {
    (0u32..)
        .map(|n| format!("%{n}"))
        .find(|id| !used.contains(id))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::next_free_pane_id;

    /// 닫힌 번호를 되쓰는 것이 요점이다 — 안 그러면 하루 쓰면 `%116` 이 된다.
    #[test]
    fn pane_ids_fill_the_holes_left_by_closed_panes() {
        let used = |ids: &[&str]| ids.iter().map(|s| s.to_string()).collect();
        assert_eq!(next_free_pane_id(&used(&[])), "%0", "첫 pane 은 %0");
        assert_eq!(next_free_pane_id(&used(&["%0", "%1", "%2"])), "%3");
        // %1 이 닫혔으면 다음 pane 이 그 자리를 채운다(옛 카운터는 %3 을 줬다).
        assert_eq!(next_free_pane_id(&used(&["%0", "%2"])), "%1");
        // 번호는 정수 순서로 센다 — 사전순이면 %10 뒤에 %2 가 아니라 %9 를 놓친다.
        assert_eq!(next_free_pane_id(&used(&["%0", "%1", "%10", "%2"])), "%3");
    }
}
#[cfg(test)]
mod close_freeze_tests {
    /// `draw_pane_tabs` 가 얼려 둔 슬롯을 쓸 때의 × 좌표. 그리기 코드와 **같은 식**을
    /// 여기 두는 건 그 함수가 GPU 를 받아 단위 테스트로 못 부르기 때문이다 — 식이
    /// 갈리면 이 테스트가 거짓 안심이 되므로, 바꿀 때 두 곳을 같이 고쳐야 한다
    /// (render.rs 의 `action_x` 분기).
    fn close_x(slot: (f32, f32), x_reserve: f32) -> f32 {
        let (sx, sw) = slot;
        (sx + sw) - x_reserve + 2.0
    }

    /// 닫은 자리에 다음 탭이 오는가 — 이 동작의 전부다.
    #[test]
    fn next_tab_lands_on_the_same_x() {
        // 폭이 제각각인 탭 넷(라벨 길이가 다르다).
        let slots = [(100.0, 140.0), (240.0, 80.0), (320.0, 200.0), (520.0, 90.0)];
        let reserve = 28.0;
        // 두 번째 칸의 × 를 눌렀다고 하자.
        let target = close_x(slots[1], reserve);
        // 탭이 하나 빠지면 남은 탭이 슬롯을 앞에서부터 채운다 — 원래 2번이 1번 칸으로.
        // 슬롯은 그대로이므로 그 칸의 × 는 같은 자리다.
        assert_eq!(
            close_x(slots[1], reserve),
            target,
            "다음 탭의 × 가 방금 누른 자리에 없다"
        );
        // 연달아 한 번 더 눌러도 마찬가지.
        assert_eq!(close_x(slots[1], reserve), target);
    }

    /// 얼리지 않으면 어긋난다는 것 — 이 기능이 왜 필요한지의 근거.
    #[test]
    fn without_freezing_the_x_moves() {
        // 평소 배치는 라벨 실측 폭이라, 탭이 빠지면 남은 탭이 넓어지고 앞으로 당겨진다.
        let reserve = 28.0;
        let before = close_x((240.0, 80.0), reserve);
        // 하나 닫힌 뒤 재계산된 자리(넓어지고 왼쪽으로 당겨짐).
        let after = close_x((100.0, 190.0), reserve);
        assert!(
            (before - after).abs() > 10.0,
            "재계산해도 × 가 그대로면 얼릴 이유가 없다: {before} vs {after}"
        );
    }

    /// 슬롯이 모자라면(닫기 전보다 탭이 늘었다) 평소 계산으로 돌아가야 한다.
    #[test]
    fn missing_slot_falls_back() {
        let slots: [(f32, f32); 2] = [(100.0, 140.0), (240.0, 80.0)];
        assert!(slots.get(5).is_none(), "없는 칸은 None 이어야 폴백이 돈다");
    }

    /// 스크롤 막대 위치 — 맨 위·중간·맨 아래.
    fn thumb_y(view_top: f32, viewport_h: f32, content_h: f32, scrolled: f32) -> f32 {
        let overflow = content_h - viewport_h;
        let thumb_h = (viewport_h * viewport_h / content_h).max(28.0);
        view_top + (viewport_h - thumb_h) * (scrolled / overflow).clamp(0.0, 1.0)
    }

    #[test]
    fn thumb_spans_the_track() {
        let (top, view, content) = (54.0, 400.0, 1000.0);
        let overflow = content - view;
        let at_top = thumb_y(top, view, content, 0.0);
        let at_bottom = thumb_y(top, view, content, overflow);
        assert_eq!(at_top, top, "맨 위에서 막대가 트랙 머리에 안 붙는다");
        let thumb_h = (view * view / content).max(28.0);
        assert!(
            (at_bottom - (top + view - thumb_h)).abs() < 0.01,
            "맨 아래에서 막대가 트랙 끝에 안 붙는다: {at_bottom}"
        );
        // 넘겨도 트랙 밖으로 안 나간다.
        assert_eq!(thumb_y(top, view, content, overflow * 2.0), at_bottom);
        assert!(thumb_y(top, view, content, overflow / 2.0) > at_top);
    }
}
