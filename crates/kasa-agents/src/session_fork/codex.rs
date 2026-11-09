//! Codex rollout 의 완료된 턴까지를 공유 자료로 고정하고, 수신 기기에서 공식 app-server `thread/fork` 로 잇는다.
//!
//! 0.167 의 `thread/fork` 는 실험 필드 `path` 로 부모 rollout 을 경로에서 읽는다(`experimentalApi`,
//! openai/codex rust-v0.167.0 `thread_fork_can_load_source_by_path`). 그래서 부모를 수신자 CODEX_HOME 에
//! 설치하지 않는다. 받은 자료는 수신자 소유의 shadow ID 로 재키잉해 비공개 staging 에 두고, 그 경로로 포크한다.
//! child 의 `forked_from_id` 가 shadow 라서 어느 요청이 만든 child 인지 증명된다.
//!
//! paginated 원본을 경로로 읽으면 app-server 가 DB 에서 같은 ID 를 다시 찾으므로 legacy 꼴로 바꿔 쓴다.
//! 화면 행(`item_completed`)은 legacy 의 user_message·agent_message 로 옮긴다.
//!
//! 압축 요약은 계정에 묶인 암호문(`compaction.encrypted_content`, `message` 는 빈 글)이라 넘길 수 없다. 대신
//! `compacted` 행을 통째로 빼고 압축 전후의 공개 response_item 을 그대로 잇는다 — rollout 은 덧붙이기만 해서
//! 압축 이전 이력이 파일에 남아 있다(0.167 실측: 압축 있는 파일 3개에 압축 전 항목 3601개). `compacted` 를 남기면
//! legacy 재구성이 그 자리에서 이력을 replacement_history 로 갈아 끼워 앞 대화가 사라진다. 비동기 질문 전달
//! (`item_completed` AgentMessage 의 delivery·questions)은 모델 이력에 `request_user_input_async` 도구 호출로 이미
//! 있어 assistant 메시지로 되살리지 않는다(실측 27건 모두 짝이 있다).

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use kasa_socket::sessions::is_uuid;
use kasa_socket::shared_rooms::NativeForkSnapshot;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use super::{
    create_private_dir, ensure_private_dir, hex, is_within, load_provenance, now_ms, record_provenance, sha256_file, sha256_hex,
    write_new_atomic, ChildState, CodexForkSupport, ExportLimits, ExportedFork, ForkBoundary, ForkError, ForkIdentity, ForkProvenance,
    Omissions, Omit, SourceRange,
};

const IMAGE_NOTE: &str = "[공유 포크에서 이미지 생략]";
const FILE_NOTE: &str = "[공유 포크에서 첨부 파일 생략]";
const META_KEYS: [&str; 5] = ["id", "timestamp", "originator", "cli_version", "history_mode"];

/// 하네스가 끼워 넣은 사용자 메시지(`<environment_context>…</environment_context>`, AGENTS.md 지침 등)의 이름.
/// 원본의 지침·환경이 실려 있어 넘기지 않는다.
/// 사람이 한 말을 하네스가 태그로 감싸 전한 것. 공개 대화라 남긴다(비동기 질문에 대한 답 등).
const PUBLIC_TAGS: [&str; 3] = ["send_user_message_question_reply", "user_shell_command", "turn_aborted"];

fn harness_context(text: &str) -> Option<&str> {
    let t = text.trim();
    if t.starts_with("# AGENTS.md instructions") {
        return Some("AGENTS.md");
    }
    let rest = t.strip_prefix('<')?;
    let name = &rest[..rest.find('>')?];
    let tag = !name.is_empty() && name.len() <= 64 && name.chars().all(|c| c.is_ascii_lowercase() || c == '_' || c == '-');
    // 모르는 태그는 주입 문맥으로 본다 — 새 판의 지침 블록이 공유되는 쪽보다 대화 한 줄을 덜 싣는 쪽이 낫다.
    (tag && !PUBLIC_TAGS.contains(&name) && t.ends_with(&format!("</{name}>"))).then_some(name)
}

fn is_harness_context(text: &str) -> bool {
    harness_context(text).is_some()
}

/// 남기지 않는 session_meta 필드의 분류. 모르는 필드도 「기타」로 센다.
fn meta_field_kind(key: &str) -> Omit {
    match key {
        "base_instructions" | "dynamic_tools" | "selected_capability_roots" => Omit::Injected,
        "creator_user_id" | "creator_account_id" => Omit::Account,
        "cwd" | "git" | "runtime_workspace_roots" => Omit::SourcePath,
        "model_provider" | "memory_mode" | "multi_agent_version" | "context_window" => Omit::Settings,
        _ => Omit::Other,
    }
}

/// 대화 밖 행 종류의 분류.
fn line_kind(kind: &str) -> Omit {
    match kind {
        "turn_context" => Omit::Settings,
        "token_usage_record" => Omit::Request,
        _ => Omit::Other,
    }
}

/// 완료 지점 후보. `out` 의 앞 `len` 개가 그 턴까지다. `refused` 는 그 안에 공유할 수 없는 항목이 있었다는 뜻이다
/// — 경계 뒤(진행 중 턴)의 항목은 앞선 완료 지점을 막지 않는다.
#[derive(Clone)]
struct Cut {
    len: usize,
    /// 원본에서 이 완료 행이 끝나는 바이트 자리.
    end: u64,
    turn_id: String,
    timestamp: Option<String>,
    refused: Option<String>,
    /// 이 지점까지 뺀 것. 경계 뒤 줄(진행 중 턴)의 것은 세지 않는다.
    omissions: Omissions,
}

