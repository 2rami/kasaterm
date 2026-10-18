//! 카사텀 소켓으로 짝 셸과 잇는다. 부르는 일은 일꾼 스레드가 해서 화면이 소켓을 기다리지 않는다.

use anyhow::{anyhow, Context, Result};
use kasa_socket::transport::LocalStream;
use kasa_socket::{Request, Response};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};

pub enum Ask {
    Cwd,
    Cd(PathBuf),
    Open(PathBuf),
}

pub enum Answer {
    Cwd(Result<PathBuf>),
    Cd(Result<()>),
    Open(Result<()>),
}

fn socket_path() -> Option<PathBuf> {
    std::env::var_os("KASATERM_SOCKET_PATH")
        .or_else(|| std::env::var_os("CMUX_SOCKET_PATH"))
        .map(PathBuf::from)
}

fn call(socket: &Path, method: &str, params: Value) -> Result<Value> {
    let stream = LocalStream::connect(socket).with_context(|| format!("카사텀에 닿지 못했어요 ({})", socket.display()))?;
    let mut writer = stream.try_clone()?;
    let request = Request { id: json!("kasa-files"), method: method.into(), params };
    let mut line = serde_json::to_string(&request)?;
    line.push('\n');
    writer.write_all(line.as_bytes())?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply)?;
    let response: Response = serde_json::from_str(reply.trim()).context("카사텀 답을 읽지 못했어요")?;
    if response.ok {
        Ok(response.result.unwrap_or(Value::Null))
    } else {
        Err(anyhow!(response.error.map_or_else(|| "카사텀이 거절했어요".to_string(), |e| e.message)))
    }
}

/// 따라갈 셸 칸이 있을 때만 잇는다. 없으면 그 자리에서 혼자 돈다.
pub fn spawn(follow: Option<String>) -> Option<(Sender<Ask>, Receiver<Answer>)> {
    let socket = socket_path()?;
    let follow = follow?;
    let (ask, asks) = std::sync::mpsc::channel::<Ask>();
    let (tell, answers) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for ask in asks {
            let answer = match ask {
                Ask::Cwd => Answer::Cwd(
                    call(&socket, "surface.cwd", json!({ "surface": follow })).and_then(|v| {
                        v.get("cwd").and_then(Value::as_str).map(PathBuf::from).ok_or_else(|| anyhow!("셸 폴더를 모르겠어요"))
                    }),
                ),
                Ask::Cd(path) => Answer::Cd(
                    call(&socket, "surface.cd", json!({ "surface": follow, "path": path })).map(|_| ()),
                ),
                // 짝 셸 칸의 탭으로 연다 — 좁은 파일 칸에 붙으면 읽을 자리가 없다.
                Ask::Open(path) => Answer::Open(
                    call(&socket, "surface.open_preview", json!({ "kind": "file", "path": path, "target": follow }))
                        .map(|_| ()),
                ),
            };
            if tell.send(answer).is_err() {
                break;
            }
        }
    });
    Some((ask, answers))
}

/// 시작할 때 한 번은 기다려서 읽는다 — 첫 화면이 엉뚱한 폴더로 뜨지 않게.
pub fn first_cwd(follow: &str) -> Option<PathBuf> {
    let socket = socket_path()?;
    call(&socket, "surface.cwd", json!({ "surface": follow }))
        .ok()?
        .get("cwd")?
        .as_str()
        .map(PathBuf::from)
}
