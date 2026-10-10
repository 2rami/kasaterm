//! 칸이 뜰 자리 — 시작 cwd·기본 셸·훅을 돌릴 셸, 그리고 이 인스턴스의 소켓 경로와 HTTP 포트.
use super::*;

/// Where the first shell of a fresh session starts. No spawning pane exists
/// yet, so the `"last"` mode falls back to home — same as every terminal's
/// very first window.
pub(crate) fn resolve_initial_cwd() -> Option<String> {
    resolve_spawn_cwd(None)
}

/// Anything unresolved falls through to home, then the process cwd.
pub(crate) fn resolve_spawn_cwd(prev: Option<std::path::PathBuf>) -> Option<String> {
    let mode = socket::read_default_cwd_mode();
    if mode == "last" {
        if let Some(p) = prev.and_then(|p| p.to_str().map(String::from)) {
            return Some(p);
        }
    }
    if let Ok(dir) = std::env::var("KASATERM_CWD") {
        if !dir.is_empty() {
            return Some(dir);
        }
    }
    match mode.as_str() {
        // "last" with no prev (first boot) falls through to home.
        "last" | "home" => {}
        path => {
            let expanded = match path.strip_prefix("~/") {
                Some(rest) => match kasa_socket::home_dir() {
                    Some(home) => format!("{}/{rest}", home.display()),
                    None => path.to_string(),
                },
                None => path.to_string(),
            };
            if std::path::Path::new(&expanded).is_dir() {
                return Some(expanded);
            }
        }
    }
    if let Some(home) = kasa_socket::home_dir() {
        return Some(home.to_string_lossy().into_owned());
    }
    std::env::current_dir()
        .ok()
        .and_then(|p| p.to_str().map(String::from))
}

pub(crate) fn resolve_default_shell() -> Option<String> {
    if let Ok(s) = std::env::var("KASATERM_SHELL") {
        if !s.is_empty() {
            return Some(s);
        }
    }
    // User's explicit choice in the settings screen wins over `$SHELL` so it
    // overrides the inherited login shell, but stays below the env launch
    // override above.
    if let Some(s) = socket::read_default_shell() {
        return Some(s);
    }
    if let Ok(s) = std::env::var("SHELL") {
        if !s.is_empty() {
            return Some(s);
        }
    }
    #[cfg(windows)]
    {
        // PowerShell 7 (pwsh) preferred, then the OS-bundled Windows
        // PowerShell (always present), then Git Bash. The settings-screen /
        // env overrides above still win, so this is only the out-of-box pick.
        let pwsh7 = r"C:\Program Files\PowerShell\7\pwsh.exe";
        if std::path::Path::new(pwsh7).is_file() {
            return Some(pwsh7.to_string());
        }
        if let Some(bash) = git_bash_path() {
            return Some(bash);
        }
        return Some("powershell.exe".to_string());
    }
    #[allow(unreachable_code)]
    None
}

/// First installed Git Bash, if any. Git for Windows ships a Unix-like
/// shell that's the closest match to the macOS zsh workflow kasaterm was
/// built around (so `ls`/`grep`/`claude` etc. just work).
#[cfg(windows)]
pub(crate) fn git_bash_path() -> Option<String> {
    for candidate in &[
        r"C:\Program Files\Git\bin\bash.exe",
        r"C:\Program Files\Git\usr\bin\bash.exe",
        r"C:\Program Files (x86)\Git\bin\bash.exe",
    ] {
        if std::path::Path::new(candidate).is_file() {
            return Some((*candidate).to_string());
        }
    }
    None
}

pub(crate) fn hook_shell_program() -> String {
    #[cfg(windows)]
    {
        if let Some(bash) = git_bash_path() {
            let sh = std::path::Path::new(&bash).with_file_name("sh.exe");
            if sh.is_file() {
                return sh.to_string_lossy().replace('\\', "/");
            }
        }
        return "sh".to_string();
    }
    #[cfg(not(windows))]
    {
        "sh".to_string()
    }
}

