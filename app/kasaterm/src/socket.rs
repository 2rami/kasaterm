//! Backend impl that bridges agent-socket to this binary's TmuxSession.
//!
//! The single-pane PoC reports a fixed workspace + surface id ("local-0"
//! / "pane-0") because we only own one tmux pane in this binary. Once
//! kasaterm grows multi-pane support the surface ids
//! become real tmux `@N` strings and `list_surfaces` returns one entry
//! per actually-open pane.

use kasa_socket::backend::{
    Backend, PaneActivity, PaneRect, SessionsInfo, SplitDirection, SurfaceInfo, WorkspaceInfo,
};
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use kasa_bridge::{Layout, TmuxSession};

use crate::transcript::snapshot_from_tail;
use crate::{UserEvent, Workspace};
use winit::event_loop::EventLoopProxy;

const FIXED_WORKSPACE_ID: &str = "local-0";
const FIXED_SURFACE_ID: &str = "pane-0";

pub struct TmuxBackend {
    tmux: Arc<TmuxSession>,
}

impl TmuxBackend {
    pub fn new(tmux: Arc<TmuxSession>) -> Self {
        Self { tmux }
    }
}

impl Backend for TmuxBackend {
    fn list_workspaces(&self) -> Result<Vec<WorkspaceInfo>> {
        Ok(vec![WorkspaceInfo {
            id: FIXED_WORKSPACE_ID.into(),
            name: "kasaterm".into(),
        }])
    }

    fn current_workspace(&self) -> Result<Option<WorkspaceInfo>> {
        Ok(Some(WorkspaceInfo {
            id: FIXED_WORKSPACE_ID.into(),
            name: "kasaterm".into(),
        }))
    }

    fn list_surfaces(&self) -> Result<Vec<SurfaceInfo>> {
        Ok(vec![SurfaceInfo {
            id: FIXED_SURFACE_ID.into(),
            workspace_id: FIXED_WORKSPACE_ID.into(),
            title: None,
        }])
    }

    fn focus_surface(&self, _surface_id: &str) -> Result<()> {
        // Single pane — no-op. Multi-pane phase will route to tmux's
        // `select-pane -t <id>`.
        Ok(())
    }

    fn split_surface(&self, direction: SplitDirection, focus: bool) -> Result<SurfaceInfo> {
        // tmux's split-window takes -h for horizontal split, -v for
        // vertical. cmux's direction terminology is what *cell rows*
        // grow into — right/left are horizontal splits, up/down are
        // vertical. -b prepends the new pane before the current one,
        // which matches cmux's "left" / "up" semantics. `-d` keeps focus
        // on the current pane (no-focus default); omit it to follow.
        let base = match direction {
            SplitDirection::Right => "split-window -h",
            SplitDirection::Left => "split-window -hb",
            SplitDirection::Down => "split-window -v",
            SplitDirection::Up => "split-window -vb",
        };
        let cmd = if focus { base.to_string() } else { format!("{base} -d") };
        self.tmux.send_cmd(&cmd)?;
        // We don't have a way to get the new pane's tmux id back
        // synchronously yet — control-mode reports it via a layout-change
        // event which the host's flusher thread receives. For the PoC
        // return a placeholder that the caller can correlate later.
        Ok(SurfaceInfo {
            id: "pane-new".into(),
            workspace_id: FIXED_WORKSPACE_ID.into(),
            title: None,
        })
    }

    fn send_text(&self, _surface_id: Option<&str>, text: &str) -> Result<()> {
        // Single pane — surface_id ignored. Send as a hex-encoded
        // payload so newlines and escape sequences pass through tmux's
        // send-keys without quoting drama.
        let hex: String = text
            .bytes()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(" ");
        self.tmux.send_keys_hex(None, &hex)
    }

    fn send_key(&self, _surface_id: Option<&str>, key: &str) -> Result<()> {
        // Map cmux's symbolic key names to the byte sequences a terminal
        // emulator emits. Anything unknown gets forwarded as a literal
        // string so clients can send single characters via send_key too.
        let bytes = key_to_bytes(key);
        let hex: String = bytes
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(" ");
        self.tmux.send_keys_hex(None, &hex)
    }

    fn close_surface(&self, _surface_id: &str) -> Result<()> {
        anyhow::bail!("close_surface not supported on the tmux backend")
    }

    fn rename_surface(&self, _surface_id: &str, _title: &str) -> Result<()> {
        anyhow::bail!("rename_surface not supported on the tmux backend")
    }

    fn set_color(&self, _surface_id: &str, _color: [u8; 4]) -> Result<()> {
        anyhow::bail!("set_color not supported on the tmux backend")
    }

    fn swap_surfaces(&self, _a: &str, _b: &str) -> Result<()> {
        anyhow::bail!("swap_surfaces not supported on the tmux backend")
    }
}

/// Local PTY-mode cmux socket backend. The socket server (claude tmux shim,
/// kasaterm-cli, pane collab) runs on its own thread and can't touch
/// `App.pty` (a plain HashMap, not Arc<Mutex>), so every pane write / split /
/// focus is routed to the GUI thread through the EventLoopProxy.
pub struct PtyBackend {
    proxy: EventLoopProxy<UserEvent>,
    ws: Arc<Mutex<Workspace>>,
    /// surface_id → claude transcript path (hook-driven via `bind_transcript`).
    /// The single source of truth for the board: `collab_board` reads each
    /// pane's transcript tail *on demand* (pull) — there is no background
    /// watcher thread filling a cache.
    bound: Arc<Mutex<HashMap<String, PathBuf>>>,
    /// surface_id → why it's blocked (the `Notification` hook's message, may be
    /// ""). Set by `attention`, cleared by `notify` (turn done) or when the
    /// pane's transcript grows again (claude resumed). The board's only source
    /// of `waiting`: a blocked claude writes nothing, so the transcript tail
    /// can't tell `collab_board` the pane is stuck — this map can.
    attention: Arc<Mutex<HashMap<String, String>>>,
    /// Cached `claude agents --json` output (sessionId → official status:
    /// idle/busy/waiting). The board polls ~1/s; shelling out to `claude` that
    /// often both costs a process spawn and risks racing claude's session
    /// registry, so we refresh at most once every 2s.
    agents_cache: Arc<Mutex<Option<(std::time::Instant, HashMap<String, String>)>>>,
    /// hook-free 발견 스로틀 — `discover_unbound` 의 ps/lsof 비용을 board 폴(1/s)
    /// 마다 다 치르지 않도록 2s 에 1회로 제한한 마지막 실행 시각.
    last_discover: Arc<Mutex<Option<std::time::Instant>>>,
    /// pane 셸 pid → (조회시각, 라이브 cwd). collab_board 가 학생 경로(cd 반영)를
    /// transcript 가 아닌 PTY pid_cwd 로 채우되, lsof 비용을 2s 캐시로 제한한다.
    cwd_cache: Arc<Mutex<HashMap<u32, (std::time::Instant, std::path::PathBuf)>>>,
}

