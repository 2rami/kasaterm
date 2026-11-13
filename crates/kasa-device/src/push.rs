//! 폰 푸시(APNs) — 학생이 승인·질문을 기다리거나 일을 끝냈을 때, 쪽지가 왔을 때
//! 아이폰 앱으로 알린다(2026-09-09 지시 「알림기능도 돼? … 만들어」).
//!
//! 재료 셋: ①폰이 `POST /term/push-token` 으로 맡긴 기기 토큰(`push_tokens.json`,
//! 0600) ②애플 푸시 열쇠(`~/.config/kasaterm/apns/key.json` — key_id·team_id·p8,
//! 앱스토어 API 열쇠와 **다른** 열쇠다) ③언제 울릴지 — 승인·질문 기다림·막힘·정말 다 끝남뿐이고, 판정과 문구는
//! 데스크톱 알림과 같은 `kasa_agents::ring` 이다. 이 기계 칸은 앱의 판정이 맥 알림과 같은 사건으로 [`send_detached`]
//! 를 부르고, 명부 기계 칸은 `push_loop` 가 그 기계 행(`/term/panes` 캐시)을 같은 규칙으로 본다.
//!
//! 계정 모드 폰은 데스크톱에 토큰을 맡기지 않고 관문에만 맡긴다(폰 `push.dart` `bindAccount`) — 이 기기엔
//! 토큰도 열쇠도 없을 수 있다. 그래서 이 기계 학생의 알림은 관문 로그인이 있으면 관문(`POST /relay/push/alert`)에도
//! 올리고, 관문이 계정 폰들에 쏜다. 명부 기계 행은 올리지 않는다 — 그 기계가 계정 기기면 스스로 올려 두 번 온다.
//! 계정 폰의 알림 확장은 이 기기의 `/term/avatar` 를 못 여니 학생 얼굴은 관문에 맡긴다(`push/face.rs`).
//!
//! APNs 는 HTTP/2 + ES256 JWT(`iss`=팀, `kid`=열쇠) 를 요구한다. 서명은 ring 으로
//! 직접 한다 — jsonwebtoken 을 더 들이지 않으려고. 토큰은 50분마다 새로 만든다
//! (애플은 1시간 넘은 것을 거절하고, 너무 잦은 갱신도 막는다).
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use kasa_agents::ring as call;

pub mod face;

/// 앱의 번들 id — `apns-topic`. 폰 프로젝트(mobile/ios)와 같아야 한다.
pub const TOPIC: &str = "com.debimarlene.kasaterm";
const TICK: Duration = Duration::from_secs(4);
/// 같은 pane 의 대기를 연달아 쏘지 않는 최소 간격 — 대기↔바쁨을 튀는 스피너 오판이 있다.
const MIN_GAP: Duration = Duration::from_secs(15);
const JWT_TTL: Duration = Duration::from_secs(50 * 60);

#[derive(Clone, Serialize, Deserialize)]
pub struct DeviceToken {
    pub token: String,
    /// `prod`(TestFlight·앱스토어) / `dev`(Xcode 디버그 빌드 — 샌드박스 서버).
    #[serde(default = "default_env")]
    pub env: String,
    #[serde(default)]
    pub user: String,
    /// 폰이 쓰는 서버 주소(`https://…/u/<slug>/`) — 알림 확장이 학생 얼굴을 받아 올
    /// 절대 주소를 여기서 만든다(확장은 앱의 열쇠고리를 못 본다).
    #[serde(default)]
    pub root: String,
    #[serde(default)]
    pub added: u64,
}

fn default_env() -> String {
    "prod".into()
}

#[derive(Deserialize)]
struct ApnsKey {
    key_id: String,
    team_id: String,
    p8: String,
}

fn config_dir() -> Option<PathBuf> {
    kasa_socket::home_dir().map(|h| h.join(".config").join("kasaterm"))
}

fn tokens_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("KASATERM_PUSH_TOKENS") {
        return Some(PathBuf::from(p));
    }
    config_dir().map(|d| d.join("push_tokens.json"))
}

pub fn tokens() -> Vec<DeviceToken> {
    tokens_path()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_tokens(list: &[DeviceToken]) {
    let Some(path) = tokens_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, serde_json::to_vec_pretty(list).unwrap_or_default()).is_ok() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        }
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// 폰이 맡긴 토큰을 적는다 — 같은 토큰이면 env·user 만 갱신.
pub fn register(token: &str, env: &str, user: &str, root: &str) -> usize {
    let token = token.trim();
    if token.is_empty() || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return tokens().len();
    }
    let env = if env == "dev" { "dev" } else { "prod" };
    let mut list = tokens();
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    match list.iter_mut().find(|t| t.token == token) {
        Some(t) => {
            t.env = env.into();
            if !user.is_empty() {
                t.user = user.into();
            }
            if !root.is_empty() {
                t.root = root.into();
            }
        }
        None => list.push(DeviceToken {
            token: token.into(),
            env: env.into(),
            user: user.into(),
            root: root.into(),
            added: now,
        }),
    }
    save_tokens(&list);
    list.len()
}

