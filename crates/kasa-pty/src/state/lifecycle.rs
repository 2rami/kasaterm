//! 세션의 탄생과 죽음 — 로컬 셸 띄우기(`start`)·원격 PTY 의 로컬 파서(`start_external`)·
//! 산 채로 넘겨받기(`adopt`)·명시적 닫기·Drop 의 자식 거두기.

use super::*;
use super::inline::INLINE_IMAGE_SLOTS;
#[cfg(unix)]
use super::process::process_table_raw;
use super::reader::spawn_reader_thread;
use super::viewport::pty_size;
use super::vt::make_term;
#[cfg(test)]
use super::test_support::{ext_session, test_posix_shell, wait_text};

/// 외부 소스(WebSocket 클라이언트 등)가 `start_external` 세션에 밀어 넣는 이벤트.
///
/// 순서가 곧 정합성이다 — SetSize 를 별도 경로로 보내면 「옛 바이트를 새 크기로
/// 파싱」하는 찢어진 프레임이 생긴다. 한 채널에 순서대로 실으면 reader 루프의
/// 기존 「read 직후 크기 재확인」이 그대로 순서를 보장한다.
pub enum ExtEvent {
    /// New connection epoch, applied only after a whole following Bytes frame.
    Generation(u64),
    /// 원격 PTY 가 뱉은 raw 바이트. 파서로 직행한다.
    Bytes(Vec<u8>),
    /// 원격 격자 크기 변경 — 다음 Bytes 를 파싱하기 전에 적용된다.
    SetSize(u16, u16),
    /// 원격 세션이 정말로 끝났다(연결 유실이 아니라). reader 가 eof 센티널을
    /// 발행해 GUI 가 pane 을 걷는다.
    Eof,
}

/// `start_external` 에 넘기는 전송 계층 — 만드는 쪽(WS 클라이언트)이 이 셋을 쥔다.
pub struct ExternalIo {
    /// 수신 이벤트 스트림. Sender 쪽이 다 사라지면 Eof 와 같다.
    pub events: Receiver<ExtEvent>,
    /// 키 입력(send_bytes)·paste 가 나가는 길.
    pub writer: Box<dyn Write + Send>,
    /// GUI 쪽 resize 요청을 원격에 알리는 콜백(제어 메시지 전송).
    pub on_resize: Arc<dyn Fn(u16, u16) + Send + Sync>,
}

/// ExtEvent 채널을 `Read` 로 감싼다 — `spawn_reader_thread` 의 입력이
/// `Box<dyn Read + Send>` 라서, 이 어댑터 하나로 파서·tap·스냅샷 배관 전부를
/// 로컬 PTY 와 공유한다.
struct ExtReader {
    generation: u64,
    parsed_generation: Arc<std::sync::atomic::AtomicU64>,
    events: Receiver<ExtEvent>,
    /// 세션의 공유 크기 — SetSize 이벤트를 여기 반영하면 reader 루프의
    /// 「read 직후 크기 재확인」이 다음 파싱 전에 Term 을 맞춘다.
    size: Arc<Mutex<(u16, u16)>>,
    /// 64KB read 버퍼보다 큰 프레임의 남은 조각.
    pending: Vec<u8>,
}

impl Read for ExtReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if !self.pending.is_empty() {
                let n = self.pending.len().min(buf.len());
                buf[..n].copy_from_slice(&self.pending[..n]);
                self.pending.drain(..n);
                if self.pending.is_empty() {
                    self.parsed_generation.store(self.generation, std::sync::atomic::Ordering::Release);
                }
                return Ok(n);
            }
            match self.events.recv() {
                Ok(ExtEvent::Generation(generation)) => self.generation = generation,
                Ok(ExtEvent::Bytes(b)) => {
                    if b.is_empty() {
                        continue;
                    }
                    self.pending = b;
                }
                Ok(ExtEvent::SetSize(c, r)) => {
                    // resize() 와 같은 하한 — alacritty MIN_COLUMNS 밑은 밟지 않는다.
                    *self.size.lock().unwrap() = (c.max(2), r.max(1));
                }
                // 채널 단절 = 만든 쪽(WS 클라이언트)이 접었다 — 세션 종료와 같다.
                Ok(ExtEvent::Eof) | Err(_) => return Ok(0),
            }
        }
    }
}

/// 세 생성자(`start`·`start_external`·`adopt`)가 함께 쓰는 배관의 앞 절반 — 쓰기 길·크기·파서.
/// 셸의 첫 출력보다 먼저 파서에 줄을 먹여야(`seed`) 해서, 읽기 스레드를 띄우는 `launch` 와 갈라 둔다.
/// 생성자마다 다른 것은 바이트가 오가는 길(`Launch`)뿐이라, 필드 조립은 `launch` 한 곳에만 있다.
struct Wiring {
    size: Arc<Mutex<(u16, u16)>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    title_handle: Arc<Mutex<Option<String>>>,
    responder: PtyEventForwarder,
    term: Arc<Mutex<Term<PtyEventForwarder>>>,
}

/// 생성자마다 다른 것 — 바이트가 오가는 길과 셸의 정체.
struct Launch {
    reader: Box<dyn Read + Send>,
    /// poll 기반 정지에 쓸 master fd(핸드오프). 채널로 읽는 원격 미러는 없다.
    poll_fd: Option<i32>,
    /// 원격 미러만 — 연결 세대가 파싱까지 끝났음을 알린다(`ExtReader`).
    parsed_generation: Option<Arc<std::sync::atomic::AtomicU64>>,
    io: SessionIo,
    shell_pid: Option<u32>,
    tty_short: Option<String>,
}

