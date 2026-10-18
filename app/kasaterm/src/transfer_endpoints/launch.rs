use super::*;
use kasa_socket::transfer::LaunchRequest;
use crate::session::restore_agent_command;

pub(crate) struct PendingLaunch {
    request: LaunchRequest,
    session: Weak<kasa_pty::PtySession>,
    revision: u64,
    deadline: Instant,
    sent: bool,
    previous_block: Option<u64>,
    sender: Reply<SessionRow>,
}

impl App {
    pub(crate) fn prepare_transfer_launch(&mut self, request: LaunchRequest, sender: Reply<SessionRow>, deadline: Instant) {
        let result = (|| -> Result<PendingLaunch> {
            anyhow::ensure!(matches!(request.harness.as_str(), "claude" | "codex"), "지원하지 않는 대화 종류예요");
            uuid::Uuid::parse_str(&request.session_id).map_err(|_| anyhow::anyhow!("대화 번호가 올바르지 않아요"))?;
            anyhow::ensure!(Instant::now() < deadline, "대화 재개 요청의 유효 시간이 지났어요");
            anyhow::ensure!([&request.model, &request.effort].iter().all(|s| s.len() <= 200 && !s.chars().any(char::is_control)), "모델 설정이 올바르지 않아요");
            self.validate_transfer_identity(&request.target)?;
            anyhow::ensure!(request.target.session_id.is_none(), "예약한 빈 셸에서만 대화를 재개할 수 있어요");
            anyhow::ensure!(!self.transfer_launches.iter().any(|item| item.request.target.pane_id == request.target.pane_id), "이미 이 대화를 켜고 있어요");
            let session = self.pty.get(&request.target.pane_id).unwrap();
            anyhow::ensure!((session.input_revision() == 0 || !session.input_draft_present()) && session.input_revision() == request.input_revision,
                "예약 뒤 도착 셸에 입력이 있어 이사를 멈췄어요");
            Ok(PendingLaunch { session: Arc::downgrade(session), revision: request.input_revision,
                request, deadline, sent: false, previous_block: None, sender: sender.clone() })
        })();
        match result {
            Ok(pending) => self.transfer_launches.push(pending),
            Err(error) => reply(&sender, Err(error)),
        }
        self.chrome_dirty = true;
        if let Some(window) = &self.window { window.request_redraw(); }
    }

    fn advance_transfer_launch(&mut self, pending: &mut PendingLaunch) -> Result<Option<SessionRow>> {
        let request = &pending.request;
        let id = &request.target.pane_id;
        let session = self.pty.get(id).cloned().ok_or_else(|| anyhow::anyhow!("도착 칸이 닫혔어요"))?;
        anyhow::ensure!(Weak::ptr_eq(&Arc::downgrade(&session), &pending.session)
            && token_for(id, &session) == request.target.token, "도착 칸의 세션이 바뀌었어요");
        let outer = self.ws.lock().unwrap().outer_for_pty(id).unwrap_or_else(|| id.clone());
        anyhow::ensure!(self.window_of_pane(&outer).is_some() || self.aux_terminal_index(&outer).is_some(), "도착 칸이 닫혔어요");
        anyhow::ensure!(Instant::now() < pending.deadline, "도착 대화의 재개를 확인하지 못했어요. 도착 기기를 확인해 주세요");
        anyhow::ensure!(!session.output_closed(), "도착 터미널이 종료됐어요");
        if !pending.sent {
            anyhow::ensure!((session.input_revision() == 0 || !session.input_draft_present()) && session.input_revision() == pending.revision, "도착 셸에 새 입력이 있어 이사를 멈췄어요");
            let Some(line) = self.shell_prompt_grid(id) else { return Ok(None) };
            anyhow::ensure!(line.trim().is_empty(), "도착 셸에 입력 중인 글이 있어요");
            self.validate_transfer_identity(&request.target)?;
            anyhow::ensure!(!self.pane_claude_sid.iter().any(|(other, sid)| other != id && sid == &request.session_id
                && self.pty.get(other).is_some_and(|pty| pty.active_agent().is_some())), "이 대화가 도착 기기에서 이미 실행 중이에요");
            anyhow::ensure!(idle_shell(session.shell_pid(), &kasa_pty::fresh_process_table()), "도착 셸에서 다른 작업이 시작됐어요");
            let character = self.ws.lock().unwrap().pane_character.get(id).cloned();
            if let Some(name) = character.filter(|name| !name.is_empty()) {
                kasa_mcp::character::bind_session_character(&request.session_id, &name)?;
            }
            let mut command = restore_agent_command(Some(&request.harness), Some(&request.session_id), true,
                (!request.model.is_empty()).then_some(request.model.as_str()),
                (!request.effort.is_empty()).then_some(request.effort.as_str()));
            if request.bypass {
                let flag = if request.harness == "codex" { "--dangerously-bypass-approvals-and-sandbox" } else { "--dangerously-skip-permissions" };
                command = format!("{} {flag}\r", command.trim_end_matches('\r'));
            }
            // Shell aliases/functions can prepend a different permission mode than the source session used.
            let command = format!("command '{}'{}", request.harness, &command[request.harness.len()..]);
            pending.previous_block = session.blocks_arc().lock().unwrap().back().map(|block| block.id);
            session.send_bytes_guarded(command.as_bytes(), Some(pending.revision))?;
            pending.sent = true;
        }
        if let Some(block) = session.blocks_arc().lock().unwrap().back() {
            anyhow::ensure!(Some(block.id) == pending.previous_block || block.exit_code.is_none(),
                "도착 대화가 준비되기 전에 실행이 끝났어요 (종료 코드 {})", block.exit_code.unwrap_or_default());
        }
        if self.pane_claude_sid.get(id) == Some(&request.session_id)
            && session.active_agent().is_some_and(|agent| agent.as_str() == request.harness)
            && kasa_pty::AgentKind::from_id(&request.harness).is_some_and(|agent| crate::tell_delivery::resumed_input_ready(&session, agent))
        {
            let machine = machine_context()?;
            let mut row = self.transfer_snapshot_gui(machine).sessions.into_iter().find(|row| row.identity.pane_id == *id)
                .ok_or_else(|| anyhow::anyhow!("재개된 대화의 위치를 확인하지 못했어요"))?;
            row.harness = Some(request.harness.clone());
            row.shell_closeable = false;
            return Ok(Some(row));
        }
        Ok(None)
    }

    pub(crate) fn tick_transfer_launches(&mut self) {
        for mut pending in std::mem::take(&mut self.transfer_launches) {
            match self.advance_transfer_launch(&mut pending) {
                Ok(Some(row)) => reply(&pending.sender, Ok(row)),
                Ok(None) => self.transfer_launches.push(pending),
                Err(error) => reply(&pending.sender, Err(error)),
            }
        }
    }

    pub(crate) fn transfer_launch_tick(&self) -> Option<Instant> {
        (!self.transfer_launches.is_empty()).then(|| Instant::now() + std::time::Duration::from_millis(40))
    }
}
