//! 화면 칸 조작 — 칸 위 누름·휠·키·글을 그 기기에 넣는다. 말은 화면 거울과 같다(`docs/screen-mirror.md` 「조작」).
//! 「조작」 단추를 켠 칸만 넣는다. 꺼져 있으면 칸 누름은 보통 칸처럼 초점만 옮긴다.

use winit::event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};

use super::Surface;
use crate::App;

/// 글자 넣기 한 번의 상한 — 그 기기가 4096 바이트 넘는 글은 버린다(`screen_mirror` TEXT_MAX).
const TEXT_CHUNK: usize = 4000;

/// 1 shift·2 control·4 option(alt)·8 command(win).
fn mods_of(m: ModifiersState) -> u8 {
    u8::from(m.shift_key()) | u8::from(m.control_key()) << 1 | u8::from(m.alt_key()) << 2 | u8::from(m.super_key()) << 3
}

fn is_modifier(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::ShiftLeft
            | KeyCode::ShiftRight
            | KeyCode::ControlLeft
            | KeyCode::ControlRight
            | KeyCode::SuperLeft
            | KeyCode::SuperRight
            | KeyCode::CapsLock
            | KeyCode::Fn
    )
}

/// 휠 한 줄을 화면 포인트로 — 화면 거울 웹 보기와 같은 값.
const LINE_PX: f64 = 40.0;

/// 확대 휠 — 휠 한 줄(40pt)에 약 1.5배.
const WHEEL_ZOOM: f64 = 0.01;