pub fn unregister(token: &str) {
    let mut list = tokens();
    list.retain(|t| t.token != token);
    save_tokens(&list);
}

/// 폰이 말한 환경이 틀렸을 때(개발 서명 판이 「prod」라 한 것) 실제로 받아 준 쪽으로 고쳐 적는다.
fn set_env(token: &str, env: &str) {
    let mut list = tokens();
    if let Some(t) = list.iter_mut().find(|t| t.token == token) {
        t.env = env.into();
        save_tokens(&list);
    }
}

fn host_of(env: &str) -> &'static str {
    if env == "dev" {
        "https://api.sandbox.push.apple.com"
    } else {
        "https://api.push.apple.com"
    }
}

/// 한 통을 한 서버에. 실패는 (상태, 애플이 준 본문).
async fn post_once(
    env: &str,
    token: &str,
    bearer: &str,
    payload: &Value,
    collapse: Option<&str>,
) -> Result<(), (reqwest::StatusCode, String)> {
    let mut req = client()
        .post(format!("{}/3/device/{token}", host_of(env)))
        .header("authorization", format!("bearer {bearer}"))
        .header("apns-topic", TOPIC)
        .header("apns-push-type", "alert")
        .header("apns-priority", "10")
        .json(payload);
    if let Some(c) = collapse {
        req = req.header("apns-collapse-id", c.chars().take(64).collect::<String>());
    }
    match req.send().await {
        Ok(r) if r.status().is_success() => Ok(()),
        Ok(r) => {
            let status = r.status();
            Err((status, r.text().await.unwrap_or_default()))
        }
        Err(e) => Err((reqwest::StatusCode::default(), e.to_string())),
    }
}

fn key_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("apns").join("key.json"))
}

fn load_key() -> Option<ApnsKey> {
    let env = (
        std::env::var("KASATERM_APNS_KEY_ID").ok(),
        std::env::var("KASATERM_APNS_TEAM_ID").ok(),
        std::env::var("KASATERM_APNS_KEY_PATH").ok(),
    );
    if let (Some(key_id), Some(team_id), Some(p8)) = env {
        return Some(ApnsKey { key_id, team_id, p8 });
    }
    let bytes = std::fs::read(key_path()?).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// 열쇠가 있어 쏠 수 있나 — 설정 화면·상태줄이 「알림 준비됨」을 말할 때.
pub fn configured() -> bool {
    load_key().is_some_and(|k| std::fs::metadata(expand(&k.p8)).is_ok())
}

fn expand(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(h) = kasa_socket::home_dir() {
            return h.join(rest);
        }
    }
    PathBuf::from(p)
}

fn b64url(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// `.p8`(PEM PKCS#8) → ring 키. 애플 열쇠는 공개키가 함께 들어 있어 ring 이 그대로 읽는다.
fn signing_key(p8: &str) -> Result<ring::signature::EcdsaKeyPair, String> {
    use base64::Engine;
    let pem = std::fs::read_to_string(expand(p8)).map_err(|e| format!("열쇠 파일: {e}"))?;
    let body: String = pem
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .map(str::trim)
        .collect();
    let der = base64::engine::general_purpose::STANDARD
        .decode(body)
        .map_err(|e| format!("열쇠 PEM: {e}"))?;
    let rng = ring::rand::SystemRandom::new();
    ring::signature::EcdsaKeyPair::from_pkcs8(
        &ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING,
        &der,
        &rng,
    )
    .map_err(|e| format!("열쇠 형식(PKCS#8 EC 여야 함): {e}"))
}

fn jwt_cache() -> &'static Mutex<Option<(Instant, String)>> {
    static C: OnceLock<Mutex<Option<(Instant, String)>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(None))
}

fn jwt(key: &ApnsKey) -> Result<String, String> {
    if let Ok(c) = jwt_cache().lock() {
        if let Some((at, tok)) = c.as_ref() {
            if at.elapsed() < JWT_TTL {
                return Ok(tok.clone());
            }
        }
    }
    let pair = signing_key(&key.p8)?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let header = b64url(serde_json::json!({ "alg": "ES256", "kid": key.key_id }).to_string().as_bytes());
    let claims = b64url(serde_json::json!({ "iss": key.team_id, "iat": now }).to_string().as_bytes());
    let signing_input = format!("{header}.{claims}");
    let rng = ring::rand::SystemRandom::new();
    let sig = pair
        .sign(&rng, signing_input.as_bytes())
        .map_err(|e| format!("서명: {e}"))?;
    let tok = format!("{signing_input}.{}", b64url(sig.as_ref()));
    if let Ok(mut c) = jwt_cache().lock() {
        *c = Some((Instant::now(), tok.clone()));
    }
    Ok(tok)
}

