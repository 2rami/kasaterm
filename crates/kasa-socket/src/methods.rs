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
        "surface.report_cwd" => surface_report_cwd(backend, id, &req.params),
        "surface.swap" => surface_swap(backend, id, &req.params),
        "surface.move" => surface_move(backend, id, &req.params),
        "surface.resize_divider" => surface_resize_divider(backend, id, &req.params),
        "surface.set_ratio" => surface_set_ratio(backend, id, &req.params),
        "surface.peek" => surface_peek(backend, id, &req.params),
        "surface.open_preview" => surface_open_preview(backend, id, &req.params),
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
                "surface.send_text",
                "surface.send_key",
                "surface.send_raw",
                "surface.resize",
                "surface.scroll",
                "surface.peek",
                "surface.open_preview",
                "surface.dock",
                "surface.undock",
                "surface.move",
                "surface.rename",
                "window.rename",
                "surface.set_color",
                "surface.resize_divider",
                "surface.set_ratio",
                "collab.board",
                "window.layout",
                "window.list",
                "collab.bind_transcript",
                "collab.transcript",
                "surface.notify",
                "surface.attention",
            ],
        }),
    )
}

fn surface_open_preview(backend: &dyn Backend, id: Value, params: &Value) -> Response {
    let kind = match params.get("kind").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return param_err(id, "surface.open_preview requires `kind` (image|markdown)"),
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
    match backend.report_cwd(surface_id, cwd, session_id) {
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
        Some(other) => {
            return param_err(
                id,
                format!("surface.split: direction must be left/right/up/down, got {other:?}"),
            )
        }
        None => {
            return param_err(
                id,
                "surface.split requires `direction` (left/right/up/down)",
            )
        }
    };
    // 기본 no-focus (자동화 경로): `focus:true` 를 명시할 때만 새 pane 으로 포커스
    // 이동. CLI 의 `--focus` 플래그가 이 값을 채운다.
    let focus = params.get("focus").and_then(|v| v.as_bool()).unwrap_or(false);
    match backend.split_surface(dir, focus) {
        Ok(s) => Response::success(id, json!({"surface": s})),
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
    simple(id, backend.resume_session(sid, cwd, newroom, attach))
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
            log_agent_tell(backend, from, to, plain);
        }
    }
    match backend.send_text(target, text) {
        Ok(()) => Response::success(id, json!({"ok": true})),
        Err(e) => backend_err(id, e),
    }
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
    let dir = std::path::Path::new("/tmp/kasaterm-collab").join(slug);
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
            }])
        }
        fn focus_surface(&self, _surface_id: &str) -> anyhow::Result<()> {
            Ok(())
        }
        fn split_surface(&self, _direction: SplitDirection, _focus: bool) -> anyhow::Result<SurfaceInfo> {
            Ok(SurfaceInfo {
                id: "surf-2".into(),
                workspace_id: "ws-1".into(),
                title: None,
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
        let path = std::path::Path::new("/tmp/kasaterm-collab").join(&slug).join("messages.jsonl");
        let content = std::fs::read_to_string(&path).expect("tell meta must be logged");
        let entry: Value = serde_json::from_str(content.lines().last().unwrap()).unwrap();
        assert_eq!(entry["from_pane"], "%9");
        assert_eq!(entry["to_pane"], "surf-1");
        assert_eq!(entry["text"], "안녕 유즈", "제어시퀀스 없는 plain 본문만 기록");
        assert!(entry["ts"].as_f64().unwrap() > 0.0);
        let _ = std::fs::remove_dir_all(std::path::Path::new("/tmp/kasaterm-collab").join(&slug));
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
        let path = std::path::Path::new("/tmp/kasaterm-collab").join(&slug).join("messages.jsonl");
        assert!(!path.exists(), "메타 없는 send_text 는 기록을 남기지 않는다");
    }

    #[test]
    fn focus_rejects_missing_surface_id() {
        let backend = FakeBackend::default();
        let r = dispatch(&backend, req("surface.focus", json!({})));
        assert!(!r.ok);
        assert_eq!(r.error.unwrap().code, codes::INVALID_PARAMS);
    }
}
