//! Claude Code transcript 의 완료 경계까지를 공유해도 되는 native 재개 자료로 고정하고, 받은 자료를 검증한다.
//!
//! transcript 에는 대화 외에도 요청 원문(`api-request*`), 계정·조직 UUID(`session-owner`), 지침·메모리·
//! 자격 조직·훅 문맥(`attachment`), 권한 모드 같은 행이 섞여 있다(2.1.319 실측). 그래서 버릴 것을 고르지
//! 않고 남길 것만 고른다: 경계에서 `parentUuid` 를 거슬러 오른 사슬의 user·assistant 행과 compact 경계.
//! 사슬은 마지막 compact 경계(parentUuid=null)에서 멈추므로 compaction 뒤의 작은 자료가 저절로 된다.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use kasa_socket::sessions::is_uuid;
use kasa_socket::shared_rooms::NativeForkSnapshot;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use super::{hex, sha256_hex, ExportLimits, ExportedFork, ForkBoundary, ForkError, Omissions, Omit, SourceRange};

const BOUNDARY_STOPS: [&str; 2] = ["end_turn", "stop_sequence"];
const REMINDER_OPEN: &str = "<system-reminder>";
const REMINDER_CLOSE: &str = "</system-reminder>";
const IMAGE_NOTE: &str = "[공유 포크에서 이미지 생략]";
const DOCUMENT_NOTE: &str = "[공유 포크에서 문서 생략]";

const COMMON_KEYS: [&str; 7] = ["type", "uuid", "parentUuid", "isSidechain", "sessionId", "timestamp", "version"];
const USER_KEYS: [&str; 3] = ["message", "isCompactSummary", "isVisibleInTranscriptOnly"];
const ASSISTANT_KEYS: [&str; 1] = ["message"];
const SYSTEM_KEYS: [&str; 5] = ["subtype", "content", "level", "isMeta", "compactMetadata"];
const ASSISTANT_MESSAGE_KEYS: [&str; 8] = ["id", "type", "role", "model", "content", "stop_reason", "stop_sequence", "usage"];
const USAGE_KEYS: [&str; 4] = ["input_tokens", "output_tokens", "cache_creation_input_tokens", "cache_read_input_tokens"];

/// 첫 훑기에서 행마다 남기는 것. 본문은 사슬이 정해진 뒤 오프셋으로 다시 읽는다.
struct RowMeta {
    offset: u64,
    len: usize,
    /// 개행까지 포함한 줄 끝.
    end: u64,
    digest: [u8; 32],
    uuid: String,
    parent: Option<String>,
    assistant: bool,
    message_id: Option<String>,
    boundary_stop: bool,
    synthetic: bool,
    sidechain: bool,
    session_id: Option<String>,
}

struct Scan {
    rows: Vec<RowMeta>,
    by_uuid: HashMap<String, usize>,
    /// 대화 사슬에 들지 않는 행(uuid 없음)의 자리와 종류. 경계 앞의 것만 omissions 로 센다.
    loose: Vec<(u64, String)>,
    source: SourceRange,
    bad_lines: usize,
}

