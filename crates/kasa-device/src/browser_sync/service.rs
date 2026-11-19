//! 앱 안에서 도는 북마크·탭 동기화 스레드. 로그인 금고 열쇠가 있고 사람이 지문을 확인한 기기에서는 늘 맞춘다 —
//! 켜고 끄는 단추가 있으면 기기마다 켜 줘야 해서 안 맞춰진 채로 남았다(2026-11-18). 바퀴마다 카사브라우저 설정
//! 「동기화」에 보일 상태(`display`)를 브라우저에 넘긴다.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

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

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn sync_dir() -> Option<PathBuf> {
    Some(kasa_socket::home_dir()?.join(".config").join("kasaterm").join("browser-sync"))
}

fn state_path(account: &str) -> Option<PathBuf> {
    Some(sync_dir()?.join(format!("state-{account}.json")))
}

fn current_account() -> Option<String> {
    let (credential, _) = device_auth::capture()?;
    Some(account_key(&credential.relay, &credential.account))
}

/// 켜고 끄던 때의 설정 파일을 걷는다. 북마크를 꺼 둔 기기는 그사이 다른 기기가 바꾼 것을 놓쳤으니, 다시 켤 때처럼
/// 관문을 처음부터 받고 그 첫 바퀴에 사라진 것은 사람 확인까지 보류한다.
fn retire_toggle(account: &str, state: &mut State) {
    let Some(path) = sync_dir().map(|d| d.join(format!("settings-{account}.json"))) else { return };
    let Ok(bytes) = std::fs::read(&path) else { return };
    let bookmarks_were_on = serde_json::from_slice::<Value>(&bytes).is_ok_and(|v| v["bookmarks"] == true);
    if !bookmarks_were_on {
        state.revision = 0;
        for known in state.marks.values_mut() {
            known.rev = 0;
        }
        state.rejoin = true;
    }
    let _ = std::fs::remove_file(path);
}

/// 사람이 확인한 보류 지우기 — 누른 때 화면에 보인 그 목록이다. 그 목록을 다 처리한 바퀴가 끝날 때까지 남는다.
static CONFIRMED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// 사람이 「지우기 N개 — 확인」을 눌렀다. 그때 보류 중이던 것만 지운다 — 그 뒤 더 사라진 것은 다시 묻는다.
pub fn allow_deletions() {
    let Some(path) = current_account().and_then(|a| state_path(&a)) else { return };
    let pending = State::load(&path).pending_deletions;
    if let Ok(mut confirmed) = CONFIRMED.lock() {
        confirmed.extend(pending);
    }
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
        "[browser-sync] 올림 {} · 브라우저 고침 {} · 지우기 대기 {} · 미룸 {} · 실패 {} · 안 풀림 {} · 탭 보냄 {} · 기기 {} · 계정 폴더 {}",
        counts.uploaded,
        counts.applied,
        counts.deletions_held,
        counts.stale,
        counts.failed,
        counts.undecryptable,
        u8::from(counts.tabs_sent),
        counts.devices_shown,
        counts.accounts_made
    );
}

fn round() -> Duration {
    let Some((credential, stamp)) = device_auth::capture() else {
        set_status(json!({"state": "signed_out"}));
        return INTERVAL;
    };
    let account = account_key(&credential.relay, &credential.account);
    let Some(path) = state_path(&account) else { return INTERVAL };
    let mut state = State::load(&path);
    let key = match vault_key::load(&account) {
        Ok(Some(key)) if key_confirmed(&account) => {
            retire_toggle(&account, &mut state);
            key.subkey(PURPOSE)
        }
        Ok(_) => {
            set_status(json!({"state": "needs_vault"}));
            return INTERVAL;
        }
        Err(_) => {
            set_status(json!({"state": "error", "message": "키체인에서 금고 열쇠를 읽지 못했어요"}));
            return INTERVAL;
        }
    };
    let Some(dir) = bridge::data_dir() else {
        set_status(json!({"state": "unsupported_platform"}));
        return INTERVAL;
    };
    let Ok(http) = Http::at(&credential, "/relay/browser-sync") else { return INTERVAL };
    let browser = Bridge::new(dir);
    let device_name = DEVICE_NAME.get().and_then(|name| name()).unwrap_or_else(|| "이름 없는 기기".into());
    let confirmed = CONFIRMED.lock().map(|c| c.clone()).unwrap_or_default();
    let engine = Engine {
        account: &credential.account,
        device: &credential.device_id,
        device_name: &device_name,
        key: &key,
        browser: &browser,
        remote: &http,
        now_ms: now_ms(),
        allowed_deletions: &confirmed,
        sync_bookmarks: true,
        send_tabs: true,
    };
    let result = engine.round(&mut state);
    if !device_auth::stamp_is_current(&stamp) {
        return INTERVAL;
    }
    if let Ok(counts) = &result {
        let clean = !counts.browser_away && counts.failed == 0 && counts.stale == 0;
        if clean {
            if let Ok(mut done) = CONFIRMED.lock() {
                done.retain(|id| !confirmed.contains(id));
            }
        }
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
                "bookmarks": bookmarks,
                "devices": devices,
                "deletions": counts.deletions_held,
                "too_large": counts.too_large,
                "at_ms": now_ms(),
            }));
            // 브라우저가 방금 만든 계정 폴더는 다음 바퀴가 처음 합류로 채운다 — 15초를 기다리지 않는다.
            if counts.accounts_made > 0 { Duration::from_secs(1) } else { INTERVAL }
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
            set_status(json!({"state": phase, "message": message}));
            if phase == "offline" { MAX_BACKOFF.min(INTERVAL * 4) } else { INTERVAL }
        }
    }
}

/// 카사브라우저 설정 「동기화」에 지금 상태를 넘긴다. 브라우저가 꺼져 있으면 그냥 넘어간다 — 켜지면 다음 바퀴에 간다.
fn show_in_browser() {
    let Some(dir) = bridge::data_dir() else { return };
    let state = super::display::display(&crate::login_vault::status(), &status(), now_ms());
    let _ = Bridge::new(dir).put_state(&state);
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
            show_in_browser();
            runtime.block_on(async {
                tokio::select! {
                    _ = notify().notified() => {}
                    _ = tokio::time::sleep(wait) => {}
                }
            });
        }
    });
}
