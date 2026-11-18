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

use crate::screen_mirror::{Denied, Input};

pub use kasa_device::kasanet::SCREEN_ALPN;

pub mod ctl {
    pub const CONFIG: u8 = b'C';
    pub const INFO: u8 = b'I';
    pub const PING: u8 = b'P';
    pub const PONG: u8 = b'p';
    pub const KEYFRAME: u8 = b'K';
    /// 조작 — 내용은 화면 거울과 같은 JSON(`{"t":"pointer",x,y,b,m}` 등, `docs/screen-mirror.md` 「조작」).
    pub const INPUT: u8 = b'U';
    /// 조작을 넣을 수 있는지가 바뀔 때 — `{ok, reason?}`.
    pub const INPUT_STATE: u8 = b'N';
    pub const DENIED: u8 = b'X';
    pub const QUIT: u8 = b'Q';
    /// 보는 쪽이 조작 중인지 — 본문 1바이트(1 = 조작 중). 조작 중이면 보내는 쪽은 커서를 장에서 뺀다: 보는 쪽 자기
    /// 포인터가 곧 커서라 영상 지연만큼 늦게 따라오는 그 기기 커서가 겹쳐 굼떠 보였다(Parsec 처럼).
    pub const CURSOR: u8 = b'M';
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
    /// 보는 쪽이 그릴 수 있는 가장 큰 픽셀 크기(0 = 모름) — 보는 기기의 가장 큰 모니터. macOS 보내는 쪽은 이 안에
    /// 들도록 줄여 뜬다: 그릴 수 없는 픽셀은 압축만 늦춘다(3024×1964 H.264 압축 p90 35ms, 1814×1178 에서 11ms).
    /// Windows 복제는 원래 크기 그대로 보낸다.
    pub max_w: u32,
    pub max_h: u32,
    /// 볼 화면 — 그 기기가 압축 정보 `displays` 로 알린 번호(맥은 CG 화면 번호, Windows 는 모니터 차례). 없으면 주 화면.
    pub display: Option<u32>,
}

impl Default for Config {
    /// S0 실측으로 정한 기본값 — HEVC 30Mbps 60fps 원래 해상도, 멈추면 30장(0.5초) 다듬기.
    fn default() -> Self {
        Self { codec: "hevc".into(), bitrate: 30_000_000, fps: 60, scale: 1.0, refine: 30, max_w: 0, max_h: 0, display: None }
    }
}

impl Config {
    pub fn to_json(&self) -> Value {
        json!({
            "codec": self.codec, "bitrate": self.bitrate, "fps": self.fps, "scale": self.scale, "refine": self.refine,
            "max_w": self.max_w, "max_h": self.max_h, "display": self.display,
        })
    }

    /// 범위 밖 값은 끝으로 붙인다 — 보는 쪽이 보낸 값이라 그대로 믿지 않는다.
    pub fn from_json(v: &Value) -> Self {
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
            max_w: v.get("max_w").and_then(Value::as_u64).map_or(0, |w| if w == 0 { 0 } else { w.clamp(160, 16384) as u32 }),
            max_h: v.get("max_h").and_then(Value::as_u64).map_or(0, |h| if h == 0 { 0 } else { h.clamp(120, 16384) as u32 }),
            display: v.get("display").and_then(Value::as_u64).and_then(|d| u32::try_from(d).ok()),
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
    /// `'F'` 틀 — 카사넷 장·폰 웹소켓 장·Windows 화면 도우미 파이프가 같은 꼴을 쓴다.
    pub fn encode(&self) -> Vec<u8> {
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

    pub fn decode(buf: &[u8]) -> Option<Self> {
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
    /// 조작을 그 화면에 넣는다. macOS 는 손쉬운 사용 권한이 없으면 `Permission`.
    fn input(&self, _display: u32, _input: &Input) -> Result<(), Denied> {
        Err(Denied::Unavailable)
    }
    /// 조작 권한 창을 그 기기 앞에 띄운다 — 연결마다 한 번.
    fn request_input_permission(&self) {}
    /// 장의 시각과 같은 호스트 시계(초).
    fn now(&self) -> f64;
}

pub trait Session: Send {
    /// 다음 장을 키프레임으로.
    fn keyframe(&self);
    /// 뜨는 화면 — 조작을 이 화면에 넣는다(`screen_mirror::Source::input` 의 화면 번호).
    fn display(&self) -> u32;
    /// 보는 쪽에 알리는 압축 정보(크기·코덱·하드웨어 여부 등).
    fn info(&self) -> Value;
    /// 커서를 장에서 뺀다(보는 쪽이 조작 중). 커서를 따로 안 뜨는 판(Windows 복제)은 할 일이 없다.
    fn set_cursor_hidden(&self, _hidden: bool) {}
    /// 압축률을 바꾼다(bps) — 길이 막히면 내리고 풀리면 올린다(`Rate`). 못 바꾸는 판은 그대로 둔다.
    fn set_bitrate(&self, _bps: u32) {}
}

static CAPTURE: OnceLock<Arc<dyn Capture>> = OnceLock::new();

/// 이 판이 화면을 압축해 보낼 수 있나 — 화면 거울 소켓의 영상 방식을 받을지 가른다.
pub fn installed() -> bool {
    CAPTURE.get().is_some()
}

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
    let display = session.display();
    let session: Arc<Mutex<Box<dyn Session>>> = Arc::new(Mutex::new(session));
    let sender = tokio::spawn(send_frames(conn.clone(), rx, session.clone(), config.bitrate));
    let mut input = Inputs::default();
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
            ctl::CURSOR => {
                if let Some(&hidden) = body.first() {
                    session.lock().unwrap_or_else(|e| e.into_inner()).set_cursor_hidden(hidden != 0);
                }
            }
            ctl::INPUT => {
                let Some(event) = std::str::from_utf8(&body).ok().and_then(crate::screen_mirror::input_of) else { continue };
                if let Some(told) = input.put(&capture, display, event).await {
                    write_msg(send, ctl::INPUT_STATE, told.to_string().as_bytes()).await?;
                }
            }
            ctl::QUIT => break Ok(()),
            _ => {}
        }
    };
    input.release(&capture, display).await;
    sender.abort();
    // 세션은 압축 일꾼 스레드에서 멈추게 한다 — 멈추기가 SCK·VT 를 기다린다.
    let _ = tokio::task::spawn_blocking(move || drop(session)).await;
    result
}

