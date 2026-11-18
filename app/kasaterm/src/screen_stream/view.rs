//! 보는 쪽 — 카사넷으로 받은 장을 풀기 일꾼이 순서대로 VT 로 풀고, 마지막 장만 GUI 가 가져가 그린다.
//! 칸 하나에 `ScreenView` 하나. 버리면 연결과 일꾼이 함께 닫힌다.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use kasa_remote::screen_stream::{self, Config, Event, Frame, Keyframes, Watch};
use serde_json::json;

use super::platform::{Picture, VideoDecoder};
use super::{contains, Layout, Surface};

/// GPU 에 넘긴 뒤 쥐고 있을 장 수 — 60fps 에서 50ms. 그려지는 동안 디코더가 그 버퍼에 다음 장을 풀지 않게.
const HELD: usize = 3;

struct Incoming {
    frame: Frame,
    offset: f64,
}

#[derive(Default)]
struct Stats {
    /// 최근 1초의 (풀린 시각, 화면→풀림 ms, 바이트).
    window: VecDeque<(f64, f64, usize)>,
    path: &'static str,
    frames: u64,
    latency_sum: f64,
}

struct Shared {
    latest: Mutex<Option<Picture>>,
    serial: AtomicU64,
    status: Mutex<String>,
    stats: Mutex<Stats>,
    /// 조작을 못 넣는 까닭 — 넣을 수 있으면 None.
    input_note: Mutex<Option<String>>,
    /// 그 기기가 Windows 다 — 압축 정보의 `os`, 그것을 안 싣는 판은 Windows 도우미 표시(`agent`)로 안다.
    windows: AtomicBool,
}


pub(crate) struct ScreenView {
    pub label: String,
    shared: Arc<Shared>,
    watch: Option<Watch>,
    uploaded: u64,
    /// 조작 중 — 칸 위 누름·휠·키를 그 기기에 넣는다. 처음엔 보기만 한다(보다가 잘못 누르면 그 기기에서 일이 일어난다).
    pub control: bool,
    pub layout: Option<Layout>,
    /// 확대 배율(1 = 칸에 맞춤)과 칸 가운데에 둘 장 안 자리(비율).
    pub zoom: f32,
    pub focus: (f64, f64),
    /// 키 메뉴가 열려 있다 — 줄마다 그린 자리와 그 줄(`special_keys` 차례). 칠할 때 채운다.
    pub key_menu: Option<Vec<((f32, f32, f32, f32), usize)>>,
    /// 지금 그 기기에 눌려 있는 단추 — 칸 밖으로 끌어 나가도 뗄 때까지 이어 보낸다.
    pub buttons: u8,
    /// 조합 중인 한글 — 그 기기에는 다 된 글자만 가니 칸 위에 보여 준다.
    pub preedit: String,
    frame: (u32, u32),
    /// GPU 에 넘긴 최근 장들 — 복사 없이 IOSurface 를 텍스처로 쓰므로 GPU 가 다 그릴 때까지 디코더가 같은 버퍼를 다시 못
    /// 쓰게 쥐고 있는다. 마지막 것은 텍스처를 못 만들 때 CPU 로 올리는 밑감이다.
    held: VecDeque<Picture>,
    /// 마지막 장을 복사 없이 올렸나 — 검증 리그가 읽는다.
    pub zero_copy: bool,
    /// 안 보이기 시작한 때 — 오래 안 보이면 연결을 닫는다(`pane.rs sync_screen_views`).
    pub hidden_since: Option<std::time::Instant>,
    /// 마지막 장을 GPU 로 올리는 데 든 시간(ms)과 올린 장 수 — 검증 리그가 읽는다.
    pub upload_ms: f64,
    pub uploads: u64,
    /// 지금 그려지는 창 — 누름은 그 창에서 온 것만 받는다(본창 칸 또는 꺼낸 화면 창).
    pub surface: Surface,
    /// 연결이 닫힌 때 — 보이는 동안 잠시 뒤 다시 붙는다(`pane.rs sync_screen_views`).
    pub closed_since: Option<std::time::Instant>,
}

