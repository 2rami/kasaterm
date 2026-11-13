//! 디스크 지키기 — 데이터 볼륨의 남은 공간을 지켜보고, 다시 만들 수 있는 빌드 산출물만 스스로 치운다.
//!
//! 2026-11-13 맥북 데이터 볼륨이 0 이 되자 카사텀이 stderr 한 줄을 못 써 panic 하고 잠금 오염 연쇄로 죽었다.
//! 채운 것은 학생 격리 빌드의 cargo target·Xcode DerivedData·flutter build·`~/Library/Caches` 였다. 앱은 뒤 스레드
//! 하나로 1분마다 남은 공간을 재고(시스템 호출 하나), 문턱 아래거나 한 시간이 지나면 정리 한 바퀴를 돈다. 그리기는
//! [`status`] 로 마지막 값을 읽기만 한다. 규칙과 까닭은 `docs/disk-guard.md`.

#[cfg(unix)]
mod in_use;
#[cfg(unix)]
mod outputs;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
#[cfg(unix)]
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;

#[cfg(unix)]
pub use outputs::Kind;

const GIB: u64 = 1 << 30;
/// 설정 화면이 고르게 하는 값(GB).
pub const WARN_CHOICES_GIB: [u64; 3] = [10, 20, 40];
pub const CAP_CHOICES_GIB: [u64; 3] = [10, 15, 30];
pub const DEFAULT_WARN_GIB: u64 = 20;
pub const DEFAULT_CAP_GIB: u64 = 15;
const DEFAULT_CRITICAL_GIB: u64 = 5;
const DEFAULT_IDLE_MIN: u64 = 30;
/// 경고가 풀리려면 문턱보다 이만큼 더 비어야 한다 — 문턱 언저리를 오가며 같은 알림이 되풀이되지 않게.
#[cfg(unix)]
const HYSTERESIS: u64 = 2 * GIB;
/// 문턱 아래에서 치울 때는 문턱 위로 이만큼 올라설 때까지 — 다음 빌드 한 번에 다시 내려가지 않게.
#[cfg(unix)]
const PRESSURE_MARGIN: u64 = 5 * GIB;
#[cfg(unix)]
const TICK: Duration = Duration::from_secs(60);
#[cfg(unix)]
const ROUTINE_EVERY: Duration = Duration::from_secs(60 * 60);
#[cfg(unix)]
/// 앱이 막 켜졌을 때는 복원·빌드로 바쁘니 첫 정기 정리는 조금 미룬다. 문턱 아래면 바로 돈다.
const FIRST_ROUTINE_AFTER: Duration = Duration::from_secs(5 * 60);
#[cfg(unix)]
const PRESSURE_EVERY: Duration = Duration::from_secs(10 * 60);
#[cfg(unix)]
const PLACES_EVERY: Duration = Duration::from_secs(60 * 60);
#[cfg(unix)]
const PLACES_SHOWN: usize = 5;
#[cfg(unix)]
const LOG_MAX: u64 = 1 << 20;
/// 리그가 사이드바 경고를 그려 보려고 남은 공간을 속인다(GB).
const FAKE_FREE_ENV: &str = "KASATERM_DISK_FAKE_FREE_GIB";
#[cfg(unix)]
/// 시험·리그는 이 폴더들(`:` 로 구분)만 정리한다. 격리 실행은 이 값 없이는 아무것도 지우지 않는다.
const ROOTS_ENV: &str = "KASATERM_DISK_ROOTS";
#[cfg(unix)]
/// 릴리스 도구의 격리 굽기 — 제 정리가 따로 있어 손대지 않는다.
const RELEASE_STATE: &str = ".local/state/kasaterm-preview";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub warn_free: u64,
    pub critical_free: u64,
    pub build_cap: u64,
    pub idle: Duration,
    pub auto_prune: bool,
}

impl Config {
    pub fn from_settings(settings: &Value) -> Self {
        let number = |key: &str, default: u64| {
            settings.get(key).and_then(Value::as_u64).filter(|v| *v > 0).unwrap_or(default)
        };
        let warn_free = number("disk_warn_free_gib", DEFAULT_WARN_GIB).saturating_mul(GIB);
        Config {
            warn_free,
            critical_free: number("disk_critical_free_gib", DEFAULT_CRITICAL_GIB).saturating_mul(GIB).min(warn_free),
            build_cap: number("disk_build_cap_gib", DEFAULT_CAP_GIB).saturating_mul(GIB),
            idle: Duration::from_secs(number("disk_idle_minutes", DEFAULT_IDLE_MIN).saturating_mul(60)),
            auto_prune: settings.get("disk_auto_prune").and_then(Value::as_bool).unwrap_or(true),
        }
    }

    pub fn load() -> Self {
        let settings = settings_path()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or(Value::Null);
        Self::from_settings(&settings)
    }
}

/// 리그는 제 설정 파일 옆에 상태를 둔다 — 본판의 기록·잠금을 보지 않는다.
fn settings_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("KASATERM_SETTINGS_FILE").filter(|v| !v.is_empty()) {
        return Some(path.into());
    }
    Some(crate::home_dir()?.join(".config/kasaterm/settings.json"))
}

