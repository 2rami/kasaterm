//! 무중단 핸드오프의 **받는 창구** — GUI 가 산 채로 넘기는 PTY(fd)를 SCM_RIGHTS
//! 로 받아 `PtySession::adopt` 로 입양하고, keep_session 으로 붙들어 둔다.
//!
//! HTTP 로는 fd 를 못 보낸다 — 전용 unix 소켓(`$TMPDIR/kasa-adopt-<port>.sock`)
//! 을 쓴다. 프로토콜은 한 왕복이다:
//!   client → sendmsg(ancillary=fd, body=[8바이트 LE 길이][JSON 헤더])
//!   server → "{\"ok\":true,\"id\":\"web-…\"}\n"
//! JSON 헤더: {child_pid, cols, rows, scrollback: [String]} — 스크롤백은 넘긴
//! 쪽 Term 의 텍스트 재생이라 커서·색은 잃지만(초기 재생 경로의 기존 한계),
//! 입양 직후 TUI 가 계속 다시 그리므로 화면은 스스로 아문다.
#![cfg(unix)]

use std::io::{BufRead, Read, Write};
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};

use anyhow::{anyhow, Context, Result};

/// 입양 소켓 경로 — HTTP 포트와 짝지어 인스턴스를 가른다.
pub fn adopt_sock_path(port: u16) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("kasa-adopt-{port}.sock"))
}

/// fd 하나와 길이-접두 JSON 을 한 sendmsg 로 보낸다. 본문이 한 세그먼트를
/// 넘으면 나머지는 보통 write 로 잇는다(ancillary 는 첫 세그먼트에 실린다).
fn send_fd_with_body(sock: &UnixStream, fd: RawFd, body: &[u8]) -> Result<()> {
    let mut framed = Vec::with_capacity(8 + body.len());
    framed.extend_from_slice(&(body.len() as u64).to_le_bytes());
    framed.extend_from_slice(body);
    // 첫 세그먼트: ancillary 와 함께 최대 32KB.
    let first = framed.len().min(32 * 1024);
    let iov = libc::iovec {
        iov_base: framed.as_ptr() as *mut _,
        iov_len: first,
    };
    let mut cbuf = [0u8; 64];
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &iov as *const _ as *mut _;
    msg.msg_iovlen = 1;
    msg.msg_control = cbuf.as_mut_ptr() as *mut _;
    msg.msg_controllen = unsafe { libc::CMSG_SPACE(4) } as _;
    let sent = unsafe {
        let c = libc::CMSG_FIRSTHDR(&msg);
        (*c).cmsg_level = libc::SOL_SOCKET;
        (*c).cmsg_type = libc::SCM_RIGHTS;
        (*c).cmsg_len = libc::CMSG_LEN(4) as _;
        std::ptr::copy_nonoverlapping(
            &fd as *const RawFd as *const u8,
            libc::CMSG_DATA(c),
            4,
        );
        libc::sendmsg(sock.as_raw_fd(), &msg, 0)
    };
    anyhow::ensure!(sent >= 0, "sendmsg 실패: {}", std::io::Error::last_os_error());
    let mut off = sent as usize;
    while off < framed.len() {
        let n = (&mut &*sock)
            .write(&framed[off..])
            .context("본문 이어쓰기")?;
        anyhow::ensure!(n > 0, "본문 이어쓰기 0바이트");
        off += n;
    }
    Ok(())
}

/// recvmsg 로 fd 와 첫 세그먼트를 받고, 길이 접두만큼 본문을 마저 읽는다.
fn recv_fd_with_body(sock: &UnixStream) -> Result<(std::os::fd::OwnedFd, Vec<u8>)> {
    let mut buf = vec![0u8; 64 * 1024];
    let iov = libc::iovec {
        iov_base: buf.as_mut_ptr() as *mut _,
        iov_len: buf.len(),
    };
    let mut cbuf = [0u8; 64];
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &iov as *const _ as *mut _;
    msg.msg_iovlen = 1;
    msg.msg_control = cbuf.as_mut_ptr() as *mut _;
    msg.msg_controllen = cbuf.len() as _;
    let n = unsafe { libc::recvmsg(sock.as_raw_fd(), &mut msg, 0) };
    anyhow::ensure!(n > 0, "recvmsg 실패: {}", std::io::Error::last_os_error());
    let mut fd: RawFd = -1;
    unsafe {
        let mut c = libc::CMSG_FIRSTHDR(&msg);
        while !c.is_null() {
            if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_RIGHTS {
                std::ptr::copy_nonoverlapping(
                    libc::CMSG_DATA(c),
                    &mut fd as *mut RawFd as *mut u8,
                    4,
                );
                break;
            }
            c = libc::CMSG_NXTHDR(&msg, c);
        }
    }
    anyhow::ensure!(fd >= 0, "ancillary 에 fd 가 없다");
    let owned = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
    let mut got = buf[..n as usize].to_vec();
    anyhow::ensure!(got.len() >= 8, "길이 접두 미달");
    let want = u64::from_le_bytes(got[..8].try_into().unwrap()) as usize;
    got.drain(..8);
    while got.len() < want {
        let mut chunk = vec![0u8; (want - got.len()).min(64 * 1024)];
        let n = (&mut &*sock).read(&mut chunk).context("본문 이어읽기")?;
        anyhow::ensure!(n > 0, "본문이 끊겼다");
        got.extend_from_slice(&chunk[..n]);
    }
    Ok((owned, got))
}

