//! 펫 머리 위에 붙는 짧은 유리 바 — 지금 보고 있는 창을 두고 한 줄 묻고 한 줄 받는다.
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
/// 답 한두 줄이 앉는 띠의 높이. 답이 없으면 바는 이만큼 접힌다.
const ANSWER: f64 = 38.0;

pub enum Event { Send(String), Close }

struct Ivars { input: Retained<NSTextField>, events: Rc<RefCell<Vec<Event>>> }
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
    /// 유리(또는 그것이 없는 판에서의 대체 바탕). 바 높이가 바뀌면 같이 늘린다.
    backdrop: Retained<NSView>,
    body: Retained<NSView>,
    input: Retained<NSTextField>,
    answer: Retained<NSTextField>,
    _actions: Retained<Actions>,
    events: Rc<RefCell<Vec<Event>>>,
    /// 답이 붙어 바가 펴져 있는가. 접힌 바는 입력 한 줄이 전부다.
    open: RefCell<bool>,
    origin: RefCell<NSPoint>,
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
        panel.setMovable(false);
        for button in [NSWindowButton::CloseButton, NSWindowButton::MiniaturizeButton, NSWindowButton::ZoomButton] {
            if let Some(button) = panel.standardWindowButton(button) { button.setHidden(true); }
        }

        let body = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, WIDTH, ROW));
        let input = NSTextField::initWithFrame(NSTextField::alloc(mtm), rect(14.0, 11.0, WIDTH - 14.0 - 34.0, 24.0));
        input.setPlaceholderString(Some(&NSString::from_str("이 창에 대해 물어보세요")));
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

        let answer = NSTextField::labelWithString(&NSString::new(), mtm);
        answer.setFrame(rect(14.0, ROW, WIDTH - 28.0, ANSWER - 8.0));
        answer.setFont(Some(&NSFont::systemFontOfSize(12.0)));
        answer.setTextColor(Some(&NSColor::secondaryLabelColor()));
        answer.setMaximumNumberOfLines(2);
        answer.setHidden(true);
        body.addSubview(&answer);

        let backdrop = backdrop(mtm, &body);
        panel.setContentView(Some(&backdrop));
        unsafe { parent.addChildWindow_ordered(&panel, NSWindowOrderingMode::Above); }
        panel.orderOut(None);
        Some(Self {
            panel, parent, backdrop, body, input, answer, _actions: actions, events,
            open: RefCell::new(false), origin: RefCell::new(NSPoint::new(0.0, 0.0)),
            shown: RefCell::new(std::time::Instant::now()),
        })
    }

    pub fn show(&self) {
        *self.shown.borrow_mut() = std::time::Instant::now();
        self.panel.makeKeyAndOrderFront(None);
        self.panel.makeFirstResponder(Some(&*self.input));
    }
    pub fn hide(&self) { self.panel.orderOut(None); }
    pub fn visible(&self) -> bool { self.panel.isVisible() }
    pub fn clear_input(&self) { self.input.setStringValue(&NSString::new()); }
    pub fn events(&self) -> Vec<Event> { self.events.borrow_mut().drain(..).collect() }

    /// 답(또는 기다리는 중이라는 말)을 붙인다. 빈 글을 주면 바가 도로 접힌다.
    pub fn say(&self, text: &str) {
        let open = !text.trim().is_empty();
        self.answer.setStringValue(&NSString::from_str(text));
        self.answer.setHidden(!open);
        if *self.open.borrow() == open { return; }
        *self.open.borrow_mut() = open;
        let height = if open { ROW + ANSWER } else { ROW };
        let frame = self.panel.frame();
        // 아래 모서리가 머리에 붙어 있으니 자라는 쪽은 위다. 위를 고정하면 답이 붙을
        // 때마다 바가 캐릭터를 파고든다.
        self.panel.setFrame_display(rect(frame.origin.x, frame.origin.y, WIDTH, height), true);
        self.backdrop.setFrame(rect(0.0, 0.0, WIDTH, height));
        self.body.setFrame(rect(0.0, 0.0, WIDTH, height));
    }

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
    /// 그만큼 내려와야 바가 머리 「바로」 위에 앉는다.
    pub fn sync(&self, headroom: f64) {
        if !self.visible() { return; }
        let Some(screen) = self.parent.screen() else { return };
        let bounds = screen.visibleFrame();
        let parent = self.parent.frame();
        let bar = self.panel.frame();
        let (x, y) = placement(
            (parent.origin.x, parent.origin.y, parent.size.width, parent.size.height),
            (bar.size.width, bar.size.height), headroom,
            (bounds.origin.x, bounds.origin.y, bounds.size.width, bounds.size.height),
        );
        let previous = *self.origin.borrow();
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
    client.ask(service.clone(), "이 창 맥미니로 이사해줘".into(), "%3".into());
    bar.clear_input();
    bar.say("나쵸가 보는 중…");

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
                    Ok(answer) => { println!("ASK_PROBE_ANSWER:{}", answer.line().replace('\n', " | ")); bar.say(&answer.line()); }
                    Err(()) => { println!("ASK_PROBE_ANSWER:실패"); bar.say("나쵸에게 닿지 못했어요."); }
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

/// 바가 앉을 자리. 가로는 펫의 한가운데, 세로는 머리 바로 위 — 화면 밖으로 나가면
/// 안쪽으로 끌어당긴다. 펫을 화면 끝에 바짝 붙여 두는 사람이 바를 못 쓰면 안 된다.
fn placement(pet: (f64, f64, f64, f64), bar: (f64, f64), headroom: f64, screen: (f64, f64, f64, f64)) -> (f64, f64) {
    let (px, py, pw, ph) = pet;
    let (bw, bh) = bar;
    let (sx, sy, sw, sh) = screen;
    let x = (px + pw / 2.0 - bw / 2.0).clamp(sx, (sx + sw - bw).max(sx));
    let head = py + ph - headroom;
    let y = (head + 4.0).clamp(sy, (sy + sh - bh).max(sy));
    (x, y)
}

#[cfg(test)]
mod tests {
    use super::placement;

    /// 바는 머리 바로 위에 앉는다 — 창 꼭대기가 아니라, 말풍선 자리만큼 내려온 곳이다.
    #[test]
    fn the_bar_sits_on_the_head_not_on_the_window_top() {
        let (x, y) = placement((500.0, 100.0, 420.0, 710.0), (320.0, 46.0), 110.0, (0.0, 30.0, 1440.0, 850.0));
        assert_eq!(x, 550.0);
        assert_eq!(y, 704.0);
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
