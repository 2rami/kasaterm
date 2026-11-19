//! 앱 안에서 도는 동기화 스레드와 설정 화면이 부르는 동작. 이 기기에 금고 열쇠가 있을 때만 맞춘다 —
//! 열쇠는 첫 기기에서 사람이 「금고 만들기」를 눌러 생기고, 그 뒤 기기는 관문에 맡긴 열쇠를 로그인만으로 받는다.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};

use super::crypto::VaultKey;
use super::engine::{Counts, Engine, Remote, RemoteError, SyncError};
use super::http::Http;
use super::state::State;
use super::store::LoginStore;
use super::vault_key;
use crate::account_sync::local::account_key;
use crate::device_auth;

const INTERVAL: Duration = Duration::from_secs(15);
const MAX_BACKOFF: Duration = Duration::from_secs(300);

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// 진짜 항목 접두사. 시험·격리 빌드만 접두사를 바꿀 수 있다 — 앱이 진짜 `web:` 를 엉뚱한 곳에 맞추지 않게.
fn prefix() -> String {
    if cfg!(any(test, feature = "test-isolation")) {
        return std::env::var("KASATERM_LOGIN_VAULT_PREFIX").unwrap_or_else(|_| super::store::TEST_PREFIX.into());
    }
    super::store::PREFIX.into()
}

fn platform_store() -> Option<Box<dyn LoginStore>> {
    #[cfg(windows)]
    {
        Some(Box::new(super::windows_store::WindowsStore::new(&prefix())))
    }
    #[cfg(target_os = "macos")]
    {
        Some(Box::new(super::macos_store::MacStore::new(&prefix())))
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = prefix();
        None
    }
}

fn vault_dir() -> Option<PathBuf> {
    Some(kasa_socket::home_dir()?.join(".config").join("kasaterm").join("login-vault"))
}

fn state_path(account: &str) -> Option<PathBuf> {
    Some(vault_dir()?.join(format!("state-{account}.json")))
}

/// 옛 판에서 폰 등록으로 열쇠를 받고 지문 확인을 기다리던 표식. 이제 관문에 맡긴 열쇠를 믿으니 보이면 걷는다 —
/// 있는 동안은 북마크·탭 동기화도 멈춰 있다([`key_confirmed`]).
fn unconfirmed_path(account: &str) -> Option<PathBuf> {
    Some(vault_dir()?.join(format!("unconfirmed-{account}")))
}

/// 사람이 「지우기 N개 — 확인」을 눌렀다. 다음 한 바퀴만 한도를 넘는 지우기를 올린다.
static ALLOW_BULK_DELETE: AtomicBool = AtomicBool::new(false);



fn status_cell() -> &'static Mutex<Value> {
    static STATUS: OnceLock<Mutex<Value>> = OnceLock::new();
    STATUS.get_or_init(|| Mutex::new(json!({"state": "checking"})))
}

fn set_status(value: Value) {
    if let Ok(mut status) = status_cell().lock() {
        *status = value;
    }
}

/// 설정 화면이 읽는 지금 상태. 사이트·아이디는 없고 개수와 열쇠 지문뿐이다.
pub fn status() -> Value {
    status_cell().lock().map(|s| s.clone()).unwrap_or_else(|_| json!({"state": "error"}))
}

fn notify() -> &'static tokio::sync::Notify {
    static NOTIFY: OnceLock<tokio::sync::Notify> = OnceLock::new();
    NOTIFY.get_or_init(tokio::sync::Notify::new)
}

pub fn poke() {
    notify().notify_one();
}

fn log_counts(counts: &Counts) {
    if *counts == Counts::default() {
        return;
    }
    eprintln!(
        "[login-vault] 올림 {} · 받음 {} · 묘비 {} · 보류 {} · 지우기 대기 {} · 실패 {} · 안 풀림 {}",
        counts.uploaded, counts.received, counts.tombstones, counts.held, counts.deletions_held, counts.failed, counts.undecryptable
    );
}

