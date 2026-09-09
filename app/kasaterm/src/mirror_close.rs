//! Closing a mirror is a choice between this view and its source device.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MirrorTarget {
    pub local: String,
    pub base: String,
    pub source: String,
    pub label: String,
}

#[derive(Debug, PartialEq, Eq)]
enum CloseEffect {
    Cancel,
    Local,
    Source,
    Stale,
    Error(String),
}

fn choice_effect(button: ConfirmBtn, unchanged: bool) -> CloseEffect {
    if button == ConfirmBtn::Cancel { return CloseEffect::Cancel; }
    if !unchanged { return CloseEffect::Stale; }
    match button {
        ConfirmBtn::Close => CloseEffect::Local,
        ConfirmBtn::CloseSource => CloseEffect::Source,
        _ => CloseEffect::Cancel,
    }
}

fn completion_effect(result: Result<(), String>, unchanged: bool) -> CloseEffect {
    match result {
        Err(error) => CloseEffect::Error(error),
        Ok(()) if unchanged => CloseEffect::Local,
        Ok(()) => CloseEffect::Stale,
    }
}

fn request_source_close(targets: &[MirrorTarget]) -> Result<(), String> {
    let mut closed = std::collections::HashSet::new();
    targets.iter().try_for_each(|t| {
        if !closed.insert((&t.base, &t.source)) { return Ok(()); }
        kasa_mcp::remote::close_remote_pane(&t.base, &t.source, None, false)
            .map_err(|e| format!("{} {}: {e:#}", t.label, t.source))
    })
}

impl App {
    /// Closing a source is a layout event even while its PTY is kept for undo.
    /// Do not use EOF or the socket disconnect as a proxy for this decision.
    pub(crate) fn notify_source_closed(&self, target: &str) {
        let ws = self.ws.lock().unwrap();
        let mut ids = vec![target.to_string()];
        if let Some(pane) = ws.panes.get(target) {
            ids.extend(pane.tabs.iter().filter_map(|tab| tab.pid.clone()));
        }
        ids.sort();
        ids.dedup();
        for id in ids { kasa_mcp::push_viewer_control(&id, r#"{"t":"source-closed"}"#); }
    }

    fn mirror_close_targets(&self, action: &PendingClose) -> Vec<MirrorTarget> {
        let ws = self.ws.lock().unwrap();
        let mut ids = match action {
            PendingClose::Tab { pane, idx } => ws.panes.get(pane)
                .and_then(|p| p.tabs.get(*idx))
                .map(|tab| vec![tab.pid.clone().unwrap_or_else(|| pane.clone())])
                .unwrap_or_default(),
            PendingClose::Pane { pane } => {
                let mut ids = vec![pane.clone()];
                if let Some(p) = ws.panes.get(pane) {
                    ids.extend(p.tabs.iter().filter_map(|tab| tab.pid.clone()));
                }
                ids
            }
            _ => Vec::new(),
        };
        ids.sort();
        ids.dedup();
        ids.into_iter().filter_map(|local| {
            let info = kasa_mcp::remote::remote_info(&local)?;
            info.view.then_some(MirrorTarget {
                local, base: info.base, source: info.remote_id, label: info.label,
            })
        }).collect()
    }

    pub(crate) fn guard_mirror_close(&mut self, action: &PendingClose) -> bool {
        let targets = self.mirror_close_targets(action);
        if targets.is_empty() { return false; }
        if self.guard_dirty(action) { return true; }
        self.confirm_close = Some(ConfirmClose {
            why: CloseWhy::Mirror { targets, closing: false, error: None },
            action: action.clone(),
        });
        self.chrome_dirty = true;
        if let Some(w) = &self.window { w.request_redraw(); }
        true
    }

    pub(crate) fn choose_mirror_close(
        &mut self, action: PendingClose, targets: Vec<MirrorTarget>, button: ConfirmBtn,
    ) {
        match choice_effect(button, self.mirror_close_targets(&action) == targets) {
            CloseEffect::Local => {
                self.remote_keep.extend(targets.iter().map(|t| t.local.clone()));
                self.do_close(action);
                return;
            }
            CloseEffect::Source => {}
            CloseEffect::Stale => {
                self.set_toast("거울이 바뀌었어. 닫을 창을 다시 골라줘".into());
                return;
            }
            _ => return,
        }
        self.confirm_close = Some(ConfirmClose {
            why: CloseWhy::Mirror { targets: targets.clone(), closing: true, error: None },
            action: action.clone(),
        });
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let result = request_source_close(&targets);
            let _ = proxy.send_event(UserEvent::MirrorCloseDone { action, targets, result });
        });
    }

