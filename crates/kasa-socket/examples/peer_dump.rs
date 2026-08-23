//! cross-session 명부를 있는 그대로 찍는다.
//! `cargo run -p kasa-socket --example peer_dump`
//!
//! board 의 `reach` 가 이상할 때 여기부터 돌리면 명부 문제인지 배선 문제인지
//! 바로 갈린다. 특히 **명부 `name` 이 pane 이름과 다른 경우**를 눈으로 보라 —
//! `/rename` 한 세션이 그렇고, 이름으로 매칭하면 조용히 어긋나는 자리다.

use kasa_socket::peers::{reach_of, read_registry};

fn live_pids() -> std::collections::HashSet<u32> {
    std::process::Command::new("ps")
        .args(["-A", "-o", "pid="])
        .output()
        .ok()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter_map(|l| l.trim().parse().ok())
                .collect()
        })
        .unwrap_or_default()
}

fn main() {
    let alive = live_pids();
    let mut peers = read_registry();
    peers.sort_by(|a, b| a.name.cmp(&b.name));
    println!("명부 {}개  (살아있는 pid {}개)\n", peers.len(), alive.len());
    println!("  {:<26} {:>7}  {:<8} {}", "name", "pid", "reach", "sessionId");
    for p in &peers {
        // 명부를 훑는 도구라 peer 는 항상 Some 이다 — 그 경로에선 하네스
        // 유무가 판정을 안 바꾸므로 true 를 고정으로 넘긴다.
        let r = reach_of(Some(p), alive.contains(&p.pid), true);
        println!(
            "  {:<26} {:>7}  {:<8} {}",
            p.name,
            p.pid,
            r.as_str(),
            &p.session_id[..8.min(p.session_id.len())]
        );
    }
}
