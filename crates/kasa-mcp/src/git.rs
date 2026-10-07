//! Git status snapshot for the webview panel. We shell out to
//! `git status --porcelain=v2 --branch` rather than linking a git library:
//! porcelain v2 is the documented machine format git promises to keep
//! stable, and a subprocess can't pull a heavy libgit2 dependency or its
//! version skew into the host. The webview polls this over HTTP.

use std::path::Path;
use std::process::Command;

use serde_json::{json, Value};

/// Git 열 한 번 읽기의 한도. git 열 개를 돌리는데, 부하가 큰 맥(load 70~110)에서는 status 하나가 2.6초 걸려
/// 옛 5초 한도에 통째로 실패했다(2026-10-07, 워크트리 30개 레포). 넘기면 그 읽기만 버리고 화면은 마지막 결과를 지킨다.
pub const PANEL_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
const PANEL_OUTPUT_LIMIT: usize = 2 * 1024 * 1024;
const BRANCH_LIST_ARGS: &[&str] = &[
    "for-each-ref", "--format=%(refname)%00%(HEAD)%00%(symref)%00%(objectname)%00%(upstream)%00%(upstream:track)",
    "refs/heads", "refs/remotes",
];

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GitBranch {
    pub name: String,
    pub remote: bool,
    pub current: bool,
    #[serde(default)]
    pub oid: String,
    #[serde(default)]
    pub upstream: Option<String>,
    #[serde(default)]
    pub ahead: Option<u32>,
    #[serde(default)]
    pub behind: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GitGraphCommit {
    pub oid: String,
    pub parents: Vec<String>,
    pub subject: String,
    pub author: String,
    pub committed_at: i64,
    pub refs: Vec<String>,
}

struct GitReadOutput {
    success: bool,
    stdout: String,
    stderr: String,
}

impl GitReadOutput {
    fn checked(self) -> Result<String, String> {
        if self.success {
            Ok(self.stdout)
        } else {
            Err(format!("git failed: {}", self.stderr.trim()))
        }
    }
}

fn run_panel_git(
    repo: &Path,
    args: &[&str],
    deadline: std::time::Instant,
) -> Result<GitReadOutput, String> {
    use std::io::Read;
    use std::process::Stdio;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    if Instant::now() >= deadline {
        return Err("git panel snapshot timed out".into());
    }
    let mut command = git_cmd();
    // Inherited Git state must not redirect a pane's cwd to another repository.
    for variable in [
        "GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY", "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CONFIG_COUNT", "GIT_CONFIG_PARAMETERS", "GIT_NAMESPACE",
    ] {
        command.env_remove(variable);
    }
    command.args([
            "--no-optional-locks", "--no-pager", "-c", "core.fsmonitor=false",
            "-c", "core.hooksPath=/dev/null", "-c", "color.ui=false",
            "-c", "diff.relative=false", "-c", "maintenance.auto=false",
        ])
        .arg("-C").arg(repo).args(args)
        .env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_ALLOW_PROTOCOL", "")
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Submodule status may spawn git children that must share the deadline.
        command.process_group(0);
    }
    let mut child = command.spawn().map_err(|e| format!("git spawn failed: {e}"))?;
    let (tx, rx) = mpsc::channel();
    let pipes: [(Box<dyn Read + Send>, usize); 2] = [
        (Box::new(child.stdout.take().unwrap()), PANEL_OUTPUT_LIMIT),
        (Box::new(child.stderr.take().unwrap()), 64 * 1024),
    ];
    for (index, (pipe, limit)) in pipes.into_iter().enumerate() {
        let tx = tx.clone();
        // Both pipes must drain together or a full stderr pipe can block stdout.
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = pipe.take((limit + 1) as u64).read_to_end(&mut bytes)
                .map_err(|e| format!("git output read failed: {e}"))
                .and_then(|_| {
                    if bytes.len() > limit {
                        Err("git panel output exceeded its limit".to_string())
                    } else {
                        Ok(String::from_utf8_lossy(&bytes).into_owned())
                    }
                });
            let _ = tx.send((index, result));
        });
    }
    drop(tx);
    let result = (|| {
        let mut output: [Option<String>; 2] = [None, None];
        let mut status = None;
        loop {
            if Instant::now() >= deadline {
                return Err("git panel snapshot timed out".into());
            }
            while let Ok((index, result)) = rx.try_recv() {
                output[index] = Some(result?);
            }
            if status.is_none() {
                status = child.try_wait().map_err(|e| format!("git wait failed: {e}"))?;
            }
            if let Some(status) = status {
                if output.iter().all(Option::is_some) {
                    return Ok(GitReadOutput {
                        success: status.success(),
                        stdout: output[0].take().unwrap(),
                        stderr: output[1].take().unwrap(),
                    });
                }
            }
            std::thread::sleep(Duration::from_millis(5).min(deadline.saturating_duration_since(Instant::now())));
        }
    })();
    if result.is_err() {
        #[cfg(unix)]
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
        let _ = child.kill();
        // Reaping a killed process must not extend the request's deadline.
        std::thread::spawn(move || { let _ = child.wait(); });
    }
    result
}

fn parse_branch_list(text: &str) -> Vec<GitBranch> {
    text.lines().filter_map(|line| {
        let mut fields = line.split('\0');
        let reference = fields.next()?;
        let current = fields.next()? == "*";
        if !fields.next()?.is_empty() {
            return None;
        }
        let (name, remote) = if let Some(name) = reference.strip_prefix("refs/heads/") {
            (name, false)
        } else {
            (reference.strip_prefix("refs/remotes/")?, true)
        };
        let oid = fields.next().unwrap_or_default().to_string();
        let upstream = fields.next().filter(|name| !name.is_empty()).map(str::to_owned);
        let track = fields.next().unwrap_or_default();
        let (ahead, behind) = if upstream.is_some() && track != "[gone]" {
            let mut counts = (0, 0);
            for part in track.trim_matches(['[', ']']).split(", ") {
                if let Some(value) = part.strip_prefix("ahead ") { counts.0 = value.parse().unwrap_or(0); }
                if let Some(value) = part.strip_prefix("behind ") { counts.1 = value.parse().unwrap_or(0); }
            }
            (Some(counts.0), Some(counts.1))
        } else { (None, None) };
        (!name.is_empty()).then(|| GitBranch {
            name: name.into(), remote, current: current && !remote, oid, upstream, ahead, behind,
        })
    }).collect()
}

pub fn git_branch_list(repo: &Path) -> Vec<GitBranch> {
    run_panel_git(repo, BRANCH_LIST_ARGS, std::time::Instant::now() + PANEL_READ_TIMEOUT)
        .and_then(GitReadOutput::checked)
        .map(|out| parse_branch_list(&out))
        .unwrap_or_default()
}

fn parse_panel_status(repo: &Path, text: &str) -> Value {
    let mut branch = String::new();
    let mut head_oid = None;
    let mut ahead = 0u32;
    let mut behind = 0u32;
    let mut unborn = false;
    let mut staged = Vec::<(char, String)>::new();
    let mut unstaged = Vec::<(char, String)>::new();
    let mut records = text.split('\0');
    while let Some(record) = records.next() {
        if let Some(name) = record.strip_prefix("# branch.head ") {
            branch = name.to_string();
        } else if let Some(oid) = record.strip_prefix("# branch.oid ") {
            unborn = oid == "(initial)";
            head_oid = (!unborn).then(|| oid.to_string());
        } else if let Some(ab) = record.strip_prefix("# branch.ab ") {
            for count in ab.split_whitespace() {
                if let Some(n) = count.strip_prefix('+') {
                    ahead = n.parse().unwrap_or(0);
                } else if let Some(n) = count.strip_prefix('-') {
                    behind = n.parse().unwrap_or(0);
                }
            }
        } else if let Some(path) = record.strip_prefix("? ") {
            unstaged.push(('U', path.into()));
        } else {
            let skip = match record.as_bytes().first() {
                Some(b'1') => 8,
                Some(b'2') => { records.next(); 9 },
                Some(b'u') => 10,
                _ => continue,
            };
            let Some(path) = record.splitn(skip + 1, ' ').nth(skip) else { continue };
            let xy = record.split(' ').nth(1).unwrap_or("..").as_bytes();
            if xy.len() != 2 || path.is_empty() {
                continue;
            }
            if xy[0] != b'.' {
                staged.push((xy[0] as char, path.into()));
            }
            if xy[1] != b'.' {
                unstaged.push((xy[1] as char, path.into()));
            }
        }
    }
    json!({
        "cwd": repo.to_string_lossy(), "no_repo": false,
        "detached": branch == "(detached)", "unborn": unborn, "head_oid": head_oid,
        "branch": branch, "ahead": ahead, "behind": behind,
        "insertions": 0, "deletions": 0, "clean": staged.is_empty() && unstaged.is_empty(),
        "staged": staged, "unstaged": unstaged, "branches": [], "branch_list": [],
        "numstat": {}, "recent_commits": [], "repo_root": null,
        "commit_graph": [], "graph_supported": true, "graph_truncated": false,
    })
}

fn parse_panel_numstat(text: &str) -> std::collections::HashMap<String, (u32, u32)> {
    let mut result = std::collections::HashMap::new();
    let mut records = text.split('\0');
    while let Some(record) = records.next() {
        let mut fields = record.splitn(3, '\t');
        let Some(ins) = fields.next() else { continue };
        let Some(del) = fields.next() else { continue };
        let Some(mut path) = fields.next() else { continue };
        if path.is_empty() {
            // A renamed path occupies two extra NUL records; the second is current.
            records.next();
            let Some(destination) = records.next() else { break };
            path = destination;
        }
        if !path.is_empty() {
            result.insert(path.to_string(), (ins.parse().unwrap_or(0), del.parse().unwrap_or(0)));
        }
    }
    result
}

fn parse_panel_log(text: &str) -> Vec<(String, String)> {
    text.split('\0').filter_map(|record| {
        let (hash, subject) = record.split_once('\x1f')?;
        (!hash.is_empty()).then(|| (hash.to_string(), subject.to_string()))
    }).collect()
}

