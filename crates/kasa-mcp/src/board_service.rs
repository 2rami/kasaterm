//! One local observer plus an independent remote I/O lane; readers only copy cache.
use anyhow::{bail, Context, Result};
use kasa_socket::{
    backend::Backend,
    board::{self, field, BoardStore},
};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

struct Service {
    machine_id: String,
    configured_machines: Option<Vec<crate::machines::Machine>>,
    store: Mutex<BoardStore>,
    backend: Weak<dyn Backend>,
    routes: Mutex<HashMap<String, crate::machines::Machine>>,
    aliases: Mutex<HashMap<String, String>>,
    stop: AtomicBool,
    source_override: Option<Value>,
}

pub struct CollectorConfig {
    pub machine_id: String,
    pub label: String,
    pub journal_path: Option<PathBuf>,
    pub remote_enabled: bool,
    pub source_override: Option<Value>,
    pub machines: Option<Vec<crate::machines::Machine>>,
}

impl Service {
    fn machines(&self) -> Vec<crate::machines::Machine> {
        if let Some(fixed) = self.configured_machines.clone() {
            return fixed;
        }
        let mut out = crate::machines::machines();
        // 직통 길과 나란히 **관문 우회 길**도 세운다 — 기계 id 를 아는 기계마다 하나. 넷버드·
        // 터널이 죽어도 폴링이 우회로 관측을 잇고, `verified_route` 는 직통이 살아 있는 한
        // 직통을 고른다(하이브리드, 2026-09-16 지시). id 는 명부 값이거나 지난 폴링이 그
        // base 에서 본 것(`aliases`).
        let aliases = self.aliases.lock().map(|a| a.clone()).unwrap_or_default();
        let mut relays = Vec::new();
        for m in &out {
            let Some(id) = m
                .machine_id
                .clone()
                .or_else(|| aliases.get(&m.base).cloned())
                .filter(|id| !id.is_empty() && *id != self.machine_id && !id.starts_with("unresolved:"))
            else {
                continue;
            };
            let Some(base) = relay_base(&id) else { break };
            if out.iter().any(|x| x.base == base) || relays.iter().any(|x: &crate::machines::Machine| x.base == base) {
                continue;
            }
            relays.push(crate::machines::Machine { base, machine_id: Some(id), ..m.clone() });
        }
        out.extend(relays);
        out
    }
}

/// 관문을 거치는 우회 base — 직통 길이 없을 때의 폴백. 관문의 `/u/<slug>/m/~<id>/…` 가 그
/// 기계의 업링크로 흘리므로 이 뒤에 `/collab/…` 를 그대로 붙이면 된다. slug 는 이 기계 주인의
/// 폰 주소(자격)라 토큰이 따로 없다. 관문이 꺼져 있거나 주인 유저가 없으면 None.
pub fn relay_base(machine_id: &str) -> Option<String> {
    let gateway = crate::mobile::gateway()?;
    let slug = crate::mobile::owner()?.slug;
    Some(format!("{}/u/{slug}/m/~{machine_id}", gateway.trim_end_matches('/')))
}

pub fn is_relay_base(base: &str) -> bool {
    base.contains("/u/") && base.contains("/m/~")
}

fn isolated() -> bool {
    kasa_socket::isolated_collab_root().is_some()
        || std::env::var_os("KASATERM_TEST_BOARD_FIXTURE").is_some()
}

fn fixture_root() -> Option<PathBuf> {
    if !cfg!(debug_assertions) || std::env::var("KASATERM_AUTORESTORE").as_deref() != Ok("fresh") {
        return None;
    }
    let root = PathBuf::from(std::env::var_os("TMPDIR")?);
    if ![
        "/tmp",
        "/private/tmp",
        "/var/folders",
        "/private/var/folders",
    ]
    .iter()
    .any(|base| root.starts_with(base))
    {
        return None;
    }
    if !root.is_absolute()
        || !root
            .file_name()?
            .to_string_lossy()
            .starts_with("kasaterm-board-")
    {
        return None;
    }
    for name in [
        "KASATERM_SETTINGS_FILE",
        "KASATERM_SESSION_FILE",
        "KASATERM_SOCKET_PATH",
        "KASATERM_WINDOW_FILE",
        "KASATERM_VIEWER_STATE_FILE",
    ] {
        let path = PathBuf::from(std::env::var_os(name)?);
        if !path.starts_with(&root)
            || path == root
            || path
                .components()
                .any(|c| c == std::path::Component::ParentDir)
        {
            return None;
        }
    }
    Some(root)
}

