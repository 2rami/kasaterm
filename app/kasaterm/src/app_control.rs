use super::*;
use kasa_socket::app_control::{self as contract, Action, Request};
use kasa_socket::app_restart::{self as restart, Facts, Job, JobState};
use serde_json::{json, Value};
use std::sync::{mpsc, OnceLock};
use std::time::{Duration, Instant};

pub(crate) struct Prepared { facts: Facts, saved: Value }
pub(crate) struct PendingExit { pub at: Instant, request: Request, job: Job, baking: bool }
pub(crate) enum Reply { View(Value), Restart(Prepared) }
#[derive(Clone)]
pub(crate) enum Event {
    PrepareAction { deadline: Instant, epoch: u64, reply: mpsc::Sender<Result<(), String>> },
    Request { request: Request, baking: bool, deadline: Instant, reply: mpsc::Sender<Result<Reply, String>> },
    RestartReady { request: Request, job: Job, baking: bool, deadline: Instant, reply: mpsc::Sender<Result<Reply, String>> },
}
impl std::fmt::Debug for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("AppControl") }
}

static BOOTED: OnceLock<(String,u32)> = OnceLock::new();
static RESTORE_PICKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static RESTORE_MISSING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static RESTORE_FINISHED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub(crate) fn booted(job: String, panes: u32) { let _ = BOOTED.set((job,panes)); }

fn machine() -> anyhow::Result<String> { kasa_collab::board_service::local_id() }

fn screen(display: Option<u32>) -> anyhow::Result<Value> {
    let source = kasa_remote::screen_mirror::installed().ok_or_else(|| anyhow::anyhow!("screen unavailable"))?;
    capture_screen(source.as_ref(), display, &machine()?)
}

fn capture_screen(source: &dyn kasa_remote::screen_mirror::Source, display: Option<u32>, machine_id: &str) -> anyhow::Result<Value> {
    use base64::Engine;
    use kasa_remote::screen_mirror::{Ask, Denied};
    let displays = source.displays();
    let selected = match display {
        Some(id) => displays.iter().find(|d| d.id == id),
        None => displays.iter().find(|d| d.main).or_else(|| displays.first()),
    }.ok_or_else(|| anyhow::anyhow!("display unavailable"))?;
    let shot = source.capture(&Ask { display: selected.id, max_w: 1600, max_h: 1600, quality: 70, prev: 0 })
        .map_err(|e| anyhow::anyhow!(match e { Denied::Permission => "screen recording permission required", Denied::Unavailable => "screen unavailable" }))?
        .ok_or_else(|| anyhow::anyhow!("no fresh screen frame"))?;
    Ok(json!({"ok":true, "machine_id":machine_id, "observed_at_ms":kasa_socket::board::now_ms(),
        "display":selected.id, "displays":displays, "width":shot.width, "height":shot.height,
        "mime":"image/jpeg", "image_base64":base64::engine::general_purpose::STANDARD.encode(shot.jpeg)}))
}

pub(crate) fn prepare_action(proxy: &winit::event_loop::EventLoopProxy<UserEvent>) -> anyhow::Result<()> {
    let (reply, rx) = mpsc::channel();
    let deadline = Instant::now() + Duration::from_secs(5);
    proxy.send_event(UserEvent::AppControl(Event::PrepareAction {
        deadline, epoch: kasa_device::device_auth::change_epoch(), reply,
    })).map_err(|_| anyhow::anyhow!("app event loop gone"))?;
    rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|_| anyhow::anyhow!("app readiness timed out; no action performed"))?.map_err(anyhow::Error::msg)
}

