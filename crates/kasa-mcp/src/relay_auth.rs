//! 관문 계정 — 사람이 관문 기계에서 `kasa-relay account add <이름>` 으로 만들고, 기기는
//! 아이디·비밀번호로 **한 번** 로그인해 기기 토큰을 받는다(`POST /relay/login`, `gateway.rs`).
//! 비밀번호는 기기에 남지 않는다.
//!
//! - 비밀번호: PBKDF2-HMAC-SHA256 + 16B 소금. 모르는 계정도 같은 계산을 해서 응답 시간으로
//!   계정이 있는지가 새지 않게 한다.
//! - 기기 토큰: 32B 무작위(`kdt_` + base64url). 관문은 sha256 만 쥔다 — 상태 파일이 새도 토큰은
//!   안 샌다.
//! - 계정 파일(`relay-accounts.json`, 0600)은 사람이 CLI 로 고친다. 돌고 있는 관문은 mtime 을
//!   보고 다시 읽는다 — 관문과 CLI 가 한 파일의 쓰기를 다투지 않는다.

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

pub const ITERATIONS: u32 = 600_000;
pub const TOKEN_PREFIX: &str = "kdt_";
const ALG: ring::pbkdf2::Algorithm = ring::pbkdf2::PBKDF2_HMAC_SHA256;

