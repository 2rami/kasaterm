//! 이사(migrate)의 파일 동기화 — 「이 기계에만 있는 것」을 통째로 떠서 다른
//! 기계의 같은 레포에 재현한다. 미push 커밋과 미커밋 변경(삭제·untracked 포함)이
//! 함께 건너가므로, 이사 관문이 「커밋·push 하고 와라」로 막아 세우는 대신
//! 그 자리에서 이어 갈 수 있다(2026-08-29 지시: 「파일이 다르면 싱크가 되고
//! 이사해야 안끊기고 작업할수있지않나」).
//!
//! 방식: 임시 인덱스로 워킹트리를 스냅샷 커밋(sync)으로 뜨고, `git bundle` 로
//! 원격 ref 에 없는 오브젝트만 실어 나른다. 도착지는 bundle 을 fetch 한 뒤
//! `reset --hard <sync>` → `reset <head>`(mixed) 로 「같은 커밋 + 같은 미저장
//! 변경」을 만든다. staged/unstaged 구분은 소실된다 — 전부 unstaged 로 깬다.
//!
//! ⚠️ 도착지 워킹트리는 공유물이다(같은 레포에서 다른 학생이 일하고 있을 수
//! 있다). 그래서 apply 는 세 관문을 세운다: ①도착지에 미저장 변경이 있으면
//! 거부 ②브랜치를 갈아타야 하면 거부 ③도착지가 더 새롭거나 갈라져 있으면
//! (되감기가 되면) 거부. 셋 다 force 로만 열린다.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

/// 스냅샷 한 벌 — bundle 바이트와, 도착지가 재현에 쓰는 메타.
pub struct Snapshot {
    pub bundle: Vec<u8>,
    /// 출발지 HEAD. 도착지 브랜치가 여기로 온다.
    pub head: String,
    /// 워킹트리 스냅샷 커밋. 미커밋 변경이 없으면 head 와 같다.
    pub sync: String,
    /// 출발지 브랜치 이름. detached 면 빈 문자열.
    pub branch: String,
    /// origin URL — 도착지에 레포가 아예 없을 때 clone 재료.
    pub origin: String,
    /// 미커밋 변경이 실려 있나(= sync != head 가 될 조건).
    pub dirty: bool,
}

/// git 자식에 줄 PATH — GUI/launchd 프로세스는 시스템 기본(/usr/bin:…)뿐이라,
/// git 이 .gitattributes 의 filter 를 부르는 순간(`git-lfs`) "command not found"
/// 로 무너진다. 터미널에선 되고 이사 버튼에서만 「파일 스냅샷 실패」가 되는
/// 함정(2026-08-29 실측, LFS 레포의 pane 이사). 도구가 사는 표준 자리를 뒤에
/// 덧붙인다 — 이미 있으면 그대로 둔다.
pub fn tool_path() -> &'static str {
    static P: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    P.get_or_init(|| {
        let mut path = std::env::var("PATH").unwrap_or_default();
        if cfg!(unix) {
            let home = std::env::var("HOME").unwrap_or_default();
            for d in
                ["/opt/homebrew/bin".to_string(), "/usr/local/bin".to_string(), format!("{home}/.local/bin")]
            {
                if !path.split(':').any(|p| p == d) && Path::new(&d).is_dir() {
                    path.push(':');
                    path.push_str(&d);
                }
            }
        }
        path
    })
}

