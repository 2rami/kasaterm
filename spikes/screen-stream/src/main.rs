//! S0 측정기 — ScreenCaptureKit + VideoToolbox 를 카사넷(iroh QUIC) 위에 올려 화면 전송의 지연과 선명도를 잰다.
//! 결과와 읽는 법은 `docs/screen-stream.md`.
//!
//!   screen-stream id --key PATH
//!   screen-stream serve --allow ID[,ID…] [--relay URL]
//!   screen-stream view <ADDR> [--key PATH] [--secs N] [--codec hevc|h264] [--mbps N] [--fps N]
//!                     [--pix bgra|420v] [--scale 0.1-1] [--no-lowlat] [--no-realtime] [--flush] [--quality-first] [--refine N] [--scroll PX] [--dump DIR]
//!                     [--crop X,Y,W,H] [--mode direct|relay] [--relay URL]
//!
//! 화면을 보내는 쪽(serve)은 화면 기록 권한이 있는 앱(카사텀 칸) 안에서 돌린다. 설정은 보는 쪽이 붙을 때 정해
//! 보내므로 serve 는 한 번 띄워 두고 view 만 바꿔 가며 잰다. serve 는 `--allow` 로 준 기기만 받는다 — 화면에는
//! 메일·비밀번호 창까지 보인다.

mod capture;
mod codec;
mod wire;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use iroh::endpoint::{presets, Connection, SendStream};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayConfig, RelayMap, RelayMode, RelayUrl, SecretKey, TransportAddr};
use kasa_net::{identity, AllowList};
use objc2_core_foundation::CFRetained;
use objc2_core_media::CMClock;
use objc2_core_video::{kCVPixelFormatType_32BGRA, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, CVPixelBuffer};
use serde_json::{json, Value};
use tokio::sync::Mutex as AsyncMutex;

use codec::{Codec, Decoder, Encoded, Encoder, EncoderConfig, FrameMeta};
use wire::{ctl, read_msg, write_msg, FrameHead, ALPN};

/// 호스트 시계(초). SCK 프레임 시각과 같은 시계다.
pub fn host_now() -> f64 {
    unsafe { CMClock::host_time_clock().time().seconds() }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |name: &str| args.iter().any(|a| a == name);
    let opt = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    match args.first().map(String::as_str) {
        Some("id") => {
            let path = opt("--key").context("--key PATH")?;
            println!("{}", identity::load_or_create(path.as_ref())?.public());
            Ok(())
        }
        Some("serve") => {
            let allow = opt("--allow").context("--allow ID[,ID…]")?.split(',').map(EndpointId::from_str).collect::<Result<Vec<_>, _>>()?;
            serve(allow, relay_map(opt("--relay"))?).await
        }
        Some("view") => {
            let addr = parse_addr(args.get(1).context("view <ADDR>")?)?;
            let codec = Codec::parse(opt("--codec").as_deref().unwrap_or("hevc"))?;
            let config = json!({
                "codec": if codec == Codec::Hevc { "hevc" } else { "h264" },
                "mbps": opt("--mbps").map(|s| s.parse::<f64>()).transpose()?.unwrap_or(30.0),
                "fps": opt("--fps").map(|s| s.parse::<u32>()).transpose()?.unwrap_or(60),
                "pix": opt("--pix").unwrap_or_else(|| "bgra".into()),
                "lowlat": !has("--no-lowlat"),
                "speed": !has("--quality-first"),
                "scale": opt("--scale").map(|s| s.parse::<f64>()).transpose()?.unwrap_or(1.0),
                "realtime": !has("--no-realtime"),
                "flush": has("--flush"),
                "scroll": opt("--scroll").map(|s| s.parse::<u32>()).transpose()?.unwrap_or(0),
                "refine": opt("--refine").map(|s| s.parse::<u32>()).transpose()?.unwrap_or(0),
            });
            let crop = opt("--crop").map(|s| parse_crop(&s)).transpose()?;
            let view = View {
                secs: opt("--secs").map(|s| s.parse()).transpose()?.unwrap_or(15),
                dump: opt("--dump").map(PathBuf::from),
                crop,
                relay_only: opt("--mode").as_deref() == Some("relay"),
            };
            let key = match opt("--key") {
                Some(p) => identity::load_or_create(p.as_ref())?,
                None => SecretKey::generate(),
            };
            view_run(addr, key, codec, config, view, relay_map(opt("--relay"))?).await
        }
        _ => bail!("id --key PATH | serve --allow ID | view <ADDR> …"),
    }
}

fn relay_map(url: Option<String>) -> Result<Option<RelayMap>> {
    let Some(url) = url else { return Ok(None) };
    let cfg = RelayConfig::from(RelayUrl::from_str(&url)?);
    let map = RelayMap::empty();
    map.insert(cfg.url.clone(), cfg.into());
    Ok(Some(map))
}

