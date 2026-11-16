//! 화면 스트림 — 하드웨어 영상 압축 장을 카사넷 QUIC(`kasa/screen/1`)으로 보낸다. 계약·실측은 `docs/screen-stream.md`.
//!
//! 화면 뜨기·압축·풀기는 OS 마다 달라 앱이 꽂는다(`Capture`). 이 모듈은 틀과 흐름만 쥔다.
//! - 제어(보는 쪽이 여는 양방향 스트림 하나): `[u8 종류][u32 길이 BE][내용]`.
//! - 장(장마다 단방향 스트림): `'F'` + `FrameHead` + 압축 자료. 옛 장일수록 우선순위가 높다 — 장마다 앞 장을 참조해
//!   순서대로 풀어야 한다.
//!
//! 시각은 보내는 쪽 호스트 시계(`Capture::now`)로 싣는다. 보는 쪽은 설정을 보내기 전에 핑으로 시계 차이를 잡는다 —
//! 장이 흐르는 중에 맞추면 그동안 쌓인 첫 장들의 전송 시간이 부풀려진다.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use kasa_net::iroh::endpoint::{Connection, RecvStream, SendStream};
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::screen_mirror::Denied;

pub use kasa_device::kasanet::SCREEN_ALPN;

pub mod ctl {
    pub const CONFIG: u8 = b'C';
    pub const INFO: u8 = b'I';
    pub const PING: u8 = b'P';
    pub const PONG: u8 = b'p';
    pub const KEYFRAME: u8 = b'K';
    pub const DENIED: u8 = b'X';
    pub const QUIT: u8 = b'Q';
}

/// 압축 장 하나의 상한. 원래 해상도 키프레임도 1MB 를 넘지 않았다(S0) — 넉넉히 잡되 끝은 둔다.
const FRAME_MAX: usize = 32 << 20;
const MSG_MAX: usize = 1 << 20;
/// 보내지 못하고 쌓인 장이 이만큼이면 버리고 키프레임부터 다시 — 느린 길에서 지연이 끝없이 쌓이지 않게.
const BACKLOG_MAX: usize = 8;
const SYNC_PINGS: usize = 20;

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub codec: String,
    pub bitrate: u32,
    pub fps: u32,
    /// 원래 픽셀 크기에 곱할 배율(0.25–1). 1 이 원래 해상도.
    pub scale: f32,
    /// 화면이 멈춘 뒤 같은 장을 몇 번 더 압축해 보낼지 — 움직일 때 무른 마지막 장을 또렷하게 다듬는다.
    pub refine: u32,
}

impl Default for Config {
    /// S0 실측으로 정한 기본값 — HEVC 30Mbps 60fps 원래 해상도, 멈추면 30장(0.5초) 다듬기.
    fn default() -> Self {
        Self { codec: "hevc".into(), bitrate: 30_000_000, fps: 60, scale: 1.0, refine: 30 }
    }
}

impl Config {
    fn to_json(&self) -> Value {
        json!({ "codec": self.codec, "bitrate": self.bitrate, "fps": self.fps, "scale": self.scale, "refine": self.refine })
    }

    /// 범위 밖 값은 끝으로 붙인다 — 보는 쪽이 보낸 값이라 그대로 믿지 않는다.
    fn from_json(v: &Value) -> Self {
        let d = Self::default();
        Self {
            codec: match v.get("codec").and_then(Value::as_str) {
                Some("h264") => "h264".into(),
                _ => d.codec,
            },
            bitrate: v.get("bitrate").and_then(Value::as_u64).map_or(d.bitrate, |b| b.clamp(1_000_000, 200_000_000) as u32),
            fps: v.get("fps").and_then(Value::as_u64).map_or(d.fps, |f| f.clamp(1, 120) as u32),
            scale: v.get("scale").and_then(Value::as_f64).map_or(d.scale, |s| s.clamp(0.25, 1.0) as f32),
            refine: v.get("refine").and_then(Value::as_u64).map_or(d.refine, |r| r.min(240) as u32),
        }
    }
}

