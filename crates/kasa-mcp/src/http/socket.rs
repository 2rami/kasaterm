//! 웹 터미널 WebSocket(`/term/ws`·`/term/layout/ws`) — raw 바이트(자기 VT 파서를 든 xterm.js)와
//! 셀 그리드(파서 없는 웹·폰) 두 갈래, 뷰어 크기 임대, 세션 교체 감지.
//!
//! 한 라우트에 두 모드가 있다:
//!   (파라미터 없음)  웹 전용 셸을 **새로 띄운다**. 연결이 끊기면 Arc 가 떨어져
//!                    셸도 함께 끝난다 — 브라우저 탭이 곧 세션 수명이다.
//!   ?pane=%1         기존 kasaterm pane 을 **미러**한다(같은 PTY 를 함께 본다).

use super::*;
#[cfg(test)]
use super::term_assets::{term_visual_asset, term_visual_builtin};

/// pane id 를 **디코딩하지 않은 원문**으로 꺼낸다.
///
/// pane id 는 `%1` 처럼 `%` 로 시작한다. 그래서 주소창에 `?pane=%116` 을 그대로 치면
/// 퍼센트 인코딩으로 해석돼 `%11`(제어문자) + `6` 이 되고, 조회가 조용히 실패한다 —
/// 화면에는 아무것도 안 뜨고 연결만 끊겨서 원인을 짐작하기 어렵다. 사람이 흔히
/// 밟는 함정이라, 디코딩된 값으로 못 찾으면 이 원문으로 한 번 더 본다.
fn raw_pane_param(raw: Option<&str>) -> Option<String> {
    raw?.split('&')
        .find_map(|kv| kv.strip_prefix("pane="))
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// 거울에게 한 번에 주는 지난 줄의 상한 — 폰은 이 이상 넘겨 볼 일이 드물고, 터널 너머로
/// 한 프레임에 실리는 크기를 묶어 둔다.
const HISTORY_ROWS_MAX: usize = 2000;

/// 웹 거울이 접을 원본 격자 — 원본 폭 그대로의 전체 행과 마지막 프레임의 나머지
/// 정보(커서·모드). 프레임은 바뀐 행만 실어 오므로 여기 얹어 전체를 유지한다.
struct WebSrc {
    cells: Vec<kasa_screen::screen::Row>,
    meta: kasa_screen::screen::ScreenUpdate,
}

impl WebSrc {
    fn from_full(u: &kasa_screen::screen::ScreenUpdate) -> Self {
        let mut s = WebSrc {
            cells: vec![
                vec![kasa_screen::screen::Cell::blank(); u.cols as usize];
                u.rows as usize
            ],
            meta: Self::meta_of(u),
        };
        s.absorb(u);
        s
    }
    fn meta_of(u: &kasa_screen::screen::ScreenUpdate) -> kasa_screen::screen::ScreenUpdate {
        let mut m = u.clone();
        m.dirty = Vec::new();
        m
    }
    fn absorb(&mut self, u: &kasa_screen::screen::ScreenUpdate) {
        if self.meta.cols != u.cols || self.meta.rows != u.rows || self.cells.len() != u.rows as usize {
            self.cells = vec![
                vec![kasa_screen::screen::Cell::blank(); u.cols as usize];
                u.rows as usize
            ];
        }
        for (r, row) in &u.dirty {
            if let Some(dst) = self.cells.get_mut(*r as usize) {
                *dst = row.clone();
            }
        }
        self.meta = Self::meta_of(u);
    }
    /// 원본 폭 그대로, 모든 행을 실은 프레임.
    fn full_raw(&self, glyphs: bool) -> String {
        crate::gridwire::encode_with_glyphs(&self.full_snapshot(), glyphs).to_string()
    }
    fn full_snapshot(&self) -> kasa_screen::screen::ScreenUpdate {
        let mut u = self.meta.clone();
        u.dirty = self.cells.iter().cloned().enumerate().map(|(i, r)| (i as u16, r)).collect();
        u
    }
    /// `cols` 폭으로 다시 접은 프레임 — 행 수는 접힌 줄 수(원본 행 수보다 적으면 그만큼 채움).
    fn reflowed(&self, cols: u16, glyphs: bool) -> String {
        let out = kasa_screen::reflow::reflow_lines(
            &self.cells,
            self.meta.cols,
            (self.meta.cursor_row, self.meta.cursor_col),
            cols,
        );
        let mut rows = out.rows;
        let min_rows = self.meta.rows as usize;
        while rows.len() < min_rows {
            rows.push(vec![kasa_screen::screen::Cell::blank(); cols.max(4) as usize]);
        }
        let mut u = self.meta.clone();
        u.cols = cols.max(4);
        u.rows = rows.len() as u16;
        u.cursor_row = out.cursor_row.unwrap_or(0);
        u.cursor_col = out.cursor_col;
        u.dirty = rows.into_iter().enumerate().map(|(i, r)| (i as u16, r)).collect();
        crate::gridwire::encode_with_glyphs(&u, glyphs).to_string()
    }
}

/// 지난 줄(스크롤백)을 실어 보낼 때 — 거울 폭이 정해져 있으면 행마다 그 폭으로 접는다
/// (이웃과 되잇지는 않는다). 오래된 순으로 준다.
fn encode_history_rows(
    rows: &[kasa_screen::screen::Row],
    src_cols: u16,
    view_cols: u16,
    glyphs: bool,
) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for r in rows.iter().rev() {
        if view_cols > 0 && view_cols != src_cols {
            for line in kasa_screen::reflow::reflow_row(r, src_cols, view_cols) {
                out.push(crate::gridwire::encode_row_with_glyphs(&line, glyphs));
            }
        } else {
            out.push(crate::gridwire::encode_row_with_glyphs(r, glyphs));
        }
    }
    out
}

#[derive(Clone, Copy, PartialEq)]
enum VisualEvent { Raw, Published, Deadline }

#[derive(Default)]
struct VisualDelivery {
    glyphs: bool,
    last_scene: Option<u64>,
    last_dimensions: Option<(u16, u16)>,
    pending_since: Option<std::time::Instant>,
    fallback: bool,
}

impl VisualDelivery {
    fn deadline(&self) -> Option<tokio::time::Instant> {
        self.pending_since.map(|since| (since + std::time::Duration::from_millis(500)).into())
    }

    fn encode(
        &mut self,
        raw: &kasa_screen::screen::ScreenUpdate,
        view_cols: u16,
        event: VisualEvent,
    ) -> Option<String> {
        let now = std::time::Instant::now();
        let dimensions = (raw.cols, raw.rows);
        if view_cols > 0 && view_cols != raw.cols {
            self.last_scene = None;
            self.last_dimensions = None;
            self.pending_since = None;
            self.fallback = false;
            let mut value: serde_json::Value = serde_json::from_str(&WebSrc::from_full(raw).reflowed(view_cols, self.glyphs)).unwrap();
            value["scene"] = serde_json::Value::Null;
            return Some(value.to_string());
        }
        let scene = crate::visual::latest_scene(&raw.pane_id, raw.cols, raw.rows);
        if let Some(scene) = scene.as_ref().filter(|scene| {
            self.last_scene.is_none_or(|revision| scene.scene_revision > revision)
        }) {
            self.last_scene = Some(scene.scene_revision);
            self.last_dimensions = Some(dimensions);
            self.fallback = false;
            // A completed frame is valid even when more PTY output is already pending.
            self.pending_since = (crate::visual::source_key(raw, 0).as_deref()
                != Some(scene.source_key.as_str())).then_some(now);
            return Some(crate::gridwire::encode_scene_with_glyphs(scene, self.glyphs).to_string());
        }
        if self.last_dimensions.is_some_and(|old| old != dimensions) {
            self.last_dimensions = Some(dimensions);
            self.last_scene = None;
            self.pending_since = Some(now);
            self.fallback = false;
            return Some(crate::gridwire::encode_raw_visual_with_glyphs(raw, self.glyphs).to_string());
        }
        if event == VisualEvent::Deadline {
            // Unchanged raw notifications do not imply that the producer has stalled.
            if scene.as_ref().is_some_and(|scene| self.last_scene == Some(scene.scene_revision)
                && crate::visual::source_key(raw, 0).as_deref() == Some(scene.source_key.as_str())) {
                self.pending_since = None;
                return None;
            }
            self.pending_since = None;
            self.fallback = true;
        }
        if self.fallback {
            if event == VisualEvent::Published { return None; }
            self.last_dimensions = Some(dimensions);
            return Some(crate::gridwire::encode_raw_visual_with_glyphs(raw, self.glyphs).to_string());
        }
        self.pending_since.get_or_insert(now);
        None
    }
}

/// 브라우저로 나가는 한 프레임. 원시 바이트(자기 VT 파서를 든 클라)와 셀 그리드
/// (파서 없이 그리는 클라)를 한 채널로 흘리려고 묶었다.
enum Frame {
    Bytes(Vec<u8>),
    Grid(Box<kasa_screen::screen::ScreenUpdate>),
    /// 거울 클라가 제 폭(`view`)을 바꿨다 — 새 프레임이 없어도 원본을 그 폭으로 다시
    /// 접어 통째로 보낸다.
    Reflow,
    Visual,
    VisualDeadline,
    /// 호스트 GUI 가 거울에게 미는 제어 JSON(`{"t":"open-url",…}` 등). 화면과
    /// 같은 채널을 타야 순서가 보장되고, 송신자(`btx`)를 등록부에 두는 것만으로
    /// 「이 pane 을 보는 거울 전부」에 닿는다.
    Control(String),
}

