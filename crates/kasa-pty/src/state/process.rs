//! 셸·전경 프로세스 정보 — 프로세스 표(공유 캐시)·명령줄·환경, 칸의 셸 pid·tty·제목·cwd·도는 일.

use super::*;
use super::agents::{descend_launchers, strip_exe_suffix};

impl PtySession {
    /// The shell's process id (None if it failed to launch). Used to look up
    /// the active pane's cwd for the git panel.
    pub fn shell_pid(&self) -> Option<u32> {
        self.shell_pid
    }

    /// The shell's controlling tty short name (e.g. "ttys004"), or None on
    /// Windows. Shown in the pane header — mirrors ghostty / Terminal.app.
    pub fn tty(&self) -> Option<&str> {
        self.tty_short.as_deref()
    }

    /// The pane's current OSC 0/2 title (set by the inner program), or None.
    /// Mirrors the header label's first-priority source so the dock chip can
    /// show the same name the header does.
    pub fn osc_title(&self) -> Option<String> {
        self.title_handle.lock().ok().and_then(|t| t.clone())
    }

    /// The shell's last OSC 9;9-reported cwd, if shell integration is emitting
    /// it (injected PowerShell prompt). None for shells that don't — callers
    /// then fall back to reading the process cwd directly.
    pub fn reported_cwd(&self) -> Option<std::path::PathBuf> {
        self.cwd_handle.lock().ok().and_then(|c| c.clone())
    }

    /// The command running under the shell right now, or None when it sits at
    /// its prompt. Unlike `active_process_name` it never falls back to the
    /// shell itself — a window-less web shell is only safe to close when idle.
    pub fn running_job(&self) -> Option<String> {
        let table = process_table_shared();
        let pid = effective_shell_pid(&table, self.shell_pid?);
        let child = table
            .iter()
            .filter(|(_, ppid, _)| *ppid == pid)
            .max_by_key(|(p, _, _)| *p)
            .map(|(p, _, n)| (*p, n.clone()));
        descend_launchers(&table, child).map(|(_, n)| strip_exe_suffix(n))
    }

    /// Best-effort label for what's running in this PTY *right now*.
    /// Returns the comm name of the most recently spawned child of
    /// our shell (typically the foreground command — vim, claude,
    /// less, …) or falls back to the shell's own comm. ps(1) is
    /// throttled to ~500ms so this is cheap to call from the render
    /// loop.
    pub fn active_process_name(&self) -> Option<String> {
        let pid = self.shell_pid?;
        let now = Instant::now();
        let mut cache = self.proc_cache.lock().ok()?;
        if now.duration_since(cache.0).as_millis() < 500 {
            return cache.1.clone();
        }
        cache.0 = now;
        // process_table() already returns bare exe names (no path), so the
        // shell row and the newest direct child are matched on pid/ppid alone.
        let table = process_table_shared();
        let pid = effective_shell_pid(&table, pid);
        let mut best_child: Option<(u32, String)> = None;
        let mut shell_comm: Option<String> = None;
        for (row_pid, row_ppid, name) in table.iter() {
            if *row_pid == pid {
                shell_comm = Some(name.clone());
            } else if *row_ppid == pid && best_child.as_ref().is_none_or(|(p, _)| *p < *row_pid) {
                best_child = Some((*row_pid, name.clone()));
            }
        }
        let best_child = descend_launchers(&table, best_child);
        let resolved = best_child.map(|(_, n)| n).or(shell_comm).map(strip_exe_suffix);
        // Git bash 는 스크립트 실행 시 중간 프로세스가 죽어 부모 사슬이 영구
        // 단절된다(bash → [dead] → sh.exe → claude.exe, VM 실측) — ppid 하강
        // 으로는 못 잇는다. 사슬이 셸에서 끊겼으면 이 GUI 가 띄운 claude
        // (argv 의 --settings 경로에 kasaterm-shim-<GUI pid> 가 박힘)를 전역
        // 스캔하는 최후 폴백. pane 여러 개 중 일부만 claude 인 경우 셸-only
        // pane 도 claude 로 오판하는 알려진 한계 — 입력박스 색은 화면 패턴
        // (prompt_box_rows)이 걸러주고 pane 테두리만 드물게 오색.
        #[cfg(windows)]
        let resolved = {
            let shellish = resolved.as_deref().is_none_or(is_shell_exe);
            if shellish && orphan_claude_of_this_gui(&table) {
                Some("claude".to_string())
            } else {
                resolved
            }
        };
        cache.1 = resolved.clone();
        resolved
    }