pub fn export_codex_snapshot(
    rollout: &Path,
    thread_id: &str,
    boundary: ForkBoundary<'_>,
    limits: &ExportLimits,
) -> Result<ExportedFork, ForkError> {
    if !is_uuid(thread_id) {
        return Err(ForkError::Invalid("원본 스레드 ID 가 UUID 가 아니에요".into()));
    }
    let name = rollout.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    if !name.starts_with("rollout-") || !name.ends_with(&format!("-{thread_id}.jsonl")) {
        return Err(ForkError::Invalid("rollout 파일 이름이 원본 스레드 ID 와 달라요".into()));
    }
    if !std::fs::symlink_metadata(rollout)?.is_file() {
        return Err(ForkError::Invalid("rollout 이 일반 파일이 아니에요".into()));
    }
    let file = std::fs::File::open(rollout)?;
    let len_at_read = file.metadata()?.len();
    if len_at_read > limits.max_source_bytes {
        return Err(ForkError::Unsupported(format!("원본 기록이 {len_at_read}바이트로 읽기 한도를 넘어요")));
    }
    let mut reader = BufReader::new(file.take(len_at_read));
    let mut prefix = Sha256::new();
    let mut prefix_len = 0u64;
    let mut omit = Omissions::default();
    let mut meta: Option<Value> = None;
    let mut out: Vec<Value> = Vec::new();
    let mut open = BTreeSet::<String>::new();
    let mut cut: Option<Cut> = None;
    let mut refused: Option<String> = None;
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = reader.read_until(b'\n', &mut line)?;
        if n == 0 || line.last() != Some(&b'\n') {
            break;
        }
        prefix.update(&line);
        prefix_len += n as u64;
        let Ok(row) = serde_json::from_slice::<Value>(&line) else {
            omit.add(Omit::Unreadable, "", 1);
            continue;
        };
        let kind = row["type"].as_str().unwrap_or_default();
        let payload = &row["payload"];
        let stamp = row["timestamp"].as_str().filter(|t| t.len() <= 64).map(str::to_string);
        let envelope = |kind: &str, payload: Value| {
            let mut env = Map::new();
            if let Some(stamp) = &stamp {
                env.insert("timestamp".into(), json!(stamp));
            }
            env.insert("type".into(), json!(kind));
            env.insert("payload".into(), payload);
            Value::Object(env)
        };
        if meta.is_none() {
            if kind != "session_meta" || payload["id"].as_str() != Some(thread_id) {
                return Err(ForkError::Invalid("첫 행이 이 스레드의 session_meta 가 아니에요".into()));
            }
            meta = Some(envelope("session_meta", export_meta(payload, &mut omit)?));
            continue;
        }
        match kind {
            "session_meta" => omit.add(Omit::Other, "복사된 session_meta", 1),
            "compacted" => {
                omit.add(Omit::Compaction, "계정에 묶인 암호화 요약 — 대신 압축 이전 공개 대화를 원래 기록 그대로 실어요", 1);
                if !payload["retained_context"].is_null() {
                    omit.add(Omit::Compaction, "retained_context", 1);
                }
            }
            "response_item" => match response_item(payload, &mut omit) {
                Ok(Some(item)) => out.push(envelope("response_item", item)),
                Ok(None) => {}
                Err(error) => refuse(error, &mut refused),
            },
            "event_msg" => {
                let event_type = payload["type"].as_str().unwrap_or_default();
                let turn = payload["turn_id"].as_str().map(str::to_string);
                match (event_type, turn) {
                    ("task_started", Some(turn)) => {
                        open.insert(turn);
                    }
                    ("turn_aborted", Some(turn)) => {
                        open.remove(&turn);
                    }
                    _ => {}
                }
                match event(payload, &mut omit) {
                    Ok(Some(event)) => out.push(envelope("event_msg", event)),
                    Ok(None) => {}
                    Err(error) => refuse(error, &mut refused),
                }
                if event_type == "task_complete" {
                    let turn = payload["turn_id"].as_str().unwrap_or_default().to_string();
                    open.remove(&turn);
                    if open.is_empty() {
                        let found = Cut {
                            len: out.len(), end: prefix_len, turn_id: turn.clone(), timestamp: stamp.clone(),
                            refused: refused.clone(), omissions: omit.clone(),
                        };
                        cut = Some(found);
                        if matches!(boundary, ForkBoundary::Turn(id) if id == turn) {
                            break;
                        }
                    } else if matches!(boundary, ForkBoundary::Turn(id) if id == turn) {
                        return Err(ForkError::Incomplete("고른 턴 안에서 다른 턴이 아직 돌고 있어요".into()));
                    }
                }
            }
            other => omit.add(line_kind(other), other, 1),
        }
    }
    let meta = meta.ok_or_else(|| ForkError::Invalid("session_meta 가 없어요".into()))?;
    let Some(found) = cut else {
        return Err(ForkError::Incomplete("완료된 턴이 아직 없어요".into()));
    };
    let mut items = out;
    if let ForkBoundary::Turn(id) = boundary {
        if found.turn_id != id {
            return Err(ForkError::Incomplete("고른 턴이 없거나 아직 끝나지 않았어요".into()));
        }
    }
    if let Some(why) = found.refused {
        return Err(ForkError::Unsupported(why));
    }
    items.truncate(found.len);
    let native_version = meta["payload"]["cli_version"].as_str().unwrap_or_default().to_string();
    let mut jsonl = String::new();
    for row in std::iter::once(&meta).chain(items.iter()) {
        jsonl.push_str(&serde_json::to_string(row).map_err(|e| ForkError::Invalid(e.to_string()))?);
        jsonl.push('\n');
    }
    let snapshot = NativeForkSnapshot {
        harness: "codex".into(),
        parent_session_id: thread_id.into(),
        boundary_id: found.turn_id.clone(),
        boundary_label: boundary_label(&items),
        native_version,
        sha256: sha256_hex(jsonl.as_bytes()),
        jsonl,
    };
    verify_codex_snapshot(&snapshot, limits)?;
    Ok(ExportedFork {
        snapshot,
        omissions: found.omissions.into_vec(),
        boundary_at_ms: found.timestamp.as_deref().and_then(super::claude::parse_rfc3339_ms),
        boundary_end: found.end,
        source: SourceRange { prefix_len, prefix_sha256: hex(&prefix.finalize()), len_at_read },
    })
}

