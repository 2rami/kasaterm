//! 폴더 한 층의 git 표시 — 셸 명령 보기의 빈 화면 트리(`/term/tree?git=1`)가 줄 끝에 단다.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Default)]
pub(super) struct Marks {
    top: PathBuf,
    /// 저장소 최상위 기준 경로 → 표시 글자(M·A·D·R·?·U).
    files: HashMap<PathBuf, char>,
    /// 안에 바뀐 파일이 있는 폴더.
    dirty_dirs: HashSet<PathBuf>,
    ignored: Vec<PathBuf>,
}

impl Marks {
    pub(super) fn mark(&self, path: &Path) -> Option<char> {
        let rel = path.strip_prefix(&self.top).ok()?;
        self.files
            .get(rel)
            .copied()
            .or_else(|| self.dirty_dirs.contains(rel).then_some('•'))
            // git 은 추적 안 하는 폴더를 폴더 한 줄로만 알린다 — 안의 것도 모두 새것이다.
            .or_else(|| rel.ancestors().skip(1).any(|dir| self.files.get(dir) == Some(&'?')).then_some('?'))
    }

    pub(super) fn ignored(&self, path: &Path) -> bool {
        path.strip_prefix(&self.top).is_ok_and(|rel| self.ignored.iter().any(|i| rel.starts_with(i)))
    }
}

fn sign(xy: &str) -> char {
    let mut chars = xy.chars();
    let (x, y) = (chars.next().unwrap_or('.'), chars.next().unwrap_or('.'));
    match if y != '.' { y } else { x } {
        'A' => 'A',
        'D' => 'D',
        'R' | 'C' => 'R',
        _ => 'M',
    }
}

/// `git status --porcelain=v2 -z --ignored=matching` 출력을 읽는다.
pub(super) fn parse(raw: &[u8], top: &Path) -> Marks {
    let mut marks = Marks { top: top.to_path_buf(), ..Default::default() };
    let text = String::from_utf8_lossy(raw);
    let mut tokens = text.split('\0');
    while let Some(token) = tokens.next() {
        let mut add = |path: &str, sign: char| {
            marks.files.insert(PathBuf::from(path), sign);
        };
        if let Some(rest) = token.strip_prefix("1 ") {
            let fields: Vec<&str> = rest.splitn(8, ' ').collect();
            if let (Some(xy), Some(path)) = (fields.first(), fields.get(7)) {
                add(path, sign(xy));
            }
        } else if let Some(rest) = token.strip_prefix("2 ") {
            let fields: Vec<&str> = rest.splitn(9, ' ').collect();
            if let (Some(xy), Some(path)) = (fields.first(), fields.get(8)) {
                add(path, sign(xy));
            }
            // 이름을 바꾼 줄은 옛 경로가 다음 토큰으로 따로 온다.
            tokens.next();
        } else if let Some(rest) = token.strip_prefix("u ") {
            if let Some(path) = rest.splitn(10, ' ').nth(9) {
                add(path, 'U');
            }
        } else if let Some(path) = token.strip_prefix("? ") {
            add(path.trim_end_matches('/'), '?');
        } else if let Some(path) = token.strip_prefix("! ") {
            marks.ignored.push(PathBuf::from(path.trim_end_matches('/')));
        }
    }
    for path in marks.files.keys() {
        for dir in path.ancestors().skip(1) {
            if dir.as_os_str().is_empty() {
                break;
            }
            marks.dirty_dirs.insert(dir.to_path_buf());
        }
    }
    marks
}

fn git(dir: &Path, args: &[&std::ffi::OsStr]) -> Option<Vec<u8>> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().ok()?;
    out.status.success().then_some(out.stdout)
}

/// `dir` 은 실제 경로(`canonicalize`)여야 한다 — git 이 주는 최상위가 실제 경로라 `/tmp` 같은 링크로는 안 맞는다.
/// 무시된 폴더는 한 줄로만 받는다(`matching`) — `target/` 안을 훑지 않게.
pub(super) fn read(dir: &Path) -> Option<Marks> {
    let top = git(dir, &["rev-parse".as_ref(), "--show-toplevel".as_ref()])?;
    let top = PathBuf::from(String::from_utf8_lossy(&top).trim());
    let args: [&std::ffi::OsStr; 7] = [
        "status".as_ref(),
        "--porcelain=v2".as_ref(),
        "-z".as_ref(),
        "--untracked-files=normal".as_ref(),
        "--ignored=matching".as_ref(),
        "--".as_ref(),
        dir.as_os_str(),
    ];
    Some(parse(&git(&top, &args)?, &top))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain_v2_files_renames_dirty_folders_and_ignored() {
        let raw = [
            "1 .M N... 100644 100644 100644 a a src/main.rs",
            "1 A. N... 000000 100644 100644 0 a docs/new file.md",
            "2 R. N... 100644 100644 100644 a a R100 src/ui/draw.rs",
            "src/old.rs",
            "u UU N... 100644 100644 100644 100644 a b c conflict.txt",
            "? notes/",
            "1 D. N... 100644 000000 000000 a 0 gone.txt",
            "! target/",
            "! app/.DS_Store",
            "",
        ]
        .join("\0");
        let marks = parse(raw.as_bytes(), Path::new("/repo"));
        let at = |p: &str| marks.mark(&Path::new("/repo").join(p));
        assert_eq!(at("src/main.rs"), Some('M'));
        assert_eq!(at("docs/new file.md"), Some('A'), "공백 든 경로");
        assert_eq!(at("src/ui/draw.rs"), Some('R'));
        assert_eq!(at("src/old.rs"), None, "이름 바꾼 옛 경로는 표시하지 않는다");
        assert_eq!(at("conflict.txt"), Some('U'));
        assert_eq!(at("notes"), Some('?'));
        assert_eq!(at("notes/today/a.md"), Some('?'), "추적 안 하는 폴더 안");
        assert_eq!(at("gone.txt"), Some('D'));
        assert_eq!(at("src"), Some('•'));
        assert_eq!(at("README.md"), None);
        assert_eq!(marks.mark(Path::new("/elsewhere/src")), None);
        let ignored = |p: &str| marks.ignored(&Path::new("/repo").join(p));
        assert!(ignored("target") && ignored("target/release/kasaterm") && ignored("app/.DS_Store"));
        assert!(!ignored("app") && !ignored("targets"));
    }
}
