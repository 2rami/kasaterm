use super::*;
use crate::session_transfer::{
    MachineSnapshot, RoomTarget, SessionIdentity, SessionRow, TransferRequest, TransferStatus,
};
use kasa_socket::backend::Backend;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum DragEndpoint {
    LocalPane(String),
    RemotePane {
        label: String,
        base: String,
        pane: String,
    },
}

impl DragEndpoint {
    fn origin_key(&self) -> String {
        match self {
            Self::LocalPane(_) => "local".into(),
            Self::RemotePane { base, .. } => format!("remote:{base}"),
        }
    }

    fn pane(&self) -> &str {
        match self {
            Self::LocalPane(pane) | Self::RemotePane { pane, .. } => pane,
        }
    }

    fn snapshot(&self, backend: &Arc<dyn Backend>) -> anyhow::Result<MachineSnapshot> {
        match self {
            Self::LocalPane(_) => backend.transfer_snapshot(),
            Self::RemotePane { label, base, .. } => {
                let current = kasa_mcp::machines::find(label)
                    .ok_or_else(|| anyhow::anyhow!("기계 연결이 없어졌어요. 다시 골라 주세요"))?;
                if current.base != *base {
                    anyhow::bail!("기계 연결이 바뀌었어요. 다시 골라 주세요");
                }
                let snapshot = kasa_mcp::remote::transfer_snapshot(base)?;
                if current
                    .machine_id
                    .as_ref()
                    .is_some_and(|id| id != &snapshot.machine_id)
                {
                    anyhow::bail!("기계 정체가 바뀌었어요. 다시 골라 주세요");
                }
                Ok(snapshot)
            }
        }
    }
}

fn endpoint_row<'a>(snapshot: &'a MachineSnapshot, pane: &str) -> anyhow::Result<&'a SessionRow> {
    let mut rows = snapshot.sessions.iter().filter(|row| {
        row.identity.machine_id == snapshot.machine_id && row.identity.pane_id == pane
    });
    let row = rows
        .next()
        .ok_or_else(|| anyhow::anyhow!("선택한 창이 바뀌거나 종료됐어요"))?;
    if rows.next().is_some() || row.identity.token.is_empty() || row.identity.instance.is_empty() {
        anyhow::bail!("선택한 창의 현재 정체를 확인하지 못했어요");
    }
    Ok(row)
}

fn request_from_snapshots(
    source: &MachineSnapshot,
    source_pane: &str,
    destination: &MachineSnapshot,
    destination_pane: &str,
) -> anyhow::Result<TransferRequest> {
    if source.machine_id == destination.machine_id {
        anyhow::bail!("같은 기계 안에서는 방 이동을 이용해 주세요");
    }
    if !source.room_transfer_supported || !destination.room_transfer_supported {
        anyhow::bail!("두 기계 모두 도착 방 선택을 지원하는 새 판이 필요해요");
    }
    let origin = endpoint_row(source, source_pane)?;
    let anchor = endpoint_row(destination, destination_pane)?;
    if origin.unavailable_reason.is_some() || origin.status == "unknown" {
        anyhow::bail!("출발 세션 상태를 확인할 수 없어 이사를 시작하지 않았어요");
    }
    match origin.harness.as_deref() {
        Some("claude" | "codex") => {}
        Some(harness) => anyhow::bail!("이사는 Claude·Codex 대화만 지원해요 ({harness})"),
        None => anyhow::bail!("빈 셸은 기기 간 대화 이사 대상이 아니에요"),
    }
    if origin
        .identity
        .session_id
        .as_ref()
        .is_none_or(|id| id.is_empty())
    {
        anyhow::bail!("출발 대화의 세션 번호를 확인하지 못했어요. 잠시 뒤 다시 끌어 주세요");
    }
    let room = anchor
        .room_id
        .as_ref()
        .filter(|id| destination.rooms.iter().any(|room| &room.id == *id))
        .ok_or_else(|| anyhow::anyhow!("도착 방이 바뀌었어요. 다시 골라 주세요"))?;
    Ok(TransferRequest {
        sessions: vec![origin.identity.clone()],
        destination_machine: destination.machine_id.clone(),
        destination_room: RoomTarget::Existing(room.clone()),
        confirmed: true,
    })
}

