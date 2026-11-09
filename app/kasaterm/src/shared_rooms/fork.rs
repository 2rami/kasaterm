//! 받은 포크 자료를 이 기기의 새 탭 하나에서 수신자 소유의 native 대화로 연다.
//!
//! 준비(자료 받기·검증·프로젝트 적용·staging·Codex 공식 fork)는 배경 스레드가 하고, 탭을 열어 명령을 치는
//! 일은 앱 스레드가 한다. 명령은 탭의 입력 revision 이 처음 그대로이고 입력줄이 비었을 때만 한 번 친다 —
//! 사람이 그 사이 무언가 적었으면 덮지 않고 needs_input 으로 멈춘다. 준비 완료는 칸의 대화 결속이 child ID 를
//! 가리키고 입력을 받을 수 있게 된 때다(claude 는 첫 입력 전엔 기록 파일을 만들지 않는다).

use super::*;
use super::service::{Kind, Status};
use kasa_agents::session_fork as native;
use std::path::{Path, PathBuf};
use std::sync::Weak;
use std::time::Duration;

const PREPARE_TIMEOUT: Duration = Duration::from_secs(90);
const PROMPT_WAIT: Duration = Duration::from_secs(30);
const READY_WAIT: Duration = Duration::from_secs(120);

fn ready_wait() -> Duration {
    if crate::verification_run() {
        if let Some(ms) = std::env::var("KASATERM_TEST_SHARED_READY_MS").ok().and_then(|s| s.parse::<u64>().ok()) {
            return Duration::from_millis(ms.clamp(100, 10_000));
        }
    }
    READY_WAIT
}

/// 탭에 칠 것까지 준비된 포크.
#[derive(Clone)]
pub(crate) struct Prepared {
    pub(super) harness: String,
    pub(super) workdir: PathBuf,
    /// POSIX 꼴 명령. 칸 셸 문법으로는 앱 스레드가 바꾼다.
    pub(super) command: String,
    pub(super) child: String,
    pub(super) notes: Vec<String>,
    /// 자료를 받은 계정의 auth scope.
    pub(super) scope: String,
    launch_marker: PathBuf,
    can_resume: bool,
    client: Client,
}

pub(crate) struct Launch {
    pub(super) operation: String,
    pane: String,
    session: Weak<kasa_pty::PtySession>,
    revision: u64,
    prepared: Prepared,
    sent: bool,
    previous_block: Option<u64>,
    deadline: Instant,
}

enum Stop {
    NeedsInput(String),
    Failed(String),
}

impl App {
    pub(crate) fn start_shared_fork(&mut self, fork_id: String, cwd: String, include_project: bool, view_sequence: Option<u64>) {
        match self.shared_fork_operation(&fork_id, cwd, include_project) {
            Ok(id) => {
                if let Some(op) = self.shared_operation_mut(&id) { op.view_sequence = view_sequence; }
                self.shared_rooms.note = Some(format!("포크를 준비하고 있어요 ({})", &id[..8]));
            }
            Err(error) => self.shared_rooms.error = Some(error),
        }
        self.chrome_dirty = true;
        if let Some(w) = &self.window { w.request_redraw(); }
    }

    /// 포크 작업 하나를 시작한다. 같은 포크(id 는 수신자·요청마다 관문이 정한다)의 끝나지 않은 작업이 있으면
    /// 그것을 돌려준다 — 같은 request_id 의 재시도가 탭을 두 개 열지 않는다.
    pub(super) fn shared_fork_operation(&mut self, fork_id: &str, cwd: String, include_project: bool) -> Result<String, String> {
        if !(fork_id.len() == 32 && fork_id.bytes().all(|b| b.is_ascii_hexdigit())) {
            return Err("포크 번호가 올바르지 않아요".into());
        }
        // 빈 폴더 = 「대화만, 새 작업 폴더에서」. 프로젝트를 함께 받을 때는 기존 저장소가 있어야 한다.
        let dir = match cwd.trim() {
            "" if include_project => return Err("프로젝트를 함께 받으려면 이 기기의 저장소 폴더를 골라 주세요".into()),
            "" => None,
            path => {
                let dir = PathBuf::from(path);
                if !dir.is_absolute() || !dir.is_dir() {
                    return Err("포크를 열 폴더가 이 기기에 없어요".into());
                }
                Some(dir)
            }
        };
        let parameters = serde_json::json!({"cwd": dir, "include_project": include_project}).to_string();
        let key = format!("fork\0{fork_id}");
        self.recover_shared_fork(&key)?;
        let (id, fresh) = self.shared_begin_operation(Kind::Fork, key, parameters)?;
        if !fresh {
            return Ok(id);
        }
        let epoch = self.shared_operation(&id).map(|op| op.epoch).unwrap_or_default();
        let expected = self.shared_rooms.client.as_ref().map(|c| c.auth_scope().to_string());
        let proxy = self.proxy.clone();
        let (operation, fork_id) = (id.clone(), fork_id.to_string());
        std::thread::spawn(move || {
            let result = prepare(&fork_id, dir.as_deref(), include_project, expected.as_deref());
            let scope = result.as_ref().ok().map(|prepared| prepared.scope.clone()).or(expected);
            let _ = proxy.send_event(UserEvent::SharedLocal(service::Event::ForkPrepared { operation, epoch, scope, result }));
        });
        Ok(id)
    }