    /// True when the shell has a child process (a command/claude/build/editor is
    /// running) — the pane has "작업현황". False for a bare idle prompt. Lets a
    /// close decide between folding into the dock (busy → keep) and just closing
    /// (idle → no chip). One `ps` scan; called only on dock/close, not per frame.
    pub fn has_active_job(&self) -> bool {
        let Some(pid) = self.shell_pid else {
            return false;
        };
        let table = process_table_shared();
        let pid = effective_shell_pid(&table, pid);
        if table.iter().any(|(_, ppid, _)| *ppid == pid) {
            return true;
        }
        // 고아 사슬로 claude 가 트리에서 끊긴 pane 은 자식-없음=idle 로 오판돼
        // confirm 없이 닫힌다 — active_process_name 과 같은 폴백으로 방어.
        #[cfg(windows)]
        if orphan_claude_of_this_gui(&table) {
            return true;
        }
        false
    }
}

/// Windows Git bash 는 런처(bin\bash.exe)가 실셸(usr\bin\bash.exe)을 자식으로
/// 한 번 더 스폰하고, sh wrapper 스크립트도 셸을 한 단 더 끼운다 — 직계 자식만
/// 보면 항상 "bash.exe"라 claude 탐지(색·프사 게이트)와 busy 판정이 전부 죽는다.
/// 유일한 자식이 셸일 때만 그쪽을 셸로 보고 내려간다(명령 실행 중이면 자식이
/// 비셸이라 그 자리에서 멈춰 기존 의미 유지). Unix 는 no-op.
pub(super) fn effective_shell_pid(table: &[(u32, u32, String)], pid: u32) -> u32 {
    #[cfg(not(windows))]
    {
        let _ = table;
        pid
    }
    #[cfg(windows)]
    {
        let mut pid = pid;
        for _ in 0..3 {
            let mut kids = table.iter().filter(|(_, pp, _)| *pp == pid);
            let (Some(only), None) = (kids.next(), kids.next()) else {
                break;
            };
            if !is_shell_exe(&only.2) {
                break;
            }
            pid = only.0;
        }
        pid
    }
}

#[cfg(windows)]
fn is_shell_exe(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let base = lower.strip_suffix(".exe").unwrap_or(&lower);
    matches!(
        base,
        "bash" | "sh" | "zsh" | "dash" | "fish" | "tcsh" | "ksh" | "cmd" | "pwsh" | "powershell"
    )
}

/// 이 GUI 프로세스가 스폰한 claude 가 어딘가 살아있는가 — shim wrapper 가 붙인
/// `--settings %TEMP%\kasaterm-shim-<GUI pid>\...` argv 마커로 판정한다. 남의
/// 터미널(VS Code 등)에서 도는 claude 는 마커가 없어 배제된다. 호출측 500ms
/// 캐시 안에서만 돌고, cmdline 조회는 이름이 claude 인 프로세스로 한정.
#[cfg(windows)]
fn orphan_claude_of_this_gui(table: &[(u32, u32, String)]) -> bool {
    let marker = format!("kasaterm-shim-{}", std::process::id());
    table
        .iter()
        .filter(|(_, _, name)| name.to_ascii_lowercase().contains("claude"))
        .any(|(pid, _, _)| process_cmdline(*pid).is_some_and(|cl| cl.contains(&marker)))
}

