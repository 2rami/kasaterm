//! 격자 크기의 주인 — GUI 레이아웃 크기와 뷰어(웹·거울) 임대 중 마지막으로 만진 쪽을 따른다.
//! 커널 PTY(ioctl·원격 콜백)·VT 격자·`size` 를 그 순서로 맞춘다.

use super::*;
#[cfg(test)]
use super::test_support::{ext_session, wait_text};

#[derive(Clone, Debug)]
struct ViewerSizeLease {
    token: u64,
    size: Option<(u16, u16)>,
}

#[derive(Clone, Debug)]
pub(super) struct ViewportSizes {
    gui: (u16, u16),
    next_token: u64,
    viewers: Vec<ViewerSizeLease>,
}

impl ViewportSizes {
    pub(super) fn new(cols: u16, rows: u16) -> Self {
        Self { gui: (cols, rows), next_token: 1, viewers: Vec::new() }
    }

    fn open(&mut self) -> u64 {
        let token = self.next_token;
        self.next_token = token.checked_add(1).expect("viewer token exhausted");
        self.viewers.push(ViewerSizeLease { token, size: None });
        token
    }

    fn owner(&self) -> Option<u64> {
        self.viewers.iter().rev().find(|v| v.size.is_some()).map(|v| v.token)
    }

    fn effective(&self) -> (u16, u16) {
        self.viewers.iter().rev().find_map(|v| v.size).unwrap_or(self.gui)
    }

    fn update(&mut self, token: u64, size: (u16, u16), acquire: bool) -> bool {
        let Some(i) = self.viewers.iter().position(|v| v.token == token) else {
            return false;
        };
        if acquire {
            let mut viewer = self.viewers.remove(i);
            viewer.size = Some(size);
            self.viewers.push(viewer);
        } else if self.viewers[i].size.is_some() {
            self.viewers[i].size = Some(size);
        }
        self.owner() == Some(token)
    }

    fn release(&mut self, token: u64) {
        if let Some(viewer) = self.viewers.iter_mut().find(|v| v.token == token) {
            viewer.size = None;
        }
    }

    fn close(&mut self, token: u64) {
        self.viewers.retain(|v| v.token != token);
    }

    /// The source GUI is the latest toucher: every viewer lease yields, including
    /// older non-owners, so a later release cannot hand the grid back to one.
    fn reclaim(&mut self) -> bool {
        let held = self.owner().is_some();
        for viewer in &mut self.viewers {
            viewer.size = None;
        }
        held
    }
}

/// 격자 크기에 화면 픽셀 크기를 곁들인다. 그림을 원본 크기로 놓는 앱(`kitten
/// icat`)은 `TIOCGWINSZ` 의 픽셀 값으로 칸 크기를 잰다 — 0 이면 그리기를 포기한다.
pub(super) fn pty_size(cols: u16, rows: u16) -> PtySize {
    let (cw, ch) = crate::kitty::cell_pixels().unwrap_or((0, 0));
    PtySize {
        rows,
        cols,
        pixel_width: (cols as u32 * cw).min(u16::MAX as u32) as u16,
        pixel_height: (rows as u32 * ch).min(u16::MAX as u32) as u16,
    }
}

impl PtySession {
    /// 현재 PTY 격자 크기 `(cols, rows)`. 미러로 붙는 쪽이 자기 화면을 여기에
    /// 맞춰야 줄바꿈이 어긋나지 않는다.
    pub fn size(&self) -> (u16, u16) {
        *self.size.lock().unwrap()
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        let mut sizes = self.viewport_sizes.lock().unwrap();
        sizes.gui = (cols.max(2), rows.max(1));
        let (cols, rows) = sizes.effective();
        self.resize_effective(cols, rows)
    }

    pub fn has_viewer_size_control(&self) -> bool {
        self.viewport_sizes.lock().unwrap().owner().is_some()
    }

    /// The viewer lease whose size the PTY currently has. A connection compares
    /// this with its own token to tell its viewer that a newer toucher won.
    pub fn viewer_size_owner(&self) -> Option<u64> {
        self.viewport_sizes.lock().unwrap().owner()
    }

    /// A person touched this pane on the source machine: return to its own
    /// layout size (tmux `window-size latest`). False when no viewer held it.
    pub fn reclaim_viewer_sizes(&self) -> Result<bool> {
        let mut sizes = self.viewport_sizes.lock().unwrap();
        if !sizes.reclaim() {
            return Ok(false);
        }
        let (cols, rows) = sizes.effective();
        self.resize_effective(cols, rows)?;
        Ok(true)
    }

