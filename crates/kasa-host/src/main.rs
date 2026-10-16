use std::path::PathBuf;

use anyhow::{bail, Result};

const USAGE: &str = "kasa-host --socket <경로>
창 없는 카사텀 — 셸 세션을 붙들고 그 소켓으로 내준다. 요청은 한 줄 JSON {id, method, params}.";

fn main() {
    if let Err(e) = run() {
        eprintln!("kasa-host: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut socket = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--socket" => socket = args.next().map(PathBuf::from),
            "--version" => {
                println!("kasa-host {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            other => bail!("모르는 인자 {other}\n{USAGE}"),
        }
    }
    let Some(socket) = socket else { bail!("--socket 이 필요하다\n{USAGE}") };
    // 카사텀 칸에서 띄우면 그 칸의 소켓·칸 번호와 shim 폴더(PATH 맨 앞, 그리고 zsh 가 그 폴더의
    // rc 를 읽어 PATH 에 다시 붙이게 하는 ZDOTDIR)가 따라와, 여기서 띄운 세션의 `claude` 가 학생
    // 감싸개로 풀리고 훅이 카사텀 판에 보고한다 — 떼어 낸 세션이 다시 섞인다.
    let shim = |v: &std::ffi::OsStr| v.to_string_lossy().contains("/kasaterm-shim-");
    for (key, _) in std::env::vars() {
        if key.starts_with("KASATERM_") {
            std::env::remove_var(key);
        }
    }
    if std::env::var_os("ZDOTDIR").is_some_and(|v| shim(&v)) {
        std::env::remove_var("ZDOTDIR");
    }
    if let Some(path) = std::env::var_os("PATH") {
        let kept: Vec<PathBuf> = std::env::split_paths(&path)
            .filter(|dir| !shim(dir.as_os_str()))
            .collect();
        std::env::set_var("PATH", std::env::join_paths(kept)?);
    }
    kasa_pty::set_host_policy(kasa_pty::HostPolicy {
        term_program: Some(("kasa-host".into(), env!("CARGO_PKG_VERSION").into())),
        ..Default::default()
    });
    let listener = kasa_host::server::bind(&socket)?;
    eprintln!("kasa-host {} — {}", env!("CARGO_PKG_VERSION"), socket.display());
    kasa_host::server::serve(listener, kasa_host::Host::new())
}
