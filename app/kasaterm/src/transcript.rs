//! claude-code transcript(jsonl) → board 스냅샷 추출.
//!
//! claude code는 세션을 `~/.claude/projects/<cwd>/<session>.jsonl`에
//! 실시간으로 append한다. 그 줄들에서 board에 필요한 것만 뽑는다:
//! - `ai-title` → 세션 제목(이 pane이 통째로 뭐 하는지)
//! - `last-prompt` → 마지막 사용자 프롬프트(지금 뭘 시켰나)
//! - `assistant`의 `text` → 최근 답변, `tool_use` → 최근 도구·충돌 파일
//! `thinking` 블록은 항상 redact(빈 문자열)이라 무시한다.
//!
//! 순수 파싱만 둔다 — 파일 tail 읽기·idle 판정은 socket.rs가 들고
//! `snapshot_from_tail`을 호출한다. **상시 폴링 스레드(watcher)는 없다**:
//! board를 부를 때 그 자리에서 transcript tail을 읽어 만든다(pull).

use kasa_socket::backend::{ConversationTurn, PaneActivity};

/// transcript에서 뽑은 한 번의 도구 사용.
#[derive(Clone, Debug)]
pub struct ToolEvent {
    /// intent 표시용 라벨, 예: "Read auth.ts", "Bash cargo build".
    pub label: String,
    /// 충돌 신호용 파일 절대경로. Edit/Write 계열(=실제로 고치는 중)만
    /// `Some` — Read/Grep은 intent엔 보이되 "claimed"는 아니므로 `None`.
    pub file: Option<String>,
}

/// 한 `tool_use` 블록(name+input)을 board 라벨로. `snapshot_from_tail`이
/// 최신 tool_use 하나에 대해 호출한다.
fn tool_event(name: &str, input: Option<&serde_json::Value>) -> ToolEvent {
    let get = |k: &str| input.and_then(|i| i.get(k)).and_then(|v| v.as_str());
    match name {
        "Read" | "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => {
            let path = get("file_path").or_else(|| get("notebook_path")).unwrap_or("");
            let is_edit = matches!(name, "Edit" | "Write" | "MultiEdit" | "NotebookEdit");
            ToolEvent {
                label: format!("{name} {}", basename(path)),
                file: (is_edit && !path.is_empty()).then(|| path.to_string()),
            }
        }
        "Bash" => {
            // 첫 명령만 (cd;build 같은 멀티라인/세미콜론 체인이 board를
            // 줄바꿈으로 더럽히지 않게) + 공백 정규화 + 40자.
            let cmd = get("command").unwrap_or("");
            let first = cmd.split(['\n', ';', '&']).next().unwrap_or("").trim();
            let short: String = first
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(40)
                .collect();
            ToolEvent {
                label: format!("Bash {short}"),
                file: None,
            }
        }
        "Grep" | "Glob" => ToolEvent {
            label: format!("{name} {}", get("pattern").or_else(|| get("path")).unwrap_or("")),
            file: None,
        },
        other => ToolEvent { label: other.to_string(), file: None },
    }
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// 한 줄 텍스트를 board용으로 정규화: 줄바꿈→공백, `max` 글자 초과 시 자르고 `…`.
fn clip(s: &str, max: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > max {
        flat.chars().take(max).collect::<String>() + "…"
    } else {
        flat
    }
}

/// 도구 이름 카운트 누적(tail 윈도 집계용).
fn bump(counts: &mut Vec<(String, u32)>, name: &str) {
    if let Some(e) = counts.iter_mut().find(|(n, _)| n == name) {
        e.1 += 1;
    } else {
        counts.push((name.to_string(), 1));
    }
}

/// 한 assistant turn 의 비용($). 모델명 부분일치로 티어 판정 후 per-million
/// 단가(Claude 4.x) 적용 — 입력/출력/캐시읽기/캐시생성 각각.
fn turn_cost(model: &str, ti: u64, to: u64, cr: u64, cc: u64) -> f64 {
    let (pin, pout, pcr, pcc) = if model.contains("opus") {
        (15.0, 75.0, 1.5, 18.75)
    } else if model.contains("haiku") {
        (0.8, 4.0, 0.08, 1.0)
    } else {
        (3.0, 15.0, 0.3, 3.75) // sonnet 기본
    };
    (ti as f64 * pin + to as f64 * pout + cr as f64 * pcr + cc as f64 * pcc) / 1_000_000.0
}

