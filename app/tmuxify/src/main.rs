//! tmuxify — iced 0.14 chrome over the `iced_term` terminal widget.
//!
//! Architecture ([[tmuxify-iced-pivot]] Phase 3):
//! - Chrome (sidebar / window tabs / onboarding / recents) stays iced widgets.
//! - Each session owns an `iced_term::Terminal`, which spawns its own pty
//!   (via alacritty's backend) and routes input/output through iced's runtime.
//! - tmux-bridge is retired — multi-session is just multiple terminals, and
//!   the user runs `tmux` inside whichever one they want session persistence
//!   for. Claude Code's pane-split / team-mode flow still works because tmux
//!   itself runs inside that pty.

use std::path::PathBuf;

use iced::advanced::input_method::{self, InputMethod, Purpose};
use iced::advanced::widget::{tree, Tree, Widget};
use iced::advanced::{layout, mouse, overlay, renderer, Clipboard, Layout, Shell};
use iced::keyboard::{self, Key, Modifiers};
use iced::widget::{button, column, container, row, text, Space};
use iced::{
    color, event, window, Background, Border, Color, Element, Event, Font, Length, Padding,
    Rectangle, Size, Subscription, Task, Theme, Vector,
};
use iced_term::{BackendCommand, Command as TermCommand, Terminal, TerminalView};

// === Palette ==============================================================

const BG: Color = color!(0x1c2026);
const ACCENT: Color = color!(0x5a82f3);
const TEXT_PRI: Color = color!(0xeaeef4);
const TEXT_SEC: Color = color!(0x9ba3b0);
const TEXT_MUT: Color = color!(0x606876);
const SIDEBAR_BG: Color = color!(0x191c21);

const SIDEBAR_W: f32 = 220.0;
const WINDOW_TAB_H: f32 = 36.0;
const TRAFFIC_LIGHTS_W: f32 = 80.0;

const FONT_SIZE: f32 = 13.0;
const MONO: Font = Font::with_name("Menlo");

// === Entry ================================================================

fn main() -> iced::Result {
    iced::application(App::new, App::update, App::view)
        .title(App::title)
        .subscription(App::subscription)
        .theme(theme)
        .default_font(MONO)
        .window_size(Size::new(1200.0, 760.0))
        .run()
}

fn theme(_: &App) -> Theme {
    Theme::Dark
}

fn startup_task() -> Task<Message> {
    match std::env::var("TMUXIFY_AUTOOPEN") {
        Ok(path) => {
            eprintln!("[tmuxify] autoopen: {path}");
            Task::done(Message::OpenPath(PathBuf::from(path)))
        }
        Err(e) => {
            eprintln!("[tmuxify] no autoopen ({e:?})");
            Task::none()
        }
    }
}

// === State ================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct SessionId(u64);

struct Session {
    id: SessionId,
    name: String,
    cwd: PathBuf,
    term: Terminal,
}

struct App {
    sessions: Vec<Session>,
    active_idx: Option<usize>,
    recents: Vec<PathBuf>,
    /// Source of stable session ids — never reused so subscriptions don't
    /// confuse a closed tab's stream with a freshly opened one.
    next_session_id: u64,
}

impl App {
    fn new() -> (Self, Task<Message>) {
        let app = Self {
            sessions: Vec::new(),
            active_idx: None,
            recents: Vec::new(),
            next_session_id: 1,
        };
        (app, startup_task())
    }

    fn title(&self) -> String {
        match self.active() {
            Some(s) => format!("tmuxify · {}", display_name(s)),
            None => "tmuxify".into(),
        }
    }

    fn active(&self) -> Option<&Session> {
        self.active_idx.and_then(|i| self.sessions.get(i))
    }

    fn push_recent(&mut self, path: PathBuf) {
        self.recents.retain(|p| p != &path);
        self.recents.insert(0, path);
        self.recents.truncate(8);
    }
}

#[derive(Debug, Clone)]
enum Message {
    PickFolder,
    FolderPicked(Option<PathBuf>),
    OpenPath(PathBuf),
    SelectTab(usize),
    CloseTab(usize),
    Terminal(SessionId, iced_term::Event),
    Event(Event),
}

// === Update ===============================================================