fn slot() -> &'static Mutex<Weak<Service>> {
    static SLOT: OnceLock<Mutex<Weak<Service>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(Weak::new()))
}

pub struct CollectorGuard(Arc<Service>);
impl Drop for CollectorGuard {
    fn drop(&mut self) {
        self.0.stop.store(true, Ordering::Release);
    }
}

pub fn instance_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| uuid::Uuid::new_v4().to_string())
}

pub fn local_id() -> Result<String> {
    if isolated() {
        static VERIFY: OnceLock<String> = OnceLock::new();
        return Ok(VERIFY
            .get_or_init(|| format!("verify-{}", uuid::Uuid::new_v4()))
            .clone());
    }
    static ID: OnceLock<String> = OnceLock::new();
    if let Some(id) = ID.get() {
        return Ok(id.clone());
    }
    let id =
        crate::mobile::machine_identity().context("persistent machine identity unavailable")?;
    let _ = ID.set(id.clone());
    Ok(id)
}

pub fn address(surface: &str, session: Option<&str>) -> Result<Value> {
    let mut address = json!({"machine_id":local_id()?,"surface_key":crate::surface_keys::ensure(surface),
        "surface_id":surface,"instance_id":instance_id()});
    if let Some(session) = session.filter(|s| !s.is_empty()) {
        address["session_id"] = json!(session);
    }
    Ok(address)
}

pub fn local_source(panes: Vec<Value>, complete: bool) -> Result<Value> {
    Ok(
        json!({"machine_id":local_id()?,"label":if isolated() {"Verification".into()} else {crate::machines::self_label()},
        "state":"online","observed_at_ms":board::now_ms(),"complete":complete,"panes":panes}),
    )
}

fn journal_path(port: u16) -> Option<PathBuf> {
    let root = if let Some(root) = kasa_socket::isolated_collab_root() {
        root
    } else if let Some(path) = std::env::var_os("KASATERM_SESSION_FILE").filter(|s| !s.is_empty()) {
        PathBuf::from(path).parent()?.to_path_buf()
    } else {
        let root = kasa_socket::home_dir()?.join(".config/kasaterm");
        kasa_socket::session_storage::session_path(&root, None)
            .parent()?
            .to_path_buf()
    };
    Some(
        root.join("collaboration")
            .join(format!("observations-{port}.json")),
    )
}

pub fn register(backend: Arc<dyn Backend>, port: u16) -> Result<CollectorGuard> {
    let fixture = std::env::var_os("KASATERM_TEST_BOARD_FIXTURE").is_some();
    if fixture && fixture_root().is_none() {
        bail!("board fixture storage is not isolated");
    }
    let machine = local_id()?;
    let label = if isolated() {
        "Verification".into()
    } else {
        crate::machines::self_label()
    };
    let source_override = fixture.then(||json!({"machine_id":machine,"label":label,"state":"online",
        "source_kind":"fixture","capabilities":["synthetic"],"complete":true,"observed_at_ms":board::now_ms(),"panes":[]}));
    register_with_config(
        backend,
        CollectorConfig {
            machine_id: machine,
            label,
            journal_path: journal_path(port),
            remote_enabled: !isolated(),
            source_override,
            machines: None,
        },
    )
}

