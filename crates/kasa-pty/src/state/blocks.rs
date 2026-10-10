//! OSC 133 명령 블록(Warp 식) — 리더가 raw 바이트에서 C/D 표지를 잡아 명령·출력·종료 코드를 모은다.

use super::*;
use super::reader::find_subslice;
#[cfg(test)]
use super::test_support::{ext_session, wait_rev};

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
    /// 출력이 상한을 넘어 앞에서 버린 줄 수. 거울은 끝(빌드 오류·테스트 결과)이 중요해
    /// 머리가 아니라 꼬리를 남긴다.
    pub dropped_lines: usize,
    /// 명령이 돈 폴더 — C 순간에 셸이 마지막으로 알린 cwd(OSC 9;9). 결과 속 상대 경로를
    /// 이 폴더 기준으로 풀어 고리로 만든다. 알림이 없는 셸이면 None.
    pub cwd: Option<String>,
}

impl PtySession {
    /// Shared handle to this pane's command-block store. The GUI hands this Arc
    /// to the socket backend (via `pane_status_pub`) so `/blocks` can read the
    /// blocks without touching `App.pty` — no per-frame snapshot/clone.
    pub fn blocks_arc(&self) -> Arc<Mutex<VecDeque<CommandBlock>>> {
        Arc::clone(&self.blocks)
    }