/// 압축된 장. 시각은 보내는 쪽 호스트 시계(초).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Frame {
    pub seq: u32,
    /// 화면이 그려진 장의 번호 — 멈춘 뒤 다시 보내는 다듬기 장은 같은 번호다.
    pub cap_id: u64,
    pub key: bool,
    /// 화면에 그려진 시각.
    pub t_cap: f64,
    /// 압축을 끝낸 시각.
    pub t_enc: f64,
    pub width: u32,
    pub height: u32,
    /// 키프레임일 때만 — VPS/SPS/PPS(HEVC) 또는 SPS/PPS(H.264).
    pub param_sets: Vec<Vec<u8>>,
    /// 4바이트 길이 머리를 붙인 NAL 들(AVCC/HVCC).
    pub data: Vec<u8>,
}

impl Frame {
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.data.len() + 64);
        out.push(b'F');
        out.extend_from_slice(&self.seq.to_be_bytes());
        out.extend_from_slice(&self.cap_id.to_be_bytes());
        out.push(u8::from(self.key));
        out.extend_from_slice(&self.t_cap.to_be_bytes());
        out.extend_from_slice(&self.t_enc.to_be_bytes());
        out.extend_from_slice(&self.width.to_be_bytes());
        out.extend_from_slice(&self.height.to_be_bytes());
        out.push(self.param_sets.len() as u8);
        for ps in &self.param_sets {
            out.extend_from_slice(&(ps.len() as u16).to_be_bytes());
            out.extend_from_slice(ps);
        }
        out.extend_from_slice(&self.data);
        out
    }

    fn decode(buf: &[u8]) -> Option<Self> {
        let mut r = Reader { buf, at: 0 };
        if r.take::<1>()? != *b"F" {
            return None;
        }
        let seq = u32::from_be_bytes(r.take()?);
        let cap_id = u64::from_be_bytes(r.take()?);
        let key = r.take::<1>()?[0] != 0;
        let t_cap = f64::from_be_bytes(r.take()?);
        let t_enc = f64::from_be_bytes(r.take()?);
        let width = u32::from_be_bytes(r.take()?);
        let height = u32::from_be_bytes(r.take()?);
        let n = r.take::<1>()?[0] as usize;
        let mut param_sets = Vec::with_capacity(n);
        for _ in 0..n {
            let len = u16::from_be_bytes(r.take()?) as usize;
            param_sets.push(r.slice(len)?.to_vec());
        }
        let data = buf.get(r.at..)?.to_vec();
        Some(Self { seq, cap_id, key, t_cap, t_enc, width, height, param_sets, data })
    }
}

struct Reader<'a> {
    buf: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.slice(N)?.try_into().ok()
    }

    fn slice(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.buf.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(s)
    }
}

/// 앱이 꽂는 화면 뜨기·압축.
pub trait Capture: Send + Sync {
    /// 뜨고 압축하기 시작한다. 장은 `out` 으로 — 압축 일꾼 스레드에서 부른다. 돌려준 세션을 버리면 멈춘다.
    fn start(&self, config: &Config, out: Box<dyn Fn(Frame) + Send + Sync>) -> Result<Box<dyn Session>, Denied>;
    /// 그 기기 앞에 화면 기록 권한 창을 띄운다 — 연결마다 한 번.
    fn request_permission(&self) {}
    /// 장의 시각과 같은 호스트 시계(초).
    fn now(&self) -> f64;
}

pub trait Session: Send {
    /// 다음 장을 키프레임으로.
    fn keyframe(&self);
    /// 보는 쪽에 알리는 압축 정보(크기·코덱·하드웨어 여부 등).
    fn info(&self) -> Value;
}

static CAPTURE: OnceLock<Arc<dyn Capture>> = OnceLock::new();

/// 앱이 부팅 때 한 번 — 카사넷 화면 입구도 함께 연다.
pub fn install(capture: Arc<dyn Capture>) {
    if CAPTURE.set(capture).is_ok() {
        kasa_device::kasanet::set_screen_handler(Arc::new(|conn| Box::pin(serve(conn))));
    }
}

async fn write_msg(send: &mut SendStream, kind: u8, body: &[u8]) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(5 + body.len());
    buf.push(kind);
    buf.extend_from_slice(&(body.len() as u32).to_be_bytes());
    buf.extend_from_slice(body);
    send.write_all(&buf).await.map_err(std::io::Error::other)
}

