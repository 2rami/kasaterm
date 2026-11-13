//! 알림 얼굴 보관 — 데스크톱이 올린 학생 얼굴을 계정별로 내용 해시로 두고, 폰 알림 확장이 받아 갈 주소를 준다.
//!
//! - 알림 확장은 앱의 열쇠고리를 못 봐 인증 없이 받아야 한다. 그래서 받는 주소는 그림마다 관문이 뽑은 128비트 난수
//!   (`/relay/push/face/<id>.png`)이고 주소가 곧 자격이다(설치 링크와 같다). 내용 해시를 주소로 쓰지 않는 까닭: 번들
//!   학생 얼굴처럼 누구나 가진 그림은 해시를 계산해 넣어 보는 것만으로 「어느 계정이 이 학생을 쓴다」를 떠볼 수 있다.
//! - 올리기(`PUT /relay/push/face/<sha256>`)는 그 계정 데스크톱만. 알림은 해시만 싣고, 관문이 모르는 해시면 409
//!   `face_unknown` 으로 그림을 청한다 — 같은 그림을 알림마다 다시 받지 않는다.
//! - 계정마다 64장·2MB, 30일 안 쓰인 얼굴은 지운다. 넘치면 가장 오래 안 쓰인 것부터.

use super::*;
use kasa_device::push::face as rule;
use serde::{Deserialize, Serialize};

const PER_ACCOUNT: usize = 64;
const ACCOUNT_BYTES: usize = 2 * 1024 * 1024;
const KEEP_SECS: u64 = 30 * 24 * 3600;
/// 쓰인 때를 이보다 자주 적지 않는다 — 알림마다 장부를 다시 쓰지 않게.
const TOUCH_SECS: u64 = 24 * 3600;

#[derive(Default, Serialize, Deserialize)]
struct Book {
    #[serde(default)]
    accounts: HashMap<String, Vec<Entry>>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    hash: String,
    id: String,
    bytes: usize,
    used: u64,
}

pub(in crate::gateway) struct Faces {
    index: Option<PathBuf>,
    dir: Option<PathBuf>,
    book: Mutex<Book>,
}

pub(in crate::gateway) enum Known {
    /// 상태 폴더가 없는 관문 — 얼굴 없이 보낸다.
    Off,
    Unknown,
    Id(String),
}

