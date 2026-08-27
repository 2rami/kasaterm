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
///
/// `Restart` 와 `FreeUp` 은 심각도가 같고 **답이 다르다**. 둘을 한 낱말로
/// 뭉뚱그리면 조언이 틀린다 — 앱이 먹어서 모자란 자리에 「재부팅하세요」는
/// 헛다리고(닫으면 돌아온다), wired 가 쌓인 자리에 「앱을 닫으세요」도
/// 헛다리다(닫아도 안 돌아온다). 2026-08-27 지적: 「wire없이 많아지면
/// 안뜨고?」 — 뜨는데 그때 하던 말이 재시작 권장이었다.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Advice {
    Ok,
    /// 아직 돌아가지만 앱 자리가 눈에 띄게 줄었다.
    Watch,
    /// 회수 경로가 재시작뿐인 구간 — wired 가 임계를 넘었다.
    Restart,
    /// 물리 메모리가 모자라지만 앱을 닫으면 돌아온다.
    FreeUp,
}

impl Advice {
    /// 하단바 칩과 팝오버 제목에 쓰는 한 낱말.
    pub(crate) fn headline(self) -> &'static str {
        match self {
            Advice::Ok => "맥북 메모리",
            Advice::Watch => "맥북 메모리 주의",
            Advice::Restart => "맥북 재시작 권장",
            Advice::FreeUp => "메모리 부족",
        }
    }

    /// 빨강으로 칠할 구간인가.
    pub(crate) fn is_danger(self) -> bool {
        matches!(self, Advice::Restart | Advice::FreeUp)
    }
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
const SWAP_LIMIT_GB: u64 = 8;

