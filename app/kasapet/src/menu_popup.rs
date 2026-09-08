use super::{Action, Content, Item};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{define_class, msg_send, DefinedClass, MainThreadOnly};
use objc2_app_kit::*;
use objc2_foundation::{MainThreadMarker, NSDictionary, NSPoint, NSRect, NSSize, NSString};
use std::{cell::RefCell, rc::Rc, sync::mpsc, time::Instant};

const WIDTH: f64 = 286.0;
const HEADER: f64 = 46.0;
const ROW: f64 = 36.0;
enum Input {
    Click(NSPoint),
    Key(u16),
    Wheel(f64),
    Palette(serde_json::Value),
}
#[derive(Clone)]
struct Colors {
    surface: [u8; 4],
    hover: [u8; 4],
    active: [u8; 4],
    border: [u8; 4],
    accent: [u8; 4],
    text: [u8; 4],
    dim: [u8; 4],
}
impl Default for Colors {
    fn default() -> Self {
        Self {
            surface: [26, 29, 35, 255],
            hover: [48, 56, 67, 255],
            active: [60, 70, 84, 255],
            border: [80, 92, 110, 110],
            accent: [90, 140, 230, 255],
            text: [236, 238, 243, 255],
            dim: [160, 166, 176, 255],
        }
    }
}
struct State {
    content: Content,
    page: usize,
    selected: Option<usize>,
    scroll: usize,
    visible: usize,
    colors: Colors,
}
struct Ivars {
    state: Rc<RefCell<State>>,
    tx: mpsc::Sender<Input>,
}
define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind=MainThreadOnly]
    #[name="KasapetPopupView"]
    #[ivars=Ivars]
    struct View;
    unsafe impl NSObjectProtocol for View {}
    impl View {
        #[unsafe(method(isFlipped))]
        fn flipped(&self)->bool {true}
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts(&self)->bool {true}
        #[unsafe(method(acceptsFirstMouse:))]
        fn first_mouse(&self,_event:Option<&NSEvent>)->bool {true}
        #[unsafe(method(mouseDown:))]
        fn clicked(&self,event:&NSEvent) { let p=self.convertPoint_fromView(event.locationInWindow(),None); let _=self.ivars().tx.send(Input::Click(p)); }
        #[unsafe(method(keyDown:))]
        fn key(&self,event:&NSEvent) { let _=self.ivars().tx.send(Input::Key(event.keyCode())); }
        #[unsafe(method(scrollWheel:))]
        fn wheel(&self,event:&NSEvent) {let _=self.ivars().tx.send(Input::Wheel(event.scrollingDeltaY()));}
        #[unsafe(method(drawRect:))]
        fn draw(&self,_dirty:NSRect) {
            let s=self.ivars().state.borrow();
            let bounds=self.bounds();
            rounded(bounds,9.0,s.colors.surface,Some(s.colors.border));
            let (title,rows)=&s.content.pages[s.page];
            label(title,if s.page==0 {14.0}else{38.0},14.0,WIDTH-80.0,s.colors.text,true);
            if s.page!=0 { line(&[(24.0,16.0),(18.0,22.0),(24.0,28.0)],s.colors.text); }
            line(&[(WIDTH-25.0,18.0),(WIDTH-17.0,26.0)],s.colors.dim);
            line(&[(WIDTH-17.0,18.0),(WIDTH-25.0,26.0)],s.colors.dim);
            for (index,row) in rows.iter().enumerate().skip(s.scroll).take(s.visible) {
                let y=HEADER+(index-s.scroll)as f64*ROW;
                if matches!(row.item,Item::Heading) {label(&row.label,14.0,y+13.0,WIDTH-28.0,s.colors.dim,false);continue;}
                let selected=s.selected==Some(index)&&row.enabled;
                rounded(rect(7.0,y+2.0,WIDTH-14.0,ROW-4.0),6.0,if row.checked==Some(true) {s.colors.active}else if selected {s.colors.hover}else{s.colors.surface},if selected {Some(s.colors.accent)}else{None});
                label(&row.label,16.0,y+10.0,WIDTH-72.0,if row.enabled{s.colors.text}else{s.colors.dim},false);
                if let Some(on)=row.checked {
                    let x=WIDTH-51.0;
                    rounded(rect(x,y+7.0,36.0,22.0),11.0,if on&&row.enabled{s.colors.accent}else{s.colors.active},None);
                    rounded(rect(x+if on {17.0}else{3.0},y+10.0,16.0,16.0),8.0,if row.enabled{s.colors.text}else{s.colors.dim},None);
                } else if matches!(row.item,Item::Page(_)) { line(&[(WIDTH-24.0,y+13.0),(WIDTH-19.0,y+18.0),(WIDTH-24.0,y+23.0)],s.colors.dim); }
            }
            if rows.len()>s.visible {
                let height=s.visible as f64*ROW;
                let thumb=height*s.visible as f64/rows.len()as f64;
                rounded(rect(WIDTH-5.0,HEADER+height*s.scroll as f64/rows.len()as f64,2.0,thumb),1.0,s.colors.dim,None);
            }
        }
    }
);
define_class!(
    #[unsafe(super(NSPanel))]
    #[thread_kind=MainThreadOnly]
    #[name="KasapetPopupPanel"]
    struct Window;
    unsafe impl NSObjectProtocol for Window {}
    impl Window {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_key(&self)->bool {true}
        #[unsafe(method(canBecomeMainWindow))]
        fn can_main(&self)->bool {false}
    }
);

