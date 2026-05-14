//! tmuxify — iced 0.14 chrome over a native wgpu cell renderer.
//!
//! R2 architecture:
//! - Chrome (sidebar / window tabs / onboarding / recents) stays iced widgets.
//! - The terminal body is `iced::widget::Shader` driving `cell_shader::TerminalPipeline`,
//!   which rasterises glyphs through cosmic-text + a hand-rolled wgpu atlas
//!   (glyphon is unusable: wgpu 27 vs 29 mismatch with iced 0.14).
//! - Each session owns a `tmux-bridge` -CC subprocess. Output flows from
//!   crossbeam channels through a tokio mpsc into the iced subscription.

mod cell_shader;

use std::path::PathBuf;
use std::sync::Arc;

use iced::advanced::input_method::{self, InputMethod, Purpose};
use iced::advanced::widget::{tree, Tree, Widget};
use iced::advanced::{layout, mouse, overlay, renderer, Clipboard, Layout, Shell};
use iced::keyboard::{self, Key, Modifiers};
use iced::widget::shader::{self, Shader};
use iced::widget::{button, column, container, row, text, Space};
use iced::{
    color, event, stream, window, Background, Border, Color, Element, Event, Font, Length,
    Padding, Rectangle, Size, Subscription, Task, Theme, Vector,
};
use tmux_bridge::{
    screen::{Cell as TbCell, Row as TbRow},
    ScreenUpdate, StartOptions, TmuxEvent, TmuxSession,
};
use tokio::sync::mpsc;

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

const TERM_BG: [f32; 4] = [0x1c as f32 / 255.0, 0x20 as f32 / 255.0, 0x26 as f32 / 255.0, 1.0];
const TERM_FG: [f32; 4] = [0xea as f32 / 255.0, 0xee as f32 / 255.0, 0xf4 as f32 / 255.0, 1.0];

// Initial pane grid — re-sized once the Shader widget reports back its
// actual bounds. Picking 89×28 matches native's StartOptions default.
const DEFAULT_COLS: u16 = 89;
const DEFAULT_ROWS: u16 = 28;

// Cell metrics in logical pixels. Tracks `cell_shader::FONT_SIZE`/
// `LINE_HEIGHT`. Used by resize logic to translate iced window pixels
// into a (cols, rows) target. Inside the shader widget the actual cell
// size is derived from bounds/cols for clean pixel alignment.
const CELL_W_PX: f32 = 8.4;
const CELL_H_PX: f32 = 18.0;

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
    let mut tasks: Vec<Task<Message>> = Vec::new();

    match std::env::var("TMUXIFY_AUTOOPEN") {
        Ok(path) => {
            eprintln!("[tmuxify] autoopen: {path}");
            tasks.push(Task::done(Message::OpenPath(PathBuf::from(path))));
        }
        Err(e) => {
            eprintln!("[tmuxify] no autoopen ({e:?})");
        }
    }

    // AUTOSEND — push a Send message after N ms. Used by the verify loop
    // to drive a scripted scenario (e.g. type `claude` and watch). The
    // text travels through `Message::AutoSend` → write_active, so it
    // honours the active pane's tmux session like real typing would.
    if let (Ok(text), Ok(ms)) = (
        std::env::var("TMUXIFY_AUTOSEND"),
        std::env::var("TMUXIFY_AUTOSEND_MS"),
    ) {
        if let Ok(ms) = ms.parse::<u64>() {
            eprintln!("[tmuxify] autosend in {ms}ms: {text:?}");
            tasks.push(Task::perform(
                async move {
                    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                    text
                },
                Message::AutoSend,
            ));
        }
    }

    // AUTOCAPTURE — `screencapture -x` of the whole screen after N ms.
    // macOS gates programmatic window-only capture behind TCC, so we
    // accept that and shoot the full screen; the iced window's normal
    // dock position is enough for visual diffing.
    if let Ok(ms) = std::env::var("TMUXIFY_AUTOCAPTURE_MS").and_then(|s| {
        s.parse::<u64>().map_err(|_| std::env::VarError::NotPresent)
    }) {
        let path = std::env::var("TMUXIFY_AUTOCAPTURE_PATH")
            .unwrap_or_else(|_| "/tmp/tmuxify-iced-r2.png".into());
        eprintln!("[tmuxify] autocapture in {ms}ms → {path}");
        tasks.push(Task::perform(
            async move {
                tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                // Raise our own window so the screencapture grabs the
                // iced surface rather than whatever editor was on top.
                // The PID lookup happens via `pgrep -lf tmuxify` because
                // osascript wants either bundle id or a known process
                // name — and our binary is just `tmuxify`.
                let pid = std::process::id();
                let _ = std::process::Command::new("osascript")
                    .args([
                        "-e",
                        &format!(
                            "tell application \"System Events\" to set frontmost of (first process whose unix id is {pid}) to true"
                        ),
                    ])
                    .status();
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                let _ = std::process::Command::new("screencapture")
                    .args(["-x", "-t", "png", &path])
                    .status();
                eprintln!("[tmuxify] captured {path}");
            },
            |_| Message::AutoCaptured,
        ));
    }

    Task::batch(tasks)
}

