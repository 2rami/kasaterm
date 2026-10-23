//! Windows 파일·폴더 고르기 창 — 셸의 `IFileOpenDialog`·`IFileSaveDialog` 를 직접 띄운다.
//!
//! 예전에는 PowerShell 로 WinForms 창을 띄우고 표준 출력으로 경로를 받았다. 그 길은
//! PowerShell 이 뜨는 1~2초가 매번 붙고, 폴더 고르기는 WinForms 에 탐색기와 같은 창이
//! 없어 아예 비어 있었다. 셸 창은 탐색기·다른 앱과 같은 창이라 즐겨찾기·최근 폴더가
//! 그대로 보인다.
//!
//! 모든 함수는 창이 닫힐 때까지 막는다 — 부르는 쪽이 워커 스레드에서 부른다. COM 은
//! 그 스레드에서 STA 로 열고 닫는다(셸 창은 STA 를 요구한다).

use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use windows::core::{Interface, HRESULT, HSTRING, PCWSTR};
use windows::Win32::Foundation::{ERROR_CANCELLED, HWND};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FileOpenDialog, FileSaveDialog, IFileDialog, IFileOpenDialog, IFileSaveDialog,
    FOS_FORCEFILESYSTEM, FOS_OVERWRITEPROMPT, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS,
    SIGDN_FILESYSPATH,
};

/// 창의 주인 — winit 창의 HWND. `HWND` 는 `Send` 가 아니라 정수로 들고 스레드를 건넌다.
pub(crate) fn owner_of(window: &winit::window::Window) -> Option<isize> {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}

/// 폴더 하나. 취소하면 `Ok(None)`.
pub(crate) fn pick_folder(title: &str, owner: Option<isize>) -> Result<Option<PathBuf>, String> {
    with_com(|| unsafe {
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        dialog.SetOptions(dialog.GetOptions()? | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST)?;
        run(&dialog.cast()?, title, owner)
    })
}

/// 파일 하나. `filters` 는 (보이는 이름, `*.svg;*.png` 꼴 패턴). 비우면 모든 파일.
pub(crate) fn pick_file(title: &str, filters: &[(&str, &str)], owner: Option<isize>) -> Result<Option<PathBuf>, String> {
    with_com(|| unsafe {
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        dialog.SetOptions(dialog.GetOptions()? | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST)?;
        // COMDLG_FILTERSPEC 은 문자열을 빌려 가기만 한다 — SetFileTypes 가 끝날 때까지
        // 원본 HSTRING 이 살아 있어야 해서 따로 묶어 둔다.
        let owned: Vec<(HSTRING, HSTRING)> = filters.iter().map(|(name, spec)| (HSTRING::from(*name), HSTRING::from(*spec))).collect();
        let specs: Vec<COMDLG_FILTERSPEC> = owned.iter().map(|(name, spec)| COMDLG_FILTERSPEC { pszName: PCWSTR(name.as_ptr()), pszSpec: PCWSTR(spec.as_ptr()) }).collect();
        if !specs.is_empty() {
            dialog.SetFileTypes(&specs)?;
        }
        run(&dialog.cast()?, title, owner)
    })
}

/// 저장할 자리. 이미 있는 파일을 고르면 셸 창이 덮어쓸지 먼저 묻는다.
pub(crate) fn save_file(title: &str, name: &str, owner: Option<isize>) -> Result<Option<PathBuf>, String> {
    with_com(|| unsafe {
        let dialog: IFileSaveDialog = CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER)?;
        dialog.SetOptions(dialog.GetOptions()? | FOS_FORCEFILESYSTEM | FOS_OVERWRITEPROMPT)?;
        dialog.SetFileName(&HSTRING::from(name))?;
        // 확장자를 지우고 이름만 고쳐 쳐도 원래 확장자가 붙게 한다 — 안 그러면 확장자 없는
        // 파일로 저장돼 더블클릭해도 안 열린다.
        if let Some(ext) = std::path::Path::new(name).extension().and_then(|e| e.to_str()) {
            dialog.SetDefaultExtension(&HSTRING::from(ext))?;
        }
        run(&dialog.cast()?, title, owner)
    })
}

unsafe fn run(dialog: &IFileDialog, title: &str, owner: Option<isize>) -> windows::core::Result<Option<PathBuf>> {
    unsafe {
        dialog.SetTitle(&HSTRING::from(title))?;
        if let Err(error) = dialog.Show(owner.or_else(|| own_foreground()).map(|h| HWND(h as *mut _))) {
            return if error.code() == HRESULT::from_win32(ERROR_CANCELLED.0) { Ok(None) } else { Err(error) };
        }
        let text = dialog.GetResult()?.GetDisplayName(SIGDN_FILESYSPATH)?;
        // UTF-16 그대로 옮긴다 — 짝 없는 서로게이트가 든 이름도 NTFS 에선 유효한 경로다.
        let path = PathBuf::from(std::ffi::OsString::from_wide(text.as_wide()));
        CoTaskMemFree(Some(text.0 as *const _));
        Ok(Some(path))
    }
}

/// 주인을 안 준 창은 지금 앞에 있는 우리 창에 붙인다. 주인 없는 창은 작업 표시줄에 따로 서고,
/// 본창을 누르면 그 뒤로 숨어 「창이 안 뜬다」로 보인다. 남의 창(다른 프로세스)에는 안 붙인다.
fn own_foreground() -> Option<isize> {
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
    let window = unsafe { GetForegroundWindow() };
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(window, Some(&mut pid)) };
    (!window.is_invalid() && pid == std::process::id()).then_some(window.0 as isize)
}

