use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;

const BODY_LIMIT: usize = 24_000;
const COOLDOWN: Duration = Duration::from_secs(60);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Submission {
    body: String,
    #[serde(default)]
    diagnostics: String,
}

impl Submission {
    fn validate(&self) -> bool {
        !self.body.trim().is_empty()
            && self.body.len() <= 16_000
            && self.diagnostics.len() <= 2_000
            && !self.body.contains('\0')
            && !self.diagnostics.contains('\0')
    }

    fn document(&self) -> String {
        if self.diagnostics.trim().is_empty() {
            self.body.trim().to_string()
        } else {
            format!("{}\n\n---\n{}", self.body.trim(), self.diagnostics.trim())
        }
    }
}

struct Sink {
    token: String,
    recipient: String,
    client: reqwest::Client,
}

#[derive(Clone)]
struct FeedbackState {
    sink: Option<Arc<Sink>>,
    recent: Arc<Mutex<HashMap<String, Instant>>>,
}

pub fn router() -> Router {
    let source = std::env::var_os("KASATERM_FEEDBACK_ENV_FILE")
        .and_then(|path| std::fs::read_to_string(path).ok())
        .unwrap_or_default();
    let token_key = std::env::var("KASATERM_FEEDBACK_TOKEN_KEY")
        .unwrap_or_else(|_| "KASATERM_FEEDBACK_DISCORD_TOKEN".into());
    let user_key = std::env::var("KASATERM_FEEDBACK_USER_KEY")
        .unwrap_or_else(|_| "KASATERM_FEEDBACK_DISCORD_USER".into());
    let token = std::env::var("KASATERM_FEEDBACK_DISCORD_TOKEN")
        .unwrap_or_else(|_| literal_env_value(&source, &token_key).unwrap_or_default());
    let recipient = std::env::var("KASATERM_FEEDBACK_DISCORD_USER")
        .unwrap_or_else(|_| literal_env_value(&source, &user_key).unwrap_or_default());
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build();
    let sink = client
        .ok()
        .filter(|_| !token.is_empty() && valid_recipient(&recipient))
        .map(|client| {
            Arc::new(Sink {
                token,
                recipient,
                client,
            })
        });
    Router::new()
        .route("/feedback", post(receive))
        .with_state(FeedbackState {
            sink,
            recent: Arc::new(Mutex::new(HashMap::new())),
        })
}

fn literal_env_value(source: &str, key: &str) -> Option<String> {
    source.lines().find_map(|line| {
        let line = line.trim().strip_prefix("export ").unwrap_or(line.trim());
        let (name, value) = line.split_once('=')?;
        if name.trim() != key {
            return None;
        }
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
            .unwrap_or(value);
        // Credential files are data, never a shell program.
        (!value.is_empty()
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)))
        .then(|| value.to_string())
    })
}

fn valid_recipient(value: &str) -> bool {
    (16..=22).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_digit())
}

fn response(status: StatusCode, code: &str) -> Response {
    (
        status,
        Json(json!({"ok": status.is_success(), "error": code})),
    )
        .into_response()
}

fn source(req: &Request) -> String {
    let peer = req
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>();
    match peer {
        Some(peer) if !peer.0.ip().is_loopback() => peer.0.ip().to_string(),
        _ => req
            .headers()
            .get("cf-connecting-ip")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<std::net::IpAddr>().ok())
            .map(|ip| ip.to_string())
            .unwrap_or_else(|| "local".to_string()),
    }
}

async fn receive(State(state): State<FeedbackState>, req: Request) -> Response {
    if req
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        != Some("application/json")
    {
        return response(StatusCode::UNSUPPORTED_MEDIA_TYPE, "json_required");
    }
    let Some(sink) = state.sink else {
        return response(StatusCode::SERVICE_UNAVAILABLE, "feedback_unavailable");
    };
    let key = source(&req);
    let bytes = match axum::body::to_bytes(req.into_body(), BODY_LIMIT).await {
        Ok(bytes) => bytes,
        Err(_) => return response(StatusCode::PAYLOAD_TOO_LARGE, "feedback_too_large"),
    };
    let submission: Submission = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => return response(StatusCode::BAD_REQUEST, "invalid_feedback"),
    };
    if !submission.validate() {
        return response(StatusCode::BAD_REQUEST, "invalid_feedback");
    }
    {
        let mut recent = state.recent.lock().unwrap_or_else(|e| e.into_inner());
        recent.retain(|_, at| at.elapsed() < COOLDOWN);
        // Both limits apply before delivery so a public form cannot flood the owner's inbox.
        if recent.contains_key(&key) || recent.len() >= 30 {
            return response(StatusCode::TOO_MANY_REQUESTS, "feedback_rate_limited");
        }
        recent.insert(key, Instant::now());
    }
    match deliver(&sink, &submission).await {
        Ok(()) => (StatusCode::OK, Json(json!({"ok": true}))).into_response(),
        Err(()) => response(StatusCode::BAD_GATEWAY, "feedback_delivery_failed"),
    }
}

