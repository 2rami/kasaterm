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

/// codex 롤아웃의 **머리**에서 `(model, effort)` 를 집는다 — 앞에서부터 첫
/// `turn_context`. 둘이 같은 줄에 실려 오므로 한 번에 뽑는다.
///
/// board 는 꼬리만 읽는데 codex 의 model 은 `turn_context` 에만 실리고, 그게 파일
/// 앞 **87~122KB** 지점에 있다(실측 2026-08-11, 세션 4개). 그 앞을 채우는 건 지시문
/// 뭉치다. 꼬리를 키워도 소용없다 — 한 턴이 exec 출력째 실려 512KB 를 넘으므로
/// 마지막 `turn_context` 는 끝에서 1MB 넘게 떨어져 있다(같은 날 실측).
///
/// effort 는 재시작 뒤 되살릴 때 쓴다(`-c model_reasoning_effort=…`). model 과 달리
/// 없는 세션이 흔하니 빈 문자열이 정상이고, 그때는 플래그를 아예 안 붙인다.
///
/// 세션 도중 모델을 갈면 첫 값이 낡는다. 그래도 빈칸보다는 낫다 — board 에서 model
/// 은 "무엇이 돌고 있나"를 가르는 칸이라 비면 행 자체가 읽히지 않는다.
pub(crate) fn codex_cfg_from_head(head: &str) -> (String, String) {
    head.lines()
        .find_map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).ok()?;
            if v.get("type")?.as_str()? != "turn_context" {
                return None;
            }
            let p = v.get("payload")?;
            let get = |k: &str| {
                p.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
            };
            let m = get("model");
            // model 이 빈 `turn_context` 는 우리가 찾는 줄이 아니다 — 계속 훑는다.
            (!m.is_empty()).then(|| (m, get("effort")))
        })
        .unwrap_or_default()
}

/// `item.content[]` 의 텍스트 조각을 잇는다.
///
/// ⚠️ 조각의 `type` 으로 거르지 마라 — **대소문자가 갈린다**(실측 2026-08-11:
/// `UserMessage` 는 `"text"`, `AgentMessage` 는 `"Text"`). `text` 필드가 있는
/// 조각을 그대로 잇는 편이 스키마가 또 흔들려도 버틴다.
fn item_text(it: &serde_json::Map<String, serde_json::Value>) -> String {
    let Some(arr) = it.get("content").and_then(|c| c.as_array()) else { return String::new() };
    arr.iter()
        .filter_map(|x| x.get("text").and_then(|t| t.as_str()))
        .collect::<Vec<_>>()
        .join(" ")
}

/// codex 롤아웃 로그(`~/.codex/sessions/**/rollout-*.jsonl`) → board 한 줄.
///
/// 스키마는 거노 머신에서 직접 재서 매핑했다(2026-08-05, 발화는 2026-08-11 재측):
/// - `session_meta` → `cwd` (여기 `context_window` 는 숫자가 아니라 `{window_id}` 다)
/// - `turn_context` → `model`(턴마다 실려 최신이 이긴다)
/// - `event_msg/token_count` → `info.total_token_usage{input,cached_input,cache_write,output}`
///   + `info.model_context_window`(실측 258400) — 창 크기의 유일한 실값
/// - `event_msg/item_completed` → `item.type` 이 `UserMessage`/`AgentMessage` 인 것이
///   마지막 프롬프트/응답. 옛 로그의 `event_msg/user_message`·`agent_message` 도 받는다.
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
                last_prompt = clip(&get_str(p, "message"), 100);
            }
            ("event_msg", "agent_message") if last_reply.is_empty() => {
                last_reply = clip(&get_str(p, "message"), 120);
            }
            // 발화의 새 자리(2026-08-11 실측). 위 두 갈래는 그날 로그에 **한 줄도
            // 없었다** — codex 가 `item_completed` 로 옮겼고, board 의 프롬프트·응답
            // 칸이 그동안 통째로 비어 있었다. 옛 갈래는 지우지 않는다: 이어가는
            // 예전 대화 로그는 여전히 그 모양이다.
            ("event_msg", "item_completed") => {
                let Some(it) = p.get("item").and_then(|i| i.as_object()) else { continue };
                match it.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                    "UserMessage" if last_prompt.is_empty() => {
                        last_prompt = clip(&item_text(it), 100);
                    }
                    "AgentMessage" if last_reply.is_empty() => {
                        last_reply = clip(&item_text(it), 120);
                    }
                    _ => {}
                }
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
        // 도달성은 transcript 로는 알 수 없다 — 명부·프로세스를 봐야 하므로
        // board 조립부(socket.rs)가 채운다. 여기선 비워 둔다.
        reach: String::new(),
        peer_name: None,
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
        detached: false,
    }
}