fn valid_graph_oid(oid: &str) -> bool {
    matches!(oid.len(), 40 | 64) && oid.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn graph_label(text: &str) -> String {
    text.chars().map(|character| if character.is_control() { ' ' } else { character }).collect()
}

fn parse_graph_log(text: &str, branches: &[GitBranch], head: Option<&str>) -> Result<Vec<GitGraphCommit>, String> {
    if text.is_empty() { return Ok(Vec::new()); }
    let fields: Vec<_> = text.strip_suffix('\0').ok_or("unterminated git graph record")?.split('\0').collect();
    if fields.len() % 5 != 0 { return Err("invalid git graph field count".into()); }
    let mut seen = std::collections::HashSet::new();
    fields.chunks_exact(5).map(|fields| {
        let oid = fields[0];
        let parents: Vec<_> = fields[1].split_whitespace().map(str::to_owned).collect();
        if !valid_graph_oid(oid) || !parents.iter().all(|parent| valid_graph_oid(parent)) || !seen.insert(oid) {
            return Err("invalid git graph object identity".into());
        }
        let mut refs = Vec::new();
        if head == Some(oid) { refs.push("HEAD".into()); }
        refs.extend(branches.iter().filter(|branch| branch.oid == oid).map(|branch| {
            format!("refs/{}/{}", if branch.remote { "remotes" } else { "heads" }, branch.name)
        }));
        Ok(GitGraphCommit {
            oid: oid.into(), parents, subject: graph_label(fields[4]), author: graph_label(fields[3]),
            committed_at: fields[2].parse().map_err(|_| "invalid git graph timestamp")?, refs,
        })
    }).collect()
}

fn graph_log_args(count: &str, head: bool) -> Vec<&str> {
    let mut args = vec!["log", count, "--topo-order", "--no-show-signature", "--no-decorate",
        "--format=%H%x00%P%x00%ct%x00%an%x00%s", "-z", "--branches", "--remotes"];
    if head { args.push("HEAD"); }
    args.push("--");
    args
}

/// 이 폴더가 속한 작업 트리의 뿌리. 저장소가 아니면 None.
pub fn panel_repo_root(repo: &Path) -> Option<std::path::PathBuf> {
    let root = run_panel_git(repo, &["rev-parse", "--show-toplevel"], std::time::Instant::now() + PANEL_READ_TIMEOUT).ok()?;
    let root = root.checked().ok()?;
    let root = root.trim_end_matches('\n');
    (!root.is_empty()).then(|| std::path::PathBuf::from(root))
}

pub fn git_panel_snapshot(repo: &Path, commits: usize) -> Result<Value, String> {
    git_panel_snapshot_within(repo, commits, PANEL_READ_TIMEOUT)
}

/// `budget` 안에 Git 열 재료를 다 읽는다. status 가 저장소인지 정한 뒤 나머지(브랜치·뿌리·numstat 셋·log·그래프)는
/// 서로 기다릴 것이 없어 한꺼번에 돌린다 — 기다림이 대부분인 느린 디스크·큰 부하에서 합이 아니라 가장 긴 하나만 걸린다.
pub fn git_panel_snapshot_within(repo: &Path, commits: usize, budget: std::time::Duration) -> Result<Value, String> {
    let deadline = std::time::Instant::now() + budget;
    let status = run_panel_git(repo, &[
        "status", "--porcelain=v2", "--branch", "-z", "--ignore-submodules=none",
    ], deadline)?;
    if !status.success && status.stderr.contains("not a git repository") {
        let mut view = parse_panel_status(repo, "");
        view["no_repo"] = json!(true);
        view["clean"] = json!(false);
        return Ok(view);
    }
    let mut view = parse_panel_status(repo, &status.checked()?);
    let unborn = view["unborn"].as_bool().unwrap_or(false);
    let head = view["head_oid"].as_str().map(str::to_owned);
    let diff_args = ["diff", "--no-ext-diff", "--no-textconv", "--ignore-submodules=none", "--numstat", "-z"];
    let staged_args = [&diff_args[..], &["--cached"]].concat();
    let head_args = [&diff_args[..], &["HEAD"]].concat();
    let log_count = format!("-{}", if commits == 0 { 5 } else { commits.min(100) });
    let log_args = ["log", log_count.as_str(), "--no-show-signature", "--format=%h%x1f%s", "-z"];
    let limit = commits.clamp(1, 200);
    let graph_count = format!("--max-count={}", limit + 1);
    let graph_args = graph_log_args(&graph_count, head.is_some());
    let read = |args: &[&str]| run_panel_git(repo, args, deadline).and_then(GitReadOutput::checked);
    let (branches, root, worktree, staged, totals, log, graph, shallow) = std::thread::scope(|scope| {
        let branches = scope.spawn(|| read(BRANCH_LIST_ARGS));
        let root = scope.spawn(|| run_panel_git(repo, &["rev-parse", "--show-toplevel"], deadline));
        let worktree = scope.spawn(|| read(&diff_args));
        let staged = scope.spawn(|| read(&staged_args));
        let totals = (!unborn).then(|| scope.spawn(|| read(&head_args)));
        let log = (!unborn).then(|| scope.spawn(|| read(&log_args)));
        // HEAD 가 없으면 그래프는 브랜치가 있을 때만 읽는다 — 아래에서 브랜치를 보고 따로.
        let graph = head.is_some().then(|| scope.spawn(|| read(&graph_args)));
        let shallow = scope.spawn(|| read(&["rev-parse", "--is-shallow-repository"]));
        let join = |handle: std::thread::ScopedJoinHandle<'_, Result<String, String>>| {
            handle.join().unwrap_or_else(|_| Err("git panel reader panicked".into()))
        };
        (
            join(branches),
            root.join().unwrap_or_else(|_| Err("git panel reader panicked".into())),
            join(worktree),
            join(staged),
            totals.map(join),
            log.map(join),
            graph.map(join),
            join(shallow),
        )
    });
    let branches = parse_branch_list(&branches?);
    view["branches"] = json!(branches.iter().filter(|b| !b.remote).map(|b| &b.name).collect::<Vec<_>>());
    view["branch_list"] = json!(&branches);
    let root = root?;
    if root.success {
        view["repo_root"] = json!(root.stdout.strip_suffix('\n').unwrap_or(&root.stdout));
    }
    let mut numstat = parse_panel_numstat(&worktree?);
    let staged = parse_panel_numstat(&staged?);
    for (path, (ins, del)) in &staged {
        let entry = numstat.entry(path.clone()).or_insert((0, 0));
        entry.0 = entry.0.max(*ins);
        entry.1 = entry.1.max(*del);
    }
    let totals = match totals {
        Some(totals) => parse_panel_numstat(&totals?),
        None => staged,
    };
    let (insertions, deletions) = totals.values().fold((0u32, 0u32), |(a, d), (ins, del)| {
        (a.saturating_add(*ins), d.saturating_add(*del))
    });
    view["insertions"] = json!(insertions);
    view["deletions"] = json!(deletions);
    view["numstat"] = json!(numstat);
    if let Some(log) = log {
        view["recent_commits"] = json!(parse_panel_log(&log?));
    }
    let graph = match graph {
        Some(graph) => Some(graph?),
        None if !branches.is_empty() => Some(read(&graph_args)?),
        None => None,
    };
    let (graph, truncated) = match graph {
        Some(output) => {
            let mut graph = parse_graph_log(&output, &branches, head.as_deref())?;
            // A shallow repository can look like a root even though its ancestry is unavailable locally.
            let truncated = graph.len() > limit || shallow?.trim() == "true";
            graph.truncate(limit);
            (graph, truncated)
        }
        None => (Vec::new(), false),
    };
    view["commit_graph"] = json!(graph);
    view["graph_truncated"] = json!(truncated);
    Ok(view)
}

/// `git` invocation with the console window suppressed on Windows. kasaterm
/// is a GUI (non-console) process, so spawning a console program like git
/// flashes a fresh console window — and a Defender-throttled call (~5s) leaves
/// that empty window on screen the whole time. CREATE_NO_WINDOW keeps it
/// hidden. No-op on other platforms.
///
/// PATH 는 도구 자리를 덧붙인 것으로 — Finder 로 띄운 앱은 `/usr/bin:/bin` 뿐이라 LFS 레포에서 status·diff 가
/// filter 로 부르는 `git-lfs` 를 못 찾고 rc 128 로 끝나, Git 열이 「읽지 못했어요」에 머물렀다(2026-10-07, 0.2.42).
fn git_cmd() -> Command {
    let mut command = crate::no_window_command("git");
    command.env("PATH", crate::reposync::tool_path());
    command
}

