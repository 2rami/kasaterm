//! 화면 칸 조작 — 누름·휠·키·글이 어느 보기로 갈지(창·끌던 단추·앞 탭)를 고른다. 한 보기 안의 판정은 보기 앱과 같이 쓰는
//! `kasa_castview`(`input.rs`)다. 「조작」을 켠 칸만 그 기기에 넣는다. 꺼져 있으면 칸 누름은 보통 칸처럼 초점만 옮긴다.

use kasa_castview::{key_mods, Click};
use winit::event::{ElementState, Ime, KeyEvent, MouseButton, WindowEvent};

use super::Surface;
use crate::App;

impl App {
    /// 본창 화면 칸 위의 마우스. 쓴 이벤트면 true — 뒤의 칸 누름 처리(초점·선택)로 흘리지 않는다.
    pub(crate) fn screen_pointer_event(&mut self, event: &WindowEvent) -> bool {
        let p = (f64::from(self.cursor_px.0), f64::from(self.cursor_px.1));
        self.screen_pointer_on(event, p, Surface::Main)
    }

    /// `on` 창에 그려진 화면 보기 위의 마우스(`p` 는 그 창의 논리 px).
    pub(crate) fn screen_pointer_on(&mut self, event: &WindowEvent, p: (f64, f64), on: Surface) -> bool {
        if !self.screen_views.values().any(|v| v.surface == on) {
            return false;
        }
        let mods = key_mods(self.modifiers);
        // 끌던 단추가 있는 칸이 먼저다 — 칸 밖에서 떼도 그 기기에 뗌이 가야 단추가 눌린 채 남지 않는다.
        let held = self.screen_views.iter().find(|(_, v)| v.surface == on && v.buttons != 0).map(|(id, _)| *id);
        match event {
            WindowEvent::MouseInput { state, button, .. } => {
                let bit = match button {
                    MouseButton::Left => 1,
                    MouseButton::Right => 2,
                    MouseButton::Middle => 4,
                    _ => return false,
                };
                let pressed = *state == ElementState::Pressed;
                if held.is_none() {
                    if let Some(view) = self.screen_views.values_mut().find(|v| v.surface == on && v.key_menu.is_some()) {
                        return view.menu_click(p, bit, pressed);
                    }
                }
                let target = held.or_else(|| self.screen_views.iter().find(|(_, v)| v.surface == on && v.takes_click(p)).map(|(id, _)| *id));
                let Some(id) = target else { return false };
                let Some(view) = self.screen_views.get_mut(&id) else { return false };
                match view.click(p, bit, pressed, mods) {
                    Click::Ignored => return false,
                    Click::Handled | Click::Extra(_) => {}
                    Click::Pressed => self.focus_screen_view(id, on),
                    Click::Display(next) => {
                        self.screen_display.insert(id, next);
                        self.screen_views.remove(&id);
                    }
                }
                self.chrome_dirty = true;
                true
            }
            WindowEvent::CursorMoved { .. } => {
                if let Some(view) = self.screen_views.values_mut().find(|v| v.surface == on && v.key_menu.is_some()) {
                    return view.hover(p, mods);
                }
                let target = held.or_else(|| self.screen_views.iter().find(|(_, v)| v.surface == on && v.control && v.on_image(p)).map(|(id, _)| *id));
                target.and_then(|id| self.screen_views.get_mut(&id)).is_some_and(|v| v.hover(p, mods))
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let m = self.modifiers;
                self.screen_views.values_mut().find(|v| v.surface == on && v.on_image(p)).is_some_and(|v| v.wheel(p, delta, m))
            }
            WindowEvent::PinchGesture { delta, .. } => {
                self.screen_views.values_mut().find(|v| v.surface == on && v.on_image(p)).is_some_and(|v| v.pinch(p, *delta))
            }
            _ => false,
        }
    }

    /// 창이 포커스를 잃었다 — 그 표면의 보기들이 혼자 누른 손가락을 잊고 눌린 단추를 뗀다.
    pub(crate) fn screen_focus_lost(&mut self, on: Surface) {
        for view in self.screen_views.values_mut().filter(|v| v.surface == on) {
            view.focus_lost();
        }
    }