// === State ================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct SessionId(u64);

/// Per-session live data. The `tmux` field is wrapped in an `Arc<Mutex>`
/// only because Iced wants `&self` in `view`/`subscription` — the actual
/// writer is the subscription thread plus the update handler, both holding
/// `&mut App`.
struct Session {
    id: SessionId,
    name: String,
    cwd: PathBuf,
    tmux: Arc<TmuxSession>,
    /// Currently displayed pane id. Filled in once the first %output
    /// names a pane; before that we render an empty grid.
    active_pane: Option<String>,
    cells: Arc<Vec<Vec<TbCell>>>,
    cols: u16,
    rows: u16,
    cursor_row: u16,
    cursor_col: u16,
    cursor_visible: bool,
    /// Latest parsed pane tree from `%layout-change`. None = haven't
    /// received one yet (single-pane). Currently logged + reflected via
    /// pane count in `display_name`; full multi-pane rendering pending.
    layout: Option<tmux_bridge::Layout>,
}

struct App {
    sessions: Vec<Session>,
    active_idx: Option<usize>,
    recents: Vec<PathBuf>,
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
    ScreenUpdate(SessionId, Arc<ScreenUpdate>),
    SessionGone(SessionId),
    Event(Event),
    /// Window resized — recompute (cols, rows) and resize every session's
    /// tmux client. Sent from the global `event::listen_with` and from
    /// the very first frame to size newly-opened sessions to fit.
    WindowResized(u32, u32),
    /// Pane layout changed in tmux's outer window. Carries the raw layout
    /// string and the parsed pane tree so view code can decide what to
    /// draw. The split tree currently only logs — actual multi-pane
    /// rendering is queued for the next round.
    LayoutChanged(SessionId, String, Arc<tmux_bridge::Layout>),
    /// Catch-all from the tmux event channel. We forward non-Exit events
    /// through the bus so LayoutChange / NewWindow / etc. can be acted
    /// on; unknown variants are logged and dropped.
    TmuxEvent(SessionId, Arc<TmuxEvent>),
    AutoSend(String),
    AutoCaptured,
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

            Message::ScreenUpdate(sid, update) => {
                if let Some(s) = self.sessions.iter_mut().find(|s| s.id == sid) {
                    if s.active_pane.is_none() {
                        s.active_pane = Some(update.pane_id.clone());
                    }
                    // Only let the currently-focused pane drive the
                    // visible grid. Backgrounded panes still get their
                    // vt100 state advanced inside tmux-bridge — we just
                    // don't paint them.
                    if s.active_pane.as_deref() == Some(update.pane_id.as_str()) {
                        ensure_grid_size(&mut s.cells, &mut s.cols, &mut s.rows, update.cols, update.rows);
                        let cells = Arc::make_mut(&mut s.cells);
                        for (r, row) in &update.dirty {
                            if let Some(dst) = cells.get_mut(*r as usize) {
                                *dst = row.clone();
                            }
                        }
                        s.cursor_row = update.cursor_row;
                        s.cursor_col = update.cursor_col;
                        s.cursor_visible = update.cursor_visible;
                        if let Some(t) = update.title.as_ref() {
                            s.name = t.clone();
                        }
                    }
                }
                Task::none()
            }

