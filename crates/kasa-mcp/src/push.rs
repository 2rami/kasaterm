//! 폰 푸시(APNs) — 학생이 승인·질문을 기다리거나 일을 끝냈을 때, 쪽지가 왔을 때
//! 아이폰 앱으로 알린다(2026-09-09 지시 「알림기능도 돼? … 만들어」).
//!
//! 재료 셋: ①폰이 `POST /term/push-token` 으로 맡긴 기기 토큰(`push_tokens.json`,
//! 0600) ②애플 푸시 열쇠(`~/.config/kasaterm/apns/key.json` — key_id·team_id·p8,
//! 앱스토어 API 열쇠와 **다른** 열쇠다) ③상태 변화 감시 루프(`push_loop`) — 폰이 읽는
//! `/term/panes` 행(이 기계 + 명부 기계 캐시)을 4초마다 대조해 「승인·질문 대기로 들어감」
//! 「바쁨→끝냄」 순간에만 쏜다. 상태를 그대로 폴링해 쏘면 같은 대기가 매번 온다.
//!
//! 계정 모드 폰은 데스크톱에 토큰을 맡기지 않고 관문에만 맡긴다(폰 `push.dart` `bindAccount`) — 이 기기엔
//! 토큰도 열쇠도 없을 수 있다. 그래서 이 기계 학생의 알림은 관문 로그인이 있으면 관문(`POST /relay/push/alert`)에도
//! 올리고, 관문이 계정 폰들에 쏜다. 명부 기계 행은 올리지 않는다 — 그 기계가 계정 기기면 스스로 올려 두 번 온다.
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

