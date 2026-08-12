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

/// `KASATERM_PROFILE` 이 켜졌는지. 렌더 루프가 매 프레임 묻기 때문에 환경변수
/// 조회 자체를 한 번으로 접는다.
pub(crate) fn profiling() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("KASATERM_PROFILE").is_some())
}

/// 행이 무엇인지. argv[0] 의 파일명만으로는 claude 아래가 전부 `npm`·`node`·
/// `Python` 세 단어로 뭉개져 계보만 보이고 정체가 안 보였다(거노: "클로드 밑으로
/// 초록점밖에 안 보인다"). 종류를 먼저 판정해 이름·색·묶음 규칙을 가른다.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ProcKind {
    #[default]
    Plain,
    /// CLI claude 세션 자신.
    Claude,
    /// codex 세션 자신. claude 와 같은 대접(강조색·요약 행)을 받는다 —
    /// 프로세스 트리에서 둘을 다르게 그리면 학생 pane 이 종류에 따라 딴판이 된다.
    Codex,
    /// claude 가 stdio 로 띄운 MCP 서버.
    Mcp,
    /// claude 의 Bash 도구가 띄운 셸(과 그 자손).
    Tool,
}

/// Info 목록의 한 행. `depth` 는 셸 바로 아래 자식이 0 이고, 렌더가 들여쓰기에
/// 쓴다. 셸 자신은 목록이 아니라 pane 그룹 머리에 따로 뜬다.
#[derive(Clone, Default, PartialEq)]
pub(crate) struct ProcRow {
    pub(crate) pid: u32,
    pub(crate) depth: u8,
    /// 표시용 이름. 런처(`npm`/`node`/`python`)면 argv 에서 캐낸 정체로 바뀐다.
    pub(crate) name: String,
    /// 부제로 흐리게 붙일 나머지 — 표시 이름이 이미 말해주는 토큰은 빠진다.
    pub(crate) rest: String,
    /// `ps` 가 보고한 CPU 점유율(%).
    pub(crate) cpu: f32,
    /// resident set size(KB).
    pub(crate) mem_kb: u64,
    pub(crate) kind: ProcKind,
    /// 계보선용 — 조상 depth `d` 에 아직 뒤따를 형제가 있으면 비트 `d` 가 1.
    /// 렌더는 이 비트가 선 세로줄만 그린다(└ 뒤로 선이 이어지면 거짓말이 된다).
    pub(crate) spine: u32,
    /// 형제 중 마지막이면 `└`, 아니면 `├`.
    pub(crate) last: bool,
    /// 이 행이 흡수한 래퍼 프로세스 수. `npm exec X` → `node …/X` 는 사람에겐
    /// 한 덩어리라 접는데, 접었다는 사실 자체는 pid 개수로 남겨둔다.
    pub(crate) folded: u8,
    /// 이 프로세스가 listen 중인 포트 — 행에 칩으로 붙는다.
    pub(crate) ports: Vec<u16>,
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
    /// 어느 pane 의 셸 자손도 **아니고** 작업 폴더가 같아서 딸려온 것. 띄운 셸이
    /// 죽어 launchd 밑으로 넘어간 dev 서버가 대부분이라, pane 이 지금 돌리는
    /// 것처럼 보이면 안 된다(끄려고 pane 을 닫아도 안 죽는다).
    ///
    /// **"주인을 모른다"는 뜻이 아니다.** 이 값이 참일 때도 `pane`·`label` 은 이미
    /// 채워져 있다 — 작업 폴더로 되짚어 찾았으니까. 예전엔 이걸 "(고아)" 로 적었는데,
    /// 학생이 백그라운드로 띄운 dev 서버가 전부 그렇게 표시돼 누가 띄웠는지 아는
    /// 서버까지 주인 없는 것처럼 읽혔다(거노). 지금은 점 색으로만 구분한다.
    pub(crate) orphan: bool,
    /// 이 포트를 쥔 프로세스가 속한 pane(`%17`). 여러 pane 이 한 목록을 공유하니
    /// 소유자를 안 밝히면 "3000 이 누구 건지" 를 결국 사람이 추적해야 한다.
    pub(crate) pane: Option<String>,
    /// 그 pane 에 배정된 학생 이름. pane id(`%17`)는 기계의 이름이라 사람이 못
    /// 외운다 — 얼굴과 이름이 있어야 "코하루가 띄운 3000" 으로 읽힌다.
    pub(crate) label: String,
    /// 무엇이 떠 있는지 — 프로젝트 폴더명, 알려진 서비스명, 또는 응답한 HTML 의
    /// `<title>`. 포트 번호만으로는 며칠 전 띄워둔 서버의 정체를 알 수 없다.
    pub(crate) site: String,
    /// 띄운 pane 이 **이미 없다**. 이때만 "꺼도 되나" 에 답할 수 있다.
    ///
    /// `orphan` 과 다르다 — 그건 "셸 자손이 아니다"(재부모화됐다)일 뿐이고, 주인이
    /// 살아 있어도 참이다. 이 값은 주인 자체가 사라졌다는 뜻이라 끄는 판단의 근거가
    /// 된다. 가릴 수 있게 된 것은 귀속을 작업 폴더가 아니라 프로세스 env 로 하기
    /// 때문이다(`panes_of`).
    pub(crate) owner_dead: bool,
}

/// 한 pane 과 그 셸 아래 프로세스들. pane 을 묶음으로 두는 건 목록이 전 pane
/// 공유로 바뀌었기 때문이다 — 평면으로 늘어놓으면 어느 pane 것인지가 행마다
/// 반복돼 정작 계보가 안 읽힌다.
#[derive(Clone, Default, PartialEq)]
pub(crate) struct PaneGroup {
    /// surface id(`%17`).
    pub(crate) pane: String,
    /// 학생 이름. 없으면 빈 문자열이고 렌더가 셸 이름으로 대신한다.
    pub(crate) label: String,
    /// 이 pane 의 claude 세션 제목 — `/rename` 이름이 있으면 그것, 없으면
    /// aiTitle(요약). 학생 이름은 "누가"고 이건 "무엇을" 이라 둘 다 필요하다.
    pub(crate) session: String,
    pub(crate) shell: String,
    pub(crate) shell_pid: u32,
    pub(crate) active: bool,
    /// 방(윈도우) 인덱스와 이름 — 방이 둘 이상일 때만 머리로 그린다.
    pub(crate) window: usize,
    pub(crate) window_label: String,
    /// 이 방이 별도 창으로 나가 있나. 나가 있으면 그 pane 들은 메인 화면에 없으므로,
    /// 표시가 없으면 "왜 여기 있는데 안 보이지"가 된다.
    pub(crate) undocked: bool,
    pub(crate) rows: Vec<ProcRow>,
}

/// 한 번의 수집 결과.
#[derive(Clone, Default, PartialEq)]
pub(crate) struct InfoSnap {
    pub(crate) panes: Vec<PaneGroup>,
    pub(crate) ports: Vec<PortRow>,
}

/// 수집할 pane 하나. GUI 스레드가 채워 워커로 넘긴다 — 워커는 `App` 을 못 보고,
/// GUI 는 `ps`/`lsof`/`git` 을 돌리면 안 되니 경계가 여기다.
#[derive(Clone, Default)]
pub(crate) struct PaneTarget {
    pub(crate) id: String,
    pub(crate) shell_pid: u32,
    pub(crate) label: String,
    pub(crate) cwd: Option<std::path::PathBuf>,
    pub(crate) active: bool,
    pub(crate) window: usize,
    pub(crate) window_label: String,
    /// 이 방이 별도 창으로 나가 있나. 나가 있으면 그 pane 들은 메인 화면에 없으므로,
    /// 표시가 없으면 "왜 여기 있는데 안 보이지"가 된다.
    pub(crate) undocked: bool,
    /// 이 pane 이 붙든 claude transcript. 제목을 뽑으려면 jsonl 꼬리를 읽어야
    /// 해서 **경로만** GUI 가 넘기고 읽기는 워커가 한다.
    pub(crate) session_path: Option<std::path::PathBuf>,
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
pub(crate) fn collect(targets: &[PaneTarget], sites: &SiteCache) -> InfoSnap {
    let table = process_snapshot();
    if table.is_empty() {
        return InfoSnap::default();
    }
    let by_pid: HashMap<u32, &Raw> = table.iter().map(|r| (r.pid, r)).collect();
    let mut panes: Vec<PaneGroup> = targets
        .iter()
        .map(|t| PaneGroup {
            pane: t.id.clone(),
            label: t.label.clone(),
            session: t.session_path.as_deref().map(session_title).unwrap_or_default(),
            shell: by_pid
                .get(&t.shell_pid)
                .map(|r| split_argv(&r.args).0)
                // 로그인 셸의 argv[0] 은 `-zsh` 처럼 하이픈이 붙는다 — 표시용이라 뗀다.
                .map(|n| n.trim_start_matches('-').to_string())
                .unwrap_or_default(),
            shell_pid: t.shell_pid,
            active: t.active,
            window: t.window,
            window_label: t.window_label.clone(),
            undocked: t.undocked,
            rows: build_rows(&table, t.shell_pid),
        })
        .collect();
    // 방이 먼저, 그 안에서 pane 번호순. 방을 1차 키로 두어야 같은 방의 pane 이
    // 붙어 서고 방 머리를 한 번만 그릴 수 있다. 정렬 기준을 고정하지 않으면
    // HashMap 순회 순서 때문에 목록이 수집할 때마다 자리를 바꾼다.
    //
    // **정렬 키에 "지금 활성" 을 넣지 마라.** 예전엔 활성 방과 활성 pane 을 각각
    // 맨 앞으로 끌어올렸는데("보고 있는 화면이 위에 있어야 스크롤 없이 읽힌다"),
    // 그러면 pane 을 옮길 때마다 목록이 통째로 재배치돼 **누르려던 행이 손가락
    // 밑에서 달아난다**(거노: "뭐 생길 때마다 왔다갔다 돼서 원하는 거 클릭 못 할
    // 때도 있어"). 자리는 고정해 두고 활성은 색으로만 알린다 — 목록은 위치가
    // 기억되는 지도여야지 매번 다시 읽어야 하는 피드가 아니다.
    panes.sort_by_key(|g| (g.window, pane_ord(&g.pane), g.pane.clone()));

    // pid → 소유 pane. 포트를 쥔 프로세스를 pane 으로 되짚는 역인덱스다.
    let mut owner: HashMap<u32, String> = HashMap::new();
    for g in &panes {
        owner.insert(g.shell_pid, g.pane.clone());
        for r in &g.rows {
            owner.insert(r.pid, g.pane.clone());
        }
    }
    // 셸 자손만 보면 **정작 찾는 서버를 놓친다**. `npm run dev` 를 띄운 셸이
    // 끝나면 서버는 launchd(ppid 1) 밑으로 넘어가 트리에서 사라지는데, 포트는
    // 그대로 물고 있다(실측: dev 서버 넷 전부 ppid 1). 거노가 "포트 열려 있는데
    // info 가 못 잡는다" 고 한 게 이것 — 그래서 전체 listen 을 훑은 뒤,
    // **작업 폴더가 어느 pane 의 레포 안**인 것까지 끌어온다. 폴더로 거르니
    // ControlCenter·Adobe 같은 시스템 포트는 안 딸려온다.
    let all = listening_ports();
    let mut port_pids: Vec<u32> = all.iter().map(|(_, pid)| *pid).collect();
    port_pids.sort_unstable();
    port_pids.dedup();
    // cwd 는 소유 여부와 무관하게 전부 받는다 — 귀속 판정에도, "무슨 사이트인지"
    // 라벨에도 같은 값을 쓰므로 lsof 를 두 번 부를 이유가 없다.
    let cwds = cwds_of(&port_pids);
    // 포트를 쥔 프로세스가 어느 pane 에서 났는지는 env 로만 정확히 알 수 있다 —
    // 부모 체인은 서버가 launchd 밑으로 넘어가는 순간 끊긴다(위 주석의 그 실측).
    let env_panes = panes_of(&port_pids);
    let roots: Vec<(String, std::path::PathBuf)> = targets
        .iter()
        .filter_map(|t| {
            let cwd = t.cwd.as_deref()?;
            // 레포일 때만 넓힌다 — `~/Desktop` 처럼 레포가 아닌 폴더를 앵커로
            // 쓰면 그 아래 모든 프로젝트의 서버가 딸려온다(실측 15개).
            let root = crate::session::git_repo_root(cwd)?;
            Some((t.id.clone(), root))
        })
        .collect();
    let mut ports: Vec<PortRow> = all
        .into_iter()
        .filter_map(|(port, pid)| {
            let (pane, orphan, owner_dead) = match owner.get(&pid) {
                Some(p) => (Some(p.clone()), false, false),
                // env 는 재부모화돼도 남으므로 **띄운 pane 을 정확히 가리킨다**. 작업
                // 폴더 추정보다 먼저 보는 이유는, 폴더로는 같은 레포에 pane 이 여럿일
                // 때 못 가르고 **죽은 pane 이 띄운 서버가 살아 있는 pane 것으로 붙기**
                // 때문이다 — 아래 폴백이 `roots`(살아 있는 pane 의 레포)에서 찾으므로
                // 주인이 죽었다는 사실 자체가 사라진다.
                None => match env_panes.get(&pid) {
                    Some(p) => {
                        let alive = targets.iter().any(|t| &t.id == p);
                        (Some(p.clone()), true, !alive)
                    }
                    None => {
                        let cwd = cwds.get(&pid)?;
                        let pane = roots
                            .iter()
                            .find(|(_, r)| cwd.starts_with(r))
                            .map(|(id, _)| id.clone())?;
                        (Some(pane), true, false)
                    }
                },
            };
            Some(PortRow {
                port,
                pid,
                name: by_pid
                    .get(&pid)
                    .map(|r| classify(&r.args, ProcKind::Plain).0)
                    .unwrap_or_default(),
                orphan,
                label: pane
                    .as_deref()
                    .and_then(|id| panes.iter().find(|g| g.pane == id))
                    .map(|g| g.label.clone())
                    .unwrap_or_default(),
                pane,
                site: site_label(port, cwds.get(&pid).map(|p| p.as_path()), sites),
                owner_dead,
            })
        })
        .collect();
    // 포트를 쥔 프로세스는 트리에서도 그렇게 보여야 한다 — 목록을 오가며 pid 를
    // 대조하지 않고 행에서 바로 읽히게 칩을 붙인다.
    let held: HashMap<u32, Vec<u16>> = ports.iter().fold(HashMap::new(), |mut m, p| {
        m.entry(p.pid).or_default().push(p.port);
        m
    });
    for g in &mut panes {
        for r in &mut g.rows {
            if let Some(ps) = held.get(&r.pid) {
                r.ports = ps.clone();
            }
        }
    }
    ports.sort_by_key(|p| (p.port, p.pid));
    // 제목은 이번 스냅샷엔 못 싣는다(물어보는 데 시간이 걸린다) — 캐시에 쌓아
    // 다음 갱신부터 붙인다.
    probe_sites(&ports.iter().map(|p| (p.port, p.pid)).collect::<Vec<_>>(), sites);
    InfoSnap { panes, ports }
}

/// `%17` → 17. pane 목록을 사람이 세는 순서로 정렬하려고 숫자만 뽑는다 —
/// 문자열 정렬은 `%10` 을 `%2` 앞에 둔다.
fn pane_ord(id: &str) -> u32 {
    id.trim_start_matches('%').parse().unwrap_or(u32::MAX)
}

/// transcript 에서 뽑은 세션 제목 — `/rename` 으로 붙인 이름이 있으면 그것,
/// 없으면 aiTitle(요약) > 첫 user 프롬프트. claude `/resume` 피커와 같은 규칙이라
/// 목록에서 보던 이름이 여기서도 그대로 보인다.
///
/// jsonl 꼬리를 읽는 일이라 **워커에서만** 부른다. 파일 크기가 그대로면 다시
/// 읽지 않는다 — transcript 는 append 로만 자라므로 크기가 곧 세대 번호다.
fn session_title(path: &std::path::Path) -> String {
    type Cache = HashMap<std::path::PathBuf, (u64, String)>;
    static CACHE: std::sync::LazyLock<std::sync::Mutex<Cache>> =
        std::sync::LazyLock::new(Default::default);
    let Ok(len) = std::fs::metadata(path).map(|m| m.len()) else {
        return String::new();
    };
    if let Ok(g) = CACHE.lock() {
        if let Some((seen, title)) = g.get(path) {
            if *seen == len {
                return title.clone();
            }
        }
    }
    let title = kasa_socket::sessions::session_label_for(path).unwrap_or_default();
    if let Ok(mut g) = CACHE.lock() {
        g.insert(path.to_path_buf(), (len, title.clone()));
    }
    title
}

/// `(포트, pid)` → 그 서버가 응답한 제목. 키에 pid 를 넣는 건 같은 포트를 다른
/// 프로세스가 물려받으면 옛 제목이 거짓이 되기 때문이다. 값이 빈 문자열이면
/// "물어봤지만 답이 없었다" — 키가 있다는 사실 자체가 재시도를 막는다.
pub(crate) type SiteCache = std::sync::Arc<std::sync::Mutex<HashMap<(u16, u32), String>>>;

/// 포트 번호만 보고는 며칠 전 띄워둔 서버가 뭔지 알 수 없다. 알아낼 수 있는
/// 것을 싼 순서로 붙인다: 표준 서비스 → 작업 폴더 이름 → 서버가 응답한 제목.
fn site_label(port: u16, cwd: Option<&std::path::Path>, sites: &SiteCache) -> String {
    if let Some(known) = well_known(port) {
        return known.to_string();
    }
    let folder = cwd
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let title = sites
        .lock()
        .ok()
        .and_then(|m| m.iter().find(|((p, _), _)| *p == port).map(|(_, t)| t.clone()))
        .unwrap_or_default();
    match (folder.is_empty(), title.is_empty()) {
        (false, false) => format!("{folder} · {title}"),
        (false, true) => folder.to_string(),
        (true, false) => title,
        (true, true) => String::new(),
    }
}

/// 표준 포트. 이름이 이미 있는 포트는 HTTP 로 찔러볼 이유도 없어서, 이 표는
/// 라벨 겸 프로브 제외 목록으로 함께 쓰인다(DB 소켓에 GET 을 쏘지 않는다).
fn well_known(port: u16) -> Option<&'static str> {
    Some(match port {
        22 => "ssh",
        25 | 465 | 587 => "smtp",
        53 => "dns",
        88 => "kerberos",
        111 => "rpcbind",
        139 | 445 => "smb",
        631 => "cups",
        993 | 995 => "imap/pop",
        1433 => "sql server",
        2049 => "nfs",
        3306 => "mysql",
        5000 | 7000 => "airplay",
        5432 => "postgres",
        5672 => "rabbitmq",
        5900 => "vnc",
        6379 => "redis",
        9092 => "kafka",
        9222 => "chrome devtools",
        11211 => "memcached",
        27017 => "mongodb",
        _ => return None,
    })
}