impl PtyBackend {
    /// `attention` is shared with the GUI (`App.collab.attention`): the CLI
    /// hook path (`kasaterm-cli attention`) and the GUI's grid-scan prompt
    /// detection both write it, so the board's `waiting` flag reflects either.
    pub fn new(
        proxy: EventLoopProxy<UserEvent>,
        ws: Arc<Mutex<Workspace>>,
        attention: Arc<Mutex<HashMap<String, String>>>,
    ) -> Self {
        Self {
            proxy,
            ws,
            bound: Arc::new(Mutex::new(HashMap::new())),
            attention,
            agents_cache: Arc::new(Mutex::new(None)),
            last_discover: Arc::new(Mutex::new(None)),
            cwd_cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// pane 셸 pid 의 라이브 cwd(pid_cwd) — 2s 캐시. cd 하면 곧 반영, lsof 폭주 방지.
    fn pane_cwd_live(&self, pid: u32) -> Option<std::path::PathBuf> {
        const TTL: std::time::Duration = std::time::Duration::from_secs(2);
        let now = std::time::Instant::now();
        if let Some((at, cwd)) = self.cwd_cache.lock().unwrap().get(&pid) {
            if now.duration_since(*at) < TTL {
                return Some(cwd.clone());
            }
        }
        let cwd = pid_cwd(pid)?;
        self.cwd_cache.lock().unwrap().insert(pid, (now, cwd.clone()));
        Some(cwd)
    }

    /// 모든 pane 의 `(surface_id, shell_pid)` — GUI 동기 RPC(메모리 즉답).
    fn query_pane_pids(&self) -> Vec<(String, u32)> {
        let (tx, rx) = std::sync::mpsc::channel();
        if self.proxy.send_event(UserEvent::SocketQueryPanePids(tx)).is_err() {
            return Vec::new();
        }
        rx.recv_timeout(std::time::Duration::from_millis(300)).unwrap_or_default()
    }

    /// hook-free 발견: bound 안 된 open pane 중 claude 실행 중인 것을 셸 pid 로
    /// 추적해 transcript 를 자동 bind 한다(claude 훅 없이도 board 가 학생을 본다).
    /// 락을 잡은 채 ps/lsof 를 호출하지 않는다(GUI 멈춤 lock-bug 회피) — pid 스냅샷·
    /// 발견은 락 밖, insert 만 짧게 락. 2s 스로틀로 폴마다 재스캔 안 함.
    fn discover_unbound(&self, live: &HashSet<String>) {
        {
            let mut last = self.last_discover.lock().unwrap();
            if last.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(2)) {
                return;
            }
            *last = Some(std::time::Instant::now());
        }
        let unbound: HashSet<String> = {
            let bound = self.bound.lock().unwrap();
            live.iter().filter(|id| !bound.contains_key(*id)).cloned().collect()
        };
        if unbound.is_empty() {
            return;
        }
        for (id, shell_pid) in self.query_pane_pids() {
            if !unbound.contains(&id) {
                continue;
            }
            if let Some(path) = discover_transcript(&id, shell_pid) {
                self.bound.lock().unwrap().insert(id, path);
            }
        }
    }

    /// sessionId → official claude status (idle/busy/waiting), cached 2s.
    /// `claude agents --json` is authoritative; the transcript-mtime heuristic
    /// in `read_tail`/`snapshot_from_tail` is only a fallback for sessions
    /// claude doesn't report. One sessionId can span several processes (shells
    /// inherit the parent's session id), so we collapse to the most-active
    /// state (busy > waiting > idle).
    fn agents_status(&self) -> HashMap<String, String> {
        const TTL: std::time::Duration = std::time::Duration::from_secs(2);
        let now = std::time::Instant::now();
        if let Some((at, map)) = self.agents_cache.lock().unwrap().as_ref() {
            if now.duration_since(*at) < TTL {
                return map.clone();
            }
        }
        let mut map: HashMap<String, String> = HashMap::new();
        if let Ok(out) = std::process::Command::new("claude")
            .args(["agents", "--json"])
            .output()
        {
            if out.status.success() {
                if let Ok(items) =
                    serde_json::from_slice::<Vec<serde_json::Value>>(&out.stdout)
                {
                    let rank = |s: &str| match s {
                        "busy" => 3,
                        "waiting" => 2,
                        _ => 1,
                    };
                    for it in &items {
                        let (Some(sid), Some(st)) = (
                            it.get("sessionId").and_then(|v| v.as_str()),
                            it.get("status").and_then(|v| v.as_str()),
                        ) else {
                            continue;
                        };
                        let e = map
                            .entry(sid.to_string())
                            .or_insert_with(|| st.to_string());
                        if rank(st) > rank(e) {
                            *e = st.to_string();
                        }
                    }
                }
            }
        }
        *self.agents_cache.lock().unwrap() = Some((now, map.clone()));
        map
    }
}

impl Backend for PtyBackend {
    fn list_workspaces(&self) -> Result<Vec<WorkspaceInfo>> {
        Ok(vec![WorkspaceInfo {
            id: FIXED_WORKSPACE_ID.into(),
            name: "kasaterm".into(),
        }])
    }

    fn current_workspace(&self) -> Result<Option<WorkspaceInfo>> {
        Ok(Some(WorkspaceInfo {
            id: FIXED_WORKSPACE_ID.into(),
            name: "kasaterm".into(),
        }))
    }

    /// 로컬 PTY 모드의 '방' = App 윈도우. GUI 스레드에 질의해(별 스레드라 직접 못 봄)
    /// 윈도우 수·활성 idx·라벨을 받는다. arona-ui 좌측 방 네비가 폴링한다(거노).
    fn sessions(&self) -> SessionsInfo {
        let (tx, rx) = std::sync::mpsc::channel();
        if self.proxy.send_event(UserEvent::SocketQuerySessions(tx)).is_err() {
            return SessionsInfo::default();
        }
        match rx.recv_timeout(std::time::Duration::from_millis(300)) {
            Ok((count, active, labels)) => SessionsInfo {
                count,
                active,
                saved: Vec::new(),
                // 방 이름 = 윈도우 라벨(name). cwd 가 있으면 부가 표기.
                labels: labels
                    .into_iter()
                    .map(|(name, cwd)| if cwd.is_empty() { name } else { format!("{name} · {cwd}") })
                    .collect(),
            },
            Err(_) => SessionsInfo::default(),
        }
    }

    /// `POST /session-switch?idx=N` — 방=윈도우 전환을 GUI 스레드에 위임.
    fn switch_session(&self, idx: usize) -> Result<()> {
        self.proxy
            .send_event(UserEvent::SocketSwitchSession(idx))
            .map_err(|_| anyhow::anyhow!("gui event loop gone"))?;
        Ok(())
    }

    /// `POST /session-new?god=<name>` — 새 방(윈도우) + 선택 god 스폰을 GUI 에 위임.
    fn new_room(&self, god: &str) -> Result<()> {
        self.proxy
            .send_event(UserEvent::SocketNewRoom(god.to_string()))
            .map_err(|_| anyhow::anyhow!("gui event loop gone"))?;
        Ok(())
    }

    /// 활성 pane(보이는 방)의 방 식별자 — 모모톡 inbox 등을 방별 격리(거노). ws 공유.
    fn active_room(&self) -> Option<String> {
        let ws = self.ws.lock().unwrap();
        ws.active_pane
            .as_ref()
            .and_then(|p| ws.pane_room.get(p).cloned())
    }

    /// `POST /session-close?idx=N` — 방(윈도우) 닫기를 GUI 스레드에 위임.
    fn close_session(&self, idx: usize) -> Result<()> {
        self.proxy
            .send_event(UserEvent::SocketCloseRoom(idx))
            .map_err(|_| anyhow::anyhow!("gui event loop gone"))?;
        Ok(())
    }

