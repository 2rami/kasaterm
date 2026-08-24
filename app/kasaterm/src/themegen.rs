//! 참조 그림 한 장으로 테마 캐릭터 한 명을 굽는다 — 설정 화면의 백엔드.
//!
//! 파이프라인 자체는 `docs/theme-pipeline.md` 와 `scripts/theme-sprites.py` 가 정본이고,
//! 여기는 그중 **한 명 굽기**만 앱 안으로 옮긴 것이다. 배치로 스무 명을 굽는 일은 여전히
//! 스크립트 몫이다 — 앱은 사용자가 그림을 떨어뜨린 그 한 명만 처리한다.
//!
//! ⚠️ **키를 argv 에 싣지 않는다.** argv 는 같은 사용자의 아무 프로세스나 `ps` 로 읽을
//! 수 있고, 실제로 진행 확인하다 키가 화면에 찍힌 적이 있다(2026-08-24).
//!
//! 그런데 **환경변수만으로는 안 된다** — ppgen 의 `config.Load()` 는 `config.json` 의
//! `apiKey` 가 비어 있을 때에만 환경변수를 본다(`internal/config/config.go`:
//! `if s.OpenAI.APIKey == "" { ... env }`). 사용자 config 에 옛 키가 남아 있으면 그게
//! 이기고, 우리가 넘긴 키는 무시된 채 401 이 난다(실측으로 한 번 걸렸다).
//!
//! 그래서 **임시 HOME 에 우리 `config.json` 만 두고 그쪽을 보게 한다.** Go 의
//! `os.UserConfigDir()` 이 macOS 에서 `$HOME/Library/Application Support` 를 보므로
//! HOME 하나만 바꾸면 갈린다. 0600 파일이라 소유자만 읽고, 끝나면 통째로 지운다.
//! 사용자의 전역 설정은 건드리지 않는다.
//!
//! codex 만 예외다 — 키가 아니라 `~/.codex` 의 OAuth 를 쓰므로 HOME 을 바꾸면 오히려
//! 인증을 못 찾는다. 그쪽은 사용자 HOME 그대로 둔다.
//!
//! desc 를 짓는 curl 도 같은 이유로 설정을 stdin(`--config -`)으로 준다.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::*;

/// 굽기에 쓸 수 있는 경로. UI 가 목록으로 그린다.
#[derive(Clone)]
pub(crate) struct ProviderStatus {
    pub kind: &'static str,
    pub label: String,
    pub available: bool,
    /// 못 쓰는 사유 한글 한 줄. 가용이면 빈 문자열.
    pub why: String,
}

/// UI 가 매 프레임 읽는 진행 스냅샷.
pub(crate) struct GenJobView {
    pub phase: GenPhase,
    /// 그대로 그리면 되는 한글 문구.
    pub phase_label: String,
    /// 부가 문구("2/3 번째 시도"). 없으면 빈 문자열.
    pub detail: String,
    pub failed_reason: Option<String>,
    pub provider: String,
    /// 시작 시각(epoch ms) — 경과 표시용.
    pub started_ms: u64,
}

/// 프레임 뭉갬 판정 문턱. `theme-sprites.py` 의 `MIN_COLORS`/`MAX_RUN` 과 같은 값이고
/// 근거도 거기 주석에 있다 — 멀쩡한 프레임은 런 2 로 나오고, 뭉갠 것은 자릿수가 다르다.
const MIN_COLORS: usize = 16;
const MAX_RUN: f32 = 4.0;
/// 재시도 횟수. 같은 프롬프트를 다시 돌리면 멀쩡히 나오는 생성 편차가 있어서다.
const ATTEMPTS: usize = 3;
/// 프로필 한 변(px).
const PROFILE_PX: u32 = 96;
/// 게이트웨이 업로드 상한 때문에 참조는 줄여 보낸다(2026-08-20 실측: 1024px PNG 는 413).
const REF_MAX_PX: u32 = 512;
const REF_MAX_BYTES: usize = 100 * 1024;
/// 프로바이더 감지를 다시 하는 간격. `which codex` 와 키 파일 stat 이라 싸지만,
/// 설정 스냅샷이 매 프레임 도는 자리라 캐시가 없으면 디스크가 계속 돈다.
const DETECT_EVERY: Duration = Duration::from_secs(5);

/// 앱이 읽는 자산 폴더와 프레임 수. `theme-sprites.py` 의 `STATES` 와 같다.
const STATES: [(&str, usize); 4] = [("idle", 4), ("walk", 6), ("wave", 4), ("cheer", 4)];

/// 앱 폴더 이름 → ppgen 생성물 폴더 이름. walk 만 어긋난다 — 옆모습을 얻으려고
/// dirset 으로 구워서 생성물이 `walk-east` 로 나온다.
fn out_state(state: &str) -> &str {
    if state == "walk" { "walk-east" } else { state }
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum GenPhase {
    Describing,
    Generating,
    Installing,
    Done,
    Failed,
}

impl GenPhase {
    /// 웹이 읽는 **언어 없는** 이름. 화면 문구는 프론트가 자기 사전에서 만들고,
    /// 네이티브는 `phase_label` 의 한글을 쓴다 — 같은 값을 두 말로 보내지 않는다.
    fn wire(self) -> &'static str {
        match self {
            GenPhase::Describing => "describing",
            GenPhase::Generating => "generating",
            GenPhase::Installing => "installing",
            GenPhase::Done => "done",
            GenPhase::Failed => "failed",
        }
    }
}

/// 백그라운드 스레드와 GUI 가 함께 보는 잡 상태.
struct Job {
    theme_id: String,
    slug: String,
    provider: &'static str,
    phase: GenPhase,
    /// 진행 문구에 덧붙는 부가 설명("2/3 번째 시도" 등).
    detail: String,
    failed: Option<String>,
    started_ms: u64,
    /// 스레드가 굽기를 끝내고 GUI 가 설치하기를 기다리는 상태. 설치는 roster 갱신과
    /// 캐시 무효화를 함께 해야 해서 GUI 스레드 몫이다.
    ready: Option<PathBuf>,
}

impl Job {
    /// 그대로 그리는 한글 문구. `detail` 은 UI 가 따로 이어 붙인다 — 붙이는 방식
    /// (가운뎃점·줄바꿈)이 화면 폭에 달려 있어서 여기서 정하면 안 된다.
    fn phase_label(&self) -> &'static str {
        match self.phase {
            GenPhase::Describing => "그림 살펴보는 중",
            GenPhase::Generating => "굽는 중",
            GenPhase::Installing => "설치하는 중",
            GenPhase::Done => "완성",
            GenPhase::Failed => "실패",
        }
    }
}

