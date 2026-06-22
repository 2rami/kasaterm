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
    /// 이 턴에 첨부된 이미지 파일 경로(거노가 붙여넣은 image block → 디코드 저장). 대화창 인라인.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
}

/// base64 디코드(std 만으로 — base64 crate 의존 회피). 표준 알파벳, 패딩·개행 무시.
fn b64_decode(s: &str) -> Vec<u8> {
    let mut lut = [255u8; 256];
    for (i, &c) in b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/".iter().enumerate() {
        lut[c as usize] = i as u8;
    }
    let (mut out, mut buf, mut bits) = (Vec::new(), 0u32, 0u32);
    for &c in s.as_bytes() {
        let v = lut[c as usize];
        if v == 255 { continue; }
        buf = (buf << 6) | v as u32;
        bits += 6;
        if bits >= 8 { bits -= 8; out.push((buf >> bits) as u8); }
    }
    out
}

/// content[] 의 image block(base64)을 /tmp/kasaterm-proxy-images/<hash>.<ext> 로 디코드 저장하고
/// 경로를 반환(거노: 이미지도 대화창에). FNV 해시로 같은 이미지는 1회만 저장(dedup).
fn extract_images(content: Option<&serde_json::Value>) -> Vec<String> {
    let Some(arr) = content.and_then(|c| c.as_array()) else { return Vec::new(); };
    let mut out = Vec::new();
    for b in arr {
        if b.get("type").and_then(|t| t.as_str()) != Some("image") { continue; }
        let Some(data) = b.pointer("/source/data").and_then(|d| d.as_str()) else { continue; };
        let media = b.pointer("/source/media_type").and_then(|m| m.as_str()).unwrap_or("image/png");
        let ext = media.rsplit('/').next().unwrap_or("png");
        let mut h: u64 = 0xcbf29ce484222325;
        for byte in data.bytes() { h ^= byte as u64; h = h.wrapping_mul(0x100000001b3); }
        let dir = std::path::Path::new("/tmp/kasaterm-proxy-images");
        if std::fs::create_dir_all(dir).is_err() { continue; }
        let path = dir.join(format!("{h:016x}.{ext}"));
        if !path.exists() {
            let _ = std::fs::write(&path, b64_decode(data));
        }
        out.push(path.to_string_lossy().to_string());
    }
    out
}

/// 인터랙티브 도구 호출(AskUserQuestion 등) — SSE tool_use 블록에서 재구성. peek 화면
/// 추정 없이 질문/선택지를 API 그대로 얻는다(거노: peek 추정 금지). input 은 도구의 원본
/// JSON(예: AskUserQuestion 의 questions/options).
#[derive(Clone, serde::Serialize)]
pub struct ToolUse {
    pub name: String,
    pub input: serde_json::Value,
}

