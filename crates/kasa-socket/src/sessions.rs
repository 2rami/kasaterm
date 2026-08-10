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
            let label = parse_session_label(&path, true)
                .unwrap_or_else(|| id.chars().take(8).collect());
            Some(RecentSession {
                harness: "claude".into(),
                id,
                label,
                mtime: mtime_secs,
                cwd: cwd_str.clone(),
            })
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
fn parse_session_label(path: &Path, allow_custom: bool) -> Option<String> {
    use std::io::BufRead;
    // title-gen junk 세션(title-sync 가 haiku 제목 생성용으로 스폰하는 `claude -p`
    // 의 cwd=kasaterm-title-gen)은 사용자 세션이 아니다 — 첫 user 가 "다음 대화
    // 발췌를 보고…" 메타프롬프트고 assistant 는 빈 발췌 거부문("대화 발췌가 제공되지
    // 않았어요…")이라 라벨·custom-title 이 전부 오염된다. 경로로 통째 제외해
    // 인레이/피커에 절대 안 뜨게 한다(거노 실측).
    if path.to_str().is_some_and(|s| s.contains("kasaterm-title-gen")) {
        return None;
    }
    if let Some(t) = last_custom_title(path) {
        // "세션제목생성" = claude 내부 제목생성(title-gen) 서브세션이 자기 세션에
        // 다는 마커 제목. 이 세션의 첫 user 는 "아래 대화의 주제를…" 메타프롬프트라
        // 폴백도 오염된다 — 라벨 자체를 포기해 인레이/피커에서 지운다. 이 가드는
        // `allow_custom` 과 무관하게 걸어야 한다(오염 판정이지 라벨 선택이 아니다).
        if t == "세션제목생성" {
            return None;
        }
        if allow_custom {
            return Some(t);
        }
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
/// 적용하는 pub 래퍼 — 목록 밖에서 한 세션의 라벨만 필요할 때 쓴다.
pub fn session_label_for(path: &Path) -> Option<String> {
    parse_session_label(path, true)
}

/// 같은 규칙에서 **사용자가 `/rename` 으로 붙인 이름만 뺀** 것(aiTitle > 첫
/// user) — 입력박스 **좌측** 제목 인레이 전용이다. claude 가 그 rename 이름을
/// 입력박스 **우측**에 이미 그리고 있어서, 좌측까지 custom-title 을 쓰면 한 줄에
/// 똑같은 이름이 두 번 서고 "이 pane 이 무슨 작업 중인지"가 사라진다
/// (거노 2026-07-30: "리네임하면 좌측은 세션이름요약, 우측칩은 리네임한 이름").
pub fn session_summary_for(path: &Path) -> Option<String> {
    parse_session_label(path, false)
}

/// `/rename` 으로 붙인 이름 **하나만** — 없으면 None. 입력박스 **우측** 인레이 전용.
///
/// `session_label_for` 의 폴백 사슬(custom-title > aiTitle > 첫 user)을 안 쓰는 게
/// 핵심이다. 폴백을 쓰면 rename 하지 않은 pane 의 우측에 aiTitle 이 뜨는데, 좌측
/// 요약이 이미 그걸 보여 주고 있어 같은 이름이 한 줄에 두 번 선다 — 좌우를 나눈
/// 이유가 사라진다(거노 2026-07-30: "리네임하면 좌측은 세션이름요약, 우측칩은
/// 리네임한 이름"). 리네임 안 했으면 우측은 비는 게 맞다.
pub fn session_rename_for(path: &Path) -> Option<String> {
    last_custom_title(path)
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
        || t.starts_with("다음 대화 발췌를 보고")
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
    let title = parse_session_label(&path, true).unwrap_or_default();
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

    fn ai_title_rec(t: &str) -> String {
        format!(r#"{{"type": "ai-title", "aiTitle": "{t}"}}"#) + "\n"
    }

    // title-gen 세션(custom-title="세션제목생성")은 라벨을 포기한다 — 스탬프가
    // 있어도 첫 user(메타프롬프트)로 폴백하지 않고 None.
    #[test]
    fn titlegen_marker_yields_no_label() {
        let mut body = user_rec("아래 대화의 주제를 나타내는 한국어 제목을 딱 하나 출력해");
        body.push_str(&title_rec("세션제목생성"));
        let p = tmp_jsonl("titlegenmarker", &body);
        assert_eq!(parse_session_label(&p, true), None);
        let _ = std::fs::remove_file(&p);
    }

    // custom-title 스탬프 전 찰나: 첫 user 가 메타프롬프트뿐이면 라벨 없음
    // (is_meta_user_text 가 걸러 first_user 가 안 잡힌다).
    #[test]
    fn titlegen_metaprompt_first_user_skipped() {
        let body = user_rec("아래 대화의 주제를 나타내는 한국어 제목을 딱 하나 출력해");
        let p = tmp_jsonl("metaonly", &body);
        assert_eq!(parse_session_label(&p, true), None);
        let _ = std::fs::remove_file(&p);
    }

    // 회귀 방지: 정상 세션 제목·첫 user 폴백은 그대로 살아있어야 한다.
    #[test]
    fn normal_session_label_unaffected() {
        let mut custom = user_rec("간단히 인사만 해줘");
        custom.push_str(&title_rec("한 문장 인사"));
        let p1 = tmp_jsonl("normalcustom", &custom);
        assert_eq!(parse_session_label(&p1, true).as_deref(), Some("한 문장 인사"));
        let _ = std::fs::remove_file(&p1);

        let p2 = tmp_jsonl("normaluser", &user_rec("파일트리 버그 고쳐줘"));
        assert_eq!(parse_session_label(&p2, true).as_deref(), Some("파일트리 버그 고쳐줘"));
        let _ = std::fs::remove_file(&p2);
    }

    /// 입력박스 좌측 인레이(`session_summary_for`)는 `/rename` 이름을 건너뛰고
    /// 요약으로 떨어져야 한다 — 우측에 claude 가 그 이름을 이미 그리므로.
    #[test]
    fn summary_skips_the_rename_but_label_keeps_it() {
        let mut body = user_rec("파일 열기를 GUI 앱으로 바꾸자");
        body.push_str(&ai_title_rec("파일 열기 방식 설정"));
        body.push_str(&title_rec("kasa"));
        let p = tmp_jsonl("renamed", &body);
        assert_eq!(session_label_for(&p).as_deref(), Some("kasa"));
        assert_eq!(session_summary_for(&p).as_deref(), Some("파일 열기 방식 설정"));
        let _ = std::fs::remove_file(&p);
    }

    /// aiTitle 조차 없으면 첫 user 로 떨어진다(rename 값으로 되돌아가지 않는다).
    #[test]
    fn summary_falls_back_to_first_user_not_the_rename() {
        let mut body = user_rec("파일트리 버그 고쳐줘");
        body.push_str(&title_rec("kasa"));
        let p = tmp_jsonl("renamednoai", &body);
        assert_eq!(session_summary_for(&p).as_deref(), Some("파일트리 버그 고쳐줘"));
        let _ = std::fs::remove_file(&p);
    }

    /// title-gen 오염 가드는 `allow_custom` 과 무관하게 걸려야 한다.
    #[test]
    fn summary_still_drops_titlegen_marker() {
        let mut body = user_rec("아래 대화의 주제를 나타내는 한국어 제목을 딱 하나 출력해");
        body.push_str(&title_rec("세션제목생성"));
        let p = tmp_jsonl("titlegensummary", &body);
        assert_eq!(session_summary_for(&p), None);
        let _ = std::fs::remove_file(&p);
    }
}

// ---------------------------------------------------------- 하네스 통합 --
//
// claude 말고도 codex·agy 가 각자 다른 곳에 다른 모양으로 대화를 쌓는다.
// 셋을 한 목록으로 보려면 저장 방식의 차이를 여기서 흡수해야 한다:
//
//   claude  ~/.claude/projects/<cwd-slug>/<uuid>.jsonl   cwd 별로 디렉터리가 갈린다
//   codex   ~/.codex/sessions/rollout-<날짜>-<uuid>.jsonl 한 디렉터리에 평평하게
//   agy     ~/.gemini/antigravity-cli/conversation_summaries.db (SQLite)
//
// agy 가 오히려 제일 싸다 — 제목·미리보기·시각·워크스페이스가 한 테이블에 이미
// 정리돼 있어 쿼리 한 번이면 끝난다. 대화 본문(`conversations/<uuid>.db`)은
// protobuf BLOB 이라 여기서 건드리지 않는다.

/// 공백을 한 칸으로 합치고 `n` **문자**까지 자른다.
/// `String::truncate` 를 쓰면 안 된다 — 그쪽은 바이트 오프셋이라 한글 한 글자
/// 가운데를 끊는 순간 panic 한다(실측: 라벨에 한글이 있으면 바로 터졌다).
trait TakeChars {
    fn take_chars(self, n: usize) -> String;
}
impl TakeChars for Vec<&str> {
    fn take_chars(self, n: usize) -> String {
        self.join(" ").chars().take(n).collect()
    }
}

/// mtime(초)으로 환산. 못 읽으면 0 — 정렬에서 맨 뒤로 밀린다.
fn mtime_secs(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// jsonl 앞부분에서 `cwd` 를 찾는다. claude 는 세션 초반(대략 3번째 줄)에 한 번
/// 적어 두므로 앞 40줄만 본다 — 디렉터리 이름(slug)에서 되돌리는 방법은 경로에
/// 원래 있던 `-` 와 구분자 `-` 를 못 갈라 실패한다.
fn jsonl_cwd(path: &Path, max_lines: usize) -> Option<String> {
    use std::io::BufRead;
    let f = std::fs::File::open(path).ok()?;
    for line in std::io::BufReader::new(f).lines().take(max_lines).map_while(Result::ok) {
        let v: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(c) = v.get("cwd").and_then(|c| c.as_str()) {
            return Some(c.to_string());
        }
    }
    None
}

/// 모든 프로젝트를 가로지르는 최근 claude 세션. `recent_sessions_for` 는 한 cwd
/// 안만 보므로 "이 프로젝트" 목록에 맞고, 통합 피커에는 이쪽이 필요하다.
pub fn recent_claude_sessions_all(limit: usize) -> Vec<RecentSession> {
    let Some(home) = std::env::var_os("HOME") else { return Vec::new() };
    let root = PathBuf::from(home).join(".claude/projects");
    let Ok(projects) = std::fs::read_dir(&root) else { return Vec::new() };

    let mut files: Vec<(u64, PathBuf)> = Vec::new();
    for proj in projects.flatten() {
        let Ok(entries) = std::fs::read_dir(proj.path()) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(meta) = e.metadata() else { continue };
            files.push((mtime_secs(&meta), p));
        }
    }
    files.sort_by(|a, b| b.0.cmp(&a.0));
    files
        .into_iter()
        // 걸러낼 것(title-gen·라벨 없는 빈 세션)이 있어 넉넉히 걷는다.
        .take(limit.saturating_mul(4))
        .filter_map(|(mtime, path)| {
            let id = path.file_stem()?.to_str()?.to_string();
            if !is_uuid(&id) {
                return None;
            }
            // 라벨이 없으면 통째로 뺀다. `recent_sessions_for` 는 한 cwd 안만 보므로
            // uuid 폴백이 무해했지만, 전체를 훑는 이쪽은 claude 가 제목 생성용으로
            // 스폰하는 title-gen 세션까지 만난다 — parse_session_label 이 그걸
            // None 으로 돌려주는데 폴백을 걸면 목록 상단이 uuid 로 오염된다.
            let label = parse_session_label(&path, true)?;
            let cwd = jsonl_cwd(&path, 40).unwrap_or_default();
            Some(RecentSession { harness: "claude".into(), id, label, mtime, cwd })
        })
        .take(limit)
        .collect()
}

/// 최근 codex 세션. rollout jsonl 은 첫 줄이 `{id, timestamp, …}` 이고 그 뒤로
/// `{type:"message", role, content:[{text}]}` 가 이어진다. 라벨은 첫 user 메시지.
pub fn recent_codex_sessions(limit: usize) -> Vec<RecentSession> {
    use std::io::BufRead;
    let Some(home) = std::env::var_os("HOME") else { return Vec::new() };
    let dir = PathBuf::from(home).join(".codex/sessions");
    let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };

    let mut files: Vec<(u64, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("jsonl") {
                return None;
            }
            Some((mtime_secs(&e.metadata().ok()?), p))
        })
        .collect();
    files.sort_by(|a, b| b.0.cmp(&a.0));

    files
        .into_iter()
        // 빈 세션을 뒤에서 거르므로 넉넉히 걷는다 — limit 만큼만 열면 그중
        // 빈 것을 뺀 나머지가 limit 에 못 미친다.
        .take(limit.saturating_mul(4))
        .filter_map(|(mtime, path)| {
            let f = std::fs::File::open(&path).ok()?;
            let mut id = String::new();
            let mut label = String::new();
            // 앞 200줄이면 헤더와 첫 사용자 발화를 지나친다. 도구 호출이 길게
            // 이어지는 세션도 user 메시지는 대개 맨 앞에 있다.
            for line in std::io::BufReader::new(f).lines().take(200).map_while(Result::ok) {
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
                if id.is_empty() {
                    if let Some(x) = v.get("id").and_then(|x| x.as_str()) {
                        id = x.to_string();
                    }
                }
                if label.is_empty()
                    && v.get("type").and_then(|t| t.as_str()) == Some("message")
                    && v.get("role").and_then(|r| r.as_str()) == Some("user")
                {
                    let text = v
                        .get("content")
                        .and_then(|c| c.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|x| x.get("text").and_then(|t| t.as_str()))
                                .collect::<Vec<_>>()
                                .join(" ")
                        })
                        .unwrap_or_default();
                    // chars() 로 자른다 — String::truncate 는 바이트 오프셋이라
                    // 한글 중간을 끊으면 그 자리에서 panic 한다(실측).
                    label = text.split_whitespace().collect::<Vec<_>>().take_chars(120);
                }
                if !id.is_empty() && !label.is_empty() {
                    break;
                }
            }
            // 파일명(rollout-<날짜>-<uuid>)에서라도 id 를 건진다.
            if id.is_empty() {
                id = path.file_stem()?.to_str()?.rsplit_once('-').map(|(_, u)| u.to_string())?;
            }
            // 사용자 발화가 하나도 없는 세션은 뺀다 — codex 는 띄우기만 하고 아무
            // 말도 안 한 세션도 rollout 파일을 남기는데(헤더 한 줄뿐), 이어갈 게
            // 없는 항목이 목록 상단을 uuid 로 채워 진짜 대화를 밀어낸다.
            if label.is_empty() {
                return None;
            }
            Some(RecentSession { harness: "codex".into(), id, label, mtime, cwd: String::new() })
        })
        .take(limit)
        .collect()
}

