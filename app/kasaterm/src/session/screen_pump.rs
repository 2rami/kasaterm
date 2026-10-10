//! 화면 펌프 — PTY 세션이 내는 화면 갱신(ScreenUpdate)을 칸·탭에 싣는다. 펌프 스레드와
//! 원격 스트림이 같은 적용 함수(`apply_screen_update`)를 지난다.
use super::*;

/// Coalesce screen data without turning a resize into lost live readiness or
/// letting an older connection certify a new one. ExtReader emits generations
/// only on parsed live bytes; resize/history snapshots have false + generation 0.
pub(super) fn coalesce_screen_updates(
    previous: kasa_bridge::screen::ScreenUpdate,
    mut next: kasa_bridge::screen::ScreenUpdate,
) -> kasa_bridge::screen::ScreenUpdate {
    let untagged_snapshot = !next.live_output && next.output_generation == 0;
    let same_generation = next.output_generation == previous.output_generation;
    if untagged_snapshot {
        next.output_generation = previous.output_generation;
        next.live_output = previous.live_output;
    } else if same_generation {
        next.live_output |= previous.live_output;
    }
    // Only dimensions invalidate dirty-row coordinates. Generation marks the
    // last parsed fragment, not the start of a full grid: earlier fragments of
    // a large snapshot can legitimately carry the preceding generation.
    if (next.cols, next.rows) != (previous.cols, previous.rows) {
        return next;
    }
    let mut rows: std::collections::HashMap<u16, Row> = previous.dirty.into_iter().collect();
    rows.extend(next.dirty);
    next.dirty = rows.into_iter().collect();
    next
}

impl App {
    /// Drain a PtySession's screen-update channel into shared workspace
    /// state. Used both by `start_pty` (initial pane) and by
    /// `split_active_pane` (every additional pane), so the per-pane
    /// state arrives through the same path no matter when the session
    /// was spawned.
    /// Apply one decoded ScreenUpdate to the workspace: route to the right
    /// tab, reflow on size change, blit dirty rows, carry cursor/mode/title.
    /// Shared by the in-process channel pump (`pump_pty_screens`) and the
    /// daemon stream pump (`pump_daemon_stream`). The caller holds the ws lock
    /// and fires the redraw; this only mutates ws.

