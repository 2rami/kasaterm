//! 부팅 처리 — `main()` 이 창을 띄우기 전에 부르는 것: 로그·패닉 기록, 물려받은 claude 표식
//! 지우기, 라이트·캡처·검증 실행 env, 엔진 호스트 정책, 자기설치, 팀 저장소 청소.

/// 종료 뒤 새 빌드를 스스로 설치하도록 도우미를 띄운다 — 껐다 켜면 최신이 되게.
///
/// 새로 구운 번들이 `dist/` 에 놓여도 도는 앱과는 무관해서, 재시작 스크립트를 따로
/// 돌리지 않으면 옛 바이너리가 계속 뜬다. 코드를 고치고 앱을 껐다 켠 사람에게
/// "그건 반영이 아니다" 를 매번 설명해야 했다 — 껐다 켜는 것이 곧 반영이어야 한다.
///
/// 설치를 지금 하지 않고 도우미에게 미루는 건, 우리 번들을 우리가 도는 중에 덮으면
/// 안 되기 때문이다: pane shim 이 번들 안 헬퍼들을 심링크로 물고 있어 `rm -rf` 가
/// 그걸 중간에 끊는다. 그래서 우리 pid 가 사라질 때까지 기다렸다가 복사한다.
///
/// 다시 띄우지는 않는다. 사람이 끄려고 끈 것일 수도 있는데 창이 혼자 되살아나면
/// 그게 더 놀랍다 — 다음에 켤 때 새것이면 충분하다.
///
/// 다음 셋을 다 만족할 때만 움직인다. 하나라도 어긋나면 조용히 아무것도 안 한다:
/// 지금 도는 것이 **그 설치본**일 것(개발 `cargo run` 이나 남의 위치 앱은 남의 것),
/// 빌드 트리의 번들이 실재할 것(배포된 머신엔 없다), 그리고 그게 **더 새것**일 것.
/// 종료할 때 설치가 **실제로 움직일 조건** — 화면의 「껐다 켜면 바뀝니다」 표시가
/// 이 답을 그대로 쓴다. 둘이 갈리면 화면이 거짓말을 한다: 표시는 떴는데 안 바뀌거나,
/// 안 떴는데 바뀌거나.
///
/// 반환은 (설치본 번들, 새로 구운 번들). `None` 이면 움직일 이유가 없다.
pub(crate) fn install_pending_paths() -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    #[cfg(target_os = "macos")]
    if crate::macos_sparkle::owns_installation() { return None; }
    let installed =
        std::path::PathBuf::from(kasa_socket::home_var().ok()?).join("Applications/kasaterm.app");
    let running = installed.join("Contents/MacOS/kasaterm");
    // 그 설치본으로 도는 앱에서만 뜻이 있다 — `cargo run` 개발 실행에서는 새로 구운
    // 쪽이 늘 더 새것이라 표시가 영구히 켜져 있게 된다.
    if std::env::current_exe().ok().as_deref() != Some(running.as_path()) {
        return None;
    }
    // 새로 구운 번들의 자리 — 구운 쪽이 남긴 표(`Resources/build-root`, build-app.sh)가
    // 먼저다. 컴파일 시점 경로(CARGO_MANIFEST_DIR)만 믿으면 임시 워크트리에서 구운
    // 판이 그 워크트리를 영영 바라봐, 워크트리를 걷은 뒤엔 자기설치가 다시는
    // 안 움직인다(2026-09-03 실측). 표가 없는 옛 번들은 컴파일 경로로 물러선다.
    let root = std::fs::read_to_string(installed.join("Contents/Resources/build-root"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let dist = root.join("dist/kasaterm.app");
    let fresh = dist.join("Contents/MacOS/kasaterm");
    let mtime = |p: &std::path::Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    (mtime(&fresh)? > mtime(&running)?).then_some((installed, dist))
}

/// 위 판정의 캐시판 — 상태줄은 프레임마다 도는 자리라 stat 두 번도 매번은 아깝다.
pub(crate) fn install_pending() -> bool {
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};
    static CACHE: OnceLock<Mutex<Option<(Instant, bool)>>> = OnceLock::new();
    let cell = CACHE.get_or_init(|| Mutex::new(None));
    let Ok(mut g) = cell.lock() else { return false };
    if let Some((at, v)) = *g {
        if at.elapsed() < Duration::from_secs(5) {
            return v;
        }
    }
    let v = install_pending_paths().is_some();
    *g = Some((Instant::now(), v));
    v
}

