//! 빈 셸의 파일 트리 — 명령 보기에서 친 명령이 없으면 입력 칸 아래 시작 칩 밑에 셸이 선 폴더를 편다(워프처럼).
//! 목록은 원본 기기의 `/term/tree?git=1` 로 한 층씩 받는다(이 기기 칸은 제 HTTP 창구로). 폴더 이름을 누르면 그
//! 셸이 `cd`, 화살표는 그 자리에서 펼치고, 파일은 그 칸의 탭으로 연다. 치수는 `docs/design.md` 「거울 셸 명령 묶음」.

use super::*;

use std::time::{Duration, Instant};

/// 받은 목록이 이만큼 묵으면 다시 받는다 — 빈 셸을 띄워 둔 동안 생긴 파일이 보이게.
const STALE: Duration = Duration::from_secs(5);
const MAX_BYTES: usize = 4 << 20;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Entry {
    pub(crate) name: String,
    pub(crate) dir: bool,
    pub(crate) repo: bool,
    pub(crate) git: Option<char>,
    pub(crate) ignored: bool,
}

#[derive(Default)]
struct Store {
    /// 폴더 → (받은 때, 목록 또는 까닭).
    dirs: HashMap<String, (Instant, Result<Vec<Entry>, String>)>,
    pending: HashSet<String>,
    version: u64,
}

#[derive(Default)]
pub(crate) struct Tree {
    /// 편 뿌리 — 셸이 지금 선 폴더.
    pub(crate) root: Option<String>,
    pub(crate) open: HashSet<String>,
    store: Arc<Mutex<Store>>,
    pub(crate) scroll: f32,
    pub(crate) scroll_max: f32,
    /// 이번 프레임에 그렸나 — 휠이 카드 대신 트리를 굴린다.
    pub(crate) shown: bool,
}

/// 그릴 줄 하나.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Row {
    /// 뿌리의 윗 폴더로 — 값은 그 경로.
    Up(String),
    Item { depth: usize, path: String, entry: Entry },
    /// 펼친 폴더의 목록이 아직 없거나 비었거나 못 받았다.
    Note { depth: usize, text: String },
}

