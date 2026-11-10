//! 프록시가 지나보내는 `/v1/messages` 응답의 한도 헤더(`anthropic-ratelimit-unified-*`)로 계정 사용량을
//! 실시간에 가깝게 맞춘다.
//!
//! `oauth/usage` 는 자주 치면 429 이고, 맞으면 그 토큰을 15분 쉬게 해 그동안 화면이 낡은 값이었다. 같은
//! 사실이 Claude Code 의 모든 요청 응답 헤더에 실려 온다(2026-11 실측: `-7d-utilization 0.92`, `-7d-reset`
//! epoch 초, `-7d-status allowed_warning`). 서버는 경고선을 넘은 창만 싣는다 — 실려 오지 않은 창(경고선 아래
//! 5시간 등)은 `oauth/usage` 값이 그대로 남는다.
//!
//! 관측은 요청의 Bearer 지문(`token_key`)으로 묶는다. 같은 지문을 쓰는 계정 조회만 그 값을 덧입히므로 다른
//! 계정의 숫자가 새지 않는다.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const PREFIX: &str = "anthropic-ratelimit-unified-";
/// 이보다 오래된 관측은 덧입히지 않는다 — 그 사이 창이 넘어갔을 수 있다.
const KEEP_FOR: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Window {
    pub(crate) pct: f64,
    pub(crate) resets_at: Option<u64>,
}

#[derive(Debug, Clone)]
struct Observation {
    at: Instant,
    windows: HashMap<String, Window>,
}

fn store() -> &'static Mutex<HashMap<u64, Observation>> {
    static STORE: std::sync::OnceLock<Mutex<HashMap<u64, Observation>>> = std::sync::OnceLock::new();
    STORE.get_or_init(Default::default)
}

static DIRTY: AtomicBool = AtomicBool::new(false);

/// 응답 헤더에서 창별 값을 꺼낸다. 창 이름은 헤더 가운데 토막(`5h`·`7d`)이고, 사용률이 없는
/// 묶음(`overage-status` 같은 계정 전체 표시)은 창이 아니라 버린다.
pub(crate) fn parse<'a>(headers: impl IntoIterator<Item = (&'a str, &'a str)>) -> HashMap<String, Window> {
    let mut pct: HashMap<String, f64> = HashMap::new();
    let mut reset: HashMap<String, u64> = HashMap::new();
    for (name, value) in headers {
        let name = name.to_ascii_lowercase();
        let Some(rest) = name.strip_prefix(PREFIX) else { continue };
        let Some((window, field)) = rest.rsplit_once('-') else { continue };
        let value = value.trim();
        match field {
            // 헤더는 0~1 비율이고, 사용량 응답(`limits[].percent`)은 0~100 이다.
            "utilization" => {
                if let Some(u) = value.parse::<f64>().ok().map(|u| u * 100.0).filter(|p| p.is_finite()) {
                    pct.insert(window.to_owned(), u);
                }
            }
            "reset" => {
                if let Ok(at) = value.parse::<u64>() {
                    reset.insert(window.to_owned(), at);
                }
            }
            _ => {}
        }
    }
    pct.into_iter()
        .map(|(window, pct)| {
            let resets_at = reset.get(&window).copied();
            (window, Window { pct, resets_at })
        })
        .collect()
}

/// 프록시가 `/v1/messages` 응답마다 부른다. `authorization` 은 실제로 보낸 값이다(401 재전송이면 바꾼 것).
///
/// 응답 하나가 그 순간 경고선을 넘은 창 전부를 싣으므로 창 묶음을 통째로 바꾼다 — 빠진 창은 경고선 아래로
/// 내려갔다는 뜻이라 옛 관측을 남기면 지난 값을 계속 덧입힌다.
pub fn record<'a>(authorization: Option<&str>, headers: impl IntoIterator<Item = (&'a str, &'a str)>) {
    let Some(token) = authorization
        .and_then(|a| a.strip_prefix("Bearer ").or_else(|| a.strip_prefix("bearer ")))
        .map(str::trim)
        .filter(|t| !t.is_empty())
    else {
        return;
    };
    let windows = parse(headers);
    if windows.is_empty() {
        return;
    }
    let Ok(mut store) = store().lock() else { return };
    let now = Instant::now();
    store.retain(|_, obs| now.duration_since(obs.at) < KEEP_FOR);
    store.insert(super::token_key(token), Observation { at: now, windows });
    DIRTY.store(true, Ordering::Release);
}