pub fn export_claude_snapshot(
    transcript: &Path,
    parent_session_id: &str,
    boundary: ForkBoundary<'_>,
    limits: &ExportLimits,
) -> Result<ExportedFork, ForkError> {
    if !is_uuid(parent_session_id) {
        return Err(ForkError::Invalid("원본 세션 ID 가 UUID 가 아니에요".into()));
    }
    if transcript.file_stem().and_then(|s| s.to_str()) != Some(parent_session_id)
        || transcript.extension().and_then(|s| s.to_str()) != Some("jsonl")
    {
        return Err(ForkError::Invalid("transcript 파일 이름이 원본 세션 ID 와 달라요".into()));
    }
    let link = std::fs::symlink_metadata(transcript)?;
    if !link.is_file() {
        return Err(ForkError::Invalid("transcript 가 일반 파일이 아니에요".into()));
    }
    // 원본은 읽기 전용으로 한 번만 연다. 두 번째 훑기도 같은 핸들이라 사이에 이름이 바뀌어도 같은 파일이다.
    let mut file = std::fs::File::open(transcript)?;
    let scan = scan(&mut file, limits)?;
    let leaf = pick_boundary(&scan, boundary)?;
    if scan.rows[leaf].session_id.as_deref() != Some(parent_session_id) {
        return Err(ForkError::Invalid("경계 행이 원본 세션의 것이 아니에요".into()));
    }
    let chain = chain_to_root(&scan, leaf)?;
    let mut omit = Omissions::default();
    omit.add(Omit::Unreadable, "", scan.bad_lines);
    let leaf_offset = scan.rows[leaf].offset;
    for (_, kind) in scan.loose.iter().filter(|(offset, _)| *offset < leaf_offset) {
        omit.add(loose_kind(kind), kind, 1);
    }
    let mut out = Vec::new();
    for &index in &chain {
        let row = read_row(&mut file, &scan.rows[index])?;
        if let Some(clean) = sanitize_row(&row, parent_session_id, &mut omit)? {
            out.push(clean);
        }
    }
    let boundary_id = scan.rows[leaf].uuid.clone();
    if out.last().and_then(|r| r["uuid"].as_str()) != Some(boundary_id.as_str()) {
        return Err(ForkError::Unsupported("경계 행이 공유 형식에 남지 않아요".into()));
    }
    relink(&mut out);
    check_tool_pairs(&out)?;
    let last = out.last().expect("경계 행을 위에서 확인했다");
    let native_version = last["version"].as_str().unwrap_or_default().to_string();
    let boundary_at_ms = last["timestamp"].as_str().and_then(parse_rfc3339_ms);
    let boundary_label = boundary_label(&out);
    let mut jsonl = String::new();
    for row in &out {
        jsonl.push_str(&serde_json::to_string(row).map_err(|e| ForkError::Invalid(e.to_string()))?);
        jsonl.push('\n');
    }
    let snapshot = NativeForkSnapshot {
        harness: "claude".into(),
        parent_session_id: parent_session_id.into(),
        boundary_id,
        boundary_label,
        native_version,
        sha256: sha256_hex(jsonl.as_bytes()),
        jsonl,
    };
    verify_claude_snapshot(&snapshot, limits)?;
    let boundary_end = scan.rows[leaf].end;
    Ok(ExportedFork { snapshot, omissions: omit.into_vec(), boundary_at_ms, boundary_end, source: scan.source })
}

fn scan(file: &mut std::fs::File, limits: &ExportLimits) -> Result<Scan, ForkError> {
    let len_at_read = file.metadata()?.len();
    if len_at_read > limits.max_source_bytes {
        return Err(ForkError::Unsupported(format!(
            "원본 기록이 {len_at_read}바이트로 읽기 한도 {}바이트를 넘어요",
            limits.max_source_bytes
        )));
    }
    let mut reader = BufReader::new(Read::by_ref(file).take(len_at_read));
    let mut prefix = Sha256::new();
    let (mut offset, mut rows, mut by_uuid, mut loose, mut bad_lines) = (0u64, Vec::new(), HashMap::new(), Vec::new(), 0usize);
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = reader.read_until(b'\n', &mut line)?;
        if n == 0 || line.last() != Some(&b'\n') {
            break;
        }
        prefix.update(&line);
        let start = offset;
        offset += n as u64;
        let body = trim_newline(&line);
        let Ok(row) = serde_json::from_slice::<Value>(body) else {
            bad_lines += 1;
            continue;
        };
        let kind = row["type"].as_str().unwrap_or_default();
        let Some(uuid) = row["uuid"].as_str() else {
            loose.push((start, kind.chars().take(48).collect()));
            continue;
        };
        let message = &row["message"];
        let meta = RowMeta {
            offset: start,
            len: body.len(),
            end: offset,
            digest: Sha256::digest(body).into(),
            uuid: uuid.to_string(),
            parent: row["parentUuid"].as_str().map(str::to_string),
            assistant: kind == "assistant",
            message_id: message["id"].as_str().map(str::to_string),
            boundary_stop: message["stop_reason"].as_str().is_some_and(|s| BOUNDARY_STOPS.contains(&s)),
            synthetic: row["isApiErrorMessage"].as_bool() == Some(true) || message["model"].as_str() == Some("<synthetic>"),
            sidechain: row["isSidechain"].as_bool() == Some(true),
            session_id: row["sessionId"].as_str().map(str::to_string),
        };
        // claude 도 같은 uuid 는 나중 행을 쓴다.
        by_uuid.insert(meta.uuid.clone(), rows.len());
        rows.push(meta);
    }
    Ok(Scan {
        rows,
        by_uuid,
        loose,
        source: SourceRange { prefix_len: offset, prefix_sha256: hex(&prefix.finalize()), len_at_read },
        bad_lines,
    })
}

