use kasa_pty::{PtyOptions, PtySession};
use kasa_bridge::screen::{Cell, Row};
use std::sync::Arc;
use winit::keyboard::{Key, NamedKey, PhysicalKey};

#[derive(Clone, Default)]
pub(crate) struct View {
    pub rows: Arc<Vec<Row>>,
    pub cursor: Option<(u16, u16)>,
    pub running: bool,
    pub focused: bool,
    pub error: Option<String>,
}

#[derive(Default)]
pub(crate) struct PreviewShell {
    pty: Option<PtySession>,
    view: View,
    size: Option<(u16, u16)>,
    app_cursor: bool,
    bracketed_paste: bool,
}

impl PreviewShell {
    pub fn view(&self) -> View { self.view.clone() }
    pub fn running(&self) -> bool { self.pty.is_some() }
    pub fn focused(&self) -> bool { self.view.focused }
    pub fn blur(&mut self) { self.view.focused = false; }

    pub fn open(&mut self, shell: &str) {
        if self.pty.is_none() {
            let (cols, rows) = self.size.unwrap_or((80, 10));
            let options = PtyOptions {
                shell: (!shell.is_empty()).then(|| shell.to_owned()),
                cwd: kasa_socket::home_dir().map(|path| path.to_string_lossy().into_owned()),
                cols, rows,
                pane_id: "__kasa_appearance_shell__".into(),
                ..Default::default()
            };
            match PtySession::start(options) {
                Ok(pty) => {
                    self.view = View { running: true, ..View::default() };
                    self.pty = Some(pty);
                }
                Err(error) => self.view.error = Some(error.to_string()),
            }
        }
        self.view.focused = self.pty.is_some();
    }

    pub fn close(&mut self) {
        self.pty = None;
        self.view = View::default();
    }

    pub fn set_size(&mut self, size: (u16, u16)) { self.size = Some(size); }

    pub fn tick(&mut self) -> bool {
        let Some(pty) = &self.pty else { return false };
        if let Some((cols, rows)) = self.size.filter(|&size| size != pty.size()) {
            let _ = pty.resize(cols, rows);
        }
        let mut changed = false;
        while let Ok(update) = pty.screens.try_recv() {
            if update.eof {
                self.pty = None;
                self.view.running = false;
                self.view.focused = false;
                self.view.cursor = None;
                return true;
            }
            changed = true;
            let rows = Arc::make_mut(&mut self.view.rows);
            rows.resize_with(update.rows as usize, || vec![Cell::blank(); update.cols as usize]);
            for (index, row) in update.dirty {
                if let Some(line) = rows.get_mut(index as usize) { *line = row; }
            }
            self.view.cursor = update.cursor_visible.then_some((update.cursor_row, update.cursor_col));
            self.app_cursor = update.app_cursor;
            self.bracketed_paste = update.bracketed_paste;
        }
        changed
    }

    pub fn send(&self, bytes: &[u8]) {
        if let Some(pty) = self.pty.as_ref() {
            pty.scroll_to_bottom();
            let _ = pty.send_bytes(bytes);
        }
    }

    pub fn paste(&self, text: &str) {
        if self.bracketed_paste {
            self.send(format!("\x1b[200~{text}\x1b[201~").as_bytes());
        } else {
            self.send(text.as_bytes());
        }
    }

    pub fn scroll(&self, lines: i32) {
        if let Some(pty) = self.pty.as_ref() { pty.scroll(lines); }
    }

    pub fn key(&self, event: &winit::event::KeyEvent, ctrl: bool) {
        if ctrl {
            if let PhysicalKey::Code(code) = event.physical_key {
                if let Some(byte) = crate::auxterm::ctrl_byte(code) {
                    self.send(&[byte]);
                    return;
                }
            }
        }
        let sequence: Option<&[u8]> = match event.logical_key {
            Key::Named(NamedKey::Enter) => Some(b"\r"),
            Key::Named(NamedKey::Tab) => Some(b"\t"),
            Key::Named(NamedKey::Backspace) => Some(b"\x7f"),
            Key::Named(NamedKey::Escape) => Some(b"\x1b"),
            Key::Named(NamedKey::ArrowUp) => Some(if self.app_cursor { b"\x1bOA" } else { b"\x1b[A" }),
            Key::Named(NamedKey::ArrowDown) => Some(if self.app_cursor { b"\x1bOB" } else { b"\x1b[B" }),
            Key::Named(NamedKey::ArrowRight) => Some(if self.app_cursor { b"\x1bOC" } else { b"\x1b[C" }),
            Key::Named(NamedKey::ArrowLeft) => Some(if self.app_cursor { b"\x1bOD" } else { b"\x1b[D" }),
            Key::Named(NamedKey::Home) => Some(b"\x1b[H"),
            Key::Named(NamedKey::End) => Some(b"\x1b[F"),
            Key::Named(NamedKey::Delete) => Some(b"\x1b[3~"),
            Key::Named(NamedKey::PageUp) => Some(b"\x1b[5~"),
            Key::Named(NamedKey::PageDown) => Some(b"\x1b[6~"),
            _ => event.text.as_ref().map(|text| text.as_bytes()),
        };
        if let Some(bytes) = sequence.filter(|bytes| !bytes.is_empty()) { self.send(bytes); }
    }
}