fn is_id(text: &str) -> bool {
    text.len() == 32 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn new_id() -> String {
    use ring::rand::SecureRandom as _;
    let mut bytes = [0u8; 16];
    let _ = ring::rand::SystemRandom::new().fill(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 기한이 지난 것을 빼고, 개수·용량 상한을 넘으면 가장 오래 안 쓰인 것부터 뺀다. 뺀 것들의 id.
fn prune(entries: &mut Vec<Entry>, now: u64) -> Vec<String> {
    let mut gone = Vec::new();
    entries.retain(|e| {
        let keep = e.used.saturating_add(KEEP_SECS) > now;
        if !keep {
            gone.push(e.id.clone());
        }
        keep
    });
    entries.sort_by_key(|e| std::cmp::Reverse(e.used));
    let mut total = 0;
    let mut kept = 0;
    for e in entries.iter() {
        if kept < PER_ACCOUNT && total + e.bytes <= ACCOUNT_BYTES {
            total += e.bytes;
            kept += 1;
        } else {
            break;
        }
    }
    gone.extend(entries.drain(kept..).map(|e| e.id));
    gone
}

impl Faces {
    pub(in crate::gateway) fn open(state_path: Option<&std::path::Path>) -> Self {
        let index = state_path.map(|p| p.with_file_name("relay-push-faces.json"));
        let book: Book = index
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        let faces = Self { index, dir: state_path.map(|p| p.with_file_name("relay-push-faces")), book: Mutex::new(book) };
        faces.sweep(now_secs());
        faces
    }

    fn file(&self, id: &str) -> Option<PathBuf> {
        Some(self.dir.as_ref()?.join(format!("{id}.png")))
    }

    fn save(&self, book: &Book) -> Result<(), &'static str> {
        let path = self.index.as_ref().ok_or("storage_unavailable")?;
        let body = serde_json::to_string(book).map_err(|_| "storage_unavailable")?;
        kasa_identity::relay_auth::write_private(path, &body).map_err(|_| "storage_unavailable")
    }

    fn forget(&self, ids: &[String]) {
        for id in ids {
            if let Some(file) = self.file(id) {
                let _ = std::fs::remove_file(file);
            }
        }
    }

    fn sweep(&self, now: u64) {
        let Ok(mut book) = self.book.lock() else { return };
        let mut gone = Vec::new();
        for entries in book.accounts.values_mut() {
            gone.extend(prune(entries, now));
        }
        book.accounts.retain(|_, entries| !entries.is_empty());
        if !gone.is_empty() && self.save(&book).is_ok() {
            self.forget(&gone);
        }
    }

    /// 알림이 실은 해시를 이 계정이 맡겼나. 맡긴 것이면 쓰인 때를 고친다(하루에 한 번만 적는다).
    pub(in crate::gateway) fn known(&self, account: &str, hash: &str, now: u64) -> Known {
        if self.index.is_none() || self.dir.is_none() {
            return Known::Off;
        }
        let Ok(mut book) = self.book.lock() else { return Known::Off };
        let Some(entries) = book.accounts.get_mut(account) else { return Known::Unknown };
        let Some(entry) = entries.iter_mut().find(|e| e.hash == hash && e.used.saturating_add(KEEP_SECS) > now) else {
            return Known::Unknown;
        };
        let id = entry.id.clone();
        if now.saturating_sub(entry.used) >= TOUCH_SECS {
            entry.used = now;
            let _ = self.save(&book);
        }
        Known::Id(id)
    }

    /// 그림을 맡는다. 같은 해시를 이미 맡았으면 쓰인 때만 고친다. `png` 는 부른 쪽이 해시·형식을 확인했다.
    pub(in crate::gateway) fn keep(&self, account: &str, hash: &str, png: &[u8], now: u64) -> Result<(), &'static str> {
        let dir = self.dir.as_ref().ok_or("storage_unavailable")?;
        let mut book = self.book.lock().map_err(|_| "storage_unavailable")?;
        let entries = book.accounts.entry(account.to_string()).or_default();
        if let Some(entry) = entries.iter_mut().find(|e| e.hash == hash) {
            entry.used = now;
            return self.save(&book);
        }
        std::fs::create_dir_all(dir).map_err(|_| "storage_unavailable")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
        let id = new_id();
        let file = self.file(&id).ok_or("storage_unavailable")?;
        profile::write_private_bytes(&file, png).map_err(|_| "storage_unavailable")?;
        entries.push(Entry { hash: hash.to_string(), id: id.clone(), bytes: png.len(), used: now });
        let gone = prune(entries, now);
        if let Err(code) = self.save(&book) {
            let _ = std::fs::remove_file(&file);
            return Err(code);
        }
        self.forget(&gone);
        Ok(())
    }

    /// 받는 주소의 id → 그림. 기한이 지났거나 상한에 밀려난 것은 없다.
    pub(in crate::gateway) fn read(&self, id: &str, now: u64) -> Option<Vec<u8>> {
        if !is_id(id) {
            return None;
        }
        let live = self
            .book
            .lock()
            .ok()?
            .accounts
            .values()
            .flatten()
            .any(|e| e.id == id && e.used.saturating_add(KEEP_SECS) > now);
        live.then(|| std::fs::read(self.file(id)?).ok()).flatten()
    }
}

/// `PUT /relay/push/face/<sha256>` — 데스크톱이 학생 얼굴 PNG 를 맡긴다. 몸통의 해시가 주소와 같아야 받는다.
pub(super) async fn upload(
    State(gate): State<Gate>,
    AxPath(hash): AxPath<String>,
    req: axum::extract::Request,
) -> axum::response::Response {
    let Some((device, rec)) = gate.device_of(req.headers()) else {
        return json_err(StatusCode::UNAUTHORIZED, "unauthorized");
    };
    if rec.kind != "desktop" {
        return json_err(StatusCode::FORBIDDEN, "desktop_only");
    }
    if !rule::is_hash(&hash) {
        return json_err(StatusCode::BAD_REQUEST, "bad_face");
    }
    let bytes = match tokio::time::timeout(Duration::from_secs(10), axum::body::to_bytes(req.into_body(), rule::MAX_BYTES)).await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(_)) => return json_err(StatusCode::PAYLOAD_TOO_LARGE, "face_too_large"),
        Err(_) => return json_err(StatusCode::REQUEST_TIMEOUT, "request_timeout"),
    };
    if rule::hash(&bytes) != hash {
        return json_err(StatusCode::BAD_REQUEST, "face_mismatch");
    }
    if !rule::acceptable(&bytes) {
        return json_err(StatusCode::BAD_REQUEST, "bad_face");
    }
    // 올리기는 알림과 따로 센다 — 처음 보는 학생이 한꺼번에 끝나도 알림 몫을 깎지 않게.
    if !gate.push_alerts.allow(&format!("face:{device}"), Instant::now()) {
        return json_err(StatusCode::TOO_MANY_REQUESTS, "rate_limited");
    }
    match gate.push_faces.keep(&rec.account, &hash, &bytes, now_secs()) {
        Ok(()) => axum::Json(serde_json::json!({"ok": true})).into_response(),
        Err(code) => json_err(StatusCode::SERVICE_UNAVAILABLE, code),
    }
}

