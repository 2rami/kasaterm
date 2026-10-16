//! 창 없는 카사텀. 다른 프로그램(나쵸)이 띄운 셸을 `kasa-pty` 세션으로 붙들고, 유닉스
//! 소켓으로 내준다. 본판 GUI 의 칸이 아니라 이 프로세스가 PTY 를 쥐므로, 여기서 도는
//! 세션은 카사텀 판에 나타나지 않고 부르는 쪽이 재시작해도 끊기지 않는다.
//!
//! 요청 한 줄 = `{"id", "method", "params"}`, 답 한 줄 = `{"id", "ok", "result"|"error"}`
//! (카사텀 소켓과 같은 봉투라 부르는 쪽이 같은 해석기를 쓴다). 메서드는 `Host::handle`.

pub mod keys;
pub mod server;

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use kasa_pty::{PtyOptions, PtySession};
use serde_json::{json, Value};

/// 롱폴 상한. 부르는 쪽의 HTTP 롱폴(25초)보다 길면 그쪽이 먼저 끊는다.
const WAIT_MAX: f64 = 25.0;
/// 붙여넣은 글이 앱(claude 입력칸)에 다 들어간 뒤 Enter 가 가야 한다 — 같은 쓰기에 실으면
/// 붙여넣기 끝 표시 전에 Enter 를 받은 앱이 빈 입력을 보낸다.
const SUBMIT_GAP: Duration = Duration::from_millis(150);

pub struct Session {
    pub id: String,
    pub name: Mutex<String>,
    pub created_ms: u64,
    pub pty: PtySession,
    /// 화면이 바뀔 때마다 하나 오른다. 롱폴은 이 값이 바뀌기를 기다린다.
    rev: Mutex<u64>,
    changed: Condvar,
}

#[derive(Default)]
pub struct Host {
    sessions: Mutex<HashMap<String, Arc<Session>>>,
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn str_param<'a>(p: &'a Value, key: &str) -> Option<&'a str> {
    p.get(key).and_then(Value::as_str)
}

fn hash_of(text: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    format!("{:016x}", h.finish())
}

impl Host {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn get(&self, p: &Value) -> Result<Arc<Session>> {
        let id = str_param(p, "id").context("id 가 없다")?;
        self.sessions.lock().unwrap().get(id).cloned().ok_or_else(|| anyhow!("세션 {id} 이(가) 없다"))
    }

    /// 요청 한 줄을 처리해 답 한 줄을 만든다. 실패도 답이다 — 연결은 끊지 않는다.
    pub fn handle(self: &Arc<Self>, req: &Value) -> Value {
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let method = str_param(req, "method").unwrap_or_default();
        let params = req.get("params").cloned().unwrap_or_else(|| json!({}));
        match self.dispatch(method, &params) {
            Ok(result) => json!({"id": id, "ok": true, "result": result}),
            Err(e) => json!({"id": id, "ok": false, "error": format!("{e:#}")}),
        }
    }

    fn dispatch(self: &Arc<Self>, method: &str, p: &Value) -> Result<Value> {
        match method {
            "ping" => Ok(json!({"version": env!("CARGO_PKG_VERSION"), "pid": std::process::id()})),
            "spawn" => self.spawn(p),
            "list" => Ok(self.list()),
            "write" => {
                let s = self.get(p)?;
                s.pty.send_bytes(str_param(p, "data").context("data 가 없다")?.as_bytes())?;
                Ok(json!({}))
            }
            "paste" => self.paste(p),
            "key" => {
                let s = self.get(p)?;
                let name = str_param(p, "name").context("name 이 없다")?;
                let app_cursor = s.pty.live_screen().app_cursor;
                let bytes = keys::bytes(name, app_cursor).ok_or_else(|| anyhow!("모르는 키 {name}"))?;
                s.pty.send_bytes(&bytes)?;
                Ok(json!({}))
            }
            "screen" => self.screen(p),
            "text" => {
                let s = self.get(p)?;
                let lines = p.get("lines").and_then(Value::as_u64).unwrap_or(40) as usize;
                Ok(json!({"text": s.pty.visible_text(lines)}))
            }
            "resize" => {
                let s = self.get(p)?;
                let cols = p.get("cols").and_then(Value::as_u64).context("cols 가 없다")? as u16;
                let rows = p.get("rows").and_then(Value::as_u64).context("rows 가 없다")? as u16;
                s.pty.resize(cols, rows)?;
                Ok(json!({}))
            }
            "rename" => {
                let s = self.get(p)?;
                *s.name.lock().unwrap() = str_param(p, "name").unwrap_or_default().trim().to_string();
                Ok(json!({}))
            }
            "close" => {
                let s = self.get(p)?;
                self.sessions.lock().unwrap().remove(&s.id);
                s.pty.terminate_local();
                Ok(json!({}))
            }
            "" => bail!("method 가 없다"),
            other => bail!("모르는 method {other}"),
        }
    }