/// 열쇠가 없는 기기 — 관문에 맡긴 열쇠를 받아 둔다. 금고의 표식과 맞는 열쇠만 키체인에 둔다(관문이 다른 금고의 열쇠를
/// 내밀어도 이 기기가 그 열쇠로 웹 로그인을 올리지 않게). 받았으면 true.
fn take_escrow(http: &Http, account: &str, key_id: &str) -> Result<bool, RemoteError> {
    let Some(key) = http.escrow()? else { return Ok(false) };
    if key.key_id() != key_id {
        eprintln!("[login-vault] 맡긴 열쇠가 금고 표식과 달라 받지 않음");
        return Ok(false);
    }
    vault_key::save(account, &key).map_err(|_| RemoteError::Failed)?;
    Ok(true)
}

/// 이 프로세스에서 열쇠를 맡긴 계정. 관문은 같은 열쇠면 그대로 두니 한 번이면 된다 — 껐다 켜면 다시 확인한다.
fn escrowed() -> &'static Mutex<std::collections::HashSet<String>> {
    static ESCROWED: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    ESCROWED.get_or_init(Mutex::default)
}

/// 열쇠를 가진 기기의 몫 — 관문에 열쇠를 맡겨, 같은 계정으로 로그인한 새 기기가 받아 가게 한다.
/// 맡기는 창구가 없는 옛 관문이면 다음 바퀴에 다시 해 본다 — 관문을 갈아 끼운 뒤 앱을 다시 켜지 않아도 맡기게.
fn leave_escrow(http: &Http, key: &VaultKey, account: &str) {
    if escrowed().lock().map_or(true, |done| done.contains(account)) {
        return;
    }
    match http.put_escrow(key) {
        Ok(()) => {
            if let Ok(mut done) = escrowed().lock() {
                done.insert(account.into());
            }
        }
        Err(RemoteError::Unsupported) => {}
        Err(error) => eprintln!("[login-vault] 열쇠 맡기기 실패 {error:?}"),
    }
}

/// 한 바퀴의 결과를 상태로. 다음 바퀴까지 기다릴 시간을 준다.
fn round() -> Duration {
    let Some((credential, stamp)) = device_auth::capture() else {
        set_status(json!({"state": "signed_out"}));
        return INTERVAL;
    };
    let account = account_key(&credential.relay, &credential.account);
    let key = match vault_key::load(&account) {
        Ok(key) => key,
        Err(_) => {
            set_status(json!({"state": "error", "message": "키체인에서 금고 열쇠를 읽지 못했어요"}));
            return INTERVAL;
        }
    };
    let Ok(http) = Http::new(&credential) else { return INTERVAL };
    let Some(key) = key else {
        let state = match http.fetch(u64::MAX) {
            Ok(fetched) if fetched.key_id.is_empty() => "no_vault",
            Ok(fetched) => match take_escrow(&http, &account, &fetched.key_id) {
                Ok(true) => {
                    set_status(json!({"state": "joined"}));
                    return Duration::from_secs(1);
                }
                Ok(false) => "waiting_for_key",
                Err(RemoteError::Unsupported) => "unsupported",
                Err(_) => "offline",
            },
            Err(RemoteError::Unsupported) => "unsupported",
            Err(_) => "offline",
        };
        set_status(json!({"state": state}));
        return INTERVAL;
    };
    let Some(store) = platform_store() else {
        set_status(json!({"state": "unsupported_platform", "fingerprint": key.fingerprint()}));
        return INTERVAL;
    };
    if let Some(flag) = unconfirmed_path(&account).filter(|p| p.exists()) {
        let _ = std::fs::remove_file(flag);
    }
    let Some(path) = state_path(&account) else { return INTERVAL };
    let mut state = State::load(&path);
    let allow_bulk_delete = ALLOW_BULK_DELETE.swap(false, Ordering::AcqRel);
    // 봉인 AAD 는 관문 계정 이름으로 묶는다 — 기기마다 관문 주소 표기가 달라도 같은 금고가 풀린다.
    let engine = Engine {
        account: &credential.account,
        device: &credential.device_id,
        key: &key,
        store: store.as_ref(),
        remote: &http,
        now_ms: now_ms(),
        allow_bulk_delete,
    };
    let result = engine.round(&mut state);
    if !device_auth::stamp_is_current(&stamp) {
        return INTERVAL;
    }
    let _ = state.save(&path);
    match result {
        Ok(counts) => {
            // 금고 열쇠가 맞는 바퀴에서만 맡긴다 — 관문도 금고의 열쇠인지 다시 본다.
            leave_escrow(&http, &key, &account);
            log_counts(&counts);
            set_status(json!({
                "state": if counts.deletions_held > 0 { "deletions_pending" } else { "synced" },
                "fingerprint": key.fingerprint(),
                "items": state.items.values().filter(|k| !k.deleted).count(),
                "held": counts.held,
                "deletions": counts.deletions_held,
                "at_ms": now_ms(),
            }));
            INTERVAL
        }
        Err(error) => {
            let (state, message) = match error {
                SyncError::KeyMismatch | SyncError::Undecryptable => ("key_mismatch", "이 기기의 금고 열쇠가 계정 금고와 달라요"),
                SyncError::ClockSkew => ("clock_mismatch", "이 기기 시계와 관문 시계가 5분 넘게 달라요. 시계를 맞추면 다시 맞춰요"),
                SyncError::NoVault => ("no_vault", "계정에 금고가 없어요"),
                SyncError::Remote(RemoteError::Unauthorized) => ("reauth_required", "다시 로그인해야 동기화할 수 있어요"),
                SyncError::Remote(RemoteError::Unsupported) => ("unsupported", "이 관문은 로그인 동기화를 아직 지원하지 않아요"),
                SyncError::Store => ("error", "키체인 목록을 읽지 못했어요"),
                SyncError::Remote(_) => ("offline", "관문에 닿지 않아요. 로컬 로그인은 그대로예요"),
            };
            eprintln!("[login-vault] 멈춤 {state}");
            set_status(json!({"state": state, "message": message, "fingerprint": key.fingerprint()}));
            if state == "offline" { MAX_BACKOFF.min(INTERVAL * 4) } else { INTERVAL }
        }
    }
}