#[derive(Default)]
pub(crate) struct ThemeGenState {
    /// 나노바나나 키 입력 버퍼. 설정 화면이 직접 읽고 쓴다.
    pub key_edit: String,
}

/// 잡 저장소와 감지 캐시는 **전역**이다. `App` 에 두면 GUI 스레드만 읽을 수 있는데,
/// 웹 설정 화면이 2초마다 진행을 물어보고 그 요청은 HTTP 스레드에서 온다. GUI 왕복
/// 채널로 우회할 수도 있지만 폴링 한 번마다 GUI 프레임을 물게 된다.
///
/// **상태만 전역이고 상태를 보고 움직이는 쪽은 하나다** — 설치(파일 이동·로스터 등록·
/// 캐시 무효화)는 여전히 GUI 의 `themegen_poll` 몫이다. 두 스레드가 같은 잡을 설치하면
/// 스프라이트 폴더가 반쯤 겹쳐 쓰인다.
static JOBS: LazyLock<RwLock<HashMap<String, Arc<Mutex<Job>>>>> = LazyLock::new(Default::default);
static PROVIDERS: LazyLock<RwLock<ProviderCache>> = LazyLock::new(Default::default);

#[derive(Default)]
struct ProviderCache {
    list: Vec<ProviderStatus>,
    at: Option<Instant>,
}

/// 웹 설정 화면이 폴링하는 `GET /settings/themegen/state` 의 본문.
///
/// GUI 를 거치지 않는다 — 잡은 전역이고 나머지(선택된 엔진·키·활성 테마)는 전부
/// 파일에 있다. 그래서 굽는 동안 화면이 아무리 바빠도 진행 표시가 안 밀린다.
pub(crate) fn themegen_state_json() -> serde_json::Value {
    let providers: Vec<_> = providers_cached()
        .into_iter()
        .map(|p| {
            serde_json::json!({
                "kind": p.kind, "label": p.label,
                "available": p.available, "why": p.why,
            })
        })
        .collect();
    let settings = socket::read_settings();
    let pick = settings
        .get("theme_gen_provider")
        .and_then(|v| v.as_str())
        .unwrap_or("opengateway")
        .to_string();
    let key_masked = settings
        .get("gemini_api_key")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(mask_key)
        .unwrap_or_default();

    let mut jobs = serde_json::Map::new();
    for (slug, job) in JOBS.read().unwrap().iter() {
        let j = job.lock().unwrap();
        jobs.insert(
            slug.clone(),
            serde_json::json!({
                "phase": j.phase.wire(),
                "phase_label": j.phase_label(),
                "detail": j.detail,
                "failed_reason": j.failed,
                "provider": j.provider,
                "started_ms": j.started_ms,
            }),
        );
    }

    serde_json::json!({
        "providers": providers,
        "provider": pick,
        "gemini_key_masked": key_masked,
        "active_theme": socket::read_character_theme(),
        "jobs": jobs,
    })
}

/// 감지 결과. 오래됐으면 새로 훑고, 아니면 캐시 그대로.
fn providers_cached() -> Vec<ProviderStatus> {
    {
        let c = PROVIDERS.read().unwrap();
        if c.at.is_some_and(|t| t.elapsed() < DETECT_EVERY) {
            return c.list.clone();
        }
    }
    let list = detect_providers();
    let mut c = PROVIDERS.write().unwrap();
    c.list = list.clone();
    c.at = Some(Instant::now());
    list
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// ppgen 실행 파일을 찾는다. env → 앱 설정 폴더 → /tmp → PATH 순.
///
/// `/tmp` 를 마지막에 두는 이유는 재부팅에 사라져서다 — 예전 파이프라인이 거기를
/// 기본값으로 써서, 하루 지나면 「없다」로 죽는 일이 있었다.
pub(crate) fn ppgen_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("PPGEN") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Some(home) = kasa_socket::home_dir() {
        let p = home.join(".config/kasaterm/bin/ppgen");
        if p.is_file() {
            return Some(p);
        }
    }
    let tmp = PathBuf::from("/tmp/ppgen");
    if tmp.is_file() {
        return Some(tmp);
    }
    which("ppgen")
}

