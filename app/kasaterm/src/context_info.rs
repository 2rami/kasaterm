//! Worker-only, bounded observations from the selected session's saved records.
//! Saved records cannot establish the bytes sent on a later network request.

use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

const READ_CAP: u64 = 512 * 1024;
const SESSION_CAP: usize = 32;
const RECORD_CAP: usize = 32768;
const NAME_CAP: usize = 256;

#[derive(Clone, Debug)]
pub(crate) struct ContextRequest {
    pub(crate) session_id: String,
    pub(crate) harness: String,
    pub(crate) path: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ContextEvidence {
    pub(crate) available: bool,
    pub(crate) partial: bool,
    pub(crate) incomplete_record: bool,
    pub(crate) limited: bool,
    pub(crate) observed_records: u64,
    pub(crate) observed_at: Option<SystemTime>,
    pub(crate) last_input_tokens: Option<u64>,
    pub(crate) observed_input_tokens: u64,
    pub(crate) observed_output_tokens: u64,
    pub(crate) cumulative_input_tokens: Option<u64>,
    pub(crate) cumulative_output_tokens: Option<u64>,
    pub(crate) skills_available: Option<Vec<String>>,
    pub(crate) skills_read: Vec<String>,
    pub(crate) skills_invoked: Vec<String>,
    pub(crate) mcp_configured: Option<Vec<String>>,
    pub(crate) mcp_called: Vec<String>,
    pub(crate) mcp_connected: Option<Vec<String>>,
    pub(crate) image_read_calls: u64,
    pub(crate) attachment_records: u64,
    pub(crate) image_payload_blocks: u64,
    pub(crate) image_reference_blocks: u64,
    pub(crate) stored_image_payload_bytes: u64,
    pub(crate) max_image_payload_bytes: u64,
    pub(crate) request_bytes: Option<u64>,
    pub(crate) request_size_error: bool,
}

impl ContextEvidence {
    pub(crate) fn summary_lines(&self) -> Vec<String> {
        if !self.available {
            return vec![
                "기록 미확인 · 이 세션의 대화 부담".into(),
                "미확인 · 스킬·MCP 목록과 사용 기록".into(),
                "미확인 · 이미지·전송 용량".into(),
            ];
        }
        let usage = self.last_input_tokens.map_or_else(
            || "미확인 · 마지막 입력 사용량".into(),
            |n| format!("마지막 기록 · 입력 {n} 토큰"),
        );
        let tools = format!(
            "{} · 스킬 읽기 {} · MCP 호출 {}종",
            self.scope(),
            self.skills_read.len(),
            self.mcp_called.len()
        );
        let images = if self.request_size_error {
            "원인 미확인 · 요청 크기 오류 기록".into()
        } else {
            format!(
                "전송 미계측 · 이미지 읽기 {}회 · 첨부 기록 {}건",
                self.image_read_calls, self.attachment_records
            )
        };
        vec![usage, tools, images]
    }

