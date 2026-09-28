//! 폰(카사모바일) → 나쵸 앱 창구 중계. `/u/<slug>/nacho/app/<rest>` 를 나쵸의 `/api/app/<rest>` 로 넘긴다.
//!
//! 나쵸는 폰을 모른다 — 누가 보냈는지는 이 서버가 주소(slug)로 확인한 `MobileUser` 가 정본이다.
//! 그래서 폰이 보낸 헤더는 **하나도 옮기지 않고** 요청을 새로 짓는다: 폰이 `X-Kasa-Owner: 1` 을
//! 스스로 실어 보내도 여기서 사라지고, 이 서버가 확인한 신원과 나쵸 키만 실린다.
//!
//! 닫힘이 기본이다.
//! - 주인 주소(`owner`)로 온 요청만. 다른 사용자 주소·주소 없는 로컬 호출은 403 — 로컬 도구는 나쵸를
//!   직접 부르면 되고, 여기로 오면 신원을 지어낼 길이 생긴다.
//! - 나쵸 자리 서술자(`nacho-ask.json` 또는 `NACHO_ASK_URL`)가 없으면 503, 앱 키(`nacho-app.key`)가 없으면 503.
//! - `m/<기계>/` 로 다른 기계를 거쳐 넘기지 않는다 — 그 길은 신원 헤더를 버린다. 폰이 붙은 허브가
//!   서술자에 적힌 나쵸로 **직접** 넘긴다(맥북 허브면 메시 주소, 미니면 127.0.0.1).

use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use std::path::{Path, PathBuf};

use crate::mobile::MobileUser;

pub(crate) const BODY_LIMIT: usize = 64 * 1024;
const TIMEOUT_SECS: u64 = 40;

#[derive(Debug, PartialEq)]
pub(crate) struct Target {
    pub url: String,
    pub key: String,
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn descriptor_path() -> Option<PathBuf> {
    std::env::var_os("NACHO_ASK_DESCRIPTOR")
        .map(PathBuf::from)
        .or_else(|| home().map(|h| h.join(".config/kasaterm/nacho-ask.json")))
}

/// 앱 창구 전용 키. 펫 창구 키(`nacho-ask.key`)와 **따로다** — 펫은 토큰을 안 싣는 판이 있어 그 키를 만들면
/// 펫이 끊긴다. 폰이 붙는 허브와 나쵸가 같은 기계면 그 기계 한 곳에만 둔다.
fn key_path() -> Option<PathBuf> {
    std::env::var_os("NACHO_APP_TOKEN_FILE")
        .map(PathBuf::from)
        .or_else(|| home().map(|h| h.join(".config/nacho-app.key")))
}

/// 넘길 곳과 키. 넘길 곳은 펫 대리인(`tools/request_journal/ask.py`)과 같은 서술자, 키는 앱 전용 파일.
pub(crate) fn target_from(env_url: Option<String>, desc: Option<&Path>, key: Option<&Path>) -> Result<Target, &'static str> {
    let url = env_url
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty())
        .or_else(|| {
            let raw = std::fs::read_to_string(desc?).ok()?;
            let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
            v.get("url").and_then(|u| u.as_str()).map(|u| u.trim().to_string())
        })
        .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
        .ok_or("nacho_unconfigured")?;
    let key = key
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
        .ok_or("nacho_key_missing")?;
    Ok(Target { url: url.trim_end_matches('/').to_string(), key })
}

fn target() -> Result<Target, &'static str> {
    target_from(std::env::var("NACHO_ASK_URL").ok(), descriptor_path().as_deref(), key_path().as_deref())
}

/// 같은 기계의 데스크톱 화면(할 일 판)이 나쵸 앱 창구를 직접 부를 때의 자리와 키. 폰 중계와
/// **같은** 서술자·키 파일이라야 두 화면이 같은 장부를 본다 — 따로 읽으면 한쪽만 옛 자리를 본다.
pub fn app_target() -> Result<(String, String), &'static str> {
    target().map(|t| (t.url, t.key))
}

pub struct NachoDesktopSession {
    credential: crate::device_auth::DeviceCred,
    stamp: crate::device_auth::Stamp,
}