pub(crate) fn parse_listing(v: &serde_json::Value) -> Result<Vec<Entry>, String> {
    if v.get("ok").and_then(|o| o.as_bool()) != Some(true) {
        return Err(v.get("error").and_then(|e| e.as_str()).unwrap_or("목록을 못 받았어요").to_string());
    }
    Ok(v.get("entries")
        .and_then(|e| e.as_array())
        .map(|entries| {
            entries
                .iter()
                .filter_map(|e| {
                    Some(Entry {
                        name: e.get("name")?.as_str()?.to_string(),
                        dir: e.get("is_dir").and_then(|d| d.as_bool()).unwrap_or(false),
                        repo: e.get("is_repo").and_then(|d| d.as_bool()).unwrap_or(false),
                        git: e.get("git").and_then(|g| g.as_str()).and_then(|g| g.chars().next()),
                        ignored: e.get("ignored").and_then(|i| i.as_bool()).unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default())
}

fn join(dir: &str, name: &str) -> String {
    format!("{}/{name}", dir.trim_end_matches('/'))
}

fn parent(dir: &str) -> Option<String> {
    let trimmed = dir.trim_end_matches('/');
    let cut = trimmed.rfind('/')?;
    (!trimmed.is_empty()).then(|| if cut == 0 { "/".to_string() } else { trimmed[..cut].to_string() })
}

impl Tree {
    /// 셸이 다른 폴더로 갔으면 펼침을 접고 처음부터 본다. 새 뿌리 밖의 목록은 버린다.
    pub(crate) fn set_root(&mut self, root: Option<String>) {
        if self.root == root {
            return;
        }
        self.open.clear();
        self.scroll = 0.0;
        if let (Some(root), Ok(mut s)) = (&root, self.store.lock()) {
            s.dirs.retain(|dir, _| dir.starts_with(root.as_str()));
            s.version += 1;
        }
        self.root = root;
    }

    pub(crate) fn version(&self) -> u64 {
        self.store.try_lock().map_or(0, |s| s.version)
    }

    /// 뿌리와 펼친 폴더 가운데 없거나 묵은 목록을 받으러 보낸다.
    pub(crate) fn request(&self, base: &str, proxy: &EventLoopProxy<UserEvent>) {
        let Some(root) = &self.root else { return };
        let Ok(mut s) = self.store.lock() else { return };
        let wanted = std::iter::once(root.clone()).chain(self.open.iter().filter(|d| d.starts_with(root.as_str())).cloned());
        for dir in wanted {
            let fresh = s.dirs.get(&dir).is_some_and(|(at, _)| at.elapsed() < STALE);
            if fresh || !s.pending.insert(dir.clone()) {
                continue;
            }
            let (store, base, proxy) = (Arc::clone(&self.store), base.to_string(), proxy.clone());
            std::thread::spawn(move || {
                let query = format!("/term/tree?path={}&git=1", kasa_mcp::remote::urlencode(&dir));
                let listing = kasa_mcp::remote::remote_get_json_bounded(&base, &query, MAX_BYTES)
                    .map_err(|e| format!("원본에 못 닿았어요 · {e}"))
                    .and_then(|v| parse_listing(&v));
                let Ok(mut s) = store.lock() else { return };
                s.pending.remove(&dir);
                // 받는 사이 뿌리가 바뀌어 버린 폴더도 담아 둔다 — 돌아오면 바로 쓴다.
                s.dirs.insert(dir, (Instant::now(), listing));
                s.version += 1;
                drop(s);
                let _ = proxy.send_event(UserEvent::Redraw);
            });
        }
    }

    pub(crate) fn toggle(&mut self, dir: &str) {
        if !self.open.remove(dir) {
            self.open.insert(dir.to_string());
        }
    }

    /// 펼친 대로 줄을 편다. 뿌리 목록을 아직 못 받았으면 안내 한 줄.
    pub(crate) fn rows(&self) -> Vec<Row> {
        let Some(root) = &self.root else { return Vec::new() };
        let Ok(s) = self.store.try_lock() else { return Vec::new() };
        let mut rows: Vec<Row> = parent(root).into_iter().map(Row::Up).collect();
        self.walk(&s, root, 0, &mut rows);
        rows
    }

    fn walk(&self, s: &Store, dir: &str, depth: usize, rows: &mut Vec<Row>) {
        let note = |text: &str| Row::Note { depth, text: text.to_string() };
        match s.dirs.get(dir) {
            None => rows.push(note("목록을 받는 중…")),
            Some((_, Err(why))) => rows.push(note(why)),
            Some((_, Ok(entries))) if entries.is_empty() => rows.push(note("빈 폴더예요")),
            Some((_, Ok(entries))) => {
                for entry in entries {
                    let path = join(dir, &entry.name);
                    let open = entry.dir && self.open.contains(&path);
                    rows.push(Row::Item { depth, path: path.clone(), entry: entry.clone() });
                    if open {
                        self.walk(s, &path, depth + 1, rows);
                    }
                }
            }
        }
    }
}

/// 친 명령이 없는 셸 — `cd` 만 쳤어도 그렇다. 트리에서 폴더를 눌러 옮겨 다니는 동안 트리가 안 사라지게.
pub(crate) fn fresh(feed: &Feed) -> bool {
    feed.blocks.iter().all(|b| {
        let cmd = b.cmd.trim();
        !b.running
            && (cmd == "cd" || cmd.starts_with("cd "))
            && b.exit == Some(0)
            && b.lines.iter().all(|l| l.iter().all(|s| s.text.trim().is_empty()))
    })
}

impl App {
    /// 트리에서 고른 파일 — 그 칸의 탭으로 열고 그 탭을 앞에 세운다. 다른 기기 칸이면 원본에서 받아 사본을 연다.
    pub(crate) fn open_tree_file(&mut self, id: &str, path: &str) {
        let tab = self.ws.lock().unwrap().active_tab_pid(id);
        let path = match kasa_mcp::remote::remote_info(&tab) {
            Some(info) => match Self::fetch_remote_file(&info.label, &info.base, std::path::Path::new(path)) {
                Ok(local) => local,
                Err(e) => {
                    self.set_toast(format!("{} 의 파일을 못 받았어요 — {e:#}", info.label));
                    return;
                }
            },
            None => std::path::PathBuf::from(path),
        };
        self.open_file(path.clone(), Some(tab), true);
        {
            let mut ws = self.ws.lock().unwrap();
            let found = ws.panes.iter().find_map(|(id, pane)| {
                pane.tabs
                    .iter()
                    .position(|tab| tab.preview_path.as_deref() == Some(path.as_path()))
                    .map(|at| (id.clone(), at))
            });
            let Some((id, at)) = found else { return };
            if let Some(pane) = ws.panes.get_mut(&id) {
                pane.active_tab = at;
                pane.dirty = true;
            }
            ws.active_pane = Some(id);
        }
        self.handoff_ime_to_active_surface();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kasa_bridge::screen::Color;

    fn entry(name: &str, dir: bool) -> Entry {
        Entry { name: name.into(), dir, repo: false, git: None, ignored: false }
    }

    #[test]
    fn rows_follow_what_is_open_and_say_what_is_missing() {
        let mut tree = Tree::default();
        tree.set_root(Some("/r".into()));
        assert_eq!(tree.rows(), vec![Row::Up("/".into()), Row::Note { depth: 0, text: "목록을 받는 중…".into() }]);
        {
            let mut s = tree.store.lock().unwrap();
            s.dirs.insert("/r".into(), (Instant::now(), Ok(vec![entry("app", true), entry("a.md", false)])));
            s.dirs.insert("/r/app/src".into(), (Instant::now(), Err("폴더를 못 읽어요".into())));
        }
        tree.toggle("/r/app");
        let rows = tree.rows();
        assert_eq!(rows[1], Row::Item { depth: 0, path: "/r/app".into(), entry: entry("app", true) });
        assert_eq!(rows[2], Row::Note { depth: 1, text: "목록을 받는 중…".into() });
        assert_eq!(rows[3], Row::Item { depth: 0, path: "/r/a.md".into(), entry: entry("a.md", false) });
        tree.store.lock().unwrap().dirs.insert("/r/app".into(), (Instant::now(), Ok(vec![entry("src", true)])));
        tree.toggle("/r/app/src");
        assert_eq!(tree.rows()[3], Row::Note { depth: 2, text: "폴더를 못 읽어요".into() });
        // 셸이 옮겨 가면 펼침은 접고 새 뿌리 밖 목록은 버린다.
        tree.set_root(Some("/r/app".into()));
        assert!(tree.open.is_empty());
        assert_eq!(tree.rows()[0], Row::Up("/r".into()));
        assert!(!tree.store.lock().unwrap().dirs.contains_key("/r"));
    }

    #[test]
    fn listing_reads_marks_and_failures() {
        let v = serde_json::json!({"ok": true, "entries": [
            {"name": "src", "is_dir": true, "is_repo": false, "git": "•"},
            {"name": "target", "is_dir": true, "ignored": true},
            {"name": "a.rs", "is_dir": false, "git": "M"},
        ]});
        let got = parse_listing(&v).unwrap();
        assert_eq!(got[0].git, Some('•'));
        assert!(got[1].ignored && got[1].git.is_none());
        assert_eq!(got[2].git, Some('M'));
        assert_eq!(parse_listing(&serde_json::json!({"ok": false, "error": "폴더를 못 읽어요"})), Err("폴더를 못 읽어요".into()));
        assert_eq!(parent("/a/b"), Some("/a".into()));
        assert_eq!(parent("/a"), Some("/".into()));
        assert_eq!(parent("/"), None);
    }

    #[test]
    fn a_shell_that_only_moved_around_is_still_fresh() {
        let block = |cmd: &str, exit: i32, out: &str| Block {
            cmd: cmd.into(),
            exit: Some(exit),
            lines: if out.is_empty() { Vec::new() } else { vec![vec![Span { text: out.into(), fg: Color::Default, bg: Color::Default, flags: 0 }]] },
            ..Default::default()
        };
        let mut feed = Feed::default();
        assert!(fresh(&feed));
        feed.blocks.push(block("cd -- '/r/app'", 0, ""));
        assert!(fresh(&feed));
        feed.blocks.push(block("cd nowhere", 1, "cd: no such file or directory: nowhere"));
        assert!(!fresh(&feed), "실패한 cd 는 사람이 봐야 한다");
        feed.blocks.pop();
        feed.blocks.push(block("ls", 0, "a"));
        assert!(!fresh(&feed));
    }
}