fn format_addr(a: &EndpointAddr) -> String {
    let relay = a.relay_urls().next().map(|u| u.to_string()).unwrap_or_else(|| "-".into());
    let ips: Vec<String> = a.ip_addrs().map(|s| s.to_string()).collect();
    format!("{};{};{}", a.id, relay, ips.join(","))
}

fn parse_addr(s: &str) -> Result<EndpointAddr> {
    let mut it = s.split(';');
    let mut addr = EndpointAddr::new(EndpointId::from_str(it.next().context("id")?)?);
    if let Some(r) = it.next().filter(|r| *r != "-" && !r.is_empty()) {
        addr = addr.with_relay_url(RelayUrl::from_str(r)?);
    }
    for ip in it.next().unwrap_or("").split(',').filter(|s| !s.is_empty()) {
        addr = addr.with_ip_addr(ip.parse()?);
    }
    Ok(addr)
}

fn parse_crop(s: &str) -> Result<[u32; 4]> {
    let v: Vec<u32> = s.split(',').map(str::parse).collect::<Result<_, _>>()?;
    v.try_into().map_err(|_| anyhow::anyhow!("--crop X,Y,W,H"))
}

// ---------------------------------------------------------------- 보내는 쪽

async fn serve(allow: Vec<EndpointId>, relay: Option<RelayMap>) -> Result<()> {
    let mut b = kasa_net::builder(presets::N0, SecretKey::generate(), &AllowList::new(allow));
    if let Some(map) = relay {
        b = b.relay_mode(RelayMode::Custom(map));
    }
    let ep = b.alpns(vec![ALPN.to_vec()]).bind().await?;
    if tokio::time::timeout(Duration::from_secs(10), ep.online()).await.is_err() {
        eprintln!("중계에 못 붙음(10초) — 직통 주소로만 받는다");
    }
    println!("SCREEN_ADDR {}", format_addr(&ep.addr()));
    while let Some(inc) = ep.accept().await {
        let Ok(accepting) = inc.accept() else { continue };
        let Ok(conn) = accepting.await else { continue };
        eprintln!("[serve] 연결 {}", conn.remote_id().fmt_short());
        if let Err(e) = host_session(conn).await {
            eprintln!("[serve] 세션 끝: {e:#}");
        }
    }
    Ok(())
}

struct SendPx(CFRetained<CVPixelBuffer>);
unsafe impl Send for SendPx {}
unsafe impl Sync for SendPx {}

struct Host {
    encoder: Mutex<Option<Encoder>>,
    force_key: AtomicBool,
    seq: AtomicU32,
    cap_id: AtomicU64,
    /// 마지막으로 보낸 장 — 정지 화면 다듬기와 원본 한 장 보내기에 쓴다.
    last: Mutex<Option<(SendPx, u64, f64)>>,
    /// SCK 가 마지막으로 준 장. 스크롤 시험은 이것을 밀어 만든다.
    base: Mutex<Option<SendPx>>,
    scroll: u32,
    /// 원본 한 장을 청하면 스크롤을 멈춰 같은 장을 견줄 수 있게 한다.
    paused: AtomicBool,
    fps: u32,
    /// 화면이 멈춘 뒤 같은 장을 몇 번 더 보낼지(남은 수).
    refine_left: AtomicU32,
    refine: u32,
    last_encode: Mutex<Instant>,
    stop: AtomicBool,
}

impl Host {
    fn submit(&self, px: &CVPixelBuffer, cap_id: u64, t_cap: f64, t_cb: f64) {
        let guard = self.encoder.lock().unwrap();
        let Some(enc) = guard.as_ref() else { return };
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let force = self.force_key.swap(false, Ordering::Relaxed);
        enc.encode(px, FrameMeta { seq, cap_id, t_cap, t_cb }, force, host_now());
        *self.last_encode.lock().unwrap() = Instant::now();
    }
}