impl ScreenView {
    /// 그 기기(base) 화면을 본다. 새 장이 풀릴 때마다 `wake` 를 부른다 — GUI 를 깨워 다시 그리게.
    /// 그 기기가 화면 스트림을 모르는 판이거나 카사넷 직통이 아니면 연결 없이 까닭만 들고 연다 — 칸은 그대로 두고
    /// 길이 서면 다시 붙는다.
    /// `fit` 은 이 기기가 그릴 수 있는 가장 큰 픽셀 크기 — 그 기기는 이 안에 들도록 줄여 보낸다.
    pub(crate) fn open(label: &str, base: &str, fit: (u32, u32), wake: Arc<dyn Fn() + Send + Sync>) -> Option<Self> {
        let shared = Arc::new(Shared {
            latest: Mutex::new(None),
            serial: AtomicU64::new(0),
            status: Mutex::new("카사넷으로 붙는 중".into()),
            stats: Mutex::new(Stats::default()),
            input_note: Mutex::new(None),
            windows: AtomicBool::new(false),
        });
        let config = Config { codec: VideoDecoder::CODEC.into(), max_w: fit.0, max_h: fit.1, ..Config::default() };
        let (tx, rx) = mpsc::channel::<Incoming>();
        let keyframes: Arc<Mutex<Option<Keyframes>>> = Arc::new(Mutex::new(None));
        let on = {
            let (shared, wake) = (shared.clone(), wake.clone());
            let tx = Mutex::new(tx);
            move |event: Event| match event {
                Event::Connected { path, .. } => {
                    shared.stats.lock().unwrap_or_else(|e| e.into_inner()).path = path;
                    set_status(&shared, &wake, "화면을 받는 중");
                }
                Event::Info(info) => {
                    let windows = info.get("os").and_then(|v| v.as_str()) == Some("windows") || info.get("agent").is_some();
                    shared.windows.store(windows, Ordering::Relaxed);
                }
                Event::Frame { frame, offset, .. } => {
                    let _ = tx.lock().unwrap_or_else(|e| e.into_inner()).send(Incoming { frame, offset });
                }
                Event::Denied(reason) => {
                    let text = if reason == "permission" {
                        "그 기기 카사텀에 화면 기록 권한이 없어요 — 그 기기에서 켜고 카사텀을 다시 열어 주세요"
                    } else {
                        "그 기기가 화면을 보낼 수 없어요"
                    };
                    set_status(&shared, &wake, text);
                }
                Event::Closed(why) => set_status(&shared, &wake, &closed_text(&why)),
                Event::Input { ok, reason } => {
                    let note = (!ok).then(|| {
                        if reason.as_deref() == Some("permission") {
                            "그 기기 카사텀에 손쉬운 사용 권한이 없어 조작을 못 넣어요".to_string()
                        } else {
                            "그 기기에 조작을 넣을 수 없어요".to_string()
                        }
                    });
                    *shared.input_note.lock().unwrap_or_else(|e| e.into_inner()) = note;
                    wake();
                }
            }
        };
        let watch = if kasa_device::kasanet::screen_capable(base) {
            let watch = screen_stream::watch(base.to_string(), config.clone(), on)?;
            *keyframes.lock().unwrap_or_else(|e| e.into_inner()) = Some(watch.keyframes());
            Some(watch)
        } else {
            *shared.status.lock().unwrap_or_else(|e| e.into_inner()) = match kasa_device::kasanet::path(base) {
                Some((p, _)) if p != "ssh" => "그 기기 카사텀이 화면 스트림을 모르는 판이에요 — 그 기기를 업데이트하면 보여요".into(),
                _ => "카사넷 직통을 기다리는 중 — 길이 서면 바로 붙어요".into(),
            };
            None
        };
        let worker_shared = shared.clone();
        std::thread::Builder::new()
            .name("screen-stream-decode".into())
            .spawn(move || decode_loop(rx, keyframes, &worker_shared, &wake))
            .ok()?;
        Some(Self {
            label: label.to_string(),
            shared,
            watch,
            uploaded: 0,
            upload_ms: 0.0,
            uploads: 0,
            control: false,
            layout: None,
            zoom: 1.0,
            focus: (0.5, 0.5),
            key_menu: None,
            buttons: 0,
            preedit: String::new(),
            frame: (0, 0),
            held: VecDeque::new(),
            zero_copy: false,
            hidden_since: None,
            surface: Surface::Main,
            closed_since: None,
        })
    }