impl NachoDesktopSession {
    pub fn capture() -> Option<Self> {
        crate::device_auth::capture().map(|(credential, stamp)| Self { credential, stamp })
    }

    pub fn stamp(&self) -> crate::device_auth::Stamp { self.stamp.clone() }

    pub fn is_current(&self) -> bool {
        crate::device_auth::with_current(&self.stamp, || Ok(())).is_ok()
    }

    /// Must run on a worker; desktop credentials are neither exposed to rendering nor copied to another device.
    pub fn request(&self, method: &str, path: &str, body: Option<&[u8]>) -> Result<(u16, Vec<u8>), &'static str> {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|_| "unavailable")?;
        let result = runtime.block_on(desktop_exchange(&self.credential, method, path, body,
            &|| crate::device_auth::with_current(&self.stamp, || Ok(())).map_err(|_| "account_changed")));
        if matches!(result, Ok((401, _))) { crate::device_auth::reject(&self.stamp); }
        result
    }
}

fn desktop_path(method: &str, path: &str, body: Option<&[u8]>) -> Result<String, &'static str> {
    let rest = path.strip_prefix("/api/app/").ok_or("invalid_request")?;
    let (name, query) = rest.split_once('?').unwrap_or((rest, ""));
    let allowed = match (method, name) {
        ("GET", "events") => body.is_none() && query.split('&').filter(|s| !s.is_empty()).all(|part| {
            part.split_once('=').is_some_and(|(key, value)|
                matches!(key, "after" | "tail" | "limit" | "wait") && !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit())
                    && (key != "wait" || value == "0"))
        }),
        ("POST", "messages") => query.is_empty() && body.is_some_and(|body| body.len() <= BODY_LIMIT),
        ("GET", "tasks") => body.is_none() && matches!(query, "" | "scope=desk"),
        ("GET", name) if name.starts_with("tasks/") => body.is_none() && query.is_empty()
            && name.strip_prefix("tasks/").is_some_and(|id| !id.is_empty() && id.len() <= 200 && id.bytes().all(|b| b.is_ascii_alphanumeric())),
        _ => false,
    };
    if !allowed { return Err("invalid_request"); }
    Ok(format!("/relay/account/nacho/app/{rest}"))
}

async fn desktop_exchange(
    credential: &crate::device_auth::DeviceCred,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    authorize: &(dyn Fn() -> Result<(), &'static str> + Sync),
) -> Result<(u16, Vec<u8>), &'static str> {
    let path = desktop_path(method, path, body)?;
    authorize()?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .connect_timeout(std::time::Duration::from_secs(3))
        .redirect(reqwest::redirect::Policy::none())
        .build().map_err(|_| "unavailable")?;
    // The gateway derives owner and machine identity; clients cannot supply owner-assertion headers.
    let mut request = client.request(reqwest::Method::from_bytes(method.as_bytes()).map_err(|_| "invalid_request")?,
        format!("{}{}", credential.relay.trim_end_matches('/'), path))
        .bearer_auth(&credential.token)
        .header(reqwest::header::ACCEPT, "application/json");
    if let Some(body) = body {
        request = request.header(reqwest::header::CONTENT_TYPE, "application/json").body(body.to_vec());
    }
    let mut response = request.send().await.map_err(|_| "unreachable")?;
    authorize()?;
    let status = response.status().as_u16();
    if response.content_length().is_some_and(|length| length > 2 * 1024 * 1024) { return Err("response_too_large"); }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "unreachable")? {
        if bytes.len().saturating_add(chunk.len()) > 2 * 1024 * 1024 { return Err("response_too_large"); }
        bytes.extend_from_slice(&chunk);
    }
    authorize()?;
    Ok((status, bytes))
}