    pub fn open_viewer_size(&self) -> u64 {
        self.viewport_sizes.lock().unwrap().open()
    }

    pub fn acquire_viewer_size(&self, token: u64, cols: u16, rows: u16) -> Result<bool> {
        self.update_viewer_size(token, cols, rows, true)
    }

    pub fn resize_viewer_size(&self, token: u64, cols: u16, rows: u16) -> Result<bool> {
        self.update_viewer_size(token, cols, rows, false)
    }

    fn update_viewer_size(&self, token: u64, cols: u16, rows: u16, acquire: bool) -> Result<bool> {
        let mut sizes = self.viewport_sizes.lock().unwrap();
        let previous = sizes.clone();
        let granted = sizes.update(token, (cols.max(2), rows.max(1)), acquire);
        if !granted {
            return Ok(false);
        }
        let (cols, rows) = sizes.effective();
        if let Err(err) = self.resize_effective(cols, rows) {
            *sizes = previous;
            return Err(err);
        }
        Ok(granted)
    }

    pub fn release_viewer_size(&self, token: u64) -> Result<()> {
        let mut sizes = self.viewport_sizes.lock().unwrap();
        let controlled = sizes.owner() == Some(token);
        sizes.release(token);
        if !controlled {
            return Ok(());
        }
        let (cols, rows) = sizes.effective();
        self.resize_effective(cols, rows)
    }

    pub fn close_viewer_size(&self, token: u64) -> Result<()> {
        let mut sizes = self.viewport_sizes.lock().unwrap();
        let controlled = sizes.owner() == Some(token);
        sizes.close(token);
        if !controlled {
            return Ok(());
        }
        let (cols, rows) = sizes.effective();
        self.resize_effective(cols, rows)
    }

