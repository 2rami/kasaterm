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
        "surface.close" => surface_close(backend, id, &req.params),
        "surface.rename" => surface_rename(backend, id, &req.params),
        "surface.set_color" => surface_set_color(backend, id, &req.params),
        "surface.swap" => surface_swap(backend, id, &req.params),
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
            ],
        }),
    )
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
    match backend.split_surface(dir) {
        Ok(s) => Response::success(id, json!({"surface": s})),
        Err(e) => backend_err(id, e),
    }
}

fn surface_send_text(backend: &dyn Backend, id: Value, params: &Value) -> Response {
    let text = match params.get("text").and_then(|v| v.as_str()) {
        Some(t) => t,
        None => return param_err(id, "surface.send_text requires `text` (string)"),
    };
    let target = params.get("surface_id").and_then(|v| v.as_str());
    match backend.send_text(target, text) {
        Ok(()) => Response::success(id, json!({"ok": true})),
        Err(e) => backend_err(id, e),
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
    }

    impl Backend for FakeBackend {
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
        fn split_surface(&self, _direction: SplitDirection) -> anyhow::Result<SurfaceInfo> {
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
    fn focus_rejects_missing_surface_id() {
        let backend = FakeBackend::default();
        let r = dispatch(&backend, req("surface.focus", json!({})));
        assert!(!r.ok);
        assert_eq!(r.error.unwrap().code, codes::INVALID_PARAMS);
    }
}
