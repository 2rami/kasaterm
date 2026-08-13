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
use kasa_socket::sessions::{is_uuid, recent_sessions_here, session_jsonl_path};
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
            cwd: None,
            character: None,
        }])
    }

    fn focus_surface(&self, _surface_id: &str) -> Result<()> {
        // Single pane — no-op. Multi-pane phase will route to tmux's
        // `select-pane -t <id>`.
        Ok(())
    }

    fn split_surface(
        &self,
        direction: SplitDirection,
        focus: bool,
        _from: Option<&str>,
    ) -> Result<SurfaceInfo> {
        // tmux 백엔드는 늘 현재 pane 을 쪼갠다 — 대상 지정은 로컬 PTY 경로만.
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
            // tmux 백엔드는 pane 픽셀 크기를 우리가 모른다(tmux 가 레이아웃 주인).
            // 종횡비 판정은 로컬 PTY 경로 전용이라 여기선 가로로 떨어진다 — 창이
            // 대개 가로로 넓으니 옛 기본과 같은 결과다.
            SplitDirection::Auto => "split-window -h",
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
            cwd: None,
            character: None,
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
    /// surface_id → 그 pane 이 **지금 돌리는** 서브에이전트·백그라운드 셸. `PreToolUse`/
    /// `PostToolUse` 훅이 `agent_status` 로 채운다. `attention` 과 같이 GUI
    /// (`App.collab.hook_activity`)와 Arc 공유 — 쓰는 쪽은 소켓 스레드, 읽는 쪽은
    /// 진행 표시(GUI)다. 이게 있기 전엔 transcript 꼬리에서 런치·회수를 짝지었는데,
    /// 꼬리가 64KB 라 세션이 커지면 런치가 밀려나 **오래 걸리는 작업일수록 안 보였다**.
    hook_activity: Arc<Mutex<HashMap<String, crate::state::HookActivity>>>,
    /// hook-free 발견 스로틀 — `discover_unbound` 의 ps/lsof 비용을 board 폴(1/s)
    /// 마다 다 치르지 않도록 2s 에 1회로 제한한 마지막 실행 시각.
    last_discover: Arc<Mutex<Option<std::time::Instant>>>,
    /// pane 셸 pid → (조회시각, 라이브 cwd). collab_board 가 학생 경로(cd 반영)를
    /// transcript 가 아닌 PTY pid_cwd 로 채우되, lsof 비용을 2s 캐시로 제한한다.
    cwd_cache: Arc<Mutex<HashMap<u32, (std::time::Instant, std::path::PathBuf)>>>,
    /// surface_id → statusLine 이 보고한 "현재 보는 경로"(report_cwd). claude 내부 cd 는
    /// lsof(cwd_cache)로 안 보여, statusline.py 가 매 렌더 직접 push 한다.
    reported_cwd: Arc<Mutex<HashMap<String, String>>>,
    /// surface_id → statusLine 이 보고한 (컨텍스트 창, 사용 토큰). 하네스가 훅 stdin 으로
    /// 준 값이라 ctx% 분모의 정본이다 — transcript 의 model 엔 `[1m]` 이 안 실려(API 응답
    /// 이 `claude-opus-5`) 모델명 추정으로는 1M 세션이 200k 로 잡혔다(18만 토큰이 92%로
    /// 보이던 원인). 미보고(구버전 statusline·창 미상)면 없음 → 추정 폴백.
    reported_ctx: Arc<Mutex<HashMap<String, (u64, u64)>>>,
    /// surface_id → 마지막 유효 (context_tokens, context_limit). transcript usage 가 tail
    /// 윈도에 없어 0 으로 떨어질 때 직전 값을 유지해 컨텍스트량·인연%가 0 으로 깜빡이지
    /// 않게 한다(거노: statusline 잘려도 화면파싱 말고 정확 추적 — 정확 소스만 신뢰).
    last_ctx: Arc<Mutex<HashMap<String, (u64, u64)>>>,
    /// codex rollout 경로 → 그 세션의 model. codex 는 model 이 실린 유일한 줄
    /// (`turn_context`)이 파일 **앞** 87~122KB 에 있어 tail 창에 영영 안 걸린다 —
    /// 그래서 머리를 한 번만 읽어 여기 기억한다. surface_id 가 아니라 **경로**가
    /// 키다: 세션을 갈아타면 경로가 바뀌어 저절로 다시 읽는다.
    codex_cfg: Arc<Mutex<HashMap<String, (String, String)>>>,
    /// surface_id → statusline 이 보고한 (model.id, effort.level). 재시작 뒤 그 pane 을
    /// **끄기 직전 쓰던 모델·effort 로** 되살리는 데 쓴다(세션 저장에 실린다).
    ///
    /// ★ board 의 `model` 과 **일부러 다른 값**이다. 그쪽은 API 응답 표기(`claude-opus-5`)
    /// 나 화면 표시명이라 사람이 읽기엔 낫지만 `[1m]` 이 없어, 복원 명령에 되먹이면 1M
    /// 세션이 200k 로 강등된다. 여기 담기는 `model.id` 만이 CLI 에 그대로 돌려줄 수 있다.
    reported_agent_cfg: Arc<Mutex<HashMap<String, (String, String)>>>,
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
    /// agents/attach 뷰로 판정된 pane 집합 — rebind_agents_panes(3s 폴러)가 재구축.
    /// 뷰 pane 의 statusline report-cwd 는 뷰어 프로세스 자신의 cwd(pane 스폰 경로)지
    /// 표시 중인 세션의 프로젝트가 아니라, 파일트리 오버라이드로 흘리면 transcript
    /// 유래 진짜 세션 cwd 를 덮는다(거노: bg 세션 파일트리가 pane cwd 고착) —
    /// report_cwd 가 이 집합을 보고 GUI 이벤트를 생략한다.
    view_panes: Arc<Mutex<HashSet<String>>>,
    /// surface_id → 명시적 완료 보고(`kasaterm-cli done`). transcript 휴리스틱은
    /// "놀고 있다"만 알지 "맡은 일이 성공/실패로 끝났다"는 모른다 — 학생 자기 보고가
    /// board 완료 판정의 정본. 소거 규칙은 board 빌더 참조(idle 을 지나 다시 working
    /// = 새 브리프 → 스테일).
    done_reports: Arc<Mutex<HashMap<String, DoneReport>>>,
}

/// 한 pane 의 명시적 완료 보고 한 건. `idle_seen`: 보고 직후엔 그 턴이 아직
/// working 이라(보고 명령 자체가 턴 안에서 돈다) "working 이면 소거"를 즉시 적용하면
/// 한 번도 못 보인다 — idle 을 한 번 관찰한 뒤의 working 만 새 브리프로 친다.
struct DoneReport {
    outcome: String,
    summary: String,
    at: std::time::Instant,
    idle_seen: bool,
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

/// 세션 name → sessionId(모호 이름 제외, 2s 캐시 공유). GUI 렌더의 agents 뷰
/// 세션 행 캐릭터 칩(행 name 을 sid→캐릭터로 역추적)에서도 쓴다.
pub(crate) fn agents_name_sids_cached() -> HashMap<String, String> {
    agents_cached().1
}

impl PtyBackend {
    /// 살아 있는 surface 전부 — BSP leaf(`ws.panes`) **와 탭 pid**(`ws.pid_to_pane`).
    ///
    /// `panes` 만 모으면 탭으로 띄운 학생이 transcript 바인딩 후보에서부터 빠지고,
    /// 그러면 board 의 `bound.filter(live.contains)` 에서도 탈락해 **아예 등재되지
    /// 않는다** — 화면에도 board 에도 없는 유령이 된다(거노 2026-08-07).
    fn live_surfaces(&self) -> std::collections::HashSet<String> {
        let ws = self.ws.lock().unwrap();
        ws.panes.keys().cloned().chain(ws.pid_to_pane.keys().cloned()).collect()
    }

    /// surface_id → (model, effort) 스냅샷. 세션 저장이 leaf 에 실으려고 읽는다.
    ///
    /// GUI(`App`)가 `socket_backend` 로 이 백엔드를 들고 있으므로 App 쪽에 같은 맵을
    /// 하나 더 두지 않는다 — `pane_claude_sid` 처럼 이벤트로 넘기면 App struct 에 필드가
    /// 늘고, 그 자리는 워커 여럿이 동시에 못 만지는 병목이다(CLAUDE.md).
    pub(crate) fn agent_cfg_snapshot(&self) -> HashMap<String, (String, String)> {
        self.reported_agent_cfg.lock().unwrap().clone()
    }