/// PATH 에서 실행 파일을 찾는다. `which` 를 spawn 하지 않는 이유는 한 프레임에
/// 여러 번 불릴 수 있어서다(감지 캐시가 있어도 첫 프레임엔 돈다).
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var("PATH").ok()?;
    for dir in path.split(':').filter(|s| !s.is_empty()) {
        let p = Path::new(dir).join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn og_key_path() -> Option<PathBuf> {
    kasa_socket::home_dir().map(|h| h.join(".config/opengateway.key"))
}

fn og_key() -> Option<String> {
    let p = og_key_path()?;
    let s = std::fs::read_to_string(p).ok()?;
    let s = s.trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// 나노바나나(gemini) 키. 설정값이 우선이고 없으면 환경변수를 본다.
fn gemini_key() -> Option<String> {
    if let Some(s) = socket::read_settings()
        .get("gemini_api_key")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Some(s.to_string());
    }
    std::env::var("GEMINI_API_KEY")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// 지금 무엇으로 구울 수 있나. UI 목록과 `themegen_start` 의 폴백이 같은 판정을 쓴다.
pub(crate) fn detect_providers() -> Vec<ProviderStatus> {
    let ppgen = ppgen_path();
    let no_ppgen = ppgen.is_none();
    let ppgen_why = "ppgen 실행 파일 없음 (~/.config/kasaterm/bin/ppgen)";

    let mut v = Vec::new();

    let og = og_key().is_some();
    v.push(ProviderStatus {
        kind: "opengateway",
        label: "OG 게이트웨이".to_string(),
        available: og && !no_ppgen,
        why: if no_ppgen {
            ppgen_why.to_string()
        } else if !og {
            "키 파일 없음 (~/.config/opengateway.key)".to_string()
        } else {
            String::new()
        },
    });

    let codex = which("codex").is_some();
    v.push(ProviderStatus {
        kind: "codex",
        label: "코덱스".to_string(),
        available: codex && !no_ppgen,
        why: if no_ppgen {
            ppgen_why.to_string()
        } else if !codex {
            "codex CLI 없음".to_string()
        } else {
            String::new()
        },
    });

    let nb = gemini_key().is_some();
    v.push(ProviderStatus {
        kind: "nanobanana",
        label: "나노바나나".to_string(),
        available: nb && !no_ppgen,
        why: if no_ppgen {
            ppgen_why.to_string()
        } else if !nb {
            "키 없음 — 아래 칸에 넣어라".to_string()
        } else {
            String::new()
        },
    });

    v
}

/// 참조 그림을 게이트웨이가 받는 크기로 줄인다. 512px·100KB 이하 JPEG.
///
/// 원본을 지우지 않고 옆에 `ref-small.jpg` 로 둔다 — 다시 구울 때 원본이 있어야
/// 같은 그림으로 재현이 된다.
fn shrink_ref(src: &Path, dst: &Path) -> Result<(), String> {
    let im = image::open(src).map_err(|e| format!("참조 그림을 못 읽었다: {e}"))?;
    let im = im.resize(
        REF_MAX_PX,
        REF_MAX_PX,
        image::imageops::FilterType::Lanczos3,
    );
    // 알파를 흰 배경에 합성한다 — JPEG 은 알파가 없어서, 그냥 버리면 투명부가
    // 검게 깔려 「검은 옷을 입은 캐릭터」로 읽힌다.
    let mut rgb = image::RgbImage::new(im.width(), im.height());
    let rgba = im.to_rgba8();
    for (x, y, p) in rgba.enumerate_pixels() {
        let a = p[3] as f32 / 255.0;
        let mix = |c: u8| ((c as f32 * a) + 255.0 * (1.0 - a)).round().clamp(0.0, 255.0) as u8;
        rgb.put_pixel(x, y, image::Rgb([mix(p[0]), mix(p[1]), mix(p[2])]));
    }
    // 상한에 들 때까지 품질을 낮춘다. 대개 첫 판(85)에서 들어간다.
    for q in [85u8, 70, 55, 40] {
        let mut buf = Vec::new();
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, q);
        enc.encode(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            image::ExtendedColorType::Rgb8,
        )
        .map_err(|e| format!("JPEG 변환 실패: {e}"))?;
        if buf.len() <= REF_MAX_BYTES || q == 40 {
            if let Some(parent) = dst.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            std::fs::write(dst, &buf).map_err(|e| format!("참조 저장 실패: {e}"))?;
            return Ok(());
        }
    }
    Err("참조를 상한까지 못 줄였다".to_string())
}

/// 키잉 배경색을 고른다. 판정 기준이 **desc 단어**인 이유는 `theme-sprites.py`
/// 주석에 있다 — 충돌의 본질이 「desc 가 pink 라 말하는데 magenta 캔버스가 pink 를
/// 금지」라서, 모델이 실제로 읽는 텍스트가 곧 기준이다. ref 픽셀 HSV 로 재면
/// 살색·붉은 하이라이트가 분홍으로 잡혀 오탐이 난다.
fn pick_chroma(desc: &str) -> &'static str {
    let d = desc.to_lowercase();
    let count = |ws: &[&str]| -> usize { ws.iter().map(|w| d.matches(w).count()).sum() };
    let pink = count(&[
        "pink", "magenta", "purple", "violet", "lavender", "fuchsia", "lilac",
    ]);
    let green = count(&["green", "lime", "mint", "emerald", "olive"]);
    if pink > green { "green" } else { "magenta" }
}

/// 프레임이 뭉갰는지. (고유색, 중앙 런 길이). ppgen 자체 점수는 프레임 수와 모션
/// 다양성을 보지 픽셀 양자화가 무너진 것은 못 잡아서 따로 잰다.
fn frame_quality(path: &Path) -> Result<(usize, f32), String> {
    let im = image::open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .to_rgba8();
    let mut set = std::collections::HashSet::new();
    for p in im.pixels() {
        set.insert((p[0], p[1], p[2]));
    }
    let (w, h) = (im.width(), im.height());
    let mut runs: Vec<u32> = Vec::new();
    let mut y = 0;
    while y < h {
        let mut run = 1u32;
        for x in 1..w {
            let cur = im.get_pixel(x, y);
            let prev = im.get_pixel(x - 1, y);
            if cur == prev {
                run += 1;
            } else {
                if prev[3] > 8 {
                    runs.push(run);
                }
                run = 1;
            }
        }
        y += 4;
    }
    let median = if runs.is_empty() {
        999.0
    } else {
        runs.sort_unstable();
        let m = runs.len() / 2;
        if runs.len() % 2 == 1 {
            runs[m] as f32
        } else {
            (runs[m - 1] + runs[m]) as f32 / 2.0
        }
    };
    Ok((set.len(), median))
}

/// 바운딩박스 안 불투명 비율. 키잉이 몸을 파먹으면 뚝 떨어진다(정상 46~66%).
/// 조각만 남아도 bbox 는 멀쩡히 잡히므로 `frame_quality` 로는 못 잡는다.
fn opaque_frac(path: &Path) -> Result<f32, String> {
    let im = image::open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .to_rgba8();
    let Some((x0, y0, x1, y1)) = alpha_bbox(&im) else {
        return Ok(0.0);
    };
    let mut total = 0u64;
    let mut solid = 0u64;
    for y in y0..=y1 {
        for x in x0..=x1 {
            total += 1;
            if im.get_pixel(x, y)[3] > 128 {
                solid += 1;
            }
        }
    }
    Ok(if total == 0 {
        0.0
    } else {
        solid as f32 / total as f32
    })
}