/// 자기설치 도우미 스크립트. 설치 직전 재확인 두 개는 arm 시점 검사와 별개로 필요하다 —
/// 헬퍼는 pid 가 사라지길 기다리는데, 그 pid 가 다른 장수 프로세스로 재사용되면 며칠 뒤에야
/// 발화할 수 있고, 사람이 끄자마자 다시 켜면 새 인스턴스가 이미 떠 있을 수도 있다. 그 상태로
/// 설치본을 치우면 도는 앱의 서명 페이지가 무효가 되어 macOS 가 앱을 SIGKILL 한다 — 그래서
/// (1) dist 가 지금도 더 새것인지, (2) 설치본을 도는 프로세스가 없는지 를 발화 시점에 다시 본다.
///
/// 설치본은 지우지 않고 [backup] 으로 옮겨 둔다(같은 폴더라 이름만 바뀐다). 복사가 중간에
/// 실패하면 반쯤 복사된 것을 걷고 옮겨 둔 판을 되돌린다 — 전에는 `rm -rf` 뒤 `cp` 라 복사가
/// 실패하면 앱이 통째로 사라졌고, 다음 판을 받을 길(업데이터·자기설치)도 함께 사라졌다.
/// 이름 끝이 `.app` 이 아니라 Launch Services 가 두 번째 카사텀으로 치지 않는다.
pub(crate) fn self_install_script(
    pid: u32,
    installed: &std::path::Path,
    dist: &std::path::Path,
    backup: &std::path::Path,
) -> String {
    let running = installed.join("Contents/MacOS/kasaterm");
    let fresh = dist.join("Contents/MacOS/kasaterm");
    format!(
        "while kill -0 {pid} 2>/dev/null; do sleep 0.3; done\n\
         [ '{fresh}' -nt '{run}' ] || {{ echo \"skipped: dist not newer $(date)\"; exit 0; }}\n\
         ! /usr/bin/pgrep -f '{run}' >/dev/null 2>&1 \
         || {{ echo \"skipped: app running $(date)\"; exit 0; }}\n\
         rm -rf '{bak}' && mv '{inst}' '{bak}' || {{ echo \"skipped: backup failed $(date)\"; exit 0; }}\n\
         if cp -R '{dist}' '{inst}' && touch '{inst}'; then echo \"installed $(date)\"; \
         else rm -rf '{inst}'; mv '{bak}' '{inst}' && echo \"install FAILED, previous restored $(date)\" \
         || echo \"install FAILED, restore FAILED — previous is at {bak} $(date)\"; fi\n",
        inst = installed.display(),
        dist = dist.display(),
        bak = backup.display(),
        fresh = fresh.display(),
        run = running.display(),
    )
}

pub(crate) fn arm_self_install() {
    let Some((installed, dist)) = install_pending_paths() else {
        return;
    };
    let log = std::env::temp_dir().join("kasaterm-selfinstall.log");
    let backup = installed.with_file_name(".kasaterm.app.previous");
    let script = self_install_script(std::process::id(), &installed, &dist, &backup);
    let Ok(out) = std::fs::File::create(&log) else {
        return;
    };
    let Ok(err) = out.try_clone() else { return };
    let _ = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .stdout(out)
        .stderr(err)
        .spawn();
}