/// agy 전사본인가 — 모든 줄에 `step_index`(정수)가 있고 `payload` 는 없다.
///
/// claude·codex 어느 쪽과도 안 겹쳐 세 갈래가 깨끗이 갈린다. codex 판정과 같은
/// 이유로 **경로가 아니라 내용**으로 가른다 — 경로로 가르면 bind 쪽까지 고쳐야 하는데
/// 내용 판정은 파서 안에서 끝난다.
fn looks_like_agy(tail: &str) -> bool {
    tail.lines().filter(|l| !l.trim().is_empty()).take(40).any(|l| {
        serde_json::from_str::<serde_json::Value>(l).is_ok_and(|v| {
            v.get("step_index").is_some_and(|s| s.is_number())
                && v.get("payload").is_none()
                && v.get("type").and_then(|t| t.as_str()).is_some_and(|t| !t.is_empty())
        })
    })
}

/// `<TAG>` … `</TAG>` 안쪽만. agy 는 사용자 발화를 태그로 감싸 보내고 그 뒤에
/// `<ADDITIONAL_METADATA>`(로컬 시각)·`<USER_SETTINGS_CHANGE>`(모델 변경 안내)를
/// 덧붙인다 — 안 벗기면 board 프롬프트 칸이 `<USER_REQUEST>` 로 시작하는 한 줄이 된다.
fn tag_body<'a>(s: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    s.split_once(&open)?.1.split_once(&close).map(|(body, _)| body.trim())
}

