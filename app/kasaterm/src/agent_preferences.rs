use serde_json::{json, Value};

pub(crate) const PROVIDERS: &[&str] = &["claude", "codex"];
pub(crate) const MODES: &[&str] = &["default", "workspace", "unrestricted"];
pub(crate) const FIELDS: &[&str] = &["model", "usage", "cwd"];

pub(crate) fn preferred_agent() -> &'static str {
    match crate::socket::read_settings()
        .get("preferred_agent")
        .and_then(Value::as_str)
    {
        Some("codex") => "codex",
        _ => "claude",
    }
}

pub(crate) fn set_preferred_agent(provider: &str) -> Result<(), String> {
    if !PROVIDERS.contains(&provider) {
        return Err("Unknown agent provider".into());
    }
    crate::socket::write_settings_patch_atomic(&[("preferred_agent", json!(provider))])
        .map_err(|error| error.to_string())
}

fn permission_from(settings: &Value, provider: &str) -> &'static str {
    let key = format!("agent_permission_{provider}");
    if let Some(value) = settings.get(&key).and_then(Value::as_str) {
        return MODES
            .iter()
            .copied()
            .find(|mode| *mode == value)
            .unwrap_or("default");
    }
    // Existing installations retain their launch policy until explicitly changed.
    if provider == "codex" {
        "unrestricted"
    } else {
        "default"
    }
}

pub(crate) fn permission(provider: &str) -> &'static str {
    permission_from(&crate::socket::read_settings(), provider)
}

pub(crate) fn statusline_enabled(field: &str) -> bool {
    crate::socket::read_settings()
        .get(format!("agent_statusline_{field}"))
        .and_then(Value::as_bool)
        .unwrap_or(true)
}

fn statusline_customized_from(settings: &Value) -> bool {
    FIELDS.iter().any(|field| {
        settings
            .get(format!("agent_statusline_{field}"))
            .and_then(Value::as_bool)
            .is_some()
    })
}

pub(crate) fn statusline_customized() -> bool {
    statusline_customized_from(&crate::socket::read_settings())
}

fn statusline_custom_patch(settings: &Value, on: bool) -> Vec<(&'static str, Value)> {
    [
        "agent_statusline_model",
        "agent_statusline_usage",
        "agent_statusline_cwd",
    ]
    .into_iter()
    .map(|key| {
        (
            key,
            if on {
                json!(settings.get(key).and_then(Value::as_bool).unwrap_or(true))
            } else {
                Value::Null
            },
        )
    })
    .collect()
}

pub(crate) fn set_statusline_custom(on: bool) -> Result<(), String> {
    crate::socket::write_settings_patch_atomic(&statusline_custom_patch(
        &crate::socket::read_settings(),
        on,
    ))
    .map_err(|error| error.to_string())
}

pub(crate) fn set_permission(provider: &str, mode: &str) -> Result<(), String> {
    if !PROVIDERS.contains(&provider) || !MODES.contains(&mode) {
        return Err("Unknown agent permission choice".into());
    }
    crate::socket::write_settings_patch_atomic(&[(
        &format!("agent_permission_{provider}"),
        json!(mode),
    )])
    .map_err(|error| error.to_string())
}

pub(crate) fn set_statusline(field: &str, enabled: bool) -> Result<(), String> {
    if !FIELDS.contains(&field) {
        return Err("Unknown agent statusline field".into());
    }
    crate::socket::write_settings_patch_atomic(&[(
        &format!("agent_statusline_{field}"),
        json!(enabled),
    )])
    .map_err(|error| error.to_string())
}

pub(crate) fn initialize_fresh_defaults() -> std::io::Result<()> {
    let settings = crate::socket::read_settings();
    crate::socket::write_settings_patch_atomic(&fresh_defaults_patch(&settings))
}

fn fresh_defaults_patch(settings: &Value) -> Vec<(&'static str, Value)> {
    ["agent_permission_claude", "agent_permission_codex"]
        .into_iter()
        .filter(|key| settings.get(*key).is_none())
        .map(|key| (key, json!("default")))
        .collect()
}