    pub(super) fn begin_shared_launch(&mut self, operation: &str, prepared: Prepared) {
        if !prepared.client.is_current() {
            self.shared_finish(operation, Status::Failed, Some("계정이 바뀌었어요. 다시 시도해 주세요".into()));
            return;
        }
        let activate = self.shared_operation(operation).and_then(|op| op.view_sequence)
            .is_some_and(|sequence| sequence == self.shared_rooms.view_sequence && self.shared_room_active());
        let opened = (|| -> Result<(String, Arc<kasa_pty::PtySession>, bool), String> {
            if let Some((pane, session)) = self.existing_shared_child(&prepared.child)? { return Ok((pane, session, true)); }
            if fork_storage::attempted(&prepared.launch_marker, &prepared.child)? && !prepared.can_resume {
                return Err("앞선 실행을 보낸 기록이 있어요. 기존 포크 탭을 확인해 주세요".into());
            }
            let anchor = self.shared_rooms.return_pane().map(str::to_owned).or_else(|| self.active_user_pane())
                .filter(|pane| self.window_of_pane(pane).is_some())
                .or_else(|| (0..self.windows.len()).filter(|i| self.internal_room_kind_at(*i).is_none())
                    .find_map(|i| self.window_leaves(i).into_iter().next()))
                .ok_or("포크를 열 작업 칸이 없어요. 방을 하나 연 뒤 다시 해 주세요")?;
            let pane = self.spawn_new_tab_at(&anchor, activate, Some(prepared.workdir.clone())).map_err(|e| format!("{e:#}"))?;
            let session = self.pty.get(&pane).cloned().ok_or("새 탭을 확인하지 못했어요")?;
            Ok((pane, session, false))
        })();
        match opened {
            Ok((pane, session, existing)) => {
                if activate && existing { self.focus_surface(&pane); }
                if let Some(op) = self.shared_operation_mut(operation) {
                    op.status = Status::Starting;
                    op.child_session_id = Some(prepared.child.clone());
                    op.notes = prepared.notes.clone();
                    op.command_sent = existing;
                }
                self.shared_rooms.runtime.launches.push(Launch {
                    operation: operation.into(), pane, revision: session.input_revision(), session: Arc::downgrade(&session),
                    prepared, sent: existing, previous_block: None, deadline: Instant::now() + if existing { ready_wait() } else { PROMPT_WAIT },
                });
            }
            Err(error) => self.shared_finish(operation, Status::Failed, Some(error)),
        }
        self.chrome_dirty = true;
        if let Some(w) = &self.window { w.request_redraw(); }
    }

    fn existing_shared_child(&self, child: &str) -> Result<Option<(String, Arc<kasa_pty::PtySession>)>, String> {
        let mut candidates = self.pane_claude_sid.iter().filter(|(_, sid)| sid.as_str() == child)
            .filter_map(|(id, _)| self.pty.get(id).filter(|pty| !pty.output_closed() && pty.active_agent().is_some())
                .map(|pty| (id.clone(), pty.clone())));
        let first = candidates.next();
        if candidates.next().is_some() { return Err("같은 포크 대화가 여러 탭에 있어요. 기존 탭을 확인해 주세요".into()); }
        Ok(first)
    }

