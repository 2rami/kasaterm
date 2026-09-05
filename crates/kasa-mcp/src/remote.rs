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

/// 원격 pane 의 「누구·어디」 — 연결 자체에는 안 쓰이고, 표시(기계 배지)와
/// 역이사(되돌아갈 자리)가 읽는다. 링크가 사는 동안 Link 에 실려 다니고
/// 세션 저장을 거쳐 재시작을 넘는다.
#[derive(Clone, Debug, Default)]
pub struct RemoteIdentity {
    /// 기계 명부(machines.json)의 라벨. 명부 밖 주소로 붙었으면 빈 문자열.
    pub label: String,
    /// 그 pane 의 **원격 기계 기준** 작업 폴더. 역이사가 원격 git 상태를 물을 때 쓴다.
    pub remote_cwd: Option<String>,
    /// 이사(migrate)로 떠나온 pane 의 **원래 로컬 경로** — 역이사가 되돌아갈 자리.
    /// 원격에서 태어난 pane 은 None 이고, 그때 역이사는 명부의 roots 매핑으로 정한다.
    pub origin_cwd: Option<String>,
}

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
    pub identity: RemoteIdentity,
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
    identity: RemoteIdentity,
    /// 거울(뷰어)인가 — 이 pane 은 원본 세션의 격자를 **절대 바꾸지 않는다**.
    /// 이사(migrate)·승격(promote)처럼 이 앱이 그 세션의 주인인 경우와 갈리는
    /// 유일한 지점이라 추측이 아니라 연결 창구(`connect_view`)로만 세운다.
    view: bool,
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
/// 호스트가 거울로 밀어 준 `open-url` 을 받을 GUI 쪽 손잡이 — (로컬 pane id, URL).
/// kasa-mcp 는 창을 모르니 앱이 시작할 때 EventLoopProxy 를 물려 등록한다.
type OpenUrlSink = Box<dyn Fn(&str, &str) + Send + Sync>;

fn open_url_sink() -> &'static std::sync::OnceLock<OpenUrlSink> {
    static S: std::sync::OnceLock<OpenUrlSink> = std::sync::OnceLock::new();
    &S
}

pub fn set_open_url_sink(f: OpenUrlSink) {
    let _ = open_url_sink().set(f);
}

fn fire_open_url(local: &str, url: &str) {
    match open_url_sink().get() {
        Some(f) => f(local, url),
        None => eprintln!("[remote] open-url 받았지만 받을 창이 없어요: {url}"),
    }
}

pub fn is_remote_pane(local_id: &str) -> bool {
    links().lock().unwrap().contains_key(local_id)
}

/// 이 pane 이 **거울**인가 — 원본 격자를 못 바꾸는 뷰어 연결. 로컬 리사이즈
/// 전파(resize_backend)와 렌더의 자동 맞춤이 이걸로 갈린다.
pub fn is_view_pane(local_id: &str) -> bool {
    links()
        .lock()
        .unwrap()
        .get(local_id)
        .is_some_and(|l| l.view)
}

/// 원격 pane 의 전송 명세 + 정체 한 벌. remote_meta 와 달리 표시·역이사가 쓴다.
#[derive(Clone, Debug)]
pub struct RemoteInfo {
    pub base: String,
    pub remote_id: String,
    pub label: String,
    pub remote_cwd: Option<String>,
    pub origin_cwd: Option<String>,
    /// 거울 연결(원본 격자 불변). 세션 저장이 이걸 실어야 재시작 뒤에도
    /// 거울로 되붙는다 — 안 실으면 복원된 pane 이 원본 크기를 뺏는다.
    pub view: bool,
}

pub fn remote_info(local_id: &str) -> Option<RemoteInfo> {
    links().lock().unwrap().get(local_id).map(|l| RemoteInfo {
        base: l.base.clone(),
        remote_id: l.remote_id.clone(),
        label: l.identity.label.clone(),
        remote_cwd: l.identity.remote_cwd.clone(),
        origin_cwd: l.identity.origin_cwd.clone(),
        view: l.view,
    })
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
/// 제어로 맞춘다(own=1 이라 force 없이 통과). **이 앱이 그 세션의 주인일 때만**
/// 쓴다(스폰·이사·승격). 남의 화면을 들여다보는 거울은 `connect_view` 로.
pub fn connect(
    spec: RemoteSpec,
    local_pane_id: &str,
    cols: u16,
    rows: u16,
) -> Result<RemoteSession> {
    connect_inner(spec, local_pane_id, cols, rows, false)
}

/// **거울(뷰어)** 로 붙는다 — 원본 세션의 격자를 절대 바꾸지 않는다.
///
/// 접속 직후의 맞춤 resize 도 보내지 않고(그래서 `cols`/`rows` 인자가 없다),
/// 이후 로컬 pane 리사이즈도 원격으로 전파하지 않는다. tmux 의 최소-클라이언트
/// 문제와 같은 것 — 작은 거울 창 하나가 원본 기계의 화면을 쪼그라뜨리던 자리다
/// (2026-09-02 지시: 「미러링할때 크기 줄이면 미러링되는곳도 pane안에서 줄어들어」).
/// 로컬 격자는 서버가 보내는 size 핸드셰이크만 따르고, 로컬 pane 이 그보다
/// 작으면 GUI 가 그 pane 의 글자 배율을 줄여 담는다.
pub fn connect_view(spec: RemoteSpec, local_pane_id: &str) -> Result<RemoteSession> {
    connect_inner(spec, local_pane_id, 0, 0, true)
}

fn connect_inner(
    spec: RemoteSpec,
    local_pane_id: &str,
    cols: u16,
    rows: u16,
    view: bool,
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
                // 거울은 원본 격자의 주인이 아니다 — 로컬 pane 이 줄어도 제어를
                // 안 보낸다. 여기가 마지막 방어선이고, 애초에 GUI 쪽 resize 전파
                // (resize_backend)가 거울 pane 을 건너뛴다.
                if view {
                    return;
                }
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
            identity: spec.identity.clone(),
            view,
            token,
        },
    );
    let session = Arc::new(session);
    // 거울은 서버 격자를 그대로 받아 산다 — 맞춤 resize 조차 원본을 흔든다.
    if !view && (cols, rows) != (rc, rr) {
        let _ = session.resize(cols, rows);
    }
    Ok(RemoteSession { session, remote_id })
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
    let _unlink = Unlink(local.clone(), token);
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
                                    // 호스트가 「이 페이지를 네 쪽에서 열어라」 —
                                    // 본진 학생이 연 브라우저를 보는 사람의 기계로
                                    // 되돌리는 길(http.rs push_viewer_control).
                                    Some("open-url") => {
                                        if let Some(u) = v.get("url").and_then(|x| x.as_str()) {
                                            fire_open_url(&local, u);
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

/// 원격 kasaterm 에 **진짜 학생 pane** 을 하나 만들고 그 pane id 를 받는다.
///
/// 이사(migrate)가 이걸 먼저 부른다. 안 부르고 새 셸만 띄우면 그 자리는 캐릭터도
/// 보드도 훅도 없는 반쪽이 된다 — 옮겨간 학생이 이름을 잃는다(2026-08-27 지시:
/// 「진짜 카사텀이 돌게」). 창 없는 축소판 서버(kasa-serve-web)는 이 창구가
/// 없으므로 실패하고, 호출자는 옛 경로로 물러선다.
pub fn spawn_student_pane(base: &str, character: &str, token: Option<&str>) -> Result<String> {
    let u = format!(
        "{}/spawn-student?character={}",
        base.trim_end_matches('/'),
        urlencode(character)
    );
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("spawn runtime")?;
    let v: serde_json::Value = rt.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .context("http client")?;
        let mut req = client.post(&u);
        if let Some(t) = token {
            req = req.header("x-kasa-token", t);
        }
        let r = req.send().await.context("캐릭터 소환 요청")?;
        let status = r.status();
        let text = r.text().await.unwrap_or_default();
        Ok::<_, anyhow::Error>(serde_json::from_str(&text).unwrap_or_else(
            |_| serde_json::json!({ "ok": false, "error": format!("HTTP {status}: {text}") }),
        ))
    })?;
    if v.get("ok").and_then(|x| x.as_bool()) != Some(true) {
        anyhow::bail!(
            "원격 캐릭터 소환 실패: {}",
            v.get("error")
                .and_then(|x| x.as_str())
                .unwrap_or("알 수 없는 이유")
        );
    }
    let id = v
        .get("surface")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    if id.is_empty() {
        anyhow::bail!("원격이 pane id 를 안 돌려줬어요");
    }
    Ok(id)
}

