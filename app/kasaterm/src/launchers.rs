//! 하단바 실행 단추 — settings.json `launchers` 의 명령을 단추 하나로 띄운다.
//!
//! 누르면 이 기기 방의 새 탭에서 그 명령을 돌리고, 그 프로그램이 이미 도는 칸이 있으면
//! 새로 띄우지 않고 그 칸으로 간다. 단추가 없던 동안 kasaslk(슬랙 TUI)는 셸에 이름을
//! 쳐야 열렸다(2026-10-01 「카사텀 버튼」).
//!
//! ```json
//! "launchers": [{ "label": "슬랙", "command": "kasaslk", "icon": "message-circle" }]
//! ```
//!
//! 키가 없으면 기본 목록(설치된 kasaslk 하나)이고, `[]` 이면 단추가 없다. 목록을 앱이
//! 다시 쓰지 않는다 — 기본값을 파일에 박아 두면 나중에 기본 목록이 바뀌어도 못 받는다.

use super::*;

/// 아이콘 이름은 `gpu::icon_svg` 에 있는 것만 받는다 — 모르는 이름은 빈칸으로 그려져
/// 단추가 글자만 남는다.
const ICONS: [&str; 12] = [
    "terminal",
    "message-circle",
    "mail",
    "globe",
    "github",
    "database",
    "server",
    "monitor",
    "folder",
    "file-text",
    "sparkles",
    "rotate-cw",
];

/// 막 띄운 칸은 프로그램이 프로세스 표에 오르기 전이다. 그 사이 다시 누르면 같은 칸으로만
/// 간다 — 셸 프롬프트로 보고 명령을 한 번 더 보내면 프로그램 입력칸에 글자가 박힌다.
const LAUNCH_GRACE: std::time::Duration = std::time::Duration::from_secs(3);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Launcher {
    pub(crate) label: String,
    pub(crate) command: String,
    pub(crate) icon: &'static str,
}

impl Launcher {
    /// 도는 칸을 알아보는 이름 — 명령 첫 낱말의 파일 이름. 프로세스 표(`running_job`)가
    /// 경로 없이 이름만 준다.
    pub(crate) fn program(&self) -> &str {
        program_of(&self.command)
    }
}

fn program_of(command: &str) -> &str {
    let first = command.split_whitespace().next().unwrap_or("");
    first.rsplit(['/', '\\']).next().unwrap_or(first)
}

pub(crate) fn from_settings(settings: &serde_json::Value) -> Vec<Launcher> {
    let Some(items) = settings.get("launchers").and_then(|v| v.as_array()) else {
        return defaults();
    };
    items.iter().filter_map(parse).collect()
}

fn parse(item: &serde_json::Value) -> Option<Launcher> {
    let command = item.get("command")?.as_str()?.trim();
    if command.is_empty() || command.contains('\n') {
        return None;
    }
    let label = item
        .get("label")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| program_of(command));
    let icon = item
        .get("icon")
        .and_then(|v| v.as_str())
        .and_then(|name| ICONS.iter().find(|known| **known == name).copied())
        .unwrap_or("terminal");
    Some(Launcher { label: label.to_string(), command: command.to_string(), icon })
}

/// 설치된 것만 기본 단추로 — 공개 레포라 kasaslk 가 없는 기기가 대부분이고, 거기서
/// 누르면 「command not found」 탭만 생긴다.
pub(crate) fn defaults() -> Vec<Launcher> {
    if !installed("kasaslk") {
        return Vec::new();
    }
    vec![Launcher {
        label: "슬랙".to_string(),
        command: "kasaslk".to_string(),
        icon: "message-circle",
    }]
}

/// 앱의 PATH 는 Dock 으로 뜨면 `/usr/bin:/bin` 뿐이라, 칸의 셸이 보는 사용자 bin 도 함께 본다.
fn installed(program: &str) -> bool {
    let name = if cfg!(windows) { format!("{program}.exe") } else { program.to_string() };
    let mut dirs: Vec<std::path::PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    if let Some(home) = kasa_socket::home_dir() {
        dirs.push(home.join(".local/bin"));
    }
    dirs.push("/opt/homebrew/bin".into());
    dirs.push("/usr/local/bin".into());
    dirs.iter().any(|dir| dir.join(&name).is_file())
}

/// 이 칸이 지금 이 프로그램을 돌리는가. 셸 프롬프트면 `running_job` 이 None 이다.
fn runs(session: &kasa_pty::PtySession, program: &str) -> bool {
    session
        .running_job()
        .is_some_and(|job| job.trim_end_matches(".exe") == program)
}