/// 알파가 있는 영역의 경계 (x0, y0, x1, y1) — 둘 다 포함.
fn alpha_bbox(im: &image::RgbaImage) -> Option<(u32, u32, u32, u32)> {
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
    for (x, y, p) in im.enumerate_pixels() {
        if p[3] > 0 {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    (x0 != u32::MAX).then_some((x0, y0, x1, y1))
}

/// 전용 초상을 알파 경계로 잘라 정사각에 통째로 맞춘다. 폭 기준 타이트 크롭도
/// 시험했지만 머리 위 장식이 잘리고 구도가 어긋났다 — 버스트 전체가 프레임 안에
/// 들어가는 쪽이 기존 자산의 기준이다.
fn fit_profile(src: &Path, dst: &Path) -> Result<(), String> {
    let im = image::open(src)
        .map_err(|e| format!("초상을 못 읽었다: {e}"))?
        .to_rgba8();
    let cropped = match alpha_bbox(&im) {
        Some((x0, y0, x1, y1)) => {
            image::imageops::crop_imm(&im, x0, y0, x1 - x0 + 1, y1 - y0 + 1).to_image()
        }
        None => im,
    };
    let (w, h) = (cropped.width(), cropped.height());
    let side = w.max(h).max(1);
    let mut sq = image::RgbaImage::new(side, side);
    image::imageops::replace(
        &mut sq,
        &cropped,
        ((side - w) / 2) as i64,
        ((side - h) / 2) as i64,
    );
    let out = image::imageops::resize(
        &sq,
        PROFILE_PX,
        PROFILE_PX,
        image::imageops::FilterType::Lanczos3,
    );
    if let Some(parent) = dst.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    out.save(dst).map_err(|e| format!("프로필 저장 실패: {e}"))
}

/// 모든 상태의 프레임이 다 있는가. 매니페스트 존재만 보면 안 된다 — ppgen 은 일부
/// 상태가 실패해도 매니페스트를 남겨서, cheer 가 통째로 빠진 채 「생성됨」이 된다.
fn generated(out: &Path) -> bool {
    if !out.join("manifest.json").is_file() {
        return false;
    }
    STATES.iter().all(|(state, count)| {
        (0..*count).all(|i| {
            out.join("frames")
                .join(out_state(state))
                .join(format!("frame-{i:02}.png"))
                .is_file()
        })
    })
}

/// 생성물이 쓸 만한지. 나쁘면 사유, 괜찮으면 None.
fn check(out: &Path) -> Option<String> {
    for (state, count) in STATES {
        for i in [0usize, count / 2] {
            let p = out
                .join("frames")
                .join(out_state(state))
                .join(format!("frame-{i:02}.png"));
            if !p.is_file() {
                return Some(format!("{state}/frame-{i:02} 없음"));
            }
            match frame_quality(&p) {
                Ok((colors, run)) => {
                    if colors < MIN_COLORS || run > MAX_RUN {
                        return Some(format!("{state} 뭉개짐(색{colors} 런{run:.0})"));
                    }
                }
                Err(e) => return Some(e),
            }
        }
    }
    None
}

/// 실행이 끝나면 지워지는 임시 HOME. ppgen 이 여기 있는 `config.json` 을 읽는다.
struct KeyHome(PathBuf);

impl Drop for KeyHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 임시 HOME 을 만들어 그 안에 키가 든 `config.json` 을 0600 으로 쓴다.
fn key_home(provider_field: &str, key: &str) -> Result<KeyHome, String> {
    let dir = std::env::temp_dir().join(format!(
        "kasaterm-ppgen-{}-{}",
        std::process::id(),
        now_ms()
    ));
    let cfg_dir = dir.join("Library/Application Support/perfectpixel");
    std::fs::create_dir_all(&cfg_dir).map_err(|e| format!("임시 설정 폴더 실패: {e}"))?;
    let body = serde_json::json!({
        "provider": provider_field,
        provider_field: { "apiKey": key, "model": "" }
    })
    .to_string();
    let path = cfg_dir.join("config.json");
    std::fs::write(&path, body).map_err(|e| format!("임시 설정 저장 실패: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(KeyHome(dir))
}

/// 프로바이더별 ppgen 인자와 환경, 그리고 키를 담은 임시 HOME(있으면).
///
/// 반환한 `KeyHome` 은 **ppgen 이 끝날 때까지 살아 있어야 한다** — 떨어뜨리는 순간
/// 폴더가 지워져 키를 못 읽는다.
fn provider_args(
    kind: &str,
) -> Result<(Vec<String>, Vec<(String, String)>, Option<KeyHome>), String> {
    match kind {
        "opengateway" => {
            let key = og_key().ok_or("OG 키 파일이 없다 (~/.config/opengateway.key)")?;
            let home = key_home("openai", &key)?;
            Ok((
                vec![
                    "-provider".into(),
                    "openai".into(),
                    "-model".into(),
                    "openai/gpt-image-2".into(),
                ],
                vec![
                    ("HOME".into(), home.0.to_string_lossy().to_string()),
                    // 게이트웨이 주소는 config 가 아니라 이 환경변수로만 간다
                    // (`internal/gen/openai.go` 가 직접 읽는다).
                    (
                        "OPENAI_BASE_URL".into(),
                        "https://apis.opengateway.ai/v1".into(),
                    ),
                ],
                Some(home),
            ))
        }
        // ⚠️ `ppgen -h` 의 프로바이더 목록에 openai·codex 가 없지만 **도움말 문자열이
        // 낡은 것**이고 정본은 소스의 `SupportedProviders`(6종)다. openai 경로는
        // 7테마 작전 전체를 실제로 구워 확정됐고, codex 는 크레딧 소진이라 인자
        // 조립까지만 검증했다. codex 는 `~/.codex` 의 OAuth 를 쓰므로 HOME 을 바꾸지
        // 않는다 — 바꾸면 인증을 못 찾는다.
        "codex" => Ok((vec!["-provider".into(), "codex".into()], Vec::new(), None)),
        "nanobanana" => {
            let key = gemini_key().ok_or("나노바나나 키가 없다")?;
            let home = key_home("gemini", &key)?;
            Ok((
                vec![
                    "-provider".into(),
                    "gemini".into(),
                    "-model".into(),
                    "gemini-2.5-flash-image".into(),
                ],
                vec![("HOME".into(), home.0.to_string_lossy().to_string())],
                Some(home),
            ))
        }
        other => Err(format!("모르는 프로바이더: {other}")),
    }
}

/// desc 를 못 지었을 때. 참조 그림 자체가 정체성이므로 굽기는 계속할 수 있다.
const FALLBACK_DESC: &str =
    "Faithfully reproduce the character shown in the identity reference image. No weapon.";

/// 비전 모델에게 참조 그림을 보여 영문 한 줄을 받는다. 규칙은
/// `docs/theme-pipeline.md` ⑤절 그대로다 — 외형만, 한 줄, 400자 이내, `No weapon.` 로 끝.
const DESC_PROMPT: &str = "Describe this character's appearance for a sprite generator, as ONE English line under 400 characters. Include hair color and style, eye color, every clothing item with its colors, and any signature accessory. Appearance only — no personality, no story, no background. End the line with exactly: No weapon.";

/// curl 로 한 번 POST 하고 본문을 돌려준다.
///
/// 설정을 **stdin** 으로 준다 — URL·헤더·본문 파일 경로가 전부 거기 실려서 argv 에는
/// `curl --config -` 만 남는다. 키가 `ps` 에 안 보이는 건 이 때문이다.
fn curl_post(url: &str, auth_header: &str, body: &str) -> Result<String, String> {
    use std::io::Write;
    let tmp = std::env::temp_dir().join(format!("kasaterm-desc-{}.json", std::process::id()));
    std::fs::write(&tmp, body).map_err(|e| format!("본문 임시 저장 실패: {e}"))?;
    let cfg = format!(
        "url = \"{url}\"\nheader = \"Content-Type: application/json\"\nheader = \"{auth_header}\"\ndata = \"@{}\"\nsilent\nshow-error\nmax-time = 120\n",
        tmp.display()
    );
    let mut child = std::process::Command::new("curl")
        .arg("--config")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("curl 실행 실패: {e}"))?;
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(cfg.as_bytes());
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("curl 대기 실패: {e}"))?;
    let _ = std::fs::remove_file(&tmp);
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail = err.lines().last().unwrap_or("").trim();
        return Err(format!("요청 실패: {tail}"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// 400자 상한으로 자른다. 생성 프롬프트에 그대로 들어가는 줄이라 길면 지시가 흐려진다
/// (2026-08-24 실측: 42개 중 26개가 411~516자였다).
fn tidy_desc(s: &str) -> String {
    let one = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let one = one.trim().trim_matches('"').trim().to_string();
    if one.chars().count() <= 400 {
        return one;
    }
    let cut: String = one.chars().take(400).collect();
    match cut.rfind(", ") {
        Some(i) if i > 200 => format!("{}. No weapon.", &cut[..i]),
        _ => cut,
    }
}

/// 참조 그림을 보고 desc 를 짓는다. 실패하면 폴백으로 계속 간다(굽기 자체는 참조로 된다).
fn describe(kind: &str, ref_jpg: &Path) -> String {
    let Ok(bytes) = std::fs::read(ref_jpg) else {
        return FALLBACK_DESC.to_string();
    };
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);

    let parsed = match kind {
        "opengateway" => og_key().and_then(|key| {
            let body = serde_json::json!({
                "model": "openai/gpt-5",
                "messages": [{
                    "role": "user",
                    "content": [
                        {"type": "text", "text": DESC_PROMPT},
                        {"type": "image_url",
                         "image_url": {"url": format!("data:image/jpeg;base64,{b64}")}}
                    ]
                }]
            })
            .to_string();
            curl_post(
                "https://apis.opengateway.ai/v1/chat/completions",
                &format!("Authorization: Bearer {key}"),
                &body,
            )
            .ok()
            .and_then(|txt| {
                serde_json::from_str::<serde_json::Value>(&txt).ok()?["choices"][0]["message"]
                    ["content"]
                    .as_str()
                    .map(str::to_string)
            })
        }),
        "nanobanana" => gemini_key().and_then(|key| {
            let body = serde_json::json!({
                "contents": [{
                    "parts": [
                        {"text": DESC_PROMPT},
                        {"inline_data": {"mime_type": "image/jpeg", "data": b64}}
                    ]
                }]
            })
            .to_string();
            curl_post(
                "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent",
                &format!("x-goog-api-key: {key}"),
                &body,
            )
            .ok()
            .and_then(|txt| {
                serde_json::from_str::<serde_json::Value>(&txt).ok()?["candidates"][0]["content"]
                    ["parts"][0]["text"]
                    .as_str()
                    .map(str::to_string)
            })
        }),
        // codex 는 CLI 로 이미지를 넣는다. 없으면 폴백.
        "codex" => which("codex").and_then(|bin| {
            let out = std::process::Command::new(bin)
                .arg("exec")
                .arg("-i")
                .arg(ref_jpg)
                .arg(DESC_PROMPT)
                .output()
                .ok()?;
            out.status
                .success()
                .then(|| String::from_utf8_lossy(&out.stdout).to_string())
        }),
        _ => None,
    };

    match parsed {
        Some(s) if !s.trim().is_empty() => {
            let mut d = tidy_desc(&s);
            if !d.ends_with("No weapon.") {
                d.push_str(" No weapon.");
                d = tidy_desc(&d);
            }
            d
        }
        _ => FALLBACK_DESC.to_string(),
    }
}

/// ppgen 한 번. 키는 `env` 가 가리키는 임시 HOME 의 config.json 에 있다(모듈 주석 참조).
fn run_ppgen(
    ppgen: &Path,
    args: &[String],
    env: &[(String, String)],
    extra: &[&str],
) -> Result<(), String> {
    let mut cmd = std::process::Command::new(ppgen);
    cmd.args(args);
    cmd.args(extra);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd
        .output()
        .map_err(|e| format!("ppgen 실행 실패: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    // ⚠️ 커맨드라인을 통째로 남기지 마라 — argv 에 키가 없더라도 로그가 길어지면
    // 사람이 그걸 그대로 붙여 넣게 되고, 그때 키가 딸려 간다. 마지막 줄만 남긴다.
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let tail = all.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("");
    Err(if tail.is_empty() {
        format!("ppgen 종료 코드 {:?}", out.status.code())
    } else {
        tail.trim().to_string()
    })
}

/// 굽기 본체 — 백그라운드 스레드에서 돈다. 성공하면 생성물 디렉토리를 돌려준다.
fn bake(job: &Arc<Mutex<Job>>, gen_dir: PathBuf, ref_png: PathBuf) -> Result<PathBuf, String> {
    let ppgen = ppgen_path().ok_or("ppgen 실행 파일이 없다")?;
    let kind = job.lock().unwrap().provider;
    // 임시 HOME 은 이 함수가 끝날 때까지 붙잡고 있어야 한다(Drop 이 폴더를 지운다).
    let (pargs, penv, _key_home) = provider_args(kind)?;

    let ref_jpg = gen_dir.join("ref-small.jpg");
    shrink_ref(&ref_png, &ref_jpg)?;

    let desc = describe(kind, &ref_jpg);
    {
        let mut j = job.lock().unwrap();
        j.phase = GenPhase::Generating;
        j.detail.clear();
    }
    let _ = std::fs::write(gen_dir.join("desc.txt"), &desc);

    let out = gen_dir.join("out");
    let chroma = pick_chroma(&desc);
    let out_s = out.to_string_lossy().to_string();
    let ref_s = ref_jpg.to_string_lossy().to_string();

    let mut why = String::new();
    for attempt in 1..=ATTEMPTS {
        if attempt > 1 {
            job.lock().unwrap().detail = format!("{attempt}/{ATTEMPTS} 번째 시도");
        }
        let _ = std::fs::remove_dir_all(&out);
        let states: Vec<&str> = vec![
            "-desc", &desc, "-style", "pixel-chibi", "-out", &out_s, "-chroma", chroma, "-states",
            "idle,wave,cheer", "-dirset", "walk", "-dirs", "east", "-ref", &ref_s,
        ];
        if let Err(e) = run_ppgen(&ppgen, &pargs, &penv, &states) {
            why = e;
            continue;
        }
        if !generated(&out) {
            why = "일부 상태가 안 나왔다".to_string();
            continue;
        }
        if let Some(e) = check(&out) {
            why = e;
            continue;
        }
        why.clear();
        break;
    }
    if !why.is_empty() {
        return Err(why);
    }

    // 프로필은 따로 굽는다 — 스프라이트 첫 프레임을 자르면 얼굴이 너무 작다.
    // 실패해도 치명적이지 않으므로(설치가 idle 첫 프레임으로 폴백) 사유만 삼킨다.
    job.lock().unwrap().detail = "프로필 굽는 중".to_string();
    let pout = gen_dir.join("out-profile");
    let pout_s = pout.to_string_lossy().to_string();
    let pdesc = format!(
        "{} Bust framing like a game profile portrait.",
        desc.replace("No weapon.", "").trim()
    );
    let pargs2: Vec<&str> = vec![
        "-desc", &pdesc, "-style", "pixel", "-portrait", "-ref", &ref_s, "-out", &pout_s,
        "-chroma", chroma,
    ];
    if run_ppgen(&ppgen, &pargs, &penv, &pargs2).is_ok() {
        // 키잉이 몸을 파먹은 초상은 지운다 — 설치가 idle 첫 프레임으로 폴백하는 편이
        // 조각난 그림을 프사로 쓰는 것보다 낫다. bbox 는 조각만 남아도 크게 잡히므로
        // 불투명 비율로만 걸린다(정상 46~66%).
        let pbase = pout.join("base.png");
        if opaque_frac(&pbase).is_ok_and(|f| f < 0.40) {
            let _ = std::fs::remove_file(&pbase);
        }
    }

    job.lock().unwrap().detail.clear();
    Ok(out)
}

/// 생성물을 앱이 읽는 자리로 옮긴다. `themes/<id>/sprites/<state>/<slug>-<i>.png`.
fn install(theme_dir: &Path, slug: &str, out: &Path) -> Result<(), String> {
    let sprites = theme_dir.join("sprites");
    for (state, count) in STATES {
        let dstdir = sprites.join(state);
        std::fs::create_dir_all(&dstdir).map_err(|e| format!("{state} 폴더 실패: {e}"))?;
        for i in 0..count {
            let src = out
                .join("frames")
                .join(out_state(state))
                .join(format!("frame-{i:02}.png"));
            if !src.is_file() {
                return Err(format!("{state} frame-{i:02} 없음"));
            }
            std::fs::copy(&src, dstdir.join(format!("{slug}-{i}.png")))
                .map_err(|e| format!("{state} 복사 실패: {e}"))?;
        }
    }

    let gifdir = sprites.join("gif");
    let _ = std::fs::create_dir_all(&gifdir);
    let gif = out.join("gif").join("idle.gif");
    if gif.is_file() {
        let _ = std::fs::copy(&gif, gifdir.join(format!("{slug}.gif")));
    }

    let profile_dst = sprites.join("profile").join(format!("{slug}.png"));
    let dedicated = out
        .parent()
        .map(|p| p.join("out-profile").join("base.png"))
        .filter(|p| p.is_file());
    let src = dedicated.unwrap_or_else(|| out.join("frames").join("idle").join("frame-00.png"));
    fit_profile(&src, &profile_dst)?;
    Ok(())
}

/// slug 를 사람이 읽는 이름으로. `chen_qianyu` → `Chen Qianyu`.
///
/// 사용자가 UI 에서 이름을 직접 주기 전까지의 자리표시다 — roster 에 이름이 없으면
/// 앱 목록에서 빈 칸이 되므로, 비워 두는 것보다 유도해 넣는 편이 낫다.
fn display_name(slug: &str) -> String {
    slug.split(['_', '-'])
        .filter(|s| !s.is_empty())
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `theme.json` 의 members 에 이 slug 가 없으면 한 줄 넣는다.
///
/// **그림만 넣고 여기를 빼면 앱이 그 캐릭터를 아예 안 보여준다** — 로스터가 명단이고
/// sprites 는 그 명단이 가리키는 그림이라서다. 이미 있으면 손대지 않는다(사용자가
/// 고쳐 둔 이름·색을 덮으면 안 된다).
fn ensure_roster(theme_dir: &Path, slug: &str) -> Result<(), String> {
    let p = theme_dir.join("theme.json");
    let txt = std::fs::read_to_string(&p).map_err(|e| format!("theme.json 을 못 읽었다: {e}"))?;
    let mut v: serde_json::Value =
        serde_json::from_str(&txt).map_err(|e| format!("theme.json 이 깨졌다: {e}"))?;
    let obj = v
        .as_object_mut()
        .ok_or("theme.json 이 객체가 아니다".to_string())?;

    if obj
        .get("leader")
        .and_then(|l| l.get("slug"))
        .and_then(|s| s.as_str())
        == Some(slug)
    {
        return Ok(());
    }
    let members = obj
        .entry("members")
        .or_insert_with(|| serde_json::Value::Array(Vec::new()));
    let arr = members
        .as_array_mut()
        .ok_or("members 가 배열이 아니다".to_string())?;
    if arr
        .iter()
        .any(|m| m.get("slug").and_then(|s| s.as_str()) == Some(slug))
    {
        return Ok(());
    }
    arr.push(serde_json::json!({
        "name": display_name(slug),
        "slug": slug,
    }));

    let out = serde_json::to_string_pretty(&v).map_err(|e| format!("직렬화 실패: {e}"))?;
    std::fs::write(&p, out).map_err(|e| format!("theme.json 저장 실패: {e}"))
}

/// 키를 화면에 보일 꼴로 가린다. 앞뒤 네 글자만 남긴다 — 「무엇이 들어 있긴 하다」와
/// 「어느 키인지」는 알려주되 값 자체는 어깨너머로 못 읽게.
pub(crate) fn mask_key(s: &str) -> String {
    let n = s.chars().count();
    if n == 0 {
        return String::new();
    }
    if n <= 8 {
        return "•".repeat(n);
    }
    let head: String = s.chars().take(4).collect();
    let tail: String = s.chars().skip(n - 4).collect();
    format!("{head}{}{tail}", "•".repeat(6))
}

/// 고른 그림을 그 캐릭터의 참조 자리에 PNG 로 넣는다.
///
/// 확장자만 바꿔 복사하지 않는다 — jpg 를 `ref.png` 로 이름만 바꾸면 파일 내용과
/// 이름이 어긋나, 나중에 이 그림을 여는 쪽이 확장자를 믿었다가 깨진다.
/// 굽기 대상 테마. 번들(빈 값)이면 참조를 놓을 자리가 없다 — 번들 폴더는 앱 안에
/// 있어서 쓰기가 막히고, 써지더라도 앱을 새로 깔면 사라진다.
fn writable_theme() -> Result<(String, PathBuf), String> {
    let id = socket::read_character_theme();
    if id.trim().is_empty() {
        return Err("기본 테마에는 못 넣는다 — 테마를 하나 복제해서 그 안에 만들어라".into());
    }
    let dir = socket::theme_dir(&id).ok_or("그 테마 폴더가 없다")?;
    Ok((id, dir))
}

/// `GET /settings/themegen/ref?slug=` — 화면에 띄울 원본 그대로.
pub(crate) fn themegen_ref_bytes(slug: &str) -> Option<Vec<u8>> {
    let dir = socket::theme_dir(&socket::read_character_theme())?;
    std::fs::read(dir.join("gen").join(slug).join("ref.png")).ok()
}

/// `POST /settings/themegen/ref` — 웹이 올린 그림을 참조 자리에 놓는다.
///
/// `slug` 가 있으면 그 캐릭터의 참조를 갈고, 없으면 `name`(원본 파일명)에서 이름을
/// 유도해 로스터에 새로 등록한다. 돌려주는 것은 **실제로 쓰인 slug** 다 — 파일명이
/// 어떻게 다듬어졌는지 화면이 알아야 그 캐릭터를 이어서 열 수 있다.
pub(crate) fn themegen_put_ref(
    slug: Option<&str>,
    name: Option<&str>,
    bytes: &[u8],
) -> Result<String, String> {
    if bytes.is_empty() {
        return Err("빈 파일이다".into());
    }
    let (_, dir) = writable_theme()?;

    // 바이트를 먼저 그림으로 읽어 본다 — 확장자만 믿고 놓으면 깨진 파일이 참조로
    // 앉아 굽기가 시작된 뒤에야 실패한다.
    image::load_from_memory(bytes).map_err(|e| format!("그림이 아니다: {e}"))?;

    let fname = name.filter(|n| !n.trim().is_empty()).unwrap_or("ref.png");
    let tmp = std::env::temp_dir().join(format!("kasaterm-ref-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).map_err(|e| format!("임시 폴더 실패: {e}"))?;
    let staged = tmp.join(sanitize_name(fname));
    std::fs::write(&staged, bytes).map_err(|e| format!("임시 저장 실패: {e}"))?;

    let out = (|| {
        let slug = match slug.filter(|s| !s.trim().is_empty()) {
            Some(s) => s.to_string(),
            None => add_theme_member(&dir, &staged).ok_or("이름을 못 정했다")?.to_string(),
        };
        place_themegen_ref(&dir, &slug, &staged)?;
        Ok::<String, String>(slug)
    })();

    let _ = std::fs::remove_dir_all(&tmp);
    out
}

/// 업로드 파일명에서 경로를 걷어낸다. 웹이 보내는 값이라 `../` 가 섞여 올 수 있고,
/// 그대로 이으면 임시 폴더 밖에 파일이 생긴다.
fn sanitize_name(n: &str) -> String {
    let base = n.rsplit(['/', '\\']).next().unwrap_or(n);
    let cleaned: String = base
        .chars()
        .filter(|c| !matches!(c, '\0' | ':'))
        .take(120)
        .collect();
    if cleaned.trim().is_empty() {
        "ref.png".to_string()
    } else {
        cleaned
    }
}

pub(crate) fn place_themegen_ref(theme_dir: &Path, slug: &str, src: &Path) -> Result<PathBuf, String> {
    let dir = theme_dir.join("gen").join(slug);
    std::fs::create_dir_all(&dir).map_err(|e| format!("폴더 실패: {e}"))?;
    let dst = dir.join("ref.png");
    let im = image::open(src).map_err(|e| format!("그림을 못 읽었다: {e}"))?;
    im.save(&dst).map_err(|e| format!("저장 실패: {e}"))?;
    Ok(dst)
}

/// 「+ 새 캐릭터」의 로스터 쪽 절반 — 파일명에서 slug 를 유도해 `theme.json` 에 넣고
/// 그 slug 를 돌려준다. 이미 있으면 등록은 건너뛰고 slug 만 준다(그림만 갈아 끼우는 길).
pub(crate) fn add_theme_member(theme_dir: &Path, src: &Path) -> Option<String> {
    let slug = slug_from_path(src);
    ensure_roster(theme_dir, &slug).ok()?;
    Some(slug)
}

/// 파일 이름에서 slug 를 유도한다. 공백·대문자를 눕히고 파일시스템에 안전한 것만 남긴다.
fn slug_from_path(p: &Path) -> String {
    let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let mut out = String::new();
    for ch in stem.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let out = out.trim_matches('_').to_string();
    if out.is_empty() { "student".to_string() } else { out }
}

impl App {
    /// 감지 스냅샷. `themegen_poll` 이 채운 캐시를 읽기만 한다 — 설정 화면이 매
    /// 프레임 부르는 자리라 여기서 디스크를 돌면 안 된다.
    pub(crate) fn themegen_providers(&self) -> Vec<ProviderStatus> {
        providers_cached()
    }

    /// 한 명 굽기를 시작한다. `ref_path` 는 UI 가 이미 놓아 둔 원본이다.
    pub(crate) fn themegen_start(
        &mut self,
        theme_id: &str,
        slug: &str,
        ref_path: &Path,
        provider_override: Option<&str>,
    ) {
        if slug.is_empty() || theme_id.is_empty() {
            self.set_toast("테마와 이름이 있어야 굽는다".to_string());
            return;
        }
        if JOBS
            .read()
            .unwrap()
            .get(slug)
            .is_some_and(|j| !matches!(j.lock().unwrap().phase, GenPhase::Done | GenPhase::Failed))
        {
            return;
        }

        let kind = provider_override
            .map(str::to_string)
            .or_else(|| {
                socket::read_settings()
                    .get("theme_gen_provider")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "opengateway".to_string());
        let kind: &'static str = match kind.as_str() {
            "codex" => "codex",
            "nanobanana" => "nanobanana",
            _ => "opengateway",
        };

        let Some(dir) = socket::theme_dir(theme_id) else {
            self.set_toast("그 테마 폴더가 없다".to_string());
            return;
        };
        let gen_dir = dir.join("gen").join(slug);
        let _ = std::fs::create_dir_all(&gen_dir);

        let job = Arc::new(Mutex::new(Job {
            theme_id: theme_id.to_string(),
            slug: slug.to_string(),
            provider: kind,
            phase: GenPhase::Describing,
            detail: String::new(),
            failed: None,
            started_ms: now_ms(),
            ready: None,
        }));
        JOBS.write().unwrap().insert(slug.to_string(), job.clone());

        let ref_png = ref_path.to_path_buf();
        std::thread::spawn(move || match bake(&job, gen_dir, ref_png) {
            Ok(out) => {
                let mut j = job.lock().unwrap();
                j.phase = GenPhase::Installing;
                j.ready = Some(out);
            }
            Err(e) => {
                let mut j = job.lock().unwrap();
                j.phase = GenPhase::Failed;
                j.failed = Some(e);
            }
        });
    }

    /// 프레임마다 한 번. 감지 캐시를 갱신하고, 다 구운 잡을 설치한다.
    pub(crate) fn themegen_poll(&mut self) {
        // 감지 캐시는 전역이라 여기서 굳이 안 훑어도 웹 폴링이 갱신한다. 다만
        // 웹을 한 번도 안 연 사용자를 위해 네이티브 틱에서도 한 번씩 태운다.
        let _ = providers_cached();

        // 설치를 기다리는 잡을 걷는다. 잠금을 들고 설치하면 UI 가 그 프레임 내내
        // 멈추므로, 필요한 것만 꺼내고 바로 놓는다.
        let mut pending: Vec<(String, String, PathBuf)> = Vec::new();
        for job in JOBS.read().unwrap().values() {
            let mut j = job.lock().unwrap();
            if j.phase == GenPhase::Installing {
                if let Some(out) = j.ready.take() {
                    pending.push((j.theme_id.clone(), j.slug.clone(), out));
                }
            }
        }
        if pending.is_empty() {
            return;
        }

        for (theme_id, slug, out) in pending {
            let result = socket::theme_dir(&theme_id)
                .ok_or_else(|| "테마 폴더가 사라졌다".to_string())
                .and_then(|dir| install(&dir, &slug, &out).map(|_| dir))
                .and_then(|dir| ensure_roster(&dir, &slug).map(|_| ()));

            if let Some(job) = JOBS.read().unwrap().get(&slug) {
                let mut j = job.lock().unwrap();
                match &result {
                    Ok(()) => j.phase = GenPhase::Done,
                    Err(e) => {
                        j.phase = GenPhase::Failed;
                        j.failed = Some(e.clone());
                    }
                }
            }
            match result {
                Ok(()) => {
                    socket::invalidate_theme_rows();
                    theme::invalidate_roster();
                    sprites::invalidate_theme_sprite_dirs();
                    sprites::invalidate_idle_anim();
                    self.set_toast(format!("✓ {slug} 완성"));
                }
                Err(e) => self.set_toast(format!("⚠ {slug} 실패: {e}")),
            }
        }
    }

    pub(crate) fn themegen_view(&self, slug: &str) -> Option<GenJobView> {
        let jobs = JOBS.read().unwrap();
        let j = jobs.get(slug)?.lock().unwrap();
        Some(GenJobView {
            phase: j.phase,
            phase_label: j.phase_label().to_string(),
            detail: j.detail.clone(),
            failed_reason: j.failed.clone(),
            provider: j.provider.to_string(),
            started_ms: j.started_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 업로드 파일명은 웹이 보내는 값이라 경로가 섞여 올 수 있다. 그대로 이으면
    /// 임시 폴더 밖에 파일이 생긴다.
    #[test]
    fn an_uploaded_name_never_escapes_its_folder() {
        assert_eq!(sanitize_name("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_name("a/b/c/Miku.png"), "Miku.png");
        assert_eq!(sanitize_name("..\\..\\windows\\evil.png"), "evil.png");
        // 걷어내고 나면 아무것도 안 남는 이름도 있다 — 빈 파일명으로 저장을
        // 시도하면 그 실패가 「그림이 아니다」로 잘못 보고된다.
        assert_eq!(sanitize_name("   "), "ref.png");
        assert_eq!(sanitize_name("/"), "ref.png");
    }

    /// 진행 단계 이름은 프론트가 자기 사전에서 문구로 옮기는 **키**다. 여기 값이
    /// 바뀌면 화면이 그 단계만 못 알아본다.
    #[test]
    fn phase_names_stay_language_free() {
        assert_eq!(GenPhase::Describing.wire(), "describing");
        assert_eq!(GenPhase::Generating.wire(), "generating");
        assert_eq!(GenPhase::Installing.wire(), "installing");
        assert_eq!(GenPhase::Done.wire(), "done");
        assert_eq!(GenPhase::Failed.wire(), "failed");
    }

    #[test]
    fn chroma_follows_desc_words_not_pixels() {
        // 분홍이 정체성이면 green 키로 돌린다 — magenta 캔버스가 피사체의 분홍까지
        // 금지해서 모델이 색을 바꿔 그리기 때문이다.
        assert_eq!(
            pick_chroma("very long pink hair, magenta halo, purple skirt"),
            "green"
        );
        assert_eq!(pick_chroma("green jacket with mint lining"), "magenta");
        // 어느 쪽 단어도 없으면 기본값.
        assert_eq!(pick_chroma("plain white shirt"), "magenta");
    }

    #[test]
    fn desc_is_trimmed_to_the_prompt_limit() {
        let long = "word ".repeat(200);
        let out = tidy_desc(&long);
        assert!(out.chars().count() <= 400, "실제 {}", out.chars().count());
        // 줄바꿈은 한 줄로 접힌다 — 생성기에 그대로 들어가는 줄이라서다.
        assert_eq!(tidy_desc("a\n b\t c"), "a b c");
        assert_eq!(tidy_desc("\"quoted\""), "quoted");
    }

    #[test]
    fn slug_becomes_a_readable_name() {
        assert_eq!(display_name("chen_qianyu"), "Chen Qianyu");
        assert_eq!(display_name("hatsune-miku"), "Hatsune Miku");
        assert_eq!(display_name("rin"), "Rin");
    }

    #[test]
    fn walk_is_the_only_state_whose_folder_differs() {
        assert_eq!(out_state("walk"), "walk-east");
        for s in ["idle", "wave", "cheer"] {
            assert_eq!(out_state(s), s);
        }
    }

    #[test]
    fn roster_gains_the_slug_once_and_keeps_existing_entries() {
        let dir = std::env::temp_dir().join(format!("kasaterm-roster-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("theme.json");
        std::fs::write(
            &p,
            r#"{"theme":"t","label":"T","leader":{"slug":"a","name":"에이"},
                "members":[{"slug":"b","name":"직접 고친 이름"}]}"#,
        )
        .unwrap();

        ensure_roster(&dir, "c").unwrap();
        ensure_roster(&dir, "c").unwrap();
        // 리더와 기존 멤버는 추가 대상이 아니다.
        ensure_roster(&dir, "a").unwrap();
        ensure_roster(&dir, "b").unwrap();

        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        let members = v["members"].as_array().unwrap();
        assert_eq!(members.len(), 2, "중복 추가됐다: {members:?}");
        assert_eq!(members[0]["name"], "직접 고친 이름");
        assert_eq!(members[1]["slug"], "c");
        assert_eq!(members[1]["name"], "C");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn generated_needs_every_state_not_just_the_manifest() {
        let dir = std::env::temp_dir().join(format!("kasaterm-gen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // 매니페스트만 있으면 생성됨이 아니다 — ppgen 은 일부 상태가 실패해도 남긴다.
        std::fs::write(dir.join("manifest.json"), "{}").unwrap();
        assert!(!generated(&dir));

        for (state, count) in STATES {
            let d = dir.join("frames").join(out_state(state));
            std::fs::create_dir_all(&d).unwrap();
            for i in 0..count {
                std::fs::write(d.join(format!("frame-{i:02}.png")), b"x").unwrap();
            }
        }
        assert!(generated(&dir));

        // 한 장만 빠져도 미완성이다.
        std::fs::remove_file(dir.join("frames/cheer/frame-03.png")).unwrap();
        assert!(!generated(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
