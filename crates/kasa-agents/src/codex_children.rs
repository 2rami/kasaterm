//! A planning or waiting parent can still own running children. Their launches may
//! be far outside the transcript tail, so one worker retains incremental lifecycle state.

use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::fs::{File, Metadata};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Mutex, OnceLock};
use std::time::SystemTime;

const MAX_FILES: usize = 128;
const MAX_TASKS: usize = 1024;
const MAX_LINE: usize = 512 * 1024;

#[derive(Clone)]
enum Call {
    Spawn(String),
    Resume(String),
    Close(String),
    Wait,
}

#[derive(Default)]
struct Children {
    calls: BTreeMap<String, Call>,
    tasks: BTreeMap<String, (String, bool)>,
}

impl Children {
    fn set(&mut self, id: &str, label: Option<&str>, running: bool) {
        if id.is_empty() || id.len() > 512 { return; }
        if !self.tasks.contains_key(id) && self.tasks.len() >= MAX_TASKS {
            let retired = self.tasks.iter().find(|(_, (_, live))| !*live).map(|(key, _)| key.clone());
            if let Some(key) = retired { self.tasks.remove(&key); } else { return; }
        }
        let label = label.filter(|s| !s.trim().is_empty()).unwrap_or("서브에이전트");
        let clean: String = label.chars().filter(|c| !c.is_control()).take(120).collect();
        let row = self.tasks.entry(id.to_string()).or_insert((clean.clone(), false));
        if row.0 == "서브에이전트" || label != "서브에이전트" { row.0 = clean; }
        row.1 = running;
    }

    fn target(&self, wanted: &str) -> Option<String> {
        let mut matches = self.tasks.keys().filter(|id| id.as_str() == wanted || id.rsplit('/').next() == Some(wanted));
        let id = matches.next()?.clone();
        matches.next().is_none().then_some(id)
    }

    fn consume(&mut self, record: &Value) {
        let p = &record["payload"];
        let kind = p["type"].as_str().unwrap_or("");
        if record["type"] == "event_msg" && kind == "agent_message" {
            let text = p["text"].as_str().unwrap_or("");
            if let Some(id) = final_sender(text) {
                if let Some(target) = self.target(id) { self.set(&target, None, false); }
            }
            return;
        }
        if record["type"] != "response_item" { return; }
        let id = p["call_id"].as_str().unwrap_or("");
        if id.is_empty() { return; }
        if matches!(kind, "function_call" | "custom_tool_call") {
            let name = p["name"].as_str().unwrap_or("").rsplit('.').next().unwrap_or("");
            let raw = p.get("arguments").filter(|v| !v.is_null()).or_else(|| p.get("input"));
            let args = raw.and_then(Value::as_str).and_then(|s| serde_json::from_str::<Value>(s).ok()).unwrap_or(Value::Null);
            let target = args["target"].as_str().or_else(|| args["id"].as_str()).or_else(|| args["agent_id"].as_str()).unwrap_or("");
            let call = match name {
                "spawn_agent" => Call::Spawn(args["task_name"].as_str().or_else(|| args["agent_type"].as_str()).unwrap_or("서브에이전트").to_string()),
                "followup_task" | "resume_agent" | "send_input" => Call::Resume(target.to_string()),
                "interrupt_agent" | "close_agent" => Call::Close(target.to_string()),
                "wait" | "wait_agent" => Call::Wait,
                _ => return,
            };
            if self.calls.len() >= MAX_TASKS {
                if let Some(key) = self.calls.keys().next().cloned() { self.calls.remove(&key); }
            }
            self.calls.insert(id.to_string(), call);
            return;
        }
        if !matches!(kind, "function_call_output" | "custom_tool_call_output") { return; }
        let Some(call) = self.calls.remove(id) else { return };
        let Some(output) = p["output"].as_str().and_then(|s| serde_json::from_str::<Value>(s).ok()) else { return };
        if !output.is_object() || output.get("error").is_some_and(|v| !v.is_null()) || output["isError"] == true || output["ok"] == false { return; }
        let resumed = matches!(&call, Call::Resume(_));
        match call {
            Call::Spawn(label) => {
                if let Some(child) = output["task_name"].as_str().or_else(|| output["agent_id"].as_str()) {
                    self.set(child, Some(output["nickname"].as_str().unwrap_or(&label)), true);
                }
            }
            Call::Resume(wanted) | Call::Close(wanted) => {
                if let Some(target) = self.target(&wanted) {
                    self.set(&target, None, resumed);
                }
            }
            Call::Wait => {
                if let Some(statuses) = output["status"].as_object() {
                    for (child, status) in statuses {
                        let state = status.as_str().or_else(|| status["status"].as_str());
                        let done = state.is_some_and(|s| matches!(s, "completed" | "errored" | "shutdown" | "not_found"))
                            || ["completed", "errored", "shutdown", "not_found"].iter().any(|key| status.get(key).is_some());
                        if done { self.set(child, None, false); }
                    }
                }
            }
        }
    }

