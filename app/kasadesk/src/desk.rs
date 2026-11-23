//! 창들 — 창 하나에 기기 하나를 본다. 기기를 아직 안 골랐으면 고르기 화면이다. 보기·조작·HUD 는 본 앱 화면 탭과 같은
//! `kasa_castview` 다. 창마다 렌더러 하나, 연결은 그 창이 열려 있는 동안만 쥔다.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use kasa_castview::{contains, key_mods, Click, Rect, ScreenView, Style, Surface};
use kasa_gridview::{GridFonts, GridRenderer};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, Ime, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use winit::window::{Fullscreen, Window, WindowAttributes, WindowId};

use crate::net::Net;
use crate::roster::Machine;

/// 보는 창의 연결이 닫히면 이만큼 뒤 다시 붙는다 — 그 기기 재시작·직통이 늦게 서는 경우. 본 앱 화면 탭과 같은 값.
const RETRY_AFTER: Duration = Duration::from_secs(3);
const FONT_SIZE: f32 = 13.0;
const BG: [u8; 4] = [24, 24, 27, 255];
const TEXT: [u8; 4] = [236, 236, 240, 255];
const MUTE: [u8; 4] = [150, 150, 160, 255];
const ACCENT: [u8; 4] = [86, 132, 245, 255];
const HOVER: [u8; 4] = [255, 255, 255, 18];
const EDGE: [u8; 4] = [255, 255, 255, 40];
const TEXTURE: &str = "kasadesk:screen";
const PAD: f32 = 24.0;
const ROW_H: f32 = 44.0;
const LIST_TOP: f32 = 84.0;

pub enum Ev {
    /// 새 장·상태 — 다시 그린다.
    Wake,
    /// 그 기기를 연다(None 이면 고르기 창).
    Open(Option<String>),
    Roster { machines: Vec<Machine>, app_alive: bool },
    /// 맥 Caps Lock — winit 이 키로 안 넘겨 AppKit 감시가 보낸다.
    #[cfg(target_os = "macos")]
    CapsLock,
}

/// `gpu` 가 `window` 앞에 선언된다 — surface 는 창이 살아 있을 때 먼저 놓아야 한다.
struct Win {
    gpu: GridRenderer,
    window: Arc<Window>,
    /// 보는 기기. None 이면 고르기 화면.
    label: Option<String>,
    view: Option<ScreenView>,
    /// 「화면 2/3」으로 고른 그 기기 화면 번호.
    display: Option<u32>,
    closed_since: Option<Instant>,
    cursor: (f32, f32),
    /// 고르기 화면에서 줄마다 그린 자리.
    rows: Vec<(Rect, String)>,
}

impl Win {
    fn logical(&self) -> (f32, f32) {
        let s = self.window.inner_size();
        let scale = self.gpu.scale.max(0.5);
        (s.width as f32 / scale, s.height as f32 / scale)
    }

    fn set_title(&self) {
        let title = self.label.as_deref().map_or_else(|| "KasaDesk".to_string(), |l| format!("{l} — KasaDesk"));
        self.window.set_title(&title);
    }

    fn fullscreen(&self) -> bool {
        self.window.fullscreen().is_some()
    }

    fn toggle_fullscreen(&self) {
        self.window.set_fullscreen(if self.fullscreen() { None } else { Some(Fullscreen::Borderless(None)) });
    }
}

pub struct Desk {
    proxy: EventLoopProxy<Ev>,
    net: Arc<Net>,
    windows: HashMap<WindowId, Win>,
    pending: Vec<Option<String>>,
    machines: Vec<Machine>,
    app_alive: bool,
    modifiers: ModifiersState,
    hangul: kasa_ime::Composer,
    focused: Option<WindowId>,
    test: crate::rig::Rig,
}

impl Desk {
    pub fn new(proxy: EventLoopProxy<Ev>, net: Arc<Net>, open: Option<String>) -> Self {
        Self {
            proxy,
            net,
            windows: HashMap::new(),
            pending: vec![open],
            machines: Vec::new(),
            app_alive: false,
            modifiers: ModifiersState::empty(),
            hangul: kasa_ime::Composer::new(),
            focused: None,
            test: crate::rig::Rig::from_env(),
        }
    }

