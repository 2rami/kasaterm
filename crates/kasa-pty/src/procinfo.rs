//! 남의 프로세스 argv·환경 — macOS 는 커널(`KERN_PROCARGS2`)에 바로 묻는다.
//!
//! `ps` 를 띄우던 자리다. board(`/term/panes`)는 pane 마다 환경을 한 번, 명령줄 표를 한 번 읽는데,
//! 그게 폴 하나에 `ps` 를 pane 수만큼 낳아 응답 178ms 의 절반 가까이를 먹었다(2026-10-02 `sample`
//! 실측 — 폰 허브가 목록을 받을 때마다 이 값을 기다린다). `ps` 도 같은 sysctl 을 부르므로 답이 같다.
//! 커널이 환경을 빼고 주는 것도 같다 — 플랫폼 바이너리(`/bin/zsh`·`/bin/sleep`)의 환경은 `ps` 에도
//! 안 보인다. 커널이 거절하면(이미 죽었거나 남의 프로세스) 예전 `ps` 길로 간다.

#[cfg(target_os = "macos")]
fn procargs(pid: u32) -> Option<(Vec<String>, Vec<String>)> {
    with_procargs(pid, parse_procargs)
}

/// `KERN_PROCARGS2` 를 스레드마다 하나인 버퍼에 받아 `read` 에 넘긴다. 커널이 거절하면 `None`.
#[cfg(target_os = "macos")]
fn with_procargs<R>(pid: u32, read: impl FnOnce(&[u8]) -> Option<R>) -> Option<R> {
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
        read(&buf[..size.min(buf.len())])
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

/// argv[0] 만 — 프로세스 표가 이름을 뽑는 자리라 환경까지 펼칠 필요가 없다.
#[cfg(any(target_os = "macos", test))]
fn parse_argv0(raw: &[u8]) -> Option<String> {
    let argc = i32::from_ne_bytes(raw.get(..4)?.try_into().ok()?);
    if argc < 1 {
        return None;
    }
    let rest = raw.get(4..)?;
    let rest = &rest[rest.iter().position(|&b| b == 0)?..];
    let rest = &rest[rest.iter().position(|&b| b != 0)?..];
    let first = rest.split(|&b| b == 0).next()?;
    Some(String::from_utf8_lossy(first).into_owned())
}

/// `ps -o comm=` 한 칸을 표 이름으로 — 공백을 하나로 접고 마지막 경로 조각만 남긴다.
#[cfg(unix)]
pub(crate) fn comm_name(comm: &str) -> String {
    let comm = comm.split_whitespace().collect::<Vec<_>>().join(" ");
    std::path::Path::new(&comm)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&comm)
        .to_string()
}

/// 프로세스 표(pid, ppid, 이름)를 커널에 바로 묻는다 — `ps -A -o pid=,ppid=,comm=` 를 띄우던 자리.
///
/// `ps` 한 번이 프로세스 1,600개 남짓인 기계에서 CPU 68ms 였고, 렌더가 300ms 캐시로 부르는 데다
/// 프로세스 표가 필요한 자리마다 따로 불러 초당 여러 번 떴다(2026-10-06 실측, 학생 12명 작업 중
/// 초당 8번 이상). 같은 일을 프로세스 안에서 하면 10ms 남짓이다 — 비싼 것은 `ps` 의 fork·exec 가
/// 아니라 프로세스마다 argv 를 복사해 오는 커널 일이라, 그것은 **내 프로세스에만** 한다.
///
/// 이름 규칙은 `ps` 의 `comm` 과 같다: 내 프로세스는 argv[0] 의 마지막 조각(`node` 가 제목을
/// `npm run dev` 로 바꾸면 그것)이고 실측으로 내 프로세스 824개가 전부 일치했다. 남의
/// 프로세스는 커널이 argv 를 안 주므로 실행 파일 경로, 그것도 없으면 커널이 아는 짧은 이름이다.
/// 커널이 아예 답을 안 하는 남의 시스템 프로세스는 표에서 빠진다(학생·셸·도구는 전부 내 것이다).
#[cfg(target_os = "macos")]
pub(crate) fn process_table() -> Option<Vec<(u32, u32, String)>> {
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if count <= 0 {
        return None;
    }
    // 두 번 묻는 사이에 생긴 프로세스 몫으로 여유를 둔다.
    let mut pids = vec![0 as libc::c_int; count as usize + 64];
    let bytes = (pids.len() * std::mem::size_of::<libc::c_int>()) as libc::c_int;
    let count = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
    if count <= 0 {
        return None;
    }
    pids.truncate(count as usize);
    let me = unsafe { libc::getuid() };
    let mut path = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let mut out = Vec::with_capacity(pids.len());
    for pid in pids {
        if pid <= 0 {
            continue;
        }
        let mut info: libc::proc_bsdshortinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdshortinfo>() as libc::c_int;
        let got = unsafe {
            libc::proc_pidinfo(pid, libc::PROC_PIDT_SHORTBSDINFO, 0, (&mut info as *mut libc::proc_bsdshortinfo).cast(), size)
        };
        if got != size {
            continue;
        }
        let own = (info.pbsi_uid == me).then(|| with_procargs(pid as u32, parse_argv0)).flatten();
        let name = own.or_else(|| {
            let n = unsafe { libc::proc_pidpath(pid, path.as_mut_ptr().cast(), path.len() as u32) };
            (n > 0).then(|| String::from_utf8_lossy(&path[..n as usize]).into_owned())
        });
        let name = match name {
            Some(name) => comm_name(&name),
            None => {
                let raw: Vec<u8> = info.pbsi_comm.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
                String::from_utf8_lossy(&raw).into_owned()
            }
        };
        out.push((pid as u32, info.pbsi_ppid, name));
    }
    Some(out)
}

