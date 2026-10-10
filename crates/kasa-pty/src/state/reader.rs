//! 읽기 스레드 — PTY 바이트를 받아 tee·OSC 스니핑(블록·그림·cwd·알림)·VT 파싱·프레임 발행을
//! `term` 락 안에서 한 번에 한다. 출력 박동도 여기서 찍는다.

use super::*;
use super::blocks::{command_at_cursor, parse_command_blocks};
use super::grid::snapshot;
use super::inline::{
    InlineScan, advance_scanning_inline, attach_inline_views, inline_cell_flow, scan_inline_image,
};
use super::screen::publish_screen_update;
use super::vt::history_lines_for_cols;

/// 이 출력 청크가 **반짝임만 다시 그린 프레임**인가 — 박동(`output_beats`)에서 뺄지 가른다.
///
/// codex 의 Astra 효과(gpt-6-astra)는 **노는 동안에도** 입력창 둘레에 점자 「별」을
/// 계속 흩뿌린다. 그 프레임 하나하나가 PTY read 로 잡히니, 박동이 1Hz 넘게 찍혀
/// `output_heartbeat` 가 영영 참이 됐다 — pane 은 놀고 있는데 헤더 working 바가
/// 멈추지 않고 지나다녔다(2026-09-16 지적). 박동은 「스피너 경과시간이 1초마다 다시
/// 그려진다」를 재는 자라, 말이 없는 장식 프레임은 그 자에 들어오면 안 된다.
///
/// 판정: ESC 시퀀스(커서 이동·색)를 걷어낸 뒤 남은 글자가 **전부** 공백·제어·점자면
/// 장식이다. 진짜 생성 중이면 경과시간 숫자가 함께 갱신되므로 이 관문에 안 걸린다.
/// claude 의 점자 스피너(`⠋ Computing… (3s)`)도 같은 청크에 글자가 실려 통과한다.
fn chunk_is_only_particles(buf: &[u8]) -> bool {
    let mut i = 0usize;
    let mut saw = false;
    while i < buf.len() {
        let b = buf[i];
        if b == 0x1b {
            i += 1;
            match buf.get(i) {
                // CSI: 파라미터·중간 바이트를 지나 최종 바이트(0x40~0x7E)까지.
                Some(b'[') => {
                    i += 1;
                    while i < buf.len() && !(0x40..=0x7e).contains(&buf[i]) {
                        i += 1;
                    }
                    i += 1;
                }
                // OSC/DCS/APC 류: BEL 이나 ST(ESC \) 까지.
                Some(b']') | Some(b'P') | Some(b'_') | Some(b'^') | Some(b'X') => {
                    i += 1;
                    while i < buf.len() {
                        if buf[i] == 0x07 {
                            i += 1;
                            break;
                        }
                        if buf[i] == 0x1b && buf.get(i + 1) == Some(&b'\\') {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                }
                _ => i += 1,
            }
            continue;
        }
        // 점자 한 글자(U+2800~U+28FF = E2 A0 80 ~ E2 A3 BF).
        if b == 0xe2
            && matches!(buf.get(i + 1), Some(0xa0..=0xa3))
            && matches!(buf.get(i + 2), Some(0x80..=0xbf))
        {
            saw = true;
            i += 3;
            continue;
        }
        // 공백과 제어문자는 장식 프레임의 자리 이동이라 말로 안 친다.
        if b == b' ' || b < 0x20 || b == 0x7f {
            i += 1;
            continue;
        }
        return false;
    }
    saw
}

pub(super) fn spawn_reader_thread(
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
    block_rev: Arc<std::sync::atomic::AtomicU64>,
    cwd_handle: Arc<Mutex<Option<std::path::PathBuf>>>,
    byte_taps: Arc<Mutex<Vec<Sender<Vec<u8>>>>>,
    screen_taps: Arc<Mutex<Vec<Sender<ScreenUpdate>>>>,
    inline_imgs: Arc<Mutex<InlineImgs>>,
    responder: PtyEventForwarder,
    scheme_reports: Arc<std::sync::atomic::AtomicBool>,
    output_beats: Arc<Mutex<VecDeque<Instant>>>,
    parsed_generation: Option<Arc<std::sync::atomic::AtomicU64>>,
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
        // read 경계에서 잘린 다중 바이트 글자 — 다음 배치 앞에 붙여 한글이 깨지지 않게.
        let mut blk_utf8: Vec<u8> = Vec::new();
        // 이번 묶음의 C 직전에 읽어 둔 명령 줄(있으면 격자를 다시 읽지 않는다).
        let mut blk_cmd: Option<String> = None;

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
                    // 말 없는 장식 프레임(codex Astra 의 점자 반짝임)은 빼고 센다 —
                    // `chunk_is_only_particles` 머리말.
                    if !chunk_is_only_particles(&buf[..n]) {
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
                    }
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
            // Sniff for iTerm OSC 1337 inline images. The scan walks the
            // byte slice, so we cheaply prefix-check first — most reads have
            // no `\x1b]1337` and we skip the walk entirely. Critical for TUI
            // throughput (claude code emits thousands of small reads per
            // second without it).
            // 셀-흐름 모드에선 이 시퀀스를 term 락 안의 advance_scanning_inline
            // 이 잡는다(커서 위치가 필요해서다). 여기 레거시 스캔은 탭 모드 전용.
            if !inline_cell_flow()
                && (img_capturing
                    || memchr::memmem::find(processed_bytes, b"\x1b]1337").is_some())
            {
                scan_inline_image(processed_bytes, &mut img_buf, &mut img_capturing);
            }
            // OSC 777 desktop notification: alacritty drops it unhandled like
            // OSC 1337, so sniff the raw batch and stash until a snapshot
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
                if inline_scan.busy()
                    || (inline_cell_flow()
                        && memchr::memmem::find(processed_bytes, b"\x1b]1337").is_some())
                    || memchr::memmem::find(processed_bytes, b"\x1b_G").is_some()
                    || processed_bytes.ends_with(b"\x1b")
                    || processed_bytes.ends_with(b"\x1b_")
                {
                    advance_scanning_inline(
                        &mut processor,
                        &mut t,
                        processed_bytes,
                        &mut inline_scan,
                        &inline_imgs,
                        &responder,
                    );
                } else if let Some(at) = find_subslice(processed_bytes, b"\x1b]133;C") {
                    // 명령의 첫 출력이 C 와 한 묶음으로 오면, 묶음을 다 먹인 뒤엔 명령 줄이
                    // 이미 출력에 밀려 있다 — C 앞까지 먹이고 그 자리에서 읽는다.
                    processor.advance(&mut *t, &processed_bytes[..at]);
                    blk_cmd = Some(command_at_cursor(&t, current_size, blk_prompt));
                    processor.advance(&mut *t, &processed_bytes[at..]);
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
                    snap.live_output = true;
                    snap.output_generation = parsed_generation.as_ref().map_or(0, |generation|
                        generation.load(std::sync::atomic::Ordering::Acquire));
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
                &cwd_handle,
                &blocks,
                &block_rev,
                &mut blk_utf8,
                &mut blk_capturing,
                &mut blk_seq,
                &mut blk_start,
                blk_prompt,
                &mut blk_cmd,
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
pub(super) struct Utf8Buffer {
    leftover: Vec<u8>,
}

impl Utf8Buffer {
    pub(super) fn new() -> Self {
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
    pub(super) fn process(&mut self, data: &[u8]) -> Vec<u8> {
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

/// First index where `needle` occurs in `haystack`, or None. Tiny
/// linear scan — used only to sniff the short OSC 133 prompt marker out
/// of each PTY read batch.
pub(super) fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}

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

#[cfg(test)]
mod utf8_buffer_tests {
    use super::*;

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
}

#[cfg(test)]
mod astra_heartbeat_tests {
    use super::chunk_is_only_particles;

    #[test]
    fn decorative_particle_frames_do_not_beat() {
        // codex Astra 가 노는 동안 되풀이하는 프레임 — 커서 이동·색·점자·공백뿐.
        let frame = b"\x1b[2;7H\x1b[38;5;240m\xe2\xa0\x88   \xe2\xa1\x80 \x1b[0m\x1b[3;40H\xe2\xa0\x84";
        assert!(chunk_is_only_particles(frame), "별밭을 박동으로 셌다");
    }

    #[test]
    fn a_frame_with_words_beats() {
        // 진짜 생성 중이면 경과시간이 함께 갱신된다.
        assert!(!chunk_is_only_particles(
            "\x1b[2;1H⠋ Computing… (3s)".as_bytes()
        ));
        assert!(!chunk_is_only_particles(b"\x1b[2J\x1b[Hhello"));
        // 점 하나 없는 커서 이동만 — 박동으로 셀 근거도, 뺄 근거도 아니다.
        assert!(!chunk_is_only_particles(b"\x1b[2;7H"));
    }
}
