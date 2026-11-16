//! 대화 보기가 읽는 기록 전체. 꼬리 512KB(`socket::read_incremental`)만 읽으면 긴 세션은 앞 대화가
//! 통째로 빠지고, 풀스크린 claude 는 `--resume` 뒤 압축 경계 앞을 화면에도 안 보여 줘서 둘 다
//! 잃는다. 그래서 처음부터 읽되, 대화 칸에 안 쓰이는 무게(그림 base64·`toolUseResult` 사본·긴
//! 도구 결과·메타 키·첨부 줄)를 걷어 가볍게 한다 — 135MB 기록이 5MB 안팎이 된다(2026-10-14 실측).
//! 걷는 것은 `parse.rs` 가 안 읽는 것뿐이라 펴진 대화는 원본과 같다. 원본 기기의
//! `/transcript-raw?history=1` 도 이 함수로 답해 거울도 같은 대화를 받는다. 그려서 보기는 그것을
//! 끝 쪽부터 한 쪽씩 받는다(`page`) — 위로 올리면 그 앞 쪽을 받아 위에 붙인다.

use kasa_socket::backend::{HistoryAsk, HistoryPage, TranscriptChunk};
use serde_json::Value;

/// 이보다 짧은 줄은 걷을 것이 없다 — 파싱 없이 그대로 둔다.
const LIGHT: usize = 2 * 1024;
/// 도구 결과·도구 입력은 접힌 한 줄과 펼친 몇 줄이면 된다.
const TOOL_CAP: usize = 2 * 1024;
const THINKING_CAP: usize = 4 * 1024;
/// 그 밖의 글(답·사람 말)은 한 칸에 이만큼까지.
const TEXT_CAP: usize = 64 * 1024;
/// Edit diff 덩이는 줄 이만큼까지.
const PATCH_LINES: usize = 80;

/// 대화 칸에 안 쓰이는 줄 바깥 키 — 무게는 작아도 줄마다 붙어 수천 줄이면 MB 가 된다.
const META_KEYS: &[&str] = &[
    "parentUuid", "logicalParentUuid", "uuid", "promptId", "requestId", "userType", "entrypoint", "cwd",
    "gitBranch", "version", "slug", "timestamp", "toolUseResult", "sourceToolAssistantUUID",
    "sourceToolUseID", "permissionMode", "thinkingMetadata", "todos",
];
const MESSAGE_KEYS: &[&str] = &["id", "model", "usage", "stop_reason", "stop_sequence", "container", "context_management"];

/// 기록 한 줄을 대화 칸에 쓰이는 만큼만 남긴다. 대화에 안 쓰이는 줄이면 None.
pub(crate) fn slim(line: &str) -> Option<String> {
    slim_for(line, HistoryView::Screen)
}

#[derive(Clone, Copy)]
enum HistoryView { Screen, Archive }