fn config_dir() -> Option<PathBuf> {
    settings_path()?.parent().map(Path::to_path_buf)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    #[default]
    Ok,
    Low,
    Critical,
}

pub fn level(free: u64, cfg: &Config) -> Level {
    if free < cfg.critical_free {
        Level::Critical
    } else if free < cfg.warn_free {
        Level::Low
    } else {
        Level::Ok
    }
}

/// 크기만 재서 보여 주는 곳. 자동으로 지우지 않는다.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Place {
    pub label: String,
    pub path: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PassSummary {
    pub at: u64,
    pub reason: &'static str,
    pub freed: u64,
    pub removed: usize,
    pub skipped: usize,
}

/// 사이드바·폰 허브·`/term/disk` 가 읽는 값.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Status {
    pub free: u64,
    pub total: u64,
    pub level: Level,
    pub warn_free: u64,
    pub critical_free: u64,
    pub auto_prune: bool,
    /// 경고 중에만 잰다 — 큰 폴더를 재는 일은 가볍지 않다.
    pub big: Vec<Place>,
    /// 마지막으로 무엇인가 지운 정리.
    pub last_pass: Option<PassSummary>,
}

pub fn size_label(bytes: u64) -> String {
    let gib = bytes as f64 / GIB as f64;
    if gib >= 10.0 { format!("{gib:.0}GB") } else { format!("{gib:.1}GB") }
}

impl Status {
    pub fn headline(&self) -> String {
        format!("디스크 {} 남음", size_label(self.free))
    }

    /// 큰 곳 앞 몇 개 — 「~/Library/Caches 126GB · /tmp 60GB」.
    pub fn big_line(&self, count: usize) -> String {
        self.big.iter().take(count).map(|p| format!("{} {}", p.label, size_label(p.bytes))).collect::<Vec<_>>().join(" · ")
    }

    /// 맥 알림·폰 푸시의 제목과 본문. 기기 이름이 있으면 제목 앞에 — 폰은 어느 기기인지 알아야 한다.
    pub fn alert_text(&self, machine: &str) -> (String, String) {
        let title = if machine.trim().is_empty() {
            self.headline()
        } else {
            format!("{} {}", machine.trim(), self.headline())
        };
        let mut body = Vec::new();
        if self.level == Level::Critical {
            body.push("곧 앱과 빌드가 멈출 수 있어요.".to_string());
        }
        if let Some(pass) = self.last_pass.as_ref().filter(|p| p.freed > 0) {
            body.push(format!("빌드 산출물 {}는 지웠어요.", size_label(pass.freed)));
        }
        if !self.big.is_empty() {
            body.push(format!("큰 곳: {}", self.big_line(3)));
        }
        if body.is_empty() {
            body.push("지울 수 있는 빌드 산출물이 없어요.".into());
        }
        (title, body.join(" "))
    }

    #[cfg(unix)]
    /// 화면에 보이는 값이 같은가 — 남은 공간은 0.1GB 단위로만 본다. 1분마다 다시 그리지 않게.
    fn shown_same(&self, other: &Status) -> bool {
        let tenth = GIB / 10;
        self.free / tenth == other.free / tenth
            && Status { free: 0, ..self.clone() } == Status { free: 0, ..other.clone() }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    Routine,
    Pressure,
    Build,
    Manual,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::Routine => "routine",
            Reason::Pressure => "pressure",
            Reason::Build => "build",
            Reason::Manual => "manual",
        }
    }
}

/// 지키미가 앱에 알리는 것. `Alert` 는 사람을 부를 일(문턱을 새로 넘음)이고 `Changed` 는 다시 그리기만.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Changed,
    Alert,
}

static PUBLISHED: Mutex<Option<Arc<Status>>> = Mutex::new(None);
static WAKE: OnceLock<Mutex<std::sync::mpsc::Sender<()>>> = OnceLock::new();

/// 지키미가 마지막으로 낸 값. 그리기 경로에서 불러도 된다(잠금 하나, 복제 없음).
pub fn status() -> Option<Arc<Status>> {
    PUBLISHED.lock().ok()?.clone()
}

/// HTTP·CLI 가 읽는 값. 지키미가 안 도는 프로세스는 그 자리에서 남은 공간만 잰다.
pub fn snapshot() -> Option<Status> {
    if let Some(status) = status() {
        return Some((*status).clone());
    }
    let cfg = Config::load();
    let (free, total) = free_space()?;
    Some(Status {
        free,
        total,
        level: level(free, &cfg),
        warn_free: cfg.warn_free,
        critical_free: cfg.critical_free,
        auto_prune: cfg.auto_prune,
        ..Status::default()
    })
}

/// 설정이 바뀌었을 때 — 다음 1분을 기다리지 않고 다시 잰다.
pub fn refresh() {
    if let Some(tx) = WAKE.get().and_then(|tx| tx.lock().ok()) {
        let _ = tx.send(());
    }
}

#[cfg(unix)]
fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

