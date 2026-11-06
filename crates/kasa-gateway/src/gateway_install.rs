//! Ad Hoc 설치 창구 — TestFlight 없이 등록된 폰·아이패드에 앱을 링크로 넣는다(`/relay/install/…`).
//!
//! - 판은 상태 폴더의 `relay-install/<token>/{kasaterm.ipa, meta.json}` 이고 `relay-install/latest` 가 지금 판의
//!   token 이다. 올리는 쪽(`mobile/tool/adhoc.sh`)이 ssh 로 쓴다 — 관문은 읽기만 한다.
//! - itms-services 는 인증 헤더도 쿠키도 못 싣는다. 그래서 판마다 새로 뽑는 token 이 곧 자격이다. 받아 가도
//!   서명에 등록된 기기에만 깔리고, 든 것은 공개 레포의 앱뿐이다.
//! - `latest` 는 관리자 계정의 기기 토큰으로만 준다 — 폰 앱이 「새 판」을 알리는 데 쓴다. 등록되지 않은
//!   기기의 사용자에게 못 까는 판을 알리지 않는다. `?have=<빌드>&wait=<초>` 면 그 빌드와 다른 판이 올라올
//!   때까지 붙들고 있다가 올라오는 즉시 답한다 — 폰이 쓰는 중에도 새 판을 곧바로 알린다.
//! - 등록 안 된 기기는 같은 페이지에서 스스로 등록한다(`gateway_install/enroll.rs`). 서명기가 없는 관문은 그
//!   단계를 감춘다.

use super::*;
use serde::Deserialize;

#[path = "gateway_install/enroll.rs"]
mod enroll;
pub(super) use enroll::Enroll;

const IPA: &str = "kasaterm.ipa";
const CHUNK: usize = 256 * 1024;

pub(super) fn routes() -> Router<Gate> {
    Router::new()
        .route("/relay/install/latest", get(latest))
        .route("/relay/install/{token}", get(page))
        .route("/relay/install/{token}/", get(page))
        .route("/relay/install/{token}/manifest.plist", get(manifest))
        .route(&format!("/relay/install/{{token}}/{IPA}"), get(ipa))
        .route("/relay/install/{token}/enroll.mobileconfig", get(enroll::profile))
        .route("/relay/install/{token}/enroll", axum::routing::post(enroll::attempt))
        .route("/relay/install/{token}/r/{job}", get(enroll::status))
        .layer(axum::middleware::map_response(private_response))
}

/// 링크가 자격이라 어디에도 남기지 않는다 — 캐시·Referer·검색 색인 모두.
async fn private_response(mut response: axum::response::Response) -> axum::response::Response {
    for (key, value) in [
        ("cache-control", "no-store"),
        ("referrer-policy", "no-referrer"),
        ("x-robots-tag", "noindex, nofollow"),
        ("x-content-type-options", "nosniff"),
        ("content-security-policy", "default-src 'none'; style-src 'unsafe-inline'; frame-ancestors 'none'; base-uri 'none'"),
    ] {
        response.headers_mut().insert(
            axum::http::HeaderName::from_static(key),
            axum::http::HeaderValue::from_static(value),
        );
    }
    response
}

#[derive(Deserialize)]
struct Meta {
    bundle_id: String,
    version: String,
    build: String,
    title: String,
    #[serde(default)]
    uploaded: u64,
}

