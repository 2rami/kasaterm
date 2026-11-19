//! 카사브라우저 설정 「동기화」의 확인 단추가 부르는 자리(`POST /browser-sync/ask`). 로그인 금고와 북마크·탭
//! 동기화의 상태는 브라우저가 그리고(`kasa_device::browser_sync::display`), 사람이 확정해야 하는 일만 여기서
//! 카사텀 창의 시트로 묻는다 — 브라우저 단추나 HTTP 부름은 시트를 띄울 뿐 아무것도 확정하지 못한다.
//! 시트에는 개수와 지문만 보인다. 사이트·아이디는 어디에도 그리지 않는다(docs/login-vault-sync.md).

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ask {
    CreateVault,
    LoginDeletions,
    BookmarkDeletions,
}

impl Ask {
    const ALL: [(Ask, &'static str); 3] = [
        (Ask::CreateVault, "create_vault"),
        (Ask::LoginDeletions, "confirm_login_deletions"),
        (Ask::BookmarkDeletions, "confirm_bookmark_deletions"),
    ];

    pub(crate) fn parse(id: &str) -> Option<Ask> {
        Self::ALL.iter().find(|(_, name)| *name == id).map(|(ask, _)| *ask)
    }

    fn id(self) -> &'static str {
        Self::ALL.iter().find(|(ask, _)| *ask == self).map_or("", |(_, name)| name)
    }

    /// 브라우저 카드 아래에 보일 답.
    pub(crate) fn reply(self) -> &'static str {
        "카사텀 창에서 확인해 주세요"
    }
}

/// 시트에서 누른 확인 — 완료 처리기는 App 을 못 잡으니 여기 두고 다음 poll 이 꺼내 간다.
static ANSWER: Mutex<Option<String>> = Mutex::new(None);
static BUSY: AtomicBool = AtomicBool::new(false);

/// 지금 상태에서 그 확인을 물을 수 있나 — 브라우저가 본 상태가 그사이 바뀌었으면 시트를 띄우지 않는다.
fn askable(ask: Ask, vault: &serde_json::Value, browser: &serde_json::Value) -> bool {
    match ask {
        Ask::CreateVault => vault["state"] == "no_vault",
        Ask::LoginDeletions => vault["state"] == "deletions_pending",
        Ask::BookmarkDeletions => browser["state"] == "deletions_pending",
    }
}

/// 시트의 (제목, 설명, 누를 단추).
fn sheet(ask: Ask, vault: &serde_json::Value, browser: &serde_json::Value) -> (String, String, &'static str) {
    match ask {
        Ask::CreateVault => (
            "이 기기에서 동기화를 시작할까요?".into(),
            "이 기기의 비밀번호를 잠가서 계정에 올리는 첫 기기가 돼요. 열쇠는 관문에도 맡겨서, 다른 기기는 같은 계정으로 로그인만 하면 받아요.".into(),
            "시작하기",
        ),
        Ask::LoginDeletions => {
            let n = vault["deletions"].as_u64().unwrap_or(0);
            (format!("비밀번호 {n}개를 지울까요?"), "이 기기에서 한꺼번에 사라졌어요. 정말 지운 것이면 이 계정의 다른 기기에서도 지워요.".into(), "지우기")
        }
        Ask::BookmarkDeletions => {
            let n = browser["deletions"].as_u64().unwrap_or(0);
            (format!("북마크 {n}개를 지울까요?"), "이 기기에서 한꺼번에 사라졌어요. 정말 지운 것이면 이 계정의 다른 기기에서도 지워요.".into(), "지우기")
        }
    }
}

impl App {
    pub(crate) fn browser_sync_ask(&mut self, ask: Ask) {
        let vault = kasa_device::login_vault::status();
        let browser = kasa_device::browser_sync::status();
        if !askable(ask, &vault, &browser) {
            kasa_device::browser_sync::poke();
            return;
        }
        let (title, info, primary) = sheet(ask, &vault, &browser);
        let Some(window) = &self.window else { return };
        window.focus_window();
        let _ = super::op::confirm_sheet(window, &title, &info, primary, &ANSWER, ask.id().to_string());
    }

    pub(crate) fn browser_sync_ask_poll(&mut self) {
        let Some(ask) = ANSWER.lock().ok().and_then(|mut a| a.take()).as_deref().and_then(Ask::parse) else { return };
        match ask {
            Ask::CreateVault => run(|| kasa_device::login_vault::create_vault().map(|_| ())),
            Ask::LoginDeletions => {
                kasa_device::login_vault::allow_deletions();
                kasa_device::browser_sync::poke();
            }
            Ask::BookmarkDeletions => kasa_device::browser_sync::allow_deletions(),
        }
    }
}

/// 금고 일은 관문을 부르니 뒤에서 한다. 실패만 알림으로 알린다 — 성공은 브라우저 화면이 다음 바퀴에 보인다.
fn run(work: impl FnOnce() -> Result<(), String> + Send + 'static) {
    if BUSY.swap(true, Ordering::AcqRel) {
        return;
    }
    std::thread::spawn(move || {
        if let Err(error) = work() {
            crate::chrome::notify_desktop("동기화를 마치지 못했어요", &error, None, Some("browser-sync-ask"), None);
        }
        BUSY.store(false, Ordering::Release);
        kasa_device::browser_sync::poke();
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_ask_the_browser_can_show_parses_back() {
        let state = kasa_device::browser_sync::display::display(
            &json!({"state": "deletions_pending", "deletions": 2, "fingerprint": "F", "items": 1}),
            &json!({"state": "deletions_pending", "deletions": 3}),
            0,
        );
        for ask in state["asks"].as_array().unwrap() {
            let id = ask["id"].as_str().unwrap();
            assert_eq!(Ask::parse(id).map(Ask::id), Some(id));
        }
        assert_eq!(Ask::parse("allow_everything"), None);
    }

    #[test]
    fn a_stale_button_opens_nothing() {
        let synced = json!({"state": "synced"});
        assert!(!askable(Ask::LoginDeletions, &synced, &json!({})));
        assert!(!askable(Ask::BookmarkDeletions, &synced, &json!({"state": "synced"})));
        assert!(!askable(Ask::CreateVault, &synced, &json!({})));
        assert!(askable(Ask::CreateVault, &json!({"state": "no_vault"}), &json!({})));
    }

    #[test]
    fn the_first_device_sheet_says_the_relay_keeps_the_key() {
        let (title, info, primary) = sheet(Ask::CreateVault, &json!({"state": "no_vault"}), &json!({}));
        assert!(title.contains("동기화"));
        assert!(info.contains("관문") && info.contains("로그인만"));
        assert_eq!(primary, "시작하기");
    }
}
