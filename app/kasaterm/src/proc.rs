//! Spawning external programs from kasaterm's GUI process. On Windows a GUI
//! (non-console) process flashes a fresh console window every time it spawns a
//! console program (git, claude, python). A polled spawn flashes it on a loop —
//! the "검은창 자꾸 생겼다가 사라져" symptom. CREATE_NO_WINDOW suppresses it.
//! Route every external spawn through here so no site reintroduces the flash.
//! No-op on other platforms.

use std::ffi::OsStr;
use std::process::Command;

pub(crate) fn command<S: AsRef<OsStr>>(program: S) -> Command {
    // `mut` 는 아래 windows 블록이 쓴다 — 떼면 그쪽 빌드가 깨지므로
    // 다른 플랫폼에서만 나는 경고를 끈다.
    #[allow(unused_mut)]
    let mut c = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

/// 파일트리 우클릭 "…에서 열기" 후보. `(표시 이름, 실행 대상)` 이고, **설치된
/// 것만** 담는다 — 없는 앱을 메뉴에 띄우면 클릭이 조용히 실패한다.
///
/// 판정은 CLI 이름이 아니라 실제 설치물(macOS 는 `.app` 번들, Windows 는 exe)로
/// 한다. 이 기기의 `/usr/local/bin/code` 는 Cursor 번들 안을 가리키는 심링크였다
/// — `code` 가 있다고 "VS Code 설치됨"으로 읽으면 없는 앱을 메뉴에 올리게 된다.
///
/// 결과는 프로세스당 한 번만 훑어 캐시한다. 우클릭할 때마다 파일시스템을 뒤지면
/// 메뉴가 뜨는 데 체감될 만큼 늦다.
pub(crate) fn open_with_apps() -> &'static [(String, String)] {
    static APPS: std::sync::OnceLock<Vec<(String, String)>> = std::sync::OnceLock::new();
    APPS.get_or_init(scan_open_with_apps)
}

#[cfg(target_os = "macos")]
fn scan_open_with_apps() -> Vec<(String, String)> {
    // 순서가 곧 메뉴 순서다. 거노가 안 쓰는 에디터(Cursor·Antigravity·Obsidian)는
    // 뺐다 — 설치돼 있다고 다 올리면 정작 쓰는 항목이 목록에 파묻힌다.
    const CANDIDATES: &[&str] = &[
        "Visual Studio Code",
        "Zed",
        "Windsurf",
        "Sublime Text",
        "Nova",
        "IntelliJ IDEA",
        "WebStorm",
        "PyCharm",
        "Xcode",
    ];
    let home = std::env::var("HOME").unwrap_or_default();
    let roots = [
        "/Applications".to_string(),
        format!("{home}/Applications"),
    ];
    let mut out: Vec<(String, String)> = Vec::new();
    for name in CANDIDATES {
        for root in &roots {
            let bundle = format!("{root}/{name}.app");
            if std::path::Path::new(&bundle).exists() {
                out.push((name.to_string(), bundle));
                break; // 같은 앱이 두 위치에 있으면 먼저 찾은 쪽만.
            }
        }
    }
    // 표준 위치에 없는 건 Spotlight 에 한 번 더 묻는다. 앱이 꼭 `/Applications`
    // 에 있으란 법이 없어서다 — 이 기기의 VS Code 는 `~/Desktop/momewomo` 에
    // 있었고, 그래서 "제일 중요한 항목이 메뉴에 없다"는 사고가 났다.
    // 질의는 미발견 후보를 묶어 **한 번만** 던진다(앱마다 fork 하면 우클릭이
    // 체감될 만큼 늦다 — 실측 1회 47ms).
    let missing: Vec<&str> = CANDIDATES
        .iter()
        .copied()
        .filter(|n| !out.iter().any(|(have, _)| have == n))
        .collect();
    if !missing.is_empty() {
        let clause = missing
            .iter()
            .map(|n| format!("kMDItemFSName == '{n}.app'"))
            .collect::<Vec<_>>()
            .join(" || ");
        let q = format!("kMDItemContentType == 'com.apple.application-bundle' && ({clause})");
        if let Ok(o) = command("mdfind").arg(&q).output() {
            for line in String::from_utf8_lossy(&o.stdout).lines() {
                let path = line.trim();
                // 다른 번들 안에 끼어 있는 사본은 앱이 아니라 부품이다.
                if path.is_empty() || path.trim_end_matches(".app").contains(".app/") {
                    continue;
                }
                let Some(name) = std::path::Path::new(path)
                    .file_stem()
                    .and_then(|s| s.to_str())
                else {
                    continue;
                };
                // Spotlight 는 같은 경로를 두 번 주기도 한다.
                if missing.contains(&name) && !out.iter().any(|(have, _)| have == name) {
                    out.push((name.to_string(), path.to_string()));
                }
            }
        }
        // mdfind 결과는 CANDIDATES 순서를 모르므로 여기서 되돌린다.
        out.sort_by_key(|(n, _)| CANDIDATES.iter().position(|c| c == n).unwrap_or(usize::MAX));
    }
    out
}

#[cfg(target_os = "windows")]
fn scan_open_with_apps() -> Vec<(String, String)> {
    let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
    let pf = std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into());
    let candidates: [(&str, Vec<String>); 4] = [
        (
            "Visual Studio Code",
            vec![
                format!(r"{local}\Programs\Microsoft VS Code\Code.exe"),
                format!(r"{pf}\Microsoft VS Code\Code.exe"),
            ],
        ),
        ("Zed", vec![format!(r"{local}\Zed\Zed.exe")]),
        ("Sublime Text", vec![format!(r"{pf}\Sublime Text\sublime_text.exe")]),
        ("Notepad++", vec![format!(r"{pf}\Notepad++\notepad++.exe")]),
    ];
    let mut out = Vec::new();
    for (name, paths) in candidates {
        if let Some(p) = paths.into_iter().find(|p| std::path::Path::new(p).exists()) {
            out.push((name.to_string(), p));
        }
    }
    out
}

#[cfg(all(unix, not(target_os = "macos")))]
fn scan_open_with_apps() -> Vec<(String, String)> {
    // PATH 를 직접 훑는다 — `which` 를 fork 하는 것보다 싸고, 셸이 없어도 된다.
    const CANDIDATES: &[(&str, &str)] = &[
        ("Visual Studio Code", "code"),
        ("Zed", "zed"),
        ("Sublime Text", "subl"),
    ];
    let path = std::env::var("PATH").unwrap_or_default();
    let dirs: Vec<&str> = path.split(':').collect();
    let mut out = Vec::new();
    for (label, bin) in CANDIDATES {
        if let Some(full) = dirs
            .iter()
            .map(|d| format!("{d}/{bin}"))
            .find(|p| std::path::Path::new(p).exists())
        {
            out.push((label.to_string(), full));
        }
    }
    out
}

/// `open_with_apps()` 가 준 대상으로 경로를 연다.
pub(crate) fn open_path_with(target: &str, path: &std::path::Path) {
    #[cfg(target_os = "macos")]
    let _ = command("open").arg("-a").arg(target).arg(path).spawn();
    #[cfg(not(target_os = "macos"))]
    let _ = command(target).arg(path).spawn();
}

/// OS 기본 연결 프로그램으로 연다.
pub(crate) fn open_path_default(path: &std::path::Path) {
    #[cfg(target_os = "macos")]
    let _ = command("open").arg(path).spawn();
    #[cfg(target_os = "windows")]
    let _ = command("cmd").arg("/C").arg("start").arg("").arg(path).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = command("xdg-open").arg(path).spawn();
}
