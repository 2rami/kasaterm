//! 관문 계정 — 카사텀이 없는 컴퓨터에서도 계정으로 로그인해 내 기기를 고르고 본다(`docs/screen-service-split.md` 「보기 앱」
//! 「계정 로그인」). 로그인은 폰과 같은 관문 길(`kind: phone`)이다 — Google·GitHub 는 시스템 브라우저 + PKCE 되돌아오기
//! (`docs/account-oauth.md`), 아이디·비밀번호는 `/relay/login`. 기기 목록은 `/relay/account-machines`, 기기마다
//! `/relay/account/m/~<id>/version` 으로 카사넷 주소를 배우고, `…/kasanet/view` 로 이 보기 앱 id 를 등록한다 — 데스크톱은
//! 관문을 거친 주인 자격만 받고 화면만 내준다(수명 15분, 그 3분의 1마다 다시).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::roster::{DeviceKind, Machine};

/// 기본 관문 — 본 앱 `kasa_device::mobile::DEFAULT_GATEWAY` 와 같다. 리그는 `KASATERM_VIEW_GATEWAY`.
const DEFAULT_GATEWAY: &str = "https://kasaterm.debimarlene.com";
const TIMEOUT: Duration = Duration::from_secs(15);
/// 브라우저 로그인을 기다리는 상한 — 관문 요청 수명(10분)과 같다.
const OAUTH_WAIT: Duration = Duration::from_secs(10 * 60);
const FILE: &str = "account.json";

/// 창과 명부 일꾼이 같이 보는 로그인 — 자격과 「곧바로 다시 물어라」 표시.
#[derive(Default)]
pub struct Session {
    pub cred: std::sync::Mutex<Option<Credential>>,
    pub refresh: std::sync::atomic::AtomicBool,
}

impl Session {
    pub fn get(&self) -> Option<Credential> {
        self.cred.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn set(&self, c: Option<Credential>) {
        *self.cred.lock().unwrap_or_else(|e| e.into_inner()) = c;
        self.refresh.store(true, std::sync::atomic::Ordering::Release);
    }
}

/// 로그인한 기기 자격. 토큰은 비밀이라 상태 폴더의 0600 파일에만 둔다.
#[derive(Clone, Debug, PartialEq)]
pub struct Credential {
    pub origin: String,
    pub token: String,
    pub account: String,
    pub display: String,
}

pub fn gateway() -> String {
    std::env::var("KASATERM_VIEW_GATEWAY").ok().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| DEFAULT_GATEWAY.into()).trim_end_matches('/').to_string()
}

fn path(state: &Path) -> PathBuf {
    state.join(FILE)
}

pub fn load(state: &Path) -> Option<Credential> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path(state)).ok()?).ok()?;
    Some(Credential {
        origin: v.get("origin")?.as_str()?.to_string(),
        token: v.get("token")?.as_str()?.to_string(),
        account: v.get("account")?.as_str()?.to_string(),
        display: v.get("display").and_then(Value::as_str).unwrap_or_default().to_string(),
    })
}

pub fn save(state: &Path, c: &Credential) -> std::io::Result<()> {
    let body = json!({ "origin": c.origin, "token": c.token, "account": c.account, "display": c.display }).to_string();
    let tmp = path(state).with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(&tmp)?.write_all(body.as_bytes())?;
    std::fs::rename(&tmp, path(state))
}

pub fn clear(state: &Path) {
    let _ = std::fs::remove_file(path(state));
}

/// 관문이 이 설치를 알아보는 이름 — 로그인마다 같은 값이라 관문 기기 목록에 한 줄로 남는다.
pub fn install_id(state: &Path) -> String {
    let p = state.join("install-id");
    if let Some(id) = std::fs::read_to_string(&p).ok().map(|s| s.trim().to_string()).filter(|s| s.len() == 32) {
        return id;
    }
    let id: String = random_bytes().iter().take(16).map(|b| format!("{b:02x}")).collect();
    let _ = std::fs::write(&p, &id);
    id
}

