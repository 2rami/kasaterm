use std::time::{Duration, Instant};

/// 일이 이만큼 멎어야 끝난 것으로 본다 — 턴 사이 잠깐 쉬거나 다른 학생의 쪽지를 기다리는 사이에 막음을
/// 풀었다 걸거나, 덮인 채 하던 일 가운데서 재우지 않게.
const QUIET_GRACE: Duration = Duration::from_secs(60);
const RELEASE_RETRY: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Phase {
    Idle,
    Armed { quiet_since: Option<Instant> },
    Away { since: Instant, quiet_since: Option<Instant> },
    Disarming { retry_at: Instant },
    Sleeping { retry_at: Instant },
}

impl Phase {
    pub(super) fn keeps_awake(self) -> bool {
        matches!(self, Self::Armed { .. } | Self::Away { .. })
    }

    pub(super) fn released(self) -> Self {
        // sleepnow 성공은 잠자기 요청 접수다. 덮인 채 다시 깨면 계속 재워야 한다.
        if matches!(self, Self::Disarming { .. }) { Self::Idle } else { self }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Sense {
    /// 켜져 있고, sudo 가 허락하고, 전원·열이 괜찮다.
    pub(super) allowed: bool,
    /// 이 기기 칸에서 학생이 일하거나, 사람이 신청한 이사가 예약·실행 중이다.
    pub(super) busy: bool,
    /// 덮였고 외부 모니터도 없다.
    pub(super) departed: bool,
    pub(super) cap: Duration,
    pub(super) now: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Act {
    Arm,
    Disarm,
    Sleep,
}

pub(super) fn step(phase: Phase, s: Sense) -> (Phase, Option<Act>) {
    let quiet = |since: Option<Instant>| if s.busy { None } else { Some(since.unwrap_or(s.now)) };
    let rested = |since: Option<Instant>| since.is_some_and(|t| s.now.duration_since(t) >= QUIET_GRACE);
    let sleep = || (Phase::Sleeping { retry_at: s.now + RELEASE_RETRY }, Some(Act::Sleep));
    let disarm = || (Phase::Disarming { retry_at: s.now + RELEASE_RETRY }, Some(Act::Disarm));
    match phase {
        Phase::Idle => {
            // 덮인 채 잠깐 깬 것(Power Nap)에 다시 걸면 가방 속에서 상한만큼 깨어 있게 된다.
            if s.allowed && s.busy && !s.departed {
                (Phase::Armed { quiet_since: None }, Some(Act::Arm))
            } else {
                (Phase::Idle, None)
            }
        }
        Phase::Armed { quiet_since } => {
            if s.departed {
                return if s.allowed {
                    (Phase::Away { since: s.now, quiet_since: quiet(quiet_since) }, None)
                } else {
                    sleep()
                };
            }
            if !s.allowed {
                return disarm();
            }
            let quiet_since = quiet(quiet_since);
            if rested(quiet_since) {
                disarm()
            } else {
                (Phase::Armed { quiet_since }, None)
            }
        }
        Phase::Away { since, quiet_since } => {
            let quiet_since = quiet(quiet_since);
            if !s.departed {
                return (Phase::Armed { quiet_since }, None);
            }
            if !s.allowed || s.now.duration_since(since) >= s.cap || rested(quiet_since) {
                sleep()
            } else {
                (Phase::Away { since, quiet_since }, None)
            }
        }
        Phase::Disarming { retry_at } => {
            if s.departed {
                sleep()
            } else if s.now >= retry_at {
                disarm()
            } else {
                (phase, None)
            }
        }
        Phase::Sleeping { retry_at } => {
            if !s.departed {
                disarm()
            } else if s.now >= retry_at {
                sleep()
            } else {
                (phase, None)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sense(now: Instant) -> Sense {
        Sense { allowed: true, busy: false, departed: false, cap: Duration::from_secs(30 * 60), now }
    }

    #[test]
    fn arms_only_when_work_needs_it_and_sudo_allows() {
        let t = Instant::now();
        assert_eq!(step(Phase::Idle, sense(t)), (Phase::Idle, None));
        let busy = Sense { busy: true, ..sense(t) };
        assert_eq!(step(Phase::Idle, busy), (Phase::Armed { quiet_since: None }, Some(Act::Arm)));
        assert_eq!(step(Phase::Idle, Sense { allowed: false, ..busy }), (Phase::Idle, None));
    }

    #[test]
    fn a_dark_wake_with_the_lid_closed_does_not_arm_again() {
        let s = Sense { busy: true, departed: true, ..sense(Instant::now()) };
        assert_eq!(step(Phase::Idle, s), (Phase::Idle, None));
    }

    #[test]
    fn a_short_pause_between_turns_keeps_the_mac_awake() {
        let t = Instant::now();
        let (p, act) = step(Phase::Armed { quiet_since: None }, sense(t));
        assert_eq!(act, None);
        let (p, act) = step(p, sense(t + QUIET_GRACE - Duration::from_secs(1)));
        assert_eq!(act, None);
        let (p, _) = step(p, Sense { busy: true, ..sense(t + QUIET_GRACE) });
        assert_eq!(p, Phase::Armed { quiet_since: None });
        let (p, _) = step(p, sense(t + QUIET_GRACE * 2));
        let at = t + QUIET_GRACE * 3 + Duration::from_secs(1);
        let (p, act) = step(p, sense(at));
        assert_eq!((p, act), (Phase::Disarming { retry_at: at + RELEASE_RETRY }, Some(Act::Disarm)));
        assert_eq!(p.released(), Phase::Idle);
    }

    #[test]
    fn closing_the_lid_waits_for_the_work_to_finish_then_sleeps() {
        let t = Instant::now();
        let closed = |d: u64, busy| Sense { departed: true, busy, ..sense(t + Duration::from_secs(d)) };
        let (p, act) = step(Phase::Armed { quiet_since: None }, closed(0, true));
        assert_eq!((p, act), (Phase::Away { since: t, quiet_since: None }, None), "덮여도 옮기지 않고 깨어 있는다");
        let (p, act) = step(p, closed(600, true));
        assert_eq!(act, None, "일하는 동안은 계속 깨어 있다");
        let (p, act) = step(p, closed(700, false));
        assert_eq!(act, None, "턴이 막 끝났으면 다음 쪽지를 잠깐 기다린다");
        let (p, act) = step(p, closed(730, true));
        assert_eq!((p, act), (Phase::Away { since: t, quiet_since: None }, None), "그 사이 다시 일하면 처음부터 기다린다");
        let (p, _) = step(p, closed(800, false));
        let at = t + Duration::from_secs(800) + QUIET_GRACE;
        assert_eq!(step(p, closed(800 + QUIET_GRACE.as_secs(), false)), (Phase::Sleeping { retry_at: at + RELEASE_RETRY }, Some(Act::Sleep)));
    }

    #[test]
    fn work_that_already_ended_before_the_lid_closed_sleeps_after_the_rest_of_the_pause() {
        let t = Instant::now();
        let (p, _) = step(Phase::Armed { quiet_since: None }, sense(t));
        let (p, act) = step(p, Sense { departed: true, ..sense(t + Duration::from_secs(50)) });
        assert_eq!(act, None);
        assert_eq!(step(p, Sense { departed: true, ..sense(t + QUIET_GRACE) }), (Phase::Sleeping { retry_at: t + QUIET_GRACE + RELEASE_RETRY }, Some(Act::Sleep)));
    }

    #[test]
    fn opening_the_lid_keeps_the_block_and_stops_counting_toward_sleep() {
        let t = Instant::now();
        let away = Phase::Away { since: t, quiet_since: None };
        assert_eq!(step(away, Sense { busy: true, ..sense(t) }), (Phase::Armed { quiet_since: None }, None));
    }

    #[test]
    fn the_cap_sleeps_even_while_students_still_work() {
        let t = Instant::now();
        let away = Phase::Away { since: t, quiet_since: None };
        let at = |d: Duration| Sense { departed: true, busy: true, ..sense(t + d) };
        let cap = Duration::from_secs(30 * 60);
        assert_eq!(step(away, at(cap - Duration::from_secs(1))).1, None);
        assert_eq!(step(away, at(cap)), (Phase::Sleeping { retry_at: t + cap + RELEASE_RETRY }, Some(Act::Sleep)));
    }

    #[test]
    fn low_battery_or_heat_at_the_lid_goes_straight_to_sleep() {
        let t = Instant::now();
        let s = Sense { allowed: false, departed: true, busy: true, ..sense(t) };
        let expected = (Phase::Sleeping { retry_at: t + RELEASE_RETRY }, Some(Act::Sleep));
        assert_eq!(step(Phase::Armed { quiet_since: None }, s), expected);
        assert_eq!(step(Phase::Away { since: t, quiet_since: None }, s), expected);
    }

    #[test]
    fn a_sleep_request_keeps_watching_even_after_success() {
        let t = Instant::now();
        let s = Sense { departed: true, busy: true, allowed: false, ..sense(t) };
        let (p, act) = step(Phase::Away { since: t, quiet_since: None }, s);
        assert_eq!(act, Some(Act::Sleep));
        assert_eq!(p.released(), p);
        assert!(!p.keeps_awake(), "감시 박동을 갱신해 실패를 가리면 안 된다");
        assert_eq!(step(p, Sense { now: t + RELEASE_RETRY - Duration::from_secs(1), ..s }), (p, None));
        let (p, act) = step(p, Sense { now: t + RELEASE_RETRY, ..s });
        assert_eq!(act, Some(Act::Sleep), "실패하거나 다시 깨면 일이 새로 들어와도 재운다");
        assert_eq!(step(p, Sense { now: t + RELEASE_RETRY * 2, ..s }).1, Some(Act::Sleep));
    }

    #[test]
    fn opening_after_sleep_releases_before_accepting_new_work() {
        let t = Instant::now();
        let sleeping = Phase::Sleeping { retry_at: t + RELEASE_RETRY };
        let s = Sense { busy: true, ..sense(t) };
        let (p, act) = step(sleeping, s);
        assert_eq!(act, Some(Act::Disarm));
        assert_eq!(p.released(), Phase::Idle);
        assert_eq!(step(p.released(), s).1, Some(Act::Arm));
    }

    #[test]
    fn a_failed_disarm_is_retried_without_refreshing_the_watchdog() {
        let t = Instant::now();
        let s = Sense { allowed: false, ..sense(t) };
        let (p, act) = step(Phase::Armed { quiet_since: None }, s);
        assert_eq!(act, Some(Act::Disarm));
        assert!(!p.keeps_awake());
        assert_eq!(step(p, Sense { now: t + Duration::from_secs(1), ..s }), (p, None));
        assert_eq!(step(p, Sense { now: t + RELEASE_RETRY, ..s }).1, Some(Act::Disarm));
        assert_eq!(step(p, Sense { departed: true, ..s }).1, Some(Act::Sleep));
    }
}
