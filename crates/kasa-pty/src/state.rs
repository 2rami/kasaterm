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

/// claude 가 확정된 사용자 프롬프트 행 머리에 남기는 마커(U+276F).
///
/// ASCII `>` 는 **일부러 제외한다** — diff·인용·다른 TUI 가 행 머리에 흔히 써서,
/// 그것까지 마커로 치면 대화와 무관한 줄이 턴 목록에 섞인다(같은 이유로
/// `screenread::prompt_box` 도 `❯`/`›` 만 인정한다).
const PROMPT_MARKER: char = '\u{276f}';

/// 그리드 전체(스크롤백+화면)에서 프롬프트 줄을 훑는다. `PtySession::prompt_anchors`
/// 의 알맹이이자, 시험이 살아 있는 PTY 없이 부를 수 있는 진입점이다.
fn scan_prompt_anchors(term: &Term<PtyEventForwarder>) -> Vec<PromptAnchor> {
    use alacritty_terminal::index::{Column, Line};
    use alacritty_terminal::term::cell::Flags;
    let grid = term.grid();
    let cols = grid.columns();
    let hist = grid.history_size();
    let screen = grid.screen_lines();
    let mut out = Vec::new();
    for i in 0..(hist + screen) {
        let line = i as i32 - hist as i32;
        if grid[Point::new(Line(line), Column(0))].c != PROMPT_MARKER {
            continue;
        }
        if cols > 1 && grid[Point::new(Line(line), Column(1))].c == '\u{a0}' {
            continue;
        }
        let mut text = String::new();
        for c in 1..cols {
            let cell = &grid[Point::new(Line(line), Column(c))];
            // wide 글리프가 차지한 **뒤칸을 건너뛴다**. 그 칸의 문자는 `\0` 이 아니라
            // 진짜 `' '` 이고 구분은 플래그에만 있어서, 문자만 보고 거르면 한글마다
            // 한 칸씩 벌어진 「질 문  1」이 나온다(2026-08-15 실측). 웹터미널이 같은
            // 자리에서 물렸던 함정과 같은 것이다.
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) || cell.c == '\0' {
                continue;
            }
            text.push(cell.c);
        }
        let text = text.trim().to_string();
        if text.is_empty() {
            continue;
        }
        out.push(PromptAnchor { abs_line: i as i64, text });
    }
    out
}

/// 뷰포트 바로 위 스크롤백 행들 — `PtySession::rows_above` 의 알맹이이자, 시험이
/// 살아 있는 PTY 없이 부를 수 있는 진입점(`scan_prompt_anchors` 와 같은 모양).
/// 뷰포트 첫 행은 그리드 줄 `-display_offset` 이므로(스냅샷과 같은 셈) 그 위는
/// `-display_offset - 1` 부터 `-history_size` 까지, 가까운 순으로 담는다.
fn read_rows_above(term: &Term<PtyEventForwarder>, n: usize) -> Vec<Row> {
    let g = term.grid();
    let cols = g.columns();
    let bottom = -(g.history_size() as i32);
    let mut line = -(g.display_offset() as i32) - 1;
    let mut out = Vec::new();
    while line >= bottom && out.len() < n {
        let mut row: Row = Vec::with_capacity(cols);
        for c in 0..cols {
            let point = Point::new(
                alacritty_terminal::index::Line(line),
                alacritty_terminal::index::Column(c),
            );
            row.push(convert_cell(&g[point]));
        }
        out.push(row);
        line -= 1;
    }
    out
}

/// 살아 있는 화면(스크롤 위치와 무관한 맨 아래 화면)의 마지막 `n` 행 — 위→아래 순.
///
/// `read_rows_above` 의 거울이다. 그쪽은 뷰포트 **위** 스크롤백을 보고, 이쪽은
/// 스크롤을 얼마나 올렸든 지금 프로그램이 그리고 있는 화면의 **꼬리**를 본다.
/// 그리드 줄 번호는 display_offset 과 무관하므로(뷰포트 r 행 = 줄 `r - offset`)
/// 화면 마지막 줄은 언제나 `screen_lines - 1` 이다.
fn read_live_tail(term: &Term<PtyEventForwarder>, n: usize) -> Vec<Row> {
    let g = term.grid();
    let cols = g.columns();
    let lines = g.screen_lines();
    let start = lines.saturating_sub(n);
    let mut out = Vec::with_capacity(lines - start);
    for line in start..lines {
        let mut row: Row = Vec::with_capacity(cols);
        for c in 0..cols {
            let point = Point::new(
                alacritty_terminal::index::Line(line as i32),
                alacritty_terminal::index::Column(c),
            );
            row.push(convert_cell(&g[point]));
        }
        out.push(row);
    }
    out
}

/// 스크롤백에 남은 사용자 프롬프트 한 줄의 자리.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptAnchor {
    /// 세션 시작을 0 으로 하는 절대 줄 번호. 스크롤로는 흔들리지 않지만
    /// 히스토리가 상한에 닿아 회전하면 조용히 밀리므로, 오래 들고 있지 말고
    /// 히스토리 길이가 바뀔 때마다 다시 스캔해서 쓴다(인라인 이미지 앵커가
    /// 같은 이유로 회전 시 통째로 버려진다).
    pub abs_line: i64,
    /// 마커 뒤 본문. 헤더에 한 줄로 띄우는 것이라 감긴 뒷줄은 포함하지 않는다.
    pub text: String,
}

/// PTY 의 실체가 어디 있는가 — 이 프로세스(Local)인가 원격 호스트(External)인가.
///
/// External 은 소유권이 원격에 있는 세션의 **로컬 파서 사본**이다: 바이트가 그대로
/// 들어와 같은 alacritty Term 을 채우므로 스크롤백·미니맵·peek·sid 마커 스캔이
/// 로컬 pane 과 똑같이 동작한다. 다른 것은 셋뿐이다 — resize 가 ioctl 대신 제어
/// 콜백으로 나가고, Drop 이 child 를 죽이지 않으며(원격 세션은 detach 로 살아남는
/// 것이 목적이다), 자동 응답(DSR·OSC 색 질의)이 나가지 않는다(원격 호스트의 Term
/// 이 이미 답한다 — 여기서도 답하면 원격 앱이 응답을 두 번 받는다).
enum SessionIo {
    Local {
        master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
        child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
    },
    External {
        on_resize: Arc<dyn Fn(u16, u16) + Send + Sync>,
    },
    /// 다른 프로세스(GUI)가 띄운 PTY 를 **산 채로 입양**했다 — 핸드오프의 데몬 쪽.
    /// fd 는 SCM_RIGHTS 로 건너온 master 이고, child 는 우리 자식이 아니라
    /// pid 로만 안다(Drop 에서 kill(pid); 좀비 회수는 원 부모가 죽으면 launchd 몫).
    #[cfg(unix)]
    Adopted {
        fd: std::os::fd::OwnedFd,
        child_pid: Option<u32>,
    },
}

