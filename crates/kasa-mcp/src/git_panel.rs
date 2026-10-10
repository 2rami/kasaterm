use kasa_socket::backend::Backend;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime};

pub const SCHEMA: &str = "kasa.git-panel.v2";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub machine_id: String,
    pub pane: String,
    pub surface_key: String,
    pub cwd: String,
}

pub fn absolute_path(path: &str) -> bool {
    !path.chars().any(char::is_control)
        && (path.starts_with('/') || path.starts_with("\\\\")
            || (path.as_bytes().get(1) == Some(&b':')
                && path.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
                && matches!(path.as_bytes().get(2), Some(b'\\' | b'/'))))
}

fn resolve(backend: &dyn Backend, query: &HashMap<String, String>) -> Result<Source, &'static str> {
    let machine_id = crate::mobile::machine_identity().ok_or("source_missing")?;
    if query.get("machine_id") != Some(&machine_id) { return Err("source_changed"); }
    let pane = query.get("pane").filter(|p| !p.is_empty()).ok_or("source_missing")?;
    let key = query.get("surface_key").filter(|k| !k.is_empty()).ok_or("update_needed")?;
    if crate::surface_keys::get(pane).as_ref() != Some(key) { return Err("source_changed"); }
    if crate::remote::remote_info(pane).is_some() { return Err("mirror_source"); }
    let cwd = backend.pane_cwds().into_iter().find(|(id, _)| id == pane)
        .map(|(_, cwd)| cwd).filter(|cwd| absolute_path(cwd)).ok_or("cwd_unavailable")?;
    if query.get("cwd").is_some_and(|expected| expected != &cwd) { return Err("source_changed"); }
    Ok(Source { machine_id, pane: pane.clone(), surface_key: key.clone(), cwd })
}

const READ_BUDGET: std::time::Duration = std::time::Duration::from_secs(8);

pub fn read(backend: &dyn Backend, query: &HashMap<String, String>) -> Value {
    let error = |code: &str| json!({"schema": SCHEMA, "ok": false, "error": code});
    if query.get("schema").map(String::as_str) != Some(SCHEMA) { return error("update_needed"); }
    let source = match resolve(backend, query) { Ok(source) => source, Err(code) => return error(code) };
    let commits = query.get("commits").and_then(|s| s.parse().ok()).unwrap_or(20usize).clamp(1, 200);
    // 보기 기기의 원격 GET 이 10초에 끊으므로 그 안에 답한다.
    let view = match panel_view(Path::new(&source.cwd), None, &[], commits, READ_BUDGET, None) {
        Ok(view) => view,
        Err(_) => return error("git_unavailable"),
    };
    // A pane may be replaced or change directory while its Git subprocess runs.
    if resolve(backend, query).as_ref() != Ok(&source) { return error("source_changed"); }
    json!({"schema": SCHEMA, "ok": true, "source": source, "view": view})
}

/// 보기 기기의 Git 열이 기다리는 한도. 원격 GET 이 10초에 끊고, 관문 우회는 답 머리를 20초까지만 기다린다.
pub const WAIT_CAP_MS: u64 = 8000;

/// 원본 칸의 Git 이 바뀌었을 수 있을 때까지 쥔다 — 그 칸의 작업 트리(부모 폴더 칸이면 그 아래 저장소들)에서 파일이
/// 바뀌었다(`git_watch`, 누가 고쳤든). 보기 기기는 이 답이 오면 `read` 로 다시 읽는다. `since` 가 없으면 지금 번호를
/// 바로 준다. 답 `{schema, ok, seq, changed, live}` — 바뀐 경로는 내보내지 않는다.
pub async fn wait(backend: std::sync::Arc<dyn Backend>, query: HashMap<String, String>) -> Value {
    let error = |code: &str| json!({"schema": SCHEMA, "ok": false, "error": code});
    if query.get("schema").map(String::as_str) != Some(SCHEMA) { return error("update_needed"); }
    let since = query.get("since").and_then(|s| s.parse::<u64>().ok());
    let hold = query.get("wait_ms").and_then(|s| s.parse::<u64>().ok()).unwrap_or(WAIT_CAP_MS).min(WAIT_CAP_MS);
    let resolved = tokio::task::spawn_blocking(move || {
        let source = resolve(backend.as_ref(), &query)?;
        let cwd = PathBuf::from(&source.cwd);
        let live = watch_panel(&cwd);
        Ok::<_, &'static str>((panel_roots(&cwd), live))
    }).await;
    let (roots, live) = match resolved { Ok(Ok(found)) => found, Ok(Err(code)) => return error(code), Err(_) => return error("git_unavailable") };
    // `live` — 원본이 그 칸의 작업 트리를 감시한다. 보기 기기는 그 칸의 주기 조회를 늦춘다.
    let answer = |seq: u64, changed: bool| json!({"schema": SCHEMA, "ok": true, "seq": seq, "changed": changed, "live": live});
    let Some(mut seen) = since else { return answer(crate::git_watch::seq(), false) };
    let deadline = tokio::time::Instant::now() + Duration::from_millis(hold);
    loop {
        let (seq, changes, lost) = crate::git_watch::changes_since(seen);
        // 번호가 줄었으면 원본 앱이 다시 떴다 — 그 사이 무엇이 바뀌었는지 모른다.
        if seen > seq || lost || roots.iter().any(|root| crate::git_watch::touched(&changes, root)) {
            return answer(seq, true);
        }
        seen = seq;
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() { return answer(seq, false); }
        crate::git_watch::wait(seen, left).await;
    }
}

