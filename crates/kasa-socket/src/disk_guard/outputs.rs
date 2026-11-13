//! 다시 만들 수 있는 빌드 산출물 찾기·재기·지우기. 무엇을 산출물로 보는지와 지우기 전 확인은
//! [docs/disk-guard.md](../../../../docs/disk-guard.md) 의 「자동으로 지우는 것」과 같다.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::Serialize;

/// cargo 가 target 뿌리에 두는 CACHEDIR.TAG. 서명 줄은 캐시 폴더 표준이라 pytest 등도 똑같이 쓰니(레포의
/// `.pytest_cache` 가 target 으로 잡혔다) 만든 도구 줄까지 본다.
pub(crate) const CARGO_TAG: &str = "Signature: 8a477f597d28d172789f06886806bc55\n# This file is a cache directory tag created by cargo.";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Cargo,
    Flutter,
    Xcode,
}

/// 찾은 산출물 하나. `workspace` 는 그 산출물을 만드는 작업 공간이다 — 누가 그 안에서 일하면 쓰는 중으로 본다.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
    pub path: PathBuf,
    pub kind: Kind,
    pub workspace: PathBuf,
}

/// 찾을 자리와 그 아래로 내려갈 깊이.
#[derive(Clone, Debug)]
pub struct Root {
    pub path: PathBuf,
    pub depth: usize,
}

pub(crate) fn real(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn is_real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
}

pub(crate) fn is_cargo_target(path: &Path) -> bool {
    std::fs::read_to_string(path.join("CACHEDIR.TAG")).is_ok_and(|text| text.replace("\r\n", "\n").starts_with(CARGO_TAG))
}

fn is_flutter_project(path: &Path) -> bool {
    std::fs::symlink_metadata(path.join("pubspec.yaml")).is_ok_and(|m| m.is_file())
}

/// 뿌리마다 깊이만큼 내려가며 cargo target 과 flutter 프로젝트의 빌드 결과를 찾는다. DerivedData 는 바로 아래
/// 항목 하나하나가 산출물이다. 심볼릭 링크는 따라가지 않는다.
pub fn find(roots: &[Root], derived_data: Option<&Path>) -> Vec<Output> {
    let mut found = Vec::new();
    for root in roots {
        let mut stack = vec![(root.path.clone(), root.depth)];
        while let Some((dir, left)) = stack.pop() {
            if !is_real_dir(&dir) {
                continue;
            }
            if is_cargo_target(&dir) {
                let workspace = dir.parent().map_or_else(|| dir.clone(), Path::to_path_buf);
                found.push(Output { path: dir, kind: Kind::Cargo, workspace });
                continue;
            }
            let flutter = is_flutter_project(&dir);
            if flutter {
                for sub in ["build", ".dart_tool/flutter_build"] {
                    let path = dir.join(sub);
                    if is_real_dir(&path) && is_real_dir(path.parent().unwrap_or(&dir)) {
                        found.push(Output { path, kind: Kind::Flutter, workspace: dir.clone() });
                    }
                }
            }
            if left == 0 {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                let name = entry.file_name();
                if flutter && (name == "build" || name == ".dart_tool") {
                    continue;
                }
                if name == ".git" || name == "node_modules" {
                    continue;
                }
                stack.push((entry.path(), left - 1));
            }
        }
    }
    if let Some(dd) = derived_data.filter(|d| is_real_dir(d)) {
        if let Ok(entries) = std::fs::read_dir(dd) {
            for entry in entries.flatten() {
                let path = entry.path();
                if is_real_dir(&path) {
                    found.push(Output { workspace: path.clone(), path, kind: Kind::Xcode });
                }
            }
        }
    }
    let mut seen = HashSet::new();
    found.retain(|o| seen.insert(real(&o.path)));
    found.sort_by(|a, b| a.path.cmp(&b.path));
    found
}

fn mtime_secs(path: &Path) -> Option<u64> {
    let modified = std::fs::symlink_metadata(path).ok()?.modified().ok()?;
    Some(modified.duration_since(UNIX_EPOCH).ok()?.as_secs())
}

/// 뿌리와 그 아래 두 겹의 가장 늦은 수정 시각(초) — 빌드가 돌면 이 자리들(debug·release·<triple>/debug,
/// Build/Intermediates.noindex)이 바뀐다.
pub(crate) fn last_used(path: &Path) -> u64 {
    let mut newest = mtime_secs(path).unwrap_or(0);
    let Ok(children) = std::fs::read_dir(path) else { return newest };
    for child in children.flatten() {
        let child = child.path();
        newest = newest.max(mtime_secs(&child).unwrap_or(0));
        if is_real_dir(&child) {
            if let Ok(grand) = std::fs::read_dir(&child) {
                for g in grand.flatten() {
                    newest = newest.max(mtime_secs(&g.path()).unwrap_or(0));
                }
            }
        }
    }
    newest
}

/// 이 산출물 안 심볼릭 링크들이 가리키는 실제 경로(세 겹까지).
pub(crate) fn links_into(path: &Path) -> HashSet<PathBuf> {
    let mut out = HashSet::new();
    let mut stack = vec![(path.to_path_buf(), 3usize)];
    while let Some((here, left)) = stack.pop() {
        let Ok(children) = std::fs::read_dir(&here) else { continue };
        for child in children.flatten() {
            let child = child.path();
            match std::fs::symlink_metadata(&child) {
                Ok(m) if m.file_type().is_symlink() => {
                    out.insert(real(&child));
                }
                Ok(m) if m.is_dir() && left > 0 => stack.push((child, left - 1)),
                _ => {}
            }
        }
    }
    out
}

