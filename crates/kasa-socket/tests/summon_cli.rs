#![cfg(unix)]

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::process::{Command, Output};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

struct Rig {
    dir: std::path::PathBuf,
    seen: Arc<Mutex<Vec<Value>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

fn address(session: bool) -> Value {
    let mut a = json!({"machine_id":"mini", "surface_key":"created-key", "surface_id":"%4", "instance_id":"boot"});
    if session {
        a["session_id"] = json!("new-session");
    }
    a
}

impl Rig {
    fn new(rename_ok: bool) -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "kt-summon-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).unwrap();
        let listener = UnixListener::bind(dir.join("app.sock")).unwrap();
        listener.set_nonblocking(true).unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (records, stopping) = (seen.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            while !stopping.load(Ordering::Relaxed) {
                let (mut stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(2));
                        continue;
                    }
                    Err(e) => panic!("{e}"),
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                    .unwrap();
                let mut line = String::new();
                BufReader::new(&stream).read_line(&mut line).unwrap();
                let req: Value = serde_json::from_str(&line).unwrap();
                records.lock().unwrap().push(req.clone());
                let params = &req["params"];
                let result = match req["method"].as_str().unwrap() {
                    "collab.snapshot" => {
                        json!({"sources":[{"machine_id":"mini","label":"mini","is_local":true}],
                        "panes":[{"address":address(true),"character":"worker","machine_label":"mini"}]})
                    }
                    "collab.act" if params["op"] == "spawn" => {
                        json!({"address":address(false),"window":2})
                    }
                    "collab.act" => json!({"ok":true}),
                    other => panic!("unexpected method {other}"),
                };
                let response = if !rename_ok && params["op"] == "rename_room" {
                    json!({"id":req["id"],"ok":false,"error":{"code":-32603,"message":"room rename refused"}})
                } else {
                    json!({"id":req["id"],"ok":true,"result":result})
                };
                writeln!(stream, "{response}").unwrap();
            }
        });
        Self {
            dir,
            seen,
            stop,
            thread: Some(thread),
        }
    }

    fn run(&self, args: &[&str], parent: Option<&str>) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_kasaterm-cli"));
        cmd.args(args)
            .env_clear()
            .env("KASATERM_SOCKET_PATH", self.dir.join("app.sock"))
            .env("CMUX_SOCKET_PATH", self.dir.join("app.sock"))
            .current_dir(&self.dir);
        if let Some(parent) = parent {
            cmd.env("KASATERM_PANE_ID", parent);
        }
        cmd.output().unwrap()
    }

    fn actions(&self) -> Vec<Value> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["method"] == "collab.act")
            .map(|r| r["params"].clone())
            .collect()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let joined = self.thread.take().unwrap().join();
        if !std::thread::panicking() {
            joined.unwrap();
        }
        std::fs::remove_dir_all(&self.dir).unwrap();
    }
}

#[test]
fn remote_summon_without_a_room_does_not_create_a_pane() {
    let rig = Rig::new(true);
    let out = rig.run(
        &["summon", "--machine", "mini", "--cmd", "codex", "test"],
        Some("%0"),
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--new-room"));
    assert!(rig.actions().is_empty());
}

#[test]
fn an_external_caller_cannot_fall_back_to_the_active_pane() {
    let rig = Rig::new(true);
    let out = rig.run(&["summon", "--cmd", "codex", "test"], None);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("부른 칸이 없어요"));
    assert!(rig.seen.lock().unwrap().is_empty());
}

#[test]
fn new_room_is_named_before_boot_and_uses_only_one_pane() {
    for remote in [false, true] {
        let rig = Rig::new(true);
        let mut args = vec![
            "summon",
            "--new-room",
            "라이브러리",
            "--name",
            "사이트 비교",
            "--cmd",
            "codex",
            "--json",
        ];
        if remote {
            args.extend(["--machine", "mini"]);
        }
        args.push("test");
        let out = rig.run(&args, Some("%0"));
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let acts = rig.actions();
        let names: Vec<_> = acts.iter().map(|r| r["op"].as_str().unwrap()).collect();
        assert_eq!(names, ["spawn", "rename_room", "rename", "send_text"]);
        assert_eq!(acts[0]["window"], "new");
        assert!(acts[0].get("address").is_none());
        assert_eq!(acts[1]["title"], "라이브러리");
        assert_eq!(acts[2]["title"], "사이트 비교");
        assert_eq!(acts[3]["address"], address(false));
        let result: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(result["address"], address(true));
    }
}

#[test]
fn existing_room_uses_the_full_anchor_even_if_someone_else_is_focused() {
    let rig = Rig::new(true);
    let anchor = address(true).to_string();
    let out = rig.run(
        &[
            "summon", "--beside", &anchor, "--tab", "--cmd", "codex", "test",
        ],
        Some("%99"),
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let acts = rig.actions();
    assert_eq!(acts[0]["address"], address(true));
    assert_eq!(acts[0]["tab"], true);
    assert!(acts[0].get("window").is_none());
    assert!(acts.iter().all(|r| r["op"] != "rename_room"));
}

#[test]
fn failed_room_naming_leaves_a_recoverable_blank_without_booting_an_agent() {
    let rig = Rig::new(false);
    let out = rig.run(
        &[
            "summon",
            "--new-room",
            "test room",
            "--cmd",
            "codex",
            "test",
        ],
        None,
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("created-key"));
    let acts = rig.actions();
    assert_eq!(acts.len(), 2);
    assert_eq!(acts[1]["op"], "rename_room");
}
