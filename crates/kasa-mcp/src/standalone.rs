//! A headless `Backend` for the standalone webview server (the `kasa-serve-web`
//! bin). kasaterm normally serves the arona-ui webview off 8765 via
//! `spawn_http_server`, so when the terminal exits the webview goes blind. This
//! backend lets a tiny stand-alone process keep serving the *daemon* view —
//! `claude agents` background sessions and their transcripts — with no GUI, no
//! live panes, just the `~/.claude/projects` files on disk.
//!
//! The `Backend` trait already defaults 57 of its methods to a safe
//! bail/empty, so this only writes the 7 required-no-default methods (live-pane
//! ops that are meaningless here → bail; list queries → empty) plus the 3 that
//! actually do work off disk: `active_cwd`, `recent_sessions`,
//! `session_transcript_raw`. `/background-agents` needs no method — its handler
//! shells out to `claude agents --json --all` and only reads
//! `pane_session_ids()` (default empty), so background sessions simply come
//! back without a `parentSurface` tag, which is correct for a paneless host.

use std::path::PathBuf;

use anyhow::Result;
use kasa_socket::backend::{
    Backend, PaneActivity, RecentSession, SplitDirection, SurfaceInfo, WorkspaceInfo,
};
use kasa_socket::sessions::{
    is_uuid, recent_sessions_for, session_board_meta, session_jsonl_path, transcript_tail_text,
};

pub struct StandaloneBackend {
    /// The cwd all disk lookups resolve against when a caller doesn't pass one.
    /// There's no GUI proxy to ask for the "active" pane cwd, so it's fixed at
    /// construction (from `--cwd` or the process cwd). `recent_sessions` and
    /// `session_transcript_raw` fall back to this when their `cwd` arg is None.
    root: PathBuf,
}

impl StandaloneBackend {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl Backend for StandaloneBackend {
    // --- required (no trait default) ---------------------------------------
    // Standalone has no live panes: list queries return empty, pane ops bail.
    fn list_workspaces(&self) -> Result<Vec<WorkspaceInfo>> {
        Ok(Vec::new())
    }
    fn current_workspace(&self) -> Result<Option<WorkspaceInfo>> {
        Ok(None)
    }
    fn list_surfaces(&self) -> Result<Vec<SurfaceInfo>> {
        Ok(Vec::new())
    }
    fn focus_surface(&self, _surface_id: &str) -> Result<()> {
        anyhow::bail!("standalone webview server has no live panes")
    }
    fn split_surface(&self, _direction: SplitDirection, _focus: bool) -> Result<SurfaceInfo> {
        anyhow::bail!("standalone webview server has no live panes")
    }
    fn send_text(&self, surface_id: Option<&str>, text: &str) -> Result<()> {
        // tell — 라이브 pane 이 없으니 대상은 background claude 세션. sessionId 의 bg-pty-host
        // 소켓(claude 가 background 세션의 pty 를 노출)에 텍스트+CR 을 써 stdin 으로 주입한다.
        // 실험적: claude 내부 소켓 프로토콜에 의존해 버전이 바뀌면 깨질 수 있다.
        let sid = surface_id
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("standalone tell requires a session id (surface)"))?;
        let short = &sid[..sid.len().min(8)];
        let sock = find_bg_pty_sock(short).ok_or_else(|| {
            anyhow::anyhow!("no bg-pty-host socket for session {sid} — 라이브 터미널 세션이거나 종료됨")
        })?;
        use std::io::Write;
        let mut stream = std::os::unix::net::UnixStream::connect(&sock)
            .map_err(|e| anyhow::anyhow!("connect {sock:?}: {e}"))?;
        stream.write_all(text.as_bytes())?;
        stream.write_all(b"\r")?;
        stream.flush()?;
        Ok(())
    }
    fn send_key(&self, _surface_id: Option<&str>, _key: &str) -> Result<()> {
        anyhow::bail!("standalone webview server has no live panes")
    }

    // --- the 3 that actually work off disk ---------------------------------
    fn active_cwd(&self) -> Option<PathBuf> {
        Some(self.root.clone())
    }

