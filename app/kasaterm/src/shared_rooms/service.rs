//! 이 기기에서 도는 공유방 일 — 원본 게시(`publisher`)와 받은 포크 열기(`fork`)의 작업표, host 창구 응답.
//!
//! 작업은 계정 신원(auth epoch·scope)에 묶인다. 계정이 바뀌면 진행 중인 작업은 실패로 끝나고 게시도 멈춘다.
//! 네트워크·파일 일은 배경 스레드가 하고 결과는 `UserEvent::SharedLocal` 로 앱 스레드에 돌아온다.

use super::*;
use std::path::PathBuf;
use std::sync::mpsc::Sender;

const MAX_OPERATIONS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    Preparing,
    Starting,
    Ready,
    NeedsInput,
    Failed,
}

impl Status {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Preparing => "preparing",
            Self::Starting => "starting",
            Self::Ready => "ready",
            Self::NeedsInput => "needs_input",
            Self::Failed => "failed",
        }
    }

    pub(crate) fn finished(self) -> bool {
        matches!(self, Self::Ready | Self::NeedsInput | Self::Failed)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Fork,
    Publish,
}

#[derive(Clone, Debug)]
pub(crate) struct OperationView {
    pub(crate) id: String,
    pub(crate) kind: Kind,
    pub(crate) status: Status,
    pub(crate) address: Option<serde_json::Value>,
    pub(crate) child_session_id: Option<String>,
    pub(crate) error: Option<String>,
    /// 받은 자료에서 뺀 것·프로젝트 안내 등 화면에 함께 보일 말.
    pub(crate) notes: Vec<String>,
    pub(super) epoch: u64,
    pub(super) key: String,
    pub(super) scope: String,
    pub(super) auth_scope: String,
    pub(super) parameters: String,
    pub(super) view_sequence: Option<u64>,
    pub(super) command_sent: bool,
}

impl OperationView {
    pub(crate) fn to_json(&self) -> serde_json::Value {
        let mut value = serde_json::json!({
            "operation_id": self.id,
            "kind": if self.kind == Kind::Fork { "fork" } else { "publish" },
            "status": self.status.as_str(),
        });
        if let Some(address) = &self.address { value["address"] = address.clone(); }
        if let Some(child) = &self.child_session_id { value["child_session_id"] = serde_json::json!(child); }
        if let Some(error) = &self.error { value["error"] = serde_json::json!(error); }
        if !self.notes.is_empty() { value["notes"] = serde_json::json!(self.notes); }
        value
    }
}

#[derive(Default)]
pub(crate) struct Runtime {
    pub(super) operations: Vec<OperationView>,
    pub(super) launches: Vec<fork::Launch>,
    pub(super) publisher: publisher::Publisher,
    host_requests: HashMap<(String, String), serde_json::Value>,
}

/// 배경 일의 결과는 시작할 때의 계정 신원(`epoch`·`scope`=auth scope)을 싣는다. 그 사이 계정이 바뀌었으면 버린다.
#[derive(Clone)]
pub(crate) enum Event {
    Host { action: String, body: serde_json::Value, guard: HostGuard, reply: Sender<Result<serde_json::Value, String>> },
    ForkPrepared { operation: String, epoch: u64, scope: Option<String>, result: Result<fork::Prepared, String> },
    Published { operation: String, epoch: u64, scope: Option<String>, sequence: u64, result: Result<publisher::Report, String> },
    Synced { epoch: u64, scope: Option<String>, sequence: u64, report: publisher::Report },
}

#[derive(Clone)]
pub(crate) struct HostGuard {
    epoch: u64,
    client: Option<Client>,
    deadline: Instant,
}

impl HostGuard {
    pub(crate) fn capture() -> Self {
        Self { epoch: kasa_device::device_auth::change_epoch(), client: capture_client().ok(),
            deadline: Instant::now() + std::time::Duration::from_secs(5) }
    }

    fn valid(&self, action: &str) -> bool {
        Instant::now() < self.deadline && self.epoch == kasa_device::device_auth::change_epoch()
            && match &self.client { Some(client) => client.is_current(), None => action == "status" }
    }
}

impl std::fmt::Debug for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Host { .. } => "SharedLocal::Host",
            Self::ForkPrepared { .. } => "SharedLocal::ForkPrepared",
            Self::Published { .. } => "SharedLocal::Published",
            Self::Synced { .. } => "SharedLocal::Synced",
        })
    }
}

