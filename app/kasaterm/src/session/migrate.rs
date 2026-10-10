//! 칸 이사 — 도는 칸을 산 채로 다른 기기(또는 이 기기 ptyd)로 옮기고 되돌린다. 턴 중이면
//! 예약해 두었다가 턴이 끝나면 옮긴다.
use super::*;

/// 이사 예약 한 건 — 학생이 턴 중일 때 이사를 걸면 여기 앉는다(migrate_pane 의
/// working 관문). `base == "local"` 은 역이사(데려오기)다. `idle_since` 는 스피너가
/// 꺼진 첫 관측 시각 — 짧은 틈을 턴 끝으로 오판하지 않게 몇 초 이어져야 발사한다
/// (run_pending_migrations).
pub(crate) struct PendingMigration {
    pub(crate) pane: String,
    pub(crate) base: String,
    pub(crate) cwd: Option<String>,
    pub(crate) force: bool,
    pub(crate) run: Option<String>,
    pub(crate) idle_since: Option<std::time::Instant>,
    pub(crate) transfer: Option<kasa_socket::transfer::MigrateRequest>,
}

pub(super) fn ensure_migration_slot(queue: &[PendingMigration], pane: &str) -> Result<()> {
    if queue.iter().any(|pending| pending.pane == pane) {
        anyhow::bail!("이 세션에는 이미 예약된 이사가 있어요. 기존 요청이 끝난 뒤 다시 시도해 주세요");
    }
    Ok(())
}

pub(super) fn ensure_transfer_agent(expected: Option<u32>, current: Option<u32>) -> Result<()> {
    if expected != current {
        anyhow::bail!("이사 준비 중 세션 프로세스가 바뀌었어요. 현재 작업을 유지하고 이사를 중단했어요");
    }
    Ok(())
}

impl App {
    /// `to` 의 뒤처리. 명령이 저쪽에 닿을 시간이 지나면 원래 자리를 걷고(저쪽 pane 은 남긴다)
    /// 그 기기 방을 보기 창으로 연다 — 거울을 원래 방에 남기면 기기 절과 중복이고, 번호가
    /// 재사용되면 남으로 둔갑했다(2026-09-17).
    pub(crate) fn tick_migrate_handoff(&mut self) {
        if self.migrate_handoff.is_none() {
            self.sweep_migrated_links();
            return;
        }
        if self.migrate_handoff.as_ref().is_none_or(|h| h.at > Instant::now()) {
            return;
        }
        let Some(h) = self.migrate_handoff.take() else { return };
        if self.pty.contains_key(&h.pid) {
            self.remote_keep.insert(h.pid.clone());
            self.remove_pane(&h.pid);
        }
        if let Err(e) = self.seat_remote_view_window(&h.label, &h.base, &h.remote_id, false, None) {
            self.set_toast(format!("{} 의 방을 보기 창으로 못 열었어요 — {e:#}", h.label));
        }
    }

    /// 예전 `to` 가 남긴 거울 — 재시작 때 세션에서 되살아나 이쪽 방에 그대로 앉는다. 그 기계
    /// 목록에 그 pane 이 보이면(캐시가 찼으면) 그 방을 보기 창으로 열고 이 자리를 걷는다.
    /// 한 번에 하나만 — 여럿이면 다음 틱에 이어서. 복원이 도는 동안은 손대지 않는다.
    pub(super) fn sweep_migrated_links(&mut self) {
        // 복원 카드가 남아 있어도(조용한 링크는 준비로 안 잡히기도 한다) 링크가 붙어 있으면
        // 걷는다 — 배치를 세우는 중(`restore_applying`)만 피한다.
        if self.tmux.is_some() || self.restore_applying.is_some() {
            return;
        }
        let candidate = self.pty.keys().find_map(|id| {
            let info = kasa_mcp::remote::remote_info(id)?;
            if info.view || info.owned || !info.remote_id.starts_with('%') { return None; }
            if !kasa_mcp::remote::connection_readiness(id).is_some_and(|(connected, _, _)| connected) { return None; }
            let window = self.window_of_pane(id)?;
            if self.remote_view_of_window(window).is_some() { return None; }
            let (label, facts) = crate::machinescol::remote_pane_facts(id)?;
            let source_window = facts.get("window").and_then(|v| v.as_u64())?;
            let room = crate::machinescol::remote_room(&facts);
            Some((id.clone(), label, source_window, room, info.remote_id))
        });
        let Some((id, label, source_window, room, remote_id)) = candidate else { return };
        // 보기 창부터 연다 — 못 열면 이 링크가 아직 손잡이다.
        if let Err(e) = self.open_remote_room(&label, Some(source_window), &room, Some(&remote_id)) {
            eprintln!("[migrate] leftover mirror {id} → {label} view failed: {e:#}");
            return;
        }
        self.remote_keep.insert(id.clone());
        self.remove_pane(&id);
        self.set_toast(format!("{label} 로 간 자리를 걷고 그 방을 보기 창으로 열었어요"));
    }

    /// 로컬 상주 PTY 데몬(kasa-serve-web)을 보장한다 — 승격된 학생의 새 집.
    ///
    /// 판정은 입양 소켓으로 한다: HTTP 포트는 남이 차지할 수 있지만 입양 소켓
    /// 파일에 연결이 되는 것은 우리 데몬뿐이다. 없으면 바이너리를 찾아 띄운다 —
    /// ①KASATERM_SERVE_WEB_BIN ②현재 exe 옆 ③PATH.
    #[cfg(unix)]
    pub(super) fn ensure_local_ptyd(&mut self) -> Result<()> {
        let sock = kasa_mcp::adopt::adopt_sock_path(LOCAL_PTYD_PORT);
        if std::os::unix::net::UnixStream::connect(&sock).is_ok() {
            return Ok(());
        }
        let bin = std::env::var("KASATERM_SERVE_WEB_BIN")
            .ok()
            .map(std::path::PathBuf::from)
            .filter(|p| p.exists())
            .or_else(|| {
                std::env::current_exe()
                    .ok()
                    .and_then(|e| e.parent().map(|d| d.join("kasa-serve-web")))
                    .filter(|p| p.exists())
            })
            .unwrap_or_else(|| std::path::PathBuf::from("kasa-serve-web"));
        let lp = std::env::temp_dir().join(format!("kasa-ptyd-{LOCAL_PTYD_PORT}.log"));
        let logf = || {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&lp)
                .ok()
        };
        let mut cmd = std::process::Command::new(&bin);
        cmd.arg("--port")
            .arg(LOCAL_PTYD_PORT.to_string())
            .arg("--cwd")
            .arg(kasa_socket::home_var().unwrap_or_else(|_| "/".into()))
            .stdin(std::process::Stdio::null())
            .stdout(
                logf()
                    .map(std::process::Stdio::from)
                    .unwrap_or_else(std::process::Stdio::null),
            )
            .stderr(
                logf()
                    .map(std::process::Stdio::from)
                    .unwrap_or_else(std::process::Stdio::null),
            );
        cmd.spawn()
            .map_err(|e| anyhow::anyhow!("로컬 PTY 데몬을 못 띄웠어요: {} ({e})", bin.display()))?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if std::os::unix::net::UnixStream::connect(&sock).is_ok() {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
        anyhow::bail!(
            "로컬 PTY 데몬이 5초 안에 안 떴어요 (로그: {})",
            lp.display()
        )
    }

