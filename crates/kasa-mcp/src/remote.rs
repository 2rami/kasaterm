//! 원격 PTY 호스트(`/term/ws`)에 붙는 **GUI 쪽 클라이언트**.
//!
//! 서버(`http.rs` 의 `term_ws_run`)와 같은 crate 에 두어 term-ws 프로토콜 지식이
//! 한 곳에 모인다. 역할은 전송뿐이다 — 파싱·스크롤백·tap 은 전부
//! `kasa_pty::PtySession::start_external` 의 로컬 파서가 맡고, 여기는 WS 프레임을
//! `ExtEvent` 로 옮겨 싣기만 한다.
//!
//! 프레임 규약(서버와 쌍):
//! - binary = PTY 바이트(양방향 — 수신은 화면, 송신은 키 입력)
//! - text  = 제어 JSON. 수신 `{"t":"size",cols,rows,id}` / `{"t":"gone"}`,
//!   송신 `{"t":"resize",cols,rows}` / `{"t":"kill"}`
//!
//! 재접속: 연결이 끊기면 백오프(0.5s→5s)로 같은 세션에 다시 붙는다. 서버가
//! 접속 직후 스냅샷(히스토리 포함)을 보내므로, 그 직전에 RIS(`ESC c`)를 파서에
//! 밀어 로컬 그리드·스크롤백을 비운다 — alacritty `Grid::reset` 이
//! `clear_history` 를 부르는 것에 기댄 설계다(kasa-pty 의
//! `external_reconnect_ris_clears_history` 테스트가 그 계약을 감시한다).
//! 「세션이 정말 끝났다」는 서버의 `gone` 만이 말한다 — 연결 유실만으로는
//! 재접속을 멈추지 않는다.

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use futures_util::{SinkExt, StreamExt};
use kasa_pty::{ExtEvent, ExternalIo, PtyOptions, PtySession};
use tokio_tungstenite::tungstenite::Message;

/// 원격 pane 하나의 연결 명세.
#[derive(Clone, Debug)]
pub struct RemoteSpec {
    /// `http://127.0.0.1:18766` 꼴 — ssh 터널의 로컬 끝이 보통이다.
    pub base: String,
    /// 이어받을 원격 세션 id(`web-…`). None = 새 셸을 띄운다.
    pub pane: Option<String>,
    /// 새 셸의 시작 디렉터리(**원격 기계 기준** 경로). pane 지정 시 무시.
    pub cwd: Option<String>,
    /// LAN 직결일 때의 remote-token. ssh 터널(양끝 loopback)이면 불필요.
    pub token: Option<String>,
}

/// 접속에 성공한 원격 pane.
pub struct RemoteSession {
    pub session: Arc<PtySession>,
    /// 서버가 확정해 준 원격 세션 id — session.json 에 저장해 재접속에 쓴다.
    pub remote_id: String,
}

enum Out {
    Input(Vec<u8>),
    Control(String),
}

/// 원격 링크 하나의 명부 항목.
struct Link {
    kill: tokio::sync::mpsc::UnboundedSender<()>,
    base: String,
    remote_id: String,
    /// 같은 local pane id 가 재사용될 때 낡은 매니저의 정리 가드가 **새 링크를**
    /// 걷어가지 않게 하는 세대 표식.
    token: u64,
}

/// 살아 있는 원격 링크 명부 — local pane id → 링크.
///
/// GUI 의 pane 닫기·저장 경로가 App 필드 없이 원격 셸을 죽이고 전송 명세를
/// 읽을 수 있게 프로세스 전역이다(kasa-pty 레지스트리와 같은 이유). 항목은
/// 매니저 스레드가 끝날 때 스스로 걷는다.
fn links() -> &'static Mutex<std::collections::HashMap<String, Link>> {
    static L: std::sync::OnceLock<Mutex<std::collections::HashMap<String, Link>>> =
        std::sync::OnceLock::new();
    L.get_or_init(Default::default)
}

