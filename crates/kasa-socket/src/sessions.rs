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
        // "세션제목생성" = claude 내부 제목생성(title-gen) 서브세션이 자기 세션에
        // 다는 마커 제목. 이 세션의 첫 user 는 "아래 대화의 주제를…" 메타프롬프트라
        // 폴백도 오염된다 — 라벨 자체를 포기해 인레이/피커에서 지운다.
        if t == "세션제목생성" {
            return None;
        }
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

/// jsonl 꼬리에서 가장 마지막 `custom-title` 레코드의 제목. 여러 번 rename 하면
/// 마지막 것이 이긴다(claude `/rename` 동일 규칙). 에이전트 세션은 한 턴에 툴
/// 결과가 수백 KB 씩 append 돼 마지막 스탬프가 금세 꼬리 64KB 밖으로 밀린다
/// (실측 594KB — title-sync 가 스탬프를 박아도 제목이 옛것으로 보이던 원인).
/// 그래서 64KB 청크를 뒤에서 앞으로 역스캔하고, 청크 경계에 걸친 레코드는
/// 겹침 4KB 로 잡는다. 발견 즉시 중단이라 정상 세션(스탬프가 꼬리 근처)은
/// 종전과 같은 1청크 비용, 스탬프가 전무한 세션만 상한 8MB 까지 읽고 폴백한다.
fn last_custom_title(path: &Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    const CHUNK: u64 = 64 * 1024;
    const OVERLAP: u64 = 4 * 1024;
    const MAX_SCAN: u64 = 8 * 1024 * 1024;
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let floor = len.saturating_sub(MAX_SCAN);
    let mut end = len;
    while end > floor {
        let start = end.saturating_sub(CHUNK).max(floor);
        let read_end = (end + OVERLAP).min(len);
        f.seek(SeekFrom::Start(start)).ok()?;
        let mut buf = vec![0u8; (read_end - start) as usize];
        f.read_exact(&mut buf).ok()?;
        // seek 이 줄/멀티바이트 중간에 떨어질 수 있어 lossy + 첫 부분줄 스킵.
        // 스킵으로 놓친 경계 레코드는 다음(더 앞) 청크가 겹침으로 다시 본다.
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
        if last.is_some() {
            return last;
        }
        end = start;
    }
    None
}

/// 라벨로 부적합한 메타성 user 텍스트(슬래시 명령·시스템 주입·bash 출력 래퍼).
/// claude 가 첫 턴에 흔히 끼워넣어 라벨을 오염시키므로 건너뛴다.
fn is_meta_user_text(t: &str) -> bool {
    t.starts_with("<command-")
        || t.starts_with("<local-command")
        || t.starts_with("<system-reminder")
        || t.starts_with("<bash-")
        || t.starts_with("Caveat:")
        // claude 내부 title-gen 서브세션의 첫 user 프롬프트 — custom-title 스탬프
        // 전 찰나에 이게 첫 user 폴백으로 새어 인레이에 유출됐다(거노 실측).
        || t.starts_with("아래 대화의 주제를 나타내는")
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp_jsonl(name: &str, body: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("kasa-sessions-test-{name}-{}.jsonl", std::process::id()));
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        p
    }

    fn title_rec(t: &str) -> String {
        format!(r#"{{"type": "custom-title", "customTitle": "{t}", "nameSource": "auto"}}"#) + "\n"
    }

    // 실사고 재현: 스탬프 뒤에 대형 에이전트 턴(수백 KB 툴 결과)이 쌓여
    // 마지막 custom-title 이 꼬리 64KB 밖으로 밀려도 역스캔이 찾아야 한다.
    #[test]
    fn stamp_beyond_64k_tail_is_found() {
        let filler_line = format!(r#"{{"type": "assistant", "pad": "{}"}}"#, "x".repeat(400)) + "\n";
        let mut body = title_rec("옛 제목");
        body.push_str(&title_rec("최신 제목"));
        for _ in 0..1600 {
            body.push_str(&filler_line); // ~650KB — 종전 64KB 창을 한참 벗어난다
        }
        let p = tmp_jsonl("beyond64k", &body);
        assert_eq!(last_custom_title(&p).as_deref(), Some("최신 제목"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn last_stamp_wins_within_tail() {
        let mut body = title_rec("첫 제목");
        body.push_str(&title_rec("마지막 제목"));
        let p = tmp_jsonl("lastwins", &body);
        assert_eq!(last_custom_title(&p).as_deref(), Some("마지막 제목"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn no_stamp_returns_none() {
        let filler_line = format!(r#"{{"type": "assistant", "pad": "{}"}}"#, "y".repeat(400)) + "\n";
        let p = tmp_jsonl("nostamp", &filler_line.repeat(300));
        assert_eq!(last_custom_title(&p), None);
        let _ = std::fs::remove_file(&p);
    }

    fn user_rec(t: &str) -> String {
        format!(r#"{{"type": "user", "message": {{"content": "{t}"}}}}"#) + "\n"
    }

    // title-gen 세션(custom-title="세션제목생성")은 라벨을 포기한다 — 스탬프가
    // 있어도 첫 user(메타프롬프트)로 폴백하지 않고 None.
    #[test]
    fn titlegen_marker_yields_no_label() {
        let mut body = user_rec("아래 대화의 주제를 나타내는 한국어 제목을 딱 하나 출력해");
        body.push_str(&title_rec("세션제목생성"));
        let p = tmp_jsonl("titlegenmarker", &body);
        assert_eq!(parse_session_label(&p), None);
        let _ = std::fs::remove_file(&p);
    }

    // custom-title 스탬프 전 찰나: 첫 user 가 메타프롬프트뿐이면 라벨 없음
    // (is_meta_user_text 가 걸러 first_user 가 안 잡힌다).
    #[test]
    fn titlegen_metaprompt_first_user_skipped() {
        let body = user_rec("아래 대화의 주제를 나타내는 한국어 제목을 딱 하나 출력해");
        let p = tmp_jsonl("metaonly", &body);
        assert_eq!(parse_session_label(&p), None);
        let _ = std::fs::remove_file(&p);
    }

    // 회귀 방지: 정상 세션 제목·첫 user 폴백은 그대로 살아있어야 한다.
    #[test]
    fn normal_session_label_unaffected() {
        let mut custom = user_rec("간단히 인사만 해줘");
        custom.push_str(&title_rec("한 문장 인사"));
        let p1 = tmp_jsonl("normalcustom", &custom);
        assert_eq!(parse_session_label(&p1).as_deref(), Some("한 문장 인사"));
        let _ = std::fs::remove_file(&p1);

        let p2 = tmp_jsonl("normaluser", &user_rec("파일트리 버그 고쳐줘"));
        assert_eq!(parse_session_label(&p2).as_deref(), Some("파일트리 버그 고쳐줘"));
        let _ = std::fs::remove_file(&p2);
    }
}