pub struct Popup {
    panel: Retained<Window>,
    view: Retained<View>,
    state: Rc<RefCell<State>>,
    tx: mpsc::Sender<Input>,
    rx: mpsc::Receiver<Input>,
    buttons: usize,
    mouse: NSPoint,
    palette_pending: bool,
    palette_at: Instant,
}
impl Popup {
    pub fn probe_window_number(&self) -> isize {
        self.panel.windowNumber()
    }
    pub fn probe_click_row(&self, index: usize) {
        let point = NSPoint::new(
            22.0,
            self.panel.frame().size.height - HEADER - index as f64 * ROW - ROW / 2.0,
        );
        if let Some(event)=NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(NSEventType::LeftMouseDown,point,NSEventModifierFlags::empty(),0.0,self.panel.windowNumber(),None,1,1,1.0) {self.panel.sendEvent(&event);}
    }
    pub fn probe_key(&self, code: u16) {
        if let Some(event)=NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(NSEventType::KeyDown,NSPoint::new(0.0,0.0),NSEventModifierFlags::empty(),0.0,self.panel.windowNumber(),None,&NSString::new(),&NSString::new(),false,code){self.panel.sendEvent(&event);}
    }
    pub fn new(content: Content) -> Option<Self> {
        let mtm = MainThreadMarker::new()?;
        let (tx, rx) = mpsc::channel();
        let state = Rc::new(RefCell::new(State {
            content,
            page: 0,
            selected: None,
            scroll: 0,
            visible: 12,
            colors: Colors::default(),
        }));
        let frame = rect(0.0, 0.0, WIDTH, 400.0);
        let panel: Retained<Window> = unsafe {
            msg_send![Window::alloc(mtm),initWithContentRect:frame,styleMask:NSWindowStyleMask::Borderless|NSWindowStyleMask::NonactivatingPanel,backing:NSBackingStoreType::Buffered,defer:false]
        };
        unsafe {
            panel.setReleasedWhenClosed(false);
        }
        panel.setOpaque(false);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        panel.setHasShadow(false);
        panel.setHidesOnDeactivate(false);
        panel.setLevel(NSFloatingWindowLevel);
        let view = View::alloc(mtm).set_ivars(Ivars {
            state: state.clone(),
            tx: tx.clone(),
        });
        let view: Retained<View> = unsafe { msg_send![super(view),initWithFrame:frame] };
        panel.setContentView(Some(&view));
        Some(Self {
            panel,
            view,
            state,
            tx,
            rx,
            buttons: 0,
            mouse: NSPoint::new(-1.0, -1.0),
            palette_pending: false,
            palette_at: Instant::now(),
        })
    }
    pub fn show(&mut self, content: Content) {
        {
            let mut s = self.state.borrow_mut();
            s.content = content;
            s.page = 0;
            s.scroll = 0;
            s.selected = None;
        }
        self.buttons = NSEvent::pressedMouseButtons();
        self.mouse = NSEvent::mouseLocation();
        self.layout(Some(self.mouse));
        self.panel.makeKeyAndOrderFront(None);
        self.panel.makeFirstResponder(Some(&*self.view));
        self.fetch_palette();
    }
    pub fn refresh(&mut self, content: Content) {
        self.state.borrow_mut().content = content;
        self.layout(None);
    }
    pub fn visible(&self) -> bool {
        self.panel.isVisible()
    }
    pub fn hide(&self) {
        self.panel.orderOut(None);
    }
    pub fn contains_cursor(&self) -> bool {
        let p = NSEvent::mouseLocation();
        let f = self.panel.frame();
        self.visible()
            && p.x >= f.origin.x
            && p.x <= f.origin.x + f.size.width
            && p.y >= f.origin.y
            && p.y <= f.origin.y + f.size.height
    }
    fn layout(&mut self, anchor: Option<NSPoint>) {
        let mtm = MainThreadMarker::new().unwrap();
        let p = anchor.unwrap_or_else(|| self.panel.frame().origin);
        let screens = NSScreen::screens(mtm);
        let screen = screens
            .iter()
            .find(|s| {
                let f = s.frame();
                p.x >= f.origin.x
                    && p.x < f.origin.x + f.size.width
                    && p.y >= f.origin.y
                    && p.y < f.origin.y + f.size.height
            })
            .or_else(|| screens.iter().next());
        let Some(screen) = screen else { return };
        let f = screen.visibleFrame();
        let height = {
            let mut s = self.state.borrow_mut();
            s.visible = ((f.size.height.min(540.0) - HEADER - 16.0) / ROW)
                .floor()
                .max(1.0) as usize;
            HEADER + 8.0 + s.content.pages[s.page].1.len().min(s.visible) as f64 * ROW
        };
        let y = if anchor.is_some() {
            p.y - height + 8.0
        } else {
            p.y
        };
        let frame = rect(
            p.x.clamp(
                f.origin.x + 6.0,
                (f.origin.x + f.size.width - WIDTH - 6.0).max(f.origin.x + 6.0),
            ),
            y.clamp(
                f.origin.y + 6.0,
                (f.origin.y + f.size.height - height - 6.0).max(f.origin.y + 6.0),
            ),
            WIDTH,
            height,
        );
        self.panel.setFrame_display(frame, true);
        self.view.setFrame(rect(0.0, 0.0, WIDTH, height));
        self.view.setNeedsDisplay(true);
    }
    fn fetch_palette(&mut self) {
        if self.palette_pending {
            return;
        }
        self.palette_pending = true;
        self.palette_at = Instant::now();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let value =
                crate::journal::get(8765, "/design-tokens").unwrap_or(serde_json::Value::Null);
            let _ = tx.send(Input::Palette(value));
        });
    }
    fn activate(&mut self, index: usize) -> Option<Action> {
        let item = {
            let s = self.state.borrow();
            s.content.pages[s.page]
                .1
                .get(index)
                .filter(|r| r.enabled)
                .map(|r| r.item.clone())
        };
        match item {
            Some(Item::Action(action, keep)) => {
                if !keep {
                    self.hide();
                }
                Some(action)
            }
            Some(Item::Page(page)) => {
                {
                    let mut s = self.state.borrow_mut();
                    s.page = page;
                    s.scroll = 0;
                    s.selected = None;
                }
                self.layout(None);
                None
            }
            _ => None,
        }
    }
    pub fn poll(&mut self) -> Option<Action> {
        while let Ok(input) = self.rx.try_recv() {
            match input {
                Input::Palette(value) => {
                    self.palette_pending = false;
                    let mut s = self.state.borrow_mut();
                    let p = &value["palette"];
                    let Colors {
                        surface,
                        hover,
                        active,
                        border,
                        accent,
                        text,
                        dim,
                    } = &mut s.colors;
                    for (key, target) in [
                        ("surface", surface),
                        ("surface_hover", hover),
                        ("surface_active", active),
                        ("border", border),
                        ("accent", accent),
                        ("text", text),
                        ("text_dim", dim),
                    ] {
                        if let Some(color) = p[key].as_str().and_then(hex) {
                            *target = color;
                        }
                    }
                    drop(s);
                    self.view.setNeedsDisplay(true);
                }
                Input::Click(p) if self.visible() => {
                    if p.y < HEADER {
                        if p.x > WIDTH - 40.0 {
                            self.hide();
                        } else if self.state.borrow().page != 0 {
                            let mut s = self.state.borrow_mut();
                            s.page = 0;
                            s.scroll = 0;
                            s.selected = None;
                            drop(s);
                            self.layout(None);
                        }
                    } else {
                        let i = self.state.borrow().scroll + ((p.y - HEADER) / ROW) as usize;
                        if let Some(a) = self.activate(i) {
                            return Some(a);
                        }
                    }
                }
                Input::Key(key) if self.visible() => {
                    if key == 53 {
                        self.hide();
                    } else if key == 123 {
                        let mut s = self.state.borrow_mut();
                        s.page = 0;
                        s.scroll = 0;
                        s.selected = None;
                        drop(s);
                        self.layout(None);
                    } else if key == 36 || key == 76 || key == 124 || key == 49 {
                        let selected = self.state.borrow().selected;
                        if let Some(i) = selected {
                            if let Some(a) = self.activate(i) {
                                return Some(a);
                            }
                        }
                    } else if key == 125 || key == 126 {
                        let mut s = self.state.borrow_mut();
                        let count = s.content.pages[s.page].1.len();
                        let start = s.selected.unwrap_or(if key == 125 {
                            count.saturating_sub(1)
                        } else {
                            0
                        });
                        for step in 1..=count {
                            let i = if key == 125 {
                                (start + step) % count
                            } else {
                                (start + count - step) % count
                            };
                            if s.content.pages[s.page].1[i].enabled {
                                s.selected = Some(i);
                                if i < s.scroll {
                                    s.scroll = i;
                                }
                                if i >= s.scroll + s.visible {
                                    s.scroll = i + 1 - s.visible;
                                }
                                break;
                            }
                        }
                        drop(s);
                        self.view.setNeedsDisplay(true);
                    }
                }
                Input::Wheel(delta) if self.visible() => {
                    let mut s = self.state.borrow_mut();
                    let max = s.content.pages[s.page].1.len().saturating_sub(s.visible);
                    s.scroll = if delta > 0.0 {
                        s.scroll.saturating_sub(1)
                    } else {
                        (s.scroll + 1).min(max)
                    };
                    drop(s);
                    self.view.setNeedsDisplay(true);
                }
                _ => {}
            }
        }
        if !self.visible() {
            return None;
        }
        let buttons = NSEvent::pressedMouseButtons();
        if outside_press(self.buttons, buttons, self.contains_cursor()) {
            self.hide();
        }
        self.buttons = buttons;
        let mouse = NSEvent::mouseLocation();
        if mouse != self.mouse {
            self.mouse = mouse;
            let f = self.panel.frame();
            let y = f.origin.y + f.size.height - mouse.y;
            let mut s = self.state.borrow_mut();
            let i = s.scroll + ((y - HEADER).max(0.0) / ROW) as usize;
            s.selected = if mouse.x >= f.origin.x && mouse.x < f.origin.x + WIDTH && y >= HEADER {
                s.content.pages[s.page]
                    .1
                    .get(i)
                    .filter(|r| r.enabled)
                    .map(|_| i)
            } else {
                None
            };
            drop(s);
            self.view.setNeedsDisplay(true);
        }
        if self.palette_at.elapsed().as_secs() >= 2 {
            self.fetch_palette();
        }
        None
    }
}
impl Drop for Popup {
    fn drop(&mut self) {
        self.hide();
    }
}
fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}
fn color(c: [u8; 4]) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(
        c[0] as f64 / 255.0,
        c[1] as f64 / 255.0,
        c[2] as f64 / 255.0,
        c[3] as f64 / 255.0,
    )
}
fn rounded(r: NSRect, radius: f64, fill: [u8; 4], stroke: Option<[u8; 4]>) {
    let path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(r, radius, radius);
    color(fill).setFill();
    path.fill();
    if let Some(c) = stroke {
        color(c).setStroke();
        path.setLineWidth(1.0);
        path.stroke();
    }
}
fn line(points: &[(f64, f64)], c: [u8; 4]) {
    let p = NSBezierPath::bezierPath();
    p.moveToPoint(NSPoint::new(points[0].0, points[0].1));
    for (x, y) in &points[1..] {
        p.lineToPoint(NSPoint::new(*x, *y));
    }
    color(c).setStroke();
    p.setLineWidth(1.5);
    p.stroke();
}
fn label(text: &str, x: f64, y: f64, w: f64, c: [u8; 4], bold: bool) {
    let font = if bold {
        NSFont::boldSystemFontOfSize(12.5)
    } else {
        NSFont::systemFontOfSize(12.5)
    };
    let c = color(c);
    let values: [&AnyObject; 2] = [&font, &c];
    let attrs = NSDictionary::from_slices(
        &[unsafe { NSFontAttributeName }, unsafe {
            NSForegroundColorAttributeName
        }],
        &values,
    );
    unsafe {
        NSString::from_str(text).drawInRect_withAttributes(rect(x, y, w, 22.0), Some(&attrs));
    }
}
fn hex(s: &str) -> Option<[u8; 4]> {
    let s = s.strip_prefix('#')?;
    if !s.is_ascii() || (s.len() != 6 && s.len() != 8) {
        return None;
    }
    Some([
        u8::from_str_radix(&s[0..2], 16).ok()?,
        u8::from_str_radix(&s[2..4], 16).ok()?,
        u8::from_str_radix(&s[4..6], 16).ok()?,
        if s.len() == 8 {
            u8::from_str_radix(&s[6..8], 16).ok()?
        } else {
            255
        },
    ])
}

fn outside_press(previous: usize, current: usize, inside: bool) -> bool {
    !inside && current & !previous != 0
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn theme_hex_uses_runtime_alpha_and_rejects_malformed_values() {
        assert_eq!(hex("#505c6e6e"), Some([80, 92, 110, 110]));
        assert_eq!(hex("#5a8ce6"), Some([90, 140, 230, 255]));
        assert_eq!(hex("#한한"), None);
        assert_eq!(hex("#xxxxxx"), None);
    }
    #[test]
    fn opening_right_press_and_release_do_not_dismiss_popup() {
        assert!(!outside_press(2, 2, false));
        assert!(!outside_press(2, 0, false));
        assert!(outside_press(0, 1, false));
        assert!(!outside_press(0, 1, true));
    }
}