/// 외부 소스(WebSocket 클라이언트 등)가 `start_external` 세션에 밀어 넣는 이벤트.
///
/// 순서가 곧 정합성이다 — SetSize 를 별도 경로로 보내면 「옛 바이트를 새 크기로
/// 파싱」하는 찢어진 프레임이 생긴다. 한 채널에 순서대로 실으면 reader 루프의
/// 기존 「read 직후 크기 재확인」이 그대로 순서를 보장한다.
pub enum ExtEvent {
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
                return Ok(n);
            }
            match self.events.recv() {
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

pub struct PtySession {
    /// Channel the renderer consumes — one ScreenUpdate per dirty
    /// frame after VT processing landed new state.
    pub screens: Receiver<ScreenUpdate>,
    io: SessionIo,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
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
    /// 우리가 파싱한 **셀 그리드**를 그대로 받는 소비자를 위한 tee. `byte_taps` 가
    /// 바이트를 주는 것과 달리 여기로는 `ScreenUpdate` 가 간다 — 받는 쪽이 자기 VT
    /// 파서 없이 화면을 그린다(웹텀). `screens` 채널을 대신 쓸 수는 없다: 그건 MPMC라
    /// 구독자가 늘면 GUI 와 프레임을 **나눠 갖게 되어** 네이티브 화면이 깨진다.
    screen_taps: Arc<Mutex<Vec<Sender<ScreenUpdate>>>>,
    /// 셀-흐름 인라인 이미지(OSC 1337) 기록. 리더 스레드가 채우고, 스냅샷을
    /// 만드는 모든 경로(reader·scroll·full_snapshot)가 뷰포트 배치로 환산해
    /// ScreenUpdate 에 싣는다.
    inline_imgs: Arc<Mutex<InlineImgs>>,
    /// 앱이 DECSET 2031(컬러스킴 변경 알림)을 켰는가 — 리더 스레드가 raw
    /// 배치에서 `CSI ?2031h/l` 을 잡아 세운다. 구독한 앱(claude 의
    /// `theme: auto` 등)에게만 테마 전환 때 `CSI ?997;N n` 리포트를 보낸다 —
    /// 구독 안 한 셸에 보내면 입력줄에 이스케이프 쓰레기가 박힌다.
    scheme_reports: Arc<std::sync::atomic::AtomicBool>,
    /// reader 스레드 정지 신호 — 핸드오프(fd 를 다른 프로세스로 넘기기) 직전에
    /// 세운다. 안 세우고 넘기면 커널이 다음 출력 청크를 **이쪽** read 에 줘 버려,
    /// 넘긴 뒤의 화면이 두 소비자에게 갈라진다.
    reader_stop: Arc<std::sync::atomic::AtomicBool>,
    /// true 면 Drop 이 child 를 죽이지 않는다 — 핸드오프로 소유권이 나간 세션.
    kill_disarmed: std::sync::atomic::AtomicBool,
    /// 마지막으로 CR/LF 가 이 PTY 로 들어간 시각 — 「방금 제출됐다」 신호.
    /// GUI 의 스피너 즉시-신뢰(턴 시작 첫 프레임부터 학생 테마)가 읽는다.
    /// 키보드·paste·소켓 send·하네스 autosend 모든 쓰기 경로가 `send_bytes`
    /// 하나로 모이므로 여기가 정본이다.
    last_submit: Mutex<Option<Instant>>,
    /// 출력 박동 — 리더가 백엔드에서 **실제 바이트를 읽은** 시각들(≥250ms 간격만,
    /// 최근 8개). scroll()·resize 재스냅샷은 리더 read 가 아니라 안 찍힌다.
    /// 「이 pane 에 출력이 흐르는가」의 글리프-독립 정본: 에이전트는 작업 중이면
    /// 스피너 경과시간을 1초마다 다시 그려 바이트가 꾸준히 흐르고, 놀면 조용하다.
    /// GUI 의 working 판정(`output_heartbeat`)이 읽는다 — 스피너 글리프가 또
    /// 바뀌어도(윈도우 `*`·점 프레임·reduce motion `●` 전례 셋) 상태 판정이 살게.
    output_beats: Arc<Mutex<VecDeque<Instant>>>,
    /// 마지막으로 **아무 바이트든** 이 PTY 로 들어간 시각(포커스 리포트 제외).
    /// 타이핑·화살표·마우스 SGR 의 에코 재그리기가 출력 박동으로 읽히는 것을
    /// 막는 억제 신호 — `output_heartbeat` 가 이 직후 1.5초는 박동을 안 믿는다.
    last_input: Mutex<Option<Instant>>,
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
        // claude 의 렌더러는 **건드리지 않는다** — no-flicker(대체화면)로 둔다.
        //
        // 두 길의 맞바꿈은 이렇다. classic 으로 돌리면 대화가 이 터미널의 스크롤백에
        // 쌓여, 「맨 위 질문 고정」 띠를 절대 줄 번호로 정확히 그릴 수 있다
        // (`turnjump.rs`). no-flicker 는 스크롤이 claude 안에서만 일어나 터미널이
        // 위치를 몰라, 그 띠를 화면 글자로 짐작해야 한다(`find_sticky_prompt`).
        // 대신 no-flicker 는 다시 그릴 때 깜빡이지 않고 입력창이 늘 제자리에 있다.
        //
        // 2026-08-30~31 사이에 이 값을 세 번 뒤집었다(강제 → 해제 → 강제 → 해제).
        // 마지막이 정본이다 — 띠의 정확도보다 화면 안정성을 고른다(2026-08-31 지시:
        // "켜고싶어"). **다음에 「띠가 엉뚱한 질문을 문다」는 얘기가 나와도 이 값을
        // 되돌리지 마라.** 그 길은 이미 두 번 가 봤고, 되돌리면 그때마다 입력창과
        // 깜빡임을 다시 잃는다. 고칠 곳은 짐작하는 쪽(`find_sticky_prompt` ·
        // `pick_scrolled_past_prompt`)이다.
        //
        // classic 을 보고 싶으면 `KASATERM_CLAUDE_CLASSIC=1`. 그러면 정확한 띠와
        // 함께, 스크롤을 올려도 입력창을 맨 아래에 붙잡는 보조(render.rs 의
        // `pinned_input_rows`)가 깨어난다 — 08-31 실측으로 둘 다 확인했다.
        // `CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN` 을 손으로 정해 뒀으면 그 값이 이긴다.
        let classic_on = std::env::var("KASATERM_CLAUDE_CLASSIC")
            .is_ok_and(|v| matches!(v.trim(), "1" | "on" | "true"));
        if classic_on && std::env::var_os("CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN").is_none() {
            cmd.env("CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN", "1");
        }
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
            respond: true,
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
        let screen_taps: Arc<Mutex<Vec<Sender<ScreenUpdate>>>> = Arc::new(Mutex::new(Vec::new()));
        let inline_imgs: Arc<Mutex<InlineImgs>> = Arc::new(Mutex::new(InlineImgs::default()));
        let scheme_reports = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let output_beats: Arc<Mutex<VecDeque<Instant>>> = Arc::new(Mutex::new(VecDeque::new()));
        let reader_thread = spawn_reader_thread(
            reader,
            Arc::clone(&reader_stop),
            poll_fd,
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
            Arc::clone(&screen_taps),
            Arc::clone(&inline_imgs),
            Arc::clone(&scheme_reports),
            Arc::clone(&output_beats),
        );

        Ok(Self {
            screens: rx,
            io: SessionIo::Local {
                master,
                child: Arc::new(Mutex::new(child)),
            },
            writer: writer_arc,
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
            screen_taps,
            title_handle,
            pane_id: opts.pane_id.clone(),
            tty_short,
            blocks,
            cwd_handle,
            inline_imgs,
            scheme_reports,
            reader_stop,
            kill_disarmed: std::sync::atomic::AtomicBool::new(false),
            last_submit: Mutex::new(None),
            output_beats,
            last_input: Mutex::new(None),
        })
    }

    /// 원격 호스트가 소유한 PTY 의 **로컬 파서 세션**을 만든다.
    ///
    /// `start` 와 배관(파서·스냅샷·tap·scrollback)이 같고 다른 것은 전송뿐이다 —
    /// 바이트는 `io.events` 로 들어오고, 입력은 `io.writer` 로 나가며, resize 는
    /// `io.on_resize` 로 원격에 알린다. `opts` 의 shell/env/initial_scrollback 은
    /// 원격 호스트 소관이라 여기선 무시된다. shell_pid 가 None 이라 ps 기반
    /// 판정(active_agent 등)은 우아하게 비활성이다.
    ///
    /// ⚠️ `start` 와 필드 조립이 쌍이다 — PtySession 에 필드를 더하면 컴파일러가
    /// 양쪽 다 잡아 주지만, 초기값의 의미(왜 None/빈값인지)는 여기 주석에 적어라.
    pub fn start_external(opts: PtyOptions, io: ExternalIo) -> Result<Self> {
        let (tx, rx) = bounded::<ScreenUpdate>(256);
        let size = Arc::new(Mutex::new((opts.cols, opts.rows)));
        let writer_arc: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(io.writer));
        let blocks: Arc<Mutex<VecDeque<CommandBlock>>> = Arc::new(Mutex::new(VecDeque::new()));
        let title_handle = Arc::new(Mutex::new(None));
        let cwd_handle: Arc<Mutex<Option<std::path::PathBuf>>> = Arc::new(Mutex::new(None));
        let listener = PtyEventForwarder {
            writer: Arc::clone(&writer_arc),
            size: Arc::clone(&size),
            last_title: Arc::clone(&title_handle),
            respond: false,
        };
        let term = Arc::new(Mutex::new(make_term(opts.cols, opts.rows, listener)));
        let byte_taps: Arc<Mutex<Vec<Sender<Vec<u8>>>>> = Arc::new(Mutex::new(Vec::new()));
        let screen_taps: Arc<Mutex<Vec<Sender<ScreenUpdate>>>> =
            Arc::new(Mutex::new(Vec::new()));
        let inline_imgs: Arc<Mutex<InlineImgs>> = Arc::new(Mutex::new(InlineImgs::default()));
        let scheme_reports = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader = Box::new(ExtReader {
            events: io.events,
            size: Arc::clone(&size),
            pending: Vec::new(),
        });
        // ExtReader 는 채널이라 poll 대상이 없다 — 정지는 채널 닫힘이 대신한다.
        let reader_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let output_beats: Arc<Mutex<VecDeque<Instant>>> = Arc::new(Mutex::new(VecDeque::new()));
        let reader_thread = spawn_reader_thread(
            reader,
            Arc::clone(&reader_stop),
            None,
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
            Arc::clone(&screen_taps),
            Arc::clone(&inline_imgs),
            Arc::clone(&scheme_reports),
            Arc::clone(&output_beats),
        );
        Ok(Self {
            screens: rx,
            io: SessionIo::External {
                on_resize: io.on_resize,
            },
            writer: writer_arc,
            size,
            _reader_thread: reader_thread,
            shell_pid: None,
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
            screen_taps,
            title_handle,
            pane_id: opts.pane_id.clone(),
            tty_short: None,
            blocks,
            cwd_handle,
            inline_imgs,
            scheme_reports,
            reader_stop,
            kill_disarmed: std::sync::atomic::AtomicBool::new(false),
            last_submit: Mutex::new(None),
            output_beats,
            last_input: Mutex::new(None),
        })
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
        let (tx, rx) = bounded::<ScreenUpdate>(256);
        let size = Arc::new(Mutex::new((opts.cols, opts.rows)));
        let writer_arc: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(writer));
        let blocks: Arc<Mutex<VecDeque<CommandBlock>>> = Arc::new(Mutex::new(VecDeque::new()));
        let title_handle = Arc::new(Mutex::new(None));
        let cwd_handle: Arc<Mutex<Option<std::path::PathBuf>>> = Arc::new(Mutex::new(None));
        let listener = PtyEventForwarder {
            writer: Arc::clone(&writer_arc),
            size: Arc::clone(&size),
            last_title: Arc::clone(&title_handle),
            // 입양자가 이제 유일한 호스트다 — 자동 응답도 이쪽 몫.
            respond: true,
        };
        let term = Arc::new(Mutex::new(make_term(opts.cols, opts.rows, listener)));
        if !opts.initial_scrollback.is_empty() {
            let mut proc: Processor<StdSyncHandler> = Processor::new();
            let mut t = term.lock().unwrap();
            for line in &opts.initial_scrollback {
                proc.advance(&mut *t, line.as_bytes());
                proc.advance(&mut *t, b"\r\n");
            }
        }
        let byte_taps: Arc<Mutex<Vec<Sender<Vec<u8>>>>> = Arc::new(Mutex::new(Vec::new()));
        let screen_taps: Arc<Mutex<Vec<Sender<ScreenUpdate>>>> =
            Arc::new(Mutex::new(Vec::new()));
        let inline_imgs: Arc<Mutex<InlineImgs>> = Arc::new(Mutex::new(InlineImgs::default()));
        let scheme_reports = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let output_beats: Arc<Mutex<VecDeque<Instant>>> = Arc::new(Mutex::new(VecDeque::new()));
        let reader_thread = spawn_reader_thread(
            reader,
            Arc::clone(&reader_stop),
            Some(raw),
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
            Arc::clone(&screen_taps),
            Arc::clone(&inline_imgs),
            Arc::clone(&scheme_reports),
            Arc::clone(&output_beats),
        );
        Ok(Self {
            screens: rx,
            io: SessionIo::Adopted { fd, child_pid },
            writer: writer_arc,
            size,
            _reader_thread: reader_thread,
            shell_pid: child_pid,
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
            screen_taps,
            title_handle,
            pane_id: opts.pane_id.clone(),
            tty_short: None,
            blocks,
            cwd_handle,
            inline_imgs,
            scheme_reports,
            reader_stop,
            kill_disarmed: std::sync::atomic::AtomicBool::new(false),
            last_submit: Mutex::new(None),
            output_beats,
            last_input: Mutex::new(None),
        })
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

    /// 이 pane 의 앱이 DECSET 2031(컬러스킴 변경 알림)을 켰는가. 테마 전환 때
    /// 여기 참인 pane 에만 `CSI ?997;1n`(다크)/`;2n`(라이트) 리포트를 보낸다.
    pub fn wants_scheme_reports(&self) -> bool {
        self.scheme_reports
            .load(std::sync::atomic::Ordering::Relaxed)
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
        let best_child = descend_launchers(&table, best_child);
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
        // 포커스 리포트(CSI I/O)는 pane 전환마다 앱이 자동으로 쏘는 것이라 사람
        // 입력이 아니다 — 이걸 세면 working pane 으로 포커스를 옮길 때마다 박동
        // 억제가 걸려 바가 1.5초 꺼졌다 켜진다.
        if bytes != b"\x1b[I" && bytes != b"\x1b[O" {
            *self.last_input.lock().unwrap() = Some(Instant::now());
        }
        {
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
        }
        if bytes.iter().any(|b| matches!(b, b'\r' | b'\n')) {
            *self.last_submit.lock().unwrap() = Some(Instant::now());
            // Enter 는 「새 전경 프로세스가 곧 뜬다」의 가장 이른 신호이기도 하다 —
            // 테이블을 앞당겨 읽어 두면 `claude` 를 친 pane 이 배너 첫 프레임부터
            // 에이전트로 판정된다(process_table_poke 머리말).
            process_table_poke();
        }
        Ok(())
    }

    /// 마지막 CR/LF 가 이 PTY 로 들어간 시각 — 없으면 아직 아무 제출도 없었다.
    pub fn last_submit(&self) -> Option<Instant> {
        *self.last_submit.lock().unwrap()
    }

    /// 백엔드에 출력이 **박자 있게** 흐르는 중인가 — 글리프와 무관한 working 신호.
    /// 에이전트는 생성 중이면 스피너 경과시간을 1초마다 다시 그려 박동이 1Hz 로
    /// 잡히고, 놀면 조용하다. 판정: 최근 3.5초 창 안에 박동 2개 이상 + 그 폭이
    /// 0.8초 이상(단발 burst — 알림 도착·재스냅샷 — 배제) + 최신이 2.2초 안(1Hz
    /// 틱 두 번 + 여유) + 최근 1.5초 입력 없음(타이핑·스크롤 에코 배제).
    /// **OR 전용으로 써라** — busy 를 세울 수만 있고 내리는 근거는 못 된다.
    pub fn output_heartbeat(&self) -> bool {
        self.heartbeat_within(2200)
    }

    /// `output_heartbeat` 의 빡빡한 판 — 최신 박동 1.2초 안. 도트 **위치**의
    /// 관대한 스캔(글리프 모르는 행 잡기)을 여는 열쇠로 쓴다: 턴이 끝난 직후
    /// 박동 여열로 본문 마지막 줄에 도트가 서는 창을 1.2초로 줄인다.
    pub fn output_heartbeat_fresh(&self) -> bool {
        self.heartbeat_within(1200)
    }

    fn heartbeat_within(&self, newest_ms: u64) -> bool {
        let now = Instant::now();
        if self
            .last_input
            .lock()
            .unwrap()
            .is_some_and(|t| now.duration_since(t).as_millis() < 1500)
        {
            return false;
        }
        let beats = self.output_beats.lock().unwrap();
        let mut oldest: Option<Instant> = None;
        let mut newest: Option<Instant> = None;
        for t in beats.iter() {
            if now.duration_since(*t).as_millis() < 3500 {
                if oldest.is_none() {
                    oldest = Some(*t);
                }
                newest = Some(*t);
            }
        }
        let (Some(o), Some(n)) = (oldest, newest) else {
            return false;
        };
        o != n
            && now.duration_since(n).as_millis() < u128::from(newest_ms)
            && n.duration_since(o).as_millis() >= 800
    }

    /// Scroll the view through alacritty's scrollback by `lines`
    /// (positive = toward older history / up, negative = toward the
    /// live tail / down). Re-snapshots immediately and pushes the
    /// frame so the renderer reflects the new position without waiting
    /// for PTY output — important for an idle TUI like claude. Returns
    /// the resulting display offset (0 = at the live bottom).
    pub fn scroll(&self, lines: i32) -> usize {
        // 스크롤도 사람 상호작용이다 — 박동 억제를 걸어, 스크롤 재스냅샷·TUI
        // 재그리기가 working 으로 읽히지 않게 한다(send_bytes 의 last_input 참고).
        *self.last_input.lock().unwrap() = Some(Instant::now());
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
        let mut update = snapshot(
            &mut t,
            cols,
            rows,
            &self.pane_id,
            &self.title_handle,
            true,
        );
        attach_inline_views(&mut update, &t, &self.inline_imgs);
        self.publish_screen(update);
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

    /// 스크롤백 + 현재 화면을 텍스트 줄로. 뒤에서 `max_lines` 만큼(최신 우선).
    ///
    /// 세션 저장이 쓰는 경로다. 예전엔 GUI 가 프레임 diff 로 「몇 줄 밀렸나」를 추측해
    /// 자체 history 를 쌓았는데, 그 추측이 scroll-region TUI 를 깨뜨려 폐기되면서
    /// (`apply_screen_update`: "Shift detection on the pty side is retired") **아무도 그
    /// history 를 안 채우게 됐다.** 그 뒤로 저장되는 스크롤백은 늘 화면 한 장뿐이라,
    /// 재시작하면 그 전 대화가 통째로 사라진다 — 저장 상한(`SCROLLBACK_SAVE_MAX`)이나
    /// 버퍼 예산(`KASATERM_SCROLLBACK_MB`)을 올려도 소용이 없었다(2026-08-11 실측:
    /// 400줄을 뿌린 pane 의 저장 스크롤백이 38줄 = 화면 크기 그대로).
    ///
    /// 진짜 스크롤백은 여기, alacritty grid 가 갖고 있다. 그래서 추측을 되살리는 대신
    /// 그것을 직접 읽는다.
    pub fn scrollback_text(&self, max_lines: usize) -> Vec<String> {
        let t = self.term.lock().unwrap();
        let grid = t.grid();
        let cols = grid.columns();
        let hist = grid.history_size();
        let screen = grid.screen_lines();
        let total = hist + screen;
        let take = max_lines.min(total);
        let mut out = Vec::with_capacity(take);
        // grid 의 줄 번호는 화면 첫 줄이 0 이고 스크롤백이 음수다.
        for i in (total - take)..total {
            let line = i as i32 - hist as i32;
            let mut row = String::with_capacity(cols);
            for c in 0..cols {
                let point = Point::new(
                    alacritty_terminal::index::Line(line),
                    alacritty_terminal::index::Column(c),
                );
                let ch = grid[point].c;
                row.push(if ch == '\0' { ' ' } else { ch });
            }
            out.push(row.trim_end().to_string());
        }
        out
    }

    /// Jump straight to the live tail (display offset 0).
    pub fn scroll_to_bottom(&self) {
        let (cols, rows) = *self.size.lock().unwrap();
        let mut t = self.term.lock().unwrap();
        t.scroll_display(alacritty_terminal::grid::Scroll::Bottom);
        let mut update = snapshot(
            &mut t,
            cols,
            rows,
            &self.pane_id,
            &self.title_handle,
            true,
        );
        attach_inline_views(&mut update, &t, &self.inline_imgs);
        self.publish_screen(update);
    }

    /// 뷰포트가 스크롤백 어디에 있나 — `(display_offset, history_size)`.
    ///
    /// 락만 잡는 싼 질의라 매 프레임 물어도 된다. 비싼 `prompt_anchors` 를 언제
    /// 다시 돌릴지도 이 값으로 정한다(히스토리 길이가 그대로면 앵커도 그대로다).
    pub fn view_state(&self) -> (usize, usize) {
        let t = self.term.lock().unwrap();
        let g = t.grid();
        (g.display_offset(), g.history_size())
    }

    /// 뷰포트 바로 위 스크롤백 행들 — 가까운 순([0] = 뷰포트 위 1줄), 최대 `n` 개.
    ///
    /// 긴 팀메시지를 스크롤해 내려가면 헤더가 화면 위로 나가, 렌더러가 화면에
    /// 남은 본문을 그 메시지로 이어 붙이려면 위를 올려다볼 창이 필요하다.
    /// 스냅샷에 태워 보내지 않는 이유: 스냅샷은 PTY 출력마다 도는 자리라 상시
    /// 비용이 되는데 이 행들은 대부분의 프레임에 쓸모가 없다 — 필요할 때만
    /// 락 잡고 읽는다.
    pub fn rows_above(&self, n: usize) -> Vec<Row> {
        read_rows_above(&self.term.lock().unwrap(), n)
    }

    /// 살아 있는 화면의 마지막 `n` 행 — 스크롤을 올려 둔 상태에서도 같은 값이다.
    ///
    /// 대체화면을 안 쓰는 claude 는 입력창이 대화의 마지막 줄일 뿐이라, 스크롤을
    /// 올리면 함께 위로 밀려난다. 렌더러가 이 행들을 떠다 뷰포트 맨 아래에 덮어
    /// 입력창을 붙잡아 둔다.
    pub fn live_tail_rows(&self, n: usize) -> Vec<Row> {
        read_live_tail(&self.term.lock().unwrap(), n)
    }

    /// 스크롤백에 남은 **사용자 프롬프트 줄**을 절대 줄 번호와 함께 모은다.
    ///
    /// claude 는 확정된 프롬프트를 `❯ <내용>` 한 줄로 남기고, 화면 하단 입력창은
    /// 같은 마커를 쓰되 뒤에 **NBSP**(U+00A0)를 넣는다 — 2026-08-15 살아 있는
    /// pane 9개를 떠서 확정했다(확정된 것은 U+0020, 입력 중인 것은 U+00A0).
    /// 그 한 글자가 「지나간 질문」과 「지금 치고 있는 것」을 가르는 유일한 표시라,
    /// 마커만 보고 잡으면 입력창이 늘 목록 끝에 끼어든다.
    ///
    /// 비용을 감당하려고 **열 0 만** 훑는다. 히스토리 상한이 10만 줄이라 전 셀을
    /// 보면 2천만 셀이지만, 마커는 반드시 행 머리에 있으므로 10만 번 인덱싱이면
    /// 끝나고 걸린 줄만 실제로 읽는다.
    pub fn prompt_anchors(&self) -> Vec<PromptAnchor> {
        scan_prompt_anchors(&self.term.lock().unwrap())
    }

    /// 절대 줄 `abs` 가 뷰포트 맨 위에 오도록 **한 번에** 이동한다.
    ///
    /// 좌표가 확정이라 정확히 닿는다 — 휠을 한 노치씩 쏘며 목표 텍스트가 화면에
    /// 나타나는지 지켜보던 방식(mouse-tracking TUI 용 `sticky_seek`)과 달리
    /// 되짚기가 없다. 반환값은 이동 뒤의 display offset.
    pub fn scroll_to_abs(&self, abs: i64) -> usize {
        let (cols, rows) = *self.size.lock().unwrap();
        let mut t = self.term.lock().unwrap();
        let hist = t.grid().history_size() as i64;
        let before = t.grid().display_offset();
        let want = (hist - abs).clamp(0, hist) as usize;
        if want == before {
            return before;
        }
        t.scroll_display(alacritty_terminal::grid::Scroll::Delta(
            want as i32 - before as i32,
        ));
        let after = t.grid().display_offset();
        let mut update =
            snapshot(&mut t, cols, rows, &self.pane_id, &self.title_handle, true);
        attach_inline_views(&mut update, &t, &self.inline_imgs);
        self.publish_screen(update);
        after
    }

    /// Build a full-grid ScreenUpdate (every row) without touching the live
    /// channel — the daemon calls this on attach to seed a freshly-connected
    /// GUI with the complete current screen before live dirty frames resume.
    pub fn full_snapshot(&self) -> ScreenUpdate {
        let (cols, rows) = *self.size.lock().unwrap();
        let mut t = self.term.lock().unwrap();
        let mut update =
            snapshot(&mut t, cols, rows, &self.pane_id, &self.title_handle, true);
        attach_inline_views(&mut update, &t, &self.inline_imgs);
        update
    }

    pub fn publish_full_snapshot(&self) {
        self.publish_screen(self.full_snapshot());
    }

    /// GUI 채널과 모든 그리드 tap 에 한 프레임을 내보낸다.
    fn publish_screen(&self, update: ScreenUpdate) {
        let _ = publish_screen_update(&self.screens_tx, &self.screen_taps, update);
    }

    /// 셀 그리드를 구독하면서 "지금 화면" 전체를 함께 받는다. 받는 쪽에는 VT 파서가
    /// 필요 없다 — 그리드를 그대로 그리면 된다(웹텀이 xterm.js 없이 도는 근거).
    ///
    /// ⚠️ 스냅샷 채취와 구독 등록이 `term` 락 하나 안에서 끝나야 하는 이유는
    /// `tap_bytes_with_snapshot` 와 같다 — 둘로 나누면 그 사이 프레임이 스냅샷에도
    /// tap 에도 없이 사라진다. 그래서 `full_snapshot` 을 부르지 않고 본문을 편다.
    pub fn tap_screens_with_snapshot(&self) -> (Receiver<ScreenUpdate>, ScreenUpdate) {
        let (cols, rows) = *self.size.lock().unwrap();
        let mut t = self.term.lock().unwrap();
        let mut snap = snapshot(&mut t, cols, rows, &self.pane_id, &self.title_handle, true);
        attach_inline_views(&mut snap, &t, &self.inline_imgs);
        let (tx, rx) = crossbeam_channel::bounded(64);
        self.screen_taps.lock().unwrap().push(tx);
        (rx, snap)
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

    /// 구독하면서 "지금 화면"을 ANSI 로 함께 받는다. 돌려준 바이트를 tap 스트림
    /// 보다 **먼저** 보내면 붙는 즉시 화면이 찬다.
    ///
    /// `tap_bytes` 만으로는 붙은 뒤의 출력만 오므로, 조용한 pane 에 미러로 붙으면
    /// 다음 출력이 날 때까지 화면이 빈 채였다(사용자가 Enter 를 쳐야 프롬프트가
    /// 보였다).
    ///
    /// ⚠️ 스냅샷 채취와 구독 등록은 `term` 락 하나 안에서 끝나야 한다. 둘로 나누면
    /// 그 사이의 출력이 스냅샷에도 tap 에도 없이 사라지거나(유실), 양쪽에 다 담겨
    /// 두 번 그려진다(중복 — `abc` 뒤에 `c` 가 또 찍히는 식). reader 도 같은 락
    /// 안에서 뿌리므로(`spawn_reader_thread`) 이 순서면 어느 쪽도 일어나지 않는다.
    pub fn tap_bytes_with_snapshot(&self) -> (Receiver<Vec<u8>>, Vec<u8>) {
        let (cols, rows) = *self.size.lock().unwrap();
        let mut t = self.term.lock().unwrap();
        let hist = history_ansi(&t, cols, rows);
        let snap = snapshot(&mut t, cols, rows, &self.pane_id, &self.title_handle, true);
        let (tx, rx) = crossbeam_channel::bounded(64);
        self.byte_taps.lock().unwrap().push(tx);
        // 스크롤백은 primary 화면의 것이다 — alt 화면(vim 등)에 붙는 미러에
        // 실으면 ?1049h 앞에 찍혀 primary 를 더럽힌다.
        let mut bytes = if snap.alt_screen { Vec::new() } else { hist };
        bytes.extend_from_slice(&snap.to_ansi());
        (rx, bytes)
    }
    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        // alacritty 격자 하한(MIN_COLUMNS=2) 밑을 부르면 wide 글자(한글) reflow 가
        // upstream 이 한 번도 안 밟는 경로로 들어간다 — 호출자(GUI floor 1열·
        // auxwin·웹텀)가 최소를 제각각 계산하므로 여기서 한 번에 막는다.
        let (cols, rows) = (cols.max(2), rows.max(1));
        if self.size() == (cols, rows) {
            return Ok(());
        }
        // Kernel-side PTY first (child sees SIGWINCH). External 은 ioctl 대신
        // 제어 콜백으로 원격에 알리고, 아래 로컬 격자는 낙관적으로 먼저 맞춘다 —
        // 원격이 실제로 바꾸면 full snapshot 이 따라와 어긋남을 스스로 치유한다.
        match &self.io {
            SessionIo::Local { master, .. } => {
                let pty = master.lock().unwrap();
                pty.resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .context("pty resize")?;
            }
            SessionIo::External { on_resize } => (on_resize)(cols, rows),
            #[cfg(unix)]
            SessionIo::Adopted { fd, .. } => {
                use std::os::fd::AsRawFd;
                let ws = libc::winsize {
                    ws_row: rows,
                    ws_col: cols,
                    ws_xpixel: 0,
                    ws_ypixel: 0,
                };
                // TIOCSWINSZ — SIGWINCH 가 자식에게 간다. 실패는 격자만 로컬 적용.
                unsafe {
                    let _ = libc::ioctl(fd.as_raw_fd(), libc::TIOCSWINSZ, &ws);
                }
            }
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
        // A quiet full-screen TUI may emit nothing after SIGWINCH. Publish the
        // reshaped grid ourselves so the GUI cannot keep clipping an old,
        // larger snapshot until a wheel/key event happens to dirty the pane.
        self.publish_full_snapshot();
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
                if let Ok(mut child) = child.lock() {
                    let _ = child.kill();
                }
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
    /// false = 자동 응답(DSR-CPR·OSC 색 질의·TextAreaSize·클립보드 read)을 묻는다.
    /// 원격 미러(`start_external`)용 — 원격 호스트의 Term 이 이미 답하고 있어서,
    /// 여기서도 답하면 원격 앱이 응답을 두 번 받아 입력줄에 이스케이프가 박힌다.
    /// (아무도 안 답하면 cmd.exe 류가 DSR 대기로 멎는 함정이 있지만, 그 「한 명」은
    /// 원격 쪽이다 — 렌더버그 카탈로그 #10 참고.)
    respond: bool,
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
            respond: self.respond,
        }
    }
}