    fn list_surfaces(&self) -> Result<Vec<SurfaceInfo>> {
        let ws = self.ws.lock().unwrap();
        Ok(ws
            .panes
            .keys()
            .map(|id| SurfaceInfo {
                id: id.clone(),
                workspace_id: FIXED_WORKSPACE_ID.into(),
                title: None,
            })
            .collect())
    }

    /// Geometry of the visible window's panes as window-relative percentages,
    /// for `kasaterm-cli layout`'s ASCII diagram. The live tree lives in the
    /// GUI thread's `pty_layout`, but `publish_pty_layout` mirrors it into
    /// `ws.layout` (tmux-shape, cell coords) on every split/close/focus — so we
    /// read that here. `ws.layout` is `None` for a single pane (≤1 leaf), so we
    /// synthesize a full-window rect for the lone pane rather than report empty.
    fn window_layout(&self) -> Result<Vec<PaneRect>> {
        let ws = self.ws.lock().unwrap();
        if let Some(layout) = ws.layout.as_ref() {
            let (_, _, tw, th) = layout.rect();
            if tw == 0 || th == 0 {
                return Ok(Vec::new());
            }
            // round(v/total * 100); total is non-zero (guarded above).
            let pct = |v: u16, total: u16| -> u16 {
                ((v as u32 * 100 + total as u32 / 2) / total as u32) as u16
            };
            return Ok(layout
                .leaves()
                .into_iter()
                .filter_map(|leaf| {
                    let Layout::Pane { id, x, y, w, h } = leaf else {
                        return None; // leaves() yields only Pane nodes
                    };
                    Some(PaneRect {
                        surface_id: format!("%{id}"),
                        x: pct(*x, tw),
                        y: pct(*y, th),
                        w: pct(*w, tw),
                        h: pct(*h, th),
                    })
                })
                .collect());
        }
        // Single pane: one full-window box.
        Ok(ws
            .active_pane
            .clone()
            .or_else(|| ws.panes.keys().next().cloned())
            .map(|surface_id| {
                vec![PaneRect {
                    surface_id,
                    x: 0,
                    y: 0,
                    w: 100,
                    h: 100,
                }]
            })
            .unwrap_or_default())
    }

    fn focus_surface(&self, surface_id: &str) -> Result<()> {
        let _ = self
            .proxy
            .send_event(UserEvent::SocketFocus(surface_id.to_string()));
        Ok(())
    }

    fn reveal_terminal(&self, show: bool, focus_pane: Option<&str>) -> Result<()> {
        let _ = self.proxy.send_event(UserEvent::SocketRevealTerminal(
            show,
            focus_pane.map(str::to_string),
        ));
        Ok(())
    }

    fn close_arona(&self) -> Result<()> {
        let _ = self.proxy.send_event(UserEvent::SocketAronaClose);
        Ok(())
    }

    fn swap_surfaces(&self, a: &str, b: &str) -> Result<()> {
        // 검증은 여기(backend 스레드, ws 조회 가능)서 — GUI 위임은 fire-and-
        // forget 이라 저쪽 실패를 CLI 에 돌려줄 수 없다.
        if a == b {
            anyhow::bail!("swap needs two distinct panes (got {a} twice)");
        }
        {
            let ws = self.ws.lock().unwrap();
            for id in [a, b] {
                if !ws.panes.contains_key(id) {
                    anyhow::bail!("no such pane: {id}");
                }
            }
        }
        let _ = self
            .proxy
            .send_event(UserEvent::SocketSwap(a.to_string(), b.to_string()));
        Ok(())
    }

    fn set_split_ratio(&self, surface_id: &str, ratio: f32) -> Result<()> {
        if !(0.05..=0.95).contains(&ratio) {
            anyhow::bail!("ratio must be within 0.05..0.95 (got {ratio})");
        }
        {
            let ws = self.ws.lock().unwrap();
            if !ws.panes.contains_key(surface_id) {
                anyhow::bail!("no such pane: {surface_id}");
            }
            if ws.panes.len() < 2 {
                anyhow::bail!("no split to resize (single pane)");
            }
        }
        let _ = self
            .proxy
            .send_event(UserEvent::SocketSetRatio(surface_id.to_string(), ratio));
        Ok(())
    }

    /// 활성 pane 의 셸 cwd — GET /mode 등 협업방 판정의 기준. trait 디폴트
    /// (None→호스트 cwd 폴백)는 .app 실행 시 cwd 가 `/` 라 항상 solo 로
    /// 오판했다(거노 실측: god 방 토글 차단). GUI 동기 RPC 로 활성 pane 의
    /// shell pid 만 받고(메모리 즉답), lsof 해석은 이 backend 스레드서 한다 —
    /// pane_faces_user 라이브 우회와 같은 철학(캐시·프로세스 cwd 불신).
    fn active_cwd(&self) -> Option<std::path::PathBuf> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.proxy
            .send_event(UserEvent::SocketQueryActivePid(tx))
            .ok()?;
        // GUI 가 라이브 리사이즈 등으로 바쁠 수 있으니 짧게 대기, 실패 시
        // None → 호출부(resolve_cwd)의 기존 폴백 유지.
        let pid = rx
            .recv_timeout(std::time::Duration::from_millis(300))
            .ok()??;
        pid_cwd(pid)
    }

    fn rename_surface(&self, surface_id: &str, title: &str) -> Result<()> {
        let _ = self.proxy.send_event(UserEvent::SocketRename(
            surface_id.to_string(),
            title.to_string(),
        ));
        Ok(())
    }

    fn rename_window(&self, surface_id: &str, title: &str) -> Result<()> {
        let _ = self.proxy.send_event(UserEvent::SocketRenameWindow(
            surface_id.to_string(),
            title.to_string(),
        ));
        Ok(())
    }

    fn set_color(&self, surface_id: &str, color: [u8; 4]) -> Result<()> {
        let _ = self
            .proxy
            .send_event(UserEvent::SocketColor(surface_id.to_string(), color));
        Ok(())
    }

