//! 기기 자동 등록 — 설치 링크를 처음 연 폰이 고유번호(UDID)를 보내면 개발자 계정에 등록하고, 그 기기까지 든
//! 프로파일로 ipa 를 다시 서명한 뒤 같은 페이지에서 바로 설치하게 한다.
//!
//! 1. `enroll.mobileconfig`: iOS 「Profile Service」 프로파일. 폰이 설정 앱에서 한 번 허용하면 UDID·기종·이름을
//!    `enroll` 로 POST 한다(기기 인증서로 싼 CMS). 프로파일은 아무것도 깔지 않는다.
//! 2. `enroll`: 일회용 challenge 를 확인하고 작업을 띄운 뒤 301 로 Safari 를 `r/<작업>` 에 연다.
//! 3. 작업: 미니의 서명기(`KASA_INSTALL_SIGNER` = `mobile/tool/adhoc_sign.py`)가 ASC 등록 → 프로파일 재발급 →
//!    재서명을 한다. 서명 키는 그 기계의 전용 키체인에만 있고 관문은 경로·UDID 만 넘긴다.
//!
//! 남용 막기: 링크 token 이 있어야 프로파일이 나오고, challenge 는 그 token 에 묶인 30분 일회용이다. IP 마다 10분에
//! 10번, 새 기기 등록은 하루 `KASA_INSTALL_DAILY_CAP`(기본 10)대까지. 애플 한도는 기종마다 한 해 100대라 이것이
//! 바닥나면 한 해를 못 쓴다. 모든 시도는 `relay-install/registrations.jsonl` 에 남고 관리 화면이 보인다.

use super::*;
use std::process::Stdio;

const CHALLENGE_TTL: Duration = Duration::from_secs(30 * 60);
const MAX_CHALLENGES: usize = 256;
const MAX_JOBS: usize = 64;
const IP_WINDOW: Duration = Duration::from_secs(10 * 60);
const IP_LIMIT: u32 = 10;
const DEFAULT_DAILY_CAP: usize = 10;
const SIGNER_TIMEOUT: Duration = Duration::from_secs(300);
const PROFILE_SIGN_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_ATTEMPT_BODY: usize = 64 * 1024;
const LOG: &str = "registrations.jsonl";
/// 등록을 마친 Safari 에 남기는 표 — 다음에 연 설치 페이지가 「기기 등록」 대신 「설치」를 먼저 보인다.
pub(super) const COOKIE: &str = "kasa_install_device";

#[derive(Clone)]
pub(super) enum Job {
    Running,
    Done { new: bool },
    Failed(String),
}

pub(in crate::gateway) struct Enroll {
    pub(super) signer: Option<PathBuf>,
    daily_cap: usize,
    /// challenge → (설치 token, 낸 때).
    challenges: Mutex<HashMap<String, (String, Instant)>>,
    jobs: Mutex<Vec<(String, Job)>>,
    hits: Mutex<HashMap<String, (Instant, u32)>>,
    /// 서명기는 한 번에 하나 — 같은 프로파일·ipa 를 만진다(서명기도 파일 잠금을 한 번 더 건다).
    run: Arc<tokio::sync::Mutex<()>>,
}

impl Enroll {
    pub(in crate::gateway) fn from_env() -> Self {
        let signer = std::env::var_os("KASA_INSTALL_SIGNER").map(PathBuf::from).filter(|p| p.is_file());
        let cap = std::env::var("KASA_INSTALL_DAILY_CAP").ok().and_then(|v| v.parse().ok()).unwrap_or(DEFAULT_DAILY_CAP);
        Self::new(signer, cap)
    }

