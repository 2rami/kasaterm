//! `/relay/launcher/*` — 카사런처(앱 설치·업데이트 런처)가 같은 계정 기기들의 앱 판을 모으고,
//! 다른 기기에 업데이트를 맡긴다.
//!
//! - 런처 토큰(`kind: "launcher"`)은 여기와 `whoami`·`logout` 에서만 통한다. 업링크·계정 프록시(칸 조작)·
//!   설정 동기화는 `Gate::device_by_token` 이 거절한다 — 런처 기기는 앱 판만 다루는데, 같은 토큰이 학생 칸에
//!   닿으면 런처가 새는 순간 카사텀 기기가 새는 것과 같아진다.
//! - 맡길 수 있는 일은 「카탈로그의 그 앱을 최신 판으로」뿐이다. 주소·파일을 넘기지 않으니 받는 런처는 늘
//!   제 카탈로그·GitHub 의 sha256·서명 확인을 거친다. 계정이 새도 공식 최신 판 설치까지만 시킬 수 있다.
//! - 받는 쪽은 `GET /relay/launcher/sync` 를 25초까지 걸어 두고, 명령이 오거나 같은 계정 기기의 판이 바뀌면
//!   바로 돌아온다 — 진행 막대가 다른 기기 화면에서도 따라 움직인다.
//! - 상태는 메모리에만 둔다. 관문이 다시 뜨면 런처들이 1분 안에 다시 올린다.

use super::*;
use serde::Deserialize;
use serde_json::json;

pub(super) const KIND: &str = "launcher";

const MAX_BODY: usize = 32 * 1024;
const MAX_APPS: usize = 32;
const MAX_WAIT: Duration = Duration::from_secs(25);
/// 런처는 25초마다 다시 묻는다 — 두 번 놓치면 「연결 안 됨」.
const ONLINE_WITHIN: Duration = Duration::from_secs(70);
/// 받는 런처가 이 안에 안 가져가면 버린다. 꺼진 기기가 한참 뒤 켜지며 옛 명령으로 설치를 시작하지 않게.
const COMMAND_TTL: Duration = Duration::from_secs(120);
const MAX_COMMANDS: usize = 16;

#[derive(Default)]
pub(super) struct Board {
    inner: Mutex<Inner>,
    changed: tokio::sync::Notify,
}

#[derive(Default)]
struct Inner {
    /// 계정 → 그 계정 기기 판이 바뀐 횟수. 받는 쪽이 「내가 본 뒤로 바뀌었나」를 이것으로 본다.
    revs: HashMap<String, u64>,
    devices: HashMap<String, Entry>,
}

struct Entry {
    account: String,
    report: Option<Report>,
    contact: Instant,
    commands: Vec<Command>,
}

#[derive(Clone, serde::Serialize, Deserialize)]
struct Report {
    name: String,
    os: String,
    apps: Vec<AppState>,
    #[serde(default)]
    at: u64,
}

#[derive(Clone, serde::Serialize, Deserialize)]
struct AppState {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    installed: Option<String>,
    #[serde(default)]
    latest: Option<String>,
    #[serde(default)]
    status: String,
    #[serde(default)]
    phase: String,
    #[serde(default)]
    progress: f64,
    #[serde(default)]
    message: String,
}

#[derive(Clone, serde::Serialize)]
struct Command {
    id: u64,
    /// 없으면 그 기기의 업데이트 전부.
    app: Option<String>,
    from: String,
    #[serde(skip)]
    at: Instant,
}

fn clean(text: &str, max: usize) -> String {
    text.trim().chars().filter(|c| !c.is_control()).take(max).collect()
}