/// 확대 휠·키의 손가락 — 맥 ⌘, Windows Ctrl(그 OS 의 확대 손가락). 조작 중에도 이 손가락이면 그 기기로 안 보내고 확대한다.
fn zoom_chord(m: ModifiersState) -> bool {
    if cfg!(target_os = "macos") { m.super_key() } else { m.control_key() }
}

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
                // 열린 키 메뉴가 먼저다 — 줄을 누르면 그 키를 보내고, 어디를 누르든 닫는다. 그 누름은 그 기기로 안 간다.
                if held.is_none() {
                    if let Some(view) = self.screen_views.values_mut().find(|v| v.surface == on && v.key_menu.is_some()) {
                        if pressed && bit == 1 {
                            if let Some(i) = view.key_row_at(p) {
                                let (_, code, keys) = view.special_keys()[i];
                                view.key(code, keys);
                            }
                            view.key_menu = None;
                        }
                        return true;
                    }
                }
                let target = held.or_else(|| {
                    self.screen_views
                        .iter()
                        .find(|(_, v)| {
                            v.surface == on
                                && (v.on_toggle(p) || v.on_zoom_chip(p) || v.on_key_button(p) || v.on_display_chip(p) || v.audio.on_pill(p)
                                    || (v.control && v.on_image(p)))
                        })
                        .map(|(id, _)| *id)
                });
                let Some(id) = target else { return false };
                let Some(view) = self.screen_views.get_mut(&id) else { return false };
                if held.is_none() && view.on_display_chip(p) {
                    // 다른 화면은 그 기기에서 새 복제·압축이라 연결을 닫고 그 화면으로 다시 붙는다(`sync_screen_views`).
                    if let Some((_, _, next)) = view.display_cycle().filter(|_| pressed && bit == 1) {
                        self.screen_display.insert(id, next);
                        self.screen_views.remove(&id);
                        self.chrome_dirty = true;
                    }
                    return true;
                }
                if held.is_none() && view.on_zoom_chip(p) {
                    if pressed && bit == 1 {
                        view.zoom_reset();
                    }
                    return true;
                }
                if held.is_none() && view.audio.on_pill(p) {
                    if pressed && bit == 1 {
                        view.toggle_audio();
                        self.chrome_dirty = true;
                    }
                    return true;
                }
                if held.is_none() && view.on_key_button(p) {
                    if pressed && bit == 1 {
                        view.key_menu = Some(Vec::new());
                    }
                    return true;
                }
                if held.is_none() && view.on_toggle(p) {
                    if pressed && bit == 1 {
                        let control = !view.control;
                        view.set_control(control);
                        self.focus_screen_view(id, on);
                        self.chrome_dirty = true;
                    }
                    return true;
                }
                let buttons = if pressed { view.buttons | bit } else { view.buttons & !bit };
                view.option_alone &= !pressed;
                let mods = view.remote_mods(mods);
                view.pointer(p, buttons, mods);
                if pressed {
                    self.focus_screen_view(id, on);
                }
                true
            }
            WindowEvent::CursorMoved { .. } => {
                // 키 메뉴를 고르는 동안은 그 기기 포인터를 안 움직인다 — 메뉴가 장 위에 떠 있다.
                if self.screen_views.values().any(|v| v.surface == on && v.key_menu.is_some()) {
                    return true;
                }
                let target = held.or_else(|| self.screen_views.iter().find(|(_, v)| v.surface == on && v.control && v.on_image(p)).map(|(id, _)| *id));
                let Some(view) = target.and_then(|id| self.screen_views.get_mut(&id)) else { return false };
                let buttons = view.buttons;
                let mods = view.remote_mods(mods);
                view.pointer(p, buttons, mods);
                true
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (f64::from(*x) * LINE_PX, f64::from(*y) * LINE_PX),
                    MouseScrollDelta::PixelDelta(d) => (d.x, d.y),
                };
                let Some(view) = self.screen_views.values_mut().find(|v| v.surface == on && v.on_image(p)) else { return false };
                if zoom_chord(self.modifiers) {
                    view.zoom_by(Some(p), (dy * WHEEL_ZOOM).exp() as f32);
                } else if view.control {
                    view.scroll(p, dx, dy);
                } else if view.zoom > 1.0 {
                    view.pan_by(dx, dy);
                } else {
                    return false;
                }
                true
            }
            WindowEvent::PinchGesture { delta, .. } => {
                let Some(view) = self.screen_views.values_mut().find(|v| v.surface == on && v.on_image(p)) else { return false };
                view.zoom_by(Some(p), (1.0 + *delta as f32).max(0.1));
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
        if event.state != ElementState::Pressed {
            self.screen_key_up(id, event);
            return true;
        }
        // 확대 키(⌘ = · ⌘ - · ⌘ 0)는 조작을 켜지 않은 보기에서만 — 조작 중이면 그 기기 단축키다.
        if let Some(view) = self.screen_views.get_mut(&id).filter(|v| !v.control) {
            if zoom_chord(self.modifiers) {
                match event.logical_key.as_ref() {
                    Key::Character("=" | "+") => view.zoom_by(None, 1.25),
                    Key::Character("-") => view.zoom_by(None, 0.8),
                    Key::Character("0") => view.zoom_reset(),
                    _ => {}
                }
                self.chrome_dirty = true;
            }
            return true;
        }
        if !self.screen_views.get(&id).is_some_and(|v| v.control) {
            return true;
        }
        let mods = mods_of(self.modifiers);
        if self.screen_views.get(&id).is_some_and(super::ScreenView::raw_keys) {
            return self.screen_raw_key(id, event, mods);
        }
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
        let Some(view) = self.screen_views.get_mut(&id) else { return true };
        if view.key_menu.is_some() && matches!(event.logical_key, Key::Named(NamedKey::Escape)) {
            view.key_menu = None;
            self.chrome_dirty = true;
            return true;
        }
        let mods = view.remote_mods(mods);
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

    /// 맥에서 Windows 를 조작할 때의 키 — 글자 대신 누른 자리(웹 `code`)를 보낸다. 한글은 이 기기 입력기 대신 그 기기
    /// 입력기가 조합하고(한/영은 Caps Lock — `screen_caps_lock`), 게임도 받는다. 그 기기가 모르는 키만 글자째 보낸다.
    fn screen_raw_key(&mut self, id: u64, event: &KeyEvent, mods: u8) -> bool {
        // 조작 방식이 바뀌기 전에 이 기기에서 조합하던 글자가 남았으면 마저 보낸다.
        if let Some(done) = self.hangul.flush() {
            self.screen_text(id, &done);
            self.screen_preedit(id);
        }
        let PhysicalKey::Code(code) = event.physical_key else { return true };
        let Some(view) = self.screen_views.get_mut(&id) else { return true };
        if view.key_menu.is_some() && code == KeyCode::Escape {
            view.key_menu = None;
            self.chrome_dirty = true;
            return true;
        }
        let option = matches!(code, KeyCode::AltLeft | KeyCode::AltRight);
        view.option_alone = option && !event.repeat;
        // 손가락 키 자체는 안 보낸다 — 다음 키에 손가락 비트로 실려 그 자리에서 눌렀다 뗀다. 따로 눌러 두면 이 앱 단축키
        // (⌘W 등)가 가로챈 뒤 그 기기에 손가락이 눌린 채 남는다.
        if option || is_modifier(code) {
            return true;
        }
        let name = format!("{code:?}");
        let mods = view.remote_mods(mods);
        if kasa_cast_host::input::known_key(&name) {
            view.key(&name, mods);
        } else if let Some(t) = event.text.as_ref().filter(|t| !t.chars().any(char::is_control)) {
            view.text(t);
        }
        true
    }

    /// 뗌 — ⌥ 만 눌렀다 떼면 Windows 키 한 번(시작 메뉴). 다른 키는 누를 때 눌렀다 떼서 보낼 것이 없다.
    fn screen_key_up(&mut self, id: u64, event: &KeyEvent) {
        let Some(view) = self.screen_views.get_mut(&id).filter(|v| v.control && v.raw_keys()) else { return };
        if matches!(event.physical_key, PhysicalKey::Code(KeyCode::AltLeft | KeyCode::AltRight)) && view.option_alone {
            view.key("MetaLeft", 0);
        }
        view.option_alone = false;
    }

    /// Caps Lock — 맥에서 Windows 를 조작 중이면 그 기기 한/영. winit 은 Caps Lock 을 키로 안 넘겨 AppKit 감시가 부른다.
    pub(crate) fn screen_caps_lock(&mut self) {
        let focused = self.screen_windows.iter().find(|w| w.focused).map(|w| w.view_id);
        let Some(id) = focused.or_else(|| self.active_screen_view()) else { return };
        if let Some(view) = self.screen_views.get(&id).filter(|v| v.control && v.raw_keys()) {
            view.key("Lang1", 0);
        }
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

/// Caps Lock 을 AppKit 에서 지켜본다 — winit 은 Caps Lock 의 `flagsChanged` 를 키로 안 넘긴다. 이 앱 창에 온 것만 보고
/// 사건은 그대로 흘린다(맥 입력 소스 바꾸기도 그대로 산다). 한 번만 건다.
#[cfg(target_os = "macos")]
pub(crate) fn watch_caps_lock(proxy: &winit::event_loop::EventLoopProxy<crate::UserEvent>) {
    use objc2_app_kit::{NSEvent, NSEventMask};
    use std::sync::atomic::{AtomicBool, Ordering};
    static ON: AtomicBool = AtomicBool::new(false);
    if ON.swap(true, Ordering::Relaxed) {
        return;
    }
    const CAPS_LOCK: u16 = 57;
    // 한 번 누름이 사건 둘(잠금 켬·끔)로 올 수 있다 — 입력 소스 바꾸기로 쓰면 누를 때 켰다가 바로 푼다. 둘 다 보내면
    // 그 기기 한/영이 두 번 바뀌어 제자리다.
    const SAME_PRESS: std::time::Duration = std::time::Duration::from_millis(300);
    let proxy = proxy.clone();
    let last = std::cell::Cell::new(None::<std::time::Instant>);
    let block = block2::RcBlock::new(move |event: std::ptr::NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: AppKit 이 감시 블록에 넘기는 사건은 블록이 도는 동안 살아 있다.
        if unsafe { event.as_ref().keyCode() } == CAPS_LOCK {
            let now = std::time::Instant::now();
            if last.get().is_none_or(|at| now.duration_since(at) > SAME_PRESS) {
                let _ = proxy.send_event(crate::UserEvent::ScreenCapsLock);
            }
            last.set(Some(now));
        }
        event.as_ptr()
    });
    // SAFETY: 블록은 받은 사건을 그대로 돌려준다. 감시는 앱이 끝날 때까지 두므로 돌려받은 표를 놓지 않는다.
    if let Some(monitor) = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::FlagsChanged, &block) } {
        std::mem::forget(monitor);
    }
}
