use serde::{Deserialize, Serialize};

pub const FORMAT_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SharedMember {
    pub account: String,
    pub display_name: String,
    pub role: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SharedRoom {
    pub id: String,
    pub title: String,
    pub owner_account: String,
    pub owner_name: String,
    pub my_role: String,
    pub allow_fork: bool,
    pub revision: u64,
    pub updated_at_ms: u64,
    #[serde(default)]
    pub members: Vec<SharedMember>,
    #[serde(default)]
    pub sessions: Vec<SharedSession>,
    #[serde(default)]
    pub invites: Vec<InviteSummary>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SharedRoomList {
    pub rooms: Vec<SharedRoom>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SharedSession {
    pub id: String,
    pub source_key: String,
    pub title: String,
    pub harness: String,
    pub publisher_machine_id: String,
    pub revision: u64,
    pub generation: String,
    pub bytes: u64,
    #[serde(default)]
    pub cursor: u64,
    pub updated_at_ms: u64,
    pub caught_up: bool,
    #[serde(default)]
    pub checkpoint: Option<CheckpointSummary>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckpointSummary {
    pub id: String,
    pub harness: String,
    pub parent_session_id: String,
    pub boundary_id: String,
    pub boundary_label: String,
    pub captured_at_ms: u64,
    pub has_project: bool,
    #[serde(default)]
    pub omissions: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateRoom {
    pub title: String,
    pub allow_fork: bool,
    #[serde(default)]
    pub request_id: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateRoom {
    pub expected_revision: u64,
    pub title: Option<String>,
    pub allow_fork: Option<bool>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateInvite {
    pub expires_in_secs: Option<u64>,
    pub max_uses: Option<u32>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SharedInvite {
    pub id: String,
    pub room_id: String,
    pub token: String,
    pub expires_at_ms: u64,
    pub max_uses: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InviteSummary {
    pub id: String,
    pub expires_at_ms: u64,
    pub max_uses: u32,
    pub uses: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishSession {
    pub source_key: String,
    pub title: String,
    pub harness: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedAppend {
    pub expected_revision: u64,
    pub generation: String,
    pub from: u64,
    pub next: u64,
    pub reset: bool,
    pub raw: String,
    pub caught_up: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SharedContent {
    pub generation: String,
    pub revision: u64,
    pub offset: u64,
    pub next: u64,
    pub end: u64,
    pub raw: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeForkSnapshot {
    pub harness: String,
    pub parent_session_id: String,
    pub boundary_id: String,
    pub boundary_label: String,
    pub native_version: String,
    pub jsonl: String,
    pub sha256: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedFile {
    pub path: String,
    pub bytes_base64: String,
    pub sha256: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSnapshot {
    pub repo_url: Option<String>,
    pub commit: Option<String>,
    pub captured_at_ms: u64,
    #[serde(default)]
    pub patch: String,
    #[serde(default)]
    pub files: Vec<SharedFile>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForkCheckpoint {
    pub format_version: u32,
    pub native: NativeForkSnapshot,
    pub captured_at_ms: u64,
    pub display_raw: String,
    pub project: Option<ProjectSnapshot>,
    #[serde(default)]
    pub omissions: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishCheckpoint {
    pub expected_generation: String,
    pub expected_revision: u64,
    pub checkpoint: ForkCheckpoint,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateFork {
    pub checkpoint_id: String,
    pub request_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SharedForkInfo {
    pub id: String,
    pub room_id: String,
    pub session_id: String,
    pub checkpoint_id: String,
    pub created_at_ms: u64,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SharedFork {
    #[serde(flatten)]
    pub info: SharedForkInfo,
    pub checkpoint: ForkCheckpoint,
}
