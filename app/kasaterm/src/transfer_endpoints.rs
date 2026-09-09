use super::*;
use std::collections::HashSet;
use kasa_socket::transfer::{MachineSnapshot, RoomInfo, RoomTarget, SessionIdentity, SessionRow, SpawnRequest};
use std::sync::{OnceLock, Weak};

type Reply<T> = std::sync::mpsc::Sender<std::result::Result<T, String>>;

struct Binding {
    session: Weak<kasa_pty::PtySession>,
    token: String,
}

fn bindings() -> &'static Mutex<HashMap<String, Binding>> {
    static VALUE: OnceLock<Mutex<HashMap<String, Binding>>> = OnceLock::new();
    VALUE.get_or_init(Default::default)
}

fn created_rooms() -> &'static Mutex<HashSet<String>> {
    static VALUE: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    VALUE.get_or_init(Default::default)
}

pub(crate) fn instance() -> &'static str {
    static VALUE: OnceLock<String> = OnceLock::new();
    VALUE.get_or_init(|| uuid::Uuid::new_v4().to_string())
}

pub(crate) fn machine_context() -> Result<&'static (String, String)> {
    static VALUE: OnceLock<(String, String)> = OnceLock::new();
    if VALUE.get().is_none() {
        let id = kasa_mcp::mobile::machine_identity().ok_or_else(|| anyhow::anyhow!("이 기계의 신원을 확인하지 못했어요"))?;
        let _ = VALUE.set((id, kasa_mcp::machines::self_label()));
    }
    Ok(VALUE.get().unwrap())
}

fn token_for(id: &str, session: &Arc<kasa_pty::PtySession>) -> String {
    let mut map = bindings().lock().unwrap();
    let entry = map.entry(id.to_string()).or_insert_with(|| Binding {
        session: Arc::downgrade(session), token: uuid::Uuid::new_v4().to_string(),
    });
    if !Weak::ptr_eq(&entry.session, &Arc::downgrade(session)) {
        *entry = Binding { session: Arc::downgrade(session), token: uuid::Uuid::new_v4().to_string() };
    }
    entry.token.clone()
}

pub(crate) fn idle_shell(pid: Option<u32>, table: &[(u32, u32, String)]) -> bool {
    let Some(pid) = pid else { return false };
    let is_shell = table.iter().find(|(found, _, _)| *found == pid).is_some_and(|(_, _, name)| {
        let name = name.trim_start_matches('-').to_ascii_lowercase();
        let name = name.strip_suffix(".exe").unwrap_or(&name);
        matches!(name, "zsh" | "bash" | "sh" | "dash" | "fish" | "ksh" | "csh" | "tcsh" | "pwsh" | "powershell" | "cmd")
    });
    is_shell
        && !table.iter().any(|(_, parent, _)| *parent == pid)
        && kasa_pty::agent_for_shell(table, pid).is_none()
}

pub(crate) fn enrich_snapshot(snapshot: &mut MachineSnapshot) {
    let table = kasa_pty::fresh_process_table();
    for row in &mut snapshot.sessions {
        let session = kasa_pty::lookup_session(&row.identity.pane_id);
        if !session.as_ref().is_some_and(|session| token_for(&row.identity.pane_id, session) == row.identity.token) {
            row.shell_closeable = false;
            row.unavailable_reason = Some("목록을 읽는 동안 세션이 바뀌었어요".into());
            continue;
        }
        let pid = session.as_ref().and_then(|s| s.shell_pid());
        row.harness = pid.and_then(|pid| kasa_pty::agent_for_shell(&table, pid)).map(|agent| agent.as_str().to_string());
        row.shell_closeable &= idle_shell(pid, &table);
        if row.harness.is_none() {
            row.identity.session_id = None;
            row.name.clear();
            row.status = if row.shell_closeable { "idle" } else { "working" }.to_string();
        } else if row.identity.session_id.is_none() {
            row.unavailable_reason = Some("대화 정보를 확인하는 중이에요".into());
        }
        row.cwd = pid.and_then(crate::socket::pid_cwd).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| row.cwd.clone());
        if pid.is_none() || !pid.is_some_and(|pid| table.iter().any(|(found, _, _)| *found == pid)) {
            row.unavailable_reason = Some("실행 상태를 확인하지 못했어요".into());
        }
    }
}

#[derive(Clone)]
pub(crate) struct SpawnPlan {
    pane: String,
    expected: Weak<kasa_pty::PtySession>,
    room: RoomTarget,
    collab_room: Option<String>,
    cwd: String,
    character: Option<String>,
    cols: u16,
    rows: u16,
}