/// 우리를 띄운 claude 세션의 흔적을 우리 env 에서 지운다 — pane 을 낳기 전에.
///
/// kasaterm 을 claude 안에서 실행하는 건 예외가 아니라 일상이다: 재시작 스크립트를
/// 세션에서 돌리면 새 앱이 그 claude 의 자식으로 뜬다. 그러면 `CHILD_SESSION`·
/// `TEAMMATE_MODE`·`SESSION_ID` 가 앱에 눌어붙고, 앱이 낳는 **모든 pane** 에 흘러
/// 거기서 뜬 claude 가 "나는 이미 남의 자식" 이라며 transcript 저장을 끈다. 사용자가
/// 본 `Transcript saving is off — inherited CLAUDE_CODE_CHILD_SESSION marker` 가
/// 그것이다. pane 하나가 아니라 그 인스턴스의 pane 전부가 그렇게 된다.
///
/// spawn 쪽이 아니라 여기서 지우는 건, 새는 통로가 pane 만이 아니어서다 — 훅·MCP
/// 서버·계정 갱신 프로브도 같은 env 를 물려받는다. 입구를 한 번 막는 게 출구를
/// 전부 세는 것보다 낫다. 앱은 이 값들을 하나도 읽지 않으므로 지워서 잃을 게 없다.
/// "이 프로세스는 claude 안에서 태어났다"를 뜻하는 env 마커 전부. 물려받으면
/// 자식 claude 가 중첩으로 오인해 transcript 저장을 끄거나 attach 대신 새 세션
/// TUI 로 폴백한다 — 부팅 스크럽(아래)과 bridge 의 attach 스폰이 같은 목록을 쓴다.
pub(crate) const CLAUDE_MARKER_ENV: [&str; 8] = [
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_TEAMMATE_MODE",
    "CLAUDE_CODE_FORK_SUBAGENT",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_PID",
    "CLAUDECODE",
];

pub(crate) fn scrub_inherited_claude_markers() {
    for k in CLAUDE_MARKER_ENV {
        std::env::remove_var(k);
    }
}

