use super::*;
use serde_json::{json, Value};

const PARENT: &str = "11111111-1111-4111-8111-111111111111";

fn uid(n: u32) -> String {
    format!("00000000-0000-4000-8000-{n:012}")
}

fn temp_dir(tag: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("kasa-fork-test-{tag}-{}-{}-{n}", std::process::id(), now_ms()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

/// 원본 claude transcript 를 흉내 낸다. 행마다 원본 경로·버전·세션을 claude 처럼 단다.
#[derive(Default)]
struct Tx {
    lines: Vec<String>,
    last: Option<String>,
    n: u32,
}

impl Tx {
    fn chain(&mut self, mut row: Value, parent: Option<String>) -> String {
        self.n += 1;
        let id = uid(self.n);
        row["uuid"] = json!(id);
        row["parentUuid"] = json!(parent);
        row["sessionId"] = json!(PARENT);
        row["timestamp"] = json!("2026-11-10T03:12:45.123Z");
        row["cwd"] = json!("/Users/source/secret-project");
        row["gitBranch"] = json!("main");
        row["version"] = json!("2.1.319");
        row["isSidechain"] = json!(false);
        row["userType"] = json!("external");
        self.lines.push(row.to_string());
        self.last = Some(id.clone());
        id
    }

    fn push(&mut self, row: Value) -> String {
        let parent = self.last.clone();
        self.chain(row, parent)
    }

    fn user(&mut self, text: &str) -> String {
        self.push(json!({"type": "user", "message": {"role": "user", "content": text}, "permissionMode": "bypassPermissions", "promptId": "p"}))
    }

    fn assistant(&mut self, msg: &str, block: Value, stop: Option<&str>) -> String {
        self.push(json!({
            "type": "assistant",
            "requestId": "req_secret",
            "message": {
                "id": msg, "type": "message", "role": "assistant", "model": "claude-opus-5-5",
                "content": [block], "stop_reason": stop, "stop_sequence": null,
                "container": {"id": "container_secret"},
                "usage": {"input_tokens": 10, "output_tokens": 5, "service_tier": "priority"},
                "context_management": null,
            }
        }))
    }

    fn text(&mut self, msg: &str, text: &str) -> String {
        self.assistant(msg, json!({"type": "text", "text": text}), Some("end_turn"))
    }

    fn thinking(&mut self, msg: &str, stop: Option<&str>) -> String {
        self.assistant(msg, json!({"type": "thinking", "thinking": "", "signature": "sig_secret"}), stop)
    }

    fn tool_use(&mut self, msg: &str, id: &str) -> String {
        self.assistant(msg, json!({"type": "tool_use", "id": id, "name": "Bash", "input": {"command": "ls"}}), Some("tool_use"))
    }

    fn tool_result(&mut self, id: &str, content: Value) -> String {
        self.push(json!({
            "type": "user",
            "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": content}]},
            "toolUseResult": {"stdout": "full output with /Users/source/secret-project"},
            "sourceToolAssistantUUID": self.last.clone(),
        }))
    }

    fn raw(&mut self, row: Value) {
        self.lines.push(row.to_string());
    }

    fn write(&self, dir: &Path, tail: &str) -> PathBuf {
        let path = dir.join(format!("{PARENT}.jsonl"));
        std::fs::write(&path, self.lines.join("\n") + "\n" + tail).unwrap();
        path
    }
}

fn export(path: &Path, boundary: ForkBoundary<'_>) -> Result<ExportedFork, ForkError> {
    export_native_snapshot("claude", path, PARENT, boundary, &ExportLimits::default())
}