/// 새 관측이 들어왔는가 — 사용량 폴러가 잠을 건너뛰고 바로 다시 읽게 한다. 읽으면 내려간다.
pub fn take_dirty() -> bool {
    DIRTY.swap(false, Ordering::AcqRel)
}

fn latest(keys: &[u64]) -> Option<Observation> {
    let store = store().lock().ok()?;
    keys.iter()
        .filter_map(|k| store.get(k))
        .filter(|obs| obs.at.elapsed() < KEEP_FOR)
        .max_by_key(|obs| obs.at)
        .cloned()
}

/// 시험용 — 그 토큰으로 본 창의 사용률(0~100).
#[cfg(any(test, feature = "test-isolation"))]
pub fn observed_pct(token: &str, window: &str) -> Option<f64> {
    latest(&[super::token_key(token)])?.windows.get(window).map(|w| w.pct)
}

/// 그 지문들의 가장 새 관측 시각.
pub(crate) fn latest_at(keys: &[u64]) -> Option<Instant> {
    latest(keys).map(|obs| obs.at)
}

/// `usage`(oauth/usage 응답 꼴)에 관측을 덧입힌다. `fetched_at` 보다 새 관측만 쓴다 — 방금 받은
/// 상류 값이 관측보다 새로우면 그쪽이 맞다. 덧입혔으면 관측 시각을 돌려준다.
pub(crate) fn overlay(keys: &[u64], usage: &mut Value, fetched_at: Option<Instant>) -> Option<Instant> {
    let obs = latest(keys)?;
    if fetched_at.is_some_and(|fetched| fetched >= obs.at) {
        return None;
    }
    let mut applied = false;
    for (window, win) in &obs.windows {
        let (kind, group, legacy) = match window.as_str() {
            "5h" => ("session", "session", "five_hour"),
            "7d" => ("weekly_all", "weekly", "seven_day"),
            _ => continue,
        };
        apply(usage, kind, group, legacy, win);
        applied = true;
    }
    applied.then_some(obs.at)
}

fn apply(usage: &mut Value, kind: &str, group: &str, legacy: &str, win: &Window) {
    let resets_at = win.resets_at.map(iso_utc);
    // `limits[]` 가 없는 옛 응답에 배열을 새로 만들면 앱이 그 배열만 읽어 다른 창(`five_hour`)을 잃는다.
    if let Some(limits) = usage.get_mut("limits").and_then(Value::as_array_mut) {
        let entry = limits
            .iter_mut()
            .find(|e| e["kind"] == kind && e.get("scope").is_none_or(Value::is_null));
        match entry {
            Some(e) => {
                e["percent"] = json!(win.pct);
                if let Some(at) = &resets_at {
                    e["resets_at"] = json!(at);
                }
                e["is_active"] = json!(true);
            }
            None => limits.push(json!({
                "kind": kind,
                "group": group,
                "percent": win.pct,
                "resets_at": resets_at,
                "scope": null,
                "is_active": true,
            })),
        }
    }
    let Some(map) = usage.as_object_mut() else { return };
    let slot = map.entry(legacy.to_owned()).or_insert(Value::Null);
    if !slot.is_object() {
        *slot = json!({});
    }
    slot["utilization"] = json!(win.pct);
    if let Some(at) = resets_at {
        slot["resets_at"] = json!(at);
    }
}