fn trim_newline(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

fn read_row(file: &mut std::fs::File, meta: &RowMeta) -> Result<Value, ForkError> {
    let mut body = vec![0u8; meta.len];
    file.seek(SeekFrom::Start(meta.offset))?;
    file.read_exact(&mut body)?;
    if <[u8; 32]>::from(Sha256::digest(&body)) != meta.digest {
        return Err(ForkError::Conflict("고정하는 사이에 원본의 앞부분이 바뀌었어요".into()));
    }
    serde_json::from_slice(&body).map_err(|e| ForkError::Invalid(e.to_string()))
}

/// 완료된 턴 끝: 합성 오류가 아닌 본 대화 assistant 행이 end_turn·stop_sequence 로 끝났고, 같은 메시지가
/// 뒤에 더 이어지지 않는다. 쓰는 중인 메시지는 stop_reason 이 비어 있어 여기서 빠진다.
fn is_turn_end(scan: &Scan, index: usize, followers: &HashMap<&str, Vec<usize>>) -> bool {
    let row = &scan.rows[index];
    if !row.assistant || row.sidechain || row.synthetic || !row.boundary_stop || row.message_id.is_none() {
        return false;
    }
    if scan.by_uuid.get(&row.uuid) != Some(&index) {
        return false;
    }
    !followers.get(row.uuid.as_str()).is_some_and(|next| {
        next.iter().any(|&i| scan.rows[i].assistant && scan.rows[i].message_id == row.message_id)
    })
}

fn pick_boundary(scan: &Scan, boundary: ForkBoundary<'_>) -> Result<usize, ForkError> {
    let mut followers: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, row) in scan.rows.iter().enumerate() {
        if let Some(parent) = row.parent.as_deref() {
            followers.entry(parent).or_default().push(i);
        }
    }
    match boundary {
        ForkBoundary::LatestComplete => (0..scan.rows.len())
            .rev()
            .find(|&i| is_turn_end(scan, i, &followers))
            .ok_or_else(|| ForkError::Incomplete("완료된 턴이 아직 없어요".into())),
        ForkBoundary::Turn(uuid) => {
            let &index = scan.by_uuid.get(uuid).ok_or_else(|| ForkError::Invalid("고른 지점이 기록에 없어요".into()))?;
            if is_turn_end(scan, index, &followers) {
                Ok(index)
            } else {
                Err(ForkError::Incomplete("고른 지점은 완료된 턴의 끝이 아니에요".into()))
            }
        }
    }
}

/// 경계에서 루트까지 거슬러 오른 사슬(루트가 앞). 이어지지 않는 부모는 기록이 잘린 것이라 거부한다.
fn chain_to_root(scan: &Scan, leaf: usize) -> Result<Vec<usize>, ForkError> {
    let mut chain = vec![leaf];
    let mut seen = HashSet::from([leaf]);
    while let Some(parent) = scan.rows[*chain.last().expect("비지 않는다")].parent.as_deref() {
        let &index = scan.by_uuid.get(parent).ok_or_else(|| ForkError::Invalid("대화 사슬의 부모 행이 기록에 없어요".into()))?;
        if !seen.insert(index) {
            return Err(ForkError::Invalid("대화 사슬이 고리를 이뤄요".into()));
        }
        chain.push(index);
    }
    chain.reverse();
    Ok(chain)
}

/// 대화 밖 행 종류의 분류. 모르는 종류도 「기타」로 세어 빠뜨리지 않는다.
fn loose_kind(kind: &str) -> Omit {
    match kind {
        "session-owner" => Omit::Account,
        "api-request" | "api-request-blob" | "api-request-shape" | "cost-state" | "tools-used" => Omit::Request,
        "permission-mode" | "mode" | "atis-latch" => Omit::Settings,
        "file-history-snapshot" | "file-history-delta" => Omit::Attachment,
        "custom-title" | "ai-title" | "agent-name" | "tag" | "last-prompt" | "pr-link" | "queue-operation" | "summary" => Omit::Display,
        _ => Omit::Other,
    }
}

/// attachment 는 하네스가 대화에 끼워 넣는 문맥이다. 지침·메모리·훅 출력이 대부분이고, 일부는 계정·설정·파일 내용이다.
fn attachment_kind(kind: &str) -> Omit {
    match kind {
        "credential_org" => Omit::Account,
        "command_permissions" | "auto_mode" | "plan_mode" | "plan_mode_exit" | "model" => Omit::Settings,
        "file" | "edited_text_file" | "compact_file_reference" | "plan_file_reference" | "read_truncation_notice" => Omit::Attachment,
        _ => Omit::Injected,
    }
}

