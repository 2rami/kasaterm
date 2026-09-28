use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const MAX_BODY: usize = 128 * 1024;
const MAX_MACHINES: usize = 128;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub revision: u64,
    pub settings: BTreeMap<String, Value>,
    pub machines: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Patch {
    pub expected_revision: u64,
    pub settings: BTreeMap<String, Value>,
    pub machines: BTreeMap<String, Value>,
}

fn text(v: &Value, max: usize) -> bool {
    v.as_str().is_some_and(|s| s.chars().count() <= max && !s.chars().any(char::is_control))
}

fn name(v: &Value, max: usize) -> bool {
    text(v, max) && v.as_str().is_some_and(|s| !s.contains(['/', '\\']) && !s.starts_with('~'))
}

fn token(v: &Value) -> bool {
    v.as_str().is_some_and(|s| !s.is_empty() && s.len() <= 128
        && s.bytes().all(|c| c.is_ascii_alphanumeric() || b"-_.:#".contains(&c)))
}

fn number(v: &Value, low: f64, high: f64) -> bool {
    v.as_f64().is_some_and(|n| n.is_finite() && (low..=high).contains(&n))
}

fn color(v: &Value) -> bool {
    v.as_str().is_some_and(|s| s.strip_prefix('#').is_some_and(|s| {
        matches!(s.len(), 6 | 8) && s.bytes().all(|c| c.is_ascii_hexdigit())
    }))
}

fn preset_icon(v: &Value) -> bool {
    matches!(v.as_str(), Some("auto" | "laptop" | "monitor" | "server" | "smartphone"))
}

fn palette(v: &Value) -> bool {
    let Some(o) = v.as_object() else { return false };
    !o.is_empty() && o.len() <= 40 && o.iter().all(|(key, value)| match key.as_str() {
        "slug" | "base" => token(value),
        "label" => text(value, 80),
        "bg" | "fg" | "surface" | "surface_hover" | "surface_active" | "border" | "text"
        | "text_dim" | "text_mute" | "success" | "danger" | "pane_bg" | "header_bg" | "sidebar_bg"
        | "titlebar_bg" | "side_panel_bg" => color(value),
        "ansi" => value.as_array().is_some_and(|a| a.len() == 16 && a.iter().all(color)),
        _ => false,
    })
}

pub fn valid_setting(key: &str, v: &Value) -> bool {
    match key {
        "language" => matches!(v.as_str(), Some("ko" | "en" | "ja" | "system" | "auto")),
        "mobile_theme_mode" => matches!(v.as_str(), Some("light" | "dark" | "system")),
        "theme" | "theme_system_light" | "theme_system_dark" | "accent" | "shape" | "character_theme"
        | "cursor_shape" | "mouse_cursor" | "tab_position" => token(v),
        "ui_font" => name(v, 128),
        "font_size" => number(v, 9.0, 72.0),
        "min_contrast" => number(v, 1.0, 21.0),
        "cursor_thickness" => number(v, 1.0, 8.0),
        "cursor_color" => color(v) || token(v),
        "status_bar_h" | "pane_footer_h" => number(v, 0.0, 96.0),
        "character_appearance" | "claude_persona" | "sidebar_persona" | "terminal_persona"
        | "file_tree_default" | "pane_footer_default" | "usage_compact" | "sidebar_pulse" => v.is_boolean(),
        "statusbar_hidden" | "statusbar_order" => v.as_array().is_some_and(|a| a.len() <= 40 && a.iter().all(token)),
        "statusbar_separators" => v.is_boolean(),
        "statusbar_colors" | "machine_colors" => v.as_object().is_some_and(|o| o.len() <= 128
            && o.iter().all(|(k, v)| text(&Value::String(k.clone()), 80) && color(v))),
        "device_icons" => v.as_object().is_some_and(|o| o.len() <= 128
            && o.iter().all(|(k, v)| text(&Value::String(k.clone()), 80) && preset_icon(v))),
        "custom_themes" => v.as_array().is_some_and(|a| a.len() <= 24 && a.iter().all(palette)),
        "custom_theme" => palette(v),
        _ => false,
    }
}

