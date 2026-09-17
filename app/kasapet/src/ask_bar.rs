//! 펫 머리 옆에 붙는 짧은 유리 바 — 한 줄 묻는 자리다. 답은 여기 안 찍히고 펫 말풍선으로
//! 나간다(2026-09-17 지시 「답변이 채팅창 밖으로」) — 바는 입력 한 줄이 전부라 늘 띄워
//! 둬도 바탕화면을 가리지 않는다. 머리 **위**가 아니라 **옆**인 것은 말풍선과 겹치지
//! 않기 위해서다(같은 날 「입력하는 것도 안 겹치게」) — 위는 나쵸가 말하는 자리다.
//! 빈 곳을 잡고 끌면 옮겨지고, 옮긴 만큼은 펫을 따라다니며 유지된다.
//!
//! 대화창(`chat_panel.rs`)과 따로 두는 이유는 쓰는 순간이 다르기 때문이다. 저쪽은
//! 앉아서 읽는 판이라 360x260 을 차지해도 되지만, 이쪽은 하던 일을 멈추지 않은 채
//! 한 마디 던지는 자리라 바탕화면을 가리면 그 자체로 방해가 된다. 그래서 바탕이
//! 유리다 — 아래 창을 가리지 않고 그 위에 얹힌 것처럼 보인다.
use std::cell::RefCell;
use std::ffi::CStr;
use std::rc::Rc;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObject, NSObjectProtocol};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadOnly};
use objc2_app_kit::*;
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize, NSString};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// 바의 너비. 한 줄 묻는 자리라 대화창의 절반쯤이면 넉넉하다.
const WIDTH: f64 = 320.0;
/// 입력 한 줄이 앉는 띠의 높이.
const ROW: f64 = 46.0;

pub enum Event { Send(String), Close }

struct Ivars { input: Retained<NSTextField>, events: Rc<RefCell<Vec<Event>>> }

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "KasapetAskBody"]
    struct Body;
    unsafe impl NSObjectProtocol for Body {}
    impl Body {
        #[unsafe(method(acceptsFirstMouse:))]
        fn first_mouse(&self, _event: Option<&NSEvent>) -> bool { true }
        /// 입력칸·× 를 비켜 간 곳을 잡으면 바가 끌린다.
        #[unsafe(method(mouseDown:))]
        fn drag(&self, event: &NSEvent) {
            if let Some(window) = self.window() { window.performWindowDragWithEvent(event); }
        }
    }
);
define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "KasapetAskActions"]
    #[ivars = Ivars]
    struct Actions;
    unsafe impl NSObjectProtocol for Actions {}
    impl Actions {
        #[unsafe(method(send:))]
        fn send(&self, _sender: &AnyObject) {
            let text = self.ivars().input.stringValue().to_string();
            if !text.trim().is_empty() {
                self.ivars().events.borrow_mut().push(Event::Send(text));
            }
        }
        #[unsafe(method(hide:))]
        fn hide(&self, _sender: &AnyObject) { self.ivars().events.borrow_mut().push(Event::Close); }
    }
);

pub struct Bar {
    panel: Retained<NSPanel>,
    parent: Retained<NSWindow>,
    /// 검증 창구(`snapshot`, 디버그 빌드에만 있다)가 그림을 뜨는 판.
    #[cfg_attr(not(debug_assertions), allow(dead_code))]
    body: Retained<NSView>,
    input: Retained<NSTextField>,
    _actions: Retained<Actions>,
    events: Rc<RefCell<Vec<Event>>>,
    origin: RefCell<NSPoint>,
    /// 사람이 끌어 옮긴 만큼. 제자리(머리 옆)에 이걸 더한 곳이 바의 자리다.
    offset: RefCell<(f64, f64)>,
    /// 지난 동기화 때 펫 창의 자리 — 펫이 움직인 프레임에는 바의 이동을 사람 손으로 안 친다.
    parent_origin: RefCell<(f64, f64)>,
    shown: RefCell<std::time::Instant>,
}

impl Bar {
    pub fn new(window: &winit::window::Window) -> Option<Self> {
        let mtm = MainThreadMarker::new()?;
        let RawWindowHandle::AppKit(handle) = window.window_handle().ok()?.as_raw() else { return None };
        let view: &NSView = unsafe { handle.ns_view.cast().as_ref() };
        let parent = view.window()?;
        Self::from_parent(parent, mtm)
    }