/// `(pid, ppid, exe_name)` for every running process — the cross-platform
/// stand-in for `ps -A -o pid=,ppid=,comm=`. `exe_name` is the bare file name
/// (no directory; e.g. "claude.exe", "pwsh"). Windows walks a Toolhelp snapshot
/// because it has no `ps`; Unix shells out to `ps`. Callers match on pid/ppid
/// and substring the name (e.g. `.contains("claude")`).
#[cfg(windows)]
pub(super) fn process_table_raw() -> Vec<(u32, u32, String)> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    let mut out = Vec::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return out;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snap, &mut entry) != 0 {
            loop {
                let end = entry
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(entry.szExeFile.len());
                let name = String::from_utf16_lossy(&entry.szExeFile[..end]);
                out.push((entry.th32ProcessID, entry.th32ParentProcessID, name));
                if Process32NextW(snap, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
    }
    out
}

#[cfg(unix)]
pub(super) fn process_table_raw() -> Vec<(u32, u32, String)> {
    #[cfg(target_os = "macos")]
    if let Some(table) = crate::procinfo::process_table() {
        return table;
    }
    let Ok(output) = std::process::Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,comm="])
        .output()
    else {
        return Vec::new();
    };
    let s = String::from_utf8_lossy(&output.stdout);
    let mut out = Vec::new();
    for line in s.lines() {
        let mut parts = line.split_whitespace();
        let (Some(pid), Some(ppid)) = (
            parts.next().and_then(|x| x.parse::<u32>().ok()),
            parts.next().and_then(|x| x.parse::<u32>().ok()),
        ) else {
            continue;
        };
        let comm = parts.collect::<Vec<_>>().join(" ");
        out.push((pid, ppid, crate::procinfo::comm_name(&comm)));
    }
    out
}

/// `process_table_raw` 를 짧은 TTL 로 감싼 전역 캐시. 세션이 많을 때 렌더 스레드가
/// pane 마다 active_process_name/is_claude_agents 로 이걸 부르면, per-pane 500ms
/// 캐시가 같은 프레임에 동시 만료될 때 K 번 ps fork 가 겹쳐 프레임드랍(사용자: 세션
/// 많을 때). 300ms 전역 캐시로 한 프레임의 중복 fork 를 1 회로 접는다(per-pane 캐시
/// 보다 촘촘해 신선도는 유지). fork 대신 Vec clone 이라 비용이 pane 수에 선형이지만
/// ps fork+파싱보다 훨씬 싸다. 빈 결과(ps 실패)는 캐싱하지 않아 다음 호출이 재시도한다.
pub fn process_table() -> Vec<(u32, u32, String)> {
    (*process_table_shared()).clone()
}

/// 파괴적 동작의 직전 검증용. 렌더·주기 폴링은 캐시된 process_table을 쓴다.
pub fn fresh_process_table() -> Vec<(u32, u32, String)> {
    process_table_raw()
}

pub type ProcessTable = std::sync::Arc<Vec<(u32, u32, String)>>;

/// 같은 캐시를 **복사 없이** 빌려준다. 렌더처럼 pane 마다 매 프레임 부르는 쪽은
/// 이걸 써야 한다 — `process_table()` 은 히트할 때도 테이블을 통째로 clone 해서
/// 프로세스 수백 개면 프레임마다 그만큼의 String 할당이 돈다.
struct CachedTable {
    at: Instant,
    table: ProcessTable,
    refreshing: bool,
}

fn table_cache() -> &'static std::sync::Mutex<CachedTable> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<CachedTable>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| {
        std::sync::Mutex::new(CachedTable {
            at: Instant::now() - std::time::Duration::from_secs(1),
            table: Default::default(),
            refreshing: false,
        })
    })
}

/// Enter 직후 프로세스 테이블을 앞당겨 읽는다 — 300ms TTL + 백그라운드 갱신
/// 구조에서는 방금 exec 된 claude 가 테이블에 실리기까지 최악 ~600ms 가 비고,
/// 그동안 배너·헤더가 학생 테마 없이 그려졌다(사용자 2026-08-20 「처음 클로드코드
/// 켜면 캐릭터 학생테마 적용안돼」). Enter 는 「새 전경 프로세스가 곧 뜬다」의
/// 가장 이른 신호지만 exec 사슬이 끝나기 전에 읽으면 헛스캔이 `at` 시계만
/// 되돌려 오히려 다음 정기 갱신을 늦춘다 — 그래서 사슬이 끝났을 100ms 뒤와,
/// 느린 런처(래퍼 스크립트→node) 대비 마지막 입력 400ms 뒤에도 읽는다.
#[derive(Default)]
struct ProcessTablePokes {
    next: Option<Instant>,
    last: Option<Instant>,
}