fn rows(snapshot: &NativeForkSnapshot) -> Vec<Value> {
    snapshot.jsonl.lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

fn resign(snapshot: &NativeForkSnapshot, jsonl: String) -> NativeForkSnapshot {
    let mut out = snapshot.clone();
    out.sha256 = sha256_hex(jsonl.as_bytes());
    out.jsonl = jsonl;
    out
}

fn edit_rows(snapshot: &NativeForkSnapshot, edit: impl FnOnce(&mut Vec<Value>)) -> NativeForkSnapshot {
    let mut all = rows(snapshot);
    edit(&mut all);
    resign(snapshot, all.iter().map(|r| r.to_string() + "\n").collect())
}

/// 완료된 첫 턴, 도구를 쓰다 만 둘째 턴, 쓰다 만 마지막 행.
fn two_turns(dir: &Path) -> (PathBuf, String) {
    let mut tx = Tx::default();
    tx.user("첫 질문");
    tx.thinking("m1", Some("end_turn"));
    let end = tx.text("m1", "첫 답");
    tx.user("둘째 질문");
    tx.tool_use("m2", "toolu_2");
    (tx.write(dir, "{\"type\":\"assistant\",\"uuid\":\"partial"), end)
}

#[test]
fn latest_boundary_stops_before_running_tool_and_partial_line() {
    let dir = temp_dir("latest");
    let (path, end) = two_turns(&dir);
    let fork = export(&path, ForkBoundary::LatestComplete).unwrap();
    assert_eq!(fork.snapshot.boundary_id, end);
    assert_eq!(fork.snapshot.native_version, "2.1.319");
    assert!(fork.snapshot.boundary_label.contains("첫 질문"), "{}", fork.snapshot.boundary_label);
    let kept = rows(&fork.snapshot);
    assert_eq!(kept.len(), 2, "질문과 답만 남는다: {}", fork.snapshot.jsonl);
    assert!(!fork.snapshot.jsonl.contains("둘째 질문"));
    assert!(fork.omissions.iter().any(|o| o == "숨긴 추론(thinking) 1개"), "{:?}", fork.omissions);
    assert!(fork.source.len_at_read > fork.source.prefix_len, "쓰다 만 행은 고정 범위 밖이다");
    let prefix = std::fs::read(&path).unwrap()[..fork.source.prefix_len as usize].to_vec();
    assert_eq!(sha256_hex(&prefix), fork.source.prefix_sha256);
    assert_eq!(fork.boundary_at_ms, Some(1_794_280_365_123));
}

#[test]
fn chosen_boundary_must_end_a_completed_turn() {
    let dir = temp_dir("chosen");
    let mut tx = Tx::default();
    tx.user("질문");
    let thinking = tx.thinking("m1", Some("end_turn"));
    let end = tx.text("m1", "답");
    tx.user("다음");
    let running = tx.tool_use("m2", "toolu_2");
    let in_progress = tx.thinking("m3", None);
    let path = tx.write(&dir, "");
    for (uuid, why) in [(thinking.as_str(), "같은 메시지가 이어진다"), (running.as_str(), "도구 결과 전"), (in_progress.as_str(), "쓰는 중")] {
        assert!(matches!(export(&path, ForkBoundary::Turn(uuid)), Err(ForkError::Incomplete(_))), "{why}");
    }
    assert!(matches!(export(&path, ForkBoundary::Turn(&uid(999))), Err(ForkError::Invalid(_))));
    assert_eq!(export(&path, ForkBoundary::Turn(&end)).unwrap().snapshot.boundary_id, end);

    let mut only_running = Tx::default();
    only_running.user("질문");
    only_running.tool_use("m1", "toolu_1");
    let path = only_running.write(&dir, "");
    assert!(matches!(export(&path, ForkBoundary::LatestComplete), Err(ForkError::Incomplete(_))));
}

#[test]
fn private_rows_and_fields_never_reach_the_snapshot() {
    let dir = temp_dir("private");
    let mut tx = Tx::default();
    tx.raw(json!({"type": "session-owner", "accountUuid": "acct-secret", "organizationUuid": "org-secret", "sessionId": PARENT}));
    tx.raw(json!({"type": "api-request", "id": "r1", "base": "SYSTEM PROMPT secret", "sessionId": PARENT}));
    tx.raw(json!({"type": "permission-mode", "permissionMode": "bypassPermissions", "sessionId": PARENT}));
    tx.push(json!({"type": "attachment", "attachment": {"type": "instructions", "content": "PRIVATE CLAUDE.md"}}));
    tx.push(json!({"type": "attachment", "attachment": {"type": "credential_org", "org": "org-secret"}}));
    tx.push(json!({"type": "user", "isMeta": true, "message": {"role": "user", "content": "skill body secret"}}));
    tx.push(json!({"type": "user", "message": {"role": "user", "content": [
        {"type": "text", "text": "보이는 질문<system-reminder>hook context secret</system-reminder>"},
        {"type": "image", "source": {"type": "base64", "data": "IMAGEDATA"}},
    ]}}));
    tx.thinking("m1", Some("tool_use"));
    tx.tool_use("m1", "toolu_1");
    tx.tool_result("toolu_1", json!([
        {"type": "text", "text": "결과<system-reminder>malware note</system-reminder>"},
        {"type": "image", "source": {"type": "base64", "data": "SCREENSHOT"}},
    ]));
    tx.push(json!({"type": "system", "subtype": "stop_hook_summary", "hookInfos": ["secret hook"]}));
    let end = tx.text("m2", "끝");
    let path = tx.write(&dir, "");

    let fork = export(&path, ForkBoundary::LatestComplete).unwrap();
    for secret in [
        "acct-secret", "org-secret", "SYSTEM PROMPT", "bypassPermissions", "PRIVATE CLAUDE.md", "skill body", "hook context",
        "malware note", "IMAGEDATA", "SCREENSHOT", "secret-project", "req_secret", "container_secret", "service_tier", "sig_secret",
        "thinking", "toolUseResult", "sourceToolAssistantUUID", "gitBranch", "userType", "secret hook", "\"cwd\"",
    ] {
        assert!(!fork.snapshot.jsonl.contains(secret), "{secret} 이 남았다: {}", fork.snapshot.jsonl);
    }
    let kept = rows(&fork.snapshot);
    assert_eq!(kept.len(), 4, "질문·도구 호출·도구 결과·답: {}", fork.snapshot.jsonl);
    assert_eq!(kept[0]["parentUuid"], Value::Null, "빠진 행 대신 사슬을 다시 잇는다");
    assert_eq!(kept.last().unwrap()["uuid"], json!(end));
    assert_eq!(
        fork.omissions,
        [
            "숨긴 추론(thinking) 1개",
            "이미지(도구 결과) 1개",
            "이미지(사용자) 1개",
            "주입된 지침·문맥(attachment: instructions) 1개",
            "주입된 지침·문맥(isMeta 사용자 행) 1개",
            "주입된 지침·문맥(system-reminder) 2개",
            "계정·자격 정보(attachment: credential_org) 1개",
            "계정·자격 정보(session-owner) 1개",
            "원본 권한·모델 설정(permission-mode) 1개",
            "원본 작업 경로·저장소 정보(cwd·gitBranch) 9개",
            "요청 원문·사용량 기록(api-request) 1개",
            "화면용 기록(system: stop_hook_summary) 1개",
            "화면용 기록(도구 상세 toolUseResult) 1개",
        ],
        "숨긴 추론·이미지·주입 지침·계정·요청 원문을 하나도 빠뜨리지 않고 센다"
    );
}

#[test]
fn unknown_blocks_are_refused_not_dropped() {
    let dir = temp_dir("unknown");
    let mut tx = Tx::default();
    tx.user("검색해 줘");
    tx.assistant("m1", json!({"type": "server_tool_use", "id": "srv_1", "name": "web_search", "input": {}}), Some("end_turn"));
    tx.text("m1", "끝");
    let path = tx.write(&dir, "");
    assert!(matches!(export(&path, ForkBoundary::LatestComplete), Err(ForkError::Unsupported(_))));
}

#[test]
fn chain_starts_at_last_compaction_and_skips_rewound_branch() {
    let dir = temp_dir("chain");
    let mut tx = Tx::default();
    tx.user("압축 전 질문");
    let before = tx.text("m0", "압축 전 답");
    let boundary = tx.chain(
        json!({"type": "system", "subtype": "compact_boundary", "content": "Conversation compacted", "level": "info",
               "logicalParentUuid": before, "compactMetadata": {"trigger": "auto", "preTokens": 150000, "secret": 1}}),
        None,
    );
    tx.push(json!({"type": "user", "isCompactSummary": true, "isVisibleInTranscriptOnly": true, "message": {"role": "user", "content": "요약"}}));
    tx.user("A");
    let first = tx.text("m1", "A 답");
    tx.user("버린 가지");
    tx.text("m2", "버린 답");
    tx.last = Some(first);
    tx.user("되감은 뒤");
    let end = tx.text("m3", "새 답");
    let path = tx.write(&dir, "");

    let fork = export(&path, ForkBoundary::LatestComplete).unwrap();
    let kept = rows(&fork.snapshot);
    assert_eq!(kept[0]["uuid"], json!(boundary));
    assert_eq!(kept[0]["compactMetadata"], json!({"trigger": "auto", "preTokens": 150000}));
    assert!(kept[0].get("logicalParentUuid").is_none());
    assert_eq!(kept[1]["isCompactSummary"], json!(true));
    for gone in ["압축 전", "버린"] {
        assert!(!fork.snapshot.jsonl.contains(gone), "{gone}");
    }
    assert_eq!(kept.last().unwrap()["uuid"], json!(end));
    assert_eq!(kept.len(), 6);
}

#[test]
fn broken_chain_or_foreign_session_is_refused() {
    let dir = temp_dir("broken");
    let mut tx = Tx::default();
    tx.last = Some(uid(500));
    tx.user("부모가 없는 질문");
    tx.text("m1", "답");
    let path = tx.write(&dir, "");
    assert!(matches!(export(&path, ForkBoundary::LatestComplete), Err(ForkError::Invalid(_))));

    let other = dir.join("22222222-2222-4222-8222-222222222222.jsonl");
    std::fs::copy(&path, &other).unwrap();
    let result = export_native_snapshot("claude", &other, "22222222-2222-4222-8222-222222222222", ForkBoundary::LatestComplete, &ExportLimits::default());
    assert!(matches!(result, Err(ForkError::Invalid(_))), "다른 세션의 행이다");
    assert!(matches!(export_native_snapshot("claude", &path, "not-a-uuid", ForkBoundary::LatestComplete, &ExportLimits::default()), Err(ForkError::Invalid(_))));
    assert!(matches!(export_native_snapshot("codex", &path, PARENT, ForkBoundary::LatestComplete, &ExportLimits::default()), Err(ForkError::Invalid(_))), "claude 기록은 codex rollout 이 아니다");
    let tight = ExportLimits { max_source_bytes: 10, ..ExportLimits::default() };
    assert!(matches!(export_native_snapshot("claude", &path, PARENT, ForkBoundary::LatestComplete, &tight), Err(ForkError::Unsupported(_))));
}

#[cfg(unix)]
#[test]
fn export_leaves_source_bytes_mtime_and_process_alone() {
    let dir = temp_dir("source");
    let (path, _) = two_turns(&dir);
    let before = (std::fs::read(&path).unwrap(), std::fs::metadata(&path).unwrap().modified().unwrap());
    let mut source = std::process::Command::new("sleep").arg("30").spawn().unwrap();
    export(&path, ForkBoundary::LatestComplete).unwrap();
    let after = (std::fs::read(&path).unwrap(), std::fs::metadata(&path).unwrap().modified().unwrap());
    let alive = source.try_wait().unwrap().is_none();
    let _ = source.kill();
    let _ = source.wait();
    assert_eq!(before, after);
    assert!(alive, "원본 프로세스를 건드리지 않는다");
}

#[test]
fn appended_source_keeps_the_same_snapshot() {
    let dir = temp_dir("append");
    let (path, _) = two_turns(&dir);
    let first = export(&path, ForkBoundary::LatestComplete).unwrap();
    let mut file = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
    writeln!(file, "\"}}").unwrap();
    writeln!(file, "{}", json!({"type": "user", "uuid": uid(900), "parentUuid": null, "sessionId": PARENT, "message": {"role": "user", "content": "뒤에 붙은 말"}})).unwrap();
    drop(file);
    let second = export(&path, ForkBoundary::Turn(&first.snapshot.boundary_id)).unwrap();
    assert_eq!(first.snapshot.sha256, second.snapshot.sha256);
    assert!(second.source.prefix_len > first.source.prefix_len);
}

fn valid_snapshot() -> NativeForkSnapshot {
    let dir = temp_dir("valid");
    let mut tx = Tx::default();
    tx.user("질문");
    tx.tool_use("m1", "toolu_1");
    tx.tool_result("toolu_1", json!("결과"));
    tx.text("m2", "답");
    let path = tx.write(&dir, "");
    export(&path, ForkBoundary::LatestComplete).unwrap().snapshot
}

#[test]
fn verify_refuses_every_tampered_snapshot() {
    let good = valid_snapshot();
    let limits = ExportLimits::default();
    verify_snapshot(&good, &limits).unwrap();

    let invalid = |s: &NativeForkSnapshot| matches!(verify_snapshot(s, &limits), Err(ForkError::Invalid(_)));
    let mut wrong_sha = good.clone();
    wrong_sha.sha256 = "0".repeat(64);
    assert!(invalid(&wrong_sha));
    let mut upper_sha = good.clone();
    upper_sha.sha256 = good.sha256.to_uppercase();
    assert!(invalid(&upper_sha));
    let mut bad_parent = good.clone();
    bad_parent.parent_session_id = "../etc".into();
    assert!(invalid(&bad_parent));
    let mut bad_label = good.clone();
    bad_label.boundary_label = "줄\n바꿈".into();
    assert!(invalid(&bad_label));
    assert!(invalid(&edit_rows(&good, |r| r[0]["cwd"] = json!("/Users/source"))));
    assert!(invalid(&edit_rows(&good, |r| r[0]["permissionMode"] = json!("bypassPermissions"))));
    assert!(invalid(&edit_rows(&good, |r| r[0]["message"]["content"] = json!("x<system-reminder>y</system-reminder>"))));
    assert!(invalid(&edit_rows(&good, |r| r[1]["message"]["content"] = json!([{"type": "thinking", "thinking": "", "signature": "s"}]))));
    assert!(invalid(&edit_rows(&good, |r| r[1]["message"]["container"] = json!({"id": "c"}))));
    assert!(invalid(&edit_rows(&good, |r| r[1]["parentUuid"] = json!(uid(77)))));
    assert!(invalid(&edit_rows(&good, |r| r[2]["sessionId"] = json!(uid(78)))));
    assert!(invalid(&edit_rows(&good, |r| r[0]["type"] = json!("attachment"))));
    assert!(invalid(&edit_rows(&good, |r| r[2]["message"]["content"][0]["content"] = json!([{"type": "image", "source": {}}]))));
    assert!(invalid(&edit_rows(&good, |r| {
        let dup = r[1].clone();
        r.insert(2, dup);
    })));
    assert!(invalid(&resign(&good, good.jsonl.trim_end().to_string())));
    let incomplete = edit_rows(&good, |r| {
        r.truncate(2);
    });
    let mut incomplete = incomplete;
    incomplete.boundary_id = rows(&incomplete)[1]["uuid"].as_str().unwrap().to_string();
    assert!(matches!(verify_snapshot(&incomplete, &limits), Err(ForkError::Incomplete(_))), "결과 없는 도구 호출로 끝난다");
    let mut other_boundary = good.clone();
    other_boundary.boundary_id = uid(79);
    assert!(matches!(verify_snapshot(&other_boundary, &limits), Err(ForkError::Incomplete(_))));
    let mut codex = good.clone();
    codex.harness = "codex".into();
    assert!(matches!(verify_claude_snapshot(&codex, &limits), Err(ForkError::Unsupported(_))));
    assert!(matches!(verify_snapshot(&codex, &limits), Err(ForkError::Invalid(_))), "claude 행은 codex 꼴이 아니다");
    let mut unknown = good.clone();
    unknown.harness = "gemini".into();
    assert!(matches!(verify_snapshot(&unknown, &limits), Err(ForkError::Unsupported(_))));
    let small = ExportLimits { max_snapshot_bytes: 16, ..limits };
    assert!(matches!(verify_snapshot(&good, &small), Err(ForkError::Unsupported(_))));
}

fn identity(request: &str) -> ForkIdentity {
    ForkIdentity { room_id: "room-1".into(), session_id: "shared-1".into(), checkpoint_id: "cp-1".into(), request_id: request.into() }
}

struct Places {
    staging: PathBuf,
    projects: PathBuf,
    workdir: PathBuf,
}

fn places(tag: &str) -> Places {
    let root = temp_dir(tag);
    let workdir = root.join("받는 사람 작업");
    std::fs::create_dir_all(&workdir).unwrap();
    std::fs::create_dir_all(root.join("projects")).unwrap();
    Places { staging: root.join("staging"), projects: root.join("projects"), workdir }
}

#[test]
fn child_ids_are_stable_per_request_and_never_the_parent() {
    let snap = valid_snapshot();
    let a = child_session_id(&identity("fork-a").fork_key(&snap.sha256).unwrap());
    let again = child_session_id(&identity("fork-a").fork_key(&snap.sha256).unwrap());
    let b = child_session_id(&identity("fork-b").fork_key(&snap.sha256).unwrap());
    assert!(is_uuid(&a) && a.as_bytes()[14] == b'4');
    assert_eq!(a, again);
    assert_ne!(a, b);
    assert!(matches!(check_child_id(PARENT, PARENT), Err(ForkError::Conflict(_))));
    assert!(matches!(check_child_id(PARENT, &PARENT.to_uppercase()), Err(ForkError::Conflict(_))));
    assert!(matches!(identity("").fork_key(&snap.sha256), Err(ForkError::Invalid(_))));
}

#[test]
fn staging_refuses_projects_root_and_changed_content() {
    let snap = valid_snapshot();
    let p = places("stage-refuse");
    let inside = p.projects.join("kasaterm-staging");
    assert!(matches!(stage_snapshot(&inside, &p.projects, &identity("f"), &snap, &p.workdir), Err(ForkError::Invalid(_))));
    assert!(!inside.exists(), "거부하면 아무것도 만들지 않는다");
    assert!(matches!(stage_snapshot(&p.staging, &p.projects, &identity("f"), &snap, Path::new("relative")), Err(ForkError::Invalid(_))));

    let staged = stage_snapshot(&p.staging, &p.projects, &identity("f"), &snap, &p.workdir).unwrap();
    let again = stage_snapshot(&p.staging, &p.projects, &identity("f"), &snap, &p.workdir).unwrap();
    assert_eq!(staged.child_session_id, again.child_session_id);
    let text = std::fs::read_to_string(&staged.transcript).unwrap();
    assert!(text.lines().all(|l| serde_json::from_str::<Value>(l).unwrap()["cwd"] == json!(p.workdir.to_str().unwrap())));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&staged.transcript).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(&staged.dir).unwrap().permissions().mode() & 0o777, 0o700);
    }
    std::fs::write(&staged.transcript, "changed").unwrap();
    assert!(matches!(stage_snapshot(&p.staging, &p.projects, &identity("f"), &snap, &p.workdir), Err(ForkError::Conflict(_))));
    assert!(matches!(claude_fork_plan(&staged, &p.projects), Err(ForkError::Conflict(_))));
}

