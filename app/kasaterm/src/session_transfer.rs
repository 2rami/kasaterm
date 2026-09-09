//! 세션의 실행 기계와 현재 정체를 기준으로 이사·방 배치를 검증한다.
pub(crate) use kasa_socket::transfer::*;

use anyhow::{anyhow, Result};
use kasa_socket::backend::Backend;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn machine_row(snapshot: &MachineSnapshot, local: bool) -> TransferMachine {
    TransferMachine {
        id: snapshot.machine_id.clone(), label: snapshot.label.clone(), local, online: true,
        room_transfer_supported: snapshot.room_transfer_supported, rooms: snapshot.rooms.clone(),
        unavailable_reason: (!snapshot.room_transfer_supported).then(|| "이 기계에 새 판이 필요해요".into()),
    }
}

fn deduplicate(rows: Vec<SessionRow>) -> Vec<SessionRow> {
    let mut out: Vec<SessionRow> = Vec::new();
    let mut keys = HashMap::new();
    for row in rows {
        let key = row.identity.canonical_key();
        if let Some(&index) = keys.get(&key) {
            let current: &mut SessionRow = &mut out[index];
            for pane in row.local_panes {
                if !current.local_panes.contains(&pane) { current.local_panes.push(pane); }
            }
        } else {
            keys.insert(key, out.len());
            out.push(row);
        }
    }
    out
}

fn import_remote_sessions(snapshot: MachineSnapshot) -> Vec<SessionRow> {
    snapshot.sessions.into_iter().filter(|row| row.identity.machine_id == snapshot.machine_id)
        .map(|mut row| {
            // 원격 응답의 local_panes는 상대 기계의 번호라 이쪽 포커스 주소가 아니다.
            row.local_panes.clear();
            row
        }).collect()
}

fn attach_local_mirror(rows: &mut Vec<SessionRow>, machine: &str, origin: &str, local: String, cwd: String) {
    if let Some(row) = rows.iter_mut().find(|row| row.identity.machine_id == machine && row.identity.pane_id == origin) {
        if !row.local_panes.contains(&local) { row.local_panes.push(local); }
    } else {
        rows.push(SessionRow {
            identity: SessionIdentity { machine_id: machine.into(), pane_id: origin.into(), ..Default::default() },
            local_panes: vec![local], cwd, status: "unknown".into(),
            unavailable_reason: Some("원본 세션의 현재 상태를 확인하지 못했어요".into()), ..Default::default()
        });
    }
}

pub(crate) fn collect(backend: &Arc<dyn Backend>) -> TransferSnapshot {
    let mut result = TransferSnapshot::default();
    match backend.transfer_snapshot() {
        Ok(local) => {
            result.machines.push(machine_row(&local, true));
            result.sessions.extend(local.sessions.into_iter().filter(|row| row.identity.machine_id == local.machine_id));
        }
        Err(error) => result.errors.push(format!("이 기계: {error:#}")),
    }
    let cached = kasa_mcp::machines::snapshot();
    let machines = kasa_mcp::machines::machines();
    let replies = std::thread::scope(|scope| {
        let jobs: Vec<_> = machines.iter().map(|machine| {
            scope.spawn(move || (machine, kasa_mcp::remote::transfer_snapshot(&machine.base)))
        }).collect();
        jobs.into_iter().filter_map(|job| job.join().ok()).collect::<Vec<_>>()
    });
    let mut bases = HashMap::new();
    for (machine, reply) in replies {
        match reply {
            Ok(snapshot) => {
                bases.insert(machine.base.clone(), snapshot.machine_id.clone());
                result.machines.push(machine_row(&snapshot, false));
                result.sessions.extend(import_remote_sessions(snapshot));
            }
            Err(error) => {
                let old = cached.iter().find(|row| row.get("label").and_then(|v| v.as_str()) == Some(&machine.label));
                let id = old.and_then(|row| row.get("route")).and_then(|v| v.as_str())
                    .and_then(|route| route.strip_prefix('~')).map(str::to_string)
                    .or_else(|| machine.machine_id.clone()).unwrap_or_else(|| machine.base.clone());
                bases.insert(machine.base.clone(), id.clone());
                let reason = format!("현재 상태를 안전하게 확인할 수 없어요: {error:#}");
                result.machines.push(TransferMachine {
                    id: id.clone(), label: machine.label.clone(), local: false,
                    online: old.and_then(|row| row.get("online")).and_then(|v| v.as_bool()).unwrap_or(false),
                    room_transfer_supported: false, rooms: Vec::new(), unavailable_reason: Some(reason.clone()),
                });
                if let Some(panes) = old.and_then(|row| row.get("panes")).and_then(|v| v.as_array()) {
                    for pane in panes {
                        let text = |name| pane.get(name).and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let pane_id = text("id");
                        if pane_id.is_empty() { continue; }
                        result.sessions.push(SessionRow {
                            identity: SessionIdentity { machine_id: id.clone(), pane_id, ..Default::default() },
                            name: text("character"), title: text("title"), cwd: text("cwd"),
                            status: "unknown".into(), unavailable_reason: Some(reason.clone()), ..Default::default()
                        });
                    }
                }
            }
        }
    }
    for pane in kasa_pty::live_sessions() {
        let Some(info) = kasa_mcp::remote::remote_info(&pane) else { continue };
        let Some(machine_id) = bases.get(&info.base) else { continue };
        attach_local_mirror(&mut result.sessions, machine_id, &info.remote_id, pane, info.remote_cwd.unwrap_or_default());
    }
    result.sessions = deduplicate(result.sessions);
    result
}