/// 공유방 로컬 자료(포크 staging·게시 바인딩) 자리. 격리 리그는 그 collab 루트 아래로 간다.
pub(super) fn shared_dir() -> Result<PathBuf, String> {
    let root = kasa_socket::isolated_collab_root().map(|root| root.join("shared-rooms"))
        .or_else(|| kasa_socket::home_dir().map(|home| home.join(".config/kasaterm/shared-rooms")))
        .ok_or_else(|| "홈 폴더를 찾지 못했어요".to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&root).map_err(|e| e.to_string())?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    Ok(root)
}

pub(super) fn new_operation_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

impl App {
    pub(crate) fn shared_operation(&self, id: &str) -> Option<OperationView> {
        let client = self.shared_rooms.client.as_ref().filter(|client| client.is_current())?;
        self.shared_rooms.runtime.operations.iter().find(|op| op.id == id && op.scope == client.scope()).cloned()
    }

    pub(super) fn shared_operation_mut(&mut self, id: &str) -> Option<&mut OperationView> {
        self.shared_rooms.runtime.operations.iter_mut().find(|op| op.id == id)
    }

    /// 같은 열쇠(포크 id·게시 대상)의 끝나지 않은 작업이 있으면 그것을 돌려준다 — 재시도가 두 번 일하지 않는다.
    pub(super) fn shared_begin_operation(&mut self, kind: Kind, key: String, parameters: String) -> Result<(String, bool), String> {
        let client = self.shared_rooms.client.as_ref().filter(|client| client.is_current()).ok_or("KASA 계정에 로그인해 주세요")?;
        let (scope, auth_scope) = (client.scope().to_string(), client.auth_scope().to_string());
        let view_sequence = None;
        let runtime = &mut self.shared_rooms.runtime;
        if let Some(op) = runtime.operations.iter_mut().find(|op| op.key == key && op.scope == scope) {
            if op.parameters != parameters { return Err("같은 요청의 목적지나 옵션이 달라졌어요. 새 포크로 시작해 주세요".into()); }
            if op.command_sent || (op.status != Status::Failed && !(kind == Kind::Publish && op.status.finished())) {
                return Ok((op.id.clone(), false));
            }
            op.status = Status::Preparing;
            op.error = None;
            op.epoch = kasa_device::device_auth::change_epoch();
            op.auth_scope = auth_scope;
            op.view_sequence = view_sequence;
            return Ok((op.id.clone(), true));
        }
        if runtime.operations.len() >= MAX_OPERATIONS {
            if let Some(at) = runtime.operations.iter().position(|op| op.status.finished()) { runtime.operations.remove(at); }
            else { return Err("진행 중인 공유 작업이 많아요. 끝난 뒤 다시 시도해 주세요".into()); }
        }
        let id = new_operation_id();
        runtime.operations.push(OperationView {
            id: id.clone(), kind, status: Status::Preparing, address: None, child_session_id: None, error: None,
            notes: Vec::new(), epoch: kasa_device::device_auth::change_epoch(), key, scope, auth_scope, parameters,
            view_sequence, command_sent: false,
        });
        Ok((id, true))
    }

    /// 시작할 때와 같은 계정의 결과인가. 앱이 아는 계정(auth scope)이 다르면 늦은 응답이다.
    fn shared_same_account(&self, epoch: u64, scope: Option<&str>) -> bool {
        epoch == kasa_device::device_auth::change_epoch()
            && self.shared_rooms.client.as_ref().is_some_and(|client| client.is_current() && scope == Some(client.auth_scope()))
    }

    pub(super) fn shared_finish(&mut self, id: &str, status: Status, error: Option<String>) {
        if let Some(op) = self.shared_operation_mut(id) {
            op.status = status;
            op.error = error.clone();
        }
        if let Some(error) = error.filter(|_| status != Status::Ready && self.shared_operation(id).is_some()) {
            self.shared_rooms.error = Some(error);
        }
        self.chrome_dirty = true;
        if let Some(w) = &self.window { w.request_redraw(); }
    }

