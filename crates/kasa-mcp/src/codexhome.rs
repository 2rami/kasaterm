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
    let tmp_home = std::env::temp_dir().join(format!(
        "kasaterm-codexmove-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let res = install_via_tmp(&tmp_home, codex_home, sid, rel, bytes);
    let _ = std::fs::remove_dir_all(&tmp_home);
    res
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
}
