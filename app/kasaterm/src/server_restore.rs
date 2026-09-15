use super::*;
use serde::{Deserialize, Serialize};
use std::sync::{Mutex, Weak};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ServerSpec {
    command: String,
    cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

impl ServerSpec {
    fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            !self.command.trim().is_empty(),
            "server command is required"
        );
        anyhow::ensure!(
            !self.command.chars().any(char::is_control),
            "server command must be one line without control characters"
        );
        anyhow::ensure!(
            !contains_literal_secret(&self.command),
            "keep credentials in environment references or a script, not in the saved command"
        );
        anyhow::ensure!(
            std::path::Path::new(&self.cwd).is_absolute(),
            "server cwd must be absolute"
        );
        anyhow::ensure!(
            !self.cwd.chars().any(char::is_control),
            "server cwd contains control characters"
        );
        anyhow::ensure!(
            self.name
                .as_ref()
                .is_none_or(|s| !s.chars().any(char::is_control)),
            "server name contains control characters"
        );
        Ok(())
    }

    fn shell_input(&self) -> String {
        let cwd = self.cwd.replace('\'', "'\\''");
        format!("(cd '{cwd}' && {})\r", self.command)
    }
}

fn contains_literal_secret(command: &str) -> bool {
    let lowered = command.to_ascii_lowercase();
    if [
        "sk-",
        "ghp_",
        "github_pat_",
        "xoxb-",
        "xoxp-",
        "-----begin",
        "authorization:",
    ]
    .iter()
    .any(|needle| lowered.contains(needle))
    {
        return true;
    }
    let mut sensitive_argument = false;
    for word in command.split_whitespace() {
        if sensitive_argument && !word.contains('$') {
            return true;
        }
        sensitive_argument = false;
        let (key, value) = word
            .split_once('=')
            .map(|(k, v)| (k, Some(v)))
            .unwrap_or((word, None));
        let key = key
            .trim_matches(['\'', '"', '-'])
            .to_ascii_lowercase()
            .replace('-', "_");
        let sensitive = [
            "token",
            "secret",
            "password",
            "passwd",
            "api_key",
            "private_key",
            "database_url",
        ]
        .iter()
        .any(|suffix| key == *suffix || key.ends_with(&format!("_{suffix}")));
        if sensitive {
            if let Some(value) = value {
                if !value.contains('$') {
                    return true;
                }
            } else {
                sensitive_argument = true;
            }
        }
    }
    false
}

#[derive(Debug)]
enum RunState {
    Queued(Instant),
    Starting(Instant),
    Running(u32),
    Ended,
}

pub(crate) struct RegisteredServer {
    spec: ServerSpec,
    session: Weak<kasa_pty::PtySession>,
    state: Mutex<RunState>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ServerOverview {
    pub name: String,
    pub command: String,
    pub cwd: String,
    pub status: String,
}

fn child_process(table: &[(u32, u32, String)], shell: u32) -> Option<u32> {
    table
        .iter()
        .filter(|(_, parent, _)| *parent == shell)
        .map(|(pid, _, _)| *pid)
        .max()
}

fn is_shell_name(name: &str) -> bool {
    matches!(
        name.trim_start_matches('-').trim_end_matches(".exe"),
        "zsh" | "bash" | "fish" | "sh" | "dash" | "ksh" | "tcsh" | "pwsh" | "powershell" | "cmd"
    )
}

fn update_run_state(
    state: &mut RunState,
    child: Option<u32>,
    alive: impl Fn(u32) -> bool,
    now: Instant,
) -> bool {
    match *state {
        RunState::Starting(deadline) => {
            // The shared process cache may still describe shell startup work
            // during the first repaint after command injection.
            if now + Duration::from_secs(9) < deadline {
                return true;
            }
            if let Some(pid) = child {
                *state = RunState::Running(pid);
            } else if now >= deadline {
                *state = RunState::Ended;
            }
        }
        RunState::Running(pid) if !alive(pid) => *state = RunState::Ended,
        _ => {}
    }
    !matches!(state, RunState::Ended)
}

impl RegisteredServer {
    pub(crate) fn overview(&self) -> ServerOverview {
        let status = match self.state.try_lock() {
            Ok(state) => match *state {
                RunState::Queued(_) | RunState::Starting(_) => "대기",
                RunState::Running(_) => "실행",
                RunState::Ended => "종료",
            },
            Err(_) => "수집실패",
        };
        ServerOverview { name: self.spec.name.clone().unwrap_or_else(|| "등록 서버".into()), command: self.spec.command.clone(), cwd: self.spec.cwd.clone(), status: status.into() }
    }