    fn resize_effective(&self, cols: u16, rows: u16) -> Result<()> {
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
                pty.resize(pty_size(cols, rows))
                .context("pty resize")?;
            }
            SessionIo::External { on_resize } => (on_resize)(cols, rows),
            #[cfg(unix)]
            SessionIo::Adopted { fd, .. } => {
                use std::os::fd::AsRawFd;
                let px = pty_size(cols, rows);
                let ws = libc::winsize {
                    ws_row: rows,
                    ws_col: cols,
                    ws_xpixel: px.pixel_width,
                    ws_ypixel: px.pixel_height,
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

#[cfg(test)]
mod viewer_viewport_tests {
    use super::*;

    #[test]
    fn viewer_viewport_preserves_gui_size_and_publishes_the_entire_grid() {
        let (sess, etx, _wrx, resized) = ext_session(21, 6);
        let viewer = sess.open_viewer_size();
        assert!(sess.acquire_viewer_size(viewer, 120, 40).unwrap());
        sess.resize(21, 6).unwrap();
        assert_eq!(sess.size(), (120, 40));
        assert!(sess.has_viewer_size_control());
        assert_eq!(resized.lock().unwrap().as_slice(), &[(120, 40)]);

        etx.send(ExtEvent::Bytes(b"\x1b[?1049h\x1b[40;115HBOTTOM".to_vec())).unwrap();
        assert!(wait_text(&sess, "BOTTOM"));
        let (_, frame) = sess.tap_screens_with_snapshot();
        assert_eq!((frame.cols, frame.rows), (120, 40));
        assert_eq!(frame.dirty.len(), 40);

        sess.resize(30, 8).unwrap();
        sess.close_viewer_size(viewer).unwrap();
        assert_eq!(sess.size(), (30, 8), "restore the latest GUI layout, not the connection snapshot");
        assert!(!sess.has_viewer_size_control());
    }

    #[test]
    fn viewer_viewport_passive_resize_cannot_steal_and_active_close_restores_previous() {
        let (sess, _etx, _wrx, _resized) = ext_session(21, 6);
        let first = sess.open_viewer_size();
        let second = sess.open_viewer_size();
        assert!(!sess.resize_viewer_size(first, 100, 30).unwrap());
        assert_eq!(sess.size(), (21, 6), "resize before acquisition grants no control");
        assert!(sess.acquire_viewer_size(first, 100, 30).unwrap());
        assert!(sess.acquire_viewer_size(second, 140, 45).unwrap());
        assert!(!sess.resize_viewer_size(first, 110, 35).unwrap());
        assert_eq!(sess.size(), (140, 45));
        sess.close_viewer_size(second).unwrap();
        assert_eq!(sess.size(), (110, 35), "a waiting viewer retains its latest desired size");
        sess.close_viewer_size(first).unwrap();
        assert_eq!(sess.size(), (21, 6));
    }

    #[test]
    fn viewer_viewport_same_size_and_reverse_disconnect_do_not_restore_stale_owner() {
        let (sess, _etx, _wrx, resized) = ext_session(21, 6);
        let first = sess.open_viewer_size();
        let second = sess.open_viewer_size();
        assert!(sess.acquire_viewer_size(first, 100, 30).unwrap());
        assert!(sess.acquire_viewer_size(second, 100, 30).unwrap());
        assert_eq!(resized.lock().unwrap().as_slice(), &[(100, 30)]);
        sess.close_viewer_size(first).unwrap();
        assert_eq!(sess.size(), (100, 30));
        assert!(sess.has_viewer_size_control(), "equal dimensions are not ownership");
        sess.close_viewer_size(second).unwrap();
        assert_eq!(sess.size(), (21, 6));
    }

    #[test]
    fn viewer_viewport_release_can_reacquire_but_closed_tokens_stay_closed() {
        let (sess, _etx, _wrx, _resized) = ext_session(21, 6);
        let first = sess.open_viewer_size();
        let second = sess.open_viewer_size();
        assert!(sess.acquire_viewer_size(first, 100, 30).unwrap());
        assert!(sess.acquire_viewer_size(second, 120, 40).unwrap());
        assert!(sess.acquire_viewer_size(first, 110, 35).unwrap());
        sess.release_viewer_size(first).unwrap();
        assert_eq!(sess.size(), (120, 40));
        assert!(!sess.resize_viewer_size(first, 130, 45).unwrap());
        assert!(sess.acquire_viewer_size(first, 110, 35).unwrap());
        sess.close_viewer_size(first).unwrap();
        assert!(!sess.acquire_viewer_size(first, 200, 60).unwrap());
        assert!(!sess.resize_viewer_size(first, 200, 60).unwrap());
        sess.close_viewer_size(first).unwrap();
        assert_eq!(sess.size(), (120, 40));
        sess.close_viewer_size(second).unwrap();
        assert_eq!(sess.size(), (21, 6));
    }

    #[test]
    fn source_touch_reclaims_every_viewer_lease_until_a_viewer_touches_again() {
        let (sess, _etx, _wrx, _resized) = ext_session(21, 6);
        let first = sess.open_viewer_size();
        let second = sess.open_viewer_size();
        assert!(!sess.reclaim_viewer_sizes().unwrap(), "nothing held, nothing to reclaim");
        assert!(sess.acquire_viewer_size(first, 100, 30).unwrap());
        assert!(sess.acquire_viewer_size(second, 120, 40).unwrap());
        assert_eq!(sess.viewer_size_owner(), Some(second));
        sess.resize(30, 8).unwrap();
        assert!(sess.reclaim_viewer_sizes().unwrap());
        assert_eq!(sess.size(), (30, 8), "the source returns to its latest layout");
        assert_eq!(sess.viewer_size_owner(), None);
        assert!(!sess.resize_viewer_size(second, 90, 20).unwrap(), "a stale lease cannot resize");
        sess.close_viewer_size(second).unwrap();
        assert_eq!(sess.size(), (30, 8), "an older waiting viewer does not inherit the grid");
        assert!(sess.acquire_viewer_size(first, 100, 30).unwrap(), "a new touch wins again");
        assert_eq!(sess.size(), (100, 30));
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
    fn viewer_viewport_passive_close_never_resizes_external_source() {
        let (sess, etx, _wrx, resized) = ext_session(21, 6);
        etx.send(ExtEvent::SetSize(120, 40)).unwrap();
        etx.send(ExtEvent::Bytes(b"remote-size".to_vec())).unwrap();
        assert!(wait_text(&sess, "remote-size"));
        let token = sess.open_viewer_size();
        assert!(!sess.resize_viewer_size(token, 80, 24).unwrap());
        sess.release_viewer_size(token).unwrap();
        sess.close_viewer_size(token).unwrap();
        assert!(!sess.acquire_viewer_size(token, 80, 24).unwrap());
        sess.close_viewer_size(token).unwrap();
        assert_eq!(sess.size(), (120, 40));
        assert!(resized.lock().unwrap().is_empty(), "a passive viewer must not resize upstream");
    }
}
