//! `PtySession` — owns the PTY pair, the child shell process, the
//! alacritty_terminal VT state, and the threads that pump bytes in and
//! diffs out.
//!
//! Lifecycle: `PtySession::start(opts)` spawns the shell, kicks off a
//! reader thread that feeds bytes through `alacritty_terminal::Term`,
//! and exposes:
//!   - `screens: Receiver<ScreenUpdate>` — diffs the renderer consumes
//!   - `send_bytes(&[u8])` — write to the PTY (key input, paste, etc)
//!   - `resize(cols, rows)` — propagate window resize to the PTY +
//!     reshape the VT grid
//!
//! ScreenUpdate format matches tmux-bridge's so the renderer is happy
//! with either backend.

use std::time::Instant;

use alacritty_terminal::event::{Event as AlacEvent, EventListener, WindowSize};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::Point;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config as TermConfig, TermDamage};
use alacritty_terminal::vte::ansi::{Color as VtColor, NamedColor, Processor, Rgb, StdSyncHandler};
use alacritty_terminal::Term;
use anyhow::{Context, Result};
use crossbeam_channel::{bounded, Receiver, Sender};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use kasa_bridge::screen::{Cell, Color, Row, ScreenUpdate};

/// One shell command's lifecycle, delimited by OSC 133 `C` (output start)
/// and `D;<exit>` (command end). Accumulated by the reader thread off the
/// raw byte stream — vte drops OSC 133, so we sniff it like OSC 777/1337.
/// Exposed over the `/blocks` HTTP endpoint to render Warp-style command
/// blocks in the arona GUI. `output` is the raw C..D byte run (ANSI kept).
#[derive(Clone, Debug)]
pub struct CommandBlock {
    pub id: u64,
    pub command: String,
    pub output: String,
    /// None while the command is still running (C seen, D not yet).
    pub exit_code: Option<i32>,
    /// Epoch milliseconds at C (command start) — drives the HISTORY panel's
    /// relative timestamps ("just now" / "3 hours ago").
    pub started_ms: u64,
    /// Wall-clock C→D duration; None while running.
    pub duration_ms: Option<u64>,
    /// The command entered an alt-screen (vim/htop/less) — its raw output is
    /// not a clean block, so the GUI falls back to a live peek for it.
    pub is_tui: bool,
}

/// What to spawn in the PTY. Sticks close to portable-pty's
/// CommandBuilder so the user can override env / cwd without us
/// re-implementing a shell-spawn API.
#[derive(Debug, Clone)]
pub struct PtyOptions {
    pub shell: Option<String>,
    pub cwd: Option<String>,
    pub cols: u16,
    pub rows: u16,
    pub env: Vec<(String, String)>,
    /// Identifier this session stamps on every ScreenUpdate it emits.
    /// The renderer keys panes by this id, so a multi-pane workspace
    /// gives each PtySession a unique value ("%0", "%1", ...).
    pub pane_id: String,
    /// Scrollback to seed on start (oldest→newest text lines). Fed through the
    /// VT parser before the shell's first output so it lands in alacritty's
    /// scrollback and shows on scroll-up. Empty = fresh terminal. Restores a
    /// pane's pre-restart screen content across a relaunch.
    pub initial_scrollback: Vec<String>,
}

impl Default for PtyOptions {
    fn default() -> Self {
        Self {
            shell: None,
            cwd: None,
            cols: 80,
            rows: 24,
            env: Vec::new(),
            pane_id: "%0".to_string(),
            initial_scrollback: Vec::new(),
        }
    }
}

pub struct PtySession {
    /// Channel the renderer consumes — one ScreenUpdate per dirty
    /// frame after VT processing landed new state.
    pub screens: Receiver<ScreenUpdate>,
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    _child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
    /// Shared cell-dim state used by the resize path so we can reshape
    /// the VT grid without re-creating the Term.
    size: Arc<Mutex<(u16, u16)>>,
    /// Held so the renderer thread doesn't get GC'd; never read from
    /// after start().
    _reader_thread: std::thread::JoinHandle<()>,
    /// PID of the shell we spawned. We walk the process tree from
    /// here to find the active foreground command (vim, claude, etc.)
    /// so the pane header can label itself the way iTerm does — by
    /// running process rather than by OSC title.
    shell_pid: Option<u32>,
    /// (last_query_at, cached_name). Throttle the ps(1) shellout to
    /// ~500ms so a 60Hz render loop doesn't fork-exec on every frame.
    proc_cache: Arc<Mutex<(Instant, Option<String>)>>,
    /// (last_query_at, is_agents_view). `claude agents`(에이전트 목록 뷰) 여부를
    /// argv 로 판정한 캐시 — process_cmdline(ps) 비용을 proc_cache 와 같은 500ms
    /// 로 스로틀. agents 뷰면 render 가 학생 대신 샬레 로고를 그린다.
    agents_cache: Arc<Mutex<(Instant, bool)>>,
    /// Shared Term so `scroll()` can drive alacritty's own scrollback
    /// (display_offset) from the main thread and re-snapshot. Using
    /// alacritty's scrollback — instead of a hand-rolled shift
    /// detection — is what makes scroll-region TUIs (claude code's
    /// pinned input) scroll back correctly.
    term: Arc<Mutex<Term<PtyEventForwarder>>>,
    /// tx clone so `scroll()` can push the re-snapshot to the same
    /// channel the reader thread feeds.
    screens_tx: Sender<ScreenUpdate>,
    title_handle: Arc<Mutex<Option<String>>>,
    pane_id: String,
    /// The shell's controlling tty short name (e.g. "ttys004"), captured from
    /// the PTY master at spawn — what ghostty / Terminal.app surface. Immutable
    /// for the pane's life; None on Windows. Shown in the pane header.
    tty_short: Option<String>,
    /// Warp-style command blocks the reader thread accumulates from the raw
    /// OSC 133 C/D stream. Shared Arc so the socket/HTTP backend reads them
    /// without routing through the GUI. Bounded (~50), newest last.
    blocks: Arc<Mutex<VecDeque<CommandBlock>>>,
    /// Shell cwd reported via OSC 9;9 (`ESC]9;9;<path>ST`) — the path-only
    /// working-directory hint Windows Terminal / ConEmu use. The reader stashes
    /// it here so the header breadcrumb tracks PowerShell `cd`, which (unlike
    /// zsh/bash) never updates the process's real cwd. None until the injected
    /// shell integration emits its first prompt.
    cwd_handle: Arc<Mutex<Option<std::path::PathBuf>>>,
    /// raw PTY 바이트를 그대로 받아 가는 구독자들 — 브라우저의 xterm.js 처럼
    /// **자기 VT 파서를 가진** 소비자를 위한 tee. 여기로 흘리는 건 우리가 파싱한
    /// 셀이 아니라 셸이 뱉은 바이트 그 자체라, 받는 쪽이 kasaterm 내부 구조에
    /// 전혀 묶이지 않는다. `blocks` 와 같은 이유로 Arc 공유 — HTTP 백엔드가
    /// GUI 스레드를 거치지 않고 직접 붙는다.
    byte_taps: Arc<Mutex<Vec<Sender<Vec<u8>>>>>,
}

impl PtySession {
    pub fn start(opts: PtyOptions) -> Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: opts.rows,
                cols: opts.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
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
        if let Some(cwd) = opts.cwd.as_deref() {
            cmd.cwd(cwd);
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
        cmd.env("TERM_PROGRAM", "kasaterm");
        cmd.env(
            "TERM_PROGRAM_VERSION",
            env!("CARGO_PKG_VERSION"),
        );
        cmd.env("COLORTERM", "truecolor");
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

        let reader = pair.master.try_clone_reader().context("clone reader")?;
        let writer = pair
            .master
            .take_writer()
            .context("take writer")?;
        let master = Arc::new(Mutex::new(pair.master));

        let (tx, rx) = bounded::<ScreenUpdate>(256);
        let size = Arc::new(Mutex::new((opts.cols, opts.rows)));
        let writer_arc: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(writer));
        let blocks: Arc<Mutex<VecDeque<CommandBlock>>> = Arc::new(Mutex::new(VecDeque::new()));

        // Spin up the VT processor loop. Owns the Term, drains the
        // reader, and emits a ScreenUpdate after each batch. Bounded
        // channel + drop-on-full keeps us from buffering frames the
        // renderer is too slow to consume.
        let title_handle = Arc::new(Mutex::new(None));
        let cwd_handle: Arc<Mutex<Option<std::path::PathBuf>>> = Arc::new(Mutex::new(None));
        let listener = PtyEventForwarder {
            writer: Arc::clone(&writer_arc),
            size: Arc::clone(&size),
            last_title: Arc::clone(&title_handle),
        };
        let term = Arc::new(Mutex::new(make_term(opts.cols, opts.rows, listener)));
        // Seed restored scrollback into alacritty before the shell's first
        // output, so scroll-up shows the pre-restart screen content. Fed as if
        // it were program output (v1: plain text, no color/attrs).
        if !opts.initial_scrollback.is_empty() {
            let mut proc: Processor<StdSyncHandler> = Processor::new();
            let mut t = term.lock().unwrap();
            for line in &opts.initial_scrollback {
                proc.advance(&mut *t, line.as_bytes());
                proc.advance(&mut *t, b"\r\n");
            }
        }
        // Mimic Terminal.app's "Last login: …" banner. login(1) writes this
        // by reading ~/.lastlogin and updating it after spawn; we keep our
        // own state file (no setuid login wrapper involved) and inject the
        // line straight into the VT grid before the reader thread starts —
        // same pattern as initial_scrollback above. We only show it when a
        // previous timestamp exists, so a brand-new install doesn't get a
        // bare "Last login: on ttysNNN" line.
        if let Some(line) = build_last_login_line(tty_short.as_deref()) {
            let mut proc: Processor<StdSyncHandler> = Processor::new();
            let mut t = term.lock().unwrap();
            proc.advance(&mut *t, line.as_bytes());
            proc.advance(&mut *t, b"\r\n");
        }
        let byte_taps: Arc<Mutex<Vec<Sender<Vec<u8>>>>> = Arc::new(Mutex::new(Vec::new()));
        let reader_thread = spawn_reader_thread(
            reader,
            tx.clone(),
            opts.cols,
            opts.rows,
            size.clone(),
            opts.pane_id.clone(),
            Arc::clone(&title_handle),
            Arc::clone(&term),
            Arc::clone(&blocks),
            Arc::clone(&cwd_handle),
            Arc::clone(&byte_taps),
        );

