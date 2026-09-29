//! 날씨(리퀴드 비) — 정본 docs/weather.md.
//!
//! 창 사각형·초점·입력·단추 자리는 앱 상태에서 바로 읽는다. 매 프레임 `weather_frame` 이
//! 자리 목록을 만들어 시뮬레이션을 한 걸음 옮기고 GPU 가 그릴 한 장(`gpu::Frame`)을 낸다.
//! 꺼져 있으면 아무것도 만들지 않는다.

pub(crate) mod gpu;
pub(crate) mod model;
pub(crate) mod sim;

use crate::agent_state::AgentState;
use crate::App;
use model::{OsMotion, PaneMood, PaneWeather, RainAmount, Verdict, WeatherSettings};
use sim::{ButtonSpot, Place, PlaceKind};
use std::collections::HashMap;
use std::time::{Duration, Instant};

const RIPPLE_TEXEL: f32 = 3.0;
const RIPPLE_HZ: f32 = 60.0;
const MIST_TIME: f32 = 10.0;

#[derive(Default)]
pub(crate) struct WeatherState {
    pub settings: WeatherSettings,
    /// 창 우클릭 「이 창 날씨」 — 이 기기 세션에만. key 는 pane id.
    pub overrides: HashMap<String, PaneWeather>,
    pub mouse_down: bool,
    world: sim::World,
    os: OsMotion,
    os_at: Option<Instant>,
    last: Option<Instant>,
    typed: Option<String>,
    places: Vec<Place>,
    spots: Vec<ButtonSpot>,
    ripple_acc: f32,
    ripple_tail: f32,
    scale: f32,
    win: [f32; 2],
}

fn key_of(id: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut h);
    h.finish()
}

fn mood(state: &AgentState) -> PaneMood {
    match state {
        AgentState::Working | AgentState::Compacting => PaneMood::Busy,
        s if s.needs_you() => PaneMood::YourTurn,
        AgentState::Idle | AgentState::Waiting { .. } => PaneMood::Resting,
        _ => PaneMood::Unknown,
    }
}

#[cfg(target_os = "macos")]
fn os_motion() -> OsMotion {
    use objc2::runtime::{AnyClass, AnyObject, Bool};
    use objc2::msg_send;
    let Some(cls) = AnyClass::get(c"NSWorkspace") else { return OsMotion::default() };
    unsafe {
        let ws: *mut AnyObject = msg_send![cls, sharedWorkspace];
        if ws.is_null() {
            return OsMotion::default();
        }
        let motion: Bool = msg_send![ws, accessibilityDisplayShouldReduceMotion];
        let transparency: Bool = msg_send![ws, accessibilityDisplayShouldReduceTransparency];
        OsMotion { reduce_motion: motion.as_bool(), reduce_transparency: transparency.as_bool() }
    }
}

#[cfg(not(target_os = "macos"))]
fn os_motion() -> OsMotion {
    OsMotion::default()
}

impl WeatherState {
    pub(crate) fn with_settings(settings: WeatherSettings) -> Self {
        WeatherState { settings, ..Default::default() }
    }

    pub(crate) fn load(value: Option<&serde_json::Value>) -> WeatherSettings {
        value
            .and_then(|v| serde_json::from_value::<WeatherSettings>(v.clone()).ok())
            .unwrap_or_default()
            .sanitized()
    }

    fn verdict(&mut self) -> Verdict {
        if !self.settings.enabled {
            return Verdict { active: false, moving: false };
        }
        if self.os_at.is_none_or(|t| t.elapsed() > Duration::from_secs(2)) {
            self.os = os_motion();
            self.os_at = Some(Instant::now());
        }
        model::verdict(&self.settings, self.os)
    }

    pub(crate) fn pane_override(&self, pane: &str) -> PaneWeather {
        self.overrides.get(pane).copied().unwrap_or_default()
    }

    /// Wet places still dripping after the rain stopped keep the frames coming.
    fn busy(&self, v: Verdict) -> bool {
        v.active
            && v.moving
            && (self.places.iter().any(|p| p.amount != RainAmount::None) || !self.world.settled() || self.ripple_tail > 0.0)
    }
}