    fn new(spec: ServerSpec, session: &Arc<kasa_pty::PtySession>, process: Option<u32>) -> Self {
        Self {
            spec,
            session: Arc::downgrade(session),
            state: Mutex::new(
                process.map(RunState::Running).unwrap_or_else(|| {
                    RunState::Queued(Instant::now() + Duration::from_millis(900))
                }),
            ),
        }
    }

    fn send_if_due(&self, session: &Arc<kasa_pty::PtySession>) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        let RunState::Queued(at) = *state else {
            return Ok(());
        };
        if Instant::now() < at {
            return Ok(());
        }
        let table = kasa_pty::fresh_process_table();
        let shell = session.shell_pid();
        let idle = shell.is_some_and(|shell| {
            child_process(&table, shell).is_none()
                && table
                    .iter()
                    .any(|(pid, _, name)| *pid == shell && is_shell_name(name))
        });
        if !Weak::ptr_eq(&self.session, &Arc::downgrade(session)) || !idle || session.input_closed()
        {
            *state = RunState::Ended;
            anyhow::bail!("server start canceled because the terminal changed or became busy");
        }
        // The queue belongs to the registration itself: clear/re-registration
        // drops the old recipe before it can type into a replacement terminal.
        session.send_bytes(format!("\x15{}", self.spec.shell_input()).as_bytes())?;
        *state = RunState::Starting(Instant::now() + Duration::from_secs(10));
        Ok(())
    }

    fn live(&self, session: &Arc<kasa_pty::PtySession>) -> bool {
        // Reused pane IDs and replaced shells must never inherit a previous run.
        if !Weak::ptr_eq(&self.session, &Arc::downgrade(session))
            || session.input_closed()
            || session.active_agent().is_some()
        {
            *self.state.lock().unwrap() = RunState::Ended;
            return false;
        }
        let Some(shell) = session.shell_pid() else {
            return false;
        };
        if matches!(*self.state.lock().unwrap(), RunState::Queued(_)) {
            return true;
        }
        let table = kasa_pty::process_table_shared();
        update_run_state(
            &mut self.state.lock().unwrap(),
            child_process(&table, shell),
            |pid| table.iter().any(|(current, _, _)| *current == pid),
            Instant::now(),
        )
    }
}

pub(crate) fn save_server(
    ws: &Workspace,
    surface: &str,
    session: &Arc<kasa_pty::PtySession>,
) -> Option<serde_json::Value> {
    let outer = ws.outer_for_pty(surface)?;
    let pane = ws.panes.get(&outer)?;
    let tab = pane
        .tabs
        .iter()
        .find(|tab| tab.pid.as_deref() == Some(surface))
        .or_else(|| (outer == surface).then(|| pane.tabs.first()).flatten())?;
    let server = tab.server.as_ref()?;
    server
        .live(session)
        .then(|| serde_json::to_value(&server.spec).ok())
        .flatten()
}

impl App {
    pub(crate) fn refresh_registered_servers(&mut self) {
        let mut ws = self.ws.lock().unwrap();
        let mut canceled = false;
        for pane in ws.panes.values_mut() {
            for tab in &mut pane.tabs {
                let keep = tab.server.as_ref().is_none_or(|server| {
                    tab.pid
                        .as_ref()
                        .and_then(|pid| self.pty.get(pid))
                        .is_some_and(|session| {
                            if let Err(error) = server.send_if_due(session) {
                                eprintln!("[server] {error}");
                                canceled = true;
                                return false;
                            }
                            server.live(session)
                        })
                });
                if !keep {
                    tab.server = None;
                    self.session_touched = true;
                }
            }
        }
        drop(ws);
        if canceled {
            self.set_toast("실행 창이 바뀌었거나 사용 중이라 서버 시작을 취소했어요".into());
        }
    }

