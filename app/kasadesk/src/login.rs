//! 고르기 화면의 계정 — 오른쪽 위 「로그인」(로그인했으면 계정 이름·「로그아웃」) 단추와, 누르면 뜨는 로그인 판. 판은
//! Google·GitHub(관문이 켠 것만, 시스템 브라우저로)와 아이디·비밀번호 칸이다. 관문 일은 `account.rs`, 여기는 그리기·판정·입력.

use kasa_castview::{contains, Rect};
use kasa_gridview::GridRenderer;
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{Key, NamedKey};

use crate::account::Credential;
use crate::text::Ui;

const TEXT: [u8; 4] = [236, 240, 246, 255];
const MUTE: [u8; 4] = [150, 160, 175, 255];
const ACCENT: [u8; 4] = [86, 132, 245, 255];
const CHIP: [u8; 4] = [30, 45, 64, 255];
const CARD: [u8; 4] = [16, 26, 40, 250];
const FIELD: [u8; 4] = [10, 18, 30, 255];
const EDGE: [u8; 4] = [64, 83, 108, 255];
const WARN: [u8; 4] = [242, 132, 120, 255];
const PAD: f32 = 28.0;
const ROW: f32 = 36.0;
const CARD_W: f32 = 400.0;

#[derive(Clone, PartialEq)]
enum Hit {
    Chip,
    Provider(String),
    Field(usize),
    Submit,
    Close,
}

/// 누름·키가 부른 일 — 관문 일은 부르는 쪽이 비동기로 한다.
pub enum Action {
    None,
    Redraw,
    /// 판을 열었다 — 켜진 로그인 방법을 물을 때.
    Opened,
    Logout,
    Provider(String),
    Password(String, String),
}

#[derive(Default)]
pub struct Login {
    pub open: bool,
    field: usize,
    id: String,
    pw: String,
    /// 관문 일을 기다리는 중 — 그동안 단추를 다시 안 받는다.
    pub busy: Option<&'static str>,
    pub error: Option<String>,
    pub providers: Vec<String>,
    hits: Vec<(Rect, Hit)>,
}

fn provider_name(p: &str) -> &str {
    match p {
        "google" => "Google",
        "github" => "GitHub",
        other => other,
    }
}

impl Login {
    /// 오른쪽 위 단추 — 고르기 머리 줄 높이에 둔다.
    pub fn paint_chip(&mut self, g: &mut GridRenderer, ui: &mut Ui, width: f32, cred: Option<&Credential>) {
        self.hits.clear();
        let text = match cred {
            Some(c) => format!("{} · 로그아웃", c.display),
            None => "로그인".to_string(),
        };
        let w = ui.measure(g, &text, 12.0, false) + 24.0;
        let r = (width - PAD - w, 24.0, w, 28.0);
        g.round_rect_fill(r.0, r.1, r.2, r.3, 6.0, if cred.is_some() { CHIP } else { ACCENT });
        ui.draw(g, r.0 + 12.0, r.1 + 7.0, &text, 12.0, TEXT, false);
        self.hits.push((r, Hit::Chip));
    }

    /// 로그인 판 — 열려 있을 때만. 단추 자리는 판정에 쓰도록 남긴다.
    pub fn paint_panel(&mut self, g: &mut GridRenderer, ui: &mut Ui, (w, h): (f32, f32)) {
        if !self.open {
            return;
        }
        self.hits.retain(|(_, hit)| *hit == Hit::Chip);
        g.rect(0.0, 0.0, w, h, [0, 0, 0, 150]);
        let inner = CARD_W - PAD * 2.0;
        let rows = self.providers.len() as f32;
        let card_h = 84.0 + rows * (ROW + 10.0) + if rows > 0.0 { 26.0 } else { 0.0 } + (ROW + 10.0) * 3.0 + if self.error.is_some() { 30.0 } else { 14.0 };
        let (cx, cy) = (((w - CARD_W) / 2.0).max(8.0), ((h - card_h) / 2.0).max(8.0));
        g.round_rect_fill(cx, cy, CARD_W, card_h, 12.0, CARD);
        g.round_rect_stroke(cx, cy, CARD_W, card_h, 12.0, 1.0, EDGE);
        ui.draw(g, cx + PAD, cy + 24.0, "카사텀 계정으로 로그인", 18.0, TEXT, true);
        ui.draw(g, cx + PAD, cy + 52.0, "카사텀이 없는 컴퓨터에서도 내 기기를 고르고 볼 수 있어요", 11.0, MUTE, false);
        let close = (cx + CARD_W - 40.0, cy + 16.0, 28.0, 28.0);
        ui.draw(g, close.0 + 9.0, close.1 + 5.0, "×", 16.0, MUTE, false);
        self.hits.push((close, Hit::Close));
        let mut y = cy + 84.0;
        for p in self.providers.clone() {
            let r = (cx + PAD, y, inner, ROW);
            g.round_rect_fill(r.0, r.1, r.2, r.3, 8.0, CHIP);
            let text = format!("{} 로 계속", provider_name(&p));
            let tw = ui.measure(g, &text, 13.0, false);
            ui.draw(g, r.0 + (r.2 - tw) / 2.0, r.1 + 10.0, &text, 13.0, TEXT, false);
            self.hits.push((r, Hit::Provider(p)));
            y += ROW + 10.0;
        }
        if rows > 0.0 {
            ui.draw(g, cx + PAD, y + 2.0, "또는 아이디로", 11.0, MUTE, false);
            y += 26.0;
        }
        let masked = "•".repeat(self.pw.chars().count());
        for (i, (hint, value)) in [("아이디", self.id.clone()), ("비밀번호", masked)].into_iter().enumerate() {
            let r = (cx + PAD, y, inner, ROW);
            g.round_rect_fill(r.0, r.1, r.2, r.3, 8.0, FIELD);
            g.round_rect_stroke(r.0, r.1, r.2, r.3, 8.0, 1.0, if self.field == i { ACCENT } else { EDGE });
            if value.is_empty() {
                ui.draw(g, r.0 + 12.0, r.1 + 10.0, hint, 13.0, MUTE, false);
            } else {
                ui.draw(g, r.0 + 12.0, r.1 + 10.0, &value, 13.0, TEXT, false);
            }
            if self.field == i && self.busy.is_none() {
                let x = r.0 + 12.0 + if value.is_empty() { 0.0 } else { ui.measure(g, &value, 13.0, false) } + 1.0;
                g.rect(x, r.1 + 9.0, 1.5, 18.0, TEXT);
            }
            self.hits.push((r, Hit::Field(i)));
            y += ROW + 10.0;
        }
        let r = (cx + PAD, y, inner, ROW);
        g.round_rect_fill(r.0, r.1, r.2, r.3, 8.0, if self.busy.is_some() { CHIP } else { ACCENT });
        let label = self.busy.unwrap_or("로그인");
        let tw = ui.measure(g, label, 13.0, false);
        ui.draw(g, r.0 + (r.2 - tw) / 2.0, r.1 + 10.0, label, 13.0, TEXT, false);
        self.hits.push((r, Hit::Submit));
        if let Some(e) = &self.error {
            ui.draw(g, cx + PAD, y + ROW + 12.0, e, 11.0, WARN, false);
        }
    }

