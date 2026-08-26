//! Method dispatcher. One function per `method:` string, mapping
//! protocol requests to `Backend` calls and folding the result back
//! into a `Response`.
//!
//! Methods covered as of the Phase B foundation:
//!
//! - `system.ping`               — liveness probe, no backend touch
//! - `system.identify`           — returns workspace/surface IDs (used by
//!                                 cmux's auto-detect)
//! - `system.capabilities`       — reports which methods this host implements
//! - `workspace.list`            — backend `list_workspaces`
//! - `workspace.current`         — backend `current_workspace`
//! - `surface.list`              — backend `list_surfaces`
//! - `surface.focus`             — backend `focus_surface`
//! - `surface.split`             — backend `split_surface`
//! - `surface.send_text`         — backend `send_text`
//! - `surface.send_key`          — backend `send_key`
//!
//! Methods cmux exposes that we don't implement yet (notifications,
//! sidebar metadata) fall through to METHOD_NOT_FOUND. They're listed
//! in `system.capabilities` only when the host opts in.

use crate::backend::{Backend, SplitDirection};
use crate::protocol::{codes, ErrorObj, Request, Response};
use serde_json::{json, Value};

/// Top-level dispatch. Looks at `req.method` and routes. Returns a
/// fully-formed `Response` ready to be serialized and written to the
/// socket.
pub fn dispatch(backend: &dyn Backend, req: Request) -> Response {
    let id = req.id.clone();
    match req.method.as_str() {
        "system.ping" => Response::success(id, json!({"pong": true})),
        "system.identify" => system_identify(backend, id),
        "system.capabilities" => system_capabilities(id),
        "workspace.list" => match backend.list_workspaces() {
            Ok(ws) => Response::success(id, json!({"workspaces": ws})),
            Err(e) => backend_err(id, e),
        },
        "workspace.current" => match backend.current_workspace() {
            Ok(Some(ws)) => Response::success(id, json!({"workspace": ws})),
            Ok(None) => Response::success(id, json!({"workspace": Value::Null})),
            Err(e) => backend_err(id, e),
        },
        "surface.list" => match backend.list_surfaces() {
            Ok(s) => Response::success(id, json!({"surfaces": s})),
            Err(e) => backend_err(id, e),
        },
        "surface.focus" => surface_focus(backend, id, &req.params),
        "surface.split" => surface_split(backend, id, &req.params),
        "surface.remote" => surface_remote(backend, id, &req.params),
        "surface.split_fleet" => surface_split_fleet(backend, id, &req.params),
        "surface.capture" => surface_capture(backend, id, &req.params),
        // 되살리기 목록. `pane` 을 주면 그것만 끄고 남은 목록을 돌려준다 — 조회와 종료를
        // 한 왕복에 두는 것은 인덱스가 아니라 pane id 로 지목하기 때문이다(목록이 그
        // 사이 바뀌어도 엉뚱한 학생을 죽이지 않는다).
        "surface.closed" => {
            let want = req.params.get("pane").and_then(|v| v.as_str());
            match backend.closed_panes(want) {
                Ok(v) => Response::success(id, v),
                Err(e) => backend_err(id, e),
            }
        }
        "surface.send_text" => surface_send_text(backend, id, &req.params),
        "surface.send_key" => surface_send_key(backend, id, &req.params),
        "surface.send_raw" => surface_send_raw(backend, id, &req.params),
        "surface.resize" => surface_resize(backend, id, &req.params),
        "surface.scroll" => surface_scroll(backend, id, &req.params),
        "session.new" => simple(id, backend.new_session()),
        "window.new" => simple(id, backend.new_window()),
        "session.switch" => switch_by_idx(id, &req.params, |i| backend.switch_session(i)),
        "window.switch" => switch_by_idx(id, &req.params, |i| backend.switch_window(i)),
        "window.close" => switch_by_idx(id, &req.params, |i| backend.close_window(i)),
        "window.reorder" => {
            let from = req.params.get("from").and_then(|v| v.as_u64());
            let to = req.params.get("to").and_then(|v| v.as_u64());
            match (from, to) {
                (Some(f), Some(t)) => simple(id, backend.reorder_window(f as usize, t as usize)),
                _ => param_err(id, "window.reorder requires `from` and `to` (usize)"),
            }
        }
        "session.close" => switch_by_idx(id, &req.params, |i| backend.close_session(i)),
        "session.resume" => session_resume(backend, id, &req.params),
        "session.recent" => session_recent(backend, id, &req.params),
        "surface.close" => surface_close(backend, id, &req.params),
        "surface.dock" => surface_dock(backend, id, &req.params),
        "surface.undock" => surface_undock(backend, id, &req.params),
        "surface.rename" => surface_rename(backend, id, &req.params),
        "window.rename" => window_rename(backend, id, &req.params),
        "surface.set_color" => surface_set_color(backend, id, &req.params),
        "surface.repersona" => surface_repersona(backend, id, &req.params),
        "surface.report_cwd" => surface_report_cwd(backend, id, &req.params),
        "surface.swap" => surface_swap(backend, id, &req.params),
        "surface.move" => surface_move(backend, id, &req.params),
        "surface.new_tab" => match backend.new_tab(
            req.params.get("outer").and_then(|v| v.as_str()),
            // 기본 no-focus: 자동화가 자기 pane 탭에 서브에이전트를 띄울 때 부모
            // 화면(사람이 보던 대화)을 덮지 않는다. `focus:true` 는 CLI --focus.
            req.params.get("focus").and_then(|v| v.as_bool()).unwrap_or(false),
        ) {
            Ok(s) => {
                // split 과 같은 이유로 학생 이름을 **여기서** 준다 — 탭으로 띄워도
                // 다음 할 일은 SendMessage 라, 이름이 없으면 board 를 되짚는
                // 왕복이 생긴다.
                let agent = backend.pane_agent(&s.id);
                let mut body = json!({"surface": s});
                if let (Some((a, t)), Some(o)) = (agent, body.as_object_mut()) {
                    o.insert("agent".into(), json!(a));
                    o.insert("team".into(), json!(t));
                }
                Response::success(id, body)
            }
            Err(e) => backend_err(id, e),
        },
        "surface.resize_divider" => surface_resize_divider(backend, id, &req.params),
        "surface.set_ratio" => surface_set_ratio(backend, id, &req.params),
        "surface.peek" => surface_peek(backend, id, &req.params),
        "surface.open_preview" => surface_open_preview(backend, id, &req.params),
        "web.drive" => web_drive(backend, id, &req.params),
        "collab.board" => {
            // Opt-in screen capture: a plain board stays metadata-only (cheap,
            // what board-watch polling wants), but an orchestrator pane can
            // pass `screen_lines` to fold each pane's visible tail in — board
            // + peek in one round-trip.
            let screen_lines = req
                .params
                .get("screen_lines")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);
            match backend.collab_board() {
                Ok(mut board) => {
                    if let Some(lines) = screen_lines {
                        for entry in &mut board {
                            entry.screen = backend.peek(&entry.surface_id, lines).ok();
                        }
                    }
                    Response::success(id, json!({ "board": board }))
                }
                Err(e) => backend_err(id, e),
            }
        }
        "window.layout" => match backend.window_layout() {
            Ok(panes) => Response::success(id, json!({ "panes": panes })),
            Err(e) => backend_err(id, e),
        },
        "window.list" => match backend.windows_overview() {
            Ok(windows) => Response::success(id, json!({ "windows": windows })),
            Err(e) => backend_err(id, e),
        },
        "collab.bind_transcript" => collab_bind_transcript(backend, id, &req.params),
        "collab.transcript" => collab_transcript(backend, id, &req.params),
        "surface.notify" => surface_notify(backend, id, &req.params),
        "surface.attention" => surface_attention(backend, id, &req.params),
        "surface.done" => surface_done(backend, id, &req.params),
        "surface.agent_status" => surface_agent_status(backend, id, &req.params),
        unknown => Response {
            id,
            ok: false,
            result: None,
            error: Some(ErrorObj {
                code: codes::METHOD_NOT_FOUND,
                message: format!("unknown method: {unknown}"),
            }),
        },
    }
}

