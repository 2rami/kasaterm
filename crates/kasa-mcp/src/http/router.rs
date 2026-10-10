//! 서버 띄우기와 라우트 표 — 백그라운드 tokio 런타임, 입구 셋(본 포트·업링크·폰), 본체만 도는 상주 루프,
//! 공통 레이어(Origin·토큰 가드 → 폰 주소 접두 벗기기).

use super::*;
use super::auth::{bind_addr, mobile_prefix_mw, via_phone_mw, via_uplink_mw};
use super::machine_proxy::PROXY_BODY_LIMIT;
use super::schedule::schedule_loop;

/// Bind the host HTTP server at `127.0.0.1:<port>` and run it on a background
/// thread. Tries `preferred_port` first, then falls back to an OS-assigned
/// port. Returns the actual port bound so the host can export it as an env var.
pub fn spawn_http_server(
    backend: Arc<dyn Backend>,
    preferred_port: u16,
) -> std::io::Result<u16> {
    spawn_http_server_opts(backend, preferred_port, true)
}

/// Like [`spawn_http_server`] but lets the caller disable the schedule loop.
/// The standalone webview server (`kasa-serve-web`) passes `run_scheduler=false`:
/// a headless backend can't deliver a due reminder (`send_text` bails), yet the
/// loop would still persist the item as consumed in the SHARED
/// `~/.config/kasaterm/schedule.json` (silent reminder loss), append phantom
/// "sensei" bubbles, and race kasaterm's own loop on the same file. Firing and
/// consuming schedules is kasaterm's job alone.
pub fn spawn_http_server_opts(
    backend: Arc<dyn Backend>,
    preferred_port: u16,
    run_scheduler: bool,
) -> std::io::Result<u16> {
    crate::install_collab_env();
    // Bind synchronously so we can learn (and return) the real port before
    // handing the socket to tokio.
    let addr = bind_addr();
    let listener = std::net::TcpListener::bind((addr.as_str(), preferred_port))
        .or_else(|_| std::net::TcpListener::bind((addr.as_str(), 0)))?;
    let port = listener.local_addr()?.port();
    listener.set_nonblocking(true)?;
    // 업링크 전용 입구 — 관문을 거친 요청은 여기로만 들어와 `ViaUplink` 표식을 단다. 본체는
    // 늘, standalone 은 리그가 `KASATERM_GATEWAY` 로 로컬 관문을 가리켰을 때만(사용자 관문에
    // 가짜 기계를 올리지 않게).
    let uplink_ingress = if run_scheduler || std::env::var_os("KASATERM_GATEWAY").is_some() {
        let l = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        l.set_nonblocking(true)?;
        Some(l)
    } else {
        None
    };
    // 카사넷 폰 입구 — 폰 연결은 HTTP 포트 대신 여기로 돌려진다(`kasanet::allow_phone`). 카사넷과 같이 본체만.
    let phone_ingress = if run_scheduler {
        let l = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        l.set_nonblocking(true)?;
        crate::kasanet::set_phone_ingress(l.local_addr()?.port());
        Some(l)
    } else {
        None
    };
    let collab_collector = match crate::board_service::register(backend.clone(),port) {
        Ok(collector) => Some(collector),
        Err(_) => { eprintln!("[collaboration] observer unavailable"); None },
    };
    // 무중단 핸드오프 입양 창구 — HTTP 포트와 짝지은 unix 소켓. 실패해도 서버는
    // 계속 뜬다(핸드오프만 못 받을 뿐).
    #[cfg(unix)]
    if let Err(e) = crate::adopt::spawn_adopt_listener(port) {
        eprintln!("[adopt] 입양 소켓을 못 열었습니다: {e:#}");
    }
    if !matches!(addr.as_str(), "127.0.0.1" | "localhost" | "::1") {
        // 여는 순간 토큰이 유일한 방어다. 어디서 얻는지를 로그에 남겨 두지 않으면
        // 「왜 403 이냐」로 헤매다 결국 토큰을 끄는 쪽으로 가게 된다.
        eprintln!(
            "[kasaspace-mcp] {addr}:{port} 로 열었습니다 — 원격 접속에는 토큰이 필요합니다.\n\
             [kasaspace-mcp]   http://<이 기기의 주소>:{port}/term/grid?t=$(cat ~/.config/kasaterm/remote-token)"
        );
        // 파일을 미리 만들어 둔다 — 첫 원격 요청 때 만들면 그 요청이 먼저 튕긴다.
        let _ = remote_token();
    }

    std::thread::Builder::new()
        .name("kasaspace-mcp-http".into())
        .spawn(move || {
            let _collab_collector = collab_collector;
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("[kasaspace-mcp] tokio runtime build failed: {e}");
                    return;
                }
            };
            rt.block_on(async move {
                let tokio_listener = match tokio::net::TcpListener::from_std(listener) {
                    Ok(l) => l,
                    Err(e) => {
                        eprintln!("[kasaspace-mcp] listener convert failed: {e}");
                        return;
                    }
                };
                // 스케줄러 백그라운드 타이머 — due 항목을 학생에게 발사(10s 주기). standalone
                // (run_scheduler=false)은 공유 schedule.json 을 소비/영속하면 안 됨(유령버블·유실·레이스).
                if run_scheduler {
                    tokio::spawn(schedule_loop(backend.clone()));
                    // 학생 자동 호출 — 큐를 보고 빈 학생에게 배정하고 없으면 스폰(10s 주기).
                    // standalone 제외 이유는 scheduler 와 같다: 공유 queue.json 을 두 곳이
                    // 뮤테이트하면 배정이 사라지거나 이중 배달된다. PTY 를 가진 본체만 쓴다.
                    tokio::spawn(crate::dispatch::dispatch_loop(backend.clone()));
                    // /resume 가시성 스위퍼 — 팀 세션 transcript 의 teamName 마커를
                    // 같은 길이 키로 중화해 claude /resume 피커에 되살리고, 학생
                    // 바인딩은 #태그로 스탬프한다(부팅 직후 + 60초 주기). standalone
                    // 제외 이유는 scheduler 와 동일(공유 파일 뮤테이터는 본체 1곳만).
                    tokio::spawn(crate::resume_visibility::sweep_loop());
                    // 다른 기계 board 를 미리 받아 두는 루프. 원격이 설정 안 됐으면
                    // 루프 자체가 안 돈다. standalone 을 빼는 이유는 위 셋과 다르다 —
                    // 공유 파일이 아니라 **순환**이다. 서로를 원격으로 걸면 board 가
                    // 서로를 물어 무한히 부푼다. 합치는 쪽은 본체 한 곳이면 된다.
                    tokio::spawn(crate::remoteboard::poll_loop());
                    // 기계 명부(machines.json) 폴링 — 이사 탭이 기계별 학생 목록을
                    // 즉시 그리게 미리 받아 둔다. 같은 순환 이유로 본체 한정.
                    // 카사넷 엔드포인트 — 폴링이 상대 id 를 배우기 전에 떠 있어야 입구가 선다.
                    tokio::spawn(crate::kasanet::start(port));
                    tokio::spawn(crate::machines::poll_loop());
                    // ssh 만 적힌 기계의 8765 터널을 앱이 든다(설정 화면이 적는 항목).
                    tokio::spawn(crate::machines::tunnel_loop());
                    // 폰 푸시 — 학생 상태 변화(대기·끝냄)를 보고 쏜다. 기계 캐시를 합쳐
                    // 보므로 순환 이유로 본체 한정.
                    tokio::spawn(crate::push::push_loop());
                    // KASA-share — 결과물 폴더를 기기끼리 맞춘다. 두 벌이 같은 폴더를 돌리면
                    // 서로의 판을 새 고침으로 읽으므로 본체 한정.
                    tokio::spawn(crate::share::run());
                }
                // 업링크 — 관문에 붙어 폰 주소를 살린다(uplink.rs). 되쏘는 곳은 전용 입구다.
                let uplink_ingress = uplink_ingress.and_then(|l| {
                    let ingress_port = l.local_addr().ok()?.port();
                    crate::uplink::spawn(ingress_port);
                    tokio::net::TcpListener::from_std(l).ok()
                });
                // ccglass-style 캡처 프록시 — claude 의 Anthropic API 호출을 가로채
                // pane 별 대화(messages[]+SSE)를 모은다. /conversation 으로 노출.
                let conv_store: crate::proxy::ConvStore = Default::default();
                let http_client = reqwest::Client::new();
                let app = axum::Router::new()
                    .merge(git_api::routes(&backend))
                    .merge(sessions::routes(&backend))
                    .merge(collab::routes(&backend))
                    .merge(host::routes(&backend))
                    .merge(migrate::routes(&backend))
                    .merge(pane_read::routes(&backend))
                    .merge(panes::routes(&backend))
                    .merge(settings::routes(&backend))
                    .merge(term_assets::routes())
                    .merge(term_api::routes(&backend))
                    .merge(term_files::routes(&backend))
                    .merge(socket::routes(&backend))
                    .merge(phone::routes())
                    .merge(machine_proxy::routes())
                    .merge(arona_ui::routes(&backend))
                    .merge(files::routes(&backend))
                    .merge(claude_account::routes())
                    .merge(schedule::routes(&backend))
                    .merge(tasks::routes(&backend))
                    .route("/term/share/manifest", get(crate::share::serve::manifest))
                    .route("/term/share/file", get(crate::share::serve::file))
                    .route("/term/share/f/{*path}", get(crate::share::serve::file_at))
                    .route("/term/share/list", get(crate::share::serve::list))
                    .route("/net/tcp", get(crate::netfwd::tcp_ws_handler))
                    .route("/term/blocks", get(crate::shell_blocks::term_blocks_get))
                    .merge(crate::claude_mod::routes())
                    .route("/browser/resolve-localhost", post(crate::browser_route::resolve_handler))
                    // 채팅 소스: 캡처 프록시가 모은 pane 대화(turns + 진행 중 streaming).
                    .route(
                        "/conversation",
                        get({
                            let store = conv_store.clone();
                            move |q: Query<std::collections::HashMap<String, String>>| {
                                let store = store.clone();
                                async move {
                                    let pane =
                                        q.get("surface").map(|s| s.trim_start_matches('%')).unwrap_or("");
                                    let cors = [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")];
                                    (cors, Json(crate::proxy::conversation_json(&store, pane)))
                                        .into_response()
                                }
                            }
                        }),
                    )
                    // 캡처 프록시: claude 가 ANTHROPIC_BASE_URL=…/p/<pane> 로 보낸 모든
                    // API 호출을 가로채 api.anthropic.com 으로 투명 포워드 + 캡처.
                    .route(
                        "/p/{pane}/{*rest}",
                        any({
                            let store = conv_store.clone();
                            let client = http_client.clone();
                            move |AxPath((pane, rest)): AxPath<(String, String)>,
                                  method: Method,
                                  headers: HeaderMap,
                                  body: Bytes| {
                                crate::proxy::proxy_handler(
                                    store.clone(),
                                    client.clone(),
                                    pane,
                                    rest,
                                    method,
                                    headers,
                                    body,
                                )
                            }
                        })
                        // axum 의 `Bytes` 추출기는 기본 2MB 에서 413 을 낸다. 긴 claude
                        // 대화는 요청 몸통이 2MB 를 넘고, 그러면 API 에 닿기도 전에
                        // 관문이 막아 claude 가 「Request too large (max 32MB)」로
                        // 오판했다(2026-09-16 실측). 상류 한도는 상류가 판정하게 둔다.
                        .layer(axum::extract::DefaultBodyLimit::max(PROXY_BODY_LIMIT)),
                    )
                    // 부작용 있는 요청에 두르는 마지막 한 겹. 라우트마다 손으로
                    // 거는 대신 레이어로 걸어야 **새로 추가될 라우트도 자동으로**
                    // 보호된다 — 31개 중 하나를 빠뜨리면 그게 곧 구멍이다.
                    .layer(axum::middleware::from_fn(origin_guard_mw));
                // 유저별 주소 접두(`/u/<slug>/`)는 라우팅 **앞**에서 벗겨야 한다 —
                // `Router::layer` 는 라우팅 뒤라 그 경로가 먼저 404 를 맞는다.
                let app = tower::ServiceBuilder::new()
                    .layer(axum::middleware::from_fn(mobile_prefix_mw))
                    .service(app);
                use axum::ServiceExt as _;
                if let Some(ingress) = uplink_ingress {
                    let via = tower::ServiceBuilder::new()
                        .layer(axum::middleware::from_fn(via_uplink_mw))
                        .service(app.clone());
                    tokio::spawn(async move {
                        if let Err(e) = axum::serve(ingress, via.into_make_service_with_connect_info::<std::net::SocketAddr>()).await {
                            eprintln!("[kasaspace-mcp] uplink ingress serve error: {e}");
                        }
                    });
                }
                if let Some(ingress) = phone_ingress.and_then(|l| tokio::net::TcpListener::from_std(l).ok()) {
                    let via = tower::ServiceBuilder::new()
                        .layer(axum::middleware::from_fn(via_phone_mw))
                        .service(app.clone());
                    tokio::spawn(async move {
                        if let Err(e) = axum::serve(ingress, via.into_make_service_with_connect_info::<std::net::SocketAddr>()).await {
                            eprintln!("[kasaspace-mcp] phone ingress serve error: {e}");
                        }
                    });
                }
                // ConnectInfo 를 붙여야 `origin_guard_mw` 가 peer 주소를 보고
                // 로컬/원격을 가를 수 있다. 이게 없으면 원격도 로컬 규칙을 타서
                // 토큰 없이 통과한다.
                if let Err(e) = axum::serve(
                    tokio_listener,
                    app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
                )
                .await
                {
                    eprintln!("[kasaspace-mcp] serve error: {e}");
                }
            });
        })?;

    Ok(port)
}