fn git_out(root: &Path, args: &[&str], envs: &[(&str, &str)]) -> Result<String> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(root).args(args);
    cmd.env("PATH", tool_path());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().with_context(|| format!("git {args:?} 실행 실패"))?;
    if !out.status.success() {
        bail!(
            "git {} 실패: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// 호출마다 고유한 임시 경로 — pid 만으로는 같은 프로세스의 동시 호출(병렬
/// 테스트, 서버의 겹친 요청)이 서로의 파일을 밟는다(실측: 단독 통과 · 동반 실패).
fn tmp_path(prefix: &str, ext: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!("{prefix}-{}-{n}{ext}", std::process::id()))
}

fn repo_root(cwd: &Path) -> Result<PathBuf> {
    let root = git_out(cwd, &["rev-parse", "--show-toplevel"], &[])
        .with_context(|| format!("{} 는 git 레포가 아니다", cwd.display()))?;
    Ok(PathBuf::from(root))
}

/// 워킹트리에 미저장 변경이 있나(untracked 포함, ignored 제외).
fn is_dirty(root: &Path) -> Result<bool> {
    Ok(!git_out(root, &["status", "--porcelain"], &[])?.is_empty())
}

/// 이 기계에만 있는 것을 뜬다. Ok(None) = 깨끗하고 미push 도 없다(실어 갈 것 없음).
///
/// 워킹트리는 건드리지 않는다 — 스냅샷 커밋은 임시 인덱스(GIT_INDEX_FILE)로
/// 만들고, 임시 ref(refs/kasaterm/sync)는 bundle 을 뜨는 동안만 산다.
pub fn snapshot(cwd: &Path) -> Result<Option<Snapshot>> {
    // git 레포가 아닌 폴더의 pane 도 이사는 간다 — 실어 갈 git 상태가 없을 뿐.
    let Ok(root) = repo_root(cwd) else {
        return Ok(None);
    };
    let head = git_out(&root, &["rev-parse", "HEAD"], &[])?;
    let branch = git_out(&root, &["rev-parse", "--abbrev-ref", "HEAD"], &[])
        .map(|b| if b == "HEAD" { String::new() } else { b })
        .unwrap_or_default();
    let origin = git_out(&root, &["remote", "get-url", "origin"], &[]).unwrap_or_default();
    let dirty = is_dirty(&root)?;
    // 미push 는 「어느 원격 ref 로도 못 닿는 커밋」으로 센다 — 업스트림이 아예
    // 없는 브랜치도 잡히고, bundle 의 `--not --remotes` 제외 기준과 일치한다.
    let unpushed = !git_out(&root, &["log", "--oneline", "HEAD", "--not", "--remotes"], &[])?
        .is_empty();
    if !dirty && !unpushed {
        return Ok(None);
    }
    let sync = if dirty {
        // 임시 인덱스에 HEAD 를 깔고 워킹트리 전체(add -A: 수정·삭제·untracked)를
        // 얹어 트리를 뜬다. 실인덱스를 안 쓰므로 사용자의 staged 상태가 안 다친다.
        let idx = tmp_path("kasaterm-syncidx", "");
        let _ = std::fs::remove_file(&idx);
        let idx_s = idx.to_string_lossy().into_owned();
        let env: &[(&str, &str)] = &[("GIT_INDEX_FILE", idx_s.as_str())];
        let made = (|| -> Result<String> {
            git_out(&root, &["read-tree", "HEAD"], env)?;
            git_out(&root, &["add", "-A"], env)?;
            let tree = git_out(&root, &["write-tree"], env)?;
            // commit-tree 는 ident 없이는 실패한다 — 이 커밋은 운반용이라
            // 사람 서명이 필요 없고, 설정이 빈 기계(갓 세팅한 미니)에서도 돌아야 한다.
            git_out(
                &root,
                &["commit-tree", &tree, "-p", &head, "-m", "kasaterm repo-sync snapshot"],
                &[
                    ("GIT_INDEX_FILE", idx_s.as_str()),
                    ("GIT_AUTHOR_NAME", "kasaterm"),
                    ("GIT_AUTHOR_EMAIL", "kasaterm@local"),
                    ("GIT_COMMITTER_NAME", "kasaterm"),
                    ("GIT_COMMITTER_EMAIL", "kasaterm@local"),
                ],
            )
        })();
        let _ = std::fs::remove_file(&idx);
        made?
    } else {
        head.clone()
    };
    // bundle 은 ref 이름을 요구한다 — 임시 ref 를 세웠다 바로 걷는다.
    git_out(&root, &["update-ref", "refs/kasaterm/sync", &sync], &[])?;
    let bfile = tmp_path("kasaterm-sync", ".bundle");
    let _ = std::fs::remove_file(&bfile);
    let made = git_out(
        &root,
        &[
            "bundle",
            "create",
            &bfile.to_string_lossy(),
            "refs/kasaterm/sync",
            "--not",
            "--remotes",
        ],
        &[],
    );
    let _ = git_out(&root, &["update-ref", "-d", "refs/kasaterm/sync"], &[]);
    made?;
    let bundle = std::fs::read(&bfile).context("bundle 파일 읽기")?;
    let _ = std::fs::remove_file(&bfile);
    Ok(Some(Snapshot { bundle, head, sync, branch, origin, dirty }))
}

/// 관문에 막혔을 때의 처신 — 세울 것인가(Bail), 짐만 보관하고 갈 것인가(Deposit).
/// Deposit 은 관문이 **워킹트리를 건드리기 전**에만 갈라지므로 안전하다 —
/// checkout/reset 도중의 실패는 이 폴백을 타지 않고 그대로 오류다.
#[derive(Clone, Copy, PartialEq)]
pub enum OnBlock {
    Bail,
    Deposit,
}

fn blocked(on_block: OnBlock, root: &Path, bundle: &[u8], why: String) -> Result<String> {
    match on_block {
        OnBlock::Bail => bail!(why),
        OnBlock::Deposit => {
            let kept = deposit(root, bundle).with_context(|| why.clone())?;
            Ok(format!("{kept} — {why}"))
        }
    }
}

/// 스냅샷을 이 기계의 레포에 재현한다. 성공 시 사람이 읽을 한 줄을 돌려준다.
///
/// 도착지 레포는 이미 있어야 하고(없으면 호출부가 clone 한다), origin 오브젝트가
/// 최신이어야 bundle 의 전제(prerequisite)가 풀린다 — 이사 경로는 ensure_repo /
/// clone 직후에 부르므로 성립한다.
pub fn apply(
    cwd: &Path,
    bundle: &[u8],
    head: &str,
    sync: &str,
    branch: &str,
    dirty: bool,
    force: bool,
    on_block: OnBlock,
) -> Result<String> {
    let root = repo_root(cwd)?;
    // 관문 ① — 이 워킹트리는 공유물일 수 있다. 남의 미저장 작업 위에
    // reset --hard 를 얹으면 그 작업은 reflog 에도 안 남고 사라진다.
    if !force && is_dirty(&root)? {
        return blocked(
            on_block,
            &root,
            bundle,
            format!(
                "도착지({})에 미저장 변경이 있다 — 그쪽을 정리하고 오거나 --force",
                root.display()
            ),
        );
    }
    let cur_branch = git_out(&root, &["rev-parse", "--abbrev-ref", "HEAD"], &[])
        .map(|b| if b == "HEAD" { String::new() } else { b })
        .unwrap_or_default();
    // 관문 ② — 브랜치 갈아타기는 같은 트리를 쓰는 남의 pane 을 통째로 딸려
    // 보내는 조작이다(전역 규칙 「브랜치 전환은 물어볼 것」의 기계판).
    if !force && !branch.is_empty() && !cur_branch.is_empty() && cur_branch != branch {
        return blocked(
            on_block,
            &root,
            bundle,
            format!(
                "도착지가 다른 브랜치({cur_branch})에 서 있다 — 실려 온 것은 {branch}. 사람이 정리하거나 --force"
            ),
        );
    }
    let bfile = tmp_path("kasaterm-apply", ".bundle");
    std::fs::write(&bfile, bundle).context("bundle 임시 파일 쓰기")?;
    let bpath = bfile.to_string_lossy().into_owned();
    let mut fetched = git_out(&root, &["fetch", &bpath, "refs/kasaterm/sync"], &[]);
    if fetched.is_err() {
        // bundle 의 전제는 「출발지 원격 ref 가 닿는 오브젝트」다 — 이쪽이 한동안
        // fetch 를 안 했으면 모자랄 수 있다. origin 을 한 번 당기고 재시도한다.
        let _ = git_out(&root, &["fetch", "--prune", "origin"], &[]);
        fetched = git_out(&root, &["fetch", &bpath, "refs/kasaterm/sync"], &[]);
    }
    let _ = std::fs::remove_file(&bfile);
    fetched.context("bundle fetch — 도착지에 출발지 origin 오브젝트가 없거나 낡았을 수 있다")?;
    // 관문 ③ — 도착지가 실려 온 head 보다 앞서 있으면(또는 갈라져 있으면)
    // 아래 checkout -B 가 브랜치를 되감는다. 커밋이 지워지진 않지만(reflog)
    // 브랜치에서는 사라진다 — 미push 작업이 이 기계에 먼저 있었단 뜻이다.
    let cur_head = git_out(&root, &["rev-parse", "HEAD"], &[])?;
    let ff_ok = std::process::Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["merge-base", "--is-ancestor", &cur_head, head])
        .env("PATH", tool_path())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !force && !ff_ok && cur_head != head {
        return blocked(
            on_block,
            &root,
            bundle,
            format!(
                "도착지가 더 새롭거나 갈라져 있다(HEAD {}) — 실려 온 {} 로 옮기면 되감긴다. 사람이 정리하거나 --force",
                &cur_head[..cur_head.len().min(8)],
                &head[..head.len().min(8)]
            ),
        );
    }
    if branch.is_empty() {
        git_out(&root, &["checkout", "--detach", head], &[])?;
    } else {
        git_out(&root, &["checkout", "-B", branch, head], &[])?;
    }
    if dirty && sync != head {
        // 트리를 스냅샷으로 맞춘 뒤 HEAD 만 원래 커밋으로 되돌린다(mixed) —
        // 스냅샷과의 차이가 그대로 「미저장 변경」으로 남는다. untracked 였던
        // 파일은 인덱스(head)에 없으므로 다시 untracked 로 깬다.
        git_out(&root, &["reset", "--hard", sync], &[])?;
        git_out(&root, &["reset", head], &[])?;
    } else {
        git_out(&root, &["reset", "--hard", head], &[])?;
    }
    let short = &head[..head.len().min(8)];
    Ok(if dirty {
        format!("{short} + 미저장 변경 재현")
    } else {
        format!("{short} 로 동기화")
    })
}