    fn recent_sessions(&self, cwd: Option<&str>) -> Result<Vec<RecentSession>> {
        let base = cwd.map(PathBuf::from).unwrap_or_else(|| self.root.clone());
        Ok(recent_sessions_for(&base, 20))
    }

    fn session_transcript_raw(&self, id: &str, cwd: Option<&str>) -> Result<String> {
        // Offline read by uuid — same resolution as PtyBackend, minus the live
        // pane: uuid guard → cwd (arg else root) → jsonl path → read.
        if !is_uuid(id) {
            anyhow::bail!("invalid session id: {id}");
        }
        let base = cwd
            .map(PathBuf::from)
            .or_else(|| self.active_cwd())
            .ok_or_else(|| anyhow::anyhow!("no cwd for session {id}"))?;
        let path = session_jsonl_path(&base, id)
            .ok_or_else(|| anyhow::anyhow!("no HOME — cannot locate session {id}"))?;
        std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("read session transcript {path:?}: {e}"))
    }

    // --- 협업 뷰(read-only) — 라이브 pane 이 없으니 대상은 claude agents 세션들 ---------
    fn collab_board(&self) -> Result<Vec<PaneActivity>> {
        // board = claude agents(background/interactive) 세션 현황. 각 세션 transcript 에서
        // title/last_prompt 를 파싱 → kasaterm 꺼져도 세션들이 서로의 작업을 board 로 본다.
        let out = std::process::Command::new(crate::http::claude_bin())
            .args(["agents", "--json", "--all"])
            .output()?;
        if !out.status.success() {
            return Ok(Vec::new());
        }
        let agents: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).unwrap_or_default();
        let mut board = Vec::new();
        for a in &agents {
            let Some(sid) = a.get("sessionId").and_then(|s| s.as_str()) else { continue };
            let name = a.get("name").and_then(|s| s.as_str()).unwrap_or("").to_string();
            let state = a
                .get("state")
                .and_then(|s| s.as_str())
                .or_else(|| a.get("status").and_then(|s| s.as_str()))
                .unwrap_or("idle")
                .to_string();
            let cwd = a
                .get("cwd")
                .and_then(|s| s.as_str())
                .map(PathBuf::from)
                .unwrap_or_else(|| self.root.clone());
            let (title, last_prompt) = session_board_meta(&cwd, sid).unwrap_or_default();
            board.push(PaneActivity {
                surface_id: sid.to_string(),
                character: (!name.is_empty()).then_some(name),
                title,
                last_prompt,
                status: state,
                cwd: cwd.to_string_lossy().into_owned(),
                ..Default::default()
            });
        }
        Ok(board)
    }

    fn peek(&self, surface_id: &str, lines: usize) -> Result<String> {
        // surface_id = sessionId(라이브 pane 이 아니므로). transcript 마지막 대화로 '엿보기'.
        let turns = if lines == 0 { 6 } else { lines };
        transcript_tail_text(&self.root, surface_id, turns)
            .ok_or_else(|| anyhow::anyhow!("no transcript for session {surface_id}"))
    }
}

/// background claude 세션의 stdin 소켓을 찾는다. claude 는 `--bg-pty-host <sock>` 로 각
/// background 세션의 pty 를 `<tmp>/cc-daemon-<uid>/<daemon>/pty/<sessionId-8>.sock` 에 노출한다.
/// macOS 는 /tmp, 그 외는 $TMPDIR 도 훑는다.
fn find_bg_pty_sock(short: &str) -> Option<PathBuf> {
    let mut roots = vec![PathBuf::from("/tmp")];
    if let Ok(t) = std::env::var("TMPDIR") {
        if !t.is_empty() {
            roots.push(PathBuf::from(t));
        }
    }
    for root in roots {
        let Ok(rd) = std::fs::read_dir(&root) else { continue };
        for entry in rd.flatten() {
            if !entry.file_name().to_string_lossy().starts_with("cc-daemon-") {
                continue;
            }
            let Ok(daemons) = std::fs::read_dir(entry.path()) else { continue };
            for d in daemons.flatten() {
                let sock = d.path().join("pty").join(format!("{short}.sock"));
                if sock.exists() {
                    return Some(sock);
                }
            }
        }
    }
    None
}
