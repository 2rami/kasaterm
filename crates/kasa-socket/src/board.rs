//! Bounded observation state. A failed source can never prove a pane's death.
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const MAX_EVENTS: usize = 1000;
pub const MAX_JOURNAL_BYTES: usize = 256 * 1024;
pub const MAX_PANES: usize = 2048;
pub const MAX_SOURCE_BYTES: usize = 4 * 1024 * 1024;
const TRACKED: &[&str] = &[
    "address",
    "room_id",
    "room_label",
    "character",
    "harness",
    "title",
    "request",
    "progress",
    "status",
    "status_reason",
    // 「기다린다」는 두 가지다 — 승인·질문(사람을 부른다)과 방치(그냥 쉬는 것). 이 둘을
    // 가르는 칸이 집계에서 잘리면 읽는 쪽은 모든 기다림을 사람 손 필요로 세고, 노는 학생
    // 마다 주황이 깜빡인다(2026-09-21 「waiting 이라기엔 idle인데」).
    "attention_kind",
    "waiting_for",
    "done_outcome",
    "done_summary",
    "detached",
    "place_state",
];

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub fn short(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end]
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

pub fn field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str().filter(|s| !s.trim().is_empty())
}

pub fn rollout_activity(tail: &str, limit: usize) -> Vec<crate::backend::ActivityEvent> {
    let mut events = VecDeque::new();
    let mut names = BTreeMap::new();
    for line in tail.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let payload = &value["payload"];
        let (kind, name, text) = match (field(&value, "type"), field(payload, "type")) {
            (Some("event_msg"), Some("user_message")) => (
                "prompt",
                String::new(),
                activity_plain_text(field(payload, "message").unwrap_or_default()),
            ),
            (Some("event_msg"), Some("agent_message")) => (
                "say",
                String::new(),
                activity_plain_text(field(payload, "message").unwrap_or_default()),
            ),
            (Some("event_msg"), Some("item_completed")) => {
                let item = &payload["item"];
                let kind = match field(item, "type") {
                    Some("UserMessage") => "prompt",
                    Some("AgentMessage") => "say",
                    _ => continue,
                };
                (kind, String::new(), activity_output_text(&item["content"]))
            }
            (Some("response_item"), Some("function_call" | "custom_tool_call")) => {
                let name = field(payload, "name").unwrap_or("tool").to_owned();
                if let Some(id) = field(payload, "call_id") {
                    names.insert(id.to_owned(), name.clone());
                }
                (
                    "tool",
                    name,
                    activity_plain_text(
                        field(payload, "arguments")
                            .or_else(|| field(payload, "input"))
                            .unwrap_or_default(),
                    ),
                )
            }
            (Some("response_item"), Some("function_call_output" | "custom_tool_call_output")) => {
                let name = field(payload, "call_id")
                    .and_then(|id| names.remove(id))
                    .unwrap_or_default();
                let text = activity_output_text(&payload["output"]);
                ("result", name, text)
            }
            _ => continue,
        };
        if text.is_empty() {
            continue;
        }
        let output = payload["output"]
            .as_str()
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
            .unwrap_or_else(|| payload["output"].clone());
        let is_error = (kind == "result").then(|| &output).and_then(|v| {
            v["exit_code"]
                .as_i64()
                .map(|exit| exit != 0)
                .or_else(|| v["is_error"].as_bool())
        });
        events.push_back(crate::backend::ActivityEvent {
            kind: kind.into(),
            name: short(&name, 100),
            text: detail_text(&text, 2048),
            is_error,
        });
        while events.len() > limit.clamp(1, 50) {
            events.pop_front();
        }
    }
    events.into_iter().collect()
}

fn activity_plain_text(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let trimmed = text.trim_start();
    if lower.contains("data:image/")
        || lower.contains(";base64,")
        || ["iVBORw0KGgo", "/9j/", "R0lGOD", "UklGR"]
            .iter()
            .any(|prefix| trimmed.starts_with(prefix))
    {
        return String::new();
    }
    detail_text(text, 2048)
}