/// Shells offered by the sidebar "+" picker: `(label, icon_svg name,
/// shell command)`. Windows 전용 — 설치된 셸(PowerShell/CMD/Git Bash/WSL)만 나열,
/// 없는 셸은 조용히 빠진다. macOS/Linux 는 빈 목록 → "+" 가 즉시 기본 셸 스폰.
pub(crate) fn available_shells() -> Vec<(&'static str, &'static str, String)> {
    #[allow(unused_mut)]
    let mut out: Vec<(&'static str, &'static str, String)> = Vec::new();
    #[cfg(windows)]
    {
        let pwsh7 = r"C:\Program Files\PowerShell\7\pwsh.exe";
        if std::path::Path::new(pwsh7).is_file() {
            out.push(("PowerShell 7", "sh/pwsh", pwsh7.to_string()));
        }
        // Windows PowerShell ships with the OS — always present.
        out.push((
            "Windows PowerShell",
            "sh/winps",
            "powershell.exe".to_string(),
        ));
        // cmd 엔 브랜드 마크가 없다. 색을 박은 SVG 를 두면 밝은/어두운 테마 중
        // 한쪽에서 반드시 묻히므로, 테마색을 따라가는 기본 글리프로 둔다.
        out.push(("Command Prompt", "terminal", "cmd.exe".to_string()));
        if let Some(bash) = git_bash_path() {
            out.push(("Git Bash", "sh/gitbash", bash));
        }
        if std::path::Path::new(r"C:\Windows\System32\wsl.exe").is_file() {
            out.push(("WSL", "sh/wsl", "wsl.exe".to_string()));
        }
    }
    out
}

/// Decide where the agent-socket should live. Honors caller-supplied
/// overrides first (`KASATERM_SOCKET_PATH`, then the cmux convention),
/// and falls back to a per-pid socket under the system temp dir. Used
/// in two places — the early env-var seed in `start_pty` so the very
/// first shell sees a stable value, and the actual server bind in
/// `start_socket_with` — and must return the same path in both.
/// Path of the file the http-serving process writes its ACTUAL bound MCP port
/// into, so pane hooks poll the right port even when the preferred one was
/// taken and `spawn_http_server` fell back to an OS-assigned one.
///
/// Derived from the socket path by swapping the extension, so it is 1:1 with
/// the socket: `kasaterm-25057.sock` → `kasaterm-25057.mcp_port`. That pairing
/// is the whole point. The old shape was `<socket's dir>/mcp_port` — one file
/// per *directory*, while sockets are per *pid* — so every concurrent instance
/// wrote its port over the same file, and pane hooks reading it got whichever
/// instance booted last. A test instance launched from a pane inherits the main
/// app's `KASATERM_SOCKET_PATH`, so it landed its port in the main app's
/// directory and silently redirected the main app's own panes at itself.
pub(crate) fn mcp_port_file_for(sock: &str) -> std::path::PathBuf {
    std::path::Path::new(sock).with_extension("mcp_port")
}

/// The socket path this process would use if it has not bound yet — env first
/// (the value our own `start_socket_with` exported, or one inherited from a
/// parent pane), else the daemon default. Read-only: unlike
/// `resolve_kasaterm_socket_path` it never probes or decides to isolate.
pub(crate) fn socket_path_hint() -> String {
    std::env::var("KASATERM_SOCKET_PATH")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            // 홈은 `home_dir()`(HOME → USERPROFILE) 로 읽는다 — Windows 엔 HOME 이
            // 없어 `var("HOME").unwrap_or_default()` 가 빈 문자열을 주고, 그러면
            // `/.config/...` 라는 드라이브 루트 경로가 되어 조용히 빗나간다.
            kasa_socket::home_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join(".config/kasaterm/daemon.sock")
                .to_string_lossy()
                .into_owned()
        })
}

/// Port the panel webviews should poll. This process's own bound port
/// (`KASASPACE_MCP_PORT`, set by `start_socket_with` right after the server
/// binds) wins — env is per-process, so each instance's panels always reach
/// their own server. The port file is only a fallback for callers without the
/// env; it is keyed to the socket path now, so it names one instance rather
/// than "whoever booted last".
///
/// The last resort is 8765, which is *someone else's* server whenever this
/// process failed to bind it — reaching it means panels quietly show another
/// instance's panes. Kept because a panel with no port at all just hangs, but
/// it is a wrong answer, not a neutral one.
pub(crate) fn mcp_panel_port() -> String {
    mcp_panel_port_certain().0
}