fn backend_err(id: Value, e: anyhow::Error) -> Response {
    Response::error(id, codes::BACKEND_ERROR, format!("{e:#}"))
}

/// Fold a no-args backend call into an `{ok:true}` response.
fn simple(id: Value, r: anyhow::Result<()>) -> Response {
    match r {
        Ok(()) => Response::success(id, json!({"ok": true})),
        Err(e) => backend_err(id, e),
    }
}

/// Dispatch a method whose only param is a `idx` (number): session/window
/// switch + session close.
fn switch_by_idx(
    id: Value,
    params: &Value,
    f: impl FnOnce(usize) -> anyhow::Result<()>,
) -> Response {
    let idx = match params.get("idx").and_then(|v| v.as_u64()) {
        Some(n) => n as usize,
        None => return param_err(id, "method requires `idx` (number)"),
    };
    simple(id, f(idx))
}

fn param_err(id: Value, msg: impl Into<String>) -> Response {
    Response::error(id, codes::INVALID_PARAMS, msg)
}

fn system_identify(backend: &dyn Backend, id: Value) -> Response {
    // cmux's identify returns the calling shell's workspace + surface
    // IDs. We resolve via `current_workspace`; surface_id is just the
    // first one in the workspace for now (single-pane PoC). Multi-pane
    // hosts will refine this later.
    let workspace = match backend.current_workspace() {
        Ok(w) => w,
        Err(e) => return backend_err(id, e),
    };
    let surfaces = match backend.list_surfaces() {
        Ok(s) => s,
        Err(e) => return backend_err(id, e),
    };
    Response::success(
        id,
        json!({
            "workspace": workspace,
            "surface": surfaces.first(),
        }),
    )
}

fn system_capabilities(id: Value) -> Response {
    // Static list — what this server is willing to dispatch. cmux uses
    // this for feature detection so callers can degrade gracefully when
    // a method isn't present.
    Response::success(
        id,
        json!({
            "methods": [
                "system.ping",
                "system.identify",
                "system.capabilities",
                "workspace.list",
                "workspace.current",
                "surface.list",
                "surface.focus",
                "surface.split",
                "surface.remote",
                "surface.split_fleet",
                "surface.closed",
                "surface.send_text",
                "surface.send_key",
                "surface.send_raw",
                "surface.resize",
                "surface.scroll",
                "surface.peek",
                "surface.open_preview",
                "web.drive",
                "surface.capture",
                "surface.dock",
                "surface.undock",
                "surface.move",
                "surface.rename",
                "window.rename",
                "surface.set_color",
                "surface.repersona",
                "surface.resize_divider",
                "surface.set_ratio",
                "collab.board",
                "window.layout",
                "window.list",
                "collab.bind_transcript",
                "collab.transcript",
                "surface.notify",
                "surface.attention",
                "surface.done",
            ],
        }),
    )
}

/// 웹 pane 조종(backend.rs `web_drive` 참조). `arg` 는 op 마다 뜻이 다르다 —
/// eval 은 JS 원문, shot 은 저장할 절대경로, text/url 은 무시.
fn web_drive(backend: &dyn Backend, id: Value, params: &Value) -> Response {
    let op = match params.get("op").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return param_err(id, "web.drive requires `op` (eval|text|shot|url)"),
    };
    let arg = params.get("arg").and_then(|v| v.as_str()).unwrap_or("");
    let surface = params.get("surface").and_then(|v| v.as_str());
    match backend.web_drive(op, arg, surface) {
        Ok(v) => Response::success(id, json!({ "value": v })),
        Err(e) => backend_err(id, e),
    }
}

fn surface_open_preview(backend: &dyn Backend, id: Value, params: &Value) -> Response {
    let kind = match params.get("kind").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return param_err(id, "surface.open_preview requires `kind` (image|markdown|web)"),
    };
    let path = match params.get("path").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return param_err(id, "surface.open_preview requires `path` (string)"),
    };
    let target = params.get("target").and_then(|v| v.as_str());
    match backend.open_preview(kind, path, target) {
        Ok(()) => Response::success(id, json!({"ok": true})),
        Err(e) => backend_err(id, e),
    }
}

fn collab_bind_transcript(backend: &dyn Backend, id: Value, params: &Value) -> Response {
    let surface_id = match params.get("surface_id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return param_err(id, "collab.bind_transcript requires `surface_id` (string)"),
    };
    let path = match params.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return param_err(id, "collab.bind_transcript requires `path` (string)"),
    };
    match backend.bind_transcript(surface_id, path) {
        Ok(()) => Response::success(id, json!({"ok": true})),
        Err(e) => backend_err(id, e),
    }
}

fn collab_transcript(backend: &dyn Backend, id: Value, params: &Value) -> Response {
    let surface_id = match params.get("surface_id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return param_err(id, "collab.transcript requires `surface_id` (string)"),
    };
    // Default to the last 6 turns — enough to see the current exchange plus a
    // little context, without dumping a whole session.
    let turns = params
        .get("turns")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(6);
    match backend.transcript_tail(surface_id, turns) {
        Ok(t) => Response::success(id, json!({ "turns": t })),
        Err(e) => backend_err(id, e),
    }
}

fn surface_notify(backend: &dyn Backend, id: Value, params: &Value) -> Response {
    let surface_id = match params.get("surface_id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return param_err(id, "surface.notify requires `surface_id` (string)"),
    };
    let title = params.get("title").and_then(|v| v.as_str()).unwrap_or("");
    let body = params.get("body").and_then(|v| v.as_str()).unwrap_or("");
    match backend.notify(surface_id, title, body) {
        Ok(()) => Response::success(id, json!({"ok": true})),
        Err(e) => backend_err(id, e),
    }
}

fn surface_attention(backend: &dyn Backend, id: Value, params: &Value) -> Response {
    let surface_id = match params.get("surface_id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return param_err(id, "surface.attention requires `surface_id` (string)"),
    };
    let reason = params.get("reason").and_then(|v| v.as_str()).unwrap_or("");
    match backend.attention(surface_id, reason) {
        Ok(()) => Response::success(id, json!({"ok": true})),
        Err(e) => backend_err(id, e),
    }
}

fn surface_done(backend: &dyn Backend, id: Value, params: &Value) -> Response {
    let surface_id = match params.get("surface_id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return param_err(id, "surface.done requires `surface_id` (string)"),
    };
    // outcome 두 값 강제 — board status 칸에서 겪은 "free text 라더니 소비부는 정확
    // 일치" 함정을 서버 입구에서 막는다. 실패도 정식 보고다(프로즈에만 실으면 못 읽음).
    let outcome = match params.get("outcome").and_then(|v| v.as_str()) {
        Some(o @ ("succeeded" | "failed")) => o,
        Some(other) => {
            return param_err(
                id,
                format!("surface.done `outcome` must be \"succeeded\" or \"failed\", got \"{other}\""),
            )
        }
        None => return param_err(id, "surface.done requires `outcome` (succeeded|failed)"),
    };
    let summary = params.get("summary").and_then(|v| v.as_str()).unwrap_or("");
    match backend.pane_done(surface_id, outcome, summary) {
        Ok(()) => Response::success(id, json!({"ok": true})),
        Err(e) => backend_err(id, e),
    }
}