    pub(crate) fn detail_sections(&self) -> Vec<(String, Vec<String>)> {
        if !self.available {
            return vec![("확인 범위".into(), vec!["선택한 세션의 로컬 기록이 없거나 읽을 수 없어요. 원격 기록은 별도로 가져오지 않아요.".into()])];
        }
        let mut range = vec![format!("{} · 중복을 제외한 기록 {}건", self.scope(), self.observed_records),
            "기록에서 관측한 값이에요. 지금 모델에 남아 있는 내용 전체나 느려진 원인을 확정하지 않아요.".into()];
        if self.incomplete_record {
            range.push("아직 쓰는 중인 마지막 기록은 집계하지 않았어요.".into());
        }
        if self.limited {
            range.push("기록 또는 목록이 수집 한도를 넘어 일부만 표시해요.".into());
        }
        let mut tokens = vec![
            self.last_input_tokens.map_or_else(
                || "미확인 · 마지막으로 기록된 입력 사용량".into(),
                |n| format!("마지막 사용량 기록: 입력 {n} 토큰 (캐시 포함)"),
            ),
            "오류가 난 요청의 사용량이 없으면 앞선 응답 기록이 남을 수 있어요.".into(),
            "토큰은 전송 바이트와 다른 단위예요.".into(),
        ];
        if self.last_input_tokens.is_some() && self.cumulative_input_tokens.is_none() {
            tokens.push(format!(
                "확인한 응답 기록의 입력: {} 토큰 · 출력: {} 토큰",
                self.observed_input_tokens, self.observed_output_tokens
            ));
        }
        if let (Some(i), Some(o)) = (self.cumulative_input_tokens, self.cumulative_output_tokens) {
            tokens.push(format!(
                "하네스가 보고한 세션 누적: 입력 {i} · 출력 {o} 토큰"
            ));
        }
        let mut skills = list(
            "세션에 제시된 사용 가능 스킬",
            self.skills_available.as_deref(),
        );
        skills.extend(list("읽기 응답이 확인된 스킬", Some(&self.skills_read)));
        skills.extend(list("스킬 호출 기록", Some(&self.skills_invoked)));
        skills.push(
            "목록에 있다고 본문을 읽은 것은 아니에요. 셸로 읽은 스킬은 식별하지 못할 수 있어요."
                .into(),
        );
        let mut mcp = list("세션에 보고된 MCP 설정", self.mcp_configured.as_deref());
        mcp.extend(list("MCP 호출 기록", Some(&self.mcp_called)));
        mcp.extend(list("연결 성공 기록", self.mcp_connected.as_deref()));
        mcp.push("현재 연결 미확인 · 연결 기록은 기록 당시 상태예요.".into());
        let mut images = vec![
            format!("이미지 읽기 도구 호출: {}회", self.image_read_calls),
            format!("이미지 첨부가 표시된 기록: {}건", self.attachment_records),
            format!(
                "본문 없이 참조만 표시된 이미지 블록: {}개",
                self.image_reference_blocks
            ),
            format!(
                "저장된 이미지 payload: {}블록 · 인코딩 문자열 {}바이트 · 가장 큰 블록 {}바이트",
                self.image_payload_blocks,
                self.stored_image_payload_bytes,
                self.max_image_payload_bytes
            ),
            "위 값은 확인한 기록 안의 값이에요. 참조만 남은 이미지의 본문 크기는 미확인이에요."
                .into(),
            "미계측 · 실제 요청 전송 바이트. 파일 크기를 요청 크기로 보지 않아요.".into(),
        ];
        if self.partial {
            images
                .push("확인한 구간이 0건이어도 전체 대화에 이미지가 없다는 뜻은 아니에요.".into());
        }
        if self.request_size_error {
            images.push(
                "원인 미확인 · 요청 크기 오류 기록이 있어요. 이미지 때문이라고 단정하지 않아요."
                    .into(),
            );
        }
        vec![
            ("확인 범위".into(), range),
            ("토큰".into(), tokens),
            ("스킬".into(), skills),
            ("MCP".into(), mcp),
            ("이미지·요청 용량".into(), images),
        ]
    }

    fn scope(&self) -> &'static str {
        if self.partial || self.limited {
            "확인한 일부 기록 · 전체 미확인"
        } else {
            "저장된 기록 기준"
        }
    }
}

fn list(label: &str, names: Option<&[String]>) -> Vec<String> {
    match names {
        None => vec![format!("미확인 · {label}")],
        Some([]) => vec![format!("기록 없음 · {label} (확인한 구간)")],
        Some(names) => std::iter::once(format!("{label}: {}개", names.len()))
            .chain(names.iter().map(|name| format!("  {name}")))
            .collect(),
    }
}

#[derive(Default)]
struct Accumulator {
    evidence: ContextEvidence,
    seen: HashSet<u64>,
    pending_skills: HashMap<String, String>,
    claude_usage: HashMap<String, (u64, u64)>,
}