/// 이 폴더를 품은 작업 트리의 뿌리 — `.git` 이 있는 가장 가까운 조상. git 을 띄우지 않는다.
pub fn repo_root_of(dir: &Path) -> Option<PathBuf> {
    dir.ancestors().find(|a| a.join(".git").exists()).map(Path::to_path_buf)
}

/// 아래로 내려가지 않는 폴더 — 숨은 폴더와 빌드·의존성 폴더.
fn skip_dir(name: &str) -> bool {
    name.starts_with('.') || matches!(name, "node_modules" | "target" | "dist" | "build" | "vendor" | "Pods" | "venv")
}

const NESTED_DEPTH: usize = 2;
const NESTED_CAP: usize = 64;
const NESTED_VISIT_CAP: usize = 4000;
const NESTED_FRESH: Duration = Duration::from_secs(30);

static NESTED: LazyLock<Mutex<HashMap<PathBuf, (Instant, Vec<PathBuf>)>>> = LazyLock::new(Default::default);

/// 칸 폴더가 여러 저장소를 담은 부모일 때 그 아래(두 단까지)의 저장소들. 찾은 저장소 안으로는 더 내려가지 않는다.
/// 폴더를 훑는 값이 들어 30초 기억한다 — 그 사이 새로 받은 레포는 다음에 잡힌다.
pub fn nested_repos(dir: &Path) -> Vec<PathBuf> {
    let now = Instant::now();
    if let Some((at, repos)) = NESTED.lock().unwrap().get(dir) {
        if now.duration_since(*at) < NESTED_FRESH {
            return repos.clone();
        }
    }
    let mut repos = Vec::new();
    let mut level = vec![dir.to_path_buf()];
    let mut visits = 0usize;
    for _ in 0..NESTED_DEPTH {
        let mut next = Vec::new();
        for parent in level {
            let Ok(entries) = std::fs::read_dir(&parent) else { continue };
            let mut children: Vec<PathBuf> = entries
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .filter(|e| e.file_name().to_str().is_some_and(|n| !skip_dir(n)))
                .map(|e| e.path())
                .collect();
            children.sort();
            for child in children {
                visits += 1;
                if visits > NESTED_VISIT_CAP || repos.len() >= NESTED_CAP {
                    break;
                }
                if child.join(".git").exists() {
                    repos.push(child);
                } else {
                    next.push(child);
                }
            }
        }
        level = next;
    }
    let mut cache = NESTED.lock().unwrap();
    if cache.len() > 32 {
        cache.retain(|_, (at, _)| now.duration_since(*at) < NESTED_FRESH);
    }
    cache.insert(dir.to_path_buf(), (now, repos.clone()));
    repos
}

/// 작업 트리의 git 폴더 — 워크트리·서브모듈은 `.git` 이 `gitdir: …` 를 담은 파일이다.
fn git_dir(repo: &Path) -> Option<PathBuf> {
    let dot = repo.join(".git");
    if dot.is_dir() {
        return Some(dot);
    }
    let text = std::fs::read_to_string(&dot).ok()?;
    let target = Path::new(text.strip_prefix("gitdir:")?.trim());
    Some(if target.is_absolute() { target.to_path_buf() } else { repo.join(target) })
}