fn activity_output_text(value: &Value) -> String {
    fn extract(value: &Value, out: &mut Vec<String>, depth: usize) {
        if depth > 16 || out.iter().map(String::len).sum::<usize>() >= 2048 {
            return;
        }
        match value {
            Value::String(text) => {
                let text = activity_plain_text(text);
                if !text.is_empty() {
                    out.push(text);
                }
            }
            Value::Array(values) => {
                for value in values {
                    extract(value, out, depth + 1);
                }
            }
            Value::Object(object) => {
                if object
                    .get("mime_type")
                    .or_else(|| object.get("mimeType"))
                    .and_then(Value::as_str)
                    .is_some_and(|mime| {
                        mime.starts_with("image/")
                            || mime.starts_with("audio/")
                            || mime == "application/octet-stream"
                    })
                {
                    return;
                }
                if let Some(kind) = object.get("type").and_then(Value::as_str) {
                    if !matches!(
                        kind.to_ascii_lowercase().as_str(),
                        "text" | "output_text" | "input_text"
                    ) {
                        return;
                    }
                }
                for key in [
                    "text",
                    "output_text",
                    "input_text",
                    "content",
                    "output",
                    "stdout",
                    "stderr",
                ] {
                    if let Some(value) = object.get(key) {
                        extract(value, out, depth + 1);
                    }
                }
            }
            _ => (),
        }
    }
    let parsed = value
        .as_str()
        .filter(|text| matches!(text.trim_start().chars().next(), Some('{' | '[')))
        .and_then(|text| serde_json::from_str::<Value>(text).ok());
    let mut out = Vec::new();
    extract(parsed.as_ref().unwrap_or(value), &mut out, 0);
    detail_text(&out.join("\n"), 2048)
}

pub fn guard_observation(
    row: &mut Value,
    current_address: &Value,
    same_binding: bool,
    same_pty: bool,
) -> bool {
    if same_binding && same_pty && row["address"] == *current_address {
        return true;
    }
    for key in ["title", "request", "progress"] {
        row[key] = json!("");
    }
    for key in ["character", "harness", "done_outcome", "done_summary"] {
        row[key] = Value::Null;
    }
    row["status"] = json!("unknown");
    row["status_reason"] = json!("binding changed during observation; refreshing");
    false
}

