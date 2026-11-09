use super::*;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize)]
struct WorkspaceRecord {
    request: serde_json::Value,
    workdir: PathBuf,
    project_ready: bool,
}

fn private_file(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
    options.open(path)
}

fn save(path: &Path, record: &WorkspaceRecord) -> Result<(), String> {
    let temporary = path.with_extension(format!("{}.part", service::new_operation_id()));
    let mut file = private_file(&temporary).map_err(|e| e.to_string())?;
    let result = (|| {
        file.write_all(&serde_json::to_vec(record).map_err(std::io::Error::other)?)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() { let _ = std::fs::remove_file(&temporary); }
    result.map_err(|e| e.to_string())
}

pub(super) fn workspace(root: &Path, scope: &str, fork: &api::SharedFork, cwd: Option<&Path>, include_project: bool)
    -> Result<(PathBuf, PathBuf), String> {
    let dir = root.join("requests").join(scope);
    #[cfg(unix)]
    { use std::os::unix::fs::DirBuilderExt; std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir).map_err(|e| e.to_string())?; }
    #[cfg(not(unix))]
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{}.json", fork.info.id));
    let cwd = cwd.map(std::fs::canonicalize).transpose().map_err(|e| e.to_string())?;
    use sha2::Digest;
    let project_hash = if include_project {
        Some(format!("{:x}", sha2::Sha256::digest(serde_json::to_vec(&fork.checkpoint.project).map_err(|e| e.to_string())?)))
    } else { None };
    let request = serde_json::json!({"cwd": cwd, "include_project": include_project, "snapshot": fork.checkpoint.native.sha256,
        "checkpoint": fork.info.checkpoint_id, "project": project_hash});
    let mut record = match std::fs::read(&path) {
        Ok(bytes) => {
            let record: WorkspaceRecord = serde_json::from_slice(&bytes).map_err(|_| "이전 포크 작업 폴더 기록을 읽지 못했어요")?;
            if record.request != request { return Err("같은 포크의 목적지나 옵션이 달라졌어요. 새 포크로 시작해 주세요".into()); }
            record
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if include_project && (cwd.is_none() || fork.checkpoint.project.is_none()) {
                return Err("프로젝트 자료와 이 기기의 저장소 폴더가 필요해요".into());
            }
            let workdir = if include_project {
                let repo = cwd.as_deref().unwrap();
                let parent = repo.parent().ok_or("프로젝트 폴더 옆에 새 폴더를 만들 수 없어요")?;
                let name = repo.file_name().and_then(|s| s.to_str()).unwrap_or("project");
                let dest = parent.join(format!("{name}-fork-{}", fork.info.id));
                if std::fs::symlink_metadata(&dest).is_ok() { return Err("포크 목적지 폴더가 이미 있어요. 기존 폴더를 덮지 않았어요".into()); }
                dest
            } else { cwd.clone().unwrap_or_else(|| root.join("workspaces").join(&fork.info.id)) };
            let record = WorkspaceRecord { request, workdir, project_ready: !include_project };
            save(&path, &record)?;
            record
        }
        Err(error) => return Err(error.to_string()),
    };
    if include_project && !record.project_ready {
        let project = fork.checkpoint.project.as_ref().ok_or("포크에 프로젝트 자료가 없어요")?;
        if std::fs::symlink_metadata(&record.workdir).is_ok() {
            return Err("이전 프로젝트 설치 결과를 확정하지 못했어요. 기존 포크 폴더를 확인해 주세요".into());
        }
        kasa_remote::shared_fork_project::apply_project(project, cwd.as_deref(), &record.workdir).map_err(|e| e.to_string())?;
        record.project_ready = true;
        save(&path, &record)?;
    } else if !include_project && cwd.is_none() {
        std::fs::create_dir_all(&record.workdir).map_err(|e| e.to_string())?;
    }
    if !record.workdir.is_dir() { return Err("이전 포크 작업 폴더가 없어졌어요".into()); }
    Ok((record.workdir, path.with_extension("launch")))
}

pub(super) fn attempted(path: &Path, child: &str) -> Result<bool, String> {
    match std::fs::read_to_string(path) {
        Ok(value) if value == child => Ok(true),
        Ok(_) => Err("이전 실행의 대화 번호가 달라졌어요".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.to_string()),
    }
}

pub(super) fn claim(path: &Path, child: &str, resume: bool) -> Result<(), String> {
    match private_file(path) {
        Ok(mut file) => file.write_all(child.as_bytes()).and_then(|()| file.sync_all()).map_err(|e| e.to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && resume && attempted(path, child)? => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Err("앞선 실행 결과가 아직 확정되지 않았어요. 기존 탭을 확인해 주세요".into()),
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_uncertain_launch_is_not_sent_twice() {
        let root = std::env::temp_dir().join(format!("shared-claim-{}", service::new_operation_id()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("launch");
        assert!(!attempted(&path, "child").unwrap());
        claim(&path, "child", false).unwrap();
        assert!(attempted(&path, "child").unwrap());
        assert!(claim(&path, "child", false).is_err());
        assert!(claim(&path, "other-child", true).is_err());
        claim(&path, "child", true).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn retries_reuse_owned_workspaces_and_preserve_edits() {
        let root = std::env::temp_dir().join(format!("shared-workspace-{}", service::new_operation_id()));
        let repo = root.join("repository");
        std::fs::create_dir_all(&repo).unwrap();
        let mut fork: api::SharedFork = serde_json::from_value(serde_json::json!({
            "id":"0123456789abcdef0123456789abcdef", "room_id":"r", "session_id":"s", "checkpoint_id":"cp", "created_at_ms":1,
            "checkpoint":{"format_version":1,"captured_at_ms":1,"display_raw":"","project":null,
                "native":{"harness":"claude","parent_session_id":"parent","boundary_id":"boundary","boundary_label":"done",
                    "native_version":"fixture","jsonl":"","sha256":"snapshot"}}
        })).unwrap();
        let (conversation, _) = workspace(&root, "account", &fork, None, false).unwrap();
        std::fs::write(conversation.join("keep.txt"), "recipient edit").unwrap();
        assert_eq!(workspace(&root, "account", &fork, None, false).unwrap().0, conversation);
        assert!(workspace(&root, "account", &fork, Some(&repo), false).is_err());
        assert_eq!(std::fs::read_to_string(conversation.join("keep.txt")).unwrap(), "recipient edit");
        fork.info.id = "1123456789abcdef0123456789abcdef".into();
        fork.checkpoint.project = Some(api::ProjectSnapshot { repo_url: None, commit: None, captured_at_ms: 1, patch: String::new(), files: Vec::new() });
        let (project, _) = workspace(&root, "account", &fork, Some(&repo), true).unwrap();
        std::fs::write(project.join("edit.txt"), "local change").unwrap();
        assert_eq!(workspace(&root, "account", &fork, Some(&repo), true).unwrap().0, project);
        assert_eq!(std::fs::read_to_string(project.join("edit.txt")).unwrap(), "local change");
        assert!(workspace(&root, "account", &fork, Some(&repo), false).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
