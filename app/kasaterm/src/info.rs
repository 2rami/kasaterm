//! Info 패널 — 활성 pane 의 셸 아래에서 도는 프로세스와, 그것들이 listen 중인
//! 포트. 개발 중엔 "dev 서버가 아직 살아 있나, 몇 번 포트를 잡았나"를 확인하려고
//! `lsof -i` 를 치는 일이 잦은데, 그 답이 pane 옆에 상주하면 물어볼 일이 없다.
//!
//! 수집은 GUI 스레드 밖에서 돈다 — `ps` + `lsof` 를 fork 하므로 렌더 루프에서
//! 부르면 프레임을 떨군다. `App::pump_info` 가 패널이 열려 있을 때만 워커를
//! 깨우고, 결과는 `InfoState.rows`(Arc<Mutex>) 로 넘어온다.
//!
//! kasa_pty::process_table 을 쓰지 않는 이유: 그쪽은 (pid, ppid, comm) 만 주는데
//! 여기서는 좀비 판별용 stat 과 표시용 argv 가 함께 필요하고, 한 번의 ps 로 다
//! 받는 편이 fork 를 늘리지 않는다.
use super::*;
use std::collections::HashMap;

/// Info 목록의 한 행. `depth` 는 셸 바로 아래 자식이 0 이고, 렌더가 들여쓰기에
/// 쓴다. 셸 자신은 목록이 아니라 패널 머리에 따로 뜬다.
#[derive(Clone, Default, PartialEq)]
pub(crate) struct ProcRow {
    pub(crate) pid: u32,
    pub(crate) depth: u8,
    /// 표시용 짧은 이름(`node`, `claude`). argv[0] 의 파일명.
    pub(crate) name: String,
    /// argv 전체에서 이름을 뺀 나머지 — 부제로 흐리게 붙인다.
    pub(crate) rest: String,
    /// `ps` 가 보고한 CPU 점유율(%).
    pub(crate) cpu: f32,
    /// resident set size(KB).
    pub(crate) mem_kb: u64,
}

impl ProcRow {
    /// `458 MB` · `1.2 GB` · `640 KB`. KB 를 그대로 보여주는 건 1MB 미만일
    /// 때뿐이다 — 대부분의 개발 프로세스는 MB 대라 자릿수만 늘어난다.
    pub(crate) fn mem_label(&self) -> String {
        match self.mem_kb {
            0..=1023 => format!("{} KB", self.mem_kb),
            1024..=1_048_575 => format!("{} MB", self.mem_kb / 1024),
            _ => format!("{:.1} GB", self.mem_kb as f64 / 1_048_576.0),
        }
    }
}

/// listen 중인 TCP 포트 하나와 그걸 쥔 프로세스. 포트를 프로세스 행에 칩으로
/// 붙이는 대신 별도 섹션으로 뺀 건 폭 때문이다 — 좁은 칼럼에서 칩이 이름과
/// 자원 수치를 밀어냈다.
#[derive(Clone, Default, PartialEq)]
pub(crate) struct PortRow {
    pub(crate) port: u16,
    pub(crate) pid: u32,
    /// 소유 프로세스의 표시 이름. 못 찾으면 빈 문자열.
    pub(crate) name: String,
    /// 이 pane 의 셸 자손이 **아니고** 작업 폴더가 같아서 딸려온 것. 띄운 셸이
    /// 죽어 launchd 밑으로 넘어간 dev 서버가 대부분이라, 이 pane 이 지금 돌리는
    /// 것처럼 보이면 안 된다(끄려고 pane 을 닫아도 안 죽는다).
    pub(crate) orphan: bool,
}

/// 한 번의 수집 결과. 셸 이름까지 같이 담는 건 머리와 목록이 같은 스냅샷에서
/// 나와야 pane 을 옮기는 찰나에 둘이 어긋나지 않기 때문이다.
#[derive(Clone, Default)]
pub(crate) struct InfoSnap {
    pub(crate) shell: String,
    pub(crate) rows: Vec<ProcRow>,
    pub(crate) ports: Vec<PortRow>,
}

/// `ps` 한 줄에서 뽑은 원시 레코드. 좀비도 담는다 — 목록에는 안 올리지만
/// 부모-자식 색인에는 있어야 좀비를 건너뛴 손자까지 트리가 이어진다.
struct Raw {
    pid: u32,
    ppid: u32,
    zombie: bool,
    cpu: f32,
    rss_kb: u64,
    args: String,
}

/// 셸 pid 아래의 프로세스와 그것들이 listen 중인 포트. 순서는 트리 선행 순회 —
/// 부모 바로 밑에 자식이 오도록 정렬해 들여쓰기가 말이 되게 한다.
pub(crate) fn collect(shell_pid: u32, root: Option<&std::path::Path>) -> InfoSnap {
    let table = process_snapshot();
    if table.is_empty() {
        return InfoSnap::default();
    }
    let by_pid: HashMap<u32, &Raw> = table.iter().map(|r| (r.pid, r)).collect();
    let out = build_rows(&table, shell_pid);
    // 셸 자신도 포트를 쥘 수 있다(`nc -l` 같은 걸 셸이 직접 돌린 경우).
    let mut mine: std::collections::HashSet<u32> = out.iter().map(|r| r.pid).collect();
    mine.insert(shell_pid);
    // 셸 자손만 보면 **정작 찾는 서버를 놓친다**. `npm run dev` 를 띄운 셸이
    // 끝나면 서버는 launchd(ppid 1) 밑으로 넘어가 트리에서 사라지는데, 포트는
    // 그대로 물고 있다(실측: dev 서버 넷 전부 ppid 1). 거노가 "포트 열려 있는데
    // info 가 못 잡는다" 고 한 게 이것 — 그래서 전체 listen 을 훑은 뒤,
    // **작업 폴더가 이 pane 의 레포 안**인 것까지 끌어온다. 폴더로 거르니
    // ControlCenter·Adobe 같은 시스템 포트는 안 딸려온다.
    let all = listening_ports();
    let extra: Vec<u32> = all
        .iter()
        .map(|(_, pid)| *pid)
        .filter(|pid| !mine.contains(pid))
        .collect();
    let cwds = if root.is_some() { cwds_of(&extra) } else { HashMap::new() };
    let ports = all
        .into_iter()
        .filter_map(|(port, pid)| {
            let own = mine.contains(&pid);
            if !own {
                let in_root = match (root, cwds.get(&pid)) {
                    (Some(r), Some(c)) => c.starts_with(r),
                    _ => false,
                };
                if !in_root {
                    return None;
                }
            }
            Some(PortRow {
                port,
                pid,
                name: by_pid
                    .get(&pid)
                    .map(|r| split_argv(&r.args).0)
                    .unwrap_or_default(),
                orphan: !own,
            })
        })
        .collect();
    let shell = by_pid
        .get(&shell_pid)
        .map(|r| split_argv(&r.args).0)
        // 로그인 셸의 argv[0] 은 `-zsh` 처럼 하이픈이 붙는다 — 표시용이라 뗀다.
        .map(|n| n.trim_start_matches('-').to_string())
        .unwrap_or_default();
    InfoSnap { shell, rows: out, ports }
}

