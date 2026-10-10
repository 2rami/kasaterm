//! 누가 들어오는가 — 세션·원격 토큰, Origin/교차 사이트 가드, 원격 peer 판정, 관문·폰 입구 표식,
//! 유저별 폰 주소 `/u/<slug>/…`, 토큰 쿠키.
//!
//! 주소 자체가 자격이다(`mobile.rs` 머리말). 라우팅 **앞**에서 접두를 벗기고
//! `MobileAuth` 를 심으면, 안쪽 라우트와 관문은 로컬 요청처럼 다룬다.

use super::*;
#[cfg(test)]
use super::phone::kasanet_phone_handler;

/// `?t=<토큰>` 으로 들어온 원격 접속에 쿠키를 심어 준다.
///
/// WebSocket 은 커스텀 헤더를 못 붙이므로, 한 번 붙은 뒤 `/term/ws` 와 정적 자산이
/// 인증을 통과하는 경로는 쿠키뿐이다. 폰은 주소를 한 번만 열면 그 다음부터 쿠키로
/// 다닌다.
/// `?t=<토큰>` 이 맞으면 심을 쿠키 문자열. 안 맞거나 없으면 `None`.
///
/// ⚠️ 입구가 하나였을 때는 `term_page_handler` 안에 인라인이었는데, 그러면 그 한
/// 페이지를 먼저 열지 않은 사람은 다른 입구에서 **HTML 만 200 이고 그 페이지의
/// JS·CSS 가 403** 이 된다(= 빈 화면). 토큰을 물고 들어올 수 있는 입구는 전부
/// 이걸 거쳐야 한다.
pub(super) fn remote_token_cookie(q: &std::collections::HashMap<String, String>) -> Option<String> {
    remote_token()
        .filter(|want| q.get("t").map(String::as_str) == Some(*want))
        .map(token_cookie)
}

/// 토큰 쿠키 한 벌. 심는 자리가 셋(`?t=` 입구 · 유저 주소 관문 · 아로나 입구)이라 여기서만 짓는다.
///
/// HttpOnly — 페이지 스크립트가 토큰을 읽을 이유가 없다.
/// SameSite=**Lax** — 전엔 Strict 였는데, 그러면 슬랙·디스코드 알림에서 링크를 눌러
/// 건너오는 **첫 화면에 쿠키가 안 실려 403** 이었다(2026-09-02 지적 「토큰없으면
/// 안봐지고」의 한 축). Lax 는 최상위 이동(GET)에는 실리고 남의 사이트가 띄우는
/// POST·iframe·fetch 에는 안 실린다 — 부작용 있는 창구는 전부 POST 라 그걸로 족하다.
fn token_cookie(want: &str) -> String {
    format!("kasa_token={want}; Path=/; HttpOnly; SameSite=Lax; Max-Age=31536000")
}

/// HTML 응답에 위 쿠키를 붙인다.
pub(super) fn html_with_token_cookie(
    html: &'static str,
    q: &std::collections::HashMap<String, String>,
) -> axum::response::Response {
    let content_type = (header::CONTENT_TYPE, "text/html; charset=utf-8");
    match remote_token_cookie(q) {
        Some(c) => ([content_type, (header::SET_COOKIE, c.as_str())], html).into_response(),
        None => ([content_type], html).into_response(),
    }
}

/// 아무 절대경로나 읽는 창구라 주인만 — 손님 폰 주소로 `~/.config/kasaterm/remote-token`
/// (셸 전권)까지 읽혔다. 기계 사이 터널·토큰 요청은 폰 주소를 안 달고 오므로 그대로 통과한다.
pub(crate) fn guest_denied(req: &axum::extract::Request) -> Option<axum::response::Response> {
    req.extensions()
        .get::<MobileAuth>()
        .is_some_and(|auth| !auth.0.owner)
        .then(|| (axum::http::StatusCode::FORBIDDEN, "owner only").into_response())
}

/// 이 서버 인스턴스의 1회용 토큰. 프로세스가 뜰 때 한 번 만들어진다.
///
/// `with_html` 로 띄우는 패널(세션·보드)은 문서 origin 이 `null` 이라 Origin 검사를
/// 통과할 수 없다. 그렇다고 `null` 을 허용하면 방어가 무너진다 — 악성 사이트가
/// `<iframe sandbox>` 안에서 fetch 하면 그것도 `null` 이기 때문이다. 그래서 그
/// 패널들에는 HTML 을 만들 때 토큰을 심어 주고(`__TOKEN__` 치환, 네트워크로 나가지
/// 않는다), 요청에 실려 온 토큰이 맞으면 통과시킨다.
pub fn session_token() -> &'static str {
    static T: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    T.get_or_init(|| uuid::Uuid::new_v4().to_string())
}