/// 연결이 쥔 조작 상태 — 화면 거울과 같은 규칙(`docs/screen-mirror.md` 「조작」).
#[derive(Default)]
struct Inputs {
    /// 지금 눌린 단추(1 왼쪽·2 오른쪽·4 가운데). 다음 누름과 견줘 누름·뗌·끌기를 가른다.
    buttons: u8,
    last_at: (f64, f64),
    /// 마지막으로 알린 사정. 바뀔 때만 다시 알린다. 화면 거울 소켓은 같은 말에 `t:"input"` 을 붙인다.
    told: Option<Option<Denied>>,
    asked: bool,
}

impl Inputs {
    /// 받은 차례대로 하나씩 넣고 끝날 때까지 기다린다 — 섞이면 누름과 뗌이 뒤바뀐다. 사정이 바뀌면 알릴 말을 돌려준다.
    async fn put(&mut self, capture: &Arc<dyn Capture>, display: u32, mut event: Input) -> Option<Value> {
        if let Input::Pointer { x, y, buttons, prev, .. } = &mut event {
            *prev = self.buttons;
            self.buttons = *buttons;
            self.last_at = (*x, *y);
        }
        let c = capture.clone();
        let got = tokio::task::spawn_blocking(move || c.input(display, &event)).await.unwrap_or(Err(Denied::Unavailable));
        let now = got.err();
        if now == Some(Denied::Permission) && !self.asked {
            self.asked = true;
            let c = capture.clone();
            let _ = tokio::task::spawn_blocking(move || c.request_input_permission()).await;
        }
        if self.told == Some(now) {
            return None;
        }
        self.told = Some(now);
        Some(match now {
            None => json!({ "ok": true }),
            Some(d) => json!({ "ok": false, "reason": if d == Denied::Permission { "permission" } else { "unavailable" } }),
        })
    }

    /// 끊길 때 눌린 단추를 그 자리에서 뗀다 — 그 기기에 단추가 눌린 채 남으면 그 앞의 사람이 마우스를 못 쓴다.
    async fn release(&mut self, capture: &Arc<dyn Capture>, display: u32) {
        if self.buttons == 0 || self.told != Some(None) {
            return;
        }
        let up = Input::Pointer { x: self.last_at.0, y: self.last_at.1, buttons: 0, prev: self.buttons, mods: 0 };
        let c = capture.clone();
        let _ = tokio::task::spawn_blocking(move || c.input(display, &up)).await;
        self.buttons = 0;
    }
}