    pub(crate) fn apply_screen_update(
        ws: &mut Workspace,
        update: kasa_bridge::screen::ScreenUpdate,
    ) {
        if ws.active_pane.is_none() {
            ws.active_pane = Some(update.pane_id.clone());
        }
        // 배정 캐릭터를 PaneState 에 동기 — has_header 가 이걸 보고 단일 pane 도 헤더 띠를
        // 띄운다(사용자: 터미널에도 학생 이름). 매 업데이트라 교체 시 다음 프레임에 반영.
        let pane_char = ws.pane_character.get(&update.pane_id).cloned();
        // Route the update to the *tab* whose pid matches this stream.
        // Single-tab panes round-trip through the outer id; secondary
        // tabs spawned via the in-pane + button route through
        // `pid_to_pane`. Falls back to creating an outer pane entry
        // when the first update from a freshly-spawned shell arrives.
        // Mirrors retain source rows/columns, including TUI mouse coordinates.
        // Display-only scaling is shared by rendering, hit testing and IME.
        // 같은 번호의 칸이 있는데 이 PTY 를 안 든다 — 번호만 겹친 남의 칸이다. 그 칸의 첫 탭을
        // 빼앗으면 남의 화면이 이 PTY 로 덮인다. 이 화면은 보일 자리가 없으니 버린다.
        if ws.panes.contains_key(&update.pane_id) && ws.find_tab_by_pty(&update.pane_id).is_none() {
            return;
        }
        let (pane, tab_idx) = match ws.find_tab_by_pty(&update.pane_id) {
            Some(p) => p,
            None => {
                // Brand-new pty id → create the outer PaneState with a
                // single tab that owns this pid. Seed pid_to_pane so
                // subsequent updates hit the O(1) path.
                let pane = ws.pane_mut(&update.pane_id);
                pane.tabs[0].pid = Some(update.pane_id.clone());
                ws.pid_to_pane
                    .insert(update.pane_id.clone(), update.pane_id.clone());
                let pane = ws.panes.get_mut(&update.pane_id).expect("just inserted");
                (pane, 0usize)
            }
        };
        pane.character = pane_char;
        let tab = &mut pane.tabs[tab_idx];
        // Layout/resize can create the outer pane before its first frame.
        // find_tab_by_pty then finds that unbound primary tab, bypassing the
        // new-pane branch above. Bind it here too: restore readiness and tab
        // routing must see the PTY whose live output is already on screen.
        if tab.pid.is_none() && tab.term().is_some() {
            tab.pid = Some(update.pane_id.clone());
        }
        // pid 라우팅이 터미널 아닌 탭(이미지/md 미리보기)에 떨어질 수 있다 — 여기서
        // expect 로 죽으면 호출자가 ws 락을 쥔 채 unwind 해 poison 이 GUI 전체로
        // 번진다. 프레임 하나를 버리는 쪽이 맞다.
        let Some(tp) = tab.term_mut() else { return };
        tp.live_output |= update.live_output;
        if update.live_output { tp.output_generation = update.output_generation; }
        let resized = tp.cols != update.cols
            || tp.rows != update.rows
            || tp.cells.len() != update.rows as usize;
        if resized {
            // Preserve existing rows / columns through a resize so
            // the user sees their old content during the brief gap
            // between SIGWINCH and the shell's reflowed repaint —
            // otherwise the grid blanks for one frame and the
            // divider drag flickers visibly on every cell crossing.
            // Truncate / extend in place; the shell's subsequent
            // `update.dirty` overwrites the affected rows.
            tp.cols = update.cols;
            tp.rows = update.rows;
            let nr = update.rows as usize;
            let nc = update.cols as usize;
            tp.cells.truncate(nr);
            while tp.cells.len() < nr {
                tp.cells.push(vec![GridCell::blank(); nc]);
            }
            for row in &mut tp.cells {
                row.truncate(nc);
                while row.len() < nc {
                    row.push(GridCell::blank());
                }
            }
            tp.prev_cells.clear();
        }
        for (r, row) in update.dirty {
            if let Some(dst) = tp.cells.get_mut(r as usize) {
                *dst = row;
            }
        }
        // Shift detection on the pty side is retired — alacritty handles
        // scrollback natively via display_offset. Hand-rolled detection
        // breaks scroll-region TUIs (like Claude Code) when they write to sync.
        tp.cursor_row = update.cursor_row;
        tp.cursor_col = update.cursor_col;
        tp.cursor_visible = update.cursor_visible;
        tp.alt_screen = update.alt_screen;
        tp.inline_images = update.inline_images;
        tp.mouse_enabled = update.mouse_enabled;
        tp.mouse_sgr = update.mouse_sgr;
        tp.mouse_motion = update.mouse_motion;
        tp.app_cursor = update.app_cursor;
        tp.bracketed_paste = update.bracketed_paste;
        // Carry the OSC 133 prompt-end mark only on frames that
        // actually emitted one; keep the last otherwise so a
        // mid-typing frame doesn't erase it.
        if let Some(pe) = update.prompt_end {
            tp.prompt_end = Some(pe);
        }
        // OSC 0/2 title from the inner program (Claude Code's
        // conversation summary, vim filename, etc.). Pinned panes
        // (renamed via surface.rename / run_job) keep their agent-set
        // label; only unpinned panes track OSC.
        if let Some(t) = update.title.clone() {
            if !tab.title_pinned {
                tab.title = Some(t);
            }
        }
        let _ = tab;
        pane.dirty = true;
    }
    pub(crate) fn pump_pty_screens(
        &self,
        screens: kasa_pty::ScreenReceiver<kasa_bridge::screen::ScreenUpdate>,
        pane_id: String,
        sess_weak: std::sync::Weak<kasa_pty::PtySession>,
    ) {
        if let Some((pane, tab_idx)) = self.ws.lock().unwrap().find_tab_by_pty(&pane_id) {
            if let Some(term) = pane.tabs[tab_idx].term_mut() {
                term.live_output = false;
                term.output_generation = 0;
            }
        }
        let ws_screens = self.ws.clone();
        let dead = self.dead_panes.clone();
        let proxy = self.proxy.clone();
        let marker_backend = self.socket_backend.clone();
        // statusline 세션 id 마커(⟦sid8⟧)의 마지막 관측값 — 값이 바뀔 때만 rebind 를
        // 태워 같은 마커의 재렌더(매 프레임)를 무시한다.
        let mut last_marker: Option<String> = None;
        std::thread::spawn(move || {
            // No throttle here: a *single* ScreenUpdate (one echoed space)
            // that landed inside a 16ms window used to be dropped, giving a
            // ~1s cursor lag after spacebar. Every update wakes the GUI.
            //
            // The wake is the proxy event only — never `Window::request_redraw`
            // from this thread. On macOS winit hops that call onto the main
            // thread with a *synchronous* dispatch, so whenever the GUI thread
            // stalls this pump blocks behind it, stops draining the PTY, and
            // the agent in the pane freezes on its next write. 2026-10-06 on
            // the mini: the main thread sat in a realloc for 25+ minutes and
            // every student in that app stopped with it. `UserEvent::Redraw`
            // already paints inline (`render_frame` at the end of
            // `user_event`), so the direct call only added the blocking hop.
            while let Ok(mut update) = screens.recv() {
                // EOF sentinel: the PTY reader died (shell/claude exited).
                // The PtySession keeps a Sender alive for scroll/resize, so
                // the channel never closes on its own — without this signal
                // the pane would linger as a zombie. Flag it dead and wake
                // the loop so reap_dead_panes drops it on the next turn.
                if update.eof {
                    // 같은 id 로 PTY 가 갈아끼워졌으면(캐릭터 교체·계정 재시작·
                    // 핸드오프 승격) 이 죽음표시는 **옛 세션**의 것이다 — 새 pane 을
                    // 걷으면 안 된다. dead_panes.retain 정리는 GUI 스레드와
                    // 마이크로초 경주가 남지만, 레지스트리의 현 세션과 내 정체를
                    // 포인터로 비교하면 결정적이다(pane_replaced).
                    if pane_replaced(&update.pane_id, &sess_weak) {
                        return;
                    }
                    dead.lock().unwrap().push(update.pane_id.clone());
                    let _ = proxy.send_event(UserEvent::Redraw);
                    return;
                }
                // OSC 777 desktop notification — drain before the coalesce
                // merge below rebuilds `update` and would drop it. 예약 title
                // (`book`)이면 알림이 아니라 「로컬로 되돌리기」 신호다.
                if let Some((title, body)) = update.notify.take() {
                    if title == crate::BRING_HOME_MARKER {
                        let _ = proxy
                            .send_event(UserEvent::SocketBringHome(update.pane_id.clone()));
                    } else {
                        let _ = proxy.send_event(UserEvent::Notify {
                            surface_id: update.pane_id.clone(),
                            title,
                            body,
                        });
                    }
                }
                // Coalesce: drain every other ScreenUpdate currently sitting
                // in the channel and merge them into one. Scroll inertia /
                // bursty Claude Code output can stuff hundreds of frames in
                // the queue between render cycles; processing each
                // separately means N ws-locks + N redraws + N renders. With
                // the merge we do ONE lock per burst, so direction reversals
                // and other late inputs aren't stuck behind a queue.
                loop {
                    match screens.try_recv() {
                        Ok(mut next) if !next.eof => {
                            // OSC 777 from a coalesced frame — fire before the
                            // merge below drops `next.notify`.
                            if let Some((title, body)) = next.notify.take() {
                                if title == crate::BRING_HOME_MARKER {
                                    let _ = proxy.send_event(
                                        UserEvent::SocketBringHome(next.pane_id.clone()),
                                    );
                                } else {
                                    let _ = proxy.send_event(UserEvent::Notify {
                                        surface_id: next.pane_id.clone(),
                                        title,
                                        body,
                                    });
                                }
                            }
                            update = coalesce_screen_updates(update, next);
                        }
                        Ok(next) => {
                            // EOF mid-burst: 같은 자리에 새 세션이 앉았으면(스왑)
                            // 이 죽음도, 병합해 둔 옛 프레임도 전부 옛 세션 것이다 —
                            // 그리지도 죽음표시도 않고 조용히 끝낸다. 이 자리만
                            // 가드가 없던 탓에, 이사(migrate)가 옛 셸을 걷는 순간
                            // 여기로 배수된 EOF 가 reap → remove_pane → kill_remote
                            // 연쇄로 **새 원격 pane 을** 걷고 마지막 pane 이면 앱까지
                            // 데려갔다(2026-08-27 백트레이스 실측).
                            if pane_replaced(&next.pane_id, &sess_weak) {
                                return;
                            }
                            dead.lock().unwrap().push(next.pane_id.clone());
                            break;
                        }
                        Err(_) => break,
                    }
                }
                // 같은 pane id에 로컬/새 원격 세션이 이미 앉았으면, 여기까지
                // 도착한 갱신은 갈아끼우기 전 세션의 마지막 프레임이다. EOF에만
                // 세대 가드를 두면 `to ..` 직후 새 로컬 화면을 옛 원격 한 프레임이
                // 다시 덮는다.
                if pane_replaced(&update.pane_id, &sess_weak) {
                    // 칸 화면이 얼어붙으면 여기부터 본다 — 이 일꾼이 끝나면 그 PTY 의 화면은 더 안 온다.
                    eprintln!("[pump] {} 에 다른 세션이 앉아 화면 받기를 멈춤", update.pane_id);
                    return;
                }
                // 세션 진입 즉시 감지(사용자): dirty 행에 statusline 세션 id 마커가 있으면
                // 그 자리에서 rebind — 3s 폴러를 기다리지 않는다. 마커는 세션 화면의
                // 일부라 agents 피커로 진입한 첫 리드로우에 반드시 실려 온다. '⟦' 스캔은
                // 문자 비교뿐이라 스트리밍 버스트에도 공짜에 가깝다. rebind 는 apply
                // "후"에 태운다 — 그리드를 다시 읽으므로 마커가 반영된 뒤여야 한다.
                let marker = update.dirty.iter().rev().find_map(|(_, row)| {
                    row.iter().any(|c| c.ch == '⟦').then(|| {
                        let s: String = row.iter().map(|c| c.ch).collect();
                        crate::socket::screen_marker_sid8(&s)
                    })?
                });
                // claude 폴더 신뢰 화면 후보 — 판정과 입력은 한글 조합 상태를 아는 GUI 스레드 몫(trust_prompt.rs).
                let trust_hint = update.dirty.iter().any(|(_, row)| crate::trust_prompt::row_mentions_trust(row));
                let pane_for_marker = update.pane_id.clone();
                let mut ws = ws_screens.lock().unwrap();
                Self::apply_screen_update(&mut ws, update);
                drop(ws);
                if trust_hint {
                    crate::trust_prompt::note(&pane_for_marker);
                }
                // Also wakes a loop parked on WaitUntil, which request_redraw
                // didn't do reliably on macOS.
                let _ = proxy.send_event(UserEvent::Redraw);
                if let (Some(m), Some(be)) = (marker, marker_backend.as_ref()) {
                    if last_marker.as_deref() != Some(m.as_str()) {
                        be.rebind_pane_marker(&pane_for_marker, &m);
                        last_marker = Some(m);
                    }
                }
            }
            // Channel disconnected — the reader thread exited because
            // the PTY hit EOF (shell quit) or errored. Flag this pane
            // for the main thread to remove on its next tick.
            // eof 와 같은 정체 가드 — 채널 단절은 승격 스왑의 Arc drop 순간에
            // 정확히 나므로 여기가 더 밟기 쉽다.
            if pane_replaced(&pane_id, &sess_weak) {
                return;
            }
            dead.lock().unwrap().push(pane_id);
            let _ = proxy.send_event(UserEvent::Redraw);
        });
    }
}

