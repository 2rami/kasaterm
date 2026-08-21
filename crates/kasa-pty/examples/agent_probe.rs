//! 셸 pid 하나를 주면 그 아래 무슨 하네스가 도는지 답한다 — 표에 하네스를
//! 새로 넣을 때 **실제로 띄워 재는** 도구다.
//!
//! 이게 필요한 이유는 2026-08-21 실측이 말해 준다: Orca 의 표를 그대로 옮겼더니
//! gemini·cursor·hermes 는 프로세스 이름이 `node`·`Python` 이라 하나도 안 잡혔다.
//! 코드만 읽어서는 「붙었다」로 보이고, 화면에도 조용히 학생만 안 서는 실패라
//! 알아채기 어렵다. 새 줄을 넣었으면 반드시 여기로 한 번 재라.
//!
//! ```text
//! # 재고 싶은 프로그램을 pty 로 띄운 뒤 그 셸 pid 로 묻는다
//! script -q /dev/null opencode &          # 그 script 의 pid 가 곧 셸 pid
//! cargo run -p kasa-pty --example agent_probe -- <pid>
//! ```
//!
//! 인자를 안 주면 지금 살아 있는 프로세스를 전부 훑어 잡히는 것만 보여준다.
fn main() {
    let table = kasa_pty::process_table();
    let args: Vec<String> = std::env::args().skip(1).collect();

    if let Some(pid) = args.first().and_then(|a| a.parse::<u32>().ok()) {
        match kasa_pty::agent_for_shell(&table, pid) {
            Some(k) => println!("{pid} → {} ({})", k.as_str(), k.label()),
            None => println!("{pid} → (하네스 아님)"),
        }
        // 안 잡혔을 때 **왜** 안 잡혔는지가 이 도구의 값어치다. 이름과 명령줄을
        // 그대로 보여주고, 표의 어느 조각과도 안 맞았음을 눈으로 확인시킨다.
        if args.iter().any(|a| a == "--why") {
            let kids = |parent: u32| -> Vec<(u32, String)> {
                table
                    .iter()
                    .filter(|(_, pp, _)| *pp == parent)
                    .map(|(p, _, n)| (*p, n.clone()))
                    .collect()
            };
            for (cp, cn) in kids(pid) {
                let ca = kasa_pty::process_cmdline(cp).unwrap_or_default();
                println!("  자식 {cp} comm={cn}");
                println!("       argv={ca}");
                println!("       표 일치: {}", matched(&cn, &ca));
                for (gp, gn) in kids(cp) {
                    let ga = kasa_pty::process_cmdline(gp).unwrap_or_default();
                    println!("    손자 {gp} comm={gn}");
                    println!("         argv={ga}");
                    println!("         표 일치: {}", matched(&gn, &ga));
                }
            }
        }
        return;
    }

    println!("표에 실린 하네스 {}종\n", kasa_pty::AGENT_TABLE.len() + 3);
    let mut hits = 0;
    for (pid, _, _) in table.iter() {
        if let Some(k) = kasa_pty::agent_for_shell(&table, *pid) {
            println!("  pid {pid:>7} → {:<14} {}", k.as_str(), k.label());
            hits += 1;
        }
    }
    if hits == 0 {
        println!("  (지금 도는 하네스 없음)");
    }
}

/// 이 이름·명령줄이 표의 어느 줄에 걸리는지 — 진단 출력용.
fn matched(comm: &str, argv: &str) -> String {
    let base = comm.rsplit(['/', '\\']).next().unwrap_or(comm);
    let by_name = kasa_pty::AGENT_TABLE
        .iter()
        .find(|s| s.procs.contains(&base))
        .map(|s| format!("이름→{}", s.id));
    let by_argv = kasa_pty::AGENT_TABLE
        .iter()
        .find(|s| s.argv_hints.iter().any(|h| argv.contains(h)))
        .map(|s| format!("명령줄→{}", s.id));
    match (by_name, by_argv) {
        (None, None) => "없음".into(),
        (a, b) => [a, b].into_iter().flatten().collect::<Vec<_>>().join(" / "),
    }
}