/// 임계는 실측으로 조정할 값이라 env 로 덮을 수 있게 둔다. 위 상수는 이 기계
/// 한 대의 표본에서 나온 추정이고, RAM 크기나 쓰는 앱 구성이 다르면 어긋난다.
fn pct_env(key: &str, default: f32) -> f32 {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// 스왑 임계(GB → byte). 「메모리 부족」 화면은 실기에서 재현할 수가 없어서
/// (커널 압박도 스왑도 우리가 만들 수 없다) 검증에도 이 문이 필요하다.
fn swap_limit() -> u64 {
    std::env::var("KASATERM_MEM_SWAP_GB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(SWAP_LIMIT_GB)
        * 1024
        * 1024
        * 1024
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
        // wired 를 **가장 먼저** 본다. 같은 위기라도 회수 경로가 갈리는 유일한
        // 기준이라, 압박이 함께 올라 있어도 재시작 쪽이 근본 처방이다.
        if pct >= pct_env("KASATERM_MEM_RESTART_PCT", RESTART_PCT) {
            Advice::Restart
        // `critical` 은 커널이 이미 메모리를 되찾으려 프로세스를 압박하는 단계고,
        // 스왑이 이만큼 나갔으면 디스크를 물고 도는 중이다. 둘 다 급하지만 wired
        // 가 정상 범위라면 쥐고 있는 것은 앱이므로, 닫으면 돌아온다.
        } else if self.pressure >= 4 || self.swap_used >= swap_limit() {
            Advice::FreeUp
        // ⚠️`warning`(2) 은 **재시작 사유가 아니다**. 큰 빌드 한 번이면 오르고
        // 끝나면 곧 1 로 돌아온다 — 실측 2026-08-27: cargo 빌드 중 캡처에서 wired
        // 16% 인데도 「재시작 권장」이 떴고, 빌드가 끝나자 압박은 1 이었다.
        // 그걸 재시작 사유로 치면 무거운 작업을 할 때마다 뜨고, 그러면 정작
        // 진짜로 쌓였을 때의 경고까지 같이 무시하게 된다. 주의까지만 말한다.
        } else if self.pressure >= 2 || pct >= pct_env("KASATERM_MEM_WATCH_PCT", WATCH_PCT) {
            Advice::Watch
        } else {
            Advice::Ok
        }
    }

    /// 왜 권하는지 한 줄 — **토스트용**이다. 토스트에는 다른 수치가 함께 뜨지
    /// 않으므로 원인이 wired 여도 숫자를 적어야 말이 된다.
    pub(crate) fn reason(&self) -> String {
        let gb = |b: u64| b as f32 / (1024.0 * 1024.0 * 1024.0);
        let wired = || format!("wired {:.0}% · {:.1}G", self.wired_pct(), gb(self.wired));
        // 판정과 **같은 순서**로 본다. 어긋나면 「재시작 권장」 옆에 압박 얘기가
        // 붙어, 왜 재시작이 답인지가 도로 흐려진다.
        match self.advice() {
            Advice::Restart => wired(),
            _ if self.pressure >= 4 => "메모리 압박 심각".to_string(),
            _ if self.swap_used >= swap_limit() => format!("스왑 {:.1}G", gb(self.swap_used)),
            _ if self.pressure >= 2 => "메모리 압박 경고".to_string(),
            _ => wired(),
        }
    }

    /// 팝오버용 이유 — 옆 칼럼이 이미 `wired 14%` 와 `5.0G / 36G` 를 적으므로,
    /// 원인이 wired 면 되풀이하지 않는다. 되풀이하면 제목이 길어져 잘리고
    /// (실측 2026-08-27: `맥북 재시작 권장 · wired 14% · 5…`), 잘린 자리에
    /// 정작 새 정보는 하나도 없다. 압박·스왑은 그 칼럼에 안 나오니 그때만 밝힌다.
    pub(crate) fn extra_reason(&self) -> Option<String> {
        let by_wired =
            self.advice() == Advice::Restart || (self.pressure < 2 && self.swap_used < swap_limit());
        (!by_wired).then(|| self.reason())
    }
}

/// `mach_host_self` 는 libc 가 mach API 를 `mach2` 크레이트로 밀어내며 붙인
/// deprecated 다 — 기능이 사라진 게 아니라 소속이 옮겨간 것이고, 이 호출 하나
/// 때문에 의존성을 늘릴 이유는 없다.
#[allow(deprecated)]
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
    fn 일시적인_압박_경고는_재시작_사유가_아니다() {
        // 큰 빌드 한 번이면 warning 이 뜬다(실측). 주의까지만.
        assert_eq!(s(4.4, 2, 0.0).advice(), Advice::Watch);
    }

    #[test]
    fn wired_가_정상이면_재시작이_아니라_비우기다() {
        // critical 은 커널이 이미 회수에 들어간 것이라 급하지만, wired 가 정상
        // 범위면 쥐고 있는 것은 앱이다 — 재부팅해도 그 앱을 다시 열면 그대로다.
        assert_eq!(s(4.4, 4, 0.0).advice(), Advice::FreeUp);
        assert_eq!(s(4.4, 1, 9.0).advice(), Advice::FreeUp);
    }

    #[test]
    fn wired_가_임계를_넘으면_압박보다_먼저다() {
        // 둘 다 걸린 자리에서는 재시작이 근본 처방이다. 앱을 닫아도 wired 는
        // 그대로라, 비우기를 권하면 사람이 해 봐도 안 나아진다.
        assert_eq!(s(15.0, 4, 9.0).advice(), Advice::Restart);
    }

    #[test]
    fn 이유는_가장_급한_신호를_말한다() {
        assert_eq!(s(4.4, 4, 0.0).reason(), "메모리 압박 심각");
        assert!(s(15.0, 1, 0.0).reason().starts_with("wired 42%"));
        // 판정이 재시작이면 이유도 wired 여야 한다 — 압박이 함께 걸려 있어도
        // 「압박 심각」이라 말하면 왜 재시작이 답인지가 흐려진다.
        assert!(s(15.0, 4, 9.0).reason().starts_with("wired"));
    }

    #[test]
    fn 답이_다르면_말도_다르다() {
        assert_eq!(s(15.0, 1, 0.0).advice().headline(), "맥북 재시작 권장");
        assert_eq!(s(4.4, 4, 0.0).advice().headline(), "메모리 부족");
        assert!(s(4.4, 4, 0.0).advice().is_danger());
        assert!(!s(4.4, 2, 0.0).advice().is_danger());
    }

    /// 판정이 아무리 옳아도 재는 값이 틀리면 소용이 없다. mach 구조체는
    /// `repr(packed)` 라 필드 하나만 어긋나도 컴파일은 통과하면서 엉뚱한 숫자가
    /// 나오는데, 그건 화면에서 그럴듯해 보인다 — 불변식으로 잡아 둔다.
    #[test]
    #[cfg(target_os = "macos")]
    fn 실제_기계에서_말이_되는_값이_나온다() {
        let m = sample().expect("macOS 에서는 늘 읽혀야 한다");
        let memsize: u64 =
            unsafe { sysctl_scalar("hw.memsize") }.expect("hw.memsize");
        assert_eq!(m.total, memsize);
        assert!(m.wired > 0, "wired 가 0 이면 필드를 잘못 짚은 것이다");
        assert!(m.wired < m.total, "wired {} >= total {}", m.wired, m.total);
        // 커널 압박은 1·2·4 중 하나. 0 이 나오면 sysctl 이름이나 타입이 틀렸다.
        assert!(matches!(m.pressure, 1 | 2 | 4), "pressure={}", m.pressure);
        let pct = m.wired_pct();
        assert!((0.0..100.0).contains(&pct), "wired_pct={pct}");
    }

    #[test]
    fn 팝오버는_옆칸과_겹치는_이유를_생략한다() {
        // wired 가 원인이면 옆 칼럼이 이미 그 숫자를 적는다.
        assert_eq!(s(15.0, 1, 0.0).extra_reason(), None);
        // 압박·스왑은 옆 칼럼에 안 나오니 밝혀야 한다.
        assert_eq!(s(4.4, 2, 0.0).extra_reason().as_deref(), Some("메모리 압박 경고"));
        assert!(s(4.4, 1, 9.0).extra_reason().is_some_and(|r| r.starts_with("스왑")));
    }

    #[test]
    fn 총량이_0이면_나누지_않는다() {
        let mut m = s(4.4, 1, 0.0);
        m.total = 0;
        assert_eq!(m.wired_pct(), 0.0);
    }
}