    pub fn click(&mut self, p: (f64, f64), logged_in: bool) -> Action {
        let hit = self.hits.iter().rev().find(|(r, _)| contains(*r, p)).map(|(_, h)| h.clone());
        match hit {
            Some(Hit::Chip) if logged_in => Action::Logout,
            Some(Hit::Chip) => {
                self.open = true;
                self.error = None;
                Action::Opened
            }
            _ if !self.open => Action::None,
            _ if self.busy.is_some() => Action::None,
            Some(Hit::Close) => {
                self.close();
                Action::Redraw
            }
            Some(Hit::Field(i)) => {
                self.field = i;
                Action::Redraw
            }
            Some(Hit::Provider(p)) => self.start(Action::Provider(p)),
            Some(Hit::Submit) => self.submit(),
            None => Action::None,
        }
    }

    /// 판이 열려 있으면 키를 다 받는다(true). 칸 사이 Tab, Enter 로 로그인, Esc 로 닫기.
    pub fn key(&mut self, event: &KeyEvent) -> Option<Action> {
        if !self.open {
            return None;
        }
        if event.state != ElementState::Pressed || self.busy.is_some() {
            return Some(Action::None);
        }
        let value = if self.field == 0 { &mut self.id } else { &mut self.pw };
        Some(match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                self.close();
                Action::Redraw
            }
            Key::Named(NamedKey::Tab) => {
                self.field = 1 - self.field;
                Action::Redraw
            }
            Key::Named(NamedKey::Enter) => self.submit(),
            Key::Named(NamedKey::Backspace) => {
                value.pop();
                Action::Redraw
            }
            _ => match event.text.as_ref().filter(|t| !t.chars().any(char::is_control)) {
                Some(t) if value.chars().count() < 256 => {
                    value.push_str(t);
                    Action::Redraw
                }
                _ => Action::None,
            },
        })
    }

    /// 입력기를 거친 글(Windows)도 지금 칸으로.
    pub fn commit(&mut self, text: &str) -> bool {
        if !self.open || self.busy.is_some() {
            return false;
        }
        let value = if self.field == 0 { &mut self.id } else { &mut self.pw };
        value.push_str(text);
        true
    }

    fn submit(&mut self) -> Action {
        if self.id.trim().is_empty() || self.pw.is_empty() {
            self.error = Some("아이디와 비밀번호를 넣어 주세요".into());
            self.field = if self.id.trim().is_empty() { 0 } else { 1 };
            return Action::Redraw;
        }
        let action = Action::Password(self.id.trim().to_string(), std::mem::take(&mut self.pw));
        self.start(action)
    }

    fn start(&mut self, action: Action) -> Action {
        self.error = None;
        self.busy = Some(if matches!(action, Action::Provider(_)) { "브라우저에서 로그인을 마쳐 주세요" } else { "로그인하는 중" });
        action
    }

    pub fn done(&mut self, result: Result<(), String>) {
        self.busy = None;
        match result {
            Ok(()) => self.close(),
            Err(e) => self.error = Some(e),
        }
    }

    fn close(&mut self) {
        self.open = false;
        self.pw.clear();
        self.error = None;
    }
}