            Message::SessionGone(sid) => {
                if let Some(idx) = self.sessions.iter().position(|s| s.id == sid) {
                    self.sessions.remove(idx);
                    self.active_idx = if self.sessions.is_empty() {
                        None
                    } else {
                        Some(idx.min(self.sessions.len() - 1))
                    };
                }
                Task::none()
            }

            Message::Event(Event::Window(window::Event::FileDropped(path))) => {
                self.open_session(path)
            }
            Message::Event(Event::Window(window::Event::Resized(size))) => {
                Task::done(Message::WindowResized(
                    size.width as u32,
                    size.height as u32,
                ))
            }
            Message::WindowResized(w, h) => {
                // Strip chrome before mapping to cells: sidebar on the
                // left, the window-tabs bar above the terminal body.
                let term_w = (w as f32 - SIDEBAR_W).max(0.0);
                let term_h = (h as f32 - WINDOW_TAB_H).max(0.0);
                let cols = (term_w / CELL_W_PX).floor().max(20.0) as u16;
                let rows = (term_h / CELL_H_PX).floor().max(5.0) as u16;
                for s in &self.sessions {
                    let _ = s.tmux.resize_client(cols, rows);
                }
                Task::none()
            }
            Message::AutoSend(text) => {
                self.write_active(text.into_bytes());
                self.write_active(vec![b'\r']);
                Task::none()
            }
            Message::AutoCaptured => Task::none(),

