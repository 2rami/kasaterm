use super::{Engine, FOLDER, META};
use serde::{Deserialize, Serialize};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

static CHANGE: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", content = "parent", rename_all = "snake_case")]
pub enum Location {
    #[default]
    Desktop,
    Folder(PathBuf),
    Hidden,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Preferences {
    pub location: Location,
    pub link: Option<PathBuf>,
}

pub fn load(root: &Path) -> io::Result<Preferences> {
    let path = root.join(META).join("shortcut.json");
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Preferences::default()),
        Err(error) => Err(error),
        Ok(metadata)
            if metadata.is_file()
                && !metadata.file_type().is_symlink()
                && metadata.len() <= 16384 =>
        {
            serde_json::from_slice(&std::fs::read(path)?).map_err(io::Error::other)
        }
        _ => Err(io::Error::other(
            "바로가기 설정 파일을 안전하게 읽을 수 없어요",
        )),
    }
}

fn ours(link: &Path, root: &Path) -> bool {
    let Ok(target) = std::fs::read_link(link) else {
        return false;
    };
    let target = if target.is_absolute() {
        target
    } else {
        link.parent().unwrap_or(Path::new(".")).join(target)
    };
    match (target.canonicalize(), root.canonicalize()) {
        (Ok(target), Ok(root)) => target == root,
        _ => false,
    }
}

fn target(root: &Path, location: &Location, desktop: Option<&Path>) -> io::Result<Option<PathBuf>> {
    let parent = match location {
        Location::Hidden => return Ok(None),
        Location::Desktop => {
            desktop.ok_or_else(|| io::Error::other("바탕화면 위치를 찾지 못했어요"))?
        }
        Location::Folder(parent) => parent,
    };
    if !parent.is_absolute() || parent.as_os_str().to_string_lossy().contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "절대 경로인 폴더를 선택해 주세요",
        ));
    }
    let real_parent = parent.canonicalize()?;
    let real_root = root.canonicalize()?;
    if !real_parent.is_dir() || real_parent.starts_with(&real_root) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "공유 폴더 밖의 폴더를 선택해 주세요",
        ));
    }
    Ok(Some(real_parent.join(FOLDER)))
}

#[cfg(any(windows, test))]
fn provider_path(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(tail) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{tail}"))
    } else if let Some(tail) = text.strip_prefix(r"\\?\") {
        PathBuf::from(tail)
    } else {
        path.to_owned()
    }
}

fn make_link(root: &Path, link: &Path) -> io::Result<bool> {
    match std::fs::symlink_metadata(link) {
        Ok(_) if ours(link, root) => return Ok(false),
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "선택한 위치에 KASA-share가 이미 있어요. 다른 폴더를 선택해 주세요",
            ))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.canonicalize()?, link)?;
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Paths stay data even when a directory name contains shell metacharacters.
        let script = "$ErrorActionPreference='Stop'; Set-Location -LiteralPath $env:KASATERM_SHARE_PARENT; New-Item -ItemType Junction -Name 'KASA-share' -Target $env:KASATERM_SHARE_ROOT | Out-Null";
        let output = std::process::Command::new("powershell.exe")
            .creation_flags(0x0800_0000)
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                script,
            ])
            .env(
                "KASATERM_SHARE_PARENT",
                provider_path(
                    link.parent()
                        .ok_or_else(|| io::Error::other("바로가기의 폴더가 없어요"))?,
                ),
            )
            .env("KASATERM_SHARE_ROOT", provider_path(&root.canonicalize()?))
            .output()?;
        if !output.status.success() {
            return Err(io::Error::other("공유 바로가기를 만들지 못했어요"));
        }
    }
    if !ours(link, root) {
        return Err(io::Error::other("만든 바로가기의 대상을 확인하지 못했어요"));
    }
    Ok(true)
}

fn remove_link(link: &Path, root: &Path) -> io::Result<()> {
    if !ours(link, root) {
        return Ok(());
    }
    // Only unlink a verified shortcut, never recurse into its target.
    #[cfg(unix)]
    {
        std::fs::remove_file(link)
    }
    #[cfg(windows)]
    {
        std::fs::remove_dir(link)
    }
}

fn save(root: &Path, preferences: &Preferences) -> io::Result<()> {
    let meta = root.join(META);
    std::fs::create_dir_all(&meta)?;
    let metadata = std::fs::symlink_metadata(&meta)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::other("공유 설정 폴더가 올바르지 않아요"));
    }
    let temporary = meta.join(format!("shortcut-{}.part", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec(preferences).map_err(io::Error::other)?)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, meta.join("shortcut.json"))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}

