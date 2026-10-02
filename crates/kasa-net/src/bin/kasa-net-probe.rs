//! P0 시험 바이너리. 두 기기 사이 직통 성립 여부·성립까지 시간·왕복·처리량을 잰다.
//!
//!   kasa-net-probe serve [--key PATH] [--allow ID] [--for SECS] [--relay URL [--n0]]
//!   kasa-net-probe dial <ADDR> [--mode direct|relay] [--trials N] [--pings N] [--bytes N] [--key PATH] [--relay URL [--n0]]
//!   kasa-net-probe id --key PATH
//!   kasa-net-probe serve-fwd --port P --allow ID [--key PATH] [--for SECS] [--relay URL [--n0]]
//!   kasa-net-probe forward <ADDR> --port P [--mode direct|relay] [--key PATH] [--relay URL [--n0]]
//!
//! serve 가 찍는 `KASANET_ADDR ...` 줄의 값을 dial 에 그대로 넘긴다.
//! relay 모드는 거는 쪽의 IP 전송을 걷어 내 중계 말고는 길이 없게 만든다.
//! `--relay` 는 n0 공용 중계 대신 그 중계 하나만 쓴다(자체 중계 측정). `--n0` 을 더하면 n0 중계도 함께 둔다.
//! `serve-fwd`·`forward` 는 앱과 같은 길(`FwdServer` ← `Link`·`Route`)로 serve 쪽 `127.0.0.1:P` 를 거는 쪽
//! `127.0.0.1:L` 로 끌어온다 — 앱 HTTP·거울을 그 길 위에서 재려고. `--relay` 로 준 중계는 믿는 중계로 친다.

use std::str::FromStr;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use iroh::defaults::prod::default_relay_map;
use iroh::endpoint::Builder;
use iroh::endpoint::{presets, Connection, RecvStream, SendStream};
use iroh::protocol::Router;
use iroh::{
    Endpoint, EndpointAddr, EndpointId, RelayConfig, RelayMap, RelayMode, RelayUrl, SecretKey,
    TransportAddr,
};
use kasa_net::{fwd, identity, AllowList, FwdServer, Link, RelayTrust, Route};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::timeout;

const ALPN: &[u8] = b"kasa/probe/1";
const CHUNK: usize = 64 * 1024;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first() else {
        bail!("serve | dial <ADDR> | id --key PATH")
    };
    let has = |name: &str| args.iter().any(|a| a == name);
    let opt = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    match cmd.as_str() {
        "id" => {
            let path = opt("--key").context("--key PATH")?;
            println!("{}", identity::load_or_create(path.as_ref())?.public());
            Ok(())
        }
        "serve" => {
            let secs = opt("--for").map(|s| s.parse()).transpose()?.unwrap_or(900);
            serve(
                key(opt("--key"))?,
                opt("--allow"),
                Duration::from_secs(secs),
                relay_map(opt("--relay"), has("--n0"))?,
            )
            .await
        }
        "serve-fwd" => {
            let port = opt("--port").context("--port P")?.parse()?;
            let allow = EndpointId::from_str(&opt("--allow").context("--allow ID")?)?;
            let secs = opt("--for").map(|s| s.parse()).transpose()?.unwrap_or(900);
            serve_fwd(
                key(opt("--key"))?,
                allow,
                port,
                Duration::from_secs(secs),
                relay_map(opt("--relay"), has("--n0"))?,
            )
            .await
        }
        "forward" => {
            let addr = parse_addr(args.get(1).context("forward <ADDR>")?)?;
            let port = opt("--port").context("--port P")?.parse()?;
            let relay_only = opt("--mode").as_deref() == Some("relay");
            let trust =
                RelayTrust::new(opt("--relay").map(|u| RelayUrl::from_str(&u)).transpose()?);
            forward(
                addr,
                port,
                relay_only,
                key(opt("--key"))?,
                relay_map(opt("--relay"), has("--n0"))?,
                trust,
            )
            .await
        }
        "dial" => {
            let addr = args.get(1).context("dial <ADDR>")?;
            let relay = match opt("--mode").as_deref() {
                None | Some("direct") => false,
                Some("relay") => true,
                Some(m) => bail!("모르는 모드 {m}"),
            };
            let trials = opt("--trials").map(|s| s.parse()).transpose()?.unwrap_or(3);
            let pings = opt("--pings")
                .map(|s| s.parse())
                .transpose()?
                .unwrap_or(200);
            let bytes = opt("--bytes")
                .map(|s| s.parse())
                .transpose()?
                .unwrap_or(20_000_000);
            let own = relay_map(opt("--relay"), has("--n0"))?;
            dial(
                parse_addr(addr)?,
                relay,
                trials,
                pings,
                bytes,
                opt("--key"),
                own,
            )
            .await
        }
        other => bail!("모르는 명령 {other}"),
    }
}