/// 짐을 **풀지 않고 보관만** 한다 — bundle 을 `refs/kasaterm/incoming` 으로
/// fetch 해 둔다. 워킹트리·인덱스·브랜치 무접촉이라 apply 의 3관문(도착지
/// dirty·브랜치 다름·되감김)이 지키려는 것을 하나도 건드리지 않는다.
///
/// 용도: 관문에 막혀 apply 를 못 할 때 이사를 세우는 대신 여기 두고 간다 —
/// 실려 온 커밋·미저장 스냅샷이 오브젝트째 남으므로 잃는 것이 없고, 그 레포의
/// 학생이 나중에 `git merge refs/kasaterm/incoming`(또는 cherry-pick)으로
/// 정리하면 된다(2026-08-30: 미쿠 역이사가 「저쪽 push → 이쪽 정리」 수순을
/// 사람 손으로 밟아야 했던 자리).
pub fn deposit(cwd: &Path, bundle: &[u8]) -> Result<String> {
    let root = repo_root(cwd)?;
    let bfile = tmp_path("kasaterm-deposit", ".bundle");
    std::fs::write(&bfile, bundle).context("bundle 임시 파일 쓰기")?;
    let bpath = bfile.to_string_lossy().into_owned();
    let refspec = "+refs/kasaterm/sync:refs/kasaterm/incoming";
    let mut fetched = git_out(&root, &["fetch", &bpath, refspec], &[]);
    if fetched.is_err() {
        // apply 와 같은 전제 — bundle 은 출발지 원격 ref 오브젝트를 깔고 있다.
        let _ = git_out(&root, &["fetch", "--prune", "origin"], &[]);
        fetched = git_out(&root, &["fetch", &bpath, refspec], &[]);
    }
    let _ = std::fs::remove_file(&bfile);
    fetched.context("bundle 보관 fetch")?;
    Ok("워킹트리가 바빠 짐을 refs/kasaterm/incoming 에 보관".to_string())
}

