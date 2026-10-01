pub mod schema;
pub mod server;
pub mod local;

use std::sync::{Arc, Mutex, OnceLock};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{json, Value};
use crate::device_auth::{DeviceCred, Stamp};
use local::{Binding, Paths};
use schema::{Snapshot, validate_patch, validate_snapshot, MAX_BODY};

#[derive(Clone, Debug)]
pub struct PendingApply {
    stamp: Stamp,
    account: String,
    paths: Paths,
    original: Snapshot,
    expected: Snapshot,
    remote: Snapshot,
    created: std::time::Instant,
}

impl PendingApply {
    pub fn changes_runtime(&self) -> bool {
        self.original.settings != self.expected.settings || self.original.machines != self.expected.machines
    }

    pub fn changes_setting(&self, key: &str) -> bool {
        self.original.settings.get(key) != self.expected.settings.get(key)
    }
}

type ApplyHook = Arc<dyn Fn(PendingApply) -> Result<(), String> + Send + Sync>;
static APPLY: OnceLock<ApplyHook> = OnceLock::new();

pub fn set_apply_hook(hook: ApplyHook) {
    let _ = APPLY.set(hook);
    spawn();
}

fn notify() -> &'static tokio::sync::Notify {
    static NOTIFY: OnceLock<tokio::sync::Notify> = OnceLock::new();
    NOTIFY.get_or_init(tokio::sync::Notify::new)
}

pub fn poke() { notify().notify_one(); }

fn sync_status() -> &'static Mutex<(String, Value)> {
    static STATUS: OnceLock<Mutex<(String, Value)>> = OnceLock::new();
    STATUS.get_or_init(|| Mutex::new((String::new(), json!({"state":"signed_out"}))))
}

fn set_status(account: &str, state: &str, message: &str) {
    if let Ok(mut status) = sync_status().lock() {
        *status = (account.into(), json!({"state":state,"message":message}));
    }
}

pub fn status() -> Value {
    let Some(credential) = crate::device_auth::current() else { return json!({"state":"signed_out"}); };
    let account = local::account_key(&credential.relay, &credential.account);
    sync_status().lock().ok().filter(|status| status.0 == account).map(|status| status.1.clone())
        .unwrap_or_else(|| json!({"state":"checking","message":"공통 설정 동기화 확인 중"}))
}

pub(crate) fn credentials_changed(previous: Option<&DeviceCred>, next: Option<&DeviceCred>) {
    let Some(paths) = Paths::current() else { return };
    if let Some(first) = previous.or(next) {
        let key = local::account_key(&first.relay, &first.account);
        if local::note_account(&paths, &key).is_err() {
            set_status(&key, "error", "계정 동기화 기록을 읽지 못했어요. 기존 설정을 유지합니다");
        }
    }
    poke();
}

pub fn spawn() {
    // A sidecar must not race the GUI's in-memory settings with another file writer.
    if cfg!(test) || APPLY.get().is_none() { return; }
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::AcqRel) { return; }
    if std::env::var_os("KASATERM_SETTINGS_FILE").is_some()
        && (std::env::var_os("KASATERM_DEVICE_FILE").is_none() || std::env::var_os("KASATERM_MACHINES_FILE").is_none()) {
        return;
    }
    std::thread::spawn(|| {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
        runtime.block_on(async {
        let client = match reqwest::Client::builder().timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none()).build() {
            Ok(client) => client,
            Err(_) => return,
        };
        loop {
            if let (Some((credential, stamp)), Some(paths)) = (crate::device_auth::capture(), Paths::current()) {
                let key = local::account_key(&credential.relay, &credential.account);
                let result = synchronize(&client, &credential, stamp.clone(), paths,
                    &|| crate::device_auth::with_current(&stamp, || Ok(())),
                    &|request| if let Some(hook) = APPLY.get() { hook(request) } else { apply_pending(request) }).await;
                if crate::device_auth::with_current(&stamp, || Ok(())).is_ok() {
                    match result {
                        Ok(()) => set_status(&key, "synced", "공통 설정과 기기 연결 목록을 동기화했어요"),
                        Err(SyncError::Unauthorized) => {
                            crate::device_auth::reject(&stamp);
                            set_status(&key, "reauth_required", "다시 로그인해야 공통 설정을 동기화할 수 있어요");
                        }
                        Err(SyncError::Unsupported) => set_status(&key, "unsupported", "이 서버는 계정 동기화를 아직 지원하지 않아요"),
                        Err(SyncError::Changed) => {}
                        Err(SyncError::Failed) => set_status(&key, "error", "동기화하지 못했어요. 기존 설정과 연결은 유지합니다"),
                    }
                }
            }
            tokio::select! {
                _ = notify().notified() => {}
                _ = tokio::time::sleep(Duration::from_secs(15)) => {}
            }
        }
        });
    });
}

