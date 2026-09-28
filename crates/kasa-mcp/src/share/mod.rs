//! KASA-share — 학생들이 만든 결과물(시안·스크린샷·문서)을 기기 전부와 폰에 똑같이 둔다.
//!
//! 실폴더는 `~/.config/kasaterm/share` 이고 바탕화면 `KASA-share` 는 그곳을 가리키는
//! 링크다. 바탕화면을 실폴더로 쓰면 iCloud 바탕화면 동기화와 겹치고, 「저장 공간
//! 최적화」가 파일을 비우는 순간 지운 것으로 읽혀 삭제가 다른 기기로 번진다.
//!
//! 각 기기는 3초마다 제 폴더를 훑어 바뀐 파일에 번호표를 달고(`version`), 살아 있는
//! 다른 기기의 목록을 받아 필요한 파일만 끌어온다(`pull`). 받은 판은 원래 번호표 그대로
//! 다시 내주므로 맥북끼리 직접 길이 없어도 맥미니를 거쳐 퍼진다.

#[cfg(test)]
mod e2e_tests;
pub mod path;
mod pull;
pub(crate) mod serve;
pub mod version;

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use version::{bump, Entry};

pub const FOLDER: &str = "KASA-share";
const META: &str = ".kasaterm";
/// 한 파일 상한. 넘는 파일은 만든 기기에만 두고 상태에 적는다.
pub const MAX_FILE: u64 = 1 << 30;
const ROUND: std::time::Duration = std::time::Duration::from_secs(3);
const TRASH_KEEP_MS: u64 = 7 * 24 * 3600 * 1000;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Stat {
    /// 이 기기 디스크 위의 실제 이름(NFD 일 수도 있다).
    pub disk: String,
    pub size: u64,
    pub mtime_ms: u64,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Cursor {
    pub epoch: String,
    pub seq: u64,
}

#[derive(Serialize, Deserialize, Default)]
pub struct Index {
    /// 색인을 새로 만들 때마다 바뀐다 — 다른 기기가 「처음부터 다시 달라」고 알아챈다.
    pub epoch: String,
    pub seq: u64,
    pub entries: BTreeMap<String, Entry>,
    #[serde(default)]
    pub stats: BTreeMap<String, Stat>,
    #[serde(default)]
    pub peers: BTreeMap<String, Cursor>,
}

impl Index {
    fn record(&mut self, mut e: Entry) {
        self.seq += 1;
        e.seq = self.seq;
        self.entries.insert(path::key(&e.path), e);
    }

    fn live(&self) -> impl Iterator<Item = (&String, &Entry, &Stat)> {
        self.entries
            .iter()
            .filter(|(_, e)| !e.deleted)
            .filter_map(|(k, e)| Some((k, e, self.stats.get(k)?)))
    }
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct PeerStatus {
    pub id: String,
    pub label: String,
    pub ok: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub last_ok_ms: u64,
    #[serde(default)]
    pub pulled: usize,
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Status {
    pub root: String,
    pub desktop_link: Option<String>,
    pub updated_ms: u64,
    pub files: usize,
    pub bytes: u64,
    pub peers: Vec<PeerStatus>,
    /// 한 번에 너무 많이 사라져 삭제를 멈춘 수. `kasaterm-cli share accept-deletes` 로 푼다.
    pub paused_deletes: usize,
    pub too_big: Vec<String>,
    /// 이 기기에 둘 수 없는 이름(윈도우) 따위로 건너뛴 경로.
    pub skipped: Vec<String>,
}

pub struct Engine {
    pub root: PathBuf,
    pub me: String,
    pub label: String,
    index: Mutex<Index>,
    status: Mutex<Status>,
}

static ENGINE: OnceLock<Arc<Engine>> = OnceLock::new();

pub fn engine() -> Option<Arc<Engine>> {
    ENGINE.get().cloned()
}

pub fn now_ms() -> u64 {
    kasa_socket::board::now_ms()
}

fn mtime_ms(md: &std::fs::Metadata) -> u64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as u64)
}

fn join(root: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(root.to_path_buf(), |p, part| p.join(part))
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// 루트 아래 파일만, 링크는 따라가지 않는다 — 학생이 `ln -s ~ share/x` 를 두면 홈
/// 전체가 폰에 열린다.
fn walk(root: &Path, rel: &str, out: &mut Vec<(String, u64, u64)>) {
    let dir = if rel.is_empty() { root.to_path_buf() } else { join(root, rel) };
    let Ok(rd) = std::fs::read_dir(&dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if path::ignored(&name) {
            continue;
        }
        let Ok(md) = std::fs::symlink_metadata(e.path()) else { continue };
        let child = if rel.is_empty() { name } else { format!("{rel}/{name}") };
        if md.is_dir() {
            walk(root, &child, out);
        } else if md.is_file() {
            out.push((child, md.len(), mtime_ms(&md)));
        }
    }
}

/// `target` 까지 루트 안의 조상이 전부 진짜 폴더인가(없으면 만든다).
fn ensure_parents(root: &Path, rel: &str) -> std::io::Result<()> {
    let mut p = root.to_path_buf();
    let parts: Vec<&str> = rel.split('/').collect();
    for part in &parts[..parts.len().saturating_sub(1)] {
        p = p.join(part);
        match std::fs::symlink_metadata(&p) {
            Ok(md) if md.is_dir() => {}
            Ok(_) => return Err(std::io::Error::other(format!("{} is not a folder", p.display()))),
            Err(_) => std::fs::create_dir(&p)?,
        }
    }
    Ok(())
}

fn prune_empty_parents(root: &Path, rel: &str) {
    let mut parts: Vec<&str> = rel.split('/').collect();
    parts.pop();
    while !parts.is_empty() {
        if std::fs::remove_dir(join(root, &parts.join("/"))).is_err() {
            return;
        }
        parts.pop();
    }
}

fn disk_matches(root: &Path, st: &Stat) -> bool {
    std::fs::symlink_metadata(join(root, &st.disk))
        .is_ok_and(|md| md.is_file() && md.len() == st.size && mtime_ms(&md) == st.mtime_ms)
}

impl Engine {
    fn open(root: PathBuf, me: String, label: String) -> Self {
        let index = std::fs::read_to_string(root.join(META).join("index.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<Index>(&s).ok())
            .filter(|i| !i.epoch.is_empty())
            .unwrap_or_else(|| Index { epoch: uuid::Uuid::new_v4().to_string(), ..Default::default() });
        let status = Status { root: root.display().to_string(), ..Default::default() };
        Self { root, me, label, index: Mutex::new(index), status: Mutex::new(status) }
    }

    fn meta(&self, sub: &str) -> PathBuf {
        self.root.join(META).join(sub)
    }

    pub fn with_index<T>(&self, f: impl FnOnce(&mut Index) -> T) -> T {
        let mut guard = self.index.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut guard)
    }

    pub fn status(&self) -> Status {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }

    fn save(&self) {
        let (body, status) = {
            let json = self.with_index(|i| serde_json::to_vec(&*i));
            let mut st = self.status.lock().unwrap_or_else(|e| e.into_inner());
            st.updated_ms = now_ms();
            (st.files, st.bytes) = self.with_index(|i| {
                let live: Vec<_> = i.live().collect();
                (live.len(), live.iter().map(|(_, e, _)| e.size).sum())
            });
            (json, serde_json::to_vec_pretty(&*st))
        };
        for (name, body) in [("index.json", body), ("status.json", status)] {
            let Ok(body) = body else { continue };
            let (tmp, dst) = (self.meta(&format!("{name}.new")), self.meta(name));
            if std::fs::write(&tmp, body).is_ok() {
                let _ = std::fs::rename(tmp, dst);
            }
        }
    }

    /// 제 폴더를 훑어 바뀐 파일에 번호표를 단다. 크기·mtime 은 「바뀌었나」의 힌트일 뿐이고,
    /// 두 번 연속 그대로여야 판을 매긴다 — 아직 쓰는 중인 파일을 반쪽으로 퍼뜨리지 않게.
    fn scan(&self, pending: &mut HashMap<String, (u64, u64)>) {
        if !self.root.is_dir() {
            return;
        }
        let mut found = Vec::new();
        walk(&self.root, "", &mut found);
        let mut seen = HashSet::new();
        let (mut hash, mut too_big) = (Vec::new(), Vec::new());
        self.with_index(|idx| {
            for (disk, size, mtime) in found {
                let key = path::key(&disk);
                if !seen.insert(key.clone()) {
                    continue;
                }
                if size > MAX_FILE {
                    too_big.push(disk);
                    continue;
                }
                let tracked = idx.entries.get(&key).is_some_and(|e| !e.deleted)
                    && idx.stats.get(&key).is_some_and(|s| s.disk == disk && s.size == size && s.mtime_ms == mtime);
                if tracked {
                    pending.remove(&key);
                    continue;
                }
                if pending.get(&key) == Some(&(size, mtime)) {
                    hash.push((key, disk, size, mtime));
                } else {
                    pending.insert(key, (size, mtime));
                }
            }
        });
        pending.retain(|k, _| seen.contains(k));

        let mut hashed = Vec::new();
        for (key, disk, size, mtime) in hash {
            let file = join(&self.root, &disk);
            let Ok(sha) = sha256_file(&file) else { continue };
            let still = std::fs::symlink_metadata(&file).is_ok_and(|md| md.len() == size && mtime_ms(&md) == mtime);
            if still {
                pending.remove(&key);
                hashed.push((key, disk, size, mtime, sha));
            }
        }

        let accept = self.meta("accept-deletes");
        let mut paused = 0;
        self.with_index(|idx| {
            for (key, disk, size, mtime, sha) in hashed {
                let stat = Stat { disk: disk.clone(), size, mtime_ms: mtime };
                let old = idx.entries.get(&key).cloned();
                if old.as_ref().is_some_and(|e| !e.deleted && e.sha == sha) {
                    idx.stats.insert(key, stat);
                    continue;
                }
                let vv = bump(&old.map(|e| e.vv).unwrap_or_default(), &self.me, now_ms());
                idx.record(Entry {
                    path: path::nfc(&disk),
                    vv,
                    sha,
                    size,
                    mtime_ms: mtime,
                    deleted: false,
                    origin: self.label.clone(),
                    seq: 0,
                });
                idx.stats.insert(key, stat);
            }
            let live = idx.entries.values().filter(|e| !e.deleted).count();
            let gone: Vec<String> = idx
                .entries
                .iter()
                .filter(|(k, e)| !e.deleted && !seen.contains(*k))
                .map(|(k, _)| k.clone())
                .collect();
            if gone.len() >= 10 && gone.len() * 10 > live * 3 && !accept.exists() {
                paused = gone.len();
                return;
            }
            let _ = std::fs::remove_file(&accept);
            for key in gone {
                let e = idx.entries[&key].clone();
                let vv = bump(&e.vv, &self.me, now_ms());
                idx.record(Entry { deleted: true, vv, origin: self.label.clone(), mtime_ms: now_ms(), ..e });
                idx.stats.remove(&key);
            }
        });
        if let Ok(mut st) = self.status.lock() {
            st.paused_deletes = paused;
            st.too_big = too_big;
        }
    }

    /// 받은 파일을 제자리에 둔다. 그사이 이 기기에서 고쳐졌으면 손대지 않고 `false` —
    /// 다음 훑기가 그 고침에 판을 매기고, 그러면 충돌로 다시 판정된다.
    fn place(&self, key: &str, entry: &Entry, part: &Path) -> std::io::Result<bool> {
        let target_rel = entry.path.clone();
        let target = join(&self.root, &target_rel);
        let old = self.with_index(|i| i.stats.get(key).cloned());
        match &old {
            Some(st) if !disk_matches(&self.root, st) => return Ok(false),
            None if std::fs::symlink_metadata(&target).is_ok() => return Ok(false),
            _ => {}
        }
        ensure_parents(&self.root, &target_rel)?;
        if let Some(st) = old.as_ref().filter(|st| st.disk != target_rel) {
            let _ = std::fs::remove_file(join(&self.root, &st.disk));
        }
        std::fs::rename(part, &target)?;
        let when = std::time::UNIX_EPOCH + std::time::Duration::from_millis(entry.mtime_ms);
        if entry.mtime_ms > 0 {
            let _ = std::fs::File::options().write(true).open(&target).and_then(|f| f.set_modified(when));
        }
        let md = std::fs::symlink_metadata(&target)?;
        let stat = Stat { disk: target_rel, size: md.len(), mtime_ms: mtime_ms(&md) };
        self.with_index(|i| {
            i.stats.insert(key.to_string(), stat);
            i.record(entry.clone());
        });
        Ok(true)
    }

    /// 진 쪽 내용을 새 이름으로 둔다. 색인에는 안 적는다 — 다음 훑기가 새 파일로 판을 매긴다.
    fn place_copy(&self, rel: &str, part: &Path) -> std::io::Result<()> {
        ensure_parents(&self.root, rel)?;
        std::fs::rename(part, join(&self.root, rel))
    }

    /// 지운 판이 오면 이 기기 사본을 휴지통에 7일 둔다.
    fn trash(&self, key: &str, entry: &Entry) -> std::io::Result<bool> {
        if let Some(st) = self.with_index(|i| i.stats.get(key).cloned()) {
            if !disk_matches(&self.root, &st) {
                return Ok(false);
            }
            let dst = join(&self.meta("trash").join(now_ms().to_string()), &st.disk);
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::rename(join(&self.root, &st.disk), dst)?;
            prune_empty_parents(&self.root, &st.disk);
        }
        self.with_index(|i| {
            i.stats.remove(key);
            i.record(entry.clone());
        });
        Ok(true)
    }

    /// 진 쪽이 이 기기 내용이면 새 이름으로 비켜 둔다.
    fn step_aside(&self, key: &str, copy: &str) -> std::io::Result<bool> {
        let Some(st) = self.with_index(|i| i.stats.get(key).cloned()) else { return Ok(false) };
        if !disk_matches(&self.root, &st) {
            return Ok(false);
        }
        ensure_parents(&self.root, copy)?;
        std::fs::rename(join(&self.root, &st.disk), join(&self.root, copy))?;
        self.with_index(|i| i.stats.remove(key));
        Ok(true)
    }

    fn taken(&self, key: &str) -> bool {
        self.with_index(|i| i.entries.get(key).is_some_and(|e| !e.deleted))
            || std::fs::symlink_metadata(join(&self.root, key)).is_ok()
    }

    fn sweep_trash(&self) {
        let Ok(rd) = std::fs::read_dir(self.meta("trash")) else { return };
        let cutoff = now_ms().saturating_sub(TRASH_KEEP_MS);
        for e in rd.flatten() {
            let old = e.file_name().to_str().and_then(|s| s.parse::<u64>().ok()).is_some_and(|t| t < cutoff);
            if old {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
}

fn desktop_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        // OneDrive 가 바탕화면을 옮겼을 수 있어 알려진 폴더를 직접 묻는다.
        let out = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", "[Environment]::GetFolderPath('Desktop')"])
            .output()
            .ok()?;
        let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    Some(kasa_socket::home_dir()?.join("Desktop"))
}

/// 바탕화면에 `KASA-share` 링크를 둔다. 같은 이름의 다른 것이 있으면 손대지 않는다.
fn ensure_desktop_link(root: &Path) -> Option<PathBuf> {
    let link = desktop_dir()?.join(FOLDER);
    let ours = |p: &Path| {
        std::fs::read_link(p).ok().and_then(|t| t.canonicalize().ok()) == root.canonicalize().ok()
    };
    match std::fs::symlink_metadata(&link) {
        Ok(_) if ours(&link) => return Some(link),
        Ok(_) => {
            eprintln!("[share] {} 에 다른 것이 있어 링크를 만들지 않았습니다", link.display());
            return None;
        }
        Err(_) => {}
    }
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(root, &link).is_ok();
    // 심볼릭 링크는 관리자·개발자 모드가 있어야 해서 정션으로 간다.
    #[cfg(windows)]
    let made = std::process::Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(&link)
        .arg(root)
        .output()
        .is_ok_and(|o| o.status.success());
    made.then_some(link)
}

/// 앱 본체에서만 돈다(`http.rs` 의 `run_scheduler`). 두 벌이 같은 폴더를 돌리면
/// 서로의 판을 새 고침으로 읽는다.
pub async fn run() {
    let explicit = std::env::var_os("KASATERM_SHARE_DIR").is_some();
    if kasa_socket::isolated_collab_root().is_some() && !explicit {
        return;
    }
    let (Some(root), Some(me)) = (kasa_socket::share_dir(), crate::mobile::machine_identity()) else {
        return;
    };
    if std::fs::create_dir_all(root.join(META).join("tmp")).is_err() {
        eprintln!("[share] {} 을 못 만들었습니다", root.display());
        return;
    }
    let engine = Arc::new(Engine::open(root.clone(), me.clone(), crate::machines::self_label()));
    if ENGINE.set(engine.clone()).is_err() {
        return;
    }
    let link = if explicit {
        None
    } else {
        let root = root.clone();
        tokio::task::spawn_blocking(move || ensure_desktop_link(&root)).await.ok().flatten()
    };
    if let Ok(mut st) = engine.status.lock() {
        st.desktop_link = link.map(|l| l.display().to_string());
    }
    let client = match reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build() {
        Ok(c) => c,
        Err(_) => return,
    };
    let mut pending = HashMap::new();
    let mut last_sweep = 0;
    loop {
        let e = engine.clone();
        let mut p = std::mem::take(&mut pending);
        pending = tokio::task::spawn_blocking(move || {
            e.scan(&mut p);
            p
        })
        .await
        .unwrap_or_default();
        let mut peers = Vec::new();
        for peer in pull::peers(&me) {
            peers.push(pull::pull(&engine, &client, &peer).await);
        }
        if let Ok(mut st) = engine.status.lock() {
            st.peers = peers;
        }
        if now_ms().saturating_sub(last_sweep) > 3600 * 1000 {
            last_sweep = now_ms();
            engine.sweep_trash();
        }
        engine.save();
        tokio::time::sleep(ROUND).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rig(name: &str) -> (PathBuf, Engine) {
        let root = std::env::temp_dir().join(format!("kasa-share-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(META).join("tmp")).unwrap();
        (root.clone(), Engine::open(root, "me".into(), "A".into()))
    }

    fn scan_twice(e: &Engine, p: &mut HashMap<String, (u64, u64)>) {
        e.scan(p);
        e.scan(p);
    }

    #[test]
    fn scan_waits_for_quiet_file_and_skips_links_and_junk() {
        let (root, e) = rig("scan");
        std::fs::create_dir_all(root.join("d")).unwrap();
        std::fs::write(root.join("d/a.png"), b"1").unwrap();
        std::fs::write(root.join("d/.DS_Store"), b"x").unwrap();
        std::fs::write(root.join("d/b.part"), b"x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc", root.join("d/escape")).unwrap();
        let mut p = HashMap::new();
        e.scan(&mut p);
        assert!(e.with_index(|i| i.entries.is_empty()), "must wait one more round");
        e.scan(&mut p);
        let keys: Vec<String> = e.with_index(|i| i.entries.keys().cloned().collect());
        assert_eq!(keys, vec!["d/a.png".to_string()]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn placed_file_does_not_bump_version_again() {
        let (root, e) = rig("echo");
        let part = root.join(META).join("tmp").join("x.part");
        std::fs::write(&part, b"hello").unwrap();
        let entry = Entry {
            path: "d/받은.png".into(),
            vv: [("peer".to_string(), 5)].into(),
            sha: "s".into(),
            size: 5,
            mtime_ms: 1_700_000_000_000,
            origin: "B".into(),
            ..Default::default()
        };
        assert!(e.place(&path::key(&entry.path), &entry, &part).unwrap());
        let before = e.with_index(|i| (i.seq, i.entries.clone()));
        let mut p = HashMap::new();
        scan_twice(&e, &mut p);
        assert_eq!(before, e.with_index(|i| (i.seq, i.entries.clone())));
        let md = std::fs::metadata(root.join("d/받은.png")).unwrap();
        assert_eq!(mtime_ms(&md), 1_700_000_000_000);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn place_refuses_to_clobber_untracked_or_edited_files() {
        let (root, e) = rig("clobber");
        std::fs::write(root.join("new.md"), b"mine").unwrap();
        let part = root.join(META).join("tmp").join("y.part");
        std::fs::write(&part, b"theirs").unwrap();
        let entry = Entry { path: "new.md".into(), vv: [("b".to_string(), 1)].into(), sha: "t".into(), ..Default::default() };
        assert!(!e.place("new.md", &entry, &part).unwrap());
        assert_eq!(std::fs::read(root.join("new.md")).unwrap(), b"mine");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn delete_is_recorded_but_mass_vanish_pauses() {
        let (root, e) = rig("delete");
        for i in 0..12 {
            std::fs::write(root.join(format!("f{i}.txt")), [i]).unwrap();
        }
        let mut p = HashMap::new();
        scan_twice(&e, &mut p);
        std::fs::remove_file(root.join("f0.txt")).unwrap();
        e.scan(&mut p);
        assert!(e.with_index(|i| i.entries["f0.txt"].deleted));
        for i in 1..12 {
            std::fs::remove_file(root.join(format!("f{i}.txt"))).unwrap();
        }
        e.scan(&mut p);
        assert_eq!(e.status().paused_deletes, 11);
        assert!(e.with_index(|i| !i.entries["f1.txt"].deleted));
        std::fs::write(e.meta("accept-deletes"), b"").unwrap();
        e.scan(&mut p);
        assert!(e.with_index(|i| i.entries.values().all(|x| x.deleted)));
        assert!(!e.meta("accept-deletes").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn trash_keeps_a_copy_and_prunes_empty_folders() {
        let (root, e) = rig("trash");
        std::fs::create_dir_all(root.join("d/sub")).unwrap();
        std::fs::write(root.join("d/sub/a.txt"), b"a").unwrap();
        let mut p = HashMap::new();
        scan_twice(&e, &mut p);
        let mut tomb = e.with_index(|i| i.entries["d/sub/a.txt"].clone());
        tomb.deleted = true;
        tomb.vv.insert("b".into(), 9);
        assert!(e.trash("d/sub/a.txt", &tomb).unwrap());
        assert!(!root.join("d").exists());
        let kept = std::fs::read_dir(e.meta("trash")).unwrap().count();
        assert_eq!(kept, 1);
        let _ = std::fs::remove_dir_all(root);
    }
}