        Ok(Self {
            screens: rx,
            master,
            writer: writer_arc,
            _child: Arc::new(Mutex::new(child)),
            size,
            _reader_thread: reader_thread,
            shell_pid,
            proc_cache: Arc::new(Mutex::new((
                Instant::now() - std::time::Duration::from_secs(1),
                None,
            ))),
            agents_cache: Arc::new(Mutex::new((
                Instant::now() - std::time::Duration::from_secs(1),
                false,
            ))),
            term,
            screens_tx: tx,
            byte_taps,
            title_handle,
            pane_id: opts.pane_id.clone(),
            tty_short,
            blocks,
            cwd_handle,
        })
    }

    /// Shared handle to this pane's command-block store. The GUI hands this Arc
    /// to the socket backend (via `pane_status_pub`) so `/blocks` can read the
    /// blocks without touching `App.pty` — no per-frame snapshot/clone.
    pub fn blocks_arc(&self) -> Arc<Mutex<VecDeque<CommandBlock>>> {
        Arc::clone(&self.blocks)
    }

    /// Best-effort label for what's running in this PTY *right now*.
    /// Returns the comm name of the most recently spawned child of
    /// our shell (typically the foreground command — vim, claude,
    /// less, …) or falls back to the shell's own comm. ps(1) is
    /// throttled to ~500ms so this is cheap to call from the render
    /// loop.
    /// The shell's process id (None if it failed to launch). Used to look up
    /// the active pane's cwd for the git panel.
    pub fn shell_pid(&self) -> Option<u32> {
        self.shell_pid
    }

    /// The shell's controlling tty short name (e.g. "ttys004"), or None on
    /// Windows. Shown in the pane header — mirrors ghostty / Terminal.app.
    pub fn tty(&self) -> Option<&str> {
        self.tty_short.as_deref()
    }

    /// The pane's current OSC 0/2 title (set by the inner program), or None.
    /// Mirrors the header label's first-priority source so the dock chip can
    /// show the same name the header does.
    pub fn osc_title(&self) -> Option<String> {
        self.title_handle.lock().ok().and_then(|t| t.clone())
    }
    /// The shell's last OSC 9;9-reported cwd, if shell integration is emitting
    /// it (injected PowerShell prompt). None for shells that don't — callers
    /// then fall back to reading the process cwd directly.
    pub fn reported_cwd(&self) -> Option<std::path::PathBuf> {
        self.cwd_handle.lock().ok().and_then(|c| c.clone())
    }

    pub fn active_process_name(&self) -> Option<String> {
        let pid = self.shell_pid?;
        let now = Instant::now();
        let mut cache = self.proc_cache.lock().ok()?;
        if now.duration_since(cache.0).as_millis() < 500 {
            return cache.1.clone();
        }
        cache.0 = now;
        // process_table() already returns bare exe names (no path), so the
        // shell row and the newest direct child are matched on pid/ppid alone.
        let table = process_table_shared();
        let pid = effective_shell_pid(&table, pid);
        let mut best_child: Option<(u32, String)> = None;
        let mut shell_comm: Option<String> = None;
        for (row_pid, row_ppid, name) in table.iter() {
            if *row_pid == pid {
                shell_comm = Some(name.clone());
            } else if *row_ppid == pid && best_child.as_ref().is_none_or(|(p, _)| *p < *row_pid) {
                best_child = Some((*row_pid, name.clone()));
            }
        }
        let resolved = best_child.map(|(_, n)| n).or(shell_comm).map(strip_exe_suffix);
        // Git bash 는 스크립트 실행 시 중간 프로세스가 죽어 부모 사슬이 영구
        // 단절된다(bash → [dead] → sh.exe → claude.exe, VM 실측) — ppid 하강
        // 으로는 못 잇는다. 사슬이 셸에서 끊겼으면 이 GUI 가 띄운 claude
        // (argv 의 --settings 경로에 kasaterm-shim-<GUI pid> 가 박힘)를 전역
        // 스캔하는 최후 폴백. pane 여러 개 중 일부만 claude 인 경우 셸-only
        // pane 도 claude 로 오판하는 알려진 한계 — 입력박스 색은 화면 패턴
        // (prompt_box_rows)이 걸러주고 pane 테두리만 드물게 오색.
        #[cfg(windows)]
        let resolved = {
            let shellish = resolved.as_deref().is_none_or(is_shell_exe);
            if shellish && orphan_claude_of_this_gui(&table) {
                Some("claude".to_string())
            } else {
                resolved
            }
        };
        cache.1 = resolved.clone();
        resolved
    }

    /// 이 pane 이 지금 돌리는 **에이전트 종류**. 셸이거나 다른 프로그램이면 None.
    ///
    /// 게이트를 이 하나로 모으는 이유: 예전엔 11곳이 `active_process_name()` 을 각자
    /// 보며 어떤 곳은 `== "claude"`, 어떤 곳은 `contains` 로 갈렸다. 종류가 둘이 되는
    /// 순간 그 사본들이 제각각 갈라진다 — 오늘만 사본 때문에 세 번 물렸다.
    ///
    /// ⚠️ **codex 는 이름만 봐선 절대 못 잡는다.** npm shim 이라 셸의 직속 자식이
    /// `node` 이고 진짜 바이너리는 **손자**다(실측):
    /// ```text
    /// 32387 ppid=셸    comm=node          args=node …/.npm-global/bin/codex
    /// 32410 ppid=32387 comm=…/bin/codex   ← 이것
    /// ```
    /// 그래서 직속 자식이 런처류(node·npm·sh…)면 한 세대 더 내려간다. 프로세스
    /// 테이블은 300ms 공유 캐시라 `ps` 추가 호출이 없다.
    pub fn active_agent(&self) -> Option<AgentKind> {
        let pid = self.shell_pid?;
        agent_for_shell(&process_table_shared(), pid)
    }

    /// `claude agents`(에이전트 목록 뷰)로 도는 pane 인지 — argv 서브커맨드로 판정.
    /// render 가 이 pane 에 개별 학생 대신 샬레 로고를 그릴지 결정한다. process_cmdline
    /// (ps) 은 비싸 active_process_name 과 같은 500ms 캐시. 대화(일반 claude/--resume)
    /// 는 argv 에 `agents` 가 없어 false → render 가 배정 학생을 그린다(실시간 전환).
    pub fn is_claude_agents(&self) -> bool {
        let Some(pid) = self.shell_pid else {
            return false;
        };
        let now = Instant::now();
        let Ok(mut cache) = self.agents_cache.lock() else {
            return false;
        };
        if now.duration_since(cache.0).as_millis() < 500 {
            return cache.1;
        }
        cache.0 = now;
        let val = claude_agents_argv(pid);
        cache.1 = val;
        val
    }

    /// True when the shell has a child process (a command/claude/build/editor is
    /// running) — the pane has "작업현황". False for a bare idle prompt. Lets a
    /// close decide between folding into the dock (busy → keep) and just closing
    /// (idle → no chip). One `ps` scan; called only on dock/close, not per frame.
    pub fn has_active_job(&self) -> bool {
        let Some(pid) = self.shell_pid else {
            return false;
        };
        let table = process_table_shared();
        let pid = effective_shell_pid(&table, pid);
        if table.iter().any(|(_, ppid, _)| *ppid == pid) {
            return true;
        }
        // 고아 사슬로 claude 가 트리에서 끊긴 pane 은 자식-없음=idle 로 오판돼
        // confirm 없이 닫힌다 — active_process_name 과 같은 폴백으로 방어.
        #[cfg(windows)]
        if orphan_claude_of_this_gui(&table) {
            return true;
        }
        false
    }

    pub fn send_bytes(&self, bytes: &[u8]) -> Result<()> {
        let mut w = self.writer.lock().unwrap();
        w.write_all(bytes).context("pty write")?;
        // Flush immediately. Without this, a one-shot write that isn't
        // followed by another (a committed Hangul syllable — the next
        // keystroke only updates the preedit overlay, not the PTY) sits
        // in the writer buffer until something else flushes it, so the
        // shell echoes "안" ~0.2s late and the user sees only the preedit
        // "ㄴ" until then. ASCII typing hid this because each keystroke's
        // write flushed the previous one.
        w.flush().context("pty flush")?;
        Ok(())
    }

    /// Scroll the view through alacritty's scrollback by `lines`
    /// (positive = toward older history / up, negative = toward the
    /// live tail / down). Re-snapshots immediately and pushes the
    /// frame so the renderer reflects the new position without waiting
    /// for PTY output — important for an idle TUI like claude. Returns
    /// the resulting display offset (0 = at the live bottom).
    pub fn scroll(&self, lines: i32) -> usize {
        let (cols, rows) = *self.size.lock().unwrap();
        let mut t = self.term.lock().unwrap();
        let before = t.grid().display_offset();
        t.scroll_display(alacritty_terminal::grid::Scroll::Delta(lines));
        let after = t.grid().display_offset();
        // Inertia at a scrollback boundary keeps firing scroll(±N) even
        // though the offset is clamped. Skipping the snapshot+send when
        // nothing moved lets the render thread answer a direction reverse
        // immediately instead of working through a queue of no-ops.
        if before == after {
            return after;
        }
        let update = snapshot(
            &mut t,
            cols,
            rows,
            &self.pane_id,
            &self.title_handle,
            true,
        );
        let _ = self.screens_tx.try_send(update);
        after
    }

    /// Read the live screen as plain text — the last `lines` visible rows,
    /// each with trailing blanks trimmed. Lets a sibling `peek` at what a
    /// pane is showing (a build log, an idle claude prompt) without focusing
    /// it. Reads the live area at offset 0, not the scrollback view.
    pub fn visible_text(&self, lines: usize) -> String {
        let t = self.term.lock().unwrap();
        let grid = t.grid();
        let cols = grid.columns();
        let total = grid.screen_lines();
        let take = lines.min(total);
        let start = total - take;
        let mut out = String::with_capacity(take * (cols + 1));
        for line in start..total {
            let mut row = String::with_capacity(cols);
            for c in 0..cols {
                let point = Point::new(
                    alacritty_terminal::index::Line(line as i32),
                    alacritty_terminal::index::Column(c),
                );
                let ch = grid[point].c;
                row.push(if ch == '\0' { ' ' } else { ch });
            }
            out.push_str(row.trim_end());
            out.push('\n');
        }
        out
    }

    /// Jump straight to the live tail (display offset 0).
    pub fn scroll_to_bottom(&self) {
        let (cols, rows) = *self.size.lock().unwrap();
        let mut t = self.term.lock().unwrap();
        t.scroll_display(alacritty_terminal::grid::Scroll::Bottom);
        let update = snapshot(
            &mut t,
            cols,
            rows,
            &self.pane_id,
            &self.title_handle,
            true,
        );
        let _ = self.screens_tx.try_send(update);
    }

    /// Build a full-grid ScreenUpdate (every row) without touching the live
    /// channel — the daemon calls this on attach to seed a freshly-connected
    /// GUI with the complete current screen before live dirty frames resume.
    pub fn full_snapshot(&self) -> ScreenUpdate {
        let (cols, rows) = *self.size.lock().unwrap();
        let mut t = self.term.lock().unwrap();
        snapshot(&mut t, cols, rows, &self.pane_id, &self.title_handle, true)
    }

    /// raw PTY 바이트 스트림을 구독한다. 받는 쪽이 자기 VT 파서를 갖고 있을 때
    /// 쓴다(브라우저 xterm.js). 돌려준 `Receiver` 를 떨어뜨리면 다음 read 때
    /// reader 가 알아서 걷어내므로 해지 API 가 따로 없다.
    ///
    /// 버퍼는 64청크 — 64KB read 기준 최악 4MB다. 소비가 이보다 밀리면 reader 가
    /// 이 구독을 끊는다(`spawn_reader_thread` 의 tee 주석 참고).
    /// 현재 PTY 격자 크기 `(cols, rows)`. 미러로 붙는 쪽이 자기 화면을 여기에
    /// 맞춰야 줄바꿈이 어긋나지 않는다.
    pub fn size(&self) -> (u16, u16) {
        *self.size.lock().unwrap()
    }
    pub fn tap_bytes(&self) -> Receiver<Vec<u8>> {
        let (tx, rx) = crossbeam_channel::bounded(64);
        self.byte_taps.lock().unwrap().push(tx);
        rx
    }
    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        // Kernel-side PTY first (child sees SIGWINCH).
        {
            let pty = self.master.lock().unwrap();
            pty.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("pty resize")?;
        }
        // Reshape the alacritty grid *here*, not lazily in the next reader
        // pass: snapshot() (incl. full_snapshot from the daemon) indexes the
        // grid by `size`, so a window where `size` is updated but the grid
        // isn't yet panics with an out-of-bounds column. Resize the Term then
        // publish `size` so any snapshot sees a grid that already matches.
        {
            let mut t = self.term.lock().unwrap();
            t.resize(TermSize::new(cols as usize, rows as usize));
        }
        *self.size.lock().unwrap() = (cols, rows);
        Ok(())
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
        if let Ok(mut child) = self._child.lock() {
            let _ = child.kill();
        }
    }
}

/// 호스트가 OSC 10/11/12 질의에 답할 색. `0x00RRGGBB` 로 담는다.
///
/// TUI 는 켤 때 이걸 한 번 물어 자기 테마(밝은 배경이냐 어두운 배경이냐)를 정한다
/// — Claude Code 의 `theme: auto` 가 그렇다. 그래서 **여기 답이 곧 그 결정**이다.
/// 예전엔 어두운 값이 박혀 있어, kasaterm 을 라이트 테마로 바꿔도 안에서 뜬 claude
/// 는 계속 자기가 어두운 터미널에 있는 줄 알았다.
///
/// crate 경계를 static 으로 넘는 건 방향 때문이다. 팔레트는 app 이 쥐고 있고
/// kasa-pty 는 app 을 의존하지 않는다(그 반대다) — 인자로 받으려면 PTY 생성 경로
/// 전체에 색을 실어 날라야 하는데, 정작 읽는 곳은 이 콜백 하나뿐이다.
pub static HOST_BG: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x252C35);
pub static HOST_FG: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0xFFFFFF);
pub static HOST_CURSOR: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(0xC5C8C6);

/// 테마가 바뀔 때마다 app 이 부른다. **이미 도는 TUI 는 안 바뀐다** — 질의는 시작할
/// 때 한 번뿐이라, 새로 뜨는 프로그램부터 적용된다.
pub fn set_host_colors(bg: (u8, u8, u8), fg: (u8, u8, u8), cursor: (u8, u8, u8)) {
    use std::sync::atomic::Ordering;
    let pack = |c: (u8, u8, u8)| (u32::from(c.0) << 16) | (u32::from(c.1) << 8) | u32::from(c.2);
    HOST_BG.store(pack(bg), Ordering::Relaxed);
    HOST_FG.store(pack(fg), Ordering::Relaxed);
    HOST_CURSOR.store(pack(cursor), Ordering::Relaxed);
}

