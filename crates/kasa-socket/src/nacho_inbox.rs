//! 나쵸네코(오케스트레이터) 인박스 — 나쵸가 띄운 학생이 끝나거나 막혔을 때 넣는
//! **구조화 보고** 한 통의 저장·중복 판정·깨우기.
//!
//! 왜 tell 이 아니라 인박스인가: 나쵸는 pane 이 아니라 상주 프로세스라 tell 이 닿을
//! 입력창이 없다. 그리고 보고는 나쵸가 **죽어 있어도** 남아야 한다(재기동 뒤 소비).
//! 그래서 디스크가 정본이고, 깨우기는 「살아 있으면 지금」이라는 덤일 뿐이다.
//!
//! 설계 불변식:
//! - **원자적 기록** — `tmp/` 에 쓰고 fsync 한 뒤 `new/` 로 rename 한다. 읽는 쪽이
//!   반쪽 파일을 볼 수 없다.
//! - **중복 지문** — 본문 필드(origin·conv·task·surface·status·summary·changed·
//!   tests·next)의 FNV-1a-64. `new/`·`done/` 어느 쪽에든 같은 지문이 있으면 **다시
//!   쓰지 않고** 기존 영수증을 돌려준다. 학생이 같은 보고를 두 번 쳐도 나쵸는 한 번
//!   깬다(자동 후속 폭주 차단의 첫 관문).
//! - **비밀 금지** — 토큰처럼 보이는 문자열이 있으면 기록 자체를 거부한다. 인박스는
//!   나쵸의 대화 프롬프트로 그대로 올라가므로 여기서 새면 모델 컨텍스트로 샌다.
//! - **origin 게이트** — `origin` 이 `nacho` 가 아니면 거부. 거노가 손수 띄운 학생이
//!   나쵸에게 함부로 보고하지 않게 하는 서버 쪽 관문이다(CLI 는 env 로 한 번 더 거른다).
//! - 깨우기는 `wake.sock`(나쵸가 여는 유닉스 소켓)에 한 줄 보내고 ack 를 기다린다.
//!   소켓이 없거나 안 받으면 `queued` — 나쵸가 다음 부팅·폴링에서 집는다. 시그널은
//!   쓰지 않는다: 핸들러 없는 옛 나쵸에 SIGUSR1 을 보내면 그 자리에서 죽는다.

use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const SCHEMA: &str = "nacho-report/1";
pub const ORIGIN: &str = "nacho";
pub const STATUSES: [&str; 4] = ["done", "blocked", "needs_restart", "needs_approval"];
/// 봉투 전체(JSON) 상한. tell 본문과 같은 급.
pub const MAX_BYTES: usize = 16 * 1024;
/// `done/` 에 남긴 소비 완료 사본을 며칠 뒤 지우나 — 중복 지문의 기억 창이기도 하다.
pub const DONE_RETENTION_MS: u64 = 48 * 60 * 60 * 1000;

/// 학생 pane 에 심는 origin 표식. 부팅 명령(`KASATERM_ORIGIN=nacho … claude`)이 박고
/// 셰임·하네스가 그대로 물려준다. 이 넷이 곧 「이 학생은 나쵸가 띄웠다」의 증거다.
pub const ENV_ORIGIN: &str = "KASATERM_ORIGIN";
pub const ENV_CONV: &str = "KASATERM_ORIGIN_CONV";
pub const ENV_TASK: &str = "KASATERM_ORIGIN_TASK";
pub const ENV_MACHINE: &str = "KASATERM_ORIGIN_MACHINE";
/// 나쵸 오케스트레이터의 실행 세대(`<task>.r<n>`). 선택 — 나쵸가 안 실으면 빈 값이다.
pub const ENV_RUN: &str = "KASATERM_ORIGIN_RUN";
/// 인박스 자리 override(나쵸·시험용). 없으면 `~/.config/kasaterm/nacho-inbox`.
pub const ENV_INBOX_DIR: &str = "NACHO_INBOX_DIR";
/// 깨우기 소켓 자리 override. 유닉스 소켓 경로는 104바이트 상한이라 인박스가 깊은 경로에
/// 있으면 `wake.sock` 을 못 연다(실측: 스크래치 경로에서 `AF_UNIX path too long`) —
/// 나쵸는 이걸로 `/tmp` 같은 짧은 자리에 열고, 쓰는 쪽도 같은 값을 본다.
pub const ENV_WAKE_SOCK: &str = "NACHO_WAKE_SOCK";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    pub conv: String,
    pub task_id: String,
    /// 나쵸가 사는 기계의 machine_id. 비면 「이 기계」.
    pub machine: String,
    /// 실행 세대. 비면 모른다 — 나쵸는 봉투의 `surface_key` 로 등록 줄을 찾는다.
    pub run: String,
}