#[cfg(target_os = "macos")]
fn volume_space(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: 0 으로 채운 statfs 는 유효한 값이고, 아래 호출이 채운다.
    let mut fs: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: path 는 NUL 로 끝나는 C 문자열이고 fs 는 쓸 수 있는 구조체다.
    if unsafe { libc::statfs(path.as_ptr(), &mut fs) } != 0 {
        return None;
    }
    let block = u64::from(fs.f_bsize);
    Some((fs.f_bavail.saturating_mul(block), fs.f_blocks.saturating_mul(block)))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn volume_space(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: 0 으로 채운 statvfs 는 유효한 값이고, 아래 호출이 채운다.
    let mut fs: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: path 는 NUL 로 끝나는 C 문자열이고 fs 는 쓸 수 있는 구조체다.
    if unsafe { libc::statvfs(path.as_ptr(), &mut fs) } != 0 {
        return None;
    }
    let block = fs.f_frsize as u64;
    Some(((fs.f_bavail as u64).saturating_mul(block), (fs.f_blocks as u64).saturating_mul(block)))
}

#[cfg(not(unix))]
fn volume_space(_path: &Path) -> Option<(u64, u64)> {
    None
}

/// 홈이 있는 볼륨의 (남은, 전체) 바이트 — `df` 의 Avail 과 같다(APFS 의 지울 수 있는 공간은 빼고 본다).
pub fn free_space() -> Option<(u64, u64)> {
    let (free, total) = volume_space(&crate::home_dir()?)?;
    let fake = std::env::var(FAKE_FREE_ENV).ok().and_then(|v| v.trim().parse::<f64>().ok()).filter(|g| *g >= 0.0);
    Some((fake.map_or(free, |gib| (gib * GIB as f64) as u64), total))
}

#[cfg(unix)]
/// 격리 실행(검증 리그)인가 — 리그가 사람의 빌드 산출물을 지우면 안 된다.
fn isolated() -> bool {
    [
        "KASATERM_SETTINGS_FILE",
        "KASATERM_SESSION_FILE",
        "KASATERM_WINDOW_FILE",
        "KASATERM_AUTOQUIT_MS",
        "KASATERM_LITE_ROOT",
        "KASATERM_COLLAB_ROOT",
        "KASATERM_WINDOW_SIZE",
        "KASATERM_WINDOW_POS",
    ]
    .iter()
    .any(|key| std::env::var_os(key).is_some_and(|v| !v.is_empty()))
}

/// 기록 — 지운 것·건너뛴 것·까닭. 디스크가 꽉 차 못 써도 조용히 넘어간다(그것 때문에 죽은 적이 있다).
pub fn log_path() -> Option<PathBuf> {
    if std::env::var_os("KASATERM_SETTINGS_FILE").is_some_and(|v| !v.is_empty()) {
        return Some(config_dir()?.join("disk-guard.jsonl"));
    }
    let home = crate::home_dir()?;
    Some(if cfg!(target_os = "macos") {
        home.join("Library/Logs/kasaterm-disk-guard.jsonl")
    } else {
        home.join(".local/state/kasaterm/disk-guard.jsonl")
    })
}

#[cfg(unix)]
fn append_log(line: &impl Serialize) {
    use std::io::Write;
    let Some(path) = log_path() else { return };
    let Ok(text) = serde_json::to_string(line) else { return };
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > LOG_MAX) {
        let _ = std::fs::rename(&path, path.with_extension("jsonl.1"));
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "{text}");
    }
}

fn repos_path() -> Option<PathBuf> {
    Some(config_dir()?.join("disk-guard-repos.json"))
}

/// `scripts/build-app.sh` 가 구운 레포를 기억한다 — 앱의 정기 정리도 그 레포의 `target`·`mobile` 을 본다.
pub fn remember_repo(repo: &Path) {
    let Ok(repo) = std::fs::canonicalize(repo) else { return };
    let Some(path) = repos_path() else { return };
    let mut repos = remembered_repos();
    repos.retain(|r| *r != repo);
    repos.push(repo);
    let keep = repos.len().saturating_sub(20);
    let list: Vec<String> = repos[keep..].iter().map(|r| r.to_string_lossy().into_owned()).collect();
    if let Ok(text) = serde_json::to_vec_pretty(&list) {
        let _ = std::fs::write(path, text);
    }
}

fn remembered_repos() -> Vec<PathBuf> {
    repos_path()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<Vec<String>>(&bytes).ok())
        .unwrap_or_default()
        .into_iter()
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .collect()
}

#[cfg(unix)]
fn tmp_root() -> PathBuf {
    let private = PathBuf::from("/private/tmp");
    if private.is_dir() { private } else { PathBuf::from("/tmp") }
}

/// 자식 프로세스를 시간 안에서만 기다린다. 표준 출력은 따로 읽어 파이프가 차서 멈추지 않게 한다.
#[cfg(unix)]
fn output_within(cmd: &mut std::process::Command, limit: Duration) -> Option<std::process::Output> {
    use std::io::Read;
    use std::process::Stdio;
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let deadline = Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    Some(std::process::Output { status, stdout: reader.join().ok()?, stderr: Vec::new() })
}

#[cfg(unix)]
fn du_bytes(path: &Path) -> Option<u64> {
    let out = output_within(std::process::Command::new("du").arg("-skx").arg(path), Duration::from_secs(300))?;
    let kib: u64 = String::from_utf8_lossy(&out.stdout).split_whitespace().next()?.parse().ok()?;
    Some(kib * 1024)
}

