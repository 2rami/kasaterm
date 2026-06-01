//! End-to-end probe for pty-backend. Spawns a shell into a real PTY,
//! sends `printf hello; exit\n`, drains the ScreenUpdate stream until
//! EOF, and prints the visible first row. If "hello" shows up there,
//! the byte → VT → ScreenUpdate path is wired correctly.

use kasa_pty::{PtyOptions, PtySession};

fn main() -> anyhow::Result<()> {
    let session = PtySession::start(PtyOptions {
        cols: 80,
        rows: 10,
        ..Default::default()
    })?;
    // Give the shell a beat to land its prompt, then send our probe.
    std::thread::sleep(std::time::Duration::from_millis(400));
    session.send_bytes(b"printf 'hello-from-pty\\n'; exit\n")?;
    // Drain until the channel closes (shell exit). The latest update
    // carries the final visible grid.
    let mut last = None;
    while let Ok(update) = session.screens.recv() {
        last = Some(update);
    }
    let update = last.expect("at least one ScreenUpdate before exit");
    println!("rows={} cols={}", update.rows, update.cols);
    for (r, row) in &update.dirty {
        let line: String = row.iter().map(|c| c.ch).collect();
        let trimmed = line.trim_end();
        if !trimmed.is_empty() {
            println!("[{r}] {trimmed}");
        }
    }
    Ok(())
}