    /// 앞에 있는 화면 탭 — 키·글을 받을 보기.
    fn active_screen_view(&self) -> Option<u64> {
        self.ws.lock().unwrap().active().and_then(|p| p.screen().map(|s| s.view_id))
    }

    /// 화면 탭이 앞이면 키를 여기서 다 받는다(true). 조작 중이면 그 기기로, 아니면 버린다 — 같은 칸 다른 탭 셸로 새지 않게.
    pub(crate) fn screen_view_key(&mut self, event: &KeyEvent) -> bool {
        let Some(id) = self.active_screen_view() else { return false };
        self.screen_key_for(id, event)
    }

    /// 그 보기에 키 하나 — 꺼낸 화면 창은 늘 이 보기 하나가 앞이다.
    pub(crate) fn screen_key_for(&mut self, id: u64, event: &KeyEvent) -> bool {
        let m = self.modifiers;
        if let Some(view) = self.screen_views.get_mut(&id) {
            if view.key_event(&mut self.hangul, event, m) {
                self.chrome_dirty = true;
            }
        }
        true
    }

    /// Caps Lock — 맥에서 Windows 를 조작 중이면 그 기기 한/영. winit 은 Caps Lock 을 키로 안 넘겨 AppKit 감시가 부른다.
    pub(crate) fn screen_caps_lock(&mut self) {
        let focused = self.screen_windows.iter().find(|w| w.focused).map(|w| w.view_id);
        let Some(id) = focused.or_else(|| self.active_screen_view()) else { return };
        if let Some(view) = self.screen_views.get(&id) {
            view.caps_lock();
        }
    }

    /// 입력기 확정 글(일본어 등 OS 입력기를 거친 글)도 조작 중인 화면 탭으로.
    pub(crate) fn screen_ime(&mut self, ime: &Ime) -> bool {
        let Some(id) = self.active_screen_view() else { return false };
        self.screen_ime_for(id, ime)
    }

    pub(crate) fn screen_ime_for(&mut self, id: u64, ime: &Ime) -> bool {
        if let (Ime::Commit(text), Some(view)) = (ime, self.screen_views.get(&id)) {
            view.commit(text);
        }
        true
    }

    /// ⌘V — 조작 중이면 이 기기 클립보드 글을 그 기기에 글자째 넣는다. 화면 탭이 앞이면 어디로도 붙이지 않는다.
    pub(crate) fn screen_paste(&mut self) -> bool {
        let Some(id) = self.active_screen_view() else { return false };
        self.screen_paste_for(id)
    }

    pub(crate) fn screen_paste_for(&mut self, id: u64) -> bool {
        let Some(view) = self.screen_views.get_mut(&id).filter(|v| v.control) else { return true };
        view.forget_alone();
        let text = match crate::shell_edit::verification_clipboard() {
            Some(clipboard) => Some(clipboard.load()),
            None => arboard::Clipboard::new().ok().and_then(|mut cb| cb.get_text().ok()),
        };
        if let (Some(text), Some(view)) = (text, self.screen_views.get(&id)) {
            view.commit(&text);
        }
        true
    }

    /// 누른 화면 탭을 앞 칸·앞 탭으로 — 키가 그 칸으로 간다. 꺼낸 화면 창은 그 창이 곧 초점이라 할 일이 없다.
    fn focus_screen_view(&mut self, id: u64, on: Surface) {
        if on != Surface::Main {
            return;
        }
        let mut ws = self.ws.lock().unwrap();
        let found = ws
            .panes
            .iter()
            .find_map(|(pid, p)| p.tabs.iter().position(|t| t.screen().is_some_and(|s| s.view_id == id)).map(|ti| (pid.clone(), ti)));
        if let Some((pid, ti)) = found {
            if let Some(p) = ws.panes.get_mut(&pid) {
                p.active_tab = ti;
                p.dirty = true;
            }
            ws.active_pane = Some(pid);
        }
    }
}
