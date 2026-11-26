//! 새 판 카드 — 찾기·받기·준비·다시 켜기를 앱 안 한 장으로 보인다(오른쪽 아래, 복원 카드와 같은 층).
//!
//! 업데이터의 표준 창(맥 Sparkle·Windows WinSparkle)은 OS 창이라 앱과 다른 모양이었고, 앱 안에서 누른 사람에게
//! 진행이 안 보이기도 했다. 이제 업데이터는 무엇이 일어나는지만 `post` 로 알리고, 무엇을 할지는 이 카드의 단추가
//! `App::update_card_act` 로 돌려준다 — 맥은 Sparkle 사용자 드라이버(`macos_sparkle.rs`), Windows 는 자체 받기
//! (`win_update.rs`). 서명 확인·설치는 여전히 그쪽 몫이다.
//!
//! 키보드로는 누를 수 없다 — 타자 중에 Return 이 「다시 켜기」를 누르는 일을 막는다.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::{gpu, native_controls, theme};

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Stage {
    /// 사람이 누른 확인이 도는 중.
    Checking,
    Found { version: String, note: Option<String> },
    Downloading { version: String, got: u64, total: u64 },
    /// 받은 판을 푸는 중 — 0.0..=1.0. Sparkle 만 알린다(Windows MSI 는 풀 것이 없다).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Preparing { version: String, progress: f64 },
    /// 받아서 확인을 마쳤다. 다시 켜면 바뀐다.
    Ready { version: String },
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    RestartPending { version: String },
    Installing { version: String },
    /// 사람이 누른 확인의 답.
    Latest,
    /// 다시 켜져 새 판이 됐다.
    Installed { version: String },
    Failed { message: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Download,
    Skip,
    Later,
    Cancel,
    Relaunch,
    Close,
    Releases,
}

/// 「나중에」가 끌 때 설치하는가 — 맥 Sparkle 은 받은 판을 종료 때 설치하고, Windows 는 다음 확인 때 다시 묻는다.
pub(crate) const LATER_INSTALLS: bool = cfg!(target_os = "macos");

/// 쉴 때 저절로 다시 켜는가 — 맥만. Windows MSI 는 관리자 확인(UAC)을 사람이 눌러야 끝난다.
pub(crate) const RELAUNCHES_ON_ITS_OWN: bool = cfg!(target_os = "macos");

/// 미리보기 참여 기기(사람의 「내 기기」) — 새 판을 묻지 않고 받고, 맥은 쉴 때 다시 켠다. 켤 때 한 번 정한다.
static AUTOMATIC: AtomicBool = AtomicBool::new(false);

pub(crate) fn set_automatic(on: bool) {
    AUTOMATIC.store(on, Ordering::Relaxed);
}

pub(crate) fn automatic() -> bool {
    AUTOMATIC.load(Ordering::Relaxed)
}

pub(crate) fn relaunches_on_its_own() -> bool {
    automatic() && RELAUNCHES_ON_ITS_OWN
}

/// 업데이터가 알린 지금 단계. 업데이터 스레드(Windows 받기)와 주 스레드(맥 Sparkle) 어디서든 쓴다.
static STAGE: Mutex<Option<Stage>> = Mutex::new(None);

pub(crate) fn post(stage: Option<Stage>) {
    if let Ok(mut s) = STAGE.lock() {
        *s = stage;
    }
}

pub(crate) fn current() -> Option<Stage> {
    STAGE.lock().ok().and_then(|s| s.clone())
}

/// 저절로 걷히는 단계 — 알리기만 하고 할 일이 없다.
fn lingers(stage: &Stage) -> Option<Duration> {
    match stage {
        Stage::Latest => Some(Duration::from_secs(4)),
        Stage::Installed { .. } => Some(Duration::from_secs(6)),
        _ => None,
    }
}

/// 카드에 그릴 것 — 제목·두 줄 설명·진행(없으면 `None`, 모르면 음수)·단추(마지막이 주 단추).
pub(crate) struct Content {
    pub title: String,
    pub body: Vec<String>,
    pub progress: Option<f32>,
    pub buttons: Vec<(&'static str, Action)>,
    /// 설명 첫 줄을 주의 색으로 — 다시 켜면 끊길 일이 있다.
    pub warn: bool,
}

/// `interrupts` 는 지금 다시 켜면 끊길 일(`App::update_interrupts_now`). `later_installs` 는 「나중에」가 끌 때 설치되는가
/// (맥 Sparkle — 받은 판은 종료 때 설치된다). `on_its_own` 은 쉴 때 저절로 다시 켜는 기기인가.
pub(crate) fn content(stage: &Stage, interrupts: Option<&str>, later_installs: bool, on_its_own: bool) -> Content {
    let mb = |b: u64| format!("{:.1}MB", b as f64 / 1_048_576.0);
    let plain = |title: String, body: Vec<String>, progress, buttons| Content { title, body, progress, buttons, warn: false };
    match stage {
        Stage::Checking => plain("새 판을 찾는 중".into(), vec!["잠깐이면 끝나요.".into()], Some(-1.0), vec![("취소", Action::Cancel)]),
        Stage::Found { version, note } => plain(
            format!("새 판 v{version} 있어요"),
            vec![
                note.clone().unwrap_or_else(|| "받는 동안에도 하던 일은 그대로 할 수 있어요.".into()),
                "받고 나면 언제 다시 켤지 물어요.".into(),
            ],
            None,
            vec![("건너뛰기", Action::Skip), ("나중에", Action::Later), ("받기", Action::Download)],
        ),
        Stage::Downloading { version, got, total } => plain(
            format!("새 판 v{version} 받는 중"),
            vec![if *total > 0 { format!("{} / {}", mb(*got), mb(*total)) } else { mb(*got) }],
            Some(if *total > 0 { (*got as f32 / *total as f32).clamp(0.0, 1.0) } else { -1.0 }),
            vec![("취소", Action::Cancel)],
        ),
        Stage::Preparing { version, progress } => plain(
            format!("새 판 v{version} 준비하는 중"),
            vec!["받은 판의 서명을 확인하고 풀고 있어요.".into()],
            Some(*progress as f32),
            Vec::new(),
        ),
        Stage::Ready { version } => {
            let later = if on_its_own {
                "아무도 안 쓰고 끊길 일이 없으면 저절로 다시 켜요."
            } else if later_installs {
                "나중에 하면 다음에 끌 때 바뀌어요."
            } else {
                "나중에 하면 다음에 다시 알려 드려요."
            };
            Content {
                title: format!("새 판 v{version} 준비됐어요"),
                body: match interrupts {
                    Some(what) => vec![format!("지금 다시 켜면 끊겨요 · {what}"), later.into()],
                    None => vec!["다시 켜면 바로 바뀌어요. 창과 탭은 그대로 돌아와요.".into(), later.into()],
                },
                progress: None,
                buttons: vec![("나중에", Action::Later), (if interrupts.is_some() { "그래도 다시 켜기" } else { "다시 켜기" }, Action::Relaunch)],
                warn: interrupts.is_some(),
            }
        }
        Stage::RestartPending { version } => Content {
            title: format!("새 판 v{version} 다시 켜기 대기 중"),
            body: vec![match interrupts {
                Some(what) => format!("지금 다시 켜면 끊겨요 · {what}"),
                None => "앱이 종료되면 업데이트를 마쳐요.".into(),
            }],
            progress: None,
            buttons: vec![("닫기", Action::Close), (if interrupts.is_some() { "그래도 다시 켜기" } else { "다시 켜기 재시도" }, Action::Relaunch)],
            warn: interrupts.is_some(),
        },
        Stage::Installing { version } => plain(
            format!("새 판 v{version} 설치하는 중"),
            vec!["곧 다시 켜져요.".into()],
            Some(-1.0),
            Vec::new(),
        ),
        Stage::Latest => plain("최신 판이에요".into(), vec![format!("지금 쓰는 판 {}", crate::version::label())], None, vec![("닫기", Action::Close)]),
        Stage::Installed { version } => plain(
            format!("새 판 v{version} 으로 바뀌었어요"),
            vec!["창과 탭은 그대로 돌아왔어요.".into()],
            None,
            vec![("닫기", Action::Close)],
        ),
        Stage::Failed { message } => plain(
            "업데이트하지 못했어요".into(),
            vec![message.clone(), "릴리스 페이지에서 직접 받을 수도 있어요.".into()],
            None,
            vec![("릴리스 페이지", Action::Releases), ("닫기", Action::Close)],
        ),
    }
}

const CARD_W: f32 = 360.0;
const PAD: f32 = 16.0;
const BUTTON_H: f32 = 30.0;

/// 카드 자리 — 오른쪽 아래, 바닥 막대 위. `above` 는 그 아래 이미 선 카드(복원 진행)의 높이.
pub(crate) fn rect(win: (f32, f32), bottom_reserved: f32, above: f32, c: &Content) -> (f32, f32, f32, f32) {
    let w = CARD_W.min((win.0 - 24.0).max(200.0));
    let h = 58.0 + 18.0 * c.body.len() as f32
        + if c.progress.is_some() { 16.0 } else { 0.0 }
        + if c.buttons.is_empty() { 0.0 } else { BUTTON_H + 14.0 }
        + 6.0;
    let gap = if above > 0.0 { above + 10.0 } else { 0.0 };
    ((win.0 - w - 16.0).max(8.0), (win.1 - bottom_reserved - h - 12.0 - gap).max(8.0), w, h)
}

/// 그린다 — 단추 자리를 돌려준다(누름 판정은 그리는 사각형과 같다).
pub(crate) fn paint(
    g: &mut gpu::GpuRenderer,
    card: (f32, f32, f32, f32),
    c: &Content,
    cursor: (f32, f32),
    now: Instant,
) -> Vec<(Action, (f32, f32, f32, f32))> {
    let (x, y, w, h) = card;
    crate::panel_rect_outlined(g, x, y, w, h, theme::radius_md() * 1.5, theme::surface_active());
    let tile = 30.0;
    let (tx, ty) = (x + PAD, y + PAD);
    g.round_rect_fill(tx, ty, tile, tile, 9.0, theme::accent());
    let ink = theme::enforce_contrast_at([255, 255, 255, 255], theme::accent(), 3.0);
    g.queue_icon("umbrella", tx + 7.0, ty + 7.0, 16.0, ink);
    let text_x = tx + tile + 12.0;
    let text_w = (x + w - PAD - text_x).max(1.0);
    let title = crate::info::fit_text(g, &c.title, text_w, 14.0, true);
    g.draw_text(text_x, ty + 1.0, &title, gpu::DrawOpts { font_size: 14.0, color: theme::text(), bold: true, italic: false });
    let sub = crate::info::fit_text(g, "KASATERM 업데이트", text_w, 11.0, false);
    g.draw_text(text_x, ty + 19.0, &sub, gpu::DrawOpts { font_size: 11.0, color: theme::text_dim(), bold: false, italic: false });
    let mut line_y = y + 58.0;
    for (i, line) in c.body.iter().enumerate() {
        let color = if i == 0 && c.warn { theme::enforce_contrast_at(theme::attention(), theme::surface_active(), 4.5) } else { theme::text_dim() };
        let shown = crate::info::fit_text(g, line, w - 2.0 * PAD, 12.5, false);
        g.draw_text(x + PAD, line_y, &shown, gpu::DrawOpts { font_size: 12.5, color, bold: false, italic: false });
        line_y += 18.0;
    }
    if let Some(p) = c.progress {
        let (bx, by, bw) = (x + PAD, line_y + 4.0, w - 2.0 * PAD);
        g.round_rect_fill(bx, by, bw, 5.0, 2.5, theme::surface());
        if p >= 0.0 {
            g.round_rect_fill(bx, by, (bw * p).max(5.0), 5.0, 2.5, theme::accent());
        } else {
            // 끝을 모르는 진행 — 짧은 막대가 오간다.
            let t = (now.duration_since(epoch()).as_secs_f32() * 0.8).fract();
            let seg = bw * 0.28;
            g.round_rect_fill(bx + (bw - seg) * (1.0 - (2.0 * t - 1.0).abs()), by, seg, 5.0, 2.5, theme::accent());
        }
    }
    let mut hits = Vec::new();
    if c.buttons.is_empty() {
        return hits;
    }
    let by = y + h - PAD - BUTTON_H + 6.0;
    let mut right = x + w - PAD;
    let last = c.buttons.len() - 1;
    for (i, (label, action)) in c.buttons.iter().enumerate().rev() {
        let primary = i == last && c.buttons.len() > 1 || matches!(action, Action::Relaunch | Action::Download);
        let bw = native_controls::button_width(g, label, BUTTON_H, primary);
        let r = (right - bw, by, bw, BUTTON_H);
        if r.0 < x + PAD {
            break;
        }
        native_controls::text_button(g, r, cursor, label, native_controls::Style { primary, ..Default::default() });
        hits.push((*action, r));
        right = r.0 - 8.0;
    }
    hits
}

fn epoch() -> Instant {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *START.get_or_init(Instant::now)
}

/// 화면에 선 카드 — 그린 자리와 단추, 단계가 바뀐 때(저절로 걷기·리그 누르기가 본다).
#[derive(Default)]
pub(crate) struct Shown {
    pub card: Option<(f32, f32, f32, f32)>,
    pub hits: Vec<(Action, (f32, f32, f32, f32))>,
    pub stage: Option<Stage>,
    pub since: Option<Instant>,
    /// 지금 다시 켜면 끊길 일 — 준비된 동안 2초마다 다시 잰다.
    pub interrupts: Option<String>,
    pub interrupts_at: Option<Instant>,
    /// 리그 — 이 단계에서 이미 눌렀다.
    pub pressed: bool,
    pub shots: u32,
    /// 「내 기기」가 묻지 않고 받기 시작한 판 — 받다 실패해도 같은 판을 되풀이해 받지 않는다.
    pub fetched_on_its_own: Option<String>,
}

impl Shown {
    /// 단계가 바뀌면 시각을 새로 잡는다. 저절로 걷힐 때가 됐으면 `true`.
    pub(crate) fn observe(&mut self, stage: Option<&Stage>, now: Instant) -> bool {
        if self.stage.as_ref() != stage {
            self.stage = stage.cloned();
            self.since = stage.map(|_| now);
            self.pressed = false;
            // 앞 단계에서 잰 끊길 일로 새 준비 카드가 저절로 다시 켜지 않게 다시 잰다.
            self.interrupts = None;
            self.interrupts_at = None;
        }
        match (stage.and_then(lingers), self.since) {
            (Some(d), Some(at)) => now.duration_since(at) >= d,
            _ => false,
        }
    }

    pub(crate) fn hit(&self, p: (f32, f32)) -> Option<Action> {
        let inside = |r: &(f32, f32, f32, f32)| p.0 >= r.0 && p.0 <= r.0 + r.2 && p.1 >= r.1 && p.1 <= r.1 + r.3;
        self.hits.iter().find(|(_, r)| inside(r)).map(|(a, _)| *a)
    }

    pub(crate) fn covers(&self, p: (f32, f32)) -> bool {
        self.card.is_some_and(|r| p.0 >= r.0 && p.0 <= r.0 + r.2 && p.1 >= r.1 && p.1 <= r.1 + r.3)
    }
}

/// 업데이트 리그(`docs/verify-app.md`) — 단계가 `after` 만큼 그대로면 누를 단추. 이름을 주면 그 단추, 아니면 주 단추.
pub(crate) fn rig_choice(c: &Content, wanted: Option<&str>) -> Option<Action> {
    if let Some(name) = wanted {
        return c.buttons.iter().find(|(label, _)| *label == name).map(|(_, a)| *a);
    }
    c.buttons.last().map(|(_, a)| *a)
}

/// 업데이터 없는 판(dev·검증 리그)에서 카드 흐름을 보는 길 — `KASATERM_FAKE_UPDATE=<판>` 이면 켠 뒤 한 번 「찾음」을
/// 세우고, 단추마다 다음 단계로 넘긴다. 받기는 2초에 걸쳐 차오르고, 다시 켜기는 1.5초 뒤 「바뀌었어요」로 끝난다.
static FAKE: Mutex<Option<(String, Instant)>> = Mutex::new(None);

pub(crate) fn fake_tick() {
    static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !STARTED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        if let Some(v) = std::env::var("KASATERM_FAKE_UPDATE").ok().filter(|v| !v.is_empty()) {
            post(Some(Stage::Found { version: v, note: None }));
        }
        return;
    }
    let Some((version, at)) = FAKE.lock().ok().and_then(|f| f.clone()) else { return };
    let elapsed = at.elapsed();
    match current() {
        Some(Stage::Downloading { total, .. }) => {
            if elapsed >= Duration::from_secs(2) {
                post(Some(Stage::Ready { version }));
            } else {
                let got = (total as f64 * elapsed.as_secs_f64() / 2.0) as u64;
                post(Some(Stage::Downloading { version, got, total }));
            }
        }
        Some(Stage::Installing { .. }) if elapsed >= Duration::from_millis(1500) => post(Some(Stage::Installed { version })),
        _ => {}
    }
}

pub(crate) fn fake_respond(action: Action) {
    let version = match current() {
        Some(Stage::Found { version, .. } | Stage::Ready { version } | Stage::Downloading { version, .. }) => version,
        _ => {
            post(None);
            return;
        }
    };
    if let Ok(mut f) = FAKE.lock() {
        *f = Some((version.clone(), Instant::now()));
    }
    post(match action {
        Action::Download => Some(Stage::Downloading { version, got: 0, total: 24 << 20 }),
        Action::Relaunch => Some(Stage::Installing { version }),
        _ => None,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_stage_says_what_happens_and_offers_a_way_out() {
        let stages = [
            Stage::Checking,
            Stage::Found { version: "0.2.300".into(), note: None },
            Stage::Downloading { version: "0.2.300".into(), got: 5 << 20, total: 20 << 20 },
            Stage::Ready { version: "0.2.300".into() },
            Stage::Latest,
            Stage::Installed { version: "0.2.300".into() },
            Stage::Failed { message: "망이 끊겼어요".into() },
        ];
        for stage in &stages {
            let c = content(stage, None, true, false);
            assert!(!c.title.is_empty() && !c.body.is_empty(), "{stage:?}");
            assert!(!c.buttons.is_empty(), "누를 것이 없으면 걷을 길도 없다: {stage:?}");
        }
        let down = content(&stages[2], None, true, false);
        assert_eq!(down.progress, Some(0.25));
        assert_eq!(down.body[0], "5.0MB / 20.0MB");
    }

    #[test]
    fn ready_names_what_a_relaunch_would_interrupt() {
        let ready = Stage::Ready { version: "0.2.300".into() };
        let calm = content(&ready, None, true, false);
        assert!(!calm.warn);
        assert_eq!(calm.buttons.last().map(|b| b.1), Some(Action::Relaunch));
        let busy = content(&ready, Some("일하는 창: 아리스 외 1개"), false, false);
        assert!(busy.warn && busy.body[0].contains("아리스 외 1개"));
        assert_eq!(busy.buttons.last().map(|b| b.0), Some("그래도 다시 켜기"));
        assert!(busy.body[1].contains("다시 알려"), "Windows 는 끌 때 설치하지 않는다");
        let alone = content(&ready, None, true, true);
        assert!(alone.body[1].contains("저절로"), "쉴 때 저절로 다시 켜는 기기는 그렇게 말한다");
    }

    #[test]
    fn notices_without_a_decision_clear_themselves() {
        let mut shown = Shown::default();
        let t0 = Instant::now();
        assert!(!shown.observe(Some(&Stage::Latest), t0));
        assert!(shown.observe(Some(&Stage::Latest), t0 + Duration::from_secs(5)));
        let found = Stage::Found { version: "1.0.0".into(), note: None };
        assert!(!shown.observe(Some(&found), t0 + Duration::from_secs(5)));
        assert!(!shown.observe(Some(&found), t0 + Duration::from_secs(600)), "결정이 필요한 카드는 기다린다");
    }

    #[test]
    fn the_rig_presses_the_named_or_primary_button() {
        let c = content(&Stage::Found { version: "1.0.0".into(), note: None }, None, true, false);
        assert_eq!(rig_choice(&c, None), Some(Action::Download));
        assert_eq!(rig_choice(&c, Some("나중에")), Some(Action::Later));
        assert_eq!(rig_choice(&c, Some("없는 단추")), None);
    }
}