/// 아직 안 물어본 포트에 한 번씩 HTTP 로 제목을 물어본다. 워커 스레드에서
/// 부르되 수집을 막지 않도록 따로 띄운다 — 응답 없는 소켓 하나가 목록 전체를
/// 세워선 안 된다. 표준 서비스 포트는 건드리지 않는다.
fn probe_sites(ports: &[(u16, u32)], sites: &SiteCache) {
    let todo: Vec<(u16, u32)> = {
        let Ok(seen) = sites.lock() else { return };
        ports
            .iter()
            .copied()
            .filter(|k| well_known(k.0).is_none() && !seen.contains_key(k))
            .collect()
    };
    if todo.is_empty() {
        return;
    }
    let sites = sites.clone();
    std::thread::spawn(move || {
        for key in todo {
            let title = http_title(key.0).unwrap_or_default();
            if let Ok(mut m) = sites.lock() {
                m.insert(key, title);
            }
        }
    });
}

/// `http://127.0.0.1:<port>/` 의 `<title>`. 타임아웃을 짧게 잡는 건 응답하지
/// 않는 소켓(비-HTTP 서버)이 흔하기 때문이고, 앞부분만 읽는 건 제목이 head 에
/// 있어서다 — 본문을 다 받을 이유가 없다.
fn http_title(port: u16) -> Option<String> {
    use std::io::{Read, Write};
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let timeout = std::time::Duration::from_millis(400);
    let mut s = std::net::TcpStream::connect_timeout(&addr, timeout).ok()?;
    s.set_read_timeout(Some(timeout)).ok()?;
    s.set_write_timeout(Some(timeout)).ok()?;
    s.write_all(
        b"GET / HTTP/1.0\r\nHost: localhost\r\nUser-Agent: kasaterm-info\r\nAccept: text/html\r\nConnection: close\r\n\r\n",
    )
    .ok()?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    while buf.len() < 16 * 1024 {
        match s.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let low = text.to_ascii_lowercase();
    let start = low.find("<title")?;
    let open = low[start..].find('>')? + start + 1;
    let end = low[open..].find("</title>")? + open;
    let title = text[open..end].split_whitespace().collect::<Vec<_>>().join(" ");
    // 40자를 넘는 제목은 좁은 칼럼에서 어차피 잘리고, 그 앞부분이 대개 서비스
    // 이름이다.
    let title: String = title.chars().take(40).collect();
    (!title.trim().is_empty()).then(|| title.trim().to_string())
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
    let live = |pid: &u32| by_pid.get(pid).is_some_and(|r| !r.zombie);
    let mut out = Vec::new();
    // 명시적 스택 DFS. 재귀를 피하는 건 깊이 때문이 아니라, ppid 가 순환하는
    // 이상 상태(부모가 죽고 pid 가 재사용된 찰나)에서도 멈추게 하려는 것 —
    // `seen` 이 같은 pid 를 두 번 펼치지 않는다.
    let mut seen = std::collections::HashSet::new();
    // 셸은 -1 로 시작한다 — 그래야 첫 자식이 0(들여쓰기 없음)이 되어, 머리로
    // 빠진 셸 자리만큼 목록 전체가 왼쪽으로 붙는다.
    let mut stack = vec![(shell_pid, -1i16, 0u32, true, ProcKind::Plain)];
    while let Some((pid, depth, spine, last, parent_kind)) = stack.pop() {
        if !seen.insert(pid) {
            continue;
        }
        let mut kind = parent_kind;
        let mut kids: Vec<u32> = children.get(&pid).cloned().unwrap_or_default();
        // depth < 0 인 건 셸 자신뿐 — 그룹 머리에 따로 뜨므로 목록에선 뺀다.
        // 좀비도 행을 안 만들지만 자리(depth)와 종류는 물려줘, 좀비를 건너뛴
        // 손자가 형제와 같은 단으로 보이는 거짓 계보를 막는다.
        if depth >= 0 {
            if let Some(raw) = by_pid.get(&pid).filter(|r| !r.zombie) {
                let (name, mut rest, k) = classify(&raw.args, parent_kind);
                kind = k;
                // 래퍼 접기 — `npm exec X` 와 그것이 exec 한 `node …/X` 는 사람에겐
                // 한 프로세스다. 남기는 쪽을 **부모(래퍼)** 로 잡은 건 종료가
                // 거기서만 통째로 먹히기 때문이다(자식만 죽이면 래퍼가 남는다).
                let mut folded = 0u8;
                while kind == ProcKind::Mcp && folded < u8::MAX {
                    let alive: Vec<u32> = kids.iter().copied().filter(live).collect();
                    let [only] = alive[..] else { break };
                    let Some(cr) = by_pid.get(&only) else { break };
                    let (cn, crest, ck) = classify(&cr.args, ProcKind::Mcp);
                    if ck != ProcKind::Mcp || cn != name {
                        break;
                    }
                    // 옵션은 대개 래퍼가 아니라 실체 쪽이 더 정확히 들고 있다.
                    if rest.is_empty() {
                        rest = crest;
                    }
                    folded += 1;
                    seen.insert(only);
                    kids = children.get(&only).cloned().unwrap_or_default();
                }
                out.push(ProcRow {
                    pid,
                    depth: depth.min(u8::MAX as i16) as u8,
                    name,
                    rest,
                    cpu: raw.cpu,
                    mem_kb: raw.rss_kb,
                    kind,
                    spine,
                    last,
                    folded,
                    ports: Vec::new(),
                });
            }
        }
        // 내가 마지막이 아니면 내 열에 세로선이 계속 내려가야 자식들의 계보가
        // 이어져 보인다. 마지막(└)이면 그 아래로 선을 끊는다.
        let next_spine = match depth {
            d if d >= 0 && !last => spine | 1u32 << (d as u32).min(31),
            _ => spine,
        };
        let alive: Vec<u32> = kids.iter().copied().filter(live).collect();
        // 좀비는 목록에 안 나오지만 자기 자식을 잇는 통로라 따로 태운다.
        for &z in kids.iter().filter(|k| !live(k)) {
            stack.push((z, depth.saturating_add(1), next_spine, true, kind));
        }
        // pop 이 역순으로 꺼내니 뒤집어 넣어야 pid 오름차순으로 나온다.
        for (i, &k) in alive.iter().enumerate().rev() {
            stack.push((k, depth.saturating_add(1), next_spine, i + 1 == alive.len(), kind));
        }
    }
    out
}

/// 한 토큰이 MCP 서버를 가리키면 그 서버 이름. 패키지·경로·스크립트 어느
/// 모양으로 와도 사람이 부르는 한 단어로 줄인다:
///
/// - `exa-mcp-server` → `exa`
/// - `@upstash/context7-mcp` → `context7`
/// - `@playwright/mcp@latest` → `playwright` (패키지명이 순수 `mcp` 면 스코프가 곧 이름)
/// - `…/node_modules/.bin/playwright-mcp` → `playwright`
/// - `…/slack_sentry_mcp.py` → `slack-sentry`
///
/// 접사를 떼는 순서가 곧 규칙이다 — `-mcp-server` 를 `-mcp` 보다 먼저 보지
/// 않으면 `exa-mcp-server` 가 `exa-mcp-server`→`exa-server` 로 어정쩡해진다.
fn mcp_name(tok: &str) -> Option<String> {
    let low = tok.to_ascii_lowercase();
    if !low.contains("mcp") && !low.contains("modelcontextprotocol") {
        return None;
    }
    // `@scope/pkg` 의 스코프 — 패키지 이름이 알맹이 없이 `mcp` 뿐일 때 쓴다.
    let scope = tok
        .strip_prefix('@')
        .and_then(|s| s.split('/').next())
        .filter(|s| !s.is_empty() && *s != "modelcontextprotocol")
        .map(str::to_string);
    let mut base = tok.rsplit('/').next().unwrap_or(tok).to_string();
    // `mcp@latest` 의 버전 꼬리. 스코프의 `@` 는 위 rsplit 에서 이미 떨어졌으므로
    // 여기 남은 `@` 는 버전뿐이다(선두 `@` 는 자르지 않는다).
    if let Some(i) = base.rfind('@').filter(|i| *i > 0) {
        base.truncate(i);
    }
    for ext in [".py", ".js", ".mjs", ".cjs", ".ts"] {
        if let Some(s) = base.strip_suffix(ext) {
            base = s.to_string();
            break;
        }
    }
    base = base.replace('_', "-");
    for suf in ["-mcp-server", "-mcp", "-server"] {
        if let Some(s) = base.strip_suffix(suf) {
            base = s.to_string();
            break;
        }
    }
    for pre in ["mcp-for-", "mcp-server-", "server-", "mcp-"] {
        if let Some(s) = base.strip_prefix(pre) {
            base = s.to_string();
            break;
        }
    }
    if base.is_empty() || base == "mcp" {
        base = scope.unwrap_or_default();
    }
    (!base.is_empty()).then_some(base)
}

/// argv 전체에서 MCP 서버 이름을 찾는다 — 이름이 패키지 인자에 있는 경우
/// (`npm exec @upstash/context7-mcp`)와 실행 파일 경로에 있는 경우
/// (`node …/.bin/context7-mcp`) 둘 다 같은 답이 나와야 래퍼 접기가 성립한다.
fn mcp_name_in(args: &str) -> Option<String> {
    args.split_whitespace().find_map(mcp_name)
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

/// argv 와 부모의 종류 → (표시 이름, 부제, 종류). 부모를 받는 건 MCP 를
/// claude 아래에서만 인정하기 위해서다 — 셸에서 직접 띄운 같은 패키지는 이
/// 세션의 MCP 가 아니라 그냥 npm 이다.
fn classify(args: &str, parent: ProcKind) -> (String, String, ProcKind) {
    let (name, rest) = split_argv(args);
    let (name, rest) = (cap_len(name, 64), cap_len(rest, 140));
    // claude 본체. `--settings <shim 경로>` 는 kasaterm 이 붙인 배선이라 사람이
    // 읽을 게 없다 — 지우면 `--resume <sid>` 같은 진짜 인자만 남는다.
    if name == "claude" {
        // kasaterm 이 붙인 배선(`--settings <shim 경로>`)과 uuid(`--session-id`)는
        // 사람이 읽을 게 없는데 자리는 제일 많이 먹는다. 학생 이름이 이미 어느
        // 세션인지 말해주므로 uuid 는 행에서 뺀다. `--resume` 같은 진짜 인자는 남는다.
        let rest = ["--settings", "--session-id"]
            .iter()
            .fold(rest, |acc, f| strip_flag_pair(&acc, f));
        return ("claude".to_string(), rest, ProcKind::Claude);
    }
    // codex 본체. npm shim(node …/bin/codex)과 진짜 바이너리 둘 다 여기로 접는다 —
    // 트리에 `node` 로 뜨면 사람이 그게 codex 인 줄 모른다.
    if name == "codex" || (name == "node" && rest.contains("/bin/codex")) {
        return ("codex".to_string(), rest, ProcKind::Codex);
    }
    // Bash 도구가 띄운 셸. 앞머리는 스냅샷 source + alias 정리 상수문이라 모든
    // 도구 셸이 똑같이 생겼고, 진짜 명령은 맨 끝 `eval '…'` 안에 있다.
    if args.contains("shell-snapshots/snapshot-") {
        return ("Bash 도구".to_string(), eval_payload(args), ProcKind::Tool);
    }
    // 도구 셸 아래는 전부 그 도구의 일부다.
    if parent == ProcKind::Tool {
        let (n, r) = launcher_identity(&name, &rest);
        return (n, r, ProcKind::Tool);
    }
    if matches!(parent, ProcKind::Claude | ProcKind::Codex | ProcKind::Mcp) {
        if let Some(server) = mcp_name_in(args) {
            return (format!("mcp {server}"), mcp_detail(&rest), ProcKind::Mcp);
        }
    }
    let (n, r) = launcher_identity(&name, &rest);
    (n, r, ProcKind::Plain)
}

/// 좁은 칼럼이 절대 다 보여줄 수 없는 꼬리를 수집 단계에서 자른다. 재는 비용은
/// 길이에 비례하는데 argv 는 수백 자가 예사라, 그리지도 못할 글자를 프레임마다
/// 재는 건 순수한 낭비다. 렌더의 말줄임이 그 앞에서 다시 한 번 줄인다.
fn cap_len(s: String, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => s[..i].to_string(),
        None => s,
    }
}

/// `--flag value` 한 쌍(과 `--flag=value` 한 토큰)을 지운다.
fn strip_flag_pair(rest: &str, flag: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut it = rest.split_whitespace();
    while let Some(t) = it.next() {
        if t == flag {
            it.next();
            continue;
        }
        if t.starts_with(flag) && t[flag.len()..].starts_with('=') {
            continue;
        }
        out.push(t);
    }
    out.join(" ")
}

/// 셸 `-c` 상수문 끝의 `eval '…'` 안에 든 실제 명령.
fn eval_payload(args: &str) -> String {
    let Some(i) = args.rfind("eval '") else {
        return String::new();
    };
    let tail = &args[i + "eval '".len()..];
    let body = tail.strip_suffix('\'').unwrap_or(tail);
    body.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// MCP 행의 부제 — 패키지·경로는 이름이 이미 말했으니 옵션만 남긴다.
fn mcp_detail(rest: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for t in rest.split_whitespace() {
        if t.starts_with('-') || !out.is_empty() {
            out.push(t);
        }
    }
    out.join(" ")
}

/// `node`·`Python` 처럼 이름이 "무슨 도구로 띄웠나"만 말하고 정체는 말하지 않는
/// 런처면, 인자에서 실제로 돌아가는 것을 찾아 이름으로 올린다. 이게 없으면
/// claude 아래가 전부 `npm`·`node`·`Python` 세 단어로 뭉개져 계보만 남는다.
fn launcher_identity(name: &str, rest: &str) -> (String, String) {
    const LAUNCHERS: &[&str] = &[
        "node", "npm", "npx", "bun", "deno", "python", "python3", "Python", "uv", "uvx", "ruby",
        "perl", "sh", "bash", "zsh", "env",
    ];
    if !LAUNCHERS.contains(&name) {
        return (name.to_string(), rest.to_string());
    }
    // 셸은 `-c` 뒤 한 줄이 통째로 명령이라 첫 단어가 곧 하는 일이다.
    if let Some(cmd) = rest.strip_prefix("-c ") {
        let cmd = cmd.trim().trim_start_matches(['\'', '"']);
        if let Some(head) = cmd.split_whitespace().next().filter(|h| !h.is_empty()) {
            let short = std::path::Path::new(head)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(head);
            return (short.to_string(), cmd.to_string());
        }
    }
    // 서브커맨드와 플래그를 건너뛰고 처음 나오는 실체.
    const SKIP: &[&str] = &["exec", "run", "start", "test", "tool", "--"];
    let toks: Vec<&str> = rest.split_whitespace().collect();
    for (i, t) in toks.iter().enumerate() {
        if SKIP.contains(t) || t.starts_with('-') {
            continue;
        }
        let short = std::path::Path::new(t)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(t);
        // `pkg@1.2.3` 의 버전 꼬리는 이름이 아니다.
        let short = short.split('@').next().filter(|s| !s.is_empty()).unwrap_or(short);
        // 이름으로 올린 토큰과 그 앞의 서브커맨드는 부제에서 뺀다 — 이름과 부제가
        // 같은 말을 반복하면 좁은 칼럼만 잡아먹는다.
        return (short.to_string(), toks[i + 1..].join(" "));
    }
    (name.to_string(), rest.to_string())
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
        ports.push((port, pid));
    }
    dedup_ports(ports)
}

/// pid → 작업 폴더. 셸 트리 밖의 포트를 "이 레포 것"으로 인정할지 가르는 유일한
/// 근거다. 포트를 쥔 프로세스만 물으므로 한 번의 fork 로 끝난다(실측 32ms).
/// pid → 그 프로세스가 물려받은 `KASATERM_PANE_ID`.
///
/// `ps eww` 는 환경변수까지 붙여 주므로, 셸이 죽어 부모 체인이 끊긴 뒤에도 **어느
/// pane 에서 났는지**가 남는다. 작업 폴더 추정과 달리 같은 레포의 pane 여럿을 가른다.
#[cfg(unix)]
fn panes_of(pids: &[u32]) -> HashMap<u32, String> {
    let mut out = HashMap::new();
    if pids.is_empty() {
        return out;
    }
    let list = pids.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    let Ok(o) = proc::command("ps").args(["eww", "-o", "pid=,command=", "-p", &list]).output()
    else {
        return out;
    };
    const KEY: &str = "KASATERM_PANE_ID=";
    for line in String::from_utf8_lossy(&o.stdout).lines() {
        let line = line.trim_start();
        let Some((pid_s, rest)) = line.split_once(' ') else { continue };
        let Ok(pid) = pid_s.parse::<u32>() else { continue };
        let Some(i) = rest.find(KEY) else { continue };
        let v = rest[i + KEY.len()..].split_whitespace().next().unwrap_or("");
        if !v.is_empty() {
            out.insert(pid, v.to_string());
        }
    }
    out
}

#[cfg(not(unix))]
fn panes_of(_pids: &[u32]) -> HashMap<u32, String> {
    HashMap::new()
}

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

/// Windows 엔 `lsof` 가 없어 pid 마다 PEB 를 직접 읽는다(`socket::pid_cwd`).
/// unix 처럼 한 번의 fork 로 끝나진 않지만, 묻는 대상이 포트를 쥔 프로세스뿐이라
/// 실제 호출은 한 자릿수고, 이 수집기 자체가 1.5초 스로틀된 워커 스레드에서 돈다.
/// 열 수 없는 프로세스(권한 부족·이미 종료)는 그냥 빠진다 — 전부 비우던
/// 종전 스텁보다 항상 낫다.
#[cfg(windows)]
fn cwds_of(pids: &[u32]) -> HashMap<u32, std::path::PathBuf> {
    pids.iter()
        .filter_map(|&pid| crate::socket::pid_cwd(pid).map(|cwd| (pid, cwd)))
        .collect()
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
        let t = vec![
            raw(100, 1, false, "-zsh"),
            raw(200, 100, false, "/usr/bin/node srv.js --port 3000"),
        ];
        let rows = build_rows(&t, 100);
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].pid, rows[0].depth), (200, 0));
        // 런처(`node`)가 아니라 실제로 도는 것이 이름이 된다 — 이름 자리가
        // `node` 로 채워지면 claude 아래 열 몇 줄이 죄다 같은 단어가 된다.
        assert_eq!((rows[0].name.as_str(), rows[0].rest.as_str()), ("srv.js", "--port 3000"));
    }

    #[test]
    fn mcp_package_and_path_forms_reduce_to_the_same_server_name() {
        // 래퍼와 실체가 같은 이름으로 줄어야 접기가 성립한다.
        assert_eq!(mcp_name("exa-mcp-server").as_deref(), Some("exa"));
        assert_eq!(
            mcp_name("/U/.npm/_npx/6f/node_modules/.bin/exa-mcp-server").as_deref(),
            Some("exa")
        );
        assert_eq!(mcp_name("@upstash/context7-mcp").as_deref(), Some("context7"));
        // 패키지 이름이 알맹이 없이 `mcp` 뿐이면 스코프가 곧 이름이다.
        assert_eq!(mcp_name("@playwright/mcp@latest").as_deref(), Some("playwright"));
        assert_eq!(mcp_name("/U/.npm/_npx/98/node_modules/.bin/playwright-mcp").as_deref(), Some("playwright"));
        assert_eq!(mcp_name("/U/sionic/slack-sentry/slack_sentry_mcp.py").as_deref(), Some("slack-sentry"));
        assert_eq!(mcp_name("@modelcontextprotocol/server-filesystem").as_deref(), Some("filesystem"));
        // MCP 와 무관한 토큰은 건드리지 않는다.
        assert_eq!(mcp_name("--cdp-endpoint"), None);
        assert_eq!(mcp_name("/usr/bin/node"), None);
    }

    #[test]
    fn npm_wrapper_and_its_exec_target_collapse_into_one_row() {
        let t = vec![
            raw(100, 1, false, "-zsh"),
            raw(200, 100, false, "/U/.local/bin/claude --settings /tmp/shim/hooks.json"),
            raw(300, 200, false, "npm exec @playwright/mcp@latest --cdp-endpoint http://localhost:9222"),
            raw(400, 300, false, "node /U/.npm/_npx/98/node_modules/.bin/playwright-mcp --cdp-endpoint http://localhost:9222"),
        ];
        let rows = build_rows(&t, 100);
        // claude + MCP 한 줄. `node …` 는 래퍼에 흡수된다.
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0].name.as_str(), rows[0].kind), ("claude", ProcKind::Claude));
        // shim 배선 플래그는 사람이 읽을 게 없어 부제에서 빠진다.
        assert_eq!(rows[0].rest, "");
        assert_eq!((rows[1].name.as_str(), rows[1].kind), ("mcp playwright", ProcKind::Mcp));
        // 남는 pid 는 래퍼 쪽 — 그것만 죽여야 통째로 정리된다.
        assert_eq!((rows[1].pid, rows[1].folded), (300, 1));
    }

    #[test]
    fn same_package_outside_claude_is_not_an_mcp_row() {
        let t = vec![
            raw(100, 1, false, "-zsh"),
            raw(200, 100, false, "npm exec @upstash/context7-mcp"),
        ];
        let rows = build_rows(&t, 100);
        assert_eq!(rows[0].kind, ProcKind::Plain);
        assert_eq!(rows[0].name, "context7-mcp");
    }

    #[test]
    fn bash_tool_shell_shows_the_command_not_the_snapshot_preamble() {
        let t = vec![
            raw(100, 1, false, "-zsh"),
            raw(200, 100, false, "/U/.local/bin/claude"),
            raw(
                300,
                200,
                false,
                "/bin/zsh -c source /U/.claude/shell-snapshots/snapshot-zsh-1.sh 2>/dev/null || true && eval 'cargo build --release'",
            ),
        ];
        let rows = build_rows(&t, 100);
        assert_eq!((rows[1].name.as_str(), rows[1].kind), ("Bash 도구", ProcKind::Tool));
        assert_eq!(rows[1].rest, "cargo build --release");
    }

    #[test]
    fn spine_marks_only_ancestors_that_still_have_siblings_below() {
        //  ├─ a        (200, 형제 300 이 남음)
        //  │  └─ a1    (250, 마지막)
        //  └─ b        (300, 마지막)
        let t = vec![
            raw(100, 1, false, "-zsh"),
            raw(200, 100, false, "a"),
            raw(250, 200, false, "a1"),
            raw(300, 100, false, "b"),
        ];
        let rows = build_rows(&t, 100);
        let at = |pid| rows.iter().find(|r| r.pid == pid).unwrap();
        assert!(!at(200).last);
        // a 아래 a1 은 a 의 열에 세로선이 이어져야 한다(a 뒤에 b 가 남았으므로).
        assert_eq!((at(250).spine, at(250).last), (1 << 0, true));
        // 마지막 형제 b 아래로는 선이 끊긴다.
        assert!(at(300).last);
        assert_eq!(at(300).spine, 0);
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
        use std::sync::atomic::Ordering::Relaxed;
        // 탭 판정보다 앞이다 — "열기" 앱 목록은 Info 의 버튼뿐 아니라 우클릭
        // 메뉴·설정도 쓰는데, 훑는 데 100ms 넘게 걸리는 Spotlight 질의라
        // 누구든 처음 부르는 쪽이 프레임을 통째로 잡아먹는다. 첫 프레임에
        // 백그라운드로 걸어 두면 그 뒤로는 아무도 기다리지 않는다.
        {
            let proxy = self.proxy.clone();
            crate::proc::warm_open_with_apps(move || {
                let _ = proxy.send_event(UserEvent::Redraw);
            });
        }
        if self.info.tab != state::SideTab::Info || !self.git.col_visible {
            return;
        }
        // 워커가 새 스냅샷을 올렸을 때만 렌더용 사본으로 옮긴다. 프레임마다
        // 잠그고 통째로 clone 하면 프로세스 수만큼의 String 할당이 60fps 로
        // 도는데, 정작 내용은 1.5초에 한 번 바뀐다.
        //
        // 단, **커서가 패널 위에 있는 동안은 갈아끼우지 않는다.** 프로세스가 뜨거나
        // 포트가 하나 열리면 그 아래 행이 전부 밀리는데, 하필 그 순간 누르면 엉뚱한
        // 것이 눌린다(거노: "뭐 생길 때마다 왔다갔다 돼서 원하는 거 클릭 못 할 때도
        // 있어"). 손을 치우면 그 다음 프레임에 바로 최신으로 따라잡는다 — rev 는
        // 계속 오르고 seen_rev 만 뒤처져 있으니 조건이 저절로 다시 참이 된다.
        let rev = self.info.rev.load(Relaxed);
        let hovering = self.info.panel_rect.is_some_and(|(px, py, pw, ph)| {
            let (cx, cy) = self.cursor_px;
            cx >= px && cx < px + pw && cy >= py && cy < py + ph
        });
        // 동결에 시한을 두는 건 `CursorLeft` 를 안 받기 때문이다 — 커서 좌표는 창을
        // 떠나도 마지막 자리에 남으므로, 패널 위에 마우스를 얹은 채 자리를 뜨면
        // 목록이 영영 굳는다. 시한이 지나면 한 프레임 흘려보내고 다시 언다: 손을
        // 얹고 있는 동안에도 최신을 아주 잃지는 않으면서, 누르려는 찰나에 행이
        // 밀릴 확률은 갱신 주기(1.5초)보다 훨씬 낮게 유지된다.
        let now = std::time::Instant::now();
        if hovering {
            let since = *self.info.frozen_since.get_or_insert(now);
            if now.duration_since(since) > std::time::Duration::from_secs(3) {
                self.info.frozen_since = None;
            }
        } else {
            self.info.frozen_since = None;
        }
        let frozen = hovering && self.info.frozen_since.is_some();
        if rev != self.info.seen_rev && !frozen {
            if let Ok(g) = self.info.snap.lock() {
                self.info.view = g.clone();
            }
            self.info.seen_rev = rev;
        }
        // 디렉터리 섹션은 파일트리와 같은 앵커를 보여준다 — 사이드바를 닫아둬도
        // 맞아야 하므로 file_tree.root 를 읽지 않고 여기서 직접 판정한다.
        self.info.root = self.info_root();
        if self.info.busy.load(Relaxed) {
            return;
        }
        let fresh = self
            .info
            .last_refresh
            .is_some_and(|t: Instant| t.elapsed() < std::time::Duration::from_millis(1500));
        if fresh {
            return;
        }
        // 방 이름은 여기서 한 번 세워 둔다(자체 1초 게이트라 재수집 주기보다 싸다).
        self.refresh_window_labels();
        let targets = self.info_targets();
        if targets.is_empty() {
            return;
        }
        // pane 이 열리거나 닫히면 목록의 뼈대가 달라진다 — 스크롤 위치를 그대로
        // 두면 없어진 그룹 자리를 보고 있게 된다.
        let key = targets
            .iter()
            .map(|t| format!("{}:{}", t.id, t.shell_pid))
            .collect::<Vec<_>>()
            .join(",");
        if key != self.info.key {
            self.info.key = key;
            self.info.scroll = 0.0;
        }
        self.info.last_refresh = Some(Instant::now());
        self.info.busy.store(true, Relaxed);
        let snap = self.info.snap.clone();
        let busy = self.info.busy.clone();
        let rev = self.info.rev.clone();
        let sites = self.info.sites.clone();
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let next = collect(&targets, &sites);
            let changed = match snap.lock() {
                Ok(mut g) => {
                    let differs = *g != next;
                    if differs {
                        *g = next;
                    }
                    differs
                }
                Err(_) => false,
            };
            busy.store(false, Relaxed);
            // 내용이 그대로면 깨우지 않는다. 1.5초마다 똑같은 그림을 다시 그리면
            // 이 앱이 idle 에 완전히 잠드는(ControlFlow::Wait) 이점이 사라진다.
            if changed {
                rev.fetch_add(1, Relaxed);
                let _ = proxy.send_event(UserEvent::Redraw);
            }
        });
    }

    /// 수집 대상 pane 전부. 프로세스·포트를 pane 별로 갈라 보여주려면 GUI 만 아는
    /// 것(누가 어느 학생인지, 어느 pane 이 활성인지)을 여기서 실어 보내야 한다.
    fn info_targets(&self) -> Vec<PaneTarget> {
        let Ok(ws) = self.ws.lock() else {
            return Vec::new();
        };
        let active = ws.active_pane.clone();
        let mut out: Vec<PaneTarget> = self
            .pty
            .iter()
            .filter_map(|(id, s)| {
                // 숨긴 pane 은 어느 트리에도 없어 `pane_window` 에 안 잡히는데, 폴백이
                // **활성 방**이라 치워 둔 pane 이 지금 보고 있는 방에 붙어 버린다(실측:
                // 방 1 에서 숨긴 %4 가 방 2 밑에 섰다). 치운 자리를 기억하는 곳이
                // `closed_panes.window` 이므로 그걸 먼저 본다.
                let window = ws
                    .pane_window
                    .get(id)
                    .copied()
                    .or_else(|| {
                        self.closed_panes.iter().find(|c| c.pane_id == *id).map(|c| c.window)
                    })
                    .unwrap_or(self.active_window);
                Some(PaneTarget {
                    // 셸만 도는 pane 엔 학생 이름을 안 붙인다. 배정은 spawn 때 **모든**
                    // pane 에 되지만(`assign_character_env`) 표시는 클로드가 실제로 돌
                    // 때만이다 — 테두리·타이틀바가 쓰는 조건과 같아야 한 pane 이 자리마다
                    // 다른 얼굴을 갖지 않는다. 안 걸었더니 `%1 유우카 zsh` 처럼 셸에
                    // 학생이 붙었다(거노 2026-08-07: "일반pane은 실행전에 학생배정
                    // 안되게하지않았나").
                    label: s
                        .active_agent()
                        .and_then(|_| self.display_pane_char(&ws, id))
                        .unwrap_or_default(),
                    // "pane 이 보는 경로"가 셸 cwd 보다 우선 — bg-attach 뷰 pane 은
                    // 셸이 spawn 디렉터리에 머물러 실제 프로젝트와 어긋난다.
                    cwd: self
                        .pane_view_cwd
                        .get(id)
                        .or_else(|| self.pane_cwd_cache.get(id))
                        .cloned(),
                    active: active.as_deref() == Some(id.as_str()),
                    window,
                    // `window_labels.0` 은 안 쓴다 — 그 자리는 대표 pane 의 OSC
                    // 타이틀이라 셸만 떠 있으면 방마다 똑같이 `zsh` 가 된다(실측).
                    // 방을 실제로 가르는 건 사용자가 붙인 이름, 없으면 작업 폴더다.
                    // 경로는 끝 조각만 — 좁은 칼럼에서 전체 경로는 앞부분만 남고
                    // 정작 구분되는 꼬리가 잘려 나간다.
                    window_label: self
                        .window_name_override
                        .get(&window)
                        .cloned()
                        .or_else(|| {
                            let (_, cwd) = self.window_labels.get(window)?;
                            let tail = cwd.rsplit('/').next().unwrap_or(cwd);
                            (!tail.is_empty()).then(|| tail.to_string())
                        })
                        .unwrap_or_default(),
                    undocked: self.window_is_undocked(window),
                    // 경로 해석까지만 GUI 가 한다 — 제목을 읽으려면 jsonl 꼬리를
                    // 훑어야 해서 그건 워커 몫이다(session_title).
                    session_path: self
                        .pane_claude_sid
                        .get(id)
                        .and_then(|sid| crate::socket::transcript_path_for_session(sid)),
                    id: id.clone(),
                    shell_pid: s.shell_pid()?,
                })
            })
            .collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
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
    pub(crate) fn run_info_menu_action(
        &mut self,
        action: state::InfoMenuAction,
        target: state::InfoTarget,
    ) {
        use state::InfoMenuAction as A;
        use state::InfoTarget as T;
        let pid = target.pid();
        match action {
            A::Terminate => self.kill_process(pid, false),
            A::ForceKill => self.kill_process(pid, true),
            A::CopyPid => self.copy_to_clipboard(pid.to_string(), "PID 복사됨"),
            A::CopyCmd => {
                let cmd = self
                    .info
                    .view
                    .panes
                    .iter()
                    .flat_map(|g| &g.rows)
                    .find(|r| r.pid == pid)
                    .map(|r| {
                        if r.rest.is_empty() {
                            r.name.clone()
                        } else {
                            format!("{} {}", r.name, r.rest)
                        }
                    });
                if let Some(cmd) = cmd {
                    self.copy_to_clipboard(cmd, "명령 복사됨");
                }
            }
            A::OpenPort => {
                if let T::Port(port, _) = target {
                    self.open_localhost(port);
                }
            }
            A::CopyUrl => {
                if let T::Port(port, _) = target {
                    self.copy_to_clipboard(format!("http://localhost:{port}"), "URL 복사됨");
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
    for (tab, label) in [
        (state::SideTab::Git, "Git"),
        (state::SideTab::Info, "Info"),
        (state::SideTab::Sessions, "세션"),
        (state::SideTab::Mcp, "MCP"),
    ] {
        let active = info.tab == tab;
        let tw = g.measure_chrome_text(label, 12.0, active);
        let hot = (tx - 4.0, y - 4.0, tw + 8.0, 21.0);
        let hovered = cursor.0 >= hot.0
            && cursor.0 <= hot.0 + hot.2
            && cursor.1 >= hot.1
            && cursor.1 <= hot.1 + hot.3;
        g.hover_pointer |= hovered;
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

/// 탭 머리와 본문 사이의 전역 진입점 — 계정·사용량 행, 그리고 아로나/설정 버튼.
/// 셋 다 우상단 아이콘 클러스터에 있던 것으로, 거기서는 제목·경로와 자리를 다퉜다.
///
/// 본문(`draw_info_col`)이 아니라 그 위에 있는 건 스크롤 때문이다 — 프로세스가
/// 수십이면 진입점이 화면 밖으로 밀려나는데, 이것들은 목록의 일부가 아니라 늘
/// 같은 자리에 있어야 하는 버튼이다.
///
/// 계정 드롭다운은 여기서 안 그린다. 패널 위로 떠야 하고 그리려면 계정 목록 전체가
/// 필요해서, 반환한 행 rect 를 앵커로 호출부(render.rs)가 마지막에 그린다.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_info_actions(
    g: &mut gpu::GpuRenderer,
    cursor: (f32, f32),
    info: &mut state::InfoState,
    acct_label: Option<&str>,
    usage: Option<&crate::UsageBadge>,
    menu_open: bool,
    arona_on: bool,
    x: f32,
    w: f32,
    top: f32,
) -> (f32, Option<(f32, f32, f32, f32)>) {
    info.action_rects.clear();
    let x0 = x + 14.0;
    let right = x + w - 12.0;
    let avail = (right - x0).max(0.0);
    let mut y = top + 2.0;

    // ── 계정 · 사용량 ──
    // 여기 보이는 한도가 **활성 계정의** 것이라 이름과 한 행에 둔다. 계정을 안 쓰면
    // 이름 없이 사용률만 — 안 쓰는 사람 행에 "기본" 을 얹는 건 잡음이다.
    let acct_rect = (acct_label.is_some() || usage.is_some()).then(|| {
        let h = 30.0_f32;
        let r = (x0, y, avail, h);
        let hov = menu_open || hit(cursor, &r);
        g.hover_pointer |= hov;
        // 한도가 코앞이면 행 전체가 물든다. 숫자 색만 바꾸면 11px 글자 하나가
        // 빨개질 뿐이라, 정작 알아야 할 때(작업 중 한도가 닫히는 것) 눈에 안 든다.
        let danger = usage.is_some_and(|u| u.pct >= 90.0);
        round_rect(
            g, r.0, r.1, r.2, r.3, theme::radius_sm(),
            match (danger, hov) {
                (true, true) => theme::with_alpha(theme::danger(), 0x44),
                (true, false) => theme::with_alpha(theme::danger(), 0x2A),
                (false, true) => theme::surface_hover(),
                (false, false) => theme::surface(),
            },
        );
        let f = 11.5_f32;
        let ty = y + (h - f) / 2.0 - 1.0;
        let chev = 11.0_f32;
        g.queue_icon(
            "chevron-down",
            right - 10.0 - chev,
            y + (h - chev) / 2.0,
            chev,
            if hov { theme::text() } else { theme::text_mute() },
        );
        let mut tx = x0 + 10.0;
        if let Some(l) = acct_label {
            let lw = g.measure_chrome_text(l, f, true);
            g.draw_text(tx, ty, l,
                gpu::DrawOpts { font_size: f, color: theme::text(), bold: true, italic: false });
            tx += lw + 8.0;
        }
        if let Some(u) = usage {
            // 70%↑ 주의·90%↑ 위험(웹뷰 UsagePill 과 같은 임계). 세 색 다 테마
            // 토큰이다 — 하드코딩한 청록/산호는 팔레트를 갈아도 그대로 남아,
            // 호박색 화면에서 이 숫자 하나만 딴 데서 온 것처럼 떴다.
            let col = if u.pct >= 90.0 {
                theme::danger()
            } else if u.pct >= 70.0 {
                theme::syn_number()
            } else {
                theme::success()
            };
            // 창 라벨을 숫자와 함께 — `5h 0%` 로 고정 표기하던 시절엔 실제 압박이
            // 주간 창 95% 인데도 「5h 0%」 가 떠서 "다 0퍼로 뜬다"가 됐다(거노
            // 2026-08-05). 이제 라벨은 그 숫자가 나온 창을 말한다.
            //
            // stale(upstream 막혀 재사용된 값)이면 흐리게 + `~` 를 앞에 붙인다.
            // 숨기지 않는 것은 빈칸이 "한도 여유"로 읽히기 때문이다.
            // 퍼센트 뒤에 그 창이 풀리기까지 남은 시간 — 드롭다운과 같은 표기다.
            let l = if u.stale {
                format!("~{} {:.0}%", u.label, u.pct)
            } else {
                format!("{} {:.0}%", u.label, u.pct)
            };
            let l = match crate::resets_in_label(u.resets_at) {
                Some(r) => format!("{l} · {r}"),
                None => l,
            };
            let col = if u.stale { theme::with_alpha(col, 0x99) } else { col };
            g.draw_text(tx, ty, &l,
                gpu::DrawOpts { font_size: f, color: col, bold: true, italic: false });
        }
        y += h + 6.0;
        r
    });

    // ── 전역 진입점 ──
    // 아로나는 shim OFF 면 진입점 자체가 없다(빈 웹뷰로 들어갈 길을 원천 차단).
    // 그때는 설정이 그 자리를 마저 쓴다 — 반 폭짜리 버튼 하나가 남으면 잘린 것처럼
    // 보인다.
    let mut btns: Vec<(state::InfoAction, &str, &str)> = Vec::new();
    if arona_on {
        btns.push((state::InfoAction::Arona, "sparkles", "아로나"));
    }
    btns.push((state::InfoAction::Settings, "settings-2", "설정"));
    btns.push((state::InfoAction::Feedback, "message-square-warning", "피드백"));
    let bh = 28.0_f32;
    let gap = 6.0;
    let bw = ((avail - gap * (btns.len() - 1) as f32) / btns.len() as f32).max(0.0);
    for (i, (kind, icon, label)) in btns.into_iter().enumerate() {
        let bx = x0 + i as f32 * (bw + gap);
        let hov = hit(cursor, &(bx, y, bw, bh));
        g.hover_pointer |= hov;
        panel_rect_outlined(
            g, bx, y, bw, bh, theme::radius_sm(),
            theme::raised_on(theme::panel_bg(), hov),
        );
        let col = if hov { theme::text() } else { theme::text_dim() };
        let f = 11.0_f32;
        let lw = g.measure_chrome_text(label, f, false);
        let inner = 13.0 + 5.0 + lw;
        let ix = bx + (bw - inner) / 2.0;
        g.queue_icon(icon, ix, y + (bh - 13.0) / 2.0, 13.0, col);
        g.draw_text(ix + 18.0, y + (bh - f) / 2.0 - 1.0, label,
            gpu::DrawOpts { font_size: f, color: col, bold: false, italic: false });
        info.action_rects.push((kind, (bx, y, bw, bh)));
    }
    y += bh + 10.0;
    g.rect(x0, y, avail, 1.0, theme::border());
    (y + 9.0, acct_rect)
}

const ROW_H: f32 = 22.0;
const SEC_H: f32 = 26.0;
/// 섹션 본문과 다음 섹션 머리 사이 숨. 없으면 목록 마지막 행과 다음 머리가
/// 붙어 두 섹션이 한 덩어리로 읽힌다.
const SEC_GAP: f32 = 8.0;
const HEAD_H: f32 = 30.0;
/// pane 그룹 머리.
const GROUP_H: f32 = 24.0;
/// 방(윈도우) 머리. **위쪽 `WIN_PAD` 는 앞 방과의 여백이고 나머지가 실제 머리다.**
///
/// 여백을 상수 밖에 따로 두지 않는 이유: 이 값을 높이 계산(스크롤 clamp)과 페인트가
/// **각각** 읽는데, 여백을 별도 항으로 더하면 한쪽만 고쳐져 목록이 어긋난다. 높이
/// 안에 품으면 상수 하나로 둘이 같이 움직인다.
///
/// 종전엔 20 으로 pane 머리(24)보다 낮았다 — "구획선에 이름이 붙은 것"을 노린 것인데,
/// 배경도 여백도 없어서 방 경계가 pane 경계보다 약하게 읽혔다(2026-08-11 지적).
/// 들여쓰기로 가르는 길은 여전히 안 쓴다: 좁은 칼럼에서 한 단 더 들이면 프로세스
/// 트리의 계보선이 설 자리가 없다.
const WIN_H: f32 = 32.0;
/// 방 머리 위 여백 — 앞 방의 마지막 프로세스 줄과 붙지 않게.
const WIN_PAD: f32 = 10.0;
/// 포트 행은 두 줄이다 — 번호·소유 pane 이 윗줄, "무엇인지"가 아랫줄.
const PORT_H: f32 = 32.0;
/// 계보 한 단의 가로 폭. 좁은 칼럼에서 깊이 3~4 단은 흔하므로(claude → MCP
/// 래퍼 → 실체) 한 단을 넓게 잡으면 정작 이름 자리가 사라진다.
const IND: f32 = 11.0;
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
    // 되살리기 대기 중인 pane(최근이 뒤). 되살릴 게 있을 때만 나타나는 독립
    // 섹션이다 — 프로세스 목록 꼬리에 달아 두면 목록이 길 때 통째로 묻히고,
    // 되돌릴 수 있다는 걸 알리지 않으면 ⌘⇧T 는 아는 사람만 쓰는 기능이 된다.
    closed: &[crate::ClosedPane],
    x: f32,
    w: f32,
    top: f32,
    bottom: f32,
) {
    let prof = profiling().then(Instant::now);
    // 커서가 이 안에 있는 동안은 목록을 갈아끼우지 않는다(pump_info 참고).
    info.panel_rect = Some((x, top, w, (bottom - top).max(0.0)));
    let x0 = x + 14.0;
    let right = x + w - 12.0;
    let avail = (right - x0).max(0.0);
    // 스냅샷을 잠그거나 복사하지 않고 잠시 꺼내 쓴다 — 이 함수는 매 프레임 도는데
    // `pump_info` 가 이미 갱신될 때만 사본을 만들어 뒀다. `take` 는 빈 값과
    // 맞바꾸는 것뿐이라 할당이 없고, 끝에서 그대로 돌려놓는다.
    let snap = std::mem::take(&mut info.view);
    info.port_rects.clear();
    info.port_kill_rects.clear();
    info.group_rects.clear();
    info.proc_rects.clear();
    info.closed_rects.clear();
    info.closed_kill_rects.clear();
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
    let proc_total: usize = snap.panes.iter().map(|g| g.rows.len()).sum();
    // 방이 하나뿐이면 머리를 안 그린다 — 늘 같은 이름 한 줄이 목록 맨 위를
    // 차지하면서 알려주는 게 없다.
    let show_windows = snap.panes.iter().any(|g| g.window != snap.panes[0].window);
    let procs_h = if info.procs_collapsed {
        0.0
    } else if snap.panes.is_empty() {
        EMPTY_H
    } else {
        let mut h = 0.0;
        let mut prev: Option<usize> = None;
        for gp in &snap.panes {
            if show_windows && prev != Some(gp.window) {
                h += WIN_H;
                prev = Some(gp.window);
            }
            if show_windows && info.group_collapsed.contains(&win_key(gp.window)) {
                continue;
            }
            h += GROUP_H;
            h += visible_row_count(info, gp) as f32 * ROW_H;
        }
        h
    };
    // 되살릴 게 없으면 섹션 머리조차 안 그린다 — 늘 비어 있는 섹션이 자리를
    // 차지하면 알려주는 게 없다.
    let closed_h = if closed.is_empty() {
        0.0
    } else if info.closed_collapsed {
        SEC_H + SEC_GAP
    } else {
        SEC_H + closed.len() as f32 * ROW_H + SEC_GAP
    };
    let ports_h = match (info.ports_collapsed, snap.ports.len()) {
        (true, _) => 0.0,
        (false, 0) => EMPTY_H,
        (false, n) => n as f32 * PORT_H,
    };
    let content =
        HEAD_H + SEC_H * 3.0 + SEC_GAP * 2.0 + dir_h + procs_h + closed_h + ports_h + 14.0;
    info.scroll = info.scroll.clamp(0.0, (content - (bottom - top)).max(0.0));
    let mut y = top - info.scroll;

    // ── 요약 머리 ──
    // 목록이 전 pane 공유라 "지금 무엇을 보고 있는지"가 셸 하나의 이름일 수
    // 없다. 몇 개의 pane 을 합쳐 몇 개를 세고 있는지가 그 자리를 대신한다.
    if y + HEAD_H > top && y < bottom {
        g.queue_icon("terminal", x0, y + 5.0, 14.0, theme::text_mute());
        let summary = if snap.panes.is_empty() {
            "읽는 중…".to_string()
        } else {
            format!(
                "pane {} · 프로세스 {} · 포트 {}",
                snap.panes.len(),
                proc_total,
                snap.ports.len()
            )
        };
        let s = fit_text(g, &summary, (right - x0 - 21.0 - 22.0).max(0.0), 11.5, false);
        g.draw_text(
            x0 + 21.0,
            y + 5.0,
            &s,
            gpu::DrawOpts { font_size: 11.5, color: theme::text_dim(), bold: false, italic: false },
        );
        let rr = (right - 15.0, y + 4.0, 15.0, 15.0);
        let rhov = hit(cursor, &(rr.0 - 4.0, rr.1 - 4.0, rr.2 + 8.0, rr.3 + 8.0));
        g.hover_pointer |= rhov;
        g.queue_icon(
            "rotate-cw",
            rr.0,
            rr.1,
            rr.2,
            if rhov { theme::text() } else { theme::text_mute() },
        );
        info.refresh_rect = Some((rr.0 - 4.0, rr.1 - 4.0, rr.2 + 8.0, rr.3 + 8.0));
    }
    y += HEAD_H;

    // ── 프로젝트 디렉터리 ──
    // git 레포라서 골라진 것인지 cwd 그대로인지를 배지로 밝힌다. 트리 루트가
    // 왜 여기인지 묻게 만들지 않는 게 목적이라, 배지 없이 경로만 두면 의미가
    // 반쯤 사라진다.
    let t_a = prof.map(|_| Instant::now());
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
            let editor = crate::proc::open_with_apps_ready()
                .and_then(<[_]>::first)
                .map(|(n, _)| short_app_name(n));
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
                g.hover_pointer |= hov;
                panel_rect_outlined(
                    g,
                    bx,
                    y,
                    bw,
                    BTN_H,
                    theme::radius_sm(),
                    theme::raised_on(theme::panel_bg(), hov),
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
    let t_procs = prof.map(|_| Instant::now());
    let r = draw_section(
        g, cursor, "프로세스", Some(proc_total), None, info.procs_collapsed, x, w, y, bottom, top,
    );
    info.sec_rects.push((state::InfoSection::Procs, r));
    y += SEC_H;
    if !info.procs_collapsed {
        if snap.panes.is_empty() {
            draw_empty(g, x0, y, top, bottom, "실행 중인 프로세스 없음");
            y += EMPTY_H;
        }
        let mut prev_win: Option<usize> = None;
        for gp in &snap.panes {
            if show_windows && prev_win != Some(gp.window) {
                prev_win = Some(gp.window);
                let key = win_key(gp.window);
                let shut = info.group_collapsed.contains(&key);
                if y + WIN_H > top && y < bottom {
                    let n = snap.panes.iter().filter(|o| o.window == gp.window).count();
                    draw_window_head(g, cursor, gp, shut, n, x, w, x0, right, y);
                }
                info.group_rects.push((key, (x, y, w, WIN_H)));
                y += WIN_H;
            }
            if show_windows && info.group_collapsed.contains(&win_key(gp.window)) {
                continue;
            }
            // 학생은 접힌 게 기본 — 펴 둔 것만 `pane_expanded` 에 있다.
            let collapsed = !info.pane_expanded.contains(&gp.pane);
            if y + GROUP_H > top && y < bottom {
                draw_group_head(g, cursor, gp, collapsed, x, w, x0, right, y);
            }
            info.group_rects.push((gp.pane.clone(), (x, y, w, GROUP_H)));
            y += GROUP_H;
            // 접혀 있어도 포트를 쥔 줄은 남는다(`visible_rows`). 소유값이라 아래
            // `info` 재차용과 안 부딪힌다.
            let rows = visible_rows(info, gp);
            for p in &rows {
                if y + ROW_H > top && y < bottom {
                    draw_proc_row(g, cursor, info, p, x, w, x0, right, y);
                }
                info.proc_rects.push((p.pid, (x, y, w, ROW_H)));
                y += ROW_H;
            }
        }
    }
    let d_dir = match (t_a, t_procs) {
        (Some(a), Some(b)) => (b - a).as_secs_f32() * 1000.0,
        _ => 0.0,
    };
    let d_procs = t_procs.map(|t| t.elapsed().as_secs_f32() * 1000.0).unwrap_or(0.0);
    y += SEC_GAP;

    // ── 되살리기 ── 최근 닫은 것이 위. 줄을 누르면 그것만, ⌘⇧T 는 언제나
    // 맨 위(=가장 최근) 것을 되살린다. 되살릴 게 없으면 통째로 없다.
    if !closed.is_empty() {
        let r = draw_section(
            g,
            cursor,
            "되살리기",
            Some(closed.len()),
            None,
            info.closed_collapsed,
            x,
            w,
            y,
            bottom,
            top,
        );
        info.sec_rects.push((state::InfoSection::Closed, r));
        y += SEC_H;
        if !info.closed_collapsed {
            for (i, c) in closed.iter().enumerate().rev() {
                if y + ROW_H > top && y < bottom {
                    if let Some(br) =
                        draw_closed_row(g, cursor, c, i + 1 == closed.len(), x, w, x0, right, y)
                    {
                        info.closed_kill_rects.push((i, br));
                    }
                }
                info.closed_rects.push((i, (x, y, w, ROW_H)));
                y += ROW_H;
            }
        }
        y += SEC_GAP;
    }

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
            if y + PORT_H > top && y < bottom {
                draw_port_row(g, cursor, info, p, x, w, x0, right, y);
            }
            info.port_rects.push((p.port, p.pid, (x, y, w, PORT_H)));
            y += PORT_H;
        }
    }

    draw_row_menu(g, cursor, info, x, w, top, bottom);
    info.view = snap;
    if let Some(t) = prof {
        eprintln!(
            "[profile] info_col {:.2}ms (head {:.2} dir {d_dir:.2} procs {d_procs:.2}) procs={proc_total} ports={}",
            t.elapsed().as_secs_f32() * 1000.0,
            (t_a.map_or(t, |p| p) - t).as_secs_f32() * 1000.0,
            info.view.ports.len()
        );
    }
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
    g.hover_pointer |= hov;
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
        pill_rect(g, right - tw - 10.0, y + 5.0, tw + 10.0, 16.0, theme::surface());
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

/// 이 학생 그룹에서 지금 보일 프로세스 행 수. 높이 계산과 그리기가 같은 판정을
/// 봐야 목록이 제 높이만큼만 스크롤된다.
fn visible_row_count(info: &state::InfoState, gp: &PaneGroup) -> usize {
    if info.pane_expanded.contains(&gp.pane) {
        gp.rows.len()
    } else {
        gp.rows.iter().filter(|r| !r.ports.is_empty()).count()
    }
}

/// 그 행들의 실제 목록. 접었으면 **포트를 쥔 줄만** 남는다 — 접는 건 목록을 줄이려는
/// 것이지 서버가 떠 있다는 사실까지 감추려는 게 아니다.
///
/// 남은 줄은 계보선을 다시 매긴다. 원래 `depth`/`spine` 은 프로세스 나무에서의
/// 자리라, 중간 가지만 뽑아 두면 부모 없는 선이 허공에서 시작한다.
fn visible_rows(info: &state::InfoState, gp: &PaneGroup) -> Vec<ProcRow> {
    if info.pane_expanded.contains(&gp.pane) {
        return gp.rows.clone();
    }
    let mut shown: Vec<ProcRow> =
        gp.rows.iter().filter(|r| !r.ports.is_empty()).cloned().collect();
    let n = shown.len();
    for (i, r) in shown.iter_mut().enumerate() {
        r.depth = 0;
        r.spine = 0;
        r.last = i + 1 == n;
    }
    shown
}

/// 방의 접힘 열쇠. pane id(`%17`)와 절대 겹치지 않는 접두사를 쓴다 — 클릭 히트
/// 목록(`group_rects`)이 방 머리와 학생 머리를 한 벌로 담아, 열쇠만 보고 어느
/// 쪽인지 갈라야 하기 때문이다(기본값이 서로 반대라 집합도 갈라져 있다).
fn win_key(idx: usize) -> String {
    format!("win:{idx}")
}

/// 방(윈도우) 머리 — 이름 붙은 구획선. pane 머리를 들여쓰지 않고 이 줄로만
/// 나누는 건, 좁은 칼럼에서 한 단계를 더 들여쓰면 정작 프로세스 트리의
/// 계보선이 설 자리가 없어지기 때문이다.
#[allow(clippy::too_many_arguments)]
fn draw_window_head(
    g: &mut gpu::GpuRenderer,
    cursor: (f32, f32),
    gp: &PaneGroup,
    collapsed: bool,
    panes: usize,
    x: f32,
    w: f32,
    x0: f32,
    right: f32,
    y: f32,
) {
    // 위 `WIN_PAD` 는 앞 방과의 여백이다 — 지나서 그린다. 아래 오프셋들이 종전 그대로
    // 동작하도록 y 를 여기서 한 번만 옮긴다.
    let y = y + WIN_PAD;
    let hh = WIN_H - WIN_PAD;
    // 옅은 밴드. 방 경계는 pane 경계보다 **세게** 읽혀야 하는데, 종전엔 배경도 여백도
    // 없이 낮은 글자 한 줄뿐이라 그 반대였다.
    round_rect(g, x, y, w, hh, theme::radius_sm(), theme::with_alpha(theme::surface(), 0x80));
    if hit(cursor, &(x, y, w, hh)) {
        g.rect(x, y, w, hh, theme::surface_hover());
    }
    g.queue_icon(
        if collapsed { "chevron-right" } else { "chevron-down" },
        x0 - 3.0,
        y + 5.0,
        11.0,
        theme::text_mute(),
    );
    let n = format!("pane {panes}");
    let nw = g.measure_chrome_text(&n, 9.5, false);
    g.draw_text(
        right - nw,
        y + 5.0,
        &n,
        gpu::DrawOpts {
            font_size: 9.5,
            color: theme::with_alpha(theme::text_mute(), 0xA0),
            bold: false,
            italic: false,
        },
    );
    let tx = x0 + 12.0;
    // 번호를 앞에 세운다 — 방 이름은 작업 폴더에서 오는데 두 방이 같은 폴더면
    // 이름만으론 구분이 안 된다(실측: 방 셋이 전부 `Desktop`). 폭이 모자라 뒤가
    // 잘려도 번호는 남는다.
    let name = if gp.window_label.is_empty() {
        format!("방 {}", gp.window + 1)
    } else {
        format!("방 {} · {}", gp.window + 1, gp.window_label)
    };
    // 별도 창으로 나간 방은 그 사실을 머리에 적는다 — 이 pane 들은 메인 화면에
    // 없으니, 표시가 없으면 목록에만 있고 어디에도 안 보이는 유령으로 읽힌다.
    let name = if gp.undocked { format!("{name} · 별도 창") } else { name };
    let name = fit_text(g, &name, (right - nw - 8.0 - tx).max(0.0), 10.5, true);
    g.draw_text(
        tx,
        y + 4.0,
        &name,
        gpu::DrawOpts { font_size: 10.5, color: theme::text_dim(), bold: true, italic: false },
    );
}

/// pane 그룹 머리 — `▾ ● %17 프라나  info 최적화   zsh 75941  [5]`. 점 색은 그
/// pane 의 학생 색으로, 터미널 헤더·테두리가 이미 쓰는 색과 같다(같은 pane 은
/// 어디서든 같은 색). 활성 pane 은 왼쪽 띠로 한 번 더 표시한다 — 목록이 전 pane
/// 공유라 "내가 지금 있는 곳"이 안 보이면 매번 번호를 대조하게 된다.
#[allow(clippy::too_many_arguments)]
fn draw_group_head(
    g: &mut gpu::GpuRenderer,
    cursor: (f32, f32),
    gp: &PaneGroup,
    collapsed: bool,
    x: f32,
    w: f32,
    x0: f32,
    right: f32,
    y: f32,
) {
    let r = (x, y, w, GROUP_H);
    if hit(cursor, &r) {
        g.rect(x, y, w, GROUP_H, theme::surface_hover());
    }
    if gp.active {
        g.rect(x, y + 2.0, 2.0, GROUP_H - 4.0, theme::accent());
    }
    g.queue_icon(
        if collapsed { "chevron-right" } else { "chevron-down" },
        x0 - 3.0,
        y + 6.0,
        12.0,
        theme::text_mute(),
    );
    // 배정된 학생이면 색 점이 아니라 그 얼굴을 놓는다 — 색만으로는 어느 학생인지
    // 외워야 알고, 픽셀 실루엣에서는 점이 네모로 굳어 상태 표시처럼 보였다.
    // 얼굴이 없는 pane(학생 미배정)만 원래대로 색 점.
    //
    // 얼굴은 점보다 넓어서 이름 시작점도 같이 민다 — 고정 오프셋을 쓰면 chevron
    // 과 이름 양쪽에 얼굴이 겹쳐 붙는다.
    let tint = theme::character_accent(&gp.label).unwrap_or_else(theme::text_mute);
    const FACE: f32 = GROUP_H - 6.0;
    let has_face = crate::render::draw_student_face(g, &gp.label, x0 + 11.0, y + 3.0, FACE);
    if !has_face {
        circle_rect(g, x0 + 12.0, y + 9.0, 6.0, tint);
    }
    // 개수 배지가 오른쪽 끝을 먼저 잡는다 — 접힌 그룹에서 유일한 내용물이라
    // 이름에 밀려 사라지면 안 된다.
    let n = gp.rows.len().to_string();
    let nw = g.measure_chrome_text(&n, 10.0, true);
    g.draw_text(
        right - nw,
        y + 6.0,
        &n,
        gpu::DrawOpts { font_size: 10.0, color: theme::text_mute(), bold: true, italic: false },
    );
    let tx = x0 + if has_face { 15.0 + FACE } else { 24.0 };
    let mut budget = (right - nw - 8.0 - tx).max(0.0);
    let title = if gp.label.is_empty() {
        gp.pane.clone()
    } else {
        format!("{} {}", gp.pane, gp.label)
    };
    let title = fit_text(g, &title, budget, 12.0, true);
    let tw = g.measure_chrome_text(&title, 12.0, true);
    g.draw_text(
        tx,
        y + 4.0,
        &title,
        gpu::DrawOpts { font_size: 12.0, color: theme::text(), bold: true, italic: false },
    );
    // 학생 이름 다음은 **세션 제목**이다. 학생은 "누가"고 제목은 "무엇을" 이라,
    // pane 이 여럿일 때 정작 찾는 단서는 이쪽이다 — 그래서 셸·pid 보다 폭을
    // 먼저 가져간다(폭이 모자라면 밀려나는 건 셸·pid 쪽).
    budget -= tw + 8.0;
    let mut cx = tx + tw + 8.0;
    let shell = format!("{} {}", gp.shell, gp.shell_pid);
    // 다만 **통째로** 밀어내진 않는다 — 긴 제목 하나가 폭을 다 먹어 pid 가 사라지면
    // 프로세스를 짚을 열쇠가 없어진다(실측: 30자 제목이 `zsh 35776` 을 지웠다).
    // 셸 몫을 떼고 남는 만큼만 제목에 준다. 둘 다 못 담을 좁은 칼럼에서만 제목이
    // 전부 가져간다 — 그때는 pid 보다 "무엇을" 이 먼저다.
    let shell_w = g.measure_chrome_text(&shell, 10.0, false) + 8.0;
    let title_budget = if budget > shell_w + 60.0 { budget - shell_w } else { budget };
    if !gp.session.is_empty() && title_budget > 40.0 {
        let s = fit_text(g, &gp.session, title_budget, 10.5, false);
        let sw = g.measure_chrome_text(&s, 10.5, false);
        g.draw_text(
            cx,
            y + 6.0,
            &s,
            gpu::DrawOpts { font_size: 10.5, color: theme::text_dim(), bold: false, italic: false },
        );
        cx += sw + 8.0;
        budget -= sw + 8.0;
    }
    // 셸과 pid 는 남는 폭에만 — 그룹을 가리키는 이름이 잘리는 것보다 낫다.
    if budget > 40.0 {
        let s = fit_text(g, &shell, budget, 10.0, false);
        g.draw_text(
            cx,
            y + 6.0,
            &s,
            gpu::DrawOpts {
                font_size: 10.0,
                color: theme::with_alpha(theme::text_mute(), 0xA0),
                bold: false,
                italic: false,
            },
        );
    }
}

/// 되살리기 대기 줄 — `%3 시로코 · tmuxify`. 살아 있는 프로세스가 아니니 흐리게
/// 두되, 누를 수 있다는 것과 ⌘⇧T 가 **어느 줄**을 되살리는지는 분명해야 한다 —
/// 스택이 여럿일 때 그 키가 무엇을 꺼낼지 모르면 누르기가 망설여진다.
#[allow(clippy::too_many_arguments)]
fn draw_closed_row(
    g: &mut gpu::GpuRenderer,
    cursor: (f32, f32),
    c: &crate::ClosedPane,
    newest: bool,
    x: f32,
    w: f32,
    x0: f32,
    right: f32,
    y: f32,
) -> Option<(f32, f32, f32, f32)> {
    let row = (x, y, w, ROW_H);
    let hov = hit(cursor, &row);
    g.hover_pointer |= hov;
    if hov {
        g.rect(x, y, w, ROW_H, theme::surface_hover());
    }
    // 아직 도는 pane 이 기본이다 — 닫아도 죽지 않으니까. 프로세스가 사라진 것만
    // 흐리게 두고 꼬리표를 달아, 되살리기가 재부착이 아니라 `--resume` 이라는 걸
    // 누르기 전에 알 수 있게 한다.
    let base = if c.alive { 0x99 } else { 0x66 };
    let fg = theme::with_alpha(theme::text_mute(), if hov { base + 0x57 } else { base });
    // 커서가 얹힌 줄에만 × — 상시 노출하면 되살리려다 잘못 끄기 쉽다(프로세스 행과
    // 같은 규칙).
    let mut right = right;
    let mut kill = None;
    if hov {
        let br = (right - 16.0, y + 3.0, 16.0, 16.0);
        let bhov = hit(cursor, &br);
        g.hover_pointer |= bhov;
        if bhov {
            round_rect(
                g,
                br.0,
                br.1,
                br.2,
                br.3,
                theme::radius_sm(),
                theme::with_alpha(theme::danger(), 0x33),
            );
        }
        g.queue_icon(
            "x",
            br.0 + 3.0,
            br.1 + 3.0,
            10.0,
            if bhov { theme::danger() } else { theme::text_mute() },
        );
        kill = Some(br);
        right = br.0 - 6.0;
    }
    // ⌘⇧T 는 맨 위 한 줄에만 적는다 — 그 키가 되살리는 건 언제나 가장 최근 것이다.
    let kbd = newest.then(|| "\u{2318}\u{21E7}T".to_string());
    let kfs = 10.0_f32;
    let kbd_w = kbd.as_deref().map_or(0.0, |k| g.measure_chrome_text(k, kfs, false));
    let isz = 12.0_f32;
    g.queue_icon("terminal", x0 + 2.0, y + (ROW_H - isz) / 2.0, isz, fg);
    let mut label = c.pane_id.clone();
    if !c.character.is_empty() {
        label.push(' ');
        label.push_str(&c.character);
    }
    if !c.folder.is_empty() {
        label.push_str(" · ");
        label.push_str(&c.folder);
    }
    if !c.alive {
        label.push_str(" · resume");
    }
    let tx = x0 + isz + 8.0;
    let label = fit_text(g, &label, (right - kbd_w - 8.0 - tx).max(0.0), 11.0, false);
    g.draw_text(
        tx,
        y + (ROW_H - 11.0) / 2.0,
        &label,
        gpu::DrawOpts { font_size: 11.0, color: fg, bold: false, italic: false },
    );
    if let Some(k) = kbd {
        g.draw_text(
            right - kbd_w,
            y + (ROW_H - kfs) / 2.0,
            &k,
            gpu::DrawOpts {
                font_size: kfs,
                color: theme::with_alpha(theme::text_mute(), 0x88),
                bold: false,
                italic: false,
            },
        );
    }
    kill
}

/// 프로세스 한 줄 — `├─ mcp playwright  --cdp-endpoint …    :9222  2% · 90 MB  pid`.
/// 행에 커서가 있으면 오른쪽 끝에 종료(×) 버튼이 들어선다. 버튼을 상시 노출하면
/// 스크롤하다 잘못 누르기 쉽다.
///
/// 폭이 모자랄 때 **이름이 마지막까지 살아남는다**. 예전엔 pid·수치가 오른쪽부터
/// 자리를 먼저 잡고 남은 폭에 이름을 우겨넣어, 좁은 칼럼에서 이름이 통째로 잘려
/// 점만 남았다(거노: "클로드 밑으로 초록점밖에 안 보인다"). 지금은 이름 몫을 먼저
/// 떼고, 곁다리는 남는 폭이 있을 때만 그린다.
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
    g.hover_pointer |= hov;
    if hov {
        g.rect(x, y, w, ROW_H, theme::surface_hover());
    }
    // ── 계보선 ── 조상 열의 세로줄 + 자기 tick(├ / └). 선이 있으면 어느 것이
    // 누구의 자식인지가 들여쓰기 폭을 세지 않아도 읽힌다.
    let line = theme::with_alpha(theme::border(), 0xDD);
    let depth = p.depth as f32;
    for d in 0..u32::from(p.depth) {
        if p.spine & (1u32 << d) != 0 {
            g.rect(x0 + d as f32 * IND + 2.0, y, 1.0, ROW_H, line);
        }
    }
    let tick = x0 + depth * IND + 2.0;
    let mid = (y + ROW_H * 0.5).round();
    g.rect(tick, y, 1.0, if p.last { mid - y } else { ROW_H }, line);
    g.rect(tick, mid, 6.0, 1.0, line);

    let cx = x0 + depth * IND + 12.0;
    // 이름 몫부터 확보한다. 이 값 아래로는 곁다리를 그리지 않는다.
    const NAME_MIN: f32 = 64.0;
    let mut rx = right;
    if hov {
        let br = (rx - 16.0, y + 3.0, 16.0, 16.0);
        let bhov = hit(cursor, &br);
        g.hover_pointer |= bhov;
        if bhov {
            round_rect(g, br.0, br.1, br.2, br.3, theme::radius_sm(), theme::with_alpha(theme::danger(), 0x33));
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
    }
    let room = |want: f32, rx: &mut f32| -> Option<f32> {
        (*rx - want - 8.0 - cx >= NAME_MIN).then(|| {
            *rx -= want + 8.0;
            *rx
        })
    };
    // 오른쪽부터 pid → 자원 수치 → 포트 칩 순으로 자리를 잡는다. pid 를 끝에
    // 고정해야 행마다 같은 열에 서서 눈이 흔들리지 않는다. 폭이 모자라면 자리를
    // 못 얻은 것부터 조용히 빠지고, 이름은 `NAME_MIN` 덕에 끝까지 남는다.
    //
    // pid 는 수치가 아니라 손잡이라 한 단계 더 물러나 있어야 한다 — 같은 밝기면
    // 바로 왼쪽 메모리 값에 붙어 `2 MB 25655` 가 한 덩어리로 읽힌다.
    let pid_s = if p.folded > 0 {
        format!("{} +{}", p.pid, p.folded)
    } else {
        p.pid.to_string()
    };
    let pid_w = g.measure_chrome_text(&pid_s, 10.0, false);
    if let Some(px) = room(pid_w, &mut rx) {
        g.draw_text(
            px,
            y + 6.0,
            &pid_s,
            gpu::DrawOpts {
                font_size: 10.0,
                color: theme::with_alpha(theme::text_mute(), 0xA0),
                bold: false,
                italic: false,
            },
        );
    }
    if p.mem_kb > 0 {
        let m = format!("{:.0}% · {}", p.cpu, p.mem_label());
        let mw = g.measure_chrome_text(&m, 10.0, false);
        if let Some(px) = room(mw, &mut rx) {
            g.draw_text(
                px,
                y + 6.0,
                &m,
                gpu::DrawOpts { font_size: 10.0, color: theme::text_mute(), bold: false, italic: false },
            );
        }
    }
    // 포트를 쥔 프로세스는 여기서 바로 읽혀야 한다 — 아래 포트 섹션과 pid 를
    // 대조하게 만들지 않는다.
    if !p.ports.is_empty() {
        let chip = p.ports.iter().map(|c| format!(":{c}")).collect::<Vec<_>>().join(" ");
        let cw = g.measure_chrome_text(&chip, 10.0, true);
        if let Some(px) = room(cw, &mut rx) {
            g.draw_text(
                px,
                y + 6.0,
                &chip,
                gpu::DrawOpts { font_size: 10.0, color: theme::accent(), bold: true, italic: false },
            );
        }
    }

    let avail = (rx - cx).max(0.0);
    // `mcp exa` 는 앞머리가 종류, 뒤가 정체다 — 앞을 흐리게 두면 서버 이름이
    // 먼저 눈에 들어온다.
    let (head, tail) = match p.kind {
        ProcKind::Mcp => p.name.split_once(' ').unwrap_or(("", p.name.as_str())),
        _ => ("", p.name.as_str()),
    };
    let mut nx = cx;
    // claude 본체는 로고를 앞에 단다. 이름만으로도 읽히지만 목록에서 계보의
    // 기점이라 — 그 아래 npm·node·Bash 가 전부 이 프로세스의 자손이다 — 눈이
    // 한 번에 찾아야 할 자리다. 색(accent)만으로는 흑백에 가까운 테마에서 약하다.
    if matches!(p.kind, ProcKind::Claude | ProcKind::Codex) && avail > 40.0 {
        g.queue_icon("claude", nx, y + 5.0, 12.0, theme::accent());
        nx += 16.0;
    }
    if !head.is_empty() {
        let hw = g.measure_chrome_text(head, 10.5, false);
        if avail > hw + 40.0 {
            g.draw_text(
                nx,
                y + 6.0,
                head,
                gpu::DrawOpts { font_size: 10.5, color: theme::text_mute(), bold: false, italic: false },
            );
            nx += hw + 5.0;
        }
    }
    let name_col = match p.kind {
        ProcKind::Claude | ProcKind::Codex => theme::accent(),
        ProcKind::Tool => theme::text_dim(),
        _ => theme::text(),
    };
    let name = fit_text(g, tail, (rx - nx).max(0.0), 12.0, true);
    let name_w = g.measure_chrome_text(&name, 12.0, true);
    g.draw_text(
        nx,
        y + 4.0,
        &name,
        gpu::DrawOpts { font_size: 12.0, color: name_col, bold: true, italic: false },
    );
    // 부제는 오른쪽 수치와 한 칸 띄운다 — 말줄임으로 끝난 부제가 수치에 바로
    // 붙으면 `http:…0%` 처럼 한 낱말로 읽힌다.
    if !p.rest.is_empty() && rx - nx - name_w > 48.0 {
        let rest = fit_text(g, &p.rest, rx - nx - name_w - 14.0, 11.0, false);
        g.draw_text(
            nx + name_w + 6.0,
            y + 5.0,
            &rest,
            gpu::DrawOpts { font_size: 11.0, color: theme::text_mute(), bold: false, italic: false },
        );
    }
}

/// 포트 한 줄(두 줄짜리) — 윗줄이 `● 5173  %15`, 아랫줄이 "무엇인지"와 그걸
/// 쥔 프로세스. 포트 번호만으로는 며칠 전 띄워둔 서버의 정체를 알 수 없어서
/// 아랫줄을 붙였고, 소유 pane 을 밝히는 건 목록이 전 pane 공유이기 때문이다.
/// 행 전체가 클릭 대상이라 `http://localhost:<port>` 로 열린다.
#[allow(clippy::too_many_arguments)]
fn draw_port_row(
    g: &mut gpu::GpuRenderer,
    cursor: (f32, f32),
    info: &mut state::InfoState,
    p: &PortRow,
    x: f32,
    w: f32,
    x0: f32,
    right: f32,
    y: f32,
) {
    let row = (x, y, w, PORT_H);
    let hov = hit(cursor, &row);
    g.hover_pointer |= hov;
    if hov {
        g.rect(x, y, w, PORT_H, theme::surface_hover());
    }
    // 점 색이 곧 "이걸 꺼도 되나" 에 대한 답이다. 세 갈래인 것이 요점 —
    //   파랑  = 지금 이 pane 이 돌리고 있다(셸 자손). 끄려면 그 pane 을 보라.
    //   흐림  = 띄운 셸이 죽어 launchd 로 넘어갔지만 **주인 pane 은 살아 있다**.
    //           pane 을 닫아도 안 죽으니 여기서 꺼야 한다.
    //   빨강  = 띄운 pane 자체가 없다. 아무도 안 쓰는 것이므로 꺼도 된다.
    // 예전엔 뒤의 둘이 같은 흐림이라, 주인이 사라진 서버와 학생이 방금 띄운 서버가
    // 구별되지 않았다(거노: "죽은 학생이 생성해서 꺼도 되는지 모르겠다").
    let dot = if p.owner_dead {
        theme::danger()
    } else if p.orphan {
        theme::text_dim()
    } else {
        theme::accent()
    };
    circle_rect(g, x0, y + 7.0, 6.0, dot);
    let port_s = p.port.to_string();
    g.draw_text(
        x0 + 12.0,
        y + 2.0,
        &port_s,
        gpu::DrawOpts { font_size: 12.0, color: theme::text(), bold: true, italic: false },
    );
    let pw = g.measure_chrome_text(&port_s, 12.0, true);
    let mut rx = right;
    if hov {
        // 포트를 닫는 유일한 방법은 그걸 쥔 프로세스를 죽이는 것이다 — 그래서
        // 종료가 프로세스 목록에만 있으면 "포트를 끄고 싶다" 는 요구가 pid 를
        // 손으로 옮겨 적는 일이 된다(거노).
        let br = (rx - 16.0, y + 3.0, 16.0, 16.0);
        let bhov = hit(cursor, &br);
        g.hover_pointer |= bhov;
        if bhov {
            round_rect(g, br.0, br.1, br.2, br.3, theme::radius_sm(), theme::with_alpha(theme::danger(), 0x33));
        }
        g.queue_icon(
            "x",
            br.0 + 3.0,
            br.1 + 3.0,
            10.0,
            if bhov { theme::danger() } else { theme::text_mute() },
        );
        info.port_kill_rects.push((p.port, p.pid, br));
        rx = br.0 - 8.0;
        g.queue_icon("external-link", rx - 12.0, y + 5.0, 12.0, theme::text_dim());
        rx -= 18.0;
    }
    // 누가 띄웠는지 — 학생이 배정된 pane 이면 얼굴을 앞에 놓고 이름을 쓴다.
    // "(고아)" 를 떼어낸 자리다: 주인은 이미 알고 있는데 그 말이 주인 없음으로
    // 읽혀, 학생이 백그라운드로 띄운 서버가 전부 미아처럼 보였다. pane 을 닫아도
    // 안 죽는다는 사실은 위의 점 색(흐림)이 이미 말한다.
    let mut ox = x0 + 12.0 + pw + 8.0;
    let avail = (rx - ox - 12.0).max(0.0);
    if avail > 30.0 {
        let owner = if p.label.is_empty() {
            p.pane.clone().unwrap_or_default()
        } else {
            p.label.clone()
        };
        if !owner.is_empty() {
            if crate::render::draw_student_face(g, &p.label, ox, y + 2.0, 14.0) {
                ox += 17.0;
            }
            let s = fit_text(g, &owner, (rx - ox - 12.0).max(0.0), 10.0, false);
            g.draw_text(
                ox,
                y + 4.0,
                &s,
                gpu::DrawOpts {
                    font_size: 10.0,
                    color: theme::text_mute(),
                    bold: false,
                    italic: false,
                },
            );
        }
    }
    // 아랫줄 — 무엇인지가 왼쪽, 그걸 쥔 프로세스가 오른쪽.
    let mut lx = right;
    if !p.name.is_empty() {
        let nw = g.measure_chrome_text(&p.name, 10.0, false);
        if nw + 60.0 < right - x0 {
            lx = right - nw;
            g.draw_text(
                lx,
                y + 17.0,
                &p.name,
                gpu::DrawOpts {
                    font_size: 10.0,
                    color: theme::with_alpha(theme::text_mute(), 0xA0),
                    bold: false,
                    italic: false,
                },
            );
        }
    }
    let site = if p.site.is_empty() { "—" } else { p.site.as_str() };
    let site = fit_text(g, site, (lx - 8.0 - x0 - 12.0).max(0.0), 11.0, false);
    g.draw_text(
        x0 + 12.0,
        y + 16.0,
        &site,
        gpu::DrawOpts { font_size: 11.0, color: theme::text_dim(), bold: false, italic: false },
    );
}

/// 프로세스 우클릭 메뉴. 칼럼 안에 가두는 건 이 칼럼이 마지막으로 그려지는
/// 레이어가 아니어서다 — 밖으로 삐져나가면 뒤에 그려질 pane 헤더가 덮는다.
fn draw_row_menu(
    g: &mut gpu::GpuRenderer,
    cursor: (f32, f32),
    info: &mut state::InfoState,
    x: f32,
    w: f32,
    top: f32,
    bottom: f32,
) {
    info.ctx_menu_rects.clear();
    let Some((rawx, rawy, target)) = info.ctx_menu else { return };
    use state::InfoMenuAction as A;
    use state::InfoTarget as T;
    // (액션, 라벨, 위험, 앞에 구분선)
    let items: Vec<(A, &str, bool, bool)> = match target {
        T::Proc(_) => vec![
            (A::Terminate, "종료 (SIGTERM)", false, false),
            (A::ForceKill, "강제 종료 (SIGKILL)", true, false),
            (A::CopyPid, "PID 복사", false, true),
            (A::CopyCmd, "명령 복사", false, false),
        ],
        // 포트는 그 자체로 죽일 수 없다 — 쥔 프로세스를 죽이는 것이 곧 포트를
        // 닫는 것이라, 같은 메뉴 안에서 열기와 닫기가 이어지게 둔다.
        T::Port(..) => vec![
            (A::OpenPort, "브라우저로 열기", false, false),
            (A::CopyUrl, "URL 복사", false, false),
            (A::Terminate, "포트 닫기 (SIGTERM)", false, true),
            (A::ForceKill, "강제 종료 (SIGKILL)", true, false),
            (A::CopyPid, "PID 복사", false, true),
        ],
    };
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
    panel_rect_outlined(g, mx, my, menu_w, menu_h, theme::radius_md(), theme::surface());
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
            crate::hover_rect(g, r.0, r.1, r.2, r.3, theme::radius_sm());
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
pub(crate) fn short_app_name(name: &str) -> &str {
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
    // 글자 폭을 누적해 끊는다. 한 글자 늘릴 때마다 줄 전체를 다시 재면 길이의
    // 제곱만큼 글리프를 뒤지게 된다 — `fit_text` 와 같은 이유로 O(n) 로 둔다.
    let mut w = 0.0;
    let mut buf = [0u8; 4];
    for ch in s.chars() {
        let cw = g.measure_chrome_text(ch.encode_utf8(&mut buf), 11.0, false);
        if w + cw > avail {
            if cur.is_empty() {
                break; // 한 글자도 안 들어가는 폭 — 그릴 게 없다.
            }
            lines.push(std::mem::take(&mut cur));
            w = 0.0;
        }
        cur.push(ch);
        w += cw;
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
///
/// 자를 위치를 찾을 때 글자를 하나 늘릴 때마다 **앞부분 전체**를 다시 재던 것이
/// "Info 를 열면 프레임이 떨어진다"(거노)의 주범이었다. `measure_chrome_text` 는
/// 그 자체가 글자마다 아틀라스를 뒤지므로 그 방식은 길이의 제곱으로 붇고,
/// claude·MCP 처럼 argv 가 긴 행이 목록에 깔리면 프레임 예산을 통째로 먹는다.
/// 글자 폭은 서로 독립이라 한 번 훑으며 누적하면 같은 답이 한 바퀴에 나온다.
pub(crate) fn fit_text(
    g: &mut gpu::GpuRenderer,
    s: &str,
    avail: f32,
    size: f32,
    bold: bool,
) -> String {
    if avail <= 0.0 {
        return String::new();
    }
    let ell = g.measure_chrome_text("…", size, bold);
    if avail <= ell {
        return String::new();
    }
    let budget = avail - ell;
    let mut w = 0.0;
    // char 경계로만 자른다 — 바이트로 자르면 한글/이모지에서 패닉한다.
    let mut cut = 0;
    let mut buf = [0u8; 4];
    // 폭을 넘기는 순간 멈춘다. 통짜로 한 번 재고 시작하면 화면에 절대 안 나올
    // 꼬리까지 재게 되는데, argv 는 수백 자가 예사라 그 비용이 목록 전체를
    // 지배했다(실측: 프로세스 5줄에 18.3ms → 이 조기 종료로 사라짐).
    for (i, ch) in s.char_indices() {
        let cw = g.measure_chrome_text(ch.encode_utf8(&mut buf), size, bold);
        if w + cw <= budget {
            cut = i + ch.len_utf8();
        }
        w += cw;
        if w > avail {
            return format!("{}…", &s[..cut]);
        }
    }
    s.to_string()
}

#[cfg(test)]
mod session_title_tests {
    use super::session_title;

    fn tmp_jsonl(tag: &str, body: &str) -> std::path::PathBuf {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let d = std::env::temp_dir().join(format!("kasaterm-sesstitle-{tag}-{n}"));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("11111111-2222-3333-4444-555555555555.jsonl");
        std::fs::write(&p, body).unwrap();
        p
    }

    /// `/rename` 으로 붙인 이름이 최우선 — pane 머리에 학생 이름 옆으로 나가는 값.
    #[test]
    fn custom_title_wins() {
        let p = tmp_jsonl(
            "custom",
            "{\"type\":\"ai-title\",\"aiTitle\":\"하이쿠 요약\"}\n\
             {\"type\":\"custom-title\",\"customTitle\":\"info 최적화\"}\n",
        );
        assert_eq!(session_title(&p), "info 최적화");
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    /// rename 이 없으면 claude 가 붙인 요약(aiTitle)으로 떨어진다.
    #[test]
    fn falls_back_to_ai_title() {
        let p = tmp_jsonl("ai", "{\"type\":\"ai-title\",\"aiTitle\":\"하이쿠 요약\"}\n");
        assert_eq!(session_title(&p), "하이쿠 요약");
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    /// 캐시 열쇠는 파일 크기다 — transcript 가 자라면(rename 이 append 된다) 반드시
    /// 다시 읽어야 한다. 여기서 옛 제목이 나오면 pane 머리가 영영 안 바뀐다.
    #[test]
    fn reread_after_the_transcript_grows() {
        let p = tmp_jsonl("grow", "{\"type\":\"ai-title\",\"aiTitle\":\"처음\"}\n");
        assert_eq!(session_title(&p), "처음");
        let mut body = std::fs::read_to_string(&p).unwrap();
        body.push_str("{\"type\":\"custom-title\",\"customTitle\":\"이름 바꿈\"}\n");
        std::fs::write(&p, body).unwrap();
        assert_eq!(session_title(&p), "이름 바꿈");
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }
}