/// 저장소를 마지막으로 누가 만진 때 — 커밋·체크아웃·스테이지·status(git 이 남기는 HEAD·reflog·인덱스 시각)와 이 앱이
/// 감시한 작업 트리 변경 가운데 늦은 것. 누가(claude·codex·사람) 했는지는 보지 않는다.
pub fn repo_activity(repo: &Path) -> Option<SystemTime> {
    let mtime = |path: PathBuf| std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let git = git_dir(repo);
    ["HEAD", "logs/HEAD", "index", "FETCH_HEAD"]
        .into_iter()
        .filter_map(|name| git.as_ref().and_then(|g| mtime(g.join(name))))
        .chain(crate::git_watch::last_change_under(repo))
        .max()
}

/// 칸 폴더 아래 저장소를 최근에 만진 순으로. `busy` 는 지금 그 칸에서 도는 명령들의 폴더와 그 명령이 시작된 때 —
/// 그 안의 저장소는 그때 만지기 시작했다.
pub fn rank_repos(dir: &Path, busy: &[(PathBuf, SystemTime)]) -> Vec<PathBuf> {
    let mut ranked: Vec<(PathBuf, Option<SystemTime>)> = nested_repos(dir)
        .into_iter()
        .map(|repo| {
            let started = busy.iter().filter(|(cwd, _)| cwd.starts_with(&repo)).map(|(_, at)| *at).max();
            let at = repo_activity(&repo).max(started);
            (repo, at)
        })
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked.into_iter().map(|(repo, _)| repo).collect()
}

/// git 이 준 뿌리(실제 경로)와 폴더에서 찾은 뿌리(`/tmp` 같은 링크 그대로)가 같은 곳인가.
fn same_tree(a: &Path, b: &Path) -> bool {
    a == b || std::fs::canonicalize(a).ok() == std::fs::canonicalize(b).ok()
}

/// 이 칸 폴더의 Git 열이 기대는 작업 트리들 — 저장소 안이면 그 뿌리 하나, 아니면 그 아래 저장소들.
pub fn panel_roots(cwd: &Path) -> Vec<PathBuf> {
    match repo_root_of(cwd) {
        Some(root) => vec![root],
        None => nested_repos(cwd),
    }
}

/// 그 작업 트리들의 파일 변경 감시를 세우거나 이어 간다. 하나라도 감시하지 못하면 false — 부르는 쪽은 주기로 읽는다.
/// 저장소가 아닌 폴더(홈 같은)는 감시하지 않는다.
pub fn watch_panel(cwd: &Path) -> bool {
    let roots = panel_roots(cwd);
    !roots.is_empty() && roots.iter().fold(true, |all, root| crate::git_watch::ensure(root) && all)
}

/// 칸 폴더의 Git 열. 폴더가 저장소면 그대로, 여러 저장소를 담은 부모면 `choice`(사람이 고른 것) 또는 가장 최근에
/// 만진 저장소를 읽고 `repos` 에 고를 거리를 최근 순으로 싣는다. `cwd` 는 칸 폴더 그대로 둔다 — 열이 어느 칸의 것인지
/// 가르는 값이다.
///
/// `same_refs` 는 바로 앞에 읽은 이 칸의 열이고 그 뒤 git 지문이 그대로다 — 같은 레포를 다시 읽는 것이면 작업 트리 쪽만
/// 읽는다(`git_panel_snapshot_within`).
pub fn panel_view(cwd: &Path, choice: Option<&Path>, busy: &[(PathBuf, SystemTime)], commits: usize, budget: Duration,
    same_refs: Option<&Value>) -> Result<Value, String> {
    let prior_root = same_refs.and_then(|prior| prior["repo_root"].as_str()).map(PathBuf::from);
    if let Some(root) = repo_root_of(cwd) {
        let same_refs = same_refs.filter(|_| prior_root.as_deref().is_some_and(|prior| same_tree(prior, &root)));
        return crate::git::git_panel_snapshot_within(cwd, commits, budget, same_refs);
    }
    let repos = rank_repos(cwd, busy);
    let Some(pick) = choice.and_then(|c| repos.iter().find(|r| r.as_path() == c)).or(repos.first()).cloned() else {
        return crate::git::git_panel_snapshot_within(cwd, commits, budget, None);
    };
    let same_refs = same_refs.filter(|_| prior_root.as_deref().is_some_and(|prior| same_tree(prior, &pick)));
    let mut view = crate::git::git_panel_snapshot_within(&pick, commits, budget, same_refs)?;
    if view.get("repo_root").and_then(Value::as_str).is_none() {
        view["repo_root"] = json!(pick.to_string_lossy());
    }
    view["cwd"] = json!(cwd.to_string_lossy());
    view["repos"] = json!(repos);
    Ok(view)
}

