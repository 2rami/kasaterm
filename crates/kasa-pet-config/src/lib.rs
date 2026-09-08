use serde_json::{json, Value};
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::Path,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PetPreferences {
    pub follow_cursor: bool,
    pub animations: bool,
    pub bubbles: bool,
    pub activity_reactions: bool,
    pub always_on_top: bool,
    pub lock_position: bool,
    pub scale_percent: Option<u32>,
    pub text_pt: u32,
    pub say_seconds: u32,
    pub sleep_minutes: u32,
}

impl Default for PetPreferences {
    fn default() -> Self {
        Self {
            follow_cursor: true,
            animations: true,
            bubbles: true,
            activity_reactions: true,
            always_on_top: true,
            lock_position: false,
            scale_percent: None,
            text_pt: 13,
            say_seconds: 12,
            sleep_minutes: 8,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreferenceChange {
    FollowCursor(bool),
    Animations(bool),
    Bubbles(bool),
    ActivityReactions(bool),
    AlwaysOnTop(bool),
    LockPosition(bool),
    ScalePercent(Option<u32>),
    TextPt(u32),
    SaySeconds(u32),
    SleepMinutes(u32),
}

impl PreferenceChange {
    fn field(self) -> (&'static str, Value) {
        match self {
            Self::FollowCursor(v) => ("follow_cursor", json!(v)),
            Self::Animations(v) => ("animations", json!(v)),
            Self::Bubbles(v) => ("bubbles", json!(v)),
            Self::ActivityReactions(v) => ("activity_reactions", json!(v)),
            Self::AlwaysOnTop(v) => ("always_on_top", json!(v)),
            Self::LockPosition(v) => ("lock_position", json!(v)),
            Self::ScalePercent(v) => ("scale_percent", json!(v.map(|n| n.clamp(40, 300)))),
            Self::TextPt(v) => ("text_pt", json!(v.clamp(8, 40))),
            Self::SaySeconds(v) => ("say_seconds", json!(v.clamp(3, 60))),
            Self::SleepMinutes(v) => ("sleep_minutes", json!(v.min(60))),
        }
    }
}

fn document(dir: &Path) -> Value {
    fs::read(dir.join("preferences.json"))
        .ok()
        .and_then(|v| serde_json::from_slice::<Value>(&v).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

fn number(value: Option<&Value>, fallback: u32, min: u32, max: u32) -> u32 {
    value
        .and_then(Value::as_i64)
        .map(|n| n.clamp(min as i64, max as i64) as u32)
        .unwrap_or(fallback)
}

pub fn read(dir: &Path) -> PetPreferences {
    let v = document(dir);
    let d = PetPreferences::default();
    let flag = |key: &str, fallback| v.get(key).and_then(Value::as_bool).unwrap_or(fallback);
    let legacy_pt = fs::read_to_string(dir.join("text_pt"))
        .ok()
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|n| n.is_finite())
        .map(|n| n.round().clamp(8.0, 40.0) as u32)
        .unwrap_or(d.text_pt);
    PetPreferences {
        follow_cursor: flag("follow_cursor", d.follow_cursor),
        animations: flag("animations", d.animations),
        bubbles: flag("bubbles", d.bubbles),
        activity_reactions: flag("activity_reactions", d.activity_reactions),
        always_on_top: flag("always_on_top", d.always_on_top),
        lock_position: flag("lock_position", d.lock_position),
        scale_percent: v
            .get("scale_percent")
            .filter(|n| n.as_i64().is_some())
            .map(|n| number(Some(n), 100, 40, 300)),
        text_pt: number(v.get("text_pt"), legacy_pt, 8, 40),
        say_seconds: number(v.get("say_seconds"), d.say_seconds, 3, 60),
        sleep_minutes: number(v.get("sleep_minutes"), d.sleep_minutes, 0, 60),
    }
}

/// Each control changes one field after acquiring the shared lock, so the pet
/// menu and settings window cannot overwrite one another with stale snapshots.
pub fn update(dir: &Path, change: PreferenceChange) -> io::Result<PetPreferences> {
    fs::create_dir_all(dir)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.join(".preferences.lock"))?;
    lock.lock()?;
    let mut v = document(dir);
    let (key, value) = change.field();
    v[key] = value;
    let temporary = dir.join(format!(".preferences-{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(serde_json::to_string_pretty(&v)?.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, dir.join("preferences.json"))?;
        Ok(read(dir))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scratch() -> std::path::PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "pet-config-test-{}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&p).unwrap();
        p
    }
    #[test]
    fn missing_broken_and_partial_documents_keep_independent_defaults() {
        let p = scratch();
        assert_eq!(read(&p), PetPreferences::default());
        fs::write(p.join("preferences.json"), "{").unwrap();
        assert_eq!(read(&p), PetPreferences::default());
        fs::write(p.join("text_pt"), "19").unwrap();
        fs::write(p.join("preferences.json"), r#"{"bubbles":false,"animations":"bad","scale_percent":999,"text_pt":-1,"say_seconds":0,"sleep_minutes":100}"#).unwrap();
        let v = read(&p);
        assert!(!v.bubbles && v.animations);
        assert_eq!(
            (v.scale_percent, v.text_pt, v.say_seconds, v.sleep_minutes),
            (Some(300), 8, 3, 60)
        );
        fs::write(p.join("preferences.json"), "{}").unwrap();
        assert_eq!(read(&p).text_pt, 19);
        fs::remove_dir_all(p).unwrap();
    }
    #[test]
    fn concurrent_controls_preserve_each_other_and_legacy_files() {
        let p = scratch();
        fs::write(p.join("current"), "Wanko").unwrap();
        fs::write(p.join("state.json"), "original-position").unwrap();
        fs::write(p.join("preferences.json"), r#"{"future_option":7}"#).unwrap();
        std::thread::scope(|s| {
            s.spawn(|| {
                update(&p, PreferenceChange::Bubbles(false)).unwrap();
            });
            s.spawn(|| {
                update(&p, PreferenceChange::TextPt(22)).unwrap();
            });
        });
        let v = read(&p);
        assert!(!v.bubbles);
        assert_eq!(v.text_pt, 22);
        assert_eq!(document(&p)["future_option"], 7);
        assert_eq!(
            fs::read_to_string(p.join("state.json")).unwrap(),
            "original-position"
        );
        assert_eq!(fs::read_to_string(p.join("current")).unwrap(), "Wanko");
        fs::remove_dir_all(p).unwrap();
    }
}
