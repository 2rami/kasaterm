//! 대화에 박힌 소리·영상 카드의 등록 저장소.
//!
//! 공유 폴더가 아니라 이 기기 전용 캐시에 내용 주소(sha256)로 복사해 둔다 — 원본이 지워지거나
//! 바뀌어도 옛 대화의 카드는 등록 때 그 바이트를 그대로 튼다. 원격(폰·다른 PC)은 id 로만 읽고,
//! 경로는 응답에 나가지 않는다. 배치:
//!
//! - `blobs/<sha256>.<ext>` — 원본 바이트. 확장자를 남겨야 AVPlayer 가 컨테이너를 알아본다
//! - `posters/<sha256>.png` — 영상 첫 장면
//! - `meta/<id>.json` — 카드 정보. id 는 이 정보 전체의 해시라 같은 id 가 다른 내용을 가리킬 수 없다
//! - `tmp/` — 복사 중인 파일. 다 쓴 뒤 덮어쓰기 없는 링크로 한 번에 공개한다

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const VERSION: u32 = 1;
pub const MAX_FILE_BYTES: u64 = 1 << 30;
pub const MAX_POSTER_BYTES: usize = 12 << 20;
pub const MAX_POSTER_SIDE: u32 = 1600;
/// 서버가 한 번에 내주는 상한. 클라이언트는 [`CLIENT_CHUNK`] 로 나눠 받는다.
pub const MAX_CHUNK: usize = 8 << 20;
pub const CLIENT_CHUNK: usize = 4 << 20;
const MAX_SIDE: u32 = 16384;
const MAX_DURATION_MS: u64 = 30 * 24 * 60 * 60 * 1000;
const MAX_NAME_CHARS: usize = 200;
const MAX_META_BYTES: u64 = 64 * 1024;
const COPY_BUF: usize = 1 << 20;
const STALE_TMP: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Probe {
    pub duration_ms: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// PNG. 영상만 받는다.
    pub poster: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Media {
    pub version: u32,
    pub id: String,
    /// `"audio"` | `"video"`
    pub kind: String,
    pub name: String,
    pub mime: String,
    pub size: u64,
    pub sha256: String,
    pub duration_ms: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub poster: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Record {
    media: Media,
    ext: String,
    poster_sha256: Option<String>,
}

/// id 를 짓는 재료. `Record` 에서 id 만 뺀 것 — 필드 순서가 곧 직렬화 순서라 해시가 흔들리지 않는다.
#[derive(Serialize)]
struct IdInput<'a> {
    version: u32,
    kind: &'a str,
    name: &'a str,
    mime: &'a str,
    size: u64,
    sha256: &'a str,
    duration_ms: Option<u64>,
    width: Option<u32>,
    height: Option<u32>,
    ext: &'a str,
    poster_sha256: Option<&'a str>,
}

struct Format {
    exts: &'static [&'static str],
    kind: &'static str,
    mime: &'static str,
    sniff: fn(&[u8]) -> bool,
}

/// 맥 AVPlayer 와 iOS 가 둘 다 트는 것만 받는다 — 한쪽에서만 열리는 카드는 만들지 않는다.
const FORMATS: &[Format] = &[
    Format { exts: &["mp3"], kind: "audio", mime: "audio/mpeg", sniff: sniff_mp3 },
    Format { exts: &["m4a", "m4b"], kind: "audio", mime: "audio/mp4", sniff: sniff_ftyp },
    Format { exts: &["aac"], kind: "audio", mime: "audio/aac", sniff: sniff_aac },
    Format { exts: &["wav"], kind: "audio", mime: "audio/wav", sniff: sniff_wav },
    Format { exts: &["aif", "aiff"], kind: "audio", mime: "audio/aiff", sniff: sniff_aiff },
    Format { exts: &["caf"], kind: "audio", mime: "audio/x-caf", sniff: sniff_caf },
    Format { exts: &["flac"], kind: "audio", mime: "audio/flac", sniff: sniff_flac },
    Format { exts: &["mp4"], kind: "video", mime: "video/mp4", sniff: sniff_ftyp },
    Format { exts: &["m4v"], kind: "video", mime: "video/x-m4v", sniff: sniff_ftyp },
    Format { exts: &["mov"], kind: "video", mime: "video/quicktime", sniff: sniff_mov },
];