    /// 블록 저장소의 바뀜 번호(0 = 셸 통합 표지를 아직 못 봄).
    pub fn blocks_rev(&self) -> u64 {
        self.block_rev.load(std::sync::atomic::Ordering::Acquire)
    }
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
pub(super) fn parse_command_blocks(
    bytes: &[u8],
    term: &Arc<Mutex<Term<PtyEventForwarder>>>,
    size: (u16, u16),
    cwd: &Mutex<Option<std::path::PathBuf>>,
    blocks: &Arc<Mutex<VecDeque<CommandBlock>>>,
    rev: &std::sync::atomic::AtomicU64,
    utf8_tail: &mut Vec<u8>,
    capturing: &mut bool,
    seq: &mut u64,
    start: &mut Option<Instant>,
    prompt: Option<(u16, u16)>,
    early_command: &mut Option<String>,
) {
    const PREFIX: &[u8] = b"\x1b]133;";
    // Fast path: nothing to do unless we're mid-block or a mark is present.
    if !*capturing && find_subslice(bytes, PREFIX).is_none() {
        return;
    }
    let bump = || {
        rev.fetch_add(1, std::sync::atomic::Ordering::Release);
    };
    let mut data = bytes;
    loop {
        match find_subslice(data, PREFIX) {
            None => {
                if *capturing && !data.is_empty() {
                    block_append_output(blocks, data, utf8_tail);
                    bump();
                }
                return;
            }
            Some(p) => {
                // Bytes before this mark are command output (when capturing).
                if *capturing {
                    block_append_output(blocks, &data[..p], utf8_tail);
                }
                bump();
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
                        let command = early_command
                            .take()
                            .unwrap_or_else(|| command_at_cursor(&term.lock().unwrap(), size, prompt));
                        utf8_tail.clear();
                        let cwd = cwd.lock().ok().and_then(|c| c.clone()).map(|p| p.display().to_string());
                        block_begin(blocks, seq, command, cwd);
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

fn block_begin(blocks: &Arc<Mutex<VecDeque<CommandBlock>>>, seq: &mut u64, command: String, cwd: Option<String>) {
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
        dropped_lines: 0,
        cwd,
    });
    while b.len() > BLOCK_CAP {
        b.pop_front();
    }
}

fn block_append_output(blocks: &Arc<Mutex<VecDeque<CommandBlock>>>, chunk: &[u8], utf8_tail: &mut Vec<u8>) {
    if chunk.is_empty() {
        return;
    }
    // Alt-screen enter ⇒ a TUI (vim/htop/less); its raw run isn't a clean block.
    let is_tui = find_subslice(chunk, b"\x1b[?1049h").is_some();
    utf8_tail.extend_from_slice(chunk);
    let text = take_utf8(utf8_tail);
    let mut b = blocks.lock().unwrap();
    if let Some(last) = b.back_mut() {
        if is_tui {
            last.is_tui = true;
        }
        last.output.push_str(&text);
        if last.output.len() > BLOCK_OUTPUT_CAP {
            // 한글 한가운데서 자르면 슬라이스가 패닉해 그 칸의 읽기 스레드가 죽고 화면이 멈춘다.
            let keep_from = last.output.ceil_char_boundary(last.output.len() - BLOCK_OUTPUT_CAP * 3 / 4);
            let cut = last.output[keep_from..].find('\n').map_or(keep_from, |nl| keep_from + nl + 1);
            last.dropped_lines += last.output[..cut].matches('\n').count();
            last.output.drain(..cut);
        }
    }
}

/// 앞에서부터 온전한 UTF-8 만 꺼내고, 끝에 걸린 미완성 글자는 `buf` 에 남긴다.
/// 깨진 바이트(진짜 잘못된 것)는 대체 문자로 바꿔 넘긴다.
fn take_utf8(buf: &mut Vec<u8>) -> String {
    let mut out = String::new();
    loop {
        match std::str::from_utf8(buf) {
            Ok(s) => {
                out.push_str(s);
                buf.clear();
                return out;
            }
            Err(e) => {
                let good = e.valid_up_to();
                out.push_str(std::str::from_utf8(&buf[..good]).unwrap_or_default());
                match e.error_len() {
                    None => {
                        buf.drain(..good);
                        return out;
                    }
                    Some(bad) => {
                        out.push('\u{FFFD}');
                        buf.drain(..good + bad);
                    }
                }
            }
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

/// Read the typed command out of the grid at C time. Enter moves the cursor to
/// the next line before preexec emits C — and when the prompt sat on the last
/// row that newline scrolls the screen, so the B mark's row is stale by one.
/// The command therefore ends on the row just above the cursor; walk up through
/// soft-wrapped rows to its first row and start that one at the B column.
/// display_offset is 0 here (the reader snaps to the live tail on output).
pub(super) fn command_at_cursor(
    t: &Term<PtyEventForwarder>,
    size: (u16, u16),
    prompt: Option<(u16, u16)>,
) -> String {
    use alacritty_terminal::index::{Column, Line};
    use alacritty_terminal::term::cell::Flags;
    let Some((prow, pcol)) = prompt else {
        return String::new();
    };
    let grid = t.grid();
    let glines = grid.screen_lines() as i32;
    let gcols = grid.columns().min(size.0 as usize);
    if gcols == 0 {
        return String::new();
    }
    let cursor = grid.cursor.point;
    let wrapped = |l: i32| grid[Line(l)][Column(gcols - 1)].flags.contains(Flags::WRAPLINE);
    let (first, last) = if cursor.column.0 == 0 && cursor.line.0 > 0 {
        let last = cursor.line.0 - 1;
        let mut first = last;
        while first > 0 && wrapped(first - 1) {
            first -= 1;
        }
        (first, last)
    } else {
        (prow as i32, prow as i32)
    };
    if first < 0 || last >= glines {
        return String::new();
    }
    let mut s = String::new();
    for line in first..=last {
        let from = if line == first { pcol as usize } else { 0 };
        for c in from..gcols {
            let cell = &grid[Line(line)][Column(c)];
            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            s.push(cell.c);
        }
    }
    s.trim_end().to_string()
}

#[cfg(test)]
mod command_block_tests {
    use super::*;

    /// 거울의 셸 묶음 데이터: 통합 표지 전엔 rev 0, 한글이 read 경계에서 잘려도 온전하고,
    /// 도는 동안 rev 가 오르며, D 가 종료 코드로 닫는다.
    #[test]
    fn command_block_store_feeds_mirrors() {
        let (sess, etx, _w, _) = ext_session(40, 6);
        assert_eq!(sess.blocks_rev(), 0, "표지 전엔 통합 없음");
        etx.send(ExtEvent::Bytes(b"\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\x1b]133;C\x07".to_vec())).unwrap();
        let r1 = wait_rev(&sess, 0);
        assert!(r1 > 0);
        let han = "한글".as_bytes();
        etx.send(ExtEvent::Bytes([b"a ".as_slice(), &han[..4]].concat())).unwrap();
        wait_rev(&sess, r1);
        etx.send(ExtEvent::Bytes([&han[4..], b"\r\n".as_slice()].concat())).unwrap();
        // rev 한 번 오른 것만 보고 단언하면 느린 러너에선 나머지 바이트가 아직 안 붙었다. 원하는 모양이 될
        // 때까지 기다리고, 단언은 잠금 밖에서 한다 — 잠금을 쥔 채 실패하면 읽기 스레드까지 Poison 으로 죽는다.
        let last = |want: &dyn Fn(&CommandBlock) -> bool| {
            let deadline = Instant::now() + std::time::Duration::from_secs(10);
            loop {
                let got = sess.blocks.lock().unwrap().back().cloned();
                if got.as_ref().is_some_and(want) || Instant::now() >= deadline {
                    return got;
                }
                let _ = sess.screens.recv_timeout(std::time::Duration::from_millis(50));
            }
        };
        let running = last(&|b| b.output == "a 한글\r\n").unwrap();
        assert_eq!(running.exit_code, None, "D 전엔 도는 중");
        assert_eq!(running.output, "a 한글\r\n");
        etx.send(ExtEvent::Bytes(b"\x1b]133;D;2\x07".to_vec())).unwrap();
        assert_eq!(last(&|b| b.exit_code.is_some()).unwrap().exit_code, Some(2));
        assert!(!sess.alt_screen());
    }

    /// 블록은 C 순간에 셸이 마지막으로 알린 폴더(OSC 9;9)를 단다 — 명령 뒤의 cd 는 다음 블록 몫이다.
    #[test]
    fn command_block_remembers_where_it_ran() {
        let (sess, etx, _w, _) = ext_session(40, 6);
        etx.send(ExtEvent::Bytes(b"\x1b]9;9;/tmp/a b\x07$ \x1b]133;B\x07cd x\r\n\x1b]133;C\x07".to_vec())).unwrap();
        let r1 = wait_rev(&sess, 0);
        etx.send(ExtEvent::Bytes(b"\x1b]133;D;0\x07\x1b]9;9;/tmp/a b/x\x07$ \x1b]133;B\x07ls\r\n\x1b]133;C\x07".to_vec())).unwrap();
        let deadline = Instant::now() + std::time::Duration::from_secs(2);
        while sess.blocks.lock().unwrap().len() < 2 && Instant::now() < deadline {
            wait_rev(&sess, r1);
        }
        let b = sess.blocks.lock().unwrap();
        let cwds: Vec<_> = b.iter().map(|b| b.cwd.as_deref()).collect();
        assert_eq!(cwds, vec![Some("/tmp/a b"), Some("/tmp/a b/x")]);
    }

    /// 프롬프트가 맨 아랫줄이면 Enter 가 화면을 한 줄 밀고 C 가 온다 — B 행은 낡았다.
    /// 감긴 긴 명령·한글도 한 줄로 읽혀야 한다.
    #[test]
    fn command_text_survives_the_enter_scroll() {
        let (sess, etx, _w, _) = ext_session(20, 4);
        let wait_cmd = |n: usize| {
            let deadline = Instant::now() + std::time::Duration::from_secs(2);
            loop {
                let _ = sess.screens.recv_timeout(std::time::Duration::from_millis(50));
                let b = sess.blocks.lock().unwrap();
                if b.len() >= n || Instant::now() > deadline {
                    return b.get(n - 1).map(|b| b.command.clone()).unwrap_or_default();
                }
            }
        };
        let send = |b: &[u8]| etx.send(ExtEvent::Bytes(b.to_vec())).unwrap();
        send(b"1\r\n2\r\n3\r\n$ \x1b]133;B\x07");
        let _ = sess.screens.recv_timeout(std::time::Duration::from_millis(300));
        send("echo 한글 끝".as_bytes());
        // 첫 출력이 C 와 한 묶음으로 와서 명령 줄을 밀어 올린다.
        send("\r\n\x1b]133;C\x07한글 끝\r\n둘\r\n셋\r\n".as_bytes());
        assert_eq!(wait_cmd(1), "echo 한글 끝");
        send(b"x\r\n\x1b]133;D;0\x07$ \x1b]133;B\x07");
        let _ = sess.screens.recv_timeout(std::time::Duration::from_millis(300));
        send(b"printf abcdefghijklmnopqrstuvwxyz");
        send(b"\r\n\x1b]133;C\x07");
        assert_eq!(wait_cmd(2), "printf abcdefghijklmnopqrstuvwxyz");
    }

    #[test]
    fn long_block_output_keeps_the_tail() {
        let blocks: Arc<Mutex<VecDeque<CommandBlock>>> = Arc::default();
        let mut seq = 0;
        block_begin(&blocks, &mut seq, "yes".into(), None);
        let mut tail = Vec::new();
        let line = format!("{}\n", "y".repeat(99));
        for _ in 0..(BLOCK_OUTPUT_CAP / 100 + 500) {
            block_append_output(&blocks, line.as_bytes(), &mut tail);
        }
        block_append_output(&blocks, b"END\n", &mut tail);
        let b = blocks.lock().unwrap();
        let last = b.back().unwrap();
        assert!(last.output.len() <= BLOCK_OUTPUT_CAP);
        assert!(last.output.ends_with("END\n"));
        assert!(last.output.starts_with('y'), "줄 머리에서 자른다");
        assert!(last.dropped_lines > 0);
    }

    #[test]
    fn long_block_output_cuts_on_a_char_boundary() {
        let blocks: Arc<Mutex<VecDeque<CommandBlock>>> = Arc::default();
        let mut seq = 0;
        block_begin(&blocks, &mut seq, "claude".into(), None);
        let mut tail = Vec::new();
        // 3바이트 글자만 이어지다 끝에 1바이트가 붙으면 자를 자리가 글자 한가운데에 떨어진다.
        let text = format!("{}a", "하".repeat(BLOCK_OUTPUT_CAP / 3 + 10));
        block_append_output(&blocks, text.as_bytes(), &mut tail);
        let b = blocks.lock().unwrap();
        let last = b.back().unwrap();
        assert!(last.output.len() <= BLOCK_OUTPUT_CAP);
        assert!(last.output.starts_with('하'));
        assert!(last.output.ends_with('a'));
    }
}