    fn spawn(self: &Arc<Self>, p: &Value) -> Result<Value> {
        let id = format!("s-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
        let mut env: Vec<(String, String)> = p
            .get("env")
            .and_then(Value::as_object)
            .map(|m| m.iter().filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string()))).collect())
            .unwrap_or_default();
        // 칸 안의 훅·도구가 「나는 kasa-host 의 어느 세션인가」를 알 수 있게.
        env.push(("KASA_HOST_SESSION".into(), id.clone()));
        let opts = PtyOptions {
            shell: str_param(p, "shell").map(str::to_string),
            cwd: str_param(p, "cwd").map(str::to_string),
            cols: p.get("cols").and_then(Value::as_u64).unwrap_or(120) as u16,
            rows: p.get("rows").and_then(Value::as_u64).unwrap_or(40) as u16,
            env,
            pane_id: id.clone(),
            initial_scrollback: Vec::new(),
        };
        let pty = PtySession::start(opts).context("세션을 못 띄웠다")?;
        let screens = pty.screens.clone();
        let session = Arc::new(Session {
            id: id.clone(),
            name: Mutex::new(str_param(p, "name").unwrap_or_default().trim().to_string()),
            created_ms: now_ms(),
            pty,
            rev: Mutex::new(0),
            changed: Condvar::new(),
        });
        self.sessions.lock().unwrap().insert(id.clone(), session.clone());

        // 화면 채널을 비워야 엔진이 프레임을 안 버리고, 롱폴이 바뀜을 안다. 셸이 끝나면(EOF)
        // 목록에서 빠진다 — 죽은 세션이 남아 있으면 부르는 쪽이 살아 있는 줄 안다.
        let host = Arc::downgrade(self);
        let watched = session.clone();
        std::thread::Builder::new()
            .name(format!("kasa-host-{id}"))
            .spawn(move || {
                while let Ok(update) = screens.recv() {
                    *watched.rev.lock().unwrap() += 1;
                    watched.changed.notify_all();
                    if update.eof {
                        break;
                    }
                }
                if let Some(host) = host.upgrade() {
                    host.sessions.lock().unwrap().remove(&watched.id);
                }
                *watched.rev.lock().unwrap() += 1;
                watched.changed.notify_all();
            })
            .context("화면 스레드")?;

        // 셸 함수·별칭이 풀리도록 명령은 띄운 셸에 쳐 넣는다(프로세스로 바로 띄우지 않는다).
        if let Some(cmd) = str_param(p, "cmd").filter(|c| !c.trim().is_empty()) {
            session.pty.send_bytes(format!("{cmd}\r").as_bytes())?;
        }
        Ok(json!({"id": id, "name": *session.name.lock().unwrap(), "pid": session.pty.shell_pid()}))
    }

    fn list(&self) -> Value {
        let mut all: Vec<Arc<Session>> = self.sessions.lock().unwrap().values().cloned().collect();
        all.sort_by_key(|s| s.created_ms);
        Value::Array(
            all.iter()
                .map(|s| {
                    let (cols, rows) = s.pty.size();
                    json!({
                        "id": s.id,
                        "name": *s.name.lock().unwrap(),
                        "pid": s.pty.shell_pid(),
                        "cols": cols,
                        "rows": rows,
                        "title": s.pty.osc_title(),
                        "cwd": s.pty.reported_cwd().map(|p| p.display().to_string()),
                        "agent": s.pty.active_agent().map(|a| a.as_str()),
                        "created_ms": s.created_ms,
                    })
                })
                .collect(),
        )
    }

    /// 앱이 붙여넣기 감싸기(2004)를 켰을 때만 감싼다 — 안 켠 앱은 그 바이트를 글자로 받는다.
    fn paste(&self, p: &Value) -> Result<Value> {
        let s = self.get(p)?;
        let text = str_param(p, "text").context("text 가 없다")?;
        let bytes = if s.pty.live_screen().bracketed_paste {
            format!("\x1b[200~{text}\x1b[201~")
        } else {
            text.replace("\r\n", "\r").replace('\n', "\r")
        };
        s.pty.send_bytes(bytes.as_bytes())?;
        if p.get("submit").and_then(Value::as_bool).unwrap_or(false) {
            std::thread::sleep(SUBMIT_GAP);
            s.pty.send_bytes(b"\r")?;
        }
        Ok(json!({}))
    }

    /// 화면 한 장(카사텀 웹 터미널과 같은 grid 프레임)과 그 해시. `since` 가 지금 해시와 같으면
    /// 바뀔 때까지 `wait` 초 기다렸다 답한다 — 끝내 안 바뀌면 같은 해시가 간다.
    fn screen(&self, p: &Value) -> Result<Value> {
        let s = self.get(p)?;
        let since = str_param(p, "since").unwrap_or_default();
        let wait = p.get("wait").and_then(Value::as_f64).unwrap_or(0.0).clamp(0.0, WAIT_MAX);
        let deadline = Instant::now() + Duration::from_secs_f64(wait);
        loop {
            let seen = *s.rev.lock().unwrap();
            let frame = kasa_screen::wire::encode(&s.pty.live_screen());
            let hash = hash_of(&frame.to_string());
            let now = Instant::now();
            if hash != since || now >= deadline || !self.sessions.lock().unwrap().contains_key(&s.id) {
                let alive = self.sessions.lock().unwrap().contains_key(&s.id);
                return Ok(json!({"hash": hash, "frame": frame, "title": s.pty.osc_title(), "alive": alive}));
            }
            let guard = s.rev.lock().unwrap();
            if *guard == seen {
                let _ = s.changed.wait_timeout(guard, deadline - now).unwrap();
            }
        }
    }
}
