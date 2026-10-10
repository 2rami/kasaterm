//! 칸 화면을 읽는 쪽의 API — 스크롤·보이는 글·스크롤백·스냅샷·구독(tap).
//! 스냅샷 채취와 구독 등록은 `term` 락 하나 안에서 끝난다(유실·중복 방지).

use super::*;
use super::grid::{
    build_update, history_ansi, live_snapshot, raw_screen_ansi, read_live_tail, read_row_at_abs,
    read_rows_above, read_rows_above_live, scan_prompt_anchors, snapshot,
};
use super::inline::{attach_inline_views, attach_inline_views_at_offset};
#[cfg(test)]
use super::test_support::{ext_session, test_posix_shell, wait_text};

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

/// One parser generation for a viewer's screen, history and pinned live input.
/// Reading these independently can join old GUI rows to a new scroll offset.
pub struct ViewerSnapshot {
    pub screen: ScreenUpdate,
    pub live: Vec<Row>,
    pub above: Vec<Row>,
    pub display_offset: usize,
    pub history_size: usize,
}

impl PtySession {
    /// 지금 대체 화면(vim·less·htop)인가. 스냅샷을 만들지 않고 모드만 본다.
    pub fn alt_screen(&self) -> bool {
        self.term.lock().unwrap().mode().contains(alacritty_terminal::term::TermMode::ALT_SCREEN)
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
                let cell = &grid[point];
                // wide 글리프의 뒤칸은 `' '` 에 플래그만 다르다 — 넣으면 한글마다 한 칸씩 벌어진다.
                if cell.flags.contains(alacritty_terminal::term::cell::Flags::WIDE_CHAR_SPACER) {
                    continue;
                }
                row.push(if cell.c == '\0' { ' ' } else { cell.c });
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

    /// 살아 있는 화면 위의 스크롤백 — 가까운 순, 스크롤 위치 무시(`read_rows_above_live`).
    pub fn rows_above_live(&self, n: usize) -> Vec<Row> {
        read_rows_above_live(&self.term.lock().unwrap(), n)
    }

    /// 절대 줄 하나를 **셀 그대로** 읽는다 — 색·굵기·마커까지 원본 그대로.
    ///
    /// 고정 머리줄이 이걸 쓴다. 머리줄을 글자만 다시 그리면 원본과 색도 마커도
    /// 어긋나서, 눌러서 그 자리로 갔을 때 같은 줄인데 다르게 보인다. 셀을 옮겨
    /// 그리면 머리줄과 본문이 **딱 겹친다**(2026-09-03 지시).
    pub fn row_at_abs(&self, abs: i64) -> Option<Row> {
        read_row_at_abs(&self.term.lock().unwrap(), abs)
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

    /// 거울에 줄 살아 있는 화면 — 데스크톱의 스크롤 위치를 무시한다(`live_snapshot`).
    pub fn live_screen(&self) -> ScreenUpdate {
        let (cols, rows) = *self.size.lock().unwrap();
        let t = self.term.lock().unwrap();
        let mut update = live_snapshot(&t, cols, rows, &self.pane_id, &self.title_handle);
        attach_inline_views_at_offset(&mut update, &t, &self.inline_imgs, 0);
        update
    }

    pub fn publish_full_snapshot(&self) {
        self.publish_screen(self.full_snapshot());
    }

    pub fn viewer_snapshot(&self, viewer_cols: usize, viewer_rows: usize) -> ViewerSnapshot {
        let t = self.term.lock().unwrap();
        let (cols, rows) = (t.grid().columns() as u16, t.grid().screen_lines() as u16);
        let display_offset = t.grid().display_offset();
        let budget = viewer_rows.saturating_mul(viewer_cols.div_ceil(usize::from(cols).max(2))).min(4096);
        ViewerSnapshot {
            screen: build_update(&t, cols, rows, &self.pane_id, &self.title_handle,
                &(0..rows).collect::<Vec<_>>(), display_offset as i32),
            live: read_live_tail(&t, rows as usize),
            above: read_rows_above(&t, budget),
            display_offset,
            history_size: t.grid().history_size(),
        }
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
        let (rx, bytes, _) = self.tap_bytes_with_sized_snapshot();
        (rx, bytes)
    }

    /// The dimensions belong to these exact snapshot bytes, not a later
    /// `size()` read. A resize between capture and WS handshake otherwise
    /// replays narrow rows at a wide margin, destroying their soft-wrap flags.
    pub fn tap_bytes_with_sized_snapshot(&self) -> (Receiver<Vec<u8>>, Vec<u8>, (u16, u16)) {
        let t = self.term.lock().unwrap();
        let (bytes, size) = self.sized_snapshot_locked(&t);
        // 탭 등록은 스냅샷과 같은 락 안이어야 한다 — 둘 사이에 들어온 프레임은
        // 스냅샷에도 탭에도 없이 사라진다.
        let (tx, rx) = crossbeam_channel::bounded(64);
        self.byte_taps.lock().unwrap().push(tx);
        (rx, bytes, size)
    }

    /// 지금 화면(스크롤백 포함)을 ANSI 바이트로 — 탭을 새로 열지 않는다. 이미 붙어
    /// 있는 raw 구독자에게 격자 변경 뒤 화면을 다시 세워 줄 때 쓴다(재접속 대신).
    pub fn sized_snapshot_bytes(&self) -> (Vec<u8>, (u16, u16)) {
        let t = self.term.lock().unwrap();
        self.sized_snapshot_locked(&t)
    }

    fn sized_snapshot_locked(&self, t: &Term<PtyEventForwarder>) -> (Vec<u8>, (u16, u16)) {
        // resize_effective reshapes the parser before publishing self.size.
        // The parser is canonical while holding its lock.
        let (cols, rows) = (t.grid().columns() as u16, t.grid().screen_lines() as u16);
        let hist = history_ansi(t, cols, rows);
        // A subscriber starts at the live screen, independently of where the
        // source GUI is reading. Its damage belongs to that GUI, not this tap.
        let snap = live_snapshot(t, cols, rows, &self.pane_id, &self.title_handle);
        // 스크롤백은 primary 화면의 것이다 — alt 화면(vim 등)에 붙는 미러에
        // 실으면 ?1049h 앞에 찍혀 primary 를 더럽힌다.
        let mut bytes = if snap.alt_screen { Vec::new() } else { hist };
        bytes.extend_from_slice(&raw_screen_ansi(&snap));
        (bytes, (cols, rows))
    }
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
pub(super) fn publish_screen_update(
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

/// 살아 있는 PTY 로 스냅샷 재생을 검증한다. 순수 변환(`to_ansi`) 쪽 테스트는
/// kasa-screen 에 있고, 여기서는 실제 셀 그리드에서 제대로 떠지는지와
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

#[cfg(test)]
mod external_screen_tests {
    use super::*;

    #[test]
    fn visible_text_keeps_wide_glyphs_together() {
        let (session, events, _, _) = ext_session(30, 4);
        events.send(ExtEvent::Generation(1)).unwrap();
        events.send(ExtEvent::Bytes("Reason: 선생님, 작업 a".as_bytes().to_vec())).unwrap();
        session.screens.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert!(session.visible_text(4).contains("Reason: 선생님, 작업 a"), "{:?}", session.visible_text(4));
    }

    #[test]
    fn live_screen_ignores_display_offset() {
        // 데스크톱이 스크롤백을 올려다보는 동안에도 거울은 바닥 화면을 받아야 한다.
        let (sess, etx, _wrx, _resized) = ext_session(20, 5);
        let mut long = Vec::new();
        for i in 0..30 {
            long.extend_from_slice(format!("line{i}\r\n").as_bytes());
        }
        etx.send(ExtEvent::Bytes(long)).unwrap();
        assert!(wait_text(&sess, "line29"));
        assert!(sess.scroll(3) > 0, "위로 올라가 있어야 전제 성립");
        let text = |u: &ScreenUpdate| -> Vec<String> {
            u.dirty
                .iter()
                .map(|(_, row)| row.iter().map(|c| c.ch).collect::<String>().trim_end().to_string())
                .collect()
        };
        let scrolled = sess.full_snapshot();
        assert!(!text(&scrolled).iter().any(|l| l == "line29"), "GUI 스냅샷은 올려다본 창");
        assert!(!scrolled.cursor_visible);
        let live = sess.live_screen();
        assert!(text(&live).iter().any(|l| l == "line29"), "거울 스냅샷은 바닥 화면");
        assert!(live.cursor_visible);
        assert_eq!(live.dirty.len(), 5, "전체 행을 담는다");
        assert_eq!(sess.view_state().0, 3, "거울 스냅샷이 스크롤 위치를 건드리지 않는다");
    }

    #[test]
    fn byte_snapshot_uses_live_grid_without_consuming_source_scroll_or_damage() {
        use alacritty_terminal::index::{Column, Line};
        let (source, _source_events, _source_writer, _source_resize) = ext_session(5, 3);
        {
            let mut term = source.term.lock().unwrap();
            let mut parser: Processor<StdSyncHandler> = Processor::new();
            for i in 0..10 { parser.advance(&mut *term, format!("H{i}\r\n").as_bytes()); }
            parser.advance(&mut *term, "\x1b[2J\x1b[Habcd한\r\nLIVE".as_bytes());
            term.scroll_display(alacritty_terminal::grid::Scroll::Delta(3));
            assert_eq!(term.grid().display_offset(), 3);
            assert!(matches!(term.damage(), TermDamage::Full));
            assert!(term.grid()[Line(0)][Column(4)].flags.contains(
                alacritty_terminal::term::cell::Flags::LEADING_WIDE_CHAR_SPACER));
        }
        let (_tap, bytes) = source.tap_bytes_with_snapshot();
        let (replay, _replay_events, _replay_writer, _replay_resize) = ext_session(5, 3);
        let mut source_term = source.term.lock().unwrap();
        assert_eq!(source_term.grid().display_offset(), 3, "tap moved the source viewport");
        assert!(matches!(source_term.damage(), TermDamage::Full), "tap consumed source GUI damage");
        let mut replay_term = replay.term.lock().unwrap();
        let mut parser: Processor<StdSyncHandler> = Processor::new();
        parser.advance(&mut *replay_term, &bytes);
        assert_eq!(replay_term.grid().display_offset(), 0);
        assert_eq!(replay_term.grid().history_size(), source_term.grid().history_size());
        assert_eq!(replay_term.grid().cursor, source_term.grid().cursor);
        for row in -(source_term.grid().history_size() as i32)..3 {
            for col in 0..5 {
                let point = Point::new(Line(row), Column(col));
                assert_eq!(replay_term.grid()[point], source_term.grid()[point],
                    "replayed live/history cell changed at {row}:{col}");
            }
        }
    }

    #[test]
    fn byte_snapshot_dimensions_stay_bound_to_captured_history_across_resize() {
        let (source, _events, _writer, _resize) = ext_session(8, 4);
        {
            let mut term = source.term.lock().unwrap();
            let mut parser: Processor<StdSyncHandler> = Processor::new();
            parser.advance(&mut *term, b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefgh\r\nLIVE");
        }
        let history = source.rows_above_live(100);
        assert!(history.iter().any(|row| row.last().is_some_and(|cell| cell.wrapped)));
        let (_tap, bytes, captured_size) = source.tap_bytes_with_sized_snapshot();
        source.resize(93, 4).unwrap();
        assert_eq!(captured_size, (8, 4));
        assert_ne!(captured_size, source.size(), "later dimensions do not describe the captured ANSI");
        let (replay, _events, _writer, _resize) = ext_session(captured_size.0, captured_size.1);
        {
            let mut term = replay.term.lock().unwrap();
            let mut parser: Processor<StdSyncHandler> = Processor::new();
            parser.advance(&mut *term, &bytes);
        }
        assert_eq!(replay.rows_above_live(100), history, "soft-wrap flags or source history were lost");

        // The old HTTP handshake used this later width: replaying the exact
        // same bytes at it loses their right-margin soft-wrap semantics.
        let (wrong, _events, _writer, _resize) = ext_session(93, 4);
        {
            let mut term = wrong.term.lock().unwrap();
            let mut parser: Processor<StdSyncHandler> = Processor::new();
            parser.advance(&mut *term, &bytes);
        }
        assert_ne!(wrong.rows_above_live(100), history);
    }

    #[test]
    fn byte_snapshot_reads_locked_parser_size_during_resize_publication_gap() {
        let (source, _events, _writer, _resize) = ext_session(8, 4);
        // resize_effective updates the parser and public size in two steps.
        // Simulate a stale size slot without involving a real user's PTY.
        *source.size.lock().unwrap() = (26, 9);
        let (_tap, _bytes, size) = source.tap_bytes_with_sized_snapshot();
        assert_eq!(size, (8, 4));
    }
}
