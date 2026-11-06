//! 배치 변경 알림. 다른 기기는 `/term/panes` 를 5초마다 물어와 새 방이 최대 5초
//! 뒤에야 떴다(2026-09-17 지적 「채팅 앱은 보내자마자 뜨잖아」). 원본이 배치를 바꿀
//! 때 번호를 올리고, 보는 쪽은 `/term/changes` 에 매달려 있다가 번호가 오르면 바로
//! 다시 읽는다. 통로(직통·관문)와 무관하게 폴링 주기가 사라진다.
//!
//! 학생 상태 전이(작업 중·기다림·쉼)도 같은 번호를 올린다 — 폰 목록이 「기다림」을
//! 5초 폴링 뒤에야 보던 것(2026-09-25 「갱신도 실시간 아니고」). 판정 틱을 도는 앱만
//! 올릴 수 있어서, 답에 `status` 로 그 사실을 싣는다. 모르는 판을 상대하는 폰은 폴링을
//! 늦추지 않는다.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use tokio::sync::watch;

fn chan() -> &'static watch::Sender<u64> {
    static CHAN: OnceLock<watch::Sender<u64>> = OnceLock::new();
    CHAN.get_or_init(|| watch::channel(1).0)
}

/// 배치·방 이름·자리 배정이 바뀌었을 때 부른다. 어느 스레드에서든 된다.
pub fn bump() {
    chan().send_modify(|epoch| *epoch += 1);
}

pub fn current() -> u64 {
    *chan().borrow()
}

static STATUS: AtomicBool = AtomicBool::new(false);

/// 상태 판정 틱이 부른다. 한 번이라도 불렸으면 이 서버의 번호는 상태 전이도 담는다.
pub fn status_tick(changed: bool) {
    STATUS.store(true, Ordering::Relaxed);
    if changed {
        bump();
    }
}

pub fn status_aware() -> bool {
    STATUS.load(Ordering::Relaxed)
}

/// `since` 보다 새 번호가 나올 때까지(최대 `timeout`) 기다렸다 현재 번호를 준다.
/// 이미 지났으면 바로 돌아온다.
pub async fn wait_past(since: u64, timeout: Duration) -> u64 {
    let mut rx = chan().subscribe();
    let _ = tokio::time::timeout(timeout, async {
        while *rx.borrow_and_update() <= since {
            if rx.changed().await.is_err() {
                break;
            }
        }
    })
    .await;
    current()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn wait_returns_at_once_when_already_past() {
        let now = current();
        let got = wait_past(now.saturating_sub(1), Duration::from_secs(5)).await;
        assert!(got >= now);
    }

    #[tokio::test]
    async fn bump_wakes_a_waiter() {
        let now = current();
        let waiter = tokio::spawn(wait_past(now, Duration::from_secs(5)));
        tokio::time::sleep(Duration::from_millis(20)).await;
        bump();
        let got = tokio::time::timeout(Duration::from_secs(1), waiter).await.unwrap().unwrap();
        assert!(got > now);
    }

    #[tokio::test]
    async fn a_status_change_wakes_a_waiter() {
        let now = current();
        let waiter = tokio::spawn(wait_past(now, Duration::from_secs(5)));
        tokio::time::sleep(Duration::from_millis(20)).await;
        status_tick(false);
        assert!(status_aware());
        status_tick(true);
        let got = tokio::time::timeout(Duration::from_secs(1), waiter).await.unwrap().unwrap();
        assert!(got > now);
    }

    #[tokio::test]
    async fn timeout_hands_back_the_current_number() {
        let now = current();
        let got = wait_past(now + 1_000_000, Duration::from_millis(30)).await;
        assert_eq!(got, current());
        assert!(got < now + 1_000_000);
    }
}
