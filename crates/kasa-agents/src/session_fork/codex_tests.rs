use super::*;
use serde_json::{json, Value};
use std::time::Duration;

const TID: &str = "019e016a-8ba0-7c80-aaf2-173f6a1a6a11";

fn temp_dir(tag: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("kasa-codex-fork-test-{tag}-{}-{}-{n}", std::process::id(), now_ms()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

/// 0.167 paginated rollout 을 흉내 낸다. 행마다 ordinal 을 단다.
#[derive(Default)]
struct Rollout {
    lines: Vec<String>,
}

impl Rollout {
    fn new() -> Self {
        let mut r = Self::default();
        r.line("session_meta", json!({
            "id": TID, "session_id": TID, "timestamp": "2026-11-10T03:12:45.123Z", "cwd": "/Users/source/secret",
            "originator": "codex_cli_rs", "cli_version": "0.167.0", "source": "cli", "model_provider": "openai",
            "base_instructions": {"text": "BASE SECRET"}, "creator_account_id": "acct-secret",
            "git": {"repository_url": "https://token@github.com/x"}, "history_mode": "paginated",
        }));
        r.item(json!({"type": "message", "role": "developer", "content": [{"type": "input_text", "text": "<permissions instructions>SECRET PERMS</permissions instructions>"}]}));
        r.user("# AGENTS.md instructions for /Users/source\n\n<INSTRUCTIONS>agents secret</INSTRUCTIONS>");
        r.user("<environment_context>\n  <cwd>/Users/source/secret</cwd>\n</environment_context>");
        r.line("turn_context", json!({"cwd": "/Users/source/secret", "approval_policy": "never", "sandbox_policy": {"type": "danger-full-access"}, "model": "gpt-5"}));
        r
    }

    fn line(&mut self, kind: &str, payload: Value) {
        let n = self.lines.len();
        self.lines.push(json!({"timestamp": "2026-11-10T03:13:00.000Z", "type": kind, "payload": payload, "ordinal": n}).to_string());
    }

    fn item(&mut self, payload: Value) {
        self.line("response_item", payload);
    }

    fn event(&mut self, payload: Value) {
        self.line("event_msg", payload);
    }

    fn user(&mut self, text: &str) {
        self.item(json!({"type": "message", "role": "user", "id": "msg_u", "status": "completed", "content": [{"type": "input_text", "text": text}]}));
    }

    fn turn(&mut self, id: &str, prompt: &str, answer: &str) {
        self.event(json!({"type": "task_started", "turn_id": id, "started_at": 1, "model_context_window": 200000, "collaboration_mode_kind": "default"}));
        self.event(json!({"type": "item_completed", "thread_id": TID, "turn_id": id, "item": {"type": "UserMessage", "id": "u", "content": [{"type": "text", "text": prompt, "text_elements": []}]}}));
        self.user(prompt);
        self.item(json!({"type": "reasoning", "id": "rs_1", "summary": [], "encrypted_content": "ENC SECRET"}));
        self.item(json!({"type": "function_call", "id": "fc", "status": "completed", "name": "exec_command", "arguments": "{\"cmd\":\"ls\"}", "call_id": format!("{id}-a"), "internal_chat_message_metadata_passthrough": {"x": 1}}));
        self.item(json!({"type": "function_call_output", "call_id": format!("{id}-a"), "output": "ok"}));
        self.item(json!({"type": "custom_tool_call", "name": "apply_patch", "input": "*** Begin Patch", "call_id": format!("{id}-b"), "status": "completed"}));
        self.item(json!({"type": "custom_tool_call_output", "call_id": format!("{id}-b"), "name": "apply_patch", "output": [{"type": "input_text", "text": "done"}, {"type": "input_image", "image_url": "data:SECRETIMG"}]}));
        self.event(json!({"type": "item_completed", "thread_id": TID, "turn_id": id, "item": {"type": "Reasoning", "id": "r", "summary_text": ["thinking secret"]}}));
        self.item(json!({"type": "message", "role": "assistant", "phase": "final_answer", "content": [{"type": "output_text", "text": answer}]}));
        self.event(json!({"type": "item_completed", "thread_id": TID, "turn_id": id, "item": {"type": "AgentMessage", "id": "a", "phase": "final_answer", "content": [{"type": "Text", "text": answer}]}}));
        self.event(json!({"type": "token_count", "info": null, "rate_limits": {"plan_type": "pro"}}));
        self.line("token_usage_record", json!({"usage": {"input_tokens": 1}}));
        self.event(json!({"type": "task_complete", "turn_id": id, "last_agent_message": answer, "completed_at": 2, "duration_ms": 1}));
    }

    fn running(&mut self, id: &str) {
        self.event(json!({"type": "task_started", "turn_id": id}));
        self.user("둘째 질문");
        self.item(json!({"type": "function_call", "name": "exec_command", "arguments": "{}", "call_id": "running"}));
    }

    fn write(&self, dir: &Path, tail: &str) -> PathBuf {
        let path = dir.join(format!("rollout-2026-11-10T03-12-45-{TID}.jsonl"));
        std::fs::write(&path, self.lines.join("\n") + "\n" + tail).unwrap();
        path
    }
}

fn export(path: &Path, boundary: ForkBoundary<'_>) -> Result<ExportedFork, ForkError> {
    export_native_snapshot("codex", path, TID, boundary, &ExportLimits::default())
}

fn rows(snapshot: &NativeForkSnapshot) -> Vec<Value> {
    snapshot.jsonl.lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

fn resign(snapshot: &NativeForkSnapshot, edit: impl FnOnce(&mut Vec<Value>)) -> NativeForkSnapshot {
    let mut all = rows(snapshot);
    edit(&mut all);
    let jsonl: String = all.iter().map(|r| r.to_string() + "\n").collect();
    NativeForkSnapshot { sha256: sha256_hex(jsonl.as_bytes()), jsonl, ..snapshot.clone() }
}

fn one_turn() -> NativeForkSnapshot {
    let dir = temp_dir("one");
    let mut r = Rollout::new();
    r.turn("turn-1", "첫 질문", "첫 답");
    export(&r.write(&dir, ""), ForkBoundary::LatestComplete).unwrap().snapshot
}

#[test]
fn export_keeps_completed_turn_and_drops_private_material() {
    let dir = temp_dir("export");
    let mut r = Rollout::new();
    r.turn("turn-1", "첫 질문", "첫 답");
    r.running("turn-2");
    let path = r.write(&dir, "{\"type\":\"event_msg\"");
    let fork = export(&path, ForkBoundary::LatestComplete).unwrap();
    let snap = &fork.snapshot;
    assert_eq!((snap.harness.as_str(), snap.boundary_id.as_str(), snap.native_version.as_str()), ("codex", "turn-1", "0.167.0"));
    assert!(snap.boundary_label.contains("첫 질문"), "{}", snap.boundary_label);
    for secret in [
        "BASE SECRET", "acct-secret", "token@", "SECRET PERMS", "AGENTS.md", "agents secret", "environment_context", "/Users/source",
        "ENC SECRET", "SECRETIMG", "danger-full-access", "approval_policy", "rate_limits", "ordinal", "둘째 질문", "running",
        "internal_chat_message_metadata_passthrough", "model_provider", "creator", "paginated", "msg_u", "thinking secret",
    ] {
        assert!(!snap.jsonl.contains(secret), "{secret} 이 남았다: {}", snap.jsonl);
    }
    let kept = rows(snap);
    assert_eq!(kept[0]["payload"], json!({"id": TID, "timestamp": "2026-11-10T03:12:45.123Z", "originator": "codex_cli_rs", "cli_version": "0.167.0", "history_mode": "legacy"}));
    let events: Vec<&str> = kept.iter().filter(|r| r["type"] == "event_msg").map(|r| r["payload"]["type"].as_str().unwrap()).collect();
    assert_eq!(events, ["task_started", "user_message", "agent_message", "task_complete"]);
    assert_eq!(kept.last().unwrap()["payload"]["turn_id"], "turn-1");
    assert_eq!(
        fork.omissions,
        [
            "숨긴 추론(reasoning) 1개",
            "숨긴 추론(화면 Reasoning) 1개",
            "이미지(도구 결과) 1개",
            "주입된 지침·문맥(AGENTS.md) 1개",
            "주입된 지침·문맥(developer·system 메시지) 1개",
            "주입된 지침·문맥(environment_context) 1개",
            "주입된 지침·문맥(session_meta: base_instructions) 1개",
            "계정·자격 정보(session_meta: creator_account_id) 1개",
            "원본 권한·모델 설정(session_meta: model_provider) 1개",
            "원본 권한·모델 설정(turn_context) 1개",
            "원본 작업 경로·저장소 정보(session_meta: cwd) 1개",
            "원본 작업 경로·저장소 정보(session_meta: git) 1개",
            "요청 원문·사용량 기록(event: token_count) 1개",
            "요청 원문·사용량 기록(token_usage_record) 1개",
        ],
        "숨긴 추론·이미지·주입 지침·계정·설정을 하나도 빠뜨리지 않고 센다"
    );
    assert!(fork.source.len_at_read > fork.source.prefix_len);
    assert_eq!(fork.boundary_at_ms, Some(1_794_280_380_000));
}

#[test]
fn chosen_turn_must_be_finished() {
    let dir = temp_dir("chosen");
    let mut r = Rollout::new();
    r.turn("turn-1", "첫 질문", "첫 답");
    r.running("turn-2");
    let path = r.write(&dir, "");
    assert_eq!(export(&path, ForkBoundary::Turn("turn-1")).unwrap().snapshot.boundary_id, "turn-1");
    for turn in ["turn-2", "turn-9"] {
        assert!(matches!(export(&path, ForkBoundary::Turn(turn)), Err(ForkError::Incomplete(_))), "{turn}");
    }
    let mut fresh = Rollout::new();
    fresh.running("turn-1");
    assert!(matches!(export(&fresh.write(&temp_dir("fresh"), ""), ForkBoundary::LatestComplete), Err(ForkError::Incomplete(_))));
}

/// 모델이 보는 공개 이력의 뜻 — 사람·assistant 의 말과 도구 호출·결과를 차례대로. 주입 문맥·지침·추론은 뺀다.
fn public_history(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Ok(row) = serde_json::from_str::<Value>(line) else { continue };
        if row["type"] != "response_item" {
            continue;
        }
        let p = &row["payload"];
        // 그림은 설계상 자리표시로 바뀌므로 원본 쪽도 같은 글로 읽는다.
        let joined = |key: &str| p[key].as_array().into_iter().flatten()
            .filter_map(|c| if c["type"] == "input_image" { Some("[공유 포크에서 이미지 생략]") } else { c["text"].as_str() })
            .collect::<Vec<_>>().join("|");
        match p["type"].as_str() {
            Some("message") if p["role"] == "assistant" => out.push(format!("assistant:{}", joined("content"))),
            Some("message") if p["role"] == "user" => {
                let text = joined("content");
                let private = text.starts_with("# AGENTS.md") || ["<environment_context>", "<skill>", "<user_instructions>"].iter().any(|t| text.starts_with(t));
                if !private {
                    out.push(format!("user:{text}"));
                }
            }
            Some("function_call") => out.push(format!("call:{}:{}:{}", p["call_id"], p["name"], p["arguments"])),
            Some("custom_tool_call") => out.push(format!("custom:{}:{}:{}", p["call_id"], p["name"], p["input"])),
            Some(kind @ ("function_call_output" | "custom_tool_call_output")) => {
                let output = p["output"].as_str().map(str::to_owned).unwrap_or_else(|| joined("output"));
                out.push(format!("{kind}:{}:{output}", p["call_id"]));
            }
            _ => {}
        }
    }
    out
}

/// 압축 전 턴, 압축, 비동기 질문과 답이 든 압축 뒤 턴, 진행 중 턴과 또 한 번의 압축.
fn compacted_rollout() -> Rollout {
    let mut r = Rollout::new();
    r.turn("turn-1", "압축 전 질문", "압축 전 답");
    r.line("compacted", json!({
        "message": "", "window_id": "w2", "resume_metadata": {"secret": 1}, "retained_context": {"user_messages": ["retained secret"]},
        "replacement_history": [
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "압축 전 질문"}]},
            {"type": "message", "role": "developer", "content": [{"type": "input_text", "text": "dev secret"}]},
            {"type": "additional_tools", "role": "developer", "id": "t", "tools": []},
            {"type": "compaction", "id": "c", "encrypted_content": "COMPACT SECRET"},
        ],
    }));
    r.user("<environment_context>\n  <cwd>/Users/source/secret</cwd>\n</environment_context>");
    r.event(json!({"type": "task_started", "turn_id": "turn-2"}));
    r.user("압축 뒤 질문");
    r.item(json!({"type": "function_call", "name": "request_user_input_async", "arguments": "{\"q\":\"어느 쪽?\"}", "call_id": "ask-1"}));
    r.item(json!({"type": "function_call_output", "call_id": "ask-1", "output": "queued"}));
    r.event(json!({"type": "item_completed", "turn_id": "turn-2", "item": {"type": "AgentMessage", "id": "q", "delivery": "async", "questions": [{"id": "x"}], "content": [{"type": "Text", "text": "어느 쪽?"}]}}));
    r.user("<send_user_message_question_reply>왼쪽</send_user_message_question_reply>");
    r.item(json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "왼쪽으로 할게요"}]}));
    r.event(json!({"type": "task_complete", "turn_id": "turn-2", "last_agent_message": "왼쪽으로 할게요"}));
    r.running("turn-3");
    r.line("compacted", json!({"message": "", "replacement_history": []}));
    r
}