    pub(crate) fn shared_local_event(&mut self, event: Event) {
        match event {
            Event::Host { action, body, guard, reply } => {
                let result = if guard.valid(&action) {
                    if self.shared_rooms.client.as_ref().map(Client::auth_scope) != guard.client.as_ref().map(Client::auth_scope) {
                        self.shared_rooms.clear_account();
                        self.shared_rooms.client = guard.client;
                    }
                    self.shared_host(&action, &body)
                } else { Err("요청이 만료됐거나 계정이 바뀌었어요. 다시 시도해 주세요".into()) };
                let _ = reply.send(result);
            }
            Event::ForkPrepared { operation, epoch, scope, result } => {
                if !self.shared_same_account(epoch, scope.as_deref()) {
                    self.shared_finish(&operation, Status::Failed, Some("계정이 바뀌었어요. 다시 포크해 주세요.".into()));
                    return;
                }
                match result {
                    Ok(prepared) => self.begin_shared_launch(&operation, prepared),
                    Err(error) => self.shared_finish(&operation, Status::Failed, Some(error)),
                }
            }
            Event::Published { operation, epoch, scope, sequence, result } => {
                if sequence != self.shared_rooms.runtime.publisher.sequence { return; }
                self.shared_rooms.runtime.publisher.syncing = false;
                if !self.shared_same_account(epoch, scope.as_deref()) {
                    self.shared_finish(&operation, Status::Failed, Some("계정이 바뀌었어요. 다시 게시해 주세요.".into()));
                    return;
                }
                match result {
                    Ok(report) => {
                        let error = report.error.clone();
                        self.shared_rooms.runtime.publisher.absorb(report);
                        self.shared_rooms.refreshed = None;
                        match error {
                            Some(error) => self.shared_finish(&operation, Status::Failed, Some(error)),
                            None => self.shared_finish(&operation, Status::Ready, None),
                        }
                    }
                    Err(error) => self.shared_finish(&operation, Status::Failed, Some(error)),
                }
            }
            Event::Synced { epoch, scope, sequence, report } => {
                if sequence != self.shared_rooms.runtime.publisher.sequence { return; }
                if self.shared_same_account(epoch, scope.as_deref()) {
                    self.shared_rooms.runtime.publisher.absorb(report);
                }
                self.shared_rooms.runtime.publisher.syncing = false;
            }
        }
    }

    /// 계정 확인·포크 탭 진행·게시 증분을 한 박자 굴린다. 이벤트 루프가 `shared_local_tick` 시각마다 부른다.
    pub(crate) fn tick_shared_local(&mut self) {
        let epoch = kasa_device::device_auth::change_epoch();
        let scope = self.shared_rooms.client.as_ref().filter(|c| c.is_current()).map(Client::auth_scope);
        let stale: Vec<String> = self.shared_rooms.runtime.operations.iter()
            .filter(|op| !op.status.finished() && (op.epoch != epoch || scope != Some(op.auth_scope.as_str())))
            .map(|op| op.id.clone()).collect();
        for id in stale {
            self.shared_rooms.runtime.launches.retain(|launch| launch.operation != id);
            self.shared_finish(&id, Status::Failed, Some("계정이 바뀌었어요. 다시 시도해 주세요.".into()));
        }
        self.tick_shared_launches();
        self.tick_shared_publisher(epoch);
    }

    pub(crate) fn shared_local_tick(&self) -> Option<Instant> {
        let runtime = &self.shared_rooms.runtime;
        if !runtime.launches.is_empty() {
            return Some(Instant::now() + std::time::Duration::from_millis(40));
        }
        runtime.publisher.next_due()
    }

