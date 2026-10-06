//! `/relay/profile` — 계정의 얼굴(닉네임·프사)과 로그인 아이디·비밀번호 바꾸기
//! (docs/account-oauth.md 「프로필」). 계정은 기기 자격증명만 정한다 — 본문은 계정을 못 고른다.
//!
//! 프사는 사람이 올린 그림이 먼저고, 없으면 연결된 Google·GitHub 의 프로필 사진 주소를 준다.
//! 공급자 사진은 관문이 받아 두지 않는다 — 앱이 그 주소(공급자 이미지 호스트만)에서 바로 받는다.

use super::*;
use crate::oauth_accounts::Provider;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// 올린 그림 한 장의 상한. 앱이 256px 로 줄여 올리니 넉넉하다.
const MAX_AVATAR: usize = 512 * 1024;
const MAX_NICKNAME: usize = 40;

#[derive(Default, Serialize, Deserialize)]
struct Book {
    #[serde(default)]
    accounts: HashMap<String, Entry>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Entry {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    nickname: String,
    /// 올린 그림의 종류(`png`·`jpeg`·`webp`)와 판. 없으면 공급자 사진.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    upload: Option<(String, String)>,
    /// 사람이 고른 공급자 사진. 없으면 Google, 그다음 GitHub.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pick: Option<Provider>,
}

pub(super) struct Profiles {
    path: Option<PathBuf>,
    dir: Option<PathBuf>,
    book: Mutex<Book>,
}

impl Profiles {
    pub(super) fn open(state_path: Option<&std::path::Path>) -> Self {
        let path = state_path.map(|p| p.with_file_name("relay-profiles.json"));
        let book = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        Self { path, dir: state_path.map(|p| p.with_file_name("relay-avatars")), book: Mutex::new(book) }
    }

    pub(super) fn nickname(&self, account: &str) -> Option<String> {
        let book = self.book.lock().ok()?;
        book.accounts.get(account).map(|e| e.nickname.clone()).filter(|n| !n.is_empty())
    }

    fn entry(&self, account: &str) -> Entry {
        self.book.lock().ok().and_then(|b| b.accounts.get(account).cloned()).unwrap_or_default()
    }

    fn change(&self, account: &str, edit: impl FnOnce(&mut Entry)) -> Result<(), &'static str> {
        let path = self.path.as_ref().ok_or("storage_unavailable")?;
        let mut book = self.book.lock().map_err(|_| "storage_unavailable")?;
        let mut entry = book.accounts.get(account).cloned().unwrap_or_default();
        edit(&mut entry);
        let mut next = Book { accounts: book.accounts.clone() };
        if entry.nickname.is_empty() && entry.upload.is_none() && entry.pick.is_none() {
            next.accounts.remove(account);
        } else {
            next.accounts.insert(account.to_string(), entry);
        }
        let body = serde_json::to_string_pretty(&next).map_err(|_| "storage_unavailable")?;
        crate::relay_auth::write_private(path, &body).map_err(|_| "storage_unavailable")?;
        *book = next;
        Ok(())
    }

    /// 그림 파일 이름은 계정 이름의 해시 — 계정 이름이 경로에 그대로 들어가지 않는다.
    fn file(&self, account: &str) -> Option<PathBuf> {
        use sha2::Digest;
        let hash = sha2::Sha256::digest(account.as_bytes());
        let name: String = hash[..16].iter().map(|b| format!("{b:02x}")).collect();
        Some(self.dir.as_ref()?.join(name))
    }

    fn save_upload(&self, account: &str, kind: &str, bytes: &[u8]) -> Result<String, &'static str> {
        use sha2::Digest;
        let dir = self.dir.as_ref().ok_or("storage_unavailable")?;
        std::fs::create_dir_all(dir).map_err(|_| "storage_unavailable")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
        let file = self.file(account).ok_or("storage_unavailable")?;
        write_private_bytes(&file, bytes).map_err(|_| "storage_unavailable")?;
        let rev: String = sha2::Sha256::digest(bytes)[..8].iter().map(|b| format!("{b:02x}")).collect();
        let saved = (kind.to_string(), rev.clone());
        self.change(account, |e| {
            e.upload = Some(saved);
            e.pick = None;
        })?;
        Ok(rev)
    }

    fn remove_upload(&self, account: &str) -> Result<(), &'static str> {
        self.change(account, |e| e.upload = None)?;
        if let Some(file) = self.file(account) {
            let _ = std::fs::remove_file(file);
        }
        Ok(())
    }

    fn upload(&self, account: &str) -> Option<(String, Vec<u8>)> {
        let (kind, _) = self.entry(account).upload?;
        Some((kind, std::fs::read(self.file(account)?).ok()?))
    }
}

fn write_private_bytes(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let tmp = path.with_extension("tmp");
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let mut f = opts.open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}

/// 그림 종류는 확장자·머리가 아니라 내용 첫 바이트로 가린다.
fn image_kind(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("jpeg")
    } else if bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("webp")
    } else {
        None
    }
}