impl Accumulator {
    fn record(&mut self, raw: &str) {
        let Ok(value) = serde_json::from_str::<Value>(raw) else {
            self.evidence.partial = true;
            return;
        };
        if self.seen.len() >= RECORD_CAP {
            self.evidence.limited = true;
            return;
        }
        let identity = value.get("uuid").and_then(Value::as_str).unwrap_or(raw);
        if !self.seen.insert(digest(identity.as_bytes())) {
            return;
        }
        self.evidence.observed_records += 1;
        let typ = text(&value, "type");
        let payload = value.get("payload").unwrap_or(&value);
        if typ == "assistant" {
            if let Some(usage) = value
                .pointer("/message/usage")
                .filter(|usage| usage.get("input_tokens").and_then(Value::as_u64).is_some())
            {
                let input = num(usage, "input_tokens")
                    .saturating_add(num(usage, "cache_read_input_tokens"))
                    .saturating_add(num(usage, "cache_creation_input_tokens"));
                let output = num(usage, "output_tokens");
                self.evidence.last_input_tokens = Some(input);
                let id = value
                    .pointer("/message/id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("record:{}", self.evidence.observed_records));
                let previous = self
                    .claude_usage
                    .insert(id, (input, output))
                    .unwrap_or_default();
                self.evidence.observed_input_tokens = self
                    .evidence
                    .observed_input_tokens
                    .saturating_sub(previous.0)
                    .saturating_add(input);
                self.evidence.observed_output_tokens = self
                    .evidence
                    .observed_output_tokens
                    .saturating_sub(previous.1)
                    .saturating_add(output);
            }
        }
        if typ == "event_msg" && text(payload, "type") == "token_count" {
            if let Some(input) = payload
                .pointer("/info/last_token_usage/input_tokens")
                .and_then(Value::as_u64)
            {
                self.evidence.last_input_tokens = Some(input);
            }
            if let Some(totals) = payload.pointer("/info/total_token_usage") {
                self.evidence.cumulative_input_tokens =
                    totals.get("input_tokens").and_then(Value::as_u64);
                self.evidence.cumulative_output_tokens =
                    totals.get("output_tokens").and_then(Value::as_u64);
            }
        }
        if typ == "system" && text(&value, "subtype") == "init" {
            if let Some(servers) = value.get("mcp_servers").and_then(Value::as_array) {
                self.evidence.mcp_configured = Some(Vec::new());
                self.evidence.mcp_connected = Some(Vec::new());
                for server in servers {
                    add_optional(&mut self.evidence.mcp_configured, text(server, "name"));
                    if text(server, "status") == "connected" {
                        add_optional(&mut self.evidence.mcp_connected, text(server, "name"));
                    }
                }
            }
            if let Some(skills) = value.get("skills").and_then(Value::as_array) {
                self.evidence.skills_available = Some(Vec::new());
                for skill in skills {
                    add_optional(
                        &mut self.evidence.skills_available,
                        skill.as_str().unwrap_or_else(|| text(skill, "name")),
                    );
                }
            }
        }
        if typ == "session_meta" {
            for field in [
                "base_instructions",
                "developer_instructions",
                "user_instructions",
            ] {
                if let Some(instructions) = payload.get(field) {
                    self.skill_catalog(
                        instructions
                            .as_str()
                            .unwrap_or_else(|| text(instructions, "text")),
                    );
                }
            }
        }
        if typ == "response_item" {
            match text(payload, "type") {
                "function_call" => {
                    let arguments = payload
                        .get("arguments")
                        .and_then(Value::as_str)
                        .and_then(|s| serde_json::from_str::<Value>(s).ok())
                        .unwrap_or(Value::Null);
                    self.tool(text(payload, "name"), &arguments, text(payload, "call_id"));
                }
                "function_call_output" => {
                    self.tool_result(text(payload, "call_id"), payload.get("output"), false)
                }
                _ => {}
            }
        }
        let mut input_image = false;
        for pointer in ["/message/content", "/payload/content", "/attachment/prompt"] {
            if let Some(blocks) = value.pointer(pointer).and_then(Value::as_array) {
                for block in blocks {
                    self.block(block);
                    input_image |= matches!(text(block, "type"), "image" | "input_image")
                        && (typ == "user"
                            || text(payload, "role") == "user"
                            || typ == "attachment");
                    if matches!(text(block, "type"), "text" | "input_text") {
                        self.skill_catalog(text(block, "text"));
                    }
                }
            }
        }
        let is_attachment = input_image
            || value
                .get("imagePasteIds")
                .and_then(Value::as_array)
                .is_some_and(|v| !v.is_empty())
            || value
                .pointer("/attachment/imagePasteIds")
                .and_then(Value::as_array)
                .is_some_and(|v| !v.is_empty())
            || payload
                .get("images")
                .and_then(Value::as_array)
                .is_some_and(|v| !v.is_empty())
            || payload
                .get("local_images")
                .and_then(Value::as_array)
                .is_some_and(|v| !v.is_empty());
        if is_attachment {
            self.evidence.attachment_records += 1;
        }
        let is_error = typ == "error"
            || (typ == "system" && matches!(text(&value, "subtype"), "api_error" | "error"))
            || (typ == "event_msg" && matches!(text(payload, "type"), "error" | "stream_error"))
            || value.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true);
        if is_error {
            // Only an error record is evidence; a user discussing the limit is not.
            let lower = raw.to_ascii_lowercase();
            self.evidence.request_size_error |= lower.contains("request too large")
                || lower.contains("request_too_large")
                || lower.contains("payload too large");
        }
    }