            Message::TmuxEvent(sid, ev) => {
                match ev.as_ref() {
                    TmuxEvent::LayoutChange { window_id, layout } => {
                        // tmux 3.x sends `%layout-change <window> <main>
                        // <visible> <flags>`. The bridge captures the
                        // whole tail as `layout`; only the first token
                        // is the active layout the user is currently
                        // looking at — the rest is the saved layout and
                        // a `*` / flag word.
                        let first = layout.split_whitespace().next().unwrap_or(layout);
                        eprintln!(
                            "[tmuxify] layout-change sid={} window={} layout={}",
                            sid.0, window_id, first
                        );
                        match tmux_bridge::parse_layout(first) {
                            Ok(parsed) => {
                                let parsed = Arc::new(parsed);
                                return Task::done(Message::LayoutChanged(
                                    sid,
                                    layout.clone(),
                                    parsed,
                                ));
                            }
                            Err(e) => {
                                eprintln!("[tmuxify] layout parse failed: {e}");
                            }
                        }
                    }
                    other => {
                        eprintln!("[tmuxify] tmux event sid={}: {:?}", sid.0, other);
                    }
                }
                Task::none()
            }
            Message::LayoutChanged(sid, _raw, parsed) => {
                if let Some(s) = self.sessions.iter_mut().find(|s| s.id == sid) {
                    let pane_count = count_panes(&parsed);
                    eprintln!(
                        "[tmuxify] session {} now has {} pane(s)",
                        sid.0, pane_count
                    );
                    // We can only own one `Layout` per session; clone out
                    // of the Arc so subsequent mutations (when we start
                    // tracking visible-pane focus) stay private.
                    s.layout = Some((*parsed).clone());
                }
                Task::none()
            }
            Message::Event(Event::InputMethod(input_method::Event::Commit(text))) => {
                self.write_active(text.into_bytes());
                Task::none()
            }
            // Cmd+D → ask tmux's control-mode server to split the active
            // window. The earlier implementation wrote `[0x02, '%']` into
            // the pane's pty — that's the Ctrl-B prefix sequence, which
            // only does anything if the user is running a *nested* tmux
            // inside the pane. Going through `send_cmd` talks to the
            // outer tmux server directly so a plain shell pane splits.
            Message::Event(Event::Keyboard(keyboard::Event::KeyPressed {
                key: Key::Character(ref s),
                modifiers,
                ..
            })) if modifiers == Modifiers::COMMAND && s.eq_ignore_ascii_case("d") => {
                if let Some(sess) = self.active_idx.and_then(|i| self.sessions.get(i)) {
                    let _ = sess.tmux.send_cmd("split-window -h");
                }
                Task::none()
            }
            // Plain typing — translate to a `send-keys -H` request. Only
            // covers ASCII for now; IME handles CJK above.
            Message::Event(Event::Keyboard(keyboard::Event::KeyPressed {
                key: Key::Character(ref s),
                modifiers,
                ..
            })) if modifiers.is_empty() || modifiers == Modifiers::SHIFT => {
                self.write_active(s.as_bytes().to_vec());
                Task::none()
            }
            Message::Event(Event::Keyboard(keyboard::Event::KeyPressed {
                key: Key::Named(n),
                ..
            })) => {
                if let Some(bytes) = named_key_bytes(n) {
                    self.write_active(bytes);
                }
                Task::none()
            }
            Message::Event(_) => Task::none(),
        }
    }

    fn write_active(&mut self, bytes: Vec<u8>) {
        let Some(s) = self.active_idx.and_then(|i| self.sessions.get(i)) else {
            return;
        };
        let hex = hex_encode(&bytes);
        let _ = s.tmux.send_keys_hex(s.active_pane.as_deref(), &hex);
    }

    fn open_session(&mut self, path: PathBuf) -> Task<Message> {
        let id = SessionId(self.next_session_id);
        self.next_session_id += 1;
        eprintln!("[tmuxify] open_session id={} path={:?}", id.0, path);

        let cwd_s = path.to_string_lossy().to_string();
        let session_name = tmux_session_name(&path);
        let opts = StartOptions {
            cwd: Some(&cwd_s),
            session_name: Some(&session_name),
            socket_name: Some("iced-poc"),
            cols: DEFAULT_COLS,
            rows: DEFAULT_ROWS,
            ..Default::default()
        };
        let tmux = match TmuxSession::start(opts) {
            Ok(t) => Arc::new(t),
            Err(e) => {
                eprintln!("[tmuxify] tmux spawn failed: {e}");
                return Task::none();
            }
        };
        spawn_bridge_threads(id, tmux.clone());

        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("session")
            .to_string();
        let cells = Arc::new(blank_grid(DEFAULT_COLS, DEFAULT_ROWS));

        self.sessions.push(Session {
            id,
            name,
            cwd: path.clone(),
            tmux,
            active_pane: None,
            cells,
            cols: DEFAULT_COLS,
            rows: DEFAULT_ROWS,
            cursor_row: 0,
            cursor_col: 0,
            cursor_visible: true,
            layout: None,
        });
        self.active_idx = Some(self.sessions.len() - 1);
        self.push_recent(path);
        Task::none()
    }
}

fn blank_grid(cols: u16, rows: u16) -> Vec<Vec<TbCell>> {
    (0..rows)
        .map(|_| (0..cols).map(|_| TbCell::blank()).collect())
        .collect()
}

