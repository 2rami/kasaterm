use super::*;
use crate::image_preview::Response;
use crate::inline_media::{self, cache, Control, Rect, Reference};
use crate::media_player::{Playback, Player};
use kasa_gridview::images::InlineSlot;
use kasa_mcp::media_stream::Stream;
use winit::keyboard::PhysicalKey;

mod probe;
type Point = (f32, f32);
/// 재생 중인 프레임 텍스처 — 카드와 큰 미리보기가 같은 열쇠를 써서, 크게 보기로 옮겨도 그림이 끊기지 않는다.
const LIVE: &str = "media-live:";

#[derive(Clone)]
struct Hit {
    reference: Reference,
    rect: Rect,
    controls: Vec<(Control, Rect)>,
    track: Rect,
}
impl Hit {
    fn control(&self, point: Point) -> Option<Control> {
        self.controls.iter().find(|(_, r)| contains(*r, point)).map(|(c, _)| *c)
    }
}
struct Press {
    owner: WindowId,
    hit: Hit,
    point: Point,
    event: WindowEvent,
}
#[derive(Clone, Copy, PartialEq, Debug)]
enum Action {
    Close,
    Toggle,
    Back,
    Forward,
    Mute,
    Retry,
}

/// 재생 한 벌 — 카드 안이든 큰 미리보기든 같은 것을 쥔다. 크게 보기는 이것을 그대로 옮겨 이어 튼다.
struct Session {
    reference: Reference,
    generation: u64,
    asset: Option<Arc<cache::Asset>>,
    /// 재생기가 먼저 떨어지도록 스트림보다 위에 둔다. 스트림이 떨어지면 원본 읽기·연결이 취소된다.
    player: Option<Player>,
    stream: Option<Stream>,
    error: Option<String>,
    muted: bool,
    /// 준비되는 대로 튼다 — 카드는 누른 순간부터, 큰 미리보기는 창에 초점이 있을 때.
    autoplay: bool,
    frame_serial: u64,
}
impl Session {
    fn new(reference: Reference, generation: u64, autoplay: bool, proxy: &EventLoopProxy<UserEvent>) -> Self {
        cache::request(&reference, proxy, true);
        Self { reference, generation, asset: None, player: None, stream: None, error: None, muted: false, autoplay, frame_serial: 0 }
    }
    fn key(&self) -> String {
        format!("{LIVE}{}", self.generation)
    }
    fn state(&self) -> Option<Playback> {
        self.player.as_ref().map(Player::state)
    }
    fn video(&self) -> bool {
        self.asset.as_ref().is_some_and(|a| a.media.kind == "video")
    }
    fn busy(&self) -> bool {
        self.error.is_none()
            && self.player.as_ref().is_none_or(|player| {
                let s = player.state();
                s.playing || s.buffering || s.seeking
            })
    }
    /// 등록 정보가 오면 재생기를 연다. 무엇이 바뀌었으면 true.
    fn tick(&mut self, proxy: &EventLoopProxy<UserEvent>) -> bool {
        if self.asset.is_some() || self.error.is_some() {
            return false;
        }
        match cache::get(&self.reference) {
            Ok(Some(asset)) => {
                let opened = Stream::open(self.reference.source.as_deref(), &asset.media)
                    .map_err(|e| e.to_string())
                    .and_then(|stream| Player::open_url(stream.url()).map(|player| (stream, player)));
                match opened {
                    Ok((stream, mut player)) => {
                        if crate::verification_run() {
                            player.set_volume(0.0);
                            self.muted = true;
                        }
                        if self.autoplay {
                            player.play();
                        }
                        self.player = Some(player);
                        self.stream = Some(stream);
                    }
                    Err(error) => self.error = Some(error),
                }
                self.asset = Some(asset);
                true
            }
            Err(error) => {
                self.error = Some(error);
                true
            }
            Ok(None) => {
                cache::request(&self.reference, proxy, false);
                false
            }
        }
    }
    fn toggle(&mut self) {
        let Some(player) = &mut self.player else {
            self.autoplay = !self.autoplay;
            return;
        };
        let s = player.state();
        if s.playing {
            player.pause();
        } else {
            if s.ended {
                player.seek(0.0);
            }
            player.play();
        }
    }
    fn pause(&mut self) {
        self.autoplay = false;
        if let Some(player) = &mut self.player {
            player.pause();
        }
    }
    fn step(&mut self, by: f64) {
        if let Some(player) = &mut self.player {
            let s = player.state();
            player.seek((s.position + by).max(0.0).min(s.duration.unwrap_or(f64::MAX)));
        }
    }
    fn mute(&mut self) {
        self.muted = !self.muted;
        if let Some(player) = &mut self.player {
            player.set_volume(if self.muted { 0.0 } else { 1.0 });
        }
    }
    fn seek_fraction(&mut self, fraction: f32) {
        if let Some(player) = &mut self.player {
            if let Some(d) = player.state().duration {
                player.seek(d * fraction.clamp(0.0, 1.0) as f64);
            }
        }
    }
    /// 재생기가 실패한 까닭과, 잠깐 끊긴 원본의 오류(다음 조각에서 걷힌다).
    fn failure(&self) -> (Option<String>, Option<String>) {
        let stream_error = self.stream.as_ref().and_then(Stream::error);
        let failure = self.error.clone().or_else(|| {
            self.state()
                .and_then(|s| s.error)
                .map(|e| stream_error.clone().unwrap_or(e))
        });
        (failure, stream_error)
    }
    fn upload_frame(&mut self, g: &mut gpu::GpuRenderer) {
        let key = self.key();
        if let Some(frame) = self.player.as_mut().and_then(Player::take_frame) {
            self.frame_serial = frame.serial;
            g.update_image(&key, &frame.rgba, frame.width, frame.height);
        }
    }
}