#[cfg(unix)]
fn launch(staged: &StagedFork, projects: &Path) -> ClaudeForkPlan {
    match claude_fork_plan(staged, projects).unwrap() {
        ClaudeForkLaunch::Launch(plan) => plan,
        ClaudeForkLaunch::AlreadyForked(_) => panic!("아직 포크하지 않았다"),
    }
}

/// claude `--resume <path> --fork-session --session-id <id>` 를 흉내 낸다: resume 파일을 읽어
/// 새 ID 로 projects 아래에 쓴다. 실제 claude 를 띄우지 않는다.
#[cfg(unix)]
fn fake_claude(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let script = dir.join("fake-claude");
    std::fs::write(
        &script,
        r#"#!/bin/sh
set -eu
resume= child= fork=
while [ $# -gt 0 ]; do
  case "$1" in
    --resume) resume=$2; shift 2 ;;
    --session-id) child=$2; shift 2 ;;
    --fork-session) fork=1; shift ;;
    *) echo "unexpected $1" >&2; exit 2 ;;
  esac
done
[ -n "$fork" ] || exit 3
out="$FAKE_PROJECTS/$FAKE_SLUG/$child.jsonl"
[ ! -e "$out" ] || { echo "Session ID $child is already in use" >&2; exit 4; }
mkdir -p "$FAKE_PROJECTS/$FAKE_SLUG"
sed "s/\"sessionId\":\"[^\"]*\"/\"sessionId\":\"$child\"/g" "$resume" > "$out"
"#,
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

