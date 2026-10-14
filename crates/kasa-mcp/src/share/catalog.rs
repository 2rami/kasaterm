use super::{path, Engine, Status};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct File {
    pub path: String,
    pub name: String,
    pub kind: String,
    pub size: u64,
    pub modified_ms: u64,
    pub origin: String,
    pub sha: String,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Catalog {
    pub files: Vec<File>,
    pub status: Status,
}

impl Engine {
    pub fn catalog(&self) -> Catalog {
        let mut files = self.with_index(|index| {
            index
                .live()
                .map(|(_, entry, _)| File {
                    path: entry.path.clone(),
                    name: entry
                        .path
                        .rsplit('/')
                        .next()
                        .unwrap_or(&entry.path)
                        .to_owned(),
                    kind: path::kind(&entry.path).to_owned(),
                    size: entry.size,
                    modified_ms: entry.mtime_ms,
                    origin: entry.origin.clone(),
                    sha: entry.sha.clone(),
                })
                .collect::<Vec<_>>()
        });
        files.sort_by(|a, b| {
            b.modified_ms
                .cmp(&a.modified_ms)
                .then_with(|| a.path.cmp(&b.path))
        });
        Catalog {
            files,
            status: self.status(),
        }
    }

    pub fn local_path(&self, relative: &str) -> io::Result<PathBuf> {
        if relative.is_empty() {
            return self.root.canonicalize();
        }
        if !path::safe(relative) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "공유 폴더 안의 경로가 아니에요",
            ));
        }
        let key = path::key(relative);
        let (disk, file) = self
            .with_index(|index| {
                if let Some(stat) = index
                    .entries
                    .get(&key)
                    .filter(|entry| !entry.deleted)
                    .and_then(|_| index.stats.get(&key))
                {
                    return Some((stat.disk.clone(), true));
                }
                let prefix = format!("{key}/");
                index
                    .live()
                    .find(|(name, _, _)| name.starts_with(&prefix))
                    .map(|(_, _, stat)| {
                        (
                            stat.disk
                                .split('/')
                                .take(relative.split('/').count())
                                .collect::<Vec<_>>()
                                .join("/"),
                            false,
                        )
                    })
            })
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "파일 목록이 바뀌었어요. 다시 선택해 주세요",
                )
            })?;
        if !path::safe(&disk) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "저장된 파일 경로가 올바르지 않아요",
            ));
        }
        let mut full = self.root.clone();
        for component in disk.split('/') {
            full.push(component);
            let metadata = std::fs::symlink_metadata(&full)?;
            #[cfg(windows)]
            let reparse = {
                use std::os::windows::fs::MetadataExt;
                metadata.file_attributes() & 0x400 != 0
            };
            #[cfg(not(windows))]
            let reparse = false;
            if metadata.file_type().is_symlink() || reparse {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "공유 폴더의 링크는 열 수 없어요",
                ));
            }
            if !metadata.is_dir() && !metadata.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "일반 파일이나 폴더만 열 수 있어요",
                ));
            }
        }
        let metadata = std::fs::metadata(&full)?;
        if (file && !metadata.is_file()) || (!file && !metadata.is_dir()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "파일 종류가 바뀌었어요. 다시 선택해 주세요",
            ));
        }
        if !full.canonicalize()?.starts_with(self.root.canonicalize()?) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "공유 폴더 밖은 열 수 없어요",
            ));
        }
        Ok(full)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn catalog_and_local_open_use_only_indexed_live_files() {
        let root = std::env::temp_dir().join(format!("share-catalog-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("시안/안쪽")).unwrap();
        std::fs::write(root.join("시안/안쪽/a.md"), b"a").unwrap();
        let engine = Engine::open(root.clone(), "test".into(), "test".into());
        let mut pending = HashMap::new();
        engine.scan(&mut pending);
        assert!(engine.catalog().files.is_empty());
        engine.scan(&mut pending);
        let files = engine.catalog().files;
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name, "a.md");
        assert_eq!(files[0].kind, "markdown");
        assert_eq!(
            engine.local_path("시안/안쪽").unwrap(),
            root.join("시안/안쪽")
        );
        assert!(engine.local_path("../escape").is_err());
        assert!(engine.local_path(".kasaterm/index.json").is_err());
        std::fs::write(root.join("not-indexed.txt"), b"no").unwrap();
        assert!(engine.local_path("not-indexed.txt").is_err());
        std::fs::remove_file(root.join("시안/안쪽/a.md")).unwrap();
        assert!(engine.local_path("시안/안쪽/a.md").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn replacing_an_indexed_parent_with_a_link_cannot_open_outside() {
        let root = std::env::temp_dir().join(format!("share-open-link-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("folder")).unwrap();
        std::fs::write(root.join("folder/a.txt"), b"a").unwrap();
        let engine = Engine::open(root.clone(), "test".into(), "test".into());
        let mut pending = HashMap::new();
        engine.scan(&mut pending);
        engine.scan(&mut pending);
        std::fs::rename(root.join("folder"), root.join("original")).unwrap();
        std::os::unix::fs::symlink(root.join("original"), root.join("folder")).unwrap();
        assert!(engine.local_path("folder/a.txt").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
