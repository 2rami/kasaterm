//! Offline Claude-session discovery from the transcript jsonl files under
//! `~/.claude/projects/<encoded-cwd>/`. Pure filesystem + serde_json — no live
//! pane, no GUI state — so both the GUI backend (PtyBackend, app/kasaterm) and
//! the standalone web server (StandaloneBackend, kasa-mcp) share one impl.
//!
//! Lifted out of app/kasaterm/src/socket.rs so the standalone `serve-web` bin
//! can list/read sessions without depending on the winit/wgpu GUI crate.

use std::collections::HashMap;
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

/// `cwd` 를 모르는 채 세션 jsonl 을 찾는다 — `~/.claude/projects/*/<id>.jsonl` 을 훑는다.
///
/// ⚠️**`cwd` 를 아는 쪽은 항상 `session_jsonl_path` 를 먼저 써라.** 이건 폴백이다.
///
/// 필요한 이유는 **목록을 만드는 길과 여는 길이 서로 다른 cwd 를 쓰기** 때문이다.
/// standalone(`kasa-serve-web`)의 board 는 `claude agents --json` 이 알려 준 **세션마다의
/// cwd** 로 제목을 읽는데, `peek`·`transcript` 는 프로세스가 뜰 때 정해진 **root 하나**로만
/// 열려 했다. 맥미니 실측에서 그 둘의 교집합이 **0개**였다 — board 에 14개가 멀쩡히 뜨는데
/// 누르면 전부 「no transcript」였고, 목록이 정상이라 화면에선 원인이 안 보인다.
///
/// 세션 id 는 uuid 라 프로젝트가 달라도 안 겹치므로, 찾은 첫 파일이 곧 그 세션이다.
/// 비용은 projects 디렉터리 한 번 읽기 + 폴더마다 `exists()` 한 번이다(파일을 안 연다).
pub fn session_jsonl_path_anywhere(id: &str) -> Option<PathBuf> {
    if !is_uuid(id) {
        return None;
    }
    let home = std::env::var_os("HOME")?;
    let root = PathBuf::from(home).join(".claude/projects");
    let name = format!("{id}.jsonl");
    for proj in std::fs::read_dir(&root).ok()?.flatten() {
        let candidate = proj.path().join(&name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// `cwd` 로 먼저, 못 찾으면 프로젝트 전체에서. 세션을 **여는** 쪽(peek·transcript·send)이
/// 쓴다 — 목록에 뜬 세션이 열리지 않는 상태를 만들지 않기 위해서다.
pub fn session_jsonl_path_resolved(cwd: &Path, id: &str) -> Option<PathBuf> {
    if let Some(p) = session_jsonl_path(cwd, id) {
        if p.is_file() {
            return Some(p);
        }
    }
    session_jsonl_path_anywhere(id)
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
                preview: last_exchange(&path, "claude"),
                id,
                label,
                mtime: mtime_secs,
                cwd: cwd_str.clone(),
                student: String::new(),
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
            if let Some(t) = custom_title_of_line(line) {
                last = Some(t);
            }
        }
        if last.is_some() {
            return last;
        }
        end = start;
    }
    None
}

/// jsonl 한 줄이 `/rename` 레코드면 그 제목, 아니면 None.
///
/// 파일을 역스캔해 찾는 쪽(`last_custom_title`)과 **이미 읽어 둔 꼬리**에서 찾는
/// 쪽(GUI 의 pane 제목 동기화)이 같은 규칙을 봐야, 한 pane 을 두고 두 화면이 서로
/// 다른 이름을 말하지 않는다.
///
/// 파싱 전에 문자열로 한 번 거른다 — 에이전트 세션은 한 줄이 수백 KB 라 전부
/// `serde_json` 에 넣으면 꼬리 훑기가 그 자체로 무거워진다.
pub fn custom_title_of_line(line: &str) -> Option<String> {
    if !line.contains("\"custom-title\"") {
        return None;
    }
    let v = serde_json::from_str::<serde_json::Value>(line).ok()?;
    if v.get("type").and_then(|t| t.as_str()) != Some("custom-title") {
        return None;
    }
    let t = v.get("customTitle").and_then(|t| t.as_str())?.trim();
    (!t.is_empty()).then(|| t.chars().take(80).collect())
}

/// 라벨로 부적합한 메타성 user 텍스트(슬래시 명령·시스템 주입·bash 출력 래퍼).
/// claude 가 첫 턴에 흔히 끼워넣어 라벨을 오염시키므로 건너뛴다.
fn is_meta_user_text(t: &str) -> bool {
    t.starts_with("<command-")
        || t.starts_with("<local-command")
        || t.starts_with("<system-reminder")
        || t.starts_with("<bash-")
        || t.starts_with("Caveat:")
        // 다른 pane 이 SendMessage 로 보낸 지시는 `<cross-session-message from=…>`
        // 래퍼로 남는다 — 첫 줄이 그 여는 태그라 라벨로 부적합하다. 오케스트레이터가
        // 굴리는 학생은 첫 발화가 대개 이것이므로 걸러야 제목이 태그로 새지 않는다.
        || t.starts_with("<cross-session-message")
        // claude 내부 title-gen 서브세션의 첫 user 프롬프트 — custom-title 스탬프
        // 전 찰나에 이게 첫 user 폴백으로 새어 인레이에 유출됐다(거노 실측).
        || t.starts_with("아래 대화의 주제를 나타내는")
        || t.starts_with("다음 대화 발췌를 보고")
}

/// 이미 읽어 둔 transcript 꼬리에서 **첫 유효 user 프롬프트**(= 갓 소환된 학생이
/// 받은 첫 지시·브리프)를 pane 제목 후보로 뽑는다. `parse_session_label` 의
/// 첫-user 규칙과 같되 파일을 다시 열지 않고 넘겨받은 꼬리 문자열만 훑는다 —
/// GUI 의 제목 동기화가 매 틱 이미 읽는 꼬리를 재활용하려는 것이다.
///
/// custom-title 이 아직 하나도 없는 학생 pane 에만 의미가 있다(제목이 있으면
/// 호출부가 이 폴백을 안 탄다). 첫 줄만 60자로 자른다 — 탭 이름표는 좁고,
/// 여러 줄 브리프의 둘째 줄부터는 맥락이라 제목감이 아니다.
pub fn first_prompt_label(tail: &str) -> Option<String> {
    for line in tail.lines() {
        if !line.contains("\"user\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if v.get("type").and_then(|t| t.as_str()) != Some("user") {
            continue;
        }
        if v.get("isMeta").and_then(|m| m.as_bool()).unwrap_or(false) {
            continue;
        }
        let Some(txt) = user_message_text(&v) else { continue };
        let txt = txt.trim();
        if txt.is_empty() || is_meta_user_text(txt) {
            continue;
        }
        let first_line = txt.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or(txt);
        if !first_line.is_empty() {
            return Some(first_line.chars().take(60).collect());
        }
    }
    None
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

/// 세션 jsonl 에서 마지막 `turns` 개의 대화 턴을 `(role, text)` 로. role 은
/// `"user"`/`"assistant"`. `turns == 0` 이면 전부.
///
/// 도구 호출·메타 줄은 뺀다 — 사람이 읽을 대화만 남긴다. **자르지 않는다**:
/// 화면에 전문을 보여주는 쪽이 부르므로, 줄이는 건 부르는 쪽이 정할 일이다.
pub fn transcript_turns_at(path: &Path, turns: usize) -> Vec<(String, String)> {
    use std::io::BufRead;
    let Ok(f) = std::fs::File::open(path) else { return Vec::new() };
    let mut out: Vec<(String, String)> = Vec::new();
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
                        out.push(("user".into(), t.to_string()));
                    }
                }
            }
            Some("assistant") => {
                if let Some(t) = assistant_message_text(&v) {
                    let t = t.trim();
                    if !t.is_empty() {
                        out.push(("assistant".into(), t.to_string()));
                    }
                }
            }
            _ => {}
        }
    }
    if turns > 0 && out.len() > turns {
        out.drain(0..out.len() - turns);
    }
    out
}