fn has_id3(h: &[u8]) -> bool {
    h.starts_with(b"ID3")
}
fn sniff_mp3(h: &[u8]) -> bool {
    has_id3(h) || (h.len() >= 2 && h[0] == 0xFF && h[1] & 0xE0 == 0xE0)
}
fn sniff_aac(h: &[u8]) -> bool {
    has_id3(h) || (h.len() >= 2 && h[0] == 0xFF && h[1] & 0xF6 == 0xF0)
}
fn sniff_ftyp(h: &[u8]) -> bool {
    matches!(h.get(4..8), Some(b"ftyp"))
}
fn sniff_mov(h: &[u8]) -> bool {
    matches!(
        h.get(4..8),
        Some(b"ftyp" | b"moov" | b"mdat" | b"wide" | b"free" | b"skip" | b"pnot")
    )
}
fn sniff_wav(h: &[u8]) -> bool {
    h.starts_with(b"RIFF") && matches!(h.get(8..12), Some(b"WAVE"))
}
fn sniff_aiff(h: &[u8]) -> bool {
    h.starts_with(b"FORM") && matches!(h.get(8..12), Some(b"AIFF" | b"AIFC"))
}
fn sniff_caf(h: &[u8]) -> bool {
    h.starts_with(b"caff")
}
fn sniff_flac(h: &[u8]) -> bool {
    h.starts_with(b"fLaC") || has_id3(h)
}

fn format_for_ext(ext: &str) -> Option<&'static Format> {
    FORMATS.iter().find(|f| f.exts.contains(&ext))
}

fn invalid_input(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg.to_string())
}
fn invalid_data(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}
fn not_found(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, msg.to_string())
}

pub fn root() -> PathBuf {
    if let Some(path) = std::env::var_os("KASATERM_INLINE_MEDIA_DIR").filter(|s| !s.is_empty()) {
        return path.into();
    }
    if let Some(parent) = std::env::var_os("KASATERM_SETTINGS_FILE")
        .filter(|s| !s.is_empty())
        .and_then(|s| PathBuf::from(s).parent().map(Path::to_path_buf))
    {
        return parent.join("inline-media");
    }
    match crate::home_dir() {
        Some(home) => home.join(".config/kasaterm/inline-media"),
        None => std::env::temp_dir().join("kasaterm-inline-media"),
    }
}

pub fn store() -> Store {
    Store::at(root())
}

