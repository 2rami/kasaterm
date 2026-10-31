//! `POST /relay/push/alert` — 계정 데스크톱이 자기 학생의 끝냄·대기·쪽지를 올리면 관문이 그 계정 폰들에 쏜다.
//! 계정 모드 폰은 푸시 토큰을 관문에만 맡긴다(`/relay/approvals/push`) — 데스크톱은 폰 토큰도 APNs 열쇠도
//! 없을 수 있어 직접 못 쏜다(`push.rs`).
//!
//! - 올리는 쪽은 데스크톱 기기만. 어느 기계인지는 몸통이 말하지 않는다 — 관문이 그 기기 기록의 machine_id 로
//!   `~<machine_id>` 를 붙인다(폰이 누르면 그 경로로 그 기계 화면을 연다).
//! - 종류는 폰을 울리는 것만 받는다 — 관문이 아무 알림이나 중계하는 문이 되지 않게.
//! - 얼굴 주소는 싣지 않는다. 알림 확장은 인증 없이 받아 와야 하는데 계정 경로는 기기 토큰이 있어야 열린다.
//! - 기기마다 분당 30통 — 고장 난 감시 루프가 폰을 끝없이 울리지 않게.

use super::*;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::VecDeque;

const PER_MINUTE: usize = 30;
const WINDOW: Duration = Duration::from_secs(60);
const MAX_BODY: usize = 16 * 1024;
/// 애플 `apns-collapse-id` 상한 — 글자가 아니라 바이트다.
const MAX_COLLAPSE: usize = 64;
/// 알림 한 통은 4KB 까지다 — 제목·본문을 다 채워도 넘지 않게.
const MAX_URL: usize = 2048;
/// 폰을 울리는 쪽지 종류(`push.rs` `note_rings`). 2026-10-29 에 일부러 줄인 목록이라 여기서 늘리지 않는다.
const NOTE_KINDS: [&str; 6] = ["done_ok", "done_fail", "permission", "question", "browser", "link"];

pub(super) fn routes() -> Router<Gate> {
    Router::new().route("/relay/push/alert", axum::routing::post(alert))
}

/// 기기 → 최근 1분 동안 받은 알림 시각. 1분짜리라 메모리에만 둔다.
#[derive(Default)]
pub(super) struct Meter(Mutex<HashMap<String, VecDeque<Instant>>>);