/// Run `git status` in `repo` and return a JSON snapshot the webview can
/// render directly. On any git failure returns `{ "error": "..." }` so the
/// caller never has to distinguish process vs. parse errors.
pub fn git_status(repo: &Path) -> Value {
    let output = match git_cmd()
        .arg("-C")
        .arg(repo)
        .args(["status", "--porcelain=v2", "--branch"])
        .output()
    {
        Ok(o) => o,
        Err(e) => return json!({ "error": format!("git spawn failed: {e}") }),
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let msg = stderr.trim();
        // "여긴 git 폴더 아님"은 실패가 아니라 정상 상태(홈·임의 폴더 등).
        // 빨간 에러 대신 패널이 부드러운 안내를 그리도록 별도 신호로 분리하고,
        // 안내에 띄울 축약 경로(홈은 ~)를 함께 넘긴다.
        if msg.contains("not a git repository") {
            return json!({ "no_repo": true, "path": display_path(repo) });
        }
        return json!({ "error": format!("git failed: {msg}") });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut v = parse_porcelain_v2(&stdout);
    // 사이드바 배지("+460 -59")는 변경 *라인* 수를 보여주는데 porcelain v2는
    // 파일 단위만 주므로 --shortstat을 한 번 더 돌려 끼워 넣는다.
    let (ins, del) = diff_line_stat(repo);
    v["insertions"] = json!(ins);
    v["deletions"] = json!(del);
    v
}

/// 사이드바 탭 git 배지(브랜치 + "+460 -59")용 경량 스냅샷. GUI가 윈도우마다
/// 1초 폴링으로 직접 호출하므로(데몬을 안 거침) 전체 `git_status`의 porcelain v2
/// 파싱 대신 rev-parse + shortstat 두 번만 돌려 호출당 비용을 줄인다.
#[derive(Clone, Debug, PartialEq)]
pub struct GitBadge {
    /// 파일트리 git 표시용 **절대경로 → 마커**(M 수정 · A 스테이지됨 · U 미추적).
    /// 변경된 파일뿐 아니라 그 조상 폴더까지 미리 펼쳐 둔다(`status_marks`) —
    /// 렌더는 행마다 조회만 하면 된다. 행마다 경로 접두어를 비교하면
    /// 트리 크기 × 변경 수가 매 프레임 돈다.
    ///
    /// 이 배지에 실은 이유: 배지 폴러는 **모든 pane 의 cwd 에 대해 항상** 도는데,
    /// git 컬럼 폴러는 그 패널이 열렸을 때만 돈다. 파일트리 표시가 남의 패널
    /// 개폐에 묶이면 안 된다.
    pub marks: std::collections::HashMap<std::path::PathBuf, char>,
    pub branch: String,
    /// Files changed vs HEAD (the leading `N` of `--shortstat`). Tracked-only,
    /// like insertions/deletions.
    pub files: u32,
    pub insertions: u32,
    pub deletions: u32,
}

/// `repo`가 git 워크트리면 배지 정보를, 아니면 `None`. 브랜치를 못 읽으면
/// (git repo 아님 등) 배지 자체를 숨기는 게 자연스러우므로 `None`을 돌린다.
pub fn git_badge(repo: &Path) -> Option<GitBadge> {
    // 브랜치와 레포 루트를 **한 번의 rev-parse** 로 같이 받는다 — 루트는 마커의
    // 상대경로를 절대경로로 펼 때 필요하고, 따로 부르면 폴링 주기마다 git 프로세스가
    // 하나 더 뜬다. 출력은 「브랜치\n루트」 두 줄.
    let (ok, head) = run_git(repo, &["rev-parse", "--abbrev-ref", "HEAD", "--show-toplevel"]);
    if !ok {
        return None;
    }
    let mut lines = head.lines();
    let branch = lines.next().unwrap_or("").trim().to_string();
    let toplevel = lines.next().map(|s| Path::new(s.trim()).to_path_buf());
    if branch.is_empty() {
        return None;
    }
    let marks = toplevel
        .map(|root| {
            // `--no-optional-locks` — status 는 평소 stat 캐시를 갱신하려 인덱스를
            // 잠근다. 1.5초마다 도는 폴러가 그 잠금을 잡으면 같은 레포에서 사람이
            // 치는 commit·add 가 `index.lock` 충돌로 죽는다. 읽기만 하면 되니 뺀다.
            let (_ok, st) = run_git(repo, &["--no-optional-locks", "status", "--porcelain=v1", "-z"]);
            status_marks(
                &root,
                parse_status_porcelain_z(&st).iter().map(|(m, p)| (*m, p.as_str())),
            )
        })
        .unwrap_or_default();
    let (_o, stat) = run_git(repo, &["--no-optional-locks", "diff", "HEAD", "--shortstat"]);
    let (insertions, deletions) = parse_shortstat(&stat);
    // Leading `N` of " 11 files changed, …" — 0 when the tree is clean.
    let files = stat
        .split_whitespace()
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    Some(GitBadge {
        marks,
        branch,
        files,
        insertions,
        deletions,
    })
}

/// 레포 하나의 자리 — 작업 트리 뿌리, 그 트리의 git 폴더, 브랜치들이 사는 공용 git 폴더
/// (워크트리면 둘이 다르다).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoPaths {
    pub root: std::path::PathBuf,
    pub git_dir: std::path::PathBuf,
    pub common_dir: std::path::PathBuf,
}

/// `dir` 이 git 작업 트리 안이면 그 자리를, 아니면 `None`.
pub fn repo_paths(dir: &Path) -> Option<RepoPaths> {
    let (ok, out) = run_git(
        dir,
        &["rev-parse", "--path-format=absolute", "--show-toplevel", "--git-dir", "--git-common-dir"],
    );
    if !ok {
        return None;
    }
    let mut lines = out.lines().map(|l| std::path::PathBuf::from(l.trim()));
    Some(RepoPaths { root: lines.next()?, git_dir: lines.next()?, common_dir: lines.next()? })
}

/// git 이 아는 상태(HEAD·인덱스·모든 ref·fetch)의 값싼 지문 — 파일 크기·수정 시각만 본다.
/// 커밋·스테이지·체크아웃·브랜치·fetch 는 이 값을 바꾸고, git 을 띄우는 것보다 수백 배 싸다.
/// **작업 트리 편집은 못 본다** — 그건 mod 깃 신호와 긴 주기가 맡는다. HEAD 를 못 읽으면
/// `None`(레포가 사라졌거나 옮겨졌다 — 자리부터 다시 찾을 때다).
pub fn repo_fingerprint(paths: &RepoPaths) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    fn stamp(h: &mut impl Hasher, path: &Path) {
        let meta = std::fs::symlink_metadata(path).ok();
        let modified = meta.as_ref().and_then(|m| m.modified().ok());
        (meta.map(|m| m.len()), modified).hash(h);
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let head = std::fs::read_to_string(paths.git_dir.join("HEAD")).ok()?;
    head.hash(&mut h);
    stamp(&mut h, &paths.git_dir.join("index"));
    stamp(&mut h, &paths.common_dir.join("packed-refs"));
    stamp(&mut h, &paths.common_dir.join("FETCH_HEAD"));
    // ref 는 파일 하나씩 바뀐다(잠금 파일을 만들어 이름을 바꾼다) — 폴더 시각은 바로 위
    // 폴더만 바뀌어 `refs/heads/aris/x` 같은 갈래를 놓치므로 파일을 다 훑는다. 브랜치가
    // 수백 개여도 stat 몇백 번이다.
    let mut stack = vec![paths.common_dir.join("refs")];
    let mut budget = 4096usize;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        let mut names: Vec<_> = entries.flatten().collect();
        names.sort_by_key(|e| e.file_name());
        for entry in names {
            if budget == 0 {
                break;
            }
            budget -= 1;
            let path = entry.path();
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(path);
            } else {
                path.hash(&mut h);
                stamp(&mut h, &path);
            }
        }
    }
    Some(h.finish())
}

/// 지문이 그대로이고 깃 신호도 없을 때 배지를 다시 읽는 주기 — 바깥 편집기나 mod 없는
/// 하네스가 고친 작업 트리는 이 주기 안에 잡힌다.
pub const BADGE_IDLE_PERIOD: std::time::Duration = std::time::Duration::from_secs(10);
/// git 레포가 아닌 폴더를 다시 확인하는 주기(그 사이 `git init` 했을 수 있다).
const NOT_A_REPO_RECHECK: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Default)]
struct BadgeRepo {
    fingerprint: Option<u64>,
    read_at: Option<std::time::Instant>,
    badge: Option<GitBadge>,
}

/// 사이드바·파일트리 git 배지 폴러의 기억. 배지 한 번이 git 셋(rev-parse·status·diff HEAD)이고
/// 큰 레포에서 CPU 330ms 남짓이라, 1.5초마다 폴더마다 다 돌리면 레포 하나가 코어의 20%를
/// 태웠다(2026-10-06 실측, 파일 4,861개). 그래서 ①같은 레포의 여러 폴더는 한 번만 읽고
/// ②git 이 아는 상태의 지문이 바뀌었거나, 그 레포를 건드린 mod 깃 신호가 왔거나,
/// `BADGE_IDLE_PERIOD` 가 지났을 때만 git 을 띄운다.
#[derive(Default)]
pub struct BadgePoller {
    cwds: std::collections::HashMap<std::path::PathBuf, (Option<RepoPaths>, std::time::Instant)>,
    repos: std::collections::HashMap<std::path::PathBuf, BadgeRepo>,
    seen: u64,
}

impl BadgePoller {
    pub fn poll(&mut self, cwds: &[std::path::PathBuf]) -> std::collections::HashMap<std::path::PathBuf, GitBadge> {
        let now = std::time::Instant::now();
        let (seq, signals, lost) = crate::claude_mod::git_signals_since(self.seen);
        self.seen = seq;
        let mut out = std::collections::HashMap::new();
        let mut roots = std::collections::HashSet::new();
        for cwd in cwds {
            let Some(paths) = self.resolve(cwd, now) else { continue };
            if roots.insert(paths.root.clone()) {
                let fingerprint = repo_fingerprint(&paths);
                if fingerprint.is_none() {
                    // 자리가 사라졌다 — 다음 바퀴에 다시 찾는다.
                    self.cwds.remove(cwd);
                }
                let touched =
                    lost || signals.iter().any(|s| crate::claude_mod::git_signal_touches(s, "", &paths.root));
                let repo = self.repos.entry(paths.root.clone()).or_default();
                let idle = repo.read_at.is_none_or(|at| now.duration_since(at) >= BADGE_IDLE_PERIOD);
                if fingerprint.is_none() || fingerprint != repo.fingerprint || touched || idle {
                    repo.badge = git_badge(&paths.root);
                    repo.fingerprint = fingerprint;
                    repo.read_at = Some(now);
                }
            }
            if let Some(badge) = self.repos.get(&paths.root).and_then(|r| r.badge.clone()) {
                out.insert(cwd.clone(), badge);
            }
        }
        self.repos.retain(|root, _| roots.contains(root));
        self.cwds.retain(|cwd, _| cwds.contains(cwd));
        out
    }