/// 포트와 **그 포트가 이 인스턴스의 것이 확실한가**.
///
/// env 나 이 인스턴스의 포트 파일에서 나왔으면 확실하다. 둘 다 없어 `8765` 로
/// 떨어지면 **남의 프로세스**일 수 있다 — 멀티 인스턴스에서 그 번호는 먼저 뜬 앱
/// 것이고, 설정 화면은 파일을 쓰므로 남의 설정을 고치게 된다.
///
/// 확실성을 따로 돌려주는 이유는 경고를 **필요할 때만** 띄우기 위해서다. 설정 창
/// 제목에 주소를 늘 박아 두면 평소엔 지저분하기만 하고(사용자 2026-08-25 「그거
/// 주소안나오게해봐」), 정작 위험한 순간에도 늘 있던 글자라 눈에 안 띈다.
pub(crate) fn mcp_panel_port_certain() -> (String, bool) {
    let trimmed_nonempty = |s: String| {
        let s = s.trim().to_string();
        (!s.is_empty()).then_some(s)
    };
    let known = std::env::var("KASASPACE_MCP_PORT")
        .ok()
        .and_then(trimmed_nonempty)
        .or_else(|| {
            std::fs::read_to_string(mcp_port_file_for(&socket_path_hint()))
                .ok()
                .and_then(trimmed_nonempty)
        });
    match known {
        Some(p) => (p, true),
        None => ("8765".to_string(), false),
    }
}

pub(crate) fn resolve_kasaterm_socket_path() -> String {
    let own = || {
        format!(
            "{}/kasaterm-{}.sock",
            std::env::temp_dir().to_string_lossy(),
            std::process::id()
        )
    };
    let inherited = std::env::var("KASATERM_SOCKET_PATH")
        .or_else(|_| std::env::var("CMUX_SOCKET_PATH"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    match inherited {
        // 부모 pane이 물려준 소켓 경로가 *이미 살아있는 다른 인스턴스*를 가리키면
        // (우리가 claude pane 안에서 `cargo run`으로 띄워진 자식인 경우), 그 경로에
        // bind 하면 Server::bind 가 기존 소켓 파일을 지우고 덮어써 메인 앱 소켓을
        // 탈취한다 — 그 결과 모든 pane 의 kasaterm-cli 가 빈 자식 인스턴스로 붙어
        // board 가 텅 빈다. connect 가 성공하면(=살아있는 리스너) 탈취하지 말고 우리
        // PID 경로로 격리한다. start_socket_with 가 resolved 경로를 자식 pane 에 다시
        // export 하므로 자식 창 pane 들도 자동으로 우리 소켓을 따라온다.
        Some(p) => {
            #[cfg(unix)]
            if std::os::unix::net::UnixStream::connect(&p).is_ok() {
                return own();
            }
            p
        }
        None => own(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_file_is_paired_with_its_socket_not_its_directory() {
        // 실사고: 두 인스턴스의 소켓은 PID 로 갈렸는데 포트 파일은 "소켓의 디렉터리 +
        // mcp_port" 라 한 파일이었다. pane 에서 띄운 테스트 인스턴스가 부모 앱의
        // TMPDIR 에 자기 포트를 덮어써, 부모 pane 의 hook 들이 테스트 서버로 갔다.
        let a = mcp_port_file_for("/tmp/kasaterm-25057.sock");
        let b = mcp_port_file_for("/tmp/kasaterm-82694.sock");
        assert_ne!(
            a, b,
            "같은 디렉터리의 두 인스턴스가 같은 포트 파일을 쓰면 안 된다"
        );
        assert_eq!(a, std::path::PathBuf::from("/tmp/kasaterm-25057.mcp_port"));
        // 셸 hook 이 ${sock%.sock}.mcp_port 로 유도하는 것과 같은 결과여야 한다 —
        // 한쪽만 바뀌면 hook 이 영영 빈 파일을 읽고 8765(남의 서버)로 폴백한다.
        for sock in [
            "/tmp/kasaterm-1.sock",
            "/home/u/.config/kasaterm/daemon.sock",
        ] {
            let shell = format!("{}.mcp_port", sock.trim_end_matches(".sock"));
            assert_eq!(mcp_port_file_for(sock), std::path::PathBuf::from(shell));
        }
    }
}
