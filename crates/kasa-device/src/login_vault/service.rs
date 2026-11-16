//! 앱 안에서 도는 동기화 스레드와 설정 화면이 부르는 동작. 이 기기에 금고 열쇠가 있을 때만 일한다 —
//! 열쇠는 사람이 「금고 만들기」(첫 기기)나 「이 기기 등록」(폰 Face ID)을 눌러야 생긴다.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};

use super::crypto::VaultKey;
use super::engine::{Counts, Engine, Remote, RemoteError, SyncError};
use super::http::Http;
use super::join::{self, Enrollment, Ledger};
use super::server::{Approval, JOIN_TTL_MS};
use super::state::State;
use super::store::LoginStore;
use super::vault_key;
use crate::account_sync::local::account_key;
use crate::device_auth::{self, approvals};

const INTERVAL: Duration = Duration::from_secs(15);
/// 등록 양쪽이 관문 시계와 이만큼 넘게 다르면 시계 탓으로 본다 — 새 기기는 등록을 시작하지 않고, 열쇠를 가진 기기는
/// 만료로 거절한 등록을 설정에 「시계 때문에」로 알린다. 아니면 새 기기는 까닭 모를 시간 초과만 보게 된다.
const JOIN_CLOCK_TOLERANCE_MS: u64 = 60_000;
/// 폰 승인 도전문의 수명. 관문 비밀 승인과 같은 2분.
const APPROVAL_TTL: Duration = Duration::from_secs(120);
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

/// 등록으로 받은 열쇠의 지문을 사람이 두 기기 화면에서 맞춰 보기 전이라는 표식. 있는 동안은 받지도 올리지도 않는다 —
/// 관문이 자기 열쇠를 봉인해 넘겼으면 첫 바퀴에 이 기기의 웹 로그인이 그 열쇠로 올라가기 때문이다.
fn unconfirmed_path(account: &str) -> Option<PathBuf> {
    Some(vault_dir()?.join(format!("unconfirmed-{account}")))
}

/// 사람이 「지우기 N개 — 확인」을 눌렀다. 다음 한 바퀴만 한도를 넘는 지우기를 올린다.
static ALLOW_BULK_DELETE: AtomicBool = AtomicBool::new(false);

/// 이 기기가 새 기기 등록을 시계 때문에 마지막으로 거절한 때(ms). 설정에 15분 동안 알린다.
static CLOCK_REFUSED_AT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
const CLOCK_REFUSED_SHOWN_MS: u64 = 15 * 60 * 1000;

fn ledger() -> &'static Mutex<Ledger> {
    static LEDGER: OnceLock<Mutex<Ledger>> = OnceLock::new();
    LEDGER.get_or_init(Mutex::default)
}

/// 설정에 보일 믿는 폰 열쇠 — 사람이 직접 믿었나(`manual`)까지. 값은 없다.
fn phones() -> Value {
    let list = crate::op_approval::trusted().unwrap_or_default();
    Value::Array(
        list.iter()
            .map(|t| json!({"id": t.id, "label": t.label, "fingerprint": kasa_identity::approval_text::key_fingerprint_long(&t.id), "manual": t.manual}))
            .collect(),
    )
}

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