pub fn valid_id(id: &str) -> bool {
    id.len() == 16 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn valid_sha(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

pub fn register(path: &Path, probe: Probe) -> io::Result<Media> {
    store().register(path, probe)
}
pub fn metadata(id: &str) -> io::Result<Media> {
    store().metadata(id)
}
pub fn media_path(id: &str) -> io::Result<PathBuf> {
    store().media_path(id)
}
pub fn poster_path(id: &str) -> io::Result<PathBuf> {
    store().poster_path(id)
}
pub fn read_chunk(id: &str, offset: u64, len: usize) -> io::Result<Vec<u8>> {
    store().read_chunk(id, offset, len)
}
pub fn read_poster(id: &str) -> io::Result<Vec<u8>> {
    store().read_poster(id)
}

/// 저장소 하나. 기본 자리는 [`store`], 시험·리그는 [`Store::at`] 으로 다른 자리를 쓴다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 로컬 CLI 전용. 원본을 끝까지 한 번 읽어 해시와 복사를 같이 하고, 그동안 원본이 바뀌면 거절한다.
    pub fn register(&self, path: &Path, probe: Probe) -> io::Result<Media> {
        self.ingest(path, probe, &mut || {})
    }

    pub fn metadata(&self, id: &str) -> io::Result<Media> {
        Ok(self.record(id)?.media)
    }

    pub fn media_path(&self, id: &str) -> io::Result<PathBuf> {
        let record = self.record(id)?;
        Ok(self.blob_path(&record.media.sha256, &record.ext))
    }

    pub fn poster_path(&self, id: &str) -> io::Result<PathBuf> {
        let record = self.record(id)?;
        let sha = record.poster_sha256.ok_or_else(|| not_found("media has no poster"))?;
        let path = self.poster_file(&sha);
        open_regular(&path)?;
        Ok(path)
    }

    /// `offset` 은 파일 안이어야 하고, 끝을 넘는 `len` 은 남은 만큼만 준다.
    pub fn read_chunk(&self, id: &str, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        if len == 0 || len > MAX_CHUNK {
            return Err(invalid_input("chunk length out of range"));
        }
        let record = self.record(id)?;
        let size = record.media.size;
        if offset >= size {
            return Err(invalid_input("offset out of range"));
        }
        let n = (size - offset).min(len as u64) as usize;
        let (mut file, meta) = open_regular(&self.blob_path(&record.media.sha256, &record.ext))?;
        if meta.len() != size {
            return Err(invalid_data("media cache entry changed"));
        }
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = vec![0; n];
        file.read_exact(&mut bytes)?;
        Ok(bytes)
    }

    pub fn read_poster(&self, id: &str) -> io::Result<Vec<u8>> {
        let record = self.record(id)?;
        let sha = record.poster_sha256.ok_or_else(|| not_found("media has no poster"))?;
        let (file, meta) = open_regular(&self.poster_file(&sha))?;
        if meta.len() > MAX_POSTER_BYTES as u64 {
            return Err(invalid_data("poster cache entry too large"));
        }
        let mut bytes = Vec::with_capacity(meta.len() as usize);
        file.take(MAX_POSTER_BYTES as u64 + 1).read_to_end(&mut bytes)?;
        poster_dims(&bytes).ok_or_else(|| invalid_data("poster cache entry corrupted"))?;
        Ok(bytes)
    }

    fn blob_path(&self, sha: &str, ext: &str) -> PathBuf {
        self.root.join("blobs").join(format!("{sha}.{ext}"))
    }
    fn poster_file(&self, sha: &str) -> PathBuf {
        self.root.join("posters").join(format!("{sha}.png"))
    }
    fn meta_path(&self, id: &str) -> PathBuf {
        self.root.join("meta").join(format!("{id}.json"))
    }

    /// 읽을 때마다 기록을 처음부터 다시 믿지 않는다 — id·해시·확장자·원본 크기가 모두 맞아야 내준다.
    fn record(&self, id: &str) -> io::Result<Record> {
        if !valid_id(id) {
            return Err(invalid_input("invalid media id"));
        }
        let (file, meta) = open_regular(&self.meta_path(id))?;
        if meta.len() > MAX_META_BYTES {
            return Err(invalid_data("media record too large"));
        }
        let mut text = String::new();
        file.take(MAX_META_BYTES + 1).read_to_string(&mut text)?;
        let record: Record =
            serde_json::from_str(&text).map_err(|_| invalid_data("media record corrupted"))?;
        let m = &record.media;
        let format = format_for_ext(&record.ext);
        let consistent = m.version == VERSION
            && m.id == id
            && valid_sha(&m.sha256)
            && format.is_some_and(|f| f.kind == m.kind && f.mime == m.mime)
            && m.poster == record.poster_sha256.is_some()
            && record.poster_sha256.as_deref().is_none_or(valid_sha)
            && m.size > 0
            && m.size <= MAX_FILE_BYTES
            && record_id(&record) == id;
        if !consistent {
            return Err(invalid_data("media record corrupted"));
        }
        let (_, blob) = open_regular(&self.blob_path(&m.sha256, &record.ext))?;
        if blob.len() != m.size {
            return Err(invalid_data("media cache entry changed"));
        }
        Ok(record)
    }

    fn ingest(&self, path: &Path, probe: Probe, mid_copy: &mut dyn FnMut()) -> io::Result<Media> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        let format = format_for_ext(&ext).ok_or_else(|| invalid_input("unsupported media type"))?;
        validate_probe(format.kind, &probe)?;

        let mut src = File::open(path)?;
        let before = src.metadata()?;
        if !before.is_file() {
            return Err(invalid_input("not a regular file"));
        }
        let size = before.len();
        if size == 0 {
            return Err(invalid_input("empty media file"));
        }
        if size > MAX_FILE_BYTES {
            return Err(invalid_input("media file larger than 1 GiB"));
        }
        let mut head = [0u8; 16];
        let head_len = read_prefix(&mut src, &mut head)?;
        if !(format.sniff)(&head[..head_len]) {
            return Err(invalid_input("file content does not match its extension"));
        }
        src.seek(SeekFrom::Start(0))?;

        self.prepare_dirs()?;
        let tmp = TempFile::new(self)?;
        let mut out = tmp.create()?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; COPY_BUF];
        let mut copied = 0u64;
        loop {
            let n = src.read(&mut buf)?;
            if n == 0 {
                break;
            }
            copied += n as u64;
            if copied > size {
                return Err(source_changed());
            }
            hasher.update(&buf[..n]);
            out.write_all(&buf[..n])?;
            mid_copy();
        }
        let after = src.metadata()?;
        if copied != size || after.len() != size || after.modified().ok() != before.modified().ok() {
            return Err(source_changed());
        }
        out.sync_all()?;
        drop(out);
        let sha = hex(&hasher.finalize());
        publish(&tmp.path, &self.blob_path(&sha, &ext), size)?;

        let poster_sha256 = match &probe.poster {
            Some(png) => {
                let poster_sha = hex(&Sha256::digest(png));
                let tmp = TempFile::new(self)?;
                let mut out = tmp.create()?;
                out.write_all(png)?;
                out.sync_all()?;
                drop(out);
                publish(&tmp.path, &self.poster_file(&poster_sha), png.len() as u64)?;
                Some(poster_sha)
            }
            None => None,
        };

        let mut record = Record {
            media: Media {
                version: VERSION,
                id: String::new(),
                kind: format.kind.into(),
                name: display_name(path, &ext),
                mime: format.mime.into(),
                size,
                sha256: sha,
                duration_ms: probe.duration_ms,
                width: probe.width,
                height: probe.height,
                poster: poster_sha256.is_some(),
            },
            ext,
            poster_sha256,
        };
        record.media.id = record_id(&record);
        self.publish_record(&record)?;
        Ok(record.media)
    }

    /// 같은 id 는 언제나 같은 기록이다. 이미 있으면 비교만 하고 덮어쓰지 않는다.
    fn publish_record(&self, record: &Record) -> io::Result<()> {
        let path = self.meta_path(&record.media.id);
        let json = serde_json::to_vec(record).map_err(|e| io::Error::other(e.to_string()))?;
        let tmp = TempFile::new(self)?;
        let mut out = tmp.create()?;
        out.write_all(&json)?;
        out.sync_all()?;
        drop(out);
        seal(&tmp.path)?;
        match link_new(&tmp.path, &path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                match self.record(&record.media.id) {
                    Ok(existing) if existing == *record => Ok(()),
                    Ok(_) => Err(io::Error::other("media id collision")),
                    // 깨진 기록은 같은 내용으로 다시 세운다 — 원본은 방금 해시로 확인했다.
                    Err(_) => {
                        std::fs::remove_file(&path)?;
                        link_new(&tmp.path, &path)
                    }
                }
            }
            Err(e) => Err(e),
        }
    }

    fn prepare_dirs(&self) -> io::Result<()> {
        for dir in [&self.root, &self.root.join("blobs"), &self.root.join("posters"), &self.root.join("meta"), &self.root.join("tmp")] {
            std::fs::create_dir_all(dir)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
            }
        }
        // 복사 중에 죽은 프로세스의 조각.
        if let Ok(entries) = std::fs::read_dir(self.root.join("tmp")) {
            for entry in entries.flatten() {
                let stale = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age > STALE_TMP);
                if stale {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
        Ok(())
    }
}