    pub(super) fn new(signer: Option<PathBuf>, daily_cap: usize) -> Self {
        Self {
            signer,
            daily_cap,
            challenges: Mutex::new(HashMap::new()),
            jobs: Mutex::new(Vec::new()),
            hits: Mutex::new(HashMap::new()),
            run: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    fn allow(&self, ip: &str) -> bool {
        let now = Instant::now();
        let mut hits = self.hits.lock().unwrap();
        hits.retain(|_, (start, _)| now.duration_since(*start) < IP_WINDOW);
        let e = hits.entry(ip.to_string()).or_insert((now, 0));
        e.1 += 1;
        e.1 <= IP_LIMIT
    }

    fn challenge(&self, token: &str) -> Option<String> {
        let mut map = self.challenges.lock().unwrap();
        map.retain(|_, (_, at)| at.elapsed() < CHALLENGE_TTL);
        if map.len() >= MAX_CHALLENGES {
            return None;
        }
        let c = uuid::Uuid::new_v4().simple().to_string();
        map.insert(c.clone(), (token.to_string(), Instant::now()));
        Some(c)
    }

    fn redeem(&self, challenge: &str, token: &str) -> bool {
        let mut map = self.challenges.lock().unwrap();
        match map.remove(challenge) {
            Some((t, at)) => t == token && at.elapsed() < CHALLENGE_TTL,
            None => false,
        }
    }

    fn set_job(&self, id: &str, job: Job) {
        let mut jobs = self.jobs.lock().unwrap();
        jobs.retain(|(j, _)| j != id);
        jobs.push((id.to_string(), job));
        let over = jobs.len().saturating_sub(MAX_JOBS);
        jobs.drain(..over);
    }

    pub(super) fn job(&self, id: &str) -> Option<Job> {
        self.jobs.lock().unwrap().iter().find(|(j, _)| j == id).map(|(_, job)| job.clone())
    }

    pub(super) fn daily_cap(&self) -> usize {
        self.daily_cap
    }
}

#[derive(serde::Serialize, Deserialize)]
pub(super) struct Record {
    pub at: u64,
    pub udid: String,
    pub product: String,
    pub version: String,
    pub name: String,
    #[serde(default)]
    pub ip: String,
    /// registered(새로 등록) · known(이미 있던 기기) · failed · capped(하루 한도) · busy
    pub result: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
}

impl Gate {
    pub(super) fn records(&self) -> Vec<Record> {
        let Some(dir) = &self.install_dir else { return Vec::new() };
        std::fs::read_to_string(dir.join(LOG))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    pub(super) fn registered_today(&self) -> usize {
        let since = now_secs().saturating_sub(86_400);
        self.records().iter().filter(|r| r.at >= since && r.result == "registered").count()
    }

    fn record(&self, rec: &Record) {
        let Some(dir) = &self.install_dir else { return };
        use std::io::Write as _;
        let line = serde_json::to_string(rec).unwrap_or_default();
        let wrote = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(LOG))
            .and_then(|mut f| writeln!(f, "{line}"));
        if let Err(e) = wrote {
            eprintln!("[install] 등록 기록을 못 남겼다: {e}");
        }
    }
}

/// 서명기를 띄워 끝날 때까지(최대 `timeout`) 기다린다. (성공, 표준출력, 표준오류 끝).
fn run_signer(signer: &std::path::Path, args: &[String], input: Option<Vec<u8>>, timeout: Duration) -> (bool, Vec<u8>, String) {
    use std::io::{Read as _, Write as _};
    let child = std::process::Command::new(signer)
        .args(args)
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => return (false, Vec::new(), format!("서명기를 못 띄웠다: {e}")),
    };
    if let (Some(data), Some(mut stdin)) = (input, child.stdin.take()) {
        std::thread::spawn(move || {
            let _ = stdin.write_all(&data);
        });
    }
    let mut out = child.stdout.take();
    let mut err = child.stderr.take();
    let out = std::thread::spawn(move || {
        let mut v = Vec::new();
        if let Some(o) = out.as_mut() {
            let _ = o.read_to_end(&mut v);
        }
        v
    });
    let err = std::thread::spawn(move || {
        let mut v = Vec::new();
        if let Some(e) = err.as_mut() {
            let _ = e.read_to_end(&mut v);
        }
        v
    });
    let deadline = Instant::now() + timeout;
    let ok = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(200)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return (false, Vec::new(), "서명기가 시간 안에 안 끝났다".into());
            }
        }
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = String::from_utf8_lossy(&err.join().unwrap_or_default()).chars().rev().take(300).collect::<String>();
    (ok, stdout, stderr.chars().rev().collect())
}

/// 폰이 보낸 CMS 봉투 안의 plist 에서 문자열 값만 뽑는다. 서명 검증은 하지 않는다 — 막는 것은 일회용
/// challenge·횟수 제한·하루 한도이고, 뽑은 값은 꼴 검사를 거쳐 서명기 인자로만 간다.
pub(super) fn device_attrs(body: &[u8]) -> Option<HashMap<String, String>> {
    let find = |hay: &[u8], pat: &[u8]| hay.windows(pat.len()).position(|w| w == pat);
    let start = find(body, b"<plist")?;
    let end = start + find(&body[start..], b"</plist>")?;
    let text = std::str::from_utf8(&body[start..end]).ok()?;
    let mut out = HashMap::new();
    let mut rest = text;
    while let Some(k) = rest.find("<key>") {
        rest = &rest[k + 5..];
        let ke = rest.find("</key>")?;
        let key = rest[..ke].to_string();
        rest = &rest[ke + 6..];
        if let Some(v) = rest.trim_start().strip_prefix("<string>") {
            let ve = v.find("</string>")?;
            out.insert(key, unescape(&v[..ve]));
            rest = &v[ve + 9..];
        }
    }
    Some(out)
}