/// 이 사용자의 임시 폴더 묶음(`/var/folders/xx/<id>`) — 맥미니에서 크롬의 서명 복제본이 여기에 69GB 쌓였다.
#[cfg(target_os = "macos")]
fn darwin_user_dir() -> Option<PathBuf> {
    let mut buf = vec![0u8; 1024];
    // SAFETY: buf 는 len 만큼 쓸 수 있고, confstr 은 NUL 로 끝나는 문자열을 넣는다.
    let n = unsafe { libc::confstr(libc::_CS_DARWIN_USER_TEMP_DIR, buf.as_mut_ptr().cast(), buf.len()) };
    if n == 0 || n > buf.len() {
        return None;
    }
    let temp = PathBuf::from(String::from_utf8_lossy(&buf[..n - 1]).into_owned());
    std::fs::canonicalize(temp.parent()?).ok()
}

/// 크기만 재는 곳. 다른 앱의 컨테이너·받은 파일·휴지통은 macOS 가 접근 허락을 물으니 재지 않는다.
#[cfg(unix)]
fn place_candidates(home: &Path) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = [
        "Library/Caches",
        "Library/Developer",
        "Library/Application Support",
        ".cache",
        ".cargo",
        ".rustup",
        RELEASE_STATE,
    ]
    .iter()
    .map(|rel| (format!("~/{rel}"), home.join(rel)))
    .collect();
    out.push(("/tmp".into(), tmp_root()));
    #[cfg(target_os = "macos")]
    if let Some(dir) = darwin_user_dir() {
        out.push(("/var/folders 임시".into(), dir));
    }
    out
}

/// 큰 곳 앞 다섯(1GB 이상).
#[cfg(unix)]
pub fn measure_places() -> Vec<Place> {
    let Some(home) = crate::home_dir() else { return Vec::new() };
    let mut places: Vec<Place> = place_candidates(&home)
        .into_iter()
        .filter(|(_, path)| path.is_dir())
        .filter_map(|(label, path)| {
            let bytes = du_bytes(&path)?;
            Some(Place { label, path: path.to_string_lossy().into_owned(), bytes })
        })
        .filter(|p| p.bytes >= GIB)
        .collect();
    places.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    places.truncate(PLACES_SHOWN);
    places
}

#[cfg(not(unix))]
pub fn measure_places() -> Vec<Place> {
    Vec::new()
}

#[cfg(unix)]
#[derive(Clone, Debug, Serialize)]
pub struct Removed {
    pub path: String,
    pub kind: Kind,
    pub bytes: u64,
}

#[cfg(unix)]
#[derive(Clone, Debug, Serialize)]
pub struct Skipped {
    pub path: String,
    pub kind: Kind,
    pub bytes: u64,
    pub why: String,
}

/// 정리 한 바퀴의 기록 — 그대로 기록 파일의 한 줄이 된다.
#[cfg(unix)]
#[derive(Clone, Debug, Default, Serialize)]
pub struct Report {
    pub at: u64,
    pub reason: &'static str,
    pub dry_run: bool,
    pub free_before: u64,
    pub free_after: u64,
    pub outputs: usize,
    pub outputs_bytes: u64,
    pub cap: u64,
    pub removed: Vec<Removed>,
    pub skipped: Vec<Skipped>,
    pub error: Option<String>,
}

#[cfg(unix)]
impl Report {
    pub fn freed(&self) -> u64 {
        self.removed.iter().map(|r| r.bytes).sum()
    }

    fn worth_logging(&self) -> bool {
        self.reason != Reason::Routine.as_str()
            || !self.removed.is_empty()
            || !self.skipped.is_empty()
            || self.error.is_some()
    }

    fn summary(&self) -> PassSummary {
        PassSummary {
            at: self.at,
            reason: self.reason,
            freed: self.freed(),
            removed: self.removed.len(),
            skipped: self.skipped.len(),
        }
    }
}

/// 어디를 보고 어디까지 지울 수 있나. `allowed` 밖·`deny` 안은 산출물 표식이 있어도 지우지 않는다.
#[cfg(unix)]
struct Scope {
    roots: Vec<outputs::Root>,
    derived_data: Option<PathBuf>,
    allowed: Vec<PathBuf>,
    deny: Vec<PathBuf>,
}

#[cfg(unix)]
impl Scope {
    fn current(extra_repos: &[PathBuf]) -> Result<Scope, &'static str> {
        let home = crate::home_dir().ok_or("홈 폴더를 몰라요")?;
        let deny = vec![outputs::real(&home.join(RELEASE_STATE))];
        if let Some(test) = std::env::var_os(ROOTS_ENV).filter(|v| !v.is_empty()) {
            let roots: Vec<PathBuf> = std::env::split_paths(&test).filter(|p| p.is_dir()).collect();
            return Ok(Scope::of(roots.into_iter().map(|path| outputs::Root { path, depth: 2 }).collect(), None, deny));
        }
        if isolated() {
            return Err("격리 실행이라 지우지 않아요");
        }
        let mut roots = Vec::new();
        if let Ok(entries) = std::fs::read_dir(tmp_root()) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let real_dir = entry.file_type().is_ok_and(|t| t.is_dir());
                if real_dir && (name.starts_with("kt-") || name.starts_with("kasaterm-")) {
                    roots.push(outputs::Root { path: entry.path(), depth: 2 });
                }
            }
        }
        roots.push(outputs::Root { path: home.join("Library/Caches/kasaterm-builds"), depth: 2 });
        for repo in remembered_repos().into_iter().chain(extra_repos.iter().cloned()) {
            roots.push(outputs::Root { path: repo, depth: 1 });
        }
        let derived = home.join("Library/Developer/Xcode/DerivedData");
        Ok(Scope::of(roots, Some(derived), deny))
    }

    fn of(roots: Vec<outputs::Root>, derived_data: Option<PathBuf>, deny: Vec<PathBuf>) -> Scope {
        let allowed = roots
            .iter()
            .map(|r| r.path.as_path())
            .chain(derived_data.as_deref())
            .filter_map(|p| std::fs::canonicalize(p).ok())
            .collect();
        Scope { roots, derived_data, allowed, deny }
    }
}