    /// 도는 pane 을 **재시작 없이** 로컬 상주 데몬으로 승격한다 — 셸·claude
    /// 프로세스는 그대로 두고 PTY 소유권만 fd(SCM_RIGHTS)로 넘긴 뒤, 이 pane 을
    /// 그 세션의 원격 소유자 연결로 갈아끼운다. 이후 앱을 굽고 껐다 켜도, 앱이
    /// 죽어도 그 학생은 살아남는다(맥북 전원이 꺼지면 같이 꺼진다 — 그건 물리).
    #[cfg(unix)]
    pub(crate) fn promote_pane(&mut self, pid: &str) -> Result<String> {
        self.ensure_user_mutation_target(
            pid,
            crate::settings_room::SettingsMutation::RemotePane,
        )?;
        let Some(sess) = self.pty.get(pid).cloned() else {
            anyhow::bail!("pane {pid} 이 없다");
        };
        if kasa_mcp::remote::is_remote_pane(pid) {
            anyhow::bail!("{pid} 은 이미 원격 pane 이다");
        }
        let Some(fd) = sess.master_raw_fd() else {
            anyhow::bail!("{pid} 은 로컬 PTY 가 아니라 승격할 수 없다");
        };
        self.ensure_local_ptyd()?;
        // reader 를 세우고 마지막 청크가 Term 에 앉을 시간을 준다 — 그 뒤에 뜬
        // 스크롤백이 곧 「넘어가는 화면」이다. 정지 순간 escape 가 반 토막 날 수
        // 있지만 TUI 는 계속 다시 그리므로 스스로 아문다(adopt 머리말).
        sess.stop_reader();
        std::thread::sleep(std::time::Duration::from_millis(450));
        let (c, r) = sess.size();
        let scroll = sess.scrollback_text(2000);
        let web_id =
            kasa_mcp::adopt::handoff_to(LOCAL_PTYD_PORT, fd, sess.shell_pid(), c, r, scroll)?;
        sess.disarm_kill();
        // 같은 pane id 로 소유자 연결 — pane 자리·이름·학생 배정은 그대로다.
        let remote = kasa_mcp::remote::connect(
            kasa_mcp::remote::RemoteSpec {
                base: format!("http://127.0.0.1:{LOCAL_PTYD_PORT}"),
                pane: Some(web_id.clone()),
                cwd: None,
                token: None,
                identity: Default::default(),
            },
            pid,
            c,
            r,
        )?;
        self.insert_pty(pid.to_string(), remote.session.clone());
        self.pump_pty_screens(
            remote.session.screens.clone(),
            pid.to_string(),
            std::sync::Arc::downgrade(&remote.session),
        );
        // 옛 세션의 늦은 죽음표시 정리(스왑 패턴) — 정체 가드가 있지만 이중으로.
        self.dead_panes.lock().unwrap().retain(|x| x != pid);
        let (wc, wr) = self.window_cells();
        self.resize_backend(wc, wr);
        self.publish_pty_layout();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        Ok(web_id)
    }

    /// pane 의 claude 를 **다른 기계로 이사**시킨다 — 로컬을 정리하고 원격에서 같은
    /// 대화로 다시 깨운다(promote 와 달리 산 채로는 못 건넌다 — 기계가 다르다).
    ///
    /// 순서: ①저쪽 폴더 확인(없으면 홈) ②claude 를 곱게 끄고(jsonl 마지막 flush 를
    /// 기다린다) ③대화 jsonl 을 원격 호스트로 업로드 ④저쪽에 학생 pane 소환
    /// ⑤같은 pane 자리를 원격 셸로 갈아끼우고 `claude --resume` 을 주입. 로컬
    /// 셸은 스왑의 Drop 이 정상 철거한다.
    ///
    /// **세션 파일만 옮긴다.** 레포 준비·미push 커밋 bundle·테마 동행은 2026-09-14
    /// 에 걷었다(사용자 지시 「세션 파일만 옮기고 대화 이어가게만」) — 코드 맞추기는
    /// git 이 할 일이고, 이사가 그걸 대신하다 큰 짐·관문으로 번번이 멈췄다. 저쪽에
    /// 같은 폴더가 없으면 홈에서 대화만 잇는다.
    ///
    /// ①~④는 **워커 스레드**가 돌고 단계마다 `UserEvent::MigrateStage` 로 보고한다
    /// — Info 「다른 기계」 줄 밑에 체크리스트로 선다(2026-09-07 지시). 전에는
    /// 여기서 동기로 돌아 업로드가 큰 대화면 앱 전체가 굳고 토스트 한 줄만 남았다.
    /// ⑤만 GUI 스레드 몫이라 `MigrateDone` 을 받은 `migrate_finish` 가 한다. 이
    /// 함수는 검사·재료 수집만 하고 바로 돌아온다 — 반환 문구는 「시작됨」이지
    /// 「끝남」이 아니다.
    #[cfg(unix)]
    pub(crate) fn migrate_pane(
        &mut self,
        pid: &str,
        base: &str,
        remote_cwd: Option<&str>,
        force: bool,
        run: Option<&str>,
    ) -> Result<String> {
        self.migrate_pane_with_reply(pid, base, remote_cwd, force, run, None)
    }

    /// `migrate_pane` + 끝났을 때 답을 받을 창구. 소켓(CLI·보드·학생 셀프 이사)은
    /// 최종 결과(원격 pane id)를 기다리므로 그 창구를 진행 상태에 맡겨 두고,
    /// `migrate_finish` 가 끝에서 보낸다. 예약(턴 중)·검사 실패는 여기서 바로 답한다.
    #[cfg(unix)]
    pub(crate) fn migrate_pane_with_reply(
        &mut self,
        pid: &str,
        base: &str,
        remote_cwd: Option<&str>,
        force: bool,
        run: Option<&str>,
        reply: Option<std::sync::mpsc::Sender<std::result::Result<String, String>>>,
    ) -> Result<String> {
        self.migrate_pane_with_destination(pid, base, remote_cwd, force, run, reply, None)
    }

    #[cfg(unix)]
    pub(crate) fn start_transfer_migration(
        &mut self,
        request: &kasa_socket::transfer::MigrateRequest,
        reply: Option<std::sync::mpsc::Sender<std::result::Result<String, String>>>,
    ) -> Result<String> {
        self.validate_transfer_identity(&request.session)?;
        let source = self.pty.get(&request.session.pane_id)
            .ok_or_else(|| anyhow::anyhow!("출발 세션이 사라졌어요"))?;
        if source.active_agent().is_none() {
            let shell = source.shell_pid().ok_or_else(|| anyhow::anyhow!("셸 상태를 확인하지 못했어요"))?;
            let command = socket::foreground_proc_name(shell)
                .ok_or_else(|| anyhow::anyhow!("실행 중인 프로그램을 확인하지 못했어요"))?;
            if !matches!(command.trim_start_matches('-'), "zsh" | "bash" | "fish" | "sh" | "dash" | "ksh" | "tcsh") {
                anyhow::bail!("다른 프로그램이 실행 중이라 이사할 수 없어요");
            }
        }
        let target = kasa_mcp::machines::find_route(&format!("~{}", request.destination_machine))
            .ok_or_else(|| anyhow::anyhow!("도착 기계를 이쪽 명부에서 찾지 못했어요"))?;
        let cwd = self.pane_current_cwd(&request.session.pane_id)
            .ok_or_else(|| anyhow::anyhow!("현재 작업 폴더를 확인하지 못했어요"))?;
        let remote_cwd = kasa_mcp::machines::map_local_to_remote(&target, &cwd.to_string_lossy())
            .ok_or_else(|| anyhow::anyhow!("도착 기기의 작업 폴더 대응 규칙이 없어요"))?;
        self.migrate_pane_with_destination(
            &request.session.pane_id, &target.base, Some(&remote_cwd), false, None,
            reply, Some(request.clone()),
        )
    }

