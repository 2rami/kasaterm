use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{atomic::{AtomicBool, AtomicUsize, Ordering}, mpsc, Arc};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
struct Message { id: i64, role: String, text: String }
#[derive(Clone)]
enum Task { Latest, Forward(i64), Older(i64), Send(String) }
struct History { rows: Vec<Message>, before: Option<i64>, replace: bool, update_before: bool, older: bool }
enum Update { Progress(String), Done(Result<History, ()>) }

#[derive(Default)]
pub struct Chat {
    history: Vec<Message>,
    pending: Option<mpsc::Receiver<Update>>,
    cancel: Option<Arc<AtomicBool>>,
    pub failed: bool,
    pub progress: String,
    question: Option<String>,
    snapshot: Option<PathBuf>,
    active: Arc<AtomicUsize>,
    next_before: Option<i64>,
    authoritative: bool,
    retry_task: Option<Task>,
    pub showing_older: bool,
}

impl Chat {
    pub fn busy(&self) -> bool { self.pending.is_some() }
    pub fn network_active(&self) -> bool { self.active.load(Ordering::Acquire) > 0 }
    pub fn has_older(&self) -> bool { self.next_before.is_some() }
    pub fn transcript(&self) -> String {
        if self.history.is_empty() && self.question.is_none() {
            return "나쵸에게 물어보세요.\n\n“재시작하면 뭐 확인해야 돼?”\n“내가 시킨 일 중 아직 남은 게 뭐야?”".into();
        }
        let mut text = self.history.iter().map(|row| format!("{}\n{}", if row.role == "user" { "나" } else { "나쵸" }, row.text)).collect::<Vec<_>>().join("\n\n");
        if let Some(question) = &self.question { text.push_str(&format!("\n\n나\n{question}")); }
        text
    }
    pub fn load(&mut self, service: PathBuf, snapshot: PathBuf) {
        if self.busy() { return; }
        if self.history.is_empty() {
            if let Ok(file) = std::fs::File::open(&snapshot) {
                let mut bytes = Vec::new();
                if file.take(2097153).read_to_end(&mut bytes).is_ok() && bytes.len() <= 2097152 {
                    if let Ok(value) = serde_json::from_slice::<Value>(&bytes) { self.history = messages(&value); }
                }
            }
        }
        self.snapshot = Some(snapshot);
        let task = if self.authoritative { self.history.last().map(|row| Task::Forward(row.id)).unwrap_or(Task::Latest) } else { Task::Latest };
        self.start(service, task);
    }
    pub fn send(&mut self, service: PathBuf, text: String) {
        if self.busy() || text.trim().is_empty() { return; }
        let text: String = text.trim().chars().take(4000).collect();
        self.question = Some(text.clone());
        self.start(service, Task::Send(text));
    }
    pub fn retry(&mut self, service: PathBuf) {
        if !self.busy() { self.start(service, self.retry_task.clone().unwrap_or(Task::Latest)); }
    }
    pub fn older(&mut self, service: PathBuf) {
        if !self.busy() { if let Some(before) = self.next_before { self.start(service, Task::Older(before)); } }
    }
    fn start(&mut self, service: PathBuf, task: Task) {
        self.failed = false;
        self.showing_older = false;
        self.retry_task = Some(task.clone());
        self.progress = "나쵸가 확인하고 있어요…".into();
        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(cancel.clone());
        self.pending = Some(receiver);
        let active = self.active.clone();
        active.fetch_add(1, Ordering::AcqRel);
        std::thread::spawn(move || {
            let result = run(&service, task, &cancel, &sender);
            if !cancel.load(Ordering::Relaxed) { let _ = sender.send(Update::Done(result)); }
            active.fetch_sub(1, Ordering::AcqRel);
        });
    }
    pub fn close(&mut self) {
        if let Some(cancel) = self.cancel.take() { cancel.store(true, Ordering::Relaxed); }
        self.pending = None;
        self.question = None;
        self.failed = false;
    }
    pub fn poll(&mut self) -> bool {
        let Some(receiver) = &self.pending else { return false };
        match receiver.try_recv() {
            Ok(Update::Progress(text)) => { self.progress = text; true }
            Ok(Update::Done(result)) => {
                self.pending = None;
                self.cancel = None;
                match result {
                    Ok(history) => {
                        if history.replace || !self.authoritative { self.history.clear(); merge(&mut self.history, history.rows); }
                        else { merge(&mut self.history, history.rows); }
                        if history.update_before { self.next_before = history.before; }
                        self.authoritative = true;
                        self.showing_older = history.older;
                        self.question = None; self.failed = false; self.retry_task = None; self.save();
                    }
                    Err(()) => self.failed = true,
                }
                true
            }
            Err(mpsc::TryRecvError::Disconnected) => { self.pending = None; self.failed = true; true }
            Err(mpsc::TryRecvError::Empty) => false,
        }
    }
    fn save(&self) {
        let Some(path) = &self.snapshot else { return };
        let value = json!({"messages": self.history.iter().rev().take(40).collect::<Vec<_>>().into_iter().rev().map(|row| json!({"id":row.id,"role":row.role,"text":row.text})).collect::<Vec<_>>()});
        let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
        if let Ok(mut file) = options.open(&temporary) {
            if file.write_all(value.to_string().as_bytes()).is_ok() { let _ = std::fs::rename(&temporary, path); }
        }
    }
}