/// peek 용 — 세션 jsonl 마지막 `turns` 개 user/assistant 텍스트를 사람이 읽을 형태로.
/// 라이브 pane 화면이 없는 standalone 에서 background 세션 '엿보기'를 대신한다.
///
/// ⚠️`cwd` 를 모르면 못 찾는다. 목록과 여는 길이 다른 cwd 를 쓰는 곳
/// (standalone)에서는 `session_jsonl_path_resolved` 로 경로를 먼저 풀고
/// `transcript_turns_at` 을 직접 부를 것.
pub fn transcript_tail_text(cwd: &Path, id: &str, turns: usize) -> Option<String> {
    let path = session_jsonl_path(cwd, id)?;
    Some(format_turns(&transcript_turns_at(&path, turns.max(1))))
}

/// 대화 턴을 peek 화면 문자열로. 한 발언 500자에서 자른다 — 엿보기지 전문이 아니다.
pub fn format_turns(turns: &[(String, String)]) -> String {
    turns
        .iter()
        .map(|(role, text)| {
            let who = if role == "user" { "[사용자]" } else { "[claude]" };
            format!("{who} {}", text.chars().take(500).collect::<String>())
        })
        .collect::<Vec<_>>()
        .join("\n\n")
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

    #[test]
    fn first_prompt_label_skips_meta_and_takes_first_line() {
        // 슬래시 명령(isMeta)·다른 pane 의 SendMessage 래퍼는 건너뛰고, 첫 진짜
        // 지시의 첫 줄만 라벨이 된다.
        let tail = concat!(
            r#"{"type":"user","isMeta":true,"message":{"content":"<command-name>/clear</command-name>"}}"#,
            "\n",
            r#"{"type":"user","message":{"content":"<cross-session-message from=\"x\">일 시켜</cross-session-message>"}}"#,
            "\n",
            r#"{"type":"user","message":{"content":"로그인 버그 고쳐줘\n맥락은 아래에"}}"#,
            "\n",
        );
        assert_eq!(first_prompt_label(tail).as_deref(), Some("로그인 버그 고쳐줘"));
    }

    #[test]
    fn first_prompt_label_none_when_only_meta() {
        let tail = concat!(
            r#"{"type":"user","message":{"content":"<system-reminder>x</system-reminder>"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":"네"}}"#,
            "\n",
        );
        assert_eq!(first_prompt_label(tail), None);
    }

    #[test]
    fn 대화턴은_메타와_도구줄을_빼고_마지막_n개만_남긴다() {
        // `/transcript` 가 `ok:true` 에 `turns: 0` 을 주던 자리의 안전망이다 —
        // 빈 목록은 오류로 안 보이고 「원래 빈 세션」과 구분이 안 된다.
        let body = concat!(
            r#"{"type":"user","isMeta":true,"message":{"content":"<command-name>/clear</command-name>"}}"#,
            "\n",
            r#"{"type":"user","message":{"content":"1+1 은?"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"2"}]}}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash"}]}}"#,
            "\n",
            r#"{"type":"user","message":{"content":"10 을 곱하면?"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":"20"}}"#,
            "\n",
        );
        let p = tmp_jsonl("turns", body);
        let all = transcript_turns_at(&p, 0);
        // isMeta 한 줄과 text 블록이 없는 tool_use 한 줄은 빠진다.
        assert_eq!(all.len(), 4);
        assert_eq!(all[0], ("user".to_string(), "1+1 은?".to_string()));
        assert_eq!(all[3], ("assistant".to_string(), "20".to_string()));
        // turns 는 **마지막** N개다(앞이 아니라).
        let tail = transcript_turns_at(&p, 2);
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0].1, "10 을 곱하면?");
        // peek 표시는 같은 턴을 사람이 읽을 형태로.
        assert_eq!(format_turns(&tail), "[사용자] 10 을 곱하면?\n\n[claude] 20");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn 세션을_전역에서_찾을_때_uuid_가_아니면_안_찾는다() {
        // pane id(`%3`)나 빈 문자열로 projects 전체를 훑지 않게 하는 관문이다.
        assert!(session_jsonl_path_anywhere("%3").is_none());
        assert!(session_jsonl_path_anywhere("").is_none());
        assert!(session_jsonl_path_anywhere("../../etc/passwd").is_none());
    }

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
//   agy     ~/.gemini/antigravity-cli/brain/<uuid>/.system_generated/logs/*.jsonl
//
// 셋 다 결국 줄 단위 JSON 이라 읽는 모양은 같다. agy 만 제목을 저장해 두지 않아
// 대화 안에서 파생해야 한다.

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

