//! 앱 안에서 도는 북마크·탭 동기화 스레드와 설정 화면이 부르는 동작. 로그인 금고 열쇠가 있고 사람이 지문을 확인한
//! 기기에서, 사람이 「북마크 맞추기」나 「탭 보내기」를 켰을 때만 일한다. 둘 다 기본은 꺼짐이다.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::bridge::{self, Bridge};
use super::engine::{Counts, Engine, SyncError};
use super::state::State;
use crate::account_sync::local::account_key;
use crate::device_auth;
use crate::login_vault::engine::RemoteError;
use crate::login_vault::http::Http;
use crate::login_vault::{key_confirmed, vault_key};

const INTERVAL: Duration = Duration::from_secs(15);
const MAX_BACKOFF: Duration = Duration::from_secs(300);
/// 금고 열쇠의 갈래 이름 — 이 이름이 바뀌면 다른 열쇠가 된다.
const PURPOSE: &str = "browser-sync";

/// 사람이 켠 것. 계정마다 따로다.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub bookmarks: bool,
    #[serde(default)]
    pub tabs: bool,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn sync_dir() -> Option<PathBuf> {
    Some(kasa_socket::home_dir()?.join(".config").join("kasaterm").join("browser-sync"))
}

fn state_path(account: &str) -> Option<PathBuf> {
    Some(sync_dir()?.join(format!("state-{account}.json")))
}

fn settings_path(account: &str) -> Option<PathBuf> {
    Some(sync_dir()?.join(format!("settings-{account}.json")))
}

fn load_settings(account: &str) -> Settings {
    settings_path(account).and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn current_account() -> Option<String> {
    let (credential, _) = device_auth::capture()?;
    Some(account_key(&credential.relay, &credential.account))
}

/// 지금 계정에서 켠 것.
pub fn settings() -> Settings {
    current_account().map(|a| load_settings(&a)).unwrap_or_default()
}

/// 켜고 끈다. 「북마크 맞추기」를 다시 켜면 관문을 처음부터 다시 받는다 — 꺼 둔 사이 다른 기기가 바꾼 것을 놓치지 않게.
pub fn set(bookmarks: Option<bool>, tabs: Option<bool>) -> Result<Settings, String> {
    let account = current_account().ok_or("로그인하지 않았어요")?;
    let mut settings = load_settings(&account);
    let turned_on = bookmarks == Some(true) && !settings.bookmarks;
    settings.bookmarks = bookmarks.unwrap_or(settings.bookmarks);
    settings.tabs = tabs.unwrap_or(settings.tabs);
    let path = settings_path(&account).ok_or("설정 폴더가 없어요")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|_| "설정을 저장하지 못했어요")?;
    }
    let text = serde_json::to_string(&settings).map_err(|_| "설정을 저장하지 못했어요")?;
    kasa_identity::relay_auth::write_private(&path, &text).map_err(|_| "설정을 저장하지 못했어요")?;
    if turned_on {
        if let Some(path) = state_path(&account) {
            let mut state = State::load(&path);
            state.revision = 0;
            for known in state.marks.values_mut() {
                known.rev = 0;
            }
            let _ = state.save(&path);
        }
    }
    poke();
    Ok(settings)
}

static ALLOW_BULK_DELETE: AtomicBool = AtomicBool::new(false);

/// 사람이 「지우기 N개 — 확인」을 눌렀다.
pub fn allow_deletions() {
    ALLOW_BULK_DELETE.store(true, Ordering::Release);
    poke();
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

/// 설정 화면이 읽는 지금 상태. 주소·제목은 없고 개수뿐이다.
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
    let quiet = Counts { browser_away: counts.browser_away, ..Counts::default() };
    if *counts == quiet {
        return;
    }
    eprintln!(
        "[browser-sync] 올림 {} · 브라우저 고침 {} · 지우기 대기 {} · 미룸 {} · 실패 {} · 안 풀림 {} · 탭 보냄 {} · 기기 {}",
        counts.uploaded,
        counts.applied,
        counts.deletions_held,
        counts.stale,
        counts.failed,
        counts.undecryptable,
        u8::from(counts.tabs_sent),
        counts.devices_shown
    );
}