fn sanitize_row(row: &Value, parent: &str, omit: &mut Omissions) -> Result<Option<Value>, ForkError> {
    let kind = row["type"].as_str().unwrap_or_default();
    let mut out = Map::new();
    out.insert("type".into(), json!(kind));
    out.insert("uuid".into(), row["uuid"].clone());
    out.insert("parentUuid".into(), Value::Null);
    out.insert("isSidechain".into(), json!(false));
    out.insert("sessionId".into(), json!(parent));
    for key in ["timestamp", "version"] {
        if let Some(text) = row[key].as_str().filter(|t| t.len() <= 64) {
            out.insert(key.into(), json!(text));
        }
    }
    if row.get("cwd").is_some() || row.get("gitBranch").is_some() {
        omit.add(Omit::SourcePath, "cwd·gitBranch", 1);
    }
    if row.get("permissionMode").is_some() {
        omit.add(Omit::Settings, "permissionMode", 1);
    }
    if row.get("toolUseResult").is_some() {
        omit.add(Omit::Display, "도구 상세 toolUseResult", 1);
    }
    match kind {
        "user" => {
            let summary = row["isCompactSummary"].as_bool() == Some(true);
            if row["isMeta"].as_bool() == Some(true) && !summary {
                omit.add(Omit::Injected, "isMeta 사용자 행", 1);
                return Ok(None);
            }
            let Some(content) = user_content(&row["message"]["content"], omit)? else {
                omit.add(Omit::Other, "빈 사용자 행", 1);
                return Ok(None);
            };
            out.insert("message".into(), json!({"role": "user", "content": content}));
            if summary {
                out.insert("isCompactSummary".into(), json!(true));
            }
            if row["isVisibleInTranscriptOnly"].as_bool() == Some(true) {
                out.insert("isVisibleInTranscriptOnly".into(), json!(true));
            }
        }
        "assistant" => {
            let message = &row["message"];
            if row["isApiErrorMessage"].as_bool() == Some(true) || message["model"].as_str() == Some("<synthetic>") {
                omit.add(Omit::Other, "API 오류 행", 1);
                return Ok(None);
            }
            let content = assistant_content(&message["content"], omit)?;
            if content.is_empty() {
                return Ok(None);
            }
            let mut msg = Map::new();
            msg.insert("type".into(), json!("message"));
            msg.insert("role".into(), json!("assistant"));
            for key in ["id", "model", "stop_reason", "stop_sequence"] {
                msg.insert(key.into(), message[key].clone());
            }
            msg.insert("content".into(), Value::Array(content));
            let usage: Map<String, Value> =
                USAGE_KEYS.iter().filter_map(|&k| message["usage"][k].as_u64().map(|n| (k.to_string(), json!(n)))).collect();
            msg.insert("usage".into(), Value::Object(usage));
            out.insert("message".into(), Value::Object(msg));
        }
        "system" if row["subtype"].as_str() == Some("compact_boundary") => {
            out.insert("subtype".into(), json!("compact_boundary"));
            out.insert("content".into(), json!(row["content"].as_str().unwrap_or("Conversation compacted")));
            out.insert("level".into(), json!(row["level"].as_str().unwrap_or("info")));
            out.insert("isMeta".into(), json!(false));
            let meta = &row["compactMetadata"];
            let mut kept = Map::new();
            if let Some(trigger) = meta["trigger"].as_str() {
                kept.insert("trigger".into(), json!(trigger));
            }
            if let Some(tokens) = meta["preTokens"].as_u64() {
                kept.insert("preTokens".into(), json!(tokens));
            }
            if !kept.is_empty() {
                out.insert("compactMetadata".into(), Value::Object(kept));
            }
        }
        "system" => {
            omit.add(Omit::Display, &format!("system: {}", row["subtype"].as_str().unwrap_or("?")), 1);
            return Ok(None);
        }
        "attachment" => {
            let kind = row["attachment"]["type"].as_str().unwrap_or("?");
            omit.add(attachment_kind(kind), &format!("attachment: {kind}"), 1);
            return Ok(None);
        }
        other => {
            omit.add(Omit::Other, other, 1);
            return Ok(None);
        }
    }
    Ok(Some(Value::Object(out)))
}

fn user_content(content: &Value, omit: &mut Omissions) -> Result<Option<Value>, ForkError> {
    match content {
        Value::String(text) => {
            let text = strip_reminders(text, omit);
            Ok((!text.trim().is_empty()).then(|| json!(text)))
        }
        Value::Array(blocks) => {
            let mut out = Vec::new();
            for block in blocks {
                match block["type"].as_str().unwrap_or_default() {
                    "text" => {
                        let text = strip_reminders(block["text"].as_str().unwrap_or_default(), omit);
                        if !text.trim().is_empty() {
                            out.push(json!({"type": "text", "text": text}));
                        }
                    }
                    "image" => {
                        omit.add(Omit::Image, "사용자", 1);
                        out.push(json!({"type": "text", "text": IMAGE_NOTE}));
                    }
                    "document" => {
                        omit.add(Omit::Attachment, "사용자 문서", 1);
                        out.push(json!({"type": "text", "text": DOCUMENT_NOTE}));
                    }
                    "tool_result" => {
                        let id = block["tool_use_id"]
                            .as_str()
                            .ok_or_else(|| ForkError::Invalid("tool_result 에 tool_use_id 가 없어요".into()))?;
                        let mut result = Map::new();
                        result.insert("type".into(), json!("tool_result"));
                        result.insert("tool_use_id".into(), json!(id));
                        result.insert("content".into(), tool_result_content(&block["content"], omit)?);
                        if block["is_error"].as_bool() == Some(true) {
                            result.insert("is_error".into(), json!(true));
                        }
                        out.push(Value::Object(result));
                    }
                    other => return Err(ForkError::Unsupported(format!("사용자 행의 {other} 블록은 공유 형식에 없어요"))),
                }
            }
            Ok((!out.is_empty()).then_some(Value::Array(out)))
        }
        Value::Null => Ok(None),
        _ => Err(ForkError::Invalid("사용자 행 내용의 꼴이 달라요".into())),
    }
}