/// env 에서 origin 표식을 읽는다. `KASATERM_ORIGIN` 이 정확히 `nacho` 일 때만 Some.
pub fn origin_from_env() -> Option<Origin> {
    origin_from(|k| std::env::var(k).ok())
}

pub fn origin_from(get: impl Fn(&str) -> Option<String>) -> Option<Origin> {
    let origin = get(ENV_ORIGIN)?;
    if origin.trim() != ORIGIN {
        return None;
    }
    let pick = |k: &str| get(k).map(|v| v.trim().to_string()).unwrap_or_default();
    Some(Origin { conv: pick(ENV_CONV), task_id: pick(ENV_TASK), machine: pick(ENV_MACHINE), run: pick(ENV_RUN) })
}

/// 세션 id → 표식 기억 자리.
///
/// 앱 재시작 복원은 pane env 를 새로 만들고 `claude --resume <sid>` 만 친다 — 부팅 명령에
/// 실렸던 표식이 사라져 그 학생의 `nacho-report` 가 거부됐다(나쵸 오케스트레이터 문서 C).
/// 세션 id 가 곧 「같은 대화」라 그 열쇠로 남겨 두고 복원이 되붙인다. 분할·새 탭은 세션이
/// 달라 물려받지 않는다 — 나쵸 학생이 띄운 손자 학생은 나쵸 것이 아니라는 규칙 그대로다.
pub fn origins_path() -> Result<PathBuf> {
    if let Some(root) = crate::isolated_collab_root() {
        return Ok(root.join("nacho-origins.json"));
    }
    Ok(crate::home_dir().context("home directory unavailable")?.join(".config/kasaterm/nacho-origins.json"))
}

/// 표식 기억을 얼마나 두나. 나쵸 장부가 48시간 지난 열린 일을 접으므로 그보다 넉넉히.
const ORIGIN_RETENTION_MS: u64 = 14 * 24 * 60 * 60 * 1000;
const ORIGIN_CAP: usize = 500;

