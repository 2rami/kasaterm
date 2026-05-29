//! iced spike — single tmux pane rendered as a monospace grid + keyboard
//! input wired to `send-keys -H`. Korean IME committed text arrives via
//! `KeyPressed.text`; preedit (composing) is not displayed (tmux itself
//! does not surface it).

use std::time::Duration;
use std::fmt::Write as _;

use iced::keyboard::key::Named;
use iced::keyboard::{Key, Modifiers};
use iced::widget::{button, column, container, row, scrollable, text, text_input};
use iced::{Element, Event, Font, Length, Subscription, Task, Theme};

use tmux_bridge::{Cell, Color, ScreenUpdate, StartOptions, TmuxSession};

const MONO: Font = Font::with_name("D2Coding");

#[derive(Debug, Clone)]
enum Message {
    Connect,
    Tick,
    Connected(String),
    ConnectFailed(String),
    Event(Event),
    Input(String),
    Submit,
}

const INPUT_ID: &str = "term-input";

struct App {
    status: String,
    session: Option<TmuxSession>,
    pane_id: Option<String>,
    grid: Vec<String>,
    rows: u16,
    cols: u16,
}

impl Default for App {
    fn default() -> Self {
        Self {
            status: "idle — click Connect to spawn tmux".into(),
            session: None,
            pane_id: None,
            grid: vec!["(no pane yet)".into()],
            rows: 0,
            cols: 0,
        }
    }
}

impl App {
    fn update(&mut self, msg: Message) -> Task<Message> {
        match msg {
            Message::Connect => {
                let cwd = std::env::var("HOME").ok();
                match TmuxSession::start(StartOptions {
                    cwd: cwd.as_deref(),
                    auto_run: None,
                    session_name: None,
                    socket_name: None,
                    flush_interval: Duration::from_millis(33),
                    cols: 80,
                    rows: 24,
                }) {
                    Ok(s) => {
                        let name = s.session_name.clone();
                        self.session = Some(s);
                        Task::batch([
                            Task::done(Message::Connected(name)),
                            text_input::focus(text_input::Id::new(INPUT_ID)),
                        ])
                    }
                    Err(e) => Task::done(Message::ConnectFailed(e.to_string())),
                }
            }
            Message::Connected(name) => {
                self.status = format!("attached: {name}");
                Task::none()
            }
            Message::ConnectFailed(e) => {
                self.status = format!("failed: {e}");
                Task::none()
            }
            Message::Tick => {
                let drained: Vec<ScreenUpdate> = self
                    .session
                    .as_ref()
                    .map(|s| s.screens.try_iter().collect())
                    .unwrap_or_default();
                for update in drained {
                    self.apply_update(update);
                }
                Task::none()
            }
            Message::Event(Event::Keyboard(iced::keyboard::Event::KeyPressed {
                key,
                modified_key: _,
                modifiers,
                text,
                ..
            })) => {
                // Subscription channel handles only NAMED keys + ctrl combos.
                // Printable + IME text comes via Message::Input (TextInput widget).
                let is_named = matches!(&key, Key::Named(_));
                let is_ctrl = modifiers.control() && matches!(&key, Key::Character(_));
                if is_named || is_ctrl {
                    self.handle_key(key, modifiers, text.map(|s| s.to_string()));
                }
                Task::none()
            }
            Message::Event(_) => Task::none(),
            Message::Input(s) => {
                if !s.is_empty() {
                    if let Some(session) = &self.session {
                        let hex = bytes_to_hex(s.as_bytes());
                        self.status = format!("input={:?} → {}", s, hex);
                        let _ = session.send_keys_hex(self.pane_id.as_deref(), &hex);
                    }
                }
                Task::none()
            }
            Message::Submit => {
                if let Some(session) = &self.session {
                    let _ = session.send_keys_hex(self.pane_id.as_deref(), "0d");
                }
                Task::none()
            }
        }
    }

    fn handle_key(&mut self, key: Key, mods: Modifiers, text: Option<String>) {
        let Some(session) = &self.session else { return };
        let bytes = key_to_bytes(&key, mods, text.as_deref());
        let hex = bytes_to_hex(&bytes);
        self.status = format!(
            "key={:?} text={:?} mods=c{}a{}s{} → {}",
            key,
            text.as_deref().unwrap_or(""),
            mods.control() as u8,
            mods.alt() as u8,
            mods.shift() as u8,
            if hex.is_empty() { "(empty)" } else { hex.as_str() },
        );
        if bytes.is_empty() {
            return;
        }
        let pane = self.pane_id.clone();
        if let Err(e) = session.send_keys_hex(pane.as_deref(), &hex) {
            self.status = format!("send failed: {e}");
        }
    }

