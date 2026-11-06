//! 줄 단위 JSON-RPC 연결.
//!
//! 읽기·쓰기 스레드를 하나씩 둔다. 응답은 요청마다 채널로, 알림과 에이전트의 요청은 이벤트
//! 채널 하나로 흘린다. 권한 요청을 콜백으로 받지 않는 이유: 사람(폰 승인)을 기다리는 동안
//! 읽기 스레드가 멈추면 같은 연결의 다른 세션 이벤트까지 막힌다.

use std::collections::HashMap;
use std::fmt;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::event::{self, Event, NewSession, PromptOutcome};
use crate::launch::LaunchSpec;

const PROTOCOL_VERSION: u64 = 1;
/// 첫 실행의 npx 내려받기가 initialize 를 20초 넘게 붙잡았다(2026-11 실측). 그 몇 배를 준다.
const CONTROL_TIMEOUT: Duration = Duration::from_secs(120);
const STDERR_TAIL_BYTES: usize = 8 * 1024;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    /// 에이전트가 오류 응답을 보냈다.
    Rpc { code: i64, message: String },
    /// 응답 전에 연결이 끊겼다(에이전트 종료·stdout 닫힘).
    Disconnected,
    Timeout,
    /// 보낼 값이나 받은 값이 기대한 꼴이 아니다.
    Invalid(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io: {e}"),
            Self::Rpc { code, message } => write!(f, "agent error {code}: {message}"),
            Self::Disconnected => f.write_str("agent disconnected"),
            Self::Timeout => f.write_str("agent did not answer in time"),
            Self::Invalid(why) => write!(f, "invalid: {why}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// 에이전트가 보낸 요청의 id. 숫자일 수도 글자일 수도 있어 원문째 들고 있다가 그대로 돌려준다.
#[derive(Debug, Clone, PartialEq)]
pub struct RequestId(Value);

#[cfg(test)]
impl RequestId {
    pub(crate) fn for_test(id: Value) -> Self {
        Self(id)
    }
}

type Reply = Sender<Result<Value, Error>>;

#[derive(Default)]
struct PendingState {
    waiting: HashMap<u64, Reply>,
    closed: bool,
}

enum Outgoing {
    Line(Vec<u8>),
    Close,
}

/// 보낸 요청 하나의 답을 기다리는 손잡이.
pub struct Pending {
    rx: Receiver<Result<Value, Error>>,
}

impl Pending {
    fn failed(e: Error) -> Self {
        let (tx, rx) = mpsc::channel();
        let _ = tx.send(Err(e));
        Self { rx }
    }

    pub fn wait(self) -> Result<Value, Error> {
        self.rx.recv().unwrap_or(Err(Error::Disconnected))
    }

    pub fn wait_timeout(&self, timeout: Duration) -> Result<Value, Error> {
        match self.rx.recv_timeout(timeout) {
            Ok(r) => r,
            Err(RecvTimeoutError::Timeout) => Err(Error::Timeout),
            Err(RecvTimeoutError::Disconnected) => Err(Error::Disconnected),
        }
    }

    /// 아직이면 `None`. 호스트의 주기 루프가 막히지 않고 들여다볼 때 쓴다.
    pub fn try_take(&self) -> Option<Result<Value, Error>> {
        match self.rx.try_recv() {
            Ok(r) => Some(r),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(Error::Disconnected)),
        }
    }
}

pub struct Connection {
    out: Sender<Outgoing>,
    pending: Arc<Mutex<PendingState>>,
    next_id: AtomicU64,
    child: Option<Child>,
    stderr_tail: Arc<Mutex<String>>,
}

impl Connection {
    /// 어댑터를 띄운다. `cwd` 는 자식의 작업 폴더일 뿐이고, 세션의 작업 폴더는 `new_session` 이 정한다.
    pub fn spawn(spec: &LaunchSpec, cwd: &Path) -> Result<(Self, Receiver<Event>), Error> {
        let mut cmd = Command::new(&spec.program);
        cmd.args(&spec.args)
            .envs(spec.env.iter().map(|(k, v)| (k, v)))
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let mut child = cmd.spawn()?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            kill_tree(&mut child);
            return Err(Error::Invalid("child stdio not piped".into()));
        };
        let (mut conn, events) = match Self::from_streams(stdout, stdin) {
            Ok(started) => started,
            Err(e) => {
                kill_tree(&mut child);
                return Err(e);
            }
        };
        let tail = conn.stderr_tail.clone();
        thread::Builder::new()
            .name("acp-stderr".into())
            .spawn(move || stderr_loop(stderr, &tail))?;
        conn.child = Some(child);
        Ok((conn, events))
    }

    /// 이미 열린 길(시험의 파이프, 원격 전송) 위에 연결을 세운다.
    pub fn from_streams(
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
    ) -> Result<(Self, Receiver<Event>), Error> {
        let (out_tx, out_rx) = mpsc::channel();
        let (ev_tx, ev_rx) = mpsc::channel();
        let pending = Arc::new(Mutex::new(PendingState::default()));
        thread::Builder::new()
            .name("acp-write".into())
            .spawn(move || write_loop(writer, &out_rx))?;
        let reader_pending = pending.clone();
        let reply = out_tx.clone();
        thread::Builder::new()
            .name("acp-read".into())
            .spawn(move || read_loop(reader, &reader_pending, &reply, &ev_tx))?;
        let conn = Self {
            out: out_tx,
            pending,
            next_id: AtomicU64::new(1),
            child: None,
            stderr_tail: Arc::default(),
        };
        Ok((conn, ev_rx))
    }

    pub fn request(&self, method: &str, params: Value) -> Pending {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        {
            let Ok(mut state) = self.pending.lock() else {
                return Pending::failed(Error::Disconnected);
            };
            if state.closed {
                return Pending::failed(Error::Disconnected);
            }
            state.waiting.insert(id, tx);
        }
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        if self.out.send(Outgoing::Line(encode(&msg))).is_err() {
            if let Ok(mut state) = self.pending.lock() {
                state.waiting.remove(&id);
            }
            return Pending::failed(Error::Disconnected);
        }
        Pending { rx }
    }

    pub fn notify(&self, method: &str, params: Value) -> Result<(), Error> {
        let msg = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        self.out.send(Outgoing::Line(encode(&msg))).map_err(|_| Error::Disconnected)
    }

    fn control(&self, method: &str, params: Value) -> Result<Value, Error> {
        self.request(method, params).wait_timeout(CONTROL_TIMEOUT)
    }

    /// 에이전트 정보(`agentInfo`·`agentCapabilities`·`authMethods`)를 원문째 돌려준다.
    pub fn initialize(&self) -> Result<Value, Error> {
        self.control(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "clientCapabilities": {
                    "fs": { "readTextFile": false, "writeTextFile": false },
                    "terminal": false
                },
                "clientInfo": { "name": "kasaterm", "version": env!("CARGO_PKG_VERSION") }
            }),
        )
    }

    pub fn new_session(&self, cwd: &Path) -> Result<NewSession, Error> {
        let v = self.control("session/new", json!({ "cwd": abs(cwd)?, "mcpServers": [] }))?;
        NewSession::from_value(&v).ok_or_else(|| Error::Invalid(format!("session/new result: {v}")))
    }

    /// 지난 대화를 이벤트로 다시 흘린 뒤 답한다.
    pub fn load_session(&self, session_id: &str, cwd: &Path) -> Result<Value, Error> {
        self.control("session/load", json!({ "sessionId": session_id, "cwd": abs(cwd)?, "mcpServers": [] }))
    }

    /// 지난 대화를 다시 흘리지 않고 이어 붙는다. 사람이 TUI 로 이어 쓰던 세션을 넘겨받을 때 쓴다.
    pub fn resume_session(&self, session_id: &str, cwd: &Path) -> Result<Value, Error> {
        self.control("session/resume", json!({ "sessionId": session_id, "cwd": abs(cwd)?, "mcpServers": [] }))
    }

    /// 턴 하나를 보낸다. 턴은 몇 시간씩 갈 수 있어 기다리기는 부르는 쪽이 정한다.
    pub fn prompt(&self, session_id: &str, text: &str) -> Pending {
        self.request(
            "session/prompt",
            json!({ "sessionId": session_id, "prompt": [{ "type": "text", "text": text }] }),
        )
    }

    pub fn prompt_outcome(result: &Value) -> Result<PromptOutcome, Error> {
        PromptOutcome::from_value(result).ok_or_else(|| Error::Invalid(format!("session/prompt result: {result}")))
    }

    /// 도는 턴을 멈춘다. 턴의 `prompt` 답이 `StopReason::Cancelled` 로 온다.
    pub fn cancel(&self, session_id: &str) -> Result<(), Error> {
        self.notify("session/cancel", json!({ "sessionId": session_id }))
    }

    pub fn set_mode(&self, session_id: &str, mode_id: &str) -> Result<(), Error> {
        self.control("session/set_mode", json!({ "sessionId": session_id, "modeId": mode_id })).map(drop)
    }

    pub fn set_config_option(&self, session_id: &str, config_id: &str, value: &str) -> Result<Value, Error> {
        self.control(
            "session/set_config_option",
            json!({ "sessionId": session_id, "configId": config_id, "value": value }),
        )
    }

    pub fn close_session(&self, session_id: &str) -> Result<(), Error> {
        self.control("session/close", json!({ "sessionId": session_id })).map(drop)
    }

    /// `None` 은 고르지 않고 물리기(cancelled)다. 에이전트는 그 도구 호출을 거절로 본다.
    pub fn answer_permission(&self, id: &RequestId, option_id: Option<&str>) -> Result<(), Error> {
        let outcome = match option_id {
            Some(option) => json!({ "outcome": "selected", "optionId": option }),
            None => json!({ "outcome": "cancelled" }),
        };
        let msg = json!({ "jsonrpc": "2.0", "id": id.0, "result": { "outcome": outcome } });
        self.out.send(Outgoing::Line(encode(&msg))).map_err(|_| Error::Disconnected)
    }

    /// 어댑터가 남긴 stderr 의 끝부분. 연결이 죽었을 때 까닭을 보이는 데 쓴다.
    pub fn stderr_tail(&self) -> String {
        self.stderr_tail.lock().map(|t| t.clone()).unwrap_or_default()
    }

    /// stdin 을 닫아 스스로 끝나게 하고, `grace` 안에 안 끝나면 프로세스 그룹째 죽인다.
    pub fn shutdown(mut self, grace: Duration) -> Option<ExitStatus> {
        let _ = self.out.send(Outgoing::Close);
        let mut child = self.child.take()?;
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            match child.try_wait() {
                Ok(Some(status)) => return Some(status),
                Ok(None) => thread::sleep(Duration::from_millis(50)),
                Err(_) => break,
            }
        }
        kill_tree(&mut child);
        child.wait().ok()
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.out.send(Outgoing::Close);
        let Some(mut child) = self.child.take() else { return };
        if !matches!(child.try_wait(), Ok(Some(_))) {
            kill_tree(&mut child);
            let _ = child.wait();
        }
    }
}

