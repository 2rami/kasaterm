//! 관문 관리 화면 — 누가 가입했고 어떻게 쓰는지를 관리자 계정에게만 보인다(`/relay/admin`).
//!
//! - 관리자는 서버 환경변수 `KASA_RELAY_ADMINS`(쉼표로 가른 관문 계정 이름)로만 정한다. 요청
//!   본문·쿠키·헤더로는 관리자가 되지 않는다 — 쿠키는 이미 확인한 로그인에 준 무작위 표일 뿐이고
//!   관문은 그 sha256 만 메모리에 쥔다(재시작하면 다시 로그인).
//! - 로그인은 구글·깃허브 OAuth 브라우저 흐름(state·PKCE·state 쿠키)을 그대로 탄다. 그 계정에
//!   **이미 연결된** 신원이어야 하고, 이 흐름은 계정도 기기 토큰도 만들지 않는다.
//! - 보이는 것은 가입 경로·시각·표시 이름·기기 수·마지막 접속·연결 시간·중계 바이트뿐이다.
//!   중계하는 요청의 내용(대화·화면)은 담지 않는다 — 업링크 프레임의 바이트 수만 센다.

use super::*;
use crate::oauth_accounts::{Identity, Provider};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

const SESSION_TTL: Duration = Duration::from_secs(2 * 3600);
const MAX_SESSIONS: usize = 64;
const COOKIE: &str = "__Secure-kasa_admin";
/// 사용량 파일은 이만큼마다 한 번만 쓴다 — 업링크가 30초마다 몫을 접어 넣는다.
const USAGE_SAVE_EVERY: Duration = Duration::from_secs(300);

pub(super) fn routes() -> Router<Gate> {
    Router::new()
        .route("/relay/admin", get(home))
        .route("/relay/admin/accounts", get(accounts_json))
        .route("/relay/admin/login/{provider}", axum::routing::post(login))
        .route("/relay/admin/logout", axum::routing::post(logout))
        .layer(axum::middleware::map_response(secure_response))
}

async fn secure_response(mut response: axum::response::Response) -> axum::response::Response {
    // 폼 POST 가 Origin 을 싣도록 HTML 은 same-origin — no-referrer 면 Origin 이 null 이 된다.
    let html = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/html"));
    for (key, value) in [
        ("cache-control", "no-store"),
        ("pragma", "no-cache"),
        ("referrer-policy", if html { "same-origin" } else { "no-referrer" }),
        ("x-content-type-options", "nosniff"),
        (
            "content-security-policy",
            "default-src 'none'; style-src 'unsafe-inline'; frame-ancestors 'none'; form-action 'self' https://accounts.google.com https://github.com; base-uri 'none'",
        ),
    ] {
        response.headers_mut().insert(
            axum::http::HeaderName::from_static(key),
            axum::http::HeaderValue::from_static(value),
        );
    }
    response
}

pub(super) struct Admins {
    accounts: HashSet<String>,
    /// 표 sha256 → (계정, 만료).
    sessions: Mutex<HashMap<String, (String, Instant)>>,
}

impl Admins {
    pub(super) fn from_env() -> Self {
        Self::new(std::env::var("KASA_RELAY_ADMINS").ok().as_deref())
    }

    pub(super) fn new(list: Option<&str>) -> Self {
        let accounts = list
            .unwrap_or("")
            .split(',')
            .map(str::trim)
            .filter(|name| crate::relay_auth::valid_account_name(name))
            .map(str::to_string)
            .collect();
        Self { accounts, sessions: Mutex::new(HashMap::new()) }
    }

    fn enabled(&self) -> bool {
        !self.accounts.is_empty()
    }

    fn issue(&self, account: &str) -> String {
        let token = crate::relay_auth::new_token();
        let mut sessions = self.sessions.lock().unwrap();
        sessions.retain(|_, (_, until)| *until > Instant::now());
        if sessions.len() >= MAX_SESSIONS {
            if let Some(oldest) = sessions.iter().min_by_key(|(_, (_, until))| *until).map(|(k, _)| k.clone()) {
                sessions.remove(&oldest);
            }
        }
        sessions.insert(
            crate::relay_auth::token_hash(&token),
            (account.to_string(), Instant::now() + SESSION_TTL),
        );
        token
    }