#[derive(Default)]
struct Mailbox {
    pending: std::collections::HashSet<DragEndpoint>,
    warming: std::collections::HashSet<String>,
    refreshed: std::collections::HashMap<String, Instant>,
    observed: std::collections::HashMap<DragEndpoint, (SessionIdentity, Option<String>, Instant)>,
    pinned: std::collections::HashMap<DragEndpoint, SessionIdentity>,
    messages: Vec<String>,
}

fn verify_drag_identity(
    snapshot: &MachineSnapshot,
    pane: &str,
    expected: &SessionIdentity,
) -> anyhow::Result<()> {
    if endpoint_row(snapshot, pane)?.identity != *expected {
        anyhow::bail!("끌기를 시작한 뒤 세션이 바뀌었어요. 새 상태를 확인하고 다시 끌어 주세요");
    }
    Ok(())
}

fn mailbox() -> &'static Mutex<Mailbox> {
    static MAILBOX: OnceLock<Mutex<Mailbox>> = OnceLock::new();
    MAILBOX.get_or_init(|| Mutex::new(Mailbox::default()))
}

fn remote_drop_command(
    source: &str,
    target: &str,
    zone: DropZone,
) -> (&'static str, serde_json::Value) {
    let direction = match zone {
        DropZone::Center => {
            return (
                "surface.swap",
                serde_json::json!({ "a": source, "b": target }),
            )
        }
        DropZone::Left => "left",
        DropZone::Right => "right",
        DropZone::Up => "up",
        DropZone::Down => "down",
    };
    (
        "surface.move",
        serde_json::json!({ "surface_id": source, "target": target, "direction": direction }),
    )
}

