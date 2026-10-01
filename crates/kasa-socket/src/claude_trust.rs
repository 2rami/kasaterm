//! claude 폴더 신뢰 선탑재 — 「Quick safety check: Is this a project you trust?」 화면이
//! 아예 안 뜨게 claude 설정에 그 폴더의 신뢰 표시를 미리 심는다.
//!
//! 이 기계에서 처음 보는 폴더면 claude 가 뜨자마자 그 화면에서 멈추고, 무인으로 띄운
//! 학생(summon·이사·자동 resume)은 브리프도 못 받은 채 밤새 서 있는다. codex shim 은
//! 같은 일을 config.toml 에 이미 하고 있다.
//!
//! claude 가 보는 자리는 `projects[<폴더>].hasTrustDialogAccepted` 이고, 폴더 키는 실경로를
//! NFC 로 맞춘 문자열이다(claude 2.1.286 실측: 판정이 cwd 에서 위로 올라가며 같은 키를 본다).
//! 그래서 이 폴더 키 하나면 git 루트를 따로 몰라도 지나간다.

use std::path::{Path, PathBuf};
use unicode_normalization::UnicodeNormalization;

/// claude 전역 설정 파일. claude 와 같이 `CLAUDE_CONFIG_DIR` 이 있으면 그 안을 본다.
pub fn config_path() -> Option<PathBuf> {
    match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
        Some(dir) => Some(PathBuf::from(dir).join(".claude.json")),
        None => Some(crate::home_dir()?.join(".claude.json")),
    }
}

/// claude 가 신뢰를 찾는 키. `/tmp` 처럼 링크를 낀 경로는 실경로(`/private/tmp`)로 적어야
/// claude 가 찾는다. 한글 폴더는 NFD 로 적힌 이름이 있어 NFC 로 맞춘다.
pub fn trust_key(dir: &Path) -> String {
    let real = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let key: String = real.to_string_lossy().nfc().collect();
    if cfg!(windows) {
        key.strip_prefix(r"\\?\").unwrap_or(&key).replace('\\', "/")
    } else {
        key
    }
}

/// 설정 원문에 신뢰를 심은 새 원문. 이미 신뢰돼 있거나 원문을 못 읽으면 `None` — 그때는
/// 파일을 건드리지 않는다.
pub fn with_trust(raw: &str, key: &str) -> Option<String> {
    let mut config = serde_json::from_str::<serde_json::Value>(raw).ok()?;
    let projects = config
        .as_object_mut()?
        .entry("projects")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()?;
    let entry = projects.entry(key.to_string()).or_insert_with(|| serde_json::json!({}));
    if entry.get("hasTrustDialogAccepted").and_then(|v| v.as_bool()) == Some(true) {
        return None;
    }
    entry.as_object_mut()?.insert("hasTrustDialogAccepted".into(), serde_json::json!(true));
    serde_json::to_string_pretty(&config).ok()
}

/// `dir` 을 신뢰한 것으로 심는다. 새로 심었으면 `true`. 실패해도 조용히 넘어간다 — 그 화면은
/// 앱이 화면을 보고 한 번 넘겨 주는 길이 따로 있다.
///
/// ⚠️ 여러 claude 가 같은 파일을 수시로 다시 쓴다. 읽은 뒤 파일이 바뀌었으면 그 갱신을 덮지
/// 않도록 다시 읽고, 교체는 temp+rename 으로 원자적으로 한다. 설정 파일이 아직 없으면(이
/// 기계에서 claude 를 처음 띄움) 만들지 않는다 — 첫 실행의 온보딩이 그 파일을 만든다.
pub fn preseed(dir: &Path) -> bool {
    let Some(cfg) = config_path() else { return false };
    let key = trust_key(dir);
    for _ in 0..3 {
        let Ok(before) = std::fs::metadata(&cfg) else { return false };
        let Ok(raw) = std::fs::read_to_string(&cfg) else { return false };
        let Some(next) = with_trust(&raw, &key) else { return false };
        let tmp = cfg.with_extension(format!("json.kasaterm-{}", std::process::id()));
        if std::fs::write(&tmp, next).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return false;
        }
        let unchanged = std::fs::metadata(&cfg).is_ok_and(|now| {
            now.len() == before.len() && now.modified().ok() == before.modified().ok()
        });
        if unchanged && std::fs::rename(&tmp, &cfg).is_ok() {
            return true;
        }
        let _ = std::fs::remove_file(&tmp);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_is_added_once_and_other_settings_survive() {
        let raw = r#"{"numStartups":3,"projects":{"/a":{"allowedTools":["Bash"]}}}"#;
        let next = with_trust(raw, "/a").expect("untrusted folder gets a trust mark");
        let v: serde_json::Value = serde_json::from_str(&next).unwrap();
        assert_eq!(v["numStartups"], 3);
        assert_eq!(v["projects"]["/a"]["allowedTools"][0], "Bash");
        assert_eq!(v["projects"]["/a"]["hasTrustDialogAccepted"], true);
        assert_eq!(with_trust(&next, "/a"), None, "already trusted folder leaves the file alone");
    }

    #[test]
    fn missing_projects_table_is_created_but_unreadable_config_is_left_alone() {
        let next = with_trust("{}", "/new").unwrap();
        let v: serde_json::Value = serde_json::from_str(&next).unwrap();
        assert_eq!(v["projects"]["/new"]["hasTrustDialogAccepted"], true);
        assert_eq!(with_trust("{ broken", "/x"), None);
        assert_eq!(with_trust("[]", "/x"), None);
    }

    #[test]
    fn key_is_the_real_path_in_nfc() {
        let base = std::env::temp_dir().join(format!("kasa-trust-{}", std::process::id()));
        let nfd: String = "신뢰".nfd().collect();
        let dir = base.join(&nfd);
        std::fs::create_dir_all(&dir).unwrap();
        let key = trust_key(&dir);
        assert!(key.ends_with("신뢰"), "NFC key: {key:?}");
        assert_eq!(key, key.nfc().collect::<String>());
        #[cfg(unix)]
        {
            let real = std::fs::canonicalize(&base).unwrap();
            assert!(key.starts_with(&*real.to_string_lossy().nfc().collect::<String>()));
        }
        let _ = std::fs::remove_dir_all(&base);
    }
}