pub fn register_with_config(
    backend: Arc<dyn Backend>,
    config: CollectorConfig,
) -> Result<CollectorGuard> {
    let machine = config.machine_id;
    let label = config.label;
    let mut store = BoardStore::new(
        machine.clone(),
        uuid::Uuid::new_v4().to_string(),
        config.journal_path,
    );
    store.fail_source(&machine, &label, "initial observation pending");
    store.persist();
    let service = Arc::new(Service {
        machine_id: machine.clone(),
        configured_machines: config.machines,
        store: Mutex::new(store),
        backend: Arc::downgrade(&backend),
        routes: Mutex::new(HashMap::new()),
        aliases: Mutex::new(HashMap::new()),
        stop: AtomicBool::new(false),
        source_override: config.source_override,
    });
    {
        let mut current = slot().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(previous) = current.upgrade() {
            previous.stop.store(true, Ordering::Release);
        }
        *current = Arc::downgrade(&service);
    }
    let weak = Arc::downgrade(&service);
    std::thread::Builder::new()
        .name("collab-local-observer".into())
        .spawn(move || loop {
            let Some(service) = weak.upgrade() else { break };
            if service.stop.load(Ordering::Acquire) {
                break;
            }
            if !collect_local(&service, &machine, &label) {
                break;
            }
            drop(service);
            std::thread::sleep(Duration::from_secs(2));
        })?;
    if !config.remote_enabled {
        return Ok(CollectorGuard(service));
    }
    let weak = Arc::downgrade(&service);
    std::thread::Builder::new()
        .name("collab-remote-observer".into())
        .spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(async move {
                let Ok(client) = http_client() else { return };
                loop {
                    let Some(service) = weak.upgrade() else { break };
                    if service.stop.load(Ordering::Acquire) || service.backend.strong_count() == 0 {
                        break;
                    }
                    refresh_remotes(&service, &client).await;
                    drop(service);
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            });
        })?;
    Ok(CollectorGuard(service))
}

fn collect_local(service: &Service, machine: &str, label: &str) -> bool {
    let Some(backend) = service.backend.upgrade() else {
        return false;
    };
    let result = if let Some(source) = &service.source_override {
        let mut source = source.clone();
        source["observed_at_ms"] = json!(board::now_ms());
        Ok(source)
    } else {
        backend.collab_board_source()
    };
    let mut store = service.store.lock().unwrap_or_else(|e| e.into_inner());
    let before = store.cursor();
    if let Err(e) = result.and_then(|source| {
        if field(&source, "machine_id") != Some(machine) {
            bail!("local source machine mismatch");
        }
        store.observe(&source)
    }) {
        // 이유를 남긴다 — 보드가 「관측 불가」로만 서면 30분이 지나도 무엇이 막는지 알 길이
        // 없다(2026-09-16). 같은 이유는 한 번만 적는다.
        static LAST: std::sync::OnceLock<std::sync::Mutex<String>> = std::sync::OnceLock::new();
        let text = format!("{e:#}");
        let mut last = LAST.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
        if *last != text {
            eprintln!("[board] local observation failed: {text}");
            *last = text;
        }
        store.fail_source(machine, label, "local observation unavailable");
    }
    if store.cursor() != before {
        store.persist();
    }
    true
}

fn service() -> Result<Arc<Service>> {
    slot()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .upgrade()
        .context("collaboration collector is not running")
}

fn local_scope(params: &Value) -> Result<bool> {
    match field(params, "scope").unwrap_or("all") {
        "local" => Ok(true),
        "all" => Ok(false),
        _ => bail!("scope must be all or local"),
    }
}

pub fn snapshot(params: &Value) -> Result<Value> {
    let local = local_scope(params)?;
    let service = service()?;
    let result = service
        .store
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .snapshot(local);
    Ok(result)
}

pub fn changes(params: &Value) -> Result<Value> {
    let local = local_scope(params)?;
    let service = service()?;
    let result = service
        .store
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .changes(
            field(params, "since"),
            params["limit"].as_u64().unwrap_or(100).min(200) as usize,
            local,
        );
    Ok(result)
}

fn http_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}