/// 원격 pane 의 전송 명세 `(base, remote_id)` — 세션 저장(layout_to_json)이
/// 재시작 후 같은 원격 세션에 다시 붙을 수 있게 싣는다.
pub fn remote_meta(local_id: &str) -> Option<(String, String)> {
    links()
        .lock()
        .unwrap()
        .get(local_id)
        .map(|l| (l.base.clone(), l.remote_id.clone()))
}

/// 이 pane 이 원격 링크인가.
pub fn is_remote_pane(local_id: &str) -> bool {
    links().lock().unwrap().contains_key(local_id)
}

/// 원격 셸까지 **정말로 죽인다**(pane 닫기 = 이사가 아니라 폐기일 때).
///
/// detach(그냥 Arc drop)와 이 길을 가르는 것이 원격 pane 수명 설계의 핵심이다 —
/// 안 부르고 닫으면 원격 셸은 살아남아 나중에 이어받을 수 있다.
pub fn kill_remote(local_id: &str) -> bool {
    let mut map = links().lock().unwrap();
    let Some(link) = map.get(local_id) else {
        return false;
    };
    if link.kill.send(()).is_err() {
        // 매니저가 이미 죽었다 — 낡은 항목을 걷는다.
        map.remove(local_id);
        return false;
    }
    true
}

/// `send_bytes` 가 쓰는 송신로 — WS 매니저의 출력 큐로 밀어 넣는다.
/// 연결이 잠깐 끊겨 있어도 큐에 쌓였다가 재접속 후 배달된다.
struct WsWriter(tokio::sync::mpsc::UnboundedSender<Out>);

impl Write for WsWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let _ = self.0.send(Out::Input(buf.to_vec()));
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn build_url(spec: &RemoteSpec, pane: Option<&str>) -> String {
    let ws_base = if let Some(rest) = spec.base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = spec.base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else if spec.base.starts_with("ws://") || spec.base.starts_with("wss://") {
        spec.base.clone()
    } else {
        format!("ws://{}", spec.base)
    };
    let mut url = format!("{}/term/ws?own=1", ws_base.trim_end_matches('/'));
    if let Some(p) = pane {
        // `%` 함정(webterm-handoff): `%N` 을 그대로 실으면 서버가 퍼센트 디코딩으로
        // 읽어 제어문자가 된다. 스크립트 경로는 반드시 인코딩한다.
        url.push_str("&pane=");
        url.push_str(&urlencode(p).replace('%', "%25").replace("%2525", "%25"));
    } else if let Some(c) = &spec.cwd {
        url.push_str("&cwd=");
        url.push_str(&urlencode(c));
    }
    if let Some(t) = &spec.token {
        url.push_str("&t=");
        url.push_str(t);
    }
    url
}

