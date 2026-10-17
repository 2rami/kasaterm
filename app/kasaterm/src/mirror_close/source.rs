use super::*;
use kasa_socket::backend::Backend;
use serde_json::{json, Value};

fn addresses(request: &Value) -> Result<Vec<Value>> {
    let values: Vec<Value> = serde_json::from_value(request["addresses"].clone())?;
    anyhow::ensure!(!values.is_empty() && values.len() <= 256, "닫을 세션 주소가 필요해요");
    let mut unique = std::collections::HashSet::new();
    for value in &values {
        anyhow::ensure!(["machine_id", "surface_id", "surface_key", "instance_id"].iter()
            .all(|key| value[key].as_str().is_some_and(|s| !s.is_empty())), "완전한 세션 주소가 필요해요");
        anyhow::ensure!(unique.insert(value["surface_key"].as_str().unwrap()), "중복된 세션 주소예요");
    }
    Ok(values)
}

impl App {
    fn source_slot(&self, id: &str) -> Result<(String, usize)> {
        anyhow::ensure!(self.pty.contains_key(id) && !kasa_mcp::remote::is_remote_pane(id),
            "이 기기의 열린 세션이 아니에요");
        let ws = self.ws.lock().unwrap();
        let outer = ws.outer_for_pty(id).unwrap_or_else(|| id.to_string());
        let slot = ws.panes.get(&outer).and_then(|p| p.tabs.iter().position(|t| t.pid.as_deref() == Some(id))).unwrap_or(0);
        drop(ws);
        anyhow::ensure!(self.window_of_pane(&outer).is_some() || self.aux_terminal_index(&outer).is_some(),
            "이미 닫힌 세션이에요");
        Ok((outer, slot))
    }

    fn pane_addresses(&self, pane: &str) -> Result<Vec<Value>> {
        let backend = self.socket_backend.as_ref().ok_or_else(|| anyhow::anyhow!("세션 정보를 읽을 수 없어요"))?;
        let ws = self.ws.lock().unwrap();
        let ids = close_surface_ids(&ws, &PendingClose::Pane { pane: pane.to_string() });
        drop(ws);
        ids.iter().filter(|id| self.pty.contains_key(*id)).map(|id| backend.collab_pane_identity(id)).collect()
    }

