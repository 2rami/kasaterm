//! 앱 업데이트 창구 — 이 기기 쪽 수락·받기·적용. 계약·검증·도우미는 `kasa_socket::app_update`, 절차는
//! `docs/app-update.md`.
//!
//! 설치 실행은 `KASATERM_APP_UPDATE=on` 일 때만 된다(기본 꺼짐) — 나쵸에 `kasaterm_update` 승인 계약이 생기기 전까지는
//! 이 앱이 받기조차 하지 않는다.

use super::*;
use kasa_socket::app_restart::Facts;
use kasa_socket::app_update::{self as update, Device, State, Step, SystemEffects, UpdateRequest};

pub(crate) fn enabled() -> bool {
    update::install_enabled(&|k| std::env::var(k).ok())
}

/// 받은 파일 자리. 격리 리그는 그 collab 루트 아래로 간다.
fn cache_dir() -> Result<std::path::PathBuf, String> {
    if let Some(root) = kasa_socket::isolated_collab_root() {
        return Ok(root.join("app-update-cache"));
    }
    kasa_socket::home_dir().map(|h| h.join("Library/Caches/kasaterm/updates")).ok_or_else(|| "home directory unavailable".into())
}

/// 한 번에 한 작업만 몬다 — 다시 보내기가 겹쳐도 받기·도우미가 둘 생기지 않는다.
fn driving() -> &'static Mutex<Option<String>> {
    static SLOT: std::sync::OnceLock<Mutex<Option<String>>> = std::sync::OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

/// 대상 쪽 수락 — 나쵸 승인과 이 기기의 지금 사실로 판정하고, 통과하면 작업을 적고 받기·준비·적용을 뒤에서 시작한다.
/// 같은 작업이 다시 오면 적힌 것을 돌려주고, 끊겼거나 기다리던 작업이면 이어서 몬다.
pub(crate) fn accept_update(facts: &Facts, req: &UpdateRequest, proxy: &winit::event_loop::EventLoopProxy<UserEvent>) -> Result<serde_json::Value, String> {
    let authority = crate::app_restart::NachoAuthority::for_request(&req.authority)?;
    let dir = update::jobs_dir().map_err(|e| e.to_string())?;
    let now = kasa_socket::board::now_ms();
    let (status, created) = update::accept(req, facts, crate::version::CURRENT, &authority, enabled(), &dir, now)?;
    if status.state < State::Armed {
        start_driving(dir, req.job.clone(), facts.app_path.clone(), proxy.clone());
    }
    Ok(serde_json::json!({"ok": true, "job_id": req.job.job_id, "created": created, "state": status.state.word()}))
}

fn start_driving(dir: std::path::PathBuf, job: update::UpdateJob, app_path: String, proxy: winit::event_loop::EventLoopProxy<UserEvent>) {
    {
        let mut slot = driving().lock().unwrap();
        if slot.is_some() {
            return;
        }
        *slot = Some(job.job_id.clone());
    }
    std::thread::spawn(move || {
        let asker = proxy.clone();
        // 바쁜 학생·미저장 편집기는 GUI 스레드만 안다 — 받은 뒤 갈아 끼우기 직전에 다시 묻는다.
        let facts_now = move || -> Result<Facts, String> {
            let (tx, rx) = std::sync::mpsc::channel();
            asker.send_event(UserEvent::SocketRestartFacts(tx)).map_err(|_| "app event loop is gone".to_string())?;
            rx.recv_timeout(std::time::Duration::from_secs(2)).map_err(|_| "app did not answer facts in time".to_string())
        };
        let result = (|| {
            let effects = SystemEffects;
            let dev = Device {
                dir,
                cache: cache_dir()?,
                installed: std::path::PathBuf::from(&app_path),
                effects: &effects,
                public_key: update::ED_PUBLIC_KEY,
                enabled: enabled(),
                launch: kasa_socket::app_restart::launch_command(&app_path).map_err(|e| e.to_string())?,
                exit_timeout_s: 60,
                boot_timeout_s: 90,
            };
            update::drive(&dev, &job, &facts_now, &kasa_socket::board::now_ms)
        })();
        *driving().lock().unwrap() = None;
        match result {
            Ok(Step::Armed(_)) => {
                let _ = proxy.send_event(UserEvent::UpdateExit(job.job_id.clone()));
            }
            Ok(Step::Waiting(why)) => eprintln!("[app-update] {} waiting: {why}", job.job_id),
            Err(why) => eprintln!("[app-update] {}: {why}", job.job_id),
        }
    });
}

/// 부팅 때 한 번 — 이 기기의 업데이트가 갈아 끼운 뒤 새 판을 기다리고 있었으면 도착과 빌드 표식을 적는다.
pub(crate) fn mark_update_booted() {
    let (Ok(dir), Ok(machine)) = (update::jobs_dir(), kasa_mcp::board_service::local_id()) else {
        return;
    };
    if let Some((job, end)) = update::mark_booted(&dir, &machine, std::process::id(), &kasa_mcp::machines::build_id(), kasa_socket::board::now_ms()) {
        eprintln!("[app-update] booted for job {job}: {}", end.word());
    }
}