/// 원격 pane 의 캐릭터를 그 이름으로 못 박는다(`GET /repersona`).
///
/// 소환만으로는 못 미덥다 — 2026-08-27 실측에서 `spawn-student?character=유즈` 로
/// 만든 자리가 **이름표는 유즈, 실제 말투는 남의 캐릭터**로 떴다(pane id 가 재사용된
/// 자리였다). 그래서 claude 를 띄우기 **직전에** 한 번 더 박는다 — 학생 명령 셰임이
/// 쓰는 것과 같은 창구고, respawn 없이 override 파일만 갱신한다.
/// `sid` 는 이사 전용 — 학생의 **원 대화 세션 id** 를 주면 서버가 그 sid 에
/// 캐릭터를 바인딩+수동 표식으로 못박아, resume 뒤의 복원·명단 검사가 이사 온
/// 학생을 개명하지 못한다(2026-08-31 실측: 시로코가 왕복에서 케이로 돌아왔다).
/// 낡은 서버는 파라미터를 몰라도 무시할 뿐 오류가 아니다.
pub fn repersona(
    base: &str,
    pane: &str,
    character: &str,
    sid: Option<&str>,
    token: Option<&str>,
) -> Result<()> {
    let mut u = format!(
        "{}/repersona?surface={}&character={}",
        base.trim_end_matches('/'),
        urlencode(pane).replace('%', "%25").replace("%2525", "%25"),
        urlencode(character)
    );
    if let Some(s) = sid.filter(|s| !s.is_empty()) {
        u.push_str("&sid=");
        u.push_str(&urlencode(s));
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("repersona runtime")?;
    rt.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .context("http client")?;
        let mut req = client.get(&u);
        if let Some(t) = token {
            req = req.header("x-kasa-token", t);
        }
        req.send().await.context("repersona 요청")?;
        Ok::<_, anyhow::Error>(())
    })
}

/// 출발지의 캐릭터 테마 선택(`character_theme` + 고른 명단 JSON)을 목적지에
/// 재현한다(`POST /term/character-theme`). 이사 때 안 실으면 도착지 기계의 제
/// 테마·명단으로 배정·복원이 돌아 학생이 다른 얼굴로 뜬다(2026-08-31 지적
/// 「이사시켜봤는데 테마 적용이 안 되네」). 실패는 Err 로 알리되 호출부는 경고만
/// 하고 이사를 계속한다 — 색·명단 문제일 뿐 대화는 무사하다.
pub fn push_character_theme(
    base: &str,
    theme_id: &str,
    picks_json: &str,
    token: Option<&str>,
) -> Result<()> {
    let u = format!(
        "{}/term/character-theme?theme={}",
        base.trim_end_matches('/'),
        urlencode(theme_id)
    );
    let body = picks_json.as_bytes().to_vec();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("theme runtime")?;
    let resp: serde_json::Value = rt.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .context("http client")?;
        let mut req = client.post(&u).body(body);
        if let Some(t) = token {
            req = req.header("x-kasa-token", t);
        }
        let r = req.send().await.context("테마 동행 요청")?;
        let status = r.status();
        let text = r.text().await.unwrap_or_default();
        Ok::<_, anyhow::Error>(serde_json::from_str(&text).unwrap_or_else(
            |_| serde_json::json!({ "ok": false, "error": format!("HTTP {status}: {text}") }),
        ))
    })?;
    if resp.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        anyhow::bail!(
            "{}",
            resp.get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("알 수 없는 이유")
        );
    }
    Ok(())
}

/// 테마 팩 zip 을 도착지에 푼다(`POST /term/character-theme?pack=1`) — 도착지에
/// 그 팩이 없어 push_character_theme 가 거절됐을 때 팩을 실어 나르는 두 번째 호출.
/// 성공 시 풀린 테마 id.
pub fn push_theme_pack(base: &str, zip: Vec<u8>, token: Option<&str>) -> Result<String> {
    let u = format!("{}/term/character-theme?pack=1", base.trim_end_matches('/'));
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("pack runtime")?;
    let resp: serde_json::Value = rt.block_on(async {
        let client = reqwest::Client::builder()
            // 그림 뭉치 수십 MB 가 터널을 타면 15초는 짧다.
            .timeout(Duration::from_secs(120))
            .build()
            .context("http client")?;
        let mut req = client.post(&u).body(zip);
        if let Some(t) = token {
            req = req.header("x-kasa-token", t);
        }
        let r = req.send().await.context("테마 팩 운반 요청")?;
        let status = r.status();
        let text = r.text().await.unwrap_or_default();
        Ok::<_, anyhow::Error>(serde_json::from_str(&text).unwrap_or_else(
            |_| serde_json::json!({ "ok": false, "error": format!("HTTP {status}: {text}") }),
        ))
    })?;
    if resp.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        anyhow::bail!(
            "{}",
            resp.get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("알 수 없는 이유")
        );
    }
    Ok(resp
        .get("theme")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string())
}

/// 원격 기계의 세션 목록 `(sid, name)` — 유령 명부 미러링의 소스(`GET /peer-registry`).
/// 소켓이 살아 있는 세션만 온다(원격이 걸러 준다).
pub fn fetch_peer_registry(base: &str, token: Option<&str>) -> Result<Vec<(String, String)>> {
    let u = format!("{}/peer-registry", base.trim_end_matches('/'));
    let (code, body) = blocking_get(&u, token, Duration::from_secs(10))?;
    if code != 200 {
        anyhow::bail!("peer-registry HTTP {code}");
    }
    let v: serde_json::Value = serde_json::from_slice(&body).context("peer-registry 파싱")?;
    let peers = v
        .get("peers")
        .and_then(|p| p.as_array())
        .ok_or_else(|| anyhow!("peers 배열이 없어요"))?;
    Ok(peers
        .iter()
        .filter_map(|p| {
            let sid = p.get("sid")?.as_str()?.to_string();
            let name = p
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or_default()
                .to_string();
            Some((sid, name))
        })
        .collect())
}

