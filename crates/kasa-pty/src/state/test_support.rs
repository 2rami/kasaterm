//! 여러 자식 모듈 시험이 함께 쓰는 세션 받침 — POSIX 셸 경로와 원격(External) 세션 리그.

use super::*;

pub(super) fn test_posix_shell() -> String {
    if cfg!(unix) {
        return "/bin/sh".to_string();
    }
    let mut cands = vec![
        std::path::PathBuf::from(r"C:\Program Files\Git\usr\bin\sh.exe"),
        std::path::PathBuf::from(r"C:\Program Files\Git\bin\sh.exe"),
    ];
    // 설치 자리가 달라도 따라가게: `<git>\cmd\git.exe` → `<git>\usr\bin\sh.exe`.
    if let Some(root) = std::process::Command::new("where")
        .arg("git")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout).lines().next().map(std::path::PathBuf::from)
        })
        .and_then(|p| Some(p.parent()?.parent()?.to_path_buf()))
    {
        cands.push(root.join(r"usr\bin\sh.exe"));
        cands.push(root.join(r"bin\sh.exe"));
    }
    for p in &cands {
        if p.exists() {
            return p.to_string_lossy().into_owned();
        }
    }
    panic!("POSIX 셸을 못 찾았다 — 이 테스트는 Git for Windows 의 sh.exe 가 필요하다: {cands:?}");
}

struct ChanWriter(std::sync::mpsc::Sender<Vec<u8>>);

impl Write for ChanWriter {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        let _ = self.0.send(b.to_vec());
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(super) fn ext_session(
    cols: u16,
    rows: u16,
) -> (
    Arc<PtySession>,
    crossbeam_channel::Sender<ExtEvent>,
    std::sync::mpsc::Receiver<Vec<u8>>,
    Arc<Mutex<Vec<(u16, u16)>>>,
) {
    let (etx, erx) = crossbeam_channel::unbounded();
    let (wtx, wrx) = std::sync::mpsc::channel::<Vec<u8>>();
    let resized: Arc<Mutex<Vec<(u16, u16)>>> = Default::default();
    let r2 = Arc::clone(&resized);
    let sess = PtySession::start_external(
        PtyOptions {
            cols,
            rows,
            pane_id: "rmt-test".into(),
            ..Default::default()
        },
        ExternalIo {
            events: erx,
            writer: Box::new(ChanWriter(wtx)),
            on_resize: Arc::new(move |c, r| r2.lock().unwrap().push((c, r))),
        },
    )
    .expect("start_external");
    (Arc::new(sess), etx, wrx, resized)
}

pub(super) fn wait_text(sess: &PtySession, needle: &str) -> bool {
    let deadline = Instant::now() + std::time::Duration::from_secs(2);
    while Instant::now() < deadline {
        if sess
            .screens
            .recv_timeout(std::time::Duration::from_millis(200))
            .is_ok()
            && sess.visible_text(50).contains(needle)
        {
            return true;
        }
    }
    false
}

pub(super) fn wait_rev(sess: &PtySession, above: u64) -> u64 {
    let deadline = Instant::now() + std::time::Duration::from_secs(2);
    while Instant::now() < deadline && sess.blocks_rev() <= above {
        let _ = sess.screens.recv_timeout(std::time::Duration::from_millis(50));
    }
    sess.blocks_rev()
}