/// `<rest>` 가 앱 창구 안의 경로인가. 점 조각(`..`)이나 이상한 글자로 `/api/app` 밖(`/api/ask`)을
/// 가리키지 못하게 조각마다 본다.
pub(crate) fn valid_rest(rest: &str) -> bool {
    !rest.is_empty()
        && rest.len() <= 200
        && rest.split('/').all(|seg| {
            !seg.is_empty()
                && seg != "."
                && seg != ".."
                && seg.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
}

/// 나쵸에게 실어 보낼 헤더 — **여기서 지은 것뿐**이다. 폰 헤더는 content-type 하나만 옮긴다.
pub(crate) fn forward_headers(user: &MobileUser, machine: &str, key: &str, content_type: Option<&HeaderValue>) -> HeaderMap {
    let mut h = HeaderMap::new();
    let mut put = |name: &'static str, value: &str| {
        if let Ok(v) = HeaderValue::from_str(value) {
            h.insert(name, v);
        }
    };
    put("x-nacho-token", key);
    put("x-kasa-owner", if user.owner { "1" } else { "0" });
    // 이름은 사람이 붙인 것이라 헤더에 못 싣는 글자(한글)가 있다 — 퍼센트로 옮긴다.
    put("x-kasa-user", &percent(&user.name));
    put("x-kasa-machine", machine);
    put("x-journal-request", "1");
    if let Some(ct) = content_type {
        h.insert(header::CONTENT_TYPE, ct.clone());
    }
    h
}

fn percent(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    if out.is_empty() { "owner".into() } else { out }
}

fn err(status: StatusCode, code: &str) -> Response {
    (status, axum::Json(serde_json::json!({ "ok": false, "error": code }))).into_response()
}

fn client() -> &'static reqwest::Client {
    static C: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(TIMEOUT_SECS))
            .build()
            .expect("reqwest client")
    })
}

pub(crate) async fn relay(
    user: Option<MobileUser>,
    method: Method,
    rest: &str,
    query: Option<&str>,
    headers: &HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let Some(user) = user.filter(|u| u.owner) else {
        return err(StatusCode::FORBIDDEN, "owner_only");
    };
    if method != Method::GET && method != Method::POST {
        return err(StatusCode::METHOD_NOT_ALLOWED, "method");
    }
    if !valid_rest(rest) {
        return err(StatusCode::NOT_FOUND, "bad_path");
    }
    let t = match target() {
        Ok(t) => t,
        Err(code) => return err(StatusCode::SERVICE_UNAVAILABLE, code),
    };
    send(&t, &user, method, rest, query, headers, body).await
}

pub(crate) async fn send(
    t: &Target,
    user: &MobileUser,
    method: Method,
    rest: &str,
    query: Option<&str>,
    headers: &HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let machine = crate::mobile::machine_identity().unwrap_or_default();
    let q = query.filter(|q| !q.is_empty()).map(|q| format!("?{q}")).unwrap_or_default();
    let url = format!("{}/api/app/{rest}{q}", t.url);
    let fwd = forward_headers(user, &machine, &t.key, headers.get(header::CONTENT_TYPE));
    let resp = match client().request(method, &url).headers(fwd).body(body).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[nacho-relay] 나쵸에 못 닿았습니다: {}", e.without_url());
            return err(StatusCode::BAD_GATEWAY, "nacho_unreachable");
        }
    };
    let mut out = Response::builder().status(resp.status().as_u16());
    for name in [header::CONTENT_TYPE, header::CACHE_CONTROL, header::CONTENT_LENGTH] {
        if let Some(v) = resp.headers().get(&name) {
            out = out.header(name, v);
        }
    }
    use futures_util::TryStreamExt as _;
    let stream = resp.bytes_stream().map_err(std::io::Error::other);
    out.body(axum::body::Body::from_stream(stream))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
}

/// 나쵸 키가 없는 기기의 데스크톱이 작업 모드·권한 표를 **읽기만** 하는 고정 창구(`/nacho/read/<이름>`,
/// docs/nacho-read-relay.md). 쓰는 길은 없다 — 키 없는 기기는 모드를 바꾸지 못한다.
///
/// 진짜 경계는 여기다. 허용목록 둘, GET 만, 로컬 호출만(기기 사이 SSH 터널은 로컬로 온다), 폰 주소
/// (`/u/<slug>`)로는 안 연다. 호출자 헤더·쿼리·몸통은 하나도 옮기지 않고 나쵸의 읽기 범위
/// (`X-Kasa-Read: 1`, 나쵸 desk-api.md)로 새로 짓는다 — 나쵸의 read_only 거절은 그 뒤의 이중 방어다.
pub(crate) const READ_PATHS: [&str; 2] = ["work-mode", "capabilities"];