    fn token(headers: &axum::http::HeaderMap) -> Option<String> {
        headers
            .get_all(header::COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(';'))
            .filter_map(|pair| pair.trim().split_once('='))
            .find(|(name, _)| *name == COOKIE)
            .map(|(_, token)| token.to_string())
            .filter(|token| token.starts_with(crate::relay_auth::TOKEN_PREFIX) && token.len() <= 200)
    }

    fn end(&self, headers: &axum::http::HeaderMap) {
        if let Some(token) = Self::token(headers) {
            self.sessions.lock().unwrap().remove(&crate::relay_auth::token_hash(&token));
        }
    }
}

impl Gate {
    /// 지금 관리자로 지정된 계정인가 — 기기 토큰으로 들어온 요청용(`gateway_install.rs`).
    pub(super) fn is_admin(&self, account: &str) -> bool {
        self.admins.accounts.contains(account)
    }

    /// 관리 화면을 봐도 되는 계정 — 표가 살아 있고, 지금도 지정돼 있고, 막히지 않았다.
    fn admin_of(&self, headers: &axum::http::HeaderMap) -> Option<String> {
        let token = Admins::token(headers)?;
        let account = {
            let sessions = self.admins.sessions.lock().unwrap();
            let (account, until) = sessions.get(&crate::relay_auth::token_hash(&token))?;
            (*until > Instant::now()).then(|| account.clone())?
        };
        (self.admins.accounts.contains(&account) && self.account_active(&account)).then_some(account)
    }

