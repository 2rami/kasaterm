//! 셸 칸 옆에 붙는 파일 트리. 셸이 폴더를 옮기면 따라가고, 폴더에서 Enter 를 누르면
//! 셸을 그리로 옮긴다. 짝이 없으면(`--follow` 없이) 그 자리에서 혼자 돈다.

mod git;
mod link;
mod preview;
mod tree;
mod ui;

use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
    MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

const CWD_EVERY: Duration = Duration::from_millis(500);
const TREE_EVERY: Duration = Duration::from_secs(2);
const GIT_EVERY: Duration = Duration::from_secs(3);
const NOTE_FOR: Duration = Duration::from_secs(4);
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

pub struct App {
    pub tree: tree::Tree,
    pub git: Option<git::Status>,
    pub preview: Option<(PathBuf, preview::Preview)>,
    pub show_preview: bool,
    pub filtering: bool,
    pub note: Option<(String, Instant)>,
    pub following: bool,
    /// 트리 줄이 그려진 자리(첫 줄 y, 줄 수). 마우스가 줄을 찾는 데 쓴다.
    pub tree_area: (u16, u16),
    pub offset: usize,
    /// 지난 화면에 알림이 있었나 — 알림이 저절로 사라질 때 한 번 다시 그린다.
    pub note_drawn: bool,
    shell_cwd: Option<PathBuf>,
    link: Option<(Sender<link::Ask>, Receiver<link::Answer>)>,
    git_ask: Sender<PathBuf>,
    git_answers: Receiver<Option<git::Status>>,
    last_click: Option<(usize, Instant)>,
    quit: bool,
}

impl App {
    fn note(&mut self, text: impl Into<String>) {
        self.note = Some((text.into(), Instant::now()));
    }

    pub fn shell_cwd(&self) -> Option<&std::path::Path> {
        self.shell_cwd.as_deref().filter(|_| self.following)
    }

    pub fn live_note(&self) -> Option<&str> {
        self.note.as_ref().filter(|(_, at)| at.elapsed() < NOTE_FOR).map(|(text, _)| text.as_str())
    }

    fn ask(&mut self, ask: link::Ask) -> bool {
        match &self.link {
            Some((send, _)) => send.send(ask).is_ok(),
            None => false,
        }
    }

    fn ask_git(&self) {
        let _ = self.git_ask.send(self.tree.root.clone());
    }

    /// 폴더는 셸을 그리로 옮기고 펼친다. 파일은 카사텀 미리보기로 연다.
    fn activate(&mut self) {
        let Some(row) = self.tree.selected_row().cloned() else { return };
        if row.dir {
            if !row.open {
                self.tree.set_open(&row.path, true);
            }
            if !self.ask(link::Ask::Cd(row.path.clone())) {
                self.note("따라갈 셸이 없어 펼치기만 했어요");
            }
        } else if !self.ask(link::Ask::Open(row.path.clone())) {
            self.note("카사텀 칸 안에서만 열 수 있어요");
        }
    }

    fn toggle_selected(&mut self) {
        let Some(row) = self.tree.selected_row().cloned() else { return };
        if row.dir {
            self.tree.set_open(&row.path, !row.open);
        }
    }

    fn key(&mut self, key: KeyEvent) {
        if self.filtering {
            match key.code {
                KeyCode::Esc => {
                    self.filtering = false;
                    self.tree.filter.clear();
                }
                KeyCode::Enter => self.filtering = false,
                KeyCode::Backspace => {
                    self.tree.filter.pop();
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => self.tree.filter.push(c),
                _ => return,
            }
            self.tree.rebuild();
            return;
        }
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => self.quit = true,
            KeyCode::Char('j') | KeyCode::Down => self.tree.move_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.tree.move_by(-1),
            KeyCode::PageDown => self.tree.move_by(self.tree_area.1.max(1) as isize),
            KeyCode::PageUp => self.tree.move_by(-(self.tree_area.1.max(1) as isize)),
            KeyCode::Char('g') | KeyCode::Home => self.tree.selected = 0,
            KeyCode::Char('G') | KeyCode::End => self.tree.move_by(isize::MAX / 2),
            KeyCode::Char('l') | KeyCode::Right => {
                if let Some(row) = self.tree.selected_row().cloned().filter(|row| row.dir) {
                    self.tree.set_open(&row.path, true);
                }
            }
            KeyCode::Char('h') | KeyCode::Left => self.tree.collapse_or_parent(),
            KeyCode::Char(' ') => self.toggle_selected(),
            KeyCode::Enter => self.activate(),
            KeyCode::Char('/') => self.filtering = true,
            KeyCode::Char('.') => {
                self.tree.show_hidden = !self.tree.show_hidden;
                self.tree.rebuild();
                self.preview = None;
            }
            KeyCode::Char('p') => self.show_preview = !self.show_preview,
            KeyCode::Char('r') => {
                self.tree.refresh(true);
                self.ask_git();
                self.note("다시 읽었어요");
            }
            KeyCode::Esc if !self.tree.filter.is_empty() => {
                self.tree.filter.clear();
                self.tree.rebuild();
            }
            _ => {}
        }
    }

