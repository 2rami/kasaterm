//! 정리 한 바퀴 — 찾은 빌드 산출물을 오래 안 쓴 것부터, 쓰는 중인 것은 건너뛰고 지운다. 언제 도는지(`Schedule`),
//! 얼마나(`Goal`), 무엇을 남기는지(`plan`), 지우기 직전 다시 재기(`run_in`)가 여기 있다. 규칙은 `docs/disk-guard.md`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;

use super::outputs::{self, Kind};
use super::{in_use, level, now_secs, Config, Level, PassSummary, Reason};
use super::{FIRST_ROUTINE_AFTER, PRESSURE_EVERY, PRESSURE_MARGIN, RELEASE_STATE, ROOTS_ENV, ROUTINE_EVERY};

/// 언제 정리할까 — 앱이 켜지고 5분 뒤부터 한 시간마다, 문턱 아래면 10분마다.
pub(super) struct Schedule {
    started: Instant,
    last_routine: Option<Instant>,
    last_pressure: Option<Instant>,
}

impl Schedule {
    pub(super) fn new() -> Self {
        Schedule { started: Instant::now(), last_routine: None, last_pressure: None }
    }

    /// 때가 됐으면 한 바퀴 돌고 기록한 뒤 요약을 낸다. 자동 정리를 껐으면 때만 넘긴다.
    pub(super) fn tick(&mut self, cfg: &Config, free: u64) -> Option<PassSummary> {
        let low = level(free, cfg) != Level::Ok;
        let due = |last: Option<Instant>, every: Duration, first: Duration| {
            last.map_or(self.started.elapsed() >= first, |t| t.elapsed() >= every)
        };
        let reason = if low && due(self.last_pressure, PRESSURE_EVERY, Duration::ZERO) {
            Reason::Pressure
        } else if due(self.last_routine, ROUTINE_EVERY, FIRST_ROUTINE_AFTER) {
            Reason::Routine
        } else {
            return None;
        };
        self.last_routine = Some(Instant::now());
        if low {
            self.last_pressure = self.last_routine;
        }
        cfg.auto_prune.then(|| run_and_log(cfg, reason, &[], false).summary())
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Removed {
    pub path: String,
    pub kind: Kind,
    pub bytes: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Skipped {
    pub path: String,
    pub kind: Kind,
    pub bytes: u64,
    pub why: String,
}

/// 정리 한 바퀴의 기록 — 그대로 기록 파일의 한 줄이 된다.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Report {
    pub at: u64,
    pub reason: &'static str,
    pub dry_run: bool,
    pub free_before: u64,
    pub free_after: u64,
    pub outputs: usize,
    pub outputs_bytes: u64,
    pub cap: u64,
    pub removed: Vec<Removed>,
    pub skipped: Vec<Skipped>,
    pub error: Option<String>,
}

impl Report {
    pub fn freed(&self) -> u64 {
        self.removed.iter().map(|r| r.bytes).sum()
    }

    fn worth_logging(&self) -> bool {
        self.reason != Reason::Routine.as_str()
            || !self.removed.is_empty()
            || !self.skipped.is_empty()
            || self.error.is_some()
    }

    fn summary(&self) -> PassSummary {
        PassSummary {
            at: self.at,
            reason: self.reason.to_string(),
            freed: self.freed(),
            removed: self.removed.len(),
            skipped: self.skipped.len(),
        }
    }
}

/// 어디를 보고 어디까지 지울 수 있나. `allowed` 밖·`deny` 안은 산출물 표식이 있어도 지우지 않는다.
struct Scope {
    roots: Vec<outputs::Root>,
    derived_data: Option<PathBuf>,
    allowed: Vec<PathBuf>,
    deny: Vec<PathBuf>,
}

impl Scope {
    fn current(extra_repos: &[PathBuf]) -> Result<Scope, &'static str> {
        let home = crate::home_dir().ok_or("홈 폴더를 몰라요")?;
        let deny = vec![outputs::real(&home.join(RELEASE_STATE))];
        if let Some(test) = std::env::var_os(ROOTS_ENV).filter(|v| !v.is_empty()) {
            let roots: Vec<PathBuf> = std::env::split_paths(&test).filter(|p| p.is_dir()).collect();
            return Ok(Scope::of(roots.into_iter().map(|path| outputs::Root { path, depth: 2 }).collect(), None, deny));
        }
        if super::isolated() {
            return Err("격리 실행이라 지우지 않아요");
        }
        let mut roots = Vec::new();
        if let Ok(entries) = std::fs::read_dir(super::tmp_root()) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let real_dir = entry.file_type().is_ok_and(|t| t.is_dir());
                if real_dir && (name.starts_with("kt-") || name.starts_with("kasaterm-")) {
                    roots.push(outputs::Root { path: entry.path(), depth: 2 });
                }
            }
        }
        roots.push(outputs::Root { path: home.join("Library/Caches/kasaterm-builds"), depth: 2 });
        for repo in super::remembered_repos().into_iter().chain(extra_repos.iter().cloned()) {
            roots.push(outputs::Root { path: repo, depth: 1 });
        }
        let derived = home.join("Library/Developer/Xcode/DerivedData");
        Ok(Scope::of(roots, Some(derived), deny))
    }

    fn of(roots: Vec<outputs::Root>, derived_data: Option<PathBuf>, deny: Vec<PathBuf>) -> Scope {
        let allowed = roots
            .iter()
            .map(|r| r.path.as_path())
            .chain(derived_data.as_deref())
            .filter_map(|p| std::fs::canonicalize(p).ok())
            .collect();
        Scope { roots, derived_data, allowed, deny }
    }
}

