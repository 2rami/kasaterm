//! Windows 업데이트 피드 — 채널(안정판·preview)과 Sparkle 형식 appcast 읽기·판 비교. 받기·확인·설치는 `win_update.rs`.
//!
//! 예전엔 WinSparkle.dll 이 받기·설치를 맡았는데 그 창은 OS 모양으로 고정이라 앱 겉모습을 입힐 수 없었다. 서명 확인(EdDSA)과
//! MSI 갈아 끼우기 도우미는 원격 업데이트 창구(`kasa_socket::app_update`)에 이미 있어 그것을 쓴다.
//!
//! 확인 주기는 맥과 같다 — 켠 지 10초 뒤, 그 뒤 안정판은 하루, preview 참여 기기는 한 시간마다. 참여 판정도
//! 맥과 같은 두 설정(`version::preview_opted_in`)이고, 참여 기기는 피드만 preview 로 바뀐다.

#[cfg(any(windows, test))]
use std::time::Duration;

#[cfg(any(windows, test))]
const STABLE_FEED: &str = "https://2rami.github.io/kasaterm/appcast-win.xml";
#[cfg(any(windows, test))]
const PREVIEW_FEED: &str = "https://2rami.github.io/kasaterm/appcast-win-preview.xml";
/// 켠 뒤 첫 확인까지 — 창이 서고 세션이 돌아온 뒤에 카드가 서게 한다(맥 `FIRST_PROBE`).
#[cfg(windows)]
pub(crate) const FIRST_PROBE: Duration = Duration::from_secs(10);
/// EdDSA 공개키는 mac Sparkle 과 **공유** — `.msi` 를 mac `sign_update`(Keychain 개인키)로 서명하고 이 키로 확인한다.
/// build-app.sh 의 SUPublicEDKey 와 같아야 한다.
#[cfg(windows)]
pub(crate) const ED_PUBLIC_KEY: &str = "E4tFAb2UND+0QhgTSv2pFYKIC3ReT/dLia20KHfZxKw=";

/// 이 기기가 받는 판 — 빠른 업데이트(preview)에 참여했나.
#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Channel {
    Stable,
    Preview,
}

#[cfg(any(windows, test))]
impl Channel {
    pub(crate) fn of(settings: &serde_json::Value) -> Self {
        if crate::version::preview_opted_in(settings) { Self::Preview } else { Self::Stable }
    }

    /// 이 채널의 Windows 피드.
    pub(crate) fn feed(self) -> &'static str {
        match self {
            Self::Stable => STABLE_FEED,
            Self::Preview => PREVIEW_FEED,
        }
    }

    /// 다음 확인까지 — 맥 안정판(Info.plist `SUScheduledCheckInterval`)과 preview(`PROBE_EVERY`) 그대로.
    pub(crate) fn probe_every(self) -> Duration {
        Duration::from_secs(match self {
            Self::Stable => 86_400,
            Self::Preview => 3600,
        })
    }
}

/// appcast XML 에서 최신 항목의 `sparkle:version` 을 뽑는다. CI 가 최신 1건만
/// 쓰는 단순 피드(docs/appcast-win.xml)라 첫 매치가 곧 최신이다.
/// 실피드(release.sh/release.yml)는 `<sparkle:version>x</sparkle:version>`
/// 엘리먼트형 — 속성형(`sparkle:version="x"`)은 Sparkle 호환 폴백.
pub(crate) fn parse_appcast_version(xml: &str) -> Option<String> {
    const ELEM: &str = "<sparkle:version>";
    if let Some(i) = xml.find(ELEM) {
        let rest = &xml[i + ELEM.len()..];
        let v = rest[..rest.find('<')?].trim();
        return (!v.is_empty()).then(|| v.to_string());
    }
    const ATTR: &str = "sparkle:version=\"";
    let i = xml.find(ATTR)? + ATTR.len();
    let rest = &xml[i..];
    let v = &rest[..rest.find('"')?];
    (!v.is_empty()).then(|| v.to_string())
}

