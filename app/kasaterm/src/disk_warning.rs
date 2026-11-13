//! 디스크 지키기의 앱 쪽 — 문턱을 새로 넘으면 맥 알림·폰 푸시, 경고 중에는 사이드바 트레이 위 카드. 재고 치우는
//! 일은 `kasa_socket::disk_guard` 의 뒤 스레드가 하고, 여기는 GUI 스레드에서 그 값을 받아 두고 그리기만 한다
//! (그리기 경로에서 자식 프로세스 금지). 규칙은 `docs/disk-guard.md`, 모양은 design.md 「디스크 경고」.

use std::sync::Arc;

use kasa_socket::disk_guard::{self, Level, Status};

use crate::{gpu, theme};

/// 사이드바 경고 카드가 트레이 위에서 차지하는 높이(위아래 여백 6 포함).
pub(crate) const CARD_H: f32 = 52.0;

/// GUI 스레드가 들고 있는 값 — 한 프레임 안의 배치·그리기·클릭이 같은 값을 본다.
#[derive(Default)]
pub(crate) struct State {
    pub(crate) status: Option<Arc<Status>>,
    pub(crate) card: Option<(f32, f32, f32, f32)>,
}

/// 설정 「연결 기기 → 디스크 지키기」가 그리는 값. 설정 파일은 바꿀 때만 다시 읽는다(그리기마다 읽지 않는다).
pub(crate) struct SettingsView {
    pub(crate) warn_gib: u64,
    pub(crate) cap_gib: u64,
    pub(crate) auto_prune: bool,
    pub(crate) note: String,
}

thread_local! {
    static CONFIG: std::cell::RefCell<Option<disk_guard::Config>> = const { std::cell::RefCell::new(None) };
}

pub(crate) fn settings_view() -> SettingsView {
    let cfg = CONFIG.with(|c| c.borrow_mut().get_or_insert_with(disk_guard::Config::load).clone());
    let gib = |bytes: u64| bytes >> 30;
    let mut note = format!(
        "남은 공간이 {} 아래로 내려가면 맥 알림·폰 푸시로 알리고 사이드바에 경고를 띄웁니다. 캐시·다른 앱 데이터는 지우지 않고 크기만 보여 줍니다.",
        disk_guard::size_label(cfg.warn_free)
    );
    if let Some(status) = disk_guard::status() {
        note.push_str(&format!(" 지금 남은 공간 {}.", disk_guard::size_label(status.free)));
    }
    if let Some(log) = disk_guard::log_path() {
        note.push_str(&format!(" 기록: {}", log.display()));
    }
    SettingsView { warn_gib: gib(cfg.warn_free), cap_gib: gib(cfg.build_cap), auto_prune: cfg.auto_prune, note }
}

/// 설정을 바꿨다 — 화면 값을 다시 읽고 지키미도 다음 1분을 기다리지 않고 다시 잰다.
pub(crate) fn forget_config() {
    CONFIG.with(|c| *c.borrow_mut() = None);
    disk_guard::refresh();
}

/// 카드에 쓰는 글 — `&mut self.gpu` 를 빌리기 전에 떠 둔다.
pub(crate) struct Card {
    pub(crate) title: String,
    pub(crate) detail: String,
    pub(crate) critical: bool,
    pub(crate) tip: String,
}

impl Card {
    fn of(status: &Status) -> Card {
        let detail = match (status.big.first(), &status.last_pass) {
            (Some(place), _) => format!("큰 곳 · {} {}", place.label, disk_guard::size_label(place.bytes)),
            (None, Some(pass)) if pass.freed > 0 => format!("빌드 산출물 {} 지움", disk_guard::size_label(pass.freed)),
            _ => format!("문턱 {}", disk_guard::size_label(status.warn_free)),
        };
        let mut tip = vec![format!(
            "남은 공간 {} · 문턱 {}",
            disk_guard::size_label(status.free),
            disk_guard::size_label(status.warn_free)
        )];
        if !status.big.is_empty() {
            tip.push(format!("큰 곳: {}", status.big_line(usize::MAX)));
            tip.push("캐시·앱 데이터는 자동으로 지우지 않아요".into());
        }
        if let Some(pass) = status.last_pass.as_ref().filter(|p| p.freed > 0) {
            tip.push(format!("빌드 산출물 {}개 {}는 지웠어요", pass.removed, disk_guard::size_label(pass.freed)));
        }
        if !status.auto_prune {
            tip.push("자동 정리가 꺼져 있어요(설정 「연결 기기」)".into());
        }
        tip.push(if status.big.is_empty() { "누르면 정리 기록을 열어요" } else { "누르면 가장 큰 곳을 파인더에서 열어요" }.into());
        Card {
            title: status.headline(),
            detail,
            critical: status.level == Level::Critical,
            tip: tip.join("\n"),
        }
    }
}

impl crate::App {
    /// 지키미 이벤트(이벤트 루프로 넘어온 것). 문턱을 새로 넘었으면 사람을 부른다.
    pub(crate) fn on_disk_guard(&mut self, alert: bool) {
        self.disk_warning.status = disk_guard::status().filter(|s| s.level != Level::Ok);
        if alert {
            self.ring_disk_alert();
        }
        self.chrome_dirty = true;
    }