/// `GET /relay/push/face/<id>.png` — 인증 없이. 주소가 자격이라 남의 캐시·Referer·색인에 남기지 않는다.
pub(super) async fn fetch(State(gate): State<Gate>, AxPath(name): AxPath<String>) -> axum::response::Response {
    let png = name.strip_suffix(".png").and_then(|id| gate.push_faces.read(id, now_secs()));
    let headers = |cache: &'static str, kind: &'static str| {
        [
            (header::CONTENT_TYPE, kind),
            (header::CACHE_CONTROL, cache),
            (header::REFERRER_POLICY, "no-referrer"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (axum::http::HeaderName::from_static("x-robots-tag"), "noindex, nofollow"),
        ]
    };
    match png {
        // 같은 id 는 같은 그림이다 — 폰은 오래 들고 있어도 된다.
        Some(png) => (StatusCode::OK, headers("private, max-age=604800, immutable", "image/png"), png).into_response(),
        None => (StatusCode::NOT_FOUND, headers("no-store", "text/plain"), Vec::new()).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rig() -> (Faces, PathBuf) {
        let dir = std::env::temp_dir().join(format!("kasa-push-face-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        (Faces::open(Some(&dir.join("relay-state.json"))), dir)
    }

    fn id_of(faces: &Faces, account: &str, hash: &str, now: u64) -> String {
        match faces.known(account, hash, now) {
            Known::Id(id) => id,
            _ => panic!("{hash} 를 모름"),
        }
    }

    #[test]
    fn a_kept_face_is_known_to_its_account_only_and_read_by_a_random_id() {
        let (faces, dir) = rig();
        let (hash, t) = ("a".repeat(64), now_secs());
        assert!(matches!(faces.known("geno", &hash, t), Known::Unknown));
        faces.keep("geno", &hash, b"png-1", t).unwrap();
        let id = id_of(&faces, "geno", &hash, t + 1);
        assert!(is_id(&id) && !hash.starts_with(&id), "주소는 내용 해시가 아니다");
        assert!(matches!(faces.known("other", &hash, t + 1), Known::Unknown));
        assert_eq!(faces.read(&id, t + 1).as_deref(), Some(b"png-1".as_slice()));
        assert_eq!(faces.read(&"0".repeat(32), t + 1), None);
        assert_eq!(faces.read("../relay-state", t + 1), None);

        faces.keep("geno", &hash, b"png-1", t + 2).unwrap();
        assert_eq!(id_of(&faces, "geno", &hash, t + 2), id, "같은 해시는 다시 안 받는다");

        let reopened = Faces::open(Some(&dir.join("relay-state.json")));
        assert_eq!(id_of(&reopened, "geno", &hash, t + 3), id, "재시작해도 남는다");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn faces_expire_and_the_oldest_leave_first() {
        let (faces, dir) = rig();
        faces.keep("geno", &"e".repeat(64), b"old", 0).unwrap();
        let old = id_of(&faces, "geno", &"e".repeat(64), 1);
        assert!(matches!(faces.known("geno", &"e".repeat(64), KEEP_SECS + 1), Known::Unknown));
        assert_eq!(faces.read(&old, KEEP_SECS + 1), None);

        let hash = |n: usize| format!("{n:064x}");
        let now = 100;
        for n in 0..PER_ACCOUNT + 3 {
            faces.keep("geno", &hash(n), b"x", now + n as u64).unwrap();
        }
        assert!(matches!(faces.known("geno", &hash(0), now + 200), Known::Unknown));
        assert!(matches!(faces.known("geno", &hash(2), now + 200), Known::Unknown));
        assert!(matches!(faces.known("geno", &hash(3), now + 200), Known::Id(_)));
        let files = std::fs::read_dir(dir.join("relay-push-faces")).unwrap().count();
        assert_eq!(files, PER_ACCOUNT, "밀려난 그림 파일도 지운다");

        let big = vec![0u8; ACCOUNT_BYTES / 2 + 1];
        faces.keep("geno", &hash(900), &big, now + 300).unwrap();
        faces.keep("geno", &hash(901), &big, now + 301).unwrap();
        assert!(matches!(faces.known("geno", &hash(900), now + 302), Known::Unknown), "용량 상한");
        assert!(matches!(faces.known("geno", &hash(901), now + 302), Known::Id(_)));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_gateway_without_state_sends_no_face() {
        let faces = Faces::open(None);
        assert!(matches!(faces.known("geno", &"a".repeat(64), 1), Known::Off));
        assert_eq!(faces.keep("geno", &"a".repeat(64), b"x", 1), Err("storage_unavailable"));
    }
}