#[cfg(unix)]
struct Row {
    output: outputs::Output,
    real: PathBuf,
    bytes: u64,
    used: u64,
}

/// 얼마나 치울지. 평소에는 산출물 합을 상한 밑으로, 문턱 아래에서는 남은 공간이 문턱 위로 올라설 때까지.
#[cfg(unix)]
#[derive(Clone, Copy, Debug)]
enum Goal {
    Cap(u64),
    Free { free: u64, want: u64 },
}

#[cfg(unix)]
fn goal_for(free: u64, cfg: &Config) -> Goal {
    if free < cfg.warn_free {
        Goal::Free { free, want: cfg.warn_free.saturating_add(PRESSURE_MARGIN) }
    } else {
        Goal::Cap(cfg.build_cap)
    }
}

/// 오래 안 쓴 것부터 지울 차례를 정한다. 최근에 바뀐 것·누가 쓰는 것·아직 남을 다른 산출물이 링크로 가리키는
/// 것은 건너뛰고 까닭을 남긴다.
#[cfg(unix)]
fn plan(
    rows: &[Row],
    goal: Goal,
    idle_secs: u64,
    used: &std::collections::HashSet<PathBuf>,
    now: u64,
) -> (Vec<usize>, Vec<(usize, String)>) {
    let links: Vec<_> = rows
        .iter()
        .map(|r| if r.output.kind == Kind::Cargo { outputs::links_into(&r.output.path) } else { Default::default() })
        .collect();
    let referrers: Vec<Vec<usize>> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            (0..rows.len()).filter(|&j| j != i && links[j].iter().any(|l| l.starts_with(&row.real))).collect()
        })
        .collect();
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by_key(|&i| rows[i].used);
    let mut total: u64 = rows.iter().map(|r| r.bytes).sum();
    let mut gained = 0u64;
    let (mut doomed, mut skipped) = (Vec::new(), Vec::new());
    for i in order {
        let enough = match goal {
            Goal::Cap(cap) => total <= cap,
            Goal::Free { free, want } => free.saturating_add(gained) >= want,
        };
        if enough {
            break;
        }
        let row = &rows[i];
        let age = now.saturating_sub(row.used);
        let why = if age < idle_secs {
            Some(format!("{}분 전에 바뀌었어요", age / 60))
        } else if in_use::in_use(&row.output, used) {
            Some("쓰는 중이에요".to_string())
        } else if referrers[i].iter().any(|j| !doomed.contains(j)) {
            Some("남는 산출물이 링크로 가리켜요".to_string())
        } else {
            None
        };
        match why {
            Some(why) => skipped.push((i, why)),
            None => {
                doomed.push(i);
                total = total.saturating_sub(row.bytes);
                gained = gained.saturating_add(row.bytes);
            }
        }
    }
    (doomed, skipped)
}