    fn shared_default_cwd(&self) -> String {
        self.shared_rooms.return_pane().map(str::to_owned).or_else(|| self.active_user_pane())
            .and_then(|pane| self.pane_cwd_cache.get(&pane).cloned())
            .or_else(kasa_socket::home_dir)
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// host 창구 응답(몸통). `ok` 는 HTTP 층이 붙인다. 오류 문장은 폰이 그대로 보여 준다.
    pub(crate) fn shared_host(&mut self, action: &str, body: &serde_json::Value) -> Result<serde_json::Value, String> {
        match action {
            "status" => Ok(serde_json::json!({"available": true, "authenticated": self.shared_rooms.client.as_ref().is_some_and(Client::is_current), "default_cwd": self.shared_default_cwd()})),
            "sources" => Ok(serde_json::json!({
                "sources": self.shared_source_list().iter().map(sources::Source::to_json).collect::<Vec<_>>(),
            })),
            "publish" => {
                let room = body["room_id"].as_str().filter(|r| !r.is_empty() && r.len() <= 128).ok_or("room_id 가 필요해요")?;
                let addresses = body["addresses"].as_array().filter(|a| !a.is_empty() && a.len() <= 16).ok_or("게시할 대화를 골라 주세요")?;
                let current = self.shared_source_list();
                let mut keys = Vec::new();
                for address in addresses {
                    let wanted = kasa_socket::tell::Address::parse(address).map_err(|_| "대화 주소가 완전하지 않아요".to_string())?;
                    let found = current.iter().find(|source| kasa_socket::tell::Address::parse(&source.address).ok().as_ref() == Some(&wanted))
                        .ok_or("고른 대화가 바뀌었어요. 목록을 새로 읽어 주세요")?;
                    keys.push(found.key().to_string());
                }
                let include_project = body["include_project"].as_bool().unwrap_or(false);
                let id = self.publish_shared_sources(room, &keys, include_project)?;
                Ok(self.shared_operation(&id).map(|op| op.to_json()).unwrap_or_default())
            }
            "fork" => {
                let fork_id = body["fork_id"].as_str().filter(|f| f.len() == 32 && f.bytes().all(|b| b.is_ascii_hexdigit())).ok_or("fork_id 가 올바르지 않아요")?;
                // 같은 request_id 의 재시도는 관문이 같은 fork_id 를 돌려주므로 fork_id 로 작업을 하나로 모은다.
                let request_id = body["request_id"].as_str().filter(|r| !r.is_empty() && r.len() <= 128 && !r.chars().any(char::is_control))
                    .ok_or("request_id 가 필요해요")?;
                let destination = &body["destination"];
                if destination["tab"].as_bool() != Some(true) {
                    return Err("새 탭으로 여는 포크만 지원해요".into());
                }
                // 비었으면 대화만 새 작업 폴더에서 연다(프로젝트 포함이면 fork 쪽이 거부한다).
                let cwd = destination["cwd"].as_str().unwrap_or_default().to_string();
                let include_project = destination["include_project"].as_bool().unwrap_or(false);
                let scope = self.shared_rooms.client.as_ref().filter(|c| c.is_current()).ok_or("KASA 계정에 로그인해 주세요")?.scope().to_string();
                let key = (scope, request_id.to_string());
                let signature = serde_json::json!({"fork_id": fork_id, "cwd": cwd.trim(), "include_project": include_project});
                if let Some(previous) = self.shared_rooms.runtime.host_requests.get(&key) {
                    if previous != &signature { return Err("같은 request_id의 포크나 목적지가 달라졌어요".into()); }
                } else if self.shared_rooms.runtime.host_requests.len() >= 1024 {
                    return Err("공유 요청 기록이 가득 찼어요. 기존 요청을 확인해 주세요".into());
                }
                let id = self.shared_fork_operation(fork_id, cwd, include_project)?;
                self.shared_rooms.runtime.host_requests.insert(key, signature);
                Ok(self.shared_operation(&id).map(|op| op.to_json()).unwrap_or_default())
            }
            "operation" => {
                let id = body["operation_id"].as_str().unwrap_or_default();
                self.shared_operation(id).map(|op| op.to_json()).ok_or_else(|| "작업을 찾지 못했어요. 앱이 다시 켜졌을 수 있어요".into())
            }
            _ => Err("모르는 공유방 요청이에요".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn queued_host_request_expires_and_revocation_prevents_side_effects() {
        let current = Arc::new(AtomicBool::new(true));
        let copy = current.clone();
        let client = Client::with_credential(kasa_device::device_auth::DeviceCred {
            relay: "http://127.0.0.1:1".into(), account: "alice".into(), device_id: "fixture".into(),
            token: "fixture".into(), display_name: None,
        }, Arc::new(move || copy.load(Ordering::SeqCst))).unwrap();
        let mut guard = HostGuard { epoch: kasa_device::device_auth::change_epoch(), client: Some(client),
            deadline: Instant::now() + std::time::Duration::from_secs(5) };
        assert!(guard.valid("fork"));
        current.store(false, Ordering::SeqCst);
        assert!(!guard.valid("fork"));
        current.store(true, Ordering::SeqCst);
        guard.deadline = Instant::now();
        assert!(!guard.valid("fork"));
        assert!(!guard.valid("status"));
    }
}