fn tool_result_content(content: &Value, omit: &mut Omissions) -> Result<Value, ForkError> {
    match content {
        Value::Null => Ok(json!("")),
        Value::String(text) => Ok(json!(strip_reminders(text, omit))),
        Value::Array(blocks) => {
            let mut out = Vec::new();
            for block in blocks {
                match block["type"].as_str().unwrap_or_default() {
                    "text" => {
                        let text = strip_reminders(block["text"].as_str().unwrap_or_default(), omit);
                        out.push(json!({"type": "text", "text": text}));
                    }
                    "image" => {
                        omit.add(Omit::Image, "도구 결과", 1);
                        out.push(json!({"type": "text", "text": IMAGE_NOTE}));
                    }
                    "document" => {
                        omit.add(Omit::Attachment, "도구 결과 문서", 1);
                        out.push(json!({"type": "text", "text": DOCUMENT_NOTE}));
                    }
                    other => return Err(ForkError::Unsupported(format!("도구 결과의 {other} 블록은 공유 형식에 없어요"))),
                }
            }
            Ok(Value::Array(out))
        }
        _ => Err(ForkError::Invalid("도구 결과 내용의 꼴이 달라요".into())),
    }
}

/// 생각 블록은 수신자에게 보이면 안 되고, 완료된 턴의 것은 다음 요청에서 다시 쓰이지 않아 빼도 된다.
fn assistant_content(content: &Value, omit: &mut Omissions) -> Result<Vec<Value>, ForkError> {
    let Some(blocks) = content.as_array() else {
        return Err(ForkError::Invalid("assistant 행 내용의 꼴이 달라요".into()));
    };
    let mut out = Vec::new();
    for block in blocks {
        match block["type"].as_str().unwrap_or_default() {
            kind @ ("thinking" | "redacted_thinking") => omit.add(Omit::Reasoning, kind, 1),
            "text" => out.push(json!({"type": "text", "text": block["text"].as_str().unwrap_or_default()})),
            "tool_use" => {
                let (Some(id), Some(name)) = (block["id"].as_str(), block["name"].as_str()) else {
                    return Err(ForkError::Invalid("tool_use 에 id·name 이 없어요".into()));
                };
                out.push(json!({"type": "tool_use", "id": id, "name": name, "input": block["input"].clone()}));
            }
            other => return Err(ForkError::Unsupported(format!("assistant 행의 {other} 블록은 공유 형식에 없어요"))),
        }
    }
    Ok(out)
}

/// 하네스가 끼워 넣은 `<system-reminder>` 조각을 뺀다. 원본의 지침·훅 문맥이 여기에 실린다.
fn strip_reminders(text: &str, omit: &mut Omissions) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut removed = 0;
    while let Some(start) = rest.find(REMINDER_OPEN) {
        out.push_str(&rest[..start]);
        removed += 1;
        rest = match rest[start..].find(REMINDER_CLOSE) {
            Some(end) => &rest[start + end + REMINDER_CLOSE.len()..],
            None => "",
        };
    }
    out.push_str(rest);
    omit.add(Omit::Injected, "system-reminder", removed);
    out
}

fn relink(rows: &mut [Value]) {
    let mut prev = Value::Null;
    for row in rows {
        row["parentUuid"] = prev;
        prev = row["uuid"].clone();
    }
}

fn check_tool_pairs(rows: &[Value]) -> Result<(), ForkError> {
    let mut open = Vec::<String>::new();
    for row in rows {
        let Some(blocks) = row["message"]["content"].as_array() else { continue };
        for block in blocks {
            match block["type"].as_str() {
                Some("tool_use") => open.push(block["id"].as_str().unwrap_or_default().to_string()),
                Some("tool_result") => {
                    let id = block["tool_use_id"].as_str().unwrap_or_default();
                    let Some(at) = open.iter().position(|o| o == id) else {
                        return Err(ForkError::Invalid(format!("앞선 도구 호출이 없는 결과가 있어요: {id}")));
                    };
                    open.remove(at);
                }
                _ => {}
            }
        }
    }
    match open.first() {
        Some(id) => Err(ForkError::Incomplete(format!("결과가 없는 도구 호출이 있어요: {id}"))),
        None => Ok(()),
    }
}

