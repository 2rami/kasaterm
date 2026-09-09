//! 폰 푸시(APNs) — 학생이 승인·질문을 기다리거나 일을 끝냈을 때, 쪽지가 왔을 때
//! 아이폰 앱으로 알린다(2026-09-09 지시 「알림기능도 돼? … 만들어」).
//!
//! 재료 셋: ①폰이 `POST /term/push-token` 으로 맡긴 기기 토큰(`push_tokens.json`,
//! 0600) ②애플 푸시 열쇠(`~/.config/kasaterm/apns/key.json` — key_id·team_id·p8,
//! 앱스토어 API 열쇠와 **다른** 열쇠다) ③상태 변화 감시 루프(`push_loop`) — 폰이 읽는
//! `/term/panes` 행(이 기계 + 명부 기계 캐시)을 4초마다 대조해 「대기로 들어감」
//! 「바쁨→끝냄」 순간에만 쏜다. 상태를 그대로 폴링해 쏘면 같은 대기가 매번 온다.
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
pub const TOPIC: &str = "com.debimarlene.kasatermMobile";
const TICK: Duration = Duration::from_secs(4);
/// 같은 pane 에 연달아 쏘지 않는 최소 간격 — 대기↔바쁨을 튀는 스피너 오판이 있다.
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
}

fn notification_thread(alert: &Alert) -> String {
    format!("{}/{}", alert.machine.as_deref().unwrap_or("local"), alert.pane)
}

/// 등록된 폰 전부에 쏜다. 열쇠나 토큰이 없으면 조용히 0.
pub async fn send(alert: &Alert) -> usize {
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
        });
        let host = if t.env == "dev" {
            "https://api.sandbox.push.apple.com"
        } else {
            "https://api.push.apple.com"
        };
        let mut req = client()
            .post(format!("{host}/3/device/{}", t.token))
            .header("authorization", format!("bearer {bearer}"))
            .header("apns-topic", TOPIC)
            .header("apns-push-type", "alert")
            .header("apns-priority", "10")
            .json(&payload);
        if let Some(c) = &alert.collapse {
            req = req.header("apns-collapse-id", c.chars().take(64).collect::<String>());
        }
        match req.send().await {
            Ok(r) if r.status().is_success() => sent += 1,
            Ok(r) => {
                let status = r.status();
                let text = r.text().await.unwrap_or_default();
                eprintln!("[push] {status} {text} (env={})", t.env);
                // 지운 앱·옛 토큰은 다시 안 쏘게 걷는다(애플이 410 으로 말해 준다).
                if status.as_u16() == 410 || text.contains("BadDeviceToken") || text.contains("Unregistered") {
                    unregister(&t.token);
                }
            }
            Err(e) => eprintln!("[push] 못 보냄: {e}"),
        }
    }
    sent
}

/// 쪽지가 들어왔을 때 — `notes::add` 뒤에서 부른다.
pub fn note_arrived(character: &str, kind: &str, summary: &str, pane: &str) {
    let who = if character.is_empty() { "학생" } else { character };
    let head = match kind {
        "permission" => "승인 기다림",
        "question" => "질문",
        "waiting" | "idle" => "답 기다림",
        "done_ok" => "끝냄",
        "done_fail" => "실패",
        "dead" => "멈춤",
        _ => "쪽지",
    };
    let alert = Alert {
        title: format!("{who} · {head}"),
        body: summary.chars().take(180).collect(),
        machine: None,
        pane: pane.to_string(),
        kind: format!("note:{kind}"),
        collapse: Some(format!("note-{pane}")),
        sender: (!character.is_empty()).then(|| character.to_string()),
        avatar_slug: crate::character::slug_for_any(character),
    };
    tokio::spawn(async move {
        send(&alert).await;
    });
}

#[derive(Clone, PartialEq, Eq)]
struct Seen {
    waiting: bool,
    busy: bool,
    kind: String,
}

fn seen_of(row: &Value) -> Seen {
    let status = row.get("status").and_then(Value::as_str).unwrap_or("");
    let waiting = status == "waiting" || status == "blocked";
    let idle = status == "idle";
    Seen {
        waiting,
        busy: !status.is_empty() && !idle && !waiting,
        kind: row.get("kind").and_then(Value::as_str).unwrap_or("").to_string(),
    }
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

/// 상태 변화 감시 — 본체(PTY 를 가진 앱) 한 곳만 돈다. 첫 바퀴는 기준만 잡고 안 쏜다
/// (켜자마자 대기 중인 학생 전부가 한꺼번에 울리지 않게).
pub async fn push_loop() {
    let mut last: HashMap<String, Seen> = HashMap::new();
    let mut sent_at: HashMap<String, Instant> = HashMap::new();
    let mut primed = false;
    loop {
        tokio::time::sleep(TICK).await;
        if tokens().is_empty() || !configured() {
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
            let alert = if cur.waiting && !prev.as_ref().is_some_and(|p| p.waiting && p.kind == cur.kind) {
                let head = match cur.kind.as_str() {
                    "permission" => "승인 기다림",
                    "question" => "질문 기다림",
                    "idle" => "오래 기다림",
                    _ => "답 기다림",
                };
                Some(Alert {
                    title: format!("{name} · {head}{where_}"),
                    body: title_of(),
                    machine: route.clone(),
                    pane: id.to_string(),
                    kind: format!("waiting:{}", cur.kind),
                    collapse: Some(format!("wait-{key}")),
                    sender: Some(name.to_string()),
                    avatar_slug: slug.clone(),
                })
            } else if !cur.busy && !cur.waiting && prev.as_ref().is_some_and(|p| p.busy) {
                Some(Alert {
                    title: format!("{name} · 끝냄{where_}"),
                    body: title_of(),
                    machine: route.clone(),
                    pane: id.to_string(),
                    kind: "done".into(),
                    collapse: Some(format!("done-{key}")),
                    sender: Some(name.to_string()),
                    avatar_slug: slug.clone(),
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
            sender: None, avatar_slug: None,
        };
        let local = super::notification_thread(&alert);
        alert.machine = Some("macbook".into());
        assert_ne!(local, super::notification_thread(&alert));
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
}