/// raw 구독 연결이 **자기 viewport 요청으로** 바꾼 격자. 출력 쪽은 격자가 바뀌면
/// 「남이 바꿨다」고 보고 연결을 닫아 재접속으로 스냅샷을 새로 받게 하는데, 자기가
/// 바꾼 것까지 그렇게 하면 acquire 는 소유권을 놓고 release 는 화면을 깜빡인다 —
/// 그 경우엔 입력 쪽이 같은 채널로 size+스냅샷을 넣으니 닫을 이유가 없다.
#[derive(Clone, Copy)]
enum SelfSized {
    Idle,
    /// 요청을 처리하는 중 — 격자가 바뀌어도 아직 무엇으로 바뀔지 모른다.
    Pending,
    At((u16, u16)),
}

/// pane 별 거울 제어 송신자 — `term_ws_run` 이 거울(mirrored)로 붙을 때 등록하고
/// 끝날 때 뺀다. 호스트가 「이 pane 을 누가 보고 있나」를 아는 유일한 창구다.
///
/// 쓰임: 원격(본진) 학생이 브라우저를 열면 그 기계 크롬이 아니라 **보고 있는
/// 사람의 기계**에서 열려야 한다(2026-09-02 「맥미니 세션으로 브라우저 켜면
/// 맥미니에서 켜져서 화면공유로 봐야」). VS Code Remote 의 브라우저 되돌리기와
/// 같은 원리 — 거울이 있으면 거울 쪽으로, 없으면 호스트가 직접 연다.
fn viewer_ctls(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, Vec<(u64, tokio::sync::mpsc::Sender<Frame>)>>>
{
    static V: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, Vec<(u64, tokio::sync::mpsc::Sender<Frame>)>>>,
    > = std::sync::OnceLock::new();
    V.get_or_init(Default::default)
}

fn register_viewer_ctl(pane: &str, tx: tokio::sync::mpsc::Sender<Frame>) -> u64 {
    static TOKEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let token = TOKEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if let Ok(mut g) = viewer_ctls().lock() {
        g.entry(pane.to_string()).or_default().push((token, tx));
    }
    token
}

fn unregister_viewer_ctl(pane: &str, token: u64) {
    if let Ok(mut g) = viewer_ctls().lock() {
        if let Some(v) = g.get_mut(pane) {
            v.retain(|(t, _)| *t != token);
            if v.is_empty() {
                g.remove(pane);
            }
        }
    }
}

/// `pane` 을 거울로 보는 모든 접속에 제어 JSON 한 줄을 민다. 닿은 거울 수를
/// 돌려준다 — 0 이면 아무도 안 보고 있으니 호스트가 스스로 처리해야 한다.
pub fn push_viewer_control(pane: &str, text: &str) -> usize {
    let Ok(mut g) = viewer_ctls().lock() else { return 0 };
    let Some(v) = g.get_mut(pane) else { return 0 };
    v.retain(|(_, tx)| !tx.is_closed());
    let n = v
        .iter()
        .filter(|(_, tx)| tx.try_send(Frame::Control(text.to_string())).is_ok())
        .count();
    if v.is_empty() {
        g.remove(pane);
    }
    n
}

/// 구독 시작 시점의 "지금 화면"과 이후 스트림. 둘을 한 락에서 받아야 그 사이
/// 프레임이 유실되지 않는다(`tap_bytes_with_snapshot` 주석).
enum Tap {
    Bytes(kasa_pty::ScreenReceiver<Vec<u8>>, Vec<u8>, (u16, u16)),
    Grid(
        kasa_pty::ScreenReceiver<kasa_screen::screen::ScreenUpdate>,
        Box<kasa_screen::screen::ScreenUpdate>,
    ),
}

struct ViewerViewport {
    session: Arc<kasa_pty::PtySession>,
    token: u64,
}

impl ViewerViewport {
    fn new(session: Arc<kasa_pty::PtySession>) -> Self {
        let token = session.open_viewer_size();
        Self { session, token }
    }
}

impl Drop for ViewerViewport {
    fn drop(&mut self) {
        // Cancellation can skip the normal disconnect tail; the lease must still expire.
        let _ = self.session.close_viewer_size(self.token);
    }
}

fn viewport_dimensions(v: &serde_json::Value) -> Option<(u16, u16)> {
    let cols = v.get("cols")?.as_u64()?;
    let rows = v.get("rows")?.as_u64()?;
    // Bound allocation before integer conversion; a wrapped dimension is a different request.
    if !(2..=1000).contains(&cols) || !(1..=1000).contains(&rows) {
        return None;
    }
    Some((cols as u16, rows as u16))
}

/// 방 배치 채널 — 거울이 붙어 트리를 받고 분할선 명령을 보낸다(`layout_feed`).
async fn term_layout_ws_handler(
    backend: Arc<dyn Backend>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> axum::response::Response {
    if !ws_origin_ok(&headers) {
        return (axum::http::StatusCode::FORBIDDEN, "cross-origin websocket refused").into_response();
    }
    ws.on_upgrade(move |socket| crate::layout_feed::serve(socket, backend)).into_response()
}

async fn term_ws_handler(
    headers: HeaderMap,
    ws: WebSocketUpgrade,
    Query(q): Query<std::collections::HashMap<String, String>>,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
) -> axum::response::Response {
    if !ws_origin_ok(&headers) {
        eprintln!(
            "[term-ws] 교차 출처 연결을 거부했습니다: {:?}",
            headers.get(header::ORIGIN)
        );
        return (
            axum::http::StatusCode::FORBIDDEN,
            "cross-origin websocket refused",
        )
            .into_response();
    }
    let pane = q.get("pane").cloned().unwrap_or_default();
    let pane_raw = raw_pane_param(raw.as_deref());
    let cwd = q.get("cwd").cloned();
    // 셀 그리드로 받을지(웹텀 자체 렌더) 원시 바이트로 받을지.
    let grid = q.get("grid").map_or(false, |v| v == "1" || v == "true");
    let glyphs = grid && q.get("glyphs").is_some_and(|v| v == "1" || v == "true");
    // own=1 — 이 연결이 pane 의 **소유자**(원격 kasaterm GUI)다. 미러 규칙 셋이
    // 뒤집힌다: resize 를 force 없이 받고, 끊겨도 격자를 되돌리지 않으며(소유자가
    // 정한 크기가 곧 원본), kill 제어 메시지를 받는다.
    let own = q.get("own").map_or(false, |v| v == "1" || v == "true");
    let expected_surface_key = q.get("surface_key").cloned();
    ws.on_upgrade(move |socket| term_ws_run(socket, pane, pane_raw, cwd, grid, own, glyphs, expected_surface_key))
        .into_response()
}

async fn term_ws_run(
    mut socket: WebSocket,
    pane: String,
    pane_raw: Option<String>,
    cwd: Option<String>,
    want_grid: bool,
    own: bool,
    glyphs: bool,
    expected_surface_key: Option<String>,
) {
    use futures_util::{SinkExt, StreamExt};
    // 미러냐 새 셸이냐. 새 셸의 pane_id 는 kasaterm 의 "%n" 과 겹치면 안 된다
    // (레지스트리 키 충돌) — 웹 전용 접두사를 붙인다.
    let (sess, mirrored, self_id) = if pane.is_empty() {
        let id = format!("web-{}", uuid::Uuid::new_v4());
        let opts = kasa_pty::PtyOptions {
            cwd: cwd.or_else(|| kasa_socket::home_dir().map(|p| p.display().to_string())),
            cols: 80,
            rows: 24,
            pane_id: id.clone(),
            ..Default::default()
        };
        match kasa_pty::PtySession::start(opts) {
            Ok(s) => {
                let sess = std::sync::Arc::new(s);
                // 목록(`/term/panes`)에 띄우고, 연결이 끊겨도 살려 둔다. 이게 없으면
                // 탭을 닫는 순간 셸이 죽어서 폰을 덮었다 열면 처음부터다.
                kasa_pty::register_session(&id, &sess);
                kasa_pty::keep_session(&id, sess.clone());
                (sess, false, id)
            }
            Err(e) => {
                eprintln!("[term-ws] 셸을 못 띄웠습니다: {e}");
                return;
            }
        }
    } else {
        // 디코딩된 값 → 원문 순으로 본다(`raw_pane_param` 주석 참고).
        let hit = kasa_pty::lookup_session(&pane)
            .map(|s| (s, pane.clone()))
            .or_else(|| {
                pane_raw
                    .as_deref()
                    .filter(|r| *r != pane)
                    .and_then(|r| kasa_pty::lookup_session(r).map(|s| (s, r.to_string())))
            });
        match hit {
            Some((s, id)) => (s, true, id),
            None => {
                eprintln!("[term-ws] 그런 pane 이 없습니다: {pane}");
                // 원격 GUI 가 「세션이 정말 끝났다」와 「연결이 잠깐 끊겼다」를 가르는
                // 유일한 신호. 이게 없으면 재접속 루프가 죽은 id 로 영원히 돈다 —
                // 연결 즉시 닫힘만으로는 네트워크 유실과 구분되지 않는다.
                let _ = socket
                    .send(Message::Text(
                        serde_json::json!({"t": "gone"}).to_string().into(),
                    ))
                    .await;
                return;
            }
        }
    };
    // A pane number can be reused between discovery and this handshake. Bind
    // only the exact persisted surface requested by an identity-aware viewer.
    if expected_surface_key.as_ref().is_some_and(|expected| crate::surface_keys::get(&self_id).as_ref() != Some(expected)) {
        let _ = socket.send(Message::Text(serde_json::json!({
            "t": "gone", "reason": "surface_identity_changed"
        }).to_string().into())).await;
        return;
    }
    let viewport = ViewerViewport::new(sess.clone());
    let viewport_token = viewport.token;
    let mut visual_subscription = (want_grid && crate::visual::producer_available())
        .then(|| crate::visual::subscribe(&self_id));
    let native_scene = visual_subscription.is_some();
    // `grid=1` 이면 우리가 파싱해 둔 셀 그리드를 그대로 보낸다 — 받는 쪽에 VT 파서가
    // 필요 없다. 그리드를 ANSI 로 되돌려 보내면 브라우저가 그걸 또 파싱해야 하고, 그
    // 파서(xterm.js)가 키 입력까지 자기 방식으로 가로채 모바일 IME 를 깨뜨렸다.
    // 구독과 화면 스냅샷을 한 번에 받는다 — 둘로 나누면 그 사이 출력이 유실되거나
    // 두 번 그려진다(`tap_bytes_with_snapshot` 주석 참고).
    let tap = if want_grid {
        let (rx, snap) = sess.tap_screens_with_snapshot();
        // 데스크톱이 스크롤백을 올려다보는 중이면 그 창이 아니라 바닥 화면을 준다 —
        // 거울은 스크롤이 따로 있다(아래 루프의 같은 갈림과 짝).
        let snap = if sess.view_state().0 > 0 { sess.live_screen() } else { snap };
        Tap::Grid(rx, Box::new(snap))
    } else {
        let (rx, bytes, size) = sess.tap_bytes_with_sized_snapshot();
        Tap::Bytes(rx, bytes, size)
    };
    // kill 제어가 놓아 줄 대상 — self_id 는 아래 size 메시지에 실려 move 된다.
    let kill_id = self_id.clone();
    let ctl_pane = self_id.clone();
    let (mut ws_tx, mut ws_rx) = socket.split();
    // 붙자마자 현재 격자 크기를 알려 준다 — 미러는 이 크기에 자기를 맞춰야
    // 줄바꿈이 어긋나지 않는다(웹이 PTY 를 바꾸면 kasaterm 쪽이 깨지므로).
    // Captured bytes must be parsed at their capture dimensions. Reading
    // sess.size() here races a resize after subscription/snapshot capture.
    let (c, r) = match &tap {
        Tap::Bytes(_, _, size) => *size,
        Tap::Grid(_, snap) => (snap.cols, snap.rows),
    };
    // `id` 는 이 연결이 실제로 붙은 세션 — 새 셸은 서버가 지은 web-uuid 라 클라가
    // 이걸 받아야 목록에서 자기 행(「보는 중」)을 안다.
    // 식별자는 여기서 한 번 읽어 이 연결 내내 같은 값을 쓴다 — 거울은 첫 악수의
    // 값을 기억해 두고 뒤에 오는 size 마다 대조하므로, 그 사이 키가 새로 생겨
    // 다른 값이 실리면 「원본이 바뀌었다」로 읽고 끊는다(viewport 처리기의 in-band size).
    let handshake_key = crate::surface_keys::get(&self_id);
    let input_surface_key = handshake_key.clone();
    let _ = ws_tx
        .send(Message::Text(
            serde_json::json!({
                "t": "size", "cols": c, "rows": r, "mirror": mirrored, "id": self_id,
                "surface_key": handshake_key,
                // `viewport_latest`: the last toucher wins, a person on this machine
                // reclaims by touching the pane, and a viewer that lost is told so.
                "capabilities": if native_scene {
                    serde_json::json!({ "mirror_viewport": 1, "viewport_latest": 1, "native_scene": 1 })
                } else { serde_json::json!({ "mirror_viewport": 1, "viewport_latest": 1 }) },
            })
            .to_string()
            .into(),
        ))
        .await;
    // 이어서 현재 화면. 크기를 먼저 알린 뒤라야 클라가 격자를 맞춘 상태에서 그린다.
    // 바이너리로 나가므로 클라는 PTY 바이트와 구분 없이 그대로 `term.write` 한다 —
    // 받는 쪽에 필요한 코드가 0줄이다. 이게 없으면 이미 떠 있는 pane 에 붙었을 때
    // 다음 출력이 날 때까지 화면이 빈 채로 남는다.
    // crossbeam recv 는 블로킹이라 tokio 워커에서 그대로 돌리면 런타임을 세운다.
    // 전용 스레드가 받아 tokio 채널로 건넨다.
    let (btx, mut brx) = tokio::sync::mpsc::channel::<Frame>(64);
    // 거울의 「지난 줄 달라」 응답도 화면과 같은 채널을 탄다 — 순서가 보장된다.
    let btx_shell = btx.clone();
    // 거울 클라가 알린 제 폭(칸 수). 0 = 원본 그대로. 원본 pane 은 resize 를 안 받으므로
    // 서버가 격자를 이 폭으로 다시 접어 보낸다(폰 앱·데스크톱 거울과 같은 규칙,
    // `kasa_screen::reflow`). 웹은 VT 파서도 접기도 없이 받은 셀을 그리기만 한다.
    let view_cols = std::sync::Arc::new(std::sync::atomic::AtomicU16::new(0));
    let view_cols_in = view_cols.clone();
    // 접을 재료 — 원본 폭 그대로의 전체 격자. 프레임은 바뀐 행만 오므로 여기 쌓는다.
    let mut web_src: Option<WebSrc> = None;
    let mut visual_delivery = VisualDelivery { glyphs, ..Default::default() };
    // 거울(이미 있는 pane 을 보는 접속)도, 이 접속이 새로 띄운 원격 셸도 등록한다 —
    // 후자는 만든 쪽이 곧 보는 사람이라(맥북의 `mini` 창) 거기가 브라우저의 자리다.
    let ctl_token = register_viewer_ctl(&ctl_pane, btx.clone());
    match tap {
        Tap::Bytes(rx, screen, _) => {
            let _ = ws_tx.send(Message::Binary(screen.into())).await;
            std::thread::spawn(move || {
                while let Ok(chunk) = rx.recv() {
                    if btx.blocking_send(Frame::Bytes(chunk)).is_err() {
                        break;
                    }
                }
            });
        }
        Tap::Grid(rx, snap) => {
            let msg = if native_scene {
                visual_delivery.encode(&sess.live_screen(), 0, VisualEvent::Raw)
            } else { Some(crate::gridwire::encode_with_glyphs(&snap, glyphs).to_string()) };
            if let Some(msg) = msg { let _ = ws_tx.send(Message::Text(msg.into())).await; }
            web_src = Some(WebSrc::from_full(&snap));
            std::thread::spawn(move || {
                while let Ok(upd) = rx.recv() {
                    if btx.blocking_send(Frame::Grid(Box::new(upd))).is_err() {
                        break;
                    }
                }
            });
        }
    }

    // 브라우저가 Ping 에 자동으로 돌려주는 Pong 의 마지막 시각. 폰 탭이
    // 백그라운드로 잠들면 TCP 는 한참 살아 있어서, 이걸 봐야 끊김-복원(아래)이
    // 언젠가는 돈다 — 안 보면 폰을 주머니에 넣은 것만으로 pane 이 좁은 채 남는다.
    let last_pong = std::sync::Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
    let pong_in = last_pong.clone();

    let sess_in = sess.clone();
    let sess_sz = sess.clone();
    let self_sized = Arc::new(std::sync::Mutex::new(SelfSized::Idle));
    let self_sized_in = self_sized.clone();
    let self_sized_out = self_sized.clone();
    let output_session_id = self_id.clone();
    let input_session_id = self_id.clone();
    let replaced = Arc::new(tokio::sync::Notify::new());
    let replaced_input = replaced.clone();
    let mut last_size = (c, r);
    // 이 연결이 원본 격자를 쥐었다고 뷰어에게 답한 상태. 입력 쪽이 허가 순간 세우고,
    // 출력 쪽이 남(원본의 사람·더 늦게 만진 뷰어)에게 넘어간 것을 보고 `lost` 로 알린다 —
    // 표본 추출이 아니라 깃발이라 짧게 쥐었다 잃어도 놓치지 않는다.
    let viewport_held = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let viewport_held_in = viewport_held.clone();
    let viewport_lost = move |sess: &kasa_pty::PtySession| {
        viewport_held.load(std::sync::atomic::Ordering::Acquire)
            && sess.viewer_size_owner() != Some(viewport_token)
            && viewport_held.swap(false, std::sync::atomic::Ordering::AcqRel)
    };
    let lost_notice = || Message::Text(
        serde_json::json!({"t": "viewport", "granted": false, "lost": true}).to_string().into(),
    );
    // 화면이 위로 밀려 스크롤백으로 들어간 줄을 거울에게도 흘린다(`scrolled`). 이게
    // 없으면 폰은 살아 있는 화면만 받아 위로 넘길 지난 줄이 없다.
    let mut last_hist = sess.view_state().1;
    let mut to_browser = tokio::spawn(async move {
        // Migration/replacement preserves the pane id but installs another
        // PtySession. A quiet terminal must reconnect too; this timer is local
        // only and must not turn the 30-second keepalive into a ping flood.
        let mut session_watch = tokio::time::interval(std::time::Duration::from_millis(250));
        session_watch.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut keepalive_at = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            if !websocket_session_is_current(&output_session_id, &sess_sz) {
                // No `gone`: the pane still exists under the same id. Closing
                // makes viewers reconnect, resubscribe and receive its new snapshot.
                let _ = ws_tx.send(Message::Close(None)).await;
                break;
            }
            // 조용할 때 ping 을 끼운다. 터널·리버스 프록시는 유휴 WebSocket 을
            // 끊는데(Cloudflare 무료 플랜 ~100초), 터미널은 아무 출력 없는 시간이
            // 길어서 반드시 걸린다. 30초면 그 절반이라 여유가 있다.
            let visual_deadline = visual_delivery.deadline();
            let incoming = tokio::select! {
                _ = session_watch.tick() => {
                    // Raw-byte subscribers do not receive publish_full_snapshot
                    // on a quiet GUI resize. Reattach atomically to the resized
                    // parser rather than mixing a new snapshot with queued old
                    // byte deltas. Grid subscribers already receive that frame.
                    // 이 연결의 viewport 요청이 바꾼 격자면 재접속하지 않는다(SelfSized 주석).
                    if viewport_lost(&sess_sz) && ws_tx.send(lost_notice()).await.is_err() {
                        break;
                    }
                    let foreign = match *self_sized_out.lock().unwrap() {
                        SelfSized::Idle => true,
                        SelfSized::Pending => false,
                        SelfSized::At(size) => size != sess_sz.size(),
                    };
                    if !want_grid && sess_sz.size() != last_size && foreign {
                        let _ = ws_tx.send(Message::Close(None)).await;
                        break;
                    }
                    continue;
                },
                _ = replaced.notified() => continue,
                _ = tokio::time::sleep_until(keepalive_at) => Err(()),
                frame = async {
                    if let Some(subscription) = visual_subscription.as_mut() {
                        tokio::select! {
                            frame = brx.recv() => frame,
                            changed = subscription.changed.changed() => changed.ok().map(|_| Frame::Visual),
                            _ = async {
                                if let Some(deadline) = visual_deadline {
                                    tokio::time::sleep_until(deadline).await;
                                } else { std::future::pending::<()>().await; }
                            } => Some(Frame::VisualDeadline),
                        }
                    } else {
                        brx.recv().await
                    }
                } => Ok(frame),
            };
            keepalive_at = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
            // A queued EOF or frame may belong to the previous session. Check
            // again after awaiting it so it cannot terminate the replacement.
            if !websocket_session_is_current(&output_session_id, &sess_sz) {
                let _ = ws_tx.send(Message::Close(None)).await;
                break;
            }
            match incoming {
                Ok(Some(chunk)) => {
                    // PTY 격자가 바뀌었으면(divider·⤢·다른 미러) 바이트보다 먼저
                    // 알린다 — 미러 xterm 이 낡은 격자로 새 바이트를 그리면 글자가
                    // 한 자씩 세로로 꺾이는 그 화면이 된다. 크기 변경은 반드시
                    // full snapshot 출력을 동반하므로(chunk) 여기서 보면 놓치지 않는다.
                    let now = sess_sz.size();
                    let self_caused = {
                        let mut marker = self_sized_out.lock().unwrap();
                        match *marker {
                            SelfSized::Pending => true,
                            SelfSized::At(size) if size == now => {
                                *marker = SelfSized::Idle;
                                true
                            }
                            _ => false,
                        }
                    };
                    if viewport_lost(&sess_sz) && ws_tx.send(lost_notice()).await.is_err() {
                        break;
                    }
                    if now != last_size {
                        if !want_grid && !self_caused {
                            // A byte delta can beat the quiet-resize timer.
                            // It cannot repair already-reflowed source history,
                            // and can itself have been queued at the old width.
                            // Reattach for an atomic history+screen snapshot;
                            // do not advance last_size and bypass the timer.
                            let _ = ws_tx.send(Message::Close(None)).await;
                            break;
                        }
                        last_size = now;
                        // raw 구독자의 자기-유발 변경은 viewport 처리기가 surface_key 까지
                        // 실은 size 와 스냅샷을 이 채널에 이미 넣었다 — 여기서 또 보내면
                        // 식별자 없는 size 가 앞질러 가 거울이 「원본이 바뀌었다」고 끊는다.
                        if want_grid {
                            let msg = serde_json::json!({
                                "t": "size", "cols": now.0, "rows": now.1, "mirror": mirrored,
                            })
                            .to_string();
                            if ws_tx.send(Message::Text(msg.into())).await.is_err() {
                                break;
                            }
                        }
                    }
                    // 셸이 끝났다(reader 의 EOF 센티널) — 「gone」으로 거울의 재접속을
                    // 멈추게 하고 접는다. 안 접으면 이 핸들러가 세션 Arc 를 쥔 채 남아
                    // 세션이 안 죽고, 거울은 죽은 화면을 계속 비춘다(state.rs EOF 주석).
                    if matches!(&chunk, Frame::Grid(u) if u.eof) {
                        let _ = ws_tx
                            .send(Message::Text(
                                serde_json::json!({"t": "gone"}).to_string().into(),
                            ))
                            .await;
                        break;
                    }
                    let sent = match chunk {
                        Frame::Bytes(b) => ws_tx.send(Message::Binary(b.into())).await,
                        // 그리드는 텍스트 프레임 — 입력(바이너리)과 섞이지 않는다.
                        Frame::Grid(u) => {
                            let (offset, hist) = sess_sz.view_state();
                            // 데스크톱이 위로 올라가 있으면 GUI 프레임은 지난 줄 창이다 — 거울에는
                            // 입력상자·상태줄이 있는 바닥 화면을 통째로 다시 떠서 준다.
                            // A reader frame queued before resize can arrive after its full snapshot.
                            // Scene notifications can overtake queued deltas at the same dimensions.
                            let u = if native_scene || offset > 0 || (u.cols, u.rows) != sess_sz.size() {
                                Box::new(sess_sz.live_screen())
                            } else {
                                u
                            };
                            let vc = view_cols.load(std::sync::atomic::Ordering::Relaxed);
                            if hist > last_hist {
                                // 이번 프레임에 스크롤백으로 들어간 줄 — 화면보다 먼저 보내야
                                // 받는 쪽이 「지난 줄 뒤에 새 화면」 순서로 잇는다. 오래된 순.
                                let k = (hist - last_hist).min(HISTORY_ROWS_MAX);
                                let rows = sess_sz.rows_above_live(k);
                                let msg = serde_json::json!({
                                    "t": "scrolled",
                                    "rows": encode_history_rows(&rows, u.cols, vc, glyphs),
                                })
                                .to_string();
                                if ws_tx.send(Message::Text(msg.into())).await.is_err() {
                                    break;
                                }
                            }
                            last_hist = hist;
                            let src = web_src.get_or_insert_with(|| WebSrc::from_full(&u));
                            src.absorb(&u);
                            let msg = if native_scene {
                                let Some(msg) = visual_delivery.encode(&u, vc, VisualEvent::Raw) else { continue };
                                msg
                            } else if vc > 0 && vc != u.cols {
                                src.reflowed(vc, glyphs)
                            } else {
                                crate::gridwire::encode_with_glyphs(&u, glyphs).to_string()
                            };
                            ws_tx.send(Message::Text(msg.into())).await
                        }
                        Frame::Reflow => {
                            let vc = view_cols.load(std::sync::atomic::Ordering::Relaxed);
                            if native_scene || web_src.as_ref().is_some_and(|src| {
                                (src.meta.cols, src.meta.rows) != sess_sz.size()
                            }) {
                                web_src = Some(WebSrc::from_full(&sess_sz.live_screen()));
                            }
                            let Some(src) = web_src.as_ref() else { continue };
                            // 폭이 원본으로 돌아가도 통째로 보낸다 — 접힌 격자를 걷어야 한다.
                            let msg = if native_scene {
                                let Some(msg) = visual_delivery.encode(&src.full_snapshot(), vc, VisualEvent::Raw) else { continue };
                                msg
                            } else if vc > 0 && vc != src.meta.cols {
                                src.reflowed(vc, glyphs)
                            } else {
                                src.full_raw(glyphs)
                            };
                            ws_tx.send(Message::Text(msg.into())).await
                        }
                        Frame::Visual | Frame::VisualDeadline => {
                            let event = if matches!(chunk, Frame::VisualDeadline) {
                                VisualEvent::Deadline
                            } else { VisualEvent::Published };
                            let raw = sess_sz.live_screen();
                            let src = WebSrc::from_full(&raw);
                            let vc = view_cols.load(std::sync::atomic::Ordering::Relaxed);
                            web_src = Some(src);
                            let Some(msg) = visual_delivery.encode(&raw, vc, event) else { continue };
                            ws_tx.send(Message::Text(msg.into())).await
                        }
                        Frame::Control(s) => ws_tx.send(Message::Text(s.into())).await,
                    };
                    if sent.is_err() {
                        break;
                    }
                }
                Ok(None) => {
                    // tap 이 끝났다 = PTY 종료(reader 가 바이트 tap 송신자를 놓았다)
                    // 또는 세션 폐기. 어느 쪽이든 「gone」을 먼저 보낸다 — 거울이
                    // 재접속으로 알아내는 왕복을 없애고, 낡은 서버처럼 죽은 세션에
                    // 다시 붙는 길을 막는다.
                    let _ = ws_tx
                        .send(Message::Text(
                            serde_json::json!({"t": "gone"}).to_string().into(),
                        ))
                        .await;
                    break;
                }
                Err(_) => {
                    // Pong 이 두 주기 넘게 없으면 피어가 잠든 것 — TCP 가 안 끊겨도
                    // 우리가 접어야 아래 복원이 돌아 pane 크기가 돌아온다.
                    let stale = pong_in
                        .lock()
                        .map(|t| t.elapsed() > std::time::Duration::from_secs(75))
                        .unwrap_or(false);
                    if stale {
                        break;
                    }
                    if ws_tx.send(Message::Ping(Vec::new().into())).await.is_err() {
                        break;
                    }
                }
            }
        }
    });
    let pong_shell = last_pong.clone();
    let mut to_shell = tokio::spawn(async move {
        let mut logical_viewport = false;
        while let Some(Ok(msg)) = ws_rx.next().await {
            if !websocket_session_is_current(&input_session_id, &sess_in) {
                // Drop obsolete input/resize/kill instead of touching the old
                // session. Keep this task alive until the sender flushes Close.
                replaced_input.notify_one();
                continue;
            }
            match msg {
                // 키 입력은 binary — 텍스트 채널과 섞이지 않아 파싱이 필요 없다.
                Message::Binary(b) => {
                    let _ = sess_in.send_bytes(&b);
                }
                Message::Text(t) => {
                    let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) else {
                        continue;
                    };
                    // `{"t":"history","rows":N}` — 살아 있는 화면 위의 지난 줄 N 개(오래된 순).
                    // 붙자마자 한 번 받아 두면 그 뒤로는 `scrolled` 가 이어 준다.
                    if v.get("t").and_then(|x| x.as_str()) == Some("history") {
                        let n = v
                            .get("rows")
                            .and_then(|x| x.as_u64())
                            .unwrap_or(300)
                            .min(HISTORY_ROWS_MAX as u64) as usize;
                        let rows = sess_in.rows_above_live(n);
                        let (src_cols, _) = sess_in.size();
                        let vc = view_cols_in.load(std::sync::atomic::Ordering::Relaxed);
                        let msg = serde_json::json!({
                            "t": "history",
                            "rows": encode_history_rows(&rows, src_cols, vc, glyphs),
                        })
                        .to_string();
                        let _ = btx_shell.send(Frame::Control(msg)).await;
                        continue;
                    }
                    // `{"t":"view","cols":N}` — 거울 클라의 제 폭. 원본은 안 건드리고 서버가
                    // 그 폭으로 다시 접어 보낸다. 0 이면 원본 그대로.
                    if v.get("t").and_then(|x| x.as_str()) == Some("view") {
                        let n = v.get("cols").and_then(|x| x.as_u64()).unwrap_or(0).min(400) as u16;
                        if logical_viewport && n != 0 {
                            continue;
                        }
                        // 같은 폭을 되풀이해 알리면 무시 — 프레임마다 다시 접어 보내는 되먹임 차단.
                        if mirrored && view_cols_in.swap(n, std::sync::atomic::Ordering::Relaxed) != n {
                            let _ = btx_shell.send(Frame::Reflow).await;
                        }
                        continue;
                    }
                    if v.get("t").and_then(|x| x.as_str()) == Some("viewport") {
                        let mut clear_reflow = false;
                        let before = sess_in.size();
                        *self_sized_in.lock().unwrap() = SelfSized::Pending;
                        let granted = match v.get("op").and_then(|x| x.as_str()) {
                            Some("acquire" | "resize") => {
                                viewport_dimensions(&v).map_or(false, |(cols, rows)| {
                                    let result = if v["op"] == "acquire" {
                                        // Legacy wrapping must not reshape a newly negotiated PTY grid.
                                        logical_viewport = true;
                                        clear_reflow = view_cols_in.swap(
                                            0, std::sync::atomic::Ordering::Relaxed,
                                        ) != 0;
                                        sess_in.acquire_viewer_size(viewport_token, cols, rows)
                                    } else {
                                        sess_in.resize_viewer_size(viewport_token, cols, rows)
                                    };
                                    result.unwrap_or(false)
                                })
                            }
                            Some("release") => {
                                let _ = sess_in.release_viewer_size(viewport_token);
                                false
                            }
                            _ => false,
                        };
                        // A refused resize means someone touched the pane after us.
                        let lost = v["op"] == "resize" && !granted
                            && viewport_held_in.load(std::sync::atomic::Ordering::Acquire);
                        viewport_held_in.store(granted, std::sync::atomic::Ordering::Release);
                        if clear_reflow {
                            let _ = btx_shell.send(Frame::Reflow).await;
                        }
                        // raw 구독자는 격자가 바뀌어도 스냅샷을 못 받는다(publish_full_snapshot
                        // 은 그리드 tap 에만 간다). 남이 바꾼 격자면 출력 쪽이 연결을 닫아
                        // 재접속으로 받게 하지만, 자기 요청(acquire·resize·release)으로 바뀐
                        // 격자는 재접속이 곧 소유권 포기(acquire)거나 헛된 깜빡임(release)이라
                        // 그 길을 못 탄다 — 그래서 여기서 직접 밀어 넣는다. size 는 첫 악수와
                        // 같은 필드(id·surface_key)를 실어야 거울이 「다른 원본」이라 오해하지
                        // 않고, `snapshot` 표식이 「스냅샷이 뒤따르니 제자리에서 받아라」다.
                        let now = sess_in.size();
                        if !want_grid && now != before {
                            *self_sized_in.lock().unwrap() = SelfSized::At(now);
                            let (bytes, (cols, rows)) = sess_in.sized_snapshot_bytes();
                            let size = serde_json::json!({
                                "t": "size", "cols": cols, "rows": rows, "mirror": mirrored,
                                "id": input_session_id,
                                "surface_key": input_surface_key,
                                "snapshot": true,
                            })
                            .to_string();
                            let _ = btx_shell.send(Frame::Control(size)).await;
                            let _ = btx_shell.send(Frame::Bytes(bytes)).await;
                        } else {
                            *self_sized_in.lock().unwrap() = SelfSized::Idle;
                        }
                        let mut reply = serde_json::json!({"t": "viewport", "granted": granted});
                        if lost {
                            reply["lost"] = serde_json::json!(true);
                        }
                        let _ = btx_shell.send(Frame::Control(reply.to_string())).await;
                        continue;
                    }
                    if v.get("t").and_then(|x| x.as_str()) == Some("resize") {
                        let force = v.get("force").and_then(|x| x.as_bool()).unwrap_or(false);
                        if !mirrored || force || own {
                            let c = v.get("cols").and_then(|x| x.as_u64()).unwrap_or(80)
                                .clamp(20, 1000) as u16;
                            let r = v.get("rows").and_then(|x| x.as_u64()).unwrap_or(24)
                                .clamp(5, 1000) as u16;
                            if mirrored && !own {
                                // Legacy explicit fit also gets an identity, never a size-based restore.
                                let _ = sess_in.acquire_viewer_size(viewport_token, c, r);
                            } else {
                                let _ = sess_in.resize(c, r);
                            }
                        }
                    }
                    // 소유자의 명시적 종료(원격 pane 닫기). keep_session 의 강한
                    // Arc 를 놓고 연결을 접는다 — 남은 참조(다른 미러)가 다
                    // 떨어지면 Drop 이 셸을 죽인다. detach(그냥 끊기)와 이 길을
                    // 갈라 두는 것이 「미러링이 아니라 이사」 설계의 반쪽이다.
                    if v.get("t").and_then(|x| x.as_str()) == Some("kill") && own {
                        kasa_pty::release_session(&kill_id);
                        break;
                    }
                }
                Message::Close(_) => break,
                // 브라우저 네트워크 스택이 우리 Ping 에 자동으로 돌려주는 응답 —
                // 이 시각이 to_browser 의 잠든-피어 판정 재료다.
                Message::Pong(_) => {
                    if let Ok(mut t) = pong_shell.lock() {
                        *t = std::time::Instant::now();
                    }
                }
                _ => {}
            }
        }
    });
    // 한쪽이 끝나면 다른 쪽도 접는다. **셸은 여기서 안 죽는다** — `keep_session` 이
    // 붙들고 있어서, 다시 붙으면 하던 작업이 그대로 있다(셸이 exit 하면 EOF 를 보고
    // 스스로 빠진다).
    tokio::select! {
        _ = &mut to_browser => to_shell.abort(),
        _ = &mut to_shell => to_browser.abort(),
    }
    unregister_viewer_ctl(&ctl_pane, ctl_token);
    drop(viewport);
}