impl PtyEventForwarder {
    fn write_to_pty(&self, bytes: &[u8]) {
        // 자동 응답의 유일한 출구 — 음소거는 여기 한 곳이면 전 경로(PtyWrite·
        // ColorRequest·TextAreaSize·ClipboardLoad)가 막힌다.
        if !self.respond {
            return;
        }
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

/// 보는 사람이 없어도 살려 둘 세션들.
///
/// `registry` 는 `Weak` 라서 **소유자가 사라지면 세션도 사라진다.** 웹에서 띄운
/// 셸은 소유자가 그 WebSocket 하나뿐이라, 탭을 닫는 순간 셸까지 죽었다. 여기에
/// 강한 `Arc` 를 두면 연결과 수명이 갈린다 — 폰을 덮었다 다시 열어도 하던 작업이
/// 그대로 있다.
fn persistent() -> &'static Mutex<std::collections::HashMap<String, Arc<PtySession>>> {
    static P: std::sync::OnceLock<Mutex<std::collections::HashMap<String, Arc<PtySession>>>> =
        std::sync::OnceLock::new();
    P.get_or_init(Default::default)
}

/// 세션을 프로세스에 붙들어 둔다. 셸이 끝나면(EOF) 스스로 빠진다.
///
/// ⚠️ **`screens` 를 소비하므로 GUI pane 에는 쓰면 안 된다.** 그 채널은 MPMC 라
/// 여기서 받은 프레임은 GUI pump 에 안 간다 — 화면이 띄엄띄엄 갱신된다. 보는
/// 사람이 따로 없는 웹 전용 셸에만 쓴다.
pub fn keep_session(id: &str, sess: Arc<PtySession>) {
    let watch = sess.screens.clone();
    persistent()
        .lock()
        .unwrap()
        .insert(id.to_string(), sess);
    let id = id.to_string();
    // 셸이 끝나면 스스로 빠진다 — 안 그러면 죽은 세션이 목록에 영원히 남는다.
    std::thread::Builder::new()
        .name(format!("pty-keep-{id}"))
        .spawn(move || {
            while let Ok(u) = watch.recv() {
                if u.eof {
                    break;
                }
            }
            persistent().lock().unwrap().remove(&id);
        })
        .ok();
}

/// 붙들어 둔 세션을 놓아 준다. 마지막 참조였다면 셸이 종료된다.
pub fn release_session(id: &str) -> bool {
    persistent().lock().unwrap().remove(id).is_some()
}

/// 붙들려 있는 세션 id 목록(정렬).
pub fn kept_sessions() -> Vec<String> {
    let mut ids: Vec<String> = persistent().lock().unwrap().keys().cloned().collect();
    ids.sort();
    ids
}

#[allow(clippy::too_many_arguments)]
/// `ScreenUpdate` 한 프레임을 GUI 채널과 그리드 tap 구독자 모두에게 내보낸다.
///
/// 구독자가 없으면 clone 을 아예 안 하므로 평소 비용은 lock 하나다. 밀린 구독자를
/// **버리는 게 아니라 끊는** 정책은 byte tap 과 같은 이유다 — dirty diff 스트림이라
/// 중간 프레임을 흘리면 받는 쪽 화면이 복구 불능으로 어긋난다. 끊으면 재연결해서
/// 전체 스냅샷부터 다시 받으므로 조용히 깨진 화면보다 낫다.
///
/// ⚠️ 전부 `try_send` 다 — 여기서 블로킹하면 reader 가 멎고 셸이 backpressure 를 먹어
/// 터미널 전체가 느려진다(`spawn_reader_thread` 의 tee 주석 참고).
fn publish_screen_update(
    tx: &Sender<ScreenUpdate>,
    taps: &Arc<Mutex<Vec<Sender<ScreenUpdate>>>>,
    update: ScreenUpdate,
) -> Result<(), crossbeam_channel::TrySendError<ScreenUpdate>> {
    {
        let mut subs = taps.lock().unwrap();
        if !subs.is_empty() {
            subs.retain(|sub| sub.try_send(update.clone()).is_ok());
        }
    }
    tx.try_send(update)
}

fn spawn_reader_thread(
    mut reader: Box<dyn Read + Send>,
    reader_stop: Arc<std::sync::atomic::AtomicBool>,
    poll_fd: Option<i32>,
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
    screen_taps: Arc<Mutex<Vec<Sender<ScreenUpdate>>>>,
    inline_imgs: Arc<Mutex<InlineImgs>>,
    scheme_reports: Arc<std::sync::atomic::AtomicBool>,
    output_beats: Arc<Mutex<VecDeque<Instant>>>,
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
        // img_buf/img_capturing 은 레거시 탭 모드 전용, inline_scan 은 셀-흐름
        // 모드 전용 — 같은 시퀀스를 두 경로가 동시에 잡지 않는다.
        let mut img_buf: Vec<u8> = Vec::new();
        let mut img_capturing = false;
        let mut inline_scan = InlineScan::default();
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
            // 핸드오프 정지 게이트. fd 를 다른 프로세스로 넘기기 전에 reader 가
            // 물러나야 커널이 다음 청크를 새 주인에게 준다. EOF 센티널은 안
            // 보낸다 — pane 은 원격 모드로 계속 살므로, 보내면 GUI 가 pane 을
            // 걷어 버린다. poll 티크(250ms)마다 재확인하니 정지는 그 안에 든다.
            if reader_stop.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            #[cfg(unix)]
            if let Some(pfd) = poll_fd {
                let mut p = libc::pollfd {
                    fd: pfd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                let r = unsafe { libc::poll(&mut p, 1, 250) };
                if r == 0 {
                    continue; // 타임아웃 — stop 재확인(루프 머리의 크기 재확인 포함)
                }
                // r<0(EINTR 등)은 read 가 판정하게 둔다
            }
            #[cfg(not(unix))]
            let _ = poll_fd;
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
                    // 그리드 구독자(원격 거울의 WS 핸들러)에게도 EOF 를 알린다 — 이
                    // 센티널은 위 `tx` 로만 가고 tap 은 못 받아서, 그쪽 핸들러가 세션
                    // Arc 를 쥔 채 프레임을 끝없이 기다렸다. 그 Arc 때문에 세션이 안
                    // 죽고 명부에도 남아, 거울 pane 이 「exit」 화면 그대로 45초 넘게
                    // 서 있었다(2026-09-02 실측, `mini` 거울). 바이트 tap(앱 거울·xterm)은
                    // 센티널을 실을 수 없으니 송신자를 통째로 놓는다 — 구독자 쪽 recv 가
                    // 끊겨 같은 뜻이 된다. 둘 다 세션이 아니라 **이 스레드**가 끝나는
                    // 순간에 해야 한다: 송신자 목록은 세션 소유라 세션이 살아 있는 한
                    // 저절로는 안 떨어진다.
                    {
                        let mut subs = screen_taps.lock().unwrap();
                        subs.retain(|sub| {
                            sub.try_send(ScreenUpdate {
                                pane_id: pane_id.clone(),
                                eof: true,
                                ..Default::default()
                            })
                            .is_ok()
                        });
                    }
                    byte_taps.lock().unwrap().clear();
                    return;
                }
                Ok(n) => {
                    // 출력 박동 — 실제 read 여기 한 곳만 찍는다(struct 필드 주석).
                    // 250ms 안의 연속 청크는 같은 burst 로 보고 한 번만 센다.
                    let now = Instant::now();
                    let mut beats = output_beats.lock().unwrap();
                    if beats
                        .back()
                        .is_none_or(|t| now.duration_since(*t).as_millis() >= 250)
                    {
                        if beats.len() >= 8 {
                            beats.pop_front();
                        }
                        beats.push_back(now);
                    }
                    drop(beats);
                    n
                }
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
            // resize() can run while read() is blocked. Refresh the reader's
            // local dimensions before parsing those newly arrived bytes, or
            // the resulting snapshot is stamped with the previous grid size
            // and can overwrite the correct resize snapshot.
            let want = *size.lock().unwrap();
            if want != current_size {
                let mut t = term.lock().unwrap();
                t.resize(TermSize::new(want.0 as usize, want.1 as usize));
                if want.0 != current_size.0 {
                    t.grid_mut().update_history(history_lines_for_cols(want.0));
                }
                drop(t);
                current_size = want;
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
            let batch = utf8_buf.process(&buf[..n]);
            // NFC 는 배치가 **온전한 UTF-8 일 때만** 돌린다. 깨진 바이트가 섞여 있으면
            // 정규화를 건너뛰고 원본을 그대로 파서에 넘긴다 — 버리지 않는 것이 핵심이다.
            let nfc_holder: Option<String> = if batch.is_ascii() {
                None
            } else {
                std::str::from_utf8(&batch).ok().map(|s| s.nfc().collect())
            };
            let processed_bytes: &[u8] =
                nfc_holder.as_deref().map(str::as_bytes).unwrap_or(batch.as_slice());
            // Sniff for iTerm OSC 1337 inline images / kitty graphics. Both
            // scans walk the byte slice, so we cheaply prefix-check first —
            // most reads have no `\x1b]1337` / `\x1b_G` and we skip the
            // walk entirely. Critical for TUI throughput (claude code emits
            // thousands of small reads per second with neither prefix).
            // 셀-흐름 모드에선 이 시퀀스를 term 락 안의 advance_scanning_inline
            // 이 잡는다(커서 위치가 필요해서다). 여기 레거시 스캔은 탭 모드 전용.
            if !inline_cell_flow()
                && (img_capturing
                    || memchr::memmem::find(processed_bytes, b"\x1b]1337").is_some())
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
            // DECSET 2031 — 컬러스킴 변경 알림 구독. alacritty 는 모르는 private
            // mode 라 조용히 버리므로 raw 배치에서 직접 잡는다(claude 2.1.232
            // 실측: 부팅 init 에 `CSI ?2031h` 가 들어 있다). 위 스니프들처럼
            // 짧고 자기완결이라 read 경계 캐리는 두지 않는다. h 와 l 이 한
            // 배치에 같이 오면 나중 상태인 l 이 이긴다(앱 종료 복원 시퀀스).
            if memchr::memmem::find(processed_bytes, b"\x1b[?2031h").is_some() {
                scheme_reports.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            if memchr::memmem::find(processed_bytes, b"\x1b[?2031l").is_some() {
                scheme_reports.store(false, std::sync::atomic::Ordering::Relaxed);
            }

            let update = {
                let mut t = term.lock().unwrap();
                let follow_live_tail = t.grid().display_offset() == 0;
                // 외부 구독자(브라우저 xterm.js 등)에게 raw 바이트를 그대로 흘린다.
                // 파싱 전 원본이라 받는 쪽은 자기 VT 파서로 독립적으로 그린다.
                //
                // ⚠️ term 락을 **든 채로** 뿌려야 한다. 밖에서 뿌리면 뿌리기와
                // 파싱 사이에 락이 풀린 틈이 생기고, 하필 그 틈에 붙은 미러는 이
                // 청크를 tap 으로도(구독 전이라) 스냅샷으로도(파싱 전이라) 못 받아
                // 그만큼 화면이 어긋난다. `tap_bytes_with_snapshot` 의 원자성이
                // 이 순서에 기대고 있다.
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
                        taps.retain(|sub| sub.try_send(buf[..n].to_vec()).is_ok());
                    }
                }
                if inline_cell_flow()
                    && (inline_scan.capturing
                        || memchr::memmem::find(processed_bytes, b"\x1b]1337").is_some())
                {
                    advance_scanning_inline(
                        &mut processor,
                        &mut t,
                        processed_bytes,
                        &mut inline_scan,
                        &inline_imgs,
                    );
                } else {
                    processor.advance(&mut *t, processed_bytes);
                }
                // alacritty buffers DECSET 2026 synchronized output internally:
                // while its sync buffer is non-empty the Term grid still holds
                // the pre-sync frame, so skip the snapshot until it flushes on
                // ?2026l or the sync timeout — no torn frame ever reaches us.
                if processor.sync_bytes_count() > 0 {
                    None
                } else {
                    // 새 출력은 맨 아래를 보고 있던 사람만 따라간다. 위의 내용을
                    // 읽는 중이라면 alacritty가 늘려 둔 display_offset을 보존해야
                    // 스트리밍 출력마다 화면이 아래로 끌려가지 않는다.
                    if follow_live_tail {
                        t.scroll_display(alacritty_terminal::grid::Scroll::Bottom);
                    }
                    let t_snap = std::time::Instant::now();
                    let mut snap = snapshot(
                        &mut t,
                        current_size.0,
                        current_size.1,
                        &pane_id,
                        &title_handle,
                        false,
                    );
                    attach_inline_views(&mut snap, &t, &inline_imgs);
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
                match publish_screen_update(&tx, &screen_taps, upd) {
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

    /// 끝에 걸린 **잘린 코드포인트만** 남기고 나머지는 바이트 그대로 넘긴다.
    ///
    /// ⚠️깨진 바이트를 버리지 않는 것이 이 함수의 계약이다. 옛 구현은 마지막
    /// 유효 시퀀스까지를 통째로 `from_utf8` 해 보고 실패하면 빈 문자열을 돌려주면서
    /// 그 배치를 **전부** 버렸다. agy(antigravity CLI)가 SGR 이스케이프 사이에 한글
    /// 코드포인트를 끊어 쓰는데(`\xeb\x94` 다음에 바로 `\x1b[`), 그 바이트 하나 때문에
    /// read 한 번이 통째로 증발해 화면에서 프레임이 통으로 빠졌다(2026-08-11 확정).
    /// 깨진 바이트는 VT 파서가 U+FFFD 로 알아서 처리하므로 그냥 흘려보내면 된다.
    fn process(&mut self, data: &[u8]) -> Vec<u8> {
        self.leftover.extend_from_slice(data);
        let n = self.leftover.len();
        let mut cut = n;
        // 잘린 시퀀스는 뒤 3바이트 안에 있다(UTF-8 최대 4바이트). 이어지는 바이트를
        // 거슬러 올라가 선두 바이트를 찾고, 길이가 모자라면 거기서 끊어 보류한다.
        for back in 1..=3.min(n) {
            let i = n - back;
            let b = self.leftover[i];
            if b & 0xc0 == 0x80 {
                continue;
            }
            let width = if b & 0xe0 == 0xc0 {
                2
            } else if b & 0xf0 == 0xe0 {
                3
            } else if b & 0xf8 == 0xf0 {
                4
            } else {
                1
            };
            if width > 1 && i + width > n {
                cut = i;
            }
            break;
        }
        let out = self.leftover[..cut].to_vec();
        self.leftover.drain(..cut);
        out
    }
}

/// 스크롤백을 접속 스냅샷에 싣는 ANSI. 히스토리 줄을 보통 출력처럼 위에서부터
/// 흘리고, 화면에 걸쳐 남은 꼬리를 바닥 행 개행으로 밀어낸 뒤(바닥에서의 개행만
/// 스크롤을 만든다) 이어지는 `to_ansi` 가 화면을 다시 그린다 — 그러면 받는 쪽
/// xterm 은 히스토리를 자기 스크롤백으로 쌓는다. 이게 없으면 미러는 뷰포트만
/// 받아서 폰에서 스와이프해도 올라갈 데가 없다(2026-08-20 확정).
///
/// xterm 기본 스크롤백 상한(1000줄)만큼만 싣는다 — 더 보내도 버려진다.
fn history_ansi(term: &Term<PtyEventForwarder>, cols: u16, rows: u16) -> Vec<u8> {
    const CAP: usize = 1000;
    let hist = term.grid().history_size().min(CAP);
    if hist == 0 {
        return Vec::new();
    }
    let grid = term.grid();
    let grid_cols = grid.columns().min(cols as usize);
    let mut out = String::new();
    for line in -(hist as i32)..0 {
        let mut row: Row = Vec::with_capacity(grid_cols);
        for c in 0..grid_cols {
            let point = Point::new(
                alacritty_terminal::index::Line(line),
                alacritty_terminal::index::Column(c),
            );
            row.push(convert_cell(&grid[point]));
        }
        if let Some(body) = kasa_bridge::screen::row_ansi(&row) {
            out.push_str(&body);
        }
        out.push_str("\r\n");
    }
    out.push_str(&format!("\x1b[{};1H", rows.max(1)));
    for _ in 0..hist.min(rows as usize) {
        out.push('\n');
    }
    out.into_bytes()
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
    // 히스토리로 몇 줄 올라가 있는지 + 스크롤백이 얼마나 깊은지. 화면 행 r 은 그리드
    // 줄 `r - display_offset` 이고, 그 값이 음수면 히스토리다(0 이 화면 첫 줄).
    // `damage()` 의 &mut 대여 전에 읽는다.
    let display_offset = term.grid().display_offset() as i32;
    let topmost = -(term.grid().history_size() as i32);
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
        // ⚠️**음수 line 을 막지 마라.** alacritty 는 스크롤백을 음수 `Line` 으로
        // 노출한다(`topmost_line()` = `-history_size`). 예전엔 `line >= 0` 만
        // 인정해 히스토리를 통째로 빈칸으로 채웠고, 그래서 위로 N줄 올리면 윗줄
        // N개가 비고 화면 높이보다 많이 올리면 화면이 통째로 비었다 —
        // 「위로 올려도 위에 게 없어진다」의 정체였다(2026-08-11 확정).
        let line = r as i32 - display_offset;
        let line_ok = line >= topmost && line < grid_lines as i32;
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
        // 뷰포트 배치는 스냅샷 뒤 attach_inline_views 가 채운다 — snapshot 은
        // Term 만 알고 이미지 기록(InlineImgs)을 모른다.
        inline_images: Vec::new(),
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

/// 셀-흐름 인라인 이미지 모드인가. 기본 on — OSC 1337 이 도착한 그 자리에
/// 그린다. `KASATERM_INLINE_IMAGES=tab` 이면 옛 동작(temp PNG → 이미지 탭)으로
/// 돌아간다 — 셀 렌더가 실사용에서 검증될 때까지 남겨 둔 도피로(2026-08-13).
fn inline_cell_flow() -> bool {
    static MODE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *MODE.get_or_init(|| {
        std::env::var("KASATERM_INLINE_IMAGES").map(|v| v != "tab").unwrap_or(true)
    })
}

/// 셀-흐름 인라인 이미지 한 장의 PTY측 기록.
///
/// 앵커는 **내용 절대 줄**(`history_size + 화면 행`) — 새 출력이 밀어 올리는
/// 일반 스크롤과 높이 리사이즈(줄이 화면↔히스토리를 오가도 이 값은 그대로)에는
/// 안정하고, 가로 리사이즈(리플로우)·히스토리 캡 회전·clear 에서만 무너진다.
/// 그 셋은 `attach_inline_views` 가 전량 폐기한다 — 어긋난 자리에 그리는 것보다
/// 안 그리는 게 낫다.
struct InlineImg {
    id: u64,
    /// 재전송 판별 키 (name, size) — recall 은 그림 자리가 밀리면 같은 시퀀스를
    /// 새 커서 위치에서 다시 흘린다(recall 35b7f5e). 같은 키는 새 레코드가
    /// 아니라 **이동**이다.
    key: (String, u64),
    path: std::path::PathBuf,
    abs_line: i64,
    col: u16,
    cols: u16,
    rows: u16,
    /// 앵커 행 텍스트(앞 64자, trim). recall 은 그림 자리에 대체 텍스트
    /// (`[그림] name`)를 먼저 깔고 시퀀스를 덮으므로, 이 행이 다른 내용으로
    /// 바뀌면 그림이 그 자리를 떠난 것이다(스크롤 아웃·리페인트) — 남겨 두면
    /// 남의 글 위에 유령으로 뜬다. **기록 시점이 아니라 flush 뒤 첫 스냅샷에서
    /// 뜬다**(None=아직) — 기록은 동기 출력(2026) 한가운데라 그리드가 옛
    /// 프레임이고, 거기서 뜨면 다음 프레임에 반드시 어긋나 자기를 지운다.
    row_sig: Option<String>,
}

#[derive(Default)]
struct InlineImgs {
    imgs: Vec<InlineImg>,
    next_id: u64,
    /// 리플로우·clear·alt 전환 감지용 직전 프레임 상태.
    last_cols: u16,
    last_rows: u16,
    last_hist: i64,
    last_alt: bool,
}

/// 리더 스레드의 OSC 1337 캡처 상태(셀-흐름 경로). 페이로드가 read 여러 번에
/// 걸칠 수 있어 buf/capturing 이 프레임을 넘어 살고, anchor 는 마커를 만난
/// 순간의 커서(= 이미지 좌상단)다.
#[derive(Default)]
struct InlineScan {
    buf: Vec<u8>,
    capturing: bool,
    anchor: Option<(i64, u16)>,
}

/// PNG IHDR 의 (width, height). PNG 가 아니면 None — 그때 줄수는 기본값으로.
fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let w = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    Some((w, h))
}

/// 접두 바이트에서 **마지막** 절대 커서 이동(CUP — `ESC[{row};{col}H|f`)의
/// 0-기반 (row, col). 동기 출력(2026) 중엔 Term 커서를 못 믿으므로, 시퀀스를
/// 그 자리에 심으려던 송신측의 CUP 이 앵커의 정본이 된다.
fn last_cup(bytes: &[u8]) -> Option<(u16, u16)> {
    let mut found = None;
    let mut i = 0;
    while let Some(rel) = find_subslice(&bytes[i..], b"\x1b[") {
        let start = i + rel + 2;
        let mut j = start;
        while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b';') {
            j += 1;
        }
        if j < bytes.len() && (bytes[j] == b'H' || bytes[j] == b'f') {
            let params = std::str::from_utf8(&bytes[start..j]).unwrap_or("");
            let mut it = params.split(';');
            let row = it.next().and_then(|s| s.parse::<u16>().ok()).unwrap_or(1);
            let col = it.next().and_then(|s| s.parse::<u16>().ok()).unwrap_or(1);
            found = Some((row.saturating_sub(1), col.saturating_sub(1)));
        }
        i = start;
    }
    found
}

/// 그리드 한 줄의 앞부분 텍스트(trim, 최대 64자) — 인라인 이미지 생존 신호용.
/// `line` 은 화면 좌표(0=화면 첫 줄, 음수=히스토리).
fn grid_line_sig(term: &Term<PtyEventForwarder>, line: i32) -> String {
    let grid = term.grid();
    if line < -(grid.history_size() as i32) || line >= grid.screen_lines() as i32 {
        return String::new();
    }
    let cols = grid.columns().min(64);
    let mut s = String::with_capacity(cols);
    for c in 0..cols {
        let ch = grid[Point::new(alacritty_terminal::index::Line(line), alacritty_terminal::index::Column(c))].c;
        s.push(if ch == '\0' { ' ' } else { ch });
    }
    s.trim().to_string()
}

/// OSC 1337 이 섞인 배치를 파서에 먹이면서 이미지를 **그 자리에서** 뜬다.
///
/// 마커 직전까지 파서를 먼저 돌려야 커서가 이미지 자리에 가 있다 — 배치를
/// 통째로 advance 한 뒤 커서를 읽으면 이미지 뒤에 온 출력(recall 의 절대좌표
/// 리페인트)이 커서를 이미 딴 데로 옮긴 뒤다. 시퀀스 본문은 파서에 안 먹인다:
/// alacritty 는 어차피 버리고, vte OSC 버퍼에 MB 급 base64 를 밀 이유가 없다.
/// 마커가 read 경계에 걸치면 이번 배치는 놓친다 — 기존 scan_inline_image 와
/// 같은 트레이드(게이트 프리픽스 검사도 같은 한계를 이미 안고 있었다).
fn advance_scanning_inline(
    processor: &mut Processor<StdSyncHandler>,
    term: &mut Term<PtyEventForwarder>,
    bytes: &[u8],
    st: &mut InlineScan,
    imgs: &Mutex<InlineImgs>,
) {
    const MARKER: &[u8] = b"\x1b]1337;File=";
    let mut data = bytes;
    loop {
        if st.capturing {
            let bel = data.iter().position(|&b| b == 0x07);
            let stx = find_subslice(data, b"\x1b\\");
            let end = match (bel, stx) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            match end {
                Some(e) => {
                    st.buf.extend_from_slice(&data[..e]);
                    record_inline_image(term, st, imgs);
                    st.buf.clear();
                    st.capturing = false;
                    let term_len = if data.get(e) == Some(&0x07) { 1 } else { 2 };
                    data = &data[(e + term_len).min(data.len())..];
                }
                None => {
                    // 말라 죽은 스트림 무한 성장 방지 — scan_inline_image 와 동일.
                    if st.buf.len() < 8 * 1024 * 1024 {
                        st.buf.extend_from_slice(data);
                    } else {
                        st.buf.clear();
                        st.capturing = false;
                        st.anchor = None;
                    }
                    return;
                }
            }
        } else {
            match find_subslice(data, MARKER) {
                Some(m) => {
                    processor.advance(term, &data[..m]);
                    // 동기 출력(DECSET 2026) 안이면 방금 먹인 바이트가 파서
                    // 버퍼에만 쌓여 커서가 옛 자리(입력줄)에 있다 — recall 은
                    // 프레임 전체를 2026 으로 감싼다(2026-08-13 실측 137쌍).
                    // 그때는 마커 직전의 절대 커서 이동(CUP)에서 앵커를 읽는다.
                    // CUP 도 없으면 앵커 불명 — 페이로드는 그대로 소비하되
                    // 기록은 버린다(엉뚱한 자리에 그리는 것보다 낫다).
                    let hist = term.grid().history_size() as i64;
                    st.anchor = if processor.sync_bytes_count() > 0 {
                        last_cup(&data[..m]).map(|(row, col)| (hist + row as i64, col))
                    } else {
                        let cur = term.grid().cursor.point;
                        Some((hist + cur.line.0.max(0) as i64, cur.column.0 as u16))
                    };
                    st.capturing = true;
                    data = &data[m + MARKER.len()..];
                }
                None => {
                    processor.advance(term, data);
                    return;
                }
            }
        }
    }
}

/// 완성된 OSC 1337 본문(`params:base64`)을 temp 파일로 떨구고 앵커에 건다.
fn record_inline_image(
    term: &Term<PtyEventForwarder>,
    st: &mut InlineScan,
    imgs: &Mutex<InlineImgs>,
) {
    let Some((abs_line, col)) = st.anchor.take() else { return };
    let body: &[u8] = &st.buf;
    let Some(colon) = body.iter().position(|&b| b == b':') else { return };
    let params = String::from_utf8_lossy(&body[..colon]).into_owned();
    let bytes = b64_decode(&body[colon + 1..]);
    if bytes.len() < 16 {
        return;
    }
    let grid_cols = term.grid().columns() as u16;
    // 히스토리 캡에 닿으면 이후 줄들이 회전해 절대 줄 번호가 조용히 밀린다 —
    // 그 상태에선 새 앵커도 못 믿으므로 받지 않는다(기존 것은
    // attach_inline_views 가 이미 전량 폐기했다).
    if term.grid().history_size() >= history_lines_for_cols(grid_cols.max(1)) {
        return;
    }
    let mut name = String::new();
    let mut size: u64 = bytes.len() as u64;
    let mut width_cells: Option<u16> = None;
    for kv in params.split(';') {
        if let Some((k, v)) = kv.split_once('=') {
            match k {
                "name" => {
                    name = String::from_utf8_lossy(&b64_decode(v.as_bytes())).into_owned()
                }
                "size" => size = v.parse().unwrap_or(size),
                // iTerm 스펙은 N(셀)·Npx·N%·auto — 셀 수만 다룬다(recall 이 보내는
                // 형태). 나머지는 기본폭으로 떨어진다.
                "width" => width_cells = v.parse().ok(),
                _ => {}
            }
        }
    }
    let cols = width_cells
        .unwrap_or_else(|| (grid_cols as u32 * 6 / 10).clamp(20, 100) as u16)
        .clamp(1, grid_cols.saturating_sub(col).max(1));
    // 셀은 세로가 가로의 두 배쯤. recall 의 자리 예약 계산(image_rows 의 2.1)과
    // 같은 비율이어야 recall 이 비워 둔 줄 수와 우리가 덮는 줄 수가 맞아떨어진다.
    // 실제 폰트 비율과의 오차는 GUI 의 contain-fit 이 레터박스로 흡수한다.
    const CELL_ASPECT: f64 = 2.1;
    let rows = match png_size(&bytes) {
        Some((w, h)) if w > 0 => {
            (((cols as f64) * h as f64 / w as f64) / CELL_ASPECT).ceil() as u16
        }
        _ => 12,
    }
    .max(1);
    let key = (name, size);
    let mut lock = imgs.lock().unwrap();
    if let Some(existing) = lock.imgs.iter_mut().find(|i| i.key == key) {
        // 같은 그림의 재전송 = 자리 이동. 파일과 id(=GUI 텍스처)는 그대로 산다.
        existing.abs_line = abs_line;
        existing.col = col;
        existing.cols = cols;
        existing.rows = rows;
        existing.row_sig = None;
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
    let id = lock.next_id;
    lock.next_id += 1;
    lock.imgs.push(InlineImg { id, key, path: tmp, abs_line, col, cols, rows, row_sig: None });
    // 한 pane 에 무한정 쌓이지 않게 — 오래된 것부터 파일째 놓는다.
    while lock.imgs.len() > 16 {
        let old = lock.imgs.remove(0);
        let _ = std::fs::remove_file(&old.path);
    }
}

/// 스냅샷에 인라인 이미지의 이번 프레임 뷰포트 배치를 싣는다. 앵커가 무너지는
/// 사건은 여기서 감지해 통째로 버린다.
fn attach_inline_views(
    update: &mut ScreenUpdate,
    term: &Term<PtyEventForwarder>,
    imgs: &Mutex<InlineImgs>,
) {
    let mut lock = imgs.lock().unwrap();
    let grid = term.grid();
    let cols = grid.columns() as u16;
    let rows = grid.screen_lines() as u16;
    let hist = grid.history_size() as i64;
    let alt = term.mode().contains(alacritty_terminal::term::TermMode::ALT_SCREEN);
    let clear_all =
        // 가로 리사이즈 = 리플로우. 절대 줄이 전부 다시 감긴다.
        (lock.last_cols != 0 && cols != lock.last_cols)
        // alt 화면 전환 = 좌표 공간이 통째로 바뀐다. alt 안의 내용이 화면에서
        // 사라지는 것과 같은 운명이라 그림도 함께 사라지는 게 일관적이다.
        || alt != lock.last_alt
        // 높이 변화 없이 히스토리가 줄었다 = clear_history. (높이가 커질 때는
        // 히스토리 줄이 화면으로 복귀하며 줄어드는데, 그건 내용 이동이라
        // 절대 줄 앵커가 산다 — 함께 버리면 안 된다.)
        || (hist < lock.last_hist && rows == lock.last_rows)
        // 캡 도달 = 이후 줄들이 회전해 번호가 조용히 밀린다.
        || hist >= history_lines_for_cols(cols.max(1)) as i64;
    if clear_all && !lock.imgs.is_empty() {
        for im in lock.imgs.drain(..) {
            let _ = std::fs::remove_file(&im.path);
        }
    }
    lock.last_cols = cols;
    lock.last_rows = rows;
    lock.last_hist = hist;
    lock.last_alt = alt;
    if lock.imgs.is_empty() {
        return;
    }
    // 앵커 행이 다른 내용으로 바뀐 그림은 그 자리를 떠났다(recall 이 스크롤로
    // 걷어 갔거나 다른 글이 덮었다) — 유령으로 남기지 않는다. 서명이 아직
    // 없는 그림(기록 직후)은 지금 그리드에서 처음 뜬다 — 동기 출력이 flush 된
    // 첫 스냅샷이 여기라서다.
    lock.imgs.retain_mut(|im| {
        let sig_now = grid_line_sig(term, (im.abs_line - hist) as i32);
        let alive = match &im.row_sig {
            None => {
                im.row_sig = Some(sig_now);
                true
            }
            Some(sig) => *sig == sig_now,
        };
        if !alive {
            let _ = std::fs::remove_file(&im.path);
        }
        alive
    });
    let top_abs = hist - grid.display_offset() as i64;
    update.inline_images = lock
        .imgs
        .iter()
        .filter_map(|im| {
            let row = im.abs_line - top_abs;
            // 뷰포트와 겹치는 것만 — GUI 는 받은 것만 그리고, 안 온 그림의
            // 텍스처는 놓는다(스크롤로 벗어난 그림의 GPU 메모리 회수).
            (row + (im.rows as i64) > 0 && row < rows as i64).then(|| {
                kasa_bridge::screen::InlineImageView {
                    id: im.id,
                    path: im.path.display().to_string(),
                    row: row as i32,
                    col: im.col,
                    cols: im.cols,
                    rows: im.rows,
                }
            })
        })
        .collect();
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

    /// 표를 한 번에 뜨는 길로 바꾸면서 깨지기 쉬운 곳은 파싱이다. `ps` 는 pid 를
    /// 폭에 맞춰 오른쪽 정렬하고 명령줄에는 공백이 얼마든지 들어가므로, 첫 공백
    /// 하나로만 갈라야 인자가 잘리지 않는다.
    #[cfg(unix)]
    #[test]
    fn 명령줄_표는_첫_공백에서만_갈린다() {
        let table = super::scan_cmdlines();
        assert!(!table.is_empty(), "ps 표가 비었다");
        // 자기 자신은 반드시 있고, 인자가 여럿이면 그게 온전히 남아야 한다.
        let me = table.get(&std::process::id()).expect("자기 pid 가 표에 없다");
        assert!(!me.trim().is_empty());
        // 커널 스레드처럼 명령줄이 빈 줄은 아예 안 담긴다 — 담기면 「이름 없는
        // 프로세스」가 학생 판정에 섞인다.
        assert!(table.values().all(|v| !v.trim().is_empty()));
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
/// 표에 실린 하네스 한 줄. `AgentKind::Other` 가 이 정적 표의 원소를 가리킨다 —
/// 종류가 서른을 넘어 enum 변종으로 세면 `match` 가 호출처마다 폭발하는데,
/// 정작 이들에게 필요한 것은 「이름이 무엇이고 프로세스가 무엇인가」뿐이다.
/// claude·codex·agy 만 변종으로 남긴 이유는 그 셋에만 고유 분기가 실재하기
/// 때문이다(claude=SendMessage·transcript, codex=입력박스 판독, agy=배너).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AgentSpec {
    /// 저장·전송용 id. Orca 의 `TuiAgent` 키와 같은 문자열을 쓴다.
    pub id: &'static str,
    /// 사람에게 보이는 이름(헤더 이름표·info).
    pub label: &'static str,
    /// 이 하네스로 인정할 프로세스 이름들. 첫 원소가 대표.
    pub procs: &'static [&'static str],
    /// comm 이 `node`·`Python` 같은 런처로 **숨을 때** 명령줄에서 찾을 조각.
    ///
    /// 이게 왜 필요한지는 실측이 말해 준다(2026-08-21, 이 컴퓨터에서 직접 띄움):
    /// gemini 는 셸의 자식도 손자도 comm 이 `node` 고, hermes 는 `Python` 이며,
    /// cursor-agent 는 `…/cursor-agent/versions/<판>/node` 라 파일명만 떼면 역시
    /// `node` 다. 이름만 보는 판정으로는 **셋 다 영영 안 잡힌다** — 표를 옮겨
    /// 놓고도 학생이 안 서는 조용한 실패라 알아채기 어렵다.
    pub argv_hints: &'static [&'static str],
}

/// 하네스 표. 출처는 Orca(`src/shared/tui-agent-config.ts` +
/// `tui-agent-display-names.ts`)이고 2026-08-21 에 기계적으로 옮겼다 — 손으로
/// 늘리면 이름 하나가 어긋나 조용히 안 잡히므로, 갱신할 때도 그 두 파일에서
/// 다시 뽑아라.
///
/// 셋은 일부러 뺐다: `claude`·`codex`·`antigravity` 는 아래 enum 변종이고,
/// `claude-agent-teams` 는 Orca 전용 런치 모드라 우리 쪽에 대응물이 없으며,
/// `kimi` 는 이 컴퓨터에서 **거노의 자작 런처 이름**이다(claude 를 다른 모델로
/// 띄우는 zsh 스크립트) — 문샷의 Kimi CLI 와 이름이 같아 넣으면 오판한다.
pub static AGENT_TABLE: &[AgentSpec] = &[
    AgentSpec { id: "aider", label: "Aider", procs: &["aider"], argv_hints: &[] },
    AgentSpec { id: "amp", label: "Amp", procs: &["amp"], argv_hints: &["@sourcegraph/amp/"] },
    AgentSpec { id: "ante", label: "Ante", procs: &["ante"], argv_hints: &[] },
    AgentSpec { id: "aug", label: "Auggie", procs: &["auggie"], argv_hints: &[] },
    AgentSpec { id: "autohand", label: "Autohand Code", procs: &["autohand"], argv_hints: &[] },
    AgentSpec { id: "cline", label: "Cline", procs: &["cline"], argv_hints: &[] },
    AgentSpec { id: "codebuff", label: "Codebuff", procs: &["codebuff"], argv_hints: &[] },
    AgentSpec { id: "command-code", label: "Command Code", procs: &["command-code"], argv_hints: &[] },
    AgentSpec { id: "continue", label: "Continue", procs: &["cn"], argv_hints: &[] },
    AgentSpec { id: "copilot", label: "GitHub Copilot", procs: &["copilot"], argv_hints: &[] },
    AgentSpec { id: "crush", label: "Charm", procs: &["crush"], argv_hints: &[] },
    AgentSpec { id: "cursor", label: "Cursor", procs: &["cursor-agent"], argv_hints: &["cursor-agent/versions/"] },
    AgentSpec { id: "devin", label: "Devin", procs: &["devin"], argv_hints: &[] },
    AgentSpec { id: "droid", label: "Droid", procs: &["droid"], argv_hints: &[] },
    // 힌트가 둘인 이유: 설치 방식마다 명령줄이 다르다. npm 전역이면
    // `node …/npm-global/bin/gemini`(이 컴퓨터 실측)이고, 패키지를 직접 가리키면
    // Orca 가 쓰는 `node_modules/@google/gemini-cli/…` 가 된다.
    AgentSpec { id: "gemini", label: "Gemini", procs: &["gemini"], argv_hints: &["node_modules/@google/gemini-cli/", "/bin/gemini"] },
    AgentSpec { id: "goose", label: "Goose", procs: &["goose"], argv_hints: &[] },
    AgentSpec { id: "grok", label: "Grok", procs: &["grok"], argv_hints: &[] },
    AgentSpec { id: "hermes", label: "Hermes", procs: &["hermes"], argv_hints: &[".hermes/hermes-agent/"] },
    AgentSpec { id: "kilo", label: "Kilocode", procs: &["kilo"], argv_hints: &[] },
    AgentSpec { id: "kiro", label: "Kiro", procs: &["kiro-cli"], argv_hints: &[] },
    AgentSpec { id: "mimo-code", label: "MiMo Code", procs: &["mimo"], argv_hints: &[] },
    AgentSpec { id: "mistral-vibe", label: "Mistral Vibe", procs: &["vibe", "mistral-vibe"], argv_hints: &[] },
    AgentSpec { id: "omp", label: "OMP", procs: &["omp"], argv_hints: &[] },
    AgentSpec { id: "openclaude", label: "OpenClaude", procs: &["openclaude"], argv_hints: &[] },
    AgentSpec { id: "openclaw", label: "OpenClaw", procs: &["openclaw"], argv_hints: &[] },
    AgentSpec { id: "opencode", label: "OpenCode", procs: &["opencode"], argv_hints: &[] },
    AgentSpec { id: "pi", label: "Pi", procs: &["pi"], argv_hints: &["pi-coding-agent/dist/cli.js"] },
    AgentSpec { id: "qwen-code", label: "Qwen Code", procs: &["qwen"], argv_hints: &[] },
    AgentSpec { id: "rovo", label: "Rovo Dev", procs: &["rovo"], argv_hints: &[] },
    AgentSpec { id: "trae", label: "Trae", procs: &["traecli"], argv_hints: &[] },
];

/// pane 에서 도는 에이전트 종류. 학생 대접(보더 학생색·타이틀바·얼굴·탭칩)은
/// claude 전용이 아니라 **이 값이 Some 이면** 붙는다(거노 2026-08-05: codex 도 학생).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AgentKind {
    Claude,
    Codex,
    Agy,
    /// 표의 한 줄. 얼굴·이름·색만 받는 하네스들이 전부 여기로 온다.
    Other(&'static AgentSpec),
}

impl AgentKind {
    /// 저장·전송용 이름. `pane_record` 의 `was_agent`, board 의 `harness`, 소켓
    /// 응답이 전부 이 하나를 쓴다 — match 를 사본으로 늘리면 한쪽만 고쳐져 갈린다.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Agy => "agy",
            Self::Other(spec) => spec.id,
        }
    }

    /// 사람에게 보이는 이름. 헤더 이름표·info 가 이걸 그린다 — `as_str` 은
    /// 저장·전송용이라 소문자 id 고, 이쪽은 표기용이라 갈라 둔다.
    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Codex => "Codex",
            Self::Agy => "Antigravity",
            Self::Other(spec) => spec.label,
        }
    }

    /// `as_str` 의 역함수. 저장된 `was_agent`·소켓의 `harness` 를 되읽는 자리가
    /// 각자 하드코딩 match 를 갖고 있었는데, 종류가 서른이 되면 그 사본들이
    /// 곧바로 갈린다.
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "agy" => Some(Self::Agy),
            other => AGENT_TABLE.iter().find(|s| s.id == other).map(Self::Other),
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
            // shim 래퍼도 `agy` 라는 이름의 sh 스크립트지만 마지막에 `exec` 로
            // 진짜 바이너리가 그 자리를 차지하므로, 여기 걸리는 건 늘 진짜다.
            "agy" => Some(Self::Agy),
            other => AGENT_TABLE
                .iter()
                .find(|spec| spec.procs.contains(&other))
                .map(Self::Other),
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
    agent_pid_for_shell(table, shell_pid).map(|(kind, _)| kind)
}

