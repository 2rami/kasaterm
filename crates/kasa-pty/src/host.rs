//! 엔진을 쓰는 호스트(본판 GUI·카사라이트·`kasa tui` 서버)가 정하는 것. 엔진은 어느
//! 호스트인지 모른다 — 자식 칸에 비칠 이름, 「Last login」 상태 파일 자리, OSC 52 가
//! 갈 곳을 호스트가 한 번 알려 준다. 프로세스에 호스트는 하나라 전역에 둔다
//! (`set_host_colors`·`set_cell_pixels` 와 같은 까닭: 읽는 곳은 PTY 안쪽 몇 군데뿐인데
//! 인자로 받으면 PTY 를 여는 모든 경로에 실어 날라야 한다).

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

/// OSC 52 읽기·쓰기를 받을 곳. 본판·라이트는 OS 클립보드, `kasa tui` 는 바깥
/// 터미널로 OSC 52 를 다시 낸다 — SSH 너머에서는 서버 기계의 OS 클립보드가 틀린
/// 자리라서다.
pub trait ClipboardSink: Send + Sync {
    /// 칸이 클립보드를 읽어 달라고 할 때 줄 글. 모르면 빈 글 — 칸이 답을 기다리며
    /// 멎지 않게 늘 답은 한다.
    fn load(&self) -> String;
    /// 칸이 클립보드에 쓴 글.
    fn store(&self, text: &str);
}

#[derive(Clone, Default)]
pub struct HostPolicy {
    /// 자식에게 알릴 `TERM_PROGRAM`·`TERM_PROGRAM_VERSION`. `None` 이면 둘 다 지운다 —
    /// 바깥 터미널에서 물려받은 이름(`iTerm.app` 등)이 남으면 자식이 그 터미널 전용
    /// 이스케이프를 보내 VT 가 엉뚱하게 그린다.
    pub term_program: Option<(String, String)>,
    /// 「Last login」 줄의 상태 파일(`last_login`)을 둘 폴더. `None` 이면 줄을 찍지 않고
    /// 아무 파일도 건드리지 않는다.
    pub last_login_dir: Option<PathBuf>,
    /// OSC 52 를 받을 곳. `None` 이면 OS 클립보드(feature `os-clipboard`), 그 feature 가
    /// 꺼져 있으면 읽기는 빈 글·쓰기는 버린다.
    pub clipboard: Option<Arc<dyn ClipboardSink>>,
}

static HOST: RwLock<Option<HostPolicy>> = RwLock::new(None);

/// 호스트가 켤 때 한 번 부른다. 이미 열린 칸에는 새 이름이 안 간다(env 는 띄울 때 정해진다).
pub fn set_host_policy(policy: HostPolicy) {
    *HOST.write().unwrap_or_else(|e| e.into_inner()) = Some(policy);
}

pub(crate) fn host_policy() -> HostPolicy {
    HOST.read().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default()
}

pub(crate) fn clipboard_load() -> String {
    if let Some(sink) = host_policy().clipboard {
        return sink.load();
    }
    os_clipboard_load()
}

pub(crate) fn clipboard_store(text: &str) {
    if let Some(sink) = host_policy().clipboard {
        return sink.store(text);
    }
    os_clipboard_store(text);
}

#[cfg(feature = "os-clipboard")]
fn os_clipboard_load() -> String {
    arboard::Clipboard::new().ok().and_then(|mut cb| cb.get_text().ok()).unwrap_or_default()
}

#[cfg(feature = "os-clipboard")]
fn os_clipboard_store(text: &str) {
    match arboard::Clipboard::new() {
        Ok(mut cb) => {
            if let Err(e) = cb.set_text(text) {
                eprintln!("[pty-backend] clipboard set failed: {e}");
            }
        }
        Err(e) => eprintln!("[pty-backend] clipboard open failed: {e}"),
    }
}

#[cfg(not(feature = "os-clipboard"))]
fn os_clipboard_load() -> String {
    String::new()
}

#[cfg(not(feature = "os-clipboard"))]
fn os_clipboard_store(_text: &str) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Recorder(Mutex<Vec<String>>);
    impl ClipboardSink for Recorder {
        fn load(&self) -> String {
            "from-host".into()
        }
        fn store(&self, text: &str) {
            self.0.lock().unwrap().push(text.to_string());
        }
    }

    #[test]
    fn host_clipboard_sink_receives_osc52() {
        let rec = Arc::new(Recorder(Mutex::new(Vec::new())));
        set_host_policy(HostPolicy { clipboard: Some(rec.clone()), ..host_policy() });
        clipboard_store("copied");
        assert_eq!(clipboard_load(), "from-host");
        assert_eq!(rec.0.lock().unwrap().as_slice(), ["copied"]);
        set_host_policy(HostPolicy { clipboard: None, ..host_policy() });
    }
}