fn multipart(submission: &Submission, boundary: &str) -> Vec<u8> {
    let excerpt: String = submission.body.chars().take(500).collect();
    let payload = json!({
        "content": format!("kasaterm 피드백\n\n{excerpt}"),
        "allowed_mentions": {"parse": []},
        "attachments": [{"id": 0, "filename": "feedback.txt"}],
    });
    format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"payload_json\"\r\nContent-Type: application/json\r\n\r\n{payload}\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"files[0]\"; filename=\"feedback.txt\"\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{}\r\n--{boundary}--\r\n",
        submission.document(),
    ).into_bytes()
}

async fn deliver(sink: &Sink, submission: &Submission) -> Result<(), ()> {
    let authorization = format!("Bot {}", sink.token);
    let channel = sink
        .client
        .post("https://discord.com/api/v10/users/@me/channels")
        .header(reqwest::header::AUTHORIZATION, &authorization)
        .json(&json!({"recipient_id": sink.recipient}))
        .send()
        .await
        .map_err(|_| ())?
        .error_for_status()
        .map_err(|_| ())?
        .json::<serde_json::Value>()
        .await
        .map_err(|_| ())?;
    let channel = channel["id"]
        .as_str()
        .filter(|id| valid_recipient(id))
        .ok_or(())?;
    let boundary = format!("kasaterm-feedback-{}", uuid::Uuid::new_v4());
    sink.client
        .post(format!(
            "https://discord.com/api/v10/channels/{channel}/messages"
        ))
        .header(reqwest::header::AUTHORIZATION, &authorization)
        .header(
            reqwest::header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(multipart(submission, &boundary))
        .send()
        .await
        .map_err(|_| ())?
        .error_for_status()
        .map_err(|_| ())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_file_reads_only_literal_selected_keys() {
        let source =
            "OTHER=ignore\nexport TOKEN='abc.def_ghi'\nUSER=123456789012345678\nBAD=$(command)";
        assert_eq!(
            literal_env_value(source, "TOKEN").as_deref(),
            Some("abc.def_ghi")
        );
        assert_eq!(
            literal_env_value(source, "USER").as_deref(),
            Some("123456789012345678")
        );
        assert_eq!(literal_env_value(source, "BAD"), None);
        assert_eq!(literal_env_value(source, "MISSING"), None);
    }

    #[test]
    fn rejects_empty_oversized_and_control_payloads() {
        for body in [
            " \n".to_string(),
            "x".repeat(16_001),
            "hello\0there".to_string(),
        ] {
            assert!(!Submission {
                body,
                diagnostics: String::new()
            }
            .validate());
        }
        assert!(Submission {
            body: "연결이 안 됩니다".into(),
            diagnostics: String::new()
        }
        .validate());
    }

    #[test]
    fn discord_upload_preserves_full_text_without_mentions() {
        let submission = Submission {
            body: format!("@everyone {}", "긴 글".repeat(800)),
            diagnostics: "kasaterm 0.2.1 · macos aarch64".into(),
        };
        let body = String::from_utf8(multipart(&submission, "test-boundary")).unwrap();
        assert!(body.contains("\"allowed_mentions\":{\"parse\":[]}"));
        assert!(body.contains(&submission.document()));
        assert!(body.ends_with("--test-boundary--\r\n"));
    }

    #[tokio::test]
    async fn unconfigured_receiver_does_not_claim_delivery() {
        let state = FeedbackState {
            sink: None,
            recent: Default::default(),
        };
        let request = Request::builder()
            .header("content-type", "application/json")
            .body(axum::body::Body::from(r#"{"body":"test"}"#))
            .unwrap();
        assert_eq!(
            receive(State(state), request).await.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
}
