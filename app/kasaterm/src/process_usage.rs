use crate::{App, Arc, ClosedPane, Instant, Result, UserEvent};
use std::sync::Weak;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Identity {
    pub pid: u32,
    pub started: u64,
    pub uid: u32,
    pub exe: String,
}

#[derive(Clone, Debug)]
pub(crate) struct Owner {
    pub surface: String,
    pub key: String,
    pub session: Weak<kasa_pty::PtySession>,
    pub shell: u32,
    pub label: String,
    pub closed_record: Option<u64>,
    pub needs_open: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct Process {
    pub id: Identity,
    pub ppid: u32,
    pub root: u32,
    pub name: String,
    pub cpu: f32,
    pub rss: u64,
    pub cwd: Option<String>,
    pub owner: Option<Owner>,
}

pub(crate) fn descendants<'a>(rows: &'a [Process], root: &Identity) -> Vec<&'a Process> {
    if !rows.iter().any(|row| row.id == *root) { return Vec::new(); }
    let mut ids = std::collections::HashSet::from([root.pid]);
    loop {
        let before = ids.len();
        for row in rows { if ids.contains(&row.ppid) { ids.insert(row.id.pid); } }
        if before == ids.len() { break; }
    }
    let mut out: Vec<_> = rows.iter().filter(|row| ids.contains(&row.id.pid)).collect();
    out.sort_by_key(|row| (row.id.pid != root.pid, row.ppid, row.id.pid));
    out
}

fn protected(pid: u32, rows: &[Process], me: u32) -> bool {
    if pid <= 1 { return true; }
    let mut at = me;
    for _ in 0..64 {
        if pid == at { return true; }
        let Some(row) = rows.iter().find(|row| row.id.pid == at) else { break };
        if row.ppid <= 1 || row.ppid == at { break; }
        at = row.ppid;
    }
    false
}

pub(crate) fn stoppable(row: &Process, rows: &[Process]) -> bool {
    cfg!(any(unix, windows)) && row.id.started != 0 && row.id.uid == crate::input::current_uid()
        && !protected(row.id.pid, rows, std::process::id())
        && !row.owner.as_ref().is_some_and(|owner| owner.shell == row.id.pid && owner.needs_open)
}

fn validate<'a>(expected: &Process, rows: &'a [Process], me: u32, uid: u32) -> Result<&'a Process, String> {
    if expected.id.started == 0 || expected.id.uid != uid || protected(expected.id.pid, rows, me) {
        return Err("이 실행은 여기서 종료할 수 없어요".into());
    }
    rows.iter().find(|row| row.id == expected.id && row.root == expected.root && row.ppid == expected.ppid)
        .ok_or_else(|| "선택한 실행이 끝났거나 바뀌었어요. 목록을 다시 확인해 주세요".into())
}

fn terminate(expected: &Process) -> Result<(), String> {
    let rows = crate::input::fresh_usage_processes().ok_or("실행 상태를 다시 확인하지 못했어요")?;
    validate(expected, &rows, std::process::id(), crate::input::current_uid())?;
    if crate::input::usage_process_birth(expected.id.pid) != Some(expected.id.started) {
        return Err("선택한 실행이 바뀌었어요".into());
    }
    #[cfg(unix)]
    {
        let result = unsafe { libc::kill(expected.id.pid as i32, libc::SIGTERM) };
        if result != 0 { return Err(format!("종료 요청 실패: {}", std::io::Error::last_os_error())); }
        Ok(())
    }
    #[cfg(windows)]
    {
        terminate_windows(expected)
    }
    #[cfg(not(any(unix, windows)))]
    Err("이 환경에서는 실행 창에서 종료해 주세요".into())
}