fn valid_token(token: &str) -> bool {
    (24..=64).contains(&token.len()) && token.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn plain(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}

impl Gate {
    fn release(&self, token: &str) -> Option<(PathBuf, Meta)> {
        if !valid_token(token) {
            return None;
        }
        let dir = self.install_dir.as_ref()?.join(token);
        let meta: Meta = serde_json::from_slice(&std::fs::read(dir.join("meta.json")).ok()?).ok()?;
        let ok = [&meta.bundle_id, &meta.version, &meta.build, &meta.title].iter().all(|v| plain(v));
        (ok && dir.join(IPA).is_file()).then_some((dir, meta))
    }

    fn current_release(&self) -> Option<(String, Meta)> {
        let token = std::fs::read_to_string(self.install_dir.as_ref()?.join("latest")).ok()?;
        let token = token.trim().to_string();
        let (_, meta) = self.release(&token)?;
        Some((token, meta))
    }

    /// 매니페스트의 ipa 주소는 절대 주소여야 한다. 관문의 공개 주소를 알면 그것을, 모르면 들어온 Host 를 쓴다.
    fn public_origin(&self, headers: &axum::http::HeaderMap) -> Option<String> {
        if !self.oauth.config.origin.is_empty() {
            return Some(self.oauth.config.origin.clone());
        }
        let host = headers.get(header::HOST)?.to_str().ok()?;
        let ok = !host.is_empty() && host.bytes().all(|b| b.is_ascii_alphanumeric() || b".-:[]".contains(&b));
        ok.then(|| format!("https://{host}"))
    }
}

fn links(origin: &str, token: &str) -> (String, String) {
    let page = format!("{origin}/relay/install/{token}/");
    let manifest = format!("{page}manifest.plist");
    let install = format!("itms-services://?action=download-manifest&url={}", percent(&manifest));
    (page, install)
}

fn percent(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn esc(value: &str) -> String {
    value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

fn gone() -> axum::response::Response {
    html(
        StatusCode::NOT_FOUND,
        "<h1>지난 설치 링크예요</h1><p class=dim>새 판이 올라오면 앞의 링크는 하나 뒤 판까지만 남아요. 새 링크를 받아 주세요.</p>",
    )
}

fn html(status: StatusCode, body: &str) -> axum::response::Response {
    let page = format!(
        "<!doctype html><html lang=ko><meta charset=utf-8><meta name=viewport content=\"width=device-width,initial-scale=1\">\
<title>KASATERM 설치</title><style>body{{margin:0;background:#12161c;color:#c8d0d9;font:16px/1.55 -apple-system,system-ui,sans-serif}}\
main{{max-width:420px;margin:0 auto;padding:48px 24px}}h1{{font-size:22px;margin:0 0 6px;color:#e6ebf0}}.dim{{color:#7d8894;font-size:14px}}\
a{{color:#e6ebf0}}a.btn{{display:block;margin:28px 0 18px;padding:14px;border-radius:6px;background:#e6ebf0;color:#12161c;text-align:center;font-weight:600;text-decoration:none}}</style>\
<body><main>{body}</main></body></html>"
    );
    (status, [(header::CONTENT_TYPE, "text/html; charset=utf-8")], page).into_response()
}

async fn page(State(gate): State<Gate>, AxPath(token): AxPath<String>, headers: axum::http::HeaderMap) -> axum::response::Response {
    let (Some((_, meta)), Some(origin)) = (gate.release(&token), gate.public_origin(&headers)) else {
        return gone();
    };
    let (_, install) = links(&origin, &token);
    let head = format!("<h1>{}</h1><p class=dim>{} · 빌드 {}</p>", esc(&meta.title), esc(&meta.version), esc(&meta.build));
    let enroll = format!("/relay/install/{token}/enroll.mobileconfig");
    let known = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .any(|pair| pair.trim().split_once('=').is_some_and(|(k, _)| k == enroll::COOKIE));
    let body = if gate.enroll.signer.is_none() {
        format!(
            "{head}<a class=btn href=\"{}\">설치</a>\
<p class=dim>등록된 폰·아이패드에서만 깔려요. 누르면 묻는 창에서 「설치」를 고르고 홈 화면에서 아이콘이 받아지는 것을 보세요.</p>",
            esc(&install)
        )
    } else if known {
        format!(
            "{head}<a class=btn href=\"{}\">설치</a>\
<p class=dim>누르면 묻는 창에서 「설치」를 고르세요. 처음 쓰는 다른 기기라면 그 기기에서 <a href=\"{}\">기기 등록</a>부터.</p>",
            esc(&install),
            esc(&enroll)
        )
    } else {
        format!(
            "{head}<a class=btn href=\"{}\">처음이면 — 기기 등록</a>\
<p class=dim>「허용」을 누른 뒤 설정 앱 위쪽의 「프로파일이 다운로드됨」에서 「설치」를 한 번 누르면, 이 기기가 등록되고 \
설치 화면으로 돌아와요. 기기에는 아무것도 남지 않아요.</p>\
<p class=dim>이미 등록한 기기라면 <a href=\"{}\">바로 설치</a>.</p>",
            esc(&enroll),
            esc(&install)
        )
    };
    html(StatusCode::OK, &body)
}

async fn manifest(State(gate): State<Gate>, AxPath(token): AxPath<String>, headers: axum::http::HeaderMap) -> axum::response::Response {
    let (Some((_, meta)), Some(origin)) = (gate.release(&token), gate.public_origin(&headers)) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>items</key><array><dict>
<key>assets</key><array><dict><key>kind</key><string>software-package</string><key>url</key><string>{}</string></dict></array>
<key>metadata</key><dict><key>bundle-identifier</key><string>{}</string><key>bundle-version</key><string>{}</string><key>kind</key><string>software</string><key>title</key><string>{}</string></dict>
</dict></array></dict></plist>
"#,
        esc(&format!("{origin}/relay/install/{token}/{IPA}")),
        esc(&meta.bundle_id),
        esc(&meta.version),
        esc(&meta.title),
    );
    ([(header::CONTENT_TYPE, "application/xml")], plist).into_response()
}

async fn ipa(State(gate): State<Gate>, AxPath(token): AxPath<String>) -> axum::response::Response {
    let Some((dir, _)) = gate.release(&token) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(file) = std::fs::File::open(dir.join(IPA)) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let (tx, rx) = mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(4);
    tokio::task::spawn_blocking(move || {
        use std::io::Read as _;
        let mut file = file;
        loop {
            let mut buf = vec![0; CHUNK];
            match file.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    buf.truncate(n);
                    if tx.blocking_send(Ok(buf.into())).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    let _ = tx.blocking_send(Err(e));
                    break;
                }
            }
        }
    });
    (
        [(header::CONTENT_TYPE, "application/octet-stream".to_string()), (header::CONTENT_LENGTH, len.to_string())],
        axum::body::Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx)),
    )
        .into_response()
}

