//! ccglass-style capture proxy. claude 의 Anthropic API 호출(`POST /v1/messages`)을
//! `ANTHROPIC_BASE_URL` 로 가로채, 요청 본문의 `messages[]` 와 SSE 응답을 pane 별로
//! 캡처한다 — peek(화면 스크래핑)·jsonl(지연) 없이 구조화된 라이브 대화 소스.
//!
//! 전송은 **투명 passthrough**: 헤더·바디 그대로 api.anthropic.com 으로 보내고 응답
//! 스트림을 그대로 흘린다(터미널의 라이브 스트리밍 유지). 캡처는 side-effect 라
//! 파싱이 실패해도 claude 는 정상 동작한다. auth(authorization/x-api-key)는 손대지
//! 않고 그대로 통과 — claude 가 보내는 그대로 Anthropic 이 받는다.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::StreamExt;

const ANTHROPIC: &str = "https://api.anthropic.com";

#[derive(Clone, serde::Serialize)]
pub struct Turn {
    pub role: String,
    pub text: String,
}

#[derive(Default)]
pub struct PaneConv {
    /// 직전 요청 messages[] 에서 뽑은 확정 대화(user/assistant).
    pub turns: Vec<Turn>,
    /// 진행 중 어시스턴트 응답(SSE text_delta 누적). 완료(message_stop)되면 turns 합류.
    pub streaming: String,
    /// SSE 청크가 줄 중간에서 잘려도 안전하게 — 미완성 라인 버퍼.
    sse_buf: String,
    pub model: String,
    pub updated: f64,
}

pub type ConvStore = Arc<Mutex<HashMap<String, PaneConv>>>;

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// claude-code 가 메시지에 끼우는 메타 래퍼(`<system-reminder>…</system-reminder>`·
/// `<command-message>…` 등)를 벗겨 사람이 친 프롬프트만 남긴다 — 세션 첫 user 메시지에
/// skills·환경 프리앰블이 통째로 붙어 채팅에 노이즈로 떴다(거노). regex 없이 수동 스캔.
fn strip_meta(text: &str) -> String {
    let mut s = text.to_string();
    for (open, close) in [
        ("<system-reminder>", "</system-reminder>"),
        ("<command-message>", "</command-message>"),
        ("<command-name>", "</command-name>"),
        ("<local-command-stdout>", "</local-command-stdout>"),
    ] {
        while let Some(start) = s.find(open) {
            match s[start + open.len()..].find(close) {
                Some(rel) => {
                    let end = start + open.len() + rel + close.len();
                    s.replace_range(start..end, "");
                }
                None => {
                    s.truncate(start); // 닫힘 없으면(잘린 래퍼) 이후 전부 버림
                    break;
                }
            }
        }
    }
    s.trim().to_string()
}

