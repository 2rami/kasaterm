//! 세션의 실행 기계와 현재 정체를 기준으로 이사·방 배치를 검증한다.
pub(crate) use kasa_socket::transfer::*;

use anyhow::{anyhow, Result};
use kasa_socket::backend::Backend;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

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
        let mut submitted = false;
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
            submitted = true;
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
        })().unwrap_or_else(|error| report(&source, if submitted { TransferStatus::Unknown } else { TransferStatus::Failed }, format!("{error:#}"), None));
        progress(result.clone());
        out.push(result);
    }
    out
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
    fn a_matching_session_in_a_different_new_room_is_not_success() {
        let rooms = vec![RoomInfo { id: "actual".into(), title: "다른 요청의 방".into() }];
        assert!(verify_destination_room(&RoomTarget::New("먼저 확인한 방".into()), "actual", &rooms).is_err());
        assert!(verify_destination_room(&RoomTarget::Existing("missing".into()), "actual", &rooms).is_err());
        assert!(verify_destination_room(&RoomTarget::New("다른 요청의 방".into()), "actual", &rooms).is_ok());
    }
}
