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
    let mut cost_usd = 0f64;
    let mut tool_counts: Vec<(String, u32)> = Vec::new();
    let mut changed_files: Vec<String> = Vec::new();
    // 서브에이전트 추적: Task/Agent tool_use(id+desc) 와 완료된 tool_result id 를
    // 따로 모아, 매칭 안 된(=진행 중) 것만 남긴다.
    let mut subagent_uses: Vec<(String, String)> = Vec::new();
    let mut completed_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut model = String::new();
    let mut cwd = String::new();

    for line in tail.lines().rev() {
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue, // tail 첫 줄은 잘렸을 수 있다 — 안전하게 무시.
        };
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
                            let ev = tool_event(name, b.get("input"));
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
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    let mut subagents: Vec<String> = subagent_uses
        .into_iter()
        .filter(|(id, _)| !completed_ids.contains(id))
        .map(|(_, d)| d)
        .collect();
    subagents.dedup();

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
        model: model.clone(),
        context_limit: context_limit_for(&model),
        cwd,
        branch: None,
    }
}

/// 모델명 → 컨텍스트 한도(토큰). 현재 전 Claude 모델 200k 공유(Sonnet 4+ 1M 베타는
/// transcript 가 베타 플래그를 기록 안 해 base 로 본다). 빈 모델 = 0(미상).
fn context_limit_for(model: &str) -> u64 {
    if model.is_empty() { 0 } else { 200_000 }
}

/// jsonl 한 줄을 대화 turn으로. 모니터링에 의미있는 것만 `Some`:
/// - `type:"user"` 이고 content가 **문자열**(사람/오케스트레이터가 타이핑한
///   프롬프트)일 때만. content가 배열이면 tool_result(노이즈)라 버린다.
/// - `type:"assistant"` 의 `text` 블록을 모아 답변으로. tool_use·thinking만
///   있는 turn은 텍스트가 비어 `None`.
pub fn parse_turn(line: &str) -> Option<ConversationTurn> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    match v.get("type").and_then(|t| t.as_str())? {
        "user" => {
            let text = v.pointer("/message/content")?.as_str()?.trim();
            (!text.is_empty()).then(|| ConversationTurn {
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
