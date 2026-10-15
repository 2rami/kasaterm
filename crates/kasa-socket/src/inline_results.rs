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

#[cfg(test)]
mod tests {
    use super::*;
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
