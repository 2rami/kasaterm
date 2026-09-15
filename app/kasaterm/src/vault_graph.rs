use std::{collections::{BTreeSet, HashMap, HashSet}, io::Read, path::Path, sync::{Arc, atomic::{AtomicU64, Ordering}}};
use pulldown_cmark::{Event, LinkType, Options, Parser, Tag};

#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct Node { pub id: String, pub name: String }
#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct Edge { pub source: String, pub target: String }
#[derive(Clone, Debug, Default, serde::Serialize)]
pub(crate) struct Graph { pub nodes: Vec<Node>, pub edges: Vec<Edge>, pub truncated: bool, pub error: Option<String> }

#[derive(Clone, Copy)]
struct Limits { nodes: usize, entries: usize, bytes: usize, file_bytes: usize, edges: usize, depth: usize }
const LIMITS: Limits = Limits { nodes: 2000, entries: 50_000, bytes: 16 * 1024 * 1024, file_bytes: 1024 * 1024, edges: 10_000, depth: 32 };

pub(crate) fn scan(root: &Path, generation: u64, current: Arc<AtomicU64>) -> Graph {
    scan_bounded(root, generation, &current, LIMITS)
}

fn scan_bounded(root: &Path, generation: u64, current: &AtomicU64, limits: Limits) -> Graph {
    let mut graph = Graph::default();
    let mut pending = vec![(String::new(), 0)];
    let mut visited = 0;
    let mut attachments = HashSet::new();
    while let Some((directory, depth)) = pending.pop() {
        if current.load(Ordering::Relaxed) != generation { return Graph { error: Some("그래프 요청이 바뀌었어요".into()), ..Graph::default() }; }
        let listing = crate::vault::list(root, &directory, 20_000);
        if let Some(error) = listing.error { graph.truncated = true; graph.error.get_or_insert(error); }
        for entry in listing.entries {
            visited += 1;
            if visited > limits.entries { graph.truncated = true; break; }
            if entry.kind == "folder" {
                if depth < limits.depth { pending.push((entry.id, depth + 1)); } else { graph.truncated = true; }
            } else if Path::new(&entry.id).extension().is_some_and(|ext| ext.eq_ignore_ascii_case("md")) {
                if graph.nodes.len() >= limits.nodes { graph.truncated = true; break; }
                graph.nodes.push(Node { name: Path::new(&entry.name).file_stem().unwrap_or_default().to_string_lossy().into_owned(), id: entry.id });
            } else {
                attachments.insert(entry.id);
            }
        }
        if visited > limits.entries || graph.nodes.len() >= limits.nodes { graph.truncated = true; break; }
    }
    graph.nodes.sort_by(|a, b| a.id.cmp(&b.id));
    let ids: HashSet<&str> = graph.nodes.iter().map(|node| node.id.as_str()).collect();
    let mut names: HashMap<String, Vec<&str>> = HashMap::new();
    for node in &graph.nodes { names.entry(node.name.clone()).or_default().push(&node.id); }
    let mut edges = BTreeSet::new();
    let mut bytes = 0;
    for node in &graph.nodes {
        if current.load(Ordering::Relaxed) != generation { graph.error = Some("그래프 요청이 바뀌었어요".into()); return graph; }
        let text = (|| {
            let path = crate::vault::resolve(root, &node.id).ok()?;
            if std::fs::symlink_metadata(root.join(&node.id)).ok()?.file_type().is_symlink() { return None; }
            let file = std::fs::File::open(path).ok()?;
            let mut text = String::new();
            file.take((limits.file_bytes + 1) as u64).read_to_string(&mut text).ok()?;
            Some(text)
        })();
        let Some(text) = text else { graph.truncated = true; continue; };
        if text.len() > limits.file_bytes { graph.truncated = true; continue; }
        bytes += text.len();
        if bytes > limits.bytes { graph.truncated = true; break; }
        for (target, wiki) in links(&text) {
            if let Some(target) = resolve_link(&node.id, &target, wiki, &ids, &names, &attachments) {
                if target == node.id { continue; }
                if edges.len() >= limits.edges { graph.truncated = true; break; }
                edges.insert((node.id.clone(), target));
            }
        }
        if edges.len() >= limits.edges { graph.truncated = true; break; }
    }
    graph.edges = edges.into_iter().map(|(source, target)| Edge { source, target }).collect();
    graph
}

fn links(text: &str) -> Vec<(String, bool)> {
    Parser::new_ext(text, Options::ENABLE_WIKILINKS).filter_map(|event| match event {
        Event::Start(Tag::Link { link_type, dest_url, .. }) => Some((dest_url.into_string(), matches!(link_type, LinkType::WikiLink { .. }))),
        Event::Start(Tag::Image { link_type: LinkType::WikiLink { .. }, dest_url, .. }) => Some((dest_url.into_string(), true)),
        _ => None,
    }).collect()
}

fn decode(value: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(value.len());
    let mut input = value.as_bytes().iter().copied();
    while let Some(byte) = input.next() {
        if byte == b'%' { bytes.push((input.next()? as char).to_digit(16)? as u8 * 16 + (input.next()? as char).to_digit(16)? as u8); }
        else { bytes.push(byte); }
    }
    String::from_utf8(bytes).ok()
}