/// 파일 **끝** 일부만 읽어 줄로 쪼갠다.
///
/// transcript 는 수십 MB 까지 자라고 목록은 한 번에 수십 개를 그린다 — 통째로
/// 읽으면 목록 한 번에 수백 MB 를 훑는다. 그래서 끝에서만 잘라 온다.
///
/// 잘린 첫 줄을 세어 버리지 않는다. JSON 으로 안 풀리면 호출부가 어차피
/// 건너뛰므로, "몇 바이트가 잘렸나"를 계산하는 자리를 아예 없애는 편이 안전하다
/// — 그 계산은 멀티바이트 문자 경계에서 조용히 틀리는 종류의 것이다.
fn tail_lines(path: &Path, max_bytes: u64) -> Vec<String> {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else { return Vec::new() };
    let Ok(meta) = f.metadata() else { return Vec::new() };
    if f.seek(SeekFrom::Start(meta.len().saturating_sub(max_bytes))).is_err() {
        return Vec::new();
    }
    let mut buf = Vec::new();
    if f.read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    String::from_utf8_lossy(&buf).lines().map(str::to_string).collect()
}

/// 목록 한 줄에 붙일 "마지막으로 오간 말". 누가 한 말인지까지 담는다 — 내가
/// 뭔가 시켜 놓고 끊긴 세션과 답을 받고 끝난 세션은 이어갈 때 하는 일이 다르다.
///
/// 끝에서 128KB 만 본다. 그 안이 통째로 큰 도구 결과 한 줄이면 못 뽑는데,
/// 그때는 빈 값이 나가고 목록은 그 줄을 안 그린다 — 없는 걸 있는 척하는 것보다 낫다.
fn last_exchange(path: &Path, harness: &str) -> String {
    const TAIL: u64 = 128 * 1024;
    const MAX_CHARS: usize = 200;

    let say = |who: &str, text: &str| {
        let t = text.split_whitespace().collect::<Vec<_>>().take_chars(MAX_CHARS);
        if t.is_empty() { String::new() } else { format!("{who}: {t}") }
    };

    for line in tail_lines(path, TAIL).iter().rev() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let got = if harness == "codex" {
            let p = v.get("payload").unwrap_or(&v);
            if p.get("type").and_then(|t| t.as_str()) != Some("message") {
                continue;
            }
            let text = p
                .get("content")
                .and_then(|c| c.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            // 주입된 맥락은 사람이 한 말이 아니다 — 라벨에서 걸러낸 것과 같은 이유다.
            if codex_injected(&text) {
                continue;
            }
            match p.get("role").and_then(|r| r.as_str()) {
                Some("user") => say("나", &text),
                Some("assistant") => say("에이전트", &text),
                _ => continue,
            }
        } else {
            match v.get("type").and_then(|t| t.as_str()) {
                Some("assistant") => assistant_message_text(&v).map(|t| say("에이전트", &t)),
                Some("user") => {
                    if v.get("isMeta").and_then(|m| m.as_bool()).unwrap_or(false) {
                        continue;
                    }
                    user_message_text(&v).filter(|t| !is_meta_user_text(t.trim())).map(|t| say("나", &t))
                }
                _ => continue,
            }
            .unwrap_or_default()
        };
        if !got.is_empty() {
            return got;
        }
    }
    String::new()
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
            let s =
                RecentSession { harness: "claude".into(), id, label, mtime, cwd, preview: String::new(), student: String::new() };
            Some((s, path))
        })
        .take(limit)
        // preview 는 **살아남은 행에만** 붙인다. 위에서 limit 의 네 배를 걷으므로
        // 여기서 붙이지 않고 filter_map 안에서 뽑으면 버려질 행까지 파일 끝을
        // 읽는다 — 네 배를 읽고 네 개 중 셋을 버리는 꼴이다.
        .map(|(mut s, path)| {
            s.preview = last_exchange(&path, "claude");
            s
        })
        .collect()
}

/// 폴더를 재귀하며 jsonl 을 모은다. 심링크는 따라가지 않는다(`file_type` 은
/// 링크를 디렉터리로 보지 않는다) — 그래서 순환이 원천봉쇄되고, `depth` 는 그
/// 위에 얹은 값싼 보험이지 구조를 뜻하는 수가 아니다.
fn collect_jsonl(dir: &Path, depth: usize, out: &mut Vec<(u64, PathBuf)>) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let Ok(ft) = e.file_type() else { continue };
        let p = e.path();
        if ft.is_dir() {
            collect_jsonl(&p, depth - 1, out);
        } else if p.extension().and_then(|x| x.to_str()) == Some("jsonl") {
            if let Ok(meta) = e.metadata() {
                out.push((mtime_secs(&meta), p));
            }
        }
    }
}

/// rollout 첫 줄에서 `(id, cwd, exec 인가)`.
///
/// 새 포맷은 모든 필드가 `payload` 아래에 있고 옛 포맷은 최상위에 평면으로 있다.
/// `payload` 가 있으면 그쪽을, 없으면 자기 자신을 보는 것으로 둘을 한 벌로 읽는다.
fn codex_head(v: &serde_json::Value) -> (String, String, bool) {
    let p = v.get("payload").unwrap_or(v);
    let s = |k: &str| p.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string();
    let id = {
        let sid = s("session_id");
        if sid.is_empty() { s("id") } else { sid }
    };
    // `codex_exec` 는 스크립트 일회성 실행이다. `codex-tui`(대화형)와 갈라야 한다.
    let exec = p.get("originator").and_then(|x| x.as_str()).is_some_and(|o| o.contains("exec"))
        || p.get("source").and_then(|x| x.as_str()) == Some("exec");
    (id, s("cwd"), exec)
}