fn boundary_label(rows: &[Value]) -> String {
    let prompt = rows.iter().rev().find_map(|row| {
        if row["type"] != "user" || row["isCompactSummary"] == true {
            return None;
        }
        match &row["message"]["content"] {
            Value::String(text) => Some(text.clone()),
            Value::Array(blocks) => blocks.iter().find_map(|b| (b["type"] == "text").then(|| b["text"].as_str().unwrap_or_default().to_string())),
            _ => None,
        }
    });
    let line = prompt.as_deref().and_then(|p| p.lines().map(str::trim).find(|l| !l.is_empty())).unwrap_or("");
    let turns = rows.iter().filter(|r| r["type"] == "assistant" && BOUNDARY_STOPS.contains(&r["message"]["stop_reason"].as_str().unwrap_or(""))).count();
    let clipped: String = line.chars().filter(|c| !c.is_control()).take(60).collect();
    if clipped.is_empty() {
        format!("{turns}번째 완료 턴")
    } else {
        format!("{turns}번째 완료 턴 · {clipped}")
    }
}

/// `2026-11-10T03:12:45.123Z` 꼴만 읽는다. claude 가 쓰는 시각이 이 꼴이다.
pub(super) fn parse_rfc3339_ms(text: &str) -> Option<u64> {
    let b = text.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' || *b.last()? != b'Z' {
        return None;
    }
    let num = |s: &str| s.parse::<i64>().ok();
    let (y, mo, d) = (num(&text[0..4])?, num(&text[5..7])?, num(&text[8..10])?);
    let (h, mi, s) = (num(&text[11..13])?, num(&text[14..16])?, num(&text[17..19])?);
    let frac = &text[19..text.len() - 1];
    let ms = match frac.strip_prefix('.') {
        Some(digits) if !digits.is_empty() && digits.bytes().all(|c| c.is_ascii_digit()) => {
            format!("{digits:0<3}")[..3].parse::<i64>().ok()?
        }
        None if frac.is_empty() => 0,
        _ => return None,
    };
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    // Howard Hinnant 의 days_from_civil.
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if mo > 2 { mo - 3 } else { mo + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(((days * 24 + h) * 60 + mi) * 60 + s).ok().map(|secs| secs * 1000 + ms as u64)
}

/// 다른 사람이 보낸 고정 자료를 믿지 않고 다시 잰다. 고정할 때 만든 꼴과 한 글자라도 다르면 거부한다.
pub fn verify_claude_snapshot(snapshot: &NativeForkSnapshot, limits: &ExportLimits) -> Result<(), ForkError> {
    if snapshot.harness != "claude" {
        return Err(ForkError::Unsupported(format!("{} 자료는 Claude 포크로 받지 않아요", snapshot.harness)));
    }
    let parent = snapshot.parent_session_id.as_str();
    if !is_uuid(parent) || !is_uuid(&snapshot.boundary_id) {
        return Err(ForkError::Invalid("세션·경계 ID 가 UUID 가 아니에요".into()));
    }
    verify_envelope(snapshot, limits)?;
    let jsonl = snapshot.jsonl.as_str();
    let mut prev: Option<String> = None;
    let mut seen = HashSet::new();
    let mut rows = Vec::new();
    for (i, line) in jsonl[..jsonl.len() - 1].split('\n').enumerate() {
        let row: Value = serde_json::from_str(line).map_err(|e| ForkError::Invalid(format!("{}번째 행: {e}", i + 1)))?;
        let bad = |why: &str| ForkError::Invalid(format!("{}번째 행: {why}", i + 1));
        let obj = row.as_object().ok_or_else(|| bad("객체가 아니에요"))?;
        let kind = obj.get("type").and_then(Value::as_str).unwrap_or_default();
        let extra: &[&str] = match kind {
            "user" => &USER_KEYS,
            "assistant" => &ASSISTANT_KEYS,
            "system" => &SYSTEM_KEYS,
            _ => return Err(bad("허용하지 않는 행 종류예요")),
        };
        if let Some(key) = obj.keys().find(|k| !COMMON_KEYS.contains(&k.as_str()) && !extra.contains(&k.as_str())) {
            return Err(bad(&format!("허용하지 않는 키 {key}")));
        }
        let uuid = obj.get("uuid").and_then(Value::as_str).filter(|u| is_uuid(u)).ok_or_else(|| bad("uuid 가 달라요"))?;
        if !seen.insert(uuid.to_string()) {
            return Err(bad("uuid 가 겹쳐요"));
        }
        if obj.get("parentUuid").and_then(Value::as_str) != prev.as_deref() || !obj.contains_key("parentUuid") {
            return Err(bad("parentUuid 사슬이 이어지지 않아요"));
        }
        if obj.get("sessionId").and_then(Value::as_str) != Some(parent) {
            return Err(bad("sessionId 가 원본과 달라요"));
        }
        if obj.get("isSidechain").is_some_and(|v| v != &json!(false)) {
            return Err(bad("곁가지 행이에요"));
        }
        for key in ["timestamp", "version"] {
            if obj.get(key).is_some_and(|v| !v.as_str().is_some_and(|t| t.len() <= 64)) {
                return Err(bad(&format!("{key} 꼴이 달라요")));
            }
        }
        match kind {
            "user" => verify_user(obj).map_err(|why| bad(&why))?,
            "assistant" => verify_assistant(obj).map_err(|why| bad(&why))?,
            _ => verify_system(obj, i).map_err(|why| bad(&why))?,
        }
        prev = Some(uuid.to_string());
        rows.push(row);
    }
    let last = rows.last().ok_or_else(|| ForkError::Invalid("행이 없어요".into()))?;
    if last["type"] != "assistant"
        || last["uuid"].as_str() != Some(snapshot.boundary_id.as_str())
        || !BOUNDARY_STOPS.contains(&last["message"]["stop_reason"].as_str().unwrap_or(""))
    {
        return Err(ForkError::Incomplete("마지막 행이 완료된 턴의 끝이 아니에요".into()));
    }
    check_tool_pairs(&rows)
}

