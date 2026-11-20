//! 화면 서비스 — 이 기기 화면 스트림의 보내는 쪽을 카사텀 앱 밖에서 돌린다(`docs/screen-service-split.md`). 앱을 다시 켜거나
//! 업데이트해도 보던 화면이 이어지고, 화면 권한이 자주 안 바뀌는 이 작은 실행 파일에 붙는다.
//!
//! - Windows: `--screen-service`(LocalSystem 서비스 `KasatermScreen`, 세션 0 — 카사넷 화면 노드·제어 파이프·콘솔 세션 도우미 감독)와
//!   `--screen-agent`(콘솔 세션 SYSTEM 도우미 — 뜨기·압축·조작·소리, `kasa_cast_host`).
//! - macOS: 사용자 LaunchAgent(단계 P4).
#![cfg_attr(all(windows, not(debug_assertions), not(test)), windows_subsystem = "windows")]

// 맥 화면 서비스(LaunchAgent)는 단계 P4 에서 같은 제어·노드·상태를 쓴다 — 그때까지 맥 빌드에서는 시험만 부른다.
#[cfg_attr(not(windows), allow(dead_code))]
mod ctl;
#[cfg_attr(not(windows), allow(dead_code))]
mod node;
mod probe;
#[cfg_attr(not(windows), allow(dead_code))]
mod state;
#[cfg(windows)]
mod win;

/// 서비스 판(`Cargo.toml` 의 판) — 앱 판과 따로 센다.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let words: Vec<String> = args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
    if matches!(words.first().map(String::as_str), Some("--probe-id" | "--probe-watch")) {
        std::process::exit(probe::run(&words));
    }
    #[cfg(windows)]
    if kasa_cast_host::service::run_with(&args, win::serve) {
        return;
    }
    eprintln!("kasaterm-screen {VERSION} — 서비스 관리자가 띄운다(Windows `--screen-service`·`--screen-agent`)");
    let _ = args;
    std::process::exit(2);
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

    /// 서비스 판이 그 코드를 따라가는지 — 서비스 코드가 바뀌었는데 판을 안 올리면 MSI 가 실행 파일을 안 갈아 끼워 새 코드가
    /// 기기에 안 간다. 판을 올렸으면 `source-hash.txt` 를 이 시험이 알려 주는 값으로 고친다.
    #[test]
    fn service_version_follows_its_source() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut files = Vec::new();
        for dir in ["app/kasaterm-screen", "crates/kasa-cast", "crates/kasa-cast-host", "crates/kasa-net"] {
            collect(&root.join(dir), &mut files);
        }
        files.sort();
        let mut h = Sha256::new();
        for f in &files {
            let rel = f.strip_prefix(&root).unwrap_or(f).to_string_lossy().replace('\\', "/");
            if rel.ends_with("source-hash.txt") {
                continue;
            }
            h.update(rel.as_bytes());
            h.update(std::fs::read(f).unwrap_or_default().iter().filter(|b| **b != b'\r').copied().collect::<Vec<u8>>());
        }
        let hash: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        let want = format!("{} {hash}", super::VERSION);
        let recorded = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("source-hash.txt")).unwrap_or_default();
        assert_eq!(
            recorded.trim(),
            want,
            "서비스 코드가 바뀌었다 — app/kasaterm-screen/Cargo.toml 의 판을 올리고 source-hash.txt 를 「<판> <해시>」로 고쳐라"
        );
    }

    fn collect(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                if p.file_name().is_some_and(|n| n != "target") {
                    collect(&p, out);
                }
            } else if p.extension().is_some_and(|x| x == "rs" || x == "toml") {
                out.push(p);
            }
        }
    }
}
