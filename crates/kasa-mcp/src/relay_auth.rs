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
        f.accounts.insert("geno".into(), Account { pbkdf2_sha256: hash_password_with("pw-1", 1000), created: 1, disabled: false });
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
