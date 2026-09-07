//! 창 하단 상태줄의 표시 순서와 밀도.
//!
//! 렌더 패스는 프레임마다 돌기 때문에 여기서는 파일을 읽지 않는다. `App`이 시작할
//! 때 설정을 한 번 정규화해 들고 있고, 설정 화면의 액션이 같은 값을 바로 바꾼다.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

pub(crate) const WIDGETS: [&str; 8] = [
    "claude",
    "codex",
    "ports",
    "pet",
    "clipboard",
    "resources",
    "tunnel",
    "version",
];
pub(crate) const USAGE_FIELDS: [&str; 5] = ["account", "email", "session", "weekly", "model"];
const PROVIDERS: [&str; 2] = ["claude", "codex"];
const DEFAULT_USAGE_FIELDS: [&str; 3] = ["account", "session", "weekly"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Prefs {
    pub(crate) order: Vec<String>,
    pub(crate) hidden: HashSet<String>,
    pub(crate) colors: HashMap<String, String>,
    pub(crate) usage_fields: HashMap<String, Vec<String>>,
    pub(crate) separators: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            order: WIDGETS.iter().map(|id| (*id).to_string()).collect(),
            hidden: HashSet::new(),
            colors: HashMap::new(),
            usage_fields: PROVIDERS
                .iter()
                .map(|provider| {
                    (
                        (*provider).to_string(),
                        DEFAULT_USAGE_FIELDS
                            .iter()
                            .map(|field| (*field).to_string())
                            .collect(),
                    )
                })
                .collect(),
            separators: true,
        }
    }
}

impl Prefs {
    pub(crate) fn from_settings(settings: &serde_json::Value) -> Self {
        let mut out = Self::default();
        if let Some(order) = settings.get("statusbar_order").and_then(|v| v.as_array()) {
            out.order = normalized_order(order.iter().filter_map(|v| v.as_str()));
        }
        if let Some(hidden) = settings.get("statusbar_hidden").and_then(|v| v.as_array()) {
            out.hidden = hidden
                .iter()
                .filter_map(|v| v.as_str())
                .filter(|id| valid_widget(id))
                .map(str::to_string)
                .collect();
        }
        if let Some(colors) = settings.get("statusbar_colors").and_then(|v| v.as_object()) {
            out.colors = colors
                .iter()
                .filter_map(|(id, value)| {
                    let color = value.as_str()?;
                    (valid_widget(id) && crate::theme::parse_hex(color).is_some())
                        .then(|| (id.clone(), color.to_ascii_lowercase()))
                })
                .collect();
        }
        if let Some(fields) = settings
            .get("statusbar_usage_fields")
            .and_then(|v| v.as_object())
        {
            for provider in PROVIDERS {
                let Some(values) = fields.get(provider).and_then(|v| v.as_array()) else {
                    continue;
                };
                let mut seen = HashSet::new();
                let normalized = values
                    .iter()
                    .filter_map(|v| v.as_str())
                    .filter(|field| valid_usage_field(field) && seen.insert((*field).to_string()))
                    .map(str::to_string)
                    .collect();
                out.usage_fields.insert(provider.to_string(), normalized);
            }
        }
        out.separators = settings
            .get("statusbar_separators")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        out
    }

    pub(crate) fn visible(&self, id: &str) -> bool {
        valid_widget(id) && !self.hidden.contains(id)
    }

    pub(crate) fn has_usage_field(&self, provider: &str, field: &str) -> bool {
        self.usage_fields
            .get(provider)
            .is_some_and(|fields| fields.iter().any(|value| value == field))
    }

    pub(crate) fn wants_usage(&self, provider: &str) -> bool {
        ["session", "weekly", "model"]
            .iter()
            .any(|field| self.has_usage_field(provider, field))
    }

    pub(crate) fn color(&self, id: &str, fallback: [u8; 4]) -> [u8; 4] {
        let Some(rgb) = self.colors.get(id).and_then(|value| crate::theme::parse_hex(value)) else {
            return fallback;
        };
        [rgb[0], rgb[1], rgb[2], fallback[3]]
    }

    pub(crate) fn toggle_item(&mut self, id: &str) {
        if !valid_widget(id) {
            return;
        }
        if !self.hidden.remove(id) {
            self.hidden.insert(id.to_string());
        }
    }

    pub(crate) fn move_item(&mut self, id: &str, delta: i8) {
        let Some(at) = self.order.iter().position(|value| value == id) else {
            return;
        };
        let to = (at as isize + delta.signum() as isize)
            .clamp(0, self.order.len().saturating_sub(1) as isize) as usize;
        if at != to && group(&self.order[at]) == group(&self.order[to]) {
            self.order.swap(at, to);
        }
    }

    pub(crate) fn set_color(&mut self, id: &str, value: &str) {
        if !valid_widget(id) {
            return;
        }
        if value.trim().is_empty() {
            self.colors.remove(id);
        } else if crate::theme::parse_hex(value).is_some() {
            self.colors.insert(id.to_string(), value.to_ascii_lowercase());
        }
    }