/// 패닉을 파일로 남긴다 — Finder 로 뜬 앱은 stderr 가 버려져서, 패닉으로 죽으면
/// 크래시 리포트도 로그도 없이 사라진다(정상 종료가 아니라 `exiting` 도 안 돌아
/// 자기설치 로그조차 안 갱신된다). "왜 꺼졌는지" 를 알 유일한 증거를 남긴 뒤
/// 기본 훅에 넘긴다. GUI 스레드 패닉(즉사)과 작업 스레드 패닉(Mutex poison 으로
/// 지연 폭발) 둘 다 여기를 지나므로, 죽음의 첫 원인이 항상 파일 맨 위에 남는다.
/// 설치 앱의 stderr 를 파일로 남긴다 — `$TMPDIR/kasaterm-app.log`(패닉 로그 옆).
///
/// Finder 로 뜬 .app 의 stderr 는 어디에도 안 닿는다. 그래서 `[sweep]`·복원처럼
/// pane 이 화면에서 빠지는 순간을 적는 eprintln 이 전부 허공에 흩어졌고, 2026-09-03
/// 하루에 세 번(아침 %5·%2, 저녁 우사기) 「재시작하니 pane 이 사라졌다」를 원인
/// 미확정으로 남겼다. 터미널에 붙어 있으면(`cargo run`) 그대로 둔다. 5MB 를 넘으면
/// 부팅 때 한 번 비운다 — 최근 부팅 몇 번이 남는 쪽이 디스크보다 값이 있다.
#[cfg(target_os = "macos")]
pub(crate) fn install_stderr_log(suffix: &str) {
    use std::os::unix::io::AsRawFd;
    // SAFETY: isatty 는 fd 번호 하나를 읽기만 한다.
    if unsafe { libc::isatty(2) } == 1 {
        return;
    }
    let log = std::env::temp_dir().join(format!("kasaterm{suffix}-app.log"));
    if std::fs::metadata(&log)
        .map(|m| m.len() > 5 * 1024 * 1024)
        .unwrap_or(false)
    {
        let _ = std::fs::remove_file(&log);
    }
    let Ok(f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
    else {
        return;
    };
    // SAFETY: f 는 열린 파일이고 dup2 는 fd 2 를 그 파일의 복제로 바꾼다. f 가 여기서
    // 닫혀도 fd 2 는 독립된 복제라 남는다.
    unsafe {
        libc::dup2(f.as_raw_fd(), 2);
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    eprintln!("==== boot epoch={ts} pid={} ====", std::process::id());
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn install_stderr_log(_suffix: &str) {}

pub(crate) fn install_panic_logger(suffix: &str) {
    let prev = std::panic::take_hook();
    let log = std::env::temp_dir().join(format!("kasaterm{suffix}-panic.log"));
    std::panic::set_hook(Box::new(move |info| {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
        {
            use std::io::Write;
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let thread = std::thread::current();
            let _ = writeln!(
                f,
                "==== panic epoch={ts} pid={} thread={} ====\n{info}\n{}\n",
                std::process::id(),
                thread.name().unwrap_or("?"),
                std::backtrace::Backtrace::force_capture(),
            );
        }
        prev(info);
    }));
}

/// `ViewerLaunch::lite` 의 전역 사본 — App 을 못 받는 자유함수(설정 페인터·nav)가
/// 묻는다. 부팅에서 한 번 세우고 다시 안 바뀐다.
pub(crate) static LITE_MODE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(crate) fn lite_mode() -> bool {
    LITE_MODE.load(std::sync::atomic::Ordering::Relaxed)
}

/// Load `$TMPDIR/kasaterm-capture.env` (KEY=VALUE lines) into the
/// process environment, then delete it. This is the bridge for capture:
/// `open` strips shell env, so a capture script drops KASATERM_* here
/// and the `open`-launched .app picks them up on startup. One-shot
/// (deleted on read) so a normal launch is never affected, and a real
/// env var still wins — we only fill in keys that aren't already set.
pub(crate) fn load_capture_config() {
    let path = std::env::temp_dir().join("kasaterm-capture.env");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return;
    };
    let _ = std::fs::remove_file(&path);
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        if std::env::var_os(k).is_none() {
            std::env::set_var(k, v);
        }
    }
}

/// pane 자식 셸이 쓸 보조 바이너리/설정을 private dir 에 깔고 그 dir 를
/// `KASATERM_TMUX_SHIM_DIR` 로 넘긴다(pty-backend 가 PATH/ZDOTDIR 에 반영):
/// kasaterm-cli(pane 간 협업)·imgopen/mdopen(preview)·zsh OSC133 prompt-mark
/// (입력줄 감지). teammate-mode tmux 위장은 제거됨 — pane 생성은 오케스트레이터가
/// `kasaterm-cli split` 로 한다. best-effort: 실패해도 본체는 동작한다.
/// 창을 활성화하지 않고 배경에 띄울지. 자동 종료하는 검증 실행이면 기본 on.
pub(crate) fn background_launch() -> bool {
    match std::env::var("KASATERM_NO_FOCUS").as_deref() {
        Ok("0") | Ok("false") | Ok("") => false,
        Ok(_) => true,
        Err(_) => std::env::var_os("KASATERM_AUTOQUIT_MS").is_some(),
    }
}

/// KasaLite 의 살림을 본판과 가른다 — `prepare_session_storage` 보다 먼저.
///
/// 격리 창구는 검증 리그가 쓰는 env 그대로다(docs/verify-app.md). 전부 **조건 없이**
/// 덮어쓴다: lite 를 본판 pane 안에서 띄우면 `KASATERM_SOCKET_PATH`·
/// `KASATERM_TMUX_SHIM_DIR` 를 물려받는데, 후자는 kasa-pty 가 그대로 PATH/ZDOTDIR 에
/// 붙여 **본판의 claude 래퍼(훅 포함)가 lite pane 에 들어간다.** 상속이 곧 위험이라
/// 「없을 때만」이 아니다. 뿌리는 `KASATERM_LITE_ROOT`(검증용) 아니면
/// `~/.config/kasaterm-lite`.
/// 엔진(kasa-pty)에 이 앱이 누구인지 알린다 — 칸의 `TERM_PROGRAM`, 「Last login」 상태
/// 파일 자리. 격리 인스턴스(라이트·검증 리그)는 세션 파일 옆에 둬서 본판
/// `~/.config/kasaterm` 을 건드리지 않는다. 그래서 env 를 읽는 캡처 설정 뒤에 부른다.
pub(crate) fn install_pty_host_policy() {
    let last_login_dir = std::env::var_os("KASATERM_SESSION_FILE")
        .filter(|v| !v.is_empty())
        .and_then(|v| std::path::PathBuf::from(v).parent().map(|d| d.to_path_buf()))
        .or_else(|| kasa_socket::home_dir().map(|h| h.join(".config").join("kasaterm")));
    kasa_pty::set_host_policy(kasa_pty::HostPolicy {
        term_program: Some(("kasaterm".into(), env!("CARGO_PKG_VERSION").into())),
        last_login_dir,
        clipboard: None,
    });
}

pub(crate) fn apply_lite_env() {
    let root = std::env::var_os("KASATERM_LITE_ROOT")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| kasa_socket::home_dir().map(|h| h.join(".config/kasaterm-lite")))
        .unwrap_or_else(|| std::env::temp_dir().join("kasaterm-lite"));
    if let Err(e) = std::fs::create_dir_all(&root) {
        eprintln!("[lite] mkdir {root:?} failed: {e}");
    }
    let set = |k: &str, v: std::path::PathBuf| std::env::set_var(k, v);
    set("KASATERM_SESSION_FILE", root.join("session.json"));
    set("KASATERM_SETTINGS_FILE", root.join("settings.json"));
    set("KASATERM_WINDOW_FILE", root.join("window.json"));
    set("KASATERM_STUDENTS_DIR", root.join("students"));
    set("KASATERM_COLLAB_ROOT", root.clone());
    set("KASATERM_MACHINES", root.join("machines.json"));
    set("KASATERM_MOBILE_USERS", root.join("mobile-users.json"));
    set("KASATERM_SOCKET_PATH", root.join("lite.sock"));
    set("CMUX_SOCKET_PATH", root.join("lite.sock"));
    std::env::remove_var("KASATERM_TMUX_SHIM_DIR");
}