/// 화면용 한 줄. session_meta 는 지침·계정·경로를 싣고 있어 통째로 뺀다.
pub(super) fn display_row(row: &Value) -> Option<Value> {
    let mut omit = Omissions::default();
    let payload = match row["type"].as_str()? {
        "response_item" => response_item(&row["payload"], &mut omit).ok()??,
        "event_msg" => event(&row["payload"], &mut omit).ok()??,
        _ => return None,
    };
    let mut clean = json!({"type": row["type"], "payload": payload});
    if let Some(stamp) = row["timestamp"].as_str().filter(|t| t.len() <= 64) {
        clean["timestamp"] = json!(stamp);
    }
    Some(clean)
}

fn refuse(error: ForkError, refused: &mut Option<String>) {
    refused.get_or_insert_with(|| match error {
        ForkError::Unsupported(why) | ForkError::Invalid(why) => why,
        other => other.to_string(),
    });
}

fn export_meta(payload: &Value, omit: &mut Omissions) -> Result<Value, ForkError> {
    let mode = payload["history_mode"].as_str().unwrap_or("legacy");
    if !matches!(mode, "legacy" | "paginated") {
        return Err(ForkError::Unsupported(format!("{mode} 이력 형식은 지원하지 않아요")));
    }
    // 부모 이력을 참조만 하는 rollout 은 파일만으로 대화가 완성되지 않는다.
    for key in ["history_base", "forked_from_ordinal_exclusive", "subagent_history_start_ordinal"] {
        if !payload[key].is_null() {
            return Err(ForkError::Unsupported(format!("다른 스레드의 이력을 참조하는 rollout 이에요({key})")));
        }
    }
    if payload["attached_thread"].as_bool() == Some(true) {
        return Err(ForkError::Unsupported("원본에 붙은 스레드는 이력이 완성되지 않아요".into()));
    }
    let text = |key: &str| payload[key].as_str().filter(|t| !t.is_empty() && t.len() <= 64 && !t.chars().any(char::is_control));
    let mut meta = Map::new();
    meta.insert("id".into(), payload["id"].clone());
    meta.insert("timestamp".into(), json!(text("timestamp").ok_or_else(|| ForkError::Invalid("session_meta 시각이 없어요".into()))?));
    meta.insert("originator".into(), json!(text("originator").unwrap_or("codex")));
    meta.insert("cli_version".into(), json!(text("cli_version").unwrap_or("unknown")));
    meta.insert("history_mode".into(), json!("legacy"));
    for (key, value) in payload.as_object().into_iter().flatten() {
        let kept = META_KEYS.contains(&key.as_str()) || matches!(key.as_str(), "session_id" | "source" | "thread_source");
        if !kept && !value.is_null() {
            omit.add(meta_field_kind(key), &format!("session_meta: {key}"), 1);
        }
    }
    Ok(Value::Object(meta))
}

fn text_of(item: &Value) -> Result<&str, ForkError> {
    item["text"].as_str().ok_or_else(|| ForkError::Invalid("text 가 문자열이 아니에요".into()))
}

fn response_item(payload: &Value, omit: &mut Omissions) -> Result<Option<Value>, ForkError> {
    let kind = payload["type"].as_str().unwrap_or_default();
    let string = |key: &str| {
        payload[key].as_str().map(str::to_string).ok_or_else(|| ForkError::Invalid(format!("{kind} 의 {key} 가 없어요")))
    };
    let item = match kind {
        "message" => match payload["role"].as_str() {
            Some("developer" | "system") => {
                omit.add(Omit::Injected, "developer·system 메시지", 1);
                return Ok(None);
            }
            Some("user") => {
                let mut content = Vec::new();
                for part in payload["content"].as_array().into_iter().flatten() {
                    match part["type"].as_str() {
                        Some("input_text") => {
                            let text = text_of(part)?;
                            if let Some(name) = harness_context(text) {
                                omit.add(Omit::Injected, name, 1);
                                return Ok(None);
                            }
                            content.push(json!({"type": "input_text", "text": text}));
                        }
                        Some("input_image") => {
                            omit.add(Omit::Image, "사용자", 1);
                            content.push(json!({"type": "input_text", "text": IMAGE_NOTE}));
                        }
                        Some("input_file") => {
                            omit.add(Omit::Attachment, "사용자 파일", 1);
                            content.push(json!({"type": "input_text", "text": FILE_NOTE}));
                        }
                        other => return Err(ForkError::Unsupported(format!("사용자 메시지의 {other:?} 조각은 공유 형식에 없어요"))),
                    }
                }
                if content.is_empty() {
                    return Ok(None);
                }
                json!({"type": "message", "role": "user", "content": content})
            }
            Some("assistant") => {
                let mut content = Vec::new();
                for part in payload["content"].as_array().into_iter().flatten() {
                    match part["type"].as_str() {
                        Some("output_text") => content.push(json!({"type": "output_text", "text": text_of(part)?})),
                        other => return Err(ForkError::Unsupported(format!("assistant 메시지의 {other:?} 조각은 공유 형식에 없어요"))),
                    }
                }
                if content.is_empty() {
                    return Ok(None);
                }
                let mut msg = json!({"type": "message", "role": "assistant", "content": content});
                if let Some(phase) = payload["phase"].as_str() {
                    msg["phase"] = json!(phase);
                }
                msg
            }
            other => return Err(ForkError::Unsupported(format!("{other:?} 역할 메시지는 공유 형식에 없어요"))),
        },
        "reasoning" => {
            omit.add(Omit::Reasoning, "reasoning", 1);
            return Ok(None);
        }
        "compaction" => {
            omit.add(Omit::Compaction, "계정에 묶인 암호화 요약 — 남은 사용자 메시지만 이어짐", 1);
            return Ok(None);
        }
        "additional_tools" => {
            omit.add(Omit::Injected, kind, 1);
            return Ok(None);
        }
        "configuration_update" => {
            omit.add(Omit::Settings, kind, 1);
            return Ok(None);
        }
        "web_search_call" | "agent_message" | "ghost_snapshot" => {
            omit.add(Omit::Other, kind, 1);
            return Ok(None);
        }
        "function_call" => {
            let mut call = json!({"type": kind, "name": string("name")?, "arguments": string("arguments")?, "call_id": string("call_id")?});
            if let Some(namespace) = payload["namespace"].as_str() {
                call["namespace"] = json!(namespace);
            }
            call
        }
        "custom_tool_call" => json!({"type": kind, "name": string("name")?, "input": string("input")?, "call_id": string("call_id")?}),
        "function_call_output" | "custom_tool_call_output" => {
            let mut result = json!({"type": kind, "call_id": string("call_id")?, "output": tool_output(&payload["output"], omit)?});
            if let (true, Some(name)) = (kind == "custom_tool_call_output", payload["name"].as_str()) {
                result["name"] = json!(name);
            }
            result
        }
        other => return Err(ForkError::Unsupported(format!("{other} 항목은 공유 형식에 없어요"))),
    };
    Ok(Some(item))
}

