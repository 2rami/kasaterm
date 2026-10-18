//! 펼친 폴더만 읽는 트리. 보이는 줄은 펼침 상태에서 매번 다시 엮는다.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Clone, Debug)]
struct Entry {
    name: String,
    dir: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub path: PathBuf,
    pub name: String,
    pub dir: bool,
    pub depth: usize,
    pub open: bool,
}

pub struct Tree {
    pub root: PathBuf,
    expanded: HashSet<PathBuf>,
    children: HashMap<PathBuf, (Option<SystemTime>, Vec<Entry>)>,
    pub rows: Vec<Row>,
    pub selected: usize,
    pub show_hidden: bool,
    pub filter: String,
}

fn mtime(dir: &Path) -> Option<SystemTime> {
    std::fs::metadata(dir).and_then(|m| m.modified()).ok()
}

fn read(dir: &Path) -> Vec<Entry> {
    let mut entries: Vec<Entry> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            // 심볼릭 링크는 가리키는 쪽이 폴더면 폴더로 편다.
            let dir = std::fs::metadata(entry.path()).is_ok_and(|m| m.is_dir());
            (name != ".git").then_some(Entry { name, dir })
        })
        .collect();
    entries.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    entries
}

/// 셸이 선 폴더에서 트리 뿌리로 삼을 곳 — 저장소 안이면 그 최상위다.
pub fn root_for(cwd: &Path) -> PathBuf {
    cwd.ancestors()
        .find(|dir| dir.join(".git").exists())
        .unwrap_or(cwd)
        .to_path_buf()
}

impl Tree {
    pub fn new(root: PathBuf) -> Self {
        let mut tree = Self {
            expanded: HashSet::from([root.clone()]),
            root,
            children: HashMap::new(),
            rows: Vec::new(),
            selected: 0,
            show_hidden: false,
            filter: String::new(),
        };
        tree.rebuild();
        tree
    }

    fn entries(&mut self, dir: &Path) -> Vec<Entry> {
        self.children
            .entry(dir.to_path_buf())
            .or_insert_with(|| (mtime(dir), read(dir)))
            .1
            .clone()
    }

    fn walk(&mut self, dir: &Path, depth: usize, rows: &mut Vec<Row>) {
        for entry in self.entries(dir) {
            if !self.show_hidden && entry.name.starts_with('.') {
                continue;
            }
            let path = dir.join(&entry.name);
            let open = entry.dir && self.expanded.contains(&path);
            rows.push(Row { path: path.clone(), name: entry.name, dir: entry.dir, depth, open });
            if open {
                self.walk(&path, depth + 1, rows);
            }
        }
    }

    /// 펼침·숨김·거르기를 반영해 보이는 줄을 다시 엮는다. 고른 줄은 경로로 지킨다.
    pub fn rebuild(&mut self) {
        let keep = self.selected_path().map(Path::to_path_buf);
        let root = self.root.clone();
        let name = root.file_name().map_or_else(|| root.to_string_lossy().into_owned(), |n| n.to_string_lossy().into_owned());
        let mut rows = vec![Row { path: root.clone(), name, dir: true, depth: 0, open: true }];
        self.walk(&root, 1, &mut rows);
        if !self.filter.is_empty() {
            rows = filtered(rows, &self.filter);
        }
        self.rows = rows;
        self.selected = keep
            .and_then(|path| self.rows.iter().position(|row| row.path == path))
            .unwrap_or(self.selected)
            .min(self.rows.len().saturating_sub(1));
    }

    pub fn selected_path(&self) -> Option<&Path> {
        self.rows.get(self.selected).map(|row| row.path.as_path())
    }

    pub fn selected_row(&self) -> Option<&Row> {
        self.rows.get(self.selected)
    }

    pub fn move_by(&mut self, delta: isize) {
        let last = self.rows.len().saturating_sub(1) as isize;
        self.selected = (self.selected as isize + delta).clamp(0, last) as usize;
    }

    pub fn set_open(&mut self, path: &Path, open: bool) {
        if path == self.root {
            return;
        }
        if open {
            self.expanded.insert(path.to_path_buf());
        } else {
            self.expanded.retain(|dir| !dir.starts_with(path));
        }
        self.rebuild();
    }

    /// 접힌 줄이나 파일에서 왼쪽은 부모 줄로 간다.
    pub fn collapse_or_parent(&mut self) {
        let Some(row) = self.selected_row().cloned() else { return };
        if row.open && row.depth > 0 {
            self.set_open(&row.path, false);
        } else if let Some(parent) = row.path.parent().filter(|p| p.starts_with(&self.root)) {
            if let Some(at) = self.rows.iter().position(|r| r.path == parent) {
                self.selected = at;
            }
        }
    }

    /// 그 경로까지 펼치고 고른다. 뿌리 밖이면 뿌리를 그 경로의 저장소 최상위로 옮긴다.
    pub fn reveal(&mut self, path: &Path) {
        if !path.starts_with(&self.root) {
            self.root = root_for(path);
            self.expanded = HashSet::from([self.root.clone()]);
            self.children.clear();
            self.filter.clear();
        }
        for dir in path.ancestors().take_while(|dir| dir.starts_with(&self.root)) {
            self.expanded.insert(dir.to_path_buf());
        }
        self.rebuild();
        if let Some(at) = self.rows.iter().position(|row| row.path == path) {
            self.selected = at;
        }
    }

