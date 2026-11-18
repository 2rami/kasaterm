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
    Join,
    ConfirmFingerprint,
    LoginDeletions,
    BookmarkDeletions,
    TrustPhone,
}

impl Ask {
    const ALL: [(Ask, &'static str); 6] = [
        (Ask::CreateVault, "create_vault"),
        (Ask::Join, "join"),
        (Ask::ConfirmFingerprint, "confirm_fingerprint"),
        (Ask::LoginDeletions, "confirm_login_deletions"),
        (Ask::BookmarkDeletions, "confirm_bookmark_deletions"),
        (Ask::TrustPhone, "trust_phone"),
    ];

    pub(crate) fn parse(id: &str) -> Option<Ask> {
        Self::ALL.iter().find(|(_, name)| *name == id).map(|(ask, _)| *ask)
    }

    fn id(self) -> &'static str {
        Self::ALL.iter().find(|(ask, _)| *ask == self).map_or("", |(_, name)| name)
    }

    /// 브라우저 카드 아래에 보일 답.
    pub(crate) fn reply(self) -> &'static str {
        match self {
            Ask::Join => "폰에서 Face ID 로 허락해 주세요",
            Ask::TrustPhone => "카사텀 설정의 연결 기기를 열었어요",
            _ => "카사텀 창에서 확인해 주세요",
        }
    }
}

/// 시트에서 누른 확인 — 완료 처리기는 App 을 못 잡으니 여기 두고 다음 poll 이 꺼내 간다.
static ANSWER: Mutex<Option<String>> = Mutex::new(None);
static BUSY: AtomicBool = AtomicBool::new(false);

/// 지금 상태에서 그 확인을 물을 수 있나 — 브라우저가 본 상태가 그사이 바뀌었으면 시트를 띄우지 않는다.
fn askable(ask: Ask, vault: &serde_json::Value, browser: &serde_json::Value) -> bool {
    match ask {
        Ask::CreateVault => vault["state"] == "no_vault",
        Ask::Join => matches!(vault["state"].as_str(), Some("needs_join" | "key_mismatch" | "join_failed")),
        Ask::ConfirmFingerprint => vault["state"] == "confirm_fingerprint",
        Ask::LoginDeletions => vault["state"] == "deletions_pending",
        Ask::BookmarkDeletions => browser["state"] == "deletions_pending",
        Ask::TrustPhone => true,
    }
}

/// 시트의 (제목, 설명, 누를 단추).
fn sheet(ask: Ask, vault: &serde_json::Value, browser: &serde_json::Value) -> Option<(String, String, &'static str)> {
    let fingerprint = vault["fingerprint"].as_str().unwrap_or_default();
    Some(match ask {
        Ask::CreateVault => (
            "이 기기에서 동기화를 시작할까요?".into(),
            "이 기기의 비밀번호를 잠가서 계정에 올리는 첫 기기가 돼요. 다른 기기는 폰 Face ID 로 등록해서 같은 열쇠를 받아요.".into(),
            "시작하기",
        ),
        Ask::ConfirmFingerprint => {
            let by = vault["created_by"].as_str().filter(|b| !b.is_empty()).unwrap_or("이름 없는 기기");
            let when = vault["created_ms"].as_u64().filter(|ms| *ms > 0).map(crate::machinescol::clock_label).unwrap_or_default();
            (
                "열쇠 지문이 같나요?".into(),
                format!(
                    "열쇠 지문  {fingerprint}\n금고를 만든 기기  {by} {when}\n\n내 다른 기기 화면에서 이 지문을 본 적이 없으면 누르지 마세요. 금고를 만든 기기가 모르는 기기여도 누르지 마세요 — 누르기 전에는 이 기기의 비밀번호를 받지도 올리지도 않아요."
                ),
                "지문이 같아요",
            )
        }
        Ask::LoginDeletions => {
            let n = vault["deletions"].as_u64().unwrap_or(0);
            (format!("비밀번호 {n}개를 지울까요?"), "이 기기에서 한꺼번에 사라졌어요. 정말 지운 것이면 이 계정의 다른 기기에서도 지워요.".into(), "지우기")
        }
        Ask::BookmarkDeletions => {
            let n = browser["deletions"].as_u64().unwrap_or(0);
            (format!("북마크 {n}개를 지울까요?"), "이 기기에서 한꺼번에 사라졌어요. 정말 지운 것이면 이 계정의 다른 기기에서도 지워요.".into(), "지우기")
        }
        Ask::Join | Ask::TrustPhone => return None,
    })
}

impl App {
    pub(crate) fn browser_sync_ask(&mut self, ask: Ask) {
        let vault = kasa_device::login_vault::status();
        let browser = kasa_device::browser_sync::status();
        if !askable(ask, &vault, &browser) {
            kasa_device::browser_sync::poke();
            return;
        }
        match ask {
            Ask::TrustPhone => {
                self.open_settings_room(Some(SettingsCat::Machines));
            }
            Ask::Join => {
                let label = crate::render::pane_identity::local_device_name().unwrap_or_else(|| "새 기기".into());
                run(move || kasa_device::login_vault::start_join(&label));
            }
            _ => {
                let Some((title, info, primary)) = sheet(ask, &vault, &browser) else { return };
                let Some(window) = &self.window else { return };
                window.focus_window();
                let _ = super::op::confirm_sheet(window, &title, &info, primary, &ANSWER, ask.id().to_string());
            }
        }
    }

    pub(crate) fn browser_sync_ask_poll(&mut self) {
        let Some(ask) = ANSWER.lock().ok().and_then(|mut a| a.take()).as_deref().and_then(Ask::parse) else { return };
        match ask {
            Ask::CreateVault => run(|| kasa_device::login_vault::create_vault().map(|_| ())),
            Ask::ConfirmFingerprint => run(kasa_device::login_vault::confirm_fingerprint),
            Ask::LoginDeletions => {
                kasa_device::login_vault::allow_deletions();
                kasa_device::browser_sync::poke();
            }
            Ask::BookmarkDeletions => kasa_device::browser_sync::allow_deletions(),
            Ask::Join | Ask::TrustPhone => {}
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
            &json!({"state": "deletions_pending", "deletions": 2, "fingerprint": "F", "items": 1, "phones": [{"manual": false}]}),
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
        assert!(!askable(Ask::ConfirmFingerprint, &synced, &json!({})));
        assert!(!askable(Ask::LoginDeletions, &synced, &json!({})));
        assert!(!askable(Ask::BookmarkDeletions, &synced, &json!({"state": "synced"})));
        assert!(!askable(Ask::CreateVault, &synced, &json!({})));
        assert!(askable(Ask::ConfirmFingerprint, &json!({"state": "confirm_fingerprint"}), &json!({})));
    }

    #[test]
    fn the_fingerprint_sheet_shows_the_fingerprint_and_who_made_the_vault() {
        let vault = json!({"state": "confirm_fingerprint", "fingerprint": "YGAG-9P8B-AVKM-HYVX", "created_by": "거실 PC"});
        let (title, info, primary) = sheet(Ask::ConfirmFingerprint, &vault, &json!({})).unwrap();
        assert!(title.contains("지문"));
        assert!(info.contains("YGAG-9P8B-AVKM-HYVX") && info.contains("거실 PC"));
        assert_eq!(primary, "지문이 같아요");
    }
}