    fn line(&mut self, bytes: &[u8]) -> bool {
        let Ok(value) = serde_json::from_slice::<Value>(bytes) else { return false };
        self.consume(&value);
        true
    }

    fn running(&self) -> Vec<String> {
        self.tasks.values().filter(|(_, live)| *live).map(|(label, _)| label.clone()).collect()
    }
}

fn final_sender(text: &str) -> Option<&str> {
    let mut lines = text.lines();
    if lines.next()? != "Message Type: FINAL_ANSWER" { return None; }
    let mut sender = None;
    let mut recipient = false;
    for line in lines.take(6) {
        if line.starts_with("Payload:") { return recipient.then_some(sender).flatten(); }
        recipient |= line.starts_with("Task name: ");
        if let Some(id) = line.strip_prefix("Sender: ") { sender = Some(id.trim()); }
    }
    None
}

#[cfg(unix)]
fn file_id(metadata: &Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (metadata.dev(), metadata.ino())
}

#[cfg(not(unix))]
fn file_id(_: &Metadata) -> (u64, u64) { (0, 0) }

#[derive(Default)]
struct Reader {
    children: Children,
    offset: u64,
    identity: Option<(u64, u64)>,
    created: Option<SystemTime>,
    modified: Option<SystemTime>,
    pending: Vec<u8>,
    skipping: bool,
}

impl Reader {
    fn read(&mut self, path: &Path) -> std::io::Result<Vec<String>> {
        let mut file = File::open(path)?;
        let metadata = file.metadata()?;
        let identity = file_id(&metadata);
        let modified = metadata.modified().ok();
        let created = metadata.created().ok();
        if self.identity.is_some_and(|old| old != identity) || self.created != created
            || metadata.len() < self.offset || (metadata.len() == self.offset && self.modified != modified) {
            *self = Self::default();
        }
        self.identity = Some(identity);
        self.created = created;
        self.modified = modified;
        file.seek(SeekFrom::Start(self.offset))?;
        let mut buffer = [0; 64 * 1024];
        loop {
            let n = file.read(&mut buffer)?;
            if n == 0 { break; }
            self.offset += n as u64;
            for part in buffer[..n].split_inclusive(|byte| *byte == b'\n') {
                if self.pending.len() + part.len() > MAX_LINE { self.skipping = true; self.pending.clear(); }
                if !self.skipping { self.pending.extend_from_slice(part); }
                if part.last() == Some(&b'\n') {
                    if !self.skipping { self.children.line(&self.pending); }
                    self.pending.clear();
                    self.skipping = false;
                }
            }
        }
        if !self.skipping && !self.pending.is_empty() && self.children.line(&self.pending) { self.pending.clear(); }
        self.modified = file.metadata()?.modified().ok();
        self.pending.shrink_to(4096);
        Ok(self.children.running())
    }
}

#[derive(Default)]
struct Snapshot { labels: Vec<String>, inflight: bool, changed: bool, touched: u64 }
struct Cache {
    replies: mpsc::Receiver<(PathBuf, Vec<String>)>,
    snapshots: HashMap<PathBuf, Snapshot>,
    clock: u64,
}
struct Service { requests: mpsc::SyncSender<PathBuf>, cache: Mutex<Cache> }