/// 그 경로 레포의 origin URL — apply 전에 clone 이 필요할 때 재료로 쓴다.
pub fn origin_of(cwd: &Path) -> Option<String> {
    let root = repo_root(cwd).ok()?;
    git_out(&root, &["remote", "get-url", "origin"], &[]).ok().filter(|s| !s.is_empty())
}

/// 왕복 검증은 `sh -c` 로 git 을 몰기 때문에 unix 에서만 돈다 — 셸 문법(`&&`,
/// `>` 리다이렉트, `rm`)에 기대 레포 상태를 짓는데 Windows 엔 `sh` 가 없어
/// 「program not found」로 죽었다(2026-08-31 실측). 이걸 Windows 로 옮길 이유도
/// 없다: 이 코드를 쓰는 이사(`migrate_pane`)가 애초에 `#[cfg(unix)]` 라, 재 봐야
/// 그 플랫폼에서 아무도 안 밟는 길을 재는 셈이다. 맥 CI 에서는 그대로 관문이다.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn sh(dir: &Path, cmd: &str) -> String {
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "`{cmd}` 실패: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// origin(bare) 하나에서 src·dst 를 clone 해, src 에 미push 커밋+미커밋
    /// 변경(수정·신규·삭제)을 만들고 snapshot→apply 로 dst 에 같은 상태가
    /// 재현되는지 끝까지 돈다 — 이 crate 안에서 왕복 전체를 검증하는 유일한 자리.
    #[test]
    fn roundtrip_sync() {
        let base = std::env::temp_dir().join(format!("kasaterm-reposync-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let ident = "-c user.name=t -c user.email=t@t";
        sh(&base, "git init --bare origin.git");
        sh(&base, &format!(
            "git clone origin.git src 2>/dev/null && cd src && echo one > a.txt && echo keep > b.txt && git add -A && git {ident} commit -qm init && git push -q origin HEAD"
        ));
        sh(&base, "git clone origin.git dst 2>/dev/null");
        // 이 기계에만 있는 것: 커밋 하나 + (수정 a / 신규 c / 삭제 b)
        sh(&base, &format!(
            "cd src && echo two >> a.txt && git add -A && git {ident} commit -qm local-only"
        ));
        sh(&base, "cd src && echo three >> a.txt && echo new > c.txt && rm b.txt");
        let src = base.join("src");
        let dst = base.join("dst");
        let snap = snapshot(&src).unwrap().expect("실어 갈 것이 있어야 한다");
        assert!(snap.dirty);
        assert_ne!(snap.head, snap.sync);
        let msg = apply(&dst, &snap.bundle, &snap.head, &snap.sync, &snap.branch, snap.dirty, false, OnBlock::Bail)
            .unwrap();
        assert!(msg.contains("미저장"));
        // 같은 커밋 + 같은 미저장 변경인지 — 내용으로 확인한다.
        assert_eq!(sh(&dst, "git rev-parse HEAD"), snap.head);
        assert_eq!(sh(&dst, "cat a.txt"), "one\ntwo\nthree");
        assert_eq!(sh(&dst, "cat c.txt"), "new");
        assert!(!dst.join("b.txt").exists());
        let status = sh(&dst, "git status --porcelain");
        assert!(status.contains("a.txt") && status.contains("c.txt") && status.contains("b.txt"));
        // 스냅샷은 출발지 워킹트리·인덱스를 건드리지 않았어야 한다.
        assert_eq!(sh(&src, "git rev-parse HEAD"), snap.head);
        assert!(sh(&src, "git status --porcelain").contains("c.txt"));
        // 관문 ①: 도착지가 dirty 면 거부.
        sh(&base, "cd dst && echo x > dirty.txt");
        let again = snapshot(&src).unwrap().unwrap();
        assert!(apply(&dst, &again.bundle, &again.head, &again.sync, &again.branch, again.dirty, false, OnBlock::Bail)
            .is_err());
        // Deposit 갈래 — 관문 자리에서 보관으로 갈라져 성공을 돌려준다.
        let kept = apply(&dst, &again.bundle, &again.head, &again.sync, &again.branch, again.dirty, false, OnBlock::Deposit)
            .unwrap();
        assert!(kept.contains("incoming") && kept.contains("미저장 변경이 있다"));
        // 관문에 막혔을 때의 보관 경로 — 워킹트리 무접촉으로 짐만 남긴다.
        let kept = deposit(&dst, &again.bundle).unwrap();
        assert!(kept.contains("incoming"));
        assert_eq!(sh(&dst, "git rev-parse refs/kasaterm/incoming"), again.sync);
        assert_eq!(sh(&dst, "cat dirty.txt"), "x");
        assert_eq!(sh(&dst, "git rev-parse HEAD"), again.head);
        let _ = std::fs::remove_dir_all(&base);
    }
}