async fn fetch_json(
    client: &reqwest::Client,
    base: &str,
    path: &str,
    query: &[(&str, String)],
) -> Result<Value> {
    let mut request = client
        .get(format!("{}{path}", base.trim_end_matches('/')))
        .query(query);
    if let Some(token) = crate::remote::connection_auth_token(base) {
        request = request.header("x-kasa-token", token);
    }
    let mut response = request
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("unreachable"))?;
    match response.status().as_u16() {
        200..=299 => (),
        404 | 405 | 501 => bail!("unsupported_api"),
        401 | 403 => bail!("authentication_required"),
        _ => bail!("remote_error"),
    }
    if response
        .content_length()
        .is_some_and(|n| n > board::MAX_SOURCE_BYTES as u64)
    {
        bail!("response_too_large");
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| anyhow::anyhow!("unreachable"))?
    {
        if bytes.len() + chunk.len() > board::MAX_SOURCE_BYTES {
            bail!("response_too_large");
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("invalid_response"))
}

fn source_from_snapshot(value: &Value, expected: Option<&str>) -> Result<Value> {
    if value["schema_version"] != 1 || value["scope"] != "local" {
        bail!("unsupported_api");
    }
    let sources = value["sources"].as_array().context("invalid_response")?;
    if sources.len() != 1 {
        bail!("ambiguous_source");
    }
    let mut source = sources[0].clone();
    let machine = field(&source, "machine_id").context("identity_missing")?;
    if expected.is_some_and(|expected| expected != machine) {
        bail!("identity_mismatch");
    }
    if source["state"] != "online" {
        bail!("source_unavailable");
    }
    let at = source["observed_at_ms"]
        .as_u64()
        .context("invalid_response")?;
    // Cache freshness is a failure state, never a reason to delete old places.
    if board::now_ms().saturating_sub(at) > 30_000 || at > board::now_ms() + 30_000 {
        bail!("stale_observation");
    }
    source["panes"] = value["panes"].clone();
    Ok(source)
}

async fn refresh_remotes(service: &Service, client: &reqwest::Client) {
    use futures_util::{stream, StreamExt};
    let local = service.machine_id.clone();
    let mut seen = HashSet::new();
    let machines: Vec<_> = service
        .machines()
        .into_iter()
        // base 마다 한 번 — 한 기계에 직통·우회 두 길이 있으면 둘 다 물어 어느 쪽이 사는지 안다.
        .filter(|machine| {
            !machine.base.is_empty()
                && machine.machine_id.as_deref() != Some(&local)
                && seen.insert(machine.base.clone())
        })
        .take(128)
        .collect();
    let mut pending = stream::iter(machines.into_iter().map(|machine| async move {
        let value = fetch_json(
            client,
            &machine.base,
            "/collab/board",
            &[("scope", "local".into())],
        )
        .await;
        let result =
            value.and_then(|value| source_from_snapshot(&value, machine.machine_id.as_deref()));
        (machine, result)
    }))
    .buffer_unordered(4);
    let mut resolved = HashSet::new();
    let mut failures = Vec::new();
    while let Some((mut machine, result)) = pending.next().await {
        if service.stop.load(Ordering::Acquire) {
            return;
        }
        let mut store = service.store.lock().unwrap_or_else(|e| e.into_inner());
        let before = store.cursor();
        match result {
            Ok(source) => {
                let id = source["machine_id"].as_str().unwrap().to_owned();
                let previous_identity = service
                    .aliases
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(machine.base.clone(), id.clone());
                if let Some(previous) =
                    previous_identity.filter(|old| !old.is_empty() && old != &id)
                {
                    store.fail_source(&previous, &machine.label, "route_identity_changed");
                }
                store.resolve_alias(&unresolved_id(&machine.base), &id);
                if id == local {
                    continue;
                }
                if !resolved.insert(id.clone()) {
                    // 이번 바퀴에 다른 길로 이미 관측했다 — 이쪽이 직통이고 잡힌 길이 우회면
                    // 길만 직통으로 갈아 끼운다(관측은 한 번이면 된다).
                    let mut routes = service.routes.lock().unwrap_or_else(|e| e.into_inner());
                    if !is_relay_base(&machine.base)
                        && routes.get(&id).is_some_and(|m| is_relay_base(&m.base))
                    {
                        machine.machine_id = Some(id.clone());
                        routes.insert(id, machine);
                    }
                    continue;
                }
                machine.machine_id = Some(id.clone());
                if store.observe(&source).is_ok() {
                    service
                        .routes
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(id, machine);
                } else {
                    store.fail_source(&id, &machine.label, "invalid or incomplete remote data");
                }
            }
            Err(error) => {
                if error.to_string() == "identity_mismatch" {
                    service
                        .aliases
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(machine.base.clone(), String::new());
                }
                let id = machine
                    .machine_id
                    .clone()
                    .or_else(|| service.aliases.lock().ok()?.get(&machine.base).cloned())
                    .unwrap_or_else(|| unresolved_id(&machine.base));
                failures.push((id, machine.label, error.to_string()));
            }
        }
        if before != store.cursor() {
            store.persist();
        }
    }
    // A roster lease expiring means discovery stopped, not that an agent died.
    let current_bases: HashSet<_> = service.machines().into_iter().map(|m| m.base).collect();
    let routes = service
        .routes
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let mut store = service.store.lock().unwrap_or_else(|e| e.into_inner());
    let before = store.cursor();
    for (id, label, error) in failures {
        if !resolved.contains(&id) {
            store.fail_source(&id, &label, &error);
        }
    }
    for (id, machine) in routes {
        if !current_bases.contains(&machine.base) {
            store.fail_source(&id, &machine.label, "source absent from current discovery");
        }
    }
    if before != store.cursor() {
        store.persist();
    }
}