/// pid 의 명령줄(argv 를 공백으로 이은 것, `ps -o args=` 와 같은 모양).
pub fn process_cmdline(pid: u32) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        if let Some((argv, _)) = procargs(pid) {
            let line = argv.join(" ");
            return (!line.trim().is_empty()).then_some(line);
        }
        // 내 프로세스를 커널이 거절했다면 이미 죽은 것이다 — `ps` 표를 떠 봐야 같은 답이다.
        if !foreign(pid) {
            return None;
        }
    }
    crate::state::process_cmdline(pid)
}

/// 살아 있는 남의 프로세스인가(커널이 argv 를 안 주는 쪽).
#[cfg(target_os = "macos")]
fn foreign(pid: u32) -> bool {
    let mut info: libc::proc_bsdshortinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdshortinfo>() as libc::c_int;
    let got = unsafe {
        libc::proc_pidinfo(pid as libc::c_int, libc::PROC_PIDT_SHORTBSDINFO, 0, (&mut info as *mut libc::proc_bsdshortinfo).cast(), size)
    };
    got == size && info.pbsi_uid != unsafe { libc::getuid() }
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

    #[test]
    fn argv0_is_the_first_argument_not_the_exec_path() {
        let r = raw(2, b"/opt/homebrew/bin/node\0\0\0npm run dev\0--x\0A=1\0\0");
        assert_eq!(parse_argv0(&r).as_deref(), Some("npm run dev"));
        assert!(parse_argv0(&raw(0, b"/bin/x\0\0")).is_none());
        assert_eq!(comm_name("/Users/k/.local/bin/claude"), "claude");
        assert_eq!(comm_name("postgres: checkpointer "), "postgres: checkpointer");
    }

    /// 커널 표가 `ps -A -o pid=,ppid=,comm=` 와 같은 답을 내는가 — 내 프로세스 전부를 견준다.
    /// 두 표를 뜨는 사이에 생기고 죽는 것이 있어 둘 다에 있는 pid 만 본다.
    #[cfg(target_os = "macos")]
    #[test]
    fn table_matches_ps_for_own_processes() {
        let mut child = std::process::Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let native = process_table().expect("kernel table");
        let ps = std::process::Command::new("ps").args(["-A", "-o", "pid=,ppid=,comm="]).output().unwrap();
        let _ = child.kill();
        let _ = child.wait();
        let me = std::process::id();
        let sleep = native.iter().find(|(pid, _, _)| *pid == child.id()).expect("child in table");
        assert_eq!((sleep.1, sleep.2.as_str()), (me, "sleep"));
        let native: std::collections::HashMap<u32, (u32, String)> =
            native.into_iter().map(|(pid, ppid, name)| (pid, (ppid, name))).collect();
        let (mut same, mut differ) = (0, Vec::new());
        for line in String::from_utf8_lossy(&ps.stdout).lines() {
            let mut parts = line.split_whitespace();
            let (Some(pid), Some(ppid)) = (parts.next().and_then(|x| x.parse::<u32>().ok()), parts.next().and_then(|x| x.parse::<u32>().ok())) else {
                continue;
            };
            let Some((n_ppid, n_name)) = native.get(&pid) else { continue };
            if procargs(pid).is_none() {
                continue;
            }
            let want = comm_name(&parts.collect::<Vec<_>>().join(" "));
            if (*n_ppid, n_name.as_str()) == (ppid, want.as_str()) {
                same += 1;
            } else {
                differ.push((pid, want, n_name.clone()));
            }
        }
        assert!(same > 10, "too few own processes compared: {same}");
        assert!(differ.len() * 100 <= same, "names differ from ps: {differ:?}");
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