fn host_rgb(cell: &std::sync::atomic::AtomicU32) -> (u8, u8, u8) {
    let v = cell.load(std::sync::atomic::Ordering::Relaxed);
    ((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

/// Bridges alacritty_terminal's `EventListener` callbacks back into
/// the PTY's input side. This is non-optional: terminals expect the
/// host to *reply* to a handful of control sequences, not just
/// passively render them. Without this, `\e[6n` (DSR-CPR) issued by
/// the shell on startup blocks waiting for a cursor-position report
/// and ConPTY-attached cmd.exe never reaches its first prompt.
///
/// We translate the events that carry a wire-format payload into
/// writes against the PTY master:
///   - PtyWrite — raw bytes alacritty already formatted
///   - ColorRequest — RGB query; reply with a fixed default
///   - TextAreaSizeRequest — geometry query; reply with current grid
///   - ClipboardLoad — paste request; reply with empty until we wire
///     real OS clipboard access through arboard
///
/// MouseCursorDirty / Title / Bell / etc are pure UI signals; the
/// renderer reads title/cursor state from the snapshot, so we drop
/// them here.
struct PtyEventForwarder {
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    size: Arc<Mutex<(u16, u16)>>,
    /// Latest OSC 0 / OSC 2 title pushed by the shell or any TUI
    /// running inside it. `None` after `ResetTitle` or until the
    /// first set. The reader thread reads this on each snapshot so
    /// the renderer's pane-header strip can reflect "✱ Claude Code",
    /// "vim filename", current cwd, etc. — anything the inner
    /// program decides to advertise.
    last_title: Arc<Mutex<Option<String>>>,
}

impl Clone for PtyEventForwarder {
    fn clone(&self) -> Self {
        Self {
            writer: Arc::clone(&self.writer),
            size: Arc::clone(&self.size),
            last_title: Arc::clone(&self.last_title),
        }
    }
}

impl PtyEventForwarder {
    fn write_to_pty(&self, bytes: &[u8]) {
        // Mirror outgoing replies into KASATERM_PTY_OUT_LOG so the
        // ghostty-vs-us escape diff can include OUR side of the
        // conversation (OSC 11 colour replies, TextAreaSize replies,
        // clipboard responses, etc) — not just what claude code sends.
        if let Ok(path) = std::env::var("KASATERM_PTY_OUT_LOG") {
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                use std::io::Write;
                let preview: String = bytes
                    .iter()
                    .take(2048)
                    .map(|b| match b {
                        0x20..=0x7e => (*b as char).to_string(),
                        b'\n' => "\\n".to_string(),
                        b'\r' => "\\r".to_string(),
                        b'\t' => "\\t".to_string(),
                        0x1b => "\\e".to_string(),
                        _ => format!("\\x{b:02x}"),
                    })
                    .collect();
                let _ = writeln!(file, "[out] {} bytes: {}", bytes.len(), preview);
            }
        }
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.write_all(bytes);
        }
    }
}

impl EventListener for PtyEventForwarder {
    fn send_event(&self, event: AlacEvent) {
        match event {
            AlacEvent::PtyWrite(s) => self.write_to_pty(s.as_bytes()),
            AlacEvent::ColorRequest(index, formatter) => {
                // Reply with values that match ghostty's defaults so
                // that Claude Code / other TUIs which probe the host
                // via OSC 10 (fg), OSC 11 (bg), OSC 12 (cursor), or
                // OSC 4;N (palette) make the same theme decisions
                // they make under ghostty. The old "black for
                // everything" answer caused Claude Code to treat us
                // as a near-black or unknown background and pick a
                // muted red palette (215,95,95 source instead of
                // ghostty's 220,38,39).
                //
                // NamedColor::Foreground = 256, Background = 257,
                // Cursor = 258 (per vte::ansi::NamedColor); 0-15 are
                // the ANSI base palette, 16-255 the xterm cube + gray
                // ramp.
                let rgb = match index {
                    // ANSI 16-base palette, ghostty default theme.
                    0 => (0x1D, 0x1F, 0x21),
                    1 => (0xCC, 0x66, 0x66),
                    2 => (0xB5, 0xBD, 0x68),
                    3 => (0xF0, 0xC6, 0x74),
                    4 => (0x81, 0xA2, 0xBE),
                    5 => (0xB2, 0x94, 0xBB),
                    6 => (0x8A, 0xBE, 0xB7),
                    7 => (0xC5, 0xC8, 0xC6),
                    8 => (0x66, 0x66, 0x66),
                    9 => (0xD5, 0x4E, 0x53),
                    10 => (0xB9, 0xCA, 0x4A),
                    11 => (0xE7, 0xC5, 0x47),
                    12 => (0x7A, 0xA6, 0xDA),
                    13 => (0xC3, 0x97, 0xD8),
                    14 => (0x70, 0xC0, 0xB1),
                    15 => (0xEA, 0xEA, 0xEA),
                    // 256/257/258 = Foreground/Background/Cursor. 이 셋만은
                    // 고정값이 아니라 **지금 화면에 실제로 깔린 색**을 답한다 —
                    // TUI 가 이 답으로 자기 테마를 고르므로(Claude Code 의
                    // `theme: auto`), 어두운 값을 박아 두면 라이트 테마로 바꿔도
                    // 안에서는 계속 어두운 터미널인 줄 안다. 나머지 ANSI 16색을
                    // ghostty 기본값으로 두는 건 그대로다: 그건 셀 팔레트라
                    // 우리가 이미 렌더 단계에서 테마에 맞춰 다시 칠한다.
                    256 => host_rgb(&HOST_FG),
                    257 => host_rgb(&HOST_BG),
                    258 => host_rgb(&HOST_CURSOR),
                    // 16-255: xterm 6×6×6 cube + 24-step gray ramp,
                    // identical to ghostty's hardcoded fallback.
                    n if n >= 16 && n < 232 => {
                        let n = n - 16;
                        let steps = [0u8, 95, 135, 175, 215, 255];
                        (steps[n / 36], steps[(n / 6) % 6], steps[n % 6])
                    }
                    n if n >= 232 && n < 256 => {
                        let v = 8 + ((n - 232) as u8) * 10;
                        (v, v, v)
                    }
                    // Any other index (dim variants, etc): fall back
                    // to a sensible neutral grey.
                    _ => (0x66, 0x66, 0x66),
                };
                let reply = formatter(Rgb { r: rgb.0, g: rgb.1, b: rgb.2 });
                self.write_to_pty(reply.as_bytes());
            }
            AlacEvent::TextAreaSizeRequest(formatter) => {
                let (cols, rows) = *self.size.lock().unwrap();
                let reply = formatter(WindowSize {
                    num_lines: rows,
                    num_cols: cols,
                    cell_width: 7,
                    cell_height: 16,
                });
                self.write_to_pty(reply.as_bytes());
            }
            AlacEvent::ClipboardLoad(_, formatter) => {
                // Read the OS clipboard and feed it back. Falls back
                // to empty so a clipboard-open failure doesn't strand
                // the shell waiting on a paste response.
                let text = arboard::Clipboard::new()
                    .ok()
                    .and_then(|mut cb| cb.get_text().ok())
                    .unwrap_or_default();
                let reply = formatter(&text);
                self.write_to_pty(reply.as_bytes());
            }
            AlacEvent::ClipboardStore(_, text) => {
                // OSC 52 set — Claude Code, helix, etc. push selected
                // text into the host clipboard through this. Best-
                // effort: a clipboard open failure is logged but does
                // not break the PTY.
                let preview: String = text.chars().take(40).collect();
                eprintln!(
                    "[pty-backend] OSC 52 set ({} chars): {preview:?}",
                    text.len()
                );
                match arboard::Clipboard::new() {
                    Ok(mut cb) => {
                        if let Err(e) = cb.set_text(text) {
                            eprintln!("[pty-backend] clipboard set failed: {e}");
                        }
                    }
                    Err(e) => eprintln!("[pty-backend] clipboard open failed: {e}"),
                }
            }
            AlacEvent::Title(name) => {
                eprintln!("[pty-backend] OSC title set: {name:?}");
                if let Ok(mut t) = self.last_title.lock() {
                    *t = Some(name);
                }
            }
            AlacEvent::ResetTitle => {
                if let Ok(mut t) = self.last_title.lock() {
                    *t = None;
                }
            }
            // UI hints with no PTY-side reply.
            AlacEvent::MouseCursorDirty
            | AlacEvent::CursorBlinkingChange
            | AlacEvent::Wakeup
            | AlacEvent::Bell
            | AlacEvent::Exit
            | AlacEvent::ChildExit(_) => {}
        }
    }
}

/// Local Dimensions impl. alacritty_terminal exposes the trait but
/// the concrete TermSize we want to pass lives behind a "test"
/// feature gate in some versions; this keeps us decoupled.
/// alacritty Cell = 24 bytes (EXPECTED_CELL_SIZE). Scrollback memory per pane
/// ≈ history_lines × cols × 24. A fixed line count therefore lets a wide
/// terminal silently use several times the RAM of a narrow one. Ghostty bounds
/// scrollback by *memory* instead — we mirror that: fix a byte budget and
/// derive the line cap from the current column width, recomputing on resize.
const SCROLLBACK_BYTES_PER_CELL: usize = 24;
const SCROLLBACK_MIN_LINES: usize = 1_000;
const SCROLLBACK_MAX_LINES: usize = 100_000;

/// 기본 예산. **줄 상한(`SCROLLBACK_MAX_LINES`)이 실질 기준이 되도록** 크게 잡는다 —
/// 1024MB 면 폭 1170칸까지 10만 줄을 다 받는다.
///
/// 크게 잡아도 되는 이유는 **캡이 예약이 아니라 상한이라서**다(실측 2026-08-06,
/// 363칸 pane): 캡 10만 줄에 1,964줄만 실으면 RSS 39MB, 61,624줄을 실제로 채우면
/// 742MB. 즉 안 쓰면 안 먹는다. 옛 기본값 16MB 는 **폭에 반비례**해서, 넓게 쓰는
/// pane 이 1,925줄밖에 못 남겼다 — 거노: "히스토리가 왜 다 안 남지, 보려고 위로
/// 올리면 없어져 있어". claude 한 세션이 몇 분이면 미는 양이다.
///
/// 대가는 **진짜로 10만 줄을 채운 pane** 이 1GB 를 쥔다는 것. RAM 이 아쉬우면
/// `KASATERM_SCROLLBACK_MB` 로 내린다.
const SCROLLBACK_DEFAULT_MB: usize = 1024;

fn scrollback_budget_bytes() -> usize {
    std::env::var("KASATERM_SCROLLBACK_MB")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|mb| *mb > 0)
        .unwrap_or(SCROLLBACK_DEFAULT_MB)
        * 1024
        * 1024
}

/// Line cap that keeps one pane's scrollback within the byte budget at the
/// given width. Clamped so a tiny width can't produce an absurd cap and a huge
/// width still keeps a usable floor.
fn history_lines_for_cols(cols: u16) -> usize {
    let per_line = (cols.max(1) as usize) * SCROLLBACK_BYTES_PER_CELL;
    (scrollback_budget_bytes() / per_line).clamp(SCROLLBACK_MIN_LINES, SCROLLBACK_MAX_LINES)
}

fn make_term(cols: u16, rows: u16, listener: PtyEventForwarder) -> Term<PtyEventForwarder> {
    let size = TermSize::new(cols as usize, rows as usize);
    let config = TermConfig {
        scrolling_history: history_lines_for_cols(cols),
        ..TermConfig::default()
    };
    Term::new(config, &size, listener)
}

/// 살아 있는 PTY 세션 레지스트리 — pane id → 세션.
///
/// 소유권은 GUI(`App.pty`)에 있고 여기엔 **Weak** 만 둔다. pane 이 닫히면 App 이
/// Arc 를 떨어뜨리는 것만으로 항목이 저절로 무효가 되므로, 해제를 잊어 유령
/// 세션이 남는 부류의 버그가 원천적으로 없다. HTTP·소켓 백엔드가 GUI 스레드를
/// 거치지 않고 세션에 직접 붙는 통로다.
fn registry() -> &'static Mutex<std::collections::HashMap<String, std::sync::Weak<PtySession>>> {
    static R: std::sync::OnceLock<
        Mutex<std::collections::HashMap<String, std::sync::Weak<PtySession>>>,
    > = std::sync::OnceLock::new();
    R.get_or_init(Default::default)
}

/// pane 을 띄운 쪽이 `Arc` 를 손에 넣은 직후 한 번 부른다.
pub fn register_session(id: &str, sess: &Arc<PtySession>) {
    registry()
        .lock()
        .unwrap()
        .insert(id.to_string(), Arc::downgrade(sess));
}

/// 살아 있으면 세션을 돌려준다. 이미 닫힌 pane 이면 `None`.
pub fn lookup_session(id: &str) -> Option<Arc<PtySession>> {
    registry().lock().unwrap().get(id)?.upgrade()
}

/// 지금 살아 있는 pane id 목록(정렬). 죽은 항목은 조회하는 김에 걷어낸다.
pub fn live_sessions() -> Vec<String> {
    let mut r = registry().lock().unwrap();
    r.retain(|_, w| w.strong_count() > 0);
    let mut ids: Vec<String> = r.keys().cloned().collect();
    ids.sort();
    ids
}