/// `agent_for_shell` 의 pid 동반판. 계정 실측(그 프로세스의 env 를 `ps` 로 읽어
/// 어느 자격증명 저장소로 떠 있는지 보는 것)이 이 pid 를 집는다 — 종류만 알아서는
/// "어느 계정인가"에 답할 수 없다.
pub fn agent_pid_for_shell(
    table: &[(u32, u32, String)],
    shell_pid: u32,
) -> Option<(AgentKind, u32)> {
    let eff = effective_shell_pid(table, shell_pid);
    if let Some(hit) = agent_pid_in_table(table, eff) {
        return Some(hit);
    }
    agent_pid_by_argv(table, eff)
}

/// 이름으로 못 잡은 pane 을 **명령줄로** 한 번 더 본다 — comm 이 `node`·`Python`
/// 인 하네스들(gemini·cursor·hermes·amp)이 여기서만 잡힌다.
///
/// ⚠️ 순서가 곧 비용이다. `process_cmdline` 은 표를 500ms 캐시하지만 그 표를
/// 뜨는 일 자체가 `ps` 한 번이라, 이름 판정보다 **먼저** 놓으면 학생이 아닌 셸
/// pane 까지 그 문을 연다. 그래서 ①이름 판정이 실패하고 ②그 자식이 실제로
/// 런처류일 때만 여기까지 온다. 결과는 아래 캐시가 1초 잡아 둔다.
fn agent_pid_by_argv(
    table: &[(u32, u32, String)],
    shell_pid: u32,
) -> Option<(AgentKind, u32)> {
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
    // 런처가 아니면 이름이 이미 진실을 말한 것이다 — 그걸로 못 잡았으면
    // 하네스가 아니라는 뜻이므로 ps 를 부를 이유가 없다.
    if !is_argv_probe_launcher(child) {
        return None;
    }
    // 자식과 손자 둘 다 본다. gemini 는 node 아래 node 로 한 겹 더 들어간다.
    let grandchild = newest_child(child_pid).map(|(p, _)| p);
    for pid in [Some(child_pid), grandchild].into_iter().flatten() {
        if let Some(spec) = agent_spec_by_argv_cached(pid) {
            return Some((AgentKind::Other(spec), pid));
        }
    }
    None
}