/// 셸 아래 트리를 선행 순회해 목록 행으로 편다. `collect` 에서 뽑아낸 건
/// 순수 함수라 테스트가 가능해서다 — `ps` 를 fork 하는 쪽과 섞여 있으면
/// 좀비 관통·들여쓰기 같은 미묘한 규칙을 회귀로 못 잡는다.
fn build_rows(table: &[Raw], shell_pid: u32) -> Vec<ProcRow> {
    let by_pid: HashMap<u32, &Raw> = table.iter().map(|r| (r.pid, r)).collect();
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for r in table {
        children.entry(r.ppid).or_default().push(r.pid);
    }
    for kids in children.values_mut() {
        kids.sort_unstable();
    }
    let mut out = Vec::new();
    // 명시적 스택 DFS. 재귀를 피하는 건 깊이 때문이 아니라, ppid 가 순환하는
    // 이상 상태(부모가 죽고 pid 가 재사용된 찰나)에서도 멈추게 하려는 것 —
    // `seen` 이 같은 pid 를 두 번 펼치지 않는다.
    let mut seen = std::collections::HashSet::new();
    // 셸은 -1 로 시작한다 — 그래야 첫 자식이 0(들여쓰기 없음)이 되어, 머리로
    // 빠진 셸 자리만큼 목록 전체가 왼쪽으로 붙는다.
    let mut stack = vec![(shell_pid, -1i16)];
    while let Some((pid, depth)) = stack.pop() {
        if !seen.insert(pid) {
            continue;
        }
        // depth < 0 인 건 셸 자신뿐 — 머리에 따로 뜨므로 목록에선 뺀다.
        if depth >= 0 {
            if let Some(raw) = by_pid.get(&pid).filter(|r| !r.zombie) {
                let (name, rest) = split_argv(&raw.args);
                out.push(ProcRow {
                    pid,
                    depth: depth.min(u8::MAX as i16) as u8,
                    name,
                    rest,
                    cpu: raw.cpu,
                    mem_kb: raw.rss_kb,
                });
            }
        }
        if let Some(kids) = children.get(&pid) {
            // pop 이 역순으로 꺼내니 뒤집어 넣어야 pid 오름차순으로 나온다.
            for &k in kids.iter().rev() {
                stack.push((k, depth.saturating_add(1)));
            }
        }
    }
    out
}

/// argv 를 (표시 이름, 나머지) 로 가른다. argv[0] 이 절대경로면 파일명만 남겨
/// `/opt/homebrew/bin/node` 가 `node` 로 읽히게 한다.
fn split_argv(args: &str) -> (String, String) {
    let args = args.trim();
    let (head, rest) = match args.split_once(' ') {
        Some((h, r)) => (h, r.trim()),
        None => (args, ""),
    };
    let name = std::path::Path::new(head)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(head)
        .to_string();
    (name, rest.to_string())
}

#[cfg(unix)]
fn process_snapshot() -> Vec<Raw> {
    let Ok(out) = proc::command("ps")
        .args(["-A", "-o", "pid=,ppid=,stat=,pcpu=,rss=,args="])
        .output()
    else {
        return Vec::new();
    };
    let s = String::from_utf8_lossy(&out.stdout);
    let mut rows = Vec::new();
    for line in s.lines() {
        let mut it = line.split_whitespace();
        let (Some(pid), Some(ppid), Some(stat), Some(cpu), Some(rss_kb)) = (
            it.next().and_then(|x| x.parse::<u32>().ok()),
            it.next().and_then(|x| x.parse::<u32>().ok()),
            it.next(),
            it.next().and_then(|x| x.parse::<f32>().ok()),
            it.next().and_then(|x| x.parse::<u64>().ok()),
        ) else {
            continue;
        };
        // 좀비(stat 앞머리 'Z')는 뺀다 — 출력을 볼 수도, kill 할 수도 없는
        // 껍데기라 목록에 있어봐야 노이즈다(kero 가 0.1.26 에서 같은 걸 고쳤다).
        // 자식 색인에는 남겨야 좀비를 건너뛴 손자까지 트리가 이어진다.
        let args = it.collect::<Vec<_>>().join(" ");
        if args.is_empty() {
            continue;
        }
        rows.push(Raw { pid, ppid, zombie: stat.starts_with('Z'), cpu, rss_kb, args });
    }
    rows
}

#[cfg(windows)]
fn process_snapshot() -> Vec<Raw> {
    // Windows 엔 ps 가 없다. 프로세스 트리는 kasa_pty 의 Toolhelp 스냅샷을 그대로
    // 쓰고(같은 (pid, ppid, exe) 형태), argv 는 못 읽으면 exe 이름으로 떨어진다.
    // 좀비는 Toolhelp 스냅샷에 애초에 안 잡히므로 unix 쪽 stat 필터가 불필요하다.
    // CPU·메모리는 Toolhelp 가 안 주므로 0 — 렌더가 0 이면 수치를 생략한다.
    kasa_pty::process_table()
        .into_iter()
        .map(|(pid, ppid, name)| Raw {
            pid,
            ppid,
            zombie: false,
            cpu: 0.0,
            rss_kb: 0,
            args: kasa_pty::process_cmdline(pid).unwrap_or(name),
        })
        .collect()
}