#[test]
fn compaction_keeps_the_whole_public_history_through_export_stage_and_fork() {
    let dir = temp_dir("compact");
    let r = compacted_rollout();
    let source = r.write(&dir, "");
    let fork = export(&source, ForkBoundary::LatestComplete).unwrap();
    assert_eq!(fork.snapshot.boundary_id, "turn-2", "진행 중 턴의 압축은 앞선 완료 지점을 지우지 않는다");
    let raw = std::fs::read_to_string(&source).unwrap();
    let parent = public_history(&raw[..fork.boundary_end as usize]);
    assert!(parent.contains(&"user:압축 전 질문".to_string()) && parent.contains(&"assistant:압축 전 답".to_string()));
    assert!(parent.contains(&"user:<send_user_message_question_reply>왼쪽</send_user_message_question_reply>".to_string()));
    assert_eq!(public_history(&fork.snapshot.jsonl), parent, "고정 자료가 공개 이력을 그대로 싣는다");
    for gone in ["\"compacted\"", "COMPACT SECRET", "retained secret", "dev secret", "window_id", "resume_metadata", "environment_context"] {
        assert!(!fork.snapshot.jsonl.contains(gone), "{gone}");
    }
    assert!(fork.omissions.iter().any(|o| o.starts_with("압축 요약(계정에 묶인 암호화 요약") && o.ends_with(" 1개")), "{:?}", fork.omissions);
    assert!(fork.omissions.iter().any(|o| o == "압축 요약(retained_context) 1개"), "{:?}", fork.omissions);

    let fake = Fake::new("compact-fork");
    let staged = stage_codex_snapshot(&fake.home.staging, &fake.home.codex, &identity("compact"), &fork.snapshot, &fake.home.workdir).unwrap();
    assert_eq!(public_history(&std::fs::read_to_string(&staged.rollout).unwrap()), parent);
    let CodexForkLaunch::Launch(plan) = fake.plan(&staged).unwrap() else { panic!("처음엔 띄운다") };
    let result = run_codex_fork(&mut fake.command("ok"), &plan, Duration::from_secs(10)).unwrap();
    let ChildState::Confirmed { transcript, .. } = confirm_codex_child(&plan, &fake.home.codex, Some(&result)) else {
        panic!("child 확인");
    };
    assert_eq!(public_history(&std::fs::read_to_string(transcript).unwrap()), parent, "child 를 읽어도 같은 공개 이력");
}

