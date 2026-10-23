//! Windows 에서 `.md` 를 더블클릭하면 탐색기가 `kasaterm.exe "<파일>"` 을 새로 띄운다. 맥은 그 일을
//! 떠 있는 앱에 넘기지만(odoc) Windows 엔 그런 길이 없어, 그대로 두면 두 번째 kasaterm 이 통째로
//! 떠서 같은 세션을 복원하고 첫 앱의 학생과 같은 대화를 다툰다(`docs/verify-app.md` 맨 위 사고).
//!
//! 그래서 부팅 맨 앞에서, 같은 exe 로 떠 있는 kasaterm 을 찾아 그 소켓에 `app.open_document` 로
//! 문서를 넘기고 이 실행은 아무것도 만들지 않고 끝낸다. 다른 exe(개발 빌드·격리 리그)는 고르지 않는다.
//! 떠 있는 앱이 없으면 평소대로 부팅해 인자 경로를 연다(main.rs). 떠 있는데 못 받으면(이 메서드를
//! 모르는 옛 판이 아직 도는 중) 부팅 정비를 안 하는 뷰어로 연다 — 그래도 두 번째 본판은 안 띄운다.

/// 부팅 맨 앞의 판정.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Forward {
    /// 떠 있는 앱이 받았다 — 이 실행은 곧장 끝난다.
    Sent,
    /// 같은 exe 가 떠 있는데 못 받았다 — 뷰어로 연다.
    Refused,
    /// 받을 앱이 없거나 문서 실행이 아니다 — 평소대로 부팅한다.
    None,
}

use std::path::{Path, PathBuf};

pub(crate) fn forward_documents(paths: &[PathBuf]) -> Forward {
    let documents: Vec<String> = paths
        .iter()
        .filter(|path| crate::is_launch_markdown(path) && path.is_file())
        .filter_map(|path| std::fs::canonicalize(path).ok())
        .map(|path| kasa_socket::shell_line::plain_path(&path.to_string_lossy()))
        .collect();
    if documents.is_empty() || documents.len() != paths.len() {
        return Forward::None;
    }
    let Some(me) = std::env::current_exe().ok().and_then(|exe| std::fs::canonicalize(exe).ok()) else {
        return Forward::None;
    };
    let mut found = false;
    let mut others: Vec<u32> = crate::live_kasaterm_pids().into_iter().filter(|pid| *pid != std::process::id()).collect();
    others.sort_unstable();
    for pid in others {
        if image_of(pid).and_then(|exe| std::fs::canonicalize(exe).ok()).as_deref() != Some(me.as_path()) {
            continue;
        }
        found = true;
        let socket = format!("{}/kasaterm-{pid}.sock", std::env::temp_dir().to_string_lossy());
        // 탐색기가 띄운 이 실행이 지금 앞 창의 주인이라, 넘겨받은 앱이 새 문서 창을 앞에 세울 수 있게
        // 그 권한을 건넨다 — 안 건네면 문서 창이 작업 표시줄에서 깜빡이기만 한다.
        unsafe { windows_sys::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(pid) };
        if documents.iter().all(|path| send(&socket, path)) {
            return Forward::Sent;
        }
    }
    if found { Forward::Refused } else { Forward::None }
}

fn send(socket: &str, path: &str) -> bool {
    use std::io::{BufRead, Write};
    let Ok(stream) = kasa_socket::transport::LocalStream::connect(Path::new(socket)) else { return false };
    let Ok(mut writer) = stream.try_clone() else { return false };
    let request = serde_json::json!({ "id": 1, "method": "app.open_document", "params": { "path": path } });
    if writer.write_all(format!("{request}\n").as_bytes()).is_err() {
        return false;
    }
    let mut line = String::new();
    if std::io::BufReader::new(stream).read_line(&mut line).is_err() {
        return false;
    }
    serde_json::from_str::<serde_json::Value>(&line).is_ok_and(|reply| reply["ok"] == true)
}

/// 그 프로세스의 실행 파일 경로.
fn image_of(pid: u32) -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
    // SAFETY: 연 핸들은 여기서 닫고, 성공했을 때 돌려준 길이만큼만 읽는다.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut buffer = vec![0u16; 1024];
        let mut length = buffer.len() as u32;
        let read = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buffer.as_mut_ptr(), &mut length) != 0;
        CloseHandle(handle);
        read.then(|| std::ffi::OsString::from_wide(&buffer[..length as usize]).into())
    }
}