struct Row {
    output: outputs::Output,
    real: PathBuf,
    bytes: u64,
    used: u64,
}

/// 얼마나 치울지. 평소에는 산출물 합을 상한 밑으로, 문턱 아래에서는 남은 공간이 문턱 위로 올라설 때까지.
#[derive(Clone, Copy, Debug)]
enum Goal {
    Cap(u64),
    Free { free: u64, want: u64 },
}

fn goal_for(free: u64, cfg: &Config) -> Goal {
    if free < cfg.warn_free {
        Goal::Free { free, want: cfg.warn_free.saturating_add(PRESSURE_MARGIN) }
    } else {
        Goal::Cap(cfg.build_cap)
    }
}

/// 오래 안 쓴 것부터 지울 차례를 정한다. 최근에 바뀐 것·누가 쓰는 것·아직 남을 다른 산출물이 링크로 가리키는
/// 것은 건너뛰고 까닭을 남긴다.
fn plan(
    rows: &[Row],
    goal: Goal,
    idle_secs: u64,
    used: &std::collections::HashSet<PathBuf>,
    now: u64,
) -> (Vec<usize>, Vec<(usize, String)>) {
    let links: Vec<_> = rows
        .iter()
        .map(|r| if r.output.kind == Kind::Cargo { outputs::links_into(&r.output.path) } else { Default::default() })
        .collect();
    let referrers: Vec<Vec<usize>> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            (0..rows.len()).filter(|&j| j != i && links[j].iter().any(|l| l.starts_with(&row.real))).collect()
        })
        .collect();
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by_key(|&i| rows[i].used);
    let mut total: u64 = rows.iter().map(|r| r.bytes).sum();
    let mut gained = 0u64;
    let (mut doomed, mut skipped) = (Vec::new(), Vec::new());
    for i in order {
        let enough = match goal {
            Goal::Cap(cap) => total <= cap,
            Goal::Free { free, want } => free.saturating_add(gained) >= want,
        };
        if enough {
            break;
        }
        let row = &rows[i];
        let age = now.saturating_sub(row.used);
        let why = if age < idle_secs {
            Some(format!("{}분 전에 바뀌었어요", age / 60))
        } else if in_use::in_use(&row.output, used) {
            Some("쓰는 중이에요".to_string())
        } else if referrers[i].iter().any(|j| !doomed.contains(j)) {
            Some("남는 산출물이 링크로 가리켜요".to_string())
        } else {
            None
        };
        match why {
            Some(why) => skipped.push((i, why)),
            None => {
                doomed.push(i);
                total = total.saturating_sub(row.bytes);
                gained = gained.saturating_add(row.bytes);
            }
        }
    }
    (doomed, skipped)
}