/// user 롤로 들어왔지만 사람이 한 말이 아닌 것.
///
/// codex 는 프로젝트 지시(AGENTS.md)·플러그인 목록·환경 정보를 **user 메시지로**
/// 밀어 넣는다. 첫 user 발화를 그냥 쓰면 **모든 행의 제목이 같아져** 목록에서
/// 세션을 고를 수가 없다 — 실측에서 대화형 세션 전부가
/// `# AGENTS.md instructions` 로 시작했다.
fn codex_injected(text: &str) -> bool {
    const MARKS: [&str; 6] = [
        "# AGENTS.md instructions",
        "<user_instructions>",
        "<environment_context>",
        "<recommended_plugins>",
        "<skills_instructions>",
        "<INSTRUCTIONS>",
    ];
    let t = text.trim_start();
    MARKS.iter().any(|m| t.starts_with(m))
}

/// 한 rollout 파일 → 목록 한 줄. 읽을 게 없으면 `None`.
fn codex_session_of(path: &Path, mtime: u64) -> Option<RecentSession> {
    use std::io::BufRead;
    let f = std::fs::File::open(path).ok()?;
    let mut id = String::new();
    let mut cwd = String::new();
    let mut label = String::new();
    // 앞 200줄이면 헤더와 첫 사람 발화를 지나친다. 주입 맥락이 서너 줄 앞에
    // 끼므로 옛 코드보다 더 걸어야 하지만, 그래도 파일 앞머리다.
    for line in std::io::BufReader::new(f).lines().take(200).map_while(Result::ok) {
        // 첫 줄에 시스템 프롬프트가 통째로 박혀 있어 50KB 를 넘는다. exec 은 열에
        // 아홉이고 그 판정 하나에 그만큼을 매번 JSON 으로 푸는 게 codex 목록의
        // 제일 큰 비용이었다 — 문자열로 먼저 걸러 그 파싱을 아예 건너뛴다.
        // 표기가 바뀌면 이 빠른 길만 안 걸리고 아래 정식 판정이 그대로 잡는다.
        if id.is_empty() && line.contains(r#""originator":"codex_exec""#) {
            return None;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        if id.is_empty() {
            let (i, c, exec) = codex_head(&v);
            // 이어갈 대화가 아니다. 임시폴더에서 돌고 끝나는데 수가 압도적이라
            // (2026-08 실측 최근 120개 중 113개) 진짜 대화를 목록 밖으로 민다.
            if exec {
                return None;
            }
            id = i;
            cwd = c;
        }
        let p = v.get("payload").unwrap_or(&v);
        if label.is_empty()
            && p.get("type").and_then(|t| t.as_str()) == Some("message")
            && p.get("role").and_then(|r| r.as_str()) == Some("user")
        {
            let text = p
                .get("content")
                .and_then(|c| c.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            if !codex_injected(&text) {
                // chars() 로 자른다 — String::truncate 는 바이트 오프셋이라
                // 한글 중간을 끊으면 그 자리에서 panic 한다(실측).
                label = text.split_whitespace().collect::<Vec<_>>().take_chars(120);
            }
        }
        if !id.is_empty() && !label.is_empty() {
            break;
        }
    }
    // 파일명(rollout-<날짜>-<uuid>)에서라도 id 를 건진다.
    if id.is_empty() {
        id = path.file_stem()?.to_str()?.rsplit_once('-').map(|(_, u)| u.to_string())?;
    }
    // 사람이 한 말이 하나도 없는 세션은 뺀다 — codex 는 띄우기만 하고 아무 말도
    // 안 한 세션도 rollout 파일을 남기는데, 이어갈 게 없는 항목이 목록 상단을
    // uuid 로 채워 진짜 대화를 밀어낸다.
    if label.is_empty() {
        return None;
    }
    let preview = last_exchange(path, "codex");
    Some(RecentSession { harness: "codex".into(), id, label, mtime, cwd, preview, student: String::new() })
}

/// 최근 codex 세션. 조심할 것이 셋이다.
///
/// **① 파일이 날짜 폴더에 있다.** codex 는 이제 `~/.codex/sessions/YYYY/MM/DD/` 에
/// 쌓고 옛 파일만 최상위에 평면으로 남는다. 최상위만 훑던 옛 코드는 2026-08 실측
/// 에서 **681개 중 8개**만 봤고 그 8개가 전부 1년 전 것이라, 화면에는 "codex 를
/// 안 쓴 사람"처럼 보였다.
///
/// **② rollout 포맷이 바뀌었다.** 예전엔 `{id, type:"message", role, content}` 가
/// 최상위에 평면으로 있었는데 지금은 전부 `payload` 아래다. 옛 파일이 그대로
/// 남아 있으니 **두 형태를 다 읽는다** — 새 포맷만 보면 옛 세션이 사라진다.
///
/// **③ `cwd` 는 있다.** `payload.cwd` 에 적힌다. 없다고 적혀 있던 옛 주석 탓에
/// 목록의 프로젝트 칸이 비어 uuid 조각이 대신 떴다.
///
/// exec 인지는 파일을 열어야 알 수 있고(첫 줄에만 적힌다) 그 비율이 압도적이라,
/// **`limit` 에 비례하는 스캔 상한은 쓰지 않는다** — 실측에서 `limit=8` 이 7개만
/// 돌려줬다. 최신 320개가 거의 다 exec 이라 상한에 먼저 걸렸고, 그 아래 있던
/// 진짜 대화는 조용히 잘렸다(같은 디스크에서 `limit=20` 은 20개를 다 채웠다).
/// 전수 스캔은 681개에 0.14초(debug)라 그 값을 살 이유가 없다. 남긴 상한은
/// 폭주 방어일 뿐이고, 걸리면 조용히 자르는 게 아니라 애초에 도달하지 않는 수다.
pub fn recent_codex_sessions(limit: usize) -> Vec<RecentSession> {
    let Some(home) = std::env::var_os("HOME") else { return Vec::new() };
    recent_codex_sessions_in(&PathBuf::from(home).join(".codex/sessions"), limit)
}

/// 한 프로젝트 안의 codex 세션만. rollout 이 `payload.cwd` 를 남기므로 가능하다.
pub fn recent_codex_sessions_for(cwd: &Path, limit: usize) -> Vec<RecentSession> {
    let Some(home) = std::env::var_os("HOME") else { return Vec::new() };
    let root = PathBuf::from(home).join(".codex/sessions");
    codex_sessions(&root, limit, Some(&cwd.to_string_lossy()))
}

/// 루트를 받는 본체. 테스트가 가짜 rollout 을 심어 돌릴 수 있게 갈라 둔다.
pub fn recent_codex_sessions_in(root: &Path, limit: usize) -> Vec<RecentSession> {
    codex_sessions(root, limit, None)
}

fn codex_sessions(root: &Path, limit: usize, only_cwd: Option<&str>) -> Vec<RecentSession> {
    /// 폭주 방어. 사람의 codex 기록이 이만큼 쌓이는 일은 없다 — 실측 681개.
    const MAX_SCAN: usize = 20_000;

    let mut files: Vec<(u64, PathBuf)> = Vec::new();
    collect_jsonl(root, 8, &mut files);
    files.sort_by(|a, b| b.0.cmp(&a.0));

    let mut out = Vec::with_capacity(limit);
    for (mtime, path) in files.into_iter().take(MAX_SCAN) {
        let Some(s) = codex_session_of(&path, mtime) else { continue };
        if only_cwd.is_some_and(|c| s.cwd != c) {
            continue;
        }
        out.push(s);
        if out.len() >= limit {
            break;
        }
    }
    out
}

/// 이 프로젝트의 최근 세션 — 세 하네스를 가로질러. 오르카의 「프로젝트」 탭에
/// 해당한다.
///
/// `recent_sessions_for` 만 쓰면 claude 만 보인다. 같은 폴더에서 codex 로 일한
/// 기록이 있어도 프로젝트 목록에는 없는 것이 되는데, 그게 "여기서 뭘 하다
/// 말았지"를 물을 때 제일 아쉬운 자리다.
pub fn recent_sessions_here(cwd: &Path, limit: usize) -> Vec<RecentSession> {
    let want = cwd.to_string_lossy().into_owned();
    let mut all = recent_sessions_for(cwd, limit);
    all.extend(recent_codex_sessions_for(cwd, limit));
    // agy 는 요약 테이블을 한 번 읽는 게 전부라, 넉넉히 걷고 걸러도 싸다.
    all.extend(recent_agy_sessions(limit.saturating_mul(4)).into_iter().filter(|s| s.cwd == want));
    all.sort_by(|a, b| b.mtime.cmp(&a.mtime));
    all.truncate(limit);
    all
}

#[cfg(test)]
mod preview_tests {
    use super::last_exchange;
    use std::path::PathBuf;

    fn tmp(name: &str, body: &str) -> PathBuf {
        let d = std::env::temp_dir().join("kasaterm-preview-test");
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join(format!("{name}.jsonl"));
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn claude_takes_the_last_thing_said() {
        let p = tmp(
            "claude-last",
            &[
                r#"{"type":"user","message":{"content":"처음 시킨 것"}}"#,
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"했습니다"}]}}"#,
                r#"{"type":"user","message":{"content":"그럼 이건?"}}"#,
            ]
            .join("\n"),
        );
        assert_eq!(last_exchange(&p, "claude"), "나: 그럼 이건?");
    }

    #[test]
    fn who_said_it_is_part_of_the_line() {
        // 내가 시켜 놓고 끊긴 세션과 답을 받고 끝난 세션은 이어갈 때 하는 일이
        // 다르다. 화자가 없으면 목록에서 그 둘을 못 가른다.
        let p = tmp(
            "claude-who",
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"다 됐습니다"}]}}"#,
        );
        assert_eq!(last_exchange(&p, "claude"), "에이전트: 다 됐습니다");
    }

    #[test]
    fn machine_chatter_is_not_the_last_word() {
        // system-reminder·명령 에코는 사람이 한 말이 아니다. 그대로 쓰면 목록의
        // 모든 행이 같은 시스템 문구로 끝난 것처럼 보인다.
        let p = tmp(
            "claude-meta",
            &[
                r#"{"type":"user","message":{"content":"진짜 마지막 말"}}"#,
                r#"{"type":"user","message":{"content":"<system-reminder>어쩌구</system-reminder>"}}"#,
                r#"{"type":"user","isMeta":true,"message":{"content":"메타"}}"#,
            ]
            .join("\n"),
        );
        assert_eq!(last_exchange(&p, "claude"), "나: 진짜 마지막 말");
    }

    #[test]
    fn codex_reads_the_payload_shape() {
        let p = tmp(
            "codex-last",
            &[
                r#"{"payload":{"type":"message","role":"user","content":[{"text":"코덱스야"}]}}"#,
                r#"{"payload":{"type":"message","role":"assistant","content":[{"text":"넵"}]}}"#,
            ]
            .join("\n"),
        );
        assert_eq!(last_exchange(&p, "codex"), "에이전트: 넵");
    }

    /// 128KB 를 넘는 파일에서도 끝을 읽어야 하고, 그때 잘려 들어온 첫 줄이
    /// 결과를 오염시키면 안 된다. 자른 바이트 수를 세지 않고 "안 풀리면 건너뛴다"
    /// 로 처리하는 것이 이 테스트가 지키는 규칙이다.
    #[test]
    fn a_huge_file_still_yields_its_tail() {
        let filler = "x".repeat(4000);
        let mut body = String::new();
        for i in 0..60 {
            body.push_str(&format!(
                r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"{i} {filler}"}}]}}}}"#
            ));
            body.push('\n');
        }
        body.push_str(r#"{"type":"user","message":{"content":"끝에 남긴 말"}}"#);
        let p = tmp("huge", &body);
        assert!(std::fs::metadata(&p).unwrap().len() > 128 * 1024, "테스트 파일이 128KB 를 못 넘었다");
        assert_eq!(last_exchange(&p, "claude"), "나: 끝에 남긴 말");
    }

    #[test]
    fn nothing_readable_means_empty_not_a_guess() {
        let p = tmp("junk", "not json\n{\"type\":\"summary\"}\n");
        assert_eq!(last_exchange(&p, "claude"), "");
    }
}