impl ProcessTablePokes {
    fn submit(&mut self, now: Instant) -> bool {
        self.last = Some(now);
        let early = now + std::time::Duration::from_millis(100);
        if let Some(next) = self.next {
            self.next = Some(next.min(early));
            return false;
        }
        self.next = Some(early);
        true
    }

    fn scanned(&mut self, now: Instant) {
        let trailing = self.last.unwrap() + std::time::Duration::from_millis(400);
        self.next = (now < trailing)
            .then(|| trailing.min(now + std::time::Duration::from_millis(400)));
    }
}

pub(super) fn process_table_poke() {
    static POKES: Mutex<ProcessTablePokes> = Mutex::new(ProcessTablePokes {
        next: None,
        last: None,
    });
    static WAKE: std::sync::Condvar = std::sync::Condvar::new();
    let spawn = POKES.lock().unwrap_or_else(|e| e.into_inner()).submit(Instant::now());
    WAKE.notify_one();
    if !spawn {
        return;
    }
    // Concurrent submissions share one sleeper and process scan. Keeping the
    // last submission preserves the slow-launcher trailing scan for every burst.
    let spawned = std::thread::Builder::new().name("process-table-poke".into()).spawn(|| {
        loop {
            let finished = {
                let mut pokes = POKES.lock().unwrap_or_else(|e| e.into_inner());
                loop {
                    let Some(next) = pokes.next else { return };
                    let delay = next.saturating_duration_since(Instant::now());
                    if delay.is_zero() {
                        break;
                    }
                    pokes = WAKE.wait_timeout(pokes, delay)
                        .unwrap_or_else(|e| e.into_inner()).0;
                }
                pokes.scanned(Instant::now());
                pokes.next.is_none()
            };
            let refresh = {
                let mut g = table_cache().lock().unwrap_or_else(|e| e.into_inner());
                let refresh = !g.refreshing;
                g.refreshing = true;
                refresh
            };
            if refresh {
                let fresh = process_table_raw();
                if let Ok(mut g) = table_cache().lock() {
                    if !fresh.is_empty() {
                        g.at = Instant::now();
                        g.table = std::sync::Arc::new(fresh);
                    }
                    g.refreshing = false;
                }
            }
            if finished {
                return;
            }
        }
    });
    if spawned.is_err() {
        POKES.lock().unwrap_or_else(|e| e.into_inner()).next = None;
    }
}

pub fn process_table_shared() -> ProcessTable {
    let cache = table_cache();
    let Ok(mut g) = cache.lock() else {
        return std::sync::Arc::new(process_table_raw());
    };
    if !g.table.is_empty() && g.at.elapsed().as_millis() < 300 {
        return g.table.clone();
    }
    // 첫 호출은 답이 없으니 그 자리에서 채운다. 그 뒤로는 **절대 프레임 안에서
    // 새로 뜨지 않는다** — 갱신은 백그라운드로 돌리고 직전 표를 그대로 준다.
    // ps fork 는 수 ms 짜리라, 300ms 마다 렌더 프레임 하나가 그걸 뒤집어쓰면
    // 초당 세 번 눈에 띄는 딸꾹질이 된다.
    if g.table.is_empty() {
        let fresh = process_table_raw();
        if fresh.is_empty() {
            // ps 실패는 캐싱하지 않는다 — 다음 호출이 재시도한다.
            return Default::default();
        }
        g.at = Instant::now();
        g.table = std::sync::Arc::new(fresh);
        return g.table.clone();
    }
    if !g.refreshing {
        g.refreshing = true;
        std::thread::spawn(move || {
            let fresh = process_table_raw();
            if let Ok(mut g) = cache.lock() {
                if !fresh.is_empty() {
                    g.at = Instant::now();
                    g.table = std::sync::Arc::new(fresh);
                }
                g.refreshing = false;
            }
        });
    }
    g.table.clone()
}