#[allow(clippy::too_many_arguments)]
fn spawn_reader_thread(
    mut reader: Box<dyn Read + Send>,
    tx: Sender<ScreenUpdate>,
    cols: u16,
    rows: u16,
    size: Arc<Mutex<(u16, u16)>>,
    pane_id: String,
    title_handle: Arc<Mutex<Option<String>>>,
    term: Arc<Mutex<Term<PtyEventForwarder>>>,
    blocks: Arc<Mutex<VecDeque<CommandBlock>>>,
    cwd_handle: Arc<Mutex<Option<std::path::PathBuf>>>,
    byte_taps: Arc<Mutex<Vec<Sender<Vec<u8>>>>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        use unicode_normalization::UnicodeNormalization;
        let mut processor: Processor<StdSyncHandler> = Processor::new();
        // 64KB matches the macOS PTY kernel buffer — one read drains a full
        // frame's worth of TUI output (ghostty / iTerm sized buffer). The
        // old 8KB forced 2-3 reads per claude-code frame, multiplying the
        // per-read snapshot cost by 2-3× and capping throughput at ~90 fps.
        let mut buf = [0u8; 65536];
        let mut current_size = (cols, rows);
        // Raw byte trace for diagnosing capability-detection differences
        // between us and ghostty. Set `KASATERM_PTY_LOG=/tmp/pty.log` and
        // each read appends `[pane_id] hex bytes\n` to that file. Open it
        // only when the env var is present so production runs pay nothing.
        let pty_log = std::env::var("KASATERM_PTY_LOG").ok().and_then(|path| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .ok()
        });
        let pty_log = std::sync::Mutex::new(pty_log);
        // Reassembles UTF-8 across read boundaries so NFC normalization
        // never sees a half codepoint (a multibyte char split between
        // two reads).
        let mut utf8_buf = Utf8Buffer::new();
        // OSC 1337 inline-image capture state (a payload can span reads).
        let mut img_buf: Vec<u8> = Vec::new();
        let mut img_capturing = false;
        // Kitty graphics protocol (APC `\x1b_G…\x1b\\`) capture state. One
        // image can arrive as multiple APC chunks linked by `m=1` / `m=0` —
        // `kitty_chunk_buf` is the current chunk's raw bytes, while
        // `kitty_payload_buf` accumulates the decoded body across chunks.
        let mut kitty_chunk_buf: Vec<u8> = Vec::new();
        let mut kitty_payload_buf: Vec<u8> = Vec::new();
        let mut kitty_capturing = false;
        // OSC 777 desktop-notification capture state (payload can span reads).
        let mut notify_buf: Vec<u8> = Vec::new();
        let mut notify_capturing = false;
        // Keeps a captured notify across frames so a sync-suppressed read
        // (sync_bytes_count > 0 → no snapshot built) still delivers next frame.
        let mut pending_notify: Option<(String, String)> = None;
        // OSC 133 C/D command-block state. C (output start) opens a block, the
        // raw bytes until D (command end) are its output, D;<exit> closes it.
        // `blk_prompt` = the B-mark cursor, where the command line begins —
        // captured from the grid at C time before output overwrites it.
        let mut blk_capturing = false;
        let mut blk_seq: u64 = 0;
        let mut blk_start: Option<Instant> = None;
        let mut blk_prompt: Option<(u16, u16)> = None;

        loop {
            // Check for a pending resize before we read more bytes —
            // a half-processed frame at the old size would land cells
            // out of bounds otherwise.
            let want = *size.lock().unwrap();
            if want != current_size {
                let s = TermSize::new(want.0 as usize, want.1 as usize);
                let mut t = term.lock().unwrap();
                t.resize(s);
                // Width changed → bytes-per-line changed → re-fit the line cap
                // to the byte budget so memory stays bounded across resizes.
                if want.0 != current_size.0 {
                    t.grid_mut().update_history(history_lines_for_cols(want.0));
                }
                drop(t);
                current_size = want;
            }
            let n = match reader.read(&mut buf) {
                Ok(0) => {
                    eprintln!("[pty-backend] EOF on PTY reader — shell exited");
                    // Tell the host pump the pane died. The PtySession also
                    // holds a Sender (for scroll/resize), so dropping our
                    // clone alone never closes the channel — the recv loop
                    // would block forever and the pane would linger as a
                    // zombie. An explicit eof sentinel reaps it instead.
                    let _ = tx.send(ScreenUpdate {
                        pane_id: pane_id.clone(),
                        eof: true,
                        ..Default::default()
                    });
                    return;
                }
                Ok(n) => n,
                Err(e) => {
                    eprintln!("[pty-backend] read error: {e}");
                    let _ = tx.send(ScreenUpdate {
                        pane_id: pane_id.clone(),
                        eof: true,
                        ..Default::default()
                    });
                    return;
                }
            };
            // 외부 구독자(브라우저 xterm.js 등)에게 raw 바이트를 그대로 흘린다.
            // 파싱 전 원본이라 받는 쪽은 자기 VT 파서로 독립적으로 그린다.
            //
            // ⚠️ 여기서 블로킹하면 아래 스냅샷 try_send 와 똑같은 병에 걸린다 —
            // reader 가 멎으면 셸이 backpressure 를 먹어 터미널 전체가 느려진다.
            // 그래서 try_send 이고, **밀린 구독자는 버리는 게 아니라 끊는다**:
            // VT 스트림은 연속이라 중간 청크를 흘리면 받는 쪽 화면이 복구 불능
            // 으로 깨진다. 조용히 깨뜨리느니 연결을 닫아 재연결시키는 편이 낫다.
            // 구독자가 없으면 lock 만 잡았다 놓으므로 평소 비용은 사실상 0.
            {
                let mut taps = byte_taps.lock().unwrap();
                if !taps.is_empty() {
                    taps.retain(|t| t.try_send(buf[..n].to_vec()).is_ok());
                }
            }
            // Append raw bytes (hex + escaped-printable preview) to the
            // KASATERM_PTY_LOG file so claude-code escape sequences can
            // be diffed against ghostty's `script` capture.
            if let Some(file) = pty_log.lock().unwrap().as_mut() {
                use std::io::Write;
                let preview: String = buf[..n.min(2048)]
                    .iter()
                    .map(|b| match b {
                        0x20..=0x7e => (*b as char).to_string(),
                        b'\n' => "\\n".to_string(),
                        b'\r' => "\\r".to_string(),
                        b'\t' => "\\t".to_string(),
                        0x1b => "\\e".to_string(),
                        _ => format!("\\x{b:02x}"),
                    })
                    .collect();
                let _ = writeln!(file, "[{}] {} bytes: {}", pane_id, n, preview);
            }
            if std::env::var("KASATERM_LOG_PTY").is_ok() {
                let preview: String = buf[..n.min(2048)]
                    .iter()
                    .map(|b| match b {
                        0x20..=0x7e => (*b as char).to_string(),
                        b'\n' => "\\n".to_string(),
                        b'\r' => "\\r".to_string(),
                        b'\t' => "\\t".to_string(),
                        0x1b => "\\e".to_string(),
                        _ => format!("\\x{b:02x}"),
                    })
                    .collect();
                eprintln!("[pty-backend] read {n} bytes: {preview}");
            }

            // NFC-normalize so decomposed Hangul (NFD jamo) collapses to
            // precomposed syllables before alacritty stores them. Pure-ASCII
            // batches (the common case — TUI rendering, ANSI control flow)
            // skip the normalize entirely; NFC is a no-op there but the
            // .nfc() iterator + String alloc still cost ~10us per read in
            // a hot loop. ASCII fast-path keeps the bytes borrowed.
            let raw_str = utf8_buf.process(&buf[..n]);
            let (nfc_holder, processed_bytes): (Option<String>, &[u8]) =
                if raw_str.is_ascii() {
                    (None, raw_str.as_bytes())
                } else {
                    let s: String = raw_str.nfc().collect();
                    (Some(s), &[])
                };
            let processed_bytes: &[u8] = match &nfc_holder {
                Some(s) => s.as_bytes(),
                None => processed_bytes,
            };
            // Sniff for iTerm OSC 1337 inline images / kitty graphics. Both
            // scans walk the byte slice, so we cheaply prefix-check first —
            // most reads have no `\x1b]1337` / `\x1b_G` and we skip the
            // walk entirely. Critical for TUI throughput (claude code emits
            // thousands of small reads per second with neither prefix).
            if img_capturing
                || memchr::memmem::find(processed_bytes, b"\x1b]1337").is_some()
            {
                scan_inline_image(processed_bytes, &mut img_buf, &mut img_capturing);
            }
            if kitty_capturing
                || memchr::memmem::find(processed_bytes, b"\x1b_G").is_some()
            {
                scan_kitty_graphics(
                    processed_bytes,
                    &mut kitty_chunk_buf,
                    &mut kitty_payload_buf,
                    &mut kitty_capturing,
                );
            }
            // OSC 777 desktop notification: alacritty drops it unhandled like
            // OSC 1337/kitty, so sniff the raw batch and stash until a snapshot
            // frame can carry it to the host pump.
            if notify_capturing
                || memchr::memmem::find(processed_bytes, b"\x1b]777").is_some()
            {
                if let Some(n) =
                    scan_osc_notify(processed_bytes, &mut notify_buf, &mut notify_capturing)
                {
                    pending_notify = Some(n);
                }
            }
            // OSC 9;9;<path> working-directory report. Our injected PowerShell
            // prompt emits it every line so the header breadcrumb can follow
            // `cd` — PowerShell freezes the process cwd at launch, so pid_cwd
            // alone shows the wrong folder. Short + self-contained, so no
            // cross-read capture state; prefix-check keeps the hot path cheap.
            if memchr::memmem::find(processed_bytes, b"\x1b]9;9;").is_some() {
                if let Some(p) = scan_osc_cwd(processed_bytes) {
                    if let Ok(mut c) = cwd_handle.lock() {
                        *c = Some(p);
                    }
                }
            }

            let update = {
                let mut t = term.lock().unwrap();
                processor.advance(&mut *t, processed_bytes);
                // alacritty buffers DECSET 2026 synchronized output internally:
                // while its sync buffer is non-empty the Term grid still holds
                // the pre-sync frame, so skip the snapshot until it flushes on
                // ?2026l or the sync timeout — no torn frame ever reaches us.
                if processor.sync_bytes_count() > 0 {
                    None
                } else {
                    // New PTY output snaps the view back to the live tail
                    // (display_offset = 0) — matches every terminal's
                    // "jump to bottom on output" behaviour and keeps the
                    // cursor row valid.
                    t.scroll_display(alacritty_terminal::grid::Scroll::Bottom);
                    let t_snap = std::time::Instant::now();
                    let mut snap = snapshot(
                        &mut t,
                        current_size.0,
                        current_size.1,
                        &pane_id,
                        &title_handle,
                        false,
                    );
                    // OSC 133 `B` = prompt end / command-input start. Our
                    // VT parser (alacritty 0.26 / vte 0.15) drops OSC 133
                    // as unhandled, so we sniff the raw batch for it and
                    // tag the snapshot with the current cursor — that's
                    // where the editable command line begins. The shell's
                    // precmd hook (injected via the ZDOTDIR shim .zshrc)
                    // is what emits it. Terminator-agnostic (BEL or ST).
                    if find_subslice(processed_bytes, b"\x1b]133;B").is_some() {
                        snap.prompt_end = Some((snap.cursor_row, snap.cursor_col));
                        // Same mark drives the command-block command extraction:
                        // the cursor here is where the typed command begins.
                        blk_prompt = Some((snap.cursor_row, snap.cursor_col));
                    }
                    // Hand off any OSC 777 notify captured this read (or a
                    // prior sync-suppressed one) to the host pump.
                    snap.notify = pending_notify.take();
                    if std::env::var_os("KASATERM_PROFILE").is_some() {
                        eprintln!(
                            "[snapshot] {}us {}x{} ({}b in)",
                            t_snap.elapsed().as_micros(),
                            current_size.0,
                            current_size.1,
                            n
                        );
                    }
                    Some(snap)
                }
            };
            // OSC 133 C/D command-block parsing — independent of the snapshot
            // (still runs when sync output suppressed it). Command text is read
            // from the grid at C time, before output overwrites the prompt line.
            parse_command_blocks(
                processed_bytes,
                &term,
                current_size,
                &blocks,
                &mut blk_capturing,
                &mut blk_seq,
                &mut blk_start,
                blk_prompt,
            );
            if let Some(upd) = update {
                // try_send (not send) so the reader is NEVER paced by the
                // pump/render side. If the consumer is behind (slow GPU
                // pass, ws-lock contention) the bounded channel fills, the
                // newest snapshot gets dropped, and bash keeps writing at
                // full PTY rate. Reader produces a fresh snapshot on the
                // next read anyway, so the only cost is a momentary stale
                // frame — which the user wouldn't have seen mid-burst.
                // The blocking `send` previously stalled the reader, which
                // backpressured bash, which made claude-code-style TUIs
                // feel ~10× slower than ghostty.
                match tx.try_send(upd) {
                    Ok(()) => {}
                    Err(crossbeam_channel::TrySendError::Full(_)) => {}
                    Err(crossbeam_channel::TrySendError::Disconnected(_)) => return,
                }
            }
        }
    })
}