pub fn validate_response(value: &Value, expected: &Source) -> Result<Value, &'static str> {
    if value.get("schema").and_then(Value::as_str) != Some(SCHEMA) { return Err("update_needed"); }
    if value.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(match value.get("error").and_then(Value::as_str) {
            Some("update_needed") => "update_needed",
            Some("source_changed" | "source_missing" | "mirror_source") => "source_changed",
            Some("cwd_unavailable") => "cwd_unavailable",
            _ => "git_unavailable",
        });
    }
    let source: Source = serde_json::from_value(value.get("source").cloned().unwrap_or(Value::Null))
        .map_err(|_| "invalid_response")?;
    if source.machine_id != expected.machine_id || source.pane != expected.pane
        || source.surface_key != expected.surface_key || !absolute_path(&source.cwd)
        || (!expected.cwd.is_empty() && source.cwd != expected.cwd) { return Err("source_changed"); }
    let view = value.get("view").filter(|v| v.is_object()).ok_or("invalid_response")?;
    if view.get("cwd").and_then(Value::as_str) != Some(source.cwd.as_str())
        || view.get("branch_list").and_then(Value::as_array).is_none()
        || view.get("no_repo").and_then(Value::as_bool).is_none() { return Err("invalid_response"); }
    Ok(view.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> Source {
        Source { machine_id: "machine-a".into(), pane: "%4".into(), surface_key: "stable-a".into(), cwd: "/repo".into() }
    }

    fn response(source: &Source) -> Value {
        json!({"schema": SCHEMA, "ok": true, "source": source,
            "view": {"cwd": source.cwd, "branch_list": [], "no_repo": false}})
    }

    #[test]
    fn old_server_never_becomes_a_non_repository_snapshot() {
        assert_eq!(validate_response(&json!({"ok": false, "error": "path required"}), &source()), Err("update_needed"));
        assert_eq!(validate_response(&json!({"ok": true, "view": {"cwd": "/repo"}}), &source()), Err("update_needed"));
    }

    #[test]
    fn response_must_match_device_surface_and_directory() {
        let expected = source();
        assert!(validate_response(&response(&expected), &expected).is_ok());
        for field in ["machine_id", "pane", "surface_key", "cwd"] {
            let mut value = response(&expected);
            value["source"][field] = json!("wrong");
            assert_eq!(validate_response(&value, &expected), Err("source_changed"));
        }
        let mut value = response(&expected);
        value["view"]["cwd"] = json!("/other-repo");
        assert_eq!(validate_response(&value, &expected), Err("invalid_response"));
    }

    fn git(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git").arg("-C").arg(dir).args(args)
            .env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t").env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t")
            .output().unwrap().status.success();
        assert!(ok, "git {args:?}");
    }

    fn repo(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("a.txt"), "a").unwrap();
        git(dir, &["add", "a.txt"]);
        git(dir, &["commit", "-q", "-m", "first"]);
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kasa-nested-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::canonicalize(dir).unwrap()
    }

    fn age(repo: &Path, secs: u64) {
        let at = SystemTime::now() - Duration::from_secs(secs);
        for name in ["HEAD", "logs/HEAD", "index"] {
            let file = std::fs::File::options().write(true).open(repo.join(".git").join(name)).unwrap();
            file.set_modified(at).unwrap();
        }
    }

    #[test]
    fn a_parent_folder_lists_the_repositories_beneath_it_without_entering_them() {
        let parent = scratch("list");
        repo(&parent.join("front"));
        repo(&parent.join("group/back"));
        repo(&parent.join("front/vendor-inner"));
        repo(&parent.join("node_modules/pkg"));
        repo(&parent.join(".hidden/x"));
        std::fs::create_dir_all(parent.join("deep/a/b")).unwrap();
        repo(&parent.join("deep/a/b/too-deep"));
        assert_eq!(nested_repos(&parent), vec![parent.join("front"), parent.join("group/back")]);
        assert_eq!(panel_roots(&parent.join("front/src")), vec![parent.join("front")], "inside a repository the root is the one tree");
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn the_most_recently_touched_repository_comes_first_and_a_running_command_counts_from_its_start() {
        let parent = scratch("rank");
        repo(&parent.join("a"));
        repo(&parent.join("b"));
        age(&parent.join("a"), 600);
        age(&parent.join("b"), 60);
        assert_eq!(rank_repos(&parent, &[]), vec![parent.join("b"), parent.join("a")]);
        let just_now = vec![(parent.join("a/src"), SystemTime::now())];
        assert_eq!(rank_repos(&parent, &just_now)[0], parent.join("a"), "a command started now in a/ wins");
        let long_ago = vec![(parent.join("a/src"), SystemTime::now() - Duration::from_secs(3600))];
        assert_eq!(rank_repos(&parent, &long_ago)[0], parent.join("b"), "a dev server running for an hour does not hold the panel");
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn a_parent_folder_pane_reads_the_recent_repository_and_keeps_its_own_folder() {
        let parent = scratch("view");
        repo(&parent.join("a"));
        repo(&parent.join("b"));
        std::fs::write(parent.join("b/new.txt"), "x").unwrap();
        age(&parent.join("a"), 600);
        age(&parent.join("b"), 60);
        // repo_root 는 git 이 준 뿌리라 Windows 에선 `C:/…` 꼴이다 — 같은 곳인지를 제품과 같은 `same_tree` 로 본다.
        let root_is = |view: &Value, want: &Path| same_tree(Path::new(view["repo_root"].as_str().unwrap_or_default()), want);
        let view = panel_view(&parent, None, &[], 5, Duration::from_secs(20), None).unwrap();
        assert_eq!(view["cwd"], json!(parent.to_string_lossy()), "the column still belongs to the pane folder");
        assert!(root_is(&view, &parent.join("b")), "{}", view["repo_root"]);
        assert_eq!(view["no_repo"], json!(false));
        assert_eq!(view["repos"], json!([parent.join("b"), parent.join("a")]));
        let chosen = panel_view(&parent, Some(&parent.join("a")), &[], 5, Duration::from_secs(20), None).unwrap();
        assert!(root_is(&chosen, &parent.join("a")), "{}", chosen["repo_root"]);
        let foreign = panel_view(&parent, Some(Path::new("/etc")), &[], 5, Duration::from_secs(20), None).unwrap();
        assert!(root_is(&foreign, &parent.join("b")), "only a repository under the pane folder can be chosen: {}", foreign["repo_root"]);
        let inside = panel_view(&parent.join("a"), None, &[], 5, Duration::from_secs(20), None).unwrap();
        assert!(inside.get("repos").is_none(), "a pane inside a repository reads it as before");
        let empty = scratch("empty");
        assert_eq!(panel_view(&empty, None, &[], 5, Duration::from_secs(20), None).unwrap()["no_repo"], json!(true));
        let _ = std::fs::remove_dir_all(&parent);
        let _ = std::fs::remove_dir_all(&empty);
    }

    #[test]
    fn an_edit_rereads_only_the_working_tree_and_a_commit_rereads_everything() {
        let parent = scratch("refs");
        let repo_dir = parent.join("r");
        repo(&repo_dir);
        let full = crate::git::git_panel_snapshot(&repo_dir, 5).unwrap();
        std::fs::write(repo_dir.join("a.txt"), "a\nb\n").unwrap();
        let mut marked = full.clone();
        marked["commit_graph"][0]["subject"] = json!("kept from the previous read");
        let light = crate::git::git_panel_snapshot_within(&repo_dir, 5, Duration::from_secs(20), Some(&marked)).unwrap();
        assert_eq!(light["numstat"]["a.txt"], json!([2, 1]), "the edit is read");
        assert_eq!(light["commit_graph"][0]["subject"], json!("kept from the previous read"), "refs were not read again");
        git(&repo_dir, &["commit", "-qam", "second"]);
        let after_commit = crate::git::git_panel_snapshot_within(&repo_dir, 5, Duration::from_secs(20), Some(&marked)).unwrap();
        assert_eq!(after_commit["commit_graph"][0]["subject"], json!("second"), "a moved HEAD reads the graph again");
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn source_paths_support_windows_without_local_path_interpretation() {
        for path in ["/work/repo", "C:\\work\\repo", "D:/work/repo", "\\\\host\\share\\repo"] { assert!(absolute_path(path)); }
        for path in ["repo", "C:repo", "/repo\nother", ""] { assert!(!absolute_path(path)); }
    }
}
