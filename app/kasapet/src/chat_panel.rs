//! The pet's chat inherits AppKit controls so selection, IME and keyboard focus
//! behave like the rest of macOS rather than a second custom text editor.
use std::cell::RefCell;
use std::rc::Rc;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadOnly};
use objc2_app_kit::*;
use objc2_foundation::{MainThreadMarker, NSPoint, NSRange, NSRect, NSSize, NSString};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

pub enum Event { Send(String), Close, Retry, Older }
struct Ivars { input: Retained<NSTextField>, events: Rc<RefCell<Vec<Event>>> }
define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "KasapetChatActions"]
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
        #[unsafe(method(retry:))]
        fn retry(&self, _sender: &AnyObject) { self.ivars().events.borrow_mut().push(Event::Retry); }
        #[unsafe(method(older:))]
        fn older(&self, _sender: &AnyObject) { self.ivars().events.borrow_mut().push(Event::Older); }
    }
);

pub struct Panel {
    panel: Retained<NSPanel>,
    parent: Retained<NSWindow>,
    input: Retained<NSTextField>,
    text: Retained<NSTextView>,
    status: Retained<NSTextField>,
    send: Retained<NSButton>,
    retry: Retained<NSButton>,
    older: Retained<NSButton>,
    _actions: Retained<Actions>,
    events: Rc<RefCell<Vec<Event>>>,
    frame: NSRect,
    transcript: RefCell<String>,
}

impl Panel {
    pub fn new(window: &winit::window::Window) -> Option<Self> {
        let mtm = MainThreadMarker::new()?;
        let RawWindowHandle::AppKit(handle) = window.window_handle().ok()?.as_raw() else { return None };
        let view: &NSView = unsafe { handle.ns_view.cast().as_ref() };
        let parent = view.window()?;
        Self::from_parent(parent, mtm)
    }

