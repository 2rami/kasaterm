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

    /// 그 셸 문법으로 「cwd 에서 명령을 돌리고, 끝나면 셸은 원래 자리」 입력. 명령은 사용자가 그 셸에
    /// 맞춰 적은 그대로 넣는다 — 다른 셸 문법으로 옮기지 않는다. cwd 로 못 가면 명령을 돌리지 않는다.
    fn shell_input(&self, shell: ShellKind) -> String {
        let command = &self.command;
        match shell {
            ShellKind::Posix => {
                let cwd = self.cwd.replace('\'', "'\\''");
                format!("(cd '{cwd}' && {command})\r")
            }
            // 위치는 런스페이스 하나라 하위 범위로는 못 가둔다 — 스택에 넣고 finally 로 꺼낸다(Ctrl+C 에도 돈다).
            // 명령 뒤에서 줄을 바꾸는 건 명령 속 `#` 가 닫는 중괄호까지 주석으로 먹지 않게다. 첫 Enter 는
            // 중괄호가 안 닫혀 실행되지 않고 이어 쓰기가 된다.
            ShellKind::PowerShell => {
                let cwd = powershell_quote(&self.cwd);
                format!("if (Push-Location -LiteralPath {cwd} -PassThru) {{ try {{ {command}\r}} finally {{ Pop-Location }} }}\r")
            }
            // cmd 엔 서브셸이 없고 pushd/popd 는 Ctrl+C 에 줄 나머지가 안 돌아 칸이 서버 폴더에 남는다 — 그래서
            // 자식 cmd 로 돌린다. 폴더는 도우미가 WorkingDirectory 로 주고 명령은 `cmd /d /s /c` 에 그대로
            // 넘긴다(/s 는 바깥 따옴표만 벗긴다). cmd 줄을 건너는 건 base64 뿐이라 경로·명령을 cmd 가 다시
            // 해석할 틈이 없다. 도우미·cmd 는 절대경로로 불러 현재 폴더의 같은 이름 실행 파일을 타지 않는다.
            ShellKind::Cmd => {
                use base64::Engine as _;
                let script = cmd_helper_script(&self.cwd, command);
                let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
                let encoded = base64::engine::general_purpose::STANDARD.encode(utf16);
                format!("\"%SystemRoot%\\System32\\WindowsPowerShell\\v1.0\\powershell.exe\" -NoLogo -NoProfile -NonInteractive -EncodedCommand {encoded}\r")
            }
        }
    }
}

/// cmd 칸의 서버를 자식 cmd 로 띄우는 PowerShell 도우미. 사용자 명령은 PowerShell 문자열 값일 뿐 실행은
/// cmd 가 한다. 콘솔을 물려받아 출력·입력·Ctrl+C 가 그 칸으로 오고, 끝나면 그 종료 코드로 나간다.
fn cmd_helper_script(cwd: &str, command: &str) -> String {
    let cwd = powershell_quote(cwd);
    let command = powershell_quote(command);
    format!(
        "$ErrorActionPreference = 'Stop'; try {{ \
         $s = New-Object System.Diagnostics.ProcessStartInfo (Join-Path ([Environment]::SystemDirectory) 'cmd.exe'); \
         $s.Arguments = '/d /s /c \"' + {command} + '\"'; $s.WorkingDirectory = {cwd}; $s.UseShellExecute = $false; \
         $p = [System.Diagnostics.Process]::Start($s); $p.WaitForExit(); exit $p.ExitCode \
         }} catch {{ [Console]::Error.WriteLine($_.Exception.Message); exit 1 }}"
    )
}

/// 서버 명령을 받을 셸의 문법. 저장 형식엔 없다 — 보낼 때 그 칸 셸 프로세스 이름에서 고른다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShellKind {
    Posix,
    PowerShell,
    Cmd,
}

impl ShellKind {
    fn from_process_name(name: &str) -> Option<Self> {
        let name = name.trim_start_matches('-').to_ascii_lowercase();
        match name.strip_suffix(".exe").unwrap_or(&name) {
            "zsh" | "bash" | "fish" | "sh" | "dash" | "ksh" | "tcsh" => Some(Self::Posix),
            "pwsh" | "powershell" => Some(Self::PowerShell),
            "cmd" => Some(Self::Cmd),
            _ => None,
        }
    }

    /// 칸 입력줄에 남은 글자를 먼저 지우는 키. Ctrl+U 는 readline 의 줄 지우기라 POSIX 셸에만 보낸다.
    fn clear_line(self) -> &'static str {
        match self {
            Self::Posix => "\x15",
            Self::PowerShell | Self::Cmd => "",
        }
    }
}