#[cfg(test)]
mod codex_listing_tests {
    use super::{codex_injected, recent_codex_sessions_in};
    use std::path::PathBuf;

    /// 테스트마다 자기 폴더를 쓴다 — 같은 루트를 나눠 쓰면 한 테스트가 심은
    /// 파일이 다른 테스트의 목록에 섞여, 실패가 남의 탓처럼 보인다.
    fn root(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("kasaterm-codex-test-{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(root: &std::path::Path, rel: &str, lines: &[&str]) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, lines.join("\n")).unwrap();
    }

    /// 새 포맷: 모든 것이 `payload` 아래. 그리고 날짜 폴더 세 단 아래에 있다.
    fn new_format(cwd: &str, origin: &str, msgs: &[&str]) -> Vec<String> {
        let mut v = vec![format!(
            r#"{{"type":"session_meta","payload":{{"session_id":"sid-新","cwd":"{cwd}","originator":"{origin}"}}}}"#
        )];
        for m in msgs {
            v.push(format!(
                r#"{{"type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"{m}"}}]}}}}"#
            ));
        }
        v
    }

    #[test]
    fn finds_sessions_under_date_folders() {
        // 최상위만 훑던 옛 코드가 실측에서 681개 중 8개만 봤다. 이 한 줄이 그 회귀다.
        let r = root("nested");
        let lines = new_format("/proj", "codex-tui", &["안녕 코덱스"]);
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        write(&r, "2026/08/09/rollout-2026-08-09T22-06-20-aaa.jsonl", &refs);

        let got = recent_codex_sessions_in(&r, 10);
        assert_eq!(got.len(), 1, "날짜 폴더 아래를 못 봤다");
        assert_eq!(got[0].label, "안녕 코덱스");
        assert_eq!(got[0].cwd, "/proj", "payload.cwd 를 안 읽었다");
        assert_eq!(got[0].harness, "codex");
    }

    #[test]
    fn old_flat_format_still_reads() {
        // 옛 파일이 디스크에 그대로 남아 있다 — 새 포맷만 보면 그게 통째로 사라진다.
        let r = root("oldflat");
        write(
            &r,
            "rollout-2025-07-15T19-42-06-7dc830cd.jsonl",
            &[
                r#"{"id":"old-sid","timestamp":"2025-07-15T19:42:06Z"}"#,
                r#"{"type":"message","role":"user","content":[{"text":"cd desktop"}]}"#,
            ],
        );
        let got = recent_codex_sessions_in(&r, 10);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, "old-sid");
        assert_eq!(got[0].label, "cd desktop");
    }