fn slim_for(line: &str, view: HistoryView) -> Option<String> {
    let text_limit = match view { HistoryView::Screen => TEXT_CAP, HistoryView::Archive => usize::MAX };
    let mut v: Value = serde_json::from_str(line).ok()?;
    if matches!(view, HistoryView::Archive) && !archive_event(&mut v) { return None; }
    let obj = v.as_object_mut()?;
    if obj.get("type").and_then(Value::as_str) == Some("token_usage_record") {
        return None;
    }
    if let Some(payload) = obj.get_mut("payload") {
        // 코덱스 rollout — 말은 두고 도구 호출·결과·생각만 줄인다. 명령 출력은 자르기 전 줄 수를 남긴다(「+ N lines」).
        let kind = payload["type"].as_str().unwrap_or("").to_string();
        let item = payload["item"]["type"].as_str().unwrap_or("").to_string();
        match (kind.as_str(), item.as_str()) {
            ("token_count", _) => return None,
            ("message" | "agent_message" | "user_message", _) | ("item_completed", "UserMessage" | "AgentMessage") => {}
            _ => {
                if let Some(out) = payload.get_mut("item").and_then(Value::as_object_mut) {
                    if let Some(n) = out.get("aggregated_output").and_then(Value::as_str).map(|o| o.lines().count()) {
                        out.insert("output_lines".into(), n.into());
                    }
                }
                cap(payload, TOOL_CAP);
            }
        }
        return Some(finish(v, text_limit));
    }
    match obj.get("type").and_then(Value::as_str).unwrap_or("") {
        "user" | "assistant" | "system" | "queue-operation" => {}
        // hook 출력만 대화에 「SessionStart … says」 줄로 선다.
        "attachment" if obj["attachment"]["type"] == "hook_success" => {}
        _ => return None,
    }
    if line.len() < LIGHT && matches!(view, HistoryView::Screen) {
        return Some(line.to_string());
    }
    // Edit 의 줄 번호 붙은 diff 덩이는 classic 이 그대로 보인다 — 사본 전체에서 그것만 남긴다.
    let patch = obj
        .get("toolUseResult")
        .and_then(|r| r.get("structuredPatch"))
        .and_then(Value::as_array)
        .filter(|p| !p.is_empty())
        .cloned();
    for key in META_KEYS {
        if matches!(view, HistoryView::Archive) && *key == "timestamp" { continue; }
        obj.remove(*key);
    }
    if let Some(mut patch) = patch {
        let mut budget = PATCH_LINES;
        patch.retain_mut(|hunk| {
            let Some(lines) = hunk.get_mut("lines").and_then(Value::as_array_mut) else { return false };
            if budget == 0 {
                return false;
            }
            lines.truncate(budget);
            budget -= lines.len();
            true
        });
        obj.insert("toolUseResult".into(), serde_json::json!({ "structuredPatch": patch }));
    }
    if let Some(message) = obj.get_mut("message").and_then(Value::as_object_mut) {
        for key in MESSAGE_KEYS {
            message.remove(*key);
        }
        if let Some(blocks) = message.get_mut("content").and_then(Value::as_array_mut) {
            for block in blocks {
                // classic 의 「Wrote N lines」는 자르기 전 줄 수다.
                if let Some(input) = block.get_mut("input").and_then(Value::as_object_mut) {
                    if let Some(n) = input.get("content").and_then(Value::as_str).map(|c| c.lines().count()) {
                        input.insert("content_lines".into(), n.into());
                    }
                }
                let limit = match block["type"].as_str().unwrap_or("") {
                    "tool_result" | "tool_use" => TOOL_CAP,
                    "thinking" => THINKING_CAP,
                    _ => text_limit,
                };
                cap(block, limit);
            }
        }
    }
    Some(finish(v, text_limit))
}

fn archive_event(value: &mut Value) -> bool {
    if value["isSidechain"] == true || (value["type"] == "user" && value["isMeta"] == true) { return false; }
    if value["payload"].is_object() {
        return match (value["type"].as_str(), value["payload"]["type"].as_str()) {
            (Some("event_msg"), Some("user_message" | "agent_message" | "agent_reasoning" | "turn_aborted")) => true,
            (Some("event_msg"), Some("item_completed")) => matches!(value["payload"]["item"]["type"].as_str(), Some("UserMessage" | "AgentMessage")),
            (Some("response_item"), Some("function_call" | "custom_tool_call" | "function_call_output" | "custom_tool_call_output")) => true,
            _ => false,
        };
    }
    if value["type"] == "system" {
        if !matches!(value["subtype"].as_str(), Some("compact_boundary" | "api_error")) { return false; }
        *value = serde_json::json!({"type":"system","subtype":value["subtype"],"timestamp":value["timestamp"],
            "sessionId":value["sessionId"],"compactMetadata":{"preTokens":value["compactMetadata"]["preTokens"]},
            "error":{"status":value["error"]["status"]},"retryAttempt":value["retryAttempt"],"maxRetries":value["maxRetries"]});
    }
    if value["type"] == "attachment" && value["attachment"]["type"] == "hook_success" {
        let message = value["attachment"]["stdout"].as_str().and_then(|s| serde_json::from_str::<Value>(s).ok())
            .and_then(|v| v["systemMessage"].as_str().map(str::to_owned));
        let Some(message) = message.filter(|s| !s.trim().is_empty()) else { return false };
        value["attachment"] = serde_json::json!({"type":"hook_success","stdout":serde_json::json!({"systemMessage":message}).to_string()});
    }
    if value["type"] == "user" {
        let hidden = |text: &str| !text.contains("[Request interrupted") && super::parse::is_injection(&super::parse::strip_meta(text));
        if value["message"]["content"].as_str().is_some_and(hidden) { return false; }
        if let Some(blocks) = value["message"]["content"].as_array_mut() {
            blocks.retain(|b| b["type"] != "text" || !b["text"].as_str().is_some_and(hidden));
            if blocks.is_empty() { return false; }
        }
    }
    true
}