/// 다른 기계의 세션에게 cross-session 메시지를 보낸다(`POST /term/message`).
/// 기계 간 세션 소통의 **발신** 절반 — 받는 쪽은 그 sid 의 소켓에 claude 규격
/// JSON 을 꽂는다(2026-08-31 유령 세션 실증). 발신자 신원 셋(세션·사람·기계)을
/// 겉봉투에 실어, 사내 다계정에서 「남의 부탁」을 가릴 자리를 처음부터 판다.
/// `person` 은 같은 계정·내 기계끼리(1단계)면 빈 문자열.
#[allow(clippy::too_many_arguments)]
pub fn send_peer_message(
    base: &str,
    target_sid: &str,
    from_name: &str,
    from_person: &str,
    from_machine: &str,
    body: &str,
    token: Option<&str>,
) -> Result<()> {
    let u = format!(
        "{}/term/message?sid={}&from_name={}&from_person={}&from_machine={}",
        base.trim_end_matches('/'),
        urlencode(target_sid),
        urlencode(from_name),
        urlencode(from_person),
        urlencode(from_machine),
    );
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("message runtime")?;
    let resp: serde_json::Value = rt.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .context("http client")?;
        let mut req = client.post(&u).body(body.as_bytes().to_vec());
        if let Some(t) = token {
            req = req.header("x-kasa-token", t);
        }
        let r = req.send().await.context("메시지 전송 요청")?;
        let status = r.status();
        let text = r.text().await.unwrap_or_default();
        Ok::<_, anyhow::Error>(serde_json::from_str(&text).unwrap_or_else(
            |_| serde_json::json!({ "ok": false, "error": format!("HTTP {status}: {text}") }),
        ))
    })?;
    if resp.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        anyhow::bail!(
            "{}",
            resp.get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("알 수 없는 이유")
        );
    }
    Ok(())
}

// --- 릴레이(중계소) 클라이언트 ---------------------------------------------
// 셋 다 X-Relay-Token 인증(사내 공용 토큰). ⚠️토큰은 ASCII 만 — HTTP 헤더가
// latin-1 이라 한글 토큰은 헤더에서 깨져 무조건 401 이 난다(2026-09-01 실측).

/// 공용: 릴레이에 요청 하나 보내고 `{ok:true}` 응답을 확인한다.
fn relay_call(
    method: reqwest::Method,
    url: &str,
    token: Option<&str>,
    body: Option<Vec<u8>>,
    json: Option<serde_json::Value>,
) -> Result<serde_json::Value> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("relay runtime")?;
    let resp: serde_json::Value = rt.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .context("http client")?;
        let mut req = client.request(method, url);
        if let Some(t) = token {
            req = req.header("x-relay-token", t);
        }
        if let Some(j) = json {
            // reqwest 가 json feature 없이 빌드돼(.json 미존재) 손으로 직렬화한다.
            req = req
                .header("content-type", "application/json")
                .body(serde_json::to_vec(&j).unwrap_or_default());
        } else if let Some(b) = body {
            req = req.body(b);
        }
        let r = req.send().await.context("릴레이 요청")?;
        let status = r.status();
        let text = r.text().await.unwrap_or_default();
        Ok::<_, anyhow::Error>(serde_json::from_str(&text).unwrap_or_else(
            |_| serde_json::json!({ "ok": false, "error": format!("HTTP {status}: {text}") }),
        ))
    })?;
    if resp.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        anyhow::bail!(
            "{}",
            resp.get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("알 수 없는 이유")
        );
    }
    Ok(resp)
}

/// `POST /relay/register` — 이 기계와 살아 있는 세션 목록을 중계소에 올린다(upsert).
/// sessions 는 `[{sid,name,status}]`. advertise 는 중계소가 이 기계로 배달할 때 칠 주소.
pub fn relay_register(
    base: &str,
    token: Option<&str>,
    machine_id: &str,
    account: &str,
    advertise_base: &str,
    advertise_token: Option<&str>,
    sessions: &[serde_json::Value],
) -> Result<()> {
    let u = format!("{}/relay/register", base.trim_end_matches('/'));
    relay_call(
        reqwest::Method::POST,
        &u,
        token,
        None,
        Some(serde_json::json!({
            "machine_id": machine_id,
            "account": account,
            "base": advertise_base,
            "token": advertise_token.unwrap_or(""),
            "sessions": sessions,
        })),
    )
    .map(|_| ())
}

/// `GET /relay/sessions` — 중계소 명단. `(machine, sid, name)` 목록으로 돌려준다.
pub fn relay_sessions(base: &str, token: Option<&str>) -> Result<Vec<(String, String, String)>> {
    let u = format!("{}/relay/sessions", base.trim_end_matches('/'));
    let resp = relay_call(reqwest::Method::GET, &u, token, None, None)?;
    Ok(resp
        .get("sessions")
        .and_then(|s| s.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|r| {
                    Some((
                        r.get("machine")?.as_str()?.to_string(),
                        r.get("sid")?.as_str()?.to_string(),
                        r.get("name")?.as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default())
}

/// `POST /relay/send` — 중계소를 거쳐 대상 세션으로 보낸다. from_account 를 정직하게
/// 대면 계정이 다른 상대에겐 릴레이가 부탁 봉투를 강제한다.
pub fn relay_send(
    base: &str,
    token: Option<&str>,
    to_sid: &str,
    from_name: &str,
    from_account: &str,
    from_machine: &str,
    body: &str,
) -> Result<()> {
    let u = format!(
        "{}/relay/send?to_sid={}&from_name={}&from_account={}&from_machine={}",
        base.trim_end_matches('/'),
        urlencode(to_sid),
        urlencode(from_name),
        urlencode(from_account),
        urlencode(from_machine),
    );
    relay_call(
        reqwest::Method::POST,
        &u,
        token,
        Some(body.as_bytes().to_vec()),
        None,
    )
    .map(|_| ())
}

/// 원격이 그 pane 에 붙여 둔 캐릭터 이름. 미러로 붙일 때 **이 창에도 같은 학생**을
/// 앉히려고 읽는다 — 안 읽으면 몸통은 유즈인데 이 창만 이름·색·얼굴이 없다
/// (2026-08-27 거노 지적: 「옮기면 왜 테마가 없어져」).
pub fn remote_pane_character(base: &str, pane: &str, token: Option<&str>) -> Option<String> {
    let u = format!("{}/term/panes", base.trim_end_matches('/'));
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    let list: serde_json::Value = rt.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .ok()?;
        let mut req = client.get(&u);
        if let Some(t) = token {
            req = req.header("x-kasa-token", t);
        }
        let r = req.send().await.ok()?;
        serde_json::from_str(&r.text().await.ok()?).ok()
    })?;
    list.as_array()?.iter().find_map(|row| {
        (row.get("id")?.as_str()? == pane)
            .then(|| row.get("name")?.as_str().map(str::to_string))
            .flatten()
    })
}

/// 원격 pane 이 지금 쥔 claude(또는 codex) 세션 id — 그 기계의 `GET /pane-session`
/// (bound transcript stem). **원격에서 태어난 학생**의 거울은 이 창이 sid 를 모르므로
/// 데려오기(`migrate … local`)가 여기서 묻는다(2026-09-02 코유키 감사: 「sid 없음으로
/// 거부 — 수동 bind-transcript 없이 데려와야」). 미바인딩·미지정은 None.
pub fn remote_pane_session(base: &str, pane: &str, token: Option<&str>) -> Option<String> {
    let u = format!(
        "{}/pane-session?pane={}",
        base.trim_end_matches('/'),
        urlencode(pane)
    );
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    let text = rt.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .ok()?;
        let mut req = client.get(&u);
        if let Some(t) = token {
            req = req.header("x-kasa-token", t);
        }
        let r = req.send().await.ok()?;
        if !r.status().is_success() {
            return None;
        }
        r.text().await.ok()
    })?;
    let sid = text.trim().to_string();
    (!sid.is_empty()).then_some(sid)
}

