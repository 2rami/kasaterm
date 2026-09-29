//! 카사넷 P3 — 다른 기기의 포트를 이 기기 localhost 로. 설계는 `docs/kasanet.md` 「P3 포트 공유」.
//!
//! 받는 쪽: 앱 HTTP 서버의 `/net/tcp?port=N` 웹소켓이 `127.0.0.1:N` 과 바이트를 잇는다. 문은 다른 라우트와
//! 같은 `origin_guard_mw`(루프백 peer 는 그대로, 원격은 토큰)에 더해 교차 출처 웹소켓과 손님 폰 주소를 막는다.
//! 끌어오는 쪽: `127.0.0.1:L`(+`[::1]:L`)을 듣고 연결마다 `route_base(기기 base)/net/tcp` 를 연다 — P2 입구가
//! 직통이면 카사넷, 아니면 원래 base(ssh 터널)로 고르므로 폴백이 따로 없다.
//!
//! 틀: 바이너리 프레임 = 날 바이트. 텍스트 프레임 `eof` = 보낸 쪽 TCP 가 쓰기를 닫았다(반쯤 닫기) — 요청을
//! 다 쓰고 쓰기를 닫은 뒤 응답을 기다리는 클라이언트가 있어 한쪽 끝을 전체 닫기로 바꾸면 응답이 잘린다.
//! 그 밖의 텍스트는 무시하고, 웹소켓 Close·끊김은 전체 닫기다.

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use axum::extract::ws::{Message as AxMessage, WebSocketUpgrade};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// 반쯤 닫기 표시. 폰(P5)도 같은 글자를 쓴다.
const EOF_TEXT: &str = "eof";
/// 관문(cloudflared) 무료 플랜은 유휴 웹소켓을 ~100초에 끊는다. 개발 서버의 HMR 소켓은 오래 조용하다.
const PING_EVERY: Duration = Duration::from_secs(30);

#[derive(Debug)]
enum Frame {
    Data(Bytes),
    Eof,
    Ping,
    Close,
    Skip,
}

fn from_axum(m: Result<AxMessage, axum::Error>) -> Frame {
    match m {
        Ok(AxMessage::Binary(b)) => Frame::Data(b),
        Ok(AxMessage::Text(t)) if t.as_str() == EOF_TEXT => Frame::Eof,
        Ok(AxMessage::Close(_)) | Err(_) => Frame::Close,
        Ok(_) => Frame::Skip,
    }
}

fn to_axum(f: Frame) -> AxMessage {
    match f {
        Frame::Data(b) => AxMessage::Binary(b),
        Frame::Eof => AxMessage::Text(EOF_TEXT.into()),
        Frame::Ping => AxMessage::Ping(Bytes::new()),
        Frame::Close | Frame::Skip => AxMessage::Close(None),
    }
}

/// TCP 하나와 웹소켓 하나를 잇는다. 두 방향은 따로 돈다 — 한쪽 쓰기가 막힌 동안 다른 쪽 읽기까지 멈추면
/// 서로의 받는 버퍼가 차 둘 다 멈춘다. 양쪽이 다 `eof` 를 냈을 때만 정상 종료(Close)다.
async fn pump<R, W>(mut rx: R, tx: W, tcp: TcpStream, ping: Option<Duration>)
where
    R: Stream<Item = Frame> + Unpin,
    W: Sink<Frame> + Unpin,
{
    let _ = tcp.set_nodelay(true);
    let (mut rd, mut wr) = tcp.into_split();
    let tx = tokio::sync::Mutex::new(tx);
    let up = async {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let frame = match rd.read(&mut buf).await {
                Ok(0) | Err(_) => Frame::Eof,
                Ok(n) => Frame::Data(Bytes::copy_from_slice(&buf[..n])),
            };
            let eof = matches!(frame, Frame::Eof);
            if tx.lock().await.send(frame).await.is_err() {
                return false;
            }
            if eof {
                return true;
            }
        }
    };
    let down = async {
        loop {
            match rx.next().await {
                Some(Frame::Data(b)) => {
                    if wr.write_all(&b).await.is_err() {
                        return false;
                    }
                }
                Some(Frame::Eof) => {
                    let _ = wr.shutdown().await;
                    return true;
                }
                Some(Frame::Skip | Frame::Ping) => {}
                Some(Frame::Close) | None => return false,
            }
        }
    };
    let pinger = async {
        let Some(every) = ping else {
            return std::future::pending::<()>().await;
        };
        loop {
            tokio::time::sleep(every).await;
            if tx.lock().await.send(Frame::Ping).await.is_err() {
                return;
            }
        }
    };
    tokio::pin!(up, down, pinger);
    let (mut up_done, mut down_done) = (false, false);
    while !(up_done && down_done) {
        tokio::select! {
            ok = &mut up, if !up_done => if ok { up_done = true } else { return },
            ok = &mut down, if !down_done => if ok { down_done = true } else { break },
            _ = &mut pinger => return,
        }
    }
    let mut tx = tx.lock().await;
    let _ = tx.send(Frame::Close).await;
    let _ = tx.close().await;
}

