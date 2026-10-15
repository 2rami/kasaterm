use crate::{gpu, App};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};
mod style;
use style::{paragraph, rgb, text, Skin};
type Rect = (f32, f32, f32, f32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Cloud,
    Precision,
    Playroom,
}
impl Kind {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "a" | "cloud" => Some(Self::Cloud),
            "b" | "precision" => Some(Self::Precision),
            "c" | "playroom" => Some(Self::Playroom),
            _ => None,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Cloud => "A · Cloud OS",
            Self::Precision => "B · Precision",
            Self::Playroom => "C · Playroom",
        }
    }
    fn accent(self) -> [u8; 4] {
        match self {
            Self::Cloud => rgb(0x2f6fd4),
            Self::Precision => rgb(0x65cabb),
            Self::Playroom => rgb(0xa35485),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page {
    Appearance,
    Workspace,
    Shared,
}
#[derive(Clone, Copy, Debug, PartialEq)]
enum Action {
    Style(Kind),
    Page(Page),
    Mode(bool),
    System,
    Accent([u8; 4]),
    Hex,
    Apply,
    Toggle,
    Font(i32),
    Density(usize),
    File(usize),
    Info(bool),
    Tab,
}
#[derive(Clone, Copy)]
struct Hit {
    action: Action,
    rect: Rect,
}

pub(crate) struct State {
    kind: Kind,
    page: Page,
    dark: bool,
    system: bool,
    accent: [u8; 4],
    hex: String,
    editing: bool,
    replace_hex: bool,
    invalid: bool,
    portraits: bool,
    font: i32,
    density: usize,
    selected_file: usize,
    info_shared: bool,
    extra_tab: bool,
    scroll: f32,
    max_scroll: f32,
    hits: Vec<Hit>,
    focus: Option<usize>,
    notice: bool,
}
impl State {
    pub(crate) fn from_env() -> Option<Self> {
        if !crate::verification_run()
            || ![
                "KASATERM_SESSION_FILE",
                "KASATERM_SETTINGS_FILE",
                "KASATERM_WINDOW_FILE",
                "KASATERM_DEVICE_FILE",
                "KASATERM_SOCKET_PATH",
            ]
            .iter()
            .all(|key| std::env::var_os(key).is_some_and(|v| !v.is_empty()))
        {
            return None;
        }
        let kind = Kind::parse(&std::env::var("KASATERM_DESIGN_STUDY").ok()?)?;
        let mut s = Self::new(kind);
        s.page = match std::env::var("KASATERM_DESIGN_PAGE").as_deref() {
            Ok("workspace") => Page::Workspace,
            Ok("shared") => Page::Shared,
            _ => Page::Appearance,
        };
        Some(s)
    }
    fn new(kind: Kind) -> Self {
        let accent = kind.accent();
        Self {
            kind,
            page: Page::Appearance,
            dark: kind == Kind::Precision,
            system: false,
            accent,
            hex: hex(accent),
            editing: false,
            replace_hex: false,
            invalid: false,
            portraits: kind == Kind::Playroom,
            font: 14,
            density: 1,
            selected_file: 0,
            info_shared: true,
            extra_tab: false,
            scroll: 0.0,
            max_scroll: 0.0,
            hits: Vec::new(),
            focus: None,
            notice: false,
        }
    }
    fn pick(&mut self, action: Action) {
        self.editing = action == Action::Hex;
        self.notice = false;
        match action {
            Action::Style(kind) => {
                let page = self.page;
                *self = Self::new(kind);
                self.page = page;
            }
            Action::Page(page) => {
                self.page = page;
                self.scroll = 0.0;
                self.focus = None;
            }
            Action::Mode(dark) => {
                self.dark = dark;
                self.system = false;
            }
            Action::System => self.system = true,
            Action::Accent(color) => {
                self.accent = color;
                self.hex = hex(color);
                self.invalid = false;
            }
            Action::Hex => self.replace_hex = true,
            Action::Apply => {
                if let Some(color) = parse_hex(&self.hex) {
                    self.accent = color;
                    self.hex = hex(color);
                    self.invalid = false;
                    self.notice = true;
                } else {
                    self.invalid = true;
                }
            }
            Action::Toggle => self.portraits = !self.portraits,
            Action::Font(delta) => self.font = (self.font + delta).clamp(11, 20),
            Action::Density(i) => self.density = i,
            Action::File(i) => self.selected_file = i,
            Action::Info(shared) => self.info_shared = shared,
            Action::Tab => self.extra_tab = !self.extra_tab,
        }
    }
    fn key(&mut self, key: &Key, shift: bool) {
        if self.editing {
            match key {
                Key::Character(s) => {
                    if self.replace_hex {
                        self.hex.clear();
                        self.replace_hex = false;
                    }
                    for c in s.chars().filter(|c| c.is_ascii_hexdigit() || *c == '#') {
                        if self.hex.len() < 7 {
                            self.hex.push(c);
                        }
                    }
                    self.invalid = false;
                }
                Key::Named(NamedKey::Backspace) => {
                    if self.replace_hex {
                        self.hex.clear();
                        self.replace_hex = false;
                    } else {
                        self.hex.pop();
                    }
                    self.invalid = false;
                }
                Key::Named(NamedKey::Enter) => {
                    self.pick(Action::Apply);
                }
                Key::Named(NamedKey::Escape) => {
                    self.hex = hex(self.accent);
                    self.editing = false;
                    self.invalid = false;
                }
                _ => {}
            }
            return;
        }
        match key {
            Key::Named(NamedKey::Tab) => {
                let n = self.hits.len();
                if n > 0 {
                    self.focus = Some(match (self.focus, shift) {
                        (Some(i), true) => (i + n - 1) % n,
                        (Some(i), false) => (i + 1) % n,
                        (None, true) => n - 1,
                        (None, false) => 0,
                    });
                }
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                if let Some(hit) = self.focus.and_then(|i| self.hits.get(i)).copied() {
                    self.pick(hit.action);
                }
            }
            _ => {}
        }
    }
    fn add(&mut self, action: Action, r: Rect) {
        self.hits.push(Hit { action, rect: r });
    }
}
fn hex(c: [u8; 4]) -> String {
    format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2])
}
fn parse_hex(s: &str) -> Option<[u8; 4]> {
    let s = s.trim().strip_prefix('#').unwrap_or(s.trim());
    if s.len() != 6 {
        return None;
    }
    let n = u32::from_str_radix(s, 16).ok()?;
    Some(rgb(n))
}
fn inside(r: Rect, p: (f32, f32)) -> bool {
    p.0 >= r.0 && p.0 < r.0 + r.2 && p.1 >= r.1 && p.1 < r.1 + r.3
}