    fn skill_catalog(&mut self, content: &str) {
        // A folder installed on disk says nothing about what this session was shown.
        let Some(start) = content.find("<skills_instructions>") else {
            return;
        };
        let section = &content[start..];
        let section = section
            .split("</skills_instructions>")
            .next()
            .unwrap_or(section);
        let Some((_, catalog)) = section.split_once("### Available skills") else {
            return;
        };
        self.evidence.skills_available = Some(Vec::new());
        for line in catalog.lines() {
            let Some(entry) = line.trim().strip_prefix("- ") else {
                continue;
            };
            if !entry.contains("(file:") {
                continue;
            }
            if let Some((name, _)) = entry.split_once(": ") {
                if self
                    .evidence
                    .skills_available
                    .as_ref()
                    .is_some_and(|v| v.len() >= NAME_CAP)
                {
                    self.evidence.limited = true;
                    break;
                }
                add_optional(&mut self.evidence.skills_available, name);
            }
        }
    }

    fn block(&mut self, block: &Value) {
        match text(block, "type") {
            "tool_use" => self.tool(
                text(block, "name"),
                block.get("input").unwrap_or(&Value::Null),
                text(block, "id"),
            ),
            "tool_result" => {
                self.tool_result(
                    text(block, "tool_use_id"),
                    block.get("content"),
                    block.get("is_error").and_then(Value::as_bool) == Some(true),
                );
                if let Some(blocks) = block.get("content").and_then(Value::as_array) {
                    for child in blocks {
                        self.image(child);
                    }
                }
            }
            "image" | "input_image" => self.image(block),
            _ => {}
        }
    }

    fn image(&mut self, block: &Value) {
        if !matches!(text(block, "type"), "image" | "input_image") {
            return;
        }
        let data = block
            .pointer("/source/data")
            .and_then(Value::as_str)
            .or_else(|| {
                block
                    .get("image_url")
                    .and_then(Value::as_str)
                    .filter(|v| v.starts_with("data:"))
                    .and_then(|v| v.split_once(',').map(|(_, data)| data))
            });
        if let Some(data) = data {
            let bytes = data.len() as u64;
            self.evidence.image_payload_blocks += 1;
            self.evidence.stored_image_payload_bytes += bytes;
            self.evidence.max_image_payload_bytes =
                self.evidence.max_image_payload_bytes.max(bytes);
        } else {
            self.evidence.image_reference_blocks += 1;
        }
    }

