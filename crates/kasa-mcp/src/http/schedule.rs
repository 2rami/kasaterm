//! 스케줄러(반복 지시 루프 · 예약 크론 · 타이머/리마인더)와 디스패처(학생 자동 호출) 표면.
//!
//! 스케줄은 모두 「정해진 시각에 surface 로 text 를 send」로 통일한다. loop=interval 마다 반복,
//! cron=at_ts 1회, timer=now+interval 1회. 백그라운드 task 가 10s 마다 발사한다.
//! 디스패처의 판단·배정 로직은 `crate::dispatch` 에 있고 여기선 HTTP 표면만 붙인다.

use super::*;
use super::collab::{persist_sensei_msg, submit_payload};

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct ScheduleItem {
    pub id: String,
    pub kind: String, // "loop" | "cron" | "timer"
    pub surface: String,
    pub text: String,
    #[serde(default)]
    pub interval_sec: u64,
    #[serde(default)]
    pub at_ts: f64,
    #[serde(default)]
    pub next_ts: f64,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub label: String,
}

fn default_true() -> bool {
    true
}

fn now_unix() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn schedule_path() -> Option<std::path::PathBuf> {
    let home = kasa_socket::home_dir()?;
    Some(home.join(".config/kasaterm/schedule.json"))
}

fn read_schedule() -> Vec<ScheduleItem> {
    let Some(p) = schedule_path() else { return Vec::new() };
    let Ok(s) = std::fs::read_to_string(&p) else { return Vec::new() };
    serde_json::from_str(&s).unwrap_or_default()
}

fn write_schedule(items: &[ScheduleItem]) {
    let Some(p) = schedule_path() else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(s) = serde_json::to_string_pretty(items) {
        let _ = std::fs::write(&p, s);
    }
}

/// 네이티브 보드와 HTTP 화면이 함께 읽는 스케줄 스냅샷. 파일 접근이 있으므로
/// GUI 렌더가 아니라 worker에서 호출해야 한다.
pub fn schedule_snapshot() -> Vec<ScheduleItem> {
    read_schedule()
}

pub fn schedule_add(
    kind: &str,
    surface: &str,
    text: &str,
    interval_sec: u64,
    at_ts: f64,
    label: &str,
) -> anyhow::Result<String> {
    if !matches!(kind, "loop" | "cron" | "timer")
        || surface.is_empty()
        || text.trim().is_empty()
    {
        anyhow::bail!("kind/surface/text required");
    }
    let now = now_unix();
    let next_ts = if kind == "cron" {
        at_ts
    } else {
        now + interval_sec.max(1) as f64
    };
    let id = format!("{:08x}", (now * 1000.0) as u64 & 0xffff_ffff);
    let item = ScheduleItem {
        id: id.clone(),
        label: label.to_string(),
        kind: kind.to_string(),
        surface: surface.to_string(),
        text: text.trim().to_string(),
        interval_sec,
        at_ts,
        next_ts,
        enabled: true,
    };
    let mut items = read_schedule();
    items.push(item);
    write_schedule(&items);
    Ok(id)
}

pub fn schedule_toggle(id: &str) -> bool {
    let mut items = read_schedule();
    let mut changed = false;
    for item in &mut items {
        if item.id == id {
            item.enabled = !item.enabled;
            changed = true;
        }
    }
    if changed {
        write_schedule(&items);
    }
    changed
}

pub fn schedule_delete(id: &str) -> bool {
    let mut items = read_schedule();
    let before = items.len();
    items.retain(|item| item.id != id);
    let changed = before != items.len();
    if changed {
        write_schedule(&items);
    }
    changed
}