/// Windows 에는 SIGTERM 이 없다. 콘솔 프로그램에 보낼 수 있는 Ctrl+C 는 그 콘솔의 모든
/// 프로세스(칸의 셸·학생까지)에 한꺼번에 가서 「고른 실행만」을 지킬 수 없다 — 그래서 고른
/// 하나만 `TerminateProcess` 로 끝낸다. 시작 시각을 **끌 핸들로 다시** 대조한다: 위의 대조와
/// 이 핸들을 여는 사이에 그 PID 가 끝나고 다른 프로세스에 다시 쓰이면 엉뚱한 것을 끈다.
#[cfg(windows)]
fn terminate_windows(expected: &Process) -> Result<(), String> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, TerminateProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
    };
    // SAFETY: 연 핸들은 여기서 닫고, 조회가 성공했을 때만 값을 읽는다.
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION, 0, expected.id.pid);
        if handle.is_null() {
            return Err(format!("종료 요청 실패: {}", std::io::Error::last_os_error()));
        }
        let (mut created, mut exited, mut kernel, mut user): (FILETIME, FILETIME, FILETIME, FILETIME) = std::mem::zeroed();
        let same = GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) != 0
            && (((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64) == expected.id.started;
        let result = if !same {
            Err("선택한 실행이 바뀌었어요".to_string())
        } else if TerminateProcess(handle, 1) == 0 {
            Err(format!("종료 요청 실패: {}", std::io::Error::last_os_error()))
        } else {
            Ok(())
        };
        CloseHandle(handle);
        result
    }
}

fn backfill_history(live: &mut serde_json::Value, saved: &serde_json::Value, last: &std::collections::HashMap<String, (String, String)>) {
    fn one(live: &mut serde_json::Value, saved: &serde_json::Value, last: &std::collections::HashMap<String, (String, String)>) {
        let mut inferred = None;
        if live["session_id"].as_str().is_none_or(str::is_empty) {
            if let Some((name, sid)) = live["pane_id"].as_str().and_then(|pid| last.get(pid)).filter(|(_, sid)| !sid.is_empty()) {
                live["session_id"] = serde_json::json!(sid);
                live["character"] = serde_json::json!(name);
                inferred = Some(sid.as_str());
            }
        }
        let old = std::iter::once(saved).chain(saved["tabs"].as_array().into_iter().flatten()).find(|old| {
            live["surface_key"].as_str().is_some_and(|key| old["surface_key"].as_str() == Some(key))
        });
        if let Some(old) = old {
            if live["session_id"].as_str().is_none_or(str::is_empty) || live["session_id"] == old["session_id"] {
                for field in ["session_id", "was_agent", "character", "model", "effort", "cwd"] {
                    if live[field].is_null() || live[field].as_str() == Some("") {
                        if let Some(value) = old.get(field) { live[field] = value.clone(); }
                    }
                }
            }
        }
        if let Some(sid) = inferred.filter(|_| live["was_agent"].as_str().is_none_or(str::is_empty)) {
            live["was_agent"] = serde_json::json!(if crate::socket::codex_root_rollout_for_session(sid).is_some() { "codex" } else { "claude" });
        }
    }
    one(live, saved, last);
    if let Some(tabs) = live["tabs"].as_array_mut() { for tab in tabs { one(tab, saved, last); } }
}

fn saved_labels(record: &mut ClosedPane) {
    record.character = record.rec["character"].as_str().unwrap_or_default().to_string();
    if let Some(cwd) = record.rec["cwd"].as_str() {
        record.folder = std::path::Path::new(cwd).file_name().map(|part| part.to_string_lossy().into_owned()).unwrap_or_default();
    }
}

fn split_saved_process(record: &serde_json::Value, surface: &str, key: &str) -> Option<(serde_json::Value, Option<serde_json::Value>)> {
    let mut all: Vec<_> = std::iter::once(record).chain(record.get("tabs").and_then(|v| v.as_array()).into_iter().flatten())
        .filter(|value| value.is_object()).cloned().collect();
    let index = all.iter().position(|value| value["surface_key"].as_str() == Some(key)
        || value["pane_id"].as_str() == Some(surface))?;
    for value in &mut all { value.as_object_mut()?.remove("tabs"); value.as_object_mut()?.remove("active_tab"); }
    let stopped = all.remove(index);
    let remaining = if all.is_empty() { None } else {
        let mut head = all.remove(0);
        if !all.is_empty() { head["tabs"] = serde_json::Value::Array(all); }
        Some(head)
    };
    Some((stopped, remaining))
}