pub(crate) fn request(proxy: &winit::event_loop::EventLoopProxy<UserEvent>, request: &Request) -> anyhow::Result<Value> {
    request.validate()?;
    static RESTART_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _restart = if request.action == Action::Restart { Some(RESTART_LOCK.try_lock().map_err(|_|anyhow::anyhow!("restart already preparing"))?) } else { None };
    if let Some(id) = &request.machine_id { anyhow::ensure!(id == &machine()?, "target identity changed"); }
    if request.action == Action::Screen { return screen(request.display); }
    let deadline = Instant::now() + Duration::from_secs(10);
    let ask = |event: Box<dyn FnOnce(mpsc::Sender<Result<Reply,String>>) -> Event + '_>| -> anyhow::Result<Reply> {
        let (tx, rx) = mpsc::channel();
        proxy.send_event(UserEvent::AppControl(event(tx))).map_err(|_| anyhow::anyhow!("app event loop gone"))?;
        rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| anyhow::anyhow!("app control timed out; inspect before retrying"))?.map_err(anyhow::Error::msg)
    };
    let baking = crate::app_restart::baking();
    let reply = ask(Box::new(|reply| Event::Request { request: request.clone(), baking, deadline, reply }) as Box<dyn FnOnce(_) -> Event>)?;
    match reply {
        Reply::View(value) => Ok(value),
        Reply::Restart(prepared) => {
            // A failed save must leave the old app running, even when its normal shutdown writer is best-effort.
            socket::write_session_state_checked(&prepared.saved)?;
            let facts = prepared.facts;
            let plan = request.expect.as_deref().unwrap_or_default();
            let job = Job { restore: true, restore_panes: App::count_panes(&prepared.saved) as u32, schema: restart::SCHEMA.into(), job_id: restart::job_id(plan, &facts.machine_id),
                plan_hash: plan.into(), machine_id: facts.machine_id.clone(), target_hash: restart::target_hash(&facts),
                old_pid: facts.pid, created_at_ms: kasa_socket::board::now_ms() };
            let dir = restart::jobs_dir()?;
            let (_, created) = restart::create_job(&dir, &job)?;
            if !created { return Ok(json!({"ok":true,"machine_id":facts.machine_id,"job_id":job.job_id,"status":restart::job_status(&dir,&job.job_id)?.state})); }
            let result = (|| {
                let spec = restart::HelperSpec { dir: dir.clone(), job_id: job.job_id.clone(), old_pid: facts.pid,
                    app_exe: facts.app_exe(), launch: restart::launch_command(&facts.os, &facts.app_path)?, exit_timeout_s:60, boot_timeout_s:90 };
                restart::spawn_helper(&spec)?;
                match ask(Box::new(|reply| Event::RestartReady { request: request.clone(), job: job.clone(), baking: crate::app_restart::baking(), deadline, reply }))? {
                    Reply::View(value) => Ok(value),
                    Reply::Restart(_) => anyhow::bail!("unexpected restart response"),
                }
            })();
            if result.is_err() { let _ = restart::advance(&dir, &job.job_id, JobState::Failed, "restart preparation or final view check failed; not exiting"); }
            result
        }
    }
}

fn progress_complete(progress: Option<&crate::restore_progress::RestoreProgress>) -> bool {
    progress.is_none_or(|p| p.built && p.failure.is_none() && p.ready >= p.expected)
}

impl App {
    fn control_ready(&self) -> bool {
        self.restore_prompt.is_none() && self.restore_applying.is_none()
            && !self.restoration_blocks_input() && self.pending_restores.is_empty()
    }

