use super::*;

struct Clipboard {
    path: std::path::PathBuf,
    paste: String,
}

impl kasa_pty::ClipboardSink for Clipboard {
    fn load(&self) -> String { self.paste.clone() }
    fn store(&self, text: &str) { let _ = std::fs::write(&self.path, text); }
}

pub(crate) fn clipboard() -> Option<Arc<dyn kasa_pty::ClipboardSink>> {
    if !crate::verification_run() || std::env::var_os("KASATERM_AUTOSHELLEDIT_DIR").is_none()
        || std::env::var_os("KASATERM_SESSION_FILE").is_none() { return None; }
    Some(Arc::new(Clipboard {
        path: std::env::var_os("KASATERM_TEST_COPY_PATH")?.into(),
        paste: std::env::var("KASATERM_TEST_PASTE_TEXT").unwrap_or_default(),
    }))
}
