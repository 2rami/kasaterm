use std::process::{Command, Output};

fn cli(args: &[&str], socket: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_kasaterm-cli"))
        .args(args)
        .env_clear()
        .env("KASATERM_SOCKET_PATH", socket)
        .env("CMUX_SOCKET_PATH", socket)
        .current_dir(std::env::temp_dir())
        .output()
        .expect("run CLI without a real app or pane")
}

/// 살아 있는 소켓을 세워 두고 읽어도 아무도 붙지 않는다 — 앱이 켜져 있어도 guide 는 앱에 묻지 않는다.
#[cfg(unix)]
#[test]
fn guide_reads_bundled_text_without_touching_the_app_socket() {
    let dir = std::env::temp_dir().join(format!("kt-guide-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("app.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let socket = socket.to_str().unwrap();

    let list = cli(&["guide"], socket);
    assert!(list.status.success(), "{}", String::from_utf8_lossy(&list.stderr));
    let list = String::from_utf8(list.stdout).unwrap();
    for args in [vec!["guide", "--help"], vec!["help", "guide"], vec!["--api", "http://127.0.0.1:1", "guide"]] {
        let out = cli(&args, socket);
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(String::from_utf8(out.stdout).unwrap(), list, "{args:?}");
    }
    let mut seen = Vec::new();
    for name in ["collab", "panes", "jobs", "media", "transfer", "develop"] {
        assert!(list.contains(name), "{list}");
        let out = cli(&["guide", name], socket);
        assert!(out.status.success(), "{name}: {}", String::from_utf8_lossy(&out.stderr));
        let text = String::from_utf8(out.stdout).unwrap();
        assert!(!text.trim().is_empty() && !seen.contains(&text), "{name} 는 제 글만 낸다");
        assert_eq!(cli(&["help", "guide", name], socket).stdout, text.as_bytes());
        seen.push(text);
    }
    assert!(matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock), "guide 가 앱 소켓에 붙었다");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn guide_rejects_unknown_topics_and_stray_arguments() {
    for (args, expected) in [
        (vec!["guide", "nope"], "모르는 지침 주제: nope"),
        (vec!["guide", "collab", "extra"], "남는 인자: extra"),
        (vec!["help", "guide", "media", "jobs"], "남는 인자: jobs"),
    ] {
        let out = cli(&args, "/missing-kasa-guide-test/socket");
        let text = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {text}");
        assert!(text.contains(expected), "{args:?}: {text}");
        assert!(!text.contains("/missing-kasa-guide-test"), "contacted socket: {text}");
        assert!(out.stdout.is_empty());
    }
}

#[test]
fn full_help_lists_guide() {
    let out = cli(&["--help"], "/missing-kasa-guide-test/socket");
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("guide [collab|panes|jobs|media|transfer|develop]"));
}