fn abs(cwd: &Path) -> Result<String, Error> {
    if !cwd.is_absolute() {
        return Err(Error::Invalid(format!("cwd must be absolute: {}", cwd.display())));
    }
    Ok(cwd.to_string_lossy().into_owned())
}

fn encode(msg: &Value) -> Vec<u8> {
    let mut line = msg.to_string().into_bytes();
    line.push(b'\n');
    line
}

fn write_loop(mut writer: impl Write, rx: &Receiver<Outgoing>) {
    for msg in rx {
        match msg {
            Outgoing::Line(bytes) => {
                if writer.write_all(&bytes).and_then(|()| writer.flush()).is_err() {
                    return;
                }
            }
            Outgoing::Close => return,
        }
    }
}

fn read_loop(
    reader: impl Read,
    pending: &Mutex<PendingState>,
    reply: &Sender<Outgoing>,
    events: &Sender<Event>,
) {
    let mut reader = BufReader::new(reader);
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        // npx 안내 같은 잡글이 stdout 에 섞여도 연결은 산다.
        let Ok(msg) = serde_json::from_slice::<Value>(&line) else { continue };
        route(&msg, pending, reply, events);
    }
    let waiting = match pending.lock() {
        Ok(mut state) => {
            state.closed = true;
            std::mem::take(&mut state.waiting)
        }
        Err(_) => HashMap::new(),
    };
    for tx in waiting.into_values() {
        let _ = tx.send(Err(Error::Disconnected));
    }
    let _ = events.send(Event::Closed);
}