    #[test]
    fn exec_runs_are_left_out() {
        // 스크립트 일회성 실행. 임시폴더에서 돌고 이어갈 대화가 없는데 수가
        // 압도적이라(실측 120개 중 113개) 진짜 대화를 목록 밖으로 민다.
        let r = root("exec");
        let ex = new_format("/tmp/ppcodex-1", "codex_exec", &["그림 그려"]);
        let tui = new_format("/proj", "codex-tui", &["진짜 대화"]);
        write(&r, "2026/08/09/a.jsonl", &ex.iter().map(String::as_str).collect::<Vec<_>>());
        write(&r, "2026/08/09/b.jsonl", &tui.iter().map(String::as_str).collect::<Vec<_>>());

        let got = recent_codex_sessions_in(&r, 10);
        assert_eq!(got.len(), 1, "exec 실행이 목록에 남았다");
        assert_eq!(got[0].label, "진짜 대화");
    }

    #[test]
    fn injected_context_is_not_a_title() {
        // codex 는 AGENTS.md 를 user 롤로 밀어 넣는다. 그대로 쓰면 모든 행의
        // 제목이 같아져 목록에서 세션을 고를 수가 없다.
        let r = root("injected");
        let lines = new_format(
            "/proj",
            "codex-tui",
            &["# AGENTS.md instructions 개발을 처음배우는", "실제로 물어본 것"],
        );
        write(&r, "2026/08/09/c.jsonl", &lines.iter().map(String::as_str).collect::<Vec<_>>());

        let got = recent_codex_sessions_in(&r, 10);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].label, "실제로 물어본 것");
    }

    #[test]
    fn injected_markers_cover_the_shapes_seen_on_disk() {
        for m in [
            "# AGENTS.md instructions\n...",
            "<user_instructions>\n...",
            "<environment_context>",
            "<recommended_plugins>\nHere is a list",
            "<skills_instructions>",
            "  <INSTRUCTIONS>",
        ] {
            assert!(codex_injected(m), "{m:?} 를 사람 발화로 봤다");
        }
        // 사람이 꺾쇠로 시작하는 말을 할 수도 있다 — 아는 것만 걸러야 한다.
        assert!(!codex_injected("<div> 태그가 왜 안 먹지"));
        assert!(!codex_injected("안녕"));
    }

    /// exec 실행이 잔뜩 쌓인 사이에 파묻힌 대화도 작은 limit 으로 찾아야 한다.
    /// `limit` 에 비례하는 스캔 상한을 두면 여기서 0개가 나온다 — 실제로 그렇게
    /// 짜여 있었고, 디스크에 71개가 있는데 `limit=8` 이 7개만 돌려줬다.
    #[test]
    fn a_conversation_buried_under_exec_runs_is_still_found() {
        let r = root("buried");
        let ex = new_format("/tmp/x", "codex_exec", &["스크립트"]);
        let refs: Vec<&str> = ex.iter().map(String::as_str).collect();
        for i in 0..300 {
            write(&r, &format!("2026/08/09/e{i:03}.jsonl"), &refs);
        }
        let tui = new_format("/proj", "codex-tui", &["묻힌 대화"]);
        write(&r, "2026/08/08/real.jsonl", &tui.iter().map(String::as_str).collect::<Vec<_>>());

        let got = recent_codex_sessions_in(&r, 1);
        assert_eq!(got.len(), 1, "exec 더미에 막혀 대화를 못 찾았다");
        assert_eq!(got[0].label, "묻힌 대화");
    }

    #[test]
    fn sessions_without_a_human_turn_are_dropped() {
        // 띄우기만 하고 아무 말도 안 한 세션. 이어갈 게 없는데 목록 상단을
        // 차지하면 진짜 대화가 밀린다.
        let r = root("empty");
        let lines = new_format("/proj", "codex-tui", &[]);
        write(&r, "2026/08/09/d.jsonl", &lines.iter().map(String::as_str).collect::<Vec<_>>());
        assert!(recent_codex_sessions_in(&r, 10).is_empty());
    }
}

