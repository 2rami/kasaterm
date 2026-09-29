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

/// 실행 상세 한 줄 — 「이름 · 값」 표 또는 이름 알약.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Detail {
    /// 값이 `None` 이면 흐린 「—」 로 그리고 `tip`(올리면 뜨는 말)이 까닭을 댄다.
    Value { name: String, value: Option<String>, tip: String, warn: bool },
    /// 이름 목록 — 알약으로 늘어놓는다. `None` 은 미확인(—), 빈 목록은 「없음」.
    Pills { name: String, items: Option<Vec<String>>, tip: String },
}

fn value(name: &str, value: Option<String>, tip: &str) -> Detail {
    Detail::Value { name: name.into(), value, tip: tip.into(), warn: false }
}

fn pills(name: &str, items: Option<&[String]>, tip: &str) -> Detail {
    Detail::Pills { name: name.into(), items: items.map(<[String]>::to_vec), tip: tip.into() }
}

/// 세 자리마다 쉼표 — 토큰·바이트는 자릿수가 커서 붙여 쓰면 한눈에 안 읽힌다.
pub(crate) fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 { out.push(','); }
        out.push(ch);
    }
    out
}

impl ContextEvidence {
    /// 상세 머리의 안내 하나. 섹션마다 되풀이하던 해설(「관측값이지 확정이 아니다」「목록에 있다고 읽은 것은
    /// 아니다」「연결은 그 당시 상태」)을 여기로 모았다(2026-09-28 지시).
    pub(crate) fn detail_note(&self) -> String {
        if !self.available {
            return "이 세션의 로컬 기록이 없거나 읽을 수 없어요. 원격 기록은 따로 가져오지 않아요.".into();
        }
        let mut note = String::from("기록에서 관측한 값이에요 — 모델에 남은 내용·느려진 원인·실제 전송 크기는 확정하지 않아요. 목록에 있다고 읽은 것은 아니고, 연결 기록은 그 당시 상태예요.");
        if self.partial {
            note.push_str(" 일부 구간만 셌어요 — 0건이어도 전체 대화에 없다는 뜻은 아니에요.");
        }
        if self.incomplete_record {
            note.push_str(" 아직 쓰는 중인 마지막 기록은 빼고 셌어요.");
        }
        if self.limited {
            note.push_str(" 기록이나 목록이 수집 한도를 넘어 일부만 보여요.");
        }
        note
    }

    /// 머리 안내 자리에 서는 한 줄 — 몇 건을 봤고 전체인지. 안내 전문은 이 줄에 올리면 뜬다.
    /// 안내 다섯 줄과 「범위」 두 줄이 상세마다 표 위를 먹어 정작 값이 화면 밖으로 밀렸다(2026-09-29).
    pub(crate) fn detail_scope(&self) -> String {
        if !self.available {
            return String::new();
        }
        let records = grouped(self.observed_records);
        if self.partial || self.limited {
            format!("기록 {records}건 · 일부만 확인")
        } else {
            format!("기록 {records}건 · 저장된 전체")
        }
    }

