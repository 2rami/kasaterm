use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{atomic::{AtomicBool, AtomicUsize, Ordering}, mpsc, Arc};
use std::time::{Duration, Instant};

enum Update { Progress, Done(Result<Vec<(String, String)>, ()>) }

#[derive(Default)]
pub struct Chat {
    history: Vec<(String, String)>,
    pending: Option<mpsc::Receiver<Update>>,
    cancel: Option<Arc<AtomicBool>>,
    pub failed: bool,
    question: Option<String>,
    snapshot: Option<PathBuf>,
    active: Arc<AtomicUsize>,
}

impl Chat {
    pub fn busy(&self) -> bool { self.pending.is_some() }
    pub fn network_active(&self) -> bool { self.active.load(Ordering::Acquire) > 0 }
    pub fn transcript(&self) -> String {
        if self.history.is_empty() && self.question.is_none() {
            return "나쵸에게 물어보세요.\n\n“재시작하면 뭐 확인해야 돼?”\n“내가 시킨 일 중 아직 남은 게 뭐야?”".into();
        }
        let mut text = self.history.iter().map(|(role, text)| format!("{}\n{text}", if role == "user" { "나" } else { "나쵸" })).collect::<Vec<_>>().join("\n\n");
        if let Some(question) = &self.question { text.push_str(&format!("\n\n나\n{question}")); }
        text
    }
    pub fn load(&mut self, service: PathBuf, snapshot: PathBuf) {
        if self.busy() { return; }
        if self.history.is_empty() {
            if let Ok(file) = std::fs::File::open(&snapshot) {
                let mut bytes = Vec::new();
                if file.take(262145).read_to_end(&mut bytes).is_ok() && bytes.len() <= 262144 {
                    if let Ok(value) = serde_json::from_slice::<Value>(&bytes) { self.history = messages(&value); }
                }
            }
        }
        self.snapshot = Some(snapshot);
        self.start(service, None);
    }
    pub fn send(&mut self, service: PathBuf, text: String) {
        if self.busy() || text.trim().is_empty() { return; }
        let text: String = text.trim().chars().take(4000).collect();
        self.question = Some(text.clone());
        self.start(service, Some(text));
    }
    pub fn retry(&mut self, service: PathBuf) {
        if !self.busy() { self.start(service, self.question.clone()); }
    }
    fn start(&mut self, service: PathBuf, question: Option<String>) {
        self.failed = false;
        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(cancel.clone());
        self.pending = Some(receiver);
        let active = self.active.clone();
        active.fetch_add(1, Ordering::AcqRel);
        std::thread::spawn(move || {
            let result = run(&service, question, &cancel, &sender);
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
            Ok(Update::Progress) => false,
            Ok(Update::Done(result)) => {
                self.pending = None;
                self.cancel = None;
                match result {
                    Ok(history) => { self.history = history; self.question = None; self.failed = false; self.save(); }
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
        let value = json!({"messages": self.history.iter().rev().take(40).collect::<Vec<_>>().into_iter().rev().map(|(role,text)| json!({"role":role,"text":text})).collect::<Vec<_>>()});
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

fn messages(value: &Value) -> Vec<(String, String)> {
    value["messages"].as_array().into_iter().flatten().filter_map(|row| {
        let role = row["role"].as_str()?;
        if !matches!(role, "user" | "assistant") { return None; }
        let text = row["text"].as_str().or_else(|| row["content"].as_str())?;
        Some((role.to_string(), text.chars().take(12000).collect()))
    }).rev().take(40).collect::<Vec<_>>().into_iter().rev().collect()
}

fn run(service: &Path, question: Option<String>, cancel: &AtomicBool, progress: &mpsc::Sender<Update>) -> Result<Vec<(String, String)>, ()> {
    let port = crate::journal::service(service)?;
    if cancel.load(Ordering::Relaxed) { return Err(()); }
    if let Some(text) = question {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|_| ())?.as_nanos();
        let response = crate::journal::request(port, "POST", "/api/chat", Some(&json!({"text":text,"conversation_id":"pet","client_request_id":format!("pet-{}-{nonce}",std::process::id())})))?;
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
                Some("queued" | "running") => { let _ = progress.send(Update::Progress); }
                _ => return Err(()),
            }
            for _ in 0..10 {
                if cancel.load(Ordering::Relaxed) { break; }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
    crate::journal::get(port, "/api/chat/history?conversation_id=pet").map(|value| messages(&value))
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
        chat.history = vec![("user".into(), "질문".into())];
        chat.pending = Some(receiver);
        chat.cancel = Some(flag.clone());
        chat.close();
        assert!(flag.load(Ordering::Relaxed));
        assert!(sender.send(Update::Done(Ok(Vec::new()))).is_err());
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
                ("POST /api/chat", json!({"job_id":"job-123","status":"queued"})),
                ("GET /api/chat/jobs/job-123", json!({"status":"completed"})),
                ("GET /api/chat/history?conversation_id=pet", json!({"messages":[{"role":"user","text":"뭘 확인해?"},{"role":"assistant","text":"메뉴를 열어 주세요."}]})),
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
        let result=run(&path,Some("뭘 확인해?".into()),&AtomicBool::new(false),&sender).unwrap();
        assert_eq!(result.len(),2);
        server.join().unwrap();
        std::fs::remove_file(path).unwrap();
    }
}
