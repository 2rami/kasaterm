//! 새 판은 OS 알림으로 알리고, 클릭하면 업데이터의 표준 확인 창을 연다.
//! 앱 종료 직전의 작업·문서 보호는 macOS 시트와 WinSparkle 콜백이 맡는다.

use super::*;

/// pane id(`%N`)와 겹치지 않는 결정 알림 센티널.
pub(crate) const ACTION: &str = "__kasaterm_update__";
const DISMISSED_KEY: &str = "update_notice_dismissed";

pub(crate) struct Notice { version: String }

/// 닫은 판이거나 그보다 옛 판이면 알리지 않는다.
fn should_offer(found: &str, dismissed: Option<&str>) -> bool {
    match dismissed {
        Some(closed) => closed != found && !crate::win_sparkle::version_newer(closed, found),
        None => true,
    }
}

/// 쉬는 중에도 전경 프로세스가 이 이름이라, 명령 이름으로는 일하는지 못 가린다.
const HARNESSES: [&str; 3] = ["claude", "codex", "gemini"];

/// 다시 켜면 잃는 것 — 끊길 창 이름(첫 것만 부른다)과 저장 안 한 문서 수.
fn interrupts(busy: &[String], dirty_docs: usize) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(who) = busy.first() {
        parts.push(match busy.len() {
            1 => format!("일하는 창: {who}"),
            n => format!("일하는 창: {who} 외 {}개", n - 1),
        });
    }
    if dirty_docs > 0 {
        parts.push(format!("저장 안 한 문서 {dirty_docs}개"));
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

impl App {
    /// 업데이터가 찾은 판을 받아 알림을 세운다. 주 스레드 루프 턴마다(`about_to_wait`).
    pub(crate) fn tick_update_notice(&mut self) {
        if self.lite {
            return;
        }
        #[cfg(target_os = "macos")]
        if let Some(updater) = self.sparkle_updater.as_ref() {
            crate::macos_sparkle::tick(updater);
            if crate::macos_sparkle::take_relaunch_request() {
                self.confirm_update_relaunch();
            }
            if crate::macos_sparkle::take_postponed() {
                self.set_toast("새 판은 다음에 끌 때 설치돼요 · 지금 하려면 업데이트 확인".to_string());
            }
        }
        if let Some(version) = crate::win_sparkle::take_found() {
            self.offer_update(version);
        }
    }

    /// Sparkle 이 받은 판을 설치하고 다시 켜려 한다. 다시 켜면 PTY 가 앱과 함께 끝나 도는 턴·명령이
    /// 끊기고, 저장 안 한 문서는 묻지 않고 닫힌다(Sparkle 의 종료는 ⌘Q 의 확인을 지나지 않는다). 끊길
    /// 것이 있으면 OS 시트로 한 번 더 묻는다. 기다렸다 저절로 다시 켜지 않는 것은, 사람이 모르는 때에
    /// 화면이 사라지기 때문이다.
    #[cfg(target_os = "macos")]
    fn confirm_update_relaunch(&mut self) {
        let Some(what) = self.update_interrupts_now() else {
            crate::macos_sparkle::answer_relaunch(true);
            return;
        };
        let info = format!("{what}\n「나중에」를 누르면 다음에 끌 때 설치돼요.");
        let shown = self.window.as_ref().is_some_and(|window| {
            crate::macos_open::confirm_sheet(window, "지금 다시 켜면 끊겨요", &info,
                ("그래도 다시 켜기", "나중에"), true, crate::macos_sparkle::answer_relaunch)
        });
        if !shown {
            crate::macos_sparkle::answer_relaunch(false);
        }
    }

    fn offer_update(&mut self, version: String) {
        let settings = crate::socket::read_settings();
        if !should_offer(&version, settings[DISMISSED_KEY].as_str()) {
            return;
        }
        // 같은 판은 한 번만 — 시간마다 도는 확인이 서 있는 알림을 다시 세우지 않는다.
        if self.update_notice.as_ref().is_some_and(|n| n.version == version) {
            return;
        }
        crate::chrome::notify_desktop(&format!("새 판 v{version} 있어요"),
            "알림을 누르면 업데이트 확인 창을 열어요", None, Some(&format!("update:{version}")),
            Some((ACTION, Some(&version))));
        self.update_notice = Some(Notice { version });
    }

    pub(crate) fn open_update_notification(&mut self, version: &str) {
        if self.update_notice.as_ref().is_some_and(|n| n.version == version) {
            self.check_for_updates_now();
        }
    }

    /// 지금 다시 켜면 잃는 것. 알림·설치 직전 확인이 같은 답을 쓴다.
    pub(crate) fn update_interrupts_now(&self) -> Option<String> {
        interrupts(&self.update_interrupted_panes(), self.dirty_docs(&crate::PendingClose::Window).len())
    }

    /// 다시 켜면 끊길 창. 학생은 상태(일함·사람 기다림)로, 학생이 아닌 창은 도는 명령으로 본다.
    /// `restart_facts` 는 상태가 잡힌 셸을 학생으로 쳐 그 안의 명령을 안 본다 — 빌드가 도는 셸을
    /// 놓친 채 다시 켜면 그 빌드가 끊긴다.
    fn update_interrupted_panes(&self) -> Vec<String> {
        let mut ids: Vec<&String> = self.pty.keys().collect();
        ids.sort();
        let mut busy = Vec::new();
        for id in ids {
            let state = self.collab.hub.resolved(id).map(|r| r.state);
            if let Some(s) = state.filter(|s| s.is_busy() || s.needs_you()) {
                busy.push(self.pane_character_if_known(id).unwrap_or_else(|| s.board_word().to_string()));
            } else if let Some(name) = self.pid_busy(id).filter(|n| !HARNESSES.iter().any(|h| n.ends_with(h))) {
                busy.push(name);
            }
        }
        busy
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_closed_version_stays_closed_until_a_newer_one() {
        assert!(should_offer("0.2.18", None));
        assert!(!should_offer("0.2.18", Some("0.2.18")));
        assert!(!should_offer("0.2.17", Some("0.2.18")));
        assert!(should_offer("0.2.19", Some("0.2.18")));
    }

    #[test]
    fn restart_warning_preserves_running_work_and_unsaved_documents() {
        assert_eq!(interrupts(&[], 0), None);
        assert_eq!(interrupts(&["학생".into()], 0).as_deref(), Some("일하는 창: 학생"));
        assert_eq!(interrupts(&["cargo".into(), "학생".into()], 2).as_deref(),
            Some("일하는 창: cargo 외 1개, 저장 안 한 문서 2개"));
    }
}
