mod tmux;

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde::Serialize;
use tauri::{Emitter, State};

/// 살아있는 tmux subprocess + vt100 파서.
/// 메인 스레드/명령 핸들러는 stdin 과 parser 에만 접근.
struct TmuxSession {
    child: Child,
    stdin: ChildStdin,
    /// pane_id → vt100 parser. tmux 의 각 pane 마다 별도 화면 상태.
    parsers: Arc<Mutex<std::collections::HashMap<String, vt100::Parser>>>,
}

#[derive(Default)]
struct AppState {
    session: Mutex<Option<TmuxSession>>,
}

/// 셀 1개의 wire 표현 — JSON 직렬화 시 compact 한 키.
#[derive(Debug, Clone, Serialize, PartialEq)]
struct CellWire {
    /// 글자 (와이드면 2 코드포인트 가능, 빈 셀은 " ")
    ch: String,
    /// fg/bg: null=default, 숫자=palette idx(0..255), "#rrggbb"=truecolor
    #[serde(skip_serializing_if = "Option::is_none")]
    fg: Option<ColorWire>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bg: Option<ColorWire>,
    /// 비트 플래그: 1=bold 2=italic 4=underline 8=inverse 16=blink
    #[serde(skip_serializing_if = "is_zero")]
    a: u8,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(untagged)]
enum ColorWire {
    Idx(u8),
    Hex(String),
}

fn is_zero(v: &u8) -> bool {
    *v == 0
}

/// 한 row 분 셀 + 그 row 의 변경 시점 식별용 dirty flag.
type RowWire = Vec<CellWire>;

/// 프론트에 보낼 화면 스냅샷.
#[derive(Debug, Clone, Serialize)]
struct ScreenWire {
    pane_id: String,
    rows: u16,
    cols: u16,
    /// 변경된 행만 (idx, cells). 전체 보내려면 별도 메시지.
    dirty: Vec<(u16, RowWire)>,
    cursor_row: u16,
    cursor_col: u16,
    cursor_visible: bool,
    /// alt screen 활성? 시각화에는 직접 안 쓰지만 디버그용
    alt: bool,
}

fn color_to_wire(c: vt100::Color) -> Option<ColorWire> {
    match c {
        vt100::Color::Default => None,
        vt100::Color::Idx(i) => Some(ColorWire::Idx(i)),
        vt100::Color::Rgb(r, g, b) => Some(ColorWire::Hex(format!(
            "#{:02x}{:02x}{:02x}",
            r, g, b
        ))),
    }
}

fn cell_to_wire(c: &vt100::Cell) -> CellWire {
    let mut a: u8 = 0;
    if c.bold() {
        a |= 1;
    }
    if c.italic() {
        a |= 2;
    }
    if c.underline() {
        a |= 4;
    }
    if c.inverse() {
        a |= 8;
    }
    let contents = c.contents();
    let ch = if contents.is_empty() {
        " ".to_string()
    } else {
        contents
    };
    CellWire {
        ch,
        fg: color_to_wire(c.fgcolor()),
        bg: color_to_wire(c.bgcolor()),
        a,
    }
}