pub fn detail_text(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

fn identity(value: &Value, key: &str) -> Result<String> {
    let text = field(value, key).with_context(|| format!("missing {key}"))?;
    if text.len() > 256 || text.chars().any(char::is_control) {
        bail!("invalid {key}");
    }
    Ok(text.to_owned())
}

pub fn pane_id(machine: &str, key: &str) -> String {
    // Length prefixes prevent collisions even if a legacy key contains '/'.
    format!("{}:{machine}{key}", machine.len())
}

pub fn normalize_panes(machine: &str, label: &str, rows: &[Value], at: u64) -> Result<Vec<Value>> {
    if rows.len() > MAX_PANES {
        bail!("source pane limit exceeded");
    }
    let mut ids = HashSet::new();
    let mut surfaces = HashSet::new();
    rows.iter()
        .filter_map(|row| {
            let address = &row["address"];
            if field(address, "machine_id") != Some(machine) {
                return Some(Err(anyhow::anyhow!("source machine mismatch")));
            }
            // 한 줄의 신원이 깨졌으면(제어문자·256자 초과) **그 줄만** 버린다. 전에는 여기서
            // 관측 전체가 실패해 그 기계 판이 「관측 불가」로 30분 넘게 굳었다(2026-09-18
            // 맥북). 어느 id 였는지는 생산자 쪽(`collab_board_source`)이 로그로 말한다.
            let (Ok(key), Ok(surface)) = (identity(address, "surface_key"), identity(address, "surface_id")) else {
                return None;
            };
            let id = pane_id(machine, &key);
            if !ids.insert(id.clone()) || !surfaces.insert(surface.clone()) {
                return Some(Err(anyhow::anyhow!("ambiguous surface identity")));
            }
            let mut clean = json!({"id":id,"address":{"machine_id":machine,
            "surface_key":key,"surface_id":surface},"machine_label":short(label,256),
            "room_label":"","title":"","request":"","progress":"","status":"unknown",
            "observed_at_ms":at,"freshness":"fresh"});
            for name in ["session_id", "instance_id"] {
                if let Ok(value) = identity(address, name) {
                    clean["address"][name] = json!(value);
                }
            }
            for name in TRACKED.iter().copied().filter(|name| *name != "address") {
                if let Some(text) = row[name].as_str() {
                    clean[name] = json!(short(text, 512));
                } else if let Some(flag) = row[name].as_bool() {
                    clean[name] = json!(flag);
                }
            }
            if !matches!(
                field(&clean, "status"),
                Some(
                    "unknown"
                        | "working"
                        | "waiting"
                        | "idle"
                        | "attention"
                        | "blocked"
                        | "completed"
                        | "failed"
                )
            ) {
                clean["status"] = json!("unknown");
            }
            Some(Ok(clean))
        })
        .collect()
}

#[cfg(test)]
mod normalize_panes_tests {
    use super::*;

    /// 깨진 id 한 줄이 그 기계 판 전체를 버리지 않는다 — 멀쩡한 줄은 남는다.
    #[test]
    fn a_row_with_a_control_character_id_is_dropped_alone() {
        let rows = vec![
            json!({"address":{"machine_id":"m","surface_key":"k1","surface_id":"%1"},"status":"working"}),
            json!({"address":{"machine_id":"m","surface_key":"k2","surface_id":"%2\u{7}"},"status":"idle"}),
            json!({"address":{"machine_id":"m","surface_key":"k3","surface_id":"%3","session_id":"bad\u{1b}"},"status":"idle"}),
        ];
        let out = normalize_panes("m", "M", &rows, 1).unwrap();
        let surfaces: Vec<&str> = out.iter().filter_map(|r| field(&r["address"], "surface_id")).collect();
        assert_eq!(surfaces, vec!["%1", "%3"]);
        assert!(field(&out[1]["address"], "session_id").is_none(), "깨진 session_id 는 칸만 비운다");
        let other = vec![json!({"address":{"machine_id":"other","surface_key":"k","surface_id":"%1"}})];
        assert!(normalize_panes("m", "M", &other, 1).is_err(), "기계가 다른 줄은 여전히 관측 실패다");
    }
}

pub struct BoardStore {
    epoch: String,
    seq: u64,
    local_id: String,
    sources: BTreeMap<String, Value>,
    panes: BTreeMap<String, Value>,
    events: VecDeque<Value>,
    path: Option<PathBuf>,
    pub journal_error: Option<String>,
    startup_reason: String,
}

impl BoardStore {
    pub fn new(local_id: String, epoch: String, path: Option<PathBuf>) -> Self {
        let mut store = Self {
            epoch,
            seq: 0,
            local_id,
            sources: BTreeMap::new(),
            panes: BTreeMap::new(),
            events: VecDeque::new(),
            path,
            journal_error: None,
            startup_reason: "restart_gap".into(),
        };
        if let Some(path) = store.path.as_ref() {
            match load_events(path) {
                Ok(events) => store.events = events,
                Err(error) if !path.exists() => {
                    let _ = error;
                }
                Err(_) => store.startup_reason = "journal_corrupt".into(),
            }
        }
        let local = store.local_id.clone();
        store.event(
            "observation_gap",
            &local,
            None,
            "Collector started; intervening activity was not observed",
            vec![],
        );
        store
    }

    pub fn cursor(&self) -> String {
        format!("{}:{}", self.epoch, self.seq)
    }

    pub fn source(&self, machine: &str) -> Option<Value> {
        self.sources.get(machine).cloned()
    }

    pub fn resolve_alias(&mut self, alias: &str, machine: &str) {
        if alias.starts_with("unresolved:") && self.sources.remove(alias).is_some() {
            self.event(
                "source_resolved",
                machine,
                None,
                "Discovered source identity resolved",
                vec!["machine_id"],
            );
        }
    }

    fn event(
        &mut self,
        kind: &str,
        machine: &str,
        pane: Option<&str>,
        summary: &str,
        fields: Vec<&str>,
    ) {
        self.seq += 1;
        let mut event = json!({"cursor":self.cursor(),"at_ms":now_ms(),"kind":kind,
            "machine_id":short(machine,256),"summary":short(summary,240),"fields":fields});
        if let Some(pane) = pane {
            event["pane_id"] = json!(short(pane, 520));
            if let Some(row) = self.panes.get(pane) {
                for key in ["room_id", "room_label"] {
                    if let Some(text) = field(row, key) {
                        event[key] = json!(short(text, 256));
                    }
                }
            }
        }
        self.events.push_back(event);
        while self.events.len() > MAX_EVENTS || self.encoded().len() > MAX_JOURNAL_BYTES {
            self.events.pop_front();
        }
    }

    fn encoded(&self) -> Vec<u8> {
        serde_json::to_vec(&json!({"schema_version":1,"events":self.events})).unwrap()
    }

    pub fn persist(&mut self) {
        if let Some(path) = &self.path {
            self.journal_error = atomic_private_write(path, &self.encoded())
                .err()
                .map(|_| "journal_write_failed".into());
        }
    }

    pub fn observe(&mut self, source: &Value) -> Result<()> {
        let machine = identity(source, "machine_id")?;
        let label = field(source, "label").unwrap_or(&machine);
        if self.sources.len() >= 128 && !self.sources.contains_key(&machine) {
            bail!("source limit exceeded");
        }
        let at = source["observed_at_ms"]
            .as_u64()
            .context("missing source observation time")?;
        if source["state"] != "online" {
            bail!("source is not online");
        }
        let complete = source["complete"]
            .as_bool()
            .context("missing source completeness")?;
        let rows = normalize_panes(
            &machine,
            label,
            source["panes"].as_array().context("missing source panes")?,
            at,
        )?;
        let total = self
            .panes
            .values()
            .filter(|p| p["address"]["machine_id"] != machine)
            .count()
            + rows.len();
        if total > MAX_PANES {
            bail!("board pane limit exceeded");
        }
        let previous = self.sources.get(&machine);
        if previous.is_none_or(|s| s["state"] != "online" || s["complete"] != complete) {
            self.event(
                "source_online",
                &machine,
                None,
                "Source observation available",
                vec!["state", "complete"],
            );
        }
        let capabilities: Vec<_> = source["capabilities"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .take(16)
            .map(|s| short(s, 64))
            .collect();
        self.sources.insert(machine.clone(), json!({"machine_id":machine,"label":short(label,256),
            "state":"online","observed_at_ms":at,"complete":complete,"is_local":machine == self.local_id,
            "source_kind":short(field(source,"source_kind").unwrap_or("unknown"),64),"capabilities":capabilities}));
        let present: HashSet<_> = rows
            .iter()
            .map(|p| p["id"].as_str().unwrap().to_owned())
            .collect();
        for row in rows {
            let id = row["id"].as_str().unwrap().to_owned();
            let fields: Vec<_> = match self.panes.get(&id) {
                Some(old) => TRACKED
                    .iter()
                    .copied()
                    .filter(|key| old[*key] != row[*key])
                    .collect(),
                None => vec![],
            };
            let existed = self.panes.contains_key(&id);
            let status = field(&row, "status").unwrap_or("unknown").to_owned();
            self.panes.insert(id.clone(), row);
            if !existed {
                self.event(
                    "pane_added",
                    &machine,
                    Some(&id),
                    "Live place observed",
                    vec![],
                );
            } else if !fields.is_empty() {
                let summary = format!("Observed {status}");
                self.event("pane_changed", &machine, Some(&id), &summary, fields);
            }
        }
        if complete {
            let missing: Vec<_> = self
                .panes
                .iter()
                .filter(|(id, row)| {
                    row["address"]["machine_id"] == machine && !present.contains(*id)
                })
                .map(|(id, _)| id.clone())
                .collect();
            for id in missing {
                self.event(
                    "pane_removed",
                    &machine,
                    Some(&id),
                    "Live place absent from complete source",
                    vec![],
                );
                self.panes.remove(&id);
            }
        }
        Ok(())
    }

    pub fn fail_source(&mut self, machine: &str, label: &str, reason: &str) {
        if self.sources.len() >= 128 && !self.sources.contains_key(machine) {
            return;
        }
        let observed = self
            .sources
            .get(machine)
            .and_then(|s| s["observed_at_ms"].as_u64())
            .unwrap_or(0);
        let state = if observed != 0 {
            "stale"
        } else if reason == "unsupported_api" {
            "unsupported"
        } else {
            "offline"
        };
        let changed = self
            .sources
            .get(machine)
            .is_none_or(|s| s["state"] != state);
        if changed {
            self.event(
                "source_unavailable",
                machine,
                None,
                "Source unavailable; prior observations retained",
                vec!["state"],
            );
        }
        let previous = self.sources.get(machine).cloned().unwrap_or(Value::Null);
        self.sources.insert(machine.to_owned(),json!({"machine_id":machine,"label":short(label,256),
            "state":state,"observed_at_ms":observed,"complete":false,"error":short(reason,240),
            "is_local":machine == self.local_id,"source_kind":field(&previous,"source_kind").unwrap_or("unknown"),
            "capabilities":previous["capabilities"].as_array().cloned().unwrap_or_default()}));
        for row in self
            .panes
            .values_mut()
            .filter(|p| p["address"]["machine_id"] == machine)
        {
            row["freshness"] = json!("stale");
        }
    }

    pub fn snapshot(&self, local: bool) -> Value {
        let included = |machine: &Value| !local || machine.as_str() == Some(&self.local_id);
        let mut sources: Vec<Value> = self
            .sources
            .values()
            .filter(|s| included(&s["machine_id"]))
            .cloned()
            .collect();
        let mut stale = HashSet::new();
        for source in &mut sources {
            let at = source["observed_at_ms"].as_u64().unwrap_or(0);
            if source["state"] == "online" && now_ms().saturating_sub(at) > 15_000 {
                source["state"] = json!("stale");
                source["complete"] = json!(false);
                source["error"] = json!("observation overdue");
            }
            if source["state"] != "online" {
                stale.insert(source["machine_id"].as_str().unwrap_or_default().to_owned());
            }
        }
        let panes: Vec<Value> = self
            .panes
            .values()
            .filter(|p| included(&p["address"]["machine_id"]))
            .cloned()
            .map(|mut p| {
                if stale.contains(p["address"]["machine_id"].as_str().unwrap_or_default()) {
                    p["freshness"] = json!("stale");
                }
                p
            })
            .collect();
        json!({"schema_version":1,"scope":if local {"local"} else {"all"},"cursor":self.cursor(),
            "observed_at_ms":now_ms(),"sources":sources,"panes":panes,
            "recent_changes":self.events.iter().rev().filter(|e| included(&e["machine_id"])).take(100).collect::<Vec<_>>(),
            "observation_mode":"periodic","journal_error":self.journal_error})
    }

    pub fn changes(&self, since: Option<&str>, limit: usize, local: bool) -> Value {
        let parsed = since
            .and_then(|s| s.rsplit_once(':'))
            .and_then(|(epoch, n)| n.parse::<u64>().ok().map(|n| (epoch, n)));
        let oldest = self
            .events
            .iter()
            .filter_map(|e| e["cursor"].as_str()?.rsplit_once(':'))
            .find_map(|(epoch, n)| {
                (epoch == self.epoch)
                    .then(|| n.parse::<u64>().ok())
                    .flatten()
            })
            .unwrap_or(self.seq + 1);
        let reason = match parsed {
            None => Some("missing_or_invalid_cursor"),
            Some((epoch, _)) if epoch != self.epoch => Some(self.startup_reason.as_str()),
            Some((_, n)) if n > self.seq => Some("cursor_ahead"),
            Some((_, n)) if n.saturating_add(1) < oldest => Some("cursor_expired"),
            _ => None,
        };
        if reason.is_some() {
            return json!({"schema_version":1,"cursor":self.cursor(),"changes":[],"reset_required":true,
                "reset_reason":reason,"has_more":false,"journal_error":self.journal_error});
        }
        let n = parsed.unwrap().1;
        let mut changes = Vec::new();
        let limit = limit.clamp(1, 200);
        let mut next = self.cursor();
        let mut has_more = false;
        for event in &self.events {
            let Some((epoch, seq)) = event["cursor"].as_str().and_then(|s| s.rsplit_once(':'))
            else {
                continue;
            };
            if epoch != self.epoch || seq.parse::<u64>().unwrap_or(0) <= n {
                continue;
            }
            if local && event["machine_id"] != self.local_id {
                continue;
            }
            if changes.len() == limit {
                has_more = true;
                break;
            }
            next = event["cursor"].as_str().unwrap().to_owned();
            changes.push(event.clone());
        }
        if !has_more {
            next = self.cursor();
        }
        json!({"schema_version":1,"cursor":next,"changes":changes,"reset_required":false,
            "reset_reason":null,"has_more":has_more,"journal_error":self.journal_error})
    }
}

fn load_events(path: &Path) -> Result<VecDeque<Value>> {
    let file = std::fs::File::open(path)?;
    if file.metadata()?.len() > MAX_JOURNAL_BYTES as u64 {
        bail!("journal too large");
    }
    let mut bytes = Vec::new();
    file.take((MAX_JOURNAL_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    let value: Value = serde_json::from_slice(&bytes)?;
    let events = value["events"].as_array().context("invalid journal")?;
    if value["schema_version"] != 1 || events.len() > MAX_EVENTS {
        bail!("invalid journal");
    }
    let mut clean = VecDeque::new();
    for event in events {
        let allowed = [
            "cursor",
            "at_ms",
            "kind",
            "machine_id",
            "pane_id",
            "summary",
            "fields",
            "room_id",
            "room_label",
        ];
        if event
            .as_object()
            .is_none_or(|o| o.keys().any(|k| !allowed.contains(&k.as_str())))
            || field(event, "cursor").is_none()
            || serde_json::to_vec(event)?.len() > 4096
        {
            bail!("invalid journal event");
        }
        clean.push_back(event.clone());
    }
    Ok(clean)
}

fn atomic_private_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("journal needs parent")?;
    std::fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }
    let temp = path.with_extension("tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(&temp)?;
    let result = (|| -> Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        replace_file(&temp, path)?;
        #[cfg(unix)]
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

#[cfg(not(windows))]
fn replace_file(source: &Path, target: &Path) -> std::io::Result<()> {
    std::fs::rename(source, target)
}

#[cfg(windows)]
fn replace_file(source: &Path, target: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let target: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            target.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source(machine: &str, ids: &[&str]) -> Value {
        json!({"machine_id":machine,"label":machine,"state":"online","complete":true,"observed_at_ms":10,
            "panes":ids.iter().map(|id| json!({"address":{"machine_id":machine,"surface_id":id,"surface_key":id},
                "request":"DO NOT PERSIST THIS BODY","progress":"DO NOT PERSIST THIS BODY","status":"working"})).collect::<Vec<_>>()})
    }
    fn store() -> BoardStore {
        BoardStore::new("a".into(), "epoch".into(), None)
    }

    #[test]
    fn selected_rollout_activity_is_bounded_and_keeps_tool_results() {
        let tail = [json!({"type":"event_msg","payload":{"type":"user_message","message":"inspect"}}),
            json!({"type":"response_item","payload":{"type":"function_call","call_id":"c","name":"exec_command","arguments":"line\nnext"}}),
            json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"c","output":"x".repeat(9000)}})]
            .into_iter().map(|v|v.to_string()).collect::<Vec<_>>().join("\n");
        let events = rollout_activity(&tail, 2);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, "tool");
        assert_eq!(events[0].text, "line\nnext");
        assert_eq!(events[1].kind, "result");
        assert_eq!(events[1].name, "exec_command");
        assert_eq!(events[1].text.len(), 2048);
    }

    #[test]
    fn observation_rechecks_binding_session_surface_and_pty_before_publication() {
        let original = json!({"address":{"machine_id":"m","surface_key":"stable","surface_id":"%1","session_id":"old","instance_id":"server"},
            "title":"old title","request":"old private request","progress":"old private reply","character":"old character",
            "harness":"codex","done_outcome":"succeeded","done_summary":"old completion","status":"idle"});
        let mut unchanged = original.clone();
        assert!(guard_observation(
            &mut unchanged,
            &original["address"],
            true,
            true
        ));
        assert_eq!(unchanged, original);
        for (field, same_binding, same_pty) in [
            ("session_id", false, true),
            ("surface_key", true, true),
            ("instance_id", true, false),
            ("", false, true),
            ("", true, false),
        ] {
            let mut current = original["address"].clone();
            if !field.is_empty() {
                current[field] = json!("replacement");
            }
            let mut row = original.clone();
            assert!(!guard_observation(
                &mut row,
                &current,
                same_binding,
                same_pty
            ));
            assert_eq!(
                row["address"], original["address"],
                "old content must never receive a replacement address"
            );
            for name in ["title", "request", "progress"] {
                assert_eq!(row[name], "");
            }
            assert!(row["done_summary"].is_null());
            assert_eq!(row["status"], "unknown");
        }
    }

    #[test]
    fn rollout_activity_reads_legacy_and_item_completed_message_shapes() {
        let records = [
            json!({"type":"event_msg","payload":{"type":"user_message","message":"legacy request"}}),
            json!({"type":"event_msg","payload":{"type":"agent_message","message":"legacy reply"}}),
            json!({"type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","content":[{"type":"text","text":"new request"}]}}}),
            json!({"type":"event_msg","payload":{"type":"item_completed","item":{"type":"AgentMessage","content":[{"type":"Text","text":"new reply"}]}}}),
        ];
        let tail = records
            .into_iter()
            .map(|record| record.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let events = rollout_activity(&tail, 10);
        assert_eq!(
            events
                .iter()
                .map(|event| (event.kind.as_str(), event.text.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("prompt", "legacy request"),
                ("say", "legacy reply"),
                ("prompt", "new request"),
                ("say", "new reply")
            ]
        );
    }

    #[test]
    fn rollout_activity_never_serializes_image_or_unknown_payload_fields() {
        let payload = "PAYLOAD_MUST_NOT_LEAK";
        let mixed = json!([{"type":"output_text","text":"safe result"},
            {"type":"image","mimeType":"image/png","data":payload,"text":payload},
            {"type":"image_url","image_url":{"url":format!("data:image/png;base64,{payload}")}},
            {"unknown":{"text":payload},"data":payload,"text":"safe caption"},
            [{"type":"Text","text":"nested safe"},{"type":"unknown","text":payload}]]);
        let outputs = [
            mixed.clone(),
            json!(mixed.to_string()),
            json!(format!("data:image/png;base64,{payload}")),
            json!({"mime_type":"image/png","data":payload,"text":payload}),
            json!(format!("iVBORw0KGgo{payload}")),
        ];
        let tail = outputs.into_iter().map(|output|json!({"type":"response_item","payload":{"type":"function_call_output","output":output}}).to_string()).collect::<Vec<_>>().join("\n");
        let events = rollout_activity(&tail, 50);
        let serialized = serde_json::to_string(&events).unwrap();
        assert!(!serialized.contains(payload));
        assert!(!serialized.contains("data:image"));
        assert!(!serialized.contains("iVBOR"));
        assert_eq!(events.len(), 2);
        assert!(events
            .iter()
            .all(|event| event.text == "safe result\nsafe caption\nnested safe"));
    }
    #[test]
    fn address_dedup_and_source_failure_never_remove_panes() {
        let mut s = store();
        let mut local = source("a", &["%1"]);
        local["panes"][0]["room_id"] = json!("room-a");
        local["panes"][0]["room_label"] = json!("First room");
        s.observe(&local).unwrap();
        s.observe(&source("b", &["%1"])).unwrap();
        assert_eq!(s.snapshot(false)["panes"].as_array().unwrap().len(), 2);
        assert!(s.observe(&source("a", &["%1", "%1"])).is_err());
        s.fail_source("b", "b", "offline");
        assert_eq!(s.snapshot(false)["panes"].as_array().unwrap().len(), 2);
        assert_eq!(s.snapshot(false)["sources"][1]["state"], "stale");
        let mut incomplete = source("b", &[]);
        incomplete["complete"] = json!(false);
        s.observe(&incomplete).unwrap();
        assert_eq!(s.panes.len(), 2);
        s.observe(&source("b", &[])).unwrap();
        assert_eq!(s.panes.len(), 1);
        s.observe(&source("b", &["%1"])).unwrap();
        assert_eq!(s.panes.len(), 2);
        s.observe(&source("a", &[])).unwrap();
        let removed = s.events.back().unwrap();
        assert_eq!(removed["kind"], "pane_removed");
        assert_eq!(removed["room_id"], "room-a");
        assert_eq!(removed["room_label"], "First room");
    }
    #[test]
    fn cursors_resume_expire_and_require_restart_reset() {
        let mut s = store();
        let cursor = s.cursor();
        s.observe(&source("a", &["%1"])).unwrap();
        let first = s.changes(Some(&cursor), 1, false);
        assert_eq!(first["has_more"], true);
        let next = s.changes(first["cursor"].as_str(), 100, false);
        assert_eq!(next["changes"].as_array().unwrap().len(), 1);
        assert_eq!(s.changes(Some("old:9"), 100, false)["reset_required"], true);
        for _ in 0..MAX_EVENTS + 2 {
            s.event("pane_changed", "a", None, "Changed", vec!["status"]);
        }
        assert_eq!(
            s.changes(Some(&cursor), 100, false)["reset_reason"],
            "cursor_expired"
        );
        assert!(s.encoded().len() <= MAX_JOURNAL_BYTES);
        assert!(!String::from_utf8(s.encoded())
            .unwrap()
            .contains("DO NOT PERSIST"));
    }
    #[test]
    fn persisted_history_is_private_bounded_and_corruption_resets() {
        let dir = std::env::temp_dir().join(format!(
            "kasa-board-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let path = dir.join("events.json");
        let mut s = BoardStore::new("a".into(), "one".into(), Some(path.clone()));
        s.observe(&source("a", &["%1"])).unwrap();
        s.persist();
        assert!(s.journal_error.is_none());
        let again = BoardStore::new("a".into(), "two".into(), Some(path.clone()));
        assert!(again.events.len() > 1);
        assert_eq!(
            again.changes(Some(&s.cursor()), 10, false)["reset_reason"],
            "restart_gap"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::write(&path, b"broken").unwrap();
        let broken = BoardStore::new("a".into(), "three".into(), Some(path.clone()));
        assert_eq!(
            broken.changes(Some("one:1"), 10, false)["reset_reason"],
            "journal_corrupt"
        );
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }
}
