//! 학생 자동 호출 사슬을 GUI·실제 claude 없이 끝까지 돌린다.
//!
//!   cargo run -p kasa-mcp --example dispatch_e2e
//!   cargo run -p kasa-mcp --example dispatch_e2e -- --planner   (판단기까지 실제 호출)
//!
//! 가짜 백엔드가 pane 을 발급하고 화면 상태를 흉내낸다: 스폰 직후엔 working, 몇 tick
//! 뒤 idle + 마지막 답변. 그래야 "부팅 유예"와 "idle 연속" 게이트가 실제로 작동하는지
//! 보인다. 라이브 설정을 건드리지 않도록 큐·명부는 임시 디렉터리로 격리한다.

use anyhow::Result;
use kasa_mcp::dispatch;
use kasa_socket::backend::{Backend, PaneActivity, SplitDirection, SurfaceInfo, WorkspaceInfo};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Fleet {
    board: Vec<PaneActivity>,
    panes: Vec<String>,
    sent: Vec<(String, String)>,
    closed: Vec<String>,
    next: usize,
    /// true 면 스폰한 pane 에 claude 가 올라오지 않는다(런처 실패·키 만료 재현).
    boot_fails: bool,
}

struct FakeBackend(Mutex<Fleet>);

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
        Ok(SurfaceInfo { id: "%x".into(), workspace_id: "w".into(), title: None })
    }
    fn send_key(&self, _: Option<&str>, _: &str) -> Result<()> {
        Ok(())
    }

    fn send_text(&self, surface: Option<&str>, text: &str) -> Result<()> {
        let mut f = self.0.lock().unwrap();
        f.sent.push((surface.unwrap_or("-").to_string(), text.to_string()));
        Ok(())
    }

    /// 새 학생 — 스폰 직후는 working 이다(첫 턴을 돌고 있다). 이게 idle 로 뜨면
    /// 디스패처가 곧바로 완료로 오해하는지 시험할 수 없다.
    fn spawn_student(&self, character: &str) -> Result<String> {
        let mut f = self.0.lock().unwrap();
        f.next += 1;
        let sid = format!("%{}", f.next);
        // 실제 학생은 곧 파일을 잡는다 — 그래야 형제의 브리프에 "남이 잡은 파일"이 실린다.
        let claimed = format!("app/kasaterm/src/pane{}.rs", f.next);
        f.panes.push(sid.clone());
        if f.boot_fails {
            return Ok(sid); // pane 은 생겼지만 claude 는 없다
        }
        f.board.push(PaneActivity {
            surface_id: sid.clone(),
            status: "working".into(),
            character: Some(character.to_string()),
            files: vec![claimed],
            ..Default::default()
        });
        Ok(sid)
    }

    fn collab_board(&self) -> Result<Vec<PaneActivity>> {
        Ok(self.0.lock().unwrap().board.clone())
    }

    /// pane 은 board 와 별개로 존재한다 — 셸만 뜬 pane 도 여기 잡혀야 상한이 지켜진다.
    fn list_surfaces(&self) -> Result<Vec<SurfaceInfo>> {
        let f = self.0.lock().unwrap();
        Ok(f
            .panes
            .iter()
            .map(|id| SurfaceInfo { id: id.clone(), workspace_id: "w".into(), title: None })
            .collect())
    }

    fn close_surface(&self, id: &str) -> Result<()> {
        let mut f = self.0.lock().unwrap();
        f.panes.retain(|p| p != id);
        f.board.retain(|r| r.surface_id != id);
        f.closed.push(id.to_string());
        Ok(())
    }
}

fn finish(be: &FakeBackend, sid: &str, reply: &str) {
    let mut f = be.0.lock().unwrap();
    if let Some(r) = f.board.iter_mut().find(|r| r.surface_id == sid) {
        r.status = "idle".into();
        r.last_reply = reply.into();
    }
}

fn queue_line(t: &dispatch::QueueTask) -> String {
    format!(
        "  [{}] {} · {} {}{}",
        t.status,
        t.brief.chars().take(28).collect::<String>(),
        if t.surface.is_empty() { "-".into() } else { t.surface.clone() },
        if t.character.is_empty() { String::new() } else { format!("({}) ", t.character) },
        if t.result.is_empty() { String::new() } else { format!("→ {}", t.result) }
    )
}