/// 디렉토리에서 tmux 세션 attach/new + 옵션 자동 명령 실행.
#[tauri::command]
fn start_tmux(
    cwd: Option<String>,
    auto_run: Option<String>,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    let mut guard = state.session.lock().map_err(|e| e.to_string())?;
    if guard.is_some() {
        return Err("tmux session already running".into());
    }

    let session_name = match cwd.as_deref() {
        Some(p) if !p.is_empty() => session_name_for_path(p),
        _ => "tmuxify-main".to_string(),
    };

    let session_exists = Command::new("tmux")
        .arg("has-session")
        .arg("-t")
        .arg(&session_name)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    let mut cmd = Command::new("tmux");
    cmd.arg("-C").arg("new-session").arg("-A").arg("-s").arg(&session_name);
    if let Some(p) = cwd.as_deref().filter(|s| !s.is_empty()) {
        cmd.arg("-c").arg(p);
    }
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| format!("tmux spawn failed: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = child.stdout.take().ok_or("no stdout")?;

    // pane 별 vt100 parser
    let parsers: Arc<Mutex<std::collections::HashMap<String, vt100::Parser>>> =
        Arc::new(Mutex::new(std::collections::HashMap::new()));

    // pane 별 이전 행 스냅샷 — diff 계산용 (flusher 스레드 전용)
    let prev_rows: Arc<Mutex<std::collections::HashMap<String, Vec<RowWire>>>> =
        Arc::new(Mutex::new(std::collections::HashMap::new()));

    // reader 스레드 — %output 은 parser 에 feed, 나머지는 emit.
    let app_reader = app.clone();
    let parsers_reader = parsers.clone();
    thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            let event = tmux::parse_line(&line);
            match &event {
                tmux::TmuxEvent::Output { pane_id, data } => {
                    let mut map = parsers_reader.lock().unwrap();
                    let p = map.entry(pane_id.clone()).or_insert_with(|| {
                        vt100::Parser::new(24, 80, 5000)
                    });
                    p.process(data.as_bytes());
                }
                _ => {
                    if app_reader.emit("tmux-event", &event).is_err() {
                        break;
                    }
                }
            }
        }
        let _ = app_reader.emit("tmux-event", &tmux::TmuxEvent::Exit);
    });

    // flusher 스레드 — 16ms 마다 각 pane 의 screen 을 diff 해서 emit.
    let app_flush = app.clone();
    let parsers_flush = parsers.clone();
    let prev_rows_flush = prev_rows.clone();
    thread::spawn(move || {
        let interval = Duration::from_millis(16);
        loop {
            thread::sleep(interval);
            let pane_ids: Vec<String> = {
                let map = parsers_flush.lock().unwrap();
                map.keys().cloned().collect()
            };
            for pid in pane_ids {
                let (rows, cols, current_rows, cursor_row, cursor_col, cursor_visible, alt) = {
                    let map = parsers_flush.lock().unwrap();
                    let Some(parser) = map.get(&pid) else { continue };
                    let s = parser.screen();
                    let (h, w) = s.size();
                    let mut rows_data: Vec<RowWire> = Vec::with_capacity(h as usize);
                    for r in 0..h {
                        let mut row: RowWire = Vec::with_capacity(w as usize);
                        for c in 0..w {
                            let cw = match s.cell(r, c) {
                                Some(cell) => cell_to_wire(cell),
                                None => CellWire {
                                    ch: " ".to_string(),
                                    fg: None,
                                    bg: None,
                                    a: 0,
                                },
                            };
                            row.push(cw);
                        }
                        rows_data.push(row);
                    }
                    let (cr, cc) = s.cursor_position();
                    let cvis = !s.hide_cursor();
                    let alt = s.alternate_screen();
                    (h, w, rows_data, cr, cc, cvis, alt)
                };
                // diff vs prev_rows
                let mut prev_map = prev_rows_flush.lock().unwrap();
                let prev = prev_map.entry(pid.clone()).or_insert_with(Vec::new);
                let mut dirty: Vec<(u16, RowWire)> = Vec::new();
                if prev.len() != current_rows.len() {
                    // 사이즈 변경 — 전부 emit
                    for (i, row) in current_rows.iter().enumerate() {
                        dirty.push((i as u16, row.clone()));
                    }
                } else {
                    for (i, row) in current_rows.iter().enumerate() {
                        if &prev[i] != row {
                            dirty.push((i as u16, row.clone()));
                        }
                    }
                }
                *prev = current_rows;
                drop(prev_map);

                // dirty 비어있어도 cursor 위치 바뀌었을 수 있어서 일단 emit
                let payload = ScreenWire {
                    pane_id: pid.clone(),
                    rows,
                    cols,
                    dirty,
                    cursor_row,
                    cursor_col,
                    cursor_visible,
                    alt,
                };
                if app_flush.emit("tmux-screen", &payload).is_err() {
                    return;
                }
            }
        }
    });

    // attach 직후 자동 명령 (새 세션에만)
    if !session_exists {
        if let Some(text) = auto_run.filter(|s| !s.is_empty()) {
            thread::sleep(Duration::from_millis(400));
            let escaped = text.replace('\'', "'\\''");
            let _ = writeln!(stdin, "send-keys -l '{}'", escaped);
            let _ = writeln!(stdin, "send-keys Enter");
            let _ = stdin.flush();
        }
    }

    *guard = Some(TmuxSession { child, stdin, parsers });
    Ok(session_name)
}

/// 임의 tmux 명령 한 줄 전송 (control mode stdin 그대로).
#[tauri::command]
fn send_tmux_cmd(cmd: String, state: State<'_, AppState>) -> Result<(), String> {
    let mut guard = state.session.lock().map_err(|e| e.to_string())?;
    let session = guard.as_mut().ok_or("no active tmux session")?;
    writeln!(session.stdin, "{cmd}").map_err(|e| e.to_string())?;
    session.stdin.flush().map_err(|e| e.to_string())?;
    Ok(())
}

/// pane 에 hex 바이트 시퀀스 전송 (UTF-8 인코딩 → hex).
#[tauri::command]
fn send_keys_hex(
    pane_id: Option<String>,
    hex: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut guard = state.session.lock().map_err(|e| e.to_string())?;
    let session = guard.as_mut().ok_or("no active tmux session")?;
    let target = pane_id
        .as_deref()
        .map(|p| format!("-t '{p}' "))
        .unwrap_or_default();
    writeln!(session.stdin, "send-keys {target}-H {hex}")
        .map_err(|e| e.to_string())?;
    session.stdin.flush().map_err(|e| e.to_string())?;
    Ok(())
}

/// 화면 크기 변경 — vt100 parser 와 tmux 둘 다 통보.
#[tauri::command]
fn resize_client(
    cols: u16,
    rows: u16,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut guard = state.session.lock().map_err(|e| e.to_string())?;
    let session = guard.as_mut().ok_or("no active tmux session")?;
    // tmux client size
    writeln!(session.stdin, "refresh-client -C {cols}x{rows}")
        .map_err(|e| e.to_string())?;
    session.stdin.flush().map_err(|e| e.to_string())?;
    // vt100 parser 들 모두 resize
    if let Ok(mut map) = session.parsers.lock() {
        for parser in map.values_mut() {
            parser.set_size(rows, cols);
        }
    }
    Ok(())
}

/// detach. 세션은 백그라운드 살아있음. 다음에 같은 cwd 열면 이어붙음.
#[tauri::command]
fn detach_tmux(state: State<'_, AppState>) -> Result<(), String> {
    let mut guard = state.session.lock().map_err(|e| e.to_string())?;
    if let Some(mut session) = guard.take() {
        let _ = writeln!(session.stdin, "detach-client");
        let _ = session.stdin.flush();
        let _ = session.child.wait();
    }
    Ok(())
}

fn session_name_for_path(path: &str) -> String {
    let basename = path.rsplit('/').find(|s| !s.is_empty()).unwrap_or("root");
    let mut hash: u64 = 1469598103934665603;
    for b in path.bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(1099511628211);
    }
    let short = format!("{:x}", hash & 0xFFFFFF);
    let safe_base: String = basename
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    format!("tmuxify-{}-{}", safe_base, short)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            start_tmux,
            send_tmux_cmd,
            send_keys_hex,
            resize_client,
            detach_tmux
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