/// 원격 호스트에 붙어 로컬 파서 세션을 만든다. 동기 — 원격 id 와 초기 격자가
/// 확정되고서야 돌아오므로, 실패가 스폰 시점에 드러난다.
///
/// `cols`/`rows` 는 GUI 가 원하는 격자 — 서버 격자와 다르면 접속 직후 resize
/// 제어로 맞춘다(own=1 이라 force 없이 통과).
pub fn connect(
    spec: RemoteSpec,
    local_pane_id: &str,
    cols: u16,
    rows: u16,
) -> Result<RemoteSession> {
    let (etx, erx) = crossbeam_channel::unbounded::<ExtEvent>();
    let (otx, orx) = tokio::sync::mpsc::unbounded_channel::<Out>();
    let (ktx, krx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let (htx, hrx) =
        std::sync::mpsc::sync_channel::<std::result::Result<(String, u16, u16), String>>(1);
    static TOKEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let token = TOKEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let spec2 = spec.clone();
    let local2 = local_pane_id.to_string();
    std::thread::Builder::new()
        .name(format!("remote-ws-{local_pane_id}"))
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = htx.try_send(Err(format!("tokio runtime: {e}")));
                    return;
                }
            };
            rt.block_on(manager(spec2, local2, token, etx, orx, krx, htx));
        })
        .context("remote-ws 스레드")?;
    let (remote_id, rc, rr) = hrx
        .recv_timeout(Duration::from_secs(15))
        .context("원격 호스트가 15초 안에 응답하지 않았어요")?
        .map_err(|e| anyhow!(e))?;
    let otx_resize = otx.clone();
    let session = PtySession::start_external(
        PtyOptions {
            cols: rc,
            rows: rr,
            pane_id: local_pane_id.to_string(),
            ..Default::default()
        },
        ExternalIo {
            events: erx,
            writer: Box::new(WsWriter(otx)),
            on_resize: Arc::new(move |c, r| {
                let _ = otx_resize.send(Out::Control(
                    serde_json::json!({"t": "resize", "cols": c, "rows": r}).to_string(),
                ));
            }),
        },
    )?;
    links().lock().unwrap().insert(
        local_pane_id.to_string(),
        Link {
            kill: ktx,
            base: spec.base.clone(),
            remote_id: remote_id.clone(),
            token,
        },
    );
    let session = Arc::new(session);
    if (cols, rows) != (rc, rr) {
        let _ = session.resize(cols, rows);
    }
    Ok(RemoteSession {
        session,
        remote_id,
    })
}