/// 최근 agy 대화. 요약 테이블 한 번만 읽는다 — 본문 DB 들은 protobuf 라 안 연다.
///
/// SQLite 를 `sqlite3` 프로세스로 읽는 이유: 이 crate 에 SQLite 의존성을 들이면
/// 빌드가 무거워지는데, 얻는 건 쿼리 하나다. macOS 는 `sqlite3` 를 기본 탑재한다.
/// ⚠️Windows 엔 없으므로 그때는 rusqlite 로 옮겨야 한다(지금은 빈 목록).
pub fn recent_agy_sessions(limit: usize) -> Vec<RecentSession> {
    let Some(home) = std::env::var_os("HOME") else { return Vec::new() };
    let db = PathBuf::from(home).join(".gemini/antigravity-cli/conversation_summaries.db");
    if !db.is_file() {
        return Vec::new();
    }
    // 시각은 SQL 에서 unix 초로 바꿔 받는다 — datetime 문자열 형식을 Rust 쪽에서
    // 또 추측하지 않으려는 것이다.
    let sql = format!(
        "SELECT json_group_array(json_object('id',conversation_id,'title',title,\
         'preview',preview,'ts',CAST(strftime('%s',last_modified_time) AS INTEGER),\
         'ws',workspace_uris)) FROM (SELECT * FROM conversation_summaries \
         ORDER BY last_modified_time DESC LIMIT {limit});"
    );
    let Ok(out) = std::process::Command::new("sqlite3").arg(&db).arg(&sql).output() else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let rows: Vec<serde_json::Value> = serde_json::from_str(text.trim()).unwrap_or_default();
    rows.iter()
        .filter_map(|r| {
            let id = r.get("id")?.as_str()?.to_string();
            let title = r.get("title").and_then(|x| x.as_str()).unwrap_or("").trim();
            let preview = r.get("preview").and_then(|x| x.as_str()).unwrap_or("").trim();
            let mut label = if title.is_empty() { preview.to_string() } else { title.to_string() };
            if label.is_empty() {
                label = id.chars().take(8).collect();
            }
            label = label.split_whitespace().collect::<Vec<_>>().take_chars(120);
            let mtime = r.get("ts").and_then(|x| x.as_u64()).unwrap_or(0);
            // workspace_uris 는 `file:///path` 목록(JSON 배열 문자열)이다.
            let cwd = r
                .get("ws")
                .and_then(|x| x.as_str())
                .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
                .and_then(|v| v.into_iter().next())
                .map(|u| u.strip_prefix("file://").unwrap_or(&u).to_string())
                .unwrap_or_default();
            Some(RecentSession { harness: "agy".into(), id, label, mtime, cwd })
        })
        .collect()
}