/// 이 실행이 **하네스가 띄운 검증 인스턴스**인가.
///
/// 판정 근거는 창 기하를 env 로 강제했다는 것 하나다 — 사람이 쓰는 창은 저장된 자리에
/// 뜨지 강제되지 않는다. 이게 참이면 그 인스턴스는 사용자의 설정을 **읽지도 쓰지도**
/// 않는다: 창 크기를 저장하지 않고(`save_window_frame`), 저장 세션 복원도 묻지 않는다.
/// 설정 파일이 인스턴스 사이에 공유되기 때문이고, 실제로 한쪽만 막았다가 나머지에
/// 당했다(430x700 검증 실행이 `window.json` 을 덮었고, 복원 대화상자가 캡처를 가렸다).
pub(crate) fn verification_run() -> bool {
    std::env::var_os("KASATERM_WINDOW_SIZE").is_some()
        || std::env::var_os("KASATERM_WINDOW_POS").is_some()
}

/// 끝난 태스크를 며칠 뒤에 지울지. `KASATERM_TASK_KEEP_DAYS` 로 조절.
pub(crate) const TASK_KEEP_DAYS: u64 = 3;

/// **열린 채로** 방치된 태스크를 며칠 뒤에 지울지. 끝난 것보다 훨씬 길게 잡는다 —
/// 어제 안 끝낸 일은 밀린 일이지만, 2주를 손 안 댄 일은 버려진 일이다.
/// (실측 2026-08-06 sionic 방: 07-24 slack-sentry 미완료 4건이 방이 다른 주제로
/// 옮겨 간 뒤에도 목록 맨 위를 차지하고 있었다 — 열린 것이 먼저 그려지기 때문.)
pub(crate) const TASK_OPEN_KEEP_DAYS: u64 = 14;

/// 다 끝난 태스크 파일을 치운다 — **태스크 저장소가 쌓이기만 하고 아무도 안 비웠다.**
///
/// 저장소는 `~/.claude/tasks/<팀>/` 이고 팀 = 방(cwd) 이라, 한 방에서 2주를 일하면
/// 그 방의 모든 pane 이 2주치 완료 목록을 달고 다닌다(실측 2026-08-06 sionic 방:
/// 34개 중 29개 완료, 가장 오래된 것이 7월 24일). board 가 그걸 거르지 않고 다 그려서
/// 「지금 뭘 하는지」가 안 보였다 — 사용자: "아루 태스크는 왜 저렇게 돼 있어".
///
/// 문턱은 둘이다: 끝난 것 `TASK_KEEP_DAYS`(3일), **열린 채 방치된 것**
/// `TASK_OPEN_KEEP_DAYS`(14일). 어제 안 끝낸 일은 밀린 일이라 살려 두고, 2주를 손 안
/// 댄 일만 버려진 것으로 본다.
///
/// 되돌릴 수 없는 삭제라 판정을 좁게 잡는다: 파일이 파싱되고 status 를 읽을 수 있을
/// 때만 손댄다(깨진 파일·모르는 형식은 그대로 둔다).
pub(crate) fn prune_finished_tasks() {
    // Windows GUI 프로세스엔 HOME 이 없다 — claude 저장소는 USERPROFILE 밑이다.
    let Some(home) = kasa_socket::home_dir() else {
        return;
    };
    let days = |k: &str, d: u64| {
        std::time::Duration::from_secs(
            std::env::var(k)
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(d)
                * 86_400,
        )
    };
    let cutoff = (
        days("KASATERM_TASK_KEEP_DAYS", TASK_KEEP_DAYS),
        days("KASATERM_TASK_OPEN_KEEP_DAYS", TASK_OPEN_KEEP_DAYS),
    );
    let Ok(teams) = std::fs::read_dir(home.join(".claude/tasks")) else {
        return;
    };
    let mut gone = 0usize;
    for team in teams.flatten() {
        let Ok(files) = std::fs::read_dir(team.path()) else {
            continue;
        };
        for f in files.flatten() {
            let p = f.path();
            if p.extension().is_none_or(|e| e != "json") {
                continue;
            }
            if !task_file_is_stale(&p, cutoff) {
                continue;
            }
            if std::fs::remove_file(&p).is_ok() {
                gone += 1;
            }
        }
    }
    if gone > 0 {
        eprintln!("[tasks] 오래된 태스크 {gone}개 정리");
    }
}

