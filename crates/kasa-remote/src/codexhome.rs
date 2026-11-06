//! Codex 대화(rollout) 설치 — 이사 창구의 「도착지 정책」 절반.
//!
//! 형식·검증은 `kasa_socket::sessions::codex_sessions`(운반 형식 helper)가 전담하고,
//! 그 helper 는 덮어쓰기·충돌·원자쓰기 정책을 일부러 창구 쪽에 남겨 뒀다. 이 모듈이
//! 그 절반이다. 받은 바이트는 **임시 Codex home 에 그대로 앉혀 helper 로 재검증**한다
//! — rollout 헤더 파싱을 여기서 중복하면 화면마다 다른 파서가 생기는 그 사고를
//! 되풀이하게 된다(transcript 파서 일원화와 같은 이유).

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

use kasa_socket::sessions::codex_sessions as fmt;

/// rollout 경로에서 Codex home 을 거꾸로 찾는다(`…/<home>/sessions/…` 의 home).
/// 계정 슬롯처럼 home 이 `~/.codex` 가 아닐 수 있어, 고정 경로 대신 조상에서
/// `sessions` 성분을 찾는 쪽이 정본이다.
pub fn codex_home_of_rollout(rollout: &Path) -> Option<PathBuf> {
    let mut cur = rollout.parent()?;
    loop {
        if cur.file_name().and_then(|n| n.to_str()) == Some("sessions") {
            return cur.parent().map(|p| p.to_path_buf());
        }
        cur = cur.parent()?;
    }
}