impl App {
    pub(crate) fn weather_settings_changed(&mut self) {
        if !self.weather.settings.enabled {
            self.weather.world.clear();
            self.weather.places.clear();
            self.weather.spots.clear();
        }
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    pub(crate) fn weather_note_typed(&mut self, pane: &str) {
        if self.weather.settings.enabled {
            self.weather.typed = Some(pane.to_owned());
        }
    }

    /// 비가 오거나 물이 아직 흐르면 다음 장을 불러야 한다.
    pub(crate) fn weather_animating(&mut self) -> bool {
        let v = self.weather.verdict();
        self.weather.busy(v)
    }

    /// After a frame's painting: keep the controls it drew for the next frame's drops.
    pub(crate) fn weather_take_spots(&mut self, spots: Vec<ButtonSpot>) {
        if !self.weather.settings.enabled {
            return;
        }
        let cursor = self.cursor_px;
        let hover = |r: (f32, f32, f32, f32)| cursor.0 >= r.0 && cursor.1 >= r.1 && cursor.0 < r.0 + r.2 && cursor.1 < r.1 + r.3;
        let mut all = spots;
        let chrome = [
            Some(self.sidebar_toggle_rect()),
            Some(self.file_tree_toggle_rect()),
            self.git_col_toggle_rect(),
            self.settings_title_rect(),
            self.status_account_rect,
            self.status_version_rect,
        ];
        for r in chrome.into_iter().flatten() {
            if r.2 > 0.0 && r.3 > 0.0 && r.2 < 240.0 {
                all.push(ButtonSpot { rect: [r.0, r.1, r.2, r.3], hover: hover(r), enabled: true });
            }
        }
        all.truncate(gpu::MAX_BUTTONS);
        self.weather.spots = all;
    }

    /// 이번 프레임의 날씨. `panes` 는 창 상자(id, x, y, w, h — 논리 px), `guard` 는 초점 창
    /// 입력줄. 꺼져 있으면 `None`.
    pub(crate) fn weather_frame(
        &mut self,
        scale: f32,
        win: [f32; 2],
        panes: &[(String, f32, f32, f32, f32)],
        active: Option<&str>,
        guard: Option<[f32; 4]>,
    ) -> Option<gpu::Frame> {
        let v = self.weather.verdict();
        if !v.active {
            return None;
        }
        let s = self.weather.settings.clone();
        let bg = model::background_amount(&s);
        let mut places = Vec::new();
        for (id, x, y, w, h) in panes {
            let focused = active == Some(id.as_str());
            let m = if s.by_status { mood(&self.agent_state(id)) } else { PaneMood::Unknown };
            places.push(Place {
                key: key_of(id),
                kind: PlaceKind::Pane,
                rect: [*x, *y, *w, *h],
                amount: model::pane_amount(&s, self.weather.pane_override(id), focused, m),
                focused,
                guard: if focused { guard } else { None },
            });
        }
        let title = crate::TITLE_HEIGHT;
        let body_h = (win[1] - title - self.status_h()).max(0.0);
        let columns = [
            ("panel:sidebar", 0.0, self.tab_strip_w()),
            ("panel:left", self.tab_strip_w(), self.left_panel_col_w()),
            ("panel:files", self.file_tree_col_x(), self.file_tree_col_w()),
            ("panel:right", self.git_col_x(), self.git_col_w()),
        ];
        for (name, x, w) in columns {
            if w > 1.0 {
                places.push(Place { key: key_of(name), kind: PlaceKind::Panel, rect: [x, title, w, body_h], amount: bg, focused: false, guard: None });
            }
        }
        places.push(Place { key: key_of("bar:title"), kind: PlaceKind::Bar, rect: [0.0, 0.0, win[0], title], amount: bg, focused: false, guard: None });
        if self.status_h() > 0.0 {
            places.push(Place {
                key: key_of("bar:status"),
                kind: PlaceKind::Bar,
                rect: [0.0, win[1] - self.status_h(), win[0], self.status_h()],
                amount: bg,
                focused: false,
                guard: None,
            });
        }
        places.truncate(gpu::MAX_PLACES);
        self.weather.places = places;
        self.weather.scale = scale;
        self.weather.win = win;
        Some(self.weather_step(v))
    }

    /// A frame from the last full frame's places, for redraws where only the weather moved.
    pub(crate) fn weather_frame_cached(&mut self) -> Option<gpu::Frame> {
        let v = self.weather.verdict();
        if !v.active || self.weather.places.is_empty() {
            return None;
        }
        Some(self.weather_step(v))
    }

    fn weather_step(&mut self, v: Verdict) -> gpu::Frame {
        let now = Instant::now();
        let dt = self.weather.last.map_or(1.0 / 30.0, |l| (now - l).as_secs_f32()).clamp(0.0, 0.1);
        self.weather.last = Some(now);
        let st = &mut self.weather;
        let s = st.settings.clone();
        let (scale, win) = (st.scale, st.win);
        let amount_at = |p: [f32; 2], places: &[Place]| {
            places.iter().find(|pl| sim::inside(p, pl.rect)).map_or(model::background_amount(&s), |pl| pl.amount)
        };
        let spots: Vec<ButtonSpot> = if s.effects.buttons {
            st.spots
                .iter()
                .filter(|b| {
                    let c = [b.rect[0] + b.rect[2] * 0.5, b.rect[1] + b.rect[3] * 0.5];
                    amount_at(c, &st.places) != RainAmount::None
                })
                .copied()
                .collect()
        } else {
            Vec::new()
        };
        let typed = st.typed.take().map(|id| key_of(&id));
        let ev = sim::Events { typed, mouse: Some([self.cursor_px.0, self.cursor_px.1]), mouse_down: st.mouse_down };
        let places = st.places.clone();
        st.world.step(dt, &s, v.moving, &places, &spots, &ev, win);

        // -- to the GPU (physical px) --
        let (w, h) = (win[0] * scale, win[1] * scale);
        let px = |r: [f32; 4]| [r[0] * scale, r[1] * scale, r[2] * scale, r[3] * scale];
        let ripple_dim = [(win[0] / RIPPLE_TEXEL).ceil().max(8.0) as u32, (win[1] / RIPPLE_TEXEL).ceil().max(8.0) as u32];
        let mut speed = 0.7f32;
        let mut pu = gpu::PlacesU::zeroed();
        let bgl = sim::LEVEL[model::background_amount(&s).index()];
        pu.n = [places.len() as f32, if s.effects.streaks { bgl[0] } else { 0.0 }, bgl[1], 0.0];
        let mut any_streak = s.effects.streaks && bgl[0] > 0.01;
        let mut any_panel = false;
        let mut any_glass = false;
        for (i, p) in places.iter().enumerate() {
            let Some(ps) = st.world.places.get(&p.key) else { continue };
            let kind = match p.kind {
                PlaceKind::Pane => 0.0,
                PlaceKind::Panel => 1.0,
                PlaceKind::Bar => 2.0,
            };
            let rewet = s.rewet_secs as f32;
            let mist = if s.effects.mist && p.kind == PlaceKind::Pane && p.amount != RainAmount::None { ps.mist_target(rewet) } else { 0.0 };
            let ripples = s.effects.ripples && p.kind == PlaceKind::Panel && p.amount != RainAmount::None;
            let damping = if ripples { if p.focused { 0.975 } else { 0.995 } } else { 0.0 };
            let streak = if s.effects.streaks { ps.cur[0] } else { 0.0 };
            speed = speed.max(ps.cur[2]);
            any_streak |= streak > 0.01;
            any_panel |= ripples;
            any_glass |= p.kind == PlaceKind::Pane && (mist > 0.0 || ps.pool > 1.0 || ps.wiper.is_some());
            pu.rect[i] = px(p.rect);
            pu.a[i] = [kind, mist, ps.pool_h() * scale, ps.wiper_y().map_or(-1.0, |y| y * scale)];
            pu.b[i] = [ps.wave * scale, damping, streak, ps.cur[1]];
            pu.c[i] = p.guard.map_or([0.0; 4], px);
        }
        if !st.world.ripples.is_empty() {
            st.ripple_tail = 8.0;
        }
        st.ripple_tail = (st.ripple_tail - dt).max(0.0);
        let ripple_on = s.effects.ripples && (any_panel || st.ripple_tail > 0.0);
        let mut ripple_steps = 0;
        if ripple_on && v.moving {
            st.ripple_acc = (st.ripple_acc + dt * RIPPLE_HZ).min(4.0);
            ripple_steps = st.ripple_acc.floor() as u32;
            st.ripple_acc -= ripple_steps as f32;
        }
        let ripples: Vec<[f32; 4]> = st
            .world
            .ripples
            .iter()
            .map(|d| [d[0] / RIPPLE_TEXEL, d[1] / RIPPLE_TEXEL, d[2] / RIPPLE_TEXEL, d[3]])
            .collect();

        let mut inst: Vec<gpu::Inst> = Vec::new();
        let rect_of = |k: u64| st.world.places.get(&k).map_or([0.0; 4], |p| p.rect);
        for d in st.world.drops.iter().take(gpu::MAX_INST / 2) {
            let sz = d.size();
            inst.push(gpu::Inst { pos: d.pos, size: sz, extra: [sz[0] / 100.0, 0.0, 0.0, 0.0], clip: rect_of(d.place) });
        }
        let n_drops = inst.len() as u32;
        for (at, size, k) in st.world.droplets.iter().take(gpu::MAX_INST / 4) {
            inst.push(gpu::Inst { pos: *at, size: [*size; 2], extra: [0.0; 4], clip: rect_of(*k) });
        }
        let n_droplets = inst.len() as u32 - n_drops;
        for p in st.world.places.values().filter(|p| p.kind == PlaceKind::Pane) {
            if let Some(y) = p.wiper_y() {
                inst.push(gpu::Inst { pos: [p.rect[0], p.rect[1]], size: [p.rect[2], y - p.rect[1]], extra: [0.0; 4], clip: p.rect });
            }
        }
        let n_wipes = inst.len() as u32 - n_drops - n_droplets;
        if s.effects.mist {
            for p in st.world.places.values().filter(|p| p.kind == PlaceKind::Pane) {
                inst.push(gpu::Inst { pos: [p.rect[0], p.rect[1]], size: [p.rect[2], p.rect[3]], extra: [dt / MIST_TIME, 0.0, 0.0, 0.0], clip: p.rect });
            }
        }
        let n_mist = inst.len() as u32 - n_drops - n_droplets - n_wipes;
        any_glass |= n_drops > 0 || n_droplets > 0;

        let mut bu = gpu::ButtonsU::zeroed();
        let mut boxes = Vec::new();
        let btns = &st.world.buttons;
        bu.n = [btns.len().min(gpu::MAX_BUTTONS) as f32, 0.0, 0.0, 0.0];
        for (i, b) in btns.iter().take(gpu::MAX_BUTTONS).enumerate() {
            bu.b[i] = [b.pos[0] * scale, b.pos[1] * scale, b.half[0] * scale, b.half[1] * scale];
            bu.s[i] = [b.hover, b.press, b.inflate, b.ring_t];
            let clip = places.iter().find(|pl| sim::inside(b.pos, pl.rect)).map_or([0.0, 0.0, win[0], win[1]], |pl| pl.rect);
            bu.c[i] = px(clip);
            bu.m[i] = [b.group as f32, 0.0, 0.0, 0.0];
        }
        for (g, k) in st.world.group_k.iter().take(gpu::MAX_ROWS).enumerate() {
            bu.k[g] = [k * scale, 0.0, 0.0, 0.0];
            let mut lo = [f32::MAX; 2];
            let mut hi = [f32::MIN; 2];
            for b in btns.iter().take(gpu::MAX_BUTTONS).filter(|b| b.group == g) {
                let ring = if (0.0..0.7).contains(&b.ring_t) { b.half[0].min(b.half[1]) + 52.0 } else { 0.0 };
                let reach = [(b.half[0] * 1.3 + 12.0).max(ring), (b.half[1] * 1.3 + 12.0).max(ring)];
                lo = [lo[0].min(b.pos[0] - reach[0]), lo[1].min(b.pos[1] - reach[1])];
                hi = [hi[0].max(b.pos[0] + reach[0]), hi[1].max(b.pos[1] + reach[1])];
            }
            if lo[0] < hi[0] {
                let (x0, y0) = ((lo[0] * scale).max(0.0), (lo[1] * scale).max(0.0));
                let (x1, y1) = ((hi[0] * scale).min(w), (hi[1] * scale).min(h));
                if x1 > x0 && y1 > y0 {
                    boxes.push(gpu::Inst { pos: [x0, y0], size: [x1 - x0, y1 - y0], ..Default::default() });
                }
            }
        }

        gpu::Frame {
            globals: gpu::Globals {
                res: [w, h, 1.0 / w.max(1.0), 1.0 / h.max(1.0)],
                t: [st.world.time % 3600.0, dt, scale, 0.0],
                rain: [speed, s.wind() * 0.45, 0.0, 0.0],
                rip: [w / ripple_dim[0] as f32, h / ripple_dim[1] as f32, 1.0 / ripple_dim[0] as f32, 1.0 / ripple_dim[1] as f32],
            },
            places: pu,
            buttons: bu,
            boxes,
            ripples,
            ripple_steps,
            ripple_dim,
            inst,
            counts: [n_drops, n_droplets, n_wipes, n_mist],
            canvas: win,
            rain: any_streak && v.moving,
            ripple: ripple_on,
            glass: any_glass && (s.effects.drops || s.effects.mist),
        }
    }
}

use bytemuck::Zeroable;

impl App {
    /// Nothing but the weather moved: redraw it over the last full frame.
    pub(crate) fn weather_only_frame(&mut self) {
        if !self.weather.settings.enabled || !self.weather_animating() {
            return;
        }
        let Some(f) = self.weather_frame_cached() else { return };
        let Some(g) = self.gpu.as_mut() else { return };
        match g.render_weather_only(f) {
            Ok(true) => {}
            // No copy of the app frame yet (first frame, resize): take a full one.
            Ok(false) => {
                self.chrome_dirty = true;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            Err(e) => eprintln!("[weather] redraw: {e:?}"),
        }
    }
}