async fn host_session(conn: Connection) -> Result<()> {
    let (mut send, mut recv) = conn.accept_bi().await?;
    // 보는 쪽은 시계부터 맞추고 설정을 보낸다 — 장이 흐르기 전에 시계가 서 있어야 첫 장들의 전송 시간이 맞다.
    let body = loop {
        let (kind, body) = read_msg(&mut recv).await?;
        match kind {
            ctl::CONFIG => break body,
            ctl::PING => {
                let (t0, _) = wire::read_f64_pair(&body)?;
                write_msg(&mut send, ctl::PONG, &wire::f64_pair(t0, host_now())).await?;
            }
            _ => {}
        }
    };
    let cfg: Value = serde_json::from_slice(&body)?;
    eprintln!("[serve] 설정 {cfg}");
    let codec = Codec::parse(cfg["codec"].as_str().unwrap_or("hevc"))?;
    let fps = cfg["fps"].as_u64().unwrap_or(60) as u32;
    let pix = match cfg["pix"].as_str() {
        Some("420v") => kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
        _ => kCVPixelFormatType_32BGRA,
    };
    let refine = cfg["refine"].as_u64().unwrap_or(0) as u32;
    let scale_by = cfg["scale"].as_f64().unwrap_or(1.0).clamp(0.1, 1.0);
    let host = Arc::new(Host {
        encoder: Mutex::new(None),
        force_key: AtomicBool::new(true),
        seq: AtomicU32::new(0),
        cap_id: AtomicU64::new(0),
        last: Mutex::new(None),
        base: Mutex::new(None),
        scroll: cfg["scroll"].as_u64().unwrap_or(0) as u32,
        paused: AtomicBool::new(false),
        fps,
        refine_left: AtomicU32::new(refine.max(1)),
        refine,
        last_encode: Mutex::new(Instant::now()),
        stop: AtomicBool::new(false),
    });

    // 화면 뜨기 — SCK 객체는 이 스레드가 쥐고 있다가 멈춘다.
    let (size_tx, size_rx) = mpsc::channel();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let h = host.clone();
    let cap_thread = std::thread::spawn(move || {
        let sink_host = h.clone();
        let cap = capture::Capture::start(fps, pix, scale_by, move |f| {
            *sink_host.base.lock().unwrap() = Some(SendPx(f.pixels.clone()));
            if sink_host.scroll > 0 {
                return;
            }
            let cap_id = sink_host.cap_id.fetch_add(1, Ordering::Relaxed) + 1;
            sink_host.submit(&f.pixels, cap_id, f.t_cap, f.t_cb);
            *sink_host.last.lock().unwrap() = Some((SendPx(f.pixels), cap_id, f.t_cap));
            sink_host.refine_left.store(sink_host.refine, Ordering::Relaxed);
        });
        match cap {
            Ok(cap) => {
                let _ = size_tx.send(Ok((cap.width, cap.height)));
                let _ = stop_rx.recv();
                drop(cap);
            }
            Err(e) => {
                let _ = size_tx.send(Err(e));
            }
        }
    });
    let (width, height) = tokio::task::spawn_blocking(move || size_rx.recv_timeout(Duration::from_secs(15)))
        .await?
        .context("화면 캡처가 답하지 않는다")??;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Encoded>();
    let enc_cfg = EncoderConfig {
        codec,
        width,
        height,
        bitrate: (cfg["mbps"].as_f64().unwrap_or(30.0) * 1_000_000.0) as i64,
        fps,
        low_latency: cfg["lowlat"].as_bool().unwrap_or(true),
        speed_over_quality: cfg["speed"].as_bool().unwrap_or(true),
        realtime: cfg["realtime"].as_bool().unwrap_or(true),
        flush: cfg["flush"].as_bool().unwrap_or(false),
    };
    let encoder = Encoder::new(&enc_cfg, move |e| {
        let _ = tx.send(e);
    })?;
    let info = json!({
        "width": width, "height": height,
        "codec": cfg["codec"], "hardware": encoder.hardware,
        "rejected": encoder.rejected, "pix": cfg["pix"], "fps": fps,
    });
    eprintln!("[serve] 인코더 {info}");
    *host.encoder.lock().unwrap() = Some(encoder);
    write_msg(&mut send, ctl::INFO, info.to_string().as_bytes()).await?;

    // 화면이 멈춰 SCK 가 장을 안 줄 때 같은 장을 더 보낸다(--refine). 첫 장은 늘 한 번은 보낸다.
    let h = host.clone();
    let mut scroller = if host.scroll > 0 { Some(codec::Scroller::new(width, height)?) } else { None };
    let ticker = std::thread::spawn(move || {
        let period = Duration::from_secs_f64(1.0 / f64::from(fps));
        let mut due = Instant::now();
        while !h.stop.load(Ordering::Relaxed) {
            if let Some(sc) = scroller.as_mut() {
                // 정해진 박자로 — 「지난 장에서 한 장 간격이 지났나」로 보면 잠든 만큼 밀려 60fps 가 42fps 가 된다.
                due += period;
                match due.checked_duration_since(Instant::now()) {
                    Some(wait) => std::thread::sleep(wait),
                    None => due = Instant::now(),
                }
                if h.paused.load(Ordering::Relaxed) {
                    // 멈춘 뒤에는 같은 장을 --refine 만큼 더 보낸다(SCK 화면이 멈췄을 때와 같은 다듬기).
                    if h.refine_left.load(Ordering::Relaxed) > 0 {
                        let last = h.last.lock().unwrap().as_ref().map(|(px, id, t)| (SendPx(px.0.clone()), *id, *t));
                        if let Some((px, cap_id, t_cap)) = last {
                            h.refine_left.fetch_sub(1, Ordering::Relaxed);
                            h.submit(&px.0, cap_id, t_cap, host_now());
                        }
                    }
                    continue;
                }
                let base = h.base.lock().unwrap().as_ref().map(|b| SendPx(b.0.clone()));
                let Some(base) = base else { continue };
                let t = host_now();
                let Some(px) = sc.next(&base.0, h.scroll as usize) else { continue };
                let cap_id = h.cap_id.fetch_add(1, Ordering::Relaxed) + 1;
                h.submit(&px, cap_id, t, host_now());
                *h.last.lock().unwrap() = Some((SendPx(px), cap_id, t));
                continue;
            }
            std::thread::sleep(period / 2);
            if h.last_encode.lock().unwrap().elapsed() < period || h.refine_left.load(Ordering::Relaxed) == 0 {
                continue;
            }
            let last = h.last.lock().unwrap().as_ref().map(|(px, id, t)| (SendPx(px.0.clone()), *id, *t));
            if let Some((px, cap_id, t_cap)) = last {
                h.refine_left.fetch_sub(1, Ordering::Relaxed);
                let now = host_now();
                h.submit(&px.0, cap_id, t_cap.max(now), now);
            }
        }
    });

    // 압축된 장을 장마다 단방향 스트림으로. 앞 장이 먼저 가도록 옛 장일수록 우선순위를 높인다.
    let sender_conn = conn.clone();
    let sender = tokio::spawn(async move {
        while let Some(e) = rx.recv().await {
            let head = FrameHead {
                seq: e.seq,
                cap_id: e.cap_id,
                key: e.key,
                t_cap: e.t_cap,
                t_cb: e.t_cb,
                t_enc: e.t_enc,
                width: width as u16,
                height: height as u16,
                param_sets: e.param_sets,
            };
            let mut buf = Vec::with_capacity(e.data.len() + 128);
            head.encode(&mut buf);
            buf.extend_from_slice(&e.data);
            let Ok(mut s) = sender_conn.open_uni().await else { break };
            let _ = s.set_priority(-(e.seq as i32));
            if s.write_all(&buf).await.is_err() {
                break;
            }
            let _ = s.finish();
        }
    });

    let result = host_control(&conn, &host, &mut send, &mut recv).await;
    host.stop.store(true, Ordering::Relaxed);
    let _ = stop_tx.send(());
    let _ = ticker.join();
    let _ = cap_thread.join();
    *host.encoder.lock().unwrap() = None;
    sender.abort();
    result
}