fn unresolved_id(base: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    base.hash(&mut hash);
    format!("unresolved:{:016x}", hash.finish())
}

fn validate_address(actual: &Value, requested: &Value) -> Result<()> {
    for key in [
        "machine_id",
        "surface_key",
        "surface_id",
        "session_id",
        "instance_id",
    ] {
        if actual.get(key) != requested.get(key) {
            bail!("pane identity changed ({key}); refresh the board");
        }
    }
    Ok(())
}

/// Internal routing only; callers must still validate the receiving pane.
pub fn known_route(machine_id: &str) -> Option<String> {
    let service = service().ok()?;
    verified_route(&service, machine_id)
}

fn verified_route(service: &Service, machine: &str) -> Option<String> {
    let source = service.store.lock().ok()?.source(machine)?;
    if source["state"] != "online"
        || board::now_ms().saturating_sub(source["observed_at_ms"].as_u64()?) > 15_000
    {
        return None;
    }
    let current = service.machines();
    let aliases = service.aliases.lock().ok()?.clone();
    let preferred = service.routes.lock().ok()?.get(machine)?.base.clone();
    // A configured identity contradicting an observed identity is ambiguous,
    // even if another alias happens to answer with the requested machine ID.
    if current.iter().any(|m| {
        m.machine_id.as_deref() == Some(machine)
            && aliases
                .get(&m.base)
                .is_some_and(|observed| observed != machine)
    }) {
        return None;
    }
    let mut candidates: Vec<_> = current
        .iter()
        .filter(|m| {
            aliases
                .get(&m.base)
                .is_some_and(|observed| observed == machine)
                && current
                    .iter()
                    .filter(|other| other.base == m.base)
                    .all(|other| other.machine_id.as_deref().is_none_or(|id| id == machine))
        })
        .map(|m| m.base.clone())
        .collect();
    // 직통이 앞, 우회가 뒤 — 둘 다 살아 있으면 직통.
    candidates.sort_by_key(|base| (is_relay_base(base), base.clone()));
    candidates.dedup();
    if candidates.contains(&preferred) {
        Some(preferred)
    } else {
        candidates.into_iter().next()
    }
}