fn valid_session_id(sid: &str) -> bool {
    (8..=64).contains(&sid.len()) && sid.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// 표식 값 한 칸. 복원 명령에 셸 문자열로 들어가므로 제어문자·줄바꿈은 받지 않는다.
fn origin_value_ok(value: &str) -> bool {
    value.len() <= 200 && !value.chars().any(|c| c.is_control())
}

/// 이 세션을 나쵸가 띄웠다고 적는다. 같은 값이면 안 쓰고 `false`.
pub fn remember_origin(session_id: &str, origin: &Origin) -> Result<bool> {
    remember_origin_at(&origins_path()?, session_id, origin, now_ms())
}

pub fn remember_origin_at(path: &Path, session_id: &str, origin: &Origin, now: u64) -> Result<bool> {
    ensure!(valid_session_id(session_id), "session id must be 8-64 [A-Za-z0-9-]");
    for value in [&origin.conv, &origin.task_id, &origin.machine, &origin.run] {
        ensure!(origin_value_ok(value), "origin value has control characters or is too long");
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    // 여러 pane 이 한꺼번에 bind 하면 읽고-고치고-쓰기가 서로를 지운다 — 옆 파일에 잠근다.
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(path.with_extension("lock"))?;
    lock.lock()?;
    let mut map: serde_json::Map<String, Value> = std::fs::read_to_string(path).ok()
        .and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let entry = json!({"conv": origin.conv, "task_id": origin.task_id, "machine": origin.machine, "run": origin.run});
    let same = map.get(session_id).is_some_and(|old| {
        ["conv", "task_id", "machine", "run"].iter().all(|k| old.get(k) == entry.get(k))
    });
    if same {
        return Ok(false);
    }
    let mut entry = entry;
    entry["at_ms"] = json!(now);
    map.insert(session_id.to_string(), entry);
    let at = |v: &Value| v.get("at_ms").and_then(|a| a.as_u64()).unwrap_or(0);
    map.retain(|_, v| now.saturating_sub(at(v)) <= ORIGIN_RETENTION_MS);
    while map.len() > ORIGIN_CAP {
        let Some(oldest) = map.iter().min_by_key(|(_, v)| at(v)).map(|(k, _)| k.clone()) else { break };
        map.remove(&oldest);
    }
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    {
        let mut file = std::fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
        }
        file.write_all(serde_json::to_string(&Value::Object(map))?.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("rename into {}", path.display()))?;
    Ok(true)
}

/// 이 세션의 표식. 없거나 읽지 못하면 `None` — 복원은 표식 없이 전과 같이 뜬다.
pub fn origin_for_session(session_id: &str) -> Option<Origin> {
    origin_for_session_at(&origins_path().ok()?, session_id)
}

pub fn origin_for_session_at(path: &Path, session_id: &str) -> Option<Origin> {
    if !valid_session_id(session_id) {
        return None;
    }
    let map: serde_json::Map<String, Value> = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let entry = map.get(session_id)?;
    let text = |k: &str| entry.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let origin = Origin { conv: text("conv"), task_id: text("task_id"), machine: text("machine"), run: text("run") };
    [&origin.conv, &origin.task_id, &origin.machine, &origin.run].iter().all(|v| origin_value_ok(v)).then_some(origin)
}

/// 복원 명령 앞에 붙일 env 한 줄(끝에 빈칸). 나쵸가 학생을 띄울 때와 같은 모양이다.
/// 값은 작은따옴표로 감싼다 — 대화 id 에 `:` 가, 세대에 `.` 가 들고 셸이 그것을 건드리면 안 된다.
pub fn origin_env_prefix(origin: &Origin) -> String {
    let q = |s: &str| format!("'{}'", s.replace('\'', r"'\''"));
    let mut out = format!("{ENV_ORIGIN}={ORIGIN} ");
    for (name, value) in [(ENV_CONV, &origin.conv), (ENV_TASK, &origin.task_id), (ENV_MACHINE, &origin.machine), (ENV_RUN, &origin.run)] {
        if !value.is_empty() {
            out.push_str(&format!("{name}={} ", q(value)));
        }
    }
    out
}

/// 봉투의 선택 칸(`surface_key`·`run_id`) 하나. 모양이 어긋나면 **보고를 막지 않고** 칸만 비운다 —
/// 보조 이름표 때문에 완료 보고가 거부되면 그게 더 큰 사고다.
fn tag_field(params: &Value, key: &str) -> String {
    let value = text_field(params, key);
    let ok = value.len() <= 96 && value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'));
    if ok { value } else { String::new() }
}

/// 인박스 루트. 검증 앱(isolated)은 자기 루트 아래, 그다음 env, 그다음 홈.
pub fn inbox_root() -> Result<PathBuf> {
    if let Some(root) = crate::isolated_collab_root() {
        return Ok(root.join("nacho-inbox"));
    }
    if let Some(dir) = std::env::var_os(ENV_INBOX_DIR).filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    Ok(crate::home_dir().context("home directory unavailable")?.join(".config/kasaterm/nacho-inbox"))
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

pub fn new_report_id() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
    format!("nr1.{}.{:x}-{:x}-{:x}", now_ms(), std::process::id(), nonce, NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
}

pub fn valid_report_id(id: &str) -> Result<()> {
    ensure!(id.len() <= 128, "invalid report_id");
    let parts: Vec<_> = id.split('.').collect();
    ensure!(
        parts.len() == 3 && parts[0] == "nr1" && parts[1].bytes().all(|b| b.is_ascii_digit()) && parts[2].len() >= 8
            && parts[2].bytes().all(|b| b.is_ascii_hexdigit() || b == b'-'),
        "report_id must be nr1.<unix-ms>.<hex-nonce>"
    );
    Ok(())
}

/// 토큰·비밀처럼 보이는 조각. 정규식 없이 손으로 훑는다(이 크레이트는 의존성이 셋뿐이다).
/// 잡히면 어떤 종류였는지 돌려준다 — 학생이 무엇을 빼야 하는지 알아야 고친다.
pub fn secret_like(text: &str) -> Option<&'static str> {
    let lower = text.to_ascii_lowercase();
    if lower.contains("-----begin") && lower.contains("private key") {
        return Some("private key block");
    }
    let is_tok = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.';
    let run_after = |start: usize| text[start..].chars().take_while(|c| is_tok(*c)).count();
    for (prefix, min, what) in [
        ("sk-", 16, "sk- API key"),
        ("xoxb-", 8, "Slack bot token"),
        ("xoxp-", 8, "Slack user token"),
        ("xoxa-", 8, "Slack token"),
        ("xapp-", 8, "Slack app token"),
        ("ghp_", 16, "GitHub token"),
        ("gho_", 16, "GitHub token"),
        ("github_pat_", 16, "GitHub token"),
        ("akia", 16, "AWS access key"),
        ("eyj", 24, "JWT"),
    ] {
        let mut from = 0;
        while let Some(pos) = lower[from..].find(prefix) {
            let at = from + pos;
            // AKIA/eyJ 는 대소문자가 증거의 일부다 — 소문자 우연(예: "akia" 단어)은 거른다.
            let literal_ok = match prefix {
                "akia" => text[at..].starts_with("AKIA"),
                "eyj" => text[at..].starts_with("eyJ"),
                _ => true,
            };
            if literal_ok && run_after(at + prefix.len()) >= min {
                return Some(what);
            }
            from = at + prefix.len();
        }
    }
    if let Some(pos) = lower.find("bearer ") {
        if run_after(pos + 7) >= 20 {
            return Some("Bearer token");
        }
    }
    // key=value / key: value 꼴 — 키 이름에 token·secret·password·api_key 가 들고 값이 길면.
    for line in text.lines() {
        for piece in line.split(|c: char| c.is_whitespace() || c == ',' || c == ';') {
            let Some((key, value)) = piece.split_once(|c| c == '=' || c == ':') else { continue };
            let key = key.trim().trim_matches(|c| c == '"' || c == '\'').to_ascii_lowercase();
            let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
            let secret_key = key.ends_with("token") || key.ends_with("secret") || key.contains("password") || key.contains("passwd")
                || key.ends_with("api_key") || key.ends_with("apikey") || key.ends_with("access_key") || key.ends_with("private_key");
            if secret_key && value.chars().count() >= 8 && value.chars().all(|c| !c.is_whitespace()) {
                return Some("credential assignment");
            }
        }
    }
    None
}

fn text_field(params: &Value, key: &str) -> String {
    match params.get(key) {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Array(items)) => items.iter().filter_map(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty())
            .collect::<Vec<_>>().join("\n"),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// `changed` 는 배열이나 줄바꿈·쉼표 구분 문자열 둘 다 받는다 — 학생이 셸에서 치기 쉬운 쪽으로.
fn list_field(params: &Value, key: &str) -> Vec<String> {
    match params.get(key) {
        Some(Value::Array(items)) => items.iter().filter_map(|v| v.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
        Some(Value::String(s)) => s.split(|c| c == '\n' || c == ',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
        _ => Vec::new(),
    }
}

fn clean(text: &str, what: &str) -> Result<String> {
    let text = text.replace("\r\n", "\n");
    ensure!(!text.chars().any(|c| c.is_control() && c != '\n' && c != '\t'), "{what} contains terminal control characters");
    Ok(text.trim().to_string())
}

/// 본문 지문 — 같은 학생이 같은 상태를 같은 말로 두 번 보고하면 같은 값. `at_ms`·id 는 안 넣는다.
pub fn fingerprint(envelope: &Value) -> String {
    let mut h = 0xcbf29ce484222325u64;
    let mut feed = |s: &str| {
        for b in s.bytes() { h = (h ^ b as u64).wrapping_mul(0x100000001b3); }
        h = (h ^ 0x1f).wrapping_mul(0x100000001b3);
    };
    for key in ["origin", "conv", "task_id", "surface", "status", "summary", "tests", "next"] {
        feed(envelope.get(key).and_then(|v| v.as_str()).unwrap_or(""));
    }
    for item in envelope.get("changed").and_then(|v| v.as_array()).into_iter().flatten() {
        feed(item.as_str().unwrap_or(""));
    }
    format!("{h:016x}")
}

/// 학생이 준 인자(CLI·소켓·HTTP 어디서 왔든 같은 모양)를 검증해 정본 봉투로 만든다.
/// 통과 못 하면 이유가 곧 학생에게 보이는 오류다.
pub fn build(params: &Value) -> Result<Value> {
    ensure!(params.is_object(), "report must be a JSON object");
    let origin = text_field(params, "origin");
    ensure!(origin == ORIGIN, "origin must be \"nacho\" — this pane was not started by nacho, do not report");
    let status = text_field(params, "status");
    ensure!(STATUSES.contains(&status.as_str()), "status must be one of done|blocked|needs_restart|needs_approval, got \"{status}\"");
    let summary = clean(&text_field(params, "summary"), "summary")?;
    ensure!(!summary.is_empty(), "summary is required — one or two lines: what was done or where it is stuck");
    let tests = clean(&text_field(params, "tests"), "tests")?;
    let next = clean(&text_field(params, "next"), "next")?;
    let changed = list_field(params, "changed").into_iter().map(|c| clean(&c, "changed")).collect::<Result<Vec<_>>>()?;
    ensure!(status == "done" || !next.is_empty() || status == "needs_approval", "blocked/needs_restart reports need `next`: what nacho should do or decide");
    for (what, text) in [("summary", &summary), ("tests", &tests), ("next", &next)] {
        if let Some(kind) = secret_like(text) {
            bail!("{what} looks like it contains a secret ({kind}); reports must not carry tokens or credentials");
        }
    }
    for c in &changed {
        if let Some(kind) = secret_like(c) { bail!("changed looks like it contains a secret ({kind})"); }
    }
    let report_id = match params.get("report_id").and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty()) {
        Some(id) => { valid_report_id(id)?; id.to_string() }
        None => new_report_id(),
    };
    let host = match params.get("host") {
        Some(Value::Object(h)) => json!({
            "machine_id": h.get("machine_id").and_then(|v| v.as_str()).unwrap_or("").trim(),
            "label": h.get("label").and_then(|v| v.as_str()).unwrap_or("").trim(),
        }),
        Some(Value::String(s)) => json!({"machine_id": "", "label": s.trim()}),
        _ => json!({"machine_id": "", "label": ""}),
    };
    let mut envelope = json!({
        "schema": SCHEMA,
        "report_id": report_id,
        "at_ms": now_ms(),
        "origin": ORIGIN,
        "conv": clean(&text_field(params, "conv"), "conv")?,
        "task_id": clean(&text_field(params, "task_id"), "task_id")?,
        "surface": clean(&text_field(params, "surface"), "surface")?,
        // 아래 둘은 지문에 안 든다 — 나쵸의 지문 검사가 고정 칸만 보므로 넣으면 봉투가 거부된다.
        // `surface_key` 는 판 주소의 UUID(창 번호는 재사용된다), `run_id` 는 나쵸의 실행 세대.
        "surface_key": tag_field(params, "surface_key"),
        "run_id": tag_field(params, "run_id"),
        "host": host,
        "cwd": clean(&text_field(params, "cwd"), "cwd")?,
        "harness": clean(&text_field(params, "harness"), "harness")?,
        "character": clean(&text_field(params, "character"), "character")?,
        "status": status,
        "summary": summary,
        "changed": changed,
        "tests": tests,
        "next": next,
    });
    envelope["fingerprint"] = json!(fingerprint(&envelope));
    let bytes = serde_json::to_vec(&envelope)?.len();
    ensure!(bytes <= MAX_BYTES, "report exceeds 16 KiB ({bytes} bytes) — shorten summary/changed/tests");
    Ok(envelope)
}

fn ensure_dirs(root: &Path) -> Result<()> {
    for sub in ["new", "done", "tmp"] {
        std::fs::create_dir_all(root.join(sub)).with_context(|| format!("create {}", root.join(sub).display()))?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for sub in ["", "new", "done", "tmp"] {
            let _ = std::fs::set_permissions(root.join(sub), std::fs::Permissions::from_mode(0o700));
        }
    }
    Ok(())
}

fn existing(root: &Path, fp: &str) -> Option<(PathBuf, Value)> {
    for sub in ["new", "done"] {
        let path = root.join(sub).join(format!("{fp}.json"));
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(value) = serde_json::from_str::<Value>(&text) {
                return Some((path, value));
            }
        }
    }
    None
}

/// `done/` 의 오래된 사본을 걷는다 — 지문 기억 창(DONE_RETENTION_MS)이 지난 것만.
fn prune_done(root: &Path, now: u64) {
    let Ok(dir) = std::fs::read_dir(root.join("done")) else { return };
    for entry in dir.flatten() {
        let path = entry.path();
        let stale = std::fs::read_to_string(&path).ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v.get("at_ms").and_then(|a| a.as_u64()))
            .is_some_and(|at| now.saturating_sub(at) > DONE_RETENTION_MS);
        if stale { let _ = std::fs::remove_file(&path); }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wake {
    /// 나쵸의 wake.sock 이 받고 ack 했다 — 지금 깬다.
    Socket,
    /// 소켓이 없거나 응답이 없다 — 파일은 남았고 나쵸가 다음 부팅·폴링에서 집는다.
    Queued(String),
}

pub fn wake_socket_path(root: &Path) -> PathBuf {
    if let Some(p) = std::env::var_os(ENV_WAKE_SOCK).filter(|v| !v.is_empty()) {
        return PathBuf::from(p);
    }
    root.join("wake.sock")
}

/// wake.sock 에 `{"kind":"report","path":…}` 한 줄을 넣고 ack 한 줄을 기다린다.
#[cfg(unix)]
pub fn wake(root: &Path, deposited: &Path) -> Wake {
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixStream;
    let sock = wake_socket_path(root);
    if !sock.exists() {
        return Wake::Queued("wake.sock absent (nacho not listening)".into());
    }
    let stream = match UnixStream::connect(&sock) {
        Ok(s) => s,
        Err(e) => return Wake::Queued(format!("wake.sock connect failed: {e}")),
    };
    let timeout = Some(std::time::Duration::from_secs(3));
    let _ = stream.set_read_timeout(timeout);
    let _ = stream.set_write_timeout(timeout);
    let mut writer = match stream.try_clone() { Ok(w) => w, Err(e) => return Wake::Queued(format!("wake.sock clone failed: {e}")) };
    let line = json!({"kind": "report", "path": deposited.to_string_lossy()}).to_string() + "\n";
    if let Err(e) = writer.write_all(line.as_bytes()) {
        return Wake::Queued(format!("wake.sock write failed: {e}"));
    }
    let mut reply = String::new();
    match BufReader::new(stream).read_line(&mut reply) {
        Ok(n) if n > 0 => Wake::Socket,
        Ok(_) => Wake::Queued("wake.sock closed without ack".into()),
        Err(e) => Wake::Queued(format!("wake.sock ack timeout: {e}")),
    }
}

#[cfg(not(unix))]
pub fn wake(_root: &Path, _deposited: &Path) -> Wake {
    Wake::Queued("wake socket unsupported on this platform".into())
}

fn receipt(envelope: &Value, path: &Path, state: &str, wake: Option<&Wake>, root: &Path) -> Value {
    let (wake_state, wake_note, alive) = match wake {
        Some(Wake::Socket) => ("socket", String::new(), true),
        Some(Wake::Queued(why)) => ("queued", why.clone(), false),
        None => ("skipped", "duplicate — the earlier report already woke nacho".into(), false),
    };
    json!({
        "ok": true,
        "report_id": envelope["report_id"],
        "fingerprint": envelope["fingerprint"],
        "status": envelope["status"],
        "state": state,
        "path": path.to_string_lossy(),
        "inbox": root.to_string_lossy(),
        "wake": wake_state,
        "wake_note": wake_note,
        "nacho_alive": alive,
    })
}

/// 정본 봉투를 인박스에 넣고 깨운다. 같은 지문이 이미 있으면 안 쓰고 그 영수증을 준다.
pub fn deposit(envelope: &Value) -> Result<Value> {
    deposit_into(&inbox_root()?, envelope)
}

pub fn deposit_into(root: &Path, envelope: &Value) -> Result<Value> {
    ensure!(envelope.get("schema").and_then(|v| v.as_str()) == Some(SCHEMA), "deposit needs a built envelope (call build first)");
    ensure_dirs(root)?;
    let fp = envelope["fingerprint"].as_str().context("envelope fingerprint missing")?.to_string();
    if let Some((path, old)) = existing(root, &fp) {
        return Ok(receipt(&old, &path, "duplicate", None, root));
    }
    let final_path = root.join("new").join(format!("{fp}.json"));
    let tmp_path = root.join("tmp").join(format!("{fp}.{}.{}.json", std::process::id(), now_ms()));
    {
        let mut file = std::fs::File::create(&tmp_path).with_context(|| format!("create {}", tmp_path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
        }
        file.write_all(serde_json::to_string_pretty(envelope)?.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
    }
    // rename 은 같은 파일시스템 안에서 원자적이다. 경쟁자가 먼저 같은 지문을 놓았으면
    // 우리 tmp 를 버리고 그쪽 영수증을 돌려준다 — 한 지문에 파일 하나.
    if final_path.exists() {
        let _ = std::fs::remove_file(&tmp_path);
        if let Some((path, old)) = existing(root, &fp) {
            return Ok(receipt(&old, &path, "duplicate", None, root));
        }
    }
    std::fs::rename(&tmp_path, &final_path).with_context(|| format!("rename into {}", final_path.display()))?;
    if let Ok(dir) = std::fs::File::open(root.join("new")) { let _ = dir.sync_all(); }
    prune_done(root, now_ms());
    let woke = wake(root, &final_path);
    Ok(receipt(envelope, &final_path, "accepted", Some(&woke), root))
}

/// 인자 → 봉투 → 인박스. 소켓 메서드·HTTP·CLI 로컬 경로가 전부 이 한 함수다.
pub fn submit_local(params: &Value) -> Result<Value> {
    deposit(&build(params)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("nacho-inbox-test-{tag}-{}-{}", std::process::id(), now_ms()));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    fn params(status: &str, summary: &str) -> Value {
        json!({"origin":"nacho","conv":"discord:1","task_id":"t-1","surface":"%7","cwd":"/repo",
               "status":status,"summary":summary,"changed":["a.py","b.py"],"tests":"pytest ok","next":"검증해 줘"})
    }

    #[test]
    fn optional_tags_ride_outside_the_fingerprint_and_bad_ones_are_dropped() {
        let plain = build(&params("done", "x")).unwrap();
        let mut tagged = params("done", "x");
        tagged["surface_key"] = json!("7fea30f0-9a88-4a95-863f-09099807746a");
        tagged["run_id"] = json!("we409f946.r2");
        let tagged = build(&tagged).unwrap();
        assert_eq!(tagged["surface_key"], "7fea30f0-9a88-4a95-863f-09099807746a");
        assert_eq!(tagged["run_id"], "we409f946.r2");
        assert_eq!(plain["fingerprint"], tagged["fingerprint"], "나쵸의 지문 검사는 고정 칸만 본다");
        assert_eq!(plain["surface_key"], "", "없으면 빈 칸");
        let mut bad = params("done", "x");
        bad["surface_key"] = json!("a b; rm -rf ~");
        let bad = build(&bad).expect("보조 칸 때문에 보고를 막지 않는다");
        assert_eq!(bad["surface_key"], "");
    }

    #[test]
    fn origin_memory_roundtrips_by_session_and_skips_unchanged_writes() {
        let root = tmp_root("origins");
        let path = root.join("nacho-origins.json");
        let origin = Origin { conv: "discord:809".into(), task_id: "we409f946".into(), machine: "mini".into(), run: "we409f946.r1".into() };
        assert!(remember_origin_at(&path, "0fd9e73e-3f56-4683-ad9d-6ebfc84833df", &origin, 1_000).unwrap());
        assert!(!remember_origin_at(&path, "0fd9e73e-3f56-4683-ad9d-6ebfc84833df", &origin, 2_000).unwrap(), "같은 값은 다시 안 쓴다");
        assert_eq!(origin_for_session_at(&path, "0fd9e73e-3f56-4683-ad9d-6ebfc84833df"), Some(origin.clone()));
        assert_eq!(origin_for_session_at(&path, "11111111-2222-3333-4444-555555555555"), None, "다른 세션은 물려받지 않는다");
        assert!(remember_origin_at(&path, "../../etc", &origin, 3_000).is_err(), "세션 id 가 아닌 열쇠는 거부");
        let later = 1_000 + ORIGIN_RETENTION_MS + 1;
        remember_origin_at(&path, "aaaaaaaa-0000-0000-0000-000000000000", &origin, later).unwrap();
        assert_eq!(origin_for_session_at(&path, "0fd9e73e-3f56-4683-ad9d-6ebfc84833df"), None, "보관 기한이 지나면 걷힌다");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn restore_prefix_quotes_values_and_skips_empty_ones() {
        let origin = Origin { conv: "discord:809".into(), task_id: "t'1".into(), machine: String::new(), run: String::new() };
        assert_eq!(
            origin_env_prefix(&origin),
            "KASATERM_ORIGIN=nacho KASATERM_ORIGIN_CONV='discord:809' KASATERM_ORIGIN_TASK='t'\\''1' "
        );
        let control = Origin { conv: "a\nb".into(), ..origin };
        let root = tmp_root("origins-control");
        assert!(remember_origin_at(&root.join("o.json"), "bbbbbbbb-0000-0000-0000-000000000000", &control, 1).is_err(), "줄바꿈은 복원 명령을 둘로 쪼갠다");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn origin_gate_rejects_everything_but_nacho() {
        assert!(build(&json!({"status":"done","summary":"x"})).is_err());
        assert!(build(&json!({"origin":"geono","status":"done","summary":"x"})).is_err());
        assert!(build(&params("done","x")).is_ok());
        assert!(origin_from(|k| (k == ENV_ORIGIN).then(|| "nacho".into())).is_some());
        assert!(origin_from(|k| (k == ENV_ORIGIN).then(|| "Nacho".into())).is_none());
        assert!(origin_from(|_| None).is_none());
    }

    #[test]
    fn status_is_closed_set_and_blocked_needs_next() {
        let e = build(&params("almost", "x")).unwrap_err().to_string();
        assert!(e.contains("done|blocked|needs_restart|needs_approval"), "{e}");
        let mut p = params("blocked", "stuck"); p["next"] = json!("");
        assert!(build(&p).unwrap_err().to_string().contains("next"));
        let mut p = params("done", "fine"); p["next"] = json!("");
        assert!(build(&p).is_ok(), "done may omit next");
    }

    #[test]
    fn secret_guard_refuses_tokens() {
        for bad in [
            "used sk-abcdefghijklmnopqrstuvwxyz1234",
            "token xoxb-1234567890-abc",
            "Authorization: Bearer abcdefghijklmnopqrstuvwxyz0123",
            "-----BEGIN RSA PRIVATE KEY-----",
            "api_key=verysecretvalue123",
            "ghp_abcdefghijklmnopqrstuvwxyz",
            "AKIAABCDEFGHIJKLMNOP",
            "jwt eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJ",
        ] {
            assert!(secret_like(bad).is_some(), "should flag: {bad}");
            let e = build(&params("done", bad)).unwrap_err().to_string();
            assert!(e.contains("secret"), "{e}");
        }
        for ok in ["Skipped the token counter", "password prompt appeared, asked geono", "changed akia handling", "status: ok=true"] {
            assert!(secret_like(ok).is_none(), "should pass: {ok}");
        }
    }

    #[test]
    fn deposit_is_atomic_dedups_and_queues_without_listener() {
        let root = tmp_root("dedup");
        let env = build(&params("done", "finished")).unwrap();
        let r1 = deposit_into(&root, &env).unwrap();
        assert_eq!(r1["state"], "accepted");
        assert_eq!(r1["wake"], "queued");
        assert_eq!(r1["nacho_alive"], false);
        let path = PathBuf::from(r1["path"].as_str().unwrap());
        assert!(path.starts_with(root.join("new")));
        let back: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(back["summary"], "finished");
        assert_eq!(back["fingerprint"], env["fingerprint"]);
        assert!(std::fs::read_dir(root.join("tmp")).unwrap().next().is_none(), "tmp must be empty after rename");
        // 같은 내용을 새 id 로 다시 → 지문이 같아 안 쓴다.
        let again = build(&params("done", "finished")).unwrap();
        assert_ne!(again["report_id"], env["report_id"]);
        let r2 = deposit_into(&root, &again).unwrap();
        assert_eq!(r2["state"], "duplicate");
        assert_eq!(r2["report_id"], env["report_id"], "the first receipt wins");
        assert_eq!(std::fs::read_dir(root.join("new")).unwrap().count(), 1);
        // 나쵸가 done/ 으로 옮긴 뒤에도 지문은 기억된다.
        std::fs::rename(&path, root.join("done").join(path.file_name().unwrap())).unwrap();
        let r3 = deposit_into(&root, &again).unwrap();
        assert_eq!(r3["state"], "duplicate");
        // 다른 상태면 새 보고다.
        let r4 = deposit_into(&root, &build(&params("blocked", "finished")).unwrap()).unwrap();
        assert_eq!(r4["state"], "accepted");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn wake_socket_gets_one_line_and_ack_marks_alive() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        let root = tmp_root("wake");
        ensure_dirs(&root).unwrap();
        let listener = UnixListener::bind(wake_socket_path(&root)).unwrap();
        let got = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let seen = got.clone();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            *seen.lock().unwrap() = line;
            let mut w = stream;
            w.write_all(b"{\"ok\":true}\n").unwrap();
        });
        let env = build(&params("needs_restart", "patched selfcare")).unwrap();
        let r = deposit_into(&root, &env).unwrap();
        server.join().unwrap();
        assert_eq!(r["wake"], "socket");
        assert_eq!(r["nacho_alive"], true);
        let line: Value = serde_json::from_str(got.lock().unwrap().trim()).unwrap();
        assert_eq!(line["kind"], "report");
        assert_eq!(line["path"], r["path"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn fingerprint_ignores_ids_and_time_but_not_body() {
        let a = build(&params("done", "same")).unwrap();
        let b = build(&params("done", "same")).unwrap();
        assert_eq!(a["fingerprint"], b["fingerprint"]);
        let mut p = params("done", "same"); p["changed"] = json!("a.py, c.py");
        assert_ne!(build(&p).unwrap()["fingerprint"], a["fingerprint"]);
        assert_eq!(build(&p).unwrap()["changed"], json!(["a.py", "c.py"]));
    }

    #[test]
    fn size_cap_and_control_chars() {
        let mut p = params("done", "x"); p["tests"] = json!("a".repeat(MAX_BYTES + 10));
        assert!(build(&p).unwrap_err().to_string().contains("16 KiB"));
        let mut p = params("done", "x\u{1b}[31m"); p["next"] = json!("");
        assert!(build(&p).unwrap_err().to_string().contains("control"));
    }
}