fn websocket_session_is_current(id: &str, session: &Arc<kasa_pty::PtySession>) -> bool {
    kasa_pty::lookup_session(id).is_some_and(|current| Arc::ptr_eq(&current, session))
}

/// 이 모듈 창구의 라우트. `router` 가 한 표로 합친 뒤 공통 레이어(Origin·토큰 가드)를 두른다.
pub(super) fn routes(backend: &Arc<dyn Backend>) -> axum::Router {
    let layout_ws_backend = backend.clone();
    axum::Router::new()
        .route(
            "/term/layout/ws",
            get(move |headers: HeaderMap, ws: WebSocketUpgrade| {
                term_layout_ws_handler(layout_ws_backend.clone(), headers, ws)
            }),
        )
        .route("/term/ws", get(term_ws_handler))
}

#[cfg(test)]
mod tests {
    #[derive(Clone)]
    struct MirrorInput(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for MirrorInput {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }

    fn replacement_source(
        id: &str,
        cols: u16,
    ) -> (std::sync::Arc<kasa_pty::PtySession>, crossbeam_channel::Sender<kasa_pty::ExtEvent>, MirrorInput) {
        let (events, receiver) = crossbeam_channel::unbounded();
        let input = MirrorInput(Default::default());
        let session = std::sync::Arc::new(kasa_pty::PtySession::start_external(
            kasa_pty::PtyOptions { pane_id: id.to_string(), cols, rows: 6, ..Default::default() },
            kasa_pty::ExternalIo {
                events: receiver, writer: Box::new(input.clone()), on_resize: std::sync::Arc::new(|_, _| {}),
            },
        ).unwrap());
        (session, events, input)
    }

    async fn wait_replacement_text(session: &kasa_pty::PtySession, text: &str) {
        tokio::time::timeout(std::time::Duration::from_secs(4), async {
            while !session.visible_text(6).contains(text) {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        }).await.expect("replacement screen did not arrive");
    }

    #[tokio::test]
    async fn raw_resize_reconnects_before_new_size_or_delta_for_quiet_and_busy_sources() {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::Message;
        for emit_delta in [false, true] {
            let id = format!("raw-resize-http-{}", uuid::Uuid::new_v4());
            let (source, events, _) = replacement_source(&id, 26);
            events.send(kasa_pty::ExtEvent::Bytes(b"ORIGINAL".to_vec())).unwrap();
            wait_replacement_text(&source, "ORIGINAL").await;
            kasa_pty::register_session(&id, &source);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let router = axum::Router::new().route("/term/ws", axum::routing::get(super::term_ws_handler));
            let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
            let url = format!("ws://{addr}/term/ws?pane={id}&own=0");
            let (mut client, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                while let Some(Ok(message)) = client.next().await {
                    if matches!(message, Message::Binary(ref bytes) if String::from_utf8_lossy(bytes).contains("ORIGINAL")) { return; }
                }
                panic!("initial raw snapshot missing");
            }).await.unwrap();
            // Let the watcher's initial tick run. The busy case then wakes
            // the byte branch before its next interval rather than relying
            // exclusively on quiet-resize polling.
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            source.resize(93, 6).unwrap();
            if emit_delta { events.send(kasa_pty::ExtEvent::Bytes(b"RESIZED_OUTPUT".to_vec())).unwrap(); }
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                while let Some(Ok(message)) = client.next().await {
                    match message {
                        Message::Close(_) => return,
                        Message::Text(text) => assert_ne!(serde_json::from_str::<serde_json::Value>(&text).unwrap()["t"], "size",
                            "raw size+delta bypassed authoritative history resubscription"),
                        Message::Binary(_) => panic!("queued raw delta crossed resize before snapshot"),
                        _ => {},
                    }
                }
                panic!("source did not close raw stream for resnapshot");
            }).await.unwrap();
            drop(client);
            let (mut fresh, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
            let first = tokio::time::timeout(std::time::Duration::from_secs(3), fresh.next()).await.unwrap().unwrap().unwrap();
            let Message::Text(size) = first else { panic!("snapshot missing its size handshake") };
            let size: serde_json::Value = serde_json::from_str(&size).unwrap();
            assert_eq!(size["cols"], 93);
            drop(fresh);
            server.abort();
            let _ = events.send(kasa_pty::ExtEvent::Eof);
        }
    }

