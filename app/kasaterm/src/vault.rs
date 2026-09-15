use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, atomic::{AtomicU64, Ordering}};

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct Record {
    pub root: String,
    #[serde(default)]
    pub active: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct Entry {
    pub id: String,
    pub name: String,
    pub kind: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Listing {
    pub parent_id: String,
    pub entries: Vec<Entry>,
    pub next_cursor: Option<usize>,
    pub error: Option<String>,
}

pub(crate) fn visible(name: &str) -> bool {
    !name.starts_with('.') && !matches!(name, "node_modules" | "target" | "dist" | "build" | "__pycache__" | "vendor" | "coverage")
}

pub(crate) fn resolve(root: &Path, relative: &str) -> Result<PathBuf, String> {
    if Path::new(relative).components().any(|part| !matches!(part, Component::Normal(name) if visible(&name.to_string_lossy()))) {
        return Err("볼트 안의 파일만 열 수 있어요".into());
    }
    let root = root.canonicalize().map_err(|_| "볼트 폴더를 찾을 수 없어요")?;
    let path = root.join(relative).canonicalize().map_err(|_| "파일을 찾을 수 없어요")?;
    if !path.starts_with(&root) { return Err("볼트 밖의 연결은 열 수 없어요".into()); }
    Ok(path)
}

pub(crate) fn kind(path: &Path, directory: bool) -> &'static str {
    if directory { return "folder"; }
    match path.extension().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase().as_str() {
        "md" | "markdown" | "mdown" | "mkd" => "markdown",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "heic" => "image",
        "pdf" => "pdf",
        _ => "file",
    }
}

pub(crate) fn list(root: &Path, relative: &str, limit: usize) -> Listing {
    let mut result = Listing { parent_id: relative.into(), entries: Vec::new(), next_cursor: None, error: None };
    let path = match resolve(root, relative) { Ok(path) => path, Err(error) => { result.error = Some(error); return result; } };
    let iterator = match std::fs::read_dir(path) { Ok(entries) => entries, Err(error) => { result.error = Some(format!("폴더를 읽지 못했어요: {error}")); return result; } };
    let mut truncated = false;
    for (index, entry) in iterator.enumerate() {
        if index >= 20_000 { truncated = true; break; }
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().to_string();
        if !visible(&name) { continue; }
        let Ok(file_type) = entry.file_type() else { continue };
        if file_type.is_symlink() || (!file_type.is_file() && !file_type.is_dir()) { continue; }
        let id = if relative.is_empty() { name.clone() } else { format!("{relative}/{name}") };
        result.entries.push(Entry { id, name, kind: kind(&entry.path(), file_type.is_dir()) });
    }
    result.entries.sort_by(|a,b| (a.kind != "folder").cmp(&(b.kind != "folder")).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())).then_with(|| a.id.cmp(&b.id)));
    let limit = limit.clamp(500, 20_000);
    if result.entries.len() > limit { result.entries.truncate(limit); result.next_cursor = Some(limit); }
    if truncated { result.error = Some("폴더가 커서 일부 항목만 표시해요. 하위 폴더를 선택해 주세요".into()); }
    result
}

pub(crate) fn search(root: &Path, query: &str, generation: u64, current: Arc<AtomicU64>) -> (Vec<Entry>, Option<String>) {
    let mut pending = vec![(String::new(), 0usize)];
    let query = query.to_lowercase();
    let mut found = Vec::new();
    let mut visited = 0usize;
    while let Some((directory, depth)) = pending.pop() {
        if current.load(Ordering::Relaxed) != generation { return (Vec::new(), None); }
        let listing = list(root, &directory, 20_000);
        for entry in listing.entries {
            visited += 1;
            if visited > 50_000 || found.len() >= 500 { return (found, Some("검색 결과가 많아요. 파일 이름을 더 입력해 주세요".into())); }
            if entry.name.to_lowercase().contains(&query) { found.push(entry.clone()); }
            if entry.kind == "folder" && depth < 32 { pending.push((entry.id, depth + 1)); }
        }
    }
    (found, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tree_keeps_attachments_and_blocks_escape_and_symlinks() {
        let root = std::env::temp_dir().join(format!("kasaterm-vault-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("notes")).unwrap();
        std::fs::create_dir(root.join("node_modules")).unwrap();
        for name in ["note.md", "image.png", "guide.pdf", ".hidden"] { std::fs::write(root.join(name), b"fixture").unwrap(); }
        #[cfg(unix)] std::os::unix::fs::symlink(&root, root.join("loop")).unwrap();
        let entries = list(&root, "", 500).entries;
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].kind, "folder");
        assert!(entries.iter().any(|entry| entry.kind == "image"));
        assert!(entries.iter().any(|entry| entry.kind == "pdf"));
        assert!(resolve(&root, "../outside").is_err());
        assert!(resolve(&root, "/etc/passwd").is_err());
        assert!(resolve(&root, "node_modules/secret").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
