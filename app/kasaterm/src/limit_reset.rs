//! claude 한도 초기화권 — 잔량은 우리가 읽고, 쓰는 곳은 claude.ai 사용량 페이지다.
//!
//! 잔량은 usage 프록시(`/claude-usage`)가 `/api/oauth/usage?cedar_ember=1` 로 함께 받아
//! 온 `cedar_ember` 묶음에 있다(2026-09-23 실측, Opus 5.5 출시 기념 1장).
//!
//! 쓰는 길 셋 중 웹을 고른 이유(2026-09-23):
//! - claude 의 `/limit-reset` 은 기능 스위치 `tengu_cedar_ember` 뒤에 있고, 2.1.280 에서
//!   이 계정은 꺼져 있다. 그러면 같은 명령이 다른 갈래(세션 한도 초기화)로 빠져
//!   「A session-limit reset isn't available right now.」로 끝난다 — 리그에서 실제로
//!   pane 에 넣어 보고 확인했다.
//! - 쓰는 창구(`POST /api/organizations/{org}/reset_rate_limits`)를 직접 치지 않는다.
//!   되돌릴 수 없는 소모인데 요청 모양은 claude 내부 약속이라, 바뀌면 모르는 사이 한 장을
//!   날린다.
//! - claude.ai 설정의 사용량 페이지에는 공식 「무료로 초기화」 단추가 있다. 확인도 거기서
//!   한다.

use super::*;

const USAGE_PAGE: &str = "https://claude.ai/settings/usage";

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ResetGrant {
    /// 남은 장수 — 멈춘 것을 뺀 모든 grant 의 합.
    pub(crate) left: u32,
    /// 다음에 쓰일 grant 의 사용 기한(epoch 초).
    pub(crate) ends_at: Option<u64>,
    /// 지금 쓸 수 있나. 거짓이면 한도에 닿아야 쓸 수 있는 grant 다.
    pub(crate) usable_now: bool,
}

/// `eligible` 이 거짓이면 없는 것으로 친다. 서버는 요청이 claude CLI 에서 온 것이 아니면
/// `ineligible_reason: "surface"` 로 grant 를 비워 보내므로, 그 답을 「0장」으로 그리면
/// 가진 초기화권을 없다고 말하게 된다.
pub(crate) fn parse(usage: &serde_json::Value) -> Option<ResetGrant> {
    let block = usage.get("cedar_ember")?;
    if block.get("eligible").and_then(|v| v.as_bool()) != Some(true) {
        return None;
    }
    let left_of = |g: &serde_json::Value| g.get("resets_left").and_then(|v| v.as_u64()).unwrap_or(0);
    let grants: Vec<&serde_json::Value> = block
        .get("grants")?
        .as_array()?
        .iter()
        .filter(|g| g.get("paused").and_then(|v| v.as_bool()) != Some(true) && left_of(g) > 0)
        .collect();
    let left = grants.iter().map(|g| left_of(g)).sum::<u64>();
    if left == 0 {
        return None;
    }
    let next_id = block.get("next_grant_id").and_then(|v| v.as_str());
    let next = next_id
        .and_then(|id| grants.iter().find(|g| g.get("id").and_then(|v| v.as_str()) == Some(id)))
        .or_else(|| grants.first())?;
    Some(ResetGrant {
        left: left.min(u32::MAX as u64) as u32,
        ends_at: next.get("ends_at").and_then(|v| v.as_str()).and_then(crate::socket::rfc3339_epoch),
        usable_now: next.get("usable_now").and_then(|v| v.as_bool()) == Some(true),
    })
}

/// `초기화권 1장 · 29일 남음`. 기한은 날짜가 아니라 남은 시간이다 — 한도 창들이 전부
/// 남은 시간으로 말하는데 이 줄만 날짜면 같은 메뉴 안에서 셈법이 갈린다.
pub(crate) fn label(grant: &ResetGrant) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    label_at(grant, now)
}

fn label_at(grant: &ResetGrant, now: u64) -> String {
    let mut out = format!("초기화권 {}장", grant.left);
    if let Some(left) = grant.ends_at.map(|at| at.saturating_sub(now)).filter(|s| *s > 0) {
        out.push_str(&format!(" · {} 남음", crate::remaining_duration_label(left)));
    }
    if !grant.usable_now {
        out.push_str(" · 한도에 닿으면 사용");
    }
    out
}