#[derive(Deserialize)]
struct LatestQuery {
    have: Option<String>,
    #[serde(default)]
    wait: u64,
}

/// 붙들고 있는 상한. 공용 주소 앞의 터널이 100초에 끊으므로 그보다 넉넉히 짧게.
const LATEST_WAIT_MAX: u64 = 50;

async fn latest(
    State(gate): State<Gate>,
    headers: axum::http::HeaderMap,
    axum::extract::Query(q): axum::extract::Query<LatestQuery>,
) -> axum::response::Response {
    let Some((_, device)) = gate.device_of(&headers) else {
        return json_err(StatusCode::UNAUTHORIZED, "unauthorized");
    };
    if !gate.is_admin(&device.account) {
        return json_err(StatusCode::FORBIDDEN, "forbidden");
    }
    if let Some(have) = q.have.as_deref() {
        // 올리는 쪽은 ssh 로 파일만 쓰고 관문에 알리지 않는다 — 1초마다 `latest` 를 다시 읽는다(작은 파일 하나).
        let until = tokio::time::Instant::now() + Duration::from_secs(q.wait.min(LATEST_WAIT_MAX));
        while gate.current_release().map(|(_, m)| m.build).unwrap_or_default() == have
            && tokio::time::Instant::now() < until
        {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
    let release = gate.current_release().zip(gate.public_origin(&headers)).map(|((token, meta), origin)| {
        let (page, install) = links(&origin, &token);
        serde_json::json!({
            "bundle_id": meta.bundle_id, "version": meta.version, "build": meta.build,
            "uploaded": meta.uploaded, "page": page, "install": install,
        })
    });
    // `waits` 가 없는 옛 관문은 have·wait 를 모른 채 곧바로 답한다 — 폰은 그때 천천히 다시 묻는다.
    axum::Json(serde_json::json!({ "ok": true, "release": release, "waits": true })).into_response()
}

/// 관리 화면(`/relay/admin`)의 「설치 링크 · 기기」 칸. 기기 수는 서명기가 등록·재서명 때마다 적는 `devices.json` 을 읽는다
/// — 화면을 열 때마다 애플에 묻지 않는다.
pub(super) fn admin_section(gate: &Gate) -> String {
    let Some(dir) = &gate.install_dir else { return String::new() };
    let mut out = String::from("<div style=\"margin-top:36px\"><h1>설치 링크 · 기기</h1>");
    let release = gate
        .current_release()
        .map(|(_, m)| format!("{} {} · 빌드 {} · {} 올림", esc(&m.title), esc(&m.version), esc(&m.build), admin::kst(m.uploaded)))
        .unwrap_or_else(|| "올라온 판 없음".into());
    let auto = if gate.enroll.signer.is_some() {
        format!("자동 등록 켜짐 · 오늘 새로 {} / 하루 {}대", gate.registered_today(), gate.enroll.daily_cap())
    } else {
        "자동 등록 꺼짐(서명기 없음)".into()
    };
    out.push_str(&format!("<p class=\"dim sum\">{release} · {auto}</p>"));
    #[derive(Deserialize)]
    struct Count {
        enabled: u64,
        disabled: u64,
    }
    #[derive(Deserialize)]
    struct Devices {
        updated: u64,
        limit: u64,
        classes: HashMap<String, Count>,
    }
    match std::fs::read(dir.join("devices.json")).ok().and_then(|b| serde_json::from_slice::<Devices>(&b).ok()) {
        Some(d) => {
            let mut classes: Vec<_> = d.classes.iter().collect();
            classes.sort_by_key(|(k, _)| k.as_str());
            out.push_str("<table><thead><tr><th>기종</th><th class=n>쓰는 기기</th><th class=n>해지</th><th class=n>올해 남은 자리</th></tr></thead><tbody>");
            for (class, c) in classes {
                let name = match class.as_str() {
                    "IPHONE" => "iPhone",
                    "IPAD" => "iPad",
                    "APPLE_WATCH" => "Apple Watch",
                    "IPOD" => "iPod",
                    other => other,
                };
                out.push_str(&format!(
                    "<tr><td>{}</td><td class=n>{}</td><td class=n>{}</td><td class=n>{}</td></tr>",
                    esc(name),
                    c.enabled,
                    c.disabled,
                    d.limit.saturating_sub(c.enabled + c.disabled)
                ));
            }
            out.push_str(&format!(
                "</tbody></table><p class=\"dim note\">애플은 기종마다 멤버십 한 해 {}대까지 받고 해지한 기기도 갱신 전까지 센다. {} 기준.</p>",
                d.limit,
                admin::kst(d.updated)
            ));
        }
        None => out.push_str("<p class=dim>기기 수를 아직 못 셌다 — 서명기가 한 번 돌면 생긴다.</p>"),
    }
    let records = gate.records();
    if !records.is_empty() {
        out.push_str("<table style=\"margin-top:16px\"><thead><tr><th>때</th><th>결과</th><th>기종</th><th>이름</th><th>UDID</th></tr></thead><tbody>");
        for r in records.iter().rev().take(20) {
            let tail: String = r.udid.chars().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect();
            let result = match r.result.as_str() {
                "registered" => "<span class=on>새로 등록</span>",
                "known" => "이미 있던 기기",
                "capped" => "<span class=off>하루 한도</span>",
                _ => "<span class=off>실패</span>",
            };
            out.push_str(&format!(
                "<tr><td>{}</td><td>{result}</td><td>{} <span class=dim>iOS {}</span></td><td>{}</td><td><code>…{}</code></td></tr>",
                admin::kst(r.at),
                esc(&r.product),
                esc(&r.version),
                esc(&r.name),
                esc(&tail)
            ));
        }
        out.push_str("</tbody></table>");
    }
    out.push_str("</div>");
    out
}

#[cfg(test)]
#[path = "gateway_install/tests.rs"]
mod tests;