    /// 업링크 하나가 지난번 뒤로 쌓은 연결 시간·바이트·요청 수를 그 계정 몫에 접어 넣는다.
    pub(super) fn meter(&self, up: &Uplink) {
        let Some(account) = &up.account else { return };
        let ms = {
            let mut last = up.metered.lock().unwrap();
            let now = Instant::now();
            let elapsed = now.duration_since(*last);
            *last = now;
            elapsed.as_millis() as u64
        };
        let bytes = up.rx_bytes.swap(0, Ordering::Relaxed) + up.tx_bytes.swap(0, Ordering::Relaxed);
        let requests = up.requests.swap(0, Ordering::Relaxed);
        self.usage.add(account, ms, bytes, requests);
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(super) struct Usage {
    #[serde(default)]
    pub connected_ms: u64,
    #[serde(default)]
    pub relayed_bytes: u64,
    #[serde(default)]
    pub requests: u64,
    #[serde(default)]
    pub last_active: u64,
}

/// 계정별 대략 사용량(`relay-usage.json`, 0600). 관문이 죽으면 마지막 저장 뒤 몫은 잃는다.
pub(super) struct Meter {
    path: Option<PathBuf>,
    inner: Mutex<(HashMap<String, Usage>, Instant)>,
}

impl Meter {
    pub(super) fn open(path: Option<PathBuf>) -> Self {
        let map = path
            .as_deref()
            .and_then(|p| match std::fs::read_to_string(p) {
                Ok(raw) => serde_json::from_str(&raw).ok().or_else(|| {
                    // 조용히 덮어쓰면 쌓인 몫이 흔적 없이 사라진다 — 옆으로 치워 둔다.
                    let _ = std::fs::rename(p, p.with_extension(format!("json.corrupt-{}", now_secs())));
                    None
                }),
                Err(_) => None,
            })
            .unwrap_or_default();
        Self { path, inner: Mutex::new((map, Instant::now())) }
    }

    fn add(&self, account: &str, ms: u64, bytes: u64, requests: u64) {
        let mut inner = self.inner.lock().unwrap();
        let usage = inner.0.entry(account.to_string()).or_default();
        usage.connected_ms = usage.connected_ms.saturating_add(ms);
        usage.relayed_bytes = usage.relayed_bytes.saturating_add(bytes);
        usage.requests = usage.requests.saturating_add(requests);
        if ms > 0 || bytes > 0 {
            usage.last_active = now_secs();
        }
        if inner.1.elapsed() >= USAGE_SAVE_EVERY {
            Self::write(&self.path, &mut inner);
        }
    }

    pub(super) fn save(&self) {
        Self::write(&self.path, &mut self.inner.lock().unwrap());
    }

    fn write(path: &Option<PathBuf>, inner: &mut (HashMap<String, Usage>, Instant)) {
        inner.1 = Instant::now();
        let Some(path) = path else { return };
        let written = serde_json::to_string(&inner.0)
            .map_err(std::io::Error::other)
            .and_then(|body| crate::relay_auth::write_private(path, &body));
        if let Err(error) = written {
            eprintln!("[admin] 사용량 저장 실패: {error}");
        }
    }

    fn snapshot(&self) -> HashMap<String, Usage> {
        self.inner.lock().unwrap().0.clone()
    }
}

#[derive(Debug, Serialize)]
pub(super) struct Row {
    account: String,
    display_name: Option<String>,
    /// `google`·`github`·`password`(관문 기계에서 만든 계정)·`unknown`.
    signup: &'static str,
    created: u64,
    /// 가입 기록이 없어 첫 기기 로그인 시각으로 가늠했다.
    created_estimated: bool,
    login_methods: Vec<&'static str>,
    active: bool,
    desktops: usize,
    phones: usize,
    online: usize,
    revoked: usize,
    last_seen: u64,
    connected_secs: u64,
    relayed_bytes: u64,
    requests: u64,
}

pub(super) fn rows(gate: &Gate) -> Vec<Row> {
    let live: Vec<Arc<Uplink>> = gate.live.lock().unwrap().values().map(|(_, up)| up.clone()).collect();
    for up in &live {
        gate.meter(up);
    }
    let online: HashSet<String> = live.iter().filter(|up| up.fresh()).filter_map(|up| up.device_id.clone()).collect();
    let usage = gate.usage.snapshot();
    let oauth = gate.oauth.overview();
    let passwords: HashMap<String, (u64, bool)> = gate
        .accounts
        .summary()
        .into_iter()
        .map(|(name, created, disabled)| (name, (created, disabled)))
        .collect();
    let devices = gate.devices.lock().unwrap().clone();
    let mut names: HashSet<String> = passwords.keys().chain(oauth.keys()).chain(usage.keys()).cloned().collect();
    names.extend(devices.values().map(|device| device.account.clone()));
    let mut rows: Vec<Row> = names
        .into_iter()
        .map(|account| {
            let mine: Vec<(&String, &DeviceRec)> = devices.iter().filter(|(_, d)| d.account == account).collect();
            let alive = || mine.iter().filter(|(_, d)| d.revoked_at.is_none());
            let first_login = mine.iter().map(|(_, d)| d.created).filter(|t| *t > 0).min().unwrap_or(0);
            let info = oauth.get(&account);
            let profile = info.and_then(|info| info.profile.as_ref());
            let (signup, created, active) = if let Some((created, disabled)) = passwords.get(&account) {
                ("password", *created, !disabled)
            } else if let Some(info) = info.filter(|info| info.oauth_created) {
                let provider = profile.and_then(|p| p.provider).or_else(|| info.providers.first().copied());
                (provider.map_or("unknown", Provider::name), profile.map_or(0, |p| p.created), info.active)
            } else {
                ("unknown", 0, false)
            };
            let mut login_methods: Vec<&'static str> = info.map(|info| info.providers.iter().map(|p| p.name()).collect()).unwrap_or_default();
            if passwords.contains_key(&account) {
                login_methods.insert(0, "password");
            }
            let used = usage.get(&account).cloned().unwrap_or_default();
            Row {
                display_name: gate.oauth.display_name(&account),
                signup,
                created: if created > 0 { created } else { first_login },
                created_estimated: created == 0 && first_login > 0,
                login_methods,
                active,
                desktops: alive().filter(|(_, d)| d.kind == "desktop").count(),
                phones: alive().filter(|(_, d)| d.kind == "phone").count(),
                online: alive().filter(|(id, _)| online.contains(*id)).count(),
                revoked: mine.len() - alive().count(),
                last_seen: mine
                    .iter()
                    .map(|(_, d)| d.last_seen)
                    .chain(profile.map(|p| p.last_login))
                    .max()
                    .unwrap_or(0)
                    .max(used.last_active),
                connected_secs: used.connected_ms / 1000,
                relayed_bytes: used.relayed_bytes,
                requests: used.requests,
                account,
            }
        })
        .collect();
    rows.sort_by(|a, b| b.created.cmp(&a.created).then_with(|| a.account.cmp(&b.account)));
    rows
}

fn html_page(status: StatusCode, body: &str) -> axum::response::Response {
    let html = format!(
        "<!doctype html><html lang=ko><meta charset=utf-8><meta name=viewport content=\"width=device-width,initial-scale=1\"><title>KASA 관문 관리</title>\
<style>body{{margin:0;background:#12161c;color:#c8d0d9;font:14px/1.55 -apple-system,system-ui,sans-serif}}main{{max-width:1180px;margin:0 auto;padding:28px 20px 48px}}\
h1{{font-size:20px;margin:0 0 4px;color:#e6ebf0}}.dim{{color:#7d8894}}.sum{{margin:0 0 20px}}\
table{{width:100%;border-collapse:collapse;font-variant-numeric:tabular-nums}}th,td{{text-align:left;padding:9px 10px;border-bottom:1px solid #232a33;vertical-align:top}}\
th{{font-weight:600;color:#9aa5b1;font-size:12px;white-space:nowrap}}td.n{{text-align:right;white-space:nowrap}}th.n{{text-align:right}}\
.who b{{color:#e6ebf0;font-weight:600}}.who code{{display:block;color:#7d8894;font-size:12px}}.tag{{display:inline-block;padding:0 6px;border:1px solid #2e3742;border-radius:4px;font-size:12px;margin:0 4px 2px 0}}\
.off{{color:#e0787a}}.on{{color:#7cc49a}}form{{display:inline}}button{{background:#1d242d;color:#e6ebf0;border:1px solid #2e3742;border-radius:6px;padding:8px 14px;font:inherit;cursor:pointer;margin:0 8px 8px 0}}\
.note{{margin-top:18px;font-size:12px}}.bar{{display:flex;justify-content:space-between;align-items:flex-start;gap:12px}}</style>\
<body><main>{body}</main></body></html>"
    );
    (status, [(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response()
}

fn esc(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// 관문은 미니(한국 시각)에서 돌지만 시간대 표 없이 UTC+9 로 적는다.
pub(super) fn kst(ts: u64) -> String {
    if ts == 0 {
        return "—".into();
    }
    let t = ts as i64 + 9 * 3600;
    let (days, secs) = (t.div_euclid(86_400), t.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year}-{month:02}-{day:02} {:02}:{:02}", secs / 3600, secs % 3600 / 60)
}

fn duration(secs: u64) -> String {
    match secs {
        0 => "—".into(),
        s if s < 3600 => format!("{}분", s.div_ceil(60)),
        s if s < 86_400 => format!("{:.1}시간", s as f64 / 3600.0),
        s => format!("{:.1}일", s as f64 / 86_400.0),
    }
}

fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{n} B") } else { format!("{value:.1} {}", UNITS[unit]) }
}

fn method_label(method: &str) -> &'static str {
    match method {
        "google" => "Google",
        "github" => "GitHub",
        "password" => "비밀번호",
        _ => "알 수 없음",
    }
}

fn dashboard(admin: &str, rows: &[Row]) -> String {
    let now = now_secs();
    let week = rows.iter().filter(|r| r.signup != "password" && r.created + 7 * 86_400 >= now).count();
    let online: usize = rows.iter().map(|r| r.online).sum();
    let mut body = format!(
        "<div class=bar><div><h1>관문 가입자</h1><p class=\"dim sum\">계정 {} · 최근 7일 OAuth 가입 {week} · 지금 붙은 데스크톱 {online} · {} 기준</p></div>\
<form method=post action=\"/relay/admin/logout\"><button type=submit>로그아웃 ({})</button></form></div>\
<table><thead><tr><th>이름</th><th>가입</th><th>로그인 수단</th><th>기기</th><th>마지막 접속</th><th class=n>연결 시간</th><th class=n>중계량</th><th class=n>요청</th><th>상태</th></tr></thead><tbody>",
        rows.len(),
        kst(now),
        esc(admin),
    );
    for row in rows {
        let name = row.display_name.as_deref().unwrap_or(&row.account);
        let methods: String = row
            .login_methods
            .iter()
            .map(|m| format!("<span class=tag>{}</span>", method_label(m)))
            .collect();
        let mut devices = Vec::new();
        if row.desktops > 0 {
            devices.push(format!("데스크톱 {}", row.desktops));
        }
        if row.phones > 0 {
            devices.push(format!("폰 {}", row.phones));
        }
        if devices.is_empty() {
            devices.push("없음".into());
        }
        let extra = [
            (row.online > 0).then(|| format!("<span class=on>접속 중 {}</span>", row.online)),
            (row.revoked > 0).then(|| format!("<span class=dim>끊은 기기 {}</span>", row.revoked)),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        body.push_str(&format!(
            "<tr><td class=who><b>{}</b><code>{}</code></td><td>{}<br><span class=dim>{}{}</span></td><td>{}</td><td>{}{}</td><td>{}</td><td class=n>{}</td><td class=n>{}</td><td class=n>{}</td><td>{}</td></tr>",
            esc(name),
            esc(&row.account),
            method_label(row.signup),
            if row.created_estimated { "~" } else { "" },
            kst(row.created),
            if methods.is_empty() { "<span class=dim>—</span>".into() } else { methods },
            devices.join(" · "),
            if extra.is_empty() { String::new() } else { format!("<br>{extra}") },
            kst(row.last_seen),
            duration(row.connected_secs),
            bytes(row.relayed_bytes),
            row.requests,
            if row.active { "<span class=on>사용 중</span>" } else { "<span class=off>막힘</span>" },
        ));
    }
    if rows.is_empty() {
        body.push_str("<tr><td colspan=9 class=dim>아직 계정이 없어요.</td></tr>");
    }
    body.push_str(
        "</tbody></table><p class=\"dim note\">대화·화면 내용은 관문에 남지 않아 여기에 없어요. 연결 시간·중계량·요청 수는 데스크톱 업링크 기준 대략값이고(폰이 그 기기를 거쳐 쓴 몫 포함), 이 기능을 넣은 뒤부터 쌓여요. \
~ 가 붙은 가입 시각은 가입 기록이 없어 첫 기기 로그인으로 가늠한 값이에요.</p>",
    );
    body
}

fn login_page(gate: &Gate) -> String {
    let buttons: String = [Provider::Google, Provider::Github]
        .into_iter()
        .filter(|provider| gate.oauth.config.enabled(*provider))
        .map(|provider| {
            format!(
                "<form method=post action=\"/relay/admin/login/{}\"><button type=submit>{} 로 로그인</button></form>",
                provider.name(),
                method_label(provider.name())
            )
        })
        .collect();
    format!(
        "<h1>KASA 관문 관리</h1><p class=dim>관리자로 지정된 계정에 연결해 둔 Google·GitHub 로그인으로 들어와요. 연결은 앱 설정 → 계정에서 해요.</p>{}",
        if buttons.is_empty() { "<p class=off>이 관문에 켜진 로그인 수단이 없어요.</p>".into() } else { buttons }
    )
}

async fn home(State(gate): State<Gate>, headers: axum::http::HeaderMap) -> axum::response::Response {
    if !gate.admins.enabled() {
        return json_err(StatusCode::NOT_FOUND, "not_found");
    }
    match gate.admin_of(&headers) {
        Some(admin) => html_page(StatusCode::OK, &(dashboard(&admin, &rows(&gate)) + &install::admin_section(&gate))),
        None => html_page(StatusCode::OK, &login_page(&gate)),
    }
}

async fn accounts_json(State(gate): State<Gate>, headers: axum::http::HeaderMap) -> axum::response::Response {
    if !gate.admins.enabled() {
        return json_err(StatusCode::NOT_FOUND, "not_found");
    }
    if gate.admin_of(&headers).is_none() {
        return json_err(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    axum::Json(serde_json::json!({ "ok": true, "generated": now_secs(), "accounts": rows(&gate) })).into_response()
}

fn same_origin_form(gate: &Gate, headers: &axum::http::HeaderMap) -> bool {
    let origin = headers.get(header::ORIGIN).and_then(|value| value.to_str().ok());
    !gate.oauth.config.origin.is_empty()
        && origin == Some(gate.oauth.config.origin.as_str())
        && headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            == Some("application/x-www-form-urlencoded")
}

async fn login(
    State(gate): State<Gate>,
    AxPath(provider): AxPath<Provider>,
    req: axum::extract::Request,
) -> axum::response::Response {
    if !gate.admins.enabled() {
        return json_err(StatusCode::NOT_FOUND, "not_found");
    }
    if !same_origin_form(&gate, req.headers()) {
        return json_err(StatusCode::FORBIDDEN, "invalid_request");
    }
    if gate.limiter.allow("oauth-admin", &client_ip(&req)).is_err() {
        return json_err(StatusCode::TOO_MANY_REQUESTS, "rate_limited");
    }
    match gate.oauth.start_admin(provider) {
        Ok((url, cookie)) => (StatusCode::SEE_OTHER, [(header::LOCATION, url), (header::SET_COOKIE, cookie)]).into_response(),
        Err(error) => json_err(StatusCode::SERVICE_UNAVAILABLE, error),
    }
}

async fn logout(State(gate): State<Gate>, headers: axum::http::HeaderMap) -> axum::response::Response {
    if !gate.admins.enabled() {
        return json_err(StatusCode::NOT_FOUND, "not_found");
    }
    if !same_origin_form(&gate, &headers) {
        return json_err(StatusCode::FORBIDDEN, "invalid_request");
    }
    gate.admins.end(&headers);
    (
        StatusCode::SEE_OTHER,
        [
            (header::LOCATION, "/relay/admin".to_string()),
            (header::SET_COOKIE, format!("{COOKIE}=; Path=/relay/admin; Secure; HttpOnly; SameSite=Lax; Max-Age=0")),
        ],
    )
        .into_response()
}

/// OAuth 콜백이 관리자 로그인이면 여기로 온다. 신원이 모르는 것이든 관리자 아닌 계정의 것이든
/// 같은 답을 준다 — 이 화면으로 어떤 신원이 가입했는지 떠보지 못하게.
pub(super) fn signed_in(gate: &Gate, identity: Result<Identity, &'static str>) -> axum::response::Response {
    let account = identity
        .ok()
        .and_then(|identity| gate.oauth.lookup(&identity))
        .filter(|account| gate.admins.accounts.contains(account) && gate.account_active(account));
    let Some(account) = account else {
        eprintln!("[admin] 관리 화면 로그인 거절");
        return (StatusCode::FORBIDDEN, "This sign-in is not a KASA relay administrator.").into_response();
    };
    let token = gate.admins.issue(&account);
    eprintln!("[admin] 관리 화면 로그인 — 계정 {account}");
    (
        StatusCode::SEE_OTHER,
        [
            (header::LOCATION, "/relay/admin".to_string()),
            (
                header::SET_COOKIE,
                format!("{COOKIE}={token}; Path=/relay/admin; Secure; HttpOnly; SameSite=Lax; Max-Age={}", SESSION_TTL.as_secs()),
            ),
        ],
    )
        .into_response()
}

#[cfg(test)]
#[path = "gateway_admin/tests.rs"]
mod tests;
