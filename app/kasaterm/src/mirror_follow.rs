//! 원본 격자는 쓰는 쪽을 따른다(tmux `window-size latest`). 폰까지 묶은 규칙은
//! docs/webterm-handoff.md 「원본 크기는 쓰는 쪽이 쥔다」 한 곳에 있다.
//!
//! 거울만 보고 있을 때는 원본 크기를 안 바꾼다 — 초점 없는 거울은 뷰어 쪽에서
//! 다시 접어 그린다(`mirror_view`). 데스크톱 뷰어의 사람이 거울 칸에 초점을 두거나
//! 입력하면 원본 PTY 를 그 칸 크기로 잡고, 원본 기기의 사람이 자기 칸을 만지면
//! 원본 크기로 되찾는다(폰이 쥔 것도 같이). 폰은 칠 때만 쥔다.
//! 옛 호스트는 이 규칙을 몰라 `touch_source` 가 아무것도 안 보낸다.
use super::*;
use winit::event::{ElementState, MouseButton, WindowEvent};

/// 사람 손이 닿은 이벤트 한 묶음 — 이벤트 처리가 끝난 뒤(`about_to_wait`) 초점이
/// 어디로 갔는지 보고 판정한다. 이벤트 처리 중에는 초점이 아직 안 옮겨졌다.
#[derive(Clone, Debug, Default)]
pub(crate) struct HumanTouch {
    before: Option<String>,
    click: Option<(f32, f32)>,
}

impl App {
    /// 사람이 이 칸을 만졌다. 거울이면 원본을 이 칸 크기로, 원본이면 뷰어가 쥔 크기를 거둔다.
    pub(crate) fn touch_surface_size(&self, surface: &str) {
        // 원본 크기를 바꾸지 않는 거울(대화형·셸 묶음)은 빌릴 일이 없다 — docs/mirror-render.md.
        if self.mirror_kind(surface) != crate::mirror_render::MirrorKind::Grid {
            return;
        }
        if kasa_mcp::remote::is_view_pane(surface) {
            kasa_mcp::remote::touch_source(surface);
        } else if let Some(session) = self.pty.get(surface) {
            if session.has_viewer_size_control() {
                let _ = session.reclaim_viewer_sizes();
            }
        }
    }

    /// 키·IME·왼클릭을 적어 둔다. 판정은 `apply_human_touch` 가 한다.
    pub(crate) fn note_human_touch(&mut self, event: &WindowEvent) {
        let click = match event {
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => Some(self.cursor_px),
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => None,
            WindowEvent::Ime(winit::event::Ime::Commit(_)) => None,
            _ => return,
        };
        let before = self.target_surface();
        let touch = self.human_touch.get_or_insert(HumanTouch { before, click: None });
        touch.click = click.or(touch.click);
    }

    /// 사람 손으로 초점이 옮겨졌거나 초점 칸 안을 눌렀으면 그 칸을 만진 것으로 친다.
    /// 입력 자체는 `send_bytes_to_surface`·`send_mouse_sgr` 가 따로 알린다.
    pub(crate) fn apply_human_touch(&mut self) {
        let Some(touch) = self.human_touch.take() else { return };
        let Some(after) = self.target_surface() else { return };
        let clicked = touch.click.and_then(|(x, y)| self.px_to_pane_cell(x, y))
            .map(|(pane, ..)| self.ws.lock().unwrap().active_tab_pid(&pane));
        if touch.before.as_deref() != Some(after.as_str()) || clicked.as_deref() == Some(after.as_str()) {
            self.touch_surface_size(&after);
        }
    }
}

#[cfg(test)]
mod tests {
    fn wait(what: &str, mut ok: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !ok() {
            assert!(std::time::Instant::now() < deadline, "timed out: {what}");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// 실제 HTTP·WS 로 원본과 거울을 잇는다. 보기만 하면 원본 격자 그대로, 만지면 뷰어 칸
    /// 크기, 칸이 바뀌면 따라가고, 원본에서 사람이 만지면 되찾아 재접속해도 안 빼앗긴다.
    #[test]
    #[cfg(unix)]
    fn the_latest_toucher_owns_the_source_grid() {
        use std::sync::Arc;
        let id = format!("follow-origin-{}", uuid::Uuid::new_v4());
        let local = format!("follow-local-{}", uuid::Uuid::new_v4());
        let source = Arc::new(kasa_pty::PtySession::start(kasa_pty::PtyOptions {
            pane_id: id.clone(), shell: Some("/bin/sh".into()),
            cols: 32, rows: 23, ..Default::default()
        }).unwrap());
        kasa_pty::register_session(&id, &source);
        let backend: Arc<dyn kasa_socket::backend::Backend> = Arc::new(
            kasa_mcp::standalone::StandaloneBackend::new(std::env::temp_dir()),
        );
        let port = kasa_mcp::spawn_http_server_opts(backend, 0, false).unwrap();
        let mirror = kasa_mcp::remote::connect_view(kasa_mcp::remote::RemoteSpec {
            base: format!("http://127.0.0.1:{port}"), pane: Some(id),
            cwd: None, token: None, identity: Default::default(),
        }, &local).unwrap();
        wait("latest capability", || kasa_mcp::remote::follows_latest(&local));

        assert!(kasa_mcp::remote::set_viewport(&local, 150, 40));
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(source.size(), (32, 23), "an untouched mirror only reflows locally");

        assert!(kasa_mcp::remote::touch_source(&local));
        wait("viewer size", || source.size() == (150, 40));
        wait("mirror parser", || mirror.session.size() == (150, 40));
        assert!(!kasa_mcp::remote::touch_source(&local), "same size sends nothing per keystroke");

        assert!(kasa_mcp::remote::set_viewport(&local, 120, 30));
        wait("follow the pane", || source.size() == (120, 30));

        source.resize(40, 20).unwrap();
        assert_eq!(source.size(), (120, 30), "a source layout change alone is not a touch");
        assert!(source.reclaim_viewer_sizes().unwrap());
        assert_eq!(source.size(), (40, 20));
        wait("lost notice", || kasa_mcp::remote::held_source_size(&local).is_none());
        wait("mirror reattached", || mirror.session.size() == (40, 20));
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert_eq!(source.size(), (40, 20), "reconnect does not steal back the reclaimed grid");

        assert!(kasa_mcp::remote::touch_source(&local));
        wait("touch again", || source.size() == (120, 30));
        drop(mirror);
        wait("closing the mirror releases", || source.size() == (40, 20));
    }
}
