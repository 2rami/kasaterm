//! 칸마다 「이 칸의 하네스는?」을 묻는 비용을 이 컴퓨터의 실제 프로세스 표로 잰다.
//!
//! 렌더 스레드는 그리는 칸마다 여러 자리에서 `active_agent` 를 묻는다. 표는 백그라운드
//! 스캔이 ~300ms 마다 갈아 끼우므로 그 사이의 물음은 같은 답이다. 여기서는 살아 있는
//! kasaterm 의 칸 셸(kasaterm.exe 의 자식) 마다 한 장에 `--per-frame` 번 묻는 것을
//! `--frames` 장 되풀이해, 표를 매번 훑는 판정과 표 한 장에 한 번만 판정하는 쪽을 견준다.
//!
//! ```text
//! cargo run --release -p kasa-pty --example agent_cost -- [--frames 2000] [--per-frame 6]
//! ```
use std::time::Instant;

fn arg(name: &str, default: usize) -> usize {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn main() {
    let frames = arg("--frames", 2000);
    let per_frame = arg("--per-frame", 6);
    let table = kasa_pty::process_table_shared();
    let apps: Vec<u32> = table
        .iter()
        .filter(|(_, _, name)| name.eq_ignore_ascii_case("kasaterm.exe") || name == "kasaterm")
        .map(|(pid, _, _)| *pid)
        .collect();
    let shells: Vec<u32> = table.iter().filter(|(_, ppid, _)| apps.contains(ppid)).map(|(pid, _, _)| *pid).collect();
    let agents = shells.iter().filter(|&&s| kasa_pty::agent_pid_for_shell(&table, s).is_some()).count();
    println!("프로세스 {}개, 칸 셸 {}개(하네스 {}개), 칸마다 한 장에 {}번 × {}장", table.len(), shells.len(), agents,
        per_frame, frames);
    if shells.is_empty() {
        println!("kasaterm 칸이 없어 잴 것이 없다");
        return;
    }

    let t = Instant::now();
    let mut seen = 0usize;
    for _ in 0..frames {
        for &shell in &shells {
            for _ in 0..per_frame {
                seen += kasa_pty::agent_pid_for_shell(&kasa_pty::process_table_shared(), shell).is_some() as usize;
            }
        }
    }
    let before = t.elapsed().as_secs_f64() * 1000.0 / frames as f64;

    let t = Instant::now();
    let mut seen_cached = 0usize;
    for _ in 0..frames {
        for &shell in &shells {
            for _ in 0..per_frame {
                seen_cached += kasa_pty::agent_pid_for_shell_shared(shell).is_some() as usize;
            }
        }
    }
    let after = t.elapsed().as_secs_f64() * 1000.0 / frames as f64;
    assert_eq!(seen, seen_cached, "같은 표에서 답이 달라졌다");
    println!("표를 매번 훑기: 한 장 {before:.3} ms (한 번 {:.2} us)", before * 1000.0 / (shells.len() * per_frame) as f64);
    println!("표 한 장에 한 번: 한 장 {after:.3} ms (한 번 {:.2} us)", after * 1000.0 / (shells.len() * per_frame) as f64);
    println!("144 Hz 로 그리면 메인 스레드: {:.1}% → {:.2}%", before * 144.0 / 10.0, after * 144.0 / 10.0);
}