    #[cfg(unix)]
    pub(super) fn migrate_pane_with_destination(
        &mut self,
        pid: &str,
        base: &str,
        remote_cwd: Option<&str>,
        force: bool,
        run: Option<&str>,
        reply: Option<std::sync::mpsc::Sender<std::result::Result<String, String>>>,
        transfer: Option<kasa_socket::transfer::MigrateRequest>,
    ) -> Result<String> {
        self.ensure_user_mutation_target(
            pid,
            crate::settings_room::SettingsMutation::Migrate,
        )?;
        ensure_migration_slot(&self.migrate_queue, pid)?;
        if self.migrate_running_any() {
            anyhow::bail!("이사가 이미 도는 중이다 — 끝나면 다시");
        }
        let Some(sess) = self.pty.get(pid).cloned() else {
            anyhow::bail!("pane {pid} 이 없다");
        };
        if kasa_mcp::remote::is_remote_pane(pid) {
            anyhow::bail!("{pid} 은 이미 원격 pane 이다 — 이사할 로컬이 없다");
        }
        let Some(shell) = sess.shell_pid() else {
            anyhow::bail!("{pid} 의 셸 pid 를 모른다");
        };
        // 에이전트가 **없는** 셸 pane 이면 이사가 아니라 **그 기계 태생 스폰**이다
        // — 옮길 대화가 없으니 레포만 맞추고 저쪽에 진짜 학생을 앉힌 뒤 이 pane 을
        // 거울로 바꾼다(2026-08-30 지시 「claude mini 이렇게 키면 맥미니에서 켜게」).
        let agent = kasa_pty::agent_pid_for_shell(&kasa_pty::process_table_shared(), shell);
        if let Some((kind, _)) = &agent {
            if !matches!(
                kind,
                kasa_pty::AgentKind::Claude | kasa_pty::AgentKind::Codex
            ) {
                anyhow::bail!(
                    "이사는 claude·codex 전용이다({kind:?} 는 대화 파일 규약이 다르다)"
                );
            }
        }
        let is_codex = matches!(&agent, Some((kasa_pty::AgentKind::Codex, _)));
        let fresh = agent.is_none();
        // 턴 중 이사는 하던 일을 자른다 — SIGTERM 이 진행 중 턴을 버리고, 옮겨간
        // 학생은 하다 만 채로 앉는다(2026-08-31 지적 「이사하고 작업이 끊겨」).
        // 일하는 학생은 죽이지 않고 예약한다: 턴이 끝나면(스피너가 꺼지고 잠시
        // 이어지면) 틱(run_pending_migrations)이 이 함수를 다시 부른다. force 는
        // 「끊겨도 지금 당장」으로 통과. 학생이 스스로 신청하는 셀프 이사는 그 CLI
        // 호출 자체가 턴 중이라 언제나 이 길로 온다 — 즉답 받고 제 턴을 마치면 간다.
        if !fresh && !force {
            let working = {
                let ws = self.ws.lock().unwrap();
                self.pane_agent_working(&ws, pid)
            };
            if working {
                self.migrate_queue.push(PendingMigration {
                    pane: pid.to_string(),
                    base: base.to_string(),
                    cwd: remote_cwd.map(str::to_string),
                    force,
                    run: run.map(str::to_string),
                    idle_since: None,
                    transfer: transfer.clone(),
                });
                self.set_toast(format!("{pid} 는 지금 일하는 중 — 턴이 끝나면 이사간다"));
                return Ok(format!("예약됨 — {pid} 가 하던 턴을 마치면 이사간다"));
            }
        }
        let sid = if fresh {
            None
        } else {
            // codex 도 같은 지도에 실린다 — statusline 바인딩이 하네스 불문이라
            // pane_claude_sid 가 codex pane 에선 rollout uuid 를 쥔다.
            Some(self.pane_claude_sid.get(pid).cloned().ok_or_else(|| {
                anyhow::anyhow!(
                    "{pid} 의 세션 id 를 아직 모른다 — 대화를 한 번 주고받은 뒤 다시"
                )
            })?)
        };
        let cwd = socket::pid_cwd(shell)
            .ok_or_else(|| anyhow::anyhow!("{pid} 의 작업 폴더를 못 읽었다"))?;
        let remote_cwd = remote_cwd
            .map(str::to_string)
            .unwrap_or_else(|| cwd.to_string_lossy().into_owned());
        // 모델·effort 는 claude 가 죽기 전에 떠 둔다 — 원천이 statusline 보고라
        // 프로세스가 사라지면 다음 스냅샷에서 빠질 수 있다.
        let (model, effort) = self
            .agent_cfg_snapshot()
            .get(pid)
            .cloned()
            .unwrap_or_default();
        let jsonl = match &sid {
            None => None,
            Some(_) if is_codex => None, // codex 는 아래 rollout 운반이 대신 간다
            Some(sid) => Some(
                kasa_socket::sessions::session_jsonl_path(&cwd, sid)
                    .filter(|p| p.exists())
                    .or_else(|| socket::transcript_path_for_session(sid))
                    .ok_or_else(|| anyhow::anyhow!("세션 {sid} 의 대화 파일을 못 찾았다"))?,
            ),
        };
        // codex 의 대화 정본은 rollout(append-only 한 파일)이다. 자리는 지금 찾아
        // 두고 **묶기는 SIGTERM 뒤에** 한다 — 마지막 턴 조각까지 실리게(jsonl 을
        // 끄고 나서 올리는 것과 같은 순서).
        let rollout = match &sid {
            Some(sid) if is_codex => Some(
                socket::codex_rollout_for_session(sid).ok_or_else(|| {
                    anyhow::anyhow!("세션 {sid} 의 Codex rollout 을 못 찾았다")
                })?,
            ),
            _ => None,
        };
        // 권한 모드를 승계한다 — 안 실으면 옮겨간 학생이 기본값(auto)으로 떠서
        // 「왜 오토모드로 바뀌었냐」가 된다(사용자 2026-08-27). 화면 문구를 읽지 않고
        // **도는 프로세스의 인자**를 본다 — 그게 유일한 진실이다.
        // 태생 스폰(fresh)은 물려받을 인자가 없다 — 학생 스폰 관례(bypass)를 따른다.
        let bypass = match &agent {
            None => true,
            Some((_, agent_pid)) => crate::proc::command("ps")
                .args(["-o", "command=", "-p", &agent_pid.to_string()])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| {
                    let cmd = String::from_utf8_lossy(&o.stdout).into_owned();
                    if is_codex {
                        cmd.contains("--dangerously-bypass-approvals-and-sandbox")
                    } else {
                        cmd.contains("--dangerously-skip-permissions")
                    }
                })
                .unwrap_or(false),
        };
        let character = self
            .ws
            .lock()
            .unwrap()
            .pane_character
            .get(pid)
            .cloned()
            .filter(|c| !c.is_empty());
        // 출발 전에 이 학생의 정체를 **이 기계에도** 못박는다(바인딩+수동 표식) —
        // 돌아왔을 때의 복원·명단 검사가 개명하지 못하게. 도착지 쪽은 워커의
        // repersona 가 sid 를 실어 같은 못박기를 한다(2026-08-31 시로코→케이 실측).
        if let (Some(s), Some(ch)) = (&sid, &character) {
            let _ = kasa_mcp::character::bind_session_character(s, ch);
            kasa_mcp::character::mark_manual_pick(s);
        }
        let label = kasa_mcp::machines::label_for_base(base).unwrap_or_else(|| base.to_string());
        let plan = MigratePlan {
            pid: pid.to_string(),
            label: label.clone(),
            base: base.to_string(),
            remote_cwd,
            cwd: cwd.to_string_lossy().into_owned(),
            force,
            agent_pid: agent.as_ref().map(|(_, p)| *p),
            sid,
            is_codex,
            jsonl,
            rollout,
            character,
            model,
            effort,
            bypass,
            run: if fresh && transfer.is_some() { Some(":".to_string()) } else { run.map(str::to_string) },
            transfer,
        };
        let mc = &mut self.info.machines_col;
        mc.busy = Some((pid.to_string(), format!("{label} 로 이사 중")));
        mc.note = None;
        mc.progress = Some(state::MigrateProgress::new(
            pid,
            &label,
            plan.character.as_deref().unwrap_or(""),
            reply,
        ));
        self.chrome_dirty = true;
        let proxy = self.proxy.clone();
        std::thread::spawn(move || migrate_worker(plan, proxy));
        Ok(format!("이사 시작 — {pid} → {label}, 진행은 Info 「다른 기계」에서"))
    }

    /// 이 pane 의 이사가 지금 워커에서 도는 중인가 — 부른 쪽이 「시작됨」과
    /// 「예약됨·끝남」을 갈라야 busy·note 를 잘못 걷지 않는다.
    pub(crate) fn migrate_running(&self, pane: &str) -> bool {
        self.info
            .machines_col
            .progress
            .as_ref()
            .is_some_and(|p| p.pane == pane && p.finished.is_none())
    }

    pub(super) fn migrate_running_any(&self) -> bool {
        self.info
            .machines_col
            .progress
            .as_ref()
            .is_some_and(|p| p.finished.is_none())
    }

    /// 워커의 단계 보고를 체크리스트에 적는다(`UserEvent::MigrateStage`).
    pub(crate) fn migrate_stage(
        &mut self,
        pane: &str,
        idx: usize,
        st: state::MigrateStageState,
        note: &str,
    ) {
        let Some(p) = self.info.machines_col.progress.as_mut() else { return };
        if p.pane != pane {
            return;
        }
        if let Some(s) = p.stages.get_mut(idx) {
            s.0 = st;
            s.1 = note.to_string();
        }
        self.info.machines_col.busy = Some((
            pane.to_string(),
            format!("{} — {}", state::MIGRATE_STAGES.get(idx).copied().unwrap_or(""), note),
        ));
        self.chrome_dirty = true;
    }

    /// 이사의 마지막 단계(GUI 스레드) — 같은 pane id 자리에 원격 셸을 앉히고 resume
    /// 을 주입한다(`UserEvent::MigrateDone`). 워커가 실패로 끝났으면 여기서 정리만.
    pub(crate) fn migrate_finish(
        &mut self,
        pid: &str,
        outcome: std::result::Result<Box<MigrateReady>, String>,
    ) {
        let reserved = outcome.as_ref().ok().and_then(|ready| ready.transfer_reserved.clone())
            .zip(outcome.as_ref().ok().map(|ready| ready.base.clone()));
        let mut result = match outcome {
            Err(why) => Err(why),
            Ok(r) => self.migrate_seat(pid, *r).map_err(|e| format!("{e:#}")),
        };
        if result.is_err() {
            if let Some((identity, base)) = reserved {
                let proxy = self.proxy.clone();
                std::thread::spawn(move || {
                    if let Err(error) = kasa_mcp::remote::transfer_close(&base, &identity) {
                        eprintln!("[transfer] attach cleanup failed {base} {}: {error:#}", identity.pane_id);
                        let _ = proxy.send_event(UserEvent::RepoCatchup("도착 기기에 준비한 빈 셸을 정리하지 못했어요".into()));
                    }
                });
                result = result.map_err(|why| format!("{why}\n대화는 출발 기기에 남아 있어요. 그 기기에서 세션을 다시 켜야 해요"));
            }
        }
        let mc = &mut self.info.machines_col;
        let ok = result.is_ok();
        if let Some(p) = mc.progress.as_mut().filter(|p| p.pane == pid) {
            let last = p.stages.len() - 1;
            match &result {
                Ok(id) => {
                    p.stages[last] = (state::MigrateStageState::Done, id.clone());
                }
                Err(why) => {
                    // 돌던 단계가 실패한 것이다 — 없으면(검사 전에 죽음) 마지막 단계에.
                    let i = p
                        .stages
                        .iter()
                        .position(|(s, _)| *s == state::MigrateStageState::Running)
                        .unwrap_or(last);
                    p.stages[i] = (state::MigrateStageState::Failed, why.clone());
                    for (s, _) in p.stages.iter_mut().skip(i + 1) {
                        if *s == state::MigrateStageState::Pending {
                            *s = state::MigrateStageState::Skipped;
                        }
                    }
                }
            }
            p.finished = Some((std::time::Instant::now(), ok));
            if let Some(reply) = p.reply.take() {
                let _ = reply.send(result.clone());
            }
        }
        mc.busy = None;
        match result {
            Ok(id) => {
                mc.note = Some((pid.to_string(), true, format!("이사 완료 · {id}")));
                self.set_toast(format!("이사 완료 — {pid} → {id}"));
            }
            Err(why) => {
                mc.note = Some((pid.to_string(), false, why.clone()));
                self.set_toast(format!("이사 실패 — {why}"));
            }
        }
        // 다음 틱에 바로 새 배치를 읽게.
        self.info.machines_col.last_refresh = None;
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// ⑦ 자리 갈아끼우기 + 켜기. 원격 pane 소환까지는 워커가 끝냈고, 여기는 이
    /// 창의 PTY 지도·배치를 만지는 부분이라 GUI 스레드여야 한다.
    pub(super) fn migrate_seat(&mut self, pid: &str, r: MigrateReady) -> Result<String> {
        self.migrate_stage(pid, state::MIGRATE_STAGES.len() - 1, state::MigrateStageState::Running, "");
        let Some(sess) = self.pty.get(pid).cloned() else {
            anyhow::bail!("이사 도중 pane {pid} 이 사라졌다");
        };
        // Keep the current shell responsive until the remote handshake succeeds.
        // 맨 셸 폴백은 캐릭터가 통째로 새 배정으로 굴러간다 — 조용히 지나가면
        // 「이사했더니 딴 학생이 됐다」로만 보이므로 크게 말한다.
        if r.character.is_some() && r.remote_pane.is_none() {
            self.set_toast(
                "저쪽에 캐릭터 pane 을 못 만들어 맨 셸로 간다 — 캐릭터가 새로 배정될 수 있다"
                    .to_string(),
            );
        }
        // 같은 pane id 로 원격 셸을 앉힌다 — 자리·이름·학생 배정 유지(promote 패턴).
        let (c, rows) = sess.size();
        let remote = kasa_mcp::remote::connect(
            kasa_mcp::remote::RemoteSpec {
                base: r.base.clone(),
                pane: r.remote_pane.clone(),
                // 이어받기(pane 지정)면 cwd 는 무시된다 — 그 셸은 이미 떠 있고,
                // 아래 주입이 `cd` 로 옮긴다.
                cwd: r.remote_pane.is_none().then(|| r.remote_cwd.clone()),
                token: None,
                // 역이사의 재료다: 어느 기계로 갔고(라벨), 거기 어디서 돌며
                // (remote_cwd), 돌아오면 어디로 갈지(origin = 지금 이 로컬 경로).
                identity: kasa_mcp::remote::RemoteIdentity {
                    label: kasa_mcp::machines::label_for_base(&r.base).unwrap_or_default(),
                    remote_cwd: Some(r.remote_cwd.clone()),
                    origin_cwd: Some(r.cwd.clone()),
                    owned: false,
                },
            },
            pid,
            c,
            rows,
        )?;
        sess.stop_reader();
        // Register before starting the pump: its generation guard rejects
        // queued snapshots while the registry still points at the old shell.
        self.insert_pty(pid.to_string(), remote.session.clone());
        self.pump_pty_screens(
            remote.session.screens.clone(),
            pid.to_string(),
            std::sync::Arc::downgrade(&remote.session),
        );
        self.dead_panes.lock().unwrap().retain(|x| x != pid);
        // 태생 스폰 + 실행 명령 지정(`mini codex`)이면 그 명령을 **그대로** 돌린다 —
        // 하네스 불문이 요점이라 플래그를 덧붙이지 않는다. 그 외엔 claude resume.
        let cmd = match (&r.sid, &r.run) {
            (None, Some(run)) => format!("{}\r", run.trim()),
            _ => {
                let c = restore_agent_command(
                    Some(if r.is_codex { "codex" } else { "claude" }),
                    r.sid.as_deref(),
                    r.sid.is_some(),
                    Some(r.model.as_str()).filter(|s| !s.is_empty()),
                    Some(r.effort.as_str()).filter(|s| !s.is_empty()),
                );
                if r.bypass {
                    let flag = if r.is_codex {
                        "--dangerously-bypass-approvals-and-sandbox"
                    } else {
                        "--dangerously-skip-permissions"
                    };
                    format!("{} {flag}\r", c.trim_end_matches('\r'))
                } else {
                    c
                }
            }
        };
        // 갓 만든 원격 pane 의 셸은 소환된 자리(그 창의 기준 pane)에서 뜬다 —
        // 레포로 옮겨 놓고 이어받아야 학생이 제 코드 위에서 깬다.
        let cmd = if r.remote_pane.is_some() {
            format!("cd '{}' && {}", r.remote_cwd.replace('\'', r"'\''"), cmd)
        } else {
            cmd
        };
        self.pending_restores.push((
            remote.session.clone(),
            cmd,
            // 갓 소환된 pane 은 셸이 뜨는 데 한 박자 더 걸린다.
            std::time::Instant::now() + std::time::Duration::from_millis(2200),
        ));
        // 원래 자리는 명령이 닿은 뒤 걷는다 — 그때까지는 이 링크가 명령을 나른다. 저쪽에
        // 방 pane 이 안 생긴(헤드리스 `web-` 셸) 경우는 이 링크가 유일한 손잡이라 남긴다.
        self.migrate_handoff = r.remote_pane.is_some().then(|| crate::MigrateHandoff {
            pid: pid.to_string(),
            label: kasa_mcp::machines::label_for_base(&r.base).unwrap_or_default(),
            base: r.base.clone(),
            remote_id: remote.remote_id.clone(),
            at: std::time::Instant::now() + std::time::Duration::from_millis(3500),
        });
        let (wc, wr) = self.window_cells();
        self.resize_backend(wc, wr);
        self.publish_pty_layout();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        Ok(remote.remote_id)
    }

    /// 역이사 — 원격 pane 의 claude 를 **이 기계로** 데려온다(migrate 의 거울).
    ///
    /// 순서: ①돌아올 폴더 확인(없으면 홈) ②원격 claude 곱게 끄기(권한 모드도
    /// 여기서 받는다) ③대화 내려받기 ④원격 셸 철거 + 같은 pane id 로 로컬
    /// PTY 스왑 ⑤`claude --resume` 주입. 순방향과 같이 **세션 파일만** 옮긴다
    /// (2026-09-14) — bundle 싱크·clone 은 걷었다.
    ///
    /// ②가 ③보다 먼저인 이유: jsonl 은 살아 있는 동안에도 읽을 수 있지만, 끄기
    /// 전에 받으면 마지막 턴이 파일에 덜 실린 채 건너온다 — 순방향이 SIGTERM
    /// 뒤에 업로드하는 것과 같은 순서다.
    #[cfg(unix)]
    pub(crate) fn migrate_pane_back(
        &mut self,
        pid: &str,
        local_cwd: Option<&str>,
        force: bool,
    ) -> Result<String> {
        self.ensure_user_mutation_target(
            pid,
            crate::settings_room::SettingsMutation::Migrate,
        )?;
        let Some(sess) = self.pty.get(pid).cloned() else {
            anyhow::bail!("pane {pid} 이 없다");
        };
        let Some(info) = kasa_mcp::remote::remote_info(pid) else {
            anyhow::bail!("{pid} 은 원격 pane 이 아니다 — 데려올 것이 없다");
        };
        // sid — 이사로 나간 pane 은 이 창이 기억하지만, **원격에서 태어난 학생**의
        // 거울은 모른다(bind-transcript 는 몸통이 있는 기계에서만 돈다). 그때는 그
        // 기계에 묻는다(2026-09-02 코유키 감사 — 전엔 「sid 없음」으로 거부해 손으로
        // bind-transcript 를 해야 했다). 얻은 값은 기억해 둔다 — 다음 시도·저장에.
        let local_sid = self.pane_claude_sid.get(pid).cloned();
        let fetched = if local_sid.is_none() {
            self.migrate_progress(pid, "저쪽 캐릭터의 대화 id 묻는 중…".to_string());
            kasa_mcp::remote::remote_pane_session(&info.base, &info.remote_id, None)
        } else {
            None
        };
        let sid = back_migration_sid(local_sid, fetched)?;
        self.pane_claude_sid.insert(pid.to_string(), sid.clone());
        let Some(remote_cwd) = info.remote_cwd.clone() else {
            anyhow::bail!("{pid} 의 원격 작업 폴더를 모른다 — 옛 저장본이다. 한 번 재시작해 다시 저장되면 생긴다");
        };
        // 순방향과 같은 관문 — 원격 학생이 턴 중이면 agent-stop 이 그 턴을 자른다.
        // 미러 그리드가 원격 화면 그대로라 같은 스피너 판정이 통한다.
        if !force {
            let working = {
                let ws = self.ws.lock().unwrap();
                self.pane_agent_working(&ws, pid)
            };
            if working {
                self.migrate_queue.retain(|q| q.pane != pid);
                self.migrate_queue.push(PendingMigration {
                    pane: pid.to_string(),
                    base: "local".into(),
                    cwd: local_cwd.map(str::to_string),
                    force,
                    run: None,
                    idle_since: None,
                    transfer: None,
                });
                self.set_toast(format!("{pid} 는 지금 일하는 중 — 턴이 끝나면 데려온다"));
                return Ok(format!("예약됨 — {pid} 가 하던 턴을 마치면 데려온다"));
            }
        }
        let machine = kasa_mcp::machines::machines()
            .into_iter()
            .find(|m| m.base == info.base.trim_end_matches('/'));
        let dest = local_cwd
            .map(str::to_string)
            .or_else(|| info.origin_cwd.clone())
            .or_else(|| {
                machine
                    .as_ref()
                    .and_then(|m| kasa_mcp::machines::map_remote_to_local(m, &remote_cwd))
            })
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "돌아올 로컬 경로를 모른다 — --cwd 로 지정하거나 machines.json 에 roots 를 적어라"
                )
            })?;
        // 돌아올 폴더가 이 기계에 없으면 홈에서 대화만 잇는다 — clone 으로 만들어
        // 주던 옛 단계는 걷었다(2026-09-14).
        let dest = if std::path::Path::new(&dest).is_dir() {
            dest
        } else {
            let home = kasa_socket::home_dir()
                .map(|h| h.to_string_lossy().into_owned())
                .ok_or_else(|| anyhow::anyhow!("{dest} 가 없고 홈도 몰라 돌아올 자리를 못 정한다"))?;
            self.set_toast(format!("{dest} 가 없어 홈에서 잇는다"));
            home
        };
        // 모델·effort 는 원격이 죽기 전에 떠 둔다(순방향과 같은 이유). 이 창의 보고는
        // 이사로 나가기 전 값이라 원격에서 태어난 학생에겐 없다 — 그땐 그 기계의
        // 목록(`/term/panes` model·effort)이 정본이고, 낡은 원격이면 기본값이다.
        let (model, effort) = self
            .agent_cfg_snapshot()
            .get(pid)
            .cloned()
            .filter(|(m, e)| !m.is_empty() || !e.is_empty())
            .or_else(|| kasa_mcp::remote::remote_pane_cfg(&info.base, &info.remote_id, None))
            .unwrap_or_default();
        // 원격 claude 곱게 끄기. 이미 꺼져 있으면(None) 권한 모드를 모르니 안전한
        // 쪽(물어보는 모드)으로 둔다.
        self.migrate_progress(pid, "저쪽 캐릭터 곱게 끄는 중…".to_string());
        let bypass = match kasa_mcp::remote::remote_agent_stop(&info.base, &info.remote_id, None) {
            Ok(stopped) => stopped.unwrap_or(false),
            Err(e) => {
                if force {
                    eprintln!("[migrate-back] 원격 종료 확인 실패(강행): {e:#}");
                    false
                } else {
                    anyhow::bail!(
                        "원격 claude 를 못 껐다({e:#}) — 반쯤 산 채 데려오면 같은 대화를 다툰다. 알고 강행하려면 --force"
                    );
                }
            }
        };
        self.migrate_progress(pid, "대화 내려받는 중…".to_string());
        // 하네스는 짐작하지 않고 **대화가 어느 창구에 있느냐**로 가른다 — codex
        // 창구를 먼저 물어(없으면 None — 낡은 기계도 같은 답) claude 로 물러선다.
        let codex_pack = kasa_mcp::remote::fetch_codex_session(&info.base, &sid, None)?;
        let is_codex = codex_pack.is_some();
        if let Some((rel, bytes)) = &codex_pack {
            // 로컬 설치는 서버 POST 창구와 같은 함수 — 검증·충돌 보관 정책이
            // 창구마다 갈리지 않게 한 벌로 쓴다(다르면 기존 것을 보관하고 앉힘).
            let home = kasa_socket::home_dir()
                .map(|h| h.join(".codex"))
                .ok_or_else(|| anyhow::anyhow!("HOME 을 몰라 Codex home 을 못 정한다"))?;
            let note = kasa_mcp::codexhome::install_codex_rollout(
                &home,
                &sid,
                std::path::Path::new(rel),
                bytes,
            )?;
            self.set_toast(format!("Codex 대화: {note}"));
        } else {
            let bytes =
                kasa_mcp::remote::download_transcript(&info.base, &remote_cwd, &sid, None)?;
            let jsonl =
                kasa_socket::sessions::session_jsonl_path(std::path::Path::new(&dest), &sid)
                    .ok_or_else(|| anyhow::anyhow!("HOME 을 몰라 대화 저장 위치를 못 정한다"))?;
            // 크기 후퇴 관문 — 서버 업로드 쪽 규칙의 거울. 로컬에 더 큰 대화가 있으면
            // 받은 쪽이 낡은 것이다(대개 이사 전 사본이 더 작으니 평소엔 통과).
            if let Ok(meta) = std::fs::metadata(&jsonl) {
                if meta.len() > bytes.len() as u64 && !force {
                    anyhow::bail!(
                        "로컬에 더 큰 대화가 이미 있다({}B > {}B) — 원격 쪽이 낡았다. 알고 덮으려면 --force",
                        meta.len(),
                        bytes.len()
                    );
                }
            }
            if let Some(dir) = jsonl.parent() {
                std::fs::create_dir_all(dir)
                    .map_err(|e| anyhow::anyhow!("대화 폴더 생성 실패: {e}"))?;
            }
            let tmp = jsonl.with_extension("jsonl.part");
            std::fs::write(&tmp, &bytes)
                .and_then(|_| std::fs::rename(&tmp, &jsonl))
                .map_err(|e| anyhow::anyhow!("대화 저장 실패: {e}"))?;
        }
        // 원격 셸을 진짜 끝낸다 — 안 걷으면 그 기계에 빈 학생 pane 이 좀비로 남는다.
        // 매니저가 이미 죽었어도(연결 유실) 실패가 아니다: 남은 셸은 닿을 때 걷는다.
        self.migrate_progress(pid, "창 바꿔 끼우고 캐릭터 깨우는 중…".to_string());
        let _ = kasa_mcp::remote::kill_remote(pid);
        // GUI pane 은 위 kill 로 안 걷힌다(앱이 제 Arc 를 쥔다) — 그 기계의 pane 자체를
        // 닫는다. 실패해도 이사는 성립한다(저쪽에 빈 셸 pane 이 남을 뿐): 로그만.
        if let Err(e) = kasa_mcp::remote::close_remote_pane(&info.base, &info.remote_id, None, true) {
            eprintln!("[migrate-back] 원격 pane {} 닫기 실패(무시): {e:#}", info.remote_id);
        }
        // 같은 pane id 로 로컬 PTY 스왑(swap_character 골격).
        let (c, r) = sess.size();
        let room = self.ws.lock().unwrap().pane_room.get(pid).cloned();
        let mut env = crate::proxy_env(pid);
        if let Some(ref rm) = room {
            env.push(("KASATERM_ROOM".to_string(), rm.clone()));
        }
        // 데려온 학생은 **같은 캐릭터로** 앉힌다 — pending 없이 배정을 돌리면
        // 랜덤 풀에서 새 학생이 뽑혀, 몸(대화)은 그대로인데 이름·얼굴이 바뀐다
        // (2026-08-31 실측: 시로코가 이사 왕복 뒤 케이로 둔갑 — 이 줄이 주사위였다).
        // pending 은 명시 지정이라 중복이어도 존중된다(assign_character_env 규칙).
        let prior_character = self.ws.lock().unwrap().pane_character.get(pid).cloned();
        if let Some(ref ch) = prior_character {
            self.pending_character = Some(ch.clone());
        }
        env.extend(self.assign_character_env(pid, Some(&dest), room.as_deref()));
        let session = kasa_pty::PtySession::start(kasa_pty::PtyOptions {
            shell: resolve_default_shell(),
            cwd: Some(dest.clone()),
            cols: c,
            rows: r,
            env,
            pane_id: pid.to_string(),
            initial_scrollback: Vec::new(),
        })
        .map_err(|e| anyhow::anyhow!("로컬 셸 스폰 실패: {e:#}"))?;
        let session = Arc::new(session);
        // insert 가 원격 링크 세션을 떨군다 — 링크 매니저는 Drop 으로 걷힌다.
        self.insert_pty(pid.to_string(), session.clone());
        self.pump_pty_screens(
            session.screens.clone(),
            pid.to_string(),
            std::sync::Arc::downgrade(&session),
        );
        self.dead_panes.lock().unwrap().retain(|x| x != pid);
        self.pane_cwd_cache
            .insert(pid.to_string(), std::path::PathBuf::from(&dest));
        let cmd = restore_agent_command(
            Some(if is_codex { "codex" } else { "claude" }),
            Some(&sid),
            true,
            Some(model.as_str()).filter(|s| !s.is_empty()),
            Some(effort.as_str()).filter(|s| !s.is_empty()),
        );
        let cmd = if bypass {
            let flag = if is_codex {
                "--dangerously-bypass-approvals-and-sandbox"
            } else {
                "--dangerously-skip-permissions"
            };
            format!("{} {flag}\r", cmd.trim_end_matches('\r'))
        } else {
            cmd
        };
        self.pending_restores.push((
            session,
            cmd,
            std::time::Instant::now() + std::time::Duration::from_millis(900),
        ));
        // 정체를 못박는다 — sid 바인딩 + 수동 표식 + 다음 부팅용 persona override.
        // 이게 없으면 명단·테마를 바꾼 다음 복원이 데려온 학생을 또 개명한다.
        if let Some(ref ch) = prior_character {
            self.repersona_pane(pid, ch);
        }
        let (wc, wr) = self.window_cells();
        self.resize_backend(wc, wr);
        self.publish_pty_layout();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        self.set_toast(format!("{pid} 을 이 기계로 데려왔어요 ({dest})"));
        Ok("local".into())
    }

    /// 이사 예약 실행기 — 틱마다 불려, 예약된 pane 의 턴이 끝났는지(스피너 꺼짐이
    /// 잠시 이어졌는지) 보고 끝났으면 이사를 실행한다. 한 틱에 하나만 발사한다 —
    /// migrate_pane 은 GUI 스레드에서 몇 초를 먹으므로 몰아서 하면 화면이 굳는다.
    pub(crate) fn run_pending_migrations(&mut self) {
        #[cfg(unix)]
        {
            if self.migrate_queue.is_empty() {
                return;
            }
            // 닫힌 pane 의 예약부터 걷는다 — index 판정보다 먼저.
            {
                let pty = &self.pty;
                self.migrate_queue.retain(|q| pty.contains_key(&q.pane));
            }
            let now = std::time::Instant::now();
            let mut fire: Option<PendingMigration> = None;
            {
                let ws = self.ws.lock().unwrap();
                let mut i = 0;
                while i < self.migrate_queue.len() {
                    let working = self.pane_agent_working(&ws, &self.migrate_queue[i].pane);
                    let q = &mut self.migrate_queue[i];
                    if working {
                        q.idle_since = None;
                        i += 1;
                        continue;
                    }
                    // 도구 호출 사이의 짧은 틈을 턴 끝으로 오판하지 않게 몇 초
                    // 이어진 조용함만 발사한다(스피너는 도구 사이에도 떠 있지만
                    // 프레임 경계의 빈 순간이 있을 수 있다 — 값싼 보험).
                    let since = *q.idle_since.get_or_insert(now);
                    if fire.is_none()
                        && now.duration_since(since) >= std::time::Duration::from_secs(3)
                    {
                        fire = Some(self.migrate_queue.remove(i));
                        continue;
                    }
                    i += 1;
                }
            }
            let Some(q) = fire else { return };
            // 학생이 그새 스스로 꺼졌으면 순방향 예약은 취소한다 — 그대로 돌리면
            // 태생 스폰(fresh)으로 새서 빈 claude 가 저쪽에 태어난다. 역이사(local)는
            // 에이전트가 원격에 있어 이 판정이 성립하지 않으니 그냥 간다.
            if q.base != "local" && q.run.is_none() {
                let alive = self
                    .pty
                    .get(&q.pane)
                    .and_then(|s| s.shell_pid())
                    .and_then(|sh| {
                        kasa_pty::agent_pid_for_shell(&kasa_pty::process_table_shared(), sh)
                    })
                    .is_some();
                if !alive {
                    self.set_toast(format!(
                        "{} 의 캐릭터가 꺼져 있어 예약 이사를 취소했다",
                        q.pane
                    ));
                    return;
                }
            }
            let res = if let Some(request) = &q.transfer {
                self.start_transfer_migration(request, None)
            } else if q.base == "local" {
                self.migrate_pane_back(&q.pane, q.cwd.as_deref(), q.force)
            } else {
                self.migrate_pane(
                    &q.pane,
                    &q.base,
                    q.cwd.as_deref(),
                    q.force,
                    q.run.as_deref(),
                )
            };
            // 보내기는 워커로 넘어간다 — busy·토스트는 migrate_finish 몫. 데려오기와
            // 검사 실패만 여기서 마무리한다(안 풀면 원격 메뉴가 잠긴 채 남는다).
            if res.is_ok() && self.migrate_running(&q.pane) {
                return;
            }
            self.info.machines_col.busy = None;
            match res {
                Ok(id) => self.set_toast(format!("예약 이사 완료: {} → {id}", q.pane)),
                Err(e) => {
                    eprintln!("[migrate] 예약 이사 실패({}): {e:#}", q.pane);
                    self.set_toast(format!("예약 이사 실패({}): {e:#}", q.pane));
                }
            }
        }
    }
}