fn surface_agent_status(backend: &dyn Backend, id: Value, params: &Value) -> Response {
    let surface_id = match params.get("surface_id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return param_err(id, "surface.agent_status requires `surface_id` (string)"),
    };
    // `surface.done` 의 outcome 과 같은 이유로 값을 입구에서 좁힌다 — 오타 하나가
    // 조용히 「아무 일도 안 일어남」이 되는 자리라, 훅 스크립트가 틀리면 알아야 한다.
    let phase = match params.get("phase").and_then(|v| v.as_str()) {
        Some(p @ ("start" | "end" | "clear")) => p,
        Some(other) => {
            return param_err(
                id,
                format!("surface.agent_status `phase` must be start|end|clear, got \"{other}\""),
            )
        }
        None => return param_err(id, "surface.agent_status requires `phase` (start|end|clear)"),
    };
    let kind = match params.get("kind").and_then(|v| v.as_str()) {
        Some(k @ ("subagent" | "background")) => k,
        Some(other) => {
            return param_err(
                id,
                format!("surface.agent_status `kind` must be subagent|background, got \"{other}\""),
            )
        }
        None => return param_err(id, "surface.agent_status requires `kind` (subagent|background)"),
    };
    let key = params.get("key").and_then(|v| v.as_str()).unwrap_or("");
    let label = params.get("label").and_then(|v| v.as_str()).unwrap_or("");
    // `clear` 는 key 를 안 본다(그 kind 통째). start/end 는 짝지을 값이 있어야 한다.
    if phase != "clear" && key.is_empty() {
        return param_err(id, "surface.agent_status start/end requires a non-empty `key`");
    }
    match backend.agent_status(surface_id, phase, kind, key, label) {
        Ok(()) => Response::success(id, json!({"ok": true})),
        Err(e) => backend_err(id, e),
    }
}

fn surface_peek(backend: &dyn Backend, id: Value, params: &Value) -> Response {
    let surface_id = match params.get("surface_id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return param_err(id, "surface.peek requires `surface_id` (string)"),
    };
    // Default to a screenful-ish tail; callers wanting the whole buffer
    // pass a big number.
    let lines = params
        .get("lines")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(30);
    match backend.peek(surface_id, lines) {
        Ok(text) => Response::success(id, json!({ "text": text })),
        Err(e) => backend_err(id, e),
    }
}

fn surface_focus(backend: &dyn Backend, id: Value, params: &Value) -> Response {
    let surface_id = match params.get("surface_id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return param_err(id, "surface.focus requires `surface_id` (string)"),
    };
    match backend.focus_surface(surface_id) {
        Ok(()) => Response::success(id, json!({"ok": true})),
        Err(e) => backend_err(id, e),
    }
}

/// pane 한 칸을 PNG 로 찍는다. `path` 를 안 주면 백엔드가 임시 경로를 만든다.
/// `max_width` 미지정 시 1200 — 큰 이미지는 받는 쪽 컨텍스트를 크게 태우므로
/// 기본값부터 작게 잡는다.
fn surface_capture(backend: &dyn Backend, id: Value, params: &Value) -> Response {
    let surface_id = match params.get("surface_id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return param_err(id, "surface.capture requires `surface_id` (string)"),
    };
    let path = params.get("path").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
    let max_width = params
        .get("max_width")
        .and_then(|v| v.as_u64())
        .unwrap_or(1200) as u32;
    match backend.capture_surface(surface_id, path, max_width) {
        Ok(v) => Response::success(id, v),
        Err(e) => backend_err(id, e),
    }
}

fn surface_close(backend: &dyn Backend, id: Value, params: &Value) -> Response {
    let surface_id = match params.get("surface_id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return param_err(id, "surface.close requires `surface_id` (string)"),
    };
    match backend.close_surface(surface_id) {
        Ok(()) => Response::success(id, json!({"ok": true})),
        Err(e) => backend_err(id, e),
    }
}

fn surface_dock(backend: &dyn Backend, id: Value, params: &Value) -> Response {
    let surface_id = match params.get("surface_id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return param_err(id, "surface.dock requires `surface_id` (string)"),
    };
    match backend.dock_surface(surface_id) {
        Ok(()) => Response::success(id, json!({"ok": true})),
        Err(e) => backend_err(id, e),
    }
}

fn surface_undock(backend: &dyn Backend, id: Value, params: &Value) -> Response {
    let surface_id = match params.get("surface_id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return param_err(id, "surface.undock requires `surface_id` (string)"),
    };
    match backend.undock_surface(surface_id) {
        Ok(()) => Response::success(id, json!({"ok": true})),
        Err(e) => backend_err(id, e),
    }
}

fn surface_rename(backend: &dyn Backend, id: Value, params: &Value) -> Response {
    let surface_id = match params.get("surface_id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return param_err(id, "surface.rename requires `surface_id` (string)"),
    };
    let title = match params.get("title").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return param_err(id, "surface.rename requires `title` (string)"),
    };
    match backend.rename_surface(surface_id, title) {
        Ok(()) => Response::success(id, json!({"ok": true})),
        Err(e) => backend_err(id, e),
    }
}

fn window_rename(backend: &dyn Backend, id: Value, params: &Value) -> Response {
    let surface_id = match params.get("surface_id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return param_err(id, "window.rename requires `surface_id` (string)"),
    };
    let title = match params.get("title").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return param_err(id, "window.rename requires `title` (string)"),
    };
    match backend.rename_window(surface_id, title) {
        Ok(()) => Response::success(id, json!({"ok": true})),
        Err(e) => backend_err(id, e),
    }
}

fn surface_set_color(backend: &dyn Backend, id: Value, params: &Value) -> Response {
    let surface_id = match params.get("surface_id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return param_err(id, "surface.set_color requires `surface_id` (string)"),
    };
    let color = match params.get("color").and_then(|v| v.as_str()).and_then(parse_hex_color) {
        Some(c) => c,
        None => return param_err(id, "surface.set_color requires `color` as #rrggbb"),
    };
    match backend.set_color(surface_id, color) {
        Ok(()) => Response::success(id, json!({"ok": true})),
        Err(e) => backend_err(id, e),
    }
}

/// pane 캐릭터 재배정(respawn 없음). 이름은 **아는 명부의 합집합**(활성∪번들∪설치
/// 테마)에서 찾으므로, 활성 테마 밖 캐릭터도 지정할 수 있다 — 나쵸 전용 테마처럼
/// 설치만 해 두고 특정 pane 에서만 쓰는 갈래가 이걸 탄다. GUI 쪽 가드가 로스터 밖
/// 이름을 막으므로 여기서는 빈 문자열만 거른다.
fn surface_repersona(backend: &dyn Backend, id: Value, params: &Value) -> Response {
    let surface_id = match params.get("surface_id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return param_err(id, "surface.repersona requires `surface_id` (string)"),
    };
    let character = match params.get("character").and_then(|v| v.as_str()).filter(|s| !s.is_empty())
    {
        Some(c) => c,
        None => return param_err(id, "surface.repersona requires `character` (string)"),
    };
    match backend.repersona(surface_id, character) {
        Ok(()) => Response::success(id, json!({"ok": true})),
        Err(e) => backend_err(id, e),
    }
}

fn surface_report_cwd(backend: &dyn Backend, id: Value, params: &Value) -> Response {
    let surface_id = match params.get("surface_id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return param_err(id, "surface.report_cwd requires `surface_id` (string)"),
    };
    let cwd = match params.get("cwd").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return param_err(id, "surface.report_cwd requires `cwd` (string)"),
    };
    let session_id = params.get("session_id").and_then(|v| v.as_str()).unwrap_or("");
    // 컨텍스트 창·사용 토큰은 선택 — 구버전 statusline 은 안 보내고, 그때는 0(미상)이라
    // GUI 가 종전 추정 폴백으로 떨어진다.
    let ctx_window = params.get("ctx_window").and_then(|v| v.as_u64()).unwrap_or(0);
    let ctx_tokens = params.get("ctx_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
    // 모델·effort 도 선택 — 빈 문자열이면 "미보고"라 종전 값을 안 덮는다.
    let model = params.get("model").and_then(|v| v.as_str()).unwrap_or("");
    let effort = params.get("effort").and_then(|v| v.as_str()).unwrap_or("");
    match backend.report_cwd(surface_id, cwd, session_id, ctx_window, ctx_tokens, model, effort) {
        Ok(()) => Response::success(id, json!({"ok": true})),
        Err(e) => backend_err(id, e),
    }
}

fn surface_swap(backend: &dyn Backend, id: Value, params: &Value) -> Response {
    let a = match params.get("a").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return param_err(id, "surface.swap requires `a` (surface_id)"),
    };
    let b = match params.get("b").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return param_err(id, "surface.swap requires `b` (surface_id)"),
    };
    match backend.swap_surfaces(a, b) {
        Ok(()) => Response::success(id, json!({"ok": true})),
        Err(e) => backend_err(id, e),
    }
}

