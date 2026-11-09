//! 게시할 수 있는 원본 대화 — 이 기기의 로컬 칸 가운데 claude·codex 가 돌고 대화 기록이 결속된 것.
//! 화면·폰에 보이는 목록에는 기록 경로를 싣지 않는다. 경로는 게시할 때 앱 안에서만 쓴다.

use super::*;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Source {
    /// 완전한 tell 주소(machine·surface_key·surface_id·session_id·instance_id).
    pub(crate) address: serde_json::Value,
    pub(crate) title: String,
    pub(crate) harness: String,
    pub(crate) cwd: String,
    pub(crate) room: String,
    pub(crate) room_label: String,
    /// 칸의 대화 번호와 결속된 기록이 같은 대화를 가리킨다. 아니면 게시를 받지 않는다.
    pub(crate) ready: bool,
}

impl Source {
    pub(crate) fn key(&self) -> &str {
        self.address["surface_key"].as_str().unwrap_or_default()
    }

    pub(crate) fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "address": self.address, "title": self.title, "harness": self.harness, "cwd": self.cwd,
            "room": self.room, "room_label": self.room_label, "ready": self.ready,
        })
    }
}

/// 게시에 필요한 원본 사실. `transcript` 는 결속된 기록, `native` 는 그 대화의 하네스 ID 다.
#[derive(Clone, Debug)]
pub(super) struct Origin {
    pub(super) source: Source,
    pub(super) transcript: PathBuf,
    pub(super) native: String,
}

impl App {
    pub(crate) fn shared_source_list(&self) -> Vec<Source> {
        self.shared_origins().into_iter().map(|origin| origin.source).collect()
    }

    pub(super) fn shared_origins(&self) -> Vec<Origin> {
        let Some(backend) = self.socket_backend.clone() else { return Vec::new() };
        let mut ids: Vec<&String> = self.pty.keys().collect();
        ids.sort();
        let mut out = Vec::new();
        for id in ids {
            if kasa_remote::remote::is_remote_pane(id) {
                continue;
            }
            let Some(session) = self.pty.get(id) else { continue };
            let Some(harness) = session.active_agent().map(|agent| agent.as_str()).filter(|h| matches!(*h, "claude" | "codex")) else {
                continue;
            };
            // 잠겨 있으면 이번에는 건너뛴다 — GUI 스레드는 소켓 쪽 결속표를 기다리지 않는다.
            let Ok(Some(transcript)) = backend.bound_transcript(id) else { continue };
            let native = if harness == "codex" {
                kasa_socket::sessions::codex_sessions::codex_id_from_rollout_path(&transcript)
            } else {
                transcript.file_stem().and_then(|s| s.to_str()).map(str::to_owned)
            };
            let Some(native) = native.filter(|n| kasa_socket::sessions::is_uuid(n)) else { continue };
            let (outer, character, room, title) = {
                let ws = self.ws.lock().unwrap();
                let outer = ws.outer_for_pty(id).unwrap_or_else(|| id.clone());
                let title = ws.panes.get(&outer).and_then(|pane| pane.tab_for_pid(id).title.clone()).unwrap_or_default();
                (outer, ws.pane_character.get(id).cloned(), ws.pane_room.get(id).cloned(), title)
            };
            let Some(window) = self.window_of_pane(&outer) else { continue };
            if self.internal_room_kind_at(window).is_some() {
                continue;
            }
            let Ok(address) = kasa_collab::board_service::address(id, Some(&native)) else { continue };
            let room_label = self.window_name_override.get(&window).cloned()
                .or_else(|| self.window_labels.get(window).map(|label| label.0.clone()).filter(|label| !label.is_empty()))
                .unwrap_or_else(|| format!("{}번 방", window + 1));
            let title = [title, character.unwrap_or_default()].into_iter().find(|t| !t.trim().is_empty())
                .unwrap_or_else(|| format!("{harness} 대화"));
            out.push(Origin {
                source: Source {
                    address,
                    title: title.chars().filter(|c| !c.is_control()).take(200).collect(),
                    harness: harness.into(),
                    cwd: self.pane_cwd_cache.get(id).map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
                    room: room.unwrap_or_else(|| format!("window-{window}")),
                    room_label,
                    ready: self.pane_claude_sid.get(id) == Some(&native),
                },
                transcript,
                native,
            });
        }
        out
    }
}