impl App {
    fn update(&mut self, msg: Message) -> Task<Message> {
        match msg {
            Message::PickFolder => Task::perform(pick_folder(), Message::FolderPicked),

            Message::FolderPicked(None) => Task::none(),
            Message::FolderPicked(Some(path)) | Message::OpenPath(path) => self.open_session(path),

            Message::SelectTab(idx) => {
                if idx < self.sessions.len() {
                    self.active_idx = Some(idx);
                }
                Task::none()
            }

            Message::CloseTab(idx) => {
                if idx < self.sessions.len() {
                    self.sessions.remove(idx);
                    self.active_idx = if self.sessions.is_empty() {
                        None
                    } else {
                        Some(idx.min(self.sessions.len() - 1))
                    };
                }
                Task::none()
            }

            Message::Terminal(sid, iced_term::Event::BackendCall(_, cmd)) => {
                if let Some(s) = self.sessions.iter_mut().find(|s| s.id == sid) {
                    use iced_term::actions::Action;
                    match s.term.handle(iced_term::Command::ProxyToBackend(cmd)) {
                        Action::Shutdown => {
                            // Backend exited — drop the tab.
                            if let Some(idx) = self.sessions.iter().position(|s| s.id == sid) {
                                self.sessions.remove(idx);
                                self.active_idx = if self.sessions.is_empty() {
                                    None
                                } else {
                                    Some(idx.min(self.sessions.len() - 1))
                                };
                            }
                        }
                        Action::ChangeTitle(t) => {
                            s.name = t;
                        }
                        _ => {}
                    }
                }
                Task::none()
            }

            Message::Event(Event::Window(window::Event::FileDropped(path))) => {
                self.open_session(path)
            }
            // OS-level IME committed text (Hangul, etc). iced_term's own
            // KeyPressed handler only forwards `Key::Character` payloads, so
            // composed CJK characters never reach it — we intercept the
            // commit here and write the bytes straight to the active pty.
            Message::Event(Event::InputMethod(input_method::Event::Commit(text))) => {
                self.write_active(text.into_bytes());
                Task::none()
            }
            // Cmd+D → split the active session's current tmux window. Only
            // works if the user has tmux running inside that pty (so claude
            // team-mode is one `tmux` invocation away from working).
            Message::Event(Event::Keyboard(keyboard::Event::KeyPressed {
                key: Key::Character(ref s),
                modifiers,
                ..
            })) if modifiers == Modifiers::COMMAND && s.eq_ignore_ascii_case("d") => {
                // tmux prefix is C-b by default. Sequence: C-b, then "%" for
                // a vertical split.
                self.write_active(vec![0x02, b'%']);
                Task::none()
            }
            Message::Event(_) => Task::none(),
        }
    }

    /// Pump raw bytes into the active session's pty. Used for IME commits
    /// and our app-level shortcuts (Cmd+D etc.). Returns silently when there
    /// is no active session.
    fn write_active(&mut self, bytes: Vec<u8>) {
        if let Some(idx) = self.active_idx {
            if let Some(s) = self.sessions.get_mut(idx) {
                let _ = s
                    .term
                    .handle(TermCommand::ProxyToBackend(BackendCommand::Write(bytes)));
            }
        }
    }

    fn open_session(&mut self, path: PathBuf) -> Task<Message> {
        let id = SessionId(self.next_session_id);
        self.next_session_id += 1;
        eprintln!("[tmuxify] open_session id={} path={:?}", id.0, path);

        // Boot straight into `tmux` so multi-pane / team-mode just works. We
        // use `new-session -A` to attach to an existing session with the same
        // name when present, otherwise create one. The session name is a
        // stable slug derived from the folder path so re-opening the same
        // folder reattaches rather than spawning a duplicate session.
        let session_name = tmux_session_name(&path);
        let settings = iced_term::settings::Settings {
            font: iced_term::settings::FontSettings {
                size: FONT_SIZE,
                font_type: MONO,
                ..Default::default()
            },
            theme: iced_term::settings::ThemeSettings::default(),
            backend: iced_term::settings::BackendSettings {
                program: "tmux".into(),
                args: vec![
                    "new-session".into(),
                    "-A".into(),
                    "-s".into(),
                    session_name,
                ],
                working_directory: Some(path.clone()),
                ..Default::default()
            },
        };

        match Terminal::new(id.0 as u64, settings) {
            Ok(term) => {
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("session")
                    .to_string();
                self.sessions.push(Session { id, name, cwd: path.clone(), term });
                self.active_idx = Some(self.sessions.len() - 1);
                self.push_recent(path);
            }
            Err(e) => {
                eprintln!("failed to spawn terminal: {e}");
            }
        }
        Task::none()
    }
}