    fn control_state(&self, own_job: Option<&str>, baking: bool) -> anyhow::Result<(Value, Facts)> {
        let mut facts = self.restart_facts_with_baking(baking);
        if facts.active_job.as_deref() == own_job { facts.active_job = None; }
        let mut facts_json = serde_json::to_value(&facts)?;
        facts_json.as_object_mut().unwrap().remove("observed_at_ms");
        let ws = self.ws.try_lock().map_err(|_| anyhow::anyhow!("layout busy; retry inspect"))?;
        let active = ws.active_pane.clone();
        let tab = active.as_ref().and_then(|id| ws.panes.get(id)).and_then(|pane| pane.tabs.get(pane.active_tab).map(|tab|
            json!({"index":pane.active_tab,"title":tab.title,"surface":tab.pid,"url":tab.web().map(|w|w.url.clone()),
                "path":tab.preview_path.as_ref().map(|p|p.to_string_lossy())})));
        drop(ws);
        let extra_windows: Vec<_> = self.aux.terminals.iter().map(|w| json!({"kind":"terminal","surface":w.pane_id,"room":w.home_window,"focused":w.window.has_focus()}))
            .chain(self.aux.windows.iter().map(|w| json!({"kind":"document","focused":w.window.has_focus()}))).collect();
        let focused = self.window_focused || extra_windows.iter().any(|w|w["focused"] == true) || self.image_preview.focused() || self.media_preview.focused();
        let progress = self.restore_progress.as_ref().map(|p| json!({"expected":p.expected,"ready":p.ready,"failure":p.failure,"built":p.built,"background":p.background}));
        let missing = RESTORE_MISSING.load(std::sync::atomic::Ordering::Relaxed);
        let restore_error = self.restore_progress.as_ref().and_then(|p|p.failure.clone())
            .or_else(|| missing.then(|| "saved work missing".into()));
        let state = json!({"schema":"kasaterm-app-control/1","machine_id":facts.machine_id,
            "instance_id":kasa_collab::board_service::instance_id(),"auth_epoch":kasa_device::device_auth::change_epoch(),"version":env!("CARGO_PKG_VERSION"),
            "focused":focused,"main_window_focused":self.window_focused,"extra_windows":extra_windows,"minimized":self.window.as_ref().and_then(|w|w.is_minimized()),
            "room":self.active_window,"room_label":self.window_labels.get(self.active_window),"surface":active,"tab":tab,
            "conversation_view":active.as_ref().is_some_and(|id|self.chat_view_showing(id)),
            "image_preview_focused":self.image_preview.focused(),"media_preview_focused":self.media_preview.focused(),
            "restore":self.restore_prompt.as_ref().map(|_|self.restore_picker.control_view()),
            "restoring":self.restore_applying.is_some() || !self.pending_restores.is_empty()
                || self.restore_progress.as_ref().is_some_and(|p| p.failure.is_none() && !progress_complete(Some(p))),
            "restore_progress":progress,"restart":facts_json,
            "ready":self.control_ready(),
            "restore_complete":self.control_ready() && !missing && progress_complete(self.restore_progress.as_ref()),
            "restore_error":restore_error,
            "before_action":"restore opens automatically on an agent mutation; inspect until ready, then resolve fresh pane addresses",
            "actions":{"restore":self.restore_prompt.is_some() || self.restore_progress.as_ref().is_some_and(|p|p.failure.is_some()),"restart":self.control_ready(),"update-check":true},
            "screen":"app screen captures the actual monitor; focused=false means this app is not in front"});
        Ok((state,facts))
    }

    fn validate_control(&self, request: &Request, own_job: Option<&str>, baking: bool) -> anyhow::Result<(Value, Facts)> {
        let (state, facts) = self.control_state(own_job, baking)?;
        contract::check_revision(&state, request.expect.as_deref().unwrap_or_default(), kasa_socket::board::now_ms())?;
        Ok((state,facts))
    }

