//! 떠 있는 현황판 — 「사람 차례·작업·끝」 세 수와 선생님을 기다리는 학생들, 맨 아래 나쵸 한 줄.
//!
//! 나쵸가 문장으로 지어 말풍선에 띄우던 요약을 갈음한다(2026-09-28 지시). 수와 목록은 카사텀이
//! 보드와 같은 판정으로 `overlay.json` 에 적어 두는 것을 그대로 읽는다 — 모델을 안 거치니 즉시·공짜고,
//! 카사텀이 앞에 없어도 펫 곁에 떠 있다. 줄을 누르면 그 학생 창으로 간다.

/// 한 번에 보이는 줄 수. 넘치면 「외 N명」.
pub const MAX_ROWS: usize = 5;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Row {
    pub name: String,
    pub line: String,
    /// 다른 기기 학생이면 그 기기 이름.
    pub machine: String,
    /// 누르면 갈 이 기기의 pane. 비었으면 카사텀을 앞으로만.
    pub pane: String,
    pub face: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Data {
    pub counts: (u64, u64, u64),
    pub waiting: Vec<Row>,
}

impl Data {
    /// 세 수 한 줄 — 카사텀 사이드바 현황 줄과 같은 말.
    pub fn counts_line(&self) -> String {
        let (yours, working, done) = self.counts;
        format!("사람 차례 {yours} · 작업 {working} · 끝 {done}")
    }
}

/// 카사텀은 20초마다 판을 다시 적는다. 이만큼 멈췄으면 앱이 꺼진 것이라 옛 숫자를 내린다.
pub const STALE: std::time::Duration = std::time::Duration::from_secs(60);

/// 그 파일이 아직 살아 있는 판인가.
pub fn fresh(modified: Option<std::time::SystemTime>, now: std::time::SystemTime) -> bool {
    modified.is_some_and(|at| now.duration_since(at).map_or(true, |age| age < STALE))
}

/// 파일 한 장을 읽는다. 없거나 깨졌으면 `None` — 판을 안 띄운다(카사텀이 아직 안 적었다).
pub fn read(path: &std::path::Path) -> Option<Data> {
    let value: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let n = |key: &str| value["counts"][key].as_u64().unwrap_or(0);
    let text = |row: &serde_json::Value, key: &str| row[key].as_str().unwrap_or("").to_string();
    let waiting = value["waiting"].as_array()?.iter().map(|row| Row {
        name: text(row, "name"),
        line: text(row, "line"),
        machine: text(row, "machine"),
        pane: text(row, "pane"),
        face: text(row, "face"),
    }).collect();
    Some(Data { counts: (n("yours"), n("working"), n("done")), waiting })
}

/// 판을 펫 옆에 둔다 — 왼쪽에 자리가 있으면 왼쪽, 없으면 오른쪽. 윗변은 펫 윗변에 맞추되 화면 안에 가둔다.
/// 모든 값은 AppKit 좌표(아래가 0)다. 반환은 판의 왼쪽 아래 모서리.
pub fn placement(pet: (f64, f64, f64, f64), board: (f64, f64), screen: (f64, f64, f64, f64)) -> (f64, f64) {
    const GAP: f64 = 8.0;
    let (px, py, pw, ph) = pet;
    let (bw, bh) = board;
    let (sx, sy, sw, sh) = screen;
    let x = if px - GAP - bw >= sx { px - GAP - bw } else { (px + pw + GAP).min(sx + sw - bw).max(sx) };
    let y = (py + ph - bh).clamp(sy, (sy + sh - bh).max(sy));
    (x, y)
}

#[cfg(target_os = "macos")]
pub use panel::{Event, Panel};

#[cfg(target_os = "macos")]
mod panel {
    use super::{Data, MAX_ROWS};
    use objc2::rc::Retained;
    use objc2::runtime::{NSObject, NSObjectProtocol};
    use objc2::{define_class, msg_send, sel, AnyThread, DefinedClass, MainThreadOnly};
    use objc2_app_kit::*;
    use objc2_foundation::{MainThreadMarker, NSMutableAttributedString, NSPoint, NSRect, NSSize, NSString};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use std::cell::RefCell;
    use std::rc::Rc;

    pub enum Event {
        /// 몇 번째 줄을 눌렀나.
        Open(usize),
    }

    const W: f64 = 300.0;
    const PAD: f64 = 12.0;
    const HEAD_H: f64 = 22.0;
    const ROW_H: f64 = 44.0;
    const FACE: f64 = 30.0;
    const FOOT_H: f64 = 34.0;

    struct Ivars {
        events: Rc<RefCell<Vec<Event>>>,
    }
    define_class!(
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "KasapetOverlayActions"]
        #[ivars = Ivars]
        struct Actions;
        unsafe impl NSObjectProtocol for Actions {}
        impl Actions {
            #[unsafe(method(open:))]
            fn open(&self, sender: &NSButton) {
                self.ivars().events.borrow_mut().push(Event::Open(sender.tag() as usize));
            }
        }
    );

    pub struct Panel {
        panel: Retained<NSPanel>,
        parent: Retained<NSWindow>,
        actions: Retained<Actions>,
        events: Rc<RefCell<Vec<Event>>>,
        shown: RefCell<Option<(Data, String)>>,
    }

    impl Panel {
        pub fn new(window: &winit::window::Window) -> Option<Self> {
            let mtm = MainThreadMarker::new()?;
            let RawWindowHandle::AppKit(handle) = window.window_handle().ok()?.as_raw() else { return None };
            let view: &NSView = unsafe { handle.ns_view.cast().as_ref() };
            Self::from_parent(view.window()?, mtm)
        }

        fn from_parent(parent: Retained<NSWindow>, mtm: MainThreadMarker) -> Option<Self> {
            let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
                NSPanel::alloc(mtm), rect(0.0, 0.0, W, 120.0),
                NSWindowStyleMask::Titled | NSWindowStyleMask::UtilityWindow | NSWindowStyleMask::HUDWindow
                    | NSWindowStyleMask::NonactivatingPanel,
                NSBackingStoreType::Buffered, false,
            );
            panel.setTitle(&NSString::from_str("학생 현황"));
            unsafe { panel.setReleasedWhenClosed(false); }
            panel.setHidesOnDeactivate(false);
            panel.setBecomesKeyOnlyIfNeeded(true);
            panel.setMovable(false);
            let events = Rc::new(RefCell::new(Vec::new()));
            let actions = Actions::alloc(mtm).set_ivars(Ivars { events: events.clone() });
            let actions: Retained<Actions> = unsafe { msg_send![super(actions), init] };
            unsafe { parent.addChildWindow_ordered(&panel, NSWindowOrderingMode::Above); }
            panel.orderOut(None);
            Some(Self { panel, parent, actions, events, shown: RefCell::new(None) })
        }

        pub fn events(&self) -> Vec<Event> { self.events.borrow_mut().drain(..).collect() }
        pub fn visible(&self) -> bool { self.panel.isVisible() }
        pub fn hide(&self) { self.panel.orderOut(None); }

        /// 자료가 바뀌었을 때만 내용물을 다시 세운다 — 매 프레임 뷰를 갈면 누르는 사이에 단추가 사라진다.
        pub fn render(&self, data: &Data, nacho: &str) {
            let key = (data.clone(), nacho.to_string());
            if self.shown.borrow().as_ref() == Some(&key) {
                if !self.visible() { self.panel.orderFront(None); }
                return;
            }
            let Some(mtm) = MainThreadMarker::new() else { return };
            let rows = data.waiting.len().min(MAX_ROWS);
            let more = data.waiting.len().saturating_sub(MAX_ROWS);
            let empty_h = if rows == 0 { 26.0 } else { 0.0 };
            let more_h = if more > 0 { 20.0 } else { 0.0 };
            let foot_h = if nacho.trim().is_empty() { 0.0 } else { FOOT_H };
            let h = PAD + HEAD_H + 6.0 + rows as f64 * ROW_H + empty_h + more_h + foot_h + PAD;
            let content = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, W, h));
            // AppKit 은 아래가 0 이라 위에서부터 쌓는 자리를 거꾸로 센다.
            let mut top = h - PAD;
            let head = label(mtm, "", 13.0, true, NSColor::labelColor());
            head.setAttributedStringValue(&counts_text(data));
            head.setFrame(rect(PAD, top - HEAD_H, W - PAD * 2.0, HEAD_H));
            content.addSubview(&head);
            top -= HEAD_H + 6.0;
            for (i, row) in data.waiting.iter().take(MAX_ROWS).enumerate() {
                let y = top - ROW_H;
                let slot = rect(PAD, y + (ROW_H - FACE) / 2.0, FACE, FACE);
                match face(&row.face) {
                    Some(image) => {
                        let view = NSImageView::imageViewWithImage(&image, mtm);
                        view.setFrame(slot);
                        content.addSubview(&view);
                    }
                    // 그림이 없는 학생(원화 없는 캐릭터·셸)은 이름 첫 글자로 자리를 지킨다 — 비워 두면
                    // 줄마다 얼굴 열이 들쭉날쭉해 보인다.
                    None => {
                        let initial: String = row.name.chars().take(1).collect();
                        let mark = label(mtm, &initial, 15.0, true, NSColor::secondaryLabelColor());
                        mark.setAlignment(NSTextAlignment::Center);
                        mark.setFrame(rect(slot.origin.x, slot.origin.y + 5.0, FACE, 20.0));
                        content.addSubview(&mark);
                    }
                }
                let tx = PAD + FACE + 10.0;
                let name = if row.machine.is_empty() { row.name.clone() } else { format!("{} · {}", row.name, row.machine) };
                let who = label(mtm, &name, 12.5, true, NSColor::labelColor());
                who.setFrame(rect(tx, y + ROW_H / 2.0, W - tx - PAD, 18.0));
                content.addSubview(&who);
                let what = label(mtm, &row.line, 11.0, false, NSColor::secondaryLabelColor());
                what.setFrame(rect(tx, y + 4.0, W - tx - PAD, 17.0));
                content.addSubview(&what);
                // 줄 전체가 단추다 — 얼굴·이름·글자 어디를 눌러도 그 창으로 간다.
                let hit = unsafe { NSButton::buttonWithTitle_target_action(&NSString::new(), Some(&self.actions), Some(sel!(open:)), mtm) };
                hit.setBordered(false);
                hit.setTransparent(true);
                hit.setTag(i as isize);
                hit.setFrame(rect(PAD - 4.0, y, W - PAD * 2.0 + 8.0, ROW_H));
                hit.setToolTip(Some(&NSString::from_str(if row.pane.is_empty() { "카사텀 열기" } else { "그 창으로 가기" })));
                content.addSubview(&hit);
                top = y;
            }
            if rows == 0 {
                let none = label(mtm, "선생님을 기다리는 학생이 없어요", 11.5, false, NSColor::secondaryLabelColor());
                none.setFrame(rect(PAD, top - 22.0, W - PAD * 2.0, 18.0));
                content.addSubview(&none);
                top -= empty_h;
            }
            if more > 0 {
                let rest = label(mtm, &format!("외 {more}명 — 카사텀 보드에서"), 11.0, false, NSColor::tertiaryLabelColor());
                rest.setFrame(rect(PAD + FACE + 10.0, top - 18.0, W - PAD * 2.0, 16.0));
                content.addSubview(&rest);
                top -= more_h;
            }
            if foot_h > 0.0 {
                let rule = NSBox::initWithFrame(NSBox::alloc(mtm), rect(PAD, top - 3.0, W - PAD * 2.0, 1.0));
                rule.setBoxType(NSBoxType::Separator);
                content.addSubview(&rule);
                let line = label(mtm, nacho.trim(), 11.0, false, NSColor::secondaryLabelColor());
                // 두 줄까지 접어 넣는다 — 한 줄로 자르면 나쵸 말의 뒤쪽(대개 「무엇을 하면 되는지」)이 잘린다.
                line.setLineBreakMode(NSLineBreakMode::ByWordWrapping);
                line.setMaximumNumberOfLines(2);
                if let Some(cell) = line.cell() { cell.setTruncatesLastVisibleLine(true); }
                line.setFrame(rect(PAD, top - foot_h - 2.0, W - PAD * 2.0, foot_h - 4.0));
                content.addSubview(&line);
            }
            let origin = self.panel.frame().origin;
            let frame = self.panel.frameRectForContentRect(rect(0.0, 0.0, W, h));
            self.panel.setContentView(Some(&content));
            self.panel.setFrame_display(NSRect::new(origin, frame.size), true);
            *self.shown.borrow_mut() = Some(key);
            self.sync();
            self.panel.orderFront(None);
        }

        /// 펫이 움직이면 따라간다.
        pub fn sync(&self) {
            if !self.visible() && self.shown.borrow().is_none() { return; }
            let Some(screen) = self.parent.screen() else { return };
            let bounds = screen.visibleFrame();
            let pet = self.parent.frame();
            let size = self.panel.frame().size;
            let (x, y) = super::placement(
                (pet.origin.x, pet.origin.y, pet.size.width, pet.size.height),
                (size.width, size.height),
                (bounds.origin.x, bounds.origin.y, bounds.size.width, bounds.size.height),
            );
            let now = self.panel.frame().origin;
            if (now.x - x).abs() > 0.5 || (now.y - y).abs() > 0.5 {
                self.panel.setFrameOrigin(NSPoint::new(x, y));
            }
            self.panel.setLevel(self.parent.level());
        }

    }

    impl Drop for Panel {
        fn drop(&mut self) { self.parent.removeChildWindow(&self.panel); self.panel.orderOut(None); }
    }

    fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect { NSRect::new(NSPoint::new(x, y), NSSize::new(w, h)) }

    fn label(mtm: MainThreadMarker, text: &str, size: f64, bold: bool, color: Retained<NSColor>) -> Retained<NSTextField> {
        let field = NSTextField::labelWithString(&NSString::from_str(text), mtm);
        let font = if bold { NSFont::boldSystemFontOfSize(size) } else { NSFont::systemFontOfSize(size) };
        field.setFont(Some(&font));
        field.setTextColor(Some(&color));
        field.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
        field
    }

    /// 「사람 차례 N · 작업 N · 끝 N」 — 카사텀 사이드바 현황 줄과 같은 색 말: 0 은 흐리게, 살아 있는
    /// 수는 사람 차례 주황·작업 파랑·끝 초록.
    fn counts_text(data: &Data) -> Retained<NSMutableAttributedString> {
        let (yours, working, done) = data.counts;
        let text = NSMutableAttributedString::initWithString(NSMutableAttributedString::alloc(), &NSString::from_str(&data.counts_line()));
        let line = data.counts_line();
        let mut from = 0usize;
        for (n, color) in [(yours, NSColor::systemOrangeColor()), (working, NSColor::systemBlueColor()), (done, NSColor::systemGreenColor())] {
            let digits = n.to_string();
            let Some(at) = line[from..].find(&digits).map(|i| i + from) else { continue };
            let start = line[..at].encode_utf16().count();
            let range = objc2_foundation::NSRange::new(start, digits.encode_utf16().count());
            let color = if n == 0 { NSColor::tertiaryLabelColor() } else { color };
            unsafe { text.addAttribute_value_range(NSForegroundColorAttributeName, &color, range); }
            from = at + digits.len();
        }
        text
    }

    fn face(path: &str) -> Option<Retained<NSImage>> {
        if path.is_empty() { return None; }
        NSImage::initWithContentsOfFile(NSImage::alloc(), &NSString::from_str(path))
    }

    /// 격리 검증 — 펫 대신 빈 창 하나를 부모로 세우고 가짜 자료로 판을 띄운 뒤, 둘째 줄을 진짜로 눌러 본다.
    #[cfg(debug_assertions)]
    pub fn probe(data_path: Option<&str>) {
        use objc2_foundation::{NSDate, NSDefaultRunLoopMode};
        let mtm = MainThreadMarker::new().unwrap();
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        app.finishLaunching();
        let parent = unsafe { NSWindow::initWithContentRect_styleMask_backing_defer(NSWindow::alloc(mtm), rect(900.0, 300.0, 260.0, 380.0), NSWindowStyleMask::Titled, NSBackingStoreType::Buffered, false) };
        unsafe { parent.setReleasedWhenClosed(false); }
        parent.setTitle(&NSString::from_str("펫 자리 · 현황판 격리 검증"));
        parent.orderFront(None);
        let panel = Panel::from_parent(parent.clone(), mtm).unwrap();
        let data = data_path.and_then(|p| super::read(std::path::Path::new(p))).unwrap_or_default();
        panel.render(&data, "미도리는 승인 하나만 누르면 이어서 끝낼 수 있어요. 아리스는 질문에 답이 필요해요.");
        println!("OVERLAY_PROBE_WINDOW_ID:{}", panel.panel.windowNumber());
        // 화면 기록 권한 없이 판을 떠 보는 길 — 창 틀(제목 줄 포함)을 그 자리에서 비트맵으로 굽는다.
        if let Ok(path) = std::env::var("KASAPET_OVERLAY_SHOT") {
            let frame = panel.panel.contentView().and_then(|view| unsafe { view.superview() });
            let png = frame.and_then(|view| {
                let bounds = view.bounds();
                let rep = view.bitmapImageRepForCachingDisplayInRect(bounds)?;
                view.cacheDisplayInRect_toBitmapImageRep(bounds, &rep);
                unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &objc2_foundation::NSDictionary::new()) }
            });
            let wrote = png.is_some_and(|data| std::fs::write(&path, data.to_vec()).is_ok());
            println!("OVERLAY_PROBE_SHOT:{wrote}");
        }
        let started = std::time::Instant::now();
        while started.elapsed() < std::time::Duration::from_secs(8) {
            let until = NSDate::dateWithTimeIntervalSinceNow(0.05);
            let event = unsafe { app.nextEventMatchingMask_untilDate_inMode_dequeue(NSEventMask::Any, Some(&until), NSDefaultRunLoopMode, true) };
            if let Some(event) = event { app.sendEvent(&event); }
            app.updateWindows();
            panel.sync();
        }
        let second = panel.panel.contentView().and_then(|view| {
            view.subviews().iter().filter_map(|sub| sub.downcast::<NSButton>().ok()).find(|button| button.tag() == 1)
        });
        if let Some(button) = second { unsafe { button.performClick(None); } }
        let opened = panel.events().into_iter().any(|event| matches!(event, Event::Open(1)));
        println!("OVERLAY_PROBE_ROW_CLICK:{opened}");
        let pf = parent.frame();
        let of = panel.panel.frame();
        println!("OVERLAY_PROBE_BESIDE_PET:{}", of.origin.x + of.size.width <= pf.origin.x || of.origin.x >= pf.origin.x + pf.size.width);
        panel.hide();
        parent.orderOut(None);
    }
}