fn route(msg: &Value, pending: &Mutex<PendingState>, reply: &Sender<Outgoing>, events: &Sender<Event>) {
    let method = msg["method"].as_str();
    let id = msg.get("id").filter(|v| !v.is_null()).cloned();
    match (method, id) {
        (Some(method), Some(id)) => agent_request(method, RequestId(id), &msg["params"], reply, events),
        (Some("session/update"), None) => {
            if let Some(ev) = event::parse_update(&msg["params"]) {
                let _ = events.send(ev);
            }
        }
        (None, Some(id)) => {
            let Some(id) = id.as_u64() else { return };
            let tx = pending.lock().ok().and_then(|mut s| s.waiting.remove(&id));
            let Some(tx) = tx else { return };
            let result = match msg.get("error") {
                Some(err) => Err(Error::Rpc {
                    code: err["code"].as_i64().unwrap_or_default(),
                    message: err["message"].as_str().unwrap_or_default().to_owned(),
                }),
                None => Ok(msg["result"].clone()),
            };
            let _ = tx.send(result);
        }
        _ => {}
    }
}

fn agent_request(method: &str, id: RequestId, params: &Value, reply: &Sender<Outgoing>, events: &Sender<Event>) {
    let error = |code: i64, message: String| {
        let msg = json!({ "jsonrpc": "2.0", "id": id.0, "error": { "code": code, "message": message } });
        let _ = reply.send(Outgoing::Line(encode(&msg)));
    };
    if method != "session/request_permission" {
        error(METHOD_NOT_FOUND, format!("{method} is not offered by this client"));
        return;
    }
    let Some(req) = event::parse_permission(id.clone(), params) else {
        error(INVALID_PARAMS, "unreadable permission request".into());
        return;
    };
    if events.send(Event::Permission(req)).is_err() {
        // 이벤트를 받을 쪽이 없으면 아무도 답하지 않아 에이전트가 영영 멈춘다.
        let msg = json!({ "jsonrpc": "2.0", "id": id.0, "result": { "outcome": { "outcome": "cancelled" } } });
        let _ = reply.send(Outgoing::Line(encode(&msg)));
    }
}

