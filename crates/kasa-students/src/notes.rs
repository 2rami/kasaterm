//! 학생 쪽지 — 폰 허브 종 목록. 사람에게 간 알림 두 가지의 기록이다(2026-11-13 「진짜 완료알림이나 진짜 내가
//! 선택해야하는거만」): 고를 것(승인·질문·막힘·한도·팅김)과 완료. 앱이 폰·맥 알림을 내는 바로 그 자리
//! (`chrome.rs ring_phone`)에서 한 줄 남겨 알림과 목록이 어긋나지 않는다. 다른 입구는 없다 — 나쵸가 넣던
//! `POST /term/notes` 는 나쵸 오케스트레이션과 함께 끊겨 걷었다.
//!
//! 폰 허브의 종 목록이 `GET /term/notes` 로 읽는다. 저장은 `~/.config/kasaterm/notes.json` — 앱을 껐다 켜도
//! 남고, 최근 200건만 둔다. 검증용 앱은 `KASATERM_NOTES_FILE` 로 다른 파일을 가리켜 사람 것을 안 건드린다.

use serde::{Deserialize, Serialize};
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
    notes.retain(|n| KINDS.contains(&n.kind.as_str()));
    notes.sort_by(|a, b| b.when.cmp(&a.when).then(b.id.cmp(&a.id)));
    notes
}

fn add_to(path: &Path, pane: &str, character: &str, kind: &str, summary: &str) -> Option<Note> {
    let summary = summary.trim();
    if summary.is_empty() || pane.trim().is_empty() || !KINDS.contains(&kind) {
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
    path().map(|p| list_from(&p)).unwrap_or_default()
}

/// 사람에게 간 알림 한 건을 남긴다 — [`KINDS`] 밖의 종류·빈 본문은 안 받는다.
pub fn add(pane: &str, character: &str, kind: &str, summary: &str) -> Option<Note> {
    add_to(&path()?, pane, character, kind, summary)
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
        add_to(p, pane, "유우카", "done", summary)
    }

    #[test]
    fn only_the_two_alert_kinds_are_kept_or_listed() {
        let p = tmp("kinds");
        assert!(add_to(&p, "%3", "유우카", "link", "페이지").is_none(), "알림 두 가지 밖은 안 받는다");
        for kind in KINDS {
            assert!(add_to(&p, "%3", "유우카", kind, "본문").is_some(), "{kind}");
        }
        let mut body = load(&p);
        body.notes.push(Note { id: 99, pane: "%1".into(), character: String::new(), kind: "done_ok".into(),
            summary: "옛 나쵸 쪽지".into(), when: 1, read: false });
        save(&p, &body).unwrap();
        assert_eq!(list_from(&p).len(), KINDS.len(), "옛 종류는 파일에 남아도 목록엔 안 선다");
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