fn surface_move(backend: &dyn Backend, id: Value, params: &Value) -> Response {
    let surface_id = match params.get("surface_id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return param_err(id, "surface.move requires `surface_id` (string)"),
    };
    let target = match params.get("target").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return param_err(id, "surface.move requires `target` (surface_id)"),
    };
    let dir = match params.get("direction").and_then(|v| v.as_str()) {
        Some("left") => SplitDirection::Left,
        Some("right") => SplitDirection::Right,
        Some("up") => SplitDirection::Up,
        Some("down") => SplitDirection::Down,
        _ => {
            return param_err(id, "surface.move requires `direction` (left/right/up/down)")
        }
    };
    match backend.move_surface(surface_id, target, dir) {
        Ok(()) => Response::success(id, json!({"ok": true})),
        Err(e) => backend_err(id, e),
    }
}

fn surface_set_ratio(backend: &dyn Backend, id: Value, params: &Value) -> Response {
    let surface_id = match params.get("surface_id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return param_err(id, "surface.set_ratio requires `surface_id` (string)"),
    };
    let ratio = match params.get("ratio").and_then(|v| v.as_f64()) {
        Some(r) => r as f32,
        None => return param_err(id, "surface.set_ratio requires `ratio` (number, 0..1)"),
    };
    match backend.set_split_ratio(surface_id, ratio) {
        Ok(()) => Response::success(id, json!({"ok": true})),
        Err(e) => backend_err(id, e),
    }
}

fn surface_resize_divider(backend: &dyn Backend, id: Value, params: &Value) -> Response {
    let path: Vec<u8> = match params.get("path").and_then(|v| v.as_array()) {
        Some(arr) => arr.iter().filter_map(|n| n.as_u64().map(|x| x as u8)).collect(),
        None => return param_err(id, "surface.resize_divider requires `path` (array of 0/1)"),
    };
    let ratio = match params.get("ratio").and_then(|v| v.as_f64()) {
        Some(r) => r as f32,
        None => return param_err(id, "surface.resize_divider requires `ratio` (number)"),
    };
    match backend.resize_divider(&path, ratio) {
        Ok(()) => Response::success(id, json!({"ok": true})),
        Err(e) => backend_err(id, e),
    }
}

/// Parse `#rrggbb` (or bare `rrggbb`) into RGBA with full alpha.
fn parse_hex_color(s: &str) -> Option<[u8; 4]> {
    let s = s.strip_prefix('#').unwrap_or(s);
    if s.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some([r, g, b, 255])
}

fn surface_split(backend: &dyn Backend, id: Value, params: &Value) -> Response {
    let dir = match params.get("direction").and_then(|v| v.as_str()) {
        Some("left") => SplitDirection::Left,
        Some("right") => SplitDirection::Right,
        Some("up") => SplitDirection::Up,
        Some("down") => SplitDirection::Down,
        Some("auto") => SplitDirection::Auto,
        Some(other) => {
            return param_err(
                id,
                format!("surface.split: direction must be left/right/up/down/auto, got {other:?}"),
            )
        }
        // 방향 생략 = auto. 부른 쪽이 pane 모양을 모르는 게 정상이라 이게 기본이다.
        None => SplitDirection::Auto,
    };
    // 기본 no-focus (자동화 경로): `focus:true` 를 명시할 때만 새 pane 으로 포커스
    // 이동. CLI 의 `--focus` 플래그가 이 값을 채운다.
    let focus = params.get("focus").and_then(|v| v.as_bool()).unwrap_or(false);
    // `from` 없이 오면 포커스된 pane 을 쪼갠다(사람이 키보드로 부른 경우). CLI 는
    // pane 안에서 부르면 자기 id 를 채워 보낸다 — 그래야 에이전트의 split 이 사람이
    // 보고 있는 창을 건드리지 않는다.
    let from = params.get("from").and_then(|v| v.as_str());
    match backend.split_surface(dir, focus, from) {
        Ok(s) => {
            // 새 pane 이 claude 로 뜨면 쓸 이름을 **여기서** 알려 준다 — 부른 쪽이
            // 부팅을 기다렸다 board 를 되짚는 왕복이 통째로 사라진다(거노: "바로
            // SendMessage 하면 되는데"). 모르면 키를 아예 안 싣는다.
            let agent = backend.pane_agent(&s.id);
            let mut body = json!({"surface": s});
            if let (Some((a, t)), Some(o)) = (agent, body.as_object_mut()) {
                o.insert("agent".into(), json!(a));
                o.insert("team".into(), json!(t));
            }
            Response::success(id, body)
        }
        Err(e) => backend_err(id, e),
    }
}

/// `surface.remote` — 원격 PTY 호스트의 세션을 pane 으로. params:
/// `{base, cwd?, pane?, from?}` — `pane` 이 있으면 이어받기, 없으면 스폰.
fn surface_remote(backend: &dyn Backend, id: Value, params: &Value) -> Response {
    let Some(base) = params.get("base").and_then(|v| v.as_str()) else {
        return param_err(id, "surface.remote requires `base` (예: http://127.0.0.1:18766)");
    };
    let cwd = params.get("cwd").and_then(|v| v.as_str());
    let pane = params.get("pane").and_then(|v| v.as_str());
    let from = params.get("from").and_then(|v| v.as_str());
    match backend.remote_pane(base, cwd, pane, from) {
        Ok(s) => Response::success(id, json!({"surface": s})),
        Err(e) => backend_err(id, e),
    }
}

/// `surface.split_fleet` — pane 여러 개를 한 번에 배치한다.
/// params: `{count, from?, host_ratio?}`.
///
/// 응답에 `requested` 와 `placed` 를 **함께** 싣는다. 하한(80칸·16줄)에 걸려 요청보다
/// 적게 앉을 수 있는데, 개수만 세어 보고 「됐다」로 읽으면 「다섯 불렀는데 셋」이 또
/// 조용히 지나간다 — 부른 쪽이 그 차이를 사람에게 말할 수 있어야 한다.
fn surface_split_fleet(backend: &dyn Backend, id: Value, params: &Value) -> Response {
    let count = match params.get("count").and_then(|v| v.as_u64()) {
        Some(n) if n >= 1 => n as usize,
        _ => return param_err(id, "surface.split_fleet requires `count` >= 1"),
    };
    // 상한을 두는 이유는 실수 한 번의 값이 크기 때문이다 — `count: 500` 은 셸 500 개를
    // 띄우고 나서야 하한에 걸린다. 하한 계산도 이걸 자르지만 그건 pane 을 낳은 뒤다.
    if count > 16 {
        return param_err(id, "surface.split_fleet: count 는 16 이하");
    }
    let from = params.get("from").and_then(|v| v.as_str());
    let host_ratio = params.get("host_ratio").and_then(|v| v.as_f64()).map(|f| f as f32);
    match backend.split_fleet(count, from, host_ratio) {
        Ok(surfaces) => {
            // 새 pane 이 claude 로 뜨면 쓸 이름을 여기서 함께 준다 — N 명을 띄우면
            // 다음 할 일이 N 통의 SendMessage 라, 이름이 같이 나와야 board 를
            // 되짚는 왕복이 안 생긴다(`surface.split` 과 같은 이유).
            let agents: Vec<Value> = surfaces
                .iter()
                .map(|s| match backend.pane_agent(&s.id) {
                    Some((a, t)) => json!({"agent": a, "team": t}),
                    None => Value::Null,
                })
                .collect();
            Response::success(
                id,
                json!({
                    "surfaces": surfaces,
                    "agents": agents,
                    "requested": count,
                    "placed": surfaces.len(),
                }),
            )
        }
        Err(e) => backend_err(id, e),
    }
}