/// 승격된 학생들이 사는 로컬 상주 데몬(kasa-serve-web)의 HTTP 포트.
///
/// ⚠️ 8790 을 쓰면 안 된다 — 기계 간 중계소(`kasa-relay`)의 포트다. 미니는 중계소
/// 본체가, 맥북은 그걸 끌어오는 터널 LaunchAgent(`kasa-relay-tunnel` 의
/// `-L 8790:127.0.0.1:8790`)가 상시 물고 있다. `kasa-serve-web` 은 요청한 포트를
/// 못 잡으면 임의 포트로 도망가는 대신 **즉시 종료**하므로(입양 소켓 이름이 포트에
/// 묶여 판정이 어긋난다), 승격이 「데몬이 5초 안에 안 떴어요」로 두 기계 모두에서
/// 실패한다.
#[cfg(unix)]
pub(crate) const LOCAL_PTYD_PORT: u16 = 8767;

/// 데려오기(`migrate … local`)가 이어갈 세션 id 를 정한다 — 이 창이 기억하는 것이
/// 먼저, 없으면 원격이 답한 것. 둘 다 없으면 이유를 사람 말로.
///
/// 원격 답은 검증한다: claude 세션 id 는 uuid, codex rollout 도 uuid 꼴이라 그 밖의
/// 글자(경로·공백·HTML 오류 페이지)가 오면 `--resume` 에 실을 수 없다.
pub(super) fn back_migration_sid(local: Option<String>, remote: Option<String>) -> Result<String> {
    let ok = |s: &str| {
        !s.is_empty()
            && s.len() <= 80
            && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    };
    if let Some(s) = local.filter(|s| ok(s)) {
        return Ok(s);
    }
    match remote {
        Some(s) if ok(&s) => Ok(s),
        Some(s) => anyhow::bail!("원격이 준 세션 id 가 이상하다({s:?}) — 저쪽 프로그램을 갱신해라"),
        None => anyhow::bail!(
            "세션 id 를 모른다 — 저쪽 캐릭터가 아직 첫 프롬프트 전이거나(대화 파일이 없다) 저쪽 프로그램이 낡아 `/pane-session` 이 없다. 저쪽에서 한마디 시킨 뒤 다시 데려와라"
        ),
    }
}

