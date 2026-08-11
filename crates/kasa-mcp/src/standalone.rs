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
        // tell — background claude 세션에 텍스트 주입. bg-pty-host pty 소켓에 raw write 는
        // 안 먹는다: 실제 진입은 control.sock 의 nudge→attach(op) 핸드셰이크 + 세션 고정 auth
        // 토큰이 필요하다(인터포저로 프로토콜 해독). auth 출처가 불투명하므로 그 핸드셰이크를
        // 직접 재현하는 대신, `claude attach <sid>` 를 forkpty 로 띄운다 — claude 가 nudge/
        // attach/auth 를 다 처리하니 우리는 pty stdin 에 텍스트+CR 을 쓰고 잠시 뒤 SIGTERM 으로
        // detach 하면 된다. (실환경 idle 세션 검증 필요 — blocked/working 세션엔 즉시 안 먹음.)
        let sid = surface_id
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("standalone tell requires a session id (surface)"))?;
        // forkpty 기반 `claude attach` 주입은 Unix 전용 — Windows 엔 forkpty 가 없다.
        // Windows standalone 서버는 아직 tell 을 지원하지 않으므로 명시적으로 bail.
        #[cfg(not(unix))]
        {
            let _ = (sid, text);
            return Err(anyhow::anyhow!(
                "standalone tell (forkpty attach) is not supported on Windows yet"
            ));
        }
        #[cfg(unix)]
        {
            let short: String = sid.chars().take(8).collect();
            let claude = crate::http::claude_bin();
            let claude_c = std::ffi::CString::new(claude.to_string_lossy().as_bytes())
                .map_err(|_| anyhow::anyhow!("bad claude path"))?;
            let attach_c = std::ffi::CString::new("attach").unwrap();
            let short_c =
                std::ffi::CString::new(short).map_err(|_| anyhow::anyhow!("bad session id"))?;
            unsafe {
                let mut master: libc::c_int = 0;
                let pid = libc::forkpty(
                    &mut master,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                );
                if pid < 0 {
                    anyhow::bail!("forkpty failed");
                }
                if pid == 0 {
                    // child: exec `claude attach <short>` on the pty.
                    let argv =
                        [claude_c.as_ptr(), attach_c.as_ptr(), short_c.as_ptr(), std::ptr::null()];
                    libc::execv(claude_c.as_ptr(), argv.as_ptr());
                    libc::_exit(127);
                }
                // parent: attach 화면이 뜰 시간을 준 뒤(pty output drain) 텍스트 주입, claude 가
                // user 메시지로 처리할 시간을 두고 SIGTERM 으로 detach(세션은 daemon 에 유지).
                libc::fcntl(master, libc::F_SETFL, libc::O_NONBLOCK);
                let mut buf = [0u8; 4096];
                let drain_until =
                    std::time::Instant::now() + std::time::Duration::from_millis(2500);
                while std::time::Instant::now() < drain_until {
                    libc::read(master, buf.as_mut_ptr() as *mut libc::c_void, buf.len());
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                let payload = format!("{text}\r");
                let _ =
                    libc::write(master, payload.as_ptr() as *const libc::c_void, payload.len());
                std::thread::sleep(std::time::Duration::from_millis(2500));
                libc::kill(pid, libc::SIGTERM);
                let mut status = 0;
                libc::waitpid(pid, &mut status, 0);
                libc::close(master);
            }
            return Ok(());
        }
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
        let out = crate::no_window_command(crate::http::claude_bin())
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
