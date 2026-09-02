//! 폰 접속 주소 — **유저마다 하나씩**.
//!
//! 전에는 기계마다 토큰 하나(`remote-token`)를 `?t=` 로 한 번 물고 들어와 쿠키로
//! 바꿔 다녔다. 실사용에서 세 군데서 깨졌다(2026-09-02 지적 「토큰없으면 안봐지고
//! 연결도끊기고 버그도많아」):
//! ① 쿠키가 `SameSite=Strict` 라 슬랙·디스코드 알림 링크에서 건너오면 안 실려 403.
//! ② 인앱 브라우저·홈화면 앱은 쿠키 창고가 따로라 처음부터 403.
//! ③ 주소에서 `?t=` 가 빠지는 순간(pane 전환·북마크 정리) 되살릴 길이 없다.
//!
//! 이제 **주소 자체가 자격**이다: `https://<host>/u/<slug>/…`. slug 는 유저마다 따로
//! 뽑은 비밀이고, 그 아래 모든 경로(`term/grid` · `arona-ui/` · `m/<기계>/…`)가 상대
//! 주소라 어디로 가든 자격이 따라간다. 쿠키는 보조일 뿐이다(옛 절대경로 fetch 용).
//!
//! 저장: `~/.config/kasaterm/mobile-users.json` (0600). env `KASATERM_MOBILE_USERS` 가
//! 우선 — 검증용 인스턴스가 사용자 파일을 안 건드리게(다른 격리 env 와 같은 규율).
//!
//! 이 모듈은 **순수 저장·판정**만 한다. HTTP 관문(`/u/<slug>/` 접두 벗기기)은
//! `http.rs` 의 `mobile_prefix_mw` 가, 화면(허브)은 `assets/term/hub.html` 이 맡는다.

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// 주소 접두. 이 뒤에 slug 가 온다.
pub const PREFIX: &str = "/u/";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct MobileUser {
    pub name: String,
    /// 주소의 비밀 부분. 유저마다 다르고, 지우면 그 주소만 죽는다.
    pub slug: String,
    #[serde(default)]
    pub created: u64,
    /// 이 기계의 주인 — 유저를 더하고 지울 수 있다. 첫 접근 때 자동으로 하나 생긴다.
    #[serde(default)]
    pub owner: bool,
}

#[derive(Default, Serialize, Deserialize)]
struct FileBody {
    #[serde(default)]
    users: Vec<MobileUser>,
    /// 관문(중계소)에 이 기계를 증명하는 비밀 — 기계마다 하나. slug 는 처음 온 키에
    /// 묶이므로, 이 키가 바뀌면 관문은 옛 slug 를 다른 기계 것으로 보고 거절한다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    machine_key: Option<String>,
    /// 관문 주소. 없으면 기본 관문(`DEFAULT_GATEWAY`), `"off"` 면 관문 없이 로컬·자기 터널만.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    gateway: Option<String>,
    /// 「● 바깥」 — 관문에 붙어 주소를 살릴지. 기본 켜짐: 앱을 켠 사람은 곧장 주소를 받는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    published: Option<bool>,
    /// 업링크를 **어디로 열지** — 폰에 주는 주소(gateway)와 다를 수 있다. 관문이 같은
    /// 기계의 ssh 터널(`http://127.0.0.1:8790`)로도 닿으면 그쪽이 공용 주소를 거치는
    /// 것보다 곧고, 공용 호스트가 다른 연결기로 갈려 있어도 흔들리지 않는다. 없으면 gateway.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    gateway_connect: Option<String>,
}

/// 기본 관문. 카사텀을 쓰는 사람마다 앱이 여기 붙어 자기 주소를 받는다(uplink.rs 머리말).
pub const DEFAULT_GATEWAY: &str = "https://kasaterm.debimarlene.com";

/// 쓰기는 한 번에 하나 — 읽고-고치고-쓰기 사이에 다른 요청이 끼면 유저가 사라진다.
static WRITE: Mutex<()> = Mutex::new(());

fn users_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("KASATERM_MOBILE_USERS") {
        return Some(PathBuf::from(p));
    }
    Some(kasa_socket::home_dir()?.join(".config/kasaterm/mobile-users.json"))
}

fn load_body(path: &std::path::Path) -> FileBody {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<FileBody>(&s).ok())
        .unwrap_or_default()
}

fn load_from(path: &std::path::Path) -> Vec<MobileUser> {
    load_body(path).users
}

/// 유저 목록만 갈아 끼우고 나머지 항목(키·관문·스위치)은 그대로 둔다.
fn save_to(path: &std::path::Path, users: &[MobileUser]) -> std::io::Result<()> {
    let mut body = load_body(path);
    body.users = users.to_vec();
    save_body(path, &body)
}