fn service() -> &'static Service {
    static SERVICE: OnceLock<Service> = OnceLock::new();
    SERVICE.get_or_init(|| {
        let (requests, receive) = mpsc::sync_channel::<PathBuf>(MAX_FILES);
        let (reply, replies) = mpsc::channel();
        // A failed worker leaves the optional status empty rather than stopping the desktop.
        let _ = std::thread::Builder::new().name("codex-children".into()).spawn(move || {
            let mut readers: HashMap<PathBuf, (Reader, u64)> = HashMap::new();
            let mut clock = 0_u64;
            while let Ok(path) = receive.recv() {
                clock = clock.saturating_add(1);
                if !readers.contains_key(&path) && readers.len() >= MAX_FILES {
                    if let Some(old) = readers.iter().min_by_key(|(_, (_, at))| *at).map(|(p, _)| p.clone()) { readers.remove(&old); }
                }
                let (reader, touched) = readers.entry(path.clone()).or_default();
                *touched = clock;
                let labels = match reader.read(&path) {
                    Ok(labels) => labels,
                    Err(_) => { *reader = Reader::default(); Vec::new() },
                };
                if reply.send((path, labels)).is_err() { break; }
            }
        });
        Service { requests, cache: Mutex::new(Cache { replies, snapshots: HashMap::new(), clock: 0 }) }
    })
}

