//! 학생 쪽지는 세션이 보고한 완료·막힘과 아직 답이 필요한 승인·질문을 보여 준다.
//!
//! 폰 허브의 종 목록과 다른 기기의 하단바 「학생 쪽지」가 `GET /term/notes` 로, 이 기기의 하단바가 [`list`] 로 바로 읽는다.
//! 쪽지마다 정본은 이 파일 하나라 어느 기기에서 읽든 여기에 적힌다. 저장은
//! `~/.config/kasaterm/notes.json` — 앱을 껐다 켜도 남고, 최근 200건만 둔다. 검증용 앱은 `KASATERM_NOTES_FILE` 로
//! 다른 파일을 가리켜 사람 것을 안 건드린다.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// 쪽지 종류 — 폰 알림 종류(`waiting:permission`·`waiting:question`·`waiting:blocked`·`done`)의 뒷말과 같다.
pub const KINDS: [&str; 4] = ["permission", "question", "blocked", "done"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Note {
    pub id: u64,
    pub pane: String,
    #[serde(default)]
    pub character: String,
    #[serde(default)]
    pub kind: String,
    /// 알림 본문 — 학생이 선생님께 하는 말.
    pub summary: String,
    pub when: u64,
    #[serde(default)]
    pub read: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

impl Note {
    pub fn is_student_alert(&self) -> bool {
        self.pane != "disk" && KINDS.contains(&self.kind.as_str())
    }
}

#[derive(PartialEq, Eq)]
pub struct Choices {
    pub permission: bool,
    pub question: bool,
    pub question_ids: HashSet<String>,
    pub since: u64,
}

static CHOICES: Mutex<Option<HashMap<String, Choices>>> = Mutex::new(None);

pub fn publish_choices(choices: HashMap<String, Choices>) -> bool {
    let mut current = CHOICES.lock().unwrap_or_else(|e| e.into_inner());
    if current.as_ref() == Some(&choices) {
        return false;
    }
    *current = Some(choices);
    true
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct FileBody {
    #[serde(default)]
    next_id: u64,
    #[serde(default)]
    notes: Vec<Note>,
}

const KEEP: usize = 200;

/// 파일 하나를 여러 요청이 동시에 고치지 않게 — 읽고 쓰는 사이에 남이 끼면 한쪽이 사라진다.
static LOCK: Mutex<()> = Mutex::new(());

fn path() -> Option<PathBuf> {
    std::env::var_os("KASATERM_NOTES_FILE")
        .map(PathBuf::from)
        .or_else(|| kasa_socket::home_dir().map(|h| h.join(".config/kasaterm/notes.json")))
}

fn load(path: &Path) -> FileBody {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save(path: &Path, body: &FileBody) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(body).map_err(|e| std::io::Error::other(e.to_string()))?;
    std::fs::write(path, text)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 두 가지 알림만 — 옛 나쵸 쪽지·페이지 쪽지가 파일에 남아 있어도 목록엔 안 선다.
fn list_from(path: &Path) -> Vec<Note> {
    let mut notes = load(path).notes;
    // Older builds filed a system disk warning under student blocks.
    notes.retain(Note::is_student_alert);
    notes.sort_by(|a, b| b.when.cmp(&a.when).then(b.id.cmp(&a.id)));
    notes
}

fn visible(notes: Vec<Note>, choices: Option<&HashMap<String, Choices>>) -> Vec<Note> {
    let mut seen = HashSet::new();
    notes.into_iter().filter(|note| {
        if matches!(note.kind.as_str(), "done" | "blocked") {
            return true;
        }
        let Some(active) = choices.and_then(|panes| panes.get(&note.pane)) else { return false };
        let matches = match (note.kind.as_str(), note.request_id.as_deref()) {
            ("permission", None) => active.permission && note.when >= active.since,
            ("question", Some(id)) => active.question_ids.contains(id),
            ("question", None) => active.question && active.question_ids.is_empty() && note.when >= active.since,
            _ => false,
        };
        // Even a read prompt is the latest copy and supersedes older duplicates.
        matches && seen.insert((note.pane.clone(), note.kind.clone(), note.request_id.clone()))
    }).collect()
}

fn add_to(path: &Path, pane: &str, character: &str, kind: &str, summary: &str, request_id: Option<&str>) -> Option<Note> {
    let summary = summary.trim();
    if summary.is_empty() || pane.trim().is_empty() || pane.trim() == "disk" || !KINDS.contains(&kind) {
        return None;
    }
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut body = load(path);
    body.next_id = body.next_id.max(1);
    let note = Note {
        id: body.next_id,
        pane: pane.trim().to_string(),
        character: character.trim().to_string(),
        kind: kind.to_string(),
        summary: summary.to_string(),
        when: now_secs(),
        read: false,
        request_id: request_id.map(str::to_owned),
    };
    body.next_id += 1;
    body.notes.push(note.clone());
    if body.notes.len() > KEEP {
        let drop = body.notes.len() - KEEP;
        body.notes.drain(..drop);
    }
    save(path, &body).ok()?;
    Some(note)
}

fn mark_read_in(path: &Path, ids: &[u64], all: bool) -> usize {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut body = load(path);
    let mut n = 0;
    for note in &mut body.notes {
        if !note.read && (all || ids.contains(&note.id)) {
            note.read = true;
            n += 1;
        }
    }
    if n > 0 && save(path, &body).is_err() {
        return 0;
    }
    n
}

/// 지우기 — `all` 이면 전부. 돌려주는 값은 지운 개수.
fn remove_in(path: &Path, ids: &[u64], all: bool) -> usize {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut body = load(path);
    let before = body.notes.len();
    body.notes.retain(|n| !(all || ids.contains(&n.id)));
    let n = before - body.notes.len();
    if n > 0 && save(path, &body).is_err() {
        return 0;
    }
    n
}

/// 최근 것부터.
pub fn list() -> Vec<Note> {
    let Some(path) = path() else { return Vec::new() };
    let notes = list_from(&path);
    let choices = CHOICES.lock().unwrap_or_else(|e| e.into_inner());
    visible(notes, choices.as_ref())
}

/// 사람에게 간 알림 한 건을 남긴다 — [`KINDS`] 밖의 종류·빈 본문은 안 받는다.
pub fn add(pane: &str, character: &str, kind: &str, summary: &str, request_id: Option<&str>) -> Option<Note> {
    add_to(&path()?, pane, character, kind, summary, request_id)
}

/// 읽음 표시 — `all` 이면 전부. 돌려주는 값은 새로 읽음이 된 개수.
pub fn mark_read(ids: &[u64], all: bool) -> usize {
    path().map(|p| mark_read_in(&p, ids, all)).unwrap_or(0)
}

/// 폰이 옆으로 밀어 지우거나 「모두 읽음」으로 비운 것 — 돌려주는 값은 지운 개수.
pub fn remove(ids: &[u64], all: bool) -> usize {
    path().map(|p| remove_in(&p, ids, all)).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kasaterm-notes-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("notes.json")
    }

    fn add(p: &Path, pane: &str, summary: &str) -> Option<Note> {
        add_to(p, pane, "유우카", "done", summary, None)
    }

    #[test]
    fn only_the_two_alert_kinds_are_kept_or_listed() {
        let p = tmp("kinds");
        assert!(add_to(&p, "%3", "유우카", "link", "페이지", None).is_none(), "알림 두 가지 밖은 안 받는다");
        for kind in KINDS {
            assert!(add_to(&p, "%3", "유우카", kind, "본문", None).is_some(), "{kind}");
        }
        let mut body = load(&p);
        body.notes.push(Note { id: 99, pane: "%1".into(), character: String::new(), kind: "done_ok".into(),
            summary: "옛 나쵸 쪽지".into(), when: 1, read: false, request_id: None });
        body.notes.push(Note { id: 100, pane: "disk".into(), character: String::new(), kind: "blocked".into(),
            summary: "디스크 경고".into(), when: 1, read: false, request_id: None });
        save(&p, &body).unwrap();
        assert_eq!(list_from(&p).len(), KINDS.len(), "옛 쪽지와 디스크 경고는 목록에 표시하지 않는다");
    }

    #[test]
    fn the_bell_keeps_reports_and_only_current_questions() {
        let p = tmp("active");
        for (kind, request) in [
            ("done", None),
            ("blocked", None),
            ("permission", None),
            ("question", None),
            ("question", Some("answered")),
            ("question", Some("current")),
            ("question", Some("another")),
            ("question", Some("current")),
        ] {
            add_to(&p, "%3", "", kind, "본문", request).unwrap();
        }
        mark_read_in(&p, &[8], false);
        let active = HashMap::from([("%3".to_string(), Choices {
            permission: false,
            question: true,
            question_ids: HashSet::from(["current".into(), "another".into()]),
            since: 0,
        })]);
        let ids = |items: Vec<Note>| items.into_iter().map(|note| note.id).collect::<Vec<_>>();
        assert_eq!(ids(visible(list_from(&p), Some(&active))), [8, 7, 2, 1]);
        assert_eq!(ids(visible(list_from(&p), None)), [2, 1]);
    }

    #[test]
    fn old_unkeyed_notes_stay_hidden_during_a_new_wait() {
        let p = tmp("unkeyed");
        let note = add_to(&p, "%4", "", "permission", "본문", None).unwrap();
        let mut active = HashMap::from([("%4".to_string(), Choices {
            permission: true, question: false, question_ids: HashSet::new(), since: note.when,
        })]);
        assert_eq!(visible(list_from(&p), Some(&active)).len(), 1);
        active.get_mut("%4").unwrap().since += 1;
        assert!(visible(list_from(&p), Some(&active)).is_empty());
    }

    #[test]
    fn add_lists_newest_first_and_marks_read() {
        let p = tmp("basic");
        assert!(add(&p, "%3", "").is_none(), "빈 본문은 안 받는다");
        let a = add(&p, "%3", "폰 상태줄 한 줄로 맞췄어요").unwrap();
        let b = add(&p, "%7", "종 아이콘 고쳤어요").unwrap();
        assert_eq!((a.id, b.id), (1, 2));
        let l = list_from(&p);
        assert_eq!(l.iter().map(|n| n.id).collect::<Vec<_>>(), vec![2, 1], "같은 초면 나중 것이 위");
        assert!(l.iter().all(|n| !n.read));
        assert_eq!(mark_read_in(&p, &[1], false), 1);
        assert_eq!(mark_read_in(&p, &[1], false), 0, "이미 읽은 건 다시 안 센다");
        assert_eq!(mark_read_in(&p, &[], true), 1);
        assert!(list_from(&p).iter().all(|n| n.read));
    }

    #[test]
    fn keeps_only_the_latest_two_hundred() {
        let p = tmp("cap");
        for _ in 0..(KEEP as u64 + 5) {
            add(&p, "%1", "x").unwrap();
        }
        let l = list_from(&p);
        assert_eq!(l.len(), KEEP);
        assert_eq!(l.first().unwrap().id, KEEP as u64 + 5, "id 는 계속 는다");
        assert_eq!(l.last().unwrap().id, 6, "오래된 것부터 버린다");
    }
}