pub fn label() -> String {
    let host = std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).ok().filter(|h| !h.trim().is_empty());
    let host = host.or_else(|| std::process::Command::new("hostname").output().ok().and_then(|o| String::from_utf8(o.stdout).ok()).map(|s| s.trim().to_string()));
    format!("KasaDesk · {}", host.unwrap_or_else(|| "이 컴퓨터".into())).chars().take(60).collect()
}

/// 운영체제 난수 32바이트 — 카사넷 키 생성기가 쓰는 것과 같은 출처다.
fn random_bytes() -> [u8; 32] {
    kasa_net::iroh::SecretKey::generate().to_bytes()
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

pub fn client() -> reqwest::Client {
    reqwest::Client::builder().timeout(TIMEOUT).redirect(reqwest::redirect::Policy::none()).build().unwrap_or_default()
}

fn error_of(v: &Value, status: reqwest::StatusCode) -> String {
    v.get("error").and_then(Value::as_str).map_or_else(|| format!("관문 {status}"), str::to_string)
}

fn credential(origin: &str, v: &Value) -> Result<Credential, String> {
    let token = v.get("token").and_then(Value::as_str).ok_or("관문 답에 기기 자격이 없다")?.to_string();
    let account = v.get("account").and_then(Value::as_str).unwrap_or_default().to_string();
    let display = v.get("display_name").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or(&account).to_string();
    Ok(Credential { origin: origin.into(), token, account, display })
}

/// 켜진 로그인 방법(`google`·`github`) — 관문 `/relay/oauth/providers` 의 `providers[].{id, enabled}`. 되돌아오기 로그인을 모르는
/// 옛 관문이면 브라우저 로그인을 세우지 않는다(이 앱은 확인 코드 길을 두지 않는다).
pub async fn providers(client: &reqwest::Client, origin: &str) -> Vec<String> {
    let Ok(res) = client.get(format!("{origin}/relay/oauth/providers")).send().await else { return Vec::new() };
    let v: Value = res.json().await.unwrap_or(Value::Null);
    if v.get("redirect_login").and_then(Value::as_bool) != Some(true) {
        return Vec::new();
    }
    v.get("providers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|p| p.get("enabled").and_then(Value::as_bool) == Some(true))
        .filter_map(|p| p.get("id").and_then(Value::as_str).map(str::to_string))
        .collect()
}

pub async fn password_login(client: &reqwest::Client, origin: &str, account: &str, password: &str, machine: &str) -> Result<Credential, String> {
    let body = json!({ "account": account, "password": password, "kind": "phone", "machine_id": machine, "label": label() });
    let res = client.post(format!("{origin}/relay/login")).json(&body).send().await.map_err(|e| format!("관문에 못 닿아요: {e}"))?;
    let status = res.status();
    let v: Value = res.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        return Err(match error_of(&v, status).as_str() {
            "bad_credentials" => "아이디나 비밀번호가 맞지 않아요".into(),
            "rate_limited" => "너무 자주 시도했어요 — 잠시 뒤 다시 해 주세요".into(),
            other => other.into(),
        });
    }
    credential(origin, &v)
}

/// Google·GitHub 로그인 — 시스템 브라우저로 관문 승인 주소를 열고, 이 컴퓨터 루프백으로 돌아온 한 번 쓰는 코드를 PKCE 증명과
/// 바꾼다. 남이 보낸 로그인 링크를 눌러도 코드는 이 프로세스의 증명이 없는 곳으로 가 쓸모가 없다.
pub async fn oauth_login(client: &reqwest::Client, origin: &str, provider: &str, machine: &str, open: impl Fn(&str)) -> Result<Credential, String> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.map_err(|e| format!("되돌아올 자리를 못 연다: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let redirect = format!("http://127.0.0.1:{port}/oauth/callback");
    let verifier = b64(&random_bytes());
    let state = b64(&random_bytes()[..16]);
    let challenge = b64(&Sha256::digest(verifier.as_bytes()));
    let body = json!({
        "provider": provider, "kind": "phone", "machine_id": machine, "label": label(), "choose": true,
        "code_challenge": challenge, "code_challenge_method": "S256", "redirect_uri": redirect, "state": state,
    });
    let res = client.post(format!("{origin}/relay/oauth/start")).json(&body).send().await.map_err(|e| format!("관문에 못 닿아요: {e}"))?;
    let status = res.status();
    let v: Value = res.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        return Err(error_of(&v, status));
    }
    let url = v.get("authorization_url").and_then(Value::as_str).ok_or("관문이 승인 주소를 안 줬어요")?;
    // 관문 자기 주소만 연다 — 다른 곳으로 보내는 답이면 쓰지 않는다.
    if !url.starts_with(&format!("{origin}/relay/oauth/authorize/")) {
        return Err("관문이 낯선 승인 주소를 줬어요".into());
    }
    open(url);
    let code = tokio::time::timeout(OAUTH_WAIT, wait_code(&listener, &state)).await.map_err(|_| "브라우저 로그인을 기다리다 시간이 지났어요".to_string())??;
    let res = client
        .post(format!("{origin}/relay/oauth/token"))
        .json(&json!({ "code": code, "code_verifier": verifier, "redirect_uri": redirect }))
        .send()
        .await
        .map_err(|e| format!("관문에 못 닿아요: {e}"))?;
    let status = res.status();
    let v: Value = res.json().await.unwrap_or(Value::Null);
    if v.get("status").and_then(Value::as_str) == Some("choose") {
        return Err("이 계정은 아직 카사텀 계정과 이어지지 않았어요 — 카사텀 설정에서 연결하거나 아이디로 로그인해 주세요".into());
    }
    if !status.is_success() {
        return Err(error_of(&v, status));
    }
    credential(origin, &v)
}

/// 되돌아온 요청 중 우리 `state` 를 실은 것만 받는다 — 다른 방문은 404 로 돌려보내고 기다림을 이어 간다.
async fn wait_code(listener: &tokio::net::TcpListener, state: &str) -> Result<String, String> {
    loop {
        let (mut stream, _) = listener.accept().await.map_err(|e| e.to_string())?;
        let mut buf = vec![0u8; 8192];
        let n = stream.read(&mut buf).await.unwrap_or(0);
        let head = String::from_utf8_lossy(&buf[..n]);
        let target = head.lines().next().and_then(|l| l.split_whitespace().nth(1)).unwrap_or_default().to_string();
        let query = target.strip_prefix("/oauth/callback?").unwrap_or_default();
        let get = |k: &str| query.split('&').find_map(|kv| kv.split_once('=').filter(|(key, _)| *key == k).map(|(_, v)| decode(v)));
        if get("state").as_deref() != Some(state) {
            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
            continue;
        }
        let (body, result) = match (get("code"), get("error")) {
            (Some(code), _) => ("KasaDesk 로그인을 마쳤어요. 이 창은 닫아도 돼요.", Ok(code)),
            (_, error) => ("로그인하지 못했어요. KasaDesk 로 돌아가 다시 해 주세요.", Err(match error.as_deref() {
                Some("access_denied") => "로그인을 취소했어요".to_string(),
                _ => "관문이 로그인을 마치지 못했어요".to_string(),
            })),
        };
        let page = format!("<!doctype html><meta charset=utf-8><title>KasaDesk</title><p style=\"font:16px system-ui;margin:3em\">{body}</p>");
        let _ = stream
            .write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}", page.len()).as_bytes())
            .await;
        return result;
    }
}