pub(crate) fn values() -> Value {
    json!({
        "preferred_agent": preferred_agent(),
        "permissions": {"claude": permission("claude"), "codex": permission("codex")},
        "statusline": {"customized": statusline_customized(), "model": statusline_enabled("model"), "usage": statusline_enabled("usage"), "cwd": statusline_enabled("cwd")},
    })
}

fn codex_statusline_items(settings: &Value, original: Vec<String>) -> Option<Vec<String>> {
    if !statusline_customized_from(settings) {
        return None;
    }
    let mut items = original;
    for (field, canonical, aliases) in [
        (
            "model",
            "model-with-reasoning",
            &["model", "model-name", "model-with-reasoning"][..],
        ),
        (
            "usage",
            "context-used",
            &["context-used", "context-remaining"][..],
        ),
        (
            "cwd",
            "current-dir",
            &["current-dir", "project", "project-name", "project-root"][..],
        ),
    ] {
        let Some(on) = settings
            .get(format!("agent_statusline_{field}"))
            .and_then(Value::as_bool)
        else {
            continue;
        };
        if !on {
            items.retain(|item| !aliases.contains(&item.as_str()));
        } else if !items.iter().any(|item| aliases.contains(&item.as_str())) {
            items.push(canonical.to_string());
        }
    }
    Some(items)
}

pub(crate) fn codex_statusline_shell() -> String {
    let original = kasa_socket::home_dir()
        .and_then(|home| std::fs::read_to_string(home.join(".codex/config.toml")).ok())
        .and_then(|source| source.parse::<toml_edit::DocumentMut>().ok())
        .and_then(|doc| {
            doc.get("tui")?.get("status_line")?.as_array().map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect()
            })
        })
        .unwrap_or_else(|| {
            vec![
                "model-with-reasoning".into(),
                "context-remaining".into(),
                "git-branch".into(),
            ]
        });
    let Some(items) = codex_statusline_items(&crate::socket::read_settings(), original) else {
        return String::new();
    };
    let mut array = toml_edit::Array::new();
    for item in items {
        array.push(item);
    }
    let config = format!("tui.status_line={array}").replace('\'', "'\\''");
    // Prepending preserves explicit -c overrides without touching the user's TOML.
    format!("set -- -c '{config}' \"$@\"\n")
}