impl Wiring {
    /// `respond` 는 자동 응답(DSR·OSC 색 질의 등)을 이쪽이 하는가 — 원격 미러는 원격 Term 이 이미 답한다.
    fn new(opts: &PtyOptions, writer: Box<dyn Write + Send>, respond: bool) -> Self {
        let size = Arc::new(Mutex::new((opts.cols, opts.rows)));
        let writer: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(writer));
        let title_handle = Arc::new(Mutex::new(None));
        let listener = PtyEventForwarder {
            writer: Arc::clone(&writer),
            size: Arc::clone(&size),
            last_title: Arc::clone(&title_handle),
            respond,
        };
        let responder = listener.clone();
        let term = Arc::new(Mutex::new(make_term(opts.cols, opts.rows, listener)));
        Self { size, writer, title_handle, responder, term }
    }

    /// 텍스트 줄을 셸의 첫 출력보다 먼저 파서에 먹인다 — 프로그램 출력처럼(v1: 색·속성 없는 글).
    fn seed(&self, lines: &[String]) {
        if lines.is_empty() {
            return;
        }
        let mut proc: Processor<StdSyncHandler> = Processor::new();
        let mut t = self.term.lock().unwrap();
        for line in lines {
            proc.advance(&mut *t, line.as_bytes());
            proc.advance(&mut *t, b"\r\n");
        }
    }

    /// Spin up the VT processor loop. Owns the Term, drains the
    /// reader, and emits a ScreenUpdate after each batch. Bounded
    /// channel + drop-on-full keeps us from buffering frames the
    /// renderer is too slow to consume.
    fn launch(self, opts: &PtyOptions, l: Launch) -> PtySession {
        let (tx, rx) = bounded::<ScreenUpdate>(256);
        let blocks: Arc<Mutex<VecDeque<CommandBlock>>> = Arc::new(Mutex::new(VecDeque::new()));
        let block_rev = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let cwd_handle: Arc<Mutex<Option<std::path::PathBuf>>> = Arc::new(Mutex::new(None));
        let byte_taps: Arc<Mutex<Vec<Sender<Vec<u8>>>>> = Arc::new(Mutex::new(Vec::new()));
        let screen_taps: Arc<Mutex<Vec<Sender<ScreenUpdate>>>> = Arc::new(Mutex::new(Vec::new()));
        let inline_imgs: Arc<Mutex<InlineImgs>> = Arc::new(Mutex::new(InlineImgs::default()));
        let scheme_reports = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let output_beats: Arc<Mutex<VecDeque<Instant>>> = Arc::new(Mutex::new(VecDeque::new()));
        let reader_thread = spawn_reader_thread(
            l.reader,
            Arc::clone(&reader_stop),
            l.poll_fd,
            tx.clone(),
            opts.cols,
            opts.rows,
            self.size.clone(),
            opts.pane_id.clone(),
            Arc::clone(&self.title_handle),
            Arc::clone(&self.term),
            Arc::clone(&blocks),
            Arc::clone(&block_rev),
            Arc::clone(&cwd_handle),
            Arc::clone(&byte_taps),
            Arc::clone(&screen_taps),
            Arc::clone(&inline_imgs),
            self.responder,
            Arc::clone(&scheme_reports),
            Arc::clone(&output_beats),
            l.parsed_generation,
        );
        PtySession {
            screens: rx,
            io: l.io,
            writer: self.writer,
            size: self.size,
            viewport_sizes: Mutex::new(ViewportSizes::new(opts.cols, opts.rows)),
            _reader_thread: reader_thread,
            shell_pid: l.shell_pid,
            proc_cache: Arc::new(Mutex::new((
                Instant::now() - std::time::Duration::from_secs(1),
                None,
            ))),
            agents_cache: Arc::default(),
            term: self.term,
            screens_tx: tx,
            byte_taps,
            screen_taps,
            title_handle: self.title_handle,
            pane_id: opts.pane_id.clone(),
            tty_short: l.tty_short,
            blocks,
            block_rev,
            cwd_handle,
            inline_imgs,
            scheme_reports,
            reader_stop,
            kill_disarmed: std::sync::atomic::AtomicBool::new(false),
            input_closed: std::sync::atomic::AtomicBool::new(false),
            input_revision: std::sync::atomic::AtomicU64::new(0),
            input_draft: std::sync::atomic::AtomicBool::new(true),
            draft_marked_at: Mutex::new(None),
            input_hold: Mutex::new(None),
            last_submit: Mutex::new(None),
            output_beats,
            last_input: Mutex::new(None),
            last_key: Mutex::new(None),
        }
    }
}