/// 원격 GUI pane 을 닫는다(`POST /close-pane`). 데려오기(`migrate … local`) 뒤에
/// 부른다 — `kill_remote` 는 세션의 keep 참조만 놓는데 GUI pane 은 앱이 제 Arc 를
/// 쥐고 있어 셸이 안 죽고, 그 기계엔 학생 이름표만 남은 빈 pane 이 좀비로 선다
/// (2026-09-02 2-앱 리그 실측: 데려온 뒤 원격에 「Resume this session with」 셸이 남았다).
pub fn close_remote_pane(base: &str, pane: &str, token: Option<&str>) -> Result<()> {
    // kill=1 — 되살리기 대열에서도 걷는다. 낡은 원격은 그 인자를 몰라 닫기만 하고,
    // 그 셸은 되살리기 목록에 산 채 남는다(다음 배포까지의 한계).
    let u = format!(
        "{}/close-pane?surface={}&kill=1",
        base.trim_end_matches('/'),
        urlencode(pane)
    );
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("close runtime")?;
    let v: serde_json::Value = rt.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .context("http client")?;
        let mut req = client.post(&u);
        if let Some(t) = token {
            req = req.header("x-kasa-token", t);
        }
        let r = req.send().await.context("원격 pane 닫기 요청")?;
        let status = r.status();
        let text = r.text().await.unwrap_or_default();
        Ok::<_, anyhow::Error>(serde_json::from_str(&text).unwrap_or_else(
            |_| serde_json::json!({ "ok": false, "error": format!("HTTP {status}: {text}") }),
        ))
    })?;
    if v.get("ok").and_then(|x| x.as_bool()) != Some(true) {
        anyhow::bail!(
            "{}",
            v.get("error").and_then(|x| x.as_str()).unwrap_or("알 수 없는 이유")
        );
    }
    Ok(())
}

/// 원격 pane 의 (model, effort) — `/term/panes` 행의 `model`·`effort`. 낡은 원격은
/// 그 필드가 없어 None — 부른 쪽은 그때 기본값으로 물러선다.
pub fn remote_pane_cfg(base: &str, pane: &str, token: Option<&str>) -> Option<(String, String)> {
    let u = format!("{}/term/panes", base.trim_end_matches('/'));
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    let list: serde_json::Value = rt.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .ok()?;
        let mut req = client.get(&u);
        if let Some(t) = token {
            req = req.header("x-kasa-token", t);
        }
        let r = req.send().await.ok()?;
        serde_json::from_str(&r.text().await.ok()?).ok()
    })?;
    let row = list
        .as_array()?
        .iter()
        .find(|row| row.get("id").and_then(|v| v.as_str()) == Some(pane))?;
    let model = row.get("model").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let effort = row.get("effort").and_then(|v| v.as_str()).unwrap_or("").to_string();
    (!model.is_empty() || !effort.is_empty()).then_some((model, effort))
}

/// 원격 기계에 그 레포를 **있게** 만든다 — 없으면 clone, 뒤처졌으면 fast-forward.
/// 이사 전에 부른다: 대화만 건너가고 코드가 없거나 옛것이면 학생이 딴 세상에서 깬다.
/// 원격에 안 올린 변경이 있으면 서버가 거부하고 그 사유가 그대로 올라온다.
pub fn ensure_repo(
    base: &str,
    path: &str,
    url: Option<&str>,
    branch: Option<&str>,
    token: Option<&str>,
) -> Result<String> {
    let mut u = format!(
        "{}/term/repo?path={}",
        base.trim_end_matches('/'),
        urlencode(path)
    );
    if let Some(g) = url.filter(|s| !s.is_empty()) {
        u.push_str(&format!("&url={}", urlencode(g)));
    }
    if let Some(b) = branch.filter(|s| !s.is_empty()) {
        u.push_str(&format!("&branch={}", urlencode(b)));
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("repo runtime")?;
    let v: serde_json::Value = rt.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .context("http client")?;
        let mut req = client.post(&u);
        if let Some(t) = token {
            req = req.header("x-kasa-token", t);
        }
        let r = req.send().await.context("레포 준비 요청")?;
        let status = r.status();
        let text = r.text().await.unwrap_or_default();
        Ok::<_, anyhow::Error>(serde_json::from_str(&text).unwrap_or_else(
            |_| serde_json::json!({ "ok": false, "error": format!("HTTP {status}: {text}") }),
        ))
    })?;
    if v.get("ok").and_then(|x| x.as_bool()) != Some(true) {
        anyhow::bail!(
            "원격 레포 준비 실패: {}",
            v.get("error")
                .and_then(|x| x.as_str())
                .unwrap_or("알 수 없는 이유")
        );
    }
    Ok(format!(
        "{} ({} @{})",
        v.get("action").and_then(|x| x.as_str()).unwrap_or("?"),
        v.get("branch").and_then(|x| x.as_str()).unwrap_or("?"),
        v.get("head").and_then(|x| x.as_str()).unwrap_or("?")
    ))
}

/// 이사(migrate)의 대화 운반 — claude jsonl 하나를 원격 호스트의
/// `/term/transcript` 로 올린다. 동기 — 성공/실패가 호출 시점에 확정된다.
///
/// `remote_cwd` 는 **원격 기계 기준** 경로다. 저장 위치(claude projects 슬러그)를
/// 서버가 그 경로로 계산하므로, 이어질 원격 스폰의 cwd 와 같아야 resume 이 찾는다.
pub fn upload_transcript(
    base: &str,
    remote_cwd: &str,
    sid: &str,
    jsonl: &std::path::Path,
    token: Option<&str>,
    force: bool,
) -> Result<u64> {
    let bytes = std::fs::read(jsonl)
        .with_context(|| format!("대화 파일을 못 읽었어요: {}", jsonl.display()))?;
    let n = bytes.len() as u64;
    let mut url = format!(
        "{}/term/transcript?cwd={}&sid={}",
        base.trim_end_matches('/'),
        urlencode(remote_cwd),
        urlencode(sid)
    );
    if force {
        url.push_str("&force=1");
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("upload runtime")?;
    let resp: serde_json::Value = rt.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(180))
            .build()
            .context("http client")?;
        let mut req = client.post(&url).body(bytes);
        if let Some(t) = token {
            req = req.header("x-kasa-token", t);
        }
        let r = req.send().await.context("대화 업로드 요청")?;
        let status = r.status();
        // reqwest 는 json 피처 없이 들어와 있다 — text 로 받아 직접 파싱한다.
        let text = r.text().await.unwrap_or_default();
        Ok::<_, anyhow::Error>(serde_json::from_str(&text).unwrap_or_else(
            |_| serde_json::json!({ "ok": false, "error": format!("HTTP {status}: {text}") }),
        ))
    })?;
    if resp.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        anyhow::bail!(
            "대화 업로드 거부: {}",
            resp.get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("알 수 없는 이유")
        );
    }
    Ok(n)
}