impl std::fmt::Debug for SpawnPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.debug_struct("SpawnPlan").field("pane", &self.pane).finish() }
}

pub(crate) struct Spawned {
    pub plan: SpawnPlan,
    pub outcome: Mutex<Option<Result<(Arc<kasa_pty::PtySession>, Option<String>)>>>,
}

impl std::fmt::Debug for Spawned {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.debug_struct("Spawned").field("plan", &self.plan).finish() }
}

pub(crate) fn spawn(plan: SpawnPlan) -> Spawned {
    let outcome = (|| {
        let mut env = crate::proxy_env(&plan.pane);
        env.retain(|(key, _)| key != "KASATERM_ROOM");
        env.push(("KASATERM_ROOM".into(), plan.collab_room.clone().unwrap_or_default()));
        let mut sid = None;
        if let Some(name) = &plan.character {
            let assigned = kasa_mcp::character::new_session_id();
            let slug = kasa_mcp::character::rslug(std::path::Path::new(&plan.cwd), plan.collab_room.as_deref());
            kasa_mcp::character::write_marker(&slug, &plan.pane, name)?;
            kasa_mcp::character::bind_session_character(&assigned, name)?;
            env.extend([
                ("KASATERM_CHARACTER".into(), name.clone()),
                ("KASATERM_SESSION_ID".into(), assigned.clone()),
                ("KASATERM_AGENT_SUFFIX".into(), crate::agent_name_suffix()),
                ("KASATERM_AGENT_SLUG".into(), crate::theme::agent_slug(name)),
            ]);
            if socket::read_claude_persona() {
                if let Some(persona) = kasa_mcp::character::persona_for_any(name) {
                    env.push(("KASATERM_PERSONA".into(), persona));
                }
            }
            sid = Some(assigned);
        }
        let session = Arc::new(kasa_pty::PtySession::start(kasa_pty::PtyOptions {
            pane_id: plan.pane.clone(), cwd: Some(plan.cwd.clone()), shell: resolve_default_shell(),
            cols: plan.cols, rows: plan.rows, env, initial_scrollback: Vec::new(),
        })?);
        Ok((session, sid))
    })();
    Spawned { plan, outcome: Mutex::new(Some(outcome)) }
}

impl App {
    fn transfer_rooms(&self) -> Vec<(RoomInfo, usize, Option<String>)> {
        let ws = self.ws.lock().unwrap();
        (0..self.windows.len()).filter(|idx| self.internal_room_kind_at(*idx).is_none()).map(|idx| {
            let leaves = self.window_leaves(idx);
            let title = self.window_name_override.get(&idx).cloned()
                .or_else(|| self.window_labels.get(idx).map(|value| value.0.clone()).filter(|v| !v.is_empty()))
                .unwrap_or_else(|| format!("{}번 방", idx + 1));
            let id = leaves.first().and_then(|id| self.pty.get(id).map(|session| format!("room:{}", token_for(id, session))))
                .unwrap_or_else(|| format!("empty:{}:{idx}:{title}", instance()));
            let collab = leaves.iter().find_map(|id| ws.pane_room.get(id).cloned());
            (RoomInfo { id, title }, idx, collab)
        }).collect()
    }

    pub(crate) fn transfer_snapshot_gui(&self, machine: &(String, String)) -> MachineSnapshot {
        let rooms = self.transfer_rooms();
        let ws = self.ws.lock().unwrap();
        let sessions = self.pty.iter().filter(|(id, _)| !kasa_mcp::remote::is_remote_pane(id)).map(|(id, session)| {
            let outer = ws.outer_for_pty(id).unwrap_or_else(|| id.clone());
            let room = rooms.iter().find(|(_, idx, _)| self.window_leaves(*idx).contains(&outer));
            let activity = self.pane_activity.get(id);
            SessionRow {
                identity: SessionIdentity { machine_id: machine.0.clone(), pane_id: id.clone(),
                    session_id: self.pane_claude_sid.get(id).cloned(), instance: instance().into(), token: token_for(id, session) },
                local_panes: vec![id.clone()],
                name: ws.pane_character.get(id).cloned().unwrap_or_default(),
                title: ws.panes.get(&outer).and_then(|p| p.title.clone()).unwrap_or_default(),
                cwd: self.pane_cwd_cache.get(id).map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
                harness: None, status: activity.map(|a| a.status.clone()).unwrap_or_else(|| "idle".into()),
                room_id: room.map(|(r, _, _)| r.id.clone()),
                shell_closeable: !(outer == *id && ws.panes.get(&outer).is_some_and(|p| p.tabs.len() > 1)),
                unavailable_reason: None,
            }
        }).collect();
        MachineSnapshot { machine_id: machine.0.clone(), label: machine.1.clone(), instance: instance().into(),
            rooms: rooms.into_iter().map(|(room, _, _)| room).collect(), sessions, room_transfer_supported: cfg!(unix) && self.tmux.is_none() }
    }

