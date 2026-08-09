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

/// codex 롤아웃 로그인가 — 한 줄이라도 `{timestamp,type,payload}` 꼴이면.
///
/// 포맷을 **내용으로** 가른다. 호출부(`socket.rs`)에 종류를 실어 보내는 게 깔끔하지만
/// 그 파일은 지금 다른 작업이 잡고 있고, 무엇보다 tail 만 보고도 확실히 갈린다 —
/// claude jsonl 은 `payload` 키가 없고 codex 는 모든 줄에 있다(실측 2026-08-05).
fn looks_like_codex(tail: &str) -> bool {
    tail.lines().filter(|l| !l.trim().is_empty()).take(40).any(|l| {
        serde_json::from_str::<serde_json::Value>(l).is_ok_and(|v| {
            v.get("payload").is_some_and(|p| p.is_object())
                && v.get("timestamp").is_some()
                && v.get("type").and_then(|t| t.as_str()).is_some_and(|t| {
                    matches!(
                        t,
                        "event_msg" | "response_item" | "turn_context" | "session_meta"
                    )
                })
        })
    })
}

/// codex 롤아웃 로그(`~/.codex/sessions/**/rollout-*.jsonl`) → board 한 줄.
///
/// 스키마는 거노 머신에서 직접 재서 매핑했다(2026-08-05):
/// - `session_meta` → `cwd` (여기 `context_window` 는 숫자가 아니라 `{window_id}` 다)
/// - `turn_context` → `model`(턴마다 실려 최신이 이긴다)
/// - `event_msg/token_count` → `info.total_token_usage{input,cached_input,cache_write,output}`
///   + `info.model_context_window`(실측 258400) — 창 크기의 유일한 실값
/// - `event_msg/user_message` → 마지막 프롬프트, `event_msg/agent_message` → 마지막 응답
///
/// **비용은 0 으로 둔다.** 로그에 단가도 금액도 없다. claude 쪽 `turn_cost` 처럼
/// 단가표를 지어 넣으면 board 에 *틀린 숫자*가 뜬다 — 빈칸이 거짓 금액보다 낫다.
/// 대신 codex 는 `rate_limits.used_percent`/`plan_type` 을 주는데, 구독제라 그쪽이
/// 실제로 알고 싶은 값이다. 담을 칸이 생기면 그때 싣는다.
fn codex_snapshot(surface_id: &str, tail: &str, idle: bool) -> PaneActivity {
    let mut model = String::new();
    let mut cwd = String::new();
    let mut last_prompt = String::new();
    let mut last_reply = String::new();
    let (mut ti, mut to, mut cr, mut cc) = (0u64, 0u64, 0u64, 0u64);
    let mut observed_ctx = 0u64;
    let mut ctx_window = 0u64;
    let mut intent = String::new();
    // 한도 — 창이 여럿이면 **가장 먼저 터질 것**(사용률 최대) 하나만 남긴다.
    let mut rate: Option<(f32, u32, i64)> = None;
    let mut plan_type: Option<String> = None;
    let mut recent_tools: Vec<String> = Vec::new();
    let mut tool_counts: Vec<(String, u32)> = Vec::new();
    // 역순 — 채움 필드는 "처음 만나는(=최신)" 것이 이긴다(claude 경로와 같은 규칙).
    for line in tail.lines().rev() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let Some(p) = v.get("payload").and_then(|p| p.as_object()) else { continue };
        let kind = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let sub = p.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let get_str = |o: &serde_json::Map<String, serde_json::Value>, k: &str| {
            o.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
        };
        match (kind, sub) {
            ("turn_context", _) if model.is_empty() => model = get_str(p, "model"),
            ("session_meta", _) => {
                if cwd.is_empty() {
                    cwd = get_str(p, "cwd");
                }
                // ⚠️ `session_meta.context_window` 는 **숫자가 아니다** — `{window_id: …}`
                // 객체다(실측). 여기서 창 크기를 읽으려 하면 늘 0 이 된다. 진짜 값은
                // `token_count.info.model_context_window`(실측 258400).
            }
            ("event_msg", "user_message") if last_prompt.is_empty() => {
                last_prompt = get_str(p, "message");
            }
            ("event_msg", "agent_message") if last_reply.is_empty() => {
                last_reply = get_str(p, "message");
            }
            ("event_msg", "token_count") => {
                if let Some(rl) = p.get("rate_limits").and_then(|r| r.as_object()) {
                    if plan_type.is_none() {
                        plan_type = rl
                            .get("plan_type")
                            .and_then(|x| x.as_str())
                            .filter(|s| !s.is_empty())
                            .map(str::to_string);
                    }
                    // 실측(78표본)에선 primary 하나뿐이었지만, 창이 늘어도 코드를 안
                    // 고치게 후보를 다 훑어 최대치를 고른다.
                    for key in ["primary", "secondary", "individual_limit"] {
                        let Some(w) = rl.get(key).and_then(|x| x.as_object()) else { continue };
                        let Some(pct) = w.get("used_percent").and_then(|x| x.as_f64()) else {
                            continue;
                        };
                        let cand = (
                            pct as f32,
                            w.get("window_minutes").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
                            w.get("resets_at").and_then(|x| x.as_i64()).unwrap_or(0),
                        );
                        if rate.is_none_or(|(cur, _, _)| cand.0 > cur) {
                            rate = Some(cand);
                        }
                    }
                }
                let Some(info) = p.get("info").and_then(|i| i.as_object()) else { continue };
                if ctx_window == 0 {
                    ctx_window = info
                        .get("model_context_window")
                        .and_then(|x| x.as_u64())
                        .unwrap_or(0);
                }
                // 누적 합은 codex 가 **이미** total 로 준다 — 우리가 더하면 이중 계산이다.
                // 그래서 최신 total 한 번만 쓴다(claude 는 턴별이라 합산이 맞지만 여기선 아니다).
                if ti == 0 && to == 0 {
                    if let Some(t) = info.get("total_token_usage").and_then(|x| x.as_object()) {
                        let n = |k: &str| t.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
                        // ⚠️ codex 의 `input_tokens` 는 **캐시된 것을 이미 포함**한다
                        // (실측: total_tokens = input + output, cached 를 안 더한다).
                        // board 의 in/cache_read 는 claude 규약(둘을 더하면 총 입력)이라
                        // 캐시분을 빼서 맞춘다 — 안 그러면 입력이 이중 계산된다.
                        cr = n("cached_input_tokens");
                        ti = n("input_tokens").saturating_sub(cr);
                        to = n("output_tokens");
                        cc = n("cache_write_input_tokens");
                    }
                }
                // 컨텍스트 점유 = **마지막 요청**이 끌어온 크기(claude 경로와 같은 정의).
                if observed_ctx == 0 {
                    if let Some(l) = info.get("last_token_usage").and_then(|x| x.as_object()) {
                        // `input_tokens` 하나가 곧 그 요청이 끌어온 컨텍스트다. 캐시분을
                        // 더하면 두 배 가까이 부풀어 board 가 "곧 터진다"고 거짓말한다.
                        observed_ctx = l.get("input_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
                    }
                }
            }
            // 도구 호출 — claude 의 `tool_use` 자리. 이름·인자 구조가 달라 라벨은
            // 여기서 만든다(실측: exec_command{cmd,workdir} · view_image{path} ·
            // update_plan{plan[]} · imagegen{prompt}).
            ("response_item", "function_call") => {
                let name = get_str(p, "name");
                if name.is_empty() {
                    continue;
                }
                let args: serde_json::Value = serde_json::from_str(&get_str(p, "arguments"))
                    .unwrap_or(serde_json::Value::Null);
                let a = |k: &str| args.get(k).and_then(|x| x.as_str()).unwrap_or("");
                let label = match name.as_str() {
                    // 첫 명령만·공백 정규화·40자 — claude Bash 라벨과 같은 규칙이라
                    // board 두 종류가 같은 모양으로 읽힌다.
                    "exec_command" => {
                        let first = a("cmd").split(['\n', ';', '&']).next().unwrap_or("").trim();
                        let short: String = first
                            .split_whitespace()
                            .collect::<Vec<_>>()
                            .join(" ")
                            .chars()
                            .take(40)
                            .collect();
                        format!("exec {short}")
                    }
                    "view_image" => format!("view {}", basename(a("path"))),
                    other => other.to_string(),
                };
                bump(&mut tool_counts, &name);
                if recent_tools.len() < 8 {
                    recent_tools.push(label.clone());
                }
                if intent.is_empty() {
                    intent = label;
                }
            }
            _ => {}
        }
    }
    let context_limit = if ctx_window > 0 {
        ctx_window
    } else {
        context_limit_for(&model, observed_ctx)
    };
    // u8 이라 100 을 넘기면 감긴다 — codex 는 compact 전에 한도를 살짝 넘길 수 있어
    // 상한을 건다(0% 로 보이는 것보다 100% 가 맞다).
    let context_pct = if context_limit > 0 {
        (((observed_ctx as f64 / context_limit as f64) * 100.0).round() as u64).min(100) as u8
    } else {
        0
    };
    PaneActivity {
        surface_id: surface_id.to_string(),
        title: String::new(),
        last_prompt,
        last_reply,
        // claude 경로와 같은 규칙 — 비면 "active"(board UI 가 그 값을 숨긴다).
        intent: if intent.is_empty() { "active".into() } else { intent },
        status: if idle { "idle".into() } else { "working".into() },
        files: Vec::new(),
        screen: None,
        character: None,
        agent_name: None,
        team: None,
        // 하네스 종류도 pane 프로세스 소관 — collab_board 가 채운다.
        harness: None,
        waiting_for: None,
        tokens_in: ti,
        tokens_out: to,
        cache_read: cr,
        cache_creation: cc,
        cost_usd: 0.0,
        tool_counts,
        changed_files: Vec::new(),
        subagents: Vec::new(),
        subagents_done: Vec::new(),
        background: Vec::new(),
        recent_tools,
        model,
        context_limit,
        context_pct,
        context_tokens: observed_ctx,
        cwd,
        view_cwd: String::new(),
        effort_default: String::new(),
        branch: None,
        window_idx: 0,
        rate_used_pct: rate.map(|(p, _, _)| p),
        rate_window_minutes: rate.map(|(_, w, _)| w),
        rate_resets_at: rate.map(|(_, _, r)| r),
        plan_type,
        // 완료 보고는 transcript 소관이 아니다 — collab_board(done_reports)가 채운다.
        done_outcome: None,
        done_summary: None,
        done_ago_secs: None,
    }
}

