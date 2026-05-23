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
        return json!({ "error": format!("git failed: {}", stderr.trim()) });
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
}
