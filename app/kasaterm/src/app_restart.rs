//! 앱 재시작 — 이 기기의 사실을 GUI 스레드에서 모은다. 계획·거부·작업 기록·도우미는
//! `kasa_socket::app_restart`, 절차는 `docs/app-restart.md`.
//!
//! 실행 창구는 아직 없다(사람 승인 흐름이 생기기 전까지). 여기서는 읽기만 한다.

use super::*;
use kasa_socket::app_restart::{BinaryId, BusyPane, Facts, PendingInstall, CAPABILITY, SCHEMA};

fn mtime_ms(meta: &std::fs::Metadata) -> u64 {
    meta.modified().ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(unix)]
fn inode(meta: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.ino()
}

#[cfg(not(unix))]
fn inode(_meta: &std::fs::Metadata) -> u64 {
    0
}

/// 굽기가 도는 중인가 — dist 를 쓰는 도중에 끄면 자기설치가 반쯤 쓴 번들을 볼 수 있다.
fn baking() -> bool {
    crate::proc::command("/bin/ps")
        .args(["-Axww", "-o", "command="])
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).lines().any(|l| l.contains("scripts/build-app.sh")))
        .unwrap_or(false)
}

fn pet_alive() -> Option<bool> {
    let path = kasa_socket::home_dir()?.join(".config/kasaterm/pet.pid");
    let pid: i32 = std::fs::read_to_string(path).ok()?.trim().parse().ok()?;
    // SAFETY: 신호 0 은 보내지 않고 존재만 묻는다.
    Some(unsafe { libc::kill(pid, 0) } == 0)
}

impl App {
    pub(crate) fn restart_facts(&self) -> Facts {
        let now = kasa_socket::board::now_ms();
        let exe = std::env::current_exe().ok();
        let binary = exe.as_ref().and_then(|p| std::fs::metadata(p).ok()).map(|meta| BinaryId {
            inode: inode(&meta),
            mtime_ms: mtime_ms(&meta),
            build: kasa_mcp::machines::build_id(),
        }).unwrap_or_default();
        let install_pending = crate::install_pending_paths().map(|(_, dist)| PendingInstall {
            dist_mtime_ms: std::fs::metadata(dist.join("Contents/MacOS/kasaterm")).map(|m| mtime_ms(&m)).unwrap_or(0),
            dist_path: dist.display().to_string(),
        });
        // 등록 서버 창은 다시 켜면 서버가 다시 뜬다 — 「명령이 도는 창」 거부에서 뺀다.
        let server_panes: std::collections::HashSet<String> = {
            let ws = self.ws.lock().unwrap();
            ws.panes.values().flat_map(|p| p.tabs.iter()).filter(|t| t.server.is_some()).filter_map(|t| t.pid.clone()).collect()
        };
        let mut ids: Vec<&String> = self.pty.keys().collect();
        ids.sort();
        let (mut busy, mut restorable, mut shells) = (Vec::new(), 0, 0);
        for id in ids {
            let state = self.collab.hub.resolved(id).map(|r| r.state);
            let character = self.pane_character_if_known(id).unwrap_or_default();
            if let Some(s) = state.as_ref().filter(|s| s.is_busy() || s.needs_you()) {
                busy.push(BusyPane { surface: id.clone(), character: character.clone(), state: s.board_word().into() });
            }
            if self.pane_session_id.contains_key(id) {
                restorable += 1;
                continue;
            }
            shells += 1;
            // 학생이 아닌 창에서 명령이 돌고 있으면(vim·빌드) 끄는 순간 그 일을 잃는다.
            let agent = state.as_ref().is_some_and(|s| !matches!(s, crate::agent_state::AgentState::Unknown));
            if !agent && !server_panes.contains(id) {
                if let Some(name) = self.pid_busy(id) {
                    busy.push(BusyPane { surface: id.clone(), character, state: format!("running {name}") });
                }
            }
        }
        let jobs = kasa_socket::app_restart::jobs_dir().ok();
        Facts {
            schema: SCHEMA.into(),
            machine_id: kasa_mcp::board_service::local_id().unwrap_or_default(),
            label: kasa_mcp::machines::self_label(),
            os: std::env::consts::OS.into(),
            capability: CAPABILITY,
            app_path: exe.as_ref().and_then(|p| p.ancestors().nth(3)).map(|p| p.display().to_string()).unwrap_or_default(),
            pid: std::process::id(),
            running_exe: exe.as_ref().map(|p| p.display().to_string()).unwrap_or_default(),
            binary,
            install_pending,
            baking: baking(),
            busy,
            dirty_editors: self.dirty_docs(&PendingClose::Window).len() as u32,
            restorable_sessions: restorable,
            plain_shells: shells,
            registered_servers: server_panes.len() as u32,
            pet_alive: pet_alive(),
            active_job: jobs.and_then(|dir| kasa_socket::app_restart::active_job(&dir, now)),
            observed_at_ms: now,
        }
    }
}

/// 부팅 때 한 번 — 이 기기의 재시작 작업이 재기동을 기다리고 있었으면 도착을 적는다.
pub(crate) fn mark_restart_booted() {
    let (Ok(dir), Ok(machine)) = (kasa_socket::app_restart::jobs_dir(), kasa_mcp::board_service::local_id()) else {
        return;
    };
    if let Some(job) = kasa_socket::app_restart::mark_booted(
        &dir, &machine, std::process::id(), &kasa_mcp::machines::build_id(), kasa_socket::board::now_ms(),
    ) {
        eprintln!("[app-restart] booted for job {job}");
    }
}
