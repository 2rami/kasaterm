//! OS 세션이 끝나는 중(Windows 로그오프·끄기·다시 시작)인지와, 그동안 띄운 자식이 창을 못 띄우게 하는 것.
//!
//! 끄기가 시작되면 세션 데스크톱이 내려가는 중이라 새로 띄운 프로세스는 user32 초기화에서 죽는다
//! (STATUS_DLL_INIT_FAILED, 0xc0000142). 앱의 주기 폴러(claude 명부·사용량 curl·git 패널·netstat)가
//! 그 사이에도 돌아, 끌 때마다 「응용 프로그램을 제대로 시작하지 못했습니다」 창이 줄줄이 떴다
//! (2026-11-11·12·13 시스템 로그 26 — 실패한 이름이 앱이 직접 띄우는 자식과 정확히 같았다).
//!
//! 두 겹으로 막는다. 끝을 알리는 메시지를 받은 앱이 [`set_ending`] 을 세우면 폴러가 띄우기를 멈추고,
//! 메시지보다 먼저 데스크톱이 막혀 이미 띄운 자식은 [`quiet_loader_error_boxes`] 가 물려준 오류 모드로
//! 창 없이 실패한다. 플래그를 세우는 쪽(숨은 창)은 winit 을 알아야 해서 앱에 있다.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 끝 신호가 이보다 오래됐는데 아직 살아 있으면 끝이 취소된 것으로 본다. 진짜 끝이면 Windows 가
/// 응답 없는 앱도 몇십 초 안에 거둔다 — 취소 알림(WM_ENDSESSION false)을 놓쳐도 폴러가 영영 멈춰
/// 있지 않게 한다.
const ENDING_TTL: Duration = Duration::from_secs(120);

/// 끝 신호를 받은 시각(유닉스 ms). 0 은 끝나는 중이 아님.
static ENDING_SINCE_MS: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64).max(1)
}

pub fn set_ending(on: bool) {
    ENDING_SINCE_MS.store(if on { now_ms() } else { 0 }, Ordering::Relaxed);
}

/// 지금 자식을 띄우면 데스크톱이 사라지는 중이라 실패한다 — 주기 폴러는 이 동안 건너뛴다.
pub fn is_ending() -> bool {
    ending_at(ENDING_SINCE_MS.load(Ordering::Relaxed), now_ms())
}

fn ending_at(since_ms: u64, now_ms: u64) -> bool {
    since_ms != 0 && now_ms.saturating_sub(since_ms) < ENDING_TTL.as_millis() as u64
}