/// listen 중인 TCP 포트 전부 — `(포트, pid)`. 실패하면 빈 목록이라 패널은
/// 포트 섹션만 비운 채 뜬다.
///
/// pid 로 미리 거르지 않는다. 고아가 된 dev 서버(띄운 셸이 죽어 ppid 1)를
/// 놓치지 않으려면 전부 받아 호출자가 걸러야 하고, 실측 비용도 46ms 로
/// pid 필터를 걸 때와 사실상 같다.
#[cfg(unix)]
fn listening_ports() -> Vec<(u16, u32)> {
    // `-F pn` 은 프로세스 레코드(p<pid>)와 이름 레코드(n<addr>)만 내보내는 lsof
    // 의 기계 판독 모드다. 사람이 읽는 표를 파싱하면 명령 이름에 공백이 든
    // 프로세스에서 열이 밀린다.
    let Ok(out) = proc::command("lsof")
        .args(["-nP", "-iTCP", "-sTCP:LISTEN", "-F", "pn"])
        .output()
    else {
        return Vec::new();
    };
    let s = String::from_utf8_lossy(&out.stdout);
    let mut ports = Vec::new();
    let mut cur: u32 = 0;
    for line in s.lines() {
        let (tag, val) = line.split_at(line.char_indices().nth(1).map_or(0, |(i, _)| i));
        match tag {
            "p" => cur = val.parse::<u32>().unwrap_or(0),
            "n" => {
                // `*:3000` · `127.0.0.1:8080` · `[::1]:5173` — 어느 쪽이든 포트는
                // 마지막 ':' 뒤다.
                if let Some(port) = val.rsplit(':').next().and_then(|p| p.parse::<u16>().ok()) {
                    ports.push((port, cur));
                }
            }
            _ => {}
        }
    }
    dedup_ports(ports)
}

#[cfg(windows)]
fn listening_ports() -> Vec<(u16, u32)> {
    let Ok(out) = proc::command("netstat").args(["-ano", "-p", "TCP"]).output() else {
        return Vec::new();
    };
    let s = String::from_utf8_lossy(&out.stdout);
    let mut ports = Vec::new();
    for line in s.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        // TCP  <local>  <remote>  LISTENING  <pid>
        if f.len() < 5 || !f[0].eq_ignore_ascii_case("TCP") || f[3] != "LISTENING" {
            continue;
        }
        let (Some(port), Some(pid)) = (
            f[1].rsplit(':').next().and_then(|p| p.parse::<u16>().ok()),
            f[4].parse::<u32>().ok(),
        ) else {
            continue;
        };
        if want.contains(&pid) {
            ports.push((port, pid));
        }
    }
    dedup_ports(ports)
}

/// pid → 작업 폴더. 셸 트리 밖의 포트를 "이 레포 것"으로 인정할지 가르는 유일한
/// 근거다. 포트를 쥔 프로세스만 물으므로 한 번의 fork 로 끝난다(실측 32ms).
#[cfg(unix)]
fn cwds_of(pids: &[u32]) -> HashMap<u32, std::path::PathBuf> {
    let mut out = HashMap::new();
    if pids.is_empty() {
        return out;
    }
    let list = pids.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    let Ok(o) = proc::command("lsof")
        .args(["-nP", "-a", "-p", &list, "-d", "cwd", "-F", "pn"])
        .output()
    else {
        return out;
    };
    let s = String::from_utf8_lossy(&o.stdout);
    let mut cur: u32 = 0;
    for line in s.lines() {
        let (tag, val) = line.split_at(line.char_indices().nth(1).map_or(0, |(i, _)| i));
        match tag {
            "p" => cur = val.parse::<u32>().unwrap_or(0),
            "n" if cur != 0 => {
                out.insert(cur, std::path::PathBuf::from(val));
            }
            _ => {}
        }
    }
    out
}

/// Windows 엔 남의 프로세스 cwd 를 싸게 읽을 방법이 없다 — 고아 서버는 못 잡고
/// 셸 자손만 뜬다(unix 와 달리 레포 폴더 확장이 안 된다).
#[cfg(windows)]
fn cwds_of(_pids: &[u32]) -> HashMap<u32, std::path::PathBuf> {
    HashMap::new()
}

/// 포트 오름차순 정렬 + 중복 제거. 같은 소켓이 IPv4 와 IPv6 로 한 번씩 잡히므로
/// 이게 없으면 대부분의 dev 서버가 두 줄로 뜬다.
fn dedup_ports(mut ports: Vec<(u16, u32)>) -> Vec<(u16, u32)> {
    ports.sort_unstable();
    ports.dedup();
    ports
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(pid: u32, ppid: u32, zombie: bool, args: &str) -> Raw {
        Raw { pid, ppid, zombie, cpu: 0.0, rss_kb: 0, args: args.to_string() }
    }

    #[test]
    fn rows_exclude_the_shell_and_start_at_depth_zero() {
        let t = vec![raw(100, 1, false, "-zsh"), raw(200, 100, false, "/usr/bin/node srv.js")];
        let rows = build_rows(&t, 100);
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].pid, rows[0].depth), (200, 0));
        assert_eq!((rows[0].name.as_str(), rows[0].rest.as_str()), ("node", "srv.js"));
    }

    #[test]
    fn zombie_is_hidden_but_its_children_still_show() {
        let t = vec![
            raw(100, 1, false, "-zsh"),
            raw(200, 100, true, "npm <defunct>"),
            raw(300, 200, false, "node worker.js"),
        ];
        let rows = build_rows(&t, 100);
        assert_eq!(rows.iter().map(|r| r.pid).collect::<Vec<_>>(), vec![300]);
        // 좀비가 목록에서 빠져도 자리는 차지한다 — 손자를 0 으로 당기면
        // 형제 프로세스와 같은 단으로 보여 계보가 거짓이 된다.
        assert_eq!(rows[0].depth, 1);
    }

    #[test]
    fn traversal_is_preorder_with_siblings_in_pid_order() {
        let t = vec![
            raw(100, 1, false, "-zsh"),
            raw(300, 100, false, "b"),
            raw(200, 100, false, "a"),
            raw(250, 200, false, "a-child"),
        ];
        let rows = build_rows(&t, 100);
        assert_eq!(rows.iter().map(|r| (r.pid, r.depth)).collect::<Vec<_>>(), vec![
            (200, 0),
            (250, 1),
            (300, 0)
        ]);
    }

    #[test]
    fn ppid_cycle_terminates() {
        let t = vec![raw(100, 200, false, "-zsh"), raw(200, 100, false, "child")];
        assert_eq!(build_rows(&t, 100).len(), 1);
    }

    #[test]
    fn same_socket_on_v4_and_v6_collapses_to_one_row() {
        assert_eq!(dedup_ports(vec![(3000, 7), (3000, 7), (80, 9)]), vec![(80, 9), (3000, 7)]);
    }

    #[test]
    fn mem_label_switches_unit_at_each_boundary() {
        let at = |kb| ProcRow { mem_kb: kb, ..Default::default() }.mem_label();
        assert_eq!(at(640), "640 KB");
        assert_eq!(at(1024), "1 MB");
        assert_eq!(at(1_048_575), "1023 MB");
        assert_eq!(at(1_258_291), "1.2 GB");
    }

    #[test]
    fn app_names_shorten_only_when_known() {
        assert_eq!(short_app_name("Visual Studio Code"), "VS Code");
        assert_eq!(short_app_name("Cursor"), "Cursor");
    }
}