/// 이 pane id 가 **다른 세션으로 갈아끼워졌는가** — pump 스레드의 죽음표시 가드.
///
/// 레지스트리의 현 세션과 pump 자신의 세션(Weak)을 포인터로 비교한다. 같은
/// 세션이 그냥 죽은 것이면 App.pty 가 아직 Arc 를 쥐고 있어 lookup 이 그 세션
/// 자신을 돌려주므로(포인터 일치) 정상 reap 은 막히지 않는다.
pub(super) fn pane_replaced(id: &str, mine: &std::sync::Weak<kasa_pty::PtySession>) -> bool {
    match kasa_pty::lookup_session(id) {
        Some(cur) => !std::ptr::eq(std::sync::Arc::as_ptr(&cur), mine.as_ptr()),
        None => false,
    }
}

#[cfg(all(test, unix))]
#[test]
fn replacement_snapshot_requires_registration_before_pump() {
    use kasa_pty::{PtyOptions, PtySession};
    use std::sync::Arc;

    let id = format!("test-replacement-pump-{}", std::process::id());
    let make_session = || {
        // cat is a standalone test PTY, never a user's shell/profile/session.
        Arc::new(PtySession::start(PtyOptions {
            shell: Some("/bin/cat".into()),
            pane_id: id.clone(),
            cols: 80,
            rows: 24,
            ..Default::default()
        }).unwrap())
    };
    let old = make_session();
    let new = make_session();
    kasa_pty::register_session(&id, &old);
    // A successful remote handshake can queue its snapshot before the GUI
    // swaps sessions. Starting the new pump now would reject that snapshot.
    new.send_bytes(b"NEW HOST\r").unwrap();
    let snapshot = new.screens.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
    assert_eq!(snapshot.pane_id, id);
    assert!(pane_replaced(&id, &Arc::downgrade(&new)));
    assert!(!pane_replaced(&id, &Arc::downgrade(&old)));

    kasa_pty::register_session(&id, &new);
    assert!(!pane_replaced(&snapshot.pane_id, &Arc::downgrade(&new)));
    // The old Arc stays alive through the swap; its late frames/EOF must
    // still be rejected while the new snapshot is accepted.
    assert!(pane_replaced(&id, &Arc::downgrade(&old)));
}