    fn split_surface(&self, direction: SplitDirection, focus: bool) -> Result<SurfaceInfo> {
        let dir = match direction {
            SplitDirection::Right | SplitDirection::Left => kasa_pty::SplitDir::Horizontal,
            SplitDirection::Up | SplitDirection::Down => kasa_pty::SplitDir::Vertical,
        };
        // Split runs on the GUI thread; block on a reply channel so we can hand
        // the new pane's real id back to the caller. The teammate launcher uses
        // it as the `-t` target for every follow-up send-keys — returning the
        // old "pane-new" placeholder dropped the `claude …` launch silently.
        // `focus` rides along so the GUI thread keeps focus on the current pane
        // unless the caller opted in (CLI `--focus`).
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = self.proxy.send_event(UserEvent::SocketSplit(dir, focus, tx));
        let id = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "pane-new".into());
        Ok(SurfaceInfo {
            id,
            workspace_id: FIXED_WORKSPACE_ID.into(),
            title: None,
        })
    }

    fn close_surface(&self, surface_id: &str) -> Result<()> {
        // 로컬 PTY 모드: close 도 split/focus 처럼 GUI 스레드에 위임(App.pty 는
        // 별도 스레드서 못 만짐). layout.rs close_pane 이 leaf 제거 + 다음 pane
        // 으로 포커스 이동까지 한다.
        let _ = self
            .proxy
            .send_event(UserEvent::SocketClose(surface_id.to_string()));
        Ok(())
    }

    fn send_text(&self, surface_id: Option<&str>, text: &str) -> Result<()> {
        let _ = self.proxy.send_event(UserEvent::SocketBytes(
            surface_id.map(|s| s.to_string()),
            text.as_bytes().to_vec(),
        ));
        Ok(())
    }

    fn send_key(&self, surface_id: Option<&str>, key: &str) -> Result<()> {
        let _ = self.proxy.send_event(UserEvent::SocketBytes(
            surface_id.map(|s| s.to_string()),
            key_to_bytes(key),
        ));
        Ok(())
    }

    fn open_preview(&self, _kind: &str, path: &str, _target: Option<&str>) -> Result<()> {
        // imgopen/mdopen 셰임·SendUserFile 훅 → 미리보기 pane split. 로컬 PTY 모드는
        // App.pty 를 별도 스레드서 못 만져 GUI 에 위임(open_file_split 이 확장자로
        // 이미지/마크다운/텍스트 분기·디코드·split 까지 한다). 데몬 제거로 빠졌던 것.
        let _ = self
            .proxy
            .send_event(UserEvent::SocketOpenPreview(path.to_string()));
        Ok(())
    }

    fn bind_transcript(&self, surface_id: &str, path: &str) -> Result<()> {
        // Record the pane's transcript path; `collab_board`/`transcript_tail`
        // read it on demand. Re-binding (claude --resume swaps the jsonl)
        // replaces the entry rather than stacking.
        self.bound
            .lock()
            .unwrap()
            .insert(surface_id.to_string(), PathBuf::from(path));
        Ok(())
    }

    fn peek(&self, surface_id: &str, lines: usize) -> Result<String> {
        let ws = self.ws.lock().unwrap();
        let pane = ws
            .panes
            .get(surface_id)
            .ok_or_else(|| anyhow::anyhow!("no such pane: {surface_id}"))?;
        Ok(pane.visible_text(lines))
    }

    fn peek_ansi(&self, surface_id: &str, lines: usize) -> Result<String> {
        let ws = self.ws.lock().unwrap();
        let pane = ws
            .panes
            .get(surface_id)
            .ok_or_else(|| anyhow::anyhow!("no such pane: {surface_id}"))?;
        Ok(pane.visible_text_ansi(lines))
    }

    fn transcript_tail(
        &self,
        surface_id: &str,
        turns: usize,
    ) -> Result<Vec<kasa_socket::backend::ConversationTurn>> {
        let path = self
            .bound
            .lock()
            .unwrap()
            .get(surface_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("pane {surface_id} has no bound transcript"))?;
        // Read the whole jsonl, parse every line to a turn, keep the last N.
        // Transcripts are line-appended and rarely huge; a full read keeps this
        // simple and correct (no offset bookkeeping like the watcher needs).
        let text = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("read transcript {path:?}: {e}"))?;
        let mut all: Vec<kasa_socket::backend::ConversationTurn> =
            text.lines().filter_map(crate::transcript::parse_turn).collect();
        if turns > 0 && all.len() > turns {
            all.drain(0..all.len() - turns);
        }
        Ok(all)
    }

    fn collab_board(&self) -> Result<Vec<PaneActivity>> {
        // Pull, not push: read each open & bound pane's transcript tail right
        // now and derive its row. No background watcher, no cache — the board
        // is exactly as fresh as the moment it's asked for. Panes with no hook
        // bind (no claude / not started) simply don't appear.
        let live: HashSet<String> = self.ws.lock().unwrap().panes.keys().cloned().collect();
        // hook-free 발견 — claude 훅(bind-transcript)이 안 걸린 pane 도 PTY 소유를
        // 이용해 직접 추적·bind(스로틀 2s). 훅은 빠른 보조 경로일 뿐, 이게 안전망.
        self.discover_unbound(&live);
        let agents = self.agents_status();
        // claude 가 실제 생성 중이면 화면 푸터에 스피너+"esc to interrupt" 가 뜬다.
        // mtime(60s) 휴리스틱이 ESC/완료 후에도 working 으로 stuck 이라(거노), 이 화면
        // 신호를 mtime-fallback(아래 None 분기)의 진짜 working 기준으로 쓴다.
        let generating: HashSet<String> = {
            let ws = self.ws.lock().unwrap();
            live.iter()
                .filter(|sid| {
                    ws.panes
                        .get(sid.as_str())
                        .map(|p| screen_shows_working(&p.visible_text(14)))
                        .unwrap_or(false)
                })
                .cloned()
                .collect()
        };
        let bound = self.bound.lock().unwrap();
        let mut attention = self.attention.lock().unwrap();
        // 방별 분리(거노): 각 pane 의 god/character 는 *그 pane 의 방(room)* collab dir
        // 에서 읽는다 — 같은 cwd 라도 방마다 god 가 다르다(아로나 방/프라나 방). pane_room
        // 없으면(기본 방) 기존 cwd-slug. ws(공유)에서 복제해 아래 map 클로저서 쓴다.
        // active_window_panes: 보이는 방(윈도우)의 pane — board 를 활성 방으로 한정
        // (거노: 아로나 방+프라나 방이 한 교실에 같이 뜸). 비었으면(초기) 필터 안 함.
        let (pane_room, active_panes) = {
            let ws = self.ws.lock().unwrap();
            (ws.pane_room.clone(), ws.active_window_panes.clone())
        };
        let mut board: Vec<PaneActivity> = bound
            .iter()
            // 활성 방 학생만(방별 격리). active_panes 가 비었으면(초기 미발행) live 로 폴백.
            .filter(|(sid, _)| {
                live.contains(sid.as_str())
                    && (active_panes.is_empty() || active_panes.contains(sid.as_str()))
            })
            .map(|(sid, path)| {
                let (tail, mtime_idle) = read_tail(path, 64 * 1024);
                let mut row = snapshot_from_tail(sid, &tail, mtime_idle);
                // Prefer claude's official status when it reports this session
                // (matched by transcript filename stem == sessionId). The
                // mtime heuristic above is only a fallback for sessions claude
                // doesn't list. `effectively_idle` then drives the attention
                // (permission-prompt) override below.
                let official = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .and_then(|stem| agents.get(stem))
                    .map(|s| s.as_str());
                let effectively_idle = match official {
                    Some("busy") => {
                        row.status = "working".into();
                        false
                    }
                    Some("waiting") => {
                        row.status = "waiting".into();
                        false
                    }
                    Some(_) => {
                        row.status = "idle".into();
                        true
                    }
                    None => {
                        // mtime 만 보면 ESC 취소·완료 후 60s 간 working 으로 stuck →
                        // 화면에 생성중 스피너가 있을 때만 working, 없으면 idle 로 교정
                        // (거노: ESC 눌러서 취소해도 '생각 중' 으로 남는 문제).
                        if generating.contains(sid.as_str()) {
                            row.status = "working".into();
                            false
                        } else {
                            row.status = "idle".into();
                            true
                        }
                    }
                };
                // A claude blocked on a permission/input prompt writes nothing
                // and reports idle, so its Notification hook flag is the only
                // `waiting` signal. Apply it only when otherwise idle; drop the
                // stale flag once the pane is active again.
                if effectively_idle {
                    if let Some(reason) = attention.get(sid) {
                        row.status = "waiting".to_string();
                        row.waiting_for = (!reason.is_empty()).then(|| reason.clone());
                    }
                } else {
                    attention.remove(sid);
                }
                // 이 pane 의 방 collab dir = cwd-slug(+ 방이면 __room_<id>). god/character
                // 둘 다 여기서 읽어 방별로 분리(거노: 프라나 방에 시로코 뜨던 버그).
                let base_slug = path
                    .parent()
                    .and_then(|d| d.file_name())
                    .and_then(|s| s.to_str())
                    .unwrap_or("");
                let rslug = match pane_room.get(sid.as_str()) {
                    Some(r) => format!("{base_slug}__room_{r}"),
                    None => base_slug.to_string(),
                };
                // god 판정 — 이 방의 lead == 나?
                let lead = std::fs::read_to_string(format!("/tmp/kasaterm-collab/{rslug}/lead"))
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
                row.is_god = lead.as_deref() == Some(sid.as_str());
                // 캐릭터 마커(assign-character) — 이 방 dir 의 character-<N>.
                row.character = std::fs::read_to_string(format!(
                    "/tmp/kasaterm-collab/{rslug}/character-{}",
                    sid.trim_start_matches('%')
                ))
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
                row
            })
            .collect();
        // Drop flags for panes that have closed since they were set.
        attention.retain(|sid, _| live.contains(sid.as_str()));
        // 학생 경로(cwd)를 PTY 셸 pid 의 라이브 cwd 로 덮어쓴다 — transcript 가 stale
        // 하거나(claude 가 jsonl 미기록) cd 직후라도 즉시 반영(2s 캐시). 아래 git
        // 브랜치도 이 라이브 cwd 기준이 되도록 branch 조회 전에 한다.
        let pane_pids: HashMap<String, u32> = self.query_pane_pids().into_iter().collect();
        // 컨텍스트 % — claude TUI 상태바에서 파싱(transcript 토큰이 0 이어도 robust).
        // 화면 스냅샷은 in-memory(visible_text)라 싸다 — 락 짧게.
        let screens: HashMap<String, String> = {
            let ws = self.ws.lock().unwrap();
            board
                .iter()
                .filter_map(|r| ws.panes.get(&r.surface_id).map(|p| (r.surface_id.clone(), p.visible_text(8))))
                .collect()
        };
        for row in &mut board {
            if let Some(&pid) = pane_pids.get(&row.surface_id) {
                if let Some(cwd) = self.pane_cwd_live(pid) {
                    row.cwd = cwd.to_string_lossy().into_owned();
                }
            }
            if let Some(screen) = screens.get(&row.surface_id) {
                if let Some(pct) = parse_context_pct(screen) {
                    row.context_pct = pct;
                }
                // 모델명도 상태바에서 — "Opus 4.8 (1M context)" 처럼 1M 변형까지 정확.
                if let Some(m) = parse_status_model(screen) {
                    row.model = m;
                }
            }
        }
        // git 브랜치 — pane cwd(transcript)에서 rev-parse. distinct cwd 1회씩(같은
        // 방 학생들이 cwd 공유)으로 git 호출을 최소화한다.
        let mut branch_cache: HashMap<String, Option<String>> = HashMap::new();
        for row in &mut board {
            if row.cwd.is_empty() {
                continue;
            }
            let cwd = row.cwd.clone();
            row.branch = branch_cache
                .entry(cwd.clone())
                .or_insert_with(|| {
                    std::process::Command::new("git")
                        .args(["rev-parse", "--abbrev-ref", "HEAD"])
                        .current_dir(&cwd)
                        .output()
                        .ok()
                        .filter(|o| o.status.success())
                        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                        .filter(|b| !b.is_empty() && b != "HEAD")
                })
                .clone();
        }
        board.sort_by(|a, b| a.surface_id.cmp(&b.surface_id));
        Ok(board)
    }

    fn notify(&self, surface_id: &str, title: &str, body: &str) -> Result<()> {
        // The turn finished → the pane can't still be blocked waiting. Clear any
        // attention flag so the board drops back to idle even if the resume
        // didn't write enough transcript to flip `idle` first.
        self.attention.lock().unwrap().remove(surface_id);
        // Hand off to the GUI thread — the desktop alert (objc/osascript) and
        // any pane/sidebar flash both need App state we can't touch here.
        let _ = self.proxy.send_event(UserEvent::Notify {
            surface_id: surface_id.to_string(),
            title: title.to_string(),
            body: body.to_string(),
        });
        Ok(())
    }

    fn attention(&self, surface_id: &str, reason: &str) -> Result<()> {
        // Remember it for the board (socket-side, pull), then hand the GUI-side
        // surfacing (toast / flash / desktop alert) to the GUI thread.
        self.attention
            .lock()
            .unwrap()
            .insert(surface_id.to_string(), reason.to_string());
        let _ = self.proxy.send_event(UserEvent::Attention {
            surface_id: surface_id.to_string(),
            reason: reason.to_string(),
        });
        Ok(())
    }
}