fn save_body(path: &std::path::Path, body: &FileBody) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let body = serde_json::to_string_pretty(body).map_err(|e| std::io::Error::other(e.to_string()))?;
    std::fs::write(path, body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn base36(mut n: u128) -> String {
    const ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    while n > 0 {
        out.push(ALPHABET[(n % 36) as usize]);
        n /= 36;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// 새 slug. UUID v4 의 122비트 난수를 base36 으로 — 25자 안팎이라 주소로 들고 다닐 만하고,
/// 맞혀 들어올 수 있는 크기가 아니다.
pub fn new_slug() -> String {
    base36(uuid::Uuid::new_v4().as_u128())
}

/// slug 꼴인가. 경로 재료(`/`·`.`)와 대문자를 애초에 거른다 — 조회 전 1차 관문.
pub fn valid_slug(s: &str) -> bool {
    (16..=40).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// 유저 이름 규칙 — 파일·화면에 그대로 실리므로 짧고 줄바꿈 없이.
fn valid_name(s: &str) -> bool {
    let n = s.trim();
    !n.is_empty() && n.chars().count() <= 40 && !n.chars().any(|c| c.is_control())
}

/// 주인 이름 기본값 — 유저 이름은 표시용이라 로그인 계정이면 충분하다.
fn default_owner_name() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "owner".to_string())
}

/// 등록된 유저 전부. 파일이 없으면 빈 목록 — 주인을 만들려면 `owner()`.
pub fn users() -> Vec<MobileUser> {
    users_path().map(|p| load_from(&p)).unwrap_or_default()
}

/// 이 기계의 주인 유저. 없으면 **지금 만든다** — 앱을 처음 켠 사람도 주소 하나는
/// 바로 받아야 한다(토큰 파일이 첫 접근 때 생기던 것과 같은 규율).
pub fn owner() -> Option<MobileUser> {
    let path = users_path()?;
    let _g = WRITE.lock().ok()?;
    let mut list = load_from(&path);
    if let Some(o) = list.iter().find(|u| u.owner) {
        return Some(o.clone());
    }
    let o = MobileUser {
        name: default_owner_name(),
        slug: new_slug(),
        created: now_secs(),
        owner: true,
    };
    list.insert(0, o.clone());
    save_to(&path, &list).ok()?;
    drop(_g);
    crate::uplink::poke();
    Some(o)
}

/// 유저를 더한다. 같은 이름이 있으면 그대로 돌려준다(두 번 눌러도 주소가 둘이 안 된다).
pub fn add(name: &str) -> Result<MobileUser, String> {
    if !valid_name(name) {
        return Err("이름은 1~40자, 줄바꿈 없이".to_string());
    }
    let name = name.trim();
    let path = users_path().ok_or("홈 폴더를 못 찾아요")?;
    let _g = WRITE.lock().map_err(|_| "잠금 실패")?;
    let mut list = load_from(&path);
    if let Some(u) = list.iter().find(|u| u.name == name) {
        return Ok(u.clone());
    }
    let u = MobileUser { name: name.to_string(), slug: new_slug(), created: now_secs(), owner: false };
    list.push(u.clone());
    save_to(&path, &list).map_err(|e| format!("저장 실패: {e}"))?;
    crate::uplink::poke();
    Ok(u)
}

/// 유저를 지운다 — 그 주소가 즉시 죽는다. 주인은 못 지운다(주소가 하나도 안 남는다).
pub fn remove(name: &str) -> Result<bool, String> {
    let path = users_path().ok_or("홈 폴더를 못 찾아요")?;
    let _g = WRITE.lock().map_err(|_| "잠금 실패")?;
    let mut list = load_from(&path);
    let Some(i) = list.iter().position(|u| u.name == name.trim()) else {
        return Ok(false);
    };
    if list[i].owner {
        return Err("주인은 지울 수 없어요 — 주소를 바꾸려면 새로 뽑기".to_string());
    }
    list.remove(i);
    save_to(&path, &list).map_err(|e| format!("저장 실패: {e}"))?;
    crate::uplink::poke();
    Ok(true)
}

/// 주소를 새로 뽑는다(옛 주소는 그 자리에서 죽는다). 주인도 된다 — 주소가 샜을 때 쓰는 손.
pub fn rotate(name: &str) -> Result<MobileUser, String> {
    let path = users_path().ok_or("홈 폴더를 못 찾아요")?;
    let _g = WRITE.lock().map_err(|_| "잠금 실패")?;
    let mut list = load_from(&path);
    let Some(u) = list.iter_mut().find(|u| u.name == name.trim()) else {
        return Err("그런 유저가 없어요".to_string());
    };
    u.slug = new_slug();
    let out = u.clone();
    save_to(&path, &list).map_err(|e| format!("저장 실패: {e}"))?;
    crate::uplink::poke();
    Ok(out)
}

/// 관문에 낼 기계 비밀. 없으면 지금 만든다(주인 주소처럼 첫 접근 때).
pub fn machine_key() -> Option<String> {
    let path = users_path()?;
    let _g = WRITE.lock().ok()?;
    let mut body = load_body(&path);
    if let Some(k) = body.machine_key.as_ref().filter(|k| k.len() >= 16) {
        return Some(k.clone());
    }
    let k = format!("{}{}", new_slug(), new_slug());
    body.machine_key = Some(k.clone());
    save_body(&path, &body).ok()?;
    Some(k)
}

/// 관문 주소. env `KASATERM_GATEWAY` 가 우선(리그가 로컬 관문을 가리키게), 빈 값·`off` 면 관문 없음.
pub fn gateway() -> Option<String> {
    let pick = |v: String| {
        let v = v.trim().trim_end_matches('/').to_string();
        (!v.is_empty() && v != "off" && v != "0" && v != "false").then_some(v)
    };
    if let Ok(v) = std::env::var("KASATERM_GATEWAY") {
        return pick(v);
    }
    let file = users_path().map(|p| load_body(&p)).unwrap_or_default();
    match file.gateway {
        Some(v) => pick(v),
        None => Some(DEFAULT_GATEWAY.to_string()),
    }
}

/// 업링크를 열 주소. env `KASATERM_GATEWAY_CONNECT` → 파일 `gateway_connect` → gateway.
pub fn gateway_connect() -> Option<String> {
    let pick = |v: String| {
        let v = v.trim().trim_end_matches('/').to_string();
        (!v.is_empty()).then_some(v)
    };
    if let Ok(v) = std::env::var("KASATERM_GATEWAY_CONNECT") {
        if let Some(v) = pick(v) {
            return Some(v);
        }
    }
    if let Some(v) = users_path().map(|p| load_body(&p)).and_then(|b| b.gateway_connect).and_then(pick) {
        return Some(v);
    }
    gateway()
}

/// 관문 호스트(스킴 없이) — 화면 표시·주소 조립용.
pub fn gateway_host() -> Option<String> {
    gateway().map(|g| {
        g.trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_end_matches('/')
            .to_string()
    })
}

/// 「● 바깥」 스위치. 기본 켜짐.
pub fn published() -> bool {
    users_path()
        .map(|p| load_body(&p))
        .and_then(|b| b.published)
        .unwrap_or(true)
}

pub fn set_published(on: bool) -> std::io::Result<()> {
    let path = users_path().ok_or_else(|| std::io::Error::other("홈 폴더 없음"))?;
    let _g = WRITE.lock().map_err(|_| std::io::Error::other("잠금 실패"))?;
    let mut body = load_body(&path);
    body.published = Some(on);
    save_body(&path, &body)?;
    drop(_g);
    crate::uplink::poke();
    Ok(())
}

pub fn by_slug(slug: &str) -> Option<MobileUser> {
    if !valid_slug(slug) {
        return None;
    }
    users().into_iter().find(|u| u.slug == slug)
}

/// 이 유저의 주소 경로(`/u/<slug>/`). 호스트를 앞에 붙이면 폰에 보낼 주소가 된다.
pub fn path_of(user: &MobileUser) -> String {
    format!("{PREFIX}{}/", user.slug)
}

/// 주인의 완성 주소. `path` 는 `/term/grid` 처럼 슬래시로 시작하거나 빈 문자열(허브).
pub fn owner_address(host: &str, path: &str) -> Option<String> {
    let o = owner()?;
    Some(format!("https://{host}{}{}", path_of(&o), path.trim_start_matches('/')))
}

/// `/u/<slug>/…` 요청을 어떻게 다룰지.
#[derive(Debug, PartialEq)]
pub enum Rewrite {
    /// 우리 접두가 아니다 — 그대로 라우터로.
    NotOurs,
    /// 모르는 slug — 404. 있는지 없는지 구분 안 되게 한 종류로.
    Unknown,
    /// `/u/<slug>` 에 슬래시가 없다 — `/u/<slug>/` 로 보낸다. 상대 주소가 디렉터리
    /// 기준으로 풀리려면 꼬리 슬래시가 있어야 한다(`/arona-ui` → `/arona-ui/` 와 같은 이유).
    NeedSlash(String),
    /// 접두를 벗긴 경로로 라우팅. 빈 꼬리(`/u/<slug>/`)는 허브다.
    Route { user: MobileUser, path: String },
}

/// 순수 판정 — 조회 함수를 받아서 테스트가 파일 없이 돈다.
pub fn rewrite_with(path: &str, lookup: impl Fn(&str) -> Option<MobileUser>) -> Rewrite {
    let Some(rest) = path.strip_prefix(PREFIX) else {
        return Rewrite::NotOurs;
    };
    let (slug, tail) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    if !valid_slug(slug) {
        return Rewrite::Unknown;
    }
    let Some(user) = lookup(slug) else {
        return Rewrite::Unknown;
    };
    if tail.is_empty() {
        return Rewrite::NeedSlash(slug.to_string());
    }
    let path = if tail == "/" { "/hub".to_string() } else { tail.to_string() };
    Rewrite::Route { user, path }
}

pub fn rewrite(path: &str) -> Rewrite {
    rewrite_with(path, by_slug)
}

/// 이 기계를 폰 화면에서 부를 이름. 릴레이 설정의 machine_id 가 있으면 그것(다른
/// 기계 명부와 같은 이름이라 헷갈리지 않는다), 없으면 OS 의 컴퓨터 이름.
pub fn machine_name() -> String {
    if let Some(c) = crate::peermirror::relay_conf() {
        if !c.machine_id.trim().is_empty() {
            return c.machine_id;
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(o) = std::process::Command::new("scutil").args(["--get", "ComputerName"]).output() {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if !s.is_empty() {
                return s;
            }
        }
    }
    std::process::Command::new("hostname")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "이 기계".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(name: &str, slug: &str, owner: bool) -> MobileUser {
        MobileUser { name: name.into(), slug: slug.into(), created: 0, owner }
    }

    #[test]
    fn slug_is_long_lowercase_alnum() {
        for _ in 0..50 {
            let s = new_slug();
            assert!(valid_slug(&s), "{s}");
        }
        assert!(!valid_slug("short"));
        assert!(!valid_slug("ABCDEFGHIJKLMNOPQRSTU"));
        assert!(!valid_slug("abcdefghijklmnop.qrs/tu"));
    }

    #[test]
    fn rewrite_strips_prefix_and_maps_root_to_hub() {
        let me = u("나", "abcdefghijklmnopqrstuvwxy", true);
        let lookup = |s: &str| (s == me.slug).then(|| me.clone());
        assert_eq!(rewrite_with("/term/grid", lookup), Rewrite::NotOurs);
        assert_eq!(rewrite_with("/u/nope/term", lookup), Rewrite::Unknown);
        assert_eq!(rewrite_with("/u/zzzzzzzzzzzzzzzzzzzzzzzzz/", lookup), Rewrite::Unknown);
        assert_eq!(
            rewrite_with("/u/abcdefghijklmnopqrstuvwxy", lookup),
            Rewrite::NeedSlash("abcdefghijklmnopqrstuvwxy".into())
        );
        assert_eq!(
            rewrite_with("/u/abcdefghijklmnopqrstuvwxy/", lookup),
            Rewrite::Route { user: me.clone(), path: "/hub".into() }
        );
        assert_eq!(
            rewrite_with("/u/abcdefghijklmnopqrstuvwxy/m/맥미니/term/ws", lookup),
            Rewrite::Route { user: me.clone(), path: "/m/맥미니/term/ws".into() }
        );
    }

    #[test]
    fn file_round_trip_keeps_users_and_locks_permissions() {
        let dir = std::env::temp_dir().join(format!("kasa-mobile-test-{}", uuid::Uuid::new_v4()));
        let path = dir.join("mobile-users.json");
        assert!(load_from(&path).is_empty());
        let list = vec![u("나", &new_slug(), true), u("우성", &new_slug(), false)];
        save_to(&path, &list).unwrap();
        assert_eq!(load_from(&path), list);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn saving_users_keeps_machine_key_and_switch() {
        let dir = std::env::temp_dir().join(format!("kasa-mobile-test-{}", uuid::Uuid::new_v4()));
        let path = dir.join("mobile-users.json");
        save_body(&path, &FileBody { users: vec![], machine_key: Some("k".repeat(20)), gateway: Some("off".into()), published: Some(false), gateway_connect: None }).unwrap();
        save_to(&path, &[u("나", &new_slug(), true)]).unwrap();
        let b = load_body(&path);
        assert_eq!(b.machine_key.as_deref(), Some("k".repeat(20).as_str()));
        assert_eq!(b.gateway.as_deref(), Some("off"));
        assert_eq!(b.published, Some(false));
        assert_eq!(b.users.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn path_of_ends_with_slash() {
        let p = path_of(&u("x", "abcdefghijklmnopqrstuvwxy", false));
        assert_eq!(p, "/u/abcdefghijklmnopqrstuvwxy/");
    }
}