impl Meter {
    fn allow(&self, device: &str, now: Instant) -> bool {
        let mut seen = self.0.lock().unwrap();
        seen.retain(|_, times| {
            while times.front().is_some_and(|t| now.duration_since(*t) >= WINDOW) {
                times.pop_front();
            }
            !times.is_empty()
        });
        let times = seen.entry(device.to_string()).or_default();
        if times.len() >= PER_MINUTE {
            return false;
        }
        times.push_back(now);
        true
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AlertBody {
    title: String,
    body: String,
    pane: String,
    kind: String,
    #[serde(default)]
    collapse: Option<String>,
    #[serde(default)]
    sender: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

fn fits(text: &str, max: usize) -> bool {
    text.chars().count() <= max && !text.chars().any(char::is_control)
}

fn kind_ok(kind: &str) -> bool {
    matches!(kind, "done" | "waiting:permission" | "waiting:question")
        || kind.strip_prefix("note:").is_some_and(|k| NOTE_KINDS.contains(&k))
}

fn url_ok(url: &str) -> bool {
    url.len() <= MAX_URL
        && !url.chars().any(char::is_control)
        && reqwest::Url::parse(url).is_ok_and(|u| matches!(u.scheme(), "http" | "https") && u.has_host())
}

/// 기기 id 를 앞에 붙인다 — 두 기계의 같은 pane(`%3`)이 서로의 알림을 갈아 끼우지 않게.
fn collapse_id(device: &str, collapse: &str) -> String {
    let mut id = format!("{device}-{collapse}");
    let mut end = id.len().min(MAX_COLLAPSE);
    while !id.is_char_boundary(end) {
        end -= 1;
    }
    id.truncate(end);
    id
}

/// 갈아 끼우지 않을 알림(한 장씩 다 보여야 하는 페이지)에도 자리 이름은 준다 — `send_push` 는 이름이 없으면
/// 승인 알림 자리(`approval`)에 넣어 버린다.
fn lone_collapse(device: &str) -> String {
    use ring::rand::SecureRandom as _;
    let mut bytes = [0u8; 8];
    let _ = ring::rand::SystemRandom::new().fill(&mut bytes);
    collapse_id(device, &bytes.iter().map(|b| format!("{b:02x}")).collect::<String>())
}

/// 몸통을 검사해 폰에 갈 payload 로. 모양은 데스크톱 `push.rs` `send()` 와 같다 — 폰 `push.dart` 가 같은 키를 읽는다.
fn payload(body: AlertBody, device: &str, machine_id: &str) -> Result<Value, &'static str> {
    let given = |v: Option<String>| v.filter(|s| !s.is_empty());
    let (collapse, sender, url) = (given(body.collapse), given(body.sender), given(body.url));
    let text_ok = fits(&body.title, 120)
        && fits(&body.body, 180)
        && !body.pane.is_empty()
        && fits(&body.pane, 64)
        && sender.as_deref().is_none_or(|s| fits(s, 40))
        && collapse.as_deref().is_none_or(|c| fits(c, MAX_COLLAPSE));
    if !text_ok {
        return Err("bad_text");
    }
    if !kind_ok(&body.kind) {
        return Err("bad_kind");
    }
    if !url.as_deref().is_none_or(url_ok) {
        return Err("bad_url");
    }
    let machine = format!("~{machine_id}");
    let collapse = collapse.map_or_else(|| lone_collapse(device), |c| collapse_id(device, &c));
    Ok(json!({
        "aps": {
            "alert": {"title": body.title, "body": body.body},
            "sound": "default",
            "thread-id": format!("{machine}/{}", body.pane),
            "mutable-content": 1,
        },
        "machine": machine,
        "pane": body.pane,
        "kind": body.kind,
        "sender": sender,
        "url": url,
        "collapse": collapse,
    }))
}

async fn alert(State(gate): State<Gate>, req: axum::extract::Request) -> axum::response::Response {
    let Some((device, rec)) = gate.device_of(req.headers()) else {
        return json_err(StatusCode::UNAUTHORIZED, "unauthorized");
    };
    if rec.kind != "desktop" {
        return json_err(StatusCode::FORBIDDEN, "desktop_only");
    }
    // 기계 이름 없이 쏘면 폰이 누를 때 기준 기기로 가 엉뚱한 기계의 같은 pane 을 연다.
    let Some(machine_id) = rec.machine_id.filter(|m| !m.is_empty()) else {
        return json_err(StatusCode::FORBIDDEN, "machine_required");
    };
    let bytes = match tokio::time::timeout(Duration::from_secs(10), axum::body::to_bytes(req.into_body(), MAX_BODY)).await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(_)) => return json_err(StatusCode::PAYLOAD_TOO_LARGE, "body_too_large"),
        Err(_) => return json_err(StatusCode::REQUEST_TIMEOUT, "request_timeout"),
    };
    let Ok(body) = serde_json::from_slice::<AlertBody>(&bytes) else {
        return json_err(StatusCode::BAD_REQUEST, "bad_request");
    };
    let payload = match payload(body, &device, &machine_id) {
        Ok(payload) => payload,
        Err(code) => return json_err(StatusCode::BAD_REQUEST, code),
    };
    if !gate.push_alerts.allow(&device, Instant::now()) {
        return json_err(StatusCode::TOO_MANY_REQUESTS, "rate_limited");
    }
    let phones = approvals::send_push(&gate, &rec.account, payload);
    axum::Json(json!({"ok": true, "phones": phones})).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(kind: &str) -> AlertBody {
        AlertBody {
            title: "시로코 · 끝냄".into(),
            body: "빌드 끝".into(),
            pane: "%3".into(),
            kind: kind.into(),
            collapse: Some("done-/%3".into()),
            sender: Some("시로코".into()),
            url: None,
        }
    }

    #[test]
    fn the_gateway_names_the_machine_and_sends_no_avatar() {
        let p = payload(body("done"), "dev_0011223344556677", "machine-macbook-1").unwrap();
        assert_eq!(p["machine"], "~machine-macbook-1");
        assert_eq!(p["aps"]["thread-id"], "~machine-macbook-1/%3");
        assert_eq!(p["aps"]["mutable-content"], 1);
        assert_eq!(p["aps"]["sound"], "default");
        assert_eq!(p["collapse"], "dev_0011223344556677-done-/%3");
        assert_eq!(p["sender"], "시로코");
        assert!(p.get("avatar").is_none());
    }

    #[test]
    fn only_kinds_that_ring_the_phone_pass() {
        for kind in ["done", "waiting:permission", "waiting:question", "note:done_ok", "note:done_fail",
            "note:permission", "note:question", "note:browser", "note:link"] {
            assert!(payload(body(kind), "dev_a", "machine-a").is_ok(), "{kind}");
        }
        for kind in ["", "test", "approval", "agent_chain", "waiting:idle", "waiting:", "note:", "note:dead",
            "note:waiting", "note:idle", "done:"] {
            assert_eq!(payload(body(kind), "dev_a", "machine-a").err(), Some("bad_kind"), "{kind}");
        }
    }

