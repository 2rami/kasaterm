use std::path::{Path, PathBuf};

pub const MAX_BYTES: u64 = 12 * 1024 * 1024;

pub fn root() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("KASATERM_INLINE_RESULT_DIR").filter(|s| !s.is_empty()) {
        return Some(path.into());
    }
    if let Some(settings) = std::env::var_os("KASATERM_SETTINGS_FILE") {
        return Some(PathBuf::from(settings).parent()?.join("inline-results"));
    }
    Some(crate::home_dir()?.join(".config/kasaterm/inline-results"))
}

pub fn valid_id(id: &str) -> bool {
    id.len() == 16
        && id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

pub fn read(id: &str) -> std::io::Result<Vec<u8>> {
    let root = root().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "image store unavailable")
    })?;
    read_at(&root, id)
}

fn read_at(root: &Path, id: &str) -> std::io::Result<Vec<u8>> {
    use std::io::{Error, ErrorKind, Read};
    if !valid_id(id) {
        return Err(Error::new(ErrorKind::InvalidInput, "invalid image id"));
    }
    let path = root.join(format!("{id}.png"));
    let meta = std::fs::symlink_metadata(&path)?;
    if !meta.file_type().is_file() || meta.len() > MAX_BYTES {
        return Err(Error::new(ErrorKind::InvalidInput, "invalid image asset"));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(Error::new(ErrorKind::InvalidData, "invalid image asset"));
    }
    Ok(bytes)
}

/// 만든 지 `keep` 가 지난 결과 그림과 하루 지난 쓰다 만 조각을 걷는다(만든 날부터 30일 — `inline_media::KEEP`).
/// 그림 파일의 수정 시각이 만든 때다 — 같은 그림을 다시 등록하면 `touch` 로 새로 센다. 돌려주는 것은 지운 수.
pub fn prune(root: &Path, keep: std::time::Duration) -> usize {
    let day = std::time::Duration::from_secs(24 * 60 * 60);
    let mut removed = 0;
    for entry in std::fs::read_dir(root).into_iter().flatten().flatten() {
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
        let age = meta.modified().ok().and_then(|t| t.elapsed().ok()).unwrap_or_default();
        let limit = match path.extension().and_then(|e| e.to_str()) {
            Some("png") if path.file_stem().and_then(|s| s.to_str()).is_some_and(valid_id) => keep,
            Some("part") => day,
            _ => continue,
        };
        if meta.is_file() && age > limit && std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// 이미 있는 그림을 다시 등록했다 — 보관 기간을 오늘부터 다시 센다.
pub fn touch(path: &Path) {
    if let Ok(file) = std::fs::File::open(path) {
        let _ = file.set_modified(std::time::SystemTime::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pictures_older_than_the_keep_and_stale_parts_go() {
        let root = std::env::temp_dir().join(format!("inline-result-prune-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&root).unwrap();
        let keep = std::time::Duration::from_secs(30 * 24 * 60 * 60);
        let ago = |days: u64| std::time::SystemTime::now() - std::time::Duration::from_secs(days * 24 * 60 * 60);
        let (old, fresh, part, other) = (
            root.join("0123456789abcdef.png"),
            root.join("1111111111111111.png"),
            root.join("abc.part"),
            root.join("notes.txt"),
        );
        for f in [&old, &fresh, &part, &other] {
            std::fs::write(f, b"x").unwrap();
        }
        for (f, d) in [(&old, 31), (&part, 2), (&other, 99)] {
            std::fs::File::open(f).unwrap().set_modified(ago(d)).unwrap();
        }
        assert_eq!(prune(&root, keep), 2);
        assert!(fresh.exists() && other.exists(), "모르는 파일은 건드리지 않는다");
        std::fs::File::open(&fresh).unwrap().set_modified(ago(40)).unwrap();
        touch(&fresh);
        assert_eq!(prune(&root, keep), 0, "다시 만든 그림은 오늘부터");
    }
    #[test]
    fn only_registered_png_ids_can_be_read() {
        let root =
            std::env::temp_dir().join(format!("inline-result-read-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&root).unwrap();
        let bytes = b"\x89PNG\r\n\x1a\nfixture";
        std::fs::write(root.join("0123456789abcdef.png"), bytes).unwrap();
        assert_eq!(read_at(&root, "0123456789abcdef").unwrap(), bytes);
        for id in ["../secret", "0123456789abcdef.png", "0123456789ABCDEF", ""] {
            assert!(read_at(&root, id).is_err());
        }
        std::fs::write(root.join("1111111111111111.png"), b"not a PNG").unwrap();
        assert!(read_at(&root, "1111111111111111").is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                root.join("0123456789abcdef.png"),
                root.join("2222222222222222.png"),
            )
            .unwrap();
            assert!(read_at(&root, "2222222222222222").is_err());
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
