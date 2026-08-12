//! 하네스별 "이어가기" 명령이 **실제로 뭐로 나가는지** 찍는다.
//! `cargo run -p kasa-socket --example resume_cmd`
//!
//! 목록은 멀쩡한데 골라도 안 열릴 때 여기부터 돌린다. 목록(어떤 세션이 있나)과
//! 명령(그걸 무슨 줄로 여나)은 따로 틀릴 수 있고, 화면만 봐서는 어느 쪽인지
//! 안 갈린다 — 찍힌 줄을 그대로 셸에 붙여 보면 바로 끝난다.
//!
//! ⚠️ CLI 플래그는 하네스가 바꾼다. 2026-08 실측 기준
//! `codex resume <UUID>` · `agy --conversation <ID>` · `claude --resume <UUID>` 이고,
//! codex 는 cwd 로 세션을 거르므로 `cd` 를 먼저 하는 것이 형식이 아니라 조건이다.

use kasa_socket::sessions::{recent_all_sessions, resume_command};

fn main() {
    let n: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(12);
    for s in recent_all_sessions(n) {
        println!(
            "{:<7} {:<34} {}",
            s.harness,
            s.label.chars().take(32).collect::<String>(),
            resume_command(&s.harness, &s.id, &s.cwd)
        );
    }
}
