//! Windows 새 판 — 피드를 보고, 사람이 카드에서 「받기」를 누르면 MSI 를 받아 EdDSA 서명을 확인하고, 「다시 켜기」면
//! 도우미에 넘겨 앱이 정상 종료한 뒤 `msiexec /passive` 로 설치하고 다시 띄운다. 피드·채널은 `win_sparkle.rs`, 서명 확인과
//! 갈아 끼우기 도우미는 원격 업데이트 창구(`kasa_socket::app_update`)와 같은 것을 쓴다.
//!
//! WinSparkle 을 걷은 까닭: 그 창은 OS 모양으로 고정이라 새 판 카드(`update_card.rs`)와 겉모습을 맞출 수 없었다.
//! 받은 MSI 는 확인을 마친 것만 `.msi` 이름을 갖는다 — 확인 전 파일(`.part`)은 다시 켜기에 쓰이지 않는다.

use crate::update_card::Action;

#[cfg(not(windows))]
pub(crate) fn available() -> bool {
    false
}

#[cfg(not(windows))]
pub(crate) fn tick() {}

/// 사람이 누른 「업데이트 확인」 — 업데이터가 없는 판은 부르는 쪽이 먼저 거른다.
#[cfg(not(windows))]
pub(crate) fn check_now() {}

#[cfg(not(windows))]
pub(crate) fn respond(_action: Action, _proxy: &winit::event_loop::EventLoopProxy<crate::UserEvent>) {}

/// 「건너뛰기」한 판 — 그 판이거나 더 옛 판이면 다시 알리지 않는다.
#[cfg(windows)]
const SKIPPED_KEY: &str = "update_notice_dismissed";

#[cfg(any(windows, test))]
fn should_offer(found: &str, skipped: Option<&str>) -> bool {
    match skipped {
        Some(closed) => closed != found && !crate::win_sparkle::version_newer(closed, found),
        None => true,
    }
}

/// 받은 파일 이름 — 확인 전은 `.part`, 확인을 마치면 `.msi`.
#[cfg(any(windows, test))]
fn file_names(version: &str) -> (String, String) {
    (format!("kasaterm-{version}.msi.part"), format!("kasaterm-{version}.msi"))
}

/// 도우미 작업 id — `app_restart::valid_job_id` 를 지키는 글자만.
#[cfg(any(windows, test))]
fn job_id(version: &str, now_ms: u64) -> String {
    let safe: String = version.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    format!("self-{safe}-{now_ms}")
}

#[cfg(windows)]
pub(crate) use imp::{available, check_now, init, respond, tick};