impl App {
    /// 패널이 열려 있는 동안 워커를 주기적으로 깨운다. 렌더 루프에서 불리므로
    /// 여기서 직접 ps/lsof 를 돌리면 안 된다 — 스레드를 띄우고 즉시 반환한다.
    /// 워커가 하나 도는 동안 다시 띄우지 않도록 `busy` 로 막는다.
    pub(crate) fn pump_info(&mut self) {
        if self.info.tab != state::SideTab::Info || !self.git.col_visible {
            return;
        }
        // 디렉터리 섹션은 파일트리와 같은 앵커를 보여준다 — 사이드바를 닫아둬도
        // 맞아야 하므로 file_tree.root 를 읽지 않고 여기서 직접 판정한다.
        self.info.root = self.info_root();
        let Some(pid) = self.active_pty().and_then(|p| p.shell_pid()) else {
            return;
        };
        // pane 을 옮기면 대상이 바뀐 것이므로 캐시를 버리고 즉시 다시 뜬다.
        if self.info.shell_pid != Some(pid) {
            self.info.shell_pid = Some(pid);
            self.info.last_refresh = None;
            self.info.scroll = 0.0;
            if let Ok(mut g) = self.info.snap.lock() {
                *g = InfoSnap::default();
            }
        }
        let fresh = self
            .info
            .last_refresh
            .is_some_and(|t: Instant| t.elapsed() < std::time::Duration::from_millis(1500));
        if fresh || self.info.busy.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        self.info.last_refresh = Some(Instant::now());
        self.info.busy.store(true, std::sync::atomic::Ordering::Relaxed);
        let snap = self.info.snap.clone();
        let busy = self.info.busy.clone();
        let proxy = self.proxy.clone();
        // 포트 섹션이 셸 트리 밖 프로세스를 끌어올 때 쓰는 경계. **git 레포일
        // 때만** 넓힌다 — `~/Desktop` 처럼 레포가 아닌 폴더를 앵커로 쓰면 그
        // 아래 모든 프로젝트의 서버가 딸려온다(실측 15개). "이 레포에서 도는
        // 서버"는 뜻이 있지만 "이 폴더 밑 아무거나"는 잡음일 뿐이다.
        let root = if self.info.root_is_repo { self.info.root.clone() } else { None };
        std::thread::spawn(move || {
            let fresh = collect(pid, root.as_deref());
            if let Ok(mut g) = snap.lock() {
                *g = fresh;
            }
            busy.store(false, std::sync::atomic::Ordering::Relaxed);
            // 새 목록이 붙었으니 한 프레임 그려달라고 깨운다.
            let _ = proxy.send_event(UserEvent::Redraw);
        });
    }

    /// 디렉터리 섹션이 보여줄 경로 — 활성 pane 의 cwd 를 감싸는 git 레포 루트,
    /// 레포 밖이면 cwd 그대로. `root_is_repo` 로 어느 쪽인지 함께 기록한다.
    fn info_root(&mut self) -> Option<std::path::PathBuf> {
        let active = self.ws.lock().ok().and_then(|w| w.active_pane.clone())?;
        let cwd = self
            .pane_view_cwd
            .get(&active)
            .or_else(|| self.pane_cwd_cache.get(&active))
            .cloned()?;
        match self.anchored_tree_root(&cwd) {
            Some(repo) => {
                self.info.root_is_repo = true;
                Some(repo)
            }
            None => {
                self.info.root_is_repo = false;
                Some(cwd)
            }
        }
    }

    /// 프로세스 종료. 렌더 스레드에서 불리므로 죽는 걸 기다리지 않고, 잠시 뒤
    /// 다시 그려달라고 깨워 목록에서 사라지는 걸 눈으로 확인시킨다. 이 앱은
    /// idle 이면 완전히 잠들어서(ControlFlow::Wait) 깨우지 않으면 다음 마우스
    /// 움직임까지 죽은 행이 남는다.
    pub(crate) fn kill_process(&mut self, pid: u32, force: bool) {
        #[cfg(unix)]
        unsafe {
            libc::kill(pid as i32, if force { libc::SIGKILL } else { libc::SIGTERM });
        }
        #[cfg(windows)]
        {
            let _ = crate::proc::command("taskkill")
                .args(if force { vec!["/F", "/PID"] } else { vec!["/PID"] })
                .arg(pid.to_string())
                .spawn();
        }
        self.set_toast(format!("{} {pid}", if force { "강제 종료" } else { "종료 신호" }));
        // 스로틀을 앞당겨 다음 프레임이 곧바로 다시 수집하게 한다.
        self.info.last_refresh = None;
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(350));
            let _ = proxy.send_event(UserEvent::Redraw);
        });
    }

    /// 우클릭 메뉴 실행.
    pub(crate) fn run_info_menu_action(&mut self, action: state::InfoMenuAction, pid: u32) {
        use state::InfoMenuAction as A;
        match action {
            A::Terminate => self.kill_process(pid, false),
            A::ForceKill => self.kill_process(pid, true),
            A::CopyPid => self.copy_to_clipboard(pid.to_string(), "PID 복사됨"),
            A::CopyCmd => {
                let cmd = self.info.snap.lock().ok().and_then(|s| {
                    s.rows.iter().find(|r| r.pid == pid).map(|r| {
                        if r.rest.is_empty() {
                            r.name.clone()
                        } else {
                            format!("{} {}", r.name, r.rest)
                        }
                    })
                });
                if let Some(cmd) = cmd {
                    self.copy_to_clipboard(cmd, "명령 복사됨");
                }
            }
        }
    }

    /// 클립보드에 넣고 토스트를 띄운다. 클립보드를 못 열면 조용히 로그만 —
    /// 실패했는데 "복사됨" 이 뜨는 게 아무 반응 없는 것보다 나쁘다.
    pub(crate) fn copy_to_clipboard(&mut self, text: String, toast: &str) {
        match arboard::Clipboard::new() {
            Ok(mut cb) => {
                if cb.set_text(text).is_ok() {
                    self.set_toast(toast.to_string());
                }
            }
            Err(e) => eprintln!("[kasaterm] clipboard open failed: {e}"),
        }
    }
}