    pub(crate) fn close_source_checked(&mut self, request: &Value) -> Result<Value> {
        let expected = addresses(request)?;
        let kind = request["kind"].as_str().unwrap_or("");
        anyhow::ensure!(matches!(kind, "tab" | "tabs" | "pane" | "room"), "알 수 없는 닫기 대상이에요");
        let backend = self.socket_backend.as_ref().ok_or_else(|| anyhow::anyhow!("세션 정보를 읽을 수 없어요"))?;
        let mut slots = Vec::new();
        for address in &expected {
            let id = address["surface_id"].as_str().unwrap();
            let slot = self.source_slot(id)?;
            anyhow::ensure!(backend.collab_pane_identity(id)? == *address, "세션이 바뀌었어요. 닫을 대상을 다시 확인해 주세요");
            slots.push(slot);
        }
        let action = match kind {
            "room" => {
                let index = request["window"].as_u64().ok_or_else(|| anyhow::anyhow!("원본 방 번호가 필요해요"))? as usize;
                anyhow::ensure!(same_room_addresses(&self.room_close_addresses(index), &expected), "방 구성이 바뀌었어요");
                anyhow::ensure!(self.user_room_count() > 1, "마지막 방은 닫을 수 없어요");
                PendingClose::Session(index)
            }
            "pane" => {
                let pane = &slots[0].0;
                anyhow::ensure!(slots.iter().all(|(p, _)| p == pane)
                    && same_room_addresses(&self.pane_addresses(pane)?, &expected), "칸의 탭 구성이 바뀌었어요");
                PendingClose::Pane { pane: pane.clone() }
            }
            "tab" => {
                anyhow::ensure!(expected.len() == 1, "탭 하나의 주소가 필요해요");
                let (pane, idx) = &slots[0];
                let count = self.ws.lock().unwrap().panes.get(pane).map_or(1, |p| p.tabs.len());
                if count <= 1 {
                    if let Some(window) = self.window_of_pane(pane).filter(|w| self.window_leaves(*w).len() <= 1) {
                        anyhow::ensure!(self.user_room_count() > 1, "마지막 방은 닫을 수 없어요");
                        PendingClose::Session(window)
                    } else { PendingClose::Pane { pane: pane.clone() } }
                } else { PendingClose::Tab { pane: pane.clone(), idx: *idx } }
            }
            _ => PendingClose::Tab { pane: slots[0].0.clone(), idx: slots[0].1 },
        };
        anyhow::ensure!(self.dirty_docs(&action).is_empty(), "원본에 저장하지 않은 문서가 있어요. 원본에서 저장한 뒤 다시 닫아 주세요");
        let before: std::collections::HashSet<_> = self.closed_panes.iter().map(|c| c.ui_id).collect();
        let execution: Result<()> = (|| {
        match action {
            PendingClose::Session(index) => self.close_window(index)?,
            PendingClose::Pane { pane } => self.close_pane(&pane),
            _ => {
                for address in &expected {
                    let id = address["surface_id"].as_str().unwrap();
                    let (outer, index) = self.source_slot(id)?;
                    let count = self.ws.lock().unwrap().panes.get(&outer).map_or(1, |p| p.tabs.len());
                    if count <= 1 { self.close_pane(&outer); }
                    else { self.close_tab(&outer, index); }
                }
            }
        }
        Ok(())
        })();
        self.publish_pty_layout();
        let complete = expected.iter().all(|address| self.source_slot(address["surface_id"].as_str().unwrap()).is_err());
        let records: Vec<_> = self.closed_panes.iter().filter(|c| !before.contains(&c.ui_id))
            .filter_map(|c| {
                let key = c.rec["surface_key"].as_str()?;
                expected.iter().any(|a| a["surface_key"] == key).then(|| json!({
                    "restore_key": c.rec["close_record_key"], "source_key": key,
                    "machine_id": expected[0]["machine_id"], "character": c.character, "folder": c.folder,
                }))
            }).collect();
        let error = execution.err().map(|e| e.to_string()).or_else(|| (!complete)
            .then(|| "일부 세션을 닫지 못했어요. 남은 원본 상태를 확인해 주세요".to_string()));
        Ok(json!({"closed": complete && error.is_none(), "records": records, "error": error}))
    }

    pub(crate) fn reopen_source_checked(&mut self, request: &Value) -> Result<Value> {
        let machine = kasa_mcp::board_service::local_source(vec![], true)?;
        anyhow::ensure!(request["machine_id"] == machine["machine_id"], "원본 기기가 바뀌었어요");
        let key = request["source_key"].as_str().filter(|s| !s.is_empty()).ok_or_else(|| anyhow::anyhow!("원본 세션 열쇠가 필요해요"))?;
        let record = request["restore_key"].as_str().filter(|s| !s.is_empty()).ok_or_else(|| anyhow::anyhow!("닫힘 기록 열쇠가 필요해요"))?;
        if let Some(index) = self.closed_panes.iter().position(|c| c.rec["close_record_key"] == record && c.rec["surface_key"] == key) {
            self.reopen_closed_pane_at(index);
            anyhow::ensure!(!self.closed_panes.iter().any(|c| c.rec["close_record_key"] == record), "원본 세션을 되살리지 못했어요");
        }
        let mut found = self.pty.keys().filter(|id| kasa_mcp::surface_keys::get(id).as_deref() == Some(key)
            && self.source_slot(id).is_ok());
        let id = found.next().cloned().ok_or_else(|| anyhow::anyhow!("이 닫힘 기록은 더 이상 복원할 수 없어요"))?;
        anyhow::ensure!(found.next().is_none(), "원본 세션이 둘 이상이에요");
        let address = self.socket_backend.as_ref().unwrap().collab_pane_identity(&id)?;
        let (outer, _) = self.source_slot(&id)?;
        Ok(json!({"address": address, "window": self.window_of_pane(&outer)}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_requires_complete_unique_addresses() {
        let address = json!({"machine_id":"m", "surface_id":"%1", "surface_key":"key", "instance_id":"instance"});
        assert!(addresses(&json!({"addresses":[address.clone()]})).is_ok());
        assert!(addresses(&json!({"addresses":[]})).is_err());
        assert!(addresses(&json!({"addresses":[{"surface_id":"%1"}]})).is_err());
        assert!(addresses(&json!({"addresses":[address.clone(),address]})).is_err());
    }
}