/// 10초마다 due 항목 발사. loop 는 next_ts 갱신, cron/timer 는 발사 후 disable.
pub(super) async fn schedule_loop(backend: Arc<dyn Backend>) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        let mut items = read_schedule();
        if items.is_empty() {
            continue;
        }
        let now = now_unix();
        let mut changed = false;
        for it in items.iter_mut() {
            if !it.enabled || it.next_ts <= 0.0 || now < it.next_ts {
                continue;
            }
            // 발사 — 학생 TUI 제출(submit_payload).
            let _ = backend.send_text(Some(&it.surface), &submit_payload(&it.text));
            // 모모톡에도 노란 버블로 — send_text 는 PTY 주입만 하고 messages.jsonl 에 안 남겨
            // 예약/타이머 발신이 대화창에 안 떴다(사용자). read=false 로 inbox(미확인) 기록.
            persist_sensei_msg(
                &resolve_cwd(&backend),
                &it.surface,
                &it.text,
                false,
                backend.active_room().as_deref(),
            );
            changed = true;
            match it.kind.as_str() {
                "loop" if it.interval_sec > 0 => {
                    it.next_ts = now + it.interval_sec as f64;
                }
                _ => {
                    it.enabled = false; // cron·timer 1회성
                }
            }
        }
        if changed {
            write_schedule(&items);
        }
    }
}

/// `GET /tasks` — 일감 큐 전체(부름 이력이 곧 이 목록이다).
async fn tasks_list_handler() -> impl IntoResponse {
    (
        [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")],
        Json(serde_json::json!({
            "ok": true,
            "items": crate::dispatch::read_queue(),
            "config": crate::dispatch::read_config(),
        })),
    )
}

/// `POST /task` — 작업 1건 직접 등록(판단기 없이). body{brief,files_hint?,depends_on?,
/// weight?,depth?}. 학생이 후속 작업을 넣을 때도 이 경로 — `depth>=1` 은 새 학생을
/// 못 부르고 빈 학생만 쓴다(증식 차단).
async fn task_add_handler(backend: Arc<dyn Backend>, body: String) -> impl IntoResponse {
    let cors = [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")];
    let v: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            return (cors, Json(serde_json::json!({ "ok": false, "error": format!("bad body: {e}") })));
        }
    };
    let brief = v.get("brief").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
    if brief.is_empty() {
        return (cors, Json(serde_json::json!({ "ok": false, "error": "brief required" })));
    }
    let strs = |key: &str| -> Vec<String> {
        v.get(key)
            .and_then(|x| x.as_array())
            .map(|a| a.iter().filter_map(|s| s.as_str()).map(|s| s.to_string()).collect())
            .unwrap_or_default()
    };
    // cwd 는 요청이 준 값 우선, 없으면 지금 방의 경로 — 학생이 어느 레포에서 뜰지가 여기서 정해진다.
    let cwd = v
        .get("cwd")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| resolve_cwd(&backend).to_string_lossy().to_string());
    let mut task = crate::dispatch::solo_task(&brief, &cwd);
    task.files_hint = strs("files_hint");
    task.depends_on = strs("depends_on");
    task.depth = v.get("depth").and_then(|x| x.as_u64()).unwrap_or(0).min(255) as u8;
    if let Some(w) = v.get("weight").and_then(|x| x.as_str()) {
        task.weight = w.to_string();
    }
    // 학생이 후속 작업을 넣을 때 자기 pane 을 주면 그 학생이 결과를 되받는다.
    if let Some(r) = v.get("report_to").and_then(|x| x.as_str()) {
        task.report_to = r.to_string();
    }
    let ids = crate::dispatch::push_tasks(vec![task]);
    (cors, Json(serde_json::json!({ "ok": true, "ids": ids })))
}

