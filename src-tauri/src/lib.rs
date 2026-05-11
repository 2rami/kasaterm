mod tmux;

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Mutex;
use std::thread;

use tauri::{Emitter, State};

/// 살아있는 tmux subprocess 핸들.
/// stdin 만 잡아두고 stdout 은 reader thread 가 가져감.
struct TmuxSession {
    child: Child,
    stdin: ChildStdin,
}

#[derive(Default)]
struct AppState {
    session: Mutex<Option<TmuxSession>>,
}

/// tmux -C 로 subprocess 띄우고 stdout 라인마다 frontend 로 `tmux-event` 발사.
#[tauri::command]
fn start_tmux(
    session_name: Option<String>,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    let mut guard = state.session.lock().map_err(|e| e.to_string())?;
    if guard.is_some() {
        return Err("tmux session already running".into());
    }

    // attach 시도, 없으면 new
    let target = session_name.unwrap_or_else(|| "main".to_string());
    let mut cmd = Command::new("tmux");
    cmd.arg("-C").arg("new-session").arg("-A").arg("-s").arg(&target);
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| format!("tmux spawn failed: {e}"))?;
    let stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = child.stdout.take().ok_or("no stdout")?;

    // stdout reader 스레드 — 라인 단위 파싱 후 emit
    let app_clone = app.clone();
    thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            let event = tmux::parse_line(&line);
            if let Err(e) = app_clone.emit("tmux-event", &event) {
                eprintln!("emit failed: {e}");
                break;
            }
        }
        let _ = app_clone.emit("tmux-event", &tmux::TmuxEvent::Exit);
    });

    *guard = Some(TmuxSession { child, stdin });
    Ok(target)
}

/// tmux 에 명령 문자열 송신 (control mode 에선 prefix 없이 바로 명령).
#[tauri::command]
fn send_tmux(cmd: String, state: State<'_, AppState>) -> Result<(), String> {
    let mut guard = state.session.lock().map_err(|e| e.to_string())?;
    let session = guard.as_mut().ok_or("no active tmux session")?;
    writeln!(session.stdin, "{cmd}").map_err(|e| e.to_string())?;
    session.stdin.flush().map_err(|e| e.to_string())?;
    Ok(())
}

/// tmux 종료. detach-client 만 보내고 child 는 자연사 대기.
#[tauri::command]
fn stop_tmux(state: State<'_, AppState>) -> Result<(), String> {
    let mut guard = state.session.lock().map_err(|e| e.to_string())?;
    if let Some(mut session) = guard.take() {
        let _ = writeln!(session.stdin, "detach-client");
        let _ = session.stdin.flush();
        let _ = session.child.wait();
    }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![start_tmux, send_tmux, stop_tmux])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
