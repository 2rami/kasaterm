//! `/relay/workspace-assistant` — the account-owned assistant behind the gateway.
//!
//! Identity comes only from the device credential on the request; a body never names an
//! account. The credential is checked again after the body has been read and once more
//! before a provider result is recorded, so a revoked device cannot finish a slow request.
//! Nothing here goes through `/relay/account/*`: that path is a proxy to the account's
//! desktop, while this service lives in the gateway itself.

use super::*;
use crate::workspace_assistant::{
    self as ws, AuthContext, DecisionPurpose, JevAdapter, MessageRequest, ProjectKind, Store,
    TaskState, TextProvider,
};
use serde::Deserialize;
use serde_json::{json, Value};

const PREFIX: &str = "/relay/workspace-assistant";
const MAX_BODY: usize = 64 * 1024;

pub(super) struct Service {
    store: Store,
    provider: TextProvider,
    /// Absent unless the deployment ships node and `scripts/jev/client.mjs`; tasks then stay
    /// unclassified instead of being guessed.
    jev: Option<JevAdapter>,
}

impl Service {
    /// `None` when the gateway keeps no state directory: the route then answers
    /// `storage_unavailable` rather than holding account keys in memory only.
    pub(super) fn open(state_path: Option<&std::path::Path>) -> Option<Arc<Self>> {
        let directory = state_path?.with_file_name("workspace-assistant");
        let store = match Store::open(directory) {
            Ok(store) => store,
            Err(error) => {
                eprintln!("[gateway] workspace assistant disabled: {}", error.code());
                return None;
            }
        };
        let priorities = std::env::var("KASA_ASSISTANT_MODELS")
            .ok()
            .map(|list| list.split(',').map(str::trim).filter(|m| !m.is_empty()).map(String::from).collect())
            .unwrap_or_default();
        let provider = match TextProvider::new(priorities) {
            Ok(provider) => provider,
            Err(_) => {
                eprintln!("[gateway] workspace assistant disabled: KASA_ASSISTANT_MODELS is not a model list");
                return None;
            }
        };
        let jev = match (std::env::var_os("KASA_JEV_NODE"), std::env::var_os("KASA_JEV_CLIENT")) {
            (Some(node), Some(client)) => match JevAdapter::new(node.into(), client.into()) {
                Ok(adapter) => Some(adapter),
                Err(_) => {
                    eprintln!("[gateway] workspace assistant: KASA_JEV_NODE/KASA_JEV_CLIENT must be absolute files; decisions disabled");
                    None
                }
            },
            _ => None,
        };
        Some(Arc::new(Self { store, provider, jev }))
    }
}

pub(super) fn routes() -> Router<Gate> {
    Router::new()
        .route(PREFIX, get(snapshot))
        .route("/relay/workspace-assistant/key", axum::routing::put(key_put).delete(key_delete))
        .route("/relay/workspace-assistant/messages", axum::routing::post(messages))
        .route("/relay/workspace-assistant/projects", axum::routing::post(projects))
        .route("/relay/workspace-assistant/tasks", axum::routing::post(tasks))
        .route("/relay/workspace-assistant/notifications/claim", axum::routing::post(claim))
        .route("/relay/workspace-assistant/route", axum::routing::post(route))
}

struct Call {
    service: Arc<Service>,
    access: AccountAccess,
    ctx: AuthContext,
    body: Value,
}

impl Call {
    fn authorized(&self) -> impl Fn() -> bool + Send + Sync + 'static {
        let access = self.access.clone();
        move || access.valid()
    }
}

/// Authenticates, reads a bounded body, then authenticates again so a revocation during a
/// slow upload cannot slip a request through.
async fn open(gate: Gate, req: axum::extract::Request) -> Result<Call, axum::response::Response> {
    let Some(service) = gate.workspace.clone() else {
        return Err(json_err(StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable"));
    };
    let Some(access) = account_access(&gate, req.headers()) else {
        return Err(json_err(StatusCode::UNAUTHORIZED, "unauthorized"));
    };
    let has_body = *req.method() != axum::http::Method::GET;
    let bytes = match tokio::time::timeout(
        Duration::from_secs(10),
        axum::body::to_bytes(req.into_body(), MAX_BODY),
    )
    .await
    {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(_)) => return Err(json_err(StatusCode::PAYLOAD_TOO_LARGE, "body_too_large")),
        Err(_) => return Err(json_err(StatusCode::REQUEST_TIMEOUT, "request_timeout")),
    };
    let body = if has_body {
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(body) if body.is_object() => body,
            _ => return Err(json_err(StatusCode::BAD_REQUEST, "invalid_request")),
        }
    } else {
        Value::Null
    };
    if !access.valid() {
        return Err(json_err(StatusCode::UNAUTHORIZED, "unauthorized"));
    }
    let Ok(ctx) = AuthContext::authenticated(&access.account, &access.device_id) else {
        return Err(json_err(StatusCode::UNAUTHORIZED, "unauthorized"));
    };
    Ok(Call { service, access, ctx, body })
}