fn resolve(backend: &Arc<dyn Backend>, machine_id: &str) -> Result<Option<kasa_mcp::machines::Machine>> {
    let local = backend.transfer_snapshot()?;
    if local.machine_id == machine_id { return Ok(None); }
    kasa_mcp::machines::find_route(&format!("~{machine_id}")).map(Some)
        .ok_or_else(|| anyhow!("기계 연결 정체가 바뀌었어요. 목록을 다시 확인해 주세요"))
}

fn snapshot_at(backend: &Arc<dyn Backend>, machine: Option<&kasa_mcp::machines::Machine>) -> Result<MachineSnapshot> {
    match machine { Some(machine) => kasa_mcp::remote::transfer_snapshot(&machine.base), None => backend.transfer_snapshot() }
}

fn current_row(snapshot: &MachineSnapshot, identity: &SessionIdentity) -> Result<SessionRow> {
    snapshot.sessions.iter().find(|row| row.identity == *identity).cloned()
        .ok_or_else(|| anyhow!("선택 뒤 세션이 바뀌거나 종료됐어요. 목록을 다시 확인해 주세요"))
}

fn verify_destination_room(target: &RoomTarget, actual: &str, rooms: &[RoomInfo]) -> Result<()> {
    let room = rooms.iter().find(|room| room.id == actual)
        .ok_or_else(|| anyhow!("도착 세션의 방을 확인하지 못했어요"))?;
    let matches = match target {
        RoomTarget::Existing(expected) => actual == expected,
        RoomTarget::New(expected) => room.title.trim() == expected.trim(),
    };
    if !matches { anyhow::bail!("기계는 옮겨졌지만 확인한 도착 방과 달라요. 결과를 확인해 주세요"); }
    Ok(())
}

fn report(source: &SessionIdentity, status: TransferStatus, message: impl Into<String>, destination: Option<SessionIdentity>) -> TransferResult {
    TransferResult { source: source.clone(), status, message: message.into(), destination }
}

