//! 칸 폴더의 파일 변경 감시 — Git 열·배지가 「깃이 바뀌었을 수 있다」를 아는 길이고, 부모 폴더 칸에서 어느 하위
//! 레포를 만지고 있는지 고르는 「최근」이다. claude·codex·셸·편집기 누가 고쳐도 같은 길이다(예전엔 claude 연결 mod 가
//! 알리는 신호뿐이라 codex·셸 칸은 10초 주기에 기댔다). macOS 는 FSEvents, Windows 는 ReadDirectoryChangesW. 그 밖에서는
//! 감시를 세우지 않고 부르는 쪽이 주기로 읽는다.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

pub fn supported() -> bool {
    cfg!(any(target_os = "macos", windows))
}

/// 아무도 다시 찾지 않은 감시를 거두는 때 — 그 칸이 닫혔거나 열이 다른 칸으로 갔다.
const IDLE_DROP: Duration = Duration::from_secs(120);
const CHANGE_CAP: usize = 256;
/// 「최근」으로 기억하는 폴더 수 — 바뀐 파일의 폴더 단위.
const ACTIVITY_CAP: usize = 512;

/// 빌드·의존성·캐시 폴더. 거의 다 gitignore 라 Git 열이 달라질 일이 없는데 빌드 중에는 초당 수천 번 바뀐다.
const NOISE_DIRS: &[&str] = &[
    "node_modules", "target", ".next", ".nuxt", ".svelte-kit", ".turbo", ".cache", ".parcel-cache", "dist", "build",
    "__pycache__", ".pytest_cache", ".mypy_cache", ".ruff_cache", ".venv", "venv", ".gradle", ".dart_tool", "Pods",
    "DerivedData", ".expo", "coverage",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub seq: u64,
    pub path: PathBuf,
}

#[derive(Default)]
struct Store {
    seq: u64,
    recent: VecDeque<Change>,
    activity: HashMap<PathBuf, SystemTime>,
}

static STORE: LazyLock<Mutex<Store>> = LazyLock::new(Default::default);
static WAKE: LazyLock<tokio::sync::Notify> = LazyLock::new(tokio::sync::Notify::new);
type Listener = Arc<dyn Fn() + Send + Sync>;
static LISTENER: OnceLock<Listener> = OnceLock::new();

/// 변경이 쌓이면 부를 자리 — 앱이 Git 열 일꾼을 깨우려고 한 번 건다.
pub fn set_listener(listener: impl Fn() + Send + Sync + 'static) {
    let _ = LISTENER.set(Arc::new(listener));
}

/// 작업 트리 밖의 git 속(`.git/`)은 HEAD·ref 만 — 커밋·체크아웃·브랜치·fetch. 인덱스는 누가 `git status` 만 해도 다시
/// 쓰이므로 빼고(인덱스는 Git 열의 지문이 따로 본다), 객체·잠금 파일은 늘 뒤따르는 것이다.
fn noise(path: &Path) -> bool {
    let mut inside_git = false;
    for part in path.components() {
        let std::path::Component::Normal(part) = part else { continue };
        let Some(part) = part.to_str() else { continue };
        if inside_git {
            if part == "refs" {
                return path.extension().is_some_and(|ext| ext == "lock");
            }
            continue;
        }
        if part == ".git" {
            inside_git = true;
        } else if NOISE_DIRS.contains(&part) {
            return true;
        }
    }
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    if inside_git {
        return !matches!(name, "HEAD" | "packed-refs" | "FETCH_HEAD" | "ORIG_HEAD" | "MERGE_HEAD");
    }
    name == ".DS_Store"
}

fn record(paths: impl IntoIterator<Item = PathBuf>) {
    let now = SystemTime::now();
    let mut any = false;
    {
        let mut store = STORE.lock().unwrap();
        for path in paths {
            if noise(&path) {
                continue;
            }
            any = true;
            store.seq += 1;
            let seq = store.seq;
            if store.recent.len() >= CHANGE_CAP {
                store.recent.pop_front();
            }
            if let Some(dir) = path.parent() {
                store.activity.insert(dir.to_path_buf(), now);
            }
            store.recent.push_back(Change { seq, path });
        }
        if store.activity.len() > ACTIVITY_CAP {
            let mut times: Vec<_> = store.activity.values().copied().collect();
            times.sort_unstable();
            let cut = times[times.len() - ACTIVITY_CAP];
            store.activity.retain(|_, at| *at >= cut);
        }
    }
    if any {
        WAKE.notify_waiters();
        if let Some(listener) = LISTENER.get() {
            listener();
        }
    }
}

pub fn seq() -> u64 {
    STORE.lock().unwrap().seq
}

/// `seen` 뒤의 변경과 지금 번호. 고리에서 밀려난 것이 있으면 `lost` — 무엇이 바뀌었는지 모르니 다시 읽을 때다.
pub fn changes_since(seen: u64) -> (u64, Vec<Change>, bool) {
    let store = STORE.lock().unwrap();
    let lost = store.recent.front().is_some_and(|first| first.seq > seen.saturating_add(1));
    let fresh = store.recent.iter().filter(|c| c.seq > seen).cloned().collect();
    (store.seq, fresh, lost)
}

/// 번호가 `seen` 을 넘거나 `wait` 가 지날 때까지 기다린다. 지금 번호를 돌려준다.
pub async fn wait(seen: u64, wait: Duration) -> u64 {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let notified = WAKE.notified();
        let now = seq();
        if now > seen || tokio::time::Instant::now() >= deadline {
            return now;
        }
        if tokio::time::timeout_at(deadline, notified).await.is_err() {
            return seq();
        }
    }
}