/// 이만큼 연달아 틀리면 그 계정을 잠그기 시작한다(잠금은 틀릴 때마다 두 배, 최대 15분).
const FREE_FAILURES: u32 = 5;
const MAX_LOCK: Duration = Duration::from_secs(15 * 60);
/// 한 곳(IP)에서 1분에 해 볼 수 있는 로그인 수.
const PER_IP_PER_MINUTE: u32 = 20;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PwHash {
    pub iterations: u32,
    pub salt: String,
    pub hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Account {
    pub pbkdf2_sha256: PwHash,
    #[serde(default)]
    pub created: u64,
    #[serde(default)]
    pub disabled: bool,
    /// 사람이 바꾼 로그인 아이디. 계정 열쇠(표의 이름)는 기기 기록·OAuth·동기화·봉인함에 박혀 있어
    /// 그대로 두고, 사람이 치고 보는 아이디만 바꾼다. 바꾼 뒤의 옛 이름은 로그인에 안 쓰이고 남에게도
    /// 안 풀린다 — 열쇠가 그 이름이라 다른 계정이 같은 이름을 쥐면 둘이 섞인다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
pub struct AccountsFile {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub accounts: HashMap<String, Account>,
}

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
}

fn random<const N: usize>() -> [u8; N] {
    use ring::rand::SecureRandom as _;
    let mut b = [0u8; N];
    ring::rand::SystemRandom::new().fill(&mut b).expect("system random");
    b
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 계정 이름 — 소문자·숫자·`-`·`_` 2~32자. 대소문자를 섞으면 로그인 때 헷갈린다.
pub fn valid_account_name(name: &str) -> bool {
    (2..=32).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// 사람이 고를 수 있는 로그인 아이디 — 계정 이름 규칙에 더해, 구글·깃허브로 만든 계정의 열쇠
/// (`oauth_…`)와 헷갈리는 이름은 받지 않는다.
pub fn valid_login(name: &str) -> bool {
    valid_account_name(name) && !name.starts_with("oauth_")
}

pub const MIN_PASSWORD_CHARS: usize = 8;

/// 이 표에서 아이디가 이미 누구 것인가 — 다른 계정의 열쇠(옛 아이디 포함)이거나 바꾼 아이디면 그 열쇠.
pub fn login_owner<'a>(file: &'a AccountsFile, name: &str) -> Option<&'a str> {
    file.accounts
        .iter()
        .find(|(_, a)| a.login.as_deref() == Some(name))
        .or_else(|| file.accounts.get_key_value(name))
        .map(|(key, _)| key.as_str())
}

pub fn hash_password_with(pw: &str, iterations: u32) -> PwHash {
    use base64::Engine as _;
    let salt = random::<16>();
    let mut out = [0u8; 32];
    let n = NonZeroU32::new(iterations.max(1)).unwrap();
    ring::pbkdf2::derive(ALG, n, &salt, pw.as_bytes(), &mut out);
    PwHash { iterations, salt: b64().encode(salt), hash: b64().encode(out) }
}

pub fn hash_password(pw: &str) -> PwHash {
    hash_password_with(pw, ITERATIONS)
}

pub fn verify_password(pw: &str, h: &PwHash) -> bool {
    use base64::Engine as _;
    let (Ok(salt), Ok(want), Some(n)) = (b64().decode(&h.salt), b64().decode(&h.hash), NonZeroU32::new(h.iterations)) else {
        return false;
    };
    ring::pbkdf2::verify(ALG, n, &salt, pw.as_bytes(), &want).is_ok()
}

pub fn new_token() -> String {
    use base64::Engine as _;
    format!("{TOKEN_PREFIX}{}", b64().encode(random::<32>()))
}

pub fn new_device_id() -> String {
    format!("dev_{}", random::<8>().iter().map(|b| format!("{b:02x}")).collect::<String>())
}

pub fn token_hash(token: &str) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(token.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

pub fn load_accounts(path: &Path) -> AccountsFile {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// 임시 파일에 쓰고 fsync 뒤 이름을 바꾼다(0600).
pub fn write_private(path: &Path, body: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("json.tmp");
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let mut f = opts.open(&tmp)?;
    f.write_all(body.as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}

pub fn save_accounts(path: &Path, file: &AccountsFile) -> std::io::Result<()> {
    let body = serde_json::to_string_pretty(&AccountsFile { version: 1, accounts: file.accounts.clone() })
        .map_err(std::io::Error::other)?;
    write_private(path, &body)
}

/// 관문이 쥐는 계정 표. 파일이 바뀌었으면 읽을 때 다시 읽는다.
pub struct Accounts {
    path: Option<PathBuf>,
    cache: Mutex<(Option<SystemTime>, AccountsFile)>,
}

impl Accounts {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self { path, cache: Mutex::new((None, AccountsFile::default())) }
    }

    fn with<R>(&self, f: impl FnOnce(&AccountsFile) -> R) -> R {
        let mut cache = self.cache.lock().unwrap();
        if let Some(p) = &self.path {
            let mtime = std::fs::metadata(p).and_then(|m| m.modified()).ok();
            if mtime != cache.0 {
                *cache = (mtime, load_accounts(p));
            }
        }
        f(&cache.1)
    }

    /// 로그인해도 되는 계정인가(있고 막히지 않음).
    pub fn active(&self, name: &str) -> bool {
        self.with(|f| f.accounts.get(name).is_some_and(|a| !a.disabled))
    }

    pub(crate) fn exists(&self, name: &str) -> bool {
        self.with(|f| f.accounts.contains_key(name))
    }

    /// 관리 화면용 — 이름·만든 시각·막힘뿐, 비밀번호 해시는 내지 않는다.
    pub(crate) fn summary(&self) -> Vec<(String, u64, bool)> {
        self.with(|f| f.accounts.iter().map(|(name, a)| (name.clone(), a.created, a.disabled)).collect())
    }

    /// 사람이 친 아이디의 계정 열쇠. 바꾼 아이디는 그 계정으로, 바꾸기 전 이름(열쇠)은 아무 데도
    /// 안 간다.
    pub fn resolve(&self, typed: &str) -> Option<String> {
        self.with(|f| {
            f.accounts
                .iter()
                .find(|(_, a)| a.login.as_deref() == Some(typed))
                .or_else(|| f.accounts.get_key_value(typed).filter(|(_, a)| a.login.is_none()))
                .map(|(key, _)| key.clone())
        })
    }

    /// 화면에 보일 로그인 아이디. 비밀번호 계정이 아니면(구글·깃허브로 만든 계정) `None`.
    pub fn login_of(&self, key: &str) -> Option<String> {
        self.with(|f| f.accounts.get(key).map(|a| a.login.clone().unwrap_or_else(|| key.to_string())))
    }

    /// 아이디·비밀번호로 들어와서 그 계정 열쇠를 받는다. 없는 아이디도 같은 계산을 거친다.
    pub fn check_login(&self, typed: &str, pw: &str) -> Option<String> {
        match self.resolve(typed) {
            Some(key) => self.check(&key, pw).then_some(key),
            None => {
                self.check("\u{0}", pw);
                None
            }
        }
    }

    /// 계정 표를 디스크에서 새로 읽어 고치고 쓴다. CLI 가 그사이 고친 줄을 덮지 않게 캐시가 아니라
    /// 파일을 기준으로 한다.
    fn update(&self, change: impl FnOnce(&mut AccountsFile) -> Result<(), &'static str>) -> Result<(), &'static str> {
        let path = self.path.as_ref().ok_or("storage_unavailable")?;
        let mut cache = self.cache.lock().unwrap();
        let mut file = load_accounts(path);
        change(&mut file)?;
        save_accounts(path, &file).map_err(|_| "storage_unavailable")?;
        let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        *cache = (mtime, file);
        Ok(())
    }

    /// 로그인 아이디를 바꾼다. 자기 열쇠 이름으로 되돌리면 바꾼 기록을 지운다.
    pub fn set_login(&self, key: &str, login: &str) -> Result<(), &'static str> {
        if !valid_login(login) {
            return Err("invalid_login");
        }
        self.update(|file| {
            if login_owner(file, login).is_some_and(|owner| owner != key) {
                return Err("login_taken");
            }
            let account = file.accounts.get_mut(key).ok_or("no_password")?;
            account.login = (login != key).then(|| login.to_string());
            Ok(())
        })
    }

    pub fn set_password(&self, key: &str, pw: &str) -> Result<(), &'static str> {
        if pw.chars().count() < MIN_PASSWORD_CHARS || pw.len() > 1024 {
            return Err("weak_password");
        }
        let hash = hash_password(pw);
        self.update(|file| {
            file.accounts.get_mut(key).ok_or("no_password")?.pbkdf2_sha256 = hash;
            Ok(())
        })
    }

    /// 비밀번호가 맞는가. 없는 계정·막힌 계정도 같은 계산을 거친다 — 걸린 시간으로 계정 유무를
    /// 가늠하지 못하게.
    pub fn check(&self, name: &str, pw: &str) -> bool {
        let (hash, usable) = self.with(|f| match f.accounts.get(name) {
            Some(a) => (a.pbkdf2_sha256.clone(), !a.disabled),
            None => {
                let iterations = f.accounts.values().next().map_or(ITERATIONS, |a| a.pbkdf2_sha256.iterations);
                (decoy(iterations), false)
            }
        });
        verify_password(pw, &hash) && usable
    }
}

/// 없는 계정용 미끼 해시 — 반복 수마다 한 번 만들어 둔다.
fn decoy(iterations: u32) -> PwHash {
    static D: std::sync::OnceLock<Mutex<HashMap<u32, PwHash>>> = std::sync::OnceLock::new();
    let map = D.get_or_init(Default::default);
    let mut m = map.lock().unwrap();
    m.entry(iterations)
        .or_insert_with(|| hash_password_with(&new_token(), iterations))
        .clone()
}

/// 로그인 시도 제한 — 계정별 잠금과 IP 별 분당 상한.
#[derive(Default)]
pub struct Limiter {
    accounts: Mutex<HashMap<String, (u32, Option<Instant>)>>,
    ips: Mutex<HashMap<String, (Instant, u32)>>,
}

impl Limiter {
    /// 지금 시도해도 되나. 안 되면 기다릴 시간.
    pub fn allow(&self, account: &str, ip: &str) -> Result<(), Duration> {
        let now = Instant::now();
        {
            let mut ips = self.ips.lock().unwrap();
            ips.retain(|_, (start, _)| now.duration_since(*start) < Duration::from_secs(60));
            let e = ips.entry(ip.to_string()).or_insert((now, 0));
            if e.1 >= PER_IP_PER_MINUTE {
                return Err(Duration::from_secs(60).saturating_sub(now.duration_since(e.0)));
            }
            e.1 += 1;
        }
        let accounts = self.accounts.lock().unwrap();
        if let Some((_, Some(until))) = accounts.get(account) {
            if *until > now {
                return Err(*until - now);
            }
        }
        Ok(())
    }

    pub fn record(&self, account: &str, ok: bool) {
        let mut accounts = self.accounts.lock().unwrap();
        if ok {
            accounts.remove(account);
            return;
        }
        let e = accounts.entry(account.to_string()).or_insert((0, None));
        e.0 += 1;
        if e.0 >= FREE_FAILURES {
            let secs = 1u64 << (e.0 - FREE_FAILURES).min(10);
            e.1 = Some(Instant::now() + Duration::from_secs(secs).min(MAX_LOCK));
        }
    }
}

pub fn stdin_is_tty() -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::isatty(libc::STDIN_FILENO) == 1 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// 화면에 안 찍히게 한 줄을 읽는다. 터미널이 아니면(파이프) 그냥 읽는다.
pub fn read_secret(prompt: &str) -> anyhow::Result<String> {
    use std::io::Write as _;
    eprint!("{prompt}");
    let _ = std::io::stderr().flush();
    #[cfg(unix)]
    let saved = if stdin_is_tty() {
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut t) == 0 {
                let old = t;
                t.c_lflag &= !libc::ECHO;
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &t);
                Some(old)
            } else {
                None
            }
        }
    } else {
        None
    };
    let mut line = String::new();
    let read = std::io::stdin().read_line(&mut line);
    #[cfg(unix)]
    if let Some(old) = saved {
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &old);
        }
        eprintln!();
    }
    read?;
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_round_trip_and_wrong_password() {
        let h = hash_password_with("correct horse", 1000);
        assert!(verify_password("correct horse", &h));
        assert!(!verify_password("correct horsf", &h));
        let h2 = hash_password_with("correct horse", 1000);
        assert_ne!(h.salt, h2.salt, "소금이 매번 달라야 한다");
    }

    #[test]
    fn accounts_reload_when_the_file_changes_and_disabled_cannot_log_in() {
        let dir = std::env::temp_dir().join(format!("kasa-acc-{}", uuid::Uuid::new_v4()));
        let p = dir.join("relay-accounts.json");
        let mut f = AccountsFile::default();
        f.accounts.insert("geno".into(), Account { pbkdf2_sha256: hash_password_with("pw-1", 1000), created: 1, disabled: false, login: None });
        save_accounts(&p, &f).unwrap();
        let acc = Accounts::new(Some(p.clone()));
        assert!(acc.check("geno", "pw-1"));
        assert!(!acc.check("geno", "pw-2"));
        assert!(!acc.check("nobody", "pw-1"));
        std::thread::sleep(Duration::from_millis(20));
        f.accounts.get_mut("geno").unwrap().disabled = true;
        save_accounts(&p, &f).unwrap();
        // mtime 해상도가 거친 파일시스템에서도 바뀌게 한 번 더 건드린다.
        let later = SystemTime::now() + Duration::from_secs(2);
        let _ = std::fs::File::options().write(true).open(&p).and_then(|file| file.set_modified(later));
        assert!(!acc.check("geno", "pw-1"), "막힌 계정이 로그인됐다");
        assert!(!acc.active("geno"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_changed_login_signs_in_and_the_old_key_stays_reserved() {
        let dir = std::env::temp_dir().join(format!("kasa-acc-{}", uuid::Uuid::new_v4()));
        let p = dir.join("relay-accounts.json");
        let mut f = AccountsFile::default();
        for name in ["geno", "other"] {
            f.accounts.insert(name.into(), Account { pbkdf2_sha256: hash_password_with("pw-1", 1000), created: 1, disabled: false, login: None });
        }
        save_accounts(&p, &f).unwrap();
        let acc = Accounts::new(Some(p.clone()));
        assert_eq!(acc.set_login("geno", "other"), Err("login_taken"));
        assert_eq!(acc.set_login("geno", "oauth_x1"), Err("invalid_login"));
        assert_eq!(acc.set_login("oauth_abc", "fresh"), Err("no_password"));
        acc.set_login("geno", "kasa").unwrap();
        assert_eq!(acc.check_login("kasa", "pw-1").as_deref(), Some("geno"));
        assert_eq!(acc.check_login("geno", "pw-1"), None, "옛 아이디로 들어왔다");
        assert_eq!(acc.login_of("geno").as_deref(), Some("kasa"));
        assert_eq!(acc.set_login("other", "geno"), Err("login_taken"), "옛 아이디가 남에게 풀렸다");
        assert_eq!(acc.set_login("other", "kasa"), Err("login_taken"));
        acc.set_password("geno", "short").unwrap_err();
        acc.set_password("geno", "new-password").unwrap();
        assert_eq!(acc.check_login("kasa", "pw-1"), None);
        assert_eq!(acc.check_login("kasa", "new-password").as_deref(), Some("geno"));
        acc.set_login("geno", "geno").unwrap();
        assert_eq!(acc.check_login("geno", "new-password").as_deref(), Some("geno"));
        assert_eq!(acc.check_login("kasa", "new-password"), None);
        let saved = load_accounts(&p);
        assert!(saved.accounts["geno"].login.is_none(), "되돌린 아이디가 기록에 남았다");
        assert!(!std::fs::read_to_string(&p).unwrap().contains("new-password"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn repeated_failures_lock_the_account_but_not_others() {
        let l = Limiter::default();
        for _ in 0..FREE_FAILURES {
            assert!(l.allow("geno", "ip-a").is_ok());
            l.record("geno", false);
        }
        assert!(l.allow("geno", "ip-b").is_err(), "연달아 틀린 계정이 안 잠겼다");
        assert!(l.allow("other", "ip-b").is_ok());
    }

    #[test]
    fn one_ip_is_capped_per_minute() {
        let l = Limiter::default();
        for i in 0..PER_IP_PER_MINUTE {
            assert!(l.allow(&format!("a{i}"), "ip").is_ok());
        }
        assert!(l.allow("fresh", "ip").is_err());
        assert!(l.allow("fresh", "other-ip").is_ok());
    }

    #[test]
    fn names_and_tokens_have_the_expected_shape() {
        assert!(valid_account_name("geno"));
        assert!(valid_account_name("kasa_2"));
        for bad in ["G", "Geno", "a", "has space", "한글"] {
            assert!(!valid_account_name(bad), "{bad}");
        }
        let t = new_token();
        assert!(t.starts_with(TOKEN_PREFIX) && t.len() > 40);
        assert_ne!(new_token(), t);
        assert_eq!(token_hash(&t).len(), 64);
    }
}
