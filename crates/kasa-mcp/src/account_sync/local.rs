use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::schema::{safe_local_settings, Snapshot};

static LOCAL_FILES: Mutex<()> = Mutex::new(());

pub fn lock_files() -> Result<MutexGuard<'static, ()>, String> {
    LOCAL_FILES.lock().map_err(|_| "local settings lock unavailable".into())
}

pub fn read_json(path: &Path, absent: Value) -> Result<Value, String> {
    use std::io::Read;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(absent),
        Err(_) => return Err("local settings cannot be read".into()),
    };
    let mut bytes = Vec::new();
    file.take(2 * 1024 * 1024 + 1).read_to_end(&mut bytes).map_err(|_| "local settings cannot be read")?;
    if bytes.len() > 2 * 1024 * 1024 { return Err("local settings exceed size limit".into()); }
    serde_json::from_slice(&bytes).map_err(|_| "local settings are not valid JSON".into())
}

pub fn write_private(path: &Path, value: &Value) -> Result<(), String> {
    write_atomic(path, value, true)
}

fn write_atomic(path: &Path, value: &Value, durable: bool) -> Result<(), String> {
    if std::fs::symlink_metadata(path).is_ok_and(|metadata| !metadata.is_file()) {
        return Err("local settings path is not a regular file".into());
    }
    let parent = path.parent().ok_or("local settings directory missing")?;
    std::fs::create_dir_all(parent).map_err(|_| "local settings directory unavailable")?;
    let temporary = parent.join(format!(".account-sync-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
        let mut file = options.open(&temporary)?;
        file.write_all(&serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?)?;
        if durable { file.sync_all()?; }
        std::fs::rename(&temporary, path)?;
        Ok::<_, std::io::Error>(())
    })();
    if result.is_err() { let _ = std::fs::remove_file(&temporary); }
    result.map_err(|_| "local settings could not be saved".into())
}

pub fn edit_json<T>(path: &Path, absent: Value, edit: impl FnOnce(&mut Value) -> Result<T, String>) -> Result<T, String> {
    let _guard = lock_files()?;
    let mut value = read_json(path, absent)?;
    let before = value.clone();
    let output = edit(&mut value)?;
    if value != before { write_atomic(path, &value, false)?; }
    Ok(output)
}

#[derive(Clone, Debug)]
pub struct Paths {
    pub settings: PathBuf,
    pub machines: PathBuf,
    pub binding: PathBuf,
}