    /// 바뀐 폴더만 다시 읽는다. 지운 폴더의 펼침은 걷는다.
    pub fn refresh(&mut self, force: bool) -> bool {
        let mut changed = false;
        self.children.retain(|dir, (stamp, entries)| {
            if !dir.is_dir() {
                changed = true;
                return false;
            }
            let now = mtime(dir);
            if force || now != *stamp {
                *stamp = now;
                *entries = read(dir);
                changed = true;
            }
            true
        });
        self.expanded.retain(|dir| dir.is_dir());
        if changed {
            self.rebuild();
        }
        changed
    }
}

/// 거르기 — 이름에 글이 든 줄과 그 조상만 남긴다.
fn filtered(rows: Vec<Row>, query: &str) -> Vec<Row> {
    let query = query.to_lowercase();
    let mut keep = vec![false; rows.len()];
    keep[0] = true;
    for (i, row) in rows.iter().enumerate() {
        if row.name.to_lowercase().contains(&query) {
            keep[i] = true;
            let mut depth = row.depth;
            for j in (0..i).rev() {
                if rows[j].depth < depth {
                    keep[j] = true;
                    depth = rows[j].depth;
                }
            }
        }
    }
    rows.into_iter().zip(keep).filter_map(|(row, keep)| keep.then_some(row)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kasa-files-tree-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&dir);
        for path in ["src/ui", "docs", ".hidden", ".git/objects"] {
            std::fs::create_dir_all(dir.join(path)).unwrap();
        }
        for file in ["src/main.rs", "src/ui/draw.rs", "Cargo.toml", "b.txt", "A.md", ".env"] {
            std::fs::write(dir.join(file), "x").unwrap();
        }
        dir
    }

    fn names(tree: &Tree) -> Vec<String> {
        tree.rows.iter().map(|row| format!("{}{}", "  ".repeat(row.depth), row.name)).collect()
    }

    #[test]
    fn folders_first_hidden_off_and_git_never_listed() {
        let root = fixture();
        let mut tree = Tree::new(root.clone());
        assert_eq!(&names(&tree)[1..], ["  docs", "  src", "  A.md", "  b.txt", "  Cargo.toml"]);
        tree.show_hidden = true;
        tree.rebuild();
        assert!(names(&tree).contains(&"  .hidden".to_string()));
        assert!(names(&tree).contains(&"  .env".to_string()));
        assert!(!names(&tree).iter().any(|n| n.trim() == ".git"));
    }

    #[test]
    fn expand_collapse_and_parent_keep_the_selection() {
        let root = fixture();
        let mut tree = Tree::new(root.clone());
        tree.set_open(&root.join("src"), true);
        assert_eq!(&names(&tree)[2..5], ["  src", "    ui", "    main.rs"]);
        tree.set_open(&root.join("src/ui"), true);
        tree.selected = tree.rows.iter().position(|r| r.name == "draw.rs").unwrap();
        tree.collapse_or_parent();
        assert_eq!(tree.selected_row().unwrap().name, "ui");
        tree.collapse_or_parent();
        assert!(!names(&tree).contains(&"      draw.rs".to_string()));
        tree.set_open(&root.join("src"), false);
        tree.set_open(&root.join("src"), true);
        assert!(!tree.rows.iter().any(|r| r.name == "draw.rs"), "닫은 하위 폴더는 다시 열 때 접혀 있어야 한다");
    }

    #[test]
    fn reveal_follows_the_shell_inside_and_reroots_outside() {
        let root = fixture();
        let mut tree = Tree::new(root.join("docs"));
        tree.reveal(&root.join("src/ui"));
        assert_eq!(tree.root, root, "뿌리 밖이면 저장소 최상위로 옮긴다");
        assert_eq!(tree.selected_row().unwrap().path, root.join("src/ui"));
        tree.reveal(&root.join("docs"));
        assert_eq!(tree.root, root, "뿌리 안이면 뿌리는 그대로다");
        assert_eq!(tree.selected_row().unwrap().path, root.join("docs"));

        std::fs::create_dir_all(root.join("src/.git")).unwrap();
        assert_eq!(root_for(&root.join("src/ui")), root.join("src"), "가장 가까운 저장소를 쓴다");
        let plain = root.join("docs/plain");
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::remove_dir_all(root.join(".git")).unwrap();
        assert_eq!(root_for(&plain), plain, "저장소가 아니면 그 폴더 자체가 뿌리다");
    }

    #[test]
    fn filter_keeps_matches_with_their_ancestors() {
        let root = fixture();
        let mut tree = Tree::new(root.clone());
        tree.set_open(&root.join("src"), true);
        tree.set_open(&root.join("src/ui"), true);
        tree.filter = "DRAW".into();
        tree.rebuild();
        assert_eq!(&names(&tree)[1..], ["  src", "    ui", "      draw.rs"]);
    }

    #[test]
    fn refresh_reads_only_changed_folders_and_drops_deleted_ones() {
        let root = fixture();
        let mut tree = Tree::new(root.clone());
        tree.set_open(&root.join("docs"), true);
        assert!(!tree.refresh(false));
        std::fs::write(root.join("docs/new.md"), "x").unwrap();
        assert!(tree.refresh(true));
        assert!(tree.rows.iter().any(|r| r.name == "new.md"));
        std::fs::remove_dir_all(root.join("docs")).unwrap();
        tree.refresh(false);
        assert!(!tree.rows.iter().any(|r| r.name == "docs"));
    }
}