fn main() -> Result<()> {
    // 라이브 큐·명부와 격리 — 검증이 선생님의 배정을 되돌리면 안 된다.
    let dir = std::env::temp_dir().join("kasaterm-dispatch-e2e");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    std::env::set_var("KASATERM_DISPATCH_DIR", &dir);
    println!("격리 디렉터리: {}", dir.display());

    dispatch::write_config(&dispatch::DispatchConfig {
        enabled: true,
        max_students: 2,
        idle_ticks: 2,
        settle_sec: 0.0, // 가짜 백엔드는 즉시 상태를 바꿔 유예가 필요 없다
        context_cap: 85,
        planner_model: "sonnet".into(),
        max_attempts: 2,
        heavy_model: String::new(),
        light_model: String::new(),
        heavy_launcher: String::new(),
        light_launcher: "glm".into(), // 가벼운 일은 게이트웨이로 — 명령 조립을 눈으로 보려고

        characters: vec!["미도리".into(), "유즈".into(), "아리스".into()],
    });

    let be = Arc::new(FakeBackend(Mutex::new(Fleet::default())));
    let backend: Arc<dyn Backend> = be.clone();
    let mut rt = dispatch::DispatchRuntime::default();

    // 독립 작업 3건, 상한 2 — 두 명만 부르고 하나는 대기해야 한다.
    dispatch::push_tasks(vec![
        mk("A: render.rs 손보기", &["app/kasaterm/src/render.rs"]),
        mk("B: http.rs 손보기", &["crates/kasa-mcp/src/http.rs"]),
        mk("C: layout.rs 손보기", &["app/kasaterm/src/layout.rs"]),
    ]);

    println!("\n[tick 1] 배정 — 상한 2 이므로 두 명만 불러야 한다");
    dispatch::dispatch_tick(&backend, &mut rt);
    dump(&be);

    println!("\n[tick 2] 아직 일하는 중 — 아무 변화가 없어야 한다");
    dispatch::dispatch_tick(&backend, &mut rt);
    dump(&be);

    println!("\n%1 이 보고하고 끝냈다고 표시");
    finish(&be, "%1", "A 끝냈어요");
    println!("[tick 3] idle 1회 — 아직 완료로 보면 안 된다");
    dispatch::dispatch_tick(&backend, &mut rt);
    dump(&be);

    println!("[tick 4] idle 2회 — 이제 수확 + 대기 중이던 C 를 그 학생에게");
    dispatch::dispatch_tick(&backend, &mut rt);
    dump(&be);

    println!("\n%1 이 닫혔다고 표시(선생님이 pane 종료)");
    be.0.lock().unwrap().board.retain(|r| r.surface_id != "%1");
    println!("[tick 5] 결과를 못 봤으니 done 이 아니라 pending 으로 되돌아야 한다");
    dispatch::dispatch_tick(&backend, &mut rt);
    dump(&be);

    println!("\n── 학생에게 실제로 간 명령 ──");
    for (sid, text) in be.0.lock().unwrap().sent.iter() {
        println!("  {sid} ← {}", text.replace('\r', "⏎").replace('\x15', "^U"));
    }

    // ── claude 가 올라오지 않는 경우 — 예전엔 여기서 pane 이 무한히 늘었다 ──
    println!("\n[부팅 실패 재현] 이후 스폰되는 pane 에는 claude 가 안 뜬다");
    {
        let mut f = be.0.lock().unwrap();
        f.boot_fails = true;
        f.board.clear(); // 기존 학생도 정리해 빈 상태에서 시작
        f.panes.clear();
        f.closed.clear();
    }
    dispatch::reset_on_boot();
    dispatch::push_tasks(vec![mk("D: 안 뜨는 학생", &["app/kasaterm/src/d.rs"])]);
    for i in 1..=9 {
        dispatch::dispatch_tick(&backend, &mut rt);
        let f = be.0.lock().unwrap();
        let st = dispatch::read_queue()
            .iter()
            .find(|t| t.brief.starts_with("D:"))
            .map(|t| format!("{}(시도 {})", t.status, t.attempts))
            .unwrap_or_default();
        println!("  tick {i}: 살아있는 pane {} · 닫은 pane {} · 작업 {}", f.panes.len(), f.closed.len(), st);
    }

    if std::env::args().any(|a| a == "--planner") {
        println!("\n── 판단기(실제 claude -p) ──");
        let rtio = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
        let (tasks, note) = rtio.block_on(dispatch::plan_tasks(
            "설정 화면 자간 슬라이더를 추가하고, 그게 잘 붙었는지 따로 확인해줘",
            &backend,
        ));
        if !note.is_empty() {
            println!("  note: {note}");
        }
        for t in tasks.iter() {
            println!("  · [{}] {} | 파일 {:?} | 선행 {:?}", t.weight, t.brief, t.files_hint, t.depends_on);
        }
    }
    Ok(())
}

fn mk(brief: &str, files: &[&str]) -> dispatch::QueueTask {
    dispatch::QueueTask {
        id: String::new(),
        brief: brief.into(),
        files_hint: files.iter().map(|s| s.to_string()).collect(),
        status: "pending".into(),
        surface: String::new(),
        character: String::new(),
        depends_on: Vec::new(),
        depth: 0,
        weight: "heavy".into(),
        origin: "설정 화면 자간 작업".into(),
        cwd: "/tmp/fake-repo".into(),
        report_to: "%9".into(),
        attempts: 0,
        result: String::new(),
        created_ts: 0.0,
        updated_ts: 0.0,
        assigned_ts: 0.0,
    }
}

fn dump(be: &FakeBackend) {
    for t in dispatch::read_queue().iter() {
        println!("{}", queue_line(t));
    }
    let f = be.0.lock().unwrap();
    let who: Vec<String> = f
        .board
        .iter()
        .map(|r| format!("{}={}({})", r.surface_id, r.status, r.character.clone().unwrap_or_default()))
        .collect();
    println!("  화면: {}", who.join(" "));
}