    #[test]
    fn text_and_urls_are_bounded() {
        let mut b = body("done");
        b.title = "줄\n바꿈".into();
        assert_eq!(payload(b, "dev_a", "machine-a").err(), Some("bad_text"));
        let mut b = body("done");
        b.body = "가".repeat(181);
        assert_eq!(payload(b, "dev_a", "machine-a").err(), Some("bad_text"));
        let mut b = body("done");
        b.pane = String::new();
        assert_eq!(payload(b, "dev_a", "machine-a").err(), Some("bad_text"));
        let mut b = body("done");
        b.sender = Some("가".repeat(41));
        assert_eq!(payload(b, "dev_a", "machine-a").err(), Some("bad_text"));
        for url in ["javascript:alert(1)", "file:///etc/passwd", "ftp://example.com/x", "https://"] {
            let mut b = body("note:link");
            b.url = Some(url.into());
            assert_eq!(payload(b, "dev_a", "machine-a").err(), Some("bad_url"), "{url}");
        }
        let mut b = body("note:link");
        b.url = Some("https://example.com/a?b=1".into());
        assert_eq!(payload(b, "dev_a", "machine-a").unwrap()["url"], "https://example.com/a?b=1");
    }

    #[test]
    fn collapse_fits_apple_and_pages_never_replace_each_other() {
        let mut b = body("done");
        b.collapse = Some("가".repeat(60));
        let id = payload(b, "dev_0011223344556677", "machine-a").unwrap()["collapse"].as_str().unwrap().to_string();
        assert!(id.len() <= MAX_COLLAPSE && id.starts_with("dev_0011223344556677-"));
        let lone = || {
            let mut b = body("note:link");
            b.collapse = None;
            payload(b, "dev_a", "machine-a").unwrap()["collapse"].as_str().unwrap().to_string()
        };
        let (one, two) = (lone(), lone());
        assert_ne!(one, two);
        assert!(one.starts_with("dev_a-") && one != "approval");
    }

    /// 데스크톱이 만든 몸통을 관문이 그대로 받는다 — 줄바꿈 든 쪽지 요약·긴 제목도 거절되지 않게 데스크톱이 다듬는다.
    #[test]
    fn the_desktop_body_passes_the_gateway() {
        for (kind, collapse, url) in [
            ("done", Some("done-/%3"), None),
            ("waiting:question", Some("wait-/%3"), None),
            ("note:link", None, Some("https://example.com/page")),
        ] {
            let alert = crate::push::Alert {
                title: format!("{} · 끝냄", "긴이름".repeat(50)),
                body: "첫 줄\n둘째 줄\t탭".repeat(30),
                machine: None,
                pane: "%3".into(),
                kind: kind.into(),
                collapse: collapse.map(str::to_string),
                sender: Some("시로코".into()),
                avatar_slug: Some("shiroko".into()),
                url: url.map(str::to_string),
            };
            let body: AlertBody = serde_json::from_value(crate::push::relay_body(&alert)).unwrap();
            let p = payload(body, "dev_a", "machine-a").unwrap_or_else(|e| panic!("{kind}: {e}"));
            assert_eq!(p["kind"], kind);
        }
    }

    #[test]
    fn each_device_gets_thirty_a_minute() {
        let meter = Meter::default();
        let start = Instant::now();
        for _ in 0..PER_MINUTE {
            assert!(meter.allow("dev_a", start));
        }
        assert!(!meter.allow("dev_a", start));
        assert!(meter.allow("dev_b", start));
        assert!(meter.allow("dev_a", start + WINDOW));
    }

    struct Rig {
        base: String,
        gate: Gate,
        dir: PathBuf,
    }