fn client() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .http2_prior_knowledge()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_default()
    })
}

/// 알림 한 통. `machine` 은 명부 route(이 기계면 None), `pane` 은 surface id —
/// 폰이 누르면 그 학생 화면으로 간다(딥링크와 같은 재료). `collapse` 가 같으면
/// 잠금화면에서 앞 것을 갈아치운다(같은 pane 의 대기 알림이 쌓이지 않게).
pub struct Alert {
    pub title: String,
    pub body: String,
    pub machine: Option<String>,
    pub pane: String,
    pub kind: String,
    pub collapse: Option<String>,
    /// 보낸 학생 이름·얼굴 슬러그 — 폰의 알림 확장이 「학생이 보낸 메시지」 모양
    /// (아이콘 자리에 얼굴)으로 바꾼다. 없으면 보통 알림.
    pub sender: Option<String>,
    pub avatar_slug: Option<String>,
    /// 학생이 보여 주려 연 페이지 — 폰이 알림을 누르면 pane 대신 이 주소를 연다.
    pub url: Option<String>,
}

fn notification_thread(alert: &Alert) -> String {
    format!("{}/{}", alert.machine.as_deref().unwrap_or("local"), alert.pane)
}

/// 이 기기에 맡긴 폰 전부 + (이 기계 학생이면) 관문을 거쳐 계정 폰 전부. 받은 폰 수.
pub async fn send(alert: &Alert) -> usize {
    let mut sent = send_local(alert).await;
    if alert.machine.is_none() {
        sent += send_account(alert).await;
    }
    sent
}

/// 관문에 올릴 몸통. 관문은 상한을 넘거나 제어 문자가 든 몸통을 통째로 거절하니(알림이 사라진다) 여기서 맞춰
/// 다듬는다 — 쪽지 요약엔 줄바꿈이 든다. 얼굴은 그림의 해시만 싣는다. 해시가 없으면 `face` 키도 없다 — 옛 관문은
/// 모르는 키가 든 몸통을 거절한다(`deny_unknown_fields`).
pub fn relay_body(alert: &Alert, face: Option<&str>) -> Value {
    let clip = |text: &str, max: usize| -> String {
        text.chars().map(|c| if c.is_control() { ' ' } else { c }).take(max).collect()
    };
    let url = alert.url.as_deref().filter(|u| {
        u.len() <= 2048 && (u.starts_with("https://") || u.starts_with("http://")) && !u.chars().any(char::is_control)
    });
    let mut body = serde_json::json!({
        "title": clip(&alert.title, 120),
        "body": clip(&alert.body, 180),
        "pane": alert.pane,
        "kind": alert.kind,
        "collapse": alert.collapse.as_deref().map(|c| clip(c, 64)),
        "sender": alert.sender.as_deref().map(|s| clip(s, 40)),
        "url": url,
    });
    if let Some(hash) = face {
        body["face"] = hash.into();
    }
    body
}

fn relay_client() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_default()
    })
}

/// 관문 로그인이 있으면 관문에 올린다. 답의 `phones` 는 토큰이 있는 계정 폰 수다.
async fn send_account(alert: &Alert) -> usize {
    let Some((cred, _)) = crate::device_auth::capture() else { return 0 };
    let face = alert.avatar_slug.as_deref().and_then(face::of);
    relay_alert(cred.relay.trim_end_matches('/'), &cred.token, alert, face.as_ref()).await
}