    /// 그 기기를 연다 — 이미 보는 창이 있으면 그 창을 앞으로, 고르기 창이 있으면 거기서, 없으면 새 창.
    fn open(&mut self, event_loop: &ActiveEventLoop, label: Option<String>) {
        if let Some(w) = label.as_ref().and_then(|l| self.windows.values().find(|w| w.label.as_ref() == Some(l))) {
            w.window.focus_window();
            return;
        }
        if label.is_none() {
            if let Some(w) = self.windows.values().next() {
                w.window.focus_window();
                return;
            }
        }
        if let Some(w) = self.windows.values_mut().find(|w| w.label.is_none()) {
            w.label = label;
            w.set_title();
            w.window.focus_window();
            w.window.request_redraw();
            return;
        }
        let attrs = WindowAttributes::default().with_title("KasaDesk").with_inner_size(LogicalSize::new(1280.0, 800.0));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("[kasadesk] 창을 못 만든다: {e}");
                return;
            }
        };
        // 본 앱과 같은 입력기 길 — macOS 는 이 앱이 한글을 조합하고, 그 밖은 OS 입력기.
        window.set_ime_allowed(cfg!(not(target_os = "macos")));
        let gpu = match GridRenderer::new(window.clone(), FONT_SIZE, GridFonts { primary: None, bundled: Vec::new() }) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("[kasadesk] 렌더러를 못 만든다: {e:#}");
                return;
            }
        };
        let win = Win { gpu, window, label, view: None, display: None, closed_since: None, cursor: (0.0, 0.0), rows: Vec::new() };
        win.set_title();
        win.window.request_redraw();
        self.windows.insert(win.window.id(), win);
    }

    /// 보는 창만 연결을 쥔다. 닫힌 연결은 `RETRY_AFTER` 뒤 다시 붙고, 그동안의 까닭은 이어 보인다.
    fn sync(&mut self) {
        let now = Instant::now();
        for w in self.windows.values_mut() {
            let Some(label) = w.label.clone() else { continue };
            let previous = match &w.view {
                Some(v) if !v.closed() => {
                    w.closed_since = None;
                    continue;
                }
                Some(v) => {
                    let since = *w.closed_since.get_or_insert(now);
                    if now.duration_since(since) < RETRY_AFTER {
                        continue;
                    }
                    Some(v.status())
                }
                None => None,
            };
            w.view = None;
            w.gpu.drop_image(TEXTURE);
            let proxy = self.proxy.clone();
            let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || drop(proxy.send_event(Ev::Wake)));
            let fit = w.window.available_monitors().map(|m| m.size()).fold((0, 0), |a, s| if s.width * s.height > a.0 * a.1 { (s.width, s.height) } else { a });
            let start = self.net.start_view(&label);
            if let Some(view) = ScreenView::open(&label, fit, w.display, self.test.silent_audio, start, wake) {
                if let Some(previous) = previous {
                    view.carry_status(&previous);
                }
                w.view = Some(view);
                w.closed_since = None;
            }
            w.window.request_redraw();
        }
    }

    fn paint(&mut self, id: WindowId) {
        let (machines, app_alive, net) = (&self.machines, self.app_alive, self.net.clone());
        let Some(w) = self.windows.get_mut(&id) else { return };
        let (ww, wh) = w.logical();
        w.gpu.clear_chrome();
        w.gpu.rect(0.0, 0.0, ww, wh, BG);
        let fullscreen = w.fullscreen();
        if let Some(view) = w.view.as_mut() {
            let extra = ["기기", if fullscreen { "창으로" } else { "전체 화면" }];
            let style = Style { accent: ACCENT, mute: MUTE, edge: EDGE, extra: &extra };
            kasa_castview::paint(&mut w.gpu, view, TEXTURE, (0.0, 0.0, ww, wh), Surface::Window(id), w.cursor, &style);
        } else {
            w.rows = picker(&mut w.gpu, (ww, wh), w.cursor, machines, app_alive, &net);
        }
        self.test.capture(&mut w.gpu);
        if let Err(e) = w.gpu.render(0.0, true, |_, _, _, _, _, _| {}) {
            eprintln!("[kasadesk] 그리기 실패: {e:#}");
        }
    }

    fn back_to_picker(w: &mut Win) {
        w.view = None;
        w.label = None;
        w.display = None;
        w.gpu.drop_image(TEXTURE);
        w.set_title();
        w.window.request_redraw();
    }

    fn mouse(&mut self, id: WindowId, event: &WindowEvent) {
        let mods = key_mods(self.modifiers);
        let modifiers = self.modifiers;
        let Some(w) = self.windows.get_mut(&id) else { return };
        let p = (f64::from(w.cursor.0), f64::from(w.cursor.1));
        let Some(view) = w.view.as_mut() else {
            if let WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } = event {
                if let Some((_, label)) = w.rows.iter().find(|(r, _)| contains(*r, p)) {
                    w.label = Some(label.clone());
                    w.set_title();
                }
            }
            w.window.request_redraw();
            return;
        };
        let redraw = match event {
            WindowEvent::MouseInput { state, button, .. } => {
                let bit = match button {
                    MouseButton::Left => 1,
                    MouseButton::Right => 2,
                    MouseButton::Middle => 4,
                    _ => return,
                };
                let pressed = *state == ElementState::Pressed;
                if view.menu_click(p, bit, pressed) {
                    true
                } else {
                    match view.click(p, bit, pressed, mods) {
                        Click::Ignored => false,
                        Click::Handled | Click::Pressed => true,
                        Click::Display(next) => {
                            w.display = Some(next);
                            w.view = None;
                            true
                        }
                        Click::Extra(0) => {
                            Self::back_to_picker(w);
                            true
                        }
                        Click::Extra(_) => {
                            w.toggle_fullscreen();
                            true
                        }
                    }
                }
            }
            WindowEvent::CursorMoved { .. } => view.hover(p, mods) && view.key_menu.is_some(),
            WindowEvent::MouseWheel { delta, .. } => view.wheel(p, delta, modifiers),
            WindowEvent::PinchGesture { delta, .. } => view.pinch(p, *delta),
            _ => false,
        };
        if redraw {
            w.window.request_redraw();
        }
    }

    fn key(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: &winit::event::KeyEvent) {
        let m = self.modifiers;
        // 창 닫기·붙여넣기·전체 화면은 이 앱이 받는다 — 본 앱 꺼낸 화면 창과 같은 손가락(맥 ⌘, Windows Ctrl+Shift — 붙여넣기는 Ctrl+V 도).
        let host = if cfg!(target_os = "macos") { m.super_key() } else { m.control_key() && m.shift_key() };
        let pressed = event.state == ElementState::Pressed;
        let code = match event.physical_key {
            PhysicalKey::Code(c) => Some(c),
            PhysicalKey::Unidentified(_) => None,
        };
        if pressed && host && code == Some(KeyCode::KeyW) {
            self.windows.remove(&id);
            if self.windows.is_empty() {
                event_loop.exit();
            }
            return;
        }
        let Some(w) = self.windows.get_mut(&id) else { return };
        if pressed && cfg!(target_os = "macos") && m.super_key() && m.control_key() && code == Some(KeyCode::KeyF) {
            w.toggle_fullscreen();
            return;
        }
        let Some(view) = w.view.as_mut() else { return };
        let paste = pressed && code == Some(KeyCode::KeyV) && (host || (cfg!(windows) && m.control_key() && !m.alt_key()));
        if paste {
            if !event.repeat && view.control {
                if let Some(text) = arboard::Clipboard::new().ok().and_then(|mut c| c.get_text().ok()) {
                    view.commit(&text);
                }
            }
            return;
        }
        if view.key_event(&mut self.hangul, event, m) {
            w.window.request_redraw();
        }
    }
}

