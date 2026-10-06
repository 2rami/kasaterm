//! 오른쪽 Info 열 맨 위 「지금 보는 칸」 카드. Git 열처럼 초점 칸 하나만 본다 — 칸 종류·경로·도는 도구와 최근
//! 도구·백그라운드·그 칸 프로세스 트리의 listen 포트·모델·추론 강도·문맥·승인 요청.
//!
//! 사실은 `observe` 한 곳에서 짓는다. 이 기기 칸은 감시 스레드가, 다른 기기가 거울로 보는 칸은 원본의
//! `/term/pane-info`(`kasa_mcp::pane_info`)가 같은 함수를 부른다. claude 칸은 연결 mod 이벤트로 바로, mod 없는
//! 칸·코덱스·셸은 프로세스 표(300ms 공유 캐시)로 프로세스가 뜨고 질 때와 2초마다, 포트는 프로세스가 바뀔 때와
//! 10초마다 다시 읽는다.
use super::*;
use kasa_mcp::claude_mod::FocusFacts;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{mpsc, Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

const TICK: Duration = Duration::from_millis(500);
/// 신호가 없는 바뀜(셸의 cd·상태줄이 알린 모델)을 잡는 주기.
const RELOOK: Duration = Duration::from_secs(2);
const PORTS_SLOW: Duration = Duration::from_secs(10);
/// 프로세스가 막 뜨면 listen 은 조금 뒤에 선다 — 그때 한 번 더 본다.
const PORTS_LATE: Duration = Duration::from_secs(2);
const REMOTE_CAP: usize = 256 * 1024;
const RECENT_SHOWN: usize = 3;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct FocusPort {
    pub(crate) port: u16,
    /// 그 포트를 쥔 프로세스 이름.
    pub(crate) name: String,
    /// 표준 서비스 이름이나 그 서버가 응답한 `<title>`.
    pub(crate) site: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct FocusCard {
    pub(crate) pane: String,
    /// `claude`·`codex`·`agy` — 셸뿐이면 빈 값.
    pub(crate) harness: String,
    pub(crate) shell: String,
    /// 에이전트가 아닌 칸에서 셸 위에 지금 도는 프로그램. 없으면 빈 값.
    pub(crate) program: String,
    pub(crate) cwd: String,
    pub(crate) model: String,
    pub(crate) effort: String,
    /// 연결 mod 가 알린 지금(claude 칸만).
    pub(crate) facts: Option<FocusFacts>,
    pub(crate) ports: Vec<FocusPort>,
}

/// 지금 그리는 카드. `at` 은 카드를 받은 때 — 도구·백그라운드 나이는 그 뒤로 흐른 만큼 더해 그린다.
#[derive(Clone, Debug, Default)]
pub(crate) struct Seen {
    pub(crate) card: Option<FocusCard>,
    pub(crate) at: Option<Instant>,
    /// 다른 기기 칸을 못 읽은 까닭.
    pub(crate) issue: Option<String>,
    /// 다른 기기 칸이면 그 기기 이름과, 그 칸의 포트를 열 주소(호스트).
    pub(crate) remote: Option<String>,
    pub(crate) host: Option<String>,
}

impl Seen {
    fn blank(target: &Option<Target>) -> Self {
        match target {
            Some(Target::Remote { label, base, .. }) => Seen { remote: Some(label.clone()), host: remote_host(base), ..Seen::default() },
            _ => Seen::default(),
        }
    }


    fn age(&self, ms: u64) -> u64 {
        ms / 1000 + self.at.map_or(0, |at| at.elapsed().as_secs())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Target {
    Local { pane: String },
    /// 거울 칸 — 원본 기기의 그 칸을 묻는다.
    Remote { pane: String, base: String, remote_id: String, machine_id: String, surface_key: Option<String>, label: String },
}

/// 카드에서 누르는 것.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum FocusHit {
    Folder(String),
    Port { port: u16, host: Option<String> },
}

enum Msg {
    Kick(String),
    Answer(u64, Result<serde_json::Value, String>),
}

/// GUI 와 감시 스레드가 함께 쥐는 자리.
pub(crate) struct FocusShared {
    target: Mutex<(u64, Option<Target>)>,
    /// 화면이 보고 있는 폴더(bg-attach 보기 칸은 셸 폴더와 다르다). 바뀌어도 카드를 비우지 않게 표적과 따로 둔다.
    cwd_hint: Mutex<Option<std::path::PathBuf>>,
    seen: Mutex<Seen>,
    rev: AtomicU64,
    visible: AtomicBool,
    tx: Mutex<Option<mpsc::Sender<Msg>>>,
}

impl Default for FocusShared {
    fn default() -> Self {
        Self {
            target: Mutex::new((0, None)),
            cwd_hint: Mutex::new(None),
            seen: Mutex::new(Seen::default()),
            rev: AtomicU64::new(0),
            visible: AtomicBool::new(false),
            tx: Mutex::new(None),
        }
    }
}

impl FocusShared {
    fn send(&self, msg: Msg) {
        if let Some(tx) = self.tx.lock().unwrap().as_ref() {
            let _ = tx.send(msg);
        }
    }

    /// 초점 칸을 정한다. 바뀌었으면 카드를 비우고 감시를 바로 깨운다.
    pub(crate) fn aim(&self, target: Option<Target>, cwd_hint: Option<std::path::PathBuf>, visible: bool) {
        *self.cwd_hint.lock().unwrap() = cwd_hint;
        let was = self.visible.swap(visible, Relaxed);
        let changed = {
            let mut current = self.target.lock().unwrap();
            if current.1 == target {
                false
            } else {
                current.0 += 1;
                *self.seen.lock().unwrap() = Seen::blank(&target);
                current.1 = target;
                self.rev.fetch_add(1, Relaxed);
                true
            }
        };
        if changed || (visible && !was) {
            self.send(Msg::Kick(String::new()));
        }
    }

    /// 새 카드가 왔으면 꺼낸다.
    pub(crate) fn take(&self, seen_rev: &mut u64) -> Option<Seen> {
        let rev = self.rev.load(Relaxed);
        if rev == *seen_rev {
            return None;
        }
        *seen_rev = rev;
        Some(self.seen.lock().unwrap().clone())
    }

    fn store(&self, generation: u64, seen: Seen) -> bool {
        if self.target.lock().unwrap().0 != generation {
            return false;
        }
        *self.seen.lock().unwrap() = seen;
        self.rev.fetch_add(1, Relaxed);
        true
    }
}

/// 감시 스레드를 띄우고 mod 신호·다른 기기 창구를 잇는다. 앱이 한 번 부른다.
pub(crate) fn start(shared: &Arc<FocusShared>, sites: info::SiteCache, backend: Arc<socket::PtyBackend>, proxy: winit::event_loop::EventLoopProxy<UserEvent>) {
    let _ = SITES.set(sites);
    let (tx, rx) = mpsc::channel();
    *shared.tx.lock().unwrap() = Some(tx.clone());
    let ports_kick = Mutex::new(tx.clone());
    let _ = PORTS_CHANGED.set(Box::new(move |pane| {
        let _ = ports_kick.lock().unwrap().send(Msg::Kick(pane.to_string()));
    }));
    let kick = Mutex::new(tx);
    kasa_mcp::claude_mod::set_focus_listener(move |surface| {
        let _ = kick.lock().unwrap().send(Msg::Kick(surface.to_string()));
    });
    let provider_backend = backend.clone();
    kasa_mcp::pane_info::set_provider(move |pane| {
        let (card, print) = observe(pane, None, Some(&provider_backend))?;
        Some((serde_json::to_value(card).ok()?, print))
    });
    let shared = shared.clone();
    std::thread::spawn(move || watch(shared, rx, backend, proxy));
}

fn watch(shared: Arc<FocusShared>, rx: mpsc::Receiver<Msg>, backend: Arc<socket::PtyBackend>, proxy: winit::event_loop::EventLoopProxy<UserEvent>) {
    let redraw = || proxy.send_event(UserEvent::Redraw).is_ok();
    let mut done_generation = u64::MAX;
    let mut print = 0u64;
    let mut procs = 0u64;
    let mut looked = Instant::now();
    let mut ticked = Instant::now();
    let mut asking: Option<u64> = None;
    let mut since: Option<String> = None;
    let mut rest_until = Instant::now();
    loop {
        let msg = match rx.recv_timeout(TICK) {
            Ok(msg) => Some(msg),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
        let (generation, target) = shared.target.lock().unwrap().clone();
        let fresh = generation != done_generation;
        if fresh {
            done_generation = generation;
            print = 0;
            procs = 0;
            since = None;
            asking = None;
            rest_until = Instant::now();
        }
        if let Some(Msg::Answer(answered, result)) = &msg {
            if *answered == generation && asking == Some(generation) {
                asking = None;
                let (mut seen, back) = remote_answer(result, &target, &mut since);
                // 잠깐 끊긴 것이면 지난 카드를 까닭과 함께 둔다 — 비웠다 다시 채우면 카드가 깜빡인다.
                if seen.card.is_none() {
                    let last = shared.seen.lock().unwrap().clone();
                    (seen.card, seen.at) = (last.card, last.at);
                }
                rest_until = Instant::now() + back;
                if shared.store(generation, seen) && !redraw() {
                    return;
                }
            }
        }
        if !shared.visible.load(Relaxed) {
            continue;
        }
        if ticking(&shared) && ticked.elapsed() >= Duration::from_secs(1) {
            ticked = Instant::now();
            if !redraw() {
                return;
            }
        }
        let Some(target) = target else { continue };
        match &target {
            Target::Local { pane } => {
                let kicked = matches!(&msg, Some(Msg::Kick(surface)) if surface.is_empty() || surface == pane);
                let now_procs = process_print(pane);
                if !(fresh || kicked || now_procs != procs || looked.elapsed() >= RELOOK) {
                    continue;
                }
                procs = now_procs;
                looked = Instant::now();
                let hint = shared.cwd_hint.lock().unwrap().clone();
                let observed = observe(pane, hint.as_deref(), Some(&backend));
                let next = observed.as_ref().map_or(1, |(_, p)| *p);
                if next == print && !fresh {
                    continue;
                }
                print = next;
                let seen = Seen { card: observed.map(|(card, _)| card), at: Some(Instant::now()), ..Seen::default() };
                if shared.store(generation, seen) && !redraw() {
                    return;
                }
            }
            Target::Remote { base, remote_id, machine_id, surface_key, .. } => {
                if asking.is_some() || Instant::now() < rest_until {
                    continue;
                }
                let Some(key) = surface_key.clone() else {
                    let seen = Seen { issue: Some("원본 기기 앱이 옛 판이라 이 칸의 정보를 못 받아요".into()), ..Seen::blank(&Some(target.clone())) };
                    rest_until = Instant::now() + Duration::from_secs(30);
                    if shared.store(generation, seen) && !redraw() {
                        return;
                    }
                    continue;
                };
                asking = Some(generation);
                let query = format!(
                    "/term/pane-info?schema={}&machine_id={}&pane={}&surface_key={}&wait_ms={}{}",
                    kasa_mcp::pane_info::SCHEMA,
                    kasa_mcp::remote::urlencode(machine_id),
                    kasa_mcp::remote::urlencode(remote_id),
                    kasa_mcp::remote::urlencode(&key),
                    kasa_mcp::pane_info::WAIT_CAP_MS,
                    since.as_deref().map(|s| format!("&since={}", kasa_mcp::remote::urlencode(s))).unwrap_or_default(),
                );
                let (base, tx) = (base.clone(), shared.tx.lock().unwrap().clone());
                std::thread::spawn(move || {
                    let result = kasa_mcp::remote::remote_get_json_bounded(&base, &query, REMOTE_CAP).map_err(|e| e.to_string());
                    if let Some(tx) = tx {
                        let _ = tx.send(Msg::Answer(generation, result));
                    }
                });
            }
        }
    }
}

/// 도는 도구·백그라운드가 있으면 그 나이를 1초마다 다시 그린다.
fn ticking(shared: &FocusShared) -> bool {
    shared.seen.lock().unwrap().card.as_ref().and_then(|c| c.facts.as_ref()).is_some_and(|f| !f.tools.is_empty() || !f.background.is_empty() || f.ask.is_some())
}

fn remote_answer(result: &Result<serde_json::Value, String>, target: &Option<Target>, since: &mut Option<String>) -> (Seen, Duration) {
    let failed = |issue: &str, back: u64| (Seen { issue: Some(issue.into()), ..Seen::blank(target) }, Duration::from_secs(back));
    match result {
        Ok(answer) if answer.get("ok").and_then(serde_json::Value::as_bool) == Some(true) => {
            *since = answer.get("seq").and_then(serde_json::Value::as_str).map(str::to_string);
            let card = answer.get("card").cloned().and_then(|card| serde_json::from_value(card).ok());
            (Seen { card, at: Some(Instant::now()), ..Seen::blank(target) }, Duration::ZERO)
        }
        Ok(answer) => {
            *since = None;
            match answer.get("error").and_then(serde_json::Value::as_str).unwrap_or("") {
                "update_needed" => failed("원본 기기 앱이 옛 판이라 이 칸의 정보를 못 받아요", 60),
                "source_changed" | "pane_gone" => failed("원본 칸이 바뀌었어요 — 다시 읽는 중이에요", 3),
                _ => failed("원본 기기가 이 칸의 정보를 못 내줬어요", 10),
            }
        }
        // 이 길이 없는 옛 원본은 404 — 자주 두드리지 않는다.
        Err(error) if error.contains("404") => {
            *since = None;
            failed("원본 기기 앱이 옛 판이라 이 칸의 정보를 못 받아요", 120)
        }
        Err(_) => {
            *since = None;
            failed("원본 기기에 닿지 않아요 — 다시 잇는 중이에요", 5)
        }
    }
}

fn name_of(comm: &str) -> String {
    comm.rsplit('/').next().unwrap_or(comm).trim_start_matches('-').to_string()
}

/// 셸 아래 프로세스 전부(셸 제외). 프로세스 표의 고리에 갇히지 않게 본 pid 는 다시 안 간다.
fn tree(table: &[(u32, u32, String)], shell_pid: u32) -> Vec<(u32, String)> {
    let mut children: HashMap<u32, Vec<(u32, &str)>> = HashMap::new();
    for (pid, ppid, comm) in table {
        children.entry(*ppid).or_default().push((*pid, comm));
    }
    let mut out = Vec::new();
    let mut seen = HashSet::from([shell_pid]);
    let mut stack = vec![shell_pid];
    while let Some(pid) = stack.pop() {
        for (child, comm) in children.get(&pid).into_iter().flatten() {
            if seen.insert(*child) {
                out.push((*child, name_of(comm)));
                stack.push(*child);
            }
        }
    }
    out
}

fn hash_of(value: impl Hash) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// 칸 프로세스의 지문 — 뜨고 진 것을 싸게 안다.
fn process_print(pane: &str) -> u64 {
    let Some(shell) = kasa_pty::lookup_session(pane).and_then(|s| s.shell_pid()) else { return 0 };
    let mut pids: Vec<u32> = tree(&kasa_pty::process_table_shared(), shell).into_iter().map(|(pid, _)| pid).collect();
    pids.sort_unstable();
    hash_of(pids)
}

struct PortsSeen {
    print: u64,
    due: Instant,
    list: Vec<FocusPort>,
    reading: bool,
}

static PORTS: LazyLock<Mutex<HashMap<String, PortsSeen>>> = LazyLock::new(Default::default);
/// 포트가 바뀐 칸을 감시 스레드에 알린다.
static PORTS_CHANGED: std::sync::OnceLock<Box<dyn Fn(&str) + Send + Sync>> = std::sync::OnceLock::new();
/// 포트가 응답한 제목 — 아래 「모든 방」 수집과 같은 캐시를 써서 같은 서버에 두 번 묻지 않는다.
static SITES: std::sync::OnceLock<info::SiteCache> = std::sync::OnceLock::new();

fn sites() -> &'static info::SiteCache {
    SITES.get_or_init(Default::default)
}

/// 칸 프로세스 트리가 listen 중인 포트 — 지금 아는 것을 바로 준다. 프로세스가 바뀌었거나 느린 주기가 지났으면 뒤에서
/// 다시 읽고(lsof 는 0.4초쯤 걸린다 — mod 사실이 그것을 기다리면 안 된다), 바뀌었으면 감시 스레드를 깨운다.
fn ports_of(pane: &str, procs: &[(u32, String)]) -> Vec<FocusPort> {
    let mut pids: Vec<u32> = procs.iter().map(|(pid, _)| *pid).collect();
    pids.sort_unstable();
    let print = hash_of(&pids);
    let now = Instant::now();
    let mut map = PORTS.lock().unwrap();
    let seen = map.entry(pane.to_string()).or_insert_with(|| PortsSeen { print: 0, due: now, list: Vec::new(), reading: false });
    if seen.reading || (seen.print == print && now < seen.due) {
        return seen.list.clone();
    }
    seen.reading = true;
    let changed = seen.print != print;
    let (pane, names) = (pane.to_string(), procs.iter().cloned().collect::<HashMap<u32, String>>());
    std::thread::spawn(move || {
        let mine: Vec<(u16, u32)> = if names.is_empty() {
            Vec::new()
        } else {
            info::listening_ports().into_iter().filter(|(_, pid)| names.contains_key(pid)).collect()
        };
        info::probe_sites(&mine, sites());
        let mut list: Vec<FocusPort> = mine
            .iter()
            .map(|(port, pid)| FocusPort { port: *port, name: names.get(pid).cloned().unwrap_or_default(), site: info::site_label(*port, None, sites()) })
            .collect();
        list.dedup_by_key(|p| p.port);
        // 프로세스가 막 바뀌었으면 listen 이 늦게 선다 — 곧 한 번 더.
        let due = Instant::now() + if changed { PORTS_LATE } else { PORTS_SLOW };
        let moved = {
            let mut map = PORTS.lock().unwrap();
            let old = map.insert(pane.clone(), PortsSeen { print, due, list: list.clone(), reading: false });
            old.is_none_or(|old| old.list != list)
        };
        if moved {
            if let Some(changed) = PORTS_CHANGED.get() {
                changed(&pane);
            }
        }
    });
    seen.list.clone()
}

/// 칸 하나의 지금과, 나이를 뺀 사실의 지문. 막는 일(프로세스 표·포트)이 있어 GUI 스레드 밖에서 부른다.
pub(crate) fn observe(pane: &str, cwd_hint: Option<&std::path::Path>, backend: Option<&socket::PtyBackend>) -> Option<(FocusCard, u64)> {
    let session = kasa_pty::lookup_session(pane)?;
    let shell_pid = session.shell_pid()?;
    let table = kasa_pty::process_table_shared();
    let procs = tree(&table, shell_pid);
    let harness = kasa_pty::agent_for_shell(&table, shell_pid).map(|kind| kind.as_str().to_string()).unwrap_or_default();
    // 셸이 지금 앞에 세운 것 — 가장 늦게 뜬 직속 자식.
    let program = if harness.is_empty() {
        table.iter().filter(|(_, ppid, _)| *ppid == shell_pid).max_by_key(|(pid, _, _)| *pid).map(|(_, _, comm)| name_of(comm)).unwrap_or_default()
    } else {
        String::new()
    };
    let cwd = cwd_hint
        .map(std::path::Path::to_path_buf)
        .or_else(|| session.reported_cwd())
        .or_else(|| socket::pid_cwd(shell_pid))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (model, effort) = backend.and_then(|b| b.agent_cfg_snapshot().remove(pane)).unwrap_or_default();
    let card = FocusCard {
        pane: pane.to_string(),
        harness,
        shell: table.iter().find(|(pid, _, _)| *pid == shell_pid).map(|(_, _, comm)| name_of(comm)).unwrap_or_default(),
        program,
        cwd,
        model,
        effort,
        facts: kasa_mcp::claude_mod::focus_facts(pane),
        ports: ports_of(pane, &procs),
    };
    let print = fingerprint(&card);
    Some((card, print))
}

/// 나이(흐르는 시간)를 뺀 지문 — 같으면 다시 그리거나 다른 기기에 답할 까닭이 없다.
fn fingerprint(card: &FocusCard) -> u64 {
    let mut still = card.clone();
    if let Some(facts) = still.facts.as_mut() {
        facts.tools.iter_mut().chain(facts.recent.iter_mut()).for_each(|t| t.age_ms = 0);
        facts.background.iter_mut().for_each(|t| t.age_ms = 0);
        if let Some(ask) = facts.ask.as_mut() {
            ask.age_ms = 0;
        }
    }
    hash_of(serde_json::to_string(&still).unwrap_or_default())
}

impl App {
    /// 지금 보는 칸 — Git 열과 같은 칸(활성 pane 의 보고 있는 탭).
    pub(crate) fn info_focus_target(&self) -> (Option<Target>, Option<std::path::PathBuf>) {
        let Some(pane) = self.ws.lock().ok().and_then(|ws| ws.active_pane.as_deref().map(|id| ws.active_tab_pid(id))) else {
            return (None, None);
        };
        let Some(remote) = kasa_mcp::remote::remote_info(&pane) else {
            let hint = self.pane_view_cwd.get(&pane).cloned();
            return (Some(Target::Local { pane }), hint);
        };
        let label = kasa_mcp::machines::label_for_base(&remote.base)
            .or_else(|| (!remote.label.is_empty()).then(|| remote.label.clone()))
            .unwrap_or_else(|| "원격 기기".into());
        let machine_id = kasa_mcp::machines::find(&label).and_then(|m| m.machine_id).unwrap_or_default();
        let surface_key = kasa_mcp::remote::remote_surface_key(&pane);
        (Some(Target::Remote { pane, base: remote.base, remote_id: remote.remote_id, machine_id, surface_key, label }), None)
    }

    /// 카드의 줄을 누른 것 — 폴더는 파일 관리자로, 포트는 브라우저(카사크롬을 골랐으면 그쪽)로.
    pub(crate) fn run_focus_hit(&mut self, hit: FocusHit) {
        match hit {
            FocusHit::Folder(path) => self.open_url(&path),
            FocusHit::Port { port, host } => {
                let pane = self.info.focus_view.card.as_ref().map(|c| c.pane.clone());
                let url = format!("http://{}:{port}", host.as_deref().unwrap_or("localhost"));
                self.open_url_for_pane(&url, pane.as_deref());
            }
        }
    }
}

/// 다른 기기 칸의 포트는 그 기기 주소로 연다 — 이 기기의 localhost 는 다른 서버다.
fn remote_host(base: &str) -> Option<String> {
    let rest = base.split_once("://").map_or(base, |(_, rest)| rest);
    let host = rest.split('/').next()?.rsplit_once(':').map_or(rest.split('/').next()?, |(host, _)| host);
    (!host.is_empty()).then(|| host.trim_matches(['[', ']']).to_string())
}

fn seconds(s: u64) -> String {
    if s < 60 {
        format!("{s}초")
    } else if s < 3600 {
        format!("{}분", s / 60)
    } else {
        let (h, m) = (s / 3600, (s % 3600) / 60);
        if m == 0 { format!("{h}시간") } else { format!("{h}시간 {m}분") }
    }
}

fn took(ms: u64) -> String {
    if ms < 1000 { format!("{:.1}초", ms as f64 / 1000.0) } else { seconds(ms / 1000) }
}

fn tokens(n: u64) -> String {
    match n {
        1_000_000.. if n % 1_000_000 == 0 => format!("{}M", n / 1_000_000),
        1_000_000.. => format!("{:.1}M", n as f64 / 1_000_000.0),
        1000.. => format!("{}k", n / 1000),
        _ => n.to_string(),
    }
}

fn harness_label(harness: &str) -> String {
    kasa_pty::AgentKind::from_id(harness).map_or_else(|| harness.to_string(), |kind| kind.label().to_string())
}

fn task_kind(kind: &str) -> &str {
    match kind {
        "subagent" => "서브에이전트",
        "shell" => "셸",
        "monitor" => "모니터",
        "workflow" => "워크플로",
        other => other,
    }
}

fn tool_line(tool: &kasa_mcp::claude_mod::FocusTool, elapsed: String) -> String {
    let head = if tool.agent { format!("서브 · {}", tool.tool) } else { tool.tool.clone() };
    if tool.label.is_empty() { format!("{head} · {elapsed}") } else { format!("{head} · {} · {elapsed}", tool.label) }
}

/// 카드의 줄들 — 맨 위 머리, 그 아래 「이름 · 값」 표. 그리기·잘림·말풍선은 Info 표와 같은 길을 탄다.
pub(crate) fn card_lines<'a>(seen: &Seen, alt_screen: bool) -> Vec<info::ExecutionLine<'a>> {
    use info::ExecutionLine as L;
    let kv = |name: &str, value: String, tip: String, warn: bool| L::Kv { name: name.into(), value: (!value.is_empty()).then_some(value), tip, warn };
    let device = seen.remote.as_deref().map(crate::render::pane_identity::device_name);
    let Some(card) = &seen.card else {
        let mut lines = vec![L::Head { title: "지금 보는 칸".into(), note: device.unwrap_or_default() }];
        lines.push(match &seen.issue {
            Some(issue) => L::Text(issue.clone(), true),
            None if seen.remote.is_some() => L::Text("원본 기기에서 읽는 중이에요".into(), false),
            None => L::Text("보고 있는 칸을 읽는 중이에요".into(), false),
        });
        return lines;
    };
    let facts = card.facts.as_ref();
    let kind = if !card.harness.is_empty() {
        harness_label(&card.harness)
    } else if alt_screen && !card.program.is_empty() {
        format!("전체 화면 · {}", card.program)
    } else if !card.program.is_empty() {
        format!("{} · {} 실행 중", card.shell, card.program)
    } else if card.shell.is_empty() {
        "셸".into()
    } else {
        format!("셸 · {}", card.shell)
    };
    let note = match &device {
        Some(device) => format!("{device} · {}", card.pane),
        None => card.pane.clone(),
    };
    let mut lines = vec![L::Head { title: kind, note }];
    if let Some(issue) = &seen.issue {
        lines.push(L::Text(issue.clone(), true));
    }
    if let Some(facts) = facts {
        let (state, warn) = match facts.state.as_str() {
            "permission" => ("승인 기다림", true),
            "question" => ("답 기다림", true),
            "compacting" => ("압축 중", false),
            "working" => ("일하는 중", false),
            _ => ("쉼", false),
        };
        lines.push(kv("상태", state.into(), String::new(), warn));
    }
    if !card.cwd.is_empty() {
        let shown = info::tilde_path(std::path::Path::new(&card.cwd));
        let (hit, tip) = if seen.remote.is_some() {
            (None, format!("{}\n원본 기기의 폴더예요", card.cwd))
        } else {
            (Some(FocusHit::Folder(card.cwd.clone())), format!("{}\n누르면 폴더를 열어요", card.cwd))
        };
        lines.push(L::Link { name: "경로".into(), value: shown, tip, hit });
    }
    if !card.model.is_empty() {
        let value = if card.effort.is_empty() { card.model.clone() } else { format!("{} · {}", card.model, card.effort) };
        lines.push(kv("모델", value, if card.effort.is_empty() { String::new() } else { format!("추론 강도 {}", card.effort) }, false));
    }
    if let Some(context) = facts.and_then(|f| f.context.as_ref()) {
        let mut parts = Vec::new();
        if let Some(percent) = context.percent {
            parts.push(format!("{percent:.0}%"));
        }
        if let (Some(used), Some(window)) = (context.tokens, context.window) {
            parts.push(format!("{} / {}", tokens(used), tokens(window)));
        }
        if !parts.is_empty() {
            lines.push(kv("문맥", parts.join(" · "), "이 대화가 쓴 문맥 창".into(), context.percent.is_some_and(|p| p >= 80.0)));
        }
    }
    if let Some(ask) = facts.and_then(|f| f.ask.as_ref()) {
        let value = if ask.preview.is_empty() { ask.tool.clone() } else { format!("{} · {}", ask.tool, ask.preview) };
        lines.push(kv("승인", format!("{value} · {}", seconds(seen.age(ask.age_ms))), ask.preview.clone(), true));
    }
    match facts {
        Some(facts) => {
            for (i, tool) in facts.tools.iter().enumerate() {
                lines.push(kv(if i == 0 { "도구" } else { "" }, tool_line(tool, seconds(seen.age(tool.age_ms))), tool.label.clone(), false));
            }
            for (i, tool) in facts.recent.iter().take(RECENT_SHOWN).enumerate() {
                let mut line = tool_line(tool, took(tool.took_ms.unwrap_or(0)));
                if tool.error {
                    line.push_str(" · 실패");
                }
                lines.push(kv(if i == 0 { "최근" } else { "" }, line, format!("{} 전에 끝남", seconds(seen.age(tool.age_ms))), tool.error));
            }
            for (i, task) in facts.background.iter().enumerate() {
                let elapsed = seconds(seen.age(task.age_ms));
                let value = if task.label.is_empty() { format!("{} · {elapsed}", task_kind(&task.kind)) } else { format!("{} · {} · {elapsed}", task_kind(&task.kind), task.label) };
                lines.push(kv(if i == 0 { "백그라운드" } else { "" }, value, task.label.clone(), false));
            }
        }
        None if card.harness == "claude" => {
            lines.push(L::Kv { name: "도구".into(), value: None, tip: "이 칸의 claude 는 연결 mod 없이 떠 있어요 — 도구·백그라운드는 mod 가 실린 칸만 보여요".into(), warn: false });
        }
        None => {}
    }
    for (i, port) in card.ports.iter().enumerate() {
        let mut value = port.port.to_string();
        for part in [&port.name, &port.site] {
            if !part.is_empty() {
                value.push_str(" · ");
                value.push_str(part);
            }
        }
        let tip = if seen.remote.is_some() { "원본 기기 주소로 열어요".to_string() } else { "누르면 브라우저로 열어요".to_string() };
        lines.push(L::Link { name: if i == 0 { "포트".into() } else { String::new() }, value, tip, hit: Some(FocusHit::Port { port: port.port, host: seen.host.clone() }) });
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use kasa_mcp::claude_mod::{FocusAsk, FocusContext, FocusTask, FocusTool};

    fn rows<'a>(lines: &[info::ExecutionLine<'a>]) -> Vec<(String, String)> {
        lines
            .iter()
            .filter_map(|line| match line {
                info::ExecutionLine::Kv { name, value, .. } => Some((name.clone(), value.clone().unwrap_or_default())),
                info::ExecutionLine::Link { name, value, .. } => Some((name.clone(), value.clone())),
                _ => None,
            })
            .collect()
    }

    fn head(lines: &[info::ExecutionLine<'_>]) -> (String, String) {
        match &lines[0] {
            info::ExecutionLine::Head { title, note } => (title.clone(), note.clone()),
            _ => panic!("카드는 머리로 시작한다"),
        }
    }

    #[test]
    fn the_tree_walks_every_descendant_once_even_through_a_cycle() {
        let table = vec![(10, 1, "-zsh".into()), (11, 10, "/usr/bin/node".into()), (12, 11, "esbuild".into()), (13, 99, "other".into()), (14, 14, "loop".into())];
        let mut found = tree(&table, 10);
        found.sort();
        assert_eq!(found, [(11, "node".to_string()), (12, "esbuild".to_string())]);
    }

    #[test]
    fn ages_do_not_change_the_fingerprint_but_a_new_tool_does() {
        let tool = FocusTool { tool: "Bash".into(), label: "npm test".into(), age_ms: 10, ..Default::default() };
        let mut card = FocusCard { pane: "%1".into(), harness: "claude".into(), facts: Some(FocusFacts { tools: vec![tool.clone()], ..Default::default() }), ..Default::default() };
        let print = fingerprint(&card);
        card.facts.as_mut().unwrap().tools[0].age_ms = 9000;
        assert_eq!(fingerprint(&card), print);
        card.facts.as_mut().unwrap().tools.push(FocusTool { tool: "Read".into(), ..tool });
        assert_ne!(fingerprint(&card), print);
    }

    #[test]
    fn a_claude_card_shows_state_folder_model_context_ask_tools_background_and_ports() {
        let facts = FocusFacts {
            state: "permission".into(),
            tools: vec![FocusTool { tool: "Bash".into(), label: "npm test".into(), age_ms: 12_000, ..Default::default() }],
            recent: vec![FocusTool { tool: "Edit".into(), label: "a.rs".into(), took_ms: Some(200), error: true, ..Default::default() }],
            background: vec![FocusTask { kind: "shell".into(), label: "npm run dev".into(), status: "running".into(), age_ms: 180_000 }],
            context: Some(FocusContext { tokens: Some(124_000), window: Some(200_000), percent: Some(62.0) }),
            ask: Some(FocusAsk { tool: "Bash".into(), preview: "rm -rf build".into(), age_ms: 3000 }),
        };
        let card = FocusCard {
            pane: "%14".into(),
            harness: "claude".into(),
            shell: "zsh".into(),
            cwd: "/work/repo".into(),
            model: "claude-opus-5-5".into(),
            effort: "high".into(),
            facts: Some(facts),
            ports: vec![FocusPort { port: 5173, name: "node".into(), site: "Vite App".into() }],
            ..Default::default()
        };
        let seen = Seen { card: Some(card), ..Default::default() };
        let lines = card_lines(&seen, false);
        assert_eq!(head(&lines), ("Claude".into(), "%14".into()));
        let rows = rows(&lines);
        let names: Vec<&str> = rows.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["상태", "경로", "모델", "문맥", "승인", "도구", "최근", "백그라운드", "포트"]);
        assert_eq!(rows[0].1, "승인 기다림");
        assert_eq!(rows[2].1, "claude-opus-5-5 · high");
        assert_eq!(rows[3].1, "62% · 124k / 200k");
        assert_eq!((tokens(1_000_000), tokens(1_250_000)), ("1M".to_string(), "1.2M".to_string()));
        assert!(rows[4].1.starts_with("Bash · rm -rf build · "));
        assert_eq!(rows[5].1, "Bash · npm test · 12초");
        assert_eq!(rows[6].1, "Edit · a.rs · 0.2초 · 실패");
        assert_eq!(rows[7].1, "셸 · npm run dev · 3분");
        assert_eq!(rows[8].1, "5173 · node · Vite App");
        assert!(lines.iter().any(|l| matches!(l, info::ExecutionLine::Link { hit: Some(FocusHit::Port { port: 5173, host: None }), .. })));
        assert!(lines.iter().any(|l| matches!(l, info::ExecutionLine::Link { hit: Some(FocusHit::Folder(path)), .. } if path == "/work/repo")));
    }

    #[test]
    fn shells_name_what_runs_on_top_and_full_screen_programs_say_so() {
        let shell = |program: &str| Seen { card: Some(FocusCard { pane: "%2".into(), shell: "zsh".into(), program: program.into(), ..Default::default() }), ..Default::default() };
        assert_eq!(head(&card_lines(&shell(""), false)).0, "셸 · zsh");
        assert_eq!(head(&card_lines(&shell("cargo"), false)).0, "zsh · cargo 실행 중");
        assert_eq!(head(&card_lines(&shell("nvim"), true)).0, "전체 화면 · nvim");
        let codex = Seen { card: Some(FocusCard { harness: "codex".into(), ..Default::default() }), ..Default::default() };
        assert_eq!(head(&card_lines(&codex, false)).0, "Codex");
        let bare = Seen { card: Some(FocusCard { harness: "claude".into(), ..Default::default() }), ..Default::default() };
        assert!(card_lines(&bare, false).iter().any(|l| matches!(l, info::ExecutionLine::Kv { name, value: None, .. } if name == "도구")), "mod 없는 claude 칸은 까닭을 단다");
    }

    #[test]
    fn a_mirror_card_names_the_source_device_and_opens_ports_there() {
        let target = Some(Target::Remote {
            pane: "%9".into(),
            base: "http://100.64.0.7:8765".into(),
            remote_id: "%3".into(),
            machine_id: "m".into(),
            surface_key: Some("k".into()),
            label: "맥미니".into(),
        });
        let mut since = None;
        let answer = serde_json::json!({"schema": kasa_mcp::pane_info::SCHEMA, "ok": true, "seq": "4.ab", "card": {"pane": "%3", "shell": "zsh", "ports": [{"port": 3000, "name": "node", "site": ""}]}});
        let (seen, back) = remote_answer(&Ok(answer), &target, &mut since);
        assert_eq!(since.as_deref(), Some("4.ab"));
        assert_eq!(back, Duration::ZERO);
        let lines = card_lines(&seen, false);
        assert!(head(&lines).1.ends_with("· %3"));
        assert!(lines.iter().any(|l| matches!(l, info::ExecutionLine::Link { hit: Some(FocusHit::Port { port: 3000, host: Some(host) }), .. } if host == "100.64.0.7")));
        let (seen, back) = remote_answer(&Err("HTTP 404".into()), &target, &mut since);
        assert!(since.is_none() && back >= Duration::from_secs(60));
        assert!(matches!(&card_lines(&seen, false)[1], info::ExecutionLine::Text(text, true) if text.contains("옛 판")));
    }

    #[test]
    fn hosts_come_out_of_bases_with_or_without_ports() {
        assert_eq!(remote_host("http://100.64.0.7:8765").as_deref(), Some("100.64.0.7"));
        assert_eq!(remote_host("https://mini.local/relay").as_deref(), Some("mini.local"));
        assert_eq!(remote_host("http://[fd00::1]:8765").as_deref(), Some("fd00::1"));
    }
}