/// 우측 칼럼 두 탭이 공유하는 머리 — 탭 이름 둘 + 확장/닫기 버튼. 활성 탭은
/// 밑줄로 표시한다(배경 pill 은 칼럼이 좁을 때 글자를 먹는다). 반환값은 본문이
/// 시작할 y.
///
/// `App` 메서드가 아니라 자유 함수인 건 빌림 때문이다 — 호출부는 이미
/// `self.gpu.as_mut()` 로 gpu 필드를 빌린 상태라 `self.method(g)` 는 self 를
/// 통째로 다시 빌려 E0499 가 난다. 쓰는 필드만 따로 받으면 서로 겹치지 않는다.
pub(crate) fn draw_side_tabs(
    g: &mut gpu::GpuRenderer,
    cursor: (f32, f32),
    info: &mut state::InfoState,
    git: &mut state::GitState,
    x: f32,
    w: f32,
    top: f32,
) -> f32 {
    let y = top + 10.0;
    let bi = 15.0_f32;
    let close_x = x + w - 12.0 - bi;
    let expand_x = close_x - bi - 8.0;
    let bhov = |cx: f32| {
        cursor.0 >= cx - 3.0
            && cursor.0 <= cx + bi + 3.0
            && cursor.1 >= y - 3.0
            && cursor.1 <= y + bi + 3.0
    };
    g.queue_icon(
        "maximize",
        expand_x,
        y,
        bi,
        if bhov(expand_x) { theme::text() } else { theme::text_mute() },
    );
    g.queue_icon(
        "x",
        close_x,
        y,
        bi,
        if bhov(close_x) { theme::text() } else { theme::text_mute() },
    );
    git.col_expand_rect = Some((expand_x - 3.0, y - 3.0, bi + 6.0, bi + 6.0));
    git.col_close_rect = Some((close_x - 3.0, y - 3.0, bi + 6.0, bi + 6.0));
    info.tab_rects.clear();
    let mut tx = x + 14.0;
    for (tab, label) in [(state::SideTab::Git, "Git"), (state::SideTab::Info, "Info")] {
        let active = info.tab == tab;
        let tw = g.measure_chrome_text(label, 12.0, active);
        let hot = (tx - 4.0, y - 4.0, tw + 8.0, 21.0);
        let hovered = cursor.0 >= hot.0
            && cursor.0 <= hot.0 + hot.2
            && cursor.1 >= hot.1
            && cursor.1 <= hot.1 + hot.3;
        let col = if active {
            theme::text()
        } else if hovered {
            theme::text_dim()
        } else {
            theme::text_mute()
        };
        g.draw_text(
            tx,
            y,
            label,
            gpu::DrawOpts { font_size: 12.0, color: col, bold: active, italic: false },
        );
        if active {
            g.rect(tx, y + 17.0, tw, 1.5, theme::accent());
        }
        info.tab_rects.push((tab, hot));
        tx += tw + 16.0;
    }
    y + 27.0
}

const ROW_H: f32 = 22.0;
const SEC_H: f32 = 26.0;
/// 섹션 본문과 다음 섹션 머리 사이 숨. 없으면 목록 마지막 행과 다음 머리가
/// 붙어 두 섹션이 한 덩어리로 읽힌다.
const SEC_GAP: f32 = 8.0;
const SHELL_H: f32 = 44.0;
const BTN_H: f32 = 24.0;
const PATH_LINE_H: f32 = 15.0;
const EMPTY_H: f32 = 22.0;

