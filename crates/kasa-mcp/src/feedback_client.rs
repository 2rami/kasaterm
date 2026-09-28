use std::time::Duration;

pub const ENDPOINT: &str = "https://kasaterm.debimarlene.com/feedback";

#[derive(Debug, PartialEq, Eq)]
pub enum DeliveryError {
    Rejected(&'static str),
    Unconfirmed,
}

impl DeliveryError {
    pub fn message(&self) -> &'static str {
        match self {
            Self::Rejected(message) => message,
            Self::Unconfirmed => "전송 결과를 확인하지 못했어요. 다시 보내면 중복될 수 있습니다",
        }
    }
}

pub fn validate(body: &str, diagnostics: &str) -> Result<(), DeliveryError> {
    if body.trim().is_empty() {
        Err(DeliveryError::Rejected("무엇이 불편했는지 적어 주세요"))
    } else if body.len() > 16_000 || diagnostics.len() > 2_000 {
        Err(DeliveryError::Rejected(
            "내용이 너무 길어요. 16,000바이트 이내로 줄여 주세요",
        ))
    } else if body.contains('\0') || diagnostics.contains('\0') {
        Err(DeliveryError::Rejected("내용에 보낼 수 없는 문자가 있어요"))
    } else {
        Ok(())
    }
}

/// The caller uses a worker so a slow public receiver cannot block typing.
pub fn send(body: &str, diagnostics: &str) -> Result<(), DeliveryError> {
    validate(body, diagnostics)?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| DeliveryError::Rejected("전송을 시작하지 못했어요"))?
        .block_on(send_to(ENDPOINT, body, diagnostics))
}

async fn send_to(endpoint: &str, body: &str, diagnostics: &str) -> Result<(), DeliveryError> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(40))
        .build()
        .map_err(|_| DeliveryError::Rejected("전송을 시작하지 못했어요"))?;
    let response = client
        .post(endpoint)
        .json(&serde_json::json!({"body":body, "diagnostics":diagnostics}))
        .send()
        .await
        .map_err(|_| DeliveryError::Unconfirmed)?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(status_error(status));
    }
    let result = response
        .json::<serde_json::Value>()
        .await
        .map_err(|_| DeliveryError::Unconfirmed)?;
    if result["ok"].as_bool() == Some(true) {
        Ok(())
    } else {
        Err(DeliveryError::Unconfirmed)
    }
}

fn status_error(status: u16) -> DeliveryError {
    if status == 502 {
        return DeliveryError::Unconfirmed;
    }
    DeliveryError::Rejected(match status {
        400 | 413 => "내용을 받지 못했어요. 글의 길이와 입력을 확인해 주세요",
        429 => "잠시 후 다시 보내 주세요. 전송 요청이 많아요",
        503 => "피드백 수신 창구가 아직 준비되지 않았어요",
        _ => "피드백 서버가 요청을 받지 못했어요",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::post, Json, Router};

    #[test]
    fn payload_limits_count_utf8_bytes() {
        assert!(validate(&"한".repeat(5333), "").is_ok());
        assert!(validate(&"한".repeat(5334), "").is_err());
        assert!(validate("hello", &"a".repeat(2001)).is_err());
        assert!(validate(" ", "").is_err());
        assert_ne!(status_error(503), DeliveryError::Unconfirmed);
        assert_eq!(status_error(502), DeliveryError::Unconfirmed);
    }

    #[tokio::test]
    async fn success_requires_a_confirmed_delivery_response() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new()
            .route(
                "/ok",
                post(|| async { Json(serde_json::json!({"ok":true})) }),
            )
            .route(
                "/pending",
                post(|| async { Json(serde_json::json!({"ok":false})) }),
            )
            .route(
                "/offline",
                post(|| async { axum::http::StatusCode::SERVICE_UNAVAILABLE }),
            );
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        assert_eq!(send_to(&format!("{base}/ok"), "feedback", "").await, Ok(()));
        assert_eq!(
            send_to(&format!("{base}/pending"), "feedback", "").await,
            Err(DeliveryError::Unconfirmed)
        );
        assert_eq!(
            send_to(&format!("{base}/offline"), "feedback", "").await,
            Err(status_error(503))
        );
        server.abort();
    }
}