#[cfg(test)]
mod migrate_back_tests {
    use super::back_migration_sid;

    #[test]
    fn local_memory_wins_over_remote() {
        let sid = back_migration_sid(Some("aaaa-1".into()), Some("bbbb-2".into())).unwrap();
        assert_eq!(sid, "aaaa-1");
    }

    /// 원격에서 태어난 학생 — 이 창은 모르고 원격만 안다. 전엔 여기서 거부했다.
    #[test]
    fn remote_answer_fills_the_gap() {
        let sid = back_migration_sid(None, Some("3f2a1b7c-0000-4000-8000-000000000001".into())).unwrap();
        assert_eq!(sid, "3f2a1b7c-0000-4000-8000-000000000001");
    }

    #[test]
    fn garbage_from_remote_is_refused_with_reason() {
        let e = back_migration_sid(None, Some("<html>404</html>".into())).unwrap_err();
        assert!(e.to_string().contains("이상하다"), "{e}");
        let e = back_migration_sid(None, None).unwrap_err();
        assert!(e.to_string().contains("첫 프롬프트"), "{e}");
    }

    /// 이 창의 기억이 비어 있는 문자열이면(저장본 손상) 원격 답으로 넘어간다.
    #[test]
    fn blank_local_memory_is_not_a_memory() {
        let sid = back_migration_sid(Some(String::new()), Some("rollout-1".into())).unwrap();
        assert_eq!(sid, "rollout-1");
    }
}