/// PowerShell 작은따옴표 문자열. 그 안에서 특별한 건 작은따옴표뿐인데, PowerShell 은 굽은 작은따옴표
/// (U+2018~U+201B)도 같은 따옴표로 읽어서 둘 다 겹쳐 쓴다.
fn powershell_quote(text: &str) -> String {
    let mut out = String::from("'");
    for c in text.chars() {
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
            out.push(c);
        }
        out.push(c);
    }
    out.push('\'');
    out
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
    ShellKind::from_process_name(name).is_some()
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
        let idle_shell = session.shell_pid().filter(|shell| child_process(&table, *shell).is_none()).and_then(|shell| {
            table
                .iter()
                .find(|(pid, _, _)| *pid == shell)
                .and_then(|(_, _, name)| ShellKind::from_process_name(name))
        });
        let Some(shell) = idle_shell.filter(|_| {
            Weak::ptr_eq(&self.session, &Arc::downgrade(session)) && !session.input_closed()
        }) else {
            *state = RunState::Ended;
            anyhow::bail!("server start canceled because the terminal changed or became busy");
        };
        // The queue belongs to the registration itself: clear/re-registration
        // drops the old recipe before it can type into a replacement terminal.
        session.send_bytes(format!("{}{}", shell.clear_line(), self.spec.shell_input(shell)).as_bytes())?;
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

    /// 서버 cwd 는 이 기기의 절대경로여야 한다 — Windows 에서 `/tmp` 는 절대경로가 아니다.
    const ROOT: &str = if cfg!(windows) { r"C:\tmp" } else { "/tmp" };

    #[test]
    fn server_command_keeps_cwd_quoted_and_only_runs_in_subshell() {
        let (cwd, quoted) = if cfg!(windows) {
            (r"C:\tmp\a b'c", r"'C:\tmp\a b'\''c'")
        } else {
            ("/tmp/a b'c", r"'/tmp/a b'\''c'")
        };
        let spec = ServerSpec {
            command: "npm run dev".into(),
            cwd: cwd.into(),
            name: None,
        };
        assert_eq!(spec.shell_input(ShellKind::Posix), format!("(cd {quoted} && npm run dev)\r"));
        assert!(spec.validate().is_ok());
        let mut invalid = spec;
        invalid.command.push('\n');
        assert!(invalid.validate().is_err());
        let foreign = if cfg!(windows) { "/tmp" } else { r"C:\tmp" };
        for cwd in ["relative/dir", foreign] {
            let spec = ServerSpec { command: "npm run dev".into(), cwd: cwd.into(), name: None };
            assert!(spec.validate().is_err(), "{cwd}");
        }
    }

    #[test]
    fn each_shell_gets_its_own_grammar_and_keeps_the_command_verbatim() {
        let spec = |cwd: &str, command: &str| ServerSpec { command: command.into(), cwd: cwd.into(), name: None };
        let ps = spec(r"C:\srv\한글 it's ‘x’", "npm run dev # 주석").shell_input(ShellKind::PowerShell);
        assert_eq!(
            ps,
            "if (Push-Location -LiteralPath 'C:\\srv\\한글 it''s ‘‘x’’' -PassThru) { try { npm run dev # 주석\r} finally { Pop-Location } }\r"
        );
        assert_eq!(ShellKind::Posix.clear_line(), "\x15");
        assert_eq!(ShellKind::PowerShell.clear_line(), "");
        assert_eq!(ShellKind::Cmd.clear_line(), "");
    }

    /// cmd 줄은 절대경로 도우미 + base64 뿐이고, 풀어 보면 cwd·명령이 PowerShell 문자열 값으로만 들어 있다.
    #[test]
    fn cmd_runs_the_command_verbatim_in_a_child_cmd_from_the_helper() {
        use base64::Engine as _;
        let cwd = r"C:\srv\%PATH% it's";
        let command = r#"echo "a & b" 'q' & npm run dev"#;
        let line = ServerSpec { command: command.into(), cwd: cwd.into(), name: None }.shell_input(ShellKind::Cmd);
        let prefix = "\"%SystemRoot%\\System32\\WindowsPowerShell\\v1.0\\powershell.exe\" -NoLogo -NoProfile -NonInteractive -EncodedCommand ";
        let encoded = line.strip_prefix(prefix).and_then(|rest| rest.strip_suffix('\r')).expect(&line);
        assert!(encoded.chars().all(|c| c.is_ascii_alphanumeric() || "+/=".contains(c)), "cmd 가 해석할 글자가 없다");
        let bytes = base64::engine::general_purpose::STANDARD.decode(encoded).unwrap();
        let units: Vec<u16> = bytes.chunks(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
        let script = String::from_utf16(&units).unwrap();
        assert_eq!(script, cmd_helper_script(cwd, command));
        assert!(script.contains(r#"$s.Arguments = '/d /s /c "' + 'echo "a & b" ''q'' & npm run dev' + '"'"#), "{script}");
        assert!(script.contains(r"$s.WorkingDirectory = 'C:\srv\%PATH% it''s'"), "{script}");
        assert!(script.contains("[Environment]::SystemDirectory"), "cmd 도 절대경로로");
    }

    #[test]
    fn shell_kind_comes_from_the_shell_process_name() {
        for (name, kind) in [
            ("-zsh", Some(ShellKind::Posix)),
            ("bash", Some(ShellKind::Posix)),
            ("pwsh.exe", Some(ShellKind::PowerShell)),
            ("PowerShell.exe", Some(ShellKind::PowerShell)),
            ("cmd.exe", Some(ShellKind::Cmd)),
            ("CMD.EXE", Some(ShellKind::Cmd)),
            ("node.exe", None),
            ("claude", None),
        ] {
            assert_eq!(ShellKind::from_process_name(name), kind, "{name}");
        }
    }

    /// 만든 줄을 그 셸에 실제로 먹이고 `after` 를 이어, 줄이 끝난 뒤 셸이 어디 서 있는지 파일로 남긴다.
    /// 경로 문자열 대신 파일로 보는 건 짧은 이름(RUNNER~1)·콘솔 코드 페이지에 비교가 흔들리지 않게다.
    fn run_line(shell: ShellKind, program: &str, line: &str, start: &std::path::Path) -> std::process::Output {
        // 키 입력의 Enter 를 스크립트의 줄바꿈으로 — PowerShell 줄은 명령 뒤에 한 번 더 Enter 가 있다.
        let line = line.trim_end_matches('\r').replace('\r', "\n");
        let mut command = std::process::Command::new(program);
        command.current_dir(start);
        match shell {
            ShellKind::Posix => {
                command.arg("-c").arg(format!("{line}; echo x > after.txt"));
            }
            ShellKind::PowerShell => {
                command.args(["-NoProfile", "-NonInteractive", "-Command"]).arg(format!(
                    "try {{ {line} }} catch {{ }}; Set-Content -LiteralPath after.txt -Value x"
                ));
            }
            ShellKind::Cmd => {
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    command.raw_arg(format!("/d /s /c \"{line} & echo x> after.txt\""));
                }
            }
        }
        command.output().unwrap()
    }

    /// 공백·한글·작은따옴표가 든 폴더에서 성공·실패하는 명령을 그 OS 의 실제 셸로 돌린다 — 명령은 그
    /// 폴더에서 돌고, 끝나면(실패해도) 셸은 원래 자리이고, 폴더가 없으면 명령을 아예 안 돌린다.
    #[test]
    fn server_line_runs_in_its_cwd_and_leaves_the_shell_where_it_was() {
        let mut shells: Vec<(ShellKind, String, Vec<&str>)> = Vec::new();
        if cfg!(windows) {
            shells.push((ShellKind::Cmd, "cmd.exe".into(), vec![
                "echo (ok)> here.txt",
                r#"echo "a & b" 'q'> here.txt"#,
                "echo ok> here.txt && dir kasaterm-missing-xyz",
            ]));
            let ps = vec![
                "Set-Content -LiteralPath here.txt -Value ok",
                "Set-Content -LiteralPath here.txt -Value ok # 주석이 닫는 중괄호를 먹으면 안 된다",
                "Set-Content -LiteralPath here.txt -Value ok; cmd /c exit 3",
                "Set-Content -LiteralPath here.txt -Value ok; throw 'boom'",
            ];
            shells.push((ShellKind::PowerShell, "powershell.exe".into(), ps.clone()));
            let pwsh = std::process::Command::new("where.exe").arg("pwsh").output();
            match pwsh.ok().filter(|o| o.status.success()) {
                Some(found) => {
                    let path = String::from_utf8_lossy(&found.stdout).lines().next().unwrap_or("pwsh.exe").trim().to_string();
                    shells.push((ShellKind::PowerShell, path, ps));
                }
                None => eprintln!("pwsh 가 없어 Windows PowerShell 로만 본다"),
            }
        } else {
            shells.push((ShellKind::Posix, "/bin/sh".into(), vec!["echo ok > here.txt", "echo ok > here.txt && false"]));
        }
        let base = std::env::temp_dir().join(format!("kasaterm-server-line-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let start = base.join("start");
        let target = base.join("srv 한글 it's %PATH%");
        std::fs::create_dir_all(&start).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        let clean = || {
            for dir in [&start, &target] {
                for f in ["here.txt", "after.txt"] {
                    let _ = std::fs::remove_file(dir.join(f));
                }
            }
        };
        for (kind, program, commands) in &shells {
            for command in commands {
                clean();
                let spec = ServerSpec { command: command.to_string(), cwd: target.to_string_lossy().into_owned(), name: None };
                let out = run_line(*kind, program, &spec.shell_input(*kind), &start);
                let why = format!("{program} `{command}`: {}", String::from_utf8_lossy(&out.stderr));
                assert!(target.join("here.txt").exists() && !start.join("here.txt").exists(), "명령이 그 폴더에서 돌아야 한다 — {why}");
                assert!(start.join("after.txt").exists() && !target.join("after.txt").exists(), "끝나면 셸은 원래 자리 — {why}");
            }
            clean();
            let missing = ServerSpec { command: commands[0].to_string(), cwd: base.join("없는 폴더 it's").to_string_lossy().into_owned(), name: None };
            let out = run_line(*kind, program, &missing.shell_input(*kind), &start);
            let why = format!("{program}: {}", String::from_utf8_lossy(&out.stderr));
            assert!(!start.join("here.txt").exists(), "폴더가 없으면 명령을 안 돌린다 — {why}");
            assert!(start.join("after.txt").exists(), "셸은 그대로 다음 줄을 받는다 — {why}");
        }
        let _ = std::fs::remove_dir_all(&base);
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
                cwd: ROOT.into(),
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