fn finish(mut v: Value, text_limit: usize) -> String {
    cap(&mut v, text_limit);
    serde_json::to_string(&v).unwrap_or_default()
}

/// 그림은 자리(블록)만 남기고 바이트는 버린다 — 대화 칸은 「그림 N장」만 센다. 생각의 서명도 버린다.
fn cap(v: &mut Value, limit: usize) {
    match v {
        Value::Object(map) => {
            if map.get("type").and_then(Value::as_str) == Some("image") && map.contains_key("source") {
                map.insert("source".into(), Value::Object(Default::default()));
            }
            if map.get("type").and_then(Value::as_str) == Some("input_image") {
                map.remove("image_url");
            }
            if let Some(Value::Array(images)) = map.get_mut("images") {
                for image in images {
                    if image.as_str().is_some_and(|s| s.starts_with("data:image/")) { *image = Value::Null; }
                }
            }
            map.remove("signature");
            for (_, child) in map.iter_mut() {
                cap(child, limit);
            }
        }
        Value::Array(list) => list.iter_mut().for_each(|child| cap(child, limit)),
        Value::String(s) if s.len() > limit => {
            let mut end = limit;
            while !s.is_char_boundary(end) {
                end -= 1;
            }
            s.truncate(end);
        }
        _ => {}
    }
}

/// `read_incremental` 과 같은 약속(완전한 줄만, 반환 `offset` 은 마지막 개행 직후, 줄었거나 처음이면
/// `reset`)이되 처음 읽기는 꼬리가 아니라 파일 처음부터다. 덧붙은 줄도 같은 거르개를 지난다.
pub(crate) fn read(path: &std::path::Path, offset: u64) -> std::io::Result<TranscriptChunk> {
    read_inner(path, offset, None)
}

pub(crate) fn read_archive(path: &std::path::Path, offset: u64) -> std::io::Result<TranscriptChunk> {
    read_inner(path, offset, Some(512 << 10))
}

fn read_inner(path: &std::path::Path, offset: u64, budget: Option<usize>) -> std::io::Result<TranscriptChunk> {
    let f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let reset = offset == 0 || offset > len;
    let start = if reset { 0 } else { offset };
    let source = crate::socket::transcript_source(path);
    if start >= len {
        return Ok(TranscriptChunk { raw: String::new(), offset: len, reset, source });
    }
    let (raw, next) = lines(f, start, u64::MAX, budget)?;
    Ok(TranscriptChunk { raw, offset: next, reset, source })
}

/// `start` 부터 `until` 앞까지 완전한 줄을 걸러 붙인다. 반환은 (걸러 낸 줄, 다음 읽을 자리).
fn lines(mut f: std::fs::File, start: u64, until: u64, budget: Option<usize>) -> std::io::Result<(String, u64)> {
    use std::io::{BufRead, BufReader, Seek, SeekFrom};
    f.seek(SeekFrom::Start(start))?;
    let mut reader = BufReader::with_capacity(256 * 1024, f);
    let mut raw = String::new();
    let mut next = start;
    let mut buf = Vec::new();
    while next < until {
        buf.clear();
        let n = if budget.is_some() {
            use std::io::Read;
            let count = (&mut reader).take((64 << 20) + 1).read_until(b'\n', &mut buf)?;
            if count > (64 << 20) { return Err(std::io::Error::other("transcript row exceeds archive limit")); }
            count
        } else { reader.read_until(b'\n', &mut buf)? };
        // 끝의 쓰다 만 줄은 다음 바퀴로 미룬다.
        if n == 0 || buf.last() != Some(&b'\n') {
            break;
        }
        let line = String::from_utf8_lossy(&buf[..n - 1]);
        let kept = (!line.trim().is_empty()).then(|| if budget.is_some() {
            slim_for(&line, HistoryView::Archive)
        } else { slim(&line) }).flatten();
        if let Some(kept) = kept {
            if budget.is_some() && kept.len() + 1 > kasa_remote::conversation_archive::MAX_APPEND {
                return Err(std::io::Error::other("conversation row exceeds archive limit"));
            }
            if budget.is_some_and(|budget| !raw.is_empty() && raw.len() + kept.len() + 1 > budget) { break; }
            raw.push_str(&kept);
            raw.push('\n');
        }
        next += n as u64;
        if budget.is_some_and(|budget| raw.len() >= budget || next - start >= (8 << 20)) { break; }
    }
    Ok((raw, next))
}