    /// 맥 알림과 폰 푸시가 같은 말을 한다. 폰은 자리에 없을 때 보는 것이라 어느 기기인지 제목에 싣는다.
    fn ring_disk_alert(&self) {
        let Some(status) = disk_guard::status().filter(|s| s.level != Level::Ok) else { return };
        let machine = crate::info::cached_local_machine_name().unwrap_or_default();
        let (title, body) = status.alert_text(machine);
        let dedup = format!("disk:{:?}", status.level);
        crate::chrome::notify_desktop(&title, &body, None, Some(&dedup), None);
        // 관문은 빈 pane 을 받지 않는다. 「disk」 는 어느 칸도 아니라 폰은 눌러도 허브에 머문다.
        self.ring_phone("disk", &title, None, &body, "waiting:blocked", "disk".into(), &dedup);
    }

    pub(crate) fn disk_card_h(&self) -> f32 {
        if self.disk_warning.status.is_some() { CARD_H } else { 0.0 }
    }

    pub(crate) fn disk_card(&self) -> Option<Card> {
        self.disk_warning.status.as_deref().map(Card::of)
    }

    /// 무엇을 비울지는 사람이 고른다 — 가장 큰 곳(없으면 정리 기록)을 파인더에서 연다.
    pub(crate) fn open_disk_biggest(&self) {
        let Some(status) = &self.disk_warning.status else { return };
        let path = match status.big.first() {
            Some(place) => std::path::PathBuf::from(&place.path),
            None => match disk_guard::log_path() {
                Some(log) => log,
                None => return,
            },
        };
        #[cfg(target_os = "macos")]
        let _ = crate::proc::command("/usr/bin/open").arg("-R").arg(&path).spawn();
        #[cfg(not(target_os = "macos"))]
        let _ = crate::proc::command("xdg-open").arg(path.parent().unwrap_or(&path)).spawn();
    }
}

/// 트레이 위 경고 카드. 문턱 아래는 주의색, 위험(5GB 아래)은 위험색으로 테두리·아이콘을 칠한다.
pub(crate) fn paint_card(g: &mut gpu::GpuRenderer, rect: (f32, f32, f32, f32), card: &Card, cursor: (f32, f32)) {
    let (x, y, w, h) = rect;
    let tone = if card.critical { theme::danger() } else { theme::attention() };
    crate::round_rect(g, x, y, w, h, theme::radius_md(), theme::with_alpha(tone, 0x80));
    crate::round_rect(g, x + 1.0, y + 1.0, w - 2.0, h - 2.0, theme::radius_md() - 1.0, theme::panel_bg());
    crate::round_rect(g, x + 1.0, y + 1.0, w - 2.0, h - 2.0, theme::radius_md() - 1.0, theme::with_alpha(tone, 0x1c));
    let hover = cursor.0 >= x && cursor.0 <= x + w && cursor.1 >= y && cursor.1 <= y + h;
    if hover {
        crate::hover_rect(g, x + 1.0, y + 1.0, w - 2.0, h - 2.0, theme::radius_md() - 1.0);
    }
    let icon = 14.0;
    g.queue_icon("triangle-alert", x + 10.0, y + (h - icon) / 2.0, icon, tone);
    let text_x = x + 10.0 + icon + 8.0;
    let avail = (x + w - 8.0 - text_x).max(0.0);
    let title = crate::info::fit_text(g, &card.title, avail, 12.0, true);
    g.draw_text(text_x, y + 5.0, &title, gpu::DrawOpts { font_size: 12.0, color: theme::text(), bold: true, italic: false });
    let detail = crate::info::fit_text(g, &card.detail, avail, 10.5, false);
    g.draw_text(text_x, y + 22.0, &detail, gpu::DrawOpts { font_size: 10.5, color: theme::text_dim(), bold: false, italic: false });
}

#[cfg(test)]
mod tests {
    use super::*;
    use kasa_socket::disk_guard::{PassSummary, Place};

    const GIB: u64 = 1 << 30;

    #[test]
    fn card_leads_with_the_biggest_place_and_explains_the_click() {
        let mut status = Status { free: 9 * GIB, warn_free: 20 * GIB, level: Level::Low, auto_prune: true, ..Status::default() };
        let card = Card::of(&status);
        assert_eq!((card.title.as_str(), card.detail.as_str()), ("디스크 9.0GB 남음", "문턱 20GB"));
        assert!(card.tip.ends_with("누르면 정리 기록을 열어요"));
        status.last_pass = Some(PassSummary { at: 0, reason: "pressure", freed: 12 * GIB, removed: 3, skipped: 1 });
        assert_eq!(Card::of(&status).detail, "빌드 산출물 12GB 지움");
        status.big = vec![Place { label: "~/Library/Caches".into(), path: "/Users/x/Library/Caches".into(), bytes: 126 * GIB }];
        let card = Card::of(&status);
        assert_eq!(card.detail, "큰 곳 · ~/Library/Caches 126GB");
        assert!(card.tip.contains("자동으로 지우지 않아요") && card.tip.contains("빌드 산출물 3개 12GB는 지웠어요"));
        assert!(!card.critical);
    }

    /// 그리기 경로는 지키미가 낸 값만 읽는다 — 큰 폴더를 재거나 정리를 부르면 프레임이 멎는다.
    #[test]
    fn render_path_never_measures_or_prunes() {
        for (name, src) in [
            ("disk_warning.rs", include_str!("disk_warning.rs")),
            ("render.rs", include_str!("render.rs")),
            ("render/sidebar.rs", include_str!("render/sidebar.rs")),
            ("chrome.rs", include_str!("chrome.rs")),
        ] {
            for banned in ["disk_guard::measure_places", "disk_guard::run_pass", "disk_guard::run_and_log", "disk_guard::snapshot"] {
                let hits = src.lines().filter(|l| l.contains(banned) && !l.contains("for banned in")).count();
                assert_eq!(hits, 0, "{name} 이 그리기 경로에서 {banned} 를 부른다");
            }
        }
    }
}
