//! 메인 스레드가 한 바퀴에 오래 묶이면 감시 스레드가 그 순간의 미니덤프를 남긴다(Windows).
//!
//! 2026-11-13 Windows 에서 미러 칸 클릭 한 번에 앱이 10분 멈췄는데(`[stall] 605897ms (click)`),
//! `[stall]` 줄은 멈춘 길이만 말하고 어디서 멈췄는지는 말하지 않아 코드만 읽어서는 못 가렸다.
//! 덤프는 멈춘 **중에** 떠야 쓸모가 있어서 바퀴가 끝난 뒤에 재는 `turn_probe` 로는 못 한다.
//! 풀이는 그 판의 PDB 로 한다(미니 `~/.local/state/kasaterm-preview/symbols/<태그>/`).

pub(crate) fn turn_opened() {
    #[cfg(windows)]
    imp::turn_opened();
}

pub(crate) fn turn_closed() {
    #[cfg(windows)]
    imp::turn_closed();
}

#[cfg(windows)]
mod imp {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Once, OnceLock};
    use std::time::{Duration, Instant};

    /// 창 끌기·크기 조절 같은 모달 루프도 바퀴를 늘리지만 사람이 10초씩 쥐는 일은 드물다.
    const STALL_DUMP: Duration = Duration::from_secs(10);
    /// 덤프 하나가 수 MB 라 오래 멈추는 판이 디스크를 채우지 않게 최근 것만 남긴다.
    const KEPT: usize = 5;

    static EPOCH: OnceLock<Instant> = OnceLock::new();
    /// 지금 열린 바퀴가 시작된 때(EPOCH 기준 ms + 1). 0 이면 바퀴 밖.
    static OPEN_AT: AtomicU64 = AtomicU64::new(0);
    /// 바퀴마다 하나씩 — 한 번 멈춘 바퀴에서 덤프를 거듭 뜨지 않게 감시 쪽이 기억한다.
    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn now_ms() -> u64 {
        EPOCH.get_or_init(Instant::now).elapsed().as_millis() as u64
    }

    pub(super) fn turn_opened() {
        SEQ.fetch_add(1, Ordering::Relaxed);
        OPEN_AT.store(now_ms() + 1, Ordering::Relaxed);
        watch();
        injected_stall();
    }

    pub(super) fn turn_closed() {
        OPEN_AT.store(0, Ordering::Relaxed);
    }

    /// 덤프를 뜰 바퀴면 그 바퀴가 묶인 시간. 이미 뜬 바퀴(`dumped`)는 다시 안 뜬다.
    fn held_too_long(open_at: u64, now: u64, seq: u64, dumped: u64, limit: Duration) -> Option<Duration> {
        if open_at == 0 || seq == dumped {
            return None;
        }
        let held = Duration::from_millis(now.saturating_sub(open_at - 1));
        (held >= limit).then_some(held)
    }

    fn limit() -> Duration {
        std::env::var("KASATERM_STALL_DUMP_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .map(Duration::from_millis)
            .unwrap_or(STALL_DUMP)
    }

    /// 검증 실행은 리그 자신의 임시 폴더에 — 사람 앱의 덤프와 섞이면 어느 앱이 멈췄는지 헷갈린다.
    fn dump_dir() -> Option<PathBuf> {
        if crate::verification_run() {
            return Some(std::env::temp_dir().join("kasaterm-stall-dumps"));
        }
        std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join(r"kasaterm\stall-dumps"))
    }

    fn watch() {
        static STARTED: Once = Once::new();
        STARTED.call_once(|| {
            let Some(dir) = dump_dir() else { return };
            let limit = limit();
            let _ = std::thread::Builder::new().name("stall-watch".into()).spawn(move || {
                let mut dumped = 0;
                loop {
                    std::thread::sleep(Duration::from_millis(500));
                    let seq = SEQ.load(Ordering::Relaxed);
                    let Some(held) = held_too_long(OPEN_AT.load(Ordering::Relaxed), now_ms(), seq, dumped, limit)
                    else {
                        continue;
                    };
                    dumped = seq;
                    match write(&dir, held) {
                        Ok(path) => {
                            eprintln!("[stall-dump] main thread held {}ms → {}", held.as_millis(), path.display())
                        }
                        Err(error) => eprintln!("[stall-dump] failed: {error}"),
                    }
                }
            });
        });
    }

    fn write(dir: &Path, held: Duration) -> std::io::Result<PathBuf> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::Diagnostics::Debug::{
            MiniDumpWithThreadInfo, MiniDumpWithUnloadedModules, MiniDumpWriteDump,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        std::fs::create_dir_all(dir)?;
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
        let path = dir.join(format!("stall-{secs}-pid{}-{}s.dmp", std::process::id(), held.as_secs()));
        let file = std::fs::File::create(&path)?;
        // 기본 덤프에 스레드 스택이 든다. 스레드 정보·내린 모듈은 풀이 쪽이 스레드와 판을 가리는 데 쓴다.
        let kind = MiniDumpWithThreadInfo | MiniDumpWithUnloadedModules;
        let ok = unsafe {
            MiniDumpWriteDump(
                GetCurrentProcess(),
                std::process::id(),
                file.as_raw_handle(),
                kind,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        if ok == 0 {
            let error = std::io::Error::last_os_error();
            drop(file);
            let _ = std::fs::remove_file(&path);
            return Err(error);
        }
        prune(dir);
        Ok(path)
    }

    fn prune(dir: &Path) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut dumps: Vec<_> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("stall-") && n.ends_with(".dmp"))
            })
            .collect();
        dumps.sort();
        let extra = dumps.len().saturating_sub(KEPT);
        for old in &dumps[..extra] {
            let _ = std::fs::remove_file(old);
        }
    }

    /// 검증 실행에서만: 시작 몇 초 뒤 한 바퀴를 일부러 붙잡아 감시가 실제로 덤프를 뜨는지 본다.
    fn injected_stall() {
        static FIRED: AtomicBool = AtomicBool::new(false);
        static STALL: OnceLock<Option<Duration>> = OnceLock::new();
        let Some(stall) = *STALL.get_or_init(|| {
            crate::verification_run()
                .then(|| std::env::var("KASATERM_TEST_MAIN_STALL_MS").ok()?.parse().ok().map(Duration::from_millis))
                .flatten()
        }) else {
            return;
        };
        if now_ms() < 5000 || FIRED.swap(true, Ordering::Relaxed) {
            return;
        }
        eprintln!("[stall-dump] test: holding the main thread {}ms", stall.as_millis());
        std::thread::sleep(stall);
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn dumps_once_per_turn_and_only_past_the_limit() {
            let limit = Duration::from_secs(10);
            assert_eq!(held_too_long(0, 50_000, 3, 0, limit), None, "no open turn");
            assert_eq!(held_too_long(1_001, 10_999, 3, 0, limit), None, "still under the limit");
            assert_eq!(held_too_long(1_001, 11_000, 3, 0, limit), Some(Duration::from_secs(10)));
            assert_eq!(held_too_long(1_001, 600_000, 3, 3, limit), None, "this turn was already dumped");
            assert_eq!(held_too_long(1_001, 600_000, 4, 3, limit).map(|d| d.as_secs()), Some(599));
        }

        #[test]
        fn writes_a_readable_minidump_and_keeps_only_recent_ones() {
            let dir = std::env::temp_dir().join(format!("kasaterm-stall-dump-test-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            for i in 0..KEPT + 2 {
                std::fs::write(dir.join(format!("stall-100000000{i}-pid1-10s.dmp")), b"old").unwrap();
            }
            let path = write(&dir, Duration::from_secs(12)).unwrap();
            let bytes = std::fs::read(&path).unwrap();
            assert_eq!(&bytes[..4], b"MDMP", "minidump signature");
            assert!(bytes.len() > 4096, "a dump with thread stacks, got {} bytes", bytes.len());
            assert_eq!(std::fs::read_dir(&dir).unwrap().count(), KEPT, "older dumps are pruned");
            assert!(path.exists(), "the newest dump survives pruning");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
