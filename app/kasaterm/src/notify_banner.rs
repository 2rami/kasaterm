//! 완료 알림 배너 — **우리가 직접 그리는** 알림 창.
//!
//! macOS 알림 센터를 못 쓴다. 자체 서명 번들은 `UNUserNotificationCenter` 등록이
//! `Notifications are not allowed for this application` 으로 거절되고, 그래서
//! `osascript` 로 떨어지면 배너에 **스크립트 편집기 아이콘**이 붙는다. 원인이 우리
//! 코드가 아니라는 것까지 배제 실측으로 확인했다(chrome.rs `notify_native` 주석).
//! 거노 2026-08-21 선택: 「앱이 직접 배너를 그린다」.
//!
//! ⚠️ **이 창은 절대 키 포커스를 뺏으면 안 된다.** 배너가 뜨는 순간은 정의상 사용자가
//! **다른 앱을 보고 있을 때**다 — 타이핑 중에 키 포커스를 가져가면 그 타이핑이
//! 끊긴다. 이 레포엔 그 사고 이력이 있다(헤드리스 캡처가 창을 튀어나오게 해 쓰던
//! 앱이 뒤로 밀렸다). `with_active(false)` 로 뜨되 마우스는 받는다 — 카드는 해당
//! pane으로 가고, X는 배너만 닫고, 드래그는 알림 스택 위치를 바꾼다.
//!
//! ## 이 방식이 잃는 것과, 그걸 이미 메우고 있는 것
//!
//! 자체 배너는 **알림 센터에 안 쌓인다**. 자리를 비웠다 오면 놓친 배너가 없다.
//! 그런데 이 앱은 그 자리를 이미 다른 언어로 말하고 있다 — `unread_panes`(못 본
//! 완료), Dock 배지, 사이드바 줄의 숨쉬기. 배너는 **「지금 알린다」만** 맡고
//! 「놓친 것」은 그쪽이 든다. 그래서 배너를 띄우는 판정을 새로 만들지 않고
//! `handle_notify` 안, 그 셋과 **같은 자리**에서 띄운다 — 판정이 두 벌이면 배너는
//! 떴는데 배지는 안 서는 식으로 갈린다.

use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::monitor::MonitorHandle;
use winit::window::{CursorIcon, Window, WindowAttributes, WindowLevel};

use super::*;

/// 배너 한 장의 논리 크기. macOS 기본 배너와 비슷한 비례 — 낯선 물건을 새 규격으로
/// 내놓을 이유가 없다.
const W: f32 = 360.0;
const H: f32 = 78.0;
/// 쌓일 때의 세로 간격.
const GAP: f32 = 10.0;
/// 화면 오른쪽 여백.
const MARGIN_R: f32 = 14.0;
/// 화면 위 여백. 메뉴바(및 노치)를 피해야 한다 — winit 은 「가려지지 않는 영역」을
/// 안 알려주므로 넉넉히 잡는다. macOS 알림도 이쯤에 뜬다.
const MARGIN_T: f32 = 44.0;
/// 한 장이 떠 있는 시간. 짧게 흘려보내면 제목과 본문을 읽기도 전에 사라진다.
const LIFE: Duration = Duration::from_secs(8);
/// hover가 끝난 뒤 보장하는 최소 읽기 시간.
const LEAVE_GRACE: Duration = Duration::from_millis(1800);
/// 카드 click과 drag를 가르는 논리 픽셀 거리.
const DRAG_THRESHOLD: f64 = 4.0;
const MARGIN_B: f32 = 14.0;
const CLOSE_BOX: f32 = 36.0;
const CLOSE_ICON: f32 = 14.0;
/// 동시에 쌓이는 최대 장수. 넘으면 **가장 오래된 것부터** 걷는다 — 무한히 쌓으면
/// 학생이 여럿 끝나는 순간 화면이 배너로 덮이고, 그건 알림이 아니라 방해다.
const MAX: usize = 3;

/// 아직 창을 못 만든 배너 한 장 — `(제목, 본문, 학생, 갈 자리)`.
pub(crate) type BannerReq = (
    String,
    String,
    Option<String>,
    Option<(String, Option<String>)>,
);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Point {
    x: i32,
    y: i32,
}

impl Point {
    fn offset(self, delta: Point) -> Self {
        Self {
            x: self.x.saturating_add(delta.x),
            y: self.y.saturating_add(delta.y),
        }
    }
}

