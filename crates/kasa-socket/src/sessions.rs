//! Offline Claude-session discovery from the transcript jsonl files under
//! `~/.claude/projects/<encoded-cwd>/`. Pure filesystem + serde_json — no live
//! pane, no GUI state — so both the GUI backend (PtyBackend, app/kasaterm) and
//! the standalone web server (StandaloneBackend, kasa-mcp) share one impl.
//!
//! Lifted out of app/kasaterm/src/socket.rs so the standalone `serve-web` bin
//! can list/read sessions without depending on the winit/wgpu GUI crate.

use std::path::{Path, PathBuf};

/// claude session ids are canonical UUIDs (8-4-4-4-12 hex). Validating guards
/// against grabbing a non-id token after a bare `-r`/`--resume` (the picker).
pub fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, &c)| match i {
            8 | 13 | 18 | 23 => c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// `~/.claude/projects/<encoded-cwd>/` — the transcript dir claude writes a
/// `cwd`'s session jsonls into. Encoding matches claude code: every `/` and `.`
/// in the absolute cwd becomes `-`. None when `$HOME` is unset.
pub fn claude_project_dir(cwd: &Path) -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let encoded = cwd.to_string_lossy().replace(['/', '.'], "-");
    Some(PathBuf::from(home).join(".claude/projects").join(encoded))
}

/// Full jsonl path for session `id` (a uuid) under `cwd`. Used both to list
/// recent sessions and to read one offline by uuid (no live pane needed).
pub fn session_jsonl_path(cwd: &Path, id: &str) -> Option<PathBuf> {
    Some(claude_project_dir(cwd)?.join(format!("{id}.jsonl")))
}

/// 최근 claude 세션 목록(`claude --resume` 후보) — `cwd` 의 projects 디렉터리에서
/// 모든 .jsonl 을 mtime 내림차순으로 모아 상위 `limit` 개만 라벨까지 파싱한다.
/// 287개씩 쌓인 디렉터리도 라벨 파싱은 최신 N개에만 들어 비용이 작다.
pub fn recent_sessions_for(cwd: &Path, limit: usize) -> Vec<RecentSession> {
    let Some(dir) = claude_project_dir(cwd) else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut files: Vec<(std::time::SystemTime, std::path::PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("jsonl") {
                return None;
            }
            let mtime = e.metadata().and_then(|m| m.modified()).ok()?;
            Some((mtime, p))
        })
        .collect();
    files.sort_by(|a, b| b.0.cmp(&a.0)); // 최신 먼저
    let cwd_str = cwd.to_string_lossy().into_owned();
    files
        .into_iter()
        .take(limit)
        .filter_map(|(mtime, path)| {
            let id = path.file_stem()?.to_str()?.to_string();
            // uuid 형식이 아닌 파일(예: 손상·임시)은 resume 대상이 아님.
            if !is_uuid(&id) {
                return None;
            }
            let mtime_secs = mtime
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let label = parse_session_label(&path)
                .unwrap_or_else(|| id.chars().take(8).collect());
            Some(RecentSession { id, label, mtime: mtime_secs, cwd: cwd_str.clone() })
        })
        .collect()
}

/// transcript jsonl 에서 사람이 읽을 라벨을 뽑는다 — claude 가 붙인 `aiTitle`
/// 우선, 없으면 첫 user 텍스트 메시지(앞 80자), 둘 다 없으면 None(호출부가 short
/// id 폴백). summary 라인은 최근 세션엔 거의 없어 안 쓴다(거노 실측). 큰 파일
/// 방어로 앞 600줄만 스캔한다.
fn parse_session_label(path: &Path) -> Option<String> {
    use std::io::BufRead;
    let f = std::fs::File::open(path).ok()?;
    let reader = std::io::BufReader::new(f);
    let mut first_user: Option<String> = None;
    for line in reader.lines().take(600).map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("ai-title") => {
                if let Some(t) = v.get("aiTitle").and_then(|t| t.as_str()) {
                    let t = t.trim();
                    if !t.is_empty() {
                        return Some(t.chars().take(80).collect());
                    }
                }
            }
            Some("user") if first_user.is_none() => {
                if v.get("isMeta").and_then(|m| m.as_bool()).unwrap_or(false) {
                    continue;
                }
                if let Some(txt) = user_message_text(&v) {
                    let txt = txt.trim();
                    // slash command(<command-name>)·시스템 주입(Caveat/<system-reminder>)
                    // 같은 메타성 첫 메시지는 라벨로 부적합 — 건너뛰고 다음 진짜 발화를
                    // 찾는다(first_user 가 None 이라 계속 스캔). 안 그러면 "<command-name>
                    // /effort…" 가 라벨로 샌다(거노 실측).
                    if !txt.is_empty() && !is_meta_user_text(txt) {
                        first_user = Some(txt.chars().take(80).collect());
                    }
                }
            }
            _ => {}
        }
    }
    first_user
}

/// 라벨로 부적합한 메타성 user 텍스트(슬래시 명령·시스템 주입·bash 출력 래퍼).
/// claude 가 첫 턴에 흔히 끼워넣어 라벨을 오염시키므로 건너뛴다.
fn is_meta_user_text(t: &str) -> bool {
    t.starts_with("<command-")
        || t.starts_with("<local-command")
        || t.starts_with("<system-reminder")
        || t.starts_with("<bash-")
        || t.starts_with("Caveat:")
}

/// user transcript 라인의 본문 텍스트 — content 가 문자열이면 그대로, 블록 배열이면
/// 첫 text 블록. tool_result 등 비텍스트는 건너뛴다.
fn user_message_text(v: &serde_json::Value) -> Option<String> {
    let content = v.get("message")?.get("content")?;
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }
    for block in content.as_array()? {
        if block.get("type").and_then(|t| t.as_str()) == Some("text") {
            if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                return Some(t.to_string());
            }
        }
    }
    None
}

use crate::backend::RecentSession;