/// 서버: 입양 소켓을 연다. HTTP 서버가 뜰 때 함께 부른다.
pub fn spawn_adopt_listener(port: u16) -> Result<()> {
    let path = adopt_sock_path(port);
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)
        .with_context(|| format!("입양 소켓 bind: {}", path.display()))?;
    // 같은 사용자만 — remote-token 과 같은 근거(로컬 = 같은 권한).
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    std::thread::Builder::new()
        .name(format!("kasa-adopt-{port}"))
        .spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { continue };
                if let Err(e) = handle_conn(&conn) {
                    eprintln!("[adopt] 입양 실패: {e:#}");
                    let _ = (&mut &conn)
                        .write_all(format!("{{\"ok\":false,\"error\":{:?}}}\n", e.to_string()).as_bytes());
                }
            }
        })
        .context("adopt 스레드")?;
    Ok(())
}

fn handle_conn(conn: &UnixStream) -> Result<()> {
    let (fd, body) = recv_fd_with_body(conn)?;
    let v: serde_json::Value = serde_json::from_slice(&body).context("헤더 JSON")?;
    let child_pid = v.get("child_pid").and_then(|x| x.as_u64()).map(|p| p as u32);
    let cols = v.get("cols").and_then(|x| x.as_u64()).unwrap_or(80) as u16;
    let rows = v.get("rows").and_then(|x| x.as_u64()).unwrap_or(24) as u16;
    let scrollback: Vec<String> = v
        .get("scrollback")
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|l| l.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let id = format!("web-{}", uuid::Uuid::new_v4());
    let sess = kasa_pty::PtySession::adopt(
        kasa_pty::PtyOptions {
            cols,
            rows,
            pane_id: id.clone(),
            initial_scrollback: scrollback,
            ..Default::default()
        },
        fd,
        child_pid,
    )?;
    let sess = std::sync::Arc::new(sess);
    kasa_pty::register_session(&id, &sess);
    // 크기 지글(SIGWINCH ×2) — 핸드오프로 받은 새 그리드는 비어 있는데, 안의
    // TUI(claude)는 자기 damage 추적을 믿고 아래 몇 줄만 다시 그린다(실측:
    // 입력박스·상태줄만 남고 대화가 안 보였다). 전체 재도색을 강제한다.
    {
        let s2 = std::sync::Arc::clone(&sess);
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            let (c, r) = s2.size();
            if r > 2 {
                let _ = s2.resize(c, r - 1);
                std::thread::sleep(std::time::Duration::from_millis(150));
                let _ = s2.resize(c, r);
            }
        });
    }
    kasa_pty::keep_session(&id, sess);
    (&mut &*conn)
        .write_all(format!("{{\"ok\":true,\"id\":\"{id}\"}}\n").as_bytes())
        .context("응답 쓰기")?;
    Ok(())
}

/// 클라이언트(GUI): fd 와 메타를 보내고 입양된 세션 id 를 받는다.
pub fn handoff_to(
    port: u16,
    fd: RawFd,
    child_pid: Option<u32>,
    cols: u16,
    rows: u16,
    scrollback: Vec<String>,
) -> Result<String> {
    let path = adopt_sock_path(port);
    let sock = UnixStream::connect(&path)
        .with_context(|| format!("입양 소켓 연결: {}", path.display()))?;
    sock.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
    let body = serde_json::json!({
        "child_pid": child_pid,
        "cols": cols,
        "rows": rows,
        "scrollback": scrollback,
    })
    .to_string();
    send_fd_with_body(&sock, fd, body.as_bytes())?;
    let mut line = String::new();
    std::io::BufReader::new(&sock)
        .read_line(&mut line)
        .context("응답 읽기")?;
    let v: serde_json::Value = serde_json::from_str(line.trim()).context("응답 JSON")?;
    if v.get("ok").and_then(|x| x.as_bool()) != Some(true) {
        return Err(anyhow!(
            "입양 거부: {}",
            v.get("error").and_then(|x| x.as_str()).unwrap_or("?")
        ));
    }
    v.get("id")
        .and_then(|x| x.as_str())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("응답에 id 가 없다"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 소켓 경계를 건너는 핸드오프 전 구간 — 실제 pty fd 가 SCM_RIGHTS 로 넘어가
    /// 입양되고, 넘긴 뒤에도 같은 셸과 왕복이 된다.
    #[test]
    fn socket_handoff_roundtrip() {
        let port = 39990; // 테스트 전용 — 소켓 파일명에만 쓰인다
        spawn_adopt_listener(port).expect("listener");
        std::thread::sleep(std::time::Duration::from_millis(100));
        let a = kasa_pty::PtySession::start(kasa_pty::PtyOptions {
            cols: 60,
            rows: 12,
            pane_id: "sock-hand-a".into(),
            ..Default::default()
        })
        .expect("start");
        a.send_bytes(b"HAND=sock-7; echo go-$HAND\r").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
        while !a.visible_text(30).contains("go-sock-7") {
            assert!(std::time::Instant::now() < deadline, "셸 에코 실패");
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let pid = a.shell_pid();
        a.stop_reader();
        std::thread::sleep(std::time::Duration::from_millis(400));
        let scroll = a.scrollback_text(100);
        let raw = a.master_raw_fd().expect("fd");
        let id = handoff_to(port, raw, pid, 60, 12, scroll).expect("handoff");
        assert!(id.starts_with("web-"));
        a.disarm_kill();
        drop(a);
        let b = kasa_pty::lookup_session(&id).expect("입양 세션 조회");
        b.send_bytes(b"echo back-$HAND\r").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
        while !b.visible_text(30).contains("back-sock-7") {
            assert!(
                std::time::Instant::now() < deadline,
                "입양 뒤 왕복 실패: {:?}",
                b.visible_text(20)
            );
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(kasa_pty::kept_sessions().contains(&id));
        kasa_pty::release_session(&id);
    }
}