/// Peel any trailing submit bytes (CR/LF) off `bytes`, returning
/// `(body, submit)` so a caller can ship them in two separate PTY writes.
///
/// `kasaterm-cli tell` appends `\r` to the message. When the body ends in a
/// multibyte codepoint (한글·이모지) and that codepoint shares a single write
/// with the trailing `\r`, claude (Ink) can submit on the CR before the
/// last codepoint's bytes finish arriving across the read boundary — the
/// half-delivered character is truncated into a lone UTF-16 high surrogate
/// (`\ud83c` with no low half). That poisons the session's saved transcript
/// and every later API request 400s ("no low surrogate in string"). Writing
/// the body first, then the CR on its own, keeps the codepoint whole.
pub(crate) fn split_trailing_submit(bytes: &[u8]) -> (&[u8], &[u8]) {
    let body_len = bytes
        .iter()
        .rposition(|&b| b != b'\r' && b != b'\n')
        .map_or(0, |i| i + 1);
    bytes.split_at(body_len)
}

/// Shared key-to-bytes table used by both TmuxBackend and PtyBackend so
/// the wire-level interpretation is identical no matter which backend
/// is wired up. Returns a `Vec<u8>` so the literal-fallback path (when
/// the key isn't a recognized symbolic name) can borrow the original
/// `str`'s bytes without lifetime gymnastics.
pub(crate) fn key_to_bytes(key: &str) -> Vec<u8> {
    match key {
        "enter" => b"\r".to_vec(),
        "tab" => b"\t".to_vec(),
        "escape" => b"\x1b".to_vec(),
        "backspace" => b"\x7f".to_vec(),
        "delete" => b"\x1b[3~".to_vec(),
        "up" => b"\x1b[A".to_vec(),
        "down" => b"\x1b[B".to_vec(),
        "right" => b"\x1b[C".to_vec(),
        "left" => b"\x1b[D".to_vec(),
        other => other.as_bytes().to_vec(),
    }
}



