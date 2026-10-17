use super::*;
use serde_json::{json, Value};
use std::time::Duration;

impl App {
    pub(super) fn remember_mirror_close(&mut self, targets: &[MirrorTarget], reply: &Value) {
        let Some(records) = reply["records"].as_array() else { return };
        for record in records {
            let receipt = &record["receipt"];
            let Some(key) = receipt["restore_key"].as_str().filter(|s| !s.is_empty()) else { continue };
            if self.closed_panes.iter().any(|c| c.rec["remote_restore"]["receipt"]["restore_key"] == key) { continue; }
            let target = targets.iter().find(|t| t.source_key.as_deref() == receipt["source_key"].as_str());
            self.push_closed_pane(ClosedPane {
                ui_id: ClosedPane::next_ui_id(), rec: json!({"remote_restore": record}),
                pane_id: target.map(|t| t.local.clone()).filter(|s| !s.is_empty()).unwrap_or_else(|| key.to_string()),
                character: receipt["character"].as_str().unwrap_or("").to_string(),
                folder: record["label"].as_str().unwrap_or("원격").to_string(),
                neighbor: None, window: self.active_window, alive: false, stashed: false,
                retained_by: None, idle_since: None, preview: None,
            });
        }
    }

    pub(crate) fn reopen_mirror_record(&mut self, record: ClosedPane) {
        let metadata = record.rec["remote_restore"].clone();
        let label = metadata["label"].as_str().unwrap_or("").to_string();
        let base = kasa_mcp::machines::find(&label).map(|m| m.base)
            .or_else(|| metadata["base"].as_str().map(str::to_string));
        let Some(base) = base else {
            self.closed_panes.push(record);
            self.set_toast("원본 기기를 찾을 수 없어요. 닫힘 기록은 보관했어요".into());
            return;
        };
        self.set_toast("원본 세션을 되살리는 중이에요".into());
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let result = kasa_mcp::remote::remote_cmd(&base, "surface.reopen_checked", metadata["receipt"].clone())
                .map_err(|error| format!("{error:#}")).and_then(|reply| {
                    let value = reply["result"].clone();
                    let address = &value["address"];
                    if address["surface_key"] != metadata["receipt"]["source_key"]
                        || address["machine_id"] != metadata["receipt"]["machine_id"] {
                        return Err("원본 기기나 세션이 바뀌었어요".into());
                    }
                    kasa_mcp::machines::poke();
                    let end = Instant::now() + Duration::from_secs(10);
                    while Instant::now() < end {
                        if address["surface_id"].as_str().and_then(|id| kasa_mcp::machines::cached_pane(&label, id))
                            .is_some_and(|row| row["surface_key"] == address["surface_key"] && !crate::machinescol::remote_pane_closed(&row)) {
                            return Ok(value);
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    Err("원본은 되살렸지만 새 목록을 아직 받지 못했어요. 다시 시도해 주세요".into())
                });
            let _ = proxy.send_event(UserEvent::MirrorReopenDone { record, result });
        });
    }

    pub(crate) fn finish_mirror_reopen(&mut self, record: ClosedPane, result: Result<Value, String>) {
        let result = result.and_then(|reply| {
            let label = record.rec["remote_restore"]["label"].as_str().ok_or("원본 기기 이름이 없어요")?;
            let id = reply["address"]["surface_id"].as_str().ok_or("원본 칸 정보가 없어요")?;
            self.info.machines_col.last_refresh = None;
            self.refresh_machines_col();
            self.open_remote_room(label, reply["window"].as_u64(), "", Some(id))
                .map(|_| ()).map_err(|error| format!("{error:#}"))
        });
        if let Err(error) = result {
            self.closed_panes.push(record);
            self.set_toast(format!("되살리지 못했어요: {error}"));
        } else {
            self.set_toast("원본 세션을 되살렸어요".into());
        }
        self.chrome_dirty = true;
    }
}
