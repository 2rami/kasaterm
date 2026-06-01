//! Git status snapshot for the webview panel. We shell out to
//! `git status --porcelain=v2 --branch` rather than linking a git library:
//! porcelain v2 is the documented machine format git promises to keep
//! stable, and a subprocess can't pull a heavy libgit2 dependency or its
//! version skew into the host. The webview polls this over HTTP.

use std::path::Path;
use std::process::Command;

use serde_json::{json, Value};

/// Run `git status` in `repo` and return a JSON snapshot the webview can
/// render directly. On any git failure returns `{ "error": "..." }` so the
/// caller never has to distinguish process vs. parse errors.
pub fn git_status(repo: &Path) -> Value {
    let output = match Command::new("git")
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
    parse_porcelain_v2(&stdout)
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
    match Command::new("git").arg("-C").arg(repo).args(args).output() {
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

/// `git push`. 결과 텍스트를 그대로 패널에 돌려준다.
pub fn git_push(repo: &Path) -> Value {
    let (ok, out) = run_git(repo, &["push"]);
    json!({ "ok": ok, "output": out.trim() })
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