    fn recover_shared_fork(&mut self, key: &str) -> Result<(), String> {
        let scope = self.shared_rooms.client.as_ref().filter(|c| c.is_current()).map(Client::scope);
        let previous = self.shared_rooms.runtime.operations.iter()
            .find(|op| op.key == key && Some(op.scope.as_str()) == scope && op.command_sent && op.status == Status::Failed).cloned();
        if let Some(previous) = previous {
            if let Some(child) = previous.child_session_id {
                if let Some((pane, session)) = self.existing_shared_child(&child)? {
                    if session.active_agent().is_some_and(|agent| crate::tell_delivery::resumed_input_ready(&session, agent)) {
                        let address = kasa_collab::board_service::address(&pane, Some(&child)).map_err(|e| e.to_string())?;
                        if let Some(op) = self.shared_operation_mut(&previous.id) { op.address = Some(address); }
                        self.shared_finish(&previous.id, Status::Ready, None);
                    }
                }
            }
        }
        Ok(())
    }

    fn advance_shared_launch(&mut self, launch: &mut Launch) -> Result<Option<serde_json::Value>, Stop> {
        if !launch.prepared.client.is_current() { return Err(Stop::Failed("계정이 바뀌었어요. 다시 시도해 주세요".into())); }
        let id = launch.pane.clone();
        let session = self.pty.get(&id).cloned().ok_or_else(|| Stop::Failed("포크 탭이 닫혔어요".into()))?;
        if !Weak::ptr_eq(&Arc::downgrade(&session), &launch.session) {
            return Err(Stop::Failed("포크 탭의 셸이 바뀌었어요".into()));
        }
        if session.output_closed() {
            return Err(Stop::Failed("포크 탭의 셸이 끝났어요".into()));
        }
        if !launch.sent {
            if let Some((pane, session)) = self.existing_shared_child(&launch.prepared.child).map_err(Stop::Failed)? {
                launch.pane = pane;
                launch.session = Arc::downgrade(&session);
                launch.sent = true;
                if let Some(op) = self.shared_operation_mut(&launch.operation) { op.command_sent = true; }
                return Ok(None);
            }
            if session.input_revision() != launch.revision || (launch.revision != 0 && session.input_draft_present()) {
                return Err(Stop::NeedsInput("새 탭에 입력이 있어 포크 명령을 치지 않았어요. 탭을 확인해 주세요".into()));
            }
            if Instant::now() >= launch.deadline {
                return Err(Stop::Failed("새 탭의 셸이 준비되지 않았어요".into()));
            }
            let Some(line) = self.shell_prompt_grid(&id) else { return Ok(None) };
            if !line.trim().is_empty() {
                return Err(Stop::NeedsInput("새 탭 입력줄에 글이 있어 포크 명령을 치지 않았어요".into()));
            }
            if !crate::transfer_endpoints::idle_shell(session.shell_pid(), &kasa_pty::fresh_process_table()) {
                return Ok(None);
            }
            let shell = session.input_shell_name().as_deref()
                .and_then(kasa_socket::shell_line::ShellKind::from_process_name)
                .unwrap_or(kasa_socket::shell_line::ShellKind::Posix);
            // 사람이 claude·codex 에 건 별칭·함수를 비켜 칸 셰임을 직접 부른다. 권한은 수신자 설정 그대로다.
            let line = kasa_socket::shell_line::direct_line(shell, &launch.prepared.command).map_err(|e| Stop::Failed(e.to_string()))?;
            if !launch.prepared.client.is_current() { return Err(Stop::Failed("계정이 바뀌었어요. 다시 시도해 주세요".into())); }
            fork_storage::claim(&launch.prepared.launch_marker, &launch.prepared.child, launch.prepared.can_resume).map_err(Stop::NeedsInput)?;
            if let Some(op) = self.shared_operation_mut(&launch.operation) { op.command_sent = true; }
            launch.previous_block = session.blocks_arc().lock().unwrap().back().map(|block| block.id);
            session.send_bytes_guarded(format!("{line}\r").as_bytes(), Some(launch.revision))
                .map_err(|_| Stop::NeedsInput("새 탭에 입력이 있어 포크 명령을 치지 않았어요".into()))?;
            launch.sent = true;
            launch.deadline = Instant::now() + ready_wait();
            return Ok(None);
        }
        if let Some(block) = session.blocks_arc().lock().unwrap().back() {
            if Some(block.id) != launch.previous_block && block.exit_code.is_some() {
                return Err(Stop::Failed(format!("포크 대화가 준비되기 전에 실행이 끝났어요 (종료 코드 {})", block.exit_code.unwrap_or_default())));
            }
        }
        let harness = launch.prepared.harness.as_str();
        let bound = self.pane_claude_sid.get(&id) == Some(&launch.prepared.child);
        let running = session.active_agent().is_some_and(|agent| agent.as_str() == harness);
        let ready = kasa_pty::AgentKind::from_id(harness).is_some_and(|agent| crate::tell_delivery::resumed_input_ready(&session, agent));
        if bound && running && ready {
            let address = kasa_collab::board_service::address(&id, Some(&launch.prepared.child)).map_err(|e| Stop::Failed(e.to_string()))?;
            return Ok(Some(address));
        }
        if Instant::now() >= launch.deadline {
            return Err(Stop::Failed("포크 대화가 새 탭에서 열리는 것을 확인하지 못했어요. 탭을 확인해 주세요".into()));
        }
        Ok(None)
    }