    pub(crate) fn app_control_event(&mut self, event: &Event) {
        if let Event::PrepareAction { deadline, epoch, reply } = event {
            let result = (|| -> anyhow::Result<()> {
                anyhow::ensure!(Instant::now() < *deadline && *epoch == kasa_device::device_auth::change_epoch(), "app readiness expired; no action performed");
                if self.restore_prompt.is_some() {
                    anyhow::ensure!(self.restore_picker.control_view()["selected_count"].as_u64().unwrap_or(0) > 0,
                        "restore_selection_empty: 복원할 작업을 선택해 주세요. 조작은 실행하지 않았어요. app inspect로 확인하세요");
                    self.restore_dialog_pick(RestoreBtn::Restore);
                    if let Some(window) = &self.window { window.request_redraw(); }
                    anyhow::bail!("app_restoring: 저장된 작업 복원을 시작했어요. 조작은 실행하지 않았어요. app inspect로 복원 완료와 새 주소를 확인한 뒤 다시 요청하세요");
                }
                anyhow::ensure!(self.control_ready() || self.restore_progress.as_ref().is_none_or(|p|p.failure.is_none()),
                    "app_restore_failed: 복원이 끝나지 않았어요. app inspect로 확인한 뒤 app restore --expect REVISION으로 다시 시도하세요");
                anyhow::ensure!(self.control_ready(),
                    "app_restoring: 복원 중이에요. 조작은 실행하지 않았어요. app inspect로 완료를 확인하세요");
                Ok(())
            })().map_err(|e|e.to_string());
            let _ = reply.send(result);
            return;
        }
        let (request, baking, deadline, reply) = match event {
            Event::Request {request,baking,deadline,reply} | Event::RestartReady {request,baking,deadline,reply,..} => (request,*baking,deadline,reply),
            Event::PrepareAction { .. } => unreachable!(),
        };
        let result = (|| -> anyhow::Result<Reply> {
            anyhow::ensure!(Instant::now() < *deadline, "app request expired");
            request.validate()?;
            if let Some(id) = &request.machine_id { anyhow::ensure!(id == &machine()?, "target identity changed"); }
            if let Event::RestartReady { job, .. } = event {
                let (_, facts) = self.validate_control(request, Some(&job.job_id), baking)?;
                check_restart(&facts, request.interrupt)?;
                self.control_exit = Some(PendingExit { at: Instant::now() + Duration::from_millis(300), request:request.clone(), job:job.clone(), baking });
                return Ok(Reply::View(json!({"ok":true,"machine_id":facts.machine_id,"job_id":job.job_id,"status":"restarting","restore":true})));
            }
            if request.action == Action::Inspect {
                let (mut state,_) = self.control_state(None, baking)?;
                let now = kasa_socket::board::now_ms();
                state["revision"] = json!(contract::revision(&state, now));
                state["observed_at_ms"] = json!(now);
                state["ok"] = json!(true);
                return Ok(Reply::View(state));
            }
            let (_, facts) = self.validate_control(request, None, baking)?;
            match request.action {
                Action::Restore => {
                    if self.restore_prompt.is_some() {
                        anyhow::ensure!(self.restore_picker.control_view()["selected_count"].as_u64().unwrap_or(0)>0, "no saved work selected");
                        self.restore_dialog_pick(RestoreBtn::Restore);
                    } else {
                        anyhow::ensure!(self.restore_progress.as_ref().is_some_and(|p|p.failure.is_some()), "no restore dialog or failed restoration");
                        self.retry_restore();
                    }
                    if let Some(window) = &self.window { window.request_redraw(); }
                    Ok(Reply::View(json!({"ok":true,"machine_id":facts.machine_id,"status":"restoring"})))
                }
                Action::Restart => {
                    anyhow::ensure!(self.control_ready(), "finish restoration before restarting");
                    check_restart(&facts, request.interrupt)?;
                    let saved = self.session_state_json().ok_or_else(|| anyhow::anyhow!("session snapshot unavailable"))?;
                    Ok(Reply::Restart(Prepared {facts,saved}))
                }
                Action::UpdateCheck => {
                    self.check_for_updates_now();
                    Ok(Reply::View(json!({"ok":true,"machine_id":facts.machine_id,"status":"update-check-opened"})))
                }
                _ => anyhow::bail!("unsupported app action"),
            }
        })().map_err(|e|e.to_string());
        let _ = reply.send(result);
    }

    pub(crate) fn tick_control_exit(&mut self) -> bool {
        if !self.control_exit.as_ref().is_some_and(|exit| Instant::now() >= exit.at) { return false; }
        let exit = self.control_exit.take().unwrap();
        let validation = self.validate_control(&exit.request, Some(&exit.job.job_id), exit.baking)
            .and_then(|(_,facts)| check_restart(&facts,exit.request.interrupt));
        if validation.is_ok() { return true; }
        std::thread::spawn(move || {
            if let Ok(dir) = restart::jobs_dir() { let _ = restart::advance(&dir,&exit.job.job_id,JobState::Failed,"view changed before exit; not exiting"); }
        });
        false
    }

    pub(crate) fn tick_control_restore(&mut self) {
        use std::sync::atomic::Ordering;
        let Some((job,expected)) = BOOTED.get() else { return };
        if RESTORE_FINISHED.load(Ordering::Relaxed) { return; }
        if !RESTORE_PICKED.swap(true, Ordering::Relaxed) {
            if self.restore_prompt.is_some() {
                self.restore_dialog_pick(RestoreBtn::Restore);
            } else if *expected > 0 && self.restore_applying.is_none() && self.restore_progress.is_none() {
                RESTORE_MISSING.store(true, Ordering::Relaxed);
            }
        }
        let failure = self.restore_progress.as_ref().and_then(|p|p.failure.clone())
            .or_else(|| RESTORE_MISSING.load(Ordering::Relaxed).then(|| "saved work missing".into()));
        if failure.is_none() && (self.restore_prompt.is_some() || self.restore_applying.is_some() || !self.pending_restores.is_empty() || self.restoration_blocks_input()) { return; }
        if self.restore_progress.as_ref().is_some_and(|p| p.ready < p.expected && p.failure.is_none()) { return; }
        if RESTORE_FINISHED.swap(true, Ordering::Relaxed) { return; }
        let job = job.clone();
        std::thread::spawn(move || {
            if let Ok(dir) = restart::jobs_dir() {
                let (state,note) = if failure.is_some() { (JobState::Failed,"app restarted; restoration incomplete") } else { (JobState::Verified,"app restarted and saved work restored") };
                let _ = restart::advance(&dir,&job,state,note);
            }
        });
    }
}