    fn tool(&mut self, name: &str, args: &Value, id: &str) {
        let leaf = name.rsplit('.').next().unwrap_or(name);
        let path = args
            .get("file_path")
            .or_else(|| args.get("path"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if leaf == "Skill" {
            add(&mut self.evidence.skills_invoked, text(args, "skill"));
        }
        if matches!(leaf, "Read" | "read_file")
            && Path::new(path).file_name().is_some_and(|p| p == "SKILL.md")
            && !id.is_empty()
            && self.pending_skills.len() < NAME_CAP
        {
            let name = Path::new(path)
                .parent()
                .and_then(Path::file_name)
                .and_then(|s| s.to_str())
                .unwrap_or("SKILL.md");
            self.pending_skills.insert(id.into(), name.into());
        }
        if let Some(mcp) = name.find("mcp__") {
            let server = name[mcp + 5..].split("__").next().unwrap_or("");
            add(&mut self.evidence.mcp_called, server);
        }
        let image_path = Path::new(path)
            .extension()
            .and_then(|s| s.to_str())
            .is_some_and(|s| {
                matches!(
                    s.to_ascii_lowercase().as_str(),
                    "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "heic"
                )
            });
        if leaf == "view_image" || (leaf == "Read" && image_path) {
            self.evidence.image_read_calls += 1;
        }
    }

    fn tool_result(&mut self, id: &str, content: Option<&Value>, failed: bool) {
        if let Some(skill) = self.pending_skills.remove(id) {
            // A call alone does not show whether the model received the file.
            let nonempty = content.is_some_and(|c| {
                c.as_str().is_some_and(|s| !s.trim().is_empty())
                    || c.as_array().is_some_and(|v| !v.is_empty())
            });
            if !failed && nonempty {
                add(&mut self.evidence.skills_read, &skill);
            }
        }
    }
}

fn add(names: &mut Vec<String>, name: &str) {
    let name = name.trim();
    if name.is_empty()
        || name.len() > 200
        || name.chars().any(char::is_control)
        || names.len() >= NAME_CAP
        || names.iter().any(|v| v == name)
    {
        return;
    }
    names.push(name.into());
    names.sort();
}
fn add_optional(names: &mut Option<Vec<String>>, name: &str) {
    add(names.get_or_insert_with(Vec::new), name);
}
fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}
fn num(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}
fn digest(bytes: &[u8]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

#[derive(Default)]
struct CachedSession {
    accumulator: Accumulator,
    offset: u64,
    len: u64,
    mtime: Option<SystemTime>,
    file_id: u128,
    prefix: Vec<u8>,
    edge: Vec<u8>,
}

impl CachedSession {
    fn update(&mut self, path: &Path) -> std::io::Result<ContextEvidence> {
        let mut file = std::fs::File::open(path)?;
        let metadata = file.metadata()?;
        let len = metadata.len();
        let mtime = metadata.modified().ok();
        let id = file_id(&metadata);
        if self.accumulator.evidence.available
            && len == self.len
            && mtime == self.mtime
            && id == self.file_id
        {
            return Ok(self.accumulator.evidence.clone());
        }
        let prefix = read_at(&mut file, 0, len.min(128))?;
        let old_edge = read_at(
            &mut file,
            self.offset.saturating_sub(self.edge.len() as u64),
            self.edge.len() as u64,
        )?;
        let changed_prefix = !self.prefix.is_empty() && !prefix.starts_with(&self.prefix);
        let reset = !self.accumulator.evidence.available
            || id != self.file_id
            || len < self.len
            || (len == self.len && mtime != self.mtime)
            || changed_prefix
            || old_edge != self.edge
            || len.saturating_sub(self.offset) > READ_CAP;
        if reset {
            self.accumulator = Accumulator::default();
            self.offset = len.saturating_sub(READ_CAP);
            self.accumulator.evidence.partial = self.offset > 0;
        }
        let start = self.offset;
        let bytes = read_at(&mut file, start, (len - start).min(READ_CAP))?;
        let end = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        let first = if reset && start > 0 {
            bytes[..end]
                .iter()
                .position(|b| *b == b'\n')
                .map_or(end, |i| i + 1)
        } else {
            0
        };
        for raw in bytes[first..end]
            .split(|b| *b == b'\n')
            .filter(|s| !s.is_empty())
        {
            if let Ok(raw) = std::str::from_utf8(raw) {
                self.accumulator.record(raw);
            } else {
                self.accumulator.evidence.partial = true;
            }
        }
        self.offset = start + end as u64;
        self.edge = read_at(
            &mut file,
            self.offset.saturating_sub(128),
            self.offset.min(128),
        )?;
        self.prefix = prefix;
        self.len = len;
        self.mtime = mtime;
        self.file_id = id;
        self.accumulator.evidence.available = true;
        self.accumulator.evidence.incomplete_record = self.offset < len;
        self.accumulator.evidence.observed_at = Some(SystemTime::now());
        Ok(self.accumulator.evidence.clone())
    }
}

fn read_at(file: &mut std::fs::File, start: u64, count: u64) -> std::io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::with_capacity(count.min(READ_CAP) as usize);
    file.take(count.min(READ_CAP)).read_to_end(&mut bytes)?;
    Ok(bytes)
}
fn file_id(metadata: &std::fs::Metadata) -> u128 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ((metadata.dev() as u128) << 64) | metadata.ino() as u128
    }
    #[cfg(not(unix))]
    {
        metadata
            .created()
            .ok()
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos())
    }
}