fn log_counts(counts: &Counts, sealed: usize) {
    if *counts == Counts::default() && sealed == 0 {
        return;
    }
    eprintln!(
        "[login-vault] 올림 {} · 받음 {} · 묘비 {} · 보류 {} · 지우기 대기 {} · 실패 {} · 안 풀림 {} · 열쇠 넘김 {}",
        counts.uploaded, counts.received, counts.tombstones, counts.held, counts.deletions_held, counts.failed, counts.undecryptable, sealed
    );
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
            Ok(_) => "needs_join",
            Err(RemoteError::Unsupported) => "unsupported",
            Err(_) => "offline",
        };
        if !joining() {
            set_status(json!({"state": state}));
        }
        return INTERVAL;
    };
    let Some(store) = platform_store() else {
        set_status(json!({"state": "unsupported_platform", "fingerprint": key.fingerprint()}));
        return INTERVAL;
    };
    if unconfirmed_path(&account).is_some_and(|p| p.exists()) {
        let origin = http.fetch(u64::MAX).ok();
        set_status(json!({
            "state": "confirm_fingerprint",
            "fingerprint": key.fingerprint(),
            "created_by": origin.as_ref().map(|f| f.created_by.clone()).unwrap_or_default(),
            "created_ms": origin.map_or(0, |f| f.created_ms),
        }));
        return INTERVAL;
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
            // 금고 열쇠가 맞는 바퀴에서만 남의 등록에 열쇠를 넘긴다 — 열쇠가 어긋난 기기의 봉인이 진짜를 앞지르지 않게.
            let sealed = serve_joins(&http, &key, &credential.account);
            log_counts(&counts, sealed);
            let refused_at = CLOCK_REFUSED_AT.load(Ordering::Relaxed);
            set_status(json!({
                "state": if counts.deletions_held > 0 { "deletions_pending" } else { "synced" },
                "fingerprint": key.fingerprint(),
                "items": state.items.values().filter(|k| !k.deleted).count(),
                "held": counts.held,
                "deletions": counts.deletions_held,
                "clock_refused": refused_at > 0 && now_ms().saturating_sub(refused_at) < CLOCK_REFUSED_SHOWN_MS,
                "phones": phones(),
                "at_ms": now_ms(),
            }));
            INTERVAL
        }
        Err(error) => {
            let (state, message) = match error {
                SyncError::KeyMismatch | SyncError::Undecryptable => ("key_mismatch", "이 기기의 금고 열쇠가 계정 금고와 달라요. 다시 등록해야 해요"),
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

/// 열쇠를 가진 기기의 몫 — 폰 승인을 받은 등록에 열쇠를 봉인해 준다. 대기 중인 등록이 있을 때만 폰 열쇠 목록을 묻는다.
fn serve_joins(http: &Http, key: &VaultKey, account: &str) -> usize {
    use super::join::Joins as _;
    if http.pending_joins().map_or(true, |p| p.is_empty()) {
        return 0;
    }
    let trusted = crate::op_approval::trusted().unwrap_or_default();
    let live = approvals::keys_blocking().unwrap_or_default();
    let Ok(mut ledger) = ledger().lock() else { return 0 };
    let served = join::serve(http, key, account, &trusted, &live, &mut ledger, now_ms());
    if served.refused > 0 {
        eprintln!("[login-vault] 등록 거절 {} · 그중 시계 {}", served.refused, served.clock);
    }
    // 만료 거절은 폰 승인이 끝자락에 와도 난다 — 이 기기 시계가 관문과 정말 어긋났을 때만 시계 탓으로 알린다.
    let off_clock = || http.fetch(u64::MAX).map_or(true, |f| now_ms().abs_diff(f.now_ms) > JOIN_CLOCK_TOLERANCE_MS);
    if served.clock > 0 && off_clock() {
        CLOCK_REFUSED_AT.store(now_ms(), Ordering::Relaxed);
    }
    served.sealed
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
            poke();
            Ok(key.fingerprint())
        }
        Err(error) => {
            let _ = vault_key::forget(&account);
            Err(match error {
                RemoteError::Exists(_) => "계정에 이미 금고가 있어요. 「이 기기 등록」을 눌러 주세요".into(),
                RemoteError::Unsupported => "이 관문은 로그인 동기화를 아직 지원하지 않아요".into(),
                _ => "관문에 금고를 만들지 못했어요".into(),
            })
        }
    }
}

static JOINING: AtomicBool = AtomicBool::new(false);

fn joining() -> bool {
    JOINING.load(Ordering::Acquire)
}

/// 새 기기 등록을 뒤에서 진행한다. 진행은 [`status`] 의 `joining`·`join_failed` 로 보인다.
pub fn start_join(label: &str) -> Result<(), String> {
    if JOINING.swap(true, Ordering::AcqRel) {
        return Err("등록이 이미 진행 중이에요".into());
    }
    let label: String = label.chars().take(64).collect();
    let result = std::thread::Builder::new().name("login-vault-join".into()).spawn(move || {
        let outcome = enroll(&label);
        JOINING.store(false, Ordering::Release);
        match outcome {
            Ok(fingerprint) => {
                set_status(json!({"state": "joined", "fingerprint": fingerprint}));
                poke();
            }
            Err(reason) => set_status(json!({"state": "join_failed", "message": reason})),
        }
    });
    if result.is_err() {
        JOINING.store(false, Ordering::Release);
        return Err("등록을 시작하지 못했어요".into());
    }
    Ok(())
}