async fn host_control(
    conn: &Connection,
    host: &Arc<Host>,
    send: &mut SendStream,
    recv: &mut iroh::endpoint::RecvStream,
) -> Result<()> {
    loop {
        let (kind, body) = read_msg(recv).await?;
        match kind {
            ctl::PING => {
                let (t0, _) = wire::read_f64_pair(&body)?;
                write_msg(send, ctl::PONG, &wire::f64_pair(t0, host_now())).await?;
            }
            ctl::KEYFRAME => host.force_key.store(true, Ordering::Relaxed),
            ctl::DUMP => {
                host.refine_left.store(host.refine, Ordering::Relaxed);
                host.paused.store(true, Ordering::Relaxed);
                // 다듬기 장이 다 나갈 때까지 기다렸다가 원본을 보낸다.
                let fps = f64::from(host.fps);
                tokio::time::sleep(Duration::from_secs_f64(f64::from(host.refine) / fps + 0.15)).await;
                let last = host.last.lock().unwrap().as_ref().map(|(px, id, _)| (SendPx(px.0.clone()), *id));
                let Some((px, cap_id)) = last else { continue };
                let png = tokio::task::spawn_blocking(move || {
                    let px = px;
                    png_of(&px.0)
                }).await??;
                let mut s = conn.open_uni().await?;
                let mut buf = vec![b'S'];
                buf.extend_from_slice(&cap_id.to_be_bytes());
                buf.extend_from_slice(&png);
                s.write_all(&buf).await?;
                s.finish()?;
                eprintln!("[serve] 원본 cap_id={cap_id} PNG {}KB 보냄", png.len() / 1024);
            }
            ctl::QUIT => return Ok(()),
            _ => {}
        }
    }
}

fn png_of(px: &CVPixelBuffer) -> Result<Vec<u8>> {
    let (rgb, w, h) = codec::bgra_to_rgb(px).context("BGRA 가 아니라 원본을 못 뜬다(--pix bgra 로)")?;
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new_with_quality(
        &mut out,
        image::codecs::png::CompressionType::Fast,
        image::codecs::png::FilterType::Adaptive,
    )
    .write_image(&rgb, w, h, image::ExtendedColorType::Rgb8)?;
    Ok(out)
}

