//! Add newly discovered source panes without touching existing connections/focus.
use super::*;
use std::collections::{HashSet, VecDeque};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[path = "mirror_sync_plan.rs"]
mod plan;

#[derive(Clone)]
struct Candidate {
    base: String,
    label: String,
    source: String,
    room: u64,
    room_name: Option<String>,
    requires_room: bool,
    wave: Instant,
    name: String,
    cwd: Option<String>,
    attempts: u8,
    after: Instant,
}

struct Ready {
    local: String,
    candidate: Candidate,
    session: Option<Arc<kasa_pty::PtySession>>,
}

#[derive(Default)]
pub(crate) struct MirrorSyncState {
    planner: plan::Planner,
    last_poll: Option<Instant>,
    queue: VecDeque<Candidate>,
    pending: HashSet<String>,
    opened_rooms: HashSet<(String, u64, Instant)>,
    receiver: Option<mpsc::Receiver<Ready>>,
    sender: Option<mpsc::Sender<Ready>>,
}

impl MirrorSyncState {
    pub(crate) fn reserved_ids(&self) -> impl Iterator<Item = String> + '_ {
        self.pending.iter().cloned()
    }
}

fn source_rows(machine: &serde_json::Value) -> Vec<plan::SourcePane> {
    machine.get("panes").and_then(|v| v.as_array()).into_iter().flatten()
        .filter_map(|row| {
            let id = row.get("id")?.as_str()?;
            // Historical detached panes often have no window. Remember their IDs
            // in the baseline too, so a later restore is not mistaken for spawn.
            let room = row.get("window").and_then(|v| v.as_u64()).unwrap_or(u64::MAX);
            Some(plan::SourcePane {
                id: id.into(), room,
                eligible: room != u64::MAX && id.starts_with('%')
                    && row.get("closed").and_then(|v| v.as_bool()) != Some(true)
                    && row.get("undocked").and_then(|v| v.as_bool()) != Some(true)
                    && !row.get("mirror_of").is_some_and(|v| !v.is_null()),
            })
        }).collect()
}