// ── 받는 쪽: /net/tcp ─────────────────────────────────────────────────────────

fn port_param(query: Option<&str>) -> Option<u16> {
    query?
        .split('&')
        .find_map(|kv| kv.strip_prefix("port="))
        .and_then(|p| p.parse::<u16>().ok())
        .filter(|p| *p != 0)
}

/// 루프백만. 개발 서버가 `localhost` 로 뜨면 `::1` 에만 묶이는 일이 있어(Node 17+ 의 Vite) 거부되면 한 번 더.
async fn connect_loopback(port: u16) -> std::io::Result<TcpStream> {
    let v4 = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let v6 = SocketAddr::from((Ipv6Addr::LOCALHOST, port));
    let first = tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(v4)).await;
    match first {
        Ok(Ok(s)) => Ok(s),
        _ => tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(v6))
            .await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "loopback connect timed out"))?,
    }
}

/// `GET /net/tcp?port=N` — 이 기기 `127.0.0.1:N` 과 잇는 웹소켓. 원격 토큰은 `origin_guard_mw` 가 이미 봤다.
pub(crate) async fn tcp_ws_handler(req: axum::extract::Request) -> Response {
    // 폰 주소로 들어오면 이 기기의 아무 포트에 닿는 로컬 권한이 된다 — 주인만.
    if let Some(denied) = crate::http::guest_denied(&req) {
        return denied;
    }
    if !crate::http::ws_origin_ok(req.headers()) {
        return (axum::http::StatusCode::FORBIDDEN, "cross-origin websocket refused").into_response();
    }
    let Some(port) = port_param(req.uri().query()) else {
        return (axum::http::StatusCode::BAD_REQUEST, "port=1..65535 required").into_response();
    };
    use axum::extract::FromRequestParts as _;
    let (mut parts, _body) = req.into_parts();
    let ws = match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
        Ok(ws) => ws,
        Err(e) => return e.into_response(),
    };
    // 업그레이드 전에 붙여 본다 — 끌어오는 쪽이 「아무도 안 듣는다」를 101 뒤 끊김이 아니라 502 로 받게.
    let tcp = match connect_loopback(port).await {
        Ok(s) => s,
        Err(_) => {
            return (axum::http::StatusCode::BAD_GATEWAY, format!("nothing listening on 127.0.0.1:{port}")).into_response();
        }
    };
    ws.on_upgrade(move |socket| async move {
        let (tx, rx) = socket.split();
        let tx = tx.with(|f: Frame| std::future::ready(Ok::<_, axum::Error>(to_axum(f))));
        pump(rx.map(from_axum), Box::pin(tx), tcp, Some(PING_EVERY)).await;
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

fn from_ws(m: Result<WsMessage, tokio_tungstenite::tungstenite::Error>) -> Frame {
    match m {
        Ok(WsMessage::Binary(b)) => Frame::Data(b),
        Ok(WsMessage::Text(t)) if t.as_str() == EOF_TEXT => Frame::Eof,
        Ok(WsMessage::Close(_)) | Err(_) => Frame::Close,
        Ok(_) => Frame::Skip,
    }
}

fn to_ws(f: Frame) -> WsMessage {
    match f {
        Frame::Data(b) => WsMessage::Binary(b),
        Frame::Eof => WsMessage::Text(EOF_TEXT.into()),
        Frame::Ping => WsMessage::Ping(Bytes::new()),
        Frame::Close | Frame::Skip => WsMessage::Close(None),
    }
}

    async fn echo_server() -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                tokio::spawn(async move {
                    let (mut r, mut w) = s.split();
                    let _ = tokio::io::copy(&mut r, &mut w).await;
                    let _ = w.shutdown().await;
                });
            }
        });
        port
    }

    async fn app(layer: Option<crate::mobile::MobileUser>) -> String {
        let mut router = axum::Router::new().route("/net/tcp", axum::routing::get(tcp_ws_handler));
        if let Some(user) = layer {
            router = router.layer(axum::middleware::from_fn(move |mut r: axum::extract::Request, next: axum::middleware::Next| {
                r.extensions_mut().insert(crate::http::MobileAuth(user.clone()));
                async move { next.run(r).await }
            }));
        }
        let router = router.layer(axum::middleware::from_fn(crate::http::origin_guard_mw));
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("ws://{}", l.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(l, router.into_make_service_with_connect_info::<SocketAddr>()).await.unwrap();
        });
        base
    }

    fn status_of(e: tokio_tungstenite::tungstenite::Error) -> u16 {
        match e {
            tokio_tungstenite::tungstenite::Error::Http(r) => r.status().as_u16(),
            other => panic!("HTTP 거부가 아니다: {other}"),
        }
    }

    async fn next_data<S>(ws: &mut S) -> Frame
    where
        S: Stream<Item = Result<WsMessage, tokio_tungstenite::tungstenite::Error>> + Unpin,
    {
        loop {
            match tokio::time::timeout(Duration::from_secs(5), ws.next()).await.expect("응답 없음") {
                Some(m) => match from_ws(m) {
                    Frame::Skip | Frame::Ping => continue,
                    f => return f,
                },
                None => return Frame::Close,
            }
        }
    }

    #[tokio::test]
    async fn bytes_round_trip_and_half_close() {
        let echo = echo_server().await;
        let base = app(None).await;
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("{base}/net/tcp?port={echo}")).await.unwrap();
        ws.send(to_ws(Frame::Data(Bytes::from_static(b"hello")))).await.unwrap();
        assert!(matches!(next_data(&mut ws).await, Frame::Data(b) if &b[..] == b"hello"));
        // 쓰기를 닫아도 받는 쪽은 살아 있어야 한다 — 에코 서버는 EOF 를 보고서야 제 쓰기를 닫는다.
        ws.send(to_ws(Frame::Data(Bytes::from_static(b"tail")))).await.unwrap();
        ws.send(to_ws(Frame::Eof)).await.unwrap();
        assert!(matches!(next_data(&mut ws).await, Frame::Data(b) if &b[..] == b"tail"));
        assert!(matches!(next_data(&mut ws).await, Frame::Eof));
        assert!(matches!(next_data(&mut ws).await, Frame::Close));
    }

    #[tokio::test]
    async fn refuses_before_upgrade() {
        let base = app(None).await;
        let dead = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let e = tokio_tungstenite::connect_async(format!("{base}/net/tcp?port={dead}")).await.unwrap_err();
        assert_eq!(status_of(e), 502);
        let e = tokio_tungstenite::connect_async(format!("{base}/net/tcp?port=0")).await.unwrap_err();
        assert_eq!(status_of(e), 400);
    }

    #[tokio::test]
    async fn remote_needs_token_and_page_origin() {
        let echo = echo_server().await;
        let base = app(None).await;
        let url = format!("{base}/net/tcp?port={echo}");
        // 터널로 들어온 요청은 peer 가 루프백이어도 원격이다.
        let mut r = url.clone().into_client_request().unwrap();
        r.headers_mut().insert("x-forwarded-for", "203.0.113.9".parse().unwrap());
        assert_eq!(status_of(tokio_tungstenite::connect_async(r).await.unwrap_err()), 403);
        // 남의 페이지가 여는 웹소켓 — 루프백이라도 막는다.
        let mut r = url.clone().into_client_request().unwrap();
        r.headers_mut().insert("origin", "https://evil.example.com".parse().unwrap());
        assert_eq!(status_of(tokio_tungstenite::connect_async(r).await.unwrap_err()), 403);
    }

    #[tokio::test]
    async fn phone_address_must_be_owner() {
        let echo = echo_server().await;
        let guest = crate::mobile::MobileUser { name: "손님".into(), slug: "abcdefghijklmnopqrstuvwxy".into(), created: 0, owner: false };
        let owner = crate::mobile::MobileUser { owner: true, ..guest.clone() };
        let base = app(Some(guest)).await;
        let e = tokio_tungstenite::connect_async(format!("{base}/net/tcp?port={echo}")).await.unwrap_err();
        assert_eq!(status_of(e), 403);
        let base = app(Some(owner)).await;
        assert!(tokio_tungstenite::connect_async(format!("{base}/net/tcp?port={echo}")).await.is_ok());
    }
}
