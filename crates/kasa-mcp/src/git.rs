//! Git status snapshot for the webview panel. We shell out to
//! `git status --porcelain=v2 --branch` rather than linking a git library:
//! porcelain v2 is the documented machine format git promises to keep
//! stable, and a subprocess can't pull a heavy libgit2 dependency or its
//! version skew into the host. The webview polls this over HTTP.

use std::path::Path;
use std::process::Command;

use serde_json::{json, Value};

/// `git` invocation with the console window suppressed on Windows. kasaterm
/// is a GUI (non-console) process, so spawning a console program like git
/// flashes a fresh console window — and a Defender-throttled call (~5s) leaves
/// that empty window on screen the whole time. CREATE_NO_WINDOW keeps it
/// hidden. No-op on other platforms.
fn git_cmd() -> Command {
    let c = Command::new("git");
    #[cfg(windows)]
    let c = {
        use std::os::windows::process::CommandExt;
        let mut c = c;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        c
    };
    c
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
    let (ok, head) = run_git(repo, &["rev-parse", "--abbrev-ref", "HEAD"]);
    if !ok {
        return None;
    }
    let branch = head.trim().to_string();
    if branch.is_empty() {
        return None;
    }
    let (_o, stat) = run_git(repo, &["diff", "HEAD", "--shortstat"]);
    let (insertions, deletions) = parse_shortstat(&stat);
    // Leading `N` of " 11 files changed, …" — 0 when the tree is clean.
    let files = stat
        .split_whitespace()
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    Some(GitBadge {
        branch,
        files,
        insertions,
        deletions,
    })
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
    if let Ok(home) = std::env::var("HOME") {
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

/// 작업트리의 모든 변경을 stage (`git add -A`) — 패널의 "전체 Stage".
pub fn git_stage_all(repo: &Path) -> Value {
    let (ok, out) = run_git(repo, &["add", "-A"]);
    json!({ "ok": ok, "output": out.trim() })
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