impl PtySession {
    pub fn start(opts: PtyOptions) -> Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(pty_size(opts.cols, opts.rows))
            .context("openpty")?;
        // Default to the user's login shell. CommandBuilder picks up
        // $SHELL fallback on its own when we don't override; pass `-il`
        // when we know we're handing off to zsh / bash so .zshrc /
        // .bashrc gets sourced (matches what tmux-bridge does inside
        // its `new-session -d 'exec $SHELL -il'`).
        let mut cmd = if let Some(shell) = opts.shell.as_deref() {
            let mut c = CommandBuilder::new(shell);
            // `-il` (login + interactive) is a bash/zsh/sh-ism that sources
            // rc files. PowerShell / cmd / wsl reject unknown flags ("Invalid
            // argument '-il'"), so only hand it to POSIX-style shells, matched
            // by executable stem (drops the `.exe` on Windows).
            let stem = std::path::Path::new(shell)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if matches!(stem.as_str(), "bash" | "zsh" | "sh" | "dash" | "ksh") {
                c.arg("-il");
            } else if matches!(stem.as_str(), "pwsh" | "powershell") {
                // PowerShell freezes the OS-level process cwd at launch (`cd`
                // only moves its internal $PWD), so the breadcrumb can't read it
                // off the process. Inject a prompt wrapper that emits
                // OSC 9;9;<path> every line; the reader sniffs it (scan_osc_cwd)
                // and the header follows `cd`. -Command runs after the user
                // profile loads, so $function:prompt captures the profile's
                // prompt and we chain it rather than clobber it.
                c.arg("-NoExit");
                c.arg("-Command");
                c.arg(PWSH_CWD_SHIM);
            }
            c
        } else {
            // Use the default shell from $SHELL.
            CommandBuilder::new_default_prog()
        };
        // 없는 폴더를 주면 셸 자체가 안 뜬다 — 다른 기계의 경로를 그대로 들고 온
        // 원격 pane(`mini`), 지워진 폴더의 세션 복원이 그렇다. 홈으로 떨어뜨린다
        // (2026-09-06 「mini 를 치면 경로 없어도 홈으로 이동하게라도」).
        if let Some(cwd) = opts.cwd.as_deref() {
            if std::path::Path::new(cwd).is_dir() {
                cmd.cwd(cwd);
            } else if let Some(home) = std::env::var_os("HOME") {
                cmd.cwd(home);
            }
        }
        // Terminal-identity env. portable-pty's CommandBuilder inherits
        // the parent process env, so if we were launched from iTerm /
        // Ghostty / Terminal.app, child TUIs (Claude Code, vim, etc)
        // see `TERM_PROGRAM=iTerm.app` and treat us as that host —
        // sending iTerm-only escapes that our alacritty parser would
        // either ignore or render as garbage. Force a consistent
        // identity and scrub the iTerm-specific leftovers so the
        // detection settles on kasaterm regardless of who launched us.
        // The truecolor decision in claude code's chalk supports-color is
        // gated on `COLORTERM === "truecolor"`. Once we stopped propagating
        // TMUX into the child env (chalk treats it as "wrapped, no
        // passthrough" and falls back to 256), COLORTERM alone is enough
        // to drive truecolor — ghostty masquerade (TERM=xterm-ghostty,
        // TERM_PROGRAM=ghostty, GHOSTTY_BIN_DIR, TERMINFO) is no longer
        // needed for colour matching. Identifying as our real selves
        // keeps the env simple and avoids breaking on ghostty-less
        // machines that don't have the bundle paths above.
        cmd.env("TERM", "xterm-256color");
        let host = crate::host::host_policy();
        match &host.term_program {
            Some((name, version)) => {
                cmd.env("TERM_PROGRAM", name);
                cmd.env("TERM_PROGRAM_VERSION", version);
            }
            None => {
                cmd.env_remove("TERM_PROGRAM");
                cmd.env_remove("TERM_PROGRAM_VERSION");
            }
        }
        cmd.env("COLORTERM", "truecolor");
        // 그림을 장수 단위로 관리하는 TUI(kasaslk)가 이 값을 읽고 그 안에서만
        // 쓴다. 안 주면 옛 한도 16 으로 보고 줄여 그린다.
        cmd.env("KASATERM_INLINE_IMAGE_SLOTS", INLINE_IMAGE_SLOTS.to_string());
        // Claude Code 는 kitty 그림을 XTVERSION 이름이 kitty·ghostty 일 때만 쓴다
        // (2.1.287: 이름 허용 목록 + `a=q` 응답). 이름을 꾸미면 다른 판정까지 남의
        // 터미널 것으로 갈려(예전 ghostty 위장을 걷은 까닭, 위 TERM 주석) 이 값 —
        // Claude Code 가 모르는 터미널을 위해 둔 공식 스위치 — 로 지원을 알린다.
        // kitty 프로토콜을 아는 다른 앱은 `a=q` 질의에 정직하게 답해 알린다.
        // ConPTY 는 APC 를 넘기지 않아 Windows 에선 그림이 못 와 켜지 않는다.
        #[cfg(unix)]
        cmd.env("CLAUDE_CODE_FORCE_TERMINAL_IMAGES", "1");
        // claude 의 렌더러(classic·fullscreen)는 여기서 정하지 않는다 — 사람이 claude 의
        // `/tui` 로 고른다. 08-30~09-28 사이 이 자리에서 강제를 다섯 번 뒤집었다. 마지막
        // 이유는 classic 이 `/config` 같은 대화창을 닫으며 화면 한 장만 다시 찍어 위 대화가
        // 사라지는 것, 그리고 classic 을 메우려 얹은 보정(입력창 붙잡기·여백 옮기기)이
        // 좌표를 어긋나게 한 것이었다(2026-09-28 「명령어 그런것도 없애고 그냥 켜자」).
        for k in [
            "ITERM_SESSION_ID",
            "ITERM_PROFILE",
            "LC_TERMINAL",
            "LC_TERMINAL_VERSION",
            // WezTerm / Alacritty leave their own crumbs too — strip them
            // so a TUI can't mis-attribute us. GHOSTTY_RESOURCES_DIR is
            // NOT in this list anymore because we want to set it
            // ourselves below; portable-pty's `env_remove` wipes the
            // entry from the same BTreeMap we just inserted into, so
            // including it here would silently undo our `env` call.
            "WEZTERM_PANE",
            "WEZTERM_EXECUTABLE",
            "ALACRITTY_LOG",
            "ALACRITTY_WINDOW_ID",
        ] {
            cmd.env_remove(k);
        }
        // pane shim 인프라. install_pane_shims 가 shim_dir 를 만들어
        // KASATERM_TMUX_SHIM_DIR 로 넘기면 PATH 앞에 붙이고 zsh ZDOTDIR 를 그
        // dir 로 가리킨다 — 자식 셸이 그 안의 kasaterm-cli(협업)·imgopen/mdopen
        // (preview)·OSC133 prompt-mark(입력줄 감지)를 쓰게 한다. (teammate-mode
        // tmux 위장은 제거됨 — pane 생성은 오케스트레이터가 `kasaterm-cli split` 로 한다.)
        if let Ok(shim_dir) = std::env::var("KASATERM_TMUX_SHIM_DIR") {
            let parent_path = std::env::var("PATH").unwrap_or_default();
            // PATH separator is platform-specific: `:` on Unix,
            // `;` on Windows. Using `:` on Windows folds the whole
            // chain into one literal entry and breaks every lookup.
            let sep = if cfg!(windows) { ';' } else { ':' };
            cmd.env("PATH", format!("{shim_dir}{sep}{parent_path}"));
            // Point zsh at the shim dir's rc files. They source the user's
            // real rc first, then re-prepend the shim dir to PATH so our
            // kasaterm-cli wins over brew. zsh-only; other shells ignore
            // ZDOTDIR and use the PATH prepend above.
            cmd.env("ZDOTDIR", &shim_dir);
        }
        // TMUX is intentionally NOT set in the child env: Claude Code / ink /
        // chalk read its presence as "inside tmux" and downgrade truecolor to
        // a 256-palette. COLORTERM=truecolor (set above) is what drives 24-bit
        // color now that we no longer masquerade as a tmux-wrapped shell.
        // Cross-pane RPC: each pane needs to know (a) which surface it
        // is and (b) where to reach the host so a script inside one
        // pane can drive another via kasaterm-cli. CommandBuilder
        // inherits the parent env by default, but make these two
        // explicit so removing the inherit later doesn't silently
        // break the integration.
        cmd.env("KASATERM_PANE_ID", &opts.pane_id);
        if let Ok(sock) = std::env::var("KASATERM_SOCKET_PATH") {
            cmd.env("KASATERM_SOCKET_PATH", sock);
        }
        // Caller-supplied env overrides everything above so tests /
        // callers can still inject a synthetic TERM if they need to.
        for (k, v) in &opts.env {
            cmd.env(k, v);
        }
        let child = pair
            .slave
            .spawn_command(cmd)
            .context("spawn shell into PTY")?;
        let shell_pid = child.process_id();
        // We drop the slave half — the spawned child holds the only
        // fd we care about. Keeping it open in our process makes
        // close-detection unreliable.
        drop(pair.slave);
        // Master knows the slave's tty path (e.g. /dev/ttys011) — Terminal.app
        // shows this as the trailing "on ttysNNN" of its Last login line and
        // we want to mirror that. Only available on unix; None on Windows.
        #[cfg(unix)]
        let tty_short = pair
            .master
            .tty_name()
            .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()));
        #[cfg(not(unix))]
        let tty_short: Option<String> = None;

        // poll 기반 정지에 쓸 master fd — Arc 로 싸기 전에 떠 둔다(핸드오프).
        #[cfg(unix)]
        let poll_fd = pair.master.as_raw_fd().map(|f| f as i32);
        #[cfg(not(unix))]
        let poll_fd: Option<i32> = None;
        let reader = pair.master.try_clone_reader().context("clone reader")?;
        let writer = pair
            .master
            .take_writer()
            .context("take writer")?;
        let master = Arc::new(Mutex::new(pair.master));

        let wiring = Wiring::new(&opts, writer, true);
        // Seed restored scrollback into alacritty before the shell's first
        // output, so scroll-up shows the pre-restart screen content.
        wiring.seed(&opts.initial_scrollback);
        // Mimic Terminal.app's "Last login: …" banner. login(1) writes this
        // by reading ~/.lastlogin and updating it after spawn; we keep our
        // own state file (no setuid login wrapper involved) and inject the
        // line straight into the VT grid before the reader thread starts —
        // same pattern as initial_scrollback above. We only show it when a
        // previous timestamp exists, so a brand-new install doesn't get a
        // bare "Last login: on ttysNNN" line.
        if let Some(line) = host
            .last_login_dir
            .as_deref()
            .and_then(|dir| build_last_login_line(dir, tty_short.as_deref()))
        {
            wiring.seed(std::slice::from_ref(&line));
        }
        Ok(wiring.launch(
            &opts,
            Launch {
                reader,
                poll_fd,
                parsed_generation: None,
                io: SessionIo::Local { master, child: Arc::new(Mutex::new(child)) },
                shell_pid,
                tty_short,
            },
        ))
    }

    /// 원격 호스트가 소유한 PTY 의 **로컬 파서 세션**을 만든다.
    ///
    /// `start` 와 배관(파서·스냅샷·tap·scrollback)이 같고 다른 것은 전송뿐이다 —
    /// 바이트는 `io.events` 로 들어오고, 입력은 `io.writer` 로 나가며, resize 는
    /// `io.on_resize` 로 원격에 알린다. `opts` 의 shell/env/initial_scrollback 은
    /// 원격 호스트 소관이라 여기선 무시된다. shell_pid 가 None 이라 ps 기반
    /// 판정(active_agent 등)은 우아하게 비활성이다.
    pub fn start_external(opts: PtyOptions, io: ExternalIo) -> Result<Self> {
        let wiring = Wiring::new(&opts, io.writer, false);
        let parsed_generation = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let reader = Box::new(ExtReader {
            generation: 0,
            parsed_generation: parsed_generation.clone(),
            events: io.events,
            size: Arc::clone(&wiring.size),
            pending: Vec::new(),
        });
        // ExtReader 는 채널이라 poll 대상이 없다 — 정지는 채널 닫힘이 대신한다.
        Ok(wiring.launch(
            &opts,
            Launch {
                reader,
                poll_fd: None,
                parsed_generation: Some(parsed_generation),
                io: SessionIo::External { on_resize: io.on_resize },
                shell_pid: None,
                tty_short: None,
            },
        ))
    }

    /// 다른 프로세스가 띄운 PTY 를 **산 채로** 입양한다 — 무중단 핸드오프의 받는 쪽.
    ///
    /// `fd` 는 SCM_RIGHTS 로 건너온 master. reader/writer 는 dup 로 가르고,
    /// resize 는 TIOCSWINSZ, Drop 은 kill(child_pid). 넘긴 쪽의 화면·스크롤백은
    /// `opts.initial_scrollback` 으로 이어받는다(start 와 같은 텍스트 재생 경로).
    /// **넘기는 쪽이 `stop_reader` 로 자기 reader 를 먼저 세우고** 보내야 출력이
    /// 두 소비자에게 갈라지지 않는다. 정지 순간 escape 시퀀스가 반 토막 나는
    /// 창이 이론상 있지만 TUI 는 계속 다시 그리므로 스스로 아문다.
    #[cfg(unix)]
    pub fn adopt(
        opts: PtyOptions,
        fd: std::os::fd::OwnedFd,
        child_pid: Option<u32>,
    ) -> Result<Self> {
        use std::os::fd::{AsRawFd, FromRawFd};
        let raw = fd.as_raw_fd();
        let rdup = unsafe { libc::dup(raw) };
        anyhow::ensure!(rdup >= 0, "dup(reader) 실패");
        let reader: Box<dyn Read + Send> =
            Box::new(unsafe { std::fs::File::from_raw_fd(rdup) });
        let wdup = unsafe { libc::dup(raw) };
        anyhow::ensure!(wdup >= 0, "dup(writer) 실패");
        let writer: Box<dyn Write + Send> =
            Box::new(unsafe { std::fs::File::from_raw_fd(wdup) });
        // 입양자가 이제 유일한 호스트다 — 자동 응답도 이쪽 몫.
        let wiring = Wiring::new(&opts, writer, true);
        wiring.seed(&opts.initial_scrollback);
        Ok(wiring.launch(
            &opts,
            Launch {
                reader,
                poll_fd: Some(raw),
                parsed_generation: None,
                io: SessionIo::Adopted { fd, child_pid },
                shell_pid: child_pid,
                tty_short: None,
            },
        ))
    }

    /// reader 스레드를 세운다(EOF 센티널 없이 조용히 퇴장). 핸드오프 직전 필수 —
    /// 다음 poll 티크(≤250ms) 안에 물러난다. 세운 뒤 400ms 쉬고 스크롤백을 떠야
    /// 마지막 청크까지 로컬 Term 에 담긴 채 넘어간다.
    pub fn stop_reader(&self) {
        self.reader_stop
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Drop 의 child kill 을 해제한다 — 핸드오프로 소유권이 밖으로 나간 껍데기용.
    pub fn disarm_kill(&self) {
        self.kill_disarmed
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// master fd(핸드오프 송신용). Local/Adopted 만 Some.
    #[cfg(unix)]
    pub fn master_raw_fd(&self) -> Option<i32> {
        match &self.io {
            SessionIo::Local { master, .. } => {
                master.lock().unwrap().as_raw_fd().map(|f| f as i32)
            }
            SessionIo::External { .. } => None,
            SessionIo::Adopted { fd, .. } => {
                use std::os::fd::AsRawFd;
                Some(fd.as_raw_fd())
            }
        }
    }

    /// Explicit close cannot rely on the last Arc disappearing: HTTP viewers
    /// and pending writes may still hold it. Only terminate our local child tree.
    pub fn terminate_local(&self) {
        if self.kill_disarmed.load(std::sync::atomic::Ordering::Acquire) { return; }
        let SessionIo::Local { child, .. } = &self.io else { return };
        let Ok(mut child) = child.lock() else { return };
        if !matches!(child.try_wait(), Ok(None)) { return; }
        if let Some(root) = child.process_id().filter(|pid| *pid > 1) {
            #[cfg(unix)]
            {
                // Fresh parent links, rooted in the still-owned child handle.
                // Never select processes by an executable name or shared tty.
                let table = process_table_raw();
                let mut owned = vec![root];
                let mut i = 0;
                while i < owned.len() {
                    let parent = owned[i];
                    for (pid, ppid, _) in &table {
                        if *ppid == parent && *pid > 1 && !owned.contains(pid) { owned.push(*pid); }
                    }
                    i += 1;
                }
                for pid in owned.into_iter().rev() { unsafe { libc::kill(pid as i32, libc::SIGKILL); } }
            }
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                let _ = std::process::Command::new("taskkill").args(["/PID", &root.to_string(), "/T", "/F"])
                    .creation_flags(0x08000000).output();
            }
        }
        let _ = child.kill();
    }
}