fn round() -> Duration {
    let Some((credential, stamp)) = device_auth::capture() else {
        set_status(json!({"state": "signed_out"}));
        return INTERVAL;
    };
    let account = account_key(&credential.relay, &credential.account);
    let settings = load_settings(&account);
    let Some(path) = state_path(&account) else { return INTERVAL };
    let mut state = State::load(&path);
    // 둘 다 끄고 뒷정리(내 탭 묘비·받은 탭 비우기)도 끝났으면 관문에 묻지도 않는다.
    if !settings.bookmarks && !settings.tabs && state.idle {
        set_status(json!({"state": "off", "settings": settings}));
        return INTERVAL;
    }
    let key = match vault_key::load(&account) {
        Ok(Some(key)) if key_confirmed(&account) => key.subkey(PURPOSE),
        Ok(_) => {
            set_status(json!({"state": "needs_vault", "settings": settings}));
            return INTERVAL;
        }
        Err(_) => {
            set_status(json!({"state": "error", "message": "키체인에서 금고 열쇠를 읽지 못했어요", "settings": settings}));
            return INTERVAL;
        }
    };
    let Some(dir) = bridge::data_dir() else {
        set_status(json!({"state": "unsupported_platform", "settings": settings}));
        return INTERVAL;
    };
    let Ok(http) = Http::at(&credential, "/relay/browser-sync") else { return INTERVAL };
    let browser = Bridge::new(dir);
    let device_name = DEVICE_NAME.get().and_then(|name| name()).unwrap_or_else(|| "이름 없는 기기".into());
    let engine = Engine {
        account: &credential.account,
        device: &credential.device_id,
        device_name: &device_name,
        key: &key,
        browser: &browser,
        remote: &http,
        now_ms: now_ms(),
        allow_bulk_delete: ALLOW_BULK_DELETE.swap(false, Ordering::AcqRel),
        sync_bookmarks: settings.bookmarks,
        send_tabs: settings.tabs,
    };
    let result = engine.round(&mut state);
    if !device_auth::stamp_is_current(&stamp) {
        return INTERVAL;
    }
    if let Ok(counts) = &result {
        state.idle = !settings.bookmarks && !settings.tabs && !counts.browser_away && counts.failed == 0;
    }
    let _ = state.save(&path);
    match result {
        Ok(counts) => {
            log_counts(&counts);
            let bookmarks = state.marks.values().filter(|k| k.alive()).count();
            let devices = state.remote_tabs.values().filter(|(_, r)| !r.gone && r.device != credential.device_id).count();
            let phase = match () {
                _ if counts.deletions_held > 0 => "deletions_pending",
                _ if counts.browser_away => "browser_closed",
                _ => "synced",
            };
            set_status(json!({
                "state": phase,
                "settings": settings,
                "bookmarks": bookmarks,
                "devices": devices,
                "deletions": counts.deletions_held,
                "at_ms": now_ms(),
            }));
            INTERVAL
        }
        Err(error) => {
            let (phase, message) = match error {
                SyncError::KeyMismatch | SyncError::Undecryptable => ("key_mismatch", "이 기기의 금고 열쇠가 계정 동기화 기록과 달라요. 로그인 동기화를 다시 등록해야 해요"),
                SyncError::ClockSkew => ("clock_mismatch", "이 기기 시계와 관문 시계가 5분 넘게 달라요. 시계를 맞추면 다시 맞춰요"),
                SyncError::Remote(RemoteError::Unauthorized) => ("reauth_required", "다시 로그인해야 동기화할 수 있어요"),
                SyncError::Remote(RemoteError::Unsupported) => ("unsupported", "이 관문은 북마크·탭 동기화를 아직 지원하지 않아요"),
                SyncError::Browser(_) => ("error", "카사브라우저와 주고받지 못했어요"),
                SyncError::Remote(_) => ("offline", "관문에 닿지 않아요. 이 기기의 북마크는 그대로예요"),
            };
            eprintln!("[browser-sync] 멈춤 {phase}");
            set_status(json!({"state": phase, "message": message, "settings": settings}));
            if phase == "offline" { MAX_BACKOFF.min(INTERVAL * 4) } else { INTERVAL }
        }
    }
}

/// 다른 기기의 「다른 기기의 탭」에 보일 이 기기 이름 — 앱이 사람이 붙인 기기 이름을 준다.
static DEVICE_NAME: OnceLock<fn() -> Option<String>> = OnceLock::new();

pub fn spawn(device_name: fn() -> Option<String>) {
    if cfg!(any(test, feature = "test-isolation")) || !device_auth::sync_environment_allowed() {
        return;
    }
    let _ = DEVICE_NAME.set(device_name);
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    let _ = std::thread::Builder::new().name("browser-sync".into()).spawn(|| {
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
