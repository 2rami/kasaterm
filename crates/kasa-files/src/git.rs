//! 저장소 상태 — 일꾼 스레드가 git 을 돌리고 그리는 쪽은 마지막 결과만 읽는다.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{Receiver, Sender};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub top: PathBuf,
    pub branch: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub added: u32,
    pub deleted: u32,
    /// 저장소 최상위 기준 경로 → 표시 글자(M·A·D·R·?·U).
    pub files: HashMap<PathBuf, char>,
    /// 안에 바뀐 파일이 있는 폴더.
    pub dirty_dirs: HashSet<PathBuf>,
}

impl Status {
    pub fn mark(&self, path: &Path) -> Option<char> {
        let rel = path.strip_prefix(&self.top).ok()?;
        self.files
            .get(rel)
            .copied()
            .or_else(|| self.dirty_dirs.contains(rel).then_some('•'))
            // git 은 추적 안 하는 폴더를 폴더 한 줄로만 알린다 — 안의 것도 모두 새것이다.
            .or_else(|| rel.ancestors().skip(1).any(|dir| self.files.get(dir) == Some(&'?')).then_some('?'))
    }
}

fn mark(xy: &str) -> char {
    let mut chars = xy.chars();
    let (x, y) = (chars.next().unwrap_or('.'), chars.next().unwrap_or('.'));
    match if y != '.' { y } else { x } {
        'M' | 'T' => 'M',
        'A' => 'A',
        'D' => 'D',
        'R' | 'C' => 'R',
        _ => 'M',
    }
}

/// `git status --porcelain=v2 -z --branch` 출력을 읽는다.
pub fn parse(raw: &[u8], top: &Path) -> Status {
    let mut status = Status { top: top.to_path_buf(), ..Default::default() };
    let text = String::from_utf8_lossy(raw);
    let mut tokens = text.split('\0');
    while let Some(token) = tokens.next() {
        let mut add = |path: &str, sign: char| {
            status.files.insert(PathBuf::from(path), sign);
        };
        if let Some(head) = token.strip_prefix("# branch.head ") {
            status.branch = (head != "(detached)").then(|| head.to_string());
        } else if let Some(ab) = token.strip_prefix("# branch.ab ") {
            for part in ab.split(' ') {
                if let Some(n) = part.strip_prefix('+') {
                    status.ahead = n.parse().unwrap_or(0);
                } else if let Some(n) = part.strip_prefix('-') {
                    status.behind = n.parse().unwrap_or(0);
                }
            }
        } else if let Some(rest) = token.strip_prefix("1 ") {
            let fields: Vec<&str> = rest.splitn(8, ' ').collect();
            if let (Some(xy), Some(path)) = (fields.first(), fields.get(7)) {
                add(path, mark(xy));
            }
        } else if let Some(rest) = token.strip_prefix("2 ") {
            let fields: Vec<&str> = rest.splitn(9, ' ').collect();
            if let (Some(xy), Some(path)) = (fields.first(), fields.get(8)) {
                add(path, mark(xy));
            }
            // 이름을 바꾼 줄은 옛 경로가 다음 토큰으로 따로 온다.
            tokens.next();
        } else if let Some(rest) = token.strip_prefix("u ") {
            if let Some(path) = rest.splitn(10, ' ').nth(9) {
                add(path, 'U');
            }
        } else if let Some(path) = token.strip_prefix("? ") {
            add(path.trim_end_matches('/'), '?');
        }
    }
    for path in status.files.keys() {
        for dir in path.ancestors().skip(1) {
            if dir.as_os_str().is_empty() {
                break;
            }
            status.dirty_dirs.insert(dir.to_path_buf());
        }
    }
    status
}