/// 감시가 알려 주는 경로는 실제 경로다(`/tmp` → `/private/tmp`). 견줄 뿌리도 그렇게 펴서 둘 다로 본다.
fn roots_of(root: &Path) -> [PathBuf; 2] {
    [root.to_path_buf(), std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf())]
}

/// 이 변경들 가운데 `root` 아래가 있나.
pub fn touched(changes: &[Change], root: &Path) -> bool {
    let roots = roots_of(root);
    changes.iter().any(|c| roots.iter().any(|r| c.path.starts_with(r)))
}

/// `root` 아래에서 마지막으로 무엇이 바뀐 때(이 앱이 감시한 동안).
pub fn last_change_under(root: &Path) -> Option<SystemTime> {
    let roots = roots_of(root);
    let store = STORE.lock().unwrap();
    store.activity.iter().filter(|(dir, _)| roots.iter().any(|r| dir.starts_with(r))).map(|(_, at)| *at).max()
}

struct Watching {
    watcher: notify::RecommendedWatcher,
    roots: HashMap<PathBuf, Instant>,
}

static WATCHING: LazyLock<Mutex<Option<Watching>>> = LazyLock::new(Default::default);

/// `dir` 아래(하위 폴더 전부)를 감시한다 — 이미 그 위를 보고 있으면 시각만 늘린다. 감시할 수 없는 곳이면 false.
/// 부르는 쪽은 보는 동안 주기마다 다시 부른다. `IDLE_DROP` 동안 안 부른 폴더는 거둔다.
pub fn ensure(dir: &Path) -> bool {
    use notify::Watcher;
    if !supported() || !dir.is_absolute() {
        return false;
    }
    let now = Instant::now();
    let mut slot = WATCHING.lock().unwrap();
    if slot.is_none() {
        let watcher = notify::recommended_watcher(|event: notify::Result<notify::Event>| {
            if let Ok(event) = event {
                if !matches!(event.kind, notify::EventKind::Access(_)) {
                    record(event.paths);
                }
            }
        });
        match watcher {
            Ok(watcher) => *slot = Some(Watching { watcher, roots: HashMap::new() }),
            Err(_) => return false,
        }
    }
    let watching = slot.as_mut().unwrap();
    let stale: Vec<PathBuf> =
        watching.roots.iter().filter(|(_, at)| now.duration_since(**at) > IDLE_DROP).map(|(r, _)| r.clone()).collect();
    for root in stale {
        let _ = watching.watcher.unwatch(&root);
        watching.roots.remove(&root);
    }
    if let Some(covering) = watching.roots.keys().find(|root| dir.starts_with(root)).cloned() {
        watching.roots.insert(covering, now);
        return true;
    }
    if watching.watcher.watch(dir, notify::RecursiveMode::Recursive).is_err() {
        return false;
    }
    let inner: Vec<PathBuf> = watching.roots.keys().filter(|root| root.starts_with(dir)).cloned().collect();
    for root in inner {
        let _ = watching.watcher.unwatch(&root);
        watching.roots.remove(&root);
    }
    watching.roots.insert(dir.to_path_buf(), now);
    true
}

#[cfg(test)]
pub(crate) fn record_for_test(paths: Vec<PathBuf>) {
    record(paths);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_output_and_git_internals_are_not_changes_but_commits_and_checkouts_are() {
        for path in [
            "/w/app/node_modules/x/index.js",
            "/w/app/target/debug/deps/a.o",
            "/w/app/.git/index",
            "/w/app/.git/index.lock",
            "/w/app/.git/objects/ab/cdef",
            "/w/app/.git/refs/heads/main.lock",
            "/w/app/src/.DS_Store",
        ] {
            assert!(noise(Path::new(path)), "{path}");
        }
        for path in [
            "/w/app/src/main.rs",
            "/w/app/.git/HEAD",
            "/w/app/.git/refs/heads/feature/x",
            "/w/app/.git/packed-refs",
            "/w/app/.git/worktrees/wt/HEAD",
            "/w/app/.gitignore",
        ] {
            assert!(!noise(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn changes_are_numbered_scoped_to_a_root_and_remember_when_a_folder_last_moved() {
        let start = seq();
        let root = std::env::temp_dir().join(format!("kasa-git-watch-{}", std::process::id()));
        record_for_test(vec![root.join("repo-a/src/a.rs"), root.join("repo-a/target/x.o"), root.join("repo-b/README.md")]);
        let (now, changes, lost) = changes_since(start);
        assert!(!lost);
        assert_eq!(changes.len(), 2, "build output is dropped before it is numbered");
        assert!(now >= start + 2);
        assert!(touched(&changes, &root.join("repo-a")) && touched(&changes, &root.join("repo-b")));
        assert!(!touched(&changes, &root.join("repo-c")));
        assert!(last_change_under(&root.join("repo-a")).is_some());
        assert!(last_change_under(&root.join("repo-c")).is_none());
    }

    #[test]
    fn a_watched_folder_reports_an_edit_made_by_any_process() {
        if !supported() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("kasa-git-watch-live-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        assert!(ensure(&dir));
        assert!(ensure(&dir.join("src")), "a folder under a watched one is already covered");
        std::thread::sleep(Duration::from_millis(300));
        let start = seq();
        std::fs::write(dir.join("src/edit.txt"), "x").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut seen = false;
        while Instant::now() < deadline && !seen {
            let (_, changes, _) = changes_since(start);
            seen = touched(&changes, &dir);
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(seen, "the edit reached the change ring");
    }
}