impl App {
    pub(crate) fn decorate_usage_processes(&mut self) {
        let seats: Vec<_> = {
            let ws = self.ws.lock().unwrap();
            self.pty.iter().filter(|(surface, _)| !kasa_mcp::remote::is_remote_pane(surface)).filter_map(|(surface, session)| {
                let shell = session.shell_pid()?;
                let outer = ws.outer_for_pty(surface).unwrap_or_else(|| surface.clone());
                let name = ws.pane_character.get(surface).filter(|name| !name.is_empty()).cloned().unwrap_or_else(|| "셸".into());
                let mixed = ws.panes.get(&outer).is_some_and(|pane| pane.tabs.iter().any(|tab| tab.pid.is_none()));
                Some((surface.clone(), Arc::downgrade(session), shell, outer, name, mixed))
            }).collect()
        };
        let owners: std::collections::HashMap<u32, Owner> = seats.into_iter().map(|(surface, session, shell, outer, name, mixed)| {
            let room = self.window_of_pane(&outer).and_then(|index| self.window_name_override.get(&index).cloned()
                .or_else(|| self.window_labels.get(index).map(|label| label.0.clone()))).unwrap_or_default();
            let label = if room.is_empty() { name } else { format!("{name} · {room}") };
            let closed_record = self.closed_panes.iter().find(|record| record.pane_id == outer && record.alive).map(|record| record.ui_id);
            (shell, Owner { key: kasa_mcp::surface_keys::ensure(&surface), surface, session, shell, label, closed_record, needs_open: mixed && closed_record.is_some() })
        }).collect();
        let parents: std::collections::HashMap<u32, u32> = self.statusbar.usage_processes.iter().map(|row| (row.id.pid, row.ppid)).collect();
        for row in &mut self.statusbar.usage_processes {
            let mut at = row.id.pid;
            for _ in 0..64 {
                if let Some(owner) = owners.get(&at) { row.owner = Some(owner.clone()); break; }
                match parents.get(&at) { Some(&parent) if parent > 1 && parent != at => at = parent, _ => break }
            }
        }
    }

    pub(crate) fn open_usage_shell(&mut self, id: &Identity) {
        let Some(owner) = self.statusbar.usage_processes.iter().find(|row| row.id == *id).and_then(|row| row.owner.clone()) else { return };
        if !self.pty.get(&owner.surface).is_some_and(|session| owner.session.ptr_eq(&Arc::downgrade(session)))
            || kasa_mcp::surface_keys::get(&owner.surface).as_deref() != Some(owner.key.as_str()) { return; }
        if let Some(index) = self.closed_panes.iter().position(|record| Some(record.ui_id) == owner.closed_record && record.alive) {
            self.reopen_closed_pane_at(index);
            self.force_usage_refresh();
        }
    }

    pub(crate) fn force_usage_refresh(&mut self) {
        let now = Instant::now();
        self.statusbar.tunnel_checked = Some(now);
        self.statusbar.probe.kick(now);
        self.chrome_dirty = true;
    }

    fn stop_hidden_usage_shell(&mut self, owner: &Owner, record_id: u64, session: &Arc<kasa_pty::PtySession>) -> Result<(), String> {
        let at = self.closed_panes.iter().position(|record| record.ui_id == record_id && record.alive)
            .ok_or("보관된 셸이 바뀌었어요")?;
        let original = self.closed_panes[at].clone();
        if self.window_of_pane(&original.pane_id).is_some() || self.aux_terminal_index(&original.pane_id).is_some() {
            return Err("셸이 다시 열렸어요. 목록을 다시 확인해 주세요".into());
        }
        if self.closed_panes.iter().filter(|record| record.alive && record.pane_id == original.pane_id).count() != 1 {
            return Err("보관 목록이 바뀌었어요. 다시 확인해 주세요".into());
        }
        if self.ws.lock().unwrap().panes.get(&original.pane_id).is_some_and(|pane| pane.tabs.iter().any(|tab| tab.pid.is_none())) {
            return Err("문서가 함께 보관돼 있어요. 셸을 열어 개별 종료해 주세요".into());
        }
        let config = self.agent_cfg_snapshot();
        let mut live = {
            let ws = self.ws.lock().unwrap();
            Self::layout_to_json(&kasa_pty::PtyLayout::single(&original.pane_id), &self.pty, &ws, &self.pane_claude_sid, &config)["leaf"].clone()
        };
        backfill_history(&mut live, &original.rec, &self.pane_last_seat);
        if let Some(key) = original.rec.get("close_record_key") { live["close_record_key"] = key.clone(); }
        let (stopped, remaining) = split_saved_process(&live, &owner.surface, &owner.key)
            .ok_or("이 셸의 복원 기록을 확인하지 못했어요")?;
        let index = {
            let ws = self.ws.lock().unwrap();
            ws.panes.get(&original.pane_id).and_then(|pane| pane.tabs.iter().position(|tab| tab.pid.as_deref() == Some(owner.surface.as_str())))
        }.ok_or("보관된 탭 구성이 바뀌었어요")?;
        // Remove the record while normal tab cleanup runs, then restore its stable receipt and separate recipes.
        self.closed_panes.remove(at);
        session.stop_reader();
        if remaining.is_some() { self.close_tab(&original.pane_id, index); }
        else { session.terminate_local(); self.kill_hidden_pane(&original.pane_id); }
        self.dead_panes.lock().unwrap().retain(|pid| pid != &owner.surface);
        let stopped_primary = original.pane_id == owner.surface;
        let mut dead = original.clone();
        dead.rec = stopped; dead.pane_id = owner.surface.clone(); dead.alive = false; dead.idle_since = None;
        saved_labels(&mut dead);
        if !stopped_primary { dead.ui_id = ClosedPane::next_ui_id(); }
        if let Some(rec) = remaining {
            let next = self.ws.lock().unwrap().panes.get(&original.pane_id)
                .and_then(|pane| pane.tabs.iter().filter_map(|tab| tab.pid.clone()).find(|pid| self.pty.contains_key(pid)));
            let mut kept = original;
            kept.rec = rec;
            saved_labels(&mut kept);
            kept.alive = next.is_some();
            if let Some(next) = next {
                if next != kept.pane_id { self.rekey_cell(&kept.pane_id, &next); kept.pane_id = next; }
            } else { kept.idle_since = None; }
            if stopped_primary { kept.ui_id = ClosedPane::next_ui_id(); }
            self.push_closed_pane(kept);
        }
        self.push_closed_pane(dead);
        self.session_touched = true;
        self.chrome_dirty = true;
        Ok(())
    }