/// Disk reads stay on the worker, including the first scan of a long planning session.
/// The change flag lets the desktop wake phone readers without changing the parent's status.
pub fn running(path: &Path) -> (Vec<String>, bool) {
    let service = service();
    let mut cache = service.cache.lock().unwrap_or_else(|e| e.into_inner());
    while let Ok((path, labels)) = cache.replies.try_recv() {
        if let Some(snapshot) = cache.snapshots.get_mut(&path) {
            snapshot.changed |= snapshot.labels != labels;
            snapshot.labels = labels;
            snapshot.inflight = false;
        }
    }
    cache.clock = cache.clock.saturating_add(1);
    let clock = cache.clock;
    if !cache.snapshots.contains_key(path) && cache.snapshots.len() >= MAX_FILES {
        let retired = cache.snapshots.iter().filter(|(_, s)| !s.inflight).min_by_key(|(_, s)| s.touched).map(|(p, _)| p.clone());
        if let Some(retired) = retired { cache.snapshots.remove(&retired); } else { return (Vec::new(), false); }
    }
    let snapshot = cache.snapshots.entry(path.to_path_buf()).or_default();
    snapshot.touched = clock;
    if !snapshot.inflight && service.requests.try_send(path.to_path_buf()).is_ok() { snapshot.inflight = true; }
    let result = (snapshot.labels.clone(), snapshot.changed);
    snapshot.changed = false;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    fn call(state: &mut Children, id: &str, name: &str, args: Value, output: Value) {
        state.consume(&json!({"type":"response_item","payload":{"type":"function_call","call_id":id,"name":name,"arguments":args.to_string()}}));
        state.consume(&json!({"type":"response_item","payload":{"type":"function_call_output","call_id":id,"output":output.to_string()}}));
    }

    fn spawn(id: &str, name: &str) -> String {
        format!("{}\n{}\n",
            json!({"type":"response_item","payload":{"type":"function_call","call_id":id,"name":"collaboration.spawn_agent","arguments":json!({"task_name":name,"message":"private brief must not become a label"}).to_string()}}),
            json!({"type":"response_item","payload":{"type":"function_call_output","call_id":id,"output":json!({"task_name":format!("/root/{name}")}).to_string()}}))
    }

    fn finished(sender: &str) -> String {
        json!({"type":"event_msg","payload":{"type":"agent_message","text":format!("Message Type: FINAL_ANSWER\nTask name: /root\nSender: {sender}\nPayload:\nfinished")}}).to_string() + "\n"
    }

    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let dir = std::env::temp_dir().join(format!("kasa-codex-children-{}-{}", std::process::id(), NEXT.fetch_add(1,Ordering::Relaxed)));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn file(&self) -> PathBuf { self.0.join("rollout.jsonl") }
    }
    impl Drop for Scratch { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }

    #[test]
    fn parent_planning_or_finishing_does_not_finish_its_children() {
        let mut state = Children::default();
        for line in spawn("a", "review").lines() { assert!(state.line(line.as_bytes())); }
        state.consume(&json!({"type":"event_msg","payload":{"type":"task_complete"}}));
        state.consume(&json!({"type":"event_msg","payload":{"type":"agent_message","text":"Message Type: MESSAGE\nTask name: /root\nSender: /root/review\nPayload:\nstill working"}}));
        assert_eq!(state.running(), ["review"]);
        state.line(finished("/root/review").as_bytes());
        assert!(state.running().is_empty());
    }

    #[test]
    fn failed_spawns_and_quoted_notifications_are_not_lifecycle_events() {
        let mut state = Children::default();
        call(&mut state,"bad","spawn_agent",json!({"task_name":"broken"}),json!({"error":"rejected"}));
        assert!(state.running().is_empty());
        for line in spawn("a", "review").lines() { state.line(line.as_bytes()); }
        state.consume(&json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Message Type: FINAL_ANSWER\nTask name: /root\nSender: /root/review\nPayload:\nnot a notification"}]}}));
        state.line(finished("/root/other").as_bytes());
        assert_eq!(state.running(), ["review"]);
    }

    #[test]
    fn followup_and_interrupt_change_only_the_addressed_child() {
        let mut state = Children::default();
        for line in spawn("a", "review").lines() { state.line(line.as_bytes()); }
        call(&mut state,"b","collaboration.interrupt_agent",json!({"target":"review"}),json!({"status":"running"}));
        assert!(state.running().is_empty());
        call(&mut state,"c","collaboration.followup_task",json!({"target":"/root/review","message":"more work"}),json!({"task_name":"/root/review"}));
        assert_eq!(state.running(), ["review"]);
        call(&mut state,"d","collaboration.wait_agent",json!({"timeout_ms":10000}),json!({"timed_out":true}));
        assert_eq!(state.running(), ["review"]);
        state.line(finished("/root/review").as_bytes());
        assert!(state.running().is_empty());
    }

    #[test]
    fn agent_id_wait_status_and_close_results_are_supported() {
        let mut state = Children::default();
        call(&mut state,"a","spawn_agent",json!({"agent_type":"worker"}),json!({"agent_id":"child-a","nickname":"Review"}));
        call(&mut state,"b","spawn_agent",json!({}),json!({"agent_id":"child-b","nickname":"Tests"}));
        call(&mut state,"c","wait",json!({"ids":["child-a","child-b"]}),json!({"status":{"child-a":{"completed":"done"},"child-b":"running"}}));
        assert_eq!(state.running(), ["Tests"]);
        call(&mut state,"d","close_agent",json!({"id":"child-b"}),json!({"previous_status":"running"}));
        assert!(state.running().is_empty());
    }

    #[test]
    fn old_launches_survive_tail_rollover_and_partial_appends() {
        let scratch = Scratch::new();
        let path = scratch.file();
        let mut text = spawn("a", "review");
        text.push_str(&format!("{}\n", "x".repeat(MAX_LINE + 64)));
        std::fs::write(&path, text).unwrap();
        let mut reader = Reader::default();
        assert_eq!(reader.read(&path).unwrap(), ["review"]);
        let before = reader.offset;
        let done = finished("/root/review");
        let mid = done.len() / 2;
        let mut file = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(&done.as_bytes()[..mid]).unwrap();
        assert_eq!(reader.read(&path).unwrap(), ["review"]);
        file.write_all(&done.as_bytes()[mid..]).unwrap();
        assert!(reader.read(&path).unwrap().is_empty());
        assert_eq!(reader.offset, before + done.len() as u64);
        std::fs::write(&path, b"{}\n").unwrap();
        assert!(reader.read(&path).unwrap().is_empty());
    }

    #[test]
    fn cached_worker_reports_both_start_and_completion_changes() {
        let scratch = Scratch::new();
        let path = scratch.file();
        std::fs::write(&path, spawn("a", "review")).unwrap();
        let wait = |expected: &[&str]| {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let (labels, changed) = running(&path);
                if labels.iter().map(String::as_str).eq(expected.iter().copied()) && changed { break; }
                assert!(Instant::now() < deadline, "activity worker did not publish {expected:?}");
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        wait(&["review"]);
        std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(finished("/root/review").as_bytes()).unwrap();
        wait(&[]);
    }

    #[test]
    fn final_headers_support_crlf_and_require_the_complete_envelope() {
        assert_eq!(final_sender("Message Type: FINAL_ANSWER\r\nTask name: /root\r\nSender: /root/a\r\nPayload:\r\ndone"),Some("/root/a"));
        assert_eq!(final_sender("Message Type: FINAL_ANSWER\nSender: /root/a\nPayload:\ndone"),None);
        assert_eq!(final_sender("Message Type: MESSAGE\nTask name: /root\nSender: /root/a\nPayload:\ndone"),None);
    }
}