/// 관문에 알림 한 통. 얼굴은 해시만 먼저 싣고, 관문이 모른다고 하면(409 `face_unknown`) 그림을 올린 뒤 다시 보낸다.
/// 새 필드 때문에 알림이 사라지지 않게 — 그림을 못 올렸거나 관문이 얼굴 든 몸통을 거절하면(옛 관문은 400
/// `bad_request`) 얼굴 없이, 막힘 종류를 모르면(400 `bad_kind`) 끝냄으로 한 번 더 보낸다. 409·400 은 아무 폰에도 안
/// 쏜 답이라 다시 보내도 두 번 울리지 않는다.
pub async fn relay_alert(base: &str, token: &str, alert: &Alert, face: Option<&face::Face>) -> usize {
    static OLD_GATEWAY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let url = format!("{base}/relay/push/alert");
    let (mut face, mut uploaded, mut plain_kind) = (face, false, false);
    loop {
        let mut body = relay_body(alert, face.map(|f| f.hash.as_str()));
        if plain_kind {
            body["kind"] = "done".into();
        }
        let response = match relay_client().post(&url).bearer_auth(token).json(&body).send().await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[push] 관문에 못 올림: {e}");
                return 0;
            }
        };
        let status = response.status().as_u16();
        let value: Value = response.json().await.unwrap_or_default();
        match (status, face) {
            (200..=299, _) => return value["phones"].as_u64().unwrap_or(0) as usize,
            (409, Some(f)) if value["error"] == "face_unknown" && !uploaded => {
                uploaded = true;
                if !upload_face(base, token, f).await {
                    face = None;
                }
            }
            // 막힘 종류를 모르는 옛 관문 — 끝냄으로라도 간다(본문이 막힌 사연을 말한다).
            (400, _) if value["error"] == "bad_kind" && alert.kind == "waiting:blocked" && !plain_kind => plain_kind = true,
            (400 | 409, Some(_)) => {
                if value["error"] == "bad_request" {
                    eprintln!("[push] 관문이 옛 판이라 얼굴 없이 보냄(face 를 모름)");
                    face::pause();
                }
                face = None;
            }
            (401, _) if value["error"] == "unauthorized" => {
                crate::device_auth::reject_token(token);
                return 0;
            }
            // 창구가 없는 옛 관문 — 알림마다 같은 말을 쌓지 않는다.
            (404, _) => {
                if !OLD_GATEWAY.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    eprintln!("[push] 관문이 옛 판이라 계정 폰 알림을 못 올림(/relay/push/alert 없음)");
                }
                return 0;
            }
            _ => {
                eprintln!("[push] 관문이 알림을 거절함 ({status} {})", value["error"].as_str().unwrap_or(""));
                return 0;
            }
        }
    }
}

async fn upload_face(base: &str, token: &str, face: &face::Face) -> bool {
    let url = format!("{base}/relay/push/face/{}", face.hash);
    let sent = relay_client()
        .put(url)
        .bearer_auth(token)
        .header(reqwest::header::CONTENT_TYPE, "image/png")
        .body(face.png.clone())
        .send()
        .await;
    match sent {
        Ok(r) if r.status().is_success() => true,
        Ok(r) => {
            eprintln!("[push] 관문이 얼굴을 안 받음 ({})", r.status());
            false
        }
        Err(e) => {
            eprintln!("[push] 얼굴을 못 올림: {e}");
            false
        }
    }
}

/// 이 기기의 `push_tokens.json` 폰 전부에 쏜다. 열쇠나 토큰이 없으면 조용히 0.
async fn send_local(alert: &Alert) -> usize {
    let Some(key) = load_key() else { return 0 };
    let list = tokens();
    if list.is_empty() {
        return 0;
    }
    let bearer = match jwt(&key) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[push] 열쇠 문제로 못 보냄: {e}");
            return 0;
        }
    };
    let mut sent = 0;
    for t in list {
        // 얼굴 주소는 폰마다 다르다(자기 서버 주소 밑의 `term/avatar/<slug>.png`).
        let avatar = match (&alert.avatar_slug, t.root.trim_end_matches('/')) {
            (Some(slug), root) if !root.is_empty() => Some(format!("{root}/term/avatar/{slug}.png")),
            _ => None,
        };
        let payload = serde_json::json!({
            "aps": {
                "alert": { "title": alert.title, "body": alert.body },
                "sound": "default",
                "thread-id": notification_thread(alert),
                "mutable-content": 1,
            },
            "machine": alert.machine,
            "pane": alert.pane,
            "kind": alert.kind,
            "sender": alert.sender,
            "avatar": avatar,
            "url": alert.url,
        });
        let collapse = alert.collapse.as_deref();
        match post_once(&t.env, &t.token, &bearer, &payload, collapse).await {
            Ok(()) => sent += 1,
            Err((status, text)) if text.contains("BadDeviceToken") => {
                // 토큰은 환경마다 다르다 — 폰이 말한 환경이 틀린 것일 수 있다(개발 서명
                // 판은 release 로 구워도 샌드박스 토큰이다). 반대쪽에 한 번 더 쏴 보고,
                // 받아 주면 그쪽으로 고쳐 적는다. 양쪽 다 거절해야 진짜 죽은 토큰이다.
                let other = if t.env == "dev" { "prod" } else { "dev" };
                match post_once(other, &t.token, &bearer, &payload, collapse).await {
                    Ok(()) => {
                        eprintln!("[push] 환경 바로잡음 {}→{other} (폰이 잘못 말함)", t.env);
                        set_env(&t.token, other);
                        sent += 1;
                    }
                    Err((s2, t2)) => {
                        eprintln!("[push] {status} {text} / {s2} {t2} — 양쪽 다 거절, 토큰 걷음");
                        unregister(&t.token);
                    }
                }
            }
            Err((status, text)) => {
                eprintln!("[push] {status} {text} (env={})", t.env);
                // 지운 앱·옛 토큰은 다시 안 쏘게 걷는다(애플이 410 으로 말해 준다).
                if status.as_u16() == 410 || text.contains("Unregistered") {
                    unregister(&t.token);
                }
            }
        }
    }
    sent
}