fn session_resume(backend: &dyn Backend, id: Value, params: &Value) -> Response {
    let sid = match params.get("id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return param_err(id, "session.resume requires `id` (session uuid)"),
    };
    let cwd = params.get("cwd").and_then(|v| v.as_str());
    let newroom = params.get("newroom").and_then(|v| v.as_bool()).unwrap_or(false);
    let attach = params.get("attach").and_then(|v| v.as_bool()).unwrap_or(false);
    // 하네스 미지정은 claude — 이 파라미터가 없던 시절의 호출을 그대로 받는다.
    let harness = params.get("harness").and_then(|v| v.as_str()).unwrap_or("claude");
    simple(id, backend.resume_session(sid, cwd, newroom, attach, harness))
}

fn session_recent(backend: &dyn Backend, id: Value, params: &Value) -> Response {
    let cwd = params.get("cwd").and_then(|v| v.as_str());
    match backend.recent_sessions(cwd) {
        Ok(list) => Response::success(id, json!({ "sessions": list })),
        Err(e) => backend_err(id, e),
    }
}

fn surface_send_text(backend: &dyn Backend, id: Value, params: &Value) -> Response {
    let text = match params.get("text").and_then(|v| v.as_str()) {
        Some(t) => t,
        None => return param_err(id, "surface.send_text requires `text` (string)"),
    };
    let target = params.get("surface_id").and_then(|v| v.as_str());
    // 학생→학생 tell 발신 기록 — CLI(tell)가 `from_pane`(발신 pane)+`plain`(제어시퀀스
    // 없는 본문)을 동봉하면 서버가 방 기준 slug 의 messages.jsonl 에 남긴다. 채팅뷰가
    // 이걸 ts+텍스트로 대조해 수신 transcript 의 user 턴을 발신 학생 버블로 그린다.
    // 발신 프로세스의 cwd 가 아닌 서버(방) 기준이라 cd 상태에 따라 기록 파일이 갈라지지
    // 않는다. 메타 없는 send_text(웹뷰 send 등)는 기존 그대로.
    if let (Some(from), Some(plain)) = (
        params.get("from_pane").and_then(|v| v.as_str()),
        params.get("plain").and_then(|v| v.as_str()),
    ) {
        if let Some(to) = target {
            // pane 발 tell(메타 동봉) 이 claude pane 을 겨누면 거부 — 지침(「SM 과
            // tell 을 같이 보내지 마라」, 2026-08-18)만으로는 학생들이 계속 겹쳐
            // 보냈다(2026-08-20 재발). 기계적으로 막아야 한 통만 남는다.
            let force = params.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
            if !force {
                if let Some(msg) = tell_into_claude_pane(backend, to) {
                    return param_err(id, &msg);
                }
            }
            log_agent_tell(backend, from, to, plain);
        }
    }
    if let Some(to) = target {
        if let Some(msg) = claude_boot_into_running_pane(backend, to, text) {
            return param_err(id, &msg);
        }
    }
    match backend.send_text(target, text) {
        Ok(()) => Response::success(id, json!({"ok": true})),
        Err(e) => backend_err(id, e),
    }
}

/// 이미 에이전트가 도는 pane 에 **부팅 커맨드**를 쏘는 것을 막는다.
///
/// 그 pane 의 에이전트는 셸이 아니라 자기 입력창에 그 문자열을 받아 지시로 읽어 버리고,
/// 브리프를 인박스에 미리 넣어 두는 스폰 절차(인박스 선주입 → pane 에서 claude 부팅)를
/// 그대로 쓰면 **아무도 뜨지 않은 이름의 인박스**가 하나 생겨 지시가 조용히 사라진다.
/// 도는 pane 에 말을 거는 정답은 인박스(SendMessage)이고, codex pane 은 tell 뿐이다.
///
/// 거부 사유 문자열을 돌려주고, 보내도 되면 `None`.
fn claude_boot_into_running_pane(backend: &dyn Backend, target: &str, text: &str) -> Option<String> {
    if !looks_like_claude_boot(text) {
        return None;
    }
    // board 에 있다 = transcript 가 도는 에이전트가 그 pane 에 있다. 비싼 조회라
    // 부팅 커맨드로 보일 때만 확인한다(대부분의 send 는 여기 오지 않는다).
    let row = backend
        .collab_board()
        .ok()?
        .into_iter()
        .find(|r| r.surface_id == target)?;
    // 줄이 있다는 것만으론 부족하다 — **셸만 있는 pane 도 줄을 갖는다**. 탭이 board 에
    // 들어온 뒤로는 갓 만든 빈 탭이 여기 걸려, 정작 학생을 띄우려는 부팅이 막혔다.
    // 막을 근거는 「이 pane 에 하네스가 실제로 잡혔다」 하나뿐이다.
    row.harness.as_deref()?;
    // codex 는 인박스가 없어 agent_name 이 영영 비므로, 하네스를 알 때는 그걸 먼저
    // 본다 — 안 그러면 닿지도 않는 SendMessage 를 답으로 알려주게 된다.
    let how = match (row.harness.as_deref(), &row.agent_name, &row.team) {
        (Some("codex"), _, _) => "codex 엔 인박스가 없다 — tell 로 보내라".to_string(),
        (_, Some(a), Some(_)) => format!("SendMessage 로 `to: \"{a}\"` 에 보내라"),
        _ => "SendMessage(같은 방 pane) 나 tell(그 밖) 로 보내라".to_string(),
    };
    let who = row.harness.as_deref().unwrap_or("에이전트");
    Some(format!(
        "{target} 에는 이미 {who} 가 돌고 있다 — 부팅 커맨드를 보내면 그 입력창에 \
         텍스트로 박힌다. 새 학생을 띄우려면 빈 pane 을 먼저 만들고, \
         이 pane 에 지시할 거라면 {how}."
    ))
}

/// pane 발 tell 이 **SendMessage 로 닿는 claude pane** 을 겨누면 거부 사유를 돌려준다.
///
/// tell 은 입력창 주입이라, SendMessage 와 겹쳐 보내면 받는 화면에 같은 말이 두 번
/// 뜨고 상대가 두 번 깨어난다. codex(인박스 없음)와 명부에 안 오른 claude(agent_name
/// 없음)는 tell 이 유일한 경로라 통과. 인박스가 실제로 죽은 비상시엔 `--force`.
fn tell_into_claude_pane(backend: &dyn Backend, target: &str) -> Option<String> {
    let row = backend
        .collab_board()
        .ok()?
        .into_iter()
        .find(|r| r.surface_id == target)?;
    if row.harness.as_deref() != Some("claude") {
        return None;
    }
    let agent = row.agent_name?;
    Some(format!(
        "{target} 의 claude 에는 SendMessage(to: \"{agent}\") 로 보내라 — tell 은 입력창 \
         주입이라 SM 과 겹치면 같은 말이 두 번 뜬다(둘 중 하나만, 기본은 SendMessage). \
         인박스가 죽어 SendMessage 가 정말 안 닿을 때만 tell --force."
    ))
}

/// 에이전트를 **띄우는** 명령처럼 보이는지. 좁게 잡는다 — "claude 가 왜 이래" 같은
/// 평범한 지시문이 걸리면 tell 이 막혀 더 나쁘다. 그래서 실행 형태(`cd … && claude`)
/// 이거나, 줄머리 `claude` + 런처 플래그가 붙은 경우만 본다.
///
/// codex 도 같은 함정이라 같이 본다 — 도는 codex pane 에 `cd … && codex` 를 쏘면
/// 그 codex 가 입력창에 그대로 받아 읽는다(claude 와 판박이).
fn looks_like_claude_boot(text: &str) -> bool {
    const FLAGS: [&str; 8] = [
        "--model",
        "--agent-id",
        "--agent-name",
        "--team-name",
        "--resume",
        "--effort",
        "--dangerously-skip-permissions",
        "--dangerously-bypass-hook-trust",
    ];
    text.lines().any(|line| {
        let l = line.trim_matches(|c: char| c.is_control() || c == '~' || c == '[').trim();
        let parts: Vec<&str> = l.split("&&").flat_map(|p| p.split(';')).map(str::trim).collect();
        let runs_agent = |p: &str| {
            ["claude", "codex"]
                .iter()
                .any(|a| p == *a || p.starts_with(&format!("{a} ")))
        };
        let bare = |p: &str| p == "claude" || p == "codex";
        // `cd … && claude …` 처럼 이어붙인 명령은 그 자체로 실행이다. 조각이 하나뿐이면
        // 사람이 쓴 문장일 수 있으니 런처 플래그가 붙었을 때만 본다.
        if parts.len() > 1 {
            return parts.iter().any(|p| runs_agent(p));
        }
        parts
            .first()
            .is_some_and(|p| runs_agent(p) && (bare(p) || FLAGS.iter().any(|f| p.contains(f))))
    })
}