    fn from_parent(parent: Retained<NSWindow>, mtm: MainThreadMarker) -> Option<Self> {
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm), rect(0.0, 0.0, WIDTH, ROW),
            // 제목줄을 그대로 두고 투명하게만 만든다. 통째로 없애면(Borderless) 창이
            // 키를 못 받아 글자를 칠 수 없다 — 이 바는 입력이 전부인 창이다.
            NSWindowStyleMask::Titled | NSWindowStyleMask::FullSizeContentView
                | NSWindowStyleMask::NonactivatingPanel | NSWindowStyleMask::UtilityWindow,
            NSBackingStoreType::Buffered, false,
        );
        unsafe { panel.setReleasedWhenClosed(false); }
        panel.setTitlebarAppearsTransparent(true);
        panel.setTitleVisibility(NSWindowTitleVisibility::Hidden);
        panel.setOpaque(false);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        panel.setHasShadow(false);
        panel.setHidesOnDeactivate(false);
        panel.setBecomesKeyOnlyIfNeeded(false);
        panel.setMovable(true);
        panel.setMovableByWindowBackground(false);
        for button in [NSWindowButton::CloseButton, NSWindowButton::MiniaturizeButton, NSWindowButton::ZoomButton] {
            if let Some(button) = panel.standardWindowButton(button) { button.setHidden(true); }
        }

        let body: Retained<Body> = unsafe { msg_send![Body::alloc(mtm), initWithFrame: rect(0.0, 0.0, WIDTH, ROW)] };
        let body: Retained<NSView> = Retained::into_super(body);
        let input = NSTextField::initWithFrame(NSTextField::alloc(mtm), rect(14.0, 11.0, WIDTH - 14.0 - 34.0, 24.0));
        input.setPlaceholderString(Some(&NSString::from_str("나쵸에게 물어보세요 — 이 창이든, 모든 기기든")));
        input.setFont(Some(&NSFont::systemFontOfSize(13.0)));
        input.setMaximumNumberOfLines(1);
        input.setBezeled(false);
        input.setDrawsBackground(false);
        input.setFocusRingType(NSFocusRingType::None);
        body.addSubview(&input);

        let events = Rc::new(RefCell::new(Vec::new()));
        let actions = Actions::alloc(mtm).set_ivars(Ivars { input: input.clone(), events: events.clone() });
        let actions: Retained<Actions> = unsafe { msg_send![super(actions), init] };
        unsafe { input.setTarget(Some(&actions)); input.setAction(Some(sel!(send:))); }

        let close = unsafe { NSButton::buttonWithTitle_target_action(&NSString::from_str("×"), Some(&actions), Some(sel!(hide:)), mtm) };
        close.setFrame(rect(WIDTH - 30.0, 12.0, 22.0, 22.0));
        close.setBordered(false);
        close.setFont(Some(&NSFont::systemFontOfSize(15.0)));
        // Esc 는 이 단추에 걸려 있다. 숨긴 단추는 키를 못 받아서, 닫는 길이 눈에도
        // 보이는 편이 낫다.
        close.setKeyEquivalent(&NSString::from_str("\u{1b}"));
        body.addSubview(&close);

        let backdrop = backdrop(mtm, &body);
        panel.setContentView(Some(&backdrop));
        unsafe { parent.addChildWindow_ordered(&panel, NSWindowOrderingMode::Above); }
        panel.orderOut(None);
        Some(Self {
            panel, parent, body, input, _actions: actions, events,
            origin: RefCell::new(NSPoint::new(0.0, 0.0)),
            offset: RefCell::new((0.0, 0.0)),
            parent_origin: RefCell::new((f64::NAN, f64::NAN)),
            shown: RefCell::new(std::time::Instant::now()),
        })
    }

    /// 띄우고 키를 준다 — 사람이 부른 바다.
    pub fn show(&self) {
        *self.shown.borrow_mut() = std::time::Instant::now();
        self.panel.makeKeyAndOrderFront(None);
        self.panel.makeFirstResponder(Some(&*self.input));
    }
    /// 키를 뺏지 않고 띄운다 — 상시 표시로 펫이 뜰 때 쓰는 길. 여기서 키를 가져가면
    /// 펫이 켜지는 순간 사람이 치던 창에서 커서가 사라진다.
    pub fn show_quiet(&self) {
        *self.shown.borrow_mut() = std::time::Instant::now();
        self.panel.orderFront(None);
    }
    /// 이미 떠 있는 바에 커서를 준다.
    pub fn focus(&self) {
        self.panel.makeKeyWindow();
        self.panel.makeFirstResponder(Some(&*self.input));
    }
    pub fn hide(&self) { self.panel.orderOut(None); }
    pub fn offset(&self) -> (f64, f64) { *self.offset.borrow() }
    pub fn set_offset(&self, offset: (f64, f64)) { *self.offset.borrow_mut() = offset; }
    pub fn visible(&self) -> bool { self.panel.isVisible() }
    pub fn clear_input(&self) { self.input.setStringValue(&NSString::new()); }
    pub fn events(&self) -> Vec<Event> { self.events.borrow_mut().drain(..).collect() }

    /// 다른 창으로 포커스가 넘어갔는가 — 넘어갔으면 바를 접는다. 방금 띄운 창은
    /// 아직 키를 못 받았을 수 있어 잠깐 봐준다.
    pub fn lost_focus(&self) -> bool {
        self.visible()
            && self.shown.borrow().elapsed() > std::time::Duration::from_millis(400)
            && !self.panel.isKeyWindow()
            && !self.parent.isKeyWindow()
    }

    /// 검증 창구 — 바가 실제로 앉은 자리(x, y, 너비, 높이).
    pub fn probe_frame(&self) -> (f64, f64, f64, f64) {
        let f = self.panel.frame();
        (f.origin.x, f.origin.y, f.size.width, f.size.height)
    }

    /// 펫이 움직이면 따라온다. `headroom` 은 창 위쪽에서 캐릭터 머리까지 비워 둔 높이라,
    /// 그만큼 내려온 곳이 머리 높이다. 펫이 안 움직였는데 바가 우리가 둔 자리에서 벗어나
    /// 있으면 그건 사람이 끈 것이다 — 그만큼을 기억해 두고 이후로도 그 자리를 지킨다.
    pub fn sync(&self, headroom: f64) {
        if !self.visible() { return; }
        let Some(screen) = self.parent.screen() else { return };
        let bounds = screen.visibleFrame();
        let parent = self.parent.frame();
        let parent_moved = {
            let last = *self.parent_origin.borrow();
            (last.0 - parent.origin.x).abs() > 0.5 || (last.1 - parent.origin.y).abs() > 0.5
        };
        *self.parent_origin.borrow_mut() = (parent.origin.x, parent.origin.y);
        let bar = self.panel.frame();
        let (bx, by) = placement(
            (parent.origin.x, parent.origin.y, parent.size.width, parent.size.height),
            (bar.size.width, bar.size.height), headroom,
            (bounds.origin.x, bounds.origin.y, bounds.size.width, bounds.size.height),
        );
        let previous = *self.origin.borrow();
        if !parent_moved && ((bar.origin.x - previous.x).abs() > 0.5 || (bar.origin.y - previous.y).abs() > 0.5) {
            *self.offset.borrow_mut() = (bar.origin.x - bx, bar.origin.y - by);
        }
        let offset = *self.offset.borrow();
        let (x, y) = (bx + offset.0, by + offset.1);
        if (previous.x - x).abs() > 0.5 || (previous.y - y).abs() > 0.5 {
            self.panel.setFrameOrigin(NSPoint::new(x, y));
            *self.origin.borrow_mut() = NSPoint::new(x, y);
        }
        self.panel.setLevel(self.parent.level());
    }
}