/// 세 하네스를 합쳐 최신순으로. 통합 피커의 단일 진입점.
/// 각 하네스에서 `limit` 개씩 걷은 뒤 합쳐서 다시 상위 `limit` 만 남긴다 —
/// 한쪽이 최근 것을 독차지해도 다른 쪽 최신 항목을 놓치지 않는다.
pub fn recent_all_sessions(limit: usize) -> Vec<RecentSession> {
    let mut all = recent_claude_sessions_all(limit);
    all.extend(recent_codex_sessions(limit));
    all.extend(recent_agy_sessions(limit));
    all.sort_by(|a, b| b.mtime.cmp(&a.mtime));
    all.truncate(limit);
    all
}

/// 하네스별 "이어가기" 셸 한 줄. 세 CLI 가 서로 다른 플래그를 쓴다.
///
/// CLI 피커와 GUI(handler.rs 가 새 pane 에 주입)가 **같은 한 벌을 쓴다** — 예전엔
/// 두 군데가 각자 `claude --resume` 을 조립하고 있었고, 그런 쌍은 한쪽만 고쳐진다.
/// PTY 주입용 개행(`\r`)은 붙이지 않는다. 필요한 쪽이 붙인다.
///
/// cwd 로 먼저 옮기는 게 중요하다 — claude 는 세션을 cwd 별 디렉터리에 나눠 두어
/// 다른 자리에서 `--resume` 하면 그 세션을 아예 못 찾는다. cwd 를 모르는 하네스
/// (codex 는 rollout 에 안 남긴다)는 지금 자리에서 연다.
///
/// 값은 전부 작은따옴표로 감싼다. id 는 uuid 라 위험할 게 없지만 cwd 는 사람이
/// 만든 경로라 공백·괄호가 흔하다 — 따옴표가 없으면 `cd` 가 거기서 끊긴다.
pub fn resume_command(harness: &str, id: &str, cwd: &str) -> String {
    let q = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
    let cmd = match harness {
        "codex" => format!("codex resume {}", q(id)),
        "agy" => format!("agy --conversation {}", q(id)),
        _ => format!("claude --resume {}", q(id)),
    };
    if cwd.is_empty() { cmd } else { format!("cd {} && {cmd}", q(cwd)) }
}