/// **다 읽은** 인박스 파일을 치운다. 이름에 부팅 꼬리가 붙은 뒤로 pane 마다 새 파일이
/// 생기므로(`agent_name_suffix`) 안 지우면 팀 디렉터리가 무한히 자란다 — 꼬리를 넣기
/// 전에 이미 74개가 쌓여 있었다.
///
/// **비어 있는 것만** 지운다. 내용이 남아 있으면 아직 아무도 안 읽은 지시일 수 있고,
/// 그건 지울 게 아니라 사람이 봐야 할 것이다(실측: 08-04 브리프 4건이 그렇게 남아
/// 있었다). 하루가 지나야 손대는 것도 같은 이유 — 방금 만들어진 빈 파일은 지금 막 뜬
/// 학생의 것이다.
pub(crate) fn prune_empty_inboxes() {
    let Some(home) = kasa_socket::home_dir() else {
        return;
    };
    let day = std::time::Duration::from_secs(86_400);
    let Ok(teams) = std::fs::read_dir(home.join(".claude/teams")) else {
        return;
    };
    let mut gone = 0usize;
    for team in teams.flatten() {
        let Ok(files) = std::fs::read_dir(team.path().join("inboxes")) else {
            continue;
        };
        for f in files.flatten() {
            let p = f.path();
            let empty = std::fs::read_to_string(&p)
                .ok()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                .is_some_and(|v| v.as_array().is_some_and(|a| a.is_empty()));
            let old = std::fs::metadata(&p)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > day);
            if empty && old && std::fs::remove_file(&p).is_ok() {
                gone += 1;
            }
        }
    }
    if gone > 0 {
        eprintln!("[inbox] 다 읽은 빈 인박스 {gone}개 정리");
    }
}

/// 지워도 되는 태스크 파일인가. `cutoff` = (끝난 것 기준, 열린 것 기준).
/// 판정을 파일시스템 순회에서 갈라 두면 규칙을 테스트로 고정할 수 있다.
pub(crate) fn task_file_is_stale(
    path: &std::path::Path,
    cutoff: (std::time::Duration, std::time::Duration),
) -> bool {
    let Ok(body) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) else {
        return false;
    };
    let limit = match v.get("status").and_then(|s| s.as_str()) {
        Some("completed") | Some("deleted") => cutoff.0,
        // 열린 것도 지우긴 하지만 훨씬 뒤에. 상태를 못 읽는 파일은 손대지 않는다.
        Some("pending") | Some("in_progress") => cutoff.1,
        _ => return false,
    };
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age > limit)
}

