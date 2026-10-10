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
//!
//! 필드는 여기 `PtySession` 한 곳에 두고, 자식 모듈이 책임별로 제 몫의 `impl` 을 든다:
//! `lifecycle`(띄우기·입양·닫기) · `reader`(읽기 스레드) · `vt`(파서·답장·호스트 색) ·
//! `grid`(격자 → 행·ANSI) · `screen`(읽기·구독 API) · `viewport`(크기의 주인) · `input`(쓰기 관문) ·
//! `blocks`(OSC 133) · `inline`(OSC 1337·kitty 그림) · `process`·`agents`(프로세스·에이전트 감지) ·
//! `registry`(세션 등록부).

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
use kasa_screen::screen::{Cell, Color, Row, ScreenUpdate};

mod lifecycle;
mod registry;
mod input;
mod viewport;
mod screen;
mod grid;
mod vt;
mod reader;
mod blocks;
mod inline;
mod process;
mod agents;
#[cfg(test)]
mod test_support;

pub use agents::{AGENT_TABLE, AgentKind, AgentSpec, agent_for_shell, agent_pid_for_shell};
pub use blocks::CommandBlock;
pub use lifecycle::{ExternalIo, ExtEvent};
pub use process::{fresh_process_table, process_table, process_table_shared};
pub use registry::{
    keep_session, kept_sessions, live_sessions, lookup_session, register_session, release_session,
};
pub use screen::{PromptAnchor, ViewerSnapshot};
pub use vt::set_host_colors;
pub(crate) use inline::{b64_decode, png_size};
pub(crate) use process::{process_cmdline, process_env_vars};
use agents::AgentsViewCache;
use inline::InlineImgs;
use viewport::ViewportSizes;
use vt::PtyEventForwarder;

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

pub struct PtySession {
    /// Channel the renderer consumes — one ScreenUpdate per dirty
    /// frame after VT processing landed new state.
    pub screens: Receiver<ScreenUpdate>,
    io: SessionIo,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    /// Shared cell-dim state used by the resize path so we can reshape
    /// the VT grid without re-creating the Term.
    size: Arc<Mutex<(u16, u16)>>,
    // GUI layout remains the fallback while a viewer supplies the logical grid.
    // Hold this lock through ioctl and publication so concurrent owners cannot interleave.
    viewport_sizes: Mutex<ViewportSizes>,
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
    // argv scans must never hold up rendering, including the first lookup.
    agents_cache: Arc<AgentsViewCache>,
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
    /// 블록 저장소의 바뀜 번호. 0 이면 OSC 133 표지를 한 번도 못 봤다(셸 통합 없음).
    /// 표지·출력이 올 때마다 오른다 — 거울의 긴 폴링이 이것만 보고 깬다.
    block_rev: Arc<std::sync::atomic::AtomicU64>,
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
    /// Closed panes reject all user/control input while awaiting disposal.
    input_closed: std::sync::atomic::AtomicBool,
    input_revision: std::sync::atomic::AtomicU64,
    input_draft: std::sync::atomic::AtomicBool,
    /// 초안 표시가 마지막으로 서거나 다시 선 시각. 표시는 Enter 로만 꺼지므로 Esc·Ctrl+C 로 비운 입력칸도
    /// 영영 초안으로 남았다 — 이 시각이 오래됐으면 표시 대신 화면이 판정한다(`input_draft_recent`).
    draft_marked_at: Mutex<Option<Instant>>,
    /// tell 이 붙여 넣고 Enter 를 칠 동안 사람 입력을 붙들어 두는 자리(기한, 모인 바이트). 그 틈에 친 글이
    /// 붙여 넣은 본문 뒤에 붙어 함께 제출되던 자리다(2026-09-29). 기한이 지나면 다음 쓰기가 먼저 흘려보낸다.
    input_hold: Mutex<Option<(Instant, Vec<u8>)>>,
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
    /// 사람이 무언가를 **누른** 마지막 시각 — `last_input` 에서 포커스·휠·호버 리포트를 뺀 것.
    /// 엔진이 승인 창의 답을 알리지 않아, 창이 뜬 뒤 칸에 들어온 이 입력이 「답했다」의 증거다.
    last_key: Mutex<Option<Instant>>,
}
