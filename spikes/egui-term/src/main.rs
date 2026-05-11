//! egui spike — same scope as iced-term: spawn tmux, render first pane
//! as a monospace grid. No colors / no input yet.

use std::time::Duration;

use eframe::egui;
use tmux_bridge::{Cell, ScreenUpdate, StartOptions, TmuxSession};

struct App {
    status: String,
    session: Option<TmuxSession>,
    pane_id: Option<String>,
    grid: Vec<String>,
}

impl Default for App {
    fn default() -> Self {
        Self {
            status: "idle — click Connect".into(),
            session: None,
            pane_id: None,
            grid: vec!["(no pane yet)".into()],
        }
    }
}

impl App {
    fn connect(&mut self) {
        let cwd = std::env::var("HOME").ok();
        match TmuxSession::start(StartOptions {
            cwd: cwd.as_deref(),
            auto_run: None,
            flush_interval: Duration::from_millis(33),
        }) {
            Ok(s) => {
                self.status = format!("attached: {}", s.session_name);
                self.session = Some(s);
            }
            Err(e) => self.status = format!("failed: {e}"),
        }
    }

    fn drain(&mut self) {
        let drained: Vec<ScreenUpdate> = self
            .session
            .as_ref()
            .map(|s| s.screens.try_iter().collect())
            .unwrap_or_default();
        for u in drained {
            self.apply(u);
        }
    }

    fn apply(&mut self, u: ScreenUpdate) {
        if self.pane_id.is_none() {
            self.pane_id = Some(u.pane_id.clone());
        }
        if self.pane_id.as_deref() != Some(&u.pane_id) {
            return;
        }
        if self.grid.len() != u.rows as usize {
            self.grid = vec![" ".repeat(u.cols as usize); u.rows as usize];
        }
        for (i, row) in u.dirty {
            if (i as usize) < self.grid.len() {
                self.grid[i as usize] = render_row(&row);
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.session.is_some() {
            ctx.request_repaint_after(Duration::from_millis(50));
            self.drain();
        }
        egui::TopBottomPanel::top("bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Connect").clicked() {
                    self.connect();
                }
                ui.label(&self.status);
            });
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                let mut buf = String::new();
                for line in &self.grid {
                    buf.push_str(line);
                    buf.push('\n');
                }
                ui.add(
                    egui::TextEdit::multiline(&mut buf.as_str())
                        .font(egui::TextStyle::Monospace)
                        .desired_width(f32::INFINITY)
                        .interactive(false),
                );
            });
        });
    }
}

fn render_row(cells: &[Cell]) -> String {
    let mut s = String::with_capacity(cells.len());
    for c in cells {
        if c.ch.is_empty() {
            s.push(' ');
        } else {
            s.push_str(&c.ch);
        }
    }
    s
}

fn main() -> eframe::Result<()> {
    let opts = eframe::NativeOptions::default();
    eframe::run_native(
        "tmuxify spike — egui",
        opts,
        Box::new(|_cc| Ok(Box::<App>::default())),
    )
}