#[test]
fn oversized_public_history_is_a_clear_limit_error() {
    let dir = temp_dir("oversize");
    let r = compacted_rollout();
    let tight = ExportLimits { max_snapshot_bytes: 2048, ..ExportLimits::default() };
    match export_native_snapshot("codex", &r.write(&dir, ""), TID, ForkBoundary::LatestComplete, &tight) {
        Err(ForkError::Unsupported(why)) => assert!(why.contains("2048") && why.contains("바이트"), "{why}"),
        Err(other) => panic!("한도 오류가 아니다: {other}"),
        Ok(_) => panic!("한도를 넘으면 자르지 않고 거부한다"),
    }
}

#[test]
fn rollouts_that_reference_other_history_are_refused() {
    let dir = temp_dir("refuse");
    for (key, value) in [("history_base", json!({"ordinal": 3})), ("forked_from_ordinal_exclusive", json!(7)), ("history_mode", json!("remote"))] {
        let mut r = Rollout::new();
        let mut meta: Value = serde_json::from_str(&r.lines[0]).unwrap();
        meta["payload"][key] = value;
        r.lines[0] = meta.to_string();
        r.turn("turn-1", "질문", "답");
        assert!(matches!(export(&r.write(&dir, ""), ForkBoundary::LatestComplete), Err(ForkError::Unsupported(_))), "{key}");
    }
    let mut r = Rollout::new();
    r.lines.remove(0);
    r.turn("turn-1", "질문", "답");
    assert!(matches!(export(&r.write(&dir, ""), ForkBoundary::LatestComplete), Err(ForkError::Invalid(_))));
    let mut r = Rollout::new();
    r.turn("turn-1", "질문", "답");
    r.item(json!({"type": "local_shell_call", "call_id": "x"}));
    r.event(json!({"type": "task_started", "turn_id": "turn-2"}));
    r.event(json!({"type": "task_complete", "turn_id": "turn-2", "last_agent_message": null}));
    assert!(matches!(export(&r.write(&dir, ""), ForkBoundary::LatestComplete), Err(ForkError::Unsupported(_))));
}

