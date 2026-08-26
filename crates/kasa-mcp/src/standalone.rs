//! A headless `Backend` for the standalone webview server (the `kasa-serve-web`
//! bin). kasaterm normally serves the arona-ui webview off 8765 via
//! `spawn_http_server`, so when the terminal exits the webview goes blind. This
//! backend lets a tiny stand-alone process keep serving the *daemon* view —
//! `claude agents` background sessions and their transcripts — with no GUI, no
//! live panes, just the `~/.claude/projects` files on disk.
//!
//! The `Backend` trait already defaults 57 of its methods to a safe
//! bail/empty, so this only writes the 7 required-no-default methods (live-pane
//! ops that are meaningless here → bail; list queries → empty) plus the ones that
//! actually do work off disk: `active_cwd`, `recent_sessions`,
//! `session_transcript_raw`, `collab_board`, `peek`, `transcript_tail`.
//!
//! ⚠️ **디스크를 읽는 창구는 전부 `jsonl_for` 를 지난다.** 세션 목록을 만드는 길과
//! 세션 하나를 여는 길이 서로 다른 cwd 를 쓰면 「목록엔 뜨는데 안 열리는」 상태가
//! 되고, 목록이 정상이라 화면에선 원인이 안 보인다(맥미니 실측: 교집합 0개).
//! 그리고 트레이트 기본값이 대부분 **빈 값**이라, 창구를 빠뜨리면 오류가 아니라
//! **빈 화면**으로 나온다 — `transcript_tail` 이 그렇게 `turns: 0` 을 주고 있었다.
//! `/background-agents` needs no method — its handler
//! shells out to `claude agents --json --all` and only reads
//! `pane_session_ids()` (default empty), so background sessions simply come
//! back without a `parentSurface` tag, which is correct for a paneless host.

use std::path::PathBuf;

use anyhow::Result;
use kasa_socket::backend::{
    Backend, ConversationTurn, PaneActivity, RecentSession, SplitDirection, SurfaceInfo,
    WorkspaceInfo,
};
use kasa_socket::sessions::{
    format_turns, is_uuid, recent_sessions_here, session_board_meta, session_jsonl_path_resolved,
    transcript_turns_at,
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

    /// 세션 하나를 **여는** 길. 목록을 만드는 길과 갈리지 않게 한 자리로 모았다.
    ///
    /// ⚠️ 원래는 세 창구가 제각기 `self.root` 로만 열려 했는데, board 는
    /// `claude agents --json` 이 준 **세션마다의 cwd** 로 제목을 읽는다. 그 둘이
    /// 갈려 있으면 **목록엔 뜨는데 누르면 안 열리는** 상태가 된다 — 맥미니 실측에서
    /// 교집합이 0개였다(root `/Users/miku`, board 14개는 전부 `/Users/miku/nacho-neko`).
    /// 목록은 멀쩡하니 화면에선 원인이 안 보인다.
    fn jsonl_for(&self, id: &str, cwd: Option<&str>) -> Result<PathBuf> {
        let base = cwd.map(PathBuf::from).unwrap_or_else(|| self.root.clone());
        session_jsonl_path_resolved(&base, id)
            .ok_or_else(|| anyhow::anyhow!("no transcript for session {id}"))
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
    fn split_surface(
        &self,
        _direction: SplitDirection,
        _focus: bool,
        _from: Option<&str>,
    ) -> Result<SurfaceInfo> {
        anyhow::bail!("standalone webview server has no live panes")
    }
    fn send_text(&self, surface_id: Option<&str>, text: &str) -> Result<()> {
        // tell — background claude 세션에 텍스트 주입. bg-pty-host pty 소켓에 raw write 는
        // 안 먹는다: 실제 진입은 control.sock 의 nudge→attach(op) 핸드셰이크 + 세션 고정 auth
        // 토큰이 필요하다(인터포저로 프로토콜 해독). auth 출처가 불투명하므로 그 핸드셰이크를
        // 직접 재현하는 대신, `claude attach <sid>` 를 forkpty 로 띄운다 — claude 가 nudge/
        // attach/auth 를 다 처리하니 우리는 pty stdin 에 텍스트+CR 을 쓰고 잠시 뒤 SIGTERM 으로
        // detach 하면 된다.
        //
        // 실환경 검증됨(2026-08-26, 맥미니): `done` 세션에 「방금 답한 숫자에 10을 곱하면?」을
        // 넣으니 앞 턴(1+1→2)을 이어받아 **20** 이라 답했다 — 주입만 되는 게 아니라 맥락이
        // 이어진다. blocked/working 세션엔 즉시 안 먹는 것은 그대로다.
        //
        // ⚠️**부르는 쪽은 이 함수의 반환을 기다려 성패를 판정하면 안 된다.** drain 2.5s +
        // 처리 2.5s 에 `claude attach` 가 SIGTERM 을 받고 정리하는 시간이 더 붙어, HTTP 로
        // 감싸면 25초를 넘겨 클라이언트가 먼저 끊는다(실측). 실제로는 성공했는데 화면엔
        // 실패로 보인다 — 「보냈다」로 끊고 transcript 갱신으로 확인하는 편이 맞다.
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
        // ⚠️ 여기는 일부러 root 를 그대로 쓴다 — 「이 폴더의 `claude --resume` 후보」가
        // 뜻이라, 위 `jsonl_for` 처럼 전역으로 넓히면 다른 뜻이 된다. 대신 standalone 은
        // root 가 프로세스가 뜬 자리일 뿐이라 board 와 갈릴 수 있다(맥미니가 그랬다).
        // 전역 목록이 필요해지면 `recent_claude_sessions_all` 이 이미 있다.
        let base = cwd.map(PathBuf::from).unwrap_or_else(|| self.root.clone());
        // 60개. 20이면 이 폴더의 목록이 최근 claude 로만 채워져, 같은 폴더에서
        // codex 로 일한 기록이 한 줄도 안 보인다(tmuxify 실측: 20칸 전부 claude,
        // 60칸이면 비-claude 6개가 올라온다). 값은 release 로 재고 정했다.
        Ok(recent_sessions_here(&base, 60))
    }

    fn session_transcript_raw(&self, id: &str, cwd: Option<&str>) -> Result<String> {
        // Offline read by uuid — same resolution as PtyBackend, minus the live
        // pane: uuid guard → cwd (arg else root) → jsonl path → read.
        if !is_uuid(id) {
            anyhow::bail!("invalid session id: {id}");
        }
        let path = self.jsonl_for(id, cwd)?;
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
        let path = self.jsonl_for(surface_id, None)?;
        Ok(format_turns(&transcript_turns_at(&path, turns)))
    }

    /// 웹뷰의 대화 화면이 읽는 것. 트레이트 기본값이 **빈 벡터**라, 이걸 안 쓰면
    /// `/transcript` 가 `ok:true` 에 `turns: 0` 을 준다 — 오류가 아니라 **빈 대화**로
    /// 보여서 「이 세션은 원래 비었다」와 구분이 안 된다(맥미니 실측으로 밟은 자리).
    fn transcript_tail(&self, surface_id: &str, turns: usize) -> Result<Vec<ConversationTurn>> {
        let path = self.jsonl_for(surface_id, None)?;
        Ok(transcript_turns_at(&path, turns)
            .into_iter()
            .map(|(role, text)| ConversationTurn { role, text })
            .collect())
    }
}
