use super::*;

fn temp_dir(tag: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("kasa-fork-project-{tag}-{}-{}-{n}", std::process::id(), now_ms()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

/// 시험 저장소용 git. 사람의 전역 설정(hooks·서명)을 읽지 않는다.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(["-c", "core.hooksPath=/dev/null", "-c", "commit.gpgsign=false", "-c", "user.name=t", "-c", "user.email=t@example.com"])
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// 커밋 하나 + 추적 파일 변경(a.txt·.env) + 추적 안 된 새 파일.
fn dirty_repo(tag: &str) -> PathBuf {
    let repo = temp_dir(tag).join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    git(&repo, &["init", "-q"]);
    std::fs::write(repo.join("a.txt"), "원래\n").unwrap();
    std::fs::write(repo.join("src/lib.rs"), "fn main() {}\n").unwrap();
    std::fs::write(repo.join(".env"), "TOKEN=old\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "base"]);
    git(&repo, &["remote", "add", "origin", "https://user:secret@github.com/o/r.git"]);
    std::fs::write(repo.join("a.txt"), "고친 내용\n").unwrap();
    std::fs::write(repo.join(".env"), "TOKEN=new-secret\n").unwrap();
    std::fs::write(repo.join("new.txt"), "새 파일\n").unwrap();
    repo
}

fn state(repo: &Path) -> (String, Vec<Vec<u8>>) {
    let files = ["a.txt", ".env", "new.txt", "src/lib.rs"].iter().map(|f| std::fs::read(repo.join(f)).unwrap()).collect();
    (git(repo, &["status", "--porcelain=v1", "--untracked-files=all"]), files)
}

#[test]
fn paths_outside_the_repo_or_with_secrets_are_refused() {
    for ok in ["src/main.rs", "docs/a b.md", "한글/파일.txt", ".github/workflows/ci.yml", "env.example"] {
        assert!(check_rel(ok).is_ok(), "{ok}");
    }
    for bad in [
        "", "/etc/passwd", "C:/x", "a/../b", "./a", "a//b", "a\\b", ".git/config", "sub/.git/hooks/post-checkout", ".git", ".env",
        ".env.local", "config/.ENV", "id_rsa", "keys/server.pem", ".claude/settings.json", ".codex/config.toml", ".mcp.json",
        ".envrc", ".ssh/config", ".config/gh/hosts.yml", ".npmrc", ".git-credentials", "secrets.yaml", ".vscode/tasks.json",
    ] {
        assert!(check_rel(bad).is_err(), "{bad}");
    }
}

#[test]
fn remote_urls_lose_credentials() {
    assert_eq!(public_url("https://user:tok@github.com/o/r.git?x=1").as_deref(), Some("https://github.com/o/r.git"));
    assert_eq!(public_url("ssh://git@host:22/o/r").as_deref(), Some("ssh://host:22/o/r"));
    assert_eq!(public_url("git@github.com:o/r.git").as_deref(), Some("git@github.com:o/r.git"));
    for bad in ["file:///tmp/r", "ext::sh -c touch% /tmp/x", "/local/path", "C:\\repo"] {
        assert_eq!(public_url(bad), None, "{bad}");
    }
}

#[test]
fn capture_then_apply_into_new_worktree_leaves_the_source_alone() {
    let repo = dirty_repo("roundtrip");
    let before = state(&repo);
    let files = ["new.txt", ".env", "../outside", "missing.txt"].map(String::from);
    let captured = capture_project(&repo, &files, Some(0), &ProjectLimits::default()).unwrap();
    let snap = &captured.snapshot;
    assert_eq!(snap.commit.as_deref(), Some(git(&repo, &["rev-parse", "HEAD"]).trim()));
    assert_eq!(snap.repo_url.as_deref(), Some("https://github.com/o/r.git"));
    assert!(snap.patch.contains("a.txt") && !snap.patch.contains("TOKEN"), "{}", snap.patch);
    assert_eq!(snap.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["new.txt"]);
    for want in ["patch 에서 뺀 경로 .env", "뺀 파일 .env", "뺀 파일 ../outside", "뺀 파일 missing.txt"] {
        assert!(captured.omissions.iter().any(|o| o.starts_with(want)), "{want}: {:?}", captured.omissions);
    }
    assert_eq!(captured.newer_than_boundary, ["a.txt", "new.txt"]);
    assert!(capture_project(&repo, &files, None, &ProjectLimits::default()).unwrap().newer_than_boundary.is_empty());

    let dest = temp_dir("roundtrip-dest").join("fork");
    let applied = apply_project(snap, Some(&repo), &dest).unwrap();
    assert!(matches!(&applied.base, ProjectBase::Worktree { commit, .. } if Some(commit) == snap.commit.as_ref()));
    assert!(applied.patch_applied);
    assert_eq!(applied.files_written, ["new.txt"]);
    assert_eq!(std::fs::read_to_string(dest.join("a.txt")).unwrap(), "고친 내용\n");
    assert_eq!(std::fs::read_to_string(dest.join("new.txt")).unwrap(), "새 파일\n");
    assert_eq!(std::fs::read_to_string(dest.join(".env")).unwrap(), "TOKEN=old\n", "자격 파일 변경은 건너가지 않는다");
    assert_eq!(state(&repo), before, "원본 작업 트리는 그대로다");

    let dest_before = std::fs::read(dest.join("a.txt")).unwrap();
    assert!(matches!(apply_project(snap, Some(&repo), &dest), Err(ProjectError::DestinationExists(_))));
    assert_eq!(std::fs::read(dest.join("a.txt")).unwrap(), dest_before);
    assert!(matches!(apply_project(snap, Some(&repo), &repo.join("inside")), Err(ProjectError::Invalid(_))));
    assert!(!repo.join("inside").exists());
    assert_eq!(state(&repo), before);
}

#[test]
fn commit_or_patch_without_a_local_base_is_project_required() {
    let repo = dirty_repo("required");
    let snap = capture_project(&repo, &["new.txt".to_string()], None, &ProjectLimits::default()).unwrap().snapshot;
    let dest = temp_dir("required-dest").join("fork");
    assert!(matches!(apply_project(&snap, None, &dest), Err(ProjectError::ProjectRequired(_))));
    let other = temp_dir("required-other");
    git(&other, &["init", "-q"]);
    std::fs::write(other.join("x"), "x").unwrap();
    git(&other, &["add", "."]);
    git(&other, &["commit", "-qm", "other"]);
    assert!(matches!(apply_project(&snap, Some(&other), &dest), Err(ProjectError::ProjectRequired(_))));
    let patch_only = ProjectSnapshot { commit: None, ..snap.clone() };
    assert!(matches!(apply_project(&patch_only, Some(&repo), &dest), Err(ProjectError::ProjectRequired(_))));
    assert!(!dest.exists(), "실패하면 목적지를 만들지 않는다");
}

#[test]
fn files_only_bundle_goes_to_a_new_folder() {
    let bytes = b"just a note\n";
    let snap = ProjectSnapshot {
        repo_url: None,
        commit: None,
        captured_at_ms: 7,
        patch: String::new(),
        files: vec![SharedFile {
            path: "notes/n.txt".into(),
            sha256: hex(&Sha256::digest(bytes)),
            bytes_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
        }],
    };
    let dest = temp_dir("files-only").join("fork");
    let applied = apply_project(&snap, None, &dest).unwrap();
    assert_eq!((applied.base, applied.patch_applied, applied.captured_at_ms), (ProjectBase::EmptyDir, false, 7));
    assert_eq!(std::fs::read(dest.join("notes/n.txt")).unwrap(), bytes);
}

#[test]
fn tampered_bundles_are_refused_before_writing() {
    let bytes = b"x";
    let file = |path: &str| SharedFile {
        path: path.into(),
        sha256: hex(&Sha256::digest(bytes)),
        bytes_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
    };
    let base = ProjectSnapshot { repo_url: None, commit: None, captured_at_ms: 0, patch: String::new(), files: vec![] };
    let limits = ProjectLimits::default();
    let refused = |snap: ProjectSnapshot| matches!(validate_project(&snap, &limits), Err(ProjectError::Invalid(_)));
    assert!(refused(ProjectSnapshot { files: vec![file(".git/hooks/post-checkout")], ..base.clone() }));
    assert!(refused(ProjectSnapshot { files: vec![file("a"), file("A")], ..base.clone() }));
    assert!(refused(ProjectSnapshot { files: vec![SharedFile { sha256: "0".repeat(64), ..file("a") }], ..base.clone() }));
    assert!(refused(ProjectSnapshot { files: vec![SharedFile { bytes_base64: "!!".into(), ..file("a") }], ..base.clone() }));
    assert!(refused(ProjectSnapshot { commit: Some("HEAD".into()), ..base.clone() }));
    for patch in [
        "diff --git a/.git/hooks/x b/.git/hooks/x\n",
        "diff --git a/l b/l\nnew file mode 120000\n",
        "diff --git \"a/q\" \"b/q\"\n",
        "diff --git a/a b/b\nrename from a\nrename to b\n",
        "diff --git a/ok b/ok\n--- a/ok\n+++ b/../escape\n",
        "diff --git a/.env b/.env\n",
    ] {
        assert!(refused(ProjectSnapshot { patch: patch.into(), ..base.clone() }), "{patch}");
    }
    let big = ProjectLimits { max_total_bytes: 0, ..limits };
    assert!(matches!(validate_project(&ProjectSnapshot { files: vec![file("a")], ..base }, &big), Err(ProjectError::Invalid(_))));
}

#[cfg(unix)]
#[test]
fn checked_out_symlink_cannot_redirect_a_file() {
    let outside = temp_dir("symlink-outside");
    let repo = temp_dir("symlink").join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    std::os::unix::fs::symlink(&outside, repo.join("out")).unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "link"]);
    let commit = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    let bytes = b"evil";
    let snap = ProjectSnapshot {
        repo_url: None,
        commit: Some(commit),
        captured_at_ms: 0,
        patch: String::new(),
        files: vec![SharedFile {
            path: "out/evil.txt".into(),
            sha256: hex(&Sha256::digest(bytes)),
            bytes_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
        }],
    };
    let dest = temp_dir("symlink-dest").join("fork");
    assert!(matches!(apply_project(&snap, Some(&repo), &dest), Err(ProjectError::Invalid(_))));
    assert!(!outside.join("evil.txt").exists());
    assert!(!dest.exists(), "실패하면 만든 worktree 를 지운다");
    assert!(!git(&repo, &["worktree", "list"]).contains(dest.to_str().unwrap()));
}
