//! 새 판 카드의 앱 쪽 — 업데이터가 알린 단계를 카드로 세우고(`update_card.rs`), 카드의 단추를 그 업데이터에 돌려준다.
//! 다시 켜면 끊길 일(일하는 창·저장 안 한 문서)은 준비된 카드가 늘 보여 준다 — 「다시 켜기」를 누르는 것이 그 확인이다.

use super::*;
use crate::update_card::{Action, Stage};

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

/// 끊길 일을 다시 재는 주기 — 프로세스를 훑으니 프레임마다 하지 않는다.
const INTERRUPTS_EVERY: std::time::Duration = std::time::Duration::from_secs(2);

/// 「내 기기」가 받은 판으로 저절로 다시 켜기 전, 아무도 키보드·마우스를 안 만진 시간.
#[cfg(any(target_os = "macos", test))]
const IDLE_BEFORE_RELAUNCH: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// 새 판으로 다시 켠다는 표식 — 다음 실행이 「복원할까요」에서 멈추지 않고 칸·세션을 바로 되살린다.
/// 설정 파일 옆에 두어 격리 리그는 제 자리에만 쓴다.
fn relaunch_marker() -> Option<std::path::PathBuf> {
    Some(crate::socket::settings_file_path()?.with_file_name("update-relaunch"))
}

/// 표식이 이만큼 지났으면 새 판 다시 켜기가 아니라 나중에 사람이 켠 것이다.
const RELAUNCH_MARKER_FRESH: std::time::Duration = std::time::Duration::from_secs(15 * 60);

pub(crate) fn mark_relaunch() {
    if let Some(path) = relaunch_marker() {
        let _ = std::fs::write(path, "");
    }
}

/// 이번 실행이 새 판 다시 켜기인가. 표식은 한 번 보면 걷는다.
pub(crate) fn relaunched_for_update() -> bool {
    let Some(path) = relaunch_marker() else { return false };
    let fresh = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|at| at.elapsed().ok())
        .is_some_and(|age| age < RELAUNCH_MARKER_FRESH);
    let _ = std::fs::remove_file(&path);
    fresh
}

#[cfg(any(target_os = "macos", test))]
fn quiet_enough(measured: bool, interrupts: Option<&str>, untouched: std::time::Duration) -> bool {
    measured && interrupts.is_none() && untouched >= IDLE_BEFORE_RELAUNCH
}

#[cfg(any(target_os = "macos", test))]
fn idle_recheck_in(untouched: std::time::Duration) -> std::time::Duration {
    IDLE_BEFORE_RELAUNCH.saturating_sub(untouched).max(INTERRUPTS_EVERY)
}

/// 이 기기를 아무도 만지지 않은 시간 — 키보드·마우스·트랙패드 어느 것이든.
#[cfg(target_os = "macos")]
fn untouched_for() -> std::time::Duration {
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventSourceSecondsSinceLastEventType(source: i32, event_type: u32) -> f64;
    }
    // kCGEventSourceStateCombinedSessionState · kCGAnyInputEventType
    let seconds = unsafe { CGEventSourceSecondsSinceLastEventType(0, u32::MAX) };
    std::time::Duration::try_from_secs_f64(seconds).unwrap_or_default()
}

impl App {
    /// 주 스레드 루프 턴마다(`about_to_wait`) — 업데이터 틱, 카드의 저절로 걷기·끊길 일 재기·리그 누르기.
    pub(crate) fn tick_update_notice(&mut self) {
        if self.lite {
            return;
        }
        #[cfg(target_os = "macos")]
        if let Some(updater) = self.sparkle_updater.as_ref() {
            crate::macos_sparkle::tick(updater);
        }
        crate::win_update::tick();
        crate::update_card::fake_tick();
        let now = std::time::Instant::now();
        let stage = crate::update_card::current();
        let was = self.update_card.stage.clone();
        if self.update_card.observe(stage.as_ref(), now) {
            crate::update_card::post(None);
        }
        if was != self.update_card.stage {
            self.chrome_dirty = true;
            // 진행 값만 바뀐 것은 같은 단계다 — 단계가 넘어갈 때만 남긴다.
            if was.as_ref().map(std::mem::discriminant) != self.update_card.stage.as_ref().map(std::mem::discriminant) {
                self.rig_update_shot();
            }
        }
        if matches!(stage, Some(Stage::Ready { .. }))
            && self.update_card.interrupts_at.is_none_or(|at| now.duration_since(at) >= INTERRUPTS_EVERY)
        {
            let what = self.update_interrupts_now();
            if what != self.update_card.interrupts {
                self.chrome_dirty = true;
            }
            self.update_card.interrupts = what;
            self.update_card.interrupts_at = Some(now);
        }
        if crate::update_card::automatic() {
            self.update_on_its_own(stage.as_ref());
        }
        // 진행 막대는 업데이터 스레드가 값을 바꾸니 보이는 동안 계속 다시 그린다.
        if matches!(stage, Some(Stage::Checking | Stage::Downloading { .. } | Stage::Preparing { .. } | Stage::Installing { .. })) {
            self.chrome_dirty = true;
        }
        self.rig_update_press(now);
    }