/// 사람 눈 대신 쓰는 창구 — 가짜 장부 서버를 하나 띄워 놓고 바에 질문을 넣어 답이
/// 실제로 찍히는 데까지를 돌린다. 서버·바·클라이언트가 한 줄로 이어지는지는 이 길로만
/// 확인된다(화면 캡처는 창 번호를 받아 밖에서 찍는다).
#[cfg(debug_assertions)]
pub fn probe() {
    use objc2_foundation::{NSDate, NSDefaultRunLoopMode};
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        for body in [
            r#"{"ok":true,"version":1,"service":"request-journal"}"#,
            r#"{"answer":"아즈사가 이 창에서 파일을 고치는 중이에요.","actions":[{"kind":"migrate_pane","ok":true,"detail":"맥미니로 이사"}]}"#,
        ] {
            let (mut socket, _) = listener.accept().unwrap();
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                let mut byte = [0; 1];
                socket.read_exact(&mut byte).unwrap();
                head.push(byte[0]);
            }
            let head = String::from_utf8(head).unwrap();
            if let Some(length) = head.lines().find_map(|line| line.strip_prefix("Content-Length: ")) {
                let mut input = vec![0; length.trim().parse().unwrap()];
                socket.read_exact(&mut input).unwrap();
                if head.starts_with("POST /api/ask") {
                    let value: serde_json::Value = serde_json::from_slice(&input).unwrap();
                    println!("ASK_PROBE_REQUEST_PANE:{}", value["pane"].as_str().unwrap_or(""));
                    println!("ASK_PROBE_REQUEST_TEXT:{}", value["text"].as_str().unwrap_or(""));
                }
            }
            write!(socket, "HTTP/1.0 200 OK\r\nContent-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
        }
    });
    let service = std::env::temp_dir().join(format!("kasapet-ask-probe-{}.json", std::process::id()));
    std::fs::write(&service, format!(r#"{{"version":1,"base_url":"http://127.0.0.1:{port}"}}"#)).unwrap();

    let mtm = MainThreadMarker::new().unwrap();
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    app.finishLaunching();
    let parent = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm), rect(220.0, 200.0, 420.0, 710.0),
            NSWindowStyleMask::Titled, NSBackingStoreType::Buffered, false)
    };
    unsafe { parent.setReleasedWhenClosed(false); }
    parent.setTitle(&NSString::from_str("펫 자리(가짜) · 머리 위 유리 바 검증"));
    parent.orderFront(None);
    let bar = Bar::from_parent(parent.clone(), mtm).unwrap();
    bar.show();
    bar.sync(110.0);
    println!("ASK_PROBE_GLASS:{}", AnyClass::get(CStr::from_bytes_with_nul(b"NSGlassEffectView\0").unwrap()).is_some());
    println!("ASK_PROBE_WINDOW_ID:{}", bar.panel.windowNumber());

    bar.input.setStringValue(&NSString::from_str("이 창 맥미니로 이사해줘"));
    unsafe { bar.input.performClick(None) };
    let asked = bar.events().into_iter().any(|event| matches!(event, Event::Send(text) if text.contains("이사")));
    println!("ASK_PROBE_SEND_EVENT:{asked}");
    let mut client = crate::ask::Client::default();
    client.ask(service.clone(), "이 창 맥미니로 이사해줘".into(), "%3".into(), serde_json::Value::Null);
    bar.clear_input();

    let started = std::time::Instant::now();
    let mut answered = false;
    while started.elapsed() < std::time::Duration::from_secs(25) {
        let until = NSDate::dateWithTimeIntervalSinceNow(0.05);
        let event = unsafe { app.nextEventMatchingMask_untilDate_inMode_dequeue(NSEventMask::Any, Some(&until), NSDefaultRunLoopMode, true) };
        if let Some(event) = event { app.sendEvent(&event); }
        app.updateWindows();
        bar.sync(110.0);
        if !answered {
            if let Some(result) = client.poll() {
                answered = true;
                match result {
                    Ok(answer) => println!("ASK_PROBE_ANSWER:{}", answer.line().replace('\n', " | ")),
                    Err(()) => println!("ASK_PROBE_ANSWER:실패"),
                }
                println!("ASK_PROBE_BAR_HEIGHT:{}", bar.panel.frame().size.height);
            }
        }
    }
    let head = parent.frame().origin.y + parent.frame().size.height - 110.0;
    println!("ASK_PROBE_ON_HEAD:{}", (bar.panel.frame().origin.y - (head + 4.0)).abs() < 1.0);
    if let Some(path) = std::env::var_os("KASAPET_ASK_SHOT") {
        println!("ASK_PROBE_SHOT:{}", snapshot(&bar, std::path::Path::new(&path)));
    }
    bar.hide();
    parent.orderOut(None);
    server.join().unwrap();
    let _ = std::fs::remove_file(service);
}

