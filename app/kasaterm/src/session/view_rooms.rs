//! 다른 기기 보기 방을 재시작 뒤 다시 연다. 보기 창·거울 탭은 세션 복원에서 빠진다(2026-10-14,
//! 로컬 셸로 바꿔 세우거나 남의 에이전트를 다시 돌리지 않으려고 — docs/mirror-render.md). 그래서
//! 재시작하면 보던 기기 방이 전부 사라졌다(2026-10-15 맥미니 보기 방 4 → 0). 거울을 저장본에서
//! 되살리지 않고 「어느 기기의 어느 방을 보고 있었나」만 기억했다가, 그 기기가 답하면 사이드바와
//! 같은 실시간 목록에서 그 방을 다시 연다. 끊긴 동안은 기기 절이 「끊김 · 이유」를 말한다.

use super::*;
use std::time::Duration;

/// 원본에서 그 방을 못 찾아도 이만큼은 기다린다 — 원본이 막 재시작해 칸을 되살리는 중일 수 있다.
const MISSING_GRACE: Duration = Duration::from_secs(20);
const OPEN_TRIES: u8 = 3;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ViewRoom {
    label: String,
    /// 원본의 방 번호. 원본이 재시작하면 밀릴 수 있어 칸 번호(`ids`)로 먼저 찾는다.
    window: Option<u64>,
    room: String,
    ids: Vec<String>,
    tries: u8,
    missing_since: Option<Instant>,
}

#[derive(Default)]
pub(crate) struct PendingViews {
    rooms: Vec<ViewRoom>,
    checked: Option<Instant>,
}

enum Reopen {
    Wait,
    AlreadyOpen,
    Open { window: u64, room: String },
    Gone,
}

fn parse(value: &serde_json::Value) -> Vec<ViewRoom> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| {
            let ids: Vec<String> = v["ids"].as_array()?.iter().filter_map(|id| id.as_str().map(str::to_string)).collect();
            Some(ViewRoom {
                label: v["label"].as_str().filter(|l| !l.is_empty())?.to_string(),
                window: v["window"].as_u64(),
                room: v["room"].as_str().unwrap_or("").to_string(),
                tries: 0,
                missing_since: None,
                ids: (!ids.is_empty()).then_some(ids)?,
            })
        })
        .collect()
}

fn json_of(rooms: &[ViewRoom]) -> serde_json::Value {
    rooms
        .iter()
        .map(|r| serde_json::json!({"label": r.label, "window": r.window, "room": r.room, "ids": r.ids}))
        .collect()
}

fn overlaps(a: &[String], b: &[String]) -> bool {
    a.iter().any(|id| b.contains(id))
}

/// 그 방을 지금 무엇으로 할까 — 이미 열렸나, 기기가 안 닿나, 원본의 어느 방인가, 원본에서 닫혔나.
fn judge(
    room: &mut ViewRoom,
    open: &[(String, Vec<String>)],
    machine: Option<&state::MachinesColMachine>,
) -> Reopen {
    if open.iter().any(|(label, ids)| *label == room.label && overlaps(ids, &room.ids)) {
        return Reopen::AlreadyOpen;
    }
    let Some(machine) = machine.filter(|m| m.online) else {
        room.missing_since = None;
        return Reopen::Wait;
    };
    let rows: Vec<&state::MachinesColRow> = machine
        .remote
        .iter()
        .chain(machine.mirrored.iter())
        .filter(|r| !r.closed && !r.remote_id.is_empty() && r.window.is_some())
        .collect();
    let found = rows
        .iter()
        .find(|r| room.ids.contains(&r.remote_id))
        .or_else(|| rows.iter().find(|r| room.window.is_some() && r.window == room.window && r.room == room.room));
    // 이름은 보던 때의 것(「+ 새 방」의 「방 N」·사람이 바꾼 이름)이 이긴다.
    let name = |row: &state::MachinesColRow| if room.room.is_empty() { row.room.clone() } else { room.room.clone() };
    match found {
        Some(row) => Reopen::Open { window: row.window.unwrap_or_default(), room: name(row) },
        None => match room.missing_since {
            Some(at) if at.elapsed() >= MISSING_GRACE => Reopen::Gone,
            Some(_) => Reopen::Wait,
            None => {
                room.missing_since = Some(Instant::now());
                Reopen::Wait
            }
        },
    }
}

impl App {
    fn open_view_rooms(&self) -> Vec<(String, Vec<String>)> {
        (0..self.windows.len()).filter_map(|i| self.remote_view_of_window(i)).collect()
    }

    /// 저장본에 싣는 보기 방 — 지금 열린 것과, 아직 못 다시 연 것(그 사이 또 재시작해도 안 잃게).
    pub(crate) fn view_rooms_json(&self) -> serde_json::Value {
        let mut rooms: Vec<ViewRoom> = (0..self.windows.len())
            .filter_map(|i| {
                let (label, ids) = self.remote_view_of_window(i)?;
                let window = ids.iter().find_map(|id| {
                    kasa_mcp::machines::cached_pane(&label, id)?.get("window")?.as_u64()
                });
                Some(ViewRoom {
                    room: self.window_name_override.get(&i).cloned().unwrap_or_default(),
                    label,
                    window,
                    ids,
                    tries: 0,
                    missing_since: None,
                })
            })
            .collect();
        for pending in &self.pending_views.rooms {
            if !rooms.iter().any(|r| r.label == pending.label && overlaps(&r.ids, &pending.ids)) {
                rooms.push(pending.clone());
            }
        }
        json_of(&rooms)
    }