    pub(super) fn tick_shared_launches(&mut self) {
        for mut launch in std::mem::take(&mut self.shared_rooms.runtime.launches) {
            match self.advance_shared_launch(&mut launch) {
                Ok(Some(address)) => {
                    if let Some(op) = self.shared_operation_mut(&launch.operation) {
                        op.address = Some(address);
                    }
                    self.shared_rooms.note = Some("포크한 대화를 새 탭에서 열었어요".into());
                    self.shared_finish(&launch.operation, Status::Ready, None);
                }
                Ok(None) => self.shared_rooms.runtime.launches.push(launch),
                Err(Stop::NeedsInput(why)) => self.shared_finish(&launch.operation, Status::NeedsInput, Some(why)),
                Err(Stop::Failed(why)) => self.shared_finish(&launch.operation, Status::Failed, Some(why)),
            }
        }
    }
}

fn claude_projects_root() -> Option<PathBuf> {
    if crate::verification_run() { return kasa_socket::isolated_collab_root().map(|root| root.join("native-claude/projects")); }
    std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()).map(PathBuf::from)
        .or_else(|| kasa_socket::home_dir().map(|home| home.join(".claude")))
        .map(|root| root.join("projects"))
}

fn codex_home() -> Option<PathBuf> {
    if crate::verification_run() { return kasa_socket::isolated_collab_root().map(|root| root.join("native-codex")); }
    std::env::var_os("CODEX_HOME").filter(|v| !v.is_empty()).map(PathBuf::from)
        .or_else(|| kasa_socket::home_dir().map(|home| home.join(".codex")))
}

/// 설치된 codex 가 경로 포크를 받는지 — 실험 스키마를 한 번 뽑아 본다. 실행 파일이 바뀌면 다시 본다.
fn codex_support(binary: &Path) -> native::CodexForkSupport {
    type Seen = Option<(PathBuf, Option<std::time::SystemTime>, native::CodexForkSupport)>;
    static SEEN: Mutex<Seen> = Mutex::new(None);
    let stamp = std::fs::metadata(binary).and_then(|m| m.modified()).ok();
    if let Some((path, at, support)) = SEEN.lock().unwrap().as_ref() {
        if path == binary && *at == stamp {
            return support.clone();
        }
    }
    let out = std::env::temp_dir().join(format!("kasaterm-codex-schema-{}", service::new_operation_id()));
    let support = match kasa_socket::process::no_window_command(binary)
        .args(["app-server", "generate-json-schema", "--experimental", "--out"]).arg(&out)
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
        .status()
    {
        Ok(status) if status.success() => native::codex_fork_support(&out),
        _ => native::CodexForkSupport::Unknown("codex 스키마를 만들지 못했어요".into()),
    };
    let _ = std::fs::remove_dir_all(&out);
    *SEEN.lock().unwrap() = Some((binary.to_path_buf(), stamp, support.clone()));
    support
}

fn fixture_program(harness: &str) -> Result<Option<PathBuf>, String> {
    if !crate::verification_run() { return Ok(None); }
    let key = format!("KASATERM_SHARED_FIXTURE_{}", harness.to_ascii_uppercase());
    let path = std::env::var_os(key).map(PathBuf::from).filter(|p| p.is_absolute() && p.is_file())
        .ok_or("검증용 하네스 실행 파일을 지정해 주세요")?;
    Ok(Some(path))
}