/// 바가 그린 것을 파일로 뜬다. 화면 녹화 권한이 없는 자리에서도 글자와 자리를
/// 눈으로 볼 수 있는 유일한 길이다 — 다만 유리가 아래 화면을 빨아들인 결과는 창 관리자가
/// 합성하는 것이라 여기엔 안 담긴다.
#[cfg(debug_assertions)]
fn snapshot(bar: &Bar, path: &std::path::Path) -> bool {
    use objc2_foundation::NSDictionary;
    // 유리 자체가 아니라 그 안에 든 것을 뜬다. 유리는 창 관리자가 합성하는 것이라
    // 이 길로는 빈 판으로만 나오고, 정작 봐야 할 글자가 통째로 빠진다.
    let bounds = bar.body.bounds();
    let Some(rep) = bar.body.bitmapImageRepForCachingDisplayInRect(bounds) else { return false };
    bar.body.cacheDisplayInRect_toBitmapImageRep(bounds, &rep);
    let Some(data) = (unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) }) else { return false };
    std::fs::write(path, data.to_vec()).is_ok()
}

impl Drop for Bar {
    fn drop(&mut self) { self.parent.removeChildWindow(&self.panel); self.panel.orderOut(None); }
}

/// 유리 바탕. macOS 26 의 유리 뷰가 있으면 그것을, 없는 판에서는 흐린 바탕으로 대신한다 —
/// 클래스를 먼저 물어보는 이유는, 없는 클래스에 말을 걸면 그 자리에서 죽기 때문이다.
fn backdrop(mtm: MainThreadMarker, body: &NSView) -> Retained<NSView> {
    let frame = rect(0.0, 0.0, WIDTH, ROW);
    if AnyClass::get(CStr::from_bytes_with_nul(b"NSGlassEffectView\0").unwrap()).is_some() {
        let glass = NSGlassEffectView::initWithFrame(NSGlassEffectView::alloc(mtm), frame);
        glass.setCornerRadius(ROW / 2.0);
        glass.setStyle(NSGlassEffectViewStyle::Regular);
        glass.setContentView(Some(body));
        return Retained::into_super(glass);
    }
    let blur = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), frame);
    blur.setMaterial(NSVisualEffectMaterial::HUDWindow);
    blur.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
    blur.setState(NSVisualEffectState::Active);
    blur.addSubview(body);
    Retained::into_super(blur)
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect { NSRect::new(NSPoint::new(x, y), NSSize::new(w, h)) }

