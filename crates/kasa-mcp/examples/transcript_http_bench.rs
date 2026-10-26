use anyhow::Result;
use kasa_socket::backend::{Backend, SurfaceInfo, TranscriptChunk, WorkspaceInfo};
use std::io::{BufRead, Write};
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

struct Fixture { raw: String, calls: AtomicUsize }
impl Backend for Fixture {
    fn list_workspaces(&self) -> Result<Vec<WorkspaceInfo>> { Ok(vec![]) }
    fn current_workspace(&self) -> Result<Option<WorkspaceInfo>> { Ok(None) }
    fn list_surfaces(&self) -> Result<Vec<SurfaceInfo>> { Ok(vec![]) }
    fn focus_surface(&self, _: &str) -> Result<()> { anyhow::bail!("fixture is read-only") }
    fn split_surface(&self, _: kasa_socket::SplitDirection, _: bool, _: Option<&str>) -> Result<SurfaceInfo> { anyhow::bail!("fixture is read-only") }
    fn send_key(&self, _: Option<&str>, _: &str) -> Result<()> { anyhow::bail!("fixture is read-only") }
    fn send_text(&self, _: Option<&str>, _: &str) -> Result<()> { anyhow::bail!("fixture is read-only") }
    fn transcript_raw(&self, _: &str, offset: u64) -> Result<TranscriptChunk> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(TranscriptChunk { raw: if offset == 0 { self.raw.clone() } else { String::new() },
            offset: self.raw.len() as u64, reset: offset == 0, source: "fixture.jsonl".into() })
    }
}

#[cfg(unix)]
fn cpu_seconds() -> f64 {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 { return 0.0; }
    let u = unsafe { usage.assume_init() };
    u.ru_utime.tv_sec as f64 + u.ru_stime.tv_sec as f64
        + (u.ru_utime.tv_usec + u.ru_stime.tv_usec) as f64 / 1_000_000.0
}
#[cfg(not(unix))]
fn cpu_seconds() -> f64 { 0.0 }

fn main() -> Result<()> {
    anyhow::ensure!(std::env::var_os("KASATERM_SETTINGS_FILE").is_some()
        && std::env::var_os("KASATERM_COLLAB_ROOT").is_some(), "isolated settings and collaboration root required");
    let bytes = std::env::args().nth(1).unwrap_or_else(|| "524288".into()).parse::<usize>()?.min(8 * 1024 * 1024);
    let line = "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"한글 응답, \\\"따옴표\\\"와 줄바꿈\\n다음 줄\"}]}}\n";
    let backend = Arc::new(Fixture { raw: line.repeat(bytes / line.len() + 1), calls: AtomicUsize::new(0) });
    let port = kasa_mcp::spawn_http_server_opts(backend.clone(), 0, false)?;
    println!("{}", serde_json::json!({"port":port,"raw_bytes":backend.raw.len()}));
    std::io::stdout().flush()?;
    let mut baseline = cpu_seconds();
    for line in std::io::stdin().lock().lines() {
        match line?.as_str() {
            "start" => { backend.calls.store(0, Ordering::Relaxed); baseline = cpu_seconds(); }
            "stop" => {
                println!("{}", serde_json::json!({"cpu_seconds":cpu_seconds()-baseline,"calls":backend.calls.load(Ordering::Relaxed)}));
                std::io::stdout().flush()?;
            }
            "quit" => break,
            _ => anyhow::bail!("expected start, stop, or quit"),
        }
    }
    Ok(())
}