#[cfg(test)]
mod transfer_concurrency_tests {
    use super::*;

    #[test]
    fn another_request_cannot_replace_the_first_queued_destination() {
        let request = kasa_socket::transfer::MigrateRequest {
            session: kasa_socket::transfer::SessionIdentity { pane_id: "%4".into(), ..Default::default() },
            destination_machine: "mini".into(), room: kasa_socket::transfer::RoomTarget::New("첫 방".into()),
        };
        let queue = vec![PendingMigration {
            pane: "%4".into(), base: "mini".into(), cwd: None, force: false, run: None,
            idle_since: None, transfer: Some(request.clone()),
        }];
        assert!(ensure_migration_slot(&queue, "%4").is_err());
        assert!(ensure_migration_slot(&queue, "%5").is_ok());
        assert_eq!(queue[0].transfer.as_ref().unwrap().room, request.room);
    }

    #[test]
    fn same_session_restarted_with_a_different_process_is_rejected() {
        assert!(ensure_transfer_agent(Some(100), Some(101)).is_err());
        assert!(ensure_transfer_agent(Some(100), None).is_err());
        assert!(ensure_transfer_agent(None, Some(100)).is_err());
        assert!(ensure_transfer_agent(Some(100), Some(100)).is_ok());
        assert!(ensure_transfer_agent(None, None).is_ok());
    }
}