/// 최근 agy 대화.
///
/// ⚠️**`conversation_summaries.db` 를 다시 쳐다보지 마라.** 이름·스키마가 딱
/// 여기 필요한 모양(제목·미리보기·시각·워크스페이스)이라 옛 구현이 그걸 읽었는데,
/// **그 테이블은 2026-07-08 에 멈췄다** — 71행뿐이고 title 은 71건 전부 빈 문자열,
/// 60/71 은 workspace 도 없다. 그래서 「제목 없는 옛날 항목 71개」가 뜨고 오늘 것은
/// 하나도 안 뜬다. 죽은 걸 눈치채기 어려운 게, 쿼리는 성공하고 행도 돌아온다.
/// 짝인 `cache/conversation_metadata.json` 도 같은 날 같은 71건에서 멈췄다.
///
/// 살아 있는 정본은 `brain/<uuid>/.system_generated/logs/transcript_full.jsonl`
/// (구버전 대화는 `transcript_full` 없이 `transcript.jsonl` 만 있다). 대화 본문이
/// 통째로 평문 JSONL 이라 `conversations/<uuid>.db` 의 protobuf 를 열 이유가 없다 —
/// 실측으로 brain 105개 == conversations 105개, 차집합 0 이다.
pub fn recent_agy_sessions(limit: usize) -> Vec<RecentSession> {
    let Some(home) = crate::home_dir() else { return Vec::new() };
    recent_agy_sessions_in(&home.join(".gemini/antigravity-cli"), limit)
}

/// 루트를 받는 본체. 테스트가 가짜 brain 을 심어 돌릴 수 있게 갈라 둔다
/// (`recent_codex_sessions_in` 과 같은 이유·같은 모양).
pub fn recent_agy_sessions_in(root: &Path, limit: usize) -> Vec<RecentSession> {
    // stat 만 먼저 하고 상위 limit 개만 파싱한다. 지금은 105개 3.4MB 라 전수를
    // 읽어도 싸지만, 이 디렉터리는 대화마다 늘고 지워지지 않는다.
    let mut files: Vec<(u64, String, PathBuf)> = Vec::new();
    let Ok(rd) = std::fs::read_dir(root.join("brain")) else { return Vec::new() };
    for ent in rd.flatten() {
        let Some(id) = ent.file_name().to_str().map(str::to_string) else { continue };
        let logs = ent.path().join(".system_generated/logs");
        // full 우선. 파일 크기로 신구를 가르면 안 된다 — transcript 쪽은 args 가
        // 이중 인코딩돼 있어 절반은 오히려 full 보다 크다.
        let path = ["transcript_full.jsonl", "transcript.jsonl"]
            .iter()
            .map(|n| logs.join(n))
            .find(|p| p.is_file());
        let Some(path) = path else { continue };
        let Ok(meta) = std::fs::metadata(&path) else { continue };
        files.push((mtime_secs(&meta), id, path));
    }
    files.sort_by(|a, b| b.0.cmp(&a.0));
    files.truncate(limit);

    let ws = agy_workspace_index(root);
    files
        .into_iter()
        .map(|(mtime, id, path)| {
            let rows = read_jsonl(&path);
            let objective = rows.iter().find_map(agy_objective);
            let request = rows.iter().find_map(agy_request);
            let label = objective
                .clone()
                .or_else(|| request.clone())
                .unwrap_or_else(|| id.chars().take(8).collect());
            // 제목으로 이미 쓴 문장이면 같은 줄을 두 번 그리게 되니 비운다.
            let preview = match &request {
                Some(r) if *r != label => r.clone(),
                _ => String::new(),
            };
            // 대화 안에 cwd 전용 필드가 없어 도구가 실제로 쓴 `Cwd` 를 줍는다.
            // ⚠️같은 args 의 `AbsolutePath` 는 쓰면 안 된다 — 파일 경로여서
            // `.../input/slime_00.png` 같은 값이 cwd 칸에 들어간다.
            let cwd = rows
                .iter()
                .find_map(agy_cwd)
                .or_else(|| ws.get(&id).cloned())
                .unwrap_or_default();
            RecentSession { harness: "agy".into(), id, label, mtime, cwd, preview, student: String::new() }
        })
        .collect()
}

/// 줄 단위 JSON 을 읽는다. 깨진 줄은 버린다 — append 도중 잘린 행이 실제로 있고
/// (105개 중 2곳), `?` 로 흘리면 그 파일 하나가 통째로 목록에서 사라진다.
fn read_jsonl(path: &Path) -> Vec<serde_json::Value> {
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// 한 행의 `content` 를 타입으로 걸러 꺼낸다.
fn agy_content<'a>(row: &'a serde_json::Value, kind: &str) -> Option<&'a str> {
    (row.get("type")?.as_str()? == kind).then(|| row.get("content")?.as_str()).flatten()
}

/// agy 가 스스로 붙인 제목. CHECKPOINT 행 안의 `# USER Objective:` 다음 줄이다.
fn agy_objective(row: &serde_json::Value) -> Option<String> {
    let body = agy_content(row, "CHECKPOINT")?;
    let rest = body.split_once("# USER Objective:")?.1;
    clean_label(rest.lines().find(|l| !l.trim().is_empty())?)
}

/// 사용자가 실제로 친 첫 문장. 제목이 없는 대화의 폴백이자 미리보기.
fn agy_request(row: &serde_json::Value) -> Option<String> {
    let body = agy_content(row, "USER_INPUT")?;
    clean_label(body.split_once("<USER_REQUEST>")?.1.split_once("</USER_REQUEST>")?.0)
}