#[cfg(unix)]
fn run_in(
    cfg: &Config,
    reason: Reason,
    scope: &Scope,
    dry_run: bool,
    measure: &dyn Fn(&Path) -> u64,
    used_paths: &dyn Fn() -> Result<std::collections::HashSet<PathBuf>, in_use::Unknown>,
    free_now: &dyn Fn() -> Option<u64>,
) -> Report {
    let at = now_secs();
    let free_before = free_now().unwrap_or(0);
    let mut report = Report { at, reason: reason.as_str(), dry_run, free_before, free_after: free_before, cap: cfg.build_cap, ..Report::default() };
    let rows: Vec<Row> = outputs::find(&scope.roots, scope.derived_data.as_deref())
        .into_iter()
        .map(|output| Row { real: outputs::real(&output.path), bytes: measure(&output.path), used: outputs::last_used(&output.path), output })
        .collect();
    report.outputs = rows.len();
    report.outputs_bytes = rows.iter().map(|r| r.bytes).sum();
    let goal = goal_for(free_before, cfg);
    let satisfied = match goal {
        Goal::Cap(cap) => report.outputs_bytes <= cap,
        Goal::Free { free, want } => free >= want,
    };
    if satisfied || rows.is_empty() {
        return report;
    }
    let used = match used_paths() {
        Ok(used) => used,
        Err(in_use::Unknown(why)) => {
            report.error = Some(format!("누가 무엇을 쓰는지 몰라 지우지 않았어요 — {why}"));
            return report;
        }
    };
    let idle = cfg.idle.as_secs();
    let (doomed, skipped) = plan(&rows, goal, idle, &used, at);
    let entry = |row: &Row| (row.output.path.to_string_lossy().into_owned(), row.output.kind, row.bytes);
    report.skipped = skipped
        .into_iter()
        .map(|(i, why)| {
            let (path, kind, bytes) = entry(&rows[i]);
            Skipped { path, kind, bytes, why }
        })
        .collect();
    for i in doomed {
        let row = &rows[i];
        let (path, kind, bytes) = entry(row);
        if let Goal::Free { want, .. } = goal {
            if !dry_run && free_now().is_some_and(|free| free >= want) {
                break;
            }
        }
        if dry_run {
            report.removed.push(Removed { path, kind, bytes });
            continue;
        }
        // 지우기 직전 다시 잰다 — 계획 뒤에 시작한 빌드가 그 산출물을 잡았을 수 있다.
        let fresh = match used_paths() {
            Ok(fresh) => fresh,
            Err(in_use::Unknown(why)) => {
                report.error = Some(format!("다시 재지 못해 멈췄어요 — {why}"));
                break;
            }
        };
        let moved = outputs::real(&row.output.path) != row.real || outputs::last_used(&row.output.path) != row.used;
        if moved || in_use::in_use(&row.output, &fresh) {
            report.skipped.push(Skipped { path, kind, bytes, why: "다시 재니 쓰는 중이에요".into() });
            continue;
        }
        match outputs::remove(&row.output, &scope.allowed, &scope.deny) {
            Ok(()) => report.removed.push(Removed { path, kind, bytes }),
            Err(why) => report.skipped.push(Skipped { path, kind, bytes, why: why.into() }),
        }
    }
    report.free_after = free_now().unwrap_or(free_before);
    report
}

/// 잠금 — 앱과 `kasaterm-cli disk prune` 이 같은 산출물을 함께 지우지 않게 한 번에 하나만 돈다.
#[cfg(unix)]
fn lock() -> Option<std::fs::File> {
    use std::os::unix::io::AsRawFd;
    let dir = config_dir()?;
    let _ = std::fs::create_dir_all(&dir);
    let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("disk-guard.lock")).ok()?;
    // SAFETY: 열린 파일의 기술자다. 잠금은 file 이 닫힐 때 풀린다.
    (unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0).then_some(file)
}

/// 정리 한 바퀴. `extra_repos` 는 이번에 함께 볼 레포(빌드 직후의 레포). 기록은 부른 쪽이 남긴다.
#[cfg(unix)]
pub fn run_pass(cfg: &Config, reason: Reason, extra_repos: &[PathBuf], dry_run: bool) -> Report {
    let free_now = || free_space().map(|(free, _)| free);
    let refused = |why: &str| Report {
        at: now_secs(),
        reason: reason.as_str(),
        dry_run,
        free_before: free_now().unwrap_or(0),
        free_after: free_now().unwrap_or(0),
        cap: cfg.build_cap,
        error: Some(why.to_string()),
        ..Report::default()
    };
    let scope = match Scope::current(extra_repos) {
        Ok(scope) => scope,
        Err(why) => return refused(why),
    };
    let Some(_lock) = lock() else { return refused("다른 정리가 도는 중이에요") };
    run_in(cfg, reason, &scope, dry_run, &|p| du_bytes(p).unwrap_or(0), &in_use::used_paths, &free_now)
}

/// 정리하고 기록한다 — 앱과 CLI 가 같은 길로 남긴다.
#[cfg(unix)]
pub fn run_and_log(cfg: &Config, reason: Reason, extra_repos: &[PathBuf], dry_run: bool) -> Report {
    let report = run_pass(cfg, reason, extra_repos, dry_run);
    if report.worth_logging() && !dry_run {
        append_log(&report);
    }
    report
}

/// 지키미를 세운다. 이미 섰으면 false. `on_event` 는 지키미 스레드에서 불린다 — 앱은 이벤트 루프로 넘기기만 한다.
pub fn spawn(on_event: impl Fn(Event) + Send + 'static) -> bool {
    #[cfg(unix)]
    {
        let (tx, rx) = std::sync::mpsc::channel();
        if WAKE.set(Mutex::new(tx)).is_err() {
            return false;
        }
        std::thread::Builder::new().name("disk-guard".into()).spawn(move || watch(&rx, &on_event)).is_ok()
    }
    #[cfg(not(unix))]
    {
        let _ = on_event;
        false
    }
}

#[cfg(unix)]
fn publish(status: Status) -> bool {
    let Ok(mut slot) = PUBLISHED.lock() else { return false };
    let changed = slot.as_deref().is_none_or(|old| !old.shown_same(&status));
    *slot = Some(Arc::new(status));
    changed
}

#[cfg(unix)]
/// 알림을 낼 수준. 문턱을 새로 넘을 때만 사람을 부르고, 문턱 위로 넉넉히 올라서야 다시 부를 수 있다.
fn next_alert(alerted: Level, free: u64, cfg: &Config) -> (Level, bool) {
    let now = level(free, cfg);
    let relaxed = alerted.min(level(free.saturating_sub(HYSTERESIS), cfg));
    if now > relaxed { (now, true) } else { (relaxed, false) }
}

