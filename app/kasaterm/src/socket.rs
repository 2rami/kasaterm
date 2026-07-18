//! Backend impl that bridges agent-socket to this binary's TmuxSession.
//!
//! The single-pane PoC reports a fixed workspace + surface id ("local-0"
//! / "pane-0") because we only own one tmux pane in this binary. Once
//! kasaterm grows multi-pane support the surface ids
//! become real tmux `@N` strings and `list_surfaces` returns one entry
//! per actually-open pane.

use kasa_socket::backend::{
    Backend, PaneActivity, PaneBlock, PaneRect, RecentSession, SessionsInfo, SplitDirection,
    SubagentInfo, SurfaceInfo, TranscriptChunk, WorkspaceInfo,
};
use kasa_socket::sessions::{is_uuid, recent_sessions_for, session_jsonl_path};
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};
use kasa_bridge::{Layout, TmuxSession};

use crate::transcript::snapshot_from_tail;
use crate::{PaneStatus, UserEvent, Workspace};
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
    /// hook-free 발견 스로틀 — `discover_unbound` 의 ps/lsof 비용을 board 폴(1/s)
    /// 마다 다 치르지 않도록 2s 에 1회로 제한한 마지막 실행 시각.
    last_discover: Arc<Mutex<Option<std::time::Instant>>>,
    /// pane 셸 pid → (조회시각, 라이브 cwd). collab_board 가 학생 경로(cd 반영)를
    /// transcript 가 아닌 PTY pid_cwd 로 채우되, lsof 비용을 2s 캐시로 제한한다.
    cwd_cache: Arc<Mutex<HashMap<u32, (std::time::Instant, std::path::PathBuf)>>>,
    /// surface_id → statusLine 이 보고한 "현재 보는 경로"(report_cwd). claude 내부 cd 는
    /// lsof(cwd_cache)로 안 보여, statusline.py 가 매 렌더 직접 push 한다.
    reported_cwd: Arc<Mutex<HashMap<String, String>>>,
    /// surface_id → 마지막 유효 (context_tokens, context_limit). transcript usage 가 tail
    /// 윈도에 없어 0 으로 떨어질 때 직전 값을 유지해 컨텍스트량·인연%가 0 으로 깜빡이지
    /// 않게 한다(거노: statusline 잘려도 화면파싱 말고 정확 추적 — 정확 소스만 신뢰).
    last_ctx: Arc<Mutex<HashMap<String, (u64, u64)>>>,
    /// surface_id → {cwd, git badge}, filled by the GUI each frame (shared Arc).
    /// `window_layout` reads it to stamp cwd/branch/diff onto each `PaneRect` so
    /// the BA GUI can draw a Warp-style bar without this thread shelling out to
    /// lsof/git. Empty until the GUI's `publish_pane_status` runs.
    pane_status_pub: Arc<Mutex<HashMap<String, PaneStatus>>>,
    /// claude sessionId → parentSessionId(background/fork 세션만). `App.bg_agents`
    /// 와 공유 — board lazy 배정이 포크 세션에 부모 학생을 상속하는 데 쓴다.
    bg_agents: Arc<Mutex<HashMap<String, Option<String>>>>,
    /// surface_id → 마지막 지글(NudgePaneResize) 발동 시각 — stale statusline pane 을
    /// 10s 에 1회만 흔들어 재실행 강제가 리사이즈 폭주가 되지 않게 한다.
    nudged: Arc<Mutex<HashMap<String, std::time::Instant>>>,
}

/// `claude agents --json` 의 sessionId→status (2s static 캐시). board(PtyBackend.
/// agents_status)와 터미널 타이틀바(render)가 같은 데이터로 claude 실행/working 판정을
/// 일치시킨다(거노: gui 동기화). 전역이라 PtyBackend 인스턴스 없이 App 도 호출.
static AGENTS_CACHE: LazyLock<
    Mutex<Option<(std::time::Instant, HashMap<String, String>, HashMap<String, String>)>>,
> = LazyLock::new(|| Mutex::new(None));