async fn read_msg(recv: &mut RecvStream) -> std::io::Result<(u8, Vec<u8>)> {
    let mut head = [0u8; 5];
    recv.read_exact(&mut head).await.map_err(std::io::Error::other)?;
    let len = u32::from_be_bytes([head[1], head[2], head[3], head[4]]) as usize;
    if len > MSG_MAX {
        return Err(std::io::Error::other("제어 메시지가 너무 크다"));
    }
    let mut body = vec![0u8; len];
    recv.read_exact(&mut body).await.map_err(std::io::Error::other)?;
    Ok((head[0], body))
}

fn pair(a: f64, b: f64) -> Vec<u8> {
    let mut v = a.to_be_bytes().to_vec();
    v.extend_from_slice(&b.to_be_bytes());
    v
}

fn read_pair(b: &[u8]) -> Option<(f64, f64)> {
    Some((f64::from_be_bytes(b.get(0..8)?.try_into().ok()?), f64::from_be_bytes(b.get(8..16)?.try_into().ok()?)))
}

// ---------------------------------------------------------------- 보내는 쪽

/// 보는 쪽 하나와의 연결. 보는 쪽마다 따로 떠서 설정을 제각각 고른다.
pub async fn serve(conn: Connection) {
    serve_conn(conn, CAPTURE.get().cloned()).await;
}

pub(crate) async fn serve_conn(conn: Connection, capture: Option<Arc<dyn Capture>>) {
    let Ok((mut send, mut recv)) = conn.accept_bi().await else { return };
    match capture {
        Some(capture) => drop(serve_with(&conn, capture, &mut send, &mut recv).await),
        None => drop(write_msg(&mut send, ctl::DENIED, json!({ "reason": "unavailable" }).to_string().as_bytes()).await),
    }
    // 마지막 메시지(거절 까닭)가 닿을 때까지 — 바로 돌아가면 연결이 먼저 닫혀 보는 쪽은 「끊김」만 본다.
    let _ = send.finish();
    let _ = tokio::time::timeout(Duration::from_secs(3), conn.closed()).await;
}

async fn serve_with(
    conn: &Connection,
    capture: Arc<dyn Capture>,
    send: &mut SendStream,
    recv: &mut RecvStream,
) -> std::io::Result<()> {
    let config = loop {
        let (kind, body) = read_msg(recv).await?;
        match kind {
            ctl::PING => {
                let (t0, _) = read_pair(&body).unwrap_or_default();
                write_msg(send, ctl::PONG, &pair(t0, capture.now())).await?;
            }
            ctl::CONFIG => break Config::from_json(&serde_json::from_slice(&body).unwrap_or(Value::Null)),
            ctl::QUIT => return Ok(()),
            _ => {}
        }
    };
    let (tx, rx) = mpsc::unbounded_channel::<Frame>();
    let started = {
        let capture = capture.clone();
        let config = config.clone();
        tokio::task::spawn_blocking(move || capture.start(&config, Box::new(move |f| drop(tx.send(f)))))
            .await
            .map_err(std::io::Error::other)?
    };
    let session = match started {
        Ok(s) => s,
        Err(denied) => {
            if denied == Denied::Permission {
                let c = capture.clone();
                let _ = tokio::task::spawn_blocking(move || c.request_permission()).await;
            }
            let reason = if denied == Denied::Permission { "permission" } else { "unavailable" };
            write_msg(send, ctl::DENIED, json!({ "reason": reason }).to_string().as_bytes()).await?;
            return Ok(());
        }
    };
    write_msg(send, ctl::INFO, session.info().to_string().as_bytes()).await?;
    let session: Arc<Mutex<Box<dyn Session>>> = Arc::new(Mutex::new(session));
    let sender = tokio::spawn(send_frames(conn.clone(), rx, session.clone()));
    let result = loop {
        let (kind, body) = match read_msg(recv).await {
            Ok(m) => m,
            Err(e) => break Err(e),
        };
        match kind {
            ctl::PING => {
                let (t0, _) = read_pair(&body).unwrap_or_default();
                write_msg(send, ctl::PONG, &pair(t0, capture.now())).await?;
            }
            ctl::KEYFRAME => session.lock().unwrap_or_else(|e| e.into_inner()).keyframe(),
            ctl::QUIT => break Ok(()),
            _ => {}
        }
    };
    sender.abort();
    // 세션은 압축 일꾼 스레드에서 멈추게 한다 — 멈추기가 SCK·VT 를 기다린다.
    let _ = tokio::task::spawn_blocking(move || drop(session)).await;
    result
}