/// The full command line (argv, space-joined) of a single process, or None if
/// it can't be read. The cross-platform stand-in for `ps -p PID -o args=`.
/// Windows has no `ps`, so it walks the target's PEB →
/// RTL_USER_PROCESS_PARAMETERS.CommandLine over ReadProcessMemory (needs
/// PROCESS_VM_READ, i.e. same-user / same-integrity processes — enough for the
/// claude panes we spawn). Unix shells out to `ps`.
#[cfg(windows)]
pub fn process_cmdline(pid: u32) -> Option<String> {
    use std::ffi::c_void;
    use windows_sys::Wdk::System::Threading::{
        NtQueryInformationProcess, ProcessBasicInformation,
    };
    use windows_sys::Win32::Foundation::{CloseHandle, UNICODE_STRING};
    use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PEB, PROCESS_BASIC_INFORMATION, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ,
        RTL_USER_PROCESS_PARAMETERS,
    };
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, 0, pid);
        if handle.is_null() {
            return None;
        }
        // Wrap the chained reads so CloseHandle always runs on any early exit.
        let read = || -> Option<String> {
            let mut pbi: PROCESS_BASIC_INFORMATION = std::mem::zeroed();
            let mut ret_len = 0u32;
            if NtQueryInformationProcess(
                handle,
                ProcessBasicInformation,
                &mut pbi as *mut _ as *mut c_void,
                std::mem::size_of::<PROCESS_BASIC_INFORMATION>() as u32,
                &mut ret_len,
            ) != 0
            {
                return None;
            }
            if pbi.PebBaseAddress.is_null() {
                return None;
            }
            let mut peb: PEB = std::mem::zeroed();
            if ReadProcessMemory(
                handle,
                pbi.PebBaseAddress as *const c_void,
                &mut peb as *mut _ as *mut c_void,
                std::mem::size_of::<PEB>(),
                std::ptr::null_mut(),
            ) == 0
            {
                return None;
            }
            let mut params: RTL_USER_PROCESS_PARAMETERS = std::mem::zeroed();
            if ReadProcessMemory(
                handle,
                peb.ProcessParameters as *const c_void,
                &mut params as *mut _ as *mut c_void,
                std::mem::size_of::<RTL_USER_PROCESS_PARAMETERS>(),
                std::ptr::null_mut(),
            ) == 0
            {
                return None;
            }
            let cmd: UNICODE_STRING = params.CommandLine;
            if cmd.Buffer.is_null() || cmd.Length == 0 {
                return None;
            }
            let mut buf = vec![0u16; (cmd.Length / 2) as usize];
            if ReadProcessMemory(
                handle,
                cmd.Buffer as *const c_void,
                buf.as_mut_ptr() as *mut c_void,
                cmd.Length as usize,
                std::ptr::null_mut(),
            ) == 0
            {
                return None;
            }
            Some(String::from_utf16_lossy(&buf))
        };
        let result = read();
        CloseHandle(handle);
        result
    }
}

/// 표를 한 번에 뜬다 — pid 하나를 물어도 `ps` 는 어차피 전 프로세스를 훑으므로,
/// 하나씩 묻는 것은 같은 일을 pane 수만큼 되풀이하는 것이다.
#[cfg(unix)]
fn scan_cmdlines() -> std::collections::HashMap<u32, String> {
    let Ok(out) = std::process::Command::new("ps").args(["-axo", "pid=,args="]).output() else {
        return std::collections::HashMap::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            // pid 는 폭에 맞춰 오른쪽 정렬돼 앞에 공백이 붙는다.
            let (pid, args) = line.trim_start().split_once(' ')?;
            let pid: u32 = pid.parse().ok()?;
            let args = args.trim();
            (!args.is_empty()).then(|| (pid, args.to_string()))
        })
        .collect()
}