fn valid_app_id(id: &str) -> bool {
    (1..=40).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn sanitize(mut report: Report) -> Option<Report> {
    if report.apps.len() > MAX_APPS || !report.apps.iter().all(|a| valid_app_id(&a.id)) {
        return None;
    }
    report.name = clean(&report.name, 60);
    report.os = match report.os.as_str() {
        "macos" | "windows" => report.os,
        _ => "other".into(),
    };
    for app in &mut report.apps {
        app.name = clean(&app.name, 60);
        app.installed = app.installed.as_deref().map(|v| clean(v, 40)).filter(|v| !v.is_empty());
        app.latest = app.latest.as_deref().map(|v| clean(v, 40)).filter(|v| !v.is_empty());
        app.status = clean(&app.status, 24);
        app.phase = clean(&app.phase, 24);
        app.progress = if app.progress.is_finite() { app.progress.clamp(0.0, 1.0) } else { 0.0 };
        app.message = clean(&app.message, 200);
    }
    report.at = now_secs();
    Some(report)
}

impl Board {
    fn touch(&self, device: &str, account: &str) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let entry = inner.devices.entry(device.to_string()).or_insert_with(|| Entry {
            account: account.to_string(),
            report: None,
            contact: Instant::now(),
            commands: Vec::new(),
        });
        let was_online = entry.contact.elapsed() < ONLINE_WITHIN && entry.report.is_some();
        entry.account = account.to_string();
        entry.contact = Instant::now();
        let back = !was_online && entry.report.is_some();
        if back {
            *inner.revs.entry(account.to_string()).or_default() += 1;
        }
        back
    }

    fn put(&self, device: &str, account: &str, report: Report) {
        let mut inner = self.inner.lock().unwrap();
        let entry = inner.devices.entry(device.to_string()).or_insert_with(|| Entry {
            account: account.to_string(),
            report: None,
            contact: Instant::now(),
            commands: Vec::new(),
        });
        entry.account = account.to_string();
        entry.contact = Instant::now();
        entry.report = Some(report);
        *inner.revs.entry(account.to_string()).or_default() += 1;
        drop(inner);
        self.changed.notify_waiters();
    }

    /// 명령이 있거나 `seen` 뒤로 판이 바뀌었으면 지금 줄 몸통, 아니면 None. 준 명령은 가져간 것으로 본다.
    fn ready(&self, gate: &Gate, me: &str, account: &str, seen: Option<u64>, force: bool) -> Option<serde_json::Value> {
        let active = active_devices(gate, account);
        let mut inner = self.inner.lock().unwrap();
        let rev = inner.revs.get(account).copied().unwrap_or(0);
        let commands = match inner.devices.get_mut(me) {
            Some(entry) => {
                entry.commands.retain(|c| c.at.elapsed() < COMMAND_TTL);
                std::mem::take(&mut entry.commands)
            }
            None => Vec::new(),
        };
        if !force && commands.is_empty() && seen == Some(rev) {
            return None;
        }
        let mut devices: Vec<serde_json::Value> = inner
            .devices
            .iter()
            .filter(|(id, e)| e.account == account && active.contains(*id))
            .filter_map(|(id, e)| {
                let report = e.report.as_ref()?;
                Some(json!({
                    "device_id": id, "name": report.name, "os": report.os, "apps": report.apps,
                    "reported_at": report.at, "online": e.contact.elapsed() < ONLINE_WITHIN, "current": id == me,
                }))
            })
            .collect();
        devices.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        Some(json!({ "ok": true, "rev": rev, "devices": devices, "commands": commands }))
    }

    fn queue(&self, gate: &Gate, account: &str, target: &str, app: Option<String>, from: String) -> Result<(), (StatusCode, &'static str)> {
        if !active_devices(gate, account).contains(target) {
            return Err((StatusCode::NOT_FOUND, "no_such_device"));
        }
        let mut inner = self.inner.lock().unwrap();
        let Some(entry) = inner.devices.get_mut(target).filter(|e| e.account == account && e.report.is_some()) else {
            return Err((StatusCode::NOT_FOUND, "no_such_device"));
        };
        if entry.contact.elapsed() >= ONLINE_WITHIN {
            return Err((StatusCode::CONFLICT, "device_offline"));
        }
        entry.commands.retain(|c| c.at.elapsed() < COMMAND_TTL);
        let covered = entry.commands.iter().any(|c| c.app.is_none() || c.app == app);
        if !covered {
            if entry.commands.len() >= MAX_COMMANDS {
                return Err((StatusCode::TOO_MANY_REQUESTS, "too_many_commands"));
            }
            let id = gate.seq.fetch_add(1, Ordering::Relaxed);
            entry.commands.push(Command { id, app, from, at: Instant::now() });
        }
        drop(inner);
        self.changed.notify_waiters();
        Ok(())
    }
}

/// 이 계정의 살아 있는 런처 기기 id. 폐기·로그아웃된 기기는 목록에서도 명령에서도 빠진다.
fn active_devices(gate: &Gate, account: &str) -> std::collections::HashSet<String> {
    if !gate.account_active(account) {
        return Default::default();
    }
    gate.devices
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, d)| d.revoked_at.is_none() && d.account == account && d.kind == KIND)
        .map(|(id, _)| id.clone())
        .collect()
}

pub(super) fn routes() -> Router<Gate> {
    Router::new()
        .route("/relay/launcher/state", axum::routing::put(put_state))
        .route("/relay/launcher/sync", get(sync))
        .route("/relay/launcher/devices/{id}/install", axum::routing::post(install))
}