/// 부작용이 있는 요청(POST)이 우리 것인지 가른다.
///
/// ⚠️ **CORS 는 「응답 읽기」를 막지 「실행」을 막지 않는다.** `Content-Type` 이
/// `text/plain` 이면 body 가 있어도 simple request 라 preflight 없이 그냥 실행된다 —
/// 악성 페이지가 응답을 못 읽어도 **부작용은 이미 일어난 뒤**다. 이 서버에는
/// `/send`(pane 에 키 입력) `/spawn-student` `/git-push` `/close-pane` 처럼 명령을
/// 실행하거나 되돌릴 수 없는 창구가 30개 넘게 있어서, wildcard CORS + 127.0.0.1
/// 바인딩만으로는 「사용자가 방문한 아무 웹페이지가 터미널에 명령을 꽂는」 경로가
/// 열려 있었다.
fn mutating_request_ok(h: &HeaderMap) -> bool {
    if has_token(h) {
        return true;
    }
    ws_origin_ok(h)
}

fn has_token(h: &HeaderMap) -> bool {
    h.get("x-kasa-token")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|t| t == session_token())
}

/// 서버가 붙을 주소. 기본은 loopback이고, 여는 것은 **명시적 선택**이어야 한다.
/// 이 서버에는 셸에 바이트를 꽂는 창구가 있다.
///
/// env 다음에 파일을 본다 — **GUI 앱은 env 를 물려받지 않는다**(`open` 이 안
/// 넘기고, Finder 로 띄우면 셸 환경 자체가 없다). 파일이 없으면 앱에서는 원격을
/// 켤 방법이 사실상 `launchctl setenv` 뿐인데 그건 로그인 세션 전역이라 거칠다.
///
/// ```json
/// // ~/.config/kasaterm/remote.json
/// { "bind": "0.0.0.0" }
/// ```
pub(super) fn bind_addr() -> String {
    if let Some(v) = std::env::var("KASATERM_BIND")
        .ok()
        .filter(|s| !s.trim().is_empty())
    {
        return v;
    }
    remote_conf_bind().unwrap_or_else(|| "127.0.0.1".to_string())
}

fn remote_conf_bind() -> Option<String> {
    let home = kasa_socket::home_dir()?;
    let path = home.join(".config/kasaterm/remote.json");
    let raw = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    v.get("bind")?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn remote_token_path() -> Option<std::path::PathBuf> {
    let home = kasa_socket::home_dir()?;
    Some(home.join(".config/kasaterm/remote-token"))
}

/// 원격 접속용 토큰. `session_token` 과 달리 **디스크에 남는다** — 프로세스마다
/// 새로 만들면 폰 북마크가 앱을 껐다 켤 때마다 깨져서 쓸 수가 없다.
///
/// 이 토큰 하나면 셸에 임의 입력을 꽂을 수 있으므로 파일은 0600 으로 만든다.
pub fn remote_token() -> Option<&'static str> {
    static T: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let path = remote_token_path()?;
        if let Some(existing) = std::fs::read_to_string(&path)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
        {
            return Some(existing);
        }
        let fresh = uuid::Uuid::new_v4().to_string();
        std::fs::create_dir_all(path.parent()?).ok()?;
        std::fs::write(&path, &fresh).ok()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        Some(fresh)
    })
    .as_deref()
}

