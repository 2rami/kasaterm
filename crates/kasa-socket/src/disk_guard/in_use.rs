//! 누가 어느 폴더를 쓰고 있나 — 지금 쓰는 산출물은 남긴다. 경로는 모두 실제 경로로 맞춰 견준다
//! (`/tmp` ↔ `/private/tmp`, 심볼릭 링크, 상대 `CARGO_TARGET_DIR`).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::outputs::{real, Output};

/// 프로세스 조회가 실패했다 — 누가 무엇을 쓰는지 모르면 지우지 않는다.
#[derive(Debug)]
pub struct Unknown(pub &'static str);

fn env_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split_whitespace().find_map(|token| token.strip_prefix(key)?.strip_prefix('='))
}

/// `ps -E` 한 줄마다 `PWD` 와 `CARGO_TARGET_DIR`(상대면 그 프로세스 PWD 기준).
pub(crate) fn env_paths(ps: &str) -> HashSet<PathBuf> {
    let mut out = HashSet::new();
    for line in ps.lines() {
        let pwd = env_value(line, "PWD");
        if let Some(pwd) = pwd {
            out.insert(PathBuf::from(pwd));
        }
        if let Some(target) = env_value(line, "CARGO_TARGET_DIR") {
            let target = Path::new(target);
            out.insert(if target.is_absolute() { target.to_path_buf() } else { Path::new(pwd.unwrap_or("/")).join(target) });
        }
    }
    out
}

/// `lsof -F n` 의 경로 줄 — 작업 폴더(cwd)·실행 파일(txt)·열린 파일.
pub(crate) fn lsof_paths(lsof: &str) -> HashSet<PathBuf> {
    lsof.lines().filter_map(|line| line.strip_prefix('n')).filter(|p| p.starts_with('/')).map(PathBuf::from).collect()
}

/// 이 사용자의 프로세스가 쓰는 경로 전부(실제 경로). `ps -E` 는 다른 사용자의 환경을 못 보지만 지우는 것도 이
/// 사용자 폴더뿐이다.
pub fn used_paths() -> Result<HashSet<PathBuf>, Unknown> {
    let ps = super::output_within(
        std::process::Command::new("ps").args(["-axEww", "-o", "command="]),
        Duration::from_secs(60),
    )
    .filter(|o| o.status.success() && !o.stdout.is_empty())
    .ok_or(Unknown("ps 를 못 읽었어요"))?;
    // SAFETY: getuid 는 실패하지 않는다.
    let uid = unsafe { libc::getuid() };
    let lsof = super::output_within(
        std::process::Command::new("lsof").args(["-n", "-P", "-w", "-F", "n", "-u", &uid.to_string()]),
        Duration::from_secs(300),
    )
    .filter(|o| matches!(o.status.code(), Some(0 | 1)) && !o.stdout.is_empty())
    .ok_or(Unknown("lsof 를 못 읽었어요"))?;
    let ps = String::from_utf8_lossy(&ps.stdout);
    let lsof = String::from_utf8_lossy(&lsof.stdout);
    Ok(env_paths(&ps).into_iter().chain(lsof_paths(&lsof)).map(|p| real(&p)).collect())
}

/// 그 산출물 안이나 그 작업 공간 안을 누가 쓰고 있나.
pub(crate) fn in_use(output: &Output, used: &HashSet<PathBuf>) -> bool {
    let (path, workspace) = (real(&output.path), real(&output.workspace));
    used.iter().any(|p| p.starts_with(&path) || p.starts_with(&workspace))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_target_dirs_resolve_against_the_process_folder() {
        let seen = env_paths("cargo test PWD=/w/ws CARGO_TARGET_DIR=out\ncargo build CARGO_TARGET_DIR=/abs/t PWD=/else\n");
        assert!(seen.contains(Path::new("/w/ws/out")));
        assert!(seen.contains(Path::new("/abs/t")));
        assert!(seen.contains(Path::new("/else")));
        assert_eq!(env_paths("cargo XPWD=/nope"), HashSet::new());
    }

    #[test]
    fn lsof_keeps_only_path_lines() {
        let seen = lsof_paths("p123\nfcwd\nn/a/b\nftxt\nn/c/bin\nf3\nnlocalhost:443\n");
        assert_eq!(seen, HashSet::from([PathBuf::from("/a/b"), PathBuf::from("/c/bin")]));
    }
}