/// a 가 b 보다 새 버전인가 — 세그먼트별 숫자 비교("0.1.10" > "0.1.9").
/// 숫자가 아닌 세그먼트가 섞이면 보수적으로 false(오판 업데이트 알림 방지).
pub(crate) fn version_newer(a: &str, b: &str) -> bool {
    let parse = |s: &str| -> Option<Vec<u64>> { s.split('.').map(|p| p.parse().ok()).collect() };
    match (parse(a), parse(b)) {
        (Some(x), Some(y)) => x > y,
        _ => false,
    }
}

/// 이 실행이 업데이터를 켜도 되나. 검증 리그·격리 env·라이트 실행은 설치본을 건드리면 안 된다(맥 Sparkle 과 같은 판정).
#[cfg(any(windows, test))]
pub(crate) fn may_start(verification: bool, lite: bool, has: impl Fn(&str) -> bool) -> bool {
    !verification && !lite && !crate::version::isolated_environment(has)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_version_from_real_feed_element_form() {
        // release.sh/release.yml 이 실제로 뿜는 형식 — 0.1.11 까지 이 형식을
        // 속성형 파서가 못 읽어 업데이트 토스트가 영영 안 떴다(실기 확인).
        let xml = r#"<item><title>0.1.12</title>
            <sparkle:version>0.1.12</sparkle:version>
            <enclosure url="https://x/kasaterm-0.1.12-x86_64.msi"
                sparkle:os="windows" length="123"/></item>"#;
        assert_eq!(parse_appcast_version(xml).as_deref(), Some("0.1.12"));
        assert_eq!(parse_appcast_version("<rss></rss>"), None);
    }

    #[test]
    fn parses_version_from_appcast_enclosure_attr_fallback() {
        let xml = r#"<item><enclosure url="https://x/kasaterm-0.1.9-x86_64.msi"
            sparkle:version="0.1.9" sparkle:os="windows" length="123"/></item>"#;
        assert_eq!(parse_appcast_version(xml).as_deref(), Some("0.1.9"));
    }

    #[test]
    fn version_compare_is_numeric_per_segment() {
        assert!(version_newer("0.1.10", "0.1.9")); // 문자열 비교였다면 false
        assert!(version_newer("0.2.0", "0.1.9"));
        assert!(!version_newer("0.1.9", "0.1.9"));
        assert!(!version_newer("0.1.8", "0.1.9"));
        assert!(!version_newer("0.2.0-beta", "0.1.9")); // 비숫자 → 보수적 false
    }

    #[test]
    fn preview_devices_watch_their_own_feed_hourly_and_others_daily() {
        assert_eq!(Channel::Stable.feed(), "https://2rami.github.io/kasaterm/appcast-win.xml");
        assert_eq!(Channel::Preview.feed(), "https://2rami.github.io/kasaterm/appcast-win-preview.xml");
        assert_eq!((Channel::Preview.probe_every(), Channel::Stable.probe_every()),
                   (Duration::from_secs(3600), Duration::from_secs(86_400)));
        assert_eq!(Channel::of(&serde_json::json!({"update_channel":"preview", "automatic_update_on_quit":true})), Channel::Preview);
        assert_eq!(Channel::of(&serde_json::json!({"update_channel":"preview"})), Channel::Stable);
        // 릴리스 도구(tools/release)는 이 글자로 「이 판의 Windows 앱이 preview 피드를 안다」고 판정한다.
        assert!(include_str!("win_sparkle.rs").contains("appcast-win-preview.xml"));
    }

    #[test]
    fn the_updater_stays_off_in_verification_isolated_and_lite_runs() {
        assert!(may_start(false, false, |_| false));
        assert!(!may_start(true, false, |_| false));
        assert!(!may_start(false, true, |_| false));
        assert!(!may_start(false, false, |key| key == "KASATERM_SETTINGS_FILE"));
    }
}