#[cfg(test)]
mod tests {

    fn readiness_frame(live_output: bool, output_generation: u64, cols: u16, row: u16)
        -> kasa_bridge::screen::ScreenUpdate
    {
        kasa_bridge::screen::ScreenUpdate {
            live_output, output_generation, cols, rows: 3,
            dirty: vec![(row, vec![crate::GridCell::blank(); cols as usize])],
            ..Default::default()
        }
    }

    #[test]
    fn coalesce_readiness_survives_resize_without_retaining_old_size_rows() {
        let merged = super::coalesce_screen_updates(
            readiness_frame(true, 7, 20, 0), readiness_frame(false, 0, 30, 1));
        assert!(merged.live_output);
        assert_eq!(merged.output_generation, 7);
        assert_eq!(merged.cols, 30);
        assert_eq!(merged.dirty.len(), 1);
        assert_eq!(merged.dirty[0].0, 1);
    }

    #[test]
    fn coalesce_readiness_does_not_cross_an_explicit_generation_boundary() {
        for (live, generation) in [(false, 8), (true, 8), (true, 0)] {
            let merged = super::coalesce_screen_updates(
                readiness_frame(true, 7, 20, 0), readiness_frame(live, generation, 20, 1));
            assert_eq!((merged.live_output, merged.output_generation), (live, generation));
            assert_eq!(merged.dirty.len(), 2, "parsed earlier fragments remain part of the canonical grid");
            let resized = super::coalesce_screen_updates(merged, readiness_frame(false, 0, 30, 2));
            assert_eq!((resized.live_output, resized.output_generation), (live, generation));
        }
    }