impl App {
    pub(crate) fn design_study_event(&mut self, event: &WindowEvent) -> bool {
        let Some(s) = self.design_study.as_mut() else {
            return false;
        };
        let cursor = self.cursor_px;
        match event {
            WindowEvent::CursorMoved { .. } => {
                if let Some(w) = &self.window {
                    w.set_cursor(if s.hits.iter().any(|h| inside(h.rect, cursor)) {
                        winit::window::CursorIcon::Pointer
                    } else {
                        winit::window::CursorIcon::Default
                    });
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(hit) = s
                    .hits
                    .iter()
                    .rev()
                    .find(|h| inside(h.rect, cursor))
                    .copied()
                {
                    s.pick(hit.action);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let y = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y * 40.0,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32,
                };
                s.scroll = (s.scroll - y).clamp(0.0, s.max_scroll);
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed && !event.repeat {
                    s.key(&event.logical_key, self.modifiers.shift_key());
                }
            }
            WindowEvent::Ime(_)
            | WindowEvent::MouseInput { .. }
            | WindowEvent::DroppedFile(_)
            | WindowEvent::Touch(_)
            | WindowEvent::PinchGesture { .. } => {}
            _ => return false,
        }
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        true
    }
    pub(crate) fn paint_design_study(&mut self, scale: f32, time: f32) -> bool {
        let Some(s) = self.design_study.as_mut() else {
            return false;
        };
        let (Some(g), Some(w)) = (self.gpu.as_mut(), self.window.as_ref()) else {
            return true;
        };
        let size = w.inner_size();
        g.maintain_atlas();
        g.clear_chrome();
        paint(
            g,
            s,
            (size.width as f32 / scale, size.height as f32 / scale),
            self.cursor_px,
        );
        if let Err(error) = g.render(&[], scale, time, true) {
            eprintln!("[design study] render: {error}");
        }
        true
    }
}

#[derive(Debug)]
struct Layout {
    nav: Rect,
    main: Rect,
    header: Rect,
    body: Rect,
    compact: bool,
}
fn layout(size: (f32, f32), kind: Kind) -> Layout {
    let (w, h) = size;
    let compact = w < 720.0;
    let margin = match kind {
        Kind::Cloud => 18.0,
        Kind::Precision => 0.0,
        Kind::Playroom => 20.0,
    };
    let top = 54.0;
    let nav_w = if compact {
        0.0
    } else {
        match kind {
            Kind::Cloud => 208.0,
            Kind::Precision => 184.0,
            Kind::Playroom => 78.0,
        }
    };
    let gap = if nav_w > 0.0 {
        if kind == Kind::Precision {
            1.0
        } else {
            18.0
        }
    } else {
        0.0
    };
    let x = margin + nav_w + gap;
    let mw = (w - x - margin).max(0.0);
    let header_h: f32 = if compact {
        122.0
    } else {
        if kind == Kind::Precision {
            76.0
        } else {
            100.0
        }
    };
    Layout {
        nav: (margin, top, nav_w, (h - top - margin).max(0.0)),
        main: (x, top, mw, (h - top - margin).max(0.0)),
        header: (x, top, mw, header_h.min((h - top).max(0.0))),
        body: (
            x,
            top + header_h,
            mw,
            (h - top - header_h - margin).max(0.0),
        ),
        compact,
    }
}
fn button(
    g: &mut gpu::GpuRenderer,
    s: &mut State,
    k: Skin,
    r: Rect,
    label: &str,
    action: Action,
    primary: bool,
    active: bool,
    cursor: (f32, f32),
) {
    k.button(g, r, label, primary, active, inside(r, cursor));
    s.add(action, r);
}
fn icon(g: &mut gpu::GpuRenderer, name: &str, x: f32, y: f32, size: f32, color: [u8; 4]) {
    g.queue_icon(name, x, y, size, color);
}
fn title(g: &mut gpu::GpuRenderer, k: Skin, r: Rect, name: &str) {
    text(
        g,
        r,
        name,
        if k.kind == Kind::Precision {
            14.0
        } else {
            16.0
        },
        k.ink,
        true,
    );
}

fn paint(g: &mut gpu::GpuRenderer, s: &mut State, size: (f32, f32), cursor: (f32, f32)) {
    s.hits.clear();
    crate::theme::set_character_appearance(s.portraits);
    let k = Skin::new(s.kind, s.dark, s.accent);
    let l = layout(size, s.kind);
    g.rect(0.0, 0.0, size.0, size.1, k.bg);
    icon(g, "terminal", 22.0, 17.0, 18.0, k.accent);
    text(g, (49.0, 17.0, 140.0, 24.0), "KASA", 15.0, k.ink, true);
    if size.0 > 600.0 {
        text(
            g,
            (123.0, 18.0, 240.0, 20.0),
            "디자인 비교 · 저장 없는 미리보기",
            11.0,
            k.dim,
            false,
        );
    }
    for (i, kind) in [Kind::Cloud, Kind::Precision, Kind::Playroom]
        .into_iter()
        .enumerate()
    {
        button(
            g,
            s,
            k,
            (size.0 - 134.0 + i as f32 * 38.0, 12.0, 32.0, 30.0),
            ["A", "B", "C"][i],
            Action::Style(kind),
            false,
            s.kind == kind,
            cursor,
        );
    }
    if l.nav.2 > 0.0 {
        if k.kind == Kind::Precision {
            g.rect(l.nav.0, l.nav.1, l.nav.2, l.nav.3, k.panel);
        } else {
            k.panel(g, l.nav, true);
        }
        if k.kind != Kind::Playroom {
            text(
                g,
                (l.nav.0 + 18.0, l.nav.1 + 24.0, l.nav.2 - 36.0, 18.0),
                "내 작업 공간",
                12.0,
                k.dim,
                true,
            );
            k.field(
                g,
                (l.nav.0 + 14.0, l.nav.1 + 58.0, l.nav.2 - 28.0, 34.0),
                false,
            );
            icon(g, "search", l.nav.0 + 25.0, l.nav.1 + 68.0, 14.0, k.dim);
            text(
                g,
                (l.nav.0 + 47.0, l.nav.1 + 68.0, l.nav.2 - 66.0, 18.0),
                "설정 찾기",
                12.0,
                k.dim,
                false,
            );
        } else {
            g.round_rect_fill(l.nav.0 + 14.0, l.nav.1 + 18.0, 50.0, 50.0, 25.0, k.field);
            face(g, 0, l.nav.0 + 21.0, l.nav.1 + 25.0, 36.0, k);
        }
        for (i, (page, name, glyph)) in [
            (Page::Workspace, "작업 공간", "layout-grid"),
            (Page::Shared, "공유 폴더", "folder"),
            (Page::Appearance, "설정", "settings-2"),
        ]
        .into_iter()
        .enumerate()
        {
            let r = (
                l.nav.0 + 10.0,
                l.nav.1
                    + if k.kind == Kind::Playroom {
                        92.0
                    } else {
                        116.0
                    }
                    + i as f32
                        * if k.kind == Kind::Precision {
                            43.0
                        } else {
                            54.0
                        },
                l.nav.2 - 20.0,
                if k.kind == Kind::Playroom { 48.0 } else { 42.0 },
            );
            if s.page == page || inside(r, cursor) {
                g.round_rect_fill(
                    r.0,
                    r.1,
                    r.2,
                    r.3,
                    k.box_radius(),
                    if s.page == page {
                        crate::theme::lerp(k.field, k.accent, 0.16)
                    } else {
                        k.field
                    },
                );
            }
            icon(
                g,
                glyph,
                r.0 + if k.kind == Kind::Playroom { 17.0 } else { 14.0 },
                r.1 + (r.3 - 18.0) / 2.0,
                18.0,
                if s.page == page { k.accent } else { k.dim },
            );
            if k.kind != Kind::Playroom {
                text(
                    g,
                    (r.0 + 44.0, r.1 + 13.0, r.2 - 54.0, 18.0),
                    name,
                    k.text,
                    k.ink,
                    s.page == page,
                );
            }
            s.add(Action::Page(page), r);
        }
        if k.kind != Kind::Playroom && l.nav.3 > 500.0 {
            text(
                g,
                (l.nav.0 + 18.0, l.nav.1 + 310.0, l.nav.2 - 30.0, 18.0),
                "바로 가기",
                11.0,
                k.dim,
                true,
            );
            for (i, name) in ["외형과 색상", "캐릭터", "연결 기기"].iter().enumerate() {
                text(
                    g,
                    (
                        l.nav.0 + 28.0,
                        l.nav.1 + 345.0 + i as f32 * 32.0,
                        l.nav.2 - 45.0,
                        18.0,
                    ),
                    name,
                    12.0,
                    k.dim,
                    false,
                );
            }
        }
        if l.nav.3 > 260.0 {
            icon(
                g,
                "laptop",
                l.nav.0 + if k.kind == Kind::Playroom { 30.0 } else { 20.0 },
                l.nav.1 + l.nav.3 - 40.0,
                18.0,
                k.dim,
            );
            if k.kind != Kind::Playroom {
                text(
                    g,
                    (
                        l.nav.0 + 50.0,
                        l.nav.1 + l.nav.3 - 38.0,
                        l.nav.2 - 58.0,
                        18.0,
                    ),
                    "이 기기",
                    12.0,
                    k.dim,
                    false,
                );
            }
        }
    }
    let pad = if l.compact { 18.0 } else { k.gap };
    if k.kind == Kind::Precision {
        g.rect(l.main.0, l.main.1, l.main.2, l.main.3, k.panel);
    } else {
        k.panel(g, l.main, true);
    }
    let heading = match (s.page, k.kind) {
        (Page::Appearance, Kind::Playroom) => "내 작업실 꾸미기",
        (Page::Appearance, _) => "외형",
        (Page::Workspace, _) => "작업 공간",
        (Page::Shared, _) => "공유 폴더",
    };
    text(
        g,
        (
            l.header.0 + pad,
            l.header.1 + 22.0,
            l.header.2 - pad * 2.0,
            34.0,
        ),
        heading,
        if k.kind == Kind::Precision {
            22.0
        } else {
            28.0
        },
        k.ink,
        true,
    );
    let desc = match s.page {
        Page::Appearance => "색과 글꼴, 화면의 느낌을 나에게 맞춰요.",
        Page::Workspace => "터미널과 작업 정보가 한 공간에 있어요.",
        Page::Shared => "작업에 쓰는 파일을 모아 보고 공유해요.",
    };
    text(
        g,
        (
            l.header.0 + pad,
            l.header.1
                + if k.kind == Kind::Precision {
                    53.0
                } else {
                    61.0
                },
            l.header.2 - pad * 2.0,
            20.0,
        ),
        desc,
        k.text,
        k.dim,
        false,
    );
    if !l.compact && l.header.2 > 650.0 {
        text(
            g,
            (
                l.header.0 + l.header.2 - 202.0,
                l.header.1 + 27.0,
                180.0,
                20.0,
            ),
            s.kind.name(),
            12.0,
            k.accent,
            true,
        );
    }
    if l.compact {
        for (i, (page, name)) in [
            (Page::Workspace, "작업"),
            (Page::Shared, "공유"),
            (Page::Appearance, "설정"),
        ]
        .into_iter()
        .enumerate()
        {
            let bw = (l.header.2 - 2.0 * pad - 16.0) / 3.0;
            button(
                g,
                s,
                k,
                (
                    l.header.0 + pad + i as f32 * (bw + 8.0),
                    l.header.1 + 84.0,
                    bw,
                    28.0,
                ),
                name,
                Action::Page(page),
                false,
                s.page == page,
                cursor,
            );
        }
    }
    let body = (
        l.body.0 + pad,
        l.body.1,
        l.body.2 - pad * 2.0,
        l.body.3 - 12.0,
    );
    let hit_start = s.hits.len();
    g.push_clip(body.0, body.1, body.2, body.3.max(0.0));
    let height = match s.page {
        Page::Appearance => appearance(g, s, k, body, cursor),
        Page::Workspace => workspace(g, s, k, body, cursor),
        Page::Shared => shared(g, s, k, body, cursor),
    };
    g.pop_clip();
    for hit in &mut s.hits[hit_start..] {
        let top = hit.rect.1.max(body.1);
        let bottom = (hit.rect.1 + hit.rect.3).min(body.1 + body.3);
        hit.rect.1 = top;
        hit.rect.3 = (bottom - top).max(0.0);
    }
    s.hits.retain(|hit| hit.rect.2 > 0.0 && hit.rect.3 > 0.0);
    if s.focus.is_some_and(|index| index >= s.hits.len()) {
        s.focus = None;
    }
    s.max_scroll = (height - body.3).max(0.0);
    s.scroll = s.scroll.min(s.max_scroll);
    if s.max_scroll > 0.0 {
        let th = (body.3 * body.3 / height).max(28.0);
        let ty = body.1 + (body.3 - th) * s.scroll / s.max_scroll;
        g.round_rect_fill(
            l.main.0 + l.main.2 - 8.0,
            ty,
            3.0,
            th,
            1.5,
            crate::theme::with_alpha(k.dim, 100),
        );
    }
    if let Some(hit) = s.focus.and_then(|i| s.hits.get(i)) {
        g.round_rect_stroke(
            hit.rect.0,
            hit.rect.1,
            hit.rect.2,
            hit.rect.3,
            k.box_radius(),
            2.0,
            crate::theme::enforce_contrast_at(k.accent, k.panel, 3.0),
        );
    }
    g.hover_pointer = s.hits.iter().any(|h| inside(h.rect, cursor));
}

fn face(g: &mut gpu::GpuRenderer, i: usize, x: f32, y: f32, size: f32, k: Skin) {
    if let Some((name, _)) = crate::theme::character_slugs().get(i) {
        if crate::sprites::draw_student_face(g, name, x, y, size) {
            return;
        }
    }
    icon(
        g,
        if i % 2 == 0 { "claude" } else { "codex" },
        x,
        y,
        size,
        k.accent,
    );
}

fn appearance(
    g: &mut gpu::GpuRenderer,
    s: &mut State,
    k: Skin,
    body: Rect,
    cursor: (f32, f32),
) -> f32 {
    let wide = body.2 >= 780.0;
    let preview_w = if wide {
        (body.2 * 0.38).min(380.0)
    } else {
        body.2
    };
    let form_w = if wide {
        body.2 - preview_w - k.gap
    } else {
        body.2
    };
    let x = body.0;
    let top = body.1 - s.scroll;
    let mut y = top;
    let card_pad = if k.kind == Kind::Precision { 0.0 } else { 18.0 };
    let stack_extra = if form_w - 2.0 * card_pad
        < if k.kind == Kind::Precision {
            260.0
        } else {
            300.0
        } {
        50.0
    } else {
        0.0
    };
    let panel_h = if k.kind == Kind::Precision {
        198.0
    } else {
        252.0
    } + stack_extra;
    if k.kind != Kind::Precision {
        k.panel(g, (x, y, form_w, panel_h), false);
    }
    let fx = x + card_pad;
    let fw = form_w - 2.0 * card_pad;
    title(
        g,
        k,
        (fx, y + if card_pad > 0.0 { 18.0 } else { 4.0 }, fw, 22.0),
        "테마와 색상",
    );
    let cy = y + if k.kind == Kind::Precision {
        34.0
    } else {
        52.0
    };
    let gap = 10.0;
    let sw = (fw - gap * 2.0) / 3.0;
    let sh = if k.kind == Kind::Precision {
        52.0
    } else {
        74.0
    };
    for (i, (name, dark)) in [("밝게", false), ("어둡게", true), ("시스템", false)]
        .into_iter()
        .enumerate()
    {
        let r = (fx + i as f32 * (sw + gap), cy, sw, sh);
        let selected = if i == 2 {
            s.system
        } else {
            !s.system && s.dark == dark
        };
        let fill = if dark { rgb(0x242b37) } else { rgb(0xf4f7fc) };
        g.round_rect_fill(r.0, r.1, r.2, r.3, k.box_radius(), fill);
        g.round_rect_stroke(
            r.0,
            r.1,
            r.2,
            r.3,
            k.box_radius(),
            if selected { 2.0 } else { 1.0 },
            if selected { k.accent } else { k.edge },
        );
        let mock = (r.0 + 10.0, r.1 + 9.0, r.2 - 20.0, (sh - 30.0).max(12.0));
        g.round_rect_fill(
            mock.0,
            mock.1,
            mock.2,
            mock.3,
            4.0,
            if dark { rgb(0x3c475b) } else { rgb(0xffffff) },
        );
        g.round_rect_fill(
            mock.0 + 5.0,
            mock.1 + 5.0,
            9.0,
            (mock.3 - 10.0).max(2.0),
            2.0,
            crate::theme::with_alpha(k.accent, 100),
        );
        g.rect(
            mock.0 + 20.0,
            mock.1 + 6.0,
            (mock.2 - 29.0).max(0.0),
            2.0,
            if dark { rgb(0x8493ac) } else { rgb(0xc4cfdf) },
        );
        text(
            g,
            (r.0 + 10.0, r.1 + sh - 18.0, r.2 - 20.0, 16.0),
            name,
            11.0,
            if dark { rgb(0xe2eaf6) } else { rgb(0x4d607b) },
            selected,
        );
        s.add(
            if i == 2 {
                Action::System
            } else {
                Action::Mode(dark)
            },
            r,
        );
    }
    let ay = cy + sh + 22.0;
    text(g, (fx, ay, fw, 20.0), "강조색", k.text, k.ink, true);
    let colors = [
        s.kind.accent(),
        rgb(0x689f8c),
        rgb(0xcb8559),
        rgb(0x9375cf),
        rgb(0xcb6f97),
    ];
    let diameter = if k.kind == Kind::Precision {
        22.0
    } else {
        30.0
    };
    for (i, color) in colors.into_iter().enumerate() {
        let r = (
            fx + i as f32 * (diameter + 10.0),
            ay + 28.0,
            diameter,
            diameter,
        );
        g.round_rect_fill(r.0, r.1, r.2, r.3, diameter / 2.0, color);
        if s.accent == color {
            icon(
                g,
                "check",
                r.0 + (diameter - 14.0) / 2.0,
                r.1 + (diameter - 14.0) / 2.0,
                14.0,
                crate::theme::foreground_on(color),
            );
        }
        s.add(Action::Accent(color), r);
    }
    let hex_y = ay + 28.0;
    let field_w = 100.0;
    let hx = (fx + fw - field_w).max(fx + 5.0 * (diameter + 10.0));
    let stacked = hx + field_w > fx + fw;
    let hex_rect = if stacked {
        (fx, hex_y + diameter + 14.0, fw.min(180.0), k.button)
    } else {
        (hx, hex_y - 3.0, field_w, k.button)
    };
    k.field(g, hex_rect, s.editing);
    text(
        g,
        (
            hex_rect.0 + 12.0,
            hex_rect.1 + (hex_rect.3 - 13.0) / 2.0,
            hex_rect.2 - 24.0,
            18.0,
        ),
        &s.hex,
        13.0,
        if s.invalid { rgb(0xc94b64) } else { k.ink },
        false,
    );
    s.add(Action::Hex, hex_rect);
    text(
        g,
        (fx, hex_rect.1 + hex_rect.3 + 10.0, fw, 18.0),
        if s.invalid {
            "6자리 HEX 색상을 입력해주세요"
        } else {
            "HEX로 원하는 색을 직접 입력"
        },
        10.5,
        if s.invalid { rgb(0xc94b64) } else { k.dim },
        false,
    );
    y += panel_h + k.gap;
    let form2h = if k.kind == Kind::Precision {
        201.0
    } else {
        268.0
    };
    if k.kind != Kind::Precision {
        k.panel(g, (x, y, form_w, form2h), false);
    }
    title(
        g,
        k,
        (fx, y + if card_pad > 0.0 { 18.0 } else { 0.0 }, fw, 22.0),
        "글꼴과 화면 밀도",
    );
    let fy = y + if k.kind == Kind::Precision {
        35.0
    } else {
        58.0
    };
    text(
        g,
        (fx, fy + 9.0, fw * 0.5, 18.0),
        "터미널 글자 크기",
        k.text,
        k.ink,
        false,
    );
    let bw = if k.kind == Kind::Precision {
        28.0
    } else {
        34.0
    };
    let right = fx + fw;
    button(
        g,
        s,
        k,
        (right - bw * 2.0 - 42.0, fy, bw, k.button),
        "−",
        Action::Font(-1),
        false,
        false,
        cursor,
    );
    text(
        g,
        (
            right - bw - 38.0,
            fy + (k.button - k.text) / 2.0,
            36.0,
            18.0,
        ),
        &s.font.to_string(),
        k.text,
        k.ink,
        true,
    );
    button(
        g,
        s,
        k,
        (right - bw, fy, bw, k.button),
        "+",
        Action::Font(1),
        false,
        false,
        cursor,
    );
    if k.kind == Kind::Precision {
        g.rect(fx, fy + k.row, fw, 1.0, k.edge);
    }
    let dy = fy + k.row + if k.kind == Kind::Precision { 8.0 } else { 12.0 };
    text(g, (fx, dy, fw, 20.0), "화면 밀도", k.text, k.ink, false);
    let dw = (fw - 16.0) / 3.0;
    for (i, name) in ["촘촘하게", "기본", "여유롭게"].iter().enumerate() {
        button(
            g,
            s,
            k,
            (fx + i as f32 * (dw + 8.0), dy + 28.0, dw, k.button),
            name,
            Action::Density(i),
            false,
            s.density == i,
            cursor,
        );
    }
    let ty = dy + 28.0 + k.button + 22.0;
    text(
        g,
        (fx, ty + 5.0, fw - 65.0, 18.0),
        "캐릭터 표시",
        k.text,
        k.ink,
        false,
    );
    let toggle = (right - 46.0, ty, 46.0, 26.0);
    g.round_rect_fill(
        toggle.0,
        toggle.1,
        toggle.2,
        toggle.3,
        13.0,
        if s.portraits { k.accent } else { k.edge },
    );
    g.round_rect_fill(
        toggle.0 + if s.portraits { 23.0 } else { 3.0 },
        toggle.1 + 3.0,
        20.0,
        20.0,
        10.0,
        rgb(0xffffff),
    );
    s.add(Action::Toggle, toggle);
    y += form2h + k.gap;
    let button_y = y;
    button(
        g,
        s,
        k,
        (x, button_y, form_w.min(240.0), k.button),
        if s.notice {
            "미리보기에 반영됨"
        } else {
            "이 색으로 미리보기"
        },
        Action::Apply,
        true,
        false,
        cursor,
    );
    y += k.button + 20.0;
    if wide {
        preview(
            g,
            s,
            k,
            (body.0 + form_w + k.gap, top, preview_w, 360.0),
            cursor,
        );
        if k.kind == Kind::Playroom {
            let r = (body.0 + form_w + k.gap, top + 384.0, preview_w, 176.0);
            k.panel(g, r, false);
            title(
                g,
                k,
                (r.0 + 20.0, r.1 + 20.0, r.2 - 40.0, 24.0),
                "함께하는 얼굴들",
            );
            for i in 0..4 {
                let fx = r.0 + 22.0 + i as f32 * ((r.2 - 44.0) / 4.0);
                g.round_rect_fill(fx, r.1 + 60.0, 48.0, 48.0, 18.0, k.field);
                face(g, i, fx + 6.0, r.1 + 66.0, 36.0, k);
            }
            text(
                g,
                (r.0 + 20.0, r.1 + 130.0, r.2 - 40.0, 22.0),
                "이름과 얼굴로 작업을 찾아요.",
                12.0,
                k.dim,
                false,
            );
        } else {
            let r = (body.0 + form_w + k.gap, top + 384.0, preview_w, 126.0);
            k.panel(g, r, false);
            title(g, k, (r.0 + 18.0, r.1 + 18.0, r.2 - 36.0, 20.0), "내 색상");
            text(
                g,
                (r.0 + 18.0, r.1 + 52.0, r.2 - 36.0, 18.0),
                &s.hex,
                18.0,
                k.accent,
                true,
            );
            text(
                g,
                (r.0 + 18.0, r.1 + 88.0, r.2 - 36.0, 18.0),
                "버튼 · 선택한 탭 · 입력 포커스",
                11.5,
                k.dim,
                false,
            );
        }
    } else {
        preview(g, s, k, (x, y, form_w, 310.0), cursor);
        y += 334.0;
    }
    (y - top).max(if wide { 560.0 } else { 0.0 })
}

fn mono(g: &mut gpu::GpuRenderer, x: f32, y: f32, w: f32, value: &str, size: f32, color: [u8; 4]) {
    let step = size * 0.6;
    for (i, ch) in value.chars().enumerate() {
        let cx = x + i as f32 * step;
        if cx + step > x + w {
            break;
        }
        g.draw_text(
            cx,
            y,
            &ch.to_string(),
            gpu::DrawOpts {
                font_size: size,
                color,
                bold: false,
                italic: false,
            },
        );
    }
}

fn terminal(
    g: &mut gpu::GpuRenderer,
    s: &mut State,
    k: Skin,
    r: Rect,
    compact: bool,
    cursor: (f32, f32),
) {
    let terminal_bg = if s.dark { rgb(0x141b26) } else { rgb(0xf7f9fd) };
    let ink = if s.dark { rgb(0xd9e6f6) } else { rgb(0x354967) };
    g.round_rect_fill(r.0, r.1, r.2, r.3, k.box_radius(), terminal_bg);
    let th = if k.kind == Kind::Precision {
        30.0
    } else {
        38.0
    };
    g.round_rect_fill(r.0, r.1, r.2, th, k.box_radius(), k.field);
    let tab_w = if compact { 100.0 } else { 130.0 };
    g.round_rect_fill(
        r.0 + 5.0,
        r.1 + 5.0,
        tab_w,
        th - 10.0,
        if k.kind == Kind::Precision { 3.0 } else { 8.0 },
        k.panel,
    );
    text(
        g,
        (r.0 + 17.0, r.1 + 12.0, tab_w - 22.0, 18.0),
        "main",
        11.5,
        k.ink,
        true,
    );
    if r.2 > 270.0 {
        text(
            g,
            (r.0 + tab_w + 24.0, r.1 + 12.0, 100.0, 18.0),
            if s.extra_tab {
                "새 터미널"
            } else {
                "build"
            },
            11.5,
            k.dim,
            false,
        );
    }
    let plus = (r.0 + r.2 - 32.0, r.1 + 4.0, 28.0, th - 8.0);
    button(g, s, k, plus, "+", Action::Tab, false, false, cursor);
    let fs = if compact {
        (s.font as f32 - 2.0).max(10.0)
    } else {
        s.font as f32
    };
    let lh = fs + 7.0 + s.density as f32 * 5.0;
    let lines = [
        ("~/Projects/kasaterm", k.dim),
        ("$ git status --short", ink),
        (" M docs/design.md", k.accent),
        (" M native_controls.rs", k.accent),
        ("", ink),
        ("$ cargo check", ink),
        ("", ink),
        ("$ ", ink),
    ];
    g.push_clip(
        r.0 + 12.0,
        r.1 + th + 12.0,
        r.2 - 24.0,
        (r.3 - th - 30.0).max(0.0),
    );
    for (i, (line, color)) in lines.iter().enumerate() {
        let ly = r.1 + th + 18.0 + i as f32 * lh;
        if ly + fs < r.1 + r.3 - 20.0 {
            mono(g, r.0 + 18.0, ly, r.2 - 36.0, line, fs, *color);
        }
    }
    g.pop_clip();
    g.rect(r.0 + 10.0, r.1 + r.3 - 24.0, r.2 - 20.0, 1.0, k.edge);
    text(
        g,
        (r.0 + 16.0, r.1 + r.3 - 17.0, r.2 - 32.0, 16.0),
        "main    ~/Projects/kasaterm",
        10.5,
        k.dim,
        false,
    );
}
fn preview(g: &mut gpu::GpuRenderer, s: &mut State, k: Skin, r: Rect, cursor: (f32, f32)) {
    k.panel(g, r, k.kind == Kind::Cloud);
    let pad = 18.0;
    title(
        g,
        k,
        (r.0 + pad, r.1 + 18.0, r.2 - pad * 2.0, 22.0),
        "작업 화면 미리보기",
    );
    terminal(
        g,
        s,
        k,
        (r.0 + pad, r.1 + 56.0, r.2 - pad * 2.0, r.3 - 126.0),
        true,
        cursor,
    );
    let by = r.1 + r.3 - 54.0;
    if s.portraits {
        face(g, 2, r.0 + pad, by - 2.0, 32.0, k);
        text(
            g,
            (r.0 + pad + 44.0, by + 1.0, r.2 - pad * 2.0 - 44.0, 18.0),
            "작업에 맞는 색과 얼굴",
            12.0,
            k.ink,
            true,
        );
    } else {
        text(
            g,
            (r.0 + pad, by + 2.0, r.2 - pad * 2.0, 18.0),
            "선택한 항목이 또렷하게",
            12.0,
            k.ink,
            true,
        );
    }
    text(
        g,
        (
            r.0 + pad + if s.portraits { 44.0 } else { 0.0 },
            by + 25.0,
            r.2 - pad * 2.0 - if s.portraits { 44.0 } else { 0.0 },
            18.0,
        ),
        "같은 스타일을 앱 전체에 적용할 수 있어요.",
        10.5,
        k.dim,
        false,
    );
}

fn workspace(
    g: &mut gpu::GpuRenderer,
    s: &mut State,
    k: Skin,
    body: Rect,
    cursor: (f32, f32),
) -> f32 {
    let wide = body.2 > 760.0;
    let side = if wide { 300.0 } else { body.2 };
    let mainw = if wide { body.2 - side - k.gap } else { body.2 };
    let top = body.1 - s.scroll;
    let small_cards = mainw < 480.0;
    if k.kind == Kind::Playroom {
        for i in 0..3 {
            let cw = (mainw - 16.0) / 3.0;
            let r = (
                body.0 + i as f32 * (cw + 8.0),
                top,
                cw,
                if small_cards { 86.0 } else { 74.0 },
            );
            k.panel(g, r, false);
            let name = ["UI 작업", "문서 정리", "공유함"][i];
            if small_cards {
                face(g, i, r.0 + (r.2 - 36.0) / 2.0, r.1 + 12.0, 36.0, k);
                text(
                    g,
                    (r.0 + 8.0, r.1 + 60.0, r.2 - 16.0, 18.0),
                    name,
                    12.0,
                    k.ink,
                    true,
                );
            } else {
                face(g, i, r.0 + 12.0, r.1 + 17.0, 36.0, k);
                text(
                    g,
                    (r.0 + 58.0, r.1 + 20.0, r.2 - 70.0, 18.0),
                    name,
                    12.0,
                    k.ink,
                    true,
                );
                text(
                    g,
                    (r.0 + 58.0, r.1 + 42.0, r.2 - 70.0, 16.0),
                    "저장된 작업",
                    10.5,
                    k.dim,
                    false,
                );
            }
        }
    }
    let offset = if k.kind == Kind::Playroom {
        if small_cards {
            106.0
        } else {
            94.0
        }
    } else {
        0.0
    };
    let term_h = if wide {
        (body.3 - offset - 94.0).max(260.0)
    } else {
        300.0
    };
    terminal(
        g,
        s,
        k,
        (body.0, top + offset, mainw, term_h),
        false,
        cursor,
    );
    let row = (body.0, top + offset + term_h + 18.0, mainw, 60.0);
    k.panel(g, row, false);
    icon(g, "git-branch", row.0 + 16.0, row.1 + 20.0, 18.0, k.accent);
    text(
        g,
        (row.0 + 46.0, row.1 + 15.0, row.2 - 60.0, 20.0),
        "디자인 비교",
        13.0,
        k.ink,
        true,
    );
    text(
        g,
        (row.0 + 46.0, row.1 + 36.0, row.2 - 60.0, 18.0),
        "같은 작업, 다른 조작부와 공간 구성",
        11.5,
        k.dim,
        false,
    );
    let right = if wide {
        (
            body.0 + mainw + k.gap,
            top,
            side,
            (term_h + offset + 78.0).max(480.0),
        )
    } else {
        (body.0, top + offset + term_h + 100.0, side, 470.0)
    };
    info_panel(g, s, k, right, cursor);
    if wide {
        right.3
    } else {
        offset + term_h + 570.0
    }
}
fn info_panel(g: &mut gpu::GpuRenderer, s: &mut State, k: Skin, r: Rect, cursor: (f32, f32)) {
    k.panel(g, r, k.kind != Kind::Precision);
    let pad = 18.0;
    title(g, k, (r.0 + pad, r.1 + 20.0, r.2 - 36.0, 22.0), "Info");
    let cw = (r.2 - 2.0 * pad - 8.0) / 2.0;
    button(
        g,
        s,
        k,
        (r.0 + pad, r.1 + 54.0, cw, k.button),
        "작업 정보",
        Action::Info(false),
        false,
        !s.info_shared,
        cursor,
    );
    button(
        g,
        s,
        k,
        (r.0 + pad + cw + 8.0, r.1 + 54.0, cw, k.button),
        "공유 폴더",
        Action::Info(true),
        false,
        s.info_shared,
        cursor,
    );
    let fy = r.1 + 54.0 + k.button + 24.0;
    if s.info_shared {
        text(
            g,
            (r.0 + pad, fy, r.2 - 36.0, 20.0),
            "최근 파일",
            12.0,
            k.dim,
            true,
        );
        files(
            g,
            s,
            k,
            (r.0 + pad, fy + 30.0, r.2 - 36.0, r.3 - (fy - r.1) - 40.0),
            cursor,
            false,
        );
    } else {
        if s.portraits {
            face(g, 2, r.0 + pad, fy, 44.0, k);
        }
        text(
            g,
            (r.0 + pad, fy + 62.0, r.2 - 36.0, 22.0),
            "디자인 비교",
            16.0,
            k.ink,
            true,
        );
        for (i, (name, value)) in [
            ("폴더", "~/Projects/kasaterm"),
            ("종류", "로컬 터미널"),
            ("작업 방", "UI 개편"),
        ]
        .iter()
        .enumerate()
        {
            let y = fy + 110.0 + i as f32 * 58.0;
            text(
                g,
                (r.0 + pad, y, r.2 - 36.0, 18.0),
                name,
                11.0,
                k.dim,
                false,
            );
            text(
                g,
                (r.0 + pad, y + 21.0, r.2 - 36.0, 20.0),
                value,
                12.0,
                k.ink,
                true,
            );
        }
    }
}
const FILES: [(&str, &str, &str); 4] = [
    ("디자인 비교.png", "image", "이미지 · 320 KB"),
    ("작업 노트.md", "file-text", "문서 · 4 KB"),
    ("UI 체크리스트.txt", "file-text", "텍스트 · 2 KB"),
    ("화면 캡처", "folder", "폴더 · 6개 항목"),
];
fn files(
    g: &mut gpu::GpuRenderer,
    s: &mut State,
    k: Skin,
    r: Rect,
    cursor: (f32, f32),
    large: bool,
) {
    let h = if large { k.row + 14.0 } else { k.row + 10.0 };
    for (i, (name, glyph, detail)) in FILES.iter().enumerate() {
        let fr = (
            r.0,
            r.1 + i as f32 * (h + if k.kind == Kind::Precision { 0.0 } else { 8.0 }),
            r.2,
            h,
        );
        if fr.1 + fr.3 > r.1 + r.3 {
            break;
        }
        let active = s.selected_file == i;
        if active || k.kind != Kind::Precision || inside(fr, cursor) {
            g.round_rect_fill(
                fr.0,
                fr.1,
                fr.2,
                fr.3,
                k.box_radius(),
                if active {
                    crate::theme::lerp(k.field, k.accent, 0.15)
                } else {
                    k.field
                },
            );
        }
        if active {
            g.round_rect_stroke(
                fr.0,
                fr.1,
                fr.2,
                fr.3,
                k.box_radius(),
                1.0,
                crate::theme::with_alpha(k.accent, 130),
            );
        }
        icon(
            g,
            glyph,
            fr.0 + 14.0,
            fr.1 + (fr.3 - 22.0) / 2.0,
            22.0,
            if active { k.accent } else { k.dim },
        );
        text(
            g,
            (fr.0 + 50.0, fr.1 + 14.0, fr.2 - 65.0, 22.0),
            name,
            k.text,
            k.ink,
            true,
        );
        text(
            g,
            (fr.0 + 50.0, fr.1 + fr.3 - 26.0, fr.2 - 65.0, 18.0),
            detail,
            11.0,
            k.dim,
            false,
        );
        s.add(Action::File(i), fr);
    }
}
fn shared(g: &mut gpu::GpuRenderer, s: &mut State, k: Skin, body: Rect, cursor: (f32, f32)) -> f32 {
    let wide = body.2 > 680.0;
    let fw = if wide { body.2 * 0.54 } else { body.2 };
    let top = body.1 - s.scroll;
    let search = (body.0, top, fw, k.button + 6.0);
    k.field(g, search, false);
    icon(
        g,
        "search",
        search.0 + 14.0,
        search.1 + (search.3 - 16.0) / 2.0,
        16.0,
        k.dim,
    );
    text(
        g,
        (
            search.0 + 42.0,
            search.1 + (search.3 - 13.0) / 2.0,
            search.2 - 54.0,
            20.0,
        ),
        "공유 폴더의 최근 파일",
        13.0,
        k.dim,
        false,
    );
    files(
        g,
        s,
        k,
        (body.0, top + search.3 + 20.0, fw, 450.0),
        cursor,
        true,
    );
    let r = if wide {
        (body.0 + fw + k.gap, top, body.2 - fw - k.gap, 480.0)
    } else {
        (body.0, top + search.3 + 420.0, body.2, 360.0)
    };
    k.panel(g, r, false);
    let (name, glyph, detail) = FILES[s.selected_file];
    icon(g, glyph, r.0 + 24.0, r.1 + 24.0, 24.0, k.accent);
    text(
        g,
        (r.0 + 62.0, r.1 + 27.0, r.2 - 84.0, 22.0),
        name,
        15.0,
        k.ink,
        true,
    );
    text(
        g,
        (r.0 + 24.0, r.1 + 68.0, r.2 - 48.0, 18.0),
        detail,
        11.5,
        k.dim,
        false,
    );
    let canvas = (r.0 + 20.0, r.1 + 104.0, r.2 - 40.0, r.3 - 134.0);
    g.round_rect_fill(
        canvas.0,
        canvas.1,
        canvas.2,
        canvas.3,
        k.box_radius(),
        k.field,
    );
    if s.selected_file == 0 {
        let mini = (
            canvas.0 + 20.0,
            canvas.1 + 30.0,
            canvas.2 - 40.0,
            canvas.3 - 60.0,
        );
        k.panel(g, mini, true);
        text(
            g,
            (mini.0 + 16.0, mini.1 + 20.0, mini.2 - 32.0, 24.0),
            "이어서 해볼까요?",
            15.0,
            k.ink,
            true,
        );
        for i in 0..3 {
            let f = mini.0 + 18.0 + i as f32 * 50.0;
            if f + 36.0 < mini.0 + mini.2 {
                face(g, i, f, mini.1 + 62.0, 36.0, k);
            }
        }
        k.button(
            g,
            (mini.0 + 14.0, mini.1 + mini.3 - 46.0, mini.2 - 28.0, 32.0),
            "선택한 작업 열기",
            true,
            false,
            false,
        );
    } else {
        title(
            g,
            k,
            (canvas.0 + 18.0, canvas.1 + 24.0, canvas.2 - 36.0, 22.0),
            "디자인 검토",
        );
        paragraph(g,(canvas.0+18.0,canvas.1+66.0,canvas.2-36.0,150.0),"버튼의 크기와 형태를 확인해요. 선택 상태와 입력 포커스는 명확하게 구분하고, 창이 좁아지면 내용을 세로로 배치해요.",k.text,k.dim);
    }
    if wide {
        500.0
    } else {
        search.3 + 800.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_three_layouts_keep_navigation_and_content_inside_the_window() {
        for kind in [Kind::Cloud, Kind::Precision, Kind::Playroom] {
            for size in [
                (390.0, 844.0),
                (700.0, 600.0),
                (1100.0, 780.0),
                (1440.0, 900.0),
            ] {
                let l = layout(size, kind);
                for r in [l.nav, l.main, l.header, l.body] {
                    assert!(r.0 >= 0.0 && r.1 >= 0.0 && r.2 >= 0.0 && r.3 >= 0.0);
                    assert!(r.0 + r.2 <= size.0 + 0.01 && r.1 + r.3 <= size.1 + 0.01);
                }
                if l.compact {
                    assert_eq!(l.nav.2, 0.0);
                } else {
                    assert!(l.nav.0 + l.nav.2 <= l.main.0);
                }
            }
        }
    }
    #[test]
    fn preview_interactions_are_local_and_color_validation_keeps_the_last_valid_color() {
        let mut s = State::new(Kind::Cloud);
        s.pick(Action::Hex);
        s.key(&Key::Character("#12ABEF".into()), false);
        s.key(&Key::Named(NamedKey::Enter), false);
        assert_eq!(s.accent, rgb(0x12abef));
        s.hex = "#BAD".into();
        s.pick(Action::Apply);
        assert!(s.invalid);
        assert_eq!(s.accent, rgb(0x12abef));
        s.pick(Action::Font(99));
        assert_eq!(s.font, 20);
        s.pick(Action::Page(Page::Shared));
        assert_eq!(s.page, Page::Shared);
        s.pick(Action::Style(Kind::Playroom));
        assert_eq!(s.page, Page::Shared);
        assert!(s.portraits);
    }
    #[test]
    fn proposals_have_distinct_control_density_and_readable_core_text() {
        let skins = [
            Skin::new(Kind::Cloud, false, Kind::Cloud.accent()),
            Skin::new(Kind::Precision, true, Kind::Precision.accent()),
            Skin::new(Kind::Playroom, false, Kind::Playroom.accent()),
        ];
        assert_eq!(skins.map(|s| s.button), [36.0, 28.0, 40.0]);
        use kasa_gridview::palette::{contrast_of, luminance};
        for s in skins {
            assert!(contrast_of(luminance(s.ink), luminance(s.panel)) >= 4.5);
            assert!(contrast_of(luminance(s.dim), luminance(s.panel)) >= 4.5);
        }
    }
    #[test]
    fn study_icons_have_native_assets() {
        for name in [
            "terminal",
            "layout-grid",
            "folder",
            "settings-2",
            "search",
            "laptop",
            "git-branch",
            "file-text",
            "image",
            "claude",
            "codex",
        ] {
            assert!(
                gpu::GpuRenderer::icon_svg(name).is_some(),
                "missing icon: {name}"
            );
        }
    }
}