/// tell 발신 이벤트를 messages.jsonl 에 append — http `persist_sensei_msg` 와 같은
/// 파일·형식(방이면 slug 에 `__room_<id>`)이되 from 은 발신 pane. 파일 IO 실패는
/// 조용히 삼킨다(기록은 표시용 부가 기능, tell 전달을 막으면 안 된다).
fn log_agent_tell(backend: &dyn Backend, from_pane: &str, to_pane: &str, text: &str) {
    let cwd = backend
        .active_cwd()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));
    let base: String = cwd
        .to_string_lossy()
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect();
    let slug = match backend.active_room() {
        Some(r) => format!("{base}__room_{r}"),
        None => base,
    };
    let dir = crate::collab_root().join(slug);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let id = format!("{:08x}", (now * 1000.0) as u64 & 0xffff_ffff);
    let line = json!({
        "id": id,
        "from": from_pane, "from_pane": from_pane,
        "to": to_pane, "to_pane": to_pane,
        "text": text, "ts": now, "read": true,
    })
    .to_string();
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("messages.jsonl"))
    {
        use std::io::Write;
        let _ = writeln!(f, "{line}");
    }
}

fn surface_send_key(backend: &dyn Backend, id: Value, params: &Value) -> Response {
    let key = match params.get("key").and_then(|v| v.as_str()) {
        Some(k) => k,
        None => return param_err(id, "surface.send_key requires `key` (string)"),
    };
    let target = params.get("surface_id").and_then(|v| v.as_str());
    match backend.send_key(target, key) {
        Ok(()) => Response::success(id, json!({"ok": true})),
        Err(e) => backend_err(id, e),
    }
}

/// Decode space-separated 2-digit hex (e.g. "1b 5b 41") into bytes — the wire
/// form used by send_raw, mirroring TmuxBackend::send_keys_hex so escapes and
/// control bytes survive the JSON-RPC text channel intact.
fn decode_hex(s: &str) -> Option<Vec<u8>> {
    s.split_whitespace()
        .map(|t| u8::from_str_radix(t, 16).ok())
        .collect()
}

fn surface_send_raw(backend: &dyn Backend, id: Value, params: &Value) -> Response {
    let hex = match params.get("hex").and_then(|v| v.as_str()) {
        Some(h) => h,
        None => return param_err(id, "surface.send_raw requires `hex` (space-separated hex bytes)"),
    };
    let bytes = match decode_hex(hex) {
        Some(b) => b,
        None => return param_err(id, "surface.send_raw: `hex` must be space-separated 2-digit hex"),
    };
    let target = params.get("surface_id").and_then(|v| v.as_str());
    match backend.send_raw(target, &bytes) {
        Ok(()) => Response::success(id, json!({"ok": true})),
        Err(e) => backend_err(id, e),
    }
}

fn surface_resize(backend: &dyn Backend, id: Value, params: &Value) -> Response {
    let surface_id = match params.get("surface_id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return param_err(id, "surface.resize requires `surface_id` (string)"),
    };
    let cols = match params.get("cols").and_then(|v| v.as_u64()) {
        Some(c) => c as u16,
        None => return param_err(id, "surface.resize requires `cols` (number)"),
    };
    let rows = match params.get("rows").and_then(|v| v.as_u64()) {
        Some(r) => r as u16,
        None => return param_err(id, "surface.resize requires `rows` (number)"),
    };
    match backend.resize_surface(surface_id, cols, rows) {
        Ok(()) => Response::success(id, json!({"ok": true})),
        Err(e) => backend_err(id, e),
    }
}