#[derive(Debug, PartialEq, Eq)]
enum SyncError { Unauthorized, Unsupported, Changed, Failed }

async fn response_json(mut response: reqwest::Response) -> Result<Value, SyncError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| SyncError::Failed)? {
        if bytes.len() + chunk.len() > MAX_BODY + 1024 { return Err(SyncError::Failed); }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| SyncError::Failed)
}

fn response_status(status: u16) -> Result<(), SyncError> {
    match status {
        200 | 409 => Ok(()),
        401 | 403 => Err(SyncError::Unauthorized),
        404 | 405 => Err(SyncError::Unsupported),
        _ => Err(SyncError::Failed),
    }
}

async fn get_remote(client: &reqwest::Client, credential: &DeviceCred) -> Result<Snapshot, SyncError> {
    let response = client.get(format!("{}/relay/account-sync", credential.relay.trim_end_matches('/')))
        .header(schema::KEYS_HEADER, schema::opt_in_header())
        .bearer_auth(&credential.token).send().await.map_err(|_| SyncError::Failed)?;
    response_status(response.status().as_u16())?;
    let snapshot: Snapshot = serde_json::from_value(response_json(response).await?).map_err(|_| SyncError::Failed)?;
    let snapshot = schema::known_only(snapshot);
    validate_snapshot(&snapshot).map_err(|_| SyncError::Failed)?;
    Ok(snapshot)
}

/// Set when the relay refused a patch that carried an opt-in key: it predates that key.
/// Those keys then stay local for a while instead of failing every sync pass. Kept per
/// relay, so signing in to a newer relay does not inherit an older one's verdict.
static LEGACY_RELAY_UNTIL: Mutex<Option<(String, std::time::Instant)>> = Mutex::new(None);

fn relay_lacks_opt_in(relay: &str) -> bool {
    LEGACY_RELAY_UNTIL.lock().ok().and_then(|g| g.clone())
        .is_some_and(|(legacy, until)| legacy == relay && std::time::Instant::now() < until)
}

fn without_opt_in(changes: &mut local::Delta) -> bool {
    let before = changes.settings.len();
    changes.settings.retain(|key, _| !schema::OPT_IN_KEYS.contains(&key.as_str()));
    changes.settings.len() != before
}

async fn synchronize(
    client: &reqwest::Client, credential: &DeviceCred, stamp: Stamp, paths: Paths,
    authorize: &(dyn Fn() -> Result<(), String> + Sync),
    apply: &(dyn Fn(PendingApply) -> Result<(), String> + Sync),
) -> Result<(), SyncError> {
    authorize().map_err(|_| SyncError::Changed)?;
    let account = local::account_key(&credential.relay, &credential.account);
    local::note_account(&paths, &account).map_err(|_| SyncError::Failed)?;
    let original = local::read_snapshot(&paths).map_err(|_| SyncError::Failed)?;
    let bindings = local::load_bindings(&paths.binding).map_err(|_| SyncError::Failed)?;
    let previous = bindings.accounts.get(&account);
    let mut remote = get_remote(client, credential).await?;
    if previous.is_some_and(|binding| remote.revision < binding.remote.revision
        || (remote.revision == binding.remote.revision && remote != binding.remote)) {
        return Err(SyncError::Failed);
    }
    authorize().map_err(|_| SyncError::Changed)?;
    let seed = previous.is_none() && bindings.first_account.as_deref() == Some(account.as_str()) && remote.revision == 0;
    let mut changes = if let Some(binding) = previous { local::delta(&binding.observed, &original) }
        else if seed { local::delta(&Snapshot::default(), &original) }
        else { local::Delta::default() };
    if relay_lacks_opt_in(&credential.relay) {
        without_opt_in(&mut changes);
    }
    if !changes.settings.is_empty() || !changes.machines.is_empty() {
        let mut committed = false;
        for _ in 0..4 {
            authorize().map_err(|_| SyncError::Changed)?;
            let patch = local::patch_on(&remote, &changes);
            validate_patch(&patch).map_err(|_| SyncError::Failed)?;
            let response = client.patch(format!("{}/relay/account-sync", credential.relay.trim_end_matches('/')))
                .header(schema::KEYS_HEADER, schema::opt_in_header())
                .bearer_auth(&credential.token).json(&patch).send().await.map_err(|_| SyncError::Failed)?;
            let status = response.status().as_u16();
            if status == 400 && without_opt_in(&mut changes) {
                if let Ok(mut until) = LEGACY_RELAY_UNTIL.lock() {
                    *until = Some((credential.relay.clone(), std::time::Instant::now() + Duration::from_secs(600)));
                }
                if changes.settings.is_empty() && changes.machines.is_empty() { committed = true; break; }
                continue;
            }
            response_status(status)?;
            let value = response_json(response).await?;
            remote = serde_json::from_value(if status == 409 { value.get("current").cloned().ok_or(SyncError::Failed)? } else { value })
                .map(schema::known_only)
                .map_err(|_| SyncError::Failed)?;
            validate_snapshot(&remote).map_err(|_| SyncError::Failed)?;
            authorize().map_err(|_| SyncError::Changed)?;
            // Another device owns initialization once its first snapshot has committed.
            if status == 409 && seed { committed = true; break; }
            if status == 200 { committed = true; break; }
        }
        if !committed { return Err(SyncError::Failed); }
    }
    authorize().map_err(|_| SyncError::Changed)?;
    let expected = local::remote_effect(previous, &original, &remote);
    apply(PendingApply { stamp, account, paths, original, expected, remote, created: std::time::Instant::now() }).map_err(|_| SyncError::Changed)
}