#[cfg(all(target_os = "macos", debug_assertions))]
pub use panel::probe;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_kasaterm_writes_and_stays_silent_without_it() {
        let dir = std::env::temp_dir().join(format!("kasapet-overlay-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("overlay.json");
        std::fs::write(&f, r#"{"counts":{"yours":2,"working":3,"done":1},"waiting":[{"name":"미도리","line":"승인 기다림 · 정리","machine":"","pane":"%3","face":""},{"name":"아리스","line":"답 기다림","machine":"나쵸네코","pane":"","face":""}]}"#).unwrap();
        let data = read(&f).expect("읽힌다");
        assert_eq!(data.counts, (2, 3, 1));
        assert_eq!(data.counts_line(), "사람 차례 2 · 작업 3 · 끝 1");
        assert_eq!(data.waiting[0].pane, "%3");
        assert_eq!(data.waiting[1].machine, "나쵸네코");
        assert!(read(&dir.join("없다.json")).is_none(), "카사텀이 안 적었으면 판을 안 띄운다");
        std::fs::write(&f, "{깨짐").unwrap();
        assert!(read(&f).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_board_kasaterm_stopped_writing_goes_away() {
        let now = std::time::SystemTime::now();
        assert!(fresh(Some(now - std::time::Duration::from_secs(25)), now));
        assert!(!fresh(Some(now - STALE), now), "앱이 꺼진 뒤의 옛 숫자는 내린다");
        assert!(!fresh(None, now));
        assert!(fresh(Some(now + std::time::Duration::from_secs(2)), now), "시계가 조금 앞선 파일도 산 것으로");
    }

    #[test]
    fn board_sits_left_of_the_pet_unless_the_screen_edge_is_there() {
        let screen = (0.0, 30.0, 1440.0, 850.0);
        let (x, y) = placement((900.0, 300.0, 260.0, 380.0), (300.0, 200.0), screen);
        assert_eq!(x, 900.0 - 8.0 - 300.0, "왼쪽에 자리가 있으면 왼쪽");
        assert_eq!(y + 200.0, 680.0, "윗변을 펫 윗변에 맞춘다");
        let (x, _) = placement((100.0, 300.0, 260.0, 380.0), (300.0, 200.0), screen);
        assert_eq!(x, 100.0 + 260.0 + 8.0, "왼쪽이 모자라면 오른쪽");
        let (_, y) = placement((900.0, 700.0, 260.0, 380.0), (300.0, 200.0), screen);
        assert!(y + 200.0 <= 880.0, "화면 위로 안 넘친다");
    }
}