/// Info 탭 본문 — 셸 머리 / 프로젝트 디렉터리 / 프로세스 / 포트. 프로세스는
/// 셸 아래 트리를 들여쓰기로 그리고 CPU·메모리를 오른쪽에 붙이며, 포트는 좁은
/// 칼럼에서 이름을 밀어내지 않도록 별도 섹션으로 뺐다.
pub(crate) fn draw_info_col(
    g: &mut gpu::GpuRenderer,
    cursor: (f32, f32),
    info: &mut state::InfoState,
    x: f32,
    w: f32,
    top: f32,
    bottom: f32,
) {
    let x0 = x + 14.0;
    let right = x + w - 12.0;
    let avail = (right - x0).max(0.0);
    let snap = info.snap.lock().map(|s| s.clone()).unwrap_or_default();
    info.port_rects.clear();
    info.proc_rects.clear();
    info.kill_rects.clear();
    info.sec_rects.clear();
    info.dir_btn_rects.clear();
    info.refresh_rect = None;

    // 경로 줄바꿈을 먼저 재는 건 내용 높이에 들어가기 때문이다 — 높이를 알아야
    // 스크롤을 그리기 *전에* clamp 할 수 있고, 그래야 한 프레임 밀리지 않는다.
    let path_s = info
        .root
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let path_lines = if info.dir_collapsed || path_s.is_empty() {
        Vec::new()
    } else {
        wrap_path(g, &path_s, avail, 2)
    };
    let dir_h = if info.dir_collapsed {
        0.0
    } else {
        path_lines.len() as f32 * PATH_LINE_H + 8.0 + BTN_H + 10.0
    };
    let list_h = |collapsed: bool, n: usize| {
        if collapsed {
            0.0
        } else if n == 0 {
            EMPTY_H
        } else {
            n as f32 * ROW_H
        }
    };
    let content = SHELL_H
        + SEC_H * 3.0
        + SEC_GAP * 2.0
        + dir_h
        + list_h(info.procs_collapsed, snap.rows.len())
        + list_h(info.ports_collapsed, snap.ports.len())
        + 14.0;
    info.scroll = info.scroll.clamp(0.0, (content - (bottom - top)).max(0.0));
    let mut y = top - info.scroll;

    // ── 셸 머리 ──
    if y + SHELL_H > top && y < bottom {
        g.queue_icon("terminal", x0, y + 4.0, 14.0, theme::text_mute());
        let shell = if snap.shell.is_empty() { "읽는 중…" } else { snap.shell.as_str() };
        g.draw_text(
            x0 + 21.0,
            y + 2.0,
            shell,
            gpu::DrawOpts { font_size: 13.0, color: theme::text(), bold: true, italic: false },
        );
        if let Some(pid) = info.shell_pid {
            g.draw_text(
                x0 + 21.0,
                y + 20.0,
                &format!("pid {pid}"),
                gpu::DrawOpts { font_size: 10.5, color: theme::text_mute(), bold: false, italic: false },
            );
        }
        let rr = (right - 15.0, y + 3.0, 15.0, 15.0);
        let rhov = hit(cursor, &(rr.0 - 4.0, rr.1 - 4.0, rr.2 + 8.0, rr.3 + 8.0));
        g.queue_icon(
            "rotate-cw",
            rr.0,
            rr.1,
            rr.2,
            if rhov { theme::text() } else { theme::text_mute() },
        );
        info.refresh_rect = Some((rr.0 - 4.0, rr.1 - 4.0, rr.2 + 8.0, rr.3 + 8.0));
    }
    y += SHELL_H;

    // ── 프로젝트 디렉터리 ──
    // git 레포라서 골라진 것인지 cwd 그대로인지를 배지로 밝힌다. 트리 루트가
    // 왜 여기인지 묻게 만들지 않는 게 목적이라, 배지 없이 경로만 두면 의미가
    // 반쯤 사라진다.
    let badge = if info.root_is_repo { "git 레포" } else { "현재 경로" };
    let r = draw_section(g, cursor, "프로젝트 디렉터리", None, Some(badge), info.dir_collapsed, x, w, y, bottom, top);
    info.sec_rects.push((state::InfoSection::Dir, r));
    y += SEC_H;
    if !info.dir_collapsed {
        for line in &path_lines {
            if y + PATH_LINE_H > top && y < bottom {
                g.draw_text(
                    x0,
                    y,
                    line,
                    gpu::DrawOpts { font_size: 11.0, color: theme::text_dim(), bold: false, italic: false },
                );
            }
            y += PATH_LINE_H;
        }
        y += 8.0;
        if y + BTN_H > top && y < bottom {
            #[cfg(target_os = "macos")]
            let reveal = "Finder";
            #[cfg(not(target_os = "macos"))]
            let reveal = "탐색기";
            let editor = crate::proc::open_with_apps().first().map(|(n, _)| short_app_name(n));
            let mut btns: Vec<(state::InfoDirBtn, &str, &str)> =
                vec![(state::InfoDirBtn::Reveal, "external-link", reveal)];
            if let Some(name) = editor {
                btns.push((state::InfoDirBtn::Editor, "file-code", name));
            }
            btns.push((state::InfoDirBtn::CopyPath, "copy", "복사"));
            let gap = 6.0;
            let bw = ((avail - gap * (btns.len() - 1) as f32) / btns.len() as f32).max(0.0);
            for (i, (kind, icon, label)) in btns.into_iter().enumerate() {
                let bx = x0 + i as f32 * (bw + gap);
                let hov = hit(cursor, &(bx, y, bw, BTN_H));
                round_rect(
                    g,
                    bx,
                    y,
                    bw,
                    BTN_H,
                    theme::RADIUS_SM,
                    if hov { theme::surface_hover() } else { theme::surface() },
                );
                let col = if hov { theme::text() } else { theme::text_dim() };
                let lw = g.measure_chrome_text(label, 10.5, false);
                // 아이콘+글자가 안 들어가면 아이콘만 가운데 — 잘린 글자보다 낫다.
                if lw + 12.0 + 4.0 + 10.0 <= bw {
                    let inner = 12.0 + 4.0 + lw;
                    let ix = bx + (bw - inner) / 2.0;
                    g.queue_icon(icon, ix, y + 6.0, 12.0, col);
                    g.draw_text(
                        ix + 16.0,
                        y + 6.0,
                        label,
                        gpu::DrawOpts { font_size: 10.5, color: col, bold: false, italic: false },
                    );
                } else {
                    g.queue_icon(icon, bx + (bw - 12.0) / 2.0, y + 6.0, 12.0, col);
                }
                info.dir_btn_rects.push((kind, (bx, y, bw, BTN_H)));
            }
        }
        y += BTN_H + 10.0;
    }
    y += SEC_GAP;

    // ── 프로세스 ──
    let r = draw_section(
        g, cursor, "프로세스", Some(snap.rows.len()), None, info.procs_collapsed, x, w, y, bottom, top,
    );
    info.sec_rects.push((state::InfoSection::Procs, r));
    y += SEC_H;
    if !info.procs_collapsed {
        if snap.rows.is_empty() {
            draw_empty(g, x0, y, top, bottom, "실행 중인 프로세스 없음");
            y += EMPTY_H;
        }
        for p in &snap.rows {
            if y + ROW_H > top && y < bottom {
                draw_proc_row(g, cursor, info, p, x, w, x0, right, y);
            }
            info.proc_rects.push((p.pid, (x, y, w, ROW_H)));
            y += ROW_H;
        }
    }
    y += SEC_GAP;

    // ── 포트 ──
    let r = draw_section(
        g, cursor, "포트", Some(snap.ports.len()), None, info.ports_collapsed, x, w, y, bottom, top,
    );
    info.sec_rects.push((state::InfoSection::Ports, r));
    y += SEC_H;
    if !info.ports_collapsed {
        if snap.ports.is_empty() {
            draw_empty(g, x0, y, top, bottom, "listen 중인 포트 없음");
            y += EMPTY_H;
        }
        for p in &snap.ports {
            if y + ROW_H > top && y < bottom {
                draw_port_row(g, cursor, p, x, w, x0, right, y);
            }
            info.port_rects.push((p.port, (x, y, w, ROW_H)));
            y += ROW_H;
        }
    }

    draw_proc_menu(g, cursor, info, x, w, top, bottom);
}

fn hit(cursor: (f32, f32), r: &(f32, f32, f32, f32)) -> bool {
    cursor.0 >= r.0 && cursor.0 <= r.0 + r.2 && cursor.1 >= r.1 && cursor.1 <= r.1 + r.3
}

