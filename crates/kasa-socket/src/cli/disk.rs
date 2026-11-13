//! `kasaterm-cli disk [status|prune]` — 디스크 지키기를 앱 없이 돈다(docs/disk-guard.md).
//! `scripts/build-app.sh` 가 구운 뒤 `disk prune --after-build --repo <레포>` 로 부른다.

use anyhow::{anyhow, Result};

use crate::disk_guard;

pub(super) fn run(args: &[String]) -> Result<String> {
    let flag = |name: &str| args.iter().any(|a| a == name);
    let value = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    match args.first().map(String::as_str).filter(|a| !a.starts_with("--")) {
        None | Some("status") => status(flag("--json"), flag("--places")),
        Some("prune") => prune(flag("--dry-run"), flag("--json"), flag("--after-build"), value("--repo")),
        Some(other) => Err(anyhow!("disk {other} 는 없어요 — status [--places] [--json] · prune [--dry-run] [--json] [--repo <폴더>]")),
    }
}

fn status(json: bool, places: bool) -> Result<String> {
    let mut status = disk_guard::snapshot().ok_or_else(|| anyhow!("남은 공간을 못 쟀어요"))?;
    if places {
        status.big = disk_guard::measure_places();
    }
    if json {
        return Ok(serde_json::to_string(&status)?);
    }
    let level = match status.level {
        disk_guard::Level::Ok => "정상",
        disk_guard::Level::Low => "문턱 아래",
        disk_guard::Level::Critical => "위험",
    };
    let mut out = format!(
        "남은 공간 {} / {} · 문턱 {} · {level}",
        disk_guard::size_label(status.free),
        disk_guard::size_label(status.total),
        disk_guard::size_label(status.warn_free),
    );
    if !status.big.is_empty() {
        out.push_str(&format!("\n큰 곳: {}", status.big_line(usize::MAX)));
    }
    if let Some(log) = disk_guard::log_path() {
        out.push_str(&format!("\n기록: {}", log.display()));
    }
    Ok(out)
}

#[cfg(unix)]
fn prune(dry_run: bool, json: bool, after_build: bool, repo: Option<String>) -> Result<String> {
    let cfg = disk_guard::Config::load();
    if after_build && !cfg.auto_prune {
        return Ok("자동 정리가 꺼져 있어 건너뜀(설정 disk_auto_prune)".into());
    }
    let repos: Vec<std::path::PathBuf> = repo.map(std::path::PathBuf::from).into_iter().collect();
    if !dry_run {
        for repo in &repos {
            disk_guard::remember_repo(repo);
        }
    }
    let reason = if after_build { disk_guard::Reason::Build } else { disk_guard::Reason::Manual };
    let report = disk_guard::run_and_log(&cfg, reason, &repos, dry_run);
    if json {
        return Ok(serde_json::to_string(&report)?);
    }
    let mut out = format!(
        "빌드 산출물 {}개 {} / 상한 {} · 남은 공간 {}",
        report.outputs,
        disk_guard::size_label(report.outputs_bytes),
        disk_guard::size_label(report.cap),
        disk_guard::size_label(report.free_after),
    );
    let verb = if dry_run { "지울 것" } else { "지움" };
    for r in &report.removed {
        out.push_str(&format!("\n{verb}: {} {}", r.path, disk_guard::size_label(r.bytes)));
    }
    for s in &report.skipped {
        out.push_str(&format!("\n건너뜀: {} {} — {}", s.path, disk_guard::size_label(s.bytes), s.why));
    }
    if report.removed.is_empty() && report.skipped.is_empty() && report.error.is_none() {
        out.push_str("\n지울 것 없음");
    }
    if let Some(error) = &report.error {
        out.push_str(&format!("\n멈춤: {error}"));
    }
    Ok(out)
}

#[cfg(not(unix))]
fn prune(_dry_run: bool, _json: bool, _after_build: bool, _repo: Option<String>) -> Result<String> {
    Err(anyhow!("빌드 산출물 정리는 macOS·리눅스에서만 돌아요 — docs/disk-guard.md 「Windows」"))
}