/// Call only from the Info worker. The cache retains metadata, never transcript text or image data.
pub(crate) fn snapshot(request: &ContextRequest) -> ContextEvidence {
    static CACHE: OnceLock<Mutex<HashMap<(String, String, PathBuf), CachedSession>>> =
        OnceLock::new();
    let Some(path) = &request.path else {
        return ContextEvidence::default();
    };
    let key = (
        request.session_id.clone(),
        request.harness.clone(),
        path.clone(),
    );
    let Ok(mut cache) = CACHE.get_or_init(Default::default).lock() else {
        return ContextEvidence::default();
    };
    if !cache.contains_key(&key) && cache.len() >= SESSION_CAP {
        let oldest = cache
            .iter()
            .min_by_key(|(_, v)| v.accumulator.evidence.observed_at)
            .map(|(k, _)| k.clone());
        if let Some(oldest) = oldest {
            cache.remove(&oldest);
        }
    }
    match cache.entry(key.clone()).or_default().update(path) {
        Ok(evidence) => evidence,
        Err(_) => {
            cache.remove(&key);
            ContextEvidence::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn parse(records: &[Value]) -> ContextEvidence {
        let mut accumulator = Accumulator::default();
        for record in records {
            accumulator.record(&record.to_string());
        }
        accumulator.evidence
    }

    #[test]
    fn claude_last_request_includes_cache_but_duplicate_messages_do_not_inflate_totals() {
        let record = serde_json::json!({"type":"assistant","uuid":"one","message":{"id":"msg","usage":{"input_tokens":10,"cache_read_input_tokens":20,"cache_creation_input_tokens":30,"output_tokens":4}}});
        let mut next = record.clone();
        next["uuid"] = "two".into();
        next["message"]["usage"]["output_tokens"] = 9.into();
        let evidence = parse(&[record.clone(), record, next]);
        assert_eq!(evidence.last_input_tokens, Some(60));
        assert_eq!(
            (
                evidence.observed_input_tokens,
                evidence.observed_output_tokens
            ),
            (60, 9)
        );
        assert_eq!(evidence.observed_records, 2);
        assert_eq!(evidence.request_bytes, None);
    }

    #[test]
    fn codex_reports_latest_totals_without_adding_cached_tokens_or_summing_snapshots() {
        let usage = |input| serde_json::json!({"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":42,"cached_input_tokens":40},"total_token_usage":{"input_tokens":input,"cached_input_tokens":900,"output_tokens":70}}}});
        let evidence = parse(&[usage(1000), usage(2000)]);
        assert_eq!(evidence.last_input_tokens, Some(42));
        assert_eq!(evidence.cumulative_input_tokens, Some(2000));
        assert_eq!(evidence.observed_input_tokens, 0);
    }

    #[test]
    fn available_configured_called_and_successfully_read_are_distinct() {
        let evidence = parse(&[
            serde_json::json!({"type":"system","subtype":"init","skills":["design","research"],"mcp_servers":[{"name":"docs","status":"connected"},{"name":"search","status":"failed"}]}),
            serde_json::json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"r","name":"Read","input":{"file_path":"/skills/design/SKILL.md"}},{"type":"tool_use","id":"m","name":"mcp__docs__search","input":{}}]}}),
            serde_json::json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"r","content":"instructions","is_error":false}]}}),
        ]);
        assert_eq!(evidence.skills_available.unwrap().len(), 2);
        assert_eq!(evidence.skills_read, ["design"]);
        assert_eq!(evidence.mcp_configured.unwrap(), ["docs", "search"]);
        assert_eq!(evidence.mcp_connected.unwrap(), ["docs"]);
        assert_eq!(evidence.mcp_called, ["docs"]);
    }

    #[test]
    fn missing_payload_and_image_calls_never_establish_request_size_or_cause() {
        let evidence = parse(&[
            serde_json::json!({"type":"response_item","payload":{"type":"function_call","name":"view_image","call_id":"c","arguments":"{\"path\":\"/tmp/a.png\"}"}}),
            serde_json::json!({"type":"user","imagePasteIds":[1],"message":{"content":[{"type":"text","text":"Request too large 32MB"}]}}),
            serde_json::json!({"type":"response_item","payload":{"type":"message","content":[{"type":"input_image","image_url":"file:///tmp/a.png"}]}}),
        ]);
        assert_eq!(evidence.image_read_calls, 1);
        assert_eq!(evidence.attachment_records, 1);
        assert_eq!(evidence.stored_image_payload_bytes, 0);
        assert_eq!(evidence.request_bytes, None);
        assert!(!evidence.request_size_error);
        assert!(evidence.skills_available.is_none());
    }

    #[test]
    fn saved_payload_bytes_count_encoded_data_without_decoding() {
        let evidence = parse(&[
            serde_json::json!({"type":"user","message":{"content":[{"type":"image","source":{"data":"NOTBASE64!","type":"base64"}}]}}),
            serde_json::json!({"type":"error","error":{"message":"Request too large (max 32MB)"}}),
        ]);
        assert_eq!(evidence.stored_image_payload_bytes, 10);
        assert_eq!(evidence.max_image_payload_bytes, 10);
        assert!(evidence.request_size_error);
    }

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "kasaterm-context-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> PathBuf {
            self.0.join("session.jsonl")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn incremental_read_defers_partial_lines_and_recovers_from_rotation_and_truncation() {
        let fixture = Fixture::new();
        let path = fixture.path();
        std::fs::write(&path, b"{\"type\":\"user\",\"uuid\":\"one\"}\n{\"type\":").unwrap();
        let mut cache = CachedSession::default();
        assert_eq!(cache.update(&path).unwrap().observed_records, 1);
        assert!(cache.update(&path).unwrap().incomplete_record);
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"\"user\",\"uuid\":\"two\"}\n")
            .unwrap();
        assert_eq!(cache.update(&path).unwrap().observed_records, 2);
        std::fs::write(&path, b"{\"type\":\"user\",\"uuid\":\"new\"}\n").unwrap();
        assert_eq!(cache.update(&path).unwrap().observed_records, 1);
        std::fs::rename(&path, fixture.0.join("old.jsonl")).unwrap();
        std::fs::write(&path, b"{\"type\":\"user\",\"uuid\":\"replacement\"}\n").unwrap();
        assert_eq!(cache.update(&path).unwrap().observed_records, 1);
    }

    #[test]
    fn huge_record_is_bounded_and_tail_zero_is_not_whole_conversation_zero() {
        let fixture = Fixture::new();
        let path = fixture.path();
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(&vec![b'x'; READ_CAP as usize + 1]).unwrap();
        file.write_all(b"\n{\"type\":\"user\",\"uuid\":\"last\"}\n")
            .unwrap();
        let evidence = CachedSession::default().update(&path).unwrap();
        assert_eq!(evidence.observed_records, 1);
        assert!(evidence.partial);
        assert!(evidence
            .detail_sections()
            .iter()
            .flat_map(|(_, lines)| lines)
            .any(|s| s.contains("전체 대화")));
    }

    #[test]
    fn missing_path_is_unknown_instead_of_empty_inventory() {
        let evidence = snapshot(&ContextRequest {
            session_id: "remote".into(),
            harness: "claude".into(),
            path: None,
        });
        assert!(!evidence.available);
        assert!(evidence.skills_available.is_none());
        assert!(evidence.mcp_connected.is_none());
        assert_eq!(evidence.request_bytes, None);
    }

    #[test]
    fn catalog_is_session_evidence_and_failed_reads_stay_unconfirmed() {
        let evidence = parse(&[
            serde_json::json!({"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"<skills_instructions>\n### Available skills\n- plugin:design: Draw UI. (file: /skills/design/SKILL.md)\n</skills_instructions>"}]}}),
            serde_json::json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"read","name":"Read","input":{"file_path":"/skills/design/SKILL.md"}}]}}),
            serde_json::json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"read","is_error":true,"content":"no permission"}]}}),
            serde_json::json!({"type":"assistant","message":{"usage":{}}}),
        ]);
        assert_eq!(evidence.skills_available.unwrap(), ["plugin:design"]);
        assert!(evidence.skills_read.is_empty());
        assert_eq!(evidence.last_input_tokens, None);
    }

    #[test]
    fn attachment_reference_is_not_payload_or_request_bytes() {
        let evidence = parse(&[
            serde_json::json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_image","image_url":"file:///tmp/photo.png"}]}}),
        ]);
        assert_eq!(evidence.attachment_records, 1);
        assert_eq!(evidence.image_reference_blocks, 1);
        assert_eq!(evidence.image_payload_blocks, 0);
        assert_eq!(evidence.request_bytes, None);
    }

    #[test]
    fn cache_identity_and_partial_summaries_do_not_claim_whole_session_counts() {
        let fixture = Fixture::new();
        let path = fixture.path();
        std::fs::write(&path, b"{\"type\":\"user\",\"uuid\":\"one\"}\n").unwrap();
        let first = snapshot(&ContextRequest {
            session_id: "machine-a:one".into(),
            harness: "claude".into(),
            path: Some(path.clone()),
        });
        let other = snapshot(&ContextRequest {
            session_id: "machine-b:two".into(),
            harness: "codex".into(),
            path: None,
        });
        assert!(first.available);
        assert!(!other.available);
        let partial = ContextEvidence {
            available: true,
            partial: true,
            ..Default::default()
        };
        assert_eq!(partial.summary_lines().len(), 3);
        assert!(partial.summary_lines()[1].contains("전체 미확인"));
    }

    #[test]
    fn uncertainty_precedes_values_and_lists_keep_each_item_visible() {
        let mut evidence = ContextEvidence {
            available: true,
            partial: true,
            ..Default::default()
        };
        assert!(evidence.summary_lines()[1].starts_with("확인한 일부 기록"));
        assert!(evidence.summary_lines()[2].starts_with("전송 미계측"));
        evidence.request_size_error = true;
        assert!(evidence.summary_lines()[2].starts_with("원인 미확인"));
        assert_eq!(
            list("목록", Some(&["first".into(), "last".into()])),
            ["목록: 2개", "  first", "  last"]
        );
        assert!(list("연결", None)[0].starts_with("미확인"));
    }

    #[test]
    fn error_without_usage_keeps_previous_report_and_labels_it_as_recorded() {
        let mut evidence = parse(&[
            serde_json::json!({"type":"assistant","message":{"usage":{"input_tokens":100,"cache_read_input_tokens":900}}}),
            serde_json::json!({"type":"assistant","isApiErrorMessage":true,"message":{"content":[{"type":"text","text":"Request too large (32MB)"}]}}),
        ]);
        evidence.available = true;
        assert_eq!(evidence.last_input_tokens, Some(1000));
        assert!(evidence.request_size_error);
        assert_eq!(evidence.request_bytes, None);
        assert!(evidence.summary_lines()[0].starts_with("마지막 기록"));
        assert!(evidence
            .detail_sections()
            .iter()
            .flat_map(|(_, lines)| lines)
            .any(|line| line.contains("앞선 응답 기록")));
    }
}