fn run_in(
    cfg: &Config,
    reason: Reason,
    scope: &Scope,
    dry_run: bool,
    measure: &dyn Fn(&Path) -> u64,
    used_paths: &dyn Fn() -> Result<std::collections::HashSet<PathBuf>, in_use::Unknown>,
    free_now: &dyn Fn() -> Option<u64>,
) -> Report {
    let at = now_secs();
    let free_before = free_now().unwrap_or(0);
    let mut report = Report { at, reason: reason.as_str(), dry_run, free_before, free_after: free_before, cap: cfg.build_cap, ..Report::default() };
    let rows: Vec<Row> = outputs::find(&scope.roots, scope.derived_data.as_deref())
        .into_iter()
        .map(|output| Row { real: outputs::real(&output.path), bytes: measure(&output.path), used: outputs::last_used(&output.path), output })
        .collect();
    report.outputs = rows.len();
    report.outputs_bytes = rows.iter().map(|r| r.bytes).sum();
    let goal = goal_for(free_before, cfg);
    let satisfied = match goal {
        Goal::Cap(cap) => report.outputs_bytes <= cap,
        Goal::Free { free, want } => free >= want,
    };
    if satisfied || rows.is_empty() {
        return report;
    }
    let used = match used_paths() {
        Ok(used) => used,
        Err(in_use::Unknown(why)) => {
            report.error = Some(format!("누가 무엇을 쓰는지 몰라 지우지 않았어요 — {why}"));
            return report;
        }
    };
    let idle = cfg.idle.as_secs();
    let (doomed, skipped) = plan(&rows, goal, idle, &used, at);
    let entry = |row: &Row| (row.output.path.to_string_lossy().into_owned(), row.output.kind, row.bytes);
    report.skipped = skipped
        .into_iter()
        .map(|(i, why)| {
            let (path, kind, bytes) = entry(&rows[i]);
            Skipped { path, kind, bytes, why }
        })
        .collect();
    for i in doomed {
        let row = &rows[i];
        let (path, kind, bytes) = entry(row);
        if let Goal::Free { want, .. } = goal {
            if !dry_run && free_now().is_some_and(|free| free >= want) {
                break;
            }
        }
        if dry_run {
            report.removed.push(Removed { path, kind, bytes });
            continue;
        }
        // 지우기 직전 다시 잰다 — 계획 뒤에 시작한 빌드가 그 산출물을 잡았을 수 있다.
        let fresh = match used_paths() {
            Ok(fresh) => fresh,
            Err(in_use::Unknown(why)) => {
                report.error = Some(format!("다시 재지 못해 멈췄어요 — {why}"));
                break;
            }
        };
        let moved = outputs::real(&row.output.path) != row.real || outputs::last_used(&row.output.path) != row.used;
        if moved || in_use::in_use(&row.output, &fresh) {
            report.skipped.push(Skipped { path, kind, bytes, why: "다시 재니 쓰는 중이에요".into() });
            continue;
        }
        match outputs::remove(&row.output, &scope.allowed, &scope.deny) {
            Ok(()) => report.removed.push(Removed { path, kind, bytes }),
            Err(why) => report.skipped.push(Skipped { path, kind, bytes, why: why.into() }),
        }
    }
    report.free_after = free_now().unwrap_or(free_before);
    report
}

/// 잠금 — 앱과 `kasaterm-cli disk prune` 이 같은 산출물을 함께 지우지 않게 한 번에 하나만 돈다.
fn lock() -> Option<std::fs::File> {
    use std::os::unix::io::AsRawFd;
    let dir = super::config_dir()?;
    let _ = std::fs::create_dir_all(&dir);
    let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("disk-guard.lock")).ok()?;
    // SAFETY: 열린 파일의 기술자다. 잠금은 file 이 닫힐 때 풀린다.
    (unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0).then_some(file)
}

/// 정리 한 바퀴. `extra_repos` 는 이번에 함께 볼 레포(빌드 직후의 레포). 기록은 부른 쪽이 남긴다.
pub fn run_pass(cfg: &Config, reason: Reason, extra_repos: &[PathBuf], dry_run: bool) -> Report {
    let free_now = || super::free_space().map(|(free, _)| free);
    let refused = |why: &str| Report {
        at: now_secs(),
        reason: reason.as_str(),
        dry_run,
        free_before: free_now().unwrap_or(0),
        free_after: free_now().unwrap_or(0),
        cap: cfg.build_cap,
        error: Some(why.to_string()),
        ..Report::default()
    };
    let scope = match Scope::current(extra_repos) {
        Ok(scope) => scope,
        Err(why) => return refused(why),
    };
    let Some(_lock) = lock() else { return refused("다른 정리가 도는 중이에요") };
    run_in(cfg, reason, &scope, dry_run, &|p| super::du_bytes(p).unwrap_or(0), &in_use::used_paths, &free_now)
}