/// 블로킹 HTTP 한 방 — 이 파일의 원격 호출 관례(현재 스레드 런타임, 실패가
/// 호출 시점에 확정)를 한 곳으로 모은다.
fn blocking_get(url: &str, token: Option<&str>, timeout: Duration) -> Result<(u16, Vec<u8>)> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("http runtime")?;
    rt.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .context("http client")?;
        let mut req = client.get(url);
        if let Some(t) = token {
            req = req.header("x-kasa-token", t);
        }
        let r = req.send().await.with_context(|| format!("GET {url}"))?;
        let status = r.status().as_u16();
        let body = r.bytes().await.unwrap_or_default().to_vec();
        Ok((status, body))
    })
}

/// 역이사의 대화 운반 — 원격 호스트의 jsonl 을 통짜로 받아 온다.
///
/// 옛 바이너리 폴백: `GET /term/transcript` 가 없는(404) 호스트에는
/// `GET /session-transcript-raw`(전부터 있던 JSON 래핑 창구)로 물러선다 —
/// 기계의 프로그램이 낡았다고 학생을 못 데려오면 안 된다.
pub fn download_transcript(
    base: &str,
    remote_cwd: &str,
    sid: &str,
    token: Option<&str>,
) -> Result<Vec<u8>> {
    let base = base.trim_end_matches('/');
    let url = format!(
        "{base}/term/transcript?cwd={}&sid={}",
        urlencode(remote_cwd),
        urlencode(sid)
    );
    let (status, body) = blocking_get(&url, token, Duration::from_secs(180))?;
    if status == 200 && !body.is_empty() {
        return Ok(body);
    }
    if status != 404 {
        anyhow::bail!(
            "대화 내려받기 실패(HTTP {status}): {}",
            String::from_utf8_lossy(&body[..body.len().min(200)])
        );
    }
    let url = format!(
        "{base}/session-transcript-raw?id={}&cwd={}",
        urlencode(sid),
        urlencode(remote_cwd)
    );
    let (status, body) = blocking_get(&url, token, Duration::from_secs(180))?;
    if status != 200 {
        anyhow::bail!("옛 창구도 실패(HTTP {status}) — 그 기계의 프로그램이 너무 낡았어요");
    }
    let v: serde_json::Value =
        serde_json::from_slice(&body).context("session-transcript-raw 응답 파싱")?;
    let raw = v
        .get("raw")
        .and_then(|r| r.as_str())
        .ok_or_else(|| anyhow!("응답에 raw 가 없어요"))?;
    Ok(raw.as_bytes().to_vec())
}

/// 원격 기계의 Codex 대화(rollout)를 내려받는다 — 역이사의 codex 판.
///
/// `Ok(None)` = 그 기계에 그 대화가 없다(창구 자체가 없는 낡은 기계도 같은 404 라
/// 여기로 온다 — 부르는 쪽이 claude 대화 실패와 합쳐 한 문장으로 말한다).
/// 반환은 (Codex home 기준 상대경로, rollout 바이트) — 상대경로가 있어야 도착지가
/// 같은 자리(`sessions/…`)에 앉힌다.
pub fn fetch_codex_session(
    base: &str,
    sid: &str,
    token: Option<&str>,
) -> Result<Option<(String, Vec<u8>)>> {
    let url = format!(
        "{}/term/codex-session?sid={}",
        base.trim_end_matches('/'),
        urlencode(sid)
    );
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("http runtime")?;
    rt.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(180))
            .build()
            .context("http client")?;
        let mut req = client.get(&url);
        if let Some(t) = token {
            req = req.header("x-kasa-token", t);
        }
        let r = req.send().await.with_context(|| format!("GET {url}"))?;
        let status = r.status().as_u16();
        if status == 404 {
            return Ok(None);
        }
        let rel = r
            .headers()
            .get("x-kasa-codex-rel")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let body = r.bytes().await.unwrap_or_default().to_vec();
        if status != 200 {
            anyhow::bail!(
                "Codex 대화 내려받기 실패(HTTP {status}): {}",
                String::from_utf8_lossy(&body[..body.len().min(200)])
            );
        }
        let rel = rel.ok_or_else(|| anyhow!("응답에 x-kasa-codex-rel 헤더가 없다"))?;
        Ok(Some((rel, body)))
    })
}

/// Codex 대화(rollout)를 원격 Codex home 의 같은 자리에 앉힌다 — 순방향 이사.
/// 도착지 검증·충돌 보관 정책은 저쪽 창구(codexhome::install_codex_rollout)가 맡는다.
pub fn push_codex_session(
    base: &str,
    sid: &str,
    rel: &str,
    bytes: Vec<u8>,
    token: Option<&str>,
) -> Result<String> {
    let url = format!(
        "{}/term/codex-session?sid={}&rel={}",
        base.trim_end_matches('/'),
        urlencode(sid),
        urlencode(rel)
    );
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("http runtime")?;
    let resp: serde_json::Value = rt.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(180))
            .build()
            .context("http client")?;
        let mut req = client.post(&url).body(bytes);
        if let Some(t) = token {
            req = req.header("x-kasa-token", t);
        }
        let r = req.send().await.context("Codex 대화 업로드 요청")?;
        let status = r.status().as_u16();
        if status == 404 || status == 405 {
            anyhow::bail!(
                "저쪽 프로그램이 낡아 Codex 이사 창구가 없다 — 그 기계를 갱신하고 다시"
            );
        }
        let text = r.text().await.unwrap_or_default();
        Ok::<_, anyhow::Error>(serde_json::from_str(&text).unwrap_or_else(|_| {
            serde_json::json!({ "ok": false, "error": format!("HTTP {status}: {text}") })
        }))
    })?;
    if resp.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        anyhow::bail!(
            "Codex 대화 업로드 거부: {}",
            resp.get("error").and_then(|v| v.as_str()).unwrap_or("알 수 없는 이유")
        );
    }
    Ok(resp
        .get("note")
        .and_then(|v| v.as_str())
        .unwrap_or("앉힘")
        .to_string())
}

/// 원격 레포의 「그 기계에만 있는 것」 — 역이사 git 관문의 재료.
/// (dirty 줄 수, 미push 커밋 수, origin, branch). 창구가 없는 옛 바이너리는
/// None — 관문을 못 세우는 것이지 이사가 불가능한 게 아니라서, 호출부가
/// force 요구로 갈음한다.
pub fn remote_repo_state(
    base: &str,
    path: &str,
    token: Option<&str>,
) -> Result<Option<(u64, u64, String, String)>> {
    let url = format!(
        "{}/term/repo?path={}",
        base.trim_end_matches('/'),
        urlencode(path)
    );
    let (status, body) = blocking_get(&url, token, Duration::from_secs(20))?;
    if status == 404 || status == 405 {
        return Ok(None);
    }
    let v: serde_json::Value = serde_json::from_slice(&body).context("repo 상태 파싱")?;
    if v.get("ok").and_then(|b| b.as_bool()) != Some(true) {
        anyhow::bail!(
            "원격 레포 상태 조회 실패: {}",
            v.get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("알 수 없는 이유")
        );
    }
    if v.get("exists").and_then(|b| b.as_bool()) == Some(false) {
        return Ok(Some((0, 0, String::new(), String::new())));
    }
    let n = |k: &str| v.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
    let t = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    Ok(Some((n("dirty"), n("unpushed"), t("origin"), t("branch"))))
}

