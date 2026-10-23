pub mod choice;
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
pub(crate) enum SyncError { Unauthorized, Unsupported, Changed, Failed }

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

pub(crate) async fn synchronize(
    client: &reqwest::Client, credential: &DeviceCred, stamp: Stamp, paths: Paths,
    authorize: &(dyn Fn() -> Result<(), String> + Sync),
    apply: &(dyn Fn(PendingApply) -> Result<(), String> + Sync),
) -> Result<(), SyncError> {
    authorize().map_err(|_| SyncError::Changed)?;
    let account = local::account_key(&credential.relay, &credential.account);
    local::note_account(&paths, &account).map_err(|_| SyncError::Failed)?;
    let (original, owner) = {
        let _guard = local::lock_files().map_err(|_| SyncError::Failed)?;
        let settings = local::read_json(&paths.settings, json!({})).map_err(|_| SyncError::Failed)?;
        let machines = local::read_json(&paths.machines, json!([])).map_err(|_| SyncError::Failed)?;
        let owner = settings.get(choice::OWNER_KEY).and_then(Value::as_str).map(str::to_string);
        (local::snapshot_values(&settings, &machines).map_err(|_| SyncError::Failed)?, owner)
    };
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
    // The choice is ordered by the relay, not rebased like a field: see `choice::plan`.
    changes.settings.remove(choice::KEY);
    let local_choice = original.settings.get(choice::KEY).cloned();
    // A choice written under another KASA sign-in is neither sent nor kept here.
    let foreign = local_choice.is_some() && owner.as_deref() != Some(account.as_str());
    let mine = if foreign { None } else { local_choice.clone() };
    let mut choice_allowed = !relay_lacks_opt_in(&credential.relay);
    if !choice_allowed {
        without_opt_in(&mut changes);
    }
    let mut done = false;
    for _ in 0..4 {
        authorize().map_err(|_| SyncError::Changed)?;
        let mut publish = match choice_allowed.then(|| choice::plan(mine.as_ref(), remote.settings.get(choice::KEY))) {
            Some(choice::Plan::Publish(intent)) => Some(intent),
            _ => None,
        };
        // Record the send before it leaves, under the lock a new choice takes, and only if it is still the local
        // value — a choice made from here on lists it in `after`; one made earlier means this one is not sent at all.
        if let Some(intent) = publish.as_ref().and_then(choice::intent).map(str::to_string) {
            let marked = local::edit_json(&paths.settings, json!({}), |settings| {
                if choice::owned(settings, &account).and_then(choice::intent) != Some(intent.as_str()) { return Ok(false); }
                choice::mark_sent(settings, &intent);
                Ok(true)
            }).unwrap_or(false);
            if !marked {
                publish = None;
                choice_allowed = false;
            }
        }
        if changes.settings.is_empty() && changes.machines.is_empty() && publish.is_none() { done = true; break; }
        let mut patch = local::patch_on(&remote, &changes);
        if let Some(intent) = &publish { patch.settings.insert(choice::KEY.into(), intent.clone()); }
        validate_patch(&patch).map_err(|_| SyncError::Failed)?;
        let response = client.patch(format!("{}/relay/account-sync", credential.relay.trim_end_matches('/')))
            .header(schema::KEYS_HEADER, schema::opt_in_header())
            .bearer_auth(&credential.token).json(&patch).send().await.map_err(|_| SyncError::Failed)?;
        let status = response.status().as_u16();
        if status == 400 && (without_opt_in(&mut changes) || publish.is_some()) {
            if let Ok(mut until) = LEGACY_RELAY_UNTIL.lock() {
                *until = Some((credential.relay.clone(), std::time::Instant::now() + Duration::from_secs(600)));
            }
            // The intent stays exactly as it was chosen; it is neither dropped nor re-issued.
            choice_allowed = false;
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
        if status == 409 && seed { done = true; break; }
        if status == 200 { done = true; break; }
    }
    if !done { return Err(SyncError::Failed); }
    authorize().map_err(|_| SyncError::Changed)?;
    let mut expected = local::remote_effect(previous, &original, &remote);
    // An intent the relay has not answered stays local; anything else follows the relay, even when the relay went
    // A→B→A and left the previous value unchanged.
    let settled = match choice_allowed.then(|| choice::plan(mine.as_ref(), remote.settings.get(choice::KEY))) {
        Some(choice::Plan::Adopt(value)) => value,
        _ if foreign => remote.settings.get(choice::KEY).cloned(),
        _ => local_choice.clone(),
    };
    match settled {
        Some(value) => { expected.settings.insert(choice::KEY.into(), value); }
        None => { expected.settings.remove(choice::KEY); }
    }
    apply(PendingApply { stamp, account, paths, original, expected, remote, created: std::time::Instant::now() }).map_err(|_| SyncError::Changed)
}

/// The signed-in KASA account as the local sync files name it — what owns a local choice.
pub fn current_account() -> Option<String> {
    crate::device_auth::current().map(|credential| local::account_key(&credential.relay, &credential.account))
}

/// The snapshot revision this device last observed for the signed-in account — what a new choice has seen.
pub fn seen_revision(paths: &Paths) -> u64 {
    let Some(account) = current_account() else { return 0 };
    let Ok(_guard) = local::lock_files() else { return 0 };
    let remote = local::load_bindings(&paths.binding).ok()
        .and_then(|bindings| bindings.accounts.get(&account).map(|binding| binding.remote.revision))
        .unwrap_or(0);
    // The GUI can have applied an answer before the sync thread saves its binding.
    let settings = local::read_json(&paths.settings, json!({})).ok();
    let chosen = settings.as_ref().and_then(|settings| choice::owned(settings, &account))
        .map_or(0, |value| choice::rev(value).unwrap_or_else(|| choice::base(value)));
    remote.max(chosen)
}

/// Records a person's (or the usage poller's) choice as a new intent in the local settings and wakes the sync.
/// Reading the current value and writing the intent share the lock `apply_files` takes, so an answer landing
/// at the same moment cannot be mistaken for this choice.
/// Writes a choice as a new local intent and wakes the sync. `seen` is the revision observed when the choice was
/// made and `owner` the KASA account signed in then — a choice that waited for its identity proof keeps both and is
/// dropped rather than written under an account signed in later.
pub fn choose_for(paths: &Paths, owner: &str, seen: u64, account_uuid: &str, organization_uuid: &str, email: &str, org: &str) -> Result<Value, String> {
    if current_account().as_deref() != Some(owner) {
        return Err("KASA account changed".into());
    }
    let intent = choose_as(paths, owner, seen, account_uuid, organization_uuid, email, org)?;
    poke();
    Ok(intent)
}

pub(crate) fn choose_as(paths: &Paths, owner: &str, seen: u64, account_uuid: &str, organization_uuid: &str, email: &str, org: &str) -> Result<Value, String> {
    local::edit_json(&paths.settings, json!({}), |settings| {
        let owned = settings.get(choice::OWNER_KEY).and_then(Value::as_str) == Some(owner);
        let sent = if owned { choice::sent(settings) } else { Vec::new() };
        let intent = choice::new_intent(account_uuid, organization_uuid, email, org, seen, &sent)
            .ok_or("account choice invalid")?;
        let object = settings.as_object_mut().ok_or("settings is not an object")?;
        if !owned { object.remove(choice::SENT_KEY); }
        object.insert(choice::KEY.into(), intent.clone());
        object.insert(choice::OWNER_KEY.into(), Value::String(owner.into()));
        Ok(intent)
    })
}

pub fn apply_pending(request: PendingApply) -> Result<(), String> {
    if request.created.elapsed() > Duration::from_secs(5) { return Err("sync response expired".into()); }
    crate::device_auth::with_current(&request.stamp, || apply_files(&request))
}

pub(crate) fn apply_files(request: &PendingApply) -> Result<(), String> {
    let _guard = local::lock_files()?;
    let mut settings = local::read_json(&request.paths.settings, json!({}))?;
    let mut machines = local::read_json(&request.paths.machines, json!([]))?;
    let original_settings = settings.clone();
    let original_machines = machines.clone();
    let mut bindings = local::load_bindings(&request.paths.binding)?;
    local::merge_files(&mut settings, &mut machines, &request.original, &request.expected, &request.account)?;
    // The relay's choice applied here now belongs to this account; a fresh local intent keeps the owner it was written with.
    match settings.get(choice::KEY).cloned() {
        Some(value) if request.expected.settings.get(choice::KEY) == Some(&value) => {
            settings[choice::OWNER_KEY] = Value::String(request.account.clone());
        }
        None => {
            let object = settings.as_object_mut().ok_or("settings must be an object")?;
            object.remove(choice::OWNER_KEY);
            object.remove(choice::SENT_KEY);
        }
        Some(_) => {}
    }
    choice::prune_sent(&mut settings, request.remote.settings.get(choice::KEY));
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
        /// Another device's choice that lands together with the injected conflict.
        rival: Arc<Mutex<Option<Value>>>,
    }

    async fn read(State(state): State<Mock>) -> axum::response::Response {
        if state.failure.load(Ordering::Acquire) { return StatusCode::SERVICE_UNAVAILABLE.into_response(); }
        let snapshot = state.store.get("alice").unwrap();
        if let Some(callback) = state.after_get.lock().unwrap().take() { callback(); }
        Json(snapshot).into_response()
    }

    async fn patch(State(state): State<Mock>, Json(patch): Json<schema::Patch>) -> axum::response::Response {
        if state.legacy.load(Ordering::Acquire) && (patch.settings.contains_key("weather") || patch.settings.contains_key(choice::KEY)) {
            return StatusCode::BAD_REQUEST.into_response();
        }
        if state.conflict.swap(false, Ordering::AcqRel) {
            let current = state.store.get("alice").unwrap();
            let mut settings: std::collections::BTreeMap<String, Value> = [("shape".into(), json!("rounded"))].into();
            if let Some(rival) = state.rival.lock().unwrap().take() { settings.insert(choice::KEY.into(), rival); }
            state.store.patch("alice", &schema::Patch { expected_revision: current.revision, settings, ..Default::default() }).unwrap();
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
            failure: Default::default(), legacy: Default::default(), rival: Default::default() };
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

    const PERSONAL: &str = "00000000-0000-4000-8000-0000000000a1";
    const GMAIL: &str = "00000000-0000-4000-8000-0000000000a2";
    const TEAM: &str = "00000000-0000-4000-8000-0000000000a3";
    const ORG: &str = "00000000-0000-4000-8000-0000000000f0";

    fn seen(paths: &Paths) -> u64 {
        local::load_bindings(&paths.binding).unwrap().accounts.values().next().map_or(0, |b| b.remote.revision)
    }

    fn owner(credential: &DeviceCred) -> String {
        local::account_key(&credential.relay, &credential.account)
    }

    fn choose_at(paths: &Paths, owner: &str, account: &str) -> Value {
        choose_as(paths, owner, seen(paths), account, ORG, "person@example.test", "Shown name").unwrap()
    }

    fn local_choice(paths: &Paths) -> Option<Value> {
        local::read_json(&paths.settings, json!({})).unwrap().get(choice::KEY).cloned()
    }

    fn remote_choice(state: &Mock) -> Option<Value> {
        state.store.get("alice").unwrap().settings.get(choice::KEY).cloned()
    }

    fn intent_of(value: Option<&Value>) -> Option<String> {
        value.and_then(|v| choice::intent(v)).map(str::to_string)
    }

    fn rig(tag: &str) -> (std::path::PathBuf, Paths, Paths) {
        let root = std::env::temp_dir().join(format!("account-sync-{tag}-{}", uuid::Uuid::new_v4()));
        let (a, b) = (paths(&root, "a"), paths(&root, "b"));
        local::write_private(&a.settings, &json!({"theme":"ink","claude_account":"acct-5"})).unwrap();
        local::write_private(&b.settings, &json!({"claude_account":"acct-6"})).unwrap();
        (root, a, b)
    }

    #[tokio::test]
    async fn an_offline_choice_from_revision_ten_loses_to_revision_eleven_after_reconnect_and_reboot() {
        let (credential, state, server) = fixture().await;
        let me = owner(&credential);
        let (root, a, b) = rig("offline");
        let client = reqwest::Client::new();
        let allow = || Ok(());
        let apply = |request| apply_files(&request);
        let sync = |paths| synchronize(&client, &credential, Stamp::fixture(), paths, &allow, &apply);
        sync(a.clone()).await.unwrap();
        sync(b.clone()).await.unwrap();
        for size in 10..19 { // move the relay to revision ten
            local::edit_json(&a.settings, json!({}), |v| { v["font_size"] = json!(size); Ok(()) }).unwrap();
            sync(a.clone()).await.unwrap();
        }
        sync(b.clone()).await.unwrap();
        assert_eq!(seen(&b), 10);
        // B goes offline and chooses; meanwhile A chooses later on the relay (revision eleven).
        let offline = choose_at(&b, &me, GMAIL);
        choose_at(&a, &me, PERSONAL);
        sync(a.clone()).await.unwrap();
        let canonical = remote_choice(&state);
        assert_eq!(canonical.as_ref().and_then(choice::rev), Some(11));
        // Reconnect: the old intent is refused, never reaches the relay, and B follows the canonical choice.
        sync(b.clone()).await.unwrap();
        assert_eq!(intent_of(remote_choice(&state).as_ref()), intent_of(canonical.as_ref()));
        assert_eq!(local_choice(&b), canonical);
        assert_ne!(intent_of(local_choice(&b).as_ref()), intent_of(Some(&offline)));
        // Reboot: a fresh pass over the same files neither revives nor re-sends the dropped intent.
        sync(b.clone()).await.unwrap();
        assert_eq!(local_choice(&b), canonical);
        assert_eq!(state.store.get("alice").unwrap().revision, 11);
        server.abort();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn a_delayed_identity_proof_keeps_its_original_revision_after_a_new_remote_choice() {
        let (credential, state, server) = fixture().await;
        let me = owner(&credential);
        let (root, a, b) = rig("late-proof");
        let client = reqwest::Client::new();
        let allow = || Ok(());
        let apply = |request| apply_files(&request);
        let sync = |paths| synchronize(&client, &credential, Stamp::fixture(), paths, &allow, &apply);
        sync(a.clone()).await.unwrap();
        sync(b.clone()).await.unwrap();
        let chosen_at = seen(&b);
        choose_at(&a, &me, TEAM);
        sync(a.clone()).await.unwrap();
        sync(b.clone()).await.unwrap();
        let canonical = remote_choice(&state);
        assert!(canonical.as_ref().and_then(choice::rev).unwrap() > chosen_at);
        // The application waited for proof while a newer canonical value arrived in its settings.
        let delayed = choose_as(&b, &me, chosen_at, PERSONAL, ORG, "person@example.test", "Shown name").unwrap();
        assert_eq!(choice::base(&delayed), chosen_at);
        sync(b.clone()).await.unwrap();
        assert_eq!(remote_choice(&state), canonical);
        assert_eq!(local_choice(&b), canonical);
        server.abort();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn a_conflict_between_get_and_patch_rebases_settings_but_not_a_stale_choice() {
        let (credential, state, server) = fixture().await;
        let me = owner(&credential);
        let (root, a, b) = rig("conflict");
        let client = reqwest::Client::new();
        let allow = || Ok(());
        let apply = |request| apply_files(&request);
        let sync = |paths| synchronize(&client, &credential, Stamp::fixture(), paths, &allow, &apply);
        sync(a.clone()).await.unwrap();
        sync(b.clone()).await.unwrap();
        let base = seen(&b);
        local::edit_json(&b.settings, json!({}), |v| { v["font_size"] = json!(20); Ok(()) }).unwrap();
        choose_at(&b, &me, GMAIL);
        // Another device's choice commits between B's GET and B's PATCH.
        let rival = choice::new_intent(PERSONAL, ORG, "", "", base, &[]).unwrap();
        *state.rival.lock().unwrap() = Some(rival.clone());
        state.conflict.store(true, Ordering::Release);
        sync(b.clone()).await.unwrap();
        let remote = state.store.get("alice").unwrap();
        assert_eq!(remote.settings["font_size"], 20, "the ordinary setting was not rebased");
        assert_eq!(remote.settings["shape"], "rounded");
        assert_eq!(intent_of(remote_choice(&state).as_ref()), choice::intent(&rival).map(str::to_string));
        assert_eq!(local_choice(&b), remote_choice(&state), "the stale choice survived the rebase");
        sync(b.clone()).await.unwrap();
        assert_eq!(intent_of(remote_choice(&state).as_ref()), choice::intent(&rival).map(str::to_string));
        server.abort();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn a_remote_a_b_a_still_overrides_a_refused_choice_and_no_clock_can_order_it() {
        let (credential, state, server) = fixture().await;
        let me = owner(&credential);
        let (root, a, b) = rig("aba");
        let client = reqwest::Client::new();
        let allow = || Ok(());
        let apply = |request| apply_files(&request);
        let sync = |paths| synchronize(&client, &credential, Stamp::fixture(), paths, &allow, &apply);
        choose_at(&a, &me, PERSONAL);
        sync(a.clone()).await.unwrap();
        sync(b.clone()).await.unwrap();
        let first = local_choice(&b);
        // B chooses while offline; A goes PERSONAL → TEAM → PERSONAL in the meantime.
        choose_at(&b, &me, GMAIL);
        for account in [TEAM, PERSONAL] {
            choose_at(&a, &me, account);
            sync(a.clone()).await.unwrap();
        }
        let back = remote_choice(&state);
        assert!(choice::same_identity(back.as_ref().unwrap(), first.as_ref().unwrap()));
        sync(b.clone()).await.unwrap();
        assert_eq!(local_choice(&b), back, "B kept its refused choice after the relay came back to the same account");
        // A value carrying a wall-clock order (even a far-future one) is not a choice at all and is never sent.
        let mut clocked = choice::new_intent(GMAIL, ORG, "", "", seen(&b), &[]).unwrap();
        clocked["at"] = json!(u64::MAX);
        local::edit_json(&b.settings, json!({}), |v| { v[choice::KEY] = clocked; Ok(()) }).unwrap();
        sync(b.clone()).await.unwrap();
        assert_eq!(intent_of(remote_choice(&state).as_ref()), intent_of(back.as_ref()));
        server.abort();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn a_late_answer_keeps_a_newer_manual_choice_and_an_old_auto_decision_loses() {
        let (credential, state, server) = fixture().await;
        let me = owner(&credential);
        let (root, a, _) = rig("late-ack");
        let client = reqwest::Client::new();
        let allow = || Ok(());
        let plain = |request: PendingApply| apply_files(&request);
        synchronize(&client, &credential, Stamp::fixture(), a.clone(), &allow, &plain).await.unwrap();
        let first = choose_at(&a, &me, PERSONAL);
        // The person chooses again (the same account!) after the relay answered the first intent but before the answer applies.
        let newer = Mutex::new(None);
        let manual = |request: PendingApply| {
            *newer.lock().unwrap() = Some(choose_at(&a, &me, PERSONAL));
            apply_files(&request)
        };
        synchronize(&client, &credential, Stamp::fixture(), a.clone(), &allow, &manual).await.unwrap();
        let newer = newer.into_inner().unwrap().unwrap();
        assert_eq!(intent_of(remote_choice(&state).as_ref()), choice::intent(&first).map(str::to_string));
        assert_eq!(local_choice(&a).as_ref(), Some(&newer), "the late answer erased the newer choice");
        assert_eq!(newer["after"], json!([first["intent"]]));
        synchronize(&client, &credential, Stamp::fixture(), a.clone(), &allow, &plain).await.unwrap();
        assert_eq!(intent_of(remote_choice(&state).as_ref()), choice::intent(&newer).map(str::to_string));
        let canonical = remote_choice(&state);
        // An automatic decision made from an older view (base before the newer choice, not chained) is refused.
        let old_auto = choice::new_intent(GMAIL, ORG, "", "", choice::base(&first), &[]).unwrap();
        local::edit_json(&a.settings, json!({}), |v| { v[choice::KEY] = old_auto; Ok(()) }).unwrap();
        synchronize(&client, &credential, Stamp::fixture(), a.clone(), &allow, &plain).await.unwrap();
        assert_eq!(remote_choice(&state), canonical);
        assert_eq!(local_choice(&a), canonical);
        server.abort();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn first_binding_and_an_older_relay_never_turn_an_existing_selection_into_a_new_choice() {
        let (credential, state, server) = fixture().await;
        let me = owner(&credential);
        let (root, a, b) = rig("migration");
        let client = reqwest::Client::new();
        let allow = || Ok(());
        let apply = |request| apply_files(&request);
        let sync = |paths| synchronize(&client, &credential, Stamp::fixture(), paths, &allow, &apply);
        // Upgrading devices publish nothing for their current slot, whichever boots first.
        sync(a.clone()).await.unwrap();
        sync(b.clone()).await.unwrap();
        assert!(remote_choice(&state).is_none() && local_choice(&a).is_none() && local_choice(&b).is_none());

        // An older relay refuses the key: the intent stays as chosen (same id and base), the rest still syncs.
        state.legacy.store(true, Ordering::Release);
        let pending = choose_at(&a, &me, GMAIL);
        local::edit_json(&a.settings, json!({}), |v| { v["font_size"] = json!(15); Ok(()) }).unwrap();
        sync(a.clone()).await.unwrap();
        sync(a.clone()).await.unwrap();
        assert_eq!(state.store.get("alice").unwrap().settings["font_size"], 15);
        assert!(remote_choice(&state).is_none());
        assert_eq!(local_choice(&a).as_ref(), Some(&pending), "the intent was dropped or re-issued");
        server.abort();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn ten_choices_while_the_first_is_in_flight_and_a_same_account_reselect_keep_the_last() {
        let (credential, state, server) = fixture().await;
        let me = owner(&credential);
        let (root, a, _) = rig("chain");
        let client = reqwest::Client::new();
        let allow = || Ok(());
        let plain = |request: PendingApply| apply_files(&request);
        synchronize(&client, &credential, Stamp::fixture(), a.clone(), &allow, &plain).await.unwrap();
        choose_at(&a, &me, PERSONAL);
        // While the first intent is on the wire the person picks twice more, ending on the same account again.
        let last = Mutex::new(None);
        let twice = |request: PendingApply| {
            for n in 0..9 { choose_at(&a, &me, if n % 2 == 0 { GMAIL } else { TEAM }); }
            *last.lock().unwrap() = Some(choose_at(&a, &me, PERSONAL));
            apply_files(&request)
        };
        synchronize(&client, &credential, Stamp::fixture(), a.clone(), &allow, &twice).await.unwrap();
        let last = last.into_inner().unwrap().unwrap();
        assert_eq!(local_choice(&a).as_ref(), Some(&last));
        synchronize(&client, &credential, Stamp::fixture(), a.clone(), &allow, &plain).await.unwrap();
        assert_eq!(intent_of(remote_choice(&state).as_ref()), choice::intent(&last).map(str::to_string), "the last choice was dropped");
        assert_eq!(intent_of(local_choice(&a).as_ref()), choice::intent(&last).map(str::to_string));
        server.abort();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn a_choice_left_by_another_kasa_sign_in_is_neither_sent_nor_followed() {
        let (credential, state, server) = fixture().await;
        let (root, a, _) = rig("owner");
        let client = reqwest::Client::new();
        let allow = || Ok(());
        let plain = |request: PendingApply| apply_files(&request);
        synchronize(&client, &credential, Stamp::fixture(), a.clone(), &allow, &plain).await.unwrap();
        // An intent written while another KASA account was signed in on this device.
        let other = local::account_key(&credential.relay, "someone-else");
        choose_at(&a, &other, GMAIL);
        synchronize(&client, &credential, Stamp::fixture(), a.clone(), &allow, &plain).await.unwrap();
        assert!(remote_choice(&state).is_none(), "another sign-in's choice was published");
        assert!(local_choice(&a).is_none(), "another sign-in's choice was kept");
        let settings = local::read_json(&a.settings, json!({})).unwrap();
        assert!(settings.get(choice::OWNER_KEY).is_none());
        // The account's own choice is published, applied back, and owned by it.
        let me = owner(&credential);
        let mine = choose_at(&a, &me, PERSONAL);
        synchronize(&client, &credential, Stamp::fixture(), a.clone(), &allow, &plain).await.unwrap();
        assert_eq!(intent_of(remote_choice(&state).as_ref()), choice::intent(&mine).map(str::to_string));
        let settings = local::read_json(&a.settings, json!({})).unwrap();
        assert_eq!(settings[choice::OWNER_KEY], me);
        assert!(choice::owned(&settings, &other).is_none());
        server.abort();
        std::fs::remove_dir_all(root).unwrap();
    }
}