pub(crate) fn read_headers(key: &str, machine: &str) -> HeaderMap {
    let mut h = HeaderMap::new();
    let mut put = |name: &'static str, value: &str| {
        if let Ok(v) = HeaderValue::from_str(value) {
            h.insert(name, v);
        }
    };
    put("x-nacho-token", key);
    put("x-kasa-read", "1");
    put("x-kasa-owner", "0");
    put("x-kasa-user", "relay-read");
    put("x-kasa-machine", machine);
    h
}

/// 읽기 범위 응답에서 사람 이름을 한 번 더 뺀다 — 나쵸도 빼지만 옛 판·실수에 기대지 않는다.
fn minimize(name: &str, body: &[u8]) -> Vec<u8> {
    let Ok(mut v) = serde_json::from_slice::<serde_json::Value>(body) else {
        return body.to_vec();
    };
    if name == "work-mode" {
        if let Some(m) = v.get_mut("work_mode").and_then(|m| m.as_object_mut()) {
            m.remove("changed_by");
        }
    }
    serde_json::to_vec(&v).unwrap_or_else(|_| body.to_vec())
}

pub(crate) async fn read_relay(method: &Method, name: &str, remote: bool, phone: bool) -> Response {
    read_relay_to(target(), method, name, remote, phone).await
}