/// Reassembles UTF-8 across PTY read boundaries. A read can split a
/// multibyte codepoint; buffering the tail until the next read keeps NFC
/// normalization from ever seeing a partial char.
struct Utf8Buffer {
    leftover: Vec<u8>,
}

impl Utf8Buffer {
    fn new() -> Self {
        Self { leftover: Vec::new() }
    }

    fn process(&mut self, data: &[u8]) -> String {
        self.leftover.extend_from_slice(data);
        let mut valid_up_to = 0;
        let mut i = 0;
        while i < self.leftover.len() {
            let b = self.leftover[i];
            let width = if b & 0x80 == 0 {
                1
            } else if b & 0xe0 == 0xc0 {
                2
            } else if b & 0xf0 == 0xe0 {
                3
            } else if b & 0xf8 == 0xf0 {
                4
            } else {
                1
            };
            if i + width <= self.leftover.len() {
                if std::str::from_utf8(&self.leftover[i..i + width]).is_ok() {
                    valid_up_to = i + width;
                }
                i += width;
            } else {
                break;
            }
        }
        if valid_up_to > 0 {
            let s = std::str::from_utf8(&self.leftover[..valid_up_to])
                .unwrap_or("")
                .to_string();
            self.leftover.drain(..valid_up_to);
            s
        } else {
            String::new()
        }
    }
}

fn snapshot(
    term: &mut Term<PtyEventForwarder>,
    cols: u16,
    rows: u16,
    pane_id: &str,
    last_title: &Arc<Mutex<Option<String>>>,
    // When false, only the lines alacritty marked damaged since the last
    // reset are rebuilt — a 1-char echo touches ~1 line instead of the
    // whole grid (180us → ~10us). The renderer keys ScreenUpdate.dirty by
    // row and leaves untouched rows alone, so a partial list is correct.
    // Callers that change the *whole* view (scroll, resize) pass true.
    force_full: bool,
) -> ScreenUpdate {
    // display_offset counts lines scrolled toward older history; visual
    // row r maps to grid line `r - display_offset`. Read it before the
    // &mut borrow from `damage()`.
    let display_offset = term.grid().display_offset() as i32;
    // Which visual rows to rebuild. damage() yields viewport-relative
    // line numbers (already display_offset-adjusted), and returns Full
    // on first frame / resize / scroll, which we expand to every row.
    let damaged: Vec<u16> = if force_full {
        (0..rows).collect()
    } else {
        match term.damage() {
            TermDamage::Full => (0..rows).collect(),
            TermDamage::Partial(iter) => {
                let mut v: Vec<u16> =
                    iter.map(|b| b.line as u16).filter(|&r| r < rows).collect();
                v.sort_unstable();
                v.dedup();
                v
            }
        }
    };
    term.reset_damage();
    let grid = term.grid();
    let mut dirty: Vec<(u16, Row)> = Vec::with_capacity(damaged.len());
    for &r in &damaged {
        let mut row: Row = Vec::with_capacity(cols as usize);
        // Clamp to the grid's real dimensions: a resize updates `size` and the
        // Term grid under separate locks, so for a frame they can disagree by a
        // column/line. Indexing `cols` (from `size`) into a grid that's one
        // smaller panics (OOB). Fill the overshoot with blanks — the next frame
        // repaints correctly, and we never crash on the race.
        let grid_cols = grid.columns();
        let grid_lines = grid.screen_lines();
        let line = r as i32 - display_offset;
        let line_ok = line >= 0 && (line as usize) < grid_lines;
        for c in 0..cols {
            if line_ok && (c as usize) < grid_cols {
                let point = Point::new(
                    alacritty_terminal::index::Line(line),
                    alacritty_terminal::index::Column(c as usize),
                );
                row.push(convert_cell(&grid[point]));
            } else {
                row.push(Cell::blank());
            }
        }
        dirty.push((r, row));
    }
    let cursor = term.grid().cursor.point;
    let cursor_row = cursor.line.0.max(0) as u16;
    let cursor_col = cursor.column.0 as u16;
    let mode = term.mode();
    // Hide the cursor while scrolled into history — the live cursor
    // sits at the bottom of the active area, which isn't where the
    // user is looking, so drawing it over scrollback is misleading.
    let cursor_visible = display_offset == 0
        && mode.contains(alacritty_terminal::term::TermMode::SHOW_CURSOR);
    let alt_screen = mode.contains(alacritty_terminal::term::TermMode::ALT_SCREEN);
    let mouse_enabled = mode.contains(alacritty_terminal::term::TermMode::MOUSE_REPORT_CLICK)
        || mode.contains(alacritty_terminal::term::TermMode::MOUSE_DRAG)
        || mode.contains(alacritty_terminal::term::TermMode::MOUSE_MOTION);
    let mouse_sgr = mode.contains(alacritty_terminal::term::TermMode::SGR_MOUSE);
    let app_cursor = mode.contains(alacritty_terminal::term::TermMode::APP_CURSOR);
    let bracketed_paste =
        mode.contains(alacritty_terminal::term::TermMode::BRACKETED_PASTE);
    // OSC 0 / OSC 2 title pushed by the inner program. Cached in the
    // forwarder so we can return the latest value on every snapshot
    // rather than draining alacritty's pending-title queue once and
    // losing it.
    let title: Option<String> = last_title.lock().ok().and_then(|t| t.clone());
    ScreenUpdate {
        pane_id: pane_id.to_string(),
        rows,
        cols,
        dirty,
        cursor_row,
        cursor_col,
        cursor_visible,
        alt_screen,
        mouse_enabled,
        mouse_sgr,
        app_cursor,
        bracketed_paste,
        title,
        eof: false,
        // Filled in by the reader thread when this batch carried an
        // OSC 133 `B` mark — snapshot() itself doesn't parse the stream.
        prompt_end: None,
        // Likewise stamped by the reader when an OSC 777 notify was sniffed.
        notify: None,
    }
}

/// First index where `needle` occurs in `haystack`, or None. Tiny
/// linear scan — used only to sniff the short OSC 133 prompt marker out
/// of each PTY read batch.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}

/// Max command blocks retained per pane, and max output bytes per block —
/// bounds memory against `yes`-style floods / long sessions.
const BLOCK_CAP: usize = 50;
const BLOCK_OUTPUT_CAP: usize = 256 * 1024;

/// Walk a PTY read batch for OSC 133 C/D command-block marks and accumulate
/// blocks into the shared store. vte drops OSC 133, so — like OSC 777/1337 —
/// we sniff the raw stream. `C` opens a block (command text read from the grid
/// at `prompt`, the B mark), the bytes until `D` are its output, `D;<exit>`
/// closes it. A new `C` while still capturing closes the prior block (no D).
#[allow(clippy::too_many_arguments)]
fn parse_command_blocks(
    bytes: &[u8],
    term: &Arc<Mutex<Term<PtyEventForwarder>>>,
    size: (u16, u16),
    blocks: &Arc<Mutex<VecDeque<CommandBlock>>>,
    capturing: &mut bool,
    seq: &mut u64,
    start: &mut Option<Instant>,
    prompt: Option<(u16, u16)>,
) {
    const PREFIX: &[u8] = b"\x1b]133;";
    // Fast path: nothing to do unless we're mid-block or a mark is present.
    if !*capturing && find_subslice(bytes, PREFIX).is_none() {
        return;
    }
    let mut data = bytes;
    loop {
        match find_subslice(data, PREFIX) {
            None => {
                if *capturing {
                    block_append_output(blocks, data);
                }
                return;
            }
            Some(p) => {
                // Bytes before this mark are command output (when capturing).
                if *capturing {
                    block_append_output(blocks, &data[..p]);
                }
                let kind_idx = p + PREFIX.len();
                let kind = data.get(kind_idx).copied();
                let mut rest = &data[(kind_idx + 1).min(data.len())..];
                match kind {
                    Some(b'C') => {
                        // A C while still capturing means the prior block never
                        // got a D (e.g. Ctrl-C at the prompt) — close it first.
                        if *capturing {
                            block_finalize(blocks, None, start);
                        }
                        let command = extract_command(term, size, prompt);
                        block_begin(blocks, seq, command);
                        *start = Some(Instant::now());
                        *capturing = true;
                        rest = &rest[skip_terminator(rest)..];
                    }
                    Some(b'D') => {
                        let (exit, consumed) = parse_d_payload(rest);
                        if *capturing {
                            block_finalize(blocks, exit, start);
                            *capturing = false;
                        }
                        rest = &rest[consumed.min(rest.len())..];
                    }
                    // A / B (handled in the snapshot path) and any split mark:
                    // skip the terminator and keep walking.
                    _ => {
                        rest = &rest[skip_terminator(rest)..];
                    }
                }
                data = rest;
            }
        }
    }
}

/// Length of an OSC terminator at the slice head: BEL (1) or ST `ESC \` (2).
fn skip_terminator(data: &[u8]) -> usize {
    match data.first() {
        Some(&0x07) => 1,
        _ if data.starts_with(b"\x1b\\") => 2,
        _ => 0,
    }
}

/// Parse a D mark payload (bytes after the `D`): `;<exit><term>` or `<term>`.
/// Returns (exit_code, bytes_consumed_including_terminator).
fn parse_d_payload(data: &[u8]) -> (Option<i32>, usize) {
    let bel = data.iter().position(|&b| b == 0x07);
    let st = find_subslice(data, b"\x1b\\");
    let end = match (bel, st) {
        (Some(a), Some(b)) => a.min(b),
        (Some(a), None) => a,
        (None, Some(b)) => b,
        (None, None) => return (None, data.len()), // terminator split across reads
    };
    let exit = data[..end]
        .strip_prefix(b";")
        .and_then(|p| std::str::from_utf8(p).ok())
        .and_then(|s| s.trim().parse::<i32>().ok());
    let term_len = if data.get(end) == Some(&0x07) { 1 } else { 2 };
    (exit, end + term_len)
}

fn block_begin(blocks: &Arc<Mutex<VecDeque<CommandBlock>>>, seq: &mut u64, command: String) {
    *seq += 1;
    let started_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let mut b = blocks.lock().unwrap();
    b.push_back(CommandBlock {
        id: *seq,
        command,
        output: String::new(),
        exit_code: None,
        started_ms,
        duration_ms: None,
        is_tui: false,
    });
    while b.len() > BLOCK_CAP {
        b.pop_front();
    }
}

fn block_append_output(blocks: &Arc<Mutex<VecDeque<CommandBlock>>>, chunk: &[u8]) {
    if chunk.is_empty() {
        return;
    }
    // Alt-screen enter ⇒ a TUI (vim/htop/less); its raw run isn't a clean block.
    let is_tui = find_subslice(chunk, b"\x1b[?1049h").is_some();
    let mut b = blocks.lock().unwrap();
    if let Some(last) = b.back_mut() {
        if is_tui {
            last.is_tui = true;
        }
        if last.output.len() < BLOCK_OUTPUT_CAP {
            last.output.push_str(&String::from_utf8_lossy(chunk));
        }
    }
}

fn block_finalize(
    blocks: &Arc<Mutex<VecDeque<CommandBlock>>>,
    exit: Option<i32>,
    start: &mut Option<Instant>,
) {
    let dur = start.take().map(|s| s.elapsed().as_millis() as u64);
    let mut b = blocks.lock().unwrap();
    if let Some(last) = b.back_mut() {
        // zsh PROMPT_SP draws a reverse-video '%' + filler ending in "\r \r"
        // right before the next prompt; it leaks into the C..D capture. Drop
        // that trailing marker line so the block output stays clean (Warp-like).
        if last.output.ends_with("\r \r") {
            match last.output.rfind('\n') {
                Some(nl) => last.output.truncate(nl + 1),
                None => last.output.clear(),
            }
        }
        if last.exit_code.is_none() {
            last.exit_code = exit;
        }
        if last.duration_ms.is_none() {
            last.duration_ms = dur;
        }
    }
}

/// Read the typed command out of the grid at C time: the `prompt` row (the B
/// mark) from its column to line end. Single-line commands only (wrapped
/// multi-line input is a follow-up). display_offset is 0 here (the reader
/// snaps to the live tail on output), so the visual row is the grid line.
fn extract_command(
    term: &Arc<Mutex<Term<PtyEventForwarder>>>,
    size: (u16, u16),
    prompt: Option<(u16, u16)>,
) -> String {
    let Some((prow, pcol)) = prompt else {
        return String::new();
    };
    let (cols, _rows) = size;
    let t = term.lock().unwrap();
    let grid = t.grid();
    let glines = grid.screen_lines();
    let gcols = grid.columns();
    let line = prow as usize;
    if line >= glines {
        return String::new();
    }
    let end_col = (cols as usize).min(gcols);
    let mut s = String::new();
    for c in (pcol as usize)..end_col {
        let point = Point::new(
            alacritty_terminal::index::Line(line as i32),
            alacritty_terminal::index::Column(c),
        );
        s.push(grid[point].c);
    }
    s.trim_end().to_string()
}

