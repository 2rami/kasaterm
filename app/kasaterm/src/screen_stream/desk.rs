//! 따로 까는 보기 앱(KasaDesk, github.com/2rami/kasadesk)으로 화면 보기를 연다 — 이 앱이 꺼지거나 다시 떠도 화면이 이어진다
//! (`docs/screen-service-split.md` 「보기 앱」). KasaDesk 가 안 깔린 기기는 화면 탭으로 물러선다.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};

use kasa_socket::app_update::{ed25519_ok, parse_feed, Effects as _, FeedItem, SystemEffects};

/// 보기 앱 공식 피드와 서명 공개키 — github.com/2rami/kasadesk `src/update.rs` 와 같다(카사텀 피드 키와 다르다).
const DESK_FEED: &str = if cfg!(windows) {
    "https://2rami.github.io/kasadesk/appcast-win.xml"
} else {
    "https://2rami.github.io/kasadesk/appcast.xml"
};
const DESK_ED_KEY: &str = "LEspOJR62N02A/l32YWcZYT24qrbP8urZxOTAyWeQ/I=";
const DESK_RELEASES: &str = "https://github.com/2rami/kasadesk/releases/download/";
#[cfg(target_os = "macos")]
const DESK_TEAM: &str = "L366799VND";
const MAX_FEED: u64 = 256 * 1024;
const MAX_PACKAGE: u64 = 300 * 1024 * 1024;
static INSTALLING: AtomicBool = AtomicBool::new(false);

/// 보기 앱 자리 — 맥 `/Applications`·`~/Applications` 의 `KasaDesk.app`, Windows `Program Files\KasaDesk\kasadesk.exe`.
/// `KASATERM_DESK` 가 있으면 그것만 본다. 검증 실행은 `KASATERM_DESK` 없이는 보기 앱을 안 띄운다 — 깔린 보기 앱은 사람 것이라
/// 떠 있으면 그쪽에 넘어가 버린다.
pub(crate) fn desk_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("KASATERM_DESK").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(path)).filter(|p| launchable(p));
    }
    if crate::verification_run() {
        return None;
    }
    installed().into_iter().find(|p| launchable(p))
}

fn launchable(path: &std::path::Path) -> bool {
    if path.extension().is_some_and(|e| e == "app") { path.join("Contents/MacOS/kasadesk").is_file() } else { path.is_file() }
}

fn installed() -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let mut out = vec![PathBuf::from("/Applications/KasaDesk.app")];
        out.extend(std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Applications/KasaDesk.app")));
        out
    }
    #[cfg(windows)]
    {
        ["ProgramW6432", "ProgramFiles"].iter().filter_map(std::env::var_os).map(|dir| PathBuf::from(dir).join("KasaDesk").join("kasadesk.exe")).collect()
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        Vec::new()
    }
}

/// 그 기기를 보기 앱으로 연다. 보기 앱이 이미 떠 있으면 새로 띄운 것이 그쪽에 넘기고 끝난다. 보기 앱이 없으면 Err.
pub(crate) fn open_in_desk(label: &str) -> Result<(), String> {
    let path = desk_path().ok_or("보기 앱이 없다")?;
    let port = std::env::var("KASASPACE_MCP_PORT").ok().filter(|p| p.trim().parse::<u16>().is_ok()).unwrap_or_else(|| "8765".into());
    let app = format!("http://127.0.0.1:{}", port.trim());
    let args = ["--machine", label, "--app", &app];
    #[cfg(target_os = "macos")]
    if path.extension().is_some_and(|e| e == "app") && !crate::verification_run() {
        // LaunchServices 로 띄워 이 앱의 자식이 아니게 — 이 앱이 다시 떠도 산다. `-n` 이 없으면 떠 있는 보기 앱이 인자를 못 받는다.
        return crate::proc::command("/usr/bin/open")
            .args(["-n", "-a"])
            .arg(&path)
            .arg("--args")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(drop)
            .map_err(|e| format!("open: {e}"));
    }
    let exe = if path.extension().is_some_and(|e| e == "app") { path.join("Contents/MacOS/kasadesk") } else { path };
    let spawn = |detach_job: bool| {
        let mut cmd = crate::proc::command(&exe);
        cmd.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // 이 앱 프로세스 묶음 밖으로 — 묶음째 가는 신호에 같이 죽지 않게.
            cmd.process_group(0);
            let _ = detach_job;
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const DETACHED_PROCESS: u32 = 0x0000_0008;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
            // 이 앱이 작업 개체 안에서 돌면(닫히면 다 죽이는 작업) 보기 앱이 같이 죽는다 — 빠져나가게. 허락 안 된 작업이면 그대로.
            // 창 없는 콘솔(`proc::command` 의 CREATE_NO_WINDOW) 대신 콘솔 없이 뗀다 — 보기 앱은 창 앱이라 콘솔이 안 뜬다.
            cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | if detach_job { CREATE_BREAKAWAY_FROM_JOB } else { 0 });
        }
        cmd.spawn()
    };
    spawn(true).or_else(|_| spawn(false)).map(drop).map_err(|e| format!("{}: {e}", exe.display()))
}

/// 이 실행이 사람 기기에 보기 앱을 깔아도 되는가 — 검증 실행·`KASATERM_DESK` 를 준 리그는 깔지 않는다.
pub(crate) fn can_install() -> bool {
    cfg!(any(target_os = "macos", windows)) && !crate::verification_run() && std::env::var_os("KASATERM_DESK").is_none()
}

