//! 물리 메모리 압박 — 하단바 사용량 위젯의 「맥북 재시작 권장」 근거
//! (2026-08-27 지시: 「WIRED 이거 많이 쌓여서 렉심해지면 알려주게하자 카사텀
//! 사용량 위젯통해서」).
//!
//! **왜 하필 wired 인가.** macOS 의 wired 는 커널이 페이지아웃할 수 **없는**
//! 메모리다. 앱을 닫아도, 캐시를 비워도 안 돌아온다 — 커널·드라이버가 쥔
//! 것이라 회수 경로가 재부팅뿐이다. 그래서 「껐다 켜면 나아진다」에 정확히
//! 대응하는 유일한 지표다. 반대로 RSS 합(옆에 이미 있는 값)은 앱을 닫으면
//! 돌아오므로, 그게 아무리 커도 재시작의 근거가 못 된다.
//!
//! `sample_process_tree_usage`(input.rs)와 나란히 5초 폴로 돈다. 여기는
//! 서브프로세스가 없다 — mach 호출 하나와 sysctl 셋이라 마이크로초 단위다.

/// 한 번 잰 값. 전부 바이트.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MemSample {
    /// 페이지아웃 불가 — 이 값이 이 모듈의 존재 이유다.
    pub(crate) wired: u64,
    pub(crate) total: u64,
    /// 압축기가 쥔 물리 페이지. **판정에는 안 쓰고 표시만 한다** — 실측
    /// (2026-08-27, 36G 기계)에서 압축기가 9.7G 를 쥐었는데도 커널 압박은
    /// normal 이고 스왑은 0 이었다. 압축이 잘 돌고 있다는 뜻이라 그 자체로는
    /// 나쁜 신호가 아니다.
    pub(crate) compressed: u64,
    pub(crate) swap_used: u64,
    /// 커널 자신의 판정: 1 normal · 2 warning · 4 critical. 추정이 아니라
    /// 사실이라 임계 비율보다 먼저 본다.
    pub(crate) pressure: u32,
}

/// 무엇을 권할지.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Advice {
    Ok,
    /// 아직 돌아가지만 앱 자리가 눈에 띄게 줄었다.
    Watch,
    /// 재시작 말고는 회수 경로가 없는 구간.
    Restart,
}

/// 주의 임계(wired / 물리 RAM, %).
///
/// 정상 wired 는 물리 RAM 의 10~20% 다(실측 2026-08-27: 36G 기계에서 학생
/// 열몇이 도는 중에 4.4G = 12%). 30% 는 아직 도는 구간이지만 정상 범위를
/// 확실히 벗어난 자리라 여기서 한 번 눈에 띄게 한다.
const WATCH_PCT: f32 = 30.0;
/// 권장 임계. 40% 면 물리 RAM 의 오분의 이가 회수 불가라, 남은 자리를 압축과
/// 스왑이 나눠 쓰기 시작한다 — 체감이 무거워지는 지점이다.
const RESTART_PCT: f32 = 40.0;
/// 스왑 보조 임계. 커널 압박이 아직 normal 이어도 이만큼 나갔으면 이미
/// 디스크를 물고 도는 중이다. 높게 잡은 것은 오탐 방지다 — macOS 는 여유가
/// 있어도 스왑을 조금씩 쓴다.
const SWAP_LIMIT: u64 = 8 * 1024 * 1024 * 1024;