pub fn apply_pending(request: PendingApply) -> Result<(), String> {
    if request.created.elapsed() > Duration::from_secs(5) { return Err("sync response expired".into()); }
    crate::device_auth::with_current(&request.stamp, || apply_files(&request))
}

fn apply_files(request: &PendingApply) -> Result<(), String> {
    let _guard = local::lock_files()?;
    let mut settings = local::read_json(&request.paths.settings, json!({}))?;
    let mut machines = local::read_json(&request.paths.machines, json!([]))?;
    let original_settings = settings.clone();
    let original_machines = machines.clone();
    let mut bindings = local::load_bindings(&request.paths.binding)?;
    local::merge_files(&mut settings, &mut machines, &request.original, &request.expected, &request.account)?;
    if settings != original_settings { local::write_private(&request.paths.settings, &settings)?; }
    if machines != original_machines { local::write_private(&request.paths.machines, &machines)?; }
    let actual = local::snapshot_values(&settings, &machines)?;
    let mut observed = request.expected.clone();
    for (id, value) in &request.original.machines {
        if !observed.machines.contains_key(id) && actual.machines.get(id) == Some(value) {
            observed.machines.insert(id.clone(), value.clone());
        }
    }
    bindings.accounts.insert(request.account.clone(), Binding { remote: request.remote.clone(), observed });
    local::write_private(&request.paths.binding, &serde_json::to_value(bindings).map_err(|_| "binding invalid")?)?;
    crate::machines::poke();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::get, Json, Router};

    #[derive(Clone)]
    struct Mock {
        store: Arc<server::Store>,
        conflict: Arc<AtomicBool>,
        after_get: Arc<Mutex<Option<Arc<dyn Fn() + Send + Sync>>>>,
        failure: Arc<AtomicBool>,
        legacy: Arc<AtomicBool>,
    }

    async fn read(State(state): State<Mock>) -> axum::response::Response {
        if state.failure.load(Ordering::Acquire) { return StatusCode::SERVICE_UNAVAILABLE.into_response(); }
        let snapshot = state.store.get("alice").unwrap();
        if let Some(callback) = state.after_get.lock().unwrap().take() { callback(); }
        Json(snapshot).into_response()
    }

    async fn patch(State(state): State<Mock>, Json(patch): Json<schema::Patch>) -> axum::response::Response {
        if state.legacy.load(Ordering::Acquire) && patch.settings.contains_key("weather") {
            return StatusCode::BAD_REQUEST.into_response();
        }
        if state.conflict.swap(false, Ordering::AcqRel) {
            let current = state.store.get("alice").unwrap();
            state.store.patch("alice", &schema::Patch { expected_revision: current.revision,
                settings: [("shape".into(), json!("rounded"))].into(), ..Default::default() }).unwrap();
        }
        match state.store.patch("alice", &patch) {
            Ok(snapshot) => Json(snapshot).into_response(),
            Err(server::Error::Conflict(current)) => (StatusCode::CONFLICT, Json(json!({"error":"revision_conflict","current":current}))).into_response(),
            _ => StatusCode::BAD_REQUEST.into_response(),
        }
    }

    fn paths(root: &std::path::Path, device: &str) -> Paths {
        Paths { settings: root.join(device).join("settings.json"), machines: root.join(device).join("machines.json"), binding: root.join(device).join("binding.json") }
    }

    async fn fixture() -> (DeviceCred, Mock, tokio::task::JoinHandle<()>) {
        let state = Mock { store: Arc::new(server::Store::new(None)), conflict: Default::default(), after_get: Default::default(),
            failure: Default::default(), legacy: Default::default() };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let relay = format!("http://{}", listener.local_addr().unwrap());
        let router = Router::new().route("/relay/account-sync", get(read).patch(patch)).with_state(state.clone());
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap(); });
        (DeviceCred { relay, account: "alice".into(), device_id: "test-device".into(), token: "fixture-only".into(), display_name: None }, state, task)
    }

    #[tokio::test]
    async fn empty_account_seed_two_devices_cas_rebase_and_errors_preserve_local_data() {
        let (credential, state, server) = fixture().await;
        let root = std::env::temp_dir().join(format!("account-sync-roundtrip-{}", uuid::Uuid::new_v4()));
        let a = paths(&root, "one");
        let b = paths(&root, "two");
        local::write_private(&a.settings, &json!({"theme":"graphite","font_size":12,"password":"private","update_channel":"preview"})).unwrap();
        local::write_private(&a.machines, &json!([{"label":"Mini","ssh":"user@mini.example","key":"/private/key","roots":{"/one":"/two"}}])).unwrap();
        local::write_private(&b.settings, &json!({"font_size":10,"default_cwd":"/local"})).unwrap();
        let client = reqwest::Client::new();
        let allow = || Ok(());
        let apply = |request| apply_files(&request);
        let sync = |paths| synchronize(&client, &credential, Stamp::fixture(), paths, &allow, &apply);
        sync(a.clone()).await.unwrap();
        let remote = state.store.get("alice").unwrap();
        assert_eq!(remote.settings["theme"], "graphite");
        assert!(!serde_json::to_string(&remote).unwrap().contains("private"));
        sync(b.clone()).await.unwrap();
        let imported = local::read_json(&b.machines, json!([])).unwrap();
        assert_eq!(imported[0]["ssh"], "ssh://user@mini.example:22");
        assert!(imported[0].get("key").is_none());
        assert_eq!(local::read_json(&b.settings, json!({})).unwrap()["default_cwd"], "/local");
        local::edit_json(&a.settings, json!({}), |value| { value["font_size"] = json!(16); Ok(()) }).unwrap();
        state.conflict.store(true, Ordering::Release);
        sync(a.clone()).await.unwrap();
        assert_eq!(state.store.get("alice").unwrap().settings["shape"], "rounded");
        sync(b.clone()).await.unwrap();
        assert_eq!(local::read_json(&b.settings, json!({})).unwrap()["font_size"], 16);
        let before = std::fs::read(&b.settings).unwrap();
        state.failure.store(true, Ordering::Release);
        assert_eq!(sync(b.clone()).await, Err(SyncError::Failed));
        assert_eq!(std::fs::read(&b.settings).unwrap(), before);
        server.abort();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn a_relay_older_than_a_key_keeps_it_local_and_syncs_the_rest() {
        let (credential, state, server) = fixture().await;
        state.legacy.store(true, Ordering::Release);
        let root = std::env::temp_dir().join(format!("account-sync-legacy-{}", uuid::Uuid::new_v4()));
        let paths = paths(&root, "device");
        local::write_private(&paths.settings, &json!({"theme":"ink","weather":{"enabled":true,"amount":"rain"}})).unwrap();
        synchronize(&reqwest::Client::new(), &credential, Stamp::fixture(), paths.clone(),
            &|| Ok(()), &|request| apply_files(&request)).await.unwrap();
        let remote = state.store.get("alice").unwrap();
        assert_eq!(remote.settings.get("theme"), Some(&json!("ink")));
        assert!(!remote.settings.contains_key("weather"));
        assert_eq!(local::read_json(&paths.settings, json!({})).unwrap()["weather"]["amount"], "rain");
        server.abort();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn student_picks_follow_the_account_both_ways() {
        let (credential, _, server) = fixture().await;
        let root = std::env::temp_dir().join(format!("account-sync-picks-{}", uuid::Uuid::new_v4()));
        let a = paths(&root, "one");
        let b = paths(&root, "two");
        let picks = json!({"__base":["아즈사","미도리"], "project-sekai":["에무"]});
        local::write_private(&a.settings, &json!({"character_theme":"project-sekai","character_picks":picks})).unwrap();
        local::write_private(&b.settings, &json!({"character_picks":{"__base":["아로나"]}})).unwrap();
        let client = reqwest::Client::new();
        let allow = || Ok(());
        let changed = Mutex::new(Vec::new());
        let apply = |request: PendingApply| {
            changed.lock().unwrap().push(request.changes_setting("character_picks"));
            apply_files(&request)
        };
        let sync = |paths| synchronize(&client, &credential, Stamp::fixture(), paths, &allow, &apply);
        sync(a.clone()).await.unwrap();
        sync(b.clone()).await.unwrap();
        let received = local::read_json(&b.settings, json!({})).unwrap();
        assert_eq!(received["character_picks"], picks, "order is the assignment order and must survive");
        assert_eq!(received["character_theme"], "project-sekai");
        assert_eq!(*changed.lock().unwrap(), [false, true]);
        local::edit_json(&b.settings, json!({}), |value| { value["character_picks"] = json!({"__base":["미도리"]}); Ok(()) }).unwrap();
        sync(b.clone()).await.unwrap();
        sync(a.clone()).await.unwrap();
        assert_eq!(local::read_json(&a.settings, json!({})).unwrap()["character_picks"], json!({"__base":["미도리"]}));
        server.abort();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn simultaneous_initial_seed_keeps_the_first_devices_snapshot() {
        let (credential, state, server) = fixture().await;
        let root = std::env::temp_dir().join(format!("account-sync-seed-{}", uuid::Uuid::new_v4()));
        let paths = paths(&root, "second");
        local::write_private(&paths.settings, &json!({"theme":"ink", "font_size":12})).unwrap();
        let store = state.store.clone();
        *state.after_get.lock().unwrap() = Some(Arc::new(move || {
            store.patch("alice", &schema::Patch { settings: [("theme".into(), json!("graphite"))].into(),
                ..Default::default() }).unwrap();
        }));
        synchronize(&reqwest::Client::new(), &credential, Stamp::fixture(), paths.clone(),
            &|| Ok(()), &|request| apply_files(&request)).await.unwrap();
        let remote = state.store.get("alice").unwrap();
        assert_eq!(remote.revision, 1);
        assert_eq!(remote.settings, [("theme".into(), json!("graphite"))].into());
        assert_eq!(local::read_json(&paths.settings, json!({})).unwrap()["theme"], "graphite");
        server.abort();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn switching_accounts_never_seeds_old_values_and_late_response_is_rejected() {
        let (credential, state, server) = fixture().await;
        let root = std::env::temp_dir().join(format!("account-sync-switch-{}", uuid::Uuid::new_v4()));
        let paths = paths(&root, "device");
        local::write_private(&paths.settings, &json!({"theme":"ink","font_size":14})).unwrap();
        local::note_account(&paths, &local::account_key(&credential.relay, "previous-account")).unwrap();
        let client = reqwest::Client::new();
        synchronize(&client, &credential, Stamp::fixture(), paths.clone(), &|| Ok(()), &|request| apply_files(&request)).await.unwrap();
        assert_eq!(state.store.get("alice").unwrap(), Snapshot::default());
        let before = std::fs::read(&paths.settings).unwrap();
        let current = Arc::new(AtomicBool::new(true));
        let changed = current.clone();
        *state.after_get.lock().unwrap() = Some(Arc::new(move || { changed.store(false, Ordering::Release); }));
        let applied = AtomicBool::new(false);
        let result = synchronize(&client, &credential, Stamp::fixture(), paths.clone(),
            &|| if current.load(Ordering::Acquire) { Ok(()) } else { Err("account changed".into()) },
            &|_| { applied.store(true, Ordering::Release); Ok(()) }).await;
        assert_eq!(result, Err(SyncError::Changed));
        assert!(!applied.load(Ordering::Acquire));
        assert_eq!(std::fs::read(&paths.settings).unwrap(), before);
        server.abort();
        std::fs::remove_dir_all(root).unwrap();
    }
}
