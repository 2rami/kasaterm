//! 앱 업데이트 창구 — 이 기기 쪽 수락·받기·적용. 계약·검증·도우미는 `kasa_socket::app_update`, 절차는
//! `docs/app-update.md`.
//!
//! 설치 실행은 `KASATERM_APP_UPDATE=on` 일 때만 된다(기본 꺼짐) — 나쵸에 `kasaterm_update` 승인 계약이 생기기 전까지는
//! 이 앱이 받기조차 하지 않는다.

use super::*;
use kasa_socket::app_restart::Facts;
use kasa_socket::app_update::{self as update, Device, State, Step, SystemEffects, UpdateRequest};

pub(crate) fn enabled() -> bool {
    #[cfg(target_os = "macos")]
    if crate::macos_sparkle::owns_installation() { return false; }
    update::install_enabled(&|k| std::env::var(k).ok())
}

/// 받은 파일 자리. 격리 리그는 그 collab 루트 아래로 간다.
fn cache_dir() -> Result<std::path::PathBuf, String> {
    if let Some(root) = kasa_socket::isolated_collab_root() {
        return Ok(root.join("app-update-cache"));
    }
    kasa_socket::home_dir().map(|h| h.join("Library/Caches/kasaterm/updates")).ok_or_else(|| "home directory unavailable".into())
}

/// 한 번에 한 작업만 몬다 — 다시 보내기가 겹쳐도 받기·도우미가 둘 생기지 않는다.
fn driving() -> &'static Mutex<Option<String>> {
    static SLOT: std::sync::OnceLock<Mutex<Option<String>>> = std::sync::OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

/// 대상 쪽 수락 — 나쵸 승인과 이 기기의 지금 사실로 판정하고, 통과하면 작업을 적고 받기·준비·적용을 뒤에서 시작한다.
/// 같은 작업이 다시 오면 적힌 것을 돌려주고, 끊겼거나 기다리던 작업이면 이어서 몬다.
pub(crate) fn accept_update(facts: &Facts, req: &UpdateRequest, proxy: &winit::event_loop::EventLoopProxy<UserEvent>) -> Result<serde_json::Value, String> {
    let authority = crate::app_restart::NachoAuthority::for_relay(&update::UPDATE_RELAY, &req.authority)?;
    let dir = update::jobs_dir().map_err(|e| e.to_string())?;
    let now = kasa_socket::board::now_ms();
    let (status, created) = update::accept(req, facts, crate::version::CURRENT, &authority, enabled(), &dir, now)?;
    if status.state < State::Armed {
        start_driving(dir, req.clone(), authority, facts.app_path.clone(), proxy.clone());
    }
    Ok(serde_json::json!({"ok": true, "job_id": req.job.job_id, "created": created, "state": status.state.word()}))
}

fn start_driving(
    dir: std::path::PathBuf,
    req: UpdateRequest,
    authority: crate::app_restart::NachoAuthority,
    app_path: String,
    proxy: winit::event_loop::EventLoopProxy<UserEvent>,
) {
    let job = req.job.clone();
    {
        let mut slot = driving().lock().unwrap();
        if slot.is_some() {
            return;
        }
        *slot = Some(job.job_id.clone());
    }
    std::thread::spawn(move || {
        let asker = proxy.clone();
        // 바쁜 학생·미저장 편집기는 GUI 스레드만 안다 — 받은 뒤 갈아 끼우기 직전에 다시 묻는다.
        let facts_now = move || -> Result<Facts, String> {
            let (tx, rx) = std::sync::mpsc::channel();
            asker.send_event(UserEvent::SocketRestartFacts(tx)).map_err(|_| "app event loop is gone".to_string())?;
            rx.recv_timeout(std::time::Duration::from_secs(2)).map_err(|_| "app did not answer facts in time".to_string())
        };
        let result = (|| {
            let effects = SystemEffects;
            let dev = Device {
                dir,
                cache: cache_dir()?,
                installed: std::path::PathBuf::from(&app_path),
                effects: &effects,
                public_key: update::ED_PUBLIC_KEY,
                enabled: enabled(),
                launch: kasa_socket::app_restart::launch_command(&app_path).map_err(|e| e.to_string())?,
                exit_timeout_s: 60,
                boot_timeout_s: 90,
            };
            // 기다리는 사이 주인이 승인을 거뒀거나 만료됐으면 도우미에 넘기지 않는다.
            let approval_now = || update::usable_approval(&authority, &req.approval_id, kasa_socket::board::now_ms()).map(|_| ());
            update::drive(&dev, &job, &facts_now, &approval_now, &kasa_socket::board::now_ms)
        })();
        *driving().lock().unwrap() = None;
        match result {
            Ok(Step::Armed(_)) => {
                let _ = proxy.send_event(UserEvent::UpdateExit(job.job_id.clone()));
            }
            Ok(Step::Waiting(why)) => eprintln!("[app-update] {} waiting: {why}", job.job_id),
            Err(why) => eprintln!("[app-update] {}: {why}", job.job_id),
        }
    });
}

