//! 화면 칸 조작 — 칸 위 누름·휠·키·글을 그 기기에 넣는다. 말은 화면 거울과 같다(`docs/screen-mirror.md` 「조작」).
//! 「조작」 단추를 켠 칸만 넣는다. 꺼져 있으면 칸 누름은 보통 칸처럼 초점만 옮긴다.

use winit::event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey, PhysicalKey};

use super::Surface;
use crate::App;

/// 글자 넣기 한 번의 상한 — 그 기기가 4096 바이트 넘는 글은 버린다(`screen_mirror` TEXT_MAX).
const TEXT_CHUNK: usize = 4000;

/// 1 shift·2 control·4 option(alt)·8 command(win).
fn mods_of(m: ModifiersState) -> u8 {
    u8::from(m.shift_key()) | u8::from(m.control_key()) << 1 | u8::from(m.alt_key()) << 2 | u8::from(m.super_key()) << 3
}

/// 휠 한 줄을 화면 포인트로 — 화면 거울 웹 보기와 같은 값.
const LINE_PX: f64 = 40.0;

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
        let mods = mods_of(self.modifiers);
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
                let target = held.or_else(|| {
                    self.screen_views
                        .iter()
                        .find(|(_, v)| v.surface == on && (v.on_toggle(p) || (v.control && v.on_image(p))))
                        .map(|(id, _)| *id)
                });
                let Some(id) = target else { return false };
                let Some(view) = self.screen_views.get_mut(&id) else { return false };
                if held.is_none() && view.on_toggle(p) {
                    if pressed && bit == 1 {
                        view.control = !view.control;
                        self.focus_screen_view(id, on);
                        self.chrome_dirty = true;
                    }
                    return true;
                }
                let buttons = if pressed { view.buttons | bit } else { view.buttons & !bit };
                view.pointer(p, buttons, mods);
                if pressed {
                    self.focus_screen_view(id, on);
                }
                true
            }
            WindowEvent::CursorMoved { .. } => {
                let target = held.or_else(|| self.screen_views.iter().find(|(_, v)| v.surface == on && v.control && v.on_image(p)).map(|(id, _)| *id));
                let Some(view) = target.and_then(|id| self.screen_views.get_mut(&id)) else { return false };
                let buttons = view.buttons;
                view.pointer(p, buttons, mods);
                true
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let Some(view) = self.screen_views.values().find(|v| v.surface == on && v.control && v.on_image(p)) else { return false };
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (f64::from(*x) * LINE_PX, f64::from(*y) * LINE_PX),
                    MouseScrollDelta::PixelDelta(d) => (d.x, d.y),
                };
                view.scroll(p, dx, dy);
                true
            }
            _ => false,
        }
    }

    /// 앞에 있는 화면 탭 — 키·글을 받을 보기.
    fn active_screen_view(&self) -> Option<u64> {
        self.ws.lock().unwrap().active().and_then(|p| p.screen().map(|s| s.view_id))
    }

    /// 화면 탭이 앞이면 키를 여기서 다 받는다(true). 조작 중이면 그 기기로, 아니면 버린다 — 같은 칸 다른 탭 셸로 새지 않게.
    /// 한글은 이 앱이 자모로 받아 조합하므로(macOS) 다 된 글자만 보낸다.
    pub(crate) fn screen_view_key(&mut self, event: &KeyEvent) -> bool {
        let Some(id) = self.active_screen_view() else { return false };
        self.screen_key_for(id, event)
    }

    /// 그 보기에 키 하나 — 꺼낸 화면 창은 늘 이 보기 하나가 앞이다.
    pub(crate) fn screen_key_for(&mut self, id: u64, event: &KeyEvent) -> bool {
        if !self.screen_views.get(&id).is_some_and(|v| v.control) || event.state != ElementState::Pressed {
            return true;
        }
        let mods = mods_of(self.modifiers);
        let chord = mods & (2 | 4 | 8) != 0;
        #[cfg(target_os = "macos")]
        if let Some(c) = event.text.as_ref().filter(|t| !chord && t.chars().count() == 1).and_then(|t| t.chars().next()) {
            if (0x3130..=0x318F).contains(&(c as u32)) {
                if let Some(done) = self.hangul.feed(c) {
                    self.screen_text(id, &done);
                }
                self.screen_preedit(id);
                return true;
            }
        }
        if matches!(event.logical_key, Key::Named(NamedKey::Backspace)) && self.hangul.backspace() {
            self.screen_preedit(id);
            return true;
        }
        if let Some(done) = self.hangul.flush() {
            self.screen_text(id, &done);
        }
        self.screen_preedit(id);
        let PhysicalKey::Code(code) = event.physical_key else { return true };
        // winit 의 키 이름은 웹 `KeyboardEvent.code` 를 따른다(KeyA·Digit1·Enter·ArrowLeft…).
        let code = format!("{code:?}");
        let Some(view) = self.screen_views.get(&id) else { return true };
        match &event.logical_key {
            _ if chord => view.key(&code, mods),
            Key::Named(NamedKey::Space) => view.text(" "),
            Key::Named(_) => view.key(&code, mods),
            _ => {
                if let Some(t) = event.text.as_ref().filter(|t| !t.chars().any(char::is_control)) {
                    view.text(t);
                }
            }
        }
        true
    }

    /// 입력기 확정 글(일본어 등 OS 입력기를 거친 글)도 조작 중인 화면 탭으로.
    pub(crate) fn screen_ime(&mut self, ime: &Ime) -> bool {
        let Some(id) = self.active_screen_view() else { return false };
        self.screen_ime_for(id, ime)
    }

    pub(crate) fn screen_ime_for(&mut self, id: u64, ime: &Ime) -> bool {
        if let Ime::Commit(text) = ime {
            if self.screen_views.get(&id).is_some_and(|v| v.control) {
                self.screen_text(id, text);
            }
        }
        true
    }

    /// ⌘V — 조작 중이면 이 기기 클립보드 글을 그 기기에 글자째 넣는다. 화면 탭이 앞이면 어디로도 붙이지 않는다.
    pub(crate) fn screen_paste(&mut self) -> bool {
        let Some(id) = self.active_screen_view() else { return false };
        self.screen_paste_for(id)
    }

    pub(crate) fn screen_paste_for(&mut self, id: u64) -> bool {
        if !self.screen_views.get(&id).is_some_and(|v| v.control) {
            return true;
        }
        let text = match crate::shell_edit::verification_clipboard() {
            Some(clipboard) => Some(clipboard.load()),
            None => arboard::Clipboard::new().ok().and_then(|mut cb| cb.get_text().ok()),
        };
        if let Some(text) = text {
            self.screen_text(id, &text);
        }
        true
    }

    fn screen_text(&self, id: u64, text: &str) {
        let Some(view) = self.screen_views.get(&id) else { return };
        let mut rest = text;
        while !rest.is_empty() {
            let mut cut = rest.len().min(TEXT_CHUNK);
            while !rest.is_char_boundary(cut) {
                cut -= 1;
            }
            view.text(&rest[..cut]);
            rest = &rest[cut..];
        }
    }

    fn screen_preedit(&mut self, id: u64) {
        let preedit = self.hangul.preedit().unwrap_or_default();
        if let Some(view) = self.screen_views.get_mut(&id) {
            view.preedit = preedit;
        }
        self.chrome_dirty = true;
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