fn source_changed() -> io::Error {
    io::Error::other("media file changed while registering")
}

fn record_id(record: &Record) -> String {
    let m = &record.media;
    let input = IdInput {
        version: m.version,
        kind: &m.kind,
        name: &m.name,
        mime: &m.mime,
        size: m.size,
        sha256: &m.sha256,
        duration_ms: m.duration_ms,
        width: m.width,
        height: m.height,
        ext: &record.ext,
        poster_sha256: record.poster_sha256.as_deref(),
    };
    let bytes = serde_json::to_vec(&input).unwrap_or_default();
    let mut digest = Sha256::new();
    digest.update(b"kasa-inline-media\0");
    digest.update(&bytes);
    hex(&digest.finalize()[..8])
}

fn validate_probe(kind: &str, probe: &Probe) -> io::Result<()> {
    if let Some(ms) = probe.duration_ms {
        if ms == 0 || ms > MAX_DURATION_MS {
            return Err(invalid_input("duration out of range"));
        }
    }
    match (probe.width, probe.height) {
        (None, None) => {}
        (Some(w), Some(h)) if kind == "video" && (1..=MAX_SIDE).contains(&w) && (1..=MAX_SIDE).contains(&h) => {}
        _ => return Err(invalid_input("invalid media dimensions")),
    }
    if let Some(png) = &probe.poster {
        if kind != "video" {
            return Err(invalid_input("only video takes a poster"));
        }
        if png.len() > MAX_POSTER_BYTES {
            return Err(invalid_input("poster larger than 12 MiB"));
        }
        let (w, h) = poster_dims(png).ok_or_else(|| invalid_input("poster must be a PNG"))?;
        if !(1..=MAX_POSTER_SIDE).contains(&w) || !(1..=MAX_POSTER_SIDE).contains(&h) {
            return Err(invalid_input("poster larger than 1600px"));
        }
    }
    Ok(())
}