/// Read the last `max_bytes` of a file as lossy UTF-8, plus whether it's gone
/// idle (no write in 60s — claude transcripts are append-only, so file mtime
/// is the last activity time; no need to parse ISO timestamps). The leading
/// (possibly mid-line) fragment of a tail read just fails to parse in
/// `snapshot_from_tail`, so it's harmless. Any IO error → empty + idle.
/// claude 가 실제로 생성 중이면 라이브 푸터에 "✳ Verbing… (12s · esc to interrupt)"
/// 가 뜬다. `rows_show_working`(input.rs)의 문자열판 — visible_text 의 마지막 비공백
/// 행들을 본다. transcript mtime(60s) 휴리스틱은 ESC 취소·완료 후에도 working 으로
/// stuck 이라(거노: ESC 눌러도 생각 중), 이 화면 신호를 mtime-fallback 의 진짜
/// working 기준으로 쓴다. 완료 요약("✻ Churned for 42s")은 별은 있어도 말줄임표가
/// 없어 제외된다.
fn screen_shows_working(screen: &str) -> bool {
    screen.lines().rev().take(10).any(|line| {
        if line.contains("esc to interrupt") {
            return true;
        }
        let has_star = line.chars().any(|c| (0x2720..=0x274F).contains(&(c as u32)));
        let has_braille = line.chars().any(|c| (0x2800..=0x28FF).contains(&(c as u32)));
        (has_star && line.contains('…')) || has_braille
    })
}

fn read_tail(path: &std::path::Path, max_bytes: u64) -> (String, bool) {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return (String::new(), true);
    };
    let meta = f.metadata().ok();
    let len = meta.as_ref().map(|m| m.len()).unwrap_or(0);
    let idle = meta
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.elapsed().ok())
        .map(|d| d.as_secs() >= 60)
        .unwrap_or(true);
    if len > max_bytes {
        let _ = f.seek(SeekFrom::Start(len - max_bytes));
    }
    let mut buf = Vec::new();
    let _ = f.read_to_end(&mut buf);
    (String::from_utf8_lossy(&buf).into_owned(), idle)
}

/// Resolve a process's current working directory via lsof. macOS has no
/// `/proc`; `lsof -d cwd` prints the cwd path. Called ~once/sec by the git
/// panel poll, so the subprocess cost is acceptable.
#[cfg(unix)]
pub(crate) fn pid_cwd(pid: u32) -> Option<std::path::PathBuf> {
    let out = std::process::Command::new("lsof")
        .args(["-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix('n').map(std::path::PathBuf::from))
}

/// Windows has no `lsof`. A process's live cwd lives in its PEB
/// (`ProcessParameters.CurrentDirectory.DosPath`), so we open the target,
/// resolve the PEB base via `NtQueryInformationProcess`, then `ReadProcessMemory`
/// our way down: PEB+0x20 → ProcessParameters pointer, +0x38 → the cwd
/// `UNICODE_STRING`, then its buffer. Offsets are the stable x64 PEB/
/// RTL_USER_PROCESS_PARAMETERS layout (windows-sys doesn't expose the fields).
#[cfg(windows)]
pub(crate) fn pid_cwd(pid: u32) -> Option<std::path::PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, NTSTATUS};
    use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
    };

    // windows-sys 0.59 dropped the ntdll process-info bindings, so declare the
    // one call we need. ProcessBasicInformation (class 0) returns the PEB base.
    #[repr(C)]
    struct ProcessBasicInfo {
        exit_status: NTSTATUS,
        peb_base_address: *mut std::ffi::c_void,
        affinity_mask: usize,
        base_priority: i32,
        unique_process_id: usize,
        inherited_from_unique_process_id: usize,
    }
    #[link(name = "ntdll")]
    extern "system" {
        fn NtQueryInformationProcess(
            handle: HANDLE,
            class: i32,
            info: *mut std::ffi::c_void,
            len: u32,
            ret_len: *mut u32,
        ) -> NTSTATUS;
    }

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, 0, pid);
        if handle.is_null() {
            return None;
        }
        let read_mem = |addr: usize, buf: *mut std::ffi::c_void, len: usize| -> bool {
            let mut got = 0usize;
            ReadProcessMemory(handle, addr as *const _, buf, len, &mut got) != 0 && got == len
        };
        let result = (|| {
            let mut pbi: ProcessBasicInfo = std::mem::zeroed();
            let mut ret_len = 0u32;
            let status = NtQueryInformationProcess(
                handle,
                0, // ProcessBasicInformation
                &mut pbi as *mut _ as *mut _,
                std::mem::size_of::<ProcessBasicInfo>() as u32,
                &mut ret_len,
            );
            if status != 0 {
                return None;
            }
            let peb = pbi.peb_base_address as usize;
            if peb == 0 {
                return None;
            }
            // PEB+0x20 = ProcessParameters pointer (x64).
            let mut params: usize = 0;
            if !read_mem(peb + 0x20, &mut params as *mut _ as *mut _, std::mem::size_of::<usize>()) || params == 0 {
                return None;
            }
            // ProcessParameters+0x38 = CurrentDirectory.DosPath UNICODE_STRING
            // { u16 Length, u16 MaximumLength, u32 _pad, u64 Buffer } (x64).
            let mut us: [u8; 16] = [0; 16];
            if !read_mem(params + 0x38, us.as_mut_ptr() as *mut _, 16) {
                return None;
            }
            let length = u16::from_le_bytes([us[0], us[1]]) as usize;
            let buffer = u64::from_le_bytes([
                us[8], us[9], us[10], us[11], us[12], us[13], us[14], us[15],
            ]) as usize;
            if length == 0 || buffer == 0 {
                return None;
            }
            let mut wide = vec![0u16; length / 2];
            if !read_mem(buffer, wide.as_mut_ptr() as *mut _, length) {
                return None;
            }
            let s = std::ffi::OsString::from_wide(&wide);
            Some(std::path::PathBuf::from(s))
        })();
        CloseHandle(handle);
        result
    }
}


/// Build one layout-tree leaf's restore record from a live PtySession: its
/// cwd, whether it was running claude, and the newest claude session id under
/// that cwd (for `claude --resume`). `cwd` is null when the shell pid/cwd
/// can't be resolved — restore then falls back to the default cwd.
pub fn pane_record(sess: &kasa_pty::PtySession) -> serde_json::Value {
    let shell_pid = sess.shell_pid();
    let cwd = shell_pid.and_then(pid_cwd);
    let was_claude = sess
        .active_process_name()
        .map_or(false, |p| p.contains("claude"));
    // Only record a session id for panes actually running claude. Prefer the
    // id straight off the running claude's argv (exact per-pane); two claudes
    // in the same cwd no longer collapse onto one id the way the cwd-mtime
    // guess does. Fall back to the mtime guess for a fresh `claude` whose argv
    // carries no id. Crucially the mtime fallback is INSIDE the was_claude
    // guard — otherwise a plain shell pane (no claude) would still get the
    // cwd's newest session id stapled on, so every pane sharing a cwd collapsed
    // onto one id and `claude --resume` restored the wrong/duplicate session.
    let session_id = if was_claude {
        shell_pid
            .and_then(claude_session_id_from_cmdline)
            .or_else(|| cwd.as_ref().and_then(|c| latest_claude_session_id(c)))
    } else {
        None
    };
    serde_json::json!({
        "cwd": cwd.as_ref().map(|c| c.to_string_lossy().into_owned()),
        "was_claude": was_claude,
        "session_id": session_id,
    })
}