    pub(crate) fn register_server(
        &mut self,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        let surface = params
            .get("surface")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("surface.server requires surface"))?;
        let pid = surface.to_string();
        let session = self
            .pty
            .get(&pid)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("server surface does not exist"))?;
        for key in ["start", "clear"] {
            anyhow::ensure!(
                params.get(key).is_none_or(|v| v.is_boolean()),
                "{key} must be boolean"
            );
        }
        let clear = params
            .get("clear")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if clear {
            anyhow::ensure!(
                ["command", "cwd", "name", "start"]
                    .iter()
                    .all(|key| params.get(*key).is_none()),
                "clear cannot be combined with launch options"
            );
            let mut ws = self.ws.lock().unwrap();
            let (pane, index) = ws
                .find_tab_by_pty(&pid)
                .ok_or_else(|| anyhow::anyhow!("server tab does not exist"))?;
            pane.tabs[index].server = None;
            self.session_touched = true;
            return Ok(
                serde_json::json!({"surface":pid,"registered":false,"start_scheduled":false}),
            );
        }
        anyhow::ensure!(
            !kasa_mcp::remote::is_remote_pane(&pid),
            "server restoration only supports local terminals"
        );
        anyhow::ensure!(
            !session.input_closed() && session.active_agent().is_none(),
            "server registration requires a live shell or server terminal"
        );
        let start = params
            .get("start")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        if start {
            let mut ws = self.ws.lock().unwrap();
            let (pane, index) = ws
                .find_tab_by_pty(&pid)
                .ok_or_else(|| anyhow::anyhow!("server tab does not exist"))?;
            anyhow::ensure!(pane.tabs[index].server.as_ref().is_none_or(|server| !server.live(&session)), "server is already registered or starting; clear the registration before starting another");
        }
        let command = params
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("server command is required"))?;
        let cwd = params
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .or_else(|| {
                self.pane_current_cwd(&pid)
                    .map(|p| p.to_string_lossy().into_owned())
            })
            .ok_or_else(|| anyhow::anyhow!("server cwd is required"))?;
        let spec = ServerSpec {
            command: command.into(),
            cwd,
            name: params
                .get("name")
                .and_then(|v| v.as_str())
                .map(str::to_owned),
        };
        spec.validate()?;
        anyhow::ensure!(
            std::path::Path::new(&spec.cwd).is_dir(),
            "server cwd is not a directory"
        );
        let table = kasa_pty::fresh_process_table();
        let process = session
            .shell_pid()
            .and_then(|shell| child_process(&table, shell));
        let is_shell = session.shell_pid().is_some_and(|shell| {
            table
                .iter()
                .any(|(pid, _, name)| *pid == shell && is_shell_name(name))
        });
        anyhow::ensure!(
            !start || (process.is_none() && is_shell),
            "terminal is busy; use register-only to keep the current server running"
        );
        anyhow::ensure!(
            start || process.is_some(),
            "register-only requires an already running server"
        );
        self.attach_server(&pid, &session, spec.clone(), process)?;
        self.session_touched = true;
        Ok(serde_json::json!({"surface":pid,"registered":true,"start_scheduled":start}))
    }

    fn attach_server(
        &mut self,
        pid: &str,
        session: &Arc<kasa_pty::PtySession>,
        spec: ServerSpec,
        process: Option<u32>,
    ) -> Result<()> {
        let mut ws = self.ws.lock().unwrap();
        let (pane, index) = ws
            .find_tab_by_pty(pid)
            .ok_or_else(|| anyhow::anyhow!("server tab does not exist"))?;
        if let Some(name) = spec.name.as_ref() {
            pane.tabs[index].title = Some(name.clone());
            pane.tabs[index].title_pinned = true;
        }
        pane.tabs[index].server = Some(RegisteredServer::new(spec, session, process));
        Ok(())
    }

    pub(crate) fn restore_server(
        &mut self,
        pid: &str,
        session: &Arc<kasa_pty::PtySession>,
        rec: &serde_json::Value,
    ) {
        let Some(value) = rec.get("server") else {
            return;
        };
        let attempt = (|| -> Result<()> {
            let spec: ServerSpec = serde_json::from_value(value.clone())?;
            spec.validate()?;
            anyhow::ensure!(
                std::path::Path::new(&spec.cwd).is_dir(),
                "server folder no longer exists"
            );
            {
                let mut ws = self.ws.lock().unwrap();
                if ws.find_tab_by_pty(pid).is_none() {
                    let pane = ws.pane_mut(pid);
                    pane.tabs[0].pid = Some(pid.into());
                }
            }
            self.attach_server(pid, session, spec.clone(), None)?;
            Ok(())
        })();
        if let Err(error) = attempt {
            eprintln!("[restore] server {pid}: {error}");
            self.set_toast(
                "서버를 다시 실행하지 못했어요. 실행 폴더와 등록 내용을 확인해주세요".into(),
            );
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingServer {
    surface_key: String,
    server: ServerSpec,
}

fn matching_records<'a>(
    node: &'a mut serde_json::Value,
    key: &str,
    found: &mut Vec<&'a mut serde_json::Value>,
) {
    if node.get("surface_key").and_then(|v| v.as_str()) == Some(key) {
        found.push(node);
        return;
    }
    match node {
        serde_json::Value::Object(object) => {
            for child in object.values_mut() {
                matching_records(child, key, found);
            }
        }
        serde_json::Value::Array(array) => {
            for child in array {
                matching_records(child, key, found);
            }
        }
        _ => {}
    }
}

fn count_key(node: &serde_json::Value, key: &str) -> usize {
    match node {
        serde_json::Value::Object(object) => {
            usize::from(object.get("surface_key").and_then(|v| v.as_str()) == Some(key))
                + object.values().map(|v| count_key(v, key)).sum::<usize>()
        }
        serde_json::Value::Array(array) => array.iter().map(|v| count_key(v, key)).sum(),
        _ => 0,
    }
}

fn merge_pending(state: &mut serde_json::Value, pending: &PendingServer) -> Result<()> {
    pending.server.validate()?;
    anyhow::ensure!(
        !pending.surface_key.is_empty() && count_key(state, &pending.surface_key) == 1,
        "pending server identity must match exactly one saved surface"
    );
    let mut found = Vec::new();
    matching_records(state, &pending.surface_key, &mut found);
    let rec = &mut found[0];
    anyhow::ensure!(
        rec.get("remote_base").is_none() && rec.get("web_url").is_none(),
        "pending server target must be a local terminal"
    );
    anyhow::ensure!(
        rec.get("was_agent").is_none_or(|v| v.is_null())
            && rec.get("session_id").is_none_or(|v| v.is_null())
            && rec.get("was_claude").and_then(|v| v.as_bool()) != Some(true),
        "pending server target is an agent"
    );
    let value = serde_json::to_value(&pending.server)?;
    anyhow::ensure!(
        rec.get("server").is_none_or(|old| old == &value),
        "pending server conflicts with an existing registration"
    );
    rec.as_object_mut().unwrap().insert("server".into(), value);
    Ok(())
}

pub(crate) fn import_pending_servers(state: &mut serde_json::Value) {
    let Some(session_path) = socket::session_file_path() else {
        return;
    };
    let Some(parent) = session_path.parent() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(parent.join("server-restore-pending")) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let attempt = (|| -> Result<()> {
            anyhow::ensure!(
                entry.file_type()?.is_file(),
                "pending registration must be a regular file"
            );
            let bytes = std::fs::read(&path)?;
            let pending: PendingServer = serde_json::from_slice(&bytes)?;
            let mut merged = state.clone();
            merge_pending(&mut merged, &pending)?;
            // Consume each independent entry only after read-back proves that
            // the launch recipe survived in the main snapshot.
            socket::write_session_state(&merged);
            anyhow::ensure!(
                socket::read_session_state().as_ref() == Some(&merged),
                "pending server snapshot could not be persisted"
            );
            *state = merged;
            anyhow::ensure!(
                std::fs::read(&path)? == bytes,
                "pending registration changed while importing"
            );
            std::fs::remove_file(&path)?;
            Ok(())
        })();
        if let Err(error) = attempt {
            eprintln!(
                "[restore] pending server registration preserved: {}: {error}",
                path.display()
            );
        }
    }
}

