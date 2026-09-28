use super::*;
use std::sync::mpsc::{self, Receiver, TryRecvError};

#[derive(Default)]
pub(crate) struct State {
    pending: Option<Receiver<Result<(), kasa_mcp::feedback_client::DeliveryError>>>,
    submitted_body: String,
    pub(crate) message: Option<(String, bool)>,
    pub(crate) attempt: u64,
}

impl State {
    pub(crate) fn busy(&self) -> bool {
        self.pending.is_some()
    }
}

impl App {
    pub(crate) fn send_feedback(&mut self) {
        if self.feedback_delivery.busy() {
            return;
        }
        let body = self.feedback_body.trim().to_string();
        let diagnostics = if self.feedback_diag {
            crate::settings::diag_line()
        } else {
            String::new()
        };
        if let Err(error) = kasa_mcp::feedback_client::validate(&body, &diagnostics) {
            self.feedback_delivery.message = Some((error.message().into(), true));
            self.chrome_dirty = true;
            return;
        }
        let Some(_) = self.save_feedback_copy() else {
            self.feedback_delivery.message =
                Some(("사본을 저장하지 못해 보내지 않았어요".into(), true));
            self.chrome_dirty = true;
            return;
        };
        let (tx, rx) = mpsc::channel();
        self.feedback_delivery.attempt = self.feedback_delivery.attempt.saturating_add(1);
        self.feedback_delivery.pending = Some(rx);
        self.feedback_delivery.submitted_body = self.feedback_body.clone();
        self.feedback_delivery.message = Some((
            "이 기기에 사본을 저장했어요. 개발자 Discord로 보내는 중…".into(),
            false,
        ));
        std::thread::spawn(move || {
            let _ = tx.send(kasa_mcp::feedback_client::send(&body, &diagnostics));
        });
        self.chrome_dirty = true;
    }

    pub(crate) fn poll_feedback_delivery(&mut self) {
        let result = match self
            .feedback_delivery
            .pending
            .as_ref()
            .map(Receiver::try_recv)
        {
            Some(Ok(result)) => result,
            Some(Err(TryRecvError::Disconnected)) => {
                Err(kasa_mcp::feedback_client::DeliveryError::Unconfirmed)
            }
            _ => return,
        };
        self.feedback_delivery.pending = None;
        let (message, error) = match result {
            Ok(()) => {
                if self.feedback_body == self.feedback_delivery.submitted_body {
                    self.feedback_body.clear();
                    self.feedback_caret = 0;
                    socket::write_setting("feedback_draft", serde_json::json!(""));
                    if self.settings_input == Some(SettingsInput::FeedbackBody) {
                        self.settings_scene.take_field_backup();
                        self.settings_input = None;
                    }
                }
                (
                    "개발자 Discord에 전달했어요. 사본은 이 기기에 남아 있습니다".to_string(),
                    false,
                )
            }
            Err(error) => (
                format!("{} · 사본은 이 기기에 남아 있습니다", error.message()),
                true,
            ),
        };
        self.feedback_delivery.submitted_body.clear();
        self.feedback_delivery.message = Some((message.clone(), error));
        self.set_toast(message);
        self.chrome_dirty = true;
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }
}
