//! 보기 조작 — 칸 위 누름·휠·키·글을 그 기기 조작으로 바꾼다. 말은 화면 거울과 같다(`docs/screen-mirror.md` 「조작」).
//! 여러 보기 중 누구에게 갈지(초점·끌던 단추)는 부르는 쪽이 고르고, 한 보기 안의 판정은 여기서 한다 — 본 앱 화면 탭과 보기 앱이
//! 같은 판정을 쓴다.

use kasa_ime::Composer;
use winit::event::{ElementState, KeyEvent, MouseScrollDelta};
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};

use crate::{contains, ScreenView};

/// 글자 넣기 한 번의 상한 — 그 기기가 4096 바이트 넘는 글은 버린다(`screen_mirror` TEXT_MAX).
const TEXT_CHUNK: usize = 4000;

/// 휠 한 줄을 화면 포인트로 — 화면 거울 웹 보기와 같은 값.
pub const LINE_PX: f64 = 40.0;

/// 확대 휠 — 휠 한 줄(40pt)에 약 1.5배.
const WHEEL_ZOOM: f64 = 0.01;

/// 1 shift·2 control·4 option(alt)·8 command(win).
pub fn key_mods(m: ModifiersState) -> u8 {
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

/// 확대 휠·키의 손가락 — 맥 ⌘, Windows Ctrl(그 OS 의 확대 손가락). 조작 중에도 이 손가락이면 그 기기로 안 보내고 확대한다.
pub fn zoom_chord(m: ModifiersState) -> bool {
    if cfg!(target_os = "macos") { m.super_key() } else { m.control_key() }
}

/// 그 기기가 한 번에 받는 크기로 자른 글 — 글자 가운데서 자르지 않는다.
pub fn text_chunks(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let mut cut = rest.len().min(TEXT_CHUNK);
        while !rest.is_char_boundary(cut) {
            cut -= 1;
        }
        let (head, tail) = rest.split_at(cut);
        rest = tail;
        Some(head)
    })
}

/// 누름 하나를 보기가 어떻게 받았나.
#[derive(Debug, PartialEq, Eq)]
pub enum Click {
    /// 이 보기 것이 아니다 — 부르는 쪽이 제 일(칸 초점 등)로 넘긴다.
    Ignored,
    /// 받았다 — 다시 그린다.
    Handled,
    /// 그 기기에 누름이 갔거나 「조작」을 바꿨다 — 키가 이 보기로 오게 초점을 옮긴다.
    Pressed,
    /// 다음 화면으로 — 그 기기에서 새 복제·압축이라 연결을 닫고 그 화면 번호로 다시 붙는다.
    Display(u32),
    /// 부르는 쪽이 얹은 단추(`Style::extra`) i 를 눌렀다.
    Extra(usize),
}

impl ScreenView {
    /// 열린 키 메뉴가 먼저다 — 줄을 누르면 그 키를 보내고, 어디를 누르든 닫는다. 그 누름은 그 기기로 안 간다. 메뉴가
    /// 열려 있었으면 true.
    pub fn menu_click(&mut self, p: (f64, f64), bit: u8, pressed: bool) -> bool {
        if self.key_menu.is_none() {
            return false;
        }
        if pressed && bit == 1 {
            if let Some(i) = self.key_row_at(p) {
                let (_, code, keys) = self.special_keys()[i];
                self.key(code, keys);
            }
            self.key_menu = None;
        }
        true
    }

    /// 이 자리 누름을 이 보기가 받나 — 단추이거나 조작 중인 장 위.
    pub fn takes_click(&self, p: (f64, f64)) -> bool {
        self.on_toggle(p)
            || self.on_zoom_chip(p)
            || self.on_key_button(p)
            || self.on_display_chip(p)
            || self.audio.on_pill(p)
            || self.extra.iter().any(|r| contains(*r, p))
            || (self.control && self.on_image(p))
    }

    /// 마우스 단추(1 왼쪽·2 오른쪽·4 가운데)를 누르거나 뗐다. 끌던 단추가 있으면 단추 판정 없이 그 기기로 — 칸 밖에서 떼도
    /// 그 기기에 뗌이 가야 단추가 눌린 채 남지 않는다.
    pub fn click(&mut self, p: (f64, f64), bit: u8, pressed: bool, mods: u8) -> Click {
        if pressed {
            self.forget_alone();
        }
        let left = pressed && bit == 1;
        if self.buttons == 0 {
            if let Some(i) = self.extra.iter().position(|r| contains(*r, p)) {
                return if left { Click::Extra(i) } else { Click::Handled };
            }
            if self.on_display_chip(p) {
                return match self.display_cycle().filter(|_| left) {
                    Some((_, _, next)) => Click::Display(next),
                    None => Click::Handled,
                };
            }
            if self.on_zoom_chip(p) {
                if left {
                    self.zoom_reset();
                }
                return Click::Handled;
            }
            if self.audio.on_pill(p) {
                if left {
                    self.toggle_audio();
                }
                return Click::Handled;
            }
            if self.on_key_button(p) {
                if left {
                    self.key_menu = Some(Vec::new());
                }
                return Click::Handled;
            }
            if self.on_toggle(p) {
                if !left {
                    return Click::Handled;
                }
                let control = !self.control;
                self.set_control(control);
                return Click::Pressed;
            }
            if !(self.control && self.on_image(p)) {
                return Click::Ignored;
            }
        }
        let buttons = if pressed { self.buttons | bit } else { self.buttons & !bit };
        let mods = self.remote_mods(mods);
        self.pointer(p, buttons, mods);
        if pressed { Click::Pressed } else { Click::Handled }
    }