/// `POST /task-delete?id=<id>` — 큐에서 제거.
async fn task_delete_handler(
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let id = params.get("id").cloned().unwrap_or_default();
    let removed = crate::dispatch::delete_task(&id);
    (
        [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")],
        Json(serde_json::json!({ "ok": true, "removed": removed })),
    )
}

/// `POST /dispatch` — 지시 원문을 넣으면 판단기가 작업으로 쪼개 큐에 넣는다.
/// body{instruction}. 응답의 `note` 는 판단기가 실패해 1건으로 떨어진 사유(있을 때만).
async fn dispatch_handler(backend: Arc<dyn Backend>, body: String) -> impl IntoResponse {
    let cors = [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")];
    let parsed = serde_json::from_str::<serde_json::Value>(&body).ok();
    let instruction = match parsed.as_ref() {
        Some(v) => v.get("instruction").and_then(|x| x.as_str()).unwrap_or("").trim().to_string(),
        // 평문 body 도 받는다 — 지시 한 줄을 보내려고 JSON 을 만들 이유가 없다.
        None => body.trim().to_string(),
    };
    if instruction.is_empty() {
        return (cors, Json(serde_json::json!({ "ok": false, "error": "instruction required" })));
    }
    let report_to = parsed
        .as_ref()
        .and_then(|v| v.get("report_to").and_then(|x| x.as_str()))
        .unwrap_or("")
        .to_string();
    let (mut tasks, note) = crate::dispatch::plan_tasks(&instruction, &backend).await;
    for t in tasks.iter_mut() {
        t.report_to = report_to.clone();
    }
    let planned: Vec<serde_json::Value> = tasks
        .iter()
        .map(|t| serde_json::json!({ "brief": t.brief, "files_hint": t.files_hint, "weight": t.weight }))
        .collect();
    let ids = crate::dispatch::push_tasks(tasks);
    (
        cors,
        Json(serde_json::json!({ "ok": true, "ids": ids, "planned": planned, "note": note })),
    )
}

/// `POST /broadcast[?all=1]` (body=알릴 내용) — 외부에서 온 소식을 일하는 학생들에게
/// 흘린다. 슬랙·CI 훅이 "배포 실패했다" 를 던지는 통로 — 일감이 아니라 정보라 큐에
/// 넣지 않고 곧바로 각 pane 에 제출한다. `all=1` 은 board 의 모든 pane(선생님 화면 포함).
async fn broadcast_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
    body: String,
) -> impl IntoResponse {
    let cors = [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")];
    // JSON{text} 도, 평문도 받는다(훅 스크립트가 curl 한 줄로 끝나게).
    let text = match serde_json::from_str::<serde_json::Value>(&body) {
        Ok(v) => v.get("text").and_then(|x| x.as_str()).unwrap_or("").trim().to_string(),
        Err(_) => body.trim().to_string(),
    };
    if text.is_empty() {
        return (cors, Json(serde_json::json!({ "ok": false, "error": "text required" })));
    }
    let all = params.get("all").map(|s| s == "1").unwrap_or(false);
    let sent = crate::dispatch::broadcast(&backend, &text, all);
    (cors, Json(serde_json::json!({ "ok": true, "sent": sent })))
}

/// `GET /dispatch-config` · `POST /dispatch-config` — 자동 호출 스위치와 상한.
/// POST 는 준 필드만 덮어쓴다(부분 갱신) — 토글 하나 바꾸려고 전체를 보낼 이유가 없다.
async fn dispatch_config_handler(body: String) -> impl IntoResponse {
    let cors = [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")];
    let mut cfg = crate::dispatch::read_config();
    let v: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            return (cors, Json(serde_json::json!({ "ok": false, "error": format!("bad body: {e}") })));
        }
    };
    if let Some(b) = v.get("enabled").and_then(|x| x.as_bool()) {
        cfg.enabled = b;
    }
    if let Some(n) = v.get("max_students").and_then(|x| x.as_u64()) {
        cfg.max_students = n.clamp(1, 12) as usize;
    }
    if let Some(n) = v.get("idle_ticks").and_then(|x| x.as_u64()) {
        cfg.idle_ticks = n.clamp(1, 30) as u8;
    }
    if let Some(n) = v.get("settle_sec").and_then(|x| x.as_f64()) {
        cfg.settle_sec = n.clamp(5.0, 600.0);
    }
    if let Some(n) = v.get("context_cap").and_then(|x| x.as_u64()) {
        cfg.context_cap = n.clamp(10, 100) as u8;
    }
    if let Some(n) = v.get("max_attempts").and_then(|x| x.as_u64()) {
        cfg.max_attempts = n.clamp(1, 10) as u8;
    }
    for (key, slot) in [
        ("planner_model", &mut cfg.planner_model),
        ("heavy_model", &mut cfg.heavy_model),
        ("light_model", &mut cfg.light_model),
        // 가벼운 일을 싼 백엔드로 — `"glm"` 같은 셸 래퍼 이름.
        ("heavy_launcher", &mut cfg.heavy_launcher),
        ("light_launcher", &mut cfg.light_launcher),
    ] {
        if let Some(s) = v.get(key).and_then(|x| x.as_str()) {
            *slot = s.to_string();
        }
    }
    if let Some(a) = v.get("characters").and_then(|x| x.as_array()) {
        cfg.characters = a.iter().filter_map(|s| s.as_str()).map(|s| s.to_string()).collect();
    }
    crate::dispatch::write_config(&cfg);
    (cors, Json(serde_json::json!({ "ok": true, "config": cfg })))
}

/// `GET /schedule` — 스케줄 목록.
async fn schedule_list_handler() -> impl IntoResponse {
    (
        [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")],
        Json(serde_json::json!({ "ok": true, "items": read_schedule() })),
    )
}

/// `POST /schedule` — 항목 추가. body{kind,surface,text,interval_sec?,at_ts?,label?}.
/// next_ts 는 kind 로 계산(loop/timer=now+interval, cron=at_ts).
async fn schedule_add_handler(body: String) -> impl IntoResponse {
    let cors = [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")];
    let v: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            return (cors, Json(serde_json::json!({ "ok": false, "error": format!("bad body: {e}") })));
        }
    };
    let kind = v.get("kind").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let surface = v.get("surface").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let text = v.get("text").and_then(|x| x.as_str()).unwrap_or("").to_string();
    if !matches!(kind.as_str(), "loop" | "cron" | "timer") || surface.is_empty() || text.is_empty() {
        return (cors, Json(serde_json::json!({ "ok": false, "error": "kind/surface/text required" })));
    }
    let interval_sec = v.get("interval_sec").and_then(|x| x.as_u64()).unwrap_or(0);
    let at_ts = v.get("at_ts").and_then(|x| x.as_f64()).unwrap_or(0.0);
    let label = v.get("label").and_then(|x| x.as_str()).unwrap_or("");
    match schedule_add(&kind, &surface, &text, interval_sec, at_ts, label) {
        Ok(id) => (cors, Json(serde_json::json!({ "ok": true, "id": id }))),
        Err(error) => (cors, Json(serde_json::json!({ "ok": false, "error": error.to_string() }))),
    }
}

/// `POST /schedule-delete?id=<id>` — 항목 삭제(없으면 toggle 용 enabled 도 받음).
async fn schedule_delete_handler(
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let cors = [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")];
    let id = params.get("id").cloned().unwrap_or_default();
    if let Some(toggle) = params.get("toggle") {
        // toggle=1 → enabled 뒤집기(삭제 대신).
        if toggle == "1" {
            schedule_toggle(&id);
            return (cors, Json(serde_json::json!({ "ok": true })));
        }
    }
    let removed = usize::from(schedule_delete(&id));
    (cors, Json(serde_json::json!({ "ok": true, "removed": removed })))
}

/// 이 모듈 창구의 라우트. `router` 가 한 표로 합친 뒤 공통 레이어(Origin·토큰 가드)를 두른다.
pub(super) fn routes(backend: &Arc<dyn Backend>) -> axum::Router {
    let dispatch_backend = backend.clone();
    let task_add_backend = backend.clone();
    let broadcast_backend = backend.clone();
    axum::Router::new()
        .route("/tasks", get(tasks_list_handler))
        .route(
            "/task",
            post(move |body: String| task_add_handler(task_add_backend.clone(), body)),
        )
        .route(
            "/task-delete",
            post(|q: Query<std::collections::HashMap<String, String>>| {
                task_delete_handler(q)
            }),
        )
        .route(
            "/dispatch",
            post(move |body: String| dispatch_handler(dispatch_backend.clone(), body)),
        )
        .route(
            "/broadcast",
            post(
                move |q: Query<std::collections::HashMap<String, String>>, body: String| {
                    broadcast_handler(broadcast_backend.clone(), q, body)
                },
            ),
        )
        .route(
            "/dispatch-config",
            get(|| dispatch_config_handler("{}".to_string()))
                .post(|body: String| dispatch_config_handler(body)),
        )
        .route(
            "/schedule",
            get(schedule_list_handler).post(|body: String| schedule_add_handler(body)),
        )
        .route(
            "/schedule-delete",
            post(|q: Query<std::collections::HashMap<String, String>>| {
                schedule_delete_handler(q)
            }),
        )
}