pub(crate) fn verification_restore_fixture() -> bool {
    crate::verification_run()
        && std::env::var("KASATERM_SERVER_RESTORE_TEST").as_deref() == Ok("1")
        && [
            "KASATERM_SESSION_FILE",
            "KASATERM_SETTINGS_FILE",
            "KASATERM_WINDOW_FILE",
            "KASATERM_SOCKET_PATH",
        ]
        .iter()
        .all(|key| std::env::var_os(key).is_some_and(|value| !value.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_command_keeps_cwd_quoted_and_only_runs_in_subshell() {
        let spec = ServerSpec {
            command: "npm run dev".into(),
            cwd: "/tmp/a b'c".into(),
            name: None,
        };
        assert_eq!(spec.shell_input(), "(cd '/tmp/a b'\\''c' && npm run dev)\r");
        assert!(spec.validate().is_ok());
        let mut invalid = spec;
        invalid.command.push('\n');
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn ended_server_never_rearms_for_a_different_command() {
        let now = Instant::now();
        let mut state = RunState::Starting(now + Duration::from_secs(1));
        assert!(update_run_state(&mut state, Some(42), |_| true, now));
        assert!(!update_run_state(
            &mut state,
            Some(99),
            |pid| pid == 99,
            now
        ));
        assert!(!update_run_state(&mut state, Some(99), |_| true, now));
    }

    #[test]
    fn failed_start_expires_without_registering_idle_shell() {
        let now = Instant::now();
        let mut state = RunState::Starting(now);
        assert!(!update_run_state(&mut state, None, |_| false, now));
    }

    #[test]
    fn saved_commands_reject_literal_credentials_but_keep_references() {
        assert!(contains_literal_secret("TOKEN=example node server.js"));
        assert!(contains_literal_secret("tunnel --token example"));
        assert!(!contains_literal_secret("tunnel --token \"$TUNNEL_TOKEN\""));
        assert!(!contains_literal_secret("source ./local-server.sh"));
    }

    #[test]
    fn pending_matches_stable_identity_in_tabs_and_rejects_collisions() {
        let pending = PendingServer {
            surface_key: "stable".into(),
            server: ServerSpec {
                command: "npm run dev".into(),
                cwd: "/tmp".into(),
                name: None,
            },
        };
        let mut state = serde_json::json!({"sessions":[{"undocked":[{"pane_id":"%1","surface_key":"other","tabs":[{"pane_id":"%9","surface_key":"stable"}]}]}]});
        merge_pending(&mut state, &pending).unwrap();
        assert_eq!(
            state["sessions"][0]["undocked"][0]["tabs"][0]["server"]["command"],
            "npm run dev"
        );
        assert!(state["sessions"][0]["undocked"][0].get("server").is_none());
        let mut duplicate =
            serde_json::json!({"surface_key":"stable","tabs":[{"surface_key":"stable"}]});
        assert!(merge_pending(&mut duplicate, &pending).is_err());
        assert!(merge_pending(
            &mut serde_json::json!({"pane_id":"%9","surface_key":"different"}),
            &pending
        )
        .is_err());
        assert!(merge_pending(
            &mut serde_json::json!({"surface_key":"stable","was_agent":"codex"}),
            &pending
        )
        .is_err());
    }

    #[test]
    #[cfg(unix)]
    fn inactive_tab_registration_survives_save_but_not_a_replaced_pty() {
        let make_session = |id: &str| {
            Arc::new(
                kasa_pty::PtySession::start(kasa_pty::PtyOptions {
                    shell: Some("/bin/cat".into()),
                    pane_id: id.into(),
                    cols: 80,
                    rows: 24,
                    ..Default::default()
                })
                .unwrap(),
            )
        };
        let root = format!("server-tab-root-{}", uuid::Uuid::new_v4());
        let tab_id = format!("server-tab-child-{}", uuid::Uuid::new_v4());
        let session = make_session(&tab_id);
        let spec = ServerSpec {
            command: "npm run dev".into(),
            cwd: "/tmp".into(),
            name: Some("local server".into()),
        };
        let mut ws = Workspace::default();
        let pane = ws.pane_mut(&root);
        pane.tabs[0].pid = Some(root.clone());
        pane.tabs.push(PaneTab {
            pid: Some(tab_id.clone()),
            server: Some(RegisteredServer::new(spec.clone(), &session, None)),
            ..Default::default()
        });
        pane.active_tab = 0;
        ws.rebuild_pid_map();
        assert!(save_server(&ws, &root, &session).is_none());
        assert_eq!(
            save_server(&ws, &tab_id, &session).unwrap(),
            serde_json::to_value(spec).unwrap()
        );
        let replacement = make_session(&tab_id);
        assert!(save_server(&ws, &tab_id, &replacement).is_none());
        assert!(
            save_server(&ws, &tab_id, &session).is_none(),
            "invalidated recipes cannot rearm"
        );
    }

    #[test]
    #[cfg(unix)]
    fn queued_command_cannot_type_into_a_replacement_terminal() {
        let make_session = || {
            Arc::new(
                kasa_pty::PtySession::start(kasa_pty::PtyOptions {
                    shell: Some("/bin/cat".into()),
                    pane_id: format!("server-queued-{}", uuid::Uuid::new_v4()),
                    cols: 80,
                    rows: 24,
                    ..Default::default()
                })
                .unwrap(),
            )
        };
        let original = make_session();
        let replacement = make_session();
        let server = RegisteredServer::new(
            ServerSpec {
                command: "echo SHOULD_NOT_TYPE".into(),
                cwd: "/tmp".into(),
                name: None,
            },
            &original,
            None,
        );
        *server.state.lock().unwrap() = RunState::Queued(Instant::now());
        assert!(server.send_if_due(&replacement).is_err());
        assert!(matches!(*server.state.lock().unwrap(), RunState::Ended));
    }
}