/// 앱의 번들 id — `apns-topic`. 폰 프로젝트(mobile/ios)와 같아야 한다.
pub const TOPIC: &str = "com.debimarlene.kasaterm";
const TICK: Duration = Duration::from_secs(4);
/// 같은 pane 에 연달아 쏘지 않는 최소 간격 — 대기↔바쁨을 튀는 스피너 오판이 있다.
const MIN_GAP: Duration = Duration::from_secs(15);
/// 이보다 오래된 완료 보고는 울리지 않는다 — 끊겼던 기계가 다시 붙으면 묵은 보고가 새 줄로 들어온다.
const DONE_FRESH_SECS: u64 = 120;
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
/// 다듬는다 — 쪽지 요약엔 줄바꿈이 든다. 얼굴은 싣지 않는다(관문이 안 받는다, `gateway_push_alert.rs`).
pub(crate) fn relay_body(alert: &Alert) -> Value {
    let clip = |text: &str, max: usize| -> String {
        text.chars().map(|c| if c.is_control() { ' ' } else { c }).take(max).collect()
    };
    let url = alert.url.as_deref().filter(|u| {
        u.len() <= 2048 && (u.starts_with("https://") || u.starts_with("http://")) && !u.chars().any(char::is_control)
    });
    serde_json::json!({
        "title": clip(&alert.title, 120),
        "body": clip(&alert.body, 180),
        "pane": alert.pane,
        "kind": alert.kind,
        "collapse": alert.collapse.as_deref().map(|c| clip(c, 64)),
        "sender": alert.sender.as_deref().map(|s| clip(s, 40)),
        "url": url,
    })
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
    static OLD_GATEWAY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let Some((cred, _)) = crate::device_auth::capture() else { return 0 };
    let url = format!("{}/relay/push/alert", cred.relay.trim_end_matches('/'));
    let response = match relay_client().post(url).bearer_auth(&cred.token).json(&relay_body(alert)).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[push] 관문에 못 올림: {e}");
            return 0;
        }
    };
    let status = response.status().as_u16();
    let value: Value = response.json().await.unwrap_or_default();
    match status {
        200..=299 => value["phones"].as_u64().unwrap_or(0) as usize,
        401 if value["error"] == "unauthorized" => {
            crate::device_auth::reject_token(&cred.token);
            0
        }
        // 창구가 없는 옛 관문 — 알림마다 같은 말을 쌓지 않는다.
        404 => {
            if !OLD_GATEWAY.swap(true, std::sync::atomic::Ordering::Relaxed) {
                eprintln!("[push] 관문이 옛 판이라 계정 폰 알림을 못 올림(/relay/push/alert 없음)");
            }
            0
        }
        _ => {
            eprintln!("[push] 관문이 알림을 거절함 ({status} {})", value["error"].as_str().unwrap_or(""));
            0
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

/// 폰을 울리는 대기는 사람이 골라야 하는 것(승인·질문)뿐이다 — 답 기다림·오래 쉼까지 울리니
/// 끝냄·승인 알림이 묻혔다(2026-10-29 「done일때만 오게, 선택해야되는거나」).
fn needs_choice(kind: &str) -> bool {
    matches!(kind, "permission" | "question")
}

/// 쪽지 중 폰을 울릴 것 — 끝냄·실패·고를 것·브라우저에서 사람 손 기다림(`docs/chrome-mirror.md`),
/// 그리고 「이 폰」으로 보낸 페이지(울리지 않으면 종 목록을 열기 전엔 못 본다). 나머지는 종 목록에만 남는다.
fn note_rings(kind: &str) -> bool {
    matches!(kind, "done_ok" | "done_fail" | "browser" | "link") || needs_choice(kind)
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

fn note_alert(character: &str, kind: &str, summary: &str, pane: &str, url: Option<&str>) -> Alert {
    let who = if character.is_empty() { "학생" } else { character };
    let head = match kind {
        "permission" => "승인 기다림",
        "question" => "질문",
        "waiting" | "idle" => "답 기다림",
        "done_ok" => "끝냄",
        "done_fail" => "실패",
        "dead" => "멈춤",
        "link" => "페이지",
        _ => "쪽지",
    };
    Alert {
        title: format!("{who} · {head}"),
        body: summary.chars().take(180).collect(),
        machine: None,
        pane: pane.to_string(),
        kind: format!("note:{kind}"),
        // 페이지는 한 장씩 다 보여야 한다 — 같은 pane 의 두 번째 링크가 첫 번째를 덮으면
        // 사람은 하나만 받은 줄 안다.
        collapse: (kind != "link").then(|| format!("note-{pane}")),
        sender: (!character.is_empty()).then(|| character.to_string()),
        avatar_slug: crate::character::slug_for_any(character),
        url: url.filter(|u| !u.is_empty()).map(str::to_string),
    }
}

#[derive(Clone, PartialEq, Eq)]
struct Seen {
    waiting: bool,
    kind: String,
    /// 완료 보고가 있으면 그 나이(초).
    done_ago: Option<u64>,
}

fn seen_of(row: &Value) -> Seen {
    let status = row.get("status").and_then(Value::as_str).unwrap_or("");
    Seen {
        waiting: status == "waiting" || status == "blocked",
        kind: row.get("kind").and_then(Value::as_str).unwrap_or("").to_string(),
        done_ago: row
            .get("done_outcome")
            .and_then(Value::as_str)
            .map(|_| row.get("done_ago_secs").and_then(Value::as_u64).unwrap_or(0)),
    }
}

/// 새 완료 보고인가 — 보고가 새로 생겼거나, 같은 칸이 다시 보고해 나이가 줄었을 때.
/// 다른 기계의 줄은 목록 캐시라 갱신 사이에 나이가 멈춰 있으니 「줄었나」로만 가른다.
fn new_done(prev: Option<&Seen>, cur: &Seen) -> bool {
    let Some(age) = cur.done_ago.filter(|a| *a <= DONE_FRESH_SECS) else {
        return false;
    };
    prev.and_then(|p| p.done_ago).is_none_or(|before| age < before)
}

/// 이 기계 + 명부 기계의 pane 행을 한 번 모은다 — (기계 route, 행).
async fn all_rows() -> Vec<(Option<String>, Value)> {
    let mut out = Vec::new();
    if let Some(port) = std::env::var("KASASPACE_MCP_PORT").ok().and_then(|p| p.parse::<u16>().ok()) {
        if let Ok(resp) = client_local()
            .get(format!("http://127.0.0.1:{port}/term/panes"))
            .timeout(Duration::from_secs(3))
            .send()
            .await
        {
            if let Ok(v) = resp.json::<Value>().await {
                for row in v.as_array().cloned().unwrap_or_default() {
                    if row.get("mirror_of").and_then(Value::as_str).is_some() {
                        continue;
                    }
                    out.push((None, row));
                }
            }
        }
    }
    for m in crate::machines::snapshot() {
        let route = m.get("route").and_then(Value::as_str).map(str::to_string);
        for row in m.get("panes").and_then(Value::as_array).cloned().unwrap_or_default() {
            // 거울 pane 은 몸통이 저쪽이라 저쪽 행이 따로 온다 — 두 번 안 쏘게 뺀다.
            if row.get("mirror_of").and_then(Value::as_str).is_some() {
                continue;
            }
            out.push((route.clone(), row));
        }
    }
    out
}

fn client_local() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| reqwest::Client::builder().build().unwrap_or_default())
}

/// 쏠 길이 하나라도 있나 — 이 기기에 맡긴 토큰과 열쇠, 또는 관문 로그인(계정 폰은 관문이 쏜다).
fn ready() -> bool {
    (!tokens().is_empty() && configured()) || crate::device_auth::capture().is_some()
}

/// 상태 변화 감시 — 본체(PTY 를 가진 앱) 한 곳만 돈다. 첫 바퀴는 기준만 잡고 안 쏜다
/// (켜자마자 대기 중인 학생 전부가 한꺼번에 울리지 않게).
///
/// 「끝냄」은 턴이 끝날 때가 아니라 완료 보고(`kasaterm-cli done`)가 들어올 때만 울린다 — 백그라운드
/// 작업을 기다리는 학생은 턴을 몇 번이고 끝내서, 끝날 때마다 폰이 울렸다(2026-10-31 「done 상태인
/// 학생만 보내게」).
pub async fn push_loop() {
    let mut last: HashMap<String, Seen> = HashMap::new();
    let mut sent_at: HashMap<String, Instant> = HashMap::new();
    let mut primed = false;
    loop {
        tokio::time::sleep(TICK).await;
        if !ready() {
            continue;
        }
        let rows = all_rows().await;
        let mut now_seen: HashMap<String, Seen> = HashMap::new();
        for (route, row) in rows {
            let Some(id) = row.get("id").and_then(Value::as_str) else { continue };
            let name = row.get("name").and_then(Value::as_str).unwrap_or("");
            if name.is_empty() || row.get("closed").and_then(Value::as_bool).unwrap_or(false) {
                continue;
            }
            let key = format!("{}/{id}", route.as_deref().unwrap_or(""));
            let cur = seen_of(&row);
            let prev = last.get(&key).cloned();
            now_seen.insert(key.clone(), cur.clone());
            if !primed {
                continue;
            }
            let title_of = || {
                row.get("title")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .or_else(|| row.get("waiting_for").and_then(Value::as_str).map(str::to_string))
                    .unwrap_or_default()
            };
            let where_ = route.as_deref().map(|r| format!(" ({r})")).unwrap_or_default();
            let slug = row
                .get("slug")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| crate::character::slug_for_any(name));
            let alert = if cur.waiting
                && needs_choice(&cur.kind)
                && !prev.as_ref().is_some_and(|p| p.waiting && p.kind == cur.kind)
            {
                let head = if cur.kind == "permission" { "승인 기다림" } else { "질문 기다림" };
                Some(Alert {
                    title: format!("{name} · {head}{where_}"),
                    body: title_of(),
                    machine: route.clone(),
                    pane: id.to_string(),
                    kind: format!("waiting:{}", cur.kind),
                    collapse: Some(format!("wait-{key}")),
                    sender: Some(name.to_string()),
                    avatar_slug: slug.clone(),
                    url: None,
                })
            } else if new_done(prev.as_ref(), &cur) {
                let failed = row.get("done_outcome").and_then(Value::as_str) == Some("failed");
                Some(Alert {
                    title: format!("{name} · {}{where_}", if failed { "실패" } else { "끝냄" }),
                    body: row
                        .get("done_summary")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .unwrap_or_else(title_of),
                    machine: route.clone(),
                    pane: id.to_string(),
                    kind: "done".into(),
                    collapse: Some(format!("done-{key}")),
                    sender: Some(name.to_string()),
                    avatar_slug: slug.clone(),
                    url: None,
                })
            } else {
                None
            };
            if let Some(alert) = alert {
                if sent_at.get(&key).is_some_and(|t| t.elapsed() < MIN_GAP) {
                    continue;
                }
                sent_at.insert(key.clone(), Instant::now());
                let n = send(&alert).await;
                if n > 0 {
                    eprintln!("[push] {} → 폰 {n}대", alert.title);
                }
            }
        }
        last = now_seen;
        primed = true;
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
        assert!(!super::needs_choice("idle"));
    }

    #[test]
    fn a_turn_ending_without_a_done_report_never_rings() {
        let seen = |row: serde_json::Value| super::seen_of(&row);
        let working = seen(serde_json::json!({"status": "working"}));
        let idle = seen(serde_json::json!({"status": "idle"}));
        assert!(!super::new_done(Some(&working), &idle));
        assert!(!super::new_done(None, &idle));

        let reported = seen(serde_json::json!({"status": "working", "done_outcome": "succeeded", "done_ago_secs": 0}));
        assert!(super::new_done(Some(&working), &reported));
        let later = seen(serde_json::json!({"status": "idle", "done_outcome": "succeeded", "done_ago_secs": 8}));
        assert!(!super::new_done(Some(&reported), &later), "같은 보고가 쉬는 칸으로 넘어가도 다시 울리지 않는다");
        let again = seen(serde_json::json!({"status": "idle", "done_outcome": "failed", "done_ago_secs": 1}));
        assert!(super::new_done(Some(&later), &again), "다시 보고하면 나이가 줄어 다시 울린다");

        let stale = seen(serde_json::json!({"status": "idle", "done_outcome": "succeeded", "done_ago_secs": 3600}));
        assert!(!super::new_done(None, &stale), "다시 붙은 기계의 묵은 보고는 울리지 않는다");
    }

    #[test]
    fn the_relay_body_fits_the_gateway_and_never_names_a_machine_or_face() {
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
        let body = super::relay_body(&alert);
        assert_eq!(body["title"].as_str().unwrap().chars().count(), 120);
        let text = body["body"].as_str().unwrap();
        assert_eq!(text.chars().count(), 180);
        assert!(text.starts_with("첫 줄 둘째 줄"));
        assert_eq!(body["sender"].as_str().unwrap().chars().count(), 40);
        assert!(!body["sender"].as_str().unwrap().contains('\t'));
        assert!(body["url"].is_null() && body["collapse"].is_null());
        assert!(body.get("avatar").is_none() && body.get("machine").is_none());

        let page = super::Alert { url: Some("https://example.com/a".into()), ..alert };
        assert_eq!(super::relay_body(&page)["url"], "https://example.com/a");
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
            avatar_slug: crate::character::slug_for_any("시로코"),
            url: None,
        };
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let n = rt.block_on(super::send(&alert));
        eprintln!("[live_send] 보낸 폰 수 = {n}");
        assert!(n > 0, "APNs 가 거절했거나 못 보냄 — stderr 의 [push] 줄을 봐라");
    }
}