    fn from_parent(parent: Retained<NSWindow>, mtm: MainThreadMarker) -> Option<Self> {
        let frame = rect(0.0, 0.0, 360.0, 260.0);
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm), frame,
            NSWindowStyleMask::Titled | NSWindowStyleMask::NonactivatingPanel | NSWindowStyleMask::UtilityWindow,
            NSBackingStoreType::Buffered, false,
        );
        panel.setTitle(&NSString::from_str("나쵸와 대화"));
        unsafe { panel.setReleasedWhenClosed(false); }
        panel.setHidesOnDeactivate(false);
        panel.setBecomesKeyOnlyIfNeeded(true);
        panel.setMovable(false);
        let content = panel.contentView()?;
        let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), rect(12.0, 72.0, 336.0, 176.0));
        scroll.setHasVerticalScroller(true);
        scroll.setAutohidesScrollers(true);
        let text = NSTextView::initWithFrame(NSTextView::alloc(mtm), rect(0.0, 0.0, 316.0, 176.0));
        text.setEditable(false);
        text.setSelectable(true);
        text.setRichText(false);
        text.setFont(Some(&NSFont::systemFontOfSize(13.0)));
        text.setTextColor(Some(&NSColor::labelColor()));
        text.setBackgroundColor(&NSColor::textBackgroundColor());
        text.setVerticallyResizable(true);
        text.setHorizontallyResizable(false);
        // The document grows independently from the fixed-height panel so long
        // server replies remain scrollable rather than ending at a layout cap.
        text.setMaxSize(NSSize::new(316.0, f64::MAX));
        text.setTextContainerInset(NSSize::new(8.0, 8.0));
        if let Some(container) = unsafe { text.textContainer() } {
            container.setWidthTracksTextView(true);
            container.setContainerSize(NSSize::new(316.0, f64::MAX));
        }
        scroll.setDocumentView(Some(&text));
        content.addSubview(&scroll);
        let input = NSTextField::initWithFrame(NSTextField::alloc(mtm), rect(12.0, 12.0, 244.0, 30.0));
        input.setPlaceholderString(Some(&NSString::from_str("나쵸에게 물어보세요")));
        input.setFont(Some(&NSFont::systemFontOfSize(13.0)));
        input.setMaximumNumberOfLines(1);
        content.addSubview(&input);
        let events = Rc::new(RefCell::new(Vec::new()));
        let actions = Actions::alloc(mtm).set_ivars(Ivars { input: input.clone(), events: events.clone() });
        let actions: Retained<Actions> = unsafe { msg_send![super(actions), init] };
        unsafe { input.setTarget(Some(&actions)); input.setAction(Some(sel!(send:))); }
        let send = unsafe { NSButton::buttonWithTitle_target_action(&NSString::from_str("보내기"), Some(&actions), Some(sel!(send:)), mtm) };
        send.setFrame(rect(264.0, 12.0, 84.0, 30.0));
        content.addSubview(&send);
        let close = unsafe { NSButton::buttonWithTitle_target_action(&NSString::from_str("닫기"), Some(&actions), Some(sel!(hide:)), mtm) };
        close.setFrame(rect(290.0, 44.0, 58.0, 24.0));
        close.setKeyEquivalent(&NSString::from_str("\u{1b}"));
        content.addSubview(&close);
        let retry = unsafe { NSButton::buttonWithTitle_target_action(&NSString::from_str("다시 보내기"), Some(&actions), Some(sel!(retry:)), mtm) };
        retry.setFrame(rect(190.0, 44.0, 96.0, 24.0));
        retry.setHidden(true);
        content.addSubview(&retry);
        let older = unsafe { NSButton::buttonWithTitle_target_action(&NSString::from_str("이전 대화"), Some(&actions), Some(sel!(older:)), mtm) };
        older.setFrame(rect(190.0, 44.0, 96.0, 24.0));
        older.setHidden(true);
        content.addSubview(&older);
        let status = NSTextField::labelWithString(&NSString::from_str("질문과 답변이 여기에 남아요."), mtm);
        status.setFrame(rect(12.0, 47.0, 178.0, 18.0));
        status.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        status.setTextColor(Some(&NSColor::secondaryLabelColor()));
        content.addSubview(&status);
        unsafe { parent.addChildWindow_ordered(&panel, NSWindowOrderingMode::Above); }
        panel.orderOut(None);
        Some(Self { panel, parent, input, text, status, send, retry, older, _actions: actions, events, frame, transcript: RefCell::new(String::new()) })
    }

    pub fn show(&self) {
        self.panel.makeKeyAndOrderFront(None);
        self.panel.makeFirstResponder(Some(&*self.input));
    }
    pub fn hide(&self) { self.panel.orderOut(None); }
    pub fn visible(&self) -> bool { self.panel.isVisible() }
    pub fn clear_input(&self) { self.input.setStringValue(&NSString::new()); }
    pub fn events(&self) -> Vec<Event> { self.events.borrow_mut().drain(..).collect() }
    pub fn render(&self, transcript: &str, pending: bool, failed: bool, progress: &str, has_older: bool, showing_older: bool) {
        if *self.transcript.borrow() != transcript {
            *self.transcript.borrow_mut() = transcript.to_string();
            self.text.setString(&NSString::from_str(transcript));
            self.text.sizeToFit();
            self.text.scrollRangeToVisible(NSRange::new(if showing_older { 0 } else { transcript.encode_utf16().count() }, 0));
        }
        self.send.setEnabled(!pending);
        self.retry.setHidden(!failed);
        self.older.setHidden(!has_older || failed);
        self.older.setEnabled(!pending);
        self.status.setStringValue(&NSString::from_str(if pending { progress } else if failed { "연결하지 못했어요." } else { "Enter로 보내기 · Esc로 닫기" }));
    }
    pub fn contains_cursor(&self) -> bool {
        if !self.visible() { return false; }
        let p = NSEvent::mouseLocation();
        let f = self.panel.frame();
        p.x >= f.origin.x && p.x <= f.origin.x + f.size.width && p.y >= f.origin.y && p.y <= f.origin.y + f.size.height
    }
    pub fn max_pet_height(&self) -> f64 {
        self.parent.screen().map(|s| s.visibleFrame().size.height - self.panel.frame().size.height - 12.0).unwrap_or(450.0)
    }
    pub fn sync(&mut self) {
        if !self.visible() { return; }
        let Some(screen) = self.parent.screen() else { return };
        let bounds = screen.visibleFrame();
        let parent = self.parent.frame();
        let panel = self.panel.frame();
        let (px, py, cx, cy) = placement(
            (parent.origin.x, parent.origin.y, parent.size.width, parent.size.height),
            (panel.size.width, panel.size.height),
            (bounds.origin.x, bounds.origin.y, bounds.size.width, bounds.size.height),
        );
        if (parent.origin.x - px).abs() > 0.5 || (parent.origin.y - py).abs() > 0.5 {
            self.parent.setFrameOrigin(NSPoint::new(px, py));
        }
        if (self.frame.origin.x - cx).abs() > 0.5 || (self.frame.origin.y - cy).abs() > 0.5 {
            self.panel.setFrameOrigin(NSPoint::new(cx, cy));
            self.frame = self.panel.frame();
        }
        self.panel.setLevel(self.parent.level());
    }
}