/// argv 를 들여다볼 가치가 있는 런처 — 이름 판정용 `is_agent_launcher` 보다 넓다.
///
/// python 을 **여기에만** 넣는 이유: 이름으로 한 세대 내려가는 판정에 python 을
/// 넣으면 `python train.py` 처럼 자기가 곧 작업인 흔한 경우에 엉뚱한 자식 이름을
/// 집는다. 반면 argv 폴백은 표의 힌트 문자열과 정확히 맞을 때만 인정하므로 그
/// 위험이 없다. hermes 가 실측에서 comm=`Python`(macOS 프레임워크 번들이라 대문자)
/// 이었고, 이 문이 닫혀 있어 표가 맞는데도 안 잡혔다(2026-08-21).
fn is_argv_probe_launcher(comm: &str) -> bool {
    if is_agent_launcher(comm) {
        return true;
    }
    let base = comm.rsplit(['/', '\\']).next().unwrap_or(comm);
    let base = strip_exe_suffix(base.to_string()).to_ascii_lowercase();
    base == "python" || base.strip_prefix("python").is_some_and(|v| {
        !v.is_empty() && v.chars().all(|c| c.is_ascii_digit() || c == '.')
    })
}

/// pid → argv 판정 결과, 1초 캐시. 아래 `process_cmdline` 이 표를 500ms 쥐므로
/// 프로세스가 매번 뜨지는 않지만, 판정(문자열 훑기)까지 매 프레임 되풀이할
/// 이유는 없다.
///
/// pid 는 재사용되지만 TTL 이 1초라 남의 결과를 물려받을 창이 사실상 없다 —
/// 그 사이에 pid 가 한 바퀴 돌려면 초당 수만 개가 떠야 한다.
fn agent_spec_by_argv_cached(pid: u32) -> Option<&'static AgentSpec> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<u32, (Instant, Option<&'static AgentSpec>)>>> =
        OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let now = Instant::now();
    if let Ok(mut map) = cache.lock() {
        if let Some((at, val)) = map.get(&pid) {
            if now.duration_since(*at).as_millis() < 1000 {
                return *val;
            }
        }
        let args = process_cmdline(pid).unwrap_or_default();
        let hit = (!args.is_empty())
            .then(|| {
                AGENT_TABLE
                    .iter()
                    .find(|spec| spec.argv_hints.iter().any(|h| args.contains(h)))
            })
            .flatten();
        // 실패도 캐시한다 — 셸 pane 은 늘 실패하는데 그때마다 ps 를 부르면
        // 캐시를 둔 의미가 없다.
        map.retain(|_, (at, _)| now.duration_since(*at).as_secs() < 5);
        map.insert(pid, (now, hit));
        return hit;
    }
    None
}