    pub(crate) fn stop_usage_process(&mut self, identity: &Identity) {
        let Some(row) = self.statusbar.usage_processes.iter().find(|row| row.id == *identity).cloned() else {
            self.set_toast("선택한 실행이 끝났거나 바뀌었어요".into()); return;
        };
        if !stoppable(&row, &self.statusbar.usage_processes) { return; }
        let armed = self.statusbar.usage_process_armed.as_ref().is_some_and(|(id, at)| id == identity && at.elapsed().as_secs() < 5);
        if !armed {
            self.statusbar.usage_process_armed = Some((identity.clone(), Instant::now()));
            self.chrome_dirty = true; return;
        }
        self.statusbar.usage_process_armed = None;
        if let Some(owner) = row.owner.as_ref().filter(|owner| owner.shell == identity.pid) {
            let Some(session) = self.pty.get(&owner.surface).cloned().filter(|session| owner.session.ptr_eq(&Arc::downgrade(session))) else {
                self.set_toast("선택한 셸이 바뀌었어요".into()); return;
            };
            if kasa_mcp::surface_keys::get(&owner.surface).as_deref() != Some(owner.key.as_str())
                || crate::input::usage_process_birth(identity.pid) != Some(identity.started) {
                self.set_toast("선택한 셸이 바뀌었어요".into()); return;
            }
            let slot = {
                let ws = self.ws.lock().unwrap();
                ws.outer_for_pty(&owner.surface).and_then(|outer| {
                    let index = ws.panes.get(&outer)?.tabs.iter().position(|tab| tab.pid.as_deref() == Some(owner.surface.as_str()))?;
                    Some((outer, index))
                })
            };
            if let Some((outer, index)) = slot.filter(|(outer, _)| self.window_of_pane(outer).is_some() || self.aux_terminal_index(outer).is_some()) {
                self.statusbar.popover = None;
                // The second button already confirmed this exact process; another index-only modal would lose that identity.
                self.close_tab(&outer, index);
                let _ = session.set_input_closed(true, false);
                session.terminate_local();
                self.force_usage_refresh();
            } else if let Some(record_id) = owner.closed_record {
                match self.stop_hidden_usage_shell(owner, record_id, &session) {
                    Ok(()) => { self.force_usage_refresh(); self.set_toast("보관된 셸을 종료했어요. 다른 탭과 복원 기록은 유지했어요".into()); }
                    Err(error) => self.set_toast(error),
                }
            } else {
                self.set_toast("셸의 위치가 바뀌었어요. 목록을 다시 확인해 주세요".into());
            }
            return;
        }
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let message = match terminate(&row) {
                Ok(()) => format!("{} · PID {} 종료 요청", row.name, row.id.pid),
                Err(error) => error,
            };
            let _ = proxy.send_event(UserEvent::UsageStopped(message));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(pid: u32, ppid: u32, root: u32, started: u64) -> Process {
        Process { id: Identity { pid, started, uid: 50, exe: "zsh".into() }, ppid, root, name: "zsh".into(), cpu: 0.0, rss: 0, cwd: None, owner: None }
    }
    #[test]
    fn saved_harness_survives_a_missing_transcript_without_replacing_a_new_session() {
        let saved = serde_json::json!({"pane_id":"%1", "surface_key":"same", "session_id":"absent-codex-fixture", "was_agent":"codex", "model":"saved-model"});
        let last = std::collections::HashMap::from([("%1".into(), ("".into(), "absent-codex-fixture".into()))]);
        let mut live = serde_json::json!({"pane_id":"%1", "surface_key":"same"});
        backfill_history(&mut live, &saved, &last);
        assert_eq!(live["was_agent"], "codex");
        assert_eq!(live["model"], "saved-model");
        let mut fresh = serde_json::json!({"pane_id":"%1", "surface_key":"same", "session_id":"new-session", "was_agent":"claude"});
        backfill_history(&mut fresh, &saved, &last);
        assert_eq!(fresh["session_id"], "new-session");
        assert_eq!(fresh["was_agent"], "claude");
        assert!(fresh["model"].is_null());
    }
    #[test]
    fn a_hidden_cell_splits_into_disjoint_launch_recipes() {
        let record = serde_json::json!({"pane_id":"%1", "surface_key":"one", "session_id":"first", "close_record_key":"receipt", "tabs":[
            {"pane_id":"%2", "surface_key":"two", "session_id":"second"},
            {"pane_id":"%3", "surface_key":"three", "session_id":"third"}
        ]});
        let (first, rest) = split_saved_process(&record, "%1", "one").unwrap();
        assert_eq!(first["close_record_key"], "receipt");
        assert!(first.get("tabs").is_none());
        let rest = rest.unwrap();
        assert_eq!(rest["session_id"], "second");
        assert_eq!(rest["tabs"][0]["session_id"], "third");
        let (second, rest) = split_saved_process(&record, "%2", "two").unwrap();
        assert_eq!(second["session_id"], "second");
        assert!(second.get("close_record_key").is_none());
        assert_eq!(rest.unwrap()["close_record_key"], "receipt");
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn a_real_child_ends_without_touching_its_sibling_and_stale_birth_is_refused() {
        struct Child(std::process::Child);
        impl Drop for Child { fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); } }
        fn sleeper() -> std::process::Child {
            #[cfg(unix)]
            return std::process::Command::new("/bin/sleep").arg("60").spawn().unwrap();
            // Windows 엔 sleep 이 없다. 셸을 거치면 셸만 끄게 되므로 ping 을 직접 띄운다.
            #[cfg(windows)]
            return std::process::Command::new("ping").args(["-n", "60", "127.0.0.1"])
                .stdout(std::process::Stdio::null()).spawn().unwrap();
        }
        let mut first = Child(sleeper());
        let mut sibling = Child(sleeper());
        let rows = crate::input::fresh_usage_processes().unwrap();
        let row = rows.iter().find(|row| row.id.pid == first.0.id()).unwrap().clone();
        let mut stale = row.clone(); stale.id.started = stale.id.started.wrapping_add(1);
        assert!(terminate(&stale).is_err());
        assert!(first.0.try_wait().unwrap().is_none());
        terminate(&row).unwrap();
        let deadline = Instant::now() + std::time::Duration::from_secs(3);
        while first.0.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(sibling.0.try_wait().unwrap().is_none());
    }

    #[test]
    fn reused_pid_changed_ownership_and_app_ancestors_cannot_be_terminated() {
        let expected = row(20, 10, 10, 1);
        let mut rows = vec![row(10, 1, 10, 1), expected.clone(), row(30, 10, 10, 1)];
        assert!(validate(&expected, &rows, 30, 50).is_ok());
        rows[1].id.started = 2;
        assert!(validate(&expected, &rows, 30, 50).is_err());
        rows[1] = expected.clone(); rows[1].root = 11;
        assert!(validate(&expected, &rows, 30, 50).is_err());
        assert!(validate(&expected, &rows, 30, 51).is_err());
        assert!(validate(&rows[0], &rows, 30, 50).is_err());
        assert!(validate(&rows[2], &rows, 30, 50).is_err());
        assert_eq!(descendants(&rows, &rows[0].id).len(), 3);
    }
}