// === View =================================================================

impl App {
    fn view(&self) -> Element<'_, Message> {
        let body: Element<Message> = match self.active() {
            None => self.onboarding(),
            Some(s) => column![self.window_tabs(s), self.terminal_view(s)]
                .width(Length::Fill)
                .height(Length::Fill)
                .into(),
        };

        let root = row![self.sidebar(), body]
            .width(Length::Fill)
            .height(Length::Fill);

        container(root)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_| container::Style {
                background: Some(Background::Color(BG)),
                text_color: Some(TEXT_PRI),
                ..Default::default()
            })
            .into()
    }

    fn sidebar(&self) -> Element<'_, Message> {
        let mut list = column![
            Space::new().height(TRAFFIC_LIGHTS_W * 0.45),
            row![
                text("sessions").size(11).color(TEXT_MUT),
                Space::new().width(Length::Fill),
                self.new_session_button(),
            ]
            .align_y(iced::Alignment::Center)
            .padding(Padding::from([0, 12])),
            Space::new().height(8),
        ];

        if self.sessions.is_empty() {
            list = list.push(
                container(text("no sessions yet").size(11).color(TEXT_MUT))
                    .padding(Padding::from([6, 12]))
                    .width(Length::Fill),
            );
        } else {
            for (i, s) in self.sessions.iter().enumerate() {
                list = list.push(self.session_card(i, s));
                list = list.push(Space::new().height(2));
            }
        }

        container(list)
            .width(Length::Fixed(SIDEBAR_W))
            .height(Length::Fill)
            .style(|_| container::Style {
                background: Some(Background::Color(SIDEBAR_BG)),
                ..Default::default()
            })
            .into()
    }

    fn session_card(&self, idx: usize, s: &Session) -> Element<'_, Message> {
        let active = self.active_idx == Some(idx);
        let title = display_name(s);
        let sub = collapse_home(&s.cwd);

        let inner = row![
            column![
                text(title).size(13).color(TEXT_PRI),
                Space::new().height(2),
                text(sub).size(10).color(TEXT_MUT),
            ],
            Space::new().width(Length::Fill),
            button(text("×").size(12).color(TEXT_MUT))
                .on_press(Message::CloseTab(idx))
                .padding(Padding::from([0, 4]))
                .style(|_, status| close_button_style(status)),
        ]
        .align_y(iced::Alignment::Center);

        button(container(inner).padding(Padding::from([8, 12])).width(Length::Fill))
            .on_press(Message::SelectTab(idx))
            .style(move |_, status| session_card_style(active, status))
            .padding(0)
            .width(Length::Fill)
            .into()
    }

    fn new_session_button(&self) -> Element<'_, Message> {
        button(text("+").size(16).color(TEXT_SEC))
            .on_press(Message::PickFolder)
            .padding(Padding::from([0, 6]))
            .style(|_, status| close_button_style(status))
            .into()
    }

    fn window_tabs(&self, s: &Session) -> Element<'_, Message> {
        let tab = container(
            row![
                text("main").size(12).color(TEXT_PRI),
                Space::new().width(8),
                text("·").size(11).color(TEXT_MUT),
                Space::new().width(8),
                text(s.name.clone()).size(11).color(TEXT_MUT),
            ]
            .align_y(iced::Alignment::Center),
        )
        .padding(Padding::from([5, 12]));

        let bar = row![Space::new().width(10), tab, Space::new().width(Length::Fill)]
            .align_y(iced::Alignment::Center)
            .height(WINDOW_TAB_H);

        container(bar)
            .width(Length::Fill)
            .style(|_| container::Style {
                background: Some(Background::Color(BG)),
                ..Default::default()
            })
            .into()
    }

    fn terminal_view<'a>(&'a self, s: &'a Session) -> Element<'a, Message> {
        let sid = s.id;
        let inner: Element<'a, Message> = TerminalView::show(&s.term)
            .map(move |e| Message::Terminal(sid, e))
            .into();
        // Wrap the terminal widget so we can opt into the OS IME. iced_term
        // never calls `Shell::request_input_method`, so winit's
        // `set_ime_allowed(true)` never fires and CJK input arrives as raw
        // jamo. The wrapper just delegates everything and adds an
        // `InputMethod::Enabled` request on every redraw.
        Element::new(ImeHost { inner })
    }

    fn onboarding(&self) -> Element<'_, Message> {
        let title = text("tmuxify").size(34).color(TEXT_PRI);
        let tagline = text("drop a folder, or open one to start a session")
            .size(13)
            .color(TEXT_MUT);

        let drop_zone = container(
            column![
                text("drop a folder here").size(15).color(TEXT_SEC),
                Space::new().height(6),
                text("anywhere on the window").size(11).color(TEXT_MUT),
            ]
            .align_x(iced::Alignment::Center),
        )
        .width(Length::Fixed(460.0))
        .height(Length::Fixed(140.0))
        .align_x(iced::Alignment::Center)
        .align_y(iced::Alignment::Center)
        .style(|_| container::Style {
            background: Some(Background::Color(color!(0x22272e))),
            border: Border {
                color: color!(0x3a414c),
                width: 1.0,
                radius: 10.0.into(),
            },
            ..Default::default()
        });

        let open_btn = button(
            container(text("open a folder").size(13).color(TEXT_PRI))
                .padding(Padding::from([9, 22])),
        )
        .on_press(Message::PickFolder)
        .style(|_, status| primary_button_style(status));

        let recents: Element<Message> = if self.recents.is_empty() {
            Space::new().height(0).into()
        } else {
            let mut col = column![text("recent").size(11).color(TEXT_MUT)]
                .spacing(6)
                .width(Length::Fixed(460.0));
            for path in &self.recents {
                col = col.push(self.recent_row(path.clone()));
            }
            container(col)
                .width(Length::Fixed(460.0))
                .padding(Padding::from([16, 0]))
                .into()
        };

        let body = column![
            title,
            Space::new().height(4),
            tagline,
            Space::new().height(22),
            drop_zone,
            Space::new().height(14),
            open_btn,
            Space::new().height(8),
            recents,
        ]
        .align_x(iced::Alignment::Center);

        container(body)
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .padding(Padding::from([0, 24]))
            .into()
    }

    fn recent_row(&self, path: PathBuf) -> Element<'_, Message> {
        let label = collapse_home(&path);
        button(
            container(
                row![
                    text("›").size(13).color(TEXT_MUT),
                    Space::new().width(10),
                    text(label).size(13).color(TEXT_SEC),
                ]
                .align_y(iced::Alignment::Center),
            )
            .padding(Padding::from([8, 12]))
            .width(Length::Fill),
        )
        .on_press(Message::OpenPath(path))
        .style(|_, status| recent_row_style(status))
        .padding(0)
        .width(Length::Fill)
        .into()
    }
}