/// 판정 본체의 종류-만 어댑터 — 이제 prod 는 pid 동반판을 쓰고, 트리 판정
/// 테스트들이 이 얇은 이름으로 남아 있다.
#[cfg(test)]
fn agent_in_table(table: &[(u32, u32, String)], shell_pid: u32) -> Option<AgentKind> {
    agent_pid_in_table(table, shell_pid).map(|(kind, _)| kind)
}

fn agent_pid_in_table(
    table: &[(u32, u32, String)],
    shell_pid: u32,
) -> Option<(AgentKind, u32)> {
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
        return Some((kind, child_pid));
    }
    if is_agent_launcher(child) {
        if let Some((grandchild_pid, grandchild)) = newest_child(child_pid) {
            return AgentKind::from_comm(grandchild).map(|k| (k, grandchild_pid));
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

/// 자기 이름으로는 아무것도 말해 주지 않는, 남을 띄우기만 하는 것들.
/// 진짜 프로그램은 이들의 자식으로 뜨므로 이름 해석은 여기서 멈추면 안 된다.
/// python 류는 넣지 않았다 — `python train.py` 처럼 자기가 곧 작업인 경우가
/// 흔해서, 내려갔다가 엉뚱한 자식 이름을 집을 수 있다.
fn is_launcher_name(name: &str) -> bool {
    matches!(
        strip_exe_suffix(name.to_string()).as_str(),
        "node" | "npx" | "npm" | "bun" | "deno"
    )
}

/// 런처를 만나면 그 아래 최신 자식으로 계속 내려간다.
///
/// 여기서 멈추면 pane 을 닫을 때 "node 실행 중"이라고 물어 무엇을 닫는 건지 알
/// 수가 없다 — codex 는 npm shim 을 거쳐 진짜 바이너리가 손자로 뜨고, agy 를
/// 게이트웨이 모델(kimi·glm)로 돌릴 때도 free-antigravity-cli(node)를 지난다.
/// 사슬이 길어질 수 있으니 몇 걸음만 내려가고, 더 못 내려가면 그 자리를 답으로 쓴다.
fn descend_launchers(
    table: &[(u32, u32, String)],
    start: Option<(u32, String)>,
) -> Option<(u32, String)> {
    let mut cur = start;
    for _ in 0..3 {
        let (cpid, cname) = cur.clone()?;
        if !is_launcher_name(&cname) {
            break;
        }
        let mut grandchild: Option<(u32, String)> = None;
        for (row_pid, row_ppid, name) in table.iter() {
            if *row_ppid == cpid && grandchild.as_ref().is_none_or(|(p, _)| *p < *row_pid) {
                grandchild = Some((*row_pid, name.clone()));
            }
        }
        match grandchild {
            Some(g) => cur = Some(g),
            None => break,
        }
    }
    cur
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
struct CachedTable {
    at: Instant,
    table: ProcessTable,
    refreshing: bool,
}

fn table_cache() -> &'static std::sync::Mutex<CachedTable> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<CachedTable>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| {
        std::sync::Mutex::new(CachedTable {
            at: Instant::now() - std::time::Duration::from_secs(1),
            table: Default::default(),
            refreshing: false,
        })
    })
}

/// Enter 직후 프로세스 테이블을 앞당겨 읽는다 — 300ms TTL + 백그라운드 갱신
/// 구조에서는 방금 exec 된 claude 가 테이블에 실리기까지 최악 ~600ms 가 비고,
/// 그동안 배너·헤더가 학생 테마 없이 그려졌다(거노 2026-08-20 「처음 클로드코드
/// 켜면 캐릭터 학생테마 적용안돼」). Enter 는 「새 전경 프로세스가 곧 뜬다」의
/// 가장 이른 신호지만 exec 사슬이 끝나기 전에 읽으면 헛스캔이 `at` 시계만
/// 되돌려 오히려 다음 정기 갱신을 늦춘다 — 그래서 사슬이 끝났을 100ms 뒤와,
/// 느린 런처(래퍼 스크립트→node) 대비 400ms 뒤 두 번 읽는다.
fn process_table_poke() {
    std::thread::spawn(|| {
        let mut slept = 0u64;
        for at in [100u64, 400] {
            std::thread::sleep(std::time::Duration::from_millis(at - slept));
            slept = at;
            {
                let Ok(mut g) = table_cache().lock() else { return };
                if g.refreshing {
                    continue;
                }
                g.refreshing = true;
            }
            let fresh = process_table_raw();
            if let Ok(mut g) = table_cache().lock() {
                if !fresh.is_empty() {
                    g.at = Instant::now();
                    g.table = std::sync::Arc::new(fresh);
                }
                g.refreshing = false;
            }
        }
    });
}

pub fn process_table_shared() -> ProcessTable {
    let cache = table_cache();
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

/// 표를 한 번에 뜬다 — pid 하나를 물어도 `ps` 는 어차피 전 프로세스를 훑으므로,
/// 하나씩 묻는 것은 같은 일을 pane 수만큼 되풀이하는 것이다.
#[cfg(unix)]
fn scan_cmdlines() -> std::collections::HashMap<u32, String> {
    let Ok(out) = std::process::Command::new("ps").args(["-axo", "pid=,args="]).output() else {
        return std::collections::HashMap::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            // pid 는 폭에 맞춰 오른쪽 정렬돼 앞에 공백이 붙는다.
            let (pid, args) = line.trim_start().split_once(' ')?;
            let pid: u32 = pid.parse().ok()?;
            let args = args.trim();
            (!args.is_empty()).then(|| (pid, args.to_string()))
        })
        .collect()
}

/// pid → 명령줄. **표 한 벌을 500ms 캐시**한다.
///
/// 종전에는 부를 때마다 `ps -p <pid>` 를 띄웠다. 이 함수는 pane 판정·board·정보
/// 패널이 pane 마다 부르는 자리라, 창이 열 개면 프로세스를 열 개 낳는 일이 화면
/// 갱신 박자로 되풀이됐다 — 2026-08-29 실측(`sample`)에서 유휴 중인 앱의 CPU
/// 표본 상위에 `__posix_spawn` 이 올라왔고, 그 스택이 board 조회였다.
///
/// 캐시가 아니라 **한 번에 다 받는 것**이 핵심이다. `ps -p` 도 커널의 프로세스
/// 표를 통째로 훑으므로 하나를 묻는 값과 전부를 묻는 값이 거의 같다.
///
/// TTL 이 500ms 인 것은 argv 가 `exec` 로 바뀌기 때문이다 — 셸 pane 에서 명령을
/// 치면 그 자리에서 달라지므로 영구 캐시는 못 쓴다. 이 값은 같은 파일의
/// `proc_cache` 와 맞춘 것이다.
#[cfg(unix)]
pub fn process_cmdline(pid: u32) -> Option<String> {
    use std::collections::HashMap;
    use std::sync::Mutex;
    static CACHE: Mutex<Option<(Instant, HashMap<u32, String>)>> = Mutex::new(None);
    // 락이 깨져도 답은 내야 한다 — 여기서 None 을 돌리면 학생 판정이 통째로 죽는다.
    let mut g = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if !g.as_ref().is_some_and(|(t, _)| t.elapsed().as_millis() < 500) {
        *g = Some((Instant::now(), scan_cmdlines()));
    }
    g.as_ref()?.1.get(&pid).cloned()
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
            shell: Some(test_posix_shell()),
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

#[cfg(test)]
mod 하네스_표_tests {
    use super::*;

    fn row(pid: u32, ppid: u32, name: &str) -> (u32, u32, String) {
        (pid, ppid, name.to_string())
    }

    /// 표에 실린 하네스는 이름만으로 잡혀야 한다 — opencode 는 실측에서 comm 이
    /// `/Users/…/.opencode/bin/opencode` 라 경로가 붙어 온다(2026-08-21).
    #[test]
    fn 표에_실린_하네스는_이름으로_잡힌다() {
        let t = vec![
            row(100, 1, "zsh"),
            row(200, 100, "/Users/kasa/.opencode/bin/opencode"),
        ];
        let got = agent_in_table(&t, 100).expect("opencode 를 못 잡았다");
        assert_eq!(got.as_str(), "opencode");
        assert_eq!(got.label(), "OpenCode");
    }

    /// ⚠️ 표의 프로세스 이름이 enum 변종 셋과 겹치면, 겹친 쪽이 `Other` 로
    /// 잡혀 claude 전용 분기(SendMessage·transcript·입력 판독)가 통째로 빠진다.
    /// match 가 먼저라 실제로는 변종이 이기지만, 표에 그 이름이 있다는 것
    /// 자체가 "표를 고치면 하네스가 바뀐다"는 함정이므로 아예 금지한다.
    #[test]
    fn 표가_내장_변종을_가리지_않는다() {
        for spec in AGENT_TABLE {
            for p in spec.procs {
                assert!(
                    !matches!(*p, "claude" | "codex" | "agy"),
                    "{}: 프로세스 이름 {p:?} 가 내장 변종과 겹친다",
                    spec.id
                );
            }
            assert!(
                !matches!(spec.id, "claude" | "codex" | "agy"),
                "{}: id 가 내장 변종과 겹친다",
                spec.id
            );
        }
    }

    /// 같은 프로세스 이름이 두 줄에 있으면 먼저 쓰인 쪽이 늘 이겨, 뒤엣것은
    /// 영영 안 잡히면서도 컴파일은 통과한다.
    #[test]
    fn 표에_같은_프로세스_이름이_두_번_없다() {
        let mut seen = std::collections::HashMap::new();
        for spec in AGENT_TABLE {
            for p in spec.procs {
                if let Some(prev) = seen.insert(*p, spec.id) {
                    panic!("프로세스 이름 {p:?} 가 {prev} 와 {} 에 겹친다", spec.id);
                }
            }
        }
    }

    /// `as_str` → `from_id` 왕복. 저장된 `was_agent` 를 되읽는 경로가 이걸 탄다 —
    /// 깨지면 재시작 때 그 pane 이 셸로 되살아난다.
    #[test]
    fn id_는_왕복한다() {
        for spec in AGENT_TABLE {
            let k = AgentKind::from_id(spec.id).expect("표에 있는 id 를 못 되읽었다");
            assert_eq!(k.as_str(), spec.id);
            assert_eq!(k, AgentKind::Other(spec));
        }
        for k in [AgentKind::Claude, AgentKind::Codex, AgentKind::Agy] {
            assert_eq!(AgentKind::from_id(k.as_str()), Some(k));
        }
        assert_eq!(AgentKind::from_id("없는하네스"), None);
    }

    /// comm 이 런처가 아니면 argv 를 볼 이유가 없다 — 그 문을 열어 두면 셸
    /// pane 마다 명령줄 표를 들추게 된다.
    #[test]
    fn 런처가_아니면_명령줄을_안_읽는다() {
        let t = vec![row(100, 1, "zsh"), row(200, 100, "vim")];
        assert_eq!(agent_pid_by_argv(&t, 100), None);
    }

    /// argv 힌트를 단 하네스는 이름 판정으로는 못 잡히는 것들이다(실측). 힌트가
    /// 빈 채로 남으면 그 줄은 표에 있어도 영영 안 선다.
    #[test]
    fn 이름에_숨는_하네스는_명령줄_힌트를_갖는다() {
        for id in ["gemini", "cursor", "hermes", "amp"] {
            let spec = AGENT_TABLE.iter().find(|s| s.id == id).expect("표에 없다");
            assert!(!spec.argv_hints.is_empty(), "{id}: argv 힌트가 비었다");
        }
    }
}

#[cfg(test)]
mod launcher_descend_tests {
    use super::descend_launchers;

    fn row(pid: u32, ppid: u32, name: &str) -> (u32, u32, String) {
        (pid, ppid, name.to_string())
    }

    #[test]
    fn stops_at_a_real_program() {
        let t = vec![row(100, 1, "zsh"), row(200, 100, "vim")];
        let got = descend_launchers(&t, Some((200, "vim".into())));
        assert_eq!(got.unwrap().1, "vim");
    }

    #[test]
    fn descends_past_node_to_the_real_binary() {
        // 실측 트리: 셸 → node(free-antigravity-cli) → agy
        let t = vec![
            row(100, 1, "zsh"),
            row(200, 100, "node"),
            row(300, 200, "agy"),
        ];
        let got = descend_launchers(&t, Some((200, "node".into())));
        assert_eq!(got.unwrap().1, "agy", "node 에서 멈추면 pane 닫기가 'node' 라고 묻는다");
    }

    #[test]
    fn descends_two_hops_for_npm_shim() {
        // codex 처럼 npm shim 을 한 번 더 지나는 경우
        let t = vec![
            row(100, 1, "zsh"),
            row(200, 100, "npm"),
            row(300, 200, "node"),
            row(400, 300, "codex"),
        ];
        let got = descend_launchers(&t, Some((200, "npm".into())));
        assert_eq!(got.unwrap().1, "codex");
    }

    #[test]
    fn keeps_launcher_when_it_has_no_child() {
        // `node` 를 맨손으로 띄운 REPL — 내려갈 곳이 없으면 그대로 둔다
        let t = vec![row(100, 1, "zsh"), row(200, 100, "node")];
        let got = descend_launchers(&t, Some((200, "node".into())));
        assert_eq!(got.unwrap().1, "node");
    }

    #[test]
    fn picks_the_newest_child() {
        let t = vec![
            row(100, 1, "zsh"),
            row(200, 100, "node"),
            row(300, 200, "old"),
            row(400, 200, "new"),
        ];
        let got = descend_launchers(&t, Some((200, "node".into())));
        assert_eq!(got.unwrap().1, "new");
    }
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
fn test_posix_shell() -> String {
    if cfg!(unix) {
        return "/bin/sh".to_string();
    }
    let mut cands = vec![
        std::path::PathBuf::from(r"C:\Program Files\Git\usr\bin\sh.exe"),
        std::path::PathBuf::from(r"C:\Program Files\Git\bin\sh.exe"),
    ];
    // 설치 자리가 달라도 따라가게: `<git>\cmd\git.exe` → `<git>\usr\bin\sh.exe`.
    if let Some(root) = std::process::Command::new("where")
        .arg("git")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout).lines().next().map(std::path::PathBuf::from)
        })
        .and_then(|p| Some(p.parent()?.parent()?.to_path_buf()))
    {
        cands.push(root.join(r"usr\bin\sh.exe"));
        cands.push(root.join(r"bin\sh.exe"));
    }
    for p in &cands {
        if p.exists() {
            return p.to_string_lossy().into_owned();
        }
    }
    panic!("POSIX 셸을 못 찾았다 — 이 테스트는 Git for Windows 의 sh.exe 가 필요하다: {cands:?}");
}