pub(crate) async fn read_relay_to(
    t: Result<Target, &'static str>,
    method: &Method,
    name: &str,
    remote: bool,
    phone: bool,
) -> Response {
    if remote || phone {
        return err(StatusCode::FORBIDDEN, "local_only");
    }
    if method != Method::GET {
        return err(StatusCode::METHOD_NOT_ALLOWED, "method");
    }
    if !READ_PATHS.contains(&name) {
        return err(StatusCode::NOT_FOUND, "bad_path");
    }
    let t = match t {
        Ok(t) => t,
        Err(code) => return err(StatusCode::SERVICE_UNAVAILABLE, code),
    };
    let machine = crate::mobile::machine_identity().unwrap_or_default();
    let url = format!("{}/api/app/{name}", t.url);
    let resp = match client().get(&url).headers(read_headers(&t.key, &machine)).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[nacho-read] 나쵸에 못 닿았습니다: {}", e.without_url());
            return err(StatusCode::BAD_GATEWAY, "nacho_unreachable");
        }
    };
    let status = resp.status().as_u16();
    let body = match resp.bytes().await {
        Ok(b) if b.len() <= BODY_LIMIT => minimize(name, &b),
        _ => return err(StatusCode::BAD_GATEWAY, "nacho_reply_unreadable"),
    };
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CACHE_CONTROL, "no-store")
        .body(axum::body::Body::from(body))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn owner() -> MobileUser {
        MobileUser { name: "주인".into(), slug: "s".repeat(20), created: 0, owner: true }
    }

    #[test]
    fn rest_stays_inside_the_app_window() {
        assert!(valid_rest("messages"));
        assert!(valid_rest("tasks/w1a2b3c4/shot"));
        assert!(valid_rest("files/12/0"));
        for bad in ["", "../ask", "tasks/../../ask", "tasks/./x", "a//b", "a%2e%2e", "a b", "x?y"] {
            assert!(!valid_rest(bad), "{bad}");
        }
    }

    #[test]
    fn target_is_closed_without_descriptor_or_key() {
        let dir = std::env::temp_dir().join(format!("nacho-relay-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let desc = dir.join("nacho-ask.json");
        let key = dir.join("nacho-app.key");
        assert_eq!(target_from(None, Some(&desc), Some(&key)), Err("nacho_unconfigured"));
        std::fs::write(&desc, r#"{"version":1,"url":"http://127.0.0.1:8792/"}"#).unwrap();
        assert_eq!(target_from(None, Some(&desc), Some(&key)), Err("nacho_key_missing"));
        std::fs::write(&key, "  \n").unwrap();
        assert_eq!(target_from(None, Some(&desc), Some(&key)), Err("nacho_key_missing"));
        std::fs::write(&key, "k-123\n").unwrap();
        assert_eq!(
            target_from(None, Some(&desc), Some(&key)),
            Ok(Target { url: "http://127.0.0.1:8792".into(), key: "k-123".into() })
        );
        assert_eq!(
            target_from(Some("http://100.1.2.3:8792".into()), Some(&desc), Some(&key)).map(|t| t.url),
            Ok("http://100.1.2.3:8792".into())
        );
        assert_eq!(target_from(Some("file:///etc".into()), None, Some(&key)), Err("nacho_unconfigured"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn forwarded_identity_is_ours_not_the_phones() {
        let h = forward_headers(&owner(), "mid-1", "k-123", Some(&HeaderValue::from_static("application/json")));
        assert_eq!(h.get("x-kasa-owner").unwrap(), "1");
        assert_eq!(h.get("x-kasa-user").unwrap(), "%EC%A3%BC%EC%9D%B8");
        assert_eq!(h.get("x-nacho-token").unwrap(), "k-123");
        assert_eq!(h.get("x-kasa-machine").unwrap(), "mid-1");
        assert_eq!(h.get("x-journal-request").unwrap(), "1");
        assert_eq!(h.len(), 6);
    }

    #[tokio::test]
    async fn relay_strips_forged_headers_and_refuses_non_owners() {
        use axum::routing::any;
        let seen: Arc<Mutex<Vec<(String, HeaderMap)>>> = Arc::default();
        let log = seen.clone();
        let app = axum::Router::new().route(
            "/api/app/{*rest}",
            any(move |uri: axum::http::Uri, h: HeaderMap| {
                let log = log.clone();
                async move {
                    log.lock().unwrap().push((uri.to_string(), h));
                    axum::Json(serde_json::json!({"ok": true}))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let t = Target { url: format!("http://{addr}"), key: "k-real".into() };

        let mut forged = HeaderMap::new();
        forged.insert("x-kasa-owner", HeaderValue::from_static("1"));
        forged.insert("x-kasa-user", HeaderValue::from_static("mallory"));
        forged.insert("x-nacho-token", HeaderValue::from_static("guessed"));
        forged.insert("cookie", HeaderValue::from_static("kasa_token=x"));
        forged.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let r = send(&t, &owner(), Method::POST, "messages", Some("a=1"), &forged, "{}".into()).await;
        assert_eq!(r.status(), StatusCode::OK);
        let (uri, h) = seen.lock().unwrap().pop().unwrap();
        assert_eq!(uri, "/api/app/messages?a=1");
        assert_eq!(h.get("x-nacho-token").unwrap(), "k-real");
        assert_eq!(h.get("x-kasa-user").unwrap(), "%EC%A3%BC%EC%9D%B8");
        assert!(h.get("cookie").is_none());
        assert_eq!(h.get(header::CONTENT_TYPE).unwrap(), "application/json");

        let guest = MobileUser { owner: false, ..owner() };
        let r = relay(Some(guest), Method::GET, "state", None, &HeaderMap::new(), Default::default()).await;
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
        let r = relay(None, Method::GET, "state", None, &HeaderMap::new(), Default::default()).await;
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
        let r = relay(Some(owner()), Method::GET, "../ask", None, &HeaderMap::new(), Default::default()).await;
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
        let r = relay(Some(owner()), Method::DELETE, "state", None, &HeaderMap::new(), Default::default()).await;
        assert_eq!(r.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert!(seen.lock().unwrap().is_empty(), "거절한 요청은 나쵸에 닿지 않는다");
    }

    /// 실제 나쵸에 읽기 범위로 GET 둘만 — 모드·승인·장부는 건드리지 않는다. `cargo test -p kasa-mcp live_read_relay -- --ignored`
    #[tokio::test]
    #[ignore]
    async fn live_read_relay_reads_mode_and_capabilities_only() {
        let r = read_relay(&Method::GET, "work-mode", false, false).await;
        assert_eq!(r.status(), StatusCode::OK);
        let v: serde_json::Value = serde_json::from_slice(&axum::body::to_bytes(r.into_body(), BODY_LIMIT).await.unwrap()).unwrap();
        assert!(matches!(v["work_mode"]["mode"].as_str(), Some("organize" | "coordinate")), "{v}");
        assert!(v["work_mode"]["rev"].is_number());
        assert!(v["work_mode"].get("changed_by").is_none());
        eprintln!("[live] mode={} rev={}", v["work_mode"]["mode"], v["work_mode"]["rev"]);
        let r = read_relay(&Method::GET, "capabilities", false, false).await;
        assert_eq!(r.status(), StatusCode::OK);
        let v: serde_json::Value = serde_json::from_slice(&axum::body::to_bytes(r.into_body(), BODY_LIMIT).await.unwrap()).unwrap();
        assert_eq!(v["policy"]["unlimited_mode"], false);
        eprintln!("[live] tiers={}", v["policy"]["tiers"].as_array().map_or(0, |t| t.len()));
    }

    #[test]
    fn read_scope_headers_are_ours_and_never_claim_the_owner() {
        let h = read_headers("k-123", "mid-1");
        assert_eq!(h.get("x-nacho-token").unwrap(), "k-123");
        assert_eq!(h.get("x-kasa-read").unwrap(), "1");
        assert_eq!(h.get("x-kasa-owner").unwrap(), "0");
        assert_eq!(h.get("x-kasa-user").unwrap(), "relay-read");
        assert_eq!(h.get("x-kasa-machine").unwrap(), "mid-1");
        assert_eq!(h.len(), 5, "content-type·journal 표시 없음 — 쓰기 모양을 만들지 않는다");
    }

    #[tokio::test]
    async fn read_relay_opens_two_fixed_gets_and_nothing_else() {
        use axum::routing::any;
        let seen: Arc<Mutex<Vec<(String, String, HeaderMap)>>> = Arc::default();
        let log = seen.clone();
        let app = axum::Router::new().route(
            "/api/app/{*rest}",
            any(move |m: Method, uri: axum::http::Uri, h: HeaderMap| {
                let log = log.clone();
                async move {
                    log.lock().unwrap().push((m.to_string(), uri.to_string(), h));
                    axum::Json(serde_json::json!({"ok": true, "work_mode": {
                        "schema": "nacho-work-mode/1", "mode": "organize", "rev": 2,
                        "changed_at_ms": 5, "changed_by": "app:%EC%A3%BC%EC%9D%B8"}}))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let t = || Ok(Target { url: format!("http://{addr}"), key: "k-real".into() });

        let r = read_relay_to(t(), &Method::GET, "work-mode", false, false).await;
        assert_eq!(r.status(), StatusCode::OK);
        let body = axum::body::to_bytes(r.into_body(), BODY_LIMIT).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["work_mode"]["mode"], "organize");
        assert_eq!(v["work_mode"]["rev"], 2);
        assert!(v["work_mode"].get("changed_by").is_none(), "사람 이름은 넘기지 않는다");
        let (method, uri, h) = seen.lock().unwrap().pop().unwrap();
        assert_eq!((method.as_str(), uri.as_str()), ("GET", "/api/app/work-mode"));
        assert_eq!(h.get("x-kasa-read").unwrap(), "1");
        assert_eq!(h.get("x-kasa-owner").unwrap(), "0");
        assert_eq!(h.get("x-nacho-token").unwrap(), "k-real");

        let r = read_relay_to(t(), &Method::GET, "capabilities", false, false).await;
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(seen.lock().unwrap().pop().unwrap().1, "/api/app/capabilities");

        for (method, name, remote, phone, want) in [
            (Method::POST, "work-mode", false, false, StatusCode::METHOD_NOT_ALLOWED),
            (Method::GET, "tasks", false, false, StatusCode::NOT_FOUND),
            (Method::GET, "events", false, false, StatusCode::NOT_FOUND),
            (Method::GET, "messages", false, false, StatusCode::NOT_FOUND),
            (Method::GET, "approvals/ap_1", false, false, StatusCode::NOT_FOUND),
            (Method::GET, "../ask", false, false, StatusCode::NOT_FOUND),
            (Method::GET, "work-mode", true, false, StatusCode::FORBIDDEN),
            (Method::GET, "work-mode", false, true, StatusCode::FORBIDDEN),
        ] {
            let r = read_relay_to(t(), &method, name, remote, phone).await;
            assert_eq!(r.status(), want, "{method} {name} remote={remote} phone={phone}");
        }
        let r = read_relay_to(Err("nacho_key_missing"), &Method::GET, "work-mode", false, false).await;
        assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(seen.lock().unwrap().is_empty(), "거절한 요청은 나쵸에 닿지 않는다");
    }

    #[test]
    fn desktop_chat_route_is_narrow_and_longpoll_is_not_allowed() {
        assert_eq!(desktop_path("GET", "/api/app/events?after=4&limit=500&wait=0", None).unwrap(), "/relay/account/nacho/app/events?after=4&limit=500&wait=0");
        assert!(desktop_path("GET", "/api/app/tasks?scope=desk", None).is_ok());
        assert!(desktop_path("GET", "/api/app/tasks/a123", None).is_ok());
        for path in ["/api/app/approvals/ap_1", "/api/app/../ask", "/api/app/events?wait=25", "/api/app/events?token=123", "/api/app/tasks/../ask", "/api/app/messages"] {
            assert!(desktop_path("GET", path, None).is_err(), "{path}");
        }
        assert!(desktop_path("POST", "/api/app/events", Some(b"{}")).is_err());
        assert!(desktop_path("POST", "/api/app/messages?owner=1", Some(b"{}")).is_err());
        assert!(NachoDesktopSession::capture().is_none(), "unit tests must not read real account credentials");
    }

    #[tokio::test]
    async fn desktop_chat_uses_account_bearer_without_owner_assertions_and_preserves_retry_body() {
        let seen: Arc<Mutex<Vec<(String, HeaderMap, Vec<u8>)>>> = Arc::default();
        let log = seen.clone();
        let app = axum::Router::new().route("/relay/account/nacho/app/{*rest}", axum::routing::any(
            move |uri: axum::http::Uri, headers: HeaderMap, body: axum::body::Bytes| {
                let log = log.clone();
                async move {
                    log.lock().unwrap().push((uri.to_string(), headers, body.to_vec()));
                    axum::Json(serde_json::json!({"ok": true, "receipt": {"state":"accepted"}}))
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let credential = crate::device_auth::DeviceCred {
            relay: format!("http://{address}"), account: "fixture".into(), device_id: "fixture-device".into(), token: "fixture-token".into(),
        };
        let body = br#"{"id":"stable-id","text":"fixture","task":"a1","rev":"3"}"#;
        for _ in 0..2 {
            assert_eq!(desktop_exchange(&credential, "POST", "/api/app/messages", Some(body), &|| Ok(())).await.unwrap().0, 200);
        }
        let observed = seen.lock().unwrap();
        assert_eq!(observed.len(), 2);
        for (uri, headers, got) in observed.iter() {
            assert_eq!(uri, "/relay/account/nacho/app/messages");
            assert_eq!(headers.get("authorization").unwrap(), "Bearer fixture-token");
            for name in ["x-kasa-owner", "x-kasa-user", "x-nacho-token", "x-kasa-read"] { assert!(headers.get(name).is_none(), "{name}"); }
            assert_eq!(got, body);
        }
        drop(observed);
        server.abort();
    }

    #[tokio::test]
    async fn desktop_chat_discards_responses_after_account_change_and_does_not_follow_redirects() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let app = axum::Router::new().route("/relay/account/nacho/app/events", axum::routing::get(move || {
            count.fetch_add(1, Ordering::SeqCst);
            async { (StatusCode::TEMPORARY_REDIRECT, [("location", "/other")], "redirect") }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let credential = crate::device_auth::DeviceCred {
            relay: format!("http://{address}"), account: "fixture".into(), device_id: "fixture-device".into(), token: "fixture-token".into(),
        };
        let (status, _) = desktop_exchange(&credential, "GET", "/api/app/events?wait=0", None, &|| Ok(())).await.unwrap();
        assert_eq!(status, 307);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let checks = AtomicUsize::new(0);
        assert_eq!(desktop_exchange(&credential, "GET", "/api/app/events?wait=0", None,
            &|| if checks.fetch_add(1, Ordering::SeqCst) == 0 { Ok(()) } else { Err("account_changed") }).await, Err("account_changed"));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(desktop_exchange(&credential, "GET", "/api/app/events?wait=0", None, &|| Err("account_changed")).await, Err("account_changed"));
        assert_eq!(calls.load(Ordering::SeqCst), 2, "a stale identity cannot initiate a request");
        server.abort();
    }
}