    pub(crate) fn finish_mirror_close(
        &mut self, action: PendingClose, targets: Vec<MirrorTarget>, result: Result<(), String>,
    ) {
        self.confirm_close = None;
        match completion_effect(result, self.mirror_close_targets(&action) == targets) {
            CloseEffect::Error(error) => {
                self.confirm_close = Some(ConfirmClose {
                    why: CloseWhy::Mirror { targets, closing: false, error: Some(error) },
                    action,
                });
            }
            CloseEffect::Local => {
                self.remote_keep.extend(targets.iter().map(|t| t.local.clone()));
                self.do_close(action);
                self.info.machines_col.last_refresh = None;
            }
            _ => self.set_toast("원본은 닫혔어. 바뀐 거울 목록은 그대로 뒀어".into()),
        }
        self.chrome_dirty = true;
        if let Some(w) = &self.window { w.request_redraw(); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn cancel_and_mirror_only_never_call_the_source() {
        let source = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        source.set_nonblocking(true).unwrap();
        assert_eq!(choice_effect(ConfirmBtn::Cancel, true), CloseEffect::Cancel);
        assert_eq!(choice_effect(ConfirmBtn::Close, true), CloseEffect::Local);
        assert_eq!(source.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
    }

    fn source_case(ok: bool) -> (CloseEffect, usize) {
        let source = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", source.local_addr().unwrap());
        let (seen, received) = std::sync::mpsc::channel();
        let (approve, approval) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = source.accept().unwrap();
            socket.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
            let mut request = [0u8; 4096];
            let count = socket.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..count]);
            assert!(request.starts_with("POST /close-pane?surface=%2516 HTTP/1.1\r\n"));
            assert!(!request.contains("kill=1"));
            seen.send(()).unwrap();
            approval.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
            let body = if ok { r#"{"ok":true}"# } else { r#"{"ok":false,"error":"source unavailable"}"# };
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            1
        });
        let target = MirrorTarget { local: "%2".into(), base, source: "%16".into(), label: "fake source".into() };
        assert_eq!(choice_effect(ConfirmBtn::CloseSource, true), CloseEffect::Source);
        let worker = std::thread::spawn(move || request_source_close(&[target.clone(), target]));
        received.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert!(!worker.is_finished(), "local close cannot be approved before the source replies");
        approve.send(()).unwrap();
        let effect = completion_effect(worker.join().unwrap(), true);
        (effect, server.join().unwrap())
    }

    #[test]
    fn source_success_closes_local_only_after_ack_and_deduplicates_source() {
        let (effect, requests) = source_case(true);
        assert_eq!(effect, CloseEffect::Local);
        assert_eq!(requests, 1);
    }

    #[test]
    fn source_failure_preserves_local_mirror_and_reports_reason() {
        let (effect, requests) = source_case(false);
        assert!(matches!(effect, CloseEffect::Error(error) if error.contains("source unavailable")));
        assert_eq!(requests, 1);
    }

    #[test]
    fn changed_target_is_never_closed_by_an_old_confirmation() {
        assert_eq!(choice_effect(ConfirmBtn::CloseSource, false), CloseEffect::Stale);
        assert_eq!(completion_effect(Ok(()), false), CloseEffect::Stale);
        assert_eq!(choice_effect(ConfirmBtn::Cancel, false), CloseEffect::Cancel);
    }
}
