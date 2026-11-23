//! 창들 — 창 하나에 기기 하나를 본다. 기기를 아직 안 골랐으면 고르기 화면이다. 보기·조작·HUD 는 본 앱 화면 탭과 같은
//! `kasa_castview` 다. 창마다 렌더러 하나, 연결은 그 창이 열려 있는 동안만 쥔다.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use kasa_castview::{key_mods, Click, ScreenView, Style, Surface};
use kasa_gridview::{GridFonts, GridRenderer};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{DeviceId, ElementState, Ime, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use winit::window::{CursorIcon, Fullscreen, Window, WindowAttributes, WindowId};

use crate::account::{self, Credential, Session};
use crate::login::{Action, Login};
use crate::net::Net;
use crate::rig::PickerAction;
use crate::roster::Machine;
use crate::scene::Room;

/// 보는 창의 연결이 닫히면 이만큼 뒤 다시 붙는다 — 그 기기 재시작·직통이 늦게 서는 경우. 본 앱 화면 탭과 같은 값.
const RETRY_AFTER: Duration = Duration::from_secs(3);
const FONT_SIZE: f32 = 13.0;
const PICKER_MIN: LogicalSize<f64> = LogicalSize::new(360.0, 300.0);
const BG: [u8; 4] = [24, 24, 27, 255];
const MUTE: [u8; 4] = [150, 150, 160, 255];
const ACCENT: [u8; 4] = [86, 132, 245, 255];
const EDGE: [u8; 4] = [255, 255, 255, 40];
const TEXTURE: &str = "kasadesk:screen";

pub enum Ev {
    /// 새 장·상태 — 다시 그린다.
    Wake,
    /// 그 기기를 연다(None 이면 고르기 창).
    Open(Option<String>),
    Roster { machines: Vec<Machine>, app_alive: bool },
    /// 로그인이 끝났다(성공이면 자격).
    Login(Result<Credential, String>),
    /// 관문이 켠 로그인 방법.
    Providers(Vec<String>),
    /// 관문이 자격을 거절했다(폐기·다른 곳에서 로그아웃) — 명부 일꾼이 이미 지웠다.
    Revoked,
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
    room: Room,
}

impl Win {
    fn logical(&self) -> (f32, f32) {
        let s = self.window.inner_size();
        let scale = self.gpu.scale.max(0.5);
        (s.width as f32 / scale, s.height as f32 / scale)
    }

    fn room_cursor(&self, count: usize) {
        self.window.set_cursor(if self.room.pointer_on_device() || self.room.pointer_on_page(count) {
            CursorIcon::Pointer
        } else {
            CursorIcon::Default
        });
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
    paths: Vec<String>,
    app_alive: bool,
    modifiers: ModifiersState,
    hangul: kasa_ime::Composer,
    focused: Option<WindowId>,
    test: crate::rig::Rig,
    session: Arc<Session>,
    state: std::path::PathBuf,
    login: Login,
}

impl Desk {
    pub fn new(proxy: EventLoopProxy<Ev>, net: Arc<Net>, open: Option<String>, session: Arc<Session>, state: std::path::PathBuf) -> Self {
        let test = crate::rig::Rig::from_env();
        let mut login = Login::default();
        login.open = test.login_panel;
        let desk = Self {
            proxy,
            net,
            windows: HashMap::new(),
            pending: vec![open],
            machines: Vec::new(),
            paths: Vec::new(),
            app_alive: false,
            modifiers: ModifiersState::empty(),
            hangul: kasa_ime::Composer::new(),
            focused: None,
            test,
            session,
            state,
            login,
        };
        if desk.login.open {
            desk.fetch_providers();
        }
        desk
    }

    fn fetch_providers(&self) {
        let proxy = self.proxy.clone();
        self.net.handle.spawn(async move {
            let list = account::providers(&account::client(), &account::gateway()).await;
            let _ = proxy.send_event(Ev::Providers(list));
        });
    }

    fn redraw_pickers(&self) {
        for w in self.windows.values().filter(|w| w.label.is_none()) {
            w.window.request_redraw();
        }
    }

    /// 로그인 판이 부른 일 — 관문 일은 카사넷 런타임에서 돌리고 결과를 창 사건으로 받는다.
    fn login_action(&mut self, action: Action) {
        match action {
            Action::None => return,
            Action::Redraw => {}
            Action::Opened => self.fetch_providers(),
            Action::Logout => {
                if let Some(cred) = self.session.get() {
                    self.net.handle.spawn(async move { account::logout(&account::client(), &cred).await });
                }
                account::clear(&self.state);
                self.session.set(None);
            }
            Action::Provider(provider) => {
                let (proxy, machine) = (self.proxy.clone(), account::install_id(&self.state));
                self.net.handle.spawn(async move {
                    let r = account::oauth_login(&account::client(), &account::gateway(), &provider, &machine, account::open_browser).await;
                    let _ = proxy.send_event(Ev::Login(r));
                });
            }
            Action::Password(id, pw) => {
                let (proxy, machine) = (self.proxy.clone(), account::install_id(&self.state));
                self.net.handle.spawn(async move {
                    let r = account::password_login(&account::client(), &account::gateway(), &id, &pw, &machine).await;
                    let _ = proxy.send_event(Ev::Login(r));
                });
            }
        }
        self.redraw_pickers();
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
            w.room.suspend();
            w.window.set_min_inner_size(None::<LogicalSize<f64>>);
            w.window.set_cursor(CursorIcon::Default);
            w.set_title();
            w.window.focus_window();
            w.window.request_redraw();
            return;
        }
        let attrs = WindowAttributes::default()
            .with_title("KasaDesk")
            .with_inner_size(LogicalSize::new(1280.0, 800.0));
        let attrs = if label.is_none() { attrs.with_min_inner_size(PICKER_MIN) } else { attrs };
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
        let mut win = Win {
            gpu, window, label, view: None, display: None, closed_since: None,
            cursor: (0.0, 0.0), room: Room::default(),
        };
        if win.label.is_none() {
            win.room.reflow(win.logical(), &self.machines);
        }
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
        let Some(w) = self.windows.get_mut(&id) else { return };
        let (ww, wh) = w.logical();
        w.gpu.clear_chrome();
        self.test.capture(&mut w.gpu);
        let result = if w.label.is_none() {
            let id = self.net.id().fmt_short();
            let origin = match (self.app_alive, self.session.get().is_some()) {
                (true, true) => "카사텀과 계정에서 기기를 받는 중",
                (true, false) => "카사텀에서 명부를 받는 중",
                (false, true) => "계정으로 기기를 받는 중",
                (false, false) => "카사텀이 꺼져 있어 지난 명부로 보여요 — 로그인하면 계정 기기도 보여요",
            };
            let foot = format!("보기 앱 {id} · {origin}");
            let (login, cred) = (&mut self.login, self.session.get());
            w.room.paint(&mut w.gpu, &self.machines, &self.paths, &foot, self.app_alive, |g, size| {
                login.paint_chip(g, size.0, cred.as_ref());
                login.paint_panel(g, size);
            })
        } else {
            w.gpu.rect(0.0, 0.0, ww, wh, BG);
            let fullscreen = w.fullscreen();
            if let Some(view) = w.view.as_mut() {
                let extra = ["기기", if fullscreen { "창으로" } else { "전체 화면" }];
                let style = Style { accent: ACCENT, mute: MUTE, edge: EDGE, extra: &extra };
                kasa_castview::paint(&mut w.gpu, view, TEXTURE, (0.0, 0.0, ww, wh), Surface::Window(id), w.cursor, &style);
            }
            w.gpu.render(0.0, true, |_, _, _, _, _, _| {})
        };
        if let Err(e) = result {
            eprintln!("[kasadesk] 그리기 실패: {e:#}");
        }
    }

    fn back_to_picker(w: &mut Win, machines: &[Machine]) {
        w.view = None;
        w.label = None;
        w.display = None;
        w.closed_since = None;
        w.gpu.drop_image(TEXTURE);
        w.window.set_min_inner_size(Some(PICKER_MIN));
        w.room.reflow(w.logical(), machines);
        w.set_title();
        w.window.request_redraw();
    }

    fn mouse(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: &WindowEvent) {
        let mods = key_mods(self.modifiers);
        let modifiers = self.modifiers;
        let Some(w) = self.windows.get_mut(&id) else { return };
        let p = (f64::from(w.cursor.0), f64::from(w.cursor.1));
        let Some(view) = w.view.as_mut() else {
            if w.label.is_some() {
                return;
            }
            if let WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } = event {
                let action = self.login.click(p, self.session.get().is_some());
                if !matches!(action, Action::None) || self.login.open {
                    self.login_action(action);
                    return;
                }
            }
            if self.login.open {
                return;
            }
            let redraw = match event {
                WindowEvent::CursorMoved { .. } => w.room.pointer(Some(w.cursor)),
                WindowEvent::CursorLeft { .. } => w.room.pointer(None),
                WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                    if w.room.click_page(&self.machines) {
                        true
                    } else {
                        if let Some(index) = w.room.hit() {
                            self.open(event_loop, Some(self.machines[index].label.clone()));
                        }
                        return;
                    }
                }
                _ => return,
            };
            w.room_cursor(self.machines.len());
            if redraw {
                w.window.request_redraw();
            }
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
                            Self::back_to_picker(w, &self.machines);
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
        let Some(view) = w.view.as_mut() else {
            if w.label.is_none() {
                if let Some(action) = self.login.key(event) {
                    self.login_action(action);
                    return;
                }
            }
            if !pressed || w.label.is_some() || m.super_key() || m.control_key() || m.alt_key() {
                return;
            }
            if let Some(code) = code {
                self.picker_key(event_loop, id, code, event.repeat);
            }
            return;
        };
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

    fn picker_key(&mut self, event_loop: &ActiveEventLoop, id: WindowId, code: KeyCode, repeat: bool) {
        let Some(w) = self.windows.get_mut(&id).filter(|w| w.label.is_none()) else { return };
        if matches!(code, KeyCode::Enter | KeyCode::NumpadEnter) {
            if !repeat {
                if let Some(index) = w.room.selected() {
                    self.open(event_loop, Some(self.machines[index].label.clone()));
                }
            }
        } else if w.room.keyboard(code, &self.machines) {
            w.window.request_redraw();
        }
    }

    fn pointer_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: &WindowEvent) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            if let Some(w) = self.windows.get_mut(&id) {
                let scale = w.gpu.scale.max(0.5);
                w.cursor = (position.x as f32 / scale, position.y as f32 / scale);
            }
        }
        self.mouse(event_loop, id, event);
    }

    fn picker_probe(&mut self, event_loop: &ActiveEventLoop, action: PickerAction) {
        let Some((&id, w)) = self.windows.iter().find(|(_, w)| w.label.is_none()) else { return };
        let (x, y, click) = match action {
            PickerAction::Hover(x, y) => (x, y, false),
            PickerAction::Click(x, y) => (x, y, true),
            PickerAction::Key(code) => {
                self.picker_key(event_loop, id, code, false);
                return;
            }
        };
        let size = w.window.inner_size();
        let position = PhysicalPosition::new(x * f64::from(size.width), y * f64::from(size.height));
        let device_id = DeviceId::dummy();
        self.pointer_event(event_loop, id, &WindowEvent::CursorMoved { device_id, position });
        if click {
            self.pointer_event(event_loop, id, &WindowEvent::MouseInput {
                device_id, state: ElementState::Pressed, button: MouseButton::Left,
            });
        }
    }
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
        if let Some((id, pw)) = self.test.login.take() {
            self.login_action(Action::Password(id, pw));
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: Ev) {
        match event {
            Ev::Wake => {
                for w in self.windows.values().filter(|w| w.label.is_some()) {
                    w.window.request_redraw();
                }
            }
            Ev::Open(label) => self.open(event_loop, label),
            Ev::Login(result) => {
                let result = result.and_then(|c| {
                    account::save(&self.state, &c).map_err(|e| format!("로그인 정보를 못 남겼어요: {e}"))?;
                    eprintln!("[kasadesk] 계정 {} 로그인", c.account);
                    self.session.set(Some(c));
                    Ok(())
                });
                self.login.done(result);
                self.redraw_pickers();
            }
            Ev::Providers(list) => {
                self.login.providers = list;
                self.redraw_pickers();
            }
            Ev::Revoked => self.redraw_pickers(),
            Ev::Roster { machines, app_alive } => {
                let paths = machines.iter().map(|m| {
                    let path = self.net.path_text(&m.label);
                    if m.fresh { path } else { format!("{path} · 지난 주소") }
                }).collect::<Vec<_>>();
                let reflow = machines.len() != self.machines.len() || machines.iter().zip(&self.machines)
                    .any(|(m, old)| m.label != old.label || m.kind != old.kind);
                let redraw = reflow || paths != self.paths || app_alive != self.app_alive;
                self.machines = machines;
                self.paths = paths;
                self.app_alive = app_alive;
                if redraw {
                    for w in self.windows.values_mut().filter(|w| w.label.is_none()) {
                        if reflow {
                            w.room.reflow(w.logical(), &self.machines);
                            w.room_cursor(self.machines.len());
                        }
                        w.window.request_redraw();
                    }
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
                    if w.label.is_none() {
                        w.room.reflow(w.logical(), &self.machines);
                        w.room_cursor(self.machines.len());
                    }
                    w.window.request_redraw();
                }
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                if let Some(w) = self.windows.get_mut(&id) {
                    let old_scale = w.gpu.scale.max(0.5);
                    w.gpu.set_scale(w.window.scale_factor() as f32);
                    let ratio = old_scale / w.gpu.scale.max(0.5);
                    w.cursor = (w.cursor.0 * ratio, w.cursor.1 * ratio);
                    if w.label.is_none() {
                        w.room.rescale_pointer(ratio);
                    }
                    w.gpu.set_font_size(FONT_SIZE);
                    let size = w.window.inner_size();
                    w.gpu.resize(size.width, size.height);
                    if w.label.is_none() {
                        w.room.reflow(w.logical(), &self.machines);
                        w.room_cursor(self.machines.len());
                    }
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
                match self.windows.get(&id) {
                    Some(w) if w.label.is_none() => {
                        if self.login.commit(text) {
                            w.window.request_redraw();
                        }
                    }
                    Some(w) => {
                        if let Some(view) = w.view.as_ref() {
                            view.commit(text);
                        }
                    }
                    None => {}
                }
            }
            WindowEvent::CursorMoved { .. } | WindowEvent::CursorLeft { .. } | WindowEvent::MouseInput { .. }
            | WindowEvent::MouseWheel { .. } | WindowEvent::PinchGesture { .. } => {
                // On a rig the hardware cursor can leave the window during a scripted hover.
                if !self.test.drives_picker() || self.windows.get(&id).is_some_and(|w| w.label.is_some()) {
                    self.pointer_event(event_loop, id, &event);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        for action in self.test.take_picker_actions(Instant::now()) {
            self.picker_probe(event_loop, action);
        }
        self.sync();
        let mut views: Vec<&mut ScreenView> = self.windows.values_mut().filter_map(|w| w.view.as_mut()).collect();
        if self.test.tick(&mut views) {
            event_loop.exit();
            return;
        }
        let now = Instant::now();
        let mut next = self.test.next_wake(now);
        if self.windows.values().any(|w| w.label.is_some()) {
            schedule(&mut next, now + Duration::from_millis(500));
        }
        if self.test.capture_due(now) {
            if let Some(w) = self.windows.values().next() {
                w.window.request_redraw();
            }
        }
        for w in self.windows.values().filter(|w| w.label.is_none()) {
            if let Some(at) = w.room.next_frame() {
                if at <= now {
                    w.window.request_redraw();
                } else {
                    schedule(&mut next, at);
                }
            }
        }
        event_loop.set_control_flow(next.map_or(ControlFlow::Wait, ControlFlow::WaitUntil));
    }
}

fn schedule(next: &mut Option<Instant>, at: Instant) {
    if next.is_none_or(|t| at < t) {
        *next = Some(at);
    }
}