pub(super) fn clean_nickname(raw: &str) -> Result<String, &'static str> {
    let name: String = raw.trim().chars().filter(|c| !c.is_control()).collect();
    if name.chars().count() > MAX_NICKNAME {
        return Err("invalid_nickname");
    }
    Ok(name)
}

pub(super) fn routes() -> Router<Gate> {
    Router::new()
        .route("/relay/profile", get(show).patch(update))
        .route("/relay/profile/avatar", get(avatar).put(upload).delete(remove))
        .route("/relay/profile/login", axum::routing::post(change_login))
        .route("/relay/profile/password", axum::routing::post(change_password))
}

fn who(gate: &Gate, headers: &axum::http::HeaderMap) -> Result<String, axum::response::Response> {
    let Some((_, rec)) = gate.device_of(headers) else {
        return Err(json_err(StatusCode::UNAUTHORIZED, "unauthorized"));
    };
    Ok(rec.account)
}

async fn view(gate: &Gate, account: &str) -> serde_json::Value {
    let entry = gate.profiles.entry(account);
    let work = match gate.connections.as_ref() {
        Some(service) => service.identities(account).await,
        None => Vec::new(),
    };
    let identities: Vec<serde_json::Value> = gate
        .oauth
        .linked(account)
        .into_iter()
        .map(|linked| {
            let display = if linked.label.display.is_empty() {
                work.iter()
                    .find(|(provider, subject, _)| *provider == linked.provider && *subject == linked.subject)
                    .map(|(_, _, display)| display.clone())
                    .unwrap_or_default()
            } else {
                linked.label.display.clone()
            };
            // GitHub 사진은 숫자 id 로 정해진다 — 라벨을 남기기 전에 이은 로그인도 사진이 있다.
            let picture = linked.label.picture.clone().or_else(|| {
                (linked.provider == Provider::Github && linked.subject.bytes().all(|b| b.is_ascii_digit()))
                    .then(|| format!("https://avatars.githubusercontent.com/u/{}?v=4", linked.subject))
            });
            json!({"provider": linked.provider.name(), "display": display, "picture": picture})
        })
        .collect();
    let provider_picture = |provider: &str| {
        identities
            .iter()
            .find(|i| i["provider"] == provider && i["picture"].is_string())
            .map(|i| json!({"source": provider, "url": i["picture"]}))
    };
    let avatar = match (&entry.upload, entry.pick) {
        (Some((_, rev)), _) => Some(json!({"source": "upload", "rev": rev})),
        (None, Some(pick)) => provider_picture(pick.name()),
        (None, None) => None,
    }
    .or_else(|| provider_picture("google"))
    .or_else(|| provider_picture("github"));
    let login = gate.accounts.login_of(account);
    json!({
        "ok": true,
        "account": account,
        "login": login,
        "has_password": login.is_some(),
        "nickname": (!entry.nickname.is_empty()).then_some(entry.nickname),
        "display_name": gate.display_name(account),
        "avatar": avatar,
        "pick": entry.pick.map(|p| p.name()),
        "identities": identities,
    })
}