/// 활성 claude 계정의 초기화권. 계정 id 와 함께 둬야 전환 직후 떠나온 계정의 장수를
/// 새 계정 것으로 그리지 않는다(`UsageBadge::account_dir` 와 같은 이유).
fn store() -> &'static std::sync::Mutex<Option<(String, Option<ResetGrant>)>> {
    static S: std::sync::OnceLock<std::sync::Mutex<Option<(String, Option<ResetGrant>)>>> =
        std::sync::OnceLock::new();
    S.get_or_init(Default::default)
}

/// 바뀌었으면 true — 폴러가 그때만 다시 그린다.
pub(crate) fn record(account: &str, usage: &serde_json::Value) -> bool {
    let next = Some((account.to_string(), parse(usage)));
    let Ok(mut g) = store().lock() else { return false };
    if *g == next {
        return false;
    }
    *g = next;
    true
}

pub(crate) fn current(account: &str) -> Option<ResetGrant> {
    let g = store().lock().ok()?;
    let (who, grant) = g.as_ref()?;
    (who == account).then(|| grant.clone()).flatten()
}

impl App {
    /// 브라우저에 로그인된 계정의 페이지가 열린다. kasaterm 의 활성 계정과 다를 수
    /// 있지만, 초기화권은 계정마다 따로라 그 페이지가 보여 주는 장수가 곧 그 계정 것이다.
    pub(crate) fn use_claude_limit_reset(&mut self) {
        crate::chrome::open_url_in_browser(USAGE_PAGE);
        self.set_toast("claude.ai 사용량 페이지를 열었어요 — 「무료로 초기화」를 누르면 써져요".into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(block: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "five_hour": { "utilization": 53.0 }, "cedar_ember": block })
    }

    /// 2026-09-23 실측 응답(claude CLI 의 User-Agent 로 물었을 때).
    #[test]
    fn 실측_응답에서_장수와_기한을_읽는다() {
        let v = usage(serde_json::json!({
            "eligible": true, "ineligible_reason": null, "at_limit": false, "exhausted": [],
            "grants": [{
                "id": "opus55-launch-promax-20260921",
                "resets_total": 1, "resets_left": 1,
                "starts_at": "2026-09-22T16:00:00+00:00", "ends_at": "2026-10-22T16:00:00+00:00",
                "clears": ["five_hour", "seven_day"], "paused": false,
                "usable_now": true, "use_requires_limit": false
            }],
            "next_grant_id": "opus55-launch-promax-20260921",
            "weekly_resets_at": "2026-09-28T07:00:00+00:00", "cooldown_until": null
        }));
        let got = parse(&v).expect("읽혀야 한다");
        assert_eq!(got, ResetGrant { left: 1, ends_at: Some(1_792_684_800), usable_now: true });
        assert_eq!(label_at(&got, 1_792_684_800 - 29 * 86400), "초기화권 1장 · 29일 남음");
    }

    /// CLI 가 아닌 곳에서 물으면 서버가 grant 를 비워 보낸다 — 「0장」이 아니라 모름이다.
    #[test]
    fn 자격_밖_응답은_없는_것으로_친다() {
        let v = usage(serde_json::json!({
            "eligible": false, "ineligible_reason": "surface", "grants": [], "next_grant_id": null
        }));
        assert!(parse(&v).is_none());
        assert!(parse(&serde_json::json!({ "five_hour": {} })).is_none());
    }

    #[test]
    fn 다_쓴_것과_멈춘_것은_세지_않는다() {
        let v = usage(serde_json::json!({
            "eligible": true,
            "grants": [
                { "id": "a", "resets_left": 0, "paused": false, "usable_now": true },
                { "id": "b", "resets_left": 2, "paused": true, "usable_now": true },
                { "id": "c", "resets_left": 1, "paused": false, "usable_now": false,
                  "ends_at": "2026-10-01T00:00:00Z" }
            ],
            "next_grant_id": "a"
        }));
        let got = parse(&v).expect("c 가 남는다");
        assert_eq!(got.left, 1);
        assert!(!got.usable_now, "next_grant_id 가 다 쓴 것이면 남은 grant 로 판정한다");
        assert!(label_at(&got, 0).ends_with("한도에 닿으면 사용"));
    }
}