pub fn inspect(backend: &dyn Backend, params: &Value) -> Result<Value> {
    let address = params.get("address").context("inspect requires address")?;
    let machine = field(address, "machine_id").context("inspect requires machine_id")?;
    let surface = field(address, "surface_id").context("inspect requires surface_id")?;
    let key = field(address, "surface_key").context("inspect requires surface_key")?;
    let limit = params["limit"].as_u64().unwrap_or(20).clamp(1, 50) as usize;
    if machine != local_id()? {
        if params["local_only"] == true {
            bail!("remote inspection must terminate at its source");
        }
        let base = known_route(machine)
            .or_else(|| relay_base(machine))
            .context("remote source has no current unambiguous route")?;
        let mut remote_params = params.clone();
        remote_params["local_only"] = json!(true);
        remote_params["limit"] = json!(limit);
        return std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            runtime.block_on(async {
                let value = fetch_json(
                    &http_client()?,
                    &base,
                    "/collab/inspect",
                    &[("params", remote_params.to_string())],
                )
                .await?;
                validate_address(&remote_params["address"], &value["address"])?;
                if value["schema_version"] != 1
                    || value["bounded"] != true
                    || value["events"]
                        .as_array()
                        .is_none_or(|events| events.len() > limit)
                    || serde_json::to_vec(&value)?.len() > 132 * 1024
                {
                    bail!("invalid bounded inspection response");
                }
                Ok(value)
            })
        })
        .join()
        .map_err(|_| anyhow::anyhow!("remote inspection worker failed"))?;
    }
    if crate::surface_keys::get(surface).as_deref() != Some(key) {
        bail!("surface identity changed; refresh the board");
    }
    let live = kasa_pty::lookup_session(surface).context("selected live place no longer exists")?;
    validate_address(&backend.collab_pane_identity(surface)?, address)?;
    let mut events = serde_json::to_value(backend.pane_activity_log(surface, limit)?)?;
    // Existing activity is bounded by input tail and event count; individual
    // tool output still needs a byte cap before it crosses the HTTP boundary.
    bound_detail(&mut events);
    let mut truncated = false;
    while serde_json::to_vec(&events)?.len() > 128 * 1024 {
        let rows = events.as_array_mut().context("activity must be an array")?;
        if rows.is_empty() {
            break;
        }
        rows.remove(0);
        truncated = true;
    }
    if validate_address(&backend.collab_pane_identity(surface)?, address).is_err()
        || crate::surface_keys::get(surface).as_deref() != Some(key)
        || !kasa_pty::lookup_session(surface).is_some_and(|now| Arc::ptr_eq(&live, &now))
    {
        bail!("pane identity changed during inspection");
    }
    Ok(
        json!({"schema_version":1,"address":address,"events":events,"bounded":true,"limit":limit,"truncated":truncated}),
    )
}