impl From<PhysicalPosition<i32>> for Point {
    fn from(value: PhysicalPosition<i32>) -> Self {
        Self {
            x: value.x,
            y: value.y,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct MonitorBounds {
    origin: Point,
    width: i32,
    height: i32,
    scale: f64,
}

impl MonitorBounds {
    fn from_handle(monitor: &MonitorHandle) -> Self {
        let position = monitor.position();
        let size = monitor.size();
        Self {
            origin: position.into(),
            width: size.width.min(i32::MAX as u32) as i32,
            height: size.height.min(i32::MAX as u32) as i32,
            scale: monitor.scale_factor(),
        }
    }

    fn contains(self, point: Point) -> bool {
        point.x >= self.origin.x
            && point.y >= self.origin.y
            && point.x < self.origin.x.saturating_add(self.width)
            && point.y < self.origin.y.saturating_add(self.height)
    }
}

#[derive(Clone, Copy, Debug)]
struct Lifetime {
    until: Instant,
    paused_left: Option<Duration>,
}

impl Lifetime {
    fn new(now: Instant) -> Self {
        Self {
            until: now + LIFE,
            paused_left: None,
        }
    }

    fn pause(&mut self, now: Instant) {
        if self.paused_left.is_none() {
            self.paused_left = Some(self.until.saturating_duration_since(now));
        }
    }

    fn resume(&mut self, now: Instant) {
        if let Some(left) = self.paused_left.take() {
            self.until = now + left.max(LEAVE_GRACE);
        }
    }

    fn alive(self, now: Instant, held: bool) -> bool {
        self.paused_left.is_some() || held || self.until > now
    }

    fn deadline(self) -> Option<Instant> {
        self.paused_left.is_none().then_some(self.until)
    }
}

#[derive(Clone, Copy, Debug)]
struct DragGesture {
    cursor_at_press: Option<(f64, f64)>,
    origin: Point,
    started: bool,
}

impl DragGesture {
    fn new(cursor_at_press: Option<(f64, f64)>, origin: Point) -> Self {
        Self {
            cursor_at_press,
            origin,
            started: false,
        }
    }

    fn crossed_threshold(self, cursor: (f64, f64)) -> bool {
        let Some(press) = self.cursor_at_press else {
            return false;
        };
        let dx = cursor.0 - press.0;
        let dy = cursor.1 - press.1;
        dx * dx + dy * dy >= DRAG_THRESHOLD * DRAG_THRESHOLD
    }
}

#[derive(Clone, Copy, Debug)]
enum Press {
    Close,
    Card(DragGesture),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReleaseAction {
    None,
    Dismiss,
    Route,
    FinishDrag,
}

fn close_hit(cursor: Option<(f64, f64)>) -> bool {
    cursor.is_some_and(|(x, y)| {
        x >= (W - CLOSE_BOX) as f64 && x <= W as f64 && y >= 0.0 && y <= CLOSE_BOX as f64
    })
}

fn release_action(press: Press, cursor: Option<(f64, f64)>) -> ReleaseAction {
    match press {
        Press::Close if close_hit(cursor) => ReleaseAction::Dismiss,
        Press::Close => ReleaseAction::None,
        Press::Card(drag) if drag.started => ReleaseAction::FinishDrag,
        Press::Card(_) => ReleaseAction::Route,
    }
}

fn stack_is_hovered(states: impl IntoIterator<Item = bool>) -> bool {
    states.into_iter().any(|hovered| hovered)
}

fn px(value: f32, scale: f64) -> i32 {
    (value as f64 * scale).round().clamp(0.0, i32::MAX as f64) as i32
}

fn stack_step(scale: f64) -> i32 {
    px(H + GAP, scale)
}

fn stack_positions(base: Point, count: usize, scale: f64) -> Vec<Point> {
    let step = stack_step(scale);
    (0..count)
        .map(|i| Point {
            x: base.x,
            y: base.y.saturating_add(step.saturating_mul(i as i32)),
        })
        .collect()
}

fn clamp_stack_origin(requested: Point, bounds: MonitorBounds, count: usize) -> Point {
    let left = bounds.origin.x.saturating_add(px(MARGIN_R, bounds.scale));
    let top = bounds.origin.y.saturating_add(px(MARGIN_T, bounds.scale));
    let width = px(W, bounds.scale);
    let stack_height = px(H, bounds.scale)
        .saturating_mul(count.max(1) as i32)
        .saturating_add(px(GAP, bounds.scale).saturating_mul(count.saturating_sub(1) as i32));
    let right = bounds
        .origin
        .x
        .saturating_add(bounds.width)
        .saturating_sub(width)
        .saturating_sub(px(MARGIN_R, bounds.scale))
        .max(left);
    let bottom = bounds
        .origin
        .y
        .saturating_add(bounds.height)
        .saturating_sub(stack_height)
        .saturating_sub(px(MARGIN_B, bounds.scale))
        .max(top);
    Point {
        x: requested.x.clamp(left, right),
        y: requested.y.clamp(top, bottom),
    }
}

fn default_stack_origin(bounds: MonitorBounds, count: usize) -> Point {
    clamp_stack_origin(
        Point {
            x: bounds.origin.x.saturating_add(bounds.width),
            y: bounds.origin.y.saturating_add(px(MARGIN_T, bounds.scale)),
        },
        bounds,
        count,
    )
}

fn overflow_to_drop(len: usize) -> usize {
    len.saturating_add(1).saturating_sub(MAX)
}

fn choose_monitor(
    monitors: impl IntoIterator<Item = MonitorBounds>,
    point: Point,
    fallback: Option<MonitorBounds>,
) -> Option<MonitorBounds> {
    monitors
        .into_iter()
        .find(|m| m.contains(point))
        .or(fallback)
}

/// macOS의 visibleFrame은 메뉴바와 Dock을 뺀 실제 작업영역이다. 좌표계가 AppKit
/// bottom-left라 winit 전역좌표로 직접 옮기지 않고, 전체 frame 대비 inset만 구해
/// winit이 준 현재 모니터 사각형에 적용한다.
#[cfg(target_os = "macos")]
fn visible_bounds_for_window(window: &Window, mut bounds: MonitorBounds) -> MonitorBounds {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use objc2_foundation::NSRect;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = window.window_handle() else {
        return bounds;
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return bounds;
    };
    let view = handle.ns_view.as_ptr() as *mut AnyObject;
    unsafe {
        let ns_window: *mut AnyObject = msg_send![view, window];
        if ns_window.is_null() {
            return bounds;
        }
        let screen: *mut AnyObject = msg_send![ns_window, screen];
        if screen.is_null() {
            return bounds;
        }
        let frame: NSRect = msg_send![screen, frame];
        let visible: NSRect = msg_send![screen, visibleFrame];
        let left = (visible.origin.x - frame.origin.x).max(0.0);
        let bottom = (visible.origin.y - frame.origin.y).max(0.0);
        let right = (frame.size.width - left - visible.size.width).max(0.0);
        let top = (frame.size.height - bottom - visible.size.height).max(0.0);
        let sx = bounds.scale;
        let left_px = (left * sx).round() as i32;
        let right_px = (right * sx).round() as i32;
        let top_px = (top * sx).round() as i32;
        let bottom_px = (bottom * sx).round() as i32;
        let horizontal = left_px.saturating_add(right_px);
        let vertical = top_px.saturating_add(bottom_px);
        bounds.origin.x = bounds.origin.x.saturating_add(left_px);
        bounds.origin.y = bounds.origin.y.saturating_add(top_px);
        bounds.width = bounds.width.saturating_sub(horizontal).max(1);
        bounds.height = bounds.height.saturating_sub(vertical).max(1);
    }
    bounds
}

#[cfg(not(target_os = "macos"))]
fn visible_bounds_for_window(_window: &Window, bounds: MonitorBounds) -> MonitorBounds {
    bounds
}

/// native window drag는 호출 동안 이벤트 루프를 AppKit이 소유한다. 다른 배너를
/// child window로 잠깐 묶으면 그 동안에도 스택 전체가 한 몸처럼 실시간 이동한다.
#[cfg(target_os = "macos")]
fn ns_window_of(window: &Window) -> Option<objc2::rc::Retained<objc2_app_kit::NSWindow>> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = window.window_handle().ok()?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return None;
    };
    unsafe {
        let view: &objc2_app_kit::NSView = handle.ns_view.cast().as_ref();
        view.window()
    }
}

#[cfg(target_os = "macos")]
fn attach_drag_group(windows: &[Arc<Window>], leader: usize) -> bool {
    let Some(parent) = windows.get(leader).and_then(|window| ns_window_of(window)) else {
        return false;
    };
    let children: Vec<_> = windows
        .iter()
        .enumerate()
        .filter(|(idx, _)| *idx != leader)
        .filter_map(|(_, window)| ns_window_of(window))
        .collect();
    if children.len() + 1 != windows.len() {
        return false;
    }
    for child in children {
        unsafe {
            parent.addChildWindow_ordered(&child, objc2_app_kit::NSWindowOrderingMode::Above)
        };
    }
    true
}

#[cfg(not(target_os = "macos"))]
fn attach_drag_group(_windows: &[Arc<Window>], _leader: usize) -> bool {
    false
}

#[cfg(target_os = "macos")]
fn detach_drag_group(windows: &[Arc<Window>], leader: usize) {
    let Some(parent) = windows.get(leader).and_then(|window| ns_window_of(window)) else {
        return;
    };
    for (idx, window) in windows.iter().enumerate() {
        if idx != leader {
            if let Some(child) = ns_window_of(window) {
                parent.removeChildWindow(&child);
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn detach_drag_group(_windows: &[Arc<Window>], _leader: usize) {}

fn read_saved_position() -> Option<Point> {
    let value = crate::socket::read_settings()
        .get("notify_banner_position")?
        .clone();
    let x = i32::try_from(value.get("x")?.as_i64()?).ok()?;
    let y = i32::try_from(value.get("y")?.as_i64()?).ok()?;
    Some(Point { x, y })
}

#[cfg(test)]
fn set_saved_position_value(settings: &mut serde_json::Value, point: Point) -> bool {
    let Some(object) = settings.as_object_mut() else {
        return false;
    };
    object.insert(
        "notify_banner_position".to_string(),
        serde_json::json!({ "x": point.x, "y": point.y }),
    );
    true
}

fn save_position(point: Point) {
    let _ = crate::socket::write_settings_patch_atomic(&[(
        "notify_banner_position",
        serde_json::json!({ "x": point.x, "y": point.y }),
    )]);
}

pub(crate) struct Banner {
    /// 자체 wgpu 렌더러. `window` 보다 먼저 드롭돼야 한다.
    pub(crate) gpu: gpu::GpuRenderer,
    pub(crate) window: Arc<Window>,
    title: String,
    body: String,
    /// 학생 이름 — 있으면 색으로 「누구」를 말한다.
    character: Option<String>,
    /// 눌렀을 때 갈 자리 `(pane id, 그때의 claude 세션 id)`. 세션까지 싣는 이유는
    /// surface id 가 재사용되기 때문이다 — 알림이 뜬 뒤 그 pane 이 닫히고 번호가
    /// 새 셸에 넘어가면 엉뚱한 자리로 간다(`macos_notify` 와 같은 규약). 못 맞추면
    /// **아무 데도 안 간다** — 엉뚱한 pane 으로 끌려가는 것보다 낫다.
    route: Option<(String, Option<String>)>,
    lifetime: Lifetime,
    hovered: bool,
    cursor: Option<(f64, f64)>,
    press: Option<Press>,
}

impl App {
    fn banner_stack_origin(&self) -> Option<Point> {
        self.banners
            .first()?
            .window
            .outer_position()
            .ok()
            .map(Point::from)
    }

    fn banner_stack_bounds(&self, requested: Point) -> Option<MonitorBounds> {
        let first = self.banners.first()?;
        let current = first
            .window
            .current_monitor()
            .map(|m| MonitorBounds::from_handle(&m));
        let chosen = choose_monitor(
            first
                .window
                .available_monitors()
                .map(|m| MonitorBounds::from_handle(&m)),
            requested,
            current,
        )?;
        Some(if current == Some(chosen) {
            visible_bounds_for_window(&first.window, chosen)
        } else {
            chosen
        })
    }

    fn place_banner_stack(&mut self, requested: Point) -> Option<Point> {
        if self.banners.is_empty() {
            return None;
        }
        let bounds = self.banner_stack_bounds(requested)?;
        let base = clamp_stack_origin(requested, bounds, self.banners.len());
        for (banner, position) in
            self.banners
                .iter()
                .zip(stack_positions(base, self.banners.len(), bounds.scale))
        {
            if banner.window.outer_position().ok().map(Point::from) != Some(position) {
                banner
                    .window
                    .set_outer_position(PhysicalPosition::new(position.x, position.y));
            }
        }
        Some(base)
    }

    fn reflow_banners_from(&mut self, base: Option<Point>) {
        if let Some(base) = base.or_else(|| self.banner_stack_origin()) {
            let _ = self.place_banner_stack(base);
        }
    }

    fn remove_banner(&mut self, idx: usize) {
        if idx >= self.banners.len() {
            return;
        }
        let base = self.banner_stack_origin();
        self.banners.remove(idx);
        self.reflow_banners_from(base);
        self.resume_banner_stack_if_unhovered(Instant::now());
    }

    fn pause_banner_stack(&mut self, now: Instant) {
        for banner in &mut self.banners {
            banner.lifetime.pause(now);
        }
    }

    fn resume_banner_stack_if_unhovered(&mut self, now: Instant) {
        if !stack_is_hovered(self.banners.iter().map(|banner| banner.hovered)) {
            for banner in &mut self.banners {
                banner.lifetime.resume(now);
            }
        }
    }

    fn translate_banner_stack(&mut self, leader: usize, delta: Point) {
        for (idx, banner) in self.banners.iter().enumerate() {
            if idx == leader {
                continue;
            }
            if let Ok(position) = banner.window.outer_position() {
                let moved = Point::from(position).offset(delta);
                banner
                    .window
                    .set_outer_position(PhysicalPosition::new(moved.x, moved.y));
            }
        }
    }

    fn finish_banner_drag(&mut self, idx: usize, origin: Point, moved_as_group: bool) {
        let Some(window) = self.banners.get(idx).map(|banner| banner.window.clone()) else {
            return;
        };
        let current = window
            .outer_position()
            .ok()
            .map(Point::from)
            .unwrap_or(origin);
        let delta = Point {
            x: current.x.saturating_sub(origin.x),
            y: current.y.saturating_sub(origin.y),
        };
        if !moved_as_group {
            self.translate_banner_stack(idx, delta);
        }
        if let Some(banner) = self.banners.get_mut(idx) {
            banner.press = None;
        }
        let requested = self.banner_stack_origin().unwrap_or(current);
        if let Some(base) = self.place_banner_stack(requested) {
            save_position(base);
        }
        self.resume_banner_stack_if_unhovered(Instant::now());
    }

    pub(crate) fn next_banner_deadline(&self) -> Option<Instant> {
        self.banners
            .iter()
            .filter(|banner| banner.press.is_none())
            .filter_map(|banner| banner.lifetime.deadline())
            .min()
    }

    /// 완료 배너를 한 장 띄운다. 실패하면 조용히 아무 일도 안 한다 — 알림을 못 띄운
    /// 것이 앱을 세울 이유는 아니다.
    pub(crate) fn push_notify_banner(
        &mut self,
        event_loop: &ActiveEventLoop,
        title: &str,
        body: &str,
        character: Option<String>,
        route: Option<(String, Option<String>)>,
    ) {
        let requested = self.banner_stack_origin().or_else(read_saved_position);
        // 오래된 것부터 걷어 자리를 낸다.
        let drop_count = overflow_to_drop(self.banners.len());
        if drop_count > 0 {
            self.banners.drain(0..drop_count);
            self.resume_banner_stack_if_unhovered(Instant::now());
        }
        // **메인 창이 있는 모니터**에 띄운다. 커서가 있는 화면을 쓰고 싶지만 winit 은
        // 전역 커서 위치를 안 준다. 그리고 화면이 여럿일 때 「내가 보고 있는 화면」에
        // 가장 가까운 답은 작업하던 창이 있는 쪽이다 — primary 로 고정하면 외장
        // 모니터로 일하는 동안 배너만 내장 화면에 뜬다.
        let mon = self
            .window
            .as_ref()
            .and_then(|w| w.current_monitor())
            .or_else(|| event_loop.primary_monitor());
        let fallback = mon
            .as_ref()
            .map(MonitorBounds::from_handle)
            .unwrap_or(MonitorBounds {
                origin: Point::default(),
                width: 1440,
                height: 900,
                scale: 1.0,
            });
        let monitor_at_saved = requested.and_then(|point| {
            event_loop
                .available_monitors()
                .map(|m| MonitorBounds::from_handle(&m))
                .find(|m| m.contains(point))
        });
        let bounds = monitor_at_saved.unwrap_or(fallback);
        let count = self.banners.len() + 1;
        let base = match (requested, monitor_at_saved) {
            (Some(point), Some(_)) => clamp_stack_origin(point, bounds, count),
            _ => default_stack_origin(bounds, count),
        };
        for (banner, position) in
            self.banners
                .iter()
                .zip(stack_positions(base, self.banners.len(), bounds.scale))
        {
            banner
                .window
                .set_outer_position(PhysicalPosition::new(position.x, position.y));
        }
        let position = stack_positions(base, count, bounds.scale)
            .pop()
            .unwrap_or(base);

        let attrs = WindowAttributes::default()
            .with_title("kasaterm 알림")
            .with_inner_size(LogicalSize::new(W, H))
            .with_position(PhysicalPosition::new(position.x, position.y))
            .with_decorations(false)
            .with_transparent(true)
            .with_resizable(false)
            // 다른 앱 위에 떠야 값이 있다. 알림이 필요한 순간은 정의상 이 앱을
            // 안 보고 있을 때다.
            .with_window_level(WindowLevel::AlwaysOnTop)
            // ★키 포커스를 가져가지 않는다. 이 한 줄이 이 기능의 전제다.
            .with_active(false);
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("[banner] 창 생성 실패 — 배너 건너뜀: {e}");
                return;
            }
        };
        window.set_ime_allowed(false);
        // 셀 폰트 크기는 다른 창과 같은 `FONT_SIZE` 로 둔다. 배너 글자 크기는
        // `draw_text` 의 `font_size` 로 따로 정한다.
        //
        // ⚠️ 한때 이 값을 13.0 으로 줬다가 「한글 자간이 그래서 벌어진다」고 적었는데
        // **그건 틀린 인과였다.** 두 캡처의 문장이 서로 달라 비교가 공평하지 않았고,
        // 값을 되돌려 다시 찍으니 그림이 바이트까지 같았다. 진짜 규칙은
        // `gpu::pen_step` 이다 — 와이드 글자는 `잉크 폭 + size*0.18` 로 미는데, 잉크
        // 폭이 글자마다 다르므로(「학」은 넓고 「이」는 좁다) 틈이 불규칙해진다.
        // **앱 전체 공통이라 인포 패널의 「프로 젝트 디렉터리」도 똑같이 벌어진다** —
        // 배너만 튀는 것이 아니고, 여기서 고칠 수 있는 자리도 아니다.
        let gpu = match gpu::GpuRenderer::new(window.clone(), crate::FONT_SIZE) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("[banner] gpu 초기화 실패 — 배너 건너뜀: {e}");
                return;
            }
        };
        let now = Instant::now();
        let mut lifetime = Lifetime::new(now);
        if stack_is_hovered(self.banners.iter().map(|banner| banner.hovered)) {
            lifetime.pause(now);
        }
        self.banners.push(Banner {
            gpu,
            window,
            title: title.to_string(),
            body: body.to_string(),
            character,
            route,
            lifetime,
            hovered: false,
            cursor: None,
            press: None,
        });
        if let Some(placed) = self.place_banner_stack(base) {
            if requested.is_some_and(|saved| saved != placed) {
                save_position(placed);
            }
        }
        let n = self.banners.len() - 1;
        self.draw_banner(n);
    }

    /// 배너 한 장을 그린다.
    pub(crate) fn draw_banner(&mut self, idx: usize) {
        let Some(b) = self.banners.get_mut(idx) else {
            return;
        };
        let scale = b.window.scale_factor() as f32;
        // ⚠️ **`set_scale` 만으로는 칸 폭이 안 따라온다.** 그건 scale 과 아틀라스만
        // 갈고 `cell_w`/`cell_h` 는 `set_font_size` 가 정한다 — 즉 칸 폭은 창을 만들
        // 때의 scale 로 굳는데, `GpuRenderer::new` 는 아직 화면에 안 붙은 창의
        // `scale_factor()` 를 읽으므로 창과 렌더러의 수명을 함께 묶는다.
        //
        // 지금 배너는 chrome 텍스트만 그려서 칸 폭을 안 쓴다 — 이 줄이 눈에 보이는
        // 것을 바꾸지는 않는다. 그래도 붙여 두는 건 칸이 실제 배율과 어긋난 채로
        // 굳어 있는 상태 자체가 함정이기 때문이다(셀을 한 줄이라도 그리게 되는
        // 순간 조용히 틀린다).
        b.gpu.set_scale(scale);
        b.gpu.set_font_size(crate::FONT_SIZE);
        let (pw, ph) = b.gpu.surface_size();
        if pw == 0 || ph == 0 {
            return;
        }
        let (w, h) = (pw as f32 / scale, ph as f32 / scale);
        // 학생색으로 「누구」를 말한다. 배정이 없으면 평소 강조색.
        let accent = b
            .character
            .as_deref()
            .and_then(theme::character_accent)
            .unwrap_or_else(theme::accent);
        let title = b.title.clone();
        let body = b.body.clone();
        let face_name = b.character.clone();
        let close_hovered = close_hit(b.cursor);

        b.gpu.clear_chrome();
        // 창을 투명으로 띄웠으므로 배경을 안 칠하면 **판 바깥이 그대로 비친다** —
        // 둥근 모서리가 사는 건 그 덕이다. 알파 합성이 안 먹는 환경에서는 이 판이
        // 그냥 사각형으로 보일 뿐, 내용은 그대로다.
        b.gpu
            .round_rect_fill(0.0, 0.0, w, h, theme::radius_md(), theme::panel_bg());
        // 왼쪽 색띠 — 알림 여럿이 쌓였을 때 누구 것인지 글자보다 먼저 읽힌다.
        b.gpu.round_rect_fill(0.0, 0.0, 4.0, h, 2.0, accent);

        // 학생 얼굴. 색띠가 색으로 말하는 「누구」를 그림으로 한 번 더 말한다 —
        // 배너는 다른 앱을 보던 중에 시야 가장자리로 들어오는 물건이라, 글자를
        // 읽기 전에 누구인지가 먼저 닿아야 값이 있다(거노 2026-08-21 「알림 배너에
        // 학생 프사도 이번에 뽑은 거 붙이자」).
        //
        // `draw_student_face` 를 그대로 쓴다 — 사이드바·statusline·tell 과 **같은
        // 함수·같은 이미지 키**라, 자리마다 다른 얼굴이 뜰 수가 없다. 학생이 없는
        // 알림이면 `false` 라 얼굴 없이 옛 여백 그대로 간다.
        const FACE: f32 = 48.0;
        let has_face = face_name.as_deref().is_some_and(|n| {
            crate::render::draw_student_face(&mut b.gpu, n, 14.0, (h - FACE) / 2.0, FACE)
        });
        let x0 = if has_face { 14.0 + FACE + 14.0 } else { 18.0 };
        // 얼굴이 폭을 가져간 만큼 글자가 설 자리가 좁아진다. 넘치면 판 밖으로
        // 흘러 옆 창 위에 글자만 떠 있는 꼴이 되므로 오른쪽 여백에서 자른다.
        let close_x = w - CLOSE_BOX;
        let clip_r = close_x - 4.0;
        let title = crate::info::fit_text(&mut b.gpu, &title, clip_r - x0, 13.0, true);
        let body = crate::info::fit_text(&mut b.gpu, &body, clip_r - x0, 12.0, false);
        b.gpu.draw_text_clipped(
            x0,
            16.0,
            &title,
            gpu::DrawOpts {
                font_size: 13.0,
                color: theme::text(),
                bold: true,
                italic: false,
            },
            x0,
            clip_r,
        );
        // 항상 보이는 선형 X. 글자나 유니코드 기호가 아니라 기존 아이콘 자산을
        // 같은 테마 색으로 그려, 어떤 폰트에서도 닫기 형태가 바뀌지 않는다.
        b.gpu.queue_icon(
            "x",
            close_x + (CLOSE_BOX - CLOSE_ICON) / 2.0,
            (CLOSE_BOX - CLOSE_ICON) / 2.0,
            CLOSE_ICON,
            if close_hovered {
                theme::text()
            } else {
                theme::text_dim()
            },
        );
        b.gpu.draw_text_clipped(
            x0,
            40.0,
            &body,
            gpu::DrawOpts {
                font_size: 12.0,
                color: theme::enforce_contrast_at(theme::text_dim(), theme::panel_bg(), 4.5),
                bold: false,
                italic: false,
            },
            x0,
            clip_r,
        );
        // 헤드리스 검증: 배너는 **별도 창**이라 메인 창을 찍는 `KASATERM_AUTOCAPTURE`
        // 로는 안 잡힌다. 자체 GPU 서피스라 별도 readback 이 필요하다.
        if let Ok(p) = std::env::var("KASATERM_AUTOBANNER_CAP") {
            let dot = p.rfind('.').unwrap_or(p.len());
            b.gpu.capture_next = Some(format!("{}-{idx}{}", &p[..dot], &p[dot..]));
        }
        let _ = b.gpu.render(&[], scale, 0.0, true);
    }

    /// 수명이 다한 배너를 걷고, 남은 것을 위로 당긴다. `about_to_wait` 에서 매 틱.
    pub(crate) fn expire_banners(&mut self) {
        if self.banners.is_empty() {
            return;
        }
        let now = Instant::now();
        let base = self.banner_stack_origin();
        let before = self.banners.len();
        self.banners
            .retain(|b| b.lifetime.alive(now, b.press.is_some()));
        if self.banners.len() != before {
            self.reflow_banners_from(base);
            self.resume_banner_stack_if_unhovered(now);
        } else if let Some(base) = base {
            // 모니터가 빠지거나 해상도/Dock 영역이 바뀌어도 다음 이벤트를 기다리지
            // 않고 현재 visible bounds 안으로 되돌린다. 같은 자리면 set을 생략한다.
            let _ = self.place_banner_stack(base);
        }
    }

    /// 배너 창으로 온 이벤트. 키 포커스는 받지 않지만 마우스 hover/click/drag는
    /// 배너 자체가 처리한다.
    pub(crate) fn banner_window_event(&mut self, idx: usize, event: &WindowEvent) -> bool {
        match event {
            WindowEvent::RedrawRequested => {
                self.draw_banner(idx);
                true
            }
            WindowEvent::CursorEntered { .. } => {
                if let Some(banner) = self.banners.get_mut(idx) {
                    banner.hovered = true;
                }
                self.pause_banner_stack(Instant::now());
                if let Some(banner) = self.banners.get(idx) {
                    banner.window.request_redraw();
                }
                true
            }
            WindowEvent::CursorLeft { .. } => {
                if let Some(banner) = self.banners.get_mut(idx) {
                    banner.hovered = false;
                    banner.cursor = None;
                    banner.window.set_cursor(CursorIcon::Default);
                    banner.window.request_redraw();
                }
                self.resume_banner_stack_if_unhovered(Instant::now());
                true
            }
            WindowEvent::CursorMoved { position, .. } => {
                let scale = self
                    .banners
                    .get(idx)
                    .map(|banner| banner.window.scale_factor())
                    .unwrap_or(1.0);
                let cursor = (position.x / scale, position.y / scale);
                let should_drag = self.banners.get(idx).is_some_and(|banner| {
                    matches!(
                        banner.press,
                        Some(Press::Card(drag))
                            if !drag.started && drag.crossed_threshold(cursor)
                    )
                });
                if let Some(banner) = self.banners.get_mut(idx) {
                    banner.cursor = Some(cursor);
                    let icon = if close_hit(banner.cursor) {
                        CursorIcon::Pointer
                    } else if banner.press.is_some() {
                        CursorIcon::Grabbing
                    } else {
                        CursorIcon::Grab
                    };
                    banner.window.set_cursor(icon);
                    banner.window.request_redraw();
                }
                if should_drag {
                    let (window, origin) = match self.banners.get_mut(idx) {
                        Some(banner) => match banner.press.as_mut() {
                            Some(Press::Card(drag)) => {
                                drag.started = true;
                                (banner.window.clone(), drag.origin)
                            }
                            _ => return true,
                        },
                        None => return true,
                    };
                    // winit의 native drag는 macOS에서 release 이벤트를 삼킬 수 있다.
                    // 호출이 돌아온 순간을 drag 종료로 삼아 route와 완전히 갈라 둔다.
                    let windows: Vec<_> = self
                        .banners
                        .iter()
                        .map(|banner| banner.window.clone())
                        .collect();
                    let moved_as_group = attach_drag_group(&windows, idx);
                    let dragged = window.drag_window().is_ok();
                    if moved_as_group {
                        detach_drag_group(&windows, idx);
                    }
                    if dragged {
                        self.finish_banner_drag(idx, origin, moved_as_group);
                    } else if let Some(banner) = self.banners.get_mut(idx) {
                        if let Some(Press::Card(drag)) = banner.press.as_mut() {
                            drag.started = false;
                        }
                    }
                }
                true
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                match state {
                    ElementState::Pressed => {
                        self.pause_banner_stack(Instant::now());
                        if let Some(banner) = self.banners.get_mut(idx) {
                            banner.press = if close_hit(banner.cursor) {
                                Some(Press::Close)
                            } else {
                                banner.window.outer_position().ok().map(|origin| {
                                    Press::Card(DragGesture::new(banner.cursor, origin.into()))
                                })
                            };
                        }
                    }
                    ElementState::Released => {
                        let (action, route, drag_origin) = match self.banners.get_mut(idx) {
                            Some(banner) => {
                                let press = banner.press.take();
                                let drag_origin = press.and_then(|press| match press {
                                    Press::Card(drag) => Some(drag.origin),
                                    Press::Close => None,
                                });
                                let action = press
                                    .map(|press| release_action(press, banner.cursor))
                                    .unwrap_or(ReleaseAction::None);
                                (action, banner.route.clone(), drag_origin)
                            }
                            None => (ReleaseAction::None, None, None),
                        };
                        match action {
                            ReleaseAction::Dismiss => self.remove_banner(idx),
                            ReleaseAction::Route => {
                                if let Some((pane, sid)) = route {
                                    let _ =
                                        self.proxy.send_event(UserEvent::NotifyFocus { pane, sid });
                                }
                                self.remove_banner(idx);
                            }
                            ReleaseAction::FinishDrag => {
                                if let Some(origin) = drag_origin {
                                    self.finish_banner_drag(idx, origin, false);
                                }
                            }
                            ReleaseAction::None => {
                                self.resume_banner_stack_if_unhovered(Instant::now())
                            }
                        }
                    }
                }
                true
            }
            WindowEvent::Resized(size) => {
                if let Some(banner) = self.banners.get_mut(idx) {
                    banner.gpu.resize(size.width, size.height);
                    banner.window.request_redraw();
                }
                let base = self.banner_stack_origin();
                self.reflow_banners_from(base);
                true
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                if let Some(banner) = self.banners.get_mut(idx) {
                    let scale = banner.window.scale_factor() as f32;
                    banner.gpu.set_scale(scale);
                    banner.gpu.set_font_size(crate::FONT_SIZE);
                    let size = banner.window.inner_size();
                    banner.gpu.resize(size.width, size.height);
                    banner.window.request_redraw();
                }
                let base = self.banner_stack_origin();
                self.reflow_banners_from(base);
                true
            }
            WindowEvent::CloseRequested | WindowEvent::Destroyed => {
                self.remove_banner(idx);
                true
            }
            _ => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifetime_is_eight_seconds_and_hover_pauses_it() {
        let start = Instant::now();
        let mut lifetime = Lifetime::new(start);
        assert_eq!(lifetime.until.duration_since(start), Duration::from_secs(8));

        lifetime.pause(start + Duration::from_secs(3));
        assert!(lifetime.alive(start + Duration::from_secs(30), false));
        assert_eq!(lifetime.deadline(), None);
        lifetime.resume(start + Duration::from_secs(30));
        assert_eq!(
            lifetime
                .until
                .duration_since(start + Duration::from_secs(30)),
            Duration::from_secs(5)
        );
    }

    #[test]
    fn hover_leave_restores_at_least_the_reading_grace() {
        let start = Instant::now();
        let mut lifetime = Lifetime::new(start);
        lifetime.pause(start + LIFE - Duration::from_millis(100));
        let leave = start + Duration::from_secs(20);
        lifetime.resume(leave);
        assert_eq!(lifetime.until.duration_since(leave), LEAVE_GRACE);

        // hover 중이던 맨 위 배너가 X로 사라져 CursorLeft를 못 보내도, 남은
        // 스택이 resume을 받으면 영구 pause가 되지 않는다.
        let mut survivor = Lifetime::new(start);
        survivor.pause(start + Duration::from_secs(1));
        survivor.resume(leave);
        assert!(survivor.deadline().is_some());
        assert!(!survivor.alive(survivor.until + Duration::from_millis(1), false));

        let mut stack = [Lifetime::new(start), Lifetime::new(start)];
        for item in &mut stack {
            item.pause(start + Duration::from_secs(2));
        }
        assert!(stack
            .iter()
            .all(|item| item.alive(start + Duration::from_secs(40), false)));
    }

    #[test]
    fn hover_state_does_not_depend_on_receiving_a_cursor_move() {
        // CursorEntered와 첫 CursorMoved 사이에도 hovered=true가 정본이다. 좌표가
        // 아직 없어도 새 배너와 남은 스택의 timer를 재개하면 안 된다.
        assert!(stack_is_hovered([false, true, false]));
        assert!(!stack_is_hovered([false, false]));
    }

    #[test]
    fn close_click_routes_nowhere_but_card_click_keeps_the_route_action() {
        let drag = DragGesture::new(Some((10.0, 10.0)), Point { x: 100, y: 100 });
        assert_eq!(
            release_action(Press::Close, Some((W as f64 - 4.0, 8.0))),
            ReleaseAction::Dismiss
        );
        assert_eq!(
            release_action(Press::Close, Some((10.0, 10.0))),
            ReleaseAction::None
        );
        assert_eq!(
            release_action(Press::Card(drag), Some((10.0, 10.0))),
            ReleaseAction::Route
        );
        let mut moved = drag;
        moved.started = true;
        assert_eq!(
            release_action(Press::Card(moved), Some((20.0, 20.0))),
            ReleaseAction::FinishDrag
        );
    }

    #[test]
    fn drag_threshold_includes_the_exact_boundary() {
        let drag = DragGesture::new(Some((20.0, 30.0)), Point::default());
        assert!(!drag.crossed_threshold((23.99, 30.0)));
        assert!(drag.crossed_threshold((24.0, 30.0)));
        assert!(drag.crossed_threshold((20.0, 34.0)));
        assert!(!DragGesture::new(None, Point::default()).crossed_threshold((100.0, 100.0)));
    }

    #[test]
    fn whole_stack_keeps_spacing_when_translated_and_reflowed() {
        let base = Point { x: 500, y: 200 };
        let original = stack_positions(base, 3, 1.0);
        let delta = Point { x: -120, y: 75 };
        let moved: Vec<Point> = original.iter().map(|point| point.offset(delta)).collect();
        assert_eq!(moved, stack_positions(base.offset(delta), 3, 1.0));

        // 가운데 장이 사라지면 같은 anchor에서 빈 칸 없이 두 장으로 재배치된다.
        assert_eq!(
            stack_positions(base, 2, 1.0),
            vec![
                base,
                Point {
                    x: base.x,
                    y: base.y + stack_step(1.0)
                }
            ]
        );
        assert_eq!(overflow_to_drop(MAX - 1), 0);
        assert_eq!(overflow_to_drop(MAX), 1);
        assert_eq!(overflow_to_drop(MAX + 2), 3);
    }

    #[test]
    fn saved_position_keeps_unrelated_settings() {
        let mut settings = serde_json::json!({
            "theme": "night",
            "font_size": 17,
            "nested": { "keep": true }
        });
        assert!(set_saved_position_value(
            &mut settings,
            Point { x: -320, y: 144 }
        ));
        assert_eq!(settings["theme"], "night");
        assert_eq!(settings["font_size"], 17);
        assert_eq!(settings["nested"]["keep"], true);
        assert_eq!(
            settings["notify_banner_position"],
            serde_json::json!({"x": -320, "y": 144})
        );
    }

    #[test]
    fn stack_origin_clamps_on_all_four_visible_edges() {
        let bounds = MonitorBounds {
            origin: Point { x: 100, y: 200 },
            width: 1000,
            height: 800,
            scale: 1.0,
        };
        let left_top = clamp_stack_origin(Point { x: -500, y: -500 }, bounds, 3);
        assert_eq!(left_top, Point { x: 114, y: 244 });
        let right_bottom = clamp_stack_origin(Point { x: 5000, y: 5000 }, bounds, 3);
        assert_eq!(right_bottom, Point { x: 726, y: 732 });
        assert_eq!(
            clamp_stack_origin(Point { x: 400, y: 500 }, bounds, 3),
            Point { x: 400, y: 500 }
        );

        let retina = MonitorBounds {
            origin: Point { x: -1920, y: -200 },
            width: 1920,
            height: 1080,
            scale: 2.0,
        };
        assert_eq!(
            clamp_stack_origin(
                Point {
                    x: i32::MIN,
                    y: i32::MIN
                },
                retina,
                3
            ),
            Point { x: -1892, y: -112 }
        );
        assert_eq!(
            clamp_stack_origin(
                Point {
                    x: i32::MAX,
                    y: i32::MAX
                },
                retina,
                3
            ),
            Point { x: -748, y: 344 }
        );
    }

    #[test]
    fn removed_monitor_falls_back_before_clamping() {
        let kept = MonitorBounds {
            origin: Point { x: 0, y: 0 },
            width: 1440,
            height: 900,
            scale: 1.0,
        };
        let old_position = Point { x: 2800, y: 100 };
        assert_eq!(choose_monitor([kept], old_position, Some(kept)), Some(kept));
        let clamped = clamp_stack_origin(old_position, kept, 3);
        assert!(kept.contains(clamped));
        assert!(clamped.x <= 1440 - W as i32 - MARGIN_R as i32);
    }
}