fn unescape(v: &str) -> String {
    v.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// 요즘 기기 `XXXXXXXX-XXXXXXXXXXXXXXXX`(25자), 옛 기기 40자 16진.
pub(super) fn valid_udid(udid: &str) -> bool {
    let hex = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit());
    match udid.split_once('-') {
        Some((a, b)) => a.len() == 8 && b.len() == 16 && hex(a) && hex(b),
        None => udid.len() == 40 && hex(udid),
    }
}

fn label(value: Option<&String>, max: usize) -> String {
    value.map(|v| v.chars().filter(|c| !c.is_control()).take(max).collect()).unwrap_or_default()
}

pub(super) async fn profile(
    State(gate): State<Gate>,
    AxPath(token): AxPath<String>,
    req: axum::extract::Request,
) -> axum::response::Response {
    let (Some(signer), Some(_), Some(origin)) =
        (gate.enroll.signer.clone(), gate.release(&token), gate.public_origin(req.headers()))
    else {
        return gone();
    };
    if !gate.enroll.allow(&client_ip(&req)) {
        return html(StatusCode::TOO_MANY_REQUESTS, "<h1>잠시 뒤 다시 해 주세요</h1><p class=dim>등록 시도가 많아요.</p>");
    }
    let Some(challenge) = gate.enroll.challenge(&token) else {
        return html(StatusCode::SERVICE_UNAVAILABLE, "<h1>잠시 뒤 다시 해 주세요</h1><p class=dim>등록 대기가 꽉 찼어요.</p>");
    };
    let config = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>PayloadContent</key><dict>
<key>URL</key><string>{}</string>
<key>DeviceAttributes</key><array><string>UDID</string><string>PRODUCT</string><string>VERSION</string><string>DEVICE_NAME</string></array>
<key>Challenge</key><string>{challenge}</string>
</dict>
<key>PayloadOrganization</key><string>KASATERM</string>
<key>PayloadDisplayName</key><string>KASATERM 기기 등록</string>
<key>PayloadDescription</key><string>이 기기의 고유번호(UDID)와 기종·이름을 KASATERM 설치 서버에 한 번 보내 설치할 수 있는 기기로 등록합니다. 기기에는 아무것도 남지 않습니다.</string>
<key>PayloadVersion</key><integer>1</integer>
<key>PayloadUUID</key><string>{}</string>
<key>PayloadIdentifier</key><string>com.debimarlene.kasaterm.enroll</string>
<key>PayloadType</key><string>Profile Service</string>
</dict></plist>
"#,
        esc(&format!("{origin}/relay/install/{token}/enroll")),
        uuid::Uuid::new_v4().to_string().to_uppercase(),
    );
    // 서명해 두면 설정 앱이 「확인됨」으로 보인다. 못 하면 서명 없이 — 그래도 깔린다(「확인되지 않음」).
    let raw = config.clone().into_bytes();
    let signed = tokio::task::spawn_blocking(move || {
        run_signer(&signer, &["sign-profile".into()], Some(raw), PROFILE_SIGN_TIMEOUT)
    })
    .await
    .ok()
    .and_then(|(ok, out, _)| (ok && !out.is_empty()).then_some(out));
    (
        [
            (header::CONTENT_TYPE, "application/x-apple-aspen-config".to_string()),
            (header::CONTENT_DISPOSITION, "attachment; filename=\"kasaterm-enroll.mobileconfig\"".to_string()),
        ],
        signed.unwrap_or_else(|| config.into_bytes()),
    )
        .into_response()
}