fn poster_dims(png: &[u8]) -> Option<(u32, u32)> {
    if png.len() < 24 || !png.starts_with(b"\x89PNG\r\n\x1a\n") || &png[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes(png[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(png[20..24].try_into().ok()?);
    Some((w, h))
}

fn display_name(path: &Path, ext: &str) -> String {
    let raw = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let clean: String = raw.chars().filter(|c| !c.is_control()).take(MAX_NAME_CHARS).collect();
    let clean = clean.trim();
    if clean.is_empty() {
        format!("media.{ext}")
    } else {
        clean.to_string()
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn read_prefix(file: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match file.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

/// 캐시 안의 파일만 연다. 링크는 따라가지 않는다 — 누가 캐시에 링크를 심어도 원격으로 바깥 파일이 새지 않게.
fn open_regular(path: &Path) -> io::Result<(File, std::fs::Metadata)> {
    let link = std::fs::symlink_metadata(path)?;
    if !link.file_type().is_file() {
        return Err(invalid_data("media cache entry is not a regular file"));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(invalid_data("media cache entry is not a regular file"));
    }
    Ok((file, meta))
}

/// 덮어쓰지 않는 공개. 같은 내용 주소에 이미 멀쩡한 파일이 있으면 그대로 쓴다.
fn publish(tmp: &Path, dest: &Path, size: u64) -> io::Result<()> {
    seal(tmp)?;
    match link_new(tmp, dest) {
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            if open_regular(dest).is_ok_and(|(_, m)| m.len() == size) {
                return Ok(());
            }
            std::fs::remove_file(dest)?;
            link_new(tmp, dest)
        }
        other => other,
    }
}

fn seal(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o400))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn link_new(tmp: &Path, dest: &Path) -> io::Result<()> {
    match std::fs::hard_link(tmp, dest) {
        Err(e) if e.kind() != io::ErrorKind::AlreadyExists => {
            // 하드 링크를 못 거는 파일 시스템. 이름 바꾸기는 덮어쓰므로 먼저 비어 있는지 본다.
            if std::fs::symlink_metadata(dest).is_ok() {
                return Err(io::Error::from(io::ErrorKind::AlreadyExists));
            }
            std::fs::rename(tmp, dest)
        }
        other => other,
    }
}

struct TempFile {
    path: PathBuf,
}

impl TempFile {
    fn new(store: &Store) -> io::Result<Self> {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let name = format!("{}-{nanos}-{seq}.part", std::process::id());
        Ok(Self { path: store.root.join("tmp").join(name) })
    }

    fn create(&self) -> io::Result<File> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(&self.path)
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("inline-media-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn mp4_bytes(len: usize, seed: u32) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(len);
        bytes.extend_from_slice(b"\0\0\0\x18ftypisom\0\0\0\0isomavc1");
        let mut x = seed.wrapping_mul(2654435761).max(1);
        while bytes.len() < len {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            bytes.push(x as u8);
        }
        bytes.truncate(len);
        bytes
    }

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        bytes.extend_from_slice(&w.to_be_bytes());
        bytes.extend_from_slice(&h.to_be_bytes());
        bytes.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
        bytes
    }

    fn video_probe() -> Probe {
        Probe { duration_ms: Some(1500), width: Some(640), height: Some(360), poster: Some(png(320, 180)) }
    }

    #[test]
    fn register_copies_into_content_addressed_cache_and_reads_back() {
        let dir = temp_root("register");
        let store = Store::at(dir.join("store"));
        let bytes = mp4_bytes(3 * COPY_BUF + 123, 1);
        let src = dir.join("clip.MP4");
        std::fs::write(&src, &bytes).unwrap();

        let media = store.register(&src, video_probe()).unwrap();
        assert!(valid_id(&media.id));
        assert_eq!((media.kind.as_str(), media.mime.as_str()), ("video", "video/mp4"));
        assert_eq!((media.name.as_str(), media.size, media.poster), ("clip.MP4", bytes.len() as u64, true));
        assert_eq!(media.sha256, hex(&Sha256::digest(&bytes)));
        assert_eq!(store.metadata(&media.id).unwrap(), media);

        let path = store.media_path(&media.id).unwrap();
        assert_eq!(path, dir.join("store/blobs").join(format!("{}.mp4", media.sha256)));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(store.read_poster(&media.id).unwrap(), png(320, 180));
        assert!(store.poster_path(&media.id).unwrap().starts_with(dir.join("store/posters")));

        let mut joined = Vec::new();
        let mut offset = 0u64;
        while offset < media.size {
            let chunk = store.read_chunk(&media.id, offset, COPY_BUF + 7).unwrap();
            offset += chunk.len() as u64;
            joined.extend(chunk);
        }
        assert_eq!(joined, bytes);
        assert_eq!(std::fs::read_dir(dir.join("store/tmp")).unwrap().count(), 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn registration_is_immutable_and_survives_source_changes() {
        let dir = temp_root("immutable");
        let store = Store::at(dir.join("store"));
        let src = dir.join("voice.wav");
        let mut first = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
        first.extend_from_slice(&[1; 4000]);
        std::fs::write(&src, &first).unwrap();

        let a = store.register(&src, Probe::default()).unwrap();
        assert_eq!((a.kind.as_str(), a.mime.as_str()), ("audio", "audio/wav"));
        assert_eq!(store.register(&src, Probe::default()).unwrap(), a);

        let mut second = first.clone();
        second.extend_from_slice(b"more");
        std::fs::write(&src, &second).unwrap();
        let b = store.register(&src, Probe { duration_ms: Some(2), ..Default::default() }).unwrap();
        assert_ne!(a.id, b.id);
        assert_ne!(a.sha256, b.sha256);

        std::fs::remove_file(&src).unwrap();
        assert_eq!(store.read_chunk(&a.id, 0, MAX_CHUNK).unwrap(), first);
        assert_eq!(store.read_chunk(&b.id, 0, MAX_CHUNK).unwrap(), second);

        // 같은 바이트, 다른 이름 — 기록은 따로, 원본 사본은 하나.
        let renamed = dir.join("again.wav");
        std::fs::write(&renamed, &first).unwrap();
        let c = store.register(&renamed, Probe::default()).unwrap();
        assert_ne!(c.id, a.id);
        assert_eq!(store.media_path(&c.id).unwrap(), store.media_path(&a.id).unwrap());
        assert_eq!(std::fs::read_dir(dir.join("store/blobs")).unwrap().count(), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn source_mutation_during_copy_is_refused() {
        let dir = temp_root("mutation");
        let store = Store::at(dir.join("store"));
        let src = dir.join("clip.mov");
        std::fs::write(&src, mp4_bytes(3 * COPY_BUF, 2)).unwrap();
        let mut grown = false;
        let err = store
            .ingest(&src, Probe::default(), &mut || {
                if !grown {
                    grown = true;
                    let mut f = OpenOptions::new().append(true).open(&src).unwrap();
                    f.write_all(b"appended while copying").unwrap();
                }
            })
            .unwrap_err();
        assert_eq!(err.to_string(), "media file changed while registering");
        assert_eq!(std::fs::read_dir(dir.join("store/tmp")).unwrap().count(), 0);
        assert_eq!(std::fs::read_dir(dir.join("store/meta")).unwrap().count(), 0);
        assert_eq!(std::fs::read_dir(dir.join("store/blobs")).unwrap().count(), 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unsupported_or_disguised_files_and_bad_probes_are_rejected() {
        let dir = temp_root("reject");
        let store = Store::at(dir.join("store"));
        let write = |name: &str, bytes: &[u8]| {
            let path = dir.join(name);
            std::fs::write(&path, bytes).unwrap();
            path
        };
        let kind = |r: io::Result<Media>| r.unwrap_err().kind();
        let video = write("ok.mp4", &mp4_bytes(4096, 3));
        let audio = write("ok.mp3", b"ID3\x04\0\0\0\0\0\0rest");

        assert_eq!(kind(store.register(&write("secret.txt", b"ID3 text"), Probe::default())), io::ErrorKind::InvalidInput);
        assert_eq!(kind(store.register(&write("noext", b"ID3"), Probe::default())), io::ErrorKind::InvalidInput);
        assert_eq!(kind(store.register(&write("fake.mp4", b"#!/bin/sh\necho hi\n"), Probe::default())), io::ErrorKind::InvalidInput);
        assert_eq!(kind(store.register(&write("empty.mp3", b""), Probe::default())), io::ErrorKind::InvalidInput);
        assert_eq!(kind(store.register(&dir, Probe::default())), io::ErrorKind::InvalidInput);
        assert_eq!(kind(store.register(&dir.join("missing.mp4"), Probe::default())), io::ErrorKind::NotFound);

        let huge = dir.join("huge.mp4");
        let f = File::create(&huge).unwrap();
        f.set_len(MAX_FILE_BYTES + 1).unwrap();
        assert_eq!(kind(store.register(&huge, Probe::default())), io::ErrorKind::InvalidInput);
        std::fs::remove_file(&huge).unwrap();

        let bad = [
            Probe { duration_ms: Some(0), ..Default::default() },
            Probe { duration_ms: Some(MAX_DURATION_MS + 1), ..Default::default() },
            Probe { width: Some(640), ..Default::default() },
            Probe { width: Some(0), height: Some(10), ..Default::default() },
            Probe { width: Some(MAX_SIDE + 1), height: Some(10), ..Default::default() },
            Probe { poster: Some(png(MAX_POSTER_SIDE + 1, 10)), ..Default::default() },
            Probe { poster: Some(png(0, 10)), ..Default::default() },
            Probe { poster: Some(b"GIF89a not png at all".to_vec()), ..Default::default() },
            Probe { poster: Some(vec![0; MAX_POSTER_BYTES + 1]), ..Default::default() },
        ];
        for probe in bad {
            assert_eq!(kind(store.register(&video, probe.clone())), io::ErrorKind::InvalidInput, "{probe:?}");
        }
        assert_eq!(kind(store.register(&audio, Probe { width: Some(1), height: Some(1), ..Default::default() })), io::ErrorKind::InvalidInput);
        assert_eq!(kind(store.register(&audio, Probe { poster: Some(png(10, 10)), ..Default::default() })), io::ErrorKind::InvalidInput);

        let edge = store
            .register(&video, Probe { poster: Some(png(MAX_POSTER_SIDE, MAX_POSTER_SIDE)), ..Default::default() })
            .unwrap();
        assert!(edge.poster);
        let plain = store.register(&audio, Probe::default()).unwrap();
        assert!(!plain.poster);
        assert_eq!(store.read_poster(&plain.id).unwrap_err().kind(), io::ErrorKind::NotFound);
        assert_eq!(store.poster_path(&plain.id).unwrap_err().kind(), io::ErrorKind::NotFound);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn ids_offsets_and_lengths_are_validated() {
        let dir = temp_root("offsets");
        let store = Store::at(dir.join("store"));
        let src = dir.join("a.m4a");
        let bytes = mp4_bytes(1000, 4);
        std::fs::write(&src, &bytes).unwrap();
        let media = store.register(&src, Probe::default()).unwrap();
        let kind = |r: io::Result<Vec<u8>>| r.unwrap_err().kind();

        for id in ["", "../meta/x", "0123456789ABCDEF", "0123456789abcde", "0123456789abcdef0", "0123456789abcdeg"] {
            assert_eq!(store.metadata(id).unwrap_err().kind(), io::ErrorKind::InvalidInput, "{id}");
            assert_eq!(kind(store.read_chunk(id, 0, 1)), io::ErrorKind::InvalidInput);
        }
        assert_eq!(store.metadata("0000000000000000").unwrap_err().kind(), io::ErrorKind::NotFound);
        assert_eq!(kind(store.read_chunk("0000000000000000", 0, 1)), io::ErrorKind::NotFound);
        assert_eq!(kind(store.read_chunk(&media.id, 0, 0)), io::ErrorKind::InvalidInput);
        assert_eq!(kind(store.read_chunk(&media.id, 0, MAX_CHUNK + 1)), io::ErrorKind::InvalidInput);
        assert_eq!(kind(store.read_chunk(&media.id, 1000, 1)), io::ErrorKind::InvalidInput);
        assert_eq!(kind(store.read_chunk(&media.id, u64::MAX, 1)), io::ErrorKind::InvalidInput);
        assert_eq!(store.read_chunk(&media.id, 999, MAX_CHUNK).unwrap(), &bytes[999..]);
        assert_eq!(store.read_chunk(&media.id, 10, 5).unwrap(), &bytes[10..15]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn tampered_or_linked_cache_entries_are_not_served() {
        let dir = temp_root("tamper");
        let store = Store::at(dir.join("store"));
        let src = dir.join("a.flac");
        std::fs::write(&src, b"fLaC\0\0\0\x22rest of the stream").unwrap();
        let media = store.register(&src, Probe::default()).unwrap();
        let meta = dir.join("store/meta").join(format!("{}.json", media.id));
        let original = std::fs::read_to_string(&meta).unwrap();
        let rewrite = |text: &str| {
            let _ = std::fs::remove_file(&meta);
            std::fs::write(&meta, text).unwrap();
        };

        rewrite(&original.replace("a.flac", "b.flac"));
        assert_eq!(store.metadata(&media.id).unwrap_err().kind(), io::ErrorKind::InvalidData);
        rewrite(&original.replace("\"ext\":\"flac\"", "\"ext\":\"../../secret\""));
        assert!(store.media_path(&media.id).is_err());
        rewrite("{not json");
        assert!(store.read_chunk(&media.id, 0, 1).is_err());
        rewrite(&original);
        assert_eq!(store.metadata(&media.id).unwrap(), media);

        // 다시 등록하면 깨진 기록을 같은 내용으로 되세운다.
        rewrite("{not json");
        assert_eq!(store.register(&src, Probe::default()).unwrap(), media);
        assert_eq!(store.metadata(&media.id).unwrap(), media);

        #[cfg(unix)]
        {
            let outside = dir.join("outside.json");
            std::fs::write(&outside, &original).unwrap();
            std::fs::remove_file(&meta).unwrap();
            std::os::unix::fs::symlink(&outside, &meta).unwrap();
            assert_eq!(store.metadata(&media.id).unwrap_err().kind(), io::ErrorKind::InvalidData);
            std::fs::remove_file(&meta).unwrap();
            std::fs::write(&meta, &original).unwrap();

            let blob = store.media_path(&media.id).unwrap();
            let secret = dir.join("secret.flac");
            std::fs::write(&secret, std::fs::read(&blob).unwrap()).unwrap();
            std::fs::remove_file(&blob).unwrap();
            std::os::unix::fs::symlink(&secret, &blob).unwrap();
            assert!(store.metadata(&media.id).is_err());
            assert!(store.read_chunk(&media.id, 0, 1).is_err());
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn default_root_is_separate_from_image_store() {
        assert_ne!(Some(root()), crate::inline_results::root());
    }
}