/// content(string | block 배열)에서 텍스트만. tool_use/tool_result/image 는 대화
/// 버블에 안 쓰므로 건너뛴다. 메타 래퍼 제거 후 빈 문자열이면 호출부에서 그 턴 제외.
fn block_text(content: Option<&serde_json::Value>) -> String {
    let raw = match content {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(arr)) => arr
            .iter()
            .filter_map(|b| match b.get("type").and_then(|t| t.as_str()) {
                Some("text") => b.get("text").and_then(|t| t.as_str()).map(str::to_string),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    strip_meta(&raw)
}

/// 백그라운드 유틸 호출(프롬프트 제안·세션 제목·/compact 요약 등)인가 — 메인 대화가
/// 아니므로 캡처에서 제외한다. claude-code 가 이런 호출에 쓰는 system/지시문 마커로
/// 판별. 안 그러면 "프롬프트 제안" 호출의 SUGGESTION MODE 지시문이 채팅에 섞였다(거노).
fn is_utility_request(body: &serde_json::Value) -> bool {
    let mut hay = String::new();
    match body.get("system") {
        Some(serde_json::Value::String(s)) => hay.push_str(s),
        Some(serde_json::Value::Array(arr)) => {
            for b in arr {
                if let Some(t) = b.get("text").and_then(|x| x.as_str()) {
                    hay.push_str(t);
                    hay.push('\n');
                }
            }
        }
        _ => {}
    }
    // 제안 모드는 지시문을 메시지로 넣기도 한다 — 앞 메시지 몇 개도 본다.
    if let Some(msgs) = body.get("messages").and_then(|m| m.as_array()) {
        for m in msgs.iter().take(2) {
            match m.get("content") {
                Some(serde_json::Value::String(s)) => hay.push_str(s),
                Some(serde_json::Value::Array(arr)) => {
                    for b in arr {
                        if let Some(t) = b.get("text").and_then(|x| x.as_str()) {
                            hay.push_str(t);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    const MARKERS: &[&str] = &[
        "SUGGESTION MODE",
        "Suggest what the user might naturally type",
        "summarize the conversation",
        "detailed summary of the conversation",
        "write a 5-10 word title",
        "concise, 5-10 word title",
        "Quiet Mode",
    ];
    MARKERS.iter().any(|m| hay.contains(m))
}

/// 요청 본문 messages[] → 대화 턴(user/assistant, 텍스트 있는 것만).
fn turns_from_request(body: &serde_json::Value) -> Vec<Turn> {
    body.get("messages")
        .and_then(|m| m.as_array())
        .map(|msgs| {
            msgs.iter()
                .filter_map(|m| {
                    let role = m.get("role").and_then(|r| r.as_str())?;
                    let text = block_text(m.get("content"));
                    let text = text.trim();
                    (!text.is_empty()).then(|| Turn {
                        role: role.to_string(),
                        text: text.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// SSE 청크 누적 — Anthropic stream(`event: …\ndata: {…}\n\n`)에서 `text_delta` 를
/// streaming 에 모으고, `message_stop` 이면 turns 에 합류. 줄 단위로만 처리하고
/// 미완성 라인은 sse_buf 에 남겨 다음 청크와 이어붙인다.
fn accumulate_sse(chunk: &str, conv: &mut PaneConv) {
    conv.sse_buf.push_str(chunk);
    while let Some(nl) = conv.sse_buf.find('\n') {
        let line: String = conv.sse_buf.drain(..=nl).collect();
        let line = line.trim_end();
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else {
            continue;
        };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("content_block_delta") => {
                if let Some(t) = v.pointer("/delta/text").and_then(|x| x.as_str()) {
                    conv.streaming.push_str(t);
                    conv.updated = now();
                }
            }
            Some("message_stop") => {
                let txt = conv.streaming.trim().to_string();
                if !txt.is_empty() {
                    conv.turns.push(Turn {
                        role: "assistant".into(),
                        text: txt,
                    });
                }
                conv.streaming.clear();
                conv.updated = now();
            }
            _ => {}
        }
    }
}

/// `/p/{pane}/{*rest}` — claude 의 Anthropic API 호출 가로채기. 캡처(side-effect) 후
/// api.anthropic.com 으로 투명 포워드, 응답 스트림 tee.
pub async fn proxy_handler(
    store: ConvStore,
    client: reqwest::Client,
    pane: String,
    rest: String,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // 캡처: 실제 생성 호출(POST /v1/messages)이면서 **메인 대화**일 때만. count_tokens·
    // 프롬프트 제안·제목·요약 등 유틸 호출은 포워드만(응답 SSE 도 캡처 안 함 → capture).
    let mut capture = false;
    if method == Method::POST && rest == "v1/messages" {
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&body) {
            if !is_utility_request(&v) {
                capture = true;
                let turns = turns_from_request(&v);
                let model = v.get("model").and_then(|m| m.as_str()).map(str::to_string);
                if let Ok(mut s) = store.lock() {
                    let e = s.entry(pane.clone()).or_default();
                    e.turns = turns;
                    e.streaming.clear();
                    e.sse_buf.clear();
                    if let Some(m) = model.filter(|m| !m.is_empty()) {
                        e.model = m;
                    }
                    e.updated = now();
                }
            }
        }
    }

    // 포워드. host/content-length 는 reqwest 가 재설정하므로 제외.
    let url = format!("{ANTHROPIC}/{rest}");
    let mut rb = client.request(method, &url);
    for (k, val) in headers.iter() {
        if k == axum::http::header::HOST || k == axum::http::header::CONTENT_LENGTH {
            continue;
        }
        rb = rb.header(k.as_str(), val.as_bytes());
    }
    let upstream = match rb.body(body.to_vec()).send().await {
        Ok(r) => r,
        Err(e) => {
            return (StatusCode::BAD_GATEWAY, format!("proxy upstream error: {e}")).into_response()
        }
    };

    let status = upstream.status();
    // 응답 SSE 누적은 메인 대화(capture) 호출일 때만 — 유틸 호출 응답은 대화에 안 섞는다.
    let is_sse = capture
        && upstream
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|c| c.to_str().ok())
            .is_some_and(|c| c.contains("event-stream"));
    let mut out = Response::builder().status(status.as_u16());
    for (k, val) in upstream.headers().iter() {
        let kl = k.as_str().to_ascii_lowercase();
        // hop-by-hop / 길이 헤더는 스트림 재구성하므로 빼고, 나머지는 그대로.
        if matches!(kl.as_str(), "transfer-encoding" | "content-length" | "connection") {
            continue;
        }
        out = out.header(k.as_str(), val.as_bytes());
    }

    let store2 = store.clone();
    let stream = upstream.bytes_stream().map(move |chunk| {
        if is_sse {
            if let Ok(b) = &chunk {
                if let Ok(text) = std::str::from_utf8(b) {
                    if let Ok(mut s) = store2.lock() {
                        accumulate_sse(text, s.entry(pane.clone()).or_default());
                    }
                }
            }
        }
        chunk.map_err(std::io::Error::other)
    });

    out.body(Body::from_stream(stream))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_text_turns_skips_tool_noise() {
        let body = serde_json::json!({
            "model": "claude-sonnet-4-6",
            "messages": [
                {"role": "user", "content": "하이"},
                {"role": "assistant", "content": [{"type": "text", "text": "안녕하세요!"}]},
                {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "x", "content": "ok"}]},
            ]
        });
        let turns = turns_from_request(&body);
        assert_eq!(turns.len(), 2); // tool_result(텍스트 블록 없음) 제외
        assert_eq!(turns[0].role, "user");
        assert_eq!(turns[0].text, "하이");
        assert_eq!(turns[1].role, "assistant");
        assert_eq!(turns[1].text, "안녕하세요!");
    }

    #[test]
    fn sse_accumulates_then_commits_on_stop() {
        let mut c = PaneConv::default();
        accumulate_sse(
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"안\"}}\n",
            &mut c,
        );
        accumulate_sse(
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"녕\"}}\n",
            &mut c,
        );
        assert_eq!(c.streaming, "안녕");
        accumulate_sse("data: {\"type\":\"message_stop\"}\n", &mut c);
        assert_eq!(c.streaming, "");
        assert_eq!(c.turns.last().unwrap().text, "안녕");
    }

    #[test]
    fn skips_utility_calls() {
        let sugg = serde_json::json!({
            "system": "[SUGGESTION MODE: Suggest what the user might naturally type next.]",
            "messages": [{"role": "user", "content": "ctx"}]
        });
        assert!(is_utility_request(&sugg));
        // 지시문이 메시지로 들어간 경우도
        let sugg2 = serde_json::json!({
            "messages": [{"role": "user", "content": "[SUGGESTION MODE: ...]"}]
        });
        assert!(is_utility_request(&sugg2));
        // 메인 대화는 통과
        let main = serde_json::json!({
            "system": "You are Claude Code, Anthropic's CLI",
            "messages": [{"role": "user", "content": "하이"}]
        });
        assert!(!is_utility_request(&main));
    }

    #[test]
    fn strips_meta_preamble() {
        assert_eq!(
            strip_meta("<system-reminder>\nskills…\n</system-reminder>\nsay exactly A"),
            "say exactly A"
        );
        // 닫힘 없는(잘린) 래퍼 → 이후 전부 버림
        assert_eq!(strip_meta("hi <system-reminder>truncated"), "hi");
        // 래퍼 없으면 그대로
        assert_eq!(strip_meta("just a prompt"), "just a prompt");
    }

    #[test]
    fn sse_handles_chunk_split_midline() {
        let mut c = PaneConv::default();
        accumulate_sse("data: {\"type\":\"content_block_de", &mut c); // 줄 중간에서 잘림
        accumulate_sse(
            "lta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"x\"}}\n",
            &mut c,
        );
        assert_eq!(c.streaming, "x");
    }
}

/// 저장된 pane 대화를 채팅용 JSON 으로. turns + 진행 중 streaming(라이브 응답).
pub fn conversation_json(store: &ConvStore, pane: &str) -> serde_json::Value {
    let g = store.lock().ok();
    let conv = g.as_ref().and_then(|s| s.get(pane));
    match conv {
        Some(c) => serde_json::json!({
            "turns": c.turns,
            "streaming": c.streaming,
            "model": c.model,
            "updated": c.updated,
        }),
        None => serde_json::json!({ "turns": [], "streaming": "", "model": "", "updated": 0.0 }),
    }
}