/// pid → 명령줄. **표 한 벌을 500ms 캐시**한다.
///
/// 종전에는 부를 때마다 `ps -p <pid>` 를 띄웠다. 이 함수는 pane 판정·board·정보
/// 패널이 pane 마다 부르는 자리라, 창이 열 개면 프로세스를 열 개 낳는 일이 화면
/// 갱신 박자로 되풀이됐다 — 2026-08-29 실측(`sample`)에서 유휴 중인 앱의 CPU
/// 표본 상위에 `__posix_spawn` 이 올라왔고, 그 스택이 board 조회였다.
///
/// 캐시가 아니라 **한 번에 다 받는 것**이 핵심이다. `ps -p` 도 커널의 프로세스
/// 표를 통째로 훑으므로 하나를 묻는 값과 전부를 묻는 값이 거의 같다.
///
/// TTL 이 500ms 인 것은 argv 가 `exec` 로 바뀌기 때문이다 — 셸 pane 에서 명령을
/// 치면 그 자리에서 달라지므로 영구 캐시는 못 쓴다. 이 값은 같은 파일의
/// `proc_cache` 와 맞춘 것이다.
#[cfg(unix)]
pub fn process_cmdline(pid: u32) -> Option<String> {
    use std::collections::HashMap;
    use std::sync::Mutex;
    static CACHE: Mutex<Option<(Instant, HashMap<u32, String>)>> = Mutex::new(None);
    // 락이 깨져도 답은 내야 한다 — 여기서 None 을 돌리면 학생 판정이 통째로 죽는다.
    let mut g = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if !g.as_ref().is_some_and(|(t, _)| t.elapsed().as_millis() < 500) {
        *g = Some((Instant::now(), scan_cmdlines()));
    }
    g.as_ref()?.1.get(&pid).cloned()
}

/// 여러 키를 **ps 한 번**으로 읽는다 — board 는 pane 마다 여러 env 를 보는데 키당
/// 프로세스를 띄우면 폴링(1s)마다 pane 수 × 키 수만큼 ps 가 뜬다.
#[cfg(unix)]
pub fn process_env_vars(pid: u32, keys: &[&str]) -> std::collections::HashMap<String, String> {
    let mut found = std::collections::HashMap::new();
    let Ok(out) = std::process::Command::new("ps")
        .args(["eww", "-p", &pid.to_string(), "-o", "command="])
        .output()
    else {
        return found;
    };
    let s = String::from_utf8_lossy(&out.stdout);
    for tok in s.split_whitespace() {
        for k in keys {
            if let Some(v) = tok.strip_prefix(&format!("{k}=")) {
                if !v.is_empty() {
                    found.insert((*k).to_string(), v.to_string());
                }
            }
        }
    }
    found
}

#[cfg(not(unix))]
pub fn process_env_vars(_pid: u32, _keys: &[&str]) -> std::collections::HashMap<String, String> {
    std::collections::HashMap::new()
}

#[cfg(test)]
mod cmdline_tests {
    // process_cmdline over the test binary's own pid must recover a non-empty
    // command line — exercises the Windows PEB/ReadProcessMemory chain (and the
    // Unix `ps` path) on a process we control.
    #[test]
    fn cmdline_of_self_nonempty() {
        let cmd = super::process_cmdline(std::process::id());
        assert!(cmd.is_some_and(|c| !c.trim().is_empty()));
    }

    /// 표를 한 번에 뜨는 길로 바꾸면서 깨지기 쉬운 곳은 파싱이다. `ps` 는 pid 를
    /// 폭에 맞춰 오른쪽 정렬하고 명령줄에는 공백이 얼마든지 들어가므로, 첫 공백
    /// 하나로만 갈라야 인자가 잘리지 않는다.
    #[cfg(unix)]
    #[test]
    fn 명령줄_표는_첫_공백에서만_갈린다() {
        let table = super::scan_cmdlines();
        assert!(!table.is_empty(), "ps 표가 비었다");
        // 자기 자신은 반드시 있고, 인자가 여럿이면 그게 온전히 남아야 한다.
        let me = table.get(&std::process::id()).expect("자기 pid 가 표에 없다");
        assert!(!me.trim().is_empty());
        // 커널 스레드처럼 명령줄이 빈 줄은 아예 안 담긴다 — 담기면 「이름 없는
        // 프로세스」가 학생 판정에 섞인다.
        assert!(table.values().all(|v| !v.trim().is_empty()));
    }
}

#[cfg(test)]
mod process_table_tests {
    use super::{process_table_shared, ProcessTable, ProcessTablePokes};
    use std::time::{Duration, Instant};