/// loopback 밖에서 온 연결인가. `ConnectInfo` 가 없으면(=연결 정보를 안 붙인
/// 경로) 원격이 아닌 것으로 본다 — 바인딩이 loopback 이면 원격 자체가 불가능하다.
///
/// ⚠️ **peer 주소만으로는 터널을 못 가른다.** `cloudflared` 같은 터널과 리버스
/// 프록시는 **같은 머신에서 loopback 으로** 붙는다. 그래서 밖에서 들어온 요청이
/// peer 로는 로컬로 보이고, 아래 토큰 관문을 통째로 건너뛴다 — 바인딩이
/// `127.0.0.1` 그대로인데도 **터널 주소를 아는 사람이 무인증으로 셸에 닿는다.**
/// 그러니 프록시가 붙이는 원-클라이언트 헤더가 있으면 그것만으로 원격으로 본다.
///
/// 이 판정은 한쪽으로만 틀릴 수 있다: 우리 코드도 브라우저도 이 헤더를 보내지
/// 않으니 로컬 경로는 그대로고, 로컬에서 굳이 위조해 붙여도 **토큰을 더 요구받을
/// 뿐**이라 느슨해지는 방향이 없다.
pub(crate) fn is_remote_peer(req: &axum::extract::Request) -> bool {
    if req.extensions().get::<ViaUplink>().is_some() {
        return true;
    }
    let h = req.headers();
    if h.contains_key("cf-connecting-ip") || h.contains_key("x-forwarded-for") {
        return true;
    }
    req.extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .is_some_and(|ci| !ci.0.ip().is_loopback())
}

/// 요청에 실려온 원격 토큰이 맞는가. 헤더 → 쿠키 → 쿼리 순으로 본다.
///
/// 셋이 다 필요하다: WebSocket 은 커스텀 헤더를 못 붙이니 **쿠키**가 실제 경로이고,
/// **쿼리**는 폰이 처음 붙을 때(북마크·QR) 쓰는 입구이며, **헤더**는 CLI 용이다.
fn has_remote_token(h: &HeaderMap, query: Option<&str>) -> bool {
    let Some(want) = remote_token() else {
        return false;
    };
    if h.get("x-kasa-token").and_then(|v| v.to_str().ok()) == Some(want) {
        return true;
    }
    if let Some(cookies) = h.get(header::COOKIE).and_then(|v| v.to_str().ok()) {
        if cookies
            .split(';')
            .any(|kv| kv.trim().strip_prefix("kasa_token=") == Some(want))
        {
            return true;
        }
    }
    query.is_some_and(|q| q.split('&').any(|kv| kv.strip_prefix("t=") == Some(want)))
}

/// 남의 사이트에서 건너온 요청인가.
///
/// ⚠️ **Origin 검사만으로는 GET 을 못 막는다.** `location = "…/open-markdown?…"`
/// 같은 top-level navigation 은 **Origin 헤더를 아예 보내지 않아서** 「Origin 이
/// 없으면 로컬 CLI」라는 판정을 그대로 통과한다. 그러면 응답을 못 읽어도 **일은
/// 이미 벌어진다** — 이 서버의 GET 에는 창을 띄우는 것(`/open-markdown`), 상태를
/// 바꾸는 것(`/repersona`), 대화·파일 내용을 내주는 것(`/peek` `/transcript`
/// `/list-dir`)이 섞여 있고, wildcard CORS 때문에 그 응답은 실제로 읽힌다.
///
/// `Sec-Fetch-Site` 는 그 구멍을 정확히 메운다 — **브라우저는 navigation 을 포함해
/// 항상 보내고, curl 같은 로컬 도구는 보내지 않는다.** 그래서 `cross-site` 하나만
/// 거부하면 「남의 웹페이지」만 걸러지고, 주소창 직접 입력(`none`)·우리 페이지
/// (`same-origin`)·로컬 CLI(헤더 없음)는 전부 살아남는다. 보수적으로 cross-site
/// 만 본다 — 브라우저마다 값이 갈리는 회색지대를 막았다가 webview 를 통째로
/// 죽이는 쪽이 더 나쁘다.
fn cross_site_request(h: &HeaderMap) -> bool {
    h.get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == "cross-site")
}

