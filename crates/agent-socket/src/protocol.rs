//! Wire types for the cmux-compatible JSON-RPC protocol.
//!
//! Every frame is exactly one JSON object followed by `\n`. The `id`
//! field on a request is opaque to us — we just echo it back on the
//! matching response so the client can match async replies to its
//! originating call. cmux's official docs use short string IDs ("req-1",
//! "split-3"); we accept any JSON value to stay compatible with clients
//! that prefer numbers or UUIDs.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One client-to-server JSON-RPC call. `method` is dot-namespaced —
/// `workspace.list`, `surface.split`, etc. `params` is method-specific;
/// when a method takes no arguments cmux still sends `{}` rather than
/// omitting the field, so we deserialize it as a `Value` and let each
/// method handler pick out what it needs.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Request {
    pub id: Value,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// One server-to-client response. Either `ok: true` with `result`, or
/// `ok: false` with `error`. Exactly one of the two latter fields is
/// populated; we keep both `Option` so a single struct can describe the
/// wire format without an untagged enum (untagged enums break round-trip
/// when the success and error shapes overlap at the type level).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub id: Value,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorObj>,
}

impl Response {
    pub fn success(id: Value, result: Value) -> Self {
        Self { id, ok: true, result: Some(result), error: None }
    }

    pub fn error(id: Value, code: i32, message: impl Into<String>) -> Self {
        Self {
            id,
            ok: false,
            result: None,
            error: Some(ErrorObj { code, message: message.into() }),
        }
    }
}

/// Error payload mirroring cmux's error shape. `code` follows the
/// JSON-RPC 2.0 convention loosely — -32601 for "method not found",
/// -32602 for bad params, -32603 for backend failure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorObj {
    pub code: i32,
    pub message: String,
}

/// Standard error codes used by the dispatcher. Method handlers can
/// return their own codes but should stay outside this range to avoid
/// colliding with framework-level errors.
pub mod codes {
    pub const PARSE_ERROR: i32 = -32700;
    pub const INVALID_REQUEST: i32 = -32600;
    pub const METHOD_NOT_FOUND: i32 = -32601;
    pub const INVALID_PARAMS: i32 = -32602;
    pub const BACKEND_ERROR: i32 = -32603;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_success_response_preserves_id() {
        // The id is opaque — we accept whatever JSON value the client
        // sent and echo it back. Verify a string id, a number id, and a
        // null id all survive the round trip.
        let cases = [
            serde_json::json!("req-1"),
            serde_json::json!(42),
            serde_json::json!(null),
        ];
        for id in cases {
            let resp = Response::success(id.clone(), serde_json::json!({"pong": true}));
            let line = serde_json::to_string(&resp).unwrap();
            let parsed: Response = serde_json::from_str(&line).unwrap();
            assert_eq!(parsed.id, id);
            assert!(parsed.ok);
            assert_eq!(parsed.result, Some(serde_json::json!({"pong": true})));
            assert!(parsed.error.is_none());
        }
    }

    #[test]
    fn error_response_omits_result_field_on_wire() {
        let resp = Response::error(
            serde_json::json!("req-1"),
            codes::METHOD_NOT_FOUND,
            "unknown method",
        );
        let line = serde_json::to_string(&resp).unwrap();
        assert!(!line.contains("\"result\""), "result must be skipped: {line}");
        assert!(line.contains("\"error\""));
        assert!(line.contains("-32601"));
    }

    #[test]
    fn request_with_missing_params_defaults_to_null() {
        // cmux clients sometimes send `{"id":"x","method":"system.ping"}`
        // with no `params` field. We treat absent params as `Value::Null`
        // rather than failing the parse, so trivial methods don't need to
        // demand a redundant `"params": {}`.
        let line = r#"{"id":"x","method":"system.ping"}"#;
        let req: Request = serde_json::from_str(line).unwrap();
        assert_eq!(req.method, "system.ping");
        assert_eq!(req.params, Value::Null);
    }
}