impl Drop for Chat { fn drop(&mut self) { self.close(); } }

fn cursor(value: &Value) -> Option<i64> { value.as_i64().or_else(|| value.as_str().and_then(|v| v.parse().ok())).filter(|id| *id >= 0) }

fn messages(value: &Value) -> Vec<Message> {
    value["messages"].as_array().into_iter().flatten().enumerate().filter_map(|(index,row)| {
        let role = row["role"].as_str()?;
        if !matches!(role, "user" | "assistant") { return None; }
        let text = row["text"].as_str().or_else(|| row["content"].as_str())?;
        Some(Message { id: cursor(&row["id"]).unwrap_or(-((index + 1) as i64)), role: role.to_string(), text: text.to_string() })
    }).collect()
}

fn merge(target: &mut Vec<Message>, rows: Vec<Message>) {
    let mut by_id: std::collections::BTreeMap<i64, Message> = target.drain(..).map(|row| (row.id,row)).collect();
    for row in rows { by_id.insert(row.id,row); }
    *target = by_id.into_values().collect();
}

fn forward<F>(mut after: i64, cancel: &AtomicBool, mut fetch: F) -> Result<Vec<Message>, ()>
where F: FnMut(i64) -> Result<Value, ()> {
    let mut rows = Vec::new();
    loop {
        if cancel.load(Ordering::Relaxed) { return Err(()); }
        let page = fetch(after)?;
        if cancel.load(Ordering::Relaxed) { return Err(()); }
        if !page["messages"].is_array() { return Err(()); }
        merge(&mut rows, messages(&page));
        match cursor(&page["next_after"]) {
            Some(next) if next > after => after = next,
            Some(_) => return Err(()),
            None => return Ok(rows),
        }
    }
}