    /// 조작을 켜면 그 기기 커서를 장에서 뺀다 — 이 기기 포인터가 곧 커서가 되어 영상 지연 없이 따라온다(Parsec 처럼).
    pub(crate) fn set_control(&mut self, on: bool) {
        self.control = on;
        if let Some(w) = &self.watch {
            w.set_cursor_hidden(on);
        }
    }

    /// 연결이 없거나 닫혔다.
    pub(crate) fn closed(&self) -> bool {
        self.watch.as_ref().is_none_or(Watch::is_closed)
    }

    /// 다시 붙을 때 이전 까닭을 이어 보인다 — 붙는 중 글로 덮으면 왜 안 되는지가 깜빡이며 사라진다.
    pub(crate) fn carry_status(&self, previous: &str) {
        let mut status = self.shared.status.lock().unwrap_or_else(|e| e.into_inner());
        if *status == "카사넷으로 붙는 중" {
            *status = previous.to_string();
        }
    }

    /// 지난번에 가져간 뒤 새로 풀린 장이 있으면 복사 없이 텍스처로 삼을 표면(macOS IOSurface·Windows D3D11 공유 핸들, 없으면
    /// null)·그 표면의 번호·크기를 내준다. 장은 `held` 에 쥔다.
    pub(crate) fn new_surface(&mut self) -> Option<(*mut std::ffi::c_void, u64, u32, u32)> {
        let serial = self.shared.serial.load(Ordering::Acquire);
        if serial == self.uploaded {
            return None;
        }
        self.uploaded = serial;
        self.take_latest()
    }

    /// 새 장을 기다리지 않고 마지막 장을 다시 낸다 — 칸을 다른 창으로 옮겨 그 창 렌더러가 아직 장을 안 가졌을 때.
    pub(crate) fn current_surface(&mut self) -> Option<(*mut std::ffi::c_void, u64, u32, u32)> {
        if self.uploaded == 0 {
            return None;
        }
        self.take_latest()
    }

    fn take_latest(&mut self) -> Option<(*mut std::ffi::c_void, u64, u32, u32)> {
        let px = self.shared.latest.lock().unwrap_or_else(|e| e.into_inner()).clone()?;
        let (w, h) = px.size();
        let (surface, key) = px.surface();
        self.held.push_back(px);
        while self.held.len() > HELD {
            self.held.pop_front();
        }
        self.frame = (w, h);
        Some((surface, key, w, h))
    }

    /// 마지막 장을 BGRA 바이트(가로·세로·줄 간격)로 — 표면을 텍스처로 못 삼을 때.
    pub(crate) fn copy_latest(&self, f: impl FnOnce(&[u8], u32, u32, u32)) {
        if let Some(px) = self.held.back() {
            px.with_bgra(f);
        }
    }

    pub(crate) fn note_upload(&mut self, took: std::time::Duration, zero_copy: bool) {
        self.upload_ms = took.as_secs_f64() * 1000.0;
        self.uploads += 1;
        self.zero_copy = zero_copy;
        super::platform::zero_copy_result(zero_copy);
    }

    pub(crate) fn has_new_frame(&self) -> bool {
        self.shared.serial.load(Ordering::Acquire) != self.uploaded
    }

    pub(crate) fn has_frame(&self) -> bool {
        self.uploaded != 0
    }

    pub(crate) fn frame_size(&self) -> (u32, u32) {
        self.frame
    }

