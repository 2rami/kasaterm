//! 셸을 다른 폴더로 옮긴다 — 왼쪽 파일트리의 「셸을 이 폴더로」와, 다른 기기가 부르는 `POST /term/cd`.
//! 놀고 있는 셸만 옮긴다: 셸 밑에 claude·빌드 같은 것이 돌면 그 입력칸에 글자가 들어간다.
//! 다른 기기 칸은 이쪽에서 그 셸이 노는지 알 수 없어 원본 기기가 판단한다.

use super::*;

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

/// 이 기기 셸 칸(`id` 는 탭 pid)을 `path` 로. 소켓·HTTP 스레드에서도 부른다 — GUI 루프를 거치지 않는다.
pub(crate) fn surface_cd(id: &str, path: &str) -> Result<()> {
    if kasa_mcp::remote::is_remote_pane(id) {
        anyhow::bail!("다른 기기 칸은 그 기기가 옮겨요");
    }
    let session = kasa_pty::lookup_session(id).ok_or_else(|| anyhow::anyhow!("옮길 셸 칸이 없어요"))?;
    if !std::path::Path::new(path).is_dir() {
        anyhow::bail!("그 폴더가 없어요");
    }
    let table = kasa_pty::fresh_process_table();
    let pid = session.shell_pid();
    if !crate::transfer_endpoints::idle_shell(pid, &table) {
        anyhow::bail!("셸에서 무언가 도는 중이라 옮기지 않았어요");
    }
    let shell = pid
        .and_then(|pid| table.iter().find(|(found, _, _)| *found == pid))
        .map_or("", |(_, _, name)| name.as_str());
    session.send_bytes(cd_line(shell, path).as_bytes())
}

impl App {
    /// 지금 칸 셸이 선 폴더 — 셸 탭일 때만(미리보기 탭이면 옮길 셸이 없다). 다른 기기 칸은 그 기기가 알린 폴더.
    pub(crate) fn active_shell_cwd(&self) -> Option<std::path::PathBuf> {
        let (id, tab) = {
            let ws = self.ws.lock().ok()?;
            let id = ws.active_pane.clone()?;
            let pane = ws.panes.get(&id)?;
            pane.tabs.get(pane.active_tab)?.term()?;
            let tab = ws.active_tab_pid(&id);
            (id, tab)
        };
        match kasa_mcp::remote::remote_info(&tab) {
            Some(info) => self.pane_cwd_cache.get(&id).cloned().or_else(|| info.remote_cwd.map(std::path::PathBuf::from)),
            None => self.pane_cwd_cache.get(&id).cloned().or_else(|| self.pane_current_cwd(&tab)),
        }
    }

    /// 파일트리에서 고른 폴더로 지금 칸의 셸을 옮긴다. 다른 기기 칸은 원본에 부탁하고 답은 알림으로 받는다.
    pub(crate) fn cd_active_shell(&mut self, dir: &std::path::Path) {
        let tab = {
            let ws = self.ws.lock().unwrap();
            let Some(active) = ws.active_pane.clone() else { return };
            ws.active_tab_pid(&active)
        };
        let path = dir.to_string_lossy().into_owned();
        let Some(info) = kasa_mcp::remote::remote_info(&tab) else {
            if let Err(e) = surface_cd(&tab, &path) {
                self.set_toast(format!("{e:#}"));
            }
            return;
        };
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let body = serde_json::json!({ "pane": info.remote_id, "path": path });
            let why = match kasa_mcp::remote::remote_post_json(&info.base, "/term/cd", &body) {
                Ok(v) if v.get("ok").and_then(|o| o.as_bool()) == Some(true) => return,
                Ok(v) => v.get("error").and_then(|e| e.as_str()).unwrap_or("셸을 옮기지 못했어요").to_string(),
                Err(e) if e.to_string().contains("404") => format!("{} 의 카사텀이 아직 셸 옮기기를 몰라요", info.label),
                Err(e) => format!("{} 에 못 닿았어요 — {e:#}", info.label),
            };
            let _ = proxy.send_event(UserEvent::SocketToast(why));
        });
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