/// 로더 단계에서 실패한 자식이 하드 오류 창을 띄우지 않게 한다. 오류 모드는 자식이 물려받으므로
/// 앱이 띄우는 모든 것 — 폴러, 칸 셸과 그 아래 claude 가 띄우는 git 까지 — 에 걸린다.
///
/// SEM_NOGPFAULTERRORBOX 는 일부러 안 건다. 이 오류와 무관한데, 칸 셸까지 물려받아 칸에서 돌린
/// 프로그램의 충돌 창까지 사라진다. SEM_FAILCRITICALERRORS 의 대가는 칸에서 DLL 이 빠진 프로그램을
/// 돌려도 창 대신 종료 코드만 남는 것이다(portable-pty 가 CREATE_DEFAULT_ERROR_MODE 를 못 받아
/// 칸만 빼 줄 수 없다).
#[cfg(windows)]
pub fn quiet_loader_error_boxes() {
    use windows_sys::Win32::System::Diagnostics::Debug::{GetErrorMode, SetErrorMode, SEM_FAILCRITICALERRORS};
    // SAFETY: 인자 없는 프로세스 전역 설정 읽기·쓰기다.
    unsafe {
        SetErrorMode(GetErrorMode() | SEM_FAILCRITICALERRORS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ending_expires_so_a_missed_cancel_does_not_stop_pollers_forever() {
        let t = 1_000_000;
        assert!(!ending_at(0, t));
        assert!(ending_at(t, t));
        assert!(ending_at(t, t + ENDING_TTL.as_millis() as u64 - 1));
        assert!(!ending_at(t, t + ENDING_TTL.as_millis() as u64));
    }

    #[test]
    fn set_and_clear() {
        set_ending(true);
        assert!(is_ending());
        set_ending(false);
        assert!(!is_ending());
    }

    /// 자식은 이 테스트를 다시 불러 제 오류 모드를 종료 코드로 돌려준다.
    const CHILD_ENV: &str = "KASA_SESSION_END_REPORT_ERROR_MODE";

    #[cfg(windows)]
    #[test]
    fn child_reports_error_mode() {
        if std::env::var_os(CHILD_ENV).is_none() {
            return;
        }
        use windows_sys::Win32::System::Diagnostics::Debug::GetErrorMode;
        // SAFETY: 인자 없는 읽기.
        std::process::exit(unsafe { GetErrorMode() } as i32);
    }

    /// 폴러가 쓰는 길 그대로(std `Command` + CREATE_NO_WINDOW) 띄운 자식이 모드를 물려받는지.
    #[cfg(windows)]
    #[test]
    fn std_command_children_inherit_the_quiet_mode() {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Diagnostics::Debug::SEM_FAILCRITICALERRORS;
        quiet_loader_error_boxes();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "session_end::tests::child_reports_error_mode", "--nocapture"])
            .env(CHILD_ENV, "1")
            .creation_flags(0x0800_0000)
            .status()
            .unwrap();
        let mode = status.code().expect("자식이 종료 코드 없이 끝났다") as u32;
        assert_ne!(mode & SEM_FAILCRITICALERRORS, 0, "자식 오류 모드 {mode:#x}");
    }

    /// 끌 때의 0xc0000142 를 끄지 않고 재현한다 — 숨은 데스크톱을 만들었다 닫아 사라지게 한 뒤 그
    /// 이름으로 NETSTAT(끌 때 실패 목록에 있던 것)를 띄우면, 내려가는 세션 데스크톱에 띄운 것처럼
    /// DLL 초기화에서 죽는다. 모드를 물려받았으면 창 없이 바로 끝나고, 못 받았으면 창이 떠 누를
    /// 때까지 멈춘다.
    ///
    /// 새 콘솔로 띄우는 건 NETSTAT 이 user32 를 안 올려 CREATE_NO_WINDOW 로는 데스크톱에 닿지 않아서다
    /// — 콘솔을 그 데스크톱에 만들다 실패한다. git.exe 는 CREATE_NO_WINDOW 로도 같은 코드로 죽는다
    /// (2026-11-15 실측). 어느 Windows 에나 있는 쪽을 고정으로 쓴다.
    ///
    /// 실패하면 사람 화면에 오류 창이 떠서 손으로 돌린다:
    /// `cargo test -p kasa-socket --lib session_end -- --ignored --nocapture`
    #[cfg(windows)]
    #[test]
    #[ignore = "실패하면 화면에 오류 창이 뜬다 — 손으로 돌린다"]
    fn loader_failure_on_a_vanished_desktop_exits_without_a_box() {
        use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
        use windows_sys::Win32::System::StationsAndDesktops::{CloseDesktop, CreateDesktopW, DESKTOP_CREATEWINDOW};
        use windows_sys::Win32::System::Threading::{
            CreateProcessW, GetExitCodeProcess, TerminateProcess, WaitForSingleObject, CREATE_NEW_CONSOLE,
            PROCESS_INFORMATION, STARTUPINFOW,
        };
        const STATUS_DLL_INIT_FAILED: u32 = 0xC000_0142;
        let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();

        quiet_loader_error_boxes();
        let name = format!("kasa-session-end-{}", std::process::id());
        let name_w = wide(&name);
        // SAFETY: 널 끝 UTF-16 이름, 나머지는 기본값(널).
        let desk = unsafe {
            CreateDesktopW(name_w.as_ptr(), std::ptr::null(), std::ptr::null(), 0, DESKTOP_CREATEWINDOW, std::ptr::null())
        };
        assert!(!desk.is_null(), "숨은 데스크톱을 못 만들었다");
        // SAFETY: 방금 받은 핸들. 마지막 핸들이 닫혀 데스크톱이 사라진다.
        unsafe { CloseDesktop(desk) };

        let exe = std::path::Path::new(&std::env::var("SystemRoot").unwrap()).join(r"System32\NETSTAT.EXE");
        let exe_w = wide(&exe.to_string_lossy());
        let mut cmd = wide(&format!("\"{}\" -?", exe.display()));
        let mut desktop = wide(&format!(r"WinSta0\{name}"));
        // SAFETY: STARTUPINFOW 는 0 이 유효한 기본값인 C 구조체다.
        let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
        si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        si.lpDesktop = desktop.as_mut_ptr();
        // SAFETY: 위와 같음.
        let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: 모든 문자열은 널 끝이고 이 호출 동안 살아 있다.
        let ok = unsafe {
            CreateProcessW(
                exe_w.as_ptr(), cmd.as_mut_ptr(), std::ptr::null(), std::ptr::null(), 0, CREATE_NEW_CONSOLE,
                std::ptr::null(), std::ptr::null(), &si, &mut pi,
            )
        };
        assert_ne!(ok, 0, "CreateProcessW 실패: {}", std::io::Error::last_os_error());
        // SAFETY: pi 의 핸들은 CreateProcessW 가 채운 유효 핸들이다.
        let (waited, code) = unsafe {
            let waited = WaitForSingleObject(pi.hProcess, 10_000);
            let mut code = 0u32;
            GetExitCodeProcess(pi.hProcess, &mut code);
            if waited != WAIT_OBJECT_0 {
                TerminateProcess(pi.hProcess, 1);
            }
            CloseHandle(pi.hThread);
            CloseHandle(pi.hProcess);
            (waited, code)
        };
        assert_eq!(waited, WAIT_OBJECT_0, "자식이 끝나지 않았다 — 오류 창이 떠서 멈춘 것");
        assert_eq!(code, STATUS_DLL_INIT_FAILED, "재현이 안 됐다: 종료 코드 {code:#x}");
    }
}