fn agents_cached() -> (HashMap<String, String>, HashMap<String, String>) {
    const TTL: std::time::Duration = std::time::Duration::from_secs(2);
    let now = std::time::Instant::now();
    if let Some((at, status, names)) = AGENTS_CACHE.lock().unwrap().as_ref() {
        if now.duration_since(*at) < TTL {
            return (status.clone(), names.clone());
        }
    }
    let mut map: HashMap<String, String> = HashMap::new();
    // 세션 name → sessionId. agents 피커로 attach 한 pane 은 kasaterm 이 어느 세션인지
    // 알 길이 없어(피커는 이벤트도 argv 흔적도 없음), pane OSC 타이틀(=세션 name)로
    // 역추적한다(rebind_agents_panes). 같은 이름 둘이면 모호 — 매핑에서 뺀다.
    let mut names: HashMap<String, String> = HashMap::new();
    let mut dup_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    // "claude" 이름 호출 금지 — .app 실행 시 kasaterm PATH 는 시스템 기본
    // (/usr/bin:/bin:…)뿐이라 ~/.local/bin 의 claude 가 안 잡혀, 이 캐시가 조용히
    // 늘 빈 값이었다(status 폴백 항상 mtime 휴리스틱 + agents 뷰 이름 매칭 불발 —
    // 거노: 이번엔 유우카로 떠). GUI 폴러와 같은 claude_bin() 리졸버를 쓴다.
    if let Ok(out) = crate::proc::command(kasa_mcp::claude_bin())
        .args(["agents", "--json"])
        .output()
    {
        if out.status.success() {
            if let Ok(items) = serde_json::from_slice::<Vec<serde_json::Value>>(&out.stdout) {
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
                    let e = map.entry(sid.to_string()).or_insert_with(|| st.to_string());
                    if rank(st) > rank(e) {
                        *e = st.to_string();
                    }
                    if let Some(n) = it.get("name").and_then(|v| v.as_str()).map(str::trim) {
                        if !n.is_empty() {
                            match names.entry(n.to_string()) {
                                std::collections::hash_map::Entry::Occupied(o) => {
                                    if o.get() != sid {
                                        dup_names.insert(n.to_string());
                                    }
                                }
                                std::collections::hash_map::Entry::Vacant(v) => {
                                    v.insert(sid.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    for n in &dup_names {
        names.remove(n);
    }
    *AGENTS_CACHE.lock().unwrap() = Some((now, map.clone(), names.clone()));
    (map, names)
}

pub(crate) fn agents_status_cached() -> HashMap<String, String> {
    agents_cached().0
}

/// 세션 name → sessionId(모호 이름 제외, 2s 캐시 공유).
fn agents_name_sids_cached() -> HashMap<String, String> {
    agents_cached().1
}

impl PtyBackend {
    /// `attention` is shared with the GUI (`App.collab.attention`): the CLI
    /// hook path (`kasaterm-cli attention`) and the GUI's grid-scan prompt
    /// detection both write it, so the board's `waiting` flag reflects either.
    pub fn new(
        proxy: EventLoopProxy<UserEvent>,
        ws: Arc<Mutex<Workspace>>,
        attention: Arc<Mutex<HashMap<String, String>>>,
        pane_status_pub: Arc<Mutex<HashMap<String, PaneStatus>>>,
        bg_agents: Arc<Mutex<HashMap<String, Option<String>>>>,
    ) -> Self {
        Self {
            proxy,
            ws,
            bound: Arc::new(Mutex::new(HashMap::new())),
            attention,
            last_discover: Arc::new(Mutex::new(None)),
            cwd_cache: Arc::new(Mutex::new(HashMap::new())),
            reported_cwd: Arc::new(Mutex::new(HashMap::new())),
            last_ctx: Arc::new(Mutex::new(HashMap::new())),
            pane_status_pub,
            bg_agents,
            nudged: Arc::new(Mutex::new(HashMap::new())),
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
        // bound 경로가 사라졌으면(세션 종료·`--resume`/fork 로 jsonl stem 교체) 그
        // stale bind 는 死 경로를 가리켜 transcript 가 영영 안 뜬다 — 한 번 bound 된
        // pane 은 재discover 대상에서 빠지기 때문. 파일이 없으면 unbound 로 취급해
        // 폴백(cmdline·recent jsonl)이 살아있는 실제 대화를 다시 묶게 한다.
        let unbound: HashSet<String> = {
            let bound = self.bound.lock().unwrap();
            live.iter()
                .filter(|id| match bound.get(*id) {
                    None => true,
                    Some(p) => !p.exists(),
                })
                .cloned()
                .collect()
        };
        if unbound.is_empty() {
            return;
        }
        for (id, shell_pid) in self.query_pane_pids() {
            if !unbound.contains(&id) {
                continue;
            }
            if let Some(path) = discover_transcript(&id, shell_pid) {
                self.publish_transcript_cwd(&id, &path);
                self.bound.lock().unwrap().insert(id, path);
            }
        }
    }

    /// bind 된 transcript 의 tail 에서 그 세션의 cwd 를 뽑아 GUI 파일트리 오버라이드로
    /// 위임. bg-attach 뷰 pane 은 statusline report-cwd 가 pane 밖(bg 프로세스)에서
    /// 돌아 안 오므로, 이 경로가 "pane 이 보는 프로젝트"를 아는 유일한 소스다.
    fn publish_transcript_cwd(&self, surface_id: &str, path: &std::path::Path) {
        let (tail, _) = read_tail(path, 64 * 1024);
        let cwd = tail.lines().rev().find_map(|l| {
            serde_json::from_str::<serde_json::Value>(l)
                .ok()?
                .get("cwd")?
                .as_str()
                .filter(|s| !s.is_empty())
                .map(std::path::PathBuf::from)
        });
        if let Some(cwd) = cwd {
            let _ = self
                .proxy
                .send_event(UserEvent::SocketViewCwd(surface_id.to_string(), cwd));
        }
    }

    /// pump(스크린 diff)가 이미 파싱한 마커 sid8 로 한 pane 을 직접 재바인딩 — 그리드
    /// 재스캔(행 창·타이밍)에 기대지 않는 진입-즉시 경로. 뷰 pane 게이트는 동일.
    pub(crate) fn rebind_pane_marker(&self, pane: &str, sid8: &str) {
        let Some((_, shell_pid)) =
            self.query_pane_pids().into_iter().find(|(id, _)| id == pane)
        else {
            return;
        };
        if claude_view_subcommand(shell_pid).is_none() {
            return;
        }
        let Some(sid) = resolve_sid8(sid8) else { return };
        let Some(path) = transcript_path_for_session(&sid) else { return };
        let cur = self.bound.lock().unwrap().get(pane).cloned();
        if cur.as_ref() != Some(&path) {
            let _ = self.bind_transcript(pane, &path.to_string_lossy());
        }
    }

    /// agents/attach 뷰 pane 의 pane↔세션 재바인딩 — 매 board 빌드마다 돈다(피커에서
    /// 다른 세션으로 갈아타면 bound 가 낡으므로 unbound 게이트를 못 탄다). 대상 세션은
    /// attach 는 argv 위치 인자, agents 피커는 pane OSC 타이틀(=세션 name)↔`claude
    /// agents --json` name 의 유일 매칭으로 알아낸다 — kasaterm 은 피커 선택을 이벤트로
    /// 못 받아 이 역추적이 유일한 파싱 경로다(거노: 백그라운드는 터미널이 파싱만).
    /// 바인딩은 bind_transcript 로 — bound(board)+SocketSessionBound(render 캐릭터)가
    /// 한 호출로 정렬된다. 매칭 실패(피커 화면·중복 이름)면 건드리지 않는다.
    pub(crate) fn rebind_agents_panes(&self, live: &HashSet<String>) {
        let mut name_sids: Option<HashMap<String, String>> = None;
        for (id, shell_pid) in self.query_pane_pids() {
            if !live.contains(&id) {
                continue;
            }
            let Some(sub) = claude_view_subcommand(shell_pid) else { continue };
            let sid = match sub {
                "attach" => attach_target_from_cmdline(shell_pid),
                _ => {
                    // 1순위: 화면의 statusline 세션 id 마커(진입 즉시·정확). 8행 —
                    // statusline 아래 입력힌트·여백 행이 붙어 3행 창은 마커를 놓친다.
                    // 2순위: OSC 타이틀↔세션 name 매칭(구 statusline·마커 잘림 폴백).
                    let (screen, title) = {
                        let ws = self.ws.lock().unwrap();
                        match ws.panes.get(&id) {
                            Some(p) => (p.visible_text(8), p.title.clone()),
                            None => continue,
                        }
                    };
                    let resolved = screen_marker_sid8(&screen)
                        .and_then(|s8| resolve_sid8(&s8))
                        .or_else(|| {
                            title.and_then(|t| {
                                let t = title_session_name(&t);
                                if t.is_empty() {
                                    return None;
                                }
                                name_sids
                                    .get_or_insert_with(agents_name_sids_cached)
                                    .get(t)
                                    .cloned()
                            })
                        });
                    // stale statusline: 우리 statusline(프사 슬롯 U+FFFC)은 떠 있는데
                    // 마커도 타이틀 매칭도 없다 — 구버전 claude(≤2.1.209 실측)는 attach
                    // 에서 statusline 을 재실행하지 않아, 사용자가 뭔가 치기 전까지
                    // 마커가 영영 안 흐른다(거노). 1행 지글로 재실행을 강제(10s
                    // rate-limit). 피커/셸 화면은 FFFC 가 없어 안 탄다.
                    if resolved.is_none()
                        && screen.contains('\u{fffc}')
                        && !screen.contains('⟦')
                    {
                        let mut nudged = self.nudged.lock().unwrap();
                        let due = nudged
                            .get(&id)
                            .is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(10));
                        if due {
                            nudged.insert(id.clone(), std::time::Instant::now());
                            let _ = self
                                .proxy
                                .send_event(UserEvent::NudgePaneResize(id.clone()));
                        }
                    }
                    resolved
                }
            };
            let Some(sid) = sid else { continue };
            let Some(path) = transcript_path_for_session(&sid) else { continue };
            let cur = self.bound.lock().unwrap().get(&id).cloned();
            if cur.as_ref() != Some(&path) {
                let _ = self.bind_transcript(&id, &path.to_string_lossy());
            }
        }
    }

    /// sessionId → official claude status (idle/busy/waiting), cached 2s.
    /// `claude agents --json` is authoritative; the transcript-mtime heuristic
    /// in `read_tail`/`snapshot_from_tail` is only a fallback for sessions
    /// claude doesn't report. One sessionId can span several processes (shells
    /// inherit the parent's session id), so we collapse to the most-active
    /// state (busy > waiting > idle).
    pub(crate) fn agents_status(&self) -> HashMap<String, String> {
        agents_status_cached()
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

    /// `POST /session-new?character=<name>` — 새 방(윈도우) + 캐릭터 지정 스폰을 GUI 에 위임.
    fn new_room(&self, character: &str) -> Result<()> {
        self.proxy
            .send_event(UserEvent::SocketNewRoom(character.to_string()))
            .map_err(|_| anyhow::anyhow!("gui event loop gone"))?;
        Ok(())
    }

    /// `POST /spawn-student?character=<name>` — 현재 방에 캐릭터 지정 학생 추가.
    fn spawn_student(&self, character: &str) -> Result<()> {
        self.proxy
            .send_event(UserEvent::SocketSpawnStudent(character.to_string()))
            .map_err(|_| anyhow::anyhow!("gui event loop gone"))?;
        Ok(())
    }

    /// `POST /swap-character?surface=<id>&character=<name>` — pane 캐릭터 교체(respawn).
    fn swap_character(&self, surface_id: &str, character: &str) -> Result<()> {
        self.proxy
            .send_event(UserEvent::SocketSwapCharacter(
                surface_id.to_string(),
                character.to_string(),
            ))
            .map_err(|_| anyhow::anyhow!("gui event loop gone"))?;
        Ok(())
    }

    /// `GET /repersona?surface=<id>&character=<name>` — respawn 없는 캐릭터 재배정.
    /// 학생 명령(`시로코`)이 claude 실행 직전에 호출한다.
    fn repersona(&self, surface_id: &str, character: &str) -> Result<()> {
        self.proxy
            .send_event(UserEvent::SocketRepersona(
                surface_id.to_string(),
                character.to_string(),
            ))
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

    /// 활성 pane 의 포그라운드 프로세스 이름("zsh"·"node"(=claude)·"vim"…). room_cd 가
    /// **셸일 때만** raw `cd` 를 보내고 claude 등엔 안 보내도록(거노: BA GUI 가 돌아가는
    /// claude 입력칸에 cd 를 박지 않게) 판단 근거로 쓴다.
    fn active_process_name(&self) -> Option<String> {
        let active = self.ws.lock().unwrap().active_pane.clone()?;
        let pid = self
            .query_pane_pids()
            .into_iter()
            .find(|(id, _)| *id == active)
            .map(|(_, p)| p)?;
        foreground_proc_name(pid)
    }

    /// pane → claude session_id(`/pane-tasks` 용) = bound transcript 파일명 stem.
    /// normal claude 는 transcript==session 이라 task store dir(`session-<id 첫8hex>`)
    /// 매핑에 폴백으로 쓴다.
    fn pane_session_ids(&self) -> Result<Vec<(String, String)>> {
        let live: std::collections::HashSet<String> =
            self.ws.lock().unwrap().panes.keys().cloned().collect();
        self.discover_unbound(&live);
        let bound = self.bound.lock().unwrap();
        let mut out: Vec<(String, String)> = Vec::new();
        for pane in &live {
            if let Some(stem) = bound
                .get(pane)
                .and_then(|p| p.file_stem())
                .and_then(|s| s.to_str())
            {
                out.push((pane.clone(), stem.to_string()));
            }
        }
        Ok(out)
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
        // Snapshot the GUI-published cwd/git map once so each pane below is a
        // cheap lookup — no lsof/git on this (per-second polled) path.
        let status = self.pane_status_pub.lock().unwrap().clone();
        let stamp = |mut rect: PaneRect| -> PaneRect {
            if let Some(s) = status.get(&rect.surface_id) {
                rect.cwd = Some(s.cwd.to_string_lossy().into_owned());
                if let Some(b) = &s.badge {
                    rect.branch = Some(b.branch.clone());
                    rect.files = Some(b.files);
                    rect.insertions = Some(b.insertions);
                    rect.deletions = Some(b.deletions);
                }
            }
            rect
        };
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
                    Some(stamp(PaneRect {
                        surface_id: format!("%{id}"),
                        x: pct(*x, tw),
                        y: pct(*y, th),
                        w: pct(*w, tw),
                        h: pct(*h, th),
                        ..Default::default()
                    }))
                })
                .collect());
        }
        // Single pane: one full-window box.
        Ok(ws
            .active_pane
            .clone()
            .or_else(|| ws.panes.keys().next().cloned())
            .map(|surface_id| {
                vec![stamp(PaneRect {
                    surface_id,
                    x: 0,
                    y: 0,
                    w: 100,
                    h: 100,
                    ..Default::default()
                })]
            })
            .unwrap_or_default())
    }

    fn focus_surface(&self, surface_id: &str) -> Result<()> {
        let _ = self
            .proxy
            .send_event(UserEvent::SocketFocus(surface_id.to_string()));
        Ok(())
    }

    fn paste_image(&self, surface: &str, bytes: Vec<u8>) -> Result<()> {
        self.proxy
            .send_event(UserEvent::SocketPasteImage(surface.to_string(), bytes))
            .map_err(|_| anyhow::anyhow!("gui event loop gone"))?;
        Ok(())
    }

    fn toggle_git_panel(&self) -> Result<()> {
        self.proxy
            .send_event(UserEvent::SocketToggleGit)
            .map_err(|_| anyhow::anyhow!("gui event loop gone"))?;
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
    /// 오판했다(거노 실측: 방 토글 차단). GUI 동기 RPC 로 활성 pane 의
    /// shell pid 만 받고(메모리 즉답), lsof 해석은 이 backend 스레드서 한다 —
    /// 라이브 lsof 가 정확(split 시점 박제 캐시·프로세스 cwd 불신).
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

    fn recent_sessions(&self, cwd: Option<&str>) -> Result<Vec<RecentSession>> {
        let base = cwd
            .map(std::path::PathBuf::from)
            .or_else(|| self.active_cwd())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));
        Ok(recent_sessions_for(&base, 20))
    }

    fn resume_session(&self, id: &str, cwd: Option<&str>, newroom: bool, attach: bool) -> Result<()> {
        self.proxy
            .send_event(UserEvent::ResumeSession {
                id: id.to_string(),
                cwd: cwd.map(str::to_string),
                newroom,
                attach,
            })
            .map_err(|_| anyhow::anyhow!("gui event loop gone"))?;
        Ok(())
    }

    fn save_session(&self, surface: Option<&str>) -> Result<()> {
        self.proxy
            .send_event(UserEvent::SaveSession {
                surface: surface.map(str::to_string),
            })
            .map_err(|_| anyhow::anyhow!("gui event loop gone"))?;
        Ok(())
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

    fn report_cwd(&self, surface_id: &str, cwd: &str, _session_id: &str) -> Result<()> {
        self.reported_cwd
            .lock()
            .unwrap()
            .insert(surface_id.to_string(), cwd.to_string());
        // GUI 파일트리가 "pane 이 보는 경로"를 셸 cwd 보다 우선하도록 위임.
        let _ = self.proxy.send_event(UserEvent::SocketViewCwd(
            surface_id.to_string(),
            std::path::PathBuf::from(cwd),
        ));
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
        // 대상 surface 가 지정됐는데 현재 없는 pane 이면 거부 — 재시작·종료로 사라진 학생에게
        // tell 이 검증 없이 ok 만 받고 조용히 사라지던 오발송을 막는다(거노). 보낸 쪽이 ok:false
        // 로 즉시 알아 떠맡기/--resume 을 결정한다. None(focused)은 항상 통과.
        if let Some(sid) = surface_id {
            if !self.ws.lock().unwrap().panes.contains_key(sid) {
                anyhow::bail!("surface {sid} 없음 — 재시작·종료로 사라진 pane (오발송 방지)");
            }
        }
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

    fn open_preview(&self, _kind: &str, path: &str, target: Option<&str>) -> Result<()> {
        // imgopen/mdopen 셰임·SendUserFile 훅 → 미리보기를 요청 pane 의 보조 탭으로
        // (크롬 탭처럼). `target` = 요청자의 $KASATERM_PANE_ID(=pid) — GUI 가
        // outer_for_pty 로 그 pane 을 찾아 거기 탭으로 붙인다. 별도 split 으로 띄우면
        // arona 멀티뷰가 터미널 pane 만 미러해 빈 pane 으로 보였던 문제 해소. 로컬 PTY
        // 모드는 App.pty 를 별도 스레드서 못 만져 GUI 에 위임(open_file 이 확장자 분기·
        // 디코드·탭 push 까지). 데몬 제거로 빠졌던 것.
        let _ = self.proxy.send_event(UserEvent::SocketOpenPreview(
            path.to_string(),
            target.map(|s| s.to_string()),
        ));
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
        self.publish_transcript_cwd(surface_id, std::path::Path::new(path));
        // transcript 파일명(stem) = claude 세션 id — GUI 에 위임해 세션→캐릭터 영속
        // 매핑을 조회/저장한다(거노 ④: resume 시 캐릭터 재사용). App 상태는 GUI 스레드
        // 소유라 proxy 로 넘긴다(SocketBytes 관례).
        if let Some(sid) = std::path::Path::new(path).file_stem().and_then(|s| s.to_str()) {
            let _ = self.proxy.send_event(UserEvent::SocketSessionBound(
                surface_id.to_string(),
                sid.to_string(),
            ));
        }
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

    fn pane_blocks(&self, surface_id: &str, limit: usize) -> Result<Vec<PaneBlock>> {
        // The GUI shares each PTY's block store through `pane_status_pub`
        // (a cheap Arc), so we read it here without touching App.pty.
        let store = {
            let g = self.pane_status_pub.lock().unwrap();
            g.get(surface_id).and_then(|s| s.blocks.clone())
        };
        let store = store
            .ok_or_else(|| anyhow::anyhow!("no command blocks for pane: {surface_id}"))?;
        let blocks = store.lock().unwrap();
        let start = blocks.len().saturating_sub(limit);
        Ok(blocks
            .iter()
            .skip(start)
            .map(|b| PaneBlock {
                id: b.id,
                command: b.command.clone(),
                output: b.output.clone(),
                exit_code: b.exit_code,
                started_ms: b.started_ms,
                duration_ms: b.duration_ms,
                is_tui: b.is_tui,
            })
            .collect())
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

    fn transcript_raw(&self, surface_id: &str, offset: u64) -> Result<TranscriptChunk> {
        // Same bound→jsonl mapping as transcript_tail, but hand back raw jsonl
        // incrementally (tail on first load, appended lines after) so the BA GUI
        // doesn't re-read & re-parse the whole multi-MB file every 1.5s poll.
        let path = self
            .bound
            .lock()
            .unwrap()
            .get(surface_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("pane {surface_id} has no bound transcript"))?;
        read_incremental(&path, offset)
            .map_err(|e| anyhow::anyhow!("read transcript {path:?}: {e}"))
    }

    fn session_transcript_raw(&self, id: &str, cwd: Option<&str>) -> Result<String> {
        // Offline read by uuid — no bound surface. Resolve the jsonl path the
        // same way recent_sessions_for discovers candidates, so the BA GUI can
        // preview a past session before deciding to resume it.
        if !is_uuid(id) {
            anyhow::bail!("invalid session id: {id}");
        }
        let base = cwd
            .map(std::path::PathBuf::from)
            .or_else(|| self.active_cwd())
            .ok_or_else(|| anyhow::anyhow!("no cwd for session {id}"))?;
        let path = session_jsonl_path(&base, id)
            .ok_or_else(|| anyhow::anyhow!("no HOME — cannot locate session {id}"))?;
        std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("read session transcript {path:?}: {e}"))
    }

    fn subagents(&self, surface_id: &str) -> Result<Vec<SubagentInfo>> {
        // Claude Code writes subagent dialogues next to the main transcript:
        // <session-dir>/subagents/agent-<id>.jsonl (+ .meta.json). The session
        // dir is the bound jsonl path with its `.jsonl` extension stripped.
        let path = self
            .bound
            .lock()
            .unwrap()
            .get(surface_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("pane {surface_id} has no bound transcript"))?;
        let dir = path.with_extension("").join("subagents");
        let Ok(entries) = std::fs::read_dir(&dir) else { return Ok(Vec::new()) };
        let mut out: Vec<SubagentInfo> = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            // Pivot on the transcript file so we only list agents we can open.
            let Some(id) = name.strip_prefix("agent-").and_then(|s| s.strip_suffix(".jsonl")) else {
                continue;
            };
            let mtime = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let (agent_type, description) = std::fs::read_to_string(dir.join(format!("agent-{id}.meta.json")))
                .ok()
                .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                .map(|v| {
                    let at = v.get("agentType").and_then(|s| s.as_str()).unwrap_or("").to_string();
                    let de = v.get("description").and_then(|s| s.as_str()).unwrap_or("").to_string();
                    (at, de)
                })
                .unwrap_or_default();
            out.push(SubagentInfo { agent_id: id.to_string(), agent_type, description, mtime });
        }
        out.sort_by(|a, b| b.mtime.cmp(&a.mtime));
        Ok(out)
    }

    fn subagent_transcript_raw(&self, surface_id: &str, agent_id: &str) -> Result<String> {
        // agent_id is interpolated into a path — allow only the hex-ish ids Claude
        // emits so a crafted `surface`/`agentId` can't traverse out of subagents/.
        if agent_id.is_empty() || !agent_id.chars().all(|c| c.is_ascii_alphanumeric()) {
            anyhow::bail!("invalid agent id: {agent_id}");
        }
        let path = self
            .bound
            .lock()
            .unwrap()
            .get(surface_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("pane {surface_id} has no bound transcript"))?;
        let file = path
            .with_extension("")
            .join("subagents")
            .join(format!("agent-{agent_id}.jsonl"));
        std::fs::read_to_string(&file)
            .map_err(|e| anyhow::anyhow!("read subagent transcript {file:?}: {e}"))
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
        // agents/attach 뷰 pane 은 discovery 대신 여기서 세션을 역추적해 (재)바인딩 —
        // 피커에서 세션을 갈아타면 bound 가 낡아 unbound 게이트로는 못 잡는다.
        self.rebind_agents_panes(&live);
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
        // 방별 분리(거노): 각 pane 의 character 는 *그 pane 의 방(room)* collab dir
        // 에서 읽는다 — 같은 cwd 라도 방마다 캐릭터가 다르다. pane_room
        // 없으면(기본 방) 기존 cwd-slug. ws(공유)에서 복제해 아래 map 클로저서 쓴다.
        // active_window_panes: 보이는 방(윈도우)의 pane — board 를 활성 방으로 한정
        // (거노: 아로나 방+프라나 방이 한 교실에 같이 뜸). 비었으면(초기) 필터 안 함.
        // 전 윈도우(방) pane → window_idx — board 를 활성 방으로 한정하지 않고 모든 방의 학생을
        // 실어 arona-ui 좌측이 방별 학생 트리를 영속한다(거노: 좌측 통합·전 방 영속). 이 맵은
        // GUI(App)의 publish_pty_layout 이 ws 로 미러한다 — PtyBackend 는 App.windows 를 못 본다.
        let (pane_room, pane_character, pane_window) = {
            let ws = self.ws.lock().unwrap();
            (ws.pane_room.clone(), ws.pane_character.clone(), ws.pane_window.clone())
        };
        // pane 셸 프로세스 env 의 KASATERM_CHARACTER — 데몬이 영속하는 세션 정체성.
        // bg/포크 세션은 re-attach 마다 claude 가 transcript id 를 새로 발급해 세션id 키
        // persistence 가 어긋나고 board 가 랜덤 둔갑한다. 게다가 ws.pane_character 는
        // 세션 저장파일로 복원돼 오염된 랜덤값이 marker 를 덮는다(거노: 데몬 영속 학생이
        // board 와 따로 논다). env 는 스폰 때 박혀 fork/재접속/재시작 너머 안 바뀌므로
        // 최우선으로 읽어 복원된 ws·marker·랜덤보다 먼저 정체성을 고정한다. 로스터 밖
        // 값은 무시(오염 방지). ps 는 폴당 pane 수만큼(1/s)이라 부담 없음.
        let pane_shell_pid: HashMap<String, u32> = self.query_pane_pids().into_iter().collect();
        let valid_members: HashSet<String> = kasa_mcp::character::characters_json()
            .map(|c| kasa_mcp::character::member_names(&c).into_iter().collect())
            .unwrap_or_default();
        // 세션 → 포크 부모 세션 id(argv --resume). detach 포크로 세션 id 가 갈려도 이 사슬
        // 끝의 원본 바인딩(session_characters.json)이 retained 학생이다(거노: bg 재진입 둔갑).
        let daemon_parents = daemon_session_parents();
        // board 빌드 한 폴링 안에서 lazy 배정된 캐릭터 — 같은 폴링에 처음 등장한 두 pane 이
        // 둘 다 같은 빈 슬롯(예: 미도리)을 고르는 걸 막는다(pane_character 클론은 빌드 중
        // 안 바뀌므로 별도 누적). 다음 폴링부턴 ws.pane_character 로 잡혀 불필요.
        let mut lazy_assigned: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut board: Vec<PaneActivity> = bound
            .iter()
            // 전 방(윈도우) 학생 — 활성 방 한정 폐기(거노: 전 방 영속). live = 모든 윈도우 pane.
            .filter(|(sid, _)| live.contains(sid.as_str()))
            .map(|(sid, path)| {
                // 512KB: 64KB 윈도는 background/subagent 런치(run_in_background·Monitor·Task)가
                // 그 뒤 대량 출력에 밀려나 윈도 밖이면 못 잡았다(거노: 유즈 background 빔 —
                // 최근 런치가 파일 끝에서 ~269KB 지점). 작은 transcript 는 전체라 부담 없음.
                let (tail, mtime_idle) = read_tail(path, 512 * 1024);
                let mut row = snapshot_from_tail(sid, &tail, mtime_idle);
                row.window_idx = pane_window.get(sid.as_str()).copied().unwrap_or(0);
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
                        // official 이 idle 이어도 화면에 생성 스피너("esc to interrupt")가
                        // 떠 있으면 working — agents --json 은 2s 캐시+데몬 보고라 TUI 보다
                        // 늦어, Generating 중인데 board 가 idle 로 새던 실측(프라나) 교정.
                        if generating.contains(sid.as_str()) {
                            row.status = "working".into();
                            false
                        } else {
                            row.status = "idle".into();
                            true
                        }
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
                // 이 pane 의 방 collab dir = cwd-slug(+ 방이면 __room_<id>). character
                // 마커를 여기서 읽어 방별로 분리(거노: 프라나 방에 시로코 뜨던 버그).
                let base_slug = path
                    .parent()
                    .and_then(|d| d.file_name())
                    .and_then(|s| s.to_str())
                    .unwrap_or("");
                let rslug = match pane_room.get(sid.as_str()) {
                    Some(r) => format!("{base_slug}__room_{r}"),
                    None => base_slug.to_string(),
                };
                // GUI 가 spawn/swap 시 배정한 ws.pane_character 우선 — 터미널 헤더
                // (pane_header_label)와 같은 소스라 board·탭 캐릭터가 항상 일치(거노:
                // board 미도리 둘 / 헤더 모모이 불일치). 없으면 방 dir 의 character-<N> 마커.
                // 세션 자신의 바인딩 → 없으면 포크 부모 사슬을 따라 원본 학생을 찾는다.
                // detach 포크로 세션 id 가 갈려도 --resume 부모 끝의 바인딩이 retained 진실
                // (per-세션이라 "다 같은 학생" 아님, 거노). stem = transcript 파일명 = 세션 id.
                let stem = path.file_stem().and_then(|s| s.to_str());
                let retained = stem
                    .and_then(|s| {
                        let mut cur = s.to_string();
                        for _ in 0..8 {
                            if let Some(c) = kasa_mcp::character::session_character(&cur) {
                                return Some(c);
                            }
                            match daemon_parents.get(&cur) {
                                Some(p) => cur = p.clone(),
                                None => break,
                            }
                        }
                        None
                    })
                    .filter(|c| valid_members.contains(c));
                // 셸 env 폴백(foreground 순정 경로) — bg 셸엔 대개 없다.
                let env_char = pane_shell_pid
                    .get(sid.as_str())
                    .and_then(|&pid| kasa_pty::process_env_var(pid, "KASATERM_CHARACTER"))
                    .filter(|c| valid_members.contains(c));
                row.character = retained
                    .clone()
                    .or(env_char)
                    .or_else(|| pane_character.get(sid.as_str()).cloned())
                    .or_else(|| {
                        std::fs::read_to_string(format!(
                            "/tmp/kasaterm-collab/{rslug}/character-{}",
                            sid.trim_start_matches('%')
                        ))
                        .ok()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                    });
                // retained 진실이 ws·marker 와 어긋나면 교정 — render(statusline·테두리·타이틀)는
                // ws.pane_character 를 보므로 복원된 오염 랜덤을 원본으로 되돌린다.
                if let Some(rc) = retained {
                    if pane_character.get(sid.as_str()) != Some(&rc) {
                        let _ = kasa_mcp::character::write_marker(&rslug, sid.as_str(), &rc);
                        self.ws.lock().unwrap().pane_character.insert(sid.clone(), rc);
                    }
                }
                // 마커 없는 pane(claude --resume 복원·spawn 의 assign_character_env 를 못 탄
                // 경로)도 board 빌드 때 빈 슬롯 캐릭터를 lazy 배정한다 — 안 하면 board
                // char=None → 프사/이름이 안 떴다(거노: %1 프사 None).
                // write_marker(atomic) 후 다음 폴링부턴 위 read 로 잡혀 1회만 배정된다.
                if row.character.is_none() {
                    // 포크(background/--resume) 세션은 부모 대화의 학생을 상속한다 —
                    // 랜덤 둔갑 방지(거노: 백그라운드에서 학생이 또 바뀜). claude sid =
                    // transcript stem → bg_agents(claude sessionId→parentSessionId) →
                    // 부모 학생. 부모가 없으면(순수 새 세션) 기존 빈 슬롯 랜덤.
                    let stem = path.file_stem().and_then(|s| s.to_str());
                    let inherited = stem
                        .and_then(|stem| {
                            self.bg_agents
                                .lock()
                                .ok()
                                .and_then(|m| m.get(stem).cloned())
                                .flatten()
                        })
                        .and_then(|parent| kasa_mcp::character::session_character(&parent));
                    // 세션 자신이 이미 배정받은 적 있으면(재시작·resume 이 같은 transcript id 로
                    // 복귀) 그 학생을 재사용 — board 첫 폴링부터 랜덤 둔갑 차단(거노). 부모
                    // 상속 다음, 빈 슬롯 랜덤 앞.
                    let own = stem.and_then(kasa_mcp::character::session_character);
                    let name = inherited.or(own).or_else(|| {
                        kasa_mcp::character::characters_json().and_then(|chars| {
                            let members = kasa_mcp::character::member_names(&chars);
                            // 살아있는 다른 pane 이 쓰는 캐릭터(이번 폴링 누적 스냅샷)는 피한다 —
                            // 죽은 pane 마커는 무시. 빈 슬롯 없으면 첫째로 순환(거노: 모모이 둘).
                            let taken: std::collections::HashSet<&String> =
                                pane_character.values().chain(lazy_assigned.iter()).collect();
                            members
                                .iter()
                                .find(|m| !taken.contains(m))
                                .cloned()
                                .or_else(|| members.first().cloned())
                        })
                    });
                    {
                        if let Some(name) = name {
                            let _ = kasa_mcp::character::write_marker(&rslug, sid.as_str(), &name);
                            // claude sid(transcript stem)에도 영속 — 재진입·재시작이 같은
                            // transcript id 로 돌아오면 own(session_character(stem))으로 잡혀
                            // 랜덤 재배정(둔갑) 없이 같은 학생을 유지한다(거노: 어느새 미도리로
                            // 바뀜). write_marker/pane_character 만으론 session_characters.json 에
                            // 안 남아 다음 폴링·재진입의 stem 조회가 계속 None → 매번 재랜덤이었다.
                            if let Some(stem) = stem {
                                let _ = kasa_mcp::character::bind_session_character(stem, &name);
                            }
                            // 단일 진실 ws.pane_character 에도 기록 — 다음 폴링·session 배정이
                            // 이 캐릭터를 중복하지 않게. 같은 폴링 내 다른 lazy 가 또 같은 캐릭터를
                            // 안 고르게 lazy_assigned 에도 누적(클론 스냅샷은 빌드 중 안 바뀜).
                            self.ws.lock().unwrap().pane_character.insert(sid.clone(), name.clone());
                            lazy_assigned.insert(name.clone());
                            row.character = Some(name);
                        }
                    }
                }
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
        // 화면 스냅샷 + OSC title 을 한 락에서. title 은 board row 라벨을 터미널 탭
        // (pane_header_label)과 같은 소스(OSC title)로 통일 — 양쪽 "미도리 · 작업명".
        let (screens, osc_titles): (HashMap<String, String>, HashMap<String, String>) = {
            let ws = self.ws.lock().unwrap();
            let mut screens = HashMap::new();
            let mut osc_titles = HashMap::new();
            for r in &board {
                if let Some(p) = ws.panes.get(&r.surface_id) {
                    screens.insert(r.surface_id.clone(), p.visible_text(8));
                    if let Some(t) = p.title.clone().filter(|t| !t.is_empty()) {
                        osc_titles.insert(r.surface_id.clone(), t);
                    }
                }
            }
            (screens, osc_titles)
        };
        // claude saved default effort(settings.json) — resume 직후 effort 카드 폴백(거노). 작은 파일
        // 1회 읽어 모든 행에 동일 적용(글로벌 설정이라 pane 무관).
        let saved_effort = claude_saved_effort();
        for row in &mut board {
            row.title = osc_titles.get(&row.surface_id).cloned().unwrap_or_default();
            row.effort_default = saved_effort.clone();
            if let Some(&pid) = pane_pids.get(&row.surface_id) {
                if let Some(cwd) = self.pane_cwd_live(pid) {
                    row.cwd = cwd.to_string_lossy().into_owned();
                }
            }
            // statusLine 이 보고한 "현재 보는 경로"(claude 내부 cd). 없으면 빈값(=cwd 만 표시).
            if let Some(vc) = self.reported_cwd.lock().unwrap().get(&row.surface_id) {
                row.view_cwd = vc.clone();
            }
            if let Some(screen) = screens.get(&row.surface_id) {
                // 모델명만 상태바에서 — "Opus 4.8 (1M context)" 처럼 1M 변형까지 정확.
                // 컨텍스트 %는 상태바를 안 쓴다: 터미널이 좁아 statusline 이 잘리면 % 가 화면 밖이라
                // 0 으로 떨어진다(거노: 화면파싱 말고 정확 추적). transcript usage 만 정확 소스.
                if let Some(m) = parse_status_model(screen) {
                    row.model = m;
                }
            }
            // 1M 보정 — 상태바 모델이 "1M context" 면 한도를 1M 로 확정. transcript 모델엔 [1m]
            // 태그가 안 실려 토큰<200k 인 1M 세션이 200k 한도로 잘못 잡히던 걸 교정.
            if row.model.to_ascii_lowercase().contains("1m") && row.context_limit < 1_000_000 {
                row.context_limit = 1_000_000;
            }
            // 정확 소스(transcript usage)가 tail 윈도에 없어 0 이면 직전 유효값을 유지 — 컨텍스트량·
            // 인연%가 0 으로 깜빡이지 않게(거노: statusline 잘려도 0 안 됨). 0 이상이면 캐시 갱신.
            {
                let mut cache = self.last_ctx.lock().unwrap();
                if row.context_tokens > 0 {
                    cache.insert(row.surface_id.clone(), (row.context_tokens, row.context_limit));
                } else if let Some(&(t, l)) = cache.get(&row.surface_id) {
                    row.context_tokens = t;
                    if row.context_limit == 0 {
                        row.context_limit = l;
                    }
                }
            }
            if row.context_tokens > 0 && row.context_limit > 0 {
                row.context_pct = (((row.context_tokens as f64 / row.context_limit as f64) * 100.0).round() as u64).min(100) as u8;
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
                    crate::proc::command("git")
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

/// 채팅뷰 증분 읽기 — `offset` 이후 append 된 **완전한 줄**만 돌려준다. offset==0
/// (첫 로드)이거나 파일이 줄었으면(세션 교체) 마지막 `TRANSCRIPT_TAIL` 바이트 윈도를
/// `reset` 으로 준다(첫 불완전 줄은 버림). 끝의 쓰다 만 줄은 다음 호출로 미뤄, 반환
/// `offset` 은 항상 마지막 개행 직후 — 멀티바이트/JSON 라인 경계가 안 깨진다. 안 바뀌면
/// raw="" 라 프론트 재파싱·리렌더가 0.
const TRANSCRIPT_TAIL: u64 = 512 * 1024;

fn read_incremental(path: &std::path::Path, offset: u64) -> std::io::Result<TranscriptChunk> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    // offset==0 = 첫 로드, offset>len = 파일이 줄어듦(세션 교체) → 둘 다 tail 재로드.
    let reset = offset == 0 || offset > len;
    let start = if reset { len.saturating_sub(TRANSCRIPT_TAIL) } else { offset };
    if start >= len {
        // 변화 없음(또는 빈 파일) — 재파싱 0.
        return Ok(TranscriptChunk { raw: String::new(), offset: len, reset: false });
    }
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::with_capacity((len - start) as usize);
    f.take(len - start).read_to_end(&mut buf)?;
    // 끝의 쓰다 만 줄(마지막 \n 이후)은 잘라 다음 호출로. next offset = 마지막 \n+1.
    let end = buf.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    let next_offset = start + end as u64;
    let mut slice = &buf[..end];
    // tail(reset)일 땐 중간부터 시작해 깨진 앞 첫 줄도 버린다.
    if reset {
        if let Some(i) = slice.iter().position(|&b| b == b'\n') {
            slice = &slice[i + 1..];
        }
    }
    let mut raw = String::from_utf8_lossy(slice).into_owned();
    // tail 윈도 밖으로 밀린 미처리 예약(queue-operation)도 채팅에 살린다 — 작업 turn 이
    // 512KB 넘게 쌓이면 오래된 enqueue 가 윈도 밖이라 큐 버블이 안 뜨던 것(거노). 큐 op
    // 라인은 작아(텍스트) 전부 prepend 해도 가볍고, 프론트가 enqueue/dequeue/remove 를
    // FIFO 매칭해 미처리만 큐 버블로 그린다(처리된 예약은 droppedQ 로 제거).
    if reset && start > 0 {
        let head = scan_queue_ops_before(path, start);
        if !head.is_empty() {
            raw = format!("{head}\n{raw}");
        }
    }
    Ok(TranscriptChunk { raw, offset: next_offset, reset })
}

/// `[0, start)` 구간에서 queue-operation(예약 enqueue/dequeue/remove/popAll) 줄만 모은다.
/// reset(tail) 로드 때 윈도 밖으로 밀린 미처리 예약 큐 버블을 복원하려는 것. enqueue 만이
/// 아니라 처리 op 까지 다 모아야 프론트 FIFO 매칭에서 이미 처리된 예약이 영구 잔존하지 않는다.
fn scan_queue_ops_before(path: &std::path::Path, start: u64) -> String {
    use std::io::{BufRead, BufReader};
    let Ok(f) = std::fs::File::open(path) else {
        return String::new();
    };
    let mut reader = BufReader::new(f);
    let mut out = String::new();
    let mut pos: u64 = 0;
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(n) => {
                pos += n as u64;
                if pos > start {
                    break; // tail 윈도 진입 — 이후 줄은 tail 이 담당
                }
                if line.contains("\"type\":\"queue-operation\"") {
                    out.push_str(line.trim_end_matches('\n'));
                    out.push('\n');
                }
            }
            Err(_) => break,
        }
    }
    out.truncate(out.trim_end().len());
    out
}

/// Foreground process name under a pane's shell pid — the youngest direct child
/// (a running `claude`/`vim`/build), else the shell itself at a bare prompt. One
/// `ps` scan (Windows has no `ps` → None, degrades to "not a shell"). Lets
/// `room_cd` send raw `cd` only at a shell, never into a live claude (거노).
pub(crate) fn foreground_proc_name(shell_pid: u32) -> Option<String> {
    let out = crate::proc::command("ps")
        .args(["-A", "-o", "pid=,ppid=,comm="])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let basename = |comm: &str| -> String {
        std::path::Path::new(comm)
            .file_name()
            .and_then(|x| x.to_str())
            .unwrap_or(comm)
            .to_string()
    };
    let mut best_child: Option<(u32, String)> = None;
    let mut shell_comm: Option<String> = None;
    for line in s.lines() {
        let mut it = line.split_whitespace();
        let (Some(pid), Some(ppid)) = (
            it.next().and_then(|x| x.parse::<u32>().ok()),
            it.next().and_then(|x| x.parse::<u32>().ok()),
        ) else {
            continue;
        };
        let comm = it.collect::<Vec<_>>().join(" ");
        if pid == shell_pid {
            shell_comm = Some(basename(&comm));
        } else if ppid == shell_pid && best_child.as_ref().is_none_or(|(p, _)| *p < pid) {
            best_child = Some((pid, basename(&comm)));
        }
    }
    best_child.map(|(_, n)| n).or(shell_comm)
}

/// Resolve a process's current working directory via lsof. macOS has no
/// `/proc`; `lsof -d cwd` prints the cwd path. Called ~once/sec by the git
/// panel poll, so the subprocess cost is acceptable.
#[cfg(unix)]
pub(crate) fn pid_cwd(pid: u32) -> Option<std::path::PathBuf> {
    let out = crate::proc::command("lsof")
        .args(["-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"])
        .output()
        .ok()?;
    out.stdout
        .split(|&b| b == b'\n')
        .find_map(|line| line.strip_prefix(b"n").map(unescape_lsof_path))
}

/// `lsof -F` escapes non-ASCII / non-printable bytes as `\xHH` (and a literal
/// backslash as `\\`) when it runs under a non-UTF-8 locale — exactly what
/// happens when kasaterm is launched from Finder/Dock with no `LANG` set, which
/// otherwise renders a 한글 cwd as `\xec\xa7\x80`. Reverse the escaping on the raw
/// bytes so the path survives; already-plain output passes through untouched.
#[cfg(unix)]
fn unescape_lsof_path(line: &[u8]) -> std::path::PathBuf {
    use std::os::unix::ffi::OsStringExt;
    let mut bytes = Vec::with_capacity(line.len());
    let mut i = 0;
    while i < line.len() {
        if line[i] == b'\\' && i + 1 < line.len() {
            match line[i + 1] {
                b'x' if i + 4 <= line.len() => {
                    if let Some(b) = std::str::from_utf8(&line[i + 2..i + 4])
                        .ok()
                        .and_then(|h| u8::from_str_radix(h, 16).ok())
                    {
                        bytes.push(b);
                        i += 4;
                        continue;
                    }
                }
                b'\\' => {
                    bytes.push(b'\\');
                    i += 2;
                    continue;
                }
                _ => {}
            }
        }
        bytes.push(line[i]);
        i += 1;
    }
    std::path::PathBuf::from(std::ffi::OsString::from_vec(bytes))
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

/// User's per-character image override dir — `~/.config/kasaterm/students/`.
/// Drop `<slug>-profile.png` / `<slug>-<i>.png` / `<slug>-walk-<i>.png` /
/// `schale-logo.png` here to replace the bundled default dots (see
/// render.rs loaders). Missing dir/file → the loader falls back to the
/// compiled-in `include_bytes!` asset, so an empty dir changes nothing.
pub fn students_dir() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("KASATERM_STUDENTS_DIR") {
        if !p.is_empty() {
            return Some(std::path::PathBuf::from(p));
        }
    }
    let home = std::env::var("HOME").ok()?;
    Some(std::path::PathBuf::from(home).join(".config/kasaterm/students"))
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

/// Where window tabs live: "side" (Warp-style vertical sidebar list, the
/// default) or "top" (Windows Terminal-style horizontal tabs in the title
/// strip). Only an explicit "top" opts into the title-strip tabs; anything
/// else — including a missing key — falls back to the side strip.
pub fn read_tab_position() -> String {
    match read_settings().get("tab_position").and_then(|x| x.as_str()) {
        Some("top") => "top".to_string(),
        _ => "side".to_string(),
    }
}

/// Base cell font size (logical px) from settings. Missing/invalid → the
/// built-in default (16). Clamped to the same sane range the stepper offers.
pub fn read_font_size() -> f32 {
    read_settings()
        .get("font_size")
        .and_then(|x| x.as_f64())
        .map(|v| (v as f32).clamp(9.0, 32.0))
        .unwrap_or(16.0)
}

/// Whether the file-tree sidebar starts open on launch. Default `false`
/// (terminal-only first screen).
pub fn read_file_tree_default() -> bool {
    read_settings()
        .get("file_tree_default")
        .and_then(|x| x.as_bool())
        .unwrap_or(false)
}

/// 새 pane 의 하단 상태바(cwd/branch/diff)를 기본으로 보일지. 키 없으면 true —
/// footer 는 원래 기본 표시였으니 기존 사용자 동작을 유지한다.
pub fn read_footer_default() -> bool {
    read_settings()
        .get("pane_footer_default")
        .and_then(|x| x.as_bool())
        .unwrap_or(true)
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

/// Per-pane claude wrapper knobs, read by the shim installer and the settings
/// "클로드" tab. Invariants (session-id / --settings / task-list) are never
/// keyed here — they stay hardcoded in the wrapper.
pub fn read_claude_persona() -> bool {
    read_settings().get("claude_persona").and_then(|x| x.as_bool()).unwrap_or(true)
}
pub fn read_claude_model() -> String {
    read_settings().get("claude_model").and_then(|x| x.as_str()).unwrap_or("").to_string()
}
pub fn read_claude_effort() -> String {
    read_settings().get("claude_effort").and_then(|x| x.as_str()).unwrap_or("").to_string()
}
pub fn read_claude_extra() -> String {
    read_settings().get("claude_extra").and_then(|x| x.as_str()).unwrap_or("").to_string()
}

/// shim 주입 전역 스위치(설정 "shim_inject"). false 면 install_pane_shims 가 shim dir 를
/// 아예 안 만들어 PATH/ZDOTDIR 무접촉 → 순정 claude(캐릭터·프록시·훅·board 전무 진짜 독립).
/// 기본 true(하위호환 — 지금 풀 경험 유지). install 은 부팅 1회라 변경은 재시작 후 적용.
pub fn read_shim_inject() -> bool {
    read_settings().get("shim_inject").and_then(|x| x.as_bool()).unwrap_or(true)
}

/// Persist the last logical window size so the next launch restores it instead
/// of the hardcoded default. Logical (DPI-independent) so moving between a
/// Retina and an external display restores the same on-screen size.
/// Persist the window frame: logical size + (optionally) the outer position in
/// physical px. `pos: None` keeps whatever position the file already has — the
/// size-only callers must not erase a previously saved position.
pub fn write_window_frame(w: f64, h: f64, pos: Option<(f64, f64)>) {
    use std::io::Write;
    let Some(path) = window_size_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let pos = pos.or_else(read_window_pos);
    let body = match pos {
        Some((x, y)) => format!("{{\"w\":{w},\"h\":{h},\"x\":{x},\"y\":{y}}}"),
        None => format!("{{\"w\":{w},\"h\":{h}}}"),
    };
    if let Ok(mut f) = std::fs::File::create(&path) {
        let _ = f.write_all(body.as_bytes());
    }
}

/// Read the persisted outer window position (physical px), if one was saved.
/// No range validation here — the caller checks the point against the live
/// monitor set (a saved monitor may be unplugged by now).
pub fn read_window_pos() -> Option<(f64, f64)> {
    let path = window_size_path()?;
    let txt = std::fs::read_to_string(&path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&txt).ok()?;
    Some((v.get("x")?.as_f64()?, v.get("y")?.as_f64()?))
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


/// claude 의 saved default effort(~/.claude/settings.json `effortLevel`). resume 직후 GUI effort
/// 카드 폴백값(거노). 파일/키 없으면 빈 문자열. ultracode 는 session-only 라 여기 안 저장된다.
fn claude_saved_effort() -> String {
    let Some(home) = std::env::var_os("HOME") else { return String::new() };
    let path = std::path::Path::new(&home).join(".claude/settings.json");
    let Ok(text) = std::fs::read_to_string(&path) else { return String::new() };
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| v.get("effortLevel").and_then(|e| e.as_str()).map(str::to_string))
        .unwrap_or_default()
}

/// 살아있는 claude 프로세스 argv 에서 session_id → 포크 부모 session_id 맵. detach 는 세션을
/// 포크(`--fork-session --resume <부모>.jsonl --session-id <포크>`)해 새 id 를 발급하므로
/// session_characters.json 의 원본 바인딩 키가 어긋나 재진입 시 랜덤 둔갑한다(거노: bg 재진입
/// 학생 바뀜, foreground 는 원래 유지). 데몬 프로세스 env(KASATERM_CHARACTER)는 세션별이
/// 아니라 데몬 띄운 셸값을 전 세션이 공유해 못 쓴다(거노: 한 뷰 다 같은 학생). 대신 argv 의
/// `--resume <부모>` 사슬을 따라 원본 세션의 바인딩까지 되짚는다. `ps`(env 불필요) 1회/프로세스,
/// 2s 캐시. parent = --resume 값의 파일명 stem(=uuid) 또는 값 그대로.
fn daemon_session_parents() -> HashMap<String, String> {
    static CACHE: std::sync::LazyLock<
        std::sync::Mutex<Option<(std::time::Instant, HashMap<String, String>)>>,
    > = std::sync::LazyLock::new(|| std::sync::Mutex::new(None));
    if let Some((t, m)) = CACHE.lock().unwrap().as_ref() {
        if t.elapsed() < std::time::Duration::from_secs(2) {
            return m.clone();
        }
    }
    let mut map = HashMap::new();
    for (pid, _ppid, name) in kasa_pty::process_table() {
        if !name.contains("claude") {
            continue;
        }
        let Some(cmd) = kasa_pty::process_cmdline(pid) else { continue };
        let toks: Vec<&str> = cmd.split_whitespace().collect();
        let val_after = |flag: &str| {
            toks.iter().position(|t| *t == flag).and_then(|i| toks.get(i + 1)).copied()
        };
        if let (Some(sid), Some(resume)) = (val_after("--session-id"), val_after("--resume")) {
            let parent = std::path::Path::new(resume)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(resume);
            if !sid.is_empty() && !parent.is_empty() && sid != parent {
                map.entry(sid.to_string()).or_insert_with(|| parent.to_string());
            }
        }
    }
    *CACHE.lock().unwrap() = Some((std::time::Instant::now(), map.clone()));
    map
}

/// Pull the claude session id straight off the running claude process's argv
/// (`--resume <uuid>` / `--session-id <uuid>`, `=`-joined or space-separated).
/// Exact per-pane — unlike the cwd-mtime guess, two claudes in the same cwd
/// keep distinct ids. Returns None for a fresh `claude` with no id on its argv.
fn claude_session_id_from_cmdline(shell_pid: u32) -> Option<String> {
    // Most-recently-spawned claude child of this shell — shared with the
    // transcript watcher's self-map path.
    let pid = claude_child_pid(shell_pid)?;
    let argv = kasa_pty::process_cmdline(pid)?;
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

/// pane 의 claude 가 `agents`/`attach` 뷰인지 — argv 토큰 검사. 값 플래그(persona 텍스트
/// 등)가 argv 에 섞이는 일반 세션을 오탐하지 않게, 세션형 플래그(--session-id/--resume/
/// --append-system-prompt)가 하나라도 있으면 뷰가 아니라고 본다(shim 이 일반 부팅엔
/// 항상 그중 하나를 얹고, agents/attach 엔 PERSONA_OK 게이트로 하나도 안 얹는다).
fn claude_view_subcommand(shell_pid: u32) -> Option<&'static str> {
    let pid = claude_child_pid(shell_pid)?;
    let argv = kasa_pty::process_cmdline(pid)?;
    let tokens: Vec<&str> = argv.split_whitespace().collect();
    if tokens
        .iter()
        .any(|t| matches!(*t, "--session-id" | "--resume" | "-r" | "--append-system-prompt"))
    {
        return None;
    }
    for tok in &tokens {
        if *tok == "agents" {
            return Some("agents");
        }
        if *tok == "attach" {
            return Some("attach");
        }
    }
    None
}

/// 화면 텍스트에서 statusline 이 실어 보낸 세션 id 마커(`⟦sid8⟧`, SGR8 로 은닉)를
/// 찾는다 — 마지막(최하단) 것을 취해 이 pane 이 "지금" 표시 중인 세션을 얻는다.
/// agents 피커 attach 는 이벤트·argv 흔적이 없어 이 채널이 유일한 진입-즉시 신호다.
pub(crate) fn screen_marker_sid8(text: &str) -> Option<String> {
    let mut found = None;
    let mut rest = text;
    while let Some(i) = rest.find('⟦') {
        let after = &rest[i + '⟦'.len_utf8()..];
        let cand: String = after.chars().take(8).collect();
        let close = after.chars().nth(8);
        if cand.len() == 8
            && cand.chars().all(|c| c.is_ascii_hexdigit())
            && close == Some('⟧')
        {
            found = Some(cand.to_ascii_lowercase());
        }
        rest = after;
    }
    found
}

/// sid 앞 8자 → 풀 세션 id. 라이브 agents 세션에서 프리픽스 유일 매칭, 없으면
/// transcript 파일명(projects 전수)에서 유일 매칭. 모호(2+)하면 None — 오귀속 금지.
fn resolve_sid8(sid8: &str) -> Option<String> {
    let agents = agents_cached().0;
    let mut hits: Vec<&String> = agents.keys().filter(|k| k.starts_with(sid8)).collect();
    if hits.len() == 1 {
        return Some(hits.pop().unwrap().clone());
    }
    if hits.len() > 1 {
        return None;
    }
    let home = std::env::var("HOME").ok()?;
    let projects = std::path::Path::new(&home).join(".claude").join("projects");
    let mut found: Option<String> = None;
    for d in std::fs::read_dir(projects).ok()?.flatten() {
        for f in std::fs::read_dir(d.path()).ok()?.flatten() {
            let name = f.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.starts_with(sid8) && name.ends_with(".jsonl") {
                let sid = name.trim_end_matches(".jsonl").to_string();
                if found.as_ref().is_some_and(|p| p != &sid) {
                    return None;
                }
                found = Some(sid);
            }
        }
    }
    found
}

/// agents 뷰 pane 의 OSC 타이틀 → 세션 name. 타이틀은 "<스피너 글리프> <세션 name>"
/// 꼴이라 선행 브라유 스피너(⠐… U+2800 블록)·별표류·공백을 벗겨 name 만 남긴다 —
/// `claude agents --json` 의 name 과 정확 일치해야 rebind_agents_panes 가 매칭한다.
fn title_session_name(t: &str) -> &str {
    t.trim_start_matches(|c: char| {
        ('\u{2800}'..='\u{28FF}').contains(&c)
            || matches!(c, '✳' | '✻' | '✢' | '✽' | '*' | '＊' | '∗')
            || c.is_whitespace()
    })
    .trim()
}

/// `claude attach <sid>` 의 대상 세션 id — attach 는 위치 인자라 기존
/// claude_session_id_from_cmdline(--resume/--session-id 전용)이 못 잡는다.
fn attach_target_from_cmdline(shell_pid: u32) -> Option<String> {
    let pid = claude_child_pid(shell_pid)?;
    let argv = kasa_pty::process_cmdline(pid)?;
    let tokens: Vec<&str> = argv.split_whitespace().collect();
    for (i, tok) in tokens.iter().enumerate() {
        if *tok == "attach" {
            return tokens.get(i + 1).filter(|v| is_uuid(v)).map(|v| (*v).to_string());
        }
    }
    None
}

/// The pid of the claude process tied to a shell pane, if any. Normal panes run
/// `zsh → claude` (claude is a child). Background sessions (claude daemon) invert
/// it — `claude --session-id … → zsh`, so claude is the shell's *parent*. We try
/// the direct child first; if there's none, walk the shell's parent chain and take
/// the first claude. Returns None only when no claude is involved at all.
fn claude_child_pid(shell_pid: u32) -> Option<u32> {
    // pid -> (ppid, is_claude). One process_table() pass feeds both the child
    // and parent walk. process_table() is cross-platform and returns the bare
    // exe name ("claude.exe" on Windows, "claude" on Unix), so a substring
    // match is enough.
    let mut procs: std::collections::HashMap<u32, (u32, bool)> = std::collections::HashMap::new();
    for (pid, ppid, name) in kasa_pty::process_table() {
        let is_claude = name.contains("claude");
        procs.insert(pid, (ppid, is_claude));
    }
    // 1) Normal pane: most-recent (highest-pid) claude child of the shell.
    if let Some(p) = procs
        .iter()
        .filter(|(_, (ppid, claude))| *ppid == shell_pid && *claude)
        .map(|(pid, _)| *pid)
        .max()
    {
        return Some(p);
    }
    // 2) Background (claude daemon): claude wraps the shell. Walk the parent chain
    //    and take the first claude (`claude --session-id … → zsh`).
    let mut cur = shell_pid;
    for _ in 0..8 {
        let ppid = procs.get(&cur)?.0;
        if procs.get(&ppid).map_or(false, |(_, claude)| *claude) {
            return Some(ppid);
        }
        if ppid <= 1 {
            break;
        }
        cur = ppid;
    }
    None
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
/// 세션 id → transcript jsonl 경로(`~/.claude/projects/*/<sid>.jsonl` 전수 스캔).
/// ResumeSession(attach/재개)이 세션 id 만 아는 시점에 bind_transcript 로 pane↔세션을
/// 즉석 확정할 때 쓴다 — cwd 로 프로젝트 dir 슬러그를 재현하는 대신 실재 파일을 찾아
/// claude 의 슬러그 규칙 드리프트에 무해하다. 세션 id 는 uuid 라 전역 유일.
pub(crate) fn transcript_path_for_session(sid: &str) -> Option<std::path::PathBuf> {
    if sid.is_empty() || sid.contains('/') {
        return None;
    }
    let home = std::env::var("HOME").ok()?;
    let projects = std::path::Path::new(&home).join(".claude").join("projects");
    let want = format!("{sid}.jsonl");
    for d in std::fs::read_dir(projects).ok()?.flatten() {
        let p = d.path().join(&want);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

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
    // agents/attach 뷰 pane 은 여기서 절대 추측하지 않는다 — 어느 세션을 보는지 cwd 로
    // 알 수 없어, recent-jsonl 폴백이 같은 cwd 의 남의 활성 세션을 훔쳤다(거노: bg 뷰
    // pane 들이 전부 첫 세션 학생으로 쏠림). 이 pane 의 바인딩은 rebind_agents_panes
    // (attach 인자·타이틀↔세션명 매칭)가 전담한다.
    if claude_view_subcommand(shell_pid).is_some() {
        return None;
    }
    // 폴백: cwd 의 최근(<30분) 활동 jsonl. 단 **정확히 1개일 때만** bind 한다.
    // 0개 = fresh claude 가 자기 세션을 아직 안 씀(부팅 중) → None, 다음 사이클
    // 재시도(곧 쓰면 잡힘). 2+ = 같은 cwd 에 여러 claude(여러 pane 공유) →
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


#[cfg(test)]
mod agents_view_tests {
    use super::*;

    #[test]
    fn screen_marker_sid8_finds_last_valid_marker() {
        // statusline 끝자락의 은닉 마커 — 8자리 hex 만, 마지막(최하단) 것을 취한다.
        assert_eq!(
            screen_marker_sid8("… xhigh ⟦2535079b⟧\n❯"),
            Some("2535079b".to_string())
        );
        assert_eq!(
            screen_marker_sid8("⟦11111111⟧ 이전 프레임\n새 프레임 ⟦8bed8dfc⟧"),
            Some("8bed8dfc".to_string())
        );
        // 8자 미만·비hex·닫힘 없음은 무시.
        assert_eq!(screen_marker_sid8("⟦abc⟧ ⟦zzzzzzzz⟧ ⟦12345678"), None);
        assert_eq!(screen_marker_sid8("마커 없음"), None);
    }

    #[test]
    fn title_session_name_strips_spinner_glyphs() {
        // 실측 타이틀: 브라유 스피너 + 공백 + 세션 name (agents 뷰 pane).
        assert_eq!(title_session_name("⠐ 대시보드 로그인 env 문제 해결"), "대시보드 로그인 env 문제 해결");
        assert_eq!(title_session_name("✻ 학생 프사 크기와 전신 모션 개선"), "학생 프사 크기와 전신 모션 개선");
        // 스피너 없는 생 타이틀·앞뒤 공백도 name 으로 수렴.
        assert_eq!(title_session_name("  tmuxify-58 "), "tmuxify-58");
        // 전부 글리프면 빈 문자열(매칭 스킵 신호).
        assert_eq!(title_session_name("⠐⠑ "), "");
    }
}