/// Standard base64 decode (no external crate). Ignores non-alphabet bytes
/// (whitespace, `=` padding) so it tolerates wrapped iTerm payloads.
fn b64_decode(s: &[u8]) -> Vec<u8> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0u32;
    for &c in s {
        let Some(v) = val(c) else { continue };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

/// MVP inline-image: a completed OSC 1337 body is `params:base64`. Decode
/// the payload, write a temp PNG/JPEG, and hand it to the existing image
/// pane viewer via the kasaspace `/open-image` endpoint. (True cell-flow
/// inline rendering is a later stage; this gets `imgcat`-style output
/// showing in kasaterm now.)
fn emit_inline_image(body: &[u8]) {
    let Some(colon) = body.iter().position(|&b| b == b':') else {
        return;
    };
    let bytes = b64_decode(&body[colon + 1..]);
    if bytes.len() < 16 {
        return;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = std::env::temp_dir().join(format!("kasaterm-inline-{nanos}.png"));
    if std::fs::write(&tmp, &bytes).is_err() {
        return;
    }
    let port = std::env::var("KASASPACE_MCP_PORT").unwrap_or_else(|_| "8765".into());
    let url = format!("http://127.0.0.1:{port}/open-image");
    let _ = std::process::Command::new("curl")
        .args([
            "-s",
            "--get",
            "--data-urlencode",
            &format!("path={}", tmp.display()),
            &url,
        ])
        .status();
}

/// Injected into PowerShell (`pwsh` / `powershell`) via `-Command` so it reports
/// its cwd over OSC 9;9 on every prompt, wrapping any profile-defined prompt.
/// Single-quoted throughout (no `"`) so Windows argv quoting stays trivial; the
/// `\` inside `'\'` is the literal ST terminator byte that closes the OSC.
const PWSH_CWD_SHIM: &str = "$__ktp=$function:prompt; function global:prompt { $l=$ExecutionContext.SessionState.Path.CurrentLocation; if($l -and $l.Provider.Name -eq 'FileSystem'){[Console]::Write([char]27+']9;9;'+$l.ProviderPath+[char]27+'\\')}; if($__ktp){& $__ktp}else{'PS '+$PWD.Path+'> '} }";

/// Extract the path from an OSC 9;9 working-directory report
/// (`ESC ] 9 ; 9 ; <path> ST|BEL`). Terminator-agnostic. Returns the last match
/// in the batch — the freshest cwd if several prompts arrived in one read.
fn scan_osc_cwd(bytes: &[u8]) -> Option<std::path::PathBuf> {
    const MARKER: &[u8] = b"\x1b]9;9;";
    let mut best: Option<std::path::PathBuf> = None;
    let mut from = 0;
    while let Some(rel) = memchr::memmem::find(&bytes[from..], MARKER) {
        let start = from + rel + MARKER.len();
        let rest = &bytes[start..];
        let end = rest
            .iter()
            .position(|&b| b == 0x07 || b == 0x1b)
            .unwrap_or(rest.len());
        let s = String::from_utf8_lossy(&rest[..end]);
        let trimmed = s.trim();
        if !trimmed.is_empty() {
            best = Some(std::path::PathBuf::from(trimmed));
        }
        from = start;
    }
    best
}

/// Capture an iTerm OSC 1337 inline-image sequence that may span several
/// PTY reads. `buf`/`capturing` persist across calls. Marker
/// `ESC ] 1337 ; File=` … terminator BEL or ST. alacritty parses the OSC
/// and drops it (unhandled), so the base64 never reaches the grid — we
/// sniff the raw batch in parallel to grab the payload.
fn scan_inline_image(bytes: &[u8], buf: &mut Vec<u8>, capturing: &mut bool) {
    const MARKER: &[u8] = b"\x1b]1337;File=";
    let mut data = bytes;
    loop {
        if *capturing {
            let bel = data.iter().position(|&b| b == 0x07);
            let st = find_subslice(data, b"\x1b\\");
            let end = match (bel, st) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            };
            match end {
                Some(e) => {
                    buf.extend_from_slice(&data[..e]);
                    emit_inline_image(buf);
                    buf.clear();
                    *capturing = false;
                    let term_len = if data.get(e) == Some(&0x07) { 1 } else { 2 };
                    data = &data[(e + term_len).min(data.len())..];
                }
                None => {
                    // Guard against unbounded growth on a malformed stream.
                    if buf.len() < 8 * 1024 * 1024 {
                        buf.extend_from_slice(data);
                    } else {
                        buf.clear();
                        *capturing = false;
                    }
                    return;
                }
            }
        } else {
            match find_subslice(data, MARKER) {
                Some(start) => {
                    *capturing = true;
                    data = &data[start + MARKER.len()..];
                }
                None => return,
            }
        }
    }
}

/// Capture an OSC 777 desktop-notification sequence that may span several PTY
/// reads. Mirror of `scan_inline_image`: marker `ESC ] 777 ; notify ;` …
/// terminator BEL or ST. alacritty parses the OSC and drops it (unhandled), so
/// we sniff the raw batch in parallel. Returns the last completed
/// `(title, body)` in this batch — realistically a single read carries at most
/// one (a human echoes them one at a time).
fn scan_osc_notify(
    bytes: &[u8],
    buf: &mut Vec<u8>,
    capturing: &mut bool,
) -> Option<(String, String)> {
    const MARKER: &[u8] = b"\x1b]777;notify;";
    let mut data = bytes;
    let mut result = None;
    loop {
        if *capturing {
            let bel = data.iter().position(|&b| b == 0x07);
            let st = find_subslice(data, b"\x1b\\");
            let end = match (bel, st) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            };
            match end {
                Some(e) => {
                    buf.extend_from_slice(&data[..e]);
                    // Copy out before clearing buf so the borrow is released.
                    let payload = String::from_utf8_lossy(buf).into_owned();
                    buf.clear();
                    result = Some(match payload.split_once(';') {
                        Some((t, b)) => (t.to_string(), b.to_string()),
                        None => (payload.clone(), String::new()),
                    });
                    *capturing = false;
                    let term_len = if data.get(e) == Some(&0x07) { 1 } else { 2 };
                    data = &data[(e + term_len).min(data.len())..];
                }
                None => {
                    // Guard against unbounded growth on a malformed stream.
                    if buf.len() < 8 * 1024 * 1024 {
                        buf.extend_from_slice(data);
                    } else {
                        buf.clear();
                        *capturing = false;
                    }
                    return result;
                }
            }
        } else {
            match find_subslice(data, MARKER) {
                Some(start) => {
                    *capturing = true;
                    data = &data[start + MARKER.len()..];
                }
                None => return result,
            }
        }
    }
}