pub(crate) fn permission_shell(provider: &str, mode: &str) -> String {
    let (patterns, args) = if provider == "codex" {
        ("--dangerously-bypass-approvals-and-sandbox|--yolo|--full-auto|--approve-for-me|--ask-for-approval|--ask-for-approval=*|-a|-a?*|--sandbox|--sandbox=*|-s|-s?*|--profile|--profile=*|-p|-p?*|approval_policy=*|sandbox_mode=*|permissions.*|--config=approval_policy=*|--config=sandbox_mode=*|--config=permissions.*|-capproval_policy=*|-csandbox_mode=*|-cpermissions.*",
         match mode { "unrestricted" => "--dangerously-bypass-approvals-and-sandbox", "workspace" => "--sandbox workspace-write --ask-for-approval on-request", _ => "" })
    } else {
        ("--dangerously-skip-permissions|--permission-mode|--permission-mode=*|--settings|--settings=*",
         match mode { "unrestricted" => "--dangerously-skip-permissions", "workspace" => "--permission-mode acceptEdits", _ => "" })
    };
    if args.is_empty() {
        return String::new();
    }
    format!("KASA_PERMISSION_EXPLICIT=0\nfor a in \"$@\"; do\n  case \"$a\" in {patterns}) KASA_PERMISSION_EXPLICIT=1 ;; esac\ndone\n[ \"$KASA_PERMISSION_EXPLICIT\" = 1 ] || set -- {args} \"$@\"\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_defaults_and_invalid_values_do_not_escalate() {
        assert_eq!(permission_from(&json!({}), "codex"), "unrestricted");
        assert_eq!(permission_from(&json!({}), "claude"), "default");
        assert_eq!(
            permission_from(&json!({"agent_permission_codex": "broken"}), "codex"),
            "default"
        );
        assert_eq!(
            permission_from(&json!({"agent_permission_codex": "workspace"}), "codex"),
            "workspace"
        );
    }

    #[test]
    fn fresh_defaults_preserve_existing_choices_and_unrelated_settings_on_disk() {
        let dir = std::env::temp_dir().join(format!(
            "kasaterm-agent-preferences-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("settings.json");
        let mut settings = json!({"agent_permission_claude": "workspace", "font_size": 16, "custom": {"keep": true}});
        for (key, value) in fresh_defaults_patch(&settings) {
            settings[key] = value;
        }
        crate::socket::write_settings_value_atomic_at(&path, &settings).unwrap();
        let loaded: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(permission_from(&loaded, "claude"), "workspace");
        assert_eq!(permission_from(&loaded, "codex"), "default");
        assert_eq!(loaded["font_size"], 16);
        assert_eq!(loaded["custom"]["keep"], true);
        assert!(fresh_defaults_patch(&loaded).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn statusline_preserves_native_fields_until_each_choice_is_explicit() {
        let original = vec![
            "model-with-reasoning".into(),
            "context-used".into(),
            "git-branch".into(),
            "five-hour-limit".into(),
        ];
        assert!(codex_statusline_items(&json!({}), original.clone()).is_none());
        assert_eq!(
            codex_statusline_items(
                &json!({"agent_statusline_model": false, "agent_statusline_cwd": true}),
                original
            )
            .unwrap(),
            [
                "context-used",
                "git-branch",
                "five-hour-limit",
                "current-dir"
            ]
        );
    }

    #[test]
    fn custom_statusline_opt_in_seeds_all_fields_and_off_restores_native_config() {
        let mut settings = json!({"font_size": 14, "agent_statusline_model": false});
        for (key, value) in statusline_custom_patch(&settings, true) {
            settings[key] = value;
        }
        assert!(statusline_customized_from(&settings));
        assert_eq!(settings["agent_statusline_model"], false);
        assert_eq!(settings["agent_statusline_usage"], true);
        assert_eq!(settings["agent_statusline_cwd"], true);
        for (key, value) in statusline_custom_patch(&settings, false) {
            settings[key] = value;
        }
        assert!(!statusline_customized_from(&settings));
        assert!(codex_statusline_items(&settings, vec!["five-hour-limit".into()]).is_none());
        assert_eq!(settings["font_size"], 14);
        for (key, value) in statusline_custom_patch(&settings, true) {
            settings[key] = value;
        }
        assert!(FIELDS
            .iter()
            .all(|field| settings[format!("agent_statusline_{field}")] == true));
    }

    #[cfg(unix)]
    fn args(provider: &str, mode: &str, input: &[&str]) -> Vec<String> {
        let script = permission_shell(provider, mode) + "printf '%s\\n' \"$@\"";
        let out = std::process::Command::new("sh")
            .args(["-c", &script, "shim"])
            .args(input)
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[test]
    #[cfg(unix)]
    fn explicit_cli_permissions_take_priority_without_matching_prompt_text() {
        for input in [
            vec!["--sandbox=read-only"],
            vec!["-sread-only"],
            vec!["-a", "never"],
            vec!["-c", "approval_policy=never"],
            vec!["--profile", "safe"],
            vec!["--approve-for-me"],
        ] {
            assert_eq!(args("codex", "unrestricted", &input), input);
        }
        assert_eq!(
            args("claude", "unrestricted", &["--permission-mode=plan"]),
            ["--permission-mode=plan"]
        );
        assert_eq!(args("codex", "default", &["hello"]), ["hello"]);
        assert_eq!(
            args("codex", "workspace", &["explain --sandbox usage"]),
            [
                "--sandbox",
                "workspace-write",
                "--ask-for-approval",
                "on-request",
                "explain --sandbox usage"
            ]
        );
    }
}