fn launcher(gate: &Gate, headers: &axum::http::HeaderMap) -> Result<(String, DeviceRec), axum::response::Response> {
    match gate.any_device_of(headers) {
        Some((id, rec)) if rec.kind == KIND => Ok((id, rec)),
        Some(_) => Err(json_err(StatusCode::FORBIDDEN, "launcher_only")),
        None => Err(json_err(StatusCode::UNAUTHORIZED, "unauthorized")),
    }
}

async fn read_body<T: serde::de::DeserializeOwned>(req: axum::extract::Request) -> Result<T, axum::response::Response> {
    let bytes = match tokio::time::timeout(Duration::from_secs(10), axum::body::to_bytes(req.into_body(), MAX_BODY)).await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(_)) => return Err(json_err(StatusCode::PAYLOAD_TOO_LARGE, "body_too_large")),
        Err(_) => return Err(json_err(StatusCode::REQUEST_TIMEOUT, "request_timeout")),
    };
    serde_json::from_slice(&bytes).map_err(|_| json_err(StatusCode::BAD_REQUEST, "bad_request"))
}

/// `PUT /relay/launcher/state` — 이 기기의 앱 판·진행 상태.
async fn put_state(State(gate): State<Gate>, req: axum::extract::Request) -> axum::response::Response {
    let headers = req.headers().clone();
    if let Err(denied) = launcher(&gate, &headers) {
        return denied;
    }
    let report: Report = match read_body(req).await {
        Ok(report) => report,
        Err(error) => return error,
    };
    let Some(report) = sanitize(report) else {
        return json_err(StatusCode::BAD_REQUEST, "bad_state");
    };
    // 몸통을 받는 사이 로그아웃·폐기됐을 수 있어 다시 본다.
    let (me, rec) = match launcher(&gate, &headers) {
        Ok(found) => found,
        Err(denied) => return denied,
    };
    gate.launcher.put(&me, &rec.account, report);
    axum::Json(json!({ "ok": true })).into_response()
}

#[derive(Deserialize)]
struct SyncQuery {
    rev: Option<u64>,
    wait: Option<u64>,
}

/// `GET /relay/launcher/sync?rev=<본 판>&wait=<초>` — 이 기기에 맡겨진 명령과 같은 계정 기기 판.
/// 명령이 없고 `rev` 뒤로 바뀐 것도 없으면 `wait`(최대 25초)까지 기다렸다 그대로 준다.
async fn sync(
    State(gate): State<Gate>,
    headers: axum::http::HeaderMap,
    axum::extract::Query(q): axum::extract::Query<SyncQuery>,
) -> axum::response::Response {
    let (me, rec) = match launcher(&gate, &headers) {
        Ok(found) => found,
        Err(denied) => return denied,
    };
    if gate.launcher.touch(&me, &rec.account) {
        gate.launcher.changed.notify_waiters();
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(q.wait.unwrap_or(0)).min(MAX_WAIT);
    loop {
        let changed = gate.launcher.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if let Some(body) = gate.launcher.ready(&gate, &me, &rec.account, q.rev, false) {
            return axum::Json(body).into_response();
        }
        if tokio::time::timeout_at(deadline, changed).await.is_err() {
            break;
        }
    }
    match gate.launcher.ready(&gate, &me, &rec.account, q.rev, true) {
        Some(body) => axum::Json(body).into_response(),
        None => json_err(StatusCode::INTERNAL_SERVER_ERROR, "unavailable"),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallBody {
    #[serde(default)]
    app: Option<String>,
}

/// `POST /relay/launcher/devices/{id}/install {app?}` — 그 기기에 업데이트를 맡긴다. `app` 이 없으면 전부.
async fn install(State(gate): State<Gate>, AxPath(target): AxPath<String>, req: axum::extract::Request) -> axum::response::Response {
    let headers = req.headers().clone();
    if let Err(denied) = launcher(&gate, &headers) {
        return denied;
    }
    let body: InstallBody = match read_body(req).await {
        Ok(body) => body,
        Err(error) => return error,
    };
    if body.app.as_deref().is_some_and(|app| !valid_app_id(app)) {
        return json_err(StatusCode::BAD_REQUEST, "bad_app");
    }
    let (me, rec) = match launcher(&gate, &headers) {
        Ok(found) => found,
        Err(denied) => return denied,
    };
    let from = {
        let inner = gate.launcher.inner.lock().unwrap();
        inner.devices.get(&me).and_then(|e| e.report.as_ref()).map(|r| r.name.clone())
    }
    .filter(|name| !name.is_empty())
    .unwrap_or(rec.label);
    match gate.launcher.queue(&gate, &rec.account, &target, body.app, from) {
        Ok(()) => axum::Json(json!({ "ok": true })).into_response(),
        Err((status, code)) => json_err(status, code),
    }
}

#[cfg(test)]
#[path = "gateway_launcher/tests.rs"]
mod tests;