fn bound_detail(value: &mut Value) {
    match value {
        Value::String(text) => *text = board::detail_text(text, 2048),
        Value::Array(values) => {
            values.truncate(50);
            for value in values {
                bound_detail(value);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                bound_detail(value);
            }
        }
        _ => (),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    #[derive(Default)]
    pub(crate) struct SyntheticBackend(pub std::sync::atomic::AtomicUsize);
    impl Backend for SyntheticBackend {
        fn list_workspaces(&self) -> Result<Vec<kasa_socket::backend::WorkspaceInfo>> {
            panic!("legacy workspace scan")
        }
        fn current_workspace(&self) -> Result<Option<kasa_socket::backend::WorkspaceInfo>> {
            panic!("legacy workspace scan")
        }
        fn list_surfaces(&self) -> Result<Vec<kasa_socket::backend::SurfaceInfo>> {
            panic!("legacy surface scan")
        }
        fn focus_surface(&self, _: &str) -> Result<()> {
            panic!("unexpected mutation")
        }
        fn split_surface(
            &self,
            _: kasa_socket::SplitDirection,
            _: bool,
            _: Option<&str>,
        ) -> Result<kasa_socket::backend::SurfaceInfo> {
            panic!("unexpected mutation")
        }
        fn send_text(&self, _: Option<&str>, _: &str) -> Result<()> {
            panic!("unexpected mutation")
        }
        fn send_key(&self, _: Option<&str>, _: &str) -> Result<()> {
            panic!("unexpected mutation")
        }
        fn collab_board(&self) -> Result<Vec<kasa_socket::backend::PaneActivity>> {
            panic!("legacy Claude/peer board must not run")
        }
        fn collab_board_source(&self) -> Result<Value> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(
                json!({"machine_id":"synthetic","label":"Synthetic","state":"online","complete":true,
                "source_kind":"synthetic","capabilities":["live_places"],"observed_at_ms":board::now_ms(),"panes":[]}),
            )
        }
        fn collab_snapshot(&self, params: &Value) -> Result<Value> {
            Ok(json!({"schema_version":1,"scope":params["scope"],"panes":[],"synthetic":true}))
        }
        fn collab_changes(&self, _: &Value) -> Result<Value> {
            Ok(json!({"changes":[],"synthetic":true}))
        }
        fn collab_inspect(&self, _: &Value) -> Result<Value> {
            Ok(json!({"events":[],"synthetic":true}))
        }
    }

    #[test]
    fn collector_is_harness_independent_and_cache_readers_do_not_scan() {
        let backend = Arc::new(SyntheticBackend::default());
        let object: Arc<dyn Backend> = backend.clone();
        let mut service = Service {
            machine_id: "synthetic".into(),
            configured_machines: Some(Vec::new()),
            store: Mutex::new(BoardStore::new("synthetic".into(), "test".into(), None)),
            backend: Arc::downgrade(&object),
            routes: Mutex::new(HashMap::new()),
            aliases: Mutex::new(HashMap::new()),
            stop: AtomicBool::new(false),
            source_override: None,
        };
        assert!(collect_local(&service, "synthetic", "Synthetic"));
        for _ in 0..100 {
            assert_eq!(
                service.store.lock().unwrap().snapshot(false)["sources"][0]["is_local"],
                true
            );
        }
        assert_eq!(backend.0.load(Ordering::SeqCst), 1);
        assert!(collect_local(&service, "synthetic", "Synthetic"));
        assert_eq!(backend.0.load(Ordering::SeqCst), 2);
        service.source_override = Some(
            json!({"machine_id":"synthetic","label":"Fixture","state":"online",
            "source_kind":"fixture","capabilities":["synthetic"],"complete":true,"panes":[]}),
        );
        assert!(collect_local(&service, "synthetic", "Fixture"));
        assert_eq!(
            backend.0.load(Ordering::SeqCst),
            2,
            "fixture must not inspect live producers"
        );
        drop(object);
        drop(backend);
        assert!(!collect_local(&service, "synthetic", "Synthetic"));
    }

    #[test]
    fn production_source_paths_do_not_reenter_legacy_agent_or_peer_inventory() {
        let desktop = include_str!("../../../app/kasaterm/src/socket.rs");
        let source = desktop
            .split("fn collab_board_source(&self)")
            .nth(1)
            .unwrap()
            .split("fn collab_board(&self)")
            .next()
            .unwrap();
        for forbidden in [
            "agents_status(",
            "agents_cached(",
            "rebind_agents_panes(",
            "peers::",
        ] {
            assert!(!source.contains(forbidden), "new source called {forbidden}");
        }
        let standalone = include_str!("standalone.rs");
        let source = standalone
            .split("fn collab_board_source(&self)")
            .nth(1)
            .unwrap()
            .split("// --- required")
            .next()
            .unwrap();
        assert!(
            !source.contains("claude_bin")
                && !source.contains("Command")
                && !source.contains("collab_board(")
        );
        assert!(
            source.contains("kasa_pty::live_sessions()") && source.contains("activity_unsupported")
        );
    }
    #[test]
    fn remote_requires_local_scope_and_matching_identity() {
        let value = json!({"schema_version":1,"scope":"local","sources":[{"machine_id":"remote",
            "state":"online","observed_at_ms":board::now_ms(),"complete":true}],"panes":[]});
        assert!(source_from_snapshot(&value, Some("remote")).is_ok());
        assert!(source_from_snapshot(&value, Some("other")).is_err());
        let mut all = value.clone();
        all["scope"] = json!("all");
        assert!(source_from_snapshot(&all, None).is_err());
        let mut stale = value;
        stale["sources"][0]["state"] = json!("stale");
        assert!(source_from_snapshot(&stale, None).is_err());
    }
    #[test]
    fn inspect_identity_includes_session_and_instance() {
        let address = json!({"machine_id":"m","surface_key":"s","surface_id":"%1","session_id":"old","instance_id":"one"});
        assert!(validate_address(&address, &address).is_ok());
        for key in [
            "machine_id",
            "surface_key",
            "surface_id",
            "session_id",
            "instance_id",
        ] {
            let mut wrong = address.clone();
            wrong[key] = json!("different");
            assert!(validate_address(&address, &wrong).is_err());
        }
    }
    #[test]
    fn remote_fetch_uses_local_scope_and_rejects_redirects() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let thread = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut buf = [0; 4096];
            let n = socket.read(&mut buf).unwrap();
            let request = String::from_utf8_lossy(&buf[..n]);
            assert!(request.starts_with("GET /collab/board?scope=local "));
            socket.write_all(b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/secret\r\nContent-Length: 0\r\n\r\n").unwrap();
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        assert!(runtime
            .block_on(fetch_json(
                &http_client().unwrap(),
                &base,
                "/collab/board",
                &[("scope", "local".into())]
            ))
            .is_err());
        thread.join().unwrap();
    }

    #[test]
    fn injected_remote_aliases_merge_and_failed_sources_keep_last_rows() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let responder = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut bytes = [0; 4096];
                let n = socket.read(&mut bytes).unwrap();
                assert!(String::from_utf8_lossy(&bytes[..n]).contains("/collab/board?scope=local"));
                let body = json!({"schema_version":1,"scope":"local","sources":[{"machine_id":"remote-machine",
                    "label":"Remote","state":"online","observed_at_ms":board::now_ms(),"complete":true}],
                    "panes":[{"address":{"machine_id":"remote-machine","surface_key":"stable","surface_id":"%1"},"status":"working"}]}).to_string();
                write!(
                    socket,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .unwrap();
            }
        });
        let machines: Vec<_> = ["first", "second"]
            .into_iter()
            .map(|label| crate::machines::Machine {
                label: label.into(),
                machine_id: None,
                base: format!("{base}/{label}"),
                host: String::new(),
                kvm: None,
                roots: Vec::new(),
                home: false,
                ssh: None,
                chrome_port: None,
                key: None,
                tunneled: false,
                guest: false,
            })
            .collect();
        let backend: Arc<dyn Backend> = Arc::new(SyntheticBackend::default());
        let mut service = Service {
            machine_id: "synthetic".into(),
            configured_machines: Some(machines.clone()),
            store: Mutex::new(BoardStore::new("synthetic".into(), "test".into(), None)),
            backend: Arc::downgrade(&backend),
            routes: Mutex::new(HashMap::new()),
            aliases: Mutex::new(HashMap::new()),
            stop: AtomicBool::new(false),
            source_override: None,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let client = http_client().unwrap();
        runtime.block_on(refresh_remotes(&service, &client));
        responder.join().unwrap();
        let first = service.store.lock().unwrap().snapshot(false);
        assert_eq!(first["sources"].as_array().unwrap().len(), 1);
        assert_eq!(first["panes"].as_array().unwrap().len(), 1);
        assert_eq!(service.aliases.lock().unwrap().len(), 2);
        assert!(
            verified_route(&service, "remote-machine").is_some(),
            "base-only configured sources must reuse the observed route"
        );
        service.configured_machines = Some(Vec::new());
        assert!(
            verified_route(&service, "remote-machine").is_none(),
            "removed routes must not remain usable"
        );
        let aliases = service.aliases.lock().unwrap().clone();
        let mut conflicting = machines.clone();
        conflicting[0].machine_id = Some("remote-machine".into());
        service
            .aliases
            .lock()
            .unwrap()
            .insert(conflicting[0].base.clone(), String::new());
        service.configured_machines = Some(conflicting);
        assert!(
            verified_route(&service, "remote-machine").is_none(),
            "contradictory identities must fail closed"
        );
        *service.aliases.lock().unwrap() = aliases;
        service.configured_machines = Some(machines);
        runtime.block_on(refresh_remotes(&service, &client));
        let failed = service.store.lock().unwrap().snapshot(false);
        assert_eq!(failed["sources"].as_array().unwrap().len(), 1);
        assert_eq!(failed["sources"][0]["state"], "stale");
        assert_eq!(failed["panes"][0]["id"], first["panes"][0]["id"]);
        assert!(
            verified_route(&service, "remote-machine").is_none(),
            "stale sources need a new observation before control"
        );
    }
}