/// 부팅 시 temp_dir 의 죽은 `kasaterm-<pid>.sock` 잔재를 청소한다. 소켓 경로가
/// PID 별이라 인스턴스마다 다른 파일을 만드는데, `Server::bind` 의 stale 정리는
/// *자기 경로* 만 치워서 죽은 다른 인스턴스 소켓이 영영 남는다(재시작·빌드 반복
/// 시 누적). 여기서 connect 가 실패하는(=리스너 없는) 소켓 파일만 지운다 —
/// 살아있는 인스턴스 소켓은 절대 건드리지 않으므로 멀티 인스턴스에서도 안전.
/// 자기 PID 소켓은 아직 bind 전이라 connect 가 실패할 수 있으니 제외한다.
#[cfg(unix)]
/// 죽은 인스턴스의 소켓 잔재를 지우고, **지금 살아있는 kasaterm pid 들**을 돌려준다
/// (connect 성공 = 살아있는 리스너). 소켓 이름이 `kasaterm-<pid>.sock` 이라 여기서
/// 인스턴스 명부가 공짜로 나온다 — collab 마커 청소가 그걸로 주인을 가린다.
/// 자기 pid 는 아직 bind 전이라 connect 로는 안 잡히므로 직접 넣는다.
pub(crate) fn live_kasaterm_pids() -> std::collections::HashSet<u32> {
    let mut live = std::collections::HashSet::from([std::process::id()]);
    let own = format!("kasaterm-{}.sock", std::process::id());
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return live;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(pid) = name
            .strip_prefix("kasaterm-")
            .and_then(|s| s.strip_suffix(".sock"))
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        if name == own {
            continue;
        }
        let path = entry.path();
        if std::os::unix::net::UnixStream::connect(&path).is_err() {
            let _ = std::fs::remove_file(&path);
        } else {
            live.insert(pid);
        }
    }
    live
}