fn tool_output(output: &Value, omit: &mut Omissions) -> Result<Value, ForkError> {
    match output {
        Value::String(text) => Ok(json!(text)),
        Value::Array(parts) => {
            let mut out = Vec::new();
            for part in parts {
                match part["type"].as_str() {
                    Some(kind @ ("input_text" | "output_text")) => out.push(json!({"type": kind, "text": text_of(part)?})),
                    Some("input_image") => {
                        omit.add(Omit::Image, "도구 결과", 1);
                        out.push(json!({"type": "input_text", "text": IMAGE_NOTE}));
                    }
                    other => return Err(ForkError::Unsupported(format!("도구 결과의 {other:?} 조각은 공유 형식에 없어요"))),
                }
            }
            Ok(Value::Array(out))
        }
        _ => Err(ForkError::Unsupported("도구 결과의 꼴이 공유 형식에 없어요".into())),
    }
}

fn event(payload: &Value, omit: &mut Omissions) -> Result<Option<Value>, ForkError> {
    let kind = payload["type"].as_str().unwrap_or_default();
    let turn = || payload["turn_id"].as_str().map(str::to_string).ok_or_else(|| ForkError::Invalid(format!("{kind} 에 turn_id 가 없어요")));
    let event = match kind {
        "task_started" => {
            let mut e = json!({"type": kind, "turn_id": turn()?});
            if let Some(at) = payload["started_at"].as_i64() {
                e["started_at"] = json!(at);
            }
            e
        }
        "task_complete" => {
            let mut e = json!({"type": kind, "turn_id": turn()?, "last_agent_message": payload["last_agent_message"].as_str()});
            for key in ["completed_at", "duration_ms"] {
                if let Some(n) = payload[key].as_i64() {
                    e[key] = json!(n);
                }
            }
            e
        }
        "user_message" => json!({"type": kind, "message": payload["message"].as_str().unwrap_or_default()}),
        "agent_message" => json!({"type": kind, "message": payload["message"].as_str().unwrap_or_default()}),
        "item_completed" => {
            let item = &payload["item"];
            let texts = |tag: &str| {
                item["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|part| part["type"].as_str() == Some(tag))
                    .filter_map(|part| part["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            match item["type"].as_str() {
                Some("UserMessage") => {
                    let message = texts("text");
                    if let Some(name) = harness_context(&message) {
                        omit.add(Omit::Injected, &format!("화면 {name}"), 1);
                        return Ok(None);
                    }
                    if item["content"].as_array().into_iter().flatten().any(|part| part["type"] != "text") {
                        omit.add(Omit::Image, "화면 첨부", 1);
                    }
                    if message.trim().is_empty() {
                        return Ok(None);
                    }
                    json!({"type": "user_message", "message": message})
                }
                Some("AgentMessage") => json!({"type": "agent_message", "message": texts("Text")}),
                other => {
                    let kind = other.unwrap_or("?");
                    let class = if kind == "Reasoning" { Omit::Reasoning } else { Omit::Display };
                    omit.add(class, &format!("화면 {kind}"), 1);
                    return Ok(None);
                }
            }
        }
        other => {
            let class = match other {
                "token_count" => Omit::Request,
                "thread_settings_applied" => Omit::Settings,
                "agent_reasoning" | "agent_reasoning_raw_content" | "agent_reasoning_section_break" => Omit::Reasoning,
                _ => Omit::Display,
            };
            omit.add(class, &format!("event: {other}"), 1);
            return Ok(None);
        }
    };
    Ok(Some(event))
}

fn boundary_label(items: &[Value]) -> String {
    let turns = items.iter().filter(|r| r["payload"]["type"] == "task_complete").count();
    let prompt = items.iter().rev().find_map(|r| (r["payload"]["type"] == "user_message").then(|| r["payload"]["message"].as_str()).flatten());
    let line = prompt.and_then(|p| p.lines().map(str::trim).find(|l| !l.is_empty())).unwrap_or("");
    let clipped: String = line.chars().filter(|c| !c.is_control()).take(60).collect();
    if clipped.is_empty() {
        format!("{turns}번째 완료 턴")
    } else {
        format!("{turns}번째 완료 턴 · {clipped}")
    }
}

fn keys_within(obj: &Map<String, Value>, allowed: &[&str]) -> Result<(), String> {
    match obj.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(key) => Err(format!("허용하지 않는 키 {key}")),
        None => Ok(()),
    }
}

fn verify_item(item: &Value, calls: &mut Vec<String>) -> Result<(), String> {
    let obj = item.as_object().ok_or("항목이 객체가 아니에요")?;
    let is_str = |key: &str| obj.get(key).is_some_and(Value::is_string);
    match obj.get("type").and_then(Value::as_str) {
        Some("message") => {
            keys_within(obj, &["type", "role", "content", "phase"])?;
            let (tag, role) = match obj.get("role").and_then(Value::as_str) {
                Some("user") => ("input_text", "user"),
                Some("assistant") => ("output_text", "assistant"),
                _ => return Err("user·assistant 외 메시지예요".into()),
            };
            if role == "user" && obj.contains_key("phase") || obj.get("phase").is_some_and(|p| !p.is_string()) {
                return Err("phase 꼴이 달라요".into());
            }
            let parts = obj.get("content").and_then(Value::as_array).filter(|c| !c.is_empty()).ok_or("내용이 비었어요")?;
            for part in parts {
                let part = part.as_object().ok_or("조각이 객체가 아니에요")?;
                let text = part.get("text").and_then(Value::as_str).ok_or("text 가 없어요")?;
                if part.len() != 2 || part.get("type").and_then(Value::as_str) != Some(tag) || (role == "user" && is_harness_context(text)) {
                    return Err("메시지 조각 꼴이 달라요".into());
                }
            }
        }
        Some("function_call") => {
            keys_within(obj, &["type", "name", "arguments", "call_id", "namespace"])?;
            if !(is_str("name") && is_str("arguments") && is_str("call_id")) || obj.get("namespace").is_some_and(|v| !v.is_string()) {
                return Err("function_call 꼴이 달라요".into());
            }
            calls.push(obj["call_id"].as_str().unwrap_or_default().to_string());
        }
        Some("custom_tool_call") => {
            keys_within(obj, &["type", "name", "input", "call_id"])?;
            if !(is_str("name") && is_str("input") && is_str("call_id")) {
                return Err("custom_tool_call 꼴이 달라요".into());
            }
            calls.push(obj["call_id"].as_str().unwrap_or_default().to_string());
        }
        Some(kind @ ("function_call_output" | "custom_tool_call_output")) => {
            let allowed: &[&str] = if kind == "custom_tool_call_output" { &["type", "call_id", "output", "name"] } else { &["type", "call_id", "output"] };
            keys_within(obj, allowed)?;
            let id = obj.get("call_id").and_then(Value::as_str).ok_or("call_id 가 없어요")?;
            let at = calls.iter().position(|c| c == id).ok_or("앞선 호출이 없는 도구 결과예요")?;
            calls.remove(at);
            match obj.get("output") {
                Some(Value::String(_)) => {}
                Some(Value::Array(parts)) => {
                    for part in parts {
                        let part = part.as_object().ok_or("결과 조각이 객체가 아니에요")?;
                        let ok = part.len() == 2
                            && matches!(part.get("type").and_then(Value::as_str), Some("input_text" | "output_text"))
                            && part.get("text").is_some_and(Value::is_string);
                        if !ok {
                            return Err("도구 결과에 글 외 조각이 있어요".into());
                        }
                    }
                }
                _ => return Err("도구 결과 꼴이 달라요".into()),
            }
        }
        _ => return Err("허용하지 않는 항목이에요(추론·지침·암호문 포함)".into()),
    }
    Ok(())
}

/// 다른 사람이 보낸 Codex 고정 자료를 믿지 않고 다시 잰다. 고정할 때 만든 꼴만 받는다.
pub fn verify_codex_snapshot(snapshot: &NativeForkSnapshot, limits: &ExportLimits) -> Result<(), ForkError> {
    if snapshot.harness != "codex" {
        return Err(ForkError::Unsupported(format!("{} 자료는 Codex 포크로 받지 않아요", snapshot.harness)));
    }
    let parent = snapshot.parent_session_id.as_str();
    let turn_ok = |t: &str| !t.is_empty() && t.len() <= 128 && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !is_uuid(parent) || !turn_ok(&snapshot.boundary_id) {
        return Err(ForkError::Invalid("스레드·턴 ID 꼴이 달라요".into()));
    }
    super::claude::verify_envelope(snapshot, limits)?;
    let mut calls = Vec::new();
    let mut open = BTreeSet::new();
    let mut last_complete = None;
    for (i, line) in snapshot.jsonl[..snapshot.jsonl.len() - 1].split('\n').enumerate() {
        let bad = |why: &str| ForkError::Invalid(format!("{}번째 행: {why}", i + 1));
        let row: Value = serde_json::from_str(line).map_err(|e| bad(&e.to_string()))?;
        let obj = row.as_object().ok_or_else(|| bad("객체가 아니에요"))?;
        keys_within(obj, &["timestamp", "type", "payload"]).map_err(|why| bad(&why))?;
        if obj.get("timestamp").is_some_and(|t| !t.as_str().is_some_and(|t| t.len() <= 64)) {
            return Err(bad("timestamp 꼴이 달라요"));
        }
        let payload = obj.get("payload").and_then(Value::as_object).ok_or_else(|| bad("payload 가 없어요"))?;
        let kind = obj.get("type").and_then(Value::as_str).unwrap_or_default();
        last_complete = None;
        match (i, kind) {
            (0, "session_meta") => {
                keys_within(payload, &META_KEYS).map_err(|why| bad(&why))?;
                let text = |k: &str| payload.get(k).and_then(Value::as_str).is_some_and(|t| t.len() <= 64 && !t.chars().any(char::is_control));
                if payload.get("id").and_then(Value::as_str) != Some(parent)
                    || payload.get("history_mode").and_then(Value::as_str) != Some("legacy")
                    || !META_KEYS.iter().all(|k| text(k))
                {
                    return Err(bad("session_meta 꼴이 달라요"));
                }
            }
            (0, _) => return Err(bad("첫 행이 session_meta 가 아니에요")),
            (_, "response_item") => verify_item(&Value::Object(payload.clone()), &mut calls).map_err(|why| bad(&why))?,
            (_, "event_msg") => {
                let turn = payload.get("turn_id").and_then(Value::as_str);
                let allowed: &[&str] = match payload.get("type").and_then(Value::as_str) {
                    Some("task_started") => &["type", "turn_id", "started_at"],
                    Some("task_complete") => &["type", "turn_id", "last_agent_message", "completed_at", "duration_ms"],
                    Some("user_message") => &["type", "message"],
                    Some("agent_message") => &["type", "message"],
                    _ => return Err(bad("허용하지 않는 이벤트예요")),
                };
                keys_within(payload, allowed).map_err(|why| bad(&why))?;
                if payload.iter().any(|(k, v)| match k.as_str() {
                    "type" | "turn_id" | "message" => !v.is_string(),
                    "last_agent_message" => !(v.is_string() || v.is_null()),
                    _ => !v.is_i64(),
                }) {
                    return Err(bad("이벤트 값 꼴이 달라요"));
                }
                match (payload.get("type").and_then(Value::as_str), turn) {
                    (Some("task_started"), Some(turn)) => {
                        open.insert(turn.to_string());
                    }
                    (Some("task_complete"), Some(turn)) => {
                        open.remove(turn);
                        last_complete = Some(turn.to_string());
                    }
                    (Some("task_started" | "task_complete"), None) => return Err(bad("turn_id 가 없어요")),
                    _ => {}
                }
            }
            _ => return Err(bad("허용하지 않는 행 종류예요")),
        }
    }
    if last_complete.as_deref() != Some(snapshot.boundary_id.as_str()) || !open.is_empty() {
        return Err(ForkError::Incomplete("마지막 행이 경계 턴의 완료가 아니에요".into()));
    }
    match calls.first() {
        Some(id) => Err(ForkError::Incomplete(format!("결과가 없는 도구 호출이 있어요: {id}"))),
        None => Ok(()),
    }
}

/// 수신 기기 staging 에 놓인 Codex 포크 자료. shadow ID 는 이 요청이 소유한다.
#[derive(Clone, Debug)]
pub struct StagedCodexFork {
    pub dir: PathBuf,
    pub rollout: PathBuf,
    pub fork_key: String,
    pub identity: ForkIdentity,
    pub parent_thread_id: String,
    pub boundary_turn_id: String,
    pub snapshot_sha256: String,
    pub staged_sha256: String,
    pub shadow_thread_id: String,
    pub workdir: PathBuf,
}

pub fn shadow_thread_id(fork_key: &str) -> String {
    let digest = Sha256::digest(format!("kasaterm-shared-fork-codex-shadow-v1\0{fork_key}").as_bytes());
    super::uuid_v4_from(&digest)
}

/// 받은 자료를 검증해 shadow ID 로 재키잉한 legacy rollout 을 `staging_root/<fork_key>/` 에 둔다.
/// staging 이 수신자 CODEX_HOME 안이면 거부한다 — 부모를 그 홈에 설치하지 않는다.
pub fn stage_codex_snapshot(
    staging_root: &Path,
    codex_home: &Path,
    identity: &ForkIdentity,
    snapshot: &NativeForkSnapshot,
    workdir: &Path,
) -> Result<StagedCodexFork, ForkError> {
    verify_codex_snapshot(snapshot, &ExportLimits::default())?;
    if !staging_root.is_absolute() || !workdir.is_absolute() {
        return Err(ForkError::Invalid("staging·작업 폴더는 절대 경로여야 해요".into()));
    }
    if !std::fs::metadata(workdir).map(|m| m.is_dir()).unwrap_or(false) {
        return Err(ForkError::Invalid(format!("작업 폴더가 없어요: {}", workdir.display())));
    }
    let workdir_text = workdir.to_str().ok_or_else(|| ForkError::Invalid("작업 폴더 경로가 UTF-8 이 아니에요".into()))?;
    if is_within(staging_root, codex_home) {
        return Err(ForkError::Invalid("staging 이 수신자 Codex 홈 안에 있어요".into()));
    }
    let fork_key = identity.fork_key(&snapshot.sha256)?;
    let shadow = shadow_thread_id(&fork_key);
    if shadow.eq_ignore_ascii_case(&snapshot.parent_session_id) {
        return Err(ForkError::Conflict("shadow ID 가 원본 ID 와 같아요".into()));
    }
    if let Some(taken) = rollouts_in(codex_home).into_iter().find(|p| p.to_string_lossy().contains(&shadow)) {
        return Err(ForkError::Conflict(format!("shadow ID 를 수신자 Codex 홈의 다른 기록이 쓰고 있어요: {}", taken.display())));
    }
    let mut lines = snapshot.jsonl.lines();
    let mut meta: Value = serde_json::from_str(lines.next().unwrap_or_default()).map_err(|e| ForkError::Invalid(e.to_string()))?;
    let stamp = meta["payload"]["timestamp"].as_str().and_then(file_stamp).ok_or_else(|| ForkError::Invalid("session_meta 시각 꼴이 달라요".into()))?;
    meta["payload"]["id"] = json!(shadow);
    meta["payload"]["session_id"] = json!(shadow);
    meta["payload"]["cwd"] = json!(workdir_text);
    let mut staged = serde_json::to_string(&meta).map_err(|e| ForkError::Invalid(e.to_string()))? + "\n";
    for line in lines {
        staged.push_str(line);
        staged.push('\n');
    }
    let staged_sha256 = sha256_hex(staged.as_bytes());

    ensure_private_dir(staging_root)?;
    let dir = staging_root.join(&fork_key);
    let rollout = dir.join(format!("rollout-{stamp}-{shadow}.jsonl"));
    let fork = StagedCodexFork {
        dir: dir.clone(),
        rollout: rollout.clone(),
        fork_key,
        identity: identity.clone(),
        parent_thread_id: snapshot.parent_session_id.clone(),
        boundary_turn_id: snapshot.boundary_id.clone(),
        snapshot_sha256: snapshot.sha256.clone(),
        staged_sha256,
        shadow_thread_id: shadow,
        workdir: workdir.to_path_buf(),
    };
    let fresh = ForkProvenance {
        harness: "codex".into(),
        shadow_session_id: Some(fork.shadow_thread_id.clone()),
        child_session_id: None,
        ..ForkProvenance::base(&fork.identity, &fork.parent_thread_id, &fork.boundary_turn_id, &fork.snapshot_sha256, &fork.fork_key, &fork.workdir, &fork.rollout)
    };
    if dir.exists() {
        if std::fs::symlink_metadata(&dir)?.file_type().is_symlink() {
            return Err(ForkError::Conflict(format!("staging 폴더가 링크예요: {}", dir.display())));
        }
        if rollout.exists() {
            if sha256_file(&rollout)? != fork.staged_sha256 {
                return Err(ForkError::Conflict(format!("같은 요청의 staging 내용이 달라요: {}", rollout.display())));
            }
            return match load_provenance(&dir)? {
                Some(prov) if !prov.same_request(&fresh) => Err(ForkError::Conflict("staging 의 기록이 이 요청과 달라요".into())),
                Some(_) => Ok(fork),
                None => record_provenance(&dir, &fresh).map(|()| fork),
            };
        }
    } else {
        create_private_dir(&dir)?;
    }
    write_new_atomic(&rollout, staged.as_bytes())?;
    record_provenance(&dir, &fresh)?;
    Ok(fork)
}

/// `2026-11-10T03:12:45.123Z` → codex rollout 파일 이름의 `2026-11-10T03-12-45`.
fn file_stamp(timestamp: &str) -> Option<String> {
    super::claude::parse_rfc3339_ms(timestamp)?;
    Some(format!("{}T{}-{}-{}", &timestamp[..10], &timestamp[11..13], &timestamp[14..16], &timestamp[17..19]))
}

/// 공식 app-server 에 보낼 요청 두 개. 권한·모델·지침 덮어쓰기는 넣지 않는다 — 수신자 설정을 쓴다.
#[derive(Clone, Debug)]
pub struct CodexForkPlan {
    pub initialize_params: Value,
    pub fork_params: Value,
    pub parent_thread_id: String,
    pub shadow_thread_id: String,
    pub boundary_turn_id: String,
    pub staged_rollout: PathBuf,
    pub staged_sha256: String,
    pub fork_dir: PathBuf,
}

#[derive(Clone, Debug)]
pub enum CodexForkLaunch {
    Launch(CodexForkPlan),
    AlreadyForked(ForkProvenance),
}

/// `support` 는 설치된 codex 의 `app-server generate-json-schema --experimental` 결과로 본 것이어야 한다.
pub fn codex_fork_plan(staged: &StagedCodexFork, codex_home: &Path, support: &CodexForkSupport) -> Result<CodexForkLaunch, ForkError> {
    if *support != CodexForkSupport::PathParam {
        return Err(super::codex_unsupported(support));
    }
    if sha256_file(&staged.rollout)? != staged.staged_sha256 {
        return Err(ForkError::Conflict("staging 파일이 고정 뒤에 바뀌었어요".into()));
    }
    if !std::fs::metadata(&staged.workdir).map(|m| m.is_dir()).unwrap_or(false) {
        return Err(ForkError::Invalid(format!("작업 폴더가 없어요: {}", staged.workdir.display())));
    }
    let path = staged.rollout.to_str().ok_or_else(|| ForkError::Invalid("staging 경로가 UTF-8 이 아니에요".into()))?;
    let cwd = staged.workdir.to_str().ok_or_else(|| ForkError::Invalid("작업 폴더 경로가 UTF-8 이 아니에요".into()))?;
    let mut prov = load_provenance(&staged.dir)?.ok_or_else(|| ForkError::Invalid("staging 에 provenance 가 없어요".into()))?;
    if prov.shadow_session_id.as_deref() != Some(staged.shadow_thread_id.as_str()) {
        return Err(ForkError::Conflict("staging 의 기록이 이 요청과 달라요".into()));
    }
    let children = children_of(codex_home, &staged.shadow_thread_id);
    match children.as_slice() {
        [] if prov.launched_at_ms.is_some() => {
            return Err(ForkError::Conflict(format!(
                "앞선 포크 실행의 결과를 확인하지 못했어요. Codex 에서 forkedFromId={} 인 스레드를 확인한 뒤 reset_codex_launch 로 다시 시도해 주세요",
                staged.shadow_thread_id
            )))
        }
        [] => {}
        [(child, path)] => {
            prov.child_session_id = Some(child.clone());
            prov.child_transcript = Some(path.display().to_string());
            prov.confirmed_at_ms.get_or_insert_with(now_ms);
            record_provenance(&staged.dir, &prov)?;
            return Ok(CodexForkLaunch::AlreadyForked(prov));
        }
        many => return Err(ForkError::Conflict(format!("같은 shadow 에서 갈라진 스레드가 {}개예요", many.len()))),
    }
    Ok(CodexForkLaunch::Launch(CodexForkPlan {
        initialize_params: json!({
            "clientInfo": {"name": "kasaterm", "title": "kasaterm shared fork", "version": env!("CARGO_PKG_VERSION")},
            "capabilities": {"experimentalApi": true},
        }),
        fork_params: json!({
            "threadId": staged.shadow_thread_id,
            "path": path,
            "cwd": cwd,
            "lastTurnId": staged.boundary_turn_id,
            "excludeTurns": true,
        }),
        parent_thread_id: staged.parent_thread_id.clone(),
        shadow_thread_id: staged.shadow_thread_id.clone(),
        boundary_turn_id: staged.boundary_turn_id.clone(),
        staged_rollout: staged.rollout.clone(),
        staged_sha256: staged.staged_sha256.clone(),
        fork_dir: staged.dir.clone(),
    }))
}

/// 실패 응답처럼 child 가 생기지 않은 것이 분명할 때 launch 표시를 지운다.
pub fn reset_codex_launch(fork_dir: &Path) -> Result<(), ForkError> {
    if let Some(mut prov) = load_provenance(fork_dir)? {
        prov.launched_at_ms = None;
        record_provenance(fork_dir, &prov)?;
    }
    Ok(())
}

/// app-server 를 직접 띄워 포크 요청 한 번을 보낸다. `app_server` 는 부르는 쪽이 고른 실행
/// (`<codex> app-server` 등)이다. 보내기 전에 launch 표시를 남겨, 응답을 못 받아도 다시 띄우지 않게 한다.
/// 띄운 자식만 정리한다.
pub fn run_codex_fork(app_server: &mut Command, plan: &CodexForkPlan, timeout: Duration) -> Result<Value, ForkError> {
    let mut prov = load_provenance(&plan.fork_dir)?.ok_or_else(|| ForkError::Invalid("staging 에 provenance 가 없어요".into()))?;
    prov.launched_at_ms = Some(now_ms());
    record_provenance(&plan.fork_dir, &prov)?;
    let mut child = app_server.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()?;
    let result = talk(&mut child, plan, timeout);
    drop(child.stdin.take());
    let deadline = Instant::now() + Duration::from_secs(2);
    while matches!(child.try_wait(), Ok(None)) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    if matches!(child.try_wait(), Ok(None)) {
        let _ = child.kill();
    }
    let _ = child.wait();
    if let Err(ForkError::Unsupported(_) | ForkError::Invalid(_)) = &result {
        reset_codex_launch(&plan.fork_dir)?;
    }
    result
}

fn talk(child: &mut std::process::Child, plan: &CodexForkPlan, timeout: Duration) -> Result<Value, ForkError> {
    let mut stdin = child.stdin.take().ok_or_else(|| ForkError::Io(std::io::Error::other("app-server stdin 이 없어요")))?;
    let stdout = child.stdout.take().ok_or_else(|| ForkError::Io(std::io::Error::other("app-server stdout 이 없어요")))?;
    let (tx, rx) = mpsc::channel::<Value>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(message) = serde_json::from_str::<Value>(&line) {
                if tx.send(message).is_err() {
                    break;
                }
            }
        }
    });
    let deadline = Instant::now() + timeout;
    let mut send = |message: Value| -> Result<(), ForkError> {
        writeln!(stdin, "{message}")?;
        stdin.flush()?;
        Ok(())
    };
    let wait = |id: u64| -> Result<Value, ForkError> {
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(message) if message["id"] == json!(id) && message.get("method").is_none() => return Ok(message),
                Ok(_) => continue,
                Err(_) => return Err(ForkError::Incomplete("app-server 응답을 받지 못했어요. child 생성 여부를 확인해야 해요".into())),
            }
        }
    };
    send(json!({"id": 1, "method": "initialize", "params": plan.initialize_params}))?;
    let init = wait(1)?;
    if let Some(error) = init.get("error") {
        return Err(ForkError::Unsupported(format!("app-server 초기화 거부: {error}")));
    }
    send(json!({"method": "initialized"}))?;
    send(json!({"id": 2, "method": "thread/fork", "params": plan.fork_params}))?;
    let reply = wait(2)?;
    if let Some(error) = reply.get("error") {
        return Err(ForkError::Invalid(format!("thread/fork 거부: {error}")));
    }
    reply.get("result").cloned().ok_or_else(|| ForkError::Invalid("thread/fork 응답에 result 가 없어요".into()))
}

