//! 칸의 하네스(claude·codex)를 곱게 끈다. 이사가 대화 파일을 옮기기 직전에 부른다 — 보내는 쪽
//! 워커와 원격 `/term/agent-stop` 이 같은 규칙을 쓴다.
//!
//! 덜 꺼진 하네스를 두고 「껐다」고 답하면 도착 기기의 `--resume` 과 같은 대화가 두 기기에서 동시에
//! 돈다. 그래서 끄지 못했으면 늘 실패로 답하고, 강제 종료는 부른 쪽이 강행(`force`)을 골랐을 때만 한다.

use std::time::Duration;

/// 끄기 신호 뒤 하네스가 마지막 대화 조각을 쓰고 나갈 때까지 기다리는 한도.
pub const STOP_DEADLINE: Duration = Duration::from_secs(8);

/// `pid` 로 도는 `kind` 하네스를 끄고 끝날 때까지 기다린다.
///
/// `prompt_idle` — 입력창이 비어 있고 차례가 사람(승인·질문)에게 있지 않은가. 화면 판독은 GUI 가
/// 아는 일이라 부른 쪽이 넘긴다. 유닉스는 신호로 끄므로 묻지 않는다.
pub fn stop_agent(
    session: &kasa_pty::PtySession,
    kind: kasa_pty::AgentKind,
    pid: u32,
    force: bool,
    prompt_idle: impl Fn() -> bool,
) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        let _ = (session, force, &prompt_idle);
        // SAFETY: 대상 pid 하나에만 보낸다. SIGKILL 은 jsonl 마지막 조각을 잃을 수 있어 쓰지 않는다.
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        anyhow::ensure!(kasa_pty::wait_exit(pid, STOP_DEADLINE), "{}", not_stopped(kind, pid));
        Ok(())
    }
    #[cfg(windows)]
    {
        // Ctrl+C 는 `claude.cmd` 를 감싼 cmd 가 「일괄 작업을 끝내시겠습니까 (Y/N)?」로 받아 거기서 멈추고,
        // 콘솔 프로세스엔 SIGTERM 같은 곱게 끄기 신호가 없다 — 하네스 자신의 끄기 명령을 친다.
        if !prompt_idle() {
            anyhow::ensure!(
                force,
                "{} 입력창에 쓰던 글이 있거나 승인·질문을 기다리고 있어 끄지 않았어요. 입력창을 비운 뒤 다시 하거나, 알고 강행하려면 --force",
                kind.label()
            );
            return kill_now(kind, pid);
        }
        session.send_control_bytes(quit_command(kind))?;
        // 명령과 Enter 를 한 번에 보내면 하네스가 붙여넣기로 받아 Enter 가 입력창의 줄바꿈이 된다.
        std::thread::sleep(Duration::from_millis(300));
        session.send_control_bytes(b"\r")?;
        if kasa_pty::wait_exit(pid, STOP_DEADLINE) {
            return Ok(());
        }
        anyhow::ensure!(force, "{}", not_stopped(kind, pid));
        kill_now(kind, pid)
    }
}

fn not_stopped(kind: kasa_pty::AgentKind, pid: u32) -> String {
    format!(
        "{}(pid {pid}) 가 {}초 안에 안 꺼졌어요 — 같은 대화가 두 기기에서 돌지 않게 멈췄어요",
        kind.label(),
        STOP_DEADLINE.as_secs()
    )
}

/// 하네스가 스스로 나가는 명령. codex 의 `/exit` 은 뒤에 생긴 별칭이라 처음부터 있던 `/quit` 을 쓴다.
#[cfg_attr(not(windows), allow(dead_code))]
fn quit_command(kind: kasa_pty::AgentKind) -> &'static [u8] {
    match kind {
        kasa_pty::AgentKind::Codex => b"/quit",
        _ => b"/exit",
    }
}

#[cfg(windows)]
fn kill_now(kind: kasa_pty::AgentKind, pid: u32) -> anyhow::Result<()> {
    kasa_pty::terminate_pid(pid)
        .map_err(|e| anyhow::anyhow!("{}(pid {pid}) 강제 종료 실패: {e}", kind.label()))?;
    anyhow::ensure!(
        kasa_pty::wait_exit(pid, Duration::from_secs(2)),
        "{}(pid {pid}) 가 강제 종료 뒤에도 남아 있어요",
        kind.label()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_harness_quits_with_its_own_command() {
        assert_eq!(quit_command(kasa_pty::AgentKind::Claude), b"/exit");
        assert_eq!(quit_command(kasa_pty::AgentKind::Codex), b"/quit");
    }

    #[test]
    fn an_exited_process_reads_as_stopped() {
        let mut child = std::process::Command::new(if cfg!(windows) { "cmd" } else { "sh" })
            .args(if cfg!(windows) { ["/c", "exit"] } else { ["-c", "exit"] })
            .spawn()
            .unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert!(kasa_pty::wait_exit(pid, Duration::from_millis(500)));
        assert!(!kasa_pty::pid_alive(pid));
        assert!(!kasa_pty::pid_alive(0), "0 은 프로세스가 아니다");
    }

    #[test]
    fn a_running_process_is_alive_until_terminated() {
        let mut child = std::process::Command::new(if cfg!(windows) { "ping" } else { "sleep" })
            .args(if cfg!(windows) { vec!["-n", "30", "127.0.0.1"] } else { vec!["30"] })
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        assert!(kasa_pty::pid_alive(pid));
        assert!(!kasa_pty::wait_exit(pid, Duration::from_millis(200)), "도는 동안은 기다림이 시간 초과로 끝난다");
        kasa_pty::terminate_pid(pid).unwrap();
        let _ = child.wait();
        assert!(kasa_pty::wait_exit(pid, Duration::from_secs(2)));
    }
}