#[cfg(unix)]
#[test]
fn fake_launch_creates_one_child_and_retry_does_not_duplicate() {
    let dir = temp_dir("source-for-fake");
    let (source, _) = two_turns(&dir);
    let source_before = std::fs::read(&source).unwrap();
    let snap = export(&source, ForkBoundary::LatestComplete).unwrap().snapshot;
    let p = places("fake-launch");
    let parent_elsewhere = p.projects.join("someone").join(format!("{PARENT}.jsonl"));
    std::fs::create_dir_all(parent_elsewhere.parent().unwrap()).unwrap();
    std::fs::write(&parent_elsewhere, "수신자가 원래 가진 같은 ID 세션\n").unwrap();

    let staged = stage_snapshot(&p.staging, &p.projects, &identity("fork-1"), &snap, &p.workdir).unwrap();
    let plan = launch(&staged, &p.projects);
    assert_eq!(plan.args[0], "--resume");
    assert_eq!(Path::new(&plan.args[1]), staged.transcript, "부모는 ID 가 아니라 staging 경로로 넘긴다");
    assert!(!plan.args.iter().any(|a| a.contains("dangerously") || a.contains("permission") || a == PARENT));
    assert_eq!(plan.args[2..4], ["--fork-session", "--session-id"]);
    assert_eq!(plan.args[4], plan.child_session_id);
    assert_ne!(plan.child_session_id, PARENT);
    assert_eq!(confirm_claude_child(&plan, &p.projects), ChildState::Pending);

    let run = || {
        std::process::Command::new(fake_claude(&p.staging))
            .args(&plan.args)
            .current_dir(&plan.cwd)
            .env("FAKE_PROJECTS", &p.projects)
            .env("FAKE_SLUG", claude_project_slug(&plan.cwd))
            .status()
            .unwrap()
    };
    assert!(run().success());
    let ChildState::Confirmed { session_id, transcript: child } = confirm_claude_child(&plan, &p.projects) else {
        panic!("child 가 확인돼야 한다");
    };
    assert_eq!(child, plan.expected_child_transcript);
    assert_eq!(session_id, plan.child_session_id);
    let prov = load_provenance(&staged.dir).unwrap().unwrap();
    assert_eq!(prov.child_transcript.as_deref(), Some(child.to_str().unwrap()));
    assert_eq!((prov.identity.request_id.as_str(), prov.parent_session_id.as_str()), ("fork-1", PARENT));

    let retry = stage_snapshot(&p.staging, &p.projects, &identity("fork-1"), &snap, &p.workdir).unwrap();
    match claude_fork_plan(&retry, &p.projects).unwrap() {
        ClaudeForkLaunch::AlreadyForked(prov) => assert_eq!(prov.child_session_id.as_deref(), Some(plan.child_session_id.as_str())),
        ClaudeForkLaunch::Launch(_) => panic!("재시도는 다시 띄우지 않는다"),
    }
    assert!(!run().success(), "같은 ID 로 또 띄우면 claude 가 거절한다");
    let children = std::fs::read_dir(child.parent().unwrap()).unwrap().count();
    assert_eq!(children, 1);

    let other = stage_snapshot(&p.staging, &p.projects, &identity("fork-2"), &snap, &p.workdir).unwrap();
    assert_ne!(other.child_session_id, plan.child_session_id, "다른 요청은 다른 child");
    assert_eq!(std::fs::read(&source).unwrap(), source_before);
    assert_eq!(std::fs::read_to_string(&parent_elsewhere).unwrap(), "수신자가 원래 가진 같은 ID 세션\n");
    assert_eq!(sha256_file(&staged.transcript).unwrap(), staged.staged_sha256);
}