    fn resolve(&mut self, cwd: &Path, now: std::time::Instant) -> Option<RepoPaths> {
        match self.cwds.get(cwd) {
            Some((Some(paths), _)) => return Some(paths.clone()),
            Some((None, at)) if now.duration_since(*at) < NOT_A_REPO_RECHECK => return None,
            _ => {}
        }
        let paths = repo_paths(cwd);
        self.cwds.insert(cwd.to_path_buf(), (paths.clone(), now));
        paths
    }
}

/// `git status --porcelain=v1 -z` 출력 → `(마커, 레포 루트 상대경로)`. 순수 함수.
///
/// `-z` 를 쓰는 이유: 기본 출력은 공백·따옴표가 든 경로를 `"..."` 로 감싸고
/// 이스케이프해서, 그런 파일만 경로가 어긋나 표시가 조용히 빠진다. NUL 구분은
/// 경로를 날것 그대로 준다.
///
/// 마커는 git 컬럼이 쓰는 것과 같은 세 가지로 접는다 — 작업트리가 더러우면 `M`,
/// 인덱스만 바뀌었으면 `A`, 미추적이면 `U`. 이름변경/복사(R·C)는 **다음 레코드가
/// 원래 경로**라 한 칸 더 먹어야 한다. 안 먹으면 그 원본 경로가 다음 항목의
/// 상태 문자로 읽혀 그 뒤가 통째로 밀린다.
pub fn parse_status_porcelain_z(out: &str) -> Vec<(char, String)> {
    let mut rows = Vec::new();
    let mut it = out.split('\0');
    while let Some(rec) = it.next() {
        if rec.len() < 4 {
            continue;
        }
        let b = rec.as_bytes();
        let (x, y) = (b[0] as char, b[1] as char);
        let path = rec[3..].to_string();
        if x == 'R' || x == 'C' {
            it.next();
        }
        // 인덱스(x)·워크트리(y) 두 축을 한 글자로 접는다. 「스테이지 여부」가
        // 아니라 「무슨 일이 일어났나」로 갈라야 파일트리에서 쓸모가 있다 —
        // 스테이지된 수정(`M `)은 추가가 아니라 수정이다.
        let marker = if x == '?' || y == '?' {
            'U'
        } else if x == 'D' || y == 'D' {
            'D'
        } else if x == 'A' || x == 'R' || x == 'C' {
            'A'
        } else {
            'M'
        };
        rows.push((marker, path));
    }
    rows
}

/// HEAD 대비 작업트리의 추가/삭제 라인 수. 사이드바 git 배지("+460 -59")용.
/// 추적 파일만 집계한다(untracked 새 파일은 `--shortstat`에 안 잡힘) — 배지는
/// "이 repo가 HEAD에서 얼마나 벌어졌나"의 한눈 신호라 그 정도로 충분하다.
fn diff_line_stat(repo: &Path) -> (u32, u32) {
    let (_ok, out) = run_git(repo, &["diff", "HEAD", "--shortstat"]);
    parse_shortstat(&out)
}

/// `git diff --shortstat` 한 줄에서 추가/삭제 라인을 뽑는다. 형식:
/// ` 3 files changed, 460 insertions(+), 59 deletions(-)`. insertions나
/// deletions 한쪽이 빠질 수 있으니(추가만/삭제만) 각각 독립적으로 찾는다.
fn parse_shortstat(text: &str) -> (u32, u32) {
    let num_before = |kw: &str| -> u32 {
        text.find(kw).and_then(|pos| {
            text[..pos]
                .rsplit(',')
                .next()
                .and_then(|seg| seg.split_whitespace().next())
                .and_then(|n| n.parse().ok())
        })
        .unwrap_or(0)
    };
    (num_before("insertion"), num_before("deletion"))
}

/// Parse the porcelain v2 + --branch stream. Header lines start with `# `;
/// entry lines start with `1`/`2` (tracked changes), `u` (unmerged), or
/// `?` (untracked). See `git status --help` "Porcelain Format Version 2".
fn parse_porcelain_v2(text: &str) -> Value {
    let mut branch = String::new();
    let mut ahead: u32 = 0;
    let mut behind: u32 = 0;
    let mut staged: Vec<String> = Vec::new();
    let mut modified: Vec<String> = Vec::new();
    let mut untracked: Vec<String> = Vec::new();

    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# ") {
            if let Some(name) = rest.strip_prefix("branch.head ") {
                branch = name.to_string();
            } else if let Some(ab) = rest.strip_prefix("branch.ab ") {
                // Format: "+<ahead> -<behind>".
                for tok in ab.split_whitespace() {
                    if let Some(n) = tok.strip_prefix('+') {
                        ahead = n.parse().unwrap_or(0);
                    } else if let Some(n) = tok.strip_prefix('-') {
                        behind = n.parse().unwrap_or(0);
                    }
                }
            }
            continue;
        }

        if let Some(rest) = line.strip_prefix("? ") {
            untracked.push(rest.to_string());
            continue;
        }

        // Ordinary (1) and renamed/copied (2) entries carry an XY status
        // field: X = staged (index) state, Y = worktree state. A '.' means
        // unmodified on that side.
        let kind = line.chars().next();
        if kind == Some('1') || kind == Some('2') {
            let mut fields = line.split(' ');
            let _ = fields.next(); // "1" | "2"
            let xy = fields.next().unwrap_or("..");
            let path = entry_path(line, kind == Some('2'));
            let mut chars = xy.chars();
            let x = chars.next().unwrap_or('.');
            let y = chars.next().unwrap_or('.');
            if x != '.' {
                staged.push(path.clone());
            }
            if y != '.' {
                modified.push(path);
            }
        }
        // 'u' (unmerged) and '!' (ignored) are intentionally not surfaced
        // in Phase 1; the panel only shows the everyday staged/modified/new
        // buckets.
    }

    let clean = staged.is_empty() && modified.is_empty() && untracked.is_empty();
    json!({
        "branch": branch,
        "ahead": ahead,
        "behind": behind,
        "staged": staged,
        "modified": modified,
        "untracked": untracked,
        "clean": clean,
    })
}

/// Render a cwd for the "not a repo" notice: collapse the home prefix to
/// `~` so the panel shows `~` or `~/Desktop` instead of a long absolute path.
fn display_path(p: &Path) -> String {
    let full = p.display().to_string();
    if let Ok(home) = kasa_socket::home_var() {
        if full == home {
            return "~".to_string();
        }
        if let Some(rest) = full.strip_prefix(&format!("{home}/")) {
            return format!("~/{rest}");
        }
    }
    full
}

/// Extract the path from a `1`/`2` entry line. Paths can contain spaces, so
/// we skip the fixed leading fields rather than split blindly. A `2` entry
/// appends `<tab><origPath>` after a rename score field — we keep only the
/// current path (before the tab).
fn entry_path(line: &str, renamed: bool) -> String {
    // Field counts before the path (per porcelain v2 spec):
    //   1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>            -> 8 fields
    //   2 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <Xscore> <path>   -> 9 fields
    let skip = if renamed { 9 } else { 8 };
    let path = line.splitn(skip + 1, ' ').nth(skip).unwrap_or("");
    // Rename entries put the original path after a tab; drop it.
    path.split('\t').next().unwrap_or(path).to_string()
}

/// `git -C <repo> <args>` 실행 → (성공여부, stdout(+실패 시 stderr)).
/// status 계열과 달리 diff/commit/push는 출력 텍스트를 그대로 패널에
/// 돌려줘야 하므로 별도 헬퍼로 묶는다.
fn run_git(repo: &Path, args: &[&str]) -> (bool, String) {
    match git_cmd().arg("-C").arg(repo).args(args).output() {
        Ok(o) => {
            let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
            if !o.status.success() {
                let err = String::from_utf8_lossy(&o.stderr);
                if !err.trim().is_empty() {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(err.trim());
                }
            }
            (o.status.success(), text)
        }
        Err(e) => (false, format!("git spawn failed: {e}")),
    }
}

/// 한 파일의 diff. HEAD 대비(staged+unstaged 통합)를 우선 보고, 비어 있으면
/// untracked 새 파일로 보고 `/dev/null` 대비 전체를 added로 표시한다.
pub fn git_diff(repo: &Path, path: &str) -> Value {
    let (_ok, text) = run_git(repo, &["diff", "HEAD", "--", path]);
    let diff = if text.trim().is_empty() {
        // untracked: --no-index는 차이가 있으면 exit 1이라 성공여부는 무시.
        let (_o, t) = run_git(repo, &["diff", "--no-index", "--", "/dev/null", path]);
        t
    } else {
        text
    };
    json!({ "path": path, "diff": diff })
}

/// 체크된 파일만 정확히 커밋. 기존 staging을 비운 뒤(mixed reset — 작업 내용은
/// 유지) 지정 파일만 add하고 commit. 빈 목록/메시지는 거부한다.
pub fn git_commit(repo: &Path, files: &[String], message: &str) -> Value {
    if files.is_empty() {
        return json!({ "ok": false, "output": "커밋할 파일이 선택되지 않았습니다" });
    }
    if message.trim().is_empty() {
        return json!({ "ok": false, "output": "커밋 메시지가 비어 있습니다" });
    }
    // 체크된 파일만 정확히 들어가도록 staging을 한 번 비운다.
    let _ = run_git(repo, &["reset", "-q"]);
    let mut add_args: Vec<&str> = vec!["add", "--"];
    for f in files {
        add_args.push(f.as_str());
    }
    let (add_ok, add_out) = run_git(repo, &add_args);
    if !add_ok {
        return json!({ "ok": false, "output": format!("add 실패: {add_out}") });
    }
    let (ok, out) = run_git(repo, &["commit", "-m", message]);
    json!({ "ok": ok, "output": out.trim() })
}

/// 패널 입력칸의 메시지로 작업트리 전체를 커밋(`git add -A` → `git commit -m`).
/// VSCode 식 "전부 stage 하고 커밋" — 선택 stage 없이 한 번에. 빈 메시지는 거부.
pub fn git_commit_all(repo: &Path, message: &str) -> Value {
    if message.trim().is_empty() {
        return json!({ "ok": false, "output": "커밋 메시지가 비어 있습니다" });
    }
    let (add_ok, add_out) = run_git(repo, &["add", "-A"]);
    if !add_ok {
        return json!({ "ok": false, "output": add_out.trim() });
    }
    let (ok, out) = run_git(repo, &["commit", "-m", message]);
    json!({ "ok": ok, "output": out.trim() })
}