    fn update_on_its_own(&mut self, stage: Option<&Stage>) {
        match stage {
            Some(Stage::Found { version, .. }) if self.update_card.fetched_on_its_own.as_deref() != Some(version.as_str()) => {
                self.update_card.fetched_on_its_own = Some(version.clone());
                self.update_card_act(Action::Download);
            }
            #[cfg(target_os = "macos")]
            Some(Stage::Ready { .. })
                if quiet_enough(
                    self.update_card.interrupts_at.is_some(),
                    self.update_card.interrupts.as_deref(),
                    untouched_for(),
                ) =>
            {
                // The card may show a measurement from before a remote input or a shell command started.
                self.collab.hub.refresh_from_gui();
                if self.update_interrupts_now().is_none() {
                    self.update_card_act(Action::Relaunch);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn update_deadline(&self) -> Option<std::time::Instant> {
        #[cfg(target_os = "macos")]
        if let Some(updater) = self.sparkle_updater.as_ref().filter(|updater| updater.automatic()) {
            let probe = updater.next_probe();
            if matches!(crate::update_card::current(), Some(Stage::Ready { .. })) {
                let retry = std::time::Instant::now() + idle_recheck_in(untouched_for());
                return Some(probe.min(retry));
            }
            return Some(probe);
        }
        None
    }

    /// 카드의 단추.
    pub(crate) fn update_card_act(&mut self, action: Action) {
        self.chrome_dirty = true;
        if action == Action::Relaunch {
            mark_relaunch();
        }
        match action {
            Action::Close => {
                crate::update_card::post(None);
                return;
            }
            Action::Releases => {
                crate::update_card::post(None);
                crate::chrome::open_url_in_browser(crate::version::RELEASES);
                return;
            }
            _ => {}
        }
        #[cfg(target_os = "macos")]
        if self.sparkle_updater.is_some() {
            crate::macos_sparkle::respond(action);
            return;
        }
        if crate::win_update::available() {
            crate::win_update::respond(action, &self.proxy);
            return;
        }
        crate::update_card::fake_respond(action);
    }

    /// 업데이트 리그 — 단계가 바뀔 때마다 앱 화면을 남긴다(`KASATERM_UPDATE_RIG_SHOTS`).
    fn rig_update_shot(&mut self) {
        if !crate::verification_run() && std::env::var_os("KASATERM_UPDATE_RIG_FEED").is_none() {
            return;
        }
        let Some(dir) = std::env::var_os("KASATERM_UPDATE_RIG_SHOTS").map(std::path::PathBuf::from) else { return };
        if self.update_card.stage.is_none() {
            return;
        }
        self.update_card.shots += 1;
        let path = dir.join(format!("{:03}-card.png", self.update_card.shots));
        self.pending_capture.push((std::time::Instant::now() + std::time::Duration::from_millis(400), path.display().to_string()));
    }

    /// 업데이트 리그 — 단계가 `KASATERM_UPDATE_RIG_PRESS_MS` 만큼 그대로면 주 단추(또는 `KASATERM_UPDATE_RIG_BUTTON`)를 누른다.
    fn rig_update_press(&mut self, now: std::time::Instant) {
        if !crate::verification_run() && std::env::var_os("KASATERM_UPDATE_RIG_FEED").is_none() {
            return;
        }
        let Some(after) = std::env::var("KASATERM_UPDATE_RIG_PRESS_MS").ok().and_then(|v| v.parse().ok()).map(std::time::Duration::from_millis) else { return };
        let (Some(stage), Some(since)) = (self.update_card.stage.clone(), self.update_card.since) else { return };
        if self.update_card.pressed || now.duration_since(since) < after {
            return;
        }
        let content = crate::update_card::content(&stage, self.update_card.interrupts.as_deref(), crate::update_card::LATER_INSTALLS, crate::update_card::relaunches_on_its_own());
        let wanted = std::env::var("KASATERM_UPDATE_RIG_BUTTON").ok();
        if let Some(action) = crate::update_card::rig_choice(&content, wanted.as_deref()) {
            self.update_card.pressed = true;
            eprintln!("[update-rig] 카드 {:?} 누름", action);
            self.update_card_act(action);
        }
    }

    /// 지금 다시 켜면 잃는 것. 준비된 카드가 같은 답을 쓴다.
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
        if self.restore_progress.is_some() || self.restore_applying.is_some() || !self.pending_restores.is_empty() {
            busy.push("세션 복원 중".into());
        }
        for id in ids {
            let resolved = self.collab.hub.resolved(id);
            if resolved.is_some_and(|r| r.state.is_busy() || r.state.needs_you() || r.bg_active || !r.questions.is_empty()) {
                busy.push(self.pane_character_if_known(id).unwrap_or_else(|| id.to_string()));
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
    fn an_update_relaunches_on_its_own_only_when_nobody_is_there_and_nothing_breaks() {
        let long = IDLE_BEFORE_RELAUNCH + std::time::Duration::from_secs(1);
        assert!(quiet_enough(true, None, long));
        assert!(!quiet_enough(false, None, long));
        assert!(!quiet_enough(true, Some("일하는 창: 학생"), long));
        assert!(!quiet_enough(true, None, std::time::Duration::from_secs(60)));
    }

    #[test]
    fn idle_update_wakes_at_the_idle_threshold_and_rechecks_blockers_afterwards() {
        use std::time::Duration;
        assert_eq!(idle_recheck_in(Duration::from_secs(60)), Duration::from_secs(540));
        assert_eq!(idle_recheck_in(IDLE_BEFORE_RELAUNCH), INTERRUPTS_EVERY);
        assert_eq!(idle_recheck_in(IDLE_BEFORE_RELAUNCH + Duration::from_secs(30)), INTERRUPTS_EVERY);
    }

    #[test]
    fn restart_warning_preserves_running_work_and_unsaved_documents() {
        assert_eq!(interrupts(&[], 0), None);
        assert_eq!(interrupts(&["학생".into()], 0).as_deref(), Some("일하는 창: 학생"));
        assert_eq!(interrupts(&["cargo".into(), "학생".into()], 2).as_deref(),
            Some("일하는 창: cargo 외 1개, 저장 안 한 문서 2개"));
    }
}