/// 고르기 화면 — 명부 기기마다 한 줄(이름·길). 줄을 누르면 그 기기를 이 창에서 본다.
fn picker(g: &mut GridRenderer, (w, h): (f32, f32), cursor: (f32, f32), machines: &[Machine], app_alive: bool, net: &Net) -> Vec<(Rect, String)> {
    g.draw_label(PAD, PAD, "KasaDesk", 20.0, TEXT);
    g.draw_label(PAD, PAD + 30.0, "화면을 볼 기기를 고르세요 — 이 앱은 카사텀을 다시 띄워도 화면이 이어져요", 13.0, MUTE);
    let mut rows = Vec::with_capacity(machines.len());
    if machines.is_empty() {
        let text = if app_alive { "볼 기기가 없어요 — 카사텀 기기 명부에 기기를 더하면 여기 보여요" } else { "카사텀을 띄우면 기기 명부를 배워요" };
        g.draw_label(PAD, LIST_TOP + 12.0, text, 14.0, MUTE);
    }
    for (i, m) in machines.iter().enumerate() {
        let r = (PAD - 8.0, LIST_TOP + ROW_H * i as f32, (w - PAD * 2.0 + 16.0).max(0.0), ROW_H - 4.0);
        if contains(r, (f64::from(cursor.0), f64::from(cursor.1))) {
            g.round_rect_fill(r.0, r.1, r.2, r.3, 6.0, HOVER);
        }
        g.draw_label(PAD, r.1 + 12.0, &m.label, 15.0, TEXT);
        let path = net.path_text(&m.label);
        let path = if m.fresh { path } else { format!("{path} · 지난 주소") };
        let pw = g.measure_label(&path, 12.0);
        g.draw_label(r.0 + r.2 - 8.0 - pw, r.1 + 14.0, &path, 12.0, MUTE);
        rows.push((r, m.label.clone()));
    }
    let id = net.id().to_string();
    let foot = format!(
        "보기 앱 {} · {}",
        &id[..id.len().min(10)],
        if app_alive { "카사텀에서 명부를 받는 중" } else { "카사텀이 꺼져 있어 지난 명부로 보여요" }
    );
    g.draw_label(PAD, h - PAD - 12.0, &foot, 11.0, MUTE);
    rows
}