/// 이사(agent-stop)로 학생을 내준 pane 들 — HTTP 핸들러는 App 상태(pane 의 sid
/// 주장)를 못 만지므로 여기 적어 두고, GUI 틱이 걷어 간다. 안 걷으면 세션 저장이
/// 그 대화를 「이 pane 것」으로 계속 굳혀, 재시작 복원이 **남의 기계로 이사 간
/// 대화를 다시 연다**(2026-08-30 실측: 미니 재기동이 맥북으로 간 미도리의 대화를
/// 열어 이중 열림).
static MIGRATED_AWAY: Mutex<Vec<String>> = Mutex::new(Vec::new());

pub fn note_migrated_away(pane: &str) {
    if let Ok(mut q) = MIGRATED_AWAY.lock() {
        if !q.iter().any(|p| p == pane) {
            q.push(pane.to_string());
        }
    }
}

pub fn drain_migrated_away() -> Vec<String> {
    MIGRATED_AWAY
        .lock()
        .map(|mut q| std::mem::take(&mut *q))
        .unwrap_or_default()
}

/// 원격 세션의 에이전트를 곱게 끈다. Ok(Some(bypass)) = 껐다(권한 모드 포함),
/// Ok(None) = 이미 꺼져 있었다. 창구가 없는 옛 바이너리는 Err 로 알린다 —
/// 확인 없이 progressing 하면 반쯤 산 claude 와 로컬 resume 이 같은 대화를
/// 다툰다(순방향 9-pane 사고의 원형).
pub fn remote_agent_stop(base: &str, pane: &str, token: Option<&str>) -> Result<Option<bool>> {
    let url = format!(
        "{}/term/agent-stop?pane={}",
        base.trim_end_matches('/'),
        urlencode(pane)
    );
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("http runtime")?;
    let (status, body): (u16, Vec<u8>) = rt.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .context("http client")?;
        let mut req = client.post(&url);
        if let Some(t) = token {
            req = req.header("x-kasa-token", t);
        }
        let r = req.send().await.with_context(|| format!("POST {url}"))?;
        let status = r.status().as_u16();
        Ok::<_, anyhow::Error>((status, r.bytes().await.unwrap_or_default().to_vec()))
    })?;
    if status == 404 || status == 405 {
        anyhow::bail!("그 기계의 프로그램이 낡아 곱게 끄는 창구가 없어요 — 저쪽을 갱신하고 다시");
    }
    let v: serde_json::Value = serde_json::from_slice(&body).context("agent-stop 응답 파싱")?;
    if v.get("ok").and_then(|b| b.as_bool()) != Some(true) {
        anyhow::bail!(
            "원격 에이전트 종료 실패: {}",
            v.get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("알 수 없는 이유")
        );
    }
    if v.get("stopped").and_then(|b| b.as_bool()) == Some(true) {
        Ok(Some(
            v.get("bypass").and_then(|b| b.as_bool()).unwrap_or(false),
        ))
    } else {
        Ok(None)
    }
}

/// 파일 싱크 스냅샷의 메타 — `reposync::Snapshot` 에서 bundle 만 뺀 것.
pub struct RepoSyncMeta {
    pub head: String,
    pub sync: String,
    pub branch: String,
    pub origin: String,
    pub dirty: bool,
}

/// `GET /term/repo-sync` 의 세 갈래 — 역이사가 이 결과로 길을 고른다.
pub enum RepoSyncFetch {
    /// 창구가 없는 옛 바이너리(404/405) — 옛 관문(막아 세우기)으로 물러선다.
    Unsupported,
    /// 그 기계에만 있는 것이 없다 — 실어 올 것 없이 그대로 진행.
    Nothing,
    /// 스냅샷이 실려 왔다 — 로컬 레포에 apply 하면 같은 상태가 된다.
    Bundle(RepoSyncMeta, Vec<u8>),
}

/// 원격 기계의 「그 기계에만 있는 것」을 bundle 로 받아 온다(역이사의 파일 싱크).
pub fn fetch_repo_sync(base: &str, path: &str, token: Option<&str>) -> Result<RepoSyncFetch> {
    let url = format!(
        "{}/term/repo-sync?path={}",
        base.trim_end_matches('/'),
        urlencode(path)
    );
    // bundle 뜨기는 큰 레포에서 수십 초다 — 대화 내려받기와 같은 여유를 준다.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("http runtime")?;
    let (status, headers, body): (u16, Vec<(String, String)>, Vec<u8>) = rt.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .context("http client")?;
        let mut req = client.get(&url);
        if let Some(t) = token {
            req = req.header("x-kasa-token", t);
        }
        let r = req.send().await.with_context(|| format!("GET {url}"))?;
        let status = r.status().as_u16();
        let headers = r
            .headers()
            .iter()
            .filter(|(k, _)| k.as_str().starts_with("x-kasa-") || k.as_str() == "content-type")
            .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
            .collect();
        let body = r.bytes().await.unwrap_or_default().to_vec();
        Ok::<_, anyhow::Error>((status, headers, body))
    })?;
    if status == 404 || status == 405 {
        return Ok(RepoSyncFetch::Unsupported);
    }
    let h = |k: &str| {
        headers
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    if h("content-type").starts_with("application/octet-stream") {
        return Ok(RepoSyncFetch::Bundle(
            RepoSyncMeta {
                head: h("x-kasa-head"),
                sync: h("x-kasa-sync"),
                branch: h("x-kasa-branch"),
                origin: h("x-kasa-origin"),
                dirty: h("x-kasa-dirty") == "1",
            },
            body,
        ));
    }
    let v: serde_json::Value = serde_json::from_slice(&body).context("repo-sync 응답 파싱")?;
    if v.get("nothing").and_then(|b| b.as_bool()) == Some(true) {
        return Ok(RepoSyncFetch::Nothing);
    }
    anyhow::bail!(
        "원격 스냅샷 실패: {}",
        v.get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("알 수 없는 이유")
    )
}

/// 로컬에서 떠낸 스냅샷을 원격 기계의 레포에 재현한다(순방향 이사의 파일 싱크).
/// Ok(None) = 창구가 없는 옛 바이너리 — 호출부가 옛 관문으로 물러선다.
pub fn push_repo_sync(
    base: &str,
    path: &str,
    meta: &RepoSyncMeta,
    bundle: Vec<u8>,
    token: Option<&str>,
    force: bool,
) -> Result<Option<String>> {
    let mut url = format!(
        "{}/term/repo-sync?path={}&head={}&sync={}&branch={}&dirty={}",
        base.trim_end_matches('/'),
        urlencode(path),
        urlencode(&meta.head),
        urlencode(&meta.sync),
        urlencode(&meta.branch),
        if meta.dirty { "1" } else { "0" }
    );
    if force {
        url.push_str("&force=1");
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("http runtime")?;
    let (status, body): (u16, Vec<u8>) = rt.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .context("http client")?;
        let mut req = client.post(&url).body(bundle);
        if let Some(t) = token {
            req = req.header("x-kasa-token", t);
        }
        let r = req.send().await.with_context(|| format!("POST {url}"))?;
        let status = r.status().as_u16();
        Ok::<_, anyhow::Error>((status, r.bytes().await.unwrap_or_default().to_vec()))
    })?;
    if status == 404 || status == 405 {
        return Ok(None);
    }
    let v: serde_json::Value = serde_json::from_slice(&body).context("repo-sync 응답 파싱")?;
    if v.get("ok").and_then(|b| b.as_bool()) != Some(true) {
        anyhow::bail!(
            "원격 적용 실패: {}",
            v.get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("알 수 없는 이유")
        );
    }
    Ok(Some(
        v.get("applied")
            .and_then(|a| a.as_str())
            .unwrap_or("적용됨")
            .to_string(),
    ))
}