    pub(crate) fn validate_transfer_identity(&self, identity: &SessionIdentity) -> Result<()> {
        if identity.machine_id != machine_context()?.0 || identity.instance != instance() || identity.token.is_empty() {
            anyhow::bail!("앱이 바뀌었어요. 목록을 새로 읽어 주세요");
        }
        let session = self.pty.get(&identity.pane_id).ok_or_else(|| anyhow::anyhow!("선택한 세션이 이미 닫혔어요"))?;
        if kasa_mcp::remote::is_remote_pane(&identity.pane_id) || token_for(&identity.pane_id, session) != identity.token {
            anyhow::bail!("선택한 자리에 다른 세션이 있어요. 목록을 새로 읽어 주세요");
        }
        let active = session.active_agent().is_some();
        if identity.session_id.is_some() != active {
            anyhow::bail!("선택한 셸의 실행 상태가 바뀌었어요. 목록을 새로 읽어 주세요");
        }
        if let Some(sid) = &identity.session_id {
            if self.pane_claude_sid.get(&identity.pane_id) != Some(sid) {
                anyhow::bail!("선택한 대화가 바뀌었어요. 목록을 새로 읽어 주세요");
            }
        }
        self.ensure_user_mutation_target(&identity.pane_id, crate::settings_room::SettingsMutation::Split)?;
        Ok(())
    }

    pub(crate) fn prepare_transfer_spawn(&mut self, request: &SpawnRequest) -> Result<SpawnPlan> {
        if self.tmux.is_some() { anyhow::bail!("이 실행 방식은 방 이사를 지원하지 않아요"); }
        let collab_room = match &request.room {
            RoomTarget::Existing(id) => self.transfer_rooms().into_iter().find(|(room, _, _)| room.id == *id)
                .ok_or_else(|| anyhow::anyhow!("도착할 방이 바뀌었어요. 목록을 새로 읽어 주세요"))?.2,
            RoomTarget::New(title) => {
                if title.trim().is_empty() || title.chars().count() > 80 || title.chars().any(char::is_control) {
                    anyhow::bail!("새 방 이름을 짧게 적어 주세요");
                }
                Some(format!("room-{}", uuid::Uuid::new_v4().simple()))
            }
        };
        let pane = self.alloc_pane_id();
        let (events, receiver) = crossbeam_channel::unbounded();
        let pending = Arc::new(kasa_pty::PtySession::start_external(kasa_pty::PtyOptions { pane_id: pane.clone(), ..Default::default() },
            kasa_pty::ExternalIo { events: receiver, writer: Box::new(std::io::sink()), on_resize: Arc::new(move |_, _| { let _keep = &events; }) })?);
        let expected = Arc::downgrade(&pending);
        self.insert_pty(pane.clone(), pending);
        let (cols, rows) = self.window_cells();
        Ok(SpawnPlan { pane, expected, room: request.room.clone(), collab_room, cwd: request.cwd.clone(),
            character: request.character.clone(), cols, rows })
    }

