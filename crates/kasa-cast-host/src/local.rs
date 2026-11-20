//! 같은 기기의 화면 서비스에서 화면을 받아 쓰는 길 — 맥 앱이 폰 웹소켓(영상·JPEG)과 옛 보는 쪽을 도우미가 뜬 화면으로 중계한다
//! (`docs/screen-service-split.md` P5). 그러면 앱은 스트림을 위해 화면 기록 권한이 필요 없다. 서비스가 없으면(13 이전·개발 실행)
//! 앱 안에서 뜬다(`Hybrid`).
//!
//! 틀은 도우미 파이프와 같다(`wire`). 한 연결은 한 가지 일만 한다:
//! - 장: `S`(설정 JSON) → `N`(압축 정보 JSON, `id` 포함)·`D`(까닭) → `F`(장)·`H`(1초 숨) …
//! - 요청: `C` 뒤로 요청 하나에 답 하나 — `K`(세션 id, 키프레임)·`R`(id + bps)·`M`(id + 커서 숨김)·`I`(조작 JSON → `i` 까닭 또는 빈 본문)·
//!   `J`(JPEG 묻기 JSON → `j` 지문 u64·가로·세로 u32·JPEG / `n` 같은 화면 / `D` 까닭)·`L`(화면 목록 → JSON)·`P`·`Q`(권한 창)·`W`(깨우기). 답이
//!   필요 없는 것은 빈 `k`.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use kasa_cast::mirror::{Ask, Denied, Display, Input, Shot, Source};
use kasa_cast::{Capture, Config, Frame, Session};
use serde_json::{json, Value};

use crate::wire::{denied_of, input_from, input_json, recv, send};

const BREATH: Duration = Duration::from_secs(1);

pub trait Duplex: Read + Write + Send {}
impl<T: Read + Write + Send> Duplex for T {}

type Connect = Arc<dyn Fn() -> Option<Box<dyn Duplex>> + Send + Sync>;
type Sessions = Arc<Mutex<HashMap<u64, Box<dyn Session>>>>;

// ---------------------------------------------------------------- 서비스 쪽

/// 서비스가 연결 하나를 맡는다(한 연결 한 스레드).
pub fn serve(mut io: impl Read + Write, capture: &Arc<dyn Capture>, source: &Arc<dyn Source>, sessions: &Sessions) {
    let Ok((kind, body)) = recv(&mut io) else { return };
    match kind {
        b'S' => serve_frames(io, &body, capture, sessions),
        b'C' => {
            while let Ok((kind, body)) = recv(&mut io) {
                let (reply, out) = answer(kind, &body, capture, source, sessions);
                if send(&mut io, reply, &out).is_err() {
                    break;
                }
            }
        }
        _ => {}
    }
}

/// 서비스 쪽 세션 표 — 서비스 프로세스에 하나.
pub fn sessions() -> Sessions {
    Arc::default()
}

fn id_of(body: &[u8]) -> u64 {
    u64::from_le_bytes(body.get(..8).and_then(|b| b.try_into().ok()).unwrap_or([0; 8]))
}