impl App {
    pub(crate) fn warm_drag_identity(&self, endpoint: DragEndpoint) {
        let Some(backend) = self.socket_backend.clone() else {
            return;
        };
        let key = endpoint.origin_key();
        {
            let mut state = mailbox().lock().unwrap();
            if state
                .refreshed
                .get(&key)
                .is_some_and(|at| at.elapsed() < Duration::from_secs(2))
                || !state.warming.insert(key.clone())
            {
                return;
            }
            state.refreshed.insert(key.clone(), Instant::now());
        }
        let backend: Arc<dyn Backend> = backend;
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let started = Instant::now();
            let snapshot = endpoint.snapshot(&backend);
            let mut state = mailbox().lock().unwrap();
            state
                .observed
                .retain(|endpoint, _| endpoint.origin_key() != key);
            if let Ok(snapshot) = snapshot {
                for row in &snapshot.sessions {
                    if row.identity.machine_id != snapshot.machine_id
                        || row.status == "unknown"
                        || row.unavailable_reason.is_some()
                        || endpoint_row(&snapshot, &row.identity.pane_id).is_err()
                    {
                        continue;
                    }
                    let endpoint = match &endpoint {
                        DragEndpoint::LocalPane(_) => {
                            DragEndpoint::LocalPane(row.identity.pane_id.clone())
                        }
                        DragEndpoint::RemotePane { label, base, .. } => DragEndpoint::RemotePane {
                            label: label.clone(),
                            base: base.clone(),
                            pane: row.identity.pane_id.clone(),
                        },
                    };
                    state.observed.insert(
                        endpoint,
                        (row.identity.clone(), row.room_id.clone(), started),
                    );
                }
            }
            state.warming.remove(&key);
            drop(state);
            let _ = proxy.send_event(UserEvent::Redraw);
        });
    }

    pub(crate) fn begin_drag_identity(&mut self, endpoint: DragEndpoint) -> bool {
        let pinned = {
            let mut state = mailbox().lock().unwrap();
            state.pinned.clear();
            let observed = state
                .observed
                .get(&endpoint)
                .filter(|(_, _, at)| at.elapsed() <= Duration::from_secs(5))
                .map(|(identity, _, _)| identity.clone());
            if let Some(identity) = observed {
                state.pinned.insert(endpoint.clone(), identity);
                true
            } else {
                false
            }
        };
        self.warm_drag_identity(endpoint);
        pinned
    }

    pub(crate) fn route_drag_move(
        &mut self,
        source: DragEndpoint,
        destination: DragEndpoint,
        zone: DropZone,
    ) -> bool {
        let expected = mailbox().lock().unwrap().pinned.remove(&source);
        if matches!(
            (&source, &destination),
            (DragEndpoint::LocalPane(_), DragEndpoint::LocalPane(_))
        ) {
            return false;
        }
        let Some(expected) = expected else {
            self.warm_drag_identity(source);
            self.set_toast(
                "끌기 시작 시점의 세션을 확인하지 못했어요. 잠시 뒤 다시 끌어 주세요".into(),
            );
            return true;
        };
        let target_expected = mailbox()
            .lock()
            .unwrap()
            .observed
            .get(&destination)
            .filter(|(_, _, at)| at.elapsed() <= Duration::from_secs(5))
            .map(|(identity, room, _)| (identity.clone(), room.clone()));
        let Some((target_expected, target_room)) = target_expected else {
            self.warm_drag_identity(destination);
            self.set_toast(
                "도착 방의 세션 정체를 확인하고 있어요. 잠시 뒤 다시 끌어 주세요".into(),
            );
            return true;
        };
        match (&source, &destination) {
            (DragEndpoint::LocalPane(_), DragEndpoint::LocalPane(_)) => return false,
            (
                DragEndpoint::RemotePane {
                    base: source_base, ..
                },
                DragEndpoint::RemotePane {
                    base: target_base, ..
                },
            ) if source_base == target_base => {
                let Some(backend) = self.socket_backend.clone() else {
                    self.set_toast("이 백엔드에서는 원격 방 이동을 지원하지 않아요".into());
                    return true;
                };
                if source.pane() == destination.pane() {
                    return true;
                }
                if !mailbox().lock().unwrap().pending.insert(source.clone()) {
                    self.set_toast("이 창의 이동 결과를 확인하고 있어요".into());
                    return true;
                }
                let backend: Arc<dyn Backend> = backend;
                let proxy = self.proxy.clone();
                self.remote_view_push_at = Some(Instant::now());
                self.set_toast("원격 방으로 창을 옮기고 있어요".into());
                std::thread::spawn(move || {
                    let mut submitted = false;
                    let result = (|| -> anyhow::Result<bool> {
                        let origin = source.snapshot(&backend)?;
                        verify_drag_identity(&origin, source.pane(), &expected)?;
                        let target = destination.snapshot(&backend)?;
                        verify_drag_identity(&target, destination.pane(), &target_expected)?;
                        let source_row = endpoint_row(&origin, source.pane())?;
                        let target_row = endpoint_row(&target, destination.pane())?;
                        if target_row.room_id != target_room {
                            anyhow::bail!("도착 창의 방이 바뀌었어요. 다시 끌어 주세요");
                        }
                        let cross_room = source_row.room_id != target_room;
                        if cross_room && target_room.is_none() {
                            anyhow::bail!("도착 방을 확인하지 못했어요. 다시 골라 주세요");
                        }
                        if origin.machine_id != target.machine_id
                            || origin.instance != target.instance
                        {
                            anyhow::bail!("원격 기계 정체가 바뀌었어요. 다시 골라 주세요");
                        }
                        if !target
                            .sessions
                            .iter()
                            .any(|row| row.identity == source_row.identity)
                        {
                            anyhow::bail!("출발 창이 바뀌었어요. 다시 골라 주세요");
                        }
                        let DragEndpoint::RemotePane { base, .. } = &source else {
                            unreachable!()
                        };
                        let (method, params) = remote_drop_command(
                            &source_row.identity.pane_id,
                            &target_row.identity.pane_id,
                            zone,
                        );
                        submitted = true;
                        kasa_mcp::remote::remote_cmd(base, method, params)?;
                        if cross_room {
                            let deadline = Instant::now() + Duration::from_secs(10);
                            loop {
                                let current = source.snapshot(&backend)?;
                                verify_drag_identity(&current, source.pane(), &expected)?;
                                if endpoint_row(&current, source.pane())?.room_id == target_room {
                                    return Ok(true);
                                }
                                if Instant::now() >= deadline {
                                    anyhow::bail!(
                                        "요청은 보냈지만 도착 방을 아직 확인하지 못했어요"
                                    );
                                }
                                std::thread::sleep(Duration::from_millis(500));
                            }
                        }
                        Ok(false)
                    })();
                    let unknown = submitted && result.is_err();
                    let message = match result {
                        Ok(true) => "선택한 원격 방에 도착한 것을 확인했어요".into(),
                        Ok(false) => "자리 이동 요청을 보냈어요".into(),
                        Err(error) => format!("창 이동 결과를 확인해 주세요: {error:#}"),
                    };
                    let mut state = mailbox().lock().unwrap();
                    state.messages.push(message);
                    if !unknown {
                        state.pending.remove(&source);
                    }
                    drop(state);
                    kasa_mcp::machines::poke();
                    let _ = proxy.send_event(UserEvent::Redraw);
                });
                return true;
            }
            _ => {}
        }
        self.queue_drag_transfer(source, destination, expected, target_expected, target_room);
        true
    }

    pub(crate) fn drag_endpoint(&self, leaf: &str) -> DragEndpoint {
        let pane = self.leaf_pty_id(leaf);
        self.drag_endpoint_for_pid(&pane)
    }

    pub(crate) fn drag_endpoint_for_pid(&self, pane: &str) -> DragEndpoint {
        match kasa_mcp::remote::remote_info(pane) {
            Some(info) => DragEndpoint::RemotePane {
                label: info.label,
                base: info.base,
                pane: info.remote_id,
            },
            None => DragEndpoint::LocalPane(pane.to_string()),
        }
    }

    fn queue_drag_transfer(
        &mut self,
        source: DragEndpoint,
        destination: DragEndpoint,
        expected: SessionIdentity,
        target_expected: SessionIdentity,
        target_room: Option<String>,
    ) -> bool {
        let Some(backend) = self.socket_backend.clone() else {
            self.set_toast("이 백엔드에서는 기기 간 이사를 지원하지 않아요".into());
            return true;
        };
        if !mailbox().lock().unwrap().pending.insert(source.clone()) {
            self.set_toast("이 창의 이사 결과를 확인하고 있어요".into());
            return true;
        }
        self.set_toast("출발 세션과 도착 방을 확인하고 있어요".into());
        let backend: Arc<dyn Backend> = backend;
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let prepared = (|| -> anyhow::Result<TransferRequest> {
                let origin = source.snapshot(&backend)?;
                verify_drag_identity(&origin, source.pane(), &expected)?;
                let target = destination.snapshot(&backend)?;
                verify_drag_identity(&target, destination.pane(), &target_expected)?;
                if endpoint_row(&target, destination.pane())?.room_id != target_room {
                    anyhow::bail!("도착 창의 방이 바뀌었어요. 다시 끌어 주세요");
                }
                request_from_snapshots(&origin, source.pane(), &target, destination.pane())
            })();
            let mut unknown = false;
            match prepared {
                Ok(request) => {
                    crate::session_transfer::execute_with_progress(&backend, request, |result| {
                        unknown |= result.status == TransferStatus::Unknown;
                        mailbox().lock().unwrap().messages.push(result.message);
                        let _ = proxy.send_event(UserEvent::Redraw);
                    });
                }
                Err(error) => mailbox()
                    .lock()
                    .unwrap()
                    .messages
                    .push(format!("{error:#}")),
            }
            // An indeterminate migration must not become a second migration on the next drop.
            if !unknown {
                mailbox().lock().unwrap().pending.remove(&source);
            }
            kasa_mcp::machines::poke();
            let _ = proxy.send_event(UserEvent::Redraw);
        });
        true
    }

    pub(crate) fn drain_drag_transfers(&mut self) {
        let messages = std::mem::take(&mut mailbox().lock().unwrap().messages);
        if let Some(message) = messages.into_iter().last() {
            self.set_toast(message);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_transfer::{RoomInfo, SessionIdentity};

    fn snapshot(machine: &str, room: &str) -> MachineSnapshot {
        MachineSnapshot {
            machine_id: machine.into(),
            room_transfer_supported: true,
            rooms: vec![RoomInfo {
                id: room.into(),
                title: "same title".into(),
            }],
            sessions: vec![SessionRow {
                identity: SessionIdentity {
                    machine_id: machine.into(),
                    pane_id: "%1".into(),
                    token: "token".into(),
                    instance: "instance".into(),
                    session_id: Some("conversation".into()),
                    ..Default::default()
                },
                status: "idle".into(),
                harness: Some("codex".into()),
                room_id: Some(room.into()),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn equal_pane_numbers_use_machine_identity_and_protocol_room() {
        let request = request_from_snapshots(
            &snapshot("source", "room:source"),
            "%1",
            &snapshot("target", "room:target"),
            "%1",
        )
        .unwrap();
        assert_eq!(request.sessions[0].machine_id, "source");
        assert_eq!(request.destination_machine, "target");
        assert_eq!(
            request.destination_room,
            RoomTarget::Existing("room:target".into())
        );
    }

    #[test]
    fn remote_center_swaps_and_edge_moves() {
        let (method, params) = remote_drop_command("%1", "%2", DropZone::Center);
        assert_eq!(method, "surface.swap");
        assert_eq!(params, serde_json::json!({ "a": "%1", "b": "%2" }));
        let (method, params) = remote_drop_command("%1", "%2", DropZone::Left);
        assert_eq!(method, "surface.move");
        assert_eq!(
            params,
            serde_json::json!({ "surface_id": "%1", "target": "%2", "direction": "left" })
        );
    }

    #[test]
    fn cross_device_drag_requires_existing_claude_or_codex_conversation() {
        let target = snapshot("target", "room:target");
        for harness in ["claude", "codex"] {
            let mut source = snapshot("source", "room:source");
            source.sessions[0].harness = Some(harness.into());
            assert!(request_from_snapshots(&source, "%1", &target, "%1").is_ok());
            source.sessions[0].identity.session_id = None;
            assert!(request_from_snapshots(&source, "%1", &target, "%1").is_err());
        }
        let mut source = snapshot("source", "room:source");
        source.sessions[0].harness = None;
        assert!(request_from_snapshots(&source, "%1", &target, "%1")
            .unwrap_err()
            .to_string()
            .contains("빈 셸"));
        source.sessions[0].harness = Some("other-harness".into());
        assert!(request_from_snapshots(&source, "%1", &target, "%1")
            .unwrap_err()
            .to_string()
            .contains("other-harness"));
    }

    #[test]
    fn stale_room_unknown_source_and_same_machine_are_rejected() {
        let source = snapshot("source", "room:source");
        let mut target = snapshot("target", "room:target");
        target.rooms.clear();
        assert!(request_from_snapshots(&source, "%1", &target, "%1").is_err());
        assert!(request_from_snapshots(&source, "%1", &source, "%1").is_err());
        let mut unknown = source;
        unknown.sessions[0].status = "unknown".into();
        assert!(
            request_from_snapshots(&unknown, "%1", &snapshot("target", "room:target"), "%1")
                .is_err()
        );
    }

    #[test]
    fn foreign_rows_and_local_mirror_numbers_cannot_select_a_source() {
        let mut source = snapshot("source", "room:source");
        source.sessions[0].identity.machine_id = "other".into();
        source.sessions[0].local_panes.push("%1".into());
        assert!(endpoint_row(&source, "%1").is_err());
        source.sessions[0].identity.machine_id = "source".into();
        source.sessions[0].identity.pane_id = "%2".into();
        assert!(endpoint_row(&source, "%1").is_err());
    }

    #[test]
    fn identity_pinned_before_drag_rejects_restarted_or_reused_pane() {
        let original = snapshot("source", "room:source");
        let pinned = original.sessions[0].identity.clone();
        assert!(verify_drag_identity(&original, "%1", &pinned).is_ok());
        for replacement in ["token", "instance", "session"] {
            let mut fresh = original.clone();
            match replacement {
                "token" => fresh.sessions[0].identity.token = "replacement".into(),
                "instance" => fresh.sessions[0].identity.instance = "restarted".into(),
                _ => fresh.sessions[0].identity.session_id = Some("replacement-session".into()),
            }
            assert!(verify_drag_identity(&fresh, "%1", &pinned).is_err());
        }
    }

    #[test]
    fn warm_cache_coalesces_remote_panes_by_origin_without_colliding_local() {
        let first = DragEndpoint::RemotePane {
            label: "host".into(),
            base: "http://host".into(),
            pane: "%1".into(),
        };
        let second = DragEndpoint::RemotePane {
            label: "host".into(),
            base: "http://host".into(),
            pane: "%2".into(),
        };
        assert_eq!(first.origin_key(), second.origin_key());
        assert_ne!(
            first.origin_key(),
            DragEndpoint::LocalPane("%1".into()).origin_key()
        );
    }

    #[test]
    fn missing_identity_duplicate_pane_and_old_capability_are_rejected() {
        let mut source = snapshot("source", "room:source");
        source.sessions[0].identity.token.clear();
        assert!(endpoint_row(&source, "%1").is_err());
        source.sessions[0].identity.token = "token".into();
        source.sessions.push(source.sessions[0].clone());
        assert!(endpoint_row(&source, "%1").is_err());
        source.sessions.pop();
        source.room_transfer_supported = false;
        assert!(
            request_from_snapshots(&source, "%1", &snapshot("target", "room:target"), "%1")
                .is_err()
        );
    }
}
