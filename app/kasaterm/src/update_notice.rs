//! 새 판 알림 — Windows 업데이터가 찾은 판을 오른쪽 위 알림 한 장으로 알리고, 사람이 [업데이트] 를
//! 눌렀을 때만 받기·설치를 맡긴다. macOS 는 Sparkle 표준 창이 안내·받기·설치를 하고(`macos_sparkle.rs`),
//! 여기서는 다시 켜기 직전에 끊길 일만 묻는다.
//!
//! 결정 알림 배관(`collab.toast_action`)을 [`ACTION`] 센티널로 빌린다. 다른 알림보다 낮다 — 복사·
//! 완료·승인이 자리를 덮으면 칩을 거두고 물러섰다가, 자리가 비면 다시 선다. 덮인 글 옆에 [업데이트]
//! 가 남으면 엉뚱한 알림에 답하게 된다.
//!
//! 「닫기」는 그 판을 닫는다 — 기기 설정(`update_notice_dismissed`, 계정 동기화 밖)에 적어 두고 다음
//! 판이 나올 때까지 다시 띄우지 않는다. 판 번호 줄의 「업데이트 확인」은 닫은 판도 다시 보인다.

use super::*;

/// pane id(`%N`)와 겹치지 않는 결정 알림 센티널.
pub(crate) const ACTION: &str = "__kasaterm_update__";
const DISMISSED_KEY: &str = "update_notice_dismissed";

#[derive(Clone, Debug, PartialEq)]
enum Stage {
    Offer,
    /// 다시 켜면 끊길 일이 있어 한 번 더 묻는다. 값은 무엇이 끊기는지.
    Interrupts(String),
}

pub(crate) struct Notice {
    version: String,
    stage: Stage,
    /// 이 알림이 세운 `collab.toast` 의 시각 — 다른 알림이 글을 덮었는지 가린다.
    shown_at: Option<std::time::Instant>,
}

impl Notice {
    fn message(&self) -> String {
        match &self.stage {
            Stage::Offer => format!("새 판 v{} 있어요 · 업데이트를 누르면 받아서 다시 켜요", self.version),
            Stage::Interrupts(what) => format!("지금 업데이트하면 끊겨요 · {what}"),
        }
    }

    fn chips(&self) -> (&'static str, &'static str) {
        match self.stage {
            Stage::Offer => ("업데이트", "닫기"),
            Stage::Interrupts(_) => ("그래도 업데이트", "닫기"),
        }
    }
}

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
        self.show_update_notice();
    }

    /// Sparkle 이 받은 판을 설치하고 다시 켜려 한다. 다시 켜면 PTY 가 앱과 함께 끝나 도는 턴·명령이
    /// 끊기고, 저장 안 한 문서는 묻지 않고 닫힌다(Sparkle 의 종료는 ⌘Q 의 확인을 지나지 않는다). 끊길
    /// 것이 있으면 OS 시트로 한 번 더 묻는다. 기다렸다 저절로 다시 켜지 않는 것은, 사람이 모르는 때에
    /// 화면이 사라지기 때문이다.
    #[cfg(target_os = "macos")]
    fn confirm_update_relaunch(&mut self) {
        let dirty = self.dirty_docs(&crate::PendingClose::Window).len();
        let Some(what) = interrupts(&self.update_interrupted_panes(), dirty) else {
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
        self.update_notice = Some(Notice { version, stage: Stage::Offer, shown_at: None });
    }

    /// 지금 오른쪽 위 알림이 이 새 판 알림인가 — 칩을 그리고 누름을 받는 조건이다.
    pub(crate) fn update_notice_showing(&self) -> bool {
        let Some(notice) = self.update_notice.as_ref() else { return false };
        self.collab.toast_action.as_deref() == Some(ACTION)
            && notice.shown_at.is_some()
            && self.collab.toast.as_ref().map(|(_, at)| *at) == notice.shown_at
    }

    /// 서 있는 새 판 알림의 칩과 뜻. 다른 알림이 자리를 덮었으면 `None`.
    pub(crate) fn update_notice_chips(&self) -> Option<((&'static str, &'static str), Option<theme::NoticeTone>)> {
        if !self.update_notice_showing() {
            return None;
        }
        let notice = self.update_notice.as_ref()?;
        // 새 판은 할 일 있음(accent)이다. 끊김을 묻는 둘째 단계만 주의.
        let tone = (notice.stage == Stage::Offer).then_some(theme::NoticeTone::Info);
        Some((notice.chips(), tone))
    }

    fn show_update_notice(&mut self) {
        if self.update_notice.is_none() || self.update_notice_showing() {
            return;
        }
        if self.collab.toast_action.as_deref() == Some(ACTION) {
            // 다른 알림이 글을 덮었다 — 칩을 거두고 그 알림이 제 시간대로 흐려지게 둔다.
            self.collab.toast_action = None;
        }
        if self.collab.toast_action.is_some() || self.collab_toast_alpha() > 0.0 {
            return;
        }
        let Some(notice) = self.update_notice.as_mut() else { return };
        let now = std::time::Instant::now();
        notice.shown_at = Some(now);
        self.collab.toast = Some((notice.message(), now));
        self.collab.toast_action = Some(ACTION.to_string());
        self.collab.toast_rect = None;
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// [업데이트]·[그래도 업데이트] 는 `install`, 닫기·본문 누름은 `!install`.
    pub(crate) fn answer_update_notice(&mut self, install: bool) {
        let Some(notice) = self.update_notice.take() else { return };
        self.clear_approval_toast();
        if !install {
            if let Err(e) = crate::socket::write_settings_patch_atomic(&[(DISMISSED_KEY, serde_json::json!(notice.version))]) {
                eprintln!("[update] 닫은 판을 못 적음: {e}");
            }
            return;
        }
        // 끊길 것을 보여 주고 한 번 더 묻는다 — 까닭은 macOS 의 `confirm_update_relaunch` 와 같다.
        if notice.stage == Stage::Offer {
            let dirty = self.dirty_docs(&crate::PendingClose::Window).len();
            if let Some(what) = interrupts(&self.update_interrupted_panes(), dirty) {
                self.update_notice = Some(Notice { stage: Stage::Interrupts(what), shown_at: None, ..notice });
                return;
            }
        }
        self.start_update_install();
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

    fn start_update_install(&mut self) {
        if crate::win_sparkle::available() {
            crate::win_sparkle::install();
            return;
        }
        self.check_for_updates_now();
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
    fn the_notice_names_the_version_and_what_a_restart_cuts() {
        let notice = Notice { version: "0.2.18".into(), stage: Stage::Offer, shown_at: None };
        assert_eq!(crate::toast::split_notice(&notice.message()).0, "새 판 v0.2.18 있어요");
        assert_eq!(notice.chips(), ("업데이트", "닫기"));

        assert_eq!(interrupts(&[], 0), None);
        assert_eq!(interrupts(&["유즈".into()], 0).as_deref(), Some("일하는 창: 유즈"));
        assert_eq!(
            interrupts(&["cargo".into(), "유즈".into()], 2).as_deref(),
            Some("일하는 창: cargo 외 1개, 저장 안 한 문서 2개")
        );
        let confirm = Notice { stage: Stage::Interrupts("일하는 창: 유즈".into()), ..notice };
        assert_eq!(crate::toast::split_notice(&confirm.message()), ("지금 업데이트하면 끊겨요", Some("일하는 창: 유즈")));
        assert_eq!(confirm.chips().0, "그래도 업데이트");
    }
}