fn ssh_target(v: &Value) -> bool {
    let Some(s) = v.as_str() else { return false };
    if s.is_empty() || s.len() > 253 || s.starts_with('-') { return false; }
    let host = if let Some((user, host)) = s.rsplit_once('@') {
        if !ssh_user(&Value::String(user.into())) { return false; }
        host
    } else { s };
    if host.is_empty() || host.starts_with('-') { return false; }
    if host.contains(':') {
        host.trim_start_matches('[').trim_end_matches(']').parse::<std::net::IpAddr>().is_ok()
    } else {
        host.bytes().all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
    }
}

fn ssh_user(v: &Value) -> bool {
    v.as_str().is_some_and(|s| !s.is_empty() && !s.starts_with('-') && s.len() <= 64
        && s.bytes().all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c)))
}

fn machine_field(key: &str, value: &Value) -> bool {
    match key {
        "label" => text(value, 80) && value.as_str().is_some_and(|s| !s.trim().is_empty()),
        "ssh" => ssh_target(value),
        "host" => ssh_target(value) && value.as_str().is_some_and(|s| !s.contains('@')),
        "user" => ssh_user(value),
        "machine_id" => token(value),
        "port" => value.as_u64().is_some_and(|n| (1..=65535).contains(&n)),
        "icon" => preset_icon(value),
        "color" => color(value),
        "unresolved" => value.is_boolean(),
        _ => false,
    }
}

pub fn valid_machine(v: &Value) -> bool {
    let Some(o) = v.as_object() else { return false };
    !o.is_empty() && o.len() <= 10 && o.iter().all(|(key, value)| machine_field(key, value))
        && o.get("label").is_some_and(|v| text(v, 80))
        && (o.contains_key("ssh") || o.contains_key("host") || o.contains_key("machine_id"))
}

pub fn valid_machine_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128
        && id.bytes().all(|c| c.is_ascii_alphanumeric() || b"-_.:".contains(&c))
}

pub fn safe_local_settings(local: &Value) -> BTreeMap<String, Value> {
    local.as_object().into_iter().flatten()
        .filter(|(key, value)| valid_setting(key, value))
        .map(|(key, value)| (key.clone(), value.clone())).collect()
}

pub fn safe_local_machine(local: &Value) -> Option<Value> {
    let o = local.as_object()?;
    let clean = Value::Object(o.iter().filter(|(k, v)| machine_field(k, v))
        .map(|(k, v)| (k.clone(), v.clone())).collect());
    valid_machine(&clean).then_some(clean)
}

fn validate_maps(settings: &BTreeMap<String, Value>, machines: &BTreeMap<String, Value>, patch: bool) -> Result<(), String> {
    if settings.len() > 64 || machines.len() > MAX_MACHINES { return Err("too_many_entries".into()); }
    for (key, value) in settings {
        if value.is_null() && patch {
            // A tombstone cannot name a field whose value would never be allowed.
            if !known_setting(key) { return Err("unsupported_setting".into()); }
        } else if !valid_setting(key, value) { return Err("unsupported_setting".into()); }
    }
    for (id, value) in machines {
        if !valid_machine_id(id) || !(patch && value.is_null() || valid_machine(value)) {
            return Err("invalid_machine".into());
        }
    }
    Ok(())
}

pub fn known_setting(key: &str) -> bool {
    matches!(key, "language" | "mobile_theme_mode" | "theme" | "theme_system_light" | "theme_system_dark" | "accent" | "shape"
        | "character_theme" | "cursor_shape" | "mouse_cursor" | "tab_position" | "ui_font" | "font_size"
        | "min_contrast" | "cursor_thickness" | "cursor_color" | "status_bar_h" | "pane_footer_h"
        | "character_appearance" | "claude_persona" | "sidebar_persona" | "terminal_persona"
        | "file_tree_default" | "pane_footer_default" | "usage_compact" | "sidebar_pulse"
        | "statusbar_hidden" | "statusbar_order" | "statusbar_separators" | "statusbar_colors"
        | "machine_colors" | "device_icons" | "custom_themes" | "custom_theme")
}