    pub(crate) fn input_note(&self) -> Option<String> {
        self.shared.input_note.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub(crate) fn on_toggle(&self, p: (f64, f64)) -> bool {
        self.layout.is_some_and(|l| contains(l.toggle, p))
    }

    pub(crate) fn on_image(&self, p: (f64, f64)) -> bool {
        self.layout.is_some_and(|l| contains(l.image, p))
    }

    pub(crate) fn on_key_button(&self, p: (f64, f64)) -> bool {
        self.layout.and_then(|l| l.key_button).is_some_and(|r| contains(r, p))
    }

    /// 열린 키 메뉴에서 `p` 의 줄.
    pub(crate) fn key_row_at(&self, p: (f64, f64)) -> Option<usize> {
        self.key_menu.as_ref()?.iter().find(|(r, _)| contains(*r, p)).map(|(_, i)| *i)
    }

    pub(crate) fn remote_windows(&self) -> bool {
        self.shared.windows.load(Ordering::Relaxed)
    }

    /// 이 기기 키보드로 못 보내는 키 — (메뉴 글, 키, 손가락). 손가락은 그 기기 것 그대로다(⌘→Ctrl 바꾸기를 안 거친다).
    pub(crate) fn special_keys(&self) -> &'static [(&'static str, &'static str, u8)] {
        if self.remote_windows() {
            &[
                ("Ctrl+Alt+Del", "Delete", 2 | 4),
                ("시작 메뉴 (Windows 키)", "MetaLeft", 0),
                ("창 전환 (Alt+Tab)", "Tab", 4),
                ("작업 관리자 (Ctrl+Shift+Esc)", "Escape", 2 | 1),
            ]
        } else {
            &[("Spotlight (⌘ Space)", "Space", 8), ("앱 전환 (⌘ Tab)", "Tab", 8), ("강제 종료 (⌥⌘ Esc)", "Escape", 4 | 8)]
        }
    }

    /// 이 기기 손가락(1 shift·2 control·4 option·8 command)을 그 기기 것으로 — 맥에서 Windows 를 볼 때 ⌘ 는 Ctrl 이다.
    /// 그대로 넘기면 ⌘C 가 Win+C(Copilot)로 들어갔다. 맥의 Ctrl 은 그대로 Ctrl 이다.
    pub(crate) fn remote_mods(&self, mods: u8) -> u8 {
        if cfg!(target_os = "macos") && self.remote_windows() && mods & 8 != 0 {
            (mods & !8) | 2
        } else {
            mods
        }
    }

    pub(crate) fn on_zoom_chip(&self, p: (f64, f64)) -> bool {
        self.layout.and_then(|l| l.zoom_chip).is_some_and(|r| contains(r, p))
    }

    /// 칸 위 자리 → 그 화면 안의 비율(0–1). 확대했으면 보이는 범위 안에서 센다. 끌어 나간 자리는 가장자리로 붙인다.
    fn at(&self, (px, py): (f64, f64)) -> Option<(f64, f64)> {
        let l = self.layout?;
        let (x, y, w, h) = l.image;
        let (u0, v0, u1, v1) = l.uv;
        (w > 0.0 && h > 0.0).then(|| {
            let fx = ((px - f64::from(x)) / f64::from(w)).clamp(0.0, 1.0);
            let fy = ((py - f64::from(y)) / f64::from(h)).clamp(0.0, 1.0);
            (f64::from(u0) + fx * f64::from(u1 - u0), f64::from(v0) + fy * f64::from(v1 - v0))
        })
    }

