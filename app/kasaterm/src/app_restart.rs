//! 앱 재시작 — 이 기기의 사실을 GUI 스레드에서 모은다. 계획·거부·작업 기록·도우미는
//! `kasa_socket::app_restart`, 절차는 `docs/app-restart.md`.
//!
//! 실행은 나쵸가 쥔 승인으로만 된다 — 조종 기기는 나쵸 `consume`(서버 1회), 대상은 나쵸에서 읽은 승인과
//! 자기 사실로 판정한다(`kasa_socket::app_restart::{authorize_run, authorize_target}`).

use super::*;
use kasa_socket::app_restart::{ApprovalView, Authority, BinaryId, BusyPane, Facts, HelperSpec, JobRequest, PendingInstall, CAPABILITY, SCHEMA};

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

/// 나쵸 승인 창구. 이 기기에 나쵸 앱 키가 있으면 나쵸에 직접 묻고, 없으면 승인을 쥔 기기(명부의 machine_id)의
/// 카사텀이 **읽기만** 대신해 준다 — 키는 그 기기 밖으로 안 나가고, 소비는 키가 있는 조종 기기에서만 된다.
pub(crate) struct NachoAuthority {
    relay: Option<String>,
}

impl NachoAuthority {
    pub(crate) fn local() -> Self {
        Self { relay: None }
    }

    pub(crate) fn for_request(authority_machine: &str) -> Result<Self, String> {
        if kasa_mcp::nacho_app_target().is_ok() {
            return Ok(Self::local());
        }
        let valid = !authority_machine.is_empty() && authority_machine.len() <= 128
            && authority_machine.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        let machine = valid.then(|| kasa_mcp::machines::find_route(&format!("~{authority_machine}"))).flatten()
            .ok_or_else(|| format!("승인을 쥔 기기({authority_machine})가 이 기기 명부에 없다"))?;
        Ok(Self { relay: Some(machine.base) })
    }
}

/// 나쵸 앱 창구의 승인 응답 — 200 이면 `approval`, 아니면 나쵸가 적은 `error` 낱말(already_used·scope_changed…).
fn nacho_approval(method: &str, path: &str, body: Option<serde_json::Value>) -> Result<ApprovalView, String> {
    let bytes = body.map(|b| b.to_string().into_bytes());
    let (status, raw) = crate::nacho_tasks::app_request(method, path, bytes.as_deref())?;
    let value: serde_json::Value = serde_json::from_slice(&raw).unwrap_or_default();
    if status == 200 {
        return serde_json::from_value(value["approval"].clone()).map_err(|_| "나쵸 승인 응답을 읽지 못했다".to_string());
    }
    Err(value["error"].as_str().map(str::to_string).unwrap_or_else(|| format!("나쵸가 답하지 못했다({status})")))
}

impl Authority for NachoAuthority {
    fn get(&self, approval_id: &str) -> Result<ApprovalView, String> {
        if !kasa_socket::app_restart::valid_approval_id(approval_id) {
            return Err("approval id 모양이 아니다".into());
        }
        match &self.relay {
            None => nacho_approval("GET", &format!("/api/app/approvals/{approval_id}"), None),
            Some(base) => kasa_mcp::remote::remote_get_json(base, &format!("/app/restart/approvals/{approval_id}"))
                .map_err(|e| e.to_string())
                .and_then(|v| serde_json::from_value(v).map_err(|e| e.to_string())),
        }
    }

    fn consume(&self, approval_id: &str, scope: &serde_json::Value, consumer: &str) -> Result<ApprovalView, String> {
        if self.relay.is_some() {
            return Err("승인 소비는 나쵸 앱 키가 있는 조종 기기에서만 한다".into());
        }
        if !kasa_socket::app_restart::valid_approval_id(approval_id) {
            return Err("approval id 모양이 아니다".into());
        }
        nacho_approval("POST", &format!("/api/app/approvals/{approval_id}/consume"),
            Some(serde_json::json!({"scope": scope, "consumer_machine_id": consumer})))
    }
}

/// 대상 쪽 수락 — 나쵸 승인과 이 기기의 지금 사실로 판정하고, 통과하면 작업을 적고 도우미를 띄운 뒤 앱 종료를 청한다.
/// 같은 작업이 다시 오면 적힌 것을 돌려줄 뿐 도우미를 또 띄우지 않는다.
pub(crate) fn accept_restart(facts: &Facts, req: &JobRequest, proxy: &winit::event_loop::EventLoopProxy<UserEvent>) -> Result<serde_json::Value, String> {
    let authority = NachoAuthority::for_request(&req.authority)?;
    let dir = kasa_socket::app_restart::jobs_dir().map_err(|e| e.to_string())?;
    let created = kasa_socket::app_restart::accept_job(req, facts, &authority, &dir, kasa_socket::board::now_ms())?;
    if created {
        let spec = HelperSpec {
            dir,
            job_id: req.job.job_id.clone(),
            old_pid: facts.pid,
            app_exe: facts.app_exe(),
            launch: kasa_socket::app_restart::launch_command(&facts.app_path).map_err(|e| e.to_string())?,
            exit_timeout_s: 60,
            boot_timeout_s: 90,
        };
        kasa_socket::app_restart::spawn_helper(&spec).map_err(|e| e.to_string())?;
        proxy.send_event(UserEvent::RestartExit(req.job.job_id.clone())).map_err(|_| "app event loop is gone".to_string())?;
    }
    Ok(serde_json::json!({"ok": true, "job_id": req.job.job_id, "created": created}))
}