#[test]
fn child_id_taken_by_another_session_is_a_conflict() {
    let snap = valid_snapshot();
    let p = places("taken");
    let staged = stage_snapshot(&p.staging, &p.projects, &identity("fork-1"), &snap, &p.workdir).unwrap();
    let foreign = p.projects.join("elsewhere").join(format!("{}.jsonl", staged.child_session_id));
    std::fs::create_dir_all(foreign.parent().unwrap()).unwrap();
    std::fs::write(&foreign, json!({"type": "user", "sessionId": uid(5)}).to_string() + "\n").unwrap();
    assert!(matches!(claude_fork_plan(&staged, &p.projects), Err(ForkError::Conflict(_))));
}

fn plan_at(cwd: &str, staged: &str) -> ClaudeForkPlan {
    let child = "33333333-3333-4333-8333-333333333333".to_string();
    ClaudeForkPlan {
        cwd: cwd.into(),
        args: vec!["--resume".into(), staged.into(), "--fork-session".into(), "--session-id".into(), child.clone()],
        parent_session_id: PARENT.into(),
        child_session_id: child,
        staged_transcript: staged.into(),
        staged_sha256: String::new(),
        expected_child_transcript: PathBuf::new(),
        fork_dir: PathBuf::new(),
    }
}

#[test]
fn launch_lines_quote_for_each_shell() {
    let plan = plan_at("/Users/받는 이/it's here", "/tmp/stage dir/p.jsonl");
    assert_eq!(
        plan.posix_command("claude"),
        "claude --resume '/tmp/stage dir/p.jsonl' --fork-session --session-id 33333333-3333-4333-8333-333333333333"
    );
    let line = plan.shell_line(ShellKind::Posix, "claude").unwrap();
    assert!(line.starts_with(r"cd '/Users/받는 이/it'\''s here' && claude --resume"), "{line}");

    let win = plan_at(r"C:\Users\받는 이\work", r"C:\Users\x\AppData\Local\kasaterm\fork 1\p.jsonl");
    let cmd = win.shell_line(ShellKind::Cmd, "claude").unwrap();
    assert!(cmd.starts_with(r#"cd /d "C:\Users\받는 이\work" && claude --resume "C:\Users\x\AppData\Local\kasaterm\fork 1\p.jsonl" --fork-session"#), "{cmd}");
    let ps = win.shell_line(ShellKind::PowerShell, "claude").unwrap();
    assert!(ps.contains(r"claude --resume 'C:\Users\x\AppData\Local\kasaterm\fork 1\p.jsonl'"), "{ps}");
    assert!(plan_at(r#"C:\a"b"#, r"C:\p.jsonl").shell_line(ShellKind::Cmd, "claude").is_err());
    assert!(plan_at(r"C:\work", r"C:\a&b\p.jsonl").shell_line(ShellKind::PowerShell, "claude").is_err());
}

#[test]
fn codex_fork_support_reads_only_clear_schemas() {
    let dir = temp_dir("codex-schema");
    let write = |name: &str, props: Value| {
        let d = dir.join(name).join("v2");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("ThreadForkParams.json"), json!({"properties": props, "required": ["threadId"]}).to_string()).unwrap();
        dir.join(name)
    };
    let id_only = write("id", json!({"threadId": {"type": "string"}, "lastTurnId": {"type": ["string", "null"]}}));
    assert_eq!(codex_fork_support(&id_only), CodexForkSupport::ThreadIdOnly);
    let path = write("path", json!({"threadId": {"type": "string"}, "path": {"type": ["string", "null"]}}));
    assert_eq!(codex_fork_support(&path), CodexForkSupport::PathParam);
    let vague = write("vague", json!({"threadId": {"type": "string"}, "path": {"anyOf": [{"$ref": "#/x"}]}}));
    assert!(matches!(codex_fork_support(&vague), CodexForkSupport::Unknown(_)));
    let mixed = write("mixed", json!({"threadId": {"type": "string"}, "path": {"type": ["string", "object"]}}));
    assert!(matches!(codex_fork_support(&mixed), CodexForkSupport::Unknown(_)));
    let no_thread = write("nothread", json!({"path": {"type": "string"}}));
    assert!(matches!(codex_fork_support(&no_thread), CodexForkSupport::Unknown(_)));
    assert!(matches!(codex_fork_support(&dir.join("missing")), CodexForkSupport::Unknown(_)));
    let broken = dir.join("broken/v2");
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::write(broken.join("ThreadForkParams.json"), "{").unwrap();
    assert!(matches!(codex_fork_support(&dir.join("broken")), CodexForkSupport::Unknown(_)));
}

#[test]
fn rfc3339_reads_claude_timestamps() {
    use claude::parse_rfc3339_ms;
    assert_eq!(parse_rfc3339_ms("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(parse_rfc3339_ms("2026-11-10T03:12:45.123Z"), Some(1_794_280_365_123));
    assert_eq!(parse_rfc3339_ms("2024-02-29T23:59:59.5Z"), Some(1_709_251_199_500));
    assert_eq!(parse_rfc3339_ms("2026-11-10 03:12:45Z"), None);
    assert_eq!(parse_rfc3339_ms("2026-13-10T03:12:45Z"), None);
}

#[test]
fn display_lines_share_only_the_allowed_conversation() {
    let dir = temp_dir("display");
    let mut tx = Tx::default();
    tx.raw(json!({"type": "session-owner", "accountUuid": "acct-secret", "sessionId": PARENT}));
    tx.push(json!({"type": "attachment", "attachment": {"type": "instructions", "content": "PRIVATE CLAUDE.md"}}));
    let first = tx.user("보이는 질문");
    tx.thinking("m1", Some("end_turn"));
    let answer = tx.text("m1", "보이는 답");
    tx.assistant("m2", json!({"type": "server_tool_use", "id": "s", "name": "web_search", "input": {}}), Some("end_turn"));
    let path = tx.write(&dir, "");
    let raw = std::fs::read_to_string(&path).unwrap();
    let shown = display_lines("claude", &raw);
    for secret in ["acct-secret", "PRIVATE CLAUDE.md", "sig_secret", "secret-project", "bypassPermissions", "server_tool_use", "\"cwd\""] {
        assert!(!shown.contains(secret), "{secret}: {shown}");
    }
    let rows: Vec<Value> = shown.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(rows.iter().map(|r| r["uuid"].as_str().unwrap()).collect::<Vec<_>>(), [first.as_str(), answer.as_str()]);
    assert_eq!(rows[1]["parentUuid"], json!(uid(3)), "화면은 원래 이음을 그대로 쓴다");
    assert!(display_lines("gemini", &raw).is_empty());
}

#[test]
fn boundary_end_stops_right_after_the_boundary_row() {
    let dir = temp_dir("boundary-end");
    let (path, end) = two_turns(&dir);
    let fork = export(&path, ForkBoundary::LatestComplete).unwrap();
    let raw = std::fs::read_to_string(&path).unwrap();
    let head = &raw[..fork.boundary_end as usize];
    assert!(head.ends_with('\n'));
    let last: Value = serde_json::from_str(head.trim_end().lines().last().unwrap()).unwrap();
    assert_eq!(last["uuid"], json!(end));
}