    pub(crate) fn finish_transfer_spawn(&mut self, spawned: &Spawned, machine: &(String, String)) -> Result<SessionRow> {
        let plan = &spawned.plan;
        let Some(current) = self.pty.get(&plan.pane) else { anyhow::bail!("생성을 취소했어요"); };
        if !Weak::ptr_eq(&Arc::downgrade(current), &plan.expected) { anyhow::bail!("생성할 자리가 바뀌었어요"); }
        self.pty.remove(&plan.pane);
        let (session, sid) = spawned.outcome.lock().unwrap().take().ok_or_else(|| anyhow::anyhow!("이미 처리한 생성 요청이에요"))??;
        let (idx, anchor) = match &plan.room {
            RoomTarget::Existing(id) => {
                let (_, idx, _) = self.transfer_rooms().into_iter().find(|(room, _, _)| room.id == *id)
                    .ok_or_else(|| anyhow::anyhow!("도착할 방이 닫혔어요"))?;
                (idx, self.window_leaves(idx).first().cloned())
            }
            RoomTarget::New(title) => {
                self.windows.push(None);
                let idx = self.windows.len() - 1;
                self.window_name_override.insert(idx, title.trim().into());
                if let Some(room) = &plan.collab_room { created_rooms().lock().unwrap().insert(room.clone()); }
                (idx, None)
            }
        };
        let layout = if idx == self.active_window { &mut self.pty_layout } else { &mut self.windows[idx] };
        if let Some(anchor) = anchor {
            if !layout.as_mut().is_some_and(|tree| tree.split_leaf(&anchor, kasa_pty::SplitDir::Vertical, plan.pane.clone())) {
                anyhow::bail!("도착할 방의 배치가 바뀌었어요");
            }
        } else {
            *layout = Some(kasa_pty::PtyLayout::single(&plan.pane));
        }
        self.insert_pty(plan.pane.clone(), session.clone());
        self.pump_pty_screens(session.screens.clone(), plan.pane.clone(), Arc::downgrade(&session));
        {
            let mut ws = self.ws.lock().unwrap();
            if let Some(room) = &plan.collab_room { ws.pane_room.insert(plan.pane.clone(), room.clone()); }
            if let Some(name) = &plan.character { ws.pane_character.insert(plan.pane.clone(), name.clone()); }
        }
        if let Some(sid) = sid { self.pane_session_id.insert(plan.pane.clone(), sid); }
        self.pane_cwd_cache.insert(plan.pane.clone(), std::path::PathBuf::from(&plan.cwd));
        self.publish_pty_layout();
        self.chrome_dirty = true;
        self.session_touched = true;
        self.transfer_snapshot_gui(machine).sessions.into_iter().find(|row| row.identity.pane_id == plan.pane)
            .ok_or_else(|| anyhow::anyhow!("생성한 세션을 확인하지 못했어요"))
    }

    pub(crate) fn close_transfer_shell(&mut self, identity: &SessionIdentity) -> Result<()> {
        self.validate_transfer_identity(identity)?;
        let session = self.pty.get(&identity.pane_id).unwrap();
        // 실행 직전 다시 검사한다. 선택 이후 시작한 빌드·서버·에이전트도 보호한다.
        if !idle_shell(session.shell_pid(), &kasa_pty::fresh_process_table()) {
            anyhow::bail!("작업이 있거나 상태를 확인할 수 없는 셸은 닫지 않아요");
        }
        let room_before = self.window_of_pane(&identity.pane_id).and_then(|idx| self.transfer_rooms().into_iter().find(|(_, at, _)| *at == idx));
        let location = {
            let ws = self.ws.lock().unwrap();
            if ws.panes.get(&identity.pane_id).is_some_and(|pane| pane.tabs.len() > 1) {
                anyhow::bail!("다른 탭이 있는 기본 셸은 함께 닫지 않아요");
            }
            ws.outer_for_pty(&identity.pane_id).and_then(|outer| ws.panes.get(&outer)
                .and_then(|p| p.tabs.iter().position(|t| t.pid.as_deref() == Some(&identity.pane_id)))
                .map(|index| (outer, index)))
        };
        if let Some((outer, index)) = location.filter(|(outer, _)| outer != &identity.pane_id) {
            self.close_tab(&outer, index);
        } else {
            self.remove_pane(&identity.pane_id);
        }
        if let Some((_, idx, Some(room))) = room_before {
            if self.window_leaves(idx).is_empty() && created_rooms().lock().unwrap().remove(&room) {
                self.close_window(idx)?;
            }
        }
        Ok(())
    }

    pub(crate) fn cancel_transfer_spawn(&mut self, plan: &SpawnPlan) {
        if self.pty.get(&plan.pane).is_some_and(|current| Weak::ptr_eq(&Arc::downgrade(current), &plan.expected)) {
            self.pty.remove(&plan.pane);
        }
    }
}

pub(crate) fn reply<T>(sender: &Reply<T>, value: Result<T>) { let _ = sender.send(value.map_err(|e| e.to_string())); }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_or_unknown_shells_are_never_cleanup_targets() {
        let shell = vec![(100, 1, "zsh".into())];
        assert!(idle_shell(Some(100), &shell));
        assert!(!idle_shell(None, &shell));
        assert!(!idle_shell(Some(100), &[]));
        for child in ["sleep", "cargo", "node", "codex", "claude"] {
            let mut table = shell.clone();
            table.push((101, 100, child.into()));
            assert!(!idle_shell(Some(100), &table), "{child}");
            assert!(!idle_shell(Some(100), &[(100, 1, child.into())]), "exec {child}");
        }
    }
}