/// 한 쪽에 읽는 기록 파일 바이트. 걸러 내면 보통 1~2MB 라 첫 화면이 빨리 선다 — 처음부터 다 거르면 90MB 기록이
/// 18MB 였고, 원본은 그 90MB 를 칸을 열 때마다 다시 읽었다(2026-11-16 실측).
const PAGE_BYTES: u64 = 8 << 20;

/// 그려서 보기의 쪽 받기 — 처음은 끝 쪽 한 쪽, 위로 올리면 그 앞 한 쪽, 그 사이엔 덧붙은 줄만(`read`).
/// 칸이 다른 파일로 옮겨 갔거나 파일이 줄었으면 새 끝 쪽으로 다시 시작한다(`reset`).
pub(crate) fn page(path: &std::path::Path, ask: &HistoryAsk) -> std::io::Result<HistoryPage> {
    let len = std::fs::metadata(path)?.len();
    let source = crate::socket::transcript_source(path);
    let moved = !ask.held.is_empty() && ask.held != source;
    match ask.before {
        Some(before) if !moved && before > 0 && before <= len => slice(path, before, false, source),
        _ if moved || ask.offset == 0 || ask.offset > len || ask.before.is_some() => slice(path, len, true, source),
        _ => Ok(HistoryPage { chunk: read(path, ask.offset)?, start: None }),
    }
}

/// `end` 앞 한 쪽 — 줄 경계에서 시작한다. 끝 쪽이면 `end` 가 파일 길이이고 쓰다 만 끝 줄은 다음 바퀴로 미룬다.
fn slice(path: &std::path::Path, end: u64, reset: bool, source: String) -> std::io::Result<HistoryPage> {
    let mut f = std::fs::File::open(path)?;
    let start = line_start(&mut f, end.saturating_sub(PAGE_BYTES))?;
    let (raw, next) = lines(f, start, end, None)?;
    Ok(HistoryPage { chunk: TranscriptChunk { raw, offset: next, reset, source }, start: Some(start) })
}

/// `pos` 가 든 줄의 첫 자리 — 그림 한 장이 든 줄은 몇 MB 라 조각씩 거꾸로 찾는다.
fn line_start(f: &mut std::fs::File, pos: u64) -> std::io::Result<u64> {
    use std::io::{Read, Seek, SeekFrom};
    const BLOCK: u64 = 64 << 10;
    let mut buf = vec![0u8; BLOCK as usize];
    let mut hi = pos;
    while hi > 0 {
        let lo = hi.saturating_sub(BLOCK);
        let n = (hi - lo) as usize;
        f.seek(SeekFrom::Start(lo))?;
        f.read_exact(&mut buf[..n])?;
        if let Some(i) = buf[..n].iter().rposition(|&b| b == b'\n') {
            return Ok(lo + i as u64 + 1);
        }
        hi = lo;
    }
    Ok(0)
}

/// 원본이 한 번에 보내는 걸러 낸 기록의 상한. 쪽 받기는 한 쪽(1~2MB)이지만, 쪽을 모르는 옛 원본과 대화 전체를
/// 받는 Info 목록은 처음부터 다 받는다 — 원격 GET 기본(16MB)이면 긴 세션이 넘쳐 그려서 보기가 빈 대화로 남았다(90MB 기록이 18MB).
const REMOTE_HISTORY_MAX: usize = 96 << 20;