#[cfg(unix)]
fn watch(rx: &std::sync::mpsc::Receiver<()>, on_event: &dyn Fn(Event)) {
    use std::sync::mpsc::RecvTimeoutError;
    let started = Instant::now();
    let mut wait = Duration::from_secs(2);
    let (mut last_routine, mut last_pressure): (Option<Instant>, Option<Instant>) = (None, None);
    let mut places: Option<(Instant, Level)> = None;
    let mut alerted = Level::Ok;
    let mut big: Vec<Place> = Vec::new();
    let mut last_pass: Option<PassSummary> = None;
    loop {
        match rx.recv_timeout(wait) {
            Ok(()) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        wait = TICK;
        let cfg = Config::load();
        let Some((mut free, total)) = free_space() else { continue };
        let low = level(free, &cfg) != Level::Ok;
        let due = |last: Option<Instant>, every: Duration, first: Duration| {
            last.map_or(started.elapsed() >= first, |t| t.elapsed() >= every)
        };
        let reason = if low && due(last_pressure, PRESSURE_EVERY, Duration::ZERO) {
            Some(Reason::Pressure)
        } else {
            due(last_routine, ROUTINE_EVERY, FIRST_ROUTINE_AFTER).then_some(Reason::Routine)
        };
        if let Some(reason) = reason {
            last_routine = Some(Instant::now());
            if low {
                last_pressure = last_routine;
            }
            if cfg.auto_prune {
                let report = run_and_log(&cfg, reason, &[], false);
                if !report.removed.is_empty() {
                    last_pass = Some(report.summary());
                }
                free = free_space().map_or(free, |(f, _)| f);
            }
        }
        let now = level(free, &cfg);
        let status = |big: &[Place]| Status {
            free,
            total,
            level: now,
            warn_free: cfg.warn_free,
            critical_free: cfg.critical_free,
            auto_prune: cfg.auto_prune,
            big: big.to_vec(),
            last_pass: last_pass.clone(),
        };
        // 카드는 곧바로 세운다 — 큰 곳 재기(`du`)는 수십 초가 걸릴 수 있다. 알림은 큰 곳을 잰 뒤에 그것을 싣고 간다.
        if now == Level::Ok {
            big.clear();
            places = None;
        }
        let mut changed = publish(status(&big));
        if now != Level::Ok && places.is_none_or(|(at, seen)| at.elapsed() >= PLACES_EVERY || now > seen) {
            if changed {
                on_event(Event::Changed);
            }
            big = measure_places();
            places = Some((Instant::now(), now));
            changed = publish(status(&big));
        }
        let (next, alert) = next_alert(alerted, free, &cfg);
        alerted = next;
        if alert {
            append_log(&serde_json::json!({"at": now_secs(), "event": "alert", "level": now, "free": free, "big": big}));
            on_event(Event::Alert);
        } else if changed {
            on_event(Event::Changed);
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::collections::HashSet;

    const NOW: u64 = 2_000_000_000;

    fn cfg() -> Config {
        Config::from_settings(&Value::Null)
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kasa-disk-pass-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        outputs::real(&dir)
    }

    fn target(at: &Path) -> PathBuf {
        std::fs::create_dir_all(at.join("debug")).unwrap();
        std::fs::write(at.join("CACHEDIR.TAG"), outputs::CARGO_TAG).unwrap();
        at.to_path_buf()
    }

    fn row(path: &Path, age_h: u64) -> Row {
        let output = outputs::Output { path: path.to_path_buf(), kind: Kind::Cargo, workspace: path.parent().unwrap().to_path_buf() };
        Row { real: outputs::real(path), output, bytes: 10 * GIB, used: NOW - age_h * 3600 }
    }

    fn doomed(rows: &[Row], goal: Goal, used: &HashSet<PathBuf>) -> Vec<PathBuf> {
        plan(rows, goal, 30 * 60, used, NOW).0.into_iter().map(|i| rows[i].output.path.clone()).collect()
    }

    #[test]
    fn settings_choose_thresholds_and_critical_never_exceeds_warning() {
        let c = cfg();
        assert_eq!((c.warn_free, c.critical_free, c.build_cap), (20 * GIB, 5 * GIB, 15 * GIB));
        assert!(c.auto_prune);
        let c = Config::from_settings(&serde_json::json!({
            "disk_warn_free_gib": 4, "disk_build_cap_gib": 30, "disk_auto_prune": false, "disk_idle_minutes": 0
        }));
        assert_eq!((c.warn_free, c.critical_free, c.build_cap, c.auto_prune), (4 * GIB, 4 * GIB, 30 * GIB, false));
        assert_eq!(c.idle, Duration::from_secs(30 * 60));
    }

    #[test]
    fn over_the_cap_the_longest_unused_go_first_until_under() {
        let root = scratch("cap");
        let (old, mid, new) = (target(&root.join("old/target")), target(&root.join("mid/target")), target(&root.join("new/target")));
        let rows = [row(&new, 2), row(&old, 9), row(&mid, 5)];
        assert_eq!(doomed(&rows, Goal::Cap(15 * GIB), &HashSet::new()), vec![old, mid]);
        assert!(doomed(&rows, Goal::Cap(30 * GIB), &HashSet::new()).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn under_pressure_the_cap_is_ignored_until_free_space_recovers() {
        let root = scratch("pressure");
        let (a, b, c) = (target(&root.join("a/target")), target(&root.join("b/target")), target(&root.join("c/target")));
        let rows = [row(&a, 9), row(&b, 8), row(&c, 7)];
        let goal = Goal::Free { free: 3 * GIB, want: 25 * GIB };
        assert_eq!(doomed(&rows, goal, &HashSet::new()), vec![a, b, c.clone()]);
        let recent = [row(&c, 0)];
        assert!(doomed(&recent, goal, &HashSet::new()).is_empty(), "a target touched minutes ago stays even under pressure");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_build_seen_through_an_alias_or_a_running_binary_protects_its_target() {
        let root = scratch("alias");
        let kt = target(&root.join("kt-x/target"));
        let idle = target(&root.join("idle/target"));
        let alias = root.join("tmp");
        std::os::unix::fs::symlink(&root, &alias).unwrap();
        let seen: HashSet<PathBuf> = in_use::env_paths(&format!("cargo build PWD={}/kt-x", alias.display()))
            .iter()
            .map(|p| outputs::real(p))
            .collect();
        assert!(doomed(&[row(&kt, 9)], Goal::Cap(GIB), &seen).is_empty());
        let running: HashSet<PathBuf> = in_use::lsof_paths(&format!("p1\nftxt\nn{}/debug/kasaterm\n", kt.display()));
        assert_eq!(doomed(&[row(&kt, 9), row(&idle, 8)], Goal::Cap(5 * GIB), &running), vec![idle]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_target_another_links_into_stays_until_that_one_goes_too() {
        let root = scratch("links");
        let shared = target(&root.join("tell-mobile/source/target"));
        std::fs::create_dir_all(shared.join("ios")).unwrap();
        let sender = target(&root.join("report-senders/source/target"));
        std::os::unix::fs::symlink(shared.join("ios"), sender.join("ios")).unwrap();
        let rows = [row(&shared, 9), row(&sender, 5)];
        for cap in [15, 1] {
            assert_eq!(doomed(&rows, Goal::Cap(cap * GIB), &HashSet::new()), vec![sender.clone()]);
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_pass_removes_idle_outputs_inside_scope_and_logs_why_others_stay() {
        let root = scratch("run");
        let old = target(&root.join("kt-old/target"));
        let busy = target(&root.join("kt-busy/target"));
        let mobile = root.join("kt-old/mobile");
        std::fs::create_dir_all(mobile.join("build/ios")).unwrap();
        std::fs::write(mobile.join("pubspec.yaml"), "name: x\n").unwrap();
        let scope = Scope::of(vec![outputs::Root { path: root.clone(), depth: 3 }], None, vec![]);
        let busy_ws = outputs::real(&root.join("kt-busy"));
        let used = move || Ok(HashSet::from([busy_ws.join("src")]));
        let mut c = cfg();
        c.build_cap = 0;
        c.idle = Duration::ZERO;
        let report = run_in(&c, Reason::Manual, &scope, false, &|_| GIB, &used, &|| Some(100 * GIB));
        let removed: HashSet<_> = report.removed.iter().map(|r| PathBuf::from(&r.path)).collect();
        assert_eq!(removed, HashSet::from([old.clone(), mobile.join("build")]));
        assert!(!old.exists() && !mobile.join("build").exists() && mobile.join("pubspec.yaml").exists());
        assert!(busy.exists());
        assert_eq!(report.skipped.len(), 1);
        assert_eq!(report.skipped[0].why, "쓰는 중이에요");
        let unknown = || Err(in_use::Unknown("ps 를 못 읽었어요"));
        let left = run_in(&c, Reason::Manual, &scope, false, &|_| GIB, &unknown, &|| Some(100 * GIB));
        assert!(left.removed.is_empty() && left.error.is_some() && busy.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn alerts_fire_once_per_new_level_and_rearm_only_well_above_the_threshold() {
        let c = cfg();
        let mut alerted = Level::Ok;
        let mut fired = Vec::new();
        for free_gib in [30, 19, 18, 21, 19, 4, 6, 4, 23, 19] {
            let (next, alert) = next_alert(alerted, free_gib * GIB, &c);
            alerted = next;
            if alert {
                fired.push(free_gib);
            }
        }
        assert_eq!(fired, vec![19, 4, 19]);
    }

    #[test]
    fn alert_text_names_the_machine_what_was_freed_and_the_big_places() {
        let status = Status {
            free: 9 * GIB / 2,
            level: Level::Critical,
            big: vec![Place { label: "~/Library/Caches".into(), path: String::new(), bytes: 126 * GIB }],
            last_pass: Some(PassSummary { at: 0, reason: "pressure", freed: 12 * GIB, removed: 2, skipped: 0 }),
            ..Status::default()
        };
        let (title, body) = status.alert_text("맥미니");
        assert_eq!(title, "맥미니 디스크 4.5GB 남음");
        assert_eq!(body, "곧 앱과 빌드가 멈출 수 있어요. 빌드 산출물 12GB는 지웠어요. 큰 곳: ~/Library/Caches 126GB");
    }
}