use image::ImageEncoder;

// ---------------------------------------------------------------- 보는 쪽

struct View {
    secs: u64,
    dump: Option<PathBuf>,
    crop: Option<[u32; 4]>,
    relay_only: bool,
}

/// 보내는 쪽 시계 = 이쪽 시계 + offset. 왕복이 가장 짧았던 핑으로 정한다.
#[derive(Default)]
struct Clock {
    offset: f64,
    best_rtt: f64,
}

impl Clock {
    fn sample(&mut self, t0: f64, th: f64, t2: f64) {
        let rtt = t2 - t0;
        if self.best_rtt == 0.0 || rtt < self.best_rtt {
            self.best_rtt = rtt;
            self.offset = th - (t0 + t2) / 2.0;
        }
    }
}

#[derive(Default)]
struct Stats {
    fresh: Vec<Sample>,
    frames: u64,
    repeats: u64,
    keys: u64,
    bytes: u64,
    errors: u64,
}

#[derive(Clone, Copy)]
struct Sample {
    cap_cb: f64,
    cb_enc: f64,
    net: f64,
    dec: f64,
    total: f64,
    bytes: usize,
    key: bool,
    at: f64,
}

/// 풀린 RGB 한 장(가로·세로).
type Rgb = (Vec<u8>, u32, u32);
/// 원본 cap_id 와 같은 장을 풀린 것 중에서 달라는 청.
type DumpAsk = (u64, mpsc::Sender<Option<Rgb>>);

struct Incoming {
    head: FrameHead,
    data: Vec<u8>,
    t_recv: f64,
}

