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

/// transcript jsonl 에서 사람이 읽을 라벨을 뽑는다 — `/rename` 이 남기는
/// `custom-title`(사용자 지정, 마지막 것 우선) 최우선, 다음 claude 가 붙인
/// `aiTitle`, 없으면 첫 user 텍스트 메시지(앞 80자), 전부 없으면 None(호출부가
/// short id 폴백). summary 라인은 최근 세션엔 거의 없어 안 쓴다(거노 실측).
/// 큰 파일 방어로 앞 600줄만 스캔한다. custom-title 은 rename 시점에 파일
/// 말미로 append 되므로 앞 스캔으론 못 보고 꼬리 64KB 역스캔으로 잡는다 —
/// teammate 세션은 claude `/rename` 이 막혀 있어 외부 append 가 유일한
/// 개명 경로라 이 우회를 피커가 반드시 읽어줘야 한다.
fn parse_session_label(path: &Path) -> Option<String> {
    use std::io::BufRead;
    if let Some(t) = last_custom_title(path) {
        return Some(t);
    }
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

/// 피커 라벨 규칙(custom-title > aiTitle > 첫 user)을 transcript 경로에 직접
/// 적용하는 pub 래퍼 — GUI 입력박스 제목 인레이(render.rs) 등 목록 밖에서
/// 한 세션의 라벨만 필요할 때 쓴다.
pub fn session_label_for(path: &Path) -> Option<String> {
    parse_session_label(path)
}

/// jsonl 꼬리 64KB 에서 가장 마지막 `custom-title` 레코드의 제목. 여러 번
/// rename 하면 마지막 것이 이긴다(claude `/rename` 동일 규칙). 파일이 64KB 를
/// 넘고 rename 이후 대화가 그만큼 더 쌓인 극단 케이스만 놓치는데, 그땐
/// ai-title 폴백이라 라벨이 비지는 않는다.
fn last_custom_title(path: &Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(64 * 1024);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    // seek 이 줄/멀티바이트 중간에 떨어질 수 있어 lossy + 첫 부분줄 스킵.
    let text = String::from_utf8_lossy(&buf);
    let mut last: Option<String> = None;
    for line in text.lines().skip(usize::from(start > 0)) {
        if !line.contains("\"custom-title\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if v.get("type").and_then(|t| t.as_str()) != Some("custom-title") {
            continue;
        }
        if let Some(t) = v.get("customTitle").and_then(|t| t.as_str()) {
            let t = t.trim();
            if !t.is_empty() {
                last = Some(t.chars().take(80).collect());
            }
        }
    }
    last
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

/// board 용 — 세션 jsonl 에서 (title, 마지막 user 발화). title 은 parse_session_label
/// 과 동일(aiTitle→첫 user), last_prompt 는 가장 최근 비메타 user 텍스트. 라이브 pane 이
/// 없는 standalone 이 background 세션들의 현황을 board 로 만들 때 쓴다.
pub fn session_board_meta(cwd: &Path, id: &str) -> Option<(String, String)> {
    let path = session_jsonl_path(cwd, id)?;
    let title = parse_session_label(&path).unwrap_or_default();
    let last_prompt = last_user_text(&path).unwrap_or_default();
    Some((title, last_prompt))
}

/// 세션 jsonl 의 가장 최근 비메타 user 발화(앞 200자).
fn last_user_text(path: &Path) -> Option<String> {
    use std::io::BufRead;
    let f = std::fs::File::open(path).ok()?;
    let mut last: Option<String> = None;
    for line in std::io::BufReader::new(f).lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        if v.get("type").and_then(|t| t.as_str()) != Some("user") {
            continue;
        }
        if v.get("isMeta").and_then(|m| m.as_bool()).unwrap_or(false) {
            continue;
        }
        if let Some(t) = user_message_text(&v) {
            let t = t.trim();
            if !t.is_empty() && !is_meta_user_text(t) {
                last = Some(t.chars().take(200).collect());
            }
        }
    }
    last
}

/// peek 용 — 세션 jsonl 마지막 `turns` 개 user/assistant 텍스트를 사람이 읽을 형태로.
/// 라이브 pane 화면이 없는 standalone 에서 background 세션 '엿보기'를 대신한다.
pub fn transcript_tail_text(cwd: &Path, id: &str, turns: usize) -> Option<String> {
    let path = session_jsonl_path(cwd, id)?;
    use std::io::BufRead;
    let f = std::fs::File::open(&path).ok()?;
    let mut msgs: Vec<String> = Vec::new();
    for line in std::io::BufReader::new(f).lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("user") => {
                if v.get("isMeta").and_then(|m| m.as_bool()).unwrap_or(false) {
                    continue;
                }
                if let Some(t) = user_message_text(&v) {
                    let t = t.trim();
                    if !t.is_empty() && !is_meta_user_text(t) {
                        msgs.push(format!("[사용자] {}", t.chars().take(500).collect::<String>()));
                    }
                }
            }
            Some("assistant") => {
                if let Some(t) = assistant_message_text(&v) {
                    let t = t.trim();
                    if !t.is_empty() {
                        msgs.push(format!("[claude] {}", t.chars().take(500).collect::<String>()));
                    }
                }
            }
            _ => {}
        }
    }
    let start = msgs.len().saturating_sub(turns.max(1));
    Some(msgs[start..].join("\n\n"))
}

/// assistant transcript 라인의 본문 텍스트 — content 블록 배열의 text 블록들을 이어붙인다.
fn assistant_message_text(v: &serde_json::Value) -> Option<String> {
    let content = v.get("message")?.get("content")?;
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }
    let mut out = String::new();
    for block in content.as_array()? {
        if block.get("type").and_then(|t| t.as_str()) == Some("text") {
            if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(t);
            }
        }
    }
    (!out.is_empty()).then_some(out)
}

use crate::backend::RecentSession;