/// 토큰 하나에 한 통 — 관문이 계정 폰에 원격 승인을 알릴 때(`gateway_approvals.rs`). 이 기기의
/// `push_tokens.json` 은 안 본다. 같은 `collapse` 의 앞 알림을 갈아 끼운다(소리·배너는 payload 가 정한다).
/// 실패는 (상태 코드, 애플이 준 본문) — 410·BadDeviceToken 이면 부른 쪽이 토큰을 걷는다.
pub async fn send_to(env: &str, token: &str, payload: &Value, collapse: &str) -> Result<(), (u16, String)> {
    let Some(key) = load_key() else { return Err((0, "no_key".into())) };
    let bearer = jwt(&key).map_err(|e| (0, e))?;
    post_once(env, token, &bearer, payload, Some(collapse))
        .await
        .map_err(|(status, text)| (status.as_u16(), text))
}

/// 쪽지 중 폰을 울릴 것 — 끝냄·실패·고를 것·브라우저에서 사람 손 기다림(`docs/chrome-mirror.md`),
/// 그리고 「이 폰」으로 보낸 페이지(울리지 않으면 종 목록을 열기 전엔 못 본다). 나머지는 종 목록에만 남는다.
fn note_rings(kind: &str) -> bool {
    matches!(kind, "done_ok" | "done_fail" | "browser" | "link") || call::wait_rings(kind)
}

/// 쪽지가 들어왔을 때 — `notes::add` 뒤에서 부른다.
pub fn note_arrived(character: &str, kind: &str, summary: &str, pane: &str, url: Option<&str>) {
    if !note_rings(kind) {
        return;
    }
    let alert = note_alert(character, kind, summary, pane, url);
    tokio::spawn(async move {
        send(&alert).await;
    });
}

/// 런타임 밖(GUI 스레드)에서 — `open` 이 폰으로 갈 때. 자기 런타임을 하나 세워 보낸다.
pub fn note_arrived_blocking(character: &str, kind: &str, summary: &str, pane: &str, url: Option<&str>) {
    if !note_rings(kind) {
        return;
    }
    let alert = note_alert(character, kind, summary, pane, url);
    if let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() {
        rt.block_on(send(&alert));
    }
}

/// GUI 스레드에서 기다리지 않고 보낸다 — 앱의 판정(맥 알림과 같은 사건)이 이 기계 칸의 알림을 폰에도 낼 때.
pub fn send_detached(alert: Alert) {
    let _ = std::thread::Builder::new().name("kasa-push".into()).spawn(move || {
        if let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() {
            rt.block_on(send(&alert));
        }
    });
}

/// 쪽지도 보낸 학생이 하는 말로 — 제목은 학생 이름, 본문은 쪽지 요약.
fn note_alert(character: &str, kind: &str, summary: &str, pane: &str, url: Option<&str>) -> Alert {
    let who = if character.is_empty() { "학생" } else { character };
    let said = call::spoken(summary);
    let body = match kind {
        _ if !said.is_empty() => said,
        "link" => "선생님, 이 페이지 봐 주세요.".to_string(),
        "permission" => call::ask("", None),
        "question" => call::question(""),
        "done_fail" => "선생님, 잘 안 됐어요.".to_string(),
        _ => "선생님, 다 끝났어요.".to_string(),
    };
    Alert {
        title: who.to_string(),
        body,
        machine: None,
        pane: pane.to_string(),
        kind: format!("note:{kind}"),
        // 페이지는 한 장씩 다 보여야 한다 — 같은 pane 의 두 번째 링크가 첫 번째를 덮으면
        // 사람은 하나만 받은 줄 안다.
        collapse: (kind != "link").then(|| format!("note-{pane}")),
        sender: (!character.is_empty()).then(|| character.to_string()),
        avatar_slug: kasa_characters::character::slug_for_any(character),
        url: url.filter(|u| !u.is_empty()).map(str::to_string),
    }
}

/// 다른 기계 칸 하나 — 끝남·막힘은 [`call::Finish`], 대기는 사람이 골라야 하는 대기로 들어선 순간.
#[derive(Default)]
struct RowWatch {
    finish: call::Finish,
    wait: Option<String>,
    waited_at: Option<Instant>,
}

/// 명부 기계 행들의 판정. 이 기계 칸은 여기서 보지 않는다 — 앱의 판정이 맥 알림과 같은 사건으로 폰에도 보낸다
/// (같은 칸을 두 곳이 판정하면 한쪽에만 오는 알림·두 번 오는 알림이 생겼다).
#[derive(Default)]
struct RemoteRows {
    seen: HashMap<String, RowWatch>,
}

