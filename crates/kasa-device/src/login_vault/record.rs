//! 항목 평문과 「어느 쪽이 이기나」 규칙. 관문은 이 평문을 못 보고, 판정은 기기가 봉인 안의 값으로만 한다 —
//! 관문이 봉인 밖에 붙이는 것(받은 시각·차례)은 시각 판정에 쓰지 않는다.

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

/// 쓰는 기기의 시계가 관문 시계보다 이만큼 넘게 앞서면 관문 시계 + 이만큼으로 깎아 봉인한다.
pub const MAX_SKEW_MS: u64 = 5 * 60 * 1000;
/// 묘비는 이만큼 지나면 걷는다. 그보다 오래 꺼져 있던 기기는 지운 항목을 되살릴 수 있다.
pub const TOMBSTONE_KEEP_MS: u64 = 180 * 24 * 60 * 60 * 1000;

/// 봉인 안의 평문. `changed_at_ms` 는 바꾼 기기가 [`stamp`] 로 정한 시각이다.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub host: String,
    pub username: String,
    #[serde(default)]
    pub password: Zeroizing<String>,
    pub changed_at_ms: u64,
    #[serde(default)]
    pub deleted: bool,
    pub device: String,
}

impl Record {
    /// 묘비에는 비밀번호를 싣지 않는다.
    pub fn tombstone(host: &str, username: &str, changed_at_ms: u64, device: &str) -> Self {
        Self {
            host: host.into(),
            username: username.into(),
            password: Zeroizing::default(),
            changed_at_ms,
            deleted: true,
            device: device.into(),
        }
    }

    pub fn version(&self) -> (u64, &str) {
        (self.changed_at_ms, &self.device)
    }
}

/// 제 변경에 찍을 시각. 기기 시계가 관문 시계보다 5분 넘게 앞서면 깎고, 이 기기가 이미 본 판보다는 늘 뒤다
/// (본 것을 고친 변경이 그것에 지지 않게). 깎은 값을 봉인하므로 받는 기기는 봉인 안의 값만 견준다.
pub fn stamp(local_ms: u64, relay_now_ms: u64, floor: u64) -> u64 {
    let capped = if relay_now_ms == 0 { local_ms } else { local_ms.min(relay_now_ms.saturating_add(MAX_SKEW_MS)) };
    capped.max(floor)
}

/// `(시각, 기기)` 순서 — 같은 시각이면 기기 이름으로 가려 두 기기가 같은 쪽을 고른다.
pub fn wins(a: (u64, &str), b: (u64, &str)) -> bool {
    a > b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn later_change_wins_and_ties_break_by_device() {
        assert!(wins((2, "a"), (1, "z")));
        assert!(!wins((1, "z"), (2, "a")));
        assert!(wins((5, "b"), (5, "a")));
        assert!(!wins((5, "a"), (5, "a")), "같은 것은 이기지 않는다");
    }

    #[test]
    fn a_clock_far_ahead_is_cut_before_sealing() {
        let relay = 1_000_000;
        assert_eq!(stamp(relay + 60_000, relay, 0), relay + 60_000, "5분 안은 그대로");
        assert_eq!(stamp(relay + 3_600_000, relay, 0), relay + MAX_SKEW_MS, "한 시간 앞선 시계는 깎는다");
        assert_eq!(stamp(relay - 10, relay, 0), relay - 10, "늦은 시계는 그대로");
        assert_eq!(stamp(10, relay, 500), 500, "본 판보다는 늘 뒤");
        assert_eq!(stamp(42, 0, 0), 42, "관문 시각이 없으면 그대로");
    }
}