impl App {
    /// 실행 단추를 누름. 도는 칸이 있으면 그리로, 내가 띄웠다가 프로그램이 끝나 셸만 남은
    /// 칸이면 거기서 다시, 둘 다 아니면 새 탭.
    pub(crate) fn open_launcher(&mut self, index: usize) {
        let Some(launcher) = self.set_statusbar.launchers.get(index).cloned() else {
            return;
        };
        let program = launcher.program();
        let remembered = self
            .statusbar
            .launched
            .get(&launcher.command)
            .filter(|(pid, _)| self.pty.contains_key(pid))
            .cloned();
        if let Some((pid, at)) = &remembered {
            let session = &self.pty[pid];
            if at.elapsed() < LAUNCH_GRACE || runs(session, program) {
                self.focus_surface(pid);
                self.chrome_dirty = true;
                return;
            }
        }
        let running = self
            .pty
            .iter()
            .filter(|(_, session)| runs(session, program))
            .map(|(pid, _)| pid.clone())
            .min();
        if let Some(pid) = running {
            self.focus_surface(&pid);
            self.chrome_dirty = true;
            return;
        }
        if let Some((pid, _)) = remembered {
            if self.pty[&pid].running_job().is_none() {
                self.focus_surface(&pid);
                // ^U 로 프롬프트에 남은 글자를 지운 뒤 친다 — 사람이 쓰다 둔 글자에 붙으면
                // 엉뚱한 명령이 된다.
                self.send_bytes_to_surface(Some(&pid), format!("\x15{}\n", launcher.command).as_bytes());
                self.statusbar.launched.insert(launcher.command.clone(), (pid, Instant::now()));
                self.chrome_dirty = true;
                return;
            }
        }
        let Some(outer) = self.ws.lock().unwrap().active_pane.clone() else {
            self.set_toast(format!("{} 을 띄울 칸이 없어요", launcher.label));
            return;
        };
        // 이 기기에 세운다 — 활성 칸이 다른 기기의 거울이면 그대로 두었을 때 새 탭이
        // 저쪽 기기에서 뜨고, 거기엔 이 명령이 없을 수 있다.
        self.pending_character = None;
        self.pending_spawn_cwd = Some(
            kasa_socket::home_dir()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| "/".to_string()),
        );
        let spawned = self.spawn_new_tab(&outer, true);
        self.pending_spawn_cwd = None;
        match spawned {
            Ok(pid) => {
                self.send_bytes_to_surface(Some(&pid), format!("{}\n", launcher.command).as_bytes());
                self.focus_surface(&pid);
                self.statusbar.launched.insert(launcher.command.clone(), (pid, Instant::now()));
            }
            Err(e) => self.set_toast(format!("{} 탭을 못 열었어요: {e:#}", launcher.label)),
        }
        self.chrome_dirty = true;
    }

    /// 단추 뒤 점(켜짐)의 근거. 프로세스 표는 공유 캐시라 `ps` 를 더 부르지 않지만,
    /// 칸 전부를 훑으므로 프레임마다가 아니라 1초에 한 번.
    pub(crate) fn launchers_tick(&mut self) {
        if self.set_statusbar.launchers.is_empty() {
            return;
        }
        if self
            .statusbar
            .launcher_checked
            .is_some_and(|at| at.elapsed() < std::time::Duration::from_secs(1))
        {
            return;
        }
        self.statusbar.launcher_checked = Some(Instant::now());
        let live: Vec<bool> = self
            .set_statusbar
            .launchers
            .iter()
            .map(|launcher| {
                let program = launcher.program();
                self.pty.values().any(|session| runs(session, program))
            })
            .collect();
        if live != self.statusbar.launcher_live {
            self.statusbar.launcher_live = live;
            self.chrome_dirty = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_is_the_first_word_without_its_path() {
        assert_eq!(program_of("kasaslk"), "kasaslk");
        assert_eq!(program_of("~/.local/bin/kasaslk --workspace sionic"), "kasaslk");
        assert_eq!(program_of("  htop "), "htop");
    }

    #[test]
    fn settings_list_replaces_the_defaults_and_drops_broken_rows() {
        let parsed = from_settings(&serde_json::json!({
            "launchers": [
                { "label": "모니터", "command": "htop", "icon": "monitor" },
                { "command": "lazygit", "icon": "없는-아이콘" },
                { "label": "빈 명령", "command": "  " },
                { "label": "두 줄", "command": "a\nb" },
                "문자열"
            ]
        }));
        assert_eq!(
            parsed,
            vec![
                Launcher { label: "모니터".into(), command: "htop".into(), icon: "monitor" },
                Launcher { label: "lazygit".into(), command: "lazygit".into(), icon: "terminal" },
            ]
        );
        assert!(from_settings(&serde_json::json!({ "launchers": [] })).is_empty());
    }

    #[test]
    fn missing_key_falls_back_to_installed_defaults() {
        assert_eq!(from_settings(&serde_json::json!({})), defaults());
        assert!(defaults().iter().all(|l| l.command == "kasaslk" && l.label == "슬랙"));
    }

    #[test]
    fn every_allowed_icon_is_drawable() {
        for name in ICONS {
            assert!(crate::gpu::GpuRenderer::icon_svg(name).is_some(), "{name}");
        }
    }
}
