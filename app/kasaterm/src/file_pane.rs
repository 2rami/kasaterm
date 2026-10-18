//! 셸 칸 옆 파일 칸(`kasa-files`). 여는 길과, 그 칸이 소켓으로 부르는 셸 폴더 읽기·옮기기.
//! 그리기는 칸 안 TUI 가 한다 — 다른 기기 칸·폰 거울에서도 같은 화면이 뜨게.

use super::*;

/// 파일 칸이 짝 셸과 나눠 갖는 폭의 몫(docs/design.md 「파일 칸」).
const FILE_PANE_SHARE: f32 = 0.3;

/// 쓰던 줄을 비우고 친다. 프롬프트에 반쯤 친 글이 있어도 섞이지 않게.
pub(crate) fn cd_line(shell: &str, path: &str) -> String {
    let name = shell.trim_start_matches('-').to_ascii_lowercase();
    let name = name.strip_suffix(".exe").unwrap_or(&name);
    match name {
        "pwsh" | "powershell" => format!("Set-Location -LiteralPath '{}'\r", path.replace('\'', "''")),
        "cmd" => format!("cd /d \"{path}\"\r"),
        _ => format!("\x15 cd -- '{}'\r", path.replace('\'', r"'\''")),
    }
}

fn local_session(id: &str) -> Result<Arc<kasa_pty::PtySession>> {
    if kasa_mcp::remote::is_remote_pane(id) {
        anyhow::bail!("다른 기기 칸은 그 기기의 파일 칸이 따라가요");
    }
    kasa_pty::lookup_session(id).ok_or_else(|| anyhow::anyhow!("따라갈 셸 칸이 없어요"))
}

/// 소켓 스레드에서 답한다 — 파일 칸이 0.5초마다 묻는데 GUI 루프를 깨우지 않게.
pub(crate) fn surface_cwd(id: &str) -> Result<String> {
    let session = local_session(id)?;
    session
        .reported_cwd()
        .or_else(|| session.shell_pid().and_then(socket::pid_cwd))
        .map(|path| path.to_string_lossy().into_owned())
        .ok_or_else(|| anyhow::anyhow!("셸 폴더를 모르겠어요"))
}

/// 놀고 있는 셸만 옮긴다. 셸 밑에 claude·빌드 같은 것이 돌면 그 입력칸에 글자가 들어간다.
pub(crate) fn surface_cd(id: &str, path: &str) -> Result<()> {
    let session = local_session(id)?;
    if !std::path::Path::new(path).is_dir() {
        anyhow::bail!("그 폴더가 없어요");
    }
    let table = kasa_pty::fresh_process_table();
    let pid = session.shell_pid();
    if !crate::transfer_endpoints::idle_shell(pid, &table) {
        anyhow::bail!("셸이 일하는 중이라 옮기지 않았어요");
    }
    let shell = pid
        .and_then(|pid| table.iter().find(|(found, _, _)| *found == pid))
        .map_or("", |(_, _, name)| name.as_str());
    session.send_bytes(cd_line(shell, path).as_bytes())
}

impl App {
    /// 파일 칸에서 사람이 고른 파일 — 짝 셸 칸의 탭으로 열고 그 탭을 앞에 세운다.
    /// 같은 탭 길을 쓰는 에이전트 미리보기는 사람이 보던 화면을 안 뺏으려고 뒤에 둔다.
    pub(crate) fn open_file_from_pane(&mut self, path: std::path::PathBuf, target: Option<String>) {
        self.open_file(path.clone(), target, true);
        {
            let mut ws = self.ws.lock().unwrap();
            let found = ws.panes.iter().find_map(|(id, pane)| {
                pane.tabs
                    .iter()
                    .position(|tab| tab.preview_path.as_deref() == Some(path.as_path()))
                    .map(|at| (id.clone(), at))
            });
            let Some((id, at)) = found else { return };
            if let Some(pane) = ws.panes.get_mut(&id) {
                pane.active_tab = at;
                pane.dirty = true;
            }
            ws.active_pane = Some(id);
        }
        self.handoff_ime_to_active_surface();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// ⋮ 「파일 트리」 — 그 칸 오른쪽에 짝 파일 칸을 연다. 이미 있으면 그리로 간다.
    pub(crate) fn open_file_pane(&mut self, pane: &str) {
        let shell = self.ws.lock().unwrap().active_tab_pid(pane);
        if let Some(files) = self.file_panes.get(&shell).cloned() {
            if self.pty.contains_key(&files) && self.focus_pane(&files) {
                return;
            }
            self.file_panes.remove(&shell);
        }
        // 거울이면 칸이 원본 기기에 서고, 거기서 셸은 원본 번호로 불린다.
        let follow = kasa_mcp::remote::remote_info(&shell).map_or_else(|| shell.clone(), |info| info.remote_id);
        let files = match self.split_active_pane_focused(kasa_pty::SplitDir::Horizontal) {
            Ok(files) => files,
            Err(e) => {
                self.set_toast(format!("파일 트리를 열지 못했어요 — {e:#}"));
                return;
            }
        };
        // 트리는 이름만 보이면 된다 — 셸에 폭을 더 준다. 칸이 모자라 탭으로 열렸으면 그대로 둔다.
        self.edit_layout_of_pane(&files, |tree| tree.set_leaf_ratio(&files, FILE_PANE_SHARE));
        let Some(session) = self.pty.get(&files).cloned() else { return };
        let follow = shell_quote_path(&follow);
        // 셸 문법이 다른 윈도우는 그냥 부른다 — 끝내면 셸이 남는다.
        let line = if cfg!(windows) {
            format!("kasa-files --follow {follow}\r")
        } else {
            format!("command -v kasa-files >/dev/null && exec kasa-files --follow {follow} || echo '이 기기의 카사텀이 아직 파일 트리를 몰라요'\r")
        };
        let at = std::time::Instant::now() + std::time::Duration::from_millis(300);
        self.pending_restores.push((session, line, at));
        self.file_panes.insert(shell, files);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cd_line_quotes_for_each_shell_and_clears_the_prompt_first() {
        assert_eq!(cd_line("-zsh", "/Users/kasa/내 드라이브"), "\x15 cd -- '/Users/kasa/내 드라이브'\r");
        assert_eq!(cd_line("bash", "/tmp/it's"), "\x15 cd -- '/tmp/it'\\''s'\r");
        assert_eq!(cd_line("fish", "/a b"), "\x15 cd -- '/a b'\r");
        assert_eq!(cd_line("pwsh.exe", r"C:\Users\it's"), "Set-Location -LiteralPath 'C:\\Users\\it''s'\r");
        assert_eq!(cd_line("cmd.exe", r"C:\a b"), "cd /d \"C:\\a b\"\r");
    }
}