    #[test]
    fn coalesce_readiness_merges_same_generation_and_local_zero_generation() {
        for generation in [0, 7] {
            let merged = super::coalesce_screen_updates(
                readiness_frame(true, generation, 20, 0), readiness_frame(false, generation, 20, 1));
            assert!(merged.live_output);
            assert_eq!(merged.output_generation, generation);
            assert_eq!(merged.dirty.len(), 2);
        }
    }

    #[test]
    fn coalesce_readiness_with_actual_external_parser_waits_for_new_bytes() {
        use std::sync::Arc;
        use std::time::Duration;
        let (events, incoming) = crossbeam_channel::unbounded();
        let session = kasa_pty::PtySession::start_external(kasa_pty::PtyOptions {
            cols: 20, rows: 3, pane_id: format!("coalesce-readiness-{}", uuid::Uuid::new_v4()),
            ..Default::default()
        }, kasa_pty::ExternalIo {
            events: incoming, writer: Box::new(std::io::sink()), on_resize: Arc::new(|_, _| {}),
        }).unwrap();
        events.send(kasa_pty::ExtEvent::Generation(7)).unwrap();
        events.send(kasa_pty::ExtEvent::Bytes(b"live".to_vec())).unwrap();
        let live = session.screens.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!((live.live_output, live.output_generation), (true, 7));
        session.resize(30, 3).unwrap();
        let resize = session.screens.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!((resize.live_output, resize.output_generation), (false, 0));
        let merged = super::coalesce_screen_updates(live, resize);
        assert_eq!((merged.live_output, merged.output_generation), (true, 7));
        events.send(kasa_pty::ExtEvent::Generation(8)).unwrap();
        // A Generation notification alone does not certify a parsed snapshot.
        let snapshot = super::coalesce_screen_updates(merged, session.full_snapshot());
        assert_eq!(snapshot.output_generation, 7);
        assert_ne!(snapshot.output_generation, 8);
        events.send(kasa_pty::ExtEvent::Bytes(b"new".to_vec())).unwrap();
        let fresh = session.screens.recv_timeout(Duration::from_secs(2)).unwrap();
        let merged = super::coalesce_screen_updates(snapshot, fresh);
        assert_eq!((merged.live_output, merged.output_generation), (true, 8));
        events.send(kasa_pty::ExtEvent::Eof).unwrap();
    }

