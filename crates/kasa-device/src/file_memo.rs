//! 자주 묻는 작은 설정 파일을 바뀔 때만 다시 읽는다. 메인 스레드가 장마다 로그인(`device.json`)·관문
//! (`mobile-users.json`)을 물으면서 그때마다 파일을 열어 읽고 파싱했다 — 실사용 앱 표본에서 메인 스레드가 일하는 시간의 약 9%.
//!
//! 마지막으로 확인한 지 [`CHECK_EVERY`] 안이면 파일 시스템에 묻지도 않고, 그 뒤에는 수정 시각·크기가 그대로이고
//! 마지막으로 읽은 지 [`MAX_AGE`] 가 안 됐으면 지난 값을 쓴다. Windows 는 수정 시각을 보는 것(`metadata`)도 핸들을
//! 열었다 닫아 읽기의 절반쯤 든다. 로그아웃·계정 바뀜을 막는 자리라: 이 프로세스가 그 파일을 쓰거나 지우면
//! [`FileMemo::forget`] 으로 바로 버리고, 다른 프로세스가 쓴 것은 [`CHECK_EVERY`] 안에 알아챈다.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

/// 이만큼 안에는 파일이 그대로인지 다시 보지 않는다 — 다른 프로세스가 쓴 것을 늦게 알 수 있는 최대 시간.
const CHECK_EVERY: Duration = Duration::from_millis(250);

/// 수정 시각·크기가 같아도 이만큼 지나면 다시 읽는다 — 같은 시각(Windows 는 수 ms 단위)에 같은 크기로 두 번
/// 쓰인 드문 경우도 이 안에는 따라간다.
const MAX_AGE: Duration = Duration::from_secs(2);

/// 파일이 그대로인지 가르는 표식. 없는 파일은 `None`.
type Mark = Option<(Option<SystemTime>, u64)>;

fn mark_of(path: &Path) -> Mark {
    std::fs::metadata(path).ok().map(|m| (m.modified().ok(), m.len()))
}

struct Entry<T> {
    path: PathBuf,
    mark: Mark,
    checked_at: Instant,
    read_at: Instant,
    value: T,
}

pub(crate) struct FileMemo<T> {
    slot: Mutex<Option<Entry<T>>>,
    check_every: Duration,
    max_age: Duration,
}

impl<T: Clone> FileMemo<T> {
    pub(crate) const fn new() -> Self {
        Self { slot: Mutex::new(None), check_every: CHECK_EVERY, max_age: MAX_AGE }
    }

    /// `path` 를 `load` 로 읽은 값. 파일이 그대로면 지난 값이다.
    pub(crate) fn get(&self, path: &Path, load: impl FnOnce(&Path) -> T) -> T {
        if let Ok(mut slot) = self.slot.lock() {
            if let Some(entry) = slot.as_mut().filter(|e| e.path == path) {
                if entry.checked_at.elapsed() < self.check_every {
                    return entry.value.clone();
                }
            }
        }
        // 읽기 전에 표식을 뜬다 — 읽는 사이 바뀌었으면 다음 확인이 새 표식과 어긋나 다시 읽는다.
        let mark = mark_of(path);
        if let Ok(mut slot) = self.slot.lock() {
            if let Some(entry) = slot.as_mut().filter(|e| e.path == path && e.mark == mark && e.read_at.elapsed() < self.max_age) {
                entry.checked_at = Instant::now();
                return entry.value.clone();
            }
        }
        let value = load(path);
        if let Ok(mut slot) = self.slot.lock() {
            let now = Instant::now();
            *slot = Some(Entry { path: path.to_path_buf(), mark, checked_at: now, read_at: now, value: value.clone() });
        }
        value
    }

    /// 이 프로세스가 그 파일을 쓰거나 지웠다 — 다음 `get` 은 다시 읽는다.
    pub(crate) fn forget(&self) {
        if let Ok(mut slot) = self.slot.lock() {
            *slot = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kasatest-file-memo-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("value.json")
    }

    #[test]
    fn an_unchanged_file_is_read_once_and_a_changed_one_again() {
        let path = temp("changed");
        std::fs::write(&path, "one").unwrap();
        let memo = FileMemo { slot: Mutex::new(None), check_every: Duration::ZERO, max_age: MAX_AGE };
        let loads = Cell::new(0);
        let read = |p: &Path| {
            loads.set(loads.get() + 1);
            std::fs::read_to_string(p).unwrap_or_default()
        };
        assert_eq!(memo.get(&path, read), "one");
        assert_eq!(memo.get(&path, read), "one");
        assert_eq!(loads.get(), 1, "그대로인 파일은 다시 안 읽는다");
        std::fs::write(&path, "three").unwrap();
        assert_eq!(memo.get(&path, read), "three", "크기가 바뀌면 다시 읽는다");
        std::fs::remove_file(&path).unwrap();
        assert_eq!(memo.get(&path, read), "", "지워진 것도 알아챈다");
        assert_eq!(loads.get(), 3);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_write_by_this_process_or_an_old_read_is_not_trusted() {
        let path = temp("forget");
        std::fs::write(&path, "same").unwrap();
        let memo = FileMemo::new();
        let loads = Cell::new(0);
        let read = |_: &Path| {
            loads.set(loads.get() + 1);
            loads.get()
        };
        memo.get(&path, read);
        memo.forget();
        memo.get(&path, read);
        assert_eq!(loads.get(), 2, "forget 뒤에는 다시 읽는다");
        let quick = FileMemo { slot: Mutex::new(None), check_every: Duration::ZERO, max_age: Duration::ZERO };
        quick.get(&path, read);
        quick.get(&path, read);
        assert_eq!(loads.get(), 4, "오래된 값은 표식이 같아도 다시 읽는다");
        let other = temp("other");
        std::fs::write(&other, "same").unwrap();
        memo.get(&other, read);
        assert_eq!(loads.get(), 5, "다른 파일은 따로 읽는다");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        let _ = std::fs::remove_dir_all(other.parent().unwrap());
    }

    #[test]
    fn a_change_by_another_process_is_noticed_after_the_check_window() {
        let path = temp("window");
        std::fs::write(&path, "one").unwrap();
        let memo = FileMemo { slot: Mutex::new(None), check_every: Duration::from_millis(80), max_age: MAX_AGE };
        let read = |p: &Path| std::fs::read_to_string(p).unwrap_or_default();
        assert_eq!(memo.get(&path, read), "one");
        std::fs::write(&path, "three").unwrap();
        assert_eq!(memo.get(&path, read), "one", "확인 창 안에서는 파일 시스템에 묻지 않는다");
        std::thread::sleep(Duration::from_millis(120));
        assert_eq!(memo.get(&path, read), "three", "창이 지나면 알아챈다");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// 한 번 묻는 값 — 손으로 돌린다(`cargo test -p kasa-device --lib file_memo -- --ignored --nocapture`).
    #[test]
    #[ignore]
    fn cost_of_a_read_against_a_memo_hit() {
        let path = temp("cost");
        std::fs::write(&path, r#"{"relay":"https://kasaterm.example","account":"kasatest","device_id":"d","token":"t"}"#).unwrap();
        let parse = |p: &Path| std::fs::read_to_string(p).ok().and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
        let n = 2000;
        let t = Instant::now();
        for _ in 0..n {
            std::hint::black_box(parse(&path));
        }
        let read = t.elapsed() / n;
        let memo = FileMemo::new();
        let t = Instant::now();
        for _ in 0..n {
            std::hint::black_box(memo.get(&path, parse));
        }
        let hit = t.elapsed() / n;
        eprintln!("read+parse {read:?} · memo {hit:?} per call");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