/// `git diff --shortstat HEAD` 한 줄에서 더한 줄·지운 줄을 읽는다.
pub fn parse_shortstat(line: &str) -> (u32, u32) {
    let mut out = (0, 0);
    for part in line.split(',') {
        let mut words = part.split_whitespace();
        let Some(n) = words.next().and_then(|n| n.parse().ok()) else { continue };
        let rest = words.collect::<Vec<_>>().join(" ");
        if rest.starts_with("insertion") {
            out.0 = n;
        } else if rest.starts_with("deletion") {
            out.1 = n;
        }
    }
    out
}

fn git(dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().ok()?;
    out.status.success().then_some(out.stdout)
}

fn read(dir: &Path) -> Option<Status> {
    let top = git(dir, &["rev-parse", "--show-toplevel"])?;
    let top = PathBuf::from(String::from_utf8_lossy(&top).trim());
    let raw = git(&top, &["status", "--porcelain=v2", "-z", "--branch", "--untracked-files=normal"])?;
    let mut status = parse(&raw, &top);
    if let Some(stat) = git(&top, &["diff", "--shortstat", "HEAD"]) {
        (status.added, status.deleted) = parse_shortstat(&String::from_utf8_lossy(&stat));
    }
    Some(status)
}

/// 폴더를 받으면 상태를 읽어 돌려준다. 밀린 요청은 마지막 것만 읽는다.
pub fn spawn() -> (Sender<PathBuf>, Receiver<Option<Status>>) {
    let (ask, asks) = std::sync::mpsc::channel::<PathBuf>();
    let (tell, answers) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        while let Ok(mut dir) = asks.recv() {
            while let Ok(newer) = asks.try_recv() {
                dir = newer;
            }
            if tell.send(read(&dir)).is_err() {
                break;
            }
        }
    });
    (ask, answers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain_v2_branch_files_renames_and_dirty_folders() {
        let raw = [
            "# branch.oid abc",
            "# branch.head main",
            "# branch.upstream origin/main",
            "# branch.ab +2 -1",
            "1 .M N... 100644 100644 100644 a a src/main.rs",
            "1 A. N... 000000 100644 100644 0 a docs/new file.md",
            "2 R. N... 100644 100644 100644 a a R100 src/ui/draw.rs",
            "src/old.rs",
            "u UU N... 100644 100644 100644 100644 a b c conflict.txt",
            "? notes/",
            "1 D. N... 100644 000000 000000 a 0 gone.txt",
            "",
        ]
        .join("\0");
        let status = parse(raw.as_bytes(), Path::new("/repo"));
        assert_eq!(status.branch.as_deref(), Some("main"));
        assert_eq!((status.ahead, status.behind), (2, 1));
        let at = |p: &str| status.mark(&Path::new("/repo").join(p));
        assert_eq!(at("src/main.rs"), Some('M'));
        assert_eq!(at("docs/new file.md"), Some('A'), "공백 든 경로");
        assert_eq!(at("src/ui/draw.rs"), Some('R'));
        assert_eq!(at("src/old.rs"), None, "이름 바꾼 옛 경로는 표시하지 않는다");
        assert_eq!(at("conflict.txt"), Some('U'));
        assert_eq!(at("notes"), Some('?'));
        assert_eq!(at("notes/today/a.md"), Some('?'), "추적 안 하는 폴더 안");
        assert_eq!(at("gone.txt"), Some('D'));
        assert_eq!(at("src"), Some('•'));
        assert_eq!(at("src/ui"), Some('•'));
        assert_eq!(at("README.md"), None);
        assert_eq!(status.mark(Path::new("/elsewhere/src")), None);
    }

    #[test]
    fn detached_head_has_no_branch_and_shortstat_reads_both_sides() {
        let status = parse(b"# branch.head (detached)\0", Path::new("/r"));
        assert_eq!(status.branch, None);
        assert_eq!(parse_shortstat(" 3 files changed, 12 insertions(+), 3 deletions(-)\n"), (12, 3));
        assert_eq!(parse_shortstat(" 1 file changed, 1 insertion(+)\n"), (1, 0));
        assert_eq!(parse_shortstat(""), (0, 0));
    }
}