/// 이미 index 에 올라간(staged) 변경만 커밋(`git commit -m`, add 없음). VSCode
/// 처럼 "Staged Changes 만 커밋" — 패널이 staged 비었는지 검사하므로 여기선
/// 빈 메시지만 막는다(git 이 staged 없으면 알아서 실패 메시지를 돌려준다).
pub fn git_commit_staged(repo: &Path, message: &str) -> Value {
    if message.trim().is_empty() {
        return json!({ "ok": false, "output": "커밋 메시지가 비어 있습니다" });
    }
    let (ok, out) = run_git(repo, &["commit", "-m", message]);
    json!({ "ok": ok, "output": out.trim() })
}

/// 한 파일을 stage (`git add -- <path>`). 패널 Changes 행의 + 버튼.
pub fn git_add_path(repo: &Path, path: &str) -> Value {
    let (ok, out) = run_git(repo, &["add", "--", path]);
    json!({ "ok": ok, "output": out.trim() })
}

/// 한 파일을 unstage (`git reset -q HEAD -- <path>`). 패널 Staged 행의 - 버튼.
/// 워킹트리는 건드리지 않고 index 에서만 내린다.
pub fn git_unstage_path(repo: &Path, path: &str) -> Value {
    let (ok, out) = run_git(repo, &["reset", "-q", "HEAD", "--", path]);
    json!({ "ok": ok, "output": out.trim() })
}

/// `git push`. 결과 텍스트를 그대로 패널에 돌려준다.
pub fn git_push(repo: &Path) -> Value {
    let (ok, out) = run_git(repo, &["push"]);
    json!({ "ok": ok, "output": out.trim() })
}

/// `git pull`. behind 커밋을 받아온다. fast-forward면 조용히 합쳐지고,
/// 갈라졌으면 git이 merge 커밋을 만들거나 충돌을 output에 보고한다(우리가
/// stash/force 하지 않는다 — push와 대칭). 패널은 poller 다음 틱에 repaint.
pub fn git_pull(repo: &Path) -> Value {
    let (ok, out) = run_git(repo, &["pull"]);
    json!({ "ok": ok, "output": out.trim() })
}

/// 최근 커밋 `n`개를 `[{ "hash": "abc1234", "subject": "..." }]` 로. 패널의
/// "최근 커밋" 미리보기용. repo가 아니거나 커밋이 없으면 빈 배열.
pub fn git_log(repo: &Path, n: u32) -> Value {
    let arg = format!("-{n}");
    let (ok, out) = run_git(repo, &["log", &arg, "--pretty=format:%h\x1f%s"]);
    if !ok {
        return json!([]);
    }
    let commits: Vec<Value> = out
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(2, '\x1f');
            let hash = parts.next()?.trim();
            if hash.is_empty() {
                return None;
            }
            let subject = parts.next().unwrap_or("").trim();
            Some(json!({ "hash": hash, "subject": subject }))
        })
        .collect();
    json!(commits)
}

/// 한 커밋이 바꾼 파일들 `[(path, additions, deletions)]`. 커밋 더블클릭 시
/// 인라인으로 펼치는 변경 파일 목록용. `--format=` 로 커밋 메타를 죽이고
/// `--numstat` 만 받는다. binary 파일은 numstat 가 `-` 라 (path, 0, 0).
pub fn git_commit_files(repo: &Path, hash: &str) -> Vec<(String, u32, u32)> {
    let (ok, out) = run_git(repo, &["show", "--numstat", "--format=", hash]);
    if !ok {
        return Vec::new();
    }
    out.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            let mut parts = line.splitn(3, '\t');
            let a = parts.next()?;
            let d = parts.next()?;
            let path = parts.next()?.to_string();
            Some((path, a.parse().unwrap_or(0), d.parse().unwrap_or(0)))
        })
        .collect()
}

/// 한 커밋 안 특정 파일의 unified diff. 커밋 펼침 안에서 파일을 다시 펼칠 때의
/// 인라인 diff — `git_file_diff` 와 같은 `DiffLine`. `parse_unified_diff` 가 파일
/// 헤더를 버리므로 `git show` 출력을 그대로(선두 공백만 잘라) 넘긴다.
pub fn git_commit_file_diff(repo: &Path, hash: &str, path: &str) -> Vec<DiffLine> {
    let (ok, out) = run_git(repo, &["show", "--format=", hash, "--", path]);
    let _ = ok;
    parse_unified_diff(out.trim_start())
}

/// 로컬 브랜치 이름 목록. 패널의 브랜치 전환 드롭다운용. repo가 아니거나
/// 브랜치가 없으면 빈 Vec. 현재 브랜치 표시는 호출부가 `git_status`의
/// branch와 비교해서 한다(여기선 순수 목록만).
pub fn git_branches(repo: &Path) -> Vec<String> {
    let (ok, out) = run_git(repo, &["branch", "--format=%(refname:short)"]);
    if !ok {
        return Vec::new();
    }
    out.lines()
        .map(str::trim)
        // detached HEAD는 빈 줄/"(HEAD …)"로 나올 수 있어 거른다.
        .filter(|l| !l.is_empty() && !l.starts_with("(HEAD") && *l != "HEAD")
        .map(String::from)
        .collect()
}

/// `branch`로 전환 (`git checkout`). dirty 작업트리면 git이 명확한 메시지로
/// 거부하는데, stash/force 하지 않고 그 메시지를 그대로 돌려준다 — 사용자가
/// 모르는 사이 작업이 stash로 숨겨지는 일이 없도록.
pub fn git_checkout(repo: &Path, branch: &str) -> Value {
    let (ok, out) = run_git(repo, &["checkout", branch]);
    json!({ "ok": ok, "output": out.trim() })
}

/// Discard a file's worktree changes (status-bar / git-panel ↩ button). Tracked
/// files are restored to HEAD (`checkout --`); an untracked file is removed.
pub fn git_discard_path(repo: &Path, path: &str, untracked: bool) -> Value {
    if untracked {
        let ok = std::fs::remove_file(repo.join(path)).is_ok();
        return json!({ "ok": ok, "output": "" });
    }
    let (ok, out) = run_git(repo, &["checkout", "--", path]);
    json!({ "ok": ok, "output": out.trim() })
}

/// Per-file `(insertions, deletions)` vs HEAD, keyed by path — both worktree
/// (`--numstat`) and index (`--cached`) merged (max each side) so a file shows a
/// count whichever side it changed on. Binary files (`-\t-`) count as 0.
pub fn git_numstat(repo: &Path) -> std::collections::HashMap<String, (u32, u32)> {
    let mut m: std::collections::HashMap<String, (u32, u32)> = std::collections::HashMap::new();
    for args in [
        ["diff", "--numstat"].as_slice(),
        ["diff", "--cached", "--numstat"].as_slice(),
    ] {
        let (ok, out) = run_git(repo, args);
        if !ok {
            continue;
        }
        for line in out.lines() {
            let mut parts = line.split('\t');
            let ins: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let del: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            if let Some(path) = parts.next() {
                let e = m.entry(path.to_string()).or_insert((0, 0));
                e.0 = e.0.max(ins);
                e.1 = e.1.max(del);
            }
        }
    }
    m
}

/// Subset of `paths` that git ignores, as a set of the same strings passed in.
/// One `git check-ignore --stdin` call — paths fed on stdin, the ignored ones
/// echoed back verbatim — so a whole file-tree level costs a single process.
/// `.git` is never matched by check-ignore, so callers italicize dotfiles
/// separately. Empty set on any failure (non-repo, git missing).
pub fn git_ignored(repo: &Path, paths: &[String]) -> std::collections::HashSet<String> {
    use std::io::Write;
    use std::process::Stdio;
    let mut set = std::collections::HashSet::new();
    if paths.is_empty() {
        return set;
    }
    let mut child = match git_cmd()
        .arg("-C")
        .arg(repo)
        .args(["check-ignore", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return set,
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(paths.join("\n").as_bytes());
        // stdin dropped here → EOF, so git stops waiting for more paths.
    }
    let out = match child.wait_with_output() {
        Ok(o) => o,
        Err(_) => return set,
    };
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        set.insert(line.to_string());
    }
    set
}

/// One row of a unified diff, line-numbered for the gutter. `Hunk` is the
/// `@@ … @@` separator (carries no line numbers); `Context`/`Add`/`Del` carry
/// the side(s) they belong to.
#[derive(Clone, Debug, PartialEq)]
pub enum DiffLineKind {
    Hunk,
    Context,
    Add,
    Del,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
    pub text: String,
}

/// Unified diff of one file for the git panel's inline expander. `staged` picks
/// the index diff (`--cached`) vs the worktree diff. An untracked file has no
/// tracked diff, so we fall back to `--no-index` against /dev/null to render the
/// whole file as additions.
pub fn git_file_diff(repo: &Path, path: &str, staged: bool) -> Vec<DiffLine> {
    let (ok, out) = if staged {
        run_git(repo, &["diff", "--cached", "--", path])
    } else {
        let (ok, out) = run_git(repo, &["diff", "--", path]);
        if ok && out.trim().is_empty() {
            // Likely untracked — show every line as an addition.
            run_git(repo, &["diff", "--no-index", "--", "/dev/null", path])
        } else {
            (ok, out)
        }
    };
    let _ = ok;
    parse_unified_diff(&out)
}