// ── 다른 기계의 계정 칸 ─────────────────────────────────────────────────────
//
// 계정 등록·자동 전환은 **학생이 실제로 도는 기계**에서 일어나야 한다. 본진
// 디스패치를 켠 뒤로 claude 는 본진에서 태어나는데, 계정 설정·사용량 조회·전환은
// 기계마다 따로인 로컬 상태라 그대로 갈라져 있었다(2026-09-05 실측: 작업대는
// 슬롯 4개·자동전환 켜짐·80%, 본진은 슬롯 0개·꺼짐·90%). 등록을 아무리 반복해도
// 실제로 한도가 차는 기계에는 아무것도 없었다.
//
// 그래서 설정창이 본진의 계정 칸을 직접 다룬다. 자격증명 자체는 절대 옮기지
// 않는다 — Keychain 이름이 그 기계의 절대경로 해시라 기계마다 다르고, refresh
// token 은 한 번 쓰면 교체돼 두 기계에 복제하면 먼저 갱신한 쪽이 다른 쪽 로그인을
// 깨뜨린다. 오가는 것은 **목록·스위치·기준값과 OAuth 코드 한 줄**뿐이다.

/// 다른 기계 설정창의 계정 칸 한 장.
#[derive(Clone, Debug, Default)]
pub struct RemoteAccounts {
    /// `/settings/values` 의 `claude.accounts` 원문. 화면이 쓰는 모양 그대로라
    /// 여기서 다시 조립하지 않는다.
    pub accounts: Vec<serde_json::Value>,
    pub active: String,
    pub autoswitch: bool,
    pub autoswitch_pct: f32,
    /// 진행 중인 로그인 `(슬롯 id, 상태, 실패 이유)`. 상태는 `running` ·
    /// `need_code` · `ok` · `error`.
    pub login: Option<(String, String, Option<String>)>,
}

/// 그 기계의 계정 칸을 읽는다(`GET /settings/values`).
pub fn fetch_accounts(base: &str, token: Option<&str>) -> Result<RemoteAccounts> {
    let u = format!("{}/settings/values", base.trim_end_matches('/'));
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("accounts runtime")?;
    let v: serde_json::Value = rt.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(8))
            .build()
            .context("http client")?;
        let mut req = client.get(&u);
        if let Some(t) = token {
            req = req.header("x-kasa-token", t);
        }
        let r = req.send().await.context("계정 칸 조회")?;
        let text = r.text().await.unwrap_or_default();
        serde_json::from_str(&text).context("계정 칸 응답이 JSON 이 아니다")
    })?;
    let c = v.get("claude").cloned().unwrap_or_default();
    let login = c.get("login").and_then(|l| {
        let id = l.get("id")?.as_str()?.to_string();
        let state = l.get("state")?.as_str()?.to_string();
        let err = l.get("error").and_then(|e| e.as_str()).map(str::to_string);
        Some((id, state, err))
    });
    Ok(RemoteAccounts {
        accounts: c
            .get("accounts")
            .and_then(|a| a.as_array())
            .cloned()
            .unwrap_or_default(),
        active: c.get("account").and_then(|a| a.as_str()).unwrap_or("").to_string(),
        autoswitch: c.get("autoswitch").and_then(serde_json::Value::as_bool).unwrap_or(false),
        autoswitch_pct: c
            .get("autoswitch_pct")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(90.0) as f32,
        login,
    })
}