    fn mouse(&mut self, mouse: MouseEvent) {
        let (top, rows) = self.tree_area;
        let row = (mouse.row >= top && mouse.row < top + rows).then(|| self.offset + (mouse.row - top) as usize);
        match mouse.kind {
            MouseEventKind::ScrollDown => self.tree.move_by(3),
            MouseEventKind::ScrollUp => self.tree.move_by(-3),
            MouseEventKind::Down(MouseButton::Left) => {
                let Some(at) = row.filter(|at| *at < self.tree.rows.len()) else { return };
                self.tree.selected = at;
                let double = self.last_click.is_some_and(|(last, when)| last == at && when.elapsed() < DOUBLE_CLICK);
                if double {
                    self.last_click = None;
                    self.activate();
                } else {
                    self.last_click = Some((at, Instant::now()));
                    let on_arrow = self.tree.rows[at].dir
                        && (mouse.column as usize) < self.tree.rows[at].depth * 2 + 2;
                    if on_arrow {
                        self.toggle_selected();
                    }
                }
            }
            _ => {}
        }
    }

    fn drain(&mut self) -> bool {
        let mut changed = false;
        while let Ok(status) = self.git_answers.try_recv() {
            self.git = status;
            changed = true;
        }
        let answers: Vec<link::Answer> = match &self.link {
            Some((_, answers)) => answers.try_iter().collect(),
            None => Vec::new(),
        };
        for answer in answers {
            changed = true;
            match answer {
                link::Answer::Cwd(Ok(cwd)) => {
                    self.following = true;
                    if self.shell_cwd.as_ref() != Some(&cwd) {
                        self.shell_cwd = Some(cwd.clone());
                        let root = self.tree.root.clone();
                        self.tree.reveal(&cwd);
                        if self.tree.root != root {
                            self.ask_git();
                        }
                    }
                }
                link::Answer::Cwd(Err(_)) => {
                    if self.following {
                        self.note("따라갈 셸이 닫혔어요");
                    }
                    self.following = false;
                }
                link::Answer::Cd(Err(error)) | link::Answer::Open(Err(error)) => self.note(format!("{error:#}")),
                link::Answer::Cd(Ok(())) | link::Answer::Open(Ok(())) => {}
            }
        }
        changed
    }

    fn refresh_preview(&mut self) {
        let Some(path) = self.tree.selected_path().map(|p| p.to_path_buf()) else { return };
        if self.preview.as_ref().is_some_and(|(at, _)| *at == path) {
            return;
        }
        let loaded = preview::load(&path, self.tree.show_hidden);
        self.preview = Some((path, loaded));
    }
}

fn args() -> (Option<String>, Option<PathBuf>) {
    let mut follow = None;
    let mut dir = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--follow" => follow = args.next(),
            "-h" | "--help" => {
                println!("kasa-files [--follow <칸 번호>] [폴더]\n셸 칸 옆에서 그 셸의 폴더를 따라가는 파일 트리");
                std::process::exit(0);
            }
            _ => dir = Some(PathBuf::from(arg)),
        }
    }
    (follow, dir)
}

fn main() -> anyhow::Result<()> {
    let (follow, dir) = args();
    let start = follow
        .as_deref()
        .and_then(link::first_cwd)
        .or(dir)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let start = start.canonicalize().unwrap_or(start);
    let (git_ask, git_answers) = git::spawn();
    let own = std::env::var("KASATERM_PANE_ID").ok();
    let mut app = App {
        tree: tree::Tree::new(tree::root_for(&start)),
        git: None,
        preview: None,
        show_preview: true,
        filtering: false,
        note: None,
        following: follow.is_some(),
        tree_area: (0, 0),
        offset: 0,
        note_drawn: false,
        shell_cwd: Some(start.clone()),
        link: link::spawn(follow, own),
        git_ask,
        git_answers,
        last_click: None,
        quit: false,
    };
    app.tree.reveal(&start);
    app.ask_git();

    let mut terminal = ratatui::init();
    execute!(std::io::stdout(), EnableMouseCapture)?;
    let result = run(&mut terminal, &mut app);
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

fn run(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> anyhow::Result<()> {
    let (mut cwd_at, mut tree_at, mut git_at) = (Instant::now(), Instant::now(), Instant::now());
    let mut dirty = true;
    while !app.quit {
        if dirty {
            app.refresh_preview();
            terminal.draw(|frame| ui::draw(frame, app))?;
            dirty = false;
        }
        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => app.key(key),
                Event::Mouse(mouse) => app.mouse(mouse),
                Event::FocusGained => {
                    app.tree.refresh(false);
                    app.ask_git();
                }
                _ => {}
            }
            dirty = true;
        }
        if cwd_at.elapsed() >= CWD_EVERY {
            cwd_at = Instant::now();
            app.ask(link::Ask::Cwd);
        }
        if tree_at.elapsed() >= TREE_EVERY {
            tree_at = Instant::now();
            dirty |= app.tree.refresh(false);
        }
        if git_at.elapsed() >= GIT_EVERY {
            git_at = Instant::now();
            app.ask_git();
        }
        dirty |= app.drain();
        dirty |= app.note_drawn && app.live_note().is_none();
    }
    Ok(())
}
