//! A mirror must remain visible until its source accepts the same close action.
use super::*;
mod source;
mod reopen;
mod scope;
pub(crate) use scope::{LocalScope, TabIdentity};

pub(crate) fn confirmation_text(dialog: &ConfirmClose) -> (String, String) {
    let what = match dialog.action {
        PendingClose::Window => "앱을 종료할까요?",
        PendingClose::Session(_) | PendingClose::RemoteRoom { .. } => "이 방을 닫을까요?",
        PendingClose::Tab { .. } => "이 탭을 닫을까요?",
        PendingClose::Pane { .. } | PendingClose::RemotePane { .. } => "이 칸을 닫을까요?",
        PendingClose::AuxEditor(_) => "이 문서 창을 닫을까요?",
    };
    match &dialog.why {
        CloseWhy::Busy(process) => (what.into(), format!("{process} 이(가) 실행 중이에요. 닫으면 작업이 중단돼요.")),
        CloseWhy::LastPane => (what.into(), "마지막 칸을 닫으면 이 방도 닫혀요.".into()),
        CloseWhy::Mirror { targets, closing, error } => {
            let names = targets.iter().take(3).map(|target| {
                let name = kasa_mcp::machines::cached_pane(&target.label, &target.source)
                    .and_then(|row| row["name"].as_str().map(str::to_owned)).unwrap_or_else(|| target.source.clone());
                format!("{} · {name}", target.label)
            }).collect::<Vec<_>>().join(", ");
            if *closing { ("닫는 중이에요".into(), names) }
            else if let Some(error) = error { ("닫지 못했어요".into(), error.clone()) }
            else { (what.into(), format!("원본 기기에서도 닫혀요 — {names}")) }
        }
        CloseWhy::Dirty(docs) => {
            let names = docs.iter().take(3).map(|(_, name)| name.as_str()).collect::<Vec<_>>().join(", ");
            ("저장하지 않은 변경이 있어요".into(), format!("{names} — {what}"))
        }
    }
}

