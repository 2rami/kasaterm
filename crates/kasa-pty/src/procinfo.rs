//! 남의 프로세스 argv·환경 — macOS 는 커널(`KERN_PROCARGS2`)에 바로 묻는다.
//!
//! `ps` 를 띄우던 자리다. board(`/term/panes`)는 pane 마다 환경을 한 번, 명령줄 표를 한 번 읽는데,
//! 그게 폴 하나에 `ps` 를 pane 수만큼 낳아 응답 178ms 의 절반 가까이를 먹었다(2026-10-02 `sample`
//! 실측 — 폰 허브가 목록을 받을 때마다 이 값을 기다린다). `ps` 도 같은 sysctl 을 부르므로 답이 같다.
//! 커널이 환경을 빼고 주는 것도 같다 — 플랫폼 바이너리(`/bin/zsh`·`/bin/sleep`)의 환경은 `ps` 에도
//! 안 보인다. 커널이 거절하면(이미 죽었거나 남의 프로세스) 예전 `ps` 길로 간다.

#[cfg(target_os = "macos")]
fn procargs(pid: u32) -> Option<(Vec<String>, Vec<String>)> {
    use std::cell::RefCell;
    use std::sync::OnceLock;

    static ARGMAX: OnceLock<usize> = OnceLock::new();
    let argmax = *ARGMAX.get_or_init(|| {
        let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
        let mut value: libc::c_int = 0;
        let mut size = std::mem::size_of::<libc::c_int>();
        let ok = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                2,
                (&mut value as *mut libc::c_int).cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        } == 0;
        if ok && value > 0 { value as usize } else { 1 << 20 }
    });
    thread_local! {
        static BUF: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    }
    BUF.with(|buf| {
        let mut buf = buf.borrow_mut();
        buf.resize(argmax, 0);
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
        let mut size = buf.len();
        let ok = unsafe {
            libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr().cast(), &mut size, std::ptr::null_mut(), 0)
        } == 0;
        if !ok {
            return None;
        }
        parse_procargs(&buf[..size.min(buf.len())])
    })
}

/// `KERN_PROCARGS2` 의 모양: `argc(i32)` · 실행 경로 · NUL 채움 · argv 들 · 환경 들(빈 문자열에서 끝).
#[cfg(any(target_os = "macos", test))]
fn parse_procargs(raw: &[u8]) -> Option<(Vec<String>, Vec<String>)> {
    let argc = i32::from_ne_bytes(raw.get(..4)?.try_into().ok()?);
    let rest = raw.get(4..)?;
    let rest = &rest[rest.iter().position(|&b| b == 0)?..];
    let rest = &rest[rest.iter().position(|&b| b != 0)?..];
    let mut parts = rest.split(|&b| b == 0);
    let argv = (0..argc.max(0))
        .map(|_| parts.next().map(|s| String::from_utf8_lossy(s).into_owned()))
        .collect::<Option<Vec<_>>>()?;
    let env = parts
        .take_while(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();
    Some((argv, env))
}

/// pid 의 명령줄(argv 를 공백으로 이은 것, `ps -o args=` 와 같은 모양).
pub fn process_cmdline(pid: u32) -> Option<String> {
    #[cfg(target_os = "macos")]
    if let Some((argv, _)) = procargs(pid) {
        let line = argv.join(" ");
        return (!line.trim().is_empty()).then_some(line);
    }
    crate::state::process_cmdline(pid)
}

/// 여러 환경 변수를 한 번에. 빈 값은 없는 것으로 친다.
pub fn process_env_vars(pid: u32, keys: &[&str]) -> std::collections::HashMap<String, String> {
    #[cfg(target_os = "macos")]
    if let Some((_, env)) = procargs(pid) {
        return env
            .iter()
            .filter_map(|kv| kv.split_once('='))
            .filter(|(k, v)| !v.is_empty() && keys.contains(k))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
    }
    crate::state::process_env_vars(pid, keys)
}

pub fn process_env_var(pid: u32, key: &str) -> Option<String> {
    process_env_vars(pid, &[key]).remove(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(argc: i32, body: &[u8]) -> Vec<u8> {
        let mut v = argc.to_ne_bytes().to_vec();
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn parse_skips_exec_path_padding_and_stops_at_env_end() {
        let r = raw(2, b"/bin/sleep\0\0\0\0sleep\0300\0A=1\0KASA_X=\xea\xb0\x80\0\0junk\0");
        let (argv, env) = parse_procargs(&r).unwrap();
        assert_eq!(argv, ["sleep", "300"]);
        assert_eq!(env, ["A=1", "KASA_X=가"]);
    }

    #[test]
    fn parse_refuses_truncated_argv() {
        assert!(parse_procargs(&raw(3, b"/bin/x\0\0x\0")).is_none());
        assert!(parse_procargs(&[1, 0]).is_none());
    }

    /// 커널 길이 `ps` 와 같은 답을 내는가. 대조 상대는 이 시험 프로세스 자신이다 — `/bin/sleep` 같은
    /// 플랫폼 바이너리는 환경이 양쪽 다 안 보여 비교가 안 된다.
    #[cfg(target_os = "macos")]
    #[test]
    fn matches_ps_for_this_process() {
        let pid = std::process::id();
        let keys = ["CARGO_PKG_NAME", "HOME", "PATH", "KASA_PROCINFO_ABSENT"];
        let native = process_env_vars(pid, &keys);
        assert_eq!(native, crate::state::process_env_vars(pid, &keys));
        assert_eq!(native.get("CARGO_PKG_NAME").map(String::as_str), Some("kasa-pty"));
        assert!(!native.contains_key("KASA_PROCINFO_ABSENT"));
        let ps_args = std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "args="])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
        assert_eq!(process_cmdline(pid), ps_args);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn dead_pid_reads_nothing() {
        let mut child = std::process::Command::new("/usr/bin/true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert!(procargs(pid).is_none());
        assert!(process_env_vars(pid, &["PATH"]).is_empty());
    }
}