fn ensure_grid_size(
    cells: &mut Arc<Vec<Vec<TbCell>>>,
    cur_cols: &mut u16,
    cur_rows: &mut u16,
    new_cols: u16,
    new_rows: u16,
) {
    if *cur_cols == new_cols && *cur_rows == new_rows && cells.len() == new_rows as usize {
        return;
    }
    *cur_cols = new_cols;
    *cur_rows = new_rows;
    *cells = Arc::new(blank_grid(new_cols, new_rows));
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
        // Show a "▌ N panes" hint when the user has split. Once the
        // shader widget grows multi-pane rendering this will become a
        // proper pane picker; for now it's the visible proof that
        // %layout-change is being consumed.
        let pane_hint = match s.layout.as_ref().map(count_panes).unwrap_or(1) {
            n if n > 1 => format!("· {n} panes"),
            _ => String::new(),
        };
        let tab = container(
            row![
                text("main").size(12).color(TEXT_PRI),
                Space::new().width(8),
                text("·").size(11).color(TEXT_MUT),
                Space::new().width(8),
                text(s.name.clone()).size(11).color(TEXT_MUT),
                Space::new().width(8),
                text(pane_hint).size(11).color(ACCENT),
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
        let program = TerminalProgram {
            cells: s.cells.clone(),
            cols: s.cols,
            rows: s.rows,
            cursor_row: s.cursor_row,
            cursor_col: s.cursor_col,
            cursor_visible: s.cursor_visible,
        };
        let shader_widget: Element<'a, Message> = Shader::new(program)
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        Element::new(ImeHost { inner: shader_widget })
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
        let events = event::listen_with(|ev, _status, _wid| match ev {
            Event::Window(window::Event::FileDropped(_))
            | Event::Window(window::Event::Resized(_))
            | Event::InputMethod(_)
            | Event::Keyboard(keyboard::Event::KeyPressed { .. }) => Some(Message::Event(ev)),
            _ => None,
        });

        Subscription::batch([events, bus_subscription()])
    }
}

/// Global pipe out of every session's background threads. iced 0.14's
/// `Subscription::run_with` wants a `fn` (not `Fn`) — it cannot capture
/// per-session state by closure — so we ship every session through a
/// single static mpsc and let one subscription drain it.
///
/// `open_session` pushes the receiver end of a oneshot bootstrap into
/// `BOOTSTRAP_TX` after spawning its bridge threads; those threads
/// forward each `ScreenUpdate` into `BUS_TX` directly. The single
/// subscription below pulls from `BUS_RX` forever.
use std::sync::OnceLock;
use tokio::sync::Mutex as AsyncMutex;

static BUS_TX: OnceLock<mpsc::UnboundedSender<Message>> = OnceLock::new();
static BUS_RX: OnceLock<AsyncMutex<mpsc::UnboundedReceiver<Message>>> = OnceLock::new();

fn bus_init() -> mpsc::UnboundedSender<Message> {
    BUS_TX
        .get_or_init(|| {
            let (tx, rx) = mpsc::unbounded_channel::<Message>();
            let _ = BUS_RX.set(AsyncMutex::new(rx));
            tx
        })
        .clone()
}

/// Spawn the two blocking forwarder threads for a freshly opened session.
fn spawn_bridge_threads(sid: SessionId, tmux: Arc<TmuxSession>) {
    let bus = bus_init();
    let screens = tmux.screens.clone();
    let bus_screens = bus.clone();
    std::thread::spawn(move || {
        while let Ok(update) = screens.recv() {
            if bus_screens
                .send(Message::ScreenUpdate(sid, Arc::new(update)))
                .is_err()
            {
                return;
            }
        }
        let _ = bus_screens.send(Message::SessionGone(sid));
    });
    let events = tmux.events.clone();
    let bus_events = bus.clone();
    std::thread::spawn(move || {
        while let Ok(ev) = events.recv() {
            if matches!(ev, TmuxEvent::Exit) {
                let _ = bus_events.send(Message::SessionGone(sid));
                return;
            }
            // Forward everything else (LayoutChange, WindowAdd, etc.) so
            // the iced update loop can decide what to do with it.
            if bus_events.send(Message::TmuxEvent(sid, Arc::new(ev))).is_err() {
                return;
            }
        }
        let _ = bus_events.send(Message::SessionGone(sid));
    });
}

fn bus_subscription() -> Subscription<Message> {
    Subscription::run(|| {
        // Initialise on first run. Subsequent restarts (after panic /
        // hot-reload) re-take the same receiver and pick up where we
        // left off.
        let _ = bus_init();
        stream::channel::<Message>(64, |mut output: futures::channel::mpsc::Sender<Message>| async move {
            let rx = BUS_RX.get().expect("bus initialised above");
            let mut guard = rx.lock().await;
            while let Some(msg) = guard.recv().await {
                if output.try_send(msg).is_err() {
                    return;
                }
            }
        })
    })
}

// === Shader program ======================================================

struct TerminalProgram {
    cells: Arc<Vec<Vec<TbCell>>>,
    cols: u16,
    rows: u16,
    cursor_row: u16,
    cursor_col: u16,
    cursor_visible: bool,
}

impl<Message> shader::Program<Message> for TerminalProgram {
    type State = ();
    type Primitive = cell_shader::TerminalPrimitive;

    fn draw(
        &self,
        _state: &Self::State,
        _cursor: mouse::Cursor,
        bounds: Rectangle,
    ) -> Self::Primitive {
        // Pick a font size that fills the widget at the current grid
        // size. Cell width = bounds.w / cols; cell height = bounds.h /
        // rows. Font size tracks cell height directly.
        let cell_w = if self.cols > 0 {
            bounds.width / self.cols as f32
        } else {
            cell_shader::FONT_SIZE * 0.6
        };
        let cell_h = if self.rows > 0 {
            bounds.height / self.rows as f32
        } else {
            cell_shader::LINE_HEIGHT
        };
        let font_size = (cell_h * 0.78).max(8.0);

        cell_shader::TerminalPrimitive {
            cells: self.cells.clone(),
            cols: self.cols,
            rows: self.rows,
            cursor_row: self.cursor_row,
            cursor_col: self.cursor_col,
            cursor_visible: self.cursor_visible,
            bg_color: TERM_BG,
            fg_color: TERM_FG,
            cell_w,
            cell_h,
            font_size,
        }
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

fn tmux_session_name(path: &PathBuf) -> String {
    let leaf = path.file_name().and_then(|n| n.to_str()).unwrap_or("tmuxify");
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

/// `tmux send-keys -H` expects one hex byte per argument-token, separated
/// by whitespace. Concatenated `\x636c61756465` reads as a single oversize
/// hex literal and tmux silently drops the input — caught the first time
/// AUTOSEND fired and "claude" never showed up at the prompt.
fn count_panes(layout: &tmux_bridge::Layout) -> usize {
    layout.leaves().len()
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(&format!("{:02x}", b));
    }
    s
}

/// Map iced named keys to terminal control sequences. Covers Enter / Tab
/// / arrows / Esc / Backspace — enough to drive claude + a shell prompt.
/// Add more as gaps show up.
fn named_key_bytes(named: keyboard::key::Named) -> Option<Vec<u8>> {
    use keyboard::key::Named;
    match named {
        Named::Enter => Some(b"\r".to_vec()),
        Named::Tab => Some(b"\t".to_vec()),
        Named::Backspace => Some(b"\x7f".to_vec()),
        Named::Escape => Some(b"\x1b".to_vec()),
        Named::ArrowUp => Some(b"\x1b[A".to_vec()),
        Named::ArrowDown => Some(b"\x1b[B".to_vec()),
        Named::ArrowRight => Some(b"\x1b[C".to_vec()),
        Named::ArrowLeft => Some(b"\x1b[D".to_vec()),
        Named::Home => Some(b"\x1b[H".to_vec()),
        Named::End => Some(b"\x1b[F".to_vec()),
        Named::PageUp => Some(b"\x1b[5~".to_vec()),
        Named::PageDown => Some(b"\x1b[6~".to_vec()),
        Named::Delete => Some(b"\x1b[3~".to_vec()),
        _ => None,
    }
}

// === ImeHost — wraps the Shader widget so winit turns IME on =============

struct ImeHost<'a, Msg> {
    inner: Element<'a, Msg>,
}

impl<'a, Msg> Widget<Msg, Theme, iced::Renderer> for ImeHost<'a, Msg> {
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
        shell: &mut Shell<'_, Msg>,
        viewport: &Rectangle,
    ) {
        self.inner
            .as_widget_mut()
            .update(tree, event, layout, cursor, renderer, clipboard, shell, viewport);

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
    ) -> Option<overlay::Element<'b, Msg, Theme, iced::Renderer>> {
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

// Suppress unused-import warning until we wire row-level coalescing.
#[allow(dead_code)]
fn _kept_for_future(_row: &TbRow) {}