    /// `p` 아래 장 자리를 그대로 둔 채 배율을 `factor` 배로. 칸 밖이면 가운데를 축으로.
    pub(crate) fn zoom_by(&mut self, p: Option<(f64, f64)>, factor: f32) {
        let Some(l) = self.layout else { return };
        let zoom = (self.zoom * factor).clamp(1.0, super::MAX_ZOOM);
        let center = (f64::from(l.slot.0 + l.slot.2 / 2.0), f64::from(l.slot.1 + l.slot.3 / 2.0));
        let p = p.filter(|p| contains(l.slot, *p)).unwrap_or(center);
        let Some(under) = self.at(p) else { return };
        let (fw, fh) = (f64::from(l.frame.0.max(1)), f64::from(l.frame.1.max(1)));
        let scale = f64::from((l.slot.2 / l.frame.0.max(1) as f32).min(l.slot.3 / l.frame.1.max(1) as f32) * zoom);
        let focus = (under.0 - (p.0 - center.0) / scale / fw, under.1 - (p.1 - center.1) / scale / fh);
        self.zoom = zoom;
        self.focus = super::clamp_focus(l.slot, l.frame, zoom, focus);
    }

    /// 확대한 장을 칸 위 px 만큼 민다(휠·끌기와 같은 쪽 — 내용이 손가락을 따라온다).
    pub(crate) fn pan_by(&mut self, dx: f64, dy: f64) {
        let Some(l) = self.layout else { return };
        let scale = f64::from((l.slot.2 / l.frame.0.max(1) as f32).min(l.slot.3 / l.frame.1.max(1) as f32) * self.zoom);
        let focus = (
            self.focus.0 - dx / scale / f64::from(l.frame.0.max(1)),
            self.focus.1 - dy / scale / f64::from(l.frame.1.max(1)),
        );
        self.focus = super::clamp_focus(l.slot, l.frame, self.zoom, focus);
    }

    pub(crate) fn zoom_reset(&mut self) {
        self.zoom = 1.0;
        self.focus = (0.5, 0.5);
    }

    fn send(&self, event: serde_json::Value) {
        if let Some(w) = &self.watch {
            w.send_input(&event);
        }
    }

    /// 단추 상태 통째(1 왼쪽·2 오른쪽·4 가운데) — 그 기기가 직전 상태와 견줘 누름·뗌·끌기를 가른다.
    pub(crate) fn pointer(&mut self, p: (f64, f64), buttons: u8, mods: u8) {
        let Some((x, y)) = self.at(p) else { return };
        self.buttons = buttons;
        self.send(json!({ "t": "pointer", "x": x, "y": y, "b": buttons, "m": mods }));
    }

    /// 양수 dy 는 위로 굴림(내용이 내려온다) — 그 기기 화면 포인트 단위.
    pub(crate) fn scroll(&self, p: (f64, f64), dx: f64, dy: f64) {
        let Some((x, y)) = self.at(p) else { return };
        self.send(json!({ "t": "scroll", "x": x, "y": y, "dx": dx.round() as i64, "dy": dy.round() as i64 }));
    }

    /// 이름 붙은 키(웹 `KeyboardEvent.code`)를 눌렀다 뗀다.
    pub(crate) fn key(&self, code: &str, mods: u8) {
        self.send(json!({ "t": "key", "k": code, "m": mods }));
    }

    /// 이 기기 입력기가 조합을 끝낸 글 — 그 기기 입력기를 다시 거치지 않게 글자째 넣는다.
    pub(crate) fn text(&self, s: &str) {
        if !s.is_empty() {
            self.send(json!({ "t": "text", "s": s }));
        }
    }