/// Windows 판. 여기선 **지울 잔재가 없다** — 소켓 경로가 파일이 아니라 named pipe
/// (`\\.\pipe\kasaterm-<pid>.sock`, `kasa_socket::transport`)로 매핑되고, 파이프는
/// 마지막 핸들이 닫히는 순간 커널이 지운다. 그래서 "존재 == 살아있음"이고, 남는
/// 일은 이름에서 pid 를 읽어내는 것뿐이다.
///
/// 파이프 네임스페이스를 못 읽으면 자기 pid 만 돌려준다 — 최악이라야 살아있는
/// 남의 마커를 지우는 건데, 마커는 pane 이 뜰 때 다시 쓰인다(unix 쪽 read_dir
/// 실패 폴백과 같은 판단).
#[cfg(windows)]
pub(crate) fn live_kasaterm_pids() -> std::collections::HashSet<u32> {
    let mut live = std::collections::HashSet::from([std::process::id()]);
    // 파이프 네임스페이스는 **슬래시 형태로만** 열린다. `\\.\pipe\` 를 주면 Rust 가
    // 이미 verbatim 취급인 UNC 로 보고 `\*` 글롭을 못 붙여 os error 3 로 죽는다
    // (실측: `//./pipe/` = 289개, `\\.\pipe\`·`\\.\pipe`·`\\?\pipe\` = 전부 실패).
    let Ok(entries) = std::fs::read_dir("//./pipe/") else {
        return live;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(pid) = name
            .strip_prefix("kasaterm-")
            .and_then(|s| s.strip_suffix(".sock"))
            .and_then(|s| s.parse::<u32>().ok())
        {
            live.insert(pid);
        }
    }
    live
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 자기설치 도우미를 실제 sh 로 돌린다 — 성공하면 이전 판이 곁에 남고, 복사가 실패하면
    /// 설치본이 되돌아오고(앱이 사라지면 다음 판을 받을 길도 사라진다), 더 새것이 아니면 손대지 않는다.
    /// 자기설치는 macOS `.app` 번들 계약이다 — 다른 OS 에서는 설치본 경로가 `current_exe` 와 안 맞아 움직이지 않는다.
    #[cfg(target_os = "macos")]
    #[test]
    fn self_install_keeps_the_previous_bundle_and_restores_it_on_failure() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::Duration;
        let tmp = std::env::temp_dir().join(format!("selfinstall-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let bundle = |dir: &std::path::Path, body: &str| {
            std::fs::create_dir_all(dir.join("Contents/MacOS")).unwrap();
            std::fs::write(dir.join("Contents/MacOS/kasaterm"), body).unwrap();
        };
        let read = |dir: &std::path::Path| std::fs::read_to_string(dir.join("Contents/MacOS/kasaterm")).ok();
        let (inst, dist, bak) = (tmp.join("Applications/kasaterm.app"), tmp.join("dist/kasaterm.app"), tmp.join("Applications/.kasaterm.app.previous"));
        let gone = std::process::Command::new("/usr/bin/true").spawn().unwrap();
        let pid = gone.id();
        let _ = { let mut c = gone; c.wait() };
        let run = || {
            let out = std::process::Command::new("/bin/sh").arg("-c").arg(self_install_script(pid, &inst, &dist, &bak)).output().unwrap();
            String::from_utf8_lossy(&out.stdout).into_owned()
        };

        bundle(&inst, "old");
        std::thread::sleep(Duration::from_millis(1100));
        bundle(&dist, "new");
        assert!(run().starts_with("installed"));
        assert_eq!((read(&inst).as_deref(), read(&bak).as_deref()), (Some("new"), Some("old")));

        assert!(run().starts_with("skipped: dist not newer"));
        assert_eq!(read(&inst).as_deref(), Some("new"));

        std::thread::sleep(Duration::from_millis(1100));
        bundle(&dist, "newer");
        std::fs::create_dir_all(dist.join("Contents/Locked")).unwrap();
        std::fs::write(dist.join("Contents/Locked/x"), "x").unwrap();
        std::fs::set_permissions(dist.join("Contents/Locked/x"), std::fs::Permissions::from_mode(0o000)).unwrap();
        let log = run();
        std::fs::set_permissions(dist.join("Contents/Locked/x"), std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(log.contains("install FAILED, previous restored"), "{log}");
        assert_eq!(read(&inst).as_deref(), Some("new"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn task_prune_spares_open_tasks_however_old() {
        let dir = std::env::temp_dir().join(format!("kt-task-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, status: &str| {
            let p = dir.join(name);
            std::fs::write(&p, format!("{{\"id\":\"1\",\"status\":\"{status}\"}}")).unwrap();
            p
        };
        let zero = std::time::Duration::from_secs(0);
        let long = std::time::Duration::from_secs(86_400);
        // 끝난 것은 지나고 열린 것은 아직 — 이 조합이 두 문턱이 갈려 있음을 고정한다.
        // (하나로 합치면 어제 안 끝낸 일이 완료분과 같이 쓸려 나간다.)
        assert!(task_file_is_stale(
            &write("done.json", "completed"),
            (zero, long)
        ));
        assert!(task_file_is_stale(
            &write("gone.json", "deleted"),
            (zero, long)
        ));
        assert!(!task_file_is_stale(
            &write("open.json", "pending"),
            (zero, long)
        ));
        assert!(!task_file_is_stale(
            &write("busy.json", "in_progress"),
            (zero, long)
        ));
        // 열린 문턱까지 지나면 그건 밀린 일이 아니라 버려진 일이다.
        assert!(task_file_is_stale(
            &write("dead.json", "pending"),
            (zero, zero)
        ));
        // 아직 안 지난 완료분은 남는다.
        assert!(!task_file_is_stale(
            &write("fresh.json", "completed"),
            (long, long)
        ));
        // 깨진 파일·없는 파일·모르는 status 는 건드리지 않는다(삭제는 못 되돌린다).
        let broken = dir.join("broken.json");
        std::fs::write(&broken, "not json").unwrap();
        assert!(!task_file_is_stale(&broken, (zero, zero)));
        assert!(!task_file_is_stale(
            &write("weird.json", "쩜쩜"),
            (zero, zero)
        ));
        assert!(!task_file_is_stale(&dir.join("nope.json"), (zero, zero)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 자기 설치가 겨누는 `dist/` 가 정말 레포 루트 밑인지. 상대 경로가 한 칸만
    /// 어긋나도 `metadata` 가 조용히 실패해 **아무 일도 안 일어나고**, 그 침묵은
    /// "새 빌드가 없어서 안 깔았다" 와 구분되지 않는다 — 껐다 켜도 옛 바이너리인
    /// 채로 아무도 눈치채지 못한다.
    #[test]
    fn self_install_dist_path_resolves_to_the_repo_root() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let root = root.canonicalize().expect("레포 루트는 언제나 실재한다");
        assert!(
            root.join("Cargo.toml").is_file(),
            "워크스페이스 매니페스트가 여기 있어야 한다"
        );
        assert!(
            root.join("scripts/build-app.sh").is_file(),
            "번들을 굽는 스크립트도 같은 자리"
        );
    }
}