    #[tokio::test]
    async fn websocket_session_replacement_closes_quiet_views_without_gone_or_stale_input() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::{tungstenite::Message, MaybeTlsStream, WebSocketStream};
        type Client = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

        async fn snapshot(client: &mut Client, expected: &str) {
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                while let Some(Ok(message)) = client.next().await {
                    match message {
                        Message::Binary(bytes) if String::from_utf8_lossy(&bytes).contains(expected) => return,
                        Message::Text(text) if text.contains(expected) => return,
                        Message::Close(_) => panic!("closed before initial snapshot"),
                        _ => {}
                    }
                }
                panic!("missing initial snapshot");
            }).await.unwrap();
        }

        async fn replaced_close(client: &mut Client) {
            tokio::time::timeout(std::time::Duration::from_millis(750), async {
                while let Some(Ok(message)) = client.next().await {
                    match message {
                        Message::Close(_) => return,
                        Message::Text(text) => assert_ne!(serde_json::from_str::<serde_json::Value>(&text).unwrap()["t"], "gone"),
                        Message::Ping(_) => panic!("replacement watcher must not send keepalive pings"),
                        _ => {}
                    }
                }
                panic!("replacement did not flush a close frame");
            }).await.expect("quiet replacement was not detected promptly");
        }

        for grid in [false, true] {
            let id = format!("swap-http-test-{}", uuid::Uuid::new_v4());
            let (old, old_events, old_input) = replacement_source(&id, 21);
            let (new, new_events, new_input) = replacement_source(&id, 37);
            old_events.send(kasa_pty::ExtEvent::Bytes(b"OLD-SOURCE".to_vec())).unwrap();
            new_events.send(kasa_pty::ExtEvent::Bytes(b"NEW-SOURCE".to_vec())).unwrap();
            wait_replacement_text(&old, "OLD-SOURCE").await;
            wait_replacement_text(&new, "NEW-SOURCE").await;
            kasa_pty::register_session(&id, &old);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let router = axum::Router::new().route("/term/ws", axum::routing::get(super::term_ws_handler));
            let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
            let url = format!("ws://{addr}/term/ws?pane={id}&grid={}", u8::from(grid));
            let (mut quiet, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
            let (mut typing, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
            snapshot(&mut quiet, "OLD-SOURCE").await;
            snapshot(&mut typing, "OLD-SOURCE").await;

            kasa_pty::register_session(&id, &new);
            // The old Arc remains alive and silent. Neither EOF nor fresh PTY
            // output can accidentally provide the wakeup this test requires.
            typing.send(Message::Binary(b"STALE-KEYS".to_vec().into())).await.unwrap();
            replaced_close(&mut quiet).await;
            replaced_close(&mut typing).await;
            assert!(old_input.0.lock().unwrap().is_empty());
            assert!(new_input.0.lock().unwrap().is_empty());

            let (mut reconnected, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
            snapshot(&mut reconnected, "NEW-SOURCE").await;
            reconnected.send(Message::Binary(b"NEW-KEYS".to_vec().into())).await.unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                while new_input.0.lock().unwrap().is_empty() {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            }).await.unwrap();
            assert_eq!(&*new_input.0.lock().unwrap(), b"NEW-KEYS");
            assert!(old_input.0.lock().unwrap().is_empty());
            let _ = quiet.close(None).await;
            let _ = typing.close(None).await;
            reconnected.close(None).await.unwrap();
            old_events.send(kasa_pty::ExtEvent::Eof).unwrap();
            new_events.send(kasa_pty::ExtEvent::Eof).unwrap();
            server.abort();
        }
    }

    #[tokio::test]
    async fn websocket_rejects_reused_number_before_any_source_frame() {
        use futures_util::StreamExt;
        let id = format!("identity-guard-{}", uuid::Uuid::new_v4());
        let (source, events, _) = replacement_source(&id, 21);
        events.send(kasa_pty::ExtEvent::Bytes(b"WRONG-SURFACE".to_vec())).unwrap();
        wait_replacement_text(&source, "WRONG-SURFACE").await;
        kasa_pty::register_session(&id, &source);
        crate::surface_keys::set(&id, "replacement-key");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = axum::Router::new().route("/term/ws", axum::routing::get(super::term_ws_handler));
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let (mut viewer, _) = tokio_tungstenite::connect_async(
            format!("ws://{addr}/term/ws?pane={id}&surface_key=legacy%3A%254")
        ).await.unwrap();
        let message = tokio::time::timeout(std::time::Duration::from_secs(2), viewer.next())
            .await.unwrap().unwrap().unwrap();
        let payload: serde_json::Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
        assert_eq!(payload["t"], "gone");
        assert_eq!(payload["reason"], "surface_identity_changed");
        crate::surface_keys::remove(&id);
        events.send(kasa_pty::ExtEvent::Eof).unwrap();
        server.abort();
    }

    #[tokio::test]
    async fn websocket_session_replacement_desktop_mirror_reconnects_to_new_screen_and_input() {
        let id = format!("swap-desktop-test-{}", uuid::Uuid::new_v4());
        let (old, old_events, old_input) = replacement_source(&id, 21);
        let (new, new_events, new_input) = replacement_source(&id, 37);
        old_events.send(kasa_pty::ExtEvent::Bytes(b"OLD-DESKTOP".to_vec())).unwrap();
        new_events.send(kasa_pty::ExtEvent::Bytes(b"NEW-DESKTOP".to_vec())).unwrap();
        wait_replacement_text(&old, "OLD-DESKTOP").await;
        wait_replacement_text(&new, "NEW-DESKTOP").await;
        kasa_pty::register_session(&id, &old);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = axum::Router::new().route("/term/ws", axum::routing::get(super::term_ws_handler));
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let local = format!("mirror-swap-client-{}", uuid::Uuid::new_v4());
        let mirror = tokio::task::spawn_blocking(move || crate::remote::connect_view(
            crate::remote::RemoteSpec {
                base: format!("http://{addr}"), pane: Some(id), cwd: None, token: None,
                identity: crate::remote::RemoteIdentity::default(),
            }, &local,
        )).await.unwrap().unwrap();
        wait_replacement_text(&mirror.session, "OLD-DESKTOP").await;
        kasa_pty::register_session(&mirror.remote_id, &new);
        wait_replacement_text(&mirror.session, "NEW-DESKTOP").await;
        assert!(!mirror.session.visible_text(6).contains("OLD-DESKTOP"));
        mirror.session.send_bytes(b"AFTER-MOVE").unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while new_input.0.lock().unwrap().is_empty() {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        }).await.unwrap();
        assert_eq!(&*new_input.0.lock().unwrap(), b"AFTER-MOVE");
        assert!(old_input.0.lock().unwrap().is_empty());
        drop(mirror);
        old_events.send(kasa_pty::ExtEvent::Eof).unwrap();
        new_events.send(kasa_pty::ExtEvent::Eof).unwrap();
        server.abort();
    }

    #[tokio::test]
    async fn websocket_glyph_segments_are_opt_in_for_full_and_delta_grids() {
        use futures_util::StreamExt;
        use serde_json::{json, Value};
        use std::sync::Arc;
        use tokio_tungstenite::{tungstenite::Message, MaybeTlsStream, WebSocketStream};

        async fn next_grid(ws: &mut WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>) -> Value {
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                while let Some(Ok(Message::Text(text))) = ws.next().await {
                    let value: Value = serde_json::from_str(&text).unwrap();
                    if value["t"] == "grid" { return value; }
                }
                panic!("grid stream ended");
            }).await.unwrap()
        }

        let pane = format!("glyph-http-test-{}", uuid::Uuid::new_v4());
        let (events, receiver) = crossbeam_channel::unbounded();
        let source = Arc::new(kasa_pty::PtySession::start_external(
            kasa_pty::PtyOptions { pane_id: pane.clone(), cols: 20, rows: 3, ..Default::default() },
            kasa_pty::ExternalIo {
                events: receiver, writer: Box::new(std::io::sink()), on_resize: Arc::new(|_, _| {}),
            },
        ).unwrap());
        kasa_pty::register_session(&pane, &source);
        events.send(kasa_pty::ExtEvent::Bytes("A⎿B가".as_bytes().to_vec())).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while !source.visible_text(3).contains("A⎿B가") {
                tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            }
        }).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = axum::Router::new().route("/term/ws", axum::routing::get(super::term_ws_handler));
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let url = format!("ws://{address}/term/ws?pane={pane}&grid=1");
        let (mut legacy, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let (mut glyphs, _) = tokio_tungstenite::connect_async(format!("{url}&glyphs=1")).await.unwrap();
        let old = next_grid(&mut legacy).await;
        let segmented = next_grid(&mut glyphs).await;
        let old_run = old["dirty"][0][1][0].as_array().unwrap();
        let new_run = segmented["dirty"][0][1][0].as_array().unwrap();
        assert_eq!(old_run.len(), 4);
        assert_eq!(&new_run[..4], old_run);
        assert_eq!(new_run[4], json!([["A", 1], ["⎿", 1], ["B", 1], ["가", 2]]));
        events.send(kasa_pty::ExtEvent::Bytes(b"X".to_vec())).unwrap();
        assert_eq!(next_grid(&mut legacy).await["dirty"][0][1][0].as_array().unwrap().len(), 4);
        assert_eq!(next_grid(&mut glyphs).await["dirty"][0][1][0].as_array().unwrap().len(), 5);
        source.resize(30, 4).unwrap();
        assert_eq!(next_grid(&mut legacy).await["dirty"][0][1][0].as_array().unwrap().len(), 4);
        assert_eq!(next_grid(&mut glyphs).await["dirty"][0][1][0].as_array().unwrap().len(), 5);
        legacy.close(None).await.unwrap();
        glyphs.close(None).await.unwrap();
        let _ = events.send(kasa_pty::ExtEvent::Eof);
        server.abort();
    }

    #[tokio::test]
    async fn native_scene_websocket_is_atomic_clears_stale_output_and_scopes_assets() {
        use futures_util::StreamExt;
        use serde_json::{json, Value};
        use std::sync::Arc;
        use tokio_tungstenite::{tungstenite::Message, MaybeTlsStream, WebSocketStream};

        async fn next_kind(
            ws: &mut WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
            kind: &str,
        ) -> Value {
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                while let Some(Ok(Message::Text(text))) = ws.next().await {
                    let value: Value = serde_json::from_str(&text).unwrap();
                    if value["t"] == kind { return value; }
                }
                panic!("websocket ended before {kind}");
            }).await.expect("scene frame timeout")
        }

        let pane = format!("scene-http-test-{}", uuid::Uuid::new_v4());
        let (events, receiver) = crossbeam_channel::unbounded();
        let source = Arc::new(kasa_pty::PtySession::start_external(
            kasa_pty::PtyOptions { pane_id: pane.clone(), cols: 21, rows: 6, ..Default::default() },
            kasa_pty::ExternalIo {
                events: receiver, writer: Box::new(std::io::sink()), on_resize: Arc::new(|_, _| {}),
            },
        ).unwrap());
        kasa_pty::register_session(&pane, &source);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = axum::Router::new()
            .route("/term/ws", axum::routing::get(super::term_ws_handler))
            .route("/term/visual-asset", axum::routing::get(super::term_visual_asset))
            .route("/term/visual-builtin/{name}", axum::routing::get(super::term_visual_builtin));
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let url = format!("ws://{address}/term/ws?pane={pane}&grid=1&glyphs=1");
        let (mut standalone, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        assert!(next_kind(&mut standalone, "size").await["capabilities"].get("native_scene").is_none());
        standalone.close(None).await.unwrap();

        crate::visual::register_producer(Arc::new(|| {}));
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        assert_eq!(next_kind(&mut ws, "size").await["capabilities"]["native_scene"], 1);
        assert!(next_kind(&mut ws, "grid").await["scene"].is_null());
        let raw = source.live_screen();
        let mut composed: Vec<_> = raw.dirty.iter().map(|(_, row)| row.clone()).collect();
        composed[0][0].ch = 'X';
        let asset = crate::visual::register_inline_asset(&pane, 7,
            Arc::from(&b"\x89PNG\r\n\x1a\nfixture"[..])).unwrap();
        assert!(crate::visual::publish(crate::visual::PaneVisualFrame {
            pane_id: pane.clone(), source_key: crate::visual::source_key(&raw, 0).unwrap(),
            raw_snapshot: raw.clone(),
            scene_revision: 1, cols: raw.cols, rows: raw.rows, offset: 0, composed_cells: composed,
            overlays: vec![crate::visual::VisualOverlay {
                id: "fixture-picture".into(), rect: crate::visual::VisualRect { x: 0.0, y: 0.0, width: 2.0, height: 2.0 },
                clip: None, z: 1, fit: "scale-down".into(), anchor: "center".into(),
                asset: crate::visual::VisualAsset::Inline { id: asset.clone() }, motion: None,
            }],
        }));
        let decorated = next_kind(&mut ws, "grid").await;
        assert_eq!(decorated["dirty"][0][1][0].as_array().unwrap().len(), 5);
        assert_eq!(decorated["dirty"][0][1][0][0], "X");
        assert_eq!(decorated["scene"]["overlays"][0]["asset"]["id"], asset);
        assert_eq!(decorated["sourceKey"], decorated["scene"]["sourceKey"]);
        assert_eq!(decorated["sceneRevision"], 1);

        // Each native composition intentionally trails one more PTY write at the same size.
        // Completed native frames must keep arriving without raw-logo frames between them.
        for revision in 2..8 {
            let marker = format!("L{revision}");
            events.send(kasa_pty::ExtEvent::Bytes(format!("\r{marker}").into_bytes())).unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                while !source.visible_text(6).contains(&marker) {
                    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
                }
            }).await.unwrap();
            let captured = source.live_screen();
            let captured_key = crate::visual::source_key(&captured, 0).unwrap();
            events.send(kasa_pty::ExtEvent::Bytes(b".".to_vec())).unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                while crate::visual::source_key(&source.live_screen(), 0).as_ref() == Some(&captured_key) {
                    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
                }
            }).await.unwrap();
            let mut next = crate::visual::latest_scene(&pane, 21, 6).unwrap().as_ref().clone();
            next.raw_snapshot = captured.clone();
            next.source_key = captured_key;
            next.scene_revision = revision;
            next.composed_cells = captured.dirty.iter().map(|(_, row)| row.clone()).collect();
            next.composed_cells[0][0].ch = 'N';
            assert!(crate::visual::publish(next));
            let rendered = next_kind(&mut ws, "grid").await;
            assert_eq!(rendered["sceneRevision"], revision, "streaming raw output displaced a completed scene");
            assert!(rendered["dirty"][0][1][0][0].as_str().unwrap().starts_with('N'));
            assert_eq!(rendered["cursor"], json!([captured.cursor_row, captured.cursor_col]),
                "composed cells must retain their own source cursor metadata");
        }

        let client = reqwest::Client::new();
        let asset_url = format!("http://{address}/term/visual-asset?pane={pane}&id={asset}");
        let response = client.get(&asset_url).send().await.unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["content-type"], "image/png");
        assert_eq!(client.get(format!("http://{address}/term/visual-asset?pane=other&id={asset}"))
            .send().await.unwrap().status(), 404);
        assert_eq!(client.get(format!("http://{address}/term/visual-asset?pane={pane}&id=not-a-file"))
            .send().await.unwrap().status(), 404);
        assert_eq!(client.get(format!("http://{address}/term/visual-builtin/unknown"))
            .send().await.unwrap().status(), 404);

        events.send(kasa_pty::ExtEvent::Bytes(b"\x1b[2J\x1b[HA".to_vec())).unwrap();
        let cleared = next_kind(&mut ws, "grid").await;
        assert!(cleared["scene"].is_null());
        assert_eq!(cleared["dirty"][0][1][0][0], "A");
        assert_eq!(cleared["dirty"].as_array().unwrap().len(), 6);
        source.resize(30, 8).unwrap();
        let resized = next_kind(&mut ws, "grid").await;
        assert!(resized["scene"].is_null());
        assert_eq!((resized["cols"].clone(), resized["rows"].clone()), (json!(30), json!(8)));
        ws.close(None).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while crate::visual::subscribed_panes().contains(&pane) {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        }).await.unwrap();
        let expired = client.get(&asset_url).send().await.unwrap();
        assert_eq!(expired.status(), 404);
        assert_eq!(expired.headers()["cache-control"], "no-store");
        let _ = events.send(kasa_pty::ExtEvent::Eof);
        server.abort();
    }

    #[tokio::test]
    async fn viewport_quiet_grid_tracks_acquire_resize_release_after_legacy_view() {
        use futures_util::{SinkExt, StreamExt};
        use serde_json::{json, Value};
        use std::sync::Arc;
        use tokio_tungstenite::{tungstenite::Message, MaybeTlsStream, WebSocketStream};

        async fn next_grid(
            ws: &mut WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
        ) -> Value {
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                while let Some(Ok(Message::Text(text))) = ws.next().await {
                    let value: Value = serde_json::from_str(&text).unwrap();
                    if value["t"] == "grid" {
                        return value;
                    }
                }
                panic!("websocket ended before grid");
            }).await.expect("quiet PTY did not publish a grid")
        }

        async fn assert_full_grid(
            ws: &mut WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
            cols: u16,
            rows: u16,
        ) {
            let frame = next_grid(ws).await;
            assert_eq!((frame["cols"].as_u64(), frame["rows"].as_u64()),
                (Some(cols as u64), Some(rows as u64)), "{frame}");
            let dirty = frame["dirty"].as_array().expect("grid has rows");
            assert_eq!(dirty.len(), rows as usize, "every row must arrive without PTY output");
            for (index, row) in dirty.iter().enumerate() {
                assert_eq!(row[0], index);
            }
            // A delayed reader/control frame must not undo the newly delivered dimensions.
            while let Ok(Some(Ok(Message::Text(text)))) = tokio::time::timeout(
                std::time::Duration::from_millis(60), ws.next(),
            ).await {
                let value: Value = serde_json::from_str(&text).unwrap();
                if value["t"] == "grid" {
                    assert_eq!((value["cols"].as_u64(), value["rows"].as_u64()),
                        (Some(cols as u64), Some(rows as u64)), "stale grid: {value}");
                }
            }
        }

        for legacy_cols in [0, 162] {
            let id = format!("quiet-viewport-test-{}", uuid::Uuid::new_v4());
            let (_events_tx, events_rx) = crossbeam_channel::unbounded();
            let sess = Arc::new(kasa_pty::PtySession::start_external(
                kasa_pty::PtyOptions { pane_id: id.clone(), cols: 21, rows: 6, ..Default::default() },
                kasa_pty::ExternalIo {
                    events: events_rx,
                    writer: Box::new(std::io::sink()),
                    on_resize: Arc::new(|_, _| {}),
                },
            ).unwrap());
            kasa_pty::register_session(&id, &sess);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let router = axum::Router::new()
                .route("/term/ws", axum::routing::get(super::term_ws_handler));
            let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
            let (mut ws, _) = tokio_tungstenite::connect_async(
                format!("ws://{addr}/term/ws?pane={id}&grid=1"),
            ).await.unwrap();
            assert_full_grid(&mut ws, 21, 6).await;
            if legacy_cols != 0 {
                ws.send(Message::Text(json!({"t":"view", "cols":legacy_cols}).to_string().into()))
                    .await.unwrap();
                assert_full_grid(&mut ws, legacy_cols, 6).await;
            }
            for (op, cols, rows) in [("acquire", 162, 43), ("resize", 49, 45), ("release", 21, 6)] {
                ws.send(Message::Text(json!({"t":"viewport", "op":op, "cols":cols, "rows":rows})
                    .to_string().into())).await.unwrap();
                assert_full_grid(&mut ws, cols, rows).await;
            }
            ws.close(None).await.unwrap();
            server.abort();
        }
    }

    #[tokio::test]
    async fn source_touch_tells_the_holding_raw_mirror_it_lost_before_reattaching() {
        use futures_util::{SinkExt, StreamExt};
        use serde_json::{json, Value};
        use std::sync::Arc;
        use tokio_tungstenite::tungstenite::Message;

        let id = format!("viewport-latest-{}", uuid::Uuid::new_v4());
        let (_events_tx, events_rx) = crossbeam_channel::unbounded();
        let sess = Arc::new(kasa_pty::PtySession::start_external(
            kasa_pty::PtyOptions { pane_id: id.clone(), cols: 32, rows: 23, ..Default::default() },
            kasa_pty::ExternalIo {
                events: events_rx,
                writer: Box::new(std::io::sink()),
                on_resize: Arc::new(|_, _| {}),
            },
        ).unwrap());
        kasa_pty::register_session(&id, &sess);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = axum::Router::new().route("/term/ws", axum::routing::get(super::term_ws_handler));
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/term/ws?pane={id}&own=0"))
            .await.unwrap();
        let mut texts = Vec::<Value>::new();
        let next_text = async |ws: &mut tokio_tungstenite::WebSocketStream<_>| -> Option<Value> {
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                loop {
                    match ws.next().await {
                        Some(Ok(Message::Text(text))) => return Some(serde_json::from_str(&text).unwrap()),
                        Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return None,
                        Some(Ok(_)) => {}
                    }
                }
            }).await.expect("websocket response timeout")
        };
        let size = next_text(&mut ws).await.unwrap();
        assert_eq!(size["capabilities"]["viewport_latest"], 1);
        ws.send(Message::Text(json!({"t":"viewport", "op":"acquire", "cols":180, "rows":50})
            .to_string().into())).await.unwrap();
        loop {
            let v = next_text(&mut ws).await.unwrap();
            if v["t"] == "viewport" { assert_eq!(v["granted"], true); break; }
        }
        assert_eq!(sess.size(), (180, 50));
        assert!(sess.reclaim_viewer_sizes().unwrap());
        assert_eq!(sess.size(), (32, 23));
        while let Some(v) = next_text(&mut ws).await {
            texts.push(v);
        }
        let lost = texts.iter().position(|v| v["t"] == "viewport" && v["lost"] == true);
        assert!(lost.is_some(), "the viewer must learn it lost the grid: {texts:?}");
        server.abort();
    }

    #[tokio::test]
    async fn viewport_websocket_disconnect_releases_only_its_own_lease() {
        use futures_util::{SinkExt, StreamExt};
        use serde_json::{json, Value};
        use std::sync::Arc;
        use tokio_tungstenite::{tungstenite::Message, MaybeTlsStream, WebSocketStream};

        async fn read_kind(
            ws: &mut WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
            kind: &str,
        ) -> Value {
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                while let Some(Ok(message)) = ws.next().await {
                    if let Message::Text(text) = message {
                        let value: Value = serde_json::from_str(&text).unwrap();
                        if value["t"] == kind {
                            return value;
                        }
                    }
                }
                panic!("websocket ended before {kind}");
            }).await.expect("websocket response timeout")
        }

        let id = format!("viewport-test-{}", uuid::Uuid::new_v4());
        let (events_tx, events_rx) = crossbeam_channel::unbounded();
        let sess = Arc::new(kasa_pty::PtySession::start_external(
            kasa_pty::PtyOptions { pane_id: id.clone(), cols: 21, rows: 6, ..Default::default() },
            kasa_pty::ExternalIo {
                events: events_rx,
                writer: Box::new(std::io::sink()),
                on_resize: Arc::new(|_, _| {}),
            },
        ).unwrap());
        kasa_pty::register_session(&id, &sess);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = axum::Router::new().route("/term/ws", axum::routing::get(super::term_ws_handler));
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let url = format!("ws://{addr}/term/ws?pane={id}&grid=1");
        let (mut first, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let size = read_kind(&mut first, "size").await;
        assert_eq!(size["capabilities"]["mirror_viewport"], 1);
        let acquire = json!({"t":"viewport", "op":"acquire", "cols":120, "rows":40});
        first.send(Message::Text(acquire.to_string().into())).await.unwrap();
        assert_eq!(read_kind(&mut first, "viewport").await["granted"], true);
        assert_eq!(sess.size(), (120, 40));

        let (mut second, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        read_kind(&mut second, "size").await;
        second.send(Message::Text(acquire.to_string().into())).await.unwrap();
        assert_eq!(read_kind(&mut second, "viewport").await["granted"], true);
        first.send(Message::Text(json!({
            "t":"viewport", "op":"resize", "cols":100, "rows":30,
        }).to_string().into())).await.unwrap();
        assert_eq!(read_kind(&mut first, "viewport").await["granted"], false);
        sess.resize(30, 8).unwrap();
        first.close(None).await.unwrap();
        second.send(Message::Text(json!({
            "t":"viewport", "op":"resize", "cols":120, "rows":40,
        }).to_string().into())).await.unwrap();
        assert_eq!(read_kind(&mut second, "viewport").await["granted"], true);
        assert_eq!(sess.size(), (120, 40));
        second.close(None).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while sess.has_viewer_size_control() {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        }).await.expect("disconnect did not release the viewport");
        assert_eq!(sess.size(), (30, 8));
        events_tx.send(kasa_pty::ExtEvent::Eof).unwrap();
        server.abort();
    }

    #[test]
    fn viewport_dimensions_reject_overflow_and_unbounded_allocations() {
        use serde_json::json;
        assert_eq!(super::viewport_dimensions(&json!({"cols": 120, "rows": 40})), Some((120, 40)));
        for value in [
            json!({"cols": 65536, "rows": 40}),
            json!({"cols": 120, "rows": 65536}),
            json!({"cols": 1, "rows": 40}),
            json!({"cols": 120, "rows": 0}),
            json!({"cols": "120", "rows": 40}),
            json!({"cols": 120}),
        ] {
            assert_eq!(super::viewport_dimensions(&value), None, "{value}");
        }
    }
}

#[cfg(test)]
mod raw_pane_tests {
    use super::raw_pane_param;

    #[test]
    fn keeps_the_percent_that_query_decoding_would_eat() {
        // pane id 는 `%1` 처럼 % 로 시작한다. 디코딩된 값은 제어문자가 되어
        // 조회에 실패하므로, 원문을 그대로 들고 있어야 한다.
        assert_eq!(raw_pane_param(Some("pane=%116")).as_deref(), Some("%116"));
        assert_eq!(
            raw_pane_param(Some("t=abc&pane=%25116")).as_deref(),
            Some("%25116")
        );
    }

    #[test]
    fn none_when_absent_or_empty() {
        assert_eq!(raw_pane_param(Some("pane=")), None);
        assert_eq!(raw_pane_param(Some("cwd=/tmp")), None);
        assert_eq!(raw_pane_param(None), None);
    }
}