// === Subscription =========================================================

impl App {
    fn subscription(&self) -> Subscription<Message> {
        // `listen_with` is the only way to see Captured events — and our app
        // shortcuts (Cmd+D) plus IME commits arrive *after* iced_term already
        // captured the keystroke. Filter to the two cases we care about so we
        // don't double-dispatch every keystroke.
        let events = event::listen_with(|ev, _status, _wid| match ev {
            Event::Window(window::Event::FileDropped(_))
            | Event::InputMethod(_)
            | Event::Keyboard(keyboard::Event::KeyPressed { .. }) => Some(Message::Event(ev)),
            _ => None,
        });

        // Each terminal exposes its own subscription. Map them so we know
        // which session originated the event when handling.
        // `Subscription::map` requires a non-capturing closure in 0.14, so we
        // smuggle `sid` through `Subscription::with` — it becomes the first
        // element of the stream tuple and the map is plain `(sid, ev) → Msg`.
        let term_subs: Vec<_> = self
            .sessions
            .iter()
            .map(|s| {
                s.term
                    .subscription()
                    .with(s.id)
                    .map(|(sid, e)| Message::Terminal(sid, e))
            })
            .collect();

        Subscription::batch(std::iter::once(events).chain(term_subs))
    }
}

// === Styles ===============================================================

fn close_button_style(status: button::Status) -> button::Style {
    let color = match status {
        button::Status::Hovered => TEXT_PRI,
        _ => TEXT_SEC,
    };
    button::Style {
        background: None,
        text_color: color,
        border: Border::default(),
        ..Default::default()
    }
}