/// 장마다 단방향 스트림. 보내지 못한 장이 쌓이면 다 버리고 키프레임부터 다시 — 압축은 앞 장을 참조해 장을 골라
/// 버릴 수 없다.
async fn send_frames(conn: Connection, mut rx: mpsc::UnboundedReceiver<Frame>, session: Arc<Mutex<Box<dyn Session>>>) {
    let mut waiting_key = false;
    while let Some(frame) = rx.recv().await {
        if rx.len() >= BACKLOG_MAX {
            while rx.try_recv().is_ok() {}
            session.lock().unwrap_or_else(|e| e.into_inner()).keyframe();
            waiting_key = true;
            continue;
        }
        if waiting_key && !frame.key {
            continue;
        }
        waiting_key = false;
        let Ok(mut s) = conn.open_uni().await else { return };
        let _ = s.set_priority(-(frame.seq as i32));
        if s.write_all(&frame.encode()).await.is_err() {
            return;
        }
        let _ = s.finish();
    }
}

// ---------------------------------------------------------------- 보는 쪽

/// 보는 쪽 시계(초) — 받은 시각·푼 시각을 같은 시계로 재야 시계 차이가 맞는다.
pub fn now() -> f64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed().as_secs_f64()
}

#[derive(Debug)]
pub enum Event {
    /// 붙었다 — 길 이름(`kasanet`·`kasanet-relay`)과 왕복(초).
    Connected { path: &'static str, rtt: f64 },
    /// 압축 정보(크기·코덱 등).
    Info(Value),
    /// 받은 장. `t_recv` 는 이쪽 시계, `offset` 은 「보내는 쪽 시계 = 이쪽 시계 + offset」.
    Frame { frame: Frame, t_recv: f64, offset: f64 },
    /// 그쪽이 못 보낸다 — `permission`(화면 기록 권한)·`unavailable`(꽂지 않은 판).
    Denied(String),
    Closed(String),
}

/// 보는 연결의 손잡이. 버리면 닫는다.
pub struct Watch {
    ctl: mpsc::UnboundedSender<u8>,
    closed: Arc<AtomicBool>,
}

/// 풀기에 실패한 일꾼이 키프레임을 청하는 손잡이 — `Watch` 와 달리 버려도 연결을 닫지 않는다.
#[derive(Clone)]
pub struct Keyframes(mpsc::UnboundedSender<u8>);

impl Keyframes {
    pub fn request(&self) {
        let _ = self.0.send(ctl::KEYFRAME);
    }
}

impl Watch {
    pub fn keyframes(&self) -> Keyframes {
        Keyframes(self.ctl.clone())
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        let _ = self.ctl.send(ctl::QUIT);
    }
}

/// base 의 기기 화면을 본다. 카사넷 직통·믿는 중계가 아니면 바로 `Closed` 로 끝난다(그때는 JPEG 거울로).
pub fn watch(base: String, config: Config, on: impl Fn(Event) + Send + Sync + 'static) -> Option<Watch> {
    let rt = kasa_device::kasanet::runtime()?;
    let (tx, rx) = mpsc::unbounded_channel();
    let closed = Arc::new(AtomicBool::new(false));
    let flag = closed.clone();
    rt.spawn(async move {
        let on: Arc<dyn Fn(Event) + Send + Sync> = Arc::new(on);
        let why = match kasa_device::kasanet::connect(&base, SCREEN_ALPN).await {
            Ok((conn, path)) => {
                on(Event::Connected { path, rtt: rtt_of(&conn) });
                let why = run_watch(conn.clone(), config, on.clone(), rx).await;
                conn.close(0u32.into(), b"done");
                why
            }
            Err(e) => e,
        };
        flag.store(true, Ordering::Relaxed);
        on(Event::Closed(why));
    });
    Some(Watch { ctl: tx, closed })
}

fn rtt_of(conn: &Connection) -> f64 {
    conn.paths().iter().find(|p| p.is_selected()).map_or(0.0, |p| p.rtt().as_secs_f64())
}

/// 붙은 연결로 본다 — 시험이 카사넷 노드 없이 부르는 자리.
pub async fn run_watch(
    conn: Connection,
    config: Config,
    on: Arc<dyn Fn(Event) + Send + Sync>,
    mut ctl_rx: mpsc::UnboundedReceiver<u8>,
) -> String {
    let (mut send, mut recv) = match conn.open_bi().await {
        Ok(s) => s,
        Err(e) => return format!("제어 스트림: {e}"),
    };
    let clock = Arc::new(Mutex::new(Clock::default()));
    for _ in 0..SYNC_PINGS {
        if let Err(e) = write_msg(&mut send, ctl::PING, &pair(now(), 0.0)).await {
            return format!("핑: {e}");
        }
        match read_msg(&mut recv).await {
            Ok((ctl::PONG, body)) => {
                if let Some((t0, th)) = read_pair(&body) {
                    clock.lock().unwrap_or_else(|e| e.into_inner()).sample(t0, th, now());
                }
            }
            Ok((ctl::DENIED, body)) => return denied(&on, &body),
            Ok(_) => {}
            Err(e) => return format!("시계 맞추기: {e}"),
        }
    }
    if let Err(e) = write_msg(&mut send, ctl::CONFIG, config.to_json().to_string().as_bytes()).await {
        return format!("설정: {e}");
    }
    let frames = {
        let (conn, on, clock) = (conn.clone(), on.clone(), clock.clone());
        tokio::spawn(async move {
            while let Ok(mut s) = conn.accept_uni().await {
                let Ok(buf) = s.read_to_end(FRAME_MAX).await else { continue };
                let t_recv = now();
                if let Some(frame) = Frame::decode(&buf) {
                    let offset = clock.lock().unwrap_or_else(|e| e.into_inner()).offset;
                    on(Event::Frame { frame, t_recv, offset });
                }
            }
        })
    };
    let mut ping = tokio::time::interval(Duration::from_secs(1));
    let why = loop {
        tokio::select! {
            msg = read_msg(&mut recv) => match msg {
                Ok((ctl::PONG, body)) => {
                    if let Some((t0, th)) = read_pair(&body) {
                        clock.lock().unwrap_or_else(|e| e.into_inner()).sample(t0, th, now());
                    }
                }
                Ok((ctl::INFO, body)) => on(Event::Info(serde_json::from_slice(&body).unwrap_or(Value::Null))),
                Ok((ctl::DENIED, body)) => break denied(&on, &body),
                Ok(_) => {}
                Err(e) => break format!("끊김: {e}"),
            },
            c = ctl_rx.recv() => match c {
                Some(ctl::QUIT) | None => {
                    let _ = write_msg(&mut send, ctl::QUIT, &[]).await;
                    break "닫음".into();
                }
                Some(kind) => {
                    if let Err(e) = write_msg(&mut send, kind, &[]).await {
                        break format!("끊김: {e}");
                    }
                }
            },
            _ = ping.tick() => {
                if let Err(e) = write_msg(&mut send, ctl::PING, &pair(now(), 0.0)).await {
                    break format!("끊김: {e}");
                }
            }
        }
    };
    frames.abort();
    why
}

fn denied(on: &Arc<dyn Fn(Event) + Send + Sync>, body: &[u8]) -> String {
    let v: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let reason = v.get("reason").and_then(Value::as_str).unwrap_or("unavailable").to_string();
    on(Event::Denied(reason.clone()));
    format!("거절: {reason}")
}

/// 보내는 쪽 시계 = 이쪽 시계 + offset. 왕복이 가장 짧았던 핑으로 정한다(그 핑의 오차가 가장 작다).
#[derive(Default)]
struct Clock {
    offset: f64,
    best_rtt: f64,
}

impl Clock {
    fn sample(&mut self, t0: f64, th: f64, t2: f64) {
        let rtt = t2 - t0;
        if rtt >= 0.0 && (self.best_rtt == 0.0 || rtt < self.best_rtt) {
            self.best_rtt = rtt;
            self.offset = th - (t0 + t2) / 2.0;
        }
    }
}

#[cfg(test)]
mod tests;
