//! 셸이 첫 프롬프트를 내기 전 칸 제목 자리의 「준비 중」.
//!
//! Windows 콘솔은 셸을 띄우자마자 제목을 실행 파일 경로로 붙이고, PowerShell 7 은 프로필을
//! 읽느라 몇 초 동안 아무것도 안 그린다. 맥은 셸보다 먼저 「Last login」 줄을 찍어 티가 나지만
//! Windows 칸은 빈 화면이라 멈춘 것과 구별이 안 됐다(2026-11-13). 준비 판정은 이사가 도착 셸을
//! 기다릴 때와 같은 OSC 133 프롬프트 표식(`prompt_end`)이다.
use super::*;
use std::time::Duration;

/// cmd·Git Bash 처럼 바로 뜨는 셸에서 깜빡이지 않게.
const SHOW_AFTER: Duration = Duration::from_millis(400);
/// 표식을 안 내는 셸(사용자 설정이 프롬프트를 통째로 덮은 경우)에서 영영 남지 않게.
const GIVE_UP: Duration = Duration::from_secs(15);
const SPINNER: [char; 4] = ['◐', '◓', '◑', '◒'];

fn loading_title(started: Instant, prompted: bool, title: Option<&str>, now: Instant) -> Option<String> {
    if prompted {
        return None;
    }
    let waited = now.checked_duration_since(started)?;
    if waited < SHOW_AFTER || waited >= GIVE_UP {
        return None;
    }
    let frame = SPINNER[(waited.as_millis() / 250) as usize % SPINNER.len()];
    Some(format!("{frame} {} 준비 중", shell_name(title)))
}

/// 콘솔이 붙인 제목(`C:\Program Files\PowerShell\7\pwsh.exe`)이면 실행 파일 이름만.
fn shell_name(title: Option<&str>) -> String {
    let Some(title) = title.map(str::trim).filter(|t| !t.is_empty()) else {
        return "셸".into();
    };
    let file = title.rsplit(['\\', '/']).next().unwrap_or(title);
    match file.len().checked_sub(4).filter(|&cut| file.is_char_boundary(cut)) {
        Some(cut) if file[cut..].eq_ignore_ascii_case(".exe") && cut > 0 => file[..cut].to_string(),
        _ => title.to_string(),
    }
}

impl App {
    /// 칸의 지금 탭이 아직 첫 프롬프트를 못 낸 이 기기 셸이면 그 「준비 중」 제목.
    pub(crate) fn shell_loading_title(&self, ws: &Workspace, pane_id: &str) -> Option<String> {
        let pane = ws.panes.get(pane_id)?;
        self.tab_loading_title(pane.tabs.get(pane.active_tab)?, pane_id)
    }

    pub(crate) fn tab_loading_title(&self, tab: &PaneTab, pane_id: &str) -> Option<String> {
        let pid = tab.pid.as_deref().unwrap_or(pane_id);
        if kasa_remote::remote::is_remote_pane(pid) {
            return None;
        }
        let session = self.pty.get(pid)?;
        session.shell_pid()?;
        let term = tab.term()?;
        // 콘솔이 제목을 붙이기 전에도 이름이 나오게 띄운 셸 경로가 먼저다.
        let name = session.shell_program().or(tab.title.as_deref());
        loading_title(term.started?, term.prompt_end.is_some(), name, Instant::now())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shows_only_between_a_moment_and_the_give_up_and_never_after_the_prompt() {
        let t0 = Instant::now();
        let pwsh = Some(r"C:\Program Files\PowerShell\7\pwsh.exe");
        assert_eq!(loading_title(t0, false, pwsh, t0 + Duration::from_millis(100)), None, "fast shells never flash");
        let shown = loading_title(t0, false, pwsh, t0 + Duration::from_secs(2)).unwrap();
        assert!(shown.ends_with("pwsh 준비 중"), "{shown}");
        assert_eq!(loading_title(t0, true, pwsh, t0 + Duration::from_secs(2)), None, "prompt is up");
        assert_eq!(loading_title(t0, false, pwsh, t0 + Duration::from_secs(15)), None, "gives up");
    }

    #[test]
    fn names_the_shell_from_the_console_title() {
        assert_eq!(shell_name(Some(r"C:\WINDOWS\system32\cmd.exe")), "cmd");
        assert_eq!(shell_name(Some(r"C:\Program Files\PowerShell\7\PWSH.EXE")), "PWSH");
        assert_eq!(shell_name(Some("Windows PowerShell")), "Windows PowerShell");
        assert_eq!(shell_name(Some("  ")), "셸");
        assert_eq!(shell_name(None), "셸");
        assert_eq!(shell_name(Some("파워셸.exe")), "파워셸");
    }
}