pub(crate) fn default_button(why: &CloseWhy) -> ConfirmBtn {
    match why {
        CloseWhy::Mirror { .. } => ConfirmBtn::Cancel,
        CloseWhy::Dirty(_) => ConfirmBtn::Save,
        _ => ConfirmBtn::Cancel,
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct MirrorTarget {
    pub local: String,
    pub base: String,
    pub source: String,
    pub label: String,
    local_key: Option<String>,
    local_pty: Option<usize>,
    source_key: Option<String>,
    address: Option<serde_json::Value>,
    window: Option<u64>,
    outer: String,
    pane_members: Vec<String>,
    room_members: Vec<String>,
    room_scope: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum CloseEffect {
    Cancel,
    Local,
    Source,
    Stale,
    Error(String),
}

fn choice_effect(button: ConfirmBtn, unchanged: bool) -> CloseEffect {
    if button == ConfirmBtn::Cancel { return CloseEffect::Cancel; }
    if !unchanged { return CloseEffect::Stale; }
    match button {
        ConfirmBtn::Close => CloseEffect::Source,
        _ => CloseEffect::Cancel,
    }
}

fn completion_effect(result: Result<(), String>, unchanged: bool) -> CloseEffect {
    match result {
        Err(error) => CloseEffect::Error(error),
        Ok(()) if unchanged => CloseEffect::Local,
        Ok(()) => CloseEffect::Stale,
    }
}

fn reply_completion(result: &Result<serde_json::Value, String>) -> Result<(), String> {
    match result {
        Ok(reply) => reply["error"].as_str().map(|e| Err(e.to_string())).unwrap_or(Ok(())),
        Err(error) => Err(error.clone()),
    }
}

fn same_members(actual: &[String], expected: &[String]) -> bool {
    let a: std::collections::BTreeSet<_> = actual.iter().collect();
    let b: std::collections::BTreeSet<_> = expected.iter().collect();
    !b.is_empty() && a == b
}

pub(crate) fn same_room_addresses(actual: &[serde_json::Value], expected: &[serde_json::Value]) -> bool {
    let keys = |rows: &[serde_json::Value]| rows.iter().filter_map(|row|
        row["surface_key"].as_str().map(str::to_owned)).collect::<Vec<_>>();
    same_members(&keys(actual), &keys(expected)) && actual.iter().all(|row| expected.contains(row))
}

fn source_address(board: &serde_json::Value, source: &str, key: Option<&str>) -> Option<serde_json::Value> {
    let key = key?;
    let mut matches = board["panes"].as_array()?.iter().map(|row| &row["address"])
        .filter(|address| address["surface_id"] == source && address["surface_key"] == key);
    let address = matches.next()?.clone();
    matches.next().is_none().then_some(address)
}

fn close_request(action: &PendingClose, targets: &[&MirrorTarget]) -> Result<serde_json::Value, String> {
    use serde_json::json;
    let first = targets.first().ok_or_else(|| "닫을 세션이 없어요".to_string())?;
    let addresses: Option<Vec<_>> = targets.iter().map(|t| t.address.clone()).collect();
    let addresses = addresses.ok_or_else(|| "원본 세션 정보를 아직 받지 못했어요".to_string())?;
    let ids: Vec<_> = targets.iter().map(|t| t.source.clone()).collect();
    let room = targets.iter().all(|t| t.room_scope && t.window == first.window)
        && same_members(&ids, &first.room_members);
    if matches!(action, PendingClose::RemoteRoom { .. }) && !room {
        return Err("원본 방 정보를 아직 받지 못했어요. 다시 확인해 주세요".into());
    }
    let pane = !matches!(action, PendingClose::Tab { .. })
        && targets.iter().all(|t| t.outer == first.outer)
        && same_members(&ids, &first.pane_members);
    let kind = if room { "room" } else if pane { "pane" } else if ids.len() == 1 { "tab" } else { "tabs" };
    Ok(json!({"kind": kind, "window": first.window, "addresses": addresses}))
}

fn request_source_close(action: &PendingClose, targets: &[MirrorTarget]) -> Result<serde_json::Value, String> {
    let mut groups: std::collections::BTreeMap<&str, Vec<&MirrorTarget>> = Default::default();
    for target in targets {
        let group = groups.entry(&target.base).or_default();
        if !group.iter().any(|old| old.source == target.source && old.address == target.address) { group.push(target); }
    }
    let requests = groups.into_iter().map(|(base, group)| {
        close_request(action, &group).map(|request| (base, group[0], request))
    }).collect::<Result<Vec<_>, _>>()?;
    let mut records = Vec::new();
    for (base, first, request) in requests {
        match kasa_mcp::remote::remote_cmd(base, "surface.close_checked", request) {
            Ok(reply) => {
                if let Some(rows) = reply["result"]["records"].as_array() {
                    for row in rows {
                        records.push(serde_json::json!({"base": base, "label": first.label, "receipt": row}));
                    }
                }
                if reply["result"]["closed"] != true {
                    return Ok(serde_json::json!({"records": records,
                        "error": reply["result"]["error"].as_str().unwrap_or("원본이 닫기를 완료하지 못했어요")}));
                }
            }
            result => {
                let error = result.err().map(|error| format!("{error:#}"))
                    .unwrap_or_else(|| "원본이 닫기를 완료하지 못했어요".into());
                let error = if error.contains("surface.close_checked") {
                    "원본 기기도 업데이트한 뒤 닫아 주세요".to_string()
                } else { format!("{}: {error}", first.label) };
                return Ok(serde_json::json!({"records": records, "error": error}));
            }
        }
    }
    Ok(serde_json::json!({"records": records}))
}

fn target_metadata(target: &mut MirrorTarget) {
    let snapshot = kasa_mcp::machines::snapshot();
    let Some(rows) = snapshot.iter().find(|m| m["label"] == target.label)
        .and_then(|m| m["panes"].as_array()) else { return };
    let Some(row) = rows.iter().find(|r| r["id"] == target.source) else { return };
    target.window = row["window"].as_u64();
    target.outer = row["tab_of"].as_str().unwrap_or(&target.source).to_string();
    for row in rows.iter().filter(|r| !crate::machinescol::remote_pane_closed(r) && r["mirror_of"].is_null()) {
        let Some(id) = row["id"].as_str() else { continue };
        if id == target.outer || row["tab_of"] == target.outer { target.pane_members.push(id.to_string()); }
        if target.window.is_some() && row["window"].as_u64() == target.window { target.room_members.push(id.to_string()); }
    }
    target.pane_members.sort();
    target.room_members.sort();
}

fn close_surface_ids(ws: &Workspace, action: &PendingClose) -> Vec<String> {
    let mut ids = match action {
        // A document tab has no PTY; falling back to its container closes a different tab.
        PendingClose::Tab { pane, idx } => match ws.panes.get(pane) {
            Some(p) => p.tabs.get(*idx).and_then(|tab| tab.pid.clone()).into_iter().collect(),
            None if *idx == 0 => vec![pane.clone()],
            None => Vec::new(),
        },
        PendingClose::Pane { pane } => {
            let mut ids = vec![pane.clone()];
            if let Some(p) = ws.panes.get(pane) {
                ids.extend(p.tabs.iter().filter_map(|tab| tab.pid.clone()));
            }
            ids
        }
        _ => Vec::new(),
    };
    ids.sort();
    ids.dedup();
    ids
}

impl App {
    /// Closing a source is a layout event even while its PTY is kept for undo.
    /// Do not use EOF or the socket disconnect as a proxy for this decision.
    pub(crate) fn notify_source_closed(&self, target: &str) {
        let ws = self.ws.lock().unwrap();
        let mut ids = vec![target.to_string()];
        if let Some(pane) = ws.panes.get(target) {
            ids.extend(pane.tabs.iter().filter_map(|tab| tab.pid.clone()));
        }
        ids.sort();
        ids.dedup();
        for id in ids { kasa_mcp::push_viewer_control(&id, r#"{"t":"source-closed"}"#); }
    }

    pub(crate) fn room_close_addresses(&self, idx: usize) -> Vec<serde_json::Value> {
        use kasa_socket::backend::Backend;
        let Some(backend) = self.socket_backend.as_ref() else { return vec![]; };
        let layout = if idx == self.active_window { self.pty_layout.as_ref() }
            else { self.windows.get(idx).and_then(|window| window.as_ref()) };
        let ws = self.ws.lock().unwrap();
        let mut ids: Vec<_> = layout.into_iter().flat_map(|layout| layout.leaves()).flat_map(|pane| {
            close_surface_ids(&ws, &PendingClose::Pane { pane: pane.to_string() })
        }).collect();
        drop(ws);
        ids.sort();
        ids.dedup();
        ids.iter().map(|id| backend.collab_pane_identity(id)).collect::<Result<Vec<_>>>().unwrap_or_default()
    }

    pub(crate) fn mirror_close_targets(&self, action: &PendingClose) -> Vec<MirrorTarget> {
        let board = kasa_mcp::board_service::snapshot(&serde_json::json!({"scope": "all"})).unwrap_or_default();
        let remote = match action {
            PendingClose::RemoteRoom { label, window, room } => Some((label, self.remote_room_panes(label, *window, room)
                .into_iter().map(|row| row.remote_id.clone()).collect::<Vec<_>>())),
            PendingClose::RemotePane { label, pane } => Some((label, vec![pane.clone()])),
            _ => None,
        };
        if let Some((label, mut ids)) = remote {
            let Some(machine) = kasa_mcp::machines::find(label) else { return vec![]; };
            if let PendingClose::RemotePane { pane, .. } = action {
                if let Some(rows) = kasa_mcp::machines::snapshot().iter()
                    .find(|m| m["label"] == *label).and_then(|m| m["panes"].as_array()) {
                    ids.extend(rows.iter().filter(|r| r["tab_of"] == *pane && !crate::machinescol::remote_pane_closed(r))
                        .filter_map(|r| r["id"].as_str().map(str::to_string)));
                }
            }
            ids.sort();
            ids.dedup();
            return ids.into_iter().map(|source| {
                let source_key = kasa_mcp::machines::cached_pane(label, &source)
                    .and_then(|row| row["surface_key"].as_str().map(str::to_owned));
                let address = source_address(&board, &source, source_key.as_deref());
                let mut target = MirrorTarget { base: machine.base.clone(), source,
                    label: label.clone(), source_key, address,
                    room_scope: matches!(action, PendingClose::RemoteRoom { .. }), ..Default::default() };
                target_metadata(&mut target);
                target
            }).collect();
        }
        let ws = self.ws.lock().unwrap();
        let ids = if let PendingClose::Session(idx) = action {
            let layout = if *idx == self.active_window {
                self.pty_layout.as_ref()
            } else {
                self.windows.get(*idx).and_then(|window| window.as_ref())
            };
            layout.into_iter().flat_map(|layout| layout.leaves()).flat_map(|pane| {
                close_surface_ids(&ws, &PendingClose::Pane { pane: pane.to_string() })
            }).collect()
        } else {
            close_surface_ids(&ws, action)
        };
        drop(ws);
        let room_scope = matches!(action, PendingClose::Session(idx) if self.remote_view_of_window(*idx).is_some());
        ids.into_iter().filter_map(|local| {
            let info = kasa_mcp::remote::remote_info(&local)?;
            let source_key = kasa_mcp::remote::remote_surface_key(&local);
            let address = source_address(&board, &info.remote_id, source_key.as_deref());
            let mut target = MirrorTarget {
                local_key: Some(kasa_mcp::surface_keys::ensure(&local)),
                local_pty: self.pty.get(&local).map(|pty| Arc::as_ptr(pty) as usize),
                source_key, address, local, base: info.base, source: info.remote_id, label: info.label,
                room_scope, ..Default::default()
            };
            target_metadata(&mut target);
            info.view.then_some(target)
        }).collect()
    }

    pub(crate) fn guard_mirror_close(&mut self, action: &PendingClose) -> bool {
        let targets = self.mirror_close_targets(action);
        if targets.is_empty() { return false; }
        self.mirror_close_scope = Some(self.local_close_scope(action, &targets));
        self.raise_confirm(ConfirmClose {
            why: CloseWhy::Mirror { targets, closing: false, error: None },
            action: action.clone(),
        });
        true
    }

    pub(crate) fn choose_mirror_close(
        &mut self, action: PendingClose, targets: Vec<MirrorTarget>, button: ConfirmBtn,
    ) {
        let scope = self.local_close_scope(&action, &targets);
        let unchanged = self.mirror_close_targets(&action) == targets
            && self.mirror_close_scope.as_ref() == Some(&scope);
        match choice_effect(button, unchanged) {
            CloseEffect::Source => {}
            CloseEffect::Stale => {
                self.set_toast("거울이 바뀌었어. 닫을 창을 다시 골라줘".into());
                return;
            }
            _ => return,
        }
        self.confirm_close = Some(ConfirmClose {
            why: CloseWhy::Mirror { targets: targets.clone(), closing: true, error: None },
            action: action.clone(),
        });
        self.chrome_dirty = true;
        if let Some(w) = &self.window { w.request_redraw(); }
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let result = request_source_close(&action, &targets);
            let _ = proxy.send_event(UserEvent::MirrorCloseDone { action, targets, scope, result });
        });
    }

    pub(crate) fn finish_mirror_close(
        &mut self, action: PendingClose, targets: Vec<MirrorTarget>, scope: LocalScope, result: Result<serde_json::Value, String>,
    ) {
        let completion = reply_completion(&result);
        let effect = completion_effect(completion, true);
        if let Ok(reply) = &result { self.remember_mirror_close(&targets, reply); }
        match effect {
            CloseEffect::Error(error) => {
                self.confirm_close = Some(ConfirmClose {
                    why: CloseWhy::Mirror { targets, closing: false, error: Some(error) },
                    action,
                });
            }
            CloseEffect::Local => {
                for target in targets.iter().filter(|_| scope.matches_workspace(self)).cloned().collect::<Vec<_>>() {
                    if !target.local.is_empty() && target.local_key.is_some()
                        && self.pty.get(&target.local).map(|pty| Arc::as_ptr(pty) as usize) == target.local_pty
                        && kasa_mcp::surface_keys::get(&target.local) == target.local_key
                        && kasa_mcp::remote::remote_info(&target.local).is_none_or(|info|
                            info.view && info.base == target.base && info.remote_id == target.source
                            && kasa_mcp::remote::remote_surface_key(&target.local) == target.source_key) {
                        self.remove_finished_surface(&target.local);
                    }
                }
                if let Err(error) = self.finish_local_close_scope(&action, &scope) { self.set_toast(error.to_string()); }
                self.confirm_close = None;
            }
            _ => { self.confirm_close = None; }
        }
        kasa_mcp::machines::poke();
        self.info.machines_col.last_refresh = None;
        self.chrome_dirty = true;
        if let Some(w) = &self.window { w.request_redraw(); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn tab_pane_and_complete_room_have_distinct_close_scopes() {
        let a = MirrorTarget { source: "%0".into(), outer: "%0".into(), window: Some(2), room_scope: true,
            pane_members: vec!["%0".into(), "%1".into()], room_members: vec!["%0".into(), "%1".into()],
            address: Some(serde_json::json!({"surface_id":"%0"})), ..Default::default() };
        let b = MirrorTarget { source: "%1".into(), address: Some(serde_json::json!({"surface_id":"%1"})), ..a.clone() };
        let tab = PendingClose::Tab { pane: "%local".into(), idx: 0 };
        assert_eq!(close_request(&tab, &[&a]).unwrap()["kind"], "tab");
        let pane = PendingClose::Pane { pane: "%local".into() };
        let mut plain_a = a.clone(); plain_a.room_scope = false;
        let mut plain_b = b.clone(); plain_b.room_scope = false;
        assert_eq!(close_request(&pane, &[&plain_a]).unwrap()["kind"], "tab", "a single mirrored tab never closes its siblings");
        assert_eq!(close_request(&pane, &[&plain_a, &plain_b]).unwrap()["kind"], "pane");
        assert_eq!(close_request(&PendingClose::Session(8), &[&a, &b]).unwrap()["kind"], "room");
        assert_eq!(close_request(&PendingClose::Session(8), &[&plain_a, &plain_b]).unwrap()["kind"], "pane", "a mixed local room never closes a source room");
        let remote = PendingClose::RemoteRoom { label: "source".into(), window: Some(2), room: "room".into() };
        assert!(close_request(&remote, &[&a]).is_err(), "a partial room list cannot degrade to closing individual panes");
    }

    #[test]
    fn a_pending_room_close_rejects_reused_indices_and_changed_members() {
        let expected = vec!["old-primary".into(), "old-tab".into()];
        assert!(same_members(&["old-tab".into(), "old-primary".into()], &expected));
        assert!(!same_members(&["replacement-room".into()], &expected));
        assert!(!same_members(&["old-primary".into(), "new-tab".into()], &expected));
        assert!(!same_members(&expected, &[]));
        assert!(!same_members(&[], &expected));
    }

    #[test]
    fn room_close_rejects_a_new_conversation_or_restarted_app_with_the_same_surface() {
        let address = serde_json::json!({"machine_id":"source", "surface_id":"%4",
            "surface_key":"original", "session_id":"conversation", "instance_id":"process"});
        assert!(same_room_addresses(&[address.clone()], &[address.clone()]));
        for field in ["session_id", "instance_id", "machine_id"] {
            let mut replacement = address.clone();
            replacement[field] = serde_json::json!("replacement");
            assert!(!same_room_addresses(&[replacement], &[address.clone()]));
        }
    }

    #[test]
    fn source_addresses_keep_the_session_and_instance_and_reject_ambiguity() {
        let address = serde_json::json!({"machine_id":"source", "surface_id":"%4",
            "surface_key":"original", "session_id":"conversation", "instance_id":"process"});
        let board = serde_json::json!({"panes":[{"address":address}]});
        assert_eq!(source_address(&board, "%4", Some("original")), Some(address.clone()));
        assert_eq!(source_address(&board, "%4", Some("reused")), None);
        assert_eq!(source_address(&board, "%4", None), None);
        let ambiguous = serde_json::json!({"panes":[{"address":address}, {"address":address}]});
        assert_eq!(source_address(&ambiguous, "%4", Some("original")), None);
    }

    #[test]
    fn return_cannot_confirm_a_remote_close_or_retry() {
        for (closing, error) in [(false, None), (true, None), (false, Some("offline".into()))] {
            let why = CloseWhy::Mirror { targets: vec![], closing, error };
            assert!(default_button(&why) == ConfirmBtn::Cancel);
        }
        assert!(default_button(&CloseWhy::Dirty(vec![])) == ConfirmBtn::Save);
        assert!(default_button(&CloseWhy::Busy("local task".into())) == ConfirmBtn::Cancel);
    }

    #[test]
    fn cancel_never_calls_the_source_and_close_always_targets_it() {
        let source = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        source.set_nonblocking(true).unwrap();
        assert_eq!(choice_effect(ConfirmBtn::Cancel, true), CloseEffect::Cancel);
        assert_eq!(choice_effect(ConfirmBtn::Close, true), CloseEffect::Source);
        assert_eq!(choice_effect(ConfirmBtn::Save, true), CloseEffect::Cancel);
        assert_eq!(source.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
    }

    fn source_case(ok: bool) -> (CloseEffect, usize) {
        let source = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", source.local_addr().unwrap());
        let (seen, received) = std::sync::mpsc::channel();
        let (approve, approval) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = source.accept().unwrap();
            socket.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
            let mut request = [0u8; 4096];
            let count = socket.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..count]);
            assert!(request.starts_with("POST /cmd HTTP/1.1\r\n"));
            assert!(request.contains("surface.close_checked"));
            assert!(!request.contains("kill=1"));
            seen.send(()).unwrap();
            approval.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
            let body = if ok { r#"{"ok":true,"result":{"closed":true,"records":[]}}"# } else { r#"{"ok":false,"error":"source unavailable"}"# };
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            1
        });
        let target = MirrorTarget { local: "%2".into(), base, source: "%16".into(), label: "fake source".into(), source_key: None, address: Some(serde_json::json!({"surface_id":"%16","surface_key":"fixture-key"})), ..Default::default() };
        assert_eq!(choice_effect(ConfirmBtn::Close, true), CloseEffect::Source);
        let worker = std::thread::spawn(move || request_source_close(&PendingClose::Pane { pane: "%2".into() }, &[target.clone(), target]));
        received.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert!(!worker.is_finished(), "local close cannot be approved before the source replies");
        approve.send(()).unwrap();
        let effect = completion_effect(reply_completion(&worker.join().unwrap()), true);
        (effect, server.join().unwrap())
    }

    #[test]
    fn source_success_closes_local_only_after_ack_and_deduplicates_source() {
        let (effect, requests) = source_case(true);
        assert_eq!(effect, CloseEffect::Local);
        assert_eq!(requests, 1);
    }

    #[test]
    fn source_failure_preserves_local_mirror_and_reports_reason() {
        let (effect, requests) = source_case(false);
        assert!(matches!(effect, CloseEffect::Error(error) if error.contains("source unavailable")));
        assert_eq!(requests, 1);
    }

    #[test]
    fn changed_target_is_never_closed_by_an_old_confirmation() {
        assert_eq!(choice_effect(ConfirmBtn::Close, false), CloseEffect::Stale);
        assert_eq!(completion_effect(Ok(()), false), CloseEffect::Stale);
        assert_eq!(choice_effect(ConfirmBtn::Cancel, false), CloseEffect::Cancel);
    }

    #[test]
    fn a_document_tab_never_closes_its_container_surface() {
        let mut ws = Workspace::default();
        ws.pane_mut("%1").tabs = vec![
            PaneTab { pid: Some("%1".into()), ..Default::default() },
            PaneTab::default(),
            PaneTab { pid: Some("%2".into()), ..Default::default() },
        ];
        let tab = |idx| PendingClose::Tab { pane: "%1".into(), idx };
        assert_eq!(close_surface_ids(&ws, &tab(0)), vec!["%1"]);
        assert!(close_surface_ids(&ws, &tab(1)).is_empty());
        assert_eq!(close_surface_ids(&ws, &tab(2)), vec!["%2"]);
        assert!(close_surface_ids(&ws, &tab(9)).is_empty());
        assert_eq!(close_surface_ids(&ws, &PendingClose::Pane { pane: "%1".into() }), vec!["%1", "%2"]);
        assert!(close_surface_ids(&ws, &PendingClose::Window).is_empty());
    }

    #[test]
    fn a_finished_mirror_is_removed_from_its_actual_tab_not_a_stale_container() {
        let mut ws = Workspace::default();
        ws.pane_mut("%1").tabs = vec![
            PaneTab { pid: Some("%1".into()), ..Default::default() },
            PaneTab::default(),
            PaneTab { pid: Some("%2".into()), ..Default::default() },
        ];
        ws.pane_mut("%9");
        ws.pid_to_pane.insert("%2".into(), "%9".into());
        assert_eq!(ws.secondary_tab_slot("%2"), Some(("%1".into(), 2)));
        assert_eq!(ws.secondary_tab_slot("%1"), None);
        assert_eq!(ws.secondary_tab_slot("%9"), None);
        assert_eq!(ws.secondary_tab_slot("%99"), None);
        ws.pane_mut("%1").tabs.remove(2);
        assert_eq!(ws.secondary_tab_slot("%2"), None);
    }

    #[test]
    fn a_reused_source_number_does_not_match_a_pending_close() {
        let old = MirrorTarget { local: "%2".into(), base: "http://127.0.0.1:1".into(),
            source: "%16".into(), label: "fixture".into(), source_key: Some("old-source".into()), address: None, ..Default::default() };
        let new = MirrorTarget { source_key: Some("new-source".into()), ..old.clone() };
        assert_eq!(choice_effect(ConfirmBtn::Close, old == new), CloseEffect::Stale);
        assert_eq!(completion_effect(Ok(()), old == new), CloseEffect::Stale);
    }
}