fn normalize(base: &str, target: &str) -> Option<String> {
    let mut parts: Vec<&str> = base.split('/').filter(|part| !part.is_empty()).collect();
    for part in target.split('/') {
        match part { "" | "." => {}, ".." => { parts.pop()?; }, _ if crate::vault::visible(part) => parts.push(part), _ => return None }
    }
    Some(parts.join("/"))
}

fn resolve_link(source: &str, value: &str, wiki: bool, ids: &HashSet<&str>, names: &HashMap<String, Vec<&str>>, attachments: &HashSet<String>) -> Option<String> {
    let value = value.split(['#', '?']).next()?.trim();
    let value = decode(value)?;
    if value.is_empty() || value.starts_with('/') || value.contains([':', '\\', '\0']) { return None; }
    let base = source.rsplit_once('/').map_or("", |(base, _)| base);
    if wiki && (attachments.contains(&normalize(base, &value)?) || normalize("", &value).is_some_and(|id| attachments.contains(&id))) { return None; }
    let target = match Path::new(&value).extension() {
        Some(ext) if ext.eq_ignore_ascii_case("md") => value.clone(),
        _ if wiki => format!("{value}.md"),
        _ => return None,
    };
    let relative = normalize(base, &target)?;
    if ids.contains(relative.as_str()) { return Some(relative); }
    if wiki {
        let rooted = normalize("", &target)?;
        if ids.contains(rooted.as_str()) { return Some(rooted); }
        if !value.contains('/') {
            let name = Path::new(&target).file_stem()?.to_str()?;
            if let Some(matches) = names.get(name) { if matches.len() == 1 { return Some(matches[0].into()); } }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self { Self(std::env::temp_dir().join(format!("kasa-graph-{}", uuid::Uuid::new_v4()))) }
        fn put(&self, name: &str, text: &str) {
            let path = self.0.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        fn scan(&self, limits: Limits) -> Graph { scan_bounded(&self.0, 1, &AtomicU64::new(1), limits) }
    }
    impl Drop for Fixture { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
    #[test]
    fn links_resolve_nested_alias_anchor_encoded_relative_and_embedded_notes() {
        let fixture = Fixture::new();
        fixture.put("notes/start.md", "[[deep|Label#heading]] [[deep#heading|Label]] ![[embed]] [Space](../Space%20Note.md#heading) [relative](../other/deep.md) [[duplicate]] [escape](../../outside.md) [web](https://example.org/no.md) ![](image.png)");
        for name in ["other/deep.md", "other/embed.md", "Space Note.md", "a/duplicate.md", "b/duplicate.md"] { fixture.put(name, ""); }
        let graph = fixture.scan(LIMITS);
        let targets: BTreeSet<_> = graph.edges.iter().filter(|edge| edge.source == "notes/start.md").map(|edge| edge.target.as_str()).collect();
        assert_eq!(targets, BTreeSet::from(["other/deep.md", "other/embed.md", "Space Note.md"]));
        assert!(!graph.truncated);
    }
    #[test]
    fn parser_ignores_code_comments_and_escaped_wiki_syntax() {
        let text = "`[[inline]]`\n\n```md\n[[fence]]\n[x](fence.md)\n```\n\n<!-- [[comment]] -->\n\\[\\[escaped]]\n[[real]]";
        assert_eq!(links(text), vec![("real".into(), true)]);
    }
    #[test]
    fn dotted_wiki_titles_work_without_hijacking_attachments() {
        let fixture = Fixture::new();
        fixture.put("start.md", "[[v1.2]] [[nested/version.3]] [[image.png]] [image](image.png) [missing](missing.png) [[guide.pdf]]");
        for name in ["versions/v1.2.md", "nested/version.3.md", "image.png", "image.png.md", "missing.png.md", "guide.pdf", "guide.pdf.md"] { fixture.put(name, ""); }
        let graph = fixture.scan(LIMITS);
        let targets: BTreeSet<_> = graph.edges.iter().map(|edge| edge.target.as_str()).collect();
        assert_eq!(targets, BTreeSet::from(["versions/v1.2.md", "nested/version.3.md"]));
    }
    #[test]
    fn boundaries_limits_and_stale_requests_are_explicit() {
        let fixture = Fixture::new();
        fixture.put("start.md", "[[other]]"); fixture.put("other.md", "");
        fixture.put(".hidden/secret.md", ""); fixture.put("build/output.md", "");
        #[cfg(unix)] std::os::unix::fs::symlink(&fixture.0, fixture.0.join("loop")).unwrap();
        assert_eq!(fixture.scan(LIMITS).nodes.len(), 2);
        assert!(fixture.scan(Limits { nodes: 1, ..LIMITS }).truncated);
        assert!(fixture.scan(Limits { bytes: 1, ..LIMITS }).truncated);
        assert!(fixture.scan(Limits { file_bytes: 1, ..LIMITS }).truncated);
        assert!(fixture.scan(Limits { entries: 1, ..LIMITS }).truncated);
        assert!(fixture.scan(Limits { edges: 0, ..LIMITS }).truncated);
        fixture.put("nested/note.md", "");
        assert!(fixture.scan(Limits { depth: 0, ..LIMITS }).truncated);
        assert!(scan_bounded(&fixture.0, 1, &AtomicU64::new(2), LIMITS).error.is_some());
    }
}
