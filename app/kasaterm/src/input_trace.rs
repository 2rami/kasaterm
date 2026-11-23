//! 창이 받은 키를 `kasa_pty::input_trace` 고리에 올린다 — 받은 순간과, 어디로도 안 갔으면 그때 앞을 가린 것.
//! 쓰기 쪽 발자국은 `send_bytes_to_surface`·PTY 관문·거울 송신이 남긴다.
use winit::event::{ElementState, Ime, WindowEvent};
use winit::keyboard::Key;

use crate::App;

/// 키 하나의 처리 구간. 떨어질 때 쓰기가 없었으면 받을 때의 상태를 사유로 남긴다.
pub(crate) struct KeyTrace {
    key: u64,
    state: String,
}

impl Drop for KeyTrace {
    fn drop(&mut self) {
        let state = std::mem::take(&mut self.state);
        kasa_pty::input_trace::end(self.key, || state);
    }
}

pub(crate) fn begin(event: &WindowEvent, state: impl FnOnce() -> String) -> Option<KeyTrace> {
    let detail = match event {
        WindowEvent::KeyboardInput { event, is_synthetic, .. } => {
            if event.state != ElementState::Pressed || crate::input::is_modifier_key(event) {
                return None;
            }
            let key = match &event.logical_key {
                Key::Character(text) => kasa_pty::input_trace::mask(text),
                Key::Named(named) => format!("{named:?}"),
                other => format!("{other:?}"),
            };
            let text = event.text.as_deref().map_or("none".into(), kasa_pty::input_trace::mask);
            format!("key {key} text={text}{}{}{}", if event.repeat { " repeat" } else { "" },
                if *is_synthetic { " synthetic" } else { "" }, queue_delay())
        }
        WindowEvent::Ime(Ime::Commit(text)) => format!("ime-commit {}{}", kasa_pty::input_trace::mask(text), queue_delay()),
        WindowEvent::Ime(Ime::Preedit(text, _)) => {
            kasa_pty::input_trace::event("ime-preedit", format!("{}자", text.chars().count()));
            return None;
        }
        WindowEvent::Ime(ime @ (Ime::Enabled | Ime::Disabled)) => {
            kasa_pty::input_trace::event("ime", format!("{ime:?}"));
            return None;
        }
        WindowEvent::Focused(focused) => {
            kasa_pty::input_trace::event("focus", format!("window focused={focused}"));
            return None;
        }
        _ => return None,
    };
    let state = state();
    let key = kasa_pty::input_trace::begin(detail);
    Some(KeyTrace { key, state })
}

/// OS 가 키를 큐에 넣은 뒤 우리가 받기까지 — 메인 스레드가 막혀 늦게 받은 키를 가른다. 윈도우만 그 시각을
/// 알려 준다(마지막으로 꺼낸 메시지의 시각, 부팅 뒤 ms).
#[cfg(windows)]
fn queue_delay() -> String {
    use windows_sys::Win32::System::SystemInformation::GetTickCount;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetMessageTime;
    let queued = unsafe { GetMessageTime() } as u32;
    let waited = unsafe { GetTickCount() }.wrapping_sub(queued);
    if waited > 60_000 { String::new() } else { format!(" q={waited}ms") }
}

#[cfg(not(windows))]
fn queue_delay() -> String {
    String::new()
}

impl App {
    /// 키가 PTY 로 못 갔을 때 그 이유가 될 만한 것 — 앞을 가린 화면·입력칸·조합 상태.
    pub(crate) fn input_trace_state(&self) -> String {
        let target = self.target_surface();
        let mut covers: Vec<&str> = Vec::new();
        let mut cover = |on: bool, name: &'static str| if on { covers.push(name) };
        cover(self.restoration_blocks_input(), "restoring");
        cover(target.as_deref().is_some_and(|id| self.restoration_blocks_surface(id)), "pane-restoring");
        cover(self.confirm_close.is_some(), "confirm-close");
        cover(self.restore_prompt.is_some(), "restore-prompt");
        cover(self.character_swap_confirm.is_some(), "swap-confirm");
        cover(self.account_switch_confirm.is_some(), "account-confirm");
        cover(self.main_confirmation_modal(), "modal");
        cover(self.settings_room_active(), "settings");
        cover(self.account_menu, "account-menu");
        cover(self.image_preview.focused(), "image-preview");
        cover(self.side_column_covers_workspace(), "side-column");
        cover(self.room_rename.editing.is_some(), "room-rename");
        cover(self.web_addr.is_some(), "web-addr");
        cover(self.web_find.is_some(), "web-find");
        cover(self.target_pane().is_some_and(|id| self.chat_view_showing(&id)), "chat-view");
        cover(self.file_tree.new.is_some() || self.file_tree.rename.is_some() || self.file_tree.search_active, "file-tree");
        cover(self.ime_active, "ime");
        cover(self.in_preedit, "preedit");
        format!("target={} [{}]", target.as_deref().unwrap_or("none"), covers.join(","))
    }
}
