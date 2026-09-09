use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionIdentity {
    pub machine_id: String,
    pub pane_id: String,
    pub session_id: Option<String>,
    pub instance: String,
    pub token: String,
}

impl SessionIdentity {
    pub fn canonical_key(&self) -> String {
        format!("{}:{}", self.machine_id, self.pane_id)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SessionRow {
    pub identity: SessionIdentity,
    pub local_panes: Vec<String>,
    pub name: String,
    pub title: String,
    pub cwd: String,
    pub harness: Option<String>,
    pub status: String,
    pub room_id: Option<String>,
    pub shell_closeable: bool,
    pub unavailable_reason: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomInfo {
    pub id: String,
    pub title: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MachineSnapshot {
    pub machine_id: String,
    pub label: String,
    pub instance: String,
    pub rooms: Vec<RoomInfo>,
    pub sessions: Vec<SessionRow>,
    pub room_transfer_supported: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TransferMachine {
    pub id: String,
    pub label: String,
    pub local: bool,
    pub online: bool,
    pub room_transfer_supported: bool,
    pub rooms: Vec<RoomInfo>,
    pub unavailable_reason: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TransferSnapshot {
    pub machines: Vec<TransferMachine>,
    pub sessions: Vec<SessionRow>,
    pub errors: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum RoomTarget {
    Existing(String),
    New(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransferRequest {
    pub sessions: Vec<SessionIdentity>,
    pub destination_machine: String,
    pub destination_room: RoomTarget,
    pub confirmed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlaceRequest {
    pub session: SessionIdentity,
    pub room: RoomTarget,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpawnRequest {
    pub room: RoomTarget,
    pub cwd: String,
    pub character: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MigrateRequest {
    pub session: SessionIdentity,
    pub destination_machine: String,
    pub room: RoomTarget,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferStatus {
    Waiting,
    Running,
    Succeeded,
    Failed,
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransferResult {
    pub source: SessionIdentity,
    pub status: TransferStatus,
    pub message: String,
    pub destination: Option<SessionIdentity>,
}