fn key(path: Option<String>) -> Result<SecretKey> {
    Ok(match path {
        Some(p) => identity::load_or_create(p.as_ref())?,
        None => SecretKey::generate(),
    })
}

/// 주어진 중계는 QUIC 주소 찾기(UDP 7842)도 켠 것으로 본다 — 국내 중계가 그렇게 선다(`tools/kasanet-relay`).
fn relay_map(url: Option<String>, n0: bool) -> Result<Option<RelayMap>> {
    let Some(url) = url else { return Ok(None) };
    let cfg = RelayConfig::from(RelayUrl::from_str(&url)?);
    let map = if n0 {
        default_relay_map()
    } else {
        RelayMap::empty()
    };
    map.insert(cfg.url.clone(), cfg.into());
    Ok(Some(map))
}

fn with_relay(b: Builder, relay: Option<RelayMap>) -> Builder {
    match relay {
        Some(map) => b.relay_mode(RelayMode::Custom(map)),
        None => b,
    }
}

async fn serve(
    secret: SecretKey,
    allow: Option<String>,
    life: Duration,
    relay: Option<RelayMap>,
) -> Result<()> {
    // 허용 목록을 안 주면 아무나 받는다 — 시험용 에코라 되돌려 주는 것 말고는 하는 일이 없다.
    let b = match allow {
        Some(id) => kasa_net::builder(
            presets::N0,
            secret,
            &AllowList::new([EndpointId::from_str(&id)?]),
        ),
        None => Endpoint::builder(presets::N0).secret_key(secret),
    };
    let ep = with_relay(b, relay)
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await?;
    if timeout(Duration::from_secs(10), ep.online()).await.is_err() {
        eprintln!("중계에 못 붙음(10초)");
    }
    println!("KASANET_ADDR {}", format_addr(&ep.addr()));
    let deadline = tokio::time::sleep(life);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => break,
            inc = ep.accept() => {
                let Some(inc) = inc else { break };
                tokio::spawn(async move {
                    let Ok(accepting) = inc.accept() else { return };
                    let Ok(conn) = accepting.await else { return };
                    eprintln!("연결: {}", conn.remote_id().fmt_short());
                    handle(conn).await;
                });
            }
        }
    }
    ep.close().await;
    Ok(())
}

async fn serve_fwd(
    secret: SecretKey,
    allow: EndpointId,
    port: u16,
    life: Duration,
    relay: Option<RelayMap>,
) -> Result<()> {
    let b = kasa_net::builder(presets::N0, secret, &AllowList::new([allow]));
    let ep = with_relay(b, relay).bind().await?;
    if timeout(Duration::from_secs(10), ep.online()).await.is_err() {
        eprintln!("중계에 못 붙음(10초)");
    }
    println!("KASANET_ADDR {}", format_addr(&ep.addr()));
    let router = Router::builder(ep)
        .accept(fwd::ALPN, FwdServer::new([port]))
        .spawn();
    tokio::time::sleep(life).await;
    router.shutdown().await?;
    Ok(())
}

