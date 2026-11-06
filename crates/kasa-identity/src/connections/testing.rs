use super::*;
use axum::extract::{Path as AxPath, State};
use axum::routing::{delete, get, post};

/// What the fake providers saw: (method path, body or query).
pub type Seen = Arc<Mutex<Vec<(String, String)>>>;

pub fn seen(calls: &Seen, what: &str) -> Vec<String> {
    calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(path, _)| path == what)
        .map(|(_, body)| body.clone())
        .collect()
}

pub fn note(calls: &Seen, path: &str, body: String) {
    calls.lock().unwrap().push((path.into(), body));
}

pub async fn providers() -> (String, Seen) {
    let calls: Seen = Arc::default();
    let app = axum::Router::new()
        .route(
            "/google/revoke",
            post(|State(calls): State<Seen>, body: String| async move {
                note(&calls, "google/revoke", body.clone());
                // Google answers 400 invalid_token for a grant that is already gone.
                if body.contains("token=gone") {
                    return axum::http::StatusCode::BAD_REQUEST;
                }
                axum::http::StatusCode::OK
            }),
        )
        .route(
            "/github/token",
            post(|State(calls): State<Seen>, body: String| async move {
                note(&calls, "github/token", body.clone());
                // GitHub answers a refused refresh with 200 and an `error` field.
                if body.contains("refresh_token=dead") {
                    return axum::Json(json!({"error":"bad_refresh_token"}));
                }
                axum::Json(json!({"access_token":"gh-access-2","expires_in":28800,"refresh_token":"gh-refresh-2",
                    "refresh_token_expires_in":15897600,"token_type":"bearer"}))
            }),
        )
        .route(
            "/github/repos/{owner}/{name}/pulls",
            post(|State(calls): State<Seen>, AxPath((owner, name)): AxPath<(String, String)>, headers: axum::http::HeaderMap, body: String| async move {
                note(&calls, "github/pulls", format!("{owner}/{name} {} {body}", headers["authorization"].to_str().unwrap()));
                match name.as_str() {
                    "closed" => (axum::http::StatusCode::NOT_FOUND, axum::Json(json!({"message":"Not Found"}))),
                    "exists" => (axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                        axum::Json(json!({"message":"Validation Failed","errors":[{"message":"A pull request already exists"}]}))),
                    _ => (axum::http::StatusCode::CREATED, axum::Json(json!({"number":7,"html_url":format!("https://github.com/{owner}/{name}/pull/7")}))),
                }
            }),
        )
        .route(
            "/github/user/installations",
            get(|State(calls): State<Seen>, headers: axum::http::HeaderMap| async move {
                note(&calls, "github/installations", headers["authorization"].to_str().unwrap().into());
                axum::Json(json!({"total_count":0,"installations":[]}))
            }),
        )
        .route(
            "/github/applications/{id}/grant",
            delete(|State(calls): State<Seen>, AxPath(id): AxPath<String>, body: String| async move {
                note(&calls, "github/revoke", format!("{id} {body}"));
                axum::http::StatusCode::NO_CONTENT
            }),
        )
        .with_state(calls.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, calls)
}

pub fn endpoints(base: &str) -> Endpoints {
    Endpoints {
        google_revoke: format!("{base}/google/revoke"),
        github_token: format!("{base}/github/token"),
        github_api: format!("{base}/github"),
    }
}

pub async fn service() -> (Arc<Service>, Seen, PathBuf) {
    let (oauth, dir) = crate::oauth_accounts::testing::fixture();
    std::fs::create_dir_all(&dir).unwrap();
    let mut service = Service::open(Some(&dir.join("state.json")), oauth.config.clone()).unwrap();
    let (base, calls) = providers().await;
    Arc::get_mut(&mut service).unwrap().endpoints = endpoints(&base);
    (service, calls, dir)
}

pub fn github(access_expires: u64) -> Grant {
    github_as("4242", access_expires, "gh-refresh-1")
}

pub fn github_as(subject: &str, access_expires: u64, refresh: &str) -> Grant {
    Grant {
        provider: Provider::Github,
        subject: subject.into(),
        display: "octo".into(),
        access_token: "gh-access-1".into(),
        access_expires,
        refresh_token: Some(refresh.into()),
        refresh_expires: 0,
    }
}

pub const ME: Caller<'static> = Caller {
    account: "one",
    device: "dev_one",
    label: "Laptop",
};

pub fn now() -> u64 {
    crate::relay_auth::now_secs()
}

pub fn pr(repo: &str) -> Write {
    Write::Pr(PrDraft {
        repo: repo.into(),
        base: "main".into(),
        head: "feature/login".into(),
        title: "로그인 정리".into(),
        body: "비밀 본문 텍스트".into(),
        draft: true,
    })
}
