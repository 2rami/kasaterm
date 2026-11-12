use anyhow::{bail, ensure, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const TTL_MS: u64 = 30_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action { Inspect, Screen, Restore, Restart, UpdateCheck }

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub action: Action,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect: Option<String>,
    #[serde(default)]
    pub interrupt: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<u32>,
}

impl Request {
    pub fn validate(&self) -> Result<()> {
        ensure!(!self.interrupt || self.action == Action::Restart, "interrupt is only valid for restart");
        ensure!(self.display.is_none() || self.action == Action::Screen, "display is only valid for screen");
        if matches!(self.action, Action::Restore | Action::Restart | Action::UpdateCheck) {
            ensure!(self.expect.as_ref().is_some_and(|v| !v.is_empty()), "inspect first, then pass --expect REVISION");
        } else {
            ensure!(self.expect.is_none(), "read actions do not accept --expect");
        }
        Ok(())
    }
}

pub fn needs_ready(method: &str) -> bool {
    matches!(method,
        "surface.focus" | "surface.split" | "surface.split_fleet" | "surface.remote" | "surface.migrate"
        | "surface.send_text" | "surface.launch" | "surface.server" | "surface.paste" | "surface.send_key" | "surface.send_raw"
        | "surface.close" | "surface.close_checked" | "surface.close_idle_checked" | "surface.reopen_checked"
        | "surface.dock" | "surface.undock" | "surface.rename" | "surface.set_color" | "surface.repersona"
        | "surface.swap" | "surface.move" | "surface.new_tab" | "surface.resize_divider" | "surface.set_ratio" | "surface.set_ratio_between"
        | "surface.open_preview" | "surface.open_url" | "app.open_document" | "web.drive"
        | "session.new" | "session.switch" | "session.close" | "session.resume"
        | "window.new" | "window.switch" | "window.close" | "window.close_checked" | "window.reorder" | "window.rename"
        | "collab.act" | "collab.tell" | "remote.spawn_shell" | "machine.unfold" | "machine.home")
}

fn hash(state: &Value) -> String {
    format!("{:x}", Sha256::digest(state.to_string().as_bytes()))
}

pub fn revision(state: &Value, now_ms: u64) -> String {
    format!("{now_ms}.{}", hash(state))
}

pub fn check_revision(state: &Value, revision: &str, now_ms: u64) -> Result<()> {
    let Some((at, digest)) = revision.split_once('.') else { bail!("invalid view revision") };
    let at: u64 = at.parse().map_err(|_| anyhow::anyhow!("invalid view revision"))?;
    ensure!(at <= now_ms && now_ms - at <= TTL_MS, "view expired; inspect again");
    ensure!(digest == hash(state), "view changed; inspect again");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn actions_cannot_reuse_a_different_instance_selection_or_expired_view() {
        let state = json!({"instance":"one", "room":1, "restore":[true,false]});
        let rev = revision(&state, 100);
        assert!(check_revision(&state, &rev, 101).is_ok());
        for changed in [json!({"instance":"two", "room":1, "restore":[true,false]}),
            json!({"instance":"one", "room":2, "restore":[true,false]}),
            json!({"instance":"one", "room":1, "restore":[true,true]})] {
            assert!(check_revision(&changed, &rev, 101).is_err());
        }
        assert!(check_revision(&state, &rev, 99).is_err());
        assert!(check_revision(&state, &rev, 100 + TTL_MS + 1).is_err());
    }

    #[test]
    fn unknown_actions_fields_and_unscoped_mutations_are_rejected() {
        assert!(serde_json::from_value::<Request>(json!({"action":"quit"})).is_err());
        assert!(serde_json::from_value::<Request>(json!({"action":"restart","approved":true})).is_err());
        for action in ["restore", "restart", "update-check"] {
            let request: Request = serde_json::from_value(json!({"action":action})).unwrap();
            assert!(request.validate().is_err());
        }
        let request: Request = serde_json::from_value(json!({"action":"inspect","interrupt":true})).unwrap();
        assert!(request.validate().is_err());
    }
}