/// 그 기계 설정창의 버튼 하나를 누른다(`POST /settings/action`).
///
/// 실패 이유를 삼키지 않는다 — 「눌렀는데 아무 일도 안 남」이 이 화면에서 제일
/// 비싼 상태라서다. 그 상태로 반복해 누른 것이 애초에 이 버그의 증상이었다.
pub fn settings_action(
    base: &str,
    action: &str,
    id: Option<&str>,
    label: Option<&str>,
    token: Option<&str>,
) -> Result<serde_json::Value> {
    let u = format!("{}/settings/action", base.trim_end_matches('/'));
    let mut body = serde_json::json!({ "action": action });
    if let Some(id) = id {
        body["id"] = serde_json::Value::String(id.to_string());
    }
    if let Some(label) = label {
        body["label"] = serde_json::Value::String(label.to_string());
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("settings action runtime")?;
    let v: serde_json::Value = rt.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .context("http client")?;
        // reqwest 의 json feature 를 안 켠 빌드라 본문을 직접 만든다.
        let mut req = client
            .post(&u)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_string());
        if let Some(t) = token {
            req = req.header("x-kasa-token", t);
        }
        let r = req.send().await.context("설정 동작 요청")?;
        let status = r.status();
        let text = r.text().await.unwrap_or_default();
        Ok::<_, anyhow::Error>(serde_json::from_str(&text).unwrap_or_else(|_| {
            serde_json::json!({ "ok": false, "error": format!("HTTP {status}: {text}") })
        }))
    })?;
    if v.get("ok").and_then(serde_json::Value::as_bool) == Some(false) {
        anyhow::bail!(
            "{}",
            v.get("error").and_then(|x| x.as_str()).unwrap_or("알 수 없는 이유")
        );
    }
    Ok(v)
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

    /// 파일 싱크 전 구간 — 같은 프로세스에 서버를 띄워 「원격 기계」로 삼고,
    /// ①push_repo_sync(순방향: 로컬 스냅샷을 원격 레포에 재현) ②fetch_repo_sync
    /// (역방향: 원격의 것을 떠 와서 로컬에 apply) ③깨끗한 레포의 Nothing 까지
    /// HTTP 창구·헤더 메타·클라이언트 파싱을 한 번에 검증한다.
    /// unix 전용인 이유는 [`reposync`] 쪽 왕복 테스트와 같다 — 레포 상태를 `sh -c`
    /// 로 짓는데 Windows 엔 `sh` 가 없고, 이걸 쓰는 이사가 `#[cfg(unix)]` 다.
    #[cfg(unix)]
    #[test]
    fn repo_sync_http_roundtrip() {
        let sh = |dir: &std::path::Path, cmd: &str| -> String {
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .current_dir(dir)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "`{cmd}` 실패: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let backend: Arc<dyn kasa_socket::backend::Backend> = Arc::new(
            crate::standalone::StandaloneBackend::new(std::env::temp_dir()),
        );
        let port = crate::spawn_http_server_opts(backend, 0, false).expect("server");
        let base = format!("http://127.0.0.1:{port}");
        let root =
            std::env::temp_dir().join(format!("kasaterm-reposync-http-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let ident = "-c user.name=t -c user.email=t@t";
        sh(&root, "git init --bare origin.git");
        sh(&root, &format!(
            "git clone origin.git src 2>/dev/null && cd src && echo one > a.txt && git add -A && git {ident} commit -qm init && git push -q origin HEAD"
        ));
        sh(&root, "git clone origin.git dst 2>/dev/null");
        sh(&root, "git clone origin.git back 2>/dev/null");
        sh(&root, &format!(
            "cd src && echo two >> a.txt && git add -A && git {ident} commit -qm local-only && echo three >> a.txt && echo new > c.txt"
        ));
        let (src, dst, back) = (root.join("src"), root.join("dst"), root.join("back"));
        // ① 순방향: src 스냅샷 → HTTP POST → 서버가 dst 에 재현.
        let snap = crate::reposync::snapshot(&src)
            .unwrap()
            .expect("실어 갈 것");
        let meta = RepoSyncMeta {
            head: snap.head.clone(),
            sync: snap.sync,
            branch: snap.branch,
            origin: snap.origin,
            dirty: snap.dirty,
        };
        let applied = push_repo_sync(
            &base,
            &dst.to_string_lossy(),
            &meta,
            snap.bundle,
            None,
            false,
        )
        .expect("push")
        .expect("창구가 있어야 한다");
        assert!(applied.contains("미저장"), "applied={applied}");
        assert_eq!(sh(&dst, "git rev-parse HEAD"), snap.head);
        assert_eq!(sh(&dst, "cat a.txt"), "one\ntwo\nthree");
        assert_eq!(sh(&dst, "cat c.txt"), "new");
        // ② 역방향: 방금 미push+미커밋이 생긴 dst 를 GET 으로 떠 와 back 에 재현.
        match fetch_repo_sync(&base, &dst.to_string_lossy(), None).expect("fetch") {
            RepoSyncFetch::Bundle(m, bytes) => {
                assert_eq!(m.head, snap.head);
                assert!(m.dirty);
                let msg = crate::reposync::apply(
                    &back,
                    &bytes,
                    &m.head,
                    &m.sync,
                    &m.branch,
                    m.dirty,
                    false,
                    crate::reposync::OnBlock::Bail,
                )
                .expect("apply");
                assert!(msg.contains("미저장"), "msg={msg}");
                assert_eq!(sh(&back, "cat a.txt"), "one\ntwo\nthree");
                assert_eq!(sh(&back, "cat c.txt"), "new");
            }
            _ => panic!("Bundle 이 와야 한다"),
        }
        // ③ 깨끗한 레포는 Nothing — 실어 올 것이 없다는 판정도 창구를 거쳐 온다.
        sh(&root, "git clone origin.git clean 2>/dev/null");
        assert!(matches!(
            fetch_repo_sync(&base, &root.join("clean").to_string_lossy(), None).expect("fetch"),
            RepoSyncFetch::Nothing
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 같은 프로세스에 서버(spawn_http_server_opts)와 클라이언트를 함께 띄워
    /// 전 구간을 돈다: 스폰 → 타이핑 → 둘째 클라이언트 이어받기(스냅샷) → kill.
    #[test]
    fn remote_spawn_type_reattach_kill_roundtrip() {
        let backend: Arc<dyn kasa_socket::backend::Backend> = Arc::new(
            crate::standalone::StandaloneBackend::new(std::env::temp_dir()),
        );
        let port = crate::spawn_http_server_opts(backend, 0, false).expect("server");
        let spec = RemoteSpec {
            base: format!("http://127.0.0.1:{port}"),
            pane: None,
            cwd: None,
            token: None,
            identity: Default::default(),
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

    /// 거울(뷰어)은 원본 세션의 격자를 **어떤 경로로도** 못 바꾼다 — 붙는 순간의
    /// 맞춤 resize 도, 그 뒤 pane 을 줄여 생기는 resize 도. 대조군으로 소유자
    /// 연결(connect)은 여전히 바꾼다. 서버 격자는 「아무것도 안 바꾸는」 거울을
    /// 하나 더 붙여 그 size 핸드셰이크로 읽는다.
    #[test]
    fn view_link_never_resizes_origin() {
        let backend: Arc<dyn kasa_socket::backend::Backend> = Arc::new(
            crate::standalone::StandaloneBackend::new(std::env::temp_dir()),
        );
        let port = crate::spawn_http_server_opts(backend, 0, false).expect("server");
        let spec = RemoteSpec {
            base: format!("http://127.0.0.1:{port}"),
            pane: None,
            cwd: None,
            token: None,
            identity: Default::default(),
        };
        let owner = connect(spec.clone(), "%vw0", 100, 30).expect("connect");
        // 서버 격자 읽기: 아무것도 안 바꾸는 거울을 하나 붙여 size 핸드셰이크만 본다.
        fn server_size(spec: &RemoteSpec, remote_id: &str, tag: &str) -> (u16, u16) {
            connect_view(
                RemoteSpec {
                    pane: Some(remote_id.to_string()),
                    ..spec.clone()
                },
                tag,
            )
            .expect("probe")
            .session
            .size()
        }
        fn settle(spec: &RemoteSpec, remote_id: &str, want: (u16, u16), tag: &str) -> (u16, u16) {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut n = 0u32;
            let mut got = server_size(spec, remote_id, &format!("{tag}{n}"));
            while got != want && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(150));
                n += 1;
                got = server_size(spec, remote_id, &format!("{tag}{n}"));
            }
            got
        }
        let rid = owner.remote_id.clone();
        // 소유자가 세운 격자가 기준선.
        assert_eq!(
            settle(&spec, &rid, (100, 30), "%vwa"),
            (100, 30),
            "소유자 resize 가 안 먹었다"
        );

        // 거울로 붙는다 — 붙는 것만으로 격자가 흔들리면 안 된다.
        let mirror = connect_view(
            RemoteSpec {
                pane: Some(rid.clone()),
                ..spec.clone()
            },
            "%vw1",
        )
        .expect("view");
        assert!(is_view_pane("%vw1"));
        assert!(!is_view_pane("%vw0"), "소유자 연결이 거울로 오판됐다");
        assert_eq!(
            mirror.session.size(),
            (100, 30),
            "거울이 서버 격자를 못 받았다"
        );

        // 거울 pane 을 줄인다(로컬 창 축소와 같은 길) — 원본은 그대로여야 한다.
        let _ = mirror.session.resize(40, 10);
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(
            server_size(&spec, &rid, "%vwb"),
            (100, 30),
            "거울이 원본 격자를 뺏었다"
        );

        // 대조군: 주인은 여전히 바꿀 수 있다.
        let _ = owner.session.resize(70, 20);
        assert_eq!(
            settle(&spec, &rid, (70, 20), "%vwc"),
            (70, 20),
            "소유자 resize 가 막혀 버렸다"
        );
        assert!(kill_remote("%vw0"));
    }

    #[test]
    fn build_url_encodes_percent_pane_and_cwd() {
        let spec = RemoteSpec {
            base: "http://127.0.0.1:8766".into(),
            pane: Some("%12".into()),
            cwd: None,
            token: None,
            identity: Default::default(),
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
            identity: Default::default(),
        };
        let url = build_url(&spec, None);
        assert!(
            url.contains("cwd=/Users/miku/%ED%95%9C%EA%B8%80%20%ED%8F%B4%EB%8D%94"),
            "{url}"
        );
        assert!(url.ends_with("&t=tok"));
    }
}
