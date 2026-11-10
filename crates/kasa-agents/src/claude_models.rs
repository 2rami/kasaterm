//! Claude Code 가 고를 수 있는 모델 목록. 카사텀이 목록을 고정해 두면 새 모델(Fable 5.2)이 나와도
//! 설정·학생 드롭다운에 안 뜬다(2026-11-10) — CLI 의 SDK 초기화 응답(`models`)을 그대로 받아 쓴다.
//!
//! `claude -p --bare` 로 묻는다. 토큰을 쓰지 않고 1초 남짓이다. `--bare` 는 훅·플러그인·CLAUDE.md 를
//! 건너뛰되 사용자 설정은 읽어서, 사용자가 고른 모델(settings 의 `model`)도 CLI 가 목록에 덧붙인다.
//! 설정 소스를 아예 끄면(`--setting-sources ""`) 그 모델이 빠진다(실측: CLI 2.1.322 의 내장 목록은
//! Fable 5.1 까지였고 5.2 는 사용자 설정으로만 왔다).

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

/// 이보다 오래된 목록은 다음에 읽을 때 뒤에서 새로 받는다.
const STALE_AFTER: Duration = Duration::from_secs(6 * 60 * 60);
const QUERY_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeModel {
    /// `--model` 에 넘길 값(`opus`·`claude-fable-5-2`·`claude-fable-5-1[1m]`).
    pub value: String,
    /// 화면 이름(`Opus 5.5`·`Fable 5.2`·`Fable 5.1 1M`).
    pub label: String,
}

/// 초기화 응답에서 모델을 꺼낸다. `default` 는 뺀다 — 카사텀의 「기본」(`--model` 안 붙임)과 같은 뜻이다.
pub fn parse_models(response: &Value) -> Vec<ClaudeModel> {
    let mut out: Vec<ClaudeModel> = Vec::new();
    for m in response["models"].as_array().into_iter().flatten() {
        let Some(value) = m["value"].as_str().map(str::trim).filter(|v| !v.is_empty() && *v != "default") else {
            continue;
        };
        if out.iter().any(|o| o.value == value) {
            continue;
        }
        out.push(ClaudeModel { value: value.to_owned(), label: label_of(m, value) });
    }
    out
}

/// 설명 앞머리(「Opus 5.5 · Best for …」)가 판 번호를 담는다. `displayName` 은 판이 없을 때가 있다(「Fable」).
fn label_of(m: &Value, value: &str) -> String {
    let head = m["description"].as_str().and_then(|d| d.split(" · ").next()).map(str::trim).unwrap_or_default();
    let mut label = if head.is_empty() || head.len() > 24 {
        m["displayName"].as_str().unwrap_or(value).trim().to_owned()
    } else {
        head.to_owned()
    };
    if value.ends_with("[1m]") && !label.contains("1M") {
        label.push_str(" 1M");
    }
    label
}

/// CLI 에 바로 묻는다. 막히면 `QUERY_TIMEOUT` 뒤 끄고 `None`.
pub fn query(claude: &Path) -> Option<Vec<ClaudeModel>> {
    let mut child = kasa_socket::process::no_window_command(claude)
        .args(["-p", "--bare", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let request = json!({ "type": "control_request", "request_id": "kasaterm-models", "request": { "subtype": "initialize" } });
    if let Some(mut stdin) = child.stdin.take() {
        // 받고 나면 stdin 이 닫혀 CLI 가 스스로 끝난다.
        let _ = writeln!(stdin, "{request}");
    }
    let stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { return };
            let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
            if msg["type"] == "control_response" {
                let _ = tx.send(msg["response"]["response"].clone());
                return;
            }
        }
    });
    let response = rx.recv_timeout(QUERY_TIMEOUT).ok();
    let _ = child.kill();
    let _ = child.wait();
    let models = parse_models(&response?);
    (!models.is_empty()).then_some(models)
}

fn cache_path() -> Option<PathBuf> {
    Some(kasa_socket::home_dir()?.join(".config/kasaterm/claude-models.json"))
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or_default()
}

fn read_cache(path: &Path) -> Option<(Vec<ClaudeModel>, u64)> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let models: Vec<ClaudeModel> = v["models"]
        .as_array()?
        .iter()
        .filter_map(|m| Some(ClaudeModel { value: m["value"].as_str()?.to_owned(), label: m["label"].as_str()?.to_owned() }))
        .collect();
    Some((models, v["fetched_at"].as_u64().unwrap_or_default()))
}

