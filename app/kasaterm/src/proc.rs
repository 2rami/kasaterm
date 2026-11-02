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
    APPS.get_or_init(scan_open_with_apps)
}

static APPS: std::sync::OnceLock<Vec<(String, String)>> = std::sync::OnceLock::new();

/// 렌더 경로 전용 — 아직 안 훑었으면 `None`. 첫 스캔이 Spotlight(`mdfind`) 를
/// fork 하므로 프레임 안에서 기다리면 창이 통째로 멈춘다(실측: Info 패널 첫
/// 프레임 104ms 중 dir 섹션이 104.45ms). 그릴 땐 준비된 것만 쓰고, 준비는
/// `warm_open_with_apps` 가 백그라운드에서 시킨다.
pub(crate) fn open_with_apps_ready() -> Option<&'static [(String, String)]> {
    APPS.get().map(Vec::as_slice)
}

/// 백그라운드로 미리 훑는다. 완료되면 `done` 을 부른다(재그리기 트리거용).
/// 이미 준비됐거나 훑는 중이면 아무 것도 하지 않는다.
pub(crate) fn warm_open_with_apps(done: impl FnOnce() + Send + 'static) {
    use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
    static STARTED: AtomicBool = AtomicBool::new(false);
    if APPS.get().is_some() || STARTED.swap(true, Relaxed) {
        return;
    }
    std::thread::spawn(move || {
        let _ = open_with_apps();
        done();
    });
}

#[cfg(target_os = "macos")]
fn scan_open_with_apps() -> Vec<(String, String)> {
    // 순서가 곧 메뉴 순서다. 사용자가 안 쓰는 에디터(Cursor·Antigravity·Obsidian)는
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
    let home = kasa_socket::home_var().unwrap_or_default();
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

#[cfg(test)]
mod tests {
    /// 숨김 없이 띄우는 자리가 하나라도 남으면 Windows 에서 그게 불릴 때마다 콘솔 창이 번쩍인다 —
    /// 2026-11-03 기기 명부의 ssh·sh 가 켤 때마다 세 번, 끊긴 터널을 다시 열 때마다 또 띄웠다.
    /// 앱은 `command`, 그 HTTP 호스트(kasa-mcp)는 `no_window_command` 로만 띄운다. 시험 모듈의
    /// 자식은 사람 화면에 안 남으니 따지지 않는다.
    #[test]
    fn every_spawn_hides_its_console_window() {
        let app = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut found = Vec::new();
        let mut dirs = vec![app.join("src"), app.join("../../crates/kasa-mcp/src")];
        while let Some(dir) = dirs.pop() {
            for path in std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()) {
                let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
                let test_file = stem == "tests" || stem == "test_support" || stem.ends_with("_tests");
                if path.is_dir() {
                    dirs.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") && !test_file {
                    let text = std::fs::read_to_string(&path).unwrap();
                    found.extend(unhidden_spawns(&text).into_iter().map(|n| format!("{}:{n}", path.display())));
                }
            }
        }
        assert!(found.is_empty(), "콘솔 창을 숨기지 않고 띄운다 — proc::command·no_window_command 로:\n{}", found.join("\n"));
    }

    #[test]
    fn the_scan_skips_test_modules_but_not_what_follows_them() {
        let src = "fn d() {\n    let mut c = std::process::Command::new(\"git\");\n    c.creation_flags(0x0800_0000);\n}\n\
                   fn b() { crate::proc::command(\"ssh\"); MyCommand::new(1); }\n\
                   #[cfg(all(test, unix))]\nmod tests {\n    fn t() { std::process::Command::new(\"sh\"); }\n}\n\
                   fn a() { std::process::Command::new(\"ssh\"); }\n\
                   fn c() { tokio::process::Command::new(\"git\"); }\n";
        assert_eq!(unhidden_spawns(src), vec![10, 11]);
    }

    /// 시험 모듈 밖에서 `Command::new(` 를 부르고 다섯 줄 안에 `creation_flags` 를 걸지 않는 줄 번호.
    /// 다섯 줄은 숨김 도우미가 플래그를 거는 데 쓰는 길이다.
    fn unhidden_spawns(text: &str) -> Vec<usize> {
        let lines: Vec<&str> = text.lines().collect();
        let mut out = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            if let Some(end) = test_module_end(&lines, i) {
                i = end + 1;
                continue;
            }
            let line = lines[i];
            let spawns = !line.trim_start().starts_with("//")
                && line.match_indices("Command::new(").any(|(at, _)| !line[..at].ends_with(|c: char| c.is_alphanumeric() || c == '_'));
            if spawns && !lines[i..lines.len().min(i + 6)].iter().any(|l| l.contains("creation_flags")) {
                out.push(i + 1);
            }
            i += 1;
        }
        out
    }

    /// `i` 줄이 시험 모듈을 여는 `#[cfg(…test…)]` 면 그 모듈을 닫는 줄. 이 저장소의 모듈은 여는 줄과
    /// 같은 들여쓰기의 `}` 로 닫힌다.
    fn test_module_end(lines: &[&str], i: usize) -> Option<usize> {
        let attr = lines[i].trim_start();
        if !attr.starts_with("#[cfg(") || !attr.contains("test") || attr.contains("not(test") {
            return None;
        }
        let indent = &lines[i][..lines[i].len() - attr.len()];
        let mut j = i + 1;
        while lines.get(j)?.trim_start().starts_with("#[") {
            j += 1;
        }
        let decl = lines[j].trim_start();
        let decl = decl.strip_prefix("pub(crate) ").or_else(|| decl.strip_prefix("pub ")).unwrap_or(decl);
        if !decl.starts_with("mod ") || !decl.trim_end().ends_with('{') {
            return None;
        }
        let close = format!("{indent}}}");
        (j + 1..lines.len()).find(|&k| lines[k].trim_end() == close)
    }
}