/// transcript의 **마지막 부분**(socket.rs가 tail 64KB를 잘라 넘김)을 역순으로
/// 1패스 훑어 board 한 줄을 만든다. 각 필드는 **처음 만나는(=최신)** 값에서
/// 채우고, 다 차면 조기 종료한다. `idle`(파일 mtime 기준)은 socket.rs가 판정해
/// 넘긴다 — transcript 자체엔 "막힘/대기" 신호가 없다.
pub fn snapshot_from_tail(surface_id: &str, tail: &str, idle: bool) -> PaneActivity {
    let mut title = String::new();
    let mut last_prompt = String::new();
    let mut last_reply = String::new();
    let mut intent = String::new();
    let mut files: Vec<String> = Vec::new();
    // P3 누적 — usage·도구·변경파일은 tail 윈도 전체 합산이라 조기 종료(break)
    // 없이 끝까지 순회한다. 채움 필드(title/prompt/reply/intent)는 `is_empty`
    // 가드로 여전히 "처음 만나는(=최신)" 값만 잡는다. tail 은 64KB 라 전체 순회도 싸다.
    let mut tokens_in = 0u64;
    let mut tokens_out = 0u64;
    let mut cache_read = 0u64;
    let mut cache_creation = 0u64;
    // context_pct 용 — 합산(throughput)이 아니라 "가장 최근(역순 첫) assistant 턴"의 컨텍스트
    // 점유량. input+cache_read+cache_creation = 그 요청이 실제로 끌어온 컨텍스트 크기(거노:
    // peek 화면 대신 정확한 소스). None = tail 에 usage 있는 assistant 턴이 아직 없음.
    let mut latest_ctx: Option<u64> = None;
    let mut cost_usd = 0f64;
    let mut tool_counts: Vec<(String, u32)> = Vec::new();
    let mut changed_files: Vec<String> = Vec::new();
    // 서브에이전트 추적: Task/Agent tool_use(id+desc) 와 완료된 tool_result id 를
    // 따로 모아, 매칭 안 된(=진행 중) 것만 남긴다.
    let mut subagent_uses: Vec<(String, String)> = Vec::new();
    let mut completed_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    // 백그라운드 셸: run_in_background Bash(tool_use_id→설명) ↔ 런치 응답 shell id
    // (tool_use_id→id) ↔ 완료 통보(<task-id>) 로 in-flight 만 남긴다.
    let mut bg_launch: Vec<(String, String)> = Vec::new();
    let mut bg_shell: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut done_tasks: std::collections::HashSet<String> = std::collections::HashSet::new();
    // 도구 흐름 타임라인 — tail 윈도 최신순 tool_use 라벨(최대 8).
    let mut recent_tools: Vec<String> = Vec::new();
    let mut model = String::new();
    let mut cwd = String::new();

    for line in tail.lines().rev() {
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue, // tail 첫 줄은 잘렸을 수 있다 — 안전하게 무시.
        };
        // 백그라운드 셸/서브에이전트 완료 통보(<task-notification>)의 task-id 수집 —
        // 완료 판정용. 원문 라인을 직접 스캔(어느 메시지 type 에 실리든 잡는다).
        let mut scan = 0usize;
        while let Some(i) = line[scan..].find("<task-id>") {
            let s = scan + i + "<task-id>".len();
            let Some(j) = line[s..].find("</task-id>") else { break };
            done_tasks.insert(line[s..s + j].trim().to_string());
            scan = s + j;
        }
        // cwd 는 user/assistant 줄 최상위에 절대경로로 실린다 — 최신(역순 첫) 1개.
        if cwd.is_empty() {
            if let Some(c) = v.get("cwd").and_then(|x| x.as_str()) {
                cwd = c.to_string();
            }
        }
        match v.get("type").and_then(|t| t.as_str()) {
            Some("ai-title") if title.is_empty() => {
                if let Some(t) = v.get("aiTitle").and_then(|x| x.as_str()) {
                    title = clip(t, 60);
                }
            }
            Some("last-prompt") if last_prompt.is_empty() => {
                if let Some(p) = v.get("lastPrompt").and_then(|x| x.as_str()) {
                    last_prompt = clip(p, 100);
                }
            }
            Some("assistant") => {
                if let Some(u) = v.pointer("/message/usage") {
                    let g = |k: &str| u.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
                    let ti = g("input_tokens");
                    let to = g("output_tokens");
                    let cr = g("cache_read_input_tokens");
                    let cc = g("cache_creation_input_tokens");
                    tokens_in += ti;
                    tokens_out += to;
                    cache_read += cr;
                    cache_creation += cc;
                    if latest_ctx.is_none() {
                        latest_ctx = Some(ti + cr + cc);
                    }
                    let m = v.pointer("/message/model").and_then(|m| m.as_str()).unwrap_or("");
                    cost_usd += turn_cost(m, ti, to, cr, cc);
                    if model.is_empty() && !m.is_empty() {
                        model = m.to_string();
                    }
                }
                let Some(content) = v.pointer("/message/content").and_then(|c| c.as_array()) else {
                    continue;
                };
                for b in content {
                    match b.get("type").and_then(|t| t.as_str()) {
                        Some("text") if last_reply.is_empty() => {
                            if let Some(t) = b.get("text").and_then(|x| x.as_str()) {
                                last_reply = clip(t, 120);
                            }
                        }
                        Some("tool_use") => {
                            let name = b.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
                            bump(&mut tool_counts, name);
                            if name == "Task" || name == "Agent" {
                                let id = b.get("id").and_then(|x| x.as_str()).unwrap_or("");
                                let desc = b
                                    .pointer("/input/description")
                                    .and_then(|x| x.as_str())
                                    .or_else(|| b.pointer("/input/subagent_type").and_then(|x| x.as_str()))
                                    .unwrap_or("subagent");
                                subagent_uses.push((id.to_string(), clip(desc, 40)));
                            }
                            // 백그라운드 런치 — run_in_background Bash(불리언/문자열 "true" 둘 다).
                            let bg = b.pointer("/input/run_in_background");
                            let is_bg = bg.and_then(|x| x.as_bool()).unwrap_or(false)
                                || bg.and_then(|x| x.as_str()).is_some_and(|s| s.eq_ignore_ascii_case("true"));
                            if name == "Bash" && is_bg {
                                let id = b.get("id").and_then(|x| x.as_str()).unwrap_or("");
                                let desc = b
                                    .pointer("/input/description")
                                    .and_then(|x| x.as_str())
                                    .or_else(|| b.pointer("/input/command").and_then(|x| x.as_str()))
                                    .unwrap_or("백그라운드 작업");
                                bg_launch.push((id.to_string(), clip(desc, 40)));
                            }
                            let ev = tool_event(name, b.get("input"));
                            if recent_tools.len() < 8 {
                                recent_tools.push(ev.label.clone());
                            }
                            if let Some(f) = ev.file.clone() {
                                if !changed_files.iter().any(|c| c == &f) {
                                    changed_files.push(f);
                                }
                            }
                            // intent/files 는 기존대로 최신 1개만(충돌 감지용).
                            if intent.is_empty() {
                                intent = ev.label;
                                if let Some(f) = ev.file {
                                    files.push(f);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some("user") => {
                // tool_result 의 tool_use_id 를 모아 "완료된" 서브에이전트를 가린다
                // (Task 호출은 assistant turn, 그 결과는 user turn 에 실린다).
                if let Some(content) = v.pointer("/message/content").and_then(|c| c.as_array()) {
                    for b in content {
                        if b.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                            if let Some(id) = b.get("tool_use_id").and_then(|x| x.as_str()) {
                                completed_ids.insert(id.to_string());
                                // 백그라운드 런치 응답("…running in background with ID: <id>…")
                                // 에서 shell id 추출 → tool_use_id 와 묶는다.
                                let txt = match b.get("content") {
                                    Some(serde_json::Value::String(s)) => s.clone(),
                                    Some(serde_json::Value::Array(a)) => a
                                        .iter()
                                        .filter_map(|x| x.get("text").and_then(|t| t.as_str()))
                                        .collect::<Vec<_>>()
                                        .join(" "),
                                    _ => String::new(),
                                };
                                if let Some(sid) = txt
                                    .split("with ID: ")
                                    .nth(1)
                                    .and_then(|s| s.split(['.', ' ', '\n']).next())
                                    .filter(|s| !s.is_empty())
                                {
                                    bg_shell.insert(id.to_string(), sid.to_string());
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // 서브에이전트: tool_result 매칭 안 된 건 in-flight, 매칭된 건 최근 완료 흔적.
    let mut subagents: Vec<String> = Vec::new();
    let mut subagents_done: Vec<String> = Vec::new();
    for (id, d) in &subagent_uses {
        if completed_ids.contains(id) {
            subagents_done.push(d.clone());
        } else {
            subagents.push(d.clone());
        }
    }
    subagents.dedup();
    subagents_done.dedup();
    subagents_done.truncate(4);
    // 백그라운드: shell id 가 잡혔고 완료 통보(<task-id>)가 아직 없는 것만 in-flight.
    let mut background: Vec<String> = bg_launch
        .iter()
        .filter(|(tid, _)| bg_shell.get(tid).is_some_and(|sid| !done_tasks.contains(sid)))
        .map(|(_, d)| d.clone())
        .collect();
    background.dedup();

    // 컨텍스트 한도·% — K-한도: model 의 [1m] 태그 또는 관측 컨텍스트>200k 면 1M 세션(200k
    // 한도면 그 전에 compact 됨). G: 최신 턴 컨텍스트/한도(>100% 클램프).
    let observed_ctx = latest_ctx.unwrap_or(0);
    let context_limit = context_limit_for(&model, observed_ctx);
    let context_pct = if context_limit > 0 {
        (((observed_ctx as f64 / context_limit as f64) * 100.0).round() as u64).min(100) as u8
    } else {
        0
    };

    PaneActivity {
        surface_id: surface_id.to_string(),
        title,
        last_prompt,
        last_reply,
        intent: if intent.is_empty() { "active".into() } else { intent },
        status: if idle { "idle".into() } else { "working".into() },
        files,
        screen: None,
        // 캐릭터는 transcript 가 아니라 collab 마커 소관 — collab_board 가 채운다.
        character: None,
        // transcript는 permission 대기를 기록하지 않는다 — 화면 peek로만 보인다.
        waiting_for: None,
        tokens_in,
        tokens_out,
        cache_read,
        cache_creation,
        cost_usd,
        tool_counts,
        changed_files,
        is_god: false,
        subagents,
        subagents_done,
        background,
        recent_tools,
        model: model.clone(),
        context_limit,
        // 컨텍스트 % = 최신 assistant 턴 컨텍스트 / 한도(거노: 화면 peek 대신 정확한 transcript
        // 소스). tail 에 usage 가 아직 없으면 0 → socket.rs 가 상태바 파싱으로 폴백.
        context_pct,
        context_tokens: observed_ctx,
        cwd,
        view_cwd: String::new(), // collab_board 가 statusLine report_cwd 보고값으로 채운다.
        effort_default: String::new(), // collab_board 가 settings.json effortLevel 로 채운다.
        branch: None,
        window_idx: 0,
    }
}

/// 모델명+관측 컨텍스트 → 컨텍스트 한도(토큰). transcript 가 1M 베타 플래그를 기록 안 하고
/// model 도 보통 `[1m]` 태그 없이 와서(예 "claude-opus-4-8"), 두 신호로 1M 을 추정한다:
/// ① model 에 `[1m]` 포함 ② 관측 컨텍스트가 200k 초과(200k 한도면 그 전에 compact 됨).
/// 둘 다 아니면 200k. model 미상 + 관측 0 이면 0(미상).
fn context_limit_for(model: &str, observed_ctx: u64) -> u64 {
    if model.is_empty() && observed_ctx == 0 {
        return 0;
    }
    let one_m = model.to_ascii_lowercase().contains("[1m]") || observed_ctx > 200_000;
    if one_m { 1_000_000 } else { 200_000 }
}

/// 하네스/시스템이 user 턴으로 주입하는 합성 메시지(사람이 타이핑한 게 아님) —
/// task-notification·system-reminder·command 출력·tool 오류 재시도 등. 메신저 뷰
/// (대화 탭)에선 노이즈라 버린다. isMeta 플래그가 없는 일부 주입(malformed 재시도)도
/// 잡으려 선두 마커로 한 번 더 거른다.
fn is_injected_user_text(s: &str) -> bool {
    const MARKERS: &[&str] = &[
        "<task-notification>",
        "<system-reminder>",
        "<command-name>",
        "<command-message>",
        "<command-args>",
        "<local-command-stdout>",
        "<local-command-stderr>",
        "<bash-input>",
        "<bash-stdout>",
        "<bash-stderr>",
        "Caveat:",
        "Your tool call was malformed",
        "[Request interrupted",
    ];
    MARKERS.iter().any(|m| s.starts_with(m))
}

/// jsonl 한 줄을 대화 turn으로. 모니터링에 의미있는 것만 `Some`:
/// - `type:"user"` 이고 content가 **문자열**(사람/오케스트레이터가 타이핑한
///   프롬프트)일 때만. content가 배열이면 tool_result(노이즈)라 버린다. 하네스
///   합성 턴(isMeta=true)·시스템 주입 문자열(`is_injected_user_text`)도 버린다.
/// - `type:"assistant"` 의 `text` 블록을 모아 답변으로. tool_use·thinking만
///   있는 turn은 텍스트가 비어 `None`.
pub fn parse_turn(line: &str) -> Option<ConversationTurn> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    // 하네스 합성 턴(isMeta=true: task-notification 등)은 대화가 아니다.
    if v.get("isMeta").and_then(|m| m.as_bool()).unwrap_or(false) {
        return None;
    }
    match v.get("type").and_then(|t| t.as_str())? {
        "user" => {
            let text = v.pointer("/message/content")?.as_str()?.trim();
            (!text.is_empty() && !is_injected_user_text(text)).then(|| ConversationTurn {
                role: "user".into(),
                text: text.to_string(),
            })
        }
        "assistant" => {
            let content = v.pointer("/message/content")?.as_array()?;
            let mut text = String::new();
            for b in content {
                if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                    if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(t);
                    }
                }
            }
            let text = text.trim();
            (!text.is_empty()).then(|| ConversationTurn {
                role: "assistant".into(),
                text: text.to_string(),
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_picks_latest_of_each_field() {
        // 역순 파싱: ai-title·last-prompt·최근 text·최근 tool_use를 각각 집고,
        // tool_result(user array)는 무시한다.
        let tail = [
            r#"{"type":"ai-title","aiTitle":"auth 500 디버깅"}"#,
            r#"{"type":"last-prompt","lastPrompt":"null 체크 넣어줘"}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"auth.ts 보고 있어"},{"type":"tool_use","name":"Edit","input":{"file_path":"/a/auth.ts"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"ok"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"고쳤어"}]}}"#,
        ]
        .join("\n");
        let a = snapshot_from_tail("%1", &tail, false);
        assert_eq!(a.title, "auth 500 디버깅");
        assert_eq!(a.last_prompt, "null 체크 넣어줘");
        assert_eq!(a.last_reply, "고쳤어", "역순 첫 text = 최신 답변");
        assert_eq!(a.intent, "Edit auth.ts");
        assert_eq!(a.files, vec!["/a/auth.ts"]);
        assert_eq!(a.status, "working");
    }

    #[test]
    fn snapshot_tracks_background_subagents_and_timeline() {
        // bg1(sh_aaa) 완료, bg2(sh_bbb) in-flight / 서브 t1 진행·t2 완료 / 타임라인.
        let tail = [
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","id":"toolu_bg1","input":{"command":"cargo build","run_in_background":true}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_bg1","content":[{"type":"text","text":"Command running in background with ID: sh_aaa. Output is being written to: /tmp/sh_aaa.output."}]}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","id":"toolu_bg2","input":{"command":"npm test","run_in_background":true}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_bg2","content":[{"type":"text","text":"Command running in background with ID: sh_bbb. Output..."}]}]}}"#,
            r#"{"type":"user","isMeta":true,"message":{"content":"<task-notification> <task-id>sh_aaa</task-id> completed exit code 0"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Task","id":"toolu_t1","input":{"description":"진행 조사"}}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Task","id":"toolu_t2","input":{"description":"끝난 조사"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_t2","content":"ok"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Edit","input":{"file_path":"/a/x.rs"}}]}}"#,
        ]
        .join("\n");
        let a = snapshot_from_tail("%1", &tail, false);
        assert_eq!(a.background, vec!["npm test"], "sh_bbb 만 in-flight(sh_aaa 완료)");
        assert_eq!(a.subagents, vec!["진행 조사"], "t1 진행 중");
        assert_eq!(a.subagents_done, vec!["끝난 조사"], "t2 완료 흔적");
        assert!(a.recent_tools.contains(&"Edit x.rs".to_string()), "타임라인에 Edit");
        assert!(a.recent_tools.len() >= 4, "여러 tool_use 가 타임라인에");
    }

    #[test]
    fn snapshot_idle_and_truncated_first_line() {
        // 잘린 첫 줄은 무시, idle=true면 status=idle.
        let tail = [
            r#"e":"Edit","input":{"file_path":"/x"}}]}}"#, // 잘린 쓰레기
            r#"{"type":"ai-title","aiTitle":"x"}"#,
        ]
        .join("\n");
        let a = snapshot_from_tail("%2", &tail, true);
        assert_eq!(a.title, "x");
        assert_eq!(a.status, "idle");
        assert_eq!(a.intent, "active", "tool_use 없으면 active");
    }

    #[test]
    fn parse_turn_skips_tool_result_keeps_prompt_and_reply() {
        assert!(parse_turn(r#"{"type":"user","message":{"content":[{"type":"tool_result"}]}}"#).is_none());
        let u = parse_turn(r#"{"type":"user","message":{"content":"고쳐줘"}}"#).unwrap();
        assert_eq!((u.role.as_str(), u.text.as_str()), ("user", "고쳐줘"));
        let a = parse_turn(r#"{"type":"assistant","message":{"content":[{"type":"text","text":"ok"}]}}"#).unwrap();
        assert_eq!((a.role.as_str(), a.text.as_str()), ("assistant", "ok"));
    }

    #[test]
    fn parse_turn_drops_harness_injected_user_turns() {
        // isMeta=true(task-notification 등) 는 사람 대화가 아니라 버린다.
        assert!(parse_turn(
            r#"{"type":"user","isMeta":true,"message":{"content":"<task-notification>\n<task-id>x</task-id>"}}"#
        )
        .is_none());
        // isMeta 없어도 선두 시스템 마커면 버린다(malformed 재시도 등).
        assert!(parse_turn(
            r#"{"type":"user","message":{"content":"Your tool call was malformed and could not be parsed. Please retry."}}"#
        )
        .is_none());
        assert!(parse_turn(
            r#"{"type":"user","message":{"content":"<system-reminder>\nbe nice\n</system-reminder>"}}"#
        )
        .is_none());
        // 진짜 프롬프트는 통과.
        assert!(parse_turn(r#"{"type":"user","message":{"content":"링크주면 충전할게"}}"#).is_some());
    }

    #[test]
    fn snapshot_accumulates_usage_tools_changed() {
        // 두 assistant turn 의 usage 합산 + 도구 카운트 + Edit 변경파일 누적 + 비용.
        let tail = [
            r#"{"type":"assistant","message":{"model":"claude-sonnet-4-6","usage":{"input_tokens":100,"output_tokens":50,"cache_read_input_tokens":10,"cache_creation_input_tokens":20},"content":[{"type":"tool_use","name":"Edit","input":{"file_path":"/a/x.rs"}}]}}"#,
            r#"{"type":"assistant","message":{"model":"claude-sonnet-4-6","usage":{"input_tokens":200,"output_tokens":80},"content":[{"type":"tool_use","name":"Bash","input":{"command":"cargo build"}},{"type":"tool_use","name":"Edit","input":{"file_path":"/a/y.rs"}}]}}"#,
        ]
        .join("\n");
        let a = snapshot_from_tail("%1", &tail, false);
        assert_eq!((a.tokens_in, a.tokens_out, a.cache_read, a.cache_creation), (300, 130, 10, 20));
        assert_eq!(a.tool_counts.iter().find(|(n, _)| n == "Edit").unwrap().1, 2);
        assert_eq!(a.tool_counts.iter().find(|(n, _)| n == "Bash").unwrap().1, 1);
        assert!(a.changed_files.contains(&"/a/x.rs".to_string()));
        assert!(a.changed_files.contains(&"/a/y.rs".to_string()));
        // sonnet 단가: (300*3 + 130*15 + 10*0.3 + 20*3.75) / 1e6
        let expect = (300.0 * 3.0 + 130.0 * 15.0 + 10.0 * 0.3 + 20.0 * 3.75) / 1_000_000.0;
        assert!((a.cost_usd - expect).abs() < 1e-12);
    }
}