    /// 포인터가 움직였다. 키 메뉴를 고르는 동안은 그 기기 포인터를 안 움직인다 — 메뉴가 장 위에 떠 있다. 받았으면 true.
    pub fn hover(&mut self, p: (f64, f64), mods: u8) -> bool {
        if self.key_menu.is_some() {
            return true;
        }
        if self.buttons == 0 && !(self.control && self.on_image(p)) {
            return false;
        }
        let (buttons, mods) = (self.buttons, self.remote_mods(mods));
        self.pointer(p, buttons, mods);
        true
    }

    /// 휠 — 확대 손가락이면 확대, 조작 중이면 그 기기로, 확대해 둔 장이면 민다. 장 밖이거나 할 일이 없으면 false.
    pub fn wheel(&mut self, p: (f64, f64), delta: &MouseScrollDelta, m: ModifiersState) -> bool {
        self.forget_alone();
        if !self.on_image(p) {
            return false;
        }
        let (dx, dy) = match delta {
            MouseScrollDelta::LineDelta(x, y) => (f64::from(*x) * LINE_PX, f64::from(*y) * LINE_PX),
            MouseScrollDelta::PixelDelta(d) => (d.x, d.y),
        };
        if zoom_chord(m) {
            self.zoom_by(Some(p), (dy * WHEEL_ZOOM).exp() as f32);
        } else if self.control {
            self.scroll(p, dx, dy);
        } else if self.zoom > 1.0 {
            self.pan_by(dx, dy);
        } else {
            return false;
        }
        true
    }

    /// 핀치 — 장 위에서만 확대.
    pub fn pinch(&mut self, p: (f64, f64), delta: f64) -> bool {
        self.forget_alone();
        if !self.on_image(p) {
            return false;
        }
        self.zoom_by(Some(p), (1.0 + delta as f32).max(0.1));
        true
    }

    /// 키 하나. 조작 중이면 그 기기로, 아니면 확대 키(⌘ = · ⌘ - · ⌘ 0)만 — 조작 중이면 그 키도 그 기기 단축키다. 한글은 이
    /// 기기가 자모로 받아 `hangul` 로 조합하므로(macOS) 다 된 글자만 보낸다. 칸 위 글(조합 중·메뉴)이 바뀌었으면 true.
    pub fn key_event(&mut self, hangul: &mut Composer, event: &KeyEvent, m: ModifiersState) -> bool {
        if event.state != ElementState::Pressed {
            self.key_up(event);
            return false;
        }
        if !self.control {
            if !zoom_chord(m) {
                return false;
            }
            match event.logical_key.as_ref() {
                Key::Character("=" | "+") => self.zoom_by(None, 1.25),
                Key::Character("-") => self.zoom_by(None, 0.8),
                Key::Character("0") => self.zoom_reset(),
                _ => {}
            }
            return true;
        }
        let mods = key_mods(m);
        if self.raw_keys() {
            return self.raw_key(hangul, event, mods);
        }
        let chord = mods & (2 | 4 | 8) != 0;
        #[cfg(target_os = "macos")]
        if let Some(c) = event.text.as_ref().filter(|t| !chord && t.chars().count() == 1).and_then(|t| t.chars().next()) {
            if (0x3130..=0x318F).contains(&(c as u32)) {
                if let Some(done) = hangul.feed(c) {
                    self.commit(&done);
                }
                self.preedit = hangul.preedit().unwrap_or_default();
                return true;
            }
        }
        if matches!(event.logical_key, Key::Named(NamedKey::Backspace)) && hangul.backspace() {
            self.preedit = hangul.preedit().unwrap_or_default();
            return true;
        }
        if let Some(done) = hangul.flush() {
            self.commit(&done);
        }
        self.preedit = hangul.preedit().unwrap_or_default();
        let PhysicalKey::Code(code) = event.physical_key else { return true };
        if self.key_menu.is_some() && matches!(event.logical_key, Key::Named(NamedKey::Escape)) {
            self.key_menu = None;
            return true;
        }
        // winit 의 키 이름은 웹 `KeyboardEvent.code` 를 따른다(KeyA·Digit1·Enter·ArrowLeft…).
        let code = format!("{code:?}");
        let mods = self.remote_mods(mods);
        match &event.logical_key {
            _ if chord => self.key(&code, mods),
            Key::Named(NamedKey::Space) => self.text(" "),
            Key::Named(_) => self.key(&code, mods),
            _ => {
                if let Some(t) = event.text.as_ref().filter(|t| !t.chars().any(char::is_control)) {
                    self.text(t);
                }
            }
        }
        true
    }