/// POST 를 전부 통과시키는 관문. `Router::layer` 로 걸린다.
///
/// GET 은 통과시킨다 — 이 서버의 GET 은 읽기 전용이고, 막으면 webview 폴링이
/// 죽는다. 부작용은 POST 에 모여 있다. 로컬 CLI·MCP 클라이언트는 Origin 을 아예
/// 안 보내므로 그대로 통과한다(이미 같은 사용자 권한으로 도는 프로세스라 막아도
/// 얻는 게 없다).
pub(crate) async fn origin_guard_mw(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    // 유저 주소(`/u/<slug>/…`)로 들어왔으면 주소 자체가 자격이다 — `mobile_prefix_mw` 가
    // slug 를 이미 대조했고, 남의 사이트는 그 slug 를 모르니 교차출처 검사도 필요 없다.
    if req.extensions().get::<MobileAuth>().is_some() {
        return next.run(req).await;
    }
    // 업링크는 언제나 `/u/<slug>/` 를 붙여 되쏜다. 그런데도 유저 주소를 안 거쳤으면 경로를
    // 비틀어 접두를 빠져나온 것이다 — 토큰이 실려 있어도 받지 않는다.
    if req.extensions().get::<ViaUplink>().is_some() {
        eprintln!("[http] 관문 경유 요청이 유저 주소 밖을 가리켜 거부했습니다: {}", req.uri().path());
        return (axum::http::StatusCode::FORBIDDEN, "gateway requests must stay under /u/<slug>/").into_response();
    }
    // 원격(loopback 밖)은 **토큰이 유일한 관문**이다. 아래 로컬 규칙을 그대로
    // 물려주면 안 된다 — 「Origin 이 없으면 로컬 CLI 라 통과」의 근거가 "이미 같은
    // 사용자 권한으로 도는 프로세스"인데 원격에는 그게 성립하지 않는다. 그대로 두면
    // 바인딩을 여는 순간 Origin 없는 요청(curl 한 줄)이 전부 무인증으로 셸에 닿는다.
    if is_remote_peer(&req) {
        let h = req.headers();
        if cross_site_request(h) || !has_remote_token(h, req.uri().query()) {
            eprintln!(
                "[http] 원격 요청을 거부했습니다: {} {}",
                req.method(),
                req.uri().path()
            );
            return (
                axum::http::StatusCode::FORBIDDEN,
                "remote access requires a valid token",
            )
                .into_response();
        }
        return next.run(req).await;
    }
    let h = req.headers();
    // ① 메서드를 가리지 않는다 — GET 에도 창을 띄우거나 대화를 내주는 창구가 있고,
    //    navigation 은 Origin 을 안 보내 ②만으로는 못 잡는다.
    let blocked = if has_token(h) {
        false
    } else {
        cross_site_request(h) || (req.method() == Method::POST && !mutating_request_ok(h))
    };
    if blocked {
        eprintln!(
            "[http] 교차 출처 요청을 거부했습니다: {} {} (origin {:?}, sec-fetch-site {:?})",
            req.method(),
            req.uri().path(),
            h.get(header::ORIGIN),
            h.get("sec-fetch-site")
        );
        return (
            axum::http::StatusCode::FORBIDDEN,
            "cross-origin request refused",
        )
            .into_response();
    }
    next.run(req).await
}

/// 이 웹소켓 연결이 우리 페이지에서 온 것인가.
///
/// ⚠️ **웹소켓은 same-origin 정책의 보호를 받지 않는다.** 브라우저는 임의 출처
/// 페이지가 `ws://127.0.0.1:<port>` 로 연결하는 걸 막지 않고 CORS preflight 도
/// 없다. 즉 127.0.0.1 바인딩은 「다른 기기」만 막을 뿐, **사용자가 방문한 아무
/// 웹페이지가 이 셸을 잡아 임의 명령을 실행하는** 경로는 그대로 열려 있다.
/// 다른 라우트의 wildcard CORS 를 정당화하던 "local-only" 논리가 여기엔 통하지
/// 않는다 — 읽기 전용 JSON 과 셸은 위험의 급이 다르다.
///
/// 그래서 Origin 을 직접 본다. Origin 이 아예 없으면 브라우저가 아닌 로컬
/// 클라이언트(curl·스크립트)이고, 그건 이미 같은 사용자 권한으로 도는 프로세스라
/// 막아도 얻는 게 없다.
pub(crate) fn ws_origin_ok(h: &HeaderMap) -> bool {
    let Some(origin) = h.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let host = h
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let o = origin.split_once("://").map(|(_, rest)| rest).unwrap_or("");
    // Origin 은 우리 Host 와 **정확히** 같아야 한다. 부분 일치로 봤다면
    // `127.0.0.1.evil.com` 이 통과한다.
    //
    // 호스트명을 127.0.0.1/localhost 로 못박지는 않는다 — 폰이 LAN IP 나 터널
    // 주소로 붙으면 Host 가 그 주소이고, 그때도 우리 페이지에서 온 요청은
    // 통과해야 한다. 브라우저는 Host 를 조작할 수 없고(실제 연결 대상으로
    // 채워진다), 원격 연결은 `origin_guard_mw` 가 토큰으로 이미 걸러 낸 뒤다.
    !host.is_empty() && o == host
}