/// 살아 있는 PTY 로 스냅샷 재생을 검증한다. 순수 변환(`to_ansi`) 쪽 테스트는
/// kasa-bridge 에 있고, 여기서는 실제 셀 그리드에서 제대로 떠지는지와
/// **구독-스냅샷 원자성**을 본다.
#[cfg(test)]
mod snapshot_tap_tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn sh(pane_id: &str) -> PtySession {
        PtySession::start(PtyOptions {
            shell: Some(test_posix_shell()),
            cols: 40,
            rows: 10,
            pane_id: pane_id.into(),
            ..Default::default()
        })
        .expect("PTY 를 못 띄웠다")
    }

    fn wait_on_screen(sess: &PtySession, needle: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let (_rx, ansi) = sess.tap_bytes_with_snapshot();
            if String::from_utf8_lossy(&ansi).contains(needle) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("{needle} 이 10초 안에 화면에 안 나타났다");
    }

    #[test]
    fn snapshot_carries_what_is_already_on_screen() {
        let sess = sh("test-snap");
        sess.send_bytes(b"printf 'HELLO-SNAP\\n'\n").unwrap();
        wait_on_screen(&sess, "HELLO-SNAP");
    }

    /// 화면 밖으로 밀려난 줄(스크롤백)도 접속 스냅샷에 실려야 한다 — 폰 미러가
    /// 스와이프로 올라갈 재료다. 뷰포트만 보내던 시절엔 이 테스트가 실패한다.
    #[test]
    fn tap_snapshot_carries_scrollback_history() {
        let sess = sh("test-hist");
        sess.send_bytes(
            b"i=1; while [ $i -le 30 ]; do echo HIST-$i; i=$((i+1)); done\n",
        )
        .unwrap();
        wait_on_screen(&sess, "HIST-30");
        let (_rx, ansi) = sess.tap_bytes_with_snapshot();
        let s = String::from_utf8_lossy(&ansi);
        // 행 직렬화는 항상 `\x1b[0m` 로 닫히므로 HIST-1 뒤에 이스케이프가 오는
        // 꼴만 정확히 HIST-1 행이다(HIST-10~ 과 구분).
        assert!(
            s.contains("HIST-1\x1b"),
            "10행 화면에서 30줄을 찍었으면 HIST-1 은 스크롤백으로 와야 한다"
        );
    }

    /// 붙는 순간 이미 화면에 있던 출력은 스냅샷으로만, 그 뒤의 출력은 tap 으로만
    /// 와야 한다. 하나라도 양쪽에 걸치면 그만큼 두 번 그려진다.
    ///
    /// unix 전용인 이유는 이 크레이트의 다른 테스트들과 다르다 — 셸이 없어서가
    /// 아니라 **재는 성질 자체가 유닉스 PTY 모델의 것**이라서다. 여기서 「겹쳤다」의
    /// 근거는 출력이 append-only 라는 전제인데, ConPTY 는 화면 갱신을 통째 재렌더로
    /// 보낼 수 있어 이미 그려진 줄이 스트림에 다시 실린다. 그러면 두 번 그리는 버그가
    /// 없어도 이 단언이 깨진다. Windows 에서 억지로 맞추면 재려던 성질이 바뀐다.
    /// (2026-08-31: 로컬 6회는 통과하고 CI 러너에서만 깨져 타이밍 의존이 드러났다.)
    #[cfg(unix)]
    #[test]
    fn subscription_and_snapshot_do_not_overlap() {
        let sess = sh("test-atomic");
        sess.send_bytes(b"printf 'BEFORE-TAP\\n'\n").unwrap();
        wait_on_screen(&sess, "BEFORE-TAP");

        let (rx, ansi) = sess.tap_bytes_with_snapshot();
        assert!(String::from_utf8_lossy(&ansi).contains("BEFORE-TAP"));

        sess.send_bytes(b"printf 'AFTER-TAP\\n'\n").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut streamed = String::new();
        while Instant::now() < deadline && !streamed.contains("AFTER-TAP") {
            if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(200)) {
                streamed.push_str(&String::from_utf8_lossy(&chunk));
            }
        }
        assert!(
            streamed.contains("AFTER-TAP"),
            "구독 뒤의 출력이 tap 으로 안 왔다"
        );
        assert!(
            !streamed.contains("BEFORE-TAP"),
            "스냅샷에 이미 담긴 출력이 tap 으로 또 왔다 — 두 번 그려진다: {streamed:?}"
        );
    }

    fn nums(s: &str) -> Vec<u32> {
        let b = s.as_bytes();
        let (mut out, mut i) = (Vec::new(), 0);
        while i < b.len() {
            if b[i] == b'L' {
                let start = i + 1;
                let mut j = start;
                while j < b.len() && b[j].is_ascii_digit() {
                    j += 1;
                }
                if j > start {
                    if let Ok(n) = s[start..j].parse::<u32>() {
                        out.push(n);
                    }
                    i = j;
                    continue;
                }
            }
            i += 1;
        }
        out
    }

    /// 조용한 pane 에서만 맞는 건 원자성이 아니다. 출력이 쏟아지는 한가운데서
    /// 구독해도 한 줄도 빠지거나 겹치지 않아야 한다.
    ///
    /// 경쟁 창은 마이크로초라 한 번 붙어서는 절대 안 걸린다 — 폭주 내내 반복해서
    /// 붙어야 한다(옛 락 순서에서 이 테스트가 실패하는 것으로 유효성을 확인했다).
    ///
    /// **CI 관문에서는 뺀다**(`cargo test -- --ignored` 로 손수 돌린다). 재는 방식이
    /// 그대로 약점이라서다 — 마이크로초 창을 노리는데 CPU 를 남과 나눠 쓰는 러너
    /// 에서는 창이 제멋대로 늘어난다. 2026-08-31 macOS 러너에서 두 판 연속 깨졌고,
    /// **깨진 단언이 매번 달랐다**(한 번은 유실 쪽 `lo <= drawn + 2`, 한 번은 중복 쪽
    /// `lo >= drawn`). 한 커밋을 두고 정반대 진단이 나온다는 건 그 실패가 코드에
    /// 대한 신호가 아니라는 뜻이다. 그런 걸 관문에 두면 죄 없는 PR 이 가끔 빨개지고,
    /// 곧 아무도 CI 를 안 보게 된다 — 관문을 세운 값이 통째로 날아간다.
    ///
    /// 지우지는 않는다. 락 순서를 만질 때 이 테스트가 실제로 회귀를 잡았고, 한가한
    /// 기계에서는 여전히 정직하게 돈다.
    #[ignore = "경쟁 창이 마이크로초라 부하 있는 CI 에서 양방향으로 흔들린다 — 손수 돌릴 것"]
    #[test]
    fn no_gap_or_overlap_while_output_streams() {
        const TOTAL: u32 = 40_000;
        let sess = sh("test-race");
        sess.send_bytes(
            format!(
                "i=0; while [ $i -lt {TOTAL} ]; do i=$((i+1)); echo L$i; \
                 for j in 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5; do :; done; done\n"
            )
            .as_bytes(),
        )
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(25);
        let mut checked = 0u32;
        while Instant::now() < deadline && checked < 300 {
            let (rx, ansi) = sess.tap_bytes_with_snapshot();
            let Some(&drawn) = nums(&String::from_utf8_lossy(&ansi)).last() else {
                continue;
            };

            let mut streamed = String::new();
            let until = Instant::now() + Duration::from_millis(60);
            while Instant::now() < until {
                match rx.recv_timeout(Duration::from_millis(30)) {
                    Ok(c) => streamed.push_str(&String::from_utf8_lossy(&c)),
                    Err(_) => break,
                }
            }
            drop(rx);

            let got = nums(&streamed);
            let (Some(&lo), Some(&hi)) = (got.iter().min(), got.iter().max()) else {
                continue;
            };
            if hi <= drawn {
                continue; // 폭주가 멎었다 — 이번 회차는 경쟁이 아니다
            }
            assert!(
                lo >= drawn,
                "L{lo} 이 스냅샷(≤L{drawn})과 tap 양쪽에 있다 — 두 번 그려진다 (시도 {checked})"
            );
            assert!(
                lo <= drawn + 2,
                "L{}~L{} 가 스냅샷에도 tap 에도 없다 — 유실 (시도 {checked})",
                drawn + 1,
                lo - 1
            );
            checked += 1;
        }
        assert!(
            checked >= 12,
            "경쟁 상태를 충분히 못 만들었다 ({checked}회) — 검증이 무의미하다"
        );
    }
}

/// 셀-흐름 인라인 이미지(OSC 1337) — 실제 PTY 로 전 경로(스캔→앵커→기록→뷰)를
/// 돈다. recall 이 kasaterm 에서 쓰는 형태(`width=<셀수>`, base64 PNG)를 그대로
/// 흘린다.
#[cfg(test)]
mod inline_image_tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// 1x1 빨강 PNG. png_size 가 (1,1) 을 읽어 rows 계산까지 실경로를 탄다.
    const PNG_1X1: &str =
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

    fn sh(pane_id: &str) -> PtySession {
        PtySession::start(PtyOptions {
            shell: Some(test_posix_shell()),
            cols: 40,
            rows: 10,
            pane_id: pane_id.into(),
            ..Default::default()
        })
        .expect("PTY 를 못 띄웠다")
    }

    fn emit(sess: &PtySession, name_b64: &str) {
        // printf 한 방이 recall 의 show_image 시퀀스와 같은 꼴이다. 뒤의 \n 은
        // 커서를 이미지 아래로 내린다(recall 은 자리를 스스로 예약한다).
        let cmd = format!(
            "printf '\\033]1337;File=name={name_b64};size=68;width=4;inline=1:{PNG_1X1}\\007\\n\\n\\n'\n"
        );
        sess.send_bytes(cmd.as_bytes()).unwrap();
    }

    fn wait_views(sess: &PtySession, want: usize) -> Vec<kasa_bridge::screen::InlineImageView> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let v = sess.full_snapshot().inline_images;
            if v.len() == want || Instant::now() > deadline {
                return v;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn osc1337_lands_as_a_placed_view() {
        let sess = sh("test-inline");
        emit(&sess, "YS5wbmc="); // a.png
        let views = wait_views(&sess, 1);
        assert_eq!(views.len(), 1, "이미지가 뷰로 안 실렸다");
        let v = &views[0];
        assert_eq!(v.cols, 4, "width=4(셀) 가 그대로 와야 한다");
        // 1x1 픽셀 → rows = ceil(4/2.1) = 2.
        assert_eq!(v.rows, 2);
        assert!((0..10).contains(&v.row), "뷰포트 밖 배치: row={}", v.row);
        assert!(std::path::Path::new(&v.path).exists(), "temp 파일이 없다");
        let _ = std::fs::remove_file(&v.path);
    }

    /// 같은 (name, size) 재전송은 새 그림이 아니라 **이동**이다 — recall 은 그림
    /// 자리가 밀리면 같은 시퀀스를 다시 흘린다(recall 35b7f5e). 두 장으로 쌓이면
    /// 재전송마다 화면에 그림이 늘어난다.
    #[test]
    fn resend_of_same_image_moves_instead_of_duplicating() {
        let sess = sh("test-inline-move");
        emit(&sess, "Yi5wbmc="); // b.png
        let first = wait_views(&sess, 1);
        assert_eq!(first.len(), 1);
        emit(&sess, "Yi5wbmc=");
        // 재전송 완료를 화면 마커로 못박는다 — 이동 결과가 첫 배치와 같은 상대
        // 위치라, 뷰만 봐서는 「처리 전」과 「이동 후」가 구별되지 않는다.
        sess.send_bytes(b"printf 'RESEND-DONE\\n'\n").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !sess.visible_text(10).contains("RESEND-DONE") {
            assert!(Instant::now() < deadline, "마커가 10초 안에 안 떴다");
            std::thread::sleep(Duration::from_millis(50));
        }
        let v = sess.full_snapshot().inline_images;
        assert_eq!(v.len(), 1, "재전송이 두 장으로 쌓였다: {v:?}");
        assert_eq!(v[0].id, first[0].id, "이동인데 id 가 바뀌었다(텍스처 캐시가 죽는다)");
        assert_eq!(v[0].path, first[0].path, "이동인데 파일이 바뀌었다");
        let _ = std::fs::remove_file(&v[0].path);
    }

    /// 스크롤백으로 밀려난 그림은 그 프레임의 뷰에서 빠지고, 위로 올리면
    /// 돌아온다 — 「그림이 대화 기록에 남는다」의 실체다.
    #[test]
    fn scrolled_out_image_returns_when_scrolling_back() {
        let sess = sh("test-inline-scroll");
        emit(&sess, "Yy5wbmc="); // c.png
        let placed = wait_views(&sess, 1);
        assert_eq!(placed.len(), 1);
        // 화면(10행)보다 많이 밀어 그림을 히스토리로 보낸다.
        sess.send_bytes(b"printf '\\n\\n\\n\\n\\n\\n\\n\\n\\n\\n\\n\\n\\n\\n'\n").unwrap();
        let gone = wait_views(&sess, 0);
        assert!(gone.is_empty(), "밀려난 그림이 뷰에 남았다: {gone:?}");
        // 한 줄씩 올리며 찾는다 — 한 번에 크게 올리면 앵커를 지나칠 수 있고
        // (실측: scroll(20)이 정확히 한 줄 지나쳐 row=10 에 뒀다), 얼마나
        // 올려야 하는지는 배너·프롬프트 줄수에 따라 환경마다 다르다.
        let mut back = Vec::new();
        for _ in 0..40 {
            sess.scroll(1);
            back = sess.full_snapshot().inline_images;
            if !back.is_empty() {
                break;
            }
        }
        assert_eq!(back.len(), 1, "스크롤백을 다 올려도 그림이 안 돌아왔다");
        let _ = std::fs::remove_file(&back[0].path);
    }
}

#[cfg(test)]
mod snapshot_fidelity_tests {
    use super::*;

