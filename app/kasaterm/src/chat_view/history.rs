//! 대화 보기가 읽는 기록 전체. 꼬리 512KB(`socket::read_incremental`)만 읽으면 긴 세션은 앞 대화가
//! 통째로 빠지고, 풀스크린 claude 는 `--resume` 뒤 압축 경계 앞을 화면에도 안 보여 줘서 둘 다
//! 잃는다. 그래서 처음부터 읽되, 대화 칸에 안 쓰이는 무게(그림 base64·`toolUseResult` 사본·긴
//! 도구 결과·메타 키·첨부 줄)를 걷어 가볍게 한다 — 135MB 기록이 5MB 안팎이 된다(2026-10-14 실측).
//! 걷는 것은 `parse.rs` 가 안 읽는 것뿐이라 펴진 대화는 원본과 같다. 원본 기기의
//! `/transcript-raw?history=1` 도 이 함수로 답해 거울도 같은 대화를 받는다.

use kasa_socket::backend::TranscriptChunk;
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
    let mut v: Value = serde_json::from_str(line).ok()?;
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
        return Some(finish(v));
    }
    match obj.get("type").and_then(Value::as_str).unwrap_or("") {
        "user" | "assistant" | "system" | "queue-operation" => {}
        // hook 출력만 대화에 「SessionStart … says」 줄로 선다.
        "attachment" if obj["attachment"]["type"] == "hook_success" => {}
        _ => return None,
    }
    if line.len() < LIGHT {
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
                    _ => TEXT_CAP,
                };
                cap(block, limit);
            }
        }
    }
    Some(finish(v))
}

fn finish(mut v: Value) -> String {
    cap(&mut v, TEXT_CAP);
    serde_json::to_string(&v).unwrap_or_default()
}

/// 그림은 자리(블록)만 남기고 바이트는 버린다 — 대화 칸은 「그림 N장」만 센다. 생각의 서명도 버린다.
fn cap(v: &mut Value, limit: usize) {
    match v {
        Value::Object(map) => {
            if map.get("type").and_then(Value::as_str) == Some("image") && map.contains_key("source") {
                map.insert("source".into(), Value::Object(Default::default()));
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
    use std::io::{BufRead, BufReader, Seek, SeekFrom};
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let reset = offset == 0 || offset > len;
    let start = if reset { 0 } else { offset };
    let source = crate::socket::transcript_source(path);
    if start >= len {
        return Ok(TranscriptChunk { raw: String::new(), offset: len, reset, source });
    }
    f.seek(SeekFrom::Start(start))?;
    let mut reader = BufReader::with_capacity(256 * 1024, f);
    let mut raw = String::new();
    let mut next = start;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf)?;
        // 끝의 쓰다 만 줄은 다음 바퀴로 미룬다.
        if n == 0 || buf.last() != Some(&b'\n') {
            break;
        }
        next += n as u64;
        let line = String::from_utf8_lossy(&buf[..n - 1]);
        if line.trim().is_empty() {
            continue;
        }
        if let Some(kept) = slim(&line) {
            raw.push_str(&kept);
            raw.push('\n');
        }
    }
    Ok(TranscriptChunk { raw, offset: next, reset, source })
}

/// 원본 기기의 `/transcript-raw` 한 조각 — 처음부터 걸러 받는다(`read`, 모르는 옛 원본은 꼬리).
/// `source` 는 `offset` 이 가리키는 파일 — 원본 칸이 다른 파일로 옮겨 갔으면 새 파일이 `reset` 으로
/// 온다. 묶인 기록이 없으면 None.
pub(crate) fn remote(base: &str, surface: &str, offset: u64, source: &str) -> anyhow::Result<Option<TranscriptChunk>> {
    let path = format!(
        "/transcript-raw?surface={}&offset={offset}&source={}&history=1",
        kasa_mcp::remote::urlencode(surface),
        kasa_mcp::remote::urlencode(source),
    );
    let v = kasa_mcp::remote::remote_get_json(base, &path)?;
    if v["ok"].as_bool() != Some(true) {
        return Ok(None);
    }
    Ok(Some(TranscriptChunk {
        raw: v["raw"].as_str().unwrap_or("").to_string(),
        offset: v["offset"].as_u64().unwrap_or(offset),
        reset: v["reset"].as_bool().unwrap_or(false),
        source: v["source"].as_str().unwrap_or("").to_string(),
    }))
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
}