/// 접히는 섹션 머리 — 셰브런 + 이름 + (개수 배지 | 상태 배지). 반환값은 클릭
/// 판정 rect.
#[allow(clippy::too_many_arguments)]
fn draw_section(
    g: &mut gpu::GpuRenderer,
    cursor: (f32, f32),
    label: &str,
    count: Option<usize>,
    badge: Option<&str>,
    collapsed: bool,
    x: f32,
    w: f32,
    y: f32,
    bottom: f32,
    top: f32,
) -> (f32, f32, f32, f32) {
    let r = (x, y, w, SEC_H);
    if y + SEC_H <= top || y >= bottom {
        return r;
    }
    let hov = hit(cursor, &r);
    if hov {
        g.rect(x, y, w, SEC_H, theme::surface_hover());
    }
    let x0 = x + 14.0;
    g.queue_icon(
        if collapsed { "chevron-right" } else { "chevron-down" },
        x0 - 3.0,
        y + 7.0,
        12.0,
        theme::text_mute(),
    );
    g.draw_text(
        x0 + 13.0,
        y + 7.0,
        label,
        gpu::DrawOpts { font_size: 11.0, color: theme::text_dim(), bold: true, italic: false },
    );
    let right = x + w - 12.0;
    if let Some(n) = count {
        let s = n.to_string();
        let tw = g.measure_chrome_text(&s, 10.0, true);
        round_rect(g, right - tw - 10.0, y + 5.0, tw + 10.0, 16.0, 8.0, theme::surface());
        g.draw_text(
            right - tw - 5.0,
            y + 7.0,
            &s,
            gpu::DrawOpts { font_size: 10.0, color: theme::text_mute(), bold: true, italic: false },
        );
    } else if let Some(b) = badge {
        let tw = g.measure_chrome_text(b, 10.0, false);
        g.draw_text(
            right - tw,
            y + 7.0,
            b,
            gpu::DrawOpts { font_size: 10.0, color: theme::text_mute(), bold: false, italic: false },
        );
    }
    r
}

fn draw_empty(g: &mut gpu::GpuRenderer, x0: f32, y: f32, top: f32, bottom: f32, text: &str) {
    if y + EMPTY_H <= top || y >= bottom {
        return;
    }
    g.draw_text(
        x0,
        y + 4.0,
        text,
        gpu::DrawOpts { font_size: 11.0, color: theme::text_mute(), bold: false, italic: false },
    );
}

/// 프로세스 한 줄 — `● 이름 argv…            12% · 458 MB  pid`.
/// 행에 커서가 있으면 자원 수치 자리에 종료(×) 버튼이 들어선다(kero 와 같은
/// 맞바꿈). 버튼을 상시 노출하면 스크롤하다 잘못 누르기 쉽다.
#[allow(clippy::too_many_arguments)]
fn draw_proc_row(
    g: &mut gpu::GpuRenderer,
    cursor: (f32, f32),
    info: &mut state::InfoState,
    p: &ProcRow,
    x: f32,
    w: f32,
    x0: f32,
    right: f32,
    y: f32,
) {
    let row = (x, y, w, ROW_H);
    let hov = hit(cursor, &row);
    if hov {
        g.rect(x, y, w, ROW_H, theme::surface_hover());
    }
    // 오른쪽부터 채운다 — pid 와 수치가 자리를 먼저 잡아야 이름이 남는 폭을
    // 정확히 알고 잘린다.
    let pid_s = p.pid.to_string();
    let pid_w = g.measure_chrome_text(&pid_s, 10.0, false);
    g.draw_text(
        right - pid_w,
        y + 6.0,
        &pid_s,
        // pid 는 수치가 아니라 손잡이라 한 단계 더 물러나 있어야 한다 — 같은
        // 밝기로 두면 바로 왼쪽 메모리 값에 붙어 `2 MB 25655` 가 한 덩어리로
        // 읽힌다(간격만 벌려선 부족했다).
        gpu::DrawOpts {
            font_size: 10.0,
            color: theme::with_alpha(theme::text_mute(), 0xA0),
            bold: false,
            italic: false,
        },
    );
    let mut rx = right - pid_w - 14.0;
    if hov {
        let br = (rx - 16.0, y + 3.0, 16.0, 16.0);
        let bhov = hit(cursor, &br);
        if bhov {
            round_rect(g, br.0, br.1, br.2, br.3, 4.0, theme::with_alpha(theme::danger(), 0x33));
        }
        g.queue_icon(
            "x",
            br.0 + 3.0,
            br.1 + 3.0,
            10.0,
            if bhov { theme::danger() } else { theme::text_mute() },
        );
        info.kill_rects.push((p.pid, br));
        rx = br.0 - 6.0;
    } else if p.mem_kb > 0 {
        let m = format!("{:.0}% · {}", p.cpu, p.mem_label());
        let mw = g.measure_chrome_text(&m, 10.0, false);
        g.draw_text(
            rx - mw,
            y + 6.0,
            &m,
            gpu::DrawOpts { font_size: 10.0, color: theme::text_mute(), bold: false, italic: false },
        );
        rx -= mw + 8.0;
    }
    // 살아 있음을 알리는 dot. 프로세스가 "지금 도는 것"이라는 걸 한눈에 주는
    // 신호라, 목록이 낡았는지 판단하는 기준이 된다.
    let nx = x0 + p.depth as f32 * 11.0;
    round_rect(g, nx, y + 8.0, 6.0, 6.0, 3.0, theme::success());
    let tx = nx + 12.0;
    let avail = (rx - tx).max(0.0);
    let name = fit_text(g, &p.name, avail, 12.0, true);
    let name_w = g.measure_chrome_text(&name, 12.0, true);
    g.draw_text(
        tx,
        y + 4.0,
        &name,
        gpu::DrawOpts { font_size: 12.0, color: theme::text(), bold: true, italic: false },
    );
    if !p.rest.is_empty() && avail > name_w + 24.0 {
        let rest = fit_text(g, &p.rest, avail - name_w - 6.0, 11.0, false);
        g.draw_text(
            tx + name_w + 6.0,
            y + 5.0,
            &rest,
            gpu::DrawOpts { font_size: 11.0, color: theme::text_mute(), bold: false, italic: false },
        );
    }
}