impl RemoteRows {
    fn update(&mut self, rows: Vec<(String, Value)>, now: Instant) -> Vec<Alert> {
        let mut out = Vec::new();
        let mut alive = std::collections::HashSet::new();
        for (route, row) in rows {
            let Some(id) = row.get("id").and_then(Value::as_str) else { continue };
            let name = row.get("name").and_then(Value::as_str).unwrap_or("");
            if name.is_empty() || row.get("closed").and_then(Value::as_bool).unwrap_or(false) {
                continue;
            }
            let key = format!("{route}/{id}");
            alive.insert(key.clone());
            let first = !self.seen.contains_key(&key);
            let watch = self.seen.entry(key.clone()).or_default();
            let status = row.get("status").and_then(Value::as_str).unwrap_or("");
            let kind = row.get("kind").and_then(Value::as_str).unwrap_or("");
            let wait = (matches!(status, "waiting" | "blocked") && call::wait_rings(kind)).then(|| kind.to_string());
            // 대기↔바쁨을 튀는 스피너 오판이 있다 — 같은 칸의 대기는 15초 안에 다시 울리지 않는다.
            let entering = !first && wait.is_some() && watch.wait != wait
                && watch.waited_at.is_none_or(|at| now.saturating_duration_since(at) >= MIN_GAP);
            watch.wait = wait;
            let slug = row
                .get("slug")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| kasa_characters::character::slug_for_any(name));
            let alert = |body: String, kind: &str, collapse: String| Alert {
                title: format!("{name} ({route})"),
                body,
                machine: Some(route.clone()),
                pane: id.to_string(),
                kind: kind.to_string(),
                collapse: Some(collapse),
                sender: Some(name.to_string()),
                avatar_slug: slug.clone(),
                url: None,
            };
            if entering {
                watch.waited_at = Some(now);
                let reason = row.get("waiting_for").and_then(Value::as_str).unwrap_or("");
                let body = if kind == "permission" { call::ask(call::tool_in_reason(reason), None) } else { call::question(reason) };
                out.push(alert(body, &format!("waiting:{kind}"), format!("wait-{key}")));
            }
            let said = row.get("last_reply").and_then(Value::as_str);
            match watch.finish.update(&call::row_facts(&row), now) {
                Some(call::Ring::Finished { report, .. }) => {
                    out.push(alert(call::finished(report.as_ref(), said), "done", format!("done-{key}")));
                }
                Some(call::Ring::Blocked { report }) => {
                    out.push(alert(call::blocked(&report, said), "waiting:blocked", format!("blocked-{key}")));
                }
                None => {}
            }
        }
        self.seen.retain(|key, _| alive.contains(key));
        out
    }
}

/// 명부 기계의 pane 행 — (기계 route, 행). 거울 pane 은 몸통이 저쪽이라 저쪽 행이 따로 온다 — 두 번 안 쏘게 뺀다.
fn remote_rows() -> Vec<(String, Value)> {
    let mut out = Vec::new();
    for m in crate::machines::snapshot() {
        let Some(route) = m.get("route").and_then(Value::as_str) else { continue };
        for row in m.get("panes").and_then(Value::as_array).cloned().unwrap_or_default() {
            if row.get("mirror_of").and_then(Value::as_str).is_none() {
                out.push((route.to_string(), row));
            }
        }
    }
    out
}