/// Write the full multi-session restore state (built by the caller from each
/// session's layout tree). Written on exit, read by start_pty. Best-effort;
/// failures are silent.
pub fn write_session_state(state: &serde_json::Value) {
    use std::io::Write;
    let Some(path) = session_file_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::File::create(&path) {
        let _ = f.write_all(state.to_string().as_bytes());
    }
}

pub fn session_file_path() -> Option<std::path::PathBuf> {
    // Override lets a debug instance keep its restore state out of the daily
    // app's shared file (and lets users relocate it).
    if let Ok(p) = std::env::var("KASATERM_SESSION_FILE") {
        if !p.is_empty() {
            return Some(std::path::PathBuf::from(p));
        }
    }
    let home = std::env::var("HOME").ok()?;
    Some(std::path::PathBuf::from(home).join(".config/kasaterm/session.json"))
}

fn window_size_path() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("KASATERM_WINDOW_FILE") {
        if !p.is_empty() {
            return Some(std::path::PathBuf::from(p));
        }
    }
    let home = std::env::var("HOME").ok()?;
    Some(std::path::PathBuf::from(home).join(".config/kasaterm/window.json"))
}

fn settings_file_path() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("KASATERM_SETTINGS_FILE") {
        if !p.is_empty() {
            return Some(std::path::PathBuf::from(p));
        }
    }
    let home = std::env::var("HOME").ok()?;
    Some(std::path::PathBuf::from(home).join(".config/kasaterm/settings.json"))
}

/// User's `default_cwd` preference for where new shells start — mirrors the
/// "working directory" setting every other terminal exposes. Returns the raw
/// string: `"last"` (inherit the spawning pane's cwd, the standard default),
/// `"home"`, or an absolute/`~`-prefixed path. Missing file/key → `"last"`.
pub fn read_default_cwd_mode() -> String {
    let fallback = || "last".to_string();
    let Some(path) = settings_file_path() else { return fallback() };
    let Ok(txt) = std::fs::read_to_string(&path) else { return fallback() };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) else { return fallback() };
    v.get("default_cwd")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
        .unwrap_or_else(fallback)
}

/// Whole `settings.json` as a JSON object (empty object if missing/invalid).
/// The settings screen reads this once to populate its controls.
pub fn read_settings() -> serde_json::Value {
    let empty = || serde_json::json!({});
    let Some(path) = settings_file_path() else { return empty() };
    let Ok(txt) = std::fs::read_to_string(&path) else { return empty() };
    serde_json::from_str::<serde_json::Value>(&txt)
        .ok()
        .filter(|v| v.is_object())
        .unwrap_or_else(empty)
}

/// Set one key in `settings.json`, preserving every other key. Loads the
/// existing object first so writing `default_shell` never clobbers
/// `default_cwd`. Silently no-ops if the path/dir can't be resolved.
pub fn write_setting(key: &str, value: serde_json::Value) {
    use std::io::Write;
    let Some(path) = settings_file_path() else { return };
    let mut obj = match read_settings() {
        serde_json::Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    obj.insert(key.to_string(), value);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(txt) = serde_json::to_string_pretty(&serde_json::Value::Object(obj)) {
        if let Ok(mut f) = std::fs::File::create(&path) {
            let _ = f.write_all(txt.as_bytes());
        }
    }
}

/// Whether the file-tree sidebar starts open on launch. Default `false`
/// (terminal-only first screen).
pub fn read_file_tree_default() -> bool {
    read_settings()
        .get("file_tree_default")
        .and_then(|x| x.as_bool())
        .unwrap_or(false)
}

/// User's preferred shell override (`default_shell` key). Empty/missing → None,
/// letting `$SHELL`/login-shell detection take over.
pub fn read_default_shell() -> Option<String> {
    read_settings()
        .get("default_shell")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// Persist the last logical window size so the next launch restores it instead
/// of the hardcoded default. Logical (DPI-independent) so moving between a
/// Retina and an external display restores the same on-screen size.
pub fn write_window_size(w: f64, h: f64) {
    use std::io::Write;
    let Some(path) = window_size_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::File::create(&path) {
        let _ = f.write_all(format!("{{\"w\":{w},\"h\":{h}}}").as_bytes());
    }
}

/// Read the persisted logical window size. Rejects degenerate sizes (a window
/// minimized/zero at exit) so a bad value can't trap the next launch tiny.
pub fn read_window_size() -> Option<(f64, f64)> {
    let path = window_size_path()?;
    let txt = std::fs::read_to_string(&path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&txt).ok()?;
    let w = v.get("w")?.as_f64()?;
    let h = v.get("h")?.as_f64()?;
    if w >= 400.0 && h >= 300.0 {
        Some((w, h))
    } else {
        None
    }
}


/// Pull the claude session id straight off the running claude process's argv
/// (`--resume <uuid>` / `--session-id <uuid>`, `=`-joined or space-separated).
/// Exact per-pane — unlike the cwd-mtime guess, two claudes in the same cwd
/// keep distinct ids. Returns None for a fresh `claude` with no id on its argv.
fn claude_session_id_from_cmdline(shell_pid: u32) -> Option<String> {
    // Most-recently-spawned claude child of this shell — shared with the
    // transcript watcher's self-map path.
    let pid = claude_child_pid(shell_pid)?;
    let args_out = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "args="])
        .output()
        .ok()?;
    let argv = String::from_utf8_lossy(&args_out.stdout);
    let tokens: Vec<&str> = argv.split_whitespace().collect();
    for (i, tok) in tokens.iter().enumerate() {
        for flag in ["--resume=", "--session-id="] {
            if let Some(v) = tok.strip_prefix(flag) {
                if is_uuid(v) {
                    return Some(v.to_string());
                }
            }
        }
        if matches!(*tok, "--resume" | "-r" | "--session-id") {
            if let Some(v) = tokens.get(i + 1) {
                if is_uuid(v) {
                    return Some((*v).to_string());
                }
            }
        }
    }
    None
}

/// The pid of the claude child of a shell pane, if any. Picks the most-recent
/// (highest-pid) `claude`-named direct child of `shell_pid`. Returns None when
/// no claude is running under the shell.
fn claude_child_pid(shell_pid: u32) -> Option<u32> {
    let out = std::process::Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,comm="])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let mut best: Option<u32> = None;
    for line in s.lines() {
        let mut parts = line.split_whitespace();
        let (pid, ppid) = match (
            parts.next().and_then(|x| x.parse::<u32>().ok()),
            parts.next().and_then(|x| x.parse::<u32>().ok()),
        ) {
            (Some(a), Some(b)) => (a, b),
            _ => continue,
        };
        if ppid != shell_pid {
            continue;
        }
        let comm = parts.collect::<Vec<_>>().join(" ");
        let is_claude = std::path::Path::new(&comm)
            .file_name()
            .and_then(|x| x.to_str())
            .map_or(false, |n| n.contains("claude"));
        if is_claude && best.map_or(true, |p| pid > p) {
            best = Some(pid);
        }
    }
    best
}