    pub(crate) fn toggle_usage_field(&mut self, provider: &str, field: &str) {
        if !PROVIDERS.contains(&provider) || !valid_usage_field(field) {
            return;
        }
        let fields = self.usage_fields.entry(provider.to_string()).or_default();
        if let Some(at) = fields.iter().position(|value| value == field) {
            fields.remove(at);
        } else {
            fields.push(field.to_string());
            fields.sort_by_key(|value| {
                USAGE_FIELDS
                    .iter()
                    .position(|field| field == value)
                    .unwrap_or(usize::MAX)
            });
        }
    }

    pub(crate) fn json_order(&self) -> serde_json::Value {
        serde_json::json!(self.order.clone())
    }

    pub(crate) fn json_hidden(&self) -> serde_json::Value {
        let hidden: Vec<&str> = self
            .order
            .iter()
            .filter(|id| self.hidden.contains(id.as_str()))
            .map(String::as_str)
            .collect();
        serde_json::json!(hidden)
    }

    pub(crate) fn json_colors(&self) -> serde_json::Value {
        serde_json::json!(self.colors.clone())
    }

    pub(crate) fn json_usage_fields(&self) -> serde_json::Value {
        serde_json::json!(self.usage_fields.clone())
    }
}

fn normalized_order<'a>(values: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for id in values {
        if valid_widget(id) && seen.insert(id.to_string()) {
            out.push(id.to_string());
        }
    }
    for id in WIDGETS {
        if seen.insert(id.to_string()) {
            out.push(id.to_string());
        }
    }
    out.sort_by_key(|id| group(id));
    out
}

fn valid_widget(id: &str) -> bool {
    WIDGETS.contains(&id)
}

fn valid_usage_field(field: &str) -> bool {
    USAGE_FIELDS.contains(&field)
}

pub(crate) fn group(id: &str) -> u8 {
    match id {
        "claude" | "codex" => 0,
        "ports" | "pet" | "clipboard" => 1,
        _ => 2,
    }
}

/// 온라인인데 현재 빌드와 다른 기기. 상태줄 렌더가 파일 명부를 프레임마다 읽지
/// 않도록 짧게 캐시하고, 오프라인은 일치로 둔갑시키지 않고 이 경고와 별개 상태로 둔다.
pub(crate) fn mismatched_machines() -> Vec<String> {
    type Cache = Mutex<Option<(Instant, Vec<String>)>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Ok(guard) = cache.lock() {
        if let Some((at, values)) = guard.as_ref() {
            if at.elapsed() < Duration::from_secs(2) {
                return values.clone();
            }
        }
    }
    let values = kasa_mcp::machines::snapshot()
        .into_iter()
        .filter(|machine| {
            machine.get("online").and_then(|v| v.as_bool()) == Some(true)
                && machine.get("build_match").and_then(|v| v.as_bool()) == Some(false)
        })
        .filter_map(|machine| {
            machine
                .get("label")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .collect::<Vec<_>>();
    if let Ok(mut guard) = cache.lock() {
        *guard = Some((Instant::now(), values.clone()));
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_keep_the_quiet_account_bar() {
        let p = Prefs::default();
        assert_eq!(p.order, WIDGETS);
        assert!(p.hidden.is_empty());
        for provider in PROVIDERS {
            assert!(p.has_usage_field(provider, "account"));
            assert!(p.has_usage_field(provider, "session"));
            assert!(p.has_usage_field(provider, "weekly"));
            assert!(!p.has_usage_field(provider, "email"));
            assert!(!p.has_usage_field(provider, "model"));
        }
    }

    #[test]
    fn malformed_settings_cannot_hide_new_widgets_or_inject_unknown_ids() {
        let p = Prefs::from_settings(&serde_json::json!({
            "statusbar_order": ["codex", "bogus", "codex"],
            "statusbar_hidden": ["bogus", "ports"],
            "statusbar_colors": {"claude":"#112233", "codex":"broken"},
            "statusbar_usage_fields": {"claude":["weekly", "bogus", "weekly"]},
        }));
        assert_eq!(p.order.first().map(String::as_str), Some("codex"));
        assert_eq!(p.order.len(), WIDGETS.len());
        assert_eq!(p.hidden, HashSet::from(["ports".to_string()]));
        assert_eq!(p.colors.get("claude").map(String::as_str), Some("#112233"));
        assert!(!p.colors.contains_key("codex"));
        assert_eq!(p.usage_fields["claude"], ["weekly"]);
        assert_eq!(p.usage_fields["codex"], DEFAULT_USAGE_FIELDS);
    }

    #[test]
    fn move_toggle_and_reset_are_reversible() {
        let mut p = Prefs::default();
        p.move_item("codex", -1);
        assert_eq!(&p.order[..2], ["codex", "claude"]);
        p.toggle_item("ports");
        assert!(!p.visible("ports"));
        p.toggle_item("ports");
        assert!(p.visible("ports"));
        p.toggle_usage_field("claude", "model");
        assert!(p.has_usage_field("claude", "model"));
        p.set_color("claude", "#ABCDEF");
        assert_eq!(p.colors["claude"], "#abcdef");
        p.set_color("claude", "");
        assert!(!p.colors.contains_key("claude"));
    }
}