impl ApplicationHandler<Ev> for Desk {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // NSApplication 이 선 뒤에 건다(한 번만 걸린다).
        #[cfg(target_os = "macos")]
        {
            let proxy = self.proxy.clone();
            kasa_castview::watch_caps_lock(move || drop(proxy.send_event(Ev::CapsLock)));
        }
        for open in std::mem::take(&mut self.pending) {
            self.open(event_loop, open);
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: Ev) {
        match event {
            Ev::Wake => {
                for w in self.windows.values() {
                    w.window.request_redraw();
                }
            }
            Ev::Open(label) => self.open(event_loop, label),
            Ev::Roster { machines, app_alive } => {
                self.machines = machines;
                self.app_alive = app_alive;
                for w in self.windows.values().filter(|w| w.view.is_none()) {
                    w.window.request_redraw();
                }
            }
            #[cfg(target_os = "macos")]
            Ev::CapsLock => {
                if let Some(view) = self.focused.and_then(|id| self.windows.get(&id)).and_then(|w| w.view.as_ref()) {
                    view.caps_lock();
                }
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        match &event {
            WindowEvent::CloseRequested => {
                self.windows.remove(&id);
                if self.windows.is_empty() {
                    event_loop.exit();
                }
            }
            WindowEvent::RedrawRequested => self.paint(id),
            WindowEvent::Resized(size) => {
                if let Some(w) = self.windows.get_mut(&id) {
                    w.gpu.resize(size.width, size.height);
                    w.window.request_redraw();
                }
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                if let Some(w) = self.windows.get_mut(&id) {
                    w.gpu.set_scale(w.window.scale_factor() as f32);
                    w.gpu.set_font_size(FONT_SIZE);
                    let size = w.window.inner_size();
                    w.gpu.resize(size.width, size.height);
                    w.window.request_redraw();
                }
            }
            WindowEvent::Focused(f) => {
                if *f {
                    self.focused = Some(id);
                } else if self.focused == Some(id) {
                    self.focused = None;
                }
            }
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::KeyboardInput { event, .. } => {
                let event = event.clone();
                self.key(event_loop, id, &event);
            }
            WindowEvent::Ime(Ime::Commit(text)) => {
                if let Some(view) = self.windows.get(&id).and_then(|w| w.view.as_ref()) {
                    view.commit(text);
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                if let Some(w) = self.windows.get_mut(&id) {
                    let scale = w.gpu.scale.max(0.5);
                    w.cursor = (position.x as f32 / scale, position.y as f32 / scale);
                }
                self.mouse(id, &event);
            }
            WindowEvent::MouseInput { .. } | WindowEvent::MouseWheel { .. } | WindowEvent::PinchGesture { .. } => self.mouse(id, &event),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.sync();
        let mut views: Vec<&mut ScreenView> = self.windows.values_mut().filter_map(|w| w.view.as_mut()).collect();
        if self.test.tick(&mut views) {
            event_loop.exit();
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(500)));
    }
}