fn command_word(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn prepare(fork_id: &str, cwd: Option<&Path>, include_project: bool, expected_scope: Option<&str>) -> Result<Prepared, String> {
    let started = Instant::now();
    let client = capture_client().map_err(|e| e.to_string())?;
    if expected_scope.is_some_and(|scope| scope != client.auth_scope()) {
        return Err("계정이 바뀌었어요. 다시 포크해 주세요.".into());
    }
    let fork = kasa_remote::shared_rooms::client::run(client.read_fork(fork_id)).map_err(|e| e.to_string())?;
    if fork.info.id != fork_id { return Err("받은 포크 번호가 요청과 달라요".into()); }
    if !client.is_current() {
        return Err("계정이 바뀌었어요. 다시 포크해 주세요.".into());
    }
    let snapshot = &fork.checkpoint.native;
    native::verify_snapshot(snapshot, &native::ExportLimits::default()).map_err(|e| e.to_string())?;
    let fixture = fixture_program(&snapshot.harness)?;
    let mut notes = fork.checkpoint.omissions.clone();
    let (workdir, launch_marker) = fork_storage::workspace(&service::shared_dir()?, client.scope(), &fork, cwd, include_project)?;
    if cwd.is_none() { notes.push(format!("새 작업 폴더에서 열어요: {}", workdir.display())); }
    if include_project { if let Some(project) = &fork.checkpoint.project { notes.push(format!("프로젝트 캡처 시각: {}", project.captured_at_ms)); } }
    let identity = native::ForkIdentity {
        room_id: fork.info.room_id.clone(), session_id: fork.info.session_id.clone(),
        checkpoint_id: fork.info.checkpoint_id.clone(), request_id: fork.info.id.clone(),
    };
    let staging = service::shared_dir()?.join("forks").join(client.scope());
    match snapshot.harness.as_str() {
        "claude" => {
            let projects = claude_projects_root().ok_or("claude 설정 폴더를 찾지 못했어요")?;
            let staged = native::stage_snapshot(&staging, &projects, &identity, snapshot, &workdir).map_err(|e| e.to_string())?;
            let program = fixture.as_deref().and_then(|p| p.to_str()).unwrap_or("claude");
            let (command, child, can_resume) = match native::claude_fork_plan(&staged, &projects).map_err(|e| e.to_string())? {
                native::ClaudeForkLaunch::Launch(plan) => (plan.posix_command(program), plan.child_session_id, false),
                native::ClaudeForkLaunch::AlreadyForked(prov) => {
                    let child = prov.child_session_id.ok_or("이전 포크의 대화 번호를 확인하지 못했어요")?;
                    (format!("{} --resume {child}", command_word(program)), child, true)
                }
            };
            Ok(Prepared { harness: "claude".into(), workdir, command, child, notes, scope: client.auth_scope().into(), launch_marker, can_resume, client })
        }
        "codex" => {
            let home = codex_home().ok_or("codex 홈을 찾지 못했어요")?;
            let binary = fixture.clone().unwrap_or_else(crate::codex_binary);
            let support = codex_support(&binary);
            let staged = native::stage_codex_snapshot(&staging, &home, &identity, snapshot, &workdir).map_err(|e| e.to_string())?;
            let child = match native::codex_fork_plan(&staged, &home, &support).map_err(|e| e.to_string())? {
                native::CodexForkLaunch::AlreadyForked(prov) => prov.child_session_id.ok_or("이전 포크의 대화 번호를 확인하지 못했어요")?,
                native::CodexForkLaunch::Launch(plan) => {
                    if !client.is_current() {
                        return Err("계정이 바뀌었어요. 다시 포크해 주세요.".into());
                    }
                    let left = PREPARE_TIMEOUT.saturating_sub(started.elapsed()).max(Duration::from_secs(10));
                    let mut app_server = kasa_socket::process::no_window_command(&binary);
                    app_server.args(["app-server", "--config=features.daemon_auto_start=false"]);
                    let result = native::run_codex_fork(&mut app_server, &plan, left).map_err(|e| e.to_string())?;
                    match native::confirm_codex_child(&plan, &home, Some(&result)) {
                        native::ChildState::Confirmed { session_id, .. } => session_id,
                        native::ChildState::Pending => result["thread"]["id"].as_str().map(str::to_owned).ok_or("새 대화 번호를 받지 못했어요")?,
                        native::ChildState::Mismatch(why) => return Err(why),
                    }
                }
            };
            let program = fixture.as_deref().and_then(|p| p.to_str()).unwrap_or("codex");
            Ok(Prepared { harness: "codex".into(), workdir, command: format!("{} resume {child}", command_word(program)),
                child, notes, scope: client.auth_scope().into(), launch_marker, can_resume: true, client })
        }
        other => Err(format!("{other} 대화는 포크할 수 없어요")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_program_paths_are_quoted_as_one_argument() {
        let path = "/fixture/it's a program";
        assert_eq!(kasa_socket::shell_line::split_posix_words(&command_word(path)).unwrap(), [path]);
    }
}