/// child 를 확인한다. `response` 는 `thread/fork` 의 result 다. 응답을 잃었으면 None 으로 불러
/// 수신자 홈에서 shadow 의 유일한 child 를 찾는다.
pub fn confirm_codex_child(plan: &CodexForkPlan, codex_home: &Path, response: Option<&Value>) -> ChildState {
    match sha256_file(&plan.staged_rollout) {
        Ok(sha) if sha == plan.staged_sha256 => {}
        Ok(_) => return ChildState::Mismatch("staging 파일이 실행 중에 바뀌었어요".into()),
        Err(error) => return ChildState::Mismatch(format!("staging 파일을 읽지 못했어요: {error}")),
    }
    let children = children_of(codex_home, &plan.shadow_thread_id);
    if let Some(thread) = response.map(|r| &r["thread"]) {
        let id = thread["id"].as_str().unwrap_or_default();
        if !is_uuid(id) || id == plan.shadow_thread_id || id == plan.parent_thread_id {
            return ChildState::Mismatch(format!("응답의 child ID 가 올바르지 않아요: {id}"));
        }
        if thread["forkedFromId"].as_str() != Some(plan.shadow_thread_id.as_str()) {
            return ChildState::Mismatch("응답의 forkedFromId 가 shadow 가 아니에요".into());
        }
        if children.iter().any(|(child, _)| child != id) {
            return ChildState::Mismatch("shadow 에서 다른 child 가 또 갈라졌어요".into());
        }
    }
    let (child, path) = match children.as_slice() {
        [] => return ChildState::Pending,
        [one] => one.clone(),
        many => return ChildState::Mismatch(format!("같은 shadow 에서 갈라진 스레드가 {}개예요", many.len())),
    };
    match load_provenance(&plan.fork_dir) {
        Ok(Some(mut prov)) => {
            if prov.child_session_id.as_deref().is_some_and(|known| known != child) {
                return ChildState::Mismatch("provenance 의 child 와 달라요".into());
            }
            if prov.child_session_id.is_none() {
                prov.child_session_id = Some(child.clone());
                prov.child_transcript = Some(path.display().to_string());
                prov.confirmed_at_ms = Some(now_ms());
                if let Err(error) = record_provenance(&plan.fork_dir, &prov) {
                    return ChildState::Mismatch(format!("provenance 를 적지 못했어요: {error}"));
                }
            }
        }
        Ok(None) => return ChildState::Mismatch("staging 에 provenance 가 없어요".into()),
        Err(error) => return ChildState::Mismatch(error.to_string()),
    }
    ChildState::Confirmed { session_id: child, transcript: path }
}

