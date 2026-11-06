//! ACP runner 학생으로 디스패처 사슬을 끝까지 돌린다 — GUI 없이, 진짜 어댑터로.
//!
//!   cargo build -p kasa-acp --bin kasa-acp
//!   cargo run -p kasa-students --example dispatch_acp_e2e -- codex|claude
//!
//! 가짜 백엔드의 「칸」은 셸 하나다(맥 `sh`, Windows `cmd`). 디스패처가 그 셸 문법으로 지은 부팅 줄을 실제로
//! 돌리므로 부팅 줄의 인용,
//! inbox 로 넣은 첫 지시, 상태 파일, 수확, inbox 로 다음 일 주기, 끝난 runner 치우기가 실물로 지나간다.
//! 큐·명부·상태 파일은 실행마다 새 임시 폴더에 둔다(라이브 설정과 격리).

use anyhow::{Result, bail};
use kasa_socket::backend::{Backend, PaneActivity, SplitDirection, SurfaceInfo, WorkspaceInfo};
use kasa_students::dispatch;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Pane {
    id: String,
    shell: Option<Child>,
}

struct Fleet {
    panes: Vec<Pane>,
    boots: Vec<(String, String)>,
    closed: Vec<String>,
    next: usize,
    logs: PathBuf,
}

struct FakeBackend(Mutex<Fleet>);

impl FakeBackend {
    fn log_of(&self, id: &str) -> PathBuf {
        self.0.lock().unwrap().logs.join(format!("{}.log", id.trim_start_matches('%')))
    }

    /// 칸의 셸 그룹에 신호를 보낸다 — 사람이 칸에서 Ctrl-C 를 누른 것과 같다. Windows 는 TERM 자리에
    /// 트리째 끄기만 한다(콘솔 Ctrl-C 를 남의 콘솔에 보낼 길이 없다).
    fn signal(&self, id: &str, sig: &str) {
        let f = self.0.lock().unwrap();
        let Some(child) = f.panes.iter().find(|p| p.id == id).and_then(|p| p.shell.as_ref()) else { return };
        if cfg!(windows) {
            if sig == "-TERM" {
                let _ = Command::new("taskkill").args(["/T", "/F", "/PID", &child.id().to_string()]).status();
            }
        } else {
            let _ = Command::new("kill").args([sig, "--", &format!("-{}", child.id())]).status();
        }
    }
}

impl Backend for FakeBackend {
    fn list_workspaces(&self) -> Result<Vec<WorkspaceInfo>> {
        Ok(vec![WorkspaceInfo { id: "w".into(), name: "w".into() }])
    }
    fn current_workspace(&self) -> Result<Option<WorkspaceInfo>> {
        Ok(None)
    }
    fn focus_surface(&self, _: &str) -> Result<()> {
        Ok(())
    }
    fn split_surface(&self, _: SplitDirection, _: bool, _: Option<&str>) -> Result<SurfaceInfo> {
        bail!("not used")
    }
    fn send_key(&self, _: Option<&str>, _: &str) -> Result<()> {
        Ok(())
    }

    /// 칸에 친 줄. Enter 로 끝나면 그 칸의 셸로 실제로 돌린다.
    fn send_text(&self, surface: Option<&str>, text: &str) -> Result<()> {
        let id = surface.unwrap_or("-").to_string();
        let log = self.log_of(&id);
        let mut f = self.0.lock().unwrap();
        f.boots.push((id.clone(), text.to_string()));
        let Some(line) = text.strip_suffix(['\r', '\n']) else { return Ok(()) };
        let Some(pane) = f.panes.iter_mut().find(|p| p.id == id) else { bail!("no pane {id}") };
        let out = std::fs::File::create(&log)?;
        let mut cmd = shell_running(line);
        cmd.stdin(Stdio::null()).stdout(out.try_clone()?).stderr(out);
        pane.shell = Some(cmd.spawn()?);
        Ok(())
    }

    fn pane_shell(&self, _: &str) -> Option<String> {
        Some(if cfg!(windows) { "cmd.exe" } else { "zsh" }.into())
    }

    fn spawn_student(&self, _character: &str) -> Result<String> {
        let mut f = self.0.lock().unwrap();
        f.next += 1;
        let id = format!("%{}", f.next);
        f.panes.push(Pane { id: id.clone(), shell: None });
        Ok(id)
    }