fn stderr_loop(mut stderr: impl Read, tail: &Mutex<String>) {
    let mut buf = [0u8; 4096];
    while let Ok(n) = stderr.read(&mut buf) {
        if n == 0 {
            return;
        }
        let Ok(mut t) = tail.lock() else { return };
        t.push_str(&String::from_utf8_lossy(&buf[..n]));
        if t.len() > STDERR_TAIL_BYTES {
            let mut cut = t.len() - STDERR_TAIL_BYTES;
            while !t.is_char_boundary(cut) {
                cut += 1;
            }
            t.drain(..cut);
        }
    }
}

#[cfg(unix)]
fn kill_tree(child: &mut Child) {
    // process_group(0) 으로 띄워 pgid 가 자식 pid 와 같다.
    if let Ok(pgid) = libc::pid_t::try_from(child.id()) {
        // SAFETY: killpg 는 메모리를 건드리지 않는다. 우리가 만든 그룹에만 보낸다.
        unsafe {
            libc::killpg(pgid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
}

#[cfg(not(unix))]
fn kill_tree(child: &mut Child) {
    let _ = child.kill();
}

#[cfg(test)]
mod tests {
    use std::io::{PipeReader, PipeWriter, pipe};

    use super::*;
    use crate::event::{PermissionKind, StopReason, ToolOutput};

    const WAIT: Duration = Duration::from_secs(5);

    struct FakeAgent {
        from_client: BufReader<PipeReader>,
        to_client: Option<PipeWriter>,
    }

    impl FakeAgent {
        fn read(&mut self) -> Value {
            let mut line = String::new();
            self.from_client.read_line(&mut line).expect("read from client");
            serde_json::from_str(&line).expect("client sent json")
        }

        fn send_raw(&mut self, line: &str) {
            let w = self.to_client.as_mut().expect("open");
            writeln!(w, "{line}").expect("write to client");
        }

        fn send(&mut self, msg: &Value) {
            self.send_raw(&msg.to_string());
        }

        fn update(&mut self, update: &Value) {
            self.send(&json!({ "jsonrpc": "2.0", "method": "session/update",
                               "params": { "sessionId": "s1", "update": update } }));
        }

        fn hang_up(&mut self) {
            self.to_client = None;
        }
    }

    fn connect() -> (Connection, Receiver<Event>, FakeAgent) {
        let (client_reads, agent_writes) = pipe().expect("pipe");
        let (agent_reads, client_writes) = pipe().expect("pipe");
        let (conn, events) = Connection::from_streams(client_reads, client_writes).expect("start");
        let agent = FakeAgent { from_client: BufReader::new(agent_reads), to_client: Some(agent_writes) };
        (conn, events, agent)
    }

    fn next(events: &Receiver<Event>) -> Event {
        events.recv_timeout(WAIT).expect("event")
    }

    #[test]
    fn prompt_turn_streams_events_then_outcome() {
        let (conn, events, mut agent) = connect();
        let pending = conn.prompt("s1", "make hello.txt");

        let req = agent.read();
        assert_eq!(req["method"], "session/prompt");
        assert_eq!(req["params"]["prompt"][0]["text"], "make hello.txt");
        agent.send_raw("npm warn exec The following package was not found and will be installed");
        agent.update(&json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "On it." } }));
        agent.update(&json!({ "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "ls", "kind": "execute",
                              "status": "in_progress", "rawInput": { "command": "ls" } }));
        agent.update(&json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed",
                              "rawOutput": { "exit_code": 0 },
                              "_meta": { "terminal_output_delta": { "data": "hello.txt\n" } } }));
        agent.update(&json!({ "sessionUpdate": "usage_update", "used": 35491, "size": 828400 }));
        agent.send(&json!({ "jsonrpc": "2.0", "id": req["id"],
                            "result": { "stopReason": "end_turn", "usage": { "outputTokens": 135 } } }));

        let result = pending.wait_timeout(WAIT).expect("answered");
        let outcome = Connection::prompt_outcome(&result).expect("outcome");
        assert_eq!(outcome.stop, StopReason::EndTurn);
        assert_eq!(outcome.output_tokens, Some(135));

        assert!(matches!(next(&events), Event::Message { ref text, .. } if text == "On it."));
        assert!(matches!(next(&events), Event::ToolCall(ref c) if c.command.as_deref() == Some("ls")));
        assert!(matches!(next(&events), Event::ToolCallUpdate(ref u)
            if u.exit_code == Some(0) && u.output == Some(ToolOutput::Delta("hello.txt\n".into()))));
        assert!(matches!(next(&events), Event::Usage { used: 35491, size: 828400, .. }));
    }

    #[test]
    fn permission_round_trip_keeps_agent_request_id() {
        let (conn, events, mut agent) = connect();
        agent.send(&json!({ "jsonrpc": "2.0", "id": "perm-7", "method": "session/request_permission",
            "params": { "sessionId": "s1",
                        "toolCall": { "toolCallId": "t9", "title": "rm -rf build" },
                        "options": [{ "optionId": "ok", "name": "Allow", "kind": "allow_once" },
                                    { "optionId": "no", "name": "Reject", "kind": "reject_once" }] } }));
        let Event::Permission(req) = next(&events) else { panic!("not a permission request") };
        assert_eq!(req.title.as_deref(), Some("rm -rf build"));
        let allow = req.option(PermissionKind::AllowOnce).expect("allow option");
        conn.answer_permission(&req.id, Some(&allow.id)).expect("sent");

        let answer = agent.read();
        assert_eq!(answer["id"], "perm-7");
        assert_eq!(answer["result"]["outcome"], json!({ "outcome": "selected", "optionId": "ok" }));
    }

    #[test]
    fn unoffered_agent_requests_get_method_not_found() {
        let (_conn, _events, mut agent) = connect();
        agent.send(&json!({ "jsonrpc": "2.0", "id": 3, "method": "fs/read_text_file",
                            "params": { "sessionId": "s1", "path": "/etc/hosts" } }));
        let answer = agent.read();
        assert_eq!(answer["id"], 3);
        assert_eq!(answer["error"]["code"], METHOD_NOT_FOUND);
    }

    #[test]
    fn permission_is_cancelled_when_nobody_listens() {
        let (_conn, events, mut agent) = connect();
        drop(events);
        agent.send(&json!({ "jsonrpc": "2.0", "id": 4, "method": "session/request_permission",
            "params": { "sessionId": "s1", "toolCall": {}, "options": [] } }));
        assert_eq!(agent.read()["result"]["outcome"], json!({ "outcome": "cancelled" }));
    }

    #[test]
    fn agent_error_reply_is_reported() {
        let (conn, _events, mut agent) = connect();
        let pending = conn.request("session/set_mode", json!({ "sessionId": "s1", "modeId": "nope" }));
        let req = agent.read();
        agent.send(&json!({ "jsonrpc": "2.0", "id": req["id"], "error": { "code": -32602, "message": "no such mode" } }));
        assert!(matches!(
            pending.wait_timeout(WAIT),
            Err(Error::Rpc { code: -32602, ref message }) if message == "no such mode"
        ));
    }

    #[test]
    fn hang_up_fails_waiting_and_later_requests() {
        let (conn, events, mut agent) = connect();
        let pending = conn.prompt("s1", "long task");
        agent.read();
        agent.hang_up();
        assert!(matches!(pending.wait_timeout(WAIT), Err(Error::Disconnected)));
        assert_eq!(next(&events), Event::Closed);
        assert!(matches!(conn.prompt("s1", "again").wait_timeout(WAIT), Err(Error::Disconnected)));
    }

    #[test]
    fn relative_cwd_is_refused_before_sending() {
        let (conn, _events, _agent) = connect();
        assert!(matches!(conn.new_session(Path::new("relative/dir")), Err(Error::Invalid(_))));
    }

    #[cfg(unix)]
    #[test]
    fn shutdown_kills_the_whole_process_group() {
        let dir = std::env::temp_dir().join(format!("kasa-acp-shutdown-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let pid_file = dir.join("grandchild.pid");
        // 손자(sleep)를 남기고 stdin EOF 에도 안 끝나는 어댑터 흉내.
        let script = format!(
            "sleep 60 & echo $! > '{}'; read -r line; \
             echo '{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{\"protocolVersion\":1}}}}'; \
             trap '' HUP; while :; do sleep 1; done",
            pid_file.display()
        );
        let mut spec = LaunchSpec::new("/bin/sh");
        spec.args = vec!["-c".into(), script.into()];
        let (conn, _events) = Connection::spawn(&spec, &dir).expect("spawn");
        assert_eq!(conn.initialize().expect("initialized")["protocolVersion"], 1);
        let grandchild: libc::pid_t = std::fs::read_to_string(&pid_file)
            .expect("pid file")
            .trim()
            .parse()
            .expect("pid");

        let started = Instant::now();
        assert!(conn.shutdown(Duration::from_millis(300)).is_some());
        assert!(started.elapsed() < Duration::from_secs(5));
        let deadline = Instant::now() + WAIT;
        // SAFETY: 신호 0 은 존재 확인만 한다.
        while unsafe { libc::kill(grandchild, 0) } == 0 {
            assert!(Instant::now() < deadline, "grandchild {grandchild} survived");
            thread::sleep(Duration::from_millis(20));
        }
        std::fs::remove_dir_all(dir).ok();
    }
}