/// 지우기 직전에 다시 확인한다 — 링크가 아니고, 작업 범위 안이고, 손대지 않는 자리가 아니고, 산출물 표식이
/// 그대로인 실제 폴더만 지운다. 지우지 않은 까닭을 돌려준다.
pub(crate) fn remove(output: &Output, allowed: &[PathBuf], deny: &[PathBuf]) -> Result<(), &'static str> {
    let meta = std::fs::symlink_metadata(&output.path).map_err(|_| "이미 없어요")?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err("링크이거나 폴더가 아니에요");
    }
    let real = std::fs::canonicalize(&output.path).map_err(|_| "실제 경로를 못 읽었어요")?;
    if real.components().count() < 4 {
        return Err("너무 얕은 경로예요");
    }
    if !allowed.iter().any(|root| real.starts_with(root) && real != *root) {
        return Err("작업 범위 밖이에요");
    }
    if deny.iter().any(|d| real.starts_with(d) || d.starts_with(&real)) {
        return Err("손대지 않는 자리예요");
    }
    let marked = match output.kind {
        Kind::Cargo => is_cargo_target(&real),
        Kind::Flutter => {
            let project = if real.ends_with(".dart_tool/flutter_build") {
                real.parent().and_then(Path::parent)
            } else {
                real.parent().filter(|_| real.file_name().is_some_and(|n| n == "build"))
            };
            project.is_some_and(is_flutter_project)
        }
        Kind::Xcode => real.parent().is_some_and(|p| p.file_name().is_some_and(|n| n == "DerivedData")),
    };
    if !marked {
        return Err("산출물 표식이 없어요");
    }
    std::fs::remove_dir_all(&real).map_err(|_| "지우다 실패했어요")
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kasa-disk-guard-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        real(&dir)
    }

    fn cargo_target(at: &Path) -> PathBuf {
        std::fs::create_dir_all(at.join("debug")).unwrap();
        std::fs::write(at.join("CACHEDIR.TAG"), format!("{CARGO_TAG}\n")).unwrap();
        at.to_path_buf()
    }

    #[test]
    fn only_cargo_tagged_real_folders_and_flutter_outputs_are_found() {
        let root = scratch("find");
        let target = cargo_target(&root.join("real/target"));
        std::fs::create_dir_all(root.join("plain/target")).unwrap();
        std::fs::create_dir_all(root.join("other/target")).unwrap();
        std::fs::write(root.join("other/target/CACHEDIR.TAG"), "Signature: something else\n").unwrap();
        std::fs::create_dir_all(root.join("real/.pytest_cache")).unwrap();
        std::fs::write(
            root.join("real/.pytest_cache/CACHEDIR.TAG"),
            "Signature: 8a477f597d28d172789f06886806bc55\n# This file is a cache directory tag created by pytest.\n",
        )
        .unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
        let mobile = root.join("app/mobile");
        std::fs::create_dir_all(mobile.join("build/ios")).unwrap();
        std::fs::create_dir_all(mobile.join(".dart_tool/flutter_build/x")).unwrap();
        std::fs::create_dir_all(mobile.join(".dart_tool/package_config_dir")).unwrap();
        std::fs::write(mobile.join("pubspec.yaml"), "name: x\n").unwrap();
        std::fs::create_dir_all(root.join("no-pubspec/build")).unwrap();
        let found = find(&[Root { path: root.clone(), depth: 2 }], None);
        let paths: Vec<_> = found.iter().map(|o| (o.path.clone(), o.kind)).collect();
        assert_eq!(paths, vec![
            (mobile.join(".dart_tool/flutter_build"), Kind::Flutter),
            (mobile.join("build"), Kind::Flutter),
            (target, Kind::Cargo),
        ]);
        assert_eq!(found[0].workspace, mobile);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn derived_data_entries_are_outputs_of_their_own() {
        let root = scratch("derived");
        let dd = root.join("DerivedData");
        std::fs::create_dir_all(dd.join("Runner-abc/Build")).unwrap();
        std::fs::write(dd.join("stray.txt"), "x").unwrap();
        let found = find(&[], Some(&dd));
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].kind, found[0].workspace.clone()), (Kind::Xcode, dd.join("Runner-abc")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn removal_refuses_outside_scope_denied_unmarked_and_linked_paths() {
        let root = scratch("remove");
        let scope = root.join("kt-a");
        let target = cargo_target(&scope.join("target"));
        let out = |path: &Path, kind| Output { path: path.to_path_buf(), kind, workspace: scope.clone() };
        let allowed = vec![scope.clone()];
        let outside = cargo_target(&root.join("elsewhere/target"));
        assert_eq!(remove(&out(&outside, Kind::Cargo), &allowed, &[]), Err("작업 범위 밖이에요"));
        assert!(outside.exists());
        assert_eq!(remove(&out(&target, Kind::Cargo), &allowed, &[scope.clone()]), Err("손대지 않는 자리예요"));
        let plain = scope.join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert_eq!(remove(&out(&plain, Kind::Cargo), &allowed, &[]), Err("산출물 표식이 없어요"));
        assert_eq!(remove(&out(&plain, Kind::Flutter), &allowed, &[]), Err("산출물 표식이 없어요"));
        std::os::unix::fs::symlink(&outside, scope.join("alias")).unwrap();
        assert_eq!(remove(&out(&scope.join("alias"), Kind::Cargo), &allowed, &[]), Err("링크이거나 폴더가 아니에요"));
        assert!(outside.exists());
        assert_eq!(remove(&out(&scope, Kind::Cargo), &allowed, &[]), Err("작업 범위 밖이에요"));
        assert_eq!(remove(&out(&target, Kind::Cargo), &allowed, &[]), Ok(()));
        assert!(!target.exists() && scope.exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