async fn forward(
    peer: EndpointAddr,
    port: u16,
    relay_only: bool,
    secret: SecretKey,
    relay: Option<RelayMap>,
    trust: RelayTrust,
) -> Result<()> {
    let mut b = with_relay(Endpoint::builder(presets::N0).secret_key(secret), relay);
    let peer = if relay_only {
        b = b.clear_ip_transports();
        only_relay(&peer)
    } else {
        peer
    };
    let ep = b.bind().await?;
    let link = Link::start(ep, peer, trust);
    let route = Route::direct_only()?;
    route.set_link(link.clone(), port);
    println!("FORWARD 127.0.0.1:{}", route.local_addr().port());
    loop {
        eprintln!(
            "길 {:?} 실은 연결 {:?} {}",
            link.state(),
            route.carried(),
            link.last_error().unwrap_or_default()
        );
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn handle(conn: Connection) {
    let dg = conn.clone();
    tokio::spawn(async move {
        while let Ok(d) = dg.read_datagram().await {
            let _ = dg.send_datagram(d);
        }
    });
    while let Ok((send, recv)) = conn.accept_bi().await {
        tokio::spawn(async move {
            if let Err(e) = serve_stream(send, recv).await {
                eprintln!("스트림: {e:#}");
            }
        });
    }
}

async fn serve_stream(mut send: SendStream, mut recv: RecvStream) -> Result<()> {
    let mut cmd = [0u8; 1];
    AsyncReadExt::read_exact(&mut recv, &mut cmd).await?;
    match cmd[0] {
        b'P' => {
            let mut buf = [0u8; 8];
            while AsyncReadExt::read_exact(&mut recv, &mut buf).await.is_ok() {
                AsyncWriteExt::write_all(&mut send, &buf).await?;
            }
        }
        b'U' => {
            let n = tokio::io::copy(&mut recv, &mut tokio::io::sink()).await?;
            AsyncWriteExt::write_all(&mut send, &n.to_be_bytes()).await?;
        }
        b'D' => {
            let mut n = [0u8; 8];
            AsyncReadExt::read_exact(&mut recv, &mut n).await?;
            let mut left = u64::from_be_bytes(n) as usize;
            let chunk = vec![0x5au8; CHUNK];
            while left > 0 {
                let k = left.min(CHUNK);
                AsyncWriteExt::write_all(&mut send, &chunk[..k]).await?;
                left -= k;
            }
        }
        c => bail!("모르는 명령 {c}"),
    }
    send.finish()?;
    let _ = send.stopped().await;
    Ok(())
}

fn format_addr(a: &EndpointAddr) -> String {
    let relay = a
        .relay_urls()
        .next()
        .map(|u| u.to_string())
        .unwrap_or_else(|| "-".into());
    let ips: Vec<String> = a.ip_addrs().map(|s| s.to_string()).collect();
    format!("{};{};{}", a.id, relay, ips.join(","))
}

fn parse_addr(s: &str) -> Result<EndpointAddr> {
    let mut it = s.split(';');
    let id = EndpointId::from_str(it.next().context("id")?)?;
    let mut addr = EndpointAddr::new(id);
    if let Some(r) = it.next().filter(|r| *r != "-" && !r.is_empty()) {
        addr = addr.with_relay_url(RelayUrl::from_str(r)?);
    }
    for ip in it.next().unwrap_or("").split(',').filter(|s| !s.is_empty()) {
        addr = addr.with_ip_addr(ip.parse()?);
    }
    Ok(addr)
}

fn only_relay(a: &EndpointAddr) -> EndpointAddr {
    EndpointAddr::from_parts(a.id, a.relay_urls().cloned().map(TransportAddr::Relay))
}

#[derive(Debug)]
struct Selected {
    direct: bool,
    remote: String,
    quic_rtt: Duration,
}

fn selected(conn: &Connection) -> Option<Selected> {
    let paths = conn.paths();
    let p = paths.iter().find(|p| p.is_selected())?;
    Some(Selected {
        direct: p.is_ip(),
        remote: p.remote_addr().to_string(),
        quic_rtt: p.rtt(),
    })
}

async fn dial(
    target: EndpointAddr,
    relay: bool,
    trials: usize,
    pings: usize,
    bytes: u64,
    key_path: Option<String>,
    own_relay: Option<RelayMap>,
) -> Result<()> {
    let mode = if relay { "relay" } else { "direct" };
    let target = if relay { only_relay(&target) } else { target };
    println!("모드 {mode}, 상대 {}", format_addr(&target));
    for trial in 1..=trials {
        // 시도마다 새 엔드포인트 — 앞 시도의 길 기억 없이 처음 붙는 시간을 잰다.
        let secret = key(key_path.clone())?;
        let t_bind = Instant::now();
        let mut b = with_relay(
            Endpoint::builder(presets::N0).secret_key(secret),
            own_relay.clone(),
        );
        if relay {
            b = b.clear_ip_transports();
        }
        let ep = b.bind().await?;
        let online = timeout(Duration::from_secs(10), ep.online()).await.is_ok();
        let t_online = t_bind.elapsed();

        let t0 = Instant::now();
        let conn = timeout(Duration::from_secs(20), ep.connect(target.clone(), ALPN))
            .await
            .context("연결 시간 초과")??;
        let t_hs = t0.elapsed();
        let first = selected(&conn);
        let mut t_direct = None;
        if !relay {
            let wait = Instant::now();
            while wait.elapsed() < Duration::from_secs(15) {
                if selected(&conn).is_some_and(|s| s.direct) {
                    t_direct = Some(t0.elapsed());
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
        println!(
            "[{mode} #{trial}] 중계 접속 {} {:.0}ms | 핸드셰이크 {:.0}ms (첫 길 {}) | 직통 {}",
            if online { "ok" } else { "실패" },
            ms(t_online),
            ms(t_hs),
            first.as_ref().map(|s| s.remote.as_str()).unwrap_or("?"),
            match t_direct {
                Some(t) => format!("{:.0}ms", ms(t)),
                None if relay => "-".into(),
                None => "성립 안 함(15초)".into(),
            }
        );

        if trial == 1 {
            measure(&conn, mode, pings, bytes).await?;
        }
        conn.close(0u32.into(), b"done");
        ep.close().await;
    }
    Ok(())
}

async fn measure(conn: &Connection, mode: &str, pings: usize, bytes: u64) -> Result<()> {
    let before = selected(conn);

    let mut dg = Vec::with_capacity(pings);
    let mut lost = 0;
    for seq in 0..(pings + 10) as u64 {
        let t = Instant::now();
        conn.send_datagram(seq.to_be_bytes().to_vec().into())?;
        let got = timeout(Duration::from_secs(1), async {
            loop {
                let d = conn.read_datagram().await?;
                if d.len() == 8 && u64::from_be_bytes(d[..8].try_into().unwrap()) == seq {
                    return anyhow::Ok(());
                }
            }
        })
        .await;
        match got {
            Ok(r) => {
                r?;
                if seq >= 10 {
                    dg.push(t.elapsed());
                }
            }
            Err(_) => lost += 1,
        }
    }

    let (mut send, mut recv) = conn.open_bi().await?;
    AsyncWriteExt::write_all(&mut send, b"P").await?;
    let mut st = Vec::with_capacity(pings);
    for seq in 0..(pings + 10) as u64 {
        let t = Instant::now();
        AsyncWriteExt::write_all(&mut send, &seq.to_be_bytes()).await?;
        let mut buf = [0u8; 8];
        AsyncReadExt::read_exact(&mut recv, &mut buf).await?;
        if seq >= 10 {
            st.push(t.elapsed());
        }
    }
    send.finish()?;

    let (mut send, mut recv) = conn.open_bi().await?;
    let t = Instant::now();
    AsyncWriteExt::write_all(&mut send, b"U").await?;
    let chunk = vec![0xa5u8; CHUNK];
    let mut left = bytes as usize;
    while left > 0 {
        let k = left.min(CHUNK);
        AsyncWriteExt::write_all(&mut send, &chunk[..k]).await?;
        left -= k;
    }
    send.finish()?;
    let mut n = [0u8; 8];
    AsyncReadExt::read_exact(&mut recv, &mut n).await?;
    let up = t.elapsed();
    if u64::from_be_bytes(n) != bytes {
        bail!("올리기: 보낸 {bytes} 받은 {}", u64::from_be_bytes(n));
    }

    let (mut send, mut recv) = conn.open_bi().await?;
    let t = Instant::now();
    AsyncWriteExt::write_all(&mut send, b"D").await?;
    AsyncWriteExt::write_all(&mut send, &bytes.to_be_bytes()).await?;
    send.finish()?;
    let got = tokio::io::copy(&mut recv, &mut tokio::io::sink()).await?;
    let down = t.elapsed();
    if got != bytes {
        bail!("내리기: 기대 {bytes} 받은 {got}");
    }

    let after = selected(conn);
    let (d50, d90) = pct(&mut dg);
    let (s50, s90) = pct(&mut st);
    println!(
        "[{mode}] 길 {} → {} (QUIC rtt {:.1}ms)",
        before.as_ref().map(|s| s.remote.as_str()).unwrap_or("?"),
        after.as_ref().map(|s| s.remote.as_str()).unwrap_or("?"),
        after.as_ref().map(|s| ms(s.quic_rtt)).unwrap_or(f64::NAN),
    );
    println!(
        "[{mode}] datagram 왕복 p50 {d50:.1}ms p90 {d90:.1}ms (n={}, 유실 {lost})",
        dg.len()
    );
    println!(
        "[{mode}] 스트림 왕복 p50 {s50:.1}ms p90 {s90:.1}ms (n={})",
        st.len()
    );
    println!(
        "[{mode}] {:.0}MB 올리기 {:.2}s = {:.1} Mbps | 내리기 {:.2}s = {:.1} Mbps",
        bytes as f64 / 1e6,
        up.as_secs_f64(),
        mbps(bytes, up),
        down.as_secs_f64(),
        mbps(bytes, down)
    );
    Ok(())
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn mbps(bytes: u64, d: Duration) -> f64 {
    bytes as f64 * 8.0 / d.as_secs_f64() / 1e6
}

fn pct(v: &mut [Duration]) -> (f64, f64) {
    if v.is_empty() {
        return (f64::NAN, f64::NAN);
    }
    v.sort();
    let at = |q: usize| ms(v[(v.len() * q / 100).min(v.len() - 1)]);
    (at(50), at(90))
}