async fn show(State(gate): State<Gate>, headers: axum::http::HeaderMap) -> axum::response::Response {
    match who(&gate, &headers) {
        Ok(account) => axum::Json(view(&gate, &account).await).into_response(),
        Err(refusal) => refusal,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Update {
    #[serde(default)]
    nickname: Option<String>,
    /// `google`·`github` 이면 올린 그림을 지우고 그 공급자 사진을, `auto` 면 정해진 차례를 쓴다.
    #[serde(default)]
    avatar: Option<String>,
}

async fn update(State(gate): State<Gate>, req: axum::extract::Request) -> axum::response::Response {
    let account = match who(&gate, req.headers()) {
        Ok(account) => account,
        Err(refusal) => return refusal,
    };
    let Ok(bytes) = axum::body::to_bytes(req.into_body(), 8 * 1024).await else {
        return json_err(StatusCode::PAYLOAD_TOO_LARGE, "body_too_large");
    };
    let Ok(input) = serde_json::from_slice::<Update>(&bytes) else {
        return json_err(StatusCode::BAD_REQUEST, "bad_request");
    };
    let nickname = match input.nickname.as_deref().map(clean_nickname).transpose() {
        Ok(nickname) => nickname,
        Err(error) => return json_err(StatusCode::BAD_REQUEST, error),
    };
    let pick = match input.avatar.as_deref() {
        None => None,
        Some("auto") => Some(None),
        Some("google") => Some(Some(Provider::Google)),
        Some("github") => Some(Some(Provider::Github)),
        Some(_) => return json_err(StatusCode::BAD_REQUEST, "bad_request"),
    };
    if pick.is_some() {
        if let Err(error) = gate.profiles.remove_upload(&account) {
            return json_err(StatusCode::SERVICE_UNAVAILABLE, error);
        }
    }
    let result = gate.profiles.change(&account, |entry| {
        if let Some(nickname) = nickname {
            entry.nickname = nickname;
        }
        if let Some(pick) = pick {
            entry.pick = pick;
        }
    });
    match result {
        Ok(()) => axum::Json(view(&gate, &account).await).into_response(),
        Err(error) => json_err(StatusCode::SERVICE_UNAVAILABLE, error),
    }
}

async fn avatar(State(gate): State<Gate>, headers: axum::http::HeaderMap) -> axum::response::Response {
    let account = match who(&gate, &headers) {
        Ok(account) => account,
        Err(refusal) => return refusal,
    };
    let Some((kind, bytes)) = gate.profiles.upload(&account) else {
        return json_err(StatusCode::NOT_FOUND, "not_found");
    };
    (
        [
            (header::CONTENT_TYPE, format!("image/{kind}")),
            (header::CACHE_CONTROL, "private, no-cache".to_string()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
        ],
        bytes,
    )
        .into_response()
}

async fn upload(State(gate): State<Gate>, req: axum::extract::Request) -> axum::response::Response {
    let account = match who(&gate, req.headers()) {
        Ok(account) => account,
        Err(refusal) => return refusal,
    };
    let Ok(bytes) = axum::body::to_bytes(req.into_body(), MAX_AVATAR).await else {
        return json_err(StatusCode::PAYLOAD_TOO_LARGE, "too_large");
    };
    let Some(kind) = image_kind(&bytes) else {
        return json_err(StatusCode::UNSUPPORTED_MEDIA_TYPE, "not_an_image");
    };
    match gate.profiles.save_upload(&account, kind, &bytes) {
        Ok(_) => axum::Json(view(&gate, &account).await).into_response(),
        Err(error) => json_err(StatusCode::SERVICE_UNAVAILABLE, error),
    }
}

async fn remove(State(gate): State<Gate>, headers: axum::http::HeaderMap) -> axum::response::Response {
    let account = match who(&gate, &headers) {
        Ok(account) => account,
        Err(refusal) => return refusal,
    };
    match gate.profiles.remove_upload(&account) {
        Ok(()) => axum::Json(view(&gate, &account).await).into_response(),
        Err(error) => json_err(StatusCode::SERVICE_UNAVAILABLE, error),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Secret {
    password: String,
    #[serde(default)]
    login: Option<String>,
    #[serde(default)]
    new_password: Option<String>,
}

/// 지금 비밀번호를 확인한다. 로그인과 같은 잠금(계정 열쇠 기준)을 쓴다.
async fn confirm(gate: &Gate, req: axum::extract::Request) -> Result<(String, Secret), axum::response::Response> {
    let account = who(gate, req.headers())?;
    let ip = client_ip(&req);
    let bytes = axum::body::to_bytes(req.into_body(), 8 * 1024)
        .await
        .map_err(|_| json_err(StatusCode::PAYLOAD_TOO_LARGE, "body_too_large"))?;
    let input: Secret =
        serde_json::from_slice(&bytes).map_err(|_| json_err(StatusCode::BAD_REQUEST, "bad_request"))?;
    if !gate.accounts.exists(&account) {
        return Err(json_err(StatusCode::CONFLICT, "no_password"));
    }
    if gate.limiter.allow(&account, &ip).is_err() {
        return Err(json_err(StatusCode::TOO_MANY_REQUESTS, "rate_limited"));
    }
    let ok = !input.password.is_empty() && input.password.len() <= 1024 && {
        let accounts = gate.accounts.clone();
        let (name, password) = (account.clone(), input.password.clone());
        tokio::task::spawn_blocking(move || accounts.check(&name, &password)).await.unwrap_or(false)
    };
    gate.limiter.record(&account, ok);
    if !ok {
        return Err(json_err(StatusCode::UNAUTHORIZED, "bad_credentials"));
    }
    Ok((account, input))
}

fn refused(error: &'static str) -> axum::response::Response {
    let status = match error {
        "login_taken" | "no_password" => StatusCode::CONFLICT,
        "storage_unavailable" => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::BAD_REQUEST,
    };
    json_err(status, error)
}

async fn change_login(State(gate): State<Gate>, req: axum::extract::Request) -> axum::response::Response {
    let (account, input) = match confirm(&gate, req).await {
        Ok(found) => found,
        Err(refusal) => return refusal,
    };
    let login = input.login.unwrap_or_default().trim().to_lowercase();
    match gate.accounts.set_login(&account, &login) {
        Ok(()) => {
            eprintln!("[gateway] 로그인 아이디 바꿈 — 계정 {account}");
            axum::Json(view(&gate, &account).await).into_response()
        }
        Err(error) => refused(error),
    }
}

async fn change_password(State(gate): State<Gate>, req: axum::extract::Request) -> axum::response::Response {
    let (account, input) = match confirm(&gate, req).await {
        Ok(found) => found,
        Err(refusal) => return refusal,
    };
    let new_password = input.new_password.unwrap_or_default();
    let accounts = gate.accounts.clone();
    let name = account.clone();
    let result = tokio::task::spawn_blocking(move || accounts.set_password(&name, &new_password))
        .await
        .unwrap_or(Err("storage_unavailable"));
    match result {
        Ok(()) => {
            eprintln!("[gateway] 비밀번호 바꿈 — 계정 {account}");
            axum::Json(json!({"ok": true})).into_response()
        }
        Err(error) => refused(error),
    }
}

#[cfg(test)]
#[path = "gateway_profile/tests.rs"]
mod tests;