    fn collab_board(&self) -> Result<Vec<PaneActivity>> {
        Ok(Vec::new())
    }

    fn list_surfaces(&self) -> Result<Vec<SurfaceInfo>> {
        let f = self.0.lock().unwrap();
        Ok(f.panes
            .iter()
            .map(|p| SurfaceInfo { id: p.id.clone(), workspace_id: "w".into(), title: None, cwd: None, character: None })
            .collect())
    }

    fn close_surface(&self, id: &str) -> Result<()> {
        self.signal(id, "-TERM");
        let mut f = self.0.lock().unwrap();
        if let Some(mut child) = f.panes.iter_mut().find(|p| p.id == id).and_then(|p| p.shell.take()) {
            let _ = child.wait();
        }
        f.panes.retain(|p| p.id != id);
        f.closed.push(id.to_string());
        Ok(())
    }
}

#[cfg(windows)]
fn shell_running(line: &str) -> Command {
    use std::os::windows::process::CommandExt;
    let mut cmd = Command::new("cmd");
    // 줄을 그대로 넘긴다 — Rust 의 인자 인용은 cmd 가 모르는 `\"` 를 쓴다.
    cmd.args(["/D", "/S", "/C"]).raw_arg(format!("\"{line}\""));
    cmd
}

#[cfg(not(windows))]
fn shell_running(line: &str) -> Command {
    use std::os::unix::process::CommandExt;
    let mut cmd = Command::new("sh");
    cmd.args(["-c", line]).process_group(0);
    cmd
}

fn task(brief: &str, cwd: &Path, files: &[&str], deps: &[&str]) -> dispatch::QueueTask {
    dispatch::QueueTask {
        id: String::new(),
        brief: brief.into(),
        files_hint: files.iter().map(|f| cwd.join(f).to_string_lossy().into_owned()).collect(),
        status: "pending".into(),
        surface: String::new(),
        character: String::new(),
        depends_on: deps.iter().map(|d| (*d).to_string()).collect(),
        depth: 0,
        weight: "heavy".into(),
        origin: "ACP e2e".into(),
        cwd: cwd.to_string_lossy().into_owned(),
        report_to: String::new(),
        attempts: 0,
        result: String::new(),
        created_ts: 0.0,
        updated_ts: 0.0,
        assigned_ts: 0.0,
        acp_turn: 0,
    }
}

fn queue_line(t: &dispatch::QueueTask) -> String {
    format!("{}:{}@{}#{} {}", t.id, t.status, t.surface, t.acp_turn, t.result.lines().next().unwrap_or_default())
}