    #[test]
    fn coalesce_readiness_keeps_earlier_rows_of_a_fragmented_external_frame() {
        use std::sync::Arc;
        use std::time::Duration;
        let (events, incoming) = crossbeam_channel::unbounded();
        let id = format!("coalesce-fragments-{}", uuid::Uuid::new_v4());
        let session = kasa_pty::PtySession::start_external(kasa_pty::PtyOptions {
            cols: 20, rows: 3, pane_id: id.clone(), ..Default::default()
        }, kasa_pty::ExternalIo {
            events: incoming, writer: Box::new(std::io::sink()), on_resize: Arc::new(|_, _| {}),
        }).unwrap();
        let mut bytes = b"FIRST\r\n".to_vec();
        // Cross the external reader's 64 KiB fragment boundary without dirtying
        // FIRST again. Only the final fragment writes SECOND and certifies gen 9.
        for _ in 0..20_000 { bytes.extend_from_slice(b"\x1b[0m"); }
        bytes.extend_from_slice(b"SECOND");
        events.send(kasa_pty::ExtEvent::Generation(9)).unwrap();
        events.send(kasa_pty::ExtEvent::Bytes(bytes)).unwrap();
        let mut merged = session.screens.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_ne!(merged.output_generation, 9);
        while merged.output_generation != 9 {
            let next = session.screens.recv_timeout(Duration::from_secs(2)).unwrap();
            merged = super::coalesce_screen_updates(merged, next);
        }
        let mut ws = crate::Workspace::default();
        super::App::apply_screen_update(&mut ws, merged);
        let actual = ws.panes[&id].tabs[0].term().unwrap();
        assert!(actual.live_output);
        assert_eq!(actual.output_generation, 9);
        for (row, cells) in session.full_snapshot().dirty {
            assert_eq!(actual.cells[row as usize], cells, "coalescing lost an earlier fragment's row");
        }
        events.send(kasa_pty::ExtEvent::Eof).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn mirror_viewport_keeps_host_grid_for_display_only_scaling() {
        use std::sync::Arc;
        let id = format!("mirror-origin-{}", uuid::Uuid::new_v4());
        let local = format!("mirror-local-{}", uuid::Uuid::new_v4());
        let source = Arc::new(kasa_pty::PtySession::start(kasa_pty::PtyOptions {
            pane_id: id.clone(), shell: Some("/bin/sh".into()),
            cols: 60, rows: 12, ..Default::default()
        }).unwrap());
        kasa_pty::register_session(&id, &source);
        let backend: Arc<dyn kasa_socket::backend::Backend> = Arc::new(
            kasa_mcp::standalone::StandaloneBackend::new(std::env::temp_dir()),
        );
        let port = kasa_mcp::spawn_http_server_opts(backend, 0, false).unwrap();
        let _mirror = kasa_mcp::remote::connect_view(kasa_mcp::remote::RemoteSpec {
            base: format!("http://127.0.0.1:{port}"), pane: Some(id),
            cwd: None, token: None, identity: Default::default(),
        }, &local).unwrap();
        assert!(kasa_mcp::remote::is_view_pane(&local));

        let mut ws = crate::Workspace::default();
        assert!(kasa_mcp::remote::set_viewport(&local, 30, 8));
        let mut snapshot = source.full_snapshot();
        snapshot.pane_id = local.clone();
        super::App::apply_screen_update(&mut ws, snapshot.clone());
        let displayed = ws.panes[&local].tabs[0].term().unwrap();
        assert_eq!((displayed.cols, displayed.rows), (60, 12));

        // Changing viewer geometry never changes the TUI grid or its input coordinates.
        assert!(kasa_mcp::remote::set_viewport(&local, 60, 6));
        let displayed = ws.panes[&local].tabs[0].term().unwrap();
        assert_eq!((displayed.cols, displayed.rows), (60, 12));

        // 다른 뷰어가 크기를 가져가거나 재접속해 원본 크기가 다시 와도 유지한다.
        super::App::apply_screen_update(&mut ws, snapshot);
        let displayed = ws.panes[&local].tabs[0].term().unwrap();
        assert_eq!((displayed.cols, displayed.rows), (60, 12));
        assert_eq!(source.size(), (60, 12));
    }
}
