use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

const SEARCH_URL: &str = "http://127.0.0.1:6341/search";
const SEARCH_LIMIT: usize = 30;
const RESPONSE_LIMIT: usize = 256 * 1024;

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<Snippet>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub(crate) struct Snippet {
    #[serde(default)]
    pub head: String,
    pub text: String,
}

#[derive(serde::Deserialize)]
struct SearchResponse {
    mode: String,
    results: Vec<SearchHit>,
}

#[derive(serde::Deserialize)]
struct SearchHit {
    path: String,
    #[serde(default)]
    snippets: Vec<Snippet>,
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

fn memory_roots(home: &Path) -> impl Iterator<Item = PathBuf> + '_ {
    ["내 드라이브/MEMORY", "memory-vault"].into_iter()
        .filter_map(|relative| home.join(relative).canonicalize().ok())
        .filter(|root| root.join("MEMORY.md").is_file())
}

pub(crate) fn default_memory_root() -> Option<PathBuf> {
    memory_roots(&kasa_socket::home_dir()?).next()
}

fn is_memory_root(root: &Path, home: &Path) -> bool {
    root.canonicalize().is_ok_and(|root| memory_roots(home).any(|candidate| candidate == root))
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
        result.entries.push(Entry { id, name, kind: kind(&entry.path(), file_type.is_dir()), snippet: None });
    }
    result.entries.sort_by(|a,b| (a.kind != "folder").cmp(&(b.kind != "folder")).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())).then_with(|| a.id.cmp(&b.id)));
    let limit = limit.clamp(500, 20_000);
    if result.entries.len() > limit { result.entries.truncate(limit); result.next_cursor = Some(limit); }
    if truncated { result.error = Some("폴더가 커서 일부 항목만 표시해요. 하위 폴더를 선택해 주세요".into()); }
    result
}

pub(crate) fn search(root: &Path, query: &str, generation: u64, current: &AtomicU64) -> (Vec<Entry>, Option<String>) {
    if query.trim().is_empty() || current.load(Ordering::Relaxed) != generation {
        return (Vec::new(), None);
    }
    if kasa_socket::home_dir().is_some_and(|home| is_memory_root(root, &home)) {
        if let Some(response) = search_memory(query, generation, current) {
            let entries = search_entries(root, response);
            if !entries.is_empty() && current.load(Ordering::Relaxed) == generation {
                return (entries, None);
            }
        }
    }
    search_names(root, query, generation, current)
}

fn search_memory(query: &str, generation: u64, current: &AtomicU64) -> Option<SearchResponse> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().ok()?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_millis(600))
        .timeout(Duration::from_secs(4))
        .build().ok()?;
    let body = serde_json::json!({ "q": query, "limit": SEARCH_LIMIT });
    match runtime.block_on(post_search(&client, SEARCH_URL, &body)) {
        Ok(response) => return response,
        Err(error) if error.is_connect() && current.load(Ordering::Relaxed) == generation => {}
        Err(_) => return None,
    }
    // 연결이 거절됐을 때만 포워딩을 연다. 이미 포워딩된 데몬의 HTTP 오류는 재전송하지 않는다.
    let machine = kasa_device::machines::find_route("나쵸네코").filter(|machine| !machine.guest)?;
    let (url, _, _) = kasa_remote::netfwd::show_from_machine_blocking(&machine, SEARCH_URL).ok()?;
    if current.load(Ordering::Relaxed) != generation {
        return None;
    }
    runtime.block_on(post_search(&client, &url, &body)).ok().flatten()
}

async fn post_search(
    client: &reqwest::Client,
    url: &str,
    body: &serde_json::Value,
) -> Result<Option<SearchResponse>, reqwest::Error> {
    let mut response = client.post(url).json(body).send().await?;
    if !response.status().is_success() || response.content_length().is_some_and(|n| n > RESPONSE_LIMIT as u64) {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len() + chunk.len() > RESPONSE_LIMIT { return Ok(None); }
        bytes.extend_from_slice(&chunk);
    }
    Ok(serde_json::from_slice(&bytes).ok())
}

fn search_entries(root: &Path, response: SearchResponse) -> Vec<Entry> {
    if !matches!(response.mode.as_str(), "hybrid" | "literal") {
        return Vec::new();
    }
    let mut seen = std::collections::HashSet::new();
    response.results.into_iter().take(SEARCH_LIMIT).filter_map(|hit| {
        let path = resolve(root, &hit.path).ok()?;
        if kind(&path, false) != "markdown" || !path.is_file() || !seen.insert(path) {
            return None;
        }
        let name = Path::new(&hit.path).file_name()?.to_string_lossy().into_owned();
        let snippet = hit.snippets.into_iter().find(|snippet| !snippet.text.trim().is_empty());
        Some(Entry { id: hit.path, name, kind: "markdown", snippet })
    }).collect()
}

fn search_names(root: &Path, query: &str, generation: u64, current: &AtomicU64) -> (Vec<Entry>, Option<String>) {
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

    #[test]
    fn semantic_search_is_limited_to_the_memory_vault_and_local_markdown() {
        let home = std::env::temp_dir().join(format!("kasaterm-vault-{}", uuid::Uuid::new_v4()));
        let root = home.join("내 드라이브/MEMORY");
        let other = home.join("other/MEMORY");
        std::fs::create_dir_all(root.join("notes")).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        for path in [root.join("MEMORY.md"), other.join("MEMORY.md"), root.join("notes/one.md"),
                     root.join("notes/two.md"), root.join("notes/photo.png"), root.join(".hidden.md")] {
            std::fs::write(path, "fixture").unwrap();
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(other.join("MEMORY.md"), root.join("outside.md")).unwrap();
        assert!(is_memory_root(&root, &home));
        assert!(!is_memory_root(&other, &home));
        let response = serde_json::from_value(serde_json::json!({
            "mode": "hybrid",
            "results": [
                {"path": "../outside.md"},
                {"path": other.join("MEMORY.md")},
                {"path": "outside.md"},
                {"path": "notes/two.md", "snippets": [{"head": "heading", "text": "a result"}]},
                {"path": "notes/two.md"},
                {"path": "notes/photo.png"},
                {"path": "notes/missing.md"},
                {"path": ".hidden.md"},
                {"path": "notes/one.md"}
            ]
        })).unwrap();
        let hits = search_entries(&root, response);
        assert_eq!(hits.iter().map(|entry| entry.id.as_str()).collect::<Vec<_>>(), ["notes/two.md", "notes/one.md"]);
        assert_eq!(hits[0].snippet.as_ref().unwrap().text, "a result");
        let current = AtomicU64::new(2);
        assert!(search(&root, "one", 1, &current).0.is_empty());
        assert_eq!(search_names(&root, "ONE", 2, &current).0[0].id, "notes/one.md");
        std::fs::remove_dir_all(home).unwrap();
    }
}