async fn view_run(
    target: EndpointAddr,
    key: SecretKey,
    codec: Codec,
    config: Value,
    view: View,
    relay: Option<RelayMap>,
) -> Result<()> {
    let mut b = Endpoint::builder(presets::N0).secret_key(key);
    if let Some(map) = relay {
        b = b.relay_mode(RelayMode::Custom(map));
    }
    let target = if view.relay_only {
        b = b.clear_ip_transports();
        EndpointAddr::from_parts(target.id, target.relay_urls().cloned().map(TransportAddr::Relay))
    } else {
        target
    };
    let ep = b.bind().await?;
    let _ = tokio::time::timeout(Duration::from_secs(10), ep.online()).await;
    let conn = tokio::time::timeout(Duration::from_secs(20), ep.connect(target, ALPN)).await.context("연결 시간 초과")??;
    if !view.relay_only {
        let wait = Instant::now();
        while wait.elapsed() < Duration::from_secs(5) && !path(&conn).0 {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    let (direct, remote, rtt) = path(&conn);
    println!("길 {} {remote} QUIC 왕복 {:.1}ms", if direct { "직통" } else { "중계" }, rtt * 1000.0);

    let (send, mut recv) = conn.open_bi().await?;
    let send = Arc::new(AsyncMutex::new(send));

    // 시계 맞추기 — 처음에 30번, 그 뒤 0.5초마다.
    let clock = Arc::new(Mutex::new(Clock::default()));
    for _ in 0..30 {
        write_msg(&mut *send.lock().await, ctl::PING, &wire::f64_pair(host_now(), 0.0)).await?;
        loop {
            let (kind, body) = read_msg(&mut recv).await?;
            if kind == ctl::PONG {
                let (t0, th) = wire::read_f64_pair(&body)?;
                clock.lock().unwrap().sample(t0, th, host_now());
                break;
            }
            if kind == ctl::INFO {
                println!("인코더 {}", String::from_utf8_lossy(&body));
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    {
        let c = clock.lock().unwrap();
        println!("시계 차이 {:+.3}ms (핑 왕복 최소 {:.2}ms)", c.offset * 1000.0, c.best_rtt * 1000.0);
    }
    write_msg(&mut *send.lock().await, ctl::CONFIG, config.to_string().as_bytes()).await?;
    println!("설정 {config}");
    let c2 = clock.clone();
    let ctl_reader = tokio::spawn(async move {
        while let Ok((kind, body)) = read_msg(&mut recv).await {
            match kind {
                ctl::PONG => {
                    if let Ok((t0, th)) = wire::read_f64_pair(&body) {
                        c2.lock().unwrap().sample(t0, th, host_now());
                    }
                }
                ctl::INFO => println!("인코더 {}", String::from_utf8_lossy(&body)),
                _ => {}
            }
        }
    });

    // 풀기 — 순서대로 한 장씩. 이쪽 스레드에서 VideoToolbox 를 동기로 부른다.
    let stats = Arc::new(Mutex::new(Stats::default()));
    let (frame_tx, frame_rx) = mpsc::channel::<Incoming>();
    let (dump_tx, dump_rx) = mpsc::channel::<DumpAsk>();
    let (want_key_tx, mut want_key_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let st = stats.clone();
    let ck = clock.clone();
    let decoder_thread = std::thread::spawn(move || decode_loop(codec, frame_rx, dump_rx, want_key_tx, st, ck));

    let (src_tx, src_rx) = tokio::sync::oneshot::channel::<(u64, Vec<u8>)>();
    let uni_conn = conn.clone();
    let uni = tokio::spawn(async move {
        let mut src_tx = Some(src_tx);
        while let Ok(mut s) = uni_conn.accept_uni().await {
            let Ok(buf) = s.read_to_end(256 << 20).await else { continue };
            let t_recv = host_now();
            match buf.first() {
                Some(b'F') => match FrameHead::decode(&buf[1..]) {
                    Ok((head, at)) => {
                        let data = buf[1 + at..].to_vec();
                        if frame_tx.send(Incoming { head, data, t_recv }).is_err() {
                            break;
                        }
                    }
                    Err(e) => eprintln!("프레임 머리: {e:#}"),
                },
                Some(b'S') if buf.len() > 9 => {
                    let cap_id = u64::from_be_bytes(buf[1..9].try_into().unwrap());
                    if let Some(tx) = src_tx.take() {
                        let _ = tx.send((cap_id, buf[9..].to_vec()));
                    }
                }
                _ => {}
            }
        }
    });

    let started = Instant::now();
    let mut last_print = Instant::now();
    let mut last_ping = Instant::now();
    let mut window_from = 0usize;
    let mut frames_at = (0u64, 0u64);
    while started.elapsed() < Duration::from_secs(view.secs) {
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
            Some(()) = want_key_rx.recv() => {
                write_msg(&mut *send.lock().await, ctl::KEYFRAME, &[]).await?;
            }
        }
        if last_ping.elapsed() >= Duration::from_millis(500) {
            write_msg(&mut *send.lock().await, ctl::PING, &wire::f64_pair(host_now(), 0.0)).await?;
            last_ping = Instant::now();
        }
        if last_print.elapsed() >= Duration::from_secs(2) {
            let s = stats.lock().unwrap();
            let secs = last_print.elapsed().as_secs_f64();
            let window = &s.fresh[window_from.min(s.fresh.len())..];
            let fps = (s.frames - frames_at.0) as f64 / secs;
            let mbps = (s.bytes - frames_at.1) as f64 * 8.0 / secs / 1e6;
            println!("[{:>4.1}s] {}", started.elapsed().as_secs_f64(), line(window, fps, mbps, (conn.paths().iter().find(|p| p.is_selected()).map(|p| p.rtt().as_secs_f64() * 1000.0)).unwrap_or(0.0)));
            window_from = s.fresh.len();
            frames_at = (s.frames, s.bytes);
            last_print = Instant::now();
        }
    }

    {
        let s = stats.lock().unwrap();
        let secs = started.elapsed().as_secs_f64();
        println!("\n== 전체 {:.1}초", secs);
        println!(
            "장 {} (새 장 {} · 다시 보낸 장 {}) · 키프레임 {} · 오류 {} · 평균 {:.1}fps · {:.1}Mbps",
            s.frames,
            s.fresh.len(),
            s.repeats,
            s.keys,
            s.errors,
            s.frames as f64 / secs,
            s.bytes as f64 * 8.0 / secs / 1e6
        );
        summary(&s.fresh);
    }

    if let Some(dir) = &view.dump {
        // 정지 화면 선명도 — 원본 마지막 장과 풀린 같은 장을 견준다.
        std::fs::create_dir_all(dir)?;
        write_msg(&mut *send.lock().await, ctl::DUMP, &[]).await?;
        match tokio::time::timeout(Duration::from_secs(20), src_rx).await {
            Ok(Ok((cap_id, png))) => {
                tokio::time::sleep(Duration::from_millis(300)).await;
                let (tx, rx) = mpsc::channel();
                let _ = dump_tx.send((cap_id, tx));
                let decoded = tokio::task::spawn_blocking(move || rx.recv_timeout(Duration::from_secs(5))).await?;
                compare(dir, &png, decoded.ok().flatten(), cap_id, view.crop)?;
            }
            _ => println!("원본 한 장을 못 받았다"),
        }
    }

    let _ = write_msg(&mut *send.lock().await, ctl::QUIT, &[]).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    conn.close(0u32.into(), b"done");
    uni.abort();
    ctl_reader.abort();
    drop(dump_tx);
    let _ = decoder_thread.join();
    ep.close().await;
    Ok(())
}

fn path(conn: &Connection) -> (bool, String, f64) {
    let paths = conn.paths();
    match paths.iter().find(|p| p.is_selected()) {
        Some(p) => (p.is_ip(), p.remote_addr().to_string(), p.rtt().as_secs_f64()),
        None => (false, "-".into(), 0.0),
    }
}

fn decode_loop(
    codec: Codec,
    frames: mpsc::Receiver<Incoming>,
    dumps: mpsc::Receiver<DumpAsk>,
    want_key: tokio::sync::mpsc::UnboundedSender<()>,
    stats: Arc<Mutex<Stats>>,
    clock: Arc<Mutex<Clock>>,
) {
    let mut dec = Decoder::new(codec);
    let mut pending: BTreeMap<u32, Incoming> = BTreeMap::new();
    let mut next: Option<u32> = None;
    let mut last_cap: Option<u64> = None;
    let mut asked_key = false;
    // 원본 한 장을 청한 사이에 새 장이 올 수 있어 최근 몇 장을 쥐고 있는다(같은 장이면 가장 다듬어진 것).
    let mut recent: std::collections::VecDeque<(u64, SendPx)> = std::collections::VecDeque::new();
    loop {
        match frames.recv_timeout(Duration::from_millis(20)) {
            Ok(f) => {
                pending.insert(f.head.seq, f);
                // 쌓인 것을 다 꺼내야 한다 — 한 바퀴에 한 장만 꺼내면 풀기가 받는 속도를 못 따라가는 것처럼 보인다.
                while let Ok(f) = frames.try_recv() {
                    pending.insert(f.head.seq, f);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if let Ok((cap_id, reply)) = dumps.try_recv() {
            let out = recent.iter().rev().find(|(id, _)| *id == cap_id).and_then(|(_, px)| codec::bgra_to_rgb(&px.0));
            if out.is_none() {
                println!("풀린 장 중에 원본 cap_id={cap_id} 가 없다(마지막 {last_cap:?})");
            }
            let _ = reply.send(out);
        }
        if next.is_none() {
            // 키프레임부터 시작한다. 그 앞의 장은 풀 수 없다.
            match pending.iter().find(|(_, f)| f.head.key).map(|(s, _)| *s) {
                Some(s) => {
                    next = Some(s);
                    pending.retain(|k, _| *k >= s);
                }
                None => {
                    if !pending.is_empty() && !asked_key {
                        let _ = want_key.send(());
                        asked_key = true;
                    }
                    continue;
                }
            }
        }
        while let Some(seq) = next {
            let Some(mut f) = pending.remove(&seq) else { break };
            next = Some(seq + 1);
            let r = dec.decode(&f.head.param_sets, &mut f.data, f.head.t_enc);
            let t_done = host_now();
            let mut s = stats.lock().unwrap();
            s.frames += 1;
            s.bytes += f.data.len() as u64;
            s.keys += u64::from(f.head.key);
            if let Err(e) = r {
                s.errors += 1;
                eprintln!("풀기 실패 seq={seq}: {e:#}");
                next = None;
                asked_key = false;
                continue;
            }
            if let Some(px) = dec.last() {
                recent.retain(|(id, _)| *id != f.head.cap_id);
                recent.push_back((f.head.cap_id, SendPx(px)));
                if recent.len() > 6 {
                    recent.pop_front();
                }
            }
            if last_cap == Some(f.head.cap_id) {
                s.repeats += 1;
                continue;
            }
            last_cap = Some(f.head.cap_id);
            let off = clock.lock().unwrap().offset;
            s.fresh.push(Sample {
                cap_cb: f.head.t_cb - f.head.t_cap,
                cb_enc: f.head.t_enc - f.head.t_cb,
                net: (f.t_recv + off) - f.head.t_enc,
                dec: t_done - f.t_recv,
                total: (t_done + off) - f.head.t_cap,
                bytes: f.data.len(),
                key: f.head.key,
                at: t_done,
            });
        }
    }
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() - 1) as f64 * p).round() as usize]
}

fn line(w: &[Sample], fps: f64, mbps: f64, rtt_ms: f64) -> String {
    let mut total: Vec<f64> = w.iter().map(|s| s.total * 1000.0).collect();
    let mut enc: Vec<f64> = w.iter().map(|s| s.cb_enc * 1000.0).collect();
    let mut net: Vec<f64> = w.iter().map(|s| s.net * 1000.0).collect();
    let mut dec: Vec<f64> = w.iter().map(|s| s.dec * 1000.0).collect();
    format!(
        "{fps:>5.1}fps {mbps:>6.1}Mbps 새장 {:>3} | 총 p50 {:>5.1} p95 {:>5.1}ms | 압축 {:>4.1} 전송 {:>5.1} 풀기 {:>4.1}ms | 왕복 {rtt_ms:.1}ms",
        w.len(),
        pct(&mut total, 0.5),
        pct(&mut total, 0.95),
        pct(&mut enc, 0.5),
        pct(&mut net, 0.5),
        pct(&mut dec, 0.5),
    )
}

fn summary(all: &[Sample]) {
    let Some(first) = all.first().map(|s| s.at) else { return };
    // 첫 1초는 세션 만들기·첫 키프레임이라 따로 본다.
    let w: Vec<Sample> = all.iter().copied().filter(|s| s.at - first >= 1.0 && !s.key).collect();
    for k in all.iter().filter(|s| s.key) {
        println!("키프레임 {:.0}KB 화면→풀림 {:.1}ms (압축 {:.1} 전송 {:.1} 풀기 {:.1})", k.bytes as f64 / 1024.0, k.total * 1000.0, k.cb_enc * 1000.0, k.net * 1000.0, k.dec * 1000.0);
    }
    println!("첫 1초·키프레임 뺀 새 장 {}개", w.len());
    let w = &w[..];
    let col = |f: fn(&Sample) -> f64| -> Vec<f64> { w.iter().map(|s| f(s) * 1000.0).collect() };
    let rows: [(&str, Vec<f64>); 5] = [
        ("화면→SCK", col(|s| s.cap_cb)),
        ("SCK→압축 끝", col(|s| s.cb_enc)),
        ("압축 끝→받음", col(|s| s.net)),
        ("받음→풀림", col(|s| s.dec)),
        ("화면→풀림(합)", col(|s| s.total)),
    ];
    println!("{:<14} {:>7} {:>7} {:>7} {:>7}", "구간(ms)", "p50", "p90", "p99", "최대");
    for (name, mut v) in rows {
        println!(
            "{:<14} {:>7.1} {:>7.1} {:>7.1} {:>7.1}",
            name,
            pct(&mut v, 0.5),
            pct(&mut v, 0.9),
            pct(&mut v, 0.99),
            pct(&mut v, 1.0)
        );
    }
    let mut sizes: Vec<f64> = w.iter().map(|s| s.bytes as f64 / 1024.0).collect();
    println!("새 장 크기 KB p50 {:.0} p90 {:.0} 최대 {:.0}", pct(&mut sizes, 0.5), pct(&mut sizes, 0.9), pct(&mut sizes, 1.0));
}

fn compare(dir: &std::path::Path, src_png: &[u8], decoded: Option<Rgb>, cap_id: u64, crop: Option<[u32; 4]>) -> Result<()> {
    let src = image::load_from_memory(src_png)?.to_rgb8();
    src.save(dir.join("src.png"))?;
    let Some((rgb, w, h)) = decoded else {
        println!("풀린 같은 장이 없어 견주지 못했다");
        return Ok(());
    };
    let dec = image::RgbImage::from_raw(w, h, rgb).context("풀린 장 크기")?;
    dec.save(dir.join("dec.png"))?;
    if src.dimensions() != dec.dimensions() {
        println!("크기가 다르다 원본 {:?} 풀림 {:?}", src.dimensions(), dec.dimensions());
        return Ok(());
    }
    let [x, y, cw, ch] = crop.unwrap_or_else(|| {
        let (cw, ch) = (w.min(1200), h.min(700));
        [(w - cw) / 2, (h - ch) / 2, cw, ch]
    });
    image::imageops::crop_imm(&src, x, y, cw, ch).to_image().save(dir.join("src_crop.png"))?;
    image::imageops::crop_imm(&dec, x, y, cw, ch).to_image().save(dir.join("dec_crop.png"))?;
    println!(
        "정지 화면 cap_id={cap_id} PSNR 전체 {:.2}dB · 잘라 본 곳 {:.2}dB · 그림 {}",
        psnr(&src, &dec, [0, 0, w, h]),
        psnr(&src, &dec, [x, y, cw, ch]),
        dir.display()
    );
    Ok(())
}

/// 밝기(Y, BT.709) PSNR — 색 공간 차이보다 글자 윤곽의 뭉개짐을 보려고 밝기만 견준다.
fn psnr(a: &image::RgbImage, b: &image::RgbImage, [x, y, w, h]: [u32; 4]) -> f64 {
    let luma = |p: &image::Rgb<u8>| 0.2126 * f64::from(p[0]) + 0.7152 * f64::from(p[1]) + 0.0722 * f64::from(p[2]);
    let mut se = 0.0;
    for yy in y..y + h {
        for xx in x..x + w {
            let d = luma(a.get_pixel(xx, yy)) - luma(b.get_pixel(xx, yy));
            se += d * d;
        }
    }
    let mse = se / f64::from(w * h);
    if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0 * 255.0 / mse).log10()
    }
}