/// HEAD 시점의 파일 본문. 편집기 거터가 「지금 버퍼 ↔ 이것」을 메모리에서 떠
/// 실시간 diff 를 만든다.
///
/// `git_file_diff` 로는 그걸 못 한다 — 그쪽은 **디스크**를 보므로 저장 전 버퍼를
/// 모른다. 여기서 원본만 받아 오고 차이는 `gitdiff` 가 낸다.
///
/// HEAD 에 없으면(미추적·새 파일·레포 아님) `None`. 그때 편집기는 표시를 아예
/// 안 그린다 — 온 줄이 초록인 화면은 아무것도 알려주지 않는다.
pub fn git_head_text(repo: &Path, rel: &str) -> Option<String> {
    // `--` 로 갈라야 `HEAD:foo` 를 리비전이 아니라 경로로 읽는 사고가 안 난다.
    let (ok, out) = run_git(repo, &["show", &format!("HEAD:{rel}"), "--"]);
    ok.then_some(out)
}

/// Parse `git diff` output into line-numbered rows. File headers (`diff`,
/// `index`, `+++`, `---`, `new file`, …) are dropped; only hunks + body lines
/// survive. Line numbers track from each hunk header's `@@ -old +new @@`.
fn parse_unified_diff(text: &str) -> Vec<DiffLine> {
    let mut rows = Vec::new();
    let mut old_no = 0u32;
    let mut new_no = 0u32;
    for line in text.lines() {
        if line.starts_with("@@") {
            // @@ -<old>[,n] +<new>[,n] @@ …
            let nums = |seg: &str| -> u32 {
                seg.trim_start_matches(['-', '+'])
                    .split(',')
                    .next()
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(1)
            };
            let mut parts = line.split_whitespace();
            let _ = parts.next(); // "@@"
            if let Some(o) = parts.next() {
                old_no = nums(o);
            }
            if let Some(n) = parts.next() {
                new_no = nums(n);
            }
            rows.push(DiffLine {
                kind: DiffLineKind::Hunk,
                old_no: None,
                new_no: None,
                text: line.to_string(),
            });
            continue;
        }
        if line.starts_with("diff ")
            || line.starts_with("index ")
            || line.starts_with("--- ")
            || line.starts_with("+++ ")
            || line.starts_with("new file")
            || line.starts_with("deleted file")
            || line.starts_with("old mode")
            || line.starts_with("new mode")
            || line.starts_with("similarity")
            || line.starts_with("rename ")
            || line.starts_with("\\ No newline")
        {
            continue;
        }
        if let Some(rest) = line.strip_prefix('+') {
            rows.push(DiffLine {
                kind: DiffLineKind::Add,
                old_no: None,
                new_no: Some(new_no),
                text: rest.to_string(),
            });
            new_no += 1;
        } else if let Some(rest) = line.strip_prefix('-') {
            rows.push(DiffLine {
                kind: DiffLineKind::Del,
                old_no: Some(old_no),
                new_no: None,
                text: rest.to_string(),
            });
            old_no += 1;
        } else {
            let rest = line.strip_prefix(' ').unwrap_or(line);
            rows.push(DiffLine {
                kind: DiffLineKind::Context,
                old_no: Some(old_no),
                new_no: Some(new_no),
                text: rest.to_string(),
            });
            old_no += 1;
            new_no += 1;
        }
    }
    rows
}

/// 마커의 우선순위 — 폴더가 자손들의 상태를 하나로 물려받을 때 쓴다.
/// 수정 > 스테이지됨 > 미추적. 폴더 하나에 색이 하나뿐이라 "가장 알려야 할 것"을
/// 고르는데, 이미 손댄 파일(M)이 새로 생긴 파일(U)보다 먼저 눈에 띄어야 한다.
pub fn mark_rank(m: char) -> u8 {
    match m {
        'M' => 4,
        'D' => 3,
        'A' => 2,
        'U' => 1,
        _ => 0,
    }
}

/// `(마커, 레포 루트 상대경로)` 목록을 **절대경로 → 마커** 맵으로 펼친다.
///
/// 파일만이 아니라 **조상 폴더까지** 같은 맵에 채우는 게 요점이다 — 접힌 폴더
/// 안에 변경이 있어도 트리에서 아무 표시가 없으면, 무엇이 바뀌었는지 보려고
/// 폴더를 하나씩 펼쳐 봐야 한다. 폴더는 자손 중 가장 높은 순위를 물려받는다.
///
/// 루트 자신도 포함한다. 트리가 레포보다 위에서 시작할 때(예: 상위 폴더를 열어
/// 둔 경우) 레포 폴더 자체에도 표시가 붙어야 한눈에 보인다.
pub fn status_marks<'a>(
    root: &Path,
    entries: impl IntoIterator<Item = (char, &'a str)>,
) -> std::collections::HashMap<std::path::PathBuf, char> {
    let mut out: std::collections::HashMap<std::path::PathBuf, char> =
        std::collections::HashMap::new();
    let mut put = |p: std::path::PathBuf, m: char| {
        match out.entry(p) {
            std::collections::hash_map::Entry::Occupied(mut e) => {
                if mark_rank(m) > mark_rank(*e.get()) {
                    e.insert(m);
                }
            }
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(m);
            }
        };
    };
    for (marker, rel) in entries {
        let rel = rel.trim_end_matches('/');
        if rel.is_empty() {
            continue;
        }
        let abs = root.join(rel);
        put(abs.clone(), marker);
        // 조상 폴더로 롤업. 루트에서 멈추되 루트 자신은 포함한다.
        let mut cur = abs.parent();
        while let Some(dir) = cur {
            put(dir.to_path_buf(), marker);
            if dir == root {
                break;
            }
            cur = dir.parent();
        }
    }
    out
}

#[cfg(test)]
mod panel_snapshot_tests {
    #[test]
    fn panel_git_finds_the_lfs_filter_when_the_app_has_a_bare_path() {
        let command = super::git_cmd();
        let path = command.get_envs().find(|(key, _)| *key == "PATH").and_then(|(_, value)| value);
        assert_eq!(path.and_then(|p| p.to_str()), Some(crate::reposync::tool_path()));
    }

    use super::*;

    #[test]
    fn porcelain_preserves_paths_and_both_status_columns() {
        let text = concat!(
            "# branch.oid abc123\0# branch.head topic\0# branch.ab +4 -2\0",
            "1 MM N... 100644 100644 100644 aaa bbb dir/a b\t\"한글\"\n.rs\0",
            "2 R. N... 100644 100644 100644 aaa bbb R100 moved\tfile.rs\0",
            "? old name\0",
            "1 .D N... 100644 100644 000000 aaa aaa deleted.rs\0",
            "? new\nfile.rs\0",
        );
        let view = parse_panel_status(Path::new("/repo/sub"), text);
        assert_eq!(view["cwd"], "/repo/sub");
        assert_eq!(view["branch"], "topic");
        assert_eq!(view["ahead"], 4);
        assert_eq!(view["behind"], 2);
        assert_eq!(view["staged"], json!([
            ["M", "dir/a b\t\"한글\"\n.rs"], ["R", "moved\tfile.rs"],
        ]));
        assert_eq!(view["unstaged"], json!([
            ["M", "dir/a b\t\"한글\"\n.rs"], ["D", "deleted.rs"], ["U", "new\nfile.rs"],
        ]));
        assert_eq!(view["clean"], false);
        assert_eq!(view["detached"], false);
        assert_eq!(view["unborn"], false);
        assert_eq!(view["head_oid"], "abc123");
    }

    #[test]
    fn conflicts_are_not_misreported_as_clean() {
        let text = "# branch.head main\0u UU N... 100644 100644 100644 100644 aaa bbb ccc conflicted file\0";
        let view = parse_panel_status(Path::new("/repo"), text);
        assert_eq!(view["staged"], json!([["U", "conflicted file"]]));
        assert_eq!(view["unstaged"], json!([["U", "conflicted file"]]));
        assert_eq!(view["clean"], false);
    }

    #[test]
    fn submodule_head_and_worktree_changes_remain_visible() {
        let view = parse_panel_status(Path::new("/repo"), concat!(
            "# branch.head main\0",
            "1 .M S.MU 160000 160000 160000 aaa aaa nested worktree\0",
            "1 M. SC.. 160000 160000 160000 aaa bbb nested head\0",
        ));
        assert_eq!(view["staged"], json!([["M", "nested head"]]));
        assert_eq!(view["unstaged"], json!([["M", "nested worktree"]]));
        assert_eq!(view["clean"], false);
    }

    #[test]
    fn detached_and_unborn_have_distinct_head_metadata() {
        let detached = parse_panel_status(Path::new("/repo"), "# branch.oid abc123\0# branch.head (detached)\0");
        assert_eq!(detached["detached"], true);
        assert_eq!(detached["unborn"], false);
        assert_eq!(detached["clean"], true);
        assert_eq!(detached["head_oid"], "abc123");
        let unborn = parse_panel_status(Path::new("/repo"), "# branch.oid (initial)\0# branch.head new-main\0");
        assert_eq!(unborn["detached"], false);
        assert_eq!(unborn["unborn"], true);
        assert_eq!(unborn["head_oid"], Value::Null);
        assert_eq!(unborn["branch"], "new-main");
        assert_eq!(unborn["branch_list"], json!([]));
    }

    #[test]
    fn branches_include_remotes_but_exclude_symbolic_aliases() {
        let branches = parse_branch_list(concat!(
            "refs/heads/main\0*\0\n",
            "refs/heads/origin/main\0 \0\n",
            "refs/remotes/origin/main\0 \0\n",
            "refs/remotes/origin/HEAD\0 \0refs/remotes/origin/main\n",
            "refs/tags/v1\0 \0\n",
        ));
        assert_eq!(branches, vec![
            GitBranch { name: "main".into(), remote: false, current: true, ..Default::default() },
            GitBranch { name: "origin/main".into(), remote: false, current: false, ..Default::default() },
            GitBranch { name: "origin/main".into(), remote: true, current: false, ..Default::default() },
        ]);
        let legacy: GitBranch = serde_json::from_value(json!({"name": "main", "remote": false, "current": true})).unwrap();
        assert_eq!(legacy, branches[0]);
    }

    #[test]
    fn numstat_preserves_rename_destination_and_binary_paths() {
        let stats = parse_panel_numstat("3\t2\tfile\twith\nspaces\0-\t-\timage.png\01\t0\t\0old\nname\0new\tname\0");
        assert_eq!(stats.get("file\twith\nspaces"), Some(&(3, 2)));
        assert_eq!(stats.get("image.png"), Some(&(0, 0)));
        assert_eq!(stats.get("new\tname"), Some(&(1, 0)));
        assert!(!stats.contains_key("old\nname"));
        assert_eq!(stats.len(), 3);
    }