/// 받은 rollout 바이트를 이 기계 Codex home 의 같은 자리(`sessions/…`)에 앉힌다.
///
/// - 검증: 임시 home 에 놓고 `bundle_codex_session` 재검증 + 요청 id 교차 확인.
/// - 충돌: 같은 바이트면 그대로 두고, **다른 내용이 있으면 덮지 않는다** — 기존
///   것을 `<이름>.kasaterm-incoming-<ts>` 로 옆에 보관한 뒤 앉힌다. 확장자가
///   `.jsonl` 이 아니게 되어 resume 탐색의 「같은 id 두 개」 오류에 안 걸린다.
/// - 쓰기: `.part` 에 쓰고 rename — 쓰다 만 파일이 정본 자리에 안 남는다.
pub fn install_codex_rollout(
    codex_home: &Path,
    sid: &str,
    rel: &Path,
    bytes: &[u8],
) -> Result<String> {
    // join·write **이전**에 거른다 — `Path::join` 은 절대경로를 받으면 기준을
    // 통째로 갈아치우고 `..` 는 임시 홈 밖으로 걸어 나간다. helper 검증에만
    // 맡기면 그 검증이 돌기 전에 이미 바이트가 밖에 써진다(독립 리뷰 지적
    // 2026-09-02 — 실측: rel=../../etc/evil 이 임시 홈 밖에 파일을 남겼다).
    validate_rel(rel)?;
    let tmp_home = std::env::temp_dir().join(format!(
        "kasaterm-codexmove-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let res = install_via_tmp(&tmp_home, codex_home, sid, rel, bytes);
    let _ = std::fs::remove_dir_all(&tmp_home);
    res
}

/// `sessions/YYYY/MM/DD/rollout-<ts>-<uuid>.jsonl` 꼴만 통과 — 성분은 전부
/// Normal(., .., 루트, 드라이브 금지), 날짜는 자릿수까지 고정, 파일명은 운반
/// helper 의 uuid 추출 규칙으로 검사한다.
fn validate_rel(rel: &Path) -> Result<()> {
    if rel.is_absolute() {
        bail!("rollout 상대경로가 절대경로다: {}", rel.display());
    }
    let mut parts: Vec<String> = Vec::new();
    for c in rel.components() {
        match c {
            std::path::Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
            _ => bail!(
                "rollout 상대경로에 허용 밖 성분(`..` 등)이 있다: {}",
                rel.display()
            ),
        }
    }
    if parts.len() != 5 || parts[0] != "sessions" {
        bail!(
            "rollout 상대경로가 sessions/YYYY/MM/DD/파일 꼴이 아니다: {}",
            rel.display()
        );
    }
    for (seg, want) in parts[1..4].iter().zip([4usize, 2, 2]) {
        if seg.len() != want || !seg.chars().all(|ch| ch.is_ascii_digit()) {
            bail!(
                "rollout 날짜 폴더가 YYYY/MM/DD 꼴이 아니다: {}",
                rel.display()
            );
        }
    }
    if fmt::codex_id_from_rollout_path(rel).is_none() {
        bail!(
            "rollout 파일명이 rollout-<ts>-<uuid>.jsonl 꼴이 아니다: {}",
            rel.display()
        );
    }
    Ok(())
}

fn install_via_tmp(
    tmp_home: &Path,
    codex_home: &Path,
    sid: &str,
    rel: &Path,
    bytes: &[u8],
) -> Result<String> {
    let tmp_path = tmp_home.join(rel);
    let parent = tmp_path
        .parent()
        .context("rollout 상대경로에 부모 폴더가 없다")?;
    std::fs::create_dir_all(parent).context("임시 검증 폴더 생성 실패")?;
    std::fs::write(&tmp_path, bytes).context("임시 검증 파일 쓰기 실패")?;
    let bundle = fmt::bundle_codex_session(tmp_home, &tmp_path)?;
    if bundle.session_id != sid {
        bail!(
            "rollout 의 세션 id({}) 가 요청 id({sid}) 와 다르다",
            bundle.session_id
        );
    }
    let files = fmt::codex_restore_files(codex_home, &bundle)?;
    let mut notes = Vec::new();
    for f in files {
        match std::fs::read(&f.path) {
            Ok(existing) if existing == f.bytes => {
                notes.push("이미 같은 대화가 있어 그대로 둔다".to_string());
                continue;
            }
            Ok(_) => {
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let name = f.path.file_name().and_then(|n| n.to_str()).unwrap_or("rollout");
                let stash = f.path.with_file_name(format!("{name}.kasaterm-incoming-{ts}"));
                std::fs::rename(&f.path, &stash)
                    .with_context(|| format!("기존 rollout 보관 실패: {}", stash.display()))?;
                notes.push(format!(
                    "다른 내용이 있어 {} 로 보관",
                    stash.file_name().and_then(|n| n.to_str()).unwrap_or("?")
                ));
            }
            Err(_) => {}
        }
        if let Some(dir) = f.path.parent() {
            std::fs::create_dir_all(dir).context("rollout 폴더 생성 실패")?;
        }
        let part = f.path.with_extension("part");
        std::fs::write(&part, f.bytes)
            .and_then(|_| std::fs::rename(&part, &f.path))
            .with_context(|| format!("rollout 저장 실패: {}", f.path.display()))?;
        notes.push(format!("{}B 앉힘", f.bytes.len()));
    }
    Ok(notes.join(" · "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_of_rollout_walks_up_to_sessions_parent() {
        let p = Path::new("/Users/x/.codex/sessions/2026/09/02/rollout-a.jsonl");
        assert_eq!(
            codex_home_of_rollout(p),
            Some(PathBuf::from("/Users/x/.codex"))
        );
        assert_eq!(codex_home_of_rollout(Path::new("/tmp/rollout.jsonl")), None);
    }

    const GOOD: &str =
        "sessions/2026/09/02/rollout-2026-09-02T00-08-01-01a05d83-4ec4-7e73-b66b-8bf5d5309d9f.jsonl";

    #[test]
    fn rel_validation_blocks_escapes_before_any_io() {
        assert!(validate_rel(Path::new(GOOD)).is_ok());
        // 탈출·형식 위반은 전부 join 이전에 선다.
        for bad in [
            "/etc/evil.jsonl",                                   // 절대경로(join 이 기준을 갈아치움)
            "../../etc/evil.jsonl",                              // 상향 탈출
            "sessions/../2026/09/02/rollout-x.jsonl",            // 중간 `..`
            "notsessions/2026/09/02/rollout-2026-09-02T00-08-01-01a05d83-4ec4-7e73-b66b-8bf5d5309d9f.jsonl",
            "sessions/26/09/02/rollout-2026-09-02T00-08-01-01a05d83-4ec4-7e73-b66b-8bf5d5309d9f.jsonl",
            "sessions/2026/09/02/evil.jsonl",                    // rollout- 아님
            "sessions/2026/09/02/rollout-nouuid.jsonl",          // uuid 없음
            "sessions/2026/09/02/x/rollout-2026-09-02T00-08-01-01a05d83-4ec4-7e73-b66b-8bf5d5309d9f.jsonl", // 깊이 초과
        ] {
            assert!(validate_rel(Path::new(bad)).is_err(), "통과해선 안 된다: {bad}");
        }
    }

    #[test]
    fn install_rejects_bad_rel_without_writing() {
        let scratch = std::env::temp_dir().join(format!(
            "kasaterm-codexhome-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).unwrap();
        let home = scratch.join("home");
        let evil_rel = "../escaped-evil.jsonl";
        let err = install_codex_rollout(
            &home,
            "01a05d83-4ec4-7e73-b66b-8bf5d5309d9f",
            Path::new(evil_rel),
            b"{}",
        );
        assert!(err.is_err());
        // 임시 홈은 temp_dir 바로 아래 생긴다 — 탈출 대상이 됐을 자리에
        // 아무것도 안 써졌는지 본다(수정 전에는 여기 파일이 실제로 남았다).
        assert!(
            !std::env::temp_dir().join("escaped-evil.jsonl").exists(),
            "검증 전에 바이트가 밖에 써졌다"
        );
        let _ = std::fs::remove_dir_all(&scratch);
    }
}