/// agy 전사본(`brain/<uuid>/.system_generated/logs/transcript_full.jsonl`) → board 한 줄.
///
/// 거노 머신에서 직접 재서 매핑했다(2026-08-11, 대화 105개):
/// - `USER_INPUT.content` → `<USER_REQUEST>` 안쪽이 마지막 프롬프트
/// - `CHECKPOINT.content` 의 `# USER Objective:` 다음 줄 → `title`. agy 가 스스로
///   붙인 세션 제목이라 claude 의 `ai-title` 과 같은 자리다.
/// - `PLANNER_RESPONSE.content` → 마지막 응답. `thinking` 은 별도 필드라 안 섞인다.
/// - `tool_calls[]` → 도구. 라벨 규칙은 codex 와 맞춘다(`exec …`/`view …`/`edit …`)
///   — board 두 종류가 같은 모양으로 읽히는 게 각자 예쁜 것보다 낫다.
///
/// **토큰·비용·컨텍스트는 0 이다.** 전사본에 그 값이 아예 없다 — 105파일 3,698행의
/// 최상위 키를 전수 집계해도 token/usage/cost 계열이 0개다. 유일한 소스는
/// `conversations/<uuid>.db` 의 protobuf 인데 그건 agy 업데이트에 조용히 깨진다.
/// 지어내느니 빈칸으로 둔다(codex 의 `cost_usd` 와 같은 판단).
///
/// **머리를 따로 읽지 않아도 된다.** codex 는 model 이 파일 앞 87~122KB 에 박혀
/// tail 로 영영 못 잡았지만(`codex_model_from_head`), agy 는 전사본 최대가 137KB 라
/// tail 512KB 가 파일 전체를 덮는다. 게다가 모델을 갈 때마다 다시 실려 역순 첫
/// 값이 곧 현재 모델이다.
fn agy_snapshot(surface_id: &str, tail: &str, idle: bool) -> PaneActivity {
    let mut title = String::new();
    let mut model = String::new();
    let mut last_prompt = String::new();
    let mut last_reply = String::new();
    let mut intent = String::new();
    let mut recent_tools: Vec<String> = Vec::new();
    let mut tool_counts: Vec<(String, u32)> = Vec::new();
    let mut changed_files: Vec<String> = Vec::new();

    // 역순 — 채움 필드는 "처음 만나는(=최신)" 것이 이긴다(claude·codex 와 같은 규칙).
    for line in tail.lines().rev() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let kind = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let content = v.get("content").and_then(|c| c.as_str()).unwrap_or("");
        match kind {
            "USER_INPUT" => {
                if last_prompt.is_empty() {
                    if let Some(req) = tag_body(content, "USER_REQUEST") {
                        last_prompt = clip(req, 100);
                    }
                }
                // 모델은 여기 안내문에 실린다. 대화 첫 프롬프트에 `from None to X` 로
                // 항상 찍히고 도중에 갈면 다시 찍히므로, 역순 첫 값이 현재 모델이다.
                if model.is_empty() {
                    model = agy_model_from_settings(content);
                }
            }
            "CHECKPOINT" if title.is_empty() => {
                if let Some(rest) = content.split_once("# USER Objective:") {
                    title = rest.1.lines().find(|l| !l.trim().is_empty()).map(|l| clip(l, 80)).unwrap_or_default();
                }
            }
            "PLANNER_RESPONSE" if last_reply.is_empty() => last_reply = clip(content, 120),
            _ => {}
        }

        let Some(calls) = v.get("tool_calls").and_then(|c| c.as_array()) else { continue };
        for call in calls {
            let Some(name) = call.get("name").and_then(|n| n.as_str()) else { continue };
            let a = |k: &str| {
                call.get("args").and_then(|x| x.get(k)).and_then(|x| x.as_str()).unwrap_or("")
            };
            let label = match name {
                // 첫 명령만·공백 정규화·40자 — codex 의 exec 라벨과 같은 규칙.
                "run_command" => {
                    let first = a("CommandLine").split(['\n', ';', '&']).next().unwrap_or("").trim();
                    let short: String =
                        first.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(40).collect();
                    format!("exec {short}")
                }
                "view_file" => format!("view {}", basename(a("AbsolutePath"))),
                "list_dir" => format!("ls {}", basename(a("DirectoryPath"))),
                "write_to_file" | "replace_file_content" => format!("edit {}", basename(a("TargetFile"))),
                "generate_image" => format!("image {}", basename(a("ImageName"))),
                // ⚠️ args 의 `toolAction` 은 agy 가 써 준 한 문장 설명이라 훨씬 친절하지만
                // 문장이다("Checking the input image file existence"). intent 칸은 짧은
                // 라벨 자리라 안 쓴다 — 담을 칸이 생기면 그때 싣는다.
                other => other.to_string(),
            };
            bump(&mut tool_counts, name);
            if recent_tools.len() < 8 {
                recent_tools.push(label.clone());
            }
            if intent.is_empty() {
                intent = label;
            }
            // 편집한 파일. ⚠️agy 는 제 할일목록·계획서를 `brain/<uuid>/` 안에 같은
            // 도구로 쓴다 — 그건 제 살림이지 이 pane 이 만지는 소스가 아니므로
            // 충돌 감지 신호에서 뺀다.
            let target = a("TargetFile");
            if matches!(name, "write_to_file" | "replace_file_content")
                && target.starts_with('/')
                && !target.contains("/antigravity-cli/brain/")
                && changed_files.len() < 12
                && !changed_files.iter().any(|f| f == target)
            {
                changed_files.push(target.to_string());
            }
        }
    }

    PaneActivity {
        // 도달성·창 번호는 transcript 로 알 수 없다 — 조립부(socket.rs)가 채운다.
        reach: String::new(),
        peer_name: None,
        surface_id: surface_id.to_string(),
        title,
        last_prompt,
        last_reply,
        intent: if intent.is_empty() { "active".into() } else { intent },
        // ⚠️행의 `status`(DONE/RUNNING)로 판정하면 안 된다 — 기록 시점 스냅샷이고
        // **절대 UPDATE 되지 않아서**, 끝난 뒤에도 RUNNING 인 채 굳은 행이 실측 7개다.
        // codex 와 똑같이 mtime idle + 화면 신호로 가른다.
        status: if idle { "idle".into() } else { "working".into() },
        files: Vec::new(),
        screen: None,
        character: None,
        agent_name: None,
        team: None,
        harness: None,
        waiting_for: None,
        tokens_in: 0,
        tokens_out: 0,
        cache_read: 0,
        cache_creation: 0,
        cost_usd: 0.0,
        tool_counts,
        changed_files,
        subagents: Vec::new(),
        subagents_done: Vec::new(),
        background: Vec::new(),
        recent_tools,
        model,
        context_limit: 0,
        context_pct: 0,
        context_tokens: 0,
        // ⚠️전사본에 pane 의 cwd 는 없다. `tool_calls.args.Cwd` 는 **그 명령이 돈
        // 자리**라 agy 제 살림(`~/.gemini/antigravity-cli/scratch`)인 경우가 흔한데,
        // board 의 cwd 는 방을 가르는 키라 거기 넣으면 pane 이 엉뚱한 방에 묶인다.
        // 살아있는 pane 은 조립부가 PTY 에서 진짜 cwd 를 덮으니 여기선 비워 둔다.
        // (세션 레일은 사정이 달라 그 값을 쓴다 — 거긴 유일한 단서다.)
        cwd: String::new(),
        view_cwd: String::new(),
        effort_default: String::new(),
        branch: None,
        window_idx: 0,
        rate_used_pct: None,
        rate_window_minutes: None,
        rate_resets_at: None,
        plan_type: None,
        done_outcome: None,
        done_summary: None,
        done_ago_secs: None,
        detached: false,
    }
}