fn failure(error: ws::Error) -> axum::response::Response {
    let status = match error {
        ws::Error::Invalid => StatusCode::BAD_REQUEST,
        ws::Error::Disabled => StatusCode::PRECONDITION_REQUIRED,
        ws::Error::Conflict => StatusCode::CONFLICT,
        ws::Error::Storage => StatusCode::SERVICE_UNAVAILABLE,
        ws::Error::Stale => StatusCode::GONE,
        ws::Error::RateLimited => StatusCode::TOO_MANY_REQUESTS,
        ws::Error::Unavailable => StatusCode::BAD_GATEWAY,
        ws::Error::EvidenceRequired => StatusCode::UNPROCESSABLE_ENTITY,
    };
    json_err(status, error.code())
}

fn ok(payload: Value) -> axum::response::Response {
    let mut body = json!({ "ok": true });
    if let (Some(into), Some(from)) = (body.as_object_mut(), payload.as_object()) {
        into.extend(from.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    (
        [(header::CACHE_CONTROL, "no-store")],
        axum::Json(body),
    )
        .into_response()
}

fn respond<T: serde::Serialize>(field: &str, result: Result<T, ws::Error>) -> axum::response::Response {
    match result {
        Ok(value) => ok(json!({ field: value })),
        Err(error) => failure(error),
    }
}

fn parse<T: serde::de::DeserializeOwned>(body: &Value) -> Result<T, axum::response::Response> {
    serde_json::from_value(body.clone()).map_err(|_| json_err(StatusCode::BAD_REQUEST, "invalid_request"))
}

async fn snapshot(State(gate): State<Gate>, req: axum::extract::Request) -> axum::response::Response {
    let call = match open(gate, req).await {
        Ok(call) => call,
        Err(response) => return response,
    };
    respond("snapshot", call.service.store.snapshot(&call.ctx))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyBody {
    expected_key_revision: u64,
    key: Option<String>,
}

async fn key_put(State(gate): State<Gate>, req: axum::extract::Request) -> axum::response::Response {
    let call = match open(gate, req).await {
        Ok(call) => call,
        Err(response) => return response,
    };
    let body: KeyBody = match parse(&call.body) {
        Ok(body) => body,
        Err(response) => return response,
    };
    let Some(key) = body.key else {
        return json_err(StatusCode::BAD_REQUEST, "invalid_request");
    };
    respond("status", call.service.store.set_key(&call.ctx, body.expected_key_revision, Some(key)))
}

async fn key_delete(State(gate): State<Gate>, req: axum::extract::Request) -> axum::response::Response {
    let call = match open(gate, req).await {
        Ok(call) => call,
        Err(response) => return response,
    };
    let body: KeyBody = match parse(&call.body) {
        Ok(body) => body,
        Err(response) => return response,
    };
    if body.key.is_some() {
        return json_err(StatusCode::BAD_REQUEST, "invalid_request");
    }
    respond("status", call.service.store.set_key(&call.ctx, body.expected_key_revision, None))
}

async fn messages(State(gate): State<Gate>, req: axum::extract::Request) -> axum::response::Response {
    let call = match open(gate, req).await {
        Ok(call) => call,
        Err(response) => return response,
    };
    let request: MessageRequest = match parse(&call.body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let authorized = call.authorized();
    let result = call
        .service
        .store
        .message(&call.ctx, request, &call.service.provider, now_secs(), &authorized)
        .await;
    respond("message", result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectBody {
    name: String,
    #[serde(default)]
    kind: ProjectKind,
    #[serde(default)]
    keywords: Vec<String>,
}

async fn projects(State(gate): State<Gate>, req: axum::extract::Request) -> axum::response::Response {
    let call = match open(gate, req).await {
        Ok(call) => call,
        Err(response) => return response,
    };
    let body: ProjectBody = match parse(&call.body) {
        Ok(body) => body,
        Err(response) => return response,
    };
    respond("project", call.service.store.create_project(&call.ctx, &body.name, body.kind, body.keywords))
}

/// One observation of a request in progress. The first sighting of a `request_id` creates the
/// task; later sightings with a state or step move it. `verified` is never accepted here —
/// that needs trusted evidence, which no HTTP body can supply.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskBody {
    request_id: String,
    prompt: String,
    work_revision: String,
    #[serde(default)]
    required_checks: Vec<String>,
    state: Option<TaskState>,
    step: Option<String>,
}

async fn tasks(State(gate): State<Gate>, req: axum::extract::Request) -> axum::response::Response {
    let call = match open(gate, req).await {
        Ok(call) => call,
        Err(response) => return response,
    };
    let body: TaskBody = match parse(&call.body) {
        Ok(body) => body,
        Err(response) => return response,
    };
    let now = now_secs();
    let store = &call.service.store;
    let mut task = match store.create_task_once(
        &call.ctx,
        &body.request_id,
        &body.prompt,
        &body.work_revision,
        body.required_checks,
        now,
    ) {
        Ok(task) => task,
        Err(error) => return failure(error),
    };
    if let Some(state) = body.state {
        let step = body.step.as_deref().unwrap_or(&task.step);
        let moved = state != task.state || step != task.step || body.work_revision != task.work_revision;
        if moved && task.state != TaskState::Verified {
            task = match store.update_task(&call.ctx, &task.id, task.rev, state, step, &body.work_revision, now) {
                Ok(task) => task,
                Err(error) => return failure(error),
            };
        }
    }
    if task.project.is_none() && call.service.jev.is_some() {
        task = classify(&call, task).await;
    }
    ok(json!({ "task": task }))
}

/// Jev runs as a subprocess with a short deadline; the task keeps its current shape when the
/// decision is unavailable, and the key never leaves the store except into that subprocess.
async fn classify(call: &Call, task: ws::Task) -> ws::Task {
    let service = call.service.clone();
    let access = call.access.clone();
    let (account, device) = (access.account.clone(), access.device_id.clone());
    let (id, rev) = (task.id.clone(), task.rev);
    let decided = tokio::task::spawn_blocking(move || {
        let ctx = AuthContext::authenticated(&account, &device).ok()?;
        let jev = service.jev.as_ref()?;
        service
            .store
            .decide(&ctx, &id, rev, DecisionPurpose::ClassifyProject, jev, now_secs(), &move || access.valid())
            .ok()
    })
    .await
    .ok()
    .flatten();
    decided.unwrap_or(task)
}

/// Advisory recipient for a message the owner is still typing. Runs the same Jev subprocess
/// as classification with the account's own key; a missing adapter answers unavailable.
async fn route(State(gate): State<Gate>, req: axum::extract::Request) -> axum::response::Response {
    let call = match open(gate, req).await {
        Ok(call) => call,
        Err(response) => return response,
    };
    let input: ws::RouteInput = match parse(&call.body) {
        Ok(input) => input,
        Err(response) => return response,
    };
    if call.service.jev.is_none() {
        return failure(ws::Error::Unavailable);
    }
    let authorized = call.authorized();
    let (service, ctx) = (call.service.clone(), call.ctx);
    let result = tokio::task::spawn_blocking(move || {
        let jev = service.jev.as_ref().ok_or(ws::Error::Unavailable)?;
        service.store.route(&ctx, &input, jev, now_secs(), &authorized)
    })
    .await
    .unwrap_or(Err(ws::Error::Unavailable));
    match result {
        Ok(advice) => ok(json!({ "probabilities": advice.probabilities, "latency_ms": advice.latency_ms })),
        Err(error) => failure(error),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClaimBody {
    task_id: String,
    revision: u64,
}

async fn claim(State(gate): State<Gate>, req: axum::extract::Request) -> axum::response::Response {
    let call = match open(gate, req).await {
        Ok(call) => call,
        Err(response) => return response,
    };
    let body: ClaimBody = match parse(&call.body) {
        Ok(body) => body,
        Err(response) => return response,
    };
    respond("notification", call.service.store.claim_notification(&call.ctx, &body.task_id, body.revision, now_secs()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate_in(dir: &std::path::Path) -> Gate {
        std::fs::create_dir_all(dir).unwrap();
        let accounts = dir.join("relay-accounts.json");
        let mut file = crate::relay_auth::AccountsFile::default();
        for name in ["geno", "other"] {
            file.accounts.insert(
                name.into(),
                crate::relay_auth::Account {
                    pbkdf2_sha256: crate::relay_auth::hash_password_with("correct horse", 1000),
                    created: 1,
                    disabled: false,
                    login: None,
                },
            );
        }
        crate::relay_auth::save_accounts(&accounts, &file).unwrap();
        Gate::with_accounts(Some(dir.join("relay-state.json")), Some(accounts))
    }

    async fn serve(gate: Gate) -> std::net::SocketAddr {
        let app = crate::relay::router().merge(router(gate));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>())
                .await
                .unwrap()
        });
        addr
    }

    async fn call(addr: std::net::SocketAddr, method: &str, path: &str, bearer: Option<&str>, body: Option<Value>) -> (u16, Value) {
        let client = reqwest::Client::new();
        let mut request = client.request(method.parse().unwrap(), format!("http://{addr}{PREFIX}{path}"));
        if let Some(token) = bearer {
            request = request.bearer_auth(token);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let reply = request.send().await.unwrap();
        (reply.status().as_u16(), reply.json().await.unwrap_or_default())
    }

    async fn login(addr: std::net::SocketAddr, account: &str, machine: &str) -> String {
        let reply = reqwest::Client::new()
            .post(format!("http://{addr}/relay/login"))
            .json(&json!({"account":account,"password":"correct horse","machine_id":machine,"kind":"desktop"}))
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap();
        reply["token"].as_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn account_scoped_routes_authenticate_isolate_and_stay_idempotent() {
        let dir = std::env::temp_dir().join(format!("kasa-workspace-http-{}", uuid::Uuid::new_v4()));
        let addr = serve(gate_in(&dir)).await;
        let token = login(addr, "geno", "workspace-mac").await;
        let other = login(addr, "other", "other-mac").await;

        assert_eq!(call(addr, "GET", "", None, None).await.0, 401);
        assert_eq!(call(addr, "GET", "", Some("not-a-token"), None).await.0, 401);
        let (status, snapshot) = call(addr, "GET", "", Some(&token), None).await;
        assert_eq!(status, 200);
        assert_eq!(snapshot["snapshot"]["status"]["key_present"], false);
        assert_eq!(snapshot["snapshot"]["tasks"], json!([]));

        // Everything but the key registration is closed until a key exists.
        let message = json!({"id":"m1","text":"안녕","task_id":null,"model":null});
        let (status, reply) = call(addr, "POST", "/messages", Some(&token), Some(message.clone())).await;
        assert_eq!((status, reply["error"].as_str()), (428, Some("key_required")));

        let (status, reply) = call(addr, "PUT", "/key", Some(&token), Some(json!({"expected_key_revision":0,"key":"sk-test-0123456789"}))).await;
        assert_eq!(status, 200, "{reply}");
        assert_eq!(reply["status"]["key_present"], true);
        assert_eq!(reply["status"]["key_revision"], 1);
        let (status, reply) = call(addr, "PUT", "/key", Some(&token), Some(json!({"expected_key_revision":0,"key":"sk-test-0123456789"}))).await;
        assert_eq!((status, reply["error"].as_str()), (409, Some("revision_conflict")));
        assert_eq!(call(addr, "PUT", "/key", Some(&token), Some(json!({"expected_key_revision":1,"key":"short"}))).await.0, 400);
        assert_eq!(call(addr, "PUT", "/key", Some(&token), Some(json!({"expected_key_revision":1,"key":"sk-x","account":"other"}))).await.0, 400);

        // The key is per account: the other account still has none and never sees geno's data.
        let (_, foreign) = call(addr, "GET", "", Some(&other), None).await;
        assert_eq!(foreign["snapshot"]["status"]["key_present"], false);
        let (_, mine) = call(addr, "GET", "", Some(&token), None).await;
        assert_eq!(mine["snapshot"]["status"]["key_present"], true);
        assert!(!mine.to_string().contains("sk-test"));

        // Project keywords are dictionary labels only, so free text never reaches the classifier.
        assert_eq!(call(addr, "POST", "/projects", Some(&token), Some(json!({"name":"KASA","kind":"coding","keywords":["kasaterm"]}))).await.0, 400);
        let (status, project) = call(addr, "POST", "/projects", Some(&token), Some(json!({"name":"KASA","kind":"coding","keywords":[]}))).await;
        assert_eq!(status, 200, "{project}");
        assert_eq!(project["project"]["kind"], "coding");

        let observation = json!({"request_id":"req-1","prompt":"브랜치 그래프를 그려 주세요","work_revision":"rev-a","required_checks":["graph-tests"]});
        let (status, first) = call(addr, "POST", "/tasks", Some(&token), Some(observation.clone())).await;
        assert_eq!(status, 200, "{first}");
        assert_eq!(first["task"]["state"], "queued");
        assert_eq!(first["task"]["original_prompt"], "브랜치 그래프를 그려 주세요");
        let (_, again) = call(addr, "POST", "/tasks", Some(&token), Some(observation.clone())).await;
        assert_eq!(again["task"]["id"], first["task"]["id"]);
        assert_eq!(again["task"]["rev"], first["task"]["rev"]);
        let mut altered = observation.clone();
        altered["prompt"] = "다른 원문".into();
        assert_eq!(call(addr, "POST", "/tasks", Some(&token), Some(altered)).await.0, 409);
        let mut moved = observation.clone();
        moved["state"] = "working".into();
        moved["step"] = "그래프 그리는 중".into();
        let (status, moved) = call(addr, "POST", "/tasks", Some(&token), Some(moved)).await;
        assert_eq!(status, 200, "{moved}");
        assert_eq!(moved["task"]["state"], "working");
        assert_eq!(moved["task"]["step"], "그래프 그리는 중");
        let mut verified = observation.clone();
        verified["state"] = "verified".into();
        assert_eq!(call(addr, "POST", "/tasks", Some(&token), Some(verified)).await.0, 400);

        // A completion notification needs trusted evidence; self-reports cannot claim one.
        let claim = json!({"task_id":moved["task"]["id"],"revision":moved["task"]["rev"]});
        let (status, reply) = call(addr, "POST", "/notifications/claim", Some(&token), Some(claim)).await;
        assert_eq!((status, reply["error"].as_str()), (422, Some("verified_evidence_required")));

        // The other account sees none of it.
        let (_, foreign) = call(addr, "GET", "", Some(&other), None).await;
        assert_eq!(foreign["snapshot"]["tasks"], json!([]));
        assert_eq!(foreign["snapshot"]["projects"], json!([]));

        let (status, reply) = call(addr, "DELETE", "/key", Some(&token), Some(json!({"expected_key_revision":1}))).await;
        assert_eq!(status, 200, "{reply}");
        assert_eq!(reply["status"]["key_present"], false);

        reqwest::Client::new().post(format!("http://{addr}/relay/logout")).bearer_auth(&token).send().await.unwrap();
        assert_eq!(call(addr, "GET", "", Some(&token), None).await.0, 401);
        assert_eq!(call(addr, "PUT", "/key", Some(&token), Some(json!({"expected_key_revision":2,"key":"sk-test-0123456789"}))).await.0, 401);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn route_is_authenticated_bounded_and_unavailable_without_a_decision_adapter() {
        let dir = std::env::temp_dir().join(format!("kasa-workspace-route-{}", uuid::Uuid::new_v4()));
        let addr = serve(gate_in(&dir)).await;
        let token = login(addr, "geno", "workspace-mac").await;
        let body = json!({"message":"미러링 다시 봐","students":[{"id":"s0","name":"유우카","title":"미러링","latest":"","status":"waiting"}]});
        assert_eq!(call(addr, "POST", "/route", None, Some(body.clone())).await.0, 401);
        let (status, reply) = call(addr, "POST", "/route", Some(&token), Some(json!({"message":"x","students":[],"account":"other"}))).await;
        assert_eq!((status, reply["error"].as_str()), (400, Some("invalid_request")));
        // Without a decision adapter the gateway refuses instead of guessing; the desktop lets the owner pick.
        let (status, reply) = call(addr, "POST", "/route", Some(&token), Some(body)).await;
        assert_eq!((status, reply["error"].as_str()), (502, Some("decision_unavailable")));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn stateless_gateway_refuses_instead_of_keeping_keys_in_memory() {
        let dir = std::env::temp_dir().join(format!("kasa-workspace-stateless-{}", uuid::Uuid::new_v4()));
        let mut gate = gate_in(&dir);
        gate.workspace = None;
        let addr = serve(gate).await;
        let token = login(addr, "geno", "stateless-mac").await;
        let (status, reply) = call(addr, "GET", "", Some(&token), None).await;
        assert_eq!((status, reply["error"].as_str()), (503, Some("storage_unavailable")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