struct Preview {
    owner: WindowId,
    context: usize,
    session: Session,
    /// 카드에서 크게 보기로 넘어왔다 — 닫으면 재생을 카드로 돌려준다.
    from_card: bool,
    actions: Vec<(Action, Rect)>,
    seek: Rect,
    dragging: bool,
    focus: usize,
}
/// 카드 안에서 도는 재생. 한 번에 하나다.
struct Inline {
    owner: WindowId,
    context: usize,
    session: Session,
    /// 탐색 막대를 끄는 중 — 그 카드의 막대 전체 자리.
    dragging: Option<Rect>,
}
#[derive(Default)]
pub(crate) struct State {
    hits: HashMap<WindowId, (usize, Vec<Hit>)>,
    press: Option<Press>,
    preview: Option<Preview>,
    inline: Option<Inline>,
    /// 저장하는 중인 카드 — 끝나면 일꾼이 뺀다.
    saving: Arc<Mutex<std::collections::HashSet<Reference>>>,
    pub replay: bool,
    generation: u64,
    suppress_key: Option<(WindowId, PhysicalKey)>,
    consumed_button: Option<(WindowId, MouseButton)>,
    focused: Option<WindowId>,
    probe_at: Option<Instant>,
}
fn contains(r: Rect, p: Point) -> bool {
    r.2 > 0.0 && r.3 > 0.0 && p.0 >= r.0 && p.1 >= r.1 && p.0 < r.0 + r.2 && p.1 < r.1 + r.3
}
fn moved(a: Point, b: Point) -> bool {
    (a.0 - b.0).hypot(a.1 - b.1) > 4.0
}
/// 파일 이름으로 못 쓰는 글자를 걷는다 — 다른 기기에서 온 카드 이름이 폴더 밖을 가리키지 않게.
fn file_name(name: &str) -> String {
    let clean: String = name
        .chars()
        .map(|c| if c.is_control() || matches!(c, '/' | '\\' | ':') { '_' } else { c })
        .collect();
    let clean = clean.trim().trim_start_matches('.').to_string();
    if clean.is_empty() { "media".into() } else { clean }
}
impl State {
    pub fn close_owner(&mut self, id: WindowId) {
        if self.owns(id) {
            self.preview = None;
        }
        if self.inline.as_ref().is_some_and(|i| i.owner == id) {
            self.inline = None;
        }
        self.hits.remove(&id);
        if self.press.as_ref().is_some_and(|p| p.owner == id) {
            self.press = None;
        }
        if self.focused == Some(id) {
            self.focused = None;
        }
        if self.suppress_key.is_some_and(|(owner, _)| owner == id) {
            self.suppress_key = None;
        }
        if self.consumed_button.is_some_and(|(owner, _)| owner == id) {
            self.consumed_button = None;
        }
    }
    pub fn owns(&self, id: WindowId) -> bool {
        self.preview.as_ref().is_some_and(|p| p.owner == id)
    }
    pub fn focused(&self) -> bool {
        self.preview
            .as_ref()
            .is_some_and(|p| self.focused == Some(p.owner))
    }
    /// 그리기를 계속 돌려야 하나 — 재생·버퍼링·탐색 중인 것이 있다.
    pub fn active(&self) -> bool {
        self.preview.as_ref().is_some_and(|p| p.session.busy()) || self.inline.as_ref().is_some_and(|i| i.session.busy())
    }
    /// 그 가운데 영상이 있다 — 원본 프레임마다 그려야 끊겨 보이지 않는다.
    #[cfg(windows)]
    pub fn video_active(&self) -> bool {
        let video = |s: &Session| s.busy() && s.video();
        self.preview.as_ref().is_some_and(|p| video(&p.session)) || self.inline.as_ref().is_some_and(|i| video(&i.session))
    }
    pub fn hit(&self, id: WindowId, point: Point) -> bool {
        self.hits
            .get(&id)
            .is_some_and(|(_, hits)| hits.iter().any(|h| contains(h.rect, point)))
    }
    /// 그리지 않는 창(내용 없는 별도 터미널 창)의 카드 자리. 방이 바뀌면 그 창의 재생을 거둔다.
    pub fn collect(&mut self, id: WindowId, context: usize, slots: &[InlineSlot]) {
        self.forget_context(id, context);
        self.hits.insert(
            id,
            (
                context,
                slots
                    .iter()
                    .filter_map(|s| {
                        Reference::from_slot(s).map(|reference| Hit {
                            reference,
                            rect: inline_media::intersection(s.rect, s.clip),
                            controls: Vec::new(),
                            track: (0.0, 0.0, 0.0, 0.0),
                        })
                    })
                    .collect(),
            ),
        );
    }
    fn forget_context(&mut self, id: WindowId, context: usize) {
        if self
            .preview
            .as_ref()
            .is_some_and(|p| p.owner == id && p.context != context)
        {
            self.preview = None;
        }
        if self
            .inline
            .as_ref()
            .is_some_and(|i| i.owner == id && i.context != context)
        {
            self.inline = None;
        }
    }
    /// 본문의 카드를 그리고 누름 자리를 남긴다. 카드에서 도는 재생은 프레임·시각·조작을 함께 그린다.
    /// 영상은 카드가 화면을 벗어나면 멈춘다 — 안 보이는 그림을 돌릴 까닭이 없다. 음성은 계속 듣는다.
    pub fn paint_cards(&mut self, id: WindowId, context: usize, g: &mut gpu::GpuRenderer, slots: &[InlineSlot], cursor: Point) {
        self.forget_context(id, context);
        let inline = self.inline.as_mut().filter(|i| i.owner == id);
        let live = inline.map(|i| {
            i.session.upload_frame(g);
            let s = i.session.state();
            let (failure, stream_error) = i.session.failure();
            let note = failure.or_else(|| {
                s.as_ref().and_then(|s| {
                    if s.buffering {
                        Some(stream_error.clone().unwrap_or_else(|| "준비 중…".into()))
                    } else if s.ended {
                        Some("재생 완료".into())
                    } else {
                        None
                    }
                })
            });
            let key = i.session.key();
            (
                i.session.reference.clone(),
                g.has_image(&key).then_some(key),
                s,
                i.session.muted,
                note.or_else(|| i.session.player.is_none().then(|| "불러오는 중…".into())),
                i.session.video(),
            )
        });
        let saving = self.saving.lock().map(|s| s.clone()).unwrap_or_default();
        let cards = {
            let view = live.as_ref().map(|(reference, frame, s, muted, note, _)| inline_media::Live {
                reference,
                frame: frame.as_deref(),
                playing: s.as_ref().is_some_and(|s| s.playing) || s.is_none(),
                position: s.as_ref().map_or(0.0, |s| s.position),
                duration: s.as_ref().and_then(|s| s.duration),
                muted: *muted,
                note: note.clone(),
            });
            inline_media::paint(g, slots, cursor, view.as_ref(), &|r| saving.contains(r))
        };
        if let Some((reference, _, s, _, _, video)) = &live {
            let shown = cards.iter().any(|c| c.reference == *reference);
            if !shown && *video && s.as_ref().is_some_and(|s| s.playing) {
                if let Some(i) = self.inline.as_mut() {
                    i.session.pause();
                }
            }
        }
        self.retain_live(g);
        self.hits.insert(
            id,
            (
                context,
                cards
                    .into_iter()
                    .map(|c| Hit { reference: c.reference, rect: c.rect, controls: c.controls, track: c.track })
                    .collect(),
            ),
        );
    }
    /// 재생 중인 세션의 프레임만 남긴다 — 카드와 큰 미리보기 어느 쪽이 쥐었든.
    fn retain_live(&self, g: &mut gpu::GpuRenderer) {
        let keep: Vec<String> = [self.preview.as_ref().map(|p| p.session.key()), self.inline.as_ref().map(|i| i.session.key())]
            .into_iter()
            .flatten()
            .collect();
        g.images.retain(|k, _| !k.starts_with(LIVE) || keep.contains(k));
    }
    pub fn cursor(&self, id: WindowId, point: Point) -> CursorIcon {
        if self.hit(id, point)
            || self.preview.as_ref().is_some_and(|p| {
                p.owner == id
                    && (p.actions.iter().any(|(_, r)| contains(*r, point))
                        || contains(p.seek, point))
            })
        {
            CursorIcon::Pointer
        } else {
            CursorIcon::Default
        }
    }
    /// 본문 밖에서 고른 음성·영상(Info 「결과물」)을 같은 미리보기로 연다 — 받기·재생·닫기 길이 카드 누름과 같다.
    pub(crate) fn open_reference(&mut self, id: WindowId, reference: Reference, proxy: &EventLoopProxy<UserEvent>) {
        self.open(id, reference, proxy);
    }
    fn open(&mut self, id: WindowId, reference: Reference, proxy: &EventLoopProxy<UserEvent>) {
        if self.inline.as_ref().is_some_and(|i| i.session.reference == reference && i.owner == id) {
            self.expand(id);
            return;
        }
        self.generation += 1;
        self.focused = Some(id);
        let context = self.hits.get(&id).map_or(0, |h| h.0);
        self.preview = Some(Preview {
            owner: id,
            context,
            session: Session::new(reference, self.generation, true, proxy),
            from_card: false,
            actions: Vec::new(),
            seek: (0.0, 0.0, 0.0, 0.0),
            dragging: false,
            focus: 0,
        });
    }
    /// 카드에서 크게 보기 — 도는 재생을 그대로 옮긴다(받은 구간·위치·소리 그대로).
    fn expand(&mut self, id: WindowId) {
        let Some(inline) = self.inline.take() else { return };
        self.focused = Some(id);
        self.preview = Some(Preview {
            owner: inline.owner,
            context: inline.context,
            session: inline.session,
            from_card: true,
            actions: Vec::new(),
            seek: (0.0, 0.0, 0.0, 0.0),
            dragging: false,
            focus: 0,
        });
    }
    /// 카드를 누름 — 그 카드가 돌고 있으면 재생·일시정지, 아니면 그 카드에서 새로 튼다(돌던 다른 카드는 거둔다).
    fn play_inline(&mut self, id: WindowId, reference: Reference, proxy: &EventLoopProxy<UserEvent>) {
        if let Some(i) = self.inline.as_mut().filter(|i| i.owner == id && i.session.reference == reference) {
            i.session.toggle();
            return;
        }
        self.generation += 1;
        let context = self.hits.get(&id).map_or(0, |h| h.0);
        self.inline = Some(Inline { owner: id, context, session: Session::new(reference, self.generation, true, proxy), dragging: None });
    }
    /// 카드의 「저장」 — 원본 파일 전체를 다운로드 폴더에 받는다. 다른 기기 카드는 그 기기에서 받는다.
    fn save(&mut self, reference: Reference, proxy: &EventLoopProxy<UserEvent>) {
        let Ok(Some(asset)) = cache::get(&reference) else { return };
        let Ok(mut saving) = self.saving.lock() else { return };
        if !saving.insert(reference.clone()) {
            return;
        }
        drop(saving);
        let saving = Arc::clone(&self.saving);
        let proxy = proxy.clone();
        let media = asset.media.clone();
        std::thread::spawn(move || {
            let dest = crate::webpane::download_path(&file_name(&media.name));
            let result = kasa_mcp::media_stream::Stream::save(reference.source.as_deref(), &media, &dest);
            if let Ok(mut s) = saving.lock() {
                s.remove(&reference);
            }
            let message = match result {
                Ok(()) => {
                    #[cfg(target_os = "macos")]
                    if !crate::verification_run() {
                        let _ = crate::proc::command("open").arg("-R").arg(&dest).spawn();
                    }
                    // 탐색기는 `/select,` 뒤 경로를 따옴표째 한 덩어리로 받아야 한다 — 보통 인자 따옴표는
                    // 쉼표 앞까지 감싸 빈 폴더 창이 뜬다.
                    #[cfg(windows)]
                    if !crate::verification_run() {
                        use std::os::windows::process::CommandExt;
                        let _ = crate::proc::command("explorer")
                            .raw_arg(format!("/select,\"{}\"", dest.display()))
                            .spawn();
                    }
                    format!("다운로드 폴더에 저장했어요 · {}", dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
                }
                Err(e) => format!("저장하지 못했어요 · {e:#}"),
            };
            eprintln!("[media-save] {} → {}: {message}", media.id, dest.display());
            let _ = proxy.send_event(UserEvent::SocketToast(message));
        });
    }
    pub fn tick(&mut self, proxy: &EventLoopProxy<UserEvent>) -> bool {
        let focused = self.focused;
        let preview = self.preview.as_mut().is_some_and(|p| {
            // 큰 미리보기는 창에 초점이 있을 때만 바로 튼다 — 뒤에서 연 것이 소리부터 내지 않게.
            if !p.from_card {
                p.session.autoplay = focused == Some(p.owner);
            }
            p.session.tick(proxy)
        });
        let inline = self.inline.as_mut().is_some_and(|i| i.session.tick(proxy));
        preview || inline
    }
    fn action(&mut self, action: Action, proxy: &EventLoopProxy<UserEvent>) {
        if action == Action::Close {
            if let Some(p) = self.preview.take().filter(|p| p.from_card) {
                self.inline = Some(Inline { owner: p.owner, context: p.context, session: p.session, dragging: None });
            }
            return;
        }
        let Some(p) = self.preview.as_mut() else {
            return;
        };
        match action {
            Action::Retry => {
                let (owner, reference) = (p.owner, p.session.reference.clone());
                self.preview = None;
                self.open(owner, reference, proxy);
            }
            Action::Toggle => p.session.toggle(),
            Action::Back => p.session.step(-5.0),
            Action::Forward => p.session.step(5.0),
            Action::Mute => p.session.mute(),
            Action::Close => {}
        }
    }
    pub fn event(
        &mut self,
        owner: WindowId,
        point: Point,
        modifiers: ModifiersState,
        event: &WindowEvent,
        proxy: &EventLoopProxy<UserEvent>,
        allow: bool,
    ) -> Response {
        if self.replay {
            return Response::Pass;
        }
        if let WindowEvent::Focused(focused) = event {
            if *focused {
                self.focused = Some(owner);
            } else if self.focused == Some(owner) {
                self.focused = None;
            }
        }
        if let WindowEvent::KeyboardInput { event, .. } = event {
            if self.suppress_key == Some((owner, event.physical_key)) {
                if event.state == ElementState::Released {
                    self.suppress_key = None;
                }
                return Response::Consumed;
            }
        }
        if let WindowEvent::MouseInput { state, button, .. } = event {
            if !state.is_pressed() && self.consumed_button == Some((owner, *button)) {
                self.consumed_button = None;
                if !self.owns(owner) {
                    return Response::Consumed;
                }
            } else if state.is_pressed() && self.owns(owner) {
                self.consumed_button = Some((owner, *button));
            }
        }
        if matches!(event, WindowEvent::Focused(false) | WindowEvent::Destroyed) {
            self.press = None;
            self.suppress_key = None;
            self.consumed_button = None;
            // 카드의 영상은 창을 떠나면 멈춘다 — 음성은 다른 일을 하며 듣도록 둔다.
            if let Some(i) = self.inline.as_mut().filter(|i| i.owner == owner) {
                i.dragging = None;
                if i.session.video() {
                    i.session.pause();
                }
            }
        }
        if self.owns(owner) {
            let p = self.preview.as_mut().unwrap();
            let mut action = None;
            match event {
                WindowEvent::CloseRequested | WindowEvent::Destroyed => {
                    self.preview = None;
                    self.inline = None;
                    self.hits.remove(&owner);
                    return Response::Pass;
                }
                WindowEvent::Focused(false) => {
                    p.session.pause();
                    p.dragging = false;
                    return Response::Pass;
                }
                WindowEvent::KeyboardInput { event, .. } => {
                    if event.state.is_pressed() {
                        action = match &event.logical_key {
                            Key::Named(NamedKey::Escape) => Some(Action::Close),
                            Key::Named(NamedKey::Space) => Some(Action::Toggle),
                            Key::Named(NamedKey::ArrowLeft) => Some(Action::Back),
                            Key::Named(NamedKey::ArrowRight) => Some(Action::Forward),
                            Key::Named(NamedKey::Enter) => p.actions.get(p.focus).map(|(a, _)| *a),
                            Key::Named(NamedKey::Tab) => {
                                if !p.actions.is_empty() {
                                    p.focus = (p.focus
                                        + if modifiers.shift_key() {
                                            p.actions.len() - 1
                                        } else {
                                            1
                                        })
                                        % p.actions.len();
                                }
                                None
                            }
                            Key::Character(k) if k == "m" => Some(Action::Mute),
                            _ => None,
                        };
                        if action == Some(Action::Close) {
                            self.suppress_key = Some((owner, event.physical_key));
                        }
                    }
                }
                WindowEvent::MouseInput {
                    state,
                    button: MouseButton::Left,
                    ..
                } => {
                    if state.is_pressed() {
                        action = p
                            .actions
                            .iter()
                            .find(|(_, r)| contains(*r, point))
                            .map(|(a, _)| *a);
                        p.dragging = contains(p.seek, point)
                            && p.session.state().and_then(|s| s.duration).is_some();
                        if p.dragging {
                            p.seek_to(point);
                        }
                    } else {
                        if p.dragging {
                            p.seek_to(point);
                        }
                        p.dragging = false;
                    }
                }
                WindowEvent::CursorMoved { .. } => {
                    if p.dragging {
                        p.seek_to(point);
                    }
                }
                WindowEvent::Ime(_)
                | WindowEvent::MouseWheel { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::Touch(_)
                | WindowEvent::PinchGesture { .. }
                | WindowEvent::DroppedFile(_) => {}
                _ => return Response::Pass,
            }
            if let Some(action) = action {
                self.action(action, proxy);
            }
            return Response::Consumed;
        }
        if let Some(i) = self.inline.as_mut().filter(|i| i.owner == owner && i.dragging.is_some()) {
            let track = i.dragging.unwrap_or_default();
            let at = |point: Point| if track.2 > 0.0 { (point.0 - track.0) / track.2 } else { 0.0 };
            match event {
                WindowEvent::CursorMoved { .. } => {
                    i.session.seek_fraction(at(point));
                    return Response::Consumed;
                }
                WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                    i.session.seek_fraction(at(point));
                    i.dragging = None;
                    return Response::Consumed;
                }
                _ => {}
            }
        }
        if let Some(press) = self.press.as_ref().filter(|p| p.owner == owner) {
            match event {
                WindowEvent::CursorMoved { .. } if moved(press.point, point) => {
                    let p = self.press.take().unwrap();
                    return Response::Replay(p.event, p.point);
                }
                WindowEvent::MouseInput {
                    state: ElementState::Released,
                    button: MouseButton::Left,
                    ..
                } => {
                    let p = self.press.take().unwrap();
                    let same = self.hits.get(&owner).and_then(|(_, hits)| {
                        hits.iter().find(|h| h.reference == p.hit.reference && contains(h.rect, point)).cloned()
                    });
                    if let Some(hit) = same.filter(|_| allow && !moved(p.point, point)) {
                        let playing = self
                            .inline
                            .as_ref()
                            .is_some_and(|i| i.owner == owner && i.session.reference == hit.reference);
                        match hit.control(point) {
                            Some(Control::Expand) => {
                                if playing {
                                    self.expand(owner);
                                } else {
                                    self.open(owner, hit.reference, proxy);
                                }
                            }
                            Some(Control::Save) => self.save(hit.reference, proxy),
                            Some(Control::Mute) if playing => {
                                if let Some(i) = self.inline.as_mut() {
                                    i.session.mute();
                                }
                            }
                            Some(Control::Seek) if playing => {}
                            _ => self.play_inline(owner, hit.reference, proxy),
                        }
                    }
                    return Response::Consumed;
                }
                WindowEvent::Focused(false)
                | WindowEvent::MouseWheel { .. }
                | WindowEvent::KeyboardInput { .. } => {
                    self.press = None;
                }
                WindowEvent::CursorMoved { .. } => return Response::Consumed,
                _ => {}
            }
        }
        if allow
            && !modifiers.alt_key()
            && !modifiers.super_key()
            && !modifiers.control_key()
            && matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                    ..
                }
            )
        {
            if let Some(hit) = self
                .hits
                .get(&owner)
                .and_then(|(_, hits)| hits.iter().rev().find(|h| contains(h.rect, point)))
                .cloned()
            {
                if hit.control(point) == Some(Control::Seek) {
                    if let Some(i) = self.inline.as_mut().filter(|i| i.owner == owner && i.session.reference == hit.reference) {
                        let fraction = if hit.track.2 > 0.0 { (point.0 - hit.track.0) / hit.track.2 } else { 0.0 };
                        i.session.seek_fraction(fraction);
                        i.dragging = Some(hit.track);
                        return Response::Consumed;
                    }
                }
                self.press = Some(Press {
                    owner,
                    hit,
                    point,
                    event: event.clone(),
                });
                return Response::Consumed;
            }
        }
        Response::Pass
    }
    pub fn paint(&mut self, owner: WindowId, g: &mut gpu::GpuRenderer, size: Point, cursor: Point) {
        let Some(p) = self.preview.as_mut().filter(|p| p.owner == owner) else {
            self.retain_live(g);
            self.observe(owner, g);
            return;
        };
        let (w, h) = size;
        let audio = p.session.asset.as_ref().is_some_and(|a| a.media.kind == "audio");
        let ow = (w - 32.0)
            .max(0.0)
            .min(if audio { 640.0 } else { f32::MAX });
        let oh = (h - 32.0)
            .max(0.0)
            .min(if audio { 240.0 } else { f32::MAX });
        let outer = ((w - ow) * 0.5, (h - oh) * 0.5, ow, oh);
        g.rect(0.0, 0.0, w, h, theme::with_alpha(theme::bg(), 235));
        crate::native_controls::panel(g, outer, theme::surface());
        let x = outer.0 + 16.0;
        let right = (outer.0 + outer.2 - 16.0).max(x);
        let mut y = outer.1 + 16.0;
        p.actions.clear();
        let close = crate::native_controls::icon_button(
            g,
            ((right - 32.0).max(x), y, 32.0, 32.0),
            cursor,
            "x",
            Default::default(),
        );
        p.actions.push((Action::Close, close));
        inline_media::text(
            g,
            (x, y + 8.0, (right - x - 40.0).max(0.0), 24.0),
            p.session.asset.as_ref().map_or("미디어", |a| a.media.name.as_str()),
            false,
        );
        y += 40.0;
        let mut bx = x;
        let s = p.session.state();
        // 스트림 오류는 일시 끊김이면 다음 조각에서 걷힌다 — 굳히지 않고, 재생기가 실패하면 그 까닭으로·
        // 기다리는 중이면 덧붙여 보인다.
        let (failure, stream_error) = p.session.failure();
        for (action, label) in [
            (
                Action::Toggle,
                if s.as_ref().is_some_and(|s| s.playing) {
                    "일시정지"
                } else {
                    "재생"
                },
            ),
            (Action::Back, "−5초"),
            (Action::Forward, "+5초"),
            (
                Action::Mute,
                if p.session.muted {
                    "소리 켜기"
                } else {
                    "음소거"
                },
            ),
            (Action::Retry, "다시 시도"),
        ] {
            if action == Action::Retry && failure.is_none() && stream_error.is_none() {
                continue;
            }
            let width = if action == Action::Toggle {
                32.0
            } else {
                crate::native_controls::button_width(g, label, 32.0, false)
            }
            .min((right - x).max(0.0));
            if bx > x && bx + width > right {
                bx = x;
                y += 40.0;
            }
            let enabled = p.session.player.is_some() || action == Action::Retry;
            let style = crate::native_controls::Style {
                enabled,
                ..Default::default()
            };
            let r = if action == Action::Toggle {
                crate::native_controls::icon_button(
                    g,
                    (bx, y, width, 32.0),
                    cursor,
                    if s.as_ref().is_some_and(|s| s.playing) {
                        "pause"
                    } else {
                        "play"
                    },
                    style,
                )
            } else {
                crate::native_controls::text_button(g, (bx, y, width, 32.0), cursor, label, style)
            };
            if enabled {
                p.actions.push((action, r));
            }
            bx += width + 8.0;
        }
        y += 40.0;
        let status = if let Some(error) = failure {
            error
        } else if let Some(s) = &s {
            format!(
                "{} / {}{}",
                inline_media::time(s.position),
                s.duration
                    .map(inline_media::time)
                    .unwrap_or_else(|| "—".into()),
                match (&stream_error, s.buffering, s.ended) {
                    (Some(e), true, _) => format!(" · {e}"),
                    (None, true, _) => " · 준비 중…".into(),
                    (_, false, true) => " · 재생 완료".into(),
                    _ => String::new(),
                }
            )
        } else {
            "미디어를 불러오는 중…".into()
        };
        inline_media::text(g, (x, y, right - x, 24.0), &status, true);
        y += 24.0;
        p.seek = (x, y, (right - x).max(0.0), 24.0);
        g.rect(x, y + 10.0, p.seek.2, 4.0, theme::border());
        if let Some(s) = &s {
            if let Some(d) = s.duration.filter(|d| *d > 0.0) {
                g.rect(
                    x,
                    y + 10.0,
                    p.seek.2 * (s.position / d).clamp(0.0, 1.0) as f32,
                    4.0,
                    theme::accent(),
                );
            }
        }
        y += 32.0;
        let area = (
            x,
            y,
            (right - x).max(0.0),
            (outer.1 + outer.3 - 16.0 - y).max(0.0),
        );
        if let Some((_, r)) = p
            .actions
            .get(p.focus.min(p.actions.len().saturating_sub(1)))
        {
            crate::native_controls::focus_ring(g, *r);
        }
        let key = p.session.key();
        p.session.upload_frame(g);
        if !g.has_image(&key) {
            if let Some(poster) = p.session.asset.as_ref().and_then(|a| a.poster.as_ref()) {
                g.upload_image(&key, &poster.rgba, poster.width, poster.height);
            }
        }
        if g.has_image(&key) {
            g.queue_image_contain(&key, area.0, area.1, area.2, area.3);
        } else if audio {
            inline_media::text(
                g,
                (x, y + 16.0, area.2, 24.0),
                "Space 재생·일시정지  ·  ← → 5초 이동  ·  Esc 닫기",
                true,
            );
        }
        self.retain_live(g);
        self.observe(owner, g);
    }
}
impl Preview {
    fn seek_to(&mut self, point: Point) {
        if self.seek.2 > 0.0 {
            self.session.seek_fraction((point.0 - self.seek.0) / self.seek.2);
        }
    }
}

