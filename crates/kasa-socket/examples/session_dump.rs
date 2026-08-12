//! 통합 세션 스캐너가 실제 디스크에서 무엇을 건지는지 눈으로 보는 도구.
//! `cargo run -p kasa-socket --example session_dump`
//!
//! UI 를 거치지 않고 데이터 계층만 확인할 수 있어야 한다 — 목록이 이상할 때
//! 피커를 의심하기 전에 여기부터 돌리면 스캐너 문제인지 바로 갈린다.
//! 실제로 이걸로 세 가지를 잡았다: 한글 라벨에서 String::truncate 가 바이트
//! 경계를 끊어 panic, codex 의 빈 세션(헤더 한 줄)이 목록을 uuid 로 채움,
//! claude 전체 스캔이 title-gen 내부 세션까지 끌어옴.

fn main() {
    for (name, list) in [
        ("claude", kasa_socket::sessions::recent_claude_sessions_all(5)),
        ("codex", kasa_socket::sessions::recent_codex_sessions(5)),
        ("agy", kasa_socket::sessions::recent_agy_sessions(5)),
    ] {
        println!("=== {name} ({}) ===", list.len());
        for s in &list {
            let cwd = if s.cwd.is_empty() { "-".into() } else { s.cwd.clone() };
            let label: String = s.label.chars().take(50).collect();
            println!("  [{}] {}  {}", s.mtime, label, cwd);
        }
    }
    let all = kasa_socket::sessions::recent_all_sessions(10);
    println!("\n=== 통합 최신순 ({}) ===", all.len());
    for s in &all {
        let label: String = s.label.chars().take(46).collect();
        println!("  {:<7} {}", s.harness, label);
    }
}