impl App {
    /// Called irrespective of the Info tab's visibility. Connections run off-thread.
    pub(crate) fn poll_mirror_sync(&mut self) {
        if self.tmux.is_some() { return; }
        self.finish_mirror_sync();
        if self.mirror_sync.last_poll.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
            return;
        }
        self.mirror_sync.last_poll = Some(Instant::now());
        let machines = kasa_mcp::machines::snapshot();
        for machine in &machines {
            let Some(base) = machine.get("base").and_then(|v| v.as_str()) else { continue };
            let mirrors = self.mirror_sync_views(base);
            let rows = source_rows(machine);
            let occupied: HashSet<u64> = rows.iter()
                .filter(|row| mirrors.iter().any(|(_, id)| id == &row.id))
                .map(|row| row.room).collect();
            let fresh = machine.get("online").and_then(|v| v.as_bool()) == Some(true)
                && machine.get("online_via").and_then(|v| v.as_str()) == Some("direct")
                // During source startup an empty list must not become the initial
                // baseline, followed by every restored historical pane as "new".
                && rows.iter().any(|row| mirrors.iter().any(|(_, id)| id == &row.id));
            let plans = self.mirror_sync.planner.observe(base, !mirrors.is_empty(), fresh, &rows, &occupied);
            for plan in plans {
                let row = machine["panes"].as_array().and_then(|rows| rows.iter()
                    .find(|row| row["id"].as_str() == Some(plan.id.as_str()))).unwrap();
                let string = |key: &str| row[key].as_str().filter(|s| !s.is_empty()).map(str::to_string);
                self.mirror_sync.queue.push_back(Candidate {
                    base: base.into(), label: machine["label"].as_str().unwrap_or(base).into(),
                    source: plan.id, room: plan.room, room_name: string("window_name"),
                    requires_room: occupied.contains(&plan.room),
                    wave: self.mirror_sync.last_poll.unwrap(),
                    name: string("name").unwrap_or_default(), cwd: string("cwd"),
                    attempts: 0, after: Instant::now(),
                });
            }
        }
        if self.mirror_sync.sender.is_none() {
            let (sender, receiver) = mpsc::channel();
            self.mirror_sync.sender = Some(sender);
            self.mirror_sync.receiver = Some(receiver);
        }
        let count = self.mirror_sync.queue.len();
        for _ in 0..count {
            if self.mirror_sync.pending.len() >= 4 { break; }
            let Some(mut candidate) = self.mirror_sync.queue.pop_front() else { break };
            if candidate.after > Instant::now() {
                self.mirror_sync.queue.push_back(candidate); continue;
            }
            let views = self.mirror_sync_views(&candidate.base);
            if views.is_empty() || views.iter().any(|(_, id)| id == &candidate.source) { continue; }
            match self.mirror_sync_source_alive(&candidate, &machines) {
                Some(true) => {},
                Some(false) => continue,
                // A stale cache is not proof that the source pane was closed.
                None => {
                    candidate.after = Instant::now() + Duration::from_secs(5);
                    self.mirror_sync.queue.push_back(candidate); continue;
                }
            }
            let local = self.alloc_pane_id();
            self.mirror_sync.pending.insert(local.clone());
            let sender = self.mirror_sync.sender.as_ref().unwrap().clone();
            let proxy = self.proxy.clone();
            candidate.attempts += 1;
            std::thread::spawn(move || {
                let session = kasa_mcp::remote::connect_view(kasa_mcp::remote::RemoteSpec {
                    base: candidate.base.clone(), pane: Some(candidate.source.clone()),
                    cwd: None, token: None,
                    identity: kasa_mcp::remote::RemoteIdentity {
                        label: candidate.label.clone(), remote_cwd: candidate.cwd.clone(),
                        origin_cwd: None, owned: false,
                    },
                }, &local).map(|remote| remote.session).ok();
                // A failed send drops the Arc; DetachOnDrop closes only this viewer.
                let _ = sender.send(Ready { local, candidate, session });
                let _ = proxy.send_event(UserEvent::Redraw);
            });
        }
    }

    fn mirror_sync_views(&self, base: &str) -> Vec<(String, String)> {
        self.pty.keys().filter_map(|local| {
            let info = kasa_mcp::remote::remote_info(local)?;
            (info.view && info.base.trim_end_matches('/') == base.trim_end_matches('/')
                && self.window_of_pane(local).is_some())
                .then(|| (local.clone(), info.remote_id))
        }).collect()
    }

    fn mirror_sync_source_alive(&self, candidate: &Candidate, machines: &[serde_json::Value]) -> Option<bool> {
        machines.iter().find(|m| m["base"].as_str() == Some(candidate.base.as_str()))
            .filter(|m| m["online_via"].as_str() == Some("direct"))
            .map(|m| source_rows(m).iter().any(|p| p.id == candidate.source && p.eligible))
    }

    fn finish_mirror_sync(&mut self) {
        let ready: Vec<_> = self.mirror_sync.receiver.as_ref()
            .map(|rx| rx.try_iter().collect()).unwrap_or_default();
        if ready.is_empty() { return; }
        let machines = kasa_mcp::machines::snapshot();
        for mut ready in ready {
            self.mirror_sync.pending.remove(&ready.local);
            let views = self.mirror_sync_views(&ready.candidate.base);
            if views.is_empty() || views.iter().any(|(_, id)| id == &ready.candidate.source)
                || self.mirror_sync_source_alive(&ready.candidate, &machines) == Some(false)
            {
                continue;
            }
            if self.mirror_sync_source_alive(&ready.candidate, &machines).is_none() {
                ready.candidate.after = Instant::now() + Duration::from_secs(5);
                self.mirror_sync.queue.push_back(ready.candidate);
                continue;
            }
            let Some(session) = ready.session.take() else {
                if ready.candidate.attempts < 3 {
                    ready.candidate.after = Instant::now() + Duration::from_secs(5);
                    self.mirror_sync.queue.push_back(ready.candidate);
                } else {
                    self.set_toast(format!("{} 새 거울 연결을 못 했어요 — 기기 목록에서 다시 열어 주세요", ready.candidate.label));
                }
                continue;
            };
            let source_rows = machines.iter().find(|m| m["base"].as_str() == Some(ready.candidate.base.as_str()))
                .map(source_rows).unwrap_or_default();
            let anchor = views.iter().find_map(|(local, source)| {
                source_rows.iter().any(|p| p.id == *source && p.room == ready.candidate.room)
                    .then(|| self.window_of_pane(local).map(|window| (local.clone(), window))).flatten()
            });
            let room_wave = (ready.candidate.base.clone(), ready.candidate.room, ready.candidate.wave);
            // Closing the destination room while connection was in flight wins,
            // including a room created by an earlier result of this same batch.
            if anchor.is_none() && (ready.candidate.requires_room
                || self.mirror_sync.opened_rooms.contains(&room_wave)) { continue; }
            let id = ready.local.clone();
            let owner = if let Some((anchor, owner)) = anchor {
                let outer = self.ws.lock().unwrap().outer_for_pty(&anchor).unwrap_or(anchor);
                let (cols, rows) = self.window_cells();
                let dir = crate::layout::pick_split_axis(cols as f32 * self.cell.w.max(1.0),
                    rows as f32 * self.cell.h.max(1.0), cols, rows);
                let layout = if owner == self.active_window { self.pty_layout.as_mut() }
                    else { self.windows.get_mut(owner).and_then(Option::as_mut) };
                if !layout.is_some_and(|l| l.split_leaf(&outer, dir, id.clone())) {
                    continue;
                }
                let mut ws = self.ws.lock().unwrap();
                if let Some(room) = ws.pane_room.get(&outer).cloned() { ws.pane_room.insert(id.clone(), room); }
                owner
            } else {
                let owner = self.windows.len();
                self.windows.push(Some(kasa_pty::PtyLayout::single(&id)));
                self.mirror_sync.opened_rooms.insert(room_wave);
                if let Some(name) = ready.candidate.room_name.clone() {
                    self.window_name_override.insert(owner, name);
                }
                owner
            };
            self.ws.lock().unwrap().panes.entry(id.clone()).or_default();
            self.insert_pty(id.clone(), session.clone());
            self.pump_pty_screens(session.screens.clone(), id.clone(), Arc::downgrade(&session));
            self.dead_panes.lock().unwrap().retain(|pane| pane != &id);
            if !ready.candidate.name.is_empty() { self.relabel_pane(&id, &ready.candidate.name); }
            self.session_touched = true;
            if owner == self.active_window {
                let (cols, rows) = self.window_cells(); self.resize_backend(cols, rows);
            }
            self.publish_pty_layout();
            self.chrome_dirty = true;
            if let Some(window) = &self.window { window.request_redraw(); }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discovery_excludes_mirrors_closed_undocked_and_nonterminal_rows() {
        let rows = source_rows(&serde_json::json!({"panes": [
            {"id":"%0", "window":0},
            {"id":"%1", "window":0, "mirror_of":"source"},
            {"id":"%2", "window":0, "closed":true},
            {"id":"%3", "window":0, "undocked":true},
            {"id":"web-1", "window":0},
            {"id":"%4", "window":null}
        ]}));
        assert_eq!(rows.len(), 6);
        assert_eq!(rows.iter().filter(|row| row.eligible).map(|row| row.id.as_str()).collect::<Vec<_>>(), vec!["%0"]);
    }
}