/// claude session ids are canonical UUIDs (8-4-4-4-12 hex). Validating guards
/// against grabbing a non-id token after a bare `-r`/`--resume` (the picker).
fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, &c)| match i {
            8 | 13 | 18 | 23 => c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// claude stores sessions under ~/.claude/projects/<encoded-cwd>/<uuid>.jsonl,
/// where the abs cwd is encoded by replacing `/` and `.` with `-`. The newest
/// .jsonl there is the session the pane was last on.
fn latest_claude_session_id(cwd: &std::path::Path) -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let encoded = cwd.to_string_lossy().replace(['/', '.'], "-");
    let dir = std::path::PathBuf::from(home)
        .join(".claude/projects")
        .join(encoded);
    let mut newest: Option<(std::time::SystemTime, String)> = None;
    for entry in std::fs::read_dir(&dir).ok()?.flatten() {
        let p = entry.path();
        if p.extension().and_then(|x| x.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(mtime) = entry.metadata().and_then(|m| m.modified()) else { continue };
        let Some(id) = p.file_stem().and_then(|x| x.to_str()) else { continue };
        if newest.as_ref().map_or(true, |(t, _)| mtime > *t) {
            newest = Some((mtime, id.to_string()));
        }
    }
    newest.map(|(_, id)| id)
}

/// hook-free transcript 발견 — 우리는 PTY 를 자체 소유하므로, 셸 pid 만으로
/// claude 자식·cwd·session 을 직접 추적해 transcript(.jsonl) 경로를 알아낸다.
/// claude 훅(bind-transcript)이 없어도 board 가 학생을 인지한다(munder 는 claude
/// 를 감싸기만 해 훅 의존; 우리는 터미널을 소유해 프로세스째 들여다본다). claude
/// 미실행이면 None(plain 셸). 같은 cwd 두 claude 는 argv session id 로 구분되고,
/// argv 에 id 없는 fresh claude 는 cwd 의 newest jsonl 로 폴백한다(pane_record 와
/// 동일 규칙). 느린 ps/lsof 호출이라 호출부(`discover_unbound`)에서 스로틀한다.
fn discover_transcript(pane_id: &str, shell_pid: u32) -> Option<std::path::PathBuf> {
    claude_child_pid(shell_pid)?; // claude 자식 없으면 아직 claude 아님 — bind 안 함
    let cwd = pid_cwd(shell_pid)?;
    // 1순위: bind-transcript 훅이 기록한 agent-roster(pane↔session, 정확). 훅이
    // 자기 transcript_path 를 보고하므로 공유 cwd 추측이 필요 없는 진짜 정답.
    // 소켓 bind 가 (타이밍 등으로) 씹혀도 이 파일은 남아 자가복구된다.
    if let Some(path) = roster_transcript(pane_id, &cwd) {
        return Some(path);
    }
    // 2순위: argv 의 session id(exact) — --resume/--session-id claude.
    if let Some(id) = claude_session_id_from_cmdline(shell_pid) {
        return project_jsonl(&cwd, &id).filter(|p| p.exists());
    }
    // 폴백: cwd 의 최근(<30분) 활동 jsonl. 단 **정확히 1개일 때만** bind 한다.
    // 0개 = fresh claude 가 자기 세션을 아직 안 씀(부팅 중) → None, 다음 사이클
    // 재시도(곧 쓰면 잡힘). 2+ = 같은 cwd 에 여러 claude(나·god·워커 공유) →
    // 어느 게 이 pane 인지 latest-mtime 으로는 모름(남의 세션 훔침) → None, hook
    // (정확 경로 보고)에 맡긴다. 이 모호성 가드가 없을 때 %2 가 남의 세션에 잘못
    // bind 돼 대화가 안 뜨던 버그(거노 실측).
    let mut recent = recent_jsonls(&cwd, std::time::Duration::from_secs(30 * 60));
    if recent.len() == 1 {
        return recent.pop();
    }
    None
}

/// bind-transcript 훅이 `~/.config/kasaterm/agent-roster/<slug(cwd)>.json` 에
/// 기록한 pane↔session 매핑에서 이 pane 의 transcript 경로를 읽는다(정확·hook
/// authoritative). `archived`(죽은 세션) 는 무시, 파일이 실제 존재할 때만 반환.
fn roster_transcript(pane_id: &str, cwd: &std::path::Path) -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let slug = cwd.to_string_lossy().replace(['/', '.'], "-");
    let roster = std::path::PathBuf::from(&home)
        .join(".config/kasaterm/agent-roster")
        .join(format!("{slug}.json"));
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&roster).ok()?).ok()?;
    let entry = v.get(pane_id)?;
    if entry.get("archived").and_then(|a| a.as_bool()).unwrap_or(false) {
        return None;
    }
    let session = entry.get("session_id").and_then(|s| s.as_str())?;
    project_jsonl(cwd, session).filter(|p| p.exists())
}

/// `cwd` 의 claude 프로젝트 디렉터리에서 `within` 안에 수정된 .jsonl 경로들.
fn recent_jsonls(cwd: &std::path::Path, within: std::time::Duration) -> Vec<std::path::PathBuf> {
    let Some(home) = std::env::var("HOME").ok() else { return Vec::new() };
    let encoded = cwd.to_string_lossy().replace(['/', '.'], "-");
    let dir = std::path::PathBuf::from(home).join(".claude/projects").join(encoded);
    let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
    entries
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("jsonl") {
                return None;
            }
            let fresh = e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|d| d < within);
            fresh.then_some(p)
        })
        .collect()
}

/// claude TUI 상태바 첫 칸의 모델 표시명. 예 "Opus 4.8 (1M context)" / "Sonnet 4.6".
/// transcript 의 model id("claude-opus-4-8")로는 1M context 변형을 구분 못 해(둘 다
/// 같은 id) — 상태바가 유일하게 "(1M context)" 까지 보여준다(거노 지적). 선두 글리프/
/// 공백 뒤 첫 영문자부터 첫 ┃ 까지.
fn parse_status_model(screen: &str) -> Option<String> {
    for line in screen.lines() {
        if !line.contains('┃') {
            continue;
        }
        let first = line.split('┃').next()?;
        let start = first.find(|c: char| c.is_ascii_alphabetic())?;
        let model = first[start..].trim();
        if !model.is_empty() && model.len() < 60 {
            return Some(model.to_string());
        }
    }
    None
}

/// claude TUI 상태바("… ┃ 5% ┃ …")에서 컨텍스트 사용량 % 파싱. ┃ 가 든 줄의
/// 첫 `(\d+)%` 를 집는다(상태바엔 컨텍스트 % 하나뿐). regex 의존 없이 수동 스캔.
fn parse_context_pct(screen: &str) -> Option<u8> {
    for line in screen.lines() {
        if !line.contains('┃') {
            continue;
        }
        let chars: Vec<char> = line.chars().collect();
        for (i, &c) in chars.iter().enumerate() {
            if c == '%' && i > 0 && chars[i - 1].is_ascii_digit() {
                let mut j = i;
                let mut num = String::new();
                while j > 0 && chars[j - 1].is_ascii_digit() {
                    j -= 1;
                    num.insert(0, chars[j]);
                }
                if let Ok(n) = num.parse::<u16>() {
                    return Some(n.min(100) as u8);
                }
            }
        }
    }
    None
}

/// `~/.claude/projects/<encoded-cwd>/<session>.jsonl` 경로 구성.
fn project_jsonl(cwd: &std::path::Path, session: &str) -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let encoded = cwd.to_string_lossy().replace(['/', '.'], "-");
    Some(
        std::path::PathBuf::from(home)
            .join(".claude/projects")
            .join(encoded)
            .join(format!("{session}.jsonl")),
    )
}