    fn apply_update(&mut self, u: ScreenUpdate) {
        if self.pane_id.is_none() {
            self.pane_id = Some(u.pane_id.clone());
        }
        if self.pane_id.as_deref() != Some(&u.pane_id) {
            return;
        }
        if self.grid.len() != u.rows as usize {
            self.grid = vec![" ".repeat(u.cols as usize); u.rows as usize];
        }
        self.rows = u.rows;
        self.cols = u.cols;
        for (i, row) in u.dirty {
            if (i as usize) < self.grid.len() {
                self.grid[i as usize] = render_row(&row);
            }
        }
    }

    fn view(&self) -> Element<'_, Message> {
        let header = row![
            button("Connect").on_press(Message::Connect),
            text(self.status.as_str()),
        ]
        .spacing(12);

        let lines: Vec<Element<Message>> = self
            .grid
            .iter()
            .map(|line| text(line.clone()).font(MONO).size(14).into())
            .collect();

        let grid = scrollable(column(lines).spacing(0));

        // TextInput value is always "" — every keystroke arrives in
        // Message::Input as the new content, which we forward and reset.
        let input = text_input("type here →", "")
            .id(text_input::Id::new(INPUT_ID))
            .on_input(Message::Input)
            .on_submit(Message::Submit)
            .font(MONO)
            .size(14);

        container(column![header, grid, input].spacing(8).padding(8))
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn subscription(&self) -> Subscription<Message> {
        let events = iced::event::listen().map(Message::Event);
        if self.session.is_some() {
            let tick = iced::time::every(Duration::from_millis(50)).map(|_| Message::Tick);
            Subscription::batch([events, tick])
        } else {
            events
        }
    }
}

fn render_row(cells: &[Cell]) -> String {
    let mut s = String::with_capacity(cells.len());
    for c in cells {
        let _ = (&c.fg, &c.bg, c.bold, c.italic, c.underline, c.inverse);
        let _ = matches!(&c.fg, Color::Default);
        if c.ch == '\0' {
            s.push(' ');
        } else {
            s.push(c.ch);
        }
    }
    s
}

/// Translate an iced key + modifiers into the raw bytes a terminal expects.
/// Committed IME text (Hangul, etc.) arrives in `text` and is sent as-is.
/// Named keys map to ASCII control bytes or CSI escape sequences.
fn key_to_bytes(key: &Key, mods: Modifiers, text: Option<&str>) -> Vec<u8> {
    if let Key::Named(named) = key {
        if let Some(bytes) = named_key_bytes(*named) {
            return bytes;
        }
    }
    if mods.control() {
        if let Key::Character(ch) = key {
            if let Some(b) = ctrl_byte(ch) {
                return vec![b];
            }
        }
    }
    if let Some(t) = text {
        if !t.is_empty() {
            return t.as_bytes().to_vec();
        }
    }
    if let Key::Character(ch) = key {
        return ch.as_bytes().to_vec();
    }
    Vec::new()
}

fn named_key_bytes(n: Named) -> Option<Vec<u8>> {
    Some(match n {
        Named::Enter => vec![0x0d],
        Named::Tab => vec![0x09],
        Named::Backspace => vec![0x7f],
        Named::Escape => vec![0x1b],
        Named::Space => vec![b' '],
        Named::ArrowUp => vec![0x1b, b'[', b'A'],
        Named::ArrowDown => vec![0x1b, b'[', b'B'],
        Named::ArrowRight => vec![0x1b, b'[', b'C'],
        Named::ArrowLeft => vec![0x1b, b'[', b'D'],
        Named::Home => vec![0x1b, b'[', b'H'],
        Named::End => vec![0x1b, b'[', b'F'],
        Named::PageUp => vec![0x1b, b'[', b'5', b'~'],
        Named::PageDown => vec![0x1b, b'[', b'6', b'~'],
        Named::Delete => vec![0x1b, b'[', b'3', b'~'],
        _ => return None,
    })
}

fn ctrl_byte(s: &str) -> Option<u8> {
    let c = s.chars().next()?;
    if c.is_ascii_alphabetic() {
        Some((c.to_ascii_lowercase() as u8) - b'a' + 1)
    } else {
        match c {
            '@' => Some(0x00),
            '[' => Some(0x1b),
            '\\' => Some(0x1c),
            ']' => Some(0x1d),
            '^' => Some(0x1e),
            '_' => Some(0x1f),
            ' ' => Some(0x00),
            _ => None,
        }
    }
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let _ = write!(out, "{:02x}", b);
    }
    out
}

fn main() -> iced::Result {
    iced::application("tmuxify spike — iced", App::update, App::view)
        .subscription(App::subscription)
        .theme(|_| Theme::Dark)
        .run()
}