fn clean_label(s: &str) -> Option<String> {
    let out = s.split_whitespace().collect::<Vec<_>>().take_chars(120);
    (!out.is_empty()).then_some(out)
}

fn agy_cwd(row: &serde_json::Value) -> Option<String> {
    row.get("tool_calls")?.as_array()?.iter().find_map(|c| {
        let v = c.get("args")?.get("Cwd")?.as_str()?;
        v.starts_with('/').then(|| v.to_string())
    })
}

/// 도구를 한 번도 안 쓴 대화는 본문에 경로가 없다. agy 가 따로 남기는
/// `{경로: 대화id}` 캐시를 뒤집어 보강한다.
fn agy_workspace_index(root: &Path) -> HashMap<String, String> {
    let Ok(text) = std::fs::read_to_string(root.join("cache/last_conversations.json")) else {
        return HashMap::new();
    };
    let Ok(map) = serde_json::from_str::<HashMap<String, String>>(&text) else {
        return HashMap::new();
    };
    map.into_iter().map(|(path, id)| (id, path)).collect()
}

#[cfg(test)]
mod recent_agy_tests {
    use super::*;
    use serde_json::json;

    fn root(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("kasaterm-agy-test-{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn convo(root: &Path, id: &str, file: &str, lines: &[&str]) {
        let p = root.join("brain").join(id).join(".system_generated/logs").join(file);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, lines.join("\n")).unwrap();
    }

    fn checkpoint(objective: &str) -> String {
        json!({"type": "CHECKPOINT", "content": format!("# USER Objective:\n{objective}\n\n# ...")})
            .to_string()
    }

    fn user_input(req: &str) -> String {
        json!({"type": "USER_INPUT", "content": format!("<USER_REQUEST>\n{req}\n</USER_REQUEST>")})
            .to_string()
    }

    #[test]
    fn objective_wins_and_the_prompt_becomes_the_preview() {
        let r = root("title");
        convo(&r, "id-a", "transcript_full.jsonl", &[&checkpoint("켄지 이름 소개"), &user_input("얘 이름 뭐야")]);
        let got = recent_agy_sessions_in(&r, 10);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].label, "켄지 이름 소개");
        assert_eq!(got[0].preview, "얘 이름 뭐야");
        assert_eq!(got[0].harness, "agy");
        assert_eq!(got[0].id, "id-a");
    }

    #[test]
    fn without_an_objective_the_prompt_is_the_title_and_the_preview_stays_empty() {
        // 제목이 없는 대화가 105건 중 14건 있었다. 프롬프트를 제목으로 올리되
        // 같은 줄을 두 번 그리지 않는지가 요지다.
        let r = root("notitle");
        convo(&r, "id-b", "transcript_full.jsonl", &[&user_input("이거도 팀모드 있나")]);
        let got = recent_agy_sessions_in(&r, 10);
        assert_eq!(got[0].label, "이거도 팀모드 있나");
        assert_eq!(got[0].preview, "");
    }

    #[test]
    fn a_torn_line_does_not_drop_the_conversation() {
        // append 도중 잘린 행이 실제로 있다. 파일 하나가 통째로 사라지면 안 된다.
        let r = root("torn");
        convo(&r, "id-c", "transcript_full.jsonl", &["{\"step_index\":3,\"content\": 중간부터", &checkpoint("살아남기")]);
        assert_eq!(recent_agy_sessions_in(&r, 10)[0].label, "살아남기");
    }

    #[test]
    fn falls_back_to_transcript_when_full_is_missing() {
        // 옛 대화엔 transcript.jsonl 만 있다(105건 중 6건).
        let r = root("fallback");
        convo(&r, "id-d", "transcript.jsonl", &[&checkpoint("옛날 대화")]);
        assert_eq!(recent_agy_sessions_in(&r, 10)[0].label, "옛날 대화");
    }

    #[test]
    fn cwd_comes_from_a_tool_call_never_from_a_file_path() {
        // ⚠️같은 args 의 AbsolutePath 를 주우면 `.../slime_00.png` 가 cwd 칸에 박힌다.
        let r = root("cwd");
        let view = json!({"type": "VIEW_FILE", "tool_calls": [
            {"name": "view_file", "args": {"AbsolutePath": "/Users/kasa/input/slime_00.png"}}]})
            .to_string();
        let run = json!({"type": "RUN_COMMAND", "tool_calls": [
            {"name": "run_command", "args": {"CommandLine": "git log", "Cwd": "/Users/kasa/proj"}}]})
            .to_string();
        convo(&r, "id-e", "transcript_full.jsonl", &[&checkpoint("경로"), &view, &run]);
        assert_eq!(recent_agy_sessions_in(&r, 10)[0].cwd, "/Users/kasa/proj");
    }

    #[test]
    fn the_workspace_cache_fills_in_a_conversation_that_used_no_tools() {
        let r = root("wscache");
        convo(&r, "id-f", "transcript_full.jsonl", &[&checkpoint("도구 안 씀")]);
        std::fs::create_dir_all(r.join("cache")).unwrap();
        std::fs::write(
            r.join("cache/last_conversations.json"),
            json!({"/Users/kasa/other": "id-f"}).to_string(),
        )
        .unwrap();
        assert_eq!(recent_agy_sessions_in(&r, 10)[0].cwd, "/Users/kasa/other");
    }

    #[test]
    fn newest_first_and_the_limit_holds() {
        let r = root("order");
        for (i, id) in ["old", "mid", "new"].iter().enumerate() {
            convo(&r, id, "transcript_full.jsonl", &[&checkpoint(id)]);
            let p = r.join("brain").join(id).join(".system_generated/logs/transcript_full.jsonl");
            let t = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000 + i as u64 * 100);
            std::fs::File::options().write(true).open(&p).unwrap().set_modified(t).unwrap();
        }
        let got = recent_agy_sessions_in(&r, 2);
        assert_eq!(got.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["new", "mid"]);
    }

    #[test]
    fn a_missing_brain_directory_is_empty_not_a_panic() {
        assert!(recent_agy_sessions_in(&root("none"), 10).is_empty());
    }
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
