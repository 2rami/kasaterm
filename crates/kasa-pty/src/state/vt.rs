//! VT 파서(`alacritty_terminal::Term`) 만들기와 그 이벤트 답장 — 제목·클립보드(OSC 52)·
//! 색 질의·크기 질의, 호스트 색, 스크롤백 예산.

use super::*;
#[cfg(test)]
use super::test_support::test_posix_shell;

impl PtySession {
    /// 이 pane 의 앱이 DECSET 2031(컬러스킴 변경 알림)을 켰는가. 테마 전환 때
    /// 여기 참인 pane 에만 `CSI ?997;1n`(다크)/`;2n`(라이트) 리포트를 보낸다.
    pub fn wants_scheme_reports(&self) -> bool {
        self.scheme_reports
            .load(std::sync::atomic::Ordering::Relaxed)
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
///   - ClipboardLoad — paste request; reply with what the host's
///     clipboard sink gives (`HostPolicy::clipboard`)
///
/// MouseCursorDirty / Title / Bell / etc are pure UI signals; the
/// renderer reads title/cursor state from the snapshot, so we drop
/// them here.
pub(super) struct PtyEventForwarder {
    pub(super) writer: Arc<Mutex<Box<dyn Write + Send>>>,
    pub(super) size: Arc<Mutex<(u16, u16)>>,
    /// false = 자동 응답(DSR-CPR·OSC 색 질의·TextAreaSize·클립보드 read)을 묻는다.
    /// 원격 미러(`start_external`)용 — 원격 호스트의 Term 이 이미 답하고 있어서,
    /// 여기서도 답하면 원격 앱이 응답을 두 번 받아 입력줄에 이스케이프가 박힌다.
    /// (아무도 안 답하면 cmd.exe 류가 DSR 대기로 멎는 함정이 있지만, 그 「한 명」은
    /// 원격 쪽이다 — 렌더버그 카탈로그 #10 참고.)
    pub(super) respond: bool,
    /// Latest OSC 0 / OSC 2 title pushed by the shell or any TUI
    /// running inside it. `None` after `ResetTitle` or until the
    /// first set. The reader thread reads this on each snapshot so
    /// the renderer's pane-header strip can reflect "✱ Claude Code",
    /// "vim filename", current cwd, etc. — anything the inner
    /// program decides to advertise.
    pub(super) last_title: Arc<Mutex<Option<String>>>,
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
    pub(super) fn write_to_pty(&self, bytes: &[u8]) {
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
            AlacEvent::PtyWrite(s) => {
                // alacritty 의 DA1 답은 칸 하나짜리 VT102(`ESC[?6c`)인데, `kitten icat`
                // (0.47)은 이 모양만 못 읽어 그림 지원 감지가 끝나지 않고 시간 초과로
                // 그리기를 포기한다(`?62c`·`?6;c`·`?62;22c` 는 다 넘어갔다). ghostty 와
                // 같은 VT220+색(`?62;22c`)으로 답한다 — 질의를 DA1 로 마무리하는
                // 앱(Claude Code 포함)은 내용 없이 도착만 본다.
                let s = if s == "\x1b[?6c" { "\x1b[?62;22c".to_string() } else { s };
                self.write_to_pty(s.as_bytes())
            }
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
                let (cw, ch) = crate::kitty::cell_pixels().unwrap_or((7, 16));
                let reply = formatter(WindowSize {
                    num_lines: rows,
                    num_cols: cols,
                    cell_width: cw.min(u16::MAX as u32) as u16,
                    cell_height: ch.min(u16::MAX as u32) as u16,
                });
                self.write_to_pty(reply.as_bytes());
            }
            AlacEvent::ClipboardLoad(_, formatter) => {
                // 실패해도 빈 글로 답한다 — 답이 없으면 셸이 붙여넣기 응답을 기다리며 멎는다.
                let text = crate::host::clipboard_load();
                let reply = formatter(&text);
                self.write_to_pty(reply.as_bytes());
            }
            AlacEvent::ClipboardStore(_, text) => {
                // OSC 52 set — Claude Code, helix, etc. push selected
                // text into the host clipboard through this.
                let preview: String = text.chars().take(40).collect();
                eprintln!(
                    "[pty-backend] OSC 52 set ({} chars): {preview:?}",
                    text.len()
                );
                crate::host::clipboard_store(&text);
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
/// pane 이 1,925줄밖에 못 남겼다 — 사용자: "히스토리가 왜 다 안 남지, 보려고 위로
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
pub(super) fn history_lines_for_cols(cols: u16) -> usize {
    let per_line = (cols.max(1) as usize) * SCROLLBACK_BYTES_PER_CELL;
    (scrollback_budget_bytes() / per_line).clamp(SCROLLBACK_MIN_LINES, SCROLLBACK_MAX_LINES)
}

pub(super) fn make_term(cols: u16, rows: u16, listener: PtyEventForwarder) -> Term<PtyEventForwarder> {
    let size = TermSize::new(cols as usize, rows as usize);
    let config = TermConfig {
        scrolling_history: history_lines_for_cols(cols),
        ..TermConfig::default()
    };
    Term::new(config, &size, listener)
}

#[cfg(test)]
mod scrollback_probe {
    use super::*;

    /// 실 PTY 로 스크롤백 **보존 줄수와 그 대가(RSS)** 를 잰다 — 사용자: "히스토리가 왜
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