/// 바가 앉을 자리. 펫 창 오른쪽 옆, 머리 높이 — 오른쪽에 자리가 없으면 왼쪽. 머리 위는
/// 말풍선 자리라 비워 둔다. 화면 밖으로 나가면 안쪽으로 끌어당긴다.
fn placement(pet: (f64, f64, f64, f64), bar: (f64, f64), headroom: f64, screen: (f64, f64, f64, f64)) -> (f64, f64) {
    let (px, py, pw, ph) = pet;
    let (bw, bh) = bar;
    let (sx, sy, sw, sh) = screen;
    const GAP: f64 = 8.0;
    let right = px + pw + GAP;
    let x = if right + bw <= sx + sw { right } else { (px - GAP - bw).max(sx) };
    let head = py + ph - headroom;
    let y = (head - bh / 2.0).clamp(sy, (sy + sh - bh).max(sy));
    (x, y)
}

#[cfg(test)]
mod tests {
    use super::placement;

    /// 바는 머리 옆에 앉는다 — 머리 위는 말풍선 자리다. 오른쪽이 막히면 왼쪽으로.
    #[test]
    fn the_bar_sits_beside_the_head_not_above_it() {
        let (x, y) = placement((500.0, 100.0, 420.0, 710.0), (320.0, 46.0), 110.0, (0.0, 30.0, 1440.0, 850.0));
        assert_eq!((x, y), (928.0, 677.0));
        let (x, _) = placement((1100.0, 100.0, 420.0, 710.0), (320.0, 46.0), 110.0, (0.0, 30.0, 1440.0, 850.0));
        assert_eq!(x, 772.0);
    }

    /// 화면 끝에 바짝 붙인 펫이라도 바는 화면 안에 남는다.
    #[test]
    fn screen_edges_pull_the_bar_back_inside() {
        for (px, py) in [(-300.0, 0.0), (1400.0, 760.0), (20.0, -200.0)] {
            let (x, y) = placement((px, py, 420.0, 710.0), (320.0, 84.0), 110.0, (0.0, 30.0, 1440.0, 850.0));
            assert!(x >= 0.0 && x + 320.0 <= 1440.0, "가로: {x}");
            assert!(y >= 30.0 && y + 84.0 <= 880.0, "세로: {y}");
        }
    }
}