    /// 픽스처를 뜬 pane 의 실제 크기. 폭이 어긋나면 재생 자체가 무의미해진다
    /// (넓은 화면에서 뜬 녹음을 좁은 화면에 틀면 divider 가 줄바꿈돼 겹친다).
    fn fixture_size() -> (u16, u16) {
        let g = |k: &str, d: u16| {
            std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d)
        };
        (g("KASATERM_SNAPSHOT_COLS", 363), g("KASATERM_SNAPSHOT_ROWS", 39))
    }

    /// 우리 `snapshot()` 이 alacritty 그리드를 그대로 옮기는가.
    ///
    /// agy(antigravity CLI) 화면이 kasaterm 에서만 뒤엉키는 걸 쫓다 여기까지 왔다
    /// (2026-08-11). 같은 바이트를 alacritty Term 에 먹이면 그리드는 **정확한데**
    /// pane 에 보이는 건 두 줄이 한 줄로 뭉쳐 있었다 — 파서가 아니라 그 위 변환층
    /// 이 범인이라는 뜻이다. 이 테스트가 그 경계를 못박는다.
    ///
    /// 입력은 `KASATERM_SNAPSHOT_FIXTURE` 로 준 실제 녹음 파일. 없으면 건너뛴다
    /// (녹음은 크고 사람 대화가 들어 있어 레포에 넣지 않는다).
    #[test]
    fn snapshot_rows_match_the_alacritty_grid() {
        let Some(path) = std::env::var_os("KASATERM_SNAPSHOT_FIXTURE") else { return };
        let raw = std::fs::read(&path).expect("fixture");
        let (cols, rows) = fixture_size();

        let listener = PtyEventForwarder {
            respond: true,
            writer: Arc::new(Mutex::new(Box::new(std::io::sink()))),
            size: Arc::new(Mutex::new((cols, rows))),
            last_title: Arc::new(Mutex::new(None)),
        };
        let mut term = make_term(cols, rows, listener);
        let mut proc: Processor<StdSyncHandler> = Processor::new();
        proc.advance(&mut term, &raw);

        // 기준 = alacritty 그리드에서 직접 읽은 줄.
        let truth: Vec<String> = {
            let g = term.grid();
            (0..rows as usize)
                .map(|r| {
                    let line = alacritty_terminal::index::Line(r as i32);
                    (0..cols as usize)
                        .map(|c| g[line][alacritty_terminal::index::Column(c)].c)
                        .collect::<String>()
                        .trim_end()
                        .to_string()
                })
                .collect()
        };

        let upd = snapshot(&mut term, cols, rows, "%t", &Arc::new(Mutex::new(None)), true);
        let mut got = vec![String::new(); rows as usize];
        for (r, row) in &upd.dirty {
            got[*r as usize] =
                row.iter().map(|c| c.ch).collect::<String>().trim_end().to_string();
        }

        check(&truth, &got, "한 번에 먹였을 때");
    }

    /// 같은 바이트를 **live 처럼 잘게 나눠** 먹이고 damage 기반 스냅샷을 누적한다.
    ///
    /// 실제 reader 가 하는 그대로다 — 읽을 때마다 `advance` → `scroll_display(Bottom)`
    /// → `snapshot(force_full=false)` → 돌아온 dirty 행만 화면에 반영. 한 번에 먹이면
    /// 멀쩡한데 이렇게 하면 어긋난다면, 범인은 파서도 스냅샷도 아니고 **부분갱신 누적**이다.
    #[test]
    fn chunked_damage_snapshots_still_match_the_grid() {
        let Some(path) = std::env::var_os("KASATERM_SNAPSHOT_FIXTURE") else { return };
        let raw = std::fs::read(&path).expect("fixture");
        // ★높이를 좁혀 **스크롤이 일어나게** 한다. live pane 은 앞선 셸 출력 때문에
        // agy 화면이 늘 스크롤 상태고, damage 는 뷰포트 기준이라 스크롤이 섞이면
        // "안 damaged 된 행"이 실은 다른 내용을 가리키게 된다.
        let (cols, base_rows) = fixture_size();
        for rows in [base_rows, base_rows / 2, 12] {
        for chunk in [65536usize, 4096, 1024, 512, 128] {
            let listener = PtyEventForwarder {
                respond: true,
                writer: Arc::new(Mutex::new(Box::new(std::io::sink()))),
                size: Arc::new(Mutex::new((cols, rows))),
                last_title: Arc::new(Mutex::new(None)),
            };
            let mut term = make_term(cols, rows, listener);
            let mut proc: Processor<StdSyncHandler> = Processor::new();
            let title = Arc::new(Mutex::new(None));
            let mut mirror = vec![String::new(); rows as usize];
            // ★reader 와 **똑같이** NFC 정규화를 거쳐 넘긴다. 이걸 빼면 실제 경로가
            // 아니다 — 비-ASCII 청크만 정규화되므로 한글이 든 스트림에서만 갈린다.
            let mut u8buf = Utf8Buffer::new();
            for part in raw.chunks(chunk) {
                use unicode_normalization::UnicodeNormalization;
                let batch = u8buf.process(part);
                let nfc_holder: Option<String> = if batch.is_ascii() {
                    None
                } else {
                    std::str::from_utf8(&batch).ok().map(|s| s.nfc().collect())
                };
                let fed: &[u8] =
                    nfc_holder.as_deref().map(str::as_bytes).unwrap_or(batch.as_slice());
                proc.advance(&mut term, fed);
                if proc.sync_bytes_count() > 0 {
                    continue; // reader 와 같은 규칙
                }
                term.scroll_display(alacritty_terminal::grid::Scroll::Bottom);
                let upd = snapshot(&mut term, cols, rows, "%t", &title, false);
                for (r, row) in &upd.dirty {
                    mirror[*r as usize] =
                        row.iter().map(|c| c.ch).collect::<String>().trim_end().to_string();
                }
            }
            let truth = grid_rows(&term, cols, rows);
            check(&truth, &mirror, &format!("rows={rows} chunk={chunk}"));
        }
        }
    }

    /// `Utf8Buffer` 가 바이트를 잃지 않는가. 청크 경계에서 갈라 넣어도 이어붙인
    /// 결과는 원본과 **바이트 단위로 같아야** 한다.
    #[test]
    fn utf8_buffer_never_loses_bytes() {
        let Some(path) = std::env::var_os("KASATERM_SNAPSHOT_FIXTURE") else { return };
        let raw = std::fs::read(&path).expect("fixture");
        for chunk in [raw.len(), 4096, 1024, 128, 7, 1] {
            let mut b = Utf8Buffer::new();
            let mut out: Vec<u8> = Vec::new();
            for part in raw.chunks(chunk.max(1)) {
                out.extend_from_slice(&b.process(part));
            }
            assert_eq!(
                out.len(),
                raw.len(),
                "chunk={chunk}: {}바이트가 사라졌다",
                raw.len() as i64 - out.len() as i64
            );
            assert!(out == raw, "chunk={chunk}: 내용이 달라졌다");
        }
    }

    /// agy 가 실제로 보낸 모양: 한글 코드포인트를 SGR 이스케이프 사이에서 끊는다.
    /// 옛 구현은 이 배치를 통째로 버렸다 — 화면에서 프레임이 통으로 사라진 원인.
    #[test]
    fn a_truncated_codepoint_mid_batch_never_eats_the_batch() {
        let batch = b"\x1b[38;2;1;2;3m\xeb\x94\x1b[m\xed\x95\x9c \xec\xa4\x84\r\n";
        let mut b = Utf8Buffer::new();
        let out = b.process(batch);
        assert_eq!(out, batch, "깨진 바이트 하나에 배치가 통째로 사라졌다");
    }

    /// 잘린 코드포인트는 **다음 read 까지만** 보류하고, 이어지면 그대로 흘려보낸다.
    #[test]
    fn a_codepoint_split_across_reads_is_rejoined() {
        let mut b = Utf8Buffer::new();
        assert_eq!(b.process(b"ab\xed\x95"), b"ab", "잘린 앞부분을 안 붙들었다");
        assert_eq!(b.process(b"\x9ccd"), "한cd".as_bytes(), "이어붙이지 못했다");
    }

    /// 위로 올린 화면에 **히스토리가 실제로 보이는가**.
    ///
    /// 기준값을 그리드에서 뽑으면 동어반복이 되므로 내용으로 못박는다: 40줄을 흘린
    /// 10행 화면은 L31..L40 을 보여주고, 5줄 올리면 L26..L35 여야 한다. 예전 코드는
    /// 히스토리를 음수 `Line` 으로 읽지 못하게 막아 윗 5줄이 빈칸이 됐다.
    #[test]
    fn scrolling_up_actually_shows_history() {
        let (cols, rows) = (40u16, 10u16);
        let listener = PtyEventForwarder {
            respond: true,
            writer: Arc::new(Mutex::new(Box::new(std::io::sink()))),
            size: Arc::new(Mutex::new((cols, rows))),
            last_title: Arc::new(Mutex::new(None)),
        };
        let mut term = make_term(cols, rows, listener);
        let mut proc: Processor<StdSyncHandler> = Processor::new();
        let mut feed = String::new();
        for i in 1..=40 {
            feed.push_str(&format!("L{i}\r\n"));
        }
        proc.advance(&mut term, feed.as_bytes());
        let title = Arc::new(Mutex::new(None));

        let view = |t: &mut Term<PtyEventForwarder>| -> Vec<String> {
            let upd = snapshot(t, cols, rows, "%t", &title, true);
            let mut got = vec![String::new(); rows as usize];
            for (r, row) in &upd.dirty {
                got[*r as usize] =
                    row.iter().map(|c| c.ch).collect::<String>().trim_end().to_string();
            }
            got
        };

        let live = view(&mut term);
        assert_eq!(&live[..9], &["L32", "L33", "L34", "L35", "L36", "L37", "L38", "L39", "L40"]);

        term.scroll_display(alacritty_terminal::grid::Scroll::Delta(5));
        assert_eq!(term.grid().display_offset(), 5, "스크롤이 안 걸렸다");
        let scrolled = view(&mut term);
        assert_eq!(
            &scrolled[..],
            &["L27", "L28", "L29", "L30", "L31", "L32", "L33", "L34", "L35", "L36"],
            "위로 올린 화면에 히스토리가 안 보인다"
        );
    }

    #[test]
    fn new_output_does_not_pull_a_scrolled_view_to_the_bottom() {
        let (cols, rows) = (40u16, 10u16);
        let listener = PtyEventForwarder {
            respond: true,
            writer: Arc::new(Mutex::new(Box::new(std::io::sink()))),
            size: Arc::new(Mutex::new((cols, rows))),
            last_title: Arc::new(Mutex::new(None)),
        };
        let mut term = make_term(cols, rows, listener);
        let mut proc: Processor<StdSyncHandler> = Processor::new();
        let mut initial = String::new();
        for i in 1..=40 {
            initial.push_str(&format!("L{i}\r\n"));
        }
        proc.advance(&mut term, initial.as_bytes());
        term.scroll_display(alacritty_terminal::grid::Scroll::Delta(5));

        let title = Arc::new(Mutex::new(None));
        let visible = |t: &mut Term<PtyEventForwarder>| -> Vec<String> {
            let update = snapshot(t, cols, rows, "%t", &title, true);
            update
                .dirty
                .into_iter()
                .map(|(_, row)| row.iter().map(|cell| cell.ch).collect::<String>())
                .collect()
        };
        let before_offset = term.grid().display_offset();
        let before_view = visible(&mut term);
        let follow_live_tail = before_offset == 0;
        proc.advance(&mut term, b"L41\r\n");
        if follow_live_tail {
            term.scroll_display(alacritty_terminal::grid::Scroll::Bottom);
        }
        let after_view = visible(&mut term);

        assert!(before_offset > 0);
        assert!(
            term.grid().display_offset() > 0,
            "새 출력이 위로 올린 화면을 맨 아래로 끌어내렸다"
        );
        assert_eq!(
            after_view, before_view,
            "새 출력이 읽고 있던 줄을 움직였다"
        );
    }

    /// 뷰포트 위 행 읽기 — 가까운 순이고, 스크롤을 따라가며, 히스토리 바닥에서
    /// 캡된다. 렌더러의 팀메시지 이어칠하기(헤더가 화면 위로 나간 본문)가 이
    /// 순서를 전제로 위로 걷는다.
    #[test]
    fn rows_above_walks_history_nearest_first() {
        let (cols, rows) = (40u16, 10u16);
        let listener = PtyEventForwarder {
            respond: true,
            writer: Arc::new(Mutex::new(Box::new(std::io::sink()))),
            size: Arc::new(Mutex::new((cols, rows))),
            last_title: Arc::new(Mutex::new(None)),
        };
        let mut term = make_term(cols, rows, listener);
        let mut proc: Processor<StdSyncHandler> = Processor::new();
        let mut feed = String::new();
        for i in 1..=40 {
            feed.push_str(&format!("L{i}\r\n"));
        }
        proc.advance(&mut term, feed.as_bytes());
        let texts = |rows: &[Row]| -> Vec<String> {
            rows.iter()
                .map(|r| r.iter().map(|c| c.ch).collect::<String>().trim_end().to_string())
                .collect()
        };
        // 라이브 바닥: 화면 첫 줄이 L32 이므로 그 위는 L31 부터 가까운 순.
        assert_eq!(texts(&read_rows_above(&term, 3)), ["L31", "L30", "L29"]);
        term.scroll_display(alacritty_terminal::grid::Scroll::Delta(5));
        assert_eq!(texts(&read_rows_above(&term, 3)), ["L26", "L25", "L24"]);
        // 히스토리 바닥 캡 — 남은 것보다 많이 달라면 있는 만큼만
        // (히스토리 L1~L31 = 31행, 스크롤 5 를 빼면 26행).
        assert_eq!(read_rows_above(&term, 1000).len(), 26);
    }

    fn grid_rows(term: &Term<PtyEventForwarder>, cols: u16, rows: u16) -> Vec<String> {
        let g = term.grid();
        (0..rows as usize)
            .map(|r| {
                let line = alacritty_terminal::index::Line(r as i32);
                (0..cols as usize)
                    .map(|c| g[line][alacritty_terminal::index::Column(c)].c)
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    fn check(truth: &[String], got: &[String], what: &str) {
        let rows = truth.len();
        let mut bad = Vec::new();
        for r in 0..rows {
            // 넓은 글자 뒷칸을 어느 쪽이 어떻게 채우든 **글자 자체**는 같아야 한다.
            let norm = |s: &str| s.replace('\0', "").replace(' ', "");
            if norm(&truth[r]) != norm(&got[r]) {
                bad.push(format!("  행 {r}\n    그리드: {:?}\n    스냅샷: {:?}", truth[r], got[r]));
            }
        }
        assert!(bad.is_empty(), "[{what}] 그리드와 스냅샷이 어긋난다:\n{}", bad.join("\n"));
    }
}

#[cfg(test)]
mod prompt_anchor_tests {
    use super::*;

    /// 주어진 줄들을 그대로 그리드에 찍은 Term. 실제 PTY 없이 스캔만 검증한다.
    fn term_with(lines: &[&str]) -> Term<PtyEventForwarder> {
        let (cols, rows) = (60u16, 10u16);
        let listener = PtyEventForwarder {
            respond: true,
            writer: Arc::new(Mutex::new(Box::new(std::io::sink()))),
            size: Arc::new(Mutex::new((cols, rows))),
            last_title: Arc::new(Mutex::new(None)),
        };
        let mut term = make_term(cols, rows, listener);
        let mut proc: Processor<StdSyncHandler> = Processor::new();
        proc.advance(&mut term, lines.join("\r\n").as_bytes());
        term
    }

    /// 확정된 프롬프트와 **입력 중인 줄**을 가르는 것은 마커 뒤 한 글자뿐이다 —
    /// claude 는 확정된 것에 일반 공백(U+0020), 화면 하단 입력창에 NBSP(U+00A0)를
    /// 쓴다(2026-08-15 살아 있는 pane 9개 대조로 확정).
    ///
    /// 이 시험이 지키는 것: claude 가 렌더를 바꿔 그 규칙이 깨지면 **여기서** 터진다.
    /// 안 그러면 「지금 치고 있는 줄」이 지나간 질문 목록 끝에 조용히 끼어드는
    /// 형태로만 드러나고, 그건 화면을 한참 보고서야 알아챈다.
    #[test]
    fn input_box_line_is_not_a_past_prompt() {
        let t = term_with(&[
            "\u{276f} 확정된 질문",
            "  답변 줄",
            "\u{276f}\u{a0}지금 치고 있는 줄",
        ]);
        let got: Vec<String> = scan_prompt_anchors(&t).into_iter().map(|a| a.text).collect();
        assert_eq!(got, vec!["확정된 질문".to_string()]);
    }

    /// 위 시험의 **대조군** — 같은 문장이 NBSP 대신 일반 공백이면 잡혀야 한다.
    /// 둘을 짝으로 둬야 「NBSP 한 글자가 유일한 차이」가 증명된다. 짝이 없으면
    /// 판정이 엉뚱한 이유(길이·위치)로 걸러도 앞 시험만 보고 통과로 읽는다.
    #[test]
    fn the_same_line_with_a_plain_space_is_a_prompt() {
        let t = term_with(&["\u{276f} 지금 치고 있는 줄"]);
        let got: Vec<String> = scan_prompt_anchors(&t).into_iter().map(|a| a.text).collect();
        assert_eq!(got, vec!["지금 치고 있는 줄".to_string()]);
    }

    /// wide 글리프(한글)의 뒤칸은 `\0` 이 아니라 진짜 공백이고 구분은 플래그에만
    /// 있다. 문자만 보고 거르면 「질 문  1」처럼 글자마다 벌어진다(실측으로 겪음).
    #[test]
    fn wide_glyph_spacers_do_not_leak_into_the_text() {
        let t = term_with(&["\u{276f} 질문 1 한글이 섞인 프롬프트"]);
        let got = scan_prompt_anchors(&t);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].text, "질문 1 한글이 섞인 프롬프트");
    }

    /// ASCII `>` 는 마커가 아니다 — diff·인용·다른 TUI 가 행 머리에 흔히 써서,
    /// 그것까지 세면 대화와 무관한 줄이 턴 목록에 들어찬다.
    #[test]
    fn ascii_angle_bracket_is_not_a_marker() {
        let t = term_with(&["> 인용문이거나 diff 한 줄", "\u{276f} 진짜 질문"]);
        let got: Vec<String> = scan_prompt_anchors(&t).into_iter().map(|a| a.text).collect();
        assert_eq!(got, vec!["진짜 질문".to_string()]);
    }

    /// 마커만 있고 본문이 빈 줄(비어 있는 입력창)은 갈 곳이 못 된다.
    #[test]
    fn empty_prompt_line_is_skipped() {
        let t = term_with(&["\u{276f}", "\u{276f} 내용 있는 질문"]);
        assert_eq!(scan_prompt_anchors(&t).len(), 1);
    }

    /// 절대 줄 번호는 화면 첫 줄이 아니라 **세션 시작**부터 센다 — 스크롤 위치를
    /// 그 번호로 되돌리는 계산(`hist - abs`)이 그 전제 위에 있다.
    #[test]
    fn anchor_line_numbers_count_from_the_session_start() {
        let t = term_with(&["첫 줄", "\u{276f} 둘째 줄의 질문"]);
        let got = scan_prompt_anchors(&t);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].abs_line, 1);
    }
}

#[cfg(test)]
mod external_session_tests {
    use super::*;

    struct ChanWriter(std::sync::mpsc::Sender<Vec<u8>>);
    impl Write for ChanWriter {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            let _ = self.0.send(b.to_vec());
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn ext_session(
        cols: u16,
        rows: u16,
    ) -> (
        Arc<PtySession>,
        crossbeam_channel::Sender<ExtEvent>,
        std::sync::mpsc::Receiver<Vec<u8>>,
        Arc<Mutex<Vec<(u16, u16)>>>,
    ) {
        let (etx, erx) = crossbeam_channel::unbounded();
        let (wtx, wrx) = std::sync::mpsc::channel::<Vec<u8>>();
        let resized: Arc<Mutex<Vec<(u16, u16)>>> = Default::default();
        let r2 = Arc::clone(&resized);
        let sess = PtySession::start_external(
            PtyOptions {
                cols,
                rows,
                pane_id: "rmt-test".into(),
                ..Default::default()
            },
            ExternalIo {
                events: erx,
                writer: Box::new(ChanWriter(wtx)),
                on_resize: Arc::new(move |c, r| r2.lock().unwrap().push((c, r))),
            },
        )
        .expect("start_external");
        (Arc::new(sess), etx, wrx, resized)
    }

    fn wait_text(sess: &PtySession, needle: &str) -> bool {
        let deadline = Instant::now() + std::time::Duration::from_secs(2);
        while Instant::now() < deadline {
            if sess
                .screens
                .recv_timeout(std::time::Duration::from_millis(200))
                .is_ok()
                && sess.visible_text(50).contains(needle)
            {
                return true;
            }
        }
        false
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
    fn external_setsize_applies_before_following_bytes() {
        // SetSize 가 같은 채널에 실리므로, 뒤따르는 바이트는 반드시 새 격자로
        // 파싱된다 — 이 순서 보장이 ExtEvent 설계의 요점이다.
        let (sess, etx, _wrx, _resized) = ext_session(20, 5);
        etx.send(ExtEvent::SetSize(40, 10)).unwrap();
        etx.send(ExtEvent::Bytes(b"resized-frame".to_vec())).unwrap();
        assert!(wait_text(&sess, "resized-frame"));
        assert_eq!(sess.size(), (40, 10));
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