fn write_cache(path: &Path, models: &[ClaudeModel]) {
    let body = json!({
        "fetched_at": now_secs(),
        "models": models.iter().map(|m| json!({ "value": m.value, "label": m.label })).collect::<Vec<_>>(),
    });
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, body.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// 지금 아는 목록. 받은 적이 없거나 오래됐으면 뒤에서 한 번 새로 받는다 — 화면은 다음 그리기에 새 목록을
/// 본다. 받은 적이 없고 받기 전이면 빈 목록이니 부르는 쪽이 옛 고정 목록으로 메운다.
pub fn choices() -> Vec<ClaudeModel> {
    let Some(path) = cache_path() else { return Vec::new() };
    let cached = read_cache(&path);
    let stale = cached.as_ref().is_none_or(|(_, at)| now_secs().saturating_sub(*at) > STALE_AFTER.as_secs());
    if stale {
        refresh_in_background(path);
    }
    cached.map(|(models, _)| models).unwrap_or_default()
}

/// 설정의 Claude 모델 줄 — 맨 앞 「기본」(`--model` 안 붙임), 다음 CLI 목록. 받은 적이 없으면 별칭 셋으로 메운다.
/// 지금 고른 값이 목록에 없으면(옛 판·손으로 쓴 값) 끝에 붙여, 선택이 화면에서 사라지지 않게 한다.
pub fn setting_choices(current: &str) -> Vec<ClaudeModel> {
    with_default_and_current(choices(), current)
}

fn with_default_and_current(models: Vec<ClaudeModel>, current: &str) -> Vec<ClaudeModel> {
    let model = |value: &str, label: &str| ClaudeModel { value: value.to_owned(), label: label.to_owned() };
    let mut out = vec![model("", "기본")];
    if models.is_empty() {
        out.extend([model("opus", "Opus"), model("sonnet", "Sonnet"), model("haiku", "Haiku")]);
    } else {
        out.extend(models);
    }
    let current = current.trim();
    if !out.iter().any(|m| m.value == current) {
        out.push(model(current, current));
    }
    out
}

fn refresh_in_background(path: PathBuf) {
    static RUNNING: AtomicBool = AtomicBool::new(false);
    static LAST_TRY: Mutex<Option<Instant>> = Mutex::new(None);
    {
        // CLI 가 없거나 계속 실패해도 그리기마다 띄우지 않게 한 시간에 한 번만 시도한다.
        let mut last = LAST_TRY.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|at| at.elapsed() < Duration::from_secs(60 * 60)) {
            return;
        }
        *last = Some(Instant::now());
    }
    if RUNNING.swap(true, Ordering::AcqRel) {
        return;
    }
    std::thread::spawn(move || {
        if let Some(models) = query(&crate::runtime::claude_bin()) {
            write_cache(&path, &models);
        }
        RUNNING.store(false, Ordering::Release);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cli_response() -> Value {
        json!({ "models": [
            { "value": "default", "displayName": "Default (recommended)", "description": "Opus 5.5 · Best for everyday, complex tasks" },
            { "value": "opus", "displayName": "Opus", "description": "Opus 5.5 · Best for everyday, complex tasks" },
            { "value": "claude-fable-5-1[1m]", "displayName": "Fable", "description": "Fable 5.1 · Most capable for your hardest and longest-running tasks" },
            { "value": "claude-fable-5-2", "displayName": "Fable 5.2", "description": "Fable 5.2 · Most capable for your hardest and longest-running tasks" },
            { "value": "sonnet", "displayName": "Sonnet", "description": "Sonnet 5.5 · Efficient for routine tasks" },
            { "value": "haiku", "displayName": "Haiku", "description": "Haiku 5.5 · Fastest for quick answers" },
            { "value": "opus", "displayName": "Opus", "description": "duplicate" },
            { "value": "custom-x", "displayName": "Custom X" }
        ]})
    }

    #[test]
    fn models_come_with_versioned_labels_and_without_default() {
        let got: Vec<(String, String)> = parse_models(&cli_response()).into_iter().map(|m| (m.value, m.label)).collect();
        let want = [
            ("opus", "Opus 5.5"),
            ("claude-fable-5-1[1m]", "Fable 5.1 1M"),
            ("claude-fable-5-2", "Fable 5.2"),
            ("sonnet", "Sonnet 5.5"),
            ("haiku", "Haiku 5.5"),
            ("custom-x", "Custom X"),
        ];
        assert_eq!(got, want.map(|(v, l)| (v.to_owned(), l.to_owned())));
        assert!(parse_models(&json!({})).is_empty());
    }

    #[test]
    fn setting_row_starts_with_default_and_keeps_the_current_pick() {
        let values = |v: Vec<ClaudeModel>| v.into_iter().map(|m| m.value).collect::<Vec<_>>();
        assert_eq!(values(with_default_and_current(Vec::new(), "")), ["", "opus", "sonnet", "haiku"]);
        let cli = parse_models(&cli_response());
        assert_eq!(
            values(with_default_and_current(cli.clone(), "claude-fable-5-2")),
            ["", "opus", "claude-fable-5-1[1m]", "claude-fable-5-2", "sonnet", "haiku", "custom-x"]
        );
        let kept = with_default_and_current(cli, "claude-opus-5[1m]");
        assert_eq!(kept.last().map(|m| (m.value.as_str(), m.label.as_str())), Some(("claude-opus-5[1m]", "claude-opus-5[1m]")));
    }

    /// 실제 Claude CLI 에 묻는다 — `cargo test -p kasa-agents live_cli -- --ignored`.
    #[test]
    #[ignore = "needs a logged-in claude CLI"]
    fn live_cli_lists_models() {
        let models = query(&crate::runtime::claude_bin()).expect("the CLI answered");
        assert!(models.iter().any(|m| m.value == "opus"), "{models:?}");
        println!("{models:?}");
    }

    #[test]
    fn cache_round_trips() {
        let dir = std::env::temp_dir().join(format!("kasa-claude-models-{}", std::process::id()));
        let path = dir.join("claude-models.json");
        let models = parse_models(&cli_response());
        write_cache(&path, &models);
        let (read, at) = read_cache(&path).expect("cache");
        assert_eq!(read, models);
        assert!(now_secs().saturating_sub(at) < 60);
        std::fs::remove_dir_all(dir).ok();
    }
}