    pub(crate) fn detail_sections(&self) -> Vec<(String, Vec<Detail>)> {
        if !self.available {
            return Vec::new();
        }
        let mut tokens = vec![value(
            "마지막 입력",
            self.last_input_tokens.map(|n| format!("{} 토큰 · 캐시 포함", grouped(n))),
            "마지막으로 기록된 사용량이에요. 오류가 난 요청은 사용량을 안 남겨 앞선 응답 기록이 보일 수 있어요. 토큰은 전송 바이트와 다른 단위예요.",
        )];
        if self.last_input_tokens.is_some() && self.cumulative_input_tokens.is_none() {
            tokens.push(value("확인한 입출력", Some(format!("입력 {} · 출력 {}", grouped(self.observed_input_tokens), grouped(self.observed_output_tokens))), "확인한 응답 기록을 더한 값"));
        }
        if let (Some(i), Some(o)) = (self.cumulative_input_tokens, self.cumulative_output_tokens) {
            tokens.push(value("세션 누적", Some(format!("입력 {} · 출력 {}", grouped(i), grouped(o))), "하네스가 보고한 값"));
        }
        let skills = vec![
            pills("사용 가능", self.skills_available.as_deref(), "세션에 제시된 스킬"),
            pills("읽음", Some(&self.skills_read), "읽기 응답이 확인된 스킬 — 셸로 읽은 것은 식별하지 못할 수 있어요"),
            pills("호출", Some(&self.skills_invoked), "스킬 호출 기록"),
        ];
        let mcp = vec![
            pills("설정", self.mcp_configured.as_deref(), "세션에 보고된 MCP 설정"),
            pills("호출", Some(&self.mcp_called), "MCP 호출 기록"),
            pills("연결 기록", self.mcp_connected.as_deref(), "기록 당시 상태 — 지금 연결은 미확인이에요"),
        ];
        let mut images = vec![
            value(
                "이미지",
                Some(format!("읽기 {}회 · 첨부 {}건 · 참조만 {}개", self.image_read_calls, self.attachment_records, self.image_reference_blocks)),
                "읽기는 이미지 읽기 도구 호출, 첨부는 이미지 첨부가 표시된 기록, 참조만은 본문 없이 참조만 남은 블록이에요 — 그 본문 크기는 미확인",
            ),
            value(
                "저장된 이미지",
                Some(format!("{}블록 · {}바이트 · 최대 {}", self.image_payload_blocks, grouped(self.stored_image_payload_bytes), grouped(self.max_image_payload_bytes))),
                "기록 안에 인코딩된 문자열 크기예요. 실제 전송 크기는 계측하지 않아요 — 파일 크기를 요청 크기로 보지 않아요",
            ),
        ];
        if self.request_size_error {
            images.push(Detail::Value {
                name: "크기 오류".into(),
                value: Some("기록 있음 · 원인 미확인".into()),
                tip: "요청 크기 오류가 기록돼 있어요. 이미지 때문이라고 단정하지 않아요".into(),
                warn: true,
            });
        }
        vec![
            ("토큰".into(), tokens),
            ("스킬".into(), skills),
            ("MCP".into(), mcp),
            ("이미지·요청".into(), images),
        ]
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
        assert!(evidence.detail_note().contains("전체 대화"), "일부만 셌다는 말은 머리 안내가 한 번 한다");
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
        assert!(partial.detail_scope().contains("일부만 확인"));
        assert!(ContextEvidence { available: true, ..Default::default() }.detail_scope().contains("저장된 전체"));
    }

    /// 모르는 값은 값 칸을 비워(「—」) 까닭을 말에 싣고, 목록은 이름 하나하나를 알약으로 남긴다.
    /// 되풀이 해설은 섹션이 아니라 머리 안내에 한 번만 선다.
    #[test]
    fn unknown_values_stay_empty_lists_keep_each_name_and_caveats_live_once() {
        let mut evidence = ContextEvidence { available: true, skills_read: vec!["first".into(), "last".into()], ..Default::default() };
        let sections = evidence.detail_sections();
        let rows: Vec<&Detail> = sections.iter().flat_map(|(_, rows)| rows).collect();
        assert!(rows.iter().any(|row| matches!(row, Detail::Value { name, tip, .. } if name == "저장된 이미지" && tip.contains("실제 전송 크기는 계측하지 않아요"))));
        assert!(rows.iter().any(|row| matches!(row, Detail::Pills { name, items: Some(items), .. } if name == "읽음" && items.len() == 2)));
        assert!(rows.iter().any(|row| matches!(row, Detail::Pills { name, items: None, .. } if name == "사용 가능")), "목록 자체를 모르면 미확인");
        let caveat = "목록에 있다고 읽은 것은 아니";
        assert!(evidence.detail_note().contains(caveat));
        assert!(!rows.iter().any(|row| matches!(row, Detail::Value { value: Some(v), .. } if v.contains(caveat))));
        evidence.request_size_error = true;
        assert!(evidence.detail_sections().iter().flat_map(|(_, rows)| rows)
            .any(|row| matches!(row, Detail::Value { warn: true, .. })));
        assert_eq!(grouped(12000), "12,000");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1234567), "1,234,567");
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
        let tokens = evidence.detail_sections().into_iter().find(|(title, _)| title == "토큰").unwrap().1;
        assert!(matches!(&tokens[0], Detail::Value { value: Some(v), tip, .. } if v.starts_with("1,000 토큰") && tip.contains("앞선 응답 기록")));
    }
}
