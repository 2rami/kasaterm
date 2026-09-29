//! 날씨 설정과 「이 창에 비가 얼마나 오나」 판정 — 정본은 docs/weather.md.
//!
//! 전역 설정은 계정으로 기기끼리 맞추고, 창별 덮어쓰기는 그 기기 세션에만 둔다.
//! 판정은 순수 함수라 설정 화면·렌더·테스트가 같은 답을 본다.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RainAmount {
    None,
    #[default]
    Drizzle,
    Rain,
    Downpour,
}

impl RainAmount {
    pub(crate) const ALL: [RainAmount; 4] = [RainAmount::None, RainAmount::Drizzle, RainAmount::Rain, RainAmount::Downpour];

    pub(crate) fn label(self) -> &'static str {
        match self {
            RainAmount::None => "없음",
            RainAmount::Drizzle => "이슬비",
            RainAmount::Rain => "비",
            RainAmount::Downpour => "폭우",
        }
    }

    pub(crate) fn index(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WeatherTarget {
    AllWindows,
    #[default]
    FocusedOnly,
    UnfocusedOnly,
    PickedOnly,
    BackgroundOnly,
}

impl WeatherTarget {
    pub(crate) const ALL: [WeatherTarget; 5] = [
        WeatherTarget::AllWindows,
        WeatherTarget::FocusedOnly,
        WeatherTarget::UnfocusedOnly,
        WeatherTarget::PickedOnly,
        WeatherTarget::BackgroundOnly,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            WeatherTarget::AllWindows => "전체 창",
            WeatherTarget::FocusedOnly => "초점 창만",
            WeatherTarget::UnfocusedOnly => "초점 아닌 창만",
            WeatherTarget::PickedOnly => "고른 창만",
            WeatherTarget::BackgroundOnly => "창 밖 배경만",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WipeMode {
    #[default]
    OnInput,
    OnFocus,
    Never,
}

impl WipeMode {
    pub(crate) const ALL: [WipeMode; 3] = [WipeMode::OnInput, WipeMode::OnFocus, WipeMode::Never];

    pub(crate) fn label(self) -> &'static str {
        match self {
            WipeMode::OnInput => "입력하면",
            WipeMode::OnFocus => "초점 오면",
            WipeMode::Never => "안 닦음",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct WeatherEffects {
    pub streaks: bool,
    pub drops: bool,
    pub mist: bool,
    pub ripples: bool,
    pub buttons: bool,
}

impl Default for WeatherEffects {
    fn default() -> Self {
        WeatherEffects { streaks: true, drops: true, mist: true, ripples: true, buttons: true }
    }
}

impl WeatherEffects {
    pub(crate) fn any(&self) -> bool {
        self.streaks || self.drops || self.mist || self.ripples || self.buttons
    }
}

/// 학생 상태 날씨의 세 갈래. 하네스 상태를 여기로 접는 것은 부르는 쪽 일이다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PaneMood {
    /// 학생이 없는 창이거나 상태를 모른다 — 전역 비 양을 쓴다.
    Unknown,
    Busy,
    YourTurn,
    Resting,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct WeatherSettings {
    pub enabled: bool,
    pub amount: RainAmount,
    /// −1(왼쪽) ~ 1(오른쪽).
    pub wind_dir: f32,
    /// 0 ~ 1.
    pub wind_strength: f32,
    pub target: WeatherTarget,
    pub effects: WeatherEffects,
    pub wipe: WipeMode,
    /// 닦인 창이 다시 다 젖기까지. 30 ~ 600초.
    pub rewet_secs: u32,
    pub by_status: bool,
    pub busy: RainAmount,
    pub your_turn: RainAmount,
    pub resting: RainAmount,
    /// 켜면 OS 「동작 줄이기」「투명도 줄이기」를 따르지 않는다.
    pub ignore_os: bool,
}

impl Default for WeatherSettings {
    fn default() -> Self {
        WeatherSettings {
            enabled: false,
            amount: RainAmount::Drizzle,
            wind_dir: 0.0,
            wind_strength: 0.0,
            target: WeatherTarget::FocusedOnly,
            effects: WeatherEffects::default(),
            wipe: WipeMode::OnInput,
            rewet_secs: 180,
            by_status: false,
            busy: RainAmount::Drizzle,
            your_turn: RainAmount::Downpour,
            resting: RainAmount::None,
            ignore_os: false,
        }
    }
}

pub(crate) const REWET_MIN: u32 = 30;
pub(crate) const REWET_MAX: u32 = 600;

impl WeatherSettings {
    /// 저장·동기화로 들어온 값을 범위 안으로.
    pub(crate) fn sanitized(mut self) -> Self {
        self.wind_dir = if self.wind_dir.is_finite() { self.wind_dir.clamp(-1.0, 1.0) } else { 0.0 };
        self.wind_strength = if self.wind_strength.is_finite() { self.wind_strength.clamp(0.0, 1.0) } else { 0.0 };
        self.rewet_secs = self.rewet_secs.clamp(REWET_MIN, REWET_MAX);
        self
    }

    /// 바람 한 값(−1~1): 방향 × 세기.
    pub(crate) fn wind(&self) -> f32 {
        self.wind_dir.clamp(-1.0, 1.0) * self.wind_strength.clamp(0.0, 1.0)
    }
}

/// 창 우클릭 「이 창 날씨」. 이 기기 세션에만 남는다.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "amount")]
pub(crate) enum PaneWeather {
    #[default]
    Follow,
    /// 「고른 창만」일 때 비가 오는 창. 비 양은 설정을 따른다.
    Picked,
    Clear,
    Fixed(RainAmount),
}

impl PaneWeather {
    pub(crate) fn label(self) -> String {
        match self {
            PaneWeather::Follow => "설정 따름".into(),
            PaneWeather::Picked => "고른 창".into(),
            PaneWeather::Clear => "맑음".into(),
            PaneWeather::Fixed(a) => format!("{} 고정", a.label()),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct OsMotion {
    pub reduce_motion: bool,
    pub reduce_transparency: bool,
}

/// 이번 프레임의 판정 결과.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Verdict {
    /// 패스를 통째로 건너뛸지.
    pub active: bool,
    /// 물방울이 흐르고 빗줄기가 내리는지. 꺼지면 맺힌 것만 남는다.
    pub moving: bool,
}

pub(crate) fn verdict(s: &WeatherSettings, os: OsMotion) -> Verdict {
    let os_blocks = !s.ignore_os && os.reduce_transparency;
    let active = s.enabled && !os_blocks && s.effects.any();
    Verdict { active, moving: active && (s.ignore_os || !os.reduce_motion) }
}

/// 창 하나에 오는 비 양. 창별 덮어쓰기가 「어디에」보다 이기고, 학생 상태는 비 양만 바꾼다.
pub(crate) fn pane_amount(s: &WeatherSettings, over: PaneWeather, focused: bool, mood: PaneMood) -> RainAmount {
    let base = || {
        if !s.by_status {
            return s.amount;
        }
        match mood {
            PaneMood::Unknown => s.amount,
            PaneMood::Busy => s.busy,
            PaneMood::YourTurn => s.your_turn,
            PaneMood::Resting => s.resting,
        }
    };
    match over {
        PaneWeather::Clear => RainAmount::None,
        PaneWeather::Fixed(a) => a,
        PaneWeather::Picked => base(),
        PaneWeather::Follow => {
            let included = match s.target {
                WeatherTarget::AllWindows => true,
                WeatherTarget::FocusedOnly => focused,
                WeatherTarget::UnfocusedOnly => !focused,
                WeatherTarget::PickedOnly | WeatherTarget::BackgroundOnly => false,
            };
            if included {
                base()
            } else {
                RainAmount::None
            }
        }
    }
}

/// 창 밖(사이드바·옆 열·머리줄·하단바)에 오는 비 양.
pub(crate) fn background_amount(s: &WeatherSettings) -> RainAmount {
    match s.target {
        WeatherTarget::AllWindows | WeatherTarget::BackgroundOnly => s.amount,
        _ => RainAmount::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on() -> WeatherSettings {
        WeatherSettings { enabled: true, ..Default::default() }
    }

    #[test]
    fn defaults_follow_the_decision_record() {
        let s = WeatherSettings::default();
        assert!(!s.enabled);
        assert_eq!(s.amount, RainAmount::Drizzle);
        assert_eq!(s.target, WeatherTarget::FocusedOnly);
        assert_eq!(s.wipe, WipeMode::OnInput);
        assert_eq!(s.rewet_secs, 180);
        assert!(s.effects.streaks && s.effects.drops && s.effects.mist && s.effects.ripples && s.effects.buttons);
        assert!(!s.by_status && !s.ignore_os);
        assert_eq!((s.wind_dir, s.wind_strength), (0.0, 0.0));
    }

    #[test]
    fn off_costs_nothing() {
        assert!(!verdict(&WeatherSettings::default(), OsMotion::default()).active);
        let mut s = on();
        s.effects = WeatherEffects { streaks: false, drops: false, mist: false, ripples: false, buttons: false };
        assert!(!verdict(&s, OsMotion::default()).active);
    }

    #[test]
    fn focused_only_rains_on_the_focused_pane() {
        let s = on();
        assert_eq!(pane_amount(&s, PaneWeather::Follow, true, PaneMood::Unknown), RainAmount::Drizzle);
        assert_eq!(pane_amount(&s, PaneWeather::Follow, false, PaneMood::Unknown), RainAmount::None);
        assert_eq!(background_amount(&s), RainAmount::None);
    }

    #[test]
    fn each_target_picks_its_places() {
        let mut s = on();
        s.target = WeatherTarget::AllWindows;
        assert_eq!(pane_amount(&s, PaneWeather::Follow, false, PaneMood::Unknown), RainAmount::Drizzle);
        assert_eq!(background_amount(&s), RainAmount::Drizzle);
        s.target = WeatherTarget::UnfocusedOnly;
        assert_eq!(pane_amount(&s, PaneWeather::Follow, true, PaneMood::Unknown), RainAmount::None);
        assert_eq!(pane_amount(&s, PaneWeather::Follow, false, PaneMood::Unknown), RainAmount::Drizzle);
        s.target = WeatherTarget::PickedOnly;
        assert_eq!(pane_amount(&s, PaneWeather::Follow, true, PaneMood::Unknown), RainAmount::None);
        assert_eq!(pane_amount(&s, PaneWeather::Picked, false, PaneMood::Unknown), RainAmount::Drizzle);
        s.target = WeatherTarget::BackgroundOnly;
        assert_eq!(pane_amount(&s, PaneWeather::Follow, true, PaneMood::Unknown), RainAmount::None);
        assert_eq!(background_amount(&s), RainAmount::Drizzle);
    }

    #[test]
    fn the_pane_override_wins_over_the_target() {
        let s = on();
        assert_eq!(pane_amount(&s, PaneWeather::Fixed(RainAmount::Downpour), false, PaneMood::Unknown), RainAmount::Downpour);
        assert_eq!(pane_amount(&s, PaneWeather::Clear, true, PaneMood::Unknown), RainAmount::None);
    }

    #[test]
    fn student_status_changes_only_the_amount() {
        let mut s = on();
        s.by_status = true;
        assert_eq!(pane_amount(&s, PaneWeather::Follow, true, PaneMood::YourTurn), RainAmount::Downpour);
        assert_eq!(pane_amount(&s, PaneWeather::Follow, true, PaneMood::Resting), RainAmount::None);
        assert_eq!(pane_amount(&s, PaneWeather::Follow, true, PaneMood::Unknown), RainAmount::Drizzle);
        assert_eq!(pane_amount(&s, PaneWeather::Follow, false, PaneMood::YourTurn), RainAmount::None);
        assert_eq!(pane_amount(&s, PaneWeather::Fixed(RainAmount::Rain), true, PaneMood::YourTurn), RainAmount::Rain);
    }

    #[test]
    fn os_settings_turn_weather_down_unless_ignored() {
        let s = on();
        let trans = OsMotion { reduce_motion: false, reduce_transparency: true };
        assert!(!verdict(&s, trans).active);
        let motion = OsMotion { reduce_motion: true, reduce_transparency: false };
        assert_eq!(verdict(&s, motion), Verdict { active: true, moving: false });
        let ignored = WeatherSettings { ignore_os: true, ..on() };
        assert_eq!(verdict(&ignored, OsMotion { reduce_motion: true, reduce_transparency: true }), Verdict { active: true, moving: true });
    }

    #[test]
    fn stored_values_are_clamped() {
        let s = WeatherSettings { wind_dir: 7.0, wind_strength: -1.0, rewet_secs: 5, ..Default::default() }.sanitized();
        assert_eq!((s.wind_dir, s.wind_strength, s.rewet_secs), (1.0, 0.0, REWET_MIN));
        let old: WeatherSettings = serde_json::from_str(r#"{"enabled":true}"#).unwrap();
        assert!(old.enabled);
        assert_eq!(old.target, WeatherTarget::FocusedOnly);
        let pane: PaneWeather = serde_json::from_str(r#"{"kind":"fixed","amount":"rain"}"#).unwrap();
        assert_eq!(pane, PaneWeather::Fixed(RainAmount::Rain));
    }
}