#[cfg(test)]
mod resume_command_tests {
    use super::resume_command;

    #[test]
    fn claude_moves_to_the_session_cwd_first() {
        // claude 는 세션을 cwd 별 디렉터리에 나눠 둔다 — 다른 자리에서 --resume
        // 하면 그 세션을 못 찾으므로 cd 가 앞에 붙어야 한다.
        let got = resume_command("claude", "abc-123", "/Users/kasa/proj");
        assert_eq!(got, "cd '/Users/kasa/proj' && claude --resume 'abc-123'");
    }

    #[test]
    fn codex_and_agy_use_their_own_flags() {
        assert_eq!(resume_command("codex", "id1", ""), "codex resume 'id1'");
        assert_eq!(resume_command("agy", "id2", ""), "agy --conversation 'id2'");
    }

    #[test]
    fn unknown_harness_falls_back_to_claude() {
        assert_eq!(resume_command("", "id3", ""), "claude --resume 'id3'");
    }

    #[test]
    fn quotes_paths_with_spaces() {
        // 사람이 만든 경로엔 공백·괄호가 흔하다. 따옴표가 없으면 cd 가 거기서 끊긴다.
        let got = resume_command("claude", "id", "/Users/kasa/My Projects (old)");
        assert_eq!(got, "cd '/Users/kasa/My Projects (old)' && claude --resume 'id'");
    }

    #[test]
    fn escapes_a_single_quote_in_the_path() {
        let got = resume_command("claude", "id", "/tmp/it's");
        assert_eq!(got, r#"cd '/tmp/it'\''s' && claude --resume 'id'"#);
    }
}