fn rollouts_in(codex_home: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for root in ["sessions", "archived_sessions"] {
        let mut stack = vec![codex_home.join(root)];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let Ok(kind) = entry.file_type() else { continue };
                let path = entry.path();
                if kind.is_dir() {
                    stack.push(path);
                } else if kind.is_file() && entry.file_name().to_string_lossy().starts_with("rollout-") {
                    found.push(path);
                }
            }
        }
    }
    found
}

/// 수신자 홈에서 session_meta.forked_from_id 가 `shadow` 인 rollout 들(첫 행만 읽는다).
fn children_of(codex_home: &Path, shadow: &str) -> Vec<(String, PathBuf)> {
    let mut children = Vec::new();
    for path in rollouts_in(codex_home) {
        let Ok(file) = std::fs::File::open(&path) else { continue };
        let mut first = Vec::new();
        if BufReader::new(file.take(1024 * 1024)).read_until(b'\n', &mut first).is_err() {
            continue;
        }
        let Ok(row) = serde_json::from_slice::<Value>(&first) else { continue };
        let meta = &row["payload"];
        if row["type"] == "session_meta" && meta["forked_from_id"].as_str() == Some(shadow) {
            if let Some(id) = meta["id"].as_str().filter(|id| *id != shadow) {
                children.push((id.to_string(), path));
            }
        }
    }
    children.sort();
    children
}