/// 이사 워커의 재료 — GUI 가 검사·수집을 끝내고 스레드에 통째로 넘긴다. 전부
/// 소유값이라 워커는 App 을 모른다.
#[cfg(unix)]
pub(super) struct MigratePlan {
    pub(super) pid: String,
    /// 명부 라벨 — 오류 문장에 어느 기계인지 박는다.
    pub(super) label: String,
    pub(super) base: String,
    pub(super) remote_cwd: String,
    pub(super) cwd: String,
    pub(super) force: bool,
    pub(super) agent_pid: Option<u32>,
    pub(super) sid: Option<String>,
    pub(super) is_codex: bool,
    pub(super) jsonl: Option<std::path::PathBuf>,
    pub(super) rollout: Option<std::path::PathBuf>,
    pub(super) character: Option<String>,
    pub(super) model: String,
    pub(super) effort: String,
    pub(super) bypass: bool,
    pub(super) run: Option<String>,
    pub(super) transfer: Option<kasa_socket::transfer::MigrateRequest>,
}

/// 워커가 끝내고 GUI 에 돌려주는 것 — ⑦(자리 갈아끼우기·켜기)에 필요한 만큼만.
#[derive(Clone, Debug)]
pub(crate) struct MigrateReady {
    pub(crate) base: String,
    pub(crate) remote_cwd: String,
    /// 출발한 로컬 경로 — 역이사가 돌아올 자리.
    pub(crate) cwd: String,
    pub(crate) sid: Option<String>,
    pub(crate) is_codex: bool,
    pub(crate) model: String,
    pub(crate) effort: String,
    pub(crate) bypass: bool,
    pub(crate) run: Option<String>,
    pub(crate) transfer_reserved: Option<kasa_socket::transfer::SessionIdentity>,
    /// 저쪽에 소환된 학생 pane. None 이면 맨 셸로 물러선다.
    pub(crate) remote_pane: Option<String>,
    pub(crate) character: Option<String>,
}

