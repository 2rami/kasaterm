//! 칸 읽기 창구 — 화면 엿보기·명령 블록·대화 기록(transcript)·서브에이전트·배치(`/layout`·`/windows`).

use super::*;

/// `GET /peek?surface=%N&lines=40[&ansi=1]` — a pane's visible screen text.
/// `ansi=1` returns SGR-encoded color/attribute sequences so a viewer can
/// render terminal colors. Without `ansi`, returns plain text (default).
/// Polling-friendly by design: one lock + visible-text copy, no transcript IO.
async fn peek_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let surface = params.get("surface").map(String::as_str).unwrap_or("");
    let body = if surface.is_empty() {
        serde_json::json!({ "ok": false, "error": "surface=%N required" })
    } else {
        let lines = params
            .get("lines")
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(40);
        let ansi = params.get("ansi").map_or(false, |v| v == "1" || v == "true");
        let result = if ansi {
            backend.peek_ansi(surface, lines)
        } else {
            backend.peek(surface, lines)
        };
        match result {
            Ok(text) => serde_json::json!({
                "ok": true,
                "surface_id": surface,
                "text": text,
                "ansi": ansi,
            }),
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        }
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `GET /blocks?surface=%N&limit=50` — a plain terminal pane's Warp-style
/// command blocks (OSC 133 C/D delimited: command, output, exit code,
/// duration). Newest last. Backs the BA GUI's command-block stack.
async fn blocks_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let surface = params.get("surface").map(String::as_str).unwrap_or("");
    let body = if surface.is_empty() {
        serde_json::json!({ "ok": false, "error": "surface=%N required" })
    } else {
        let limit = params
            .get("limit")
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(50);
        match backend.pane_blocks(surface, limit) {
            Ok(blocks) => serde_json::json!({
                "ok": true,
                "surface_id": surface,
                "blocks": blocks,
            }),
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        }
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `GET /transcript?surface=%N&turns=20` — a pane's structured dialogue
/// (user prompts + assistant replies, including off-screen turns). Unlike
/// `/peek` (raw rendered screen), this is the clean conversation for the
/// classroom "click a student → see the chat" view; tool_use/tool_result
/// noise is already stripped by `parse_turn`.
async fn transcript_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let surface = params.get("surface").map(String::as_str).unwrap_or("");
    let body = if surface.is_empty() {
        serde_json::json!({ "ok": false, "error": "surface=%N required" })
    } else {
        let turns = params
            .get("turns")
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(20);
        match backend.transcript_tail(surface, turns) {
            Ok(ts) => serde_json::json!({ "ok": true, "surface_id": surface, "turns": ts }),
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        }
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

const TRANSCRIPT_FILE_TICK: std::time::Duration = std::time::Duration::from_millis(250);

/// `GET /transcript-raw?surface=%N&offset=<n>` — a pane's bound transcript jsonl,
/// raw and *incremental*. `offset=0` (or omitted) returns the tail window with
/// `reset:true`; `offset>0` returns only whole lines appended since that byte
/// (`reset:false`, empty when unchanged). The BA GUI accumulates `offset` and
/// appends, instead of re-parsing the whole (multi-MB) file every poll. Response
/// `{ ok, surface_id, raw, offset, reset }`.
async fn transcript_raw_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let surface = params.get("surface").map(String::as_str).unwrap_or("");
    let offset = params.get("offset").and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
    // `wait_ms` — 새 줄이 없으면 그 칸의 mod 가 대화 행을 알릴 때까지 쥐었다가 다시 읽는다(긴 폴링).
    // mod 없는 칸은 바로 답한다. 행은 mod 가 알린 직후 파일에 닿으므로 한 박자 늦춰 읽는다.
    let wait = params
        .get("wait_ms")
        .and_then(|s| s.parse::<u64>().ok())
        .map(|ms| std::time::Duration::from_millis(ms.min(25_000)));
    let seen = wait.and_then(|_| crate::claude_mod::row_count(surface));
    let mut chunk = (!surface.is_empty()).then(|| backend.transcript_raw(surface, offset));
    if let (Some(wait), Some(mut seen)) = (wait, seen) {
        // 행 알림만 기다리면 행이 아닌 줄(작업 중에 보낸 말의 큐 기록·사진 첨부)이 다음 행까지
        // 묶여 폰에 몇 초씩 늦게 떴다. 짧은 박자로 파일이 자랐는지도 본다 — 안 바뀌면 stat 하나다.
        let deadline = tokio::time::Instant::now() + wait;
        while matches!(&chunk, Some(Ok(c)) if c.raw.is_empty() && !c.reset) {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                break;
            }
            crate::claude_mod::wait_rows(surface, seen, left.min(TRANSCRIPT_FILE_TICK)).await;
            let rows = crate::claude_mod::row_count(surface);
            if let Some(n) = rows.filter(|&n| n > seen) {
                seen = n;
                tokio::time::sleep(std::time::Duration::from_millis(120)).await;
            }
            chunk = Some(backend.transcript_raw(surface, offset));
            // mod 가 떠난 칸은 wait_rows 가 곧바로 돌아온다 — 헛돌지 않고 지금 것으로 답한다.
            if rows.is_none() {
                break;
            }
        }
    }
    let body = match chunk {
        None => serde_json::json!({ "ok": false, "error": "surface=%N required" }),
        Some(chunk) => match chunk {
            Ok(c) => serde_json::json!({
                "ok": true, "surface_id": surface,
                "raw": c.raw, "offset": c.offset, "reset": c.reset,
            }),
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        },
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `GET /session-transcript-raw?id=<uuid>&cwd=<abs>` — a *past* (offline)
/// session's transcript jsonl, raw and unparsed, addressed by its session uuid
/// + the cwd it ran in (no live pane needed). The BA GUI's resume picker reads
/// this to preview a recent session read-only before the user decides to resume.
async fn session_transcript_raw_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let id = params.get("id").map(String::as_str).unwrap_or("");
    let cwd = params.get("cwd").filter(|s| !s.is_empty()).map(String::as_str);
    let body = if id.is_empty() {
        serde_json::json!({ "ok": false, "error": "id=<uuid> required" })
    } else {
        match backend.session_transcript_raw(id, cwd) {
            Ok(raw) => serde_json::json!({ "ok": true, "id": id, "raw": raw }),
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        }
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `GET /subagents?surface=%N` — the subagents (Task/Agent) a pane's claude has
/// spawned, newest first, from its `subagents/agent-*.meta.json` sidecars. The
/// BA GUI lists these so the user can drill into a subagent's full dialogue.
async fn subagents_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let surface = params.get("surface").map(String::as_str).unwrap_or("");
    let body = if surface.is_empty() {
        serde_json::json!({ "ok": false, "error": "surface=%N required" })
    } else {
        match backend.subagents(surface) {
            Ok(list) => serde_json::json!({ "ok": true, "surface_id": surface, "subagents": list }),
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        }
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `GET /subagent-transcript-raw?surface=%N&agentId=<id>` — one subagent's
/// transcript jsonl, raw and unparsed. Same `{ raw }` shape as `/transcript-raw`;
/// the BA GUI renders it with the same per-tool path.
async fn subagent_transcript_raw_handler(
    backend: Arc<dyn Backend>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let surface = params.get("surface").map(String::as_str).unwrap_or("");
    let agent_id = params.get("agentId").map(String::as_str).unwrap_or("");
    let body = if surface.is_empty() || agent_id.is_empty() {
        serde_json::json!({ "ok": false, "error": "surface=%N and agentId=<id> required" })
    } else {
        match backend.subagent_transcript_raw(surface, agent_id) {
            Ok(raw) => serde_json::json!({ "ok": true, "surface_id": surface, "agent_id": agent_id, "raw": raw }),
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        }
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// `GET /layout` — 현재 윈도우의 pane split 배치(% rect 배열, window_layout 재활용).
/// BA GUI 가 이걸로 터미널 분할을 그대로 미러한 그리드를 그린다(각 pane = 세션 뷰어 칸).
/// rect 가 이미 % 좌표라 프론트는 position:absolute 로 배치만 하면 된다.
/// 방(윈도우)마다의 배치 — 폰 허브 미니맵. 보고 있는 방은 `/layout` 과 같은 사각형이고,
/// 안 보는 방은 데스크톱이 창 크기로 펴 둔 배치의 비율이다.
async fn windows_handler(backend: Arc<dyn Backend>) -> impl IntoResponse {
    let body = match backend.windows_overview() {
        Ok(windows) => serde_json::json!({ "ok": true, "windows": windows }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

async fn layout_handler(backend: Arc<dyn Backend>) -> impl IntoResponse {
    let body = match backend.window_layout() {
        Ok(panes) => serde_json::json!({ "ok": true, "panes": panes }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    };
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(body))
}

/// 이 모듈 창구의 라우트. `router` 가 한 표로 합친 뒤 공통 레이어(Origin·토큰 가드)를 두른다.
pub(super) fn routes(backend: &Arc<dyn Backend>) -> axum::Router {
    let peek_backend = backend.clone();
    let blocks_backend = backend.clone();
    let transcript_backend = backend.clone();
    let transcript_raw_backend = backend.clone();
    let session_transcript_raw_backend = backend.clone();
    let subagents_backend = backend.clone();
    let subagent_transcript_raw_backend = backend.clone();
    let layout_backend = backend.clone();
    let windows_backend = backend.clone();
    axum::Router::new()
        .route(
            "/transcript",
            get(move |q: Query<std::collections::HashMap<String, String>>| {
                transcript_handler(transcript_backend.clone(), q)
            }),
        )
        .route(
            "/transcript-raw",
            get(move |q: Query<std::collections::HashMap<String, String>>| {
                transcript_raw_handler(transcript_raw_backend.clone(), q)
            }),
        )
        .route(
            "/session-transcript-raw",
            get(move |q: Query<std::collections::HashMap<String, String>>| {
                session_transcript_raw_handler(session_transcript_raw_backend.clone(), q)
            }),
        )
        .route(
            "/subagents",
            get(move |q: Query<std::collections::HashMap<String, String>>| {
                subagents_handler(subagents_backend.clone(), q)
            }),
        )
        .route(
            "/subagent-transcript-raw",
            get(move |q: Query<std::collections::HashMap<String, String>>| {
                subagent_transcript_raw_handler(subagent_transcript_raw_backend.clone(), q)
            }),
        )
        .route(
            "/layout",
            get(move || layout_handler(layout_backend.clone())),
        )
        .route(
            "/windows",
            get(move || windows_handler(windows_backend.clone())),
        )
        .route(
            "/peek",
            get(move |q: Query<std::collections::HashMap<String, String>>| {
                peek_handler(peek_backend.clone(), q)
            }),
        )
        .route(
            "/blocks",
            get(move |q: Query<std::collections::HashMap<String, String>>| {
                blocks_handler(blocks_backend.clone(), q)
            }),
        )
}