/// 화면용 한 줄. 사슬 정리 없이 고정 자료와 같은 허용 목록만 적용하고, 화면이 쓰는 원래 uuid 이음은 둔다.
pub(super) fn display_row(row: &Value) -> Option<Value> {
    let session = row["sessionId"].as_str().unwrap_or_default().to_string();
    let mut clean = sanitize_row(row, &session, &mut Omissions::default()).ok()??;
    clean["parentUuid"] = row["parentUuid"].as_str().map_or(Value::Null, |p| json!(p));
    Some(clean)
}

/// 하네스와 상관없는 겉 검사: 표시 글, 판 표기, 크기, 해시, 완성된 행.
pub(super) fn verify_envelope(snapshot: &NativeForkSnapshot, limits: &ExportLimits) -> Result<(), ForkError> {
    if snapshot.boundary_label.chars().count() > 200 || snapshot.boundary_label.chars().any(char::is_control) {
        return Err(ForkError::Invalid("경계 이름이 너무 길거나 제어 문자가 있어요".into()));
    }
    if snapshot.native_version.len() > 64 || !snapshot.native_version.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+')) {
        return Err(ForkError::Invalid("native 판 표기가 달라요".into()));
    }
    let jsonl = snapshot.jsonl.as_str();
    if jsonl.len() > limits.max_snapshot_bytes {
        return Err(ForkError::Unsupported(format!(
            "공유할 native 기록이 {}바이트로 한도 {}바이트를 넘어요",
            jsonl.len(),
            limits.max_snapshot_bytes
        )));
    }
    if snapshot.sha256.len() != 64 || !snapshot.sha256.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(ForkError::Invalid("sha256 표기가 달라요".into()));
    }
    if sha256_hex(jsonl.as_bytes()) != snapshot.sha256 {
        return Err(ForkError::Invalid("sha256 이 내용과 맞지 않아요".into()));
    }
    if jsonl.is_empty() || !jsonl.ends_with('\n') {
        return Err(ForkError::Invalid("jsonl 이 완성된 행으로 끝나지 않아요".into()));
    }
    Ok(())
}

fn verify_user(obj: &Map<String, Value>) -> Result<(), String> {
    for key in ["isCompactSummary", "isVisibleInTranscriptOnly"] {
        if obj.get(key).is_some_and(|v| v != &json!(true)) {
            return Err(format!("{key} 꼴이 달라요"));
        }
    }
    let message = obj.get("message").and_then(Value::as_object).ok_or("message 가 없어요")?;
    if message.keys().any(|k| k != "role" && k != "content") || message.get("role") != Some(&json!("user")) {
        return Err("사용자 message 꼴이 달라요".into());
    }
    let clean_text = |text: Option<&Value>| match text.and_then(Value::as_str) {
        Some(t) if !t.contains(REMINDER_OPEN) => Ok(()),
        Some(_) => Err("system-reminder 가 남아 있어요".to_string()),
        None => Err("text 가 문자열이 아니에요".to_string()),
    };
    match message.get("content") {
        Some(Value::String(_)) => clean_text(message.get("content")),
        Some(Value::Array(blocks)) if !blocks.is_empty() => {
            for block in blocks {
                let block = block.as_object().ok_or("블록이 객체가 아니에요")?;
                match block.get("type").and_then(Value::as_str) {
                    Some("text") if block.len() == 2 => clean_text(block.get("text"))?,
                    Some("tool_result") => {
                        if block.keys().any(|k| !matches!(k.as_str(), "type" | "tool_use_id" | "content" | "is_error"))
                            || !block.get("tool_use_id").is_some_and(Value::is_string)
                            || block.get("is_error").is_some_and(|v| v != &json!(true))
                        {
                            return Err("tool_result 꼴이 달라요".into());
                        }
                        match block.get("content") {
                            Some(content @ Value::String(_)) => clean_text(Some(content))?,
                            Some(Value::Array(parts)) => {
                                for part in parts {
                                    let part = part.as_object().ok_or("도구 결과 조각이 객체가 아니에요")?;
                                    if part.len() != 2 || part.get("type") != Some(&json!("text")) {
                                        return Err("도구 결과에 text 외 조각이 있어요".into());
                                    }
                                    clean_text(part.get("text"))?;
                                }
                            }
                            _ => return Err("도구 결과 내용이 없어요".into()),
                        }
                    }
                    _ => return Err("허용하지 않는 사용자 블록이에요".into()),
                }
            }
            Ok(())
        }
        _ => Err("사용자 내용이 비었어요".into()),
    }
}