/// 원본 기기의 `/transcript-raw` 한 조각 — 처음부터 다 걸러 받는다(`read`, 모르는 옛 원본은 꼬리). Info 의
/// 내 프롬프트·결과물 목록처럼 대화 전체가 필요한 곳이 쓴다. 묶인 기록이 없으면 None.
pub(crate) fn remote(base: &str, surface: &str, offset: u64, source: &str) -> anyhow::Result<Option<TranscriptChunk>> {
    let path = format!(
        "/transcript-raw?surface={}&offset={offset}&source={}&history=1",
        kasa_remote::remote::urlencode(surface),
        kasa_remote::remote::urlencode(source),
    );
    let v = kasa_remote::remote::remote_get_json_bounded(base, &path, REMOTE_HISTORY_MAX)?;
    if v["ok"].as_bool() != Some(true) {
        return Ok(None);
    }
    Ok(Some(chunk_of(&v, offset)))
}

fn chunk_of(v: &serde_json::Value, offset: u64) -> TranscriptChunk {
    TranscriptChunk {
        raw: v["raw"].as_str().unwrap_or("").to_string(),
        offset: v["offset"].as_u64().unwrap_or(offset),
        reset: v["reset"].as_bool().unwrap_or(false),
        source: v["source"].as_str().unwrap_or("").to_string(),
    }
}