pub fn spawn() {
    if cfg!(any(test, feature = "test-isolation")) || !device_auth::sync_environment_allowed() {
        return;
    }
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    let _ = std::thread::Builder::new().name("login-vault".into()).spawn(|| {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
        loop {
            let wait = round();
            runtime.block_on(async {
                tokio::select! {
                    _ = notify().notified() => {}
                    _ = tokio::time::sleep(wait) => {}
                }
            });
        }
    });
}

/// 첫 기기: 금고를 만든다. 열쇠를 먼저 키체인에 두고 관문에 올린다 — 거꾸로면 올린 뒤 저장이 실패할 때 아무도 못 푸는 금고가 남는다.
pub fn create_vault() -> Result<String, String> {
    let (credential, _) = device_auth::capture().ok_or("로그인하지 않았어요")?;
    let account = account_key(&credential.relay, &credential.account);
    if vault_key::load(&account).map_err(|_| "키체인을 읽지 못했어요")?.is_some() {
        return Err("이 기기에는 이미 금고 열쇠가 있어요".into());
    }
    let key = VaultKey::generate().map_err(|_| "열쇠를 만들지 못했어요")?;
    vault_key::save(&account, &key).map_err(|_| "키체인에 열쇠를 두지 못했어요")?;
    let http = Http::new(&credential).map_err(|_| "관문에 닿지 않아요")?;
    match http.init(&key.key_id(), &key.sign_pub()) {
        Ok(()) => {
            leave_escrow(&http, &key, &account);
            poke();
            Ok(key.fingerprint())
        }
        Err(error) => {
            let _ = vault_key::forget(&account);
            Err(match error {
                RemoteError::Exists(_) => "계정에 이미 금고가 있어요. 맡긴 열쇠를 곧 받아요".into(),
                RemoteError::Unsupported => "이 관문은 로그인 동기화를 아직 지원하지 않아요".into(),
                _ => "관문에 금고를 만들지 못했어요".into(),
            })
        }
    }
}



/// 옛 판의 지문 확인 표식이 걷혔나 — 북마크·탭 동기화도 그 뒤에만 열쇠의 갈래로 일한다.
pub(crate) fn key_confirmed(account: &str) -> bool {
    unconfirmed_path(account).is_some_and(|flag| !flag.exists())
}

/// 사람이 「지우기 N개 — 확인」을 눌렀다.
pub fn allow_deletions() {
    ALLOW_BULK_DELETE.store(true, Ordering::Release);
    poke();
}