fn verify_assistant(obj: &Map<String, Value>) -> Result<(), String> {
    let message = obj.get("message").and_then(Value::as_object).ok_or("message 가 없어요")?;
    if let Some(key) = message.keys().find(|k| !ASSISTANT_MESSAGE_KEYS.contains(&k.as_str())) {
        return Err(format!("assistant message 에 허용하지 않는 키 {key}"));
    }
    if message.get("role") != Some(&json!("assistant"))
        || message.get("type") != Some(&json!("message"))
        || !message.get("id").is_some_and(Value::is_string)
        || !message.get("model").is_some_and(Value::is_string)
        || !message.get("stop_reason").is_some_and(Value::is_string)
    {
        return Err("assistant message 꼴이 달라요 — 쓰는 중인 메시지일 수 있어요".into());
    }
    if let Some(usage) = message.get("usage") {
        let usage = usage.as_object().ok_or("usage 꼴이 달라요")?;
        if usage.iter().any(|(k, v)| !USAGE_KEYS.contains(&k.as_str()) || !v.is_u64()) {
            return Err("usage 에 허용하지 않는 값이 있어요".into());
        }
    }
    let blocks = message.get("content").and_then(Value::as_array).filter(|b| !b.is_empty()).ok_or("assistant 내용이 비었어요")?;
    for block in blocks {
        let block = block.as_object().ok_or("블록이 객체가 아니에요")?;
        let ok = match block.get("type").and_then(Value::as_str) {
            Some("text") => block.len() == 2 && block.get("text").is_some_and(Value::is_string),
            Some("tool_use") => {
                block.len() == 4
                    && block.get("id").is_some_and(Value::is_string)
                    && block.get("name").is_some_and(Value::is_string)
                    && block.contains_key("input")
            }
            _ => false,
        };
        if !ok {
            return Err("허용하지 않는 assistant 블록이에요(생각·서버 도구 블록 포함)".into());
        }
    }
    Ok(())
}

fn verify_system(obj: &Map<String, Value>, index: usize) -> Result<(), String> {
    if index != 0 || obj.get("subtype") != Some(&json!("compact_boundary")) {
        return Err("system 행은 맨 앞 compact 경계만 받아요".into());
    }
    if obj.get("isMeta").is_some_and(|v| v != &json!(false))
        || ["content", "level"].iter().any(|k| obj.get(*k).is_some_and(|v| !v.is_string()))
    {
        return Err("compact 경계 꼴이 달라요".into());
    }
    if let Some(meta) = obj.get("compactMetadata") {
        let meta = meta.as_object().ok_or("compactMetadata 꼴이 달라요")?;
        let ok = meta.iter().all(|(k, v)| (k == "trigger" && v.is_string()) || (k == "preTokens" && v.is_u64()));
        if !ok {
            return Err("compactMetadata 에 허용하지 않는 값이 있어요".into());
        }
    }
    Ok(())
}

/// 수신 기기에서 행마다 cwd 를 수신자 작업 폴더로 넣는다. 원본 경로는 고정 자료에 처음부터 없다.
pub(super) fn with_cwd(jsonl: &str, cwd: &str) -> Result<String, ForkError> {
    let mut out = String::with_capacity(jsonl.len() + jsonl.len() / 8);
    for line in jsonl.lines() {
        let mut row: Value = serde_json::from_str(line).map_err(|e| ForkError::Invalid(e.to_string()))?;
        row["cwd"] = json!(cwd);
        out.push_str(&serde_json::to_string(&row).map_err(|e| ForkError::Invalid(e.to_string()))?);
        out.push('\n');
    }
    Ok(out)
}