fn tick_until(
    backend: &Arc<dyn Backend>,
    rt: &mut dispatch::DispatchRuntime,
    limit: Duration,
    done: impl Fn(&[dispatch::QueueTask]) -> bool,
) -> bool {
    let started = Instant::now();
    let mut last = String::new();
    while started.elapsed() < limit {
        dispatch::dispatch_tick(backend, rt);
        let q = dispatch::read_queue();
        let now = q.iter().map(queue_line).collect::<Vec<_>>().join(" | ");
        if now != last {
            println!("  {:>5.1}s  {now}", started.elapsed().as_secs_f64());
            last = now;
        }
        if done(&q) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    false
}

fn main() -> Result<()> {
    let harness = std::env::args().nth(1).unwrap_or_else(|| "codex".into());
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis();
    let root = std::env::temp_dir().join(format!("kasaterm-dispatch-acp-{harness}-{stamp}"));
    let repo = root.join("repo");
    let logs = root.join("panes");
    std::fs::create_dir_all(&repo)?;
    std::fs::create_dir_all(&logs)?;
    Command::new("git").args(["init", "-q"]).current_dir(&repo).status()?;
    std::env::set_var("KASATERM_DISPATCH_DIR", root.join("config"));
    // 예제 실행 파일 옆에는 kasa-acp 가 없다 — 칸 셸이 target/debug 의 것을 찾게 한다.
    let bin_dir = std::env::current_exe()?.parent().and_then(Path::parent).map(Path::to_path_buf);
    if let Some(bin_dir) = bin_dir {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let joined = std::env::join_paths(std::iter::once(bin_dir).chain(std::env::split_paths(&path)))?;
        std::env::set_var("PATH", joined);
    }
    println!("격리 폴더: {}", root.display());

    dispatch::write_config(&dispatch::DispatchConfig {
        enabled: true,
        max_students: 1,
        characters: vec!["미도리".into()],
        runtime: "acp".into(),
        acp_harness: harness.clone(),
        // 사람 없는 학생 — 파일 쓰기는 묻지 않게 한다. codex 는 기본(자동 검토)으로 충분하다.
        acp_mode: if harness == "claude" { "acceptEdits".into() } else { String::new() },
        ..Default::default()
    });

    let be = Arc::new(FakeBackend(Mutex::new(Fleet {
        panes: Vec::new(),
        boots: Vec::new(),
        closed: Vec::new(),
        next: 0,
        logs,
    })));
    let backend: Arc<dyn Backend> = be.clone();
    let mut rt = dispatch::DispatchRuntime::default();

    let ids = dispatch::push_tasks(vec![task(
        "Use your file editing tool to create a.txt containing exactly one line: one\nThen reply with exactly: T1-DONE",
        &repo,
        &["a.txt"],
        &[],
    )]);
    dispatch::push_tasks(vec![task(
        "Use your file editing tool to append a second line to a.txt: two\nThen reply with exactly: T2-DONE",
        &repo,
        &["a.txt"],
        &[&ids[0]],
    )]);

    println!("\n[1] 두 작업 — 상한 1, 둘째는 첫째 뒤. 한 학생이 inbox 로 이어 받아야 한다");
    let finished = tick_until(&backend, &mut rt, Duration::from_secs(400), |q| {
        q.iter().all(|t| t.status == "done" || t.status == "failed")
    });
    let q = dispatch::read_queue();
    let boots: Vec<_> = be.0.lock().unwrap().boots.clone();
    let content = std::fs::read_to_string(repo.join("a.txt")).unwrap_or_default();
    println!("  끝남 {finished} · 부팅 줄 {}개 · a.txt = {content:?}", boots.len());
    for (id, line) in &boots {
        println!("  {id} ← {}", line.replace('\r', "⏎"));
    }

    println!("\n[2] runner 를 Ctrl-C 로 끝낸다 — 다음 tick 에 칸을 치우고 명부에서 빼야 한다");
    if cfg!(windows) {
        println!("  Windows 는 남의 콘솔에 Ctrl-C 를 보낼 길이 없어 건너뛴다");
    } else if let Some(id) = boots.first().map(|(id, _)| id.clone()) {
        be.signal(&id, "-INT");
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(20) && !be.0.lock().unwrap().closed.contains(&id) {
            dispatch::dispatch_tick(&backend, &mut rt);
            std::thread::sleep(Duration::from_millis(500));
        }
        println!("  닫은 칸 {:?}", be.0.lock().unwrap().closed);
        let screen = std::fs::read_to_string(be.log_of(&id)).unwrap_or_default();
        println!("\n── {id} 칸 화면 ──\n{}", screen.trim_end());
    }

    let screen = boots.first().map(|(id, _)| std::fs::read_to_string(be.log_of(id)).unwrap_or_default());
    if cfg!(windows) {
        println!("\n── 칸 화면 ──\n{}", screen.as_deref().unwrap_or_default().trim_end());
    }
    let retired = cfg!(windows) || boots.first().is_some_and(|(id, _)| be.0.lock().unwrap().closed.contains(id));
    let leftover: Vec<String> = be.0.lock().unwrap().panes.iter().map(|p| p.id.clone()).collect();
    for id in &leftover {
        let _ = backend.close_surface(id);
    }
    let ok = retired
        && finished
        && q.iter().all(|t| t.status == "done")
        && q.first().is_some_and(|t| t.result.contains("T1-DONE"))
        && q.get(1).is_some_and(|t| t.result.contains("T2-DONE") && t.acp_turn == 2)
        && boots.len() == 1
        && content.lines().collect::<Vec<_>>() == ["one", "two"];
    println!("\n결과: {}", if ok { "통과" } else { "실패" });
    if !ok {
        bail!("ACP dispatch e2e failed");
    }
    Ok(())
}