/// 임계는 실측으로 조정할 값이라 env 로 덮을 수 있게 둔다. 위 상수는 이 기계
/// 한 대의 표본에서 나온 추정이고, RAM 크기나 쓰는 앱 구성이 다르면 어긋난다.
fn pct_env(key: &str, default: f32) -> f32 {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

impl MemSample {
    pub(crate) fn wired_pct(&self) -> f32 {
        if self.total == 0 {
            return 0.0;
        }
        self.wired as f32 / self.total as f32 * 100.0
    }

    pub(crate) fn advice(&self) -> Advice {
        let pct = self.wired_pct();
        // 커널 판정이 먼저다 — warning 이상은 커널이 직접 「자리가 없다」고
        // 말하는 것이라, 비율이 임계 아래여도 그쪽이 맞다.
        if self.pressure >= 2
            || pct >= pct_env("KASATERM_MEM_RESTART_PCT", RESTART_PCT)
            || self.swap_used >= SWAP_LIMIT
        {
            Advice::Restart
        } else if pct >= pct_env("KASATERM_MEM_WATCH_PCT", WATCH_PCT) {
            Advice::Watch
        } else {
            Advice::Ok
        }
    }

    /// 왜 권하는지 한 줄. 토스트와 팝오버가 같은 문장을 쓴다 — 두 자리가 서로
    /// 다른 이유를 대면 어느 쪽이 진짜인지 알 수가 없다.
    pub(crate) fn reason(&self) -> String {
        let gb = |b: u64| b as f32 / (1024.0 * 1024.0 * 1024.0);
        if self.pressure >= 4 {
            "메모리 압박 심각".to_string()
        } else if self.pressure >= 2 {
            "메모리 압박 경고".to_string()
        } else if self.swap_used >= SWAP_LIMIT {
            format!("스왑 {:.1}G", gb(self.swap_used))
        } else {
            format!("wired {:.0}% · {:.1}G", self.wired_pct(), gb(self.wired))
        }
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn sample() -> Option<MemSample> {
    // SAFETY: 전부 읽기 전용 조회다. mach 쪽은 우리가 잡은 버퍼와 그 길이를
    // 함께 넘기고 반환 코드를 검사하며, sysctl 쪽은 `sysctl_scalar` 가 넘긴
    // 타입과 커널이 돌려준 길이가 같을 때만 값을 쓴다.
    unsafe {
        let mut vm: libc::vm_statistics64 = std::mem::zeroed();
        let mut cnt = libc::HOST_VM_INFO64_COUNT;
        let rc = libc::host_statistics64(
            libc::mach_host_self(),
            libc::HOST_VM_INFO64,
            std::ptr::addr_of_mut!(vm).cast(),
            &mut cnt,
        );
        if rc != libc::KERN_SUCCESS {
            return None;
        }
        // `vm_statistics64` 는 `repr(packed)` 라 필드를 참조로 잡으면 정렬이
        // 안 맞는다. 값으로 복사해 쓴다.
        let wire_count = vm.wire_count as u64;
        let compressor_page_count = vm.compressor_page_count as u64;

        let page = sysctl_scalar::<u32>("hw.pagesize")? as u64;
        let total = sysctl_scalar::<u64>("hw.memsize")?;
        // 압박 레벨은 없는 커널도 있을 수 있으니 못 읽으면 normal 로 둔다 —
        // 이 하나가 없다고 wired 판정까지 버릴 이유는 없다.
        let pressure = sysctl_scalar::<u32>("kern.memorystatus_vm_pressure_level").unwrap_or(1);
        let swap_used = sysctl_scalar::<libc::xsw_usage>("vm.swapusage")
            .map(|s| s.xsu_used)
            .unwrap_or(0);

        Some(MemSample {
            wired: wire_count * page,
            total,
            compressed: compressor_page_count * page,
            swap_used,
            pressure,
        })
    }
}

/// 고정 크기 sysctl 값 하나. 커널이 돌려준 길이가 `T` 와 다르면 버린다 —
/// 이름을 잘못 짚었거나 커널 구조가 바뀐 경우라, 그 바이트를 `T` 로 읽으면
/// 조용히 엉뚱한 숫자가 나온다.
#[cfg(target_os = "macos")]
unsafe fn sysctl_scalar<T: Copy>(name: &str) -> Option<T> {
    let c = std::ffi::CString::new(name).ok()?;
    let mut out: T = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<T>();
    let rc = unsafe {
        libc::sysctlbyname(
            c.as_ptr(),
            std::ptr::addr_of_mut!(out).cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (rc == 0 && len == std::mem::size_of::<T>()).then_some(out)
}

/// macOS 밖에서는 재지 않는다. Windows 에도 비슷한 개념(비페이지드 풀)이
/// 있지만 임계의 근거가 되는 실측이 없어, 숫자를 지어내느니 위젯에서 통째로
/// 빼는 편이 낫다.
#[cfg(not(target_os = "macos"))]
pub(crate) fn sample() -> Option<MemSample> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(wired_gb: f32, pressure: u32, swap_gb: f32) -> MemSample {
        let g = 1024.0 * 1024.0 * 1024.0;
        MemSample {
            wired: (wired_gb * g) as u64,
            total: (36.0 * g) as u64,
            compressed: 0,
            swap_used: (swap_gb * g) as u64,
            pressure,
        }
    }

    #[test]
    fn 정상_구간은_조용하다() {
        // 실측 표본(36G 기계, 학생 열몇 도는 중) — 여기서 경고가 뜨면 오탐이다.
        assert_eq!(s(4.4, 1, 0.0).advice(), Advice::Ok);
    }

    #[test]
    fn wired_비율로_단계가_갈린다() {
        assert_eq!(s(11.0, 1, 0.0).advice(), Advice::Watch); // 30.5%
        assert_eq!(s(15.0, 1, 0.0).advice(), Advice::Restart); // 41.6%
    }

    #[test]
    fn 커널_압박은_비율을_앞선다() {
        // wired 가 정상이어도 커널이 경고하면 그쪽이 사실이다.
        assert_eq!(s(4.4, 2, 0.0).advice(), Advice::Restart);
    }

    #[test]
    fn 스왑이_많이_나가면_권한다() {
        assert_eq!(s(4.4, 1, 9.0).advice(), Advice::Restart);
    }

    #[test]
    fn 이유는_가장_급한_신호를_말한다() {
        assert_eq!(s(4.4, 4, 0.0).reason(), "메모리 압박 심각");
        assert!(s(15.0, 1, 0.0).reason().starts_with("wired 42%"));
    }

    #[test]
    fn 총량이_0이면_나누지_않는다() {
        let mut m = s(4.4, 1, 0.0);
        m.total = 0;
        assert_eq!(m.wired_pct(), 0.0);
    }
}
