//! 유닉스 소켓. 연결 하나에 스레드 하나, 줄 하나에 요청 하나 — 롱폴(`screen`)이 그 연결만
//! 붙잡고 다른 연결은 막지 않는다.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::Host;

/// 소켓을 연다. 살아 있는 호스트가 이미 그 자리를 쥐고 있으면 거절한다 — 둘이 뜨면 부르는
/// 쪽이 어느 쪽 세션을 보는지 갈린다. 죽은 호스트가 남긴 파일만 치운다.
pub fn bind(path: &Path) -> Result<UnixListener> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("{} 를 못 만들었다", dir.display()))?;
    }
    if path.exists() {
        if UnixStream::connect(path).is_ok() {
            bail!("{} 에 이미 kasa-host 가 떠 있다", path.display());
        }
        std::fs::remove_file(path).with_context(|| format!("남은 소켓 {} 를 못 지웠다", path.display()))?;
    }
    let listener = UnixListener::bind(path).with_context(|| format!("{} 에 못 붙었다", path.display()))?;
    // 이 소켓에 닿으면 세션에 무엇이든 칠 수 있다 — 띄운 사용자만.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

pub fn serve(listener: UnixListener, host: Arc<Host>) -> Result<()> {
    for conn in listener.incoming() {
        let Ok(stream) = conn else { continue };
        let host = host.clone();
        std::thread::Builder::new()
            .name("kasa-host-conn".into())
            .spawn(move || {
                let _ = talk(stream, &host);
            })
            .context("연결 스레드")?;
    }
    Ok(())
}

fn talk(stream: UnixStream, host: &Arc<Host>) -> Result<()> {
    let mut out = stream.try_clone()?;
    for line in BufReader::new(stream).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(req) => host.handle(&req),
            Err(e) => json!({"id": null, "ok": false, "error": format!("JSON 이 아니다: {e}")}),
        };
        writeln!(out, "{reply}")?;
        out.flush()?;
    }
    Ok(())
}