#[test]
fn codex_verify_refuses_tampered_snapshots() {
    let good = one_turn();
    let limits = ExportLimits::default();
    verify_snapshot(&good, &limits).unwrap();
    let invalid = |s: NativeForkSnapshot| matches!(verify_snapshot(&s, &limits), Err(ForkError::Invalid(_)));
    assert!(invalid(resign(&good, |r| r[0]["payload"]["base_instructions"] = json!({"text": "x"}))));
    assert!(invalid(resign(&good, |r| r[0]["payload"]["cwd"] = json!("/Users/source"))));
    assert!(invalid(resign(&good, |r| r[0]["payload"]["id"] = json!("22222222-2222-4222-8222-222222222222"))));
    assert!(invalid(resign(&good, |r| r[0]["payload"]["history_mode"] = json!("paginated"))));
    assert!(invalid(resign(&good, |r| r.insert(1, json!({"type": "response_item", "payload": {"type": "reasoning", "encrypted_content": "e"}})))));
    assert!(invalid(resign(&good, |r| r.insert(1, json!({"type": "response_item", "payload": {"type": "message", "role": "developer", "content": [{"type": "input_text", "text": "d"}]}})))));
    assert!(invalid(resign(&good, |r| r.insert(1, json!({"type": "response_item", "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "<environment_context>x</environment_context>"}]}})))));
    assert!(invalid(resign(&good, |r| r.insert(1, json!({"type": "event_msg", "payload": {"type": "token_count", "rate_limits": {}}})))));
    assert!(invalid(resign(&good, |r| r.insert(1, json!({"type": "turn_context", "payload": {"approval_policy": "never"}})))));
    assert!(invalid(resign(&good, |r| r[1]["ordinal"] = json!(1))));
    let unpaired = resign(&good, |r| {
        let at = r.len() - 1;
        r.insert(at, json!({"type": "response_item", "payload": {"type": "function_call", "name": "x", "arguments": "{}", "call_id": "open"}}));
    });
    assert!(matches!(verify_snapshot(&unpaired, &limits), Err(ForkError::Incomplete(_))));
    let mut other_turn = good.clone();
    other_turn.boundary_id = "turn-9".into();
    assert!(matches!(verify_snapshot(&other_turn, &limits), Err(ForkError::Incomplete(_))));
    let mut bad_turn = good.clone();
    bad_turn.boundary_id = "../x".into();
    assert!(invalid(bad_turn));
    let mut claude = good.clone();
    claude.harness = "claude".into();
    assert!(matches!(verify_codex_snapshot(&claude, &limits), Err(ForkError::Unsupported(_))));
}

struct Home {
    staging: PathBuf,
    codex: PathBuf,
    workdir: PathBuf,
    parent_copy: PathBuf,
}

/// 수신자 Codex 홈에 원본과 같은 ID 의 기록이 이미 있는 경우도 함께 만든다.
fn home(tag: &str) -> Home {
    let root = temp_dir(tag);
    let workdir = root.join("받는 쪽 작업");
    std::fs::create_dir_all(&workdir).unwrap();
    let day = root.join("codex/sessions/2026/11/09");
    std::fs::create_dir_all(&day).unwrap();
    let parent_copy = day.join(format!("rollout-2026-11-09T00-00-00-{TID}.jsonl"));
    std::fs::write(&parent_copy, "{\"type\":\"session_meta\",\"payload\":{\"id\":\"mine\"}}\n").unwrap();
    Home { staging: root.join("staging"), codex: root.join("codex"), workdir, parent_copy }
}

fn identity(request: &str) -> ForkIdentity {
    ForkIdentity { room_id: "room".into(), session_id: "shared".into(), checkpoint_id: "cp".into(), request_id: request.into() }
}

#[test]
fn staging_rekeys_to_shadow_outside_codex_home() {
    let snap = one_turn();
    let h = home("stage");
    assert!(matches!(stage_codex_snapshot(&h.codex.join("tmp"), &h.codex, &identity("f"), &snap, &h.workdir), Err(ForkError::Invalid(_))));
    let staged = stage_codex_snapshot(&h.staging, &h.codex, &identity("f"), &snap, &h.workdir).unwrap();
    assert_ne!(staged.shadow_thread_id, TID);
    assert!(is_uuid(&staged.shadow_thread_id));
    assert!(staged.rollout.starts_with(&h.staging));
    assert_eq!(staged.rollout.file_name().unwrap().to_str().unwrap(), format!("rollout-2026-11-10T03-12-45-{}.jsonl", staged.shadow_thread_id));
    let text = std::fs::read_to_string(&staged.rollout).unwrap();
    assert!(!text.contains(TID), "원본 ID 는 staging 파일에도 남지 않는다");
    let meta: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    assert_eq!(meta["payload"]["id"], json!(staged.shadow_thread_id));
    assert_eq!(meta["payload"]["session_id"], json!(staged.shadow_thread_id));
    assert_eq!(meta["payload"]["cwd"], json!(h.workdir.to_str().unwrap()));
    let again = stage_codex_snapshot(&h.staging, &h.codex, &identity("f"), &snap, &h.workdir).unwrap();
    assert_eq!(again.shadow_thread_id, staged.shadow_thread_id);
    assert_ne!(stage_codex_snapshot(&h.staging, &h.codex, &identity("g"), &snap, &h.workdir).unwrap().shadow_thread_id, staged.shadow_thread_id);
    let prov = load_provenance(&staged.dir).unwrap().unwrap();
    assert_eq!((prov.harness.as_str(), prov.shadow_session_id.as_deref(), prov.child_session_id.as_deref()), ("codex", Some(staged.shadow_thread_id.as_str()), None));
    std::fs::write(&staged.rollout, "changed").unwrap();
    assert!(matches!(stage_codex_snapshot(&h.staging, &h.codex, &identity("f"), &snap, &h.workdir), Err(ForkError::Conflict(_))));
}

#[test]
fn plan_needs_path_capability_and_carries_no_overrides() {
    let snap = one_turn();
    let h = home("plan");
    let staged = stage_codex_snapshot(&h.staging, &h.codex, &identity("f"), &snap, &h.workdir).unwrap();
    for support in [CodexForkSupport::ThreadIdOnly, CodexForkSupport::Unknown("x".into())] {
        assert!(matches!(codex_fork_plan(&staged, &h.codex, &support), Err(ForkError::Unsupported(_))));
    }
    let CodexForkLaunch::Launch(plan) = codex_fork_plan(&staged, &h.codex, &CodexForkSupport::PathParam).unwrap() else {
        panic!("처음엔 띄운다");
    };
    assert_eq!(plan.initialize_params["capabilities"]["experimentalApi"], json!(true));
    let params = plan.fork_params.as_object().unwrap();
    let mut keys: Vec<&str> = params.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["cwd", "excludeTurns", "lastTurnId", "path", "threadId"]);
    assert_eq!(params["threadId"], json!(staged.shadow_thread_id));
    assert_eq!(params["path"], json!(staged.rollout.to_str().unwrap()));
    assert_eq!(params["lastTurnId"], json!("turn-1"));
}

const FAKE_APP_SERVER: &str = r#"
import json, os, sys, uuid
mode = os.environ["FAKE_MODE"]
home = os.environ["FAKE_CODEX_HOME"]
experimental = False
for line in sys.stdin:
    msg = json.loads(line)
    method = msg.get("method")
    if method == "initialize":
        experimental = bool(msg["params"].get("capabilities", {}).get("experimentalApi"))
        print(json.dumps({"id": msg["id"], "result": {"userAgent": "fake"}}), flush=True)
    elif method == "thread/fork":
        p = msg["params"]
        if mode == "error" or not experimental or not p.get("path"):
            print(json.dumps({"id": msg["id"], "error": {"code": -32600, "message": "path needs experimentalApi"}}), flush=True)
            continue
        if mode == "hang":
            continue
        with open(p["path"], encoding="utf-8") as f:
            lines = f.read().splitlines()
        meta = json.loads(lines[0])
        src = meta["payload"]["id"]
        child = str(uuid.uuid4())
        day = os.path.join(home, "sessions", "2026", "11", "10")
        os.makedirs(day, exist_ok=True)
        path = os.path.join(day, "rollout-2026-11-10T09-00-00-%s.jsonl" % child)
        meta["payload"].update({"id": child, "session_id": child, "forked_from_id": src, "cwd": p["cwd"]})
        with open(path, "w", encoding="utf-8") as f:
            f.write(json.dumps(meta) + "\n" + "\n".join(lines) + "\n")
        if mode == "lost":
            sys.exit(0)
        print(json.dumps({"method": "thread/started", "params": {}}), flush=True)
        print(json.dumps({"id": msg["id"], "result": {"thread": {"id": child, "forkedFromId": src, "path": path}}}), flush=True)
"#;

struct Fake {
    home: Home,
    script: PathBuf,
}

impl Fake {
    fn new(tag: &str) -> Self {
        let home = home(tag);
        let scripts = temp_dir(&format!("{tag}-script"));
        let script = scripts.join("fake_app_server.py");
        std::fs::write(&script, FAKE_APP_SERVER).unwrap();
        Self { home, script }
    }

    fn command(&self, mode: &str) -> std::process::Command {
        let mut cmd = std::process::Command::new("python3");
        cmd.arg("-I").arg(&self.script).env("FAKE_MODE", mode).env("FAKE_CODEX_HOME", &self.home.codex);
        cmd
    }

    fn launch(&self, request: &str) -> (StagedCodexFork, CodexForkPlan) {
        let staged = stage_codex_snapshot(&self.home.staging, &self.home.codex, &identity(request), &one_turn(), &self.home.workdir).unwrap();
        match codex_fork_plan(&staged, &self.home.codex, &CodexForkSupport::PathParam).unwrap() {
            CodexForkLaunch::Launch(plan) => (staged, plan),
            CodexForkLaunch::AlreadyForked(_) => panic!("아직 포크하지 않았다"),
        }
    }

    fn plan(&self, staged: &StagedCodexFork) -> Result<CodexForkLaunch, ForkError> {
        codex_fork_plan(staged, &self.home.codex, &CodexForkSupport::PathParam)
    }
}

#[test]
fn fake_app_server_forks_once_and_retry_reuses_the_child() {
    let fake = Fake::new("fake-ok");
    let parent_before = std::fs::read(&fake.home.parent_copy).unwrap();
    let (staged, plan) = fake.launch("ok");
    let result = run_codex_fork(&mut fake.command("ok"), &plan, Duration::from_secs(10)).unwrap();
    let child = result["thread"]["id"].as_str().unwrap().to_string();
    let ChildState::Confirmed { session_id, transcript } = confirm_codex_child(&plan, &fake.home.codex, Some(&result)) else {
        panic!("child 를 확인해야 한다");
    };
    assert_eq!(session_id, child);
    assert!(transcript.starts_with(&fake.home.codex));
    assert_ne!(child, TID);
    assert_ne!(child, plan.shadow_thread_id);
    let prov = load_provenance(&staged.dir).unwrap().unwrap();
    assert_eq!(prov.child_session_id.as_deref(), Some(child.as_str()));
    assert!(prov.launched_at_ms.is_some());
    match fake.plan(&staged).unwrap() {
        CodexForkLaunch::AlreadyForked(prov) => assert_eq!(prov.child_session_id.as_deref(), Some(child.as_str())),
        CodexForkLaunch::Launch(_) => panic!("재시도는 다시 띄우지 않는다"),
    }
    assert_eq!(std::fs::read(&fake.home.parent_copy).unwrap(), parent_before, "수신자의 같은 ID 기록은 그대로다");
    assert_eq!(sha256_file(&staged.rollout).unwrap(), staged.staged_sha256);
    let wrong = json!({"thread": {"id": child, "forkedFromId": TID}});
    assert!(matches!(confirm_codex_child(&plan, &fake.home.codex, Some(&wrong)), ChildState::Mismatch(_)));
}

#[test]
fn lost_reply_recovers_the_single_child() {
    let fake = Fake::new("fake-lost");
    let (staged, plan) = fake.launch("lost");
    assert!(matches!(run_codex_fork(&mut fake.command("lost"), &plan, Duration::from_secs(10)), Err(ForkError::Incomplete(_))));
    let ChildState::Confirmed { session_id, .. } = confirm_codex_child(&plan, &fake.home.codex, None) else {
        panic!("응답 없이도 shadow 의 child 를 찾는다");
    };
    match fake.plan(&staged).unwrap() {
        CodexForkLaunch::AlreadyForked(prov) => assert_eq!(prov.child_session_id, Some(session_id)),
        CodexForkLaunch::Launch(_) => panic!("child 가 있으면 다시 띄우지 않는다"),
    }
}

#[test]
fn unknown_outcome_blocks_relaunch_until_reset() {
    let fake = Fake::new("fake-hang");
    let (staged, plan) = fake.launch("hang");
    assert!(matches!(run_codex_fork(&mut fake.command("hang"), &plan, Duration::from_millis(800)), Err(ForkError::Incomplete(_))));
    assert_eq!(confirm_codex_child(&plan, &fake.home.codex, None), ChildState::Pending);
    assert!(matches!(fake.plan(&staged), Err(ForkError::Conflict(_))), "결과를 모르면 다시 띄우지 않는다");
    reset_codex_launch(&staged.dir).unwrap();
    assert!(matches!(fake.plan(&staged), Ok(CodexForkLaunch::Launch(_))));
}

#[test]
fn refused_fork_clears_the_launch_mark() {
    let fake = Fake::new("fake-error");
    let (staged, plan) = fake.launch("error");
    assert!(matches!(run_codex_fork(&mut fake.command("error"), &plan, Duration::from_secs(10)), Err(ForkError::Invalid(_))));
    assert!(load_provenance(&staged.dir).unwrap().unwrap().launched_at_ms.is_none());
    assert!(matches!(fake.plan(&staged), Ok(CodexForkLaunch::Launch(_))));

    let mut no_experimental = plan.clone();
    no_experimental.initialize_params["capabilities"]["experimentalApi"] = json!(false);
    assert!(matches!(run_codex_fork(&mut fake.command("ok"), &no_experimental, Duration::from_secs(10)), Err(ForkError::Invalid(_))));
    assert_eq!(confirm_codex_child(&plan, &fake.home.codex, None), ChildState::Pending);
}

#[test]
fn two_children_of_one_shadow_is_a_conflict() {
    let fake = Fake::new("fake-dup");
    let (staged, plan) = fake.launch("dup");
    run_codex_fork(&mut fake.command("ok"), &plan, Duration::from_secs(10)).unwrap();
    reset_codex_launch(&staged.dir).unwrap();
    run_codex_fork(&mut fake.command("lost"), &plan, Duration::from_secs(10)).unwrap_err();
    assert!(matches!(fake.plan(&staged), Err(ForkError::Conflict(_))));
    assert!(matches!(confirm_codex_child(&plan, &fake.home.codex, None), ChildState::Mismatch(_)));
}

#[test]
fn codex_display_lines_drop_meta_reasoning_and_injected_context() {
    let dir = temp_dir("display");
    let mut r = Rollout::new();
    r.turn("turn-1", "보이는 질문", "보이는 답");
    let raw = std::fs::read_to_string(r.write(&dir, "")).unwrap();
    let shown = display_lines("codex", &raw);
    for secret in ["BASE SECRET", "acct-secret", "SECRET PERMS", "AGENTS.md", "environment_context", "ENC SECRET", "SECRETIMG", "danger-full-access", "rate_limits", "ordinal", "session_meta", "thinking secret"] {
        assert!(!shown.contains(secret), "{secret}: {shown}");
    }
    assert!(shown.contains("보이는 질문") && shown.contains("보이는 답"));
    let fork = export(&r.write(&dir, ""), ForkBoundary::LatestComplete).unwrap();
    let last: Value = serde_json::from_str(raw[..fork.boundary_end as usize].trim_end().lines().last().unwrap()).unwrap();
    assert_eq!((last["payload"]["type"].as_str(), last["payload"]["turn_id"].as_str()), (Some("task_complete"), Some("turn-1")), "경계 행까지");
}