fn check_restart(facts: &Facts, interrupt: bool) -> anyhow::Result<()> {
    let refusals = restart::refusals(&facts.machine_id, facts, kasa_socket::board::now_ms());
    if let Some(refusal) = refusals.iter().find(|r| !(interrupt && matches!(r,restart::Refusal::BusyStudents {..}))) {
        anyhow::bail!("{}",refusal.message());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_built_background_layout_is_not_complete_until_its_students_are_ready() {
        let mut progress = crate::restore_progress::RestoreProgress::new(json!({
            "active_session":0,"sessions":[{"windows":[{"leaf":{"pane_id":"%1"}},{"leaf":{"pane_id":"%2"}}]}]
        }));
        assert_eq!(progress.expected, 2);
        progress.built = true;
        progress.background = true;
        progress.ready = 1;
        assert!(!progress_complete(Some(&progress)));
        progress.ready = 2;
        progress.failure = Some("resume failed".into());
        assert!(!progress_complete(Some(&progress)));
        progress.failure = None;
        assert!(progress_complete(Some(&progress)));
        progress.built = false;
        assert!(!progress_complete(Some(&progress)));
        assert!(progress_complete(None));
    }

    #[test]
    fn screen_selection_and_permission_fail_without_fallback_or_permission_prompts() {
        use kasa_remote::screen_mirror::{Source, Display, Ask, Shot, Denied};
        struct Fixture;
        impl Source for Fixture {
            fn displays(&self) -> Vec<Display> { vec![Display {id:7,width:1920,height:1080,main:true}] }
            fn capture(&self, ask: &Ask) -> Result<Option<Shot>,Denied> {
                assert_eq!(ask.display,7);
                Err(Denied::Permission)
            }
            fn request_permission(&self) { panic!("read action must not trigger a permission dialog") }
        }
        assert!(capture_screen(&Fixture,Some(999),"fixture").unwrap_err().to_string().contains("display unavailable"));
        assert!(capture_screen(&Fixture,None,"fixture").unwrap_err().to_string().contains("permission required"));
    }

    #[test]
    fn interrupt_only_allows_known_work_interruption_not_documents_or_install_changes() {
        let mut facts = Facts { schema:restart::SCHEMA.into(), machine_id:"fixture".into(), os:"macos".into(),
            capability:restart::CAPABILITY, app_path:"/Applications/kasaterm.app".into(),
            running_exe:"/Applications/kasaterm.app/Contents/MacOS/kasaterm".into(),
            observed_at_ms:kasa_socket::board::now_ms(), ..Default::default() };
        facts.busy.push(restart::BusyPane {surface:"%1".into(),character:"fixture".into(),state:"working".into()});
        assert!(check_restart(&facts,false).is_err());
        assert!(check_restart(&facts,true).is_ok());
        facts.dirty_editors=1;
        assert!(check_restart(&facts,true).is_err());
        facts.dirty_editors=0;
        facts.install_pending=Some(restart::PendingInstall::default());
        assert!(check_restart(&facts,true).is_err());
        facts.install_pending=None;
        facts.active_job=Some("other".into());
        assert!(check_restart(&facts,true).is_err());
    }
}

// The isolated app assigns its own machine ID; a relaunch fixture must bind after that ID exists.
pub(crate) fn prepare_restore_fixture(dir: &std::path::Path, machine: &str) {
    if !crate::server_restore::verification_restore_fixture() { return; }
    let Ok(expected) = std::env::var("KASATERM_TEST_CONTROL_RESTORE").unwrap_or_default().parse::<u32>() else { return };
    if expected > 16 { return; }
    let job = Job {restore:true,restore_panes:expected,schema:restart::SCHEMA.into(),job_id:"rscontrolfixture".into(),
        plan_hash:"fixture".into(),machine_id:machine.into(),target_hash:"fixture".into(),old_pid:0,created_at_ms:kasa_socket::board::now_ms()};
    if restart::create_job(dir,&job).is_ok_and(|(_,created)|created) {
        for state in [JobState::HelperStarted,JobState::Exited,JobState::Relaunching,JobState::Launched] {
            let _=restart::advance(dir,&job.job_id,state,"isolated relaunch fixture");
        }
    }
}