/// 부팅 때 한 번 — 이 기기의 업데이트가 갈아 끼운 뒤 새 판을 기다리고 있었으면 도착과 빌드 표식을 적는다.
pub(crate) fn mark_update_booted() {
    let (Ok(dir), Ok(machine)) = (update::jobs_dir(), kasa_mcp::board_service::local_id()) else {
        return;
    };
    if let Some((job, end)) = update::mark_booted(&dir, &machine, std::process::id(), &kasa_mcp::machines::build_id(), kasa_socket::board::now_ms()) {
        eprintln!("[app-update] booted for job {job}: {}", end.word());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kasa_socket::app_restart::Authority;
    use kasa_socket::app_update::{Outcome, Reach, Rollout, RunPolicy, Status, Transport};
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// 기기 쪽 흉내 — 받기는 실제 `accept`(나쵸에서 읽은 승인으로 판정)이고, 받은 뒤 진행만 한 걸음씩 적는다.
    struct Fleet {
        devs: RefCell<HashMap<String, (Facts, std::path::PathBuf, bool)>>,
        nacho: crate::app_restart::NachoAuthority,
    }

    const STEPS: [State; 10] = [State::Fetched, State::Checked, State::Staged, State::Armed, State::HelperStarted, State::Exited,
                                State::Swapped, State::Launched, State::Booted, State::Done];

    impl Transport for Fleet {
        fn facts(&self, id: &str) -> Result<Facts, String> {
            let devs = self.devs.borrow();
            let (f, dir, done) = devs.get(id).ok_or("없는 기기")?;
            let mut f = f.clone();
            f.observed_at_ms = kasa_socket::board::now_ms();
            if *done {
                let job = std::fs::read_dir(dir).unwrap().flatten()
                    .find_map(|e| e.file_name().to_string_lossy().strip_suffix(".json").map(str::to_string)).unwrap();
                f.pid += 1000;
                f.binary.build = update::status(dir, &job).unwrap().job.build[..8].to_string();
            }
            Ok(f)
        }
        fn start(&self, id: &str, req: &UpdateRequest) -> Result<serde_json::Value, Reach> {
            let facts = self.facts(id).map_err(Reach::Unreachable)?;
            let dir = self.devs.borrow()[id].1.clone();
            update::accept(req, &facts, "0.2.0", &self.nacho, true, &dir, kasa_socket::board::now_ms())
                .map(|(s, created)| serde_json::json!({"state": s.state.word(), "created": created}))
                .map_err(Reach::Refused)
        }
        fn status(&self, id: &str, job_id: &str) -> Result<Status, String> {
            let mut devs = self.devs.borrow_mut();
            let (_, dir, done) = devs.get_mut(id).ok_or("없는 기기")?;
            let now = update::status(dir, job_id).map_err(|e| e.to_string())?;
            if let Some(next) = STEPS.iter().find(|s| **s > now.state).filter(|_| !now.state.terminal()) {
                update::advance(dir, job_id, *next, "interop").map_err(|e| e.to_string())?;
                *done = *next == State::Done;
            }
            update::status(dir, job_id).map_err(|e| e.to_string())
        }
    }

    /// 나쵸 레포의 실제 승인 서버(임시 폴더·가짜 키, update 창구)에 이 앱의 실제 나쵸 클라이언트로 붙는다 — 조종 쪽 러너가 실제 HTTP 로
    /// 한 번 소비하고, 기기 쪽 판정이 나쵸에서 읽은 승인으로 돌고, 소비 뒤 창(대상 × 35분)·두 번째 소비 거절·기록으로 다시 굴리기·
    /// 주인이 거둔 승인·창구 닫힘·틀린 키를 본다. `scripts/nacho-update-interop.sh` 가 서버를 띄우고 env 를 걸어 부른다.
    #[test]
    #[ignore]
    fn nacho_update_round_trip_against_isolated_nacho() {
        let case: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(std::env::var("KASATERM_UPDATE_INTEROP").expect("KASATERM_UPDATE_INTEROP")).unwrap()).unwrap();
        let rollout: Rollout = serde_json::from_value(case["rollout"].clone()).unwrap();
        assert_eq!(update::check_rollout(&rollout), Ok(()), "파이썬이 지은 rollout 을 러스트가 그대로 받는다");
        let work = std::env::temp_dir().join(format!("kasa-update-interop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&work);
        let devs = rollout.jobs.iter().map(|j| {
            let f: Facts = serde_json::from_value(case["facts"][&j.machine_id].clone()).unwrap();
            assert_eq!(kasa_socket::app_restart::target_hash(&f), j.target_hash, "파이썬 target_hash = 러스트");
            let dir = work.join(&j.machine_id[..8]);
            std::fs::create_dir_all(&dir).unwrap();
            (j.machine_id.clone(), (f, dir, false))
        }).collect();
        let fleet = Fleet { devs: RefCell::new(devs), nacho: crate::app_restart::NachoAuthority::local() };
        let nacho = crate::app_restart::NachoAuthority::local();
        let id = |k: &str| case["ids"][k].as_str().unwrap().to_string();
        let main = id("main");
        let policy = RunPolicy { poll_every: std::time::Duration::from_millis(1), ..RunPolicy::default() };
        let saved: RefCell<Option<update::Grant>> = RefCell::new(None);
        let go = |grant: Option<&update::Grant>| update::run(&rollout, &main, &nacho, &fleet, &policy, grant,
            &|g| { *saved.borrow_mut() = Some(g.clone()); Ok(()) }, &kasa_socket::board::now_ms, &std::thread::sleep);
        let words = |out: &[(String, Outcome)]| out.iter().map(|(_, o)| serde_json::to_value(o).unwrap()["outcome"].as_str().unwrap().to_string()).collect::<Vec<_>>();

        match case["mode"].as_str().unwrap() {
            "off" => {
                assert_eq!(nacho.get(&main).unwrap_err(), "no_approval", "update 창구가 닫혀 있으면 승인된 요청도 못 읽는다");
                let out = go(None);
                assert!(out.iter().all(|(_, o)| matches!(o, Outcome::Skipped { reason } if reason.contains("no_approval"))), "{out:?}");
                let _ = std::fs::remove_dir_all(&work);
                return;
            }
            "bad_key" => {
                assert_eq!(nacho.get(&main).unwrap_err(), "bad_token");
                let _ = std::fs::remove_dir_all(&work);
                return;
            }
            mode => assert_eq!(mode, "on"),
        }

        let before = nacho.get(&main).unwrap();
        assert_eq!(before.scope_hash, rollout.approval_scope_hash, "나쵸 정규화 해시 = 러스트·파이썬");
        assert!(update::usable_approval(&nacho, &main, kasa_socket::board::now_ms()).unwrap_err().contains("소비"), "소비 전엔 기기가 안 받는다");
        let out = go(None);
        assert_eq!(words(&out), ["updated", "handed_off"], "{out:?}");
        let grant = saved.borrow().clone().expect("소비 기록");
        let after = nacho.get(&main).unwrap();
        assert_eq!(after.consumed_by.as_deref(), Some(rollout.controller()));
        assert_eq!(after.expires_at_ms, after.consumed_at_ms.unwrap() + rollout.jobs.len() as u64 * 35 * 60_000, "소비 뒤 창 = 대상 × 35분");
        assert_eq!(nacho.consume(&main, &rollout.approval_scope, rollout.controller()).unwrap_err(), "already_used");
        let again = go(Some(&grant));
        assert_eq!(words(&again), ["updated", "handed_off"], "기록으로 다시 굴리면 읽기만 한다 · {again:?}");
        assert!(nacho.consume(&id("pending"), &rollout.approval_scope, rollout.controller()).unwrap_err().starts_with("not_approved"));

        let revoke = std::env::var("KASATERM_UPDATE_INTEROP_REVOKE").expect("KASATERM_UPDATE_INTEROP_REVOKE");
        let st = std::process::Command::new("/bin/sh").arg(&revoke).arg(&main).status().unwrap();
        assert!(st.success(), "나쵸 revoke");
        assert_eq!(nacho.get(&main).unwrap().state, "revoked");
        let last = rollout.jobs.last().unwrap();
        let req = UpdateRequest { job: last.clone(), approval_id: main.clone(), authority: rollout.controller().into() };
        assert!(matches!(fleet.start(&last.machine_id, &req), Err(Reach::Refused(why)) if why.contains("revoked")), "거둔 승인이면 기기가 다시 보내기를 거절");
        let stopped = go(Some(&grant));
        assert!(stopped.iter().all(|(_, o)| matches!(o, Outcome::Skipped { reason } if reason.contains("revoked"))), "{stopped:?}");
        let _ = std::fs::remove_dir_all(&work);
    }
}