/// 명부 기계 학생의 알림을 이 기기에 맡긴 폰에 쏜다 — 본체(PTY 를 가진 앱) 한 곳만 돈다. 계정 폰은 그 기계가 스스로
/// 관문에 올리므로 여기서는 이 기기 토큰으로만 보낸다(`send` 는 다른 기계 칸을 관문에 안 올린다). 첫 바퀴는 기준만
/// 잡는다(켜자마자 대기 중인 학생 전부가 한꺼번에 울리지 않게).
pub async fn push_loop() {
    let mut rows = RemoteRows::default();
    loop {
        tokio::time::sleep(TICK).await;
        if tokens().is_empty() || !configured() {
            continue;
        }
        for alert in rows.update(remote_rows(), Instant::now()) {
            let n = send(&alert).await;
            if n > 0 {
                eprintln!("[push] {} → 폰 {n}대", alert.title);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn same_pane_on_different_machines_has_separate_notifications() {
        let mut alert = super::Alert {
            title: String::new(), body: String::new(), machine: None,
            pane: "%3".into(), kind: "done".into(), collapse: None,
            sender: None, avatar_slug: None, url: None,
        };
        let local = super::notification_thread(&alert);
        alert.machine = Some("macbook".into());
        assert_ne!(local, super::notification_thread(&alert));
    }

    #[test]
    fn only_done_and_choices_ring() {
        for kind in ["done_ok", "done_fail", "permission", "question", "browser", "link"] {
            assert!(super::note_rings(kind), "{kind}");
        }
        for kind in ["waiting", "idle", "dead", "note", ""] {
            assert!(!super::note_rings(kind), "{kind}");
        }
        let note = super::note_alert("시로코", "done_ok", "## 결과\n**빌드** 끝났어요.", "%3", None);
        assert_eq!((note.title.as_str(), note.body.as_str()), ("시로코", "빌드 끝났어요."));
    }

    fn row(status: &str, end: Option<(&str, bool)>) -> serde_json::Value {
        let mut row = serde_json::json!({"id": "%3", "name": "시로코", "status": status});
        if let Some((completion, background)) = end {
            row["response_end"] = serde_json::json!({"surface_id": "%3", "session_id": "s1", "source": "a",
                "completion": completion, "idle": status == "idle", "error": false, "background": background});
        }
        row
    }

    /// 다른 기계 칸도 데스크톱과 같은 규칙이다 — 턴만 끝나고 bg 가 돌면 안 울리고, 다 끝난 순간 한 번.
    #[test]
    fn other_machine_rows_ring_by_the_shared_rule() {
        let mut rows = super::RemoteRows::default();
        let start = std::time::Instant::now();
        let at = |s: u64| start + std::time::Duration::from_secs(s);
        let feed = |rows: &mut super::RemoteRows, row: serde_json::Value, t| rows.update(vec![("mini".into(), row)], t);
        assert!(feed(&mut rows, row("working", Some(("", false))), at(0)).is_empty());
        let mut waiting = row("waiting", Some(("", false)));
        waiting["kind"] = "permission".into();
        waiting["waiting_for"] = "Bash 승인 대기".into();
        let asked = feed(&mut rows, waiting.clone(), at(4));
        assert_eq!(asked.len(), 1);
        assert_eq!((asked[0].title.as_str(), asked[0].body.as_str(), asked[0].kind.as_str()),
            ("시로코 (mini)", "선생님, 명령을 실행해도 될까요?", "waiting:permission"));
        assert!(feed(&mut rows, waiting, at(8)).is_empty(), "같은 대기는 한 번");

        assert!(feed(&mut rows, row("idle", Some(("0a", true))), at(12)).is_empty());
        assert!(feed(&mut rows, row("idle", Some(("0a", true))), at(30)).is_empty(), "bg 가 도는 동안은 끝이 아니다");
        assert!(feed(&mut rows, row("idle", Some(("0a", false))), at(34)).is_empty());
        let done = feed(&mut rows, row("idle", Some(("0a", false))), at(40));
        assert_eq!(done.len(), 1);
        assert_eq!((done[0].kind.as_str(), done[0].body.as_str()), ("done", "선생님, 다 끝났어요."));
        assert!(feed(&mut rows, row("idle", Some(("0a", false))), at(50)).is_empty());

        // 막힘은 그 턴이 끝난 뒤 학생의 답 글로 — 행의 `last_reply` 가 없으면 보고에서.
        let mut stuck = row("idle", Some(("0b", false)));
        stuck["done_outcome"] = "failed".into();
        stuck["done_summary"] = "승인 필요: 배포".into();
        stuck["done_at_ms"] = 99.into();
        assert!(feed(&mut rows, stuck.clone(), at(54)).is_empty());
        let blocked = feed(&mut rows, stuck.clone(), at(60));
        assert_eq!((blocked[0].kind.as_str(), blocked[0].body.as_str()), ("waiting:blocked", "선생님, 승인이 필요해요. 배포"));
        stuck["done_at_ms"] = 100.into();
        stuck["last_reply"] = "선생님, 배포해도 될까요? 시험은 다 통과했어요.".into();
        assert!(feed(&mut rows, stuck.clone(), at(64)).is_empty());
        let asked = feed(&mut rows, stuck, at(70));
        assert_eq!(asked[0].body, "선생님, 배포해도 될까요?");
    }

    #[test]
    fn the_relay_body_fits_the_gateway_and_names_a_face_only_by_hash() {
        let alert = super::Alert {
            title: "가".repeat(130),
            body: "첫 줄\n둘째 줄".repeat(40),
            machine: None,
            pane: "%3".into(),
            kind: "note:link".into(),
            collapse: None,
            sender: Some("시로코\t".repeat(10)),
            avatar_slug: Some("shiroko".into()),
            url: Some("javascript:alert(1)".into()),
        };
        let body = super::relay_body(&alert, None);
        assert_eq!(body["title"].as_str().unwrap().chars().count(), 120);
        let text = body["body"].as_str().unwrap();
        assert_eq!(text.chars().count(), 180);
        assert!(text.starts_with("첫 줄 둘째 줄"));
        assert_eq!(body["sender"].as_str().unwrap().chars().count(), 40);
        assert!(!body["sender"].as_str().unwrap().contains('\t'));
        assert!(body["url"].is_null() && body["collapse"].is_null());
        assert!(body.get("machine").is_none() && body.get("avatar").is_none());
        assert!(body.get("face").is_none(), "옛 관문은 모르는 키가 든 몸통을 통째로 거절한다");

        let hash = "ab".repeat(32);
        let page = super::Alert { url: Some("https://example.com/a".into()), ..alert };
        let body = super::relay_body(&page, Some(&hash));
        assert_eq!(body["url"], "https://example.com/a");
        assert_eq!(body["face"], hash.as_str());
        assert!(body.get("avatar").is_none());
    }

    /// 얼굴 필드를 모르는 옛 관문(`deny_unknown_fields`)에 보내도 알림은 얼굴 없이 한 번 간다.
    #[tokio::test]
    async fn an_old_gateway_still_gets_the_alert_without_the_face() {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        #[allow(dead_code)]
        struct OldBody {
            title: String,
            body: String,
            pane: String,
            kind: String,
            collapse: Option<String>,
            sender: Option<String>,
            url: Option<String>,
        }
        let accepted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = accepted.clone();
        let app = axum::Router::new().route(
            "/relay/push/alert",
            axum::routing::post(move |raw: axum::body::Bytes| {
                let seen = seen.clone();
                async move {
                    use axum::response::IntoResponse as _;
                    let old_kinds = ["done", "waiting:permission", "waiting:question"];
                    match serde_json::from_slice::<OldBody>(&raw) {
                        Ok(body) if !old_kinds.contains(&body.kind.as_str()) => (
                            axum::http::StatusCode::BAD_REQUEST,
                            axum::Json(serde_json::json!({"ok": false, "error": "bad_kind"})),
                        )
                            .into_response(),
                        Ok(_) => {
                            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            axum::Json(serde_json::json!({"ok": true, "phones": 2})).into_response()
                        }
                        Err(_) => (
                            axum::http::StatusCode::BAD_REQUEST,
                            axum::Json(serde_json::json!({"ok": false, "error": "bad_request"})),
                        )
                            .into_response(),
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let face = super::face::Face { hash: "ab".repeat(32), png: vec![1, 2, 3] };
        let alert = super::Alert {
            title: "시로코".into(), body: "끝났어요.".into(), machine: None,
            pane: "%3".into(), kind: "done".into(), collapse: None,
            sender: Some("시로코".into()), avatar_slug: Some("shiroko".into()), url: None,
        };
        assert_eq!(super::relay_alert(&base, "kasa_test", &alert, Some(&face)).await, 2);
        assert_eq!(accepted.load(std::sync::atomic::Ordering::SeqCst), 1, "한 번만 울린다");
        let stuck = super::Alert { kind: "waiting:blocked".into(), ..alert };
        assert_eq!(super::relay_alert(&base, "kasa_test", &stuck, Some(&face)).await, 2, "막힘은 끝냄으로라도 간다");
        assert_eq!(accepted.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    /// 진짜 열쇠가 있는 기계에서만 — ring 이 애플 .p8 을 읽고 서명하는지.
    #[test]
    fn apple_p8_signs_when_present() {
        let Some(key) = super::load_key() else { return };
        if std::fs::metadata(super::expand(&key.p8)).is_err() {
            return;
        }
        let tok = super::jwt(&key).expect("애플 .p8 로 ES256 서명");
        assert_eq!(tok.split('.').count(), 3);
    }

    /// 진짜 열쇠·토큰이 있는 기계에서, 앱과 같은 경로(ring JWT + reqwest h2)로 한 통 쏜다.
    /// 폰에 실제로 울리므로 `--ignored` 로만 — 「서버가 쏘는데 폰이 안 받나」를 가를 때.
    #[test]
    #[ignore]
    fn live_send_one_alert() {
        if !super::configured() || super::tokens().is_empty() {
            return;
        }
        let alert = super::Alert {
            title: "시로코 · 서버 경로 시험".into(),
            body: "앱과 같은 코드로 보낸 알림".into(),
            machine: None,
            pane: "%0".into(),
            kind: "test".into(),
            collapse: None,
            sender: Some("시로코".into()),
            avatar_slug: kasa_characters::character::slug_for_any("시로코"),
            url: None,
        };
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let n = rt.block_on(super::send(&alert));
        eprintln!("[live_send] 보낸 폰 수 = {n}");
        assert!(n > 0, "APNs 가 거절했거나 못 보냄 — stderr 의 [push] 줄을 봐라");
    }
}