fn answer(kind: u8, body: &[u8], capture: &Arc<dyn Capture>, source: &Arc<dyn Source>, sessions: &Sessions) -> (u8, Vec<u8>) {
    let with = |f: &dyn Fn(&dyn Session)| {
        if let Some(s) = sessions.lock().unwrap_or_else(|e| e.into_inner()).get(&id_of(body)) {
            f(s.as_ref());
        }
    };
    match kind {
        b'K' => with(&|s| s.keyframe()),
        b'R' if body.len() == 12 => with(&|s| s.set_bitrate(u32::from_le_bytes(body[8..12].try_into().unwrap_or([0; 4])))),
        b'M' if body.len() == 9 => with(&|s| s.set_cursor_hidden(body[8] != 0)),
        b'I' => {
            let Some((display, input)) = serde_json::from_slice::<Value>(body).ok().as_ref().and_then(input_from) else { return (b'i', b"unavailable".to_vec()) };
            return match capture.input(display, &input) {
                Ok(()) => (b'i', Vec::new()),
                Err(d) => (b'i', d.reason().as_bytes().to_vec()),
            };
        }
        b'J' => {
            let v: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
            let n = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
            let ask = Ask { display: n("display") as u32, max_w: n("max_w") as u32, max_h: n("max_h") as u32, quality: n("quality") as u8, prev: n("prev") };
            return match source.capture(&ask) {
                Ok(Some(shot)) => {
                    let mut out = Vec::with_capacity(16 + shot.jpeg.len());
                    out.extend_from_slice(&shot.hash.to_le_bytes());
                    out.extend_from_slice(&shot.width.to_le_bytes());
                    out.extend_from_slice(&shot.height.to_le_bytes());
                    out.extend_from_slice(&shot.jpeg);
                    (b'j', out)
                }
                Ok(None) => (b'n', Vec::new()),
                Err(d) => (b'D', d.reason().as_bytes().to_vec()),
            };
        }
        b'L' => return (b'l', serde_json::to_vec(&source.displays()).unwrap_or_default()),
        b'P' => capture.request_permission(),
        b'Q' => capture.request_input_permission(),
        b'W' => source.keep_awake(),
        _ => {}
    }
    (b'k', Vec::new())
}

