//! PTY 로 들어가는 쓰기의 단일 관문 — 닫힌 칸 거절·tell 의 비교 후 쓰기·사람 입력 붙들기·
//! 초안/제출/키 시각, 그리고 그 시각을 쓰는 출력 박동 판정.

use super::*;
use super::process::process_table_poke;
#[cfg(test)]
use super::test_support::ext_session;

impl PtySession {
    pub fn input_closed(&self) -> bool {
        self.input_closed.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Serialize with writes so no delayed submit can cross the close boundary.
    /// Interrupt is deliberately opt-in: a mirror must never cancel its source.
    pub fn set_input_closed(&self, closed: bool, interrupt: bool) -> Result<()> {
        let mut writer = self.writer.lock().unwrap();
        let was_closed = self.input_closed.swap(closed, std::sync::atomic::Ordering::AcqRel);
        if was_closed != closed {
            self.input_revision.fetch_add(1,std::sync::atomic::Ordering::AcqRel);
        }
        if closed && !was_closed && interrupt {
            writer.write_all(b"\x03").context("interrupt closing pane")?;
            writer.flush()?;
        }
        Ok(())
    }

    pub fn send_bytes(&self, bytes: &[u8]) -> Result<()> {
        self.send_bytes_guarded(bytes,None).map(|_|())
    }

    pub fn input_revision(&self) -> u64 {
        self.input_revision.load(std::sync::atomic::Ordering::Acquire)
    }

    pub fn input_quiet_for(&self, duration: std::time::Duration) -> bool {
        self.last_input.lock().unwrap().is_none_or(|at|at.elapsed() >= duration)
    }

    pub fn input_draft_present(&self) -> bool {
        self.input_draft.load(std::sync::atomic::Ordering::Acquire)
    }

    /// 초안 표시가 `within` 안에 섰는가. 그보다 오래된 표시는 화면이 확인할 몫이다 — 친 글은 그 사이
    /// 입력칸에 그려졌거나(그러면 화면이 막는다) 지워졌다.
    pub fn input_draft_recent(&self, within: std::time::Duration) -> bool {
        self.input_draft_present()
            && self.draft_marked_at.lock().unwrap().is_some_and(|at| at.elapsed() < within)
    }

    /// 새 에이전트 세션이 섰다(claude SessionStart). 그 입력창은 빈 채로 뜨므로, 셸에 넣은 부팅
    /// 줄이 남긴 초안 표시를 거둔다 — 부팅 줄은 `send` 로 LF 로 끝나 위의 「제출」 판정(CR)에
    /// 안 걸리고, 사람이 Enter 를 칠 때까지 tell 이 「초안 있음」으로 영영 미뤄졌다(2026-09-28
    /// 새로 띄운 학생에게 첫 브리프가 4분 넘게 안 들어감). 부팅 줄 **뒤에** 누가 또 쳤으면
    /// (마지막 입력이 마지막 줄바꿈보다 늦다) 진짜 초안일 수 있어 그대로 둔다.
    pub fn agent_session_started(&self) -> bool {
        let _writer = self.writer.lock().unwrap();
        let input = *self.last_input.lock().unwrap();
        let submit = *self.last_submit.lock().unwrap();
        if input.is_some_and(|input| submit.is_none_or(|submit| input > submit)) {
            return false;
        }
        self.input_draft.store(false,std::sync::atomic::Ordering::Release);
        true
    }

    pub fn reserve_input_draft(&self) {
        let _writer = self.writer.lock().unwrap();
        self.input_draft.store(true,std::sync::atomic::Ordering::Release);
        *self.draft_marked_at.lock().unwrap() = Some(Instant::now());
        self.input_revision.fetch_add(1,std::sync::atomic::Ordering::AcqRel);
    }

    /// tell 이 붙여 넣고 Enter 를 칠 동안 사람 입력(`expected` 없는 쓰기)을 붙들어 둔다. 풀리는 것은
    /// `release_input` 이나 기한이다. 붙여넣기·Enter 는 `expected` 를 달고 오므로 그대로 지나간다.
    pub fn hold_input(&self, for_: std::time::Duration) {
        let mut hold = self.input_hold.lock().unwrap();
        let held = hold.take().map(|(_, bytes)| bytes).unwrap_or_default();
        *hold = Some((Instant::now() + for_, held));
    }

    /// 붙들어 둔 입력을 친 순서 그대로 흘려보낸다.
    pub fn release_input(&self) -> Result<()> {
        let held = self.input_hold.lock().unwrap().take().map(|(_, bytes)| bytes).unwrap_or_default();
        if held.is_empty() {
            return Ok(());
        }
        self.send_bytes_guarded(&held, None).map(|_| ())
    }

    /// Compare and write under the same lock used by keyboard, web and socket input.
    pub fn send_bytes_guarded(&self, bytes: &[u8], expected: Option<u64>) -> Result<u64> {
        // 포커스 리포트(CSI I/O)는 pane 전환마다 앱이 자동으로 쏘는 것이라 사람
        // 입력이 아니다 — 이걸 세면 working pane 으로 포커스를 옮길 때마다 박동
        // 억제가 걸려 바가 1.5초 꺼졌다 켜진다.
        if bytes != b"\x1b[I" && bytes != b"\x1b[O" {
            *self.last_input.lock().unwrap() = Some(Instant::now());
        }
        // 포커스·휠·호버 리포트는 입력창 글을 못 바꾼다. 이걸 입력으로 세면 학생 창을 스크롤하거나
        // 마우스만 올려도 tell 이 붙여 넣은 뒤 Enter 를 보류해 글이 입력창에 남고(2026-09-29 두 건),
        // 초안 표시가 서서 다음 tell 은 사람이 Enter 를 칠 때까지 줄만 섰다.
        let passive = passive_report(bytes);
        if !passive {
            *self.last_key.lock().unwrap() = Some(Instant::now());
        }
        let merged;
        let bytes = if expected.is_none() && !passive {
            let mut hold = self.input_hold.lock().unwrap();
            match hold.take() {
                Some((until, mut held)) if Instant::now() < until => {
                    held.extend_from_slice(bytes);
                    *hold = Some((until, held));
                    return Ok(self.input_revision());
                }
                Some((_, mut held)) if !held.is_empty() => {
                    held.extend_from_slice(bytes);
                    merged = held;
                    &merged[..]
                }
                _ => bytes,
            }
        } else {
            bytes
        };
        let revision = {
            let mut w = self.writer.lock().unwrap();
            anyhow::ensure!(!self.input_closed(), "pane is closed — reopen it before sending input");
            anyhow::ensure!(expected.is_none_or(|revision|revision == self.input_revision()), "input changed during tell delivery");
            // 클릭은 입력칸에 글을 못 넣는다 — 세면 학생 창을 눌러 보기만 해도 사람이 Enter 를 칠 때까지
            // tell 이 줄만 섰다(2026-10-01 실측: 하루 61건 중 21건 만료).
            if expected.is_none() && !passive && !mouse_report(bytes) {
                // A separately submitted Enter proves a user draft is gone;
                // editing, wrapped lines and attachment sequences do not.
                let submitted = bytes == b"\r" || (bytes.ends_with(b"\r") && !bytes.contains(&0x1b));
                self.input_draft.store(!submitted,std::sync::atomic::Ordering::Release);
                if !submitted { *self.draft_marked_at.lock().unwrap() = Some(Instant::now()); }
            }
            let revision = if passive { self.input_revision() }
                else { self.input_revision.fetch_add(1,std::sync::atomic::Ordering::AcqRel) + 1 };
            w.write_all(bytes).context("pty write")?;
            // Flush immediately. Without this, a one-shot write that isn't
            // followed by another (a committed Hangul syllable — the next
            // keystroke only updates the preedit overlay, not the PTY) sits
            // in the writer buffer until something else flushes it, so the
            // shell echoes "안" ~0.2s late and the user sees only the preedit
            // "ㄴ" until then. ASCII typing hid this because each keystroke's
            // write flushed the previous one.
            w.flush().context("pty flush")?;
            revision
        };
        if bytes.iter().any(|b| matches!(b, b'\r' | b'\n')) {
            *self.last_submit.lock().unwrap() = Some(Instant::now());
            // Enter 는 「새 전경 프로세스가 곧 뜬다」의 가장 이른 신호이기도 하다 —
            // 테이블을 앞당겨 읽어 두면 `claude` 를 친 pane 이 배너 첫 프레임부터
            // 에이전트로 판정된다(process_table_poke 머리말).
            process_table_poke();
        }
        Ok(revision)
    }

    /// 마지막 CR/LF 가 이 PTY 로 들어간 시각 — 없으면 아직 아무 제출도 없었다.
    pub fn last_submit(&self) -> Option<Instant> {
        *self.last_submit.lock().unwrap()
    }

    pub fn last_key(&self) -> Option<Instant> {
        *self.last_key.lock().unwrap()
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
}

/// 입력창 글을 바꿀 수 없는 리포트 — 포커스(CSI I/O), SGR 마우스 휠, 버튼 없는 이동(호버).
/// 클릭·끌기는 TUI 의 선택지를 누를 수 있어 입력으로 친다.
/// 마우스 리포트(SGR `CSI < … M|m`, 옛 X10 `CSI M` + 3바이트) 한 덩어리인가.
fn mouse_report(bytes: &[u8]) -> bool {
    if let Some(rest) = bytes.strip_prefix(b"\x1b[<") {
        return matches!(rest.last(), Some(b'M' | b'm'))
            && rest[..rest.len() - 1].iter().all(|b| b.is_ascii_digit() || *b == b';');
    }
    bytes.len() == 6 && bytes.starts_with(b"\x1b[M")
}

fn passive_report(bytes: &[u8]) -> bool {
    if bytes == b"\x1b[I" || bytes == b"\x1b[O" {
        return true;
    }
    let Some(body) = bytes.strip_prefix(b"\x1b[<")
        .and_then(|rest| rest.strip_suffix(b"M").or_else(|| rest.strip_suffix(b"m")))
    else {
        return false;
    };
    let Some(button) = std::str::from_utf8(body).ok()
        .and_then(|body| body.split(';').next())
        .and_then(|button| button.parse::<u16>().ok())
    else {
        return false;
    };
    button & 64 != 0 || (button & 32 != 0 && button & 3 == 3)
}

#[cfg(test)]
mod passive_report_tests {
    #[test]
    fn focus_wheel_and_hover_never_count_as_typing() {
        for passive in [&b"\x1b[I"[..], b"\x1b[O", b"\x1b[<64;10;5M", b"\x1b[<65;10;5M", b"\x1b[<35;80;20M"] {
            assert!(super::passive_report(passive), "{passive:?}");
        }
        for typing in [&b"\x1b[<0;10;5M"[..], b"\x1b[<0;10;5m", b"\x1b[<32;10;5M", b"\r", b"a", b"\x1b[A", b"\x1b[<64;10"] {
            assert!(!super::passive_report(typing), "{typing:?}");
        }
    }

    #[test]
    fn wheel_and_hover_keep_the_tell_guard_and_draft_state() {
        let (_events, erx) = crossbeam_channel::unbounded();
        let pty = super::PtySession::start_external(
            super::PtyOptions { pane_id: "passive-report".into(), ..Default::default() },
            super::ExternalIo {
                events: erx,
                writer: Box::new(std::io::sink()),
                on_resize: std::sync::Arc::new(|_, _| {}),
            },
        ).unwrap();
        pty.send_bytes(b"\r").unwrap();
        let pasted = pty.send_bytes_guarded(b"\x1b[200~tell\x1b[201~", Some(pty.input_revision())).unwrap();
        pty.send_bytes(b"\x1b[<65;10;5M").unwrap();
        pty.send_bytes(b"\x1b[<35;11;5M").unwrap();
        pty.send_bytes(b"\x1b[I").unwrap();
        assert_eq!(pty.input_revision(), pasted, "휠·호버·포커스가 붙여넣기 뒤 Enter 를 막으면 안 된다");
        assert!(!pty.input_draft_present(), "휠·호버는 초안이 아니다");
        pty.send_bytes(b"\x1b[<0;10;5M").unwrap();
        assert_ne!(pty.input_revision(), pasted, "클릭은 여전히 입력으로 센다");
        pty.send_bytes(b"\x1b[<0;10;5m").unwrap();
        assert!(!pty.input_draft_present(), "클릭은 입력칸에 글을 못 넣는다 — 초안이 아니다");
    }

    #[test]
    fn a_keystroke_marks_a_recent_draft_that_the_screen_takes_over_later() {
        let (_events, erx) = crossbeam_channel::unbounded();
        let pty = super::PtySession::start_external(
            super::PtyOptions { pane_id: "draft-recent".into(), ..Default::default() },
            super::ExternalIo { events: erx, writer: Box::new(std::io::sink()), on_resize: std::sync::Arc::new(|_, _| {}) },
        ).unwrap();
        let window = std::time::Duration::from_millis(80);
        assert!(!pty.input_draft_recent(window), "막 연 칸은 표시가 서도 시각이 없다 — 화면이 판정한다");
        pty.send_bytes(b"\x1b").unwrap();
        assert!(pty.input_draft_recent(window), "Esc 직후는 화면이 아직 못 따라왔을 수 있다");
        std::thread::sleep(window * 2);
        assert!(pty.input_draft_present() && !pty.input_draft_recent(window), "Enter 없이도 시간이 지나면 화면이 정본");
        pty.send_bytes(b"\r").unwrap();
        assert!(!pty.input_draft_recent(window));
        pty.reserve_input_draft();
        assert!(pty.input_draft_recent(window), "이미지 붙여넣기 예약도 같은 창 안에서 막는다");
    }

    #[test]
    fn mouse_reports_are_recognized_whole() {
        for mouse in [&b"\x1b[<0;10;5M"[..], b"\x1b[<2;1;1m", b"\x1b[<32;3;4M", b"\x1b[M #!"] {
            assert!(super::mouse_report(mouse), "{mouse:?}");
        }
        for other in [&b"\x1b[<0;10"[..], b"\x1b[A", b"a", b"\x1b[<0;1;1Mabc", b"\x1b"] {
            assert!(!super::mouse_report(other), "{other:?}");
        }
    }
}

#[cfg(test)]
mod input_gate_tests {
    use super::*;

    #[test]
    fn closed_input_gate_rejects_delayed_messages_and_reopens_without_replay() {
        let (sess, _events, writer, _) = ext_session(20, 5);
        sess.set_input_closed(true, true).unwrap();
        assert_eq!(writer.recv().unwrap(), b"\x03");
        assert!(sess.send_bytes(b"new work\r").is_err());
        sess.set_input_closed(false, false).unwrap();
        assert!(writer.try_recv().is_err());
        sess.send_bytes(b"fresh input").unwrap();
        assert_eq!(writer.recv().unwrap(), b"fresh input");
    }

    #[test]
    fn guarded_tell_preserves_intervening_draft_and_withholds_enter() {
        let (session,_events,writer,_) = ext_session(20,5);
        let revision = session.input_revision();
        let revision = session.send_bytes_guarded(b"\x1b[200~hello\x1b[201~",Some(revision)).unwrap();
        session.send_bytes(b"user draft").unwrap();
        assert!(session.send_bytes_guarded(b"\r",Some(revision)).is_err());
        assert_eq!(writer.recv().unwrap(),b"\x1b[200~hello\x1b[201~");
        assert_eq!(writer.recv().unwrap(),b"user draft");
        assert!(writer.try_recv().is_err());
    }

    /// 붙여넣기와 Enter 사이에 사람이 친 글은 본문에 안 섞이고, Enter 뒤 빈 입력창으로 들어간다.
    #[test]
    fn held_user_input_lands_after_the_tell_submits() {
        let (session,_events,writer,_) = ext_session(20,5);
        session.hold_input(std::time::Duration::from_secs(5));
        let revision = session.send_bytes_guarded(b"\x1b[200~hello\x1b[201~",Some(session.input_revision())).unwrap();
        session.send_bytes(b"\xec\x95\x88").unwrap();
        session.send_bytes(b"\x1b[I").unwrap();
        session.send_bytes(b"x").unwrap();
        session.send_bytes_guarded(b"\r",Some(revision)).expect("held input must not break the tell guard");
        session.release_input().unwrap();
        assert_eq!(writer.recv().unwrap(),b"\x1b[200~hello\x1b[201~");
        assert_eq!(writer.recv().unwrap(),b"\x1b[I", "포커스 리포트는 붙들지 않는다");
        assert_eq!(writer.recv().unwrap(),b"\r");
        assert_eq!(writer.recv().unwrap(),"안x".as_bytes(), "친 순서 그대로 한 번에");
        assert!(session.input_draft_present(), "흘려보낸 글은 초안이다 — 다음 tell 이 기다린다");
        session.release_input().unwrap();
        assert!(writer.try_recv().is_err());
    }

    /// 기한이 지나면 푸는 쪽이 안 와도 다음 입력이 모인 것을 먼저 흘려보낸다.
    #[test]
    fn expired_hold_flushes_before_the_next_input() {
        let (session,_events,writer,_) = ext_session(20,5);
        session.hold_input(std::time::Duration::from_millis(0));
        std::thread::sleep(std::time::Duration::from_millis(5));
        session.send_bytes(b"a").unwrap();
        assert_eq!(writer.recv().unwrap(),b"a");
        session.hold_input(std::time::Duration::from_millis(30));
        session.send_bytes(b"b").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(40));
        session.send_bytes(b"c").unwrap();
        assert_eq!(writer.recv().unwrap(),b"bc");
    }

    #[test]
    fn guarded_tell_closing_between_paste_and_enter_cannot_submit() {
        let (session,_events,writer,_) = ext_session(20,5);
        let revision = session.send_bytes_guarded(b"hello",Some(session.input_revision())).unwrap();
        session.set_input_closed(true,false).unwrap();
        assert!(session.send_bytes_guarded(b"\r",Some(revision)).is_err());
        assert_eq!(writer.recv().unwrap(),b"hello");
        assert!(writer.try_recv().is_err());
    }

    #[test]
    fn guarded_tell_reports_disconnect_after_body_instead_of_success() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self,_bytes: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe,"fake PTY disconnected"))
            }
            fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
        }
        let (session,_events,writer,_) = ext_session(20,5);
        let revision = session.send_bytes_guarded(b"hello",Some(session.input_revision())).unwrap();
        assert_eq!(writer.recv().unwrap(),b"hello");
        *session.writer.lock().unwrap() = Box::new(Broken);
        assert!(session.send_bytes_guarded(b"\r",Some(revision)).is_err());
        assert_ne!(session.input_revision(),revision);
    }

    #[test]
    fn closed_mirror_gate_never_interrupts_or_terminates_its_source() {
        let (sess, _events, writer, _) = ext_session(20, 5);
        sess.set_input_closed(true, false).unwrap();
        sess.terminate_local();
        assert!(writer.try_recv().is_err());
        assert!(sess.send_bytes(b"forbidden").is_err());
        sess.set_input_closed(false, false).unwrap();
        sess.send_bytes(b"still attached").unwrap();
        assert_eq!(writer.recv().unwrap(), b"still attached");
    }
}