fn run(service: &Path, task: Task, cancel: &AtomicBool, progress: &mpsc::Sender<Update>) -> Result<History, ()> {
    let port = crate::journal::service(service)?;
    if cancel.load(Ordering::Relaxed) { return Err(()); }
    let mut after = if let Task::Forward(id) = task { Some(id) } else { None };
    if let Task::Send(text) = &task {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|_| ())?.as_nanos();
        let response = crate::journal::request(port, "POST", "/api/chat", Some(&json!({"text":text,"conversation_id":"pet","client_request_id":format!("pet-{}-{nonce}",std::process::id())})))?;
        after = Some(cursor(&response["user_message_id"]).filter(|id| *id > 0).ok_or(())? - 1);
        let id = response["job_id"].as_str().filter(|id| !id.is_empty() && id.len() < 160 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')).ok_or(())?;
        let route = format!("/api/chat/jobs/{id}");
        let started = Instant::now();
        loop {
            if cancel.load(Ordering::Relaxed) || started.elapsed() > Duration::from_secs(300) {
                let _ = crate::journal::request(port, "DELETE", &route, Some(&json!({})));
                return Err(());
            }
            let job = match crate::journal::get(port, &route) {
                Ok(job) => job,
                Err(()) => { let _ = crate::journal::request(port, "DELETE", &route, Some(&json!({}))); return Err(()); }
            };
            match job["status"].as_str() {
                Some("completed") => break,
                Some("failed" | "cancelled") => return Err(()),
                Some("queued" | "running") => {
                    let done = job["progress"]["completed_batches"].as_u64().unwrap_or(0);
                    let total = job["progress"]["total_batches"].as_u64().unwrap_or(0);
                    let text = if total > 0 { format!("나쵸 확인 중 · {done}/{total}") } else { "나쵸가 확인하고 있어요…".into() };
                    let _ = progress.send(Update::Progress(text));
                }
                _ => return Err(()),
            }
            for _ in 0..10 {
                if cancel.load(Ordering::Relaxed) { break; }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
    if let Some(after) = after {
        let rows = forward(after,cancel,|cursor| crate::journal::get(port,&format!("/api/chat/history?conversation_id=pet&after={cursor}")))?;
        return Ok(History { rows, before:None, replace:false, update_before:false, older:false });
    }
    let route = match task { Task::Older(before) => format!("/api/chat/history?conversation_id=pet&before={before}"), _ => "/api/chat/history?conversation_id=pet".into() };
    let value = crate::journal::get(port, &route)?;
    if cancel.load(Ordering::Relaxed) { return Err(()); }
    if !value["messages"].is_array() { return Err(()); }
    Ok(History { rows:messages(&value), before:cursor(&value["next_before"]), replace:matches!(task,Task::Latest), update_before:true, older:matches!(task,Task::Older(_)) })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn history_replaces_snapshot_and_ignores_non_messages() {
        let rows = messages(&json!({"messages":[{"role":"user","text":"뭘 확인해?"},{"role":"tool","text":"private"},{"role":"assistant","content":"펫 메뉴를 확인해 주세요."}]}));
        assert_eq!(rows.len(), 2);
        let mut chat = Chat::default();
        chat.history = rows;
        assert!(!chat.transcript().contains("private"));
    }
    #[test]
    fn close_drops_late_response_and_preserves_history() {
        let (sender, receiver) = mpsc::channel();
        let flag = Arc::new(AtomicBool::new(false));
        let mut chat = Chat::default();
        chat.history = vec![Message{id:1,role:"user".into(),text:"질문".into()}];
        chat.pending = Some(receiver);
        chat.cancel = Some(flag.clone());
        chat.close();
        assert!(flag.load(Ordering::Relaxed));
        assert!(sender.send(Update::Done(Err(()))).is_err());
        assert_eq!(chat.history.len(), 1);
        assert!(!chat.busy());
    }
    #[test]
    fn chat_job_uses_json_header_and_fetches_canonical_history() {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            for (expected, body) in [
                ("GET /health", json!({"ok":true,"version":1,"service":"request-journal"})),
                ("POST /api/chat", json!({"job_id":"job-123","status":"queued","user_message_id":1})),
                ("GET /api/chat/jobs/job-123", json!({"status":"completed"})),
                ("GET /api/chat/history?conversation_id=pet&after=0", json!({"messages":[{"id":1,"role":"user","text":"뭘 확인해?"},{"id":2,"role":"assistant","text":"메뉴를 열어 주세요."}]})),
            ] {
                let (mut socket, _) = listener.accept().unwrap();
                let mut bytes = Vec::new();
                while !bytes.ends_with(b"\r\n\r\n") { let mut b=[0]; socket.read_exact(&mut b).unwrap(); bytes.push(b[0]); }
                let head = String::from_utf8(bytes).unwrap();
                assert!(head.starts_with(expected));
                let length: usize = head.lines().find_map(|line| line.strip_prefix("Content-Length: ")).unwrap().parse().unwrap();
                let mut input = vec![0;length]; socket.read_exact(&mut input).unwrap();
                if expected.starts_with("POST") {
                    assert!(head.contains("X-Journal-Request: 1"));
                    assert_eq!(serde_json::from_slice::<Value>(&input).unwrap()["conversation_id"], "pet");
                }
                let body=body.to_string();
                write!(socket,"HTTP/1.0 200 OK\r\nContent-Length: {}\r\n\r\n{}",body.len(),body).unwrap();
            }
        });
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("pet-chat-service-{nonce}.json"));
        std::fs::write(&path,json!({"version":1,"base_url":format!("http://127.0.0.1:{port}")}).to_string()).unwrap();
        let (sender, _) = mpsc::channel();
        let result=run(&path,Task::Send("뭘 확인해?".into()),&AtomicBool::new(false),&sender).unwrap();
        assert_eq!(result.rows.len(),2);
        server.join().unwrap();
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn long_utf8_answer_keeps_all_pages_in_order_without_duplicates() {
        let cancelled = AtomicBool::new(false);
        let text = "긴 목록 항목\n".repeat(1000);
        let rows = forward(0,&cancelled,|after| {
            let start = if after == 0 { 1 } else { after };
            let end = (after + 20).min(65);
            Ok(json!({"messages":(start..=end).rev().map(|id| json!({"id":id,"role":"assistant","text":format!("항목{id}\n{text}")})).collect::<Vec<_>>(),"next_after":if end < 65 {Some(end)} else {None}}))
        }).unwrap();
        assert_eq!(rows.len(),65);
        assert_eq!(rows.first().unwrap().id,1);
        assert_eq!(rows.last().unwrap().id,65);
        assert!(rows.iter().all(|row|row.text.contains(&text)));
        let mut prior=vec![Message{id:1,role:"assistant".into(),text:"기존".into()}];
        merge(&mut prior,rows);
        assert_eq!(prior.len(),65);
        assert!(prior[0].text.starts_with("항목1"));
    }
    #[test]
    fn pagination_cancellation_and_repeated_cursor_are_not_partial_success() {
        let cancelled=AtomicBool::new(false);
        assert!(forward(0,&cancelled,|_| {
            cancelled.store(true,Ordering::Relaxed);
            Ok(json!({"messages":[{"id":1,"role":"assistant","text":"첫 페이지"}],"next_after":1}))
        }).is_err());
        assert!(forward(5,&AtomicBool::new(false),|_|Ok(json!({"messages":[],"next_after":5}))).is_err());
    }
}
