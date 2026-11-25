use std::path::{Path, PathBuf};

pub const MAX_BYTES: u64 = 12 * 1024 * 1024;
/// 움직이는 그림 원본(GIF·WebP·APNG) 상한. 넘으면 첫 장만 정지 그림으로 등록한다.
pub const ANIM_MAX_BYTES: u64 = 16 * 1024 * 1024;

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

/// 결과 그림의 움직이는 원본(`<id>.anim`). 정지 그림 PNG 의 `kasaterm-anim` 글 조각이 이 파일의 SHA-256 을
/// 들고 있어, 받는 쪽은 그 값으로 원본을 확인한다. 돌려주는 것은 바이트와 MIME.
pub fn read_anim(id: &str) -> std::io::Result<(Vec<u8>, &'static str)> {
    let root = root().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "image store unavailable")
    })?;
    read_anim_at(&root, id)
}

fn read_anim_at(root: &Path, id: &str) -> std::io::Result<(Vec<u8>, &'static str)> {
    use std::io::{Error, ErrorKind, Read};
    if !valid_id(id) {
        return Err(Error::new(ErrorKind::InvalidInput, "invalid image id"));
    }
    let path = root.join(format!("{id}.anim"));
    let meta = std::fs::symlink_metadata(&path)?;
    if !meta.file_type().is_file() || meta.len() > ANIM_MAX_BYTES {
        return Err(Error::new(ErrorKind::InvalidInput, "invalid animation asset"));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(ANIM_MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    match anim_mime(&bytes) {
        Some(mime) if bytes.len() as u64 <= ANIM_MAX_BYTES => Ok((bytes, mime)),
        _ => Err(Error::new(ErrorKind::InvalidData, "invalid animation asset")),
    }
}

/// 받아 줄 움직이는 그림 형식만 MIME 을 준다 — 그 밖의 바이트는 원본으로 내보내지 않는다.
pub fn anim_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/apng")
    } else {
        None
    }
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
            Some("png" | "anim") if path.file_stem().and_then(|s| s.to_str()).is_some_and(valid_id) => keep,
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
    if let Ok(file) = open_for_times(path) {
        let _ = file.set_modified(std::time::SystemTime::now());
    }
}

/// 수정 시각만 고칠 수 있게 연다. Windows 는 읽기로 연 파일엔 시각을 못 쓴다(SetFileTime 이 속성 쓰기 권한을
/// 요구한다) — 그래서 같은 결과를 다시 등록해도 보관 기간이 안 늘었다. 읽기 전용으로 봉인한 파일도 속성은 고친다.
pub fn open_for_times(path: &Path) -> std::io::Result<std::fs::File> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
        std::fs::OpenOptions::new().access_mode(FILE_WRITE_ATTRIBUTES).open(path)
    }
    #[cfg(not(windows))]
    std::fs::File::open(path)
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
        let (old, fresh, part, other, anim) = (
            root.join("0123456789abcdef.png"),
            root.join("1111111111111111.png"),
            root.join("abc.part"),
            root.join("notes.txt"),
            root.join("0123456789abcdef.anim"),
        );
        for f in [&old, &fresh, &part, &other, &anim] {
            std::fs::write(f, b"x").unwrap();
        }
        for (f, d) in [(&old, 31), (&part, 2), (&other, 99), (&anim, 31)] {
            open_for_times(f).unwrap().set_modified(ago(d)).unwrap();
        }
        assert_eq!(prune(&root, keep), 3);
        assert!(fresh.exists() && other.exists(), "모르는 파일은 건드리지 않는다");
        open_for_times(&fresh).unwrap().set_modified(ago(40)).unwrap();
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
        std::fs::write(root.join("0123456789abcdef.anim"), b"GIF89a-fixture").unwrap();
        assert_eq!(read_anim_at(&root, "0123456789abcdef").unwrap(), (b"GIF89a-fixture".to_vec(), "image/gif"));
        std::fs::write(root.join("1111111111111111.anim"), b"#!/bin/sh").unwrap();
        assert!(read_anim_at(&root, "1111111111111111").is_err(), "모르는 바이트는 원본으로 안 낸다");
        assert!(read_anim_at(&root, "../secret").is_err());
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