    #[test]
    fn concurrent_submissions_share_initial_and_trailing_scans() {
        let now = Instant::now();
        let mut pokes = ProcessTablePokes::default();
        assert!(pokes.submit(now));
        for _ in 0..1000 {
            assert!(!pokes.submit(now));
        }
        assert_eq!(pokes.next, Some(now + Duration::from_millis(100)));
        pokes.scanned(now + Duration::from_millis(100));
        assert_eq!(pokes.next, Some(now + Duration::from_millis(400)));
        pokes.scanned(now + Duration::from_millis(400));
        assert_eq!(pokes.next, None);
        assert!(pokes.submit(now + Duration::from_millis(401)));
    }

    #[test]
    fn late_burst_keeps_trailing_scan_without_extra_worker() {
        let now = Instant::now();
        let mut pokes = ProcessTablePokes::default();
        assert!(pokes.submit(now));
        pokes.scanned(now + Duration::from_millis(100));
        assert!(!pokes.submit(now + Duration::from_millis(390)));
        pokes.scanned(now + Duration::from_millis(400));
        assert_eq!(pokes.next, Some(now + Duration::from_millis(790)));
        assert!(!pokes.submit(now + Duration::from_millis(401)));
        assert_eq!(pokes.next, Some(now + Duration::from_millis(501)));
        pokes.scanned(now + Duration::from_millis(501));
        assert_eq!(pokes.next, Some(now + Duration::from_millis(801)));
        pokes.scanned(now + Duration::from_millis(801));
        assert_eq!(pokes.next, None);
    }

    #[test]
    fn sustained_submissions_do_not_starve_refresh() {
        let now = Instant::now();
        let mut pokes = ProcessTablePokes::default();
        pokes.submit(now);
        for tick in 1..=30 {
            let at = now + Duration::from_millis(tick * 100);
            assert!(!pokes.submit(at));
            let due = pokes.next.unwrap();
            if at >= due {
                pokes.scanned(at);
            }
            assert!(pokes.next.unwrap() <= at + Duration::from_millis(400));
        }
        pokes.scanned(now + Duration::from_millis(3400));
        assert_eq!(pokes.next, None);
    }

    #[test]
    fn submission_after_final_scan_claims_its_own_worker() {
        let now = Instant::now();
        let mut pokes = ProcessTablePokes::default();
        assert!(pokes.submit(now));
        pokes.scanned(now + Duration::from_millis(400));
        let old_worker_finished = pokes.next.is_none();
        assert!(pokes.submit(now + Duration::from_millis(410)));
        assert!(old_worker_finished);
        assert_eq!(pokes.next, Some(now + Duration::from_millis(510)));
        assert!(!pokes.submit(now + Duration::from_millis(411)));
    }

    /// 표 자체가 맞는지 — 자기 프로세스는 반드시 들어 있다. `ps` 출력 파싱이
    /// 깨지면(열 순서·comm 공백) 여기서 잡힌다.
    #[test]
    fn shared_table_contains_this_process() {
        let t: ProcessTable = process_table_shared();
        let me = std::process::id();
        assert!(!t.is_empty(), "표가 비었다 — ps 파싱 실패");
        assert!(t.iter().any(|(pid, _, _)| *pid == me), "자기 pid 가 표에 없다");
    }

    /// TTL 안의 두 호출은 **같은 Arc** 여야 한다. 여기서 복사본이 나오기 시작하면
    /// 렌더가 pane 마다 매 프레임 표를 통째로 clone 하던 시절로 돌아간다 —
    /// 프로세스 수백 개면 프레임마다 그만큼의 String 할당이다.
    #[test]
    fn shared_table_is_not_copied_within_ttl() {
        // 첫 호출이 백그라운드 갱신을 걸었을 수 있으니 가라앉힌 뒤에 잰다.
        let _ = process_table_shared();
        std::thread::sleep(std::time::Duration::from_millis(80));
        let a = process_table_shared();
        let b = process_table_shared();
        assert!(std::sync::Arc::ptr_eq(&a, &b), "TTL 안인데 표가 새로 만들어졌다");
    }
}