pub fn validate_snapshot(snapshot: &Snapshot) -> Result<(), String> {
    validate_maps(&snapshot.settings, &snapshot.machines, false)?;
    if serde_json::to_vec(snapshot).map_err(|_| "invalid_snapshot")?.len() > MAX_BODY {
        return Err("snapshot_too_large".into());
    }
    Ok(())
}

pub fn validate_patch(patch: &Patch) -> Result<(), String> {
    validate_maps(&patch.settings, &patch.machines, true)?;
    if serde_json::to_vec(patch).map_err(|_| "invalid_patch")?.len() > MAX_BODY {
        return Err("patch_too_large".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn secrets_commands_paths_and_update_preferences_never_leave_local_settings() {
        let local = json!({"theme":"graphite", "font_size":14, "character_appearance":false,
            "gemini_api_key":"secret", "password":"secret", "default_shell":"/bin/zsh",
            "ui_font":"/tmp/font.ttf", "default_cwd":"/private", "shim_inject":true,
            "update_channel":"preview", "automatic_update_on_quit":true,
            "feedback_draft":"private text", "device_icons":{"mini":"file:/secret.svg"}});
        let got = safe_local_settings(&local);
        assert_eq!(got.keys().map(String::as_str).collect::<Vec<_>>(), ["character_appearance", "font_size", "theme"]);
        let machine = safe_local_machine(&json!({"label":"Mini", "ssh":"user@example.com", "port":22,
            "base":"http://127.0.0.1:9999", "key":"/private/key", "roots":{"/a":"/b"},
            "home":true, "restart_approvals":true, "ProxyCommand":"echo secret"})).unwrap();
        assert_eq!(machine, json!({"label":"Mini", "ssh":"user@example.com", "port":22}));
    }

    #[test]
    fn remote_values_and_tombstones_cannot_escape_the_same_allowlist() {
        for (key, value) in [("password", json!(null)), ("ui_font", json!("../key")),
            ("custom_themes", json!([{"slug":"test","token":"secret"}]))] {
            let patch = Patch { settings: [(key.into(), value)].into(), ..Default::default() };
            assert!(validate_patch(&patch).is_err());
        }
        assert!(!valid_machine(&json!({"label":"bad", "ssh":"-oProxyCommand=bad"})));
        assert!(!valid_machine(&json!({"label":"bad", "ssh":"user:password@host"})));
        assert!(!valid_machine(&json!({"label":"bad", "ssh":"host", "key":"secret"})));
        assert!(!valid_machine_id("../../another-account"));
    }

    #[test]
    fn custom_palette_and_explicit_safe_deletion_round_trip() {
        let patch = Patch { settings: [("theme".into(), Value::Null),
            ("custom_themes".into(), json!([{"slug":"my-palette", "label":"Custom", "base":"graphite",
                "pane_bg":"#112233", "header_bg":"#223344", "sidebar_bg":"#334455",
                "titlebar_bg":"#445566", "side_panel_bg":"#556677"}]))].into(),
            ..Default::default() };
        assert_eq!(validate_patch(&patch), Ok(()));
        let round_trip: Patch = serde_json::from_value(serde_json::to_value(&patch).unwrap()).unwrap();
        assert_eq!(round_trip.settings["custom_themes"][0]["titlebar_bg"], "#445566");
        assert_eq!(round_trip.settings["custom_themes"][0]["side_panel_bg"], "#556677");
        assert!(serde_json::from_value::<Patch>(json!({"expected_revision":0,"settings":{},"machines":{},"account":"other"})).is_err());
    }
}