    /// 맥에서 Windows 를 조작할 때의 키 — 글자 대신 누른 자리(웹 `code`)를 보낸다. 한글은 이 기기 입력기 대신 그 기기
    /// 입력기가 조합하고(한/영은 Caps Lock — [`Self::caps_lock`]), 게임도 받는다. 그 기기가 모르는 키만 글자째 보낸다.
    fn raw_key(&mut self, hangul: &mut Composer, event: &KeyEvent, mods: u8) -> bool {
        // 조작 방식이 바뀌기 전에 이 기기에서 조합하던 글자가 남았으면 마저 보낸다.
        let mut repaint = false;
        if let Some(done) = hangul.flush() {
            self.commit(&done);
            self.preedit = hangul.preedit().unwrap_or_default();
            repaint = true;
        }
        let PhysicalKey::Code(code) = event.physical_key else { return repaint };
        if self.key_menu.is_some() && code == KeyCode::Escape {
            self.key_menu = None;
            return true;
        }
        let option = matches!(code, KeyCode::AltLeft | KeyCode::AltRight);
        self.option_alone = option && !event.repeat;
        self.command_alone = matches!(code, KeyCode::SuperLeft | KeyCode::SuperRight) && !event.repeat && mods & !8 == 0;
        // 손가락 키 자체는 안 보낸다 — 다음 키에 손가락 비트로 실려 그 자리에서 눌렀다 뗀다. 따로 눌러 두면 이 앱 단축키
        // (⌘W 등)가 가로챈 뒤 그 기기에 손가락이 눌린 채 남는다.
        if option || is_modifier(code) {
            return repaint;
        }
        // 맥은 PC 키보드의 PrtSc 를 F13 으로 받는다(맥 키보드에는 없는 키) — 게임 스크린샷이 PrtSc 만 받는다.
        let name = if code == KeyCode::F13 { "PrintScreen".to_string() } else { format!("{code:?}") };
        let mods = self.remote_mods(mods);
        if kasa_cast_host::input::known_key(&name) {
            self.key(&name, mods);
        } else if let Some(t) = event.text.as_ref().filter(|t| !t.chars().any(char::is_control)) {
            self.text(t);
        }
        repaint
    }

    /// 뗌 — ⌥ 만 눌렀다 떼면 Windows 키 한 번(시작 메뉴), ⌘ 만 눌렀다 떼면 Alt 한 번(메뉴 막대). 다른 키는 누를 때
    /// 눌렀다 떼서 보낼 것이 없다.
    fn key_up(&mut self, event: &KeyEvent) {
        if !(self.control && self.raw_keys()) {
            return;
        }
        match event.physical_key {
            PhysicalKey::Code(KeyCode::AltLeft | KeyCode::AltRight) if self.option_alone => self.key("MetaLeft", 0),
            PhysicalKey::Code(KeyCode::SuperLeft | KeyCode::SuperRight) if self.command_alone => self.key("AltLeft", 0),
            _ => {}
        }
        self.forget_alone();
    }

    /// Caps Lock — 맥에서 Windows 를 조작 중이면 그 기기 한/영.
    pub fn caps_lock(&self) {
        if self.control && self.raw_keys() {
            self.key("Lang1", 0);
        }
    }

    /// 다 된 글(입력기 확정·붙여넣기)을 조작 중일 때 글자째 넣는다 — 그 기기가 받는 크기로 잘라서.
    pub fn commit(&self, text: &str) {
        if !self.control {
            return;
        }
        for chunk in text_chunks(text) {
            self.text(chunk);
        }
    }
}

/// Caps Lock 을 AppKit 에서 지켜본다 — winit 은 Caps Lock 의 `flagsChanged` 를 키로 안 넘긴다. 이 앱 창에 온 것만 보고
/// 사건은 그대로 흘린다(맥 입력 소스 바꾸기도 그대로 산다). 한 프로세스에 한 번만 건다.
#[cfg(target_os = "macos")]
pub fn watch_caps_lock(on: impl Fn() + 'static) {
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
    let last = std::cell::Cell::new(None::<std::time::Instant>);
    let block = block2::RcBlock::new(move |event: std::ptr::NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: AppKit 이 감시 블록에 넘기는 사건은 블록이 도는 동안 살아 있다.
        if unsafe { event.as_ref().keyCode() } == CAPS_LOCK {
            let now = std::time::Instant::now();
            if last.get().is_none_or(|at| now.duration_since(at) > SAME_PRESS) {
                on();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_text_is_cut_on_character_boundaries() {
        let text = "가".repeat(2000);
        let chunks: Vec<&str> = text_chunks(&text).collect();
        assert!(chunks.iter().all(|c| c.len() <= TEXT_CHUNK && c.chars().all(|ch| ch == '가')));
        assert_eq!(chunks.concat(), text);
        assert_eq!(text_chunks("").count(), 0);
    }
}