    #[test]
    fn log_subject_can_contain_the_field_separator() {
        assert_eq!(parse_panel_log("a123\x1fsubject\x1fextra\0b456\x1fnext\0"), vec![
            ("a123".into(), "subject\x1fextra".into()), ("b456".into(), "next".into()),
        ]);
        assert!(parse_panel_log("").is_empty());
    }

    #[test]
    fn branch_metadata_keeps_upstream_identity_and_unknown_tracking_distinct() {
        let branches = parse_branch_list(concat!(
            "refs/heads/한글\0*\0\0aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\0refs/remotes/origin/한글\0[ahead 3, behind 2]\n",
            "refs/heads/gone\0 \0\0bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\0refs/remotes/origin/gone\0[gone]\n",
            "refs/heads/no-upstream\0 \0\0cccccccccccccccccccccccccccccccccccccccc\0\0\n",
        ));
        assert_eq!(branches[0].upstream.as_deref(), Some("refs/remotes/origin/한글"));
        assert_eq!((branches[0].ahead, branches[0].behind), (Some(3), Some(2)));
        assert_eq!((branches[1].ahead, branches[1].behind), (None, None));
        assert!(branches[1].upstream.is_some());
        assert!(branches[2].upstream.is_none());
    }

    #[test]
    fn graph_fields_preserve_unicode_and_never_treat_display_controls_as_structure() {
        let oid = "a".repeat(40);
        let parent = "b".repeat(40);
        let text = format!("{oid}\0{parent}\01700000000\0작성자\x1f이름\0한글\x1f제목\n둘째\t줄\0");
        let branches = vec![
            GitBranch { name: "origin/main".into(), oid: oid.clone(), ..Default::default() },
            GitBranch { name: "origin/main".into(), oid: oid.clone(), remote: true, ..Default::default() },
        ];
        let graph = parse_graph_log(&text, &branches, Some(&oid)).unwrap();
        assert_eq!(graph[0].parents, [parent]);
        assert_eq!(graph[0].subject, "한글 제목 둘째 줄");
        assert_eq!(graph[0].author, "작성자 이름");
        assert_eq!(graph[0].refs, ["HEAD", "refs/heads/origin/main", "refs/remotes/origin/main"]);
        assert!(parse_graph_log(&text.replace("1700000000", "bad"), &[], None).is_err());
        assert!(parse_graph_log(&text.replace(&oid, "not-an-object"), &[], None).is_err());
        assert!(parse_graph_log(&format!("{text}{text}"), &[], None).is_err());
        assert!(parse_graph_log(&text.replace("제목", "제\0목"), &[], None).is_err());
    }

    struct GraphRepo(std::path::PathBuf);

    impl GraphRepo {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let repo = Self(std::env::temp_dir().join(format!("kasa-git-graph-{}-{sequence}", std::process::id())));
            std::fs::create_dir(&repo.0).unwrap();
            repo.git(&["init", "-q", "--initial-branch=main"]);
            repo
        }

        fn git(&self, args: &[&str]) -> String {
            let output = git_cmd().arg("-C").arg(&self.0).args([
                "-c", "core.hooksPath=/dev/null", "-c", "commit.gpgSign=false",
                "-c", "user.name=Graph Test", "-c", "user.email=graph@example.invalid",
            ]).args(args).env("GIT_TERMINAL_PROMPT", "0").output().unwrap();
            assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
            String::from_utf8(output.stdout).unwrap().trim_end_matches('\n').to_owned()
        }

        fn commit(&self, subject: &str) -> String {
            self.git(&["commit", "-q", "--allow-empty", "-m", subject]);
            self.git(&["rev-parse", "HEAD"])
        }

        fn snapshot(&self, limit: usize) -> (Value, Vec<GitGraphCommit>) {
            let view = git_panel_snapshot(&self.0, limit).unwrap();
            let graph = serde_json::from_value(view["commit_graph"].clone()).unwrap();
            (view, graph)
        }
    }

    impl Drop for GraphRepo {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }

    #[test]
    fn real_graph_preserves_fork_merge_edges_and_remote_heads_without_mutation() {
        let repo = GraphRepo::new();
        let base = repo.commit("처음");
        repo.git(&["switch", "-q", "-c", "feature/한글"]);
        let feature = repo.commit("가지\x1f제목");
        repo.git(&["switch", "-q", "main"]);
        let main = repo.commit("main line");
        repo.git(&["merge", "--no-ff", "-q", "feature/한글", "-m", "merge"]);
        let merge = repo.git(&["rev-parse", "HEAD"]);
        repo.git(&["update-ref", "refs/remotes/origin/main", &main]);
        repo.git(&["symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/main"]);
        let before = repo.git(&["show-ref"]);
        let (view, graph) = repo.snapshot(200);
        assert_eq!(repo.git(&["show-ref"]), before);
        assert_eq!(repo.git(&["rev-parse", "HEAD"]), merge);
        assert_eq!(view["graph_supported"], true);
        assert_eq!(view["graph_truncated"], false);
        assert_eq!(graph.len(), 4);
        assert_eq!(graph[0].oid, merge);
        assert_eq!(graph[0].parents, [main.clone(), feature.clone()]);
        assert_eq!(graph.last().unwrap().oid, base);
        assert!(graph.last().unwrap().parents.is_empty());
        assert_eq!(graph.iter().find(|commit| commit.oid == main).unwrap().refs, ["refs/remotes/origin/main"]);
        assert_eq!(graph.iter().find(|commit| commit.oid == feature).unwrap().subject, "가지 제목");
        assert!(graph.iter().all(|commit| !commit.refs.iter().any(|reference| reference.ends_with("origin/HEAD"))));
        for (index, commit) in graph.iter().enumerate() {
            for parent in &commit.parents {
                assert!(graph.iter().position(|candidate| &candidate.oid == parent).unwrap() > index);
            }
        }
        assert!(view["recent_commits"].as_array().unwrap().iter().all(|row| row.as_array().unwrap().len() == 2));
    }

    #[test]
    fn real_graph_includes_remote_only_and_detached_heads_but_not_tag_only_history() {
        let repo = GraphRepo::new();
        let base = repo.commit("base");
        repo.git(&["switch", "-q", "--detach", &base]);
        let remote = repo.commit("remote only");
        repo.git(&["update-ref", "refs/remotes/origin/topic", &remote]);
        repo.git(&["switch", "-q", "--detach", &base]);
        let tagged = repo.commit("tag only");
        repo.git(&["-c", "tag.gpgSign=false", "tag", "unrelated", &tagged]);
        repo.git(&["switch", "-q", "--detach", &base]);
        let detached = repo.commit("detached only");
        let (view, graph) = repo.snapshot(200);
        assert_eq!(view["detached"], true);
        assert!(graph.iter().any(|commit| commit.oid == remote && commit.refs == ["refs/remotes/origin/topic"]));
        assert!(graph.iter().any(|commit| commit.oid == detached && commit.refs == ["HEAD"]));
        assert!(!graph.iter().any(|commit| commit.oid == tagged));
    }

    #[test]
    fn real_graph_unborn_is_supported_and_truncation_keeps_boundary_parent_ids() {
        let repo = GraphRepo::new();
        let (view, graph) = repo.snapshot(200);
        assert_eq!(view["unborn"], true);
        assert_eq!(view["graph_supported"], true);
        assert_eq!(view["graph_truncated"], false);
        assert!(graph.is_empty());
        let parent = repo.commit("root");
        let head = repo.commit("second");
        for limit in [0, 1] {
            let (view, graph) = repo.snapshot(limit);
            assert_eq!(view["graph_truncated"], true);
            assert_eq!(graph.len(), 1);
            assert_eq!(graph[0].oid, head);
            assert_eq!(graph[0].parents, [parent.clone()]);
        }
    }

    #[test]
    fn real_shallow_graph_reports_incomplete_ancestry_without_fetching() {
        let source = GraphRepo::new();
        source.commit("root");
        source.commit("second");
        let clone_parent = GraphRepo::new();
        let clone_path = clone_parent.0.join("shallow");
        let source_path = source.0.to_string_lossy().replace('\\', "/");
        let source_url = format!("file://{}{source_path}", if source_path.starts_with('/') { "" } else { "/" });
        clone_parent.git(&["-c", "protocol.file.allow=always", "clone", "-q", "--depth=1",
            &source_url, clone_path.to_str().unwrap()]);
        let view = git_panel_snapshot(&clone_path, 200).unwrap();
        assert_eq!(view["graph_supported"], true);
        assert_eq!(view["graph_truncated"], true);
        assert_eq!(view["commit_graph"].as_array().unwrap().len(), 1);
        assert_eq!(git_cmd().arg("-C").arg(&clone_path).args(["rev-list", "--count", "HEAD"])
            .output().unwrap().stdout, b"1\n");
    }
}

#[cfg(test)]
mod status_marks_tests {
    use super::*;
    use std::path::PathBuf;

    fn root() -> PathBuf {
        PathBuf::from("/repo")
    }

    #[test]
    fn files_and_every_ancestor_get_a_mark() {
        let m = status_marks(&root(), [('M', "app/src/main.rs")]);
        assert_eq!(m.get(&PathBuf::from("/repo/app/src/main.rs")), Some(&'M'));
        assert_eq!(m.get(&PathBuf::from("/repo/app/src")), Some(&'M'));
        assert_eq!(m.get(&PathBuf::from("/repo/app")), Some(&'M'));
        // 루트 자신도 — 트리가 레포보다 위에서 시작하면 레포 폴더에도 표시가 붙는다.
        assert_eq!(m.get(&root()), Some(&'M'));
    }

    #[test]
    fn does_not_leak_above_the_repo_root() {
        let m = status_marks(&root(), [('M', "a.txt")]);
        assert_eq!(m.get(&PathBuf::from("/")), None);
    }