/// A completed kitty graphics payload (`f=100` PNG bytes already decoded). Same
/// path as the iTerm OSC 1337 emitter — write a temp file and ask the kasaspace
/// MCP to open it in an image pane.
fn emit_kitty_image(payload: &[u8]) {
    if payload.len() < 16 {
        return;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = std::env::temp_dir().join(format!("kasaterm-kitty-{nanos}.png"));
    if std::fs::write(&tmp, payload).is_err() {
        return;
    }
    let port = std::env::var("KASASPACE_MCP_PORT").unwrap_or_else(|_| "8765".into());
    let url = format!("http://127.0.0.1:{port}/open-image");
    let _ = std::process::Command::new("curl")
        .args([
            "-s",
            "--get",
            "--data-urlencode",
            &format!("path={}", tmp.display()),
            &url,
        ])
        .status();
}

/// Capture a kitty graphics protocol sequence (APC `\x1b_G<params>;<body>\x1b\\`)
/// that may span multiple PTY reads AND multiple chunks (linked by `m=1` /
/// `m=0`). MVP: only `f=100` (PNG) direct-base64 payloads are accepted —
/// `f=32`/`f=24` raw RGB(A) are skipped because the image pane expects a
/// decodable container. alacritty's VT parser drops APCs, so we sniff in
/// parallel from the raw byte stream.
fn scan_kitty_graphics(
    bytes: &[u8],
    chunk_buf: &mut Vec<u8>,
    payload_buf: &mut Vec<u8>,
    capturing: &mut bool,
) {
    const APC_G: &[u8] = b"\x1b_G";
    const ST: &[u8] = b"\x1b\\";
    let mut data = bytes;
    loop {
        if *capturing {
            match find_subslice(data, ST) {
                Some(e) => {
                    chunk_buf.extend_from_slice(&data[..e]);
                    // Parse params (before ';') and body (after).
                    let sep = chunk_buf.iter().position(|&b| b == b';');
                    if let Some(sep_pos) = sep {
                        let (params, body) = chunk_buf.split_at(sep_pos);
                        let body = &body[1..]; // skip ';'
                        let params_s = std::str::from_utf8(params).unwrap_or("");
                        let mut more = false;
                        let mut format_png = true; // default if missing
                        for kv in params_s.split(',') {
                            let kv = kv.trim();
                            if let Some((k, v)) = kv.split_once('=') {
                                match k {
                                    "m" => more = v == "1",
                                    // First chunk carries `f=`; subsequent
                                    // chunks usually omit it.
                                    "f" if !v.is_empty() => format_png = v == "100",
                                    _ => {}
                                }
                            }
                        }
                        // Reject non-PNG formats once detected; clear state.
                        if !format_png {
                            payload_buf.clear();
                            chunk_buf.clear();
                            *capturing = false;
                            data = &data[(e + ST.len()).min(data.len())..];
                            continue;
                        }
                        let decoded = b64_decode(body);
                        payload_buf.extend_from_slice(&decoded);
                        if !more {
                            emit_kitty_image(payload_buf);
                            payload_buf.clear();
                        }
                    }
                    chunk_buf.clear();
                    *capturing = false;
                    data = &data[(e + ST.len()).min(data.len())..];
                }
                None => {
                    chunk_buf.extend_from_slice(data);
                    if chunk_buf.len() > 8 * 1024 * 1024 {
                        chunk_buf.clear();
                        payload_buf.clear();
                        *capturing = false;
                    }
                    return;
                }
            }
        } else {
            match find_subslice(data, APC_G) {
                Some(start) => {
                    *capturing = true;
                    data = &data[start + APC_G.len()..];
                }
                None => return,
            }
        }
    }
}

/// Build the "Last login: <time> on <tty>" banner Terminal.app shows.
/// Returns None on first ever spawn (no stored timestamp) or when we
/// couldn't resolve a tty name — both cases would render as an
/// awkward partial line.
///
/// State lives at `$HOME/.config/kasaterm/last_login` as one line of
/// pre-formatted text (e.g. "Tue May 26 13:05:54"). We re-emit the
/// *previous* contents and overwrite with `date(1)`-formatted "now"
/// so the next spawn sees this run's timestamp.
fn build_last_login_line(tty: Option<&str>) -> Option<String> {
    let tty = tty?;
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from)?;
    let dir = home.join(".config").join("kasaterm");
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

fn convert_cell(cell: &alacritty_terminal::term::cell::Cell) -> Cell {
    let ch = if cell.c == '\0' { ' ' } else { cell.c };
    Cell {
        ch,
        fg: convert_color(cell.fg),
        bg: convert_color(cell.bg),
        bold: cell
            .flags
            .contains(alacritty_terminal::term::cell::Flags::BOLD),
        italic: cell
            .flags
            .contains(alacritty_terminal::term::cell::Flags::ITALIC),
        underline: cell
            .flags
            .contains(alacritty_terminal::term::cell::Flags::UNDERLINE),
        inverse: cell
            .flags
            .contains(alacritty_terminal::term::cell::Flags::INVERSE),
        dim: cell
            .flags
            .contains(alacritty_terminal::term::cell::Flags::DIM),
        hidden: cell
            .flags
            .contains(alacritty_terminal::term::cell::Flags::HIDDEN),
    }
}

fn convert_color(c: VtColor) -> Color {
    match c {
        VtColor::Named(NamedColor::Foreground) | VtColor::Named(NamedColor::Background) => {
            Color::Default
        }
        VtColor::Named(n) => Color::Idx(n as u8),
        VtColor::Spec(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
        VtColor::Indexed(i) => Color::Idx(i),
    }
}

#[cfg(test)]
mod osc_notify_tests {
    use super::*;

    fn scan_all(reads: &[&[u8]]) -> Vec<(String, String)> {
        let mut buf = Vec::new();
        let mut cap = false;
        let mut out = Vec::new();
        for r in reads {
            if let Some(n) = scan_osc_notify(r, &mut buf, &mut cap) {
                out.push(n);
            }
        }
        out
    }

    #[test]
    fn basic_title_body_bel() {
        assert_eq!(
            scan_all(&[b"\x1b]777;notify;Build done;took 3m\x07"]),
            vec![("Build done".into(), "took 3m".into())]
        );
    }

    #[test]
    fn title_only_no_body() {
        assert_eq!(
            scan_all(&[b"\x1b]777;notify;Heads up\x07"]),
            vec![("Heads up".into(), String::new())]
        );
    }

    #[test]
    fn st_terminator() {
        assert_eq!(
            scan_all(&[b"\x1b]777;notify;T;B\x1b\\"]),
            vec![("T".into(), "B".into())]
        );
    }

    #[test]
    fn body_keeps_extra_semicolons() {
        assert_eq!(
            scan_all(&[b"\x1b]777;notify;T;a;b;c\x07"]),
            vec![("T".into(), "a;b;c".into())]
        );
    }

    #[test]
    fn spans_two_reads() {
        assert_eq!(
            scan_all(&[b"\x1b]777;notify;Ti", b"tle;Body\x07"]),
            vec![("Title".into(), "Body".into())]
        );
    }

    #[test]
    fn ignores_unrelated_osc() {
        assert!(scan_all(&[b"\x1b]0;just a window title\x07hello"]).is_empty());
    }

    #[test]
    fn embedded_in_shell_output() {
        assert_eq!(
            scan_all(&[b"done\r\n\x1b]777;notify;X;Y\x07$ "]),
            vec![("X".into(), "Y".into())]
        );
    }

    // process_cmdline over the test binary's own pid must recover a non-empty
    // command line — exercises the Windows PEB/ReadProcessMemory chain (and the
    // Unix `ps` path) on a process we control.
    #[test]
    fn cmdline_of_self_nonempty() {
        let cmd = super::process_cmdline(std::process::id());
        assert!(cmd.is_some_and(|c| !c.trim().is_empty()));
    }

    fn cwd_of(bytes: &[u8]) -> Option<String> {
        super::scan_osc_cwd(bytes).map(|p| p.to_string_lossy().into_owned())
    }

    #[test]
    fn osc_cwd_reads_st_and_bel_terminators() {
        // What the injected PowerShell prompt actually emits: OSC 9;9, path, ST,
        // then the chained prompt text.
        let st = b"\x1b]9;9;C:\\Users\\x\x1b\\PS C:\\Users\\x> ";
        assert_eq!(cwd_of(st), Some("C:\\Users\\x".to_string()));
        let bel = b"\x1b]9;9;/home/u\x07$ ";
        assert_eq!(cwd_of(bel), Some("/home/u".to_string()));
    }

    #[test]
    fn osc_cwd_takes_last_report_in_batch() {
        // Two prompts landed in one read — the freshest cwd must win.
        assert_eq!(cwd_of(b"\x1b]9;9;/a\x07\x1b]9;9;/b\x07"), Some("/b".to_string()));
    }

    #[test]
    fn osc_cwd_ignores_unrelated_output() {
        assert_eq!(cwd_of(b"hello\x1b]0;title\x07"), None);
    }
}

/// Windows Git bash 는 런처(bin\bash.exe)가 실셸(usr\bin\bash.exe)을 자식으로
/// 한 번 더 스폰하고, sh wrapper 스크립트도 셸을 한 단 더 끼운다 — 직계 자식만
/// 보면 항상 "bash.exe"라 claude 탐지(색·프사 게이트)와 busy 판정이 전부 죽는다.
/// 유일한 자식이 셸일 때만 그쪽을 셸로 보고 내려간다(명령 실행 중이면 자식이
/// 비셸이라 그 자리에서 멈춰 기존 의미 유지). Unix 는 no-op.
fn effective_shell_pid(table: &[(u32, u32, String)], pid: u32) -> u32 {
    #[cfg(not(windows))]
    {
        let _ = table;
        pid
    }
    #[cfg(windows)]
    {
        let mut pid = pid;
        for _ in 0..3 {
            let mut kids = table.iter().filter(|(_, pp, _)| *pp == pid);
            let (Some(only), None) = (kids.next(), kids.next()) else {
                break;
            };
            if !is_shell_exe(&only.2) {
                break;
            }
            pid = only.0;
        }
        pid
    }
}

#[cfg(windows)]
fn is_shell_exe(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let base = lower.strip_suffix(".exe").unwrap_or(&lower);
    matches!(
        base,
        "bash" | "sh" | "zsh" | "dash" | "fish" | "tcsh" | "ksh" | "cmd" | "pwsh" | "powershell"
    )
}

/// 이 GUI 프로세스가 스폰한 claude 가 어딘가 살아있는가 — shim wrapper 가 붙인
/// `--settings %TEMP%\kasaterm-shim-<GUI pid>\...` argv 마커로 판정한다. 남의
/// 터미널(VS Code 등)에서 도는 claude 는 마커가 없어 배제된다. 호출측 500ms
/// 캐시 안에서만 돌고, cmdline 조회는 이름이 claude 인 프로세스로 한정.
#[cfg(windows)]
fn orphan_claude_of_this_gui(table: &[(u32, u32, String)]) -> bool {
    let marker = format!("kasaterm-shim-{}", std::process::id());
    table
        .iter()
        .filter(|(_, _, name)| name.to_ascii_lowercase().contains("claude"))
        .any(|(pid, _, _)| process_cmdline(*pid).is_some_and(|cl| cl.contains(&marker)))
}

/// Windows 프로세스명은 "claude.exe" — active_process_name 호출자들은 "claude" /
/// "bash" 같은 bare 이름과 정확 일치 비교하므로 여기서 확장자를 벗겨 플랫폼
/// 균질화한다. Unix 는 no-op.
/// pane 에서 도는 에이전트 종류. 학생 대접(보더 학생색·타이틀바·얼굴·탭칩)은
/// claude 전용이 아니라 **이 값이 Some 이면** 붙는다(거노 2026-08-05: codex 도 학생).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AgentKind {
    Claude,
    Codex,
}

impl AgentKind {
    /// 저장·전송용 이름. `pane_record` 의 `was_agent`, board 의 `harness`, 소켓
    /// 응답이 전부 이 하나를 쓴다 — match 를 사본으로 늘리면 한쪽만 고쳐져 갈린다.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    /// 프로세스 comm 으로 판정. comm 은 경로가 붙어 올 수 있어(손자 행은
    /// `…/bin/codex`) 파일명만 떼어 본다.
    fn from_comm(comm: &str) -> Option<Self> {
        let base = comm.rsplit(['/', '\\']).next().unwrap_or(comm);
        let base = strip_exe_suffix(base.to_string());
        match base.as_str() {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            _ => None,
        }
    }
}

/// 셸 pid 아래에서 에이전트를 찾는다 — 테이블만 보는 순수 함수라 실측 트리로 잴 수 있다.
///
/// 같은 부모의 자식 중 **가장 나중에 뜬 것**(pid 큰 쪽)을 고른다 — `active_process_name`
/// 과 같은 규칙. 직속 자식이 런처류면 한 세대 더 내려간다.
/// 셸 pid 하나로 하네스를 묻는다 — `PtySession` 을 못 쥐고 pid 만 아는 호출자
/// (board 조립·소켓 백엔드)용. 프로세스 테이블은 이미 공유 캐시라 `ps` 가 추가로
/// 안 돈다. 판정 본체는 `agent_in_table` 하나뿐이라 `active_agent` 와 결과가 같다.
pub fn agent_for_shell(table: &[(u32, u32, String)], shell_pid: u32) -> Option<AgentKind> {
    agent_in_table(table, effective_shell_pid(table, shell_pid))
}

fn agent_in_table(table: &[(u32, u32, String)], shell_pid: u32) -> Option<AgentKind> {
    let newest_child = |parent: u32| -> Option<(u32, &str)> {
        let mut best: Option<(u32, &str)> = None;
        for (row_pid, row_ppid, name) in table.iter() {
            if *row_ppid == parent && best.as_ref().is_none_or(|(p, _)| *p < *row_pid) {
                best = Some((*row_pid, name.as_str()));
            }
        }
        best
    };
    let (child_pid, child) = newest_child(shell_pid)?;
    if let Some(kind) = AgentKind::from_comm(child) {
        return Some(kind);
    }
    if is_agent_launcher(child) {
        if let Some((_, grandchild)) = newest_child(child_pid) {
            return AgentKind::from_comm(grandchild);
        }
    }
    None
}

/// 에이전트를 감싸 띄우는 것들 — 이게 직속 자식이면 진짜 프로세스는 한 세대 아래다.
/// codex 가 npm shim 이라 `node` 를 거치는 게 대표 사례고, `npx`·래퍼 셸도 같다.
fn is_agent_launcher(comm: &str) -> bool {
    let base = comm.rsplit(['/', '\\']).next().unwrap_or(comm);
    let base = strip_exe_suffix(base.to_string());
    matches!(
        base.as_str(),
        "node" | "npm" | "npx" | "bun" | "deno" | "env" | "sh" | "bash" | "zsh" | "fish"
    )
}

fn strip_exe_suffix(name: String) -> String {
    #[cfg(not(windows))]
    {
        name
    }
    #[cfg(windows)]
    {
        if name.to_ascii_lowercase().ends_with(".exe") {
            name[..name.len() - 4].to_string()
        } else {
            name
        }
    }
}

/// `(pid, ppid, exe_name)` for every running process — the cross-platform
/// stand-in for `ps -A -o pid=,ppid=,comm=`. `exe_name` is the bare file name
/// (no directory; e.g. "claude.exe", "pwsh"). Windows walks a Toolhelp snapshot
/// because it has no `ps`; Unix shells out to `ps`. Callers match on pid/ppid
/// and substring the name (e.g. `.contains("claude")`).
#[cfg(windows)]
fn process_table_raw() -> Vec<(u32, u32, String)> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    let mut out = Vec::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return out;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snap, &mut entry) != 0 {
            loop {
                let end = entry
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(entry.szExeFile.len());
                let name = String::from_utf16_lossy(&entry.szExeFile[..end]);
                out.push((entry.th32ProcessID, entry.th32ParentProcessID, name));
                if Process32NextW(snap, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
    }
    out
}

#[cfg(unix)]
fn process_table_raw() -> Vec<(u32, u32, String)> {
    let Ok(output) = std::process::Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,comm="])
        .output()
    else {
        return Vec::new();
    };
    let s = String::from_utf8_lossy(&output.stdout);
    let mut out = Vec::new();
    for line in s.lines() {
        let mut parts = line.split_whitespace();
        let (Some(pid), Some(ppid)) = (
            parts.next().and_then(|x| x.parse::<u32>().ok()),
            parts.next().and_then(|x| x.parse::<u32>().ok()),
        ) else {
            continue;
        };
        let comm = parts.collect::<Vec<_>>().join(" ");
        let name = std::path::Path::new(&comm)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(&comm)
            .to_string();
        out.push((pid, ppid, name));
    }
    out
}

/// `process_table_raw` 를 짧은 TTL 로 감싼 전역 캐시. 세션이 많을 때 렌더 스레드가
/// pane 마다 active_process_name/is_claude_agents 로 이걸 부르면, per-pane 500ms
/// 캐시가 같은 프레임에 동시 만료될 때 K 번 ps fork 가 겹쳐 프레임드랍(거노: 세션
/// 많을 때). 300ms 전역 캐시로 한 프레임의 중복 fork 를 1 회로 접는다(per-pane 캐시
/// 보다 촘촘해 신선도는 유지). fork 대신 Vec clone 이라 비용이 pane 수에 선형이지만
/// ps fork+파싱보다 훨씬 싸다. 빈 결과(ps 실패)는 캐싱하지 않아 다음 호출이 재시도한다.
pub fn process_table() -> Vec<(u32, u32, String)> {
    (*process_table_shared()).clone()
}

pub type ProcessTable = std::sync::Arc<Vec<(u32, u32, String)>>;

/// 같은 캐시를 **복사 없이** 빌려준다. 렌더처럼 pane 마다 매 프레임 부르는 쪽은
/// 이걸 써야 한다 — `process_table()` 은 히트할 때도 테이블을 통째로 clone 해서
/// 프로세스 수백 개면 프레임마다 그만큼의 String 할당이 돈다.
pub fn process_table_shared() -> ProcessTable {
    struct Cached {
        at: Instant,
        table: ProcessTable,
        refreshing: bool,
    }
    static CACHE: std::sync::OnceLock<std::sync::Mutex<Cached>> = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(|| {
        std::sync::Mutex::new(Cached {
            at: Instant::now() - std::time::Duration::from_secs(1),
            table: Default::default(),
            refreshing: false,
        })
    });
    let Ok(mut g) = cache.lock() else {
        return std::sync::Arc::new(process_table_raw());
    };
    if !g.table.is_empty() && g.at.elapsed().as_millis() < 300 {
        return g.table.clone();
    }
    // 첫 호출은 답이 없으니 그 자리에서 채운다. 그 뒤로는 **절대 프레임 안에서
    // 새로 뜨지 않는다** — 갱신은 백그라운드로 돌리고 직전 표를 그대로 준다.
    // ps fork 는 수 ms 짜리라, 300ms 마다 렌더 프레임 하나가 그걸 뒤집어쓰면
    // 초당 세 번 눈에 띄는 딸꾹질이 된다.
    if g.table.is_empty() {
        let fresh = process_table_raw();
        if fresh.is_empty() {
            // ps 실패는 캐싱하지 않는다 — 다음 호출이 재시도한다.
            return Default::default();
        }
        g.at = Instant::now();
        g.table = std::sync::Arc::new(fresh);
        return g.table.clone();
    }
    if !g.refreshing {
        g.refreshing = true;
        std::thread::spawn(move || {
            let fresh = process_table_raw();
            if let Ok(mut g) = cache.lock() {
                if !fresh.is_empty() {
                    g.at = Instant::now();
                    g.table = std::sync::Arc::new(fresh);
                }
                g.refreshing = false;
            }
        });
    }
    g.table.clone()
}

/// shell 의 직계 claude 자식이 `claude agents`(에이전트 목록 뷰) 서브커맨드로
/// 도는지. agents 뷰는 shell→claude 직계라 부모 체인 walk 불필요(background
/// --resume 은 실제 대화라 여기 해당 없음). argv 에 독립 토큰 `agents` 가 있으면
/// true — 일반 대화 argv 엔 없다.
fn claude_agents_argv(shell_pid: u32) -> bool {
    let claude_pid = process_table()
        .into_iter()
        .filter(|(_, ppid, name)| *ppid == shell_pid && name.contains("claude"))
        .map(|(pid, _, _)| pid)
        .max();
    let Some(pid) = claude_pid else {
        return false;
    };
    let Some(argv) = process_cmdline(pid) else {
        return false;
    };
    // attach 도 뷰 — agents 목록과 마찬가지로 "남의 세션을 보는 pane"이라, 학생 표시를
    // 파싱 결과로만 하는 게이트(display_pane_char)가 같은 판정을 공유한다. 일반 세션
    // 부팅은 --session-id/--resume/persona 가 붙어 이 토큰이 나올 일이 없다.
    argv.split_whitespace().any(|t| t == "agents" || t == "attach")
}

/// The full command line (argv, space-joined) of a single process, or None if
/// it can't be read. The cross-platform stand-in for `ps -p PID -o args=`.
/// Windows has no `ps`, so it walks the target's PEB →
/// RTL_USER_PROCESS_PARAMETERS.CommandLine over ReadProcessMemory (needs
/// PROCESS_VM_READ, i.e. same-user / same-integrity processes — enough for the
/// claude panes we spawn). Unix shells out to `ps`.
#[cfg(windows)]
pub fn process_cmdline(pid: u32) -> Option<String> {
    use std::ffi::c_void;
    use windows_sys::Wdk::System::Threading::{
        NtQueryInformationProcess, ProcessBasicInformation,
    };
    use windows_sys::Win32::Foundation::{CloseHandle, UNICODE_STRING};
    use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PEB, PROCESS_BASIC_INFORMATION, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ,
        RTL_USER_PROCESS_PARAMETERS,
    };
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, 0, pid);
        if handle.is_null() {
            return None;
        }
        // Wrap the chained reads so CloseHandle always runs on any early exit.
        let read = || -> Option<String> {
            let mut pbi: PROCESS_BASIC_INFORMATION = std::mem::zeroed();
            let mut ret_len = 0u32;
            if NtQueryInformationProcess(
                handle,
                ProcessBasicInformation,
                &mut pbi as *mut _ as *mut c_void,
                std::mem::size_of::<PROCESS_BASIC_INFORMATION>() as u32,
                &mut ret_len,
            ) != 0
            {
                return None;
            }
            if pbi.PebBaseAddress.is_null() {
                return None;
            }
            let mut peb: PEB = std::mem::zeroed();
            if ReadProcessMemory(
                handle,
                pbi.PebBaseAddress as *const c_void,
                &mut peb as *mut _ as *mut c_void,
                std::mem::size_of::<PEB>(),
                std::ptr::null_mut(),
            ) == 0
            {
                return None;
            }
            let mut params: RTL_USER_PROCESS_PARAMETERS = std::mem::zeroed();
            if ReadProcessMemory(
                handle,
                peb.ProcessParameters as *const c_void,
                &mut params as *mut _ as *mut c_void,
                std::mem::size_of::<RTL_USER_PROCESS_PARAMETERS>(),
                std::ptr::null_mut(),
            ) == 0
            {
                return None;
            }
            let cmd: UNICODE_STRING = params.CommandLine;
            if cmd.Buffer.is_null() || cmd.Length == 0 {
                return None;
            }
            let mut buf = vec![0u16; (cmd.Length / 2) as usize];
            if ReadProcessMemory(
                handle,
                cmd.Buffer as *const c_void,
                buf.as_mut_ptr() as *mut c_void,
                cmd.Length as usize,
                std::ptr::null_mut(),
            ) == 0
            {
                return None;
            }
            Some(String::from_utf16_lossy(&buf))
        };
        let result = read();
        CloseHandle(handle);
        result
    }
}