impl Drop for PtySession {
    /// A pane close drops its `Arc<PtySession>`; the final drop must guarantee
    /// the shell actually dies. Closing the PTY master *should* SIGHUP the
    /// child, but the master is `Arc`-shared (the reader thread holds a clone)
    /// and can outlive this drop, so the hangup may never land — leaving a
    /// zombie shell. Kill the child explicitly so a closed pane is always
    /// fully reaped.
    fn drop(&mut self) {
        // 핸드오프로 소유권이 나갔다 — 이 껍데기가 죽어도 셸은 남의 것이다.
        if self.kill_disarmed.load(std::sync::atomic::Ordering::Relaxed) {
            // ⚠️ portable-pty 의 UnixMasterWriter 는 Drop 에서 `\n`+EOF(ctrl-D) 를
            // pty 에 밀어 넣는다 — 산 채로 넘긴 셸이 그걸 받으면 프롬프트에서 그대로
            // 종료된다(실측: 핸드오프 직후 EIO 로 확정). Arc 클론 하나를 영원히
            // 잊어 그 Drop 이 영영 안 돌게 한다. 비용은 핸드오프당 fd 하나 누수.
            std::mem::forget(Arc::clone(&self.writer));
            return;
        }
        match &self.io {
            SessionIo::Local { child, .. } => {
                // 죽이고 거두는 일은 뒤 스레드에서 한다. 칸 출력을 아무도 안 빼면(읽기 스레드가
                // 죽은 칸) 끝나는 셸이 tty 를 닫으며 남은 출력이 빠지기를 커널에서 기다리는데,
                // 그 기다림은 master 가 닫혀야 풀리고 master 는 이 drop 이 끝난 뒤에야 닫힌다.
                // 여기서 wait 하면 영영 안 돌아와 메인 스레드가 묶였다(2026-10-07 「응답 없음」
                // 두 번: close_pane → Drop → wait4). 안 거두면 좀비가 남으므로(2026-09-22)
                // 거두기는 그대로 한다.
                let child = Arc::clone(child);
                let _ = std::thread::Builder::new().name("pty-reap".into()).spawn(move || {
                    if let Ok(mut child) = child.lock() {
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                });
            }
            // External: 원격 세션은 detach 로 살아남는 것이 목적이다 — 정말 죽일
            // 때는 호출자가 제어 메시지(kill)를 원격에 보낸다. 전송 스레드는
            // writer/이벤트 채널이 닫히면 스스로 끝난다.
            SessionIo::External { .. } => {}
            #[cfg(unix)]
            SessionIo::Adopted { child_pid, .. } => {
                if let Some(pid) = child_pid {
                    unsafe {
                        let _ = libc::kill(*pid as i32, libc::SIGHUP);
                    }
                }
            }
        }
    }
}

/// Injected into PowerShell (`pwsh` / `powershell`) via `-Command` so it reports
/// its cwd over OSC 9;9 on every prompt, wrapping any profile-defined prompt.
/// Single-quoted throughout (no `"`) so Windows argv quoting stays trivial; the
/// `\` inside `'\'` is the literal ST terminator byte that closes the OSC.
/// 두 번째 줄이 `claude` 를 shim 으로 되돌린다. PowerShell 은 **함수가 PATH 조회를
/// 이겨서**, 사용자 프로필에 `function claude { & claude.exe ... }` 가 있으면 PATH 맨
/// 앞의 shim(`claude.cmd`)이 통째로 우회된다. 그 래퍼가 붙이던 `--session-id` 와
/// `--settings` 가 함께 사라지고, 그러면 세션 id 를 채우는 두 경로(argv 스캔·
/// bind-transcript 훅)가 같이 죽어 `session.json` 의 `session_id` 가 영영 null 이 된다
/// — 재시작은 「claude 였다」는 것만 알고 어느 대화인지 몰라 빈 셸을 띄운다
/// (2026-09-01 Windows 실측: 저장본 5벌 전부 sid 0개).
///
/// `-Command` 는 프로필 로드 **뒤**에 돌아서 여기서 다시 정의하면 프로필을 이긴다.
/// 프로필이 붙이던 플래그(`--dangerously-skip-permissions` 등)는 정의 문자열에서 뽑아
/// 승계한다 — 값을 받는 플래그(`--model opus`)는 값까지 옮기지 못하는 한계가 있다.
/// `__ktcs` 마커로 재진입을 막아 두 번 돌아도 승계한 플래그를 잃지 않는다.
/// 앱 밖(shim 디렉터리 없음)에서는 아무것도 하지 않는다.
const PWSH_CWD_SHIM: &str = concat!(
    "$__ktp=$function:prompt; function global:prompt { $l=$ExecutionContext.SessionState.Path.CurrentLocation; if($l -and $l.Provider.Name -eq 'FileSystem'){[Console]::Write([char]27+']9;9;'+$l.ProviderPath+[char]27+'\\')}; if($__ktp){& $__ktp}else{'PS '+$PWD.Path+'> '} }",
    "; if($env:KASATERM_TMUX_SHIM_DIR){$__kts=Join-Path $env:KASATERM_TMUX_SHIM_DIR 'claude.cmd'; $__ktq=(Test-Path function:claude) -and (\"$function:claude\" -match '__ktcs'); if((Test-Path $__kts) -and (-not $__ktq)){$__ktf=@(); if(Test-Path function:claude){$__ktf=@([regex]::Matches(\"$function:claude\",'--[a-z][a-z0-9-]*')|ForEach-Object{$_.Value}|Select-Object -Unique)}; $global:__ktcs=$__kts; $global:__ktcf=$__ktf; function global:claude { $a=$global:__ktcf; & $global:__ktcs @a @args }}}",
);

/// Build the "Last login: <time> on <tty>" banner Terminal.app shows.
/// Returns None on first ever spawn (no stored timestamp) or when we
/// couldn't resolve a tty name — both cases would render as an
/// awkward partial line.
///
/// State lives at `<dir>/last_login` as one line of
/// pre-formatted text (e.g. "Tue May 26 13:05:54"). We re-emit the
/// *previous* contents and overwrite with `date(1)`-formatted "now"
/// so the next spawn sees this run's timestamp. `dir` 는 호스트가 정한다
/// (`HostPolicy::last_login_dir`).
fn build_last_login_line(dir: &std::path::Path, tty: Option<&str>) -> Option<String> {
    let tty = tty?;
    let path = dir.join("last_login");
    let previous = std::fs::read_to_string(&path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    // Shell out to date(1) — saves pulling chrono/time into the
    // workspace just for one strftime call. Format matches what
    // Terminal.app writes ("%a %b %e %H:%M:%S").
    let now = std::process::Command::new("date")
        .args(["+%a %b %e %H:%M:%S"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if let Some(now) = &now {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(&path, now);
    }
    previous.map(|p| format!("Last login: {p} on {tty}"))
}

/// 테스트가 띄우는 POSIX 셸. 재려는 것은 셸이 아니라 **PTY** 라, 플랫폼마다 셸만
/// 갈아 끼우면 검증은 그대로 성립한다 — `/bin/sh` 를 박아 두면 Windows 에서
/// `CreateProcessW` 가 「지정된 경로를 찾을 수 없습니다」로 죽는다(2026-08-31 실측,
/// 이 크레이트에서 7개가 그렇게 넘어졌다). Windows 는 Git for Windows 가 같은 셸을
/// 동봉하고, GitHub Actions 의 windows 러너에도 Git 이 기본으로 깔려 있다.
///
/// 못 찾으면 건너뛰지 않고 **죽인다.** 조용히 넘기면 「초록인데 아무것도 안 잰 CI」가
/// 되어, 정작 ConPTY 가 깨진 날에도 아무도 모른다.
///
/// `cfg!` 로 가르는 것은 양쪽 갈래가 다 컴파일되게 하려는 것이다 — `#[cfg]` 로 꺼
/// 두면 맥에서 Windows 갈래의 오타가 영영 안 잡힌다(이 레포가 반복해 밟은 함정).
#[cfg(test)]
mod missing_cwd_tests {
    use super::*;

    #[test]
    fn missing_cwd_falls_back_to_home() {
        let s = PtySession::start(PtyOptions {
            shell: Some(test_posix_shell()),
            cwd: Some("/nonexistent-kasaterm-cwd-xyz".into()),
            cols: 80,
            rows: 10,
            pane_id: "%cwd".into(),
            ..Default::default()
        })
        .expect("없는 폴더여도 셸은 떠야 한다");
        s.send_bytes(b"pwd\n").unwrap();
        let home = std::env::var("HOME").unwrap();
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        while Instant::now() < deadline {
            if s.visible_text(10).contains(&home) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("홈으로 안 떨어졌다: {}", s.visible_text(10));
    }
}

#[cfg(test)]
mod external_session_tests {
    use super::*;

    #[test]
    fn restoration_generation_waits_for_the_last_fragment() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let parsed = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let mut reader = ExtReader { events: rx, size: Arc::new(Mutex::new((80, 24))),
            pending: Vec::new(), generation: 0, parsed_generation: parsed.clone() };
        tx.send(ExtEvent::Generation(1)).unwrap();
        tx.send(ExtEvent::Bytes(b"firstframe".to_vec())).unwrap();
        let mut small = [0u8; 5];
        assert_eq!(reader.read(&mut small).unwrap(), 5);
        assert_eq!(parsed.load(std::sync::atomic::Ordering::Acquire), 0);
        reader.read(&mut small).unwrap();
        assert_eq!(parsed.load(std::sync::atomic::Ordering::Acquire), 1);
        tx.send(ExtEvent::Generation(2)).unwrap();
        tx.send(ExtEvent::Bytes(b"secondframe".to_vec())).unwrap();
        reader.read(&mut small).unwrap();
        assert_eq!(parsed.load(std::sync::atomic::Ordering::Acquire), 1, "stale frame must not claim new connection readiness");
        reader.read(&mut small).unwrap();
        reader.read(&mut small).unwrap();
        assert_eq!(parsed.load(std::sync::atomic::Ordering::Acquire), 2);
    }

    #[test]
    fn restoration_live_frame_is_parsed_but_history_and_resize_are_not() {
        let (session, events, _, _) = ext_session(20, 5);
        assert!(!session.full_snapshot().live_output);
        events.send(ExtEvent::Generation(7)).unwrap();
        events.send(ExtEvent::Bytes(b"LIVE".to_vec())).unwrap();
        let update = session.screens.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert!(update.live_output);
        assert_eq!(update.output_generation, 7);
        assert!(session.visible_text(5).contains("LIVE"));
        session.resize(30, 8).unwrap();
        let update = session.screens.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert!(!update.live_output);
        assert_eq!(update.output_generation, 0);
    }

    #[test]
    fn dropping_a_pane_whose_output_nobody_reads_returns_at_once() {
        let sess = PtySession::start(PtyOptions {
            pane_id: format!("undrained-{}", std::process::id()),
            shell: Some("/bin/sh".into()),
            cols: 80,
            rows: 24,
            ..Default::default()
        })
        .unwrap();
        // 읽기 스레드가 패닉으로 죽은 칸과 같은 자리 — 출력을 아무도 안 뺀다.
        sess.stop_reader();
        sess.send_bytes(b"echo x\n").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        sess.send_bytes(b"yes\n").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(500));
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            drop(sess);
            let _ = tx.send(());
        });
        rx.recv_timeout(std::time::Duration::from_secs(3))
            .expect("Drop 이 끝나지 못하는 셸을 기다리며 부른 스레드를 붙들었다");
    }

    #[test]
    fn external_bytes_land_in_local_grid_and_input_goes_to_writer() {
        let (sess, etx, wrx, resized) = ext_session(20, 5);
        etx.send(ExtEvent::Bytes(b"hello".to_vec())).unwrap();
        assert!(wait_text(&sess, "hello"), "원격 바이트가 로컬 그리드에 실려야 한다");
        // 입력은 writer(원격 송신로)로 나간다.
        sess.send_bytes(b"ls\r").unwrap();
        assert_eq!(
            wrx.recv_timeout(std::time::Duration::from_secs(1)).unwrap(),
            b"ls\r".to_vec()
        );
        // resize 는 ioctl 이 아니라 콜백으로 나가고, 로컬 격자는 낙관 적용된다.
        sess.resize(30, 6).unwrap();
        assert_eq!(resized.lock().unwrap().as_slice(), &[(30, 6)]);
        assert_eq!(sess.size(), (30, 6));
    }

    #[test]
    fn external_eof_emits_reap_sentinel() {
        let (sess, etx, _wrx, _resized) = ext_session(20, 5);
        etx.send(ExtEvent::Bytes(b"x".to_vec())).unwrap();
        etx.send(ExtEvent::Eof).unwrap();
        let deadline = Instant::now() + std::time::Duration::from_secs(2);
        let mut saw_eof = false;
        while Instant::now() < deadline {
            match sess.screens.recv_timeout(std::time::Duration::from_millis(200)) {
                Ok(u) if u.eof => {
                    saw_eof = true;
                    break;
                }
                Ok(_) => continue,
                Err(_) => continue,
            }
        }
        assert!(saw_eof, "Eof 이벤트는 eof 센티널 프레임이 되어야 한다");
    }

    #[test]
    fn external_reconnect_ris_clears_history() {
        // 재접속 시나리오: RIS(ESC c) 한 방이 화면과 스크롤백을 모두 비워, 이어지는
        // 스냅샷 재생이 중복 없이 상태를 다시 세운다(alacritty Grid::reset 이
        // clear_history 를 부르는 것에 기댄다 — 이 테스트가 그 계약의 회귀 감시다).
        let (sess, etx, _wrx, _resized) = ext_session(20, 5);
        let mut long = Vec::new();
        for i in 0..30 {
            long.extend_from_slice(format!("line{i}\r\n").as_bytes());
        }
        etx.send(ExtEvent::Bytes(long)).unwrap();
        assert!(wait_text(&sess, "line29"));
        assert!(sess.view_state().1 > 0, "스크롤백이 쌓여 있어야 전제 성립");
        etx.send(ExtEvent::Bytes(b"\x1bcfresh".to_vec())).unwrap();
        assert!(wait_text(&sess, "fresh"));
        assert_eq!(sess.view_state().1, 0, "RIS 뒤 히스토리는 0 이어야 한다");
    }
}

#[cfg(all(test, unix))]
mod handoff_tests {
    use super::*;
    use std::os::fd::FromRawFd;