/// 이사 ①~④ — 네트워크·기다림뿐이라 GUI 밖에서 돈다. 단계마다 `MigrateStage`,
/// 끝에 `MigrateDone` 을 보낸다. 실패는 그 단계가 Running 인 채로 Err 가 가고,
/// GUI 가 그 자리에 ✗ 를 찍는다.
#[cfg(unix)]
pub(super) fn migrate_worker(p: MigratePlan, proxy: winit::event_loop::EventLoopProxy<UserEvent>) {
    use state::MigrateStageState as S;
    let stage = |i: usize, st: S, note: String| {
        let _ = proxy.send_event(UserEvent::MigrateStage(p.pid.clone(), i, st, note));
    };
    let mut reserved: Option<kasa_socket::transfer::SessionRow> = None;
    let mut source_stopped = false;
    let validate = || -> Result<()> {
        if let Some(request) = &p.transfer {
            let (tx, rx) = std::sync::mpsc::channel();
            proxy.send_event(UserEvent::ValidateTransfer(request.session.clone(), tx))
                .map_err(|_| anyhow::anyhow!("출발 창이 닫혔어요"))?;
            rx.recv_timeout(std::time::Duration::from_secs(10))
                .map_err(|_| anyhow::anyhow!("출발 세션 상태를 확인하지 못했어요"))?
                .map_err(anyhow::Error::msg)?;
        }
        Ok(())
    };
    let mut outcome = (|| -> Result<MigrateReady> {
        // ① 저쪽 폴더 확인 — 같은 경로가 저쪽에 없으면 **홈에서** 대화만 잇는다.
        // 레포를 만들어 맞추던 옛 단계는 걷었다(2026-09-14). 창구가 없는 옛 판은
        // 있다고 치고 그대로 간다 — 그때 폴더가 없으면 저쪽 셸의 cd 가 실패하고,
        // resume 명령이 홈에서 뜬다(대화 파일은 그 경로 이름으로 올라가 있으니 안 잇긴다).
        let mut remote_cwd = p.remote_cwd.clone();
        stage(0, S::Running, String::new());
        match kasa_mcp::remote::remote_path_probe(&p.base, &remote_cwd, None) {
            Ok(Some((true, _))) => stage(0, S::Done, "있음".to_string()),
            Ok(Some((false, home))) if !home.is_empty() => {
                remote_cwd = home;
                stage(0, S::Done, "없음 — 홈에서 잇는다".to_string());
            }
            Ok(Some((false, _))) => anyhow::bail!(
                "{}에 {} 가 없고 저쪽 홈도 못 알아냈다",
                p.label,
                p.remote_cwd
            ),
            Ok(None) => stage(0, S::Done, "옛 판 — 있다고 치고".to_string()),
            Err(e) => anyhow::bail!("{} 쪽 폴더 확인: {e:#}", p.label),
        }
        if let Some(request) = &p.transfer {
            validate()?;
            reserved = Some(kasa_mcp::remote::transfer_spawn(&p.base, &kasa_socket::transfer::SpawnRequest {
                room: request.room.clone(),
                cwd: remote_cwd.clone(),
                character: p.character.clone(),
            })?);
            // 긴 복사·도착 방 생성 사이 출발 세션이 바뀌거나 일을 재개할 수 있다.
            validate()?;
        }
        if p.transfer.is_some() {
            let table = kasa_pty::fresh_process_table();
            let shell = kasa_pty::lookup_session(&p.pid).and_then(|session| session.shell_pid());
            let current = shell.and_then(|shell| kasa_pty::agent_pid_for_shell(&table, shell)).map(|(_, pid)| pid);
            ensure_transfer_agent(p.agent_pid, current)?;
            if p.agent_pid.is_none() && !crate::transfer_endpoints::idle_shell(shell, &table) {
                anyhow::bail!("이사 준비 중 셸에서 작업이 시작됐어요. 현재 작업을 유지하고 이사를 중단했어요");
            }
        }
        // ③ 곱게 끈다 — SIGKILL 은 jsonl 마지막 조각을 유실할 수 있다. 안 죽으면
        // 강행하지 않고 세운다: 반쯤 산 claude 와 원격 resume 이 같은 대화를
        // 다투는 것이 최악이다(옛 9-pane 사고의 원형). 태생 스폰은 끌 것도
        // 옮길 대화도 없어 ③④를 통째로 건너뛴다.
        match (p.agent_pid, &p.sid) {
            (Some(agent_pid), Some(sid)) => {
                stage(1, S::Running, String::new());
                unsafe { libc::kill(agent_pid as i32, libc::SIGTERM) };
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
                while unsafe { libc::kill(agent_pid as i32, 0) } == 0 {
                    if std::time::Instant::now() > deadline {
                        anyhow::bail!("캐릭터(pid {agent_pid}) 이 8초 안에 안 꺼졌다 — 이사를 세웠다");
                    }
                    std::thread::sleep(std::time::Duration::from_millis(120));
                }
                source_stopped = true;
                stage(1, S::Done, String::new());
                // ③ 대화 옮기기
                if let Some(jsonl) = &p.jsonl {
                    let size = std::fs::metadata(jsonl)
                        .map(|m| format!("{:.1}MB", m.len() as f64 / 1048576.0))
                        .unwrap_or_default();
                    stage(2, S::Running, size.clone());
                    kasa_mcp::remote::upload_transcript(
                        &p.base,
                        &remote_cwd,
                        sid,
                        jsonl,
                        None,
                        p.force,
                    )?;
                    stage(2, S::Done, size);
                }
                if let Some(rollout) = &p.rollout {
                    // 끄고 난 뒤에 묶는다 — append-only rollout 의 마지막 조각까지.
                    // 검증(헤더·경로·id 교차)은 운반 helper 가 전담한다.
                    stage(2, S::Running, "Codex 대화 묶는 중".to_string());
                    let home = kasa_mcp::codexhome::codex_home_of_rollout(rollout)
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "rollout 이 sessions 트리 밖이다: {}",
                                rollout.display()
                            )
                        })?;
                    let bundle = kasa_socket::sessions::codex_sessions::bundle_codex_session(
                        &home, rollout,
                    )?;
                    if bundle.session_id != *sid {
                        anyhow::bail!(
                            "rollout 헤더 id({}) 가 pane 세션 id({sid}) 와 다르다",
                            bundle.session_id
                        );
                    }
                    let file = bundle
                        .files
                        .into_iter()
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("빈 Codex bundle"))?;
                    let size = format!("{:.1}MB", file.bytes.len() as f64 / 1048576.0);
                    stage(2, S::Running, format!("Codex 대화 {size} 옮기는 중"));
                    let note = kasa_mcp::remote::push_codex_session(
                        &p.base,
                        sid,
                        &file.codex_home_relative_path.to_string_lossy(),
                        file.bytes,
                        None,
                    )?;
                    stage(2, S::Done, note);
                }
            }
            _ => {
                stage(1, S::Skipped, "끌 캐릭터 없음".to_string());
                stage(2, S::Skipped, "옮길 대화 없음".to_string());
            }
        }
        // ④ 목적지에 **진짜 학생 pane** 을 먼저 만든다 — 그래야 옮겨간 자리에
        // 캐릭터·보드·훅이 다 붙는다. 창 없는 축소판 서버는 이 창구가 없으므로
        // 실패하고, 그때는 옛 경로(맨 셸 스폰)로 물러선다 — 반쪽이라도 대화는 잇는다.
        let remote_pane = if let Some(row) = &reserved {
            stage(3, S::Done, "선택한 방에 새 자리 준비됨".to_string());
            if let Some(character) = &p.character {
                kasa_mcp::remote::repersona(&p.base, &row.identity.pane_id, character, p.sid.as_deref(), None)?;
            }
            Some(row.identity.pane_id.clone())
        } else { match &p.character {
            None => {
                stage(3, S::Skipped, "캐릭터 없음 — 맨 셸".to_string());
                None
            }
            Some(c) => {
                stage(3, S::Running, c.clone());
                match kasa_mcp::remote::spawn_student_pane(&p.base, c, None) {
                    Ok(id) => {
                        // 소환만으로는 못 미덥다 — 이름표만 그 학생이고 말투는 남의
                        // 것으로 뜬 실측이 있다(2026-08-27, pane id 재사용 자리).
                        // claude 를 띄우기 직전에 한 번 더 박는다 — sid 까지 실어
                        // 도착지 복원·명단 검사도 이 학생을 개명하지 못하게 한다.
                        if let Err(e) =
                            kasa_mcp::remote::repersona(&p.base, &id, c, p.sid.as_deref(), None)
                        {
                            eprintln!("[migrate] 캐릭터 못박기 실패(계속 진행): {e:#}");
                        }
                        stage(3, S::Done, format!("{c} · {id}"));
                        Some(id)
                    }
                    Err(e) => {
                        eprintln!("[migrate] 진짜 pane 소환 실패 — 맨 셸로 물러섭니다: {e:#}");
                        stage(3, S::Done, format!("소환 실패 — 맨 셸로: {e:#}"));
                        None
                    }
                }
            }
        }};
        Ok(MigrateReady {
            base: p.base.clone(),
            remote_cwd,
            cwd: p.cwd.clone(),
            sid: p.sid.clone(),
            is_codex: p.is_codex,
            model: p.model.clone(),
            effort: p.effort.clone(),
            bypass: p.bypass,
            run: p.run.clone(),
            transfer_reserved: reserved.as_ref().map(|row| row.identity.clone()),
            remote_pane,
            character: p.character.clone(),
        })
    })();
    if p.transfer.is_some() && source_stopped {
        outcome = outcome.map_err(|error| anyhow::anyhow!(
            "{error:#}\n대화는 출발 기기에 남아 있어요. 그 기기에서 세션을 다시 켜야 해요"
        ));
    }
    if outcome.is_err() {
        if let Some(row) = reserved.take() {
            if let Err(error) = kasa_mcp::remote::transfer_close(&p.base, &row.identity) {
                eprintln!("[transfer] reserved shell cleanup failed {} {}: {error:#}", p.base, row.identity.pane_id);
                let _ = proxy.send_event(UserEvent::RepoCatchup("이사는 실패했고 도착 방의 빈 셸을 정리하지 못했어요".into()));
                outcome = outcome.map_err(|error| anyhow::anyhow!("{error:#}\n도착 기기에 준비한 빈 셸이 남아 있을 수 있어요"));
            }
        }
    }
    let sent = proxy.send_event(UserEvent::MigrateDone(
        p.pid.clone(),
        outcome.map(Box::new).map_err(|e| format!("{e:#}")),
    ));
    if sent.is_err() {
        if let Some(row) = reserved {
            let _ = kasa_mcp::remote::transfer_close(&p.base, &row.identity);
        }
    }
}