/// transcript의 **마지막 부분**(socket.rs가 tail 64KB를 잘라 넘김)을 역순으로
/// 1패스 훑어 board 한 줄을 만든다. 각 필드는 **처음 만나는(=최신)** 값에서
/// 채우고, 다 차면 조기 종료한다. `idle`(파일 mtime 기준)은 socket.rs가 판정해
/// 넘긴다 — transcript 자체엔 "막힘/대기" 신호가 없다.
///
/// codex 롤아웃 로그면 `codex_snapshot` 으로 넘긴다 — 스키마가 통째로 달라
/// 같은 루프에서 갈래를 치면 두 포맷이 서로를 오염시킨다.
pub fn snapshot_from_tail(surface_id: &str, tail: &str, idle: bool) -> PaneActivity {
    if looks_like_codex(tail) {
        return codex_snapshot(surface_id, tail, idle);
    }
    let mut title = String::new();
    let mut custom_title = String::new();
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
            // `/rename` 은 사람이 직접 붙인 이름이라 자동 생성 제목을 이긴다. 역순
            // 파싱이므로 먼저 만나는 것이 가장 최근 rename 이다(여러 번 바꾸면 마지막).
            Some("custom-title") if custom_title.is_empty() => {
                if let Some(t) = v.get("customTitle").and_then(|x| x.as_str()) {
                    custom_title = clip(t, 60);
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
                            // Monitor 도구도 백그라운드 작업(거노: 유즈 "2 monitors" 누락) — 단
                            // board-watch/wake-watch 는 협업 상시 감시(작업 아님)라 제외.
                            if name == "Monitor" {
                                let cmd = b
                                    .pointer("/input/command")
                                    .and_then(|x| x.as_str())
                                    .unwrap_or("");
                                if !cmd.contains("kasaterm-board-watch") && !cmd.contains("wake-watch") {
                                    let id = b.get("id").and_then(|x| x.as_str()).unwrap_or("");
                                    let desc = b
                                        .pointer("/input/description")
                                        .and_then(|x| x.as_str())
                                        .unwrap_or("모니터");
                                    bg_launch.push((id.to_string(), clip(desc, 40)));
                                }
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
                                // run_in_background Bash("…with ID: <id>.") + Monitor("Monitor
                                // started (task <id>, persistent…") 둘 다에서 런치 id 추출.
                                if let Some(sid) = txt
                                    .split("with ID: ")
                                    .nth(1)
                                    .or_else(|| txt.split("Monitor started (task ").nth(1))
                                    .and_then(|s| s.split(['.', ' ', '\n', ',', ')']).next())
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
        title: if custom_title.is_empty() { title } else { custom_title },
        last_prompt,
        last_reply,
        intent: if intent.is_empty() { "active".into() } else { intent },
        status: if idle { "idle".into() } else { "working".into() },
        files,
        screen: None,
        // 캐릭터는 transcript 가 아니라 collab 마커 소관 — collab_board 가 채운다.
        character: None,
        // 하네스 이름·팀·종류도 pane 프로세스 소관 — collab_board 가 채운다.
        agent_name: None,
        team: None,
        harness: None,
        // transcript는 permission 대기를 기록하지 않는다 — 화면 peek로만 보인다.
        waiting_for: None,
        tokens_in,
        tokens_out,
        cache_read,
        cache_creation,
        cost_usd,
        tool_counts,
        changed_files,
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
        // claude 는 종량제라 이 지표가 없다 — 비용($)이 그 자리를 대신한다.
        rate_used_pct: None,
        rate_window_minutes: None,
        rate_resets_at: None,
        plan_type: None,
        // 완료 보고는 transcript 소관이 아니다 — collab_board(done_reports)가 채운다.
        done_outcome: None,
        done_summary: None,
        done_ago_secs: None,
    }
}

/// 모델명+관측 컨텍스트 → 컨텍스트 한도(토큰). **폴백 전용이다** — 정본은 statusLine 이
/// report_cwd 로 보고하는 하네스 창(socket.rs `reported_ctx`)이고, 이 함수는 그 보고가
/// 아직 없는 프레임에만 쓰인다.
///
/// 추정이 폴백으로 밀려난 이유: transcript 는 1M 베타 플래그를 기록하지 않고 model 에도
/// `[1m]` 이 안 실려(shim 이 `--model 'claude-opus-5[1m]'` 로 줘도 API 응답은
/// `claude-opus-5`), 토큰<200k 인 1M 세션이 통째로 200k 로 잡혔다(실측: 18만 토큰이
/// 92% 빨강 → 200k 를 넘는 순간 20% 로 역주행). fable/mythos 계열만 API 기본이 1M
/// (최대=기본)이라 모델명으로 확정할 수 있고, 나머지는 두 신호로 추정한다:
/// ① model 에 `[1m]` 포함 ② 관측 컨텍스트가 200k 초과(200k 한도면 그 전에 compact 됨).
/// 둘 다 아니면 200k. model 미상 + 관측 0 이면 0(미상).
fn context_limit_for(model: &str, observed_ctx: u64) -> u64 {
    if model.is_empty() && observed_ctx == 0 {
        return 0;
    }
    let m = model.to_ascii_lowercase();
    let one_m = m.contains("fable") || m.contains("mythos") || m.contains("[1m]")
        || observed_ctx > 200_000;
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

#[cfg(test)]
mod codex_tests {
    use super::*;

    /// 거노 머신 실물 로그(2026-08-05, gpt-5.5)에서 뽑은 값 그대로. 스키마 매핑이
    /// 조용히 되돌아가면 board 가 **거짓 숫자**를 내므로 실값으로 못박는다.
    fn sample() -> String {
        [
            r#"{"timestamp":"t","type":"session_meta","payload":{"cwd":"/repo","context_window":{"window_id":"w1"},"session_id":"s1"}}"#,
            r#"{"timestamp":"t","type":"turn_context","payload":{"model":"gpt-5.5","effort":"high","cwd":"/repo"}}"#,
            r#"{"timestamp":"t","type":"event_msg","payload":{"type":"user_message","message":"이거 해줘"}}"#,
            r#"{"timestamp":"t","type":"event_msg","payload":{"type":"agent_message","message":"했습니다"}}"#,
            r#"{"timestamp":"t","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":37822,"cached_input_tokens":22272,"cache_write_input_tokens":0,"output_tokens":1375,"total_tokens":39197},"last_token_usage":{"input_tokens":19055,"cached_input_tokens":17792,"output_tokens":1046,"total_tokens":20101},"model_context_window":258400}}}"#,
        ]
        .join("\n")
    }

    #[test]
    fn codex_로그를_claude_파서로_보내지_않는다() {
        assert!(looks_like_codex(&sample()));
        // 음성 대조군 — claude jsonl 은 payload 가 없다.
        assert!(!looks_like_codex(
            r#"{"type":"assistant","message":{"content":[]}}"#
        ));
    }

    #[test]
    fn 모델과_cwd_를_뽑는다() {
        let a = snapshot_from_tail("%1", &sample(), false);
        assert_eq!(a.model, "gpt-5.5");
        assert_eq!(a.cwd, "/repo");
        assert_eq!(a.last_prompt, "이거 해줘");
        assert_eq!(a.last_reply, "했습니다");
    }

    #[test]
    fn 입력토큰에서_캐시분을_뺀다() {
        // codex 의 input_tokens 는 cached 를 **포함**한다(total = input + output 으로 확인).
        // board 규약은 in + cache_read = 총 입력이라, 안 빼면 입력이 이중 계산된다.
        let a = snapshot_from_tail("%1", &sample(), false);
        assert_eq!(a.cache_read, 22272);
        assert_eq!(a.tokens_in, 37822 - 22272);
        assert_eq!(a.tokens_in + a.cache_read, 37822, "합이 원본 input 이어야 한다");
        assert_eq!(a.tokens_out, 1375);
    }

    #[test]
    fn 컨텍스트는_마지막_요청의_input_하나다() {
        // 캐시분을 더하면 36847 로 부풀어 board 가 "곧 터진다"고 거짓말한다.
        let a = snapshot_from_tail("%1", &sample(), false);
        assert_eq!(a.context_tokens, 19055);
        assert_eq!(a.context_limit, 258400, "창 크기는 token_count 쪽 실값");
        assert_eq!(a.context_pct, 7);
    }

    #[test]
    fn 도구_호출을_intent_와_최근도구에_싣는다() {
        // 실측 인자 구조(exec_command{cmd,workdir} · view_image{path}).
        let tail = [
            r#"{"timestamp":"t","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"cargo build -p kasaterm\",\"workdir\":\"/repo\"}"}}"#,
            r#"{"timestamp":"t","type":"response_item","payload":{"type":"function_call","name":"view_image","arguments":"{\"path\":\"/a/b/shot.png\"}"}}"#,
            r#"{"timestamp":"t","type":"turn_context","payload":{"model":"gpt-5.5"}}"#,
        ]
        .join("\n");
        let a = snapshot_from_tail("%1", &tail, false);
        // 역순 순회라 **가장 나중 호출**이 intent 다.
        assert_eq!(a.intent, "view shot.png");
        assert_eq!(a.recent_tools, vec!["view shot.png", "exec cargo build -p kasaterm"]);
        assert_eq!(a.tool_counts.len(), 2);
    }

    #[test]
    fn 도구가_없으면_intent_는_active() {
        // board UI 가 "active" 를 숨긴다 — 빈 문자열을 넣으면 빈 줄이 생긴다.
        assert_eq!(snapshot_from_tail("%1", &sample(), false).intent, "active");
    }

    #[test]
    fn 한도는_가장_먼저_터질_창을_고른다() {
        // 실측(2026-08-05, 78표본)은 primary 주간창 하나뿐이고 secondary 는 늘 null
        // 이었다. 창이 늘어도 코드를 안 고치게 최대치를 고르는지 함께 잰다.
        let tail = [
            r#"{"timestamp":"t","type":"event_msg","payload":{"type":"token_count","info":{},"rate_limits":{"plan_type":"plus","primary":{"used_percent":3.0,"window_minutes":10080,"resets_at":1786433096},"secondary":{"used_percent":62.5,"window_minutes":300,"resets_at":1786400000}}}}"#,
        ]
        .join("\n");
        let a = snapshot_from_tail("%1", &tail, false);
        assert_eq!(a.rate_used_pct, Some(62.5), "주간 3% 가 아니라 5시간 62.5% 가 먼저 터진다");
        assert_eq!(a.rate_window_minutes, Some(300));
        assert_eq!(a.rate_resets_at, Some(1786400000));
        assert_eq!(a.plan_type.as_deref(), Some("plus"));
    }

    #[test]
    fn 실측_한도는_주간창_하나다() {
        let tail = r#"{"timestamp":"t","type":"event_msg","payload":{"type":"token_count","info":{},"rate_limits":{"plan_type":"plus","primary":{"used_percent":3.0,"window_minutes":10080,"resets_at":1786433096},"secondary":null}}}"#;
        let a = snapshot_from_tail("%1", tail, false);
        assert_eq!(a.rate_used_pct, Some(3.0));
        assert_eq!(a.rate_window_minutes, Some(10080), "10080분 = 7일");
    }

    #[test]
    fn 비용은_지어내지_않는다() {
        // 로그에 단가도 금액도 없다 — 빈칸이 거짓 금액보다 낫다.
        assert_eq!(snapshot_from_tail("%1", &sample(), false).cost_usd, 0.0);
    }
}