    impl Rig {
        async fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("kasa-push-alert-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let mut accounts = crate::relay_auth::AccountsFile::default();
            for name in ["geno", "other"] {
                accounts.accounts.insert(
                    name.into(),
                    crate::relay_auth::Account {
                        pbkdf2_sha256: crate::relay_auth::hash_password_with("fixture-pass", 1),
                        created: 1,
                        disabled: false,
                        login: None,
                    },
                );
            }
            crate::relay_auth::save_accounts(&dir.join("relay-accounts.json"), &accounts).unwrap();
            let gate = Gate::with_accounts(Some(dir.join("relay-state.json")), Some(dir.join("relay-accounts.json")));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let served = gate.clone();
            tokio::spawn(async move {
                axum::serve(listener, router(served).into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap()
            });
            Self { base: format!("http://{address}"), gate, dir }
        }

        fn device(&self, id: &str, account: &str, kind: &str) -> String {
            let token = crate::relay_auth::new_token();
            self.gate.devices.lock().unwrap().insert(
                id.into(),
                DeviceRec {
                    token_hash: crate::relay_auth::token_hash(&token),
                    account: account.into(),
                    kind: kind.into(),
                    machine_id: (kind == "desktop").then(|| format!("machine-{id}")),
                    label: String::new(),
                    created: 1,
                    last_seen: 1,
                    revoked_at: None,
                },
            );
            token
        }

        async fn post(&self, path: &str, bearer: Option<&str>, body: Value) -> (u16, Value) {
            let mut req = reqwest::Client::new().post(format!("{}{path}", self.base)).json(&body);
            if let Some(token) = bearer {
                req = req.bearer_auth(token);
            }
            let response = req.send().await.unwrap();
            (response.status().as_u16(), response.json().await.unwrap_or_default())
        }

        async fn alert(&self, bearer: Option<&str>, body: Value) -> (u16, Value) {
            self.post("/relay/push/alert", bearer, body).await
        }

        async fn give_push_token(&self, phone: &str, token: &str) {
            let (status, _) = self.post("/relay/approvals/push", Some(phone), json!({"token": token})).await;
            assert_eq!(status, 200);
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn done() -> Value {
        json!({"title": "시로코 · 끝냄", "body": "빌드 끝", "pane": "%3", "kind": "done", "collapse": "done-/%3", "sender": "시로코"})
    }

    #[tokio::test]
    async fn only_a_live_desktop_login_may_ring_the_phones() {
        let rig = Rig::new().await;
        let desk = rig.device("dev_desk", "geno", "desktop");
        let phone = rig.device("dev_phone", "geno", "phone");
        assert_eq!(rig.alert(None, done()).await, (401, json!({"ok": false, "error": "unauthorized"})));
        assert_eq!(rig.alert(Some("kasa_not-a-token"), done()).await.0, 401);
        assert_eq!(rig.alert(Some(&phone), done()).await, (403, json!({"ok": false, "error": "desktop_only"})));
        assert_eq!(rig.alert(Some(&desk), done()).await.0, 200);
        assert!(rig.gate.revoke("dev_desk"));
        assert_eq!(rig.alert(Some(&desk), done()).await.0, 401);
    }

    #[tokio::test]
    async fn bad_kinds_text_and_fields_are_refused() {
        let rig = Rig::new().await;
        let desk = rig.device("dev_desk", "geno", "desktop");
        for (patch, code) in [
            (json!({"kind": "waiting:idle"}), "bad_kind"),
            (json!({"kind": "note:dead"}), "bad_kind"),
            (json!({"title": "줄\n바꿈"}), "bad_text"),
            (json!({"pane": ""}), "bad_text"),
            (json!({"kind": "note:link", "url": "javascript:alert(1)"}), "bad_url"),
            (json!({"machine": "someone-else"}), "bad_request"),
        ] {
            let mut body = done();
            for (k, v) in patch.as_object().unwrap() {
                body[k] = v.clone();
            }
            assert_eq!(rig.alert(Some(&desk), body).await, (400, json!({"ok": false, "error": code})), "{patch}");
        }
    }

    #[tokio::test]
    async fn the_answer_counts_this_accounts_phones_with_tokens() {
        let rig = Rig::new().await;
        let desk = rig.device("dev_desk", "geno", "desktop");
        let first = rig.device("dev_phone1", "geno", "phone");
        let second = rig.device("dev_phone2", "geno", "phone");
        let stranger = rig.device("dev_phone3", "other", "phone");
        assert_eq!(rig.alert(Some(&desk), done()).await, (200, json!({"ok": true, "phones": 0})));
        rig.give_push_token(&first, &"a1".repeat(32)).await;
        rig.give_push_token(&stranger, &"c3".repeat(32)).await;
        assert_eq!(rig.alert(Some(&desk), done()).await.1["phones"], 1);
        rig.give_push_token(&second, &"b2".repeat(32)).await;
        assert_eq!(rig.alert(Some(&desk), done()).await.1["phones"], 2);
        assert!(rig.gate.revoke("dev_phone1"));
        assert_eq!(rig.alert(Some(&desk), done()).await.1["phones"], 1);
    }

    #[tokio::test]
    async fn a_desktop_past_thirty_a_minute_is_told_to_wait() {
        let rig = Rig::new().await;
        let desk = rig.device("dev_desk", "geno", "desktop");
        let other = rig.device("dev_desk2", "geno", "desktop");
        for _ in 0..PER_MINUTE {
            assert_eq!(rig.alert(Some(&desk), done()).await.0, 200);
        }
        assert_eq!(rig.alert(Some(&desk), done()).await, (429, json!({"ok": false, "error": "rate_limited"})));
        assert_eq!(rig.alert(Some(&other), done()).await.0, 200);
    }
}