/// 공식 피드의 최신 보기 앱을 받아 서명을 확인하고 깐다 — 맥은 `~/Applications/KasaDesk.app`(공증·팀 확인 뒤), Windows 는
/// MSI(관리자 확인 창이 뜬다). 내려받기·설치라 오래 걸리니 일꾼 스레드에서 부른다. 이미 까는 중이면 Err.
pub(crate) fn install() -> Result<String, String> {
    if INSTALLING.swap(true, Ordering::SeqCst) {
        return Err("이미 깔고 있어요".into());
    }
    struct Done;
    impl Drop for Done {
        fn drop(&mut self) {
            INSTALLING.store(false, Ordering::SeqCst);
        }
    }
    let _done = Done;
    let fx = SystemEffects;
    let item = parse_feed(&fx.feed(DESK_FEED, MAX_FEED)?).ok_or("KasaDesk 피드를 못 읽었어요")?;
    check_item(&item)?;
    let dir = std::env::temp_dir().join(format!("kasadesk-install-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let result = install_from(&fx, &item, &dir);
    let _ = std::fs::remove_dir_all(&dir);
    result.map(|()| item.version)
}

fn check_item(item: &FeedItem) -> Result<(), String> {
    if !item.url.starts_with(DESK_RELEASES) || item.length == 0 || item.length > MAX_PACKAGE {
        return Err(format!("KasaDesk 피드의 파일이 공식 릴리스가 아니에요({})", item.url));
    }
    Ok(())
}

fn install_from(fx: &SystemEffects, item: &FeedItem, dir: &std::path::Path) -> Result<(), String> {
    let package = dir.join(if cfg!(windows) { "KasaDesk.msi" } else { "KasaDesk.dmg" });
    fx.fetch(&item.url, MAX_PACKAGE, &package).map_err(|e| format!("받기 실패 — {e:?}"))?;
    let bytes = std::fs::read(&package).map_err(|e| e.to_string())?;
    if bytes.len() as u64 != item.length || !ed25519_ok(DESK_ED_KEY, &item.signature, &bytes) {
        return Err("받은 파일의 크기나 서명이 피드와 달라요".into());
    }
    #[cfg(target_os = "macos")]
    {
        let mount = dir.join("mnt");
        std::fs::create_dir_all(&mount).map_err(|e| e.to_string())?;
        fx.mount(&package, &mount)?;
        let copied = copy_app(fx, &mount.join("KasaDesk.app"));
        fx.unmount(&mount);
        copied
    }
    #[cfg(windows)]
    {
        let msiexec = format!(r"{}\System32\msiexec.exe", std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into()));
        // 콘솔은 숨기되(`proc::command`) msiexec 자기 진행 창(/passive)과 관리자 확인 창은 그대로 뜬다.
        let status = crate::proc::command(msiexec)
            .args(["/i"])
            .arg(&package)
            .args(["/passive", "/norestart"])
            .status()
            .map_err(|e| format!("msiexec: {e}"))?;
        match status.code() {
            Some(0) | Some(3010) => Ok(()),
            Some(1602) => Err("설치를 취소했어요".into()),
            code => Err(format!("설치기가 {code:?} 로 끝났어요")),
        }
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = (fx, package);
        Err("이 OS 용 KasaDesk 는 없어요".into())
    }
}

/// 마운트한 dmg 의 앱이 공증된 우리 팀 판인지 보고 `~/Applications` 에 갈아 끼운다(관리자 권한 없이 쓰는 자리 — Sparkle 도 거기서 바꾼다).
#[cfg(target_os = "macos")]
fn copy_app(fx: &SystemEffects, app: &std::path::Path) -> Result<(), String> {
    let id = fx.identity(app)?;
    if !(id.verified && id.notarized && id.team.as_deref() == Some(DESK_TEAM)) {
        return Err(format!("dmg 안 앱 서명이 맞지 않아요(팀 {:?}, 공증 {})", id.team, id.notarized));
    }
    let apps = PathBuf::from(std::env::var_os("HOME").ok_or("HOME 이 없어요")?).join("Applications");
    std::fs::create_dir_all(&apps).map_err(|e| e.to_string())?;
    let staging = apps.join(".KasaDesk.app.installing");
    let target = apps.join("KasaDesk.app");
    let _ = std::fs::remove_dir_all(&staging);
    fx.copy_bundle(app, &staging)?;
    if target.exists() {
        std::fs::remove_dir_all(&target).map_err(|e| format!("옛 KasaDesk 를 못 치웠어요: {e}"))?;
    }
    std::fs::rename(&staging, &target).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_official_release_files_are_installed() {
        let item = |url: &str, length| FeedItem { version: "0.1.0".into(), url: url.into(), length, signature: "x".into() };
        assert!(check_item(&item("https://github.com/2rami/kasadesk/releases/download/v0.1.0/KasaDesk-0.1.0-macos-arm64.dmg", 10)).is_ok());
        assert!(check_item(&item("https://example.com/KasaDesk.dmg", 10)).is_err());
        assert!(check_item(&item("https://github.com/2rami/kasadesk/releases/download/v0.1.0/x.dmg", 0)).is_err());
        assert!(check_item(&item("https://github.com/2rami/kasadesk/releases/download/v0.1.0/x.dmg", MAX_PACKAGE + 1)).is_err());
    }

    #[test]
    fn the_feed_and_key_match_the_desk_repository() {
        assert!(DESK_FEED.starts_with("https://2rami.github.io/kasadesk/appcast"));
        assert_eq!(DESK_ED_KEY.len(), 44);
    }
}