fn session_card_style(active: bool, status: button::Status) -> button::Style {
    let bg = if active {
        color!(0x2a3142)
    } else if matches!(status, button::Status::Hovered) {
        color!(0x21252b)
    } else {
        Color::TRANSPARENT
    };
    button::Style {
        background: Some(Background::Color(bg)),
        text_color: TEXT_PRI,
        border: Border::default(),
        ..Default::default()
    }
}

fn primary_button_style(status: button::Status) -> button::Style {
    let bg = match status {
        button::Status::Hovered => Color { a: 0.85, ..ACCENT },
        _ => ACCENT,
    };
    button::Style {
        background: Some(Background::Color(bg)),
        text_color: TEXT_PRI,
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: 6.0.into(),
        },
        ..Default::default()
    }
}

fn recent_row_style(status: button::Status) -> button::Style {
    let bg = match status {
        button::Status::Hovered => color!(0x252a31),
        _ => Color::TRANSPARENT,
    };
    button::Style {
        background: Some(Background::Color(bg)),
        text_color: TEXT_SEC,
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: 6.0.into(),
        },
        ..Default::default()
    }
}

// === Helpers ==============================================================

fn display_name(s: &Session) -> String {
    s.cwd
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.to_string())
        .unwrap_or_else(|| s.name.clone())
}

fn collapse_home(path: &PathBuf) -> String {
    let s = path.to_string_lossy().to_string();
    if let Ok(home) = std::env::var("HOME") {
        if let Some(rest) = s.strip_prefix(&home) {
            return format!("~{rest}");
        }
    }
    s
}

/// Slugify a cwd into a stable tmux session name. tmux rejects `.` and `:`
/// in names, so we replace any non-alphanumeric byte with `_`.
fn tmux_session_name(path: &PathBuf) -> String {
    let leaf = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("tmuxify");
    let mut out = String::with_capacity(leaf.len() + 8);
    out.push_str("tmuxify-");
    for ch in leaf.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    out
}

// === ImeHost — wraps the terminal view so winit turns IME on ==============

struct ImeHost<'a, Message> {
    inner: Element<'a, Message>,
}

impl<'a, Message> Widget<Message, Theme, iced::Renderer> for ImeHost<'a, Message> {
    fn size(&self) -> Size<Length> {
        self.inner.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.inner.as_widget().size_hint()
    }

    fn tag(&self) -> tree::Tag {
        self.inner.as_widget().tag()
    }

    fn state(&self) -> tree::State {
        self.inner.as_widget().state()
    }

    fn children(&self) -> Vec<Tree> {
        self.inner.as_widget().children()
    }

    fn diff(&self, tree: &mut Tree) {
        self.inner.as_widget().diff(tree);
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.inner.as_widget_mut().layout(tree, renderer, limits)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.inner
            .as_widget()
            .draw(tree, renderer, theme, style, layout, cursor, viewport);
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn iced::advanced::widget::Operation,
    ) {
        self.inner
            .as_widget_mut()
            .operate(tree, layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.inner
            .as_widget_mut()
            .update(tree, event, layout, cursor, renderer, clipboard, shell, viewport);

        // request_input_method is only honored during a RedrawRequested event —
        // we ask every frame so the IME stays armed for the active terminal.
        if matches!(event, Event::Window(window::Event::RedrawRequested(_))) {
            let bounds = layout.bounds();
            let cursor_rect = Rectangle {
                x: bounds.x + bounds.width.min(40.0),
                y: bounds.y + bounds.height - 24.0,
                width: 1.0,
                height: 18.0,
            };
            shell.request_input_method::<&str>(&InputMethod::Enabled {
                cursor: cursor_rect,
                purpose: Purpose::Terminal,
                preedit: None,
            });
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        self.inner
            .as_widget()
            .mouse_interaction(tree, layout, cursor, viewport, renderer)
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, iced::Renderer>> {
        self.inner
            .as_widget_mut()
            .overlay(tree, layout, renderer, viewport, translation)
    }
}

async fn pick_folder() -> Option<PathBuf> {
    rfd::AsyncFileDialog::new()
        .set_title("Pick a folder for the new session")
        .pick_folder()
        .await
        .map(|h| h.path().to_path_buf())
}