#[cfg(windows)]
mod imp {
    use super::*;
    use crate::update_card::{self, Stage};
    use kasa_socket::app_update::{self as update, FeedItem};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Mutex, OnceLock};

    static STARTED: AtomicBool = AtomicBool::new(false);
    /// 피드의 최신 항목 — 체커가 쓰고 카드의 「받기」가 읽는다.
    static LATEST: Mutex<Option<FeedItem>> = Mutex::new(None);
    /// 확인을 마친 받은 판 — (판, MSI, sha256 hex).
    static READY: Mutex<Option<(String, PathBuf, String)>> = Mutex::new(None);
    /// 받는 curl — 「취소」가 끈다.
    static DOWNLOAD: Mutex<Option<std::process::Child>> = Mutex::new(None);
    static CANCELLED: AtomicBool = AtomicBool::new(false);

    fn install_root() -> Option<String> {
        let exe = std::env::current_exe().ok()?;
        let root = exe.ancestors().nth(2)?.display().to_string();
        kasa_socket::app_restart::valid_app_path("windows", &root).then_some(root)
    }

    fn cache() -> Option<PathBuf> {
        crate::app_update::cache_dir().ok().map(|d| d.join("self"))
    }

    /// MSI 로 Program Files 에 설치한 판만 — 휴대용 zip 판이 MSI 를 돌리면 설치본이 하나 더 생긴다.
    pub(crate) fn available() -> bool {
        STARTED.load(Ordering::Acquire)
    }

    pub(crate) fn init() {
        if !crate::win_sparkle::may_start(crate::verification_run(), crate::lite_mode(), |key| std::env::var_os(key).is_some()) {
            return;
        }
        if install_root().is_none() || STARTED.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(dir) = cache() {
            mark_booted(&dir);
        }
        let channel = crate::win_sparkle::Channel::of(&crate::socket::read_settings());
        std::thread::Builder::new()
            .name("update-probe".into())
            .spawn(move || {
                std::thread::sleep(crate::win_sparkle::FIRST_PROBE);
                loop {
                    probe(channel.feed());
                    std::thread::sleep(channel.probe_every());
                }
            })
            .ok();
    }

    /// 지난 판이 띄운 도우미가 이 프로세스를 기다린다 — 떴다고 적어 주고, 갈아 끼운 판이면 「바뀌었어요」를 세운다.
    fn mark_booted(dir: &Path) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "events") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            if text.lines().any(|l| l.split_whitespace().nth(1) == Some("booted")) {
                continue;
            }
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            let _ = std::fs::OpenOptions::new().append(true).open(&path)
                .and_then(|mut f| std::io::Write::write_all(&mut f, format!("{now} booted {}\n", std::process::id()).as_bytes()));
            if text.lines().any(|l| l.split_whitespace().nth(1) == Some("swapped")) {
                update_card::post(Some(Stage::Installed { version: crate::version::CURRENT.to_string() }));
            }
        }
    }

    fn feed() -> &'static str {
        crate::win_sparkle::Channel::of(&crate::socket::read_settings()).feed()
    }

    /// 주기 확인 — 건너뛴 판은 거르고, 서 있는 카드(받는 중·다른 결정)를 덮지 않는다.
    fn probe(feed: &str) {
        let Some(item) = fetch(feed).and_then(|xml| update::parse_feed(&xml)) else { return };
        if !crate::win_sparkle::version_newer(&item.version, crate::version::CURRENT) {
            return;
        }
        let settings = crate::socket::read_settings();
        if !should_offer(&item.version, settings[SKIPPED_KEY].as_str()) || update_card::current().is_some() {
            return;
        }
        offer(item);
    }

    fn offer(item: FeedItem) {
        let version = item.version.clone();
        if let Ok(mut l) = LATEST.lock() {
            *l = Some(item);
        }
        let ready = READY.lock().ok().and_then(|r| r.clone()).is_some_and(|(v, msi, _)| v == version && msi.is_file());
        update_card::post(Some(if ready { Stage::Ready { version } } else { Stage::Found { version, note: None } }));
    }

    /// 사람이 누른 확인 — 건너뛴 판도 다시 보이고, 없으면 「최신」, 못 받으면 까닭을 남긴다.
    pub(crate) fn check_now() {
        if matches!(update_card::current(), Some(Stage::Downloading { .. } | Stage::Installing { .. })) {
            return;
        }
        update_card::post(Some(Stage::Checking));
        std::thread::Builder::new()
            .name("update-check".into())
            .spawn(|| match fetch(feed()).and_then(|xml| update::parse_feed(&xml)) {
                Some(item) if crate::win_sparkle::version_newer(&item.version, crate::version::CURRENT) => offer(item),
                Some(_) => update_card::post(Some(Stage::Latest)),
                None => update_card::post(Some(Stage::Failed { message: "새 판 목록을 받지 못했어요. 망을 확인해 주세요.".into() })),
            })
            .ok();
    }

    fn fetch(feed: &str) -> Option<String> {
        let out = crate::proc::command("curl.exe").args(["-fsSL", "--max-time", "15", feed]).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// 1초마다 받은 크기를 카드에 옮긴다 — curl 은 따로 진행을 안 알린다.
    pub(crate) fn tick() {
        static LAST: OnceLock<Mutex<std::time::Instant>> = OnceLock::new();
        let last = LAST.get_or_init(|| Mutex::new(std::time::Instant::now()));
        let Ok(mut at) = last.lock() else { return };
        if at.elapsed() < std::time::Duration::from_millis(250) {
            return;
        }
        *at = std::time::Instant::now();
        let Some(Stage::Downloading { version, total, .. }) = update_card::current() else { return };
        let Some(dir) = cache() else { return };
        let got = std::fs::metadata(dir.join(file_names(&version).0)).map(|m| m.len()).unwrap_or(0);
        if DOWNLOAD.lock().is_ok_and(|d| d.is_some()) {
            update_card::post(Some(Stage::Downloading { version, got, total }));
        }
    }

    pub(crate) fn respond(action: Action, proxy: &winit::event_loop::EventLoopProxy<crate::UserEvent>) {
        let version = match update_card::current() {
            Some(Stage::Found { version, .. } | Stage::Ready { version } | Stage::Downloading { version, .. }) => version,
            _ => {
                update_card::post(None);
                return;
            }
        };
        match action {
            Action::Download => download(version),
            Action::Skip => {
                let _ = crate::socket::write_settings_patch_atomic(&[(SKIPPED_KEY, serde_json::json!(version))]);
                update_card::post(None);
            }
            Action::Cancel => {
                CANCELLED.store(true, Ordering::Release);
                if let Some(mut child) = DOWNLOAD.lock().ok().and_then(|mut d| d.take()) {
                    let _ = child.kill();
                }
                update_card::post(None);
            }
            Action::Relaunch => relaunch(&version, proxy),
            _ => update_card::post(None),
        }
    }

    fn download(version: String) {
        let Some(item) = LATEST.lock().ok().and_then(|l| l.clone()).filter(|i| i.version == version) else {
            update_card::post(Some(Stage::Failed { message: "피드에서 그 판을 다시 찾지 못했어요.".into() }));
            return;
        };
        let Some(dir) = cache() else {
            update_card::post(Some(Stage::Failed { message: "받을 자리(LOCALAPPDATA)를 찾지 못했어요.".into() }));
            return;
        };
        CANCELLED.store(false, Ordering::Release);
        update_card::post(Some(Stage::Downloading { version: version.clone(), got: 0, total: item.length }));
        std::thread::Builder::new()
            .name("update-download".into())
            .spawn(move || {
                let outcome = fetch_and_verify(&dir, &item);
                if CANCELLED.load(Ordering::Acquire) {
                    return;
                }
                match outcome {
                    Ok((msi, sha)) => {
                        if let Ok(mut r) = READY.lock() {
                            *r = Some((item.version.clone(), msi, sha));
                        }
                        update_card::post(Some(Stage::Ready { version: item.version }));
                    }
                    Err(message) => update_card::post(Some(Stage::Failed { message })),
                }
            })
            .ok();
    }

    /// 받고 크기·EdDSA 서명을 확인한 뒤에만 `.msi` 로 옮긴다.
    fn fetch_and_verify(dir: &Path, item: &FeedItem) -> Result<(PathBuf, String), String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("받을 자리를 못 만들었어요: {e}"))?;
        let (part_name, msi_name) = file_names(&item.version);
        let (part, msi) = (dir.join(part_name), dir.join(msi_name));
        let _ = std::fs::remove_file(&part);
        let child = crate::proc::command("curl.exe")
            .args(["-fsSL", "--max-time", "1800", "-o"])
            .arg(&part)
            .arg(&item.url)
            .spawn()
            .map_err(|e| format!("받기를 시작하지 못했어요: {e}"))?;
        if let Ok(mut d) = DOWNLOAD.lock() {
            *d = Some(child);
        }
        let status = loop {
            let done = DOWNLOAD.lock().ok().and_then(|mut d| d.as_mut().map(|c| c.try_wait()));
            match done {
                Some(Ok(Some(status))) => break Some(status),
                Some(Ok(None)) => std::thread::sleep(std::time::Duration::from_millis(200)),
                _ => break None,
            }
        };
        if let Ok(mut d) = DOWNLOAD.lock() {
            d.take();
        }
        if !status.is_some_and(|s| s.success()) {
            let _ = std::fs::remove_file(&part);
            return Err("받다가 끊겼어요. 망을 확인하고 다시 받아 주세요.".into());
        }
        let bytes = std::fs::read(&part).map_err(|e| format!("받은 파일을 못 읽었어요: {e}"))?;
        if bytes.len() as u64 != item.length
            || !update::ed25519_ok(crate::win_sparkle::ED_PUBLIC_KEY, &item.signature, &bytes)
        {
            let _ = std::fs::remove_file(&part);
            return Err("받은 파일의 서명이 맞지 않아 설치하지 않았어요.".into());
        }
        std::fs::rename(&part, &msi).map_err(|e| format!("받은 파일을 옮기지 못했어요: {e}"))?;
        let sha = update::sha256_file(&msi)?;
        Ok((msi, sha.trim_start_matches("sha256:").to_string()))
    }

    /// 도우미를 띄우고 앱은 정상 종료(`UpdateExit`, 세션 저장)한다 — 도우미가 앱이 꺼지길 기다렸다 MSI 해시를 다시 재고 설치·다시 띄운다.
    fn relaunch(version: &str, proxy: &winit::event_loop::EventLoopProxy<crate::UserEvent>) {
        let ready = READY.lock().ok().and_then(|r| r.clone()).filter(|(v, msi, _)| v == version && msi.is_file());
        let (Some((_, msi, sha)), Some(root), Some(dir)) = (ready, install_root(), cache()) else {
            update_card::post(Some(Stage::Failed { message: "받은 판을 찾지 못했어요. 다시 받아 주세요.".into() }));
            return;
        };
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
        let spec = (|| -> Result<update::SwapSpec, String> {
            Ok(update::SwapSpec {
                dir: dir.clone(),
                job_id: job_id(version, now),
                old_pid: std::process::id(),
                installed: PathBuf::from(&root),
                launch: kasa_socket::app_restart::launch_command("windows", &root).map_err(|e| e.to_string())?,
                exit_timeout_s: 60,
                boot_timeout_s: 90,
                installer: Some(update::Installer { program: update::msiexec_path(), msi, sha256: sha, timeout_s: 900 }),
            })
        })();
        let started = spec.and_then(|spec| update::spawn_swap(&spec).map(|_| spec.job_id).map_err(|e| e.to_string()));
        match started {
            Ok(job) => {
                update_card::post(Some(Stage::Installing { version: version.to_string() }));
                let _ = proxy.send_event(crate::UserEvent::UpdateExit(job));
            }
            Err(why) => update_card::post(Some(Stage::Failed { message: format!("설치 도우미를 띄우지 못했어요: {why}") })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_skipped_version_stays_skipped_until_a_newer_one() {
        assert!(should_offer("0.2.18", None));
        assert!(!should_offer("0.2.18", Some("0.2.18")));
        assert!(!should_offer("0.2.17", Some("0.2.18")));
        assert!(should_offer("0.2.19", Some("0.2.18")));
    }

    #[test]
    fn only_a_verified_download_gets_the_installer_name_and_job_ids_stay_safe() {
        let (part, msi) = file_names("0.2.300");
        assert!(part.ends_with(".part") && msi.ends_with(".msi") && part.starts_with(&msi));
        let id = job_id("0.2.300", 1_794_000_000_000);
        assert!(kasa_socket::app_restart::valid_job_id(&id), "{id}");
        assert!(kasa_socket::app_restart::valid_job_id(&job_id("1.0.0-rc.1+x", 1)));
    }
}