    pub(crate) fn remember_view_rooms(&mut self, state: &serde_json::Value) {
        self.pending_views = PendingViews { rooms: parse(&state["view_rooms"]), checked: None };
    }

    pub(crate) fn has_pending_view_rooms(&self) -> bool {
        !self.pending_views.rooms.is_empty()
    }

    pub(crate) fn pending_view_count(&self, label: &str) -> usize {
        self.pending_views.rooms.iter().filter(|r| r.label == label).count()
    }

    /// 기다리던 보기 방을 그 기기가 답하는 대로 다시 연다. 보던 방은 그대로 둔다.
    pub(crate) fn reopen_view_rooms(&mut self) {
        if self.pending_views.rooms.is_empty()
            || self.tmux.is_some()
            || self.restore_prompt.is_some()
            || self.restoration_blocks_input()
            || self.close_freeze.live()
            || self.internal_room_active_any()
            || self.pending_views.checked.is_some_and(|at| at.elapsed() < Duration::from_secs(1))
        {
            return;
        }
        self.pending_views.checked = Some(Instant::now());
        let mut open = self.open_view_rooms();
        let back = self.active_window;
        let mut reopened = Vec::new();
        let mut keep = Vec::new();
        for mut room in std::mem::take(&mut self.pending_views.rooms) {
            let machine = self.info.machines_col.machines.iter().find(|m| m.label == room.label);
            match judge(&mut room, &open, machine) {
                Reopen::Wait => keep.push(room),
                Reopen::AlreadyOpen => {}
                Reopen::Gone => eprintln!(
                    "[view-rooms] {} 의 「{}」 — 원본에서 닫혀 다시 안 연다({})",
                    room.label, room.room, room.ids.join(",")
                ),
                Reopen::Open { window, room: name } => {
                    match self.open_remote_room(&room.label, Some(window), &name, None) {
                        Ok(()) => {
                            open.push((room.label.clone(), room.ids.clone()));
                            reopened.push(room.label.clone());
                        }
                        Err(e) => {
                            room.tries += 1;
                            eprintln!("[view-rooms] {} 의 「{name}」 다시 열기 실패({}번째): {e:#}", room.label, room.tries);
                            if room.tries < OPEN_TRIES {
                                keep.push(room);
                            }
                        }
                    }
                }
            }
        }
        self.pending_views.rooms = keep;
        if reopened.is_empty() {
            return;
        }
        if back < self.windows.len() && self.active_window != back {
            self.switch_window(back);
        }
        reopened.dedup();
        let names: Vec<String> = reopened.iter().map(|l| crate::render::pane_identity::device_name(l)).collect();
        self.set_toast(format!("{} 보기 방을 다시 열었어요", names.join(", ")));
        self.session_touched = true;
        self.chrome_dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, window: u64, room: &str) -> state::MachinesColRow {
        state::MachinesColRow {
            pane: String::new(), remote_id: id.into(), remote_cwd: String::new(), name: String::new(),
            title: String::new(), status: String::new(), room: room.into(), closed: false, rect: None,
            window: Some(window), tab_of: None, attention_kind: None,
        }
    }

    fn machine(online: bool, remote: Vec<state::MachinesColRow>) -> state::MachinesColMachine {
        state::MachinesColMachine {
            label: "미니".into(), online, ago_secs: None, outdated: false, host: String::new(), kvm: None,
            mirrored: Vec::new(), remote, closed: 0, web_shells: Vec::new(), link: Default::default(),
        }
    }

    fn saved() -> ViewRoom {
        parse(&serde_json::json!([{"label": "미니", "window": 3, "room": "nacho", "ids": ["%5", "%6"]}])).remove(0)
    }

    #[test]
    fn saved_view_rooms_round_trip_and_skip_broken_entries() {
        let value = serde_json::json!([
            {"label": "미니", "window": 3, "room": "nacho", "ids": ["%5"]},
            {"label": "", "ids": ["%1"]},
            {"label": "미니", "ids": []},
        ]);
        let rooms = parse(&value);
        assert_eq!(rooms.len(), 1);
        assert_eq!(json_of(&rooms), serde_json::json!([{"label": "미니", "window": 3, "room": "nacho", "ids": ["%5"]}]));
    }

    #[test]
    fn waits_while_the_device_is_away_and_finds_the_room_by_pane_first() {
        let mut room = saved();
        assert!(matches!(judge(&mut room, &[], None), Reopen::Wait));
        assert!(matches!(judge(&mut room, &[], Some(&machine(false, vec![row("%5", 3, "nacho")]))), Reopen::Wait));
        // 원본이 재시작해 방 번호가 밀려도 칸 번호로 찾는다.
        let moved = machine(true, vec![row("%1", 3, "other"), row("%6", 7, "nacho")]);
        assert!(matches!(judge(&mut room, &[], Some(&moved)), Reopen::Open { window: 7, .. }));
        let open = vec![("미니".to_string(), vec!["%6".to_string()])];
        assert!(matches!(judge(&mut room, &open, Some(&moved)), Reopen::AlreadyOpen));
    }

    #[test]
    fn a_room_closed_on_the_source_is_dropped_only_after_the_grace() {
        let mut room = saved();
        let without = machine(true, vec![row("%9", 1, "다른 방")]);
        assert!(matches!(judge(&mut room, &[], Some(&without)), Reopen::Wait));
        room.missing_since = Some(Instant::now() - MISSING_GRACE);
        assert!(matches!(judge(&mut room, &[], Some(&without)), Reopen::Gone));
    }
}
