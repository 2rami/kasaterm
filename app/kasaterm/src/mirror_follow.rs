//! 원본 격자는 쓰는 쪽을 따른다(tmux `window-size latest`). 폰까지 묶은 규칙은
//! docs/webterm-handoff.md 「원본 크기는 쓰는 쪽이 쥔다」 한 곳에 있다.
//!
//! PC 거울은 원본 TUI 격자로 시작한다. 보는 칸 크기로 원본 PTY 를 잡고, 원본 기기의 사람이
//! 만지면 원본 크기로 되찾는다. 사람이 대화·명령 묶음을 선택한 동안에는 원본 크기를 잡지 않는다.
//! 옛 호스트는 이 규칙을 몰라 `touch_source` 가 아무것도 안 보낸다.
use super::*;
use std::collections::HashSet;
use winit::event::{ElementState, MouseButton, WindowEvent};

/// 잡을 일이 생기고 이만큼 그대로여야 잡는다 — 판정이 잠깐 스친 것으로 원본 화면을 흔들지 않게.
const SETTLE: std::time::Duration = std::time::Duration::from_millis(1200);
/// 보기 시작한 뒤 이만큼은 「보기 시작」이다 — 셸 통합 여부처럼 늦게 오는 판정도 그 안이면 잡는다.
const VIEW_START: std::time::Duration = std::time::Duration::from_secs(6);

/// 보는 동안 원본 격자를 쥐는 거울들(탭 pid).
#[derive(Default)]
pub(crate) struct ViewHold {
    /// 보이는 거울과 보이기 시작한 때.
    seen: HashMap<String, Instant>,
    /// 잡을 일이 생긴 때.
    wanting: HashMap<String, Instant>,
    /// 보는 동안이라 쥔 거울 — 안 보이거나 다시 그릴 수 있게 되면 놓는다.
    holding: HashSet<String>,
    /// 원본 사람이 되찾았거나 저쪽에서 띄운 앱이라 비켜 둔 거울 — 이 칸을 만지거나 다시 보기 시작할 때까지 안 잡는다.
    yielded: HashSet<String>,
}

impl ViewHold {
    /// `seen` 은 지금 보이는 거울, `want` 는 그중 원본 크기를 잡아야 하는 것, `focused` 는 초점 탭.
    pub(crate) fn step(&mut self, seen: &HashSet<String>, want: &HashSet<String>, focused: Option<&str>, now: Instant) {
        self.seen.retain(|tab, _| seen.contains(tab));
        for tab in seen {
            self.seen.entry(tab.clone()).or_insert(now);
        }
        self.yielded.retain(|tab| seen.contains(tab));
        self.wanting.retain(|tab, _| want.contains(tab));
        for tab in self.holding.clone() {
            if !want.contains(&tab) {
                kasa_mcp::remote::restore_source(&tab);
                self.holding.remove(&tab);
            } else if kasa_mcp::remote::held_source_size(&tab).is_none() {
                self.holding.remove(&tab);
                self.yielded.insert(tab);
            }
        }
        for tab in want {
            if self.holding.contains(tab) {
                continue;
            }
            // 사람이 이 칸을 만져 이미 쥐었다.
            if kasa_mcp::remote::held_source_size(tab).is_some() {
                self.yielded.remove(tab);
                self.holding.insert(tab.clone());
                continue;
            }
            if self.yielded.contains(tab) {
                continue;
            }
            if !self.wanting.contains_key(tab) {
                // 한참 보던 칸에 이제야 잡을 일이 생긴 것은 저쪽 사람이 앱을 띄운 것이다 — 여기 초점이 아니면 비킨다.
                let viewing_since = self.seen.get(tab).copied().unwrap_or(now);
                if now.duration_since(viewing_since) > VIEW_START && focused != Some(tab.as_str()) {
                    self.yielded.insert(tab.clone());
                    continue;
                }
                self.wanting.insert(tab.clone(), now);
            }
            if self.wanting.get(tab).is_some_and(|t| now.duration_since(*t) >= SETTLE) && kasa_mcp::remote::touch_source(tab) {
                self.holding.insert(tab.clone());
            }
        }
    }
}