/// `<USER_SETTINGS_CHANGE>` 안내문에서 사람이 읽는 모델 이름을 꺼낸다.
///
/// 실물: ``The user changed setting `Model Selection` from None to Gemini 3.5
/// Flash (High). No need to comment on this change…``
///
/// ⚠️첫 마침표에서 끊으면 안 된다 — 버전의 점에 걸려 "Gemini 3" 이 된다.
/// 이 라벨이 정확한 값이라는 건 교차검증됐다: 97개 DB 의 표시명과 97/97 일치.
fn agy_model_from_settings(content: &str) -> String {
    let Some(body) = tag_body(content, "USER_SETTINGS_CHANGE") else { return String::new() };
    let Some(after) = body.split_once("`Model Selection`") else { return String::new() };
    let Some((_, rest)) = after.1.split_once(" to ") else { return String::new() };
    // 안내 문장이 이어 붙는다. 문구가 바뀌어도 통째로 싣지 않게 줄·길이로도 막는다.
    let cut = rest
        .split_once(". No need")
        .map(|(m, _)| m)
        .unwrap_or_else(|| rest.lines().next().unwrap_or(""));
    clip(cut.trim().trim_end_matches('.'), 40)
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
    if looks_like_agy(tail) {
        return agy_snapshot(surface_id, tail, idle);
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
        // 도달성은 transcript 로는 알 수 없다 — 명부·프로세스를 봐야 하므로
        // board 조립부(socket.rs)가 채운다. 여기선 비워 둔다.
        reach: String::new(),
        peer_name: None,
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
        detached: false,
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

    /// 새 스키마(2026-08-11 실물 로그). 옛 갈래만 보던 동안 board 의 프롬프트·응답
    /// 칸이 **통째로 비어 있었다** — 조용히 비는 회귀라 아무도 못 알아챈다.
    #[test]
    fn 발화가_item_completed_로_옮겨간_뒤에도_읽는다() {
        let tail = [
            r#"{"timestamp":"t","type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","id":"u1","content":[{"type":"text","text":"그래","text_elements":[]}]}}}"#,
            r#"{"timestamp":"t","type":"event_msg","payload":{"type":"item_completed","item":{"type":"AgentMessage","id":"msg_1","phase":"final_answer","content":[{"type":"Text","text":"연동을\n붙였어요"}]}}}"#,
        ]
        .join("\n");
        let a = snapshot_from_tail("%1", &tail, false);
        assert_eq!(a.last_prompt, "그래");
        // 조각 `type` 이 `text`/`Text` 로 갈리므로 그걸로 걸렀다면 한쪽이 빈다.
        // 줄바꿈은 board 한 줄에 못 들어가니 clip 이 공백으로 편다.
        assert_eq!(a.last_reply, "연동을 붙였어요");
    }

    /// model·effort 는 꼬리가 아니라 **머리**에서, 그것도 같은 줄에서 온다(실측
    /// 87~122KB 지점). 세션 도중 갈아도 **첫** 것을 쓴다 — 뒤엣것은 우리 머리 창에
    /// 없을 수도 있어, 있다 없다 하는 값보다 한 번 정해진 값이 board 에서 덜 헷갈린다.
    #[test]
    fn model_과_effort_는_머리의_첫_turn_context_다() {
        let head = [
            r#"{"timestamp":"t","type":"session_meta","payload":{"cwd":"/repo"}}"#,
            r#"{"timestamp":"t","type":"response_item","payload":{"type":"message","role":"user"}}"#,
            r#"{"timestamp":"t","type":"turn_context","payload":{"model":"gpt-5.5","effort":"high"}}"#,
            r#"{"timestamp":"t","type":"turn_context","payload":{"model":"뒷턴에서-갈아탄-것"}}"#,
        ]
        .join("\n");
        assert_eq!(codex_cfg_from_head(&head), ("gpt-5.5".into(), "high".into()));
        // 아직 첫 턴을 안 쓴 세션 — 빈 문자열이어야 호출부가 "다음에 다시" 로 읽는다.
        let only_meta = r#"{"timestamp":"t","type":"session_meta","payload":{"cwd":"/repo"}}"#;
        assert_eq!(codex_cfg_from_head(only_meta), (String::new(), String::new()));
        assert_eq!(codex_cfg_from_head("깨진 줄\n{}"), (String::new(), String::new()));
        // effort 없는 세션이 흔하다 — model 만 있어도 채택해야 한다(빈 effort 때문에
        // model 까지 버리면 복원이 통째로 기본값으로 떨어진다).
        let no_effort = r#"{"timestamp":"t","type":"turn_context","payload":{"model":"gpt-5.5"}}"#;
        assert_eq!(codex_cfg_from_head(no_effort), ("gpt-5.5".into(), String::new()));
    }

    /// codex 의 답변은 변경 목록째 실려 와 길다 — board 한 줄에 들어가려면 잘려야 한다.
    #[test]
    fn 긴_발화는_board_폭으로_자른다() {
        let long = "가".repeat(300);
        let tail = format!(
            r#"{{"timestamp":"t","type":"event_msg","payload":{{"type":"item_completed","item":{{"type":"AgentMessage","content":[{{"type":"Text","text":"{long}"}}]}}}}}}"#
        );
        let a = snapshot_from_tail("%1", &tail, false);
        assert_eq!(a.last_reply.chars().count(), 121, "120자 + 말줄임");
        assert!(a.last_reply.ends_with('…'));
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


#[cfg(test)]
mod agy_snapshot_tests {
    use super::*;

    /// 실물에서 그대로 뜬 줄들(2026-08-11, `brain/2e7d9059…`·`550915fa…`).
    ///
    /// ⚠️**빈 칸은 조용하다.** codex 가 발화를 `item_completed` 로 옮겼을 때 board 의
    /// 프롬프트·응답 칸이 통째로 비었는데 터지지도 로그가 남지도 않아 아무도 못
    /// 알아챘다(2026-08-11). 실측값을 그대로 박아 두면 스키마가 또 흔들릴 때
    /// 테스트가 대신 비명을 지른다.
    fn real_tail() -> String {
        [
            r##"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","content":"<USER_REQUEST>\n내 이름이 뭐였지?\n</USER_REQUEST>\n<ADDITIONAL_METADATA>\nThe current local time is 2026-08-11.\n</ADDITIONAL_METADATA>\n<USER_SETTINGS_CHANGE>\nThe user changed setting `Model Selection` from None to Gemini 3.6 Flash (Low). No need to comment on this change if the user doesn't ask about it.\n</USER_SETTINGS_CHANGE>"}"##,
            r##"{"step_index":1,"source":"MODEL","type":"CHECKPOINT","status":"DONE","content":"# Conversation\n# USER Objective:\n켄지 이름 소개\n\n# Progress"}"##,
            r##"{"step_index":2,"source":"MODEL","type":"RUN_COMMAND","status":"DONE","content":"Output: ok","tool_calls":[{"name":"run_command","args":{"CommandLine":"git log --oneline\nrm -rf /","Cwd":"/Users/kasa/proj","toolAction":"Checking history"}}]}"##,
            r##"{"step_index":3,"source":"MODEL","type":"CODE_ACTION","status":"RUNNING","content":"","tool_calls":[{"name":"write_to_file","args":{"TargetFile":"/Users/kasa/proj/foo.rs","Overwrite":true}},{"name":"write_to_file","args":{"TargetFile":"/Users/kasa/.gemini/antigravity-cli/brain/2e7d9059/todo_list.md","Overwrite":true}}]}"##,
            r##"{"step_index":4,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","thinking":"**Recalling the name**","content":"켄지 님이라고 하셨습니다!"}"##,
        ]
        .join("\n")
    }

    #[test]
    fn a_real_transcript_fills_every_plaintext_field() {
        let a = agy_snapshot("%9", &real_tail(), false);
        assert_eq!(a.title, "켄지 이름 소개");
        assert_eq!(a.last_prompt, "내 이름이 뭐였지?", "<USER_REQUEST> 를 안 벗기면 태그째 들어온다");
        assert_eq!(a.last_reply, "켄지 님이라고 하셨습니다!");
        assert_eq!(a.model, "Gemini 3.6 Flash (Low)", "버전의 점에서 끊기면 'Gemini 3' 이 된다");
        assert_eq!(a.status, "working");
        // 역순이라 가장 최근 도구가 intent.
        assert_eq!(a.intent, "edit foo.rs");
    }

    #[test]
    fn the_exec_label_stops_at_the_first_command() {
        // codex 의 `exec` 라벨과 같은 규칙 — board 두 종류가 같은 모양으로 읽혀야 한다.
        let a = agy_snapshot("%9", &real_tail(), false);
        assert!(a.recent_tools.contains(&"exec git log --oneline".to_string()), "{:?}", a.recent_tools);
    }

    #[test]
    fn agys_own_artifacts_are_not_reported_as_changed_files() {
        // agy 는 제 할일목록·계획서를 같은 도구로 `brain/<uuid>/` 안에 쓴다. 그건 제
        // 살림이지 이 pane 이 만지는 소스가 아니라, 충돌 감지 신호에 섞이면 안 된다.
        let a = agy_snapshot("%9", &real_tail(), false);
        assert_eq!(a.changed_files, ["/Users/kasa/proj/foo.rs"]);
    }

    #[test]
    fn the_pane_cwd_is_left_to_the_assembler() {
        // args 의 Cwd 는 **그 명령이 돈 자리**다. board 의 cwd 는 방을 가르는 키라
        // 여기에 넣으면 pane 이 엉뚱한 방에 묶인다.
        assert_eq!(agy_snapshot("%9", &real_tail(), false).cwd, "");
    }

    #[test]
    fn a_running_row_does_not_make_the_pane_look_busy() {
        // ⚠️행의 status 는 기록 시점 스냅샷이고 **절대 UPDATE 되지 않는다** — 끝난
        // 뒤에도 RUNNING 인 채 굳은 행이 실측 7개다. 판정은 mtime idle 만 쓴다.
        assert_eq!(agy_snapshot("%9", &real_tail(), true).status, "idle");
    }

    #[test]
    fn the_three_harnesses_route_to_their_own_parser() {
        // 오분류는 조용히 빈 행이 된다 — 세 갈래가 서로를 안 먹는지 못박는다.
        let agy = &real_tail();
        let codex = r##"{"timestamp":"2026-08-11T00:00:00Z","type":"turn_context","payload":{"model":"gpt-5"}}"##;
        let claude = r##"{"type":"user","message":{"role":"user","content":"안녕"}}"##;
        assert!(looks_like_agy(agy) && !looks_like_codex(agy));
        assert!(looks_like_codex(codex) && !looks_like_agy(codex));
        assert!(!looks_like_agy(claude) && !looks_like_codex(claude));
        assert_eq!(snapshot_from_tail("%9", agy, false).title, "켄지 이름 소개");
        assert_eq!(snapshot_from_tail("%9", codex, false).model, "gpt-5");
    }

    #[test]
    fn tokens_and_cost_stay_zero_because_the_plaintext_has_none() {
        // 105파일 3,698행의 최상위 키를 전수 집계해도 token/usage/cost 계열이 0개다.
        // 지어내면 board 에 *틀린 숫자*가 뜬다 — 빈칸이 거짓 금액보다 낫다.
        let a = agy_snapshot("%9", &real_tail(), false);
        assert_eq!((a.tokens_in, a.tokens_out, a.context_pct), (0, 0, 0));
        assert_eq!(a.cost_usd, 0.0);
    }

    #[test]
    fn a_model_line_without_the_trailing_notice_still_parses() {
        // 안내 문구가 바뀌어도 모델명만 남게. 줄·길이로 이중 방어한다.
        let line = r##"{"step_index":0,"type":"USER_INPUT","content":"<USER_REQUEST>\n하이\n</USER_REQUEST>\n<USER_SETTINGS_CHANGE>\nThe user changed setting `Model Selection` from Gemini 3.5 Flash (High) to Claude Opus 4.6 (Thinking)\n</USER_SETTINGS_CHANGE>"}"##;
        assert_eq!(agy_snapshot("%9", line, false).model, "Claude Opus 4.6 (Thinking)");
    }
}

/// `[Image #N]` 이 가리키는 원본 이미지 바이트를 transcript 꼬리에서 되찾는다.
///
/// claude code 는 붙인 그림을 프롬프트에 `[Image #6]` 이라는 **글자로만** 남기고,
/// 진짜 픽셀은 jsonl 에 base64 로 따로 적는다. 그 둘을 잇는 것이 `imagePasteIds`
/// 다 — 화면의 `#6` 과 같은 값이 배열로 적혀 있고, 같은 줄의 image 블록과 순서로
/// 대응한다(실측 2026-08-15: 138건 전부 개수 일치, 다중 20건).
///
/// 줄 모양이 두 가지라 둘 다 본다: 곧바로 보낸 프롬프트는 `type:"user"` 줄의
/// 최상위 `imagePasteIds` + `message.content`, 큐에 넣은 것은 `type:"attachment"`
/// 줄의 `attachment.imagePasteIds` + `attachment.prompt` 에 들어간다.
///
/// **뒤에서부터** 찾는다. 번호는 claude code 를 다시 켤 때마다 1부터 다시 매겨져
/// 한 세션 파일 안에서도 같은 `#1` 이 여러 그림을 가리킨다(실측: 8/12 와 8/13 의
/// `[Image #1]` 이 서로 다른 그림). 화면에 떠 있는 것은 언제나 최근 쪽이다.
pub fn image_paste_bytes(tail: &str, n: u32) -> Option<Vec<u8>> {
    use base64::Engine as _;
    for line in tail.lines().rev() {
        // 값싼 프리체크 — 이 꼬리는 수 MB 고 그중 이미지 줄은 몇 개뿐이라,
        // 전 줄을 serde 에 넣으면 호버 한 번이 수백 ms 가 된다.
        if !line.contains("imagePasteIds") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue; // 꼬리 첫 줄은 중간에서 잘려 있다
        };
        let (ids, blocks) = match v.get("imagePasteIds") {
            Some(ids) => (ids, v.pointer("/message/content")),
            None => (
                v.pointer("/attachment/imagePasteIds")?,
                v.pointer("/attachment/prompt"),
            ),
        };
        let Some(at) = ids
            .as_array()
            .and_then(|a| a.iter().position(|v| v.as_u64() == Some(n as u64)))
        else {
            continue;
        };
        let data = blocks
            .and_then(|b| b.as_array())
            .into_iter()
            .flatten()
            .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("image"))
            .nth(at)
            .and_then(|b| b.pointer("/source/data"))
            .and_then(|d| d.as_str());
        // 번호는 맞는데 픽셀이 없는 줄(파일 참조 등)에서 멈추면 뒤로 더 못 간다 —
        // 계속 훑어 진짜 데이터가 있는 줄을 찾는다.
        if let Some(bytes) = data
            .and_then(|d| base64::engine::general_purpose::STANDARD.decode(d).ok())
            .filter(|b| !b.is_empty())
        {
            return Some(bytes);
        }
    }
    None
}

#[cfg(test)]
mod image_paste_tests {
    use super::*;
    use base64::Engine as _;

    fn b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    /// 곧바로 보낸 프롬프트 줄(type:"user").
    fn user_line(ids: &[u32], payloads: &[&[u8]]) -> String {
        let blocks: Vec<String> = payloads
            .iter()
            .map(|p| {
                format!(
                    r#"{{"type":"image","source":{{"type":"base64","media_type":"image/png","data":"{}"}}}}"#,
                    b64(p)
                )
            })
            .collect();
        format!(
            r#"{{"type":"user","imagePasteIds":{:?},"message":{{"role":"user","content":[{{"type":"text","text":"보기"}},{}]}}}}"#,
            ids,
            blocks.join(",")
        )
    }

    /// 큐에 넣은 프롬프트 줄(type:"attachment").
    fn queued_line(ids: &[u32], payload: &[u8]) -> String {
        format!(
            r#"{{"type":"attachment","attachment":{{"type":"queued_command","imagePasteIds":{:?},"prompt":[{{"type":"text","text":"보기"}},{{"type":"image","source":{{"type":"base64","data":"{}"}}}}]}}}}"#,
            ids,
            b64(payload)
        )
    }

    #[test]
    fn finds_a_typed_prompts_image() {
        let tail = user_line(&[1], &[b"PNGDATA"]);
        assert_eq!(image_paste_bytes(&tail, 1).as_deref(), Some(&b"PNGDATA"[..]));
        assert_eq!(image_paste_bytes(&tail, 2), None);
    }

    #[test]
    fn finds_a_queued_prompts_image() {
        let tail = queued_line(&[6], b"QUEUED");
        assert_eq!(image_paste_bytes(&tail, 6).as_deref(), Some(&b"QUEUED"[..]));
    }

    // 한 줄에 여러 장이면 `imagePasteIds` 의 자리와 image 블록의 자리가 짝이다.
    #[test]
    fn maps_each_id_to_its_own_block() {
        let tail = user_line(&[3, 4], &[b"THREE", b"FOUR"]);
        assert_eq!(image_paste_bytes(&tail, 3).as_deref(), Some(&b"THREE"[..]));
        assert_eq!(image_paste_bytes(&tail, 4).as_deref(), Some(&b"FOUR"[..]));
    }

    // 번호는 claude 를 다시 켤 때마다 1부터라, 같은 파일에 같은 번호가 또 나온다.
    #[test]
    fn later_lines_win_for_a_reused_number() {
        let tail = format!("{}\n{}", user_line(&[1], &[b"OLD"]), user_line(&[1], &[b"NEW"]));
        assert_eq!(image_paste_bytes(&tail, 1).as_deref(), Some(&b"NEW"[..]));
    }

    // 꼬리를 바이트로 자르면 첫 줄은 반드시 깨져 있다.
    #[test]
    fn survives_a_truncated_first_line() {
        let tail = format!("Ids\":[9],\"message\":...\n{}", user_line(&[2], &[b"OK"]));
        assert_eq!(image_paste_bytes(&tail, 2).as_deref(), Some(&b"OK"[..]));
        assert_eq!(image_paste_bytes(&tail, 9), None);
    }

    // 번호만 맞고 픽셀이 없는 줄에서 멈추면 그 뒤(더 오래된 진짜 데이터)를 못 본다.
    #[test]
    fn keeps_looking_past_a_pixelless_line() {
        let empty = r#"{"type":"user","imagePasteIds":[5],"message":{"content":[{"type":"text","text":"없음"}]}}"#;
        let tail = format!("{}\n{}", user_line(&[5], &[b"REAL"]), empty);
        assert_eq!(image_paste_bytes(&tail, 5).as_deref(), Some(&b"REAL"[..]));
    }
}