pub(super) async fn attempt(
    State(gate): State<Gate>,
    AxPath(token): AxPath<String>,
    req: axum::extract::Request,
) -> axum::response::Response {
    let ip = client_ip(&req);
    let (Some(signer), Some((dir, _)), Some(origin)) =
        (gate.enroll.signer.clone(), gate.release(&token), gate.public_origin(req.headers()))
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !gate.enroll.allow(&ip) {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    let Ok(body) = axum::body::to_bytes(req.into_body(), MAX_ATTEMPT_BODY).await else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    let Some(attrs) = device_attrs(&body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let challenge = attrs.get("CHALLENGE").map(String::as_str).unwrap_or("");
    let udid = attrs.get("UDID").cloned().unwrap_or_default();
    if !gate.enroll.redeem(challenge, &token) || !valid_udid(&udid) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let mut rec = Record {
        at: now_secs(),
        udid: udid.clone(),
        product: label(attrs.get("PRODUCT"), 32),
        version: label(attrs.get("VERSION"), 32),
        name: label(attrs.get("DEVICE_NAME"), 64),
        ip,
        result: String::new(),
        error: String::new(),
    };
    let job = uuid::Uuid::new_v4().simple().to_string();
    let location = format!("{origin}/relay/install/{token}/r/{job}");
    if gate.registered_today() >= gate.enroll.daily_cap() {
        rec.result = "capped".into();
        gate.record(&rec);
        gate.enroll.set_job(&job, Job::Failed("오늘 새 기기 등록 한도를 다 썼어요. 내일 다시 해 주세요.".into()));
        return (StatusCode::MOVED_PERMANENTLY, [(header::LOCATION, location)]).into_response();
    }
    gate.enroll.set_job(&job, Job::Running);
    let name = if rec.name.is_empty() { rec.product.clone() } else { rec.name.clone() };
    let args = vec!["register".into(), dir.to_string_lossy().into_owned(), udid, name];
    let worker = gate.clone();
    let id = job.clone();
    tokio::spawn(async move {
        let _turn = worker.enroll.run.clone().lock_owned().await;
        let (ok, out, err) = tokio::task::spawn_blocking(move || run_signer(&signer, &args, None, SIGNER_TIMEOUT))
            .await
            .unwrap_or((false, Vec::new(), "서명 작업이 죽었다".into()));
        let reply: Option<serde_json::Value> =
            String::from_utf8_lossy(&out).lines().rev().find(|l| !l.trim().is_empty()).and_then(|l| serde_json::from_str(l).ok());
        let new = reply.as_ref().and_then(|r| r["new"].as_bool()).unwrap_or(false);
        let error = reply.as_ref().and_then(|r| r["error"].as_str()).map(str::to_string).unwrap_or(err);
        if ok && reply.as_ref().is_some_and(|r| r["ok"] == true) {
            rec.result = if new { "registered" } else { "known" }.into();
            worker.enroll.set_job(&id, Job::Done { new });
        } else {
            eprintln!("[install] 기기 등록 실패: {}", error.chars().take(200).collect::<String>());
            rec.result = "failed".into();
            rec.error = error.chars().take(300).collect();
            worker.enroll.set_job(&id, Job::Failed("등록하거나 다시 서명하지 못했어요. 잠시 뒤 다시 해 주세요.".into()));
        }
        worker.record(&rec);
    });
    (StatusCode::MOVED_PERMANENTLY, [(header::LOCATION, location)]).into_response()
}

pub(super) async fn status(
    State(gate): State<Gate>,
    AxPath((token, job)): AxPath<(String, String)>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let (Some((_, meta)), Some(origin)) = (gate.release(&token), gate.public_origin(&headers)) else {
        return gone();
    };
    let (page, install) = links(&origin, &token);
    match gate.enroll.job(&job) {
        Some(Job::Running) => html(
            StatusCode::OK,
            "<meta http-equiv=refresh content=3><h1>기기를 등록하는 중이에요</h1>\
<p class=dim>이 기기를 설치 목록에 올리고 앱을 다시 서명하고 있어요. 보통 1분 안에 끝나요 — 이 화면은 저절로 바뀌어요.</p>",
        ),
        Some(Job::Done { new }) => {
            let cookie = format!("{COOKIE}=1; Path=/relay/install; Max-Age=31536000; Secure; HttpOnly; SameSite=Lax");
            let mut res = html(
                StatusCode::OK,
                &format!(
                    "<h1>{}</h1><p class=dim>{} {} · 빌드 {}</p><a class=btn href=\"{}\">설치</a>\
<p class=dim>누르면 묻는 창에서 「설치」를 고르고 홈 화면에서 아이콘이 받아지는 것을 보세요.</p>",
                    if new { "기기를 등록했어요" } else { "이미 등록된 기기예요" },
                    esc(&meta.title),
                    esc(&meta.version),
                    esc(&meta.build),
                    esc(&install),
                ),
            );
            if let Ok(v) = axum::http::HeaderValue::from_str(&cookie) {
                res.headers_mut().insert(header::SET_COOKIE, v);
            }
            res
        }
        Some(Job::Failed(why)) => html(
            StatusCode::OK,
            &format!("<h1>등록하지 못했어요</h1><p class=dim>{}</p><a class=btn href=\"{}\">처음으로</a>", esc(&why), esc(&page)),
        ),
        None => html(
            StatusCode::OK,
            &format!(
                "<h1>등록 상태를 잃었어요</h1><p class=dim>서버가 다시 켜졌을 수 있어요. 처음 화면에서 「설치」를 눌러 보고, 안 되면 다시 등록해 주세요.</p>\
<a class=btn href=\"{}\">처음으로</a>",
                esc(&page)
            ),
        ),
    }
}