impl App {
    pub(crate) fn focus_preview_surface(&mut self, point: Point) {
        if let Some((pane, _, _)) = self.px_to_pane_cell(point.0, point.1) {
            let (surface, switched) = {
                let ws = self.ws.lock().unwrap();
                (
                    ws.active_tab_pid(&pane),
                    ws.active_pane.as_deref() != Some(pane.as_str()),
                )
            };
            self.focus_surface(&surface);
            if switched {
                self.selection = None;
                self.drag_anchor = None;
                self.mouse_forward_pane = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_card_name_from_another_device_cannot_leave_the_downloads_folder() {
        assert_eq!(file_name("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(file_name("..\\시험.mp4"), "_시험.mp4");
        assert_eq!(file_name(" 회의 녹음.m4a "), "회의 녹음.m4a");
        assert_eq!(file_name("..."), "media");
        assert_eq!(file_name("a:b\nc.mp4"), "a_b_c.mp4");
    }

    #[test]
    fn a_press_on_a_card_goes_to_the_control_under_it() {
        let hit = Hit {
            reference: Reference { id: "0123456789abcdef".into(), source: None },
            rect: (0.0, 0.0, 400.0, 88.0),
            controls: vec![(Control::Toggle, (12.0, 40.0, 32.0, 32.0)), (Control::Save, (356.0, 40.0, 32.0, 32.0))],
            track: (0.0, 0.0, 0.0, 0.0),
        };
        assert_eq!(hit.control((20.0, 50.0)), Some(Control::Toggle));
        assert_eq!(hit.control((360.0, 50.0)), Some(Control::Save));
        assert_eq!(hit.control((200.0, 20.0)), None, "카드 몸통은 재생·일시정지로 간다");
    }
}