pub(crate) fn execute_with_progress(
    backend: &Arc<dyn Backend>, request: TransferRequest, mut progress: impl FnMut(TransferResult),
) -> Vec<TransferResult> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut room = request.destination_room.clone();
    for source in request.sessions {
        if !seen.insert(source.canonical_key()) { continue; }
        let result = (|| -> Result<TransferResult> {
            if !request.confirmed { anyhow::bail!("이사 확인이 필요해요"); }
            if source.machine_id == request.destination_machine { anyhow::bail!("현재 기기와 같아요. 기존 방 이동을 이용해 주세요"); }
            let origin = resolve(backend, &source.machine_id)?;
            let destination = resolve(backend, &request.destination_machine)?;
            let origin_snapshot = snapshot_at(backend, origin.as_ref())?;
            let row = current_row(&origin_snapshot, &source)?;
            let dest_snapshot = snapshot_at(backend, destination.as_ref())?;
            let previous_destinations: HashSet<String> = dest_snapshot.sessions.iter()
                .map(|row| row.identity.canonical_key()).collect();
            if !origin_snapshot.room_transfer_supported || !dest_snapshot.room_transfer_supported {
                anyhow::bail!("출발·도착 기계 모두 도착 방 선택을 지원하는 새 판이 필요해요");
            }
            if let RoomTarget::Existing(id) = &room {
                if !dest_snapshot.rooms.iter().any(|item| &item.id == id) { anyhow::bail!("도착 방이 바뀌었어요. 다시 골라 주세요"); }
            }
            if row.unavailable_reason.is_some() || row.status == "unknown" { anyhow::bail!("세션 상태를 확인할 수 없어 이사를 시작하지 않았어요"); }
            progress(report(&source, TransferStatus::Running, "도착 방을 준비하고 이사하고 있어요", None));
            let migrate = MigrateRequest { session: source.clone(), destination_machine: request.destination_machine.clone(), room: room.clone() };
            let response = match &origin {
                Some(machine) => kasa_mcp::remote::transfer_migrate(&machine.base, &migrate),
                None => backend.transfer_migrate(&migrate),
            };
            let remote_id = match response {
                Ok(value) if value.starts_with("예약됨") => {
                    progress(report(&source, TransferStatus::Waiting, "현재 작업이 끝나기를 기다리고 있어요", None));
                    None
                }
                Ok(value) if value.starts_with('%') || value.starts_with("web-") => Some(value),
                Ok(_) => return Ok(report(&source, TransferStatus::Unknown, "요청은 전달됐지만 완료를 확인하지 못했어요", None)),
                Err(error) => return Ok(report(&source, TransferStatus::Unknown, format!("이사 결과를 확인해야 해요: {error:#}"), None)),
            };
            let deadline = Instant::now() + Duration::from_secs(if remote_id.is_some() { 30 } else { 300 });
            loop {
                let dest = snapshot_at(backend, destination.as_ref())?;
                let found = dest.sessions.iter().find(|candidate| {
                    candidate.identity.machine_id == request.destination_machine
                        && !previous_destinations.contains(&candidate.identity.canonical_key())
                        && remote_id.as_ref().map_or_else(
                            || source.session_id.as_ref().is_some_and(|sid| candidate.identity.session_id.as_ref() == Some(sid)),
                            |id| &candidate.identity.pane_id == id,
                        )
                        && source.session_id.as_ref().map_or(candidate.harness.is_none(), |sid| candidate.identity.session_id.as_ref() == Some(sid))
                });
                if let Some(found) = found {
                    let Some(actual_room) = &found.room_id else { anyhow::bail!("도착 세션의 방을 확인하지 못했어요"); };
                    verify_destination_room(&room, actual_room, &dest.rooms)?;
                    room = RoomTarget::Existing(actual_room.clone());
                    return Ok(report(&source, TransferStatus::Succeeded, "선택한 기계의 방에 도착했어요", Some(found.identity.clone())));
                }
                if Instant::now() >= deadline {
                    return Ok(report(&source, TransferStatus::Unknown, "완료를 아직 확인하지 못했어요. 대기나 실패를 성공으로 처리하지 않았어요", None));
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        })().unwrap_or_else(|error| report(&source, TransferStatus::Failed, format!("{error:#}"), None));
        progress(result.clone());
        out.push(result);
    }
    out
}

pub(crate) fn close_shells_with_progress(
    backend: &Arc<dyn Backend>, sessions: Vec<SessionIdentity>, confirmed: bool, mut progress: impl FnMut(TransferResult),
) -> Vec<TransferResult> {
    let mut seen = HashSet::new();
    sessions.into_iter().filter(|source| seen.insert(source.canonical_key())).map(|source| {
        let result = (|| -> Result<()> {
            if !confirmed { anyhow::bail!("셸 닫기 확인이 필요해요"); }
            let machine = resolve(backend, &source.machine_id)?;
            let row = current_row(&snapshot_at(backend, machine.as_ref())?, &source)?;
            if !row.shell_closeable || row.harness.is_some() || row.status == "unknown" {
                anyhow::bail!("빈 셸임을 확인하지 못했거나 프로그램이 실행 중이에요");
            }
            match machine {
                Some(machine) => kasa_mcp::remote::transfer_close(&machine.base, &source),
                None => backend.transfer_close(&source),
            }
        })();
        let result = match result {
            Ok(()) => report(&source, TransferStatus::Succeeded, "확인한 빈 셸을 닫았어요", None),
            Err(error) => report(&source, TransferStatus::Failed, format!("{error:#}"), None),
        };
        progress(result.clone());
        result
    }).collect()
}

pub(crate) fn focus_session(backend: &Arc<dyn Backend>, identity: &SessionIdentity) -> Result<String> {
    let snapshot = collect(backend);
    let row = snapshot.sessions.iter().find(|row| row.identity == *identity)
        .ok_or_else(|| anyhow!("세션이 바뀌었어요. 목록을 다시 확인해 주세요"))?;
    if let Some(pane) = row.local_panes.first() {
        backend.focus_surface(pane)?;
        return Ok("세션을 열었어요".into());
    }
    let Some(machine) = resolve(backend, &identity.machine_id)? else {
        backend.focus_surface(&identity.pane_id)?;
        return Ok("세션을 열었어요".into());
    };
    backend.remote_pane(&machine.base, Some(&row.cwd), Some(&identity.pane_id), None, false, None)?;
    Ok("원격 세션을 열었어요".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(machine: &str, pane: &str, token: &str) -> SessionRow {
        SessionRow {
            identity: SessionIdentity { machine_id: machine.into(), pane_id: pane.into(), token: token.into(), ..Default::default() },
            name: "같은 이름".into(), cwd: "/same/project".into(), ..Default::default()
        }
    }

    #[test]
    fn canonical_origin_merges_mirrors_but_not_equal_names_or_paths() {
        let mut first = row("mini", "%8", "live");
        first.local_panes = vec!["%20".into()];
        let mut mirror = first.clone();
        mirror.local_panes = vec!["%21".into()];
        let rows = deduplicate(vec![first, mirror, row("mini", "%9", "other"), row("macbook", "%8", "another")]);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].local_panes, vec!["%20", "%21"]);
    }

    #[test]
    fn revalidation_rejects_reused_pane_and_changed_session() {
        let old = row("mini", "%8", "old");
        let mut snapshot = MachineSnapshot { sessions: vec![row("mini", "%8", "new")], ..Default::default() };
        assert!(current_row(&snapshot, &old.identity).is_err());
        snapshot.sessions[0] = old.clone();
        snapshot.sessions[0].identity.session_id = Some("new-session".into());
        assert!(current_row(&snapshot, &old.identity).is_err());
        snapshot.sessions[0] = old.clone();
        assert!(current_row(&snapshot, &old.identity).is_ok());
    }

    #[test]
    fn remote_pane_number_is_not_a_local_address() {
        let mut local = row("macbook", "%4", "local");
        local.local_panes = vec!["%4".into()];
        let mut remote = row("mini", "%4", "remote");
        remote.local_panes = vec!["%4".into()];
        let mut rows = vec![local];
        rows.extend(import_remote_sessions(MachineSnapshot { machine_id: "mini".into(), sessions: vec![remote], ..Default::default() }));
        assert_eq!(rows[0].local_panes, vec!["%4"]);
        assert!(rows[1].local_panes.is_empty());
        attach_local_mirror(&mut rows, "mini", "%4", "%18".into(), "/project".into());
        assert_eq!(rows[0].local_panes, vec!["%4"]);
        assert_eq!(rows[1].local_panes, vec!["%18"]);
    }

    #[test]
    fn a_matching_session_in_a_different_new_room_is_not_success() {
        let rooms = vec![RoomInfo { id: "actual".into(), title: "다른 요청의 방".into() }];
        assert!(verify_destination_room(&RoomTarget::New("먼저 확인한 방".into()), "actual", &rooms).is_err());
        assert!(verify_destination_room(&RoomTarget::Existing("missing".into()), "actual", &rooms).is_err());
        assert!(verify_destination_room(&RoomTarget::New("다른 요청의 방".into()), "actual", &rooms).is_ok());
    }
}