#[cfg(unix)]
pub fn process_cmdline(pid: u32) -> Option<String> {
    let out = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "args="])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// pane 프로세스 env 의 한 변수 값 — 세션 캐릭터 anchor(`KASATERM_SESSION_ID`) 복원용.
/// 포크·`--resume`·`agents`·`--bg` 는 claude 가 transcript id 를 새로 발급해 stem ≠ 원본
/// anchor 라, stem 매핑도 부모 상속(parentSessionId)도 실패한다(거노: 백그라운드 재접속에서
/// 학생이 유우카로 둔갑). env 의 KASATERM_SESSION_ID 는 스폰 때 캐릭터에 바인딩된 원본이라
/// (env 상속으로 포크/재접속 너머 보존) 유일하게 진짜 학생을 가리킨다. 값은 uuid(공백 없음)라
/// 공백 split 파싱이 안전하다. `ps eww` = env 를 command 열 뒤에 붙여 출력(macOS/BSD).
#[cfg(unix)]
pub fn process_env_var(pid: u32, key: &str) -> Option<String> {
    process_env_vars(pid, &[key]).remove(key)
}

/// 여러 키를 **ps 한 번**으로 읽는다 — board 는 pane 마다 여러 env 를 보는데 키당
/// 프로세스를 띄우면 폴링(1s)마다 pane 수 × 키 수만큼 ps 가 뜬다.
#[cfg(unix)]
pub fn process_env_vars(pid: u32, keys: &[&str]) -> std::collections::HashMap<String, String> {
    let mut found = std::collections::HashMap::new();
    let Ok(out) = std::process::Command::new("ps")
        .args(["eww", "-p", &pid.to_string(), "-o", "command="])
        .output()
    else {
        return found;
    };
    let s = String::from_utf8_lossy(&out.stdout);
    for tok in s.split_whitespace() {
        for k in keys {
            if let Some(v) = tok.strip_prefix(&format!("{k}=")) {
                if !v.is_empty() {
                    found.insert((*k).to_string(), v.to_string());
                }
            }
        }
    }
    found
}

#[cfg(not(unix))]
pub fn process_env_var(_pid: u32, _key: &str) -> Option<String> {
    None
}

#[cfg(not(unix))]
pub fn process_env_vars(_pid: u32, _keys: &[&str]) -> std::collections::HashMap<String, String> {
    std::collections::HashMap::new()
}

#[cfg(test)]
mod process_table_tests {
    use super::{process_table_shared, ProcessTable};

    /// 표 자체가 맞는지 — 자기 프로세스는 반드시 들어 있다. `ps` 출력 파싱이
    /// 깨지면(열 순서·comm 공백) 여기서 잡힌다.
    #[test]
    fn shared_table_contains_this_process() {
        let t: ProcessTable = process_table_shared();
        let me = std::process::id();
        assert!(!t.is_empty(), "표가 비었다 — ps 파싱 실패");
        assert!(t.iter().any(|(pid, _, _)| *pid == me), "자기 pid 가 표에 없다");
    }

    /// TTL 안의 두 호출은 **같은 Arc** 여야 한다. 여기서 복사본이 나오기 시작하면
    /// 렌더가 pane 마다 매 프레임 표를 통째로 clone 하던 시절로 돌아간다 —
    /// 프로세스 수백 개면 프레임마다 그만큼의 String 할당이다.
    #[test]
    fn shared_table_is_not_copied_within_ttl() {
        // 첫 호출이 백그라운드 갱신을 걸었을 수 있으니 가라앉힌 뒤에 잰다.
        let _ = process_table_shared();
        std::thread::sleep(std::time::Duration::from_millis(80));
        let a = process_table_shared();
        let b = process_table_shared();
        assert!(std::sync::Arc::ptr_eq(&a, &b), "TTL 안인데 표가 새로 만들어졌다");
    }
}

#[cfg(test)]
mod agent_kind_tests {
    use super::*;

    /// 실측 트리(2026-08-05, 거노 머신). codex 는 npm shim 이라 진짜 바이너리가
    /// **손자**다 — 이름만 보는 판정은 여기서 반드시 실패한다.
    fn codex_tree() -> Vec<(u32, u32, String)> {
        vec![
            (60536, 1, "zsh".into()),
            (60973, 60536, "node".into()),
            (60992, 60973, "codex".into()),
        ]
    }

    #[test]
    fn codex_는_node_아래_손자로_잡힌다() {
        assert_eq!(agent_in_table(&codex_tree(), 60536), Some(AgentKind::Codex));
    }

    #[test]
    fn claude_는_직속_자식으로_잡힌다() {
        let t = vec![(71388, 1, "zsh".into()), (71391, 71388, "claude".into())];
        assert_eq!(agent_in_table(&t, 71388), Some(AgentKind::Claude));
    }

    #[test]
    fn 그냥_셸은_아무것도_아니다() {
        // 음성 대조군이 없으면 "늘 Some" 을 내는 판정도 통과한다.
        let t = vec![(100, 1, "zsh".into()), (101, 100, "vim".into())];
        assert_eq!(agent_in_table(&t, 100), None);
    }

    #[test]
    fn 런처만_있고_손자가_없으면_아무것도_아니다() {
        // node 를 띄웠지만 codex 가 아닌 경우 — 런처를 봤다고 에이전트로 치면 안 된다.
        let t = vec![(200, 1, "zsh".into()), (201, 200, "node".into())];
        assert_eq!(agent_in_table(&t, 200), None);
    }

    #[test]
    fn 자식이_여럿이면_가장_나중_것() {
        // active_process_name 과 같은 규칙(pid 큰 쪽) — 옛 자식이 남아 있어도
        // 지금 화면에 보이는 것을 고른다.
        let t = vec![
            (300, 1, "zsh".into()),
            (301, 300, "vim".into()),
            (302, 300, "claude".into()),
        ];
        assert_eq!(agent_in_table(&t, 300), Some(AgentKind::Claude));
    }

    #[test]
    fn 경로가_붙어_와도_파일명으로_본다() {
        // 테이블은 basename 으로 정규화하지만, 그 전제가 깨져도 판정은 서야 한다.
        let t = vec![
            (400, 1, "zsh".into()),
            (401, 400, "/usr/local/bin/node".into()),
            (402, 401, "/opt/homebrew/bin/codex".into()),
        ];
        assert_eq!(agent_in_table(&t, 400), Some(AgentKind::Codex));
    }
}

#[cfg(test)]
mod scrollback_probe {
    use super::*;

    /// 실 PTY 로 스크롤백 **보존 줄수와 그 대가(RSS)** 를 잰다 — 거노: "히스토리가 왜
    /// 다 안 남지, 보려고 위로 올리면 없어져 있어". 캡은 폭에서 나오므로(예산 ÷ 폭)
    /// 넓은 pane 일수록 짧아진다. `KASATERM_SCROLLBACK_MB` 와 `PROBE_COLS` 로 조합을
    /// 바꿔 가며 돌린다. 무시(ignore)인 이유는 셸을 띄우고 몇십 초 기다려서다.
    #[test]
    #[ignore]
    fn how_many_lines_survive() {
        let cols: u16 = std::env::var("PROBE_COLS").ok().and_then(|s| s.parse().ok()).unwrap_or(363);
        let rss = || -> u64 {
            let out = std::process::Command::new("ps")
                .args(["-o", "rss=", "-p", &std::process::id().to_string()])
                .output().ok();
            out.and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<u64>().ok())
                .unwrap_or(0) / 1024
        };
        let before = rss();
        let s = PtySession::start(PtyOptions {
            shell: Some("/bin/sh".into()),
            cols, rows: 40, pane_id: "%probe".into(),
            ..Default::default()
        }).unwrap();
        s.send_bytes(format!("for i in $(seq 1 {}); do echo line-$i; done\n", std::env::var("PROBE_LINES").unwrap_or_else(|_| "200000".into())).as_bytes()).unwrap();
        std::thread::sleep(std::time::Duration::from_secs(
            std::env::var("PROBE_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(40),
        ));
        // 양수 = 오래된 쪽(위).
        let up = s.scroll(1_000_000);
        eprintln!(
            "예산={}MB cols={cols} 캡={}줄 실제보존={up}줄 RSS {}→{}MB",
            scrollback_budget_bytes() / 1024 / 1024,
            history_lines_for_cols(cols),
            before, rss()
        );
    }
}