/// 포트 한 줄 — `◆ 5173  node                    ↗`. 행 전체가 클릭 대상이라
/// `http://localhost:<port>` 로 연다.
#[allow(clippy::too_many_arguments)]
fn draw_port_row(
    g: &mut gpu::GpuRenderer,
    cursor: (f32, f32),
    p: &PortRow,
    x: f32,
    w: f32,
    x0: f32,
    right: f32,
    y: f32,
) {
    let row = (x, y, w, ROW_H);
    let hov = hit(cursor, &row);
    if hov {
        g.rect(x, y, w, ROW_H, theme::surface_hover());
    }
    // 이 pane 이 돌리는 게 아니면 점을 흐리게 — pane 을 닫아도 안 죽는다는
    // 사실이 목록에서 바로 보여야 한다(대개 셸이 죽어 launchd 로 넘어간 서버).
    let dot = if p.orphan { theme::text_dim() } else { theme::accent() };
    round_rect(g, x0, y + 8.0, 6.0, 6.0, 3.0, dot);
    let port_s = p.port.to_string();
    g.draw_text(
        x0 + 12.0,
        y + 4.0,
        &port_s,
        gpu::DrawOpts { font_size: 12.0, color: theme::text(), bold: true, italic: false },
    );
    let pw = g.measure_chrome_text(&port_s, 12.0, true);
    let mut rx = right;
    if hov {
        g.queue_icon("external-link", right - 12.0, y + 5.0, 12.0, theme::text_dim());
        rx -= 18.0;
    }
    if !p.name.is_empty() {
        let avail = (rx - x0 - 12.0 - pw - 8.0).max(0.0);
        let name = fit_text(g, &p.name, avail, 11.0, false);
        g.draw_text(
            x0 + 12.0 + pw + 8.0,
            y + 5.0,
            &name,
            gpu::DrawOpts { font_size: 11.0, color: theme::text_mute(), bold: false, italic: false },
        );
    }
}

/// 프로세스 우클릭 메뉴. 칼럼 안에 가두는 건 이 칼럼이 마지막으로 그려지는
/// 레이어가 아니어서다 — 밖으로 삐져나가면 뒤에 그려질 pane 헤더가 덮는다.
fn draw_proc_menu(
    g: &mut gpu::GpuRenderer,
    cursor: (f32, f32),
    info: &mut state::InfoState,
    x: f32,
    w: f32,
    top: f32,
    bottom: f32,
) {
    info.ctx_menu_rects.clear();
    let Some((rawx, rawy, _pid)) = info.ctx_menu else { return };
    use state::InfoMenuAction as A;
    let items: [(A, &str, bool, bool); 4] = [
        (A::Terminate, "종료 (SIGTERM)", false, false),
        (A::ForceKill, "강제 종료 (SIGKILL)", true, false),
        (A::CopyPid, "PID 복사", false, true),
        (A::CopyCmd, "명령 복사", false, false),
    ];
    let mih = 28.0_f32;
    let sep = 7.0_f32;
    let pad = 6.0_f32;
    let widest = items
        .iter()
        .map(|(_, l, _, _)| g.measure_chrome_text(l, 13.0, false))
        .fold(0.0_f32, f32::max);
    let menu_w = (widest + 32.0).min(w - 8.0);
    let nsep = items.iter().filter(|(_, _, _, s)| *s).count() as f32;
    let menu_h = pad * 2.0 + items.len() as f32 * mih + nsep * sep;
    let mx = rawx.min(x + w - menu_w - 4.0).max(x + 4.0);
    let my = rawy.min(bottom - menu_h - 4.0).max(top);
    round_rect(g, mx, my, menu_w, menu_h, theme::RADIUS_MD, theme::surface());
    let bc = theme::with_alpha(theme::border(), 0xCC);
    g.rect(mx, my, menu_w, 1.0, bc);
    g.rect(mx, my + menu_h - 1.0, menu_w, 1.0, bc);
    g.rect(mx, my, 1.0, menu_h, bc);
    g.rect(mx + menu_w - 1.0, my, 1.0, menu_h, bc);
    let mut iy = my + pad;
    for (action, label, danger, sep_before) in items {
        if sep_before {
            g.rect(mx + pad, iy + sep * 0.5, menu_w - pad * 2.0, 1.0, theme::with_alpha(theme::border(), 0x88));
            iy += sep;
        }
        let r = (mx + 4.0, iy, menu_w - 8.0, mih);
        if hit(cursor, &r) {
            round_rect(g, r.0, r.1, r.2, r.3, theme::RADIUS_SM, theme::surface_hover());
        }
        g.draw_text(
            r.0 + 12.0,
            r.1 + (mih - 13.0) / 2.0,
            label,
            gpu::DrawOpts {
                font_size: 13.0,
                color: if danger { theme::danger() } else { theme::text() },
                bold: false,
                italic: false,
            },
        );
        info.ctx_menu_rects.push((action, r));
        iy += mih;
    }
}

/// `Visual Studio Code` 처럼 긴 앱 이름을 버튼에 들어갈 길이로. 목록에 없는
/// 이름은 그대로 두고, 안 들어가면 호출부가 아이콘만 그린다.
fn short_app_name(name: &str) -> &str {
    match name {
        "Visual Studio Code" => "VS Code",
        "IntelliJ IDEA" => "IntelliJ",
        "Sublime Text" => "Sublime",
        other => other,
    }
}

/// 경로를 최대 `max_lines` 줄로 접는다. 넘치면 **앞을** 버리고 "…" 를 붙인다 —
/// 경로에서 알아야 하는 건 프로젝트 이름이 있는 꼬리 쪽이다.
fn wrap_path(g: &mut gpu::GpuRenderer, s: &str, avail: f32, max_lines: usize) -> Vec<String> {
    if avail <= 0.0 {
        return Vec::new();
    }
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for ch in s.chars() {
        cur.push(ch);
        if g.measure_chrome_text(&cur, 11.0, false) > avail {
            cur.pop();
            if cur.is_empty() {
                break; // 한 글자도 안 들어가는 폭 — 그릴 게 없다.
            }
            lines.push(std::mem::take(&mut cur));
            cur.push(ch);
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.len() > max_lines {
        let tail = lines.split_off(lines.len() - max_lines);
        lines = tail;
        lines[0] = format!("…{}", lines[0]);
    }
    lines
}

/// 주어진 폭에 들어가도록 꼬리를 자르고 말줄임을 붙인다. 폭이 아예 부족하면 빈
/// 문자열 — 잘린 한 글자만 남는 것보다 아무것도 없는 편이 읽기 낫다.
fn fit_text(g: &mut gpu::GpuRenderer, s: &str, avail: f32, size: f32, bold: bool) -> String {
    if g.measure_chrome_text(s, size, bold) <= avail {
        return s.to_string();
    }
    let ell = g.measure_chrome_text("…", size, bold);
    if avail <= ell {
        return String::new();
    }
    // char 경계로만 자른다 — 바이트로 자르면 한글/이모지에서 패닉한다.
    let mut cut = 0;
    for (i, _) in s.char_indices() {
        if g.measure_chrome_text(&s[..i], size, bold) + ell > avail {
            break;
        }
        cut = i;
    }
    format!("{}…", &s[..cut])
}