    /// `attention` is shared with the GUI (`App.collab.attention`): the CLI
    /// hook path (`kasaterm-cli attention`) and the GUI's grid-scan prompt
    /// detection both write it, so the board's `waiting` flag reflects either.
    /// `hook_activity` 도 같은 이유로 공유 — 훅은 이 소켓으로 들어오고, 그걸 그리는
    /// 것은 GUI 다.
    pub fn new(
        proxy: EventLoopProxy<UserEvent>,
        ws: Arc<Mutex<Workspace>>,
        attention: Arc<Mutex<HashMap<String, String>>>,
        hook_activity: Arc<Mutex<HashMap<String, crate::state::HookActivity>>>,
        pane_status_pub: Arc<Mutex<HashMap<String, PaneStatus>>>,
        bg_agents: Arc<Mutex<HashMap<String, Option<String>>>>,
    ) -> Self {
        Self {
            proxy,
            ws,
            bound: Arc::new(Mutex::new(HashMap::new())),
            attention,
            hook_activity,
            last_discover: Arc::new(Mutex::new(None)),
            cwd_cache: Arc::new(Mutex::new(HashMap::new())),
            reported_cwd: Arc::new(Mutex::new(HashMap::new())),
            reported_ctx: Arc::new(Mutex::new(HashMap::new())),
            last_ctx: Arc::new(Mutex::new(HashMap::new())),
            codex_cfg: Arc::new(Mutex::new(HashMap::new())),
            reported_agent_cfg: Arc::new(Mutex::new(HashMap::new())),
            pane_status_pub,
            bg_agents,
            nudged: Arc::new(Mutex::new(HashMap::new())),
            view_panes: Arc::new(Mutex::new(HashSet::new())),
            done_reports: Arc::new(Mutex::new(HashMap::new())),
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
        // 이번 패스의 뷰 pane 집합 — 끝에서 통째 교체해 죽은 pane·뷰 종료가
        // 자연히 빠진다(뷰가 아니게 된 pane 의 statusline report 는 다시 흐름).
        let mut views: HashSet<String> = HashSet::new();
        for (id, shell_pid) in self.query_pane_pids() {
            if !live.contains(&id) {
                continue;
            }
            let Some(sub) = claude_view_subcommand(shell_pid) else { continue };
            views.insert(id.clone());
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
            } else {
                // 바인딩이 그대로여도 view_cwd 는 재공표 — 뷰 판정이 3s 폴이라
                // 첫 판정 전에 statusline report 가 오버라이드를 pane cwd 로 덮는
                // 선착 경합이 있다. 매 패스 진실(transcript cwd)로 재수렴시킨다.
                self.publish_transcript_cwd(&id, &path);
            }
        }
        *self.view_panes.lock().unwrap() = views;
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
    /// split 은 GUI 스레드에서 도니 reply 채널로 새 pane id 를 받아 돌려준다
    /// (`split_surface` 와 같은 패턴) — 디스패처가 그 주소로 브리프를 쏜다.
    fn spawn_student(&self, character: &str) -> Result<String> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.proxy
            .send_event(UserEvent::SocketSpawnStudent(character.to_string(), tx))
            .map_err(|_| anyhow::anyhow!("gui event loop gone"))?;
        Ok(rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap_or_default())
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

    /// 활성 pane 의 하네스. `active_process_name` 은 직속 자식 이름이라 codex 를 못
    /// 본다(npm shim → `node`) — 판정은 kasa-pty 의 `agent_for_shell` 에 맡긴다.
    fn active_agent(&self) -> Option<String> {
        let active = self.ws.lock().unwrap().active_pane.clone()?;
        let pid = self
            .query_pane_pids()
            .into_iter()
            .find(|(id, _)| *id == active)
            .map(|(_, p)| p)?;
        kasa_pty::agent_for_shell(&kasa_pty::process_table_shared(), pid)
            .map(|k| k.as_str().to_string())
    }

    /// pane → claude session_id(`/pane-tasks` 용) = bound transcript 파일명 stem.
    /// normal claude 는 transcript==session 이라 task store dir(`session-<id 첫8hex>`)
    /// 매핑에 폴백으로 쓴다.
    fn pane_session_ids(&self) -> Result<Vec<(String, String)>> {
        let live = self.live_surfaces();
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

    /// cwd·학생까지 실어 준다 — board 가 못 싣는 pane 이 있기 때문이다.
    ///
    /// board 는 transcript 가 바인딩된 pane 만 순회하므로 codex pane 이나 셸뿐인 pane 은
    /// 줄이 아예 없다. `dismiss` 는 닫기 전에 그 pane 의 cwd 로 커밋 안 된 변경을 세는데,
    /// board 만 보면 그 pane 들은 cwd 를 모른 채 **보호 없이 닫혔다**(실측: codex pane 이
    /// `closed %5 ? — ` 로 학생도 폴더도 없이 닫혔다).
    ///
    /// cwd 는 GUI 가 공표하는 맵에서 읽는다 — `window_layout` 이 쓰는 그 맵이라 여기서도
    /// lsof 없이 조회로 끝난다.
    fn list_surfaces(&self) -> Result<Vec<SurfaceInfo>> {
        let status = self.pane_status_pub.lock().unwrap().clone();
        let ws = self.ws.lock().unwrap();
        Ok(ws
            .panes
            .keys()
            .map(|id| SurfaceInfo {
                id: id.clone(),
                workspace_id: FIXED_WORKSPACE_ID.into(),
                title: None,
                cwd: status.get(id).map(|s| s.cwd.to_string_lossy().into_owned()),
                character: ws.pane_character.get(id).cloned(),
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
        // 60개. 20이면 이 폴더의 목록이 최근 claude 로만 채워져, 같은 폴더에서
        // codex 로 일한 기록이 한 줄도 안 보인다(tmuxify 실측: 20칸 전부 claude,
        // 60칸이면 비-claude 6개가 올라온다). 값은 release 로 재고 정했다.
        Ok(recent_sessions_here(&base, 60))
    }

    fn resume_session(
        &self,
        id: &str,
        cwd: Option<&str>,
        newroom: bool,
        attach: bool,
        harness: &str,
    ) -> Result<()> {
        self.proxy
            .send_event(UserEvent::ResumeSession {
                id: id.to_string(),
                cwd: cwd.map(str::to_string),
                newroom,
                attach,
                harness: harness.to_string(),
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

    fn report_cwd(
        &self,
        surface_id: &str,
        cwd: &str,
        session_id: &str,
        ctx_window: u64,
        ctx_tokens: u64,
        model: &str,
        effort: &str,
    ) -> Result<()> {
        self.reported_cwd
            .lock()
            .unwrap()
            .insert(surface_id.to_string(), cwd.to_string());
        // 둘 중 **하나라도** 실려 오면 채택한다. 빈 값은 "미보고"라 종전 값을 안 덮는다 —
        // effort 는 아예 안 정한 세션이 흔해서, 빈 effort 때문에 model 까지 버리면 안 된다.
        if !model.is_empty() || !effort.is_empty() {
            let mut cfg = self.reported_agent_cfg.lock().unwrap();
            let e = cfg.entry(surface_id.to_string()).or_default();
            if !model.is_empty() {
                e.0 = model.to_string();
            }
            if !effort.is_empty() {
                e.1 = effort.to_string();
            }
        }
        // 창을 아는 보고만 채택 — 0 은 "미보고"라 옛 정답을 덮지 않는다. 뷰 pane 도
        // 저장한다: cwd 와 달리 컨텍스트는 뷰어 자신의 것이 맞고, 그 pane 의 ctx% 는
        // 뷰어 세션 기준으로 보여야 한다.
        if ctx_window > 0 {
            self.reported_ctx
                .lock()
                .unwrap()
                .insert(surface_id.to_string(), (ctx_window, ctx_tokens));
        }
        // agents/attach 뷰 pane: 이 보고는 뷰어 claude 프로세스 자신의 cwd(pane
        // 스폰 경로)지 표시 중인 세션의 프로젝트가 아니다 — GUI 로 흘리면
        // publish_transcript_cwd 가 넣은 진짜 세션 cwd 를 매 렌더 덮는다(거노:
        // bg 세션 파일트리가 pane cwd 고착). 오버라이드는 transcript bind 에 맡긴다.
        // (session_id 바인딩도 뷰 pane 은 뷰어 세션이라 오염되므로 함께 스킵.)
        if self.view_panes.lock().unwrap().contains(surface_id) {
            return Ok(());
        }
        // pane 활성 세션의 real sid 로 pane_claude_sid 를 보강(SocketSessionBound 재사용).
        // bg job(bind-transcript hook 을 CLAUDE_JOB_DIR 로 스킵)·포크(SessionStart 가
        // 못 온 pane)는 pane_claude_sid 가 비어 display_pane_char 가 None → statusline
        // 프사·이름이 빈다(거노: bg 세션 얼굴 없고 그 자리 배경만 = F/H). statusline 은
        // 이 세션에서도 매 렌더 real sid 를 report 하므로, 이 경로가 pane→세션 바인딩의
        // 최후 보루가 된다(handler arm 이 같은 sid 면 no-op → 매 report 부하 없음).
        if !session_id.is_empty() {
            let _ = self.proxy.send_event(UserEvent::SocketSessionBound(
                surface_id.to_string(),
                session_id.to_string(),
            ));
        }
        // GUI 파일트리가 "pane 이 보는 경로"를 셸 cwd 보다 우선하도록 위임.
        let _ = self.proxy.send_event(UserEvent::SocketViewCwd(
            surface_id.to_string(),
            std::path::PathBuf::from(cwd),
        ));
        Ok(())
    }

    fn split_surface(
        &self,
        direction: SplitDirection,
        focus: bool,
        from: Option<&str>,
    ) -> Result<SurfaceInfo> {
        let dir = match direction {
            SplitDirection::Right | SplitDirection::Left => Some(kasa_pty::SplitDir::Horizontal),
            SplitDirection::Up | SplitDirection::Down => Some(kasa_pty::SplitDir::Vertical),
            // 여기선 못 정한다 — pane 픽셀 크기는 GUI 스레드만 안다.
            SplitDirection::Auto => None,
        };
        // Split runs on the GUI thread; block on a reply channel so we can hand
        // the new pane's real id back to the caller. The teammate launcher uses
        // it as the `-t` target for every follow-up send-keys — returning the
        // old "pane-new" placeholder dropped the `claude …` launch silently.
        // `focus` rides along so the GUI thread keeps focus on the current pane
        // unless the caller opted in (CLI `--focus`).
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = self.proxy.send_event(UserEvent::SocketSplit(
            dir,
            focus,
            from.map(str::to_string),
            tx,
        ));
        // 타임아웃이 넉넉한 이유: GUI 스레드가 답하는 데 걸리는 시간은 머신 부하에
        // 좌우된다. 로드 400 에서 5초를 넘겨 자리표시자로 떨어졌고, 그게 곧 "성공했다"로
        // 읽혀 학생 스폰이 통째로 샜다(거노 실사고 2026-08-05). 한가할 때 실측 0.06초라
        // 정상 경로에서 이 값이 체감되는 일은 없다.
        let id = match rx.recv_timeout(std::time::Duration::from_secs(20)) {
            Ok(Ok(id)) if !id.is_empty() => id,
            // **성공 봉투에 자리표시자를 싣지 않는다.** 못 만들었으면 못 만들었다고
            // 답해야 호출자가 재시도·중단을 고를 수 있다.
            Ok(Ok(_)) => anyhow::bail!("split 이 빈 pane id 를 돌려줬다"),
            Ok(Err(why)) => anyhow::bail!("split 실패: {why}"),
            Err(_) => anyhow::bail!(
                "split 응답 없음(20초) — GUI 스레드가 막혀 있다. 머신 부하를 확인해라"
            ),
        };
        Ok(SurfaceInfo {
            id,
            workspace_id: FIXED_WORKSPACE_ID.into(),
            title: None,
            cwd: None,
            character: None,
        })
    }

    /// 셰임(`teammate_case_arms`/`install_claude_hook_shim`)이 조립하는 것과 **같은
    /// 규칙**으로 이름을 미리 짓는다: `<학생 슬러그>-p<pane 번호>` + cwd 기준 팀.
    /// 규칙이 갈리면 부른 쪽이 닿지 않는 인박스에 브리프를 넣고도 성공으로 읽으므로,
    /// 셰임 쪽을 고칠 땐 여기도 같이 고쳐야 한다.
    fn closed_panes(&self, discard: Option<&str>) -> anyhow::Result<serde_json::Value> {
        // `closed_panes` 는 App 필드라 이 스레드에서 직접 못 읽는다 — split 과 같은
        // 회신 채널 패턴으로 GUI 스레드에 물어본다.
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = self
            .proxy
            .send_event(UserEvent::SocketClosedPanes(discard.map(str::to_string), tx));
        match rx.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(why)) => anyhow::bail!("{why}"),
            Err(_) => anyhow::bail!("되살리기 목록 응답 없음(10초) — GUI 스레드가 막혀 있다"),
        }
    }
    fn pane_agent(&self, surface_id: &str) -> Option<(String, String)> {
        let name = self.ws.lock().unwrap().pane_character.get(surface_id).cloned()?;
        let slug = crate::theme::agent_slug(&name);
        let cwd = self
            .query_pane_pids()
            .into_iter()
            .find(|(p, _)| p == surface_id)
            .and_then(|(_, pid)| self.pane_cwd_live(pid))?;
        let team = kasa_mcp::team::team_name_for(&kasa_mcp::character::mode_slug(&cwd));
        // 셰임은 팀명이 비면 트리플을 통째로 생략한다 — 그때는 이름도 안 생긴다.
        if team.is_empty() {
            return None;
        }
        Some((
            format!(
                "{slug}-p{}{}",
                surface_id.trim_start_matches('%'),
                crate::agent_name_suffix()
            ),
            team,
        ))
    }

    /// 모든 창 + 그 창의 pane 들. `move`(창 간 이동)를 쓰려면 **어느 창에 뭐가 있는지**
    /// 보여야 하는데, 이게 미구현이라 `kasaterm-cli windows` 가 늘 "(윈도우 없음)"을
    /// 냈다 — 이동 기능을 붙여 놓고 목적지를 못 찾는 상태였다.
    ///
    /// GUI RPC 없이 `ws.pane_window`(pane → 창 인덱스)로 짓는다. 그건 `publish_pty_layout`
    /// 이 **전 윈도우** leaf 를 채워 두는 미러라 socket 스레드에서 그대로 읽힌다
    /// (App 의 `windows`/`pty_layout` 은 GUI 스레드 소유라 여기서 못 본다).
    ///
    /// rect 는 **활성 창만** 채운다 — ws 에 실리는 layout 트리가 활성 창 하나뿐이다.
    /// 비활성 창은 pane 목록만 준다(이동 대상을 고르는 데는 그걸로 충분하다).
    fn windows_overview(&self) -> Result<Vec<kasa_socket::backend::WindowOverview>> {
        // ws 를 잠그기 **전에** 부른다 — std Mutex 는 재진입이 안 돼 안에서 부르면 멈춘다.
        let active_rects = self.window_layout().unwrap_or_default();
        let ws = self.ws.lock().unwrap();
        let mut by_win: std::collections::BTreeMap<usize, Vec<String>> = Default::default();
        for (pane, idx) in &ws.pane_window {
            by_win.entry(*idx).or_default().push(pane.clone());
        }
        // 활성 창 = 활성 leaf 집합의 아무 pane 이 속한 창.
        let active_idx = ws
            .active_window_panes
            .iter()
            .find_map(|p| ws.pane_window.get(p))
            .copied();
        drop(ws);
        Ok(by_win
            .into_iter()
            .map(|(idx, mut surfaces)| {
                surfaces.sort();
                let active = Some(idx) == active_idx;
                kasa_socket::backend::WindowOverview {
                    idx,
                    active,
                    panes: if active { active_rects.clone() } else { Vec::new() },
                    surfaces,
                }
            })
            .collect())
    }

    fn new_window(&self) -> Result<()> {
        // 창 생성은 회신할 게 없다(창 인덱스는 `windows` 로 읽는다) — 이벤트만 던진다.
        let _ = self.proxy.send_event(UserEvent::SocketNewWindow);
        Ok(())
    }

    /// 부른 pane **안에 새 탭**. 쪼개지 않으므로 화면이 안 줄어든다 — 학생을 하나 더
    /// 띄울 때마다 split 하면 네 번째쯤에서 다 종잇장이 된다(거노 2026-08-05).
    fn new_tab(&self, outer: Option<&str>) -> Result<SurfaceInfo> {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = self.proxy.send_event(UserEvent::SocketNewTab(
            outer.map(str::to_string),
            tx,
        ));
        // 타임아웃·자리표시자 정책은 split 과 같다 — 못 만들었으면 못 만들었다고
        // 답해야 호출자가 재시도를 고를 수 있다.
        let id = match rx.recv_timeout(std::time::Duration::from_secs(20)) {
            Ok(Ok(id)) if !id.is_empty() => id,
            Ok(Ok(_)) => anyhow::bail!("new_tab 이 빈 pane id 를 돌려줬다"),
            Ok(Err(why)) => anyhow::bail!("탭 생성 실패: {why}"),
            Err(_) => anyhow::bail!(
                "탭 생성 응답 없음(20초) — GUI 스레드가 막혀 있다. 머신 부하를 확인해라"
            ),
        };
        Ok(SurfaceInfo {
            id,
            workspace_id: FIXED_WORKSPACE_ID.into(),
            title: None,
            cwd: None,
            character: None,
        })
    }

    /// pane 을 다른 pane 옆으로 — **대상이 다른 창이면 창을 건너뛴다.** PTY 는 안
    /// 죽고 레이아웃 트리만 옮겨 붙는다(GUI 의 사이드바 드롭과 같은 경로).
    fn move_surface(
        &self,
        surface_id: &str,
        target: &str,
        direction: SplitDirection,
    ) -> Result<()> {
        let zone = match direction {
            SplitDirection::Left => crate::DropZone::Left,
            SplitDirection::Right => crate::DropZone::Right,
            SplitDirection::Up => crate::DropZone::Up,
            SplitDirection::Down => crate::DropZone::Down,
            // 놓을 방향을 안 정했으면 오른쪽 — 창이 대개 가로로 넓다. split 처럼
            // 종횡비로 고르지 않는 이유: 여기선 "어디에 붙일지"가 사용자 의도라
            // 자동으로 뒤집으면 놓인 자리가 예측이 안 된다.
            SplitDirection::Auto => crate::DropZone::Right,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = self.proxy.send_event(UserEvent::SocketMovePane(
            surface_id.to_string(),
            target.to_string(),
            zone,
            tx,
        ));
        match rx.recv_timeout(std::time::Duration::from_secs(20)) {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(why)) => anyhow::bail!("이동 실패: {why}"),
            Err(_) => anyhow::bail!("이동 응답 없음(20초) — GUI 스레드가 막혀 있다"),
        }
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

    fn capture_surface(
        &self,
        surface_id: &str,
        path: Option<&str>,
        max_width: u32,
    ) -> Result<serde_json::Value> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.proxy
            .send_event(UserEvent::SocketCapture(
                surface_id.to_string(),
                path.map(|s| s.to_string()),
                max_width,
                tx,
            ))
            .map_err(|_| anyhow::anyhow!("gui event loop is gone"))?;
        // GUI 가 이벤트를 받아 한 프레임을 그리고 리드백까지 마쳐야 답이 온다. 창이
        // 다른 창 뒤에 있거나 리사이즈 중이면 그 프레임이 늦으므로 넉넉히 준다 —
        // 무한 대기는 안 된다(소켓 워커가 물려 다른 명령까지 멈춘다).
        match rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => anyhow::bail!("{e}"),
            Err(_) => anyhow::bail!("capture timed out (window may be minimized)"),
        }
    }

    fn send_text(&self, surface_id: Option<&str>, text: &str) -> Result<()> {
        // 대상 surface 가 지정됐는데 현재 없는 pane 이면 거부 — 재시작·종료로 사라진 학생에게
        // tell 이 검증 없이 ok 만 받고 조용히 사라지던 오발송을 막는다(거노). 보낸 쪽이 ok:false
        // 로 즉시 알아 떠맡기/--resume 을 결정한다. None(focused)은 항상 통과.
        if let Some(sid) = surface_id {
            // pane 뿐 아니라 **탭 pid** 도 유효한 대상이다(`surface.new_tab` 이 주는 id).
            // 탭은 `ws.panes` 가 아니라 `pid_to_pane` 에 등록되므로 panes 만 보면 방금
            // 만든 탭이 "없는 pane" 으로 거절된다 — 만들어 놓고 아무것도 못 보내니
            // 기능이 통째로 무의미했다. 배달 경로(`pty_for_pane`)는 원래 탭을 찾는다.
            let ws = self.ws.lock().unwrap();
            let known = ws.panes.contains_key(sid) || ws.pid_to_pane.contains_key(sid);
            drop(ws);
            if !known {
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

    /// 살아 있는 토큰을 그대로 읽는다 — atomic 슬롯 로드뿐이라 GUI 스레드에
    /// 위임(`EventLoopProxy`)할 필요가 없다. `App` 상태를 안 만지는 몇 안 되는
    /// 창구다.
    fn design_tokens(&self) -> serde_json::Value {
        crate::theme::tokens_json()
    }

    /// 테마 카드 목록은 `theme_rows()` 를 그대로 쓴다 — **캐시된 함수**라서 매
    /// 요청에 79명치 theme.json 을 다시 파싱하지 않는다(네이티브 화면이 이미
    /// 같은 이유로 이걸 쓴다). 미리보기 얼굴은 경로가 아니라 slug 만 넘긴다:
    /// 파일 경로를 웹에 흘리면 그게 곧 임의 파일 읽기 창구가 된다.
    fn settings_characters(&self) -> serde_json::Value {
        let themes: Vec<serde_json::Value> = theme_rows()
            .into_iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.id,
                    "label": r.label,
                    "count": r.count,
                    "faces": r.faces.into_iter().map(|(slug, _)| slug).collect::<Vec<_>>(),
                })
            })
            .collect();
        // characters_json 은 활성 테마의 theme.json 을 최우선으로 본다 — 그래서
        // 테마를 고르면 이 목록도 함께 바뀐다.
        let roster = kasa_mcp::character::characters_json()
            .as_ref()
            .map(roster_entries)
            .unwrap_or_default();
        serde_json::json!({
            "active_theme": kasa_mcp::character::active_theme_id(),
            // persona 토글은 로스터와 같은 화면에 있다 — 상태를 안 실으면 웹이
            // 토글을 항상 켜진 모양으로 그려 화면이 거짓말을 한다.
            "persona_enabled": read_claude_persona(),
            "themes": themes,
            "roster": roster,
        })
    }

    fn character_face(&self, slug: &str, theme: Option<&str>) -> Option<Vec<u8>> {
        if !safe_path_component(slug) {
            return None;
        }
        let file = format!("{slug}-profile.png");
        // 테마를 지정했으면 그 폴더 안에서만 찾는다. 없으면 404 로 두고 번들로
        // 떨어지지 않는다 — 카드는 "이 테마의 얼굴"을 보이는 자리라, 폴백하면
        // 그 테마에 없는 그림이 그 테마 것처럼 보인다.
        if let Some(id) = theme.filter(|s| !s.is_empty()) {
            if !safe_path_component(id) {
                return None;
            }
            let root = kasa_mcp::character::themes_root()?;
            return read_file_under(&root, &root.join(id).join("sprites").join(&file));
        }
        // 활성 스프라이트 폴더(테마의 sprites/ 또는 ~/.config/kasaterm/students/)가
        // 번들을 덮어쓴다 — 네이티브 로더와 같은 순서다(render.rs `user_asset_rgba`
        // 우선). 순서가 뒤집히면 사용자가 넣은 그림이 무시된다.
        if let Some(dir) = students_dir() {
            if let Some(b) = read_file_under(&dir, &dir.join(&file)) {
                return Some(b);
            }
        }
        // 번들 PNG 는 **이미 바이너리에 있는 것을 재사용**한다. 여기서 다시
        // include_bytes! 하면 79장이 두 번 들어가 바이너리가 그만큼 커진다.
        crate::render::student_profile_png(slug).map(|b| b.to_vec())
    }

    fn save_character(
        &self,
        name: &str,
        persona: Option<&str>,
        new_name: Option<&str>,
    ) -> Result<serde_json::Value> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.proxy
            .send_event(UserEvent::SocketSaveCharacter(
                name.to_string(),
                persona.map(str::to_string),
                new_name.map(str::to_string),
                tx,
            ))
            .map_err(|_| anyhow::anyhow!("gui event loop is gone"))?;
        // 파일 두 번 쓰기(성격·이름)와 shim 재생성이 끝나야 답이 온다 — 밀리초
        // 단위지만 GUI 가 프레임을 그리는 중이면 그 뒤로 밀린다. 무한 대기는 안
        // 된다(소켓 워커가 물려 다른 명령까지 멈춘다).
        match rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => anyhow::bail!("{e}"),
            Err(_) => anyhow::bail!("저장이 시간 안에 안 끝났어요"),
        }
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
        //
        // codex 는 `rollout-<ts>-<uuid>.jsonl` 이라 stem 이 sid 가 아니다 — 그대로 쓰면
        // `rollout-2026-…-019f…` 가 세션 id 로 박혀 캐릭터 조회도 재시작 이어가기도 전부
        // 빗나간다. 파일명이 rollout 꼴이면 거기서 uuid 를 떼어 쓴다.
        let p = std::path::Path::new(path);
        let sid = codex_sid_from_rollout(p)
            .or_else(|| p.file_stem().and_then(|s| s.to_str()).map(str::to_string));
        if let Some(sid) = sid {
            let _ = self
                .proxy
                .send_event(UserEvent::SocketSessionBound(surface_id.to_string(), sid));
        }
        Ok(())
    }

    fn peek(&self, surface_id: &str, lines: usize) -> Result<String> {
        let ws = self.ws.lock().unwrap();
        let key = ws.outer_for_pty(surface_id).unwrap_or_else(|| surface_id.to_string());
        let pane = ws
            .panes
            .get(&key)
            .ok_or_else(|| anyhow::anyhow!("no such pane: {surface_id}"))?;
        Ok(pane.tab_for_pid(surface_id).visible_text(lines))
    }

    /// pane 을 스크롤백 안에서 움직인다 — 휠과 **같은 경로**(alacritty display_offset).
    ///
    /// 이게 없어서 스크롤 문제를 화면 밖에서 재현할 방법이 아예 없었다(트레이트에는
    /// 정의돼 있는데 GUI 가 구현을 안 해 늘 unsupported 였다). `peek` 은 스크롤 위치와
    /// 무관하게 라이브 화면만 읽으므로 이 둘을 짝지어야 「올려도 안 보인다」를 잰다.
    ///
    /// 부호는 트레이트 약속대로 **음수가 과거**다. `PtySession::scroll` 은 반대 규약
    /// (양수가 과거)이라 여기서 뒤집는다.
    fn scroll_surface(&self, surface_id: &str, lines: i32) -> Result<()> {
        let key = {
            let ws = self.ws.lock().unwrap();
            ws.outer_for_pty(surface_id).unwrap_or_else(|| surface_id.to_string())
        };
        let sess = kasa_pty::lookup_session(surface_id)
            .or_else(|| kasa_pty::lookup_session(&key))
            .ok_or_else(|| anyhow::anyhow!("no live pty for pane: {surface_id}"))?;
        sess.scroll(-lines);
        Ok(())
    }

    fn peek_ansi(&self, surface_id: &str, lines: usize) -> Result<String> {
        let ws = self.ws.lock().unwrap();
        let key = ws.outer_for_pty(surface_id).unwrap_or_else(|| surface_id.to_string());
        let pane = ws
            .panes
            .get(&key)
            .ok_or_else(|| anyhow::anyhow!("no such pane: {surface_id}"))?;
        Ok(pane.tab_for_pid(surface_id).visible_text_ansi(lines))
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
        let live = self.live_surfaces();
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
        // ⚠️ KASATERM_* 은 **pane 셸에 없다** — shim 이 claude 를 띄우며 그 프로세스에만
        // 실어 준다(실측: `/bin/zsh -il` env 엔 하나도 없고 자식 claude 엔 전부 있다).
        // 그래서 셸이 아니라 자식 claude 를 찾아 읽는다. AGENT/TEAM 이 있는 pane 은
        // 인박스를 폴링하므로, 말을 걸 때 입력창이 아니라 인박스를 써야 한다.
        // pane 당 ps 한 번 — 키마다 부르면 폴링마다 pane×키 개의 ps 가 뜬다.
        let ptable = kasa_pty::process_table_shared();
        // cross-session 명부 — 이 pane 에 SendMessage 가 닿는지 판정한다. 보내 보고
        // 아는 수밖에 없던 자리인데, 그 성공 응답이 도달을 증명하지 않아 늘 추측이었다.
        // 살아있는 pid 는 위 ptable 을 그대로 쓴다(명부에 소켓 파일이 남아 있어도
        // 프로세스가 죽었으면 안 닿는다 — 파일만 보면 그걸 못 가른다).
        let peers = kasa_socket::peers::by_session_id();
        let live_pids: HashSet<u32> = ptable.iter().map(|(pid, _, _)| *pid).collect();
        let pane_env: HashMap<String, HashMap<String, String>> = pane_shell_pid
            .iter()
            .filter_map(|(sid, &shell)| {
                let pid = claude_under(&ptable, shell)?;
                let vars = kasa_pty::process_env_vars(
                    pid,
                    &[
                        "KASATERM_SESSION_ID",
                        "KASATERM_CHARACTER",
                        "KASATERM_AGENT",
                        "KASATERM_TEAM",
                    ],
                );
                Some((sid.clone(), vars))
            })
            .collect();
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
        // 훅이 보고한 in-flight — 아래에서 꼬리 판정 위에 얹는다. 루프 밖에서 한 번만
        // 뜬다(pane 마다 잠그면 board 한 번에 락을 pane 수만큼 잡는다).
        let hook_act = self.hook_activity.lock().unwrap().clone();
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
                // 훅이 본 것을 얹는다. 512KB 도 충분히 큰 세션에선 밀려나는데(24MB 짜리가
                // 실재한다), 훅은 그 순간 오므로 파일 크기와 무관하다. 꼬리를 지우지 않고
                // **합치는** 이유: 훅은 앱이 뜬 뒤에 시작한 것만 알아서, 그 전부터 돌던
                // 작업은 꼬리에만 있다. 둘 중 하나라도 보면 도는 것이다.
                if let Some(h) = hook_act.get(sid.as_str()) {
                    for l in crate::state::HookActivity::labels(&h.subagents) {
                        if !row.subagents.contains(&l) {
                            row.subagents.push(l);
                        }
                    }
                    for l in crate::state::HookActivity::labels(&h.background) {
                        if !row.background.contains(&l) {
                            row.background.push(l);
                        }
                    }
                }
                row.window_idx = pane_window.get(sid.as_str()).copied().unwrap_or(0);
                // codex 는 model 이 위 창 밖이라(파일 앞 87~122KB 의 `turn_context`)
                // 머리를 한 번 읽어 채운다. rollout 파일일 때만 — claude 는 부팅
                // 직후 잠깐 빌 뿐 곧 tail 에서 잡히니 여기서 읽으면 헛일이다.
                if row.model.is_empty() && codex_sid_from_rollout(path).is_some() {
                    const HEAD: u64 = 384 * 1024;
                    let key = path.to_string_lossy().into_owned();
                    let hit = self.codex_cfg.lock().unwrap().get(&key).cloned();
                    let cfg = match hit {
                        Some(c) => c,
                        None => {
                            let head = read_head(path, HEAD);
                            let c = crate::transcript::codex_cfg_from_head(&head);
                            // 찾았거나, 머리를 꽉 읽고도 없을 때만 굳힌다. 후자는
                            // 지시문이 우리 창보다 크다는 뜻이라 다시 읽어도 같은 답이다.
                            // 더 짧게 읽혔으면 파일을 통째로 본 것 — 아직 첫 턴을 안
                            // 썼을 뿐이라 굳히지 않고 다음 폴링에 다시 본다.
                            if !c.0.is_empty() || head.len() as u64 >= HEAD {
                                self.codex_cfg.lock().unwrap().insert(key, c.clone());
                            }
                            c
                        }
                    };
                    row.model.clone_from(&cfg.0);
                    // 저장 경로가 맵 하나만 보게 여기서 합류시킨다 — claude 는
                    // statusline 이, codex 는 이 자리가 같은 맵을 채운다.
                    if !cfg.0.is_empty() || !cfg.1.is_empty() {
                        self.reported_agent_cfg.lock().unwrap().insert(sid.clone(), cfg);
                    }
                }
                // Prefer claude's official status when it reports this session
                // (matched by transcript filename stem == sessionId). The
                // mtime heuristic above is only a fallback for sessions claude
                // doesn't list. `effectively_idle` then drives the attention
                // (permission-prompt) override below.
                let stem = path.file_stem().and_then(|s| s.to_str());
                let official = stem.and_then(|s| agents.get(s)).map(|s| s.as_str());
                // 같은 stem(=sessionId)으로 명부를 조회한다. **이름으로 잇지 않는 게
                // 핵심이다** — 명부의 name 은 /rename 으로 바뀌고(pane 은
                // arisu-p116 인데 명부엔 "agy code") 같은 캐릭터가 여러 pane 에 뜨면
                // 겹친다. sessionId 만 안 흔들린다.
                let peer = stem.and_then(|s| peers.get(s));
                let peer_alive = peer.map(|p| live_pids.contains(&p.pid)).unwrap_or(false);
                row.reach = kasa_socket::peers::reach_of(peer, peer_alive).as_str().to_string();
                row.peer_name = peer.map(|p| p.name.clone()).filter(|n| !n.is_empty());
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
                // 명시적 완료 보고 부착 — idle 을 한 번 지난 보고가 다시 working 이
                // 되면 새 브리프를 받은 것이므로 소거한다(스테일 방지). 그 전까지는
                // working 중에도 싣는다: 보고 시점엔 아직 자기 턴이 안 끝났는데
                // 그때 숨기면 "보고 즉시 표시"라는 명시 보고의 이점이 죽는다.
                {
                    let mut reports = self.done_reports.lock().unwrap();
                    if let Some(rep) = reports.get_mut(sid.as_str()) {
                        if row.status == "working" && rep.idle_seen {
                            reports.remove(sid.as_str());
                        } else {
                            if row.status != "working" {
                                rep.idle_seen = true;
                            }
                            row.done_outcome = Some(rep.outcome.clone());
                            row.done_summary =
                                (!rep.summary.is_empty()).then(|| rep.summary.clone());
                            row.done_ago_secs = Some(rep.at.elapsed().as_secs());
                        }
                    }
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
                // 렌더(render.rs)와 같은 소스라 board·탭 캐릭터가 항상 일치(거노:
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
                // 셸 env 폴백(foreground 순정 경로) — bg 셸엔 대개 없다. 단 spawn 시
                // 동결된 KASATERM_CHARACTER 는 --resume/재배정 후 stale 하다(거노: 복원
                // 후 board 가 전부 미도리 — env CHARACTER 는 미도리로 굳었지만 pane env 의
                // SESSION_ID 가 가리키는 실제 세션 bind 는 각자 아루·히마리·아리스였다).
                // 그래서 SESSION_ID 의 세션 bind 를 먼저(신선) 조회하고, 없을 때만 동결
                // CHARACTER 로 폴백한다.
                let env = pane_env.get(sid.as_str());
                let env_char = env
                    .and_then(|e| {
                        e.get("KASATERM_SESSION_ID")
                            .and_then(|s| kasa_mcp::character::session_character(s))
                            .or_else(|| e.get("KASATERM_CHARACTER").cloned())
                    })
                    .filter(|c| valid_members.contains(c));
                // 둘 다 있을 때만 인박스 경로가 성립한다 — 한쪽만으론 파일 경로가 안 나온다.
                if let (Some(a), Some(t)) =
                    (env.and_then(|e| e.get("KASATERM_AGENT")), env.and_then(|e| e.get("KASATERM_TEAM")))
                {
                    row.agent_name = Some(a.clone());
                    row.team = Some(t.clone());
                }
                // 어느 하네스인지 — codex 는 인박스가 없어 위 두 칸이 영영 비고,
                // 그것만으론 "트리플 없이 뜬 claude" 와 구별이 안 된다. 종류를 밝혀야
                // 오케스트레이터가 SendMessage 대신 tell 을 고른다.
                row.harness = pane_shell_pid
                    .get(sid.as_str())
                    .and_then(|&pid| kasa_pty::agent_for_shell(&ptable, pid))
                    .map(|k| k.as_str().to_string());
                row.character = retained
                    .clone()
                    .or(env_char)
                    .or_else(|| pane_character.get(sid.as_str()).cloned())
                    .or_else(|| {
                        std::fs::read_to_string(kasa_socket::collab_root().join(format!(
                            "{rslug}/character-{}",
                            sid.trim_start_matches('%')
                        )))
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
                    // respawn/새 세션(claude 가 --resume 없이 새 sid 발급)은 pane 셸 env 의
                    // SESSION_ID(스폰·swap 이 박은 원본 anchor)가 가리키는 학생을 상속한다 —
                    // 안 하면 lazy 빈슬롯 배정이 미도리로 오배정돼 retained 가 오염됐다(거노:
                    // swap 후 %3 이 매 턴 새 세션을 발급하며 계속 미도리로 뭉침). apply_session_
                    // character 의 anchored 경로와 동일 규칙.
                    let anchored = pane_shell_pid
                        .get(sid.as_str())
                        .and_then(|&pid| kasa_pty::process_env_var(pid, "KASATERM_SESSION_ID"))
                        .and_then(|es| kasa_mcp::character::session_character(&es))
                        .filter(|c| valid_members.contains(c));
                    let name = anchored.or(inherited).or(own).or_else(|| {
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
        self.done_reports.lock().unwrap().retain(|sid, _| live.contains(sid.as_str()));
        // 훅 상태도 같이 — pane 이 닫히면 `end` 훅은 영영 안 온다. 안 걷으면 죽은
        // 자리의 작업이 계속 도는 것처럼 남는다.
        self.hook_activity
            .lock()
            .unwrap()
            .retain(|sid, _| live.contains(sid.as_str()));
        // 학생 경로(cwd)를 PTY 셸 pid 의 라이브 cwd 로 덮어쓴다 — transcript 가 stale
        // 하거나(claude 가 jsonl 미기록) cd 직후라도 즉시 반영(2s 캐시). 아래 git
        // 브랜치도 이 라이브 cwd 기준이 되도록 branch 조회 전에 한다.
        let pane_pids: HashMap<String, u32> = self.query_pane_pids().into_iter().collect();
        // 컨텍스트 % — claude TUI 상태바에서 파싱(transcript 토큰이 0 이어도 robust).
        // 화면 스냅샷은 in-memory(visible_text)라 싸다 — 락 짧게.
        // 화면 스냅샷 + OSC title 을 한 락에서. title 은 board row 라벨을 터미널 탭
        // 렌더(render.rs)와 같은 소스(OSC title)로 통일 — 양쪽 "미도리 · 작업명".
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
            // OSC title 은 claude 작업 중 "⠂ 제목" 꼴로 스피너 글리프가 붙는다 —
            // board 라벨(웹뷰 "학생 · 작업명")에 새지 않게 벗겨서 싣는다.
            // ⚠️OSC 제목이 **없을 때 빈 값으로 덮지 않는다.** 예전엔
            // `unwrap_or_default()` 라, 터미널 제목을 안 다는 하네스(agy 는 TUI 라
            // 안 단다)는 파서가 전사본에서 뽑아 온 제목까지 통째로 지워져 board 행이
            // 늘 무제목이었다. OSC 가 있으면 그쪽이 여전히 이긴다 — 살아있는 값이라서다.
            if let Some(t) = osc_titles.get(&row.surface_id) {
                row.title = crate::strip_activity_prefix(t).to_string();
            }
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
            // 컨텍스트 창 — statusLine 이 보고한 하네스 정본이 최우선. transcript 의 model
            // 엔 `[1m]` 이 안 실리고(API 응답이 `claude-opus-5`) 상태바 모델명도 좁은 pane
            // 대비로 "(1M context)" 괄호가 잘려 나가, 추정 3종이 모두 빗나가면 1M 세션이
            // 200k 로 잡혔다 — 18만 토큰이 92%(빨강)로 보이다 200k 를 넘는 순간 20% 로
            // 떨어지는 역주행의 원인. 보고가 없을 때만 종전 상태바 폴백을 쓴다.
            let reported = self
                .reported_ctx
                .lock()
                .unwrap()
                .get(&row.surface_id)
                .copied();
            if let Some((win, tok)) = reported {
                row.context_limit = win;
                // transcript usage 가 tail 윈도 밖이라 0 이면 보고된 토큰으로 메운다.
                if row.context_tokens == 0 {
                    row.context_tokens = tok;
                }
            } else if row.model.to_ascii_lowercase().contains("1m") && row.context_limit < 1_000_000
            {
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

    fn pane_done(&self, surface_id: &str, outcome: &str, summary: &str) -> Result<()> {
        // 보고만 기록 — 표시는 board 빌더가, 데스크톱 알림은 어차피 그 턴 끝의
        // Stop 훅(notify)이 한다. notify 가 attention 처럼 이 맵을 지우면 안 된다:
        // done 직후 같은 턴 끝에 notify 가 와서 보고가 보이기도 전에 죽는다.
        self.done_reports.lock().unwrap().insert(
            surface_id.to_string(),
            DoneReport {
                outcome: outcome.to_string(),
                summary: summary.to_string(),
                at: std::time::Instant::now(),
                idle_seen: false,
            },
        );
        Ok(())
    }

    fn agent_status(
        &self,
        surface_id: &str,
        phase: &str,
        kind: &str,
        key: &str,
        label: &str,
    ) -> Result<()> {
        let mut map = self.hook_activity.lock().unwrap();
        let entry = map.entry(surface_id.to_string()).or_default();
        entry.apply(phase, kind, key, label);
        if entry.is_empty() {
            map.remove(surface_id);
        }
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

pub(crate) fn read_tail(path: &std::path::Path, max_bytes: u64) -> (String, bool) {
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

/// 파일 **머리** `max_bytes`. `read_tail` 의 짝이다 — 알고 싶은 값이 파일 앞에만
/// 있는 로그(codex 의 `turn_context`)를 위해서다. 반환이 `max_bytes` 보다 짧으면
/// 파일을 통째로 본 것이라, 호출부가 "아직 안 쓰였다"와 "우리 창 밖이다"를 가른다.
pub(crate) fn read_head(path: &std::path::Path, max_bytes: u64) -> String {
    use std::io::Read;
    let Ok(f) = std::fs::File::open(path) else { return String::new() };
    let mut buf = Vec::new();
    let _ = f.take(max_bytes).read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
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
/// ⚠️ 이쪽은 런처(node·npx)를 지나 내려가지 **않는다**. 여기 쓰임은 "셸이냐
/// 아니냐" 하나뿐이라 이름이 `node` 로 나와도 목적을 이루기 때문이다. 사용자에게
/// 보여줄 정확한 프로그램 이름이 필요하면 kasa-pty 의 `active_process_name`
/// (런처를 만나면 자식으로 내려간다)을 써라 — 같은 일을 하는 코드가 둘이라는
/// 사실 자체가 함정이므로, 고칠 일이 생기면 양쪽을 같이 볼 것.
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

/// 프로세스의 현재 작업 디렉터리 — libproc 에 직접 묻는다.
///
/// 오래 `lsof -d cwd` 를 fork 했고 "git 패널이 초당 한 번 부르니 서브프로세스
/// 값은 감당된다"고 적혀 있었는데, 그 전제가 깨진 지 오래다. 지금은 렌더가
/// pane 마다 **매 프레임** 부른다(`smart_pane_label`). fork+exec 한 번이 그
/// 자리에서 수십 ms 라, 마크다운 스크롤 프레임 간격 32ms 중 메인 스레드 샘플의
/// 절반이 이 함수였다(아리스 제보 → sample 로 확인). 같은 답을 syscall 하나로
/// 얻을 수 있다 — Windows 가 PEB 를 직접 읽는 것과 같은 결이다.
#[cfg(target_os = "macos")]
pub(crate) fn pid_cwd(pid: u32) -> Option<std::path::PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let want = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    // 남의 프로세스는 같은 uid 일 때만 답한다(lsof 도 마찬가지였다) — 실패는 None.
    let got = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            std::ptr::addr_of_mut!(info).cast(),
            want,
        )
    };
    if got < want {
        return None;
    }
    // libc 가 낡은 rustc 호환 때문에 1024바이트 경로를 [[c_char; 32]; 32] 로
    // 쪼개 뒀다 — 실제 메모리는 평면이라 그대로 편다.
    let path = unsafe {
        std::slice::from_raw_parts(info.pvi_cdir.vip_path.as_ptr().cast::<u8>(), 32 * 32)
    };
    let end = path.iter().position(|&b| b == 0).unwrap_or(path.len());
    (end > 0).then(|| std::path::PathBuf::from(std::ffi::OsString::from_vec(path[..end].to_vec())))
}

/// Resolve a process's current working directory via lsof (non-macOS unix).
#[cfg(all(unix, not(target_os = "macos")))]
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
#[cfg(all(unix, not(target_os = "macos")))]
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
/// PEB 의 cwd 는 늘 구분자로 끝난다(`C:\Users\x\`). 그대로 흘리면 홈 접기가 `~`
/// 대신 `~\` 를 만들어 빵부스러기·상태바·pane 라벨에 그대로 새고, 저장된 세션
/// 경로도 다른 경로에서 온 같은 위치와 문자열 비교가 어긋난다. 드라이브 루트
/// (`C:\`)만은 구분자를 떼면 "그 드라이브의 현재 폴더"라는 **상대경로**가 되니
/// 남긴다.
#[cfg(windows)]
fn trim_trailing_sep(s: &str) -> &str {
    let t = s.trim_end_matches(['\\', '/']);
    if t.len() < 3 { s } else { t }
}

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
            Some(std::path::PathBuf::from(trim_trailing_sep(&s.to_string_lossy())))
        })();
        CloseHandle(handle);
        result
    }
}


/// Build one layout-tree leaf's restore record from a live PtySession: its
/// cwd, **which harness** it was running (`was_agent`: "claude"|"codex"|null),
/// and that claude's session id (for `claude --resume`). `cwd` is null when the
/// shell pid/cwd can't be resolved — restore then falls back to the default cwd.
pub fn pane_record(sess: &kasa_pty::PtySession) -> serde_json::Value {
    let shell_pid = sess.shell_pid();
    let cwd = shell_pid.and_then(pid_cwd);
    // 어떤 하네스로 돌던 pane 인지 **종류**를 남긴다. 예전엔 bool 하나(`was_claude`)
    // 라서 codex pane 은 재시작하면 셸로 돌아왔다 — 무엇이었는지 기록이 없으니
    // 되살릴 수가 없었다. 판정은 state.rs 의 `active_agent`(런처 한 세대 하강 포함).
    let agent = sess.active_agent();
    let was_agent = agent.map(|k| k.as_str());
    // Only record a session id for panes actually running claude, straight off
    // the running claude's argv (exact per-pane). The cwd-mtime fallback that
    // used to fill argv-less `claude` panes is gone — it collapsed every pane
    // sharing a cwd onto one session id (거노: 재시작 시 여러 pane 이 다 같은 대화+
    // 캐릭터로 뭉침). layout_to_json 이 pane_claude_sid(SocketSessionBound)로 정확한
    // per-pane 세션을 채우므로, pane_record 는 argv id 만 보고하고 없으면 None 을 둔다
    // (restore_leaf 가 fresh claude 로 복원).
    //
    // codex 는 여기서 세션 id 를 못 집는다 — argv 에 없고 rollout 파일명에만 있다(실측).
    // 대신 bind-transcript 훅이 보고한 값이 `pane_claude_sid` 에 들어와 `layout_to_json`
    // 이 그걸 정본으로 덮어쓴다(claude 와 같은 경로). 그래서 여기선 None 이 맞다.
    let session_id = if matches!(agent, Some(kasa_pty::AgentKind::Claude)) {
        shell_pid.and_then(claude_session_id_from_cmdline)
    } else {
        None
    };
    serde_json::json!({
        "cwd": cwd.as_ref().map(|c| c.to_string_lossy().into_owned()),
        "was_agent": was_agent,
        "session_id": session_id,
    })
}

/// Write the full multi-session restore state (built by the caller from each
/// session's layout tree). Written on exit, read by start_pty. Best-effort;
/// failures are silent.
/// Persist the restore snapshot **atomically** — temp file, flush, rename.
///
/// 이전엔 목적지에 곧바로 `create` + `write_all` 했다. 종료 시 한 번만 쓸 땐
/// 티가 안 났지만, 자동 저장(`autosave_session`)이 붙으면서 쓰는 도중에 강제
/// 종료당할 창이 생겼다 — 그러면 JSON 이 잘려 `read_session_state` 가 None 을
/// 내고 **복원 창이 아예 안 뜬다**(안 하느니만 못한 결과). rename(2) 은 같은
/// 파일시스템 안에서 원자적이라, 어느 순간에 죽어도 디스크엔 완전한 옛 파일
/// 아니면 완전한 새 파일만 남는다. rename 전 `sync_all` 은 정전 대비 —
/// 데이터가 아직 캐시에만 있는 채로 이름만 바뀌면 빈 파일이 정본이 된다.
pub fn write_session_state(state: &serde_json::Value) {
    use std::io::Write;
    let Some(path) = session_file_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("json.tmp");
    let Ok(mut f) = std::fs::File::create(&tmp) else { return };
    if f.write_all(state.to_string().as_bytes()).is_err() || f.sync_all().is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    drop(f);
    if std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Read the restore state written by `write_session_state`. `None` when the
/// file is absent or unparseable — the caller then boots a fresh session with
/// no restore prompt.
pub fn read_session_state() -> Option<serde_json::Value> {
    let path = session_file_path()?;
    let bytes = std::fs::read(&path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Discard the saved restore state (user chose "새로 시작"). Best-effort — a
/// missing file is already the desired end state.
pub fn clear_session_state() {
    if let Some(path) = session_file_path() {
        let _ = std::fs::remove_file(path);
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
    // HOME 직참조 금지 — Windows GUI(Explorer 실행)는 HOME 부재라 영속이 통째로 죽는다.
    Some(kasa_socket::home_dir()?.join(".config/kasaterm/session.json"))
}

fn window_size_path() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("KASATERM_WINDOW_FILE") {
        if !p.is_empty() {
            return Some(std::path::PathBuf::from(p));
        }
    }
    Some(kasa_socket::home_dir()?.join(".config/kasaterm/window.json"))
}

fn settings_file_path() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("KASATERM_SETTINGS_FILE") {
        if !p.is_empty() {
            return Some(std::path::PathBuf::from(p));
        }
    }
    Some(kasa_socket::home_dir()?.join(".config/kasaterm/settings.json"))
}

/// 지금 화면이 학생 그림을 찾는 폴더. `<slug>-profile.png` / `<slug>-<i>.png` /
/// `<slug>-walk-<i>.png` / `schale-logo.png` 를 여기 넣으면 번들 도트를 대체한다
/// (render.rs 로더). 파일이 없으면 로더가 `include_bytes!` 번들로 떨어지므로 빈
/// 폴더는 아무것도 바꾸지 않는다.
///
/// **테마를 골랐으면 그 테마의 `sprites/` 가 이 자리다** — 테마 팩에서 로스터와
/// 그림은 한 벌이라, 이름은 새 테마인데 그림은 옛 폴더에서 오면 짝이 어긋난다.
/// 테마를 안 고른 사용자는 종전대로 `~/.config/kasaterm/students/` 를 쓴다.
pub fn students_dir() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("KASATERM_STUDENTS_DIR") {
        if !p.is_empty() {
            return Some(std::path::PathBuf::from(p));
        }
    }
    if let Some(d) = kasa_mcp::character::active_theme_dir() {
        return Some(d.join("sprites"));
    }
    Some(kasa_socket::home_dir()?.join(".config/kasaterm/students"))
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

/// kasaterm 테마가 바뀌면 Claude Code 도 따라간다 — `~/.claude/settings.json`
/// 의 `theme` 를 새 배경의 밝기에 맞춰 고쳐 쓴다. Claude Code 는 설정 파일을
/// 감시하다 즉시 리로드하므로 **이미 떠 있는 세션도 그 자리에서** 바뀐다.
/// 파일을 안 쓰면 달리는 세션은 따라올 길이 없다: `theme: auto` 조차 배경
/// 질의(OSC 11)를 시작할 때 한 번만 하기 때문이다(2026-08-13 지적 — 라이트로
/// 바꿔도 안쪽 claude 는 어두운 채라 /theme 을 손으로 쳐야 했다).
///
/// - `-daltonized`·`-ansi` 변형은 밝기 절반만 갈아 끼운다 — 색약 배려·ANSI
///   고정은 사용자의 선택이라 지우면 안 된다.
/// - `custom:<슬러그>` 는 건드리지 않는다. 사용자가 직접 고른 전용 팔레트다.
/// - `auto` 는 명시값으로 **대체한다**: auto 의 목적(터미널 배경 따라가기)을
///   우리가 라이브로 대신 이뤄 주는 것이라, 시작 때 한 번 판별로 끝나는
///   원래 auto 보다 의도에 더 충실하다.
/// - 계정 슬롯은 자격증명 저장소만 가르므로(`CLAUDE_SECURESTORAGE_CONFIG_DIR`,
///   `claude_account_export_line` 참고) 설정은 모든 계정이 이 한 파일을 읽는다.
/// - 파일이 JSON 으로 안 읽히면 손대지 않는다 — 테마 하나 맞추자고 env·권한
///   설정이 든 파일을 날리는 것보다 안 바뀌는 쪽이 싸다.
/// - 스크래치 설정(`KASATERM_SETTINGS_FILE`)으로 뜬 헤드리스 리그에서는 아무
///   것도 안 한다: 리그는 HOME 을 공유해서, 리그의 테마 실험이 진짜 Claude
///   설정을 뒤집으면 안 된다. (`KASATERM_SOCKET_PATH` 는 가드로 못 쓴다 —
///   본 앱도 부팅하며 자기 env 에 export 한다.)
pub fn sync_claude_theme(light: bool) {
    if std::env::var_os("KASATERM_SETTINGS_FILE").is_some() {
        return;
    }
    let Some(path) = kasa_socket::home_dir().map(|h| h.join(".claude/settings.json")) else {
        return;
    };
    let mut obj = match std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
    {
        Some(serde_json::Value::Object(m)) => m,
        Some(_) => return,
        // 파일이 없으면 새로 만든다 — 키 하나짜리 파일도 Claude 는 잘 읽는다.
        None if !path.exists() => serde_json::Map::new(),
        None => return,
    };
    let cur = obj.get("theme").and_then(|x| x.as_str()).unwrap_or("dark");
    if cur.starts_with("custom:") {
        return;
    }
    let suffix = if cur.ends_with("-daltonized") {
        "-daltonized"
    } else if cur.ends_with("-ansi") {
        "-ansi"
    } else {
        ""
    };
    let desired = format!("{}{}", if light { "light" } else { "dark" }, suffix);
    if cur == desired {
        return;
    }
    obj.insert("theme".to_string(), serde_json::Value::String(desired));
    if let Ok(txt) = serde_json::to_string_pretty(&serde_json::Value::Object(obj)) {
        let _ = std::fs::write(&path, txt);
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

/// 터미널 커서 모양 — `"block"`(기본) · `"bar"`(Ghostty 식 세로선) · `"underline"`.
///
/// 모르는 값은 block 으로 떨어뜨린다. 설정 파일을 손으로 고치다 오타가 나도 커서가
/// 사라지지는 않아야 한다 — 커서가 없으면 어디를 치는지 알 수가 없다.
pub fn read_cursor_shape() -> String {
    match read_settings().get("cursor_shape").and_then(|x| x.as_str()) {
        Some("bar") => "bar".to_string(),
        Some("underline") => "underline".to_string(),
        _ => "block".to_string(),
    }
}

/// 터미널 셀 위에서 마우스 포인터 모양 — `"arrow"`(기본) · `"ibeam"`.
///
/// 터미널은 글자를 고르는 자리라 I-beam 이 맞다는 사람과, 화살표여야 클릭 대상이
/// 보인다는 사람이 갈린다. 텍스트 입력칸(파일트리 검색 등) 위 I-beam 은 이 설정과
/// 무관하게 늘 뜬다 — 거긴 정말 글자를 치는 자리다.
pub fn read_mouse_cursor() -> String {
    match read_settings().get("mouse_cursor").and_then(|x| x.as_str()) {
        Some("ibeam") => "ibeam".to_string(),
        _ => "arrow".to_string(),
    }
}

/// `bar`·`underline` 커서의 굵기(논리 px). block 은 셀을 통째로 채우므로 안 쓴다.
///
/// 1~6 으로 조인다. 0 이면 커서가 보이지 않고, 셀 폭(≈8.5px)을 넘기면 bar 가 block
/// 과 구분이 안 된다 — 어느 쪽도 「고를 수 있는 값」이 아니다.
pub fn read_cursor_thickness() -> f32 {
    read_settings()
        .get("cursor_thickness")
        .and_then(|x| x.as_f64())
        .map(|v| (v as f32).clamp(1.0, 6.0))
        .unwrap_or(2.0)
}

/// Base cell font size (logical px) from settings. Missing/invalid → the
/// built-in default. Clamped to the same sane range the stepper offers.
///
/// 기본값 16 → 13 (2026-07-27). 셀 치수를 주 폰트 metric 에서 뽑는데 주 폰트가
/// D2Coding(advance 0.500em · line 1.160em)에서 JetBrains Mono(0.600em · 1.320em)로
/// 바뀌어, 같은 16 에서 칸이 가로 20%·세로 14% 커졌다(거노: "전체적으로 폰트가
/// 커졌네"). 13 이면 0.600 × 13 = 7.8px 로 옛 0.500 × 16 = 8px 과 사실상 같다.
/// 폰트 크기 = em 픽셀이라는 의미는 그대로 두고 기본값만 새 폰트에 맞춘 것 —
/// 명시적으로 값을 저장해 둔 사용자는 자기 크기를 그대로 유지한다.
/// 설정을 지웠을 때 돌아오는 셀 폰트 크기. 설정 화면의 "되돌리기"도 같은 값을
/// 써야 해서 상수로 둔다 — 두 곳에 숫자를 적으면 한쪽만 바뀌어 되돌린 결과가
/// 기본값과 다른 자리에 선다.
pub const DEFAULT_FONT_SIZE: f32 = 13.0;

pub fn read_font_size() -> f32 {
    read_settings()
        .get("font_size")
        .and_then(|x| x.as_f64())
        .map(|v| (v as f32).clamp(9.0, 32.0))
        .unwrap_or(DEFAULT_FONT_SIZE)
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

/// Editor autosave quiet period in ms (`editor_autosave_ms`). 0 / missing =
/// off. Clamped to 200ms..60s: below that every keystroke is a disk write,
/// above it the setting stops being autosave in any useful sense.
pub fn read_editor_autosave() -> Option<std::time::Duration> {
    let ms = read_settings().get("editor_autosave_ms")?.as_u64()?;
    (ms > 0).then(|| std::time::Duration::from_millis(ms.clamp(200, 60_000)))
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

/// 폴더 이름으로 쓸 수 있게 다듬는다. 한글은 그대로 둔다 — macOS·Windows 모두
/// 유니코드 폴더명을 받고, 사용자가 붙인 이름이 Finder 에서 그대로 보이는 편이
/// `theme-3` 보다 낫다. 걷어내는 건 실제로 깨지는 것들뿐이다: 경로 구분자(하위
/// 폴더를 만들어 버린다) · 제어문자 · 앞뒤 공백과 점(`.` `..` 과 숨김 파일).
fn sanitize_theme_id(label: &str) -> String {
    let cleaned: String = label
        .chars()
        .map(|c| if c == '/' || c == '\\' || c == ':' || c.is_control() { ' ' } else { c })
        .collect();
    cleaned.trim().trim_matches('.').trim().to_string()
}

/// 새 테마를 만든다 — 지금 로스터와 그림을 그 폴더로 복제해 채운다.
///
/// 빈 껍데기를 만들지 않는 건 **본보기가 없으면 아무도 테마를 못 만들기**
/// 때문이다: 80명치 JSON 스키마와 파일명 규칙을 맨손으로 맞춰야 한다. 채워
/// 두면 고칠 것만 고치면 된다.
///
/// 만든 테마를 곧바로 활성화하지 않는다 — 내용이 지금 것과 같아서 켜 봤자
/// 아무것도 안 바뀐 것처럼 보이고, 그럼 "만들기가 실패했나" 로 읽힌다.
pub fn create_theme(label: &str) -> std::io::Result<std::path::PathBuf> {
    // 목록을 읽는 곳과 **같은 뿌리**여야 한다 — 여기만 따로 계산하면
    // `KASATERM_THEMES_DIR` 을 쓰는 사용자는 새 테마가 목록에 안 뜬다.
    let root = kasa_mcp::character::themes_root()
        .ok_or_else(|| std::io::Error::other("홈 폴더를 못 찾았다"))?;
    let base = match sanitize_theme_id(label) {
        s if s.is_empty() => "my-theme".to_string(),
        s => s,
    };
    // 이름이 겹치면 뒤에 번호를 붙인다 — 이미 만들어 편집 중인 테마를 덮어쓰는 건
    // 되돌릴 수 없다.
    let dir = (1..1000)
        .map(|n| root.join(if n == 1 { base.clone() } else { format!("{base}-{n}") }))
        .find(|p| !p.exists())
        .ok_or_else(|| std::io::Error::other("빈 이름을 못 찾았다"))?;
    std::fs::create_dir_all(&dir)?;

    let mut roster = kasa_mcp::character::characters_json()
        .ok_or_else(|| std::io::Error::other("로스터를 못 읽었다"))?;
    if let Some(o) = roster.as_object_mut() {
        // 화면에 보일 이름은 한글로 짓는다 — 폴더 이름(`my-theme-3`)을 그대로
        // 보이면 한국어 화면에 영어 슬러그가 튄다. 그렇다고 폴더까지 한글로
        // 만들지는 않는다: macOS 는 한글 경로를 자모로 분해해 저장하는데 폴더
        // 이름이 곧 테마 id 인 구조라, 조합형으로 들어온 id 와 어긋나는 순간
        // 고른 테마를 못 찾는다.
        let shown = if label.trim().is_empty() {
            match dir.file_name().and_then(|s| s.to_str()).and_then(|s| s.strip_prefix("my-theme")) {
                None | Some("") => "새 테마".to_string(),
                Some(n) => format!("새 테마 {}", n.trim_start_matches('-')),
            }
        } else {
            label.trim().to_string()
        };
        o.insert("label".into(), serde_json::Value::String(shown));
    }
    let body = serde_json::to_string_pretty(&roster).map_err(std::io::Error::other)?;
    std::fs::write(dir.join("theme.json"), body)?;
    crate::render::export_student_sprites(&dir.join("sprites"))?;
    Ok(dir)
}

/// 테마 목록의 한 줄 — 카드 하나가 이걸 그린다.
#[derive(Clone)]
pub struct ThemeRow {
    /// 폴더 이름. 번들은 빈 문자열이다.
    pub id: String,
    pub label: String,
    /// 로스터에 든 캐릭터 수 — "이 테마에 몇 명 있나"가 고르는 기준이 된다.
    pub count: usize,
    /// 미리보기 얼굴 `(slug, png 경로)`. 경로가 `None` 이면 번들 그림.
    pub faces: Vec<(String, Option<std::path::PathBuf>)>,
}

/// 목록에 그릴 테마들. **캐시된다** — 카드 한 장마다 79명치 theme.json 을 파싱하는데
/// 스냅샷은 매 프레임 만들어지므로, 캐시가 없으면 설정 화면을 여는 것만으로 디스크가
/// 계속 돈다. 테마를 만들거나 지우거나 이름을 바꾸면 `invalidate_theme_rows` 로 비운다.
///
/// 손으로 폴더를 넣은 경우는 캐시가 모른다 — 그래서 "새로고침" 버튼이 이것도 함께
/// 비운다(그 버튼의 뜻이 곧 "파일을 다시 봐라"다).
pub fn theme_rows() -> Vec<ThemeRow> {
    if let Some(v) = THEME_ROWS.read().unwrap().as_ref() {
        return v.clone();
    }
    let mut w = THEME_ROWS.write().unwrap();
    if let Some(v) = w.as_ref() {
        return v.clone();
    }
    let v = build_theme_rows();
    *w = Some(v.clone());
    v
}

pub fn invalidate_theme_rows() {
    *THEME_ROWS.write().unwrap() = None;
}

static THEME_ROWS: std::sync::RwLock<Option<Vec<ThemeRow>>> = std::sync::RwLock::new(None);

/// 미리보기로 몇 명을 세울지. 셋이면 "여러 명이 든 한 벌"이라는 게 보이고, 카드가
/// 목록으로 늘어설 만큼 좁게 남는다.
const THEME_PREVIEW_FACES: usize = 3;

fn build_theme_rows() -> Vec<ThemeRow> {
    // 번들이 맨 앞 — 폴더가 없어 `list_themes` 에 안 잡히지만 「지금 무엇을
    // 쓰는가」는 목록에 보여야 되돌아갈 수 있다.
    let bundled = ThemeRow {
        id: String::new(),
        label: "블루 아카이브 (기본)".into(),
        count: crate::theme::CHARACTER_SLUGS.len(),
        faces: crate::theme::CHARACTER_SLUGS
            .iter()
            .take(THEME_PREVIEW_FACES)
            .map(|(_, slug)| (slug.to_string(), None))
            .collect(),
    };
    let mut out = vec![bundled];
    for (id, label) in kasa_mcp::character::list_themes() {
        let dir = kasa_mcp::character::themes_root().map(|r| r.join(&id));
        let roster = dir
            .as_ref()
            .and_then(|d| std::fs::read_to_string(d.join("theme.json")).ok())
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
        let slugs = roster.as_ref().map(roster_slugs).unwrap_or_default();
        let faces = slugs
            .iter()
            .filter_map(|slug| {
                let p = dir.as_ref()?.join("sprites").join(format!("{slug}-profile.png"));
                p.is_file().then(|| (slug.clone(), Some(p)))
            })
            .take(THEME_PREVIEW_FACES)
            .collect();
        out.push(ThemeRow { id, label, count: slugs.len(), faces });
    }
    out
}

/// 경로 조각으로 써도 안전한가. slug·테마 id 는 HTTP 쿼리에서 오고 그대로
/// `join` 되므로, 구분자와 상위참조를 여기서 끊는다. 아래 canonicalize 검사와
/// 이중 방어다 — 이쪽은 `..` 를 아예 만들지 않고, 저쪽은 심볼릭 링크로 밖을
/// 가리키는 경우를 잡는다.
fn safe_path_component(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && !s.contains('/')
        && !s.contains('\\')
        && !s.contains('\0')
        && s != "."
        && s != ".."
}

/// `root` 아래에 실재하는 파일만 읽는다(`arona_ui_serve` 와 같은 방어).
fn read_file_under(root: &std::path::Path, path: &std::path::Path) -> Option<Vec<u8>> {
    let (croot, cpath) = (root.canonicalize().ok()?, path.canonicalize().ok()?);
    if !cpath.starts_with(&croot) {
        return None;
    }
    std::fs::read(&cpath).ok()
}

/// characters.json 의 캐릭터들을 **화면에 세울 순서대로** 펼친다. 이름이 키라
/// 중복은 첫 것만 남긴다 — `member_names` 와 같은 규칙이어야 설정 화면의 목록과
/// 실제 배정 대상이 어긋나지 않는다. leader/leaders 는 하위호환 필드다(god 개념은
/// 폐기됐고 전원이 동등한 배정 대상이다).
fn roster_entries(v: &serde_json::Value) -> Vec<serde_json::Value> {
    let mut out: Vec<serde_json::Value> = Vec::new();
    let mut push = |e: &serde_json::Value| {
        let Some(name) = e.get("name").and_then(|x| x.as_str()).filter(|s| !s.is_empty()) else {
            return;
        };
        if out.iter().any(|o| o.get("name").and_then(|x| x.as_str()) == Some(name)) {
            return;
        }
        let field = |k: &str| e.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string();
        out.push(serde_json::json!({
            "name": name,
            "slug": field("slug"),
            "school": field("school"),
            "header_color": field("header_color"),
            "persona": field("persona"),
        }));
    };
    if let Some(l) = v.get("leader") {
        push(l);
    }
    for key in ["leaders", "members"] {
        if let Some(arr) = v.get(key).and_then(|x| x.as_array()) {
            for e in arr {
                push(e);
            }
        }
    }
    out
}

/// 로스터의 슬러그를 등장 순서대로. leader 가 먼저인 건 미리보기에 그 테마의
/// 얼굴마담이 서야 하기 때문 — 알파벳 순으로 자르면 아무 상관 없는 셋이 뽑힌다.
fn roster_slugs(v: &serde_json::Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |e: &serde_json::Value| {
        if let Some(s) = e.get("slug").and_then(|x| x.as_str()) {
            if !s.is_empty() && !out.iter().any(|o| o == s) {
                out.push(s.to_string());
            }
        }
    };
    if let Some(l) = v.get("leader") {
        push(l);
    }
    for key in ["leaders", "members"] {
        if let Some(arr) = v.get(key).and_then(|x| x.as_array()) {
            for e in arr {
                push(e);
            }
        }
    }
    out
}

/// 테마 폴더. id 가 비었으면 번들이라 폴더가 없다.
pub fn theme_dir(id: &str) -> Option<std::path::PathBuf> {
    if id.is_empty() {
        return None;
    }
    let d = kasa_mcp::character::themes_root()?.join(id);
    d.is_dir().then_some(d)
}

/// 목록에 보이는 이름만 바꾼다 — **폴더는 그대로 둔다.**
///
/// 폴더까지 따라 옮기면 이름 한 번 고치는 데 주소가 바뀐다: 활성 테마 선택이
/// 끊기고, 열어 둔 Finder 창과 사용자가 걸어 둔 심링크가 죽는다. label 은
/// 화면에 보이는 이름이고 폴더명은 파일시스템의 주소라, 이 둘은 갈라 두는 게 맞다.
pub fn rename_theme(id: &str, label: &str) -> std::io::Result<()> {
    let dir = theme_dir(id).ok_or_else(|| std::io::Error::other("그 테마 폴더가 없다"))?;
    let p = dir.join("theme.json");
    let mut v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p)?)
        .map_err(std::io::Error::other)?;
    let o = v.as_object_mut().ok_or_else(|| std::io::Error::other("theme.json 이 객체가 아니다"))?;
    match label.trim() {
        // 이름을 비우면 폴더명으로 되돌린다 — 빈 이름은 목록에서 고를 수 없는
        // 칸이 되어 그 테마가 사라진 것처럼 보인다.
        "" => {
            o.remove("label");
        }
        s => {
            o.insert("label".into(), serde_json::Value::String(s.to_string()));
        }
    }
    let body = serde_json::to_string_pretty(&v).map_err(std::io::Error::other)?;
    std::fs::write(&p, body)
}

/// 테마를 목록에서 치운다 — 지우지 않고 `themes/_trash/` 로 옮긴다.
///
/// 사용자가 며칠에 걸쳐 그림을 갈아 끼운 폴더를 클릭 한 번에 영영 날리는 건
/// 되돌릴 방법이 없다. `_trash` 안은 `theme.json` 이 한 단계 더 깊어 `list_themes`
/// 에 안 잡히므로, 목록에선 사라지고 파일은 남는다. 옮겨진 자리를 돌려주니
/// 부르는 쪽이 그 경로를 사용자에게 보여 줄 수 있다.
pub fn delete_theme(id: &str) -> std::io::Result<std::path::PathBuf> {
    let dir = theme_dir(id).ok_or_else(|| std::io::Error::other("그 테마 폴더가 없다"))?;
    let trash = kasa_mcp::character::themes_root()
        .ok_or_else(|| std::io::Error::other("홈 폴더를 못 찾았다"))?
        .join("_trash");
    std::fs::create_dir_all(&trash)?;
    let dest = (1..1000)
        .map(|n| trash.join(if n == 1 { id.to_string() } else { format!("{id}-{n}") }))
        .find(|p| !p.exists())
        .ok_or_else(|| std::io::Error::other("빈 이름을 못 찾았다"))?;
    std::fs::rename(&dir, &dest)?;
    Ok(dest)
}

/// 고른 캐릭터 테마 id — 빈 문자열이면 번들. 폴더가 실재하는지는 여기서 안 본다
/// (`character::active_theme_dir` 이 그걸 판정한다). 설정 화면이 「지금 고른 것」을
/// 표시하는 데 쓰므로, 폴더가 사라져도 고른 값 자체는 그대로 보여야 사용자가
/// 무엇이 어긋났는지 안다.
pub fn read_character_theme() -> String {
    kasa_mcp::character::active_theme_id()
}

pub fn read_claude_persona() -> bool {
    read_settings().get("claude_persona").and_then(|x| x.as_bool()).unwrap_or(true)
}
/// 파일트리에서 파일을 열 때 무엇으로 여는가 — `"builtin"`(내장 편집기 pane,
/// 기본) · `"app"`(VS Code 같은 GUI 편집기) · `"terminal"`(새 pane 에서 CLI
/// 편집기). `"system"` 은 `"app"` 의 옛 이름이라 그대로 받아 준다.
pub fn read_file_open_mode() -> String {
    read_settings()
        .get("file_open_mode")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("builtin")
        .to_string()
}

/// `"app"` 모드가 쓸 앱의 표시 이름(`proc::open_with_apps()` 의 첫 필드).
/// 빈 문자열이면 OS 연결 프로그램으로 연다.
pub fn read_file_open_app() -> String {
    read_settings().get("file_open_app").and_then(|x| x.as_str()).unwrap_or("").to_string()
}

/// `"terminal"` 모드가 pane 에서 실행할 명령줄. `{}` 가 있으면 파일 경로로
/// 치환하고, 없으면 뒤에 붙인다. 빈 문자열이면 `resolve_terminal_editor()` 가
/// 고른다.
pub fn read_file_open_cmd() -> String {
    read_settings().get("file_open_cmd").and_then(|x| x.as_str()).unwrap_or("").to_string()
}

/// PATH 에 있는 CLI 편집기를 하나 고른다 — `$VISUAL`/`$EDITOR` 가 먼저고(사용자가
/// 이미 밝힌 취향이다), 없으면 helix→neovim→vim→nano 순. 아무것도 없으면 `None`
/// 이라 호출자가 내장 편집기로 되돌아간다.
pub fn resolve_terminal_editor() -> Option<String> {
    let on_path = |cmd: &str| {
        // `$EDITOR="code -w"` 처럼 인자가 붙어 올 수 있어 첫 토큰만 찾는다.
        let Some(bin) = cmd.split_whitespace().next() else { return false };
        if bin.contains('/') {
            return std::path::Path::new(bin).is_file();
        }
        let Some(path) = std::env::var_os("PATH") else { return false };
        std::env::split_paths(&path).any(|d| d.join(bin).is_file())
    };
    for key in ["VISUAL", "EDITOR"] {
        if let Ok(v) = std::env::var(key) {
            if !v.trim().is_empty() && on_path(&v) {
                return Some(v);
            }
        }
    }
    ["hx", "helix", "nvim", "vim", "nano"].into_iter().find(|c| on_path(c)).map(String::from)
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

/// pane 안 `claude` 에 자동으로 얹을 MCP 서버 목록의 위치. `--mcp-config` 가 그대로
/// 먹는 `{"mcpServers":{…}}` 파일이라 **파일이 곧 진실**이다 — 여기가 갈리면 shim 을
/// 다시 굽지 않아도 다음 `claude` 부터 반영된다. `settings.json` 에 넣지 않은 이유는
/// `settings_set()` 이 그 파일을 통째로 다시 써서, MCP 를 등록하는 외부 스크립트와
/// 설정창이 동시에 쓰면 한쪽 변경이 조용히 사라지기 때문.
pub fn claude_mcp_config_path() -> Option<std::path::PathBuf> {
    Some(settings_file_path()?.with_file_name("claude-mcp.json"))
}

/// 위 파일을 shim 이 `--mcp-config` 로 넘길지. 기본 on — 파일이 없으면 shim 쪽 검사에서
/// 알아서 빠지므로, 평소 끄는 방법은 등록 스크립트로 항목을 빼는 것이다.
///
/// 설정창 토글은 **일부러 안 만들었다**: 파일에서 항목을 빼는 것과 기능이 겹쳐 노브가
/// 둘이 되고, 어느 쪽이 진실인지 헷갈린다. 파일은 그대로 두고 잠시 꺼야 할 때만
/// `settings.json` 에 `"claude_mcp": false` 를 직접 넣는다.
pub fn read_claude_mcp() -> bool {
    read_settings().get("claude_mcp").and_then(|x| x.as_bool()).unwrap_or(true)
}

/// One switchable Claude login. `id` names a directory under
/// `~/.config/kasaterm/claude-accounts/`; that path — not the label — is what
/// Claude Code hashes into its credential-store key, so **renaming is free but
/// re-`id`-ing would orphan the login**.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ClaudeAccount {
    pub id: String,
    #[serde(default)]
    pub label: String,
}

/// Configured extra logins, in display order. The default login (whatever
/// `claude` already uses) is *not* in this list — it is the implicit first row.
pub fn read_claude_accounts() -> Vec<ClaudeAccount> {
    read_settings()
        .get("claude_accounts")
        .and_then(|v| serde_json::from_value::<Vec<ClaudeAccount>>(v.clone()).ok())
        .unwrap_or_default()
}

/// Active account id, or `""` for the default login. An id that is no longer in
/// `read_claude_accounts` reads as `""` — a deleted account must fall back to
/// the default rather than point `claude` at a store nothing can log into.
pub fn read_claude_account() -> String {
    let id = read_settings()
        .get("claude_account")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    if id.is_empty() || read_claude_accounts().iter().any(|a| a.id == id) {
        id
    } else {
        String::new()
    }
}

/// Credential-store directory for an account id. `""` → None, meaning "add no
/// env at all" — the default login is the *absence* of the override, not a path.
///
/// Hangs off the settings file's directory rather than a hardcoded `~/.config`,
/// so a headless run pointed at a scratch `KASATERM_SETTINGS_FILE` also gets
/// scratch account dirs — a test can never hash its way onto a real login.
pub fn claude_account_dir(id: &str) -> Option<std::path::PathBuf> {
    if id.is_empty() {
        return None;
    }
    let base = settings_file_path()?.parent()?.to_path_buf();
    Some(base.join("claude-accounts").join(id))
}

/// One switchable Codex (ChatGPT) login. 필드는 `ClaudeAccount` 와 같지만 **타입을
/// 일부러 가른다** — 두 목록을 섞어 넣는 실수가 컴파일에서 잡힌다.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CodexAccount {
    pub id: String,
    #[serde(default)]
    pub label: String,
}

/// 계정 메뉴 표시 밀도. `true` = Compact — 창별 막대를 접고 가장 빡빡한 창 하나만.
/// 기본은 Detailed(false): 처음 여는 사람에게는 전부 보이는 편이 낫고, 좁게 쓰고 싶은
/// 사람은 메뉴 안에서 바로 바꾼다.
pub fn read_usage_compact() -> bool {
    read_settings().get("usage_compact").and_then(|x| x.as_bool()).unwrap_or(false)
}

/// 등록된 codex 슬롯들. claude 와 같은 규칙 — 기본 로그인(`~/.codex/auth.json`)은
/// 이 목록에 없고 암묵적 첫 행이다.
pub fn read_codex_accounts() -> Vec<CodexAccount> {
    read_settings()
        .get("codex_accounts")
        .and_then(|v| serde_json::from_value::<Vec<CodexAccount>>(v.clone()).ok())
        .unwrap_or_default()
}

/// 활성 codex 슬롯 id, `""` = 기본 로그인. 목록에서 사라진 id 는 `""` 로 읽는다 —
/// 지워진 슬롯을 가리키면 codex 가 아무도 로그인 못 하는 자리를 본다.
pub fn read_codex_account() -> String {
    let id = read_settings()
        .get("codex_account")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    if id.is_empty() || read_codex_accounts().iter().any(|a| a.id == id) {
        id
    } else {
        String::new()
    }
}

/// 슬롯의 `auth.json` 이 사는 자리. claude 와 달리 **인증 파일 하나만** 여기 둔다 —
/// codex shim 은 이미 pane 별 `CODEX_HOME` 을 세우고 `~/.codex` 를 심볼릭으로 미러하니
/// 계정마다 갈라야 하는 것은 auth.json 뿐이다. 홈을 통째로 가르면 세션·플러그인·스킬·
/// 캐시까지 계정 수만큼 쪼개져, pane 안 codex 가 pane 밖 codex 와 다른 것을 보게 된다.
pub fn codex_account_dir(id: &str) -> Option<std::path::PathBuf> {
    if id.is_empty() {
        return None;
    }
    let base = settings_file_path()?.parent()?.to_path_buf();
    Some(base.join("codex-accounts").join(id))
}

/// 슬롯별 OAuth 브라우저 프로필 자리. 계정 저장소와 같은 이유로 설정 파일 옆에
/// 매단다 — 스크래치 설정으로 도는 헤드리스 실행이 진짜 프로필을 안 밟는다.
/// 프로필이 슬롯마다 갈려야 두 번째 로그인이 첫 번째 세션을 물려받지 않는다.
pub fn oauth_profile_dir(id: &str) -> Option<std::path::PathBuf> {
    if id.is_empty() {
        return None;
    }
    let base = settings_file_path()?.parent()?.to_path_buf();
    Some(base.join("oauth-profiles").join(id))
}

/// 한도가 차면 다음 계정으로 알아서 넘어갈지(설정 "claude_account_autoswitch").
/// **기본 off** — 켜지 않은 사람의 인증을 마음대로 바꾸는 건 사고다.
pub fn read_account_autoswitch() -> bool {
    read_settings()
        .get("claude_account_autoswitch")
        .and_then(|x| x.as_bool())
        .unwrap_or(false)
}

/// 전환을 부르는 사용률(%) — 기본 90. 100 이면 벽에 부딪힌 뒤에나 넘어가니
/// 여유를 두고 미리 넘어가는 게 이 기능의 요점이다.
pub fn read_account_autoswitch_pct() -> f32 {
    read_settings()
        .get("claude_account_autoswitch_pct")
        .and_then(|x| x.as_f64())
        .map(|x| x as f32)
        .filter(|x| (1.0..=100.0).contains(x))
        .unwrap_or(90.0)
}

/// PixelDelta 스크롤 감도 배율 — 기본 0.3(트랙패드 기준). 트랙패드와 고해상도
/// 마우스휠이 winit 에서 같은 델타로 와 구분이 안 되므로, 마우스를 쓰는 사람이
/// 여기서 올린다. 상한은 안전장치다(오타 하나로 한 번에 화면 열 장이 넘어가면
/// 되돌릴 방법을 찾기 어렵다).
pub fn read_wheel_pixel_gain() -> f32 {
    read_settings()
        .get("wheel_pixel_gain")
        .and_then(|x| x.as_f64())
        .map(|x| x as f32)
        .filter(|x| (0.05..=5.0).contains(x))
        .unwrap_or(0.3)
}

/// 한도가 차서 떠나온 계정의 "이때 전까진 돌아가지 마라" 표. id → epoch 초
/// (기본 로그인은 `""` 키). 계정 dir 과 같은 이유로 설정 파일 옆에 둔다 —
/// 스크래치 설정으로 도는 헤드리스 실행이 진짜 쿨다운을 밟지 않게.
fn account_cooldown_path() -> Option<std::path::PathBuf> {
    Some(settings_file_path()?.parent()?.join("account-cooldown.json"))
}

pub fn read_account_cooldowns() -> std::collections::HashMap<String, u64> {
    let Some(p) = account_cooldown_path() else { return Default::default() };
    std::fs::read_to_string(p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// 만료 시각을 기록한다. 이미 더 뒤를 가리키고 있으면 그대로 둔다 — 짧은 창
/// (5시간)이 긴 창(주간)의 금지를 덮어 계정을 너무 일찍 되돌리면 안 된다.
pub fn write_account_cooldown(id: &str, until: u64) {
    let Some(p) = account_cooldown_path() else { return };
    let mut map = read_account_cooldowns();
    if map.get(id).is_some_and(|&t| t >= until) {
        return;
    }
    map.insert(id.to_string(), until);
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(txt) = serde_json::to_string(&map) {
        let _ = std::fs::write(p, txt);
    }
}

/// 쿨다운 표를 통째로 비운다 — 자동 전환을 켜는 순간에 부른다. 며칠 묵은
/// 소진 기록이 남아 있으면 켜자마자 "갈 곳이 없다"로 조용히 잠들어 버린다.
pub fn clear_account_cooldowns() {
    if let Some(p) = account_cooldown_path() {
        let _ = std::fs::remove_file(p);
    }
}

/// 지금 압박을 주는 한도 — `limits[]` 중 percent 가 가장 높은 것.
///
/// **화면도 이걸 봐야 한다**(거노 2026-08-05: "info에는 다 0퍼로뜨는데"). 전에는
/// pill·info 가 `five_hour.utilization` 만 봤는데, 실측 세 계정 모두 그 값이 `0.0`
/// 이고 실제 압박은 전부 `weekly_all`(95%/25%)이었다 — 화면은 한도가 코앞인데도
/// 0% 를 보여줬고, 자동 전환만 이 함수로 옳게 판정하고 있었다. 사용자에게 "이 세션이
/// 얼마나 남았나"와 "언제 막히나"를 갈라 보여줄 이유가 없다: 먼저 닫히는 창이 답이다.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UsagePressure {
    pub pct: f32,
    /// 그 창이 풀리는 시각(epoch 초). 없으면 쿨다운 없이 즉시 후보로 돌아온다.
    pub resets_at: Option<u64>,
    /// 어느 창인가 — 표시용 짧은 라벨. 숫자만 보여주면 5시간 창인지 주간인지 몰라
    /// "0% 인데 왜 막히나"가 된다(그게 정확히 이번 신고였다).
    pub label: &'static str,
}

/// `limits[].group`(`session`/`weekly`) → 화면 라벨. `kind` 가 아니라 `group` 을
/// 보는 것은 `weekly_all`·`weekly_scoped` 가 같은 주간 창의 두 갈래라서다.
fn usage_window_label(e: &serde_json::Value) -> &'static str {
    match e.get("group").and_then(|g| g.as_str()) {
        Some("session") => "5h",
        Some("weekly") => "7d",
        _ => "한도",
    }
}

pub fn usage_pressure(v: &serde_json::Value) -> Option<UsagePressure> {
    let top = v.get("limits").and_then(|l| l.as_array()).and_then(|arr| {
        arr.iter()
            .filter_map(|e| {
                let pct = e.get("percent").and_then(|p| p.as_f64())? as f32;
                Some((
                    pct,
                    e.get("resets_at").and_then(|s| s.as_str()).and_then(rfc3339_epoch),
                    usage_window_label(e),
                ))
            })
            .max_by(|a, b| a.0.total_cmp(&b.0))
    });
    if let Some((pct, resets_at, label)) = top {
        return Some(UsagePressure { pct, resets_at, label });
    }
    // limits[] 가 없는 옛/축약 응답 폴백 — pill 과 같은 소스.
    let five = v.get("five_hour")?;
    Some(UsagePressure {
        pct: five.get("utilization")?.as_f64()? as f32,
        resets_at: five.get("resets_at").and_then(|s| s.as_str()).and_then(rfc3339_epoch),
        label: "5h",
    })
}

/// `2026-07-30T11:49:59.589840+00:00` → epoch 초. chrono 를 끌어오기엔 쓰임이
/// 이거 하나뿐이라 직접 판다. 오프셋(`Z`/`±HH:MM`)까지 반영한다.
fn rfc3339_epoch(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || (b[10] != b'T' && b[10] != b' ') {
        return None;
    }
    let n = |a: usize, z: usize| s.get(a..z)?.parse::<i64>().ok();
    let (y, mo, d) = (n(0, 4)?, n(5, 7)?, n(8, 10)?);
    let (h, mi, sec) = (n(11, 13)?, n(14, 16)?, n(17, 19)?);
    let mut secs = days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + sec;
    // 소수점 이하는 버리고 오프셋만 찾는다.
    let tail = &s[19..];
    if let Some(i) = tail.find(['+', '-']) {
        let off = &tail[i..];
        let sign = if off.starts_with('-') { 1 } else { -1 };
        let oh = off.get(1..3)?.parse::<i64>().ok()?;
        let om = off.get(4..6).and_then(|x| x.parse::<i64>().ok()).unwrap_or(0);
        secs += sign * (oh * 3600 + om * 60);
    }
    u64::try_from(secs).ok()
}

/// 그레고리력 날짜 → 1970-01-01 기준 일수 (Howard Hinnant, `days_from_civil`).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// 다음으로 옮겨갈 계정 id. 후보는 `""`(기본 로그인) + 설정 목록 순서이고,
/// 현재 계정 **다음** 자리부터 한 바퀴 돌며 쿨다운이 풀린 첫 후보를 고른다.
/// 전부 잠겨 있으면 `None` — 어차피 갈 곳이 없는데 옮기면 멀쩡한 계정에서
/// 소진된 계정으로 내려앉는 꼴이 된다.
pub fn pick_next_account(
    current: &str,
    accounts: &[ClaudeAccount],
    cooldowns: &std::collections::HashMap<String, u64>,
    now: u64,
) -> Option<String> {
    let mut ids: Vec<&str> = vec![""];
    ids.extend(accounts.iter().map(|a| a.id.as_str()));
    let here = ids.iter().position(|&i| i == current).unwrap_or(0);
    (1..ids.len())
        .map(|step| ids[(here + step) % ids.len()])
        .find(|id| cooldowns.get(*id).is_none_or(|&t| t <= now))
        .map(str::to_string)
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
    let Some(home) = kasa_socket::home_dir() else { return String::new() };
    let path = home.join(".claude/settings.json");
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
/// pane 셸 아래에서 도는 claude 프로세스 pid. shim 이 심는 KASATERM_* env 와 팀원
/// 트리플은 이 프로세스에만 있다(셸엔 없다). 셸 → claude 가 보통 직계지만 래퍼가
/// 끼는 경우가 있어 몇 대 아래까지 훑는다.
fn claude_under(table: &[(u32, u32, String)], shell: u32) -> Option<u32> {
    let mut frontier = vec![shell];
    for _ in 0..3 {
        let mut next = Vec::new();
        for (pid, ppid, cmd) in table {
            if !frontier.contains(ppid) {
                continue;
            }
            // 셸이 낳은 것 중 claude 실행파일만 — `claude` 를 인자로 든 셸 명령이
            // 아니라 실행 경로가 claude 로 끝나는 프로세스.
            if cmd.split_whitespace().next().is_some_and(|exe| exe.ends_with("claude")) {
                return Some(*pid);
            }
            next.push(*pid);
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    None
}

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
    let projects = kasa_socket::home_dir()?.join(".claude").join("projects");
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
    crate::strip_activity_prefix(t).trim_end()
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
    let projects = kasa_socket::home_dir()?.join(".claude").join("projects");
    scan_projects_for_session(&projects, sid)
}

/// `transcript_path_for_session` 의 순수 부분 — projects 루트를 인자로 받아
/// `$HOME` 없이도 테스트할 수 있게 갈라 뒀다.
///
/// 같은 sid 가 여러 폴더에 있을 수 있다 — rename 을 겪은 사람이 옛 폴더의 대화를
/// 새 폴더로 복사해 손수 복구하기 때문이다(미도리 실측). `read_dir` 순서는
/// 파일시스템이 정하므로 첫 히트를 쓰면 어느 쪽을 이어갈지가 실행마다 달라진다.
/// 최근에 쓰인 것이 곧 이어가려던 대화라 mtime 최신을 고르고, 같으면 경로
/// 사전순으로 끊어 답을 하나로 굳힌다.
fn scan_projects_for_session(
    projects: &std::path::Path,
    sid: &str,
) -> Option<std::path::PathBuf> {
    if sid.is_empty() || sid.contains('/') {
        return None;
    }
    let want = format!("{sid}.jsonl");
    let mut hits: Vec<(std::time::SystemTime, std::path::PathBuf)> =
        std::fs::read_dir(projects)
            .ok()?
            .flatten()
            .filter_map(|d| {
                let p = d.path().join(&want);
                let mtime = p.metadata().and_then(|m| m.modified()).ok()?;
                Some((mtime, p))
            })
            .collect();
    hits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    hits.into_iter().next().map(|(_, p)| p)
}

/// codex rollout 파일명에서 세션 id 를 뽑는다 — `rollout-<ISO ts>-<uuid>.jsonl`.
///
/// claude 는 파일명 자체가 sid 라 `file_stem()` 이 곧 답이지만 codex 는 접두사·타임스탬프가
/// 붙는다. 그대로 stem 을 쓰면 `rollout-2026-08-05T19-46-01-019f…` 가 sid 로 박혀
/// `codex resume` 도 sqlite 조회도 전부 빗나간다.
///
/// uuid 는 `-` 를 품으므로 뒤에서 5토막을 떼어 붙인다(8-4-4-4-12). 타임스탬프도 `-` 를
/// 품어 앞에서 세는 방식은 못 쓴다.
pub(crate) fn codex_sid_from_rollout(path: &std::path::Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let rest = stem.strip_prefix("rollout-")?;
    let parts: Vec<&str> = rest.rsplitn(6, '-').collect();
    if parts.len() < 6 {
        return None;
    }
    // rsplitn 은 역순 — 앞 5개가 uuid 의 뒤 5토막이다(6번째는 타임스탬프 잔여).
    let sid = parts[..5]
        .iter()
        .rev()
        .copied()
        .collect::<Vec<_>>()
        .join("-");
    let ok = sid.len() == 36
        && sid
            .chars()
            .all(|c| c.is_ascii_hexdigit() || c == '-');
    ok.then_some(sid)
}

/// 세션 id → codex rollout jsonl. `transcript_path_for_session` 의 codex 판.
///
/// sqlite(`state_5.sqlite` 의 `threads`)에도 같은 정보가 있지만 **파일 탐색으로 간다**:
/// 워크스페이스에 sqlite 의존성이 없고(넣을 만큼 큰 조회가 아니다), rollout 파일명이
/// sid 를 그대로 품으며, transcript 파서도 같은 디렉터리를 읽는다 — 정본이 하나로 남는다.
///
/// **`~/.codex/sessions` 만 본다.** shim 이 세운 pane 별 CODEX_HOME 은 이 디렉터리를
/// 심볼릭으로 미러하므로 pane 안에서 만든 세션의 실체도 여기 있다(실측). sqlite 에는
/// 그때의 pane 홈 경유 경로가 박히는데 그 홈은 GUI 재시작이면 사라진다 — 그래서 기록된
/// 경로를 믿지 않고 여기서 다시 찾는 편이 재시작 후에도 성립한다.
pub(crate) fn codex_rollout_for_session(sid: &str) -> Option<std::path::PathBuf> {
    let root = kasa_socket::home_dir()?.join(".codex").join("sessions");
    scan_codex_sessions(&root, sid)
}

/// `codex_rollout_for_session` 의 순수 부분 — 루트를 인자로 받아 `$HOME` 없이 테스트한다.
///
/// 레이아웃은 `sessions/<Y>/<M>/<D>/rollout-<ts>-<sid>.jsonl`. 날짜 칸이 셋이라 깊이 3을
/// 그대로 내려간다(전체 walk 은 하지 않는다 — 오래 쓰면 날짜 폴더만 수백 개다).
fn scan_codex_sessions(root: &std::path::Path, sid: &str) -> Option<std::path::PathBuf> {
    if sid.is_empty() || sid.contains('/') {
        return None;
    }
    let suffix = format!("-{sid}.jsonl");
    // 날짜 폴더를 이름 역순으로 — 최신이 먼저라 대개 첫 폴더에서 끝난다.
    let sorted_dirs = |d: &std::path::Path| -> Vec<std::path::PathBuf> {
        let mut v: Vec<_> = std::fs::read_dir(d)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        v.sort_by(|a, b| b.cmp(a));
        v
    };
    for y in sorted_dirs(root) {
        for m in sorted_dirs(&y) {
            for d in sorted_dirs(&m) {
                for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                    let p = e.path();
                    if p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.ends_with(&suffix))
                    {
                        return Some(p);
                    }
                }
            }
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
        return jsonl_for_session(&cwd, &id);
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
    let slug = cwd.to_string_lossy().replace(['/', '.'], "-");
    let roster = kasa_socket::home_dir()?
        .join(".config/kasaterm/agent-roster")
        .join(format!("{slug}.json"));
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&roster).ok()?).ok()?;
    let entry = v.get(pane_id)?;
    if entry.get("archived").and_then(|a| a.as_bool()).unwrap_or(false) {
        return None;
    }
    let session = entry.get("session_id").and_then(|s| s.as_str())?;
    jsonl_for_session(cwd, session)
}

/// `session` 의 transcript 경로. cwd 로 만든 폴더를 먼저 보고, 없으면 projects
/// 전체에서 그 uuid 를 찾는다.
///
/// ⚠️ 폴더명은 **claude 가 시작한 시점의 cwd** 로 굳는다 — 레포 폴더 이름을
/// 바꾸면 대화는 옛 이름 폴더에 남는데 우리는 새 cwd 로만 찾아, 살아 있는
/// 세션의 바인딩이 조용히 끊겼다. 그러면 저장에 sid 가 안 실리고 다음 재시작이
/// 이어가기 대신 빈 세션을 띄운다(미도리 실측: chromeclaude→kasachrome rename
/// 뒤 19MB 대화가 끊김). sid 는 uuid 라 전역에서 유일하니 폴더가 갈려도 안전하다.
pub(crate) fn jsonl_for_session(
    cwd: &std::path::Path,
    session: &str,
) -> Option<std::path::PathBuf> {
    let projects = kasa_socket::home_dir()?.join(".claude/projects");
    jsonl_for_session_in(&projects, cwd, session)
}

/// `jsonl_for_session` 의 순수 부분 — projects 루트를 인자로 받는다.
fn jsonl_for_session_in(
    projects: &std::path::Path,
    cwd: &std::path::Path,
    session: &str,
) -> Option<std::path::PathBuf> {
    let direct = projects.join(project_slug(cwd)).join(format!("{session}.jsonl"));
    if direct.exists() {
        return Some(direct);
    }
    scan_projects_for_session(projects, session)
}

/// `cwd` 의 claude 프로젝트 디렉터리에서 `within` 안에 수정된 .jsonl 경로들.
fn recent_jsonls(cwd: &std::path::Path, within: std::time::Duration) -> Vec<std::path::PathBuf> {
    let Some(home) = kasa_socket::home_dir() else { return Vec::new() };
    let encoded = cwd.to_string_lossy().replace(['/', '.'], "-");
    let dir = home.join(".claude/projects").join(encoded);
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

/// claude 가 cwd 를 projects 폴더 이름으로 굳힐 때 쓰는 규칙 — `/` 와 `.` 이 `-`.
pub(crate) fn project_slug(cwd: &std::path::Path) -> String {
    cwd.to_string_lossy().replace(['/', '.'], "-")
}

/// `~/.claude/projects/<encoded-cwd>/<session>.jsonl` 경로 구성.
pub(crate) fn project_jsonl(cwd: &std::path::Path, session: &str) -> Option<std::path::PathBuf> {
    Some(
        kasa_socket::home_dir()?
            .join(".claude/projects")
            .join(project_slug(cwd))
            .join(format!("{session}.jsonl")),
    )
}


#[cfg(test)]
mod codex_session_lookup_tests {
    use super::*;

    /// codex 파일명은 `rollout-<ts>-<uuid>.jsonl` 이라 stem 이 sid 가 아니다.
    /// 타임스탬프도 uuid 도 `-` 를 품어 앞에서 세는 방식은 못 쓴다.
    #[test]
    fn sid_comes_from_the_tail_not_the_stem() {
        let p = std::path::Path::new(
            "/x/sessions/2026/08/05/rollout-2026-08-05T19-46-01-019fd187-ba6e-7812-8976-2a27ffcd843e.jsonl",
        );
        assert_eq!(
            codex_sid_from_rollout(p).as_deref(),
            Some("019fd187-ba6e-7812-8976-2a27ffcd843e")
        );
        // claude 파일(파일명 자체가 sid)은 rollout 이 아니라 None — 호출측이 stem 폴백을 쓴다.
        assert_eq!(
            codex_sid_from_rollout(std::path::Path::new("/x/abcd-1234.jsonl")),
            None
        );
        // 토막이 모자라거나 hex 가 아니면 안 받는다 — 엉뚱한 값을 sid 로 박느니 없는 편이 낫다.
        assert_eq!(
            codex_sid_from_rollout(std::path::Path::new("/x/rollout-2026-08-05.jsonl")),
            None
        );
    }

    /// 날짜 3단 아래에서 sid 로 끝나는 파일을 찾는다. **suffix 매칭이라** 다른 세션의
    /// 파일명이 이 sid 를 접두사로 품어도 안 걸린다.
    #[test]
    fn finds_the_rollout_under_the_date_dirs() {
        let root = std::env::temp_dir().join(format!("kt-codexsess-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let day = root.join("2026/08/05");
        std::fs::create_dir_all(&day).unwrap();
        let sid = "019fd187-ba6e-7812-8976-2a27ffcd843e";
        let want = day.join(format!("rollout-2026-08-05T19-46-01-{sid}.jsonl"));
        std::fs::write(&want, "{}").unwrap();
        // 다른 날 + 다른 세션 — 골라내면 안 되는 것들.
        let other = root.join("2026/08/04");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(
            other.join("rollout-2026-08-04T10-00-00-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee.jsonl"),
            "{}",
        )
        .unwrap();
        assert_eq!(scan_codex_sessions(&root, sid).as_deref(), Some(want.as_path()));
        assert_eq!(scan_codex_sessions(&root, "no-such-session"), None);
        // 방어: 빈 sid·경로 조각은 디렉터리 전체를 훑게 두지 않는다.
        assert_eq!(scan_codex_sessions(&root, ""), None);
        assert_eq!(scan_codex_sessions(&root, "../x"), None);
        std::fs::remove_dir_all(&root).ok();
    }
}

#[cfg(test)]
mod agents_view_tests {
    use super::*;

    #[test]
    fn session_found_after_the_repo_folder_was_renamed() {
        // 폴더명은 claude 가 시작한 시점의 cwd 로 굳는다 — rename 하면 대화는 옛
        // 이름 폴더에 남는다. 새 cwd 로만 찾던 동안엔 살아 있는 세션의 바인딩이
        // 조용히 끊겨 다음 재시작이 빈 세션이 됐다(미도리 실측).
        let root = std::env::temp_dir().join(format!("kt-proj-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let new_cwd = std::path::Path::new("/Users/kasa/Desktop/momewomo/kasachrome");
        let old = root.join("-Users-kasa-Desktop-momewomo-chromeclaude");
        let new = root.join(project_slug(new_cwd));
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap(); // rename 직후라 비어 있다
        let sid = "6fbe280f-1111-2222-3333-444455556666";
        let in_old = old.join(format!("{sid}.jsonl"));
        std::fs::write(&in_old, "{}\n").unwrap();

        assert_eq!(jsonl_for_session_in(&root, new_cwd, sid), Some(in_old.clone()));
        assert_eq!(jsonl_for_session_in(&root, new_cwd, "no-such-session"), None);

        // 새 cwd 폴더에 같은 대화가 생기면(사용자가 손수 복구) 그쪽이 이긴다 —
        // 폴백은 cwd 로 못 찾았을 때만 도는 뒷문이다.
        let in_new = new.join(format!("{sid}.jsonl"));
        std::fs::write(&in_new, "{}\n").unwrap();
        assert_eq!(jsonl_for_session_in(&root, new_cwd, sid), Some(in_new.clone()));

        // 폴백이 여러 폴더에서 같은 sid 를 만나도 답은 하나여야 한다 — mtime 최신.
        // (read_dir 순서에 기대면 어느 대화를 이어갈지가 실행마다 갈린다.)
        let now = std::time::SystemTime::now();
        set_mtime(&in_old, now);
        set_mtime(&in_new, now - std::time::Duration::from_secs(60));
        let other_cwd = std::path::Path::new("/nowhere");
        assert_eq!(jsonl_for_session_in(&root, other_cwd, sid), Some(in_old));
        set_mtime(&new.join(format!("{sid}.jsonl")), now + std::time::Duration::from_secs(60));
        assert_eq!(jsonl_for_session_in(&root, other_cwd, sid), Some(in_new));
        std::fs::remove_dir_all(&root).ok();
    }

    fn set_mtime(p: &std::path::Path, t: std::time::SystemTime) {
        let f = std::fs::File::options().write(true).open(p).unwrap();
        f.set_modified(t).unwrap();
    }

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

/// 한도 자동 계정 전환의 판정부. 전부 순수 함수라 실제 인증 저장소·설정 파일을
/// 건드리지 않고 검증된다 — 이 기능의 실수는 남의 로그인을 갈아치우는 실수라
/// 로직만이라도 파일 IO 밖에서 확인할 수 있게 갈라 뒀다.
#[cfg(test)]
mod account_autoswitch_tests {
    use super::*;

    #[test]
    fn rfc3339_epoch_reads_offsets_and_fractions() {
        // oauth/usage 가 실제로 주는 모양(마이크로초 + `+00:00`).
        assert_eq!(rfc3339_epoch("2026-07-30T11:49:59.589840+00:00"), Some(1785412199));
        // 같은 순간을 KST 로 쓴 것 — 오프셋을 안 빼면 9시간이 어긋난다.
        assert_eq!(rfc3339_epoch("2026-07-30T20:49:59+09:00"), Some(1785412199));
        assert_eq!(rfc3339_epoch("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(rfc3339_epoch("어제"), None);
    }

    #[test]
    fn usage_pressure_takes_the_worst_window_not_the_five_hour_one() {
        // 5시간 창은 한가한데 주간이 차 있는 상태 — pill(12%)만 보면 못 넘어간다.
        let v = serde_json::json!({
            "five_hour": { "utilization": 12.0, "resets_at": "2026-07-30T11:49:59.589840+00:00" },
            "limits": [
                { "kind": "session", "percent": 12, "resets_at": "2026-07-30T11:49:59.589840+00:00" },
                { "kind": "weekly_all", "percent": 94, "resets_at": "2026-08-05T09:59:59+00:00" },
            ]
        });
        let p = usage_pressure(&v).expect("limits 가 있으면 판정된다");
        assert_eq!(p.pct, 94.0);
        assert_eq!(p.resets_at, Some(1785923999));
    }

    #[test]
    fn usage_pressure_falls_back_to_five_hour_without_limits() {
        let v = serde_json::json!({ "five_hour": { "utilization": 91.0 } });
        let p = usage_pressure(&v).expect("five_hour 만 있어도 판정된다");
        assert_eq!(p.pct, 91.0);
        assert_eq!(p.resets_at, None);
        assert_eq!(p.label, "5h");
    }

    /// 거노 화면에서 그대로 뜬 응답(2026-08-05, 기본 슬롯 토큰으로 직접 조회).
    /// `five_hour.utilization` 이 **0.0** 인데 주간이 95% 다 — 화면이 five_hour 만
    /// 보던 탓에 한도가 코앞인데 「0%」 가 떴다. 라벨까지 재는 것은 숫자만 고치면
    /// 「5h 95%」 가 되어 5시간 창 이야기로 읽히기 때문이다.
    #[test]
    fn real_world_zero_five_hour_with_critical_weekly() {
        let v = serde_json::json!({
            "five_hour": { "utilization": 0.0, "resets_at": null },
            "limits": [
                { "group": "session", "kind": "session", "percent": 0, "resets_at": null },
                { "group": "weekly", "kind": "weekly_all", "percent": 95,
                  "resets_at": "2026-08-05T10:00:00.423760+00:00" },
                { "group": "weekly", "kind": "weekly_scoped", "percent": 11,
                  "resets_at": "2026-08-05T10:00:00.424089+00:00" },
            ]
        });
        let p = usage_pressure(&v).expect("limits 가 있으면 판정된다");
        assert_eq!(p.pct, 95.0, "화면에 뜰 숫자는 주간 95% 여야 한다");
        assert_eq!(p.label, "7d", "그게 어느 창인지도 말해야 한다");
    }

    /// `group` 이 없는(옛/축약) 항목은 창 종류를 단정하지 않는다 — `5h` 로 찍으면
    /// 주간 압박을 5시간 이야기로 오독하게 만든다.
    #[test]
    fn unknown_group_gets_a_neutral_label() {
        let v = serde_json::json!({ "limits": [{ "kind": "mystery", "percent": 42 }] });
        let p = usage_pressure(&v).expect("percent 만 있어도 판정된다");
        assert_eq!(p.pct, 42.0);
        assert_eq!(p.label, "한도");
    }

    fn accts(ids: &[&str]) -> Vec<ClaudeAccount> {
        ids.iter()
            .map(|i| ClaudeAccount { id: i.to_string(), label: String::new() })
            .collect()
    }

    #[test]
    fn pick_next_account_rotates_from_the_current_slot() {
        let a = accts(&["acct-1", "acct-2"]);
        let none = Default::default();
        // 기본 → 첫 슬롯 → 둘째 슬롯 → 다시 기본.
        assert_eq!(pick_next_account("", &a, &none, 0).as_deref(), Some("acct-1"));
        assert_eq!(pick_next_account("acct-1", &a, &none, 0).as_deref(), Some("acct-2"));
        assert_eq!(pick_next_account("acct-2", &a, &none, 0).as_deref(), Some(""));
    }

    #[test]
    fn pick_next_account_skips_and_then_refuses_cooled_down_slots() {
        let a = accts(&["acct-1", "acct-2"]);
        let mut cool = std::collections::HashMap::new();
        cool.insert("acct-1".to_string(), 500_u64);
        // acct-1 은 아직 잠겨 있으니 건너뛴다.
        assert_eq!(pick_next_account("", &a, &cool, 100).as_deref(), Some("acct-2"));
        // 풀린 뒤에는 다시 1순위.
        assert_eq!(pick_next_account("", &a, &cool, 600).as_deref(), Some("acct-1"));
        // 전부 잠기면 안 옮긴다 — 멀쩡한 자리에서 소진된 자리로 내려앉지 않게.
        cool.insert("acct-2".to_string(), 500);
        cool.insert(String::new(), 500);
        assert_eq!(pick_next_account("acct-1", &a, &cool, 100), None);
    }

    #[test]
    fn pick_next_account_has_nowhere_to_go_with_a_single_login() {
        assert_eq!(pick_next_account("", &[], &Default::default(), 0), None);
    }
}

// macOS(libproc)·Windows(PEB) 두 네이티브 구현만 — 나머지 unix 는 `lsof` 셸아웃이라
// 설치 여부에 결과가 달린다. Windows 를 넣는 이유는 그쪽이 가장 위험해서다: 하드코딩
// 오프셋으로 남의 주소공간을 직접 읽는 코드라, 어긋나도 예외 없이 쓰레기 경로를 낸다.
#[cfg(all(test, any(target_os = "macos", windows)))]
mod pid_cwd_tests {
    /// libproc 경로가 예전 `lsof -d cwd` 와 같은 답을 내는지 — 자기 자신에게
    /// 물어 `current_dir` 과 맞춰본다. FFI(구조체 크기·평면화한 vip_path·NUL
    /// 종료)가 어긋나면 조용히 빈 경로나 쓰레기를 내므로 여기서 잡는다.
    #[test]
    fn pid_cwd_reads_our_own_cwd() {
        let got = super::pid_cwd(std::process::id()).expect("자기 cwd 는 항상 읽힌다");
        let want = std::env::current_dir().unwrap();
        assert_eq!(got.canonicalize().unwrap(), want.canonicalize().unwrap());
    }

    /// 없는 pid 는 None — 실패를 빈 PathBuf 로 흘리면 호출부가 루트를 cwd 로 본다.
    #[test]
    fn pid_cwd_is_none_for_a_dead_pid() {
        assert_eq!(super::pid_cwd(u32::MAX - 1), None);
    }

    #[cfg(windows)]
    #[test]
    fn peb_cwd_loses_its_trailing_separator_but_keeps_the_drive_root() {
        assert_eq!(super::trim_trailing_sep(r"C:\Users\x\"), r"C:\Users\x");
        assert_eq!(super::trim_trailing_sep(r"C:\Users\x"), r"C:\Users\x");
        assert_eq!(super::trim_trailing_sep(r"C:\"), r"C:\");
        assert_eq!(super::trim_trailing_sep(r"\\srv\share\"), r"\\srv\share");
    }

    #[test]
    fn finds_claude_below_the_pane_shell() {
        // 실측 형태: pane 셸(zsh) → claude. claude 의 자식 셸(Bash 툴)도 같이 있다.
        let table = vec![
            (100, 1, "/bin/zsh -il".to_string()),
            (200, 100, "/Users/x/.local/bin/claude --model opus".to_string()),
            (300, 200, "/bin/zsh -c ls".to_string()),
        ];
        assert_eq!(super::claude_under(&table, 100), Some(200));
        // claude 없이 셸만 도는 pane — 인박스로 말을 걸 수 없다.
        assert_eq!(super::claude_under(&table[..1], 100), None);
    }

    #[test]
    fn a_shell_running_the_word_claude_is_not_claude() {
        // `send` 로 부팅 커맨드를 흘려보낸 직후의 셸 — 아직 claude 가 아니다.
        let table = vec![
            (100, 1, "/bin/zsh -il".to_string()),
            (200, 100, "/bin/zsh -c cd /repo && claude --model opus".to_string()),
        ];
        assert_eq!(super::claude_under(&table, 100), None);
    }
}