pub fn apply(root: &Path, location: Location, desktop: Option<&Path>) -> io::Result<Preferences> {
    let _guard = CHANGE
        .lock()
        .map_err(|_| io::Error::other("다른 위치 변경이 끝난 뒤 다시 시도해 주세요"))?;
    let previous = load(root)?;
    let next = target(root, &location, desktop)?;
    let made = match next.as_deref() {
        Some(link) => make_link(root, link)?,
        None => false,
    };
    let preferences = Preferences {
        location,
        link: next.clone(),
    };
    if let Err(error) = save(root, &preferences) {
        if made {
            if let Some(link) = next.as_deref() {
                let _ = remove_link(link, root);
            }
        }
        return Err(error);
    }
    // Older versions made the desktop shortcut before recording a managed path.
    let old = previous
        .link
        .or_else(|| desktop.map(|path| path.join(FOLDER)));
    let identity = |path: &Path| {
        path.parent()
            .and_then(|parent| parent.canonicalize().ok())
            .zip(path.file_name())
            .map(|(parent, name)| parent.join(name))
    };
    if let Some(old) = old.filter(|old| {
        next.as_ref()
            .is_none_or(|next| identity(old) != identity(next))
    }) {
        remove_link(&old, root).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("새 위치는 저장했지만 이전 바로가기를 걷지 못했어요: {error}"),
            )
        })?;
    }
    Ok(preferences)
}

impl Engine {
    pub fn shortcut_preferences(&self) -> io::Result<Preferences> {
        load(&self.root)
    }

    pub fn set_shortcut(&self, location: Location) -> io::Result<Preferences> {
        // Isolated apps must never create or remove the real desktop shortcut.
        let desktop = if std::env::var_os("KASATERM_SHARE_DIR").is_some() {
            None
        } else {
            super::desktop_dir()
        };
        let result = apply(&self.root, location, desktop.as_deref());
        if let Ok(mut status) = self.status.lock() {
            if let Ok(preferences) = load(&self.root) {
                status.desktop_link = preferences.link.map(|path| path.display().to_string());
            }
            status.shortcut_error = result.as_ref().err().map(|error| error.to_string());
        }
        self.save();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_provider_paths_preserve_names_without_verbatim_prefixes() {
        assert_eq!(
            provider_path(Path::new(r"\\?\C:\사용자\[자료] & 100%")),
            PathBuf::from(r"C:\사용자\[자료] & 100%")
        );
        assert_eq!(
            provider_path(Path::new(r"\\?\UNC\server\share\folder")),
            PathBuf::from(r"\\server\share\folder")
        );
    }

    fn rig() -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("share-shortcut-{}", uuid::Uuid::new_v4()));
        let root = base.join("store");
        let desktop = base.join("desktop");
        let other = base.join("other");
        for directory in [&root, &desktop, &other] {
            std::fs::create_dir_all(directory).unwrap();
        }
        std::fs::write(root.join("keep.txt"), "original").unwrap();
        (base, root, desktop, other)
    }

    #[test]
    fn location_changes_only_the_owned_shortcut_and_keeps_all_files() {
        let (base, root, desktop, other) = rig();
        apply(&root, Location::Desktop, Some(&desktop)).unwrap();
        assert!(ours(&desktop.join(FOLDER), &root));
        apply(&root, Location::Folder(other.clone()), Some(&desktop)).unwrap();
        assert!(!desktop.join(FOLDER).exists());
        assert!(ours(&other.join(FOLDER), &root));
        apply(&root, Location::Hidden, Some(&desktop)).unwrap();
        assert!(!other.join(FOLDER).exists());
        assert_eq!(
            std::fs::read_to_string(root.join("keep.txt")).unwrap(),
            "original"
        );
        assert_eq!(load(&root).unwrap().location, Location::Hidden);
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn existing_user_folder_and_changed_old_shortcut_are_never_removed() {
        let (base, root, desktop, other) = rig();
        std::fs::create_dir(other.join(FOLDER)).unwrap();
        std::fs::write(other.join(FOLDER).join("user.txt"), "keep").unwrap();
        apply(&root, Location::Desktop, Some(&desktop)).unwrap();
        assert!(apply(&root, Location::Folder(other.clone()), Some(&desktop)).is_err());
        assert!(ours(&desktop.join(FOLDER), &root));
        assert_eq!(load(&root).unwrap().location, Location::Desktop);
        remove_link(&desktop.join(FOLDER), &root).unwrap();
        std::fs::create_dir(desktop.join(FOLDER)).unwrap();
        std::fs::write(desktop.join(FOLDER).join("user.txt"), "keep").unwrap();
        apply(&root, Location::Hidden, Some(&desktop)).unwrap();
        assert_eq!(
            std::fs::read_to_string(desktop.join(FOLDER).join("user.txt")).unwrap(),
            "keep"
        );
        assert_eq!(
            std::fs::read_to_string(other.join(FOLDER).join("user.txt")).unwrap(),
            "keep"
        );
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn invalid_parents_do_not_change_the_saved_location() {
        let (base, root, desktop, _) = rig();
        for parent in [
            PathBuf::from("relative"),
            root.clone(),
            root.join("child"),
            base.join("missing"),
        ] {
            assert!(apply(&root, Location::Folder(parent), Some(&desktop)).is_err());
        }
        assert!(!root.join(META).join("shortcut.json").exists());
        assert!(apply(&root, Location::Desktop, None).is_err());
        assert_eq!(
            std::fs::read_to_string(root.join("keep.txt")).unwrap(),
            "original"
        );
        std::fs::remove_dir_all(base).unwrap();
    }
}