    pub(crate) fn status(&self) -> String {
        self.shared.status.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// 칸 위 작은 표시 — `60fps · 24ms · 직통`.
    pub(crate) fn hud(&self) -> String {
        let s = self.shared.stats.lock().unwrap_or_else(|e| e.into_inner());
        let mut lat: Vec<f64> = s.window.iter().map(|w| w.1).collect();
        lat.sort_by(|a, b| a.total_cmp(b));
        let p50 = lat.get(lat.len() / 2).copied().unwrap_or(0.0);
        let path = if s.path == "kasanet" { "직통" } else { "중계" };
        format!("{}fps · {:.0}ms · {path}", s.window.len(), p50)
    }
}

impl Drop for ScreenView {
    fn drop(&mut self) {
        drop(self.watch.take());
        let s = self.shared.stats.lock().unwrap_or_else(|e| e.into_inner());
        if s.frames > 0 {
            eprintln!(
                "[screen-stream] {} 닫음 — 새 장 {}개, 화면→풀림 평균 {:.1}ms",
                self.label,
                s.frames,
                s.latency_sum / s.frames as f64
            );
        }
    }
}

/// 카사넷 연결 실패 말(`kasanet::connect`)을 칸에 띄울 말로.
fn closed_text(why: &str) -> String {
    if why.contains("직통도 믿는 중계도 아니다") || why.contains("이은 길이 없다") {
        "카사넷 직통을 기다리는 중 — 길이 서면 바로 붙어요".into()
    } else if why.contains("주소를 아직 모른다") {
        "그 기기를 찾는 중".into()
    } else {
        format!("끊겼어요 — {why} · 곧 다시 붙어요")
    }
}

fn set_status(shared: &Shared, wake: &Arc<dyn Fn() + Send + Sync>, text: &str) {
    *shared.status.lock().unwrap_or_else(|e| e.into_inner()) = text.to_string();
    wake();
}

/// 장은 앞 장을 참조해 순서대로만 풀린다. 단방향 스트림마다 도착 순서가 조금 엇갈려 seq 로 다시 줄 세우고, 키프레임
/// 전의 장은 버린다. 풀기에 실패하면 키프레임을 청하고 다음 키프레임까지 기다린다.
fn decode_loop(
    rx: mpsc::Receiver<Incoming>,
    keyframes: Arc<Mutex<Option<Keyframes>>>,
    shared: &Shared,
    wake: &Arc<dyn Fn() + Send + Sync>,
) {
    let mut dec = VideoDecoder::new();
    let mut pending: BTreeMap<u32, Incoming> = BTreeMap::new();
    let mut next: Option<u32> = None;
    let mut last_cap: Option<u64> = None;
    let mut asked_key = false;
    let ask_key = || {
        if let Some(k) = keyframes.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            k.request();
        }
    };
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(f) => {
                pending.insert(f.frame.seq, f);
                while let Ok(f) = rx.try_recv() {
                    pending.insert(f.frame.seq, f);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
        if next.is_none() {
            match pending.iter().find(|(_, f)| f.frame.key).map(|(s, _)| *s) {
                Some(s) => {
                    next = Some(s);
                    pending.retain(|k, _| *k >= s);
                }
                None => {
                    if !asked_key {
                        ask_key();
                        asked_key = true;
                    }
                    pending.clear();
                    continue;
                }
            }
        }
        let mut decoded = false;
        while let Some(seq) = next {
            let Some(mut f) = pending.remove(&seq) else { break };
            next = Some(seq.wrapping_add(1));
            match dec.decode(&mut f.frame) {
                Ok(Some(picture)) => {
                    *shared.latest.lock().unwrap_or_else(|e| e.into_inner()) = Some(picture);
                    decoded = true;
                }
                Ok(None) => {}
                Err(e) => {
                    eprintln!("[screen-stream] 풀기 실패 seq={seq}: {e:#} — 키프레임을 청한다");
                    next = None;
                    asked_key = true;
                    ask_key();
                    break;
                }
            }
            asked_key = false;
            let t_done = screen_stream::now();
            if last_cap != Some(f.frame.cap_id) {
                last_cap = Some(f.frame.cap_id);
                let latency = ((t_done + f.offset) - f.frame.t_cap) * 1000.0;
                let mut s = shared.stats.lock().unwrap_or_else(|e| e.into_inner());
                s.window.push_back((t_done, latency, f.frame.data.len()));
                s.frames += 1;
                s.latency_sum += latency;
            }
        }
        if decoded {
            shared.serial.fetch_add(1, Ordering::Release);
            let now = screen_stream::now();
            let mut s = shared.stats.lock().unwrap_or_else(|e| e.into_inner());
            while s.window.front().is_some_and(|w| now - w.0 > 1.0) {
                s.window.pop_front();
            }
            drop(s);
            wake();
        }
    }
}