fn serve_frames(mut io: impl Read + Write, body: &[u8], capture: &Arc<dyn Capture>, sessions: &Sessions) {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let config = serde_json::from_slice::<Value>(body).map(|v| Config::from_json(&v)).unwrap_or_default();
    let (tx, rx) = mpsc::channel::<Frame>();
    let session = match capture.start(&config, Box::new(move |f| drop(tx.send(f)))) {
        Ok(s) => s,
        Err(d) => {
            let _ = send(&mut io, b'D', d.reason().as_bytes());
            return;
        }
    };
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut info = session.info();
    info["id"] = json!(id);
    info["display_id"] = json!(session.display());
    if send(&mut io, b'N', info.to_string().as_bytes()).is_err() {
        return;
    }
    sessions.lock().unwrap_or_else(|e| e.into_inner()).insert(id, session);
    loop {
        let sent = match rx.recv_timeout(BREATH) {
            Ok(frame) => send(&mut io, b'F', &frame.encode()),
            Err(mpsc::RecvTimeoutError::Timeout) => send(&mut io, b'H', &[]),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if sent.is_err() {
            break;
        }
    }
    let gone = sessions.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
    drop(gone);
}

// ---------------------------------------------------------------- 앱 쪽

/// 서비스가 있으면 서비스에서, 없으면 앱 안에서(`fallback`) 뜬다. 화면 스트림(`Capture`)과 JPEG 거울(`Source`) 둘 다.
pub struct Hybrid {
    connect: Connect,
    control: Mutex<Option<Box<dyn Duplex>>>,
    capture: Option<Arc<dyn Capture>>,
    source: Arc<dyn Source>,
}

impl Hybrid {
    pub fn new(connect: impl Fn() -> Option<Box<dyn Duplex>> + Send + Sync + 'static, capture: Option<Arc<dyn Capture>>, source: Arc<dyn Source>) -> Self {
        Self { connect: Arc::new(connect), control: Mutex::new(None), capture, source }
    }

    /// 요청 하나를 보내고 답을 받는다. 서비스가 없으면 None.
    fn ask(&self, kind: u8, body: &[u8]) -> Option<(u8, Vec<u8>)> {
        let mut conn = self.control.lock().unwrap_or_else(|e| e.into_inner());
        for _ in 0..2 {
            if conn.is_none() {
                *conn = (self.connect)().and_then(|mut c| send(&mut c, b'C', &[]).ok().map(|()| c));
            }
            let c = conn.as_mut()?;
            if send(c, kind, body).is_ok() {
                if let Ok(reply) = recv(c) {
                    return Some(reply);
                }
            }
            *conn = None;
        }
        None
    }
}

struct Remote {
    id: u64,
    info: Value,
    display: u32,
    stop: Arc<AtomicBool>,
    hybrid: Arc<Hybrid>,
}

impl Session for Remote {
    fn keyframe(&self) {
        let _ = self.hybrid.ask(b'K', &self.id.to_le_bytes());
    }

    fn display(&self) -> u32 {
        self.display
    }

    fn info(&self) -> Value {
        self.info.clone()
    }

    fn set_cursor_hidden(&self, hidden: bool) {
        let mut body = self.id.to_le_bytes().to_vec();
        body.push(u8::from(hidden));
        let _ = self.hybrid.ask(b'M', &body);
    }

    fn set_bitrate(&self, bps: u32) {
        let mut body = self.id.to_le_bytes().to_vec();
        body.extend_from_slice(&bps.to_le_bytes());
        let _ = self.hybrid.ask(b'R', &body);
    }
}

impl Drop for Remote {
    /// 읽기 스레드가 다음 장·숨에서 멈추고 연결을 닫는다 — 서비스는 쓰기 실패로 세션을 거둔다.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// `Arc` 로 쥐어 세션이 요청 길을 나눠 쓴다.
pub struct HybridCapture(pub Arc<Hybrid>);

impl Capture for HybridCapture {
    fn start(&self, config: &Config, out: Box<dyn Fn(Frame) + Send + Sync>) -> Result<Box<dyn Session>, Denied> {
        let Some(mut io) = (self.0.connect)() else {
            return match &self.0.capture {
                Some(c) => c.start(config, out),
                None => Err(Denied::Unavailable),
            };
        };
        send(&mut io, b'S', config.to_json().to_string().as_bytes()).map_err(|_| Denied::Unavailable)?;
        let (kind, body) = recv(&mut io).map_err(|_| Denied::Unavailable)?;
        match kind {
            b'N' => {
                let mut info: Value = serde_json::from_slice(&body).map_err(|_| Denied::Unavailable)?;
                let id = info.get("id").and_then(Value::as_u64).ok_or(Denied::Unavailable)?;
                let display = info.get("display_id").and_then(Value::as_u64).unwrap_or(0) as u32;
                if let Some(o) = info.as_object_mut() {
                    o.remove("id");
                    o.remove("display_id");
                }
                let stop = Arc::new(AtomicBool::new(false));
                let s = stop.clone();
                std::thread::Builder::new()
                    .name("screen-service-frames".into())
                    .spawn(move || {
                        while !s.load(Ordering::Relaxed) {
                            match recv(&mut io) {
                                Ok((b'F', body)) => {
                                    if let Some(frame) = Frame::decode(&body) {
                                        out(frame);
                                    }
                                }
                                Ok(_) => {}
                                Err(_) => return,
                            }
                        }
                    })
                    .map_err(|_| Denied::Unavailable)?;
                Ok(Box::new(Remote { id, info, display, stop, hybrid: self.0.clone() }))
            }
            // 서비스가 아직 화면 기록 권한이 없으면(새로 깐 도우미) 그 기기 앞에 서비스 권한 창을 한 번 띄우고, 그동안은 앱 안에서
            // 뜬다 — 앱이 권한을 쥐고 있던 기기가 넘어가는 동안 화면이 끊기지 않게.
            b'D' if denied_of(&body) == Denied::Permission => {
                let _ = self.0.ask(b'P', &[]);
                match &self.0.capture {
                    Some(c) => c.start(config, out),
                    None => Err(Denied::Permission),
                }
            }
            b'D' => Err(denied_of(&body)),
            _ => Err(Denied::Unavailable),
        }
    }

    fn request_permission(&self) {
        if self.0.ask(b'P', &[]).is_none() {
            if let Some(c) = &self.0.capture {
                c.request_permission();
            }
        }
    }

    fn input(&self, display: u32, input: &Input) -> Result<(), Denied> {
        match self.0.ask(b'I', input_json(display, input).to_string().as_bytes()) {
            Some((_, why)) if why.is_empty() => Ok(()),
            Some((_, why)) => Err(denied_of(&why)),
            None => self.0.capture.as_ref().map_or(Err(Denied::Unavailable), |c| c.input(display, input)),
        }
    }

    fn request_input_permission(&self) {
        if self.0.ask(b'Q', &[]).is_none() {
            if let Some(c) = &self.0.capture {
                c.request_input_permission();
            }
        }
    }

    fn now(&self) -> f64 {
        crate::host_now()
    }
}

/// JPEG 거울도 같은 길로.
pub struct HybridSource(pub Arc<Hybrid>);

impl Source for HybridSource {
    fn displays(&self) -> Vec<Display> {
        match self.0.ask(b'L', &[]) {
            Some((b'l', body)) => serde_json::from_slice::<Vec<Value>>(&body)
                .unwrap_or_default()
                .iter()
                .filter_map(|d| {
                    Some(Display {
                        id: d.get("id")?.as_u64()? as u32,
                        width: d.get("width")?.as_u64()? as u32,
                        height: d.get("height")?.as_u64()? as u32,
                        main: d.get("main")?.as_bool()?,
                    })
                })
                .collect(),
            _ => self.0.source.displays(),
        }
    }

    fn capture(&self, ask: &Ask) -> Result<Option<Shot>, Denied> {
        let body = json!({ "display": ask.display, "max_w": ask.max_w, "max_h": ask.max_h, "quality": ask.quality, "prev": ask.prev });
        match self.0.ask(b'J', body.to_string().as_bytes()) {
            Some((b'j', b)) if b.len() >= 16 => Ok(Some(Shot {
                hash: u64::from_le_bytes(b[0..8].try_into().unwrap_or([0; 8])),
                width: u32::from_le_bytes(b[8..12].try_into().unwrap_or([0; 4])),
                height: u32::from_le_bytes(b[12..16].try_into().unwrap_or([0; 4])),
                jpeg: b[16..].to_vec(),
            })),
            Some((b'n', _)) => Ok(None),
            Some((b'D', why)) if denied_of(&why) == Denied::Permission => self.0.source.capture(ask),
            Some((b'D', why)) => Err(denied_of(&why)),
            Some(_) => Err(Denied::Unavailable),
            None => self.0.source.capture(ask),
        }
    }

    fn request_permission(&self) {
        if self.0.ask(b'P', &[]).is_none() {
            self.0.source.request_permission();
        }
    }

    fn input(&self, display: u32, input: &Input) -> Result<(), Denied> {
        HybridCapture(self.0.clone()).input(display, input)
    }

    fn request_input_permission(&self) {
        HybridCapture(self.0.clone()).request_input_permission();
    }

    fn keep_awake(&self) {
        if self.0.ask(b'W', &[]).is_none() {
            self.0.source.keep_awake();
        }
    }
}

/// 시스템 소리 — 서비스가 있으면 앱은 뜨지 않는다(서비스로 붙는 보는 쪽은 서비스가 소리를 보내고, 앱으로 중계되는 옛 보는 쪽은
/// 소리 없이 본다). 서비스가 없으면 앱 안에서 뜬다.
pub struct HybridAudio(pub Arc<Hybrid>, pub Arc<dyn kasa_cast::audio::Source>);

impl kasa_cast::audio::Source for HybridAudio {
    fn start(&self, out: Box<dyn FnMut(f64, &[i16]) + Send>) -> Result<Box<dyn Send>, String> {
        if (self.0.connect)().is_some() {
            return Err("화면 서비스가 소리를 보낸다".into());
        }
        self.1.start(out)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::net::{UnixListener, UnixStream};

    struct Still;

    impl Source for Still {
        fn displays(&self) -> Vec<Display> {
            vec![Display { id: 9, width: 10, height: 20, main: true }]
        }
        fn capture(&self, ask: &Ask) -> Result<Option<Shot>, Denied> {
            if ask.prev == 7 {
                return Ok(None);
            }
            Ok(Some(Shot { hash: 7, jpeg: vec![1, 2, 3], width: 4, height: 5 }))
        }
        fn input(&self, _: u32, _: &Input) -> Result<(), Denied> {
            Err(Denied::Permission)
        }
    }

    struct Frames(Arc<AtomicU64>);

    struct Run(Arc<AtomicBool>, Arc<AtomicU64>);

    impl Session for Run {
        fn keyframe(&self) {
            self.1.fetch_add(100, Ordering::Relaxed);
        }
        fn display(&self) -> u32 {
            9
        }
        fn info(&self) -> Value {
            json!({ "width": 64 })
        }
    }

    impl Drop for Run {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }

    impl Capture for Frames {
        fn start(&self, _: &Config, out: Box<dyn Fn(Frame) + Send + Sync>) -> Result<Box<dyn Session>, Denied> {
            let stop = Arc::new(AtomicBool::new(false));
            let s = stop.clone();
            std::thread::spawn(move || {
                let mut seq = 0;
                while !s.load(Ordering::Relaxed) {
                    seq += 1;
                    out(Frame { seq, key: seq == 1, width: 64, height: 32, data: vec![1; 10], ..Frame::default() });
                    std::thread::sleep(Duration::from_millis(5));
                }
            });
            Ok(Box::new(Run(stop, self.0.clone())))
        }
        fn input(&self, _: u32, _: &Input) -> Result<(), Denied> {
            Err(Denied::Permission)
        }
        fn now(&self) -> f64 {
            0.0
        }
    }

    #[test]
    fn the_app_relays_frames_and_stills_from_the_service() {
        let dir = std::env::temp_dir().join(format!("kasa-local-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("local.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let keys = Arc::new(AtomicU64::new(0));
        let capture: Arc<dyn Capture> = Arc::new(Frames(keys.clone()));
        let source: Arc<dyn Source> = Arc::new(Still);
        let table = sessions();
        std::thread::spawn(move || {
            for s in listener.incoming().flatten() {
                let (c, src, t) = (capture.clone(), source.clone(), table.clone());
                std::thread::spawn(move || serve(s, &c, &src, &t));
            }
        });
        let p = path.clone();
        let hybrid = Arc::new(Hybrid::new(move || UnixStream::connect(&p).ok().map(|s| Box::new(s) as Box<dyn Duplex>), None, Arc::new(Still)));
        let (tx, rx) = mpsc::channel();
        let session = HybridCapture(hybrid.clone()).start(&Config::default(), Box::new(move |f| drop(tx.send(f)))).unwrap();
        assert_eq!((session.display(), session.info()["width"].as_u64()), (9, Some(64)));
        let first = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(first.key && first.seq == 1, "장이 차례대로 온다");
        session.keyframe();
        assert_eq!(keys.load(Ordering::Relaxed), 100, "키프레임 청이 서비스 세션에 닿는다");
        assert_eq!(HybridCapture(hybrid.clone()).input(9, &Input::Text("a".into())), Err(Denied::Permission), "까닭이 돌아온다");
        let src = HybridSource(hybrid);
        assert_eq!(src.displays()[0].id, 9);
        let shot = src.capture(&Ask { display: 9, max_w: 1, max_h: 1, quality: 50, prev: 0 }).unwrap().unwrap();
        assert_eq!((shot.hash, shot.width, shot.height, shot.jpeg), (7, 4, 5, vec![1, 2, 3]));
        assert!(src.capture(&Ask { display: 9, max_w: 1, max_h: 1, quality: 50, prev: 7 }).unwrap().is_none(), "같은 화면이면 None");
        drop(session);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn without_a_service_it_falls_back_to_the_app() {
        let hybrid = Arc::new(Hybrid::new(|| None, None, Arc::new(Still)));
        assert_eq!(HybridSource(hybrid.clone()).displays()[0].id, 9);
        assert!(HybridCapture(hybrid).start(&Config::default(), Box::new(|_| {})).is_err(), "앱 안 보내기가 없으면 못 보낸다");
    }
}