fn decode(v: &str) -> String {
    let bytes = v.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
                match hex {
                    Some(b) => {
                        out.push(b);
                        i += 3;
                        continue;
                    }
                    None => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 관문이 자격을 거절했다(폐기·로그아웃) — 저장한 자격을 지울 때.
pub struct Revoked;

/// 계정 기기 — 관문이 지금 붙어 있다고 아는 데스크톱과 그 카사넷 주소. 내 보기 앱 id 를 알리는 기기(이 컴퓨터의 본 앱)는 뺀다.
pub async fn machines(client: &reqwest::Client, c: &Credential, me: &str) -> Result<Vec<Machine>, Revoked> {
    let res = client.get(format!("{}/relay/account-machines", c.origin)).bearer_auth(&c.token).send().await;
    let Ok(res) = res else { return Ok(Vec::new()) };
    if res.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(Revoked);
    }
    let v: Value = res.json().await.unwrap_or(Value::Null);
    let mut out = Vec::new();
    for m in v.get("machines").and_then(Value::as_array).into_iter().flatten() {
        let (Some(id), Some(name)) = (m.get("id").and_then(Value::as_str), m.get("machine").and_then(Value::as_str)) else { continue };
        let Some(version) = get_json(client, c, &format!("m/~{id}/version")).await else { continue };
        let Some(kasanet) = version.get("kasanet").filter(|k| k.get("id").is_some()) else { continue };
        if kasanet.get("view_id").and_then(Value::as_str) == Some(me) {
            continue;
        }
        let kind = DeviceKind::classify(name, &version);
        out.push(Machine { label: name.to_string(), kind, kasanet: kasanet.clone(), fresh: true, account: Some(id.to_string()) });
    }
    Ok(out)
}

async fn get_json(client: &reqwest::Client, c: &Credential, path: &str) -> Option<Value> {
    let res = client.get(format!("{}/relay/account/{path}", c.origin)).bearer_auth(&c.token).send().await.ok()?;
    res.status().is_success().then_some(())?;
    res.json().await.ok()
}

/// 그 기기에 이 보기 앱 id 를 등록한다. 수명(초)을 돌려준다. 그 기기가 옛 판이면(404) None.
pub async fn register(client: &reqwest::Client, c: &Credential, machine: &str, view_id: &str) -> Option<u64> {
    let res = client
        .post(format!("{}/relay/account/m/~{machine}/kasanet/view", c.origin))
        .bearer_auth(&c.token)
        .json(&json!({ "id": view_id }))
        .send()
        .await
        .ok()?;
    let v: Value = res.json().await.ok()?;
    v.get("ttl_secs").and_then(Value::as_u64)
}

/// 로그아웃 — 관문에서 이 기기 자격을 거둔다. 못 닿아도 이쪽 파일은 지운다(부르는 쪽).
pub async fn logout(client: &reqwest::Client, c: &Credential) {
    let _ = client.post(format!("{}/relay/logout", c.origin)).bearer_auth(&c.token).send().await;
}

/// 브라우저로 연다 — 셸을 거치지 않는다(주소의 `&` 가 명령으로 읽히지 않게).
pub fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("/usr/bin/open").arg(url).spawn();
    #[cfg(windows)]
    let r = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", url]).spawn();
    #[cfg(not(any(target_os = "macos", windows)))]
    let r = std::process::Command::new("xdg-open").arg(url).spawn();
    if let Err(e) = r {
        eprintln!("[kasadesk] 브라우저를 못 연다: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_credential_file_round_trips_and_is_private() {
        let dir = std::env::temp_dir().join(format!("kasadesk-account-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let c = Credential { origin: "https://relay.example".into(), token: "t".into(), account: "a".into(), display: "에이".into() };
        save(&dir, &c).unwrap();
        assert_eq!(load(&dir), Some(c));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(path(&dir)).unwrap().permissions().mode() & 0o777, 0o600);
        }
        clear(&dir);
        assert_eq!(load(&dir), None);
        let id = install_id(&dir);
        assert_eq!(id.len(), 32);
        assert_eq!(install_id(&dir), id, "같은 설치는 같은 이름");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn callback_values_are_percent_decoded() {
        assert_eq!(decode("a%2Bb%3D%3D"), "a+b==");
        assert_eq!(decode("x+y"), "x y");
        assert_eq!(decode("%zz"), "%zz");
    }
}