/// 사람 손이 닿은 이벤트 한 묶음 — 이벤트 처리가 끝난 뒤(`about_to_wait`) 초점이
/// 어디로 갔는지 보고 판정한다. 이벤트 처리 중에는 초점이 아직 안 옮겨졌다.
#[derive(Clone, Debug, Default)]
pub(crate) struct HumanTouch {
    before: Option<String>,
    click: Option<(f32, f32)>,
}

impl App {
    /// 사람이 이 칸을 만졌다. 다시 그릴 수 없는 거울이면 원본을 이 칸 크기로, 원본이면 뷰어가 쥔 크기를 거둔다.
    pub(crate) fn touch_surface_size(&self, surface: &str) {
        if kasa_mcp::remote::is_view_pane(surface) {
            // 대화·명령 묶음은 원본 크기 없이 다시 짓는다 — docs/mirror-render.md. 입력마다 불리는 자리라
            // 판정은 ws 를 기다리지 않는다(쥐고 부른 자리면 판정을 모르는 거울처럼 다룬다).
            let needs = match self.ws.try_lock() {
                Ok(ws) => ws.panes.keys().find(|id| ws.active_tab_pid(id) == surface).is_some_and(|id| self.mirror_needs_source(&ws, id, surface)),
                Err(_) => false,
            };
            if needs || self.mirror_kind(surface) == crate::mirror_render::MirrorKind::Grid {
                kasa_mcp::remote::touch_source(surface);
            }
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
    /// 다른 쪽(서버·거울 스레드)이 그 상태에 닿을 때까지 기다린다. 판정은 상태로만 한다 — 시간 한도는 멈춘
    /// 시험을 끊는 울타리일 뿐이라 부하가 걸린 검사 기계에서도 넉넉하게 둔다.
    fn wait(what: &str, mut ok: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !ok() {
            assert!(std::time::Instant::now() < deadline, "timed out: {what}");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// 실제 HTTP·WS 로 원본(32×23 셸)과 거울을 잇는다 — (원본, 거울 연결, 거울 칸 id).
    #[cfg(unix)]
    fn linked(tag: &str) -> (std::sync::Arc<kasa_pty::PtySession>, kasa_mcp::remote::RemoteSession, String) {
        use std::sync::Arc;
        let id = format!("{tag}-origin-{}", uuid::Uuid::new_v4());
        let local = format!("{tag}-local-{}", uuid::Uuid::new_v4());
        let source = Arc::new(kasa_pty::PtySession::start(kasa_pty::PtyOptions {
            pane_id: id.clone(), shell: Some("/bin/sh".into()),
            cols: 32, rows: 23, ..Default::default()
        }).unwrap());
        kasa_pty::register_session(&id, &source);
        // 열쇠를 먼저 붙여 거울이 실제 앱처럼 늘 열쇠로 원본을 되찾는 재접속 길을 탄다 — 안 붙이면 보드
        // 수집기가 악수보다 먼저 도느냐에 따라 길이 갈린다(`mirror_host` 머리말).
        kasa_mcp::surface_keys::ensure(&id);
        let backend = kasa_mcp::standalone::mirror_host::MirrorHost::new(&[&id]);
        let port = kasa_mcp::spawn_http_server_opts(backend, 0, false).unwrap();
        let mirror = kasa_mcp::remote::connect_view(kasa_mcp::remote::RemoteSpec {
            base: format!("http://127.0.0.1:{port}"), pane: Some(id),
            cwd: None, token: None, identity: Default::default(),
        }, &local).unwrap();
        wait("latest capability", || kasa_mcp::remote::follows_latest(&local));
        (source, mirror, local)
    }

    /// 보기만 하면 원본 격자 그대로, 만지면 뷰어 칸 크기, 칸이 바뀌면 따라가고, 원본에서 사람이 만지면
    /// 되찾아 재접속해도 안 빼앗긴다.
    #[test]
    #[cfg(unix)]
    fn the_latest_toucher_owns_the_source_grid() {
        let (source, mirror, local) = linked("follow");

        // 손대지 않은 거울은 칸 크기를 적어 두기만 하고 아무것도 안 보낸다 — 쥔 크기가 없으니 보낼 길이 없다.
        assert!(kasa_mcp::remote::set_viewport(&local, 150, 40));
        assert_eq!(kasa_mcp::remote::held_source_size(&local), None);
        assert_eq!(source.viewer_size_owner(), None, "an untouched mirror only reflows locally");
        assert_eq!(source.size(), (32, 23));

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
        // 서버는 같은 연결에 「잃음」을 먼저, 닫기를 그 뒤에 보낸다. 거울은 그 순서대로 쥔 크기를 비우고 다시
        // 붙으므로, 다시 붙는 악수에 되잡기(acquire)를 실을 근거가 없다 — 소유자가 비어 있으면 아무도 안 잡은 것이다.
        wait("lost notice", || kasa_mcp::remote::held_source_size(&local).is_none());
        wait("mirror reattached", || mirror.session.size() == (40, 20));
        assert_eq!(kasa_mcp::remote::held_source_size(&local), None);
        assert_eq!(source.viewer_size_owner(), None, "reconnect does not steal back the reclaimed grid");
        assert_eq!(source.size(), (40, 20));

        assert!(kasa_mcp::remote::touch_source(&local));
        wait("touch again", || source.size() == (120, 30));
        drop(mirror);
        wait("closing the mirror releases", || source.size() == (40, 20));
    }

    /// 다시 그릴 수 없는 거울은 보는 동안만 쥔다 — 보기 시작하면 잡고, 원본 사람이 되찾으면 비켜 두다
    /// 여기서 만지면 다시 잡고, 안 보게 되면 놓는다. 한참 보던 칸에 저쪽이 앱을 띄우면 초점이 없는 한 안 잡는다.
    #[test]
    #[cfg(unix)]
    fn a_viewed_mirror_holds_the_source_only_while_it_is_viewed() {
        use super::{ViewHold, SETTLE, VIEW_START};
        use std::collections::HashSet;
        use std::time::{Duration, Instant};
        let (source, _mirror, local) = linked("hold");
        assert!(kasa_mcp::remote::set_viewport(&local, 150, 40));
        let seen: HashSet<String> = [local.clone()].into();
        let none = HashSet::new();
        let mut hold = ViewHold::default();
        let t0 = Instant::now();

        hold.step(&seen, &seen, None, t0);
        assert_eq!(source.size(), (32, 23), "a passing judgement does not shake the source");
        hold.step(&seen, &seen, None, t0 + SETTLE);
        wait("held while viewed", || source.size() == (150, 40));

        assert!(source.reclaim_viewer_sizes().unwrap());
        wait("lost notice", || kasa_mcp::remote::held_source_size(&local).is_none());
        hold.step(&seen, &seen, None, t0 + SETTLE * 2);
        hold.step(&seen, &seen, None, t0 + SETTLE * 4);
        assert_eq!(source.size(), (32, 23), "the source person took it back; the viewer stands aside");

        assert!(kasa_mcp::remote::touch_source(&local));
        wait("a touch here takes it again", || source.size() == (150, 40));
        hold.step(&seen, &seen, None, t0 + SETTLE * 5);
        hold.step(&none, &none, None, t0 + SETTLE * 6);
        wait("out of view releases", || source.size() == (32, 23));

        // 다시 보기 시작했지만 한참 셸이었다가 저쪽에서 앱을 띄웠다.
        let t1 = t0 + SETTLE * 7;
        hold.step(&seen, &none, None, t1);
        let later = t1 + VIEW_START + Duration::from_secs(1);
        hold.step(&seen, &seen, None, later);
        hold.step(&seen, &seen, None, later + SETTLE);
        assert_eq!(source.size(), (32, 23), "an app launched over there is not grabbed from an unfocused viewer");
        hold.step(&none, &none, None, later + SETTLE * 2);

        // 여기 사람이 초점을 둔 칸이면 띄운 것이 여기서 온 것이다.
        let t2 = later + SETTLE * 3;
        hold.step(&seen, &none, Some(&local), t2);
        let later = t2 + VIEW_START + Duration::from_secs(1);
        hold.step(&seen, &seen, Some(&local), later);
        hold.step(&seen, &seen, Some(&local), later + SETTLE);
        wait("a focused viewer takes it", || source.size() == (150, 40));
    }
}