/// 정리하고 기록한다 — 앱과 CLI 가 같은 길로 남긴다.
pub fn run_and_log(cfg: &Config, reason: Reason, extra_repos: &[PathBuf], dry_run: bool) -> Report {
    let report = run_pass(cfg, reason, extra_repos, dry_run);
    if report.worth_logging() && !dry_run {
        super::append_log(&report);
    }
    report
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    const GIB: u64 = 1 << 30;

    const NOW: u64 = 2_000_000_000;

    fn cfg() -> Config {
        Config::from_settings(&serde_json::Value::Null)
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kasa-disk-pass-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        outputs::real(&dir)
    }

    fn target(at: &Path) -> PathBuf {
        std::fs::create_dir_all(at.join("debug")).unwrap();
        std::fs::write(at.join("CACHEDIR.TAG"), outputs::CARGO_TAG).unwrap();
        at.to_path_buf()
    }

    fn row(path: &Path, age_h: u64) -> Row {
        let output = outputs::Output { path: path.to_path_buf(), kind: Kind::Cargo, workspace: path.parent().unwrap().to_path_buf() };
        Row { real: outputs::real(path), output, bytes: 10 * GIB, used: NOW - age_h * 3600 }
    }

    fn doomed(rows: &[Row], goal: Goal, used: &HashSet<PathBuf>) -> Vec<PathBuf> {
        plan(rows, goal, 30 * 60, used, NOW).0.into_iter().map(|i| rows[i].output.path.clone()).collect()
    }

    #[test]
    fn over_the_cap_the_longest_unused_go_first_until_under() {
        let root = scratch("cap");
        let (old, mid, new) = (target(&root.join("old/target")), target(&root.join("mid/target")), target(&root.join("new/target")));
        let rows = [row(&new, 2), row(&old, 9), row(&mid, 5)];
        assert_eq!(doomed(&rows, Goal::Cap(15 * GIB), &HashSet::new()), vec![old, mid]);
        assert!(doomed(&rows, Goal::Cap(30 * GIB), &HashSet::new()).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn under_pressure_the_cap_is_ignored_until_free_space_recovers() {
        let root = scratch("pressure");
        let (a, b, c) = (target(&root.join("a/target")), target(&root.join("b/target")), target(&root.join("c/target")));
        let rows = [row(&a, 9), row(&b, 8), row(&c, 7)];
        let goal = Goal::Free { free: 3 * GIB, want: 25 * GIB };
        assert_eq!(doomed(&rows, goal, &HashSet::new()), vec![a, b, c.clone()]);
        let recent = [row(&c, 0)];
        assert!(doomed(&recent, goal, &HashSet::new()).is_empty(), "a target touched minutes ago stays even under pressure");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_build_seen_through_an_alias_or_a_running_binary_protects_its_target() {
        let root = scratch("alias");
        let kt = target(&root.join("kt-x/target"));
        let idle = target(&root.join("idle/target"));
        let alias = root.join("tmp");
        std::os::unix::fs::symlink(&root, &alias).unwrap();
        let seen: HashSet<PathBuf> = in_use::env_paths(&format!("cargo build PWD={}/kt-x", alias.display()))
            .iter()
            .map(|p| outputs::real(p))
            .collect();
        assert!(doomed(&[row(&kt, 9)], Goal::Cap(GIB), &seen).is_empty());
        let running: HashSet<PathBuf> = in_use::lsof_paths(&format!("p1\nftxt\nn{}/debug/kasaterm\n", kt.display()));
        assert_eq!(doomed(&[row(&kt, 9), row(&idle, 8)], Goal::Cap(5 * GIB), &running), vec![idle]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_target_another_links_into_stays_until_that_one_goes_too() {
        let root = scratch("links");
        let shared = target(&root.join("tell-mobile/source/target"));
        std::fs::create_dir_all(shared.join("ios")).unwrap();
        let sender = target(&root.join("report-senders/source/target"));
        std::os::unix::fs::symlink(shared.join("ios"), sender.join("ios")).unwrap();
        let rows = [row(&shared, 9), row(&sender, 5)];
        for cap in [15, 1] {
            assert_eq!(doomed(&rows, Goal::Cap(cap * GIB), &HashSet::new()), vec![sender.clone()]);
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_pass_removes_idle_outputs_inside_scope_and_logs_why_others_stay() {
        let root = scratch("run");
        let old = target(&root.join("kt-old/target"));
        let busy = target(&root.join("kt-busy/target"));
        let mobile = root.join("kt-old/mobile");
        std::fs::create_dir_all(mobile.join("build/ios")).unwrap();
        std::fs::write(mobile.join("pubspec.yaml"), "name: x\n").unwrap();
        let scope = Scope::of(vec![outputs::Root { path: root.clone(), depth: 3 }], None, vec![]);
        let busy_ws = outputs::real(&root.join("kt-busy"));
        let used = move || Ok(HashSet::from([busy_ws.join("src")]));
        let mut c = cfg();
        c.build_cap = 0;
        c.idle = Duration::ZERO;
        let report = run_in(&c, Reason::Manual, &scope, false, &|_| GIB, &used, &|| Some(100 * GIB));
        let removed: HashSet<_> = report.removed.iter().map(|r| PathBuf::from(&r.path)).collect();
        assert_eq!(removed, HashSet::from([old.clone(), mobile.join("build")]));
        assert!(!old.exists() && !mobile.join("build").exists() && mobile.join("pubspec.yaml").exists());
        assert!(busy.exists());
        assert_eq!(report.skipped.len(), 1);
        assert_eq!(report.skipped[0].why, "쓰는 중이에요");
        let unknown = || Err(in_use::Unknown("ps 를 못 읽었어요"));
        let left = run_in(&c, Reason::Manual, &scope, false, &|_| GIB, &unknown, &|| Some(100 * GIB));
        assert!(left.removed.is_empty() && left.error.is_some() && busy.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

}