/// 원본 기기의 `/transcript-raw` 쪽 받기(`page`). 쪽을 모르는 옛 원본은 `page`·`before` 를 모르고 처음부터 다
/// 보내며 `start` 가 없다 — 받는 쪽은 그것을 처음부터 다 받은 것으로 본다. 묶인 기록이 없으면 None.
pub(crate) fn remote_page(base: &str, surface: &str, ask: &HistoryAsk) -> anyhow::Result<Option<HistoryPage>> {
    let mut path = format!(
        "/transcript-raw?surface={}&offset={}&source={}&history=1&page=1",
        kasa_remote::remote::urlencode(surface),
        ask.offset,
        kasa_remote::remote::urlencode(&ask.held),
    );
    if let Some(before) = ask.before {
        path.push_str(&format!("&before={before}"));
    }
    let v = kasa_remote::remote::remote_get_json_bounded(base, &path, REMOTE_HISTORY_MAX)?;
    if v["ok"].as_bool() != Some(true) {
        return Ok(None);
    }
    Ok(Some(HistoryPage { chunk: chunk_of(&v, ask.offset), start: v["start"].as_u64() }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weight_goes_but_what_the_conversation_reads_stays() {
        let photo = "A".repeat(300 * 1024);
        let long = "줄\n".repeat(4000);
        let line = serde_json::json!({
            "type": "user", "uuid": "u", "cwd": "/w", "sessionId": "s", "isMeta": false,
            "toolUseResult": { "stdout": long },
            "message": { "role": "user", "content": [
                { "type": "image", "source": { "type": "base64", "data": photo } },
                { "type": "tool_result", "tool_use_id": "t1", "content": long, "is_error": true },
                { "type": "text", "text": "그림 봐 줘" },
            ]},
        })
        .to_string();
        let kept: Value = serde_json::from_str(&slim(&line).unwrap()).unwrap();
        assert!(kept.get("toolUseResult").is_none() && kept.get("uuid").is_none());
        assert_eq!(kept["sessionId"], "s");
        let blocks = kept["message"]["content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], "image");
        assert!(blocks[0]["source"].as_object().unwrap().is_empty());
        assert!(blocks[1]["content"].as_str().unwrap().len() <= TOOL_CAP);
        assert_eq!(blocks[1]["is_error"], true);
        assert_eq!(blocks[2]["text"], "그림 봐 줘");
        assert!(slim(&line).unwrap().len() < 4 * 1024);
    }

    #[test]
    fn lines_the_conversation_never_shows_are_dropped() {
        assert!(slim(r#"{"type":"file-history-snapshot","snapshot":{}}"#).is_none());
        assert!(slim(r#"{"type":"attachment","attachment":{"type":"skill_listing"}}"#).is_none());
        assert!(slim(r#"{"type":"attachment","attachment":{"type":"hook_success","stdout":"x"}}"#).is_some());
        assert!(slim(r#"{"type":"payload-less"}"#).is_none());
        assert!(slim("not json").is_none());
        let codex = format!(r#"{{"type":"response_item","payload":{{"type":"function_call_output","output":"{}"}}}}"#, "x".repeat(9000));
        assert!(slim(&codex).unwrap().len() < 3 * 1024);
    }

    #[test]
    fn the_whole_file_is_read_from_the_start_and_a_half_line_waits() {
        let dir = std::env::temp_dir().join(format!("kasaterm-history-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.jsonl");
        let first = r#"{"type":"user","sessionId":"s","message":{"content":"맨 처음 말"}}"#;
        let filler = format!(r#"{{"type":"assistant","sessionId":"s","message":{{"content":[{{"type":"text","text":"{}"}}]}}}}"#, "가".repeat(200));
        let mut body = format!("{first}\n");
        for _ in 0..4000 {
            body.push_str(&filler);
            body.push('\n');
        }
        std::fs::write(&path, format!("{body}{{\"type\":\"us")).unwrap();
        let chunk = read(&path, 0).unwrap();
        assert!(chunk.reset);
        assert!(chunk.raw.starts_with(first), "{}", &chunk.raw[..80]);
        assert_eq!(chunk.offset, body.len() as u64);
        let mut conv = super::super::parse::Conversation::default();
        assert!(conv.apply(&chunk.raw));
        assert_eq!(conv.items.len(), 4001);
        std::fs::write(&path, format!("{body}{first}\n")).unwrap();
        let next = read(&path, chunk.offset).unwrap();
        assert!(!next.reset);
        assert_eq!(next.raw, format!("{first}\n"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn archive_pages_preserve_the_first_message_every_boundary_and_the_original_cursor() {
        let dir = std::env::temp_dir().join(format!("kasaterm-archive-reader-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.jsonl");
        let line = serde_json::json!({"type":"user","sessionId":"s","message":{"content":"한글 문장".repeat(100)}}).to_string();
        let body = format!("{line}\n").repeat(1200);
        std::fs::write(&path, format!("{body}{{\"type\":\"user\"" )).unwrap();
        let expected = read(&path, 0).unwrap().raw;
        let mut all = String::new(); let mut cursor = 0; let mut pages = 0;
        loop {
            let chunk = read_archive(&path, cursor).unwrap();
            assert!(chunk.raw.len() <= kasa_remote::conversation_archive::MAX_APPEND);
            if chunk.offset == cursor { assert!(chunk.raw.is_empty()); break; }
            assert!(chunk.offset > cursor);
            cursor = chunk.offset; all.push_str(&chunk.raw); pages += 1;
        }
        assert!(pages > 1); assert_eq!(cursor, body.len() as u64); assert_eq!(all, expected);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn archived_messages_keep_long_text_and_timestamps_without_embedding_image_payloads() {
        let text = "긴 대화 내용".repeat(20_000);
        let line = serde_json::json!({"type":"assistant","timestamp":"2026-11-04T00:00:00Z","message":{"content":[{"type":"text","text":text}]}}).to_string();
        let archived: Value = serde_json::from_str(&slim_for(&line, HistoryView::Archive).unwrap()).unwrap();
        assert_eq!(archived["message"]["content"][0]["text"], text);
        assert_eq!(archived["timestamp"], "2026-11-04T00:00:00Z");
        let codex = serde_json::json!({"type":"event_msg","payload":{"type":"user_message","message":text,"images":["data:image/png;base64,AAAA"]}}).to_string();
        let archived: Value = serde_json::from_str(&slim_for(&codex, HistoryView::Archive).unwrap()).unwrap();
        assert_eq!(archived["payload"]["message"], text);
        assert!(archived["payload"]["images"][0].is_null());
    }

    #[test]
    fn archive_omits_hidden_context_and_keeps_only_displayed_error_and_hook_fields() {
        for line in [
            serde_json::json!({"type":"session_meta","payload":{"developer_instructions":"private context"}}),
            serde_json::json!({"type":"turn_context","payload":{"cwd":"/private","model":"m"}}),
            serde_json::json!({"type":"response_item","payload":{"type":"message","role":"developer","content":"hidden context"}}),
            serde_json::json!({"type":"user","isMeta":true,"message":{"content":"hidden instruction"}}),
            serde_json::json!({"type":"assistant","isSidechain":true,"message":{"content":"another conversation"}}),
        ] { assert!(slim_for(&line.to_string(), HistoryView::Archive).is_none()); }
        let error = serde_json::json!({"type":"system","subtype":"api_error","error":{"status":429,"request_headers":"private"},"retryAttempt":2,"maxRetries":3});
        let kept = slim_for(&error.to_string(), HistoryView::Archive).unwrap();
        assert!(!kept.contains("private")); assert!(kept.contains("429"));
        let hook = serde_json::json!({"type":"attachment","attachment":{"type":"hook_success","stdout":serde_json::json!({"systemMessage":"visible","internal":"private"}).to_string()}});
        let kept = slim_for(&hook.to_string(), HistoryView::Archive).unwrap();
        assert!(!kept.contains("private")); assert!(kept.contains("visible"));
    }

    #[test]
    fn a_long_mirrored_history_past_the_default_remote_limit_still_arrives() {
        use std::io::{BufRead, BufReader, Write};
        let raw = format!("{}\n", "a".repeat(18 << 20));
        let body = serde_json::json!({"ok": true, "raw": raw, "offset": 90_000_000u64, "reset": true, "source": "s.jsonl"}).to_string();
        assert!(body.len() > 16 << 20);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap() > 2 {
                line.clear();
            }
            let mut stream = stream;
            write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len()).unwrap();
            stream.write_all(body.as_bytes()).unwrap();
        });
        let page = remote_page(&format!("http://127.0.0.1:{port}"), "%2", &HistoryAsk::default()).unwrap().unwrap();
        server.join().unwrap();
        assert_eq!(page.chunk.raw.len(), raw.len());
        assert!(page.chunk.reset);
        assert_eq!(page.chunk.offset, 90_000_000);
        assert_eq!(page.start, None, "쪽을 모르는 옛 원본은 처음부터 다 보낸 것이다");
    }

    fn turn(i: usize) -> String {
        serde_json::json!({"type":"user","sessionId":"s","message":{"content":format!("질문 {i} {}", "x".repeat(200))}}).to_string()
    }

    #[test]
    fn the_first_page_is_the_end_and_earlier_pages_meet_it_without_gaps() {
        let dir = std::env::temp_dir().join(format!("kasaterm-history-page-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        let n = (PAGE_BYTES as usize * 5 / 2) / 260;
        let mut body: String = (0..n).map(|i| turn(i) + "\n").collect();
        body.push_str("{\"type\":\"user\",\"sessionId\":\"s\",\"mess");
        std::fs::write(&path, &body).unwrap();
        let full = read(&path, 0).unwrap();

        let tail = page(&path, &HistoryAsk::default()).unwrap();
        assert!(tail.chunk.reset);
        let end = tail.chunk.offset;
        assert_eq!(end, full.offset, "쓰다 만 끝 줄은 다음 바퀴로 미룬다");
        assert!(tail.chunk.raw.ends_with(&format!("{}\n", slim(&turn(n - 1)).unwrap())));
        let mut start = tail.start.unwrap();
        assert!(start > 0 && end - start >= PAGE_BYTES);
        let mut text = tail.chunk.raw;
        while start > 0 {
            let source = crate::socket::transcript_source(&path);
            let older = page(&path, &HistoryAsk { offset: end, before: Some(start), held: source }).unwrap();
            assert!(!older.chunk.reset);
            assert_eq!(older.chunk.offset, start, "앞 쪽은 받은 쪽 첫 줄에서 끝난다");
            text = older.chunk.raw + &text;
            start = older.start.unwrap();
        }
        assert_eq!(text, full.raw, "쪽을 다 이으면 처음부터 다 받은 것과 같다");

        let after = page(&path, &HistoryAsk { offset: end, before: None, held: crate::socket::transcript_source(&path) }).unwrap();
        assert_eq!(after.start, None);
        assert!(after.chunk.raw.is_empty() && !after.chunk.reset);
        let moved = page(&path, &HistoryAsk { offset: end, before: Some(start.max(1)), held: "other.jsonl".into() }).unwrap();
        assert!(moved.chunk.reset && moved.start.is_some(), "다른 파일로 옮겨 갔으면 새 끝 쪽부터");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
