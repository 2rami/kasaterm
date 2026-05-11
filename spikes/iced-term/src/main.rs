//! iced spike — single tmux pane rendered as a monospace grid.
//!
//! No colors, no IME input; goal is to evaluate iced's text rendering
//! quality and ergonomics for a terminal grid. A real tmux session is
//! spawned in `cwd = $HOME` and the first pane discovered is shown.

use std::time::Duration;

use iced::widget::{button, column, container, row, scrollable, text};
use iced::{Element, Font, Length, Subscription, Task, Theme};

use tmux_bridge::{Cell, Color, ScreenUpdate, StartOptions, TmuxSession};

const MONO: Font = Font::with_name("monospace");

#[derive(Debug, Clone)]
enum Message {
    Connect,
    Tick,
    Connected(String),
    ConnectFailed(String),
}

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
                    flush_interval: Duration::from_millis(33),
                }) {
                    Ok(s) => {
                        let name = s.session_name.clone();
                        self.session = Some(s);
                        Task::done(Message::Connected(name))
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

        container(column![header, grid].spacing(8).padding(8))
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn subscription(&self) -> Subscription<Message> {
        if self.session.is_some() {
            iced::time::every(Duration::from_millis(50)).map(|_| Message::Tick)
        } else {
            Subscription::none()
        }
    }
}

fn render_row(cells: &[Cell]) -> String {
    let mut s = String::with_capacity(cells.len());
    for c in cells {
        // Visualize inverse cells with a marker only if non-blank; spike-only.
        let _ = (&c.fg, &c.bg, c.bold, c.italic, c.underline, c.inverse);
        let _ = matches!(&c.fg, Color::Default);
        if c.ch.is_empty() {
            s.push(' ');
        } else {
            s.push_str(&c.ch);
        }
    }
    s
}

fn main() -> iced::Result {
    iced::application("tmuxify spike — iced", App::update, App::view)
        .subscription(App::subscription)
        .theme(|_| Theme::Dark)
        .run()
}
