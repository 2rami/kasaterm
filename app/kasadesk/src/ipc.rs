//! 하나만 띄우기 — 이미 떠 있으면 새로 띄운 것은 「이 기기 열어」 한 줄만 넘기고 끝난다. 본 앱의 「화면 보기」도 이 길로 넘긴다.
//! 길은 상태 폴더의 유닉스 소켓(맥)·사용자 이름 붙은 이름 파이프(Windows). 한 줄 JSON `{"open": "<기기>" | null}`, 답은 `ok`.

use std::path::Path;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[cfg(windows)]
fn pipe_name(state: &Path) -> String {
    // 상태 폴더마다 갈라 격리 리그가 사람 보기 앱에 붙지 않게.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in state.to_string_lossy().to_lowercase().bytes() {
        h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
    }
    format!(r"\\.\pipe\kasadesk-{h:016x}")
}

/// 떠 있는 보기 앱에 넘긴다. 넘겼으면 true — 이 프로세스는 끝난다.
pub async fn hand_over(state: &Path, open: Option<&str>) -> bool {
    let line = format!("{}\n", json!({ "open": open }));
    #[cfg(unix)]
    let stream = tokio::net::UnixStream::connect(state.join("kasadesk.sock")).await.ok();
    #[cfg(windows)]
    let stream = tokio::net::windows::named_pipe::ClientOptions::new().open(pipe_name(state)).ok();
    let Some(mut stream) = stream else { return false };
    if stream.write_all(line.as_bytes()).await.is_err() {
        return false;
    }
    let mut answer = String::new();
    BufReader::new(stream).read_line(&mut answer).await.is_ok() && answer.trim() == "ok"
}

/// 이 프로세스가 보기 앱이 된다 — 받은 「열어」를 `on` 으로 넘긴다. 이미 다른 프로세스가 쥐었으면 Err.
pub fn listen(state: &Path, on: impl Fn(Option<String>) + Send + Sync + 'static) -> std::io::Result<()> {
    let on = std::sync::Arc::new(on);
    #[cfg(unix)]
    {
        let path = state.join("kasadesk.sock");
        // 넘기기가 실패했으니 남은 소켓 파일은 죽은 프로세스 것이다.
        let _ = std::fs::remove_file(&path);
        let listener = tokio::net::UnixListener::bind(&path)?;
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve(stream, on.clone()));
            }
        });
    }
    #[cfg(windows)]
    {
        use tokio::net::windows::named_pipe::ServerOptions;
        let name = pipe_name(state);
        let mut server = ServerOptions::new().first_pipe_instance(true).create(&name)?;
        tokio::spawn(async move {
            loop {
                if server.connect().await.is_err() {
                    return;
                }
                let Ok(next) = ServerOptions::new().create(&name) else { return };
                let stream = std::mem::replace(&mut server, next);
                tokio::spawn(serve(stream, on.clone()));
            }
        });
    }
    Ok(())
}

async fn serve<S>(stream: S, on: std::sync::Arc<impl Fn(Option<String>) + Send + Sync + 'static>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (read, mut write) = tokio::io::split(stream);
    let mut line = String::new();
    if BufReader::new(read).read_line(&mut line).await.is_err() {
        return;
    }
    let Ok(v) = serde_json::from_str::<Value>(&line) else { return };
    on(v.get("open").and_then(Value::as_str).map(str::to_string));
    let _ = write.write_all(b"ok\n").await;
}