/// 유저 주소로 들어온 요청임을 라우트 안쪽에 알리는 표식.
#[derive(Clone)]
pub(crate) struct MobileAuth(pub crate::mobile::MobileUser);

/// 관문(업링크)을 거쳐 들어온 요청이라는 표식 — 업링크 전용 입구 리스너가 심는다.
/// 업링크의 되쏘기는 loopback 이라 peer 주소로는 로컬 CLI 와 구분이 안 된다.
#[derive(Clone, Copy)]
pub(crate) struct ViaUplink;

pub(super) async fn via_uplink_mw(mut req: axum::extract::Request, next: axum::middleware::Next) -> axum::response::Response {
    req.extensions_mut().insert(ViaUplink);
    next.run(req).await
}

/// 카사넷으로 들어온 폰 연결이라는 표식 — 폰 입구 리스너가 `ViaUplink` 와 함께 심는다(`kasanet::allow_phone`).
#[derive(Clone, Copy)]
pub(crate) struct ViaPhone;

/// 폰 입구. 관문 업링크가 되쏘는 것과 똑같이 주인 주소(`/u/<주인>/`) 아래로 고쳐 쓰고 `ViaUplink` 를 단다 —
/// 폰이 카사넷으로 와도 관문 경유보다 넓은 자격을 얻지 않는다. 관문이 걷는 자격류 헤더도 여기서 걷는다.
pub(super) async fn via_phone_mw(mut req: axum::extract::Request, next: axum::middleware::Next) -> axum::response::Response {
    let Some(owner) = crate::mobile::owner() else {
        return (axum::http::StatusCode::SERVICE_UNAVAILABLE, "no owner address").into_response();
    };
    let pq = req.uri().path_and_query().map_or("/", |p| p.as_str()).to_string();
    let Ok(uri) = format!("{}{}{pq}", crate::mobile::PREFIX, owner.slug).parse::<axum::http::Uri>() else {
        return (axum::http::StatusCode::BAD_REQUEST, "bad path").into_response();
    };
    *req.uri_mut() = uri;
    for h in ["cookie", "authorization", "x-kasa-token", "origin", "referer"] {
        req.headers_mut().remove(h);
    }
    req.extensions_mut().insert(ViaUplink);
    req.extensions_mut().insert(ViaPhone);
    next.run(req).await
}

/// 유저 주소 응답에 원격 토큰 쿠키를 심을까. 절대경로(`/settings/…`)로 부르는 옛 fetch
/// 를 위한 보조인데, 값이 이 기계의 원격 토큰(셸 전권)이라 **주인에게만**, 관문 경유로는
/// 아예 안 준다 — 관문 너머 절대경로는 어차피 이 기계에 안 닿는다. 이미 물고 있으면 안 건드린다.
fn wants_token_cookie(user: &crate::mobile::MobileUser, via_uplink: bool, h: &HeaderMap) -> bool {
    user.owner && !via_uplink && !has_remote_token(h, None)
}

