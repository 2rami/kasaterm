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
#[cfg(unix)]
mod prune;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[cfg(unix)]
pub use outputs::Kind;
#[cfg(unix)]
pub use prune::{run_and_log, run_pass, Removed, Report, Skipped};

const GIB: u64 = 1 << 30;
/// 설정 화면이 고르게 하는 값(GB).
pub const WARN_CHOICES_GIB: [u64; 3] = [10, 20, 40];
pub const CAP_CHOICES_GIB: [u64; 3] = [10, 15, 30];
pub const DEFAULT_WARN_GIB: u64 = 20;
pub const DEFAULT_CAP_GIB: u64 = 15;
const DEFAULT_CRITICAL_GIB: u64 = 5;
const DEFAULT_IDLE_MIN: u64 = 30;
/// 경고가 풀리려면 문턱보다 이만큼 더 비어야 한다 — 문턱 언저리를 오가며 같은 알림이 되풀이되지 않게.
const HYSTERESIS: u64 = 2 * GIB;
/// 문턱 아래에서 치울 때는 문턱 위로 이만큼 올라설 때까지 — 다음 빌드 한 번에 다시 내려가지 않게.
#[cfg(unix)]
const PRESSURE_MARGIN: u64 = 5 * GIB;
const TICK: Duration = Duration::from_secs(60);
#[cfg(unix)]
const ROUTINE_EVERY: Duration = Duration::from_secs(60 * 60);
#[cfg(unix)]
/// 앱이 막 켜졌을 때는 복원·빌드로 바쁘니 첫 정기 정리는 조금 미룬다. 문턱 아래면 바로 돈다.
const FIRST_ROUTINE_AFTER: Duration = Duration::from_secs(5 * 60);
#[cfg(unix)]
const PRESSURE_EVERY: Duration = Duration::from_secs(10 * 60);
const PLACES_EVERY: Duration = Duration::from_secs(60 * 60);
#[cfg(unix)]
const PLACES_SHOWN: usize = 5;
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Place {
    pub label: String,
    pub path: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PassSummary {
    pub at: u64,
    pub reason: String,
    pub freed: u64,
    pub removed: usize,
    pub skipped: usize,
}

/// 사이드바·폰 허브·`/term/disk` 가 읽는 값. 다른 기기의 값도 같은 꼴로 받는다(모르는 칸은 기본값).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
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

#[cfg(windows)]
fn volume_space(path: &Path) -> Option<(u64, u64)> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
    // SAFETY: wide 는 NUL 로 끝나는 경로이고 세 출력 칸은 이 함수가 쥔 지역 변수다.
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut avail, &mut total, &mut free) };
    (ok != 0).then_some((avail, total))
}

#[cfg(not(any(unix, windows)))]
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

/// 지키미를 세운다. 이미 섰으면 false. `on_event` 는 지키미 스레드에서 불린다 — 앱은 이벤트 루프로 넘기기만 한다.
/// Windows 는 재고 알리기만 한다(정리는 docs/disk-guard.md 「Windows」).
pub fn spawn(on_event: impl Fn(Event) + Send + 'static) -> bool {
    let (tx, rx) = std::sync::mpsc::channel();
    if WAKE.set(Mutex::new(tx)).is_err() {
        return false;
    }
    std::thread::Builder::new().name("disk-guard".into()).spawn(move || watch(&rx, &on_event)).is_ok()
}

fn publish(status: Status) -> bool {
    let Ok(mut slot) = PUBLISHED.lock() else { return false };
    let changed = slot.as_deref().is_none_or(|old| !old.shown_same(&status));
    *slot = Some(Arc::new(status));
    changed
}

/// 알림을 낼 수준. 문턱을 새로 넘을 때만 사람을 부르고, 문턱 위로 넉넉히 올라서야 다시 부를 수 있다.
fn next_alert(alerted: Level, free: u64, warn: u64, critical: u64) -> (Level, bool) {
    let at = |free: u64| if free < critical { Level::Critical } else if free < warn { Level::Low } else { Level::Ok };
    let now = at(free);
    let relaxed = alerted.min(at(free.saturating_sub(HYSTERESIS)));
    if now > relaxed { (now, true) } else { (relaxed, false) }
}

impl Status {
    /// 다른 기기의 값으로 같은 판정을 한다 — 그 기기가 알린 문턱 그대로. `(다음 기억, 지금 부를까)`.
    pub fn alert_step(&self, alerted: Level) -> (Level, bool) {
        next_alert(alerted, self.free, self.warn_free, self.critical_free)
    }
}

fn watch(rx: &std::sync::mpsc::Receiver<()>, on_event: &dyn Fn(Event)) {
    use std::sync::mpsc::RecvTimeoutError;
    let mut wait = Duration::from_secs(2);
    #[cfg(unix)]
    let mut schedule = prune::Schedule::new();
    let mut places: Option<(Instant, Level)> = None;
    let mut alerted = Level::Ok;
    let mut big: Vec<Place> = Vec::new();
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut last_pass: Option<PassSummary> = None;
    loop {
        match rx.recv_timeout(wait) {
            Ok(()) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        wait = TICK;
        let cfg = Config::load();
        #[cfg_attr(not(unix), allow(unused_mut))]
        let Some((mut free, total)) = free_space() else { continue };
        #[cfg(unix)]
        if let Some(pass) = schedule.tick(&cfg, free) {
            if pass.removed > 0 {
                last_pass = Some(pass);
            }
            free = free_space().map_or(free, |(f, _)| f);
        }
        let now = level(free, &cfg);
        let status = |big: &[Place]| Status {
            free,
            total,
            level: now,
            warn_free: cfg.warn_free,
            critical_free: cfg.critical_free,
            auto_prune: cfg.auto_prune && cfg!(unix),
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
        let (next, alert) = next_alert(alerted, free, cfg.warn_free, cfg.critical_free);
        alerted = next;
        if alert {
            append_log(&serde_json::json!({"at": now_secs(), "event": "alert", "level": now, "free": free, "big": big}));
            on_event(Event::Alert);
        } else if changed {
            on_event(Event::Changed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config::from_settings(&Value::Null)
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
    fn alerts_fire_once_per_new_level_and_rearm_only_well_above_the_threshold() {
        let c = cfg();
        let mut alerted = Level::Ok;
        let mut fired = Vec::new();
        for free_gib in [30, 19, 18, 21, 19, 4, 6, 4, 23, 19] {
            let (next, alert) = next_alert(alerted, free_gib * GIB, c.warn_free, c.critical_free);
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
            last_pass: Some(PassSummary { at: 0, reason: "pressure".into(), freed: 12 * GIB, removed: 2, skipped: 0 }),
            ..Status::default()
        };
        let (title, body) = status.alert_text("맥미니");
        assert_eq!(title, "맥미니 디스크 4.5GB 남음");
        assert_eq!(body, "곧 앱과 빌드가 멈출 수 있어요. 빌드 산출물 12GB는 지웠어요. 큰 곳: ~/Library/Caches 126GB");
    }
}