fn surface_scroll(backend: &dyn Backend, id: Value, params: &Value) -> Response {
    let surface_id = match params.get("surface_id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return param_err(id, "surface.scroll requires `surface_id` (string)"),
    };
    let lines = match params.get("lines").and_then(|v| v.as_i64()) {
        Some(l) => l as i32,
        None => return param_err(id, "surface.scroll requires `lines` (number)"),
    };
    match backend.scroll_surface(surface_id, lines) {
        Ok(()) => Response::success(id, json!({"ok": true})),
        Err(e) => backend_err(id, e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{Backend, SurfaceInfo, WorkspaceInfo};
    use std::sync::Mutex;

    /// Fake backend that records calls and returns canned answers.
    /// Single-pane behavior matches what kasaterm-sugarloaf-cli will
    /// expose in its first Backend impl.
    #[derive(Default)]
    struct FakeBackend {
        sent_text: Mutex<Vec<(Option<String>, String)>>,
        sent_keys: Mutex<Vec<(Option<String>, String)>>,
        resized: Mutex<Vec<(Vec<u8>, f32)>>,
        // tell 기록(log_agent_tell)의 slug 소스 — 테스트가 스크래치 경로를 지정해
        // 실제 방 slug 를 오염시키지 않게 한다. None 이면 trait 기본(None)과 동일.
        cwd: Option<std::path::PathBuf>,
        // claude 가 도는 pane 들 — 부팅 커맨드 가드가 이걸 보고 판정한다.
        board: Vec<crate::backend::PaneActivity>,
        // 완료 보고 기록 — surface.done 이 outcome 검증을 통과했을 때만 쌓인다.
        done: Mutex<Vec<(String, String, String)>>,
    }

    impl Backend for FakeBackend {
        fn active_cwd(&self) -> Option<std::path::PathBuf> {
            self.cwd.clone()
        }
        fn list_workspaces(&self) -> anyhow::Result<Vec<WorkspaceInfo>> {
            Ok(vec![WorkspaceInfo {
                id: "ws-1".into(),
                name: "main".into(),
            }])
        }
        fn current_workspace(&self) -> anyhow::Result<Option<WorkspaceInfo>> {
            Ok(Some(WorkspaceInfo {
                id: "ws-1".into(),
                name: "main".into(),
            }))
        }
        fn list_surfaces(&self) -> anyhow::Result<Vec<SurfaceInfo>> {
            Ok(vec![SurfaceInfo {
                id: "surf-1".into(),
                workspace_id: "ws-1".into(),
                title: None,
                cwd: None,
                character: None,
            }])
        }
        fn focus_surface(&self, _surface_id: &str) -> anyhow::Result<()> {
            Ok(())
        }
        fn collab_board(&self) -> anyhow::Result<Vec<crate::backend::PaneActivity>> {
            Ok(self.board.clone())
        }
        fn split_surface(
            &self,
            _direction: SplitDirection,
            _focus: bool,
            _from: Option<&str>,
        ) -> anyhow::Result<SurfaceInfo> {
            Ok(SurfaceInfo {
                id: "surf-2".into(),
                workspace_id: "ws-1".into(),
                title: None,
                cwd: None,
                character: None,
            })
        }
        fn send_text(&self, surface: Option<&str>, text: &str) -> anyhow::Result<()> {
            self.sent_text
                .lock()
                .unwrap()
                .push((surface.map(String::from), text.into()));
            Ok(())
        }
        fn send_key(&self, surface: Option<&str>, key: &str) -> anyhow::Result<()> {
            self.sent_keys
                .lock()
                .unwrap()
                .push((surface.map(String::from), key.into()));
            Ok(())
        }
        fn resize_divider(&self, path: &[u8], ratio: f32) -> anyhow::Result<()> {
            self.resized.lock().unwrap().push((path.to_vec(), ratio));
            Ok(())
        }
        fn pane_done(&self, surface_id: &str, outcome: &str, summary: &str) -> anyhow::Result<()> {
            self.done.lock().unwrap().push((
                surface_id.to_string(),
                outcome.to_string(),
                summary.to_string(),
            ));
            Ok(())
        }
    }

    fn req(method: &str, params: Value) -> Request {
        Request { id: json!("test-id"), method: method.into(), params }
    }

    #[test]
    fn ping_returns_pong_without_touching_backend() {
        let backend = FakeBackend::default();
        let r = dispatch(&backend, req("system.ping", json!({})));
        assert!(r.ok);
        assert_eq!(r.result.unwrap(), json!({"pong": true}));
    }

    #[test]
    fn capabilities_lists_implemented_methods() {
        let backend = FakeBackend::default();
        let r = dispatch(&backend, req("system.capabilities", json!({})));
        let methods = r.result.unwrap()["methods"].as_array().unwrap().clone();
        assert!(methods.iter().any(|m| m == "surface.split"));
        assert!(methods.iter().any(|m| m == "system.ping"));
        assert!(methods.iter().any(|m| m == "surface.resize_divider"));
    }

    #[test]
    fn resize_divider_forwards_path_and_ratio() {
        let backend = FakeBackend::default();
        let r = dispatch(
            &backend,
            req("surface.resize_divider", json!({"path": [1, 0], "ratio": 0.3})),
        );
        assert!(r.ok);
        let calls = backend.resized.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, vec![1u8, 0]);
        assert!((calls[0].1 - 0.3).abs() < 1e-6);
    }

    #[test]
    fn resize_divider_requires_path_and_ratio() {
        let backend = FakeBackend::default();
        let no_path = dispatch(&backend, req("surface.resize_divider", json!({"ratio": 0.3})));
        assert!(!no_path.ok);
        let no_ratio = dispatch(&backend, req("surface.resize_divider", json!({"path": [0]})));
        assert!(!no_ratio.ok);
        // A malformed call must never reach the backend.
        assert!(backend.resized.lock().unwrap().is_empty());
    }

    #[test]
    fn unknown_method_returns_method_not_found() {
        let backend = FakeBackend::default();
        let r = dispatch(&backend, req("nope.does_not_exist", json!({})));
        assert!(!r.ok);
        assert_eq!(r.error.unwrap().code, codes::METHOD_NOT_FOUND);
    }

    #[test]
    fn split_validates_direction_value() {
        let backend = FakeBackend::default();
        let r = dispatch(&backend, req("surface.split", json!({"direction": "sideways"})));
        assert!(!r.ok);
        assert_eq!(r.error.unwrap().code, codes::INVALID_PARAMS);
    }

    #[test]
    fn split_with_valid_direction_returns_new_surface() {
        let backend = FakeBackend::default();
        let r = dispatch(&backend, req("surface.split", json!({"direction": "right"})));
        assert!(r.ok);
        assert_eq!(
            r.result.unwrap()["surface"]["id"],
            json!("surf-2"),
        );
    }

    #[test]
    fn send_text_targets_specific_surface_when_provided() {
        let backend = FakeBackend::default();
        let r = dispatch(
            &backend,
            req(
                "surface.send_text",
                json!({"surface_id": "surf-1", "text": "echo hi\n"}),
            ),
        );
        assert!(r.ok);
        let sent = backend.sent_text.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0.as_deref(), Some("surf-1"));
        assert_eq!(sent[0].1, "echo hi\n");
    }

    #[test]
    fn send_text_defaults_to_focused_when_no_surface_id() {
        let backend = FakeBackend::default();
        let r = dispatch(&backend, req("surface.send_text", json!({"text": "x"})));
        assert!(r.ok);
        let sent = backend.sent_text.lock().unwrap();
        assert_eq!(sent[0].0, None, "absent surface_id means focused pane");
    }

    #[test]
    fn send_text_rejects_missing_text_field() {
        let backend = FakeBackend::default();
        let r = dispatch(&backend, req("surface.send_text", json!({})));
        assert!(!r.ok);
        assert_eq!(r.error.unwrap().code, codes::INVALID_PARAMS);
    }

    #[test]
    fn send_text_with_tell_meta_logs_to_room_messages() {
        // slug 는 cwd 문자열 변환이라 유니크 cwd 로 실제 방 파일과 격리.
        let fake_cwd =
            std::env::temp_dir().join(format!("kasa-tell-test-{}", std::process::id()));
        let backend = FakeBackend { cwd: Some(fake_cwd.clone()), ..Default::default() };
        let r = dispatch(
            &backend,
            req(
                "surface.send_text",
                json!({"surface_id": "surf-1", "from_pane": "%9",
                       "plain": "안녕 유즈", "text": "\u{15}\u{1b}[200~안녕 유즈\u{1b}[201~\r"}),
            ),
        );
        assert!(r.ok);
        // PTY 로는 wrapper 포함 원문이 그대로 간다 — 기록이 전달을 바꾸면 안 된다.
        assert_eq!(backend.sent_text.lock().unwrap().len(), 1);
        let slug: String = fake_cwd
            .to_string_lossy()
            .chars()
            .map(|c| if c == '/' || c == '.' { '-' } else { c })
            .collect();
        let path = crate::collab_root().join(&slug).join("messages.jsonl");
        let content = std::fs::read_to_string(&path).expect("tell meta must be logged");
        let entry: Value = serde_json::from_str(content.lines().last().unwrap()).unwrap();
        assert_eq!(entry["from_pane"], "%9");
        assert_eq!(entry["to_pane"], "surf-1");
        assert_eq!(entry["text"], "안녕 유즈", "제어시퀀스 없는 plain 본문만 기록");
        assert!(entry["ts"].as_f64().unwrap() > 0.0);
        let _ = std::fs::remove_dir_all(crate::collab_root().join(&slug));
    }

    #[test]
    fn send_text_without_tell_meta_logs_nothing() {
        let fake_cwd =
            std::env::temp_dir().join(format!("kasa-tell-none-{}", std::process::id()));
        let backend = FakeBackend { cwd: Some(fake_cwd.clone()), ..Default::default() };
        let r = dispatch(
            &backend,
            req("surface.send_text", json!({"surface_id": "surf-1", "text": "plain send"})),
        );
        assert!(r.ok);
        let slug: String = fake_cwd
            .to_string_lossy()
            .chars()
            .map(|c| if c == '/' || c == '.' { '-' } else { c })
            .collect();
        let path = crate::collab_root().join(&slug).join("messages.jsonl");
        assert!(!path.exists(), "메타 없는 send_text 는 기록을 남기지 않는다");
    }

    #[test]
    fn claude_boot_signature_is_narrow() {
        // 실행 형태 — 막아야 한다.
        assert!(looks_like_claude_boot("cd /repo && claude"));
        assert!(looks_like_claude_boot("cd /repo && claude --model 'claude-opus-5[1m]'"));
        assert!(looks_like_claude_boot("claude --resume abc123"));
        assert!(looks_like_claude_boot("claude"));
        // 사람이 쓴 지시문 — tell 이 막히면 안 된다.
        assert!(!looks_like_claude_boot("claude 코드 좀 봐줘"));
        assert!(!looks_like_claude_boot("claude 가 왜 이래?"));
        assert!(!looks_like_claude_boot("이거 claude --model 로 띄웠었나?"));
    }

    /// codex 도 같은 함정이다 — 도는 codex pane 에 부팅 커맨드를 쏘면 그 codex 의
    /// 입력창에 텍스트로 박힌다. claude 판정만 있던 시절엔 그냥 통과했다.
    #[test]
    fn codex_boot_is_caught_too() {
        assert!(looks_like_claude_boot("cd /repo && codex"));
        assert!(looks_like_claude_boot("codex --dangerously-bypass-hook-trust"));
        assert!(looks_like_claude_boot("codex"));
        // 좁기는 claude 와 똑같이 — 사람이 쓴 문장은 통과시킨다.
        assert!(!looks_like_claude_boot("codex 가 왜 이래?"));
        assert!(!looks_like_claude_boot("codex 로 한번 띄워봐"));
    }

    /// codex pane 은 인박스가 없어 `agent_name` 이 영영 빈다 — 그때 SendMessage 를
    /// 답으로 알려주면 부른 쪽이 닿지도 않는 곳에 쏘고 기다린다.
    #[test]
    fn codex_pane_is_told_to_use_tell_not_sendmessage() {
        let backend = FakeBackend {
            board: vec![crate::backend::PaneActivity {
                surface_id: "surf-9".into(),
                harness: Some("codex".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let r = dispatch(
            &backend,
            req(
                "surface.send_text",
                json!({"surface_id": "surf-9", "text": "cd /repo && codex\n"}),
            ),
        );
        assert!(!r.ok);
        let msg = r.error.unwrap().message;
        assert!(msg.contains("tell"), "tell 을 답으로 줘야 한다: {msg}");
        assert!(!msg.contains("SendMessage"), "codex 엔 인박스가 없다: {msg}");
        assert!(msg.contains("codex"), "무엇이 돌고 있는지 밝혀야 한다: {msg}");
        assert!(backend.sent_text.lock().unwrap().is_empty(), "거부했으면 보내지 않는다");
    }

    /// 학생이 SM 과 tell 을 겹쳐 보내는 이중 발송 — 지침은 두 번 어겨졌으니(08-18,
    /// 08-20) 서버가 막는다. SendMessage 로 닿는 claude pane 이 과녁이면 거부하고
    /// 정답(agent 이름)을 알려준다.
    #[test]
    fn tell_into_claude_pane_is_refused_with_sendmessage_answer() {
        let backend = FakeBackend {
            board: vec![crate::backend::PaneActivity {
                surface_id: "surf-3".into(),
                harness: Some("claude".into()),
                agent_name: Some("midori-p4-v32".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let r = dispatch(
            &backend,
            req(
                "surface.send_text",
                json!({"surface_id": "surf-3", "from_pane": "%9",
                       "plain": "판독 끝", "text": "\u{15}\u{1b}[200~판독 끝\u{1b}[201~\r"}),
            ),
        );
        assert!(!r.ok);
        let msg = r.error.unwrap().message;
        assert!(msg.contains("SendMessage"), "정답 경로를 알려줘야 한다: {msg}");
        assert!(msg.contains("midori-p4-v32"), "to 에 넣을 이름까지 줘야 한다: {msg}");
        assert!(backend.sent_text.lock().unwrap().is_empty(), "거부했으면 보내지 않는다");
    }

    /// 인박스가 죽은 비상시의 탈출구 — force 가 오면 같은 과녁이라도 통과한다.
    #[test]
    fn tell_force_overrides_claude_pane_guard() {
        let fake_cwd =
            std::env::temp_dir().join(format!("kasa-tell-force-{}", std::process::id()));
        let backend = FakeBackend {
            cwd: Some(fake_cwd.clone()),
            board: vec![crate::backend::PaneActivity {
                surface_id: "surf-3".into(),
                harness: Some("claude".into()),
                agent_name: Some("midori-p4-v32".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let r = dispatch(
            &backend,
            req(
                "surface.send_text",
                json!({"surface_id": "surf-3", "from_pane": "%9", "force": true,
                       "plain": "비상", "text": "비상\r"}),
            ),
        );
        assert!(r.ok);
        assert_eq!(backend.sent_text.lock().unwrap().len(), 1);
        let slug: String = fake_cwd
            .to_string_lossy()
            .chars()
            .map(|c| if c == '/' || c == '.' { '-' } else { c })
            .collect();
        let _ = std::fs::remove_dir_all(crate::collab_root().join(&slug));
    }

    /// tell 이 유일한 경로인 곳은 그대로 열려 있어야 한다 — codex pane 과,
    /// 명부에 안 오른 claude(agent_name 없음).
    #[test]
    fn tell_still_reaches_codex_and_unlisted_claude() {
        let fake_cwd =
            std::env::temp_dir().join(format!("kasa-tell-open-{}", std::process::id()));
        let backend = FakeBackend {
            cwd: Some(fake_cwd.clone()),
            board: vec![
                crate::backend::PaneActivity {
                    surface_id: "surf-9".into(),
                    harness: Some("codex".into()),
                    ..Default::default()
                },
                crate::backend::PaneActivity {
                    surface_id: "surf-10".into(),
                    harness: Some("claude".into()),
                    agent_name: None,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        for surf in ["surf-9", "surf-10"] {
            let r = dispatch(
                &backend,
                req(
                    "surface.send_text",
                    json!({"surface_id": surf, "from_pane": "%9",
                           "plain": "이어서", "text": "이어서\r"}),
                ),
            );
            assert!(r.ok, "{surf} 는 tell 이 유일한 경로다");
        }
        assert_eq!(backend.sent_text.lock().unwrap().len(), 2);
        let slug: String = fake_cwd
            .to_string_lossy()
            .chars()
            .map(|c| if c == '/' || c == '.' { '-' } else { c })
            .collect();
        let _ = std::fs::remove_dir_all(crate::collab_root().join(&slug));
    }

    /// done 의 outcome 은 두 값뿐 — status 칸에서 겪은 "free text 라더니 소비부는
    /// 정확 일치" 함정을 서버 입구에서 막는다. 통과한 보고만 backend 에 닿는다.
    #[test]
    fn surface_done_gates_outcome_to_two_values() {
        let backend = FakeBackend::default();
        let r = dispatch(
            &backend,
            req(
                "surface.done",
                json!({"surface_id": "surf-1", "outcome": "거의 다 됨", "summary": "x"}),
            ),
        );
        assert!(!r.ok);
        assert!(r.error.unwrap().message.contains("succeeded"), "고칠 값을 알려줘야 한다");
        assert!(backend.done.lock().unwrap().is_empty(), "거부했으면 기록하지 않는다");

        let r = dispatch(
            &backend,
            req(
                "surface.done",
                json!({"surface_id": "surf-1", "outcome": "failed", "summary": "빌드 깨짐"}),
            ),
        );
        assert!(r.ok);
        assert_eq!(
            backend.done.lock().unwrap().as_slice(),
            &[("surf-1".to_string(), "failed".to_string(), "빌드 깨짐".to_string())]
        );
    }

    #[test]
    fn boot_command_into_running_pane_is_refused() {
        let backend = FakeBackend {
            board: vec![crate::backend::PaneActivity {
                surface_id: "surf-1".into(),
                agent_name: Some("prana-p5".into()),
                team: Some("kt-x".into()),
                // 막는 근거는 「이 pane 에 하네스가 실제로 잡혔다」 하나다(a9acb69) —
                // 줄이 있다는 것만으론 셸뿐인 빈 탭까지 막혀 정작 학생을 못 띄웠다.
                harness: Some("claude".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let r = dispatch(
            &backend,
            req(
                "surface.send_text",
                json!({"surface_id": "surf-1", "text": "cd /repo && claude --model opus\n"}),
            ),
        );
        assert!(!r.ok);
        // 무엇을 대신 쓸지까지 알려줘야 부른 쪽이 고칠 수 있다.
        assert!(r.error.unwrap().message.contains("prana-p5"));
        assert!(backend.sent_text.lock().unwrap().is_empty(), "거부했으면 보내지 않는다");
    }

    #[test]
    fn boot_command_into_shell_only_pane_passes() {
        // board 에 없다 = claude 가 아직 없다. 새 학생을 띄우는 정상 경로다.
        let backend = FakeBackend::default();
        let r = dispatch(
            &backend,
            req(
                "surface.send_text",
                json!({"surface_id": "surf-9", "text": "cd /repo && claude --model opus\n"}),
            ),
        );
        assert!(r.ok);
        assert_eq!(backend.sent_text.lock().unwrap().len(), 1);
    }

    #[test]
    fn focus_rejects_missing_surface_id() {
        let backend = FakeBackend::default();
        let r = dispatch(&backend, req("surface.focus", json!({})));
        assert!(!r.ok);
        assert_eq!(r.error.unwrap().code, codes::INVALID_PARAMS);
    }
}