    fn wait_contains(s: &PtySession, needle: &str) -> bool {
        let deadline = Instant::now() + std::time::Duration::from_secs(6);
        while Instant::now() < deadline {
            if s.visible_text(40).contains(needle) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        false
    }

    #[test]
    fn close_terminates_owned_process_even_with_a_retained_viewer_arc() {
        let source = Arc::new(PtySession::start(PtyOptions {
            shell: Some("/bin/sh".into()), pane_id: "close-grace-owned".into(),
            ..Default::default()
        }).unwrap());
        let retained_viewer = source.clone();
        source.set_input_closed(true, true).unwrap();
        source.terminate_local();
        let SessionIo::Local { child, .. } = &source.io else { panic!("expected owned child") };
        let end = Instant::now() + std::time::Duration::from_secs(2);
        while child.lock().unwrap().try_wait().unwrap().is_none() && Instant::now() < end {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(child.lock().unwrap().try_wait().unwrap().is_some());
        assert!(retained_viewer.send_bytes(b"late work").is_err());
    }

    /// 핸드오프 전 구간: 산 셸의 fd 를 다른 세션이 입양해도 셸이 재시작되지 않고
    /// (변수 기억 유지), 껍데기 drop 은 무해하며, 입양자 drop 만 셸을 죽인다.
    #[test]
    fn adopt_takes_over_live_shell_without_restart() {
        let a = PtySession::start(PtyOptions {
            cols: 60,
            rows: 12,
            pane_id: "hand-a".into(),
            ..Default::default()
        })
        .expect("start");
        a.send_bytes(b"MARK=alive-42; echo ready-$MARK\r").unwrap();
        assert!(wait_contains(&a, "ready-alive-42"), "셸 부팅/에코 실패");
        let pid = a.shell_pid().expect("pid");
        let raw = a.master_raw_fd().expect("fd");
        // 넘기기: reader 정지 → 마지막 청크가 Term 에 앉게 잠깐 → 스크롤백 뜨기
        a.stop_reader();
        std::thread::sleep(std::time::Duration::from_millis(400));
        let scroll = a.scrollback_text(200);
        let dup = unsafe { libc::dup(raw) };
        assert!(dup >= 0);
        let owned = unsafe { std::os::fd::OwnedFd::from_raw_fd(dup) };
        let b = PtySession::adopt(
            PtyOptions {
                cols: 60,
                rows: 12,
                pane_id: "hand-b".into(),
                initial_scrollback: scroll,
                ..Default::default()
            },
            owned,
            Some(pid),
        )
        .expect("adopt");
        a.disarm_kill();
        drop(a); // 껍데기 폐기 — 셸은 살아 있어야 한다
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(unsafe { libc::kill(pid as i32, 0) }, 0, "핸드오프 뒤 셸이 죽었다");
        // 입양자로 이어서 타이핑 — **같은** 셸이어야 변수를 기억한다
        b.send_bytes(b"echo again-$MARK\r").unwrap();
        assert!(wait_contains(&b, "again-alive-42"), "입양자 쪽 왕복 실패(다른 셸?)");
        // 스크롤백 이어받기
        assert!(
            b.scrollback_text(300).iter().any(|l| l.contains("ready-alive-42")),
            "이어받은 스크롤백에 이전 출력이 없다"
        );
        // 입양자 drop = 진짜 종료(SIGHUP). 부모는 이 테스트 프로세스라 waitpid 로 걷는다.
        drop(b);
        let mut reaped = false;
        for _ in 0..40 {
            let mut st = 0i32;
            let r = unsafe { libc::waitpid(pid as i32, &mut st, libc::WNOHANG) };
            if r == pid as i32 {
                reaped = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(reaped, "입양자를 버렸는데 셸이 안 죽었다");
    }
}