/// epoch 초 → `2026-11-16T07:00:00+00:00`(사용량 응답의 `resets_at` 꼴).
fn iso_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // 하워드 히넌트의 civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}+00:00",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-11 실측 응답 헤더 그대로.
    fn measured() -> Vec<(&'static str, &'static str)> {
        vec![
            ("anthropic-ratelimit-unified-7d-reset", "1794812400"),
            ("anthropic-ratelimit-unified-7d-status", "allowed_warning"),
            ("anthropic-ratelimit-unified-7d-surpassed-threshold", "0.75"),
            ("anthropic-ratelimit-unified-7d-utilization", "0.92"),
            ("anthropic-ratelimit-unified-fallback-percentage", "0.5"),
            ("anthropic-ratelimit-unified-overage-disabled-reason", "org_level_disabled"),
            ("anthropic-ratelimit-unified-overage-status", "rejected"),
            ("anthropic-ratelimit-unified-representative-claim", "seven_day"),
            ("anthropic-ratelimit-unified-reset", "1794812400"),
            ("anthropic-ratelimit-unified-status", "allowed_warning"),
            ("content-type", "application/json"),
        ]
    }

    #[test]
    fn only_windows_with_a_utilization_are_kept() {
        let got = parse(measured());
        assert_eq!(got.len(), 1, "{got:?}");
        let week = &got["7d"];
        assert!((week.pct - 92.0).abs() < 1e-9);
        assert_eq!(week.resets_at, Some(1_794_812_400));
    }

    #[test]
    fn iso_matches_the_usage_response_shape() {
        assert_eq!(iso_utc(1_794_812_400), "2026-11-16T07:00:00+00:00");
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00+00:00");
        assert_eq!(iso_utc(951_782_400), "2000-02-29T00:00:00+00:00");
    }

    #[test]
    fn overlay_updates_the_matching_window_and_adds_a_missing_session() {
        let token = "tok-overlay-test";
        let mut headers = measured();
        headers.push(("anthropic-ratelimit-unified-5h-utilization", "0.4"));
        headers.push(("anthropic-ratelimit-unified-5h-reset", "1794330000"));
        record(Some(&format!("Bearer {token}")), headers);
        let key = super::super::token_key(token);
        let mut usage = json!({
            "five_hour": null,
            "seven_day": { "utilization": 90.0, "resets_at": "old" },
            "limits": [
                { "kind": "weekly_all", "group": "weekly", "percent": 90, "resets_at": "old", "scope": null },
                { "kind": "weekly_scoped", "group": "weekly", "percent": 0, "scope": { "model": { "display_name": "Fable" } } }
            ]
        });
        let fetched = Instant::now() - Duration::from_secs(120);
        assert!(overlay(&[key], &mut usage, Some(fetched)).is_some());
        assert_eq!(usage["limits"][0]["percent"], json!(92.0));
        assert_eq!(usage["limits"][0]["resets_at"], "2026-11-16T07:00:00+00:00");
        assert_eq!(usage["limits"][1]["percent"], json!(0), "a model-scoped window is not the 7d header");
        let session = usage["limits"].as_array().unwrap().iter().find(|e| e["kind"] == "session").expect("session added");
        assert_eq!(session["percent"], json!(40.0));
        assert_eq!(usage["five_hour"]["utilization"], json!(40.0));
        assert_eq!(usage["seven_day"]["utilization"], json!(92.0));
    }

    #[test]
    fn newer_upstream_value_or_another_token_is_left_alone() {
        record(Some("Bearer tok-newer-test"), measured());
        let key = super::super::token_key("tok-newer-test");
        let mut usage = json!({ "limits": [{ "kind": "weekly_all", "percent": 50, "scope": null }] });
        assert!(overlay(&[key], &mut usage, Some(Instant::now() + Duration::from_secs(1))).is_none());
        assert!(overlay(&[super::super::token_key("someone-else")], &mut usage, None).is_none());
        assert_eq!(usage["limits"][0]["percent"], json!(50));
    }

    #[test]
    fn legacy_shape_without_limits_stays_without_limits() {
        record(Some("Bearer tok-legacy-test"), measured());
        let key = super::super::token_key("tok-legacy-test");
        let mut usage = json!({ "five_hour": { "utilization": 10.0 } });
        assert!(overlay(&[key], &mut usage, None).is_some());
        assert!(usage.get("limits").is_none());
        assert_eq!(usage["seven_day"]["utilization"], json!(92.0));
        assert_eq!(usage["five_hour"]["utilization"], json!(10.0));
    }

    #[test]
    fn non_bearer_or_header_less_responses_record_nothing() {
        record(None, measured());
        record(Some("x-api-key sk"), measured());
        record(Some("Bearer tok-empty-test"), [("content-type", "text/event-stream")]);
        assert!(latest_at(&[super::super::token_key("tok-empty-test")]).is_none());
    }
}