/// 라우팅 앞에 두르는 레이어. `/u/<slug>/term/grid` 를 `/term/grid` 로 고쳐 쓴다.
/// ⚠️ `Router::layer` 로 걸면 안 된다 — 그건 라우팅 **뒤**라 경로가 이미 404 다.
/// `spawn_http_server_opts` 가 ServiceBuilder 로 라우터 바깥에 건다.
pub(super) async fn mobile_prefix_mw(
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use crate::mobile::Rewrite;
    let query = req.uri().query().map(|q| format!("?{q}")).unwrap_or_default();
    match crate::mobile::rewrite(req.uri().path()) {
        Rewrite::NotOurs => next.run(req).await,
        // 있는지 없는지 구분되지 않게 한 종류로 — 주소를 맞혀 보는 쪽에 힌트를 안 준다.
        Rewrite::Unknown => {
            (axum::http::StatusCode::NOT_FOUND, "no such address").into_response()
        }
        Rewrite::NeedSlash(slug) => axum::response::Redirect::temporary(&format!(
            "{}{slug}/{query}",
            crate::mobile::PREFIX
        ))
        .into_response(),
        Rewrite::Route { user, path } => {
            if !crate::uplink::safe_path(&path) {
                return (axum::http::StatusCode::BAD_REQUEST, "bad path").into_response();
            }
            let Ok(uri) = format!("{path}{query}").parse::<axum::http::Uri>() else {
                return (axum::http::StatusCode::BAD_REQUEST, "bad path").into_response();
            };
            let need_cookie = wants_token_cookie(&user, req.extensions().get::<ViaUplink>().is_some(), req.headers());
            *req.uri_mut() = uri;
            req.extensions_mut().insert(MobileAuth(user));
            let mut res = next.run(req).await;
            if need_cookie {
                if let Some(v) = remote_token()
                    .map(token_cookie)
                    .and_then(|c| axum::http::HeaderValue::from_str(&c).ok())
                {
                    res.headers_mut().append(header::SET_COOKIE, v);
                }
            }
            res
        }
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn gateway_requests_must_stay_under_a_user_address() {
        use super::*;
        use axum::http::StatusCode;
        let inner = axum::Router::new()
            .route("/version", get(|| async { "local" }))
            .layer(axum::middleware::from_fn(origin_guard_mw));
        let via = tower::ServiceBuilder::new()
            .layer(axum::middleware::from_fn(via_uplink_mw))
            .service(inner.clone());
        use axum::ServiceExt as _;
        let l1 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let direct = format!("http://{}", l1.local_addr().unwrap());
        let t1 = tokio::spawn(async move {
            axum::serve(l1, inner.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap();
        });
        let l2 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gateway = format!("http://{}", l2.local_addr().unwrap());
        let t2 = tokio::spawn(async move {
            axum::serve(l2, via.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap();
        });
        let client = reqwest::Client::new();
        assert_eq!(client.get(format!("{direct}/version")).send().await.unwrap().status(), StatusCode::OK);
        assert_eq!(client.get(format!("{gateway}/version")).send().await.unwrap().status(), StatusCode::FORBIDDEN);
        // 토큰 값과 무관하게 막혀야 한다 — 관문 경유 판정이 토큰 검사보다 앞선다.
        let with_token = client.get(format!("{gateway}/version")).header("x-kasa-token", "any").send().await.unwrap();
        assert_eq!(with_token.status(), StatusCode::FORBIDDEN, "관문 경유에 토큰이 통했다");
        t1.abort();
        t2.abort();
    }

    #[tokio::test]
    async fn phone_ingress_gets_gateway_rights_and_cannot_renew_itself() {
        use super::*;
        use axum::http::StatusCode;
        use axum::ServiceExt as _;
        let dir = std::env::temp_dir().join(format!("kasa-phone-ingress-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("KASATERM_MOBILE_USERS", dir.join("mobile-users.json"));
        let owner = crate::mobile::owner().unwrap();
        let inner = axum::Router::new()
            .route("/probe", get(|req: axum::extract::Request| async move {
                let e = req.extensions();
                let owner = e.get::<MobileAuth>().is_some_and(|a| a.0.owner);
                let cookie = req.headers().contains_key("cookie");
                format!("{} {} {owner} {cookie}", e.get::<ViaUplink>().is_some(), e.get::<ViaPhone>().is_some())
            }))
            .route("/kasanet/phone", axum::routing::post(kasanet_phone_handler))
            .layer(axum::middleware::from_fn(origin_guard_mw));
        let app = tower::ServiceBuilder::new().layer(axum::middleware::from_fn(mobile_prefix_mw)).service(inner);
        let bind = || async { tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap() };
        let (l_direct, l_phone, l_up) = (bind().await, bind().await, bind().await);
        let at = |l: &tokio::net::TcpListener| format!("http://{}", l.local_addr().unwrap());
        let (direct, phone, up) = (at(&l_direct), at(&l_phone), at(&l_up));
        let via_phone = tower::ServiceBuilder::new().layer(axum::middleware::from_fn(via_phone_mw)).service(app.clone());
        let via_up = tower::ServiceBuilder::new().layer(axum::middleware::from_fn(via_uplink_mw)).service(app.clone());
        let tasks = [
            tokio::spawn(async move { axum::serve(l_direct, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap() }),
            tokio::spawn(async move { axum::serve(l_phone, via_phone.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap() }),
            tokio::spawn(async move { axum::serve(l_up, via_up.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap() }),
        ];
        let c = reqwest::Client::new();
        // 폰 입구: 주소를 몰라도 주인 주소 아래 관문 자격으로 들어가고, 실어 온 쿠키는 걷힌다.
        let probe = c.get(format!("{phone}/probe")).header("cookie", "kasa_token=x").send().await.unwrap();
        assert_eq!(probe.text().await.unwrap(), "true true true false");
        let reg = |base: String| {
            let c = c.clone();
            async move { c.post(format!("{base}/kasanet/phone")).body(r#"{"id":"x"}"#).send().await.unwrap() }
        };
        assert_eq!(reg(phone.clone()).await.status(), StatusCode::FORBIDDEN, "카사넷으로 온 등록은 받지 않는다");
        assert_eq!(reg(direct.clone()).await.status(), StatusCode::FORBIDDEN, "관문을 안 거친 등록은 받지 않는다");
        // 관문(주인 주소)으로 온 등록은 자격을 통과한다 — 시험에는 카사넷이 없어 503.
        let ok = reg(format!("{up}{}{}", crate::mobile::PREFIX, owner.slug)).await;
        assert_eq!(ok.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(ok.json::<serde_json::Value>().await.unwrap()["error"], "kasanet_off");
        for t in tasks {
            t.abort();
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_reads_are_owner_only_for_phone_addresses() {
        let guest = crate::mobile::MobileUser { name: "손님".into(), slug: "abcdefghijklmnopqrstuvwxy".into(), created: 0, owner: false };
        let owner = crate::mobile::MobileUser { owner: true, ..guest.clone() };
        let req = |user: Option<crate::mobile::MobileUser>| {
            let mut r = axum::extract::Request::new(axum::body::Body::empty());
            if let Some(u) = user {
                r.extensions_mut().insert(super::MobileAuth(u));
            }
            r
        };
        assert_eq!(super::guest_denied(&req(Some(guest))).map(|r| r.status()), Some(axum::http::StatusCode::FORBIDDEN));
        assert!(super::guest_denied(&req(Some(owner))).is_none());
        assert!(super::guest_denied(&req(None)).is_none(), "machine-to-machine tunnels carry no phone address");
    }

    #[test]
    fn token_cookie_is_withheld_from_guests_and_gateway() {
        let guest = crate::mobile::MobileUser { name: "손님".into(), slug: "abcdefghijklmnopqrstuvwxy".into(), created: 0, owner: false };
        let owner = crate::mobile::MobileUser { owner: true, ..guest.clone() };
        let h = axum::http::HeaderMap::new();
        assert!(!super::wants_token_cookie(&guest, false, &h));
        assert!(!super::wants_token_cookie(&owner, true, &h));
    }

    #[test]
    fn token_cookie_is_lax_not_strict() {
        // Strict 면 슬랙·디스코드 링크에서 건너오는 첫 화면에 쿠키가 안 실려 403 이다.
        let c = super::token_cookie("abc");
        assert!(c.contains("SameSite=Lax"), "{c}");
        assert!(c.contains("HttpOnly"));
        assert!(c.starts_with("kasa_token=abc;"));
    }
}

#[cfg(test)]
mod remote_peer_tests {
    use super::is_remote_peer;

    fn req(headers: &[(&str, &str)], peer: Option<&str>) -> axum::extract::Request {
        let mut b = axum::http::Request::builder().uri("/term/ws");
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        let mut r = b.body(axum::body::Body::empty()).unwrap();
        if let Some(p) = peer {
            r.extensions_mut().insert(axum::extract::ConnectInfo(
                p.parse::<std::net::SocketAddr>().unwrap(),
            ));
        }
        r
    }

    #[test]
    fn plain_loopback_stays_local() {
        assert!(!is_remote_peer(&req(&[], Some("127.0.0.1:51234"))));
        assert!(!is_remote_peer(&req(&[], Some("[::1]:51234"))));
        assert!(!is_remote_peer(&req(&[], None)));
    }

    #[test]
    fn another_machine_is_remote() {
        assert!(is_remote_peer(&req(&[], Some("192.168.0.7:51234"))));
    }

    /// 터널은 같은 머신에서 loopback 으로 붙는다. peer 만 보면 로컬로 보여
    /// 토큰 관문을 건너뛰므로, 프록시 헤더 하나로도 원격이어야 한다.
    #[test]
    fn tunnel_arriving_over_loopback_is_remote() {
        for h in ["cf-connecting-ip", "x-forwarded-for"] {
            assert!(
                is_remote_peer(&req(&[(h, "203.0.113.9")], Some("127.0.0.1:51234"))),
                "{h} 가 붙었는데도 로컬로 봤다"
            );
        }
    }
}