#[allow(clippy::too_many_arguments)]
async fn manager(
    spec: RemoteSpec,
    local: String,
    token: u64,
    etx: crossbeam_channel::Sender<ExtEvent>,
    mut orx: tokio::sync::mpsc::UnboundedReceiver<Out>,
    mut krx: tokio::sync::mpsc::UnboundedReceiver<()>,
    htx: std::sync::mpsc::SyncSender<std::result::Result<(String, u16, u16), String>>,
) {
    // 어떤 길로 나가든 명부를 걷는다 — 안 걷으면 pane 번호가 재사용될 때 새 로컬
    // pane 이 「원격」으로 오판된다. 세대 표식이 맞을 때만 걷는 이유는 Link 주석에.
    struct Unlink(String, u64);
    impl Drop for Unlink {
        fn drop(&mut self) {
            let mut map = links().lock().unwrap();
            if map.get(&self.0).is_some_and(|l| l.token == self.1) {
                map.remove(&self.0);
            }
        }
    }
    let _unlink = Unlink(local, token);
    let mut remote_id: Option<String> = spec.pane.clone();
    // 첫 attach 가 이미 성사됐는가 — 이후의 연결 유실은 재접속 대상이고,
    // 그 전의 실패는 「스폰 실패」로 호출자에게 돌려준다.
    let mut had_attach = false;
    let mut attempts_before_first = 0u32;
    let mut backoff_ms = 500u64;
    'outer: loop {
        let url = build_url(&spec, remote_id.as_deref());
        match tokio_tungstenite::connect_async(&url).await {
            Ok((ws, _resp)) => {
                let (mut tx, mut rx) = ws.split();
                // 이 연결에서 size 핸드셰이크를 받았는가 — 재접속 RIS 는 연결마다
                // 첫 size 에서 딱 한 번.
                let mut sized_this_conn = false;
                loop {
                    tokio::select! {
                        m = rx.next() => match m {
                            Some(Ok(Message::Text(t))) => {
                                let Ok(v) = serde_json::from_str::<serde_json::Value>(t.as_str())
                                else { continue };
                                match v.get("t").and_then(|x| x.as_str()) {
                                    Some("size") => {
                                        let c = v.get("cols").and_then(|x| x.as_u64()).unwrap_or(80) as u16;
                                        let r = v.get("rows").and_then(|x| x.as_u64()).unwrap_or(24) as u16;
                                        if remote_id.is_none() {
                                            remote_id = v
                                                .get("id")
                                                .and_then(|x| x.as_str())
                                                .map(str::to_string);
                                        }
                                        if etx.send(ExtEvent::SetSize(c, r)).is_err() {
                                            return; // 세션이 사라졌다(detach)
                                        }
                                        if had_attach && !sized_this_conn {
                                            // 재접속 — 곧 올 스냅샷 앞에 RIS 로 그리드·
                                            // 스크롤백을 비운다(모듈 머리말 참고).
                                            let _ = etx.send(ExtEvent::Bytes(b"\x1bc".to_vec()));
                                        }
                                        sized_this_conn = true;
                                        if !had_attach {
                                            had_attach = true;
                                            let id = remote_id.clone().unwrap_or_default();
                                            let _ = htx.try_send(Ok((id, c, r)));
                                        }
                                    }
                                    Some("gone") => {
                                        // 세션이 정말 끝났다 — 재접속하지 않는다.
                                        let _ = htx.try_send(Err(
                                            "원격 세션이 이미 끝났어요".to_string(),
                                        ));
                                        let _ = etx.send(ExtEvent::Eof);
                                        return;
                                    }
                                    _ => {}
                                }
                            }
                            Some(Ok(Message::Binary(b))) => {
                                if etx.send(ExtEvent::Bytes(b.to_vec())).is_err() {
                                    return;
                                }
                            }
                            Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                            // Ping/Pong — tungstenite 가 pong 을 알아서 큐잉한다.
                            // 실제 송신은 아래 주기 flush 가 밀어낸다.
                            Some(Ok(_)) => {}
                        },
                        o = orx.recv() => match o {
                            Some(Out::Input(b)) => {
                                if tx.send(Message::Binary(b.into())).await.is_err() {
                                    break;
                                }
                            }
                            Some(Out::Control(s)) => {
                                if tx.send(Message::Text(s.into())).await.is_err() {
                                    break;
                                }
                            }
                            // 세션이 drop 됐다(모든 송신자 소멸) = detach. 원격 셸은
                            // 살려 둔 채 조용히 접는다 — 단, 직전에 kill 신호가 큐에
                            // 남아 있으면 그건 폐기 의도이므로 마지막으로 배달한다.
                            None => {
                                if krx.try_recv().is_ok() {
                                    let _ = tx.send(Message::Text(
                                        serde_json::json!({"t": "kill"}).to_string().into(),
                                    )).await;
                                }
                                let _ = tx.close().await;
                                break 'outer;
                            }
                        },
                        k = krx.recv() => {
                            if k.is_some() {
                                // 명시적 폐기 — 서버가 keep 을 놓고, 우리는 eof 로 pane 을 걷는다.
                                let _ = tx.send(Message::Text(
                                    serde_json::json!({"t": "kill"}).to_string().into(),
                                )).await;
                                let _ = tx.close().await;
                                let _ = etx.send(ExtEvent::Eof);
                                break 'outer;
                            }
                            // ktx 전부 소멸(명부에서 걷힘) — kill 은 더 안 온다. 채널이
                            // 닫힌 채 recv 를 계속 부르면 busy loop 라, 여기서부터는
                            // 영원히 잠드는 팔로 바꾼다.
                            std::future::pending::<()>().await;
                        }
                        // 조용한 pane 대비: tungstenite 가 큐잉해 둔 auto-pong 을 주기로
                        // 밀어낸다. 안 밀면 서버의 75초 무-pong 판정에 걸려 끊긴다.
                        _ = tokio::time::sleep(Duration::from_secs(15)) => {
                            let _ = tx.flush().await;
                        }
                    }
                }
                backoff_ms = 500;
            }
            Err(e) => {
                if !had_attach {
                    attempts_before_first += 1;
                    if attempts_before_first >= 6 {
                        let _ = htx.try_send(Err(format!("원격 호스트에 못 붙었어요: {e}")));
                        return;
                    }
                }
            }
        }
        // 세션이 이미 사라졌으면 재접속할 이유가 없다.
        match orx.try_recv() {
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break 'outer,
            _ => {}
        }
        tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
        backoff_ms = (backoff_ms * 2).min(5000);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_visible(sess: &PtySession, needle: &str, secs: u64) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(secs);
        while std::time::Instant::now() < deadline {
            if sess.visible_text(50).contains(needle) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }

    /// 같은 프로세스에 서버(spawn_http_server_opts)와 클라이언트를 함께 띄워
    /// 전 구간을 돈다: 스폰 → 타이핑 → 둘째 클라이언트 이어받기(스냅샷) → kill.
    #[test]
    fn remote_spawn_type_reattach_kill_roundtrip() {
        let backend: Arc<dyn kasa_socket::backend::Backend> =
            Arc::new(crate::standalone::StandaloneBackend::new(std::env::temp_dir()));
        let port = crate::spawn_http_server_opts(backend, 0, false).expect("server");
        let spec = RemoteSpec {
            base: format!("http://127.0.0.1:{port}"),
            pane: None,
            cwd: None,
            token: None,
        };
        let rs = connect(spec.clone(), "%rmt0", 60, 12).expect("connect");
        assert!(rs.remote_id.starts_with("web-"), "id={}", rs.remote_id);
        assert!(is_remote_pane("%rmt0"));
        // 셸이 뜨고 에코가 로컬 그리드에 맺힌다 — 원격 파싱이 아니라 로컬 파싱.
        rs.session
            .send_bytes(b"printf 'hi-remote-42\\n'\r")
            .unwrap();
        assert!(
            wait_visible(&rs.session, "hi-remote-42", 10),
            "타이핑 에코가 그리드에 없다: {:?}",
            rs.session.visible_text(20)
        );
        // 이어받기: 같은 remote_id 로 붙은 둘째 클라이언트는 접속 스냅샷만으로
        // 이전 출력을 본다(그 사이 새 출력 없음).
        let rs2 = connect(
            RemoteSpec {
                pane: Some(rs.remote_id.clone()),
                ..spec.clone()
            },
            "%rmt1",
            60,
            12,
        )
        .expect("reattach");
        assert!(
            wait_visible(&rs2.session, "hi-remote-42", 10),
            "스냅샷 재생에 이전 출력이 없다"
        );
        // kill — keep 목록에서 걷히고, 세션은 eof 프레임으로 pane 을 걷게 한다.
        assert!(kill_remote("%rmt0"));
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let kept = kasa_pty::kept_sessions();
            if !kept.contains(&rs.remote_id) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "kill 후에도 keep 목록에 남아 있다: {kept:?}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut saw_eof = false;
        while std::time::Instant::now() < deadline {
            match rs.session.screens.recv_timeout(Duration::from_millis(200)) {
                Ok(u) if u.eof => {
                    saw_eof = true;
                    break;
                }
                _ => continue,
            }
        }
        assert!(saw_eof, "kill 뒤 eof 센티널이 안 왔다");
    }

    #[test]
    fn build_url_encodes_percent_pane_and_cwd() {
        let spec = RemoteSpec {
            base: "http://127.0.0.1:8766".into(),
            pane: Some("%12".into()),
            cwd: None,
            token: None,
        };
        assert_eq!(
            build_url(&spec, spec.pane.as_deref()),
            "ws://127.0.0.1:8766/term/ws?own=1&pane=%2512"
        );
        let spec = RemoteSpec {
            base: "http://h:1".into(),
            pane: None,
            cwd: Some("/Users/miku/한글 폴더".into()),
            token: Some("tok".into()),
        };
        let url = build_url(&spec, None);
        assert!(url.contains("cwd=/Users/miku/%ED%95%9C%EA%B8%80%20%ED%8F%B4%EB%8D%94"), "{url}");
        assert!(url.ends_with("&t=tok"));
    }
}