impl Paths {
    pub fn current() -> Option<Self> {
        let base = kasa_socket::home_dir()?.join(".config/kasaterm");
        let settings = std::env::var_os("KASATERM_SETTINGS_FILE").map(PathBuf::from).unwrap_or_else(|| base.join("settings.json"));
        let machines = crate::machines::machines_path()?;
        let binding = std::env::var_os("KASATERM_ACCOUNT_SYNC_FILE").map(PathBuf::from)
            .or_else(|| std::env::var_os("KASATERM_DEVICE_FILE").map(|path| PathBuf::from(path).with_extension("sync.json")))
            .unwrap_or_else(|| base.join("account-sync.json"));
        Some(Self { settings, machines, binding })
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub remote: Snapshot,
    pub observed: Snapshot,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bindings {
    pub first_account: Option<String>,
    #[serde(default)]
    pub accounts: BTreeMap<String, Binding>,
}

pub fn account_key(relay: &str, account: &str) -> String {
    format!("{:x}", Sha256::digest(format!("{}\0{account}", relay.trim_end_matches('/')).as_bytes()))
}

pub fn load_bindings(path: &Path) -> Result<Bindings, String> {
    serde_json::from_value(read_json(path, serde_json::json!({"first_account":null,"accounts":{}}))?)
        .map_err(|_| "account sync binding cannot be read".into())
}

pub fn note_account(paths: &Paths, account: &str) -> Result<(), String> {
    let _guard = lock_files()?;
    let mut binding = load_bindings(&paths.binding)?;
    if binding.first_account.is_none() {
        binding.first_account = Some(account.into());
        write_private(&paths.binding, &serde_json::to_value(binding).map_err(|_| "account sync binding invalid")?)?;
    }
    Ok(())
}

fn host_name(value: &str) -> bool {
    !value.is_empty() && value.len() <= 253 && !value.starts_with('-')
        && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || b".-:".contains(&byte))
        && value != "localhost" && value != "127.0.0.1" && value != "::1"
}

fn user_name(value: &str) -> bool {
    !value.is_empty() && value.len() <= 64 && !value.starts_with('-')
        && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

fn direct_target(target: &str) -> Option<(String, String, u16)> {
    if let Some(uri) = target.strip_prefix("ssh://") {
        let (user, rest) = uri.split_once('@')?;
        let (host, port) = rest.rsplit_once(':')?;
        let host = host.trim_matches(['[', ']']);
        let port: u16 = port.parse().ok()?;
        return (user_name(user) && host_name(host) && port != 0).then(|| (user.into(), host.into(), port));
    }
    let (user, host) = target.split_once('@')?;
    (user_name(user) && host_name(host)).then(|| (user.into(), host.into(), 22))
}

fn config_target(alias: &str, config: &str) -> Option<(String, String, u16)> {
    if !user_name(alias) || config.len() > 512 * 1024 { return None; }
    let (mut host, mut user, mut port) = (None, None, None);
    let mut matches = true;
    for raw in config.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        let Some(split) = line.find(|c: char| c.is_ascii_whitespace() || c == '=') else { continue };
        let key = line[..split].to_ascii_lowercase();
        let value = line[split..].trim_start_matches(|c: char| c.is_ascii_whitespace() || c == '=');
        if key == "host" {
            let patterns: Vec<_> = value.split_ascii_whitespace().collect();
            matches = patterns.iter().any(|p| *p == "*" || p.eq_ignore_ascii_case(alias))
                && !patterns.iter().any(|p| p.strip_prefix('!').is_some_and(|p| p == "*" || p.eq_ignore_ascii_case(alias)));
            // Wildcard precedence requires OpenSSH evaluation; this parser never invokes it.
            if patterns.iter().any(|p| p != &"*" && p.contains(['*', '?', '%'])) { return None; }
            continue;
        }
        if key == "match" { return None; }
        if !matches { continue; }
        let value = value.trim_matches('"');
        match key.as_str() {
            "include" | "localcommand" | "remotecommand" => return None,
            "proxycommand" | "proxyjump" if value != "none" => return None,
            "hostname" if host.is_none() => {
                if !host_name(value) { return None; }
                host = Some(value.to_string());
            }
            "user" if user.is_none() => {
                if !user_name(value) { return None; }
                user = Some(value.to_string());
            }
            "port" if port.is_none() => {
                let parsed = value.parse::<u16>().ok()?;
                if parsed == 0 { return None; }
                port = Some(parsed);
            }
            _ => {}
        }
    }
    Some((user?, host?, port.unwrap_or(22)))
}

fn portable_alias(alias: &str) -> Option<(String, String, u16)> {
    if cfg!(test) || std::env::var_os("KASATERM_SETTINGS_FILE").is_some() { return None; }
    let path = kasa_socket::home_dir()?.join(".ssh/config");
    if std::fs::metadata(&path).ok()?.len() > 512 * 1024 { return None; }
    config_target(alias, &std::fs::read_to_string(path).ok()?)
}

pub fn shared_machine(entry: &Value) -> Option<Value> {
    let mut candidate = serde_json::Map::new();
    for key in ["label", "machine_id", "host", "user", "port", "icon", "color"] {
        if let Some(value) = entry.get(key) { candidate.insert(key.into(), value.clone()); }
    }
    let target = entry["ssh"].as_str().unwrap_or_default();
    if let Some((user, host, port)) = direct_target(target).or_else(|| portable_alias(target)) {
        candidate.insert("user".into(), Value::String(user));
        candidate.insert("host".into(), Value::String(host));
        candidate.entry("port").or_insert(Value::from(port));
    } else if !target.is_empty() {
        candidate.insert("ssh".into(), Value::String(target.into()));
    }
    let resolved = candidate.get("host").and_then(Value::as_str).is_some_and(host_name)
        && candidate.get("user").and_then(Value::as_str).is_some_and(user_name);
    candidate.insert("unresolved".into(), Value::Bool(!resolved));
    super::schema::safe_local_machine(&Value::Object(candidate))
}

pub fn machine_key(entry: &Value, shared: &Value) -> String {
    if let Some(id) = entry["account_sync_id"].as_str() { return id.to_string(); }
    if let Some(id) = shared["machine_id"].as_str() { return id.to_string(); }
    let identity = serde_json::json!([shared.get("host"), shared.get("user"), shared.get("port"), shared.get("ssh")]);
    format!("connection-{:x}", Sha256::digest(identity.to_string().as_bytes()))
}

pub fn snapshot_values(settings: &Value, machines: &Value) -> Result<Snapshot, String> {
    if !settings.is_object() || !machines.is_array() { return Err("local settings shape invalid".into()); }
    let mut output = Snapshot { revision: 0, settings: safe_local_settings(settings), machines: BTreeMap::new() };
    for entry in machines.as_array().unwrap() {
        if let Some(shared) = shared_machine(entry) { output.machines.insert(machine_key(entry, &shared), shared); }
    }
    super::schema::validate_snapshot(&output)?;
    Ok(output)
}

pub fn read_snapshot(paths: &Paths) -> Result<Snapshot, String> {
    let _guard = lock_files()?;
    snapshot_values(&read_json(&paths.settings, serde_json::json!({}))?, &read_json(&paths.machines, serde_json::json!([]))?)
}

#[derive(Clone, Debug, Default)]
pub struct Delta {
    pub settings: BTreeMap<String, Value>,
    pub machines: BTreeMap<String, Option<BTreeMap<String, Value>>>,
}

fn fields_diff(before: &BTreeMap<String, Value>, after: &BTreeMap<String, Value>) -> BTreeMap<String, Value> {
    before.keys().chain(after.keys()).filter(|key| before.get(*key) != after.get(*key))
        .map(|key| (key.clone(), after.get(key).cloned().unwrap_or(Value::Null))).collect()
}

fn object_map(value: Option<&Value>) -> BTreeMap<String, Value> {
    value.and_then(Value::as_object).map(|value| value.iter().map(|(key, value)| (key.clone(), value.clone())).collect()).unwrap_or_default()
}

pub fn delta(before: &Snapshot, after: &Snapshot) -> Delta {
    let machines = before.machines.keys().chain(after.machines.keys())
        .filter(|key| before.machines.get(*key) != after.machines.get(*key))
        .map(|key| (key.clone(), after.machines.get(key).map(|_| fields_diff(&object_map(before.machines.get(key)), &object_map(after.machines.get(key))))))
        .collect();
    Delta { settings: fields_diff(&before.settings, &after.settings), machines }
}

pub fn patch_on(remote: &Snapshot, changes: &Delta) -> super::schema::Patch {
    let mut machines = BTreeMap::new();
    for (id, change) in &changes.machines {
        let value = match change {
            None => Value::Null,
            Some(fields) => {
                let mut entry = object_map(remote.machines.get(id));
                patch_fields(&mut entry, fields);
                serde_json::to_value(entry).unwrap()
            }
        };
        machines.insert(id.clone(), value);
    }
    super::schema::Patch { expected_revision: remote.revision, settings: changes.settings.clone(), machines }
}

pub fn patch_fields(target: &mut BTreeMap<String, Value>, changes: &BTreeMap<String, Value>) {
    for (key, value) in changes {
        if value.is_null() { target.remove(key); } else { target.insert(key.clone(), value.clone()); }
    }
}

pub fn remote_effect(previous: Option<&Binding>, original: &Snapshot, remote: &Snapshot) -> Snapshot {
    let empty = Snapshot::default();
    let old_remote = previous.map(|binding| &binding.remote).unwrap_or(&empty);
    let changes = delta(old_remote, remote);
    let mut expected = original.clone();
    patch_fields(&mut expected.settings, &changes.settings);
    for (id, value) in patch_on(&expected, &changes).machines {
        if value.is_null() { expected.machines.remove(&id); } else { expected.machines.insert(id, value); }
    }
    expected
}

fn transport_target(shared: &Value) -> Option<String> {
    let host = shared["host"].as_str().filter(|value| host_name(value))?;
    let user = shared["user"].as_str().filter(|value| user_name(value))?;
    let port = shared["port"].as_u64().unwrap_or(22);
    if port == 0 || port > 65535 { return None; }
    let host = if host.contains(':') { format!("[{host}]") } else { host.to_string() };
    Some(format!("ssh://{user}@{host}:{port}"))
}

pub fn merge_files(settings: &mut Value, machines: &mut Value, original: &Snapshot, expected: &Snapshot, account: &str) -> Result<(), String> {
    let current = snapshot_values(settings, machines)?;
    let remote = delta(original, expected);
    let settings = settings.as_object_mut().ok_or("settings must be an object")?;
    for (key, value) in remote.settings {
        if current.settings.get(&key) != original.settings.get(&key) { continue; }
        if value.is_null() { settings.remove(&key); } else { settings.insert(key, value); }
    }
    let rows = machines.as_array_mut().ok_or("machines must be an array")?;
    for (id, fields) in remote.machines {
        let index = rows.iter().position(|entry| shared_machine(entry).is_some_and(|shared| machine_key(entry, &shared) == id));
        if fields.is_none() {
            if let Some(index) = index {
                let owned = rows[index]["account_sync_owner"].as_str() == Some(account);
                let local_fields = ["base", "key", "roots", "home", "kvm", "chrome_port"].iter().any(|key| rows[index].get(key).is_some());
                if owned && !local_fields && current.machines.get(&id) == original.machines.get(&id) { rows.remove(index); }
            }
            continue;
        }
        let mut value = object_map(current.machines.get(&id));
        let old = object_map(original.machines.get(&id));
        for (key, field) in fields.unwrap() {
            if value.get(&key) == old.get(&key) {
                if field.is_null() { value.remove(&key); } else { value.insert(key, field); }
            }
        }
        let shared = serde_json::to_value(value).map_err(|_| "connection invalid")?;
        let entry = if let Some(index) = index { &mut rows[index] } else {
            rows.push(serde_json::json!({"account_sync_id":id,"account_sync_owner":account}));
            rows.last_mut().unwrap()
        };
        let object = entry.as_object_mut().ok_or("connection must be an object")?;
        for key in ["label", "machine_id", "host", "user", "port", "icon", "color"] {
            if let Some(value) = shared.get(key) { object.insert(key.into(), value.clone()); }
            else { object.remove(key); }
        }
        let managed = object.get("account_sync_owner").and_then(Value::as_str) == Some(account);
        if let Some(target) = transport_target(&shared) {
            if index.is_none() || managed || object.get("ssh").and_then(Value::as_str).and_then(direct_target).is_some() {
                object.insert("ssh".into(), Value::String(target));
            }
            object.remove("account_sync_unresolved");
        } else if index.is_none() || managed {
            object.insert("ssh".into(), shared.get("ssh").cloned().unwrap_or(Value::String(String::new())));
            object.insert("account_sync_unresolved".into(), Value::Bool(true));
        }
        object.insert("account_sync_id".into(), Value::String(id));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn simple_ssh_alias_exports_only_portable_public_fields_without_running_openssh() {
        let text = "Host mini\n HostName mini.example.com\n User operator\n Port 2222\n IdentityFile /private/id_secret\nHost *\n User fallback\n";
        assert_eq!(config_target("mini", text), Some(("operator".into(), "mini.example.com".into(), 2222)));
        assert_eq!(config_target("other", text), None);
        for extra in ["ProxyCommand cloudflared access ssh --hostname %h", "ProxyJump bastion",
            "Include /private/config", "Match exec secret-command", "RemoteCommand command"] {
            let config = format!("Host mini\n HostName mini.example.com\n User operator\n {extra}\n");
            assert_eq!(config_target("mini", &config), None, "{extra}");
        }
    }

    #[test]
    fn machine_metadata_never_exports_credentials_paths_or_proxy_commands() {
        let raw = json!({"label":"Mini","ssh":"user@mini.example","key":"/private/key", "base":"http://localhost:1234",
            "roots":{"/private":"/secret"},"home":true,"ProxyCommand":"secret", "password":"secret"});
        let shared = shared_machine(&raw).unwrap();
        assert_eq!(shared["host"], "mini.example");
        assert_eq!(shared["user"], "user");
        assert!(!shared.to_string().contains("secret"));
        assert!(!shared.to_string().contains("private"));
        let alias = shared_machine(&json!({"label":"Mini","ssh":"private-alias"})).unwrap();
        assert_eq!(alias["unresolved"], true);
        let mut settings = json!({});
        let mut machines = json!([]);
        let remote = Snapshot { machines: [("mini".into(), alias)].into(), ..Default::default() };
        merge_files(&mut settings, &mut machines, &Snapshot::default(), &remote, "account-a").unwrap();
        assert_eq!(machines[0]["account_sync_unresolved"], true);
        assert!(crate::machines::parse_listed(&machines).is_empty());
    }

    #[test]
    fn rebase_preserves_remote_fields_in_the_same_connection() {
        let base = Snapshot { settings: [("font_size".into(), json!(12))].into(),
            machines: [("mini".into(), json!({"label":"Mini","host":"one.example","user":"user","port":22}))].into(), ..Default::default() };
        let mut local = base.clone();
        local.settings.insert("font_size".into(), json!(16));
        local.machines.get_mut("mini").unwrap()["label"] = json!("New name");
        let mut remote = base.clone();
        remote.revision = 2;
        remote.settings.insert("theme".into(), json!("graphite"));
        remote.machines.get_mut("mini").unwrap()["host"] = json!("two.example");
        let patch = patch_on(&remote, &delta(&base, &local));
        assert_eq!(patch.expected_revision, 2);
        assert!(!patch.settings.contains_key("theme"));
        assert_eq!(patch.machines["mini"]["host"], "two.example");
        assert_eq!(patch.machines["mini"]["label"], "New name");
    }

    #[test]
    fn late_local_edits_and_local_connection_fields_survive_remote_apply() {
        let old_settings = json!({"font_size":12,"theme":"graphite","update_channel":"preview", "gemini_api_key":"private"});
        let old_machines = json!([{"label":"Mini","ssh":"user@mini.example", "key":"/private/key","roots":{"/local":"/remote"}}]);
        let original = snapshot_values(&old_settings, &old_machines).unwrap();
        let mut expected = original.clone();
        expected.settings.insert("font_size".into(), json!(16));
        expected.settings.insert("theme".into(), json!("ink"));
        let mut settings = old_settings;
        settings["font_size"] = json!(18);
        let mut machines = old_machines;
        merge_files(&mut settings, &mut machines, &original, &expected, "account-a").unwrap();
        assert_eq!(settings["font_size"], 18);
        assert_eq!(settings["theme"], "ink");
        assert_eq!(settings["gemini_api_key"], "private");
        assert_eq!(settings["update_channel"], "preview");
        assert_eq!(machines[0]["key"], "/private/key");
        assert_eq!(machines[0]["roots"], json!({"/local":"/remote"}));
    }

    #[test]
    fn invalid_local_json_is_never_replaced_and_atomic_edits_are_private() {
        let directory = std::env::temp_dir().join(format!("account-sync-local-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let file = directory.join("settings.json");
        std::fs::write(&file, "broken").unwrap();
        assert!(edit_json(&file, json!({}), |_| Ok(())).is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "broken");
        write_private(&file, &json!({"token":"local secret","theme":"ink"})).unwrap();
        edit_json(&file, json!({}), |value| { value["font_size"] = json!(16); Ok(()) }).unwrap();
        assert_eq!(read_json(&file, json!({})).unwrap()["token"], "local secret");
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