fn nonce() -> String {
    use ring::rand::SecureRandom as _;
    let mut bytes = [0u8; 16];
    let _ = ring::rand::SystemRandom::new().fill(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn enroll(label: &str) -> Result<String, String> {
    let (credential, stamp) = device_auth::capture().ok_or("로그인하지 않았어요")?;
    let account = account_key(&credential.relay, &credential.account);
    let http = Http::new(&credential).map_err(|_| "관문에 닿지 않아요")?;
    let origin = http.fetch(u64::MAX).map_err(|_| "관문에 닿지 않아요")?;
    let key_id = origin.key_id;
    if key_id.is_empty() {
        return Err("계정에 금고가 없어요. 「금고 만들기」를 눌러 주세요".into());
    }
    let skew = now_ms().abs_diff(origin.now_ms);
    if origin.now_ms == 0 || skew > JOIN_CLOCK_TOLERANCE_MS {
        return Err(format!("이 기기 시계가 관문 시계와 {}분 넘게 달라요. 시계를 맞춘 뒤 다시 등록해 주세요", (skew / 60_000).max(1)));
    }
    let enrollment = Enrollment::open(&http, label).map_err(|_| "등록을 열지 못했어요")?;
    set_status(json!({"state": "joining", "step": "phone", "code": enrollment.code}));
    // 만료는 관문 시계로 잡는다 — 관문과 열쇠를 가진 기기가 같은 시계로 판정한다.
    let exp = origin.now_ms + APPROVAL_TTL.as_millis() as u64;
    let text = enrollment.challenge(&credential.account, &credential.device_id, &nonce(), exp, label);
    let view = approvals::create_secret_blocking(&text).map_err(|e| format!("폰 승인을 요청하지 못했어요({e})"))?;
    let id = view["id"].as_str().unwrap_or_default().to_string();
    let asked = std::time::Instant::now();
    let decided = loop {
        if asked.elapsed() > APPROVAL_TTL + Duration::from_secs(5) {
            let _ = approvals::cancel_blocking(&id, "gone");
            return Err("폰 승인 시간이 지났어요".into());
        }
        match approvals::wait_one_blocking(&id, 20) {
            Ok(view) if view["state"] == "pending" => {}
            Ok(view) => break view,
            Err(error) => {
                if let Some(view) = approvals::closed_view(&error) {
                    break view;
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        }
    };
    if decided["state"] != "allowed" {
        return Err("폰에서 허락하지 않았어요".into());
    }
    let approval = Approval {
        challenge: decided["challenge"].as_str().unwrap_or_default().into(),
        key: decided["key"].as_str().unwrap_or_default().into(),
        sig: decided["sig"].as_str().unwrap_or_default().into(),
        by_device: decided["by"]["device"].as_str().unwrap_or_default().into(),
    };
    enrollment.approved(&http, &approval).map_err(|_| "승인을 관문에 올리지 못했어요")?;
    set_status(json!({"state": "joining", "step": "holder", "code": enrollment.code}));
    let waiting_since = std::time::Instant::now();
    let mut waiting = enrollment;
    let key = loop {
        if waiting_since.elapsed() > Duration::from_millis(JOIN_TTL_MS) {
            return Err("열쇠를 가진 기기가 10분 안에 답하지 않았어요. 그 기기가 켜져 있고 시계가 맞는지(설정의 로그인 동기화에 「시계가 맞지 않아요」가 없는지) 보고 다시 해 주세요".into());
        }
        match waiting.finish(&http, &credential.account, &credential.device_id, &key_id) {
            Ok(Ok(key)) => break key,
            Ok(Err(again)) => waiting = again,
            Err(_) => return Err("받은 열쇠가 계정 금고와 맞지 않아요".into()),
        }
        std::thread::sleep(Duration::from_secs(3));
    };
    if !device_auth::stamp_is_current(&stamp) {
        return Err("그사이 로그인이 바뀌었어요".into());
    }
    // 열쇠보다 표식을 먼저 둔다 — 그 사이 동기화 스레드가 확인 전 열쇠로 한 바퀴 돌지 않게.
    let flag = unconfirmed_path(&account).ok_or("설정 폴더를 찾지 못했어요")?;
    if let Some(dir) = flag.parent() {
        std::fs::create_dir_all(dir).map_err(|_| "설정 폴더를 만들지 못했어요")?;
    }
    kasa_identity::relay_auth::write_private(&flag, "").map_err(|_| "설정 폴더에 쓰지 못했어요")?;
    vault_key::save(&account, &key).map_err(|_| "키체인에 열쇠를 두지 못했어요")?;
    Ok(key.fingerprint())
}

/// 사람이 두 기기 화면의 열쇠 지문이 같다고 눌렀다 — 이제 맞추기 시작한다.
pub fn confirm_fingerprint() -> Result<(), String> {
    let (credential, _) = device_auth::capture().ok_or("로그인하지 않았어요")?;
    let account = account_key(&credential.relay, &credential.account);
    if let Some(flag) = unconfirmed_path(&account) {
        let _ = std::fs::remove_file(flag);
    }
    poke();
    Ok(())
}

/// 사람이 「지우기 N개 — 확인」을 눌렀다.
pub fn allow_deletions() {
    ALLOW_BULK_DELETE.store(true, Ordering::Release);
    poke();
}

/// 사람이 폰 지문을 보고 그 열쇠를 금고 승인에 믿었다.
pub fn confirm_phone(id: &str) -> Result<(), String> {
    crate::op_approval::confirm(id).map_err(|_| "그 폰 열쇠를 찾지 못했어요".to_string())?;
    poke();
    Ok(())
}