#[derive(Default)]
pub struct PaneConv {
    /// 직전 요청 messages[] 에서 뽑은 확정 대화(user/assistant).
    pub turns: Vec<Turn>,
    /// 진행 중 어시스턴트 응답(SSE text_delta 누적). 완료(message_stop)되면 turns 합류.
    pub streaming: String,
    /// SSE 청크가 줄 중간에서 잘려도 안전하게 — 미완성 라인 버퍼.
    sse_buf: String,
    /// SSE 원시 바이트 버퍼 — 청크가 UTF-8 멀티바이트(한글) 경계에서 잘려도 유효
    /// prefix 만 디코드하고 불완전 꼬리는 다음 청크와 잇는다. from_utf8 로 청크를 바로
    /// 디코드하면 잘린 청크를 통째 버려 한글 응답 streaming 이 0 이었다(거노 실측).
    sse_bytes: Vec<u8>,
    /// 진행 중 tool_use 누적: content block index → (도구명, partial_json). stop 시 완성.
    pending_tools: HashMap<i64, (String, String)>,
    /// 완성된 인터랙티브 도구 호출(AskUserQuestion 등). 새 생성 요청 캡처 시 클리어.
    pub tool_uses: Vec<ToolUse>,
    pub model: String,
    /// reasoning effort(low/medium/high/xhigh/max) — 신형은 output_config.effort 문자열,
    /// 구형만 thinking.budget_tokens 역산. 빈 문자열 = 미상(effort 미지정·기본 high). arona effort 칩.
    pub effort: String,
    /// 진행 중 응답의 누적 출력 토큰 — arona 라이브 spinner "↓N". message_delta.usage
    /// (정확)와 생성 글자수 추정 중 max(단조증가). message_delta 는 응답 끝에만 와서
    /// 진행 중엔 글자수 추정으로 라이브를 메운다. 새 생성 요청 캡처마다 0 리셋.
    pub tokens_out: u64,
    /// 추정용 — 생성된 text/thinking 글자 누적. tokens_out ≈ gen_chars/3(혼합 근사).
    gen_chars: u64,
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
        ("<command-args>", "</command-args>"),
        ("<local-command-stdout>", "</local-command-stdout>"),
        // Monitor/cron 등 background 작업 알림 — 시스템이 user 턴에 주입하는 메타라
        // 사람이 친 게 아님(거노: task-notification 이 선생님 말풍선으로 샜다).
        ("<task-notification>", "</task-notification>"),
        // /effort·/loop 등 슬래시 명령 확장 시 user 메시지 앞에 붙는 caveat 래퍼 —
        // "Caveat: The messages below were generated by the user while running local
        // commands…" 가 노란 선생님 말풍선에 raw 로 샜다(거노 실측, 자주 발생).
        ("<local-command-caveat>", "</local-command-caveat>"),
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
    // claude 가 이미지 붙일 때 메시지 텍스트에 끼우는 플레이스홀더 줄 제거 — 실제 이미지는
    // image 블록(extract_images)으로 따로 렌더되므로 텍스트엔 노이즈(거노: 말풍선에 경로 떴음).
    // "[Image #1]" / "[Image: source: /.../image-cache/.../1.png]" / 좌표 매핑 안내
    // "[Image: original 3024x1898, displayed at … Multiply coordinates by … ]" 같은 줄.
    let s = s
        .lines()
        .filter(|l| {
            let t = l.trim();
            !(t.starts_with("[Image: source:")
                || t.starts_with("[Image: original ")
                || (t.starts_with("[Image #") && t.ends_with(']')))
        })
        .collect::<Vec<_>>()
        .join("\n");
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
    // 제안/요약 지시문은 보통 첫 메시지(요약·제목) 또는 **마지막 메시지**(프롬프트 제안)에
    // 들어간다 — 양끝 몇 개만 본다(거대 대화 중간은 스캔 안 함 → 중간에 사용자가 마커를
    // 인용해도 오탐 없음). 첫 버전이 앞 2개만 봐서 마지막에 붙는 SUGGESTION MODE 를 놓쳐
    // 전체 시스템프롬프트째로 채팅에 새어나왔다(거노).
    if let Some(msgs) = body.get("messages").and_then(|m| m.as_array()) {
        let n = msgs.len();
        for (i, m) in msgs.iter().enumerate() {
            if i >= 2 && i + 2 < n {
                continue; // 양끝(앞2·뒤2)만
            }
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
            hay.push('\n');
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

/// 캡처할 **메인 대화** 호출인가. claude-code 메인 에이전트 루프는 항상 도구를
/// `tools[]`(Bash/Read/…)에 실어 보낸다. 반면 `/compact` 요약·프롬프트 제안·제목
/// 생성 같은 백그라운드 유틸 호출은 도구 없는 순수 생성이라 `tools` 가 비거나 없다.
/// 마커(`is_utility_request`)는 claude-code 버전 따라 문구가 바뀌어 취약하므로 —
/// 실제로 `/compact` 호출 하나가 마커를 빠져나가 전체 대화를 "quota" 한 단어로
/// 덮어썼다(거노) — `tools[]` 유무라는 구조적 시그널로 한 번 더 거른다. 둘 다 통과해야
/// 캡처: ① 도구를 싣는 메인 루프 호출이고 ② 알려진 유틸 마커가 없을 때만.
fn is_main_conversation(body: &serde_json::Value) -> bool {
    let has_tools = body
        .get("tools")
        .and_then(|t| t.as_array())
        .is_some_and(|a| !a.is_empty());
    has_tools && !is_utility_request(body) && !is_subagent_request(body)
}

/// 요청 system 프롬프트 앞부분(최대 200자). 메인/서브 판별·진단용.
fn system_text(body: &serde_json::Value) -> String {
    let raw = match body.get("system") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(arr)) => arr
            .iter()
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    };
    raw.chars().take(200).collect()
}

/// 서브에이전트(Task/Agent) 호출인가 — 메인 학생 대화 turns 에 섞이지 않게 분리한다.
/// claude-code 메인 루프 system 은 "You are Claude Code, Anthropic's official CLI" 로
/// 시작하지만, 서브에이전트는 자기 system(claude-code 풀 프롬프트 아님)을 받아 그 문구가
/// 없다. fork 등 메인 system 을 상속하는 서브는 못 거르지만(실측 보정 여지), Explore·
/// 커스텀 등 명백한 서브는 잡아 대화창 혼입을 막는다(거노). system 없으면 메인으로 본다.
fn is_subagent_request(body: &serde_json::Value) -> bool {
    let sys = system_text(body);
    !sys.is_empty() && !sys.contains("You are Claude Code")
}

/// 요청의 reasoning effort 추출. Opus 4.7/4.8·Fable5 는 effort 를 **문자열**로
/// `output_config.effort`(low/medium/high/xhigh/max)에 싣는다 — `thinking.budget_tokens`
/// 는 이 세대에서 제거됐다(보내면 400). 그래서 budget 만 보던 옛 코드는 Opus 4.8 요청에서
/// 항상 None 이라 effort 칩이 빈칸이었다. 신형은 문자열 그대로, 구형(Sonnet 4.5 등)만
/// budget 역산으로 폴백. effort 미지정(기본 high) 요청은 None.
fn effort_from_request(body: &serde_json::Value) -> Option<String> {
    if let Some(effort) = body.pointer("/output_config/effort").and_then(|e| e.as_str()) {
        let e = effort.trim();
        if !e.is_empty() {
            return Some(e.to_string());
        }
    }
    // 구형 폴백: thinking.budget_tokens 역산(경계는 버전 의존 → 실측 보정 가능).
    let budget = body
        .get("thinking")
        .and_then(|t| t.get("budget_tokens"))
        .and_then(|b| b.as_i64())?;
    if budget <= 0 {
        return None;
    }
    let level = if budget < 6_000 {
        "low"
    } else if budget < 13_000 {
        "medium"
    } else if budget < 24_000 {
        "high"
    } else if budget < 50_000 {
        "xhigh"
    } else {
        "max"
    };
    Some(level.to_string())
}

/// 요청 본문 messages[] → 대화 턴(user/assistant, 텍스트 있는 것만).
fn turns_from_request(body: &serde_json::Value) -> Vec<Turn> {
    body.get("messages")
        .and_then(|m| m.as_array())
        .map(|msgs| {
            msgs.iter()
                .filter_map(|m| {
                    let role = m.get("role").and_then(|r| r.as_str())?;
                    // 대화 버블엔 user/assistant 만 — claude-code 가 messages 에 끼우는
                    // system 프리앰블(agent types 목록·MCP 안내 등)은 사람 발화가 아니라
                    // 대화창에 노이즈로 떴다(거노 실측) → 제외.
                    if role != "user" && role != "assistant" {
                        return None;
                    }
                    let text = block_text(m.get("content"));
                    let images = extract_images(m.get("content"));
                    let text = text.trim();
                    // claude-code Stop hook 출력은 user-role 메시지로 "Stop hook feedback:\n…"
                    // 형태로 주입된다(collab inbox drain 등). 사람이 친 게 아니라 선생님
                    // 노란 말풍선에 잘못 떴다(거노 실측) → 제외.
                    if role == "user" && text.starts_with("Stop hook feedback:") {
                        return None;
                    }
                    (!text.is_empty() || !images.is_empty()).then(|| Turn {
                        role: role.to_string(),
                        text: text.to_string(),
                        images,
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
            Some("content_block_start") => {
                // tool_use 블록 시작 — AskUserQuestion 등 인터랙티브 도구. input_json_delta
                // 를 모을 준비(content_block_stop 에서 완성). peek 화면 추정 대체(거노).
                if let Some("tool_use") = v.pointer("/content_block/type").and_then(|x| x.as_str()) {
                    let idx = v.get("index").and_then(|x| x.as_i64()).unwrap_or(0);
                    let name = v
                        .pointer("/content_block/name")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    conv.pending_tools.insert(idx, (name, String::new()));
                }
            }
            Some("content_block_delta") => {
                if let Some(t) = v.pointer("/delta/text").and_then(|x| x.as_str()) {
                    conv.streaming.push_str(t);
                    conv.gen_chars += t.chars().count() as u64;
                    conv.updated = now();
                } else if let Some(th) = v.pointer("/delta/thinking").and_then(|x| x.as_str()) {
                    // thinking 도 출력 토큰에 포함 — 라이브 추정에 반영(화면엔 안 쌓음).
                    conv.gen_chars += th.chars().count() as u64;
                    conv.updated = now();
                } else if let Some(pj) = v.pointer("/delta/partial_json").and_then(|x| x.as_str()) {
                    let idx = v.get("index").and_then(|x| x.as_i64()).unwrap_or(0);
                    conv.gen_chars += pj.chars().count() as u64;
                    if let Some((_, buf)) = conv.pending_tools.get_mut(&idx) {
                        buf.push_str(pj);
                    }
                }
                // 진행 중 라이브 토큰 추정(message_delta 정확값 오기 전 메움) — 단조증가.
                let est = conv.gen_chars / 3;
                if est > conv.tokens_out {
                    conv.tokens_out = est;
                }
            }
            Some("content_block_stop") => {
                let idx = v.get("index").and_then(|x| x.as_i64()).unwrap_or(0);
                if let Some((name, buf)) = conv.pending_tools.remove(&idx) {
                    if let Ok(input) = serde_json::from_str::<serde_json::Value>(&buf) {
                        conv.tool_uses.push(ToolUse { name, input });
                        conv.updated = now();
                    }
                }
            }
            Some("message_delta") => {
                // 정확한 누적 출력 토큰 — Anthropic SSE 가 응답 끝(또는 긴 응답 중 주기적)
                // 으로 준다. 추정(gen_chars/3)과 max 로 단조증가 유지(숫자 안 줄게).
                if let Some(n) = v.pointer("/usage/output_tokens").and_then(|x| x.as_u64()) {
                    conv.tokens_out = conv.tokens_out.max(n);
                    conv.updated = now();
                }
            }
            Some("message_stop") => {
                let txt = conv.streaming.trim().to_string();
                if !txt.is_empty() {
                    conv.turns.push(Turn {
                        role: "assistant".into(),
                        text: txt,
                        images: Vec::new(),
                    });
                }
                conv.streaming.clear();
                conv.updated = now();
            }
            _ => {}
        }
    }
}

/// SSE 바이트 청크를 누적해 유효 UTF-8 prefix 만 accumulate_sse 로 넘긴다. 멀티바이트
/// (한글)가 청크 경계에서 잘려도 안전 — 불완전 꼬리는 버퍼에 남겨 다음 청크와 잇는다.
fn feed_sse_bytes(conv: &mut PaneConv, bytes: &[u8]) {
    conv.sse_bytes.extend_from_slice(bytes);
    let valid = match std::str::from_utf8(&conv.sse_bytes) {
        Ok(_) => conv.sse_bytes.len(),
        Err(e) => e.valid_up_to(),
    };
    if valid > 0 {
        let text = String::from_utf8_lossy(&conv.sse_bytes[..valid]).into_owned();
        conv.sse_bytes.drain(..valid);
        accumulate_sse(&text, conv);
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
            // 캡처 판정 진단 — KASATERM_PROXY_DIAG 켜졌을 때만. 메인/보조 오분류
            // (god nudge·요약 호출이 메인 대화로 새는 케이스)를 raw 로 잡으려 stderr 로.
            if std::env::var_os("KASATERM_PROXY_DIAG").is_some() {
                let arr = v.get("messages").and_then(|m| m.as_array());
                let n_tools = v.get("tools").and_then(|t| t.as_array()).map_or(0, |a| a.len());
                let n_msgs = arr.map_or(0, |a| a.len());
                let tail = arr
                    .and_then(|a| a.last())
                    .map(|m| block_text(m.get("content")))
                    .unwrap_or_default();
                let oc_effort = v.pointer("/output_config/effort").and_then(|e| e.as_str()).unwrap_or("-");
                let budget = v.pointer("/thinking/budget_tokens").and_then(|b| b.as_i64()).unwrap_or(-1);
                eprintln!(
                    "[proxy-diag] /{rest} pane={pane} tools={n_tools} msgs={n_msgs} main={} sub={} effort=oc:{oc_effort}/budget:{budget}/resolved:{} model={} sys={:.60} tail={:.100}",
                    is_main_conversation(&v),
                    is_subagent_request(&v),
                    effort_from_request(&v).unwrap_or_else(|| "-".into()),
                    v.get("model").and_then(|m| m.as_str()).unwrap_or("-"),
                    system_text(&v).replace('\n', " "),
                    tail.replace('\n', " ")
                );
            }
            if is_main_conversation(&v) {
                capture = true;
                let turns = turns_from_request(&v);
                let model = v.get("model").and_then(|m| m.as_str()).map(str::to_string);
                let effort = effort_from_request(&v);
                if let Ok(mut s) = store.lock() {
                    let e = s.entry(pane.clone()).or_default();
                    e.turns = turns;
                    e.streaming.clear();
                    e.sse_buf.clear();
                    e.sse_bytes.clear();
                    e.tool_uses.clear();
                    e.pending_tools.clear();
                    e.tokens_out = 0;
                    e.gen_chars = 0;
                    if let Some(m) = model.filter(|m| !m.is_empty()) {
                        e.model = m;
                    }
                    if let Some(ef) = effort {
                        e.effort = ef;
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
        // accept-encoding 도 빼고 아래서 identity 강제 — Anthropic 이 gzip/br 로 압축하면
        // reqwest(압축 feature 없음)가 압축 바이트를 그대로 흘려 SSE tee 가 "data:" 를 못
        // 찾아 streaming 이 0 이었다(거노 실측: 답변이 다음 턴까지 안 뜸). 평문으로 받아야
        // 라이브 캡처가 되고, claude 로 보내는 응답에도 content-encoding 이 안 붙어 정상.
        if k == axum::http::header::HOST
            || k == axum::http::header::CONTENT_LENGTH
            || k == axum::http::header::ACCEPT_ENCODING
        {
            continue;
        }
        rb = rb.header(k.as_str(), val.as_bytes());
    }
    rb = rb.header(axum::http::header::ACCEPT_ENCODING, "identity");
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
    if std::env::var_os("KASATERM_PROXY_DIAG").is_some() {
        eprintln!(
            "[proxy-diag] resp capture={capture} is_sse={is_sse} status={}",
            status.as_u16()
        );
    }
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
                if let Ok(mut s) = store2.lock() {
                    feed_sse_bytes(s.entry(pane.clone()).or_default(), b);
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
                {"role": "system", "content": "Available agent types for the Agent tool: …"},
            ]
        });
        let turns = turns_from_request(&body);
        assert_eq!(turns.len(), 2); // tool_result(텍스트 블록 없음)·system 프리앰블 제외
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
        // 실제 프롬프트 제안 호출 — 지시문이 **마지막** 메시지에 붙는다(앞 2개만 보던
        // 옛 필터가 놓쳐 채팅에 새어나온 케이스).
        let sugg_last = serde_json::json!({
            "system": "You are Claude Code, Anthropic's CLI",
            "messages": [
                {"role": "user", "content": "실제 첫 질문"},
                {"role": "assistant", "content": [{"type": "text", "text": "답"}]},
                {"role": "user", "content": "두번째 질문"},
                {"role": "assistant", "content": [{"type": "text", "text": "또 답"}]},
                {"role": "user", "content": "[SUGGESTION MODE: Suggest what the user might naturally type next into Claude Code.]"}
            ]
        });
        assert!(is_utility_request(&sugg_last));
        // 메인 대화는 통과
        let main = serde_json::json!({
            "system": "You are Claude Code, Anthropic's CLI",
            "messages": [{"role": "user", "content": "하이"}]
        });
        assert!(!is_utility_request(&main));
        // 중간 메시지에서 사용자가 마커를 인용해도 오탐 없이 통과(양끝만 스캔)
        let quoted = serde_json::json!({
            "messages": [
                {"role": "user", "content": "첫 질문"},
                {"role": "assistant", "content": [{"type": "text", "text": "응"}]},
                {"role": "user", "content": "로그에 SUGGESTION MODE 라고 떠 있는데 뭐야"},
                {"role": "assistant", "content": [{"type": "text", "text": "그건…"}]},
                {"role": "user", "content": "고마워"}
            ]
        });
        assert!(!is_utility_request(&quoted));
    }

    #[test]
    fn captures_only_main_conversation() {
        // 메인 대화 — 도구를 싣고 유틸 마커 없음 → 캡처
        let main = serde_json::json!({
            "system": "You are Claude Code",
            "tools": [{"name": "Bash"}, {"name": "Read"}],
            "messages": [{"role": "user", "content": "하이"}]
        });
        assert!(is_main_conversation(&main));

        // "/compact 할 때 뜨던" quota — 도구 없는 짧은 보조 호출, 알려진 마커도 없음.
        // 통째 교체되는 turns 를 "quota" 한 단어로 덮어쓴 실제 버그(거노) → 스킵.
        let quota = serde_json::json!({
            "messages": [{"role": "user", "content": "quota"}]
        });
        assert!(!is_main_conversation(&quota));

        // /compact 요약 — 도구 없음 + summary 마커 → 스킵
        let summary = serde_json::json!({
            "system": "Summarize the conversation so far into a detailed summary of the conversation",
            "messages": [{"role": "user", "content": "(대화 전체)"}]
        });
        assert!(!is_main_conversation(&summary));

        // 도구 없으면 마커가 없어도 보조로 보고 스킵 — 미지의 백그라운드 호출이
        // 전체 대화를 덮어쓰지 못하게(메인 루프는 항상 도구를 싣는다). 놓친 tools-없는
        // 메인은 다음 메인 턴의 통째 교체로 자가치유되지만, 오캡처는 대화를 날린다.
        let no_tools = serde_json::json!({
            "messages": [
                {"role": "user", "content": "짧은 호출"},
                {"role": "assistant", "content": [{"type": "text", "text": "응"}]}
            ]
        });
        assert!(!is_main_conversation(&no_tools));

        // 빈 tools 배열도 도구 없음으로 취급
        let empty_tools = serde_json::json!({
            "tools": [],
            "messages": [{"role": "user", "content": "x"}]
        });
        assert!(!is_main_conversation(&empty_tools));

        // 도구가 있어도 마지막 메시지가 SUGGESTION 마커면 보조 → 스킵(마커 우선)
        let sugg_with_tools = serde_json::json!({
            "tools": [{"name": "Bash"}],
            "messages": [
                {"role": "user", "content": "실제 질문"},
                {"role": "assistant", "content": [{"type": "text", "text": "답"}]},
                {"role": "user", "content": "[SUGGESTION MODE: Suggest what the user might naturally type next into Claude Code.]"}
            ]
        });
        assert!(!is_main_conversation(&sugg_with_tools));
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
        // 이미지 첨부 시 하네스가 박는 좌표 매핑 안내 줄 제거(거노: 말풍선에 샜음)
        assert_eq!(
            strip_meta(
                "[Image: original 3024x1898, displayed at 2000x1255. Multiply coordinates by 1.51 to map to original image.]\n실제 질문"
            ),
            "실제 질문"
        );
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

    #[test]
    fn sse_feed_handles_utf8_split_midchar() {
        // 한글 응답이 청크 경계에서 멀티바이트 중간으로 잘리는 실측 케이스(거노) —
        // feed_sse_bytes 가 유효 prefix 만 떼고 불완전 꼬리를 다음 청크와 이어야 한다.
        let mut c = PaneConv::default();
        let line = "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"한글\"}}\n";
        let b = line.as_bytes();
        let cut = line.find("한글").unwrap() + 1; // "한" 첫 바이트 직후(멀티바이트 중간)
        feed_sse_bytes(&mut c, &b[..cut]);
        assert_eq!(c.streaming, ""); // 라인 미완성 + 멀티바이트 잘림 → 아직 누적 0
        feed_sse_bytes(&mut c, &b[cut..]);
        assert_eq!(c.streaming, "한글"); // 깨지지 않고 온전히 누적
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
            "tool_uses": c.tool_uses,
            "model": c.model,
            "effort": c.effort,
            "tokens_out": c.tokens_out,
            "updated": c.updated,
        }),
        None => serde_json::json!({ "turns": [], "streaming": "", "tool_uses": [], "model": "", "effort": "", "tokens_out": 0, "updated": 0.0 }),
    }
}