#[cfg(debug_assertions)]
pub fn probe() {
    use objc2_foundation::{NSDate, NSDefaultRunLoopMode};
    let mtm = MainThreadMarker::new().unwrap();
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    app.finishLaunching();
    let parent = unsafe { NSWindow::initWithContentRect_styleMask_backing_defer(NSWindow::alloc(mtm), rect(120.0, 340.0, 280.0, 380.0), NSWindowStyleMask::Titled, NSBackingStoreType::Buffered, false) };
    unsafe { parent.setReleasedWhenClosed(false); }
    parent.setTitle(&NSString::from_str("펫 위치 · 대화창 격리 검증"));
    parent.orderFront(None);
    let mut panel = Panel::from_parent(parent.clone(), mtm).unwrap();
    panel.render("나\n재시작하면 뭐 확인해야 돼?\n\n나쵸\n1. 펫을 우클릭해 ‘나쵸와 대화’를 열어 주세요.\n2. 한글 질문을 입력하고 답변이 오는지 확인해 주세요.\n3. 대화창을 닫았다 열어도 이전 대화가 남는지 봐 주세요.\n\n실제 반영 여부는 아직 확인이 필요해요.\n\n긴 답변도 스크롤해서 읽을 수 있어요.\n아래쪽 확인 항목입니다.\n마지막 항목입니다.", false, false, "", true, false);
    panel.show();
    panel.sync();
    println!("CHAT_PROBE_WINDOW_ID:{}", panel.panel.windowNumber());
    let started = std::time::Instant::now();
    while started.elapsed() < std::time::Duration::from_secs(15) {
        let until = NSDate::dateWithTimeIntervalSinceNow(0.05);
        let event = unsafe { app.nextEventMatchingMask_untilDate_inMode_dequeue(NSEventMask::Any, Some(&until), NSDefaultRunLoopMode, true) };
        if let Some(event) = event { app.sendEvent(&event); }
        app.updateWindows();
        panel.sync();
    }
    panel.input.setStringValue(&NSString::from_str("한글 입력 검증"));
    unsafe { panel.send.performClick(None); }
    let sent = panel.events().into_iter().any(|event| matches!(event, Event::Send(text) if text == "한글 입력 검증"));
    println!("CHAT_PROBE_NATIVE_ACTION:{}", sent);
    parent.setFrameOrigin(NSPoint::new(-80.0, 0.0));
    panel.sync();
    let pf = parent.frame();
    let cf = panel.panel.frame();
    let sf = parent.screen().unwrap().visibleFrame();
    let contained = cf.origin.x >= sf.origin.x && cf.origin.y >= sf.origin.y
        && pf.origin.y + pf.size.height <= sf.origin.y + sf.size.height + 1.0
        && (cf.origin.y + cf.size.height + 6.0 - pf.origin.y).abs() < 1.0;
    println!("CHAT_PROBE_GROUP_CONTAINED:{}", contained);
    panel.hide();
    parent.orderOut(None);
}

impl Drop for Panel {
    fn drop(&mut self) { self.parent.removeChildWindow(&self.panel); self.panel.orderOut(None); }
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect { NSRect::new(NSPoint::new(x, y), NSSize::new(w, h)) }

fn placement(pet: (f64, f64, f64, f64), chat: (f64, f64), screen: (f64, f64, f64, f64)) -> (f64, f64, f64, f64) {
    let (x, y, w, h) = pet;
    let (cw, ch) = chat;
    let (sx, sy, sw, sh) = screen;
    let center = (x + w / 2.0).clamp(sx + w.max(cw) / 2.0, (sx + sw - w.max(cw) / 2.0).max(sx + w.max(cw) / 2.0));
    let py = y.clamp(sy + ch + 6.0, (sy + sh - h).max(sy + ch + 6.0));
    (center - w / 2.0, py, center - cw / 2.0, py - ch - 6.0)
}

#[cfg(test)]
mod tests {
    use super::placement;
    #[test]
    fn bottom_and_side_edges_keep_chat_below_pet() {
        for x in [-300.0, 20.0, 1400.0] {
            let (px, py, cx, cy) = placement((x, 0.0, 300.0, 420.0), (360.0, 282.0), (0.0, 30.0, 1440.0, 850.0));
            assert!(px >= 0.0 && cx >= 0.0 && cx + 360.0 <= 1440.0);
            assert!(cy >= 30.0 && py + 420.0 <= 880.0);
            assert_eq!(cy + 282.0 + 6.0, py);
        }
    }
}