/// 화면 거울 소켓의 영상 방식(`/screen/ws?codec=hevc`, `docs/screen-stream.md` S2) — 폰이 카사넷 직통일 때 JPEG 대신 받는다.
/// 첫 말은 화면 거울과 같은 `hello` 에 `codec`·장 크기를 더한 것, 장은 바이너리 `'F'` 틀(카사넷 장과 같다). 보는 쪽은
/// `{"t":"keyframe"}` 과 화면 거울 조작 JSON 을 보낸다. 사정은 화면 거울처럼 `{"t":"denied"}`·`{"t":"input"}`.
pub async fn serve_ws(socket: axum::extract::ws::WebSocket, config: Config) {
    serve_ws_with(socket, CAPTURE.get().cloned(), config).await;
}

pub(crate) async fn serve_ws_with(socket: axum::extract::ws::WebSocket, capture: Option<Arc<dyn Capture>>, config: Config) {
    use axum::extract::ws::Message;
    use futures_util::{SinkExt, StreamExt};
    let text = |v: Value| Message::Text(v.to_string().into());
    let (mut tx, mut rx) = socket.split();
    let Some(capture) = capture else {
        let _ = tx.send(text(json!({ "t": "denied", "reason": "unavailable" }))).await;
        let _ = tx.close().await;
        return;
    };
    let (frames_tx, mut frames) = mpsc::unbounded_channel::<Frame>();
    let started = {
        let (capture, config) = (capture.clone(), config.clone());
        tokio::task::spawn_blocking(move || capture.start(&config, Box::new(move |f| drop(frames_tx.send(f))))).await
    };
    let session = match started {
        Ok(Ok(s)) => s,
        Ok(Err(denied)) => {
            if denied == Denied::Permission {
                let c = capture.clone();
                let _ = tokio::task::spawn_blocking(move || c.request_permission()).await;
            }
            let reason = if denied == Denied::Permission { "permission" } else { "unavailable" };
            let _ = tx.send(text(json!({ "t": "denied", "reason": reason }))).await;
            let _ = tx.close().await;
            return;
        }
        Err(_) => return,
    };
    let info = session.info();
    let display = session.display();
    let (w, h) = (info.get("width").cloned().unwrap_or(Value::Null), info.get("height").cloned().unwrap_or(Value::Null));
    let hello = json!({
        "t": "hello", "codec": config.codec, "info": info, "display": display,
        "displays": [{ "id": display, "width": w, "height": h, "main": true }],
    });
    if tx.send(text(hello)).await.is_err() {
        return;
    }
    let mut input = Inputs::default();
    let mut waiting_key = false;
    let mut ping = tokio::time::interval_at(tokio::time::Instant::now() + Duration::from_secs(20), Duration::from_secs(20));
    loop {
        tokio::select! {
            frame = frames.recv() => {
                let Some(frame) = frame else { break };
                // 보내지 못한 장이 쌓이면 다 버리고 키프레임부터 — 카사넷 장과 같은 규칙.
                if frames.len() >= BACKLOG_MAX {
                    while frames.try_recv().is_ok() {}
                    session.keyframe();
                    waiting_key = true;
                    continue;
                }
                if waiting_key && !frame.key {
                    continue;
                }
                waiting_key = false;
                if tx.send(Message::Binary(frame.encode().into())).await.is_err() {
                    break;
                }
            }
            msg = rx.next() => {
                let raw = match msg {
                    Some(Ok(Message::Text(t))) => t,
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    _ => continue,
                };
                if serde_json::from_str::<Value>(raw.as_str()).ok().and_then(|v| v.get("t")?.as_str().map(str::to_string)).as_deref() == Some("keyframe") {
                    session.keyframe();
                    continue;
                }
                let Some(event) = crate::screen_mirror::input_of(raw.as_str()) else { continue };
                if let Some(mut told) = input.put(&capture, display, event).await {
                    told["t"] = json!("input");
                    if tx.send(text(told)).await.is_err() {
                        break;
                    }
                }
            }
            _ = ping.tick() => {
                if tx.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
        }
    }
    input.release(&capture, display).await;
    let _ = tokio::task::spawn_blocking(move || drop(session)).await;
    let _ = tx.close().await;
}

/// 장마다 단방향 스트림. 보내지 못한 장이 쌓이면 다 버리고 키프레임부터 다시 — 압축은 앞 장을 참조해 장을 골라
/// 버릴 수 없다.
async fn send_frames(conn: Connection, mut rx: mpsc::UnboundedReceiver<Frame>, session: Arc<Mutex<Box<dyn Session>>>, bitrate: u32) {
    let mut waiting_key = false;
    let mut rate = Rate::new(bitrate);
    while let Some(frame) = rx.recv().await {
        let queued = rx.len();
        if let Some(bps) = rate.observe(queued, Instant::now()) {
            session.lock().unwrap_or_else(|e| e.into_inner()).set_bitrate(bps);
        }
        if queued >= BACKLOG_MAX {
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

/// 자동 압축률 — 보내지 못한 장이 쌓이면(길이 압축률보다 좁다) 30% 내리고, 3초 조용하면 15%씩 처음 값까지 올린다.
/// 망이 나빠도 장을 버리고 키프레임부터 다시 하는 일(`BACKLOG_MAX`)이 드물어진다 — Parsec 의 자동 비트레이트처럼.
pub(crate) struct Rate {
    max: u32,
    now: u32,
    last_change: Instant,
}

impl Rate {
    /// 쌓였다고 볼 줄 길이 — 한두 장은 큰 키프레임 하나로도 잠깐 선다.
    const CROWDED: usize = 3;
    const FLOOR: u32 = 2_000_000;
    const DOWN: f64 = 0.7;
    const UP: f64 = 1.15;
    /// 내린 뒤 다음 판정까지 — 내린 압축률이 실제 장에 닿을 시간.
    const SETTLE: Duration = Duration::from_millis(700);
    const CALM: Duration = Duration::from_secs(3);

    pub(crate) fn new(max: u32) -> Self {
        Self { max, now: max, last_change: Instant::now() }
    }

    /// 장 하나를 꺼낼 때 — 줄에 남은 장 수를 보고 바꿀 압축률을 돌려준다.
    pub(crate) fn observe(&mut self, queued: usize, at: Instant) -> Option<u32> {
        let since = at.saturating_duration_since(self.last_change);
        let next = if queued >= Self::CROWDED && since >= Self::SETTLE {
            ((f64::from(self.now) * Self::DOWN).round() as u32).max(Self::FLOOR.min(self.max))
        } else if queued == 0 && since >= Self::CALM && self.now < self.max {
            ((f64::from(self.now) * Self::UP).round() as u32).min(self.max)
        } else {
            return None;
        };
        if queued > 0 || next != self.now {
            self.last_change = at;
        }
        (next != self.now).then(|| {
            self.now = next;
            next
        })
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
    /// 조작을 넣을 수 있는지가 바뀌었다 — 안 되면 `permission`(손쉬운 사용 권한)·`unavailable`.
    Input { ok: bool, reason: Option<String> },
    Closed(String),
}

/// 보는 연결의 손잡이. 버리면 닫는다.
pub struct Watch {
    ctl: mpsc::UnboundedSender<(u8, Vec<u8>)>,
    closed: Arc<AtomicBool>,
}

/// 풀기에 실패한 일꾼이 키프레임을 청하는 손잡이 — `Watch` 와 달리 버려도 연결을 닫지 않는다.
#[derive(Clone)]
pub struct Keyframes(mpsc::UnboundedSender<(u8, Vec<u8>)>);

impl Keyframes {
    pub fn request(&self) {
        let _ = self.0.send((ctl::KEYFRAME, Vec::new()));
    }
}

impl Watch {
    pub fn keyframes(&self) -> Keyframes {
        Keyframes(self.ctl.clone())
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }

    /// 조작 하나 — 화면 거울과 같은 JSON(`{"t":"pointer",…}`).
    pub fn send_input(&self, event: &Value) {
        let _ = self.ctl.send((ctl::INPUT, event.to_string().into_bytes()));
    }

    /// 조작 중이면 그 기기 커서를 장에서 빼 달라고 한다. 붙기 전에 불러도 붙은 뒤 차례대로 간다.
    pub fn set_cursor_hidden(&self, hidden: bool) {
        let _ = self.ctl.send((ctl::CURSOR, vec![u8::from(hidden)]));
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        let _ = self.ctl.send((ctl::QUIT, Vec::new()));
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
    mut ctl_rx: mpsc::UnboundedReceiver<(u8, Vec<u8>)>,
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
                Ok((ctl::INPUT_STATE, body)) => {
                    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                    let reason = v.get("reason").and_then(Value::as_str).map(str::to_string);
                    on(Event::Input { ok: v.get("ok").and_then(Value::as_bool).unwrap_or(false), reason });
                }
                Ok((ctl::DENIED, body)) => break denied(&on, &body),
                Ok(_) => {}
                Err(e) => break format!("끊김: {e}"),
            },
            c = ctl_rx.recv() => match c {
                Some((ctl::QUIT, _)) | None => {
                    let _ = write_msg(&mut send, ctl::QUIT, &[]).await;
                    break "닫음".into();
                }
                Some((kind, body)) => {
                    if let Err(e) = write_msg(&mut send, kind, &body).await {
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
