use std::process::{Command, Output};

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_kasaterm-cli"))
        .args(args)
        .env_clear()
        .env("KASATERM_SOCKET_PATH", "/missing-kasa-move-test/socket")
        .env("CMUX_SOCKET_PATH", "/missing-kasa-move-test/fallback")
        .output()
        .expect("run CLI without a real app or pane")
}

#[test]
fn move_help_is_available_without_a_socket_or_pane() {
    for args in [
        vec!["machines", "move", "--help"],
        vec!["machines", "move", "-h"],
        vec!["help", "machines", "move"],
        vec!["--api", "http://127.0.0.1:1", "machines", "move", "--help"],
    ] {
        let output = cli(&args);
        let text = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{args:?}: {text}");
        assert!(text.contains("machines move %3"), "{text}");
        assert!(text.contains("예약됨은 이동 완료가 아닙니다"), "{text}");
        assert!(output.stdout.is_empty());
    }
    for args in [vec!["--help"], vec!["machines", "--help"]] {
        let output = cli(&args);
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("machines move --help"));
    }
}

#[test]
fn invalid_arguments_are_rejected_before_socket_resolution() {
    for (args, expected) in [
        (
            vec!["machines", "move", "%3", "mini", "--typo"],
            "알 수 없는 이동 옵션",
        ),
        (
            vec!["machines", "move", "%3", "mini", "--cwd"],
            "--cwd 뒤에",
        ),
        (
            vec!["machines", "move", "%3", "mini", "other"],
            "도착 기기는 하나만",
        ),
        (vec!["machines", "move", "mini"], "칸 밖에서는"),
        (vec!["machines", "move", "%3@mini", "local"], "이쪽 칸 번호"),
    ] {
        let output = cli(&args);
        let text = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(2), "{args:?}: {text}");
        assert!(text.contains(expected), "{args:?}: {text}");
        assert!(
            !text.contains("/missing-kasa-move-test"),
            "contacted socket: {text}"
        );
        assert!(output.stdout.is_empty());
    }
}