fn with_com(body: impl FnOnce() -> windows::core::Result<Option<PathBuf>>) -> Result<Option<PathBuf>, String> {
    // 이미 MTA 로 열린 스레드(RPC_E_CHANGED_MODE)면 짝을 맞출 CoUninitialize 를 부르면 안 된다 —
    // 남이 연 COM 을 우리가 닫게 된다.
    let opened = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }.is_ok();
    let result = body().map_err(|error| format!("파일 창을 열지 못했어요: {}", error.message()));
    if opened {
        unsafe { CoUninitialize() };
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::{GENERIC_ALL, LPARAM, WPARAM};
    use windows::Win32::System::StationsAndDesktops::{
        CloseDesktop, CreateDesktopW, EnumDesktopWindows, SetThreadDesktop, DESKTOP_CONTROL_FLAGS, HDESK,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowTextW, PostMessageW, IDCANCEL, IDOK, WM_COMMAND};

    /// 사람 화면에 띄우면 그 사람이 치던 글자가 이름 칸으로 들어가고 Enter 가 엉뚱한 파일을
    /// 고른다. 보이지 않는 데스크톱에서 띄우고 창 제목으로 찾아 단추 명령만 보낸다.
    fn on_hidden_desktop<T: Send + 'static>(title: &'static str, button: i32, open: impl FnOnce() -> T + Send + 'static) -> T {
        let name = HSTRING::from(format!("kasaterm-dialog-{}-{title}", std::process::id()));
        let desk = unsafe { CreateDesktopW(&name, None, None, DESKTOP_CONTROL_FLAGS(0), GENERIC_ALL.0, None) }.expect("데스크톱");
        let raw = desk.0 as isize;
        let worker = std::thread::spawn(move || {
            unsafe { SetThreadDesktop(HDESK(raw as *mut _)) }.expect("스레드를 옮기지 못했다");
            open()
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let found = loop {
            if let Some(hwnd) = find_titled(desk, title) {
                break hwnd;
            }
            assert!(std::time::Instant::now() < deadline, "{title} 창이 안 떴다");
            std::thread::sleep(std::time::Duration::from_millis(100));
        };
        // 창이 막 서고 바로 누르면 셸이 결과 항목을 아직 안 채워 OK 가 무시된다.
        std::thread::sleep(std::time::Duration::from_millis(800));
        unsafe { PostMessageW(Some(found), WM_COMMAND, WPARAM(button as usize), LPARAM(0)) }.unwrap();
        let out = worker.join().unwrap();
        // 폴더 창은 셸 일꾼 스레드를 그 데스크톱에 남겨 닫기가 ERROR_BUSY 로 실패한다 — 그 스레드가
        // 끝나거나 시험 프로세스가 끝날 때 함께 사라지므로 기다리지 않는다.
        let _ = unsafe { CloseDesktop(desk) };
        out
    }

    fn find_titled(desk: HDESK, title: &str) -> Option<HWND> {
        struct Probe(Vec<u16>, Option<HWND>);
        unsafe extern "system" fn each(hwnd: HWND, data: LPARAM) -> windows::core::BOOL {
            let probe = unsafe { &mut *(data.0 as *mut Probe) };
            let mut text = [0u16; 256];
            let n = unsafe { GetWindowTextW(hwnd, &mut text) } as usize;
            if text[..n] == probe.0[..] {
                probe.1 = Some(hwnd);
                return false.into();
            }
            true.into()
        }
        let mut probe = Probe(title.encode_utf16().collect(), None);
        let _ = unsafe { EnumDesktopWindows(Some(desk), Some(each), LPARAM(&mut probe as *mut Probe as isize)) };
        probe.1
    }

    #[test]
    #[ignore = "실제 셸 창을 띄운다 — 대화형 데스크톱이 있는 Windows 에서 손으로 돌린다"]
    fn save_dialog_returns_the_proposed_name_with_its_extension() {
        let name = format!("kasa-probe-{}.png", std::process::id());
        let wanted = name.clone();
        let picked = on_hidden_desktop("저장 시험", IDOK.0, move || save_file("저장 시험", &name, None)).unwrap();
        let picked = picked.expect("OK 면 경로가 와야 한다");
        assert!(picked.is_absolute(), "{picked:?}");
        assert_eq!(picked.file_name().and_then(|n| n.to_str()), Some(wanted.as_str()));
        assert!(!picked.exists(), "저장 창은 자리만 고르고 파일은 안 만든다");
    }

    #[test]
    #[ignore = "실제 셸 창을 띄운다 — 대화형 데스크톱이 있는 Windows 에서 손으로 돌린다"]
    fn folder_dialog_returns_an_existing_folder() {
        let picked = on_hidden_desktop("폴더 시험", IDOK.0, || pick_folder("폴더 시험", None)).unwrap();
        assert!(picked.expect("OK 면 폴더가 와야 한다").is_dir());
    }

    #[test]
    #[ignore = "실제 셸 창을 띄운다 — 대화형 데스크톱이 있는 Windows 에서 손으로 돌린다"]
    fn cancelling_is_not_an_error() {
        let picked = on_hidden_desktop("열기 시험", IDCANCEL.0, || pick_file("열기 시험", &[("SVG", "*.svg")], None));
        assert_eq!(picked, Ok(None));
    }
}