    #[test]
    fn folder_inherits_the_highest_ranked_descendant() {
        // 같은 폴더 아래 미추적(U)과 수정(M)이 섞이면 폴더는 M 이어야 한다 —
        // 순서를 뒤집어도 결과가 같아야 진짜 순위 비교다.
        let m = status_marks(&root(), [('U', "src/new.rs"), ('M', "src/old.rs")]);
        assert_eq!(m.get(&PathBuf::from("/repo/src")), Some(&'M'));
        let m = status_marks(&root(), [('M', "src/old.rs"), ('U', "src/new.rs")]);
        assert_eq!(m.get(&PathBuf::from("/repo/src")), Some(&'M'));
        // 파일 자신은 자기 마커를 그대로 지킨다.
        assert_eq!(m.get(&PathBuf::from("/repo/src/new.rs")), Some(&'U'));
    }

    #[test]
    fn a_file_in_both_buckets_keeps_the_stronger_marker() {
        // 부분 스테이지된 파일은 staged(A)·unstaged(M) 양쪽에 나온다.
        let m = status_marks(&root(), [('A', "x.rs"), ('M', "x.rs")]);
        assert_eq!(m.get(&PathBuf::from("/repo/x.rs")), Some(&'M'));
    }

    #[test]
    fn porcelain_z_folds_status_by_what_happened_not_by_staging() {
        // `M ` 은 스테이지됐을 뿐 여전히 수정이다 — 추가(A)로 접으면 새 파일과
        // 구분이 사라진다. 삭제는 자기 글자를 지켜야 눈에 띈다.
        let out = "?? new.rs\0 M edited.rs\0M  staged.rs\0MM both.rs\0A  added.rs\0 D gone.rs\0";
        let rows = parse_status_porcelain_z(out);
        assert_eq!(
            rows,
            vec![
                ('U', "new.rs".into()),
                ('M', "edited.rs".into()),
                ('M', "staged.rs".into()),
                ('M', "both.rs".into()),
                ('A', "added.rs".into()),
                ('D', "gone.rs".into()),
            ]
        );
    }

    #[test]
    fn porcelain_z_consumes_the_rename_origin_record() {
        // R 레코드 뒤엔 원래 경로가 한 칸 더 온다. 안 먹으면 그 경로가 다음
        // 항목의 상태 문자로 읽혀 뒤가 통째로 밀린다.
        let out = "R  new/name.rs\0old/name.rs\0 M after.rs\0";
        let rows = parse_status_porcelain_z(out);
        assert_eq!(rows, vec![('A', "new/name.rs".into()), ('M', "after.rs".into())]);
    }

    #[test]
    fn porcelain_z_keeps_paths_with_spaces_intact() {
        // 기본 출력이면 따옴표로 감싸여 경로가 어긋나던 자리.
        let rows = parse_status_porcelain_z(" M dir with space/a b.rs\0");
        assert_eq!(rows, vec![('M', "dir with space/a b.rs".into())]);
    }

    #[test]
    fn untracked_directory_entry_marks_the_directory_itself() {
        // git 은 미추적 폴더를 `dir/` 하나로 접어서 준다 — 슬래시를 안 떼면
        // 트리의 폴더 경로와 안 맞아 표시가 통째로 빠진다.
        let m = status_marks(&root(), [('U', "assets/icons/")]);
        assert_eq!(m.get(&PathBuf::from("/repo/assets/icons")), Some(&'U'));
        assert_eq!(m.get(&PathBuf::from("/repo/assets")), Some(&'U'));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_branch_ahead_and_buckets() {
        let sample = "# branch.oid abc123\n\
# branch.head main\n\
# branch.upstream origin/main\n\
# branch.ab +4 -2\n\
1 .M N... 100644 100644 100644 aaa bbb crates/foo bar.rs\n\
1 M. N... 100644 100644 100644 ccc ddd staged.rs\n\
2 R. N... 100644 100644 100644 eee fff R100 new.rs\told.rs\n\
? brand new.rs\n";
        let v = parse_porcelain_v2(sample);
        assert_eq!(v["branch"], "main");
        assert_eq!(v["ahead"], 4);
        assert_eq!(v["behind"], 2);
        // ".M" -> worktree modified; "crates/foo bar.rs" has a space.
        assert_eq!(v["modified"], serde_json::json!(["crates/foo bar.rs"]));
        // "M." and the rename "R." are index-side -> staged.
        assert_eq!(v["staged"], serde_json::json!(["staged.rs", "new.rs"]));
        assert_eq!(v["untracked"], serde_json::json!(["brand new.rs"]));
        assert_eq!(v["clean"], false);
    }

    #[test]
    fn shortstat_parses_both_and_one_sided() {
        assert_eq!(
            parse_shortstat(" 3 files changed, 460 insertions(+), 59 deletions(-)"),
            (460, 59)
        );
        // 추가만 / 삭제만 — 한쪽이 빠진 형식.
        assert_eq!(parse_shortstat(" 1 file changed, 7 insertions(+)"), (7, 0));
        assert_eq!(parse_shortstat(" 1 file changed, 4 deletions(-)"), (0, 4));
        // 변경 없음(빈 출력).
        assert_eq!(parse_shortstat(""), (0, 0));
    }

    #[test]
    fn clean_repo_reports_clean() {
        let v = parse_porcelain_v2("# branch.head main\n# branch.ab +0 -0\n");
        assert_eq!(v["clean"], true);
        assert_eq!(v["ahead"], 0);
    }

    #[test]
    fn fingerprint_moves_with_git_state_not_worktree_edits() {
        use std::process::Command as C;
        let dir = std::env::temp_dir().join(format!("kasa-git-print-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let git = |args: &[&str]| C::new("git").arg("-C").arg(&dir).args(args).output().unwrap();
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "tester"]);
        std::fs::write(dir.join("a.txt"), "v1\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "init"]);
        let paths = repo_paths(&dir.join("sub")).expect("subdir resolves to its repo");
        assert_eq!(paths.root.canonicalize().unwrap(), dir.canonicalize().unwrap());
        let print = || repo_fingerprint(&paths).unwrap();
        let start = print();
        // 작업 트리 편집은 지문 밖이다(mod 신호·긴 주기 몫).
        std::fs::write(dir.join("a.txt"), "v2\n").unwrap();
        assert_eq!(print(), start);
        git(&["add", "a.txt"]);
        let staged = print();
        assert_ne!(staged, start, "stage moves the index");
        git(&["commit", "-qm", "two"]);
        let committed = print();
        assert_ne!(committed, staged, "commit moves the branch ref");
        // 갈래 폴더 안의 브랜치 — 폴더 시각만 보면 놓친다.
        git(&["branch", "kei/nested"]);
        let branched = print();
        assert_ne!(branched, committed);
        git(&["update-ref", "refs/heads/kei/nested", "HEAD~1"]);
        assert_ne!(print(), branched, "nested ref update");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn badge_poller_reads_one_repo_once_for_many_cwds() {
        use std::process::Command as C;
        let dir = std::env::temp_dir().join(format!("kasa-git-poll-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let git = |args: &[&str]| C::new("git").arg("-C").arg(&dir).args(args).output().unwrap();
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "tester"]);
        std::fs::write(dir.join("a.txt"), "v1\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "init"]);
        let outside = std::env::temp_dir().join(format!("kasa-git-poll-none-{}", std::process::id()));
        std::fs::create_dir_all(&outside).unwrap();
        let cwds = vec![dir.clone(), dir.join("sub"), outside.clone()];
        let mut poller = BadgePoller::default();
        let first = poller.poll(&cwds);
        assert_eq!(first.len(), 2, "both cwds in the repo get a badge, the plain folder none");
        assert_eq!(first[&dir], first[&dir.join("sub")]);
        assert_eq!(poller.repos.len(), 1);
        let read_at = poller.repos.values().next().unwrap().read_at;
        // 지문이 그대로면 다시 읽지 않는다.
        std::fs::write(dir.join("a.txt"), "v2\n").unwrap();
        let again = poller.poll(&cwds);
        assert_eq!(poller.repos.values().next().unwrap().read_at, read_at);
        assert_eq!(again[&dir].insertions, 0);
        // 스테이지하면 지문이 움직여 그 바퀴에 다시 읽는다.
        git(&["add", "a.txt"]);
        let staged = poller.poll(&cwds);
        assert_eq!(staged[&dir].insertions, 1);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn commit_stages_only_checked_files() {
        use std::process::Command as C;
        let dir = std::env::temp_dir().join(format!("kasa-git-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| C::new("git").arg("-C").arg(&dir).args(args).output().unwrap();
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "tester"]);
        std::fs::write(dir.join("a.txt"), "v1\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "init"]);
        // a.txt 수정 + b.txt 신규(untracked)
        std::fs::write(dir.join("a.txt"), "v2\n").unwrap();
        std::fs::write(dir.join("b.txt"), "new\n").unwrap();

        // a.txt만 체크해서 커밋 → b.txt는 빠져야 한다.
        let r = git_commit(&dir, &["a.txt".to_string()], "only a");
        assert_eq!(r["ok"], true, "commit should succeed: {r:?}");

        let st = String::from_utf8(git(&["status", "--porcelain"]).stdout).unwrap();
        assert!(st.contains("?? b.txt"), "b.txt must remain untracked: {st:?}");
        assert!(!st.contains("a.txt"), "a.txt must be committed (gone from status): {st:?}");

        // diff: 커밋된 a.txt는 HEAD 대비 변경 없음, 미커밋 b.txt는 내용 노출
        let d = git_diff(&dir, "b.txt");
        assert!(d["diff"].as_str().unwrap().contains("new"), "untracked diff should show content: {d:?}");

        // 빈 메시지 / 빈 목록 거부
        assert_eq!(git_commit(&dir, &[], "x")["ok"], false);
        assert_eq!(git_commit(&dir, &["a.txt".to_string()], "  ")["ok"], false);

        std::fs::remove_dir_all(&dir).ok();
    }
}
