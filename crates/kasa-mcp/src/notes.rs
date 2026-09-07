//! 학생 쪽지 — 나쵸가 학생 알림마다 남기는 「시킨 것 → 한 것」 한 줄(2026-09-08 지시
//! 「알림기능에 나쵸네코가 내가 뭘 시켰고 어떻게 했는지 한줄요약하게 하자」).
//!
//! 나쵸는 `POST /term/notes` 로 넣고, 폰 허브의 종 목록이 `GET /term/notes` 로 읽는다.
//! 저장은 `~/.config/kasaterm/notes.json` — 앱을 껐다 켜도 남고, 최근 200건만 둔다.
//! 검증용 앱은 `KASATERM_NOTES_FILE` 로 다른 파일을 가리켜 사람 것을 안 건드린다.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Note {
    pub id: u64,
    pub pane: String,
    #[serde(default)]
    pub character: String,
    #[serde(default)]
    pub kind: String,
    pub summary: String,
    #[serde(default)]
    pub asked: String,
    #[serde(default)]
    pub did: String,
    pub when: u64,
    #[serde(default)]
    pub read: bool,
    /// 사진이 딸렸는가 — `/term/notes/<id>.png`.
    #[serde(default)]
    pub image: bool,
}

/// 나쵸가 보내는 본문 — id·읽음은 서버가 붙인다.
#[derive(Debug, Clone, Deserialize)]
pub struct NoteInput {
    pub pane: String,
    #[serde(default)]
    pub character: String,
    #[serde(default)]
    pub kind: String,
    pub summary: String,
    #[serde(default)]
    pub asked: String,
    #[serde(default)]
    pub did: String,
    #[serde(default)]
    pub when: Option<u64>,
    /// pane 사진 PNG 를 base64 로. 나쵸가 DM 에 붙이는 그 사진.
    #[serde(default)]
    pub image: Option<String>,
}

/// 사진 상한 — 폰 목록의 썸네일이지 원본 보관이 아니다.
const IMAGE_MAX: usize = 3 * 1024 * 1024;

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

fn list_from(path: &Path) -> Vec<Note> {
    let mut notes = load(path).notes;
    notes.sort_by(|a, b| b.when.cmp(&a.when).then(b.id.cmp(&a.id)));
    notes
}

fn image_path(path: &Path, id: u64) -> PathBuf {
    path.with_extension("").join(format!("{id}.png"))
}

fn decode_image(b64: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    let raw = b64.trim();
    let raw = raw.rsplit_once(',').map(|(_, d)| d).unwrap_or(raw);
    let bytes = base64::engine::general_purpose::STANDARD.decode(raw).ok()?;
    (bytes.len() <= IMAGE_MAX && bytes.starts_with(b"\x89PNG")).then_some(bytes)
}

fn add_to(path: &Path, input: NoteInput) -> Option<Note> {
    let summary = input.summary.trim();
    if summary.is_empty() || input.pane.trim().is_empty() {
        return None;
    }
    let image = input.image.as_deref().and_then(decode_image);
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut body = load(path);
    body.next_id = body.next_id.max(1);
    let note = Note {
        id: body.next_id,
        pane: input.pane.trim().to_string(),
        character: input.character.trim().to_string(),
        kind: input.kind.trim().to_string(),
        summary: summary.to_string(),
        asked: input.asked.trim().to_string(),
        did: input.did.trim().to_string(),
        when: input.when.unwrap_or_else(now_secs),
        read: false,
        image: image.is_some(),
    };
    body.next_id += 1;
    body.notes.push(note.clone());
    if body.notes.len() > KEEP {
        let drop = body.notes.len() - KEEP;
        for old in body.notes.drain(..drop) {
            if old.image {
                let _ = std::fs::remove_file(image_path(path, old.id));
            }
        }
    }
    if let Some(bytes) = image {
        let ip = image_path(path, note.id);
        if let Some(dir) = ip.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(ip, bytes);
    }
    save(path, &body).ok()?;
    Some(note)
}

fn image_from(path: &Path, id: u64) -> Option<Vec<u8>> {
    std::fs::read(image_path(path, id)).ok()
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

/// 최근 것부터.
pub fn list() -> Vec<Note> {
    path().map(|p| list_from(&p)).unwrap_or_default()
}

pub fn add(input: NoteInput) -> Option<Note> {
    add_to(&path()?, input)
}

/// 읽음 표시 — `all` 이면 전부. 돌려주는 값은 새로 읽음이 된 개수.
pub fn mark_read(ids: &[u64], all: bool) -> usize {
    path().map(|p| mark_read_in(&p, ids, all)).unwrap_or(0)
}

pub fn image_bytes(id: u64) -> Option<Vec<u8>> {
    image_from(&path()?, id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kasaterm-notes-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("notes.json")
    }

    fn input(pane: &str, summary: &str, when: u64) -> NoteInput {
        NoteInput {
            pane: pane.into(),
            character: "유우카".into(),
            kind: "done_ok".into(),
            summary: summary.into(),
            asked: String::new(),
            did: String::new(),
            when: Some(when),
            image: None,
        }
    }

    #[test]
    fn image_is_stored_beside_the_file_and_dropped_with_the_note() {
        use base64::Engine;
        let p = tmp("image");
        let png = b"\x89PNG\r\n\x1a\nfake".to_vec();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
        let mut i = input("%3", "사진 있음", 5);
        i.image = Some(format!("data:image/png;base64,{b64}"));
        let n = add_to(&p, i).unwrap();
        assert!(n.image);
        assert_eq!(image_from(&p, n.id).unwrap(), png);
        let mut bad = input("%3", "png 아님", 6);
        bad.image = Some(base64::engine::general_purpose::STANDARD.encode(b"hello"));
        assert!(!add_to(&p, bad).unwrap().image, "PNG 가 아니면 사진 없이 받는다");
        for k in 0..KEEP as u64 {
            add_to(&p, input("%1", "x", 10 + k)).unwrap();
        }
        assert!(image_from(&p, n.id).is_none(), "쪽지가 밀려나면 사진도 같이 지운다");
    }

    #[test]
    fn add_lists_newest_first_and_marks_read() {
        let p = tmp("basic");
        assert!(add_to(&p, input("%3", "", 1)).is_none(), "빈 요약은 안 받는다");
        let a = add_to(&p, input("%3", "폰 상태줄 한 줄로 → 폴더부터 빼서 맞춤", 10)).unwrap();
        let b = add_to(&p, input("%7", "종 아이콘 → 눌러도 반응 없음 보고", 20)).unwrap();
        assert_eq!((a.id, b.id), (1, 2));
        let l = list_from(&p);
        assert_eq!(l.iter().map(|n| n.id).collect::<Vec<_>>(), vec![2, 1]);
        assert!(l.iter().all(|n| !n.read));
        assert_eq!(mark_read_in(&p, &[1], false), 1);
        assert_eq!(mark_read_in(&p, &[1], false), 0, "이미 읽은 건 다시 안 센다");
        assert_eq!(mark_read_in(&p, &[], true), 1);
        assert!(list_from(&p).iter().all(|n| n.read));
    }

    #[test]
    fn keeps_only_the_latest_two_hundred() {
        let p = tmp("cap");
        for i in 0..(KEEP as u64 + 5) {
            add_to(&p, input("%1", "x", i)).unwrap();
        }
        let l = list_from(&p);
        assert_eq!(l.len(), KEEP);
        assert_eq!(l.first().unwrap().id, KEEP as u64 + 5, "id 는 계속 는다");
        assert_eq!(l.last().unwrap().id, 6, "오래된 것부터 버린다");
    }
}
